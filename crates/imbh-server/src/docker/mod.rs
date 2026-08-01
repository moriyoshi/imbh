//! Docker logging-driver plugin (optional `docker` feature, ARCHITECTURE.md §10.16).
//!
//! Makes `imbhd` usable as `--log-driver imbh`: Docker hands the plugin one FIFO per container, the
//! plugin reads the container's stdout/stderr off it, and the lines land in the embedded `Db` as
//! OTLP logs — queryable over the same `POST /api/query` endpoint (and by SQL, full-text `matches`,
//! or the typed logs API) as everything else `imbhd` holds. `docker logs` keeps working, served back
//! out of the database.
//!
//! ## The protocol
//!
//! A Docker plugin is an HTTP/1.1 server on a Unix socket. Requests are `POST`s with a JSON body,
//! answered in `application/vnd.docker.plugins.v1.1+json`. Five endpoints make a log driver:
//!
//! | Endpoint | What it does |
//! |----------|--------------|
//! | `/Plugin.Activate` | handshake — declares `LogDriver` |
//! | `/LogDriver.StartLogging` | a container started: open its FIFO and pump it into the DB |
//! | `/LogDriver.StopLogging` | that container stopped |
//! | `/LogDriver.Capabilities` | declares `ReadLogs` so `docker logs` routes here |
//! | `/LogDriver.ReadLogs` | streams a container's stored logs back ([`readlogs`]) |
//!
//! Failures are reported *in* the body (`{"Err": "..."}`) with HTTP 200 — that is the plugin
//! contract; a non-200 is only for a request the plugin cannot parse as a plugin request at all.
//!
//! ## Threads
//!
//! The plugin API runs on the same axum/hyper stack as the TCP server, over a `UnixListener`, and
//! shares its request handling ([`crate::handle`]) — so the body limits, phase deadlines, and
//! decoding are the same on both sockets. Alongside it sits one reader thread per live container
//! FIFO, all funneling into a single batching [`ingest::Ingestor`] worker so ingest cost is per
//! batch, not per line. Nothing here is on the query path.
//!
//! `ReadLogs` is the awkward one: it streams length-prefixed frames for as long as the client wants
//! them, and the logic that produces them ([`readlogs::stream`]) is blocking and generic over
//! `io::Write`. Rather than rewrite it as a `Stream`, it runs unchanged on a `spawn_blocking` task
//! whose sink is a bounded channel, and the response body drains that channel — so backpressure and
//! client disconnects reach the generator as ordinary `io::Error`s. On the wire this is now
//! `Transfer-Encoding: chunked` (hyper frames it, since the length is unknowable); Docker's plugin
//! client reads the body through Go's `net/http`, which un-chunks transparently.
//!
//! ## Footprint
//!
//! No new crate: the wire types use prost's derive and OTLP's message types, both already in the
//! default `imbh` graph via `imbh-otlp`. JSON goes through `imbh::parse_json`. Unix only.

pub mod entry;
pub mod ingest;
mod json;
pub mod readlogs;

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufReader, Write};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::RecvTimeoutError;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::header;
use axum::response::IntoResponse;
use axum::routing::any;
use hyper_util::rt::{TokioIo, TokioTimer};
use hyper_util::server::graceful::GracefulShutdown;

use imbh::Db;

use entry::{EntryReader, PartialAssembler};
use ingest::{Container, IngestConfig, Ingestor};

use crate::shutdown::Shutdown;
use crate::{Limits, Response};

/// The content type Docker's plugin client expects on every non-streaming reply.
const PLUGIN_CONTENT_TYPE: &str = "application/vnd.docker.plugins.v1.1+json";

/// The default socket path inside a managed plugin's rootfs. Docker derives it from the
/// `interface.socket` field of `config.json`, so the two must agree.
pub const DEFAULT_SOCKET: &str = "/run/docker/plugins/imbh.sock";

/// How long `StartLogging` waits for the FIFO to open before answering anyway.
///
/// Docker opens the FIFO `O_RDWR` *before* it calls `StartLogging`, so the plugin's read-side open
/// returns immediately in practice. Waiting briefly for it turns the common failure (a path the
/// plugin cannot see) into a useful error in the response; capping the wait means an unexpected
/// blocking open delays the container's start rather than wedging the daemon.
const OPEN_TIMEOUT: Duration = Duration::from_secs(2);

/// Report a plugin-level problem. Routed through `tracing` when `imbhd` is built with that feature,
/// so it joins the rest of the server's instrumentation; plain stderr otherwise.
pub(crate) fn warn(message: &str) {
    #[cfg(feature = "tracing")]
    tracing::warn!(target: "imbh_server::docker", "{message}");
    #[cfg(not(feature = "tracing"))]
    eprintln!("imbhd docker plugin: {message}");
}

/// Serve the logging-driver plugin API on `socket` until the process exits.
///
/// Creates the socket's parent directory and replaces a stale socket left by a previous run. Blocks
/// on the accept loop; run it on its own thread if the caller has other work (see `imbhd`'s `main`).
///
/// Never returns on its own; a host that wants to stop serving wants [`serve_plugin_until`].
pub fn serve_plugin(db: Arc<Db>, socket: impl AsRef<Path>) -> std::io::Result<()> {
    serve_plugin_with(db, socket, IngestConfig::default())
}

/// [`serve_plugin`] with the ingest batching tuned.
pub fn serve_plugin_with(
    db: Arc<Db>,
    socket: impl AsRef<Path>,
    ingest: IngestConfig,
) -> std::io::Result<()> {
    serve_plugin_with_until(db, socket, ingest, Shutdown::new())
}

/// [`serve_plugin`], stopping when `shutdown` trips.
pub fn serve_plugin_until(
    db: Arc<Db>,
    socket: impl AsRef<Path>,
    shutdown: Arc<Shutdown>,
) -> std::io::Result<()> {
    serve_plugin_with_until(db, socket, IngestConfig::default(), shutdown)
}

/// [`serve_plugin_until`] with the ingest batching tuned — what `imbhd` runs on its plugin thread.
///
/// The wind-down order is what makes container output survive a `docker stop` of the plugin:
///
/// 1. stop accepting (a throwaway connect to our own socket unblocks `accept`, so a running plugin
///    pays no poll tick while idle),
/// 2. stop every container's FIFO reader and drain the ingest queue into the `Db` — the caller is
///    about to close it, and a line already read must not be stranded in the queue,
/// 3. let in-flight plugin requests finish, bounded by [`Shutdown::drain_timeout`] (a `docker logs -f`
///    ends on its own once its container's stream is gone),
/// 4. unlink the socket, so a restart binds a clean path instead of clearing someone else's leftover.
pub fn serve_plugin_with_until(
    db: Arc<Db>,
    socket: impl AsRef<Path>,
    ingest: IngestConfig,
    shutdown: Arc<Shutdown>,
) -> std::io::Result<()> {
    // Multi-threaded for the same reason the HTTP listener is: `crate::offload*` needs
    // `block_in_place`, and `ReadLogs` parks a blocking task per follow stream.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(serve_plugin_async(db, socket.as_ref(), ingest, shutdown))
}

/// The plugin's accept loop. Binds first, so a bind failure still reaches the caller — which for
/// `imbhd` is fatal, since Docker would otherwise mark the plugin healthy and every
/// `docker run --log-driver imbh` would hang on a socket nobody is listening to.
async fn serve_plugin_async(
    db: Arc<Db>,
    socket: &Path,
    ingest: IngestConfig,
    shutdown: Arc<Shutdown>,
) -> std::io::Result<()> {
    if let Some(parent) = socket.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }
    remove_stale_socket(socket)?;

    let listener = tokio::net::UnixListener::bind(socket)?;
    let plugin = Arc::new(Plugin::new(db, ingest));
    let app = plugin_app(Arc::clone(&plugin));

    let (stop_tx, mut stop_rx) = tokio::sync::oneshot::channel::<()>();
    shutdown.on_trigger(move || {
        let _ = stop_tx.send(());
    });

    // The peer is `dockerd` over a local socket — prompt or gone — so the deadlines are not
    // operator-tunable here; they exist so a wedged peer cannot pin a connection forever.
    let limits = Limits::default();
    let graceful = GracefulShutdown::new();
    let mut builder = hyper::server::conn::http1::Builder::new();
    builder.timer(TokioTimer::new());
    builder.header_read_timeout(limits.timeouts.header_deadline());

    loop {
        let stream = tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok((stream, _peer)) => stream,
                Err(_) => continue,
            },
            _ = &mut stop_rx => break,
        };
        let service = hyper::service::service_fn({
            let app = app.clone();
            move |request| {
                let app = app.clone();
                async move {
                    Ok::<_, std::convert::Infallible>(crate::handle(app, request, limits).await)
                }
            }
        });
        let connection = graceful.watch(builder.serve_connection(TokioIo::new(stream), service));
        tokio::spawn(async move {
            let _ = connection.await;
        });
    }

    // Order matters, and it is not the HTTP listener's order. The container readers stop and the
    // ingest queue drains *before* the connection drain, because clearing the stream registry is
    // also what ends the `docker logs -f` responses still open — follow mode exits once its
    // container has no live stream. Draining first would mean waiting out the full timeout on
    // connections that only stop because of this call.
    plugin.shutdown();
    if tokio::time::timeout(shutdown.drain_timeout(), graceful.shutdown())
        .await
        .is_err()
    {
        warn(&format!(
            "in-flight plugin request(s) abandoned after the {:?} shutdown drain",
            shutdown.drain_timeout()
        ));
    }
    // Unlink last, so a restart binds a clean path rather than clearing someone else's leftover.
    let _ = std::fs::remove_file(socket);
    Ok(())
}

/// Unlink a socket left behind by a previous run, which would otherwise fail the bind with
/// `AddrInUse`. Only an actual socket is removed — if the path holds a regular file or a directory,
/// something else owns it and the bind error is the right outcome.
fn remove_stale_socket(socket: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::FileTypeExt;
    match std::fs::symlink_metadata(socket) {
        Ok(meta) if meta.file_type().is_socket() => std::fs::remove_file(socket),
        _ => Ok(()),
    }
}

/// The plugin's route table.
///
/// Deliberately method-agnostic (`any`): Docker posts, but the parser this replaced ignored the
/// method entirely, and a `405` to a daemon that changed its mind would be a worse failure than
/// simply answering.
fn plugin_app(plugin: Arc<Plugin>) -> Router {
    Router::new()
        .route("/Plugin.Activate", any(activate))
        .route("/LogDriver.Capabilities", any(capabilities))
        .route("/LogDriver.StartLogging", any(start_logging))
        .route("/LogDriver.StopLogging", any(stop_logging))
        .route("/LogDriver.ReadLogs", any(read_logs))
        .fallback(not_found)
        .with_state(plugin)
}

/// Run one of the four small JSON endpoints through [`Plugin::route`], off the runtime workers:
/// `StartLogging` blocks for up to [`OPEN_TIMEOUT`] waiting on a FIFO open.
async fn dispatch(plugin: Arc<Plugin>, path: &'static str, body: Bytes) -> Response {
    crate::offload_blocking(move || plugin.route(path, &body)).await
}

async fn activate(State(plugin): State<Arc<Plugin>>, body: Bytes) -> Response {
    dispatch(plugin, "/Plugin.Activate", body).await
}

async fn capabilities(State(plugin): State<Arc<Plugin>>, body: Bytes) -> Response {
    dispatch(plugin, "/LogDriver.Capabilities", body).await
}

async fn start_logging(State(plugin): State<Arc<Plugin>>, body: Bytes) -> Response {
    dispatch(plugin, "/LogDriver.StartLogging", body).await
}

async fn stop_logging(State(plugin): State<Arc<Plugin>>, body: Bytes) -> Response {
    dispatch(plugin, "/LogDriver.StopLogging", body).await
}

async fn not_found() -> Response {
    Response::text(404, "not found")
}

/// `/LogDriver.ReadLogs` — stream a container's stored logs back as length-prefixed frames.
///
/// [`readlogs::stream`] is blocking and generic over `io::Write`, and under `Follow` it runs until
/// the container stops or the client leaves. It goes on a blocking task writing into a bounded
/// channel; the response body is that channel. Nothing about the generator changes.
async fn read_logs(State(plugin): State<Arc<Plugin>>, body: Bytes) -> axum::response::Response {
    let (tx, rx) = tokio::sync::mpsc::channel::<Bytes>(FRAME_QUEUE);
    let db = Arc::clone(&plugin.db);
    tokio::task::spawn_blocking(move || {
        let mut sink = FrameSink::new(tx);
        let watcher = Arc::clone(&plugin);
        let outcome = readlogs::stream(&db, &body, &mut sink, |id| watcher.is_active(id))
            .and_then(|()| sink.flush());
        if let Err(e) = outcome
            // The client hanging up mid-stream is how a `docker logs` normally ends, not a fault.
            && !matches!(
                e.kind(),
                std::io::ErrorKind::BrokenPipe | std::io::ErrorKind::TimedOut
            )
        {
            warn(&format!("ReadLogs stream ended: {e}"));
        }
    });

    axum::response::Response::builder()
        .status(200)
        .header(header::CONTENT_TYPE, READLOGS_CONTENT_TYPE)
        .body(Body::new(FrameBody { rx }))
        .unwrap_or_else(|_| Response::text(500, "cannot start the log stream").into_response())
}

/// The content type of the framed `ReadLogs` body.
const READLOGS_CONTENT_TYPE: &str = "application/x-json-stream";

/// Frame batches buffered ahead of a slow `docker logs` client before the generator has to wait.
/// Small on purpose: the point of streaming is that a busy container's history is not held in memory.
const FRAME_QUEUE: usize = 16;

/// How long one write may wait for a client that has stopped reading before the stream is abandoned.
/// This is the backpressure bound that the socket write timeout used to provide — without it a
/// `docker logs -f` whose client vanished without closing would hold a blocking task indefinitely.
const STREAM_STALL: Duration = Duration::from_secs(30);

/// How long to wait between attempts when the frame queue is full.
const STREAM_RETRY: Duration = Duration::from_millis(10);

/// The blocking `io::Write` sink [`readlogs::stream`] writes frames into, backed by the response
/// body's channel.
struct FrameSink {
    tx: tokio::sync::mpsc::Sender<Bytes>,
    buffered: Vec<u8>,
}

impl FrameSink {
    fn new(tx: tokio::sync::mpsc::Sender<Bytes>) -> FrameSink {
        FrameSink {
            tx,
            buffered: Vec::new(),
        }
    }
}

impl Write for FrameSink {
    /// Accumulate: `write_entry` emits a frame in several small writes, and `readlogs` flushes once
    /// per batch, which is the granularity worth sending.
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        self.buffered.extend_from_slice(data);
        Ok(data.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        if self.buffered.is_empty() {
            return Ok(());
        }
        let mut chunk = Bytes::from(std::mem::take(&mut self.buffered));
        let deadline = Instant::now() + STREAM_STALL;
        loop {
            match self.tx.try_send(chunk) {
                Ok(()) => return Ok(()),
                // The body was dropped: the client is gone.
                Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::BrokenPipe,
                        "the docker logs client closed the stream",
                    ));
                }
                Err(tokio::sync::mpsc::error::TrySendError::Full(returned)) => {
                    if Instant::now() >= deadline {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::TimedOut,
                            "the docker logs client stopped reading",
                        ));
                    }
                    chunk = returned;
                    std::thread::sleep(STREAM_RETRY);
                }
            }
        }
    }
}

/// The `ReadLogs` response body: whatever the generator task has put on the channel, until it ends.
struct FrameBody {
    rx: tokio::sync::mpsc::Receiver<Bytes>,
}

impl http_body::Body for FrameBody {
    type Data = Bytes;
    /// The generator reports its own failures; a stream that ends early is an ended body, not a
    /// body error — there is no way to signal one mid-response anyway.
    type Error = std::convert::Infallible;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<http_body::Frame<Bytes>, Self::Error>>> {
        self.get_mut()
            .rx
            .poll_recv(cx)
            .map(|frame| frame.map(|bytes| Ok(http_body::Frame::data(bytes))))
    }
}

/// A container's live FIFO reader.
struct Stream {
    container_id: String,
    stop: Arc<AtomicBool>,
}

/// The driver: shared `Db`, the ingest worker, and the set of live container streams.
pub(crate) struct Plugin {
    db: Arc<Db>,
    ingest: Arc<Ingestor>,
    /// Keyed by the FIFO path, which is what `StopLogging` identifies a stream by.
    streams: Mutex<HashMap<String, Stream>>,
}

impl Plugin {
    pub(crate) fn new(db: Arc<Db>, ingest: IngestConfig) -> Plugin {
        Plugin {
            ingest: Arc::new(Ingestor::start(db.clone(), ingest)),
            db,
            streams: Mutex::new(HashMap::new()),
        }
    }

    /// Dispatch one plugin request. Exposed to tests so the endpoints can be exercised without a
    /// socket, mirroring the HTTP server's `route`.
    pub(crate) fn route(&self, path: &str, body: &[u8]) -> Response {
        let json = |body: Vec<u8>| Response::with_content_type(200, PLUGIN_CONTENT_TYPE, body);
        match path {
            "/Plugin.Activate" => json(br#"{"Implements":["LogDriver"]}"#.to_vec()),
            // `ReadLogs: true` is what makes `docker logs` call into this plugin instead of
            // reporting that the driver does not support reading.
            "/LogDriver.Capabilities" => json(br#"{"Err":"","Cap":{"ReadLogs":true}}"#.to_vec()),
            "/LogDriver.StartLogging" => json(json::err_response(
                self.start_logging(body).err().as_deref(),
            )),
            "/LogDriver.StopLogging" => {
                json(json::err_response(self.stop_logging(body).err().as_deref()))
            }
            _ => Response::text(404, "not found"),
        }
    }

    /// Wind the driver down: stop every container's FIFO reader and drain the ingest queue into the
    /// DB, so the caller can close it knowing nothing read is still in flight.
    ///
    /// The readers are deliberately **not** joined. A reader is parked in a blocking read on a FIFO
    /// whose writer — the still-running container — has it open, so only the process exiting ends
    /// that read; waiting for one would turn a `docker stop` of the plugin into a hang. They observe
    /// `stop` between frames, and `Ingestor::shutdown` refuses whatever a late one produces, so no
    /// record reaches a DB that is closing.
    ///
    /// Clearing the stream registry also ends any `docker logs -f` this plugin is serving: follow mode
    /// stops once the container has no live stream, which is what lets those connections drain.
    pub(crate) fn shutdown(&self) {
        let streams = {
            let mut streams = self.streams.lock().expect("docker plugin stream registry");
            std::mem::take(&mut *streams)
        };
        for (_fifo, stream) in streams {
            stream.stop.store(true, Ordering::Relaxed);
        }
        self.ingest.shutdown();
    }

    /// Whether `container_id` still has a live FIFO reader — follow mode's "is it still running?".
    fn is_active(&self, container_id: &str) -> bool {
        self.streams
            .lock()
            .expect("docker plugin stream registry")
            .values()
            .any(|s| s.container_id == container_id)
    }

    /// `/LogDriver.StartLogging` — open the container's FIFO and start pumping it into the DB.
    fn start_logging(&self, body: &[u8]) -> Result<(), String> {
        let root = json::parse(body);
        let path = json::string(&root, "File");
        if path.is_empty() {
            return Err("StartLogging request has no File".to_owned());
        }
        let info = json::field(&root, "Info")
            .cloned()
            .unwrap_or(imbh::AnyValue::Map(Vec::new()));
        let container = Arc::new(Container::from_info(&info));

        let stop = Arc::new(AtomicBool::new(false));
        let container_id = container.id.clone();

        // Claim the FIFO path *before* spawning, not after the open confirms. Docker will not start
        // two streams on one FIFO, but a retried request arriving while the first is still inside
        // `OPEN_TIMEOUT` would otherwise slip past a check-then-insert and leave two readers on one
        // stream — every line ingested twice.
        {
            let mut streams = self.streams.lock().expect("docker plugin stream registry");
            if streams.contains_key(&path) {
                return Ok(());
            }
            streams.insert(
                path.clone(),
                Stream {
                    container_id: container_id.clone(),
                    stop: stop.clone(),
                },
            );
        }
        // From here on, every failure must release that claim.
        let release = || {
            self.streams
                .lock()
                .expect("docker plugin stream registry")
                .remove(&path);
        };

        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (fifo, ingest) = (PathBuf::from(&path), self.ingest.clone());
        if let Err(e) = std::thread::Builder::new()
            .name(format!("imbh-docker-fifo-{}", short(&container_id)))
            .spawn(move || pump(&fifo, container, &ingest, &stop, ready_tx))
        {
            release();
            return Err(format!("cannot spawn a reader for {path}: {e}"));
        }

        // A failed open comes back as an error the daemon shows the user; a slow one does not hold
        // up the container.
        match ready_rx.recv_timeout(OPEN_TIMEOUT) {
            Ok(Ok(())) | Err(RecvTimeoutError::Timeout) => Ok(()),
            Ok(Err(e)) => {
                release();
                Err(e)
            }
            // The reader thread vanished without reporting — treat as a failed start.
            Err(RecvTimeoutError::Disconnected) => {
                release();
                Err(format!("log reader for {path} exited during startup"))
            }
        }
    }

    /// `/LogDriver.StopLogging` — deregister the stream and ask its reader to stop.
    ///
    /// The reader is not joined: it is parked in a blocking read on the FIFO, and Docker closes its
    /// write end right after this call, which ends the read. The flag covers the case where entries
    /// are still arriving — the reader checks it between frames and drains what it has buffered.
    fn stop_logging(&self, body: &[u8]) -> Result<(), String> {
        let path = json::string(&json::parse(body), "File");
        if path.is_empty() {
            return Err("StopLogging request has no File".to_owned());
        }
        if let Some(stream) = self
            .streams
            .lock()
            .expect("docker plugin stream registry")
            .remove(&path)
        {
            stream.stop.store(true, Ordering::Relaxed);
        }
        Ok(())
    }
}

/// Read one container's FIFO to the end, reassembling split lines and queueing every complete line
/// for ingest. Reports the result of opening the FIFO on `ready` so `StartLogging` can answer.
fn pump(
    path: &Path,
    container: Arc<Container>,
    ingest: &Ingestor,
    stop: &AtomicBool,
    ready: std::sync::mpsc::Sender<Result<(), String>>,
) {
    let fifo = match File::open(path) {
        Ok(f) => {
            let _ = ready.send(Ok(()));
            f
        }
        Err(e) => {
            let _ = ready.send(Err(format!(
                "cannot open log stream {}: {e}",
                path.display()
            )));
            return;
        }
    };
    drop(ready);

    let mut reader = EntryReader::new(BufReader::new(fifo));
    let mut assembler = PartialAssembler::default();
    while !stop.load(Ordering::Relaxed) {
        match reader.next_entry() {
            Ok(Some(wire)) => {
                if let Some(line) = assembler.push(wire) {
                    let record = container.record(&line);
                    if !ingest.send(container.clone(), record) {
                        return; // the ingest worker is gone; the DB is closing
                    }
                }
            }
            Ok(None) => break, // the container closed its output
            Err(e) => {
                warn(&format!("log stream {} ended: {e}", container.name_or_id()));
                break;
            }
        }
    }

    // A line that was still being reassembled when the container exited is worth more in the DB
    // than in a dropped buffer.
    for line in assembler.drain() {
        let record = container.record(&line);
        if !ingest.send(container.clone(), record) {
            return;
        }
    }
}

/// The first 12 characters of a container id — Docker's own short form, used for thread names.
fn short(id: &str) -> String {
    id.chars().take(12).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plugin() -> Plugin {
        let db = Db::in_memory().open().expect("open in-memory db");
        Plugin::new(db, IngestConfig::default())
    }

    fn body(resp: &Response) -> String {
        String::from_utf8(resp.body.clone()).expect("utf-8 body")
    }

    #[test]
    fn activate_declares_the_log_driver_interface() {
        let resp = plugin().route("/Plugin.Activate", b"");
        assert_eq!(resp.status, 200);
        assert_eq!(resp.content_type, PLUGIN_CONTENT_TYPE);
        assert_eq!(body(&resp), r#"{"Implements":["LogDriver"]}"#);
    }

    #[test]
    fn capabilities_advertise_read_logs() {
        let resp = plugin().route("/LogDriver.Capabilities", b"");
        assert_eq!(body(&resp), r#"{"Err":"","Cap":{"ReadLogs":true}}"#);
    }

    #[test]
    fn unknown_endpoints_are_404() {
        assert_eq!(plugin().route("/LogDriver.Nope", b"").status, 404);
    }

    #[test]
    fn start_logging_without_a_file_is_a_protocol_error_not_a_crash() {
        let resp = plugin().route("/LogDriver.StartLogging", br#"{"Info":{}}"#);
        // The plugin contract: HTTP 200, the failure in `Err`.
        assert_eq!(resp.status, 200);
        assert!(body(&resp).contains("no File"), "got {}", body(&resp));
    }

    #[test]
    fn start_logging_reports_an_unopenable_fifo() {
        let p = plugin();
        let resp = p.route(
            "/LogDriver.StartLogging",
            br#"{"File":"/nonexistent/imbh/fifo","Info":{"ContainerID":"abc"}}"#,
        );
        assert_eq!(resp.status, 200);
        assert!(
            body(&resp).contains("cannot open log stream"),
            "got {}",
            body(&resp)
        );
        // A stream that never opened must not be registered as live.
        assert!(!p.is_active("abc"));
    }

    #[test]
    fn stop_logging_without_a_file_is_a_protocol_error() {
        let resp = plugin().route("/LogDriver.StopLogging", b"{}");
        assert!(body(&resp).contains("no File"), "got {}", body(&resp));
    }

    #[test]
    fn stopping_an_unknown_stream_succeeds() {
        // Docker may stop a stream the plugin never started (e.g. after a plugin restart); that is
        // not an error the daemon should surface.
        let resp = plugin().route("/LogDriver.StopLogging", br#"{"File":"/run/whatever"}"#);
        assert_eq!(body(&resp), r#"{"Err":""}"#);
    }

    #[test]
    fn malformed_json_does_not_panic_any_endpoint() {
        let p = plugin();
        for path in [
            "/Plugin.Activate",
            "/LogDriver.Capabilities",
            "/LogDriver.StartLogging",
            "/LogDriver.StopLogging",
        ] {
            assert_eq!(p.route(path, b"{not json").status, 200);
        }
    }
}

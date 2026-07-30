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
//! Thread-per-connection for the plugin API (as in the HTTP server), plus one reader thread per
//! live container FIFO, all funneling into a single batching [`ingest::Ingestor`] worker so ingest
//! cost is per batch, not per line. Nothing here is on the query path.
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
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::RecvTimeoutError;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use imbh::Db;

use entry::{EntryReader, PartialAssembler};
use ingest::{Container, IngestConfig, Ingestor};

use crate::Response;

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
pub fn serve_plugin(db: Arc<Db>, socket: impl AsRef<Path>) -> std::io::Result<()> {
    serve_plugin_with(db, socket, IngestConfig::default())
}

/// [`serve_plugin`] with the ingest batching tuned.
pub fn serve_plugin_with(
    db: Arc<Db>,
    socket: impl AsRef<Path>,
    ingest: IngestConfig,
) -> std::io::Result<()> {
    let socket = socket.as_ref();
    if let Some(parent) = socket.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }
    remove_stale_socket(socket)?;

    let listener = UnixListener::bind(socket)?;
    let plugin = Arc::new(Plugin::new(db, ingest));
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let plugin = plugin.clone();
        std::thread::spawn(move || {
            if let Err(e) = handle_conn(&plugin, stream) {
                warn(&format!("connection error: {e}"));
            }
        });
    }
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

fn handle_conn(plugin: &Arc<Plugin>, mut stream: UnixStream) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let Some((_method, path, body)) = crate::read_request(&mut reader)? else {
        return Ok(());
    };

    // ReadLogs streams frames for as long as the client wants them, so it writes its own header and
    // owns the socket; every other endpoint is a single small JSON reply.
    if path == "/LogDriver.ReadLogs" {
        stream.write_all(
            b"HTTP/1.1 200 OK\r\nContent-Type: application/x-json-stream\r\nConnection: close\r\n\r\n",
        )?;
        stream.flush()?;
        let watcher = plugin.clone();
        return readlogs::stream(&plugin.db, &body, &mut stream, |id| watcher.is_active(id));
    }

    let resp = plugin.route(&path, &body);
    crate::write_response(&mut stream, &resp)
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

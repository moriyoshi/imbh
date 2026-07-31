//! imbhd — the reference HTTP server (ARCHITECTURE.md §10.16).
//!
//! A deliberately tiny HTTP/1.1 server over `std::net` (thread-per-connection), showing one way a
//! host can expose the imbh library over HTTP. It is **reference wiring, not the product** (§10.1):
//! no axum/hyper, so it adds no heavy dependencies and keeps the footprint story intact.
//!
//! Routes:
//! - `POST /v1/logs` · `/v1/traces` · `/v1/metrics` — OTLP/HTTP protobuf ingest (uncompressed).
//! - `POST /api/query` — a SQL string body → JSON rows.
//! - `GET /stats` — DB operational stats (per-table counts + buffer/WAL bytes + durable LSN) as JSON.
//! - `POST /admin/flush` · `/admin/compact` — maintenance actions (seal the buffer; force-merge
//!   segments). These are unauthenticated by design — a real deployment gates `/admin/*` itself.
//! - `GET /health` — liveness.
//!
//! Unlike the library, `imbhd` runs a flush scheduler by default — see [`flush_policy`] and
//! [`maintenance_interval`] for the `IMBH_FLUSH` / `IMBH_MAINTENANCE_INTERVAL` knobs behind it. A
//! collector that never seals keeps every row in the buffer + WAL, so `/admin/flush` should be a
//! manual override, not the only path to Parquet.
//!
//! OTLP/gRPC is available on a second port behind the optional `grpc` feature (see [`grpc`]); the
//! default build carries no gRPC transport. A Docker logging-driver plugin endpoint is available
//! behind the optional `docker` feature (see [`docker`]). Not handled here (follow-ups): gzip request
//! bodies, TLS, and the OTLP partial-success response shape.
//!
//! Every endpoint stops on a shared [`Shutdown`] token: `SIGINT`/`SIGTERM` stop the accept loops,
//! in-flight requests get a bounded drain, and `imbhd` seals the buffer with `Db::close()` before it
//! exits. See the [`shutdown`] module for the mechanism and [`serve_until`] for the HTTP side.

#[cfg(all(feature = "docker", unix))]
pub mod docker;
#[cfg(feature = "grpc")]
pub mod grpc;
pub mod shutdown;

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::Duration;

use std::sync::Arc;

pub use shutdown::{DEFAULT_DRAIN_TIMEOUT, Shutdown};
use shutdown::{InFlight, wake_tcp_listener};

use imbh::arrow::array::Array;
use imbh::arrow::record_batch::RecordBatch;
use imbh::arrow::util::display::{ArrayFormatter, FormatOptions};
use imbh::{Db, FlushPolicy};

/// A minimal HTTP response.
pub struct Response {
    pub status: u16,
    pub content_type: String,
    pub body: Vec<u8>,
}

impl Response {
    fn text(status: u16, s: &str) -> Self {
        Response {
            status,
            content_type: "text/plain".to_owned(),
            body: s.as_bytes().to_vec(),
        }
    }
    fn json(status: u16, body: Vec<u8>) -> Self {
        Response {
            status,
            content_type: "application/json".to_owned(),
            body,
        }
    }
    /// A response with an explicit content type — used by the Docker plugin endpoint, which must
    /// answer in `application/vnd.docker.plugins.v1.1+json`.
    #[cfg(all(feature = "docker", unix))]
    pub(crate) fn with_content_type(status: u16, content_type: &str, body: Vec<u8>) -> Self {
        Response {
            status,
            content_type: content_type.to_owned(),
            body,
        }
    }
}

/// Resolve `imbhd`'s HTTP listen address from the positional argument and the `IMBH_LISTEN_ADDR`
/// environment variable, in that order of precedence, falling back to `default`.
///
/// The environment variable exists for the Docker plugin (ARCHITECTURE.md §10.16). A managed
/// plugin's `entrypoint` args are baked into its `config.json` and cannot be changed without
/// rebuilding the plugin, whereas `env` entries declared `settable` can be changed at any time with
/// `docker plugin set` — so the listen address has to arrive as an environment variable to be
/// operator-tunable at all.
///
/// An **empty** value (`IMBH_LISTEN_ADDR=`) is not a missing value: it means *do not listen on TCP*.
/// That is the private posture — the log-driver plugin keeps working over its Unix socket while the
/// process opens no network port at all. Returns `None` in that case.
pub fn listen_addr(arg: Option<String>, env: Option<String>, default: &str) -> Option<String> {
    let chosen = arg
        .or(env)
        .unwrap_or_else(|| default.to_owned())
        .trim()
        .to_owned();
    (!chosen.is_empty()).then_some(chosen)
}

/// `imbhd`'s default flush policy: seal at least every 5 seconds, plus the memory-budget byte
/// threshold the engine applies anyway. A collector is a write-mostly process whose rows are only in
/// Parquet (and whose WAL is only reclaimable) once the buffer is sealed, so it defaults to a
/// scheduler, unlike the library — where an embedder must opt into every background thread.
pub const DEFAULT_FLUSH: &str = "interval=5s";

/// `imbhd`'s default retention cadence: how often the scheduler applies the retention policy. Nothing
/// to do unless a retention policy is configured, so this can be relaxed.
pub const DEFAULT_MAINTENANCE_INTERVAL: Duration = Duration::from_secs(60);

/// Resolve `imbhd`'s flush policy from `IMBH_FLUSH`, falling back to [`DEFAULT_FLUSH`].
///
/// The value is a [`FlushPolicy`] spec — comma-separated triggers that OR together, e.g.
/// `interval=5s`, `buffer=16MiB`, `rows=50000`, `wal=64MiB`, `idle=2s`, `tick=250ms` — or the single
/// word `manual` to seal only on `POST /admin/flush` and shutdown. Like the listen addresses, it is an
/// environment variable rather than an argument because a managed Docker plugin can retune `env`
/// entries with `docker plugin set` but cannot change its frozen entrypoint arguments.
///
/// An **empty** value means "unset" (the default applies), matching how an unset variable reads. A
/// malformed spec is an error, never a silent fallback: a typo in a deployment's config must not
/// quietly leave the buffer unsealed.
pub fn flush_policy(env: Option<String>) -> imbh::Result<FlushPolicy> {
    let spec = env.unwrap_or_default();
    let spec = spec.trim();
    if spec.is_empty() {
        DEFAULT_FLUSH.parse()
    } else {
        spec.parse()
    }
}

/// Resolve the retention cadence from `IMBH_MAINTENANCE_INTERVAL` (a duration such as `60s`, `5m`),
/// falling back to [`DEFAULT_MAINTENANCE_INTERVAL`]. Empty means unset; a malformed value is an error.
pub fn maintenance_interval(env: Option<String>) -> imbh::Result<Duration> {
    let value = env.unwrap_or_default();
    let value = value.trim();
    if value.is_empty() {
        return Ok(DEFAULT_MAINTENANCE_INTERVAL);
    }
    // A zero interval would run retention every slice of the scheduler loop; treat it as "as often as
    // the scheduler wakes" by flooring it at one tick rather than rejecting it.
    Ok(imbh::parse_duration(value)?.max(FlushPolicy::DEFAULT_TICK))
}

/// `imbhd`'s default drain: how long a listener waits for in-flight requests at shutdown.
pub const DEFAULT_SHUTDOWN_TIMEOUT: Duration = DEFAULT_DRAIN_TIMEOUT;

/// Resolve the shutdown drain from `IMBH_SHUTDOWN_TIMEOUT` (a duration such as `5s`, `500ms`),
/// falling back to [`DEFAULT_SHUTDOWN_TIMEOUT`]. Empty means unset; a malformed value is an error.
///
/// `0` is meaningful and kept: stop accepting and return without waiting for anything in flight. It
/// is an environment variable for the same reason the listen addresses are — a managed Docker plugin
/// can retune `env` entries but not its frozen entrypoint arguments — and it is what an operator
/// tunes against the supervisor's own patience (Docker sends `SIGKILL` 10s after `SIGTERM`).
pub fn shutdown_timeout(env: Option<String>) -> imbh::Result<Duration> {
    let value = env.unwrap_or_default();
    let value = value.trim();
    if value.is_empty() {
        return Ok(DEFAULT_SHUTDOWN_TIMEOUT);
    }
    imbh::parse_duration(value)
}

/// `imbhd`'s default header-phase deadline: how long a client gets to deliver the request line and
/// headers, in total. Generous for a localhost/bridge collector; the point is that "connected and went
/// quiet" is bounded at all.
pub const DEFAULT_HEADER_TIMEOUT: Duration = Duration::from_secs(10);

/// `imbhd`'s default body deadline: how long a single read of a request body — or a single write of a
/// response — may stall. Per read, not per request, so a large upload that keeps making progress is
/// never cut off for being large.
pub const DEFAULT_BODY_TIMEOUT: Duration = Duration::from_secs(30);

/// Per-connection I/O deadlines, so a client cannot park a server thread indefinitely.
///
/// Thread-per-connection means an idle connection costs a thread, and the parser blocks in
/// `read_line`/`read_exact` with no deadline of its own. The two phases want different rules, which is
/// why this is two values rather than one socket timeout:
///
/// - [`IoTimeouts::header`] bounds the request line + headers **in total**. A per-read allowance is not
///   enough here: a client trickling one byte per allowance is never idle, yet holds a thread forever.
/// - [`IoTimeouts::body`] is a **per-read** allowance for the body, and the write allowance for the
///   response. A 50 MiB OTLP body over a slow link must not be punished for its size — only for
///   stalling — and a total deadline cannot tell those apart.
///
/// `Duration::ZERO` in either field means *no deadline* for that phase ([`IoTimeouts::DISABLED`] is
/// both), which is the pre-timeout behaviour and the right choice for a host fronting `imbhd` with a
/// proxy that already sheds slow clients.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IoTimeouts {
    /// Total time allowed for the request line and headers.
    pub header: Duration,
    /// Longest a single body read, or a single response write, may stall.
    pub body: Duration,
}

impl Default for IoTimeouts {
    fn default() -> Self {
        IoTimeouts {
            header: DEFAULT_HEADER_TIMEOUT,
            body: DEFAULT_BODY_TIMEOUT,
        }
    }
}

impl IoTimeouts {
    /// No deadline on any phase: connections may stall for as long as the client keeps them open.
    pub const DISABLED: IoTimeouts = IoTimeouts {
        header: Duration::ZERO,
        body: Duration::ZERO,
    };

    /// The socket timeout for the body/response phase — `None` when disabled, which is also how
    /// `set_read_timeout`/`set_write_timeout` spell "block forever".
    fn socket_timeout(self) -> Option<Duration> {
        (!self.body.is_zero()).then_some(self.body)
    }
}

/// Resolve the connection deadlines from `IMBH_HEADER_TIMEOUT` / `IMBH_BODY_TIMEOUT` (durations such
/// as `10s`, `500ms`), each falling back to its default. Empty means unset; `0` disables that phase; a
/// malformed value is an error, never a silent fallback.
pub fn io_timeouts(header: Option<String>, body: Option<String>) -> imbh::Result<IoTimeouts> {
    let resolve = |env: Option<String>, default: Duration| -> imbh::Result<Duration> {
        let value = env.unwrap_or_default();
        let value = value.trim();
        if value.is_empty() {
            return Ok(default);
        }
        imbh::parse_duration(value)
    };
    Ok(IoTimeouts {
        header: resolve(header, DEFAULT_HEADER_TIMEOUT)?,
        body: resolve(body, DEFAULT_BODY_TIMEOUT)?,
    })
}

/// Serve `db` on `addr` (e.g. `127.0.0.1:4318`) until the process exits. Thread-per-connection;
/// each connection drives the async `Db` API on its own current-thread runtime.
///
/// Never returns on its own; a host that wants to stop serving wants [`serve_until`].
pub fn serve(db: Arc<Db>, addr: &str) -> std::io::Result<()> {
    serve_until(db, addr, Shutdown::new())
}

/// Serve `db` on `addr` until `shutdown` trips, then stop accepting, give the in-flight requests up
/// to [`Shutdown::drain_timeout`] to finish, and return.
///
/// Binding happens before anything else, so a bind error still surfaces to the caller. Once bound,
/// the listener registers its own wake-up with the token ([`Shutdown::on_trigger`]): `accept` stays a
/// *blocking* call — no poll tick added to the latency of a protocol that opens one connection per
/// request — and the throwaway connection at trigger time is what gets the thread out of it.
///
/// A request that is still being read when the drain expires is abandoned, not waited for: the reply
/// is lost but the ingest it asked for is already durable or not at all (`Db::ingest_otlp_*` is what
/// decides that, not this loop), so an OTLP client's retry is the correct resolution either way.
///
/// Connections get the default [`IoTimeouts`]; [`serve_with_until`] takes them as an argument.
pub fn serve_until(db: Arc<Db>, addr: &str, shutdown: Arc<Shutdown>) -> std::io::Result<()> {
    serve_with_until(db, addr, IoTimeouts::default(), shutdown)
}

/// [`serve_until`] with the per-connection deadlines set explicitly — what `imbhd` calls, so
/// `IMBH_HEADER_TIMEOUT` / `IMBH_BODY_TIMEOUT` reach the connections.
pub fn serve_with_until(
    db: Arc<Db>,
    addr: &str,
    timeouts: IoTimeouts,
    shutdown: Arc<Shutdown>,
) -> std::io::Result<()> {
    let listener = TcpListener::bind(addr)?;
    let local = listener.local_addr()?;
    shutdown.on_trigger(move || wake_tcp_listener(local));

    let in_flight = Arc::new(InFlight::default());
    while !shutdown.is_triggered() {
        let stream = match listener.accept() {
            Ok((stream, _peer)) => stream,
            // Per-connection errors (`ECONNABORTED`, `EMFILE`); the listener itself is still good.
            Err(_) => continue,
        };
        // Either the wake-up connection or a client that raced the shutdown. Dropping it unserved is
        // the honest answer — the reply would arrive on a socket we are about to stop reading.
        if shutdown.is_triggered() {
            break;
        }
        let db = db.clone();
        let busy = in_flight.enter();
        std::thread::spawn(move || {
            let _busy = busy;
            let rt = tokio::runtime::Builder::new_current_thread()
                .build()
                .expect("build connection runtime");
            let _ = rt.block_on(handle_conn(db, stream, timeouts));
        });
    }
    let left = in_flight.drain(shutdown.drain_timeout());
    if left > 0 {
        warn(&format!(
            "{left} in-flight HTTP connection(s) abandoned after the {:?} shutdown drain",
            shutdown.drain_timeout()
        ));
    }
    Ok(())
}

/// Report a server-level problem. Routed through `tracing` when `imbhd` is built with that feature,
/// so it joins the rest of the server's instrumentation; plain stderr otherwise.
pub(crate) fn warn(message: &str) {
    #[cfg(feature = "tracing")]
    tracing::warn!(target: "imbh_server", "{message}");
    #[cfg(not(feature = "tracing"))]
    eprintln!("imbhd: {message}");
}

async fn handle_conn(
    db: Arc<Db>,
    mut stream: TcpStream,
    timeouts: IoTimeouts,
) -> std::io::Result<()> {
    let mut reader = BufReader::new(Armed::new(stream.try_clone()?, timeouts));
    // Set before the read, so it also bounds the 408 below: a client that stalls sending is a fair bet
    // to stall reading too, and a blocked write parks this thread exactly as a blocked read would.
    stream.set_write_timeout(timeouts.socket_timeout())?;
    let request = match read_request(&mut reader) {
        Ok(Some(request)) => request,
        Ok(None) => return Ok(()),
        // A client that stalled mid-request gets the status that says so — best-effort, since it may
        // well be gone. Every other error is a broken connection with nobody left to answer.
        Err(e) if is_timeout(&e) => {
            return write_response(&mut stream, &Response::text(408, "request timed out"));
        }
        Err(e) => return Err(e),
    };
    let (method, path, body) = request;
    let resp = route(&db, &method, &path, &body).await;
    write_response(&mut stream, &resp)
}

/// Whether an I/O error is a deadline expiring. A socket read/write timeout surfaces as `WouldBlock`
/// on Linux and `TimedOut` on Windows/macOS, so both spellings count.
pub(crate) fn is_timeout(e: &std::io::Error) -> bool {
    matches!(
        e.kind(),
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
    )
}

/// Dispatch a request to the imbh library. Exposed for testing without sockets.
#[cfg_attr(
    feature = "tracing",
    tracing::instrument(
        level = "info",
        name = "request",
        skip_all,
        fields(method, path, status = tracing::field::Empty)
    )
)]
pub async fn route(db: &Arc<Db>, method: &str, path: &str, body: &[u8]) -> Response {
    let resp = match (method, path) {
        ("GET", "/health") | ("GET", "/") => Response::text(200, "ok"),
        ("POST", "/v1/logs") => ingest_response(db.ingest_otlp_logs(body).await),
        ("POST", "/v1/traces") => ingest_response(db.ingest_otlp_traces(body).await),
        ("POST", "/v1/metrics") => ingest_response(db.ingest_otlp_metrics(body).await),
        ("POST", "/api/query") => query_response(db, body).await,
        ("GET", "/stats") => stats_response(db).await,
        ("POST", "/admin/flush") => match db.flush().await {
            Ok(()) => Response::json(200, b"{\"flushed\":true}".to_vec()),
            Err(e) => error_response(&e),
        },
        ("POST", "/admin/compact") => match db.compact().await {
            Ok(r) => Response::json(
                200,
                format!(
                    "{{\"segments_merged\":{},\"segments_created\":{}}}",
                    r.segments_merged, r.segments_created
                )
                .into_bytes(),
            ),
            Err(e) => error_response(&e),
        },
        _ => Response::text(404, "not found"),
    };
    #[cfg(feature = "tracing")]
    tracing::Span::current().record("status", resp.status);
    resp
}

/// `GET /stats` — the DB's operational stats as JSON (VM `/status/tsdb` analogue): per-table
/// segment/row/buffer counts and time span, plus buffer bytes, WAL bytes, and the durable LSN.
async fn stats_response(db: &Arc<Db>) -> Response {
    let stats = match db.stats().await {
        Ok(s) => s,
        Err(e) => return error_response(&e),
    };
    let opt = |v: Option<i64>| v.map_or("null".to_owned(), |n| n.to_string());
    let mut tables = String::from("[");
    for (i, t) in stats.tables.iter().enumerate() {
        if i > 0 {
            tables.push(',');
        }
        use std::fmt::Write as _;
        let _ = write!(
            tables,
            "{{\"table\":{},\"segment_count\":{},\"segment_rows\":{},\"buffer_rows\":{},\
             \"min_time_unix_nano\":{},\"max_time_unix_nano\":{}}}",
            json_string(t.table.as_str()),
            t.segment_count,
            t.segment_rows,
            t.buffer_rows,
            opt(t.min_time_unix_nano),
            opt(t.max_time_unix_nano),
        );
    }
    tables.push(']');
    let body = format!(
        "{{\"buffer_bytes\":{},\"wal_bytes\":{},\"durable_lsn\":{},\"tables\":{}}}",
        stats.buffer_bytes,
        stats.wal_bytes,
        stats.durable_lsn.map_or(0, |l| l.get()),
        tables,
    );
    Response::json(200, body.into_bytes())
}

fn ingest_response(result: imbh::Result<imbh::IngestReceipt>) -> Response {
    match result {
        Ok(r) => Response::json(
            200,
            format!(
                "{{\"accepted\":{},\"rejected\":{},\"durable\":{},\"queued\":{}}}",
                r.accepted,
                r.rejected,
                r.durable,
                r.is_queued()
            )
            .into_bytes(),
        ),
        Err(e) => error_response(&e),
    }
}

async fn query_response(db: &Arc<Db>, body: &[u8]) -> Response {
    let sql = match std::str::from_utf8(body) {
        Ok(s) => s,
        Err(_) => return Response::text(400, "query body is not UTF-8"),
    };
    match db.sql(sql).collect().await {
        Ok(batches) => Response::json(200, batches_to_json(&batches)),
        Err(e) => error_response(&e),
    }
}

/// Map an imbh error to an HTTP status using the §10.3 classifiers: 404 not-found, 400 user
/// error, 500 otherwise.
fn error_response(e: &imbh::Error) -> Response {
    let status = if e.is_not_found() {
        404
    } else if e.is_user_error() {
        400
    } else {
        500
    };
    Response::json(
        status,
        format!("{{\"error\":{}}}", json_string(&e.to_string())).into_bytes(),
    )
}

// ── JSON serialization of query results ─────────────────────────────────────────────────

/// Serialize result batches into a JSON array of row objects. Numeric columns render as JSON
/// numbers; everything else as JSON strings (via arrow's value formatter); nulls as `null`.
fn batches_to_json(batches: &[RecordBatch]) -> Vec<u8> {
    let mut out = String::from("[");
    let opts = FormatOptions::default();
    let mut first_row = true;
    for batch in batches {
        let names: Vec<String> = batch
            .schema()
            .fields()
            .iter()
            .map(|f| f.name().clone())
            .collect();
        // A column whose type arrow can't build a formatter for renders as `null` rather than
        // panicking the connection (`.ok()` instead of `.expect(...)`). Every type imbh emits is
        // supported, so this is defensive.
        let formatters: Vec<Option<ArrayFormatter>> = batch
            .columns()
            .iter()
            .map(|c| ArrayFormatter::try_new(c, &opts).ok())
            .collect();
        for row in 0..batch.num_rows() {
            if !first_row {
                out.push(',');
            }
            first_row = false;
            out.push('{');
            for (col, name) in names.iter().enumerate() {
                if col > 0 {
                    out.push(',');
                }
                out.push_str(&json_string(name));
                out.push(':');
                let array = batch.column(col);
                match formatters[col].as_ref() {
                    Some(f) if !array.is_null(row) => {
                        let value = f.value(row).to_string();
                        if is_numeric(array.data_type()) {
                            out.push_str(&value);
                        } else {
                            out.push_str(&json_string(&value));
                        }
                    }
                    _ => out.push_str("null"),
                }
            }
            out.push('}');
        }
    }
    out.push(']');
    out.into_bytes()
}

fn is_numeric(dt: &imbh::arrow::datatypes::DataType) -> bool {
    use imbh::arrow::datatypes::DataType::*;
    matches!(
        dt,
        Int8 | Int16 | Int32 | Int64 | UInt8 | UInt16 | UInt32 | UInt64 | Float32 | Float64
    )
}

/// JSON-quote and escape a string.
pub(crate) fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                use std::fmt::Write as _;
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

// ── minimal HTTP/1.1 ────────────────────────────────────────────────────────────────────

type ParsedRequest = (String, String, Vec<u8>);

/// A socket whose read deadline can be re-armed: `TcpStream` and (under `docker`) `UnixStream`, the two
/// transports this parser serves.
pub(crate) trait ReadDeadline {
    fn arm_read_deadline(&self, timeout: Option<Duration>) -> std::io::Result<()>;
}

impl ReadDeadline for TcpStream {
    fn arm_read_deadline(&self, timeout: Option<Duration>) -> std::io::Result<()> {
        self.set_read_timeout(timeout)
    }
}

#[cfg(all(feature = "docker", unix))]
impl ReadDeadline for std::os::unix::net::UnixStream {
    fn arm_read_deadline(&self, timeout: Option<Duration>) -> std::io::Result<()> {
        self.set_read_timeout(timeout)
    }
}

/// A socket that arms its own read deadline before every read, wrapped *under* the `BufReader` so it
/// sees the real reads rather than the buffered line/exact calls above it.
///
/// That placement is the whole point. Arming once per `read_line` would give each underlying read a
/// fresh allowance, and a client dribbling one byte per allowance is never idle long enough to trip it
/// — it would hold a thread forever while technically making progress. Arming per read against an
/// *absolute* deadline bounds the header phase in total instead.
///
/// The body phase is a constant per-read allowance, so it is armed once at the switch
/// ([`Armed::begin_body`]) and left alone: the steady-state cost of all this is one extra `setsockopt`
/// per request, since a `BufReader` normally swallows a whole request head in a single read.
pub(crate) struct Armed<S: ReadDeadline + Read> {
    socket: S,
    timeouts: IoTimeouts,
    /// When the header budget runs out; `None` when that phase is unbounded.
    header_deadline: Option<std::time::Instant>,
    /// Whether the body's (constant, already-armed) allowance is in effect.
    body: bool,
}

impl<S: ReadDeadline + Read> Armed<S> {
    pub(crate) fn new(socket: S, timeouts: IoTimeouts) -> Self {
        Armed {
            socket,
            header_deadline: (!timeouts.header.is_zero())
                .then(|| std::time::Instant::now() + timeouts.header),
            timeouts,
            body: false,
        }
    }

    /// Switch to the body phase: one allowance per read, armed once because it never changes.
    fn begin_body(&mut self) -> std::io::Result<()> {
        self.body = true;
        self.socket
            .arm_read_deadline(self.timeouts.socket_timeout())
    }
}

impl<S: ReadDeadline + Read> Read for Armed<S> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if !self.body
            && let Some(deadline) = self.header_deadline
        {
            match deadline.checked_duration_since(std::time::Instant::now()) {
                // Spent. (`set_read_timeout` rejects a zero duration — `None` there means *no*
                // deadline — so the exhausted case has to be an error we raise ourselves.)
                None => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "the request head did not arrive within the header timeout",
                    ));
                }
                // A sub-millisecond remainder is spent for practical purposes; ask for 1ms so the
                // syscall stays valid.
                Some(left) => self
                    .socket
                    .arm_read_deadline(Some(left.max(Duration::from_millis(1))))?,
            }
        }
        self.socket.read(buf)
    }
}

/// Parse one HTTP/1.1 request (method, path, body); `None` on a clean EOF before the request line.
/// Generic over the socket so the same parser serves the TCP server and the Unix-socket Docker plugin
/// endpoint (`docker`), which speaks the same HTTP/1.1 dialect.
///
/// The [`Armed`] reader is what enforces the [`IoTimeouts`]: the head in total, the body per read.
/// Without them a client that connects and says nothing parks this thread (and its `Db` handle) for as
/// long as it cares to.
pub(crate) fn read_request<S: ReadDeadline + Read>(
    reader: &mut BufReader<Armed<S>>,
) -> std::io::Result<Option<ParsedRequest>> {
    let mut request_line = String::new();
    if reader.read_line(&mut request_line)? == 0 {
        return Ok(None);
    }
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_owned();
    let raw_path = parts.next().unwrap_or_default();
    let path = raw_path.split('?').next().unwrap_or_default().to_owned();

    let mut content_length = 0usize;
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header)? == 0 {
            break;
        }
        let trimmed = header.trim_end();
        if trimmed.is_empty() {
            break;
        }
        if let Some((name, value)) = trimmed.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            content_length = value.trim().parse().unwrap_or(0);
        }
    }

    // The body switches to a per-read allowance: a slow upload that keeps delivering bytes is fine,
    // one that stops mid-body is not.
    reader.get_mut().begin_body()?;
    let mut body = vec![0u8; content_length];
    reader.read_exact(&mut body)?;
    Ok(Some((method, path, body)))
}

pub(crate) fn write_response<W: Write>(stream: &mut W, resp: &Response) -> std::io::Result<()> {
    let reason = match resp.status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        408 => "Request Timeout",
        500 => "Internal Server Error",
        _ => "OK",
    };
    let header = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        resp.status,
        reason,
        resp.content_type,
        resp.body.len()
    );
    stream.write_all(header.as_bytes())?;
    stream.write_all(&resp.body)?;
    stream.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn listen_address_precedence() {
        let s = |v: &str| Some(v.to_owned());

        // Positional argument wins over the environment.
        assert_eq!(
            listen_addr(s("0.0.0.0:1"), s("10.0.0.1:2"), "127.0.0.1:4318"),
            s("0.0.0.0:1")
        );
        // Environment fills in when there is no argument — the Docker plugin's path, since a managed
        // plugin cannot change its entrypoint args without a rebuild.
        assert_eq!(
            listen_addr(None, s("172.17.0.1:4318"), "127.0.0.1:4318"),
            s("172.17.0.1:4318")
        );
        // Neither → the default.
        assert_eq!(
            listen_addr(None, None, "127.0.0.1:4318"),
            s("127.0.0.1:4318")
        );
    }

    #[test]
    fn an_empty_listen_address_means_do_not_listen() {
        // `IMBH_DOCKER_PLUGIN_SOCKET=… IMBH_LISTEN_ADDR= imbhd` is the private posture: the plugin
        // serves over its Unix socket and no TCP port is opened. Whitespace counts as empty so a
        // `docker plugin set IMBH_LISTEN_ADDR=" "` does not silently bind something.
        assert_eq!(
            listen_addr(None, Some(String::new()), "127.0.0.1:4318"),
            None
        );
        assert_eq!(
            listen_addr(None, Some("  ".to_owned()), "127.0.0.1:4318"),
            None
        );
        assert_eq!(
            listen_addr(Some(String::new()), None, "127.0.0.1:4318"),
            None
        );
        // An empty *default* with nothing else set is the same statement.
        assert_eq!(listen_addr(None, None, ""), None);
    }

    #[test]
    fn flush_policy_defaults_to_a_periodic_seal() {
        // Unset and empty both mean "use the default" — a `docker plugin set IMBH_FLUSH=` must read
        // the same as never having set it.
        for env in [None, Some(String::new()), Some("  ".to_owned())] {
            let p = flush_policy(env).unwrap();
            assert_eq!(p, DEFAULT_FLUSH.parse().unwrap());
            assert_eq!(p.interval(), Some(Duration::from_secs(5)));
            assert!(!p.is_manual(), "imbhd must seal without being asked to");
        }
        // An operator's spec wins, including the one that turns automatic sealing off.
        let p = flush_policy(Some("interval=30s,wal=64MiB".to_owned())).unwrap();
        assert_eq!(p.interval(), Some(Duration::from_secs(30)));
        assert_eq!(p.wal_bytes(), Some(64 << 20));
        assert!(flush_policy(Some("manual".to_owned())).unwrap().is_manual());
        // A typo is fatal, not a silent fallback to a different cadence than was asked for.
        let err = flush_policy(Some("intrval=5s".to_owned())).unwrap_err();
        assert!(err.is_user_error(), "{err}");
    }

    #[test]
    fn maintenance_interval_defaults_and_floors() {
        assert_eq!(
            maintenance_interval(None).unwrap(),
            DEFAULT_MAINTENANCE_INTERVAL
        );
        assert_eq!(
            maintenance_interval(Some(String::new())).unwrap(),
            DEFAULT_MAINTENANCE_INTERVAL
        );
        assert_eq!(
            maintenance_interval(Some(" 5m ".to_owned())).unwrap(),
            Duration::from_secs(300)
        );
        // `0` would ask for retention on every scheduler slice; floor it at one tick instead.
        assert_eq!(
            maintenance_interval(Some("0".to_owned())).unwrap(),
            imbh::FlushPolicy::DEFAULT_TICK
        );
        assert!(maintenance_interval(Some("soon".to_owned())).is_err());
    }

    #[test]
    fn shutdown_timeout_defaults_and_accepts_zero() {
        assert_eq!(shutdown_timeout(None).unwrap(), DEFAULT_SHUTDOWN_TIMEOUT);
        assert_eq!(
            shutdown_timeout(Some(String::new())).unwrap(),
            DEFAULT_SHUTDOWN_TIMEOUT
        );
        assert_eq!(
            shutdown_timeout(Some(" 500ms ".to_owned())).unwrap(),
            Duration::from_millis(500)
        );
        // Unlike the maintenance interval, `0` is not floored: "do not wait for in-flight requests"
        // is a real answer for an operator whose supervisor is impatient.
        assert_eq!(
            shutdown_timeout(Some("0".to_owned())).unwrap(),
            Duration::ZERO
        );
        assert!(shutdown_timeout(Some("eventually".to_owned())).is_err());
    }

    #[test]
    fn io_timeouts_default_and_disable() {
        // Unset and empty both mean "use the default".
        assert_eq!(io_timeouts(None, None).unwrap(), IoTimeouts::default());
        assert_eq!(
            io_timeouts(Some(String::new()), Some("  ".to_owned())).unwrap(),
            IoTimeouts::default()
        );
        assert_eq!(IoTimeouts::default().header, DEFAULT_HEADER_TIMEOUT);
        assert_eq!(IoTimeouts::default().body, DEFAULT_BODY_TIMEOUT);

        // Each phase is set independently.
        let t = io_timeouts(Some("2s".to_owned()), Some(" 500ms ".to_owned())).unwrap();
        assert_eq!(t.header, Duration::from_secs(2));
        assert_eq!(t.body, Duration::from_millis(500));
        assert_eq!(t.socket_timeout(), Some(Duration::from_millis(500)));

        // `0` disables a phase, which is how the socket layer spells "block forever" (`None`).
        let off = io_timeouts(Some("0".to_owned()), Some("0".to_owned())).unwrap();
        assert_eq!(off, IoTimeouts::DISABLED);
        assert_eq!(off.socket_timeout(), None);

        // A typo is fatal, per phase — never a silent fallback to a deadline nobody asked for.
        assert!(io_timeouts(Some("soon".to_owned()), None).is_err());
        assert!(io_timeouts(None, Some("soon".to_owned())).is_err());
    }

    #[test]
    fn listen_address_is_trimmed() {
        // `docker plugin set` values arrive verbatim; a stray space would fail to parse as a socket
        // address and take the whole server down at bind time.
        assert_eq!(
            listen_addr(None, Some(" 172.17.0.1:4318\n".to_owned()), "x"),
            Some("172.17.0.1:4318".to_owned())
        );
    }

    fn otlp_log(service: &str, body_text: &str, time: u64) -> Vec<u8> {
        use opentelemetry_proto::tonic::collector::logs::v1::ExportLogsServiceRequest;
        use opentelemetry_proto::tonic::common::v1::{AnyValue, KeyValue, any_value};
        use opentelemetry_proto::tonic::logs::v1::{LogRecord, ResourceLogs, ScopeLogs};
        use opentelemetry_proto::tonic::resource::v1::Resource;
        use prost::Message;

        let sv = |s: &str| AnyValue {
            value: Some(any_value::Value::StringValue(s.to_owned())),
        };
        ExportLogsServiceRequest {
            resource_logs: vec![ResourceLogs {
                resource: Some(Resource {
                    attributes: vec![KeyValue {
                        key: "service.name".to_owned(),
                        value: Some(sv(service)),
                        ..Default::default()
                    }],
                    ..Default::default()
                }),
                scope_logs: vec![ScopeLogs {
                    log_records: vec![LogRecord {
                        time_unix_nano: time,
                        severity_number: 9,
                        body: Some(sv(body_text)),
                        ..Default::default()
                    }],
                    ..Default::default()
                }],
                ..Default::default()
            }],
        }
        .encode_to_vec()
    }

    /// A one-point OTLP explicit-bucket histogram (for exercising `List` columns over HTTP).
    fn otlp_hist(metric: &str, bounds: &[f64], counts: &[u64]) -> Vec<u8> {
        use opentelemetry_proto::tonic::collector::metrics::v1::ExportMetricsServiceRequest;
        use opentelemetry_proto::tonic::metrics::v1::{
            Histogram, HistogramDataPoint, Metric, ResourceMetrics, ScopeMetrics, metric,
        };
        use prost::Message;

        ExportMetricsServiceRequest {
            resource_metrics: vec![ResourceMetrics {
                scope_metrics: vec![ScopeMetrics {
                    metrics: vec![Metric {
                        name: metric.to_owned(),
                        data: Some(metric::Data::Histogram(Histogram {
                            data_points: vec![HistogramDataPoint {
                                time_unix_nano: 1,
                                count: counts.iter().sum(),
                                explicit_bounds: bounds.to_vec(),
                                bucket_counts: counts.to_vec(),
                                ..Default::default()
                            }],
                            aggregation_temporality: 2,
                        })),
                        ..Default::default()
                    }],
                    ..Default::default()
                }],
                ..Default::default()
            }],
        }
        .encode_to_vec()
    }

    #[tokio::test(flavor = "current_thread")]
    async fn query_list_column_renders() {
        let db = Db::in_memory().open().unwrap();
        assert_eq!(
            route(
                &db,
                "POST",
                "/v1/metrics",
                &otlp_hist("lat", &[1.0, 5.0], &[2, 3, 2])
            )
            .await
            .status,
            200
        );
        // A List column (`bucket_counts`) must serialize to JSON without panicking the connection.
        let q = route(
            &db,
            "POST",
            "/api/query",
            b"SELECT metric, bucket_counts FROM metrics_histogram",
        )
        .await;
        assert_eq!(q.status, 200);
        let json = String::from_utf8(q.body).unwrap();
        assert!(json.contains("\"metric\":\"lat\""), "got {json}");
        assert!(json.contains("bucket_counts"), "got {json}");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn health_ingest_query() {
        let db = Db::in_memory().open().unwrap();

        assert_eq!(route(&db, "GET", "/health", b"").await.status, 200);
        assert_eq!(route(&db, "GET", "/nope", b"").await.status, 404);

        // OTLP/HTTP logs ingest.
        let r = route(&db, "POST", "/v1/logs", &otlp_log("cart", "hello", 1)).await;
        assert_eq!(r.status, 200);
        assert!(
            String::from_utf8(r.body)
                .unwrap()
                .contains("\"accepted\":1")
        );

        // SQL query → JSON rows.
        let q = route(
            &db,
            "POST",
            "/api/query",
            b"SELECT service, count(*) AS c FROM logs GROUP BY service",
        )
        .await;
        assert_eq!(q.status, 200);
        let json = String::from_utf8(q.body).unwrap();
        assert!(json.contains("\"service\":\"cart\""), "got {json}");
        assert!(json.contains("\"c\":1"), "got {json}");

        // A bad query → 400.
        let bad = route(&db, "POST", "/api/query", b"SELECT nope FROM missing").await;
        assert_eq!(bad.status, 400);

        // GET /stats → operational JSON with the engine gauges and a logs table entry.
        let s = route(&db, "GET", "/stats", b"").await;
        assert_eq!(s.status, 200);
        let stats = String::from_utf8(s.body).unwrap();
        assert!(stats.contains("\"buffer_bytes\":"), "got {stats}");
        assert!(stats.contains("\"wal_bytes\":"), "got {stats}");
        assert!(stats.contains("\"durable_lsn\":"), "got {stats}");
        assert!(stats.contains("\"table\":\"logs\""), "got {stats}");
        assert!(stats.contains("\"buffer_rows\":1"), "got {stats}");

        // Admin maintenance actions return JSON results (no-op on this in-memory DB).
        let f = route(&db, "POST", "/admin/flush", b"").await;
        assert_eq!(f.status, 200);
        assert!(
            String::from_utf8(f.body)
                .unwrap()
                .contains("\"flushed\":true")
        );
        let c = route(&db, "POST", "/admin/compact", b"").await;
        assert_eq!(c.status, 200);
        assert!(
            String::from_utf8(c.body)
                .unwrap()
                .contains("\"segments_merged\":0")
        );
    }
}

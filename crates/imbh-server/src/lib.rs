//! imbhd — the reference HTTP server (ARCHITECTURE.md §10.16).
//!
//! An HTTP/1.1 server on **axum over hyper**, showing one way a host can expose the imbh library over
//! HTTP. It is **reference wiring, not the product** (§10.1) — the library imposes no server
//! framework, and this crate's choices bind nobody. Footprint-wise it is free where it counts: the
//! crate-count gate measures `cargo tree -p imbh` (`scripts/footprint-gate.sh`) and the dependency
//! direction is `imbh ← imbh-server`, so nothing here reaches the number the budget is written
//! against. Under `--features grpc` the whole subtree is already present anyway, since tonic routes
//! through axum.
//!
//! ## The runtime model
//!
//! One shared multi-threaded runtime drives the accept loop and every connection. That matters
//! because `Db`'s futures do **blocking** parquet/tantivy I/O inside themselves — the library has no
//! `spawn_blocking` anywhere — so awaiting one on a runtime worker would park that worker and starve
//! every other connection, `/health` included. Every `Db` call therefore goes through [`offload`],
//! which runs it under `tokio::task::block_in_place` so tokio replaces the worker for the duration.
//! Request concurrency is then bounded by the blocking pool (i.e. by *work*) rather than by socket
//! count, which is the right axis: connections are cheap now, and the old design's thread per
//! connection was not.
//!
//! Routes:
//! - `POST /v1/logs` · `/v1/traces` · `/v1/metrics` — OTLP/HTTP protobuf ingest, `Content-Encoding:
//!   gzip` accepted (the OTel Collector's `otlphttp` exporter compresses by default).
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
//! Requests are bounded on three axes, all tunable and all defaulted (see [`Limits`]): the head by
//! [`IoTimeouts::header`], each body read by [`IoTimeouts::body`], the body's size by
//! [`Limits::max_body`], and simultaneous connections by [`Limits::max_connections`].
//!
//! OTLP/gRPC is available on a second port behind the optional `grpc` feature (see [`grpc`]); the
//! default build carries no gRPC transport. A Docker logging-driver plugin endpoint is available
//! behind the optional `docker` feature (see [`docker`]); it speaks a different protocol on a Unix
//! socket but runs on this same stack, sharing [`handle`] and so the same body limits, deadlines,
//! and decoding. Not handled here (follow-ups): TLS and the OTLP partial-success response shape.
//!
//! Every endpoint stops on a shared [`Shutdown`] token: `SIGINT`/`SIGTERM` stop the accept loops,
//! in-flight requests get a bounded drain, and `imbhd` seals the buffer with `Db::close()` before it
//! exits. See the [`shutdown`] module for the mechanism and [`serve_until`] for the HTTP side.

#[cfg(all(feature = "docker", unix))]
pub mod docker;
#[cfg(feature = "grpc")]
pub mod grpc;
pub mod shutdown;

use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, PoisonError};
use std::time::Duration;

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{Request, StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use http_body_util::BodyExt;
use hyper_util::rt::{TokioIo, TokioTimer};
use hyper_util::server::graceful::GracefulShutdown;
use tower::ServiceExt;

pub use shutdown::{DEFAULT_DRAIN_TIMEOUT, Shutdown};

use imbh::arrow::array::Array;
use imbh::arrow::record_batch::RecordBatch;
use imbh::arrow::util::display::{ArrayFormatter, FormatOptions};
use imbh::{Db, FlushPolicy};

/// A minimal HTTP response — the shape every handler in this crate returns, converted into an axum
/// response on the way out (and written directly by the Docker plugin endpoint, which does not go
/// through hyper).
pub struct Response {
    pub status: u16,
    pub content_type: String,
    pub body: Vec<u8>,
}

impl IntoResponse for Response {
    fn into_response(self) -> axum::response::Response {
        // An unmappable code would be a bug here (every construction site uses a real status), but a
        // 500 is a better answer than a panic on a connection thread.
        let status = StatusCode::from_u16(self.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        (
            status,
            [(header::CONTENT_TYPE, self.content_type)],
            self.body,
        )
            .into_response()
    }
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

/// Per-connection I/O deadlines, so a client cannot hold a connection open without making progress.
///
/// The two phases want different rules, which is why this is two values rather than one socket
/// timeout:
///
/// - [`IoTimeouts::header`] bounds the request line + headers **in total** — hyper's
///   `header_read_timeout`. A per-read allowance is not enough here: a client trickling one byte per
///   allowance is never idle, yet never finishes either. It is armed for every head on a connection,
///   so it also bounds an idle keep-alive connection between requests.
/// - [`IoTimeouts::body`] is a **per-read** allowance for the body. A 50 MiB OTLP body over a slow
///   link must not be punished for its size — only for stalling — and a total deadline cannot tell
///   those apart.
///
/// `Duration::ZERO` in either field means *no deadline* for that phase ([`IoTimeouts::DISABLED`] is
/// both), which is the right choice for a host fronting `imbhd` with a proxy that already sheds slow
/// clients.
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

    /// The head deadline as hyper wants it: `None` disables `header_read_timeout` entirely.
    fn header_deadline(self) -> Option<Duration> {
        (!self.header.is_zero()).then_some(self.header)
    }

    /// The per-read body allowance — `None` when the phase is unbounded.
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

/// `imbhd`'s default cap on one request body: large enough for a fat OTLP batch, small enough that a
/// forged `Content-Length` asks for a refusal rather than for the machine's memory.
pub const DEFAULT_MAX_BODY: u64 = 64 * 1024 * 1024;

/// `imbhd`'s default cap on simultaneous connections. Connections are cheap now (a task, not a
/// thread), so this is a guard against file-descriptor exhaustion rather than a throughput knob —
/// what bounds actual work is the blocking pool every `Db` call goes through (see [`offload`]).
///
/// Deliberately under the usual 1024 soft `RLIMIT_NOFILE`, because connections are not the only
/// descriptors this process wants: parquet segments and tantivy's mmaps need their share too, and a
/// connection that has not sent a request head yet is briefly holding two (see [`serve_async`]).
pub const DEFAULT_MAX_CONNECTIONS: usize = 512;

/// Resolve the request-body cap from `IMBH_MAX_BODY` (a byte size such as `64MiB`), falling back to
/// [`DEFAULT_MAX_BODY`]. Empty means unset; `0` means no cap; a malformed value is an error.
pub fn max_body(env: Option<String>) -> imbh::Result<u64> {
    let value = env.unwrap_or_default();
    let value = value.trim();
    if value.is_empty() {
        return Ok(DEFAULT_MAX_BODY);
    }
    imbh::parse_bytes(value)
}

/// Resolve the connection cap from `IMBH_MAX_CONNECTIONS`, falling back to
/// [`DEFAULT_MAX_CONNECTIONS`]. Empty means unset; `0` means no cap; a malformed value is an error.
pub fn max_connections(env: Option<String>) -> imbh::Result<usize> {
    let value = env.unwrap_or_default();
    let value = value.trim();
    if value.is_empty() {
        return Ok(DEFAULT_MAX_CONNECTIONS);
    }
    value
        .parse()
        .map_err(|_| imbh::Error::config_msg(format!("not a connection count: {value}")))
}

/// Everything that bounds one connection: the phase deadlines, the body cap, and how many
/// connections may be open at once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Per-phase I/O deadlines.
    pub timeouts: IoTimeouts,
    /// Largest request body accepted, in bytes, measured *after* any `Content-Encoding` is undone.
    /// `0` means no cap.
    pub max_body: u64,
    /// Most connections open at once; further ones wait for a slot rather than being accepted.
    /// `0` means no cap.
    pub max_connections: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            timeouts: IoTimeouts::default(),
            max_body: DEFAULT_MAX_BODY,
            max_connections: DEFAULT_MAX_CONNECTIONS,
        }
    }
}

impl Limits {
    /// Whether `len` bytes is over the body cap (never, when the cap is off).
    fn exceeds_body(self, len: u64) -> bool {
        self.max_body != 0 && len > self.max_body
    }
}

/// Serve `db` on `addr` (e.g. `127.0.0.1:4318`) until the process exits.
///
/// Never returns on its own; a host that wants to stop serving wants [`serve_until`].
pub fn serve(db: Arc<Db>, addr: &str) -> std::io::Result<()> {
    serve_until(db, addr, Shutdown::new())
}

/// Serve `db` on `addr` until `shutdown` trips, then stop accepting, give the in-flight requests up
/// to [`Shutdown::drain_timeout`] to finish, and return.
///
/// A request still in flight when the drain expires is abandoned, not waited for: the reply is lost
/// but the ingest it asked for is already durable or not at all (`Db::ingest_otlp_*` decides that,
/// not this loop), so an OTLP client's retry is the correct resolution either way.
///
/// Connections get the default [`Limits`]; [`serve_with_until`] and [`serve_with_limits_until`] take
/// them as an argument.
pub fn serve_until(db: Arc<Db>, addr: &str, shutdown: Arc<Shutdown>) -> std::io::Result<()> {
    serve_with_limits_until(db, addr, Limits::default(), shutdown)
}

/// [`serve_until`] with the per-connection deadlines set explicitly, leaving the size and count caps
/// at their defaults.
pub fn serve_with_until(
    db: Arc<Db>,
    addr: &str,
    timeouts: IoTimeouts,
    shutdown: Arc<Shutdown>,
) -> std::io::Result<()> {
    serve_with_limits_until(
        db,
        addr,
        Limits {
            timeouts,
            ..Limits::default()
        },
        shutdown,
    )
}

/// [`serve_until`] with every bound set explicitly — what `imbhd` calls, so `IMBH_HEADER_TIMEOUT`,
/// `IMBH_BODY_TIMEOUT`, `IMBH_MAX_BODY`, and `IMBH_MAX_CONNECTIONS` all reach the connections.
///
/// Blocking: it owns the runtime for its whole life, which is what lets `imbhd`'s `main` run each
/// listener on a plain thread. A host that already has a runtime should mount [`app`] in its own
/// server instead of calling this.
pub fn serve_with_limits_until(
    db: Arc<Db>,
    addr: &str,
    limits: Limits,
    shutdown: Arc<Shutdown>,
) -> std::io::Result<()> {
    // Multi-threaded on purpose: `offload` needs `block_in_place`, which a current-thread runtime does
    // not have, and one blocking `Db` call would otherwise stop the listener answering anything.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(serve_async(db, addr, limits, shutdown))
}

/// The accept loop. Binds first, so a bind failure still reaches the caller.
async fn serve_async(
    db: Arc<Db>,
    addr: &str,
    limits: Limits,
    shutdown: Arc<Shutdown>,
) -> std::io::Result<()> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let app = app(db);

    // The token is a sync primitive (a condvar, tripped from the signal watcher thread), so bridge it
    // into a future once rather than polling it. `on_trigger` runs the closure immediately if the
    // token is already tripped, which covers binding after the signal arrived.
    let (stop_tx, mut stop_rx) = tokio::sync::oneshot::channel::<()>();
    shutdown.on_trigger(move || {
        let _ = stop_tx.send(());
    });

    let graceful = GracefulShutdown::new();
    let connections = Arc::new(tokio::sync::Semaphore::new(match limits.max_connections {
        0 => tokio::sync::Semaphore::MAX_PERMITS,
        n => n,
    }));

    let mut builder = hyper::server::conn::http1::Builder::new();
    // `header_read_timeout` is a no-op without a timer, so this line is what makes `IoTimeouts::header`
    // real. It is armed for every head on a connection, so it bounds idle keep-alive periods too.
    builder.timer(TokioTimer::new());
    builder.header_read_timeout(limits.timeouts.header_deadline());

    loop {
        // The permit is taken *before* the accept, so the cap bounds connections the kernel has handed
        // us rather than letting them pile up inside the process.
        let permit = tokio::select! {
            permit = connections.clone().acquire_owned() => match permit {
                Ok(permit) => permit,
                // The semaphore is never closed; bail rather than spin if that ever changes.
                Err(_) => break,
            },
            _ = &mut stop_rx => break,
        };
        let stream = tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok((stream, _peer)) => stream,
                // Per-connection errors (`ECONNABORTED`, `EMFILE`); the listener itself is still good.
                Err(_) => continue,
            },
            _ = &mut stop_rx => break,
        };

        // A second descriptor on the same socket, held aside so a header-phase timeout can still say
        // 408. hyper reports that deadline but answers nothing (`role::on_error` maps parse errors to a
        // status and header timeouts to `None`), and by then it has dropped its own handle — this
        // duplicate is what keeps the socket alive long enough to explain itself.
        //
        // Only worth taking while a 408 is still possible: none if the header phase is unbounded, and
        // released the moment a request head arrives (below), so the doubled descriptor lasts only as
        // long as a connection that has not asked for anything yet. Otherwise the connection cap would
        // quietly be a cap on *half* as many descriptors as it says.
        let (stream, late) = match stream.into_std() {
            Ok(raw) => {
                let late = limits.timeouts.header_deadline().and_then(|_| {
                    raw.try_clone()
                        .and_then(tokio::net::TcpStream::from_std)
                        .ok()
                });
                match tokio::net::TcpStream::from_std(raw) {
                    Ok(stream) => (stream, late),
                    Err(_) => continue,
                }
            }
            Err(_) => continue,
        };
        let late = Arc::new(std::sync::Mutex::new(late));

        // Set once a request head has been parsed, so the 408 above is only ever sent to a client that
        // never asked for anything — not appended after a keep-alive connection's last response.
        let served = Arc::new(AtomicBool::new(false));
        let service = hyper::service::service_fn({
            let app = app.clone();
            let served = Arc::clone(&served);
            let late = Arc::clone(&late);
            move |request: Request<hyper::body::Incoming>| {
                served.store(true, Ordering::SeqCst);
                // This connection has asked for something, so the spare descriptor has no 408 left to
                // send: give it back now rather than at the end of the connection.
                drop(late.lock().unwrap_or_else(PoisonError::into_inner).take());
                let app = app.clone();
                async move { Ok::<_, std::convert::Infallible>(handle(app, request, limits).await) }
            }
        });
        let connection = graceful.watch(builder.serve_connection(TokioIo::new(stream), service));
        tokio::spawn(async move {
            let _permit = permit;
            let outcome = connection.await;
            // The head deadline ran out before a request arrived. hyper reports that but does not
            // answer it, so the spare descriptor does.
            let unanswered =
                matches!(&outcome, Err(e) if e.is_timeout()) && !served.load(Ordering::SeqCst);
            // Taken out of the lock in its own statement: a `std::sync` guard must not live across
            // the await below, and a let-chain would keep it alive for the whole `if` body.
            let late = match unanswered {
                true => late.lock().unwrap_or_else(PoisonError::into_inner).take(),
                false => None,
            };
            if let Some(mut late) = late {
                use tokio::io::AsyncWriteExt as _;
                let _ = tokio::time::timeout(LATE_REPLY, late.write_all(HTTP_408)).await;
            }
        });
    }

    // Signals every live connection to finish what it is doing and close. An idle keep-alive
    // connection goes at once; one mid-request gets to finish it.
    if tokio::time::timeout(shutdown.drain_timeout(), graceful.shutdown())
        .await
        .is_err()
    {
        warn(&format!(
            "in-flight HTTP connection(s) abandoned after the {:?} shutdown drain",
            shutdown.drain_timeout()
        ));
    }
    Ok(())
}

/// The 408 written to a client that opened a connection and never sent a request head. Pre-rendered
/// because it goes out on a raw socket, after hyper has given up on the connection.
const HTTP_408: &[u8] = b"HTTP/1.1 408 Request Timeout\r\nContent-Type: text/plain\r\n\
    Content-Length: 17\r\nConnection: close\r\n\r\nrequest timed out";

/// How long that last-gasp 408 may take to go out. It is 17 bytes into an empty socket buffer, so
/// this only ever expires when the peer is already gone.
const LATE_REPLY: Duration = Duration::from_secs(1);

/// Report a server-level problem. Routed through `tracing` when `imbhd` is built with that feature,
/// so it joins the rest of the server's instrumentation; plain stderr otherwise.
pub(crate) fn warn(message: &str) {
    #[cfg(feature = "tracing")]
    tracing::warn!(target: "imbh_server", "{message}");
    #[cfg(not(feature = "tracing"))]
    eprintln!("imbhd: {message}");
}

/// One request: read and decode the body under [`Limits`], then let the router dispatch it.
///
/// The body is buffered here rather than in an extractor so that "too big", "stalled", and "not
/// actually gzip" each get the status they deserve (413 / 408 / 400) instead of the one blanket
/// rejection an extractor would produce. By the time the router runs, the body is a plain `Bytes`.
#[cfg_attr(
    feature = "tracing",
    tracing::instrument(
        level = "info",
        name = "request",
        skip_all,
        fields(
            method = %request.method(),
            path = %request.uri().path(),
            status = tracing::field::Empty,
        )
    )
)]
pub(crate) async fn handle(
    app: Router,
    request: Request<hyper::body::Incoming>,
    limits: Limits,
) -> axum::response::Response {
    let (parts, body) = request.into_parts();
    let response = match read_body(&parts, body, limits).await {
        Ok(body) => app
            .oneshot(Request::from_parts(parts, Body::from(body)))
            // The router's error type is `Infallible`, so this arm is uninhabited.
            .await
            .unwrap_or_else(|e: std::convert::Infallible| match e {}),
        Err(response) => response.into_response(),
    };
    #[cfg(feature = "tracing")]
    tracing::Span::current().record("status", response.status().as_u16());
    response
}

/// Buffer a request body, enforcing the size cap, the per-read deadline, and `Content-Encoding`.
///
/// Chunked bodies need no special handling — hyper has already undone the framing, which is the bug
/// the hand-rolled parser had: it keyed entirely off `Content-Length` and read a chunked upload as
/// zero bytes, then answered `200 {"accepted":0}`.
async fn read_body(
    parts: &axum::http::request::Parts,
    body: hyper::body::Incoming,
    limits: Limits,
) -> Result<Vec<u8>, Response> {
    // A declared length over the cap is refused before a byte is read: allocating for it up front is
    // precisely what a forged `Content-Length` is asking for.
    if let Some(declared) = parts
        .headers
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        && limits.exceeds_body(declared)
    {
        return Err(too_large(limits.max_body));
    }

    let allowance = limits.timeouts.socket_timeout();
    let mut body = body;
    let mut buffered: Vec<u8> = Vec::new();
    loop {
        // Per frame, not per request: an upload that keeps delivering is never cut off for being
        // large, only for going quiet.
        let frame = match allowance {
            Some(allowance) => match tokio::time::timeout(allowance, body.frame()).await {
                Ok(frame) => frame,
                Err(_) => return Err(Response::text(408, "request timed out")),
            },
            None => body.frame().await,
        };
        let Some(frame) = frame else { break };
        let frame = frame.map_err(|_| Response::text(400, "malformed request body"))?;
        // Trailers carry no body bytes; skip them rather than treating them as data.
        if let Ok(data) = frame.into_data() {
            if limits.exceeds_body((buffered.len() + data.len()) as u64) {
                return Err(too_large(limits.max_body));
            }
            buffered.extend_from_slice(&data);
        }
    }

    if !is_gzip(parts) {
        return Ok(buffered);
    }
    let max_body = limits.max_body;
    offload_blocking(move || gunzip(&buffered, max_body)).await
}

/// Whether the request declares a gzip body. The OTel Collector's `otlphttp` exporter sets this by
/// default, so a stock collector in front of `imbhd` depends on it.
fn is_gzip(parts: &axum::http::request::Parts) -> bool {
    parts
        .headers
        .get(header::CONTENT_ENCODING)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("gzip"))
}

/// Inflate a gzip body, refusing one that expands past the cap.
///
/// The cap is enforced by reading one byte *past* it and treating that byte's existence as the
/// overflow — a compression bomb is a small upload, so the declared length cannot catch it and only
/// bounding the output can.
fn gunzip(body: &[u8], max_body: u64) -> Result<Vec<u8>, Response> {
    use std::io::Read as _;

    let ceiling = match max_body {
        0 => u64::MAX,
        max => max.saturating_add(1),
    };
    let mut inflated = Vec::new();
    flate2::read::GzDecoder::new(body)
        .take(ceiling)
        .read_to_end(&mut inflated)
        .map_err(|_| Response::text(400, "malformed gzip request body"))?;
    if max_body != 0 && inflated.len() as u64 > max_body {
        return Err(too_large(max_body));
    }
    Ok(inflated)
}

/// The 413 for a body over [`Limits::max_body`], in the same `{"error": ...}` shape as every other
/// failure so a client has one thing to parse.
fn too_large(max_body: u64) -> Response {
    Response::json(
        413,
        format!(
            "{{\"error\":{}}}",
            json_string(&format!("request body exceeds the {max_body}-byte limit"))
        )
        .into_bytes(),
    )
}

/// The route table, over a shared `Db`.
///
/// Public because it is the useful half of this crate for a host that already runs axum: mount it
/// (or a `Router::nest` of it) in an existing application and imbh's endpoints come along, without
/// [`serve`]'s opinions about runtimes, ports, or shutdown.
pub fn app(db: Arc<Db>) -> Router {
    Router::new()
        .route("/", get(health))
        .route("/health", get(health))
        .route("/v1/logs", post(ingest_logs))
        .route("/v1/traces", post(ingest_traces))
        .route("/v1/metrics", post(ingest_metrics))
        .route("/api/query", post(query))
        .route("/stats", get(stats))
        .route("/admin/flush", post(admin_flush))
        .route("/admin/compact", post(admin_compact))
        .fallback(not_found)
        .with_state(db)
}

async fn health() -> Response {
    Response::text(200, "ok")
}

async fn not_found() -> Response {
    Response::text(404, "not found")
}

async fn ingest_logs(State(db): State<Arc<Db>>, body: Bytes) -> Response {
    ingest_response(offload(db.ingest_otlp_logs(&body)).await)
}

async fn ingest_traces(State(db): State<Arc<Db>>, body: Bytes) -> Response {
    ingest_response(offload(db.ingest_otlp_traces(&body)).await)
}

async fn ingest_metrics(State(db): State<Arc<Db>>, body: Bytes) -> Response {
    ingest_response(offload(db.ingest_otlp_metrics(&body)).await)
}

async fn query(State(db): State<Arc<Db>>, body: Bytes) -> Response {
    query_response(&db, &body).await
}

async fn stats(State(db): State<Arc<Db>>) -> Response {
    stats_response(&db).await
}

async fn admin_flush(State(db): State<Arc<Db>>) -> Response {
    match offload(db.flush()).await {
        Ok(()) => Response::json(200, b"{\"flushed\":true}".to_vec()),
        Err(e) => error_response(&e),
    }
}

async fn admin_compact(State(db): State<Arc<Db>>) -> Response {
    match offload(db.compact()).await {
        Ok(r) => Response::json(
            200,
            format!(
                "{{\"segments_merged\":{},\"segments_created\":{}}}",
                r.segments_merged, r.segments_created
            )
            .into_bytes(),
        ),
        Err(e) => error_response(&e),
    }
}

/// Dispatch one request through the same route table [`app`] builds, without a socket. Exposed for
/// testing, and for a host that owns its own transport and just wants the handlers.
pub async fn route(db: &Arc<Db>, method: &str, path: &str, body: &[u8]) -> Response {
    let request = match Request::builder()
        .method(method)
        .uri(path)
        .body(Body::from(body.to_vec()))
    {
        Ok(request) => request,
        // A method or URI that is not well-formed at all; hyper would have rejected it before the
        // router ever saw it, so this only reaches a caller building requests by hand.
        Err(_) => return Response::text(400, "malformed request"),
    };
    let response = app(db.clone())
        .oneshot(request)
        .await
        .unwrap_or_else(|e: std::convert::Infallible| match e {});

    let status = response.status().as_u16();
    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("text/plain")
        .to_owned();
    // Handler bodies are all in memory already, so there is nothing to bound here.
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap_or_default()
        .to_vec();
    Response {
        status,
        content_type,
        body,
    }
}

/// Whether `block_in_place` is available — it is not on a current-thread runtime, where it panics.
/// That is what `#[tokio::test]` gives by default, and what the `Db` facade's own blocking mirror
/// builds.
fn can_block_in_place() -> bool {
    matches!(
        tokio::runtime::Handle::try_current().map(|handle| handle.runtime_flavor()),
        Ok(tokio::runtime::RuntimeFlavor::MultiThread)
    )
}

/// Run a `Db` future to completion without leaving a runtime worker parked on its blocking I/O.
///
/// `Db`'s futures block *inside themselves* — parquet and tantivy reads are synchronous and the
/// library has no `spawn_blocking` anywhere — so awaiting one directly on a worker stops that worker
/// serving anything else, `/health` included. `block_in_place` hands this worker's queue to a
/// replacement for the duration, which is the contract we want and costs no `Send + 'static` bound:
/// the future goes on borrowing the `Db` and the request body.
///
/// On a current-thread runtime it simply awaits. Nothing else shares that thread to starve, which is
/// the situation in `#[tokio::test]` and in an embedder driving [`route`] by hand.
pub async fn offload<F: Future>(fut: F) -> F::Output {
    if can_block_in_place() {
        tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(fut))
    } else {
        fut.await
    }
}

/// [`offload`] for synchronous CPU work — gzip inflation, and the Docker plugin's endpoints, which
/// do blocking filesystem work (`StartLogging` waits up to `OPEN_TIMEOUT` on a FIFO open).
pub(crate) async fn offload_blocking<T>(work: impl FnOnce() -> T) -> T {
    if can_block_in_place() {
        tokio::task::block_in_place(work)
    } else {
        work()
    }
}

/// `GET /stats` — the DB's operational stats as JSON (VM `/status/tsdb` analogue): per-table
/// segment/row/buffer counts and time span, plus buffer bytes, WAL bytes, and the durable LSN.
async fn stats_response(db: &Arc<Db>) -> Response {
    let stats = match offload(db.stats()).await {
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
    // The heaviest offload in the crate: a scan is blocking parquet and tantivy I/O from start to
    // finish, so this is the call that would park a worker for whole seconds.
    match offload(db.sql(sql).collect()).await {
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

    #[tokio::test(flavor = "current_thread")]
    async fn a_known_path_with_the_wrong_method_is_405() {
        // A behaviour change from the hand-rolled dispatcher, which matched on the `(method, path)`
        // pair and so answered 404 for everything it did not recognise. The router knows a path it
        // serves from one it does not, and says which of the two is wrong.
        let db = Db::in_memory().open().unwrap();
        assert_eq!(route(&db, "GET", "/v1/logs", b"").await.status, 405);
        assert_eq!(route(&db, "POST", "/health", b"").await.status, 405);
        // An unknown path is still a 404, wrong method or not.
        assert_eq!(route(&db, "DELETE", "/nope", b"").await.status, 404);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_query_string_is_not_part_of_the_route() {
        let db = Db::in_memory().open().unwrap();
        let stats = route(&db, "GET", "/stats?pretty=1", b"").await;
        assert_eq!(stats.status, 200);
        assert_eq!(stats.content_type, "application/json");
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

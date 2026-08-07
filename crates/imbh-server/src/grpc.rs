//! OTLP/gRPC ingest for the reference server (optional `grpc` feature, ARCHITECTURE.md §10.16).
//!
//! The std-net HTTP/1.1 server in `lib.rs` speaks OTLP/HTTP; this module adds the OTLP/gRPC twin so a
//! stock OTel SDK's gRPC exporter (the collector default) can push to `imbhd` too. gRPC is HTTP/2 +
//! protobuf framing, which the hand-rolled HTTP/1.1 server can't do, so here we lean on **tonic** —
//! pulled in only by the off-by-default `grpc` feature, keeping the default footprint gate unchanged.
//!
//! The three OTLP collector services (`LogsService` / `TraceService` / `MetricsService`) all funnel
//! into one [`OtlpGrpc`] value sharing the `Arc<Db>`. Each `export` re-encodes the decoded request
//! back to protobuf bytes and hands them to the same `Db::ingest_otlp_*` entry points the HTTP routes
//! use — one ingest path, one validation story. Errors map to gRPC status codes via the §10.3
//! classifiers, mirroring `error_response`'s HTTP mapping.

use std::net::SocketAddr;
use std::sync::Arc;

use imbh::Db;
use prost::Message;
use tonic::{Request, Response, Status};

use opentelemetry_proto::tonic::collector::logs::v1::{
    ExportLogsServiceRequest, ExportLogsServiceResponse,
    logs_service_server::{LogsService, LogsServiceServer},
};
use opentelemetry_proto::tonic::collector::metrics::v1::{
    ExportMetricsPartialSuccess, ExportMetricsServiceRequest, ExportMetricsServiceResponse,
    metrics_service_server::{MetricsService, MetricsServiceServer},
};
use opentelemetry_proto::tonic::collector::trace::v1::{
    ExportTraceServiceRequest, ExportTraceServiceResponse,
    trace_service_server::{TraceService, TraceServiceServer},
};

/// The OTLP/gRPC collector: one handler backing all three signal services over a shared `Db`.
struct OtlpGrpc {
    db: Arc<Db>,
}

/// Map an imbh error onto a gRPC status using the §10.3 classifiers, matching the HTTP
/// `error_response` mapping: not-found → `NotFound`, user error → `InvalidArgument`, else `Internal`.
fn to_status(e: &imbh::Error) -> Status {
    let msg = e.to_string();
    if e.is_not_found() {
        Status::not_found(msg)
    } else if e.is_user_error() {
        Status::invalid_argument(msg)
    } else {
        Status::internal(msg)
    }
}

#[tonic::async_trait]
impl LogsService for OtlpGrpc {
    async fn export(
        &self,
        request: Request<ExportLogsServiceRequest>,
    ) -> Result<Response<ExportLogsServiceResponse>, Status> {
        let bytes = request.into_inner().encode_to_vec();
        self.db
            .ingest_otlp_logs(&bytes)
            .await
            .map_err(|e| to_status(&e))?;
        // Full success: an empty `partial_success` per the OTLP spec (no rejected points to report).
        Ok(Response::new(ExportLogsServiceResponse::default()))
    }
}

#[tonic::async_trait]
impl TraceService for OtlpGrpc {
    async fn export(
        &self,
        request: Request<ExportTraceServiceRequest>,
    ) -> Result<Response<ExportTraceServiceResponse>, Status> {
        let bytes = request.into_inner().encode_to_vec();
        self.db
            .ingest_otlp_traces(&bytes)
            .await
            .map_err(|e| to_status(&e))?;
        Ok(Response::new(ExportTraceServiceResponse::default()))
    }
}

#[tonic::async_trait]
impl MetricsService for OtlpGrpc {
    async fn export(
        &self,
        request: Request<ExportMetricsServiceRequest>,
    ) -> Result<Response<ExportMetricsServiceResponse>, Status> {
        let bytes = request.into_inner().encode_to_vec();
        let receipt = self
            .db
            .ingest_otlp_metrics(&bytes)
            .await
            .map_err(|e| to_status(&e))?;
        // Only metrics have a rejection policy (`Duplicates::Reject`, issue #27), and the OTLP spec
        // wants `partial_success` left unset on full success — a message on every export is noise a
        // compliant client is entitled to log.
        let partial_success = (receipt.rejected > 0).then(|| ExportMetricsPartialSuccess {
            rejected_data_points: receipt.rejected as i64,
            error_message:
                "duplicate (series, timestamp) rejected by the database's duplicate policy"
                    .to_owned(),
        });
        Ok(Response::new(ExportMetricsServiceResponse {
            partial_success,
        }))
    }
}

/// Serve OTLP/gRPC on `addr` until the future is dropped or the server errors. `async`: the caller
/// supplies the runtime (the tests drive it directly; `serve_grpc_blocking` wraps it for the binary).
///
/// All three collector services share one `Arc<Db>` via `from_arc`, so ingest fans into the same
/// buffer/WAL the HTTP routes write to.
pub async fn serve_grpc(db: Arc<Db>, addr: SocketAddr) -> Result<(), tonic::transport::Error> {
    server(db).serve(addr).await
}

/// [`serve_grpc`], stopping when `shutdown` trips.
///
/// tonic's own graceful path (`serve_with_shutdown`): the listener closes as soon as the signal
/// future resolves, and in-flight `export` calls run to completion — so a batch that was already
/// decoding still lands in the WAL before this returns. That future is where the token is observed,
/// and it is the one place `imbhd` polls: HTTP/2 keeps connections alive across requests, so a tick
/// of shutdown latency here costs no per-request latency, unlike the HTTP/1.1 accept loop.
pub async fn serve_grpc_until(
    db: Arc<Db>,
    addr: SocketAddr,
    shutdown: Arc<crate::Shutdown>,
) -> Result<(), tonic::transport::Error> {
    server(db)
        .serve_with_shutdown(addr, async move {
            while !shutdown.is_triggered() {
                tokio::time::sleep(SHUTDOWN_POLL).await;
            }
        })
        .await
}

/// [`serve_grpc_until`] on a listener somebody else bound, optionally refusing peers.
///
/// The Docker plugin's multi-address supervisor (`docker::serve`) owns the socket so that one
/// definition of "which bind failures are fatal" covers both protocols; tonic is happy to take the
/// listener as an incoming stream rather than binding its own.
///
/// `allow` is applied to that stream, which puts a refused peer exactly where the HTTP accept loop
/// puts it — closed before a byte is read, with nothing sent back that would confirm something is
/// listening.
#[cfg(all(feature = "docker", unix))]
pub(crate) async fn serve_grpc_on_listener(
    db: Arc<Db>,
    listener: tokio::net::TcpListener,
    allow: Option<crate::PeerFilter>,
    shutdown: Arc<crate::Shutdown>,
) -> Result<(), tonic::transport::Error> {
    let incoming = Allowed {
        inner: tokio_stream::wrappers::TcpListenerStream::new(listener),
        allow,
    };
    server(db)
        .serve_with_incoming_shutdown(incoming, async move {
            while !shutdown.is_triggered() {
                tokio::time::sleep(SHUTDOWN_POLL).await;
            }
        })
        .await
}

/// An incoming-connection stream that drops what the allow-list refuses.
///
/// Hand-written rather than assembled from stream combinators: it is a dozen lines, and the
/// alternative is a `futures` dependency for one `filter`.
#[cfg(all(feature = "docker", unix))]
struct Allowed {
    inner: tokio_stream::wrappers::TcpListenerStream,
    allow: Option<crate::PeerFilter>,
}

#[cfg(all(feature = "docker", unix))]
impl tokio_stream::Stream for Allowed {
    type Item = std::io::Result<tokio::net::TcpStream>;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        use std::task::Poll;
        loop {
            let polled = std::pin::Pin::new(&mut self.inner).poll_next(cx);
            let Poll::Ready(Some(Ok(stream))) = polled else {
                return polled;
            };
            let allowed = match (&self.allow, stream.peer_addr()) {
                (None, _) => true,
                (Some(allow), Ok(peer)) => allow(peer.ip()),
                // A connection whose peer cannot be read is one that has already gone away; there
                // is nothing to test and nothing to serve.
                (Some(_), Err(_)) => false,
            };
            if allowed {
                return Poll::Ready(Some(Ok(stream)));
            }
            // Dropping the stream closes it. Round again rather than returning `Pending`, which
            // would park the listener with a readable socket and no waker pending.
            drop(stream);
        }
    }
}

/// How often the gRPC shutdown future rechecks the token. Bounds how long `imbhd`'s exit waits on
/// this listener; small enough to be invisible next to a supervisor's stop grace.
const SHUTDOWN_POLL: std::time::Duration = std::time::Duration::from_millis(50);

/// The three collector services over one shared handler.
fn server(db: Arc<Db>) -> tonic::transport::server::Router {
    let handler = Arc::new(OtlpGrpc { db });
    tonic::transport::Server::builder()
        .add_service(LogsServiceServer::from_arc(handler.clone()))
        .add_service(TraceServiceServer::from_arc(handler.clone()))
        .add_service(MetricsServiceServer::from_arc(handler))
}

/// Blocking entry point for the `imbhd` binary: build a multi-threaded tokio runtime and run
/// [`serve_grpc`] on it until the process exits. Mirrors `serve()`'s blocking contract so `main` can
/// run one server on a thread and the other in the foreground.
pub fn serve_grpc_blocking(db: Arc<Db>, addr: &str) -> Result<(), Box<dyn std::error::Error>> {
    let addr: SocketAddr = addr.parse()?;
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    rt.block_on(serve_grpc(db, addr))?;
    Ok(())
}

/// [`serve_grpc_blocking`], stopping when `shutdown` trips — what `imbhd` runs on its gRPC thread.
///
/// Returning drops the runtime, which is also what stops the worker threads tonic spawned.
pub fn serve_grpc_blocking_until(
    db: Arc<Db>,
    addr: &str,
    shutdown: Arc<crate::Shutdown>,
) -> Result<(), Box<dyn std::error::Error>> {
    let addr: SocketAddr = addr.parse()?;
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    rt.block_on(serve_grpc_until(db, addr, shutdown))?;
    Ok(())
}

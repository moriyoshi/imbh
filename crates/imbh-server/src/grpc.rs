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
    ExportMetricsServiceRequest, ExportMetricsServiceResponse,
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
        self.db
            .ingest_otlp_metrics(&bytes)
            .await
            .map_err(|e| to_status(&e))?;
        Ok(Response::new(ExportMetricsServiceResponse::default()))
    }
}

/// Serve OTLP/gRPC on `addr` until the future is dropped or the server errors. `async`: the caller
/// supplies the runtime (the tests drive it directly; `serve_grpc_blocking` wraps it for the binary).
///
/// All three collector services share one `Arc<Db>` via `from_arc`, so ingest fans into the same
/// buffer/WAL the HTTP routes write to.
pub async fn serve_grpc(db: Arc<Db>, addr: SocketAddr) -> Result<(), tonic::transport::Error> {
    let handler = Arc::new(OtlpGrpc { db });
    tonic::transport::Server::builder()
        .add_service(LogsServiceServer::from_arc(handler.clone()))
        .add_service(TraceServiceServer::from_arc(handler.clone()))
        .add_service(MetricsServiceServer::from_arc(handler))
        .serve(addr)
        .await
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

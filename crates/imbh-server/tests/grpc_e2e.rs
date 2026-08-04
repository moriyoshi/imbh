//! End-to-end test of the reference server's OTLP/gRPC ingest (the optional `grpc` feature) over a
//! **real loopback HTTP/2 socket**: bind an ephemeral `127.0.0.1` port, run `serve_grpc()` on a tokio
//! task, and drive it with generated tonic OTLP clients. This exercises the full gRPC path — HTTP/2
//! framing, protobuf decode, the `export` handlers, and the `Db::ingest_otlp_*` fan-in — the surface
//! neither the socket-free `route()` unit tests nor the HTTP `http_e2e` test can reach. Loopback only,
//! no external daemon, so it stays within the hermetic `cargo test` rule (TESTING.md Layer 1).
//!
//! Gated on the `grpc` feature: `cargo test -p imbh-server --features grpc`. Absent the feature the
//! file compiles to nothing, so the default `cargo test --workspace` path is unaffected.
#![cfg(feature = "grpc")]

use std::net::TcpListener;
use std::sync::Arc;
use std::time::Duration;

use imbh::Db;
use imbh::arrow::array::Array;
use imbh_server::grpc::serve_grpc;
use imbh_test_support::otlp::{otlp_hist, otlp_log, otlp_metrics, otlp_trace};
use prost::Message;

use opentelemetry_proto::tonic::collector::logs::v1::{
    ExportLogsServiceRequest, logs_service_client::LogsServiceClient,
};
use opentelemetry_proto::tonic::collector::metrics::v1::{
    ExportMetricsServiceRequest, metrics_service_client::MetricsServiceClient,
};
use opentelemetry_proto::tonic::collector::trace::v1::{
    ExportTraceServiceRequest, trace_service_client::TraceServiceClient,
};

/// Grab a free `127.0.0.1` port by binding `:0` and immediately releasing it (same approach as the
/// HTTP e2e test). A tiny window exists before `serve_grpc()` re-binds, but nothing else races for a
/// loopback ephemeral port in a hermetic test.
fn free_addr() -> String {
    let l = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    l.local_addr().expect("local addr").to_string()
}

/// Total rows across the batches of a `SELECT` — used to assert the gRPC ingest landed in the buffer.
async fn count_rows(db: &Arc<Db>, sql: &str) -> usize {
    db.sql(sql)
        .collect()
        .await
        .expect("query")
        .iter()
        .map(|b| b.num_rows())
        .sum()
}

#[tokio::test(flavor = "multi_thread")]
async fn grpc_wire_ingest_all_signals_and_errors() {
    let addr = free_addr();
    let db: Arc<Db> = Db::in_memory().open().expect("open in-memory db");

    // Serve on a background task over a clone; keep `db` to verify ingest landed.
    let serve_db = db.clone();
    let serve_addr = addr.clone();
    tokio::spawn(async move {
        let _ = serve_grpc(serve_db, serve_addr.parse().expect("addr")).await;
    });

    // Connect (retrying until the accept loop is up). tonic needs an `http://` origin.
    let endpoint = format!("http://{addr}");
    let mut logs = None;
    for _ in 0..200 {
        if let Ok(c) = LogsServiceClient::connect(endpoint.clone()).await {
            logs = Some(c);
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let mut logs = logs.expect("gRPC server did not become ready");
    let mut traces = TraceServiceClient::connect(endpoint.clone())
        .await
        .expect("connect traces");
    let mut metrics = MetricsServiceClient::connect(endpoint.clone())
        .await
        .expect("connect metrics");

    // The `otlp_*` helpers emit encoded Export*ServiceRequest bytes; decode them back into the typed
    // request the tonic client sends.
    let log_req =
        ExportLogsServiceRequest::decode(otlp_log("cart", "hello", 1).as_slice()).unwrap();
    logs.export(log_req).await.expect("export logs");

    let trace_req = ExportTraceServiceRequest::decode(
        otlp_trace("cart", "GET /x", 2, 1000, 1500, 0).as_slice(),
    )
    .unwrap();
    traces.export(trace_req).await.expect("export traces");

    let metrics_req = ExportMetricsServiceRequest::decode(otlp_metrics("cart").as_slice()).unwrap();
    metrics.export(metrics_req).await.expect("export metrics");

    // A List-typed column (histogram bucket_counts) must survive the decode→re-encode→ingest path.
    let hist_req =
        ExportMetricsServiceRequest::decode(otlp_hist("lat", &[1.0, 5.0], &[2, 3, 2]).as_slice())
            .unwrap();
    metrics.export(hist_req).await.expect("export hist");

    // Verify each signal landed in the shared Db via SQL.
    assert_eq!(
        count_rows(&db, "SELECT service FROM logs WHERE service = 'cart'").await,
        1,
        "logs ingested over gRPC"
    );
    assert_eq!(
        count_rows(&db, "SELECT * FROM spans WHERE service = 'cart'").await,
        1,
        "traces ingested over gRPC"
    );
    // cpu (gauge) + requests (sum) = 2 scalar points.
    assert_eq!(
        count_rows(&db, "SELECT * FROM metrics_gauge").await
            + count_rows(&db, "SELECT * FROM metrics_sum").await,
        2,
        "metric scalar points ingested over gRPC"
    );
    let hist = db
        .sql("SELECT metric, bucket_counts FROM metrics_histogram")
        .collect()
        .await
        .expect("hist query");
    assert!(
        hist.iter()
            .any(|b| b.num_rows() > 0 && !b.column(0).is_null(0)),
        "histogram (List column) ingested over gRPC"
    );

    // A well-formed empty request is a no-op success (0 points), not an error — the OTLP equivalent
    // of the HTTP e2e's empty-body case. The imbh→gRPC error classifier itself is unit-tested.
    logs.export(ExportLogsServiceRequest::default())
        .await
        .expect("empty export is accepted");
}

/// The gRPC listener stops on a triggered token and gives the port back — `imbhd`'s SIGTERM path for
/// this endpoint. An export accepted just before the trigger is still in the DB afterwards, since
/// tonic's graceful shutdown lets in-flight calls finish.
#[tokio::test(flavor = "multi_thread")]
async fn shutdown_stops_the_grpc_listener_and_frees_the_port() {
    use imbh_server::Shutdown;
    use imbh_server::grpc::serve_grpc_until;

    let addr = free_addr();
    let db: Arc<Db> = Db::in_memory().open().expect("open in-memory db");
    let shutdown = Shutdown::with_drain_timeout(Duration::from_secs(5));

    let server = {
        let (db, serve_addr, shutdown) = (db.clone(), addr.clone(), shutdown.clone());
        tokio::spawn(async move {
            serve_grpc_until(db, serve_addr.parse().expect("addr"), shutdown)
                .await
                .expect("serve gRPC")
        })
    };

    let endpoint = format!("http://{addr}");
    let mut logs = None;
    for _ in 0..200 {
        if let Ok(c) = LogsServiceClient::connect(endpoint.clone()).await {
            logs = Some(c);
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let mut logs = logs.expect("gRPC server did not become ready");
    let req = ExportLogsServiceRequest::decode(otlp_log("cart", "before shutdown", 1).as_slice())
        .expect("decode");
    logs.export(req).await.expect("export before shutdown");

    shutdown.trigger();
    // tonic's shutdown future is the one place the token is polled, so this is bounded by that tick
    // plus the runtime's scheduling — a whole second of slack, and it hangs if the wiring regresses.
    tokio::time::timeout(Duration::from_secs(1), server)
        .await
        .expect("the gRPC server stops promptly")
        .expect("the server task did not panic");

    // The port is free for a restart.
    assert!(
        TcpListener::bind(&addr).is_ok(),
        "the gRPC port is still held after shutdown"
    );
    // And the row exported a moment before the trigger is in the DB.
    assert_eq!(count_rows(&db, "SELECT body FROM logs").await, 1);
}

/// OTLP's own channel for "I took some of that, not all of it": under `Duplicates::Reject` a
/// duplicate `(series, timestamp)` comes back as `partial_success.rejected_data_points`, which is
/// what an SDK exporter surfaces to the producer (issue #27). A fully accepted export leaves
/// `partial_success` unset, as the spec asks.
#[tokio::test(flavor = "multi_thread")]
async fn grpc_metrics_export_reports_partial_success_on_duplicates() {
    use imbh_test_support::otlp::otlp_sum;

    let addr = free_addr();
    let db: Arc<Db> = Db::in_memory()
        .duplicates(imbh::Duplicates::reject())
        .open()
        .expect("open in-memory db");

    let serve_db = db.clone();
    let serve_addr = addr.clone();
    tokio::spawn(async move {
        let _ = serve_grpc(serve_db, serve_addr.parse().expect("addr")).await;
    });

    let endpoint = format!("http://{addr}");
    let mut metrics = None;
    for _ in 0..200 {
        if let Ok(c) = MetricsServiceClient::connect(endpoint.clone()).await {
            metrics = Some(c);
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let mut metrics = metrics.expect("gRPC server did not become ready");

    let request = || {
        ExportMetricsServiceRequest::decode(otlp_sum("cart", "m", 2, &[(10, 1.0)]).as_slice())
            .unwrap()
    };

    let first = metrics.export(request()).await.expect("first export");
    assert!(
        first.into_inner().partial_success.is_none(),
        "a fully accepted export must not carry a partial-success message"
    );

    let second = metrics.export(request()).await.expect("duplicate export");
    let partial = second
        .into_inner()
        .partial_success
        .expect("the rejected duplicate is reported");
    assert_eq!(partial.rejected_data_points, 1);
    assert!(!partial.error_message.is_empty(), "the reason is named");

    assert_eq!(
        count_rows(&db, "SELECT * FROM metrics_sum WHERE metric = 'm'").await,
        1,
        "only the first point was stored"
    );
}

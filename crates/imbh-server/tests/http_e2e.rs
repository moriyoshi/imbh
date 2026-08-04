//! End-to-end test of the reference `imbhd` server over a **real loopback socket**: bind an
//! ephemeral `127.0.0.1` port, run `serve()` on a background thread, and drive it with a blocking
//! HTTP/1.1 client (`imbh_test_support::http`). This exercises the `serve()` accept loop, the
//! HTTP/1.1 request parser, and the status/error mapping over the wire — the surface the socket-free
//! `route()` unit tests can't reach. Loopback only: no external network or daemon, so it stays
//! within the hermetic `cargo test --workspace` rule (TESTING.md Layer 1).

use std::net::TcpListener;
use std::sync::Arc;
use std::time::Duration;

use imbh::Db;
use imbh_server::serve;
use imbh_test_support::http;
use imbh_test_support::otlp::{otlp_hist, otlp_log, otlp_metrics, otlp_trace};

/// Grab a free `127.0.0.1` port by binding `:0` and immediately releasing it, then hand the address
/// to `serve()`. There is a tiny window before `serve()` re-binds, but nothing else races for a
/// loopback ephemeral port in a hermetic test.
fn free_addr() -> String {
    let l = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    l.local_addr().expect("local addr").to_string()
}

/// Start `imbhd` on a background thread over an in-memory DB and wait until it answers `/health`.
fn start_server() -> String {
    start_server_with(Db::in_memory().open().expect("open in-memory db"))
}

/// Start `imbhd` over a caller-configured DB, so a test can exercise a non-default policy.
fn start_server_with(db: Arc<Db>) -> String {
    let addr = free_addr();
    let serve_addr = addr.clone();
    std::thread::spawn(move || {
        let _ = serve(db, &serve_addr);
    });

    // Poll until the accept loop is up (the thread has to re-bind the port first).
    for _ in 0..200 {
        if let Ok(resp) = http::get(&addr, "/health")
            && resp.status == 200
        {
            return addr;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("server did not become ready on {addr}");
}

#[test]
fn http_wire_ingest_query_stats_and_errors() {
    let addr = start_server();

    // Liveness.
    let health = http::get(&addr, "/health").expect("GET /health");
    assert_eq!(health.status, 200);
    assert_eq!(health.text(), "ok");
    assert_eq!(http::get(&addr, "/").expect("GET /").status, 200);

    // OTLP/HTTP ingest of all three signals over the socket.
    let logs = http::post(
        &addr,
        "/v1/logs",
        "application/x-protobuf",
        &otlp_log("cart", "hello", 1),
    )
    .expect("POST /v1/logs");
    assert_eq!(logs.status, 200);
    assert_eq!(logs.content_type, "application/json");
    assert!(
        logs.text().contains("\"accepted\":1"),
        "got {}",
        logs.text()
    );

    let traces = http::post(
        &addr,
        "/v1/traces",
        "application/x-protobuf",
        &otlp_trace("cart", "GET /x", 2, 1000, 1500, 0),
    )
    .expect("POST /v1/traces");
    assert_eq!(traces.status, 200);
    assert!(traces.text().contains("\"accepted\":1"));

    let metrics = http::post(
        &addr,
        "/v1/metrics",
        "application/x-protobuf",
        &otlp_metrics("cart"),
    )
    .expect("POST /v1/metrics");
    assert_eq!(metrics.status, 200);
    // cpu (gauge) + requests (sum) = 2 scalar points accepted.
    assert!(
        metrics.text().contains("\"accepted\":2"),
        "got {}",
        metrics.text()
    );

    // A List-typed column (histogram bucket_counts) must serialize to JSON over the wire.
    assert_eq!(
        http::post(
            &addr,
            "/v1/metrics",
            "application/x-protobuf",
            &otlp_hist("lat", &[1.0, 5.0], &[2, 3, 2])
        )
        .expect("POST hist")
        .status,
        200
    );

    // Full round-trip: query the just-ingested logs back as JSON rows.
    let q = http::post(
        &addr,
        "/api/query",
        "text/plain",
        b"SELECT service, count(*) AS c FROM logs GROUP BY service",
    )
    .expect("POST /api/query");
    assert_eq!(q.status, 200);
    let json = q.text();
    assert!(json.contains("\"service\":\"cart\""), "got {json}");
    assert!(json.contains("\"c\":1"), "got {json}");

    let hist_q = http::post(
        &addr,
        "/api/query",
        "text/plain",
        b"SELECT metric, bucket_counts FROM metrics_histogram",
    )
    .expect("POST hist query");
    assert_eq!(hist_q.status, 200);
    assert!(
        hist_q.text().contains("bucket_counts"),
        "got {}",
        hist_q.text()
    );

    // Operational stats.
    let stats = http::get(&addr, "/stats").expect("GET /stats");
    assert_eq!(stats.status, 200);
    let s = stats.text();
    assert!(s.contains("\"tables\""), "got {s}");
    assert!(s.contains("\"durable_lsn\""), "got {s}");

    // Admin maintenance actions.
    assert_eq!(
        http::post(&addr, "/admin/flush", "text/plain", b"")
            .expect("flush")
            .status,
        200
    );
    assert_eq!(
        http::post(&addr, "/admin/compact", "text/plain", b"")
            .expect("compact")
            .status,
        200
    );

    // Error paths over the wire (validates error_response's classifier mapping).
    // Malformed protobuf → decode error → user error → 400.
    let bad_proto = http::post(&addr, "/v1/logs", "application/x-protobuf", &[0x08, 0x80])
        .expect("POST bad proto");
    assert_eq!(
        bad_proto.status,
        400,
        "malformed protobuf: {}",
        bad_proto.text()
    );
    // Bad SQL → query error → user error → 400.
    let bad_sql = http::post(
        &addr,
        "/api/query",
        "text/plain",
        b"SELECT nope FROM missing",
    )
    .expect("POST bad sql");
    assert_eq!(bad_sql.status, 400, "bad sql: {}", bad_sql.text());
    // Unknown route → 404.
    assert_eq!(
        http::get(&addr, "/does/not/exist")
            .expect("GET unknown")
            .status,
        404
    );

    // An empty body is a well-formed empty request, not an error: accepted 0, still 200.
    let empty = http::post(&addr, "/v1/logs", "application/x-protobuf", b"").expect("POST empty");
    assert_eq!(empty.status, 200);
    assert!(
        empty.text().contains("\"accepted\":0"),
        "got {}",
        empty.text()
    );
}

/// The scheduler wiring `imbhd`'s `main` builds — `IMBH_FLUSH` → [`imbh_server::flush_policy`] →
/// `DbBuilder::flush`, with a background maintenance thread — over a real socket. This is the
/// regression for the reported defect: `imbhd` opened its DB with the library default
/// (`Maintenance::Manual`), so rows ingested over HTTP stayed in the buffer and the WAL forever unless
/// an operator POSTed `/admin/flush`. A short interval keeps the test quick; the default (`interval=5s`)
/// is asserted in the crate's unit tests.
#[test]
fn ingested_rows_are_sealed_without_an_admin_flush() {
    let dir = tempfile::tempdir().expect("tempdir");
    let policy = imbh_server::flush_policy(Some("interval=100ms".to_owned())).expect("policy");
    let db: Arc<Db> = Db::builder(dir.path())
        .maintenance(imbh::Maintenance::Background(
            imbh_server::maintenance_interval(None).expect("interval"),
        ))
        .flush(policy)
        .open()
        .expect("open db");

    let addr = free_addr();
    let serve_db = db.clone();
    let serve_addr = addr.clone();
    std::thread::spawn(move || {
        let _ = serve(serve_db, &serve_addr);
    });
    for _ in 0..200 {
        if let Ok(resp) = http::get(&addr, "/health")
            && resp.status == 200
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    let resp = http::post(
        &addr,
        "/v1/logs",
        "application/x-protobuf",
        &otlp_log("checkout", "scheduled seal", 1),
    )
    .expect("POST /v1/logs");
    assert_eq!(resp.status, 200, "{}", resp.text());

    // No /admin/flush anywhere in this test: the scheduler has to do it.
    let mut sealed = false;
    for _ in 0..300 {
        if !db.segments().is_empty() {
            sealed = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(sealed, "imbhd's flush scheduler never sealed the buffer");

    // And the sealed rows are still exactly what was ingested, read back over the wire.
    let rows = http::post(
        &addr,
        "/api/query",
        "text/plain",
        b"SELECT count(*) AS n FROM logs",
    )
    .expect("POST /api/query");
    assert_eq!(rows.status, 200);
    assert!(rows.text().contains("\"n\":1"), "{}", rows.text());
}

/// A duplicate `(series, timestamp)` reaching a `Duplicates::Reject` database is reported on the
/// wire in the ingest response's `rejected` count — the field that was dead before issue #27, and
/// the signal the reporter never got while their producer republished 1136 unreadable points.
#[test]
fn http_ingest_reports_rejected_duplicate_points() {
    use imbh_test_support::otlp::otlp_sum;

    let addr = start_server_with(
        Db::in_memory()
            .duplicates(imbh::Duplicates::reject())
            .open()
            .expect("open in-memory db"),
    );
    let body = otlp_sum("cart", "m", 2, &[(10, 1.0)]);

    let first = http::post(&addr, "/v1/metrics", "application/x-protobuf", &body).expect("post");
    assert_eq!(first.status, 200);
    assert!(first.text().contains("\"accepted\":1"), "{}", first.text());
    assert!(first.text().contains("\"rejected\":0"), "{}", first.text());

    let second = http::post(&addr, "/v1/metrics", "application/x-protobuf", &body).expect("post");
    assert_eq!(second.status, 200, "a duplicate is not a request error");
    assert!(
        second.text().contains("\"accepted\":0"),
        "{}",
        second.text()
    );
    assert!(
        second.text().contains("\"rejected\":1"),
        "{}",
        second.text()
    );
}

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

/// Queued housekeeping: `POST` accepts and answers a job id, the job runs, and polling that id
/// reports what it did.
///
/// The point of the endpoint is that the *submission* is fast whatever the pass costs, so the
/// assertions are about the handoff — a `202`, a terminal state on a later poll, and a report that
/// describes the work rather than the request.
#[test]
fn housekeeping_is_queued_and_polled_by_job_id() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = Db::builder(dir.path()).open().expect("open db");
    let addr = start_server_with(Arc::clone(&db));

    // Something to seal, so the pass has work to report.
    let ingest = http::post(
        &addr,
        "/v1/logs",
        "application/x-protobuf",
        &otlp_log("cart", "checkout failed", 1_000),
    )
    .expect("ingest");
    assert_eq!(ingest.status, 200, "{}", ingest.text());

    // Accepted, not performed: `202`, with an id and a non-terminal state.
    let submitted = http::post(
        &addr,
        "/admin/housekeeping",
        "application/json",
        br#"{"compact":true}"#,
    )
    .expect("submit");
    assert_eq!(submitted.status, 202, "{}", submitted.text());
    let submitted: serde_json::Value = serde_json::from_str(&submitted.text()).expect("job JSON");
    let job_id = submitted["job_id"].as_str().expect("a job id").to_owned();
    assert!(!job_id.is_empty());
    assert_eq!(submitted["compact"], serde_json::json!(true));
    assert!(
        ["queued", "running"].contains(&submitted["state"].as_str().expect("state")),
        "the work has not finished at submission time: {submitted}"
    );
    assert!(submitted["report"].is_null());

    // Poll to a terminal state.
    let mut job = serde_json::Value::Null;
    for _ in 0..400 {
        let response = http::get(&addr, &format!("/admin/housekeeping/{job_id}")).expect("poll");
        assert_eq!(response.status, 200, "{}", response.text());
        job = serde_json::from_str(&response.text()).expect("job JSON");
        if ["succeeded", "failed"].contains(&job["state"].as_str().expect("state")) {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(job["state"], "succeeded", "{job}");
    assert_eq!(job["job_id"], job_id.as_str());
    assert!(job["error"].is_null());
    // The report describes both halves, so a caller can tell a pass that found nothing to do from
    // one that never ran the compaction it asked for.
    let report = &job["report"];
    assert!(report["sealed"].is_boolean(), "{job}");
    assert!(report["segments_dropped"].is_number(), "{job}");
    assert!(report["pending_applied"].is_number(), "{job}");
    assert!(
        report["segments_merged"].is_number(),
        "compaction was requested, so its counters are present: {job}"
    );
    // The timestamps bracket the work.
    let (submitted_at, started, finished) = (
        job["submitted_unix_nano"].as_i64().expect("submitted"),
        job["started_unix_nano"].as_i64().expect("started"),
        job["finished_unix_nano"].as_i64().expect("finished"),
    );
    assert!(submitted_at <= started && started <= finished, "{job}");

    // The listing carries it, newest first.
    let listed = http::get(&addr, "/admin/housekeeping").expect("list");
    assert_eq!(listed.status, 200);
    let listed: serde_json::Value = serde_json::from_str(&listed.text()).expect("jobs JSON");
    assert_eq!(listed["jobs"][0]["job_id"], job_id.as_str(), "{listed}");

    // An id this process never issued is a 404 that says why, not an empty job.
    let missing = http::get(&addr, "/admin/housekeeping/nope-1").expect("missing");
    assert_eq!(missing.status, 404);
    assert!(missing.text().contains("restart"), "{}", missing.text());

    // A malformed body is a 400 rather than a job that silently ignores it.
    let bad = http::post(
        &addr,
        "/admin/housekeeping",
        "application/json",
        b"{not json",
    )
    .expect("bad body");
    assert_eq!(bad.status, 400, "{}", bad.text());

    // An empty body is the common `curl -XPOST` case and means the defaults.
    let plain = http::post(&addr, "/admin/housekeeping", "application/json", b"").expect("plain");
    assert_eq!(plain.status, 202, "{}", plain.text());
    let plain: serde_json::Value = serde_json::from_str(&plain.text()).expect("job JSON");
    assert_eq!(plain["compact"], serde_json::json!(false));
    assert_eq!(plain["max_jobs"], serde_json::Value::Null, "unbounded");
    assert_eq!(plain["coalesced"], serde_json::json!(false));
}

/// Duplicate submissions **coalesce**: while a pass is still queued, the same request answers with
/// that job's id rather than queueing a second one. A caller on a timer would otherwise pile up
/// passes that each do nothing the one before them did not — and the passes are serialized, so the
/// pile-up is pure wait.
#[test]
fn duplicate_submissions_join_the_queued_job() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = Db::builder(dir.path()).open().expect("open db");
    let addr = start_server_with(Arc::clone(&db));

    let submit = |body: &'static str| {
        http::post(
            &addr,
            "/admin/housekeeping",
            "application/json",
            body.as_bytes(),
        )
        .expect("submit")
    };

    // Fire a burst of the identical request. Whichever ones arrive while a pass is still queued join
    // it; the point is that no submission is *rejected* and every one names a job that will run.
    let first = submit("{}");
    assert_eq!(first.status, 202, "{}", first.text());
    let first: serde_json::Value = serde_json::from_str(&first.text()).expect("job JSON");
    let mut coalesced = 0;
    for _ in 0..8 {
        let response = submit("{}");
        let job: serde_json::Value = serde_json::from_str(&response.text()).expect("job JSON");
        assert!(
            [200, 202].contains(&response.status),
            "every submission is accepted: {}",
            response.text()
        );
        if response.status == 200 {
            assert_eq!(job["coalesced"], serde_json::json!(true));
            assert_eq!(
                job["state"], "queued",
                "only a *queued* pass is joined — a running one may already be past what this \
                 request wants covered: {job}"
            );
            coalesced += 1;
        } else {
            assert_eq!(job["coalesced"], serde_json::json!(false));
            assert_ne!(job["job_id"], first["job_id"]);
        }
    }

    // Different parameters are different work, so they never join.
    let other = submit(r#"{"compact":true}"#);
    assert_eq!(other.status, 202, "{}", other.text());
    let other: serde_json::Value = serde_json::from_str(&other.text()).expect("job JSON");
    assert_ne!(other["job_id"], first["job_id"]);

    // Whatever the interleaving was, far fewer than nine passes exist — that is the point.
    let listed = http::get(&addr, "/admin/housekeeping").expect("list");
    let listed: serde_json::Value = serde_json::from_str(&listed.text()).expect("jobs JSON");
    let count = listed["jobs"].as_array().expect("jobs").len();
    assert!(
        count <= 10 - coalesced,
        "{coalesced} submissions coalesced, so at most {} passes exist: {count}",
        10 - coalesced
    );
}

/// `max_jobs` bounds the partitions one pass rewrites — `imbh-housekeeper --max-jobs` for the
/// endpoint — so a corpus too large to compact in one call can be drained a slice at a time.
#[test]
fn housekeeping_takes_a_work_bound_and_reports_whether_it_finished() {
    const DAY: u64 = 24 * 3_600 * 1_000_000_000;
    let dir = tempfile::tempdir().expect("tempdir");
    let db = Db::builder(dir.path()).open().expect("open db");
    let addr = start_server_with(Arc::clone(&db));

    // Two segments in each of two day-partitions: two merges available, so a bound of one has
    // strictly less budget than there is work.
    for day in 1..=2u64 {
        for n in 0..2u64 {
            let ingest = http::post(
                &addr,
                "/v1/logs",
                "application/x-protobuf",
                &otlp_log("cart", "hello", day * DAY + n),
            )
            .expect("ingest");
            assert_eq!(ingest.status, 200, "{}", ingest.text());
            let flush = http::post(&addr, "/admin/flush", "application/json", b"").expect("flush");
            assert_eq!(flush.status, 200, "{}", flush.text());
        }
    }

    let run = |body: &str| -> serde_json::Value {
        let submitted = http::post(
            &addr,
            "/admin/housekeeping",
            "application/json",
            body.as_bytes(),
        )
        .expect("submit");
        assert_eq!(submitted.status, 202, "{}", submitted.text());
        let submitted: serde_json::Value =
            serde_json::from_str(&submitted.text()).expect("job JSON");
        let id = submitted["job_id"].as_str().expect("job id").to_owned();
        for _ in 0..400 {
            let response = http::get(&addr, &format!("/admin/housekeeping/{id}")).expect("poll");
            let job: serde_json::Value = serde_json::from_str(&response.text()).expect("job JSON");
            if ["succeeded", "failed"].contains(&job["state"].as_str().expect("state")) {
                assert_eq!(job["state"], "succeeded", "{job}");
                return job;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("housekeeping job did not finish");
    };

    // A bound of one does one partition and says it did not finish.
    let first = run(r#"{"compact":true,"max_jobs":1}"#);
    assert_eq!(first["max_jobs"], serde_json::json!(1));
    assert_eq!(first["report"]["segments_created"], serde_json::json!(1));
    assert_eq!(
        first["report"]["compaction_complete"],
        serde_json::json!(false),
        "the pass hit its bound, so there is more to do: {first}"
    );

    // The next pass takes the rest, and the one after finds nothing — what a drain loop stops on.
    let second = run(r#"{"compact":true,"max_jobs":4}"#);
    assert_eq!(second["report"]["segments_created"], serde_json::json!(1));
    assert_eq!(
        second["report"]["compaction_complete"],
        serde_json::json!(true),
        "under its bound, so nothing was left out: {second}"
    );
    let drained = run(r#"{"compact":true,"max_jobs":4}"#);
    assert_eq!(drained["report"]["segments_created"], serde_json::json!(0));

    // Zero is refused rather than accepted as a compaction that compacts nothing.
    let refused = http::post(
        &addr,
        "/admin/housekeeping",
        "application/json",
        br#"{"compact":true,"max_jobs":0}"#,
    )
    .expect("submit");
    assert_eq!(refused.status, 400, "{}", refused.text());
    assert!(refused.text().contains("positive"), "{}", refused.text());
}

/// A read-only server refuses the submission rather than queueing a pass that could never do
/// anything: a reader holds no writer lock, so the job would exist only to fail.
#[test]
fn housekeeping_is_refused_where_it_could_not_run() {
    let dir = tempfile::tempdir().expect("tempdir");
    // A writer has to create the directory (and its WAL) before a reader can open it.
    let writer = Db::builder(dir.path()).open().expect("open db");
    let addr = start_server_with(Db::open_read_only(dir.path()).expect("open read-only"));

    let refused =
        http::post(&addr, "/admin/housekeeping", "application/json", b"{}").expect("submit");
    assert_eq!(refused.status, 400, "{}", refused.text());
    assert!(refused.text().contains("read-only"), "{}", refused.text());
    // Nothing was queued, so there is nothing to poll.
    let listed = http::get(&addr, "/admin/housekeeping").expect("list");
    let listed: serde_json::Value = serde_json::from_str(&listed.text()).expect("jobs JSON");
    assert_eq!(listed["jobs"].as_array().expect("jobs").len(), 0);

    drop(writer);
}

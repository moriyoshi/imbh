//! End-to-end test of the **head API** over a real loopback socket (ARCHITECTURE.md §10.19).
//!
//! The property under test is the one the whole design rests on: a head pointed at a running
//! `imbhd` must get the *same answer* as a head that opened the database itself. So every case here
//! runs the operation both ways over the same `Arc<Db>` — `imbh_head::exec::*` in-process, and
//! `HeadClient` through `serve()`'s accept loop, the HTTP/1.1 stack, and the Arrow IPC or JSON
//! codec — and asserts the two agree. Anything the transport loses (a non-finite sample value, a
//! paging cursor, a nested attribute map, the narrowed trace window) fails here rather than in a
//! terminal.
//!
//! Loopback only: no external network or daemon, so it stays within the hermetic
//! `cargo test --workspace` rule (TESTING.md Layer 1).

use std::net::TcpListener;
use std::sync::Arc;
use std::time::Duration;

use imbh::Db;
use imbh_head::client::HeadClient;
use imbh_head::{dto, exec};
use imbh_server::serve;
use imbh_test_support::http;
use imbh_test_support::otlp::{otlp_log, otlp_metrics, otlp_trace};

fn free_addr() -> String {
    let l = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    l.local_addr().expect("local addr").to_string()
}

/// Start `imbhd` over a shared in-memory DB, ingest one of each signal, and hand back both halves:
/// the `Db` a local head would hold and a client onto the daemon serving it.
fn start() -> (Arc<Db>, HeadClient) {
    let addr = free_addr();
    let db: Arc<Db> = Db::in_memory().open().expect("open in-memory db");
    let served = Arc::clone(&db);
    let serve_addr = addr.clone();
    std::thread::spawn(move || {
        let _ = serve(served, &serve_addr);
    });
    for _ in 0..200 {
        if let Ok(resp) = http::get(&addr, "/health")
            && resp.status == 200
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    // Ingest through the real OTLP endpoints, so the rows under test are the ones a collector would
    // have produced rather than ones a fixture wrote straight into storage.
    let post = |path: &str, body: Vec<u8>| {
        let resp = http::post(&addr, path, "application/x-protobuf", &body).expect(path);
        assert_eq!(resp.status, 200, "{path}: {}", resp.text());
    };
    post("/v1/logs", otlp_log("cart", "checkout failed", 1_000));
    post("/v1/logs", otlp_log("cart", "checkout retried", 2_000));
    post("/v1/logs", otlp_log("api", "ok", 3_000));
    post(
        "/v1/traces",
        otlp_trace("api", "GET /cart", 2, 1_000, 5_000, 2),
    );
    post("/v1/metrics", otlp_metrics("cart"));

    (db, HeadClient::new(&addr).expect("head client"))
}

/// The window every query in this test runs over: wide enough to contain the fixtures, whose
/// timestamps are small absolute nanosecond values rather than "now". The step is chosen so the
/// span yields a handful of evaluation points rather than tripping the engine's own point cap.
const WINDOW: dto::EvalWindow = dto::EvalWindow {
    start_ns: 0,
    end_ns: 1_000_000_000,
    step_ns: 100_000_000,
    lookback_ns: 1_000_000_000,
};

fn eval(query: &str) -> dto::EvalRequest {
    dto::EvalRequest::one(query, WINDOW, dto::EvalCaps::default())
}

#[tokio::test]
async fn a_remote_head_sees_what_a_local_one_sees() {
    let (db, client) = start();

    // ── stats (JSON) ────────────────────────────────────────────────────────────────────────────
    let local = exec::stats(&db).await.expect("local stats");
    let remote = client.stats().await.expect("remote stats");
    assert_eq!(local, remote);
    // And it is a real answer, not two identical empties.
    assert!(
        local
            .tables
            .iter()
            .any(|t| t.buffer_rows + t.segment_rows > 0),
        "the fixtures should have landed somewhere: {local:?}"
    );

    // ── metric catalog (JSON) ───────────────────────────────────────────────────────────────────
    let names = |catalog: &dto::MetricCatalog| {
        catalog
            .metrics
            .iter()
            .map(|m| m.metric.clone())
            .collect::<Vec<_>>()
    };
    let local = exec::metric_catalog(&db).await.expect("local catalog");
    let remote = client.metric_catalog().await.expect("remote catalog");
    assert_eq!(names(&local), names(&remote));
    assert!(
        names(&local).contains(&"cpu".to_owned()),
        "{:?}",
        names(&local)
    );

    // ── PromQL (Arrow IPC) ──────────────────────────────────────────────────────────────────────
    let request = eval("cpu");
    let local = exec::promql(&db, &request).await.expect("local promql");
    let remote = client.promql(&request).await.expect("remote promql");
    assert_eq!(local, remote);
    assert!(!local.is_empty(), "`cpu` should evaluate to a series");
    assert!(
        local[0].samples.iter().any(|s| s.value == 0.5),
        "the gauge's value must survive the wire: {local:?}"
    );

    // Several queries in one request: the series concatenate in request order, and — the reason the
    // request is plural — the metric catalog is read once for the lot rather than once apiece.
    let request = dto::EvalRequest {
        queries: vec!["cpu".to_owned(), "requests".to_owned()],
        window: WINDOW,
        caps: dto::EvalCaps::default(),
    };
    let local = exec::promql(&db, &request).await.expect("local promql");
    let remote = client.promql(&request).await.expect("remote promql");
    assert_eq!(local, remote);
    let named = |series: &dto::Series| {
        series
            .labels
            .iter()
            .find(|l| l.name == "__name__")
            .map(|l| l.value.clone())
    };
    assert_eq!(
        local.iter().filter_map(named).collect::<Vec<_>>(),
        vec!["cpu".to_owned(), "requests".to_owned()],
        "each sub-query keeps its own __name__, in request order"
    );

    // Aggregation drops `__name__` (Prometheus semantics), so two sub-queries answering with the
    // *same* labels is ordinary. The wire must still hand back two series: recovering the
    // boundaries by grouping equal labels fused them, which is a remote head disagreeing with a
    // local one over the same data.
    let request = dto::EvalRequest {
        queries: vec!["sum(cpu)".to_owned(), "sum(cpu)".to_owned()],
        window: WINDOW,
        caps: dto::EvalCaps::default(),
    };
    let local = exec::promql(&db, &request).await.expect("local promql");
    let remote = client.promql(&request).await.expect("remote promql");
    assert_eq!(local.len(), 2, "two sub-queries, two series: {local:?}");
    assert!(local[0].labels.is_empty() && local[1].labels.is_empty());
    assert_eq!(local, remote);

    // ── metric dimensions (JSON) ────────────────────────────────────────────────────────────────
    let request = dto::MetricDimensionsRequest {
        metric: "cpu".to_owned(),
        max_values: None,
    };
    let local = exec::metric_dimensions(&db, &request)
        .await
        .expect("local dimensions");
    let remote = client
        .metric_dimensions(&request)
        .await
        .expect("remote dimensions");
    assert_eq!(local, remote);
    assert_eq!(
        local.dimensions,
        vec![dto::MetricDimension {
            label: "service".to_owned(),
            values: vec!["cart".to_owned()],
            truncated: false,
        }],
        "the resource axis is groupable and comes back named"
    );

    // ── LogQL metric expression (Arrow IPC) ─────────────────────────────────────────────────────
    let request = eval(r#"rate({service="cart"}[5m])"#);
    let local = exec::logql(&db, &request).await.expect("local logql");
    let remote = client.logql(&request).await.expect("remote logql");
    assert_eq!(local, remote);

    // ── TraceQL (Arrow IPC), including the narrowed-window report ────────────────────────────────
    let request = dto::TraceSearchRequest {
        query: "{}".to_owned(),
        start_ns: 0,
        end_ns: 1_000_000_000,
        caps: dto::EvalCaps::default(),
        narrow_steps: 6,
    };
    let local = exec::traceql(&db, &request).await.expect("local traceql");
    let remote = client.traceql(&request).await.expect("remote traceql");
    assert_eq!(local, remote);
    assert_eq!(local.matches.len(), 1, "{local:?}");
    assert_eq!(local.effective_start_ns, 0, "the full window fits");
    let trace_id = local.matches[0].trace_id.clone();

    // ── one trace (Arrow IPC) — the waterfall's source ───────────────────────────────────────────
    let request = dto::TraceGetRequest {
        trace_id: trace_id.clone(),
    };
    let local = exec::trace(&db, &request).await.expect("local trace");
    let remote = client.trace(&request).await.expect("remote trace");
    let (local, remote) = (local.expect("present"), remote.expect("present"));
    assert_eq!(local.trace_id, remote.trace_id);
    assert_eq!(local.root_service, remote.root_service);
    assert_eq!(local.root_name, remote.root_name);
    assert_eq!(local.start_time, remote.start_time);
    assert_eq!(local.duration_ns, remote.duration_ns);
    assert_eq!(local.spans.len(), remote.spans.len());
    assert_eq!(local.spans[0].name, remote.spans[0].name);
    assert_eq!(local.spans[0].attributes, remote.spans[0].attributes);
    assert_eq!(local.spans[0].resource, remote.spans[0].resource);
    assert_eq!(local.root_name.as_deref(), Some("GET /cart"));

    // A trace that is not there is `None`, not an error — and not an empty trace either.
    let missing = dto::TraceGetRequest {
        trace_id: "ff".repeat(16),
    };
    assert!(exec::trace(&db, &missing).await.expect("local").is_none());
    assert!(client.trace(&missing).await.expect("remote").is_none());

    // ── a log page (Arrow IPC) — entries, scan stats, and the paging cursor ──────────────────────
    let request = dto::LogQueryRequest {
        query: imbh::LogQuery::new().limit(2),
    };
    let local = exec::log_query(&db, &request).await.expect("local logs");
    let remote = client.log_query(&request).await.expect("remote logs");
    assert_eq!(local.entries.len(), remote.entries.len());
    assert_eq!(local.entries.len(), 2, "the limit is what bounds the page");
    for (local, remote) in local.entries.iter().zip(&remote.entries) {
        assert_eq!(local.time, remote.time);
        assert_eq!(local.body, remote.body);
        assert_eq!(local.service, remote.service);
        assert_eq!(local.severity_number, remote.severity_number);
        assert_eq!(local.trace_id, remote.trace_id);
        // The nested attribute maps are what a JSON-of-Arrow round trip is most likely to flatten.
        assert_eq!(local.attributes, remote.attributes);
        assert_eq!(local.resource, remote.resource);
        assert_eq!(local.scope, remote.scope);
    }
    assert_eq!(local.stats.rows_scanned, remote.stats.rows_scanned);
    assert_eq!(local.stats.used_index, remote.stats.used_index);
    // A capped page has more behind it, and the cursor that reaches it must cross intact — this is
    // the `n`/`p` paging in the viewer.
    assert_eq!(local.next.is_some(), remote.next.is_some());
    assert!(remote.next.is_some(), "3 logs, page of 2");
    let paged = dto::LogQueryRequest {
        query: imbh::LogQuery::new()
            .limit(2)
            .after(remote.next.expect("cursor")),
    };
    let second = client.log_query(&paged).await.expect("second page");
    assert_eq!(second.entries.len(), 1, "the remaining row");

    // ── log volume (JSON) ───────────────────────────────────────────────────────────────────────
    let request = dto::LogVolumeRequest {
        query: imbh::LogQuery::new(),
        step_ns: 1_000_000_000,
    };
    let local = exec::log_volume(&db, &request).await.expect("local volume");
    let remote = client.log_volume(&request).await.expect("remote volume");
    let counts =
        |result: &dto::LogVolumeResult| result.buckets.iter().map(|b| b.count).collect::<Vec<_>>();
    assert_eq!(counts(&local), counts(&remote));
    assert_eq!(counts(&local).iter().sum::<u64>(), 3);

    // ── attribute vocabularies (JSON) ───────────────────────────────────────────────────────────
    let local = exec::attribute_keys(&db).await.expect("local keys");
    let remote = client.attribute_keys().await.expect("remote keys");
    assert_eq!(local, remote);
    assert!(
        local.names.iter().any(|n| n == "service.name"),
        "{:?}",
        local.names
    );

    let request = dto::AttributeValuesRequest {
        key: "service.name".to_owned(),
    };
    let local = exec::attribute_values(&db, &request).await.expect("local");
    let remote = client.attribute_values(&request).await.expect("remote");
    assert_eq!(local, remote);
    assert!(
        local.names.contains(&"cart".to_owned()),
        "{:?}",
        local.names
    );

    // ── exemplars (JSON) ────────────────────────────────────────────────────────────────────────
    let request = dto::ExemplarsRequest {
        metric: "cpu".to_owned(),
    };
    let local = exec::exemplars(&db, &request).await.expect("local");
    let remote = client.exemplars(&request).await.expect("remote");
    assert_eq!(local.exemplars, remote.exemplars);
}

#[tokio::test]
async fn failures_cross_the_wire_as_the_database_stated_them() {
    let (db, client) = start();

    // A query-language error is the daemon's message, verbatim — not "HTTP 400".
    let request = eval("this is not promql {{{");
    let local = exec::promql(&db, &request).await.expect_err("local");
    let remote = client.promql(&request).await.expect_err("remote");
    assert_eq!(local.status(), 400);
    assert_eq!(remote.status(), 400);
    assert_eq!(local.message(), remote.message());

    // A LogQL *selector* is not a metric expression, and saying so is more useful than an empty plot.
    let request = eval(r#"{service="cart"}"#);
    let remote = client.logql(&request).await.expect_err("not a metric expr");
    assert!(
        remote.message().contains("range aggregation"),
        "{}",
        remote.message()
    );

    // A malformed id is refused before any query runs.
    let request = dto::TraceGetRequest {
        trace_id: "not-hex".to_owned(),
    };
    let remote = client.trace(&request).await.expect_err("bad id");
    assert_eq!(remote.status(), 400);
    assert!(remote.message().contains("hex trace id"), "{remote}");

    // A blown cap is flagged as such, so a head knows to retry in a narrower window rather than to
    // tell the user their query is wrong.
    let request = dto::TraceSearchRequest {
        query: "{}".to_owned(),
        start_ns: 0,
        end_ns: 1_000_000_000,
        caps: dto::EvalCaps {
            max_traces: Some(0),
            ..dto::EvalCaps::default()
        },
        // No retries, so the cap surfaces instead of being narrowed away.
        narrow_steps: 0,
    };
    let local = exec::traceql(&db, &request).await.expect_err("local cap");
    let remote = client.traceql(&request).await.expect_err("remote cap");
    assert!(local.is_limit_exceeded(), "{local:?}");
    assert!(remote.is_limit_exceeded(), "{remote:?}");
    assert_eq!(local.message(), remote.message());
}

#[tokio::test]
async fn a_head_pointed_at_nothing_says_so() {
    // Nothing is listening on this port: the failure must name the address, and be a transport
    // failure rather than a database answer, so a head can tell "imbhd is down" from "bad query".
    let client = HeadClient::new(&free_addr()).expect("client");
    let e = client.stats().await.expect_err("nothing is listening");
    assert!(matches!(e, imbh_head::HeadError::Transport(_)), "{e:?}");
    assert!(
        e.message().contains("cannot reach the imbh head API"),
        "{e}"
    );
    assert_eq!(e.status(), 503);
}

#[test]
fn the_head_prefix_is_a_gateable_unit() {
    // Every head path lives below one prefix, which is what lets a deployment put an auth check (or a
    // deny rule) in front of the whole surface without enumerating it.
    for path in [
        imbh_head::path::STATS,
        imbh_head::path::METRICS_CATALOG,
        imbh_head::path::METRICS_DIMENSIONS,
        imbh_head::path::METRICS_PROMQL,
        imbh_head::path::METRICS_EXEMPLARS,
        imbh_head::path::TRACES_SEARCH,
        imbh_head::path::TRACES_GET,
        imbh_head::path::LOGS_QUERY,
        imbh_head::path::LOGS_VOLUME,
        imbh_head::path::LOGS_LOGQL,
        imbh_head::path::ATTRIBUTES_KEYS,
        imbh_head::path::ATTRIBUTES_VALUES,
    ] {
        assert!(
            path.starts_with(imbh_head::path::PREFIX),
            "{path} is outside the head prefix"
        );
    }
}

/// Issue #27's user-visible symptom, end to end: two stored points at one instant made every PromQL
/// query of that metric a bare `400 Bad Request` naming nothing. The failure still happens under the
/// default policy — but it now names the metric, the label set and the instant, and it carries a
/// machine-readable kind so a head can say "fix the producer" rather than "try a shorter range".
#[tokio::test]
async fn a_duplicate_timestamp_names_the_series_over_the_wire() {
    use imbh_test_support::otlp::otlp_sum;

    let addr = free_addr();
    let db: Arc<Db> = Db::in_memory().open().expect("open in-memory db");
    let served = Arc::clone(&db);
    let serve_addr = addr.clone();
    std::thread::spawn(move || {
        let _ = serve(served, &serve_addr);
    });
    for _ in 0..200 {
        if let Ok(resp) = http::get(&addr, "/health")
            && resp.status == 200
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    // Two points at one instant, exactly as a source republishing its own last reading produces.
    let body = otlp_sum("cart", "dupe", 2, &[(1_000, 1.0), (1_000, 5.0)]);
    let resp = http::post(&addr, "/v1/metrics", "application/x-protobuf", &body).expect("ingest");
    assert_eq!(resp.status, 200, "ingest still succeeds: {}", resp.text());

    let client = HeadClient::new(&addr).expect("head client");
    let request = eval("dupe");
    let local = exec::promql(&db, &request).await.expect_err("local");
    let remote = client.promql(&request).await.expect_err("remote");

    assert_eq!(local.status(), 400);
    assert_eq!(local.message(), remote.message(), "one message, both paths");
    assert!(remote.is_duplicate_timestamp(), "{}", remote.message());
    for expected in ["__name__=\"dupe\"", "service=\"cart\"", "1000"] {
        assert!(
            remote.message().contains(expected),
            "message should name {expected}: {}",
            remote.message()
        );
    }
}

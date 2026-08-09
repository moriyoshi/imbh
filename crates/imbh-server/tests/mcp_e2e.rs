//! End-to-end test of `imbhd`'s MCP endpoint: ingest real OTLP over the router, then drive
//! `POST /mcp` the way a client does — the legacy `initialize` handshake, the stateless
//! `2026-07-28` era, and the tool calls themselves.
//!
//! Requests go through [`imbh_server::app`] with `tower`'s `oneshot` rather than a socket, because
//! the modern transport's rules are about *headers* (`MCP-Protocol-Version`, `Mcp-Method`,
//! `Mcp-Name`) and the socket-free `route()` helper cannot set them. Everything is in-memory: no
//! daemon, no network (TESTING.md Layer 1).

use std::sync::Arc;

use axum::body::Body;
use axum::http::Request;
use imbh::Db;
use imbh_server::app;
use imbh_test_support::otlp::{
    otlp_hist, otlp_metrics, otlp_rich, otlp_trace_tree, otlp_trace_wide,
};
use serde_json::{Value, json};
use tower::ServiceExt;

/// The fixtures sit at epoch nanosecond ~1, so every tool call passes explicit bounds — a `since`
/// window ending *now* would exclude them. That is also the path a model takes after reading
/// `db_stats`, which reports each table's real time span.
const WINDOW: &str = r#""start_unix_nano":0,"end_unix_nano":9223372036854775807"#;

const TRACE_ID: [u8; 16] = [0x11; 16];
const TRACE_HEX: &str = "11111111111111111111111111111111";

/// The version this server implements for the stateless era. Kept as a literal so a bump to
/// `imbh_server::mcp::LATEST_VERSION` has to be a deliberate edit here too.
const MODERN: &str = "2026-07-28";

async fn post(db: &Arc<Db>, headers: &[(&str, &str)], body: &str) -> (u16, String) {
    let mut request = Request::builder().method("POST").uri("/mcp");
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let response = app(db.clone())
        .oneshot(request.body(Body::from(body.to_owned())).expect("request"))
        .await
        .expect("router is infallible");
    let status = response.status().as_u16();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read body");
    (
        status,
        String::from_utf8(body.to_vec()).expect("utf-8 body"),
    )
}

/// A legacy-era request: no `_meta`, no headers to agree with.
async fn legacy(db: &Arc<Db>, body: &str) -> (u16, String) {
    post(db, &[], body).await
}

/// A modern-era `tools/call`, with the header mirror the transport requires.
async fn call_tool(db: &Arc<Db>, tool: &str, arguments: &str) -> String {
    let body = format!(
        r#"{{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{{"name":"{tool}","arguments":{arguments},"_meta":{{"io.modelcontextprotocol/protocolVersion":"{MODERN}"}}}}}}"#
    );
    let (status, body) = post(
        db,
        &[
            ("mcp-protocol-version", MODERN),
            ("mcp-method", "tools/call"),
            ("mcp-name", tool),
        ],
        &body,
    )
    .await;
    assert_eq!(status, 200, "tools/call {tool} → {body}");
    body
}

/// The `text` of the single content block a tool answers with, having asserted it is not an error.
fn tool_text(response: &str) -> String {
    let value: Value = serde_json::from_str(response).expect("response is JSON");
    let result = &value["result"];
    assert!(
        !result.is_null(),
        "a result, not an error, was expected: {response}"
    );
    assert_eq!(
        result["isError"],
        json!(false),
        "tool reported an error: {response}"
    );
    let text = result["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("no text block in {response}"))
        .to_owned();
    // Every tool answers with a JSON document, so a client can parse the block rather than read it.
    serde_json::from_str::<Value>(&text).unwrap_or_else(|e| panic!("tool text is not JSON: {e}"));
    text
}

/// The parsed tool payload, for assertions that are about structure rather than substrings.
fn tool_value(response: &str) -> Value {
    serde_json::from_str(&tool_text(response)).expect("tool text is JSON")
}

async fn seeded_db() -> Arc<Db> {
    let db: Arc<Db> = Db::in_memory().open().expect("open in-memory db");
    db.ingest_otlp_logs(&otlp_rich(
        "cart",
        "checkout failed for user 42",
        1,
        17,
        &[("http.route", "/checkout")],
    ))
    .await
    .expect("ingest logs");
    db.ingest_otlp_logs(&otlp_rich("cart", "checkout ok", 2, 9, &[]))
        .await
        .expect("ingest logs");
    db.ingest_otlp_traces(&otlp_trace_tree("cart", TRACE_ID))
        .await
        .expect("ingest traces");
    // A second trace whose spans carry attributes — `otlp_trace_tree`'s spans have none, so it can
    // only exercise grouping's *shape*, not whether a group key resolves.
    db.ingest_otlp_traces(&otlp_trace_wide(
        "cart",
        [0x22; 16],
        3,
        &[("http.method", "GET")],
    ))
    .await
    .expect("ingest attributed traces");
    db.ingest_otlp_metrics(&otlp_metrics("cart"))
        .await
        .expect("ingest metrics");
    db.ingest_otlp_metrics(&otlp_hist(
        "http.server.duration",
        &[10.0, 100.0, 1000.0],
        &[1, 8, 1, 0],
    ))
    .await
    .expect("ingest histogram");
    db
}

#[tokio::test]
async fn legacy_clients_get_the_initialize_handshake() {
    let db = seeded_db().await;

    let (status, body) = legacy(
        &db,
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"test","version":"1"}}}"#,
    )
    .await;
    assert_eq!(status, 200);
    // The requested revision is supported, so it comes back unchanged.
    assert!(body.contains(r#""protocolVersion":"2025-06-18""#), "{body}");
    assert!(body.contains(r#""serverInfo":{"name":"imbhd""#), "{body}");
    assert!(body.contains(r#""tools":{"listChanged":false}"#), "{body}");
    // A legacy result carries no `resultType`: that field only exists in the stateless revision.
    assert!(!body.contains("resultType"), "{body}");

    // The handshake's third leg is a notification: accepted, no body, no JSON-RPC answer.
    let (status, body) = legacy(
        &db,
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
    )
    .await;
    assert_eq!(status, 202);
    assert!(
        body.is_empty(),
        "a notification must not be answered: {body}"
    );

    // And a legacy client can then list and call tools with no headers at all.
    let (status, body) = legacy(&db, r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#).await;
    assert_eq!(status, 200);
    assert!(body.contains(r#""name":"search_logs""#), "{body}");
    assert!(body.contains(r#""inputSchema":{"#), "{body}");

    let (status, body) = legacy(
        &db,
        &format!(
            r#"{{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{{"name":"count_logs","arguments":{{{WINDOW}}}}}}}"#
        ),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(tool_text(&body), r#"{"count":2}"#);
}

#[tokio::test]
async fn modern_clients_discover_and_call_statelessly() {
    let db = seeded_db().await;

    // `server/discover` is the probe a modern client may lead with — no handshake, no headers.
    let (status, body) = post(
        &db,
        &[],
        r#"{"jsonrpc":"2.0","id":"d1","method":"server/discover","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28"}}}"#,
    )
    .await;
    assert_eq!(status, 200);
    assert!(
        body.contains(&format!(r#""supportedVersions":["{MODERN}"]"#)),
        "{body}"
    );
    assert!(body.contains(r#""resultType":"complete""#), "{body}");
    assert!(
        body.contains("io.modelcontextprotocol/serverInfo"),
        "{body}"
    );
    // The id is echoed verbatim, string ids included.
    assert!(body.contains(r#""id":"d1""#), "{body}");

    // A tool call with the required header mirror succeeds and carries `resultType`.
    let body = call_tool(&db, "count_logs", &format!("{{{WINDOW}}}")).await;
    assert!(body.contains(r#""resultType":"complete""#), "{body}");
    assert_eq!(tool_text(&body), r#"{"count":2}"#);
}

#[tokio::test]
async fn the_modern_transport_refuses_mismatched_headers_and_versions() {
    let db = seeded_db().await;
    let body = |version: &str| {
        format!(
            r#"{{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{{"name":"count_logs","arguments":{{}},"_meta":{{"io.modelcontextprotocol/protocolVersion":"{version}"}}}}}}"#
        )
    };

    // A body that declares a version but no header to match it.
    let (status, out) = post(&db, &[], &body(MODERN)).await;
    assert_eq!(status, 400);
    assert!(out.contains(r#""code":-32020"#), "{out}");
    assert!(out.contains("MCP-Protocol-Version"), "{out}");

    // Header and body disagree — the split-brain the mirror rule exists to prevent.
    let (status, out) = post(
        &db,
        &[
            ("mcp-protocol-version", "2025-06-18"),
            ("mcp-method", "tools/call"),
            ("mcp-name", "count_logs"),
        ],
        &body(MODERN),
    )
    .await;
    assert_eq!(status, 400);
    assert!(out.contains(r#""code":-32020"#), "{out}");

    // `Mcp-Name` naming a different tool than the body.
    let (status, out) = post(
        &db,
        &[
            ("mcp-protocol-version", MODERN),
            ("mcp-method", "tools/call"),
            ("mcp-name", "query_sql"),
        ],
        &body(MODERN),
    )
    .await;
    assert_eq!(status, 400);
    assert!(out.contains(r#""code":-32020"#), "{out}");
    assert!(out.contains("count_logs"), "{out}");

    // A version this server does not implement is refused with the list it does.
    let (status, out) = post(
        &db,
        &[
            ("mcp-protocol-version", "2099-01-01"),
            ("mcp-method", "tools/call"),
            ("mcp-name", "count_logs"),
        ],
        &body("2099-01-01"),
    )
    .await;
    assert_eq!(status, 400);
    assert!(out.contains(r#""code":-32022"#), "{out}");
    assert!(
        out.contains(&format!(r#""supported":["{MODERN}"]"#)),
        "{out}"
    );
    assert!(out.contains(r#""requested":"2099-01-01""#), "{out}");
}

#[tokio::test]
async fn unknown_methods_and_tools_are_protocol_errors() {
    let db = seeded_db().await;

    // Legacy: a plain JSON-RPC error on a 200.
    let (status, body) = legacy(&db, r#"{"jsonrpc":"2.0","id":1,"method":"resources/list"}"#).await;
    assert_eq!(status, 200);
    assert!(body.contains(r#""code":-32601"#), "{body}");

    // Modern: 404, so a client can tell this from a server that hosts no MCP endpoint at all.
    let (status, body) = post(
        &db,
        &[
            ("mcp-protocol-version", MODERN),
            ("mcp-method", "resources/list"),
        ],
        &format!(
            r#"{{"jsonrpc":"2.0","id":1,"method":"resources/list","params":{{"_meta":{{"io.modelcontextprotocol/protocolVersion":"{MODERN}"}}}}}}"#
        ),
    )
    .await;
    assert_eq!(status, 404);
    assert!(body.contains(r#""code":-32601"#), "{body}");

    // An unknown *tool* is a protocol error too — no argument would fix it.
    let (status, body) = legacy(
        &db,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"drop_everything","arguments":{}}}"#,
    )
    .await;
    assert_eq!(status, 200);
    assert!(body.contains(r#""code":-32602"#), "{body}");
    assert!(body.contains("Unknown tool: drop_everything"), "{body}");
}

#[tokio::test]
async fn bad_arguments_come_back_as_tool_errors_the_model_can_fix() {
    let db = seeded_db().await;

    // A SQL error is the model's to correct, so it arrives as `isError`, not as a JSON-RPC error.
    let body = call_tool(&db, "query_sql", r#"{"sql":"SELECT nope FROM missing"}"#).await;
    assert!(body.contains(r#""isError":true"#), "{body}");
    assert!(!body.contains(r#""error":{"code""#), "{body}");

    // Same for an argument that cannot be parsed.
    let body = call_tool(&db, "search_logs", r#"{"since":"a while"}"#).await;
    assert!(body.contains(r#""isError":true"#), "{body}");
    assert!(body.contains("not a duration"), "{body}");

    let body = call_tool(&db, "get_trace", r#"{"trace_id":"nope"}"#).await;
    assert!(body.contains(r#""isError":true"#), "{body}");
    assert!(body.contains("32 hexadecimal"), "{body}");

    // A missing required argument names itself.
    let body = call_tool(&db, "metric_series", "{}").await;
    assert!(body.contains(r#""isError":true"#), "{body}");
    assert!(body.contains("`metric` is required"), "{body}");
}

#[tokio::test]
async fn the_log_tools_answer_over_real_data() {
    let db = seeded_db().await;

    let text = tool_text(&call_tool(&db, "search_logs", &format!("{{{WINDOW}}}")).await);
    assert!(text.contains(r#""entry_count":2"#), "{text}");
    assert!(text.contains("checkout failed for user 42"), "{text}");
    assert!(text.contains(r#""service":"cart""#), "{text}");
    assert!(text.contains(r#""http.route":"/checkout""#), "{text}");
    assert!(text.contains(r#""has_more":false"#), "{text}");

    // Severity filtering, by name.
    let text = tool_text(
        &call_tool(
            &db,
            "search_logs",
            &format!(r#"{{{WINDOW},"severity_at_least":"error"}}"#),
        )
        .await,
    );
    assert!(text.contains(r#""entry_count":1"#), "{text}");
    assert!(text.contains("checkout failed"), "{text}");

    // Attribute filtering.
    let text = tool_text(
        &call_tool(
            &db,
            "count_logs",
            &format!(r#"{{{WINDOW},"attributes":{{"http.route":"/checkout"}}}}"#),
        )
        .await,
    );
    assert_eq!(text, r#"{"count":1}"#);

    // Full-text search over the body — the index-accelerated path.
    let text = tool_text(
        &call_tool(
            &db,
            "count_logs",
            &format!(r#"{{{WINDOW},"matches":"failed"}}"#),
        )
        .await,
    );
    assert_eq!(text, r#"{"count":1}"#);

    // Volume buckets.
    let text =
        tool_text(&call_tool(&db, "log_volume", &format!(r#"{{{WINDOW},"step":"1h"}}"#)).await);
    assert!(text.contains(r#""buckets":[{"#), "{text}");
    assert!(text.contains(r#""count":2"#), "{text}");
}

#[tokio::test]
async fn the_trace_tools_answer_over_real_data() {
    let db = seeded_db().await;

    let text = tool_text(&call_tool(&db, "search_traces", &format!("{{{WINDOW}}}")).await);
    assert!(
        text.contains(&format!(r#""trace_id":"{TRACE_HEX}""#)),
        "{text}"
    );
    assert!(text.contains(r#""root_service":"cart""#), "{text}");

    let text = tool_text(
        &call_tool(
            &db,
            "get_trace",
            &format!(r#"{{"trace_id":"{TRACE_HEX}"}}"#),
        )
        .await,
    );
    assert!(text.contains(r#""found":true"#), "{text}");
    assert!(text.contains(r#""spans":[{"#), "{text}");
    assert!(text.contains(r#""status_code":"#), "{text}");

    // A well-formed id that is not in the database is `found: false`, not an error.
    let text = tool_text(
        &call_tool(
            &db,
            "get_trace",
            r#"{"trace_id":"00000000000000000000000000000000"}"#,
        )
        .await,
    );
    assert!(text.contains(r#""found":false"#), "{text}");

    // RED metrics from the same spans.
    let text = tool_text(
        &call_tool(
            &db,
            "span_metrics",
            &format!(r#"{{{WINDOW},"step":"1h","group_by":["http.method"]}}"#),
        )
        .await,
    );
    assert!(text.contains(r#""series":[{"#), "{text}");
    assert!(text.contains(r#""calls":"#), "{text}");
    assert!(text.contains(r#""p95_ns":"#), "{text}");
    // The grouped label must actually *resolve*, not merely be present. Asserting only the response
    // shape is what let a `group_by: ["service.name"]` example survive in the tool descriptions while
    // silently producing `{"service.name": ""}` for every series.
    assert!(text.contains(r#""http.method":"GET""#), "{text}");
    // The spans that *lack* the key group under "" — correct SQL semantics for a missing attribute,
    // and the reason a model must be told which keys are groupable rather than guessing.
    assert!(text.contains(r#""http.method":"""#), "{text}");
    // `service.name` is not a record attribute — it is the built-in `service` column — so it used to
    // group into one empty-labelled series with every count merged. `SqlParams::attr_field` now
    // resolves both its spellings to that column, so it groups like any other key.
    for key in ["service.name", "service"] {
        let text = tool_text(
            &call_tool(
                &db,
                "span_metrics",
                &format!(r#"{{{WINDOW},"step":"1h","group_by":["{key}"]}}"#),
            )
            .await,
        );
        assert!(text.contains(&format!(r#""{key}":"cart""#)), "{text}");
        assert!(!text.contains(&format!(r#""{key}":"""#)), "{text}");
    }
}

/// Grouping by `service.name` must produce one series *per service*, not one merged series. The
/// shared `seeded_db` only ever ingests `cart`, so a single-service fixture cannot tell a working
/// group-by from the collapsed one — this seeds two services and checks both the split and the
/// counts.
#[tokio::test]
async fn grouping_by_service_name_splits_the_series_per_service() {
    let db: Arc<Db> = Db::in_memory().open().expect("open in-memory db");
    db.ingest_otlp_traces(&otlp_trace_wide("cart", [0x31; 16], 3, &[]))
        .await
        .expect("ingest cart spans");
    db.ingest_otlp_traces(&otlp_trace_wide("checkout", [0x32; 16], 2, &[]))
        .await
        .expect("ingest checkout spans");

    let value = tool_value(
        &call_tool(
            &db,
            "span_metrics",
            &format!(r#"{{{WINDOW},"step":"1h","group_by":["service.name"]}}"#),
        )
        .await,
    );
    let series = value["series"].as_array().expect("a series array");
    let mut calls: Vec<(String, i64)> = series
        .iter()
        .map(|s| {
            let service = s["labels"]["service.name"]
                .as_str()
                .unwrap_or_else(|| panic!("no service.name label in {s}"))
                .to_owned();
            let calls = s["points"]
                .as_array()
                .expect("a points array")
                .iter()
                .map(|p| p["calls"].as_i64().expect("a calls count"))
                .sum();
            (service, calls)
        })
        .collect();
    calls.sort();
    // `otlp_trace_wide(_, _, n, _)` emits `n` spans (a root plus `n - 1` children).
    assert_eq!(
        calls,
        vec![("cart".to_owned(), 3), ("checkout".to_owned(), 2)],
        "{value}"
    );

    // The same key used as an attribute *filter* agrees with the breakdown — one spelling, both
    // directions. (Previously this matched nothing: `json_get_str(attributes, 'service.name')` is
    // NULL on every span.)
    let value = tool_value(
        &call_tool(
            &db,
            "span_metrics",
            &format!(r#"{{{WINDOW},"step":"1h","attributes":{{"service.name":"checkout"}}}}"#),
        )
        .await,
    );
    let filtered: i64 = value["series"][0]["points"]
        .as_array()
        .expect("a points array")
        .iter()
        .map(|p| p["calls"].as_i64().expect("a calls count"))
        .sum();
    assert_eq!(filtered, 2, "{value}");
}

#[tokio::test]
async fn the_metric_and_discovery_tools_answer_over_real_data() {
    let db = seeded_db().await;

    let catalog = tool_value(&call_tool(&db, "list_metrics", "{}").await);
    let metrics = catalog["metrics"].as_array().expect("a metrics array");
    assert!(!metrics.is_empty(), "{catalog}");

    // Pull a real metric name out of the catalogue and query it, the way a model would.
    let name = metrics
        .iter()
        .find(|m| m["kind"] == json!("gauge"))
        .and_then(|m| m["metric"].as_str())
        .expect("a gauge metric in the fixture")
        .to_owned();

    let text = tool_text(
        &call_tool(
            &db,
            "query_metric_range",
            &format!(r#"{{"metric":"{name}","kind":"gauge","step":"1h",{WINDOW}}}"#),
        )
        .await,
    );
    assert!(text.contains(r#""series":[{"#), "{text}");
    assert!(text.contains(r#""samples":[{"#), "{text}");

    let text = tool_text(
        &call_tool(
            &db,
            "query_metric_instant",
            &format!(r#"{{"metric":"{name}","step":"1h",{WINDOW}}}"#),
        )
        .await,
    );
    assert!(text.contains(r#""samples":["#), "{text}");

    let text =
        tool_text(&call_tool(&db, "metric_series", &format!(r#"{{"metric":"{name}"}}"#)).await);
    assert!(text.contains(r#""series_count":"#), "{text}");

    // The histogram quantile, over the explicit-bucket fixture: p95 lands in the 100..=1000 bucket.
    let text = tool_text(
        &call_tool(
            &db,
            "histogram_quantile",
            &format!(r#"{{"metric":"http.server.duration","quantile":0.95,"step":"1h",{WINDOW}}}"#),
        )
        .await,
    );
    assert!(text.contains(r#""quantile":0.95"#), "{text}");
    assert!(text.contains(r#""samples":[{"#), "{text}");

    // Attribute discovery spans every signal.
    let text = tool_text(&call_tool(&db, "list_attribute_keys", "{}").await);
    assert!(text.contains("service.name"), "{text}");
    assert!(text.contains("http.route"), "{text}");

    let text =
        tool_text(&call_tool(&db, "list_attribute_values", r#"{"key":"service.name"}"#).await);
    assert!(text.contains(r#""cart""#), "{text}");

    // Stats, which is what tells a model the real time span of the data.
    let text = tool_text(&call_tool(&db, "db_stats", "{}").await);
    assert!(text.contains(r#""table":"logs""#), "{text}");
    assert!(text.contains(r#""max_time_unix_nano""#), "{text}");

    // And SQL, capped and flagged.
    let text = tool_text(
        &call_tool(
            &db,
            "query_sql",
            r#"{"sql":"SELECT service, body FROM logs ORDER BY time","max_rows":1}"#,
        )
        .await,
    );
    assert!(text.contains(r#""row_count":1"#), "{text}");
    assert!(text.contains(r#""truncated":true"#), "{text}");
}

#[tokio::test]
async fn the_endpoint_defends_itself() {
    let db = seeded_db().await;
    let list = r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#;

    // A browser origin outside the loopback set cannot drive the tools — the DNS-rebinding defence.
    let (status, body) = post(&db, &[("origin", "https://evil.example.com")], list).await;
    assert_eq!(status, 403, "{body}");

    // A loopback origin (a local dev UI) is fine, and so is no origin at all (every real client).
    let (status, _) = post(&db, &[("origin", "http://localhost:5173")], list).await;
    assert_eq!(status, 200);

    // Malformed input is a parse error, not a panic.
    let (status, body) = legacy(&db, "{not json").await;
    assert_eq!(status, 400);
    assert!(body.contains(r#""code":-32700"#), "{body}");

    // The verbs of the older revisions' SSE stream and session teardown are refused.
    for method in ["GET", "DELETE"] {
        let response = app(db.clone())
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri("/mcp")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("router is infallible");
        assert_eq!(response.status().as_u16(), 405, "{method} /mcp");
    }
}

/// The attribute-statistics and promotion tools, end to end through the router.
///
/// These need an **on-disk** database, unlike every other case here: the measurement is defined over
/// sealed segments, and promotion is a write only a writer can serve.
#[tokio::test]
async fn the_attribute_and_promotion_tools_close_the_loop() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db: Arc<Db> = Db::builder(dir.path()).open().expect("open db");
    db.ingest_otlp_logs(&otlp_rich(
        "cart",
        "checkout failed",
        1,
        17,
        &[("http.route", "/checkout")],
    ))
    .await
    .expect("ingest");
    db.flush().await.expect("flush");

    // Measure. The default window is every sealed segment, since a `promote` list is chosen from the
    // whole corpus rather than a recent slice.
    let stats = tool_value(&call_tool(&db, "attribute_stats", "{}").await);
    assert_eq!(
        stats["range_unix_nanos"],
        Value::Null,
        "unbounded by default"
    );
    assert_eq!(stats["all_tables"]["unit"], "all_tables");
    let key = stats["all_tables"]["keys"]
        .as_array()
        .expect("keys")
        .iter()
        .find(|key| key["key"] == "http.route")
        .unwrap_or_else(|| panic!("http.route should be measured: {stats}"));
    assert_eq!(key["scope"], "attributes");
    assert_eq!(key["promoted"], json!(false));
    assert!(!key["promote"].is_null(), "the roll-up carries a verdict");
    // Per-table sections answer for themselves; only the roll-up judges promotion, which is DB-wide.
    let logs = stats["tables"]
        .as_array()
        .expect("tables")
        .iter()
        .find(|unit| unit["unit"] == "logs")
        .unwrap_or_else(|| panic!("a logs section: {stats}"));
    assert!(logs["segments"].as_u64().expect("segments") >= 1);
    for key in logs["keys"].as_array().expect("keys") {
        assert_eq!(
            key["promote"],
            Value::Null,
            "per-table rows give no verdict"
        );
    }

    // Act on it. The whole set is sent, and the answer is what is now in effect.
    let promoted = tool_value(
        &call_tool(
            &db,
            "set_promoted_attributes",
            r#"{"keys":["http.route","env"]}"#,
        )
        .await,
    );
    assert_eq!(promoted["promoted"], json!(["http.route", "env"]));
    let listed = tool_value(&call_tool(&db, "list_promoted_attributes", "{}").await);
    assert_eq!(listed, promoted, "one description of one set");
    assert_eq!(db.promote().keys(), ["http.route", "env"]);

    // And the measurement reflects it, which is the loop closing.
    let stats = tool_value(&call_tool(&db, "attribute_stats", "{}").await);
    let key = stats["all_tables"]["keys"]
        .as_array()
        .expect("keys")
        .iter()
        .find(|key| key["key"] == "http.route")
        .expect("http.route");
    assert_eq!(key["promoted"], json!(true));

    db.close().await.expect("close");
}

/// A read-only server is never *offered* the write. The tool has nothing to call there — a reader
/// holds no writer lock — so it is absent from `tools/list` rather than present and failing.
#[tokio::test]
async fn the_write_tool_is_hidden_from_a_read_only_server() {
    let dir = tempfile::tempdir().expect("tempdir");
    let writer: Arc<Db> = Db::builder(dir.path()).open().expect("open db");
    writer
        .ingest_otlp_logs(&otlp_rich("cart", "hello", 1, 9, &[]))
        .await
        .expect("ingest");
    writer.flush().await.expect("flush");

    let names = |body: &str| -> Vec<String> {
        let value: Value = serde_json::from_str(body).expect("response is JSON");
        value["result"]["tools"]
            .as_array()
            .expect("tools")
            .iter()
            .map(|tool| tool["name"].as_str().expect("name").to_owned())
            .collect()
    };

    let (_, listed) = legacy(&writer, r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#).await;
    let listed = names(&listed);
    assert!(listed.iter().any(|name| name == "attribute_stats"));
    assert!(listed.iter().any(|name| name == "set_promoted_attributes"));

    let reader = Db::open_read_only(dir.path()).expect("open read-only");
    let (_, from_reader) =
        legacy(&reader, r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#).await;
    let from_reader = names(&from_reader);
    assert!(
        from_reader.iter().any(|name| name == "attribute_stats"),
        "reads are unaffected"
    );
    assert!(
        !from_reader
            .iter()
            .any(|name| name == "set_promoted_attributes"),
        "the write is not offered where it cannot work: {from_reader:?}"
    );

    // A client working from a cached list gets a reason rather than a storage-layer error.
    let refused = call_tool(&reader, "set_promoted_attributes", r#"{"keys":[]}"#).await;
    assert!(refused.contains(r#""isError":true"#), "{refused}");
    assert!(refused.contains("read-only"), "{refused}");

    writer.close().await.expect("close");
}

/// Housekeeping over MCP: an agent submits a pass, gets a job id back, and polls it — the *same*
/// queue `POST /admin/housekeeping` uses, not a second one of its own.
///
/// Unlike every other case here this drives **one router** across several calls rather than building
/// one per call: the queue lives in the router's state, so a job submitted through one router cannot
/// be polled through another (see `imbh_server::route`, which documents the same trap).
#[tokio::test]
async fn an_agent_can_queue_housekeeping_and_poll_it() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db: Arc<Db> = Db::builder(dir.path()).open().expect("open db");
    db.ingest_otlp_logs(&otlp_rich("cart", "hello", 1, 9, &[]))
        .await
        .expect("ingest");

    // `Router` clones share their state, so every call below reaches the same queue.
    let router = app(db.clone());
    let call = async |tool: &str, arguments: &str| -> String {
        let body = format!(
            r#"{{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{{"name":"{tool}","arguments":{arguments},"_meta":{{"io.modelcontextprotocol/protocolVersion":"{MODERN}"}}}}}}"#
        );
        let request = Request::builder()
            .method("POST")
            .uri("/mcp")
            .header("content-type", "application/json")
            .header("mcp-protocol-version", MODERN)
            .header("mcp-method", "tools/call")
            .header("mcp-name", tool)
            .body(Body::from(body))
            .expect("request");
        let response = router
            .clone()
            .oneshot(request)
            .await
            .expect("router is infallible");
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read body");
        String::from_utf8(body.to_vec()).expect("utf-8 body")
    };

    // Submitting answers a handle, not an outcome.
    let submitted = tool_value(&call("run_housekeeping", "{}").await);
    let job_id = submitted["job_id"].as_str().expect("a job id").to_owned();
    assert!(!job_id.is_empty());
    assert_eq!(submitted["coalesced"], json!(false));
    assert!(
        ["queued", "running"].contains(&submitted["state"].as_str().expect("state")),
        "the work has not finished at submission time: {submitted}"
    );

    // The same request while that one is still queued joins it rather than queueing a second pass.
    let again = tool_value(&call("run_housekeeping", "{}").await);
    if again["coalesced"] == json!(true) {
        assert_eq!(again["job_id"], job_id.as_str());
    }

    // Poll to a terminal state through the status tool.
    let mut job = Value::Null;
    for _ in 0..400 {
        job = tool_value(
            &call(
                "housekeeping_status",
                &format!(r#"{{"job_id":"{job_id}"}}"#),
            )
            .await,
        );
        if ["succeeded", "failed"].contains(&job["state"].as_str().expect("state")) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(job["state"], "succeeded", "{job}");
    assert!(job["report"]["sealed"].is_boolean(), "{job}");

    // No id lists the recent jobs instead.
    let listed = tool_value(&call("housekeeping_status", "{}").await);
    assert!(
        listed["jobs"]
            .as_array()
            .expect("jobs")
            .iter()
            .any(|job| job["job_id"] == job_id.as_str()),
        "{listed}"
    );

    // An unknown id says why rather than answering an empty job.
    let missing = call("housekeeping_status", r#"{"job_id":"nope-1"}"#).await;
    assert!(missing.contains(r#""isError":true"#), "{missing}");
    assert!(missing.contains("restart"), "{missing}");

    // Zero is refused, as it is over HTTP: "compact nothing" is what `compact: false` says.
    let refused = call("run_housekeeping", r#"{"compact":true,"max_jobs":0}"#).await;
    assert!(refused.contains(r#""isError":true"#), "{refused}");
    assert!(refused.contains("positive"), "{refused}");

    db.close().await.expect("close");
}

/// The housekeeping tools are offered only by a host that runs a queue. `imbh-tui --mcp-stdio` runs
/// none, and `imbh_mcp::handle` — the entry point without one — must not advertise what it cannot do.
#[tokio::test]
async fn a_host_with_no_queue_does_not_offer_the_housekeeping_tools() {
    let db: Arc<Db> = Db::in_memory().open().expect("open in-memory db");
    let reply = imbh_mcp::handle(
        &db,
        br#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,
        &imbh_mcp::Transport::Http(imbh_mcp::Headers {
            protocol_version: None,
            method: None,
            name: None,
        }),
    )
    .await;
    let body = reply.body.expect("a result body");
    let names: Vec<&str> = body["result"]["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .map(|tool| tool["name"].as_str().expect("name"))
        .collect();
    assert!(names.contains(&"db_stats"), "the reads are unaffected");
    assert!(
        !names.contains(&"run_housekeeping") && !names.contains(&"housekeeping_status"),
        "no queue, so no tools that need one: {names:?}"
    );
}

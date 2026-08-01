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
use imbh::{AnyValue, Db, parse_json};
use imbh_server::app;
use imbh_test_support::otlp::{
    otlp_hist, otlp_metrics, otlp_rich, otlp_trace_tree, otlp_trace_wide,
};
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
    let value = parse_json(response).expect("response is JSON");
    let field = |v: &AnyValue, name: &str| match v {
        AnyValue::Map(pairs) => pairs
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone()),
        _ => None,
    };
    let result = field(&value, "result").expect("a result, not an error");
    assert_eq!(
        field(&result, "isError"),
        Some(AnyValue::Bool(false)),
        "tool reported an error: {response}"
    );
    let Some(AnyValue::Array(content)) = field(&result, "content") else {
        panic!("no content array in {response}");
    };
    let Some(AnyValue::Str(text)) = field(&content[0], "text") else {
        panic!("no text block in {response}");
    };
    // Every tool answers with a JSON document, so a client can parse the block rather than read it.
    assert!(parse_json(&text).is_some(), "tool text is not JSON: {text}");
    text
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
    // silently producing `{"service.name": ""}` for every series: a group key is looked up in the
    // record's `attributes`, and `service.name` is never there (it is the promoted `service` column).
    assert!(text.contains(r#""http.method":"GET""#), "{text}");
    // The spans that *lack* the key group under "" — correct SQL semantics for a missing attribute,
    // and the reason a model must be told which keys are groupable rather than guessing.
    assert!(text.contains(r#""http.method":"""#), "{text}");
    // And the trap itself: grouping by `service.name` yields one empty-labelled series, never a
    // per-service breakdown. Pinned so a library-side fix shows up here as a failing test.
    let text = tool_text(
        &call_tool(
            &db,
            "span_metrics",
            &format!(r#"{{{WINDOW},"step":"1h","group_by":["service.name"]}}"#),
        )
        .await,
    );
    assert!(text.contains(r#""service.name":"""#), "{text}");
    assert!(!text.contains(r#""service.name":"cart""#), "{text}");
}

#[tokio::test]
async fn the_metric_and_discovery_tools_answer_over_real_data() {
    let db = seeded_db().await;

    let text = tool_text(&call_tool(&db, "list_metrics", "{}").await);
    assert!(text.contains(r#""metric":"#), "{text}");
    assert!(text.contains(r#""kind":"#), "{text}");

    // Pull a real metric name out of the catalogue and query it, the way a model would.
    let catalog = parse_json(&text).expect("catalogue is JSON");
    let AnyValue::Map(pairs) = catalog else {
        panic!("catalogue is not an object")
    };
    let Some((_, AnyValue::Array(metrics))) = pairs.into_iter().find(|(k, _)| k == "metrics")
    else {
        panic!("no metrics array")
    };
    let name = metrics
        .iter()
        .find_map(|m| match m {
            AnyValue::Map(fields) => {
                let get = |k: &str| {
                    fields
                        .iter()
                        .find(|(key, _)| key == k)
                        .and_then(|(_, v)| v.as_str())
                };
                (get("kind") == Some("gauge")).then(|| get("metric").unwrap_or_default().to_owned())
            }
            _ => None,
        })
        .expect("a gauge metric in the fixture");

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

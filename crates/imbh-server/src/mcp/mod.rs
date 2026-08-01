//! The MCP (Model Context Protocol) endpoint: imbh's telemetry as agent tools.
//!
//! `imbhd` serves MCP over the Streamable HTTP transport at `POST /mcp`, so an agent can search
//! logs, pull traces, and query metrics through the same process that ingests them — no Grafana, no
//! datasource proxy, and no second copy of the data. The tool surface is [`tools`]: read-only
//! wrappers over the imbh library's typed query APIs plus raw SQL.
//!
//! # Two protocol eras
//!
//! MCP revision `2026-07-28` made the protocol **stateless**: there is no `initialize` handshake,
//! every request carries its own version in `params._meta`, and `server/discover` reports what the
//! server supports. Revisions `2025-11-25` and earlier ("legacy") open with `initialize` instead.
//! This endpoint is **dual-era** — it answers both, choosing per request:
//!
//! - a request whose `params._meta` carries `io.modelcontextprotocol/protocolVersion`, or a
//!   `server/discover` call, is served as modern: HTTP header/body agreement is enforced, the
//!   version must be one we implement, and results carry `resultType: "complete"`;
//! - anything else is served as legacy, starting from `initialize`.
//!
//! That matters because the clients in the field today are legacy-era, while new ones are moving to
//! the stateless revision; a server that picked one would be unusable by half of them.
//!
//! # What is deliberately not here
//!
//! No SSE: every response is a single `application/json` body, which the transport explicitly
//! permits, and nothing this server does streams. Consequently `GET`/`DELETE /mcp` answer `405`, as
//! the spec prescribes for a server that offers no stream and no session. No resources, no prompts,
//! no sampling. No sessions: `Mcp-Session-Id` is neither minted nor required, so a client may fire
//! `tools/call` at a cold server without any handshake at all.
//!
//! # Exposure
//!
//! The endpoint is unauthenticated, like the rest of `imbhd` (ARCHITECTURE.md §10.16) — a real
//! deployment gates it. What it *does* enforce is the transport's DNS-rebinding defence: a browser
//! `Origin` outside the loopback set is refused with `403` unless `IMBH_MCP_ALLOWED_ORIGINS` says
//! otherwise. Keep `imbhd` bound to `127.0.0.1` when an agent on the same machine is the only
//! client.

pub(crate) mod json;
pub(crate) mod tools;

use std::sync::Arc;

use imbh::{AnyValue, Db, parse_json};

use json::{Args, Arr, Obj, any_value, decode_header_value};

/// The stateless revision this server implements. Modern requests must name exactly this.
pub const LATEST_VERSION: &str = "2026-07-28";

/// Handshake-era revisions this server answers `initialize` for, newest first. A legacy client
/// asking for something outside this list gets the newest one back, per the lifecycle's negotiation
/// rule ("respond with another protocol version it supports").
pub const LEGACY_VERSIONS: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26"];

/// The server name reported to clients. `serverInfo` is display/logging metadata, not identity.
const SERVER_NAME: &str = "imbhd";

/// Orientation handed to the model once, so it does not have to discover the shape of the data by
/// trial and error.
const INSTRUCTIONS: &str = "\
imbh is an embedded observability database holding OpenTelemetry logs, traces, and metrics for one \
process or host. Start with `db_stats` to see which signals hold data and over what time span, and \
`list_attribute_values` on `service.name` to see which services report. Prefer the typed tools \
(`search_logs`, `search_traces`, `span_metrics`, `query_metric_range`) over `query_sql`: they use \
the time and full-text indexes, while raw SQL scans. Timestamps are epoch nanoseconds throughout; \
every query tool takes a `since` window (default 1h) or explicit `start_unix_nano`/`end_unix_nano` \
bounds. Data is immutable and bounded by retention — nothing here can modify it.";

// JSON-RPC and MCP error codes. The negative sub-range below -32000 is the MCP-allocated one.
const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;
const HEADER_MISMATCH: i64 = -32020;
const UNSUPPORTED_PROTOCOL_VERSION: i64 = -32022;

/// The `_meta` key a modern request carries its protocol version in.
const META_VERSION: &str = "io.modelcontextprotocol/protocolVersion";

/// The Streamable HTTP request headers this endpoint validates against the body.
#[derive(Default)]
pub(crate) struct Headers<'a> {
    /// `MCP-Protocol-Version`.
    pub(crate) protocol_version: Option<&'a str>,
    /// `Mcp-Method`.
    pub(crate) method: Option<&'a str>,
    /// `Mcp-Name` — the tool name on a `tools/call`.
    pub(crate) name: Option<&'a str>,
}

/// One MCP message's answer: a JSON-RPC body, or nothing at all for an accepted notification.
pub(crate) struct Reply {
    pub(crate) status: u16,
    pub(crate) body: Option<String>,
}

impl Reply {
    fn json(status: u16, body: String) -> Self {
        Reply {
            status,
            body: Some(body),
        }
    }

    /// `202 Accepted`, no body — the transport's required answer to a notification.
    fn accepted() -> Self {
        Reply {
            status: 202,
            body: None,
        }
    }
}

/// Handle one JSON-RPC message from the MCP endpoint.
///
/// Transport-agnostic on purpose: it takes bytes and the (already extracted) header values and
/// returns bytes, so the same dispatch can sit behind a stdio loop later without moving any of the
/// protocol logic.
pub(crate) async fn handle(db: &Arc<Db>, body: &[u8], headers: &Headers<'_>) -> Reply {
    let Ok(text) = std::str::from_utf8(body) else {
        return Reply::json(
            400,
            error_body(None, PARSE_ERROR, "request body is not UTF-8", None),
        );
    };
    let Some(AnyValue::Map(message)) = parse_json(text) else {
        return Reply::json(
            400,
            error_body(None, PARSE_ERROR, "request body is not a JSON object", None),
        );
    };

    let field = |name: &str| {
        message
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v)
            .filter(|v| !matches!(v, AnyValue::Null))
    };
    // A message with no usable id is a notification: it gets 202 and no body, whatever it asked for.
    let id = field("id").map(any_value);
    let id = id.as_deref();
    let Some(AnyValue::Str(method)) = field("method") else {
        return match id {
            Some(_) => Reply::json(
                400,
                error_body(id, INVALID_REQUEST, "missing `method`", None),
            ),
            None => Reply::accepted(),
        };
    };
    let params = field("params");
    let param = |name: &str| match params {
        Some(AnyValue::Map(pairs)) => pairs
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v)
            .filter(|v| !matches!(v, AnyValue::Null)),
        _ => None,
    };

    // The version a modern client declares in `_meta`; its presence is what selects the era.
    let declared = match param("_meta") {
        Some(AnyValue::Map(meta)) => meta
            .iter()
            .find(|(k, _)| k == META_VERSION)
            .and_then(|(_, v)| v.as_str()),
        _ => None,
    };
    let modern = declared.is_some() || method == "server/discover";

    if id.is_none() {
        // Notifications carry no reply. `notifications/initialized` is the legacy handshake's third
        // leg and is the one that actually shows up here.
        return Reply::accepted();
    }
    let id = id.expect("checked just above");

    if modern && let Some(reply) = validate_modern(id, method, declared, param("name"), headers) {
        return reply;
    }

    match method.as_str() {
        // ── modern ──────────────────────────────────────────────────────────────────────────────
        "server/discover" => Reply::json(200, result_body(id, &discover())),

        // ── legacy handshake ────────────────────────────────────────────────────────────────────
        "initialize" if !modern => {
            let requested = param("protocolVersion").and_then(|v| v.as_str());
            Reply::json(200, result_body(id, &initialize(requested)))
        }

        // ── both eras ───────────────────────────────────────────────────────────────────────────
        "ping" => Reply::json(200, result_body(id, &new_result(modern).finish())),
        "tools/list" => {
            let mut list = Arr::new();
            for tool in tools::TOOLS {
                list.raw(
                    &Obj::new()
                        .str("name", tool.name)
                        .str("title", tool.title)
                        .str("description", tool.description)
                        .raw("inputSchema", tool.input_schema)
                        .finish(),
                );
            }
            let result = new_result(modern).raw("tools", &list.finish()).finish();
            Reply::json(200, result_body(id, &result))
        }
        "tools/call" => {
            let Some(AnyValue::Str(name)) = param("name") else {
                return Reply::json(
                    200,
                    error_body(
                        Some(id),
                        INVALID_PARAMS,
                        "missing tool name in `params.name`",
                        None,
                    ),
                );
            };
            let args = Args::new(param("arguments"));
            match tools::call(db, name, &args).await {
                // An unknown tool is a protocol error: no argument the model could pick would fix it.
                None => Reply::json(
                    200,
                    error_body(
                        Some(id),
                        INVALID_PARAMS,
                        &format!("Unknown tool: {name}"),
                        None,
                    ),
                ),
                Some(outcome) => {
                    let (text, is_error) = match outcome {
                        Ok(json) => (json, false),
                        Err(message) => (message, true),
                    };
                    let content = Arr::new()
                        .raw(&Obj::new().str("type", "text").str("text", &text).finish())
                        .finish();
                    let result = new_result(modern)
                        .raw("content", &content)
                        .bool("isError", is_error)
                        .finish();
                    Reply::json(200, result_body(id, &result))
                }
            }
        }

        // A modern client is told `404` for an unknown method so it can tell this apart from a
        // legacy server that does not host the endpoint at all; a legacy one gets the plain
        // JSON-RPC error.
        other => Reply::json(
            if modern { 404 } else { 200 },
            error_body(
                Some(id),
                METHOD_NOT_FOUND,
                &format!("Method not found: {other}"),
                None,
            ),
        ),
    }
}

/// Enforce the modern transport's header/body agreement and version support. `Some` is the refusal.
fn validate_modern(
    id: &str,
    method: &str,
    declared: Option<&str>,
    name: Option<&AnyValue>,
    headers: &Headers<'_>,
) -> Option<Reply> {
    let mismatch = |message: String| {
        Some(Reply::json(
            400,
            error_body(Some(id), HEADER_MISMATCH, &message, None),
        ))
    };

    // `server/discover` is how a client probes an unknown server, so it is exempt from the
    // header-agreement rules it cannot yet know it needs; everything else must comply.
    if method != "server/discover" {
        let Some(header_version) = headers.protocol_version else {
            return mismatch("missing required `MCP-Protocol-Version` header".to_owned());
        };
        if Some(header_version) != declared {
            return mismatch(format!(
                "header mismatch: `MCP-Protocol-Version: {header_version}` does not match the request body's {META_VERSION}"
            ));
        }
        match headers.method {
            None => return mismatch("missing required `Mcp-Method` header".to_owned()),
            Some(m) if m != method => {
                return mismatch(format!(
                    "header mismatch: `Mcp-Method: {m}` does not match the request body's method `{method}`"
                ));
            }
            Some(_) => {}
        }
        if method == "tools/call" {
            let body_name = name.and_then(|v| v.as_str()).unwrap_or_default();
            let header_name = headers.name.map(decode_header_value);
            match header_name {
                None => return mismatch("missing required `Mcp-Name` header".to_owned()),
                Some(None) => {
                    return mismatch("`Mcp-Name` header is not a valid Base64 sentinel".to_owned());
                }
                Some(Some(n)) if n != body_name => {
                    return mismatch(format!(
                        "header mismatch: `Mcp-Name: {n}` does not match the request body's tool name `{body_name}`"
                    ));
                }
                Some(Some(_)) => {}
            }
        }
    }

    // Version support is checked after the headers agree, so the version reported back is the one
    // the client actually meant.
    let requested = declared.or(headers.protocol_version);
    match requested {
        Some(v) if v == LATEST_VERSION => None,
        // A `server/discover` with no declared version is the legitimate "what do you speak?" probe.
        None if method == "server/discover" => None,
        _ => {
            let data = Obj::new()
                .raw("supported", &Arr::new().str(LATEST_VERSION).finish())
                .opt_str("requested", requested)
                .finish();
            Some(Reply::json(
                400,
                error_body(
                    Some(id),
                    UNSUPPORTED_PROTOCOL_VERSION,
                    "Unsupported protocol version",
                    Some(&data),
                ),
            ))
        }
    }
}

/// `server/discover` — the modern era's identity/capability probe.
fn discover() -> String {
    let meta = Obj::new()
        .raw("io.modelcontextprotocol/serverInfo", &server_info())
        .finish();
    new_result(true)
        .raw(
            "supportedVersions",
            &Arr::new().str(LATEST_VERSION).finish(),
        )
        .raw("capabilities", &Obj::new().raw("tools", "{}").finish())
        .raw("_meta", &meta)
        .str("instructions", INSTRUCTIONS)
        .finish()
}

/// `initialize` — the legacy era's handshake. An unknown requested version is answered with the
/// newest legacy revision rather than refused, which is what the lifecycle asks for.
fn initialize(requested: Option<&str>) -> String {
    let negotiated = match requested {
        Some(v) if LEGACY_VERSIONS.contains(&v) => v,
        _ => LEGACY_VERSIONS[0],
    };
    let capabilities = Obj::new()
        .raw(
            "tools",
            // The tool table is a compile-time constant, so it never changes under a client.
            &Obj::new().bool("listChanged", false).finish(),
        )
        .finish();
    Obj::new()
        .str("protocolVersion", negotiated)
        .raw("capabilities", &capabilities)
        .raw("serverInfo", &server_info())
        .str("instructions", INSTRUCTIONS)
        .finish()
}

fn server_info() -> String {
    Obj::new()
        .str("name", SERVER_NAME)
        .str("title", "imbh embedded observability database")
        .str("version", env!("CARGO_PKG_VERSION"))
        .finish()
}

/// Start a result object, tagged with `resultType` for the modern era (where every result carries
/// one) and untagged for the legacy era (where the field does not exist).
fn new_result(modern: bool) -> Obj {
    let mut obj = Obj::new();
    if modern {
        obj.str("resultType", "complete");
    }
    obj
}

fn result_body(id: &str, result: &str) -> String {
    Obj::new()
        .str("jsonrpc", "2.0")
        .raw("id", id)
        .raw("result", result)
        .finish()
}

/// A JSON-RPC error response. `id` is `None` only where the request's id could not be read, which
/// JSON-RPC spells as a null id.
fn error_body(id: Option<&str>, code: i64, message: &str, data: Option<&str>) -> String {
    let mut error = Obj::new();
    error.int("code", code).str("message", message);
    if let Some(data) = data {
        error.raw("data", data);
    }
    Obj::new()
        .str("jsonrpc", "2.0")
        .raw("id", id.unwrap_or("null"))
        .raw("error", &error.finish())
        .finish()
}

// ── DNS-rebinding defence ───────────────────────────────────────────────────────────────────────

/// Whether a browser `Origin` may reach the MCP endpoint.
///
/// A browser attaches `Origin` to cross-site requests, so this is the check that stops a web page
/// the user is merely *visiting* from driving the tools on their loopback `imbhd` (the DNS-rebinding
/// attack the transport spec calls out). Non-browser clients — the agents this endpoint is actually
/// for — send no `Origin` and are unaffected.
///
/// With no configured allowlist, only loopback origins pass. `allowed` entries are matched
/// case-insensitively in full (`https://app.example.com`), and the single entry `*` disables the
/// check.
pub fn origin_allowed(origin: &str, allowed: &[String]) -> bool {
    if allowed.iter().any(|a| a == "*") {
        return true;
    }
    if allowed
        .iter()
        .any(|a| a.eq_ignore_ascii_case(origin.trim()))
    {
        return true;
    }
    // `Origin: null` (a sandboxed iframe or a `file://` page) is opaque by design — it names no
    // host, so it can never be recognized as loopback.
    matches!(origin_host(origin), Some(host) if is_loopback(&host))
}

/// The host of an `Origin` (`scheme://host[:port]`), lowercased and with any IPv6 brackets kept.
fn origin_host(origin: &str) -> Option<String> {
    let rest = origin.trim().split_once("://")?.1;
    // Strip the port, taking care not to cut inside an IPv6 literal's brackets.
    let host = match rest.strip_prefix('[') {
        Some(v6) => &rest[..v6.find(']')? + 2],
        None => rest.split(':').next()?,
    };
    (!host.is_empty()).then(|| host.to_ascii_lowercase())
}

fn is_loopback(host: &str) -> bool {
    matches!(host, "localhost" | "127.0.0.1" | "[::1]")
        || host.strip_prefix("127.").is_some_and(|rest| {
            rest.split('.').count() == 3 && rest.split('.').all(|o| o.parse::<u8>().is_ok())
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_origins_pass_and_others_do_not() {
        let none: &[String] = &[];
        for ok in [
            "http://localhost:5173",
            "http://127.0.0.1:8080",
            "https://LOCALHOST",
            "http://[::1]:3000",
            "http://127.10.0.9:1",
        ] {
            assert!(origin_allowed(ok, none), "{ok} should be allowed");
        }
        for bad in [
            "https://evil.example.com",
            "http://localhost.evil.com",
            "null",
            "http://127.0.0.1.evil.com",
            "http://[::2]:3000",
        ] {
            assert!(!origin_allowed(bad, none), "{bad} should be refused");
        }
    }

    #[test]
    fn an_allowlist_extends_and_a_star_disables_the_check() {
        let allowed = vec!["https://App.Example.com".to_owned()];
        assert!(origin_allowed("https://app.example.com", &allowed));
        assert!(origin_allowed("http://localhost:1", &allowed));
        assert!(!origin_allowed("https://other.example.com", &allowed));

        let star = vec!["*".to_owned()];
        assert!(origin_allowed("https://anything.example.com", &star));
        assert!(origin_allowed("null", &star));
    }

    #[test]
    fn legacy_initialize_negotiates_a_version() {
        assert!(initialize(Some("2025-06-18")).contains(r#""protocolVersion":"2025-06-18""#));
        // Unknown (or absent) → the newest legacy revision, never the stateless one, which a
        // handshake-era client could not speak.
        assert!(initialize(Some("1999-01-01")).contains(r#""protocolVersion":"2025-11-25""#));
        assert!(initialize(None).contains(r#""protocolVersion":"2025-11-25""#));
        assert!(!initialize(None).contains(LATEST_VERSION));
    }

    #[test]
    fn discovery_reports_the_stateless_revision_and_tools() {
        let d = discover();
        assert!(d.contains(r#""resultType":"complete""#));
        assert!(d.contains(&format!(r#""supportedVersions":["{LATEST_VERSION}"]"#)));
        assert!(d.contains(r#""capabilities":{"tools":{}}"#));
        assert!(d.contains("io.modelcontextprotocol/serverInfo"));
    }
}

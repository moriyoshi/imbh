//! The MCP (Model Context Protocol) server: imbh's telemetry as agent tools.
//!
//! An agent connected here can search logs, pull traces, and query metrics through the same process
//! that holds them — no Grafana, no datasource proxy, and no second copy of the data. The tool
//! surface is read-only wrappers over the imbh library's typed query APIs plus raw SQL.
//!
//! # Transports
//!
//! [`handle`] is the whole protocol, and it is transport-agnostic: bytes plus a [`Transport`] in, a
//! [`Reply`] out. Two transports wire into it, and they are the two MCP defines:
//!
//! - **Streamable HTTP** — `imbhd` serves it at `POST /mcp` (`imbh-server`), which is where
//!   [`Transport::Http`]'s header/body agreement rules apply.
//! - **stdio** — [`stdio::serve`], newline-delimited JSON-RPC over a pipe, hosted by the `imbh-tui`
//!   binary (`imbh-tui --mcp-stdio`). stdio is what clients "SHOULD support whenever possible", it
//!   needs no listening port, and it is the transport an agent launching a subprocess speaks. Its
//!   framing carries no headers, so [`Transport::Stdio`] validates none.
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
//! Over HTTP the endpoint is unauthenticated, like the rest of `imbhd` (ARCHITECTURE.md §10.16) — a
//! real deployment gates it. What it *does* enforce is the transport's DNS-rebinding defence: a
//! browser `Origin` outside the loopback set is refused with `403` unless `IMBH_MCP_ALLOWED_ORIGINS`
//! says otherwise ([`origin_allowed`]). Keep `imbhd` bound to `127.0.0.1` when an agent on the same
//! machine is the only client. Over stdio the question does not arise: the pipe *is* the
//! authorization — whoever spawned the process is the only one who can talk to it.

pub(crate) mod json;
pub(crate) mod tools;

pub mod proxy;
mod render;
pub mod stdio;

use std::future::Future;
use std::sync::Arc;

use imbh::Db;
use serde_json::{Value, json};

use json::{Args, decode_header_value};

pub use render::{batches_to_json, json_string, stats_json};

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
pub struct Headers<'a> {
    /// `MCP-Protocol-Version`.
    pub protocol_version: Option<&'a str>,
    /// `Mcp-Method`.
    pub method: Option<&'a str>,
    /// `Mcp-Name` — the tool name on a `tools/call`.
    pub name: Option<&'a str>,
}

/// Which MCP transport a message arrived over.
///
/// The difference is entirely about *headers*. The stateless revision's Streamable HTTP transport
/// mirrors the request's method, protocol version, and tool name into HTTP headers and requires the
/// two to agree, so a proxy can route without parsing the body. stdio frames messages as bare lines
/// with nowhere to put a header — those rules are the HTTP transport's, not the protocol's, so over
/// stdio there is nothing to check and a modern request is served on its `_meta` alone.
pub enum Transport<'a> {
    /// Streamable HTTP: header/body agreement is enforced.
    Http(Headers<'a>),
    /// stdio: newline-delimited JSON with no header channel.
    Stdio,
}

impl<'a> Transport<'a> {
    /// The headers to validate against, or `None` where the transport has no header channel.
    fn headers(&self) -> Option<&Headers<'a>> {
        match self {
            Transport::Http(headers) => Some(headers),
            Transport::Stdio => None,
        }
    }
}

/// One MCP message's answer: a JSON-RPC body, or nothing at all for an accepted notification.
///
/// The body stays a [`Value`] rather than bytes so the transport owns serialization — which is what
/// lets the stdio loop reuse this dispatch unchanged. `status` is the HTTP status the Streamable
/// HTTP transport should answer with; stdio ignores it, since a pipe has no status line.
pub struct Reply {
    pub status: u16,
    pub body: Option<Value>,
}

impl Reply {
    fn json(status: u16, body: Value) -> Self {
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
/// Transport-agnostic on purpose: it takes the message bytes plus which [`Transport`] they arrived
/// over (with the already-extracted header values, for HTTP) and returns a [`Reply`] the caller
/// serializes, so the HTTP endpoint and the stdio loop share every line of protocol logic.
pub async fn handle(db: &Arc<Db>, body: &[u8], transport: &Transport<'_>) -> Reply {
    // `from_slice` covers invalid UTF-8 too, so there is one parse-failure path rather than two.
    let message: Value = match serde_json::from_slice(body) {
        Ok(message) => message,
        Err(e) => {
            return Reply::json(
                400,
                error_body(None, PARSE_ERROR, &format!("request body is not JSON: {e}")),
            );
        }
    };
    let Some(message) = message.as_object() else {
        return Reply::json(
            400,
            error_body(
                None,
                INVALID_REQUEST,
                "request body is not a JSON-RPC object",
            ),
        );
    };

    let field = |name: &str| message.get(name).filter(|v| !v.is_null());
    // A message with no usable id is a notification: it gets 202 and no body, whatever it asked for.
    let id = field("id").cloned();
    let Some(method) = field("method").and_then(Value::as_str) else {
        return match id {
            Some(id) => Reply::json(
                400,
                error_body(Some(id), INVALID_REQUEST, "missing `method`"),
            ),
            None => Reply::accepted(),
        };
    };
    let params = field("params");
    let param = |name: &str| {
        params
            .and_then(Value::as_object)
            .and_then(|p| p.get(name))
            .filter(|v| !v.is_null())
    };

    // The version a modern client declares in `_meta`; its presence is what selects the era.
    let declared = param("_meta")
        .and_then(|meta| meta.get(META_VERSION))
        .and_then(Value::as_str);
    let modern = declared.is_some() || method == "server/discover";

    let Some(id) = id else {
        // Notifications carry no reply. `notifications/initialized` is the legacy handshake's third
        // leg and is the one that actually shows up here.
        return Reply::accepted();
    };

    if modern && let Some(reply) = validate_modern(&id, method, declared, param("name"), transport)
    {
        return reply;
    }

    match method {
        // ── modern ──────────────────────────────────────────────────────────────────────────────
        "server/discover" => Reply::json(200, result_body(id, discover())),

        // ── legacy handshake ────────────────────────────────────────────────────────────────────
        "initialize" if !modern => {
            let requested = param("protocolVersion").and_then(Value::as_str);
            Reply::json(200, result_body(id, initialize(requested)))
        }

        // ── both eras ───────────────────────────────────────────────────────────────────────────
        "ping" => Reply::json(200, result_body(id, tag(modern, json!({})))),
        "tools/list" => {
            let list: Vec<Value> = tools::TOOLS
                .iter()
                .map(|tool| {
                    json!({
                        "name": tool.name,
                        "title": tool.title,
                        "description": tool.description,
                        "inputSchema": tool.schema(),
                    })
                })
                .collect();
            Reply::json(200, result_body(id, tag(modern, json!({ "tools": list }))))
        }
        "tools/call" => {
            let Some(name) = param("name").and_then(Value::as_str) else {
                return Reply::json(
                    200,
                    error_body(
                        Some(id),
                        INVALID_PARAMS,
                        "missing tool name in `params.name`",
                    ),
                );
            };
            let args = Args::new(param("arguments"));
            match tools::call(db, name, &args).await {
                // An unknown tool is a protocol error: no argument the model could pick would fix it.
                None => Reply::json(
                    200,
                    error_body(Some(id), INVALID_PARAMS, &format!("Unknown tool: {name}")),
                ),
                Some(outcome) => {
                    // A tool answers with one JSON document in a text block; a failure puts the
                    // message a model can act on in that same block and flags `isError`.
                    let (text, is_error) = match outcome {
                        Ok(value) => (value.to_string(), false),
                        Err(message) => (message, true),
                    };
                    let result = json!({
                        "content": [{"type": "text", "text": text}],
                        "isError": is_error,
                    });
                    Reply::json(200, result_body(id, tag(modern, result)))
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
            ),
        ),
    }
}

/// Enforce the modern transport's header/body agreement and version support. `Some` is the refusal.
fn validate_modern(
    id: &Value,
    method: &str,
    declared: Option<&str>,
    name: Option<&Value>,
    transport: &Transport<'_>,
) -> Option<Reply> {
    let mismatch = |message: String| {
        Some(Reply::json(
            400,
            error_body(Some(id.clone()), HEADER_MISMATCH, &message),
        ))
    };

    // The header mirror is the Streamable HTTP transport's rule, so stdio skips this block whole.
    // `server/discover` is how a client probes an unknown server, so it is exempt too — it cannot
    // yet know the rules it would have to comply with; everything else must.
    if let Some(headers) = transport.headers()
        && method != "server/discover"
    {
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
            let body_name = name.and_then(Value::as_str).unwrap_or_default();
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
    let requested = declared.or_else(|| transport.headers().and_then(|h| h.protocol_version));
    match requested {
        Some(v) if v == LATEST_VERSION => None,
        // A `server/discover` with no declared version is the legitimate "what do you speak?" probe.
        None if method == "server/discover" => None,
        _ => Some(Reply::json(
            400,
            error_with_data(
                Some(id.clone()),
                UNSUPPORTED_PROTOCOL_VERSION,
                "Unsupported protocol version",
                json!({"supported": [LATEST_VERSION], "requested": requested}),
            ),
        )),
    }
}

/// `server/discover` — the modern era's identity/capability probe.
fn discover() -> Value {
    tag(
        true,
        json!({
            "supportedVersions": [LATEST_VERSION],
            "capabilities": {"tools": {}},
            "_meta": {"io.modelcontextprotocol/serverInfo": server_info()},
            "instructions": INSTRUCTIONS,
        }),
    )
}

/// `initialize` — the legacy era's handshake. An unknown requested version is answered with the
/// newest legacy revision rather than refused, which is what the lifecycle asks for.
fn initialize(requested: Option<&str>) -> Value {
    let negotiated = match requested {
        Some(v) if LEGACY_VERSIONS.contains(&v) => v,
        _ => LEGACY_VERSIONS[0],
    };
    json!({
        "protocolVersion": negotiated,
        // The tool table is a compile-time constant, so it never changes under a client.
        "capabilities": {"tools": {"listChanged": false}},
        "serverInfo": server_info(),
        "instructions": INSTRUCTIONS,
    })
}

fn server_info() -> Value {
    json!({
        "name": SERVER_NAME,
        "title": "imbh embedded observability database",
        "version": env!("CARGO_PKG_VERSION"),
    })
}

/// Tag a result with `resultType` for the modern era (where every result carries one) and leave it
/// untagged for the legacy era (where the field does not exist).
fn tag(modern: bool, mut result: Value) -> Value {
    if modern && let Some(object) = result.as_object_mut() {
        object.insert("resultType".to_owned(), json!("complete"));
    }
    result
}

fn result_body(id: Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

/// A JSON-RPC error response. `id` is `None` only where the request's id could not be read, which
/// JSON-RPC spells as a null id.
fn error_body(id: Option<Value>, code: i64, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id.unwrap_or(Value::Null),
        "error": {"code": code, "message": message},
    })
}

/// [`error_body`] with the `data` payload the MCP-defined errors carry.
fn error_with_data(id: Option<Value>, code: i64, message: &str, data: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id.unwrap_or(Value::Null),
        "error": {"code": code, "message": message, "data": data},
    })
}

// ── running blocking `Db` work ──────────────────────────────────────────────────────────────────

/// Run a `Db` future to completion without leaving a runtime worker parked on its blocking I/O.
///
/// `Db`'s futures block *inside themselves* — parquet and tantivy reads are synchronous and the
/// library has no `spawn_blocking` anywhere — so awaiting one on a multi-threaded runtime's worker
/// stops that worker serving anything else. `block_in_place` hands this worker's queue to a
/// replacement for the duration, which is the contract we want and costs no `Send + 'static` bound:
/// the future goes on borrowing the `Db` and the request arguments.
///
/// On a current-thread runtime it simply awaits. Nothing else shares that thread to starve, which is
/// the situation in `#[tokio::test]`, in an embedder driving the dispatch by hand, and in the stdio
/// transport — where the loop is strictly one message at a time anyway.
pub async fn offload<F: Future>(fut: F) -> F::Output {
    let multi_thread = matches!(
        tokio::runtime::Handle::try_current().map(|handle| handle.runtime_flavor()),
        Ok(tokio::runtime::RuntimeFlavor::MultiThread)
    );
    if multi_thread {
        tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(fut))
    } else {
        fut.await
    }
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
        let version = |v: Option<&str>| initialize(v)["protocolVersion"].clone();
        assert_eq!(version(Some("2025-06-18")), json!("2025-06-18"));
        // Unknown (or absent) → the newest legacy revision, never the stateless one, which a
        // handshake-era client could not speak.
        assert_eq!(version(Some("1999-01-01")), json!("2025-11-25"));
        assert_eq!(version(None), json!("2025-11-25"));
        assert_ne!(version(None), json!(LATEST_VERSION));
        // The handshake result carries no `resultType`: that field exists only in the modern era.
        assert!(initialize(None).get("resultType").is_none());
        assert_eq!(
            initialize(None)["capabilities"],
            json!({"tools": {"listChanged": false}})
        );
    }

    #[test]
    fn discovery_reports_the_stateless_revision_and_tools() {
        let d = discover();
        assert_eq!(d["resultType"], json!("complete"));
        assert_eq!(d["supportedVersions"], json!([LATEST_VERSION]));
        assert_eq!(d["capabilities"], json!({"tools": {}}));
        assert_eq!(
            d["_meta"]["io.modelcontextprotocol/serverInfo"]["name"],
            json!(SERVER_NAME)
        );
    }

    #[test]
    fn the_modern_tag_is_added_only_for_the_stateless_era() {
        assert_eq!(tag(true, json!({})), json!({"resultType": "complete"}));
        assert_eq!(tag(false, json!({})), json!({}));
        // A non-object result (the protocol has none today) is passed through rather than mangled.
        assert_eq!(tag(true, json!([1])), json!([1]));
    }

    #[test]
    fn error_bodies_echo_the_id_verbatim() {
        // JSON-RPC ids may be numbers or strings, and a client matches on the exact value it sent.
        assert_eq!(error_body(Some(json!(7)), -32601, "nope")["id"], json!(7));
        assert_eq!(
            error_body(Some(json!("d1")), -32601, "nope")["id"],
            json!("d1")
        );
        // An unreadable id is spelled null, which is what JSON-RPC asks for.
        assert_eq!(error_body(None, -32700, "bad")["id"], Value::Null);
    }
}

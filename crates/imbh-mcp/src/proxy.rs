//! Forwarding a stdio session to a running `imbhd`.
//!
//! `imbh-tui --mcp-stdio --url http://127.0.0.1:4318` is a stdio MCP server on the outside and an
//! HTTP MCP *client* on the inside: each line it reads becomes one `POST /mcp` against the daemon,
//! and the daemon's JSON-RPC body becomes the line it writes back. That is the mode to use when the
//! answer has to include what is still in the writer's buffer — a read-only opener sees only what
//! `imbhd` has already sealed — or when the database is on another host.
//!
//! # Why the HTTP is hand-written
//!
//! One buffered POST and one buffered response per message, on a connection opened and closed around
//! it. An HTTP client crate would bring a subtree (hyper's client stack, or worse) into the `imbh-tui`
//! binary to send a request with a fixed method, a fixed path, a known-length body, and no
//! redirects, no compression, no TLS, and no connection pool. So this speaks HTTP/1.1 over
//! `std::net::TcpStream` directly and adds **no dependency at all** (ARCHITECTURE.md §11). The cost
//! is a TCP handshake per message, which against a loopback daemon answering an agent's occasional
//! tool call is not a cost worth a dependency.
//!
//! No TLS: an `https://` URL is refused rather than silently downgraded. `imbhd` serves plain HTTP
//! (§10.16), and the deployment this is for is an agent talking to a daemon on its own machine.

use std::io::{self, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use serde_json::Value;

use crate::json::encode_header_value;
use crate::stdio::request_id;
use crate::{META_VERSION, error_body};

/// How long to wait for the TCP handshake. Bounded because a wrong `--url` should say so rather than
/// hang an agent; the *response* is deliberately unbounded, since a legitimate `query_sql` over a
/// large retention window can take a while and a client-side deadline would turn a slow answer into
/// a wrong one.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// The port `imbhd` listens on unless told otherwise, used when a `--url` names no port.
const DEFAULT_PORT: u16 = 4318;

/// The path `imbhd` serves MCP at, used when a `--url` names no path.
const DEFAULT_PATH: &str = "/mcp";

/// JSON-RPC's "internal error" — what a client sees when the daemon cannot be reached or answers
/// something that is not JSON-RPC. Deliberately not one of MCP's own codes: the failure is this
/// proxy's, not the protocol's.
const INTERNAL_ERROR: i64 = -32603;

/// A running `imbhd`'s MCP endpoint, as `--url` names it.
#[derive(Debug)]
pub struct Endpoint {
    /// What to connect to: `host:port`, resolved per request.
    authority: String,
    /// The request target, e.g. `/mcp`.
    path: String,
}

impl Endpoint {
    /// Parse a `--url` value.
    ///
    /// Accepts what a person would actually type: `127.0.0.1:4318`, `http://127.0.0.1:4318`,
    /// `http://host:4318/mcp`, `[::1]:4318`. A missing scheme is `http://`, a missing port is
    /// [`DEFAULT_PORT`], and a missing path is [`DEFAULT_PATH`] — so the common case is just the
    /// address `imbhd` was started on.
    pub fn parse(spec: &str) -> Result<Endpoint, String> {
        let spec = spec.trim();
        if spec.is_empty() {
            return Err("--url is empty".to_owned());
        }
        let rest = match spec.split_once("://") {
            Some(("http", rest)) => rest,
            Some((scheme, _)) => {
                return Err(format!(
                    "--url scheme `{scheme}` is not supported: the MCP proxy speaks plain HTTP/1.1 \
                     only (imbhd serves no TLS). Terminate TLS in front of it, or point --url at \
                     the daemon directly."
                ));
            }
            None => spec,
        };
        // Everything from the first `/` is the path; what precedes it is the authority.
        let (authority, path) = match rest.find('/') {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, ""),
        };
        if authority.is_empty() {
            return Err(format!("--url `{spec}` names no host"));
        }
        if authority.contains('@') {
            return Err(format!(
                "--url `{spec}` carries credentials, which the imbh MCP endpoint does not use"
            ));
        }
        Ok(Endpoint {
            authority: with_port(authority),
            path: if path.is_empty() || path == "/" {
                DEFAULT_PATH.to_owned()
            } else {
                path.to_owned()
            },
        })
    }

    /// The endpoint as a URL, for the banner a person reads when a session starts.
    pub fn url(&self) -> String {
        format!("http://{}{}", self.authority, self.path)
    }

    /// Forward one message and return the line to write back, or `None` for a notification (which
    /// `imbhd` answers `202` with no body, and which JSON-RPC gives no response to either way).
    pub(crate) fn forward(&self, message: &[u8]) -> Option<Vec<u8>> {
        // A message that does not parse is forwarded verbatim with no header mirror: the daemon's
        // own parse error is a better answer than a guess made here.
        let parsed: Option<Value> = serde_json::from_slice(message).ok();
        let headers = parsed.as_ref().map(mirror).unwrap_or_default();

        match self.post(message, &headers) {
            Ok((status, body)) => self.reply(status, &body, message),
            // The daemon is down, the address is wrong, or the connection broke mid-message. A
            // notification still gets no response; a request gets an error it can act on.
            Err(e) => Some(serialize(error_body(
                Some(request_id(message)?),
                INTERNAL_ERROR,
                &format!("cannot reach the imbh MCP endpoint at {}: {e}", self.url()),
            ))),
        }
    }

    /// Turn an HTTP response into the line to write back.
    fn reply(&self, status: u16, body: &[u8], message: &[u8]) -> Option<Vec<u8>> {
        let text = String::from_utf8_lossy(body);
        let text = text.trim();
        if text.is_empty() {
            // `202 Accepted`, no body: the notification path.
            return None;
        }
        // Re-serialized rather than passed through, so a daemon that ever answered with pretty JSON
        // could not break this transport's one-message-per-line framing.
        match serde_json::from_str::<Value>(text) {
            Ok(value) if value.get("jsonrpc").is_some() => Some(serialize(value)),
            // Something that is not JSON-RPC at all: an error page, a `403` from the origin check, a
            // `405` from the wrong path. Report it as this request's failure rather than writing a
            // body the client cannot correlate.
            _ => Some(serialize(error_body(
                Some(request_id(message)?),
                INTERNAL_ERROR,
                &format!(
                    "{} answered HTTP {status} with a body that is not JSON-RPC: {}",
                    self.url(),
                    truncate(text)
                ),
            ))),
        }
    }

    /// One buffered `POST`, one buffered response.
    fn post(&self, body: &[u8], extra: &[(&'static str, String)]) -> io::Result<(u16, Vec<u8>)> {
        let mut stream = self.connect()?;
        let mut request = Vec::with_capacity(body.len() + 256);
        let mut head = format!(
            "POST {} HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\n\
             Accept: application/json\r\nContent-Length: {}\r\nConnection: close\r\n",
            self.path,
            self.authority,
            body.len(),
        );
        for (name, value) in extra {
            head.push_str(name);
            head.push_str(": ");
            head.push_str(value);
            head.push_str("\r\n");
        }
        head.push_str("\r\n");
        request.extend_from_slice(head.as_bytes());
        request.extend_from_slice(body);
        stream.write_all(&request)?;
        stream.flush()?;

        // `Connection: close` means the server hangs up after the response, so reading to EOF is
        // both correct and framing-independent.
        let mut raw = Vec::new();
        stream.read_to_end(&mut raw)?;
        parse_response(&raw)
    }

    fn connect(&self) -> io::Result<TcpStream> {
        let mut last = None;
        for addr in self.authority.to_socket_addrs()? {
            match TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT) {
                Ok(stream) => {
                    // Every message is one small request and one buffered response, so waiting to
                    // coalesce the head with the body would only add latency.
                    let _ = stream.set_nodelay(true);
                    return Ok(stream);
                }
                Err(e) => last = Some(e),
            }
        }
        Err(last.unwrap_or_else(|| {
            io::Error::new(
                io::ErrorKind::AddrNotAvailable,
                format!("`{}` resolved to no address", self.authority),
            )
        }))
    }
}

/// Append [`DEFAULT_PORT`] unless the authority already names one. The `]` dance keeps an IPv6
/// literal's own colons from reading as a port separator.
fn with_port(authority: &str) -> String {
    let has_port = match authority.rfind(']') {
        Some(bracket) => authority[bracket..].contains(':'),
        None => authority.contains(':'),
    };
    if has_port {
        authority.to_owned()
    } else {
        format!("{authority}:{DEFAULT_PORT}")
    }
}

/// The Streamable HTTP header mirror for a message, derived from the message itself.
///
/// The stateless revision requires a modern request's method, protocol version, and tool name to
/// appear as headers *and* agree with the body — a rule for proxies that route without parsing.
/// Nothing else knows those values here, so they are read back out of the body that is about to be
/// sent. A legacy-era message (no `_meta` version) carries no headers, which is what the daemon
/// expects for one.
fn mirror(message: &Value) -> Vec<(&'static str, String)> {
    let params = message.get("params");
    let param = |name: &str| params.and_then(|p| p.get(name)).filter(|v| !v.is_null());
    let Some(version) = param("_meta")
        .and_then(|meta| meta.get(META_VERSION))
        .and_then(Value::as_str)
    else {
        return Vec::new();
    };
    let Some(method) = message.get("method").and_then(Value::as_str) else {
        return Vec::new();
    };
    let mut headers = vec![
        ("MCP-Protocol-Version", version.to_owned()),
        ("Mcp-Method", method.to_owned()),
    ];
    if method == "tools/call"
        && let Some(name) = param("name").and_then(Value::as_str)
    {
        headers.push(("Mcp-Name", encode_header_value(name)));
    }
    headers
}

/// Split an HTTP/1.1 response into its status and its body, undoing chunked framing if the server
/// used it.
fn parse_response(raw: &[u8]) -> io::Result<(u16, Vec<u8>)> {
    let malformed = |what: &str| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("malformed HTTP response: {what}"),
        )
    };
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| malformed("no header terminator"))?;
    let head =
        std::str::from_utf8(&raw[..split]).map_err(|_| malformed("headers are not UTF-8"))?;
    let body = &raw[split + 4..];

    let mut lines = head.split("\r\n");
    let status = lines
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or_else(|| malformed("no status code"))?;
    let chunked = lines.any(|line| {
        line.split_once(':').is_some_and(|(name, value)| {
            name.eq_ignore_ascii_case("transfer-encoding")
                && value.to_ascii_lowercase().contains("chunked")
        })
    });

    if chunked {
        Ok((status, dechunk(body).ok_or_else(|| malformed("bad chunk"))?))
    } else {
        Ok((status, body.to_vec()))
    }
}

/// Undo `Transfer-Encoding: chunked`. `imbhd` sends `Content-Length` for the MCP endpoint's buffered
/// bodies, so this is for a proxy in between that re-frames them.
fn dechunk(mut body: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(body.len());
    loop {
        let end = body.windows(2).position(|w| w == b"\r\n")?;
        let line = std::str::from_utf8(&body[..end]).ok()?;
        // A chunk-size line may carry `;ext=value` extensions after the size.
        let size = usize::from_str_radix(line.split(';').next()?.trim(), 16).ok()?;
        body = &body[end + 2..];
        if size == 0 {
            return Some(out);
        }
        out.extend_from_slice(body.get(..size)?);
        // Skip the chunk's own trailing CRLF.
        body = body.get(size + 2..)?;
    }
}

fn serialize(body: Value) -> Vec<u8> {
    // The value is one this crate built from `&str`s and numbers, so it always serializes.
    serde_json::to_vec(&body).unwrap_or_default()
}

/// Keep a foreign error body from filling a model's context.
fn truncate(text: &str) -> String {
    const MAX: usize = 200;
    if text.chars().count() <= MAX {
        return text.to_owned();
    }
    let kept: String = text.chars().take(MAX).collect();
    format!("{kept}…")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn urls_parse_the_way_a_person_types_them() {
        let e = |spec: &str| Endpoint::parse(spec).expect(spec).url();
        assert_eq!(e("127.0.0.1:4318"), "http://127.0.0.1:4318/mcp");
        assert_eq!(e("http://127.0.0.1:4318"), "http://127.0.0.1:4318/mcp");
        assert_eq!(e("http://127.0.0.1:4318/"), "http://127.0.0.1:4318/mcp");
        assert_eq!(e("http://host:4318/mcp"), "http://host:4318/mcp");
        // A non-default path is honoured — a reverse proxy may mount the endpoint anywhere.
        assert_eq!(e("http://host:8080/imbh/mcp"), "http://host:8080/imbh/mcp");
        // A missing port is imbhd's default, and an IPv6 literal's own colons are not a port.
        assert_eq!(e("localhost"), "http://localhost:4318/mcp");
        assert_eq!(e("[::1]"), "http://[::1]:4318/mcp");
        assert_eq!(e("http://[::1]:9/mcp"), "http://[::1]:9/mcp");
    }

    #[test]
    fn unsupported_urls_are_refused_with_a_reason() {
        // Silently downgrading https to http would send an agent's queries in the clear.
        let e = Endpoint::parse("https://example.com/mcp").unwrap_err();
        assert!(e.contains("plain HTTP/1.1"), "{e}");
        assert!(Endpoint::parse("").is_err());
        assert!(Endpoint::parse("http:///mcp").is_err());
        assert!(Endpoint::parse("http://user:pw@host:4318").is_err());
    }

    #[test]
    fn the_header_mirror_is_derived_from_the_body() {
        let modern = json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {
                "name": "search_logs",
                "_meta": {"io.modelcontextprotocol/protocolVersion": crate::LATEST_VERSION},
            },
        });
        assert_eq!(
            mirror(&modern),
            vec![
                ("MCP-Protocol-Version", crate::LATEST_VERSION.to_owned()),
                ("Mcp-Method", "tools/call".to_owned()),
                ("Mcp-Name", "search_logs".to_owned()),
            ]
        );
        // `Mcp-Name` only exists on a tool call.
        let listed = json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/list",
            "params": {"_meta": {"io.modelcontextprotocol/protocolVersion": crate::LATEST_VERSION}},
        });
        assert_eq!(mirror(&listed).len(), 2);
        // A legacy-era message declares no version, so it carries no headers to agree with.
        assert!(mirror(&json!({"jsonrpc": "2.0", "id": 1, "method": "initialize"})).is_empty());
        // A tool name outside the wire-safe set travels as the transport's Base64 sentinel.
        let unicode = json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {"name": "世界", "_meta": {"io.modelcontextprotocol/protocolVersion": crate::LATEST_VERSION}},
        });
        assert_eq!(mirror(&unicode)[2].1, "=?base64?5LiW55WM?=");
    }

    #[test]
    fn responses_split_into_status_and_body() {
        let (status, body) =
            parse_response(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}").expect("well-formed");
        assert_eq!((status, body.as_slice()), (200, b"{}".as_slice()));

        // A body re-framed as chunked by something in between still reads back whole.
        let (status, body) = parse_response(
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\n{\"a\r\n3\r\n\":1\r\n1\r\n}\r\n0\r\n\r\n",
        )
        .expect("chunked");
        assert_eq!((status, body.as_slice()), (200, br#"{"a":1}"#.as_slice()));

        // A 202 with no body at all is the notification path, and must not look like an error.
        let (status, body) =
            parse_response(b"HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\n\r\n").expect("202");
        assert_eq!((status, body.len()), (202, 0));

        assert!(parse_response(b"garbage").is_err());
        assert!(parse_response(b"HTTP/1.1\r\n\r\n").is_err());
    }

    #[test]
    fn a_non_jsonrpc_response_becomes_an_error_the_client_can_correlate() {
        let endpoint = Endpoint::parse("127.0.0.1:4318").expect("url");
        let request = br#"{"jsonrpc":"2.0","id":9,"method":"tools/list"}"#;

        let line = endpoint
            .reply(200, br#"{"jsonrpc":"2.0","id":9,"result":{}}"#, request)
            .expect("a request gets a response");
        let value: Value = serde_json::from_slice(&line).expect("json");
        assert_eq!(value["result"], json!({}));

        let line = endpoint
            .reply(405, br#"{"error":"POST only"}"#, request)
            .expect("a request gets a response");
        let value: Value = serde_json::from_slice(&line).expect("json");
        assert_eq!(value["id"], json!(9));
        assert_eq!(value["error"]["code"], json!(INTERNAL_ERROR));
        assert!(
            value["error"]["message"]
                .as_str()
                .expect("message")
                .contains("HTTP 405")
        );

        // An empty body is `202 Accepted`: no line at all.
        assert!(endpoint.reply(202, b"", request).is_none());
        // A notification that fails upstream still gets no response, since JSON-RPC defines none.
        assert!(
            endpoint
                .reply(
                    405,
                    b"nope",
                    br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#
                )
                .is_none()
        );
    }

    #[test]
    fn foreign_error_bodies_are_truncated() {
        let long = "x".repeat(500);
        assert_eq!(truncate(&long).chars().count(), 201);
        assert_eq!(truncate("short"), "short");
    }
}

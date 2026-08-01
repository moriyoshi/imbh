//! End-to-end test of the MCP **stdio** transport: drive [`imbh_mcp::stdio::serve`] with the byte
//! stream a client would write to a spawned server's stdin, and read back what it would see on
//! stdout.
//!
//! Both backends are covered. The local one answers from a `Db` seeded with real OTLP in this
//! process; the remote one forwards to a hand-rolled fake `imbhd` on a loopback socket, which is
//! what lets the header mirror the proxy has to synthesize be asserted from the *receiving* side.
//! Everything is in-memory or on loopback: no daemon, no fixtures, no network (TESTING.md Layer 1).

use std::io::{BufRead, BufReader, Cursor, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::mpsc;

use imbh::Db;
use imbh_mcp::proxy::Endpoint;
use imbh_mcp::stdio::{self, Backend};
use imbh_test_support::otlp::otlp_rich;
use serde_json::Value;

const MODERN: &str = "2026-07-28";

/// Feed `lines` to a stdio session and collect the lines it writes back.
async fn session(backend: &Backend, lines: &[&str]) -> Vec<Value> {
    let input = Cursor::new(lines.join("\n").into_bytes());
    let mut output = Vec::new();
    stdio::serve(backend, input, &mut output)
        .await
        .expect("the session ends at EOF");
    let text = String::from_utf8(output).expect("stdout is UTF-8");
    text.lines()
        .map(|line| {
            serde_json::from_str(line)
                .unwrap_or_else(|e| panic!("response line is not JSON ({e}): {line}"))
        })
        .collect()
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
    db
}

/// A modern (stateless-era) request, which over stdio carries its protocol version in `_meta` and
/// nothing else — there is no header channel to mirror it into.
fn modern(id: u32, method: &str, params: &str) -> String {
    format!(
        r#"{{"jsonrpc":"2.0","id":{id},"method":"{method}","params":{{{params}{}"_meta":{{"io.modelcontextprotocol/protocolVersion":"{MODERN}"}}}}}}"#,
        if params.is_empty() { "" } else { "," }
    )
}

#[tokio::test]
async fn a_stdio_session_serves_both_protocol_eras_from_a_local_database() {
    let backend = Backend::Local(seeded_db().await);

    let replies = session(
        &backend,
        &[
            // Legacy era: the handshake, then its notification (which gets no answer at all).
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}"#,
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
            // Modern era: no headers anywhere, and it still works — the point of the transport split.
            &modern(2, "server/discover", ""),
            &modern(3, "tools/list", ""),
            &modern(
                4,
                "tools/call",
                r#""name":"search_logs","arguments":{"service":"cart","start_unix_nano":0,"end_unix_nano":9223372036854775807}"#,
            ),
            // Framing: blank lines are skipped rather than answered.
            "",
            "   ",
            r#"{"jsonrpc":"2.0","id":5,"method":"ping"}"#,
        ],
    )
    .await;

    // Four requests plus one ping; the notification and the blank lines wrote nothing.
    assert_eq!(replies.len(), 5, "{replies:#?}");

    assert_eq!(replies[0]["id"], 1);
    assert_eq!(replies[0]["result"]["protocolVersion"], "2025-06-18");

    assert_eq!(replies[1]["result"]["supportedVersions"][0], MODERN);
    assert_eq!(replies[1]["result"]["resultType"], "complete");

    let tools = replies[2]["result"]["tools"]
        .as_array()
        .expect("a tool list");
    assert!(
        tools.iter().any(|t| t["name"] == "search_logs"),
        "{tools:#?}"
    );
    assert!(tools.iter().all(|t| t["inputSchema"]["type"] == "object"));

    let call = &replies[3]["result"];
    assert_eq!(call["isError"], false, "{call:#?}");
    let text: Value =
        serde_json::from_str(call["content"][0]["text"].as_str().expect("one text block"))
            .expect("the tool's document");
    assert_eq!(text["entries"][0]["service"], "cart");
    assert!(
        text["entries"][0]["body"]
            .as_str()
            .expect("a body")
            .contains("checkout failed"),
        "{text:#?}"
    );

    assert_eq!(replies[4]["id"], 5);
}

#[tokio::test]
async fn one_bad_line_does_not_end_the_session() {
    let backend = Backend::Local(seeded_db().await);

    let replies = session(
        &backend,
        &[
            "this is not json",
            r#"{"jsonrpc":"2.0","id":1,"method":"no/such/method"}"#,
            // A modern request naming a version this server does not implement.
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"1999-01-01"}}}"#,
            // ...and the session is still serving after all three.
            r#"{"jsonrpc":"2.0","id":3,"method":"ping"}"#,
        ],
    )
    .await;

    assert_eq!(replies.len(), 4, "{replies:#?}");
    assert_eq!(replies[0]["error"]["code"], -32700); // parse error
    assert_eq!(replies[0]["id"], Value::Null);
    assert_eq!(replies[1]["error"]["code"], -32601); // method not found
    assert_eq!(replies[2]["error"]["code"], -32022); // unsupported protocol version
    assert_eq!(replies[2]["error"]["data"]["supported"][0], MODERN);
    assert!(replies[3]["result"].is_object(), "{replies:#?}");
}

#[tokio::test]
async fn a_tool_error_is_reported_to_the_model_not_to_the_transport() {
    let backend = Backend::Local(seeded_db().await);

    let replies = session(
        &backend,
        &[
            // A bad argument is something a model can fix, so it comes back as a tool result with
            // `isError`, not as a JSON-RPC error that would look like a broken server.
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"search_logs","arguments":{"since":"whenever"}}}"#,
            // An unknown tool, by contrast, is a protocol error: no argument would fix it.
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"drop_everything"}}"#,
        ],
    )
    .await;

    assert_eq!(replies[0]["result"]["isError"], true);
    assert!(
        replies[0]["result"]["content"][0]["text"]
            .as_str()
            .expect("a message")
            .contains("since"),
        "{:#?}",
        replies[0]
    );
    assert_eq!(replies[1]["error"]["code"], -32602);
    assert!(
        replies[1]["error"]["message"]
            .as_str()
            .expect("a message")
            .contains("Unknown tool"),
    );
}

// ── the `--url` proxy ───────────────────────────────────────────────────────────────────────────

/// One request a fake `imbhd` received: its headers, lowercased by name, and its body.
struct Received {
    headers: Vec<(String, String)>,
    body: String,
}

impl Received {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }
}

/// A fake `imbhd` on loopback: answers every request with `response`, and reports what it was sent.
///
/// Hand-rolled for the same reason the proxy is: driving `imbh_server::app` from here would make
/// this crate's tests depend on the crate that depends on *it*, and what needs asserting is the
/// bytes on the wire anyway.
fn fake_imbhd(response: &'static str, requests: usize) -> (String, mpsc::Receiver<Received>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let addr = listener.local_addr().expect("addr").to_string();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for _ in 0..requests {
            let (stream, _) = listener.accept().expect("accept");
            let received = serve_one(stream, response);
            if tx.send(received).is_err() {
                return;
            }
        }
    });
    (addr, rx)
}

fn serve_one(stream: TcpStream, response: &str) -> Received {
    let mut reader = BufReader::new(stream.try_clone().expect("clone"));
    let mut headers = Vec::new();
    let mut line = String::new();
    reader.read_line(&mut line).expect("request line");
    assert!(line.starts_with("POST /mcp HTTP/1.1"), "{line}");
    let mut length = 0usize;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).expect("header line");
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        let (name, value) = line.split_once(':').expect("a header");
        let (name, value) = (name.trim().to_ascii_lowercase(), value.trim().to_owned());
        if name == "content-length" {
            length = value.parse().expect("a length");
        }
        headers.push((name, value));
    }
    let mut body = vec![0u8; length];
    reader.read_exact(&mut body).expect("body");

    let mut stream = stream;
    let head = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        response.len()
    );
    stream.write_all(head.as_bytes()).expect("write head");
    stream.write_all(response.as_bytes()).expect("write body");
    stream.flush().expect("flush");
    // `Connection: close`: the client reads to EOF, so the socket has to actually close.
    drop(stream);

    Received {
        headers,
        body: String::from_utf8(body).expect("UTF-8 body"),
    }
}

#[tokio::test]
async fn the_proxy_mirrors_a_modern_request_into_the_headers_imbhd_requires() {
    let (addr, received) = fake_imbhd(r#"{"jsonrpc":"2.0","id":4,"result":{"tools":[]}}"#, 1);
    let backend = Backend::Remote(Endpoint::parse(&addr).expect("url"));

    let replies = session(
        &backend,
        &[&modern(
            4,
            "tools/call",
            r#""name":"search_logs","arguments":{}"#,
        )],
    )
    .await;

    // The daemon's JSON-RPC body is what the client sees, unchanged.
    assert_eq!(replies.len(), 1);
    assert_eq!(replies[0]["result"]["tools"], serde_json::json!([]));

    // ...and the request that produced it carried the header mirror the stateless transport demands,
    // which only this proxy could have synthesized: the message arrived over stdio with no headers.
    let request = received.recv().expect("the fake daemon was reached");
    assert_eq!(request.header("mcp-protocol-version"), Some(MODERN));
    assert_eq!(request.header("mcp-method"), Some("tools/call"));
    assert_eq!(request.header("mcp-name"), Some("search_logs"));
    assert_eq!(request.header("content-type"), Some("application/json"));
    assert!(request.body.contains(r#""method":"tools/call""#));
}

#[tokio::test]
async fn a_legacy_message_is_forwarded_without_a_header_mirror() {
    let (addr, received) = fake_imbhd(r#"{"jsonrpc":"2.0","id":1,"result":{}}"#, 1);
    let backend = Backend::Remote(Endpoint::parse(&addr).expect("url"));

    let replies = session(
        &backend,
        &[r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}"#],
    )
    .await;
    assert_eq!(replies.len(), 1);

    // A legacy request that arrived with `MCP-Protocol-Version` would be *refused* by the daemon's
    // modern-era validation, so the mirror must stay off for one.
    let request = received.recv().expect("the fake daemon was reached");
    assert_eq!(request.header("mcp-protocol-version"), None);
    assert_eq!(request.header("mcp-method"), None);
}

#[tokio::test]
async fn an_unreachable_daemon_is_an_error_the_client_can_correlate() {
    // Bound and immediately dropped, so the port is (almost certainly) closed.
    let addr = {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        listener.local_addr().expect("addr").to_string()
    };
    let backend = Backend::Remote(Endpoint::parse(&addr).expect("url"));

    let replies = session(
        &backend,
        &[
            r#"{"jsonrpc":"2.0","id":7,"method":"tools/list"}"#,
            // A notification has no response even when forwarding fails — JSON-RPC defines none.
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        ],
    )
    .await;

    assert_eq!(replies.len(), 1, "{replies:#?}");
    assert_eq!(replies[0]["id"], 7);
    assert_eq!(replies[0]["error"]["code"], -32603);
    assert!(
        replies[0]["error"]["message"]
            .as_str()
            .expect("a message")
            .contains("cannot reach the imbh MCP endpoint"),
        "{:#?}",
        replies[0]
    );
}

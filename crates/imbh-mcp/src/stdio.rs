//! The stdio transport: newline-delimited JSON-RPC over a pipe.
//!
//! stdio is the transport MCP clients "SHOULD support whenever possible", and it is what an agent
//! that *launches* its server speaks: the client spawns the process, writes one JSON-RPC message per
//! line to its stdin, and reads one response per line from its stdout. There is no port to bind, no
//! `Origin` to check, and no session to keep — the pipe is the authorization, since only the process
//! that spawned this one can write to it.
//!
//! The framing rules are the transport's: messages are UTF-8, delimited by newlines, and **must not
//! contain embedded newlines**. That falls out for free here — `serde_json` escapes any newline
//! inside a string — so the loop can be a plain `read_until(b'\n')`.
//!
//! # Two backends
//!
//! A stdio session answers from one of two places ([`Backend`]):
//!
//! - [`Backend::Local`] opens the database directory itself, read-only. `Db::open_read_only` takes
//!   no writer lock, so this works *alongside* a running `imbhd` writing the same directory: the
//!   reader sees every segment the writer has sealed.
//! - [`Backend::Remote`] forwards each message to a running `imbhd`'s `POST /mcp` instead ([`proxy`](crate::proxy)),
//!   which is what you want when the data you are asking about is still in that process's write
//!   buffer, or when the database lives on another host.
//!
//! # Concurrency
//!
//! One message at a time, in order. Reads and writes are the blocking `std::io` ones even though
//! [`serve`] is `async`, which is sound precisely because the loop is the only thing on its runtime:
//! there is nothing else on that thread for a blocking `read_until` to starve. Serving requests
//! concurrently would buy nothing either — a `Db` query is blocking parquet/tantivy I/O from start to
//! finish (see [`offload`](crate::offload)), so two in flight would contend for the same disk rather
//! than overlap.

use std::io::{self, BufRead, Write};
use std::sync::Arc;

use imbh::Db;
use serde_json::Value;

use crate::proxy::Endpoint;
use crate::{Transport, error_body, handle};

/// JSON-RPC's "internal error", used when a reply cannot be serialized at all.
const INTERNAL_ERROR: i64 = -32603;

/// Where a stdio session's messages are answered.
pub enum Backend {
    /// In-process, against a database this process has open (read-only).
    Local(Arc<Db>),
    /// Forwarded to a running `imbhd`'s MCP endpoint.
    Remote(Endpoint),
}

impl Backend {
    /// Answer one message. `None` is a notification: JSON-RPC defines no response for one, and on a
    /// line-framed transport that means writing nothing at all rather than writing an empty line.
    async fn respond(&self, message: &[u8]) -> Option<Vec<u8>> {
        match self {
            Backend::Local(db) => {
                let reply = handle(db, message, &Transport::Stdio).await;
                Some(serialize(reply.body?, message))
            }
            Backend::Remote(endpoint) => endpoint.forward(message),
        }
    }
}

/// Serialize a reply body, falling back to a JSON-RPC error that keeps the client's id.
///
/// Serializing a `Value` fails only on a non-finite float or a non-string map key, neither of which
/// the tools can construct (`json::number` maps a non-finite float to `null`) — but a stdio session
/// must not die on a request path, so this degrades to an error the client can correlate.
fn serialize(body: Value, message: &[u8]) -> Vec<u8> {
    serde_json::to_vec(&body).unwrap_or_else(|e| {
        let fallback = error_body(
            request_id(message),
            INTERNAL_ERROR,
            &format!("response could not be serialized: {e}"),
        );
        serde_json::to_vec(&fallback).unwrap_or_else(|_| b"{\"jsonrpc\":\"2.0\",\"id\":null,\"error\":{\"code\":-32603,\"message\":\"internal error\"}}".to_vec())
    })
}

/// The `id` of a message we may only be able to read partially — used to correlate an error a
/// transport (rather than the dispatch) produced. A message that does not parse has no id.
pub(crate) fn request_id(message: &[u8]) -> Option<Value> {
    serde_json::from_slice::<Value>(message)
        .ok()?
        .get("id")
        .filter(|id| !id.is_null())
        .cloned()
}

/// Run a stdio MCP session until `input` reaches EOF.
///
/// EOF is how a client says it is done — it closes the child's stdin and expects the process to
/// exit — so that is a normal return, not an error. A malformed line is *not* fatal: it is answered
/// with a JSON-RPC parse error and the loop goes on, because one bad message from a client should
/// not take down a session that is otherwise working.
///
/// Errors returned here are I/O ones on the pipes themselves. A broken pipe (the client went away
/// mid-write) is reported as `Ok(())` for the same reason EOF is.
pub async fn serve<R: BufRead, W: Write>(
    backend: &Backend,
    mut input: R,
    mut output: W,
) -> io::Result<()> {
    let mut line = Vec::new();
    loop {
        line.clear();
        if input.read_until(b'\n', &mut line)? == 0 {
            return Ok(());
        }
        // Trim the delimiter and any framing whitespace. A blank keep-alive line is skipped rather
        // than answered with a parse error.
        let message = trim(&line);
        if message.is_empty() {
            continue;
        }
        let Some(response) = backend.respond(message).await else {
            continue;
        };
        match write_line(&mut output, &response) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::BrokenPipe => return Ok(()),
            Err(e) => return Err(e),
        }
    }
}

fn trim(line: &[u8]) -> &[u8] {
    let start = line.iter().position(|b| !b.is_ascii_whitespace());
    let Some(start) = start else { return &[] };
    let end = line
        .iter()
        .rposition(|b| !b.is_ascii_whitespace())
        .unwrap_or(start);
    &line[start..=end]
}

/// Write one framed message and flush it. The flush is the point: a client is blocked reading this
/// line, and a buffered response that never leaves the process is a hung agent.
fn write_line<W: Write>(output: &mut W, response: &[u8]) -> io::Result<()> {
    output.write_all(response)?;
    output.write_all(b"\n")?;
    output.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn framing_whitespace_is_trimmed_and_blank_lines_are_empty() {
        assert_eq!(trim(b"{\"a\":1}\n"), b"{\"a\":1}");
        // A client on Windows line endings, and one that indents its framing.
        assert_eq!(trim(b"  {\"a\":1}  \r\n"), b"{\"a\":1}");
        assert_eq!(trim(b"\r\n"), b"");
        assert_eq!(trim(b""), b"");
    }

    #[test]
    fn a_serializable_body_round_trips_and_a_line_is_one_line() {
        // The framing contract: no response may contain a raw newline, whatever a tool returned.
        let body = json!({"jsonrpc": "2.0", "id": 1, "result": {"text": "a\nb"}});
        let bytes = serialize(body, b"{}");
        assert!(!bytes.contains(&b'\n'));
        let back: Value = serde_json::from_slice(&bytes).expect("valid json");
        assert_eq!(back["result"]["text"], json!("a\nb"));
    }

    #[test]
    fn the_id_of_a_partially_readable_message_is_recovered() {
        assert_eq!(request_id(br#"{"id":7,"method":"ping"}"#), Some(json!(7)));
        assert_eq!(request_id(br#"{"id":"a"}"#), Some(json!("a")));
        // A notification and an unparseable line both have no id to correlate with.
        assert_eq!(request_id(br#"{"method":"x"}"#), None);
        assert_eq!(request_id(br#"{"id":null}"#), None);
        assert_eq!(request_id(b"not json"), None);
    }
}

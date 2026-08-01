//! HTTP/1.1 protocol handling the hand-rolled parser got wrong or did not do at all, over a **real
//! loopback socket**.
//!
//! Each test here pins something the move to hyper either fixed or newly bounded:
//!
//! - a **chunked** body is ingested. The old parser keyed entirely off `Content-Length`, so a chunked
//!   upload read as zero bytes and came back `200 {"accepted":0}` — a success status for silently
//!   dropped telemetry, which is the worst shape a bug can take. Go's `http.Client` sends chunked
//!   whenever the body is not a sized reader, so this was reachable from a stock client.
//! - a **gzip** body is inflated. The OTel Collector's `otlphttp` exporter sets
//!   `compression: gzip` by default, so a stock collector in front of `imbhd` was failing outright.
//! - a body over [`Limits::max_body`] is refused, whether it announces itself in `Content-Length`,
//!   arrives without announcing anything, or only expands past the cap once inflated.
//! - **keep-alive** works, so a collector exporting every second stops paying a handshake per batch.
//!
//! Loopback only, no daemon (TESTING.md Layer 1).

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::time::{Duration, Instant};

use imbh::Db;
use imbh_server::{Limits, Shutdown, serve_with_limits_until};
use imbh_test_support::http;
use imbh_test_support::otlp::otlp_log;

/// Longest any wait here takes before it is called a failure.
const PATIENCE: Duration = Duration::from_secs(20);

/// A running server plus the token that stops it, so each test gets its own port and shuts down.
struct Server {
    addr: String,
    shutdown: Arc<Shutdown>,
    thread: Option<std::thread::JoinHandle<()>>,
    db: Arc<Db>,
}

impl Server {
    fn start(limits: Limits) -> Server {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
        let addr = listener.local_addr().expect("local addr").to_string();
        drop(listener);

        let db: Arc<Db> = Db::in_memory().open().expect("open in-memory db");
        let shutdown = Shutdown::with_drain_timeout(Duration::from_secs(1));
        let thread = {
            let (db, addr, shutdown) = (db.clone(), addr.clone(), shutdown.clone());
            std::thread::spawn(move || {
                serve_with_limits_until(db, &addr, limits, shutdown).expect("serve")
            })
        };

        let deadline = Instant::now() + PATIENCE;
        while Instant::now() < deadline {
            if let Ok(resp) = http::get(&addr, "/health")
                && resp.status == 200
            {
                return Server {
                    addr,
                    shutdown,
                    thread: Some(thread),
                    db,
                };
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("server did not become ready on {addr}");
    }

    /// Open a connection with a bounded read, so a test asserting on the reply cannot hang.
    fn connect(&self) -> TcpStream {
        let conn = TcpStream::connect(&self.addr).expect("connect");
        conn.set_read_timeout(Some(PATIENCE)).expect("read timeout");
        conn
    }

    /// Send a raw request and read the reply until the server closes.
    fn exchange(&self, head: &str, body: &[u8]) -> String {
        let mut conn = self.connect();
        conn.write_all(head.as_bytes()).expect("write head");
        conn.write_all(body).expect("write body");
        conn.flush().expect("flush");
        let mut reply = String::new();
        let _ = conn.read_to_string(&mut reply);
        reply
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.shutdown.trigger();
        if let Some(thread) = self.thread.take() {
            thread.join().expect("the accept loop returns");
        }
    }
}

/// How many rows the `logs` table holds — the `count(*)` *value*, not the batch's row count.
fn logs_in(db: &Arc<Db>) -> i64 {
    use imbh::arrow::array::{Array, Int64Array};
    let batches = db
        .blocking()
        .sql("SELECT count(*) AS c FROM logs")
        .expect("count the logs table");
    let column = batches
        .iter()
        .find(|b| b.num_rows() > 0)
        .expect("a row of output")
        .column(0)
        .as_any()
        .downcast_ref::<Int64Array>()
        .expect("count(*) is Int64")
        .clone();
    assert_eq!(column.len(), 1);
    column.value(0)
}

/// gzip `data` the way an OTLP exporter would.
fn gzip(data: &[u8]) -> Vec<u8> {
    use flate2::Compression;
    use flate2::write::GzEncoder;
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(data).expect("compress");
    encoder.finish().expect("finish compressing")
}

/// A `POST /v1/logs` body split into `Transfer-Encoding: chunked` frames, with no `Content-Length` —
/// exactly what a client streaming an unsized body sends.
fn chunked(body: &[u8], chunk: usize) -> Vec<u8> {
    let mut framed = Vec::new();
    for piece in body.chunks(chunk.max(1)) {
        framed.extend_from_slice(format!("{:x}\r\n", piece.len()).as_bytes());
        framed.extend_from_slice(piece);
        framed.extend_from_slice(b"\r\n");
    }
    framed.extend_from_slice(b"0\r\n\r\n");
    framed
}

#[test]
fn a_chunked_body_is_ingested_rather_than_read_as_empty() {
    let server = Server::start(Limits::default());
    let body = otlp_log("cart", "sent without a content-length", 1);

    let head = format!(
        "POST /v1/logs HTTP/1.1\r\nHost: {}\r\nContent-Type: application/x-protobuf\r\n\
         Transfer-Encoding: chunked\r\nConnection: close\r\n\r\n",
        server.addr
    );
    let reply = server.exchange(&head, &chunked(&body, 7));

    assert!(
        reply.starts_with("HTTP/1.1 200 OK"),
        "a chunked upload was rejected: {reply:?}"
    );
    // The regression this test exists for: the old parser answered 200 with `"accepted":0`, so the
    // status alone proves nothing — the count is the assertion that matters.
    assert!(
        reply.contains("\"accepted\":1"),
        "a chunked upload was read as an empty body: {reply:?}"
    );
    assert_eq!(
        logs_in(&server.db),
        1,
        "the chunked upload's row is missing"
    );
}

#[test]
fn a_gzip_body_is_inflated_and_ingested() {
    let server = Server::start(Limits::default());
    let body = otlp_log("cart", "compressed on the wire", 1);
    let compressed = gzip(&body);

    let head = format!(
        "POST /v1/logs HTTP/1.1\r\nHost: {}\r\nContent-Type: application/x-protobuf\r\n\
         Content-Encoding: gzip\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        server.addr,
        compressed.len()
    );
    let reply = server.exchange(&head, &compressed);

    assert!(
        reply.starts_with("HTTP/1.1 200 OK") && reply.contains("\"accepted\":1"),
        "a gzip upload was not inflated: {reply:?}"
    );
    assert_eq!(logs_in(&server.db), 1, "the gzip upload's row is missing");
}

#[test]
fn a_body_that_is_not_actually_gzip_is_a_400() {
    let server = Server::start(Limits::default());
    let body = otlp_log("cart", "mislabelled", 1);

    let head = format!(
        "POST /v1/logs HTTP/1.1\r\nHost: {}\r\nContent-Type: application/x-protobuf\r\n\
         Content-Encoding: gzip\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        server.addr,
        body.len()
    );
    let reply = server.exchange(&head, &body);

    assert!(
        reply.starts_with("HTTP/1.1 400 Bad Request"),
        "a mislabelled body was not refused: {reply:?}"
    );
    assert_eq!(logs_in(&server.db), 0, "a mislabelled body was ingested");
}

#[test]
fn a_declared_length_over_the_cap_is_refused_before_the_body_arrives() {
    // The allocation bug the cap exists for: the old parser did `vec![0u8; content_length]` straight
    // from this header, so a 10 GiB claim with no body behind it was a 10 GiB allocation.
    let server = Server::start(Limits {
        max_body: 1024,
        ..Limits::default()
    });

    let head = format!(
        "POST /v1/logs HTTP/1.1\r\nHost: {}\r\nContent-Type: application/x-protobuf\r\n\
         Content-Length: 10737418240\r\nConnection: close\r\n\r\n",
        server.addr
    );
    // Deliberately no body: the refusal must come from the header alone, so this returns rather than
    // waiting for ten gigabytes that will never arrive.
    let started = Instant::now();
    let reply = server.exchange(&head, b"");
    assert!(
        reply.starts_with("HTTP/1.1 413 Payload Too Large"),
        "an oversized claim was not refused: {reply:?}"
    );
    assert!(
        started.elapsed() < PATIENCE / 2,
        "the refusal waited for the body it was refusing: {:?}",
        started.elapsed()
    );
}

#[test]
fn an_unannounced_body_over_the_cap_is_refused_as_it_arrives() {
    // Chunked has no up-front length, so the only thing that can stop it is counting bytes while they
    // land.
    let server = Server::start(Limits {
        max_body: 512,
        ..Limits::default()
    });

    let head = format!(
        "POST /v1/logs HTTP/1.1\r\nHost: {}\r\nContent-Type: application/x-protobuf\r\n\
         Transfer-Encoding: chunked\r\nConnection: close\r\n\r\n",
        server.addr
    );
    let reply = server.exchange(&head, &chunked(&vec![b'x'; 8192], 256));

    assert!(
        reply.starts_with("HTTP/1.1 413 Payload Too Large"),
        "an unannounced oversized body was not refused: {reply:?}"
    );
}

#[test]
fn a_gzip_body_that_expands_past_the_cap_is_refused() {
    // A compression bomb is a *small* upload, so neither the declared length nor the received byte
    // count can catch it — only bounding the inflated output can.
    let server = Server::start(Limits {
        max_body: 4096,
        ..Limits::default()
    });
    let compressed = gzip(&vec![0u8; 1024 * 1024]);
    assert!(
        compressed.len() < 4096,
        "the bomb must be under the cap on the wire to test anything, got {}",
        compressed.len()
    );

    let head = format!(
        "POST /v1/logs HTTP/1.1\r\nHost: {}\r\nContent-Type: application/x-protobuf\r\n\
         Content-Encoding: gzip\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        server.addr,
        compressed.len()
    );
    let reply = server.exchange(&head, &compressed);

    assert!(
        reply.starts_with("HTTP/1.1 413 Payload Too Large"),
        "an inflating body was not refused: {reply:?}"
    );
}

#[test]
fn one_connection_serves_more_than_one_request() {
    // Every response used to carry `Connection: close`, so an exporter pushing a batch a second paid a
    // TCP handshake — and a thread spawn — for each one.
    let server = Server::start(Limits::default());
    let mut conn = server.connect();

    let body = otlp_log("cart", "reused connection", 1);
    let head = format!(
        "POST /v1/logs HTTP/1.1\r\nHost: {}\r\nContent-Type: application/x-protobuf\r\n\
         Content-Length: {}\r\n\r\n",
        server.addr,
        body.len()
    );
    for _ in 0..2 {
        conn.write_all(head.as_bytes()).expect("write head");
        conn.write_all(&body).expect("write body");
        conn.flush().expect("flush");
    }

    // Read until both replies are in, rather than to EOF: the point of the test is that the server
    // does *not* close after the first one.
    let deadline = Instant::now() + PATIENCE;
    let mut replies = Vec::new();
    while Instant::now() < deadline {
        let mut buf = [0u8; 4096];
        match conn.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => replies.extend_from_slice(&buf[..n]),
            Err(_) => break,
        }
        if String::from_utf8_lossy(&replies)
            .matches("HTTP/1.1 200 OK")
            .count()
            >= 2
        {
            break;
        }
    }
    let replies = String::from_utf8_lossy(&replies).into_owned();

    assert_eq!(
        replies.matches("HTTP/1.1 200 OK").count(),
        2,
        "the connection was not reused: {replies:?}"
    );
    assert!(
        !replies.contains("Connection: close"),
        "the server still closes after every response: {replies:?}"
    );
    assert_eq!(logs_in(&server.db), 2, "both requests should have ingested");
}

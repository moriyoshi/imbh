//! Per-connection I/O deadlines (`IoTimeouts`), over a **real loopback socket**.
//!
//! Thread-per-connection means an idle client costs a server thread, and the hand-rolled parser blocks
//! in `read_line`/`read_exact` with no deadline of its own. These tests pin the two phase rules that
//! bound it, and the distinction between them — which is the part a single socket timeout gets wrong:
//!
//! - the request **head** is bounded *in total*, so a client that trickles bytes forever (never idle,
//!   never finished) is still cut off;
//! - the **body** is bounded *per read*, so a large upload that keeps making progress is never cut off
//!   for taking a while — only for stalling.
//!
//! Plus the payoff for shutdown: an idle connection no longer holds the drain open for its whole
//! deadline. Loopback only, no daemon (TESTING.md Layer 1).

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::time::{Duration, Instant};

use imbh::Db;
use imbh_server::{IoTimeouts, Shutdown, serve_with_until};
use imbh_test_support::http;
use imbh_test_support::otlp::otlp_log;

/// Longest any wait here takes before it is called a failure.
const PATIENCE: Duration = Duration::from_secs(20);

/// The deadlines under test. Short enough to keep the suite fast, long enough that a loaded CI box
/// cannot trip them by accident: every "must not time out" case in this file stalls for at most a third
/// of the phase it is testing.
const HEADER: Duration = Duration::from_millis(600);
const BODY: Duration = Duration::from_millis(600);

/// A running server plus the token that stops it, so each test gets its own port and shuts down.
struct Server {
    addr: String,
    shutdown: Arc<Shutdown>,
    thread: Option<std::thread::JoinHandle<()>>,
    db: Arc<Db>,
}

impl Server {
    fn start(timeouts: IoTimeouts, drain: Duration) -> Server {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
        let addr = listener.local_addr().expect("local addr").to_string();
        drop(listener);

        let db: Arc<Db> = Db::in_memory().open().expect("open in-memory db");
        let shutdown = Shutdown::with_drain_timeout(drain);
        let thread = {
            let (db, addr, shutdown) = (db.clone(), addr.clone(), shutdown.clone());
            std::thread::spawn(move || {
                serve_with_until(db, &addr, timeouts, shutdown).expect("serve")
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

    /// Stop the server and wait for the accept loop to return.
    fn stop(&mut self) {
        self.shutdown.trigger();
        if let Some(thread) = self.thread.take() {
            thread.join().expect("the accept loop returns");
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop();
    }
}

/// The head of a `POST /v1/logs` announcing `len` body bytes, without the body.
fn post_head(addr: &str, len: usize) -> String {
    format!(
        "POST /v1/logs HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/x-protobuf\r\n\
         Content-Length: {len}\r\n\r\n"
    )
}

/// Read whatever the server says until it closes, or `PATIENCE` runs out.
fn read_reply(conn: &mut TcpStream) -> String {
    let mut reply = String::new();
    let _ = conn.read_to_string(&mut reply);
    reply
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

#[test]
fn a_client_that_connects_and_says_nothing_gets_408() {
    let server = Server::start(
        IoTimeouts {
            header: HEADER,
            body: BODY,
        },
        PATIENCE,
    );

    let started = Instant::now();
    let mut conn = server.connect();
    let reply = read_reply(&mut conn);
    let elapsed = started.elapsed();

    assert!(
        reply.starts_with("HTTP/1.1 408 Request Timeout"),
        "expected a 408, got {reply:?}"
    );
    // Bounded by the header deadline, not by the client giving up.
    assert!(
        elapsed >= HEADER && elapsed < HEADER * 8,
        "408 arrived after {elapsed:?}, expected ~{HEADER:?}"
    );
}

#[test]
fn a_trickling_client_is_cut_off_by_the_total_head_deadline() {
    let server = Server::start(
        IoTimeouts {
            header: HEADER,
            body: BODY,
        },
        PATIENCE,
    );
    let mut conn = server.connect();

    // One byte every HEADER/6, forever: never idle for a whole allowance, never finishing the head
    // either. A per-read timeout would let this run indefinitely — the total budget is what stops it.
    let writer = {
        let mut conn = conn.try_clone().expect("clone the connection");
        std::thread::spawn(move || {
            for _ in 0..60 {
                if conn.write_all(b"X").is_err() || conn.flush().is_err() {
                    return; // the server hung up on us, which is the point of the test
                }
                std::thread::sleep(HEADER / 6);
            }
        })
    };

    let started = Instant::now();
    let reply = read_reply(&mut conn);
    let elapsed = started.elapsed();
    writer.join().expect("writer thread");

    assert!(
        reply.starts_with("HTTP/1.1 408 Request Timeout"),
        "a trickling client was not cut off; got {reply:?}"
    );
    assert!(
        elapsed < HEADER * 8,
        "the head deadline behaved like a per-read allowance: {elapsed:?}"
    );
}

#[test]
fn a_slow_but_progressing_body_is_not_cut_off() {
    let server = Server::start(
        IoTimeouts {
            header: HEADER,
            body: BODY,
        },
        PATIENCE,
    );
    let mut conn = server.connect();

    let body = otlp_log("cart", "slow upload", 1);
    conn.write_all(post_head(&server.addr, body.len()).as_bytes())
        .expect("write head");
    conn.flush().expect("flush head");

    // Deliver the body a few bytes at a time, pausing well under the per-read allowance each time. The
    // total transfer takes *longer* than the body deadline, which is exactly what must be allowed: the
    // rule is "do not stall", not "do not take a while".
    let chunk = body.len().div_ceil(5).max(1);
    for piece in body.chunks(chunk) {
        std::thread::sleep(BODY / 3);
        conn.write_all(piece).expect("write a body chunk");
        conn.flush().expect("flush a body chunk");
    }
    let reply = read_reply(&mut conn);

    assert!(
        reply.starts_with("HTTP/1.1 200 OK"),
        "a slow-but-progressing upload was cut off: {reply:?}"
    );
    assert!(
        reply.contains("\"accepted\":1"),
        "the ingest did not complete: {reply:?}"
    );
    assert_eq!(logs_in(&server.db), 1, "the slow upload's row is missing");
}

#[test]
fn a_body_that_stalls_mid_transfer_gets_408() {
    let server = Server::start(
        IoTimeouts {
            header: HEADER,
            body: BODY,
        },
        PATIENCE,
    );
    let mut conn = server.connect();

    let body = otlp_log("cart", "never finished", 1);
    conn.write_all(post_head(&server.addr, body.len() + 64).as_bytes())
        .expect("write head");
    // Announce more than we send, then stop: the server is parked in `read_exact` on the remainder.
    conn.write_all(&body).expect("write a partial body");
    conn.flush().expect("flush");

    let started = Instant::now();
    let reply = read_reply(&mut conn);
    let elapsed = started.elapsed();

    assert!(
        reply.starts_with("HTTP/1.1 408 Request Timeout"),
        "a stalled body was not cut off; got {reply:?}"
    );
    assert!(
        elapsed >= BODY && elapsed < BODY * 8,
        "408 arrived after {elapsed:?}, expected ~{BODY:?}"
    );
    // And nothing was ingested from the truncated request: the body never reached `route`.
    assert_eq!(
        logs_in(&server.db),
        0,
        "a request that never finished was ingested anyway"
    );
}

#[test]
fn disabled_timeouts_leave_a_quiet_connection_alone() {
    // The opt-out: a host fronted by a proxy that already sheds slow clients keeps the old behaviour.
    let server = Server::start(IoTimeouts::DISABLED, Duration::from_millis(200));
    let mut conn = server.connect();
    conn.set_read_timeout(Some(HEADER * 3)).expect("timeout");

    // Nothing is sent, and nothing comes back — no 408, no close.
    let mut buf = [0u8; 32];
    let read = conn.read(&mut buf);
    assert!(
        read.is_err(),
        "a disabled deadline still answered: {:?}",
        read.map(|n| String::from_utf8_lossy(&buf[..n]).into_owned())
    );
    // The server is otherwise healthy on another connection.
    assert_eq!(
        http::get(&server.addr, "/health").expect("health").status,
        200
    );
}

#[test]
fn an_idle_connection_no_longer_holds_the_shutdown_drain_open() {
    // The point of the deadlines for shutdown: the drain waits for connections that are *working*, and
    // an idle one stops being one within the header deadline instead of sitting out the whole drain.
    let drain = Duration::from_secs(30);
    let mut server = Server::start(
        IoTimeouts {
            header: HEADER,
            body: BODY,
        },
        drain,
    );

    let _idle = server.connect();
    // Let the server thread reach `read_line` before shutting down.
    std::thread::sleep(HEADER / 3);

    let started = Instant::now();
    server.stop();
    let elapsed = started.elapsed();
    assert!(
        elapsed < drain / 2,
        "shutdown waited {elapsed:?} on an idle connection — the drain sat out its deadline"
    );
}

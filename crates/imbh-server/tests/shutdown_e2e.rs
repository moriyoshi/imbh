//! Graceful shutdown of the reference server, over a **real loopback socket** and a **real process**.
//!
//! Two layers, because they can fail independently:
//!
//! 1. [`serve_until`] as a library call — a triggered token stops the accept loop *promptly* (the
//!    wake-up connection, not a poll tick), the port is released, in-flight requests are allowed to
//!    finish, and a request that arrives after the trigger is refused rather than half-served.
//! 2. The `imbhd` binary under `SIGTERM` — it exits 0, and the rows it accepted a moment earlier are
//!    **sealed into segments**, which is the whole point of handling the signal: without it those rows
//!    would sit in the WAL waiting for the next start to replay them. Built with `IMBH_FLUSH=manual`
//!    so the only thing that can have sealed them is the shutdown path.
//!
//! Loopback and a temp directory only: no daemon, no network (TESTING.md Layer 1).

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use imbh::Db;
use imbh_server::{Shutdown, serve_until};
use imbh_test_support::http;
use imbh_test_support::otlp::otlp_log;

/// Longest any wait in this test file takes before it is called a failure. Generous for a loaded CI
/// box; the paths under test are all sub-second in practice.
const PATIENCE: Duration = Duration::from_secs(20);

/// Grab a free `127.0.0.1` port by binding `:0` and releasing it. Nothing else races for a loopback
/// ephemeral port in a hermetic test.
fn free_addr() -> String {
    let l = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    l.local_addr().expect("local addr").to_string()
}

/// Poll `/health` until the accept loop answers.
fn await_ready(addr: &str) {
    let deadline = Instant::now() + PATIENCE;
    while Instant::now() < deadline {
        if let Ok(resp) = http::get(addr, "/health")
            && resp.status == 200
        {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("server did not become ready on {addr}");
}

#[test]
fn a_triggered_token_stops_the_accept_loop_and_frees_the_port() {
    let addr = free_addr();
    let db: Arc<Db> = Db::in_memory().open().expect("open in-memory db");
    let shutdown = Shutdown::with_drain_timeout(Duration::from_secs(2));

    let returned = Arc::new(AtomicBool::new(false));
    let server = {
        let (db, addr, shutdown, returned) =
            (db.clone(), addr.clone(), shutdown.clone(), returned.clone());
        std::thread::spawn(move || {
            serve_until(db, &addr, shutdown).expect("serve");
            returned.store(true, Ordering::SeqCst);
        })
    };
    await_ready(&addr);

    // A request accepted before the trigger is served normally.
    let logs = http::post(
        &addr,
        "/v1/logs",
        "application/x-protobuf",
        &otlp_log("cart", "before shutdown", 1),
    )
    .expect("POST /v1/logs");
    assert_eq!(logs.status, 200);

    // Trigger, then require *promptness*: the wake-up connection is what gets the thread out of its
    // blocking `accept`, so this must not need a poll interval — a second is orders of magnitude more
    // slack than the mechanism needs, and hangs here if the wake-up ever regresses.
    let stopped_at = Instant::now();
    shutdown.trigger();
    server.join().expect("the accept loop returns");
    assert!(returned.load(Ordering::SeqCst), "serve_until returned Ok");
    assert!(
        stopped_at.elapsed() < Duration::from_secs(1),
        "shutdown took {:?} — the accept wake-up did not fire",
        stopped_at.elapsed()
    );

    // The listener is gone: the port is bindable again, which is what lets a supervisor restart
    // `imbhd` immediately instead of hitting `AddrInUse`.
    let rebound = TcpListener::bind(&addr);
    assert!(
        rebound.is_ok(),
        "the port is still held after shutdown: {:?}",
        rebound.err()
    );

    // And the DB is intact and queryable — shutting the listener down is not closing the database
    // (that is `main`'s next step, deliberately after every endpoint has stopped).
    let rows = db
        .blocking()
        .sql("SELECT count(*) AS c FROM logs")
        .expect("query after shutdown");
    assert_eq!(count(&rows), 1);
}

/// The single `count(*)` value out of a query result.
fn count(batches: &[imbh::arrow::record_batch::RecordBatch]) -> i64 {
    use imbh::arrow::array::{Array, Int64Array};
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
fn an_in_flight_request_is_allowed_to_finish() {
    let addr = free_addr();
    let db: Arc<Db> = Db::in_memory().open().expect("open in-memory db");
    let shutdown = Shutdown::with_drain_timeout(PATIENCE);
    let server = {
        let (db, addr, shutdown) = (db.clone(), addr.clone(), shutdown.clone());
        std::thread::spawn(move || serve_until(db, &addr, shutdown).expect("serve"))
    };
    await_ready(&addr);

    // Open a connection and send only the head of a request, so the handler thread is parked reading
    // the body when shutdown begins — the case the drain exists for.
    let body = otlp_log("cart", "mid-flight", 2);
    let mut conn = TcpStream::connect(&addr).expect("connect");
    conn.write_all(
        format!(
            "POST /v1/logs HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/x-protobuf\r\n\
             Content-Length: {}\r\n\r\n",
            body.len()
        )
        .as_bytes(),
    )
    .expect("write request head");
    conn.flush().expect("flush");
    // Give the server thread time to be inside `read_exact` on the body.
    std::thread::sleep(Duration::from_millis(100));

    shutdown.trigger();

    // Now finish the request. The drain is still holding the door open, so this must be answered.
    conn.write_all(&body).expect("write body");
    conn.flush().expect("flush body");
    conn.set_read_timeout(Some(PATIENCE)).expect("read timeout");
    let mut response = String::new();
    conn.read_to_string(&mut response).expect("read response");
    assert!(
        response.starts_with("HTTP/1.1 200 OK"),
        "in-flight request was cut off: {response:?}"
    );
    assert!(
        response.contains("\"accepted\":1"),
        "the ingest did not complete: {response:?}"
    );

    server.join().expect("the accept loop returns");
    // The row that was in flight during shutdown is in the DB.
    let rows = db
        .blocking()
        .sql("SELECT count(*) AS c FROM logs WHERE body = 'mid-flight'")
        .expect("query the in-flight row");
    assert_eq!(
        count(&rows),
        1,
        "the drained request's row never reached the DB"
    );
}

#[test]
fn a_connection_arriving_after_the_trigger_is_not_served() {
    let addr = free_addr();
    let db: Arc<Db> = Db::in_memory().open().expect("open in-memory db");
    let shutdown = Shutdown::with_drain_timeout(Duration::ZERO);
    let server = {
        let (db, addr, shutdown) = (db.clone(), addr.clone(), shutdown.clone());
        std::thread::spawn(move || serve_until(db, &addr, shutdown).expect("serve"))
    };
    await_ready(&addr);

    shutdown.trigger();
    server.join().expect("the accept loop returns");

    // Whether the connect itself fails or the read comes back empty depends on the listen backlog's
    // state, and both are correct answers; what must not happen is a 200. A client that gets a refusal
    // retries against the restarted process, which is exactly what an OTLP exporter does.
    match http::get(&addr, "/health") {
        Err(_) => {}
        Ok(resp) => panic!("a stopped server answered with {}", resp.status),
    }
}

/// The binary under a real `SIGTERM`: it must seal and exit 0.
#[cfg(unix)]
#[test]
fn the_binary_seals_its_buffer_on_sigterm() {
    use std::process::{Command, Stdio};

    let dir = tempfile::tempdir().expect("temp dir");
    let data = dir.path().join("data");
    let addr = free_addr();

    let mut child = Command::new(env!("CARGO_BIN_EXE_imbhd"))
        .arg(&data)
        .arg(&addr)
        // `manual` means nothing seals on a timer, so a segment can only exist if the shutdown path
        // sealed it. Without signal handling this test's final assertion is unreachable.
        .env("IMBH_FLUSH", "manual")
        .env("IMBH_SHUTDOWN_TIMEOUT", "5s")
        // Keep the optional endpoints off whatever features the binary was built with: an empty value
        // disables a listener, so a `--features grpc,docker` build does not go looking for the default
        // gRPC port (4317, which may well be taken on a developer's box) or `/run/docker`.
        .env("IMBH_GRPC_LISTEN_ADDR", "")
        .env("IMBH_DOCKER_PLUGIN_SOCKET", "")
        .stdout(Stdio::null())
        .spawn()
        .expect("spawn imbhd");
    await_ready(&addr);

    for i in 0..3 {
        let resp = http::post(
            &addr,
            "/v1/logs",
            "application/x-protobuf",
            &otlp_log("cart", "sealed by SIGTERM", 1_000 + i),
        )
        .expect("POST /v1/logs");
        assert_eq!(resp.status, 200);
    }
    // Nothing is sealed yet: the rows are in the buffer + WAL only.
    let stats = http::get(&addr, "/stats").expect("GET /stats").text();
    assert!(
        stats.contains("\"segment_count\":0"),
        "IMBH_FLUSH=manual should have sealed nothing yet: {stats}"
    );

    // SAFETY: `kill` with a valid pid and signal; the child installs a handler for it at startup.
    let pid = child.id() as libc::pid_t;
    assert_eq!(unsafe { libc::kill(pid, libc::SIGTERM) }, 0, "send SIGTERM");

    // A graceful exit is a *successful* exit — a supervisor must not read `docker stop` as a crash.
    let status = wait_with_timeout(&mut child, PATIENCE);
    assert!(
        status.success(),
        "imbhd exited with {status:?} after SIGTERM (a signal death here means the handler never ran)"
    );

    // The payoff: an independent reader sees the rows in *sealed segments*, so the next start replays
    // nothing.
    let db = Db::open_read_only(&data).expect("reopen the data directory");
    let stats = db.blocking().stats().expect("stats");
    let logs = stats
        .tables
        .iter()
        .find(|t| t.table == imbh::Table::Logs)
        .expect("a logs table entry");
    assert_eq!(
        logs.segment_rows, 3,
        "the shutdown seal lost rows: {logs:?}"
    );
    assert!(
        logs.segment_count > 0,
        "nothing was sealed on shutdown: {logs:?}"
    );
}

/// `Child::wait` with a deadline: poll `try_wait` so a hung `imbhd` fails the test instead of hanging
/// the suite. Kills the child before panicking so a failure leaves no stray process behind.
#[cfg(unix)]
fn wait_with_timeout(
    child: &mut std::process::Child,
    timeout: Duration,
) -> std::process::ExitStatus {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        match child.try_wait().expect("try_wait") {
            Some(status) => return status,
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    panic!("imbhd did not exit within {timeout:?} of SIGTERM");
}

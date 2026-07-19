//! True **two-process** cross-process concurrency test (the cross-process concurrency design lives
//! in `.agents/docs/ARCHITECTURE.md` §7.1).
//!
//! The in-lib tests use two `Db` handles in one process — a valid stand-in because `flock` is
//! per-open-file-description, so a same-process second open contends just like a second process.
//! This test goes the rest of the way: it re-execs the test binary as a **separate OS process**
//! (the writer) and acts as the read-only reader in the parent, validating the things only real
//! processes exercise — cross-process `writer.lock` rejection, page-cache WAL visibility, and no
//! drop / no double-count while a reader queries a directory another process is actively sealing
//! and reclaiming. No network, no daemons, one temp dir (per TESTING.md).
//!
//! Mechanism: the one `#[test]` function branches on `IMBH_XPROC_ROLE`. The parent re-execs
//! `current_exe()` with `--exact <this test>` and that env set to `writer`, so the child runs the
//! writer half and exits; without the env, the process is the reader half. A `.parent-done`
//! sentinel file lets the parent hold the writer alive across the lock-rejection check, then release
//! it — making both the "lock held while writer lives" and "lock free after writer exits" checks
//! deterministic rather than timing-dependent.

use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use imbh::arrow::array::Int64Array;
use imbh::{Db, Result, WalMode};

const ROLE_ENV: &str = "IMBH_XPROC_ROLE";
const DIR_ENV: &str = "IMBH_XPROC_DIR";
const TEST_NAME: &str = "cross_process_writer_and_reader";
/// Total rows the writer ingests; the reader's terminal count must equal this.
const N: u64 = 240;

#[test]
fn cross_process_writer_and_reader() {
    // Child role: be the writer, then exit 0 (a passing test). Detected before any reader assertion.
    if std::env::var(ROLE_ENV).as_deref() == Ok("writer") {
        let dir = std::env::var(DIR_ENV).expect("writer child needs IMBH_XPROC_DIR");
        run_writer(Path::new(&dir));
        return;
    }
    run_reader();
}

/// A minimal OTLP/logs `ExportLogsServiceRequest` with one record, `service.name = service` and the
/// given body/time — the same shape the in-lib tests build, duplicated here because that helper is
/// private to the crate's test module.
fn otlp_log(service: &str, body_text: &str, time: u64) -> Vec<u8> {
    use opentelemetry_proto::tonic::collector::logs::v1::ExportLogsServiceRequest;
    use opentelemetry_proto::tonic::common::v1::{AnyValue as PbAny, KeyValue, any_value};
    use opentelemetry_proto::tonic::logs::v1::{LogRecord, ResourceLogs, ScopeLogs};
    use opentelemetry_proto::tonic::resource::v1::Resource;
    use prost::Message;

    let sv = |s: &str| PbAny {
        value: Some(any_value::Value::StringValue(s.to_owned())),
    };
    ExportLogsServiceRequest {
        resource_logs: vec![ResourceLogs {
            resource: Some(Resource {
                attributes: vec![KeyValue {
                    key: "service.name".to_owned(),
                    value: Some(sv(service)),
                    ..Default::default()
                }],
                ..Default::default()
            }),
            scope_logs: vec![ScopeLogs {
                log_records: vec![LogRecord {
                    time_unix_nano: time,
                    severity_number: 9,
                    body: Some(sv(body_text)),
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        }],
    }
    .encode_to_vec()
}

/// A single-threaded runtime (the crate's `tokio` has no `rt-multi-thread`); each process drives its
/// own on its main thread.
fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap()
}

/// The writer process: own the dir, ingest `N` distinct rows while sealing + reclaiming the WAL
/// mid-stream, then hold the exclusive lock until the parent signals it is done (`.parent-done`), so
/// the parent's lock-held / lock-released checks are deterministic.
fn run_writer(dir: &Path) {
    rt().block_on(async {
        let db = Db::builder(dir)
            .wal(WalMode::Always)
            .open()
            .expect("writer opens the DB and takes the writer.lock");
        for i in 0..N {
            db.ingest_otlp_logs(&otlp_log("svc", &format!("row-{i}"), i + 1))
                .await
                .expect("ingest");
            if i % 30 == 0 {
                // Seal buffer → segment and reclaim covered WAL, so the reader is querying a dir that
                // is concurrently gaining segments and losing WAL bytes.
                db.flush().await.expect("flush");
            }
            // Spread ingest over wall-clock so the reader's polls genuinely interleave with writes.
            std::thread::sleep(Duration::from_millis(2));
        }
        db.flush().await.expect("final flush");

        // Stay alive (holding the lock) until the parent is done with the lock-held check, or bail
        // after a generous timeout so a wedged parent can't hang the child forever.
        let done = dir.join(".parent-done");
        let deadline = Instant::now() + Duration::from_secs(30);
        while !done.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        // Dropping `db` here releases the writer.lock (OS-cleaned on exit regardless).
    });
}

/// The reader process (test parent): spawn the writer child, then read-only-query the shared dir.
fn run_reader() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().to_path_buf();

    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", TEST_NAME, "--nocapture", "--test-threads=1"])
        .env(ROLE_ENV, "writer")
        .env(DIR_ENV, &dir)
        .spawn()
        .expect("spawn writer child process");

    // Guard so a failed assertion (panic) still reaps the child instead of leaking it.
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        rt().block_on(reader_body(&dir));
    }));

    // Whatever happened, let the writer exit: create the sentinel, then wait.
    let _ = std::fs::File::create(dir.join(".parent-done"));
    let status = child.wait().expect("wait for writer child");

    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
    assert!(status.success(), "writer child process failed: {status:?}");

    // The writer has exited, so its lock is released: a fresh read-write open must now succeed.
    let w2 = Db::builder(&dir)
        .wal(WalMode::Always)
        .open()
        .expect("writer.lock is free once the first writer process exited");
    drop(w2);

    // Terminal state on disk: every ingested row is present exactly once.
    let reader = Db::open_read_only(&dir).unwrap();
    let (all, distinct) = rt().block_on(count_logs(&reader)).unwrap();
    assert_eq!(all, N as i64, "final count reflects every ingested row");
    assert_eq!(distinct, N as i64, "no duplicates on disk");
}

async fn reader_body(dir: &Path) {
    // The writer creates the dir + manifest during its own open; retry until the read-only view is
    // available (it errors if the dir isn't a DB yet).
    let reader = open_read_only_retry(dir, Duration::from_secs(15))
        .expect("read-only open of the writer's dir");

    // Wait until the reader (a *separate process*) observes at least one row the writer ingested —
    // proof of cross-process, page-cache WAL visibility, and proof the writer is past `open()` and so
    // definitely holds the lock before we probe it.
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut prev = 0i64;
    loop {
        let (all, distinct) = count_logs(&reader).await.expect("read-only query succeeds");
        assert_eq!(
            distinct, all,
            "reader must not double-count (all == distinct)"
        );
        assert!(
            all >= prev,
            "count went backwards ({prev} -> {all}): a drop across seal/reclaim"
        );
        assert!(all <= N as i64, "count {all} exceeds total ingested {N}");
        prev = all;
        if all >= 1 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "reader never saw the writer's rows"
        );
        std::thread::sleep(Duration::from_millis(5));
    }

    // Cross-process exclusivity: while the writer process lives, a second read-write open is rejected.
    match Db::builder(dir).wal(WalMode::Always).open() {
        Ok(_) => panic!("a second writer must be rejected while the writer process holds the lock"),
        Err(e) => assert!(
            format!("{e}").contains("lock held"),
            "second-writer open should fail with a lock-held error, got: {e}"
        ),
    }

    // Keep reading until the full set is visible, holding the no-drop / no-double-count invariants the
    // whole way — the reader is querying across the writer's live seals + WAL reclaims.
    loop {
        let (all, distinct) = count_logs(&reader).await.expect("read-only query succeeds");
        assert_eq!(
            distinct, all,
            "reader must not double-count (all == distinct)"
        );
        assert!(
            all >= prev,
            "count went backwards ({prev} -> {all}): a drop across seal/reclaim"
        );
        assert!(all <= N as i64, "count {all} exceeds total ingested {N}");
        prev = all;
        if all == N as i64 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "reader never reached the full count ({prev}/{N})"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// `Db::open_read_only`, retried until the writer has initialized the directory (or a timeout).
fn open_read_only_retry(dir: &Path, within: Duration) -> Result<std::sync::Arc<Db>> {
    let deadline = Instant::now() + within;
    loop {
        match Db::open_read_only(dir) {
            Ok(db) => return Ok(db),
            Err(e) if Instant::now() >= deadline => return Err(e),
            Err(_) => std::thread::sleep(Duration::from_millis(10)),
        }
    }
}

/// `(count(*), count(DISTINCT body))` over `logs`. Distinct bodies make the two equal exactly when no
/// row is double-counted.
async fn count_logs(db: &std::sync::Arc<Db>) -> Result<(i64, i64)> {
    let batches = db
        .sql("SELECT count(*) AS n, count(DISTINCT body) AS d FROM logs")
        .collect()
        .await?;
    let b = &batches[0];
    let col = |i: usize| {
        b.column(i)
            .as_any()
            .downcast_ref::<Int64Array>()
            .unwrap()
            .value(0)
    };
    Ok((col(0), col(1)))
}

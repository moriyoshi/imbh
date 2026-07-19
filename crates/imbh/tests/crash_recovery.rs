//! Crash-recovery E2E: a *separate* writer process is hard-killed (SIGKILL) while it holds the DB
//! open, then the directory is reopened and must recover exactly the durable rows from the WAL — no
//! loss, no duplication, and the `writer.lock` is free again (the OS releases the flock on death).
//! This is the crash the WAL exists to survive, exercised as a real process kill rather than the
//! in-process torn-tail unit tests. No production code change is needed; the deterministic
//! *mid-seal* hazard-point kills live in `imbh-storage/tests/crash_points.rs` behind the
//! `fault-injection` feature.

use std::sync::Arc;
use std::time::Duration;

use imbh::{Db, WalMode};
use imbh_test_support::harness;
use imbh_test_support::{count_logs, otlp::otlp_log, rt::ct_rt};

const ROLE_ENV: &str = "IMBH_CRASH_ROLE";
const DIR_ENV: &str = "IMBH_CRASH_DIR";
const TEST_NAME: &str = "crash_recovery_hard_kill_replays_wal";
const READY: &str = ".child-ready";
const N: u64 = 50;

/// Child role: open the DB with `WalMode::Always`, ingest N durable (fsync'd) rows with distinct
/// bodies, signal readiness, then spin forever waiting to be killed. It never seals or closes.
fn run_writer_child(dir: &std::path::Path) -> ! {
    let db: Arc<Db> = Db::builder(dir)
        .wal(WalMode::Always)
        .open()
        .expect("child opens db read-write");
    ct_rt().block_on(async {
        for i in 0..N {
            let receipt = db
                .ingest_otlp_logs(&otlp_log("svc", &format!("msg-{i}"), i + 1))
                .await
                .expect("child ingest");
            assert!(receipt.durable, "WalMode::Always ⇒ durable receipt");
        }
    });
    // All N rows are fsync'd; announce readiness and wait to be SIGKILL'd by the parent.
    harness::touch(&dir.join(READY));
    loop {
        std::thread::sleep(Duration::from_secs(3600));
    }
}

#[test]
fn crash_recovery_hard_kill_replays_wal() {
    // Child branch: this process was re-exec'd as the writer.
    if let Some(role) = harness::role(ROLE_ENV) {
        assert_eq!(role, "writer");
        let dir = std::env::var(DIR_ENV).expect("child dir env");
        run_writer_child(std::path::Path::new(&dir));
    }

    // Parent branch: spawn the writer, let it fill the WAL, then hard-kill it.
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let mut child = harness::spawn_role(TEST_NAME, ROLE_ENV, "writer", DIR_ENV, dir);

    let ready = dir.join(READY);
    let appeared = harness::wait_for(&ready, Duration::from_secs(30));
    // Kill BEFORE asserting readiness so a failed wait can never orphan the child.
    let kill = child.kill();
    let _ = child.wait();
    assert!(appeared, "writer child never signalled readiness");
    kill.expect("SIGKILL the writer child");

    // Reopen read-write: this both proves the lock is free (OS-released on the child's death) and
    // replays the WAL. All N durable rows must come back exactly once.
    let db: Arc<Db> = Db::builder(dir)
        .wal(WalMode::Always)
        .open()
        .expect("reopen after crash (lock must be free)");
    let (count, distinct) = ct_rt().block_on(count_logs(&db));
    assert_eq!(count, N as i64, "every durable row replayed from the WAL");
    assert_eq!(distinct, N as i64, "no duplicated rows after replay");
}

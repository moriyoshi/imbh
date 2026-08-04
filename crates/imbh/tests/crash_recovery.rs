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

// ── duplicate-timestamp guard across a crash (issue #27) ────────────────────────────────────────
//
// The guard is process-local and never persisted: it starts empty at every open and is rebuilt by
// re-running the same rule over the replayed WAL tail. These tests pin the direction that makes
// that safe — the replay guard's key set is always a subset of the writer's, so replay is strictly
// *more* permissive and can never drop a row the writer accepted. Dropping the `Db` without
// `close()` is the crash: with `WalMode::Always` every frame is already fsynced, nothing is sealed,
// and the reopen replays exactly the tail the buffer held.

use imbh::Duplicates;
use imbh_test_support::assert::int_at;
use imbh_test_support::otlp::otlp_sum;

async fn gauge_or_sum_rows(db: &Arc<Db>) -> i64 {
    int_at(
        &db.sql("SELECT count(*) AS c FROM metrics_sum")
            .collect()
            .await
            .expect("count query")[0],
        0,
    )
}

fn open_rejecting(dir: &std::path::Path) -> Arc<Db> {
    Db::builder(dir)
        .wal(WalMode::Always)
        .duplicates(Duplicates::reject())
        .open()
        .expect("open with the duplicate guard")
}

#[test]
fn duplicate_reject_survives_replay() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let rt = ct_rt();

    {
        let db = open_rejecting(dir);
        rt.block_on(async {
            // Two exports at the same instant with *different* values: the second is dropped.
            db.ingest_otlp_metrics(&otlp_sum("cart", "m", 2, &[(10, 1.0)]))
                .await
                .unwrap();
            let r = db
                .ingest_otlp_metrics(&otlp_sum("cart", "m", 2, &[(10, 2.0)]))
                .await
                .unwrap();
            assert_eq!((r.accepted, r.rejected), (0, 1));
            assert_eq!(gauge_or_sum_rows(&db).await, 1);
        });
        // Crash: drop without close(), so nothing is sealed and the WAL tail holds both raw bodies.
    }

    let db = open_rejecting(dir);
    // The WAL kept the *unfiltered* body of the rejected export, so without re-running the guard on
    // replay the reopened DB would show two rows.
    assert_eq!(
        rt.block_on(gauge_or_sum_rows(&db)),
        1,
        "replay must re-apply the guard, not resurrect the rejected point"
    );
}

#[test]
fn replay_never_rejects_what_the_writer_accepted() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let rt = ct_rt();

    let before = {
        let db = open_rejecting(dir);
        rt.block_on(async {
            // Distinct instants either side of a seal. The seal advances the watermark, so the
            // pre-seal points are in a segment and only the post-seal tail is replayed — with a
            // guard that starts empty, which must not drop any of it.
            for ts in 0..5u64 {
                db.ingest_otlp_metrics(&otlp_sum("cart", "m", 2, &[(ts, 1.0)]))
                    .await
                    .unwrap();
            }
            db.flush().await.unwrap();
            for ts in 5..10u64 {
                db.ingest_otlp_metrics(&otlp_sum("cart", "m", 2, &[(ts, 1.0)]))
                    .await
                    .unwrap();
            }
            gauge_or_sum_rows(&db).await
        })
    };
    assert_eq!(before, 10);

    let db = open_rejecting(dir);
    assert_eq!(
        rt.block_on(gauge_or_sum_rows(&db)),
        before,
        "an empty replay guard must drop nothing the writer kept"
    );
}

#[test]
fn duplicate_across_a_seal_boundary_is_re_accepted_after_restart() {
    // The documented permissive drift, pinned so it is not later mistaken for a bug: the guard
    // starts empty at every open, so the first point per series after a restart is always accepted.
    // The read side is what resolves the resulting duplicate.
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let rt = ct_rt();

    {
        let db = open_rejecting(dir);
        rt.block_on(async {
            db.ingest_otlp_metrics(&otlp_sum("cart", "m", 2, &[(10, 1.0)]))
                .await
                .unwrap();
            db.close().await.unwrap();
        });
    }

    let db = open_rejecting(dir);
    let r = rt.block_on(db.ingest_otlp_metrics(&otlp_sum("cart", "m", 2, &[(10, 2.0)])));
    let r = r.unwrap();
    assert_eq!(
        (r.accepted, r.rejected),
        (1, 0),
        "the guard cannot see across a restart; this is best-effort by design"
    );
    assert_eq!(rt.block_on(gauge_or_sum_rows(&db)), 2);
}

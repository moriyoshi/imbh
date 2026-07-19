//! Deterministic mid-seal crash recovery. Compiled only under the `fault-injection` feature (so the
//! default `cargo test --workspace` run skips it entirely); run with:
//!
//! ```sh
//! cargo test -p imbh --features fault-injection --test crash_points
//! ```
//!
//! For each seal hazard point (segment on disk but manifest stale; manifest durable but WAL
//! un-reclaimed), a re-exec'd writer child ingests N durable rows and force-seals; the armed
//! `IMBH_FAULT_ABORT_*` hook `abort()`s the child *inside* `seal()`. The parent then reopens the
//! directory and asserts every row is recovered **exactly once** — no loss, no duplication — proving
//! seal is crash-safe at both hazards (the WAL covers the pre-manifest crash; the watermark makes the
//! post-manifest replay idempotent).
#![cfg(feature = "fault-injection")]

use std::sync::Arc;
use std::time::Duration;

use imbh::{Db, WalMode};
use imbh_test_support::harness;
use imbh_test_support::{count_logs, otlp::otlp_log, rt::ct_rt};

const ROLE_ENV: &str = "IMBH_CRASH_POINT_ROLE";
const DIR_ENV: &str = "IMBH_CRASH_POINT_DIR";
const TEST_NAME: &str = "crash_points_seal_hazards_recover_exactly_once";
const N: u64 = 40;

/// The two `seal()` hazard points, matching the `fault::maybe_abort` calls in imbh-storage.
const HAZARDS: &[&str] = &[
    "IMBH_FAULT_ABORT_BEFORE_MANIFEST",
    "IMBH_FAULT_ABORT_AFTER_MANIFEST",
];

/// Child role: ingest N durable distinct rows, then force-seal. The armed hazard hook aborts the
/// process inside the seal; if no hook fires (feature off / bug), exit 0 so the parent's
/// `!success()` assertion flags it.
fn run_writer_child(dir: &std::path::Path) -> ! {
    let db: Arc<Db> = Db::builder(dir)
        .wal(WalMode::Always)
        .open()
        .expect("child opens db");
    ct_rt().block_on(async {
        for i in 0..N {
            db.ingest_otlp_logs(&otlp_log("svc", &format!("m-{i}"), i + 1))
                .await
                .expect("child ingest");
        }
    });
    // Force-seal: whichever hazard the parent armed aborts us here, mid-seal.
    let _ = ct_rt().block_on(db.flush());
    std::process::exit(0);
}

#[test]
fn crash_points_seal_hazards_recover_exactly_once() {
    // Child branch.
    if harness::role(ROLE_ENV).is_some() {
        let dir = std::env::var(DIR_ENV).expect("child dir env");
        run_writer_child(std::path::Path::new(&dir));
    }

    // Parent branch: one child per hazard, over its own fresh dir.
    for hazard in HAZARDS {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();

        let status = harness::child_command(TEST_NAME)
            .env(ROLE_ENV, "child")
            .env(DIR_ENV, dir)
            .env(hazard, "1")
            .spawn()
            .expect("spawn writer child")
            .wait()
            .expect("await writer child");
        assert!(
            !status.success(),
            "{hazard}: child was expected to abort mid-seal, but exited cleanly ({status:?})"
        );

        // Reopen after the mid-seal crash and require exactly-once recovery.
        let db: Arc<Db> = Db::builder(dir)
            .wal(WalMode::Always)
            .open()
            .expect("reopen after mid-seal crash");
        let (count, distinct) = ct_rt().block_on(count_logs(&db));
        assert_eq!(count, N as i64, "{hazard}: every row recovered");
        assert_eq!(distinct, N as i64, "{hazard}: no duplicated rows");
        drop(db);
        // Give the OS a moment to release the writer.lock before the next iteration's open (defensive;
        // the drop above already released it).
        std::thread::sleep(Duration::from_millis(20));
    }
}

//! Opt-in RSS soak gate (Linux only). Runs a sustained ingest → seal → query loop and asserts the
//! process's steady-state resident set stays under a generous budget — a sentinel for unbounded
//! growth / leaks, closing the "idle/steady/peak RSS unmeasured" gap flagged in QUALITY_GATE.md §2.
//! Kept out of the default `cargo test --workspace` path (it's slow); run explicitly:
//!
//! ```sh
//! cargo test -p imbh --test soak_rss -- --ignored --nocapture
//! ```
#![cfg(target_os = "linux")]

use std::sync::Arc;

use imbh::{Db, WalMode};
use imbh_test_support::procinfo::vm_rss_bytes;
use imbh_test_support::{otlp::otlp_log, rt::ct_rt};

const ROUNDS: u64 = 20;
const ROWS_PER_ROUND: u64 = 1_000;
/// Compact every Nth round (not every round) so the soak stays bounded without O(rounds²) merge cost.
const COMPACT_EVERY: u64 = 5;
/// Generous headroom over the ~36 MB Appendix C baseline: high enough to avoid flakiness from the
/// DataFusion pool / Tantivy mmaps, low enough that a genuine leak (unbounded per-round growth) trips
/// it. Tune here if the steady figure printed below drifts.
const BUDGET_BYTES: u64 = 512 * 1024 * 1024;
/// Ceiling for peak RSS (`VmHWM`, the process high-water mark). `VmHWM >= VmRSS` always, so the peak
/// budget must sit above the steady one; 1.5× the steady budget gives room for transient spikes (a
/// compaction merge, a query's DataFusion pool) that retreat before `steady` is sampled, while still
/// tripping on unbounded growth. Note this is a debug/glibc `VmHWM` that *includes* file-backed
/// Parquet/Tantivy mmap pages (OVERVIEW.md §2 budgets anonymous RSS), so it is an upper-bound proxy,
/// not the exact anonymous peak. Tune here if the VmHWM figure printed below drifts.
const PEAK_BUDGET_BYTES: u64 = 768 * 1024 * 1024;

#[test]
#[ignore = "RSS soak: run explicitly with --ignored"]
fn steady_state_rss_stays_within_budget() {
    let tmp = tempfile::tempdir().unwrap();
    let db: Arc<Db> = Db::builder(tmp.path()).wal(WalMode::Always).open().unwrap();
    let rt = ct_rt();

    let idle = vm_rss_bytes().expect("VmRSS readable on Linux");

    rt.block_on(async {
        let mut seq = 0u64;
        for round in 0..ROUNDS {
            for _ in 0..ROWS_PER_ROUND {
                seq += 1;
                db.ingest_otlp_logs(&otlp_log("svc", &format!("m-{seq}"), seq))
                    .await
                    .expect("ingest");
            }
            db.flush().await.unwrap(); // seal to a Parquet segment (+ .tidx sidecar)
            // A representative query each round keeps the read path (DataFusion pool, Tantivy) warm.
            let _ = db
                .sql("SELECT count(*) FROM logs WHERE matches(body, 'm')")
                .collect()
                .await
                .unwrap();
            // Periodically merge segments so segment count stays bounded (steady state, not growth).
            if round % COMPACT_EVERY == COMPACT_EVERY - 1 {
                db.compact().await.unwrap();
            }
        }
    });

    let steady = vm_rss_bytes().expect("VmRSS readable on Linux");
    let peak = vm_hwm_bytes().expect("VmHWM readable on Linux");
    let total = ROUNDS * ROWS_PER_ROUND;
    println!(
        "SOAK_RSS idle={} MiB steady={} MiB peak={} MiB rows={} budget={} MiB peak_budget={} MiB",
        idle >> 20,
        steady >> 20,
        peak >> 20,
        total,
        BUDGET_BYTES >> 20,
        PEAK_BUDGET_BYTES >> 20
    );

    rt.block_on(db.close()).unwrap();
    assert!(
        steady < BUDGET_BYTES,
        "steady RSS {} MiB exceeded budget {} MiB after {} rows (possible leak)",
        steady >> 20,
        BUDGET_BYTES >> 20,
        total
    );
    // Peak (high-water) RSS must stay bounded too: a transient spike that unwinds before `steady` is
    // sampled would slip past the steady assertion, so gate the run-lifetime maximum explicitly.
    assert!(
        peak < PEAK_BUDGET_BYTES,
        "peak RSS (VmHWM) {} MiB exceeded peak budget {} MiB after {} rows (unbounded spike)",
        peak >> 20,
        PEAK_BUDGET_BYTES >> 20,
        total
    );
    // Cheap invariant: the high-water mark is by definition >= any current sample, including the
    // steady one we just read.
    assert!(
        peak >= steady,
        "VmHWM {} MiB < VmRSS {} MiB — impossible; /proc parse likely wrong",
        peak >> 20,
        steady >> 20
    );
}

/// Peak resident set size in bytes from `/proc/self/status` (`VmHWM`), or `None` when unavailable.
/// Mirrors `vm_rss_bytes` in `imbh-test-support`; `VmHWM` is the high-water mark since process start.
fn vm_hwm_bytes() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmHWM:") {
            // "VmHWM:  \t   12345 kB" — value is kiB despite the `kB` label (kernel quirk).
            let kb: u64 = rest.split_whitespace().next()?.parse().ok()?;
            return Some(kb * 1024);
        }
    }
    None
}

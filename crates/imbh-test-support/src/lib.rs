//! Shared test support for imbh's integration / E2E suites.
//!
//! This crate is **dev-only**: it is pulled in exclusively through the `[dev-dependencies]` of the
//! test crates, so it never enters the shipping `imbh`/`imbhd` graph (zero footprint — see
//! ARCHITECTURE.md §11/§12 and .agents/docs/TESTING.md). It consolidates helpers that were
//! previously copy-pasted across per-crate `#[cfg(test)]` modules:
//!
//! - [`otlp`] — OTLP protobuf builders returning encoded request bytes.
//! - [`http`] — a tiny blocking HTTP/1.1 client for driving the reference `imbhd` server.
//! - [`harness`] — a re-exec harness for multi-process tests (crash / cross-process).
//! - [`assert`] / [`rt`] / [`procinfo`] — result assertions, a current-thread runtime, and a
//!   `VmRSS` reader (all re-exported below).

pub mod harness;
pub mod http;
pub mod otlp;

/// Result-assertion helpers over query output.
pub mod assert {
    use imbh::arrow::array::{Array, Int64Array};
    use imbh::arrow::record_batch::RecordBatch;

    /// The `i64` at row 0 of column `col` — the standard `SELECT count(*)`/aggregate extractor.
    pub fn int_at(batch: &RecordBatch, col: usize) -> i64 {
        batch
            .column(col)
            .as_any()
            .downcast_ref::<Int64Array>()
            .expect("Int64 column")
            .value(0)
    }
}

/// A current-thread tokio runtime for driving the async `Db` API from a synchronous test (the
/// facade's tokio has no `rt-multi-thread`, so this is the established idiom). `enable_time()` is on
/// for the timers the query builders' `step` fields need.
pub mod rt {
    /// Build a fresh current-thread runtime with timers enabled.
    pub fn ct_rt() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("build current-thread runtime")
    }
}

/// Query the `logs` table for `(count(*), count(DISTINCT body))` — the no-drop/no-double-count
/// invariant pair used across the concurrency and crash tests. Async; drive it with [`rt::ct_rt`].
pub async fn count_logs(db: &std::sync::Arc<imbh::Db>) -> (i64, i64) {
    let batches = db
        .sql("SELECT count(*) AS n, count(DISTINCT body) AS d FROM logs")
        .collect()
        .await
        .expect("count logs query");
    (
        assert::int_at(&batches[0], 0),
        assert::int_at(&batches[0], 1),
    )
}

/// Reader-side process memory. Linux-only (`/proc/self/status`); `None` elsewhere or on parse
/// failure. Reused by the opt-in RSS soak gate (pattern from `examples/rss-probe`).
pub mod procinfo {
    /// Resident set size in bytes from `/proc/self/status` (`VmRSS`), or `None` when unavailable.
    #[cfg(target_os = "linux")]
    pub fn vm_rss_bytes() -> Option<u64> {
        let status = std::fs::read_to_string("/proc/self/status").ok()?;
        for line in status.lines() {
            if let Some(rest) = line.strip_prefix("VmRSS:") {
                // "VmRSS:  \t   12345 kB"
                let kb: u64 = rest.split_whitespace().next()?.parse().ok()?;
                return Some(kb * 1024);
            }
        }
        None
    }

    /// Non-Linux fallback: RSS reporting is not wired up, so callers must self-skip.
    #[cfg(not(target_os = "linux"))]
    pub fn vm_rss_bytes() -> Option<u64> {
        None
    }
}

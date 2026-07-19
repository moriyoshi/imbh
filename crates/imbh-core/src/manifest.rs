//! Manifest types (ARCHITECTURE.md §7/§12). `imbh-core` owns the *types*; `imbh-storage` owns the
//! IO (load/persist). M0 uses a naive whole-file manifest; M1 replaces it with the
//! append-only delta log + compacted checkpoint.

/// A reference to one immutable sealed segment. The manifest — never a directory scan — is
/// the source of truth for what is queryable (ARCHITECTURE.md §7). `[min_time, max_time]` drives
/// time-window pruning; ranges may overlap under out-of-order ingest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentRef {
    /// Path relative to the DB directory, e.g. `logs/2026-07-18/01J....parquet`.
    pub relative_path: String,
    pub min_time_unix_nano: i64,
    pub max_time_unix_nano: i64,
    pub rows: u64,
}

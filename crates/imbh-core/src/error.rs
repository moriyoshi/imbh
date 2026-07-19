//! The imbh error model (ARCHITECTURE.md §10.3).
//!
//! One [`Error`] with typed category variants — `Open`/`Ingest`/`Query`/`Storage`/`Config`, each
//! wrapping a typed leaf `*Kind` plus the chained backing error (surfaced through
//! [`std::error::Error::source`]) — and a terminal `Closed`. Categories and leaves are
//! `#[non_exhaustive]` so detail can grow without a breaking change (§10.14).
//!
//! The `is_backpressure`/`is_not_found`/`is_user_error` classifiers are the stable public contract a
//! server maps to HTTP status; they match the typed leaves exactly, never error text. Build errors
//! through the constructor helpers (`Error::query_plan`, `Error::column_type`, `Error::storage_io`,
//! …) — the leaf enums are `#[non_exhaustive]`, so external code matches on them but does not
//! construct them directly.

use std::fmt;
use std::path::PathBuf;

use crate::enums::Signal;

pub type Result<T> = std::result::Result<T, Error>;

/// The boxed backing error every category can carry. `Send + Sync + 'static` so an [`Error`] can
/// cross async/thread boundaries and be wrapped by DataFusion's `External` variant.
type Src = Box<dyn std::error::Error + Send + Sync>;

/// The top-level imbh error (ARCHITECTURE.md §10.3). `#[non_exhaustive]` so leaf detail can grow
/// without a breaking change (§10.14).
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// DB open / recovery failure (lockfile, corrupt manifest, unsupported WAL frame, I/O).
    Open(OpenError),
    /// Ingest failure (OTLP decode, buffer backpressure).
    Ingest(IngestError),
    /// Query planning or execution failure.
    Query(QueryError),
    /// Storage-engine failure (seal, segment I/O, WAL, Parquet, manifest).
    Storage(StorageError),
    /// Invalid configuration.
    Config(ConfigError),
    /// The handle (or one of its clones) has been closed.
    Closed,
}

// ── Category payloads: a typed leaf `kind` plus the chained backing error ────────────────────────

/// DB-open / recovery failure detail.
#[derive(Debug)]
#[non_exhaustive]
pub struct OpenError {
    pub kind: OpenKind,
    source: Option<Src>,
}

/// Ingest failure detail.
#[derive(Debug)]
#[non_exhaustive]
pub struct IngestError {
    pub kind: IngestKind,
    source: Option<Src>,
}

/// Query planning / execution failure detail.
#[derive(Debug)]
#[non_exhaustive]
pub struct QueryError {
    pub kind: QueryKind,
    source: Option<Src>,
}

/// Storage-engine failure detail.
#[derive(Debug)]
#[non_exhaustive]
pub struct StorageError {
    pub kind: StorageKind,
    source: Option<Src>,
}

/// Configuration failure detail.
#[derive(Debug)]
#[non_exhaustive]
pub struct ConfigError {
    pub kind: ConfigKind,
    source: Option<Src>,
}

// ── Leaf kinds ───────────────────────────────────────────────────────────────────────────────────

/// What went wrong while opening / recovering a DB.
#[derive(Debug)]
#[non_exhaustive]
pub enum OpenKind {
    /// The on-disk manifest did not parse. `line` is the 1-based line when known.
    CorruptManifest { line: Option<usize>, detail: String },
    /// A WAL frame carried a signal tag the build does not understand.
    UnsupportedWalSignal { lsn: u64, signal: u8 },
    /// The single-writer lock is held by another process (reserved; no site yet).
    LockHeld { path: PathBuf },
    /// Any other open-path failure (WAL/manifest I/O), with the cause in `source()`.
    Message(String),
}

/// What went wrong while ingesting.
#[derive(Debug)]
#[non_exhaustive]
pub enum IngestKind {
    /// An OTLP protobuf export request failed to decode.
    Decode { signal: Signal },
    /// The mutable buffer is at its byte cap; the caller should retry/await (backpressure).
    QueueFull { queued: usize, cap: usize },
    /// Any other ingest failure.
    Message(String),
}

/// What went wrong while planning or executing a query.
#[derive(Debug)]
#[non_exhaustive]
pub enum QueryKind {
    /// A referenced table does not exist (→ `is_not_found`).
    UnknownTable { table: String },
    /// A referenced column does not exist (→ `is_not_found`).
    UnknownColumn { column: String },
    /// A result column had an unexpected Arrow type. `actual` is filled when known.
    ColumnType {
        column: String,
        expected: String,
        actual: Option<String>,
    },
    /// A buffer↔segment schema coercion failed (internal — deliberately NOT `UnknownColumn`, so a
    /// schema mismatch is never mis-classified as not-found). `column` names the offending column.
    Coerce { column: Option<String> },
    /// DataFusion logical/physical planning failed (cause in `source()`).
    Plan,
    /// DataFusion execution failed (cause in `source()`).
    Execute,
    /// A full-text (Tantivy) search failed. `doc_id` is set for a missing-row-ordinal defect.
    Search { doc_id: Option<u64> },
    /// Any other query failure.
    Message(String),
}

/// A WAL I/O phase (for `StorageKind::Wal`).
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub enum WalPhase {
    Append,
    Fsync,
    Rotate,
    Remove,
    DirFsync,
}

/// A Parquet-writer phase (for `StorageKind::Parquet`).
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub enum ParquetPhase {
    Writer,
    Write,
    Close,
    Fsync,
}

/// What went wrong in the storage engine.
#[derive(Debug)]
#[non_exhaustive]
pub enum StorageKind {
    /// Segment / directory I/O (and the blanket `From<std::io::Error>`). `path` when known; cause in
    /// `source()`.
    Io { path: Option<PathBuf> },
    /// A write-ahead-log operation failed at `phase` (cause in `source()`).
    Wal { phase: WalPhase },
    /// A Parquet write failed at `phase` (cause in `source()`).
    Parquet { phase: ParquetPhase },
    /// A WAL frame payload exceeded the frame limit.
    PayloadTooLarge { len: usize, limit: u64 },
    /// The configured zstd level is out of range (cause in `source()`).
    InvalidZstdLevel { level: i32 },
    /// Building the Arrow `RecordBatch` for a table failed (cause in `source()`).
    BuildBatch { table: String },
    /// An expected column was absent from a batch.
    MissingColumn { column: String },
    /// Any other storage failure.
    Message(String),
}

/// What was wrong with the configuration.
#[derive(Debug)]
#[non_exhaustive]
pub enum ConfigKind {
    /// A durable DB was opened without a path.
    MissingDatabasePath,
    /// A write (ingest/flush/maintain/compact/snapshot) was attempted on a read-only handle.
    ReadOnly,
    /// A read-only open was requested against a writer whose WAL is disabled, so the reader cannot
    /// get near-real-time freshness (only seal-interval). Opt in via the builder to accept it.
    ReaderWalDisabled,
    /// Any other config failure.
    Message(String),
}

// ── Display ──────────────────────────────────────────────────────────────────────────────────────

fn write_category(
    f: &mut fmt::Formatter<'_>,
    label: &str,
    kind: &dyn fmt::Display,
    source: &Option<Src>,
) -> fmt::Result {
    write!(f, "{label} error: {kind}")?;
    if let Some(s) = source {
        write!(f, ": {s}")?;
    }
    Ok(())
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Open(e) => write_category(f, "open", &e.kind, &e.source),
            Error::Ingest(e) => write_category(f, "ingest", &e.kind, &e.source),
            Error::Query(e) => write_category(f, "query", &e.kind, &e.source),
            Error::Storage(e) => write_category(f, "storage", &e.kind, &e.source),
            Error::Config(e) => write_category(f, "config", &e.kind, &e.source),
            Error::Closed => write!(f, "database is closed"),
        }
    }
}

impl fmt::Display for OpenKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OpenKind::CorruptManifest {
                line: Some(l),
                detail,
            } => write!(f, "corrupt manifest at line {l}: {detail}"),
            OpenKind::CorruptManifest { line: None, detail } => {
                write!(f, "corrupt manifest: {detail}")
            }
            OpenKind::UnsupportedWalSignal { lsn, signal } => {
                write!(f, "WAL record {lsn} has unsupported signal tag {signal}")
            }
            OpenKind::LockHeld { path } => write!(f, "database lock held: {}", path.display()),
            OpenKind::Message(m) => f.write_str(m),
        }
    }
}

impl fmt::Display for IngestKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IngestKind::Decode { signal } => {
                let s = match signal {
                    Signal::Logs => "logs",
                    Signal::Traces => "traces",
                    Signal::Metrics => "metrics",
                };
                write!(f, "OTLP/{s} protobuf decode failed")
            }
            IngestKind::QueueFull { queued, cap } => {
                write!(f, "ingest queue full: {queued} of {cap} bytes buffered")
            }
            IngestKind::Message(m) => f.write_str(m),
        }
    }
}

impl fmt::Display for QueryKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            QueryKind::UnknownTable { table } => write!(f, "unknown table `{table}`"),
            QueryKind::UnknownColumn { column } => write!(f, "unknown column `{column}`"),
            QueryKind::ColumnType {
                column,
                expected,
                actual: Some(actual),
            } => write!(
                f,
                "column `{column}` has type {actual}, expected {expected}"
            ),
            QueryKind::ColumnType {
                column,
                expected,
                actual: None,
            } => write!(f, "column `{column}` is not {expected}"),
            QueryKind::Coerce { column: Some(c) } => write!(f, "coerce column `{c}`"),
            QueryKind::Coerce { column: None } => write!(f, "coerce batch"),
            QueryKind::Plan => f.write_str("plan"),
            QueryKind::Execute => f.write_str("execute"),
            QueryKind::Search { doc_id: Some(d) } => {
                write!(f, "full-text search: hit missing row ordinal (doc {d})")
            }
            QueryKind::Search { doc_id: None } => f.write_str("full-text search"),
            QueryKind::Message(m) => f.write_str(m),
        }
    }
}

impl fmt::Display for WalPhase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            WalPhase::Append => "append",
            WalPhase::Fsync => "fsync",
            WalPhase::Rotate => "rotate",
            WalPhase::Remove => "remove",
            WalPhase::DirFsync => "dir fsync",
        })
    }
}

impl fmt::Display for ParquetPhase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            ParquetPhase::Writer => "writer",
            ParquetPhase::Write => "write",
            ParquetPhase::Close => "close",
            ParquetPhase::Fsync => "fsync",
        })
    }
}

impl fmt::Display for StorageKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StorageKind::Io { path: Some(p) } => write!(f, "I/O error at {}", p.display()),
            StorageKind::Io { path: None } => f.write_str("I/O error"),
            StorageKind::Wal { phase } => write!(f, "WAL {phase}"),
            StorageKind::Parquet { phase } => write!(f, "Parquet {phase}"),
            StorageKind::PayloadTooLarge { len, limit } => write!(
                f,
                "WAL payload too large: {len} bytes exceeds the {limit}-byte frame limit"
            ),
            StorageKind::InvalidZstdLevel { level } => write!(f, "invalid zstd level {level}"),
            StorageKind::BuildBatch { table } => write!(f, "build {table} batch"),
            StorageKind::MissingColumn { column } => write!(f, "missing `{column}` column"),
            StorageKind::Message(m) => f.write_str(m),
        }
    }
}

impl fmt::Display for ConfigKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigKind::MissingDatabasePath => f.write_str("no database path provided"),
            ConfigKind::ReadOnly => {
                f.write_str("database opened read-only: writes are not allowed")
            }
            ConfigKind::ReaderWalDisabled => f.write_str(
                "read-only open rejected: the writer's WAL is disabled, so a reader gets only \
                 seal-interval freshness, not near-real-time; call allow_stale_reads() to accept it",
            ),
            ConfigKind::Message(m) => f.write_str(m),
        }
    }
}

// ── source() chaining ────────────────────────────────────────────────────────────────────────────

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        let source: &Option<Src> = match self {
            Error::Open(e) => &e.source,
            Error::Ingest(e) => &e.source,
            Error::Query(e) => &e.source,
            Error::Storage(e) => &e.source,
            Error::Config(e) => &e.source,
            Error::Closed => return None,
        };
        source
            .as_deref()
            .map(|s| s as &(dyn std::error::Error + 'static))
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::storage_io(None, e)
    }
}

// ── Classifiers (stable public contract; a server maps these to HTTP status) ─────────────────────

impl Error {
    /// Ingest backpressure — the caller should retry/await (ARCHITECTURE.md §10.3). True for an
    /// `Ingest` error whose leaf is [`IngestKind::QueueFull`].
    pub fn is_backpressure(&self) -> bool {
        matches!(self, Error::Ingest(e) if matches!(e.kind, IngestKind::QueueFull { .. }))
    }

    /// Maps to HTTP 404 (ARCHITECTURE.md §10.3). True for a `Query` error whose leaf is an
    /// unknown table/column. imbh's "absent" reads return `Ok(None)`, so this is rarely hit today.
    pub fn is_not_found(&self) -> bool {
        matches!(
            self,
            Error::Query(e)
                if matches!(e.kind, QueryKind::UnknownTable { .. } | QueryKind::UnknownColumn { .. })
        )
    }

    /// The 4xx vs 5xx split a server needs (ARCHITECTURE.md §10.3): user errors are bad ingest
    /// payloads, bad queries, and bad config; open/storage/closed are server-side (5xx).
    pub fn is_user_error(&self) -> bool {
        matches!(self, Error::Ingest(_) | Error::Query(_) | Error::Config(_))
    }
}

// ── Constructors (make the migration mechanical; the leaves stay non-constructible externally) ───

impl Error {
    // Open ------------------------------------------------------------------------------------------
    /// An open-path failure with a message and no typed leaf.
    pub fn open_msg(message: impl Into<String>) -> Self {
        Error::Open(OpenError {
            kind: OpenKind::Message(message.into()),
            source: None,
        })
    }
    /// An open-path failure carrying a message and its cause.
    pub fn open_ctx(message: impl Into<String>, source: impl Into<Src>) -> Self {
        Error::Open(OpenError {
            kind: OpenKind::Message(message.into()),
            source: Some(source.into()),
        })
    }
    /// The manifest failed to parse (`line` 1-based when known).
    pub fn corrupt_manifest(line: Option<usize>, detail: impl Into<String>) -> Self {
        Error::Open(OpenError {
            kind: OpenKind::CorruptManifest {
                line,
                detail: detail.into(),
            },
            source: None,
        })
    }
    /// A WAL frame carried an unknown signal tag.
    pub fn unsupported_wal_signal(lsn: u64, signal: u8) -> Self {
        Error::Open(OpenError {
            kind: OpenKind::UnsupportedWalSignal { lsn, signal },
            source: None,
        })
    }
    /// The single-writer lock is held (reserved).
    pub fn lock_held(path: impl Into<PathBuf>) -> Self {
        Error::Open(OpenError {
            kind: OpenKind::LockHeld { path: path.into() },
            source: None,
        })
    }

    // Ingest ----------------------------------------------------------------------------------------
    /// An OTLP protobuf decode failure for `signal`, carrying the prost cause.
    pub fn ingest_decode(signal: Signal, source: impl Into<Src>) -> Self {
        Error::Ingest(IngestError {
            kind: IngestKind::Decode { signal },
            source: Some(source.into()),
        })
    }
    /// The mutable buffer is at its byte cap (backpressure).
    pub fn queue_full(queued: usize, cap: usize) -> Self {
        Error::Ingest(IngestError {
            kind: IngestKind::QueueFull { queued, cap },
            source: None,
        })
    }
    /// Any other ingest failure.
    pub fn ingest_msg(message: impl Into<String>) -> Self {
        Error::Ingest(IngestError {
            kind: IngestKind::Message(message.into()),
            source: None,
        })
    }

    // Query -----------------------------------------------------------------------------------------
    /// A referenced table does not exist (→ `is_not_found`).
    pub fn unknown_table(table: impl Into<String>) -> Self {
        Error::Query(QueryError {
            kind: QueryKind::UnknownTable {
                table: table.into(),
            },
            source: None,
        })
    }
    /// A referenced column does not exist (→ `is_not_found`).
    pub fn unknown_column(column: impl Into<String>) -> Self {
        Error::Query(QueryError {
            kind: QueryKind::UnknownColumn {
                column: column.into(),
            },
            source: None,
        })
    }
    /// A result column had an unexpected Arrow type.
    pub fn column_type(
        column: impl Into<String>,
        expected: impl Into<String>,
        actual: Option<String>,
    ) -> Self {
        Error::Query(QueryError {
            kind: QueryKind::ColumnType {
                column: column.into(),
                expected: expected.into(),
                actual,
            },
            source: None,
        })
    }
    /// A coerce failure for a named column, carrying the cause.
    pub fn coerce(column: Option<String>, source: impl Into<Src>) -> Self {
        Error::Query(QueryError {
            kind: QueryKind::Coerce { column },
            source: Some(source.into()),
        })
    }
    /// A coerce failure because a source batch lacked a column (no cause).
    pub fn coerce_missing(column: impl Into<String>) -> Self {
        Error::Query(QueryError {
            kind: QueryKind::Coerce {
                column: Some(column.into()),
            },
            source: None,
        })
    }
    /// DataFusion planning failed, carrying the cause.
    pub fn query_plan(source: impl Into<Src>) -> Self {
        Error::Query(QueryError {
            kind: QueryKind::Plan,
            source: Some(source.into()),
        })
    }
    /// DataFusion execution failed, carrying the cause.
    pub fn query_execute(source: impl Into<Src>) -> Self {
        Error::Query(QueryError {
            kind: QueryKind::Execute,
            source: Some(source.into()),
        })
    }
    /// A full-text search failed, carrying the Tantivy cause.
    pub fn query_search(source: impl Into<Src>) -> Self {
        Error::Query(QueryError {
            kind: QueryKind::Search { doc_id: None },
            source: Some(source.into()),
        })
    }
    /// A full-text hit was missing its row ordinal (an index defect).
    pub fn search_missing_row(doc_id: u64) -> Self {
        Error::Query(QueryError {
            kind: QueryKind::Search {
                doc_id: Some(doc_id),
            },
            source: None,
        })
    }
    /// Any other query failure with a message and no cause.
    pub fn query_msg(message: impl Into<String>) -> Self {
        Error::Query(QueryError {
            kind: QueryKind::Message(message.into()),
            source: None,
        })
    }
    /// Any other query failure with a message and a cause.
    pub fn query_ctx(message: impl Into<String>, source: impl Into<Src>) -> Self {
        Error::Query(QueryError {
            kind: QueryKind::Message(message.into()),
            source: Some(source.into()),
        })
    }

    // Storage ---------------------------------------------------------------------------------------
    /// Segment / directory I/O, carrying the io cause and the path when known.
    pub fn storage_io(path: Option<PathBuf>, source: impl Into<Src>) -> Self {
        Error::Storage(StorageError {
            kind: StorageKind::Io { path },
            source: Some(source.into()),
        })
    }
    /// A WAL op failed at `phase`, carrying the cause.
    pub fn wal(phase: WalPhase, source: impl Into<Src>) -> Self {
        Error::Storage(StorageError {
            kind: StorageKind::Wal { phase },
            source: Some(source.into()),
        })
    }
    /// A Parquet write failed at `phase`, carrying the cause.
    pub fn parquet(phase: ParquetPhase, source: impl Into<Src>) -> Self {
        Error::Storage(StorageError {
            kind: StorageKind::Parquet { phase },
            source: Some(source.into()),
        })
    }
    /// A WAL frame payload exceeded the limit.
    pub fn payload_too_large(len: usize, limit: u64) -> Self {
        Error::Storage(StorageError {
            kind: StorageKind::PayloadTooLarge { len, limit },
            source: None,
        })
    }
    /// The configured zstd level was invalid, carrying the cause.
    pub fn invalid_zstd_level(level: i32, source: impl Into<Src>) -> Self {
        Error::Storage(StorageError {
            kind: StorageKind::InvalidZstdLevel { level },
            source: Some(source.into()),
        })
    }
    /// Building the Arrow batch for `table` failed, carrying the cause.
    pub fn build_batch(table: impl Into<String>, source: impl Into<Src>) -> Self {
        Error::Storage(StorageError {
            kind: StorageKind::BuildBatch {
                table: table.into(),
            },
            source: Some(source.into()),
        })
    }
    /// An expected column was absent, carrying the cause.
    pub fn missing_column(column: impl Into<String>, source: impl Into<Src>) -> Self {
        Error::Storage(StorageError {
            kind: StorageKind::MissingColumn {
                column: column.into(),
            },
            source: Some(source.into()),
        })
    }
    /// Any other storage failure with a message and no cause.
    pub fn storage_msg(message: impl Into<String>) -> Self {
        Error::Storage(StorageError {
            kind: StorageKind::Message(message.into()),
            source: None,
        })
    }
    /// Any other storage failure with a message and a cause.
    pub fn storage_ctx(message: impl Into<String>, source: impl Into<Src>) -> Self {
        Error::Storage(StorageError {
            kind: StorageKind::Message(message.into()),
            source: Some(source.into()),
        })
    }

    // Config ----------------------------------------------------------------------------------------
    /// A durable DB was opened without a path.
    pub fn missing_database_path() -> Self {
        Error::Config(ConfigError {
            kind: ConfigKind::MissingDatabasePath,
            source: None,
        })
    }
    /// A write was attempted on a read-only handle (→ `is_user_error`).
    pub fn read_only() -> Self {
        Error::Config(ConfigError {
            kind: ConfigKind::ReadOnly,
            source: None,
        })
    }
    /// A read-only open was rejected because the writer's WAL is disabled (→ `is_user_error`).
    pub fn reader_wal_disabled() -> Self {
        Error::Config(ConfigError {
            kind: ConfigKind::ReaderWalDisabled,
            source: None,
        })
    }
    /// Any other config failure.
    pub fn config_msg(message: impl Into<String>) -> Self {
        Error::Config(ConfigError {
            kind: ConfigKind::Message(message.into()),
            source: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn io_err() -> std::io::Error {
        std::io::Error::other("disk full")
    }

    #[test]
    fn classifiers() {
        // is_user_error: Ingest / Query / Config true; Open / Storage / Closed false.
        assert!(Error::ingest_msg("bad OTLP").is_user_error());
        assert!(Error::query_msg("parse error").is_user_error());
        assert!(Error::missing_database_path().is_user_error());
        assert!(!Error::storage_msg("disk full").is_user_error());
        assert!(!Error::open_msg("lock held").is_user_error());
        assert!(!Error::Closed.is_user_error());

        // is_backpressure keys off the typed QueueFull leaf, not error text.
        assert!(Error::queue_full(10, 10).is_backpressure());
        assert!(!Error::ingest_decode(Signal::Logs, io_err()).is_backpressure());
        assert!(!Error::ingest_msg("QueueFull").is_backpressure()); // no substring hack anymore

        // is_not_found keys off UnknownTable/UnknownColumn.
        assert!(Error::unknown_table("logs").is_not_found());
        assert!(Error::unknown_column("service").is_not_found());
        assert!(!Error::storage_msg("x").is_not_found());
        // A coerce schema mismatch is NOT not-found (guards the deliberate Coerce != UnknownColumn split).
        assert!(!Error::coerce_missing("service").is_not_found());
    }

    #[test]
    fn source_chaining() {
        let e: Error = io_err().into();
        assert!(!e.is_user_error(), "io error is a storage (5xx) error");
        let src = std::error::Error::source(&e).expect("io error chained as source");
        assert!(src.downcast_ref::<std::io::Error>().is_some());

        let e = Error::storage_io(Some(PathBuf::from("/x/seg.parquet")), io_err());
        assert!(
            std::error::Error::source(&e)
                .and_then(|s| s.downcast_ref::<std::io::Error>())
                .is_some()
        );

        // No source → None.
        assert!(std::error::Error::source(&Error::Closed).is_none());
        assert!(std::error::Error::source(&Error::queue_full(1, 1)).is_none());
    }

    #[test]
    fn display() {
        assert_eq!(Error::Closed.to_string(), "database is closed");

        // Message + appended source: reproduces the old "query error: plan: <cause>" shape.
        let e = Error::query_plan(io_err());
        let s = e.to_string();
        assert!(s.starts_with("query error: plan"), "{s}");
        assert!(s.contains("disk full"), "{s}");

        // Structured fields render into the message.
        let e = Error::payload_too_large(5 << 30, u32::MAX as u64);
        let s = e.to_string();
        assert!(s.contains(&(5u64 << 30).to_string()), "{s}");
        assert!(s.contains(&(u32::MAX as u64).to_string()), "{s}");

        let e = Error::column_type("value", "Float64", Some("Int64".to_owned()));
        assert_eq!(
            e.to_string(),
            "query error: column `value` has type Int64, expected Float64"
        );

        assert_eq!(
            Error::unsupported_wal_signal(7, 9).to_string(),
            "open error: WAL record 7 has unsupported signal tag 9"
        );
    }

    #[test]
    fn error_is_send_sync() {
        fn assert_send_sync<T: Send + Sync + 'static>() {}
        assert_send_sync::<Error>();
    }
}

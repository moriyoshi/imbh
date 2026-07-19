//! imbh storage engine (ARCHITECTURE.md §7).
//!
//! Scope: a WAL with XXH3-64 frames and idempotent replay gated on a manifest watermark; a mutable
//! buffer of normalized rows; a `seal()` that sorts by time, writes an immutable Parquet segment
//! (+ `.tidx` sidecar) via a temp→rename, and bumps the durable watermark; the **append-only manifest
//! delta log with a compacted checkpoint** (the [`manifest`] module) carrying that watermark + segment
//! set; and `retain()` (age + disk-budget). The O(1) freeze-and-swap buffer over Arrow builders,
//! dict-encoded columns, WAL rotation/reclaim, orphan cleanup, and compaction are all built; each is
//! called out inline where it shapes the code.

mod manifest;
mod schema;
mod wal;

use manifest::{Manifest, ManifestView, ManifestWriter};

use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use arrow::array::{
    Array, ArrayRef, BooleanBuilder, FixedSizeBinaryBuilder, Float64Builder, Int32Builder,
    ListBuilder, StringArray, StringBuilder, StringDictionaryBuilder, TimestampNanosecondArray,
    TimestampNanosecondBuilder, UInt8Builder, UInt32Builder, UInt64Array, UInt64Builder,
};
use arrow::datatypes::{DataType, Int32Type, SchemaRef};
use arrow::record_batch::RecordBatch;
use parquet::arrow::ArrowWriter;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use parquet::basic::{Compression as PqCompression, ZstdLevel};
use parquet::file::properties::WriterProperties;
use parquet::schema::types::ColumnPath;

use fs4::fs_std::FileExt;
use imbh_core::{
    AnyValue, Compression, Error, ExpHistogramRow, HistogramRow, LogRow, Lsn, MemoryBudget,
    ParquetPhase, Promote, Result, Retention, ScalarMetricRow, SegmentRef, SpanRow, SummaryRow,
    Table, Timestamp, WalMode, json_get,
};

pub use schema::{
    exp_histogram_schema, histogram_schema, logs_schema, metric_scalar_schema, promoted_columns,
    spans_schema, summary_schema,
};
pub use wal::{SIGNAL_LOGS, SIGNAL_METRICS, SIGNAL_TRACES, WalRecord, WalTailCursor};

/// The scalar metric tables backed by [`metric_scalar_schema`] (what the query layer registers).
pub const SCALAR_METRIC_TABLES: [Table; 2] = [Table::MetricsGauge, Table::MetricsSum];

/// Deterministic crash injection for the crash-recovery E2E tests, compiled only under the
/// `fault-injection` feature (off by default — see the feature note in Cargo.toml). `seal()` calls
/// `maybe_abort` at named hazard points; when the matching `IMBH_FAULT_ABORT_<POINT>` env var is set
/// to `1`, the process `abort()`s there so a test can reopen the directory and assert recovery.
mod fault {
    /// `process::abort()` if `IMBH_FAULT_ABORT_<POINT>` (uppercased) is set to `1`. A no-op without
    /// the `fault-injection` feature (and inlined away).
    #[cfg(feature = "fault-injection")]
    pub(crate) fn maybe_abort(point: &str) {
        let var = format!("IMBH_FAULT_ABORT_{}", point.to_uppercase());
        if std::env::var(var).as_deref() == Ok("1") {
            eprintln!("fault-injection: aborting seal at hazard point `{point}`");
            std::process::abort();
        }
    }

    #[cfg(not(feature = "fault-injection"))]
    #[inline(always)]
    pub(crate) fn maybe_abort(_point: &str) {}
}

use wal::Wal;

/// Default seal threshold: flush the buffer to a segment once it exceeds this many bytes.
/// Seal is still explicit (`Db::flush`) in M1; the auto-seal trigger arrives with the M1
/// maintenance path.
const DEFAULT_SEAL_BYTES: usize = 8 << 20;

/// The single-writer lock file (ARCHITECTURE.md §5). A `ReadWrite` open holds an exclusive advisory
/// lock on it for the DB's lifetime; readers never touch it. Not a WAL/segment/manifest artifact, so
/// `cleanup_orphans`, the WAL segment scan, and the manifest loader all ignore it.
const LOCK_FILE: &str = "writer.lock";

/// A small human-readable marker the writer persists at open so a *reader* process — which cannot see
/// the writer's in-RAM config — can learn whether the writer maintains a WAL. A reader needs the WAL
/// tail for near-real-time freshness; if the writer's WAL is off, the reader sees only seal-interval
/// freshness, so `Db::open_read_only` uses this to reject-by-default (ARCHITECTURE.md §5). One line,
/// `wal_mode\t<off|interval|always>`. Absent / unparseable ⇒ "unknown", which never rejects (a
/// pre-marker DB reads as before).
const INFO_FILE: &str = "db.info";

/// How many times `read_disk_snapshot` re-reads the manifest when a concurrent seal moves the
/// watermark mid-read before giving up. A seal is infrequent relative to a snapshot read, so the
/// bracket almost always stabilizes on the first retry; the cap only bounds a pathological
/// seal-storm.
const SNAPSHOT_RECHECK_TRIES: usize = 8;

/// The storage engine for one DB directory (or an in-memory DB).
pub struct Storage {
    /// `None` for an in-memory DB: everything stays in the buffer, `seal()` is a no-op.
    dir: Option<PathBuf>,
    compression: Compression,
    wal_mode: WalMode,
    retention: Retention,
    /// Attribute keys promoted to typed columns (ARCHITECTURE.md §6.1). Immutable for the DB's
    /// lifetime; drives both the schema width (`*_schema(self.promote.keys())`) and the extra
    /// columns each `*_rows_to_batch` materializes from the row's canonical JSON. Empty by default.
    promote: Promote,
    /// Auto-seal trigger: `seal_if_full` seals once `buffer_bytes` reaches this (ARCHITECTURE.md §7).
    seal_threshold_bytes: usize,
    seq: AtomicU64,
    /// Highest LSN whose data is durable (fsync'd WAL, or captured in a sealed segment).
    durable_lsn: AtomicU64,
    /// `None` for in-memory or WAL-disabled DBs.
    wal: Option<Mutex<Wal>>,
    /// The append-only manifest writer for a `ReadWrite` on-disk DB (ARCHITECTURE.md §7); `None` for
    /// in-memory / read-only DBs (which never persist). Diffs each seal/retain/compact against the
    /// last-persisted state and appends only the delta, rolling to a checkpoint as it grows.
    manifest: Option<Mutex<ManifestWriter>>,
    /// A read-only view (`Access::ReadOnly`): no writer lock, no WAL append handle, and every write
    /// path (`ingest*`/`seal`) refuses. Queries read a fresh on-disk snapshot per call (the facade
    /// uses [`read_disk_snapshot`]); the live buffers here stay empty.
    read_only: bool,
    /// The held `writer.lock` file for a `ReadWrite` on-disk DB. Kept alive only so the exclusive
    /// advisory lock persists for the DB's lifetime; dropping it (or process exit) releases the lock.
    /// `None` for in-memory / read-only DBs.
    _writer_lock: Option<std::fs::File>,
    inner: Mutex<Inner>,
}

/// Acquire the exclusive single-writer advisory lock on `<dir>/writer.lock`, creating the file if
/// absent. Returns the locked handle (keep it alive to hold the lock) or [`Error::lock_held`] when
/// another writer already holds it (ARCHITECTURE.md §5). The lock releases on handle drop / process
/// exit, so a crashed writer never leaves a stale lock.
fn acquire_writer_lock(dir: &Path) -> Result<std::fs::File> {
    let path = dir.join(LOCK_FILE);
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .map_err(|e| Error::open_ctx("open writer.lock", e))?;
    // Non-blocking: a held lock returns `Ok(false)` rather than parking, so a second writer fails
    // fast instead of hanging.
    match file.try_lock_exclusive() {
        Ok(true) => Ok(file),
        Ok(false) => Err(Error::lock_held(path)),
        Err(e) => Err(Error::open_ctx("lock writer.lock", e)),
    }
}

/// The per-table batches one in-flight [`Storage::seal`] took out of the live buffers but has not
/// yet registered as a segment. Held in [`Inner::sealing`] for the duration of the off-lock Parquet
/// write so a concurrent [`Storage::query_snapshot`] still sees those rows — closing the window in
/// which they are in neither the buffer (already taken) nor a segment (not yet written). Batch
/// clones are cheap (Arrow columns are `Arc`-shared), so staging costs a pointer copy, not data.
#[derive(Default)]
struct SealingBatches {
    logs: Vec<RecordBatch>,
    spans: Vec<RecordBatch>,
    metrics: BTreeMap<Table, Vec<RecordBatch>>,
    histogram: Vec<RecordBatch>,
    exp_histogram: Vec<RecordBatch>,
    summary: Vec<RecordBatch>,
}

struct Inner {
    /// The `logs` table buffer and sealed segments. The buffer is an ordered `Vec<RecordBatch>`:
    /// each ingest/replay call encodes its rows to a `RecordBatch` once and appends it (IOx-style,
    /// ARCHITECTURE.md §7), so seal is an O(1) freeze-and-swap and the rows are columnar from
    /// arrival (lower steady RSS than holding `Vec<Row>`). Batches are held in LSN/append order.
    buffer: Vec<RecordBatch>,
    segments: Vec<SegmentRef>,
    /// The `spans` table buffer (ordered `Vec<RecordBatch>`) and sealed segments.
    spans_buffer: Vec<RecordBatch>,
    spans_segments: Vec<SegmentRef>,
    /// Scalar metric tables (`metrics_gauge`/`metrics_sum`), keyed by [`Table`]; each an ordered
    /// `Vec<RecordBatch>`.
    metric_buffers: BTreeMap<Table, Vec<RecordBatch>>,
    /// Sealed metric segments, keyed by [`Table`]. Holds the scalar tables **and**
    /// `metrics_histogram` — the map is schema-agnostic (`SegmentRef`s + paths), so retention,
    /// snapshot, manifest persistence, and path enumeration cover histograms for free. Only the
    /// live buffer and the seal/read schema differ, hence the separate [`Inner::histogram_buffer`].
    metric_segments: BTreeMap<Table, Vec<SegmentRef>>,
    /// The `metrics_histogram` table buffer (List-column batches can't share the scalar schema).
    histogram_buffer: Vec<RecordBatch>,
    /// The `metrics_exp_histogram` table buffer (its own List-column schema).
    exp_histogram_buffer: Vec<RecordBatch>,
    /// The `metrics_summary` table buffer (precomputed-quantile List-column schema).
    summary_buffer: Vec<RecordBatch>,
    buffer_bytes: usize,
    /// Highest LSN fully captured in sealed segments across all tables (ARCHITECTURE.md §7). Because
    /// `seal` seals every table together, this is simply `buffer_max_lsn` at the last seal.
    /// Replay skips records with `lsn <=` this.
    watermark: u64,
    /// Highest LSN whose rows are currently buffered (any table) — the next seal's watermark.
    buffer_max_lsn: u64,
    /// Next LSN to hand out.
    next_lsn: u64,
    /// WAL records with `lsn > watermark`, awaiting decode+replay by the facade on open.
    pending_replay: Vec<WalRecord>,
    /// In-flight seals' taken-but-not-yet-registered batches, keyed by a per-seal generation
    /// ([`Inner::next_seal_gen`]). A [`Storage::query_snapshot`] unions these with the live buffers so
    /// no query misses rows mid-seal; each seal removes its entry once its segment is registered (or
    /// on the error path, when it hands the batches back to the buffer). Empty in steady state.
    sealing: BTreeMap<u64, SealingBatches>,
    /// Monotonic id handed to the next [`Storage::seal`] to key its [`Inner::sealing`] entry.
    next_seal_gen: u64,
}

impl Storage {
    /// Open storage on a directory (created if absent): load the manifest (segments +
    /// watermark), scan the WAL, and stage records with `lsn > watermark` for replay.
    pub fn open(
        dir: impl AsRef<Path>,
        compression: Compression,
        wal_mode: WalMode,
        retention: Retention,
        budget: MemoryBudget,
    ) -> Result<Self> {
        let dir = dir.as_ref().to_path_buf();
        std::fs::create_dir_all(&dir)?;
        // Claim single-writer ownership before touching the WAL/manifest, so a second writer on the
        // same directory fails fast rather than racing recovery (ARCHITECTURE.md §5).
        let writer_lock = acquire_writer_lock(&dir)?;
        // Advertise our WAL mode so reader processes can tell whether near-real-time reads are
        // possible (best-effort — a hint file, never fails the open).
        write_db_info(&dir, wal_mode);
        // Resolve CURRENT and replay the manifest delta log (migrating a legacy whole-file MANIFEST on
        // first sight), returning the reconstructed state + a writer positioned to append (§7).
        let (manifest, manifest_writer) = manifest::open(&dir)?;
        let watermark = manifest.watermark;
        // Reclaim crash debris: segment files (+ sidecars), stale temp files, and stray `MANIFEST-*`
        // logs from an interrupted roll that the live manifest does not reference — e.g. a seal that
        // wrote segments but crashed before persisting the edit (its WAL frames are still `> watermark`
        // and will replay). Best-effort, never fails open.
        cleanup_orphans(&dir, &manifest, manifest_writer.active_num());

        // Read across all numbered WAL segments as one stream (LSN-monotonic across boundaries).
        let frames = wal::read_all_frames(&dir)?;
        let max_lsn = frames.iter().map(|r| r.lsn).max().unwrap_or(0);
        let next_lsn = watermark.max(max_lsn) + 1;
        let pending_replay: Vec<WalRecord> =
            frames.into_iter().filter(|r| r.lsn > watermark).collect();
        let wal = Wal::open(&dir)?;

        Ok(Self {
            dir: Some(dir),
            compression,
            wal_mode,
            retention,
            promote: Promote::default(),
            seal_threshold_bytes: seal_threshold(budget),
            seq: AtomicU64::new(0),
            durable_lsn: AtomicU64::new(watermark),
            wal: Some(Mutex::new(wal)),
            manifest: Some(Mutex::new(manifest_writer)),
            read_only: false,
            _writer_lock: Some(writer_lock),
            inner: Mutex::new(Inner {
                buffer: Vec::new(),
                segments: manifest.logs,
                spans_buffer: Vec::new(),
                spans_segments: manifest.spans,
                metric_buffers: BTreeMap::new(),
                metric_segments: manifest.metrics,
                histogram_buffer: Vec::new(),
                exp_histogram_buffer: Vec::new(),
                summary_buffer: Vec::new(),
                buffer_bytes: 0,
                watermark,
                buffer_max_lsn: watermark,
                next_lsn,
                pending_replay,
                sealing: BTreeMap::new(),
                next_seal_gen: 0,
            }),
        })
    }

    /// Open an ephemeral in-memory storage: no directory, no WAL, no segments, no sealing.
    pub fn in_memory(compression: Compression, budget: MemoryBudget) -> Self {
        Self {
            dir: None,
            compression,
            wal_mode: WalMode::Off,
            retention: Retention::none(),
            promote: Promote::default(),
            seal_threshold_bytes: seal_threshold(budget),
            seq: AtomicU64::new(0),
            durable_lsn: AtomicU64::new(0),
            wal: None,
            manifest: None,
            read_only: false,
            _writer_lock: None,
            inner: Mutex::new(Inner {
                buffer: Vec::new(),
                segments: Vec::new(),
                spans_buffer: Vec::new(),
                spans_segments: Vec::new(),
                metric_buffers: BTreeMap::new(),
                metric_segments: BTreeMap::new(),
                histogram_buffer: Vec::new(),
                exp_histogram_buffer: Vec::new(),
                summary_buffer: Vec::new(),
                buffer_bytes: 0,
                watermark: 0,
                buffer_max_lsn: 0,
                next_lsn: 1,
                pending_replay: Vec::new(),
                sealing: BTreeMap::new(),
                next_seal_gen: 0,
            }),
        }
    }

    /// Open a **read-only** view of an existing on-disk DB (`Access::ReadOnly`, ARCHITECTURE.md §5).
    /// Takes no writer lock and opens no WAL append handle, so it coexists with the single writer and
    /// with other readers, and never mutates the directory. It holds only config + schemas + the
    /// directory path; the live buffers stay empty. Queries reconstruct a point-in-time snapshot per
    /// call via [`read_disk_snapshot`] (manifest segments ∪ WAL tail). Errors if `dir` does not
    /// exist (a reader must not create the DB).
    pub fn open_read_only(
        dir: impl AsRef<Path>,
        compression: Compression,
        budget: MemoryBudget,
    ) -> Result<Self> {
        let dir = dir.as_ref().to_path_buf();
        if !dir.is_dir() {
            return Err(Error::open_ctx(
                "open read-only",
                std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!("database directory {} does not exist", dir.display()),
                ),
            ));
        }
        // Seed the durable watermark from the manifest so `durable_through()` is meaningful on a
        // reader; everything else is derived per query.
        let watermark = manifest::read(&dir)?.watermark;
        Ok(Self {
            dir: Some(dir),
            compression,
            wal_mode: WalMode::Off,
            retention: Retention::none(),
            promote: Promote::default(),
            seal_threshold_bytes: seal_threshold(budget),
            seq: AtomicU64::new(0),
            durable_lsn: AtomicU64::new(watermark),
            wal: None,
            manifest: None,
            read_only: true,
            _writer_lock: None,
            inner: Mutex::new(Inner {
                buffer: Vec::new(),
                segments: Vec::new(),
                spans_buffer: Vec::new(),
                spans_segments: Vec::new(),
                metric_buffers: BTreeMap::new(),
                metric_segments: BTreeMap::new(),
                histogram_buffer: Vec::new(),
                exp_histogram_buffer: Vec::new(),
                summary_buffer: Vec::new(),
                buffer_bytes: 0,
                watermark,
                buffer_max_lsn: watermark,
                next_lsn: watermark + 1,
                pending_replay: Vec::new(),
                sealing: BTreeMap::new(),
                next_seal_gen: 0,
            }),
        })
    }

    /// `true` for a read-only view ([`Storage::open_read_only`]); every write path refuses.
    pub fn is_read_only(&self) -> bool {
        self.read_only
    }

    /// The DB directory, or `None` for an in-memory DB. Readers need it to locate the manifest/WAL
    /// for their per-query snapshot.
    pub fn dir(&self) -> Option<&Path> {
        self.dir.as_deref()
    }

    /// Set the promoted attribute keys (ARCHITECTURE.md §6.1). Consuming builder the facade calls
    /// right after construction; storage defaults to no promotion. Must be set before any ingest so
    /// every buffered batch and sealed segment shares one schema width for the DB's lifetime.
    pub fn with_promote(mut self, promote: Promote) -> Self {
        self.promote = promote;
        self
    }

    /// The promoted attribute keys in effect (empty when nothing is promoted).
    pub fn promote(&self) -> &Promote {
        &self.promote
    }

    /// The `logs` table schema (shared with the query layer through the facade).
    pub fn schema(&self) -> SchemaRef {
        logs_schema(self.promote.keys())
    }

    /// The `spans` table schema.
    pub fn schema_spans(&self) -> SchemaRef {
        spans_schema(self.promote.keys())
    }

    /// The shared scalar-metric schema (used by `metrics_gauge`/`metrics_sum`).
    pub fn schema_metric_scalar(&self) -> SchemaRef {
        metric_scalar_schema(self.promote.keys())
    }

    /// Live ingest: assign an LSN, append the raw OTLP bytes to the WAL (fsync per
    /// [`WalMode`] when `sync_now`), then append the normalized rows to the buffer. Returns
    /// the LSN and whether it is durable on return. `sync_now == false` is the fail-fast
    /// path (`try_ingest_*`), which never fsyncs inline (ARCHITECTURE.md §10.5).
    pub fn ingest(
        &self,
        signal: u8,
        raw: &[u8],
        rows: Vec<LogRow>,
        sync_now: bool,
    ) -> Result<(Lsn, bool)> {
        if self.read_only {
            return Err(Error::read_only());
        }
        let mut inner = self.inner.lock().unwrap();
        let (lsn, durable) = self.wal_append_assign(signal, raw, sync_now, &mut inner)?;
        push_log_batch(&mut inner, rows, self.promote.keys())?;
        Ok((Lsn::new(lsn).expect("assigned LSN is >= 1"), durable))
    }

    /// Live ingest for the `spans` table (ARCHITECTURE.md §6.3).
    pub fn ingest_traces(
        &self,
        raw: &[u8],
        rows: Vec<SpanRow>,
        sync_now: bool,
    ) -> Result<(Lsn, bool)> {
        if self.read_only {
            return Err(Error::read_only());
        }
        let mut inner = self.inner.lock().unwrap();
        let (lsn, durable) = self.wal_append_assign(SIGNAL_TRACES, raw, sync_now, &mut inner)?;
        push_span_batch(&mut inner, rows, self.promote.keys())?;
        Ok((Lsn::new(lsn).expect("assigned LSN is >= 1"), durable))
    }

    /// Assign the next LSN, append the raw bytes to the WAL, and apply the fsync policy. The
    /// caller holds the `Inner` lock and appends the decoded rows to the right buffer.
    #[cfg_attr(
        feature = "tracing",
        tracing::instrument(
            level = "trace",
            name = "wal.append",
            skip_all,
            fields(signal, bytes = raw.len(), lsn = tracing::field::Empty, durable = tracing::field::Empty)
        )
    )]
    fn wal_append_assign(
        &self,
        signal: u8,
        raw: &[u8],
        sync_now: bool,
        inner: &mut Inner,
    ) -> Result<(u64, bool)> {
        let lsn = inner.next_lsn;
        debug_assert!(lsn >= 1, "assigned LSN must be >= 1 (Lsn is NonZero)");
        inner.next_lsn += 1;
        let mut durable = false;
        if let Some(wal) = &self.wal {
            let mut w = wal.lock().unwrap();
            w.append(lsn, signal, raw)?;
            if sync_now && matches!(self.wal_mode, WalMode::Always) {
                w.sync()?;
                self.durable_lsn.fetch_max(lsn, Ordering::AcqRel);
                durable = true;
            }
        }
        inner.buffer_max_lsn = lsn;
        #[cfg(feature = "tracing")]
        {
            let span = tracing::Span::current();
            span.record("lsn", lsn);
            span.record("durable", durable);
        }
        Ok((lsn, durable))
    }

    /// Refill the `logs` buffer from a WAL record during recovery (buffer only — the frame is
    /// already in the WAL). Advances the LSN bookkeeping so a later seal writes the right
    /// watermark and new ingests keep monotonic LSNs.
    pub fn replay(&self, lsn: u64, rows: Vec<LogRow>) {
        let mut inner = self.inner.lock().unwrap();
        // Replay runs single-threaded at open before any live ingest, so encoding under the lock and
        // propagating a malformed-row error is not possible here — a corrupt row would already have
        // failed at its original ingest. Panic on the (unreachable) encode failure rather than widen
        // this fn's signature to `Result`.
        push_log_batch(&mut inner, rows, self.promote.keys()).expect("replay: encode logs batch");
        inner.buffer_max_lsn = inner.buffer_max_lsn.max(lsn);
        inner.next_lsn = inner.next_lsn.max(lsn + 1);
    }

    /// Refill the `spans` buffer from a WAL record during recovery.
    pub fn replay_traces(&self, lsn: u64, rows: Vec<SpanRow>) {
        let mut inner = self.inner.lock().unwrap();
        push_span_batch(&mut inner, rows, self.promote.keys()).expect("replay: encode spans batch");
        inner.buffer_max_lsn = inner.buffer_max_lsn.max(lsn);
        inner.next_lsn = inner.next_lsn.max(lsn + 1);
    }

    /// Live ingest for the scalar metric tables (ARCHITECTURE.md §6.4). Rows are routed to
    /// `metrics_gauge`/`metrics_sum` by their `table` field.
    #[allow(clippy::too_many_arguments)]
    pub fn ingest_metrics(
        &self,
        raw: &[u8],
        rows: Vec<ScalarMetricRow>,
        histograms: Vec<HistogramRow>,
        exp_histograms: Vec<ExpHistogramRow>,
        summaries: Vec<SummaryRow>,
        sync_now: bool,
    ) -> Result<(Lsn, bool)> {
        if self.read_only {
            return Err(Error::read_only());
        }
        let mut inner = self.inner.lock().unwrap();
        // One WAL frame + one LSN for the whole OTLP request; all row kinds re-derive on replay.
        let (lsn, durable) = self.wal_append_assign(SIGNAL_METRICS, raw, sync_now, &mut inner)?;
        let keys = self.promote.keys();
        push_scalar_metric_batches(&mut inner, rows, keys)?;
        push_histogram_batch(&mut inner, histograms, keys)?;
        push_exp_histogram_batch(&mut inner, exp_histograms, keys)?;
        push_summary_batch(&mut inner, summaries, keys)?;
        Ok((Lsn::new(lsn).expect("assigned LSN is >= 1"), durable))
    }

    /// Refill the scalar metric buffers from a WAL record during recovery.
    pub fn replay_metrics(&self, lsn: u64, rows: Vec<ScalarMetricRow>) {
        let mut inner = self.inner.lock().unwrap();
        push_scalar_metric_batches(&mut inner, rows, self.promote.keys())
            .expect("replay: encode scalar-metric batch");
        inner.buffer_max_lsn = inner.buffer_max_lsn.max(lsn);
        inner.next_lsn = inner.next_lsn.max(lsn + 1);
    }

    /// Refill the `metrics_histogram` buffer from a WAL record during recovery. Called for the same
    /// record as [`Storage::replay_metrics`] (same LSN/frame), so it does not re-advance the LSN.
    pub fn replay_histograms(&self, lsn: u64, rows: Vec<HistogramRow>) {
        let mut inner = self.inner.lock().unwrap();
        push_histogram_batch(&mut inner, rows, self.promote.keys())
            .expect("replay: encode histogram batch");
        inner.buffer_max_lsn = inner.buffer_max_lsn.max(lsn);
        inner.next_lsn = inner.next_lsn.max(lsn + 1);
    }

    /// Refill the `metrics_exp_histogram` buffer from a WAL record during recovery.
    pub fn replay_exp_histograms(&self, lsn: u64, rows: Vec<ExpHistogramRow>) {
        let mut inner = self.inner.lock().unwrap();
        push_exp_histogram_batch(&mut inner, rows, self.promote.keys())
            .expect("replay: encode exp-histogram batch");
        inner.buffer_max_lsn = inner.buffer_max_lsn.max(lsn);
        inner.next_lsn = inner.next_lsn.max(lsn + 1);
    }

    /// Refill the `metrics_summary` buffer from a WAL record during recovery.
    pub fn replay_summaries(&self, lsn: u64, rows: Vec<SummaryRow>) {
        let mut inner = self.inner.lock().unwrap();
        push_summary_batch(&mut inner, rows, self.promote.keys())
            .expect("replay: encode summary batch");
        inner.buffer_max_lsn = inner.buffer_max_lsn.max(lsn);
        inner.next_lsn = inner.next_lsn.max(lsn + 1);
    }

    /// A snapshot of a scalar metric table's buffer as one Arrow batch.
    pub fn buffer_snapshot_metric(&self, table: Table) -> Result<RecordBatch> {
        let inner = self.inner.lock().unwrap();
        let batches = inner.metric_buffers.get(&table).map(Vec::as_slice);
        concat_buffer(
            batches.unwrap_or(&[]),
            metric_scalar_schema(self.promote.keys()),
        )
    }

    /// The `metrics_histogram` Arrow schema.
    pub fn schema_histogram(&self) -> SchemaRef {
        histogram_schema(self.promote.keys())
    }

    /// A snapshot of the `metrics_histogram` buffer as one Arrow batch.
    pub fn buffer_snapshot_histogram(&self) -> Result<RecordBatch> {
        let inner = self.inner.lock().unwrap();
        concat_buffer(
            &inner.histogram_buffer,
            histogram_schema(self.promote.keys()),
        )
    }

    /// The sealed `metrics_histogram` segments (stored in the shared metric-segment map).
    pub fn segments_histogram(&self) -> Vec<SegmentRef> {
        self.segments_metric(Table::MetricsHistogram)
    }

    /// Absolute paths of the sealed `metrics_histogram` segments.
    pub fn segment_paths_histogram(&self) -> Vec<PathBuf> {
        self.segment_paths_metric(Table::MetricsHistogram)
    }

    /// The `metrics_exp_histogram` Arrow schema.
    pub fn schema_exp_histogram(&self) -> SchemaRef {
        exp_histogram_schema(self.promote.keys())
    }

    /// A snapshot of the `metrics_exp_histogram` buffer as one Arrow batch.
    pub fn buffer_snapshot_exp_histogram(&self) -> Result<RecordBatch> {
        let inner = self.inner.lock().unwrap();
        concat_buffer(
            &inner.exp_histogram_buffer,
            exp_histogram_schema(self.promote.keys()),
        )
    }

    /// The sealed `metrics_exp_histogram` segments (stored in the shared metric-segment map).
    pub fn segments_exp_histogram(&self) -> Vec<SegmentRef> {
        self.segments_metric(Table::MetricsExpHistogram)
    }

    /// Absolute paths of the sealed `metrics_exp_histogram` segments.
    pub fn segment_paths_exp_histogram(&self) -> Vec<PathBuf> {
        self.segment_paths_metric(Table::MetricsExpHistogram)
    }

    /// The `metrics_summary` Arrow schema.
    pub fn schema_summary(&self) -> SchemaRef {
        summary_schema(self.promote.keys())
    }

    /// A snapshot of the `metrics_summary` buffer as one Arrow batch.
    pub fn buffer_snapshot_summary(&self) -> Result<RecordBatch> {
        let inner = self.inner.lock().unwrap();
        concat_buffer(&inner.summary_buffer, summary_schema(self.promote.keys()))
    }

    /// The sealed `metrics_summary` segments (stored in the shared metric-segment map).
    pub fn segments_summary(&self) -> Vec<SegmentRef> {
        self.segments_metric(Table::MetricsSummary)
    }

    /// Absolute paths of the sealed `metrics_summary` segments.
    pub fn segment_paths_summary(&self) -> Vec<PathBuf> {
        self.segment_paths_metric(Table::MetricsSummary)
    }

    /// The sealed segments of a scalar metric table.
    pub fn segments_metric(&self, table: Table) -> Vec<SegmentRef> {
        self.inner
            .lock()
            .unwrap()
            .metric_segments
            .get(&table)
            .cloned()
            .unwrap_or_default()
    }

    /// Absolute paths of a scalar metric table's sealed segments.
    pub fn segment_paths_metric(&self, table: Table) -> Vec<PathBuf> {
        let inner = self.inner.lock().unwrap();
        match (&self.dir, inner.metric_segments.get(&table)) {
            (Some(dir), Some(segs)) => segs.iter().map(|s| dir.join(&s.relative_path)).collect(),
            _ => Vec::new(),
        }
    }

    /// Take the WAL records staged for replay on open (`lsn > watermark`). The facade decodes
    /// each by signal and calls [`Storage::replay`].
    pub fn take_pending_replay(&self) -> Vec<WalRecord> {
        std::mem::take(&mut self.inner.lock().unwrap().pending_replay)
    }

    /// Append rows directly to the buffer with no WAL and no LSN tracking. Used by in-memory
    /// tests; the durable path is [`Storage::ingest`].
    pub fn append_logs(&self, rows: Vec<LogRow>) -> usize {
        let n = rows.len();
        let mut inner = self.inner.lock().unwrap();
        // Test-only helper over always-valid rows; a build failure here is a bug, not a runtime path.
        push_log_batch(&mut inner, rows, self.promote.keys())
            .expect("append_logs: encode logs batch");
        n
    }

    /// Highest fsync'd / sealed LSN (ARCHITECTURE.md §10.2), or `None` when nothing is durable yet
    /// (the watermark sits at 0, which is not a valid [`Lsn`]).
    pub fn durable_through(&self) -> Option<Lsn> {
        Lsn::new(self.durable_lsn.load(Ordering::Acquire))
    }

    /// Group-commit the WAL: fsync the current segment **once** and advance `durable_lsn` to the
    /// highest LSN appended so far. This is the batched counterpart to the per-append fsync in
    /// [`Self::ingest`] under `WalMode::Always` — the async-ingest worker appends a drained burst with
    /// `sync_now = false`, then calls this once to make the whole burst durable with a single fsync
    /// (ARCHITECTURE.md §10.5). A no-op unless `WalMode::Always`: `Interval`/`Off` never fsync
    /// per-append, so there is nothing to force here beyond their own policy.
    ///
    /// Reading `buffer_max_lsn` before the fsync is safe as a durability lower bound: `ingest` writes
    /// each WAL frame (`w.append`) before publishing its `buffer_max_lsn`, so any LSN we observe is
    /// already in the file and covered by `sync_data`; a concurrent append that lands afterward is
    /// flushed too but simply not advertised until the next commit (conservative, never overclaims).
    pub fn group_commit(&self) -> Result<()> {
        if !matches!(self.wal_mode, WalMode::Always) {
            return Ok(());
        }
        let Some(wal) = &self.wal else {
            return Ok(());
        };
        let max_lsn = self.inner.lock().unwrap().buffer_max_lsn;
        wal.lock().unwrap().sync()?;
        self.durable_lsn.fetch_max(max_lsn, Ordering::AcqRel);
        Ok(())
    }

    /// The sealed-segment watermark (highest LSN captured in segments).
    pub fn watermark(&self) -> u64 {
        self.inner.lock().unwrap().watermark
    }

    /// Approximate live heap bytes held by the mutable buffers across all tables (ARCHITECTURE.md §10.11).
    pub fn buffer_bytes(&self) -> usize {
        self.inner.lock().unwrap().buffer_bytes
    }

    /// On-disk WAL size in bytes: the sum of all numbered segment files (0 for in-memory DBs, or
    /// when no segments exist yet).
    pub fn wal_bytes(&self) -> u64 {
        match &self.dir {
            Some(dir) => wal::total_bytes(dir),
            None => 0,
        }
    }

    /// A consistent snapshot of the `logs` buffer as one Arrow batch (ARCHITECTURE.md §7: queries see
    /// buffer + segments). The buffer is already a `Vec<RecordBatch>` (columnar from arrival), so a
    /// snapshot concatenates the frozen batches under the short lock — no per-row rebuild. Empty
    /// buffer → an empty batch carrying the schema.
    pub fn buffer_snapshot(&self) -> Result<RecordBatch> {
        let inner = self.inner.lock().unwrap();
        concat_buffer(&inner.buffer, logs_schema(self.promote.keys()))
    }

    /// A snapshot of the `spans` buffer as one Arrow batch.
    pub fn buffer_snapshot_spans(&self) -> Result<RecordBatch> {
        let inner = self.inner.lock().unwrap();
        concat_buffer(&inner.spans_buffer, spans_schema(self.promote.keys()))
    }

    /// Absolute paths of all sealed `logs` segments, in manifest order.
    pub fn segment_paths(&self) -> Vec<PathBuf> {
        self.abs_paths(|inner| &inner.segments)
    }

    /// Absolute paths of all sealed `spans` segments.
    pub fn segment_paths_spans(&self) -> Vec<PathBuf> {
        self.abs_paths(|inner| &inner.spans_segments)
    }

    fn abs_paths(&self, pick: impl Fn(&Inner) -> &Vec<SegmentRef>) -> Vec<PathBuf> {
        let inner = self.inner.lock().unwrap();
        match &self.dir {
            Some(dir) => pick(&inner)
                .iter()
                .map(|s| dir.join(&s.relative_path))
                .collect(),
            None => Vec::new(),
        }
    }

    /// The current sealed `logs` segment set (manifest snapshot).
    pub fn segments(&self) -> Vec<SegmentRef> {
        self.inner.lock().unwrap().segments.clone()
    }

    /// The current sealed `spans` segment set.
    pub fn segments_spans(&self) -> Vec<SegmentRef> {
        self.inner.lock().unwrap().spans_segments.clone()
    }

    /// A single-lock, point-in-time view of **every** table's live buffer and sealed segments, for
    /// the writer's own intra-process query path (ARCHITECTURE.md §5). Reading each table's buffer
    /// and its segment set separately (`buffer_snapshot()` then `segments()`) crosses two lock
    /// acquisitions; a background `seal` in between moves rows buffer → segment and would let a query
    /// double-count them (seen in the buffer *and* the new segment). Capturing all buffers and all
    /// segment sets under one `inner` lock — the same lock `seal` holds for its freeze-and-swap —
    /// makes buffer ∪ segments atomic w.r.t. seal. It also unions each buffer with any in-flight
    /// [`Inner::sealing`] batches — rows a concurrent seal has taken out of the buffer but not yet
    /// registered as a segment (its Parquet write runs off-lock) — so a query in that window neither
    /// double-counts nor transiently drops them. Each table's `buffer ∪ sealing` is concatenated to
    /// one batch under the lock; an empty result yields an empty batch carrying the schema.
    pub fn query_snapshot(&self) -> Result<QuerySnapshot> {
        let inner = self.inner.lock().unwrap();
        // `live` (the current buffer) plus every in-flight seal's staged batches for the same table.
        let unioned = |live: &[RecordBatch], pick: fn(&SealingBatches) -> &Vec<RecordBatch>| {
            let mut v = live.to_vec();
            for staged in inner.sealing.values() {
                v.extend(pick(staged).iter().cloned());
            }
            v
        };
        let mut metric_buffers = BTreeMap::new();
        for table in SCALAR_METRIC_TABLES {
            let mut v = inner
                .metric_buffers
                .get(&table)
                .cloned()
                .unwrap_or_default();
            for staged in inner.sealing.values() {
                if let Some(b) = staged.metrics.get(&table) {
                    v.extend(b.iter().cloned());
                }
            }
            let keys = self.promote.keys();
            metric_buffers.insert(table, concat_buffer(&v, metric_scalar_schema(keys))?);
        }
        let keys = self.promote.keys();
        Ok(QuerySnapshot {
            dir: self.dir.clone(),
            logs_buffer: concat_buffer(&unioned(&inner.buffer, |s| &s.logs), logs_schema(keys))?,
            logs_segments: inner.segments.clone(),
            spans_buffer: concat_buffer(
                &unioned(&inner.spans_buffer, |s| &s.spans),
                spans_schema(keys),
            )?,
            spans_segments: inner.spans_segments.clone(),
            metric_buffers,
            histogram_buffer: concat_buffer(
                &unioned(&inner.histogram_buffer, |s| &s.histogram),
                histogram_schema(keys),
            )?,
            exp_histogram_buffer: concat_buffer(
                &unioned(&inner.exp_histogram_buffer, |s| &s.exp_histogram),
                exp_histogram_schema(keys),
            )?,
            summary_buffer: concat_buffer(
                &unioned(&inner.summary_buffer, |s| &s.summary),
                summary_schema(keys),
            )?,
            metric_segments: inner.metric_segments.clone(),
        })
    }

    /// Force-seal every table's buffer into immutable Parquet segments: sort by time, write to a
    /// temp path and rename, register in the manifest, and bump the watermark to the highest
    /// buffered LSN so those WAL records are never replayed again. Sealing all tables together
    /// keeps the single global watermark correct. Returns the `logs` segment (or the `spans`
    /// one) for convenience, or `None` when there is nothing to seal / the DB is in-memory.
    /// Persist the current segment set + watermark durably via the append-only manifest log
    /// (ARCHITECTURE.md §7): the writer diffs against the last-persisted state and appends only the
    /// delta (or rolls to a fresh checkpoint). A no-op for in-memory / read-only DBs (no writer). Must
    /// be called under the `inner` lock (the lists it reads must be a consistent snapshot).
    fn persist_manifest(
        &self,
        logs: &[SegmentRef],
        spans: &[SegmentRef],
        metrics: &BTreeMap<Table, Vec<SegmentRef>>,
        watermark: u64,
    ) -> Result<()> {
        if let Some(mw) = &self.manifest {
            mw.lock().unwrap().persist(ManifestView {
                logs,
                spans,
                metrics,
                watermark,
            })?;
        }
        Ok(())
    }

    #[cfg_attr(
        feature = "tracing",
        tracing::instrument(level = "debug", name = "storage.seal", skip_all)
    )]
    pub fn seal(&self) -> Result<Option<SegmentRef>> {
        let Some(dir) = self.dir.clone() else {
            return Ok(None); // in-memory: buffers are the store.
        };

        let (
            logs_batches,
            spans_batches,
            metric_batches,
            histogram_batches,
            exp_histogram_batches,
            summary_batches,
            taken_bytes,
            watermark,
            seal_gen,
        ) = {
            let mut inner = self.inner.lock().unwrap();
            let metrics_empty = inner.metric_buffers.values().all(|v| v.is_empty());
            if inner.buffer.is_empty()
                && inner.spans_buffer.is_empty()
                && metrics_empty
                && inner.histogram_buffer.is_empty()
                && inner.exp_histogram_buffer.is_empty()
                && inner.summary_buffer.is_empty()
            {
                return Ok(None);
            }
            // O(1) freeze-and-swap: the per-table `Vec<RecordBatch>` buffers are moved out wholesale;
            // the batches were already encoded at ingest, so no per-row work happens under the lock.
            // Capture the aggregate byte total so the error path can restore it exactly (every byte in
            // `buffer_bytes` is accounted to a taken batch, so this equals their summed row estimate).
            let taken_bytes = inner.buffer_bytes;
            inner.buffer_bytes = 0;
            let watermark = inner.buffer_max_lsn;
            let logs_b = std::mem::take(&mut inner.buffer);
            let spans_b = std::mem::take(&mut inner.spans_buffer);
            let metric_b = std::mem::take(&mut inner.metric_buffers);
            let histogram_b = std::mem::take(&mut inner.histogram_buffer);
            let exp_b = std::mem::take(&mut inner.exp_histogram_buffer);
            let summary_b = std::mem::take(&mut inner.summary_buffer);
            // Stage clones of the taken batches so a concurrent `query_snapshot` still sees these rows
            // while the segment is written off-lock below — closing the window where they are out of
            // the buffer but not yet in a segment. Clones are cheap (Arrow columns are `Arc`-shared).
            // The entry is removed once the segment is registered (or handed back on the error path).
            let seal_gen = inner.next_seal_gen;
            inner.next_seal_gen += 1;
            inner.sealing.insert(
                seal_gen,
                SealingBatches {
                    logs: logs_b.clone(),
                    spans: spans_b.clone(),
                    metrics: metric_b.clone(),
                    histogram: histogram_b.clone(),
                    exp_histogram: exp_b.clone(),
                    summary: summary_b.clone(),
                },
            );
            (
                logs_b,
                spans_b,
                metric_b,
                histogram_b,
                exp_b,
                summary_b,
                taken_bytes,
                watermark,
                seal_gen,
            )
        };

        // Concat + time-sort each non-empty table's frozen batches, then write one immutable segment.
        // The batches are *borrowed* (taken into these locals but not consumed), so on any write error
        // we hand the un-sealed batches back to the buffer instead of dropping them — otherwise a
        // later successful seal would advance the watermark past them and truncate their
        // still-only-in-WAL frames (data loss).
        type WriteOut = (
            Option<SegmentRef>,
            Option<SegmentRef>,
            Vec<(Table, SegmentRef)>,
        );
        let written: Result<WriteOut> = (|| {
            let logs_seg = if logs_batches.is_empty() {
                None
            } else {
                let sorted = concat_and_sort(&logs_batches, "time")?;
                Some(self.write_logs_segment(&dir, sorted)?)
            };
            let spans_seg = if spans_batches.is_empty() {
                None
            } else {
                let sorted = concat_and_sort(&spans_batches, "start_time")?;
                Some(self.write_spans_segment(&dir, sorted)?)
            };
            let mut metric_segs: Vec<(Table, SegmentRef)> = Vec::new();
            for (table, batches) in metric_batches.iter() {
                if !batches.is_empty() {
                    let sorted = concat_and_sort(batches, "time")?;
                    metric_segs.push((*table, self.write_metric_segment(&dir, *table, sorted)?));
                }
            }
            if !histogram_batches.is_empty() {
                let sorted = concat_and_sort(&histogram_batches, "time")?;
                let seg = self.write_metric_segment(&dir, Table::MetricsHistogram, sorted)?;
                metric_segs.push((Table::MetricsHistogram, seg));
            }
            if !exp_histogram_batches.is_empty() {
                let sorted = concat_and_sort(&exp_histogram_batches, "time")?;
                let seg = self.write_metric_segment(&dir, Table::MetricsExpHistogram, sorted)?;
                metric_segs.push((Table::MetricsExpHistogram, seg));
            }
            if !summary_batches.is_empty() {
                let sorted = concat_and_sort(&summary_batches, "time")?;
                let seg = self.write_metric_segment(&dir, Table::MetricsSummary, sorted)?;
                metric_segs.push((Table::MetricsSummary, seg));
            }
            Ok((logs_seg, spans_seg, metric_segs))
        })();

        let (logs_seg, spans_seg, metric_segs) = match written {
            Ok(v) => v,
            Err(e) => {
                // Hand the un-sealed batches back, ahead of anything ingested concurrently (which
                // appended higher-LSN batches while the write ran off-lock), and restore the byte
                // accounting. Segments already written this pass become orphans that `cleanup_orphans`
                // reclaims on the next open.
                let mut inner = self.inner.lock().unwrap();
                prepend_front(&mut inner.buffer, logs_batches);
                prepend_front(&mut inner.spans_buffer, spans_batches);
                for (table, batches) in metric_batches {
                    prepend_front(inner.metric_buffers.entry(table).or_default(), batches);
                }
                prepend_front(&mut inner.histogram_buffer, histogram_batches);
                prepend_front(&mut inner.exp_histogram_buffer, exp_histogram_batches);
                prepend_front(&mut inner.summary_buffer, summary_batches);
                inner.buffer_bytes += taken_bytes;
                // The rows are back in the buffer; drop the staging entry so they aren't double-counted.
                inner.sealing.remove(&seal_gen);
                return Err(e);
            }
        };

        // Any segment created (prefer logs, then spans, then a metric) as the return value.
        let result = logs_seg
            .clone()
            .or_else(|| spans_seg.clone())
            .or_else(|| metric_segs.first().map(|(_, s)| s.clone()));

        let mut inner = self.inner.lock().unwrap();
        if let Some(s) = logs_seg {
            inner.segments.push(s);
        }
        if let Some(s) = spans_seg {
            inner.spans_segments.push(s);
        }
        for (table, seg) in metric_segs {
            inner.metric_segments.entry(table).or_default().push(seg);
        }
        // Rows are now in registered segments; drop the staging entry under the same lock so a query
        // sees them exactly once — never in both `sealing` and a segment, never in neither. Cleared
        // before `persist_manifest` so a manifest-write error still leaves them visible (in the
        // in-RAM segment) rather than double-counted.
        inner.sealing.remove(&seal_gen);
        inner.watermark = watermark;
        // Hazard point (test-only): the segment Parquet is on disk but the manifest still points at
        // the old watermark and the WAL is un-reclaimed. A crash here must recover every row from the
        // WAL, with the just-written segment cleaned up as an orphan on the next open.
        fault::maybe_abort("before_manifest");
        self.persist_manifest(
            &inner.segments,
            &inner.spans_segments,
            &inner.metric_segments,
            watermark,
        )?;
        self.durable_lsn.fetch_max(watermark, Ordering::AcqRel);
        drop(inner);
        // Hazard point (test-only): the manifest is durable (watermark bumped) but the WAL has not
        // been reclaimed yet. A crash here must recover each row exactly once — from the segment, with
        // the still-present WAL frames skipped by the watermark on idempotent replay.
        fault::maybe_abort("after_manifest");
        // Reclaim WAL space: every frame at/below the new watermark is now in a durable segment, so
        // drop the whole segments that hold only such frames. Done after the manifest is durable, and
        // under the WAL lock so no append races the rotation.
        if let Some(wal) = &self.wal {
            let mut w = wal.lock().unwrap();
            w.reclaim(watermark)?;
        }
        #[cfg(feature = "tracing")]
        tracing::debug!(
            watermark,
            sealed = result.is_some(),
            "sealed buffers to segment(s)"
        );
        Ok(result)
    }

    /// Seal only when the mutable buffer has grown to/past the per-DB byte threshold
    /// (`seal_threshold_bytes`, derived from the memory budget — ARCHITECTURE.md §7). The compare
    /// lives inside storage so the threshold field stays private; the maintenance scheduler calls
    /// this every tick for prompt sealing under load, without waiting a full interval. Returns the
    /// same shape as [`Storage::seal`]: the created segment, or `None` when the buffer is still
    /// below the threshold (or the DB is in-memory, where `seal` is a no-op).
    pub fn seal_if_full(&self) -> Result<Option<SegmentRef>> {
        let full = { self.inner.lock().unwrap().buffer_bytes >= self.seal_threshold_bytes };
        if full { self.seal() } else { Ok(None) }
    }

    /// Write a metric table's already-time-sorted batch to a Parquet segment (no Tantivy — metrics
    /// are not indexed, §6.4). Schema-agnostic across the scalar and List-column metric tables: the
    /// batch is prebuilt at ingest and merged/sorted at seal, so only the `time` bounds and table
    /// name differ per call.
    fn write_metric_segment(
        &self,
        dir: &Path,
        table: Table,
        batch: RecordBatch,
    ) -> Result<SegmentRef> {
        let (min_time, max_time) = time_bounds(&batch, "time");
        let (relative_path, _abs) =
            self.write_segment_parquet(dir, table.as_str(), min_time, &batch)?;
        Ok(SegmentRef {
            relative_path,
            min_time_unix_nano: min_time,
            max_time_unix_nano: max_time,
            rows: batch.num_rows() as u64,
        })
    }

    /// Write the `logs` segment (+ `.tidx` sidecar) from an already-time-sorted batch, return the
    /// segment ref. The `.tidx` row ordinal = Parquet row order = the batch's row order.
    fn write_logs_segment(&self, dir: &Path, batch: RecordBatch) -> Result<SegmentRef> {
        let (min_time, max_time) = time_bounds(&batch, "time");
        let (relative_path, abs_path) =
            self.write_segment_parquet(dir, "logs", min_time, &batch)?;
        // Build the per-segment Tantivy sidecar `<segment>.tidx` (ARCHITECTURE.md §8) from the batch —
        // the same batch-derived index rows the compaction path uses. No-op without `search`.
        build_logs_sidecar(
            &abs_path.with_extension("tidx"),
            &logs_batch_to_index_rows(&batch),
        )?;
        Ok(SegmentRef {
            relative_path,
            min_time_unix_nano: min_time,
            max_time_unix_nano: max_time,
            rows: batch.num_rows() as u64,
        })
    }

    /// Write the `spans` segment (+ `.tidx` sidecar over `name`) from an already-start-time-sorted
    /// batch. `max_time` is `max(start + duration)` so time-window pruning stays correct for long
    /// spans (§6.3).
    fn write_spans_segment(&self, dir: &Path, batch: RecordBatch) -> Result<SegmentRef> {
        let starts = ts_column(&batch, "start_time");
        let durations = u64_column(&batch, "duration_ns");
        let min_time = if starts.is_empty() {
            0
        } else {
            starts.value(0)
        };
        // `saturating` guards a crafted-huge duration from overflowing i64 (would corrupt the
        // segment's `max_time` and weaken time-window pruning; a real span never overflows).
        let max_time = (0..batch.num_rows())
            .map(|i| starts.value(i).saturating_add(durations.value(i) as i64))
            .max()
            .unwrap_or(0);
        let (relative_path, abs) = self.write_segment_parquet(dir, "spans", min_time, &batch)?;
        // Per-segment Tantivy sidecar over the span `name` (row ordinal = Parquet row order), so
        // `traces().matches(name)` prunes like logs. No-op without `search`.
        build_spans_sidecar(
            &abs.with_extension("tidx"),
            &spans_batch_to_index_rows(&batch),
        )?;
        Ok(SegmentRef {
            relative_path,
            min_time_unix_nano: min_time,
            max_time_unix_nano: max_time,
            rows: batch.num_rows() as u64,
        })
    }

    /// Write a batch to `<table>/<day>/<id>.parquet` via temp→rename; return (relative, absolute).
    fn write_segment_parquet(
        &self,
        dir: &Path,
        table: &str,
        min_time: i64,
        batch: &RecordBatch,
    ) -> Result<(String, PathBuf)> {
        let day = utc_date_string(min_time);
        let seq = self.seq.fetch_add(1, Ordering::Relaxed);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let relative_path = format!("{table}/{day}/{now:020}-{seq:06}.parquet");
        let abs_path = dir.join(&relative_path);
        if let Some(parent) = abs_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp_path = abs_path.with_extension("tmp");
        // Point-lookup acceleration (ARCHITECTURE.md §8): only the `spans` table gets Parquet bloom
        // filters, on its high-cardinality id columns, so the query provider can skip whole segments
        // that cannot contain a looked-up `trace_id`/`span_id`. Scoped to spans because those ids
        // don't identify rows in the other tables and a bloom costs bytes; the read-side pruning in
        // `imbh-query`'s provider consumes exactly these filters.
        let bloom_columns: &[&str] = if table == Table::Spans.as_str() {
            &["trace_id", "span_id"]
        } else {
            &[]
        };
        write_parquet(batch, &tmp_path, self.compression, bloom_columns)?;
        std::fs::rename(&tmp_path, &abs_path)?;
        // fsync the day-partition dir so the rename (the segment becoming visible) survives a crash.
        if let Some(parent) = abs_path.parent() {
            fsync_dir(parent)?;
        }
        Ok((relative_path, abs_path))
    }

    /// Drop segments outside the retention policy (ARCHITECTURE.md §7): older than `max_age`, or
    /// oldest-first until the total on-disk size is under `max_disk_bytes`. Deletes the Parquet
    /// file and its `.tidx` sidecar and rewrites the manifest. No-op for in-memory DBs. The
    /// watermark is unchanged — dropped segments' WAL records are `<= watermark`, so they never
    /// replay.
    pub fn retain(&self) -> Result<RetentionReport> {
        let Some(dir) = self.dir.clone() else {
            return Ok(RetentionReport::default());
        };
        let mut inner = self.inner.lock().unwrap();

        // All segments across tables, oldest-first, for age + disk-budget decisions.
        let mut ordered: Vec<SegmentRef> = inner
            .segments
            .iter()
            .chain(inner.spans_segments.iter())
            .chain(inner.metric_segments.values().flatten())
            .cloned()
            .collect();
        ordered.sort_by_key(|s| s.min_time_unix_nano);

        let mut drop_set: std::collections::HashSet<String> = std::collections::HashSet::new();

        if let Some(max_age) = self.retention.max_age() {
            // Clamp (not `as i64`) so an absurd `max_age` (> ~292 years) can't wrap the cast into a
            // future cutoff that drops every segment.
            let age_ns = i64::try_from(max_age.as_nanos()).unwrap_or(i64::MAX);
            let cutoff = Timestamp::now().0.saturating_sub(age_ns);
            for s in &ordered {
                if s.max_time_unix_nano < cutoff {
                    drop_set.insert(s.relative_path.clone());
                }
            }
        }

        if let Some(budget) = self.retention.disk_budget() {
            let survivors: Vec<&SegmentRef> = ordered
                .iter()
                .filter(|s| !drop_set.contains(&s.relative_path))
                .collect();
            let sizes: Vec<u64> = survivors.iter().map(|s| segment_size(&dir, s)).collect();
            let mut total: u64 = sizes.iter().sum();
            let mut i = 0;
            while total > budget && i < survivors.len() {
                drop_set.insert(survivors[i].relative_path.clone());
                total -= sizes[i];
                i += 1;
            }
        }

        if drop_set.is_empty() {
            return Ok(RetentionReport::default());
        }

        // Size the drops now, before any deletion (segment_size stats the on-disk files).
        let mut bytes_freed = 0u64;
        for s in &ordered {
            if drop_set.contains(&s.relative_path) {
                bytes_freed += segment_size(&dir, s);
            }
        }
        let count = |inner: &Inner| {
            inner.segments.len()
                + inner.spans_segments.len()
                + inner.metric_segments.values().map(Vec::len).sum::<usize>()
        };
        let before = count(&inner);
        inner
            .segments
            .retain(|s| !drop_set.contains(&s.relative_path));
        inner
            .spans_segments
            .retain(|s| !drop_set.contains(&s.relative_path));
        for segs in inner.metric_segments.values_mut() {
            segs.retain(|s| !drop_set.contains(&s.relative_path));
        }
        let segments_dropped = (before - count(&inner)) as u64;
        let watermark = inner.watermark;
        // Persist the manifest WITHOUT the dropped segments and make it durable BEFORE deleting the
        // files. A crash in between leaves orphan files (removed by `cleanup_orphans` on next open),
        // never a manifest that references missing files (which would fail every query).
        self.persist_manifest(
            &inner.segments,
            &inner.spans_segments,
            &inner.metric_segments,
            watermark,
        )?;
        for s in &ordered {
            if drop_set.contains(&s.relative_path) {
                delete_segment(&dir, s)?;
            }
        }
        Ok(RetentionReport {
            segments_dropped,
            bytes_freed,
        })
    }

    /// Per-table statistics (segments, rows, buffered rows, time span) — ARCHITECTURE.md §10.11.
    pub fn stats(&self) -> Vec<TableStats> {
        let inner = self.inner.lock().unwrap();
        let mut out = vec![
            table_stats(Table::Logs, &inner.segments, buffered_rows(&inner.buffer)),
            table_stats(
                Table::Spans,
                &inner.spans_segments,
                buffered_rows(&inner.spans_buffer),
            ),
        ];
        for table in SCALAR_METRIC_TABLES {
            let segs = inner
                .metric_segments
                .get(&table)
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            let buffered = inner
                .metric_buffers
                .get(&table)
                .map_or(0, |b| buffered_rows(b));
            out.push(table_stats(table, segs, buffered));
        }
        let hist_segs = inner
            .metric_segments
            .get(&Table::MetricsHistogram)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        out.push(table_stats(
            Table::MetricsHistogram,
            hist_segs,
            buffered_rows(&inner.histogram_buffer),
        ));
        let exp_segs = inner
            .metric_segments
            .get(&Table::MetricsExpHistogram)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        out.push(table_stats(
            Table::MetricsExpHistogram,
            exp_segs,
            buffered_rows(&inner.exp_histogram_buffer),
        ));
        let summary_segs = inner
            .metric_segments
            .get(&Table::MetricsSummary)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        out.push(table_stats(
            Table::MetricsSummary,
            summary_segs,
            buffered_rows(&inner.summary_buffer),
        ));
        out
    }

    /// Snapshot the DB to `dest` (ARCHITECTURE.md §10.11): copy the manifest and hard-link every segment
    /// Parquet file + `.tidx` sidecar (segments are immutable, so links are safe). Falls back to a
    /// copy across filesystems. Errors for an in-memory DB.
    pub fn snapshot(&self, dest: &Path) -> Result<SnapshotInfo> {
        let Some(dir) = self.dir.clone() else {
            return Err(Error::storage_msg("cannot snapshot an in-memory DB"));
        };
        std::fs::create_dir_all(dest)?;
        let inner = self.inner.lock().unwrap();

        let all: Vec<&SegmentRef> = inner
            .segments
            .iter()
            .chain(inner.spans_segments.iter())
            .chain(inner.metric_segments.values().flatten())
            .collect();
        for seg in &all {
            let src = dir.join(&seg.relative_path);
            let dst = dest.join(&seg.relative_path);
            if let Some(parent) = dst.parent() {
                std::fs::create_dir_all(parent)?;
            }
            link_or_copy(&src, &dst)?;
            let src_tidx = src.with_extension("tidx");
            if src_tidx.is_dir() {
                link_dir(&src_tidx, &dst.with_extension("tidx"))?;
            }
        }
        // Write the destination a fresh, self-contained v2 manifest (one checkpoint frame + CURRENT)
        // from the in-RAM state, so the snapshot directory opens like any other DB.
        manifest::write_fresh(
            dest,
            ManifestView {
                logs: &inner.segments,
                spans: &inner.spans_segments,
                metrics: &inner.metric_segments,
                watermark: inner.watermark,
            },
        )?;
        Ok(SnapshotInfo {
            dir: dest.to_path_buf(),
            segments: all.len() as u64,
        })
    }

    /// Compact each table's small segments within a UTC-day partition into one (ARCHITECTURE.md §7):
    /// read → concat → sort by time → write one merged Parquet, rebuild the `logs` Tantivy index,
    /// and delete the inputs. Optional — a DB that never compacts is still correct.
    #[cfg_attr(
        feature = "tracing",
        tracing::instrument(level = "debug", name = "storage.compact", skip_all)
    )]
    pub fn compact(&self) -> Result<CompactionReport> {
        let Some(dir) = self.dir.clone() else {
            return Ok(CompactionReport::default());
        };

        // 1. Snapshot the segment lists under the lock (a cheap `Vec<SegmentRef>` clone — the segment
        //    *metadata*, not the Parquet data), then release. The read/concat/sort/write below runs
        //    OFF-lock, so it neither stalls concurrent ingest/query for the whole compaction nor can
        //    poison the mutex if an Arrow op panics (`inner` is never mid-mutated during the I/O).
        let (logs_snap, spans_snap, metrics_snap) = {
            let inner = self.inner.lock().unwrap();
            (
                inner.segments.clone(),
                inner.spans_segments.clone(),
                inner.metric_segments.clone(),
            )
        };

        // 2. Compute the merges off-lock. `compact_partition` is schema-agnostic (concat + sort by
        //    time, optional Tantivy index) so it merges scalar and List-column metric tables alike.
        let mut report = CompactionReport::default();
        let mut deferred_deletes: Vec<SegmentRef> = Vec::new();
        let new_logs = self.compact_partition(
            &dir,
            Table::Logs,
            "time",
            true,
            logs_snap,
            &mut report,
            &mut deferred_deletes,
        )?;
        let new_spans = self.compact_partition(
            &dir,
            Table::Spans,
            "start_time",
            true,
            spans_snap,
            &mut report,
            &mut deferred_deletes,
        )?;
        let mut new_metrics: Vec<(Table, Vec<SegmentRef>)> = Vec::new();
        for (table, segs) in metrics_snap {
            let merged = self.compact_partition(
                &dir,
                table,
                "time",
                false,
                segs,
                &mut report,
                &mut deferred_deletes,
            )?;
            new_metrics.push((table, merged));
        }

        // 3. Reconcile + persist under the lock: drop the compacted sources, add the merged results,
        //    and keep any segments a concurrent seal appended while we were off-lock.
        let deleted: std::collections::HashSet<String> = deferred_deletes
            .iter()
            .map(|s| s.relative_path.clone())
            .collect();
        {
            let mut inner = self.inner.lock().unwrap();
            reconcile_segments(&mut inner.segments, &deleted, new_logs);
            reconcile_segments(&mut inner.spans_segments, &deleted, new_spans);
            for (table, merged) in new_metrics {
                let buf = inner.metric_segments.entry(table).or_default();
                reconcile_segments(buf, &deleted, merged);
                if buf.is_empty() {
                    inner.metric_segments.remove(&table);
                }
            }
            let watermark = inner.watermark;
            // Manifest (now pointing at the merged segments) durable before the sources are deleted.
            self.persist_manifest(
                &inner.segments,
                &inner.spans_segments,
                &inner.metric_segments,
                watermark,
            )?;
        }
        // 4. …then reclaim the merged-away sources. A crash before this leaves them as orphans that
        //    `cleanup_orphans` removes on next open (the manifest no longer references them).
        for s in &deferred_deletes {
            delete_segment(&dir, s)?;
        }
        #[cfg(feature = "tracing")]
        tracing::debug!(report = ?report, "compaction complete");
        Ok(report)
    }

    /// Merge each day-partition group of `>1` segments into a single sorted segment.
    #[allow(clippy::too_many_arguments)]
    fn compact_partition(
        &self,
        dir: &Path,
        table: Table,
        time_col: &str,
        build_index: bool,
        segs: Vec<SegmentRef>,
        report: &mut CompactionReport,
        deferred_deletes: &mut Vec<SegmentRef>,
    ) -> Result<Vec<SegmentRef>> {
        let mut by_day: BTreeMap<String, Vec<SegmentRef>> = BTreeMap::new();
        for s in segs {
            by_day
                .entry(day_of_path(&s.relative_path))
                .or_default()
                .push(s);
        }

        let mut result = Vec::new();
        for group in by_day.into_values() {
            if group.len() <= 1 {
                result.extend(group);
                continue;
            }
            let mut batches = Vec::new();
            for s in &group {
                batches.extend(read_parquet_file(&dir.join(&s.relative_path))?);
            }
            if batches.is_empty() {
                result.extend(group);
                continue;
            }
            let sorted = concat_and_sort(&batches, time_col)?;
            let min_time = group
                .iter()
                .map(|s| s.min_time_unix_nano)
                .min()
                .unwrap_or(0);
            let max_time = group
                .iter()
                .map(|s| s.max_time_unix_nano)
                .max()
                .unwrap_or(0);
            let rows = sorted.num_rows() as u64;
            let (relative_path, abs) =
                self.write_segment_parquet(dir, table.as_str(), min_time, &sorted)?;
            if build_index {
                let tidx = abs.with_extension("tidx");
                match table {
                    Table::Spans => {
                        build_spans_sidecar(&tidx, &spans_batch_to_index_rows(&sorted))?
                    }
                    // Logs (and any other future text-indexed table) index the `body`.
                    _ => build_logs_sidecar(&tidx, &logs_batch_to_index_rows(&sorted))?,
                }
            }
            report.segments_merged += group.len() as u64;
            report.segments_created += 1;
            // Defer deleting the merged-away sources until the manifest that points at the new
            // merged segment is durable (see `compact`). Deleting them now would risk losing every
            // row if a crash struck before the manifest was persisted.
            deferred_deletes.extend(group);
            result.push(SegmentRef {
                relative_path,
                min_time_unix_nano: min_time,
                max_time_unix_nano: max_time,
                rows,
            });
        }
        Ok(result)
    }
}

/// Outcome of a compaction pass (ARCHITECTURE.md §7).
#[derive(Debug, Default, Clone)]
pub struct CompactionReport {
    pub segments_merged: u64,
    pub segments_created: u64,
}

/// The UTC-day component of a `<table>/<day>/<id>.parquet` relative path.
fn day_of_path(rel: &str) -> String {
    rel.split('/').nth(1).unwrap_or_default().to_owned()
}

fn read_parquet_file(path: &Path) -> Result<Vec<RecordBatch>> {
    let file = std::fs::File::open(path)?;
    let reader = ParquetRecordBatchReaderBuilder::try_new(file)
        .map_err(|e| Error::storage_io(Some(path.to_path_buf()), e))?
        .build()
        .map_err(|e| Error::storage_io(Some(path.to_path_buf()), e))?;
    let mut out = Vec::new();
    for b in reader {
        out.push(b.map_err(|e| Error::storage_io(Some(path.to_path_buf()), e))?);
    }
    Ok(out)
}

// ── mutable buffer (Vec<RecordBatch>) helpers ────────────────────────────────────────────

/// Encode `rows` to a `logs` [`RecordBatch`] and append it to the buffer, charging the row byte
/// estimate to `buffer_bytes` first (the seal-threshold accounting depends on this estimate, not the
/// Arrow memory size). Empty input is a no-op — no empty batch is pushed, so every buffered batch
/// has ≥1 row and seal's `concat_and_sort` never sees an empty input.
fn push_log_batch(inner: &mut Inner, rows: Vec<LogRow>, promote: &[String]) -> Result<()> {
    if rows.is_empty() {
        return Ok(());
    }
    inner.buffer_bytes += rows.iter().map(|r| r.approx_bytes()).sum::<usize>();
    let batch = rows_to_batch(&rows, promote)?;
    inner.buffer.push(batch);
    Ok(())
}

/// Encode `rows` to a `spans` [`RecordBatch`] and append it. Empty input is a no-op.
fn push_span_batch(inner: &mut Inner, rows: Vec<SpanRow>, promote: &[String]) -> Result<()> {
    if rows.is_empty() {
        return Ok(());
    }
    inner.buffer_bytes += rows.iter().map(|r| r.approx_bytes()).sum::<usize>();
    let batch = spans_rows_to_batch(&rows, promote)?;
    inner.spans_buffer.push(batch);
    Ok(())
}

/// Route scalar metric rows to their table buffers: partition by `r.table` (preserving each table's
/// relative order), then encode each non-empty group to one [`RecordBatch`] and append it. A single
/// OTLP request may carry both gauge and sum points, hence the per-table split into ≤1 batch each.
fn push_scalar_metric_batches(
    inner: &mut Inner,
    rows: Vec<ScalarMetricRow>,
    promote: &[String],
) -> Result<()> {
    let mut by_table: BTreeMap<Table, Vec<ScalarMetricRow>> = BTreeMap::new();
    for r in rows {
        inner.buffer_bytes += r.approx_bytes();
        by_table.entry(r.table).or_default().push(r);
    }
    for (table, group) in by_table {
        // A map key exists only after a push, so `group` is non-empty.
        let batch = scalar_metrics_rows_to_batch(&group, promote)?;
        inner.metric_buffers.entry(table).or_default().push(batch);
    }
    Ok(())
}

/// Encode `rows` to a `metrics_histogram` [`RecordBatch`] and append it. Empty input is a no-op.
fn push_histogram_batch(
    inner: &mut Inner,
    rows: Vec<HistogramRow>,
    promote: &[String],
) -> Result<()> {
    if rows.is_empty() {
        return Ok(());
    }
    inner.buffer_bytes += rows.iter().map(|r| r.approx_bytes()).sum::<usize>();
    let batch = histogram_rows_to_batch(&rows, promote)?;
    inner.histogram_buffer.push(batch);
    Ok(())
}

/// Encode `rows` to a `metrics_exp_histogram` [`RecordBatch`] and append it. Empty input is a no-op.
fn push_exp_histogram_batch(
    inner: &mut Inner,
    rows: Vec<ExpHistogramRow>,
    promote: &[String],
) -> Result<()> {
    if rows.is_empty() {
        return Ok(());
    }
    inner.buffer_bytes += rows.iter().map(|r| r.approx_bytes()).sum::<usize>();
    let batch = exp_histogram_rows_to_batch(&rows, promote)?;
    inner.exp_histogram_buffer.push(batch);
    Ok(())
}

/// Encode `rows` to a `metrics_summary` [`RecordBatch`] and append it. Empty input is a no-op.
fn push_summary_batch(inner: &mut Inner, rows: Vec<SummaryRow>, promote: &[String]) -> Result<()> {
    if rows.is_empty() {
        return Ok(());
    }
    inner.buffer_bytes += rows.iter().map(|r| r.approx_bytes()).sum::<usize>();
    let batch = summary_rows_to_batch(&rows, promote)?;
    inner.summary_buffer.push(batch);
    Ok(())
}

/// Total buffered row count across a table's frozen batches (for `stats()` and any row-count view).
fn buffered_rows(batches: &[RecordBatch]) -> usize {
    batches.iter().map(RecordBatch::num_rows).sum()
}

/// Snapshot a table's frozen buffer as one Arrow batch: concatenate the batches, or return an empty
/// batch carrying `schema` when the buffer is empty (queries see buffer ∪ segments, so the schema
/// must be present even with no rows). Non-destructive — the buffer is left intact.
fn concat_buffer(batches: &[RecordBatch], schema: SchemaRef) -> Result<RecordBatch> {
    use arrow::compute::concat_batches;
    concat_batches(&schema, batches).map_err(|e| Error::storage_ctx("concat buffer", e))
}

/// The `TimestampNanosecondArray` for a named time column of `batch`.
fn ts_column<'a>(batch: &'a RecordBatch, name: &str) -> &'a TimestampNanosecondArray {
    let idx = batch
        .schema()
        .index_of(name)
        .unwrap_or_else(|_| panic!("no `{name}` column"));
    batch
        .column(idx)
        .as_any()
        .downcast_ref::<TimestampNanosecondArray>()
        .unwrap_or_else(|| panic!("`{name}` is not a Timestamp(ns) column"))
}

/// The `UInt64Array` for a named column of `batch`.
fn u64_column<'a>(batch: &'a RecordBatch, name: &str) -> &'a UInt64Array {
    let idx = batch
        .schema()
        .index_of(name)
        .unwrap_or_else(|_| panic!("no `{name}` column"));
    batch
        .column(idx)
        .as_any()
        .downcast_ref::<UInt64Array>()
        .unwrap_or_else(|| panic!("`{name}` is not a UInt64 column"))
}

/// `(min, max)` of a time-sorted batch's time column: the first and last values (the batch is sorted
/// ascending by `concat_and_sort` before this is called). Empty batch → `(0, 0)`.
fn time_bounds(batch: &RecordBatch, time_col: &str) -> (i64, i64) {
    let t = ts_column(batch, time_col);
    if t.is_empty() {
        (0, 0)
    } else {
        (t.value(0), t.value(t.len() - 1))
    }
}

/// Concatenate batches and sort by the `time_col` column, ascending.
fn concat_and_sort(batches: &[RecordBatch], time_col: &str) -> Result<RecordBatch> {
    use arrow::compute::{concat_batches, sort_to_indices, take};
    let schema = batches[0].schema();
    let merged =
        concat_batches(&schema, batches).map_err(|e| Error::storage_ctx("concat segments", e))?;
    let time_idx = schema
        .index_of(time_col)
        .map_err(|e| Error::missing_column(time_col, e))?;
    let indices = sort_to_indices(merged.column(time_idx), None, None)
        .map_err(|e| Error::storage_ctx("sort segments", e))?;
    let cols: std::result::Result<Vec<ArrayRef>, _> = merged
        .columns()
        .iter()
        .map(|c| take(c, &indices, None))
        .collect();
    let cols = cols.map_err(|e| Error::storage_ctx("take sorted rows", e))?;
    RecordBatch::try_new(schema, cols).map_err(|e| Error::storage_ctx("build merged batch", e))
}

/// Build the per-segment Tantivy `.tidx` sidecar for a `logs` segment. Confined here so
/// `imbh_index` is referenced from exactly one place. Without the `search` feature this is a no-op
/// (and imbh-index is not compiled in), so `matches()` falls back to a full scan (§11).
#[cfg(feature = "search")]
fn build_logs_sidecar(tidx_path: &Path, rows: &[LogRow]) -> Result<()> {
    imbh_index::build_logs_index(tidx_path, rows)
}

#[cfg(not(feature = "search"))]
fn build_logs_sidecar(_tidx_path: &Path, _rows: &[LogRow]) -> Result<()> {
    Ok(())
}

/// Build the per-segment Tantivy `.tidx` sidecar for a `spans` segment (indexes the span `name`);
/// no-op without the `search` feature. Mirrors [`build_logs_sidecar`].
#[cfg(feature = "search")]
fn build_spans_sidecar(tidx_path: &Path, spans: &[SpanRow]) -> Result<()> {
    imbh_index::build_spans_index(tidx_path, spans)
}

#[cfg(not(feature = "search"))]
fn build_spans_sidecar(_tidx_path: &Path, _spans: &[SpanRow]) -> Result<()> {
    Ok(())
}

/// Extract the `LogRow`s the Tantivy index needs (body/service/severity_text + the canonical-JSON
/// `attributes` string) from a `logs` batch, preserving row order so the `row` ordinal stays
/// aligned. `attributes` feeds the index's `attrs` JSON field for attr-equality pushdown (§8/§9.2);
/// `resource`/`scope` are left empty because the index does not index them.
fn logs_batch_to_index_rows(batch: &RecordBatch) -> Vec<LogRow> {
    let body = str_column(batch, "body");
    let service = str_column(batch, "service");
    let severity = str_column(batch, "severity_text");
    let attributes = str_column(batch, "attributes");
    (0..batch.num_rows())
        .map(|i| LogRow {
            time_unix_nano: 0,
            observed_time_unix_nano: None,
            service: service[i].clone(),
            severity_number: 0,
            severity_text: severity[i].clone(),
            body: body[i].clone().unwrap_or_default(),
            attributes: attributes[i].clone().unwrap_or_default(),
            resource: String::new(),
            scope: String::new(),
            trace_id: None,
            span_id: None,
            flags: 0,
        })
        .collect()
}

/// Extract the `SpanRow`s the Tantivy index needs (name/service + the canonical-JSON `attributes`
/// string) from a `spans` batch, preserving row order so the `row` ordinal stays aligned. `name`
/// feeds the `body` field and `attributes` the `attrs` JSON field; the rest are placeholders.
fn spans_batch_to_index_rows(batch: &RecordBatch) -> Vec<SpanRow> {
    let name = str_column(batch, "name");
    let service = str_column(batch, "service");
    let attributes = str_column(batch, "attributes");
    (0..batch.num_rows())
        .map(|i| SpanRow {
            trace_id: [0u8; 16],
            span_id: [0u8; 8],
            parent_span_id: None,
            name: name[i].clone().unwrap_or_default(),
            kind: String::new(),
            start_time_unix_nano: 0,
            duration_ns: 0,
            status_code: String::new(),
            status_message: None,
            service: service[i].clone(),
            attributes: attributes[i].clone().unwrap_or_default(),
            resource: String::new(),
            scope: String::new(),
            events: None,
            links: None,
            trace_state: None,
            flags: 0,
        })
        .collect()
}

fn str_column(batch: &RecordBatch, name: &str) -> Vec<Option<String>> {
    let Ok(idx) = batch.schema().index_of(name) else {
        return vec![None; batch.num_rows()];
    };
    let col = batch.column(idx);
    // `service` is dict-encoded (`Dictionary(Int32, Utf8)`); cast to `Utf8` so the downcast below
    // works for both plain-string columns (body/name/severity_text) and dict columns.
    let cast_holder;
    let col: &dyn Array = if col.as_any().is::<StringArray>() {
        col.as_ref()
    } else {
        match arrow::compute::cast(col, &DataType::Utf8) {
            Ok(c) => {
                cast_holder = c;
                cast_holder.as_ref()
            }
            Err(_) => return vec![None; batch.num_rows()],
        }
    };
    match col.as_any().downcast_ref::<StringArray>() {
        Some(a) => (0..a.len())
            .map(|i| (!a.is_null(i)).then(|| a.value(i).to_owned()))
            .collect(),
        None => vec![None; batch.num_rows()],
    }
}

/// Per-table statistics (ARCHITECTURE.md §10.11).
#[derive(Debug, Clone)]
pub struct TableStats {
    pub table: Table,
    pub segment_count: u64,
    pub segment_rows: u64,
    pub buffer_rows: u64,
    pub min_time_unix_nano: Option<i64>,
    pub max_time_unix_nano: Option<i64>,
}

fn table_stats(table: Table, segs: &[SegmentRef], buffer_rows: usize) -> TableStats {
    TableStats {
        table,
        segment_count: segs.len() as u64,
        segment_rows: segs.iter().map(|s| s.rows).sum(),
        buffer_rows: buffer_rows as u64,
        min_time_unix_nano: segs.iter().map(|s| s.min_time_unix_nano).min(),
        max_time_unix_nano: segs.iter().map(|s| s.max_time_unix_nano).max(),
    }
}

/// Result of [`Storage::snapshot`].
#[derive(Debug, Clone)]
pub struct SnapshotInfo {
    pub dir: PathBuf,
    pub segments: u64,
}

/// Hard-link `src` to `dst`, falling back to a copy across filesystems.
fn link_or_copy(src: &Path, dst: &Path) -> Result<()> {
    match std::fs::hard_link(src, dst) {
        Ok(()) => Ok(()),
        Err(_) => {
            std::fs::copy(src, dst)?;
            Ok(())
        }
    }
}

/// Recreate `src` dir tree at `dst`, hard-linking (or copying) each file.
fn link_dir(src: &Path, dst: &Path) -> Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let target = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            link_dir(&entry.path(), &target)?;
        } else {
            link_or_copy(&entry.path(), &target)?;
        }
    }
    Ok(())
}

/// Outcome of a retention pass (ARCHITECTURE.md §7).
#[derive(Debug, Default, Clone)]
pub struct RetentionReport {
    pub segments_dropped: u64,
    pub bytes_freed: u64,
}

fn seal_threshold(budget: MemoryBudget) -> usize {
    (budget.total_bytes() / 4).max(DEFAULT_SEAL_BYTES)
}

/// Look up a promoted key in a row's **record-level `attributes`** scope. Only string values promote;
/// a non-string or absent key yields `None` (a NULL cell) — promoted label columns are text, like
/// `service`. This mirrors `json_get_str(attributes, key)` exactly (same scope, same string-only
/// rule), so the query layer can substitute the column for that scan with identical results. Resource
/// and scope attributes are deliberately **not** merged in: they are different scopes (a
/// record-attribute predicate must not see a resource value), and `service.name` is the one promoted
/// resource attribute, which has its own column. The key also stays in the JSON blob, so the column
/// is a projection, not a move (ARCHITECTURE.md §6.1).
fn lookup_promoted(attributes: &str, key: &str) -> Option<String> {
    match json_get(attributes, key) {
        Some(AnyValue::Str(v)) => Some(v),
        _ => None,
    }
}

/// Build the promoted label columns for a batch: one nullable `Dictionary(Int32,Utf8)` per
/// [`promoted_columns`] entry, in schema order, each cell resolved via [`lookup_promoted`] over the
/// row's record `attributes` JSON. `attributes` yields that JSON per row, in row order; the returned
/// arrays are appended after a signal's fixed columns. Empty `promote` returns no columns (zero cost).
fn build_promoted_columns<'a>(
    promote: &[String],
    attributes: impl ExactSizeIterator<Item = &'a str>,
) -> Vec<ArrayRef> {
    let cols = promoted_columns(promote);
    if cols.is_empty() {
        return Vec::new();
    }
    let mut builders: Vec<StringDictionaryBuilder<Int32Type>> = (0..cols.len())
        .map(|_| StringDictionaryBuilder::new())
        .collect();
    for attrs in attributes {
        for (b, key) in builders.iter_mut().zip(&cols) {
            b.append_option(lookup_promoted(attrs, key).as_deref());
        }
    }
    builders
        .into_iter()
        .map(|mut b| std::sync::Arc::new(b.finish()) as ArrayRef)
        .collect()
}

/// Build a `logs` [`RecordBatch`] from normalized rows.
fn rows_to_batch(rows: &[LogRow], promote: &[String]) -> Result<RecordBatch> {
    let mut time = TimestampNanosecondBuilder::with_capacity(rows.len());
    let mut observed = TimestampNanosecondBuilder::with_capacity(rows.len());
    let mut service = StringDictionaryBuilder::<Int32Type>::new();
    let mut sev_num = UInt8Builder::with_capacity(rows.len());
    let mut sev_text = StringBuilder::new();
    let mut body = StringBuilder::new();
    let mut attributes = StringBuilder::new();
    let mut resource = StringDictionaryBuilder::<Int32Type>::new();
    let mut scope = StringDictionaryBuilder::<Int32Type>::new();
    let mut trace_id = FixedSizeBinaryBuilder::new(16);
    let mut span_id = FixedSizeBinaryBuilder::new(8);
    let mut flags = UInt32Builder::with_capacity(rows.len());

    for r in rows {
        time.append_value(r.time_unix_nano);
        observed.append_option(r.observed_time_unix_nano);
        service.append_option(r.service.as_deref());
        sev_num.append_value(r.severity_number);
        sev_text.append_option(r.severity_text.as_deref());
        body.append_value(&r.body);
        attributes.append_value(&r.attributes);
        resource.append_value(&r.resource);
        scope.append_value(&r.scope);
        match &r.trace_id {
            Some(id) => trace_id
                .append_value(id)
                .map_err(|e| Error::build_batch("logs", e))?,
            None => trace_id.append_null(),
        }
        match &r.span_id {
            Some(id) => span_id
                .append_value(id)
                .map_err(|e| Error::build_batch("logs", e))?,
            None => span_id.append_null(),
        }
        flags.append_value(r.flags);
    }

    let mut columns: Vec<ArrayRef> = vec![
        std::sync::Arc::new(time.finish().with_timezone("UTC")),
        std::sync::Arc::new(observed.finish().with_timezone("UTC")),
        std::sync::Arc::new(service.finish()),
        std::sync::Arc::new(sev_num.finish()),
        std::sync::Arc::new(sev_text.finish()),
        std::sync::Arc::new(body.finish()),
        std::sync::Arc::new(attributes.finish()),
        std::sync::Arc::new(resource.finish()),
        std::sync::Arc::new(scope.finish()),
        std::sync::Arc::new(trace_id.finish()),
        std::sync::Arc::new(span_id.finish()),
        std::sync::Arc::new(flags.finish()),
    ];
    columns.extend(build_promoted_columns(
        promote,
        rows.iter().map(|r| r.attributes.as_str()),
    ));

    RecordBatch::try_new(logs_schema(promote), columns).map_err(|e| Error::build_batch("logs", e))
}

/// Build a `spans` [`RecordBatch`] from normalized span rows (schema order per `spans_schema`).
fn spans_rows_to_batch(rows: &[SpanRow], promote: &[String]) -> Result<RecordBatch> {
    let mut trace_id = FixedSizeBinaryBuilder::new(16);
    let mut span_id = FixedSizeBinaryBuilder::new(8);
    let mut parent = FixedSizeBinaryBuilder::new(8);
    let mut name = StringBuilder::new();
    let mut kind = StringBuilder::new();
    let mut start_time = TimestampNanosecondBuilder::with_capacity(rows.len());
    let mut duration = UInt64Builder::with_capacity(rows.len());
    let mut status_code = StringBuilder::new();
    let mut status_message = StringBuilder::new();
    let mut service = StringDictionaryBuilder::<Int32Type>::new();
    let mut attributes = StringBuilder::new();
    let mut resource = StringDictionaryBuilder::<Int32Type>::new();
    let mut scope = StringDictionaryBuilder::<Int32Type>::new();
    let mut events = StringBuilder::new();
    let mut links = StringBuilder::new();
    let mut trace_state = StringBuilder::new();
    let mut flags = UInt32Builder::with_capacity(rows.len());

    for r in rows {
        trace_id
            .append_value(r.trace_id)
            .map_err(|e| Error::build_batch("spans", e))?;
        span_id
            .append_value(r.span_id)
            .map_err(|e| Error::build_batch("spans", e))?;
        match &r.parent_span_id {
            Some(id) => parent
                .append_value(id)
                .map_err(|e| Error::build_batch("spans", e))?,
            None => parent.append_null(),
        }
        name.append_value(&r.name);
        kind.append_value(&r.kind);
        start_time.append_value(r.start_time_unix_nano);
        duration.append_value(r.duration_ns);
        status_code.append_value(&r.status_code);
        status_message.append_option(r.status_message.as_deref());
        service.append_option(r.service.as_deref());
        attributes.append_value(&r.attributes);
        resource.append_value(&r.resource);
        scope.append_value(&r.scope);
        events.append_option(r.events.as_deref());
        links.append_option(r.links.as_deref());
        trace_state.append_option(r.trace_state.as_deref());
        flags.append_value(r.flags);
    }

    let mut columns: Vec<ArrayRef> = vec![
        std::sync::Arc::new(trace_id.finish()),
        std::sync::Arc::new(span_id.finish()),
        std::sync::Arc::new(parent.finish()),
        std::sync::Arc::new(name.finish()),
        std::sync::Arc::new(kind.finish()),
        std::sync::Arc::new(start_time.finish().with_timezone("UTC")),
        std::sync::Arc::new(duration.finish()),
        std::sync::Arc::new(status_code.finish()),
        std::sync::Arc::new(status_message.finish()),
        std::sync::Arc::new(service.finish()),
        std::sync::Arc::new(attributes.finish()),
        std::sync::Arc::new(resource.finish()),
        std::sync::Arc::new(scope.finish()),
        std::sync::Arc::new(events.finish()),
        std::sync::Arc::new(links.finish()),
        std::sync::Arc::new(trace_state.finish()),
        std::sync::Arc::new(flags.finish()),
    ];
    columns.extend(build_promoted_columns(
        promote,
        rows.iter().map(|r| r.attributes.as_str()),
    ));

    RecordBatch::try_new(spans_schema(promote), columns).map_err(|e| Error::build_batch("spans", e))
}

/// Build a scalar-metric [`RecordBatch`] (schema order per `metric_scalar_schema`).
fn scalar_metrics_rows_to_batch(
    rows: &[ScalarMetricRow],
    promote: &[String],
) -> Result<RecordBatch> {
    use std::sync::Arc;
    let mut time = TimestampNanosecondBuilder::with_capacity(rows.len());
    let mut start = TimestampNanosecondBuilder::with_capacity(rows.len());
    let mut metric = StringBuilder::new();
    let mut unit = StringBuilder::new();
    let mut service = StringDictionaryBuilder::<Int32Type>::new();
    let mut attributes = StringBuilder::new();
    let mut resource = StringDictionaryBuilder::<Int32Type>::new();
    let mut scope = StringDictionaryBuilder::<Int32Type>::new();
    let mut flags = UInt32Builder::with_capacity(rows.len());
    let mut value = Float64Builder::with_capacity(rows.len());
    let mut temporality = StringBuilder::new();
    let mut is_monotonic = BooleanBuilder::new();
    let mut exemplars = StringBuilder::new();

    for r in rows {
        time.append_value(r.time_unix_nano);
        start.append_option(r.start_time_unix_nano);
        metric.append_value(&r.metric);
        unit.append_value(&r.unit);
        service.append_option(r.service.as_deref());
        attributes.append_value(&r.attributes);
        resource.append_value(&r.resource);
        scope.append_value(&r.scope);
        flags.append_value(r.flags);
        value.append_value(r.value);
        temporality.append_option(r.temporality.as_deref());
        is_monotonic.append_option(r.is_monotonic);
        exemplars.append_value(&r.exemplars);
    }

    let mut columns: Vec<ArrayRef> = vec![
        Arc::new(time.finish().with_timezone("UTC")),
        Arc::new(start.finish().with_timezone("UTC")),
        Arc::new(metric.finish()),
        Arc::new(unit.finish()),
        Arc::new(service.finish()),
        Arc::new(attributes.finish()),
        Arc::new(resource.finish()),
        Arc::new(scope.finish()),
        Arc::new(flags.finish()),
        Arc::new(value.finish()),
        Arc::new(temporality.finish()),
        Arc::new(is_monotonic.finish()),
        Arc::new(exemplars.finish()),
    ];
    columns.extend(build_promoted_columns(
        promote,
        rows.iter().map(|r| r.attributes.as_str()),
    ));

    RecordBatch::try_new(metric_scalar_schema(promote), columns)
        .map_err(|e| Error::build_batch("scalar-metric", e))
}

/// Build a histogram [`RecordBatch`] (schema order per `histogram_schema`). `explicit_bounds` and
/// `bucket_counts` become `List` columns via `ListBuilder`.
fn histogram_rows_to_batch(rows: &[HistogramRow], promote: &[String]) -> Result<RecordBatch> {
    use std::sync::Arc;
    let mut time = TimestampNanosecondBuilder::with_capacity(rows.len());
    let mut start = TimestampNanosecondBuilder::with_capacity(rows.len());
    let mut metric = StringBuilder::new();
    let mut unit = StringBuilder::new();
    let mut service = StringDictionaryBuilder::<Int32Type>::new();
    let mut attributes = StringBuilder::new();
    let mut resource = StringDictionaryBuilder::<Int32Type>::new();
    let mut scope = StringDictionaryBuilder::<Int32Type>::new();
    let mut flags = UInt32Builder::with_capacity(rows.len());
    let mut count = UInt64Builder::with_capacity(rows.len());
    let mut sum = Float64Builder::with_capacity(rows.len());
    let mut min = Float64Builder::with_capacity(rows.len());
    let mut max = Float64Builder::with_capacity(rows.len());
    let mut bounds = ListBuilder::new(Float64Builder::new());
    let mut counts = ListBuilder::new(UInt64Builder::new());
    let mut temporality = StringBuilder::new();
    let mut exemplars = StringBuilder::new();

    for r in rows {
        time.append_value(r.time_unix_nano);
        start.append_option(r.start_time_unix_nano);
        metric.append_value(&r.metric);
        unit.append_value(&r.unit);
        service.append_option(r.service.as_deref());
        attributes.append_value(&r.attributes);
        resource.append_value(&r.resource);
        scope.append_value(&r.scope);
        flags.append_value(r.flags);
        count.append_value(r.count);
        sum.append_option(r.sum);
        min.append_option(r.min);
        max.append_option(r.max);
        for &b in &r.explicit_bounds {
            bounds.values().append_value(b);
        }
        bounds.append(true);
        for &c in &r.bucket_counts {
            counts.values().append_value(c);
        }
        counts.append(true);
        temporality.append_option(r.temporality.as_deref());
        exemplars.append_value(&r.exemplars);
    }

    let mut columns: Vec<ArrayRef> = vec![
        Arc::new(time.finish().with_timezone("UTC")),
        Arc::new(start.finish().with_timezone("UTC")),
        Arc::new(metric.finish()),
        Arc::new(unit.finish()),
        Arc::new(service.finish()),
        Arc::new(attributes.finish()),
        Arc::new(resource.finish()),
        Arc::new(scope.finish()),
        Arc::new(flags.finish()),
        Arc::new(count.finish()),
        Arc::new(sum.finish()),
        Arc::new(min.finish()),
        Arc::new(max.finish()),
        Arc::new(bounds.finish()),
        Arc::new(counts.finish()),
        Arc::new(temporality.finish()),
        Arc::new(exemplars.finish()),
    ];
    columns.extend(build_promoted_columns(
        promote,
        rows.iter().map(|r| r.attributes.as_str()),
    ));

    RecordBatch::try_new(histogram_schema(promote), columns)
        .map_err(|e| Error::build_batch("histogram", e))
}

/// Build an exponential-histogram [`RecordBatch`] (schema order per `exp_histogram_schema`).
fn exp_histogram_rows_to_batch(
    rows: &[ExpHistogramRow],
    promote: &[String],
) -> Result<RecordBatch> {
    use std::sync::Arc;
    let mut time = TimestampNanosecondBuilder::with_capacity(rows.len());
    let mut start = TimestampNanosecondBuilder::with_capacity(rows.len());
    let mut metric = StringBuilder::new();
    let mut unit = StringBuilder::new();
    let mut service = StringDictionaryBuilder::<Int32Type>::new();
    let mut attributes = StringBuilder::new();
    let mut resource = StringDictionaryBuilder::<Int32Type>::new();
    let mut scope = StringDictionaryBuilder::<Int32Type>::new();
    let mut flags = UInt32Builder::with_capacity(rows.len());
    let mut count = UInt64Builder::with_capacity(rows.len());
    let mut sum = Float64Builder::with_capacity(rows.len());
    let mut min = Float64Builder::with_capacity(rows.len());
    let mut max = Float64Builder::with_capacity(rows.len());
    let mut scale = Int32Builder::with_capacity(rows.len());
    let mut zero_count = UInt64Builder::with_capacity(rows.len());
    let mut zero_threshold = Float64Builder::with_capacity(rows.len());
    let mut positive_offset = Int32Builder::with_capacity(rows.len());
    let mut positive_counts = ListBuilder::new(UInt64Builder::new());
    let mut negative_offset = Int32Builder::with_capacity(rows.len());
    let mut negative_counts = ListBuilder::new(UInt64Builder::new());
    let mut temporality = StringBuilder::new();
    let mut exemplars = StringBuilder::new();

    for r in rows {
        time.append_value(r.time_unix_nano);
        start.append_option(r.start_time_unix_nano);
        metric.append_value(&r.metric);
        unit.append_value(&r.unit);
        service.append_option(r.service.as_deref());
        attributes.append_value(&r.attributes);
        resource.append_value(&r.resource);
        scope.append_value(&r.scope);
        flags.append_value(r.flags);
        count.append_value(r.count);
        sum.append_option(r.sum);
        min.append_option(r.min);
        max.append_option(r.max);
        scale.append_value(r.scale);
        zero_count.append_value(r.zero_count);
        zero_threshold.append_value(r.zero_threshold);
        positive_offset.append_value(r.positive_offset);
        for &c in &r.positive_counts {
            positive_counts.values().append_value(c);
        }
        positive_counts.append(true);
        negative_offset.append_value(r.negative_offset);
        for &c in &r.negative_counts {
            negative_counts.values().append_value(c);
        }
        negative_counts.append(true);
        temporality.append_option(r.temporality.as_deref());
        exemplars.append_value(&r.exemplars);
    }

    let mut columns: Vec<ArrayRef> = vec![
        Arc::new(time.finish().with_timezone("UTC")),
        Arc::new(start.finish().with_timezone("UTC")),
        Arc::new(metric.finish()),
        Arc::new(unit.finish()),
        Arc::new(service.finish()),
        Arc::new(attributes.finish()),
        Arc::new(resource.finish()),
        Arc::new(scope.finish()),
        Arc::new(flags.finish()),
        Arc::new(count.finish()),
        Arc::new(sum.finish()),
        Arc::new(min.finish()),
        Arc::new(max.finish()),
        Arc::new(scale.finish()),
        Arc::new(zero_count.finish()),
        Arc::new(zero_threshold.finish()),
        Arc::new(positive_offset.finish()),
        Arc::new(positive_counts.finish()),
        Arc::new(negative_offset.finish()),
        Arc::new(negative_counts.finish()),
        Arc::new(temporality.finish()),
        Arc::new(exemplars.finish()),
    ];
    columns.extend(build_promoted_columns(
        promote,
        rows.iter().map(|r| r.attributes.as_str()),
    ));

    RecordBatch::try_new(exp_histogram_schema(promote), columns)
        .map_err(|e| Error::build_batch("exp-histogram", e))
}

/// Build a summary [`RecordBatch`] (schema order per `summary_schema`). `quantiles` and `values`
/// become `List<Float64>` columns.
fn summary_rows_to_batch(rows: &[SummaryRow], promote: &[String]) -> Result<RecordBatch> {
    use std::sync::Arc;
    let mut time = TimestampNanosecondBuilder::with_capacity(rows.len());
    let mut start = TimestampNanosecondBuilder::with_capacity(rows.len());
    let mut metric = StringBuilder::new();
    let mut unit = StringBuilder::new();
    let mut service = StringDictionaryBuilder::<Int32Type>::new();
    let mut attributes = StringBuilder::new();
    let mut resource = StringDictionaryBuilder::<Int32Type>::new();
    let mut scope = StringDictionaryBuilder::<Int32Type>::new();
    let mut flags = UInt32Builder::with_capacity(rows.len());
    let mut count = UInt64Builder::with_capacity(rows.len());
    let mut sum = Float64Builder::with_capacity(rows.len());
    let mut quantiles = ListBuilder::new(Float64Builder::new());
    let mut values = ListBuilder::new(Float64Builder::new());
    let mut temporality = StringBuilder::new();

    for r in rows {
        time.append_value(r.time_unix_nano);
        start.append_option(r.start_time_unix_nano);
        metric.append_value(&r.metric);
        unit.append_value(&r.unit);
        service.append_option(r.service.as_deref());
        attributes.append_value(&r.attributes);
        resource.append_value(&r.resource);
        scope.append_value(&r.scope);
        flags.append_value(r.flags);
        count.append_value(r.count);
        sum.append_value(r.sum);
        for &q in &r.quantiles {
            quantiles.values().append_value(q);
        }
        quantiles.append(true);
        for &v in &r.values {
            values.values().append_value(v);
        }
        values.append(true);
        temporality.append_null(); // summaries carry no temporality
    }

    let mut columns: Vec<ArrayRef> = vec![
        Arc::new(time.finish().with_timezone("UTC")),
        Arc::new(start.finish().with_timezone("UTC")),
        Arc::new(metric.finish()),
        Arc::new(unit.finish()),
        Arc::new(service.finish()),
        Arc::new(attributes.finish()),
        Arc::new(resource.finish()),
        Arc::new(scope.finish()),
        Arc::new(flags.finish()),
        Arc::new(count.finish()),
        Arc::new(sum.finish()),
        Arc::new(quantiles.finish()),
        Arc::new(values.finish()),
        Arc::new(temporality.finish()),
    ];
    columns.extend(build_promoted_columns(
        promote,
        rows.iter().map(|r| r.attributes.as_str()),
    ));

    RecordBatch::try_new(summary_schema(promote), columns)
        .map_err(|e| Error::build_batch("summary", e))
}

fn write_parquet(
    batch: &RecordBatch,
    path: &Path,
    compression: Compression,
    bloom_columns: &[&str],
) -> Result<()> {
    let comp = match compression {
        Compression::None => PqCompression::UNCOMPRESSED,
        Compression::Lz4 => PqCompression::LZ4_RAW,
        Compression::Zstd(level) => {
            let level =
                ZstdLevel::try_new(level).map_err(|e| Error::invalid_zstd_level(level, e))?;
            PqCompression::ZSTD(level)
        }
    };
    let mut builder = WriterProperties::builder().set_compression(comp);
    // Enable a Parquet bloom filter per requested column (spans' id columns) so a point lookup can
    // rule the segment out without reading it (ARCHITECTURE.md §8).
    for col in bloom_columns {
        builder = builder.set_column_bloom_filter_enabled(ColumnPath::from(*col), true);
    }
    let props = builder.build();
    let file = std::fs::File::create(path)?;
    let mut writer = ArrowWriter::try_new(file, batch.schema(), Some(props))
        .map_err(|e| Error::parquet(ParquetPhase::Writer, e))?;
    writer
        .write(batch)
        .map_err(|e| Error::parquet(ParquetPhase::Write, e))?;
    let file = writer
        .into_inner()
        .map_err(|e| Error::parquet(ParquetPhase::Close, e))?;
    // fsync the finished file before the caller renames it into place — otherwise a power loss
    // after the rename (but before the OS flushes) leaves a zero/short segment while the WAL frames
    // it captured may already be truncated (durability, ARCHITECTURE.md §7).
    file.sync_all()
        .map_err(|e| Error::parquet(ParquetPhase::Fsync, e))?;
    Ok(())
}

/// fsync a directory so a preceding create/rename inside it is durable (the standard
/// write-temp → fsync-temp → rename → fsync-dir pattern). Best-effort on platforms that reject
/// `open` of a directory: an error here is surfaced so the caller can treat the write as failed.
pub(crate) fn fsync_dir(dir: &Path) -> Result<()> {
    let f = std::fs::File::open(dir).map_err(|e| Error::storage_io(Some(dir.to_path_buf()), e))?;
    f.sync_all()
        .map_err(|e| Error::storage_io(Some(dir.to_path_buf()), e))
}

/// Put `restored` elements back at the front of `buffer`. Used on the seal error path over the
/// per-table `Vec<RecordBatch>` buffers: the un-sealed batches carry lower LSNs than anything
/// ingested concurrently while the failed seal ran (the seal lock is released during the writes),
/// so they must precede it to keep the buffer LSN-ordered.
fn prepend_front<T>(buffer: &mut Vec<T>, mut restored: Vec<T>) {
    restored.append(buffer); // restored = [un-sealed batches …, previously-buffered batches …]
    *buffer = restored;
}

/// Apply a compaction result to a live segment list: drop the compacted-away sources (`deleted`),
/// then add the segments the compaction produced (merged + untouched singletons), skipping any
/// already present. Preserves segments a concurrent seal appended while compaction ran off-lock.
fn reconcile_segments(
    current: &mut Vec<SegmentRef>,
    deleted: &std::collections::HashSet<String>,
    produced: Vec<SegmentRef>,
) {
    current.retain(|s| !deleted.contains(&s.relative_path));
    for p in produced {
        if !current.iter().any(|s| s.relative_path == p.relative_path) {
            current.push(p);
        }
    }
}

// ── segment sizing & deletion (retention) ───────────────────────────────────────────────

/// Total on-disk size of a segment: the Parquet file plus its `.tidx` sidecar directory.
fn segment_size(dir: &Path, seg: &SegmentRef) -> u64 {
    let parquet = dir.join(&seg.relative_path);
    let parquet_size = std::fs::metadata(&parquet).map(|m| m.len()).unwrap_or(0);
    parquet_size + dir_size(&parquet.with_extension("tidx"))
}

fn dir_size(path: &Path) -> u64 {
    let mut total = 0;
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            match entry.file_type() {
                Ok(ft) if ft.is_dir() => total += dir_size(&entry.path()),
                Ok(_) => total += entry.metadata().map(|m| m.len()).unwrap_or(0),
                Err(_) => {}
            }
        }
    }
    total
}

/// Delete a segment's Parquet file and `.tidx` sidecar (ignoring already-absent paths).
fn delete_segment(dir: &Path, seg: &SegmentRef) -> Result<()> {
    let parquet = dir.join(&seg.relative_path);
    remove_file_if_exists(&parquet)?;
    let tidx = parquet.with_extension("tidx");
    match std::fs::remove_dir_all(&tidx) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(Error::storage_io(Some(tidx.clone()), e)),
    }
}

fn remove_file_if_exists(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(Error::storage_io(Some(path.to_path_buf()), e)),
    }
}

/// A consistent, point-in-time on-disk snapshot for a **read-only** query (ARCHITECTURE.md §5): the
/// manifest's sealed segments at some watermark `W`, plus the writer's live WAL frames past `W` (the
/// not-yet-sealed tail). A separate reader process cannot see the writer's in-RAM buffer, but these
/// two on-disk artifacts reconstruct the same queryable state. The facade decodes `pending` through
/// the same replay path as open to materialize the reader buffer, then unions it with the segments.
pub struct DiskSnapshot {
    /// The DB directory the segment relative paths are rooted at.
    pub dir: PathBuf,
    /// Sealed-segment watermark: every row with `lsn <= watermark` lives in the `*_segments` below.
    pub watermark: u64,
    /// Sealed `logs` segments (manifest order).
    pub logs_segments: Vec<SegmentRef>,
    /// Sealed `spans` segments.
    pub spans_segments: Vec<SegmentRef>,
    /// Sealed metric-table segments, keyed by [`Table`] (scalar tables + histogram/exp/summary).
    pub metric_segments: BTreeMap<Table, Vec<SegmentRef>>,
    /// WAL frames with `lsn > watermark` — the unsealed tail to replay into the reader buffer.
    pub pending: Vec<WalRecord>,
}

/// A consistent view of the **writer's** own tables (live buffers + sealed segments), captured
/// under a single `inner` lock by [`Storage::query_snapshot`]. Used by the read-write facade query
/// path so a background seal cannot make an intra-process query double-count. The buffers are
/// already concatenated to one batch per table (empty batch with schema when the buffer is empty).
pub struct QuerySnapshot {
    /// The DB directory the segment relative paths are rooted at (`None` for in-memory DBs).
    dir: Option<PathBuf>,
    /// The `logs` buffer as one batch.
    pub logs_buffer: RecordBatch,
    /// Sealed `logs` segments (manifest order).
    pub logs_segments: Vec<SegmentRef>,
    /// The `spans` buffer as one batch.
    pub spans_buffer: RecordBatch,
    /// Sealed `spans` segments.
    pub spans_segments: Vec<SegmentRef>,
    /// Scalar metric buffers (`metrics_gauge`/`metrics_sum`), one batch each; both keys always present.
    pub metric_buffers: BTreeMap<Table, RecordBatch>,
    /// The `metrics_histogram` buffer as one batch.
    pub histogram_buffer: RecordBatch,
    /// The `metrics_exp_histogram` buffer as one batch.
    pub exp_histogram_buffer: RecordBatch,
    /// The `metrics_summary` buffer as one batch.
    pub summary_buffer: RecordBatch,
    /// Sealed segments for every metric table (scalar + histogram/exp/summary), keyed by [`Table`].
    pub metric_segments: BTreeMap<Table, Vec<SegmentRef>>,
}

impl QuerySnapshot {
    /// Absolute paths of a segment list rooted at this snapshot's `dir` (empty for in-memory DBs).
    pub fn abs_paths(&self, segs: &[SegmentRef]) -> Vec<PathBuf> {
        match &self.dir {
            Some(dir) => segs.iter().map(|s| dir.join(&s.relative_path)).collect(),
            None => Vec::new(),
        }
    }

    /// The sealed segments of one metric `table` (empty when none).
    pub fn metric(&self, table: Table) -> Vec<SegmentRef> {
        self.metric_segments
            .get(&table)
            .cloned()
            .unwrap_or_default()
    }

    /// The buffer batch of a scalar metric `table`. Scalar keys are always populated; other tables
    /// have dedicated fields ([`Self::histogram_buffer`] etc.), so a miss returns an empty batch.
    pub fn metric_buffer(&self, table: Table) -> Result<RecordBatch> {
        match self.metric_buffers.get(&table) {
            Some(b) => Ok(b.clone()),
            // Dead in practice (scalar keys are always populated); the empty batch carries the
            // fixed schema and the query layer's `coerce` null-fills any promoted columns.
            None => concat_buffer(&[], metric_scalar_schema(&[])),
        }
    }
}

impl DiskSnapshot {
    /// Absolute paths of a segment list rooted at this snapshot's `dir`.
    pub fn abs_paths(&self, segs: &[SegmentRef]) -> Vec<PathBuf> {
        segs.iter()
            .map(|s| self.dir.join(&s.relative_path))
            .collect()
    }

    /// The sealed segments of one metric `table` (empty when none), for symmetry with the writer's
    /// per-table `segments_metric`.
    pub fn metric(&self, table: Table) -> Vec<SegmentRef> {
        self.metric_segments
            .get(&table)
            .cloned()
            .unwrap_or_default()
    }
}

/// Read a consistent [`DiskSnapshot`] for a reader, using the **manifest re-check bracket**
/// (ARCHITECTURE.md §5): read the manifest (watermark `W` + segments), scan the WAL frames, then
/// re-read the manifest; if the watermark advanced — a seal committed mid-read — retry. On a stable
/// bracket the segments (`lsn <= W`) and the pending frames (`lsn > W`) are provably disjoint (no
/// double-count), and the writer cannot yet have reclaimed any frame `> W` (reclaim runs only after
/// the manifest that supersedes them is durable), so nothing is dropped. Bounded by
/// [`SNAPSHOT_RECHECK_TRIES`]; a raced WAL-segment reclaim during the scan is tolerated by
/// [`wal::read_all_frames`] (a `NotFound` segment is skipped).
pub fn read_disk_snapshot(dir: impl AsRef<Path>) -> Result<DiskSnapshot> {
    // A fresh cursor scans every segment from byte 0 — identical to the pre-incremental behavior.
    let mut cursor = WalTailCursor::default();
    read_disk_snapshot_incremental(dir, &mut cursor)
}

/// Like [`read_disk_snapshot`], but reuses a caller-held [`WalTailCursor`] so a long-lived read-only
/// handle scans only the WAL bytes appended since its previous snapshot (ARCHITECTURE.md §5) instead
/// of re-reading and re-decoding the whole tail per query. The returned snapshot is identical to a
/// fresh [`read_disk_snapshot`] at the same instant — the cursor is purely a performance cache — and
/// the same manifest re-check bracket guarantees `segments (lsn <= W)` and `pending (lsn > W)` are
/// disjoint and complete. The cursor accumulates the tail across calls and prunes the now-sealed
/// prefix each time, so its memory tracks the live (unsealed) tail, not the whole history.
pub fn read_disk_snapshot_incremental(
    dir: impl AsRef<Path>,
    cursor: &mut WalTailCursor,
) -> Result<DiskSnapshot> {
    let dir = dir.as_ref();
    let mut manifest = manifest::read(dir)?;
    for _ in 0..SNAPSHOT_RECHECK_TRIES {
        let watermark = manifest.watermark;
        // Scan only the newly appended bytes across segments into the cursor's accumulated tail.
        cursor.advance(dir)?;
        let recheck = manifest::read(dir)?;
        if recheck.watermark == watermark {
            // Drop the sealed prefix (now durable in segments); the remainder is exactly `lsn > W`.
            cursor.prune_sealed(watermark);
            let pending: Vec<WalRecord> = cursor
                .frames()
                .iter()
                .filter(|r| r.lsn > watermark)
                .cloned()
                .collect();
            return Ok(DiskSnapshot {
                dir: dir.to_path_buf(),
                watermark,
                logs_segments: manifest.logs,
                spans_segments: manifest.spans,
                metric_segments: manifest.metrics,
                pending,
            });
        }
        // A seal landed between the two manifest reads; re-bracket against the newer watermark.
        manifest = recheck;
    }
    Err(Error::storage_msg(
        "read_disk_snapshot: manifest watermark did not stabilize (writer sealing continuously)",
    ))
}

pub(crate) fn table_from_manifest_name(s: &str) -> Option<Table> {
    match s {
        "metrics_gauge" => Some(Table::MetricsGauge),
        "metrics_sum" => Some(Table::MetricsSum),
        "metrics_histogram" => Some(Table::MetricsHistogram),
        "metrics_exp_histogram" => Some(Table::MetricsExpHistogram),
        "metrics_summary" => Some(Table::MetricsSummary),
        _ => None,
    }
}

/// Delete orphan segment debris under `dir` on open (ARCHITECTURE.md §7): `*.parquet` files (and their
/// `*.tidx` sidecar dirs) not referenced by the manifest, plus stray `*.tmp` / `*.compact` temps
/// from interrupted segment/manifest/WAL writes. Best-effort — a failed unlink just leaves the file,
/// never a correctness problem, since the manifest is the source of truth and replay re-derives any
/// unsealed data. The manifest is written last on every mutation, so anything it does not name is
/// dead.
fn cleanup_orphans(dir: &Path, manifest: &Manifest, active_manifest: Option<u64>) {
    let mut referenced: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    for s in manifest
        .logs
        .iter()
        .chain(manifest.spans.iter())
        .chain(manifest.metrics.values().flatten())
    {
        let p = dir.join(&s.relative_path);
        referenced.insert(p.with_extension("tidx"));
        referenced.insert(p);
    }
    // The live manifest log (named by CURRENT) must be kept; stray `MANIFEST-*` from an interrupted
    // roll must go. `active_manifest` is `None` for a DB whose manifest is not yet materialized.
    let active_name = active_manifest.map(|n| format!("MANIFEST-{n:06}"));

    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(ftype) = entry.file_type() else {
                continue;
            };
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if ftype.is_dir() {
                if ext == "tidx" {
                    if !referenced.contains(&path) {
                        let _ = std::fs::remove_dir_all(&path);
                    }
                    continue; // never descend into a sidecar index
                }
                stack.push(path);
            } else if ext == "parquet" {
                if !referenced.contains(&path) {
                    let _ = std::fs::remove_file(&path);
                }
            } else if ext == "tmp" || ext == "compact" {
                let _ = std::fs::remove_file(&path); // interrupted temp write (incl. CURRENT.tmp)
            } else if d == dir && name.starts_with("MANIFEST-") {
                // A superseded/partial roll target the live CURRENT does not name.
                if active_name.as_deref() != Some(name) {
                    let _ = std::fs::remove_file(&path);
                }
            }
        }
    }
}

/// The `wal_mode` token persisted in [`INFO_FILE`] — the reader only needs `off` vs. not-off.
fn wal_mode_token(mode: WalMode) -> &'static str {
    match mode {
        WalMode::Off => "off",
        WalMode::Interval(_) => "interval",
        WalMode::Always => "always",
    }
}

/// Persist the writer's WAL mode to [`INFO_FILE`] (temp → rename, so a reader never sees a torn
/// line). Best-effort: a hint for readers, never a correctness input for the writer, so any I/O error
/// is swallowed rather than failing the open.
fn write_db_info(dir: &Path, wal_mode: WalMode) {
    let text = format!("wal_mode\t{}\n", wal_mode_token(wal_mode));
    let tmp = dir.join(format!("{INFO_FILE}.tmp"));
    let final_path = dir.join(INFO_FILE);
    let wrote =
        std::fs::write(&tmp, text.as_bytes()).and_then(|()| std::fs::rename(&tmp, &final_path));
    if wrote.is_err() {
        let _ = std::fs::remove_file(&tmp); // don't leave a stray temp behind
    }
}

/// Whether the writer that last opened `dir` advertised a **disabled** WAL (via [`INFO_FILE`]).
/// `true` only when the marker is present *and* says `off`; a missing / unparseable marker returns
/// `false` (unknown ⇒ never reject), so a DB written before this marker existed reads as before.
pub fn writer_wal_disabled(dir: impl AsRef<Path>) -> bool {
    let Ok(text) = std::fs::read_to_string(dir.as_ref().join(INFO_FILE)) else {
        return false;
    };
    text.lines().any(|line| {
        let mut it = line.split('\t');
        it.next() == Some("wal_mode") && it.next() == Some("off")
    })
}

// ── date partitioning ───────────────────────────────────────────────────────────────────

/// UTC `YYYY-MM-DD` for a unix-nanos instant (partition directory, ARCHITECTURE.md §7).
fn utc_date_string(unix_nanos: i64) -> String {
    let days = unix_nanos.div_euclid(86_400_000_000_000);
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Days-since-epoch → (year, month, day), via Howard Hinnant's `civil_from_days`.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(time: i64, service: &str, body: &str) -> LogRow {
        LogRow {
            time_unix_nano: time,
            observed_time_unix_nano: None,
            service: Some(service.to_owned()),
            severity_number: 9,
            severity_text: Some("INFO".to_owned()),
            body: body.to_owned(),
            attributes: "{}".to_owned(),
            resource: "{}".to_owned(),
            scope: "{}".to_owned(),
            trace_id: None,
            span_id: None,
            flags: 0,
        }
    }

    fn open(dir: &Path, wal: WalMode) -> Storage {
        Storage::open(
            dir,
            Compression::default(),
            wal,
            Retention::none(),
            MemoryBudget::default(),
        )
        .unwrap()
    }

    /// Under `WalMode::Always`, appending with `sync_now = false` (the async-worker path) does not
    /// advance `durable_through`; one `group_commit` fsyncs the burst and advances it to the highest
    /// appended LSN.
    #[test]
    fn group_commit_batches_fsync_and_advances_durable() {
        let dir = tempfile::tempdir().unwrap();
        let s = open(dir.path(), WalMode::Always);
        let (_lsn1, d1) = s
            .ingest(SIGNAL_LOGS, b"l1", vec![row(1, "a", "x")], false)
            .unwrap();
        let (lsn2, d2) = s
            .ingest(SIGNAL_LOGS, b"l2", vec![row(2, "b", "y")], false)
            .unwrap();
        assert!(!d1 && !d2, "sync_now=false never fsyncs inline");
        assert!(
            s.durable_through() < Some(lsn2),
            "durable must not advance before the group commit"
        );
        s.group_commit().unwrap();
        assert_eq!(
            s.durable_through(),
            Some(lsn2),
            "one group commit makes the whole burst durable"
        );
    }

    /// `group_commit` is a no-op unless `WalMode::Always` — `Interval`/`Off` never fsync per-append.
    #[test]
    fn group_commit_is_noop_without_always() {
        let dir = tempfile::tempdir().unwrap();
        let s = open(
            dir.path(),
            WalMode::Interval(std::time::Duration::from_secs(1)),
        );
        s.ingest(SIGNAL_LOGS, b"l1", vec![row(1, "a", "x")], false)
            .unwrap();
        let before = s.durable_through();
        s.group_commit().unwrap();
        assert_eq!(
            s.durable_through(),
            before,
            "Interval group_commit does not force durability"
        );
    }

    fn span_row(trace_id: [u8; 16], span_id: [u8; 8]) -> SpanRow {
        SpanRow {
            trace_id,
            span_id,
            parent_span_id: None,
            name: "GET /x".to_owned(),
            kind: "SERVER".to_owned(),
            start_time_unix_nano: 1,
            duration_ns: 5,
            status_code: "OK".to_owned(),
            status_message: None,
            service: Some("svc".to_owned()),
            attributes: "{}".to_owned(),
            resource: "{}".to_owned(),
            scope: "{}".to_owned(),
            events: None,
            links: None,
            trace_state: None,
            flags: 0,
        }
    }

    /// `true` iff column `col_idx` of the first row group in `path` carries a Parquet bloom filter.
    fn has_bloom(path: &Path, col_idx: usize) -> bool {
        let file = std::fs::File::open(path).unwrap();
        let builder = ParquetRecordBatchReaderBuilder::try_new(file).unwrap();
        builder
            .get_row_group_column_bloom_filter(0, col_idx)
            .unwrap()
            .is_some()
    }

    /// The spans writer sets bloom filters on the id columns (`trace_id`, `span_id`) so the query
    /// provider can prune segments on a point lookup (ARCHITECTURE.md §8); logs get none. This is the
    /// write half of that feature — it must land together with the read-side pruning.
    #[test]
    fn spans_segment_has_id_bloom_filters_logs_do_not() {
        let dir = tempfile::tempdir().unwrap();
        let s = open(dir.path(), WalMode::Always);
        s.ingest_traces(b"t1", vec![span_row([0xAA; 16], [0x01; 8])], true)
            .unwrap();
        let spans_seg = s.seal().unwrap().expect("spans segment");
        let spans_path = dir.path().join(&spans_seg.relative_path);
        // spans schema: trace_id is column 0, span_id column 1.
        assert!(
            has_bloom(&spans_path, 0),
            "spans trace_id has a bloom filter"
        );
        assert!(
            has_bloom(&spans_path, 1),
            "spans span_id has a bloom filter"
        );

        // Verify a bloom filter's contents: the ingested id present, an absent id ruled out.
        let file = std::fs::File::open(&spans_path).unwrap();
        let builder = ParquetRecordBatchReaderBuilder::try_new(file).unwrap();
        let sbbf = builder
            .get_row_group_column_bloom_filter(0, 0)
            .unwrap()
            .unwrap();
        assert!(sbbf.check([0xAA_u8; 16].as_slice()), "ingested id present");
        assert!(!sbbf.check([0xCC_u8; 16].as_slice()), "absent id ruled out");

        // Logs must NOT carry an id bloom filter (blooms cost bytes; only spans are point-looked-up).
        s.ingest(SIGNAL_LOGS, b"l1", vec![row(1, "a", "x")], true)
            .unwrap();
        let logs_seg = s.seal().unwrap().expect("logs segment");
        let logs_path = dir.path().join(&logs_seg.relative_path);
        // logs schema: trace_id is column 9, span_id column 10.
        assert!(
            !has_bloom(&logs_path, 9),
            "logs trace_id has no bloom filter"
        );
        assert!(
            !has_bloom(&logs_path, 10),
            "logs span_id has no bloom filter"
        );
    }

    #[test]
    fn retention_by_age_drops_old_segments() {
        let dir = tempfile::tempdir().unwrap();
        let s = Storage::open(
            dir.path(),
            Compression::default(),
            WalMode::Always,
            Retention::days(1),
            MemoryBudget::default(),
        )
        .unwrap();
        // Ancient segment (1970) → outside a 1-day window.
        s.ingest(SIGNAL_LOGS, b"old", vec![row(1, "a", "x")], true)
            .unwrap();
        let old = s.seal().unwrap().expect("segment");
        // Fresh segment (now) → inside the window.
        let now = Timestamp::now().0;
        s.ingest(SIGNAL_LOGS, b"new", vec![row(now, "a", "y")], true)
            .unwrap();
        s.seal().unwrap().expect("segment");
        assert_eq!(s.segments().len(), 2);

        let report = s.retain().unwrap();
        assert_eq!(report.segments_dropped, 1);
        assert!(report.bytes_freed > 0);
        assert_eq!(s.segments().len(), 1);
        assert!(!dir.path().join(&old.relative_path).exists());
        assert!(
            !dir.path()
                .join(&old.relative_path)
                .with_extension("tidx")
                .exists()
        );
    }

    #[test]
    fn retention_by_disk_budget_drops_segments() {
        let dir = tempfile::tempdir().unwrap();
        let s = Storage::open(
            dir.path(),
            Compression::default(),
            WalMode::Always,
            Retention::none().max_disk_bytes(0),
            MemoryBudget::default(),
        )
        .unwrap();
        s.ingest(SIGNAL_LOGS, b"r1", vec![row(1, "a", "x")], true)
            .unwrap();
        s.seal().unwrap().expect("segment");
        let report = s.retain().unwrap();
        assert_eq!(report.segments_dropped, 1);
        assert!(s.segments().is_empty());
    }

    #[test]
    fn writer_lock_is_exclusive_and_released_on_drop() {
        let dir = tempfile::tempdir().unwrap();
        let s1 = open(dir.path(), WalMode::Always);
        // A second writer on the same directory fails fast with a lock-held open error.
        // (`Storage` is not `Debug`, so match rather than `unwrap_err`.)
        let err = match Storage::open(
            dir.path(),
            Compression::default(),
            WalMode::Always,
            Retention::none(),
            MemoryBudget::default(),
        ) {
            Ok(_) => panic!("a second writer must be rejected while the first holds the lock"),
            Err(e) => e,
        };
        assert!(format!("{err}").contains("lock held"), "{err}");
        // A read-only view never contends for the lock.
        let _ro =
            Storage::open_read_only(dir.path(), Compression::default(), MemoryBudget::default())
                .unwrap();
        // Dropping the writer releases the lock; a fresh writer can then open.
        drop(s1);
        let _s2 = open(dir.path(), WalMode::Always);
    }

    #[test]
    fn read_only_view_sees_wal_tail_and_refuses_writes() {
        let dir = tempfile::tempdir().unwrap();
        let w = open(dir.path(), WalMode::Always);
        w.ingest(SIGNAL_LOGS, b"l1", vec![row(1, "a", "x")], true)
            .unwrap();

        let ro =
            Storage::open_read_only(dir.path(), Compression::default(), MemoryBudget::default())
                .unwrap();
        assert!(ro.is_read_only());
        // A write on the read-only view refuses.
        let err = ro
            .ingest(SIGNAL_LOGS, b"l2", vec![row(2, "b", "y")], true)
            .unwrap_err();
        assert!(format!("{err}").contains("read-only"), "{err}");

        // The unsealed WAL frame is visible to a fresh disk snapshot before any seal.
        let snap = read_disk_snapshot(dir.path()).unwrap();
        assert_eq!(snap.watermark, 0, "nothing sealed yet");
        assert_eq!(snap.pending.len(), 1, "one unsealed frame in the tail");
        assert!(snap.logs_segments.is_empty());

        // After a seal, the frame is captured by a segment; the watermark advances and the tail is
        // reclaimed — segments ∪ tail stays exactly one row's worth (no double-count, no drop).
        w.seal().unwrap().expect("logs segment");
        let snap2 = read_disk_snapshot(dir.path()).unwrap();
        assert_eq!(snap2.watermark, 1);
        assert_eq!(snap2.logs_segments.len(), 1);
        assert!(
            snap2.pending.is_empty(),
            "the sealed frame is no longer in the tail"
        );
    }

    /// A reused [`WalTailCursor`] (the incremental reader path) yields byte-for-byte the same snapshot
    /// as a fresh full scan at every step — across appends, a seal (watermark advance drops the sealed
    /// prefix), and post-seal appends. Guards that the offset/monotonicity bookkeeping stays exact.
    #[test]
    fn incremental_cursor_matches_fresh_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let w = open(dir.path(), WalMode::Always);
        let mut cursor = WalTailCursor::default();

        // Compare the incremental snapshot (reused cursor) against a fresh full scan at this instant.
        let assert_matches = |cursor: &mut WalTailCursor| {
            let inc = read_disk_snapshot_incremental(dir.path(), cursor).unwrap();
            let fresh = read_disk_snapshot(dir.path()).unwrap();
            assert_eq!(inc.watermark, fresh.watermark, "watermark");
            let key = |s: &DiskSnapshot| -> Vec<(u64, u8, Vec<u8>)> {
                s.pending
                    .iter()
                    .map(|r| (r.lsn, r.signal, r.payload.clone()))
                    .collect()
            };
            assert_eq!(key(&inc), key(&fresh), "pending tail");
            inc
        };

        // 1. Empty.
        assert!(assert_matches(&mut cursor).pending.is_empty());

        // 2. Two appends — the second refresh must scan only the newly written frame.
        w.ingest(SIGNAL_LOGS, b"l1", vec![row(1, "a", "x")], true)
            .unwrap();
        assert_eq!(assert_matches(&mut cursor).pending.len(), 1);
        w.ingest(SIGNAL_LOGS, b"l2", vec![row(2, "b", "y")], true)
            .unwrap();
        let after = assert_matches(&mut cursor);
        assert_eq!(after.pending.len(), 2, "both log frames in the tail");
        assert!(
            after.pending.windows(2).all(|w| w[0].lsn < w[1].lsn),
            "tail is LSN-ordered"
        );

        // 3. Seal — the watermark advances past every appended frame, so the tail empties.
        w.seal().unwrap();
        let sealed = assert_matches(&mut cursor);
        assert!(sealed.watermark >= 2, "watermark advanced past the seal");
        assert!(
            sealed.pending.is_empty(),
            "sealed prefix pruned from the tail"
        );

        // 4. Post-seal append — a fresh frame beyond the new watermark reappears in the tail.
        w.ingest(SIGNAL_LOGS, b"l3", vec![row(3, "c", "z")], true)
            .unwrap();
        let post = assert_matches(&mut cursor);
        assert_eq!(post.pending.len(), 1, "only the post-seal frame is pending");
        assert!(post.pending[0].lsn > sealed.watermark);
    }

    /// A crash mid-roll can leave a stray `MANIFEST-<next>` (written before the `CURRENT` flip) and a
    /// leftover `CURRENT.tmp`. On reopen, `cleanup_orphans` removes both while keeping the live log and
    /// the data intact (ARCHITECTURE.md §7).
    #[test]
    fn open_cleans_stray_manifest_files_from_an_interrupted_roll() {
        let dir = tempfile::tempdir().unwrap();
        let w = open(dir.path(), WalMode::Always);
        w.ingest(SIGNAL_LOGS, b"l1", vec![row(1, "a", "x")], true)
            .unwrap();
        w.seal().unwrap(); // writes MANIFEST-000001 + CURRENT (active = 1)
        drop(w); // release the writer lock

        // Simulate interrupted-roll debris: a half-written next checkpoint + a leftover temp pointer.
        std::fs::write(dir.path().join("MANIFEST-000002"), b"garbage").unwrap();
        std::fs::write(dir.path().join("CURRENT.tmp"), b"MANIFEST-000002\n").unwrap();

        let _w2 = open(dir.path(), WalMode::Always);
        assert!(
            !dir.path().join("MANIFEST-000002").exists(),
            "stray roll target removed"
        );
        assert!(
            !dir.path().join("CURRENT.tmp").exists(),
            "leftover temp removed"
        );
        assert!(
            dir.path().join("MANIFEST-000001").exists(),
            "the live manifest is kept"
        );
        // The data is intact: the sealed segment is still listed.
        let snap = read_disk_snapshot(dir.path()).unwrap();
        assert_eq!(snap.watermark, 1);
        assert_eq!(snap.logs_segments.len(), 1);
    }

    #[test]
    fn db_info_marker_advertises_wal_mode_to_readers() {
        let dir = tempfile::tempdir().unwrap();
        // No writer has opened yet ⇒ no marker ⇒ "unknown", which must not read as disabled.
        assert!(
            !writer_wal_disabled(dir.path()),
            "absent marker ⇒ not disabled (pre-marker DBs read as before)"
        );

        let open = |mode| {
            // Each writer drops (releasing the lock) before the next opens.
            let _s = Storage::open(
                dir.path(),
                Compression::default(),
                mode,
                Retention::none(),
                MemoryBudget::default(),
            )
            .unwrap();
        };

        open(WalMode::Off);
        assert!(
            writer_wal_disabled(dir.path()),
            "a WAL-off writer advertises off"
        );
        open(WalMode::Always);
        assert!(
            !writer_wal_disabled(dir.path()),
            "a WAL-on writer clears the off marker"
        );
        open(WalMode::Interval(std::time::Duration::from_secs(1)));
        assert!(
            !writer_wal_disabled(dir.path()),
            "interval WAL is not disabled"
        );
    }

    #[test]
    fn buffer_snapshot_has_rows() {
        let s = Storage::in_memory(Compression::default(), MemoryBudget::default());
        s.append_logs(vec![row(1, "a", "x"), row(2, "b", "y")]);
        let batch = s.buffer_snapshot().unwrap();
        assert_eq!(batch.num_rows(), 2);
        assert_eq!(batch.num_columns(), 12);
    }

    #[test]
    fn in_memory_seal_is_noop() {
        let s = Storage::in_memory(Compression::default(), MemoryBudget::default());
        s.append_logs(vec![row(1, "a", "x")]);
        assert!(s.seal().unwrap().is_none());
        assert_eq!(s.buffer_snapshot().unwrap().num_rows(), 1);
    }

    #[test]
    fn seal_if_full_seals_only_past_threshold() {
        let dir = tempfile::tempdir().unwrap();
        // A tiny budget floors the threshold at DEFAULT_SEAL_BYTES (8 MiB): budget/4 == 0, max 8 MiB.
        let s = Storage::open(
            dir.path(),
            Compression::default(),
            WalMode::Off,
            Retention::none(),
            MemoryBudget::total(1),
        )
        .unwrap();
        // Below the threshold → no seal, no segment.
        s.ingest(SIGNAL_LOGS, b"small", vec![row(1, "a", "x")], false)
            .unwrap();
        assert!(s.seal_if_full().unwrap().is_none());
        assert!(s.segments().is_empty());
        // Cross the 8 MiB floor with ~1 MiB bodies (9 MiB > 8 MiB).
        let big = "x".repeat(1 << 20);
        for i in 0..9i64 {
            s.ingest(SIGNAL_LOGS, b"big", vec![row(2 + i, "a", &big)], false)
                .unwrap();
        }
        let seg = s
            .seal_if_full()
            .unwrap()
            .expect("buffer past threshold → sealed");
        assert_eq!(seg.rows, 10, "the small row plus the nine big ones");
        assert_eq!(s.segments().len(), 1);
        // Buffer drained by the seal → back below the threshold, so no further seal.
        assert!(s.seal_if_full().unwrap().is_none());
        assert_eq!(s.segments().len(), 1);
    }

    #[test]
    fn seal_writes_segment_and_persists_manifest() {
        let dir = tempfile::tempdir().unwrap();
        {
            let s = open(dir.path(), WalMode::Always);
            s.ingest(SIGNAL_LOGS, b"r1", vec![row(3, "a", "x")], true)
                .unwrap();
            s.ingest(SIGNAL_LOGS, b"r2", vec![row(1, "a", "y")], true)
                .unwrap();
            s.ingest(SIGNAL_LOGS, b"r3", vec![row(2, "a", "z")], true)
                .unwrap();
            let seg = s.seal().unwrap().expect("segment");
            assert_eq!(seg.rows, 3);
            assert_eq!(seg.min_time_unix_nano, 1);
            assert_eq!(seg.max_time_unix_nano, 3);
            assert!(dir.path().join(&seg.relative_path).exists());
            assert_eq!(s.buffer_snapshot().unwrap().num_rows(), 0);
            assert_eq!(s.watermark(), 3);
        }
        // Reopen: manifest survives; sealed WAL records are not replayed.
        let s2 = open(dir.path(), WalMode::Always);
        assert_eq!(s2.segments().len(), 1);
        assert_eq!(s2.segments()[0].rows, 3);
        assert_eq!(s2.watermark(), 3);
        assert!(s2.take_pending_replay().is_empty());
    }

    #[test]
    fn compaction_merges_and_survives_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let merged_path;
        {
            let s = open(dir.path(), WalMode::Always);
            // Two seals → two segments in the same (1970-01-01) day partition.
            s.ingest(
                SIGNAL_LOGS,
                b"r1",
                vec![row(1, "a", "x"), row(3, "a", "z")],
                true,
            )
            .unwrap();
            let seg1 = s.seal().unwrap().expect("segment 1");
            s.ingest(SIGNAL_LOGS, b"r2", vec![row(2, "a", "y")], true)
                .unwrap();
            let seg2 = s.seal().unwrap().expect("segment 2");
            assert_eq!(s.segments().len(), 2);

            let report = s.compact().unwrap();
            assert_eq!(report.segments_merged, 2);
            assert_eq!(report.segments_created, 1);
            assert_eq!(
                s.segments().len(),
                1,
                "two same-day segments merged into one"
            );
            assert_eq!(
                s.segments()[0].rows,
                3,
                "every row preserved through the merge"
            );

            // Sources are deleted (only after the merged manifest is durable); the merged file exists.
            merged_path = s.segments()[0].relative_path.clone();
            assert!(!dir.path().join(&seg1.relative_path).exists());
            assert!(!dir.path().join(&seg2.relative_path).exists());
            assert!(dir.path().join(&merged_path).exists());
        }
        // Reopen: the manifest lists only the merged segment — no dangling references, all rows there.
        let s2 = open(dir.path(), WalMode::Always);
        assert_eq!(s2.segments().len(), 1);
        assert_eq!(s2.segments()[0].relative_path, merged_path);
        assert_eq!(s2.segments()[0].rows, 3);
    }

    #[test]
    fn reconcile_preserves_concurrent_segments() {
        let seg = |p: &str| SegmentRef {
            relative_path: p.to_owned(),
            min_time_unix_nano: 0,
            max_time_unix_nano: 0,
            rows: 0,
        };
        // Compaction snapshot had A+B and merged them → M, so it deletes {A, B} and produces [M].
        // Meanwhile a concurrent seal appended C to the live list. Reconcile must drop A/B, keep the
        // concurrently-added C, and add the merged M — never losing C.
        let mut current = vec![seg("A"), seg("B"), seg("C")];
        let deleted: std::collections::HashSet<String> =
            ["A".to_owned(), "B".to_owned()].into_iter().collect();
        reconcile_segments(&mut current, &deleted, vec![seg("M")]);
        let paths: Vec<&str> = current.iter().map(|s| s.relative_path.as_str()).collect();
        assert_eq!(paths, vec!["C", "M"]);
    }

    #[test]
    fn failed_seal_restores_buffer() {
        let dir = tempfile::tempdir().unwrap();
        let s = open(dir.path(), WalMode::Always);
        s.ingest(
            SIGNAL_LOGS,
            b"r1",
            vec![row(1, "a", "x"), row(2, "a", "y")],
            true,
        )
        .unwrap();
        assert_eq!(s.buffer_snapshot().unwrap().num_rows(), 2);

        // Block the seal deterministically: a regular file where the `logs/` partition directory
        // must be created makes the segment write fail (create_dir_all under a file).
        std::fs::write(dir.path().join("logs"), b"blocker").unwrap();
        assert!(
            s.seal().is_err(),
            "seal must fail when the segment write cannot create its dir"
        );

        // The un-sealed rows must still be buffered — NOT dropped on the write error (the fix).
        assert_eq!(
            s.buffer_snapshot().unwrap().num_rows(),
            2,
            "un-sealed rows restored to the buffer after a failed seal"
        );

        // Unblock and retry: the same rows now seal into a real segment, none lost.
        std::fs::remove_file(dir.path().join("logs")).unwrap();
        let seg = s.seal().unwrap().expect("segment on retry");
        assert_eq!(seg.rows, 2);
        assert_eq!(s.buffer_snapshot().unwrap().num_rows(), 0);
    }

    #[cfg(feature = "search")]
    #[test]
    fn seal_builds_tantivy_sidecar() {
        let dir = tempfile::tempdir().unwrap();
        let s = open(dir.path(), WalMode::Always);
        s.ingest(SIGNAL_LOGS, b"r1", vec![row(1, "a", "request ok")], true)
            .unwrap();
        s.ingest(
            SIGNAL_LOGS,
            b"r2",
            vec![row(2, "a", "connection error")],
            true,
        )
        .unwrap();
        let seg = s.seal().unwrap().expect("segment");
        let tidx = dir.path().join(&seg.relative_path).with_extension("tidx");
        assert!(
            tidx.is_dir(),
            "expected a .tidx sidecar next to the segment"
        );
        // Rows are sorted by time at seal, so ordinal 1 is the "connection error" row.
        let hits = imbh_index::search_body(&tidx, &["error".to_owned()]).unwrap();
        assert_eq!(hits, vec![1]);
    }

    #[test]
    fn wal_replays_unsealed_records() {
        let dir = tempfile::tempdir().unwrap();
        {
            let s = open(dir.path(), WalMode::Always);
            let (lsn, durable) = s
                .ingest(SIGNAL_LOGS, b"raw-1", vec![row(1, "a", "x")], true)
                .unwrap();
            assert_eq!(lsn.get(), 1);
            assert!(durable);
            assert_eq!(s.durable_through(), Lsn::new(1));
            // Drop without sealing — simulate a crash.
        }
        let s2 = open(dir.path(), WalMode::Always);
        let pending = s2.take_pending_replay();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].lsn, 1);
        assert_eq!(pending[0].payload, b"raw-1");
        assert_eq!(s2.segments().len(), 0);
    }

    #[test]
    fn watermark_prevents_double_replay() {
        let dir = tempfile::tempdir().unwrap();
        {
            let s = open(dir.path(), WalMode::Always);
            s.ingest(SIGNAL_LOGS, b"r1", vec![row(1, "a", "x")], true)
                .unwrap(); // lsn 1
            s.seal().unwrap(); // watermark = 1
            s.ingest(SIGNAL_LOGS, b"r2", vec![row(2, "a", "y")], true)
                .unwrap(); // lsn 2, unsealed
        }
        let s2 = open(dir.path(), WalMode::Always);
        let pending = s2.take_pending_replay();
        assert_eq!(pending.len(), 1); // only the unsealed record
        assert_eq!(pending[0].lsn, 2);
        assert_eq!(s2.watermark(), 1);
        assert_eq!(s2.segments().len(), 1);
    }

    #[test]
    fn open_cleans_orphan_segments() {
        let dir = tempfile::tempdir().unwrap();
        // Seal a real segment (referenced by the manifest).
        let real_rel = {
            let s = open(dir.path(), WalMode::Always);
            s.ingest(SIGNAL_LOGS, b"r1", vec![row(1, "a", "x")], true)
                .unwrap();
            s.seal().unwrap().expect("segment").relative_path
        };
        let real_path = dir.path().join(&real_rel);
        assert!(real_path.exists());

        // Simulate crash debris: an orphan parquet + sidecar not in the manifest, and stray temps.
        let orphan_day = dir.path().join("logs").join("1999-01-01");
        std::fs::create_dir_all(&orphan_day).unwrap();
        let orphan_parquet = orphan_day.join("00000000000000000000-999999.parquet");
        std::fs::write(&orphan_parquet, b"not a real parquet").unwrap();
        let orphan_tidx = orphan_day.join("00000000000000000000-999999.tidx");
        std::fs::create_dir_all(&orphan_tidx).unwrap();
        let stray_tmp = dir.path().join("logs").join("interrupted.tmp");
        std::fs::write(&stray_tmp, b"tmp").unwrap();

        // Reopen triggers cleanup.
        let s2 = open(dir.path(), WalMode::Always);
        assert!(!orphan_parquet.exists(), "orphan parquet removed");
        assert!(!orphan_tidx.exists(), "orphan sidecar removed");
        assert!(!stray_tmp.exists(), "stray temp removed");
        assert!(real_path.exists(), "referenced segment survives");
        assert_eq!(s2.segments().len(), 1);
    }

    #[test]
    fn seal_truncates_wal() {
        let dir = tempfile::tempdir().unwrap();
        {
            let s = open(dir.path(), WalMode::Always);
            for i in 1..=5 {
                s.ingest(SIGNAL_LOGS, b"payload-bytes", vec![row(i, "a", "x")], true)
                    .unwrap();
            }
            let grown = s.wal_bytes();
            assert!(grown > 0, "WAL should have frames before seal");

            // watermark = 5; every pre-seal frame sits in a now-closed segment (max-LSN 5 <= 5), so
            // reclaim deletes those whole segments and leaves a fresh, empty current segment.
            s.seal().unwrap();
            let after = s.wal_bytes();
            assert_eq!(
                after, 0,
                "sealed segments reclaimed by whole-file deletion (WAL emptied)"
            );

            // A post-seal ingest (lsn 6 > watermark) survives the next round-trip.
            s.ingest(SIGNAL_LOGS, b"post-seal", vec![row(6, "a", "y")], true)
                .unwrap();
            assert!(s.wal_bytes() > 0);
        }
        // Reopen: the sealed rows come from the segment, the unsealed one replays from the WAL.
        let s2 = open(dir.path(), WalMode::Always);
        let pending = s2.take_pending_replay();
        assert_eq!(pending.len(), 1, "only the post-seal frame replays");
        assert_eq!(pending[0].lsn, 6);
        assert_eq!(s2.watermark(), 5);
        assert_eq!(s2.segments().len(), 1);
    }

    #[test]
    fn torn_wal_tail_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        {
            let s = open(dir.path(), WalMode::Always);
            s.ingest(SIGNAL_LOGS, b"good", vec![row(1, "a", "x")], true)
                .unwrap();
        }
        // Append garbage (a torn frame) to the current WAL segment's tail.
        let seg_path = wal::current_segment_path(dir.path());
        let mut bytes = std::fs::read(&seg_path).unwrap();
        bytes.extend_from_slice(&[0xff; 7]); // shorter than a header → torn
        std::fs::write(&seg_path, &bytes).unwrap();

        let s2 = open(dir.path(), WalMode::Always);
        let pending = s2.take_pending_replay();
        assert_eq!(pending.len(), 1); // the one good frame survives; garbage ignored
        assert_eq!(pending[0].payload, b"good");
    }

    #[test]
    fn civil_from_days_epoch() {
        assert_eq!(utc_date_string(0), "1970-01-01");
        // 2026-07-18T00:00:00Z = 20652 days since epoch.
        assert_eq!(utc_date_string(20652 * 86_400_000_000_000), "2026-07-18");
    }
}

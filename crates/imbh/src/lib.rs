//! imbh — the embeddable observability database facade (ARCHITECTURE.md §10).
//!
//! `Db` is the root handle: it wires OTLP ingest (`imbh-otlp`) into the storage engine
//! (`imbh-storage`) and answers SQL through the query layer (`imbh-query`). It is a concrete
//! `Send + Sync` struct (not `Clone`); `open()`/`open_read_only()` hand back an `Arc<Db>` and you
//! share that `Arc` across threads/tasks.
//!
//! Current surface: builder/open (on-disk or in-memory), WAL-backed OTLP **logs, traces, and
//! metrics** ingest with idempotent replay, `flush`/`maintain` (seal + retention), the typed
//! `logs()` (query/volume), `traces()` (get/search), `metrics()` (catalog/range/instant), and
//! `attrs()` (discovery) APIs, `sql(...).collect()` over the `logs`/`spans`/`metrics_gauge`/
//! `metrics_sum`/`metrics_histogram` tables (buffer ∪ segments) with the
//! `matches`/`json_get_str`/`hex`/`histogram_quantile` UDFs and the cost-gated Tantivy
//! `RowSelection` pushdown, `stats`/`snapshot`/`compact`/`segment_files`, the
//! `blocking()` facade for sync hosts, span RED metrics (`traces().span_metrics`),
//! `durable_through`, the opt-in background maintenance scheduler (`DbBuilder::maintenance`),
//! Arrow-IPC `export`, and `close`. Still to come: exponential histograms + summaries,
//! `rate_delta`, `logs().tail`, and the typed error leaf sub-enums. The reference `imbhd` HTTP
//! server lives in the `imbh-server` crate.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};

#[cfg(feature = "query")]
use imbh_query::{SegmentInput, TableInput};
use imbh_storage::SCALAR_METRIC_TABLES;
use imbh_storage::Storage;
#[cfg(feature = "ingest")]
use imbh_storage::{SIGNAL_LOGS, SIGNAL_METRICS, SIGNAL_TRACES};

#[cfg(feature = "query")]
mod attrs;
#[cfg(feature = "tracing-console")]
pub mod console;
#[cfg(feature = "ingest")]
mod dedup;
#[cfg(feature = "ingest")]
mod ingest_queue;
#[cfg(feature = "query")]
mod logs;
#[cfg(feature = "query")]
mod metrics;
#[cfg(feature = "proto")]
pub mod proto;
#[cfg(feature = "proto")]
mod proto_impl;
#[cfg(feature = "query")]
mod sql;
#[cfg(feature = "query")]
mod traces;

#[cfg(feature = "query")]
pub use attrs::AttrsApi;
#[cfg(feature = "query")]
pub use logs::{
    LogEntry, LogPage, LogQuery, LogStringField, LogsApi, PageCursor, QueryStats, StringPredicate,
    VolumeBucket,
};
#[cfg(feature = "query")]
pub use metrics::{
    Aggregation, Exemplar, ExpHistogramQuery, HistogramQuery, InstantSample, Matrix, MetricMeta,
    MetricPoint, MetricPointKind, MetricPointValue, MetricPointsQuery, MetricQuery, MetricSeries,
    MetricsApi, Sample, Vector,
};
#[cfg(feature = "query")]
pub use traces::{
    Span, SpanMetricPoint, SpanMetricSeries, SpanMetrics, SpanMetricsQuery, Trace, TraceQuery,
    TraceSummary, TracesApi,
};

/// Arrow/Parquet are re-exported through DataFusion so hosts can't create a version-skewed
/// Arrow (ARCHITECTURE.md §9.1/§10.1).
pub use ::{arrow, parquet};

/// The lazy result stream type returned by [`Query::stream`], re-exported so a host/binding can name
/// it without a direct DataFusion dependency (and gets the exact engine-matched type).
#[cfg(feature = "query")]
pub use datafusion::physical_plan::SendableRecordBatchStream;

/// Read-side scan statistics for a streamed query and the handle that exposes them after the stream
/// is drained (prescription I-5). Returned by [`Query::stream_with_stats`]; re-exported so a
/// host/binding can name them without a direct `imbh-query` dependency. See [`StreamStatsHandle::get`].
#[cfg(feature = "query")]
pub use imbh_query::{ScanStats, StreamStatsHandle};

/// Arrow C Data Interface types (`cdata` feature), re-exported so an FFI binding names them off the
/// facade rather than reaching through `arrow::ffi` — and, critically, gets the *same*
/// arrow instance the query surface allocates batches with (a separately-versioned arrow would make
/// these structs ABI-incompatible; ARCHITECTURE.md §9.1). The `cdata` feature turns on `arrow/ffi`.
#[cfg(feature = "cdata")]
pub use arrow::ffi::{FFI_ArrowArray, FFI_ArrowSchema};
#[cfg(feature = "cdata")]
pub use arrow::ffi_stream::FFI_ArrowArrayStream;
pub use imbh_core::{
    Access, AnyValue, Attributes, Compression, Direction, Duplicates, DurationNs, Error,
    FlushPolicy, FlushSize, Ingest, LogRow, Lsn, Maintenance, MemoryBudget, Overflow, Promote,
    Refresh, Result, Retention, SegmentRef, SeverityNumber, SpanId, Table, TimeRange, Timestamp,
    TraceId, WalMode, parse_bytes, parse_duration, parse_json,
};

pub use imbh_storage::{CompactionReport, FlushGauges, SnapshotInfo, TableStats};
#[cfg(feature = "ingest")]
use ingest_queue::{IngestChannel, IngestJob};

#[cfg(feature = "query")]
use arrow::record_batch::RecordBatch;
#[cfg(feature = "query")]
use datafusion::scalar::ScalarValue;

/// Record the ingest outcome (`accepted`/`rejected`/`lsn`/`durable`) onto the current `tracing`
/// span. A no-op (compiled away) without the `tracing` feature — the shared tail of the three
/// `ingest_*_inner` spans so the field-recording lives in one place. Logs and traces have no
/// duplicate policy and always pass `rejected = 0`, which keeps the signature uniform.
#[inline]
#[cfg(feature = "ingest")]
#[cfg_attr(not(feature = "tracing"), allow(unused_variables))]
fn record_ingest(accepted: u64, rejected: u64, lsn: Lsn, durable: bool) {
    #[cfg(feature = "tracing")]
    {
        let span = tracing::Span::current();
        span.record("accepted", accepted);
        span.record("rejected", rejected);
        span.record("lsn", lsn.get());
        span.record("durable", durable);
    }
}

/// The imbh database handle — a concrete, non-`Clone` struct. Share it across threads/tasks by
/// wrapping in an [`Arc`]: `Db::builder(..).open()` (and `open_read_only`) hand back an `Arc<Db>`, and
/// the typed-query namespaces (`logs()`/`traces()`/…) and `sql()` take `self: &Arc<Self>` so they can
/// keep an owned `'static` share. (Consumers already hold the DB inside an `Arc`, so the handle owns
/// no second refcount of its own.)
pub struct Db {
    storage: Storage,
    mem_budget: MemoryBudget,
    /// How this handle opened the directory. `ReadOnly` handles reject every write and answer
    /// queries from a fresh on-disk snapshot per call (ARCHITECTURE.md §5).
    access: Access,
    closed: AtomicBool,
    /// Lazily-created current-thread runtime backing the blocking facade (§10.12).
    blocking_rt: OnceLock<Arc<tokio::runtime::Runtime>>,
    /// Handle for the opt-in background maintenance worker (an owned OS thread for
    /// `Maintenance::Background`, or a tokio task for `Maintenance::Runtime`), so `close()` can wait
    /// for an in-flight scheduled seal to finish before returning (a clean shutdown). `None` when no
    /// background maintenance worker was spawned.
    maintenance_handle: Mutex<Option<MaintHandle>>,
    /// The opt-in async-ingest queue (`Ingest::Async`). `Some` routes every ingest through the
    /// background worker; `None` (the default) keeps ingest fully inline on the caller's thread
    /// (ARCHITECTURE.md §5/§10.5).
    #[cfg(feature = "ingest")]
    ingest: Option<Arc<IngestChannel>>,
    /// Join handle for the async-ingest worker task, so `close()` can drain the queue and await the
    /// worker before the final seal. `None` unless `Ingest::Async` was configured.
    #[cfg(feature = "ingest")]
    ingest_handle: Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// The metric duplicate-timestamp policy (issue #27). Kept outside the `ingest` cfg because the
    /// read side consults it too — a query-only build still has to know whether to collapse a
    /// duplicated instant or fail the query.
    duplicates: Duplicates,
    /// The bounded guard backing [`Duplicates::Reject`]. Inert and unallocated under every other
    /// policy. Read-only handles always get an inert guard: they never ingest, and their per-query
    /// WAL-tail materialization must not accumulate state across queries.
    #[cfg(feature = "ingest")]
    dedup: dedup::DedupGuard,
    /// Read-only snapshot-refresh policy (ARCHITECTURE.md §5). `OnQuery` (default) rebuilds every
    /// query; `Ttl`/`Manual` reuse [`Self::reader_cache`]. Irrelevant for read-write handles.
    #[cfg(feature = "query")]
    refresh: Refresh,
    /// Read-only snapshot cache: the incremental WAL cursor plus the last-built reader tables and when
    /// they were built. Lets a `Ttl`/`Manual` reader reuse a snapshot across queries, and lets every
    /// reader (including `OnQuery`) scan only newly appended WAL bytes per rebuild. Untouched by a
    /// read-write handle (it queries live buffers under the storage lock instead).
    #[cfg(feature = "query")]
    reader_cache: Mutex<ReaderCache>,
}

/// The read-only snapshot cache backing [`Db::reader_cache`] (ARCHITECTURE.md §5).
#[cfg(feature = "query")]
#[derive(Default)]
struct ReaderCache {
    /// Incremental WAL-tail cursor, reused across rebuilds so each scans only new bytes.
    cursor: imbh_storage::WalTailCursor,
    /// The last built reader tables and the instant they were built. `None` until the first build;
    /// consulted by [`Refresh::Ttl`]/[`Refresh::Manual`] to decide whether to reuse or rebuild.
    built: Option<(Vec<TableInput>, std::time::Instant)>,
}

/// The background-maintenance worker handle: either an owned OS thread (`Maintenance::Background`)
/// or a task scheduled on a host runtime (`Maintenance::Runtime`). `close()` joins the thread or
/// awaits the task so shutdown is synchronous with any in-flight seal.
enum MaintHandle {
    Thread(std::thread::JoinHandle<()>),
    Task(tokio::task::JoinHandle<()>),
}

impl Db {
    /// Open on a directory (created if absent). Startup I/O (manifest load) is allowed to
    /// block, so this builder is entered synchronously.
    pub fn builder(path: impl AsRef<Path>) -> DbBuilder {
        DbBuilder {
            path: Some(path.as_ref().to_path_buf()),
            in_memory: false,
            memory_budget: MemoryBudget::default(),
            compression: Compression::default(),
            wal: WalMode::default(),
            retention: Retention::default(),
            maintenance: Maintenance::default(),
            flush: None,
            ingest: Ingest::default(),
            access: Access::ReadWrite,
            promote: Promote::default(),
            allow_stale_reads: false,
            refresh: Refresh::default(),
            duplicates: Duplicates::default(),
        }
    }

    /// Open an existing DB directory **read-only** (ARCHITECTURE.md §5): a shortcut for
    /// `Db::builder(path).access(Access::ReadOnly).open()`. Takes no writer lock, so it coexists with
    /// the single writer process and with other readers; queries see the writer's segments unioned
    /// with its live WAL tail (near-real-time). Every write returns [`Error::read_only`].
    ///
    /// Rejected with [`Error::reader_wal_disabled`] if the writer's WAL is off (the reader could then
    /// get only seal-interval freshness, not near-real-time); use
    /// `Db::builder(path).access(Access::ReadOnly).allow_stale_reads().open()` to accept that.
    pub fn open_read_only(path: impl AsRef<Path>) -> Result<Arc<Db>> {
        Db::builder(path).access(Access::ReadOnly).open()
    }

    /// Ephemeral DB: no directory, no segments on disk; everything lives in the buffer.
    pub fn in_memory() -> DbBuilder {
        DbBuilder {
            path: None,
            in_memory: true,
            memory_budget: MemoryBudget::default(),
            compression: Compression::default(),
            wal: WalMode::Off,
            retention: Retention::default(),
            maintenance: Maintenance::default(),
            flush: None,
            ingest: Ingest::default(),
            access: Access::ReadWrite,
            promote: Promote::default(),
            allow_stale_reads: false,
            refresh: Refresh::default(),
            duplicates: Duplicates::default(),
        }
    }

    /// Ingest a protobuf OTLP/logs export request body. The WAL frame is written before the
    /// buffer append; under `WalMode::Always` the awaiting path also fsyncs, so the receipt is
    /// `durable`. Confirm otherwise via [`Db::durable_through`] or [`Db::flush`] (ARCHITECTURE.md §10.5).
    ///
    /// Under `Ingest::Async` (opt-in) the protobuf is still decoded here (so `accepted` and a decode
    /// error stay synchronous), then the WAL + buffer write is handed to the background worker: the
    /// receipt is `queued` (no `lsn`/`durable`) and this call awaits only a free queue slot per the
    /// configured [`Overflow`] policy.
    #[cfg(feature = "ingest")]
    pub async fn ingest_otlp_logs(&self, body: &[u8]) -> Result<IngestReceipt> {
        if let Some(ch) = &self.ingest {
            self.ensure_writable()?;
            let rows = imbh_otlp::decode_logs_to_rows(body)?;
            let accepted = rows.len() as u64;
            ch.send(IngestJob::Logs {
                raw: body.to_vec(),
                rows,
            })
            .await?;
            return Ok(IngestReceipt::queued(accepted, 0));
        }
        self.ingest_logs_inner(body, true)
    }

    /// Fail-fast ingest that never blocks and never fsyncs inline, so its receipt is never
    /// `durable` (ARCHITECTURE.md §10.5). Under `Ingest::Async` it enqueues without ever awaiting (the
    /// `Block` policy degrades to fail-fast on this path).
    #[cfg(feature = "ingest")]
    pub fn try_ingest_otlp_logs(&self, body: &[u8]) -> Result<IngestReceipt> {
        self.ingest_logs_inner(body, false)
    }

    #[cfg_attr(
        feature = "tracing",
        tracing::instrument(
            level = "debug",
            name = "ingest.logs",
            skip_all,
            fields(bytes = body.len(), sync_now, accepted = tracing::field::Empty, rejected = tracing::field::Empty, lsn = tracing::field::Empty, durable = tracing::field::Empty)
        )
    )]
    #[cfg(feature = "ingest")]
    fn ingest_logs_inner(&self, body: &[u8], sync_now: bool) -> Result<IngestReceipt> {
        self.ensure_writable()?;
        let rows = imbh_otlp::decode_logs_to_rows(body)?;
        let accepted = rows.len() as u64;
        if let Some(ch) = &self.ingest {
            ch.try_send(IngestJob::Logs {
                raw: body.to_vec(),
                rows,
            })?;
            return Ok(IngestReceipt::queued(accepted, 0));
        }
        let (lsn, durable) = self.storage.ingest(SIGNAL_LOGS, body, rows, sync_now)?;
        record_ingest(accepted, 0, lsn, durable);
        Ok(IngestReceipt::synced(accepted, 0, lsn, durable))
    }

    /// Ingest a protobuf OTLP/traces export request body (ARCHITECTURE.md §10.5). Under `Ingest::Async`
    /// the WAL + buffer write is offloaded to the background worker (queued receipt).
    #[cfg(feature = "ingest")]
    pub async fn ingest_otlp_traces(&self, body: &[u8]) -> Result<IngestReceipt> {
        if let Some(ch) = &self.ingest {
            self.ensure_writable()?;
            let rows = imbh_otlp::decode_traces_to_rows(body)?;
            let accepted = rows.len() as u64;
            ch.send(IngestJob::Traces {
                raw: body.to_vec(),
                rows,
            })
            .await?;
            return Ok(IngestReceipt::queued(accepted, 0));
        }
        self.ingest_traces_inner(body, true)
    }

    /// Fail-fast traces ingest (never fsyncs inline).
    #[cfg(feature = "ingest")]
    pub fn try_ingest_otlp_traces(&self, body: &[u8]) -> Result<IngestReceipt> {
        self.ingest_traces_inner(body, false)
    }

    #[cfg_attr(
        feature = "tracing",
        tracing::instrument(
            level = "debug",
            name = "ingest.traces",
            skip_all,
            fields(bytes = body.len(), sync_now, accepted = tracing::field::Empty, rejected = tracing::field::Empty, lsn = tracing::field::Empty, durable = tracing::field::Empty)
        )
    )]
    #[cfg(feature = "ingest")]
    fn ingest_traces_inner(&self, body: &[u8], sync_now: bool) -> Result<IngestReceipt> {
        self.ensure_writable()?;
        let rows = imbh_otlp::decode_traces_to_rows(body)?;
        let accepted = rows.len() as u64;
        if let Some(ch) = &self.ingest {
            ch.try_send(IngestJob::Traces {
                raw: body.to_vec(),
                rows,
            })?;
            return Ok(IngestReceipt::queued(accepted, 0));
        }
        let (lsn, durable) = self.storage.ingest_traces(body, rows, sync_now)?;
        record_ingest(accepted, 0, lsn, durable);
        Ok(IngestReceipt::synced(accepted, 0, lsn, durable))
    }

    /// Ingest a protobuf OTLP/metrics export request body (gauge + sum points; ARCHITECTURE.md §10.5).
    /// Under `Ingest::Async` the WAL + buffer write is offloaded to the background worker (queued
    /// receipt).
    #[cfg(feature = "ingest")]
    pub async fn ingest_otlp_metrics(&self, body: &[u8]) -> Result<IngestReceipt> {
        if let Some(ch) = &self.ingest {
            self.ensure_writable()?;
            let (job, accepted, rejected) = decode_metrics_job(body, &self.dedup)?;
            ch.send(job).await?;
            return Ok(IngestReceipt::queued(accepted, rejected));
        }
        self.ingest_metrics_inner(body, true)
    }

    /// Fail-fast metrics ingest (never fsyncs inline).
    #[cfg(feature = "ingest")]
    pub fn try_ingest_otlp_metrics(&self, body: &[u8]) -> Result<IngestReceipt> {
        self.ingest_metrics_inner(body, false)
    }

    #[cfg_attr(
        feature = "tracing",
        tracing::instrument(
            level = "debug",
            name = "ingest.metrics",
            skip_all,
            fields(bytes = body.len(), sync_now, accepted = tracing::field::Empty, rejected = tracing::field::Empty, lsn = tracing::field::Empty, durable = tracing::field::Empty)
        )
    )]
    #[cfg(feature = "ingest")]
    fn ingest_metrics_inner(&self, body: &[u8], sync_now: bool) -> Result<IngestReceipt> {
        self.ensure_writable()?;
        if let Some(ch) = &self.ingest {
            let (job, accepted, rejected) = decode_metrics_job(body, &self.dedup)?;
            ch.try_send(job)?;
            return Ok(IngestReceipt::queued(accepted, rejected));
        }
        let decoded = decode_metrics(body, &self.dedup)?;
        let DecodedMetrics {
            rows,
            histograms,
            exp_histograms,
            summaries,
            accepted,
            rejected,
        } = decoded;
        // `body` stays the raw, unfiltered bytes: the WAL frame is the wire bytes we received, and
        // replay re-derives the rows through the same guard (see `decode_metrics`).
        let (lsn, durable) = self.storage.ingest_metrics(
            body,
            rows,
            histograms,
            exp_histograms,
            summaries,
            sync_now,
        )?;
        record_ingest(accepted, rejected, lsn, durable);
        Ok(IngestReceipt::synced(accepted, rejected, lsn, durable))
    }

    /// The configured metric duplicate-timestamp policy (issue #27), as the read side consults it to
    /// decide whether a duplicated instant collapses or fails the query.
    pub fn duplicates(&self) -> Duplicates {
        self.duplicates
    }

    /// Highest LSN that is durable (fsync'd WAL or captured in a sealed segment), or `None` when
    /// nothing is durable yet.
    pub async fn durable_through(&self) -> Option<Lsn> {
        self.storage.durable_through()
    }

    /// The typed Logs query namespace (ARCHITECTURE.md §10.6). Holds an owned (`Arc`-backed) [`Db`]
    /// handle, so the returned value is `'static` and can be stored or moved into a task.
    #[cfg(feature = "query")]
    pub fn logs(self: &Arc<Self>) -> LogsApi {
        LogsApi {
            db: Arc::clone(self),
        }
    }

    /// The typed Traces query namespace (ARCHITECTURE.md §10.7). Holds an owned (`Arc`-backed) [`Db`]
    /// handle, so the returned value is `'static` and can be stored or moved into a task.
    #[cfg(feature = "query")]
    pub fn traces(self: &Arc<Self>) -> TracesApi {
        TracesApi {
            db: Arc::clone(self),
        }
    }

    /// The typed Metrics query namespace (ARCHITECTURE.md §10.8). Holds an owned (`Arc`-backed) [`Db`]
    /// handle, so the returned value is `'static` and can be stored or moved into a task.
    #[cfg(feature = "query")]
    pub fn metrics(self: &Arc<Self>) -> MetricsApi {
        MetricsApi {
            db: Arc::clone(self),
        }
    }

    /// The attribute-discovery namespace (ARCHITECTURE.md §10.9). Holds an owned (`Arc`-backed) [`Db`]
    /// handle, so the returned value is `'static` and can be stored or moved into a task.
    #[cfg(feature = "query")]
    pub fn attrs(self: &Arc<Self>) -> AttrsApi {
        AttrsApi {
            db: Arc::clone(self),
        }
    }

    /// A lazy SQL query over the `logs` table (buffer ∪ segments). `.collect().await` runs it.
    #[cfg(feature = "query")]
    pub fn sql(self: &Arc<Self>, sql: &str) -> Query {
        Query {
            db: Arc::clone(self),
            sql: sql.to_owned(),
            params: Vec::new(),
        }
    }

    /// A lazy SQL query carrying DataFusion bind parameters for `$1..$N` placeholders. The typed
    /// query builders (logs/traces/metrics/attrs) use this so user values are bound, never
    /// interpolated into the SQL text (ARCHITECTURE.md §10). Not public: the raw [`Db::sql`] API
    /// takes a complete statement.
    #[cfg(feature = "query")]
    pub(crate) fn sql_with_params(
        self: &Arc<Self>,
        sql: String,
        params: Vec<ScalarValue>,
    ) -> Query {
        Query {
            db: Arc::clone(self),
            sql,
            params,
        }
    }

    /// Force-seal the buffer into an immutable segment (ARCHITECTURE.md §10.2). No-op for in-memory.
    pub async fn flush(&self) -> Result<()> {
        self.ensure_writable()?;
        self.storage.seal()?;
        Ok(())
    }

    /// Run maintenance: seal the buffer, then apply retention (ARCHITECTURE.md §5/§10.2). This is the
    /// inline path behind the "no background threads unless opted in" guarantee; a host that
    /// wants it automatic wires a scheduler to call this.
    pub async fn maintain(&self) -> Result<MaintenanceReport> {
        self.ensure_writable()?;
        let sealed = self.storage.seal()?.is_some();
        let retention = self.storage.retain()?;
        Ok(MaintenanceReport {
            sealed,
            segments_dropped: retention.segments_dropped,
            bytes_freed: retention.bytes_freed,
        })
    }

    /// The current sealed-segment set (manifest snapshot).
    pub fn segments(&self) -> Vec<SegmentRef> {
        self.storage.segments()
    }

    /// Per-table statistics plus engine-wide gauges (buffer bytes, WAL bytes, durable LSN) — ARCHITECTURE.md
    /// §10.11.
    pub async fn stats(&self) -> Result<DbStats> {
        self.ensure_open()?;
        let storage = &self.storage;
        // A read-only handle keeps no live buffers/segments — its query view is derived per call from
        // the on-disk manifest + WAL tail (`reader_tables`). `storage.stats()` would therefore report
        // all-zero row counts on a reader; build the per-table stats from the same disk snapshot
        // instead so the overview reflects the populated database.
        let tables = if self.access == Access::ReadOnly {
            reader_stats(self)?
        } else {
            storage.stats()
        };
        // The async-ingest queue gauges are only meaningful in a build with the ingest engine; a
        // query-only consumer has no queue and reports zero.
        #[cfg(feature = "ingest")]
        let (ingest_queue_depth, ingest_dropped, ingest_errors, ingest_rejected) = (
            self.ingest.as_ref().map_or(0, |c| c.depth()),
            self.ingest.as_ref().map_or(0, |c| c.dropped()),
            self.ingest.as_ref().map_or(0, |c| c.errors()),
            self.dedup.rejected(),
        );
        #[cfg(not(feature = "ingest"))]
        let (ingest_queue_depth, ingest_dropped, ingest_errors, ingest_rejected) =
            (0usize, 0u64, 0u64, 0u64);
        Ok(DbStats {
            tables,
            buffer_bytes: storage.buffer_bytes(),
            wal_bytes: storage.wal_bytes(),
            durable_lsn: storage.durable_through(),
            ingest_queue_depth,
            ingest_dropped,
            ingest_errors,
            ingest_rejected,
        })
    }

    /// Snapshot the DB into `dir` — a manifest copy plus hard-linked segment files (ARCHITECTURE.md
    /// §10.11). Errors for an in-memory DB.
    pub async fn snapshot(&self, dir: impl AsRef<Path>) -> Result<SnapshotInfo> {
        // Snapshot copies the writer's manifest + segment set; a read-only handle holds neither
        // (its view is per-query), so it is a writer-only operation for now (ARCHITECTURE.md §5).
        self.ensure_writable()?;
        self.storage.snapshot(dir.as_ref())
    }

    /// Compact small segments within each UTC-day partition into one, rebuilding the search index
    /// (ARCHITECTURE.md §7/§10.11). Optional maintenance — a DB that never compacts is still correct.
    pub async fn compact(&self) -> Result<CompactionReport> {
        self.ensure_writable()?;
        self.storage.compact()
    }

    /// Export a table's rows over `range` as **Arrow-IPC stream** bytes (ARCHITECTURE.md §10.11). The
    /// result is a self-describing Arrow IPC stream (schema followed by record batches) that
    /// DuckDB, polars, or pandas/`pyarrow` load directly — the copy-out companion to
    /// [`segment_files`](Self::segment_files) (which hands back live Parquet paths for zero-copy).
    ///
    /// One table per call: an IPC stream carries a single schema, and imbh's tables
    /// (`logs`/`spans`/`metrics_*`) have different schemas. Rows come from buffer ∪ segments and
    /// arrive ordered by `time`. Today this collects then serializes; the bounded-memory
    /// `RecordBatchStream` variant over huge ranges is a follow-up. Exporting a table that is not
    /// yet materialized (e.g. the histogram tables) surfaces as a `Query` error.
    #[cfg(feature = "query")]
    pub async fn export(self: &Arc<Self>, table: Table, range: TimeRange) -> Result<Vec<u8>> {
        self.ensure_open()?;
        let sql = format!(
            "SELECT * FROM {} WHERE CAST(\"time\" AS BIGINT) >= {} \
             AND CAST(\"time\" AS BIGINT) < {} ORDER BY \"time\"",
            table.as_str(),
            range.start.0,
            range.end.0,
        );
        // `collect_with_schema` hands back the exact result schema on a non-empty result and the
        // query's declared output schema when empty, so the schema-only stream is always valid.
        let (schema, batches) = self.sql(&sql).collect_with_schema().await?;
        encode_ipc_stream(&schema, &batches)
    }

    /// Absolute Parquet paths of a table's sealed segments — a zero-copy handoff for external
    /// tools (e.g. DuckDB `read_parquet`), ARCHITECTURE.md §10.11.
    pub fn segment_files(&self, table: Table) -> Vec<PathBuf> {
        match table {
            Table::Logs => self.storage.segment_paths(),
            Table::Spans => self.storage.segment_paths_spans(),
            other => self.storage.segment_paths_metric(other),
        }
    }

    /// A blocking mirror of the async API for sync hosts (ARCHITECTURE.md §10.2/§10.12). Methods drop the
    /// `.await` and run on an owned, lazily-created current-thread runtime — so there is no doubled
    /// method set. Do not call from inside an async runtime.
    pub fn blocking(self: &Arc<Self>) -> BlockingDb {
        let rt = self
            .blocking_rt
            .get_or_init(|| {
                Arc::new(
                    tokio::runtime::Builder::new_current_thread()
                        .build()
                        .expect("build blocking current-thread runtime"),
                )
            })
            .clone();
        BlockingDb {
            db: Arc::clone(self),
            rt,
        }
    }

    /// Flush, seal, and mark every clone closed. Idempotent; takes `&self` because other
    /// `Arc` handles would otherwise keep the DB alive (ARCHITECTURE.md §10.2).
    pub async fn close(&self) -> Result<()> {
        if self.closed.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        // Drain the async-ingest worker first (if any): signal the channel closed and await the task
        // so every queued job lands in the buffer before the final seal — a clean shutdown loses no
        // enqueued data. (Take the handle out of the lock, then await outside it.)
        #[cfg(feature = "ingest")]
        {
            if let Some(chan) = &self.ingest {
                chan.close();
            }
            let ingest_task = self.ingest_handle.lock().unwrap().take();
            if let Some(task) = ingest_task {
                // `.await` yields, so the worker gets polled, drains the queue, observes `closed`, and
                // exits even on a single-threaded host runtime — no self-deadlock.
                let _ = task.await;
            }
        }
        // Wait for the background maintenance worker (if any) to observe `closed` and exit, so a
        // scheduled seal in flight finishes before we seal a final time — no seal runs concurrently
        // with, or after, this. (Take the handle out of the lock first, then join/await outside it.)
        let handle = self.maintenance_handle.lock().unwrap().take();
        match handle {
            Some(MaintHandle::Thread(h)) => {
                let _ = h.join();
            }
            Some(MaintHandle::Task(t)) => {
                // `.await` yields, so even on a single-threaded host runtime the maintenance task
                // gets polled, notices `closed`, and completes — no self-deadlock.
                let _ = t.await;
            }
            None => {}
        }
        self.storage.seal()?;
        Ok(())
    }

    fn ensure_open(&self) -> Result<()> {
        if self.closed.load(Ordering::SeqCst) {
            Err(Error::Closed)
        } else {
            Ok(())
        }
    }

    /// Reject a write on a read-only handle (ARCHITECTURE.md §5), after the open check.
    fn ensure_writable(&self) -> Result<()> {
        self.ensure_open()?;
        if self.access == Access::ReadOnly {
            Err(Error::read_only())
        } else {
            Ok(())
        }
    }
}

/// Builder + config for opening a [`Db`] (ARCHITECTURE.md §10.2). Honors path/in-memory, the
/// memory budget, the compression codec, and the WAL/retention/maintenance/promotion settings.
pub struct DbBuilder {
    path: Option<PathBuf>,
    in_memory: bool,
    memory_budget: MemoryBudget,
    compression: Compression,
    wal: WalMode,
    retention: Retention,
    maintenance: Maintenance,
    /// When the opt-in scheduler seals the buffer (ARCHITECTURE.md §5/§7). `None` = the host never
    /// chose one, in which case open resolves it to `FlushPolicy::default().or_interval(maintenance
    /// interval)` — the historical behavior, where the maintenance interval was also the seal cadence.
    flush: Option<FlushPolicy>,
    ingest: Ingest,
    access: Access,
    /// Attribute keys promoted to typed columns (ARCHITECTURE.md §6.1). Fixed at open; must match a
    /// read-write DB's prior promotion for on-disk consistency (segments written under a different set
    /// are reconciled by the query layer's `coerce` null-fill, so a superset stays readable).
    promote: Promote,
    /// Read-only opens only: accept a WAL-off writer's seal-interval freshness instead of being
    /// rejected. Ignored for read-write / in-memory opens.
    allow_stale_reads: bool,
    /// Read-only opens only: how often the point-in-time snapshot is rebuilt (ARCHITECTURE.md §5).
    /// Default `OnQuery` (near-real-time). Ignored for read-write / in-memory opens.
    refresh: Refresh,
    /// What to do about two metric points sharing a series and a timestamp (issue #27). Default
    /// [`Duplicates::ErrorOnRead`] — accept at ingest, fail the PromQL query.
    duplicates: Duplicates,
}

impl DbBuilder {
    pub fn memory_budget(mut self, b: MemoryBudget) -> Self {
        self.memory_budget = b;
        self
    }

    /// Open for reading + writing (the default, single writer) or read-only (a coexisting reader) —
    /// ARCHITECTURE.md §5. Ignored for in-memory DBs (always writable). A `ReadWrite` open acquires
    /// the exclusive `writer.lock`; a second one fails with [`Error::lock_held`].
    pub fn access(mut self, access: Access) -> Self {
        self.access = access;
        self
    }

    /// Read-only opens only: accept a WAL-off writer's **seal-interval** freshness rather than being
    /// rejected with [`Error::reader_wal_disabled`]. Without this, `Access::ReadOnly` refuses to open
    /// a DB whose writer advertised its WAL off, because the reader would silently miss the not-yet-
    /// sealed tail (ARCHITECTURE.md §5). No effect on read-write / in-memory opens.
    pub fn allow_stale_reads(mut self) -> Self {
        self.allow_stale_reads = true;
        self
    }

    /// Read-only opens only: how often the point-in-time snapshot is rebuilt (ARCHITECTURE.md §5).
    /// The default [`Refresh::OnQuery`] rebuilds per query (near-real-time); [`Refresh::Ttl`] reuses a
    /// snapshot for a bounded window, and [`Refresh::Manual`] pins it until [`Db::refresh`]. Every
    /// rebuild is incremental (it scans only WAL bytes appended since the last). No effect on
    /// read-write / in-memory opens, which query their own live buffers.
    pub fn refresh(mut self, refresh: Refresh) -> Self {
        self.refresh = refresh;
        self
    }

    pub fn compression(mut self, c: Compression) -> Self {
        self.compression = c;
        self
    }

    /// WAL fsync policy (ARCHITECTURE.md §7/§10.2). Ignored for in-memory DBs.
    pub fn wal(mut self, mode: WalMode) -> Self {
        self.wal = mode;
        self
    }

    /// Retention policy applied by [`Db::maintain`] (ARCHITECTURE.md §7/§10.2). Ignored for in-memory DBs.
    pub fn retention(mut self, r: Retention) -> Self {
        self.retention = r;
        self
    }

    /// Maintenance policy (ARCHITECTURE.md §5/§10.2) — who runs the scheduler.
    /// `Maintenance::Background(interval)` spawns one owned thread; `interval` is the retention
    /// cadence (and, absent [`Self::flush`], the seal cadence too). Ignored for in-memory DBs.
    pub fn maintenance(mut self, m: Maintenance) -> Self {
        self.maintenance = m;
        self
    }

    /// Flush policy (ARCHITECTURE.md §5/§7/§10.2) — *when* the scheduler configured by
    /// [`Self::maintenance`] turns the buffer into a segment. Triggers OR together, so a policy can be
    /// periodic, size-based (buffered bytes or rows), WAL-size-based, idle-based, or any mix:
    ///
    /// ```no_run
    /// # use imbh::{Db, FlushPolicy, Maintenance};
    /// # use std::time::Duration;
    /// let db = Db::builder("./telemetry")
    ///     .maintenance(Maintenance::Background(Duration::from_secs(300))) // retention cadence
    ///     .flush(FlushPolicy::periodic(Duration::from_secs(5)).at_wal_bytes(64 << 20))
    ///     .open()?;
    /// # Ok::<(), imbh::Error>(())
    /// ```
    ///
    /// With `Maintenance::Manual` there is no scheduler to consult it, so the policy is inert (the host
    /// calls `flush()`/`maintain()` itself). Leaving this unset keeps the historical behavior: the
    /// buffer seals on the maintenance interval and at the memory-budget-derived byte threshold.
    /// Ignored for in-memory DBs.
    pub fn flush(mut self, policy: FlushPolicy) -> Self {
        self.flush = Some(policy);
        self
    }

    /// Ingest execution policy (ARCHITECTURE.md §5/§10.5). The default `Ingest::Sync` runs ingest inline
    /// on the caller's thread; `Ingest::Async { handle, capacity, overflow }` offloads the WAL + buffer
    /// write to one background worker task on `handle` (the protobuf decode still runs on the caller).
    /// Ignored for in-memory and read-only DBs.
    pub fn ingest(mut self, i: Ingest) -> Self {
        self.ingest = i;
        self
    }

    /// Promote attribute keys to typed columns (ARCHITECTURE.md §6.1). Each key becomes a nullable
    /// dictionary column on every signal, materialized from the record `attributes` → `resource` →
    /// `scope` JSON at ingest; the key also stays in the JSON, so `json_get_str` and existing queries
    /// are unaffected while a promoted-label filter can hit the column instead of a JSON scan. Set
    /// once at open, before any ingest. Re-opening a read-write DB with a different set stays readable
    /// (older segments are null-filled for keys they predate), but for stable pushdown keep it fixed.
    pub fn promote(mut self, promote: Promote) -> Self {
        self.promote = promote;
        self
    }

    /// What to do about two metric points sharing a series **and** a timestamp (issue #27).
    ///
    /// [`Duplicates::ErrorOnRead`] (the default) accepts everything at ingest and fails a PromQL
    /// query over an affected series. [`Duplicates::LastWins`] collapses the duplicated instant at
    /// read time instead — the escape hatch for a database that already holds duplicates, since no
    /// ingest-side policy can repair data already written. [`Duplicates::Reject`] drops the repeat at
    /// ingest and reports it in [`IngestReceipt::rejected`], so the responsible producer sees it at
    /// write time; it costs a fixed ~13 MB at the default lookback and nothing at all otherwise.
    pub fn duplicates(mut self, duplicates: Duplicates) -> Self {
        self.duplicates = duplicates;
        self
    }

    /// Open the DB (synchronous startup I/O: manifest load + WAL replay).
    pub fn open(self) -> Result<Arc<Db>> {
        // In-memory DBs are always writable; a read-only in-memory DB would have nothing to read.
        let access = if self.in_memory {
            Access::ReadWrite
        } else {
            self.access
        };
        // The guard is built before the replay loop below, which borrows it, and moved into the `Db`
        // afterwards. A read-only handle is always inert: it never ingests, and it re-materializes
        // the WAL tail into a throwaway buffer on *every* query, so a live guard there would both
        // accumulate unbounded state and start rejecting rows the writer had accepted. The *policy*
        // is still carried through unchanged — the read side honors it on a reader too.
        #[cfg(feature = "ingest")]
        let dedup = dedup::DedupGuard::new(if access == Access::ReadOnly {
            Duplicates::default()
        } else {
            self.duplicates
        });
        let storage = if self.in_memory {
            Storage::in_memory(self.compression, self.memory_budget)
                .with_promote(self.promote.clone())
        } else {
            let path = self.path.ok_or_else(Error::missing_database_path)?;
            match access {
                // A reader takes no writer lock and materializes the WAL tail per query
                // (`reader_tables`), not at open — so it coexists with the writer (ARCHITECTURE.md §5).
                Access::ReadOnly => {
                    // Guard against silent staleness: if the writer advertised its WAL off, the reader
                    // can only ever see seal-interval freshness, so reject unless the caller opted in.
                    if !self.allow_stale_reads && imbh_storage::writer_wal_disabled(&path) {
                        return Err(Error::reader_wal_disabled());
                    }
                    Storage::open_read_only(path, self.compression, self.memory_budget)?
                        .with_promote(self.promote.clone())
                }
                Access::ReadWrite => {
                    let storage = Storage::open(
                        path,
                        self.compression,
                        self.wal,
                        self.retention,
                        self.memory_budget,
                    )?
                    .with_promote(self.promote.clone());
                    // Idempotent replay: re-ingest WAL records not yet captured in a segment
                    // (`lsn > watermark`), decoding each by its signal tag (ARCHITECTURE.md §7).
                    // Decoding the raw OTLP payload needs the `ingest` decoder; a query-only build
                    // has none, so it skips replay (it reads sealed segments only).
                    #[cfg(feature = "ingest")]
                    {
                        #[cfg(feature = "tracing")]
                        let _replay_span = tracing::debug_span!("wal.replay").entered();
                        #[cfg(feature = "tracing")]
                        let mut replayed = 0usize;
                        for rec in storage.take_pending_replay() {
                            #[cfg(feature = "tracing")]
                            {
                                replayed += 1;
                            }
                            replay_record(&storage, &rec, &dedup)?;
                        }
                        #[cfg(feature = "tracing")]
                        tracing::debug!(records = replayed, "WAL replay complete");
                    }
                    storage
                }
            }
        };
        // Opt-in async ingest: build the bounded queue up front (only for a writable on-disk DB) so
        // the `Db` owns it before any ingest can race in; the worker task is spawned just after the
        // `Arc<Db>` exists (it needs a `Weak<Db>` to reach storage). In-memory / read-only DBs keep
        // ingest inline (`None`). Only compiled when the ingest engine is in the build.
        #[cfg(feature = "ingest")]
        let ingest_chan = match &self.ingest {
            Ingest::Async {
                capacity, overflow, ..
            } if !self.in_memory && access == Access::ReadWrite => {
                Some(Arc::new(IngestChannel::new(*capacity, *overflow)))
            }
            _ => None,
        };
        let db = Arc::new(Db {
            storage,
            mem_budget: self.memory_budget,
            access,
            closed: AtomicBool::new(false),
            blocking_rt: OnceLock::new(),
            maintenance_handle: Mutex::new(None),
            #[cfg(feature = "ingest")]
            ingest: ingest_chan.clone(),
            #[cfg(feature = "ingest")]
            ingest_handle: Mutex::new(None),
            duplicates: self.duplicates,
            #[cfg(feature = "ingest")]
            dedup,
            #[cfg(feature = "query")]
            refresh: self.refresh,
            #[cfg(feature = "query")]
            reader_cache: Mutex::new(ReaderCache::default()),
        });
        // Spawn the async-ingest worker onto the host runtime (`Ingest::Async` implies a tokio
        // `Handle`; never an owned OS thread). Its handle is kept so `close()` can drain the queue and
        // await the worker before the final seal.
        #[cfg(feature = "ingest")]
        if let (Some(chan), Ingest::Async { handle, .. }) = (&ingest_chan, &self.ingest) {
            let task = handle.spawn(run_ingest_worker(Arc::downgrade(&db), Arc::clone(chan)));
            *db.ingest_handle.lock().unwrap() = Some(task);
        }
        // Opt-in background maintenance: a worker that seals + retains on an interval (and seals
        // promptly once the buffer crosses its byte threshold). It holds only a Weak handle, so
        // dropping every `Db` stops it; its handle is kept so `close()` can wait for an in-flight
        // seal before returning (clean shutdown). Skipped entirely for in-memory DBs (seal is a
        // no-op there). `Background` owns an OS thread; `Runtime` schedules onto the host runtime.
        if !self.in_memory && access == Access::ReadWrite {
            let weak = Arc::downgrade(&db);
            // The flush policy decides *when* to seal; the maintenance interval decides how often
            // retention runs. A host that set no policy inherits the maintenance interval as its seal
            // cadence, which is exactly what the scheduler did before this knob existed.
            let worker = match &self.maintenance {
                Maintenance::Background(interval) => {
                    let interval = *interval;
                    let flush = resolve_flush(self.flush, interval);
                    Some(MaintHandle::Thread(std::thread::spawn(move || {
                        run_maintenance(weak, interval, flush)
                    })))
                }
                Maintenance::Runtime(handle, interval) => {
                    let interval = *interval;
                    let flush = resolve_flush(self.flush, interval);
                    Some(MaintHandle::Task(
                        handle.spawn(run_maintenance_async(weak, interval, flush)),
                    ))
                }
                Maintenance::Manual => None,
            };
            if let Some(worker) = worker {
                *db.maintenance_handle.lock().unwrap() = Some(worker);
            }
        }
        Ok(db)
    }

    /// Open for callers already inside a runtime (startup I/O is still synchronous in M0).
    pub async fn open_async(self) -> Result<Arc<Db>> {
        self.open()
    }
}

/// A lazy SQL query (ARCHITECTURE.md §10.1: SQL is lazy, typed queries eager).
#[cfg(feature = "query")]
pub struct Query {
    db: Arc<Db>,
    sql: String,
    /// DataFusion bind values for the `$1..$N` placeholders in `sql` (empty for raw [`Db::sql`]).
    params: Vec<ScalarValue>,
}

#[cfg(feature = "query")]
impl Query {
    /// Execute the query and collect all result batches.
    ///
    /// The batches are **owned, segment-independent allocations** (see [`Query::collect_with_schema`]
    /// for the full invariant): they remain valid for the lifetime of the `RecordBatch` regardless of
    /// subsequent seals, retention, or [`Db::compact`], so a host may hand them across an FFI boundary
    /// (the `cdata` feature) without a keep-alive token.
    pub async fn collect(self) -> Result<Vec<RecordBatch>> {
        Ok(self.collect_with_stats().await?.1)
    }

    /// Execute the query and collect all result batches together with the **result schema**: the
    /// exact schema the batches carry on a non-empty result, and the query's declared output schema
    /// when zero rows come back (an empty `Vec<RecordBatch>` carries no schema of its own). This is
    /// the shape an Arrow-IPC / C-Data-Interface exporter needs — it always learns the columns — so a
    /// binding building an in-memory `RecordBatchReader` need not repeat the empty-result schema dance
    /// (see [`Db::export`]).
    ///
    /// **Ownership invariant.** Arrow batches returned by the query surface are owned,
    /// segment-independent allocations: DataFusion's Parquet scan *decodes* segment bytes into
    /// freshly-allocated, `Arc`-refcounted Arrow rather than borrowing the mmap'd file, and the
    /// mutable-buffer snapshot is a copy taken under the storage lock. They therefore remain valid for
    /// the lifetime of the `RecordBatch` regardless of subsequent seals, retention, or
    /// [`Db::compact`] unlinking the segments the query read.
    pub async fn collect_with_schema(
        self,
    ) -> Result<(arrow::datatypes::SchemaRef, Vec<RecordBatch>)> {
        let (schema, batches, _scan) = self.collect_with_stats().await?;
        Ok((schema, batches))
    }

    /// Like [`Query::collect`], but also returns the result schema (valid even when empty) and the
    /// read-side scan statistics (segments read vs. bloom-pruned, rows materialized after
    /// `RowSelection` pruning, and whether the Tantivy index was consulted). The typed query APIs use
    /// this to populate `QueryStats`.
    pub(crate) async fn collect_with_stats(
        self,
    ) -> Result<(
        arrow::datatypes::SchemaRef,
        Vec<RecordBatch>,
        imbh_query::ScanStats,
    )> {
        if self.db.closed.load(Ordering::SeqCst) {
            return Err(Error::Closed);
        }
        // A read-only handle cannot see the writer's in-RAM buffer, so it rebuilds a fresh
        // point-in-time snapshot (manifest segments ∪ WAL tail) for every query (ARCHITECTURE.md §5).
        if self.db.access == Access::ReadOnly {
            let budget = self.db.mem_budget.total_bytes();
            // Read-during-delete hardening (ARCHITECTURE.md §5): retention/`compact()` on the writer
            // can unlink a segment between the moment this snapshot captured its path and the moment
            // DataFusion opens it. If a query fails *and* a segment the snapshot named has since
            // vanished, that race is the cause — re-derive from the current manifest (which no longer
            // lists the file) and retry, bounded. A failure with every path still present is a real
            // error and is returned as-is.
            for attempt in 0..READER_QUERY_TRIES {
                // After a failed attempt, force a rebuild: a cached (`Ttl`/`Manual`) snapshot may still
                // name the just-deleted segment, so reusing it would loop on the same error.
                let tables = self.db.reader_tables(attempt > 0)?;
                let paths = snapshot_paths(&tables);
                match imbh_query::run_sql(tables, budget, &self.sql, self.params.clone()).await {
                    Ok(out) => return Ok(out),
                    Err(e) => {
                        let last = attempt + 1 == READER_QUERY_TRIES;
                        if !last && paths.iter().any(|p| !p.exists()) {
                            continue; // a snapshotted segment was deleted mid-query; re-snapshot.
                        }
                        return Err(e);
                    }
                }
            }
            unreachable!("READER_QUERY_TRIES >= 1");
        }
        // Capture buffers ∪ segments for every table under one storage lock (ARCHITECTURE.md §5), so
        // a background seal (which moves rows buffer → segment under that same lock) cannot make this
        // query double-count. The intra-process analogue of the read-only re-check bracket.
        let snap = self.db.storage.query_snapshot()?;
        let tables = writer_tables(&self.db.storage, &snap);
        imbh_query::run_sql(
            tables,
            self.db.mem_budget.total_bytes(),
            &self.sql,
            self.params,
        )
        .await
    }

    /// Execute the query lazily, yielding result batches as a **bounded-memory stream** instead of
    /// collecting the whole result into RAM (ARCHITECTURE.md §10.11). The stream is `'static` and
    /// self-contained — it roots its own execution context and the mutable-buffer batches, so it stays
    /// valid across seals/retention like collected batches (see [`Query::collect_with_schema`] for the
    /// ownership invariant) — which is what lets a host wrap it as an `FFI_ArrowArrayStream` (the
    /// `cdata` feature) and hand it to a foreign runtime.
    ///
    /// The scan is **genuinely lazy** — one Parquet batch per `poll_next` (prescription I-4a) — so
    /// memory is bounded to roughly one batch plus any pipeline-breaker's build set, and each poll
    /// does a single synchronous segment read + decode and then yields. Two residual quanta are
    /// expected and bounded (not the caller's to eliminate): a pipeline-breaker (`ORDER BY`,
    /// hash-aggregate, `DISTINCT`) drains its whole input before the first output batch, and a cold
    /// segment's `std::fs` read blocks for that read (warm/page-cache reads are microseconds).
    ///
    /// The point-in-time snapshot is fixed when this method returns: a streamed query holds its view
    /// open for the stream's life (bounded memory, but a consistent snapshot). Because the scan is
    /// lazy, the segment Parquet files it names must not be unlinked until the stream is drained or
    /// dropped; unlike [`Query::collect`] there is no read-during-delete retry (a read-only handle
    /// captures the snapshot once). Scan statistics are not carried on this stream — use
    /// [`Query::stream_with_stats`] for the read-side counters, or [`Query::collect`]/the typed APIs
    /// when you need the full `QueryStats`.
    pub async fn stream(self) -> Result<SendableRecordBatchStream> {
        Ok(self.stream_with_stats().await?.0)
    }

    /// Like [`Query::stream`], but also returns a [`StreamStatsHandle`] sharing the query's live scan
    /// accumulator (prescription I-5). Because the scan is lazy, the handle's counters
    /// ([`StreamStatsHandle::get`] → [`ScanStats`]: segments read vs. bloom-pruned, rows materialized,
    /// whether the Tantivy index was consulted) accrue as the stream is drained and are **complete
    /// only after it is fully exhausted** — a mid-drain snapshot undercounts. `rows_returned` and
    /// `elapsed` are not included (the caller owns the drain loop and can time/count it); this is the
    /// read-side subset of the typed APIs' `QueryStats`.
    pub async fn stream_with_stats(self) -> Result<(SendableRecordBatchStream, StreamStatsHandle)> {
        if self.db.closed.load(Ordering::SeqCst) {
            return Err(Error::Closed);
        }
        let budget = self.db.mem_budget.total_bytes();
        // A read-only handle rebuilds a fresh point-in-time snapshot (manifest segments ∪ WAL tail);
        // the writer captures buffers ∪ segments under one storage lock. Either way the snapshot is
        // taken once and pinned for the stream's life (no collect-time retry loop — a lazy stream
        // cannot re-snapshot mid-drain).
        let tables = if self.db.access == Access::ReadOnly {
            self.db.reader_tables(false)?
        } else {
            let snap = self.db.storage.query_snapshot()?;
            writer_tables(&self.db.storage, &snap)
        };
        imbh_query::run_sql_stream(tables, budget, &self.sql, self.params).await
    }
}

/// Build the per-table query inputs for a **read-write** handle from a [`QuerySnapshot`] captured
/// under a single storage lock. Schemas are stateless (taken from `storage`); the buffers and
/// segment sets all come from the one atomic `snap`, so an interleaved seal cannot split a row
/// across buffer and segment. Symmetric with [`build_reader_tables`], which builds the same shape
/// from a cross-process [`imbh_storage::DiskSnapshot`].
#[cfg(feature = "query")]
fn writer_tables(storage: &Storage, snap: &imbh_storage::QuerySnapshot) -> Vec<TableInput> {
    let seg = |segs: &[SegmentRef]| segment_inputs(segs.to_vec(), snap.abs_paths(segs));
    let mut tables = vec![
        TableInput {
            name: "logs",
            schema: storage.schema(),
            buffer: snap.logs_buffer.clone(),
            segments: seg(&snap.logs_segments),
            text_column: Some("body"),
            bloom_columns: &[],
        },
        TableInput {
            name: "spans",
            schema: storage.schema_spans(),
            buffer: snap.spans_buffer.clone(),
            segments: seg(&snap.spans_segments),
            // The span `name` drives the Tantivy `RowSelection` bridge (its `.tidx` indexes the
            // name), symmetric with logs' `body`.
            text_column: Some("name"),
            // Spans segments carry Parquet bloom filters on these id columns; a `trace_id`/
            // `span_id` point lookup skips segments whose bloom proves the id absent (§8).
            bloom_columns: &["trace_id", "span_id"],
        },
    ];
    for table in SCALAR_METRIC_TABLES {
        tables.push(TableInput {
            name: table.as_str(),
            schema: storage.schema_metric_scalar(),
            buffer: snap
                .metric_buffer(table)
                .expect("scalar metric buffer batch"),
            segments: seg(&snap.metric(table)),
            text_column: None,
            bloom_columns: &[],
        });
    }
    tables.push(TableInput {
        name: Table::MetricsHistogram.as_str(),
        schema: storage.schema_histogram(),
        buffer: snap.histogram_buffer.clone(),
        segments: seg(&snap.metric(Table::MetricsHistogram)),
        text_column: None,
        bloom_columns: &[],
    });
    tables.push(TableInput {
        name: Table::MetricsExpHistogram.as_str(),
        schema: storage.schema_exp_histogram(),
        buffer: snap.exp_histogram_buffer.clone(),
        segments: seg(&snap.metric(Table::MetricsExpHistogram)),
        text_column: None,
        bloom_columns: &[],
    });
    tables.push(TableInput {
        name: Table::MetricsSummary.as_str(),
        schema: storage.schema_summary(),
        buffer: snap.summary_buffer.clone(),
        segments: seg(&snap.metric(Table::MetricsSummary)),
        text_column: None,
        bloom_columns: &[],
    });
    tables
}

/// The flush policy the scheduler will actually run: the host's, or — when it configured maintenance
/// but no policy — the default one with the maintenance interval as its seal cadence, which is what
/// the scheduler did before `DbBuilder::flush` existed. An *explicit* `FlushPolicy::manual()` is
/// honored as written and never gets an interval grafted on.
fn resolve_flush(
    policy: Option<FlushPolicy>,
    maintenance_interval: std::time::Duration,
) -> FlushPolicy {
    match policy {
        Some(policy) => policy,
        None => FlushPolicy::default().or_interval(maintenance_interval),
    }
}

/// The clockwork shared by the two maintenance loops (ARCHITECTURE.md §5/§7): it owns the elapsed-time
/// bookkeeping for the flush policy's periodic trigger, the WAL fsync timer, and the retention pass,
/// so the sync (owned-thread) and async (host-runtime) loops differ only in how they sleep.
///
/// Sleeping happens in slices of at most a second even when the policy's tick is longer, so `close()`
/// never waits a whole tick for the loop to notice the `closed` flag. The policy's tick therefore
/// throttles only the *gauge* evaluation — the one part that costs a lock (and, with a WAL-size
/// trigger, a directory scan).
struct FlushScheduler {
    policy: FlushPolicy,
    /// How often the buffer gauges are evaluated (the policy's effective tick).
    tick: std::time::Duration,
    /// How often `retain()` runs — the `Maintenance` interval.
    retention_interval: std::time::Duration,
    since_eval: std::time::Duration,
    since_seal: std::time::Duration,
    since_retention: std::time::Duration,
    since_wal_sync: std::time::Duration,
}

impl FlushScheduler {
    /// A second: the coarsest sleep that still keeps `close()` prompt.
    const MAX_SLICE: std::time::Duration = std::time::Duration::from_secs(1);

    fn new(policy: FlushPolicy, retention_interval: std::time::Duration) -> Self {
        FlushScheduler {
            tick: policy.effective_tick(),
            policy,
            retention_interval,
            since_eval: std::time::Duration::ZERO,
            since_seal: std::time::Duration::ZERO,
            since_retention: std::time::Duration::ZERO,
            since_wal_sync: std::time::Duration::ZERO,
        }
    }

    /// How long to sleep before the next [`Self::advance`].
    fn slice(&self) -> std::time::Duration {
        self.tick.min(Self::MAX_SLICE)
    }

    /// Do whatever `elapsed` more time has made due: seal (per the policy), fsync a
    /// `WalMode::Interval` WAL, and apply retention. Errors are swallowed exactly as they were before
    /// this loop had a policy — a scheduled pass that fails must not kill the scheduler; the next
    /// explicit `flush()`/`close()` surfaces the failure to the host.
    fn advance(&mut self, db: &Db, elapsed: std::time::Duration) {
        self.since_eval += elapsed;
        self.since_seal += elapsed;
        self.since_retention += elapsed;
        self.since_wal_sync += elapsed;

        // 1. Seal. The periodic trigger is our own bookkeeping; the rest read the live gauges, no more
        // often than one policy tick.
        let mut seal = self.policy.interval().is_some_and(|i| self.since_seal >= i);
        if !seal && self.since_eval >= self.tick && !self.policy.is_manual() {
            self.since_eval = std::time::Duration::ZERO;
            let gauges = db.storage.flush_gauges();
            let wal_bytes = self
                .policy
                .needs_wal_bytes()
                .then(|| db.storage.wal_bytes());
            seal = self.policy.triggered(
                gauges.buffer_bytes,
                gauges.buffer_rows,
                wal_bytes,
                gauges.idle_for,
                db.storage.seal_threshold_bytes(),
            );
        }
        if seal {
            self.since_seal = std::time::Duration::ZERO;
            self.since_eval = std::time::Duration::ZERO;
            let _ = db.storage.seal();
        }

        // 2. The WAL fsync timer — what makes `WalMode::Interval(d)` mean something. `Always` already
        // fsynced inline and `Off` asked us not to, so both report no interval.
        if let Some(d) = db.storage.wal_sync_interval()
            && self.since_wal_sync >= d
        {
            self.since_wal_sync = std::time::Duration::ZERO;
            let _ = db.storage.sync_wal();
        }

        // 3. Retention, on the maintenance interval.
        if self.since_retention >= self.retention_interval {
            self.since_retention = std::time::Duration::ZERO;
            let _ = db.storage.retain();
        }
    }
}

/// The background-maintenance loop (ARCHITECTURE.md §5), on an owned thread. Sleeps in slices of at
/// most a second to notice a close / drop promptly, and on each slice lets the [`FlushScheduler`] do
/// what has come due: seal per `flush`, fsync an interval-mode WAL, and apply retention every
/// `interval`. Exits when the last `Db` handle is dropped (the `Weak` fails to upgrade) or the DB is
/// closed.
fn run_maintenance(weak: Weak<Db>, interval: std::time::Duration, flush: FlushPolicy) {
    let mut sched = FlushScheduler::new(flush, interval);
    loop {
        let slice = sched.slice();
        std::thread::sleep(slice);
        let Some(inner) = weak.upgrade() else {
            break; // all Db handles dropped
        };
        if inner.closed.load(Ordering::Acquire) {
            break;
        }
        sched.advance(&inner, slice);
        drop(inner); // release the strong ref before sleeping again
    }
}

/// The async twin of [`run_maintenance`] for `Maintenance::Runtime`: same close/drop semantics and the
/// same [`FlushScheduler`], but scheduled on a host-provided tokio runtime (via `tokio::time::sleep`)
/// instead of an owned OS thread. `close()` awaits the returned task, so shutdown stays synchronous
/// with any in-flight seal.
async fn run_maintenance_async(weak: Weak<Db>, interval: std::time::Duration, flush: FlushPolicy) {
    let mut sched = FlushScheduler::new(flush, interval);
    loop {
        let slice = sched.slice();
        tokio::time::sleep(slice).await;
        let Some(inner) = weak.upgrade() else {
            break; // all Db handles dropped
        };
        if inner.closed.load(Ordering::Acquire) {
            break;
        }
        sched.advance(&inner, slice);
        drop(inner); // release the strong ref before the next sleep (never held across `.await`)
    }
}

/// All four metric row kinds decoded from one OTLP request, after the duplicate guard has run.
#[cfg(feature = "ingest")]
pub(crate) struct DecodedMetrics {
    pub(crate) rows: Vec<imbh_core::ScalarMetricRow>,
    pub(crate) histograms: Vec<imbh_core::HistogramRow>,
    pub(crate) exp_histograms: Vec<imbh_core::ExpHistogramRow>,
    pub(crate) summaries: Vec<imbh_core::SummaryRow>,
    /// Points that survived the guard — the four vector lengths summed.
    pub(crate) accepted: u64,
    /// Points the guard dropped as duplicate `(series, timestamp)` repeats. Always 0 unless
    /// [`Duplicates::Reject`] is configured.
    pub(crate) rejected: u64,
}

/// Decode an OTLP/metrics body **once** into all four row kinds and apply `guard`.
///
/// The single choke point shared by the inline path, the async decode, and WAL replay, so all three
/// make the same accept/reject decision under the same rule. One OTLP request carries scalar
/// (gauge/sum) + explicit-/exponential-histogram + summary points, and one WAL frame / LSN covers
/// them all.
#[cfg(feature = "ingest")]
fn decode_metrics(body: &[u8], guard: &dedup::DedupGuard) -> Result<DecodedMetrics> {
    let req = imbh_otlp::decode_metrics_request(body)?;
    let mut decoded = DecodedMetrics {
        rows: imbh_otlp::metrics_request_to_rows(&req),
        histograms: imbh_otlp::metrics_request_to_histogram_rows(&req),
        exp_histograms: imbh_otlp::metrics_request_to_exp_histogram_rows(&req),
        summaries: imbh_otlp::metrics_request_to_summary_rows(&req),
        accepted: 0,
        rejected: 0,
    };
    decoded.rejected = guard.retain(&mut decoded);
    decoded.accepted = (decoded.rows.len()
        + decoded.histograms.len()
        + decoded.exp_histograms.len()
        + decoded.summaries.len()) as u64;
    Ok(decoded)
}

/// Decode an OTLP/metrics request into an [`IngestJob::Metrics`] plus the accepted and rejected
/// point counts, for the async-ingest path.
///
/// The guard runs here, on the caller's thread, rather than in the worker — the queued receipt is
/// returned before the worker runs, so this is the only place the async path can report an exact
/// rejection count. The job carries the *filtered* rows alongside the *unfiltered* `raw` body.
#[cfg(feature = "ingest")]
fn decode_metrics_job(body: &[u8], guard: &dedup::DedupGuard) -> Result<(IngestJob, u64, u64)> {
    let decoded = decode_metrics(body, guard)?;
    Ok((
        IngestJob::Metrics {
            raw: body.to_vec(),
            rows: decoded.rows,
            histograms: decoded.histograms,
            exp_histograms: decoded.exp_histograms,
            summaries: decoded.summaries,
        },
        decoded.accepted,
        decoded.rejected,
    ))
}

/// The async-ingest worker (ARCHITECTURE.md §5/§10.5): the single consumer of the [`IngestChannel`].
/// It pops decoded jobs FIFO and performs the WAL append + Arrow encode + buffer push that the sync
/// path would have run on the caller's thread — calling the unchanged `Storage::ingest*` with
/// `sync_now = true`, so `WalMode::Always` durability is preserved (the fsync just happens here, off
/// the caller). Exits when the channel is closed **and** drained (a clean `close()` loses nothing) or
/// when the last `Db` handle is dropped (the `Weak` fails to upgrade — dropping without `close()`
/// discards in-flight jobs, matching the maintenance worker's lifecycle).
#[cfg(feature = "ingest")]
async fn run_ingest_worker(weak: Weak<Db>, chan: Arc<IngestChannel>) {
    loop {
        drain_ingest_burst(&weak, &chan);
        if chan.is_closed() {
            // A job can be accepted by a producer between our drain emptying the queue and our reading
            // `is_closed()` here (the producer observed `closed == false` and enqueued). Because the
            // channel fences enqueues once `close()` commits the flag under the queue lock, no *new*
            // job can arrive after this point, so one more drain catches that racing job and then we
            // exit having lost nothing.
            drain_ingest_burst(&weak, &chan);
            break; // closed and fully drained
        }
        chan.wait_for_item().await;
    }
}

/// Drain everything currently queued as one burst, appending each job to the WAL without a per-job
/// fsync, then group-commit the whole burst with a single fsync (a no-op unless `WalMode::Always`).
/// A worker-side panic (e.g. a poisoned storage mutex, or an `unwrap` deep in the Arrow encode) is
/// caught per job and counted like any other worker failure, so one bad job cannot kill the sole
/// consumer and wedge the queue (parking `Overflow::Block` producers) forever.
#[cfg(feature = "ingest")]
fn drain_ingest_burst(weak: &Weak<Db>, chan: &IngestChannel) {
    let mut processed = 0usize;
    while let Some(job) = chan.pop() {
        let Some(db) = weak.upgrade() else { return };
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            process_ingest_job(&db.storage, job)
        }));
        match outcome {
            Ok(Ok(())) => processed += 1,
            Ok(Err(_e)) => {
                chan.record_error();
                #[cfg(feature = "tracing")]
                tracing::warn!(error = %_e, "async ingest worker: job failed");
            }
            Err(_panic) => {
                chan.record_error();
                #[cfg(feature = "tracing")]
                tracing::error!("async ingest worker: job panicked; skipping it");
            }
        }
        drop(db); // never hold a strong Db ref across the caller's await
    }
    if processed == 0 {
        return;
    }
    let Some(db) = weak.upgrade() else { return };
    let commit =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| db.storage.group_commit()));
    match commit {
        Ok(Ok(())) => {}
        Ok(Err(_e)) => {
            chan.record_error();
            #[cfg(feature = "tracing")]
            tracing::warn!(error = %_e, "async ingest worker: group commit failed");
        }
        Err(_panic) => {
            chan.record_error();
            #[cfg(feature = "tracing")]
            tracing::error!("async ingest worker: group commit panicked");
        }
    }
}

/// Apply one decoded [`IngestJob`] to storage (the worker's per-job body). Appends with
/// `sync_now = false` — the worker batches the `WalMode::Always` fsyncs of a drained burst into one
/// `Storage::group_commit` (ARCHITECTURE.md §10.5); `Interval`/`Off` never fsync per-append anyway.
#[cfg(feature = "ingest")]
fn process_ingest_job(storage: &Storage, job: IngestJob) -> Result<()> {
    match job {
        IngestJob::Logs { raw, rows } => {
            storage.ingest(SIGNAL_LOGS, &raw, rows, false)?;
        }
        IngestJob::Traces { raw, rows } => {
            storage.ingest_traces(&raw, rows, false)?;
        }
        IngestJob::Metrics {
            raw,
            rows,
            histograms,
            exp_histograms,
            summaries,
        } => {
            storage.ingest_metrics(&raw, rows, histograms, exp_histograms, summaries, false)?;
        }
    }
    Ok(())
}

/// Serialize result batches into Arrow-IPC **stream** bytes (schema message + batches + EOS). An
/// empty result still emits a valid schema-only stream, so a reader always learns the columns.
#[cfg(feature = "query")]
fn encode_ipc_stream(
    schema: &arrow::datatypes::SchemaRef,
    batches: &[RecordBatch],
) -> Result<Vec<u8>> {
    use arrow::ipc::writer::StreamWriter;
    let mut buf = Vec::new();
    let mut writer = StreamWriter::try_new(&mut buf, schema)
        .map_err(|e| Error::query_ctx("arrow-ipc export", e))?;
    for batch in batches {
        writer
            .write(batch)
            .map_err(|e| Error::query_ctx("arrow-ipc export", e))?;
    }
    writer
        .finish()
        .map_err(|e| Error::query_ctx("arrow-ipc export", e))?;
    drop(writer);
    Ok(buf)
}

/// Decode one WAL frame by its signal tag and replay its rows into `storage`. Shared by open-time
/// recovery (into the writer's live buffer) and the read-only per-query snapshot (into a throwaway
/// in-memory buffer), so both see identical WAL-tail semantics. WAL-tail replay decodes the raw OTLP
/// payload, so it is compiled only when the OTLP decoder (`ingest`) is in the build; a query-only
/// consumer has no decoder and reads sealed segments only (its reader paths skip the replay loop).
#[cfg(feature = "ingest")]
fn replay_record(
    storage: &Storage,
    rec: &imbh_storage::WalRecord,
    guard: &dedup::DedupGuard,
) -> Result<()> {
    match rec.signal {
        SIGNAL_LOGS => storage.replay(rec.lsn, imbh_otlp::decode_logs_to_rows(&rec.payload)?),
        SIGNAL_TRACES => {
            storage.replay_traces(rec.lsn, imbh_otlp::decode_traces_to_rows(&rec.payload)?)
        }
        SIGNAL_METRICS => {
            // The WAL frame holds the raw body, so a point the guard dropped at ingest is still here
            // and must be re-decided by the same rule. The rejection count is deliberately discarded:
            // a replay rejection is a re-decision of one already made, not a fresh producer error, so
            // counting it would double-report the WAL tail in `stats().ingest_rejected`.
            let decoded = decode_metrics(&rec.payload, guard)?;
            // Same order as `Storage::ingest_metrics` pushes them, so replay and ingest agree on the
            // within-record row ordering.
            storage.replay_metrics(rec.lsn, decoded.rows);
            storage.replay_histograms(rec.lsn, decoded.histograms);
            storage.replay_exp_histograms(rec.lsn, decoded.exp_histograms);
            storage.replay_summaries(rec.lsn, decoded.summaries);
        }
        other => return Err(Error::unsupported_wal_signal(rec.lsn, other)),
    }
    Ok(())
}

/// Build the per-table query inputs for a **read-only** handle from a fresh on-disk snapshot
/// (ARCHITECTURE.md §5): the manifest's sealed segments unioned with the writer's live WAL tail. The
/// tail is materialized into a throwaway in-memory buffer via the same [`replay_record`] path as
/// open, so a reader sees the writer's just-ingested rows without touching the writer's process.
/// Re-run per query for near-real-time visibility.
/// Per-table [`TableStats`] for a **read-only** handle, built from the same cross-process disk
/// snapshot the query path uses (`read_disk_snapshot`): segment counts/rows/time-bounds from the
/// manifest, and unsealed buffer rows from replaying the WAL tail into a scratch buffer. Mirrors
/// [`build_reader_tables`]; without this a reader's `stats()` reads its (always-empty) live buffers
/// and reports zero rows for a populated database.
fn reader_stats(inner: &Db) -> Result<Vec<TableStats>> {
    let dir = inner
        .storage
        .dir()
        .ok_or_else(|| Error::storage_msg("read-only stats on a DB with no directory"))?;
    let snap = imbh_storage::read_disk_snapshot(dir)?;
    // The unsealed WAL tail → a scratch buffer; its `stats()` then carries exactly the unsealed row
    // counts per table (the scratch holds no segments of its own).
    let scratch = Storage::in_memory(Compression::default(), inner.mem_budget)
        .with_promote(inner.storage.promote().clone());
    // The WAL tail is decoded only when the OTLP decoder (`ingest`) is in the build; a query-only
    // consumer leaves the scratch empty and reports sealed-segment stats only.
    #[cfg(feature = "ingest")]
    for rec in &snap.pending {
        replay_record(&scratch, rec, &inner.dedup)?;
    }
    let buffer_rows: std::collections::HashMap<Table, u64> = scratch
        .stats()
        .into_iter()
        .map(|table| (table.table, table.buffer_rows))
        .collect();
    let make = |table: Table, segs: &[SegmentRef]| TableStats {
        table,
        segment_count: segs.len() as u64,
        segment_rows: segs.iter().map(|s| s.rows).sum(),
        buffer_rows: buffer_rows.get(&table).copied().unwrap_or(0),
        min_time_unix_nano: segs.iter().map(|s| s.min_time_unix_nano).min(),
        max_time_unix_nano: segs.iter().map(|s| s.max_time_unix_nano).max(),
    };
    let mut out = vec![
        make(Table::Logs, &snap.logs_segments),
        make(Table::Spans, &snap.spans_segments),
    ];
    for table in SCALAR_METRIC_TABLES {
        out.push(make(table, &snap.metric(table)));
    }
    out.push(make(
        Table::MetricsHistogram,
        &snap.metric(Table::MetricsHistogram),
    ));
    out.push(make(
        Table::MetricsExpHistogram,
        &snap.metric(Table::MetricsExpHistogram),
    ));
    out.push(make(
        Table::MetricsSummary,
        &snap.metric(Table::MetricsSummary),
    ));
    Ok(out)
}

#[cfg(feature = "query")]
impl Db {
    /// Build (or reuse) the per-table query inputs for a read-only handle, honoring the [`Refresh`]
    /// policy (ARCHITECTURE.md §5). `OnQuery` always rebuilds (near-real-time); `Ttl(d)` reuses the
    /// cached tables until `d` elapses; `Manual` reuses until an explicit [`Db::refresh`]. Every
    /// rebuild goes through the persistent incremental cursor, so it scans only newly appended WAL
    /// bytes. `force` bypasses the cache and rebuilds unconditionally — used by the read-during-delete
    /// retry, where a cached snapshot may name a segment the writer just unlinked.
    fn reader_tables(&self, force: bool) -> Result<Vec<TableInput>> {
        let mut cache = self.reader_cache.lock().unwrap();
        if !force && let Some((tables, built)) = &cache.built {
            let reuse = match self.refresh {
                Refresh::OnQuery => false,
                Refresh::Ttl(ttl) => built.elapsed() < ttl,
                Refresh::Manual => true,
            };
            if reuse {
                return Ok(tables.clone());
            }
        }
        let tables = build_reader_tables(self, &mut cache.cursor)?;
        // Cache the fresh build so a subsequent `Ttl`/`Manual` query can reuse it. `OnQuery` overwrites
        // it every time (and never reads it back), which also keeps the cursor's tail pruned.
        cache.built = Some((tables.clone(), std::time::Instant::now()));
        Ok(tables)
    }

    /// Rebuild a read-only handle's point-in-time snapshot now, so subsequent queries under
    /// [`Refresh::Manual`] or [`Refresh::Ttl`] see writes up to this moment (ARCHITECTURE.md §5). This
    /// is the explicit companion to those policies; under the default [`Refresh::OnQuery`] every query
    /// already refreshes, so calling this is harmless but unnecessary. A no-op for read-write and
    /// in-memory handles, which always query live state. Cheap: the rebuild scans only WAL bytes
    /// appended since the previous snapshot.
    pub fn refresh(&self) -> Result<()> {
        if self.access != Access::ReadOnly {
            return Ok(());
        }
        if self.closed.load(Ordering::SeqCst) {
            return Err(Error::Closed);
        }
        let mut cache = self.reader_cache.lock().unwrap();
        let tables = build_reader_tables(self, &mut cache.cursor)?;
        cache.built = Some((tables, std::time::Instant::now()));
        Ok(())
    }
}

#[cfg(feature = "query")]
fn build_reader_tables(
    inner: &Db,
    cursor: &mut imbh_storage::WalTailCursor,
) -> Result<Vec<TableInput>> {
    let dir = inner
        .storage
        .dir()
        .ok_or_else(|| Error::storage_msg("read-only query on a DB with no directory"))?;
    let snap = imbh_storage::read_disk_snapshot_incremental(dir, cursor)?;
    // The unsealed WAL tail → a scratch in-memory buffer (no writer lock, no WAL, no files written).
    // It shares the reader's promotion so its buffers match the segment/registered schema width.
    let scratch = Storage::in_memory(Compression::default(), inner.mem_budget)
        .with_promote(inner.storage.promote().clone());
    // The WAL tail is decoded only when the OTLP decoder (`ingest`) is in the build; a query-only
    // consumer leaves the scratch empty and queries sealed segments only.
    #[cfg(feature = "ingest")]
    for rec in &snap.pending {
        replay_record(&scratch, rec, &inner.dedup)?;
    }
    // Schemas are stateless — take them from the read-only storage; buffers from the scratch replay;
    // segments (+ their `.tidx` sidecars) from the snapshot's manifest, rooted at the DB dir.
    let storage = &inner.storage;
    let seg = |segs: &[SegmentRef]| segment_inputs(segs.to_vec(), snap.abs_paths(segs));
    let mut tables = vec![
        TableInput {
            name: "logs",
            schema: storage.schema(),
            buffer: scratch.buffer_snapshot()?,
            segments: seg(&snap.logs_segments),
            text_column: Some("body"),
            bloom_columns: &[],
        },
        TableInput {
            name: "spans",
            schema: storage.schema_spans(),
            buffer: scratch.buffer_snapshot_spans()?,
            segments: seg(&snap.spans_segments),
            text_column: Some("name"),
            bloom_columns: &["trace_id", "span_id"],
        },
    ];
    for table in SCALAR_METRIC_TABLES {
        tables.push(TableInput {
            name: table.as_str(),
            schema: storage.schema_metric_scalar(),
            buffer: scratch.buffer_snapshot_metric(table)?,
            segments: seg(&snap.metric(table)),
            text_column: None,
            bloom_columns: &[],
        });
    }
    tables.push(TableInput {
        name: Table::MetricsHistogram.as_str(),
        schema: storage.schema_histogram(),
        buffer: scratch.buffer_snapshot_histogram()?,
        segments: seg(&snap.metric(Table::MetricsHistogram)),
        text_column: None,
        bloom_columns: &[],
    });
    tables.push(TableInput {
        name: Table::MetricsExpHistogram.as_str(),
        schema: storage.schema_exp_histogram(),
        buffer: scratch.buffer_snapshot_exp_histogram()?,
        segments: seg(&snap.metric(Table::MetricsExpHistogram)),
        text_column: None,
        bloom_columns: &[],
    });
    tables.push(TableInput {
        name: Table::MetricsSummary.as_str(),
        schema: storage.schema_summary(),
        buffer: scratch.buffer_snapshot_summary()?,
        segments: seg(&snap.metric(Table::MetricsSummary)),
        text_column: None,
        bloom_columns: &[],
    });
    Ok(tables)
}

/// How many times a read-only query re-snapshots and retries when a segment it captured is unlinked
/// mid-flight by writer-side retention/`compact()` (read-during-delete, ARCHITECTURE.md §5). Each
/// retry re-derives from the current manifest, which no longer lists the deleted file, so two tries
/// clear a single deletion; the cap only bounds a pathologically aggressive retention loop.
#[cfg(feature = "query")]
const READER_QUERY_TRIES: usize = 4;

/// Every on-disk path a snapshot's segments name — the Parquet file and its `.tidx` sidecar (when
/// present). Used to detect the read-during-delete race: if a query fails and any of these has
/// vanished, a concurrent retention/compaction deleted it and the read-only path re-snapshots.
#[cfg(feature = "query")]
fn snapshot_paths(tables: &[TableInput]) -> Vec<PathBuf> {
    tables
        .iter()
        .flat_map(|t| &t.segments)
        .flat_map(|s| std::iter::once(s.parquet_path.clone()).chain(s.index_path.clone()))
        .collect()
}

/// Pair each segment with its `.tidx` sidecar (if built) for the RowSelection bridge.
#[cfg(feature = "query")]
fn segment_inputs(segments: Vec<SegmentRef>, paths: Vec<PathBuf>) -> Vec<SegmentInput> {
    segments
        .iter()
        .zip(paths)
        .map(|(sref, parquet_path)| {
            let idx = parquet_path.with_extension("tidx");
            SegmentInput {
                parquet_path,
                index_path: idx.is_dir().then_some(idx),
                rows: sref.rows,
            }
        })
        .collect()
}

/// Ingest result (ARCHITECTURE.md §10.5). `accepted` is always the number of decoded records. On the
/// inline (`Ingest::Sync`) path `lsn` is `Some(assigned_lsn)` and `durable` reflects the fsync policy.
/// On the async (`Ingest::Async`) path the receipt is a *queued acknowledgement*: `accepted` is real
/// but the row is not yet written, so `lsn == None` (see [`Self::is_queued`]) and `durable == false` —
/// confirm durability globally with [`Db::flush`]/[`Db::close`] rather than a per-call
/// `durable_through() >= receipt.lsn` handshake. Because `lsn` is `Option<Lsn>` the queued case cannot
/// masquerade as a real LSN 0.
#[cfg(feature = "ingest")]
#[derive(Debug, Clone)]
pub struct IngestReceipt {
    pub accepted: u64,
    pub rejected: u64,
    /// `Some(lsn)` on the inline path (the LSN assigned to these records, always ≥ 1); `None` while
    /// queued for the async-ingest worker (nothing written yet).
    pub lsn: Option<Lsn>,
    pub durable: bool,
}

#[cfg(feature = "ingest")]
impl IngestReceipt {
    /// `true` when the records were enqueued for the async-ingest worker rather than written inline;
    /// then `lsn` is `None` and `durable` carries no information yet.
    pub fn is_queued(&self) -> bool {
        self.lsn.is_none()
    }

    /// Receipt for the inline path: real `lsn`, `durable` per the fsync policy.
    fn synced(accepted: u64, rejected: u64, lsn: Lsn, durable: bool) -> Self {
        IngestReceipt {
            accepted,
            rejected,
            lsn: Some(lsn),
            durable,
        }
    }

    /// Receipt for the async path: records enqueued, write pending on the worker. `rejected` is
    /// still exact — the duplicate guard runs at decode time, on the caller's thread, before the job
    /// is enqueued.
    fn queued(accepted: u64, rejected: u64) -> Self {
        IngestReceipt {
            accepted,
            rejected,
            lsn: None,
            durable: false,
        }
    }
}

/// The blocking facade (ARCHITECTURE.md §10.12): a mirror of the async `Db` API for sync hosts, running
/// each call on an owned current-thread runtime.
pub struct BlockingDb {
    db: Arc<Db>,
    rt: Arc<tokio::runtime::Runtime>,
}

impl BlockingDb {
    #[cfg(feature = "ingest")]
    pub fn ingest_otlp_logs(&self, body: &[u8]) -> Result<IngestReceipt> {
        self.db.try_ingest_otlp_logs(body)
    }
    #[cfg(feature = "ingest")]
    pub fn ingest_otlp_traces(&self, body: &[u8]) -> Result<IngestReceipt> {
        self.db.try_ingest_otlp_traces(body)
    }
    #[cfg(feature = "ingest")]
    pub fn ingest_otlp_metrics(&self, body: &[u8]) -> Result<IngestReceipt> {
        self.db.try_ingest_otlp_metrics(body)
    }
    /// Run SQL and collect the result batches.
    #[cfg(feature = "query")]
    pub fn sql(&self, sql: &str) -> Result<Vec<RecordBatch>> {
        self.rt.block_on(self.db.sql(sql).collect())
    }
    pub fn flush(&self) -> Result<()> {
        self.rt.block_on(self.db.flush())
    }
    pub fn maintain(&self) -> Result<MaintenanceReport> {
        self.rt.block_on(self.db.maintain())
    }
    pub fn compact(&self) -> Result<CompactionReport> {
        self.rt.block_on(self.db.compact())
    }
    pub fn snapshot(&self, dir: impl AsRef<Path>) -> Result<SnapshotInfo> {
        self.rt.block_on(self.db.snapshot(dir))
    }
    pub fn stats(&self) -> Result<DbStats> {
        self.rt.block_on(self.db.stats())
    }
    /// Export a table's rows over `range` as Arrow-IPC stream bytes ([`Db::export`]).
    #[cfg(feature = "query")]
    pub fn export(&self, table: Table, range: TimeRange) -> Result<Vec<u8>> {
        self.rt.block_on(self.db.export(table, range))
    }
    pub fn close(&self) -> Result<()> {
        self.rt.block_on(self.db.close())
    }
    /// Rebuild the read-only snapshot now ([`Db::refresh`]); a no-op for read-write handles.
    #[cfg(feature = "query")]
    pub fn refresh(&self) -> Result<()> {
        self.db.refresh()
    }
}

/// Database statistics (ARCHITECTURE.md §10.11): per-table breakdown plus engine-wide gauges.
#[derive(Debug, Clone)]
pub struct DbStats {
    pub tables: Vec<TableStats>,
    /// Approximate live heap held by the mutable ingest buffers (all tables), in bytes.
    pub buffer_bytes: usize,
    /// On-disk WAL size in bytes (0 for in-memory / WAL-off DBs).
    pub wal_bytes: u64,
    /// Highest durable LSN (fsync'd WAL or captured in a sealed segment); `None` when nothing is
    /// durable yet.
    pub durable_lsn: Option<Lsn>,
    /// In-flight jobs in the async-ingest queue (`Ingest::Async`); 0 in the default inline mode.
    pub ingest_queue_depth: usize,
    /// Jobs evicted by `Overflow::DropOldest` since open (0 unless that policy is used).
    pub ingest_dropped: u64,
    /// Async-ingest worker failures since open (WAL/buffer errors with no caller to return to).
    pub ingest_errors: u64,
    /// Metric points dropped since open by the duplicate-timestamp guard (0 unless
    /// [`Duplicates::Reject`] is configured). The cumulative "is a producer republishing?" signal.
    pub ingest_rejected: u64,
}

/// Result of [`Db::maintain`] (ARCHITECTURE.md §10.2).
#[derive(Debug, Clone)]
pub struct MaintenanceReport {
    pub sealed: bool,
    pub segments_dropped: u64,
    pub bytes_freed: u64,
}

/// JSON round-trip coverage for the `serde` feature: the query builders and result DTOs must
/// serialize and deserialize losslessly. Equality is checked by re-serializing the deserialized
/// value and comparing the two JSON strings (most DTOs don't implement `PartialEq`).
#[cfg(all(test, feature = "serde"))]
mod serde_tests {
    use std::time::Duration;

    use super::{
        Aggregation, AnyValue, Attributes, Direction, DurationNs, ExpHistogramQuery,
        HistogramQuery, LogEntry, LogQuery, Matrix, MetricQuery, MetricSeries, Sample,
        SeverityNumber, Span, SpanId, SpanMetricsQuery, TimeRange, Timestamp, Trace, TraceId,
        TraceQuery,
    };

    /// Serialize → deserialize → re-serialize and assert the JSON is identical (a lossless round
    /// trip without needing `PartialEq`).
    fn assert_roundtrip<T: serde::Serialize + serde::de::DeserializeOwned>(v: &T) -> String {
        let json1 = serde_json::to_string(v).expect("serialize");
        let back: T = serde_json::from_str(&json1).expect("deserialize");
        let json2 = serde_json::to_string(&back).expect("re-serialize");
        assert_eq!(json1, json2, "round-trip changed the JSON");
        json1
    }

    fn sample_attrs() -> Attributes {
        Attributes::from_pairs(vec![
            ("http.route".to_owned(), AnyValue::Str("/cart".to_owned())),
            ("status".to_owned(), AnyValue::Int(500)),
        ])
    }

    #[test]
    fn log_query_roundtrips() {
        let q = LogQuery::new()
            .service("cart")
            .severity_at_least(SeverityNumber::ERROR)
            .matches("timeout")
            .attr_eq("env", "prod")
            .attr_ge("status", 500.0)
            .attr_lt("status", 600.0)
            .attr_regex("path", "^/api")
            .attr_in("route", &["/cart", "/pay"])
            .attr_not_in("route", &["/health"])
            .range(TimeRange::between(Timestamp(0), Timestamp(1_000)))
            .direction(Direction::Forward)
            .limit(50);
        let json = assert_roundtrip(&q);
        // The NumOp operator and the Direction serialize by name — a shape guard.
        assert!(json.contains("\"Ge\""), "attr_ge → NumOp::Ge: {json}");
        assert!(json.contains("\"Forward\""), "direction: {json}");
    }

    #[test]
    fn metric_and_histogram_queries_roundtrip() {
        let m = MetricQuery::gauge("cpu")
            .aggregation(Aggregation::Max)
            .filter("host", "a")
            .filter_ne("region", "eu")
            .filter_regex("pod", "web-.*")
            .group_by("host")
            .rate_counter()
            .step(Duration::from_secs(30))
            .range(TimeRange::between(Timestamp(1), Timestamp(2)));
        assert_roundtrip(&m);

        let h = HistogramQuery::new("latency")
            .quantile(0.99)
            .filter("route", "/pay")
            .step(Duration::from_secs(60));
        assert_roundtrip(&h);

        let e = ExpHistogramQuery::new("latency")
            .quantile(0.5)
            .group_by("svc");
        assert_roundtrip(&e);
    }

    #[test]
    fn trace_queries_roundtrip() {
        let t = TraceQuery::new()
            .service("cart")
            .matches("checkout")
            .min_duration(Duration::from_millis(5))
            .status("ERROR")
            .attr_gt("retries", 2.0);
        assert_roundtrip(&t);

        let sm = SpanMetricsQuery::new()
            .service("cart")
            .group_by("http.route")
            .step(Duration::from_secs(15));
        assert_roundtrip(&sm);
    }

    #[test]
    fn result_dtos_roundtrip() {
        let entry = LogEntry {
            time: Timestamp(10),
            observed_time: Some(Timestamp(11)),
            severity_number: SeverityNumber::ERROR,
            severity_text: Some("ERROR".to_owned()),
            service: Some("cart".to_owned()),
            body: "boom".to_owned(),
            attributes: sample_attrs(),
            resource: Attributes::new(),
            scope: Attributes::new(),
            trace_id: Some(TraceId([0xab; 16])),
            span_id: Some(SpanId([0x01; 8])),
            flags: 3,
        };
        let json = assert_roundtrip(&entry);
        // The embedded ids serialize as hex strings (via imbh-core's manual impls).
        assert!(json.contains(&"ab".repeat(16)), "trace id as hex: {json}");

        let matrix = Matrix(vec![MetricSeries {
            labels: vec![("host".to_owned(), "a".to_owned())],
            samples: vec![
                Sample {
                    time: Timestamp(1),
                    value: 2.5,
                },
                Sample {
                    time: Timestamp(2),
                    value: 3.0,
                },
            ],
        }]);
        assert_roundtrip(&matrix);

        let span = Span {
            trace_id: TraceId([0xcd; 16]),
            span_id: SpanId([0x02; 8]),
            parent_span_id: None,
            name: "GET /".to_owned(),
            kind: "SERVER".to_owned(),
            start_time: Timestamp(100),
            duration_ns: DurationNs(500),
            status_code: "OK".to_owned(),
            status_message: None,
            service: Some("cart".to_owned()),
            attributes: sample_attrs(),
            resource: Attributes::new(),
            scope: Attributes::new(),
            events: None,
            links: None,
            trace_state: None,
            flags: 0,
        };
        let trace = Trace {
            trace_id: TraceId([0xcd; 16]),
            root_service: Some("cart".to_owned()),
            root_name: Some("GET /".to_owned()),
            start_time: Timestamp(100),
            duration_ns: DurationNs(500),
            spans: vec![span],
        };
        assert_roundtrip(&trace);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arrow::array::{Array, Float64Array, Int64Array};

    /// Build a one-record OTLP/logs protobuf body for `service`, `body`.
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

    /// Build a one-record OTLP/logs body with severity + attributes.
    fn otlp_rich(
        service: &str,
        body_text: &str,
        time: u64,
        severity: i32,
        attrs: &[(&str, &str)],
    ) -> Vec<u8> {
        use opentelemetry_proto::tonic::collector::logs::v1::ExportLogsServiceRequest;
        use opentelemetry_proto::tonic::common::v1::{AnyValue as PbAny, KeyValue, any_value};
        use opentelemetry_proto::tonic::logs::v1::{LogRecord, ResourceLogs, ScopeLogs};
        use opentelemetry_proto::tonic::resource::v1::Resource;
        use prost::Message;

        let sv = |s: &str| PbAny {
            value: Some(any_value::Value::StringValue(s.to_owned())),
        };
        let kv = |k: &str, v: &str| KeyValue {
            key: k.to_owned(),
            value: Some(sv(v)),
            ..Default::default()
        };
        ExportLogsServiceRequest {
            resource_logs: vec![ResourceLogs {
                resource: Some(Resource {
                    attributes: vec![kv("service.name", service)],
                    ..Default::default()
                }),
                scope_logs: vec![ScopeLogs {
                    log_records: vec![LogRecord {
                        time_unix_nano: time,
                        severity_number: severity,
                        body: Some(sv(body_text)),
                        attributes: attrs.iter().map(|(k, v)| kv(k, v)).collect(),
                        ..Default::default()
                    }],
                    ..Default::default()
                }],
                ..Default::default()
            }],
        }
        .encode_to_vec()
    }

    /// `SELECT count(*)` over the `logs` table, for the cross-process reader assertions.
    async fn count_logs(db: &Arc<Db>) -> i64 {
        let batches = db
            .sql("SELECT count(*) AS n FROM logs")
            .collect()
            .await
            .unwrap();
        batches[0]
            .column(0)
            .as_any()
            .downcast_ref::<Int64Array>()
            .unwrap()
            .value(0)
    }

    /// Column names of the canonical `logs` batch projection returned by `query_batches`.
    #[cfg(feature = "proto")]
    const LOG_BATCH_COLS: &[&str] = &[
        "time",
        "observed_time",
        "service",
        "severity_number",
        "severity_text",
        "body",
        "attributes",
        "resource",
        "scope",
        "trace_id",
        "span_id",
        "flags",
    ];

    /// A protobuf `LogQuery` maps onto the builder and executes identically to the hand-built query —
    /// the proto→builder→SQL→Arrow path is faithful for the exercised fields (§10.17).
    #[cfg(feature = "proto")]
    #[tokio::test(flavor = "current_thread")]
    async fn proto_log_query_maps_and_executes() {
        use crate::proto;

        let db = Db::in_memory().open().unwrap();
        for (body, status) in [("a", "200"), ("b", "500"), ("c", "503")] {
            db.ingest_otlp_logs(&otlp_rich("cart", body, 1, 17, &[("status", status)]))
                .await
                .unwrap();
        }
        db.ingest_otlp_logs(&otlp_rich("checkout", "d", 2, 17, &[("status", "500")]))
            .await
            .unwrap();

        // service=cart AND status >= 500 → rows "b" and "c".
        let pb = proto::LogQuery {
            service: Some("cart".to_owned()),
            min_severity: Some(17),
            attr_num: vec![proto::NumFilter {
                key: "status".to_owned(),
                op: proto::NumOp::Ge as i32,
                value: 500.0,
            }],
            direction: proto::Direction::Forward as i32,
            ..Default::default()
        };
        let from_proto = LogQuery::try_from(pb).unwrap();
        let hand = LogQuery::new()
            .service("cart")
            .severity_at_least(SeverityNumber::ERROR)
            .attr_ge("status", 500.0)
            .direction(Direction::Forward);

        let (pbatches, pstats) = db
            .logs()
            .query_batches_with_stats(from_proto)
            .await
            .unwrap();
        let (hbatches, hstats) = db.logs().query_batches_with_stats(hand).await.unwrap();

        let rows = |bs: &[RecordBatch]| bs.iter().map(|b| b.num_rows()).sum::<usize>();
        assert_eq!(rows(&pbatches), 2, "cart + status>=500 → b, c");
        assert_eq!(rows(&pbatches), rows(&hbatches), "proto == hand-built");
        assert_eq!(pstats.rows_returned, 2);
        assert_eq!(pstats.rows_returned, hstats.rows_returned);

        // Canonical schema is stable (a binding contract).
        let schema = pbatches[0].schema();
        let names: Vec<&str> = schema.fields().iter().map(|f| f.name().as_str()).collect();
        assert_eq!(names, LOG_BATCH_COLS);
    }

    /// The metric / span-metric proto mappings compile to executable SQL (smoke test on an empty DB:
    /// the batch entry points return an Ok, empty result — no ingest fixtures needed), and the
    /// histogram mappings convert without error.
    #[cfg(feature = "proto")]
    #[tokio::test(flavor = "current_thread")]
    async fn proto_metric_mappings_execute() {
        use crate::proto;

        let db = Db::in_memory().open().unwrap();

        let mq = proto::MetricQuery {
            table: proto::MetricTable::Gauge as i32,
            metric: "cpu".to_owned(),
            aggregation: Some(proto::Aggregation::Max as i32),
            group_by: vec!["host".to_owned()],
            filters: vec![proto::LabelFilter {
                key: "region".to_owned(),
                op: proto::LabelOp::Ne as i32,
                value: "eu".to_owned(),
            }],
            rate: proto::RateMode::Counter as i32,
            step_nanos: Some(30_000_000_000),
            ..Default::default()
        };
        let (batches, _) = db
            .metrics()
            .range_batches(MetricQuery::try_from(mq).unwrap())
            .await
            .unwrap();
        assert_eq!(batches.iter().map(|b| b.num_rows()).sum::<usize>(), 0);

        let sm = proto::SpanMetricsQuery {
            service: Some("cart".to_owned()),
            group_by: vec!["http.route".to_owned()],
            step_nanos: Some(15_000_000_000),
            ..Default::default()
        };
        db.traces()
            .span_metrics_batches(SpanMetricsQuery::try_from(sm).unwrap())
            .await
            .unwrap();

        // Histogram builders convert cleanly (execution is covered by the DTO-path tests).
        let h = proto::HistogramQuery {
            metric: "latency".to_owned(),
            phi: Some(0.99),
            filters: vec![proto::LabelFilter {
                key: "route".to_owned(),
                op: proto::LabelOp::Eq as i32,
                value: "/pay".to_owned(),
            }],
            ..Default::default()
        };
        HistogramQuery::try_from(h).unwrap();
        ExpHistogramQuery::try_from(proto::ExpHistogramQuery {
            metric: "latency".to_owned(),
            ..Default::default()
        })
        .unwrap();
    }

    /// Malformed protobuf requests (values outside the domain type's range) are rejected as user
    /// errors rather than silently coerced.
    #[cfg(feature = "proto")]
    #[test]
    fn proto_rejects_malformed_requests() {
        use crate::proto;

        // Out-of-range enum discriminant.
        let bad_dir = proto::LogQuery {
            direction: 99,
            ..Default::default()
        };
        let err = LogQuery::try_from(bad_dir).unwrap_err();
        assert!(err.is_user_error(), "bad discriminant is a user error");

        // Severity above the u8 range.
        assert!(
            LogQuery::try_from(proto::LogQuery {
                min_severity: Some(300),
                ..Default::default()
            })
            .is_err()
        );

        // Invalid label-op discriminant on a metric filter.
        assert!(
            MetricQuery::try_from(proto::MetricQuery {
                metric: "m".to_owned(),
                filters: vec![proto::LabelFilter {
                    key: "k".to_owned(),
                    op: 42,
                    value: "v".to_owned(),
                }],
                ..Default::default()
            })
            .is_err()
        );

        // Negative step duration.
        assert!(
            MetricQuery::try_from(proto::MetricQuery {
                metric: "m".to_owned(),
                step_nanos: Some(-1),
                ..Default::default()
            })
            .is_err()
        );
    }

    /// `encode_query_stats` copies the scan counters into the protobuf envelope faithfully.
    #[cfg(feature = "proto")]
    #[tokio::test(flavor = "current_thread")]
    async fn proto_query_stats_encodes() {
        let db = Db::in_memory().open().unwrap();
        db.ingest_otlp_logs(&otlp_log("svc", "x", 1)).await.unwrap();
        let (_, stats) = db
            .logs()
            .query_batches_with_stats(LogQuery::new())
            .await
            .unwrap();
        let pb = crate::proto::encode_query_stats(&stats);
        assert_eq!(pb.rows_returned, stats.rows_returned);
        assert_eq!(pb.rows_returned, 1);
        assert_eq!(pb.elapsed_ns, stats.elapsed.0);
        assert_eq!(pb.used_index, stats.used_index);
    }

    /// A read-only handle in a *separate* handle (standing in for a separate process) sees the
    /// writer's just-ingested rows via the WAL tail before any seal, and the count is stable across a
    /// seal — the manifest re-check bracket neither double-counts nor drops rows (ARCHITECTURE.md §5).
    #[tokio::test(flavor = "current_thread")]
    async fn cross_process_reader_sees_tail_and_survives_seal() {
        let dir = tempfile::tempdir().unwrap();
        let writer = Db::builder(dir.path()).wal(WalMode::Always).open().unwrap();
        let reader = Db::open_read_only(dir.path()).unwrap();

        assert_eq!(count_logs(&reader).await, 0, "empty DB");

        // Writer ingests; the reader sees the unsealed rows from the WAL tail (near-real-time).
        writer
            .ingest_otlp_logs(&otlp_log("svc", "one", 1))
            .await
            .unwrap();
        writer
            .ingest_otlp_logs(&otlp_log("svc", "two", 2))
            .await
            .unwrap();
        assert_eq!(
            count_logs(&reader).await,
            2,
            "reader sees unsealed rows via the WAL tail"
        );

        // Seal moves rows buffer→segment and reclaims the WAL. The reader count is unchanged.
        writer.flush().await.unwrap();
        assert_eq!(
            count_logs(&reader).await,
            2,
            "same rows after seal, now served from segments — no double-count, no drop"
        );

        // Post-seal ingest is visible too (fresh snapshot per query).
        writer
            .ingest_otlp_logs(&otlp_log("svc", "three", 3))
            .await
            .unwrap();
        assert_eq!(count_logs(&reader).await, 3);
    }

    /// `Refresh::Manual` pins the reader's snapshot: it is built on the first query and then frozen
    /// until an explicit `refresh()`, so writes in between are invisible (ARCHITECTURE.md §5).
    #[tokio::test(flavor = "current_thread")]
    async fn reader_manual_refresh_pins_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let writer = Db::builder(dir.path()).wal(WalMode::Always).open().unwrap();
        let reader = Db::builder(dir.path())
            .access(Access::ReadOnly)
            .refresh(Refresh::Manual)
            .open()
            .unwrap();

        writer
            .ingest_otlp_logs(&otlp_log("svc", "one", 1))
            .await
            .unwrap();
        // First query builds and caches the snapshot (one row visible).
        assert_eq!(count_logs(&reader).await, 1);

        // A write after that first snapshot stays invisible until refresh — the pinned view.
        writer
            .ingest_otlp_logs(&otlp_log("svc", "two", 2))
            .await
            .unwrap();
        assert_eq!(count_logs(&reader).await, 1, "Manual pins the snapshot");

        // Explicit refresh rebuilds; the new row appears.
        reader.refresh().unwrap();
        assert_eq!(count_logs(&reader).await, 2, "refresh() picks up the write");
    }

    /// `Refresh::Ttl` reuses the snapshot within the window and rebuilds once it elapses
    /// (ARCHITECTURE.md §5).
    #[tokio::test(flavor = "current_thread")]
    async fn reader_ttl_reuses_then_rebuilds() {
        let dir = tempfile::tempdir().unwrap();
        let writer = Db::builder(dir.path()).wal(WalMode::Always).open().unwrap();
        let reader = Db::builder(dir.path())
            .access(Access::ReadOnly)
            .refresh(Refresh::Ttl(std::time::Duration::from_millis(120)))
            .open()
            .unwrap();

        writer
            .ingest_otlp_logs(&otlp_log("svc", "one", 1))
            .await
            .unwrap();
        assert_eq!(count_logs(&reader).await, 1); // builds + caches

        // A write inside the TTL window is not yet visible (the cached snapshot is reused).
        writer
            .ingest_otlp_logs(&otlp_log("svc", "two", 2))
            .await
            .unwrap();
        assert_eq!(
            count_logs(&reader).await,
            1,
            "within TTL: cached snapshot reused"
        );

        // Past the TTL, the next query rebuilds and sees the write.
        tokio::time::sleep(std::time::Duration::from_millis(180)).await;
        assert_eq!(
            count_logs(&reader).await,
            2,
            "TTL elapsed: snapshot rebuilt"
        );
    }

    /// `refresh()` is a harmless no-op on a read-write handle (it always queries live state), and the
    /// default `Refresh::OnQuery` reader needs no refresh to see each write.
    #[tokio::test(flavor = "current_thread")]
    async fn refresh_is_noop_on_writer_and_on_query_is_live() {
        let dir = tempfile::tempdir().unwrap();
        let writer = Db::builder(dir.path()).wal(WalMode::Always).open().unwrap();
        writer.refresh().expect("refresh() is a no-op for a writer");

        let reader = Db::open_read_only(dir.path()).unwrap(); // default OnQuery
        writer
            .ingest_otlp_logs(&otlp_log("svc", "one", 1))
            .await
            .unwrap();
        assert_eq!(count_logs(&reader).await, 1, "OnQuery is near-real-time");
        writer
            .ingest_otlp_logs(&otlp_log("svc", "two", 2))
            .await
            .unwrap();
        // No refresh() call — OnQuery rebuilds every query.
        assert_eq!(
            count_logs(&reader).await,
            2,
            "OnQuery sees the next write immediately"
        );
    }

    /// The single-writer lock rejects a second writer on the same directory but never a reader, and
    /// releases when the writer drops (ARCHITECTURE.md §5).
    #[tokio::test(flavor = "current_thread")]
    async fn second_writer_rejected_readers_allowed() {
        let dir = tempfile::tempdir().unwrap();
        let writer = Db::builder(dir.path()).wal(WalMode::Always).open().unwrap();

        // `Db` is not `Debug`, so match rather than `unwrap_err`.
        let err = match Db::builder(dir.path()).open() {
            Ok(_) => panic!("a second writer must be rejected while the first holds the lock"),
            Err(e) => e,
        };
        assert!(
            format!("{err}").contains("lock held"),
            "second writer is rejected: {err}"
        );

        // Any number of readers coexist with the writer.
        let _r1 = Db::open_read_only(dir.path()).unwrap();
        let _r2 = Db::open_read_only(dir.path()).unwrap();

        // Dropping the writer frees the lock for a new writer.
        drop(writer);
        let _w2 = Db::builder(dir.path())
            .wal(WalMode::Always)
            .open()
            .expect("lock is free after the first writer drops");
    }

    /// Every write path on a read-only handle refuses with a user-facing read-only error.
    #[tokio::test(flavor = "current_thread")]
    async fn read_only_handle_refuses_writes() {
        let dir = tempfile::tempdir().unwrap();
        let _writer = Db::builder(dir.path()).wal(WalMode::Always).open().unwrap();
        let reader = Db::open_read_only(dir.path()).unwrap();

        let err = reader
            .ingest_otlp_logs(&otlp_log("svc", "x", 1))
            .await
            .unwrap_err();
        assert!(err.is_user_error(), "a read-only write is a user error");
        assert!(format!("{err}").contains("read-only"), "{err}");
        assert!(reader.flush().await.is_err(), "flush refused");
        assert!(reader.maintain().await.is_err(), "maintain refused");
        assert!(reader.compact().await.is_err(), "compact refused");
    }

    /// A read-only open against a **WAL-off** writer is rejected by default (the reader could only see
    /// seal-interval freshness, not near-real-time), and `allow_stale_reads()` opts into it
    /// (ARCHITECTURE.md §5). A WAL-on writer is never rejected.
    #[tokio::test(flavor = "current_thread")]
    async fn read_only_guards_against_wal_off_writer() {
        let dir = tempfile::tempdir().unwrap();
        let writer = Db::builder(dir.path()).wal(WalMode::Off).open().unwrap();
        writer
            .ingest_otlp_logs(&otlp_log("svc", "one", 1))
            .await
            .unwrap();

        // Default read-only open is rejected, with a user-facing WAL-disabled error.
        let err = match Db::open_read_only(dir.path()) {
            Ok(_) => panic!("read-only open must reject a WAL-off writer by default"),
            Err(e) => e,
        };
        assert!(err.is_user_error(), "reader-wal-disabled is a user error");
        assert!(
            format!("{err}").contains("WAL is disabled"),
            "clear WAL-disabled message: {err}"
        );

        // Opting in accepts seal-interval freshness and opens successfully.
        let reader = Db::builder(dir.path())
            .access(Access::ReadOnly)
            .allow_stale_reads()
            .open()
            .expect("allow_stale_reads() bypasses the guard");
        // A WAL-off writer only exposes rows after a seal; once sealed, the reader sees them.
        writer.flush().await.unwrap();
        assert_eq!(count_logs(&reader).await, 1);

        // A WAL-on writer (default open, interval WAL) is never rejected.
        let dir2 = tempfile::tempdir().unwrap();
        let _w2 = Db::builder(dir2.path()).open().unwrap();
        assert!(
            Db::open_read_only(dir2.path()).is_ok(),
            "a WAL-on writer's reader opens without opting in"
        );
    }

    /// A single-threaded tokio runtime for the concurrency tests (the crate's `tokio` has no
    /// `rt-multi-thread`), so each concurrent actor drives its own runtime on its own OS thread.
    fn ct_rt() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap()
    }

    /// Read one `Int64` cell from a result batch (count aggregates), for the concurrency assertions.
    fn int_at(batch: &RecordBatch, col: usize) -> i64 {
        batch
            .column(col)
            .as_any()
            .downcast_ref::<Int64Array>()
            .unwrap()
            .value(0)
    }

    /// A read-write query captures buffer ∪ segments under one lock ([`Storage::query_snapshot`]), so
    /// a seal running concurrently on another task cannot make the query double-count a row (seen in
    /// both the buffer and the freshly written segment) or transiently drop one (ARCHITECTURE.md §5).
    /// With distinct row bodies, `count(*) == count(DISTINCT body)` is the no-double-count witness and
    /// a never-decreasing count is the no-drop witness.
    #[test]
    fn writer_query_atomic_across_concurrent_seal() {
        let dir = tempfile::tempdir().unwrap();
        let writer = Db::builder(dir.path()).wal(WalMode::Always).open().unwrap();
        let n = 300u64;

        let w = writer.clone();
        let ingest = std::thread::spawn(move || {
            ct_rt().block_on(async move {
                for i in 0..n {
                    w.ingest_otlp_logs(&otlp_log("svc", &format!("row-{i}"), i + 1))
                        .await
                        .unwrap();
                    if i % 5 == 0 {
                        w.flush().await.unwrap(); // seal mid-stream → constant buffer→segment churn
                    }
                }
            });
        });

        ct_rt().block_on(async {
            let mut prev = 0i64;
            while !ingest.is_finished() {
                let rows = writer
                    .sql("SELECT count(*) AS n, count(DISTINCT body) AS d FROM logs")
                    .collect()
                    .await
                    .unwrap();
                let all = int_at(&rows[0], 0);
                let dist = int_at(&rows[0], 1);
                assert_eq!(
                    all, dist,
                    "a concurrent seal double-counted buffer∪segments"
                );
                assert!(
                    all >= prev,
                    "count went backwards ({prev} → {all}): a seal dropped rows"
                );
                assert!(all <= n as i64, "count {all} exceeds total ingested {n}");
                prev = all;
            }
            assert_eq!(
                count_logs(&writer).await,
                n as i64,
                "every ingested row present after the run"
            );
        });
        ingest.join().unwrap();
    }

    /// A read-only reader re-derives its snapshot per query, but writer-side retention can unlink a
    /// segment between the snapshot and DataFusion's file open. The bounded retry-on-missing
    /// ([`READER_QUERY_TRIES`]) re-snapshots from the current manifest instead of surfacing a spurious
    /// `NotFound`, so every concurrent read succeeds even under an aggressive retention loop
    /// (ARCHITECTURE.md §5). Distinct bodies keep `count(*) == count(DISTINCT body)` on the reader path.
    #[test]
    fn reader_tolerates_segment_deletion_under_retention() {
        let dir = tempfile::tempdir().unwrap();
        let writer = Db::builder(dir.path())
            .wal(WalMode::Always)
            // 1-byte budget ⇒ retention drops every just-sealed segment: maximal deletion churn.
            .retention(Retention::none().max_disk_bytes(1))
            .open()
            .unwrap();
        let reader = Db::open_read_only(dir.path()).unwrap();

        let w = writer.clone();
        let ingest = std::thread::spawn(move || {
            ct_rt().block_on(async move {
                for i in 0..200u64 {
                    w.ingest_otlp_logs(&otlp_log("svc", &format!("row-{i}"), i + 1))
                        .await
                        .unwrap();
                    w.maintain().await.unwrap(); // seal + aggressive retention → segment deletion churn
                }
            });
        });

        ct_rt().block_on(async {
            let mut queries = 0u64;
            while !ingest.is_finished() {
                // Must never error with a vanished-segment `NotFound`; the retry re-snapshots.
                let rows = reader
                    .sql("SELECT count(*) AS n, count(DISTINCT body) AS d FROM logs")
                    .collect()
                    .await
                    .expect("reader query survives a concurrent segment deletion");
                assert_eq!(
                    int_at(&rows[0], 0),
                    int_at(&rows[0], 1),
                    "no double-count on the reader path"
                );
                queries += 1;
            }
            assert!(queries > 0, "the reader ran at least one concurrent query");
        });
        ingest.join().unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn logs_cursor_paging() {
        let db = Db::in_memory().open().unwrap();
        for i in 0..5u64 {
            db.ingest_otlp_logs(&otlp_rich("cart", &format!("msg {i}"), i + 1, 9, &[]))
                .await
                .unwrap();
        }

        // Page forward (ascending time) with limit 2 until the cursor runs out.
        let mut collected = Vec::new();
        let mut cursor: Option<PageCursor> = None;
        let mut pages = 0;
        loop {
            let mut q = LogQuery::new().direction(Direction::Forward).limit(2);
            if let Some(c) = cursor {
                q = q.after(c);
            }
            let page = db.logs().query(q).await.unwrap();
            pages += 1;
            for e in &page.entries {
                collected.push(e.body.clone());
            }
            match page.next {
                Some(c) => cursor = Some(c),
                None => break,
            }
            assert!(pages < 10, "paging should terminate");
        }
        assert_eq!(pages, 3, "5 rows / limit 2 → pages of 2,2,1");
        assert_eq!(
            collected,
            vec!["msg 0", "msg 1", "msg 2", "msg 3", "msg 4"],
            "each row appears exactly once, in order"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn logs_attr_matchers() {
        let db = Db::in_memory().open().unwrap();
        db.ingest_otlp_logs(&otlp_rich(
            "cart",
            "a",
            1,
            9,
            &[("http.route", "/cart"), ("env", "prod")],
        ))
        .await
        .unwrap();
        db.ingest_otlp_logs(&otlp_rich(
            "cart",
            "b",
            2,
            9,
            &[("http.route", "/checkout")],
        ))
        .await
        .unwrap();
        db.ingest_otlp_logs(&otlp_rich("cart", "c", 3, 9, &[("http.route", "/pay")]))
            .await
            .unwrap();

        // attr_exists: only the row that has the `env` attribute.
        let has_env = db
            .logs()
            .query(LogQuery::new().attr_exists("env"))
            .await
            .unwrap();
        assert_eq!(has_env.entries.len(), 1);
        assert_eq!(has_env.entries[0].body, "a");

        // attr_matches: term-search the `http.route` value.
        let checkout = db
            .logs()
            .query(LogQuery::new().attr_matches("http.route", "checkout"))
            .await
            .unwrap();
        assert_eq!(checkout.entries.len(), 1);
        assert_eq!(checkout.entries[0].body, "b");

        // attr_in: keep only rows whose route is in the given set (/cart or /pay, not /checkout).
        let in_set = db
            .logs()
            .query(LogQuery::new().attr_in("http.route", &["/cart", "/pay"]))
            .await
            .unwrap();
        let mut bodies: Vec<&str> = in_set.entries.iter().map(|e| e.body.as_str()).collect();
        bodies.sort_unstable();
        assert_eq!(bodies, vec!["a", "c"]);

        // attr_in with an empty set matches nothing.
        let none = db
            .logs()
            .query(LogQuery::new().attr_in("http.route", &[]))
            .await
            .unwrap();
        assert!(none.entries.is_empty());

        // attr_not_in: exclude /checkout — keeps /cart and /pay, AND the row missing http.route
        // ("a" has http.route=/cart; every row here has the key, so exclude /checkout → a, c).
        let excluded = db
            .logs()
            .query(LogQuery::new().attr_not_in("http.route", &["/checkout"]))
            .await
            .unwrap();
        let mut nbodies: Vec<&str> = excluded.entries.iter().map(|e| e.body.as_str()).collect();
        nbodies.sort_unstable();
        assert_eq!(nbodies, vec!["a", "c"]);

        // NULL-aware: exclude env=prod. Only "a" has env (=prod, excluded); "b"/"c" lack env and
        // are KEPT (their env is not in the excluded set).
        let no_prod = db
            .logs()
            .query(LogQuery::new().attr_not_in("env", &["prod"]))
            .await
            .unwrap();
        let mut kept: Vec<&str> = no_prod.entries.iter().map(|e| e.body.as_str()).collect();
        kept.sort_unstable();
        assert_eq!(kept, vec!["b", "c"], "rows missing the key are kept");

        // count() = total matching, ignoring limit/paging.
        assert_eq!(db.logs().count(LogQuery::new()).await.unwrap(), 3);
        assert_eq!(
            db.logs()
                .count(LogQuery::new().attr_in("http.route", &["/cart", "/pay"]))
                .await
                .unwrap(),
            2
        );
        // A limit on the filter does not cap the count.
        assert_eq!(db.logs().count(LogQuery::new().limit(1)).await.unwrap(), 3);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn logs_numeric_attr_filter() {
        let db = Db::in_memory().open().unwrap();
        for (body, status) in [("a", "200"), ("b", "500"), ("c", "503"), ("d", "notnum")] {
            db.ingest_otlp_logs(&otlp_rich("s", body, 1, 9, &[("status", status)]))
                .await
                .unwrap();
        }
        db.ingest_otlp_logs(&otlp_rich("s", "e", 1, 9, &[])) // no status attribute
            .await
            .unwrap();
        let count = |q: LogQuery| {
            let db = db.clone();
            async move { db.logs().count(q).await.unwrap() }
        };

        // >= 500 keeps b, c; the 200, the non-numeric "notnum", and the missing-key row are excluded.
        assert_eq!(count(LogQuery::new().attr_ge("status", 500.0)).await, 2);
        // A numeric range (500..=502) keeps only b (500), not c (503).
        assert_eq!(
            count(
                LogQuery::new()
                    .attr_ge("status", 500.0)
                    .attr_le("status", 502.0)
            )
            .await,
            1
        );
        assert_eq!(count(LogQuery::new().attr_gt("status", 200.0)).await, 2); // b, c
        assert_eq!(count(LogQuery::new().attr_lt("status", 300.0)).await, 1); // a

        // Regex on the same attribute: `^5` matches 500/503; `^[0-9]+$` matches the three numerics
        // (not "notnum"); the missing-key row is excluded in both.
        assert_eq!(count(LogQuery::new().attr_regex("status", "^5")).await, 2);
        assert_eq!(
            count(LogQuery::new().attr_regex("status", "^[0-9]+$")).await,
            3
        );
        assert_eq!(count(LogQuery::new().attr_regex("status", "num")).await, 1); // "notnum"
    }

    /// Regression: numeric matchers must match an *integer-typed* OTLP attribute, not only a number
    /// that arrived as a string. OTLP `IntValue`/`DoubleValue` attributes are stored as bare JSON
    /// numbers (`{"http.status_code":500}`), which the old `TRY_CAST(json_get_str(...) AS DOUBLE)`
    /// path could not read (`json_get_str` returns NULL for a non-string scalar) — so `attr_ge` etc.
    /// silently dropped them. The `json_get_num` UDF fixes it. This test ingests genuinely int- and
    /// double-typed attributes (not string literals) to exercise that path.
    #[tokio::test(flavor = "current_thread")]
    async fn numeric_matchers_match_typed_numeric_attributes() {
        use opentelemetry_proto::tonic::collector::logs::v1::ExportLogsServiceRequest;
        use opentelemetry_proto::tonic::common::v1::{AnyValue as PbAny, KeyValue, any_value};
        use opentelemetry_proto::tonic::logs::v1::{LogRecord, ResourceLogs, ScopeLogs};
        use opentelemetry_proto::tonic::resource::v1::Resource;
        use prost::Message;

        // One log with an integer-typed `http.status_code` and a double-typed `ratio` attribute.
        let otlp_typed = |status: i64, ratio: f64, time: u64| -> Vec<u8> {
            ExportLogsServiceRequest {
                resource_logs: vec![ResourceLogs {
                    resource: Some(Resource {
                        attributes: vec![KeyValue {
                            key: "service.name".to_owned(),
                            value: Some(PbAny {
                                value: Some(any_value::Value::StringValue("svc".to_owned())),
                            }),
                            ..Default::default()
                        }],
                        ..Default::default()
                    }),
                    scope_logs: vec![ScopeLogs {
                        log_records: vec![LogRecord {
                            time_unix_nano: time,
                            severity_number: 9,
                            attributes: vec![
                                KeyValue {
                                    key: "http.status_code".to_owned(),
                                    value: Some(PbAny {
                                        value: Some(any_value::Value::IntValue(status)),
                                    }),
                                    ..Default::default()
                                },
                                KeyValue {
                                    key: "ratio".to_owned(),
                                    value: Some(PbAny {
                                        value: Some(any_value::Value::DoubleValue(ratio)),
                                    }),
                                    ..Default::default()
                                },
                            ],
                            ..Default::default()
                        }],
                        ..Default::default()
                    }],
                    ..Default::default()
                }],
            }
            .encode_to_vec()
        };

        let db = Db::in_memory().open().unwrap();
        for (status, ratio, t) in [(200i64, 0.1f64, 1u64), (500, 0.9, 2), (503, 0.95, 3)] {
            db.ingest_otlp_logs(&otlp_typed(status, ratio, t))
                .await
                .unwrap();
        }
        let count = |q: LogQuery| {
            let db = db.clone();
            async move { db.logs().count(q).await.unwrap() }
        };

        // Integer-typed attribute: >= 500 keeps the 500 and 503 rows (0 before the fix).
        assert_eq!(
            count(LogQuery::new().attr_ge("http.status_code", 500.0)).await,
            2
        );
        assert_eq!(
            count(LogQuery::new().attr_lt("http.status_code", 500.0)).await,
            1
        );
        assert_eq!(
            count(
                LogQuery::new()
                    .attr_ge("http.status_code", 500.0)
                    .attr_le("http.status_code", 500.0)
            )
            .await,
            1
        );
        // Double-typed attribute: > 0.9 keeps only the 0.95 row.
        assert_eq!(count(LogQuery::new().attr_gt("ratio", 0.9)).await, 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn typed_logs_query_api() {
        let db = Db::in_memory().open().unwrap();
        db.ingest_otlp_logs(&otlp_rich(
            "cart",
            "connection error timeout",
            10,
            17,
            &[("http.route", "/cart")],
        ))
        .await
        .unwrap();
        db.ingest_otlp_logs(&otlp_rich(
            "cart",
            "request ok",
            20,
            9,
            &[("http.route", "/cart")],
        ))
        .await
        .unwrap();
        db.ingest_otlp_logs(&otlp_rich(
            "checkout",
            "request ok",
            30,
            9,
            &[("http.route", "/pay")],
        ))
        .await
        .unwrap();

        // Filter by service; Backward (default) = newest first; limit default 100.
        let page = db
            .logs()
            .query(LogQuery::new().service("cart"))
            .await
            .unwrap();
        assert_eq!(page.entries.len(), 2);
        assert_eq!(page.entries[0].time, Timestamp(20));
        assert_eq!(page.entries[0].service.as_deref(), Some("cart"));
        assert_eq!(page.stats.rows_returned, 2);

        // Full-text.
        let errs = db
            .logs()
            .query(LogQuery::new().matches("error"))
            .await
            .unwrap();
        assert_eq!(errs.entries.len(), 1);
        assert_eq!(errs.entries[0].body, "connection error timeout");
        // In-memory: the rows are in the buffer with no `.tidx`, so the full-text match runs through
        // the row-wise `matches` UDF, not the Tantivy index. `used_index` reflects real consultation.
        assert!(!errs.stats.used_index);
        assert!(
            errs.stats.rows_scanned > 0,
            "buffer rows were materialized and scanned"
        );

        // Severity threshold.
        let sev = db
            .logs()
            .query(LogQuery::new().severity_at_least(SeverityNumber::ERROR))
            .await
            .unwrap();
        assert_eq!(sev.entries.len(), 1);
        assert_eq!(sev.entries[0].severity_number, SeverityNumber(17));

        // Attribute equality + materialized attributes.
        let pay = db
            .logs()
            .query(LogQuery::new().attr_eq("http.route", "/pay"))
            .await
            .unwrap();
        assert_eq!(pay.entries.len(), 1);
        assert_eq!(
            pay.entries[0].attributes.get_str("http.route"),
            Some("/pay")
        );

        // Forward direction = oldest first.
        let fwd = db
            .logs()
            .query(
                LogQuery::new()
                    .service("cart")
                    .direction(Direction::Forward),
            )
            .await
            .unwrap();
        assert_eq!(fwd.entries[0].time, Timestamp(10));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn attribute_discovery_and_volume() {
        let db = Db::in_memory().open().unwrap();
        db.ingest_otlp_logs(&otlp_rich(
            "cart",
            "a",
            1000,
            9,
            &[("http.route", "/cart"), ("env", "prod")],
        ))
        .await
        .unwrap();
        db.ingest_otlp_logs(&otlp_rich(
            "checkout",
            "b",
            2000,
            9,
            &[("http.route", "/pay")],
        ))
        .await
        .unwrap();

        // names() = attribute keys ∪ service.name.
        let names = db.attrs().names().await.unwrap();
        assert!(names.contains(&"http.route".to_owned()));
        assert!(names.contains(&"env".to_owned()));
        assert!(names.contains(&"service.name".to_owned()));

        // values() for an attribute key and for the promoted service column.
        assert_eq!(
            db.attrs().values("http.route").await.unwrap(),
            vec!["/cart".to_owned(), "/pay".to_owned()]
        );
        assert_eq!(
            db.attrs().values("service.name").await.unwrap(),
            vec!["cart".to_owned(), "checkout".to_owned()]
        );

        // volume: 2 records at t=1000 and t=2000 ns, step 1000 ns → two buckets of 1.
        let vol = db
            .logs()
            .volume(LogQuery::new(), std::time::Duration::from_nanos(1000))
            .await
            .unwrap();
        assert_eq!(vol.len(), 2);
        assert_eq!(vol[0].time, Timestamp(1000));
        assert_eq!(vol[0].count, 1);
        assert_eq!(vol[1].time, Timestamp(2000));
        assert_eq!(vol[1].count, 1);
        assert!(vol[0].labels.is_empty(), "ungrouped volume has no labels");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn attribute_discovery_is_cross_signal() {
        // A key on each signal, and a distinct service per signal — discovery must union all three.
        let db = Db::in_memory().open().unwrap();
        db.ingest_otlp_logs(&otlp_rich("logsvc", "a", 1, 9, &[("log.only", "L")]))
            .await
            .unwrap();
        db.ingest_otlp_traces(&otlp_span_attr("tracesvc", "op", &[("span.only", "S")]))
            .await
            .unwrap();
        db.ingest_otlp_metrics(&otlp_gauge_labeled("metricsvc", "g", "metric.only", &["M"]))
            .await
            .unwrap();

        // names() unions attribute keys from logs + spans + metrics, plus service.name.
        let names = db.attrs().names().await.unwrap();
        for expected in ["log.only", "span.only", "metric.only", "service.name"] {
            assert!(
                names.contains(&expected.to_owned()),
                "names() missing {expected}"
            );
        }

        // values() reaches each signal's own key…
        assert_eq!(
            db.attrs().values("span.only").await.unwrap(),
            vec!["S".to_owned()]
        );
        assert_eq!(
            db.attrs().values("metric.only").await.unwrap(),
            vec!["M".to_owned()]
        );
        // …and service.name unions the promoted service column across all three signals.
        assert_eq!(
            db.attrs().values("service.name").await.unwrap(),
            vec![
                "logsvc".to_owned(),
                "metricsvc".to_owned(),
                "tracesvc".to_owned()
            ]
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn logs_volume_by_group() {
        let db = Db::in_memory().open().unwrap();
        // Three logs in one 1000ns bucket: route=/a twice, route=/b once.
        db.ingest_otlp_logs(&otlp_rich("cart", "x", 1, 9, &[("route", "/a")]))
            .await
            .unwrap();
        db.ingest_otlp_logs(&otlp_rich("cart", "y", 2, 9, &[("route", "/a")]))
            .await
            .unwrap();
        db.ingest_otlp_logs(&otlp_rich("cart", "z", 3, 9, &[("route", "/b")]))
            .await
            .unwrap();

        let buckets = db
            .logs()
            .volume_by(
                LogQuery::new(),
                std::time::Duration::from_nanos(1000),
                &["route"],
            )
            .await
            .unwrap();
        assert_eq!(buckets.len(), 2, "one bucket, two label sets");
        let a = buckets
            .iter()
            .find(|b| b.labels == vec![("route".to_owned(), "/a".to_owned())])
            .expect("route=/a bucket");
        assert_eq!(a.count, 2);
        let b = buckets
            .iter()
            .find(|b| b.labels == vec![("route".to_owned(), "/b".to_owned())])
            .expect("route=/b bucket");
        assert_eq!(b.count, 1);
    }

    /// `service.name` is a resource attribute lifted into the built-in `service` column, never a
    /// record `attributes` entry, so grouping by it used to resolve through
    /// `json_get_str(attributes, 'service.name')` — NULL on every row, collapsing the breakdown into
    /// one `{"service.name": ""}` series with the counts merged. `SqlParams::attr_field` resolves
    /// both spellings to the column now; this pins the split (and the equivalent filter).
    #[tokio::test(flavor = "current_thread")]
    async fn logs_group_and_filter_by_service_name() {
        let db = Db::in_memory().open().unwrap();
        db.ingest_otlp_logs(&otlp_rich("cart", "x", 1, 9, &[]))
            .await
            .unwrap();
        db.ingest_otlp_logs(&otlp_rich("cart", "y", 2, 9, &[]))
            .await
            .unwrap();
        db.ingest_otlp_logs(&otlp_rich("checkout", "z", 3, 9, &[]))
            .await
            .unwrap();

        // Both spellings group the same way: the OTel key and the column name.
        for key in ["service.name", "service"] {
            let buckets = db
                .logs()
                .volume_by(
                    LogQuery::new(),
                    std::time::Duration::from_nanos(1000),
                    &[key],
                )
                .await
                .unwrap();
            let mut counts: Vec<(String, u64)> = buckets
                .iter()
                .map(|b| {
                    let (k, v) = &b.labels[0];
                    assert_eq!(k, key);
                    (v.clone(), b.count)
                })
                .collect();
            counts.sort();
            assert_eq!(
                counts,
                vec![("cart".to_owned(), 2), ("checkout".to_owned(), 1)],
                "grouping by {key}"
            );
        }

        // The same key as an attribute *filter* agrees with the breakdown.
        let page = db
            .logs()
            .query(LogQuery::new().attr_eq("service.name", "cart"))
            .await
            .unwrap();
        assert_eq!(page.entries.len(), 2);
        assert!(
            page.entries
                .iter()
                .all(|e| e.service.as_deref() == Some("cart"))
        );

        // And `attr_exists` sees it, rather than treating every row as missing the key.
        let page = db
            .logs()
            .query(LogQuery::new().attr_exists("service.name"))
            .await
            .unwrap();
        assert_eq!(page.entries.len(), 3);
    }

    /// Build a one-span OTLP/traces body.
    fn otlp_trace(
        service: &str,
        name: &str,
        kind: i32,
        start: u64,
        end: u64,
        status: i32,
    ) -> Vec<u8> {
        use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
        use opentelemetry_proto::tonic::common::v1::{AnyValue as PbAny, KeyValue, any_value};
        use opentelemetry_proto::tonic::resource::v1::Resource;
        use opentelemetry_proto::tonic::trace::v1::{ResourceSpans, ScopeSpans, Span, Status};
        use prost::Message;

        let sv = |s: &str| PbAny {
            value: Some(any_value::Value::StringValue(s.to_owned())),
        };
        ExportTraceServiceRequest {
            resource_spans: vec![ResourceSpans {
                resource: Some(Resource {
                    attributes: vec![KeyValue {
                        key: "service.name".to_owned(),
                        value: Some(sv(service)),
                        ..Default::default()
                    }],
                    ..Default::default()
                }),
                scope_spans: vec![ScopeSpans {
                    spans: vec![Span {
                        trace_id: vec![0xaa; 16],
                        span_id: vec![0x01; 8],
                        name: name.to_owned(),
                        kind,
                        start_time_unix_nano: start,
                        end_time_unix_nano: end,
                        status: Some(Status {
                            code: status,
                            message: String::new(),
                        }),
                        ..Default::default()
                    }],
                    ..Default::default()
                }],
                ..Default::default()
            }],
        }
        .encode_to_vec()
    }

    #[tokio::test(flavor = "current_thread")]
    async fn traces_ingest_seal_query_recover() {
        let dir = tempfile::tempdir().unwrap();
        {
            let db = Db::builder(dir.path()).open().unwrap();
            db.ingest_otlp_traces(&otlp_trace("checkout", "GET /cart", 2, 1000, 1500, 2))
                .await
                .unwrap();
            db.ingest_otlp_traces(&otlp_trace("cart", "internal", 1, 2000, 2100, 0))
                .await
                .unwrap();

            // Buffer-only query over the new `spans` table.
            assert_eq!(count(&db, "SELECT count(*) AS c FROM spans").await, 2);
            // Typed columns decoded correctly.
            assert_eq!(
                count(&db, "SELECT count(*) AS c FROM spans WHERE kind = 'SERVER'").await,
                1
            );
            assert_eq!(
                count(
                    &db,
                    "SELECT count(*) AS c FROM spans WHERE status_code = 'ERROR'"
                )
                .await,
                1
            );
            assert_eq!(
                count(
                    &db,
                    "SELECT count(*) AS c FROM spans WHERE duration_ns = 500"
                )
                .await,
                1
            );
            assert_eq!(
                count(
                    &db,
                    "SELECT count(*) AS c FROM spans WHERE name = 'GET /cart'"
                )
                .await,
                1
            );
            // The logs table coexists (and is empty).
            assert_eq!(count(&db, "SELECT count(*) AS c FROM logs").await, 0);

            db.flush().await.unwrap(); // seal spans → Parquet segment
        }
        // Reopen: spans recovered from the sealed segment via the manifest.
        let db2 = Db::builder(dir.path()).open().unwrap();
        assert_eq!(count(&db2, "SELECT count(*) AS c FROM spans").await, 2);
        assert_eq!(
            count(
                &db2,
                "SELECT count(*) AS c FROM spans WHERE service = 'cart'"
            )
            .await,
            1
        );
    }

    /// Build an OTLP/traces body: a root span + one child, sharing `trace_id`.
    fn otlp_trace_tree(service: &str, trace_id: [u8; 16]) -> Vec<u8> {
        use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
        use opentelemetry_proto::tonic::common::v1::{AnyValue as PbAny, KeyValue, any_value};
        use opentelemetry_proto::tonic::resource::v1::Resource;
        use opentelemetry_proto::tonic::trace::v1::{ResourceSpans, ScopeSpans, Span, Status};
        use prost::Message;

        let sv = |s: &str| PbAny {
            value: Some(any_value::Value::StringValue(s.to_owned())),
        };
        let span = |span_id: Vec<u8>,
                    parent: Vec<u8>,
                    name: &str,
                    kind: i32,
                    start: u64,
                    end: u64,
                    status: i32| Span {
            trace_id: trace_id.to_vec(),
            span_id,
            parent_span_id: parent,
            name: name.to_owned(),
            kind,
            start_time_unix_nano: start,
            end_time_unix_nano: end,
            status: Some(Status {
                code: status,
                message: String::new(),
            }),
            ..Default::default()
        };
        ExportTraceServiceRequest {
            resource_spans: vec![ResourceSpans {
                resource: Some(Resource {
                    attributes: vec![KeyValue {
                        key: "service.name".to_owned(),
                        value: Some(sv(service)),
                        ..Default::default()
                    }],
                    ..Default::default()
                }),
                scope_spans: vec![ScopeSpans {
                    spans: vec![
                        span(vec![1; 8], vec![], "GET /cart", 2, 1000, 1500, 2), // root, ERROR
                        span(vec![2; 8], vec![1; 8], "db query", 3, 1100, 1300, 1), // child, OK
                    ],
                    ..Default::default()
                }],
                ..Default::default()
            }],
        }
        .encode_to_vec()
    }

    #[tokio::test(flavor = "current_thread")]
    async fn span_red_metrics() {
        let db = Db::in_memory().open().unwrap();
        // Three SERVER spans for "checkout", durations 100/200/300 ns, one ERROR — all at t=1000.
        db.ingest_otlp_traces(&otlp_trace("checkout", "GET /x", 2, 1000, 1100, 0))
            .await
            .unwrap();
        db.ingest_otlp_traces(&otlp_trace("checkout", "GET /x", 2, 1000, 1200, 0))
            .await
            .unwrap();
        db.ingest_otlp_traces(&otlp_trace("checkout", "GET /x", 2, 1000, 1300, 2))
            .await
            .unwrap();

        let m = db
            .traces()
            .span_metrics(
                SpanMetricsQuery::new()
                    .service("checkout")
                    .step(std::time::Duration::from_nanos(10_000)),
            )
            .await
            .unwrap();
        assert_eq!(m.0.len(), 1); // one (unlabeled) series
        let p = &m.0[0].points[0];
        assert_eq!(p.calls, 3);
        assert_eq!(p.errors, 1);
        assert!((p.error_rate - 1.0 / 3.0).abs() < 1e-9);
        assert!(p.p50_ns >= 100.0 && p.p50_ns <= 300.0, "p50={}", p.p50_ns);
        assert!(p.p99_ns >= p.p50_ns);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn traces_get_and_search() {
        let db = Db::in_memory().open().unwrap();
        let tid = [0xab; 16];
        db.ingest_otlp_traces(&otlp_trace_tree("checkout", tid))
            .await
            .unwrap();
        db.ingest_otlp_traces(&otlp_trace("cart", "internal", 1, 5000, 5100, 0))
            .await
            .unwrap();

        // get() assembles the span tree.
        let trace = db.traces().get(TraceId(tid)).await.unwrap().unwrap();
        assert_eq!(trace.spans.len(), 2);
        assert_eq!(trace.root_service.as_deref(), Some("checkout"));
        assert_eq!(trace.root_name.as_deref(), Some("GET /cart"));
        assert_eq!(trace.duration_ns.0, 500);
        assert!(
            db.traces()
                .get(TraceId([0x00; 16]))
                .await
                .unwrap()
                .is_none()
        );

        // search() by service returns a summary.
        let results = db
            .traces()
            .search(TraceQuery::new().service("checkout"))
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].trace_id, TraceId(tid));
        assert_eq!(results[0].span_count, 2);
        assert!(results[0].error);
        assert_eq!(results[0].root_name.as_deref(), Some("GET /cart"));

        // search() by min-duration keeps the slow trace, drops the 100ns unrelated one.
        let slow = db
            .traces()
            .search(TraceQuery::new().min_duration(std::time::Duration::from_nanos(400)))
            .await
            .unwrap();
        assert_eq!(slow.len(), 1);
        assert_eq!(slow[0].trace_id, TraceId(tid));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn traces_get_prunes_across_sealed_segments() {
        // On disk so each trace seals into its own Parquet span segment, each carrying bloom filters
        // on the id columns. get(A) must return only A's spans though B lives in a different segment
        // — correctness across the bloom-pruned segment boundary (ARCHITECTURE.md §8). The read-side
        // pruning itself is asserted in imbh-query's provider unit test; here we assert end-to-end
        // that pruning never drops or leaks a row across segments.
        let dir = tempfile::tempdir().unwrap();
        let db = Db::builder(dir.path()).open().unwrap();
        let a = [0x1a; 16];
        let b = [0x2b; 16];
        db.ingest_otlp_traces(&otlp_trace_tree("alpha", a))
            .await
            .unwrap();
        db.flush().await.unwrap(); // seal → segment 1 (trace A)
        db.ingest_otlp_traces(&otlp_trace_tree("beta", b))
            .await
            .unwrap();
        db.flush().await.unwrap(); // seal → segment 2 (trace B)

        let ta = db.traces().get(TraceId(a)).await.unwrap().unwrap();
        assert_eq!(ta.spans.len(), 2);
        assert!(
            ta.spans.iter().all(|s| s.trace_id == TraceId(a)),
            "no span from trace B leaked into trace A"
        );
        assert_eq!(ta.root_service.as_deref(), Some("alpha"));

        let tb = db.traces().get(TraceId(b)).await.unwrap().unwrap();
        assert_eq!(tb.spans.len(), 2);
        assert!(tb.spans.iter().all(|s| s.trace_id == TraceId(b)));
        assert_eq!(tb.root_service.as_deref(), Some("beta"));

        // An id present in neither segment → None (both segments are bloom-pruned; still correct).
        assert!(
            db.traces()
                .get(TraceId([0x00; 16]))
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn traces_search_by_name_hits_sealed_index() {
        // On disk so spans seal to a Parquet segment + `.tidx` (the span-name index). Searching by
        // name then drives the RowSelection bridge over that sidecar.
        let dir = tempfile::tempdir().unwrap();
        let db = Db::builder(dir.path()).open().unwrap();
        let tid = [0xcd; 16];
        db.ingest_otlp_traces(&otlp_trace_tree("checkout", tid))
            .await
            .unwrap();
        db.flush().await.unwrap(); // seal spans → build the `.tidx` over span names

        // A term present in a span name ("GET /cart") → the trace is found through the sealed index.
        let hit = db
            .traces()
            .search(TraceQuery::new().matches("cart"))
            .await
            .unwrap();
        assert_eq!(
            hit.len(),
            1,
            "the sealed span index found the trace by name"
        );
        assert_eq!(hit[0].trace_id, TraceId(tid));

        // A term absent from every span name → no trace (index miss and the re-checked UDF agree).
        let miss = db
            .traces()
            .search(TraceQuery::new().matches("zzznomatch"))
            .await
            .unwrap();
        assert!(miss.is_empty(), "no span name contains the term");

        // Compaction merges span segments and rebuilds the `.tidx`; name search still works.
        db.ingest_otlp_traces(&otlp_trace_tree("checkout", [0xef; 16]))
            .await
            .unwrap();
        db.flush().await.unwrap(); // a second span segment in the same day-partition
        db.compact().await.unwrap(); // merge the two → rebuilt span index
        let after = db
            .traces()
            .search(TraceQuery::new().matches("cart"))
            .await
            .unwrap();
        assert_eq!(
            after.len(),
            2,
            "both traces found after span-segment compaction"
        );
    }

    /// Build a one-span OTLP trace whose span carries the given data-point attributes.
    fn otlp_span_attr(service: &str, name: &str, attrs: &[(&str, &str)]) -> Vec<u8> {
        use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
        use opentelemetry_proto::tonic::common::v1::{AnyValue as PbAny, KeyValue, any_value};
        use opentelemetry_proto::tonic::resource::v1::Resource;
        use opentelemetry_proto::tonic::trace::v1::{ResourceSpans, ScopeSpans, Span};
        use prost::Message;

        let sv = |s: &str| PbAny {
            value: Some(any_value::Value::StringValue(s.to_owned())),
        };
        let kv = |k: &str, v: &str| KeyValue {
            key: k.to_owned(),
            value: Some(sv(v)),
            ..Default::default()
        };
        ExportTraceServiceRequest {
            resource_spans: vec![ResourceSpans {
                resource: Some(Resource {
                    attributes: vec![kv("service.name", service)],
                    ..Default::default()
                }),
                scope_spans: vec![ScopeSpans {
                    spans: vec![Span {
                        trace_id: vec![0xaa; 16],
                        span_id: vec![1; 8],
                        name: name.to_owned(),
                        kind: 2,
                        start_time_unix_nano: 1000,
                        end_time_unix_nano: 1500,
                        attributes: attrs.iter().map(|(k, v)| kv(k, v)).collect(),
                        ..Default::default()
                    }],
                    ..Default::default()
                }],
                ..Default::default()
            }],
        }
        .encode_to_vec()
    }

    #[tokio::test(flavor = "current_thread")]
    async fn traces_attr_matchers() {
        let db = Db::in_memory().open().unwrap();
        db.ingest_otlp_traces(&otlp_span_attr(
            "cart",
            "GET /x",
            &[
                ("http.route", "/checkout"),
                ("env", "prod"),
                ("size", "1500"),
            ],
        ))
        .await
        .unwrap();

        // attr_exists
        assert_eq!(
            db.traces()
                .search(TraceQuery::new().attr_exists("env"))
                .await
                .unwrap()
                .len(),
            1
        );
        assert!(
            db.traces()
                .search(TraceQuery::new().attr_exists("missing"))
                .await
                .unwrap()
                .is_empty()
        );
        // attr_matches (term-search the attribute value)
        assert_eq!(
            db.traces()
                .search(TraceQuery::new().attr_matches("http.route", "checkout"))
                .await
                .unwrap()
                .len(),
            1
        );
        assert!(
            db.traces()
                .search(TraceQuery::new().attr_matches("http.route", "cart"))
                .await
                .unwrap()
                .is_empty()
        );
        // attr_in: the route is in the given set → matched; a disjoint set → empty.
        assert_eq!(
            db.traces()
                .search(TraceQuery::new().attr_in("http.route", &["/checkout", "/pay"]))
                .await
                .unwrap()
                .len(),
            1
        );
        assert!(
            db.traces()
                .search(TraceQuery::new().attr_in("http.route", &["/cart", "/home"]))
                .await
                .unwrap()
                .is_empty()
        );
        // attr_not_in: excluding /checkout drops the only trace; excluding an unrelated value keeps it.
        assert!(
            db.traces()
                .search(TraceQuery::new().attr_not_in("http.route", &["/checkout"]))
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            db.traces()
                .search(TraceQuery::new().attr_not_in("http.route", &["/other"]))
                .await
                .unwrap()
                .len(),
            1
        );
        // Numeric attr filter: size (1500) is > 1000 but not > 2000.
        assert_eq!(
            db.traces()
                .search(TraceQuery::new().attr_gt("size", 1000.0))
                .await
                .unwrap()
                .len(),
            1
        );
        assert!(
            db.traces()
                .search(TraceQuery::new().attr_gt("size", 2000.0))
                .await
                .unwrap()
                .is_empty()
        );
        // Regex on a span attribute: /checkout matches `^/check`, not `^/cart`.
        assert_eq!(
            db.traces()
                .search(TraceQuery::new().attr_regex("http.route", "^/check"))
                .await
                .unwrap()
                .len(),
            1
        );
        assert!(
            db.traces()
                .search(TraceQuery::new().attr_regex("http.route", "^/cart"))
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn traces_search_by_name_matches() {
        let db = Db::in_memory().open().unwrap();
        // One trace with two differently-named spans.
        db.ingest_otlp_traces(&otlp_trace("cart", "GET /checkout", 2, 1000, 1500, 0))
            .await
            .unwrap();
        db.ingest_otlp_traces(&otlp_trace("cart", "db query", 3, 1100, 1300, 1))
            .await
            .unwrap();

        // `.matches()` term-searches span names: "checkout" hits the trace, an absent term misses.
        let hit = db
            .traces()
            .search(TraceQuery::new().matches("checkout"))
            .await
            .unwrap();
        assert_eq!(hit.len(), 1, "a span named 'GET /checkout' matches");
        let miss = db
            .traces()
            .search(TraceQuery::new().matches("nonexistent"))
            .await
            .unwrap();
        assert!(miss.is_empty(), "no span name contains the term");
    }

    /// Build an OTLP/metrics body: one gauge (`cpu`) and one cumulative sum (`requests`).
    fn otlp_metrics(service: &str) -> Vec<u8> {
        use opentelemetry_proto::tonic::collector::metrics::v1::ExportMetricsServiceRequest;
        use opentelemetry_proto::tonic::common::v1::{AnyValue as PbAny, KeyValue, any_value};
        use opentelemetry_proto::tonic::metrics::v1::{
            Gauge, Metric, NumberDataPoint, ResourceMetrics, ScopeMetrics, Sum, metric,
            number_data_point,
        };
        use opentelemetry_proto::tonic::resource::v1::Resource;
        use prost::Message;

        let sv = |s: &str| PbAny {
            value: Some(any_value::Value::StringValue(s.to_owned())),
        };
        let dp = |v: f64| NumberDataPoint {
            time_unix_nano: 100,
            value: Some(number_data_point::Value::AsDouble(v)),
            ..Default::default()
        };
        ExportMetricsServiceRequest {
            resource_metrics: vec![ResourceMetrics {
                resource: Some(Resource {
                    attributes: vec![KeyValue {
                        key: "service.name".to_owned(),
                        value: Some(sv(service)),
                        ..Default::default()
                    }],
                    ..Default::default()
                }),
                scope_metrics: vec![ScopeMetrics {
                    metrics: vec![
                        Metric {
                            name: "cpu".to_owned(),
                            unit: "1".to_owned(),
                            data: Some(metric::Data::Gauge(Gauge {
                                data_points: vec![dp(0.5)],
                            })),
                            ..Default::default()
                        },
                        Metric {
                            name: "requests".to_owned(),
                            unit: "1".to_owned(),
                            data: Some(metric::Data::Sum(Sum {
                                data_points: vec![dp(42.0)],
                                aggregation_temporality: 2,
                                is_monotonic: true,
                            })),
                            ..Default::default()
                        },
                    ],
                    ..Default::default()
                }],
                ..Default::default()
            }],
        }
        .encode_to_vec()
    }

    /// Build an OTLP Gauge with one data point per label value, each carrying attribute `{key: val}`.
    #[tokio::test(flavor = "current_thread")]
    async fn metrics_exemplars_round_trip() {
        use opentelemetry_proto::tonic::collector::metrics::v1::ExportMetricsServiceRequest;
        use opentelemetry_proto::tonic::metrics::v1::{
            Exemplar, Gauge, Histogram, HistogramDataPoint, Metric, NumberDataPoint,
            ResourceMetrics, ScopeMetrics, exemplar, metric, number_data_point,
        };
        use prost::Message;

        let db = Db::in_memory().open().unwrap();
        let tid = [0xab_u8; 16];
        let sid = [0xcd_u8; 8];
        let exemplar = || Exemplar {
            time_unix_nano: 900,
            value: Some(exemplar::Value::AsDouble(41.5)),
            trace_id: tid.to_vec(),
            span_id: sid.to_vec(),
            filtered_attributes: vec![opentelemetry_proto::tonic::common::v1::KeyValue {
                key: "sampler".to_owned(),
                value: Some(opentelemetry_proto::tonic::common::v1::AnyValue {
                    value: Some(
                        opentelemetry_proto::tonic::common::v1::any_value::Value::StringValue(
                            "always_on".to_owned(),
                        ),
                    ),
                }),
                ..Default::default()
            }],
        };
        let dp = NumberDataPoint {
            time_unix_nano: 1000,
            value: Some(number_data_point::Value::AsDouble(42.0)),
            exemplars: vec![exemplar()],
            ..Default::default()
        };
        let hdp = HistogramDataPoint {
            time_unix_nano: 1000,
            count: 3,
            explicit_bounds: vec![1.0, 5.0],
            bucket_counts: vec![1, 1, 1],
            exemplars: vec![exemplar()],
            ..Default::default()
        };
        let req = ExportMetricsServiceRequest {
            resource_metrics: vec![ResourceMetrics {
                scope_metrics: vec![ScopeMetrics {
                    metrics: vec![
                        Metric {
                            name: "cpu".to_owned(),
                            data: Some(metric::Data::Gauge(Gauge {
                                data_points: vec![dp],
                            })),
                            ..Default::default()
                        },
                        Metric {
                            name: "lat".to_owned(),
                            data: Some(metric::Data::Histogram(Histogram {
                                data_points: vec![hdp],
                                aggregation_temporality: 2,
                            })),
                            ..Default::default()
                        },
                    ],
                    ..Default::default()
                }],
                ..Default::default()
            }],
        };
        db.ingest_otlp_metrics(&req.encode_to_vec()).await.unwrap();

        // Exemplars round-trip on both a scalar table and the histogram table.
        for (table, metric_name) in [("metrics_gauge", "cpu"), ("metrics_histogram", "lat")] {
            let batches = db
                .sql(&format!(
                    "SELECT exemplars FROM {table} WHERE metric = '{metric_name}'"
                ))
                .collect()
                .await
                .unwrap();
            let ex = crate::logs::get_str(batches[0].column(0).as_ref(), 0).unwrap();
            assert!(
                ex.contains(&"ab".repeat(16)),
                "{table}: trace_id hex present: {ex}"
            );
            assert!(
                ex.contains(&"cd".repeat(8)),
                "{table}: span_id hex present: {ex}"
            );
            assert!(
                ex.contains("\"value\":41.5"),
                "{table}: exemplar value present: {ex}"
            );
            assert!(
                ex.contains("\"attributes\":{\"sampler\":\"always_on\"}"),
                "{table}: exemplar filtered_attributes present: {ex}"
            );
        }

        // Typed accessor: parse the stored exemplars back into DTOs (the trace-drill-down API).
        let exs = db.metrics().exemplars("cpu").await.unwrap();
        assert_eq!(exs.len(), 1);
        let e = &exs[0];
        assert_eq!(e.trace_id, Some(TraceId(tid)));
        assert_eq!(e.span_id, Some(SpanId(sid)));
        assert_eq!(e.value, 41.5);
        assert_eq!(e.time, Timestamp(900));
        assert_eq!(e.attributes, "{\"sampler\":\"always_on\"}");

        // A point with no exemplars stores "[]" (valid empty JSON, not "") and yields no DTOs.
        db.ingest_otlp_metrics(&otlp_gauge_labeled("s", "noex", "k", &["v"]))
            .await
            .unwrap();
        let empty = db
            .sql("SELECT exemplars FROM metrics_gauge WHERE metric = 'noex'")
            .collect()
            .await
            .unwrap();
        assert_eq!(
            crate::logs::get_str(empty[0].column(0).as_ref(), 0).unwrap(),
            "[]"
        );
        assert!(db.metrics().exemplars("noex").await.unwrap().is_empty());
    }

    fn otlp_gauge_labeled(service: &str, metric: &str, key: &str, vals: &[&str]) -> Vec<u8> {
        use opentelemetry_proto::tonic::collector::metrics::v1::ExportMetricsServiceRequest;
        use opentelemetry_proto::tonic::common::v1::{AnyValue as PbAny, KeyValue, any_value};
        use opentelemetry_proto::tonic::metrics::v1::{
            Gauge, Metric, NumberDataPoint, ResourceMetrics, ScopeMetrics, metric,
            number_data_point,
        };
        use opentelemetry_proto::tonic::resource::v1::Resource;
        use prost::Message;

        let sv = |s: &str| PbAny {
            value: Some(any_value::Value::StringValue(s.to_owned())),
        };
        let kv = |k: &str, v: &str| KeyValue {
            key: k.to_owned(),
            value: Some(sv(v)),
            ..Default::default()
        };
        let dps = vals
            .iter()
            .enumerate()
            .map(|(i, v)| NumberDataPoint {
                time_unix_nano: i as u64 + 1,
                value: Some(number_data_point::Value::AsDouble(1.0)),
                attributes: vec![kv(key, v)],
                ..Default::default()
            })
            .collect();
        ExportMetricsServiceRequest {
            resource_metrics: vec![ResourceMetrics {
                resource: Some(Resource {
                    attributes: vec![kv("service.name", service)],
                    ..Default::default()
                }),
                scope_metrics: vec![ScopeMetrics {
                    metrics: vec![Metric {
                        name: metric.to_owned(),
                        unit: "1".to_owned(),
                        data: Some(metric::Data::Gauge(Gauge { data_points: dps })),
                        ..Default::default()
                    }],
                    ..Default::default()
                }],
                ..Default::default()
            }],
        }
        .encode_to_vec()
    }

    #[tokio::test(flavor = "current_thread")]
    async fn metrics_promql_label_selectors() {
        let db = Db::in_memory().open().unwrap();
        db.ingest_otlp_metrics(&otlp_gauge_labeled(
            "s",
            "cpu",
            "host",
            &["web1", "web2", "db1"],
        ))
        .await
        .unwrap();
        let series = |q: MetricQuery| {
            let db = db.clone();
            async move {
                db.metrics()
                    .range(q.group_by("host").step(std::time::Duration::from_secs(60)))
                    .await
                    .unwrap()
                    .0
                    .len()
            }
        };
        assert_eq!(series(MetricQuery::gauge("cpu")).await, 3); // web1, web2, db1
        assert_eq!(
            series(MetricQuery::gauge("cpu").filter("host", "web1")).await,
            1
        );
        assert_eq!(
            series(MetricQuery::gauge("cpu").filter_ne("host", "web1")).await,
            2
        );
        assert_eq!(
            series(MetricQuery::gauge("cpu").filter_regex("host", "^web")).await,
            2
        );
        assert_eq!(
            series(MetricQuery::gauge("cpu").filter_not_regex("host", "^web")).await,
            1 // only db1
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn metrics_series_lists_label_sets() {
        let db = Db::in_memory().open().unwrap();
        db.ingest_otlp_metrics(&otlp_gauge_labeled(
            "cart",
            "temp",
            "zone",
            &["a", "b", "a"],
        ))
        .await
        .unwrap();

        // Three points, two distinct label sets: {zone:a} and {zone:b}.
        let series = db.metrics().series("temp").await.unwrap();
        assert_eq!(series.len(), 2, "distinct label sets");
        let mut zones: Vec<String> = series
            .iter()
            .filter_map(|a| a.get_str("zone").map(str::to_owned))
            .collect();
        zones.sort();
        assert_eq!(zones, vec!["a".to_owned(), "b".to_owned()]);

        // A metric with no data → no series.
        assert!(db.metrics().series("nope").await.unwrap().is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn metrics_series_spans_all_tables() {
        let db = Db::in_memory().open().unwrap();
        // Same metric name under two different tables: a labeled gauge and an (attribute-less) summary.
        db.ingest_otlp_metrics(&otlp_gauge_labeled("cart", "m", "zone", &["a"]))
            .await
            .unwrap();
        db.ingest_otlp_metrics(&otlp_summary("cart", "m", 100, 1, 1.0, &[(0.5, 1.0)]))
            .await
            .unwrap();
        // gauge contributes {zone:a}; summary contributes {} → series() unions across all tables.
        let series = db.metrics().series("m").await.unwrap();
        assert_eq!(series.len(), 2, "series() reaches the summary table too");
    }

    /// Build an OTLP monotonic Sum with one data point per `(time, value)` and the given
    /// `aggregation_temporality` (1 = DELTA, 2 = CUMULATIVE).
    fn otlp_sum(service: &str, metric: &str, temporality: i32, points: &[(u64, f64)]) -> Vec<u8> {
        use opentelemetry_proto::tonic::collector::metrics::v1::ExportMetricsServiceRequest;
        use opentelemetry_proto::tonic::common::v1::{AnyValue as PbAny, KeyValue, any_value};
        use opentelemetry_proto::tonic::metrics::v1::{
            Metric, NumberDataPoint, ResourceMetrics, ScopeMetrics, Sum, metric, number_data_point,
        };
        use opentelemetry_proto::tonic::resource::v1::Resource;
        use prost::Message;

        let sv = |s: &str| PbAny {
            value: Some(any_value::Value::StringValue(s.to_owned())),
        };
        let dps = points
            .iter()
            .map(|(t, v)| NumberDataPoint {
                time_unix_nano: *t,
                value: Some(number_data_point::Value::AsDouble(*v)),
                ..Default::default()
            })
            .collect();
        ExportMetricsServiceRequest {
            resource_metrics: vec![ResourceMetrics {
                resource: Some(Resource {
                    attributes: vec![KeyValue {
                        key: "service.name".to_owned(),
                        value: Some(sv(service)),
                        ..Default::default()
                    }],
                    ..Default::default()
                }),
                scope_metrics: vec![ScopeMetrics {
                    metrics: vec![Metric {
                        name: metric.to_owned(),
                        unit: "1".to_owned(),
                        data: Some(metric::Data::Sum(Sum {
                            data_points: dps,
                            aggregation_temporality: temporality,
                            is_monotonic: true,
                        })),
                        ..Default::default()
                    }],
                    ..Default::default()
                }],
                ..Default::default()
            }],
        }
        .encode_to_vec()
    }

    #[tokio::test(flavor = "current_thread")]
    async fn metrics_rate_of_delta_sum() {
        let db = Db::in_memory().open().unwrap();
        // Three delta increments of 3 within one 3-second bucket → 9 total → 3.0 req/s.
        db.ingest_otlp_metrics(&otlp_sum(
            "cart",
            "requests",
            1, // DELTA
            &[(0, 3.0), (1_000_000_000, 3.0), (2_000_000_000, 3.0)],
        ))
        .await
        .unwrap();

        let m = db
            .metrics()
            .range(
                MetricQuery::sum("requests")
                    .rate()
                    .step(std::time::Duration::from_secs(3)),
            )
            .await
            .unwrap();
        assert_eq!(m.0.len(), 1);
        let s = &m.0[0];
        assert_eq!(s.samples.len(), 1, "all three points fall in one bucket");
        assert!(
            (s.samples[0].value - 3.0).abs() < 1e-9,
            "rate={}",
            s.samples[0].value
        );

        // Without .rate(), the same query sums to the raw increase (9), not a per-second rate.
        let raw = db
            .metrics()
            .range(MetricQuery::sum("requests").step(std::time::Duration::from_secs(3)))
            .await
            .unwrap();
        assert!((raw.0[0].samples[0].value - 9.0).abs() < 1e-9);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn metrics_count_aggregation_returns_float() {
        // Regression: `count(value)` is Int64, but the materializer downcasts the value column to
        // Float64 — so Count must be emitted as `CAST(count(...) AS DOUBLE)` or it errors at runtime.
        let db = Db::in_memory().open().unwrap();
        db.ingest_otlp_metrics(&otlp_sum(
            "cart",
            "requests",
            1, // DELTA
            &[(0, 3.0), (1_000_000_000, 5.0), (2_000_000_000, 7.0)],
        ))
        .await
        .unwrap();

        let m = db
            .metrics()
            .range(
                MetricQuery::sum("requests")
                    .aggregation(Aggregation::Count)
                    .step(std::time::Duration::from_secs(3)),
            )
            .await
            .unwrap();
        assert_eq!(m.0.len(), 1);
        assert_eq!(m.0[0].samples.len(), 1);
        assert!(
            (m.0[0].samples[0].value - 3.0).abs() < 1e-9,
            "count of 3 points = 3.0, got {}",
            m.0[0].samples[0].value
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn metrics_rate_of_cumulative_counter() {
        let db = Db::in_memory().open().unwrap();
        // Cumulative counter reads 10 → 13 → 16 within one 3-second bucket: increase 6 → 2.0/s.
        db.ingest_otlp_metrics(&otlp_sum(
            "cart",
            "bytes_total",
            2, // CUMULATIVE
            &[(0, 10.0), (1_000_000_000, 13.0), (2_000_000_000, 16.0)],
        ))
        .await
        .unwrap();

        let m = db
            .metrics()
            .range(
                MetricQuery::sum("bytes_total")
                    .rate_counter()
                    .step(std::time::Duration::from_secs(3)),
            )
            .await
            .unwrap();
        assert_eq!(m.0.len(), 1);
        let s = &m.0[0];
        assert_eq!(s.samples.len(), 1);
        assert!(
            (s.samples[0].value - 2.0).abs() < 1e-9,
            "counter rate (max-min)/3s = 2.0, got {}",
            s.samples[0].value
        );
    }

    #[test]
    fn blocking_facade_works() {
        // A plain sync test: no #[tokio::test], no async context — the whole point of `blocking()`.
        let dir = tempfile::tempdir().unwrap();
        let db = Db::builder(dir.path()).open().unwrap();
        let b = db.blocking();

        assert_eq!(
            b.ingest_otlp_logs(&otlp_log("cart", "hello error", 1))
                .unwrap()
                .accepted,
            1
        );
        b.flush().unwrap(); // seal → segment + index

        let batches = b
            .sql("SELECT count(*) AS c FROM logs WHERE matches(body, 'error')")
            .unwrap();
        let c = batches[0]
            .column(0)
            .as_any()
            .downcast_ref::<Int64Array>()
            .unwrap()
            .value(0);
        assert_eq!(c, 1);

        let stats = b.stats().unwrap();
        assert!(
            stats
                .tables
                .iter()
                .any(|t| t.table == Table::Logs && t.segment_rows == 1)
        );
        assert_eq!(db.segment_files(Table::Logs).len(), 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn compaction_merges_and_reindexes() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::builder(dir.path()).open().unwrap();
        // Two seals in the same UTC day → two logs segments.
        db.ingest_otlp_logs(&otlp_log("cart", "connection error", 10))
            .await
            .unwrap();
        db.flush().await.unwrap();
        db.ingest_otlp_logs(&otlp_log("checkout", "upstream error", 20))
            .await
            .unwrap();
        db.flush().await.unwrap();
        assert_eq!(db.segments().len(), 2);

        let report = db.compact().await.unwrap();
        assert_eq!(report.segments_merged, 2);
        assert_eq!(report.segments_created, 1);
        assert_eq!(db.segments().len(), 1);

        // Data intact and searchable (the Tantivy index was rebuilt from the merged segment).
        assert_eq!(count(&db, "SELECT count(*) AS c FROM logs").await, 2);
        assert_eq!(
            count(
                &db,
                "SELECT count(*) AS c FROM logs WHERE matches(body, 'error')"
            )
            .await,
            2
        );
        assert_eq!(
            count(
                &db,
                "SELECT count(*) AS c FROM logs WHERE service = 'checkout'"
            )
            .await,
            1
        );

        // Survives reopen.
        drop(db);
        let db2 = Db::builder(dir.path()).open().unwrap();
        assert_eq!(count(&db2, "SELECT count(*) AS c FROM logs").await, 2);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn stats_and_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::builder(dir.path()).open().unwrap();
        db.ingest_otlp_logs(&otlp_log("cart", "a", 10))
            .await
            .unwrap();
        db.ingest_otlp_logs(&otlp_log("cart", "b", 20))
            .await
            .unwrap();
        db.ingest_otlp_metrics(&otlp_metrics("cart")).await.unwrap();
        db.flush().await.unwrap(); // seal logs + metric tables

        let stats = db.stats().await.unwrap();
        let logs = stats
            .tables
            .iter()
            .find(|t| t.table == Table::Logs)
            .unwrap();
        assert_eq!(logs.segment_count, 1);
        assert_eq!(logs.segment_rows, 2);
        assert_eq!(logs.min_time_unix_nano, Some(10));
        assert_eq!(logs.max_time_unix_nano, Some(20));
        let gauge = stats
            .tables
            .iter()
            .find(|t| t.table == Table::MetricsGauge)
            .unwrap();
        assert_eq!(gauge.segment_rows, 1);

        // snapshot → manifest + hard-linked segments queryable from the copy.
        let snap_dir = tempfile::tempdir().unwrap();
        let info = db.snapshot(snap_dir.path()).await.unwrap();
        assert!(info.segments >= 2); // logs + gauge + sum
        // The snapshot carries a self-contained v2 manifest (CURRENT + a checkpoint log).
        assert!(snap_dir.path().join("CURRENT").exists());
        let snap_db = Db::builder(snap_dir.path()).open().unwrap();
        assert_eq!(count(&snap_db, "SELECT count(*) AS c FROM logs").await, 2);
        assert_eq!(
            count(&snap_db, "SELECT count(*) AS c FROM metrics_gauge").await,
            1
        );
    }

    /// Regression: a read-only handle holds no live buffers/segments (its query view is derived per
    /// call from the on-disk snapshot), so the naive `storage.stats()` reported zero rows for every
    /// table even on a populated DB. `stats()` must read the disk snapshot + WAL tail instead.
    #[tokio::test(flavor = "current_thread")]
    async fn read_only_stats_reflect_segments_and_wal_tail() {
        let dir = tempfile::tempdir().unwrap();
        let writer = Db::builder(dir.path()).wal(WalMode::Always).open().unwrap();
        let reader = Db::open_read_only(dir.path()).unwrap();

        let logs = |s: &DbStats| {
            s.tables
                .iter()
                .find(|t| t.table == Table::Logs)
                .unwrap()
                .clone()
        };

        // Populated but never sealed: the rows live only in the WAL tail, which the reader must count
        // as buffer rows (this is exactly the all-zero case the overview hit).
        writer
            .ingest_otlp_logs(&otlp_log("cart", "a", 10))
            .await
            .unwrap();
        writer
            .ingest_otlp_logs(&otlp_log("cart", "b", 20))
            .await
            .unwrap();
        let l = logs(&reader.stats().await.unwrap());
        assert_eq!(l.segment_rows, 0);
        assert_eq!(l.buffer_rows, 2, "unsealed WAL tail counted as buffer rows");

        // After a seal the rows move buffer→segment; the reader now reads them from the manifest,
        // with the segment time-bounds, and a metric table is counted too.
        writer
            .ingest_otlp_metrics(&otlp_metrics("cart"))
            .await
            .unwrap();
        writer.flush().await.unwrap();
        let stats = reader.stats().await.unwrap();
        let l = logs(&stats);
        assert_eq!(l.segment_rows, 2, "sealed rows counted from the manifest");
        assert_eq!(l.buffer_rows, 0);
        assert_eq!(l.segment_count, 1);
        assert_eq!(l.min_time_unix_nano, Some(10));
        assert_eq!(l.max_time_unix_nano, Some(20));
        let gauge = stats
            .tables
            .iter()
            .find(|t| t.table == Table::MetricsGauge)
            .unwrap();
        assert_eq!(gauge.segment_rows, 1, "metric tables counted too");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn metrics_typed_api() {
        let db = Db::in_memory().open().unwrap();
        db.ingest_otlp_metrics(&otlp_metrics("cart")).await.unwrap();
        db.ingest_otlp_metrics(&otlp_histogram(
            "cart",
            "http.server.duration",
            50,
            7,
            12.5,
            &[1.0, 5.0],
            &[2, 3, 2],
        ))
        .await
        .unwrap();

        // catalog() reports gauge, sum, and histogram metrics with their kind/temporality.
        let cat = db.metrics().catalog().await.unwrap();
        assert!(cat.iter().any(|m| m.metric == "cpu" && m.kind == "gauge"));
        assert!(cat.iter().any(|m| {
            m.metric == "requests"
                && m.kind == "sum"
                && m.temporality.as_deref() == Some("CUMULATIVE")
        }));
        assert!(
            cat.iter()
                .any(|m| m.metric == "http.server.duration" && m.kind == "histogram"),
            "catalog should include the histogram metric"
        );

        // range() over the gauge → one (unlabeled) series with one bucketed sample.
        let matrix = db
            .metrics()
            .range(MetricQuery::gauge("cpu").step(std::time::Duration::from_nanos(1000)))
            .await
            .unwrap();
        assert_eq!(matrix.0.len(), 1);
        assert_eq!(matrix.0[0].samples.len(), 1);
        assert_eq!(matrix.0[0].samples[0].value, 0.5);

        // instant() over the sum → the last sample's value.
        let vector = db
            .metrics()
            .instant(MetricQuery::sum("requests").step(std::time::Duration::from_nanos(1000)))
            .await
            .unwrap();
        assert_eq!(vector.0.len(), 1);
        assert_eq!(vector.0[0].sample.value, 42.0);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn metrics_ingest_seal_query_recover() {
        let dir = tempfile::tempdir().unwrap();
        {
            let db = Db::builder(dir.path()).open().unwrap();
            let r = db.ingest_otlp_metrics(&otlp_metrics("cart")).await.unwrap();
            assert_eq!(r.accepted, 2);
            assert_eq!(
                count(&db, "SELECT count(*) AS c FROM metrics_gauge").await,
                1
            );
            assert_eq!(count(&db, "SELECT count(*) AS c FROM metrics_sum").await, 1);
            assert_eq!(
                count(
                    &db,
                    "SELECT count(*) AS c FROM metrics_sum \
                     WHERE metric = 'requests' AND value = 42 AND temporality = 'CUMULATIVE'"
                )
                .await,
                1
            );
            db.flush().await.unwrap(); // seal both metric tables
        }
        // Reopen: metric segments recovered from the manifest.
        let db2 = Db::builder(dir.path()).open().unwrap();
        assert_eq!(
            count(&db2, "SELECT count(*) AS c FROM metrics_gauge").await,
            1
        );
        assert_eq!(
            count(
                &db2,
                "SELECT count(*) AS c FROM metrics_sum WHERE value = 42"
            )
            .await,
            1
        );
    }

    async fn count(db: &Arc<Db>, sql: &str) -> i64 {
        let out = db.sql(sql).collect().await.unwrap();
        out[0]
            .column(0)
            .as_any()
            .downcast_ref::<Int64Array>()
            .unwrap()
            .value(0)
    }

    #[tokio::test(flavor = "current_thread")]
    async fn db_stats_engine_gauges() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::builder(dir.path()).wal(WalMode::Always).open().unwrap();
        let r = db
            .ingest_otlp_logs(&otlp_log("cart", "hello", 1))
            .await
            .unwrap();
        assert!(r.durable, "WalMode::Always fsyncs the ingest");

        let s = db.stats().await.unwrap();
        assert!(s.buffer_bytes > 0, "buffered rows hold heap");
        assert!(s.wal_bytes > 0, "a WAL frame was written");
        assert_eq!(s.durable_lsn, r.lsn, "durable through the fsync'd LSN");

        db.flush().await.unwrap();
        let s2 = db.stats().await.unwrap();
        assert_eq!(s2.buffer_bytes, 0, "seal empties the buffers");
        assert_eq!(s2.wal_bytes, 0, "seal truncates the sealed WAL frames");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn buffer_only_query() {
        let db = Db::in_memory().open().unwrap();
        let r = db
            .ingest_otlp_logs(&otlp_log("cart", "hello", 1))
            .await
            .unwrap();
        assert_eq!(r.accepted, 1);
        assert_eq!(count(&db, "SELECT count(*) AS c FROM logs").await, 1);
    }

    /// Build a one-point OTLP explicit-bucket histogram protobuf body.
    fn otlp_histogram(
        service: &str,
        metric: &str,
        time: u64,
        count: u64,
        sum: f64,
        bounds: &[f64],
        counts: &[u64],
    ) -> Vec<u8> {
        use opentelemetry_proto::tonic::collector::metrics::v1::ExportMetricsServiceRequest;
        use opentelemetry_proto::tonic::common::v1::{AnyValue as PbAny, KeyValue, any_value};
        use opentelemetry_proto::tonic::metrics::v1::{
            Histogram, HistogramDataPoint, Metric, ResourceMetrics, ScopeMetrics, metric,
        };
        use opentelemetry_proto::tonic::resource::v1::Resource;
        use prost::Message;

        let sv = |s: &str| PbAny {
            value: Some(any_value::Value::StringValue(s.to_owned())),
        };
        ExportMetricsServiceRequest {
            resource_metrics: vec![ResourceMetrics {
                resource: Some(Resource {
                    attributes: vec![KeyValue {
                        key: "service.name".to_owned(),
                        value: Some(sv(service)),
                        ..Default::default()
                    }],
                    ..Default::default()
                }),
                scope_metrics: vec![ScopeMetrics {
                    metrics: vec![Metric {
                        name: metric.to_owned(),
                        unit: "ms".to_owned(),
                        data: Some(metric::Data::Histogram(Histogram {
                            data_points: vec![HistogramDataPoint {
                                time_unix_nano: time,
                                count,
                                sum: Some(sum),
                                explicit_bounds: bounds.to_vec(),
                                bucket_counts: counts.to_vec(),
                                ..Default::default()
                            }],
                            aggregation_temporality: 2, // CUMULATIVE
                        })),
                        ..Default::default()
                    }],
                    ..Default::default()
                }],
                ..Default::default()
            }],
        }
        .encode_to_vec()
    }

    /// Build a one-point OTLP exponential-histogram protobuf body (positive buckets only).
    #[allow(clippy::too_many_arguments)]
    fn otlp_exp_histogram(
        service: &str,
        metric: &str,
        time: u64,
        count: u64,
        scale: i32,
        zero_count: u64,
        pos_offset: i32,
        pos_counts: &[u64],
    ) -> Vec<u8> {
        use opentelemetry_proto::tonic::collector::metrics::v1::ExportMetricsServiceRequest;
        use opentelemetry_proto::tonic::common::v1::{AnyValue as PbAny, KeyValue, any_value};
        use opentelemetry_proto::tonic::metrics::v1::{
            ExponentialHistogram, ExponentialHistogramDataPoint, Metric, ResourceMetrics,
            ScopeMetrics, exponential_histogram_data_point::Buckets, metric,
        };
        use opentelemetry_proto::tonic::resource::v1::Resource;
        use prost::Message;

        let sv = |s: &str| PbAny {
            value: Some(any_value::Value::StringValue(s.to_owned())),
        };
        ExportMetricsServiceRequest {
            resource_metrics: vec![ResourceMetrics {
                resource: Some(Resource {
                    attributes: vec![KeyValue {
                        key: "service.name".to_owned(),
                        value: Some(sv(service)),
                        ..Default::default()
                    }],
                    ..Default::default()
                }),
                scope_metrics: vec![ScopeMetrics {
                    metrics: vec![Metric {
                        name: metric.to_owned(),
                        unit: "ms".to_owned(),
                        data: Some(metric::Data::ExponentialHistogram(ExponentialHistogram {
                            data_points: vec![ExponentialHistogramDataPoint {
                                time_unix_nano: time,
                                count,
                                sum: Some(1.0),
                                scale,
                                zero_count,
                                positive: Some(Buckets {
                                    offset: pos_offset,
                                    bucket_counts: pos_counts.to_vec(),
                                }),
                                ..Default::default()
                            }],
                            aggregation_temporality: 2, // CUMULATIVE
                        })),
                        ..Default::default()
                    }],
                    ..Default::default()
                }],
                ..Default::default()
            }],
        }
        .encode_to_vec()
    }

    /// Build a one-point OTLP Summary protobuf body with the given (quantile, value) pairs.
    fn otlp_summary(
        service: &str,
        metric: &str,
        time: u64,
        count: u64,
        sum: f64,
        qvs: &[(f64, f64)],
    ) -> Vec<u8> {
        use opentelemetry_proto::tonic::collector::metrics::v1::ExportMetricsServiceRequest;
        use opentelemetry_proto::tonic::common::v1::{AnyValue as PbAny, KeyValue, any_value};
        use opentelemetry_proto::tonic::metrics::v1::{
            Metric, ResourceMetrics, ScopeMetrics, Summary, SummaryDataPoint, metric,
            summary_data_point::ValueAtQuantile,
        };
        use opentelemetry_proto::tonic::resource::v1::Resource;
        use prost::Message;

        let sv = |s: &str| PbAny {
            value: Some(any_value::Value::StringValue(s.to_owned())),
        };
        ExportMetricsServiceRequest {
            resource_metrics: vec![ResourceMetrics {
                resource: Some(Resource {
                    attributes: vec![KeyValue {
                        key: "service.name".to_owned(),
                        value: Some(sv(service)),
                        ..Default::default()
                    }],
                    ..Default::default()
                }),
                scope_metrics: vec![ScopeMetrics {
                    metrics: vec![Metric {
                        name: metric.to_owned(),
                        unit: "ms".to_owned(),
                        data: Some(metric::Data::Summary(Summary {
                            data_points: vec![SummaryDataPoint {
                                time_unix_nano: time,
                                count,
                                sum,
                                quantile_values: qvs
                                    .iter()
                                    .map(|(q, v)| ValueAtQuantile {
                                        quantile: *q,
                                        value: *v,
                                    })
                                    .collect(),
                                ..Default::default()
                            }],
                        })),
                        ..Default::default()
                    }],
                    ..Default::default()
                }],
                ..Default::default()
            }],
        }
        .encode_to_vec()
    }

    #[tokio::test(flavor = "current_thread")]
    async fn summary_table_query_and_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        {
            let db = Db::builder(dir.path()).open().unwrap();
            db.ingest_otlp_metrics(&otlp_summary(
                "cart",
                "lat",
                100,
                10,
                55.0,
                &[(0.5, 3.0), (0.95, 9.0), (0.99, 11.0)],
            ))
            .await
            .unwrap();

            // Buffer path: scalar + List columns intact; the p95 value is directly readable.
            assert_eq!(
                count(
                    &db,
                    "SELECT count(*) AS c FROM metrics_summary \
                     WHERE metric = 'lat' AND \"count\" = 10 \
                     AND array_length(quantiles, 1) = 3 AND array_length(values, 1) = 3",
                )
                .await,
                1
            );
            // catalog reports the summary kind.
            let cat = db.metrics().catalog().await.unwrap();
            assert!(cat.iter().any(|m| m.metric == "lat" && m.kind == "summary"));

            db.flush().await.unwrap();
            assert!(!db.segment_files(Table::MetricsSummary).is_empty());
            assert_eq!(
                count(
                    &db,
                    "SELECT count(*) AS c FROM metrics_summary WHERE array_length(values, 1) = 3",
                )
                .await,
                1,
                "segment: List columns survive Parquet + coerce"
            );
            // Unsealed second point for WAL replay.
            db.ingest_otlp_metrics(&otlp_summary("cart", "lat", 200, 4, 8.0, &[(0.5, 2.0)]))
                .await
                .unwrap();
        }
        let db2 = Db::builder(dir.path()).open().unwrap();
        assert_eq!(
            count(&db2, "SELECT count(*) AS c FROM metrics_summary").await,
            2,
            "sealed + WAL-replayed summaries both recover"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn typed_exp_histogram_quantile() {
        let db = Db::in_memory().open().unwrap();
        // scale 0 (base=2): one bucket index 0 = (1, 2] with 4 values → p50 = 1.5.
        db.ingest_otlp_metrics(&otlp_exp_histogram("cart", "lat", 100, 4, 0, 0, 0, &[4]))
            .await
            .unwrap();

        let m = db
            .metrics()
            .exp_histogram_quantile(ExpHistogramQuery::new("lat").quantile(0.5))
            .await
            .unwrap();
        assert_eq!(m.0.len(), 1);
        assert_eq!(m.0[0].samples.len(), 1);
        let v = m.0[0].samples[0].value;
        assert!((v - 1.5).abs() < 1e-9, "exp-histogram p50={v}");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn exp_histogram_quantile_merges_across_scales() {
        let db = Db::in_memory().open().unwrap();
        // Two points in one 1s bucket at DIFFERENT scales:
        //  A: scale 1, offset 2, [8] → downscales to scale-0 bucket 1 = (2,4].
        //  B: scale 0, offset 0, [2] → scale-0 bucket 0 = (1,2].
        db.ingest_otlp_metrics(&otlp_exp_histogram("cart", "lat", 0, 8, 1, 0, 2, &[8]))
            .await
            .unwrap();
        db.ingest_otlp_metrics(&otlp_exp_histogram("cart", "lat", 100, 2, 0, 0, 0, &[2]))
            .await
            .unwrap();

        // Per-point: two separate samples.
        let per = db
            .metrics()
            .exp_histogram_quantile(ExpHistogramQuery::new("lat").quantile(0.5))
            .await
            .unwrap();
        assert_eq!(per.0.len(), 1);
        assert_eq!(per.0[0].samples.len(), 2);

        // Merged over a 1s step: aligns to scale 0 (buckets {0:2, 1:8}) → p50 = 2.75.
        let merged = db
            .metrics()
            .exp_histogram_quantile(
                ExpHistogramQuery::new("lat")
                    .quantile(0.5)
                    .step(std::time::Duration::from_secs(1)),
            )
            .await
            .unwrap();
        assert_eq!(merged.0.len(), 1);
        assert_eq!(merged.0[0].samples.len(), 1, "points merge into one bucket");
        let v = merged.0[0].samples[0].value;
        assert!(
            (v - 2.75).abs() < 1e-9,
            "scale-aligned merged p50 = 2.75, got {v}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn exp_histogram_merge_guards_extreme_scale_delta() {
        let db = Db::in_memory().open().unwrap();
        // Scales 40 and 0 differ by > 32; the down-scale shift `>> delta` must be clamped, not panic.
        db.ingest_otlp_metrics(&otlp_exp_histogram("cart", "lat", 0, 4, 40, 0, 0, &[4]))
            .await
            .unwrap();
        db.ingest_otlp_metrics(&otlp_exp_histogram("cart", "lat", 100, 4, 0, 0, 0, &[4]))
            .await
            .unwrap();
        let m = db
            .metrics()
            .exp_histogram_quantile(
                ExpHistogramQuery::new("lat")
                    .quantile(0.5)
                    .step(std::time::Duration::from_secs(1)),
            )
            .await
            .unwrap();
        assert_eq!(m.0.len(), 1);
        assert!(
            m.0[0].samples[0].value.is_finite(),
            "extreme scale delta is clamped → finite, no shift-overflow panic"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn exp_histogram_merge_guards_pathological_offsets() {
        let db = Db::in_memory().open().unwrap();
        // Two points in one bucket whose positive offsets are ~2M apart: densifying the merged span
        // would allocate millions of buckets. The guard returns NaN instead of OOM/overflow-panicking.
        db.ingest_otlp_metrics(&otlp_exp_histogram("cart", "lat", 0, 4, 0, 0, 0, &[4]))
            .await
            .unwrap();
        db.ingest_otlp_metrics(&otlp_exp_histogram(
            "cart",
            "lat",
            100,
            4,
            0,
            0,
            2_000_000,
            &[4],
        ))
        .await
        .unwrap();

        let m = db
            .metrics()
            .exp_histogram_quantile(
                ExpHistogramQuery::new("lat")
                    .quantile(0.5)
                    .step(std::time::Duration::from_secs(1)),
            )
            .await
            .unwrap();
        assert_eq!(m.0.len(), 1);
        assert_eq!(m.0[0].samples.len(), 1);
        assert!(
            m.0[0].samples[0].value.is_nan(),
            "pathological offset span is guarded → NaN, not a panic/OOM"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn exp_histogram_table_query_and_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        {
            let db = Db::builder(dir.path()).open().unwrap();
            db.ingest_otlp_metrics(&otlp_exp_histogram(
                "cart",
                "lat",
                100,
                9,
                3,
                1,
                2,
                &[2, 3, 2],
            ))
            .await
            .unwrap();

            // Buffer path: scalar + Int32 + List columns intact.
            assert_eq!(
                count(
                    &db,
                    "SELECT count(*) AS c FROM metrics_exp_histogram \
                     WHERE metric = 'lat' AND \"count\" = 9 AND scale = 3 AND zero_count = 1 \
                     AND positive_offset = 2 AND array_length(positive_counts, 1) = 3",
                )
                .await,
                1,
                "buffer: exp-histogram columns intact"
            );

            // Seal → Parquet segment (List columns), re-query the segment.
            db.flush().await.unwrap();
            assert!(!db.segment_files(Table::MetricsExpHistogram).is_empty());
            assert_eq!(
                count(
                    &db,
                    "SELECT count(*) AS c FROM metrics_exp_histogram \
                     WHERE scale = 3 AND array_length(positive_counts, 1) = 3",
                )
                .await,
                1,
                "segment: List/Int32 columns survive Parquet + coerce"
            );

            // Unsealed second point for WAL replay.
            db.ingest_otlp_metrics(&otlp_exp_histogram("cart", "lat", 200, 4, 3, 0, 1, &[1, 1]))
                .await
                .unwrap();
        }
        // Reopen: one from the segment, one WAL-replayed.
        let db2 = Db::builder(dir.path()).open().unwrap();
        assert_eq!(
            count(&db2, "SELECT count(*) AS c FROM metrics_exp_histogram").await,
            2,
            "sealed + WAL-replayed exp-histograms both recover"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn compact_merges_histogram_segments() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::builder(dir.path()).open().unwrap();
        // Two histogram segments in the same UTC day (times 100, 200).
        db.ingest_otlp_metrics(&otlp_histogram(
            "cart",
            "lat",
            100,
            7,
            12.5,
            &[1.0, 5.0],
            &[2, 3, 2],
        ))
        .await
        .unwrap();
        db.flush().await.unwrap();
        db.ingest_otlp_metrics(&otlp_histogram(
            "cart",
            "lat",
            200,
            4,
            8.0,
            &[1.0, 5.0],
            &[1, 2, 1],
        ))
        .await
        .unwrap();
        db.flush().await.unwrap();
        assert_eq!(
            db.segment_files(Table::MetricsHistogram).len(),
            2,
            "two segments before compaction"
        );

        let report = db.compact().await.unwrap();
        assert!(
            report.segments_merged >= 2,
            "histogram segments were merged"
        );
        assert_eq!(
            db.segment_files(Table::MetricsHistogram).len(),
            1,
            "compacted to a single segment"
        );
        // Data (incl. the List columns) intact after compaction.
        assert_eq!(
            count(&db, "SELECT count(*) AS c FROM metrics_histogram").await,
            2
        );
        assert_eq!(
            count(
                &db,
                "SELECT count(*) AS c FROM metrics_histogram WHERE array_length(bucket_counts, 1) = 3",
            )
            .await,
            2,
            "List columns survive compaction"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn histogram_table_query_and_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        {
            let db = Db::builder(dir.path()).open().unwrap();
            db.ingest_otlp_metrics(&otlp_histogram(
                "cart",
                "http.server.duration",
                100,
                7,
                12.5,
                &[1.0, 5.0],
                &[2, 3, 2], // N+1 counts for N=2 bounds
            ))
            .await
            .unwrap();

            // Buffer path: the row is queryable, scalar + List columns intact.
            assert_eq!(
                count(&db, "SELECT count(*) AS c FROM metrics_histogram").await,
                1
            );
            assert_eq!(
                count(
                    &db,
                    "SELECT count(*) AS c FROM metrics_histogram \
                     WHERE metric = 'http.server.duration' AND \"count\" = 7 \
                     AND array_length(explicit_bounds, 1) = 2 \
                     AND array_length(bucket_counts, 1) = 3",
                )
                .await,
                1,
                "buffer: scalar + List columns should be intact"
            );

            // Seal → Parquet segment with List columns, then re-query the segment path.
            db.flush().await.unwrap();
            assert!(
                !db.segment_files(Table::MetricsHistogram).is_empty(),
                "histogram buffer should have sealed a segment"
            );
            assert_eq!(
                count(
                    &db,
                    "SELECT count(*) AS c FROM metrics_histogram \
                     WHERE \"count\" = 7 AND array_length(bucket_counts, 1) = 3 \
                     AND array_length(explicit_bounds, 1) = 2",
                )
                .await,
                1,
                "segment: List columns should survive Parquet + coerce"
            );

            // The histogram_quantile UDF over the sealed segment's List columns: bounds=[1,5],
            // counts=[2,3,2] → p50 interpolates to 3.0 inside the (1,5] bucket.
            let hq = db
                .sql(
                    "SELECT histogram_quantile(0.5, explicit_bounds, bucket_counts) AS p50 \
                     FROM metrics_histogram WHERE \"count\" = 7",
                )
                .collect()
                .await
                .unwrap();
            let p50 = hq[0]
                .column(0)
                .as_any()
                .downcast_ref::<Float64Array>()
                .unwrap()
                .value(0);
            assert!((p50 - 3.0).abs() < 1e-9, "histogram_quantile p50={p50}");

            // A second histogram stays in the buffer (WAL frame written, not sealed).
            db.ingest_otlp_metrics(&otlp_histogram(
                "cart",
                "http.server.duration",
                200,
                4,
                8.0,
                &[1.0, 5.0],
                &[1, 2, 1],
            ))
            .await
            .unwrap();
            assert_eq!(
                count(&db, "SELECT count(*) AS c FROM metrics_histogram").await,
                2
            );
        }
        // Reopen: one histogram from the sealed segment (manifest), one replayed from the WAL.
        let db2 = Db::builder(dir.path()).open().unwrap();
        assert_eq!(
            count(&db2, "SELECT count(*) AS c FROM metrics_histogram").await,
            2,
            "one sealed + one WAL-replayed histogram should both recover"
        );
        assert_eq!(
            count(
                &db2,
                "SELECT count(*) AS c FROM metrics_histogram WHERE \"count\" = 4",
            )
            .await,
            1,
            "the buffered histogram must replay from the WAL on reopen"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn typed_histogram_quantile() {
        let db = Db::in_memory().open().unwrap();
        // Two data points for one histogram metric at t=100 and t=200.
        for t in [100u64, 200] {
            db.ingest_otlp_metrics(&otlp_histogram(
                "cart",
                "http.server.duration",
                t,
                7,
                12.5,
                &[1.0, 5.0],
                &[2, 3, 2],
            ))
            .await
            .unwrap();
        }

        // p50 → 3.0 inside the (1,5] bucket, one sample per data point.
        let m = db
            .metrics()
            .histogram_quantile(HistogramQuery::new("http.server.duration").quantile(0.5))
            .await
            .unwrap();
        assert_eq!(m.0.len(), 1, "one (unlabeled) series");
        let s = &m.0[0];
        assert!(s.labels.is_empty());
        assert_eq!(s.samples.len(), 2);
        assert_eq!(s.samples[0].time.0, 100);
        assert!(
            (s.samples[0].value - 3.0).abs() < 1e-9,
            "p50={}",
            s.samples[0].value
        );
        assert_eq!(s.samples[1].time.0, 200);

        // Default p95 lands in the +Inf overflow → clamps to the top finite bound 5.0.
        let m95 = db
            .metrics()
            .histogram_quantile(HistogramQuery::new("http.server.duration"))
            .await
            .unwrap();
        assert!((m95.0[0].samples[0].value - 5.0).abs() < 1e-9);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn histogram_quantile_merges_across_step() {
        let db = Db::in_memory().open().unwrap();
        // Two points in the same 1s bucket with disjoint distributions: all-low vs all-overflow.
        db.ingest_otlp_metrics(&otlp_histogram(
            "cart",
            "lat",
            0,
            10,
            5.0,
            &[1.0, 5.0],
            &[10, 0, 0],
        ))
        .await
        .unwrap();
        db.ingest_otlp_metrics(&otlp_histogram(
            "cart",
            "lat",
            100,
            10,
            5.0,
            &[1.0, 5.0],
            &[0, 0, 10],
        ))
        .await
        .unwrap();

        // Per-point (no .step()): two separate samples, p50 = 0.5 and 5.0.
        let perpoint = db
            .metrics()
            .histogram_quantile(HistogramQuery::new("lat").quantile(0.5))
            .await
            .unwrap();
        assert_eq!(perpoint.0.len(), 1);
        assert_eq!(perpoint.0[0].samples.len(), 2);

        // Merged over a 1s step: counts=[10,0,10] → p50 = 1.0, distinct from either per-point value.
        let merged = db
            .metrics()
            .histogram_quantile(
                HistogramQuery::new("lat")
                    .quantile(0.5)
                    .step(std::time::Duration::from_secs(1)),
            )
            .await
            .unwrap();
        assert_eq!(merged.0.len(), 1);
        assert_eq!(
            merged.0[0].samples.len(),
            1,
            "both points merge into one bucket"
        );
        let v = merged.0[0].samples[0].value;
        assert!((v - 1.0).abs() < 1e-9, "merged p50 should be 1.0, got {v}");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn export_arrow_ipc_roundtrips() {
        use arrow::ipc::reader::StreamReader;

        let dir = tempfile::tempdir().unwrap();
        let db = Db::builder(dir.path()).open().unwrap();
        // Two rows in a sealed segment, one still in the buffer → export unions both.
        db.ingest_otlp_logs(&otlp_log("cart", "hello", 10))
            .await
            .unwrap();
        db.ingest_otlp_logs(&otlp_log("cart", "world", 20))
            .await
            .unwrap();
        db.flush().await.unwrap(); // seal → Parquet segment
        db.ingest_otlp_logs(&otlp_log("cart", "again", 30))
            .await
            .unwrap();

        let bytes = db.export(Table::Logs, TimeRange::all()).await.unwrap();
        assert!(!bytes.is_empty());

        // Decode the IPC stream with a stock Arrow reader (what a DuckDB/polars host would do).
        let reader = StreamReader::try_new(&bytes[..], None).unwrap();
        let schema = reader.schema();
        assert!(schema.column_with_name("service").is_some());
        assert!(schema.column_with_name("body").is_some());
        let batches: Vec<_> = reader.map(|b| b.unwrap()).collect();
        let rows: usize = batches.iter().map(|b| b.num_rows()).sum();
        assert_eq!(rows, 3, "buffer ∪ segment should export all three rows");

        // A range that matches nothing still yields a valid schema-only stream (0 rows).
        let empty = db
            .export(
                Table::Logs,
                TimeRange::between(Timestamp(1_000), Timestamp(2_000)),
            )
            .await
            .unwrap();
        let reader = StreamReader::try_new(&empty[..], None).unwrap();
        assert!(reader.schema().column_with_name("body").is_some());
        let empty_rows: usize = reader.map(|b| b.unwrap().num_rows()).sum();
        assert_eq!(empty_rows, 0);

        // Every metric table is materialized: exporting an empty one yields a valid schema-only
        // stream (0 rows), not an error.
        let empty_summary = db
            .export(Table::MetricsSummary, TimeRange::all())
            .await
            .unwrap();
        let reader = arrow::ipc::reader::StreamReader::try_new(&empty_summary[..], None).unwrap();
        assert!(reader.schema().column_with_name("quantiles").is_some());
    }

    /// I-2 invariant: batches returned by the query surface are owned, segment-independent
    /// allocations. Collect a result whose rows came from a sealed Parquet segment, then run
    /// `maintain()` (seal + retention with a zero disk budget) which *unlinks every segment on disk*,
    /// and confirm the already-collected batches are still fully readable. If any batch borrowed the
    /// mmap'd segment bytes this would be a use-after-free / short read; it is not.
    #[tokio::test(flavor = "current_thread")]
    async fn collected_batches_outlive_segment_reclaim() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::builder(dir.path())
            .retention(Retention::none().max_disk_bytes(0))
            .open()
            .unwrap();
        db.ingest_otlp_logs(&otlp_log("cart", "hello", 10))
            .await
            .unwrap();
        db.ingest_otlp_logs(&otlp_log("cart", "world", 20))
            .await
            .unwrap();
        db.flush().await.unwrap(); // seal → one Parquet segment on disk

        // Collect the rows (they come out of the sealed segment) and grab the schema too.
        let (schema, batches) = db
            .sql("SELECT service, body FROM logs ORDER BY body")
            .collect_with_schema()
            .await
            .unwrap();
        let seg_paths = db.segment_files(Table::Logs);
        assert!(!seg_paths.is_empty(), "rows should be in a sealed segment");

        // Retention (budget 0) unlinks every segment the query read.
        let report = db.maintain().await.unwrap();
        assert!(report.segments_dropped >= 1);
        assert!(
            seg_paths.iter().all(|p| !p.exists()),
            "every read segment is unlinked: {seg_paths:?}"
        );

        // The collected batches remain valid and complete.
        let rows: usize = batches.iter().map(|b| b.num_rows()).sum();
        assert_eq!(rows, 2, "both rows survive segment reclaim");
        assert!(schema.column_with_name("body").is_some());
        let bodies: Vec<String> = batches
            .iter()
            .flat_map(|b| {
                let col = b
                    .column(1)
                    .as_any()
                    .downcast_ref::<arrow::array::StringArray>()
                    .unwrap();
                (0..col.len())
                    .map(|i| col.value(i).to_owned())
                    .collect::<Vec<_>>()
            })
            .collect();
        assert_eq!(bodies, vec!["hello".to_owned(), "world".to_owned()]);
    }

    /// I-2, FFI flavour (the `cdata` feature): the analogue of the above across the Arrow C Data
    /// Interface. Export a query result to an in-memory `FFI_ArrowArrayStream`, unlink every segment
    /// the query read, then import that stream back and drain it — the data is intact because the
    /// batches the stream carries are owned, not borrowed from the reclaimed Parquet.
    #[cfg(feature = "cdata")]
    #[tokio::test(flavor = "current_thread")]
    async fn ffi_stream_outlives_segment_reclaim() {
        use arrow::array::RecordBatchReader;
        use arrow::ffi_stream::{ArrowArrayStreamReader, FFI_ArrowArrayStream};
        use arrow::record_batch::RecordBatchIterator;

        let dir = tempfile::tempdir().unwrap();
        let db = Db::builder(dir.path())
            .retention(Retention::none().max_disk_bytes(0))
            .open()
            .unwrap();
        db.ingest_otlp_logs(&otlp_log("cart", "hello", 10))
            .await
            .unwrap();
        db.ingest_otlp_logs(&otlp_log("cart", "world", 20))
            .await
            .unwrap();
        db.flush().await.unwrap();

        let (schema, batches) = db
            .sql("SELECT service, body FROM logs ORDER BY body")
            .collect_with_schema()
            .await
            .unwrap();
        let seg_paths = db.segment_files(Table::Logs);
        assert!(!seg_paths.is_empty());

        // Build the C Data Interface export a binding would hand to a foreign runtime: an
        // FFI_ArrowArrayStream over an in-memory RecordBatchReader.
        let reader = RecordBatchIterator::new(batches.into_iter().map(Ok), schema);
        let ffi = FFI_ArrowArrayStream::new(Box::new(reader));

        // Now unlink every segment the query read — the moral equivalent of the binding holding the
        // stream past retention on the imbh side.
        let report = db.maintain().await.unwrap();
        assert!(report.segments_dropped >= 1);
        assert!(seg_paths.iter().all(|p| !p.exists()));

        // Import the stream back (what the foreign consumer does) and drain it.
        let imported = ArrowArrayStreamReader::try_new(ffi).unwrap();
        assert!(imported.schema().column_with_name("body").is_some());
        let drained: Vec<_> = imported.map(|b| b.unwrap()).collect();
        let rows: usize = drained.iter().map(|b| b.num_rows()).sum();
        assert_eq!(rows, 2, "FFI stream data intact after segment reclaim");
    }

    /// I-4: `Query::stream` returns a `'static`, self-rooting stream. Drop *every* local binding —
    /// including the `Arc<Db>` — before draining, and it must still yield the full result (the stream
    /// owns its execution context and the snapshot's buffer batches; the on-disk segments stay put
    /// because the tempdir outlives the drain).
    #[tokio::test(flavor = "current_thread")]
    async fn stream_is_self_rooting_and_matches_collect() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::builder(dir.path()).open().unwrap();
        // Two rows sealed into a segment, one still in the buffer → the stream unions both.
        db.ingest_otlp_logs(&otlp_log("cart", "a", 10))
            .await
            .unwrap();
        db.ingest_otlp_logs(&otlp_log("cart", "b", 20))
            .await
            .unwrap();
        db.flush().await.unwrap();
        db.ingest_otlp_logs(&otlp_log("cart", "c", 30))
            .await
            .unwrap();

        // Collect the expected result for comparison, then build the stream and drop the DB handle.
        let expected = count(&db, "SELECT count(*) AS c FROM logs").await;
        let stream = db
            .sql("SELECT body FROM logs ORDER BY body")
            .stream()
            .await
            .unwrap();
        drop(db); // the stream must root everything it needs; only `dir` (the files) is kept alive.

        let drained = datafusion::physical_plan::common::collect(stream)
            .await
            .unwrap();
        let rows: i64 = drained.iter().map(|b| b.num_rows() as i64).sum();
        assert_eq!(
            rows, expected,
            "stream yields the same rows as collect (buffer ∪ segment)"
        );
        assert_eq!(rows, 3);
    }

    /// I-5: `Query::stream_with_stats` exposes the read-side scan counters. Because the scan is lazy
    /// (I-4a), they are **zero before the stream is polled** and complete only after it is fully
    /// drained — the counters accrue on the poll path, not at plan time.
    #[tokio::test(flavor = "current_thread")]
    async fn stream_with_stats_reports_scan_counters_after_drain() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::builder(dir.path()).open().unwrap();
        db.ingest_otlp_logs(&otlp_log("cart", "a", 10))
            .await
            .unwrap();
        db.ingest_otlp_logs(&otlp_log("cart", "b", 20))
            .await
            .unwrap();
        db.flush().await.unwrap(); // seal 2 rows into one segment
        db.ingest_otlp_logs(&otlp_log("cart", "c", 30))
            .await
            .unwrap(); // 1 row still in the buffer

        let (stream, stats) = db
            .sql("SELECT body FROM logs")
            .stream_with_stats()
            .await
            .unwrap();
        // Lazy: building the stream (and its plan) reads nothing yet.
        assert_eq!(
            stats.get().rows_scanned,
            0,
            "no rows read before the stream is polled"
        );

        let drained = datafusion::physical_plan::common::collect(stream)
            .await
            .unwrap();
        let rows: usize = drained.iter().map(|b| b.num_rows()).sum();
        assert_eq!(rows, 3);

        // After a full drain the counters are complete.
        let s = stats.get();
        assert_eq!(s.segments_scanned, 1, "one sealed segment read");
        assert_eq!(s.segments_pruned, 0);
        assert_eq!(s.rows_scanned, 3, "2 sealed + 1 buffered rows materialized");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn sealed_and_buffered_union() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::builder(dir.path()).open().unwrap();
        // Two records → seal → segment; then two more stay in the buffer.
        db.ingest_otlp_logs(&otlp_log("cart", "a", 10))
            .await
            .unwrap();
        db.ingest_otlp_logs(&otlp_log("checkout", "b", 20))
            .await
            .unwrap();
        db.flush().await.unwrap();
        assert_eq!(db.segments().len(), 1);
        db.ingest_otlp_logs(&otlp_log("cart", "c", 30))
            .await
            .unwrap();

        // Query sees buffer ∪ segments.
        assert_eq!(count(&db, "SELECT count(*) AS c FROM logs").await, 3);
        assert_eq!(
            count(&db, "SELECT count(*) AS c FROM logs WHERE service = 'cart'").await,
            2
        );

        // A grouped aggregation over the union.
        let out = db
            .sql("SELECT service, count(*) AS c FROM logs GROUP BY service ORDER BY service")
            .collect()
            .await
            .unwrap();
        // `service` is dict-encoded, so a GROUP BY surfaces it as a `DictionaryArray`; the shared
        // `get_str` helper reads either encoding.
        let services = out[0].column(0);
        assert_eq!(
            crate::logs::get_str(services.as_ref(), 0).as_deref(),
            Some("cart")
        );
        assert_eq!(
            crate::logs::get_str(services.as_ref(), 1).as_deref(),
            Some("checkout")
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn service_resource_scope_are_dict_encoded_and_survive_roundtrip() {
        use arrow::datatypes::DataType;

        // The three low-cardinality columns are `Dictionary(Int32, Utf8)` in every table schema
        // (ARCHITECTURE.md §6.2).
        let want = DataType::Dictionary(Box::new(DataType::Int32), Box::new(DataType::Utf8));
        let schema = imbh_storage::logs_schema(&[]);
        for col in ["service", "resource", "scope"] {
            let f = schema.field_with_name(col).unwrap();
            assert_eq!(f.data_type(), &want, "logs.{col} must be dict-encoded");
        }

        // Round-trip through a sealed segment ∪ the mutable buffer: the dict values must come back
        // as the original strings via `SELECT DISTINCT service`.
        let dir = tempfile::tempdir().unwrap();
        let db = Db::builder(dir.path()).open().unwrap();
        db.ingest_otlp_logs(&otlp_log("cart", "a", 10))
            .await
            .unwrap();
        db.ingest_otlp_logs(&otlp_log("checkout", "b", 20))
            .await
            .unwrap();
        db.flush().await.unwrap();
        assert_eq!(db.segments().len(), 1);
        // This one stays in the mutable buffer (dict-encoded batch, not yet sealed).
        db.ingest_otlp_logs(&otlp_log("payments", "c", 30))
            .await
            .unwrap();

        let out = db
            .sql("SELECT DISTINCT service FROM logs ORDER BY service")
            .collect()
            .await
            .unwrap();
        let col = out[0].column(0);
        // A DISTINCT over the dict column surfaces a `DictionaryArray`; `get_str` decodes it.
        let got: Vec<String> = (0..col.len())
            .filter_map(|i| crate::logs::get_str(col.as_ref(), i))
            .collect();
        assert_eq!(got, vec!["cart", "checkout", "payments"]);

        // The public discovery path (`attrs().values`) reads the same dict column across the union.
        let via_attrs = db.attrs().values("service.name").await.unwrap();
        assert_eq!(via_attrs, vec!["cart", "checkout", "payments"]);
    }

    /// One OTLP log carrying a record-level attribute `http.route`, plus the promoted `service.name`.
    fn otlp_log_with_route(service: &str, route: &str, body_text: &str, time: u64) -> Vec<u8> {
        use opentelemetry_proto::tonic::collector::logs::v1::ExportLogsServiceRequest;
        use opentelemetry_proto::tonic::common::v1::{AnyValue as PbAny, KeyValue, any_value};
        use opentelemetry_proto::tonic::logs::v1::{LogRecord, ResourceLogs, ScopeLogs};
        use opentelemetry_proto::tonic::resource::v1::Resource;
        use prost::Message;

        let sv = |s: &str| PbAny {
            value: Some(any_value::Value::StringValue(s.to_owned())),
        };
        let kv = |k: &str, v: &str| KeyValue {
            key: k.to_owned(),
            value: Some(sv(v)),
            ..Default::default()
        };
        ExportLogsServiceRequest {
            resource_logs: vec![ResourceLogs {
                resource: Some(Resource {
                    attributes: vec![kv("service.name", service)],
                    ..Default::default()
                }),
                scope_logs: vec![ScopeLogs {
                    log_records: vec![LogRecord {
                        time_unix_nano: time,
                        severity_number: 9,
                        body: Some(sv(body_text)),
                        attributes: vec![kv("http.route", route)],
                        ..Default::default()
                    }],
                    ..Default::default()
                }],
                ..Default::default()
            }],
        }
        .encode_to_vec()
    }

    /// A promoted attribute key (ARCHITECTURE.md §6.1) becomes a real typed column readable via SQL —
    /// across both a sealed segment and the live buffer — while still resolving through `json_get_str`
    /// because the key stays in the canonical-JSON blob (the keep-in-JSON decision).
    #[tokio::test(flavor = "current_thread")]
    async fn promoted_attribute_is_a_typed_column_and_stays_in_json() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::builder(dir.path())
            .promote(Promote::new(["http.route"]))
            .open()
            .unwrap();

        // First record → sealed into a segment (Parquet round-trip of the promoted column).
        db.ingest_otlp_logs(&otlp_log_with_route("cart", "/api/cart", "a", 10))
            .await
            .unwrap();
        db.flush().await.unwrap();
        assert_eq!(db.segments().len(), 1);
        // Second record → stays in the live buffer (buffer ∪ segment union carries the column).
        db.ingest_otlp_logs(&otlp_log_with_route("checkout", "/api/checkout", "b", 20))
            .await
            .unwrap();

        // The promoted column exists and is populated from both the segment and the buffer.
        let out = db
            .sql("SELECT \"http.route\" AS r FROM logs ORDER BY \"time\"")
            .collect()
            .await
            .unwrap();
        let col = out[0].column(0);
        let routes: Vec<String> = (0..col.len())
            .filter_map(|i| crate::logs::get_str(col.as_ref(), i))
            .collect();
        assert_eq!(routes, vec!["/api/cart", "/api/checkout"]);

        // The key ALSO remains inside the JSON blob → `json_get_str` still resolves it unchanged.
        let via_json = db
            .sql("SELECT json_get_str(attributes, 'http.route') AS r FROM logs ORDER BY \"time\"")
            .collect()
            .await
            .unwrap();
        let jcol = via_json[0].column(0);
        let jroutes: Vec<String> = (0..jcol.len())
            .filter_map(|i| crate::logs::get_str(jcol.as_ref(), i))
            .collect();
        assert_eq!(jroutes, vec!["/api/cart", "/api/checkout"]);

        // The promoted column is dict-encoded like `service` (ARCHITECTURE.md §6.1).
        use arrow::datatypes::DataType;
        let f = imbh_storage::logs_schema(&["http.route".to_owned()])
            .field_with_name("http.route")
            .unwrap()
            .clone();
        assert_eq!(
            f.data_type(),
            &DataType::Dictionary(Box::new(DataType::Int32), Box::new(DataType::Utf8))
        );
    }

    /// Stage 3 dispatch: a typed `attr_eq` filter on a **promoted** key returns exactly the same
    /// rows as the same filter on a DB with **no** promotion (which uses the `json_get_str` scan).
    /// Because the promoted column mirrors the record `attributes` scope and the key stays in JSON,
    /// the column-pushdown path and the JSON-scan path are provably equivalent (ARCHITECTURE.md §6.1).
    #[tokio::test(flavor = "current_thread")]
    async fn promoted_key_filter_matches_the_json_scan_result() {
        async fn routes_matching(promote: Option<&str>, filter_route: &str) -> Vec<String> {
            let dir = tempfile::tempdir().unwrap();
            let mut b = Db::builder(dir.path());
            if let Some(k) = promote {
                b = b.promote(Promote::new([k]));
            }
            let db = b.open().unwrap();
            // Three records: two on /api/cart (one sealed, one buffered), one on /api/checkout.
            db.ingest_otlp_logs(&otlp_log_with_route("cart", "/api/cart", "a", 10))
                .await
                .unwrap();
            db.ingest_otlp_logs(&otlp_log_with_route("cart", "/api/checkout", "b", 20))
                .await
                .unwrap();
            db.flush().await.unwrap();
            db.ingest_otlp_logs(&otlp_log_with_route("cart", "/api/cart", "c", 30))
                .await
                .unwrap();

            let page = db
                .logs()
                .query(LogQuery::new().attr_eq("http.route", filter_route))
                .await
                .unwrap();
            let mut bodies: Vec<String> = page.entries.iter().map(|e| e.body.clone()).collect();
            bodies.sort();
            bodies
        }

        let promoted = routes_matching(Some("http.route"), "/api/cart").await;
        let json_scan = routes_matching(None, "/api/cart").await;
        // Both must select the two /api/cart rows ("a" and "c"), identically.
        assert_eq!(promoted, vec!["a".to_owned(), "c".to_owned()]);
        assert_eq!(
            promoted, json_scan,
            "promoted-column pushdown must equal the json_get_str scan"
        );
    }

    /// A DB with no promotion is byte-for-byte the old behavior: the fixed schema, no extra columns.
    #[tokio::test(flavor = "current_thread")]
    async fn no_promotion_leaves_the_schema_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::builder(dir.path()).open().unwrap();
        db.ingest_otlp_logs(&otlp_log_with_route("cart", "/api/cart", "a", 10))
            .await
            .unwrap();
        let out = db.sql("SELECT * FROM logs").collect().await.unwrap();
        // The 12 fixed `logs` columns, no promoted extras.
        assert_eq!(out[0].num_columns(), 12);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn reopen_recovers_segments() {
        let dir = tempfile::tempdir().unwrap();
        {
            let db = Db::builder(dir.path()).open().unwrap();
            db.ingest_otlp_logs(&otlp_log("cart", "a", 10))
                .await
                .unwrap();
            db.flush().await.unwrap();
        }
        let db2 = Db::builder(dir.path()).open().unwrap();
        assert_eq!(db2.segments().len(), 1);
        assert_eq!(count(&db2, "SELECT count(*) AS c FROM logs").await, 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn full_text_matches_in_sql() {
        let db = Db::in_memory().open().unwrap();
        db.ingest_otlp_logs(&otlp_log("cart", "connection error timeout", 1))
            .await
            .unwrap();
        db.ingest_otlp_logs(&otlp_log("cart", "request ok", 2))
            .await
            .unwrap();

        let m = |q: &'static str| {
            let db = db.clone();
            async move {
                count(
                    &db,
                    &format!("SELECT count(*) AS c FROM logs WHERE matches(body, '{q}')"),
                )
                .await
            }
        };
        assert_eq!(m("error").await, 1);
        assert_eq!(m("timeout error").await, 1); // implicit AND, order-free
        assert_eq!(m("refused").await, 0);
        assert_eq!(m("err").await, 0); // substring is not a term match
    }

    #[tokio::test(flavor = "current_thread")]
    async fn matches_over_sealed_segment_and_buffer() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::builder(dir.path()).open().unwrap();
        for (svc, body, t) in [
            ("cart", "connection error", 1u64),
            ("cart", "request ok", 2),
            ("checkout", "upstream timeout", 3),
            ("checkout", "request ok", 4),
        ] {
            db.ingest_otlp_logs(&otlp_log(svc, body, t)).await.unwrap();
        }
        db.flush().await.unwrap(); // seal → Parquet segment + .tidx sidecar
        assert_eq!(db.segments().len(), 1);

        let m = |q: &'static str| {
            let db = db.clone();
            async move {
                count(
                    &db,
                    &format!("SELECT count(*) AS c FROM logs WHERE matches(body, '{q}')"),
                )
                .await
            }
        };
        // Selective query → index RowSelection; less-selective → full scan; both correct.
        assert_eq!(m("error").await, 1);
        assert_eq!(m("timeout").await, 1);
        assert_eq!(m("request").await, 2);
        assert_eq!(m("absent").await, 0);

        // Add an unsealed buffered row: the union sees both.
        db.ingest_otlp_logs(&otlp_log("cart", "another error here", 5))
            .await
            .unwrap();
        assert_eq!(m("error").await, 2);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn query_stats_report_index_pruning() {
        // Index pruning must be observable in production `QueryStats`, not only in provider tests.
        let dir = tempfile::tempdir().unwrap();
        let db = Db::builder(dir.path()).open().unwrap();
        for (body, t) in [
            ("connection error", 1u64),
            ("request ok", 2),
            ("upstream ok", 3),
            ("cache ok", 4),
        ] {
            db.ingest_otlp_logs(&otlp_log("cart", body, t))
                .await
                .unwrap();
        }
        db.flush().await.unwrap(); // seal → one Parquet segment + `.tidx`
        assert_eq!(db.segments().len(), 1);

        // A selective full-text query: only 1 of 4 rows contains "error", so the `.tidx` RowSelection
        // reads just that row.
        let page = db
            .logs()
            .query(LogQuery::new().matches("error"))
            .await
            .unwrap();
        assert_eq!(page.entries.len(), 1);
        assert!(page.stats.used_index, "the sealed `.tidx` was consulted");
        assert_eq!(page.stats.segments_scanned, 1);
        assert_eq!(page.stats.segments_pruned, 0); // no bloom columns on logs
        assert_eq!(
            page.stats.rows_scanned, 1,
            "the RowSelection pruned the segment to the single matching row"
        );
        assert!(page.stats.bytes_scanned > 0);
    }

    /// The `StringPredicate::Matches` term predicate (what the LogQL `|?` dialect operator lowers to)
    /// renders `matches(body, ?)` and is pushed to the sealed `.tidx`, exactly like `LogQuery::matches`.
    /// Its `NotMatches` twin (`!?`) is the exact complement. Term semantics, not substring: `err` does
    /// not match `error`.
    #[tokio::test(flavor = "current_thread")]
    async fn matches_string_predicate_is_index_accelerated() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::builder(dir.path()).open().unwrap();
        for (body, t) in [
            ("connection error", 1u64),
            ("upstream timeout", 2),
            ("request ok", 3),
            ("cache ok", 4),
        ] {
            db.ingest_otlp_logs(&otlp_log("cart", body, t))
                .await
                .unwrap();
        }
        db.flush().await.unwrap();
        assert_eq!(db.segments().len(), 1);

        // `|?` term match: only 1 of 4 rows has the token `timeout` → the `.tidx` prunes to that row.
        let hit = db
            .logs()
            .query(LogQuery::new().string_predicate(
                LogStringField::Body,
                StringPredicate::Matches,
                "timeout",
            ))
            .await
            .unwrap();
        assert_eq!(hit.entries.len(), 1);
        assert_eq!(hit.entries[0].body, "upstream timeout");
        assert!(hit.stats.used_index, "the sealed `.tidx` was consulted");
        assert_eq!(hit.stats.rows_scanned, 1, "RowSelection pruned to one row");

        // Term, not substring: `err` is not a token of `error`.
        let none = db
            .logs()
            .query(LogQuery::new().string_predicate(
                LogStringField::Body,
                StringPredicate::Matches,
                "err",
            ))
            .await
            .unwrap();
        assert_eq!(none.entries.len(), 0);

        // `!?` is the exact complement of `|?`: the three rows without the `timeout` token.
        let rest = db
            .logs()
            .query(LogQuery::new().string_predicate(
                LogStringField::Body,
                StringPredicate::NotMatches,
                "timeout",
            ))
            .await
            .unwrap();
        assert_eq!(rest.entries.len(), 3);
        assert!(
            rest.entries
                .iter()
                .all(|entry| entry.body != "upstream timeout")
        );
        assert!(
            rest.stats.used_index,
            "the `.tidx` was consulted for `!?` too"
        );
    }

    /// Attr-equality pushdown: `attr_eq("k","v1")` returns only the `v1` rows, correct across a
    /// sealed segment (where the `.tidx` `attrs` field drives a `RowSelection`) unioned with the
    /// buffer. The non-matching `v2` rows are pruned from the sealed read yet the result is exact,
    /// because `json_get_str(attributes,'k') = 'v1'` is re-applied above the scan. A dotted key
    /// (`http.route`) exercises the JSON-path escaping. Pruning is not directly observable (no
    /// RowSelection counter); the imbh-index/imbh-query suites assert the selection is the exact
    /// matching subset, and this test locks in end-to-end correctness over the sealed+buffered union.
    #[tokio::test(flavor = "current_thread")]
    async fn attr_eq_prunes_over_sealed_segment_and_buffer() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::builder(dir.path()).open().unwrap();
        // Sealed rows: three v1, two v2 (with a distinct route on each `k`).
        for (k, route, t) in [
            ("v1", "/cart", 1u64),
            ("v2", "/pay", 2),
            ("v1", "/cart", 3),
            ("v2", "/pay", 4),
            ("v1", "/home", 5),
        ] {
            let body = format!("req {t}");
            db.ingest_otlp_logs(&otlp_rich(
                "cart",
                &body,
                t,
                9,
                &[("k", k), ("http.route", route)],
            ))
            .await
            .unwrap();
        }
        db.flush().await.unwrap(); // seal → Parquet segment + .tidx with the `attrs` field
        assert_eq!(db.segments().len(), 1);

        let eq_count = |key: &'static str, val: &'static str| {
            let db = db.clone();
            async move {
                count(
                    &db,
                    &format!(
                        "SELECT count(*) AS c FROM logs \
                         WHERE json_get_str(attributes, '{key}') = '{val}'"
                    ),
                )
                .await
            }
        };
        // Sealed-only: exact matches, non-matching rows pruned.
        assert_eq!(eq_count("k", "v1").await, 3);
        assert_eq!(eq_count("k", "v2").await, 2);
        assert_eq!(eq_count("k", "absent").await, 0);
        // Dotted key routes through the JSON-path escaping.
        assert_eq!(eq_count("http.route", "/cart").await, 2);
        assert_eq!(eq_count("http.route", "/home").await, 1);

        // The typed builder API (compiles to the same pushed predicate) returns only v1 rows.
        let page = db
            .logs()
            .query(LogQuery::new().attr_eq("k", "v1"))
            .await
            .unwrap();
        assert_eq!(page.entries.len(), 3);
        assert!(
            page.entries
                .iter()
                .all(|e| e.attributes.get_str("k") == Some("v1"))
        );

        // Add an unsealed buffered v1 row: the buffer ∪ sealed-segment union counts it too.
        db.ingest_otlp_logs(&otlp_rich(
            "cart",
            "req 6",
            6,
            9,
            &[("k", "v1"), ("http.route", "/cart")],
        ))
        .await
        .unwrap();
        assert_eq!(eq_count("k", "v1").await, 4);
        assert_eq!(eq_count("k", "v2").await, 2);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn wal_recovers_unsealed_records() {
        let dir = tempfile::tempdir().unwrap();
        {
            let db = Db::builder(dir.path()).wal(WalMode::Always).open().unwrap();
            let r = db
                .ingest_otlp_logs(&otlp_log("cart", "boom", 1))
                .await
                .unwrap();
            assert!(r.durable);
            assert_eq!(db.durable_through().await, r.lsn);
            // No flush → drop, simulating a crash with an unsealed buffer.
        }
        // Reopen: nothing was sealed, but the WAL replays the record into the buffer.
        let db2 = Db::builder(dir.path()).open().unwrap();
        assert_eq!(db2.segments().len(), 0);
        assert_eq!(count(&db2, "SELECT count(*) AS c FROM logs").await, 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn background_maintenance_auto_seals() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::builder(dir.path())
            .maintenance(Maintenance::Background(std::time::Duration::from_millis(
                20,
            )))
            .open()
            .unwrap();
        db.ingest_otlp_logs(&otlp_log("cart", "x", 1))
            .await
            .unwrap();

        // The scheduler runs on its own thread; poll (with a timeout) for it to seal a segment.
        let mut sealed = false;
        for _ in 0..200 {
            if !db.segments().is_empty() {
                sealed = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(
            sealed,
            "background maintenance did not seal within the timeout"
        );
        assert_eq!(count(&db, "SELECT count(*) AS c FROM logs").await, 1);

        // Clean shutdown: close() joins the maintenance thread — it returns (does not hang on the
        // join), is idempotent, and no maintenance runs afterward (ops are rejected as closed).
        db.close().await.unwrap();
        db.close().await.unwrap();
        assert!(matches!(
            db.logs().count(LogQuery::new()).await,
            Err(Error::Closed)
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn background_maintenance_byte_threshold_seals() {
        // A long interval means the timer-driven seal cannot fire during the test — any segment we
        // observe was produced by the per-tick byte-threshold seal. A tiny budget floors the seal
        // threshold at DEFAULT_SEAL_BYTES (8 MiB), so ~9 MiB of buffered rows crosses it.
        let dir = tempfile::tempdir().unwrap();
        let db = Db::builder(dir.path())
            .memory_budget(MemoryBudget::total(1))
            .maintenance(Maintenance::Background(std::time::Duration::from_secs(
                3600,
            )))
            .open()
            .unwrap();
        let big = "x".repeat(1 << 20); // ~1 MiB body per record.
        for i in 0..9u64 {
            db.ingest_otlp_logs(&otlp_log("cart", &big, i + 1))
                .await
                .unwrap();
        }

        // The scheduler (its own thread) wakes each tick (min(interval,1s)=1s); poll — well inside
        // the 3600s interval — for the byte-threshold seal to materialize a segment.
        let mut sealed = false;
        for _ in 0..300 {
            if !db.segments().is_empty() {
                sealed = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(
            sealed,
            "byte-threshold seal did not fire before the interval elapsed"
        );
        assert_eq!(count(&db, "SELECT count(*) AS c FROM logs").await, 9);
        db.close().await.unwrap();
    }

    /// Poll (with a timeout) for a condition the background scheduler is expected to bring about. The
    /// `Background` loop runs on its own OS thread, so a thread sleep is the right wait even inside a
    /// current-thread runtime — nothing here needs the test's runtime to be free.
    fn wait_until(timeout: std::time::Duration, mut done: impl FnMut() -> bool) -> bool {
        let step = std::time::Duration::from_millis(10);
        let mut waited = std::time::Duration::ZERO;
        while waited < timeout {
            if done() {
                return true;
            }
            std::thread::sleep(step);
            waited += step;
        }
        done()
    }

    fn wait_for_segment(db: &Arc<Db>, timeout: std::time::Duration) -> bool {
        wait_until(timeout, || !db.segments().is_empty())
    }

    /// A long maintenance interval means the *retention* clock cannot seal anything during the test;
    /// every segment observed below is the flush policy's doing. 30s of slack is generous for a
    /// millisecond-scale trigger while still failing rather than hanging.
    const SLOW_MAINTENANCE: std::time::Duration = std::time::Duration::from_secs(3600);
    const SEAL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

    /// The size-based strategy with an *explicit* threshold: a handful of small rows seals, where the
    /// budget-derived default would have needed megabytes.
    #[tokio::test(flavor = "current_thread")]
    async fn flush_policy_size_trigger_seals_a_small_buffer() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::builder(dir.path())
            .maintenance(Maintenance::Background(SLOW_MAINTENANCE))
            .flush(FlushPolicy::size_based(512).tick(std::time::Duration::from_millis(10)))
            .open()
            .unwrap();
        db.ingest_otlp_logs(&otlp_log("cart", &"x".repeat(600), 1))
            .await
            .unwrap();
        assert!(
            wait_for_segment(&db, SEAL_TIMEOUT),
            "explicit byte threshold did not seal"
        );
        assert_eq!(count(&db, "SELECT count(*) AS c FROM logs").await, 1);
        db.close().await.unwrap();
    }

    /// The periodic strategy, on the *policy's* clock rather than the maintenance interval — the two
    /// cadences are independent, so a 3600s retention interval does not slow sealing down.
    #[tokio::test(flavor = "current_thread")]
    async fn flush_policy_periodic_trigger_is_independent_of_retention() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::builder(dir.path())
            .maintenance(Maintenance::Background(SLOW_MAINTENANCE))
            .flush(FlushPolicy::periodic(std::time::Duration::from_millis(20)))
            .open()
            .unwrap();
        db.ingest_otlp_logs(&otlp_log("cart", "x", 1))
            .await
            .unwrap();
        assert!(
            wait_for_segment(&db, SEAL_TIMEOUT),
            "periodic trigger did not seal inside the retention interval"
        );
        assert_eq!(count(&db, "SELECT count(*) AS c FROM logs").await, 1);
        db.close().await.unwrap();
    }

    /// The row-count strategy: seal once N rows are buffered, whatever they weigh.
    #[tokio::test(flavor = "current_thread")]
    async fn flush_policy_row_trigger_seals_at_the_row_count() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::builder(dir.path())
            .maintenance(Maintenance::Background(SLOW_MAINTENANCE))
            .flush(
                FlushPolicy::manual()
                    .at_buffer_rows(3)
                    .tick(std::time::Duration::from_millis(10)),
            )
            .open()
            .unwrap();
        // Two rows is under the threshold; give the scheduler ticks to prove it stays put.
        for i in 1..=2u64 {
            db.ingest_otlp_logs(&otlp_log("cart", "x", i))
                .await
                .unwrap();
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert!(
            db.segments().is_empty(),
            "sealed below the configured row count"
        );
        db.ingest_otlp_logs(&otlp_log("cart", "x", 3))
            .await
            .unwrap();
        assert!(
            wait_for_segment(&db, SEAL_TIMEOUT),
            "row-count trigger did not seal"
        );
        assert_eq!(count(&db, "SELECT count(*) AS c FROM logs").await, 3);
        db.close().await.unwrap();
    }

    /// The WAL-size strategy: sealing is what lets the WAL be reclaimed, so this trigger is how a
    /// long-running writer bounds WAL growth on disk.
    #[tokio::test(flavor = "current_thread")]
    async fn flush_policy_wal_trigger_seals_and_reclaims_the_wal() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::builder(dir.path())
            .maintenance(Maintenance::Background(SLOW_MAINTENANCE))
            .flush(
                FlushPolicy::manual()
                    .at_wal_bytes(4096)
                    .tick(std::time::Duration::from_millis(10)),
            )
            .open()
            .unwrap();
        // Each record's raw OTLP payload goes into the WAL, so a few KiB of bodies crosses 4 KiB.
        let big = "x".repeat(2048);
        for i in 1..=4u64 {
            db.ingest_otlp_logs(&otlp_log("cart", &big, i))
                .await
                .unwrap();
        }
        assert!(
            wait_for_segment(&db, SEAL_TIMEOUT),
            "WAL-size trigger did not seal"
        );
        assert_eq!(count(&db, "SELECT count(*) AS c FROM logs").await, 4);
        // Each seal advances the watermark, so the covered WAL segments are reclaimed — the WAL has to
        // fall back under the trigger, or it would fire forever. The scheduler may have sealed
        // part-way through the ingest loop, so this converges over a tick or two rather than at once.
        assert!(
            wait_until(SEAL_TIMEOUT, || db.storage.wal_bytes() < 4096),
            "sealing must let the WAL shrink, else the trigger would fire forever"
        );
        db.close().await.unwrap();
    }

    /// The idle strategy: a quiet workload's tail lands in Parquet without a short periodic timer, and
    /// an empty buffer never asks for a no-op seal.
    #[tokio::test(flavor = "current_thread")]
    async fn flush_policy_idle_trigger_seals_after_quiet() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::builder(dir.path())
            .maintenance(Maintenance::Background(SLOW_MAINTENANCE))
            .flush(FlushPolicy::manual().after_idle(std::time::Duration::from_millis(50)))
            .open()
            .unwrap();
        // Nothing ingested: the idle window passes with an empty buffer and nothing is sealed.
        std::thread::sleep(std::time::Duration::from_millis(150));
        assert!(db.segments().is_empty(), "no rows, no segment");

        db.ingest_otlp_logs(&otlp_log("cart", "x", 1))
            .await
            .unwrap();
        assert!(
            wait_for_segment(&db, SEAL_TIMEOUT),
            "idle trigger did not seal the tail"
        );
        assert_eq!(count(&db, "SELECT count(*) AS c FROM logs").await, 1);
        db.close().await.unwrap();
    }

    /// `FlushPolicy::manual()` is honored as written: the scheduler still runs (retention, WAL fsync)
    /// but never seals on its own, even though the maintenance interval is short.
    #[tokio::test(flavor = "current_thread")]
    async fn flush_policy_manual_never_auto_seals() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::builder(dir.path())
            .maintenance(Maintenance::Background(std::time::Duration::from_millis(
                10,
            )))
            .flush(FlushPolicy::manual())
            .open()
            .unwrap();
        db.ingest_otlp_logs(&otlp_log("cart", "x", 1))
            .await
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert!(
            db.segments().is_empty(),
            "an explicit manual policy must not inherit the maintenance interval"
        );
        // The rows are still queryable from the buffer, and an explicit flush still seals.
        assert_eq!(count(&db, "SELECT count(*) AS c FROM logs").await, 1);
        db.flush().await.unwrap();
        assert_eq!(db.segments().len(), 1);
        db.close().await.unwrap();
    }

    /// `WalMode::Interval(d)` is a promise the scheduler keeps: with one running, the durable watermark
    /// advances on the timer, with no `flush()`/`close()` and no per-ingest fsync.
    #[tokio::test(flavor = "current_thread")]
    async fn wal_interval_is_fsynced_by_the_scheduler() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::builder(dir.path())
            .wal(WalMode::Interval(std::time::Duration::from_millis(20)))
            // Manual flushing keeps the seal path out of it: any durability we observe came from the
            // WAL fsync timer, not from a segment write advancing the watermark.
            .flush(FlushPolicy::manual())
            .maintenance(Maintenance::Background(SLOW_MAINTENANCE))
            .open()
            .unwrap();
        let receipt = db
            .ingest_otlp_logs(&otlp_log("cart", "x", 1))
            .await
            .unwrap();
        assert!(!receipt.durable, "interval mode does not fsync inline");

        let mut durable = None;
        for _ in 0..300 {
            durable = db.durable_through().await;
            if durable.is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(
            durable.map(|l| l.get()),
            Some(1),
            "the scheduler never fsynced an interval-mode WAL"
        );
        assert!(db.segments().is_empty(), "the fsync timer must not seal");
        db.close().await.unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn runtime_maintenance_auto_seals() {
        use tokio::runtime::Handle;
        // Schedule maintenance onto the test's own runtime (no owned OS thread). A tiny budget
        // floors the seal threshold at 8 MiB; ~9 MiB of buffered rows trips the per-tick
        // byte-threshold seal on the async loop.
        let dir = tempfile::tempdir().unwrap();
        let db = Db::builder(dir.path())
            .memory_budget(MemoryBudget::total(1))
            .maintenance(Maintenance::Runtime(
                Handle::current(),
                std::time::Duration::from_secs(30),
            ))
            .open()
            .unwrap();
        let big = "x".repeat(1 << 20);
        for i in 0..9u64 {
            db.ingest_otlp_logs(&otlp_log("cart", &big, i + 1))
                .await
                .unwrap();
        }

        // Poll with ASYNC sleeps so the maintenance task (on this same single-threaded runtime) is
        // scheduled and can run its seal.
        let mut sealed = false;
        for _ in 0..300 {
            if !db.segments().is_empty() {
                sealed = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        assert!(sealed, "runtime-scheduled maintenance did not seal");
        assert_eq!(count(&db, "SELECT count(*) AS c FROM logs").await, 9);

        // Clean async shutdown: close() awaits the maintenance task, is idempotent, and no
        // maintenance runs afterward (ops are then rejected as closed).
        db.close().await.unwrap();
        db.close().await.unwrap();
        assert!(matches!(
            db.logs().count(LogQuery::new()).await,
            Err(Error::Closed)
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn maintain_seals_then_applies_retention() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::builder(dir.path())
            .retention(Retention::none().max_disk_bytes(0))
            .open()
            .unwrap();
        db.ingest_otlp_logs(&otlp_log("cart", "a", 1))
            .await
            .unwrap();

        let report = db.maintain().await.unwrap();
        assert!(report.sealed); // buffer was sealed into a segment
        assert_eq!(report.segments_dropped, 1); // then retention (budget 0) dropped it
        assert!(report.bytes_freed > 0);
        assert_eq!(db.segments().len(), 0);
        assert_eq!(count(&db, "SELECT count(*) AS c FROM logs").await, 0);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn closed_db_rejects_ops() {
        let db = Db::in_memory().open().unwrap();
        db.close().await.unwrap();
        assert!(matches!(
            db.ingest_otlp_logs(&otlp_log("x", "y", 1)).await,
            Err(Error::Closed)
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn bad_sql_error_is_diagnosable() {
        // A query against a non-existent table fails in planning. The typed error (§10.3) must
        // (a) classify as a user error (HTTP 400), (b) chain the underlying DataFusion cause via
        // `source()`, and (c) render that cause in `Display` so the reference server's JSON body
        // carries it — the diagnosability the old string-payload error dropped.
        let db = Db::in_memory().open().unwrap();
        let err = db
            .sql("SELECT * FROM no_such_table")
            .collect()
            .await
            .unwrap_err();
        assert!(err.is_user_error(), "bad SQL is a 400 user error: {err}");
        assert!(
            std::error::Error::source(&err).is_some(),
            "the DataFusion cause is chained as source(): {err}"
        );
        let display = err.to_string();
        assert!(
            display.starts_with("query error: plan"),
            "display: {display}"
        );
        assert!(
            display.contains("no_such_table"),
            "underlying detail surfaced in Display: {display}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn parameterized_query_spike() {
        use arrow::array::Int64Array;
        let db = Db::in_memory().open().unwrap();
        db.ingest_otlp_logs(&otlp_log("cart", "error alpha", 1000))
            .await
            .unwrap();

        let count1 = |batches: Vec<RecordBatch>| -> i64 {
            batches[0]
                .column(0)
                .as_any()
                .downcast_ref::<Int64Array>()
                .unwrap()
                .value(0)
        };

        // `$N` as a UDF arg (`matches`), a dict-encoded column equality (`service`), and an integer
        // time bound must all resolve placeholder types at plan time and run.
        let b = db
            .sql_with_params(
                "SELECT count(*) AS c FROM logs WHERE service = $1 AND matches(body, $2) \
                 AND CAST(\"time\" AS BIGINT) >= $3"
                    .to_owned(),
                vec![
                    ScalarValue::Utf8(Some("cart".into())),
                    ScalarValue::Utf8(Some("error".into())),
                    ScalarValue::Int64(Some(0)),
                ],
            )
            .collect()
            .await
            .unwrap();
        assert_eq!(count1(b), 1, "service/matches/time-bound params");

        // `$N` inside `json_get_str` and `regexp_like` (the hardest placeholder positions — the type
        // must flow from the function signature).
        let b = db
            .sql_with_params(
                "SELECT count(*) AS c FROM logs WHERE json_get_str(attributes, $1) = $2 \
                 OR regexp_like(body, $3)"
                    .to_owned(),
                vec![
                    ScalarValue::Utf8(Some("k".into())),
                    ScalarValue::Utf8(Some("v".into())),
                    ScalarValue::Utf8(Some("^error".into())),
                ],
            )
            .collect()
            .await
            .unwrap();
        assert_eq!(count1(b), 1, "json_get_str/regexp_like params");
    }

    // ── duplicate-timestamp ingest guard (issue #27) ────────────────────────────────────────────

    /// A gauge with one point per `(time, value)`, so a caller can put two on the same instant.
    fn otlp_gauge_points(service: &str, metric: &str, points: &[(u64, f64)]) -> Vec<u8> {
        use opentelemetry_proto::tonic::collector::metrics::v1::ExportMetricsServiceRequest;
        use opentelemetry_proto::tonic::common::v1::{AnyValue as PbAny, KeyValue, any_value};
        use opentelemetry_proto::tonic::metrics::v1::{
            Gauge, Metric, NumberDataPoint, ResourceMetrics, ScopeMetrics, metric,
            number_data_point,
        };
        use opentelemetry_proto::tonic::resource::v1::Resource;
        use prost::Message;

        ExportMetricsServiceRequest {
            resource_metrics: vec![ResourceMetrics {
                resource: Some(Resource {
                    attributes: vec![KeyValue {
                        key: "service.name".to_owned(),
                        value: Some(PbAny {
                            value: Some(any_value::Value::StringValue(service.to_owned())),
                        }),
                        ..Default::default()
                    }],
                    ..Default::default()
                }),
                scope_metrics: vec![ScopeMetrics {
                    metrics: vec![Metric {
                        name: metric.to_owned(),
                        unit: "1".to_owned(),
                        data: Some(metric::Data::Gauge(Gauge {
                            data_points: points
                                .iter()
                                .map(|(t, v)| NumberDataPoint {
                                    time_unix_nano: *t,
                                    value: Some(number_data_point::Value::AsDouble(*v)),
                                    ..Default::default()
                                })
                                .collect(),
                        })),
                        ..Default::default()
                    }],
                    ..Default::default()
                }],
                ..Default::default()
            }],
        }
        .encode_to_vec()
    }

    async fn gauge_rows(db: &Arc<Db>) -> i64 {
        count(db, "SELECT count(*) AS c FROM metrics_gauge").await
    }

    #[tokio::test(flavor = "current_thread")]
    async fn duplicates_default_accepts_repeated_timestamps() {
        // Locks "ingest rejection is opt-in": the default policy must behave exactly as it always
        // has, taking every point and reporting nothing rejected.
        let db = Db::in_memory().open().unwrap();
        let body = otlp_gauge_points("cart", "m", &[(10, 1.0)]);
        for _ in 0..2 {
            let r = db.ingest_otlp_metrics(&body).await.unwrap();
            assert_eq!((r.accepted, r.rejected), (1, 0));
        }
        assert_eq!(gauge_rows(&db).await, 2);
        assert_eq!(db.stats().await.unwrap().ingest_rejected, 0);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn duplicates_reject_drops_the_repeat_within_and_across_exports() {
        let db = Db::in_memory()
            .duplicates(Duplicates::reject())
            .open()
            .unwrap();

        // Within one export.
        let r = db
            .ingest_otlp_metrics(&otlp_gauge_points("cart", "m", &[(10, 1.0), (10, 5.0)]))
            .await
            .unwrap();
        assert_eq!((r.accepted, r.rejected), (1, 1));

        // Across exports — the case issue #27 actually hit, minutes of LSNs apart.
        let r = db
            .ingest_otlp_metrics(&otlp_gauge_points("cart", "m", &[(10, 1.0)]))
            .await
            .unwrap();
        assert_eq!((r.accepted, r.rejected), (0, 1));

        assert_eq!(gauge_rows(&db).await, 1);
        assert_eq!(db.stats().await.unwrap().ingest_rejected, 2);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn duplicates_reject_keeps_distinct_timestamps_and_label_sets() {
        let db = Db::in_memory()
            .duplicates(Duplicates::reject())
            .open()
            .unwrap();
        let r = db
            .ingest_otlp_metrics(&otlp_gauge_points("cart", "m", &[(10, 1.0), (11, 1.0)]))
            .await
            .unwrap();
        assert_eq!((r.accepted, r.rejected), (2, 0));
        // Two label sets under one metric name and one timestamp: distinct series, both kept.
        let r = db
            .ingest_otlp_metrics(&otlp_gauge_labeled("cart", "n", "pod", &["a", "b"]))
            .await
            .unwrap();
        assert_eq!((r.accepted, r.rejected), (2, 0));
        assert_eq!(gauge_rows(&db).await, 4);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn duplicates_reject_accepts_out_of_order_points() {
        // The boundary against a per-series `last_timestamp` rule: only an exact (series, timestamp)
        // repeat is a duplicate, so backfill and multi-producer clock skew keep working.
        let db = Db::in_memory()
            .duplicates(Duplicates::reject())
            .open()
            .unwrap();
        db.ingest_otlp_metrics(&otlp_gauge_points("cart", "m", &[(200, 1.0)]))
            .await
            .unwrap();
        let r = db
            .ingest_otlp_metrics(&otlp_gauge_points("cart", "m", &[(100, 1.0)]))
            .await
            .unwrap();
        assert_eq!((r.accepted, r.rejected), (1, 0));
        assert_eq!(gauge_rows(&db).await, 2);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn duplicates_reject_separates_the_gauge_and_sum_instrument_kinds() {
        // A false rejection is silent data loss, so a legitimate sum point must never be dropped
        // because a same-named gauge existed. The resulting PromQL duplicate is the read side's job.
        let db = Db::in_memory()
            .duplicates(Duplicates::reject())
            .open()
            .unwrap();
        db.ingest_otlp_metrics(&otlp_gauge_points("cart", "m", &[(10, 1.0)]))
            .await
            .unwrap();
        let r = db
            .ingest_otlp_metrics(&otlp_sum("cart", "m", 2, &[(10, 9.0)]))
            .await
            .unwrap();
        assert_eq!((r.accepted, r.rejected), (1, 0));
        assert_eq!(gauge_rows(&db).await, 1);
        assert_eq!(count(&db, "SELECT count(*) AS c FROM metrics_sum").await, 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn duplicates_reject_covers_every_metric_row_kind() {
        let db = Db::in_memory()
            .duplicates(Duplicates::reject())
            .open()
            .unwrap();
        let bodies = [
            (
                "metrics_gauge",
                otlp_gauge_points("cart", "g", &[(10, 1.0)]),
            ),
            ("metrics_sum", otlp_sum("cart", "s", 2, &[(10, 1.0)])),
            (
                "metrics_histogram",
                otlp_histogram("cart", "h", 10, 3, 6.0, &[1.0, 2.0], &[1, 1, 1]),
            ),
            (
                "metrics_exp_histogram",
                otlp_exp_histogram("cart", "e", 10, 3, 0, 0, 0, &[1, 1, 1]),
            ),
            (
                "metrics_summary",
                otlp_summary("cart", "u", 10, 3, 6.0, &[(0.5, 2.0)]),
            ),
        ];
        for (table, body) in &bodies {
            let first = db.ingest_otlp_metrics(body).await.unwrap();
            let second = db.ingest_otlp_metrics(body).await.unwrap();
            assert_eq!((first.accepted, first.rejected), (1, 0), "{table} first");
            assert_eq!((second.accepted, second.rejected), (0, 1), "{table} repeat");
            assert_eq!(
                count(&db, &format!("SELECT count(*) AS c FROM {table}")).await,
                1,
                "{table} kept exactly one point"
            );
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn duplicates_guard_is_bounded_and_eventually_forgets() {
        // A `recent` of 8 means two generations of 4, so the earliest keys are evicted well before
        // the twelfth point. Re-admitting an evicted key is the guard failing *permissive*, which is
        // the safe direction.
        let db = Db::in_memory()
            .duplicates(Duplicates::Reject { recent: 8 })
            .open()
            .unwrap();
        for ts in 0..12u64 {
            let r = db
                .ingest_otlp_metrics(&otlp_gauge_points("cart", "m", &[(ts, 1.0)]))
                .await
                .unwrap();
            assert_eq!((r.accepted, r.rejected), (1, 0), "ts={ts}");
        }
        let r = db
            .ingest_otlp_metrics(&otlp_gauge_points("cart", "m", &[(0, 1.0)]))
            .await
            .unwrap();
        assert_eq!(
            (r.accepted, r.rejected),
            (1, 0),
            "an evicted key is new again"
        );
    }
}

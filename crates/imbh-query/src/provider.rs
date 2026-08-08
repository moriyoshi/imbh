//! The custom `logs` TableProvider and the cost-gated Tantivy → Parquet `RowSelection` bridge
//! (ARCHITECTURE.md §9.2/§8).
//!
//! The provider unions the mutable-buffer snapshot with the sealed segments. For a pushed
//! `matches(body, 'query')` predicate — or an attribute equality `json_get_str(attributes,
//! '<key>') = '<value>'` — it runs the segment's `.tidx` Tantivy index (`search_body` /
//! `search_attr_eq`) to a sorted row-id set and, **when the hit fraction is below a threshold**,
//! reads only those Parquet rows via a `RowSelection`; otherwise it reads the whole file. Multiple
//! pushed constraints intersect (implicit AND) into one selection. Each predicate is claimed
//! `Inexact`, so DataFusion keeps a `FilterExec` with the `matches` / `json_get_str` UDF above the
//! scan — the index is a pure accelerator and Parquet stays the ground truth. Index-less segments,
//! non-`body` text matchers, and non-equality attribute matchers simply fall through to a full scan.
//!
//! **Time/range pruning.** A comparison against an `INT64`-domain column — `time`/`start_time`/
//! `observed_time` (`Timestamp`), whether written bare or in the `CAST(col AS BIGINT)` form the typed
//! builders emit — is also claimed `Inexact` and turned into a [`RangeProbe`]. Each segment's Parquet
//! row-group **statistics** then decide whether the segment can contain a matching row at all: when
//! every row group is ruled out the file is skipped whole (the `segments_pruned` counter, exactly like
//! the bloom path), and when only some are ruled out the read is narrowed to the survivors. Statistics
//! only ever prove a row group *cannot* match, so this never drops a row — and the `FilterExec` above
//! the scan re-checks the predicate regardless. A segment without statistics for the column is always
//! read.

use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};

use async_trait::async_trait;
use datafusion::arrow::datatypes::{DataType, SchemaRef, TimeUnit};
use datafusion::arrow::record_batch::RecordBatch;
use datafusion::catalog::{Session, TableProvider};
use datafusion::common::Column;
use datafusion::error::{DataFusionError, Result as DFResult};
use datafusion::execution::TaskContext;
use datafusion::logical_expr::expr::InList;
use datafusion::logical_expr::{
    BinaryExpr, Cast, Expr, Operator, TableProviderFilterPushDown, TableType,
};
use datafusion::parquet::arrow::arrow_reader::{
    ParquetRecordBatchReader, ParquetRecordBatchReaderBuilder, RowSelection, RowSelector,
};
use datafusion::parquet::file::statistics::Statistics;
use datafusion::physical_expr::LexOrdering;
use datafusion::physical_plan::stream::RecordBatchStreamAdapter;
use datafusion::physical_plan::streaming::{PartitionStream, StreamingTableExec};
use datafusion::physical_plan::{ExecutionPlan, SendableRecordBatchStream};
use datafusion::scalar::ScalarValue;

/// Test-only counters for the bloom-filter segment-pruning path: how many span segments were read
/// vs. skipped (proven absent by their bloom filter). Let a unit test assert that a point lookup
/// actually skipped the non-matching segment (the read-side half of ARCHITECTURE.md §8).
#[cfg(test)]
pub(crate) mod prune_counters {
    use std::sync::atomic::{AtomicU64, Ordering};
    pub(crate) static READ: AtomicU64 = AtomicU64::new(0);
    pub(crate) static PRUNED: AtomicU64 = AtomicU64::new(0);
    /// Serializes tests that run `scan()` and assert on the process-global READ/PRUNED counters, so a
    /// concurrent scan in another test can't pollute the count. Hold it for the whole test body.
    pub(crate) static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
    pub(crate) fn reset() {
        READ.store(0, Ordering::SeqCst);
        PRUNED.store(0, Ordering::SeqCst);
    }
    pub(crate) fn read() -> u64 {
        READ.load(Ordering::SeqCst)
    }
    pub(crate) fn pruned() -> u64 {
        PRUNED.load(Ordering::SeqCst)
    }
}

use crate::coerce;

/// Apply the `RowSelection` only when the index selects fewer than this fraction of the
/// segment's rows; above it, a plain filtered scan is cheaper than the index round-trip
/// (ARCHITECTURE.md §8, "only applies the `RowSelection` when the estimated fraction is below a
/// threshold").
#[cfg(feature = "search")]
const SELECTIVITY_THRESHOLD: f64 = 0.5;

/// Read-side scan statistics accumulated by the provider(s) as batches are pulled, so index/bloom
/// pruning is observable in production `QueryStats` — not only in tests. Shared (`Arc`) across every
/// table's provider for one query.
///
/// The scan is **lazy** (one Parquet batch per `poll_next`; see [`SegmentBatchIter`]), so these
/// counters accrue while the stream is drained and are **final only after it is fully exhausted**.
/// The collect path ([`run_sql`](crate::run_sql)) drains completely before it snapshots, so its
/// `ScanStats` are complete; the streaming path exposes them post-drain via
/// [`StreamStatsHandle`](crate::StreamStatsHandle) (a mid-drain snapshot is a partial count).
#[derive(Debug, Default)]
pub(crate) struct ScanAccum {
    segments_scanned: AtomicU64,
    segments_pruned: AtomicU64,
    rows_scanned: AtomicU64,
    bytes_scanned: AtomicU64,
    index_searched: AtomicBool,
}

impl ScanAccum {
    pub(crate) fn snapshot(&self) -> ScanStats {
        ScanStats {
            segments_scanned: self.segments_scanned.load(Relaxed),
            segments_pruned: self.segments_pruned.load(Relaxed),
            rows_scanned: self.rows_scanned.load(Relaxed),
            bytes_scanned: self.bytes_scanned.load(Relaxed),
            index_searched: self.index_searched.load(Relaxed),
        }
    }
}

/// A snapshot of [`ScanAccum`] returned by `run_sql` — one query's read-side pruning stats.
#[derive(Debug, Clone, Copy, Default)]
pub struct ScanStats {
    /// Segments actually read (i.e. neither bloom- nor statistics-pruned).
    pub segments_scanned: u64,
    /// Segments skipped whole, either by a Parquet bloom filter (`trace_id`/`span_id` point lookups)
    /// or by Parquet row-group statistics (a time/range predicate outside the segment's min/max).
    pub segments_pruned: u64,
    /// Rows materialized from the buffer ∪ segments — after the Tantivy `RowSelection` pruned each
    /// segment — i.e. the rows the `matches`/`json_get_str` UDF filters actually evaluated.
    pub rows_scanned: u64,
    /// Approximate in-memory bytes of those materialized rows.
    pub bytes_scanned: u64,
    /// Whether a segment's Tantivy `.tidx` was searched (a `matches`/attr-equality pushdown fired).
    pub index_searched: bool,
}

/// One sealed segment as seen by the query layer: the Parquet file, its optional `.tidx`
/// sidecar, and its row count (for the cost gate).
#[derive(Debug, Clone)]
pub struct SegmentInput {
    pub parquet_path: PathBuf,
    pub index_path: Option<PathBuf>,
    pub rows: u64,
    /// The segment's `[min, max]` event-time bounds (inclusive, nanoseconds) as recorded in the
    /// manifest, describing its table's `time_column` ([`crate::TableInput`]). When present, a pushed
    /// time predicate that cannot intersect this range skips the segment **without opening the file
    /// at all** — no `File::open`, no Parquet footer read, no `.tidx` search.
    ///
    /// That is the whole point of carrying it: the footer-statistics path can only prune *after*
    /// paying to read the footer, which measured as the dominant cost of a narrow query once
    /// row-level pruning was in place (~35 us/segment across 60 segments). `None` disables the check
    /// and falls back to the footer statistics, which stay correct on their own.
    pub time_range: Option<(i64, i64)>,
}

/// A table = mutable-buffer snapshot ∪ sealed segments. `text_column` names the column whose
/// `matches(col, …)` predicate drives the Tantivy `RowSelection` bridge (`Some("body")` for
/// logs); `None` disables index pushdown (all `matches` fall back to the UDF). `bloom_columns`
/// names the binary id columns that carry a Parquet bloom filter in each segment (`trace_id`,
/// `span_id` for spans); a raw-binary membership predicate on one of them (`col = X'…'`, `col IN
/// (X'…', …)`, or the equivalent `OR` chain) lets the scan skip whole segments whose blooms prove
/// every probed value absent (ARCHITECTURE.md §8). Empty for tables without blooms.
///
/// `time_column` names the column that each [`SegmentInput::time_range`] describes — the table's
/// sort column (`time` for logs and metrics, `start_time` for spans). It must be `None` unless those
/// bounds are populated, and it is what keeps the manifest-range skip honest: a range predicate on
/// some *other* INT64 column (`duration_ns`, say) must never be tested against time bounds.
pub struct SegmentTableProvider {
    schema: SchemaRef,
    buffer: RecordBatch,
    segments: Vec<SegmentInput>,
    text_column: Option<String>,
    bloom_columns: Vec<String>,
    time_column: Option<String>,
    stats: Arc<ScanAccum>,
}

impl SegmentTableProvider {
    pub(crate) fn new(
        schema: SchemaRef,
        buffer: RecordBatch,
        segments: Vec<SegmentInput>,
        text_column: Option<String>,
        bloom_columns: Vec<String>,
        time_column: Option<String>,
        stats: Arc<ScanAccum>,
    ) -> Self {
        Self {
            schema,
            buffer,
            segments,
            time_column,
            text_column,
            bloom_columns,
            stats,
        }
    }

    /// Whether this table's segments carry a `.tidx` sidecar with the `attrs` JSON field, so an
    /// attribute equality can be pushed to `search_attr_eq`. The sidecar is built exactly for the
    /// text-indexed tables (`logs`/`spans`), which are the ones with a `text_column`; metric tables
    /// have neither, so their attr equalities fall through to a full scan (still correct).
    fn has_attr_index(&self) -> bool {
        self.text_column.is_some()
    }
}

impl fmt::Debug for SegmentTableProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SegmentTableProvider")
            .field("buffer_rows", &self.buffer.num_rows())
            .field("segments", &self.segments.len())
            .field("text_column", &self.text_column)
            .finish()
    }
}

#[async_trait]
impl TableProvider for SegmentTableProvider {
    fn schema(&self) -> SchemaRef {
        self.schema.clone()
    }

    fn table_type(&self) -> TableType {
        TableType::Base
    }

    fn supports_filters_pushdown(
        &self,
        filters: &[&Expr],
    ) -> DFResult<Vec<TableProviderFilterPushDown>> {
        Ok(filters
            .iter()
            .map(|f| {
                if matches_text_terms(f, self.text_column.as_deref()).is_some()
                    || not_matches_text_terms(f, self.text_column.as_deref()).is_some()
                    || (self.has_attr_index() && attr_eq_predicate(f).is_some())
                    || bloom_probe(f, &self.bloom_columns).is_some()
                    || stats_range_probe(f, &self.schema).is_some()
                {
                    // Inexact: we may pre-prune (Tantivy row-selection, a bloom-filter segment skip,
                    // or a row-group statistics range skip), but DataFusion re-checks the predicate
                    // above the scan, so the pruning is a pure accelerator and results are identical.
                    TableProviderFilterPushDown::Inexact
                } else {
                    TableProviderFilterPushDown::Unsupported
                }
            })
            .collect())
    }

    async fn scan(
        &self,
        _state: &dyn Session,
        projection: Option<&Vec<usize>>,
        filters: &[Expr],
        limit: Option<usize>,
    ) -> DFResult<Arc<dyn ExecutionPlan>> {
        // Combine terms from every pushed `matches(text_column, …)` conjunct (implicit AND) and,
        // separately, the `NOT matches(text_column, …)` conjuncts (the `!?` must-not terms). Together
        // they drive a single `search_body_bool` (`+must -must_not`) per segment.
        let mut terms: Vec<String> = Vec::new();
        let mut not_terms: Vec<String> = Vec::new();
        for f in filters {
            if let Some(t) = matches_text_terms(f, self.text_column.as_deref()) {
                terms.extend(t);
            }
            if let Some(t) = not_matches_text_terms(f, self.text_column.as_deref()) {
                not_terms.extend(t);
            }
        }
        terms.sort();
        terms.dedup();
        not_terms.sort();
        not_terms.dedup();

        // Attribute equalities the `attrs` index can pre-prune: each `json_get_str(attributes,
        // '<key>') = '<value>'` conjunct becomes a `(key, value)` probe. Combined with the body
        // terms as an implicit AND (intersection) per segment in `row_selection_for`.
        let attr_probes: Vec<(String, String)> = if self.has_attr_index() {
            filters.iter().filter_map(attr_eq_predicate).collect()
        } else {
            Vec::new()
        };

        // Extract raw-id membership predicates (`trace_id`/`span_id` `= X'…'`, `IN (X'…', …)`, or the
        // `= a OR = b` chain the simplifier rewrites a short `IN` into) that the segment bloom filters
        // can rule out (ARCHITECTURE.md §8). Each probe is a (column, candidate-value-set) pair; a
        // segment is skipped only when the bloom filters *prove* every candidate absent — never on
        // uncertainty.
        let bloom_probes: Vec<(String, Vec<Vec<u8>>)> = if self.bloom_columns.is_empty() {
            Vec::new()
        } else {
            filters
                .iter()
                .filter_map(|f| bloom_probe(f, &self.bloom_columns))
                .collect()
        };

        // Comparisons against an INT64-domain column (the `Timestamp` time columns, bare or under the
        // `CAST(col AS BIGINT)` the typed builders emit) that Parquet row-group statistics can rule
        // out. A segment whose every row group is excluded is skipped without reading a single row —
        // the whole point of a `WHERE time > now() - 5m` over a month of data.
        let range_probes: Vec<ColumnRangeProbe> = filters
            .iter()
            .filter_map(|f| stats_range_probe(f, &self.schema))
            .collect();

        let has_constraint = !terms.is_empty() || !not_terms.is_empty() || !attr_probes.is_empty();

        // Lazy scan (ARCHITECTURE.md §10.12, prescription I-4a): rather than reading every segment
        // into a `MemTable` here (during physical planning, before the first batch is polled), hand a
        // `PartitionStream` to `StreamingTableExec`, which reads **one Parquet batch per `poll_next`**.
        // The exec applies `projection`, `limit` (via a `LimitStream` that stops polling early — so a
        // `LIMIT` never reads past segments), and cooperative yielding itself; our partition yields
        // full-schema batches. Pushdown is `Inexact`, so a `FilterExec` above re-checks predicates —
        // laziness never changes results.
        let partition = Arc::new(SegmentPartitionStream {
            schema: self.schema.clone(),
            buffer: self.buffer.clone(),
            segments: self.segments.clone(),
            terms,
            not_terms,
            attr_probes,
            bloom_probes,
            range_probes,
            time_column: self.time_column.clone(),
            has_constraint,
            stats: self.stats.clone(),
        });
        let exec = StreamingTableExec::try_new(
            self.schema.clone(),
            vec![partition],
            projection,
            None::<LexOrdering>,
            false, // finite/bounded source
            limit,
        )?;
        Ok(Arc::new(exec))
    }
}

/// One lazy [`PartitionStream`] over a table's mutable-buffer snapshot ∪ sealed segments. Cloned into
/// a fresh [`SegmentBatchIter`] on each `execute()`; owns everything it reads (buffer batches are
/// `Arc`-cloned; segment paths are owned `PathBuf`s), so the stream it returns is `'static` and
/// self-rooting — it does not borrow the `Db` or the `SessionContext` (prescription I-4).
#[derive(Debug)]
struct SegmentPartitionStream {
    schema: SchemaRef,
    buffer: RecordBatch,
    segments: Vec<SegmentInput>,
    terms: Vec<String>,
    not_terms: Vec<String>,
    attr_probes: Vec<(String, String)>,
    bloom_probes: Vec<(String, Vec<Vec<u8>>)>,
    range_probes: Vec<ColumnRangeProbe>,
    time_column: Option<String>,
    has_constraint: bool,
    stats: Arc<ScanAccum>,
}

impl PartitionStream for SegmentPartitionStream {
    fn schema(&self) -> &SchemaRef {
        &self.schema
    }

    fn execute(&self, _ctx: Arc<TaskContext>) -> SendableRecordBatchStream {
        let iter = SegmentBatchIter {
            schema: self.schema.clone(),
            stats: self.stats.clone(),
            // Emit the (single) buffer snapshot batch first, only if it has rows.
            buffer: (self.buffer.num_rows() > 0).then(|| self.buffer.clone()),
            segments: self.segments.clone().into_iter(),
            terms: self.terms.clone(),
            not_terms: self.not_terms.clone(),
            attr_probes: self.attr_probes.clone(),
            bloom_probes: self.bloom_probes.clone(),
            range_probes: self.range_probes.clone(),
            time_column: self.time_column.clone(),
            has_constraint: self.has_constraint,
            current: None,
        };
        // `stream::iter` polls exactly one `iter.next()` per `poll_next`, so each poll does one
        // synchronous segment-batch read + decode and returns — the per-batch yield I-4a requires.
        Box::pin(RecordBatchStreamAdapter::new(
            self.schema.clone(),
            futures::stream::iter(iter),
        ))
    }
}

/// The lazy per-batch scan state machine (prescription I-4a). Yields the buffer batch first, then
/// walks the segments, opening each one's `ParquetRecordBatchReader` only when reached and pulling a
/// single batch per `next()`. Emits **full-schema** batches (projection is applied by the enclosing
/// [`StreamingTableExec`]). Read-side [`ScanAccum`] counters are bumped here, on the poll path, so
/// they are final only once the iterator is fully consumed.
struct SegmentBatchIter {
    schema: SchemaRef,
    stats: Arc<ScanAccum>,
    /// The buffer snapshot batch, emitted first; `take()`n on the first `next()`.
    buffer: Option<RecordBatch>,
    segments: std::vec::IntoIter<SegmentInput>,
    terms: Vec<String>,
    not_terms: Vec<String>,
    attr_probes: Vec<(String, String)>,
    bloom_probes: Vec<(String, Vec<Vec<u8>>)>,
    range_probes: Vec<ColumnRangeProbe>,
    time_column: Option<String>,
    has_constraint: bool,
    /// The reader for the segment currently being drained, one batch at a time.
    current: Option<ParquetRecordBatchReader>,
}

impl SegmentBatchIter {
    /// Coerce a batch to the canonical schema and account its rows/bytes (buffer and segment batches
    /// alike). Buffer and `parquet`-crate segment batches both arrive `Utf8` (not `Utf8View`), so
    /// `coerce` is usually an identity (see [`crate::coerce`]).
    fn account(&self, b: RecordBatch) -> DFResult<RecordBatch> {
        let b = coerce(b, &self.schema).map_err(df_err)?;
        self.stats
            .rows_scanned
            .fetch_add(b.num_rows() as u64, Relaxed);
        self.stats
            .bytes_scanned
            .fetch_add(b.get_array_memory_size() as u64, Relaxed);
        Ok(b)
    }
}

impl SegmentBatchIter {
    /// Whether the manifest's declared time bounds for `seg` already prove no row can satisfy the
    /// pushed time predicates — the ARCHITECTURE.md §9.2 "manifest range skip".
    ///
    /// Only probes on `time_column` are consulted: [`SegmentInput::time_range`] describes that column
    /// and nothing else, so testing a `duration_ns` comparison against it would skip segments that do
    /// contain matching rows. Requires both a known `time_column` and a populated `time_range`;
    /// either being absent simply defers to the footer statistics.
    ///
    /// Bounds are **inclusive** on both ends, matching `SegmentRef`'s `min_time_unix_nano` /
    /// `max_time_unix_nano`, and [`RangeProbe::excludes`] is written against that convention — which
    /// is what makes a row sitting exactly on a half-open range's `start` survive.
    fn manifest_range_excludes(&self, seg: &SegmentInput) -> bool {
        let Some(time_column) = self.time_column.as_deref() else {
            return false;
        };
        let Some((min, max)) = seg.time_range else {
            return false;
        };
        self.range_probes
            .iter()
            .any(|p| p.column == time_column && p.probe.excludes(min, max))
    }
}

impl Iterator for SegmentBatchIter {
    type Item = DFResult<RecordBatch>;

    fn next(&mut self) -> Option<Self::Item> {
        // 1. The buffer snapshot batch is emitted before any segment.
        if let Some(buf) = self.buffer.take() {
            return Some(self.account(buf));
        }
        loop {
            // 2. Drain the currently-open segment one batch per call.
            if let Some(reader) = self.current.as_mut() {
                match reader.next() {
                    Some(Ok(b)) => return Some(self.account(b)),
                    Some(Err(e)) => {
                        return Some(Err(DataFusionError::Execution(format!(
                            "parquet read: {e}"
                        ))));
                    }
                    None => self.current = None, // segment exhausted → advance
                }
            }
            // 3. Advance to the next segment, opening its reader (or skipping it if bloom-pruned).
            let seg = self.segments.next()?;
            // A child span per segment scan, covering the `.tidx` search, the `RowSelection` cost-gate
            // decision, and the Parquet open. Its `Empty` fields are filled in below (and inside
            // `row_selection_for`) so index-pruning shows up per segment in a trace viewer. Entered for
            // the duration of the decision block; the lazy row reads themselves happen on later polls.
            #[cfg(feature = "tracing")]
            let scan_span = tracing::debug_span!(
                "query.scan_segment",
                segment = %seg.parquet_path.display(),
                rows = seg.rows,
                indexed = seg.index_path.is_some(),
                index_hits = tracing::field::Empty,
                hit_fraction = tracing::field::Empty,
                row_selection = tracing::field::Empty,
                pruned = tracing::field::Empty,
            )
            .entered();
            // 3a. The cheapest skip there is: the manifest already told us this segment's time
            // bounds, so a time predicate that cannot intersect them rules it out with **no file
            // opened** — no `File::open`, no footer read, no `.tidx` search. Deliberately ahead of
            // `row_selection_for` and `open_segment`, both of which cost I/O. The footer-statistics
            // path below stays in place and remains correct on its own; this only front-runs it for
            // segments whose declared range already settles the question.
            if self.manifest_range_excludes(&seg) {
                self.stats.segments_pruned.fetch_add(1, Relaxed);
                #[cfg(feature = "tracing")]
                scan_span.record("pruned", true);
                #[cfg(test)]
                prune_counters::PRUNED.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                continue;
            }
            // The `.tidx` is searched whenever a pushable predicate meets an indexed segment (whatever
            // the cost gate then decides). Records that the index was consulted, for `QueryStats`.
            if self.has_constraint && seg.index_path.is_some() {
                self.stats.index_searched.store(true, Relaxed);
            }
            // `row_selection_for` records `index_hits`/`hit_fraction`/`row_selection` onto the current
            // (`scan_span`) span when it derives a selection.
            let selection =
                match row_selection_for(&seg, &self.terms, &self.not_terms, &self.attr_probes) {
                    Ok(s) => s,
                    Err(e) => return Some(Err(e)),
                };
            match open_segment(
                &seg.parquet_path,
                selection,
                &self.bloom_probes,
                &self.range_probes,
            ) {
                Ok(None) => {
                    self.stats.segments_pruned.fetch_add(1, Relaxed);
                    #[cfg(feature = "tracing")]
                    scan_span.record("pruned", true);
                    #[cfg(test)]
                    prune_counters::PRUNED.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    // loop: try the next segment
                }
                Ok(Some(reader)) => {
                    self.stats.segments_scanned.fetch_add(1, Relaxed);
                    #[cfg(feature = "tracing")]
                    scan_span.record("pruned", false);
                    #[cfg(test)]
                    prune_counters::READ.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    self.current = Some(reader);
                    // loop: drain the reader we just opened
                }
                Err(e) => return Some(Err(e)),
            }
        }
    }
}

/// If `expr` is `matches(<text_column>, '<literal>')` for this table's indexed text column,
/// return the query tokens. Returns `None` otherwise (including when `text_column` is `None`,
/// or the matcher targets a different column) so the predicate is not claimed for pushdown.
fn matches_text_terms(expr: &Expr, text_column: Option<&str>) -> Option<Vec<String>> {
    let text_column = text_column?;
    let Expr::ScalarFunction(sf) = expr else {
        return None;
    };
    if sf.func.name() != "matches" || sf.args.len() != 2 {
        return None;
    }
    let Expr::Column(Column { name, .. }) = &sf.args[0] else {
        return None;
    };
    if name != text_column {
        return None;
    }
    let query = utf8_literal(&sf.args[1])?;
    Some(imbh_core::tokenize(&query))
}

/// If `expr` is `NOT matches(<text_column>, '<literal>')`, return the query tokens as must-not
/// terms. The imbh dialect's `!?` operator renders exactly this, so a pure `|?`/`!?` chain reduces
/// to one `search_body_bool` (`+must -must_not`) Tantivy query. Returns `None` otherwise, so the
/// predicate is not claimed for pushdown (and is left for the `FilterExec` above the scan).
fn not_matches_text_terms(expr: &Expr, text_column: Option<&str>) -> Option<Vec<String>> {
    let Expr::Not(inner) = expr else {
        return None;
    };
    matches_text_terms(inner, text_column)
}

/// The bloom-filter probe `expr` implies, if any: `(column_name, candidate_values)` where the
/// predicate can only be true for a row whose `column_name` is **one of** `candidate_values`. A
/// segment may then be skipped when the blooms prove *every* candidate absent.
///
/// Three shapes are recognized, all on a bloom-indexed id column with raw binary literals:
/// * `col = X'…'` — the point lookup (`TracesApi::get`-shaped);
/// * `col IN (X'…', …)` — the trace-search shape (k trace ids at once), non-negated only: `NOT IN`
///   proves nothing about absence;
/// * `col = X'a' OR col = X'b'` — what DataFusion's `ShortenInListSimplifier` rewrites a short `IN`
///   (≤ 3 values) into *before* filter pushdown, so the `IN` shape alone would miss the common case.
///   Both sides must probe the same column; a disjunct on any other column (or an unrecognized one)
///   means the row could match without `col` holding a candidate, so nothing is claimed.
///
/// Returns `None` for anything else — including the `hex(col) = '…'` UDF form, which never yields the
/// raw id bytes a bloom lookup needs. Correctness never depends on this firing: a miss just means the
/// segment is read unpruned (DataFusion still applies the predicate above the `Inexact` pushdown).
fn bloom_probe(expr: &Expr, bloom_columns: &[String]) -> Option<(String, Vec<Vec<u8>>)> {
    match expr {
        // A disjunction: the candidate sets union, but only if both sides constrain one same column.
        Expr::BinaryExpr(BinaryExpr {
            left,
            op: Operator::Or,
            right,
        }) => {
            let (column, mut values) = bloom_probe(left, bloom_columns)?;
            let (right_column, right_values) = bloom_probe(right, bloom_columns)?;
            if column != right_column {
                return None;
            }
            values.extend(right_values);
            Some((column, values))
        }
        Expr::BinaryExpr(_) => bloom_id_eq(expr, bloom_columns).map(|(c, v)| (c, vec![v])),
        Expr::InList(InList {
            expr,
            list,
            negated,
        }) => {
            if *negated || list.is_empty() {
                return None;
            }
            let name = column_name(expr)?;
            if !bloom_columns.iter().any(|c| c == &name) {
                return None;
            }
            // Every list element must be a binary literal — one non-literal (a column reference, a
            // string) and the predicate can hold for a value we cannot probe.
            let values = list
                .iter()
                .map(binary_literal_bytes)
                .collect::<Option<_>>()?;
            Some((name, values))
        }
        _ => None,
    }
}

/// If `expr` is a raw binary equality on one of this table's bloom-indexed id columns
/// (`<bloom_col> = X'…'`, in either operand order, tolerating a `CAST` the type-coercer may wrap
/// around the column or literal), return `(column_name, value_bytes)` for a bloom-filter probe.
/// Returns `None` for anything else — including the `hex(col) = '…'` UDF form, which never yields
/// the raw id bytes a bloom lookup needs. Correctness never depends on this firing: a miss just
/// means the segment is read unpruned (DataFusion still applies the predicate).
fn bloom_id_eq(expr: &Expr, bloom_columns: &[String]) -> Option<(String, Vec<u8>)> {
    let Expr::BinaryExpr(BinaryExpr { left, op, right }) = expr else {
        return None;
    };
    if *op != Operator::Eq {
        return None;
    }
    bloom_id_eq_sides(left, right, bloom_columns)
        .or_else(|| bloom_id_eq_sides(right, left, bloom_columns))
}

/// One operand ordering of [`bloom_id_eq`]: `col_side` should resolve to a bloom-indexed column and
/// `lit_side` to a binary literal.
fn bloom_id_eq_sides(
    col_side: &Expr,
    lit_side: &Expr,
    bloom_columns: &[String],
) -> Option<(String, Vec<u8>)> {
    let name = column_name(col_side)?;
    if !bloom_columns.iter().any(|c| c == &name) {
        return None;
    }
    let bytes = binary_literal_bytes(lit_side)?;
    Some((name, bytes))
}

/// The column name behind an expression that is a bare column or a `CAST` of one (type coercion
/// may wrap the id column in a cast when comparing a `FixedSizeBinary` column to a `Binary`
/// literal). `None` for any other shape.
fn column_name(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Column(Column { name, .. }) => Some(name.clone()),
        Expr::Cast(Cast { expr, .. }) => column_name(expr),
        _ => None,
    }
}

/// The raw bytes of a binary-typed literal (`X'…'`), across the binary flavors the coercer may
/// produce, unwrapping a `CAST` around the literal. `None` for a non-binary literal.
fn binary_literal_bytes(expr: &Expr) -> Option<Vec<u8>> {
    match expr {
        Expr::Literal(ScalarValue::Binary(Some(b)), _)
        | Expr::Literal(ScalarValue::LargeBinary(Some(b)), _)
        | Expr::Literal(ScalarValue::BinaryView(Some(b)), _)
        | Expr::Literal(ScalarValue::FixedSizeBinary(_, Some(b)), _) => Some(b.clone()),
        Expr::Cast(Cast { expr, .. }) => binary_literal_bytes(expr),
        _ => None,
    }
}

/// One pushed comparison `<op> <i64 literal>` that Parquet row-group statistics can rule out.
/// `value` is the literal in the column's own INT64 storage domain — for a `Timestamp` column that
/// is the raw tick count Parquet stores, which is also exactly what `CAST(col AS BIGINT)` yields, so
/// both spellings of a time bound compare against the statistics without conversion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RangeProbe {
    op: RangeOp,
    value: i64,
}

/// The comparison operators a min/max statistic can decide. `!=` is deliberately absent: a row group
/// whose min/max bracket the value can still contain other values, so it proves nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RangeOp {
    Lt,
    LtEq,
    Gt,
    GtEq,
    Eq,
}

impl RangeOp {
    /// The operator of the mirrored expression (`lit < col` ⇒ `col > lit`), so a literal-on-the-left
    /// comparison prunes identically.
    fn mirrored(self) -> Self {
        match self {
            RangeOp::Lt => RangeOp::Gt,
            RangeOp::LtEq => RangeOp::GtEq,
            RangeOp::Gt => RangeOp::Lt,
            RangeOp::GtEq => RangeOp::LtEq,
            RangeOp::Eq => RangeOp::Eq,
        }
    }

    fn from_operator(op: Operator) -> Option<Self> {
        match op {
            Operator::Lt => Some(RangeOp::Lt),
            Operator::LtEq => Some(RangeOp::LtEq),
            Operator::Gt => Some(RangeOp::Gt),
            Operator::GtEq => Some(RangeOp::GtEq),
            Operator::Eq => Some(RangeOp::Eq),
            _ => None,
        }
    }
}

impl RangeProbe {
    /// `true` iff **no** value in the inclusive interval `[min, max]` can satisfy this comparison —
    /// i.e. the row group is *proven* to hold no matching row and may be skipped. Never returns
    /// `true` on uncertainty: this is the only place correctness rests, and it is the exact dual of
    /// the operator (`col > v` needs some element `> v`, which is impossible iff `max <= v`, …).
    ///
    /// Rows whose column value is NULL are covered too: Parquet min/max describe the non-null values
    /// only, but a comparison is NULL (never true) for a NULL operand, so a NULL row can never be the
    /// match a skip would lose.
    fn excludes(&self, min: i64, max: i64) -> bool {
        match self.op {
            RangeOp::Gt => max <= self.value,
            RangeOp::GtEq => max < self.value,
            RangeOp::Lt => min >= self.value,
            RangeOp::LtEq => min > self.value,
            RangeOp::Eq => self.value < min || self.value > max,
        }
    }
}

/// A pushed range predicate paired with the column it constrains.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ColumnRangeProbe {
    column: String,
    probe: RangeProbe,
}

/// If `expr` is a comparison between an INT64-domain column of `schema` and a matching integer
/// literal, return the probe its Parquet statistics can be tested against. `None` for every other
/// shape — a miss only means the segment is read unpruned.
///
/// Accepted column spellings (see [`int64_domain_column`]): a bare `Timestamp`/`Int64` column, and
/// the value-preserving casts around it — notably `CAST("time" AS BIGINT)`, which every typed query
/// builder emits for its time range. The literal must already be in that same domain (DataFusion's
/// type coercion guarantees this for a well-typed comparison), so no lossy conversion is ever
/// performed here.
fn stats_range_probe(expr: &Expr, schema: &SchemaRef) -> Option<ColumnRangeProbe> {
    let Expr::BinaryExpr(BinaryExpr { left, op, right }) = expr else {
        return None;
    };
    let op = RangeOp::from_operator(*op)?;
    range_probe_sides(left, right, op, schema)
        .or_else(|| range_probe_sides(right, left, op.mirrored(), schema))
}

/// One operand ordering of [`stats_range_probe`]: `col_side` must resolve to an INT64-domain column
/// and `lit_side` to a literal of that same type.
fn range_probe_sides(
    col_side: &Expr,
    lit_side: &Expr,
    op: RangeOp,
    schema: &SchemaRef,
) -> Option<ColumnRangeProbe> {
    let (column, domain) = int64_domain_column(col_side, schema)?;
    let value = i64_literal_in(lit_side, &domain)?;
    Some(ColumnRangeProbe {
        column,
        probe: RangeProbe { op, value },
    })
}

/// The column behind an expression whose value is stored by Parquet as a signed `INT64`, together
/// with the Arrow type the comparison actually happens in. Only two base types qualify — `Int64` and
/// `Timestamp(_, _)` — because both map to the `INT64` physical type with signed ordering, so their
/// row-group statistics can be read as `i64` without any reinterpretation.
///
/// A `CAST` is unwrapped **only** when it cannot change the stored value: `Timestamp → Int64` (Arrow
/// reinterprets the tick count), `Int64 → Int64`, and `Timestamp → Timestamp` with the *same*
/// `TimeUnit` (only the timezone annotation differs; Arrow timestamps are always UTC instants). A
/// unit-changing or numeric-narrowing cast returns `None` rather than risk a wrong bound.
fn int64_domain_column(expr: &Expr, schema: &SchemaRef) -> Option<(String, DataType)> {
    match expr {
        Expr::Column(Column { name, .. }) => {
            let field = schema.field_with_name(name).ok()?;
            match field.data_type() {
                DataType::Int64 | DataType::Timestamp(_, _) => {
                    Some((name.clone(), field.data_type().clone()))
                }
                _ => None,
            }
        }
        Expr::Cast(Cast { expr, field }) => {
            let (name, src) = int64_domain_column(expr, schema)?;
            let data_type = field.data_type();
            match (&src, data_type) {
                (DataType::Timestamp(_, _) | DataType::Int64, DataType::Int64) => {
                    Some((name, DataType::Int64))
                }
                (DataType::Timestamp(a, _), DataType::Timestamp(b, _)) if a == b => {
                    Some((name, data_type.clone()))
                }
                _ => None,
            }
        }
        _ => None,
    }
}

/// The `i64` value of a literal **already typed as** `dt` — the domain [`int64_domain_column`]
/// reported for the other operand. Requiring an exact type match (including the timestamp
/// `TimeUnit`) is what makes the comparison against raw Parquet statistics sound; a mismatched or
/// non-literal operand yields `None` and the segment is simply read unpruned.
fn i64_literal_in(expr: &Expr, dt: &DataType) -> Option<i64> {
    let Expr::Literal(v, _) = expr else {
        return None;
    };
    match (dt, v) {
        (DataType::Int64, ScalarValue::Int64(Some(v))) => Some(*v),
        (DataType::Timestamp(TimeUnit::Second, _), ScalarValue::TimestampSecond(Some(v), _)) => {
            Some(*v)
        }
        (
            DataType::Timestamp(TimeUnit::Millisecond, _),
            ScalarValue::TimestampMillisecond(Some(v), _),
        ) => Some(*v),
        (
            DataType::Timestamp(TimeUnit::Microsecond, _),
            ScalarValue::TimestampMicrosecond(Some(v), _),
        ) => Some(*v),
        (
            DataType::Timestamp(TimeUnit::Nanosecond, _),
            ScalarValue::TimestampNanosecond(Some(v), _),
        ) => Some(*v),
        _ => None,
    }
}

/// Which row groups of a segment survive the pushed range probes.
#[derive(Debug, PartialEq, Eq)]
enum RowGroups {
    /// Nothing was ruled out — read the file as-is.
    All,
    /// Every row group is proven to hold no matching row — skip the segment entirely.
    None,
    /// Only these row groups (in ascending order) can match.
    Subset(Vec<usize>),
}

/// Test every row group's statistics against the pushed range probes. A row group is dropped only
/// when some probe *proves* it cannot match ([`RangeProbe::excludes`]); a missing column, missing
/// statistics, or a non-`INT64` statistic keeps it (statistics may only ever rule a row group out).
fn surviving_row_groups(
    builder: &ParquetRecordBatchReaderBuilder<std::fs::File>,
    range_probes: &[ColumnRangeProbe],
) -> RowGroups {
    let metadata = builder.metadata();
    let num_row_groups = metadata.num_row_groups();
    if num_row_groups == 0 || range_probes.is_empty() {
        return RowGroups::All;
    }
    // Resolve each probe's leaf-column index once for this file; a probe on a column the segment does
    // not carry (an older segment, a promoted column) simply cannot prune.
    let columns = builder.parquet_schema().columns();
    let resolved: Vec<(usize, RangeProbe)> = range_probes
        .iter()
        .filter_map(|p| {
            let idx = columns.iter().position(|c| c.name() == p.column)?;
            Some((idx, p.probe))
        })
        .collect();
    if resolved.is_empty() {
        return RowGroups::All;
    }
    let mut keep = Vec::with_capacity(num_row_groups);
    for rg in 0..num_row_groups {
        let meta = metadata.row_group(rg);
        let excluded = resolved.iter().any(|(idx, probe)| {
            match meta.column(*idx).statistics().and_then(int64_bounds) {
                Some((min, max)) => probe.excludes(min, max),
                None => false, // no usable statistics → the row group must be read
            }
        });
        if !excluded {
            keep.push(rg);
        }
    }
    if keep.is_empty() {
        RowGroups::None
    } else if keep.len() == num_row_groups {
        RowGroups::All
    } else {
        RowGroups::Subset(keep)
    }
}

/// The `(min, max)` of an `INT64` column chunk. Deliberately narrow: [`int64_domain_column`] only
/// admits `Int64`/`Timestamp` columns, whose physical type is `INT64` with signed ordering, so any
/// other statistics variant here means the column is not what the predicate thought it was and
/// nothing is pruned.
fn int64_bounds(stats: &Statistics) -> Option<(i64, i64)> {
    match stats {
        Statistics::Int64(s) => Some((*s.min_opt()?, *s.max_opt()?)),
        _ => None,
    }
}

/// Express "read only these row groups" as a whole-file [`RowSelection`], so it can be intersected
/// with a Tantivy row selection (which is addressed in whole-file row ordinals and would be silently
/// re-based by `with_row_groups`).
fn row_group_selection(
    builder: &ParquetRecordBatchReaderBuilder<std::fs::File>,
    keep: &[usize],
) -> RowSelection {
    let metadata = builder.metadata();
    let selectors: Vec<RowSelector> = (0..metadata.num_row_groups())
        .map(|rg| {
            let rows = metadata.row_group(rg).num_rows() as usize;
            if keep.contains(&rg) {
                RowSelector::select(rows)
            } else {
                RowSelector::skip(rows)
            }
        })
        .collect();
    RowSelection::from(selectors)
}

fn utf8_literal(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Literal(ScalarValue::Utf8(Some(s)), _)
        | Expr::Literal(ScalarValue::LargeUtf8(Some(s)), _)
        | Expr::Literal(ScalarValue::Utf8View(Some(s)), _) => Some(s.clone()),
        _ => None,
    }
}

/// The `RowSelection` to read this segment with, or `None` to read the whole file. Combines the
/// pushed body `terms` and attribute-equality `attr_probes` as an implicit AND: it intersects the
/// exact hit set of each (`search_body` for the terms, `search_attr_eq` per probe), cost-gates the
/// result, and turns it into a `RowSelection`. Every constituent search returns the exact matching
/// rows, so the intersection is the exact candidate set; the UDFs above the scan re-check anyway.
#[cfg(feature = "search")]
fn row_selection_for(
    seg: &SegmentInput,
    terms: &[String],
    not_terms: &[String],
    attr_probes: &[(String, String)],
) -> DFResult<Option<RowSelection>> {
    if seg.rows == 0 {
        return Ok(None);
    }
    let has_body = !terms.is_empty() || !not_terms.is_empty();
    if !has_body && attr_probes.is_empty() {
        return Ok(None); // no pushable constraint → whole-file scan
    }
    let Some(index_path) = &seg.index_path else {
        return Ok(None); // index-less segment → plain scan (still correct)
    };

    // Intersect every constraint's exact hit set. Start from the body terms (`+must -must_not`, if
    // any), then fold in each attribute probe.
    let mut hits: Option<Vec<u64>> = if has_body {
        Some(imbh_index::search_body_bool(index_path, terms, not_terms).map_err(df_err)?)
    } else {
        None
    };
    for (key, value) in attr_probes {
        let attr_hits = imbh_index::search_attr_eq(index_path, key, value).map_err(df_err)?;
        hits = Some(match hits {
            Some(h) => intersect_sorted(&h, &attr_hits),
            None => attr_hits,
        });
    }
    // At least one of body/attr was present, so `hits` is `Some`.
    let hits = hits.unwrap_or_default();

    // Defensive: a hit ordinal at/after the segment's row count means the `.tidx` disagrees with the
    // Parquet file (only a storage/compaction bug could cause this). Rather than build an
    // out-of-range `RowSelection` that would over-run the reader (panic / read error), fall back to a
    // full scan — the UDF re-filter above still returns correct results, just unpruned.
    if hits.last().is_some_and(|&h| h >= seg.rows) {
        debug_assert!(
            false,
            "tantivy hit ordinal {:?} >= segment rows {}",
            hits.last(),
            seg.rows
        );
        return Ok(None);
    }
    // Cost gate: only worth a RowSelection if the combined predicate is selective enough.
    let applied = (hits.len() as f64) < (seg.rows as f64) * SELECTIVITY_THRESHOLD;
    // Surface the index-pruning decision (hit count, hit fraction, whether the `RowSelection` was
    // applied) as fields on the enclosing `query.scan_segment` span.
    #[cfg(feature = "tracing")]
    {
        let span = tracing::Span::current();
        span.record("index_hits", hits.len() as u64);
        span.record("hit_fraction", hits.len() as f64 / seg.rows as f64);
        span.record("row_selection", applied);
    }
    if !applied {
        return Ok(None);
    }
    Ok(Some(row_selection_from_sorted(&hits, seg.rows)))
}

/// Without the `search` feature there is no term index compiled in, so every segment is read in
/// full; the `matches()` / `json_get_str()` UDFs still filter row-by-row, so results are identical
/// (just unpruned). (`seg.index_path` is always `None` in this build anyway, since no `.tidx` is
/// written.)
#[cfg(not(feature = "search"))]
fn row_selection_for(
    _seg: &SegmentInput,
    _terms: &[String],
    _not_terms: &[String],
    _attr_probes: &[(String, String)],
) -> DFResult<Option<RowSelection>> {
    Ok(None)
}

/// The sorted intersection of two sorted, deduplicated row-ordinal lists (an implicit AND of two
/// pushed constraints). Both inputs come from `search_*`, which return sorted-deduped ordinals.
#[cfg(feature = "search")]
fn intersect_sorted(a: &[u64], b: &[u64]) -> Vec<u64> {
    let mut out = Vec::new();
    let (mut i, mut j) = (0usize, 0usize);
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                out.push(a[i]);
                i += 1;
                j += 1;
            }
        }
    }
    out
}

/// If `expr` is `json_get_str(attributes, '<key>') = '<value>'` (in either operand order), return
/// `(key, value)` for the attr-equality `RowSelection` pushdown. Only the `attributes` column is
/// matched — it is the sole column the `.tidx` `attrs` field indexes, so a `json_get_str` on any
/// other column (or any other predicate shape) returns `None`. Correctness never depends on this
/// firing: a miss just reads the segment unpruned, and DataFusion still applies the predicate.
fn attr_eq_predicate(expr: &Expr) -> Option<(String, String)> {
    let Expr::BinaryExpr(BinaryExpr { left, op, right }) = expr else {
        return None;
    };
    if *op != Operator::Eq {
        return None;
    }
    attr_eq_sides(left, right).or_else(|| attr_eq_sides(right, left))
}

/// One operand ordering of [`attr_eq_predicate`]: `fn_side` should be the `json_get_str(attributes,
/// '<key>')` call and `lit_side` the string value literal.
fn attr_eq_sides(fn_side: &Expr, lit_side: &Expr) -> Option<(String, String)> {
    let key = json_get_str_attr_key(fn_side)?;
    let value = utf8_literal(lit_side)?;
    Some((key, value))
}

/// The attribute key of a `json_get_str(attributes, '<key>')` call — the only json-access shape the
/// `attrs` index can accelerate. `None` for any other expression (wrong function, arity, column, or
/// a non-literal key).
fn json_get_str_attr_key(expr: &Expr) -> Option<String> {
    let Expr::ScalarFunction(sf) = expr else {
        return None;
    };
    if sf.func.name() != "json_get_str" || sf.args.len() != 2 {
        return None;
    }
    let Expr::Column(Column { name, .. }) = &sf.args[0] else {
        return None;
    };
    if name != "attributes" {
        return None;
    }
    utf8_literal(&sf.args[1])
}

/// Convert a sorted, deduplicated set of selected row ordinals into a Parquet `RowSelection`,
/// coalescing consecutive selects/skips.
#[cfg(feature = "search")]
fn row_selection_from_sorted(sorted_hits: &[u64], total: u64) -> RowSelection {
    let mut selectors: Vec<RowSelector> = Vec::new();
    let mut cursor = 0u64;
    let mut i = 0usize;
    while i < sorted_hits.len() {
        let start = sorted_hits[i];
        if start > cursor {
            selectors.push(RowSelector::skip((start - cursor) as usize));
        }
        // Extend a run of consecutive hits.
        let mut end = start;
        while i + 1 < sorted_hits.len() && sorted_hits[i + 1] == end + 1 {
            i += 1;
            end = sorted_hits[i];
        }
        selectors.push(RowSelector::select((end - start + 1) as usize));
        cursor = end + 1;
        i += 1;
    }
    if cursor < total {
        selectors.push(RowSelector::skip((total - cursor) as usize));
    }
    RowSelection::from(selectors)
}

/// Open a Parquet segment (all columns) for **lazy** reading, optionally restricted to a
/// `RowSelection`. Returns the `parquet`-crate reader (an `Iterator` yielding one batch per `next()`,
/// which [`SegmentBatchIter`] pulls one-at-a-time), or `None` when a bloom filter proves the segment
/// can be skipped. Reads via the `parquet` crate directly (not DataFusion), which yields `Utf8` — not
/// `Utf8View` — string columns matching the canonical schema.
///
/// Before opening, if some `bloom_probes` entry (a raw `trace_id`/`span_id` candidate set) has
/// *every* candidate proven absent by this segment's Parquet bloom filters, the segment is skipped
/// (`Ok(None)`) — the point-lookup accelerator of ARCHITECTURE.md §8. A segment lacking a bloom
/// filter for the probed column (older segments), or reporting any candidate maybe-present, is
/// always read: bloom filters only ever rule a value *out*, so this can never drop a matching row.
///
/// The `range_probes` (time/`INT64` comparisons) are then tested against the row-group statistics
/// already present in the footer this open just read: a segment whose every row group is ruled out is
/// skipped (`Ok(None)`) without touching a data page — so `WHERE time > now() - 5m` over a month of
/// segments opens each footer but reads rows from only the in-range ones. When just *some* row groups
/// are ruled out the read is narrowed to the survivors; if a Tantivy `selection` is also in play the
/// narrowing is expressed as a whole-file `RowSelection` and intersected with it, because
/// `with_row_groups` would silently re-base the selection's row ordinals onto the kept groups.
fn open_segment(
    path: &PathBuf,
    selection: Option<RowSelection>,
    bloom_probes: &[(String, Vec<Vec<u8>>)],
    range_probes: &[ColumnRangeProbe],
) -> DFResult<Option<ParquetRecordBatchReader>> {
    let file = std::fs::File::open(path)
        .map_err(|e| DataFusionError::Execution(format!("open segment {}: {e}", path.display())))?;
    let mut builder = ParquetRecordBatchReaderBuilder::try_new(file)
        .map_err(|e| DataFusionError::Execution(format!("parquet reader: {e}")))?;
    if !bloom_probes.is_empty() && bloom_rules_out(&builder, bloom_probes)? {
        return Ok(None);
    }
    match surviving_row_groups(&builder, range_probes) {
        RowGroups::None => return Ok(None),
        RowGroups::All => {
            if let Some(sel) = selection {
                builder = builder.with_row_selection(sel);
            }
        }
        RowGroups::Subset(keep) => match selection {
            None => builder = builder.with_row_groups(keep),
            Some(sel) => {
                let rg_sel = row_group_selection(&builder, &keep);
                builder = builder.with_row_selection(rg_sel.intersection(&sel));
            }
        },
    }
    let reader = builder
        .build()
        .map_err(|e| DataFusionError::Execution(format!("parquet build: {e}")))?;
    Ok(Some(reader))
}

/// `true` iff some probe `(column, values)` has its **whole candidate set** *proven absent* from the
/// segment: for every row group, that column has a bloom filter and none of `values` is reported
/// present. Any row group missing a bloom filter, or reporting *any* candidate maybe-present, means
/// the segment must be read — a probe set is a disjunction (`col = v1 OR … OR col = vn`), so one
/// maybe-present member already makes a matching row possible.
///
/// Distinct probes, by contrast, are conjuncts (`WHERE a AND b`), so ruling out *any single* one is
/// enough to skip the segment.
fn bloom_rules_out(
    builder: &ParquetRecordBatchReaderBuilder<std::fs::File>,
    bloom_probes: &[(String, Vec<Vec<u8>>)],
) -> DFResult<bool> {
    for (column, values) in bloom_probes {
        if values.is_empty() {
            continue; // no candidate to probe → nothing proven
        }
        let Some(col_idx) = builder
            .parquet_schema()
            .columns()
            .iter()
            .position(|c| c.name() == column)
        else {
            continue; // column not in this segment → can't prune on it
        };
        let num_row_groups = builder.metadata().num_row_groups();
        if num_row_groups == 0 {
            continue;
        }
        let mut all_absent = true;
        'row_groups: for rg in 0..num_row_groups {
            // No bloom for this row group → can't rule the segment out.
            let Some(sbbf) = builder
                .get_row_group_column_bloom_filter(rg, col_idx)
                .map_err(|e| DataFusionError::Execution(format!("read bloom filter: {e}")))?
            else {
                all_absent = false;
                break;
            };
            for value in values {
                // Maybe-present candidate → this row group may hold a matching row.
                if sbbf.check(&value[..]) {
                    all_absent = false;
                    break 'row_groups;
                }
            }
        }
        if all_absent {
            return Ok(true);
        }
    }
    Ok(false)
}

fn df_err(e: imbh_core::Error) -> DataFusionError {
    DataFusionError::External(Box::new(e))
}

// Both tests exercise the `search`-only `RowSelection` bridge.
#[cfg(all(test, feature = "search"))]
mod tests {
    use super::*;

    #[test]
    fn row_selection_coalesces_runs() {
        // hits 0,1,2 then 5 out of 7 → select 3, skip 2, select 1, skip 1.
        let sel = row_selection_from_sorted(&[0, 1, 2, 5], 7);
        let rows: Vec<RowSelector> = sel.into();
        assert_eq!(
            rows,
            vec![
                RowSelector::select(3),
                RowSelector::skip(2),
                RowSelector::select(1),
                RowSelector::skip(1),
            ]
        );
    }

    #[test]
    fn row_selection_leading_skip() {
        let sel = row_selection_from_sorted(&[3], 5);
        let rows: Vec<RowSelector> = sel.into();
        assert_eq!(
            rows,
            vec![
                RowSelector::skip(3),
                RowSelector::select(1),
                RowSelector::skip(1),
            ]
        );
    }

    #[test]
    fn intersect_sorted_is_the_sorted_and() {
        assert_eq!(intersect_sorted(&[0, 2, 5], &[4, 5]), vec![5]);
        assert_eq!(intersect_sorted(&[1, 2, 3], &[1, 2, 3]), vec![1, 2, 3]);
        assert_eq!(intersect_sorted(&[0, 1], &[2, 3]), Vec::<u64>::new());
        assert_eq!(intersect_sorted(&[], &[1, 2]), Vec::<u64>::new());
    }

    #[test]
    fn attr_eq_predicate_detects_json_get_str_equality() {
        use datafusion::prelude::{col, lit};
        let jget = || crate::json_get_str_udf().call(vec![col("attributes"), lit("k")]);

        // Either operand order → (key, value).
        assert_eq!(
            attr_eq_predicate(&jget().eq(lit("v1"))),
            Some(("k".to_owned(), "v1".to_owned()))
        );
        assert_eq!(
            attr_eq_predicate(&lit("v1").eq(jget())),
            Some(("k".to_owned(), "v1".to_owned()))
        );

        // Not claimed: a json_get_str on a different column, a non-Eq op, or a plain column equality.
        let jget_resource = crate::json_get_str_udf().call(vec![col("resource"), lit("k")]);
        assert_eq!(attr_eq_predicate(&jget_resource.eq(lit("v1"))), None);
        assert_eq!(attr_eq_predicate(&jget().gt(lit("v1"))), None);
        assert_eq!(attr_eq_predicate(&col("attributes").eq(lit("x"))), None);
    }

    /// A real `.tidx` drives `row_selection_for`: an attr-equality prunes to its exact rows, a
    /// body+attr conjunction intersects, and the cost gate falls back to a full scan when the
    /// combined predicate is not selective enough.
    #[test]
    fn attr_eq_row_selection_prunes_and_intersects() {
        use imbh_core::LogRow;
        fn logrow(body: &str, attrs: &str) -> LogRow {
            LogRow {
                time_unix_nano: 0,
                observed_time_unix_nano: None,
                service: None,
                severity_number: 0,
                severity_text: None,
                body: body.to_owned(),
                attributes: attrs.to_owned(),
                resource: String::new(),
                scope: String::new(),
                trace_id: None,
                span_id: None,
                flags: 0,
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let idx = dir.path().join("seg.tidx");
        let rows = [
            logrow("alpha", r#"{"env":"prod"}"#),    // 0
            logrow("beta", r#"{"env":"prod"}"#),     // 1
            logrow("alpha", r#"{"env":"prod"}"#),    // 2
            logrow("gamma", r#"{"env":"prod"}"#),    // 3
            logrow("delta", r#"{"env":"staging"}"#), // 4
            logrow("alpha", r#"{"env":"staging"}"#), // 5
        ];
        imbh_index::build_logs_index(&idx, &rows).unwrap();
        let seg = SegmentInput {
            parquet_path: dir.path().join("seg.parquet"),
            index_path: Some(idx),
            rows: rows.len() as u64,
            time_range: None,
        };

        let sel = |terms: &[&str], not_terms: &[&str], probes: &[(&str, &str)]| {
            let terms: Vec<String> = terms.iter().map(|t| (*t).to_owned()).collect();
            let not_terms: Vec<String> = not_terms.iter().map(|t| (*t).to_owned()).collect();
            let probes: Vec<(String, String)> = probes
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect();
            row_selection_for(&seg, &terms, &not_terms, &probes)
                .unwrap()
                .map(<Vec<RowSelector>>::from)
        };
        let want =
            |hits: &[u64]| Some(<Vec<RowSelector>>::from(row_selection_from_sorted(hits, 6)));

        // attr-only, selective (2/6 < 0.5) → selects exactly rows 4,5.
        assert_eq!(sel(&[], &[], &[("env", "staging")]), want(&[4, 5]));
        // body "alpha" ∩ env=staging → row 5 only (implicit AND).
        assert_eq!(sel(&["alpha"], &[], &[("env", "staging")]), want(&[5]));
        // attr env=prod is 4/6 ≥ 0.5 → cost gate → full scan (None).
        assert_eq!(sel(&[], &[], &[("env", "prod")]), None);
        // Contradictory equalities intersect to nothing → an all-skip selection (reads no rows).
        assert_eq!(
            sel(&[], &[], &[("env", "prod"), ("env", "staging")]),
            want(&[])
        );
        // A pure `!?` chain reduces to a Tantivy `+all -alpha -beta` (MustNot excludes any hit term):
        // alpha is at rows 0,2,5 and beta at row 1, so the survivors are 3,4 (2/6 < 0.5 → selective).
        assert_eq!(sel(&[], &["alpha", "beta"], &[]), want(&[3, 4]));
        // `+must -must_not` subtracts the must-not set from the must set: `+alpha -alpha` → nothing.
        assert_eq!(sel(&["alpha"], &["alpha"], &[]), want(&[]));
        // An index-less segment always falls back to a full scan.
        let no_index = SegmentInput {
            parquet_path: dir.path().join("seg.parquet"),
            index_path: None,
            rows: 6,
            time_range: None,
        };
        assert!(
            row_selection_for(
                &no_index,
                &[],
                &[],
                &[("env".to_owned(), "staging".to_owned())]
            )
            .unwrap()
            .is_none()
        );
    }

    /// End-to-end proof that a **parameterized** `matches(body, $1)` — the form the typed logs API
    /// now emits — actually consults the segment's `.tidx` rather than silently full-scanning. The
    /// index is built to DELIBERATELY OMIT the one matching row (its indexed body lacks the term),
    /// while the Parquet keeps it. So the two paths give *different* answers: if the index is
    /// consulted, `search_body` returns no hits → that row is pruned from the read → count 0; a
    /// silent full scan would read the row and the re-checked `matches` UDF would find it → count 1.
    /// Asserting count 0 proves (a) `$1` reached the provider as a literal (substituted before
    /// physical planning), (b) `matches_text_terms` extracted the term, and (c) `search_body` drove
    /// the `RowSelection`. Guards against parameterization severing the index pushdown.
    #[tokio::test(flavor = "current_thread")]
    #[allow(clippy::await_holding_lock)] // single-thread runtime; the guard just serializes vs. the bloom counter test
    async fn parameterized_matches_consults_the_logs_index() {
        use crate::TableInput;
        use datafusion::arrow::array::{Int64Array, StringArray};

        // `run_sql` → `scan()` bumps the process-global READ counter (under `#[cfg(test)]`), which the
        // bloom test asserts on — serialize against it.
        let _serial = prune_counters::SERIAL
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        use datafusion::arrow::datatypes::{DataType, Field, Schema};
        use datafusion::parquet::arrow::ArrowWriter;
        use datafusion::scalar::ScalarValue;
        use imbh_core::LogRow;
        use std::sync::Arc;

        fn logrow(body: &str) -> LogRow {
            LogRow {
                time_unix_nano: 0,
                observed_time_unix_nano: None,
                service: None,
                severity_number: 0,
                severity_text: None,
                body: body.to_owned(),
                attributes: String::new(),
                resource: String::new(),
                scope: String::new(),
                trace_id: None,
                span_id: None,
                flags: 0,
            }
        }

        let dir = tempfile::tempdir().unwrap();
        // Parquet keeps the real bodies (row 0 contains "alpha")…
        let parquet_bodies = ["alpha one", "beta two", "gamma three", "delta four"];
        // …but the index is built from bodies where row 0 does NOT contain "alpha", so a consulted
        // index cannot find it. (Only a genuine consultation can observe this divergence.)
        let index_bodies = ["zeta one", "beta two", "gamma three", "delta four"];
        let index_rows: Vec<LogRow> = index_bodies.iter().map(|b| logrow(b)).collect();

        let tidx = dir.path().join("seg.tidx");
        imbh_index::build_logs_index(&tidx, &index_rows).unwrap();

        let schema = Arc::new(Schema::new(vec![Field::new("body", DataType::Utf8, false)]));
        let batch = RecordBatch::try_new(
            schema.clone(),
            vec![Arc::new(StringArray::from(parquet_bodies.to_vec())) as _],
        )
        .unwrap();
        let parquet_path = dir.path().join("seg.parquet");
        {
            let file = std::fs::File::create(&parquet_path).unwrap();
            let mut w = ArrowWriter::try_new(file, schema.clone(), None).unwrap();
            w.write(&batch).unwrap();
            w.close().unwrap();
        }

        let table = TableInput {
            name: "t",
            schema: schema.clone(),
            buffer: RecordBatch::new_empty(schema.clone()),
            segments: vec![SegmentInput {
                parquet_path,
                index_path: Some(tidx),
                rows: parquet_bodies.len() as u64,
                time_range: None,
            }],
            text_column: Some("body"),
            bloom_columns: &[],
            time_column: None,
        };

        let (_schema, batches, stats) = crate::run_sql(
            vec![table],
            64 * 1024 * 1024,
            "SELECT count(*) AS c FROM t WHERE matches(body, $1)",
            vec![ScalarValue::Utf8(Some("alpha".to_owned()))],
        )
        .await
        .unwrap();

        // The scan stats corroborate the divergence proof: the `.tidx` was searched, and the
        // `RowSelection` pruned the (stale-indexed) row so nothing was read.
        assert!(stats.index_searched, "the `.tidx` was consulted");
        assert_eq!(
            stats.rows_scanned, 0,
            "the matching row was pruned by the index"
        );
        assert_eq!(stats.segments_scanned, 1);

        let c = batches[0]
            .column(0)
            .as_any()
            .downcast_ref::<Int64Array>()
            .unwrap()
            .value(0);
        assert_eq!(
            c, 0,
            "the (stale) `.tidx` was consulted for the parameterized `matches(body, $1)` and pruned \
             the matching row; a count of 1 would mean the index was bypassed for a full scan"
        );
    }
}

/// Bloom-filter segment pruning (ARCHITECTURE.md §8). Feature-independent: blooms live in the
/// Parquet file, so this path compiles and runs with or without `search`.
#[cfg(test)]
mod bloom_tests {
    use super::*;
    use datafusion::arrow::array::{ArrayRef, FixedSizeBinaryArray, StringArray};
    use datafusion::arrow::datatypes::{DataType, Field, Schema};
    use datafusion::parquet::arrow::ArrowWriter;
    use datafusion::parquet::file::properties::WriterProperties;
    use datafusion::parquet::schema::types::ColumnPath;
    use datafusion::prelude::SessionContext;

    fn spans_test_schema() -> SchemaRef {
        Arc::new(Schema::new(vec![
            Field::new("trace_id", DataType::FixedSizeBinary(16), false),
            Field::new("span_id", DataType::FixedSizeBinary(8), false),
            Field::new("name", DataType::Utf8, false),
        ]))
    }

    /// Write a one-row spans segment with bloom filters on both id columns, return its path + rows.
    fn write_segment_file(
        dir: &std::path::Path,
        file: &str,
        trace_id: [u8; 16],
        span_id: [u8; 8],
        name: &str,
    ) -> SegmentInput {
        let schema = spans_test_schema();
        let tid = FixedSizeBinaryArray::try_from_iter(std::iter::once(trace_id)).unwrap();
        let sid = FixedSizeBinaryArray::try_from_iter(std::iter::once(span_id)).unwrap();
        let names = StringArray::from(vec![name]);
        let batch = RecordBatch::try_new(
            schema.clone(),
            vec![
                Arc::new(tid) as ArrayRef,
                Arc::new(sid) as ArrayRef,
                Arc::new(names) as ArrayRef,
            ],
        )
        .unwrap();
        let props = WriterProperties::builder()
            .set_column_bloom_filter_enabled(ColumnPath::from("trace_id"), true)
            .set_column_bloom_filter_enabled(ColumnPath::from("span_id"), true)
            .build();
        let path = dir.join(file);
        let f = std::fs::File::create(&path).unwrap();
        let mut w = ArrowWriter::try_new(f, schema, Some(props)).unwrap();
        w.write(&batch).unwrap();
        w.close().unwrap();
        SegmentInput {
            parquet_path: path,
            index_path: None,
            rows: 1,
            time_range: None,
        }
    }

    async fn run(provider: SegmentTableProvider, sql: &str) -> Vec<RecordBatch> {
        let ctx = SessionContext::new();
        ctx.register_table("spans", Arc::new(provider)).unwrap();
        ctx.sql(sql).await.unwrap().collect().await.unwrap()
    }

    fn provider_over(segments: Vec<SegmentInput>) -> SegmentTableProvider {
        let schema = spans_test_schema();
        SegmentTableProvider::new(
            schema.clone(),
            RecordBatch::new_empty(schema),
            segments,
            None,
            vec!["trace_id".to_owned(), "span_id".to_owned()],
            None,
            Arc::new(ScanAccum::default()),
        )
    }

    fn row_count(batches: &[RecordBatch]) -> usize {
        batches.iter().map(RecordBatch::num_rows).sum()
    }

    fn name_at(batches: &[RecordBatch], row: usize) -> String {
        batches[0]
            .column(0)
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap()
            .value(row)
            .to_owned()
    }

    /// The bloom-pruning read path, as a single test so the global prune counters (which no other
    /// test in this binary touches) reflect one scan at a time. Covers, in sequence:
    /// a `trace_id` point lookup that skips the non-matching segment; a fully-absent id that prunes
    /// every segment; a `span_id` lookup (so the second written bloom is exercised); and a
    /// bloom-less segment that is always read (blooms only ever rule a value *out*).
    #[tokio::test(flavor = "current_thread")]
    #[allow(clippy::await_holding_lock)] // single-thread runtime; the guard serializes the global counter asserts
    async fn point_lookup_prunes_nonmatching_segments() {
        // Serialize against any other test whose `scan()` bumps the process-global READ/PRUNED
        // counters this test asserts on.
        let _serial = super::prune_counters::SERIAL
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        // Segment A: trace 0xAA…, span 0x01…. Segment B: trace 0xBB…, span 0x02….
        let seg_a = write_segment_file(dir.path(), "a.parquet", [0xAA; 16], [0x01; 8], "a-span");
        let seg_b = write_segment_file(dir.path(), "b.parquet", [0xBB; 16], [0x02; 8], "b-span");

        // Look up trace A: segment B must be pruned (its bloom proves 0xAA absent), A read.
        prune_counters::reset();
        let hit = run(
            provider_over(vec![seg_a.clone(), seg_b.clone()]),
            &format!(
                "SELECT name FROM spans WHERE trace_id = X'{}'",
                "aa".repeat(16)
            ),
        )
        .await;
        assert_eq!(row_count(&hit), 1, "only trace A's span is returned");
        assert_eq!(name_at(&hit, 0), "a-span");
        assert_eq!(
            prune_counters::pruned(),
            1,
            "segment B skipped via its bloom"
        );
        assert_eq!(prune_counters::read(), 1, "segment A read");

        // Look up a trace absent from both: both segments pruned, no rows.
        prune_counters::reset();
        let miss = run(
            provider_over(vec![seg_a.clone(), seg_b.clone()]),
            &format!(
                "SELECT name FROM spans WHERE trace_id = X'{}'",
                "cc".repeat(16)
            ),
        )
        .await;
        assert_eq!(row_count(&miss), 0);
        assert_eq!(prune_counters::pruned(), 2, "both segments skipped");
        assert_eq!(prune_counters::read(), 0);

        // A span_id lookup exercises the span_id bloom: segment A pruned, B read.
        prune_counters::reset();
        let by_span = run(
            provider_over(vec![seg_a, seg_b]),
            &format!(
                "SELECT name FROM spans WHERE span_id = X'{}'",
                "02".repeat(8)
            ),
        )
        .await;
        assert_eq!(row_count(&by_span), 1);
        assert_eq!(name_at(&by_span, 0), "b-span");
        assert_eq!(
            prune_counters::pruned(),
            1,
            "segment A skipped via its bloom"
        );
        assert_eq!(prune_counters::read(), 1, "segment B read");

        // A segment written WITHOUT bloom filters is never pruned (still correct on older segments).
        let schema = spans_test_schema();
        let batch = RecordBatch::try_new(
            schema.clone(),
            vec![
                Arc::new(FixedSizeBinaryArray::try_from_iter(std::iter::once([0xAA; 16])).unwrap())
                    as ArrayRef,
                Arc::new(FixedSizeBinaryArray::try_from_iter(std::iter::once([0x01; 8])).unwrap())
                    as ArrayRef,
                Arc::new(StringArray::from(vec!["a-span"])) as ArrayRef,
            ],
        )
        .unwrap();
        let nobloom_path = dir.path().join("nobloom.parquet");
        let mut w =
            ArrowWriter::try_new(std::fs::File::create(&nobloom_path).unwrap(), schema, None)
                .unwrap();
        w.write(&batch).unwrap();
        w.close().unwrap();
        prune_counters::reset();
        let read_all = run(
            provider_over(vec![SegmentInput {
                parquet_path: nobloom_path,
                index_path: None,
                rows: 1,
                time_range: None,
            }]),
            &format!(
                "SELECT name FROM spans WHERE trace_id = X'{}'",
                "aa".repeat(16)
            ),
        )
        .await;
        assert_eq!(
            row_count(&read_all),
            1,
            "the bloom-less segment is still read"
        );
        assert_eq!(prune_counters::pruned(), 0, "no bloom → never pruned");
        assert_eq!(prune_counters::read(), 1);
    }

    /// The **trace-search** shape: `trace_id IN (…)` over k raw ids must prune the segments holding
    /// none of them — the whole point of `TracesApi::search`'s phase-2 fetch. Covers both physical
    /// forms of the predicate, since DataFusion's simplifier rewrites a short `IN` (≤ 3 values) into
    /// an `OR` chain *before* filter pushdown, and leaves longer ones as an `InList`. `NOT IN` must
    /// prune nothing (a bloom proves absence, which says nothing about a negated membership).
    #[tokio::test(flavor = "current_thread")]
    #[allow(clippy::await_holding_lock)] // single-thread runtime; the guard serializes the global counter asserts
    async fn in_list_lookup_prunes_segments_holding_no_candidate() {
        let _serial = super::prune_counters::SERIAL
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let seg_a = write_segment_file(dir.path(), "a.parquet", [0xAA; 16], [0x01; 8], "a-span");
        let seg_b = write_segment_file(dir.path(), "b.parquet", [0xBB; 16], [0x02; 8], "b-span");
        let seg_c = write_segment_file(dir.path(), "c.parquet", [0xCC; 16], [0x03; 8], "c-span");
        let segs = vec![seg_a, seg_b, seg_c];

        // Two candidates (≤ THRESHOLD_INLINE_INLIST → an `OR` chain by the time it is pushed down):
        // segment C holds neither and is skipped.
        prune_counters::reset();
        let two = run(
            provider_over(segs.clone()),
            &format!(
                "SELECT name FROM spans WHERE trace_id IN (X'{}', X'{}')",
                "aa".repeat(16),
                "bb".repeat(16)
            ),
        )
        .await;
        assert_eq!(row_count(&two), 2, "both candidate traces' spans returned");
        assert_eq!(
            prune_counters::pruned(),
            1,
            "segment C skipped via its bloom"
        );
        assert_eq!(prune_counters::read(), 2);

        // Four candidates (> the inline threshold → a genuine `Expr::InList` reaches the provider),
        // two of them absent from every segment: only A and B are read.
        prune_counters::reset();
        let four = run(
            provider_over(segs.clone()),
            &format!(
                "SELECT name FROM spans WHERE trace_id IN (X'{}', X'{}', X'{}', X'{}')",
                "aa".repeat(16),
                "bb".repeat(16),
                "dd".repeat(16),
                "ee".repeat(16)
            ),
        )
        .await;
        assert_eq!(row_count(&four), 2);
        assert_eq!(
            prune_counters::pruned(),
            1,
            "segment C skipped via its bloom"
        );
        assert_eq!(prune_counters::read(), 2);

        // Every candidate absent → every segment pruned, empty (and still correct) result.
        prune_counters::reset();
        let none = run(
            provider_over(segs.clone()),
            &format!(
                "SELECT name FROM spans WHERE trace_id IN (X'{}', X'{}')",
                "dd".repeat(16),
                "ee".repeat(16)
            ),
        )
        .await;
        assert_eq!(row_count(&none), 0);
        assert_eq!(prune_counters::pruned(), 3, "all three segments skipped");
        assert_eq!(prune_counters::read(), 0);

        // `NOT IN` claims nothing: absence from a bloom cannot rule the segment out.
        prune_counters::reset();
        let negated = run(
            provider_over(segs),
            &format!(
                "SELECT name FROM spans WHERE trace_id NOT IN (X'{}', X'{}')",
                "aa".repeat(16),
                "bb".repeat(16)
            ),
        )
        .await;
        assert_eq!(row_count(&negated), 1, "only segment C's span survives");
        assert_eq!(prune_counters::pruned(), 0, "NOT IN proves no absence");
        assert_eq!(prune_counters::read(), 3);
    }

    /// The probe extractor itself, over the shapes it must and must not claim.
    #[test]
    fn bloom_probe_extraction() {
        use datafusion::prelude::{col, lit};

        let cols = vec!["trace_id".to_owned(), "span_id".to_owned()];
        let id = |b: u8| ScalarValue::FixedSizeBinary(16, Some(vec![b; 16]));

        // Multi-value `IN` over the bloom column → the whole candidate set.
        assert_eq!(
            bloom_probe(
                &col("trace_id").in_list(vec![lit(id(0xAA)), lit(id(0xBB))], false),
                &cols
            ),
            Some(("trace_id".to_owned(), vec![vec![0xAA; 16], vec![0xBB; 16]]))
        );
        // Single-value `IN` → a one-element set (same as the `=` point lookup).
        assert_eq!(
            bloom_probe(&col("trace_id").in_list(vec![lit(id(0xAA))], false), &cols),
            Some(("trace_id".to_owned(), vec![vec![0xAA; 16]]))
        );
        assert_eq!(
            bloom_probe(&col("trace_id").eq(lit(id(0xAA))), &cols),
            Some(("trace_id".to_owned(), vec![vec![0xAA; 16]]))
        );
        // Negated: `NOT IN` proves nothing about absence.
        assert!(
            bloom_probe(
                &col("trace_id").in_list(vec![lit(id(0xAA)), lit(id(0xBB))], true),
                &cols
            )
            .is_none()
        );
        // A non-binary list element: the predicate could hold for a value we cannot probe.
        assert!(
            bloom_probe(
                &col("trace_id").in_list(vec![lit(id(0xAA)), lit("aa")], false),
                &cols
            )
            .is_none()
        );
        // Not a bloom-indexed column.
        assert!(bloom_probe(&col("name").in_list(vec![lit(id(0xAA))], false), &cols).is_none());
        // A `CAST` the type-coercer may wrap around the column is still unwrapped.
        assert_eq!(
            bloom_probe(
                &Expr::Cast(Cast::new(
                    Box::new(col("trace_id")),
                    DataType::FixedSizeBinary(16)
                ))
                .in_list(vec![lit(id(0xAA))], false),
                &cols
            ),
            Some(("trace_id".to_owned(), vec![vec![0xAA; 16]]))
        );
        // The `OR` chain a short `IN` is simplified into: same column → the candidate sets union.
        assert_eq!(
            bloom_probe(
                &col("trace_id")
                    .eq(lit(id(0xAA)))
                    .or(col("trace_id").eq(lit(id(0xBB)))),
                &cols
            ),
            Some(("trace_id".to_owned(), vec![vec![0xAA; 16], vec![0xBB; 16]]))
        );
        // …but a disjunct on another column (or an unrecognized one) means a row can match without
        // `trace_id` holding a candidate — nothing is claimed.
        assert!(
            bloom_probe(
                &col("trace_id")
                    .eq(lit(id(0xAA)))
                    .or(col("span_id")
                        .eq(lit(ScalarValue::FixedSizeBinary(8, Some(vec![0x01; 8]))))),
                &cols
            )
            .is_none()
        );
        assert!(
            bloom_probe(
                &col("trace_id")
                    .eq(lit(id(0xAA)))
                    .or(col("name").eq(lit("a-span"))),
                &cols
            )
            .is_none()
        );
    }

    /// The scan is **lazy** (prescription I-4a): building the plan reads nothing, and pulling a single
    /// batch reads exactly one segment — not all of them. Draining the rest then reads the remainder.
    /// This is the regression guard against the old eager `MemTable` path, which read and decoded
    /// every segment inside `scan()` before any batch was polled. Executes the provider's own plan
    /// directly (no `CoalesceBatchesExec` above it), so one `poll_next` maps to one source batch.
    #[tokio::test(flavor = "current_thread")]
    #[allow(clippy::await_holding_lock)] // single-thread runtime; the guard serializes the global counter bumps
    async fn scan_reads_one_segment_per_poll() {
        use futures::StreamExt;
        // `scan()`/drain bump the process-global prune counters (cfg(test)); serialize with the other
        // test in this binary that asserts on them.
        let _serial = super::prune_counters::SERIAL
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let segs = vec![
            write_segment_file(dir.path(), "a.parquet", [0xA1; 16], [0x01; 8], "a"),
            write_segment_file(dir.path(), "b.parquet", [0xB2; 16], [0x02; 8], "b"),
            write_segment_file(dir.path(), "c.parquet", [0xC3; 16], [0x03; 8], "c"),
        ];
        let schema = spans_test_schema();
        let stats = Arc::new(ScanAccum::default());
        let provider = SegmentTableProvider::new(
            schema.clone(),
            RecordBatch::new_empty(schema),
            segs,
            None,
            vec!["trace_id".to_owned(), "span_id".to_owned()],
            None,
            stats.clone(),
        );
        let ctx = SessionContext::new();
        // No filter → no bloom pruning; all three segments are readable. Execute the StreamingTableExec
        // directly so there is no coalescing layer folding several source batches into one poll.
        let plan = provider.scan(&ctx.state(), None, &[], None).await.unwrap();
        let mut stream = plan.execute(0, ctx.task_ctx()).unwrap();

        // Planning/execute() reads nothing — the eager path would already have read all three here.
        assert_eq!(
            stats.snapshot().segments_scanned,
            0,
            "building the plan reads no segments"
        );

        let first = stream.next().await.unwrap().unwrap();
        assert_eq!(first.num_rows(), 1);
        assert_eq!(
            stats.snapshot().segments_scanned,
            1,
            "one poll reads exactly one segment, not all three"
        );

        let mut rows = first.num_rows();
        while let Some(b) = stream.next().await {
            rows += b.unwrap().num_rows();
        }
        assert_eq!(rows, 3, "the full result is three rows");
        assert_eq!(
            stats.snapshot().segments_scanned,
            3,
            "draining the stream reads every segment"
        );
    }
}

/// Time/range segment pruning from Parquet row-group statistics. Feature-independent (statistics
/// live in the Parquet file, like blooms), so this compiles and runs with or without `search`.
#[cfg(test)]
mod range_tests {
    use super::*;
    use datafusion::arrow::array::{Array, ArrayRef, StringArray, TimestampNanosecondArray};
    use datafusion::arrow::datatypes::{Field, Schema};
    use datafusion::parquet::arrow::ArrowWriter;
    use datafusion::parquet::file::properties::WriterProperties;
    use datafusion::prelude::{SessionContext, col, lit};

    fn ts_type() -> DataType {
        DataType::Timestamp(TimeUnit::Nanosecond, Some("UTC".into()))
    }

    /// A miniature `logs`-shaped table: the nanosecond `time` column the range pruning targets, plus
    /// a text column so a result row can be identified.
    fn logs_test_schema() -> SchemaRef {
        Arc::new(Schema::new(vec![
            Field::new("time", ts_type(), false),
            Field::new("body", DataType::Utf8, false),
        ]))
    }

    /// Write a segment holding one row per entry of `times` (body = `row-<time>`). `rows_per_group`
    /// forces the Parquet row-group size so the *within*-segment path can be exercised; the
    /// production writer emits one row group per segment.
    fn write_time_segment(
        dir: &std::path::Path,
        file: &str,
        times: &[i64],
        rows_per_group: Option<usize>,
    ) -> SegmentInput {
        let schema = logs_test_schema();
        let time_col = TimestampNanosecondArray::from(times.to_vec()).with_timezone("UTC");
        let bodies: Vec<String> = times.iter().map(|t| format!("row-{t}")).collect();
        let batch = RecordBatch::try_new(
            schema.clone(),
            vec![
                Arc::new(time_col) as ArrayRef,
                Arc::new(StringArray::from(bodies)) as ArrayRef,
            ],
        )
        .unwrap();
        let mut props = WriterProperties::builder();
        if let Some(n) = rows_per_group {
            props = props.set_max_row_group_row_count(Some(n));
        }
        let path = dir.join(file);
        let f = std::fs::File::create(&path).unwrap();
        let mut w = ArrowWriter::try_new(f, schema, Some(props.build())).unwrap();
        w.write(&batch).unwrap();
        w.close().unwrap();
        SegmentInput {
            parquet_path: path,
            index_path: None,
            rows: times.len() as u64,
            time_range: None,
        }
    }

    /// Run `sql` over a `logs` table of these segments, returning the `body` of each result row
    /// (**sorted** — the query carries no `ORDER BY`, so batch order is not part of the contract) and
    /// the scan statistics.
    async fn run(segments: Vec<SegmentInput>, sql: &str) -> (Vec<String>, ScanStats) {
        run_inner(segments, sql, None).await
    }

    /// [`run`] with the manifest-range skip armed: `time_column` names the column each segment's
    /// declared `time_range` describes.
    async fn run_with_time_column(
        segments: Vec<SegmentInput>,
        sql: &str,
    ) -> (Vec<String>, ScanStats) {
        run_inner(segments, sql, Some("time".to_owned())).await
    }

    async fn run_inner(
        segments: Vec<SegmentInput>,
        sql: &str,
        time_column: Option<String>,
    ) -> (Vec<String>, ScanStats) {
        let accum = Arc::new(ScanAccum::default());
        let schema = logs_test_schema();
        let provider = SegmentTableProvider::new(
            schema.clone(),
            RecordBatch::new_empty(schema),
            segments,
            Some("body".to_owned()),
            Vec::new(),
            time_column,
            accum.clone(),
        );
        let ctx = SessionContext::new();
        ctx.register_table("logs", Arc::new(provider)).unwrap();
        let batches = ctx.sql(sql).await.unwrap().collect().await.unwrap();
        let mut out = Vec::new();
        for b in &batches {
            let c = b
                .column(0)
                .as_any()
                .downcast_ref::<StringArray>()
                .expect("every test here selects `body`");
            for i in 0..c.len() {
                out.push(c.value(i).to_owned());
            }
        }
        out.sort();
        (out, accum.snapshot())
    }

    /// The whole point of the change: a bounded `WHERE time …` must not READ the segments whose
    /// Parquet statistics prove they hold nothing in range. Asserted on the process-global prune
    /// counters (a read segment bumps READ, a skipped one PRUNED) — never on timing. Covers, in one
    /// test so the counters describe one scan at a time: an interior range (both flanking segments
    /// skipped), a range matching nothing at all, an *inclusive/exclusive boundary* (a row sitting
    /// exactly on the bound is still returned, and so is its segment), and an unbounded query (every
    /// segment read — pruning must never fire without a predicate).
    #[tokio::test(flavor = "current_thread")]
    #[allow(clippy::await_holding_lock)] // single-thread runtime; the guard serializes the global counter asserts
    async fn time_range_prunes_out_of_range_segments() {
        // Serialize against every other test whose `scan()` bumps the global READ/PRUNED counters.
        let _serial = prune_counters::SERIAL
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        // Three disjoint segments: A [100,200], B [300,400], C [500,600].
        let segs = || {
            vec![
                write_time_segment(dir.path(), "a.parquet", &[100, 200], None),
                write_time_segment(dir.path(), "b.parquet", &[300, 400], None),
                write_time_segment(dir.path(), "c.parquet", &[500, 600], None),
            ]
        };

        // 1. An interior range: only B can match, so A and C are never read.
        prune_counters::reset();
        let (rows, stats) = run(
            segs(),
            r#"SELECT body FROM logs
               WHERE CAST("time" AS BIGINT) >= 300 AND CAST("time" AS BIGINT) < 500"#,
        )
        .await;
        assert_eq!(rows, vec!["row-300", "row-400"]);
        assert_eq!(prune_counters::read(), 1, "only segment B is read");
        assert_eq!(prune_counters::pruned(), 2, "segments A and C are skipped");
        assert_eq!(stats.segments_scanned, 1);
        assert_eq!(stats.segments_pruned, 2);
        assert_eq!(stats.rows_scanned, 2, "only B's two rows are materialized");

        // 2. A range no segment overlaps: every segment is skipped, nothing is decoded at all.
        prune_counters::reset();
        let (rows, stats) = run(
            segs(),
            r#"SELECT body FROM logs WHERE CAST("time" AS BIGINT) > 1000"#,
        )
        .await;
        assert!(rows.is_empty());
        assert_eq!(prune_counters::read(), 0);
        assert_eq!(prune_counters::pruned(), 3, "all three segments skipped");
        assert_eq!(stats.rows_scanned, 0, "no row was decoded");

        // 3. Boundary, inclusive: `>= 200 AND <= 300` sits exactly on A's max and B's min. Both rows
        //    must come back and both segments must be read; only C is skipped. An off-by-one in
        //    `RangeProbe::excludes` would silently lose one of these rows.
        prune_counters::reset();
        let (rows, _) = run(
            segs(),
            r#"SELECT body FROM logs
               WHERE CAST("time" AS BIGINT) >= 200 AND CAST("time" AS BIGINT) <= 300"#,
        )
        .await;
        assert_eq!(
            rows,
            vec!["row-200", "row-300"],
            "rows sitting exactly on an inclusive bound are returned"
        );
        assert_eq!(
            prune_counters::read(),
            2,
            "A and B each hold a boundary row"
        );
        assert_eq!(prune_counters::pruned(), 1, "only C is out of range");

        // 4. Boundary, exclusive: `> 200 AND < 300` admits neither boundary row — and therefore no
        //    segment, since A's max is 200 and B's min is 300.
        prune_counters::reset();
        let (rows, _) = run(
            segs(),
            r#"SELECT body FROM logs
               WHERE CAST("time" AS BIGINT) > 200 AND CAST("time" AS BIGINT) < 300"#,
        )
        .await;
        assert!(rows.is_empty(), "the exclusive bounds admit no row");
        assert_eq!(prune_counters::pruned(), 3);

        // 5. No time bound at all: nothing may be pruned — every segment is read in full.
        prune_counters::reset();
        let (rows, stats) = run(segs(), "SELECT body FROM logs").await;
        assert_eq!(rows.len(), 6, "all six rows come back");
        assert_eq!(prune_counters::read(), 3, "every segment is read");
        assert_eq!(prune_counters::pruned(), 0, "nothing is pruned");
        assert_eq!(stats.rows_scanned, 6);

        // 6. A non-range predicate is not a probe either: a `body` equality prunes no segment.
        prune_counters::reset();
        let (rows, _) = run(segs(), "SELECT body FROM logs WHERE body = 'row-500'").await;
        assert_eq!(rows, vec!["row-500"]);
        assert_eq!(prune_counters::read(), 3, "a text equality prunes nothing");
        assert_eq!(prune_counters::pruned(), 0);
    }

    /// A segment with several row groups is narrowed to the surviving ones rather than skipped or
    /// read whole: the segment is read (it *does* hold matching rows) but only the in-range row
    /// group's rows are materialized. The production writer emits one row group per segment, so this
    /// covers the `RowGroups::Subset` arm a single-row-group segment never reaches.
    #[tokio::test(flavor = "current_thread")]
    #[allow(clippy::await_holding_lock)] // single-thread runtime; the guard serializes the global counter asserts
    async fn multi_row_group_segment_reads_only_the_surviving_groups() {
        let _serial = prune_counters::SERIAL
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        // Six rows, two per row group → groups [0,100], [200,300], [400,500].
        let seg = write_time_segment(
            dir.path(),
            "multi.parquet",
            &[0, 100, 200, 300, 400, 500],
            Some(2),
        );

        prune_counters::reset();
        let (rows, stats) = run(
            vec![seg],
            r#"SELECT body FROM logs
               WHERE CAST("time" AS BIGINT) >= 200 AND CAST("time" AS BIGINT) <= 300"#,
        )
        .await;
        assert_eq!(rows, vec!["row-200", "row-300"]);
        assert_eq!(prune_counters::read(), 1, "the segment itself is read");
        assert_eq!(prune_counters::pruned(), 0);
        assert_eq!(
            stats.rows_scanned, 2,
            "only the middle row group's rows are decoded, not all six"
        );
    }

    /// The `RowGroups::Subset` + Tantivy-`RowSelection` combination, driven through `open_segment`
    /// directly (the selection is handed in rather than derived from a `.tidx`, so this runs without
    /// the `search` feature too). The selection is in **whole-file** row ordinals; handing the
    /// surviving row groups to `with_row_groups` would re-base it onto the kept groups and return the
    /// wrong rows, so the two must be intersected instead. Rows 0..5 hold times 0,100,…,500 in three
    /// two-row groups; the selection picks rows {1,3,4} and the range keeps group 1 (rows {2,3}) — so
    /// exactly row 3 (time 300) must come back.
    #[test]
    fn row_group_subset_intersects_a_row_selection_instead_of_rebasing_it() {
        let dir = tempfile::tempdir().unwrap();
        let seg = write_time_segment(
            dir.path(),
            "combo.parquet",
            &[0, 100, 200, 300, 400, 500],
            Some(2),
        );
        let selection = RowSelection::from(vec![
            RowSelector::skip(1),   // row 0
            RowSelector::select(1), // row 1
            RowSelector::skip(1),   // row 2
            RowSelector::select(2), // rows 3, 4
            RowSelector::skip(1),   // row 5
        ]);
        let probe = |op, value| ColumnRangeProbe {
            column: "time".to_owned(),
            probe: RangeProbe { op, value },
        };
        let probes = vec![probe(RangeOp::GtEq, 200), probe(RangeOp::LtEq, 300)];
        let reader = open_segment(&seg.parquet_path, Some(selection), &[], &probes)
            .unwrap()
            .expect("the segment holds matching rows");
        let mut bodies: Vec<String> = Vec::new();
        for batch in reader {
            let batch = batch.unwrap();
            let c = batch
                .column(1)
                .as_any()
                .downcast_ref::<StringArray>()
                .unwrap()
                .clone();
            for i in 0..c.len() {
                bodies.push(c.value(i).to_owned());
            }
        }
        assert_eq!(
            bodies,
            vec!["row-300"],
            "the intersection of the selection {{1,3,4}} with the surviving group {{2,3}}"
        );
    }

    /// A segment carrying no statistics for the probed column must be read, never skipped —
    /// statistics may only ever rule a row group *out*. Guards older/foreign segments.
    #[tokio::test(flavor = "current_thread")]
    #[allow(clippy::await_holding_lock)] // single-thread runtime; the guard serializes the global counter asserts
    async fn a_segment_without_statistics_is_always_read() {
        use datafusion::parquet::file::properties::EnabledStatistics;
        let _serial = prune_counters::SERIAL
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let schema = logs_test_schema();
        let batch = RecordBatch::try_new(
            schema.clone(),
            vec![
                Arc::new(TimestampNanosecondArray::from(vec![100i64, 200]).with_timezone("UTC"))
                    as ArrayRef,
                Arc::new(StringArray::from(vec!["row-100", "row-200"])) as ArrayRef,
            ],
        )
        .unwrap();
        let path = dir.path().join("nostats.parquet");
        let props = WriterProperties::builder()
            .set_statistics_enabled(EnabledStatistics::None)
            .build();
        let mut w =
            ArrowWriter::try_new(std::fs::File::create(&path).unwrap(), schema, Some(props))
                .unwrap();
        w.write(&batch).unwrap();
        w.close().unwrap();

        prune_counters::reset();
        let (rows, _) = run(
            vec![SegmentInput {
                parquet_path: path,
                index_path: None,
                rows: 2,
                time_range: None,
            }],
            r#"SELECT body FROM logs WHERE CAST("time" AS BIGINT) > 1000"#,
        )
        .await;
        assert!(rows.is_empty(), "the filter above the scan still applies");
        assert_eq!(
            prune_counters::read(),
            1,
            "no statistics ⇒ the segment must be read"
        );
        assert_eq!(prune_counters::pruned(), 0);
    }

    /// `RangeProbe::excludes` is the one place correctness rests: it must answer `true` only when
    /// *no* value in `[min, max]` can satisfy the comparison. Checked against the inclusive edges of
    /// a `[10, 20]` row group, where every off-by-one shows up.
    #[test]
    fn excludes_is_the_exact_dual_of_the_operator() {
        let p = |op, value| RangeProbe { op, value };
        // col > v: impossible iff max <= v.
        assert!(p(RangeOp::Gt, 20).excludes(10, 20));
        assert!(!p(RangeOp::Gt, 19).excludes(10, 20));
        // col >= v: impossible iff max < v.
        assert!(p(RangeOp::GtEq, 21).excludes(10, 20));
        assert!(!p(RangeOp::GtEq, 20).excludes(10, 20));
        // col < v: impossible iff min >= v.
        assert!(p(RangeOp::Lt, 10).excludes(10, 20));
        assert!(!p(RangeOp::Lt, 11).excludes(10, 20));
        // col <= v: impossible iff min > v.
        assert!(p(RangeOp::LtEq, 9).excludes(10, 20));
        assert!(!p(RangeOp::LtEq, 10).excludes(10, 20));
        // col = v: impossible iff v falls outside [min, max] — both edges are inside.
        assert!(p(RangeOp::Eq, 9).excludes(10, 20));
        assert!(p(RangeOp::Eq, 21).excludes(10, 20));
        assert!(!p(RangeOp::Eq, 10).excludes(10, 20));
        assert!(!p(RangeOp::Eq, 20).excludes(10, 20));
        // A single-valued row group (min == max) behaves the same.
        assert!(p(RangeOp::Gt, 5).excludes(5, 5));
        assert!(!p(RangeOp::GtEq, 5).excludes(5, 5));
    }

    /// Which expression shapes are claimed as a range probe — and, just as important, which are not.
    #[test]
    fn stats_range_probe_claims_only_value_preserving_shapes() {
        let schema: SchemaRef = Arc::new(Schema::new(vec![
            Field::new("time", ts_type(), false),
            Field::new("body", DataType::Utf8, false),
            Field::new("n", DataType::Int64, false),
            Field::new("f", DataType::Float64, false),
        ]));
        let probe = |e: Expr| stats_range_probe(&e, &schema);
        let want = |c: &str, op, value| {
            Some(ColumnRangeProbe {
                column: c.to_owned(),
                probe: RangeProbe { op, value },
            })
        };
        let ts = |v: i64| {
            lit(ScalarValue::TimestampNanosecond(
                Some(v),
                Some("UTC".into()),
            ))
        };
        let as_i64 = |e: Expr| Expr::Cast(Cast::new(Box::new(e), DataType::Int64));

        // The bare timestamp comparison…
        assert_eq!(
            probe(col("time").gt_eq(ts(5))),
            want("time", RangeOp::GtEq, 5)
        );
        // …and the `CAST("time" AS BIGINT) >= 5` form every typed query builder emits.
        assert_eq!(
            probe(as_i64(col("time")).gt_eq(lit(5i64))),
            want("time", RangeOp::GtEq, 5)
        );
        // A literal on the left mirrors the operator rather than being dropped.
        assert_eq!(
            probe(lit(5i64).lt(as_i64(col("time")))),
            want("time", RangeOp::Gt, 5)
        );
        // A plain Int64 column and an equality both qualify.
        assert_eq!(probe(col("n").lt(lit(7i64))), want("n", RangeOp::Lt, 7));
        assert_eq!(probe(col("n").eq(lit(7i64))), want("n", RangeOp::Eq, 7));

        // NOT claimed: `!=` (min/max prove nothing about it), a non-INT64 column, an unknown column,
        // and a non-literal operand.
        assert_eq!(probe(col("n").not_eq(lit(7i64))), None);
        assert_eq!(probe(col("f").gt(lit(1.5f64))), None);
        assert_eq!(probe(col("body").gt(lit("x"))), None);
        assert_eq!(probe(col("nope").gt(lit(1i64))), None);
        assert_eq!(probe(col("n").gt(col("n"))), None);
        // A unit-changing cast rescales the value, so a statistics bound in the source unit does not
        // transfer — not claimed.
        assert_eq!(
            probe(
                Expr::Cast(Cast::new(
                    Box::new(col("time")),
                    DataType::Timestamp(TimeUnit::Millisecond, None),
                ))
                .gt(lit(ScalarValue::TimestampMillisecond(Some(5), None)))
            ),
            None
        );
        // A literal outside the column's own domain yields no sound bound either, in either
        // direction.
        assert_eq!(probe(col("time").gt(lit(5i64))), None);
        assert_eq!(probe(col("n").gt(ts(5))), None);
    }

    /// Option (a), the **manifest-range skip**: a segment whose declared `[min, max]` cannot satisfy
    /// the pushed time predicate must be ruled out with *no file access at all*.
    ///
    /// The assertion is deliberately brutal — the out-of-range segments' Parquet files are **deleted**
    /// before the query runs. Any code path that opens them (a `File::open`, a footer read, a `.tidx`
    /// search) fails loudly instead of quietly costing I/O, so this test cannot pass vacuously the way
    /// a counter assertion could if the skip merely moved earlier.
    #[tokio::test(flavor = "current_thread")]
    #[allow(clippy::await_holding_lock)] // single-thread runtime; the guard serializes the global counter asserts
    async fn manifest_range_skip_never_opens_the_file() {
        let _serial = prune_counters::SERIAL
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let mut segs = vec![
            write_time_segment(dir.path(), "a.parquet", &[100, 200], None),
            write_time_segment(dir.path(), "b.parquet", &[300, 400], None),
            write_time_segment(dir.path(), "c.parquet", &[500, 600], None),
        ];
        // Declare each segment's bounds the way the manifest does: inclusive on both ends.
        segs[0].time_range = Some((100, 200));
        segs[1].time_range = Some((300, 400));
        segs[2].time_range = Some((500, 600));
        // A and C must never be touched. Make that physically true.
        std::fs::remove_file(dir.path().join("a.parquet")).unwrap();
        std::fs::remove_file(dir.path().join("c.parquet")).unwrap();

        prune_counters::reset();
        let (rows, stats) = run_with_time_column(
            segs,
            "SELECT body FROM logs WHERE CAST(\"time\" AS BIGINT) >= 300 \
             AND CAST(\"time\" AS BIGINT) < 500",
        )
        .await;
        assert_eq!(rows, vec!["row-300", "row-400"]);
        assert_eq!(stats.segments_scanned, 1, "only B is opened");
        assert_eq!(stats.segments_pruned, 2, "A and C skipped unopened");
        assert_eq!(prune_counters::read(), 1);
        assert_eq!(prune_counters::pruned(), 2);
    }

    /// The manifest bounds are **inclusive**, so a row sitting exactly on a half-open range's `start`
    /// keeps its segment. Guards the off-by-one that would silently drop boundary rows — the failure
    /// mode a range skip is most likely to introduce and least likely to be noticed.
    #[tokio::test(flavor = "current_thread")]
    #[allow(clippy::await_holding_lock)]
    async fn manifest_range_skip_honors_inclusive_bounds() {
        let _serial = prune_counters::SERIAL
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let segs = || {
            let mut v = vec![
                write_time_segment(dir.path(), "a.parquet", &[100, 200], None),
                write_time_segment(dir.path(), "b.parquet", &[300, 400], None),
            ];
            v[0].time_range = Some((100, 200));
            v[1].time_range = Some((300, 400));
            v
        };

        // [200, 300) — A's max is exactly 200 and must survive; B's min is 300, outside the half-open
        // end, so B is prunable.
        prune_counters::reset();
        let (rows, stats) = run_with_time_column(
            segs(),
            "SELECT body FROM logs WHERE CAST(\"time\" AS BIGINT) >= 200 \
             AND CAST(\"time\" AS BIGINT) < 300",
        )
        .await;
        assert_eq!(rows, vec!["row-200"], "the boundary row is returned");
        assert_eq!(stats.segments_pruned, 1, "only B is skipped");

        // No predicate at all → nothing may be skipped.
        prune_counters::reset();
        let (rows, stats) = run_with_time_column(segs(), "SELECT body FROM logs").await;
        assert_eq!(rows.len(), 4);
        assert_eq!(stats.segments_pruned, 0, "no predicate → no skip");
    }

    /// The skip must consult **only** probes on `time_column`. A range predicate on a different
    /// INT64 column describes nothing about event time, so testing it against the manifest bounds
    /// would skip segments that really do contain matching rows.
    #[tokio::test(flavor = "current_thread")]
    #[allow(clippy::await_holding_lock)]
    async fn manifest_range_skip_ignores_probes_on_other_columns() {
        let _serial = prune_counters::SERIAL
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let mut segs = vec![
            write_time_segment(dir.path(), "a.parquet", &[100, 200], None),
            write_time_segment(dir.path(), "b.parquet", &[300, 400], None),
        ];
        // Bounds that a `severity_number`-shaped predicate would "exclude" if wrongly applied.
        segs[0].time_range = Some((100, 200));
        segs[1].time_range = Some((300, 400));

        prune_counters::reset();
        let (rows, _stats) =
            run_with_time_column(segs, "SELECT body FROM logs WHERE length(body) > 0").await;
        assert_eq!(rows.len(), 4, "a non-time predicate prunes nothing");
        assert_eq!(prune_counters::pruned(), 0);
    }
}

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

use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};

use async_trait::async_trait;
use datafusion::arrow::datatypes::SchemaRef;
use datafusion::arrow::record_batch::RecordBatch;
use datafusion::catalog::{Session, TableProvider};
use datafusion::common::Column;
use datafusion::error::{DataFusionError, Result as DFResult};
use datafusion::execution::TaskContext;
use datafusion::logical_expr::{
    BinaryExpr, Cast, Expr, Operator, TableProviderFilterPushDown, TableType,
};
#[cfg(feature = "search")]
use datafusion::parquet::arrow::arrow_reader::RowSelector;
use datafusion::parquet::arrow::arrow_reader::{
    ParquetRecordBatchReader, ParquetRecordBatchReaderBuilder, RowSelection,
};
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
    /// Segments actually read (i.e. not bloom-pruned).
    pub segments_scanned: u64,
    /// Segments skipped whole by a Parquet bloom filter (`trace_id`/`span_id` point lookups).
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
}

/// A table = mutable-buffer snapshot ∪ sealed segments. `text_column` names the column whose
/// `matches(col, …)` predicate drives the Tantivy `RowSelection` bridge (`Some("body")` for
/// logs); `None` disables index pushdown (all `matches` fall back to the UDF). `bloom_columns`
/// names the binary id columns that carry a Parquet bloom filter in each segment (`trace_id`,
/// `span_id` for spans); a `col = X'…'` equality on one of them lets the scan skip whole segments
/// whose bloom proves the value absent (ARCHITECTURE.md §8). Empty for tables without blooms.
pub struct SegmentTableProvider {
    schema: SchemaRef,
    buffer: RecordBatch,
    segments: Vec<SegmentInput>,
    text_column: Option<String>,
    bloom_columns: Vec<String>,
    stats: Arc<ScanAccum>,
}

impl SegmentTableProvider {
    pub(crate) fn new(
        schema: SchemaRef,
        buffer: RecordBatch,
        segments: Vec<SegmentInput>,
        text_column: Option<String>,
        bloom_columns: Vec<String>,
        stats: Arc<ScanAccum>,
    ) -> Self {
        Self {
            schema,
            buffer,
            segments,
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
                    || bloom_id_eq(f, &self.bloom_columns).is_some()
                {
                    // Inexact: we may pre-prune (Tantivy row-selection or a bloom-filter segment
                    // skip), but DataFusion re-checks the predicate above the scan, so the pruning
                    // is a pure accelerator and results are identical.
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

        // Extract raw-id equalities (`trace_id`/`span_id` = X'…') the segment bloom filters can
        // rule out (ARCHITECTURE.md §8). Each probe is a (column, value-bytes) pair; a segment is
        // skipped only when a bloom filter *proves* the value absent — never on uncertainty.
        let bloom_probes: Vec<(String, Vec<u8>)> = if self.bloom_columns.is_empty() {
            Vec::new()
        } else {
            filters
                .iter()
                .filter_map(|f| bloom_id_eq(f, &self.bloom_columns))
                .collect()
        };

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
    bloom_probes: Vec<(String, Vec<u8>)>,
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
    bloom_probes: Vec<(String, Vec<u8>)>,
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
            match open_segment(&seg.parquet_path, selection, &self.bloom_probes) {
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
/// Before opening, if any `bloom_probes` (raw `trace_id`/`span_id` equalities) is *proven absent* by
/// this segment's Parquet bloom filter, the segment is skipped (`Ok(None)`) — the point-lookup
/// accelerator of ARCHITECTURE.md §8. A segment lacking a bloom filter for the probed column (older
/// segments, or a maybe-present answer) is always read: bloom filters only ever rule a value *out*,
/// so this can never drop a matching row.
fn open_segment(
    path: &PathBuf,
    selection: Option<RowSelection>,
    bloom_probes: &[(String, Vec<u8>)],
) -> DFResult<Option<ParquetRecordBatchReader>> {
    let file = std::fs::File::open(path)
        .map_err(|e| DataFusionError::Execution(format!("open segment {}: {e}", path.display())))?;
    let mut builder = ParquetRecordBatchReaderBuilder::try_new(file)
        .map_err(|e| DataFusionError::Execution(format!("parquet reader: {e}")))?;
    if !bloom_probes.is_empty() && bloom_rules_out(&builder, bloom_probes)? {
        return Ok(None);
    }
    if let Some(sel) = selection {
        builder = builder.with_row_selection(sel);
    }
    let reader = builder
        .build()
        .map_err(|e| DataFusionError::Execution(format!("parquet build: {e}")))?;
    Ok(Some(reader))
}

/// `true` iff some probe `(column, value)` is *proven absent* from the whole segment: every row
/// group has a bloom filter for that column and none report the value present. Any row group
/// missing a bloom filter, or reporting the value maybe-present, means the segment must be read.
fn bloom_rules_out(
    builder: &ParquetRecordBatchReaderBuilder<std::fs::File>,
    bloom_probes: &[(String, Vec<u8>)],
) -> DFResult<bool> {
    for (column, value) in bloom_probes {
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
        for rg in 0..num_row_groups {
            let bloom = builder
                .get_row_group_column_bloom_filter(rg, col_idx)
                .map_err(|e| DataFusionError::Execution(format!("read bloom filter: {e}")))?;
            match bloom {
                // Bloom present and value not in it → this row group is proven absent.
                Some(sbbf) if !sbbf.check(&value[..]) => {}
                // No bloom for this row group, or maybe-present → can't rule the segment out.
                _ => {
                    all_absent = false;
                    break;
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
            }],
            text_column: Some("body"),
            bloom_columns: &[],
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

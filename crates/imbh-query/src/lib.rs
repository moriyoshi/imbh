//! imbh query layer — DataFusion session config and SQL over buffer + segments (ARCHITECTURE.md §9).
//!
//! This is the only crate that knows the DataFusion query engine. It configures the session
//! per §9.1, ships the `matches` text-search UDF (§9.3), and registers the custom
//! [`LogsProvider`] (§9.2) that unions the mutable-buffer snapshot with the sealed segments and
//! applies the cost-gated Tantivy → Parquet `RowSelection` bridge (see `provider`).
//!
//! **Remaining simplification:** each `run_sql` builds a fresh `SessionContext` and provider
//! from a point-in-time snapshot rather than keeping one long-lived context over live storage
//! state. That is a performance refinement, not a correctness one, and is tracked for later M1.

mod provider;

pub use provider::{ScanStats, SegmentInput, SegmentTableProvider};

use std::sync::Arc;

use datafusion::arrow::array::{
    Array, ArrayRef, BinaryArray, BooleanArray, FixedSizeBinaryArray, Float64Array, Float64Builder,
    ListArray, StringArray, UInt64Array,
};
use datafusion::arrow::datatypes::{DataType, SchemaRef};
use datafusion::arrow::record_batch::RecordBatch;
use datafusion::error::DataFusionError;
use datafusion::execution::memory_pool::GreedyMemoryPool;
use datafusion::execution::runtime_env::RuntimeEnvBuilder;
use datafusion::logical_expr::{
    ColumnarValue, ScalarFunctionArgs, ScalarUDF, ScalarUDFImpl, Signature, Volatility, create_udf,
};
use datafusion::physical_plan::SendableRecordBatchStream;
use datafusion::prelude::*;
use datafusion::scalar::ScalarValue;

use imbh_core::{AnyValue, Error, Result, histogram_quantile, json_get, matches_terms};

/// Build a `SessionContext` per ARCHITECTURE.md §9.1: `target_partitions = 1`, `batch_size ≈ 4096`
/// (RSS over throughput), and a `GreedyMemoryPool` sized from the memory budget.
pub fn session_context(pool_bytes: usize) -> Result<SessionContext> {
    let runtime = RuntimeEnvBuilder::new()
        .with_memory_pool(Arc::new(GreedyMemoryPool::new(pool_bytes)))
        .build_arc()
        .map_err(|e| Error::query_ctx("build runtime env", e))?;
    let config = SessionConfig::new()
        .with_batch_size(4096)
        .with_target_partitions(1);
    let ctx = SessionContext::new_with_config_rt(config, runtime);
    ctx.register_udf(matches_udf());
    ctx.register_udf(json_get_str_udf());
    ctx.register_udf(json_get_num_udf());
    ctx.register_udf(ScalarUDF::from(HexUdf::new()));
    ctx.register_udf(ScalarUDF::from(HistogramQuantileUdf::new()));
    Ok(ctx)
}

/// `hex(binary) -> Utf8` — lowercase hex of a `FixedSizeBinary`/`Binary` column. Used to filter
/// and render `trace_id`/`span_id` (the `encoding_expressions` package that ships `encode` is
/// disabled for footprint, ARCHITECTURE.md §9.1). Accepts any single argument via `Signature::any`.
#[derive(Debug, PartialEq, Eq, Hash)]
struct HexUdf {
    signature: Signature,
}

impl HexUdf {
    fn new() -> Self {
        HexUdf {
            signature: Signature::any(1, Volatility::Immutable),
        }
    }
}

impl ScalarUDFImpl for HexUdf {
    fn name(&self) -> &str {
        "hex"
    }
    fn signature(&self) -> &Signature {
        &self.signature
    }
    fn return_type(&self, _args: &[DataType]) -> std::result::Result<DataType, DataFusionError> {
        Ok(DataType::Utf8)
    }
    fn invoke_with_args(
        &self,
        args: ScalarFunctionArgs,
    ) -> std::result::Result<ColumnarValue, DataFusionError> {
        let arr = match &args.args[0] {
            ColumnarValue::Array(a) => a.clone(),
            ColumnarValue::Scalar(s) => s.to_array()?,
        };
        let out: StringArray = if let Some(a) = arr.as_any().downcast_ref::<FixedSizeBinaryArray>()
        {
            (0..a.len())
                .map(|i| (!a.is_null(i)).then(|| hex_lower(a.value(i))))
                .collect()
        } else if let Some(a) = arr.as_any().downcast_ref::<BinaryArray>() {
            (0..a.len())
                .map(|i| (!a.is_null(i)).then(|| hex_lower(a.value(i))))
                .collect()
        } else {
            return Err(DataFusionError::Execution(
                "hex(): argument must be binary".to_owned(),
            ));
        };
        Ok(ColumnarValue::Array(Arc::new(out)))
    }
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0xf) as usize] as char);
    }
    s
}

/// The `json_get_str(json, key)` attribute-access UDF (ARCHITECTURE.md §9.3): parse a canonical-JSON
/// object column and return the string value at `key` (NULL when absent or non-string).
pub fn json_get_str_udf() -> ScalarUDF {
    create_udf(
        "json_get_str",
        vec![DataType::Utf8, DataType::Utf8],
        DataType::Utf8,
        Volatility::Immutable,
        Arc::new(json_get_str_impl),
    )
}

fn json_get_str_impl(
    args: &[ColumnarValue],
) -> std::result::Result<ColumnarValue, DataFusionError> {
    if args.len() != 2 {
        return Err(DataFusionError::Execution(
            "json_get_str(json, key) takes exactly 2 arguments".to_owned(),
        ));
    }
    let json = to_utf8_array(&args[0])?;
    let jsons = json.as_any().downcast_ref::<StringArray>().ok_or_else(|| {
        DataFusionError::Execution("json_get_str(): first argument is not text".to_owned())
    })?;
    let out: StringArray = match str_literal(&args[1])? {
        Some(key) => jsons
            .iter()
            .map(|opt| {
                opt.and_then(|s| match json_get(s, &key) {
                    Some(AnyValue::Str(v)) => Some(v),
                    _ => None,
                })
            })
            .collect(),
        None => jsons.iter().map(|_| None::<String>).collect(),
    };
    Ok(ColumnarValue::Array(Arc::new(out)))
}

/// The `json_get_num(json, key) -> Float64` attribute-access UDF: the numeric twin of
/// [`json_get_str_udf`]. Where `json_get_str` returns a value only for a JSON *string*,
/// `json_get_num` returns the numeric value of a canonical-JSON scalar — an integer, a double, or a
/// string that parses as a number — and NULL for anything else (absent key, bool, object/array,
/// non-numeric string). It exists because OTLP `IntValue`/`DoubleValue` attributes are stored as bare
/// JSON numbers (`{"http.status_code":500}`), which `json_get_str` cannot see (it returns NULL for a
/// non-string scalar). The typed numeric matchers (`attr_gt`/`ge`/`lt`/`le`) route through this so a
/// comparison against an integer- or double-typed attribute matches; the string-parse arm preserves
/// the prior behavior for numbers that arrived as strings (`"500"`).
pub fn json_get_num_udf() -> ScalarUDF {
    create_udf(
        "json_get_num",
        vec![DataType::Utf8, DataType::Utf8],
        DataType::Float64,
        Volatility::Immutable,
        Arc::new(json_get_num_impl),
    )
}

fn json_get_num_impl(
    args: &[ColumnarValue],
) -> std::result::Result<ColumnarValue, DataFusionError> {
    if args.len() != 2 {
        return Err(DataFusionError::Execution(
            "json_get_num(json, key) takes exactly 2 arguments".to_owned(),
        ));
    }
    let json = to_utf8_array(&args[0])?;
    let jsons = json.as_any().downcast_ref::<StringArray>().ok_or_else(|| {
        DataFusionError::Execution("json_get_num(): first argument is not text".to_owned())
    })?;
    let out: Float64Array = match str_literal(&args[1])? {
        Some(key) => jsons
            .iter()
            .map(|opt| {
                opt.and_then(|s| match json_get(s, &key) {
                    Some(AnyValue::Int(v)) => Some(v as f64),
                    Some(AnyValue::Double(d)) => Some(d),
                    // A number that arrived as a string still matches (parity with the prior
                    // `TRY_CAST(json_get_str(...) AS DOUBLE)` path); non-numeric strings ⇒ NULL.
                    Some(AnyValue::Str(v)) => v.parse::<f64>().ok(),
                    _ => None,
                })
            })
            .collect(),
        None => jsons.iter().map(|_| None::<f64>).collect(),
    };
    Ok(ColumnarValue::Array(Arc::new(out)))
}

/// The `matches(column, query)` text-search UDF (ARCHITECTURE.md §9.3). This is the row-wise fallback
/// (ARCHITECTURE.md §9.2): it tokenizes both sides with the shared tokenizer and returns true when all
/// query terms are present. The cost-gated Tantivy `RowSelection` pushdown that can *skip*
/// segment rows is added with the custom TableProvider in M1c; the result is identical because
/// both paths share [`imbh_core::matches_terms`].
pub fn matches_udf() -> ScalarUDF {
    create_udf(
        "matches",
        vec![DataType::Utf8, DataType::Utf8],
        DataType::Boolean,
        Volatility::Immutable,
        Arc::new(matches_impl),
    )
}

fn matches_impl(args: &[ColumnarValue]) -> std::result::Result<ColumnarValue, DataFusionError> {
    if args.len() != 2 {
        return Err(DataFusionError::Execution(
            "matches(column, query) takes exactly 2 arguments".to_owned(),
        ));
    }
    let text = to_utf8_array(&args[0])?;
    let strs = text.as_any().downcast_ref::<StringArray>().ok_or_else(|| {
        DataFusionError::Execution("matches(): first argument is not text".to_owned())
    })?;

    let out: BooleanArray = match str_literal(&args[1])? {
        Some(query) => strs
            .iter()
            .map(|opt| opt.map(|s| matches_terms(s, &query)))
            .collect(),
        // NULL query matches nothing (keeps NULL text as NULL).
        None => strs.iter().map(|opt| opt.map(|_| false)).collect(),
    };
    Ok(ColumnarValue::Array(Arc::new(out)))
}

/// `histogram_quantile(phi, explicit_bounds, bucket_counts) -> Float64` (ARCHITECTURE.md §10.8). Estimates
/// the `phi`-quantile (0..1) of one explicit-bucket histogram data point with Prometheus-style
/// linear interpolation inside the matched bucket. `explicit_bounds` is the `List<Float64>` of N
/// ascending upper bounds; `bucket_counts` is the `List<UInt64>` of N+1 per-bucket counts (OTLP
/// layout, last = `+Inf`). Operates **per row** (one data point's buckets); merging buckets across
/// series/time is the caller's job — a bucket-summing aggregate is a follow-up.
#[derive(Debug, PartialEq, Eq, Hash)]
struct HistogramQuantileUdf {
    signature: Signature,
}

impl HistogramQuantileUdf {
    fn new() -> Self {
        HistogramQuantileUdf {
            signature: Signature::any(3, Volatility::Immutable),
        }
    }
}

impl ScalarUDFImpl for HistogramQuantileUdf {
    fn name(&self) -> &str {
        "histogram_quantile"
    }
    fn signature(&self) -> &Signature {
        &self.signature
    }
    fn return_type(&self, _args: &[DataType]) -> std::result::Result<DataType, DataFusionError> {
        Ok(DataType::Float64)
    }
    fn invoke_with_args(
        &self,
        args: ScalarFunctionArgs,
    ) -> std::result::Result<ColumnarValue, DataFusionError> {
        let num_rows = args.number_rows;
        let phi = columnar_to_array(&args.args[0], num_rows)?;
        let phi = datafusion::arrow::compute::cast(&phi, &DataType::Float64)?;
        let phi = phi.as_any().downcast_ref::<Float64Array>().ok_or_else(|| {
            DataFusionError::Execution("histogram_quantile(): phi is not numeric".to_owned())
        })?;
        let bounds = columnar_to_array(&args.args[1], num_rows)?;
        let bounds = bounds.as_any().downcast_ref::<ListArray>().ok_or_else(|| {
            DataFusionError::Execution(
                "histogram_quantile(): explicit_bounds must be a List column".to_owned(),
            )
        })?;
        let counts = columnar_to_array(&args.args[2], num_rows)?;
        let counts = counts.as_any().downcast_ref::<ListArray>().ok_or_else(|| {
            DataFusionError::Execution(
                "histogram_quantile(): bucket_counts must be a List column".to_owned(),
            )
        })?;

        let mut out = Float64Builder::with_capacity(num_rows);
        for i in 0..num_rows {
            if phi.is_null(i) || bounds.is_null(i) || counts.is_null(i) {
                out.append_null();
                continue;
            }
            let b_list = bounds.value(i);
            let c_list = counts.value(i);
            let bvals = b_list
                .as_any()
                .downcast_ref::<Float64Array>()
                .ok_or_else(|| {
                    DataFusionError::Execution(
                        "histogram_quantile(): explicit_bounds child is not Float64".to_owned(),
                    )
                })?;
            let cvals = c_list
                .as_any()
                .downcast_ref::<UInt64Array>()
                .ok_or_else(|| {
                    DataFusionError::Execution(
                        "histogram_quantile(): bucket_counts child is not UInt64".to_owned(),
                    )
                })?;
            let bounds_vec: Vec<f64> = (0..bvals.len()).map(|k| bvals.value(k)).collect();
            let counts_vec: Vec<u64> = (0..cvals.len()).map(|k| cvals.value(k)).collect();
            out.append_value(histogram_quantile(phi.value(i), &bounds_vec, &counts_vec));
        }
        Ok(ColumnarValue::Array(Arc::new(out.finish())))
    }
}

/// Broadcast a scalar argument to `num_rows`, or pass an array argument through.
fn columnar_to_array(
    v: &ColumnarValue,
    num_rows: usize,
) -> std::result::Result<ArrayRef, DataFusionError> {
    match v {
        ColumnarValue::Array(a) => Ok(a.clone()),
        ColumnarValue::Scalar(s) => Ok(s.to_array_of_size(num_rows)?),
    }
}

/// Materialize a value argument to a `Utf8` array, casting `Utf8View`/`LargeUtf8`/dict down.
fn to_utf8_array(v: &ColumnarValue) -> std::result::Result<ArrayRef, DataFusionError> {
    let arr = match v {
        ColumnarValue::Array(a) => a.clone(),
        ColumnarValue::Scalar(s) => s.to_array()?,
    };
    if arr.data_type() == &DataType::Utf8 {
        Ok(arr)
    } else {
        Ok(datafusion::arrow::compute::cast(&arr, &DataType::Utf8)?)
    }
}

/// Extract the string literal query argument; `Ok(None)` for a NULL literal.
fn str_literal(v: &ColumnarValue) -> std::result::Result<Option<String>, DataFusionError> {
    match v {
        ColumnarValue::Scalar(ScalarValue::Utf8(q))
        | ColumnarValue::Scalar(ScalarValue::LargeUtf8(q))
        | ColumnarValue::Scalar(ScalarValue::Utf8View(q)) => Ok(q.clone()),
        _ => Err(DataFusionError::Execution(
            "matches(): query argument must be a string literal".to_owned(),
        )),
    }
}

/// One table to register for a query: its name, schema, buffer snapshot, sealed segments, the
/// text column (if any) whose `matches(col, …)` predicate drives Tantivy pruning, and the binary
/// id columns (if any) that carry a Parquet bloom filter for segment-skipping point lookups.
///
/// `Clone` is cheap: every field is either a `Copy`/`'static` scalar, an `Arc` (`SchemaRef`,
/// `RecordBatch`), or a small `Vec<SegmentInput>` of paths — so a read-only handle can cache a built
/// set of table inputs and hand out clones across queries (the facade's reader snapshot cache).
#[derive(Clone)]
pub struct TableInput {
    pub name: &'static str,
    pub schema: SchemaRef,
    pub buffer: RecordBatch,
    pub segments: Vec<SegmentInput>,
    pub text_column: Option<&'static str>,
    pub bloom_columns: &'static [&'static str],
}

/// Run SQL over the given tables (each = buffer snapshot ∪ sealed segments), with cost-gated
/// Tantivy pruning applied through [`SegmentTableProvider`].
#[cfg_attr(
    feature = "tracing",
    tracing::instrument(
        level = "debug",
        name = "query.run_sql",
        skip_all,
        fields(
            sql = sql,
            pool_bytes = pool_bytes,
            segments_scanned = tracing::field::Empty,
            segments_pruned = tracing::field::Empty,
            rows_scanned = tracing::field::Empty,
            bytes_scanned = tracing::field::Empty,
            index_searched = tracing::field::Empty,
        )
    )
)]
pub async fn run_sql(
    tables: Vec<TableInput>,
    pool_bytes: usize,
    sql: &str,
    params: Vec<ScalarValue>,
) -> Result<(SchemaRef, Vec<RecordBatch>, ScanStats)> {
    let (_ctx, df, stats) = plan_query(tables, pool_bytes, sql, params).await?;
    // The logical output schema — known before execution, so it is the authoritative result schema
    // even when zero rows come back (an empty `Vec<RecordBatch>` carries no schema of its own). The
    // collected batches carry an identical schema on a non-empty result. This is what lets a
    // downstream Arrow-IPC / C-Data-Interface exporter always describe the columns (§10.11).
    let schema = df.schema().inner().clone();
    let batches = df.collect().await.map_err(|e| {
        #[cfg(feature = "tracing")]
        tracing::error!(error = %e, "SQL execution failed");
        Error::query_execute(e)
    })?;
    let scan = stats.snapshot();
    #[cfg(feature = "tracing")]
    {
        // Surface the read-side pruning counters as fields on the `run_sql` span itself (so a trace
        // viewer shows them on the span, not only in the completion event below).
        let span = tracing::Span::current();
        span.record("segments_scanned", scan.segments_scanned);
        span.record("segments_pruned", scan.segments_pruned);
        span.record("rows_scanned", scan.rows_scanned);
        span.record("bytes_scanned", scan.bytes_scanned);
        span.record("index_searched", scan.index_searched);
        let rows: usize = batches.iter().map(|b| b.num_rows()).sum();
        tracing::debug!(batches = batches.len(), rows, stats = ?scan, "query complete");
    }
    Ok((schema, batches, scan))
}

/// The streaming twin of [`run_sql`]: plan the same query over the same tables, but return a
/// bounded-memory [`SendableRecordBatchStream`] that yields batches lazily instead of collecting the
/// whole result into memory (ARCHITECTURE.md §10.11). This is the tap point a foreign-runtime binding
/// wraps as an `FFI_ArrowArrayStream` for zero-copy streaming.
///
/// **Lifetime.** The returned stream is `'static` and self-contained: `DataFrame::execute_stream`
/// builds the physical plan and an owned `TaskContext` from the `DataFrame`, which itself holds an
/// `Arc` to the session state — so the local `SessionContext` and the registered
/// [`SegmentTableProvider`]s can drop when this function returns without affecting the stream. The
/// plan owns everything it touches: the mutable-buffer batches (`Arc`-cloned under the storage lock)
/// and the sealed-segment Parquet paths. The snapshot is therefore fixed at plan time and the segment
/// paths are pinned for the stream's life — a streamed query holds its point-in-time view open (the
/// desired bounded-memory-but-consistent behavior). Unlike [`run_sql`], there is no read-during-delete
/// retry loop: the caller must not unlink the streamed segments until the stream is drained/dropped.
///
/// Scan statistics are returned as a [`StreamStatsHandle`] that shares the live scan accumulator with
/// the stream (prescription I-5). Because the scan is lazy, the counters accrue as the stream is
/// drained and are **complete only after it is fully exhausted** — `handle.get()` mid-drain is a
/// partial snapshot. The binding may ignore the handle if it does not need stats.
#[cfg_attr(
    feature = "tracing",
    tracing::instrument(
        level = "debug",
        name = "query.run_sql_stream",
        skip_all,
        fields(sql = sql, pool_bytes = pool_bytes)
    )
)]
pub async fn run_sql_stream(
    tables: Vec<TableInput>,
    pool_bytes: usize,
    sql: &str,
    params: Vec<ScalarValue>,
) -> Result<(SendableRecordBatchStream, StreamStatsHandle)> {
    let (_ctx, df, stats) = plan_query(tables, pool_bytes, sql, params).await?;
    let stream = df.execute_stream().await.map_err(|e| {
        #[cfg(feature = "tracing")]
        tracing::error!(error = %e, "SQL stream execution failed");
        Error::query_execute(e)
    })?;
    Ok((stream, StreamStatsHandle(stats)))
}

/// A handle to the read-side scan statistics of a [`run_sql_stream`] query, sharing the live
/// accumulator with the returned stream (prescription I-5). Snapshot the current counters with
/// [`get`](StreamStatsHandle::get); they are **complete only after the stream is fully drained**
/// (the scan is lazy, so a mid-drain snapshot undercounts). Cheap to clone/hold — it is just a
/// reference-counted pointer to the same accumulator the stream writes.
#[derive(Debug, Clone)]
pub struct StreamStatsHandle(Arc<provider::ScanAccum>);

impl StreamStatsHandle {
    /// Snapshot the read-side scan counters accumulated so far (final once the stream is exhausted).
    pub fn get(&self) -> ScanStats {
        self.0.snapshot()
    }
}

/// Shared setup for [`run_sql`] and [`run_sql_stream`]: build the `SessionContext`, register one
/// [`SegmentTableProvider`] per table against a shared scan accumulator, plan the SQL, and bind the
/// `$1..$N` parameters. Returns the context (kept alive by the caller for the collect path; droppable
/// once the stream is built), the planned [`DataFrame`], and the scan accumulator (snapshotted after
/// a collect).
async fn plan_query(
    tables: Vec<TableInput>,
    pool_bytes: usize,
    sql: &str,
    params: Vec<ScalarValue>,
) -> Result<(SessionContext, DataFrame, Arc<provider::ScanAccum>)> {
    let ctx = session_context(pool_bytes)?;
    // One accumulator shared by every table's provider, snapshotted after execution (read-side
    // pruning stats → the typed API's `QueryStats`).
    let stats = Arc::new(provider::ScanAccum::default());
    for t in tables {
        let provider = SegmentTableProvider::new(
            t.schema,
            t.buffer,
            t.segments,
            t.text_column.map(str::to_owned),
            t.bloom_columns.iter().map(|c| (*c).to_owned()).collect(),
            stats.clone(),
        );
        ctx.register_table(t.name, Arc::new(provider))
            .map_err(|e| Error::query_ctx(format!("register table `{}`", t.name), e))?;
    }

    let mut df = ctx.sql(sql).await.map_err(|e| {
        #[cfg(feature = "tracing")]
        tracing::warn!(error = %e, "SQL planning failed");
        Error::query_plan(e)
    })?;
    // Bind `$1..$N` placeholders emitted by the typed query builders (user values are parameters,
    // never interpolated into the SQL text). Raw `Db::sql` passes no params, so this is skipped.
    if !params.is_empty() {
        df = df.with_param_values(params).map_err(Error::query_plan)?;
    }
    Ok((ctx, df, stats))
}

/// Normalize a batch to the canonical table schema so buffer and segment batches unite. The
/// `parquet`-crate reader yields `Utf8` matching the canonical schema, so this is usually an
/// identity (modulo the schema *metadata* parquet attaches); it remains a safety net (and casts
/// `Utf8View` down if a source ever supplies it). Columns are matched **by name** — column order in
/// the source is irrelevant, and a source column of an uncastable type returns an `Error::Query`
/// (never a positional-index panic).
///
/// **Schema evolution (promoted columns).** A canonical column *missing* from the source is
/// tolerated only when the field is **nullable**: it is filled with an all-null array of the field
/// type, so a segment written before a label key was promoted (ARCHITECTURE.md §6.1) still unites
/// with the wider current schema instead of failing every query that touches it. A missing
/// **non-nullable** column is still a hard error — that can only be a genuine schema mismatch, not
/// forward evolution, and fabricating nulls would violate the column's contract.
pub(crate) fn coerce(batch: RecordBatch, schema: &SchemaRef) -> Result<RecordBatch> {
    // Fast path: an exactly-identical schema (the metadata-free buffer snapshot) passes through.
    // Parquet-read segment batches carry schema metadata, so they fall through and are rebuilt to
    // the canonical schema below — which is what the provider registered and the union expects.
    if batch.schema().as_ref() == schema.as_ref() {
        return Ok(batch);
    }
    let src = batch.schema();
    let mut columns = Vec::with_capacity(schema.fields().len());
    for field in schema.fields() {
        let col = match src.index_of(field.name()) {
            Ok(idx) => {
                let col = batch.column(idx);
                if col.data_type() == field.data_type() {
                    col.clone()
                } else {
                    datafusion::arrow::compute::cast(col, field.data_type())
                        .map_err(|e| Error::coerce(Some(field.name().to_owned()), e))?
                }
            }
            // Missing canonical column: null-fill if the field permits it (a promoted column added
            // after this segment was sealed), else error — see the schema-evolution note above.
            Err(_) if field.is_nullable() => {
                datafusion::arrow::array::new_null_array(field.data_type(), batch.num_rows())
            }
            Err(_) => return Err(Error::coerce_missing(field.name())),
        };
        columns.push(col);
    }
    RecordBatch::try_new(schema.clone(), columns).map_err(|e| Error::coerce(None, e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use datafusion::arrow::array::{Array, Int64Array, StringArray};
    use datafusion::arrow::datatypes::{DataType, Field, Schema};

    #[test]
    fn coerce_matches_by_name_and_errors_on_missing_column() {
        let schema: SchemaRef = Arc::new(Schema::new(vec![
            Field::new("a", DataType::Int64, false),
            Field::new("b", DataType::Utf8, false),
        ]));
        // Source columns in REVERSE order (b, a): coerce must match by name, not position.
        let src = Arc::new(Schema::new(vec![
            Field::new("b", DataType::Utf8, false),
            Field::new("a", DataType::Int64, false),
        ]));
        let batch = RecordBatch::try_new(
            src,
            vec![
                Arc::new(StringArray::from(vec!["x"])) as ArrayRef,
                Arc::new(Int64Array::from(vec![1i64])),
            ],
        )
        .unwrap();
        let out = coerce(batch, &schema).unwrap();
        assert_eq!(out.schema().field(0).name(), "a");
        assert_eq!(
            out.column(0)
                .as_any()
                .downcast_ref::<Int64Array>()
                .unwrap()
                .value(0),
            1
        );
        assert_eq!(
            out.column(1)
                .as_any()
                .downcast_ref::<StringArray>()
                .unwrap()
                .value(0),
            "x"
        );

        // A source missing a *non-nullable* schema column `b` returns an error — not a
        // positional-index panic, and not a fabricated null (which would break the non-null contract).
        let missing_schema = Arc::new(Schema::new(vec![Field::new("a", DataType::Int64, false)]));
        let missing = RecordBatch::try_new(
            missing_schema,
            vec![Arc::new(Int64Array::from(vec![1i64])) as ArrayRef],
        )
        .unwrap();
        assert!(coerce(missing, &schema).is_err());
    }

    #[test]
    fn coerce_null_fills_a_missing_nullable_column() {
        // Schema evolution: the canonical schema gained a nullable promoted column `label` that an
        // older segment does not carry. coerce must null-fill it instead of erroring, so the old
        // segment still unites with the current schema (ARCHITECTURE.md §6.1 promoted columns).
        let schema: SchemaRef = Arc::new(Schema::new(vec![
            Field::new("a", DataType::Int64, false),
            Field::new("label", DataType::Utf8, true),
        ]));
        let old_schema = Arc::new(Schema::new(vec![Field::new("a", DataType::Int64, false)]));
        let old = RecordBatch::try_new(
            old_schema,
            vec![Arc::new(Int64Array::from(vec![1i64, 2i64])) as ArrayRef],
        )
        .unwrap();

        let out = coerce(old, &schema).unwrap();
        assert_eq!(out.num_columns(), 2);
        assert_eq!(out.num_rows(), 2);
        let label = out
            .column(1)
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        assert_eq!(
            label.null_count(),
            2,
            "the backfilled column is entirely null"
        );
    }

    fn one_table(schema: SchemaRef, buffer: RecordBatch) -> Vec<TableInput> {
        vec![TableInput {
            name: "logs",
            schema,
            buffer,
            segments: Vec::new(),
            text_column: None,
            bloom_columns: &[],
        }]
    }

    /// Plan-shape (EXPLAIN) assertions for the `logs` provider (TESTING.md Layer 1). These check the
    /// *structure* of the compiled physical plan — the operators and pushdown markers — rather than the
    /// query results, complementing the behavioural pruning tests in `provider`. Assertions target
    /// stable substrings of the `displayable(...).indent()` output (not whole-plan equality), which is
    /// resilient to the cosmetic plan-format churn between DataFusion releases.
    mod plan_shape {
        use super::*;

        /// Build a `logs`-shaped table (body/service/severity_number over an in-memory buffer, no
        /// segment files needed — the provider always emits its streaming scan regardless) and return
        /// the compiled physical plan formatted with `displayable(...).indent(true)`.
        async fn physical_plan(sql: &str) -> String {
            let schema: SchemaRef = Arc::new(Schema::new(vec![
                Field::new("body", DataType::Utf8, false),
                Field::new("service", DataType::Utf8, true),
                Field::new("severity_number", DataType::Int64, false),
            ]));
            let buffer = RecordBatch::try_new(
                schema.clone(),
                vec![
                    Arc::new(StringArray::from(vec!["error one", "ok two"])) as ArrayRef,
                    Arc::new(StringArray::from(vec![Some("api"), Some("web")])) as ArrayRef,
                    Arc::new(Int64Array::from(vec![9, 17])) as ArrayRef,
                ],
            )
            .unwrap();
            let tables = vec![TableInput {
                name: "logs",
                schema,
                buffer,
                segments: Vec::new(),
                text_column: Some("body"),
                bloom_columns: &[],
            }];
            let (_ctx, df, _stats) = plan_query(tables, 64 << 20, sql, Vec::new()).await.unwrap();
            let plan = df.create_physical_plan().await.unwrap();
            datafusion::physical_plan::displayable(plan.as_ref())
                .indent(true)
                .to_string()
        }

        /// The custom lazy scan node — `StreamingTableExec` (the `PartitionStream` wrapper of
        /// ARCHITECTURE.md §10.12, one Parquet batch per poll) — is what a buffer scan compiles to, not
        /// an eager `MemoryExec`/`MemTable` that would materialize the whole table up front.
        #[tokio::test(flavor = "current_thread")]
        async fn buffer_scan_is_the_streaming_exec_not_a_memory_exec() {
            let plan = physical_plan("SELECT * FROM logs").await;
            assert!(
                plan.contains("StreamingTableExec"),
                "the provider's lazy streaming scan node should appear:\n{plan}"
            );
            assert!(
                !plan.contains("MemoryExec") && !plan.contains("MemTable"),
                "the buffer must not be scanned via an eager MemoryExec/MemTable:\n{plan}"
            );
        }

        /// `LIMIT n` reaches the scan: the physical `StreamingTableExec` carries `fetch=n`, so the
        /// limit is pushed into the lazy source (a `LimitStream` stops polling early — a `LIMIT` never
        /// reads past segments) rather than being applied only by an outer limit operator.
        #[tokio::test(flavor = "current_thread")]
        async fn limit_is_pushed_into_the_streaming_scan() {
            let plan = physical_plan("SELECT body FROM logs LIMIT 5").await;
            assert!(
                plan.contains("StreamingTableExec"),
                "expected the streaming scan node:\n{plan}"
            );
            assert!(
                plan.contains("fetch=5"),
                "the LIMIT should be pushed into the scan as fetch=5:\n{plan}"
            );
        }

        /// A narrow projection (`SELECT body`) is pushed into the scan: the `StreamingTableExec` shows
        /// `projection=[body]` — only the requested column — not a full-schema read that a
        /// `ProjectionExec` above the scan would then discard.
        #[tokio::test(flavor = "current_thread")]
        async fn projection_is_narrowed_at_the_scan() {
            let plan = physical_plan("SELECT body FROM logs").await;
            assert!(
                plan.contains("projection=[body]"),
                "the scan should project only `body`, not the full schema:\n{plan}"
            );
            assert!(
                !plan.contains("service") && !plan.contains("severity_number"),
                "unprojected columns must not appear anywhere in the plan:\n{plan}"
            );
        }

        /// A `matches(body, …)` predicate is claimed `Inexact` by the provider, so DataFusion keeps a
        /// `FilterExec` with the `matches` UDF *above* the streaming scan. This proves the Tantivy
        /// `RowSelection` bridge is a pure pushdown accelerator (Parquet stays ground truth), not the
        /// sole filter — the plan re-checks the predicate after the (possibly pruned) scan.
        #[tokio::test(flavor = "current_thread")]
        async fn matches_predicate_is_reapplied_as_a_filter_above_the_scan() {
            let plan = physical_plan("SELECT * FROM logs WHERE matches(body, 'error')").await;
            assert!(
                plan.contains("FilterExec"),
                "an Inexact matches() pushdown must leave a FilterExec above the scan:\n{plan}"
            );
            assert!(
                plan.contains("matches(body"),
                "the retained filter should re-apply the matches() UDF:\n{plan}"
            );
            // The FilterExec sits above the streaming scan (the pushdown is an accelerator, not a
            // replacement): both nodes are present, with the filter ahead of the scan in the indent.
            let filter_at = plan.find("FilterExec");
            let scan_at = plan.find("StreamingTableExec");
            assert!(
                matches!((filter_at, scan_at), (Some(f), Some(s)) if f < s),
                "FilterExec should sit above StreamingTableExec:\n{plan}"
            );
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn empty_buffer_counts_zero() {
        let schema: SchemaRef =
            Arc::new(Schema::new(vec![Field::new("v", DataType::Int64, false)]));
        let empty = RecordBatch::new_empty(schema.clone());
        let (_schema, out, _stats) = run_sql(
            one_table(schema, empty),
            64 << 20,
            "SELECT count(*) AS c FROM logs",
            Vec::new(),
        )
        .await
        .unwrap();
        let c = out[0]
            .column(0)
            .as_any()
            .downcast_ref::<Int64Array>()
            .unwrap();
        assert_eq!(c.value(0), 0);
    }

    /// `json_get_num` reads a numeric attribute regardless of how the canonical JSON encoded it — a
    /// bare integer, a double, or a number that arrived as a string — and yields NULL for a
    /// non-numeric string or an absent key. This is the fix that lets `attr_gt`/`ge`/`lt`/`le` match
    /// integer/double-typed OTLP attributes (stored as JSON numbers), which `json_get_str` could not
    /// see. NULL sorts out of range comparisons, so the excluded rows never match.
    #[tokio::test(flavor = "current_thread")]
    async fn json_get_num_reads_numbers_regardless_of_json_encoding() {
        let schema: SchemaRef = Arc::new(Schema::new(vec![Field::new(
            "attributes",
            DataType::Utf8,
            false,
        )]));
        let batch = RecordBatch::try_new(
            schema.clone(),
            vec![Arc::new(StringArray::from(vec![
                r#"{"code":500}"#,    // integer-typed
                r#"{"code":1.5}"#,    // double-typed
                r#"{"code":"503"}"#,  // number that arrived as a string
                r#"{"code":"nope"}"#, // non-numeric string ⇒ NULL
                r#"{"other":1}"#,     // absent key ⇒ NULL
            ])) as ArrayRef],
        )
        .unwrap();
        let (_schema, out, _stats) = run_sql(
            one_table(schema, batch),
            64 << 20,
            "SELECT json_get_num(attributes, 'code') AS n FROM logs",
            Vec::new(),
        )
        .await
        .unwrap();
        let n = out[0]
            .column(0)
            .as_any()
            .downcast_ref::<Float64Array>()
            .unwrap();
        assert_eq!(n.value(0), 500.0);
        assert_eq!(n.value(1), 1.5);
        assert_eq!(n.value(2), 503.0);
        assert!(n.is_null(3));
        assert!(n.is_null(4));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn buffer_rows_are_visible() {
        let schema: SchemaRef =
            Arc::new(Schema::new(vec![Field::new("v", DataType::Int64, false)]));
        let batch = RecordBatch::try_new(
            schema.clone(),
            vec![Arc::new(Int64Array::from(vec![1, 2, 3]))],
        )
        .unwrap();
        let (_schema, out, _stats) = run_sql(
            one_table(schema, batch),
            64 << 20,
            "SELECT sum(v) AS s FROM logs",
            Vec::new(),
        )
        .await
        .unwrap();
        let s = out[0]
            .column(0)
            .as_any()
            .downcast_ref::<Int64Array>()
            .unwrap();
        assert_eq!(s.value(0), 6);
    }
}

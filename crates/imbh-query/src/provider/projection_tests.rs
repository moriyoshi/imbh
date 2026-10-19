//! Parquet column-projection pushdown (the `ProjectionMask` the provider pushes into each segment
//! reader). Two guarantees are pinned here:
//!
//! 1. **Equivalence.** A corpus of logs / spans / metrics tables — each a buffer snapshot plus
//!    segments written under older, reordered, extra-column, and differently-typed schemas, with
//!    Tantivy sidecars, bloom filters, and multi-row-group files — is queried with the pushdown ON
//!    and OFF, and every query must return the very same batches (and the same scan counters, bar
//!    `bytes_scanned`, which may only shrink). The query list deliberately covers predicates whose
//!    columns are *not* in the `SELECT` list (the `Inexact` `FilterExec` above the scan needs them),
//!    Tantivy `RowSelection`s, row-group subsets intersected with them, bloom pruning, promoted
//!    columns null-filled for old segments, nested `List` columns, `count(*)`'s empty projection,
//!    `LIMIT`, and a join.
//! 2. **Effect.** A two-column traces query decodes only the projected (and filter) columns — never
//!    the wide `attributes`/`resource`/`events`/`links` JSON — asserted on the readers' own projected
//!    schemas and on `bytes_scanned`.

use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use datafusion::arrow::array::{
    ArrayRef, FixedSizeBinaryArray, Float64Array, Float64Builder, Int32Array, Int64Array,
    LargeStringArray, ListBuilder, StringArray, TimestampNanosecondArray, UInt64Builder,
};
use datafusion::arrow::compute::cast;
use datafusion::arrow::datatypes::{DataType, Field, Schema, SchemaRef, TimeUnit};
use datafusion::arrow::record_batch::RecordBatch;
use datafusion::parquet::arrow::ArrowWriter;
use datafusion::parquet::file::properties::WriterProperties;
use datafusion::parquet::schema::types::ColumnPath;

use super::{ScanStats, SegmentInput, projected_reads, prune_counters};
use crate::TableInput;

fn ts() -> DataType {
    DataType::Timestamp(TimeUnit::Nanosecond, Some("UTC".into()))
}

/// A fat JSON padding value, so the wide columns dominate a row's bytes as they do in production.
fn pad(tag: &str, i: usize) -> String {
    format!("{tag}-{i}-{}", "x".repeat(96))
}

fn utf8(values: impl IntoIterator<Item = String>) -> ArrayRef {
    Arc::new(StringArray::from_iter_values(values))
}

fn utf8_opt(values: impl IntoIterator<Item = Option<String>>) -> ArrayRef {
    Arc::new(StringArray::from_iter(values))
}

fn fsb(size: i32, values: impl IntoIterator<Item = Option<Vec<u8>>>) -> ArrayRef {
    Arc::new(
        FixedSizeBinaryArray::try_from_sparse_iter_with_size(values.into_iter(), size).unwrap(),
    )
}

fn time_col(range: Range<usize>) -> ArrayRef {
    Arc::new(
        TimestampNanosecondArray::from_iter_values(range.map(|i| i as i64 * 1000))
            .with_timezone("UTC"),
    )
}

/// Write `batch` as a Parquet segment with `rows_per_group`-row row groups and bloom filters on
/// `blooms`.
fn write_parquet(path: &Path, batch: &RecordBatch, rows_per_group: usize, blooms: &[&str]) {
    let mut props = WriterProperties::builder().set_max_row_group_row_count(Some(rows_per_group));
    for c in blooms {
        props = props.set_column_bloom_filter_enabled(ColumnPath::from(*c), true);
    }
    let f = std::fs::File::create(path).unwrap();
    let mut w = ArrowWriter::try_new(f, batch.schema(), Some(props.build())).unwrap();
    w.write(batch).unwrap();
    w.close().unwrap();
}

/// The field names of `schema` except `drop` — an older schema that predates column `drop`.
fn names_except(schema: &SchemaRef, drop: &str) -> Vec<String> {
    schema
        .fields()
        .iter()
        .map(|f| f.name().clone())
        .filter(|n| n != drop)
        .collect()
}

/// `batch` restricted to (and reordered as) `names` — an older or differently-ordered schema.
fn select(batch: &RecordBatch, names: &[String]) -> RecordBatch {
    let idx: Vec<usize> = names
        .iter()
        .map(|n| batch.schema().index_of(n).unwrap())
        .collect();
    batch.project(&idx).unwrap()
}

/// `batch` with column `name` replaced by its cast to `to` (a segment storing a column under a
/// different physical type than the current schema).
fn recast(batch: &RecordBatch, name: &str, to: DataType) -> RecordBatch {
    let schema = batch.schema();
    let idx = schema.index_of(name).unwrap();
    let mut fields: Vec<Field> = schema.fields().iter().map(|f| f.as_ref().clone()).collect();
    fields[idx] = Field::new(name, to.clone(), fields[idx].is_nullable());
    let mut cols = batch.columns().to_vec();
    cols[idx] = cast(&cols[idx], &to).unwrap();
    RecordBatch::try_new(Arc::new(Schema::new(fields)), cols).unwrap()
}

/// `batch` with an extra trailing column the current schema no longer has (a dropped column).
fn with_extra(batch: &RecordBatch, name: &str, col: ArrayRef) -> RecordBatch {
    let schema = batch.schema();
    let mut fields: Vec<Field> = schema.fields().iter().map(|f| f.as_ref().clone()).collect();
    fields.push(Field::new(name, col.data_type().clone(), true));
    let mut cols = batch.columns().to_vec();
    cols.push(col);
    RecordBatch::try_new(Arc::new(Schema::new(fields)), cols).unwrap()
}

fn segment(
    path: PathBuf,
    index_path: Option<PathBuf>,
    rows: Range<usize>,
    with_range: bool,
) -> SegmentInput {
    SegmentInput {
        parquet_path: path,
        index_path,
        rows: rows.len() as u64,
        time_range: with_range.then(|| (rows.start as i64 * 1000, (rows.end as i64 - 1) * 1000)),
    }
}

// ---- logs ----

const LOG_WORDS: [&str; 5] = ["error", "ok", "ok", "timeout", "ok"];

fn log_body(i: usize) -> String {
    format!("{} request {i}", LOG_WORDS[i % 5])
}
fn log_attrs(i: usize) -> String {
    let env = if i.is_multiple_of(7) {
        "staging"
    } else {
        "prod"
    };
    let code = if i.is_multiple_of(5) { 500 } else { 200 };
    format!(r#"{{"env":"{env}","http.status_code":{code}}}"#)
}
fn log_service(i: usize) -> Option<String> {
    match i % 11 {
        0 => None,
        n if n.is_multiple_of(2) => Some("api".to_owned()),
        _ => Some("web".to_owned()),
    }
}

fn logs_schema() -> SchemaRef {
    Arc::new(Schema::new(vec![
        Field::new("time", ts(), false),
        Field::new("service", DataType::Utf8, true),
        Field::new("severity_number", DataType::Int32, false),
        Field::new("body", DataType::Utf8, false),
        Field::new("attributes", DataType::Utf8, false),
        Field::new("resource", DataType::Utf8, false),
        Field::new("trace_id", DataType::FixedSizeBinary(16), true),
        // A promoted column: older segments predate it and must read back as NULL.
        Field::new("env", DataType::Utf8, true),
    ]))
}

fn logs_batch(rows: Range<usize>) -> RecordBatch {
    let r = || rows.clone();
    RecordBatch::try_new(
        logs_schema(),
        vec![
            time_col(r()),
            utf8_opt(r().map(log_service)),
            Arc::new(Int32Array::from_iter_values(r().map(|i| (i % 24) as i32))),
            utf8(r().map(log_body)),
            utf8(r().map(log_attrs)),
            utf8(r().map(|i| pad("res", i))),
            fsb(
                16,
                r().map(|i| i.is_multiple_of(2).then(|| vec![i as u8; 16])),
            ),
            utf8_opt(r().map(|i| match i % 3 {
                0 => None,
                1 => Some("prod".to_owned()),
                _ => Some("staging".to_owned()),
            })),
        ],
    )
    .unwrap()
}

#[cfg(feature = "search")]
fn logs_index(dir: &Path, name: &str, rows: Range<usize>) -> Option<PathBuf> {
    let path = dir.join(name);
    let rows: Vec<imbh_core::LogRow> = rows
        .map(|i| imbh_core::LogRow {
            time_unix_nano: i as i64 * 1000,
            observed_time_unix_nano: None,
            service: log_service(i),
            severity_number: (i % 24) as u8,
            severity_text: None,
            body: log_body(i),
            attributes: log_attrs(i),
            resource: pad("res", i),
            scope: String::new(),
            trace_id: None,
            span_id: None,
            flags: 0,
        })
        .collect();
    imbh_index::build_logs_index(&path, &rows).unwrap();
    Some(path)
}
#[cfg(not(feature = "search"))]
fn logs_index(_dir: &Path, _name: &str, _rows: Range<usize>) -> Option<PathBuf> {
    None
}

// ---- spans ----

const SPAN_NAMES: [&str; 6] = [
    "GET /api",
    "POST /api",
    "db query",
    "cache get",
    "GET /health",
    "render page",
];

fn span_attrs(i: usize) -> String {
    let method = if i.is_multiple_of(3) { "POST" } else { "GET" };
    format!(r#"{{"http.method":"{method}","n":{i}}}"#)
}
fn span_service(i: usize) -> Option<String> {
    (!i.is_multiple_of(5)).then(|| ["checkout", "cart"][i % 2].to_owned())
}

fn spans_schema() -> SchemaRef {
    Arc::new(Schema::new(vec![
        Field::new("trace_id", DataType::FixedSizeBinary(16), false),
        Field::new("span_id", DataType::FixedSizeBinary(8), false),
        Field::new("parent_span_id", DataType::FixedSizeBinary(8), true),
        Field::new("name", DataType::Utf8, false),
        Field::new("service", DataType::Utf8, true),
        Field::new("start_time", ts(), false),
        Field::new("duration_ns", DataType::Int64, false),
        Field::new("status_code", DataType::Utf8, false),
        Field::new("attributes", DataType::Utf8, false),
        Field::new("resource", DataType::Utf8, false),
        Field::new("events", DataType::Utf8, true),
        Field::new("links", DataType::Utf8, true),
    ]))
}

fn spans_batch(rows: Range<usize>) -> RecordBatch {
    let r = || rows.clone();
    RecordBatch::try_new(
        spans_schema(),
        vec![
            fsb(16, r().map(|i| Some(vec![(i / 4) as u8; 16]))),
            fsb(8, r().map(|i| Some(vec![i as u8; 8]))),
            fsb(
                8,
                r().map(|i| (!i.is_multiple_of(4)).then(|| vec![(i - i % 4) as u8; 8])),
            ),
            utf8(r().map(|i| SPAN_NAMES[i % 6].to_owned())),
            utf8_opt(r().map(span_service)),
            time_col(r()),
            Arc::new(Int64Array::from_iter_values(
                r().map(|i| (i * 37 % 500) as i64),
            )),
            utf8(r().map(|i| ["OK", "ERROR", "UNSET"][i % 3].to_owned())),
            utf8(r().map(span_attrs)),
            utf8(r().map(|i| pad("res", i))),
            utf8_opt(r().map(|i| (i % 2 == 1).then(|| pad("evt", i)))),
            utf8_opt(r().map(|i| (i % 3 == 1).then(|| pad("lnk", i)))),
        ],
    )
    .unwrap()
}

#[cfg(feature = "search")]
fn spans_index(dir: &Path, name: &str, rows: Range<usize>) -> Option<PathBuf> {
    let path = dir.join(name);
    let rows: Vec<imbh_core::SpanRow> = rows
        .map(|i| imbh_core::SpanRow {
            trace_id: [(i / 4) as u8; 16],
            span_id: [i as u8; 8],
            parent_span_id: None,
            name: SPAN_NAMES[i % 6].to_owned(),
            kind: "SERVER".to_owned(),
            start_time_unix_nano: i as i64 * 1000,
            duration_ns: (i * 37 % 500) as u64,
            status_code: "OK".to_owned(),
            status_message: None,
            service: span_service(i),
            attributes: span_attrs(i),
            resource: String::new(),
            scope: String::new(),
            events: None,
            links: None,
            trace_state: None,
            flags: 0,
        })
        .collect();
    imbh_index::build_spans_index(&path, &rows).unwrap();
    Some(path)
}
#[cfg(not(feature = "search"))]
fn spans_index(_dir: &Path, _name: &str, _rows: Range<usize>) -> Option<PathBuf> {
    None
}

// ---- metrics ----

fn metrics_schema() -> SchemaRef {
    let list = |t: DataType| DataType::List(Arc::new(Field::new_list_field(t, true)));
    Arc::new(Schema::new(vec![
        Field::new("time", ts(), false),
        Field::new("metric_name", DataType::Utf8, false),
        Field::new("value", DataType::Float64, true),
        Field::new("explicit_bounds", list(DataType::Float64), true),
        Field::new("bucket_counts", list(DataType::UInt64), true),
        Field::new("attributes", DataType::Utf8, false),
        // Promoted label column, absent from the older metrics segment.
        Field::new("host", DataType::Utf8, true),
    ]))
}

fn metrics_batch(rows: Range<usize>) -> RecordBatch {
    let r = || rows.clone();
    let is_hist = |i: usize| i % 3 == 2;
    let mut bounds = ListBuilder::new(Float64Builder::new());
    let mut counts = ListBuilder::new(UInt64Builder::new());
    for i in r() {
        if is_hist(i) {
            bounds.values().append_slice(&[10.0, 50.0, 100.0]);
            bounds.append(true);
            counts
                .values()
                .append_slice(&[(i % 5) as u64, 3, 2, (i % 2) as u64]);
            counts.append(true);
        } else {
            bounds.append(false);
            counts.append(false);
        }
    }
    RecordBatch::try_new(
        metrics_schema(),
        vec![
            time_col(r()),
            utf8(r().map(|i| ["cpu", "mem", "latency"][i % 3].to_owned())),
            Arc::new(Float64Array::from_iter(
                r().map(|i| (!is_hist(i)).then_some(i as f64 * 0.5)),
            )),
            Arc::new(bounds.finish()),
            Arc::new(counts.finish()),
            utf8(r().map(|i| format!(r#"{{"core":"{}","pad":"{}"}}"#, i % 2, pad("a", i)))),
            utf8_opt(r().map(|i| Some(format!("h{}", i % 4)))),
        ],
    )
    .unwrap()
}

/// The whole corpus: three tables, each a buffer snapshot ∪ segments of deliberately mixed shapes.
struct Corpus {
    _dir: tempfile::TempDir,
    tables: Vec<TableInput>,
}

const LOGS_ROWS: usize = 107;
const SPANS_ROWS: usize = 70;

impl Corpus {
    fn build() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();

        // logs — L1: pre-promotion schema (no `env`), 10-row groups, indexed.
        let l1 = d.join("l1.parquet");
        let old_names = names_except(&logs_schema(), "env");
        write_parquet(&l1, &select(&logs_batch(0..40), &old_names), 10, &[]);
        // L2: current columns in *reverse* order plus a dropped `legacy` column, 16-row groups,
        // indexed — a by-index projection would read the wrong columns here.
        let l2 = d.join("l2.parquet");
        let mut rev = names_except(&logs_schema(), "");
        rev.reverse();
        let l2_batch = with_extra(
            &select(&logs_batch(40..80), &rev),
            "legacy",
            utf8((40..80).map(|i| pad("legacy", i))),
        );
        write_parquet(&l2, &l2_batch, 16, &[]);
        // L3: `severity_number` stored as Int64 and `body` as LargeUtf8 (both cast back by
        // `coerce`), one row group, no index, no manifest time range.
        let l3 = d.join("l3.parquet");
        let l3_batch = recast(
            &recast(&logs_batch(80..100), "severity_number", DataType::Int64),
            "body",
            DataType::LargeUtf8,
        );
        assert!(
            l3_batch
                .column_by_name("body")
                .unwrap()
                .as_any()
                .is::<LargeStringArray>()
        );
        write_parquet(&l3, &l3_batch, 1024, &[]);
        let logs = TableInput {
            name: "logs",
            schema: logs_schema(),
            buffer: logs_batch(100..LOGS_ROWS),
            segments: vec![
                segment(l1, logs_index(d, "l1.tidx", 0..40), 0..40, true),
                segment(l2, logs_index(d, "l2.tidx", 40..80), 40..80, true),
                segment(l3, None, 80..100, false),
            ],
            text_column: Some("body"),
            bloom_columns: &[],
            time_column: Some("time"),
        };

        // spans — S1: full schema, 8-row groups, blooms + index. S2: older schema without `links`.
        let s1 = d.join("s1.parquet");
        write_parquet(&s1, &spans_batch(0..32), 8, &["trace_id", "span_id"]);
        let s2 = d.join("s2.parquet");
        let s2_names = names_except(&spans_schema(), "links");
        write_parquet(
            &s2,
            &select(&spans_batch(32..64), &s2_names),
            16,
            &["trace_id", "span_id"],
        );
        let spans = TableInput {
            name: "spans",
            schema: spans_schema(),
            buffer: spans_batch(64..SPANS_ROWS),
            segments: vec![
                segment(s1, spans_index(d, "s1.tidx", 0..32), 0..32, true),
                segment(s2, spans_index(d, "s2.tidx", 32..64), 32..64, true),
            ],
            text_column: Some("name"),
            bloom_columns: &["trace_id", "span_id"],
            time_column: Some("start_time"),
        };

        // metrics — M1: no `host` (pre-promotion), 10-row groups. M2: full schema.
        let m1 = d.join("m1.parquet");
        let m1_names = names_except(&metrics_schema(), "host");
        write_parquet(&m1, &select(&metrics_batch(0..30), &m1_names), 10, &[]);
        let m2 = d.join("m2.parquet");
        write_parquet(&m2, &metrics_batch(30..60), 1024, &[]);
        let metrics = TableInput {
            name: "metrics",
            schema: metrics_schema(),
            buffer: metrics_batch(60..64),
            segments: vec![
                segment(m1, None, 0..30, true),
                segment(m2, None, 30..60, true),
            ],
            text_column: None,
            bloom_columns: &[],
            time_column: Some("time"),
        };

        Corpus {
            _dir: dir,
            tables: vec![logs, spans, metrics],
        }
    }
}

/// Plan and collect `sql` over the corpus with the projection pushdown `on`/off, returning the
/// result schema, the batches exactly as collected, and the scan counters.
async fn run(
    tables: &[TableInput],
    sql: &str,
    pushdown: bool,
) -> (SchemaRef, Vec<RecordBatch>, ScanStats) {
    let (_ctx, df, stats) =
        crate::plan_query_with(tables.to_vec(), 64 << 20, sql, Vec::new(), pushdown)
            .await
            .unwrap_or_else(|e| panic!("plan {sql}: {e}"));
    let schema = df.schema().inner().clone();
    let batches = df
        .collect()
        .await
        .unwrap_or_else(|e| panic!("execute {sql} (pushdown={pushdown}): {e}"));
    (schema, batches, stats.snapshot())
}

fn rows(batches: &[RecordBatch]) -> usize {
    batches.iter().map(RecordBatch::num_rows).sum()
}

type Check = fn(&ScanStats, usize) -> bool;

/// `(sql, optional sanity check on (ON-path stats, result rows))`. The checks only make sure the
/// corpus really exercises the path a query is listed for (an index hit, a pruned segment, …);
/// correctness is the ON == OFF comparison itself.
fn cases() -> Vec<(&'static str, Option<Check>)> {
    let selective_index: Check = |s, _| s.index_searched || cfg!(not(feature = "search"));
    vec![
        // ---- logs ----
        ("SELECT * FROM logs", Some(|_, n| n == LOGS_ROWS)),
        ("SELECT body FROM logs", None),
        ("SELECT env, service FROM logs", None),
        // Tantivy RowSelection; `body` is both output and filter column.
        (
            "SELECT time, body FROM logs WHERE matches(body, 'error')",
            Some(|s, n| {
                n == 22 && (s.rows_scanned < LOGS_ROWS as u64 || cfg!(not(feature = "search")))
            }),
        ),
        // Filter column (`body`) absent from the SELECT list: it must survive the projection.
        (
            "SELECT service FROM logs WHERE matches(body, 'error')",
            Some(selective_index),
        ),
        (
            "SELECT count(*) AS c FROM logs WHERE matches(body, 'error')",
            None,
        ),
        // must / must-not body terms + a GROUP BY on another column.
        (
            "SELECT service, count(*) AS c FROM logs \
             WHERE matches(body, 'request') AND NOT matches(body, 'ok') GROUP BY service",
            None,
        ),
        // Attribute-index pushdown: filter on `attributes`, which is not projected.
        (
            "SELECT body FROM logs WHERE json_get_str(attributes, 'env') = 'staging'",
            Some(selective_index),
        ),
        // Non-selective (cost gate → full scan) body predicate plus a plain column filter.
        (
            "SELECT body, service FROM logs WHERE service = 'api' AND matches(body, 'ok')",
            None,
        ),
        // Empty projection.
        ("SELECT count(*) AS c FROM logs", Some(|_, n| n == 1)),
        // Promoted column: NULL for the pre-promotion segment.
        ("SELECT body FROM logs WHERE env IS NULL", None),
        ("SELECT time FROM logs WHERE env = 'prod'", None),
        // Row-group statistics: a subset of L1's and L2's groups, L3 pruned whole.
        (
            "SELECT body FROM logs \
             WHERE CAST(time AS BIGINT) >= 25000 AND CAST(time AS BIGINT) < 47000",
            Some(|s, n| n == 22 && s.segments_pruned >= 1),
        ),
        // Row-group subset ∩ Tantivy RowSelection.
        (
            "SELECT severity_number FROM logs WHERE CAST(time AS BIGINT) >= 25000 \
             AND CAST(time AS BIGINT) < 47000 AND matches(body, 'timeout')",
            None,
        ),
        // Manifest-range skip + a predicate on a cast-at-read column (L3 stores Int64).
        (
            "SELECT body FROM logs WHERE time > to_timestamp_nanos(90000) AND severity_number > 3",
            None,
        ),
        (
            "SELECT hex(trace_id) AS t, body FROM logs \
             WHERE trace_id IS NOT NULL AND matches(body, 'timeout')",
            None,
        ),
        (
            "SELECT severity_number, body FROM logs ORDER BY time DESC LIMIT 5",
            Some(|_, n| n == 5),
        ),
        ("SELECT body FROM logs LIMIT 3", Some(|_, n| n == 3)),
        // ---- spans ----
        ("SELECT * FROM spans", Some(|_, n| n == SPANS_ROWS)),
        ("SELECT name, duration_ns FROM spans", None),
        // Bloom point lookup: trace 3 lives only in S1, so S2 is pruned.
        (
            "SELECT name, events FROM spans WHERE trace_id = X'03030303030303030303030303030303'",
            Some(|s, n| n == 4 && s.segments_pruned >= 1),
        ),
        (
            "SELECT hex(span_id) AS s FROM spans WHERE trace_id IN (\
             X'01010101010101010101010101010101', X'09090909090909090909090909090909', \
             X'0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f', X'c8c8c8c8c8c8c8c8c8c8c8c8c8c8c8c8')",
            None,
        ),
        // Span-name index (text column `name`) without `name` in the SELECT list.
        (
            "SELECT duration_ns FROM spans WHERE matches(name, 'db')",
            Some(selective_index),
        ),
        (
            "SELECT count(*) AS c FROM spans \
             WHERE json_get_str(attributes, 'http.method') = 'POST'",
            None,
        ),
        (
            "SELECT span_id, links FROM spans \
             WHERE CAST(start_time AS BIGINT) > 20000 AND duration_ns > 100",
            None,
        ),
        (
            "SELECT service, avg(duration_ns) AS d FROM spans GROUP BY service",
            None,
        ),
        ("SELECT count(*) AS c FROM spans", None),
        // ---- metrics ----
        ("SELECT * FROM metrics", None),
        ("SELECT metric_name, value FROM metrics", None),
        // Nested List columns must be projected whole.
        (
            "SELECT histogram_quantile(0.5, explicit_bounds, bucket_counts) AS q FROM metrics \
             WHERE explicit_bounds IS NOT NULL",
            None,
        ),
        ("SELECT bucket_counts FROM metrics WHERE host = 'h1'", None),
        (
            "SELECT host, sum(value) AS s FROM metrics GROUP BY host",
            None,
        ),
        (
            "SELECT count(*) AS c FROM metrics \
             WHERE CAST(time AS BIGINT) >= 12000 AND CAST(time AS BIGINT) <= 25000",
            Some(|s, _| s.rows_scanned < 64 && s.segments_pruned >= 1),
        ),
        // ---- across tables ----
        (
            "SELECT l.body, s.name FROM logs l JOIN spans s ON l.trace_id = s.trace_id",
            None,
        ),
    ]
}

/// The equivalence gate the pushdown ships behind: every corpus query returns identical batches
/// (schema, columns, and batch boundaries) with the pushdown on and off, the same row/segment
/// counters, and never more `bytes_scanned`.
#[tokio::test(flavor = "current_thread")]
#[allow(clippy::await_holding_lock)] // single-thread runtime; the guard serializes the global prune counters
async fn pushdown_on_and_off_return_identical_batches() {
    let _serial = prune_counters::SERIAL
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let corpus = Corpus::build();
    let mut narrowed = 0;
    for (sql, check) in cases() {
        let (on_schema, on, on_stats) = run(&corpus.tables, sql, true).await;
        let (off_schema, off, off_stats) = run(&corpus.tables, sql, false).await;
        assert_eq!(on_schema, off_schema, "result schema differs for {sql}");
        assert_eq!(on, off, "result batches differ for {sql}");
        assert_eq!(on_stats.rows_scanned, off_stats.rows_scanned, "{sql}");
        assert_eq!(
            on_stats.segments_scanned, off_stats.segments_scanned,
            "{sql}"
        );
        assert_eq!(on_stats.segments_pruned, off_stats.segments_pruned, "{sql}");
        assert_eq!(on_stats.index_searched, off_stats.index_searched, "{sql}");
        assert!(
            on_stats.bytes_scanned <= off_stats.bytes_scanned,
            "the pushdown may only shrink bytes_scanned ({} > {}) for {sql}",
            on_stats.bytes_scanned,
            off_stats.bytes_scanned
        );
        if on_stats.bytes_scanned < off_stats.bytes_scanned {
            narrowed += 1;
        }
        if let Some(check) = check {
            assert!(
                check(&on_stats, rows(&on)),
                "corpus sanity check failed for {sql}: {on_stats:?}, {} rows",
                rows(&on)
            );
        }
    }
    assert!(
        narrowed >= 20,
        "most corpus queries are narrow and must read fewer bytes ({narrowed} did)"
    );
    projected_reads::take();
}

/// The point of the pushdown: a two-column traces query decodes only `name`, `duration_ns` and the
/// filter's `start_time` from each span segment — none of the wide JSON columns — and its
/// `bytes_scanned` drops accordingly. `count(*)` decodes no column at all.
#[tokio::test(flavor = "current_thread")]
#[allow(clippy::await_holding_lock)] // single-thread runtime; the guard serializes the global prune counters
async fn narrow_traces_query_skips_the_json_columns() {
    let _serial = prune_counters::SERIAL
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let corpus = Corpus::build();
    let sql = "SELECT name, duration_ns FROM spans WHERE CAST(start_time AS BIGINT) >= 0";

    projected_reads::take();
    let (_, on, on_stats) = run(&corpus.tables, sql, true).await;
    let reads = projected_reads::take();
    assert_eq!(reads.len(), 2, "both span segments are read: {reads:?}");
    for cols in &reads {
        let mut cols = cols.clone();
        cols.sort();
        assert_eq!(
            cols,
            ["duration_ns", "name", "start_time"],
            "only the projected + filter columns are decoded"
        );
    }

    let (_, off, off_stats) = run(&corpus.tables, sql, false).await;
    let reads_off = projected_reads::take();
    assert_eq!(reads_off.len(), 2);
    for cols in &reads_off {
        for wide in ["attributes", "resource", "events"] {
            assert!(
                cols.iter().any(|c| c == wide),
                "the OFF path reads every column: {cols:?}"
            );
        }
    }
    assert_eq!(on, off);
    assert_eq!(rows(&on), SPANS_ROWS);
    assert!(
        on_stats.bytes_scanned * 4 < off_stats.bytes_scanned,
        "bytes_scanned should drop sharply: on={} off={}",
        on_stats.bytes_scanned,
        off_stats.bytes_scanned
    );

    // `count(*)`: the empty projection reaches the reader, which decodes no column at all.
    let (_, counted, _) = run(&corpus.tables, "SELECT count(*) AS c FROM spans", true).await;
    let reads = projected_reads::take();
    assert_eq!(reads.len(), 2);
    assert!(reads.iter().all(Vec::is_empty), "{reads:?}");
    let c = counted[0]
        .column(0)
        .as_any()
        .downcast_ref::<Int64Array>()
        .unwrap()
        .value(0);
    assert_eq!(c, SPANS_ROWS as i64);
}

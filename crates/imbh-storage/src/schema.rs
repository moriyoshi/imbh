//! The Arrow schema for the `logs` table (ARCHITECTURE.md §6.2).
//!
//! `imbh-storage` owns Arrow-schema construction and hands the resulting [`SchemaRef`] to
//! `imbh-query` through the facade, so the two agree without either depending on the other
//! (ARCHITECTURE.md §9.1/§12). Arrow types come via DataFusion's re-exports — never a direct
//! `arrow` dependency — to make version skew impossible.
//!
//! The three low-cardinality string columns `service`/`resource`/`scope` are dict-encoded as
//! `Dictionary(Int32, Utf8)` (ARCHITECTURE.md §6.2) so resource/scope are nearly free; the higher-
//! cardinality strings (`body`/`attributes`/`metric`/`unit`/`severity_text`/`kind`/`status_code`/
//! `name`) stay plain `Utf8`. Dict-encoding does not affect SQL correctness — the query layer
//! coerces every buffer/segment batch to these field types before the union.

use std::sync::Arc;

use arrow::datatypes::{DataType, Field, Schema, SchemaRef, TimeUnit};

/// UTC-timestamped nanosecond time type, matching `Timestamp` storage in `imbh-core`.
fn ts() -> DataType {
    DataType::Timestamp(TimeUnit::Nanosecond, Some("UTC".into()))
}

/// Dictionary-encoded UTF-8 (`Int32` keys) for the low-cardinality `service`/`resource`/`scope`
/// columns (ARCHITECTURE.md §6.2). Keeping these as dictionaries makes resource/scope nearly free.
/// Promoted label columns share this type — they are low-cardinality strings like `service`.
fn dict_utf8() -> DataType {
    DataType::Dictionary(Box::new(DataType::Int32), Box::new(DataType::Utf8))
}

/// Built-in column names across all six table schemas. A promoted key (ARCHITECTURE.md §6.1) that
/// collides with one of these is dropped by [`promoted_columns`] — it is already a first-class
/// column, so promoting it again would duplicate a name. Kept as one union (not per-signal) so the
/// promoted-column set is uniform across signals and a key means the same column everywhere.
const RESERVED_COLUMNS: &[&str] = &[
    "attributes",
    "body",
    "bucket_counts",
    "count",
    "duration_ns",
    "events",
    "exemplars",
    "explicit_bounds",
    "flags",
    "is_monotonic",
    "kind",
    "links",
    "max",
    "metric",
    "min",
    "name",
    "negative_counts",
    "negative_offset",
    "observed_time",
    "parent_span_id",
    "positive_counts",
    "positive_offset",
    "quantiles",
    "resource",
    "scale",
    "scope",
    "service",
    "severity_number",
    "severity_text",
    "span_id",
    "start_time",
    "status_code",
    "status_message",
    "sum",
    "temporality",
    "time",
    "trace_id",
    "trace_state",
    "unit",
    "value",
    "values",
    "zero_count",
    "zero_threshold",
];

/// The effective promoted column names for `promote`: the requested keys minus any that collide
/// with a built-in column ([`RESERVED_COLUMNS`]) or repeat, first-occurrence order preserved. Shared
/// by every `*_schema()` builder **and** the matching `*_rows_to_batch` in `lib.rs`, so the schema
/// and the built columns always agree in name, count, and order (ARCHITECTURE.md §6.1).
pub fn promoted_columns(promote: &[String]) -> Vec<&str> {
    let mut out: Vec<&str> = Vec::new();
    for k in promote {
        let k = k.as_str();
        if !RESERVED_COLUMNS.contains(&k) && !out.contains(&k) {
            out.push(k);
        }
    }
    out
}

/// Append the promoted label columns (nullable `Dictionary(Int32,Utf8)`, like `service`) to a
/// signal's fixed field list, in [`promoted_columns`] order.
fn with_promoted(mut fields: Vec<Field>, promote: &[String]) -> Vec<Field> {
    for name in promoted_columns(promote) {
        fields.push(Field::new(name, dict_utf8(), true));
    }
    fields
}

/// The `logs` table Arrow schema. `promote` appends nullable label columns (ARCHITECTURE.md §6.1);
/// pass `&[]` for the fixed schema.
pub fn logs_schema(promote: &[String]) -> SchemaRef {
    Arc::new(Schema::new(with_promoted(
        vec![
            Field::new("time", ts(), false),
            Field::new("observed_time", ts(), true),
            Field::new("service", dict_utf8(), true),
            Field::new("severity_number", DataType::UInt8, false),
            Field::new("severity_text", DataType::Utf8, true),
            Field::new("body", DataType::Utf8, false),
            Field::new("attributes", DataType::Utf8, false),
            Field::new("resource", dict_utf8(), false),
            Field::new("scope", dict_utf8(), false),
            Field::new("trace_id", DataType::FixedSizeBinary(16), true),
            Field::new("span_id", DataType::FixedSizeBinary(8), true),
            Field::new("flags", DataType::UInt32, false),
        ],
        promote,
    )))
}

/// Shared Arrow schema for the scalar metric tables `metrics_gauge` and `metrics_sum`
/// (ARCHITECTURE.md §6.4). Gauge rows leave `temporality`/`is_monotonic` null. Column order matches
/// `scalar_metrics_rows_to_batch`.
pub fn metric_scalar_schema(promote: &[String]) -> SchemaRef {
    Arc::new(Schema::new(with_promoted(
        vec![
            Field::new("time", ts(), false),
            Field::new("start_time", ts(), true),
            Field::new("metric", DataType::Utf8, false),
            Field::new("unit", DataType::Utf8, false),
            Field::new("service", dict_utf8(), true),
            Field::new("attributes", DataType::Utf8, false),
            Field::new("resource", dict_utf8(), false),
            Field::new("scope", dict_utf8(), false),
            Field::new("flags", DataType::UInt32, false),
            Field::new("value", DataType::Float64, false),
            Field::new("temporality", DataType::Utf8, true),
            Field::new("is_monotonic", DataType::Boolean, true),
            // Canonical-JSON array of exemplars (trace links); empty string when none (ARCHITECTURE.md §6.4).
            Field::new("exemplars", DataType::Utf8, false),
        ],
        promote,
    )))
}

/// Arrow schema for the `metrics_histogram` table (ARCHITECTURE.md §6.4). `explicit_bounds` (N ascending
/// upper bounds) and `bucket_counts` (N+1 counts, last = `+Inf`) are `List` columns; the child
/// fields use [`Field::new_list_field`] so they match exactly what `ListBuilder` emits (child named
/// `item`, nullable) and survive the Parquet round-trip. Column order matches
/// `histogram_rows_to_batch`.
pub fn histogram_schema(promote: &[String]) -> SchemaRef {
    let float_list = DataType::List(Arc::new(Field::new_list_field(DataType::Float64, true)));
    let uint_list = DataType::List(Arc::new(Field::new_list_field(DataType::UInt64, true)));
    Arc::new(Schema::new(with_promoted(
        vec![
            Field::new("time", ts(), false),
            Field::new("start_time", ts(), true),
            Field::new("metric", DataType::Utf8, false),
            Field::new("unit", DataType::Utf8, false),
            Field::new("service", dict_utf8(), true),
            Field::new("attributes", DataType::Utf8, false),
            Field::new("resource", dict_utf8(), false),
            Field::new("scope", dict_utf8(), false),
            Field::new("flags", DataType::UInt32, false),
            Field::new("count", DataType::UInt64, false),
            Field::new("sum", DataType::Float64, true),
            Field::new("min", DataType::Float64, true),
            Field::new("max", DataType::Float64, true),
            Field::new("explicit_bounds", float_list, false),
            Field::new("bucket_counts", uint_list, false),
            Field::new("temporality", DataType::Utf8, true),
            Field::new("exemplars", DataType::Utf8, false),
        ],
        promote,
    )))
}

/// Arrow schema for the `metrics_exp_histogram` table (ARCHITECTURE.md §6.4). Base-2 histogram: `scale`
/// sets the resolution, `zero_count`/`zero_threshold` the near-zero bucket, and the positive/
/// negative ranges each an `*_offset` (Int32) + `*_counts` (`List<UInt64>`). Column order matches
/// `exp_histogram_rows_to_batch`.
pub fn exp_histogram_schema(promote: &[String]) -> SchemaRef {
    let uint_list = DataType::List(Arc::new(Field::new_list_field(DataType::UInt64, true)));
    Arc::new(Schema::new(with_promoted(
        vec![
            Field::new("time", ts(), false),
            Field::new("start_time", ts(), true),
            Field::new("metric", DataType::Utf8, false),
            Field::new("unit", DataType::Utf8, false),
            Field::new("service", dict_utf8(), true),
            Field::new("attributes", DataType::Utf8, false),
            Field::new("resource", dict_utf8(), false),
            Field::new("scope", dict_utf8(), false),
            Field::new("flags", DataType::UInt32, false),
            Field::new("count", DataType::UInt64, false),
            Field::new("sum", DataType::Float64, true),
            Field::new("min", DataType::Float64, true),
            Field::new("max", DataType::Float64, true),
            Field::new("scale", DataType::Int32, false),
            Field::new("zero_count", DataType::UInt64, false),
            Field::new("zero_threshold", DataType::Float64, false),
            Field::new("positive_offset", DataType::Int32, false),
            Field::new("positive_counts", uint_list.clone(), false),
            Field::new("negative_offset", DataType::Int32, false),
            Field::new("negative_counts", uint_list, false),
            Field::new("temporality", DataType::Utf8, true),
            Field::new("exemplars", DataType::Utf8, false),
        ],
        promote,
    )))
}

/// Arrow schema for the `metrics_summary` table (ARCHITECTURE.md §6.4). Summaries store precomputed
/// quantiles: `quantiles` (the phi levels) and `values` (the value at each), both `List<Float64>`
/// and index-paired. Column order matches `summary_rows_to_batch`.
pub fn summary_schema(promote: &[String]) -> SchemaRef {
    let float_list = DataType::List(Arc::new(Field::new_list_field(DataType::Float64, true)));
    Arc::new(Schema::new(with_promoted(
        vec![
            Field::new("time", ts(), false),
            Field::new("start_time", ts(), true),
            Field::new("metric", DataType::Utf8, false),
            Field::new("unit", DataType::Utf8, false),
            Field::new("service", dict_utf8(), true),
            Field::new("attributes", DataType::Utf8, false),
            Field::new("resource", dict_utf8(), false),
            Field::new("scope", dict_utf8(), false),
            Field::new("flags", DataType::UInt32, false),
            Field::new("count", DataType::UInt64, false),
            Field::new("sum", DataType::Float64, false),
            Field::new("quantiles", float_list.clone(), false),
            Field::new("values", float_list, false),
            // Summaries have no OTLP temporality; the column exists (always null) so the metric tables
            // share the `metric`/`unit`/`temporality` catalog identity uniformly.
            Field::new("temporality", DataType::Utf8, true),
        ],
        promote,
    )))
}

/// The `spans` table Arrow schema (ARCHITECTURE.md §6.3). Column order matches `spans_rows_to_batch`.
/// `name`/`kind`/`status_code` stay `Utf8`; `service` (like `resource`/`scope`) is dict-encoded.
pub fn spans_schema(promote: &[String]) -> SchemaRef {
    Arc::new(Schema::new(with_promoted(
        vec![
            Field::new("trace_id", DataType::FixedSizeBinary(16), false),
            Field::new("span_id", DataType::FixedSizeBinary(8), false),
            Field::new("parent_span_id", DataType::FixedSizeBinary(8), true),
            Field::new("name", DataType::Utf8, false),
            Field::new("kind", DataType::Utf8, false),
            Field::new("start_time", ts(), false),
            Field::new("duration_ns", DataType::UInt64, false),
            Field::new("status_code", DataType::Utf8, false),
            Field::new("status_message", DataType::Utf8, true),
            Field::new("service", dict_utf8(), true),
            Field::new("attributes", DataType::Utf8, false),
            Field::new("resource", dict_utf8(), false),
            Field::new("scope", dict_utf8(), false),
            Field::new("events", DataType::Utf8, true),
            Field::new("links", DataType::Utf8, true),
            Field::new("trace_state", DataType::Utf8, true),
            Field::new("flags", DataType::UInt32, false),
        ],
        promote,
    )))
}

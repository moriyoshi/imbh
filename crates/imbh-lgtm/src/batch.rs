//! Arrow-native result batches for the LGTM execution surface.
//!
//! The `*SemanticsExt` traits also expose `*_batches` twins of their typed executors that return
//! these `RecordBatch`es instead of `Vec<PromSeries>` / `Vec<LogSeries>` / `Vec<TraceQueryMatch>`.
//! An Arrow result is what makes the LGTM surface usable the same three ways the facade's own
//! `*_batches` are: zero-copy over the Arrow C Data Interface (out-of-process bindings), directly
//! `SELECT`-able by DataFusion (analytics on results), and free of a bespoke row DTO.
//!
//! Layout is **long form** (one row per sample / per matched trace) so callers can `GROUP BY` /
//! join / filter the result with plain SQL. String payload (labels, ids) uses [`DataType::Utf8View`]
//! so the columns are view-capable: today the builders copy at the `Vec<Struct>` -> batch boundary,
//! but the schema is already the view type, so a later revision can share the scan's `Arc<Buffer>`s
//! through this surface without changing the schema or any signature (see
//! `.agents/docs/ARROW_LGTM_API_PROPOSAL.md`).
//!
//! Schemas are derived from the built arrays' own data types, so an empty result carries a schema
//! byte-for-byte identical to a populated one (the `run_sql` / `collect_with_schema` invariant).

use std::sync::Arc;

use imbh::arrow::array::{
    Array, ArrayRef, Float64Array, Float64Builder, ListBuilder, MapBuilder, StringViewBuilder,
    TimestampNanosecondArray, UInt64Builder,
};
use imbh::arrow::datatypes::{Field, Schema, SchemaRef};
use imbh::arrow::record_batch::RecordBatch;

use crate::{LabelSet, LogSeries, PromHistogramSeries, PromSeries, TraceQueryMatch};

/// A string-view builder that deduplicates its bytes: repeated strings become additional *views*
/// into one shared buffer region rather than fresh copies. This is the load-bearing "views" win of
/// the long-form layout - a series' labels repeat once per sample, and every metric/stream shares a
/// small vocabulary of label keys/values, so dedup collapses that repetition to one copy per distinct
/// string. (Views are the byte-sharing mechanism; this is the most we can share without threading the
/// scan's `Arc<Buffer>`s through the owned `LabelSet` type - see `ARROW_LGTM_API_PROPOSAL.md`.)
fn view_builder() -> StringViewBuilder {
    StringViewBuilder::new().with_deduplicate_strings()
}

fn label_map_builder() -> MapBuilder<StringViewBuilder, StringViewBuilder> {
    MapBuilder::new(None, view_builder(), view_builder())
}

fn append_labels(
    builder: &mut MapBuilder<StringViewBuilder, StringViewBuilder>,
    labels: &LabelSet,
) {
    for (key, value) in labels.iter() {
        builder.keys().append_value(key);
        builder.values().append_value(value);
    }
    builder
        .append(true)
        .expect("map row append is infallible for balanced key/value counts");
}

fn col<A: Array + 'static>(array: A) -> ArrayRef {
    Arc::new(array)
}

/// Assemble a batch, deriving each column's `Field` from the array it holds so the schema always
/// matches (and an empty batch's schema matches a populated one). Top-level columns are non-null by
/// construction: every emitted row carries a value in every column.
fn finish_batch(columns: Vec<(&str, ArrayRef)>) -> RecordBatch {
    let fields = columns
        .iter()
        .map(|(name, array)| Field::new(*name, array.data_type().clone(), false))
        .collect::<Vec<_>>();
    let arrays = columns
        .into_iter()
        .map(|(_, array)| array)
        .collect::<Vec<_>>();
    RecordBatch::try_new(Arc::new(Schema::new(fields)), arrays)
        .expect("imbh-lgtm result columns are length-consistent by construction")
}

fn series_to_batch<'a, 'b: 'a>(
    series: impl IntoIterator<Item = (&'a LabelSet<'b>, &'a [crate::FloatSample])>,
) -> RecordBatch {
    let mut labels = label_map_builder();
    let mut timestamps: Vec<i64> = Vec::new();
    let mut values: Vec<f64> = Vec::new();
    for (label_set, samples) in series {
        for sample in samples {
            append_labels(&mut labels, label_set);
            timestamps.push(sample.timestamp_ns);
            values.push(sample.value);
        }
    }
    finish_batch(vec![
        ("labels", col(labels.finish())),
        ("ts", col(TimestampNanosecondArray::from(timestamps))),
        ("value", col(Float64Array::from(values))),
    ])
}

/// PromQL matrix result as `{ labels: Map<Utf8View,Utf8View>, ts: Timestamp(ns), value: f64 }`,
/// one row per sample.
pub fn prom_series_to_batch(series: &[PromSeries<'_>]) -> RecordBatch {
    series_to_batch(series.iter().map(|s| (&s.labels, s.samples.as_slice())))
}

/// LogQL metric result (count/rate over time). Identical schema to [`prom_series_to_batch`].
pub fn log_series_to_batch(series: &[LogSeries<'_>]) -> RecordBatch {
    series_to_batch(series.iter().map(|s| (&s.labels, s.samples.as_slice())))
}

/// PromQL native-histogram series as
/// `{ labels, ts, explicit_bounds: List<f64>, bucket_counts: List<u64> }`, one row per point.
pub fn prom_histogram_to_batch(series: &[PromHistogramSeries<'_>]) -> RecordBatch {
    let mut labels = label_map_builder();
    let mut timestamps: Vec<i64> = Vec::new();
    let mut bounds = ListBuilder::new(Float64Builder::new());
    let mut counts = ListBuilder::new(UInt64Builder::new());
    for entry in series {
        for point in &entry.points {
            append_labels(&mut labels, &entry.labels);
            timestamps.push(point.timestamp_ns);
            for bound in point.explicit_bounds {
                bounds.values().append_value(*bound);
            }
            bounds.append(true);
            for count in point.bucket_counts {
                counts.values().append_value(*count);
            }
            counts.append(true);
        }
    }
    finish_batch(vec![
        ("labels", col(labels.finish())),
        ("ts", col(TimestampNanosecondArray::from(timestamps))),
        ("explicit_bounds", col(bounds.finish())),
        ("bucket_counts", col(counts.finish())),
    ])
}

/// TraceQL result as `{ trace_id: Utf8View, span_ids: List<Utf8View> }`, one row per matched trace.
pub fn trace_matches_to_batch(matches: &[TraceQueryMatch]) -> RecordBatch {
    let mut trace_ids = view_builder();
    let mut span_ids = ListBuilder::new(view_builder());
    for entry in matches {
        trace_ids.append_value(&entry.trace_id);
        for span_id in &entry.spanset.selected_span_ids {
            span_ids.values().append_value(span_id);
        }
        span_ids.append(true);
    }
    finish_batch(vec![
        ("trace_id", col(trace_ids.finish())),
        ("span_ids", col(span_ids.finish())),
    ])
}

/// Canonical schema for the PromQL matrix / LogQL metric batches (identical for both).
pub fn prom_matrix_schema() -> SchemaRef {
    prom_series_to_batch(&[]).schema()
}

/// Canonical schema for the LogQL metric batch. Identical to [`prom_matrix_schema`]; provided as a
/// named entry point for LogQL callers.
pub fn log_matrix_schema() -> SchemaRef {
    log_series_to_batch(&[]).schema()
}

/// Canonical schema for the PromQL native-histogram batch.
pub fn prom_histogram_schema() -> SchemaRef {
    prom_histogram_to_batch(&[]).schema()
}

/// Canonical schema for the TraceQL match batch.
pub fn trace_matches_schema() -> SchemaRef {
    trace_matches_to_batch(&[]).schema()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FloatSample, TraceSpanset};
    use imbh::arrow::array::{Float64Array, ListArray, MapArray, StringViewArray};
    use imbh::arrow::datatypes::DataType;

    fn series<'a>(labels: &[(&'a str, &'a str)], samples: &[(i64, f64)]) -> PromSeries<'a> {
        PromSeries {
            labels: LabelSet::new(labels.iter().map(|(k, v)| (*k, *v))),
            samples: samples
                .iter()
                .map(|(timestamp_ns, value)| FloatSample {
                    timestamp_ns: *timestamp_ns,
                    value: *value,
                })
                .collect(),
        }
    }

    #[test]
    fn prom_matrix_is_long_form_with_one_row_per_sample() {
        let input = vec![
            series(&[("route", "/checkout")], &[(10, 1.0), (20, 1.2)]),
            series(&[("route", "/cart")], &[(10, 4.0)]),
        ];
        let batch = prom_series_to_batch(&input);

        assert_eq!(batch.num_rows(), 3, "one row per sample across both series");
        assert_eq!(
            batch.schema().field(0).name(),
            "labels",
            "columns are labels, ts, value in order",
        );
        assert!(matches!(
            batch.schema().field(0).data_type(),
            DataType::Map(_, _),
        ));
        assert_eq!(batch.schema().field(1).name(), "ts");
        assert_eq!(batch.schema().field(2).name(), "value");

        let values = batch
            .column(2)
            .as_any()
            .downcast_ref::<Float64Array>()
            .expect("value column is Float64");
        assert_eq!(values.values(), &[1.0, 1.2, 4.0]);

        let labels = batch
            .column(0)
            .as_any()
            .downcast_ref::<MapArray>()
            .expect("labels column is a Map");
        // First row's single label is route=/checkout, stored as Utf8View.
        let keys = labels
            .keys()
            .as_any()
            .downcast_ref::<StringViewArray>()
            .expect("map keys are Utf8View");
        assert_eq!(keys.value(0), "route");
    }

    #[test]
    fn trace_matches_carry_trace_id_and_span_id_list() {
        let matches = vec![
            TraceQueryMatch {
                trace_id: "aabb".to_owned(),
                start_time_ns: 0,
                spanset: TraceSpanset {
                    selected_span_ids: vec!["01".to_owned(), "02".to_owned()],
                },
            },
            TraceQueryMatch {
                trace_id: "ccdd".to_owned(),
                start_time_ns: 0,
                spanset: TraceSpanset {
                    selected_span_ids: vec!["03".to_owned()],
                },
            },
        ];
        let batch = trace_matches_to_batch(&matches);

        assert_eq!(batch.num_rows(), 2, "one row per matched trace");
        let ids = batch
            .column(0)
            .as_any()
            .downcast_ref::<StringViewArray>()
            .expect("trace_id is Utf8View");
        assert_eq!(ids.value(0), "aabb");
        assert_eq!(ids.value(1), "ccdd");

        let spans = batch
            .column(1)
            .as_any()
            .downcast_ref::<ListArray>()
            .expect("span_ids is a List");
        assert_eq!(spans.value_length(0), 2, "first trace selects two spans");
        assert_eq!(spans.value_length(1), 1);
    }

    #[test]
    fn repeated_labels_share_one_buffer_copy_across_samples() {
        // A label value longer than 12 bytes is stored in the view's data buffer (short strings are
        // inlined into the view). Repeated once per sample across a long series, dedup must collapse
        // those copies to a single buffer region - the long-form "views" win.
        let long_value = "/api/v1/checkout/confirm"; // 24 bytes, not inlinable
        let samples: Vec<(i64, f64)> = (0..200).map(|i| (i, i as f64)).collect();
        let batch = prom_series_to_batch(&[series(&[("route", long_value)], &samples)]);
        assert_eq!(batch.num_rows(), 200);

        let values = batch
            .column(0)
            .as_any()
            .downcast_ref::<MapArray>()
            .expect("labels is a Map")
            .values()
            .as_any()
            .downcast_ref::<StringViewArray>()
            .expect("map values are Utf8View")
            .clone();

        let buffer_bytes: usize = values.data_buffers().iter().map(|b| b.len()).sum();
        // Without dedup this would be 200 * 24 = 4800 bytes; dedup keeps a single copy.
        assert!(
            buffer_bytes < 200 * long_value.len(),
            "repeated label value should share buffer bytes, got {buffer_bytes} bytes",
        );
        assert!(
            buffer_bytes < 4 * long_value.len(),
            "dedup should keep roughly one copy of the repeated value, got {buffer_bytes} bytes",
        );
        // Correctness is preserved: every row still reads back the full value.
        assert_eq!(values.value(0), long_value);
        assert_eq!(values.value(199), long_value);
    }

    #[test]
    fn empty_result_still_carries_the_schema() {
        let batch = prom_series_to_batch(&[]);
        assert_eq!(batch.num_rows(), 0);
        assert_eq!(batch.schema(), prom_matrix_schema());
        // Empty and populated batches share an identical schema.
        assert_eq!(
            batch.schema(),
            prom_series_to_batch(&[series(&[("a", "b")], &[(1, 1.0)])]).schema(),
        );
    }
}

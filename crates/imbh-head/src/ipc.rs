//! Arrow IPC: the wire format for every row-shaped head result.
//!
//! # Why not JSON
//!
//! JSON cannot carry these results soundly. It has no `NaN`, no `Infinity`, and no `-Infinity`, and
//! `serde_json` writes all three as `null` — which then fails to read back as an `f64`. A PromQL
//! evaluation produces all three routinely (`histogram_quantile` over an empty window, a division by
//! zero), so a sample matrix encoded as JSON numbers is a transport error waiting for ordinary data.
//! Arrow's `Float64Array` stores the IEEE-754 bit pattern, so the question does not arise.
//!
//! It is also the format these results are already *in*: a query answers in `RecordBatch`es, and
//! `arrow-ipc` is already compiled into every build that has DataFusion, so this codec costs no
//! dependency at all.
//!
//! Small, scalar answers — stats, the metric catalog, exemplars, attribute vocabularies — stay JSON.
//! They are neither tabular nor float-bearing, and a batch would be more machinery than payload.
//!
//! # What crosses, and how
//!
//! One `RecordBatch` per response, framed as an Arrow **IPC stream**. Anything that is not row-shaped
//! — a paging cursor, the scan counters, a trace's assembled header, the narrowed window start —
//! rides in the schema's custom metadata, so a response is one self-describing message with no
//! side-channel.
//!
//! The encoders take the *materialized* result types rather than the engine's own `*_batches` twins.
//! That is what keeps [`exec`](crate::exec) at one return type for both backends: a local head uses
//! the value directly and never encodes anything, and the remote path is that same value through
//! [`encode`] and back through [`decode`]. The `*_roundtrip` tests below hold the two identical.
//!
//! The series schema is deliberately the one `imbh_lgtm::prom_matrix_schema` already defines
//! (`{labels, ts, value}`, long form, one row per sample), so a head result is the same shape as the
//! engine's own Arrow surface; `series_schema_matches_the_engines_own` pins that.

use std::collections::HashMap;
use std::sync::Arc;

use imbh::arrow::array::{
    Array, ArrayRef, Float64Array, ListBuilder, MapArray, MapBuilder, RecordBatch, StringViewArray,
    StringViewBuilder, TimestampNanosecondArray, UInt8Array, UInt32Array, UInt64Array,
};
use imbh::arrow::datatypes::{Field, Schema, SchemaRef};
use imbh::arrow::ipc::reader::StreamReader;
use imbh::arrow::ipc::writer::StreamWriter;
use imbh::{
    Attributes, DurationNs, LogEntry, LogPage, QueryStats, SeverityNumber, Span, SpanId, Timestamp,
    Trace, TraceId,
};

use crate::{HeadError, dto};

/// The media type an Arrow IPC stream travels under. Registered by the Arrow project; a client that
/// does not recognise it will at least not try to parse the body as text.
pub const CONTENT_TYPE: &str = "application/vnd.apache.arrow.stream";

// Schema-metadata keys. Namespaced so they cannot collide with anything Arrow or DataFusion writes.
const META_SERIES_STARTS: &str = "imbh.series.starts";
const META_EFFECTIVE_START: &str = "imbh.traces.effective_start_ns";
const META_LOG_STATS: &str = "imbh.logs.stats";
const META_LOG_NEXT: &str = "imbh.logs.next_cursor";
const META_TRACE_PRESENT: &str = "imbh.trace.present";
const META_TRACE_ID: &str = "imbh.trace.trace_id";
const META_TRACE_ROOT_SERVICE: &str = "imbh.trace.root_service";
const META_TRACE_ROOT_NAME: &str = "imbh.trace.root_name";
const META_TRACE_START: &str = "imbh.trace.start_time_ns";
const META_TRACE_DURATION: &str = "imbh.trace.duration_ns";

// ── framing ─────────────────────────────────────────────────────────────────────────────────────

/// Frame one batch as an Arrow IPC stream.
pub fn encode(batch: &RecordBatch) -> Result<Vec<u8>, HeadError> {
    let mut out = Vec::new();
    let mut writer = StreamWriter::try_new(&mut out, &batch.schema())
        .map_err(|e| internal(format!("cannot start an Arrow IPC stream: {e}")))?;
    writer
        .write(batch)
        .map_err(|e| internal(format!("cannot write an Arrow IPC batch: {e}")))?;
    writer
        .finish()
        .map_err(|e| internal(format!("cannot finish an Arrow IPC stream: {e}")))?;
    Ok(out)
}

/// Read back a stream framed by [`encode`].
///
/// A stream with no batches is a legitimate empty result — the schema (and so the metadata) still
/// arrives — so it decodes to a zero-row batch rather than an error. Several batches concatenate,
/// which the encoders here never produce but a future streaming writer might.
pub fn decode(bytes: &[u8]) -> Result<RecordBatch, HeadError> {
    let reader = StreamReader::try_new(std::io::Cursor::new(bytes), None)
        .map_err(|e| malformed(format!("not an Arrow IPC stream: {e}")))?;
    let schema = reader.schema();
    let batches = reader
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| malformed(format!("malformed Arrow IPC stream: {e}")))?;
    match batches.len() {
        1 => Ok(batches.into_iter().next().expect("length checked")),
        0 => Ok(RecordBatch::new_empty(schema)),
        _ => imbh::arrow::compute::concat_batches(&schema, &batches)
            .map_err(|e| malformed(format!("cannot concatenate Arrow IPC batches: {e}"))),
    }
}

// ── series (PromQL / LogQL) ─────────────────────────────────────────────────────────────────────

/// `{ labels: Map<Utf8View,Utf8View>, ts: Timestamp(ns), value: Float64 }`, one row per sample —
/// the long form `imbh_lgtm::prom_matrix_schema` defines.
///
/// A series with no samples contributes no rows and so does not survive the encoding. That is the
/// engine's own Arrow surface behaving the same way, and an empty series carries no information a
/// head renders.
///
/// The row offset each surviving series starts at rides in the schema metadata. Within one
/// evaluation a label set identifies a series, so the boundaries could be recovered by grouping runs
/// of equal labels — but a result may be the *concatenation* of several evaluations
/// ([`exec::promql`](crate::exec::promql) takes a batch of queries), and PromQL aggregation drops
/// `__name__`, so two queries routinely answer with series that carry identical labels. Recovering
/// the boundaries by grouping would silently fuse those into one, which is a remote head answering
/// differently from a local one over the same data.
pub fn series_to_batch(series: &[dto::Series]) -> RecordBatch {
    let mut labels = MapBuilder::new(None, view_builder(), view_builder());
    let mut timestamps: Vec<i64> = Vec::new();
    let mut values: Vec<f64> = Vec::new();
    let mut starts: Vec<String> = Vec::new();
    for item in series {
        if item.samples.is_empty() {
            continue;
        }
        starts.push(timestamps.len().to_string());
        for sample in &item.samples {
            for label in &item.labels {
                labels.keys().append_value(&label.name);
                labels.values().append_value(&label.value);
            }
            labels
                .append(true)
                .expect("map row append is infallible for balanced key/value counts");
            timestamps.push(sample.timestamp_ns);
            values.push(sample.value);
        }
    }
    batch_with_metadata(
        vec![
            ("labels", col(labels.finish())),
            ("ts", col(TimestampNanosecondArray::from(timestamps))),
            ("value", col(Float64Array::from(values))),
        ],
        [(META_SERIES_STARTS.to_owned(), starts.join(","))].into(),
    )
}

/// Rebuild the series from a long-form batch, splitting it at the row offsets
/// [`series_to_batch`] recorded in the schema metadata, so order, grouping, and two same-labelled
/// series' separateness all survive.
///
/// A batch with no such metadata is one an older daemon wrote; fall back to grouping the runs of
/// consecutive rows that share a label set, which is what this always did and is exact for any
/// single-query result.
pub fn series_from_batch(batch: &RecordBatch) -> Result<Vec<dto::Series>, HeadError> {
    let labels = column::<MapArray>(batch, "labels")?;
    let timestamps = column::<TimestampNanosecondArray>(batch, "ts")?;
    let values = column::<Float64Array>(batch, "value")?;
    let starts: Option<Vec<usize>> = batch
        .schema()
        .metadata()
        .get(META_SERIES_STARTS)
        .map(|encoded| {
            encoded
                .split(',')
                .filter(|part| !part.is_empty())
                .map(|part| {
                    part.parse::<usize>().map_err(|_| {
                        malformed(format!("series start offsets are not numbers: {encoded}"))
                    })
                })
                .collect::<Result<Vec<_>, _>>()
        })
        .transpose()?;

    let mut out: Vec<dto::Series> = Vec::new();
    // The recorded offsets are ascending, so one cursor walks them alongside the rows.
    let mut next_start = 0usize;
    for row in 0..batch.num_rows() {
        let row_labels = map_row(labels, row)?;
        let sample = dto::SamplePoint {
            timestamp_ns: timestamps.value(row),
            value: values.value(row),
        };
        let continues = match &starts {
            Some(starts) => {
                let breaks = starts.get(next_start) == Some(&row);
                if breaks {
                    next_start += 1;
                }
                !breaks
            }
            // No boundaries recorded: a run of equal labels is one series.
            None => out.last().is_some_and(|series| series.labels == row_labels),
        };
        match out.last_mut() {
            Some(series) if continues => series.samples.push(sample),
            _ => out.push(dto::Series {
                labels: row_labels,
                samples: vec![sample],
            }),
        }
    }
    Ok(out)
}

// ── TraceQL search ──────────────────────────────────────────────────────────────────────────────

/// `{ trace_id: Utf8View, start_time_ns: Timestamp(ns), span_ids: List<Utf8View> }`, one row per
/// matched trace, plus the narrowed window start in the schema metadata.
///
/// A superset of `imbh_lgtm::trace_matches_schema`, which carries no start time; a head lists *when*
/// each match happened, and re-fetching every trace to learn that would cost one query per row.
pub fn trace_search_to_batch(search: &dto::TraceSearch) -> RecordBatch {
    let mut trace_ids = view_builder();
    let mut starts: Vec<i64> = Vec::new();
    let mut span_ids = ListBuilder::new(view_builder());
    for item in &search.matches {
        trace_ids.append_value(&item.trace_id);
        starts.push(item.start_time_ns);
        for span_id in &item.selected_span_ids {
            span_ids.values().append_value(span_id);
        }
        span_ids.append(true);
    }
    batch_with_metadata(
        vec![
            ("trace_id", col(trace_ids.finish())),
            ("start_time_ns", col(TimestampNanosecondArray::from(starts))),
            ("span_ids", col(span_ids.finish())),
        ],
        [(
            META_EFFECTIVE_START.to_owned(),
            search.effective_start_ns.to_string(),
        )]
        .into(),
    )
}

pub fn trace_search_from_batch(batch: &RecordBatch) -> Result<dto::TraceSearch, HeadError> {
    let trace_ids = column::<StringViewArray>(batch, "trace_id")?;
    let starts = column::<TimestampNanosecondArray>(batch, "start_time_ns")?;
    let span_ids = column::<imbh::arrow::array::ListArray>(batch, "span_ids")?;
    let mut matches = Vec::with_capacity(batch.num_rows());
    for row in 0..batch.num_rows() {
        let selected = span_ids.value(row);
        let selected = selected
            .as_any()
            .downcast_ref::<StringViewArray>()
            .ok_or_else(|| malformed("`span_ids` values are not string views".to_owned()))?;
        matches.push(dto::TraceMatch {
            trace_id: trace_ids.value(row).to_owned(),
            start_time_ns: starts.value(row),
            selected_span_ids: (0..selected.len())
                .map(|i| selected.value(i).to_owned())
                .collect(),
        });
    }
    Ok(dto::TraceSearch {
        matches,
        effective_start_ns: meta_i64(batch, META_EFFECTIVE_START)?,
    })
}

// ── log page ────────────────────────────────────────────────────────────────────────────────────

/// One row per log entry, with the scan counters and the paging cursor in the schema metadata.
///
/// Ids travel as hex and attribute maps as their canonical JSON — the same encoding the storage
/// layer writes them in (ARCHITECTURE.md §6.1), so this is a re-frame rather than a re-modelling.
/// The cursor is opaque (`PageCursor` has a private field), so it rides as its own JSON.
pub fn log_page_to_batch(page: &LogPage) -> Result<RecordBatch, HeadError> {
    let mut time: Vec<i64> = Vec::new();
    let mut observed: TimestampNanosecondBuilder = TimestampNanosecondBuilder::new();
    let mut severity_number: Vec<u8> = Vec::new();
    let mut severity_text = view_builder();
    let mut service = view_builder();
    let mut body = view_builder();
    let mut attributes = view_builder();
    let mut resource = view_builder();
    let mut scope = view_builder();
    let mut trace_id = view_builder();
    let mut span_id = view_builder();
    let mut flags: Vec<u32> = Vec::new();

    for entry in &page.entries {
        time.push(entry.time.0);
        match entry.observed_time {
            Some(t) => observed.append_value(t.0),
            None => observed.append_null(),
        }
        severity_number.push(entry.severity_number.0);
        append_option(&mut severity_text, entry.severity_text.as_deref());
        append_option(&mut service, entry.service.as_deref());
        body.append_value(&entry.body);
        attributes.append_value(attrs_json(&entry.attributes));
        resource.append_value(attrs_json(&entry.resource));
        scope.append_value(attrs_json(&entry.scope));
        append_option(
            &mut trace_id,
            entry.trace_id.map(|id| id.to_hex()).as_deref(),
        );
        append_option(&mut span_id, entry.span_id.map(|id| id.to_hex()).as_deref());
        flags.push(entry.flags);
    }

    let mut metadata = HashMap::from([(
        META_LOG_STATS.to_owned(),
        serde_json::to_string(&page.stats)
            .map_err(|e| internal(format!("cannot encode the log scan stats: {e}")))?,
    )]);
    if let Some(next) = &page.next {
        metadata.insert(
            META_LOG_NEXT.to_owned(),
            serde_json::to_string(next)
                .map_err(|e| internal(format!("cannot encode the log paging cursor: {e}")))?,
        );
    }

    Ok(batch_with_metadata(
        vec![
            ("time", col(TimestampNanosecondArray::from(time))),
            ("observed_time", col(observed.finish())),
            ("severity_number", col(UInt8Array::from(severity_number))),
            ("severity_text", col(severity_text.finish())),
            ("service", col(service.finish())),
            ("body", col(body.finish())),
            ("attributes", col(attributes.finish())),
            ("resource", col(resource.finish())),
            ("scope", col(scope.finish())),
            ("trace_id", col(trace_id.finish())),
            ("span_id", col(span_id.finish())),
            ("flags", col(UInt32Array::from(flags))),
        ],
        metadata,
    ))
}

pub fn log_page_from_batch(batch: &RecordBatch) -> Result<LogPage, HeadError> {
    let time = column::<TimestampNanosecondArray>(batch, "time")?;
    let observed = column::<TimestampNanosecondArray>(batch, "observed_time")?;
    let severity_number = column::<UInt8Array>(batch, "severity_number")?;
    let severity_text = column::<StringViewArray>(batch, "severity_text")?;
    let service = column::<StringViewArray>(batch, "service")?;
    let body = column::<StringViewArray>(batch, "body")?;
    let attributes = column::<StringViewArray>(batch, "attributes")?;
    let resource = column::<StringViewArray>(batch, "resource")?;
    let scope = column::<StringViewArray>(batch, "scope")?;
    let trace_id = column::<StringViewArray>(batch, "trace_id")?;
    let span_id = column::<StringViewArray>(batch, "span_id")?;
    let flags = column::<UInt32Array>(batch, "flags")?;

    let mut entries = Vec::with_capacity(batch.num_rows());
    for row in 0..batch.num_rows() {
        entries.push(LogEntry {
            time: Timestamp(time.value(row)),
            observed_time: observed
                .is_valid(row)
                .then(|| Timestamp(observed.value(row))),
            severity_number: SeverityNumber(severity_number.value(row)),
            severity_text: opt_str(severity_text, row),
            service: opt_str(service, row),
            body: body.value(row).to_owned(),
            attributes: Attributes::from_canonical_json(attributes.value(row)),
            resource: Attributes::from_canonical_json(resource.value(row)),
            scope: Attributes::from_canonical_json(scope.value(row)),
            trace_id: opt_str(trace_id, row).and_then(|hex| TraceId::from_hex(&hex)),
            span_id: opt_str(span_id, row).and_then(|hex| SpanId::from_hex(&hex)),
            flags: flags.value(row),
        });
    }
    let stats: QueryStats = serde_json::from_str(meta_str(batch, META_LOG_STATS)?)
        .map_err(|e| malformed(format!("malformed log scan stats: {e}")))?;
    let next = match batch.schema().metadata().get(META_LOG_NEXT) {
        Some(cursor) => Some(
            serde_json::from_str(cursor)
                .map_err(|e| malformed(format!("malformed log paging cursor: {e}")))?,
        ),
        None => None,
    };
    Ok(LogPage {
        entries,
        stats,
        next,
    })
}

// ── one trace ───────────────────────────────────────────────────────────────────────────────────

/// One row per span, with the assembled trace header in the schema metadata.
///
/// The header is carried rather than re-derived: `Trace`'s root/start/duration come from a private
/// assembly rule, and a head that re-ran its own version of that rule would be a second definition
/// of what a trace's root *is*. A missing trace is a zero-row batch flagged in the metadata, which is
/// how "no such trace" is told apart from "a trace with no spans".
pub fn trace_to_batch(trace: Option<&Trace>) -> RecordBatch {
    let mut trace_id = view_builder();
    let mut span_id = view_builder();
    let mut parent_span_id = view_builder();
    let mut name = view_builder();
    let mut kind = view_builder();
    let mut start_time: Vec<i64> = Vec::new();
    let mut duration_ns: Vec<u64> = Vec::new();
    let mut status_code = view_builder();
    let mut status_message = view_builder();
    let mut service = view_builder();
    let mut attributes = view_builder();
    let mut resource = view_builder();
    let mut scope = view_builder();
    let mut events = view_builder();
    let mut links = view_builder();
    let mut trace_state = view_builder();
    let mut flags: Vec<u32> = Vec::new();

    for span in trace.map(|t| t.spans.as_slice()).unwrap_or_default() {
        trace_id.append_value(span.trace_id.to_hex());
        span_id.append_value(span.span_id.to_hex());
        append_option(
            &mut parent_span_id,
            span.parent_span_id.map(|id| id.to_hex()).as_deref(),
        );
        name.append_value(&span.name);
        kind.append_value(&span.kind);
        start_time.push(span.start_time.0);
        duration_ns.push(span.duration_ns.0);
        status_code.append_value(&span.status_code);
        append_option(&mut status_message, span.status_message.as_deref());
        append_option(&mut service, span.service.as_deref());
        attributes.append_value(attrs_json(&span.attributes));
        resource.append_value(attrs_json(&span.resource));
        scope.append_value(attrs_json(&span.scope));
        append_option(&mut events, span.events.as_deref());
        append_option(&mut links, span.links.as_deref());
        append_option(&mut trace_state, span.trace_state.as_deref());
        flags.push(span.flags);
    }

    let mut metadata =
        HashMap::from([(META_TRACE_PRESENT.to_owned(), trace.is_some().to_string())]);
    if let Some(trace) = trace {
        metadata.insert(META_TRACE_ID.to_owned(), trace.trace_id.to_hex());
        metadata.insert(META_TRACE_START.to_owned(), trace.start_time.0.to_string());
        metadata.insert(
            META_TRACE_DURATION.to_owned(),
            trace.duration_ns.0.to_string(),
        );
        if let Some(service) = &trace.root_service {
            metadata.insert(META_TRACE_ROOT_SERVICE.to_owned(), service.clone());
        }
        if let Some(name) = &trace.root_name {
            metadata.insert(META_TRACE_ROOT_NAME.to_owned(), name.clone());
        }
    }

    batch_with_metadata(
        vec![
            ("trace_id", col(trace_id.finish())),
            ("span_id", col(span_id.finish())),
            ("parent_span_id", col(parent_span_id.finish())),
            ("name", col(name.finish())),
            ("kind", col(kind.finish())),
            (
                "start_time",
                col(TimestampNanosecondArray::from(start_time)),
            ),
            ("duration_ns", col(UInt64Array::from(duration_ns))),
            ("status_code", col(status_code.finish())),
            ("status_message", col(status_message.finish())),
            ("service", col(service.finish())),
            ("attributes", col(attributes.finish())),
            ("resource", col(resource.finish())),
            ("scope", col(scope.finish())),
            ("events", col(events.finish())),
            ("links", col(links.finish())),
            ("trace_state", col(trace_state.finish())),
            ("flags", col(UInt32Array::from(flags))),
        ],
        metadata,
    )
}

pub fn trace_from_batch(batch: &RecordBatch) -> Result<Option<Trace>, HeadError> {
    if meta_str(batch, META_TRACE_PRESENT)? != "true" {
        return Ok(None);
    }
    let hex = meta_str(batch, META_TRACE_ID)?;
    let trace_id = TraceId::from_hex(hex)
        .ok_or_else(|| malformed(format!("`{hex}` is not a hex trace id")))?;

    let span_trace_id = column::<StringViewArray>(batch, "trace_id")?;
    let span_id = column::<StringViewArray>(batch, "span_id")?;
    let parent_span_id = column::<StringViewArray>(batch, "parent_span_id")?;
    let name = column::<StringViewArray>(batch, "name")?;
    let kind = column::<StringViewArray>(batch, "kind")?;
    let start_time = column::<TimestampNanosecondArray>(batch, "start_time")?;
    let duration_ns = column::<UInt64Array>(batch, "duration_ns")?;
    let status_code = column::<StringViewArray>(batch, "status_code")?;
    let status_message = column::<StringViewArray>(batch, "status_message")?;
    let service = column::<StringViewArray>(batch, "service")?;
    let attributes = column::<StringViewArray>(batch, "attributes")?;
    let resource = column::<StringViewArray>(batch, "resource")?;
    let scope = column::<StringViewArray>(batch, "scope")?;
    let events = column::<StringViewArray>(batch, "events")?;
    let links = column::<StringViewArray>(batch, "links")?;
    let trace_state = column::<StringViewArray>(batch, "trace_state")?;
    let flags = column::<UInt32Array>(batch, "flags")?;

    let mut spans = Vec::with_capacity(batch.num_rows());
    for row in 0..batch.num_rows() {
        let id = |array: &StringViewArray| -> Result<SpanId, HeadError> {
            SpanId::from_hex(array.value(row))
                .ok_or_else(|| malformed(format!("`{}` is not a hex span id", array.value(row))))
        };
        spans.push(Span {
            trace_id: TraceId::from_hex(span_trace_id.value(row)).ok_or_else(|| {
                malformed(format!(
                    "`{}` is not a hex trace id",
                    span_trace_id.value(row)
                ))
            })?,
            span_id: id(span_id)?,
            parent_span_id: opt_str(parent_span_id, row).and_then(|hex| SpanId::from_hex(&hex)),
            name: name.value(row).to_owned(),
            kind: kind.value(row).to_owned(),
            start_time: Timestamp(start_time.value(row)),
            duration_ns: DurationNs(duration_ns.value(row)),
            status_code: status_code.value(row).to_owned(),
            status_message: opt_str(status_message, row),
            service: opt_str(service, row),
            attributes: Attributes::from_canonical_json(attributes.value(row)),
            resource: Attributes::from_canonical_json(resource.value(row)),
            scope: Attributes::from_canonical_json(scope.value(row)),
            events: opt_str(events, row),
            links: opt_str(links, row),
            trace_state: opt_str(trace_state, row),
            flags: flags.value(row),
        });
    }

    Ok(Some(Trace {
        trace_id,
        root_service: batch
            .schema()
            .metadata()
            .get(META_TRACE_ROOT_SERVICE)
            .cloned(),
        root_name: batch.schema().metadata().get(META_TRACE_ROOT_NAME).cloned(),
        start_time: Timestamp(meta_i64(batch, META_TRACE_START)?),
        duration_ns: DurationNs(
            meta_str(batch, META_TRACE_DURATION)?
                .parse()
                .map_err(|_| malformed("malformed trace duration".to_owned()))?,
        ),
        spans,
    }))
}

// ── builders and readers ────────────────────────────────────────────────────────────────────────

type TimestampNanosecondBuilder = imbh::arrow::array::TimestampNanosecondBuilder;

/// A string-view builder that deduplicates its bytes, so a value repeated down a column (a service
/// name, a status code, a trace id shared by every span) is stored once and viewed many times.
fn view_builder() -> StringViewBuilder {
    StringViewBuilder::new().with_deduplicate_strings()
}

fn append_option(builder: &mut StringViewBuilder, value: Option<&str>) {
    match value {
        Some(value) => builder.append_value(value),
        None => builder.append_null(),
    }
}

fn attrs_json(attributes: &Attributes) -> String {
    imbh_core::canonical_json_object(
        &attributes
            .iter()
            .map(|(key, value)| (key.to_owned(), value.clone()))
            .collect::<Vec<_>>(),
    )
}

fn col<A: Array + 'static>(array: A) -> ArrayRef {
    Arc::new(array)
}

/// Assemble a batch, deriving each column's `Field` from the array it holds so an empty result
/// carries a schema byte-for-byte identical to a populated one — which is what lets a head decode
/// the empty case through exactly the same path.
fn batch_with_metadata(
    columns: Vec<(&str, ArrayRef)>,
    metadata: HashMap<String, String>,
) -> RecordBatch {
    let fields = columns
        .iter()
        .map(|(name, array)| Field::new(*name, array.data_type().clone(), array.null_count() > 0))
        .collect::<Vec<_>>();
    let arrays = columns.into_iter().map(|(_, array)| array).collect();
    let schema: SchemaRef = Arc::new(Schema::new(fields).with_metadata(metadata));
    RecordBatch::try_new(schema, arrays)
        .expect("head result columns are length-consistent by construction")
}

fn column<'a, A: Array + 'static>(batch: &'a RecordBatch, name: &str) -> Result<&'a A, HeadError> {
    let array = batch
        .column_by_name(name)
        .ok_or_else(|| malformed(format!("the result batch has no `{name}` column")))?;
    array.as_any().downcast_ref::<A>().ok_or_else(|| {
        malformed(format!(
            "the result batch's `{name}` column has the wrong type"
        ))
    })
}

fn opt_str(array: &StringViewArray, row: usize) -> Option<String> {
    array.is_valid(row).then(|| array.value(row).to_owned())
}

/// One row of a `Map<Utf8View,Utf8View>` label column, as the head's ordered pair list.
fn map_row(array: &MapArray, row: usize) -> Result<Vec<dto::Label>, HeadError> {
    let entries = array.value(row);
    let keys = entries
        .column(0)
        .as_any()
        .downcast_ref::<StringViewArray>()
        .ok_or_else(|| malformed("label keys are not string views".to_owned()))?;
    let values = entries
        .column(1)
        .as_any()
        .downcast_ref::<StringViewArray>()
        .ok_or_else(|| malformed("label values are not string views".to_owned()))?;
    Ok((0..keys.len())
        .map(|i| dto::Label {
            name: keys.value(i).to_owned(),
            value: values.value(i).to_owned(),
        })
        .collect())
}

fn meta_str<'a>(batch: &'a RecordBatch, key: &str) -> Result<&'a str, HeadError> {
    batch
        .schema_ref()
        .metadata()
        .get(key)
        .map(String::as_str)
        .ok_or_else(|| malformed(format!("the result is missing its `{key}` metadata")))
}

fn meta_i64(batch: &RecordBatch, key: &str) -> Result<i64, HeadError> {
    meta_str(batch, key)?
        .parse()
        .map_err(|_| malformed(format!("`{key}` metadata is not a number")))
}

fn internal(message: String) -> HeadError {
    HeadError::Api {
        status: 500,
        kind: None,
        message,
    }
}

fn malformed(message: String) -> HeadError {
    HeadError::Transport(message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use imbh::{AnyValue, PageCursor};

    fn label(name: &str, value: &str) -> dto::Label {
        dto::Label {
            name: name.to_owned(),
            value: value.to_owned(),
        }
    }

    fn sample(timestamp_ns: i64, value: f64) -> dto::SamplePoint {
        dto::SamplePoint {
            timestamp_ns,
            value,
        }
    }

    /// Round-trip a value through the exact remote path — encode, frame, unframe, decode — and give
    /// back what a remote head would have seen. Every test below asserts that against the value a
    /// *local* head gets, which is the property the whole design rests on.
    fn through_ipc<T>(
        value: &T,
        to_batch: impl Fn(&T) -> RecordBatch,
        from_batch: impl Fn(&RecordBatch) -> Result<T, HeadError>,
    ) -> T {
        let bytes = encode(&to_batch(value)).expect("encode");
        from_batch(&decode(&bytes).expect("decode")).expect("read back")
    }

    /// A cursor, built the only way anything outside the facade can build one: through its own
    /// serde form. That it works here is also what the metadata encoding relies on.
    fn cursor() -> PageCursor {
        serde_json::from_str("0").expect("a page cursor is its own serde form")
    }

    fn scan_stats(rows_scanned: u64, bytes_scanned: u64, used_index: bool) -> QueryStats {
        QueryStats {
            segments_scanned: 1,
            segments_pruned: 0,
            rows_scanned,
            rows_returned: 0,
            bytes_scanned,
            elapsed: DurationNs(0),
            used_index,
        }
    }

    #[cfg(feature = "exec")]
    #[test]
    fn the_series_schema_is_the_engines_own() {
        // The head's matrix must be the same shape `imbh-lgtm`'s Arrow surface emits, or a consumer
        // would have to know which of the two produced a batch before reading it.
        assert_eq!(
            series_to_batch(&[]).schema().fields(),
            imbh_lgtm::prom_matrix_schema().fields()
        );
        assert_eq!(
            series_to_batch(&[]).schema().fields(),
            imbh_lgtm::log_matrix_schema().fields()
        );
    }

    #[test]
    fn non_finite_sample_values_survive_the_wire() {
        // The reason this wire is Arrow and not JSON: all three specials are ordinary f64 bits here.
        let series = vec![dto::Series {
            labels: vec![label("__name__", "up"), label("service", "cart")],
            samples: vec![
                sample(1, 1.5),
                sample(2, f64::NAN),
                sample(3, f64::INFINITY),
                sample(4, f64::NEG_INFINITY),
            ],
        }];
        let back = through_ipc(&series, |s| series_to_batch(s), series_from_batch);
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].labels, series[0].labels);
        let values: Vec<f64> = back[0].samples.iter().map(|s| s.value).collect();
        assert_eq!(values[0], 1.5);
        assert!(values[1].is_nan());
        assert_eq!(values[2], f64::INFINITY);
        assert_eq!(values[3], f64::NEG_INFINITY);
        // And the timestamps are exact i64, not a float that happened to round-trip.
        let timestamps: Vec<i64> = back[0].samples.iter().map(|s| s.timestamp_ns).collect();
        assert_eq!(timestamps, vec![1, 2, 3, 4]);
    }

    #[test]
    fn several_series_keep_their_grouping_and_order() {
        let series = vec![
            dto::Series {
                labels: vec![label("service", "cart")],
                samples: vec![sample(1, 1.0), sample(2, 2.0)],
            },
            dto::Series {
                labels: vec![label("service", "api")],
                samples: vec![sample(1, 3.0)],
            },
            // No labels at all is a legitimate result (an aggregation with no `by`).
            dto::Series {
                labels: Vec::new(),
                samples: vec![sample(9, 9.0)],
            },
        ];
        assert_eq!(
            through_ipc(&series, |s| series_to_batch(s), series_from_batch),
            series
        );
    }

    /// `exec::promql` concatenates one evaluation per requested query, and PromQL aggregation drops
    /// `__name__` — so two queries answering with the same label set is ordinary, not pathological.
    /// Recovering the series boundaries by grouping runs of equal labels fused them into one, which
    /// made a remote head answer differently from a local one over the same data (the catalog's
    /// multi-metric selection showed a single row).
    #[test]
    fn two_series_with_the_same_labels_stay_two_series() {
        let series = vec![
            dto::Series {
                labels: Vec::new(),
                samples: vec![sample(1, 1.0), sample(2, 2.0)],
            },
            dto::Series {
                labels: Vec::new(),
                samples: vec![sample(1, 3.0), sample(2, 4.0)],
            },
            // Adjacent, identical, *and* the same samples: nothing about the rows tells them apart.
            dto::Series {
                labels: vec![label("service", "cart")],
                samples: vec![sample(1, 5.0)],
            },
            dto::Series {
                labels: vec![label("service", "cart")],
                samples: vec![sample(1, 5.0)],
            },
        ];
        assert_eq!(
            through_ipc(&series, |s| series_to_batch(s), series_from_batch),
            series
        );
    }

    /// A batch from a daemon that predates the boundary metadata still decodes — by the run-of-equal-
    /// labels grouping, which is exact for any single-query result.
    #[test]
    fn a_batch_without_boundary_metadata_falls_back_to_label_runs() {
        let series = vec![
            dto::Series {
                labels: vec![label("service", "cart")],
                samples: vec![sample(1, 1.0), sample(2, 2.0)],
            },
            dto::Series {
                labels: vec![label("service", "api")],
                samples: vec![sample(1, 3.0)],
            },
        ];
        let batch = series_to_batch(&series);
        let stripped = RecordBatch::try_new(
            Arc::new(Schema::new(batch.schema().fields().clone())),
            batch.columns().to_vec(),
        )
        .expect("same columns, metadata-free schema");
        assert!(
            !stripped
                .schema()
                .metadata()
                .contains_key(META_SERIES_STARTS),
            "the fallback path is the one under test"
        );
        assert_eq!(series_from_batch(&stripped).expect("decode"), series);
    }

    #[test]
    fn an_empty_result_decodes_through_the_same_path() {
        // An IPC stream with no batches still carries its schema, so the empty case is not special.
        assert_eq!(
            through_ipc(&Vec::new(), |s| series_to_batch(s), series_from_batch),
            Vec::<dto::Series>::new()
        );
        let empty = dto::TraceSearch {
            matches: Vec::new(),
            effective_start_ns: 42,
        };
        assert_eq!(
            through_ipc(&empty, trace_search_to_batch, trace_search_from_batch),
            empty
        );
    }

    #[test]
    fn a_trace_search_carries_its_narrowed_window() {
        let search = dto::TraceSearch {
            matches: vec![
                dto::TraceMatch {
                    trace_id: "aa".repeat(16),
                    start_time_ns: 1_700_000_000_000_000_000,
                    selected_span_ids: vec!["bb".repeat(8), "cc".repeat(8)],
                },
                // A match whose spanset selected nothing still lists.
                dto::TraceMatch {
                    trace_id: "dd".repeat(16),
                    start_time_ns: 1_700_000_000_000_000_001,
                    selected_span_ids: Vec::new(),
                },
            ],
            effective_start_ns: 1_699_999_000_000_000_000,
        };
        assert_eq!(
            through_ipc(&search, trace_search_to_batch, trace_search_from_batch),
            search
        );
    }

    #[test]
    fn a_log_page_keeps_its_cursor_stats_and_attributes() {
        let attributes = Attributes::from_pairs(vec![
            ("http.route".to_owned(), AnyValue::Str("/cart".to_owned())),
            ("retry".to_owned(), AnyValue::Bool(true)),
            ("status".to_owned(), AnyValue::Int(500)),
        ]);
        let page = LogPage {
            entries: vec![
                LogEntry {
                    time: Timestamp(1_700_000_000_000_000_000),
                    observed_time: Some(Timestamp(1_700_000_000_000_000_001)),
                    severity_number: SeverityNumber::ERROR,
                    severity_text: Some("ERROR".to_owned()),
                    service: Some("cart".to_owned()),
                    body: "checkout failed\nretrying".to_owned(),
                    attributes: attributes.clone(),
                    resource: Attributes::new(),
                    scope: Attributes::new(),
                    trace_id: Some(TraceId([0xaa; 16])),
                    span_id: Some(SpanId([0xbb; 8])),
                    flags: 1,
                },
                // Every nullable column exercised in one row.
                LogEntry {
                    time: Timestamp(1_700_000_000_000_000_002),
                    observed_time: None,
                    severity_number: SeverityNumber(0),
                    severity_text: None,
                    service: None,
                    body: String::new(),
                    attributes: Attributes::new(),
                    resource: Attributes::new(),
                    scope: Attributes::new(),
                    trace_id: None,
                    span_id: None,
                    flags: 0,
                },
            ],
            stats: scan_stats(1234, 56_789, true),
            next: Some(cursor()),
        };
        let back = through_ipc(
            &page,
            |p| log_page_to_batch(p).expect("encode"),
            log_page_from_batch,
        );

        assert_eq!(back.entries.len(), 2);
        let first = &back.entries[0];
        assert_eq!(first.time, page.entries[0].time);
        assert_eq!(first.observed_time, page.entries[0].observed_time);
        assert_eq!(first.severity_number, SeverityNumber::ERROR);
        assert_eq!(first.service.as_deref(), Some("cart"));
        // A body with a newline in it must not be re-framed by the transport.
        assert_eq!(first.body, "checkout failed\nretrying");
        assert_eq!(first.attributes, attributes);
        assert_eq!(first.trace_id, Some(TraceId([0xaa; 16])));
        assert_eq!(first.span_id, Some(SpanId([0xbb; 8])));

        let second = &back.entries[1];
        assert_eq!(second.observed_time, None);
        assert_eq!(second.severity_text, None);
        assert_eq!(second.service, None);
        assert_eq!(second.trace_id, None);
        assert_eq!(second.span_id, None);

        // The scan counters and the opaque paging cursor ride in the schema metadata.
        assert_eq!(back.stats.rows_scanned, 1234);
        assert_eq!(back.stats.bytes_scanned, 56789);
        assert!(back.stats.used_index);
        assert!(back.next.is_some());
    }

    #[test]
    fn a_page_with_no_next_page_says_so() {
        let page = LogPage {
            entries: Vec::new(),
            stats: scan_stats(0, 0, false),
            next: None,
        };
        let back = through_ipc(
            &page,
            |p| log_page_to_batch(p).expect("encode"),
            log_page_from_batch,
        );
        assert!(back.entries.is_empty());
        assert!(back.next.is_none());
    }

    fn span(id: u8, parent: Option<u8>) -> Span {
        Span {
            trace_id: TraceId([0xaa; 16]),
            span_id: SpanId([id; 8]),
            parent_span_id: parent.map(|p| SpanId([p; 8])),
            name: format!("span-{id}"),
            kind: "SERVER".to_owned(),
            start_time: Timestamp(1_000 * i64::from(id)),
            duration_ns: DurationNs(500),
            status_code: "OK".to_owned(),
            status_message: None,
            service: Some("api".to_owned()),
            attributes: Attributes::from_pairs(vec![(
                "http.status_code".to_owned(),
                AnyValue::Int(200),
            )]),
            resource: Attributes::new(),
            scope: Attributes::new(),
            events: Some(r#"[{"name":"retry"}]"#.to_owned()),
            links: None,
            trace_state: None,
            flags: 0,
        }
    }

    #[test]
    fn a_trace_keeps_its_assembled_header_and_span_tree() {
        let trace = Trace {
            trace_id: TraceId([0xaa; 16]),
            root_service: Some("api".to_owned()),
            root_name: Some("GET /cart".to_owned()),
            start_time: Timestamp(1_000),
            duration_ns: DurationNs(10_000),
            spans: vec![span(1, None), span(2, Some(1)), span(3, Some(2))],
        };
        let back = through_ipc(
            &Some(trace.clone()),
            |t| trace_to_batch(t.as_ref()),
            trace_from_batch,
        )
        .expect("present");

        // The header is carried, not re-derived: a head must not hold a second opinion about which
        // span is the root.
        assert_eq!(back.trace_id, trace.trace_id);
        assert_eq!(back.root_service, trace.root_service);
        assert_eq!(back.root_name, trace.root_name);
        assert_eq!(back.start_time, trace.start_time);
        assert_eq!(back.duration_ns, trace.duration_ns);

        assert_eq!(back.spans.len(), 3);
        assert_eq!(back.spans[0].parent_span_id, None);
        assert_eq!(back.spans[1].parent_span_id, Some(SpanId([1; 8])));
        assert_eq!(back.spans[2].span_id, SpanId([3; 8]));
        assert_eq!(back.spans[0].attributes, trace.spans[0].attributes);
        assert_eq!(back.spans[0].events, trace.spans[0].events);
        assert_eq!(back.spans[0].links, None);
    }

    #[test]
    fn a_missing_trace_is_not_a_trace_with_no_spans() {
        let absent = through_ipc(
            &None,
            |t: &Option<Trace>| trace_to_batch(t.as_ref()),
            trace_from_batch,
        );
        assert!(absent.is_none());

        let empty = Trace {
            trace_id: TraceId([0xaa; 16]),
            root_service: None,
            root_name: None,
            start_time: Timestamp(0),
            duration_ns: DurationNs(0),
            spans: Vec::new(),
        };
        let present = through_ipc(
            &Some(empty),
            |t: &Option<Trace>| trace_to_batch(t.as_ref()),
            trace_from_batch,
        );
        let present = present.expect("a trace that exists but has no spans is still a trace");
        assert!(present.spans.is_empty());
        assert_eq!(present.root_service, None);
    }

    #[test]
    fn a_body_that_is_not_an_ipc_stream_is_a_transport_failure() {
        // A proxy's error page, or an older imbhd answering something else entirely.
        let e = decode(b"<html>not arrow</html>").expect_err("not a stream");
        assert!(matches!(e, HeadError::Transport(_)), "{e:?}");
    }
}

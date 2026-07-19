//! OTLP decode → normalized rows (ARCHITECTURE.md §12, ingest half of §5).
//!
//! This crate owns the OTLP wire types (prost messages, no tonic services) and the
//! normalization step: `ExportLogsServiceRequest` → `Vec<LogRow>`, and the equivalent for
//! traces and metrics. Attribute scopes are encoded to canonical JSON here via the one shared
//! encoder in `imbh-core`, so the bytes that reach storage are already dict-ready
//! (ARCHITECTURE.md §6.1).

use imbh_core::{
    AnyValue, Error, ExpHistogramRow, HistogramRow, LogRow, Result, ScalarMetricRow, Signal,
    SpanRow, SummaryRow, Table, canonical_json_object, canonical_json_value,
};

use opentelemetry_proto::tonic::collector::logs::v1::ExportLogsServiceRequest;
use opentelemetry_proto::tonic::collector::metrics::v1::ExportMetricsServiceRequest;
use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
use opentelemetry_proto::tonic::common::v1::{
    AnyValue as PbAnyValue, InstrumentationScope, KeyValue, any_value,
};
use opentelemetry_proto::tonic::metrics::v1::{
    Exemplar, ExponentialHistogramDataPoint, HistogramDataPoint, Metric, NumberDataPoint,
    SummaryDataPoint, exemplar, metric, number_data_point,
};
use opentelemetry_proto::tonic::trace::v1::span::{Event, Link};
use prost::Message;

/// Decode a protobuf OTLP/logs export request (uncompressed body).
pub fn decode_logs_request(bytes: &[u8]) -> Result<ExportLogsServiceRequest> {
    ExportLogsServiceRequest::decode(bytes).map_err(|e| Error::ingest_decode(Signal::Logs, e))
}

/// Decode and normalize an OTLP/logs export request into rows in one step.
#[cfg_attr(
    feature = "tracing",
    tracing::instrument(level = "debug", name = "otlp.decode_logs", skip_all, fields(bytes = bytes.len()))
)]
pub fn decode_logs_to_rows(bytes: &[u8]) -> Result<Vec<LogRow>> {
    let rows = logs_request_to_rows(&decode_logs_request(bytes)?);
    #[cfg(feature = "tracing")]
    tracing::debug!(rows = rows.len(), "decoded OTLP/logs");
    Ok(rows)
}

/// Normalize an already-decoded OTLP/logs request into [`LogRow`]s (the self-observation
/// path that skips protobuf decode).
pub fn logs_request_to_rows(req: &ExportLogsServiceRequest) -> Vec<LogRow> {
    let mut rows = Vec::new();
    for rl in &req.resource_logs {
        let resource_pairs = rl
            .resource
            .as_ref()
            .map(|r| kvs_to_pairs(&r.attributes))
            .unwrap_or_default();
        let service = service_name(&resource_pairs);
        let resource_json = canonical_json_object(&resource_pairs);

        for sl in &rl.scope_logs {
            let scope_json = scope_to_json(sl.scope.as_ref());
            for rec in &sl.log_records {
                rows.push(LogRow {
                    time_unix_nano: rec.time_unix_nano as i64,
                    observed_time_unix_nano: nonzero(rec.observed_time_unix_nano),
                    service: service.clone(),
                    severity_number: rec.severity_number.clamp(0, 24) as u8,
                    severity_text: nonempty(&rec.severity_text),
                    body: body_to_string(rec.body.as_ref()),
                    attributes: canonical_json_object(&kvs_to_pairs(&rec.attributes)),
                    resource: resource_json.clone(),
                    scope: scope_json.clone(),
                    trace_id: <[u8; 16]>::try_from(rec.trace_id.as_slice()).ok(),
                    span_id: <[u8; 8]>::try_from(rec.span_id.as_slice()).ok(),
                    flags: rec.flags,
                });
            }
        }
    }
    rows
}

/// Decode a protobuf OTLP/traces export request.
pub fn decode_traces_request(bytes: &[u8]) -> Result<ExportTraceServiceRequest> {
    ExportTraceServiceRequest::decode(bytes).map_err(|e| Error::ingest_decode(Signal::Traces, e))
}

/// Decode and normalize an OTLP/traces export request into span rows in one step.
#[cfg_attr(
    feature = "tracing",
    tracing::instrument(level = "debug", name = "otlp.decode_traces", skip_all, fields(bytes = bytes.len()))
)]
pub fn decode_traces_to_rows(bytes: &[u8]) -> Result<Vec<SpanRow>> {
    let rows = traces_request_to_rows(&decode_traces_request(bytes)?);
    #[cfg(feature = "tracing")]
    tracing::debug!(rows = rows.len(), "decoded OTLP/traces");
    Ok(rows)
}

/// Normalize an already-decoded OTLP/traces request into [`SpanRow`]s (ARCHITECTURE.md §6.3).
pub fn traces_request_to_rows(req: &ExportTraceServiceRequest) -> Vec<SpanRow> {
    let mut rows = Vec::new();
    for rs in &req.resource_spans {
        let resource_pairs = rs
            .resource
            .as_ref()
            .map(|r| kvs_to_pairs(&r.attributes))
            .unwrap_or_default();
        let service = service_name(&resource_pairs);
        let resource_json = canonical_json_object(&resource_pairs);

        for ss in &rs.scope_spans {
            let scope_json = scope_to_json(ss.scope.as_ref());
            for sp in &ss.spans {
                let duration_ns = sp
                    .end_time_unix_nano
                    .saturating_sub(sp.start_time_unix_nano);
                let (status_code, status_message) = match &sp.status {
                    Some(s) => (status_code_str(s.code).to_owned(), nonempty(&s.message)),
                    None => ("UNSET".to_owned(), None),
                };
                rows.push(SpanRow {
                    trace_id: fixed16(&sp.trace_id),
                    span_id: fixed8(&sp.span_id),
                    parent_span_id: <[u8; 8]>::try_from(sp.parent_span_id.as_slice()).ok(),
                    name: sp.name.clone(),
                    kind: span_kind_str(sp.kind).to_owned(),
                    start_time_unix_nano: sp.start_time_unix_nano as i64,
                    duration_ns,
                    status_code,
                    status_message,
                    service: service.clone(),
                    attributes: canonical_json_object(&kvs_to_pairs(&sp.attributes)),
                    resource: resource_json.clone(),
                    scope: scope_json.clone(),
                    events: events_json(&sp.events),
                    links: links_json(&sp.links),
                    trace_state: nonempty(&sp.trace_state),
                    flags: sp.flags,
                });
            }
        }
    }
    rows
}

fn span_kind_str(k: i32) -> &'static str {
    match k {
        1 => "INTERNAL",
        2 => "SERVER",
        3 => "CLIENT",
        4 => "PRODUCER",
        5 => "CONSUMER",
        _ => "UNSPECIFIED",
    }
}

fn status_code_str(c: i32) -> &'static str {
    match c {
        1 => "OK",
        2 => "ERROR",
        _ => "UNSET",
    }
}

fn events_json(events: &[Event]) -> Option<String> {
    if events.is_empty() {
        return None;
    }
    let arr = AnyValue::Array(
        events
            .iter()
            .map(|e| {
                let mut pairs: Vec<(String, AnyValue)> = Vec::new();
                if e.time_unix_nano != 0 {
                    pairs.push(("time".to_owned(), AnyValue::Int(e.time_unix_nano as i64)));
                }
                if !e.name.is_empty() {
                    pairs.push(("name".to_owned(), AnyValue::Str(e.name.clone())));
                }
                if !e.attributes.is_empty() {
                    pairs.push((
                        "attributes".to_owned(),
                        AnyValue::Map(kvs_to_pairs(&e.attributes)),
                    ));
                }
                AnyValue::Map(pairs)
            })
            .collect(),
    );
    Some(canonical_json_value(&arr))
}

fn links_json(links: &[Link]) -> Option<String> {
    if links.is_empty() {
        return None;
    }
    let arr = AnyValue::Array(
        links
            .iter()
            .map(|l| {
                let mut pairs: Vec<(String, AnyValue)> = Vec::new();
                if !l.trace_id.is_empty() {
                    pairs.push(("trace_id".to_owned(), AnyValue::Str(hex_lower(&l.trace_id))));
                }
                if !l.span_id.is_empty() {
                    pairs.push(("span_id".to_owned(), AnyValue::Str(hex_lower(&l.span_id))));
                }
                if !l.trace_state.is_empty() {
                    pairs.push((
                        "trace_state".to_owned(),
                        AnyValue::Str(l.trace_state.clone()),
                    ));
                }
                if !l.attributes.is_empty() {
                    pairs.push((
                        "attributes".to_owned(),
                        AnyValue::Map(kvs_to_pairs(&l.attributes)),
                    ));
                }
                AnyValue::Map(pairs)
            })
            .collect(),
    );
    Some(canonical_json_value(&arr))
}

fn fixed16(b: &[u8]) -> [u8; 16] {
    <[u8; 16]>::try_from(b).unwrap_or([0; 16])
}

fn fixed8(b: &[u8]) -> [u8; 8] {
    <[u8; 8]>::try_from(b).unwrap_or([0; 8])
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

/// Decode a protobuf OTLP/metrics export request.
pub fn decode_metrics_request(bytes: &[u8]) -> Result<ExportMetricsServiceRequest> {
    ExportMetricsServiceRequest::decode(bytes).map_err(|e| Error::ingest_decode(Signal::Metrics, e))
}

/// Decode and normalize an OTLP/metrics export request into scalar rows in one step.
#[cfg_attr(
    feature = "tracing",
    tracing::instrument(level = "debug", name = "otlp.decode_metrics", skip_all, fields(bytes = bytes.len()))
)]
pub fn decode_metrics_to_rows(bytes: &[u8]) -> Result<Vec<ScalarMetricRow>> {
    let rows = metrics_request_to_rows(&decode_metrics_request(bytes)?);
    #[cfg(feature = "tracing")]
    tracing::debug!(rows = rows.len(), "decoded OTLP/metrics (scalar)");
    Ok(rows)
}

/// Decode and normalize an OTLP/metrics export request into explicit-bucket histogram rows.
pub fn decode_metrics_to_histogram_rows(bytes: &[u8]) -> Result<Vec<HistogramRow>> {
    Ok(metrics_request_to_histogram_rows(&decode_metrics_request(
        bytes,
    )?))
}

/// Decode and normalize an OTLP/metrics export request into exponential-histogram rows.
pub fn decode_metrics_to_exp_histogram_rows(bytes: &[u8]) -> Result<Vec<ExpHistogramRow>> {
    Ok(metrics_request_to_exp_histogram_rows(
        &decode_metrics_request(bytes)?,
    ))
}

/// Decode and normalize an OTLP/metrics export request into summary rows.
pub fn decode_metrics_to_summary_rows(bytes: &[u8]) -> Result<Vec<SummaryRow>> {
    Ok(metrics_request_to_summary_rows(&decode_metrics_request(
        bytes,
    )?))
}

/// Normalize an already-decoded OTLP/metrics request into [`ScalarMetricRow`]s for the gauge and
/// sum tables (ARCHITECTURE.md §6.4). Histogram/exp-histogram/summary points are skipped for now (M3c).
/// Delta→cumulative normalization is stateful and happens at ingest in `imbh-storage` (§6.4).
pub fn metrics_request_to_rows(req: &ExportMetricsServiceRequest) -> Vec<ScalarMetricRow> {
    let mut rows = Vec::new();
    for rm in &req.resource_metrics {
        let resource_pairs = rm
            .resource
            .as_ref()
            .map(|r| kvs_to_pairs(&r.attributes))
            .unwrap_or_default();
        let service = service_name(&resource_pairs);
        let resource_json = canonical_json_object(&resource_pairs);

        for sm in &rm.scope_metrics {
            let scope_json = scope_to_json(sm.scope.as_ref());
            for m in &sm.metrics {
                match &m.data {
                    Some(metric::Data::Gauge(g)) => {
                        for dp in &g.data_points {
                            rows.push(scalar_row(
                                Table::MetricsGauge,
                                m,
                                dp,
                                &service,
                                &resource_json,
                                &scope_json,
                                None,
                                None,
                            ));
                        }
                    }
                    Some(metric::Data::Sum(s)) => {
                        let temporality =
                            Some(temporality_str(s.aggregation_temporality).to_owned());
                        for dp in &s.data_points {
                            rows.push(scalar_row(
                                Table::MetricsSum,
                                m,
                                dp,
                                &service,
                                &resource_json,
                                &scope_json,
                                temporality.clone(),
                                Some(s.is_monotonic),
                            ));
                        }
                    }
                    // Histogram / ExponentialHistogram / Summary → M3c.
                    _ => {}
                }
            }
        }
    }
    rows
}

#[allow(clippy::too_many_arguments)]
fn scalar_row(
    table: Table,
    m: &Metric,
    dp: &NumberDataPoint,
    service: &Option<String>,
    resource_json: &str,
    scope_json: &str,
    temporality: Option<String>,
    is_monotonic: Option<bool>,
) -> ScalarMetricRow {
    ScalarMetricRow {
        table,
        time_unix_nano: dp.time_unix_nano as i64,
        start_time_unix_nano: nonzero(dp.start_time_unix_nano),
        metric: m.name.clone(),
        unit: m.unit.clone(),
        service: service.clone(),
        attributes: canonical_json_object(&kvs_to_pairs(&dp.attributes)),
        resource: resource_json.to_owned(),
        scope: scope_json.to_owned(),
        flags: dp.flags,
        value: number_value(dp),
        temporality,
        is_monotonic,
        exemplars: exemplars_json(&dp.exemplars),
    }
}

/// Encode a data point's OTLP exemplars as a canonical-JSON array (`"[]"` when none, so the column
/// is always valid JSON for external consumers). Each entry carries the trace link for metric→trace
/// drill-down (ARCHITECTURE.md §6.4): `{"time_unix_nano","value","trace_id","span_id"}`, plus a nested
/// `"attributes"` object when the exemplar has `filtered_attributes`. Non-finite values encode as
/// JSON `null` (NaN/Inf are not valid JSON numbers).
fn exemplars_json(exemplars: &[Exemplar]) -> String {
    if exemplars.is_empty() {
        return "[]".to_owned();
    }
    use std::fmt::Write as _;
    let mut s = String::from("[");
    for (i, e) in exemplars.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        let value = match &e.value {
            Some(exemplar::Value::AsDouble(d)) => *d,
            Some(exemplar::Value::AsInt(v)) => *v as f64,
            None => 0.0,
        };
        let value_str = if value.is_finite() {
            format!("{value}")
        } else {
            "null".to_owned()
        };
        // The exemplar's `filtered_attributes` (the sampling context) — included only when present,
        // as a nested canonical-JSON object, so the common no-attrs case stays compact.
        let attrs = if e.filtered_attributes.is_empty() {
            String::new()
        } else {
            format!(
                ",\"attributes\":{}",
                canonical_json_object(&kvs_to_pairs(&e.filtered_attributes))
            )
        };
        let _ = write!(
            s,
            "{{\"time_unix_nano\":{},\"value\":{},\"trace_id\":\"{}\",\"span_id\":\"{}\"{}}}",
            e.time_unix_nano,
            value_str,
            // Normalize to the canonical 16/8-byte id length (like span ingest) so the hex is always
            // 32/16 chars and `TraceId::from_hex`/`SpanId::from_hex` round-trip it.
            hex_lower(&fixed16(&e.trace_id)),
            hex_lower(&fixed8(&e.span_id)),
            attrs,
        );
    }
    s.push(']');
    s
}

fn number_value(dp: &NumberDataPoint) -> f64 {
    match &dp.value {
        Some(number_data_point::Value::AsDouble(d)) => *d,
        Some(number_data_point::Value::AsInt(i)) => *i as f64,
        None => 0.0,
    }
}

/// Normalize an already-decoded OTLP/metrics request into [`HistogramRow`]s for the
/// `metrics_histogram` table (ARCHITECTURE.md §6.4). Only explicit-bucket `Histogram` points are handled;
/// exponential histograms and summaries are separate follow-ups. Delta→cumulative normalization is
/// stateful and happens at ingest in `imbh-storage` (§6.4), so `temporality` is carried through.
pub fn metrics_request_to_histogram_rows(req: &ExportMetricsServiceRequest) -> Vec<HistogramRow> {
    let mut rows = Vec::new();
    for rm in &req.resource_metrics {
        let resource_pairs = rm
            .resource
            .as_ref()
            .map(|r| kvs_to_pairs(&r.attributes))
            .unwrap_or_default();
        let service = service_name(&resource_pairs);
        let resource_json = canonical_json_object(&resource_pairs);

        for sm in &rm.scope_metrics {
            let scope_json = scope_to_json(sm.scope.as_ref());
            for m in &sm.metrics {
                if let Some(metric::Data::Histogram(h)) = &m.data {
                    let temporality = Some(temporality_str(h.aggregation_temporality).to_owned());
                    for dp in &h.data_points {
                        rows.push(histogram_row(
                            m,
                            dp,
                            &service,
                            &resource_json,
                            &scope_json,
                            temporality.clone(),
                        ));
                    }
                }
            }
        }
    }
    rows
}

fn histogram_row(
    m: &Metric,
    dp: &HistogramDataPoint,
    service: &Option<String>,
    resource_json: &str,
    scope_json: &str,
    temporality: Option<String>,
) -> HistogramRow {
    HistogramRow {
        time_unix_nano: dp.time_unix_nano as i64,
        start_time_unix_nano: nonzero(dp.start_time_unix_nano),
        metric: m.name.clone(),
        unit: m.unit.clone(),
        service: service.clone(),
        attributes: canonical_json_object(&kvs_to_pairs(&dp.attributes)),
        resource: resource_json.to_owned(),
        scope: scope_json.to_owned(),
        flags: dp.flags,
        count: dp.count,
        sum: dp.sum,
        min: dp.min,
        max: dp.max,
        explicit_bounds: dp.explicit_bounds.clone(),
        bucket_counts: dp.bucket_counts.clone(),
        temporality,
        exemplars: exemplars_json(&dp.exemplars),
    }
}

/// Normalize an already-decoded OTLP/metrics request into [`ExpHistogramRow`]s for the
/// `metrics_exp_histogram` table (ARCHITECTURE.md §6.4). Handles `ExponentialHistogram` points only.
pub fn metrics_request_to_exp_histogram_rows(
    req: &ExportMetricsServiceRequest,
) -> Vec<ExpHistogramRow> {
    let mut rows = Vec::new();
    for rm in &req.resource_metrics {
        let resource_pairs = rm
            .resource
            .as_ref()
            .map(|r| kvs_to_pairs(&r.attributes))
            .unwrap_or_default();
        let service = service_name(&resource_pairs);
        let resource_json = canonical_json_object(&resource_pairs);

        for sm in &rm.scope_metrics {
            let scope_json = scope_to_json(sm.scope.as_ref());
            for m in &sm.metrics {
                if let Some(metric::Data::ExponentialHistogram(h)) = &m.data {
                    let temporality = Some(temporality_str(h.aggregation_temporality).to_owned());
                    for dp in &h.data_points {
                        rows.push(exp_histogram_row(
                            m,
                            dp,
                            &service,
                            &resource_json,
                            &scope_json,
                            temporality.clone(),
                        ));
                    }
                }
            }
        }
    }
    rows
}

fn exp_histogram_row(
    m: &Metric,
    dp: &ExponentialHistogramDataPoint,
    service: &Option<String>,
    resource_json: &str,
    scope_json: &str,
    temporality: Option<String>,
) -> ExpHistogramRow {
    let (positive_offset, positive_counts) = dp
        .positive
        .as_ref()
        .map(|b| (b.offset, b.bucket_counts.clone()))
        .unwrap_or((0, Vec::new()));
    let (negative_offset, negative_counts) = dp
        .negative
        .as_ref()
        .map(|b| (b.offset, b.bucket_counts.clone()))
        .unwrap_or((0, Vec::new()));
    ExpHistogramRow {
        time_unix_nano: dp.time_unix_nano as i64,
        start_time_unix_nano: nonzero(dp.start_time_unix_nano),
        metric: m.name.clone(),
        unit: m.unit.clone(),
        service: service.clone(),
        attributes: canonical_json_object(&kvs_to_pairs(&dp.attributes)),
        resource: resource_json.to_owned(),
        scope: scope_json.to_owned(),
        flags: dp.flags,
        count: dp.count,
        sum: dp.sum,
        min: dp.min,
        max: dp.max,
        scale: dp.scale,
        zero_count: dp.zero_count,
        zero_threshold: dp.zero_threshold,
        positive_offset,
        positive_counts,
        negative_offset,
        negative_counts,
        temporality,
        exemplars: exemplars_json(&dp.exemplars),
    }
}

/// Normalize an already-decoded OTLP/metrics request into [`SummaryRow`]s for the `metrics_summary`
/// table (ARCHITECTURE.md §6.4). Summaries carry precomputed quantiles and have no temporality.
pub fn metrics_request_to_summary_rows(req: &ExportMetricsServiceRequest) -> Vec<SummaryRow> {
    let mut rows = Vec::new();
    for rm in &req.resource_metrics {
        let resource_pairs = rm
            .resource
            .as_ref()
            .map(|r| kvs_to_pairs(&r.attributes))
            .unwrap_or_default();
        let service = service_name(&resource_pairs);
        let resource_json = canonical_json_object(&resource_pairs);

        for sm in &rm.scope_metrics {
            let scope_json = scope_to_json(sm.scope.as_ref());
            for m in &sm.metrics {
                if let Some(metric::Data::Summary(s)) = &m.data {
                    for dp in &s.data_points {
                        rows.push(summary_row(m, dp, &service, &resource_json, &scope_json));
                    }
                }
            }
        }
    }
    rows
}

fn summary_row(
    m: &Metric,
    dp: &SummaryDataPoint,
    service: &Option<String>,
    resource_json: &str,
    scope_json: &str,
) -> SummaryRow {
    let quantiles = dp.quantile_values.iter().map(|q| q.quantile).collect();
    let values = dp.quantile_values.iter().map(|q| q.value).collect();
    SummaryRow {
        time_unix_nano: dp.time_unix_nano as i64,
        start_time_unix_nano: nonzero(dp.start_time_unix_nano),
        metric: m.name.clone(),
        unit: m.unit.clone(),
        service: service.clone(),
        attributes: canonical_json_object(&kvs_to_pairs(&dp.attributes)),
        resource: resource_json.to_owned(),
        scope: scope_json.to_owned(),
        flags: dp.flags,
        count: dp.count,
        sum: dp.sum,
        quantiles,
        values,
    }
}

fn temporality_str(t: i32) -> &'static str {
    match t {
        1 => "DELTA",
        2 => "CUMULATIVE",
        _ => "UNSPECIFIED",
    }
}

/// A log body: raw text for a simple string body, canonical JSON for a structured body,
/// empty string when absent (ARCHITECTURE.md §6.2).
fn body_to_string(body: Option<&PbAnyValue>) -> String {
    match body.and_then(|b| b.value.as_ref()) {
        None => String::new(),
        Some(any_value::Value::StringValue(s)) => s.clone(),
        Some(_) => canonical_json_value(&pb_to_any(body.unwrap())),
    }
}

/// The scope column: `{name, version, attributes}` as one canonical JSON object.
fn scope_to_json(scope: Option<&InstrumentationScope>) -> String {
    let Some(s) = scope else {
        return "{}".to_owned();
    };
    let mut pairs: Vec<(String, AnyValue)> = Vec::new();
    if !s.name.is_empty() {
        pairs.push(("name".to_owned(), AnyValue::Str(s.name.clone())));
    }
    if !s.version.is_empty() {
        pairs.push(("version".to_owned(), AnyValue::Str(s.version.clone())));
    }
    if !s.attributes.is_empty() {
        pairs.push((
            "attributes".to_owned(),
            AnyValue::Map(kvs_to_pairs(&s.attributes)),
        ));
    }
    canonical_json_object(&pairs)
}

fn service_name(resource_pairs: &[(String, AnyValue)]) -> Option<String> {
    resource_pairs
        .iter()
        .find(|(k, _)| k == "service.name")
        .and_then(|(_, v)| v.as_str().map(str::to_owned))
}

fn kvs_to_pairs(kvs: &[KeyValue]) -> Vec<(String, AnyValue)> {
    kvs.iter()
        .map(|kv| {
            let v = kv.value.as_ref().map(pb_to_any).unwrap_or(AnyValue::Null);
            (kv.key.clone(), v)
        })
        .collect()
}

fn pb_to_any(v: &PbAnyValue) -> AnyValue {
    match &v.value {
        None => AnyValue::Null,
        Some(any_value::Value::StringValue(s)) => AnyValue::Str(s.clone()),
        Some(any_value::Value::BoolValue(b)) => AnyValue::Bool(*b),
        Some(any_value::Value::IntValue(i)) => AnyValue::Int(*i),
        Some(any_value::Value::DoubleValue(d)) => AnyValue::Double(*d),
        Some(any_value::Value::BytesValue(b)) => AnyValue::Bytes(b.clone()),
        Some(any_value::Value::ArrayValue(a)) => {
            AnyValue::Array(a.values.iter().map(pb_to_any).collect())
        }
        Some(any_value::Value::KvlistValue(kv)) => AnyValue::Map(kvs_to_pairs(&kv.values)),
        // Experimental OTLP string-table index — unresolvable without the referenced
        // table, which imbh does not carry in M0. Treated as absent.
        Some(any_value::Value::StringValueStrindex(_)) => AnyValue::Null,
    }
}

fn nonempty(s: &str) -> Option<String> {
    (!s.is_empty()).then(|| s.to_owned())
}

fn nonzero(v: u64) -> Option<i64> {
    (v != 0).then_some(v as i64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use opentelemetry_proto::tonic::common::v1::{AnyValue as PbAny, KeyValue};
    use opentelemetry_proto::tonic::logs::v1::{LogRecord, ResourceLogs, ScopeLogs};
    use opentelemetry_proto::tonic::resource::v1::Resource;

    fn str_val(s: &str) -> PbAny {
        PbAny {
            value: Some(any_value::Value::StringValue(s.to_owned())),
        }
    }

    fn kv(key: &str, val: PbAny) -> KeyValue {
        KeyValue {
            key: key.to_owned(),
            value: Some(val),
            ..Default::default()
        }
    }

    fn sample_request() -> ExportLogsServiceRequest {
        ExportLogsServiceRequest {
            resource_logs: vec![ResourceLogs {
                resource: Some(Resource {
                    attributes: vec![kv("service.name", str_val("checkout"))],
                    ..Default::default()
                }),
                scope_logs: vec![ScopeLogs {
                    log_records: vec![
                        LogRecord {
                            time_unix_nano: 1,
                            severity_number: 9,
                            severity_text: "INFO".to_owned(),
                            body: Some(str_val("request ok")),
                            attributes: vec![kv("http.route", str_val("/cart"))],
                            ..Default::default()
                        },
                        LogRecord {
                            time_unix_nano: 2,
                            severity_number: 17,
                            body: Some(str_val("connection error timeout")),
                            ..Default::default()
                        },
                    ],
                    ..Default::default()
                }],
                ..Default::default()
            }],
        }
    }

    #[test]
    fn normalizes_records_and_service() {
        let rows = logs_request_to_rows(&sample_request());
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].service.as_deref(), Some("checkout"));
        assert_eq!(rows[0].severity_text.as_deref(), Some("INFO"));
        assert_eq!(rows[0].body, "request ok");
        assert_eq!(rows[0].attributes, r#"{"http.route":"/cart"}"#);
        assert_eq!(rows[0].resource, r#"{"service.name":"checkout"}"#);
        assert_eq!(rows[1].severity_number, 17);
        assert_eq!(rows[1].severity_text, None);
    }

    #[test]
    fn protobuf_round_trip() {
        let bytes = sample_request().encode_to_vec();
        let rows = decode_logs_to_rows(&bytes).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].body, "connection error timeout");
    }

    #[test]
    fn normalizes_spans() {
        use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
        use opentelemetry_proto::tonic::resource::v1::Resource;
        use opentelemetry_proto::tonic::trace::v1::span::Event;
        use opentelemetry_proto::tonic::trace::v1::{ResourceSpans, ScopeSpans, Span, Status};

        let req = ExportTraceServiceRequest {
            resource_spans: vec![ResourceSpans {
                resource: Some(Resource {
                    attributes: vec![kv("service.name", str_val("checkout"))],
                    ..Default::default()
                }),
                scope_spans: vec![ScopeSpans {
                    spans: vec![Span {
                        trace_id: vec![0xab; 16],
                        span_id: vec![0x01; 8],
                        name: "GET /cart".to_owned(),
                        kind: 2, // SERVER
                        start_time_unix_nano: 1000,
                        end_time_unix_nano: 1500,
                        status: Some(Status {
                            code: 2,
                            message: "boom".to_owned(),
                        }),
                        attributes: vec![kv("http.route", str_val("/cart"))],
                        events: vec![Event {
                            time_unix_nano: 1100,
                            name: "exception".to_owned(),
                            ..Default::default()
                        }],
                        ..Default::default()
                    }],
                    ..Default::default()
                }],
                ..Default::default()
            }],
        };

        let rows = traces_request_to_rows(&req);
        assert_eq!(rows.len(), 1);
        let s = &rows[0];
        assert_eq!(s.name, "GET /cart");
        assert_eq!(s.kind, "SERVER");
        assert_eq!(s.service.as_deref(), Some("checkout"));
        assert_eq!(s.start_time_unix_nano, 1000);
        assert_eq!(s.duration_ns, 500);
        assert_eq!(s.status_code, "ERROR");
        assert_eq!(s.status_message.as_deref(), Some("boom"));
        assert_eq!(s.attributes, r#"{"http.route":"/cart"}"#);
        assert_eq!(s.trace_id, [0xab; 16]);
        assert!(s.events.as_ref().unwrap().contains("exception"));

        // protobuf round-trip.
        let bytes = req.encode_to_vec();
        let back = decode_traces_to_rows(&bytes).unwrap();
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].kind, "SERVER");
    }

    #[test]
    fn normalizes_metrics() {
        use opentelemetry_proto::tonic::collector::metrics::v1::ExportMetricsServiceRequest;
        use opentelemetry_proto::tonic::metrics::v1::{
            Gauge, Metric, NumberDataPoint, ResourceMetrics, ScopeMetrics, Sum, metric,
            number_data_point,
        };
        use opentelemetry_proto::tonic::resource::v1::Resource;

        let dp = |v: f64, t: u64| NumberDataPoint {
            time_unix_nano: t,
            value: Some(number_data_point::Value::AsDouble(v)),
            attributes: vec![kv("host", str_val("h1"))],
            ..Default::default()
        };
        let req = ExportMetricsServiceRequest {
            resource_metrics: vec![ResourceMetrics {
                resource: Some(Resource {
                    attributes: vec![kv("service.name", str_val("cart"))],
                    ..Default::default()
                }),
                scope_metrics: vec![ScopeMetrics {
                    metrics: vec![
                        Metric {
                            name: "cpu".to_owned(),
                            unit: "1".to_owned(),
                            data: Some(metric::Data::Gauge(Gauge {
                                data_points: vec![dp(0.5, 100)],
                            })),
                            ..Default::default()
                        },
                        Metric {
                            name: "requests".to_owned(),
                            unit: "1".to_owned(),
                            data: Some(metric::Data::Sum(Sum {
                                data_points: vec![dp(42.0, 200)],
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
        };

        let rows = metrics_request_to_rows(&req);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].table, Table::MetricsGauge);
        assert_eq!(rows[0].metric, "cpu");
        assert_eq!(rows[0].value, 0.5);
        assert_eq!(rows[0].service.as_deref(), Some("cart"));
        assert_eq!(rows[0].attributes, r#"{"host":"h1"}"#);
        assert_eq!(rows[1].table, Table::MetricsSum);
        assert_eq!(rows[1].value, 42.0);
        assert_eq!(rows[1].temporality.as_deref(), Some("CUMULATIVE"));
        assert_eq!(rows[1].is_monotonic, Some(true));

        let bytes = req.encode_to_vec();
        assert_eq!(decode_metrics_to_rows(&bytes).unwrap().len(), 2);
    }

    #[test]
    fn normalizes_histograms() {
        use opentelemetry_proto::tonic::collector::metrics::v1::ExportMetricsServiceRequest;
        use opentelemetry_proto::tonic::metrics::v1::{
            Histogram, HistogramDataPoint, Metric, ResourceMetrics, ScopeMetrics, metric,
        };
        use opentelemetry_proto::tonic::resource::v1::Resource;

        let dp = HistogramDataPoint {
            time_unix_nano: 100,
            start_time_unix_nano: 50,
            count: 7,
            sum: Some(12.5),
            min: Some(0.1),
            max: Some(9.0),
            explicit_bounds: vec![1.0, 5.0],
            bucket_counts: vec![2, 3, 2], // N+1 for N=2 bounds
            attributes: vec![kv("http.route", str_val("/cart"))],
            ..Default::default()
        };
        let req = ExportMetricsServiceRequest {
            resource_metrics: vec![ResourceMetrics {
                resource: Some(Resource {
                    attributes: vec![kv("service.name", str_val("cart"))],
                    ..Default::default()
                }),
                scope_metrics: vec![ScopeMetrics {
                    metrics: vec![Metric {
                        name: "http.server.duration".to_owned(),
                        unit: "ms".to_owned(),
                        data: Some(metric::Data::Histogram(Histogram {
                            data_points: vec![dp],
                            aggregation_temporality: 2, // CUMULATIVE
                        })),
                        ..Default::default()
                    }],
                    ..Default::default()
                }],
                ..Default::default()
            }],
        };

        let rows = metrics_request_to_histogram_rows(&req);
        assert_eq!(rows.len(), 1);
        let r = &rows[0];
        assert_eq!(r.metric, "http.server.duration");
        assert_eq!(r.unit, "ms");
        assert_eq!(r.service.as_deref(), Some("cart"));
        assert_eq!(r.attributes, r#"{"http.route":"/cart"}"#);
        assert_eq!(r.count, 7);
        assert_eq!(r.sum, Some(12.5));
        assert_eq!(r.min, Some(0.1));
        assert_eq!(r.max, Some(9.0));
        assert_eq!(r.explicit_bounds, vec![1.0, 5.0]);
        assert_eq!(r.bucket_counts, vec![2, 3, 2]);
        assert_eq!(r.bucket_counts.len(), r.explicit_bounds.len() + 1);
        assert_eq!(r.temporality.as_deref(), Some("CUMULATIVE"));
        assert_eq!(r.start_time_unix_nano, Some(50));

        // The scalar extractor must ignore histogram points (they go to their own table).
        assert!(metrics_request_to_rows(&req).is_empty());

        let bytes = req.encode_to_vec();
        assert_eq!(decode_metrics_to_histogram_rows(&bytes).unwrap().len(), 1);
    }

    #[test]
    fn normalizes_exp_histograms() {
        use opentelemetry_proto::tonic::collector::metrics::v1::ExportMetricsServiceRequest;
        use opentelemetry_proto::tonic::metrics::v1::{
            ExponentialHistogram, ExponentialHistogramDataPoint, Metric, ResourceMetrics,
            ScopeMetrics, exponential_histogram_data_point::Buckets, metric,
        };
        use opentelemetry_proto::tonic::resource::v1::Resource;

        let dp = ExponentialHistogramDataPoint {
            time_unix_nano: 100,
            start_time_unix_nano: 50,
            count: 6,
            sum: Some(20.0),
            min: Some(0.5),
            max: Some(8.0),
            scale: 2,
            zero_count: 1,
            zero_threshold: 1e-6,
            positive: Some(Buckets {
                offset: 3,
                bucket_counts: vec![2, 1, 2],
            }),
            negative: None,
            attributes: vec![kv("http.route", str_val("/cart"))],
            ..Default::default()
        };
        let req = ExportMetricsServiceRequest {
            resource_metrics: vec![ResourceMetrics {
                resource: Some(Resource {
                    attributes: vec![kv("service.name", str_val("cart"))],
                    ..Default::default()
                }),
                scope_metrics: vec![ScopeMetrics {
                    metrics: vec![Metric {
                        name: "http.server.duration".to_owned(),
                        unit: "ms".to_owned(),
                        data: Some(metric::Data::ExponentialHistogram(ExponentialHistogram {
                            data_points: vec![dp],
                            aggregation_temporality: 1, // DELTA
                        })),
                        ..Default::default()
                    }],
                    ..Default::default()
                }],
                ..Default::default()
            }],
        };

        let rows = metrics_request_to_exp_histogram_rows(&req);
        assert_eq!(rows.len(), 1);
        let r = &rows[0];
        assert_eq!(r.metric, "http.server.duration");
        assert_eq!(r.service.as_deref(), Some("cart"));
        assert_eq!(r.attributes, r#"{"http.route":"/cart"}"#);
        assert_eq!(r.count, 6);
        assert_eq!(r.sum, Some(20.0));
        assert_eq!(r.scale, 2);
        assert_eq!(r.zero_count, 1);
        assert_eq!(r.positive_offset, 3);
        assert_eq!(r.positive_counts, vec![2, 1, 2]);
        assert_eq!(r.negative_offset, 0);
        assert!(r.negative_counts.is_empty());
        assert_eq!(r.temporality.as_deref(), Some("DELTA"));
        assert_eq!(r.start_time_unix_nano, Some(50));

        // Explicit-bucket and scalar extractors ignore exponential points.
        assert!(metrics_request_to_histogram_rows(&req).is_empty());
        assert!(metrics_request_to_rows(&req).is_empty());

        let bytes = req.encode_to_vec();
        assert_eq!(
            decode_metrics_to_exp_histogram_rows(&bytes).unwrap().len(),
            1
        );
    }

    #[test]
    fn normalizes_summaries() {
        use opentelemetry_proto::tonic::collector::metrics::v1::ExportMetricsServiceRequest;
        use opentelemetry_proto::tonic::metrics::v1::{
            Metric, ResourceMetrics, ScopeMetrics, Summary, SummaryDataPoint, metric,
            summary_data_point::ValueAtQuantile,
        };
        use opentelemetry_proto::tonic::resource::v1::Resource;

        let dp = SummaryDataPoint {
            time_unix_nano: 100,
            start_time_unix_nano: 50,
            count: 10,
            sum: 55.0,
            quantile_values: vec![
                ValueAtQuantile {
                    quantile: 0.5,
                    value: 3.0,
                },
                ValueAtQuantile {
                    quantile: 0.99,
                    value: 11.0,
                },
            ],
            attributes: vec![kv("http.route", str_val("/cart"))],
            ..Default::default()
        };
        let req = ExportMetricsServiceRequest {
            resource_metrics: vec![ResourceMetrics {
                resource: Some(Resource {
                    attributes: vec![kv("service.name", str_val("cart"))],
                    ..Default::default()
                }),
                scope_metrics: vec![ScopeMetrics {
                    metrics: vec![Metric {
                        name: "lat".to_owned(),
                        unit: "ms".to_owned(),
                        data: Some(metric::Data::Summary(Summary {
                            data_points: vec![dp],
                        })),
                        ..Default::default()
                    }],
                    ..Default::default()
                }],
                ..Default::default()
            }],
        };

        let rows = metrics_request_to_summary_rows(&req);
        assert_eq!(rows.len(), 1);
        let r = &rows[0];
        assert_eq!(r.metric, "lat");
        assert_eq!(r.count, 10);
        assert_eq!(r.sum, 55.0);
        assert_eq!(r.quantiles, vec![0.5, 0.99]);
        assert_eq!(r.values, vec![3.0, 11.0]);
        assert_eq!(r.start_time_unix_nano, Some(50));

        // Other extractors ignore summary points.
        assert!(metrics_request_to_rows(&req).is_empty());
        assert!(metrics_request_to_histogram_rows(&req).is_empty());
        assert!(metrics_request_to_exp_histogram_rows(&req).is_empty());

        let bytes = req.encode_to_vec();
        assert_eq!(decode_metrics_to_summary_rows(&bytes).unwrap().len(), 1);
    }
}

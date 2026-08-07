//! OTLP protobuf builders returning encoded request bytes, ready to feed to `Db::ingest_otlp_*`
//! or `POST /v1/{logs,traces,metrics}`. Consolidated from the per-crate `#[cfg(test)]` copies that
//! previously lived in `imbh/src/lib.rs`, `imbh-server/src/lib.rs`, and `imbh/tests/cross_process.rs`.

use opentelemetry_proto::tonic::common::v1::{AnyValue as PbAny, KeyValue, any_value};
use opentelemetry_proto::tonic::resource::v1::Resource;
use prost::Message;

fn sv(s: &str) -> PbAny {
    PbAny {
        value: Some(any_value::Value::StringValue(s.to_owned())),
    }
}

fn kv(k: &str, v: &str) -> KeyValue {
    KeyValue {
        key: k.to_owned(),
        value: Some(sv(v)),
        ..Default::default()
    }
}

fn service_resource(service: &str) -> Resource {
    Resource {
        attributes: vec![kv("service.name", service)],
        ..Default::default()
    }
}

/// A one-record OTLP/logs body for `service` with `body_text` at `time` (severity INFO=9).
pub fn otlp_log(service: &str, body_text: &str, time: u64) -> Vec<u8> {
    otlp_rich(service, body_text, time, 9, &[])
}

/// A one-record OTLP/logs body with an explicit `severity` number and `attrs`.
pub fn otlp_rich(
    service: &str,
    body_text: &str,
    time: u64,
    severity: i32,
    attrs: &[(&str, &str)],
) -> Vec<u8> {
    use opentelemetry_proto::tonic::collector::logs::v1::ExportLogsServiceRequest;
    use opentelemetry_proto::tonic::logs::v1::{LogRecord, ResourceLogs, ScopeLogs};

    ExportLogsServiceRequest {
        resource_logs: vec![ResourceLogs {
            resource: Some(service_resource(service)),
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

/// A one-record OTLP/logs body carrying explicit `trace_id`/`span_id` — for exercising trace→log
/// correlation (`LogQuery::trace_id`/`span_id`). Ids are 16/8 bytes as OTLP requires.
pub fn otlp_log_correlated(
    service: &str,
    body_text: &str,
    time: u64,
    trace_id: [u8; 16],
    span_id: [u8; 8],
) -> Vec<u8> {
    use opentelemetry_proto::tonic::collector::logs::v1::ExportLogsServiceRequest;
    use opentelemetry_proto::tonic::logs::v1::{LogRecord, ResourceLogs, ScopeLogs};

    ExportLogsServiceRequest {
        resource_logs: vec![ResourceLogs {
            resource: Some(service_resource(service)),
            scope_logs: vec![ScopeLogs {
                log_records: vec![LogRecord {
                    time_unix_nano: time,
                    severity_number: 9,
                    body: Some(sv(body_text)),
                    trace_id: trace_id.to_vec(),
                    span_id: span_id.to_vec(),
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        }],
    }
    .encode_to_vec()
}

/// A one-span OTLP/traces body (`kind`: 2=SERVER, 1=INTERNAL; `status`: 2=ERROR, 1=OK, 0=UNSET).
pub fn otlp_trace(
    service: &str,
    name: &str,
    kind: i32,
    start: u64,
    end: u64,
    status: i32,
) -> Vec<u8> {
    use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
    use opentelemetry_proto::tonic::trace::v1::{ResourceSpans, ScopeSpans, Span, Status};

    ExportTraceServiceRequest {
        resource_spans: vec![ResourceSpans {
            resource: Some(service_resource(service)),
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

/// A one-span OTLP/traces body carrying a single **integer-typed** span attribute (OTLP `IntValue`,
/// not a string) — for exercising numeric attribute matching/pushdown, which must read a JSON-number
/// attribute (`{"key":n}`), not only a number stored as a string. `trace_id`/`span_id` are derived
/// from `trace_seed` so callers can ingest several distinct traces.
pub fn otlp_trace_int_attr(
    service: &str,
    name: &str,
    start: u64,
    end: u64,
    key: &str,
    value: i64,
    trace_seed: u8,
) -> Vec<u8> {
    use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
    use opentelemetry_proto::tonic::trace::v1::{ResourceSpans, ScopeSpans, Span};

    let int_kv = KeyValue {
        key: key.to_owned(),
        value: Some(PbAny {
            value: Some(any_value::Value::IntValue(value)),
        }),
        ..Default::default()
    };
    ExportTraceServiceRequest {
        resource_spans: vec![ResourceSpans {
            resource: Some(service_resource(service)),
            scope_spans: vec![ScopeSpans {
                spans: vec![Span {
                    trace_id: [trace_seed; 16].to_vec(),
                    span_id: [trace_seed; 8].to_vec(),
                    name: name.to_owned(),
                    kind: 2,
                    start_time_unix_nano: start,
                    end_time_unix_nano: end,
                    attributes: vec![int_kv],
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        }],
    }
    .encode_to_vec()
}

/// An OTLP/traces body: a root span + one child, sharing `trace_id`.
pub fn otlp_trace_tree(service: &str, trace_id: [u8; 16]) -> Vec<u8> {
    use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
    use opentelemetry_proto::tonic::trace::v1::{ResourceSpans, ScopeSpans, Span, Status};

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
            resource: Some(service_resource(service)),
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

/// A wide trace: one root + `n-1` child spans of the root, each carrying `attrs` (for
/// materialization-cost benchmarks). Span names cycle through a small vocabulary.
pub fn otlp_trace_wide(
    service: &str,
    trace_id: [u8; 16],
    n: usize,
    attrs: &[(&str, &str)],
) -> Vec<u8> {
    use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
    use opentelemetry_proto::tonic::trace::v1::{ResourceSpans, ScopeSpans, Span, Status};

    let root_id = 1u64.to_be_bytes().to_vec();
    let mk = |i: usize| Span {
        trace_id: trace_id.to_vec(),
        span_id: (i as u64 + 1).to_be_bytes().to_vec(),
        parent_span_id: if i == 0 { vec![] } else { root_id.clone() },
        name: format!("span-{}", i % 8),
        kind: 3,
        start_time_unix_nano: 1000 + i as u64,
        end_time_unix_nano: 1000 + i as u64 + 100,
        attributes: attrs.iter().map(|(k, v)| kv(k, v)).collect(),
        status: Some(Status {
            code: 1,
            message: String::new(),
        }),
        ..Default::default()
    };
    ExportTraceServiceRequest {
        resource_spans: vec![ResourceSpans {
            resource: Some(service_resource(service)),
            scope_spans: vec![ScopeSpans {
                spans: (0..n).map(mk).collect(),
                ..Default::default()
            }],
            ..Default::default()
        }],
    }
    .encode_to_vec()
}

/// An OTLP/metrics body: one gauge (`cpu`) and one cumulative monotonic sum (`requests`).
pub fn otlp_metrics(service: &str) -> Vec<u8> {
    use opentelemetry_proto::tonic::collector::metrics::v1::ExportMetricsServiceRequest;
    use opentelemetry_proto::tonic::metrics::v1::{
        Gauge, Metric, NumberDataPoint, ResourceMetrics, ScopeMetrics, Sum, metric,
        number_data_point,
    };

    let dp = |v: f64| NumberDataPoint {
        time_unix_nano: 100,
        value: Some(number_data_point::Value::AsDouble(v)),
        ..Default::default()
    };
    ExportMetricsServiceRequest {
        resource_metrics: vec![ResourceMetrics {
            resource: Some(service_resource(service)),
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

/// An OTLP Gauge `metric` with one data point per `vals` entry, each carrying attribute `{key: val}`.
pub fn otlp_gauge_labeled(service: &str, metric_name: &str, key: &str, vals: &[&str]) -> Vec<u8> {
    use opentelemetry_proto::tonic::collector::metrics::v1::ExportMetricsServiceRequest;
    use opentelemetry_proto::tonic::metrics::v1::{
        Gauge, Metric, NumberDataPoint, ResourceMetrics, ScopeMetrics, metric, number_data_point,
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
            resource: Some(service_resource(service)),
            scope_metrics: vec![ScopeMetrics {
                metrics: vec![Metric {
                    name: metric_name.to_owned(),
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

/// A one-point OTLP gauge carrying multiple record-level attributes (multiple labels per point).
pub fn otlp_gauge_attrs(
    service: &str,
    metric_name: &str,
    time: u64,
    attrs: &[(&str, &str)],
) -> Vec<u8> {
    use opentelemetry_proto::tonic::collector::metrics::v1::ExportMetricsServiceRequest;
    use opentelemetry_proto::tonic::metrics::v1::{
        Gauge, Metric, NumberDataPoint, ResourceMetrics, ScopeMetrics, metric, number_data_point,
    };

    ExportMetricsServiceRequest {
        resource_metrics: vec![ResourceMetrics {
            resource: Some(service_resource(service)),
            scope_metrics: vec![ScopeMetrics {
                metrics: vec![Metric {
                    name: metric_name.to_owned(),
                    unit: "1".to_owned(),
                    data: Some(metric::Data::Gauge(Gauge {
                        data_points: vec![NumberDataPoint {
                            time_unix_nano: time,
                            value: Some(number_data_point::Value::AsDouble(1.0)),
                            attributes: attrs.iter().map(|(k, v)| kv(k, v)).collect(),
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

/// An OTLP monotonic Sum with one point per `(time, value)` and the given `temporality`
/// (1=DELTA, 2=CUMULATIVE).
pub fn otlp_sum(
    service: &str,
    metric_name: &str,
    temporality: i32,
    points: &[(u64, f64)],
) -> Vec<u8> {
    use opentelemetry_proto::tonic::collector::metrics::v1::ExportMetricsServiceRequest;
    use opentelemetry_proto::tonic::metrics::v1::{
        Metric, NumberDataPoint, ResourceMetrics, ScopeMetrics, Sum, metric, number_data_point,
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
            resource: Some(service_resource(service)),
            scope_metrics: vec![ScopeMetrics {
                metrics: vec![Metric {
                    name: metric_name.to_owned(),
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

/// An OTLP explicit-bucket cumulative histogram for `service` carrying record-level `attrs`, with one
/// data point per `(time, counts)` entry (`counts.len() == bounds.len() + 1`). The multi-point,
/// attributed shape a `rate()`/`histogram_quantile()` evaluation needs.
pub fn otlp_hist_labeled(
    service: &str,
    metric_name: &str,
    attrs: &[(&str, &str)],
    bounds: &[f64],
    points: &[(u64, &[u64])],
) -> Vec<u8> {
    use opentelemetry_proto::tonic::collector::metrics::v1::ExportMetricsServiceRequest;
    use opentelemetry_proto::tonic::metrics::v1::{
        Histogram, HistogramDataPoint, Metric, ResourceMetrics, ScopeMetrics, metric,
    };

    let data_points = points
        .iter()
        .map(|(time, counts)| HistogramDataPoint {
            time_unix_nano: *time,
            count: counts.iter().sum(),
            explicit_bounds: bounds.to_vec(),
            bucket_counts: counts.to_vec(),
            attributes: attrs.iter().map(|(k, v)| kv(k, v)).collect(),
            ..Default::default()
        })
        .collect();
    ExportMetricsServiceRequest {
        resource_metrics: vec![ResourceMetrics {
            resource: Some(service_resource(service)),
            scope_metrics: vec![ScopeMetrics {
                metrics: vec![Metric {
                    name: metric_name.to_owned(),
                    unit: "s".to_owned(),
                    data: Some(metric::Data::Histogram(Histogram {
                        data_points,
                        aggregation_temporality: 2,
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

/// A one-point OTLP explicit-bucket cumulative histogram (`counts.len() == bounds.len() + 1`).
pub fn otlp_hist(metric_name: &str, bounds: &[f64], counts: &[u64]) -> Vec<u8> {
    use opentelemetry_proto::tonic::collector::metrics::v1::ExportMetricsServiceRequest;
    use opentelemetry_proto::tonic::metrics::v1::{
        Histogram, HistogramDataPoint, Metric, ResourceMetrics, ScopeMetrics, metric,
    };

    ExportMetricsServiceRequest {
        resource_metrics: vec![ResourceMetrics {
            scope_metrics: vec![ScopeMetrics {
                metrics: vec![Metric {
                    name: metric_name.to_owned(),
                    data: Some(metric::Data::Histogram(Histogram {
                        data_points: vec![HistogramDataPoint {
                            time_unix_nano: 1,
                            count: counts.iter().sum(),
                            explicit_bounds: bounds.to_vec(),
                            bucket_counts: counts.to_vec(),
                            ..Default::default()
                        }],
                        aggregation_temporality: 2,
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

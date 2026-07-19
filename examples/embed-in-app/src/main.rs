//! Embedding example (OVERVIEW.md §13 M5/M6): a host app links imbh in-process, feeds it OTLP for all
//! three signals, and queries it through the typed APIs and SQL — no server, no `docker run`.
//!
//! Run: `cargo run -p embed-in-app`

use std::error::Error;
use std::time::Duration;

use imbh::{Db, LogQuery, MetricQuery, TraceId, TraceQuery};

use opentelemetry_proto::tonic::collector::logs::v1::ExportLogsServiceRequest;
use opentelemetry_proto::tonic::collector::metrics::v1::ExportMetricsServiceRequest;
use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
use opentelemetry_proto::tonic::common::v1::{AnyValue, KeyValue, any_value};
use opentelemetry_proto::tonic::logs::v1::{LogRecord, ResourceLogs, ScopeLogs};
use opentelemetry_proto::tonic::metrics::v1::{
    Gauge, Metric, NumberDataPoint, ResourceMetrics, ScopeMetrics, metric, number_data_point,
};
use opentelemetry_proto::tonic::resource::v1::Resource;
use opentelemetry_proto::tonic::trace::v1::{ResourceSpans, ScopeSpans, Span, Status};
use prost::Message;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn Error>> {
    // Render imbh's internal instrumentation to stderr via the facade's `console` collector (same
    // one-liner `imbhd` uses): RUST_LOG-aware and, absent RUST_LOG, every imbh target at `info`.
    // Off in the default build; enable with `cargo run -p embed-in-app --features tracing`.
    #[cfg(feature = "tracing")]
    imbh::console::init();

    // Open an ephemeral, in-process database. Use `Db::builder(path)` for a durable one.
    let db = Db::in_memory().open()?;

    // ── Ingest OTLP for all three signals ────────────────────────────────────────────────
    db.ingest_otlp_logs(&logs_body()).await?;
    db.ingest_otlp_traces(&traces_body()).await?;
    db.ingest_otlp_metrics(&metrics_body()).await?;

    // ── Logs: typed query with full-text `matches` ───────────────────────────────────────
    let errors = db
        .logs()
        .query(LogQuery::new().service("checkout").matches("error"))
        .await?;
    println!(
        "logs · checkout errors ({} matched):",
        errors.stats.rows_returned
    );
    for e in &errors.entries {
        println!("  [{}] {}", e.severity_number.0, e.body);
    }

    // ── Traces: assemble a trace, then search ────────────────────────────────────────────
    let trace = db.traces().get(TraceId([0xaa; 16])).await?;
    if let Some(t) = trace {
        println!(
            "\ntrace · root {}::{} · {} spans · {} ns",
            t.root_service.as_deref().unwrap_or("?"),
            t.root_name.as_deref().unwrap_or("?"),
            t.spans.len(),
            t.duration_ns.0
        );
    }
    let slow = db
        .traces()
        .search(TraceQuery::new().min_duration(Duration::from_millis(1)))
        .await?;
    println!("traces · slow traces: {}", slow.len());

    // ── Metrics: typed range query + catalog ─────────────────────────────────────────────
    let cat = db.metrics().catalog().await?;
    println!(
        "\nmetrics · catalog: {:?}",
        cat.iter().map(|m| &m.metric).collect::<Vec<_>>()
    );
    let matrix = db
        .metrics()
        .range(MetricQuery::gauge("cpu.utilization").step(Duration::from_secs(1)))
        .await?;
    for series in &matrix.0 {
        println!("  cpu.utilization: {} sample(s)", series.samples.len());
    }

    // ── Cross-signal SQL: correlate logs and spans by trace_id ───────────────────────────
    let batches = db
        .sql(
            "SELECT l.service, count(*) AS log_lines \
             FROM logs l WHERE l.trace_id IS NOT NULL GROUP BY l.service",
        )
        .collect()
        .await?;
    println!(
        "\nsql · logs with a trace_id, by service:\n{}",
        pretty(&batches)
    );

    // ── Ops ──────────────────────────────────────────────────────────────────────────────
    let stats = db.stats().await?;
    println!("stats · buffered rows per table:");
    for t in &stats.tables {
        if t.buffer_rows > 0 {
            println!("  {:?}: {}", t.table, t.buffer_rows);
        }
    }
    Ok(())
}

fn pretty(batches: &[imbh::arrow::record_batch::RecordBatch]) -> String {
    imbh::arrow::util::pretty::pretty_format_batches(batches)
        .map(|d| d.to_string())
        .unwrap_or_default()
}

// ── OTLP body builders ───────────────────────────────────────────────────────────────────

fn sv(s: &str) -> AnyValue {
    AnyValue {
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

fn resource(service: &str) -> Option<Resource> {
    Some(Resource {
        attributes: vec![kv("service.name", service)],
        ..Default::default()
    })
}

fn logs_body() -> Vec<u8> {
    let rec = |sev: i32, body: &str, trace: Vec<u8>| LogRecord {
        time_unix_nano: 1_000,
        severity_number: sev,
        body: Some(sv(body)),
        trace_id: trace,
        ..Default::default()
    };
    ExportLogsServiceRequest {
        resource_logs: vec![ResourceLogs {
            resource: resource("checkout"),
            scope_logs: vec![ScopeLogs {
                log_records: vec![
                    rec(17, "payment error: gateway timeout", vec![0xaa; 16]),
                    rec(9, "request ok", vec![0xaa; 16]),
                ],
                ..Default::default()
            }],
            ..Default::default()
        }],
    }
    .encode_to_vec()
}

fn traces_body() -> Vec<u8> {
    let span =
        |span_id: Vec<u8>, parent: Vec<u8>, name: &str, kind: i32, end: u64, status: i32| Span {
            trace_id: vec![0xaa; 16],
            span_id,
            parent_span_id: parent,
            name: name.to_owned(),
            kind,
            start_time_unix_nano: 1_000,
            end_time_unix_nano: end,
            status: Some(Status {
                code: status,
                message: String::new(),
            }),
            ..Default::default()
        };
    ExportTraceServiceRequest {
        resource_spans: vec![ResourceSpans {
            resource: resource("checkout"),
            scope_spans: vec![ScopeSpans {
                spans: vec![
                    span(vec![1; 8], vec![], "POST /checkout", 2, 6_000_000, 2), // 6ms, ERROR
                    span(vec![2; 8], vec![1; 8], "charge", 3, 4_000_000, 0),
                ],
                ..Default::default()
            }],
            ..Default::default()
        }],
    }
    .encode_to_vec()
}

fn metrics_body() -> Vec<u8> {
    let dp = |v: f64| NumberDataPoint {
        time_unix_nano: 1_000,
        value: Some(number_data_point::Value::AsDouble(v)),
        ..Default::default()
    };
    ExportMetricsServiceRequest {
        resource_metrics: vec![ResourceMetrics {
            resource: resource("checkout"),
            scope_metrics: vec![ScopeMetrics {
                metrics: vec![Metric {
                    name: "cpu.utilization".to_owned(),
                    unit: "1".to_owned(),
                    data: Some(metric::Data::Gauge(Gauge {
                        data_points: vec![dp(0.42)],
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

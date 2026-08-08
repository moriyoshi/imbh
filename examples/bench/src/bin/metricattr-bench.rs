//! How big is the metrics attribute gap, and what actually closes it?
//!
//! Metric tables get no Tantivy `.tidx` (§8), so a PromQL-style label matcher — the dominant metric
//! query shape — has only two possible paths: a promoted dictionary column, or a full
//! `json_get_str` scan. `logs` has a third, the `attrs` index, which measurably takes a *selective*
//! filter to the `count(*)` floor. This sizes what metrics are missing and what each candidate fix
//! would recover, by running the same corpus and the same label matcher three ways:
//!
//!   1. **metrics, unpromoted** — today's only path for an arbitrary label.
//!   2. **metrics, promoted** — what a good `promote` list (or auto-promotion) buys. Works at *every*
//!      selectivity, since a promoted column measures at the floor regardless.
//!   3. **logs, unpromoted** — the same filter where the `attrs` index exists, so the difference
//!      against (1) is exactly what extending that index to metric segments would recover.
//!
//! Run at two selectivities, because they separate the candidates: the index's cost gate declines to
//! prune above a ~50% hit fraction, so an unselective matcher (`service="api"` in a single-service
//! deployment) gets nothing from (3) while (2) still helps.
//!
//! Run: `cargo run --release -p bench --bin metricattr-bench -- [segments] [rows_per_segment]`

use std::error::Error;
use std::time::Instant;

use imbh::{Db, Promote, WalMode};
use opentelemetry_proto::tonic::collector::logs::v1::ExportLogsServiceRequest;
use opentelemetry_proto::tonic::collector::metrics::v1::ExportMetricsServiceRequest;
use opentelemetry_proto::tonic::common::v1::{AnyValue, KeyValue, any_value};
use opentelemetry_proto::tonic::logs::v1::{LogRecord, ResourceLogs, ScopeLogs};
use opentelemetry_proto::tonic::metrics::v1::{
    Gauge, Metric, NumberDataPoint, ResourceMetrics, ScopeMetrics, metric, number_data_point,
};
use opentelemetry_proto::tonic::resource::v1::Resource;
use prost::Message;

const REPS: usize = 5;
/// Labels each metric point carries — a realistic instrumented counter, not a toy.
const LABELS_PER_POINT: usize = 10;

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = std::env::args().skip(1);
    let segments: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(20);
    let rows: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(5_000);

    println!(
        "imbh metrics attribute gap — {segments} segments x {rows} points, {LABELS_PER_POINT} labels/point, best of {REPS}\n"
    );

    for (cardinality, label) in [(2usize, "50%"), (100usize, "1%")] {
        println!(
            "-- label `k` has {cardinality} distinct values: matcher selects {label} of points"
        );

        // (1) and (3): nothing promoted. Both signals get the same rows and the same matcher.
        let dir = tempfile::tempdir()?;
        let db = Db::builder(dir.path()).wal(WalMode::Off).open()?;
        let b = db.blocking();
        for i in 0..segments {
            b.ingest_otlp_metrics(&metrics_body(i, rows, cardinality))?;
            b.ingest_otlp_logs(&logs_body(i, rows, cardinality))?;
            b.flush()?;
        }
        let m_floor = bench(
            &b,
            "metrics count(*) floor",
            "SELECT count(*) AS c FROM metrics_gauge",
        )?;
        let m_json = bench(
            &b,
            "metrics, label via json  (today)",
            "SELECT count(*) AS c FROM metrics_gauge WHERE json_get_str(attributes, 'k') = 'v0'",
        )?;
        let l_json = bench(
            &b,
            "logs,    label via json  (attrs index exists)",
            "SELECT count(*) AS c FROM logs WHERE json_get_str(attributes, 'k') = 'v0'",
        )?;

        // (2): same corpus, `k` promoted.
        let dir2 = tempfile::tempdir()?;
        let db2 = Db::builder(dir2.path())
            .wal(WalMode::Off)
            .promote(Promote::new(["k"]))
            .open()?;
        let b2 = db2.blocking();
        for i in 0..segments {
            b2.ingest_otlp_metrics(&metrics_body(i, rows, cardinality))?;
            b2.flush()?;
        }
        let m_promoted = bench(
            &b2,
            "metrics, label via promoted column",
            "SELECT count(*) AS c FROM metrics_gauge WHERE \
             CASE WHEN \"k\" IS NOT NULL THEN CAST(\"k\" AS VARCHAR) \
             ELSE json_get_str(attributes, 'k') END = 'v0'",
        )?;

        println!(
            "   gap today = {:+.1} ms over floor | promotion recovers {:+.1} ms | \
             an attrs index on metrics would recover ~{:+.1} ms\n",
            m_json - m_floor,
            m_promoted - m_json,
            l_json - m_json
        );
    }
    Ok(())
}

fn bench(b: &imbh::BlockingDb, label: &str, sql: &str) -> Result<f64, Box<dyn Error>> {
    let warm = b.sql(sql)?;
    let rows: usize = warm.iter().map(|x| x.num_rows()).sum();
    let mut best = f64::MAX;
    for _ in 0..REPS {
        let t = Instant::now();
        let out = b.sql(sql)?;
        std::hint::black_box(&out);
        best = best.min(t.elapsed().as_secs_f64() * 1e3);
    }
    println!("   {label:<46} {best:8.2} ms  ({rows} rows)");
    Ok(best)
}

fn sv(s: &str) -> AnyValue {
    AnyValue {
        value: Some(any_value::Value::StringValue(s.to_owned())),
    }
}

fn labels(j: usize, cardinality: usize) -> Vec<KeyValue> {
    let mut kv = vec![KeyValue {
        key: "k".to_owned(),
        value: Some(sv(&format!("v{}", j % cardinality))),
        ..Default::default()
    }];
    for n in 0..LABELS_PER_POINT - 1 {
        kv.push(KeyValue {
            key: format!("z{n}"),
            value: Some(sv("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")),
            ..Default::default()
        });
    }
    kv
}

fn resource() -> Option<Resource> {
    Some(Resource {
        attributes: vec![KeyValue {
            key: "service.name".to_owned(),
            value: Some(sv("cart")),
            ..Default::default()
        }],
        ..Default::default()
    })
}

fn metrics_body(seg: usize, rows: usize, cardinality: usize) -> Vec<u8> {
    let base = (seg * rows) as u64;
    let points = (0..rows)
        .map(|j| NumberDataPoint {
            time_unix_nano: base + j as u64 + 1,
            value: Some(number_data_point::Value::AsDouble(j as f64)),
            attributes: labels(j, cardinality),
            ..Default::default()
        })
        .collect();
    ExportMetricsServiceRequest {
        resource_metrics: vec![ResourceMetrics {
            resource: resource(),
            scope_metrics: vec![ScopeMetrics {
                metrics: vec![Metric {
                    name: "g".to_owned(),
                    data: Some(metric::Data::Gauge(Gauge {
                        data_points: points,
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

fn logs_body(seg: usize, rows: usize, cardinality: usize) -> Vec<u8> {
    let base = (seg * rows) as u64;
    let records = (0..rows)
        .map(|j| LogRecord {
            time_unix_nano: base + j as u64 + 1,
            severity_number: 9,
            body: Some(sv("request completed ok")),
            attributes: labels(j, cardinality),
            ..Default::default()
        })
        .collect();
    ExportLogsServiceRequest {
        resource_logs: vec![ResourceLogs {
            resource: resource(),
            scope_logs: vec![ScopeLogs {
                log_records: records,
                ..Default::default()
            }],
            ..Default::default()
        }],
    }
    .encode_to_vec()
}

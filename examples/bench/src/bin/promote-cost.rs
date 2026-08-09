//! What does one promoted attribute key actually cost?
//!
//! This is the hard gate before any auto-promotion policy: a threshold cannot be chosen from first
//! principles, and picking one from intuition is exactly how the (rejected) promoted-push-down plan
//! went wrong. Three axes, per §6.1's design:
//!
//! 1. **On-disk bytes.** A promoted key appends a nullable `Dictionary(Int32,Utf8)` column to
//!    **every** signal schema — `logs`, `spans`, and all five metric tables — not just the one that
//!    carries the key. So the cost is one *populated* column plus six *all-NULL* ones. Both are
//!    measured here by ingesting all three signals with the keys present only on logs.
//! 2. **Seal time.** `build_promoted_columns` calls `lookup_promoted` → `json_get` once **per key per
//!    row**, so promoting N keys means N attribute lookups per row on the encode path.
//! 3. **Buffer RSS.** `push_log_batch` builds the Arrow batch at *ingest*, so promoted columns are
//!    resident in the mutable buffer, not merely at seal. Note `DbStats::buffer_bytes` cannot see
//!    them — it sums `Row::approx_bytes()`, the pre-Arrow row size — so this reads `VmRSS` directly.
//!
//! Run one process per promote-count: `cargo run --release -p bench --bin promote-cost -- <keys> [rows] [cardinality]`

use std::error::Error;
use std::time::Instant;

use imbh::{Db, Promote, WalMode};
use opentelemetry_proto::tonic::collector::logs::v1::ExportLogsServiceRequest;
use opentelemetry_proto::tonic::collector::metrics::v1::ExportMetricsServiceRequest;
use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
use opentelemetry_proto::tonic::common::v1::{AnyValue, KeyValue, any_value};
use opentelemetry_proto::tonic::logs::v1::{LogRecord, ResourceLogs, ScopeLogs};
use opentelemetry_proto::tonic::metrics::v1::{
    Gauge, Metric, NumberDataPoint, ResourceMetrics, ScopeMetrics, metric, number_data_point,
};
use opentelemetry_proto::tonic::resource::v1::Resource;
use opentelemetry_proto::tonic::trace::v1::{ResourceSpans, ScopeSpans, Span};
use prost::Message;

/// Attributes each log record carries. The first `PROMOTE_COUNTS.max()` are the promotable ones.
const ATTRS_PER_RECORD: usize = 20;

/// One promote-count per process invocation. Sharing a process across counts makes the RSS axis
/// meaningless: the allocator does not return pages between runs, so the first count absorbs every
/// first-touch page (~200 MiB) and later ones measure ~0. Run the binary once per count and diff the
/// lines. Sweep 0/1/5/20 keys.
fn main() -> Result<(), Box<dyn Error>> {
    let mut args = std::env::args().skip(1);
    let n: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let rows: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(50_000);
    // Distinct values per promotable key. The default is the case promotion is *for* (a label);
    // passing a large value measures what a wrong auto-promotion choice would cost, since a
    // high-cardinality dictionary column is the anti-pattern §6.1 warns against.
    let cardinality: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(16);
    // How many segments to spread the rows over. This is what separates *global* cardinality from
    // *per-segment* cardinality, and only the latter should cost anything: Parquet builds its
    // dictionary per column chunk, so a key with many values overall but few within any one segment
    // gets a small dictionary in every segment. `pod.name` over a month is exactly that shape.
    let segments: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(1);
    assert!(
        n <= ATTRS_PER_RECORD,
        "only {ATTRS_PER_RECORD} promotable keys exist per record"
    );

    let dir = tempfile::tempdir()?;
    let keys: Vec<String> = (0..n).map(|i| format!("a{i:02}")).collect();
    let db = Db::builder(dir.path())
        .wal(WalMode::Off)
        .promote(Promote::new(keys))
        .open()?;
    let b = db.blocking();

    let rss_before = vm_rss_kib()?;
    // Logs carry the promotable keys; spans and metrics deliberately do not, so their promoted
    // columns are entirely NULL — the "every signal pays" half of the cost.
    let per_segment = rows.div_ceil(segments);
    for seg in 0..segments {
        b.ingest_otlp_logs(&logs_body(seg, per_segment, cardinality))?;
        if seg + 1 < segments {
            b.flush()?;
        }
    }
    b.ingest_otlp_traces(&traces_body(rows / 10))?;
    b.ingest_otlp_metrics(&metrics_body(rows / 10))?;
    // Read RSS before the seal: this is the *buffer* cost, which `push_*_batch` has already paid at
    // ingest, not the encode-path cost.
    let rss_after = vm_rss_kib()?;

    let t = Instant::now();
    b.flush()?;
    let seal_ms = t.elapsed().as_secs_f64() * 1e3;

    println!(
        "PROMOTE_COST keys={n} rows={rows} card={cardinality} segs={segments} per_seg_card={} disk_bytes={} seal_ms={seal_ms:.1} buffer_rss_kib={}",
        cardinality.min(per_segment),
        dir_bytes(dir.path())?,
        rss_after.saturating_sub(rss_before)
    );
    Ok(())
}

/// Total bytes of every file under `dir`, recursively — segments, index sidecars, manifest.
fn dir_bytes(dir: &std::path::Path) -> Result<u64, Box<dyn Error>> {
    let mut total = 0;
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let meta = entry.metadata()?;
        total += if meta.is_dir() {
            dir_bytes(&entry.path())?
        } else {
            meta.len()
        };
    }
    Ok(total)
}

/// `VmRSS` in kiB from `/proc/self/status` (Linux-only, same source as `examples/rss-probe`).
fn vm_rss_kib() -> Result<u64, Box<dyn Error>> {
    let status = std::fs::read_to_string("/proc/self/status")?;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            return Ok(rest
                .split_whitespace()
                .next()
                .ok_or("malformed /proc/self/status")?
                .parse::<u64>()?);
        }
    }
    Err("VmRSS not found".into())
}

fn sv(s: &str) -> AnyValue {
    AnyValue {
        value: Some(any_value::Value::StringValue(s.to_owned())),
    }
}

fn resource(service: &str) -> Option<Resource> {
    Some(Resource {
        attributes: vec![KeyValue {
            key: "service.name".to_owned(),
            value: Some(sv(service)),
            ..Default::default()
        }],
        ..Default::default()
    })
}

/// Log records carrying `ATTRS_PER_RECORD` attributes named `a00..`, each taking one of
/// `cardinality` distinct values. At the default 16 a dictionary column is at its most favourable —
/// the case promotion exists for. Raise it to `rows` to measure the anti-pattern instead.
fn logs_body(seg: usize, rows: usize, cardinality: usize) -> Vec<u8> {
    let base = (seg * rows) as u64;
    let records = (0..rows)
        .map(|j| LogRecord {
            time_unix_nano: base + j as u64 + 1,
            severity_number: 9,
            body: Some(sv("request completed ok")),
            attributes: (0..ATTRS_PER_RECORD)
                .map(|i| KeyValue {
                    key: format!("a{i:02}"),
                    // Values are namespaced by segment, so each segment sees at most `rows`
                    // distinct values however large the global cardinality gets.
                    value: Some(sv(&format!("s{seg}v{}", j % cardinality))),
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        })
        .collect();
    ExportLogsServiceRequest {
        resource_logs: vec![ResourceLogs {
            resource: resource("cart"),
            scope_logs: vec![ScopeLogs {
                log_records: records,
                ..Default::default()
            }],
            ..Default::default()
        }],
    }
    .encode_to_vec()
}

/// Spans with **no** promotable attributes: every promoted column on `spans` is all-NULL.
fn traces_body(rows: usize) -> Vec<u8> {
    let spans = (0..rows)
        .map(|j| Span {
            trace_id: (j as u128 + 1).to_be_bytes().to_vec(),
            span_id: (j as u64 + 1).to_be_bytes().to_vec(),
            name: "op".to_owned(),
            start_time_unix_nano: j as u64 + 1,
            end_time_unix_nano: j as u64 + 500,
            ..Default::default()
        })
        .collect();
    ExportTraceServiceRequest {
        resource_spans: vec![ResourceSpans {
            resource: resource("cart"),
            scope_spans: vec![ScopeSpans {
                spans,
                ..Default::default()
            }],
            ..Default::default()
        }],
    }
    .encode_to_vec()
}

/// Gauge points with **no** promotable attributes: same all-NULL story on a metric table.
fn metrics_body(rows: usize) -> Vec<u8> {
    let points = (0..rows)
        .map(|j| NumberDataPoint {
            time_unix_nano: j as u64 + 1,
            value: Some(number_data_point::Value::AsDouble(j as f64)),
            ..Default::default()
        })
        .collect();
    ExportMetricsServiceRequest {
        resource_metrics: vec![ResourceMetrics {
            resource: resource("cart"),
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

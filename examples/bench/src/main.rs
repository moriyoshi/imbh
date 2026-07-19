//! A lightweight imbh benchmark — ingest throughput plus a few representative query latencies,
//! timed with `std::time` (no criterion, no heavy deps). This is a rough operational sketch, not a
//! statistically rigorous microbenchmark; it exists to give hosts a feel for the numbers and to
//! catch gross regressions.
//!
//! Run: `cargo run --release -p bench -- [total_records]` (default 100_000).

use std::error::Error;
use std::time::Instant;

use imbh::{Db, WalMode};
use opentelemetry_proto::tonic::collector::logs::v1::ExportLogsServiceRequest;
use opentelemetry_proto::tonic::common::v1::{AnyValue, KeyValue, any_value};
use opentelemetry_proto::tonic::logs::v1::{LogRecord, ResourceLogs, ScopeLogs};
use opentelemetry_proto::tonic::resource::v1::Resource;
use prost::Message;

const RECORDS_PER_BODY: usize = 200;
const SERVICES: [&str; 4] = ["cart", "checkout", "search", "payments"];
const BODIES: [&str; 4] = [
    "request completed ok",
    "connection error to upstream",
    "cache miss for key",
    "slow query detected",
];

fn main() -> Result<(), Box<dyn Error>> {
    let total: usize = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(100_000);
    let bodies = total.div_ceil(RECORDS_PER_BODY);
    let records = bodies * RECORDS_PER_BODY;

    let dir = tempfile::tempdir()?;
    // WAL off isolates the storage engine's ingest cost from fsync policy (the ingest ceiling).
    let db = Db::builder(dir.path()).wal(WalMode::Off).open()?;
    let b = db.blocking();

    println!("imbh bench — {records} log records ({bodies} OTLP bodies × {RECORDS_PER_BODY})\n");

    // ── ingest ───────────────────────────────────────────────────────────────────────────
    let t = Instant::now();
    for i in 0..bodies {
        b.ingest_otlp_logs(&logs_body(i))?;
    }
    let ingest = t.elapsed();
    report("ingest (buffer)", records, ingest);

    // ── seal ─────────────────────────────────────────────────────────────────────────────
    let t = Instant::now();
    b.flush()?;
    let seal = t.elapsed();
    println!(
        "  seal (buffer → Parquet + Tantivy): {:.1} ms  ({:.1} M rows/s)",
        seal.as_secs_f64() * 1e3,
        records as f64 / seal.as_secs_f64() / 1e6
    );

    // ── queries (over the sealed segment) ──────────────────────────────────────────────────
    println!("\nqueries over {records} sealed rows:");
    query(&b, "count(*)", "SELECT count(*) AS c FROM logs")?;
    query(
        &b,
        "filter by service",
        "SELECT count(*) AS c FROM logs WHERE service = 'cart'",
    )?;
    query(
        &b,
        "matches('error') full-text",
        "SELECT count(*) AS c FROM logs WHERE matches(body, 'error')",
    )?;
    query(
        &b,
        "group by service",
        "SELECT service, count(*) AS c FROM logs GROUP BY service",
    )?;
    query(
        &b,
        "matches + service + limit 100",
        "SELECT \"time\", body FROM logs WHERE matches(body, 'error') AND service = 'cart' \
         ORDER BY \"time\" DESC LIMIT 100",
    )?;

    Ok(())
}

fn report(label: &str, records: usize, elapsed: std::time::Duration) {
    println!(
        "  {label}: {:.1} ms  ({:.2} M rows/s)",
        elapsed.as_secs_f64() * 1e3,
        records as f64 / elapsed.as_secs_f64() / 1e6
    );
}

fn query(b: &imbh::BlockingDb, label: &str, sql: &str) -> Result<(), Box<dyn Error>> {
    let t = Instant::now();
    let batches = b.sql(sql)?;
    let rows: usize = batches.iter().map(|x| x.num_rows()).sum();
    println!(
        "  {label}: {:.2} ms  ({rows} result rows)",
        t.elapsed().as_secs_f64() * 1e3
    );
    Ok(())
}

/// One OTLP/logs body of `RECORDS_PER_BODY` records, varied across services/bodies/severities.
fn logs_body(batch: usize) -> Vec<u8> {
    let sv = |s: &str| AnyValue {
        value: Some(any_value::Value::StringValue(s.to_owned())),
    };
    let base = (batch * RECORDS_PER_BODY) as u64;
    let records = (0..RECORDS_PER_BODY)
        .map(|j| {
            let idx = batch + j;
            LogRecord {
                time_unix_nano: base + j as u64 + 1,
                severity_number: 9 + (idx % 8) as i32,
                body: Some(sv(BODIES[idx % BODIES.len()])),
                attributes: vec![KeyValue {
                    key: "http.route".to_owned(),
                    value: Some(sv(if idx.is_multiple_of(2) {
                        "/cart"
                    } else {
                        "/checkout"
                    })),
                    ..Default::default()
                }],
                ..Default::default()
            }
        })
        .collect();
    ExportLogsServiceRequest {
        resource_logs: vec![ResourceLogs {
            resource: Some(Resource {
                attributes: vec![KeyValue {
                    key: "service.name".to_owned(),
                    value: Some(sv(SERVICES[batch % SERVICES.len()])),
                    ..Default::default()
                }],
                ..Default::default()
            }),
            scope_logs: vec![ScopeLogs {
                log_records: records,
                ..Default::default()
            }],
            ..Default::default()
        }],
    }
    .encode_to_vec()
}

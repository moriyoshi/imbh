//! M0 walking-skeleton example (OVERVIEW.md §13 M0): OTLP/logs bytes → buffer → Parquet
//! segment → SQL round-trip.
//!
//! Usage:
//!   replay-otlp-file [OTLP_LOGS_PROTOBUF_FILE] [DB_DIR]
//!
//! With no file argument it ingests a small built-in sample. With no DB dir it uses a
//! throwaway temp directory. Either way it ingests, force-seals a segment, and prints a few
//! SQL results over the union of the sealed segment and the live buffer.

use std::error::Error;
use std::sync::Arc;

use imbh::Db;
use imbh::arrow::util::pretty::pretty_format_batches;

use opentelemetry_proto::tonic::collector::logs::v1::ExportLogsServiceRequest;
use opentelemetry_proto::tonic::common::v1::{AnyValue, KeyValue, any_value};
use opentelemetry_proto::tonic::logs::v1::{LogRecord, ResourceLogs, ScopeLogs};
use opentelemetry_proto::tonic::resource::v1::Resource;
use prost::Message;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn Error>> {
    // Render imbh's internal instrumentation to stderr via the facade's `console` collector (same
    // one-liner `imbhd` uses): RUST_LOG-aware and, absent RUST_LOG, every imbh target at `info`.
    // Off in the default build; enable with `cargo run -p replay-otlp-file --features tracing`.
    #[cfg(feature = "tracing")]
    imbh::console::init();

    let mut args = std::env::args().skip(1);
    let file_arg = args.next();
    let dir_arg = args.next();

    // Open the DB (temp dir when none is given; kept alive for the run).
    let _tmp;
    let db = match dir_arg {
        Some(dir) => Db::builder(dir).open()?,
        None => {
            _tmp = tempfile::tempdir()?;
            Db::builder(_tmp.path()).open()?
        }
    };

    // Ingest: a provided OTLP/logs protobuf file, or the built-in sample.
    let bytes = match &file_arg {
        Some(path) => {
            println!("ingesting OTLP/logs from {path}");
            std::fs::read(path)?
        }
        None => {
            println!(
                "ingesting built-in sample (pass an OTLP/logs protobuf file to replay your own)"
            );
            sample_request().encode_to_vec()
        }
    };
    let receipt = db.ingest_otlp_logs(&bytes).await?;
    println!("accepted {} record(s)", receipt.accepted);

    // Force-seal the buffer into an immutable Parquet segment, then add one more buffered
    // record so the query spans buffer ∪ segment.
    db.flush().await?;
    println!("sealed {} segment(s)", db.segments().len());
    db.ingest_otlp_logs(&one_log("checkout", "post-seal buffered record", 9_999))
        .await?;

    run(&db, "total rows", "SELECT count(*) AS rows FROM logs").await?;
    run(
        &db,
        "rows per service",
        "SELECT service, count(*) AS rows FROM logs GROUP BY service ORDER BY service",
    )
    .await?;
    run(
        &db,
        "error/timeout bodies",
        "SELECT time, service, body FROM logs \
         WHERE body LIKE '%error%' OR body LIKE '%timeout%' ORDER BY time",
    )
    .await?;

    db.close().await?;
    Ok(())
}

async fn run(db: &Arc<Db>, title: &str, sql: &str) -> Result<(), Box<dyn Error>> {
    let batches = db.sql(sql).collect().await?;
    println!("\n── {title} ──\n{}", pretty_format_batches(&batches)?);
    Ok(())
}

fn sv(s: &str) -> AnyValue {
    AnyValue {
        value: Some(any_value::Value::StringValue(s.to_owned())),
    }
}

fn one_log(service: &str, body: &str, time: u64) -> Vec<u8> {
    request_of(vec![(service, body, time)]).encode_to_vec()
}

fn sample_request() -> ExportLogsServiceRequest {
    request_of(vec![
        ("cart", "request ok", 1_000),
        ("cart", "connection error to checkout", 2_000),
        ("checkout", "request ok", 3_000),
        ("checkout", "upstream timeout", 4_000),
        ("checkout", "request ok", 5_000),
        ("cart", "request ok", 6_000),
    ])
}

/// Build an OTLP/logs request with one `ResourceLogs` per distinct service.
fn request_of(records: Vec<(&str, &str, u64)>) -> ExportLogsServiceRequest {
    use std::collections::BTreeMap;
    let mut by_service: BTreeMap<&str, Vec<LogRecord>> = BTreeMap::new();
    for (service, body, time) in records {
        by_service.entry(service).or_default().push(LogRecord {
            time_unix_nano: time,
            severity_number: 9,
            severity_text: "INFO".to_owned(),
            body: Some(sv(body)),
            ..Default::default()
        });
    }
    let resource_logs = by_service
        .into_iter()
        .map(|(service, log_records)| ResourceLogs {
            resource: Some(Resource {
                attributes: vec![KeyValue {
                    key: "service.name".to_owned(),
                    value: Some(sv(service)),
                    ..Default::default()
                }],
                ..Default::default()
            }),
            scope_logs: vec![ScopeLogs {
                log_records,
                ..Default::default()
            }],
            ..Default::default()
        })
        .collect();
    ExportLogsServiceRequest { resource_logs }
}

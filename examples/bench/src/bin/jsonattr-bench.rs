//! How much does an *unpromoted* attribute filter cost, and how does that scale with how many
//! attributes a record carries?
//!
//! This is the floor question: whatever key a user picks, if it is not promoted the query runs
//! `json_get_str(attributes, 'k')` once per row. `imbh_core::json_get`
//! (`crates/imbh-core/src/attributes.rs`) parses the **entire** blob into a `Vec<(String, AnyValue)>`
//! — allocating a `String` for every key and every string value — and then linear-searches it for one
//! field. So the cost should scale with blob *width*, not with the key being looked for, and a
//! key-targeted extractor that returns at the match without materializing the map should flatten it.
//!
//! Measured here before that extractor is written, so the decision to write it is a number rather
//! than an intuition.
//!
//! Two selectivities per width, because they exercise different things:
//!   - **50%** — the Tantivy `attrs` cost gate declines to prune, so *every* row is parsed. This is
//!     the pure JSON-cost signal.
//!   - **1%** — the index prunes hard, so the JSON UDF runs on few rows. Shows how much of the win
//!     the existing index already captures for `logs`/`spans` (and, by absence, what metric tables
//!     never get).
//!
//! Run: `cargo run --release -p bench --bin jsonattr-bench -- [segments] [rows_per_segment]`

use std::error::Error;
use std::time::Instant;

use imbh::{Db, WalMode};
use opentelemetry_proto::tonic::collector::logs::v1::ExportLogsServiceRequest;
use opentelemetry_proto::tonic::common::v1::{AnyValue, KeyValue, any_value};
use opentelemetry_proto::tonic::logs::v1::{LogRecord, ResourceLogs, ScopeLogs};
use opentelemetry_proto::tonic::resource::v1::Resource;
use prost::Message;

const REPS: usize = 5;
/// Attributes carried per record. 2 is the toy case the earlier benchmark used; 10 is an ordinary
/// instrumented HTTP span; 40 is a fat record from a framework that attaches everything it knows.
const WIDTHS: [usize; 3] = [2, 10, 40];

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = std::env::args().skip(1);
    let segments: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(20);
    let rows: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(5_000);

    println!(
        "imbh unpromoted-attribute cost vs blob width — {segments} segments x {rows} rows, best of {REPS}\n"
    );
    println!("  the probed key `k` is the FIRST key in each record's attribute map, so a targeted");
    println!(
        "  extractor could stop almost immediately; `json_get` parses the whole map regardless.\n"
    );

    for width in WIDTHS {
        for (cardinality, label) in [(2usize, "50%"), (100usize, "1%")] {
            let dir = tempfile::tempdir()?;
            // No `promote` at all: this is deliberately the path a user gets for an arbitrary key.
            let db = Db::builder(dir.path()).wal(WalMode::Off).open()?;
            let b = db.blocking();
            for i in 0..segments {
                b.ingest_otlp_logs(&logs_body(i, rows, cardinality, width))?;
                b.flush()?;
            }

            let floor = bench(&b, "count(*) floor", "SELECT count(*) AS c FROM logs")?;
            let json = bench(
                &b,
                "json_get_str = 'v0'",
                "SELECT count(*) AS c FROM logs WHERE json_get_str(attributes, 'k') = 'v0'",
            )?;
            // Key position, isolated. Both use the SAME predicate (`IS NOT NULL`) so the only
            // variable is how far into the object the scan must walk. An earlier version of this
            // benchmark compared `= 'v0'` on the first key against `IS NOT NULL` on the last and was
            // therefore measuring two things at once — the `= 'v0'` shape is also the one the Tantivy
            // `attrs` index recognizes, so it carries an index search the other does not.
            let first_notnull = bench(
                &b,
                "json_get_str('k') IS NOT NULL   (FIRST key)",
                "SELECT count(*) AS c FROM logs WHERE json_get_str(attributes, 'k') IS NOT NULL",
            )?;
            let last_notnull = bench(
                &b,
                &format!("json_get_str('z{}') IS NOT NULL   (LAST key)", width - 1),
                &format!(
                    "SELECT count(*) AS c FROM logs WHERE json_get_str(attributes, 'z{}') IS NOT NULL",
                    width - 1
                ),
            )?;
            println!(
                "  width {width:>2}, sel {label:>3}: eq-filter - floor = {:+.2} ms | \
                 IS NOT NULL first {:.1} ms vs last {:.1} ms (position cost {:+.1} ms)\n",
                json - floor,
                first_notnull,
                last_notnull,
                last_notnull - first_notnull
            );
        }
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
    println!("    {label:<44} {best:8.2} ms  ({rows} rows)");
    Ok(best)
}

fn sv(s: &str) -> AnyValue {
    AnyValue {
        value: Some(any_value::Value::StringValue(s.to_owned())),
    }
}

/// One OTLP/logs body whose records carry `width` attributes: the probed `k` first (canonical JSON
/// sorts keys, and `k` < `z…`, so it stays first on disk), then `width - 1` `z<N>` fillers.
fn logs_body(i: usize, rows: usize, cardinality: usize, width: usize) -> Vec<u8> {
    let base = i as u64 * 1_000_000;
    let records = (0..rows)
        .map(|j| {
            let mut attributes = vec![KeyValue {
                key: "k".to_owned(),
                value: Some(sv(&format!("v{}", j % cardinality))),
                ..Default::default()
            }];
            for n in 0..width - 1 {
                attributes.push(KeyValue {
                    key: format!("z{n}"),
                    value: Some(sv("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")),
                    ..Default::default()
                });
            }
            LogRecord {
                time_unix_nano: base + j as u64,
                severity_number: 9,
                body: Some(sv("request completed ok")),
                attributes,
                ..Default::default()
            }
        })
        .collect();
    ExportLogsServiceRequest {
        resource_logs: vec![ResourceLogs {
            resource: Some(Resource {
                attributes: vec![KeyValue {
                    key: "service.name".to_owned(),
                    value: Some(sv("cart")),
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

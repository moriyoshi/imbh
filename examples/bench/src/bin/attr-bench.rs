//! Is the promoted-attribute push-down worth building? Measured **before** implementing it.
//!
//! The proposed change would route `CAST("k" AS VARCHAR) = 'v'` (what a promoted key compiles to)
//! into the same `attrs` Tantivy index that `json_get_str(attributes,'k') = 'v'` already uses. So the
//! win it would add is exactly *what that index pruning is worth*, and that is measurable today with
//! no new code — by running the same corpus against a build whose cost gate never applies a
//! `RowSelection`.
//!
//! Three shapes per selectivity:
//!   1. `json_get_str(...) = 'v'`      — index pruning + per-row JSON UDF
//!   2. `CAST("k" AS VARCHAR) = 'v'`   — no pruning + cheap dictionary compare (today's promoted path)
//!   3. `count(*)`                     — the floor: what reading the corpus costs at all
//!
//! Shape 1 measured with the gate live vs. disabled isolates the pruning component; shape 2 is where
//! a promoted key sits now, and promoted+push-down would land at (2) minus that component.
//!
//! Deliberately **no time predicate**: segment-level skips would otherwise mask the row-level effect
//! this is trying to size.
//!
//! Run: `cargo run --release -p bench --bin attr-bench -- [segments] [rows_per_segment]`

use std::error::Error;
use std::time::Instant;

use imbh::{Db, Promote, WalMode};
use opentelemetry_proto::tonic::collector::logs::v1::ExportLogsServiceRequest;
use opentelemetry_proto::tonic::common::v1::{AnyValue, KeyValue, any_value};
use opentelemetry_proto::tonic::logs::v1::{LogRecord, ResourceLogs, ScopeLogs};
use opentelemetry_proto::tonic::resource::v1::Resource;
use prost::Message;

const REPS: usize = 5;
/// Distinct values of the `k` attribute. Selectivity of `k = 'v0'` within a segment is `1/K`, so
/// these bracket the cost gate (which declines to build a `RowSelection` above ~50% hit fraction).
const CARDINALITIES: [usize; 4] = [2, 10, 100, 1000];

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = std::env::args().skip(1);
    let segments: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(20);
    let rows: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(5_000);

    println!(
        "imbh promoted-attribute push-down sizing — {segments} segments x {rows} rows, best of {REPS}"
    );
    println!("(no time predicate: segment skips would mask the row-level effect being sized)\n");

    for k in CARDINALITIES {
        let dir = tempfile::tempdir()?;
        // `promote(["k"])` gives every row BOTH spellings: the dictionary column and the JSON blob.
        // That is what makes shapes 1 and 2 directly comparable — same rows, same answer.
        let db = Db::builder(dir.path())
            .wal(WalMode::Off)
            .promote(Promote::new(["k"]))
            .open()?;
        let b = db.blocking();
        for i in 0..segments {
            b.ingest_otlp_logs(&logs_body(i, rows, k))?;
            b.flush()?;
        }

        let total = segments * rows;
        let matching = total / k;
        println!(
            "-- k has {k} distinct values: 'v0' matches {matching}/{total} rows ({:.1}%)",
            100.0 / k as f64
        );

        let floor = bench(&b, "count(*) floor", "SELECT count(*) AS c FROM logs")?;
        let json = bench(
            &b,
            "json_get_str = 'v0'   (indexed)",
            "SELECT count(*) AS c FROM logs WHERE json_get_str(attributes, 'k') = 'v0'",
        )?;
        let promoted = bench(
            &b,
            "CAST(\"k\" AS VARCHAR) = 'v0'   (promoted, unpushed)",
            "SELECT count(*) AS c FROM logs WHERE CAST(\"k\" AS VARCHAR) = 'v0'",
        )?;
        // Four spellings of the same promoted-key filter, to settle two questions with data rather
        // than argument:
        //   (1) is hand-written CASE actually better than `COALESCE`, or does DataFusion's CSE make
        //       the cast-in-the-WHEN free?
        //   (2) is the `CAST(... AS VARCHAR)` worth anything for a filter, or is comparing in
        //       dictionary space cheaper?
        // Every row here carries the column, so the fallback arm is dead — this measures the price of
        // the safety net on the path that matters, not the fallback itself.
        let bare_dict = bench(
            &b,
            "\"k\" = 'v0'   (no cast, dictionary space)",
            "SELECT count(*) AS c FROM logs WHERE \"k\" = 'v0'",
        )?;
        let case_form = bench(
            &b,
            "CASE WHEN \"k\" IS NOT NULL ...   (what ships)",
            "SELECT count(*) AS c FROM logs WHERE \
             CASE WHEN \"k\" IS NOT NULL THEN CAST(\"k\" AS VARCHAR) \
             ELSE json_get_str(attributes, 'k') END = 'v0'",
        )?;
        let coalesce_form = bench(
            &b,
            "COALESCE(CAST(\"k\"...), json_get_str(...))",
            "SELECT count(*) AS c FROM logs WHERE \
             COALESCE(CAST(\"k\" AS VARCHAR), json_get_str(attributes, 'k')) = 'v0'",
        )?;
        println!(
            "   vs bare cast column: CASE {:+.2} ms   coalesce {:+.2} ms   no-cast {:+.2} ms",
            case_form - promoted,
            coalesce_form - promoted,
            bare_dict - promoted
        );
        // The strongest case *for* the push-down: a wide projection, where a `RowSelection` avoids
        // decoding the fat `body`/`attributes` columns for non-matching rows — the saving §5 of the
        // plan actually claims. `count(*)` above barely projects anything, so it would understate it.
        let json_wide = bench(
            &b,
            "wide proj, json_get_str  (indexed)",
            "SELECT count(body) AS c, count(attributes) AS d FROM logs \
             WHERE json_get_str(attributes, 'k') = 'v0'",
        )?;
        let promoted_wide = bench(
            &b,
            "wide proj, CAST(\"k\" AS VARCHAR)  (unpushed)",
            "SELECT count(body) AS c, count(attributes) AS d FROM logs \
             WHERE CAST(\"k\" AS VARCHAR) = 'v0'",
        )?;
        println!(
            "   promoted vs json: {:+.2} ms   json vs floor: {:+.2} ms   wide: promoted vs json {:+.2} ms\n",
            promoted - json,
            json - floor,
            promoted_wide - json_wide
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
    println!("   {label:<52} {best:8.2} ms  ({rows} rows)");
    Ok(best)
}

fn sv(s: &str) -> AnyValue {
    AnyValue {
        value: Some(any_value::Value::StringValue(s.to_owned())),
    }
}

fn logs_body(i: usize, rows: usize, cardinality: usize) -> Vec<u8> {
    let base = i as u64 * 1_000_000;
    let records = (0..rows)
        .map(|j| LogRecord {
            time_unix_nano: base + j as u64,
            severity_number: 9,
            body: Some(sv("request completed ok")),
            attributes: vec![
                KeyValue {
                    key: "k".to_owned(),
                    value: Some(sv(&format!("v{}", j % cardinality))),
                    ..Default::default()
                },
                // Ballast so a row is not trivially cheap to decode — real records carry more than
                // one attribute, and the pruning story is about avoiding exactly this decode work.
                KeyValue {
                    key: "filler".to_owned(),
                    value: Some(sv("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")),
                    ..Default::default()
                },
            ],
            ..Default::default()
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

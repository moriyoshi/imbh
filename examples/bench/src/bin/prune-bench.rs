//! Segment-pruning benchmark: do time-range pruning and the trace-id bloom actually *pay*, not
//! merely fire?
//!
//! The sibling `bench` binary seals once, so it has a single segment and can observe neither. This
//! one builds many segments and then asks for a narrow slice of them, which is the shape both
//! accelerators exist for.
//!
//! Why a synthetic corpus is legitimate *here* (unlike for sigma): both wins are **structural**.
//! Time-range pruning saves in proportion to `segments outside the window / total`, and the bloom in
//! proportion to `segments not holding the probed id / total`. Those ratios are set by retention
//! depth and seal cadence, not by what the attribute values look like — so the shape of the speedup
//! carries, whereas a sigma measured on generated data would only echo the generator.
//!
//! Everything runs through `BlockingDb::sql`, so this file compiles unchanged against a build
//! without the pruning work — which is what makes the A/B against a pristine tree possible.
//!
//! Run: `cargo run --release -p bench --bin prune-bench -- [segments] [rows_per_segment]`

use std::error::Error;
use std::time::Instant;

use imbh::{Db, WalMode};
use opentelemetry_proto::tonic::collector::logs::v1::ExportLogsServiceRequest;
use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
use opentelemetry_proto::tonic::common::v1::{AnyValue, KeyValue, any_value};
use opentelemetry_proto::tonic::logs::v1::{LogRecord, ResourceLogs, ScopeLogs};
use opentelemetry_proto::tonic::resource::v1::Resource;
use opentelemetry_proto::tonic::trace::v1::{ResourceSpans, ScopeSpans, Span};
use prost::Message;

/// Nanoseconds of event time each sealed segment spans. Segment `i` covers `[i*STEP, (i+1)*STEP)`.
const STEP: u64 = 1_000_000;
/// Timed repetitions per query, after one warm-up. The reported figure is the best of these — the
/// least noisy estimator for "how fast can this go", and the one least polluted by page-cache and
/// scheduler jitter.
const REPS: usize = 5;

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = std::env::args().skip(1);
    let segments: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(60);
    let rows: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(2_000);
    // Optional third arg: keep the corpus at this path instead of a temp dir, so `attr-stats` can be
    // pointed at it afterwards. The `shard` key here is perfectly time-localized (one value per
    // segment), which makes this the one corpus in the repo with a genuinely low sigma.
    let keep = args.next();

    println!("imbh segment-pruning bench — {segments} segments x {rows} rows, best of {REPS}\n");

    let tmp = tempfile::tempdir()?;
    let path = match &keep {
        Some(p) => std::path::Path::new(p).to_path_buf(),
        None => tmp.path().to_path_buf(),
    };
    if keep.is_some() {
        std::fs::create_dir_all(&path)?;
    }
    let db = Db::builder(&path).wal(WalMode::Off).open()?;
    let b = db.blocking();

    // ── build the corpus: one flush per batch => one sealed segment per batch ────────────────
    let t = Instant::now();
    for i in 0..segments {
        b.ingest_otlp_logs(&logs_body(i, rows))?;
        b.ingest_otlp_traces(&traces_body(i))?;
        b.flush()?;
    }
    println!(
        "  built {segments} log + {segments} span segments in {:.1} s",
        t.elapsed().as_secs_f64()
    );

    let mid = (segments / 2) as u64;

    // ── logs: time-range pruning ────────────────────────────────────────────────────────────
    // Needs the pristine-tree A/B to attribute: a narrow window and a full scan do different
    // amounts of downstream work, so the ratio below is a floor on the win, not the win itself.
    println!(
        "\nlogs — {} rows across {segments} segments:",
        segments * rows
    );
    let full = bench(&b, "full scan", "SELECT count(*) AS c FROM logs")?;
    let narrow = bench(
        &b,
        "1-segment time window",
        // `CAST("time" AS BIGINT)` is exactly what the typed builders emit (a bare integer literal
        // will not coerce against `Timestamp(ns, UTC)`), so this is the production predicate shape.
        &format!(
            "SELECT count(*) AS c FROM logs \
             WHERE CAST(\"time\" AS BIGINT) >= {} AND CAST(\"time\" AS BIGINT) < {}",
            mid * STEP,
            (mid + 1) * STEP
        ),
    )?;
    println!("  => narrow / full = {:.3}x", narrow / full);

    // ── traces: the bloom, measured in-build ────────────────────────────────────────────────
    // This pair IS self-attributing: both queries return the same rows and do the same downstream
    // work; they differ only in whether the predicate shape lets a bloom rule segments out. That is
    // exactly what the `hex()` -> raw-bytes fix changed, with no second build required.
    println!("\ntraces — {segments} traces, one per segment:");
    let one = hex_of(mid as usize);
    let two = hex_of(((mid + 1) as usize) % segments);

    let raw_point = bench(
        &b,
        "point lookup, raw    trace_id = X'..'",
        &format!("SELECT count(*) AS c FROM spans WHERE trace_id = X'{one}'"),
    )?;
    let hex_point = bench(
        &b,
        "point lookup, hex()  hex(trace_id) = '..'",
        &format!("SELECT count(*) AS c FROM spans WHERE hex(trace_id) = '{one}'"),
    )?;
    println!("  => raw / hex = {:.3}x", raw_point / hex_point);

    let raw_in = bench(
        &b,
        "2-id fetch,   raw    trace_id IN (..)",
        &format!("SELECT count(*) AS c FROM spans WHERE trace_id IN (X'{one}', X'{two}')"),
    )?;
    let hex_in = bench(
        &b,
        "2-id fetch,   hex()  hex(trace_id) IN (..)",
        &format!("SELECT count(*) AS c FROM spans WHERE hex(trace_id) IN ('{one}', '{two}')"),
    )?;
    println!("  => raw / hex = {:.3}x", raw_in / hex_in);

    Ok(())
}

/// Warm up once, then time `REPS` runs and report the best. Returns milliseconds.
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
    println!("  {label:<40} {best:8.2} ms  ({rows} result rows)");
    Ok(best)
}

fn sv(s: &str) -> AnyValue {
    AnyValue {
        value: Some(any_value::Value::StringValue(s.to_owned())),
    }
}

/// A 16-byte trace id unique to segment `i`.
fn trace_id(i: usize) -> [u8; 16] {
    let mut id = [0u8; 16];
    id[..8].copy_from_slice(&(i as u64 + 1).to_be_bytes());
    id[8..].copy_from_slice(&0xA5A5_A5A5_A5A5_A5A5u64.to_be_bytes());
    id
}

/// Uppercase hex of [`trace_id`] — the spelling both `X'..'` literals and `hex()` output use.
fn hex_of(i: usize) -> String {
    trace_id(i).iter().map(|b| format!("{b:02X}")).collect()
}

/// One OTLP/logs body of `rows` records, all inside segment `i`'s time window.
fn logs_body(i: usize, rows: usize) -> Vec<u8> {
    let base = i as u64 * STEP;
    let records = (0..rows)
        .map(|j| LogRecord {
            time_unix_nano: base + (j as u64 % STEP),
            severity_number: 9 + (j % 8) as i32,
            body: Some(sv("request completed ok")),
            attributes: vec![KeyValue {
                key: "shard".to_owned(),
                value: Some(sv(&format!("s{i}"))),
                ..Default::default()
            }],
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

/// One trace (4 spans) living entirely in segment `i`, tagged `shard=s<i>`.
fn traces_body(i: usize) -> Vec<u8> {
    let base = i as u64 * STEP;
    let tid = trace_id(i).to_vec();
    let spans = (0..4u64)
        .map(|j| Span {
            trace_id: tid.clone(),
            span_id: (j + 1 + i as u64 * 8).to_be_bytes().to_vec(),
            name: format!("op-{j}"),
            start_time_unix_nano: base + j,
            end_time_unix_nano: base + j + 500,
            attributes: vec![KeyValue {
                key: "shard".to_owned(),
                value: Some(sv(&format!("s{i}"))),
                ..Default::default()
            }],
            ..Default::default()
        })
        .collect();
    ExportTraceServiceRequest {
        resource_spans: vec![ResourceSpans {
            resource: Some(Resource {
                attributes: vec![KeyValue {
                    key: "service.name".to_owned(),
                    value: Some(sv("cart")),
                    ..Default::default()
                }],
                ..Default::default()
            }),
            scope_spans: vec![ScopeSpans {
                spans,
                ..Default::default()
            }],
            ..Default::default()
        }],
    }
    .encode_to_vec()
}

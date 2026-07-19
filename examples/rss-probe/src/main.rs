//! RSS soak harness — measures imbh's idle and steady resident-set size (OVERVIEW.md §2).
//!
//! The crate-count and binary-size footprint axes are scripted in `scripts/footprint-gate.sh`;
//! this fills in the RSS axis §2 marks *(unmeasured, M1)*. It is a self-contained, hermetic,
//! no-network harness: it opens a `Db` on a fresh tempdir, exercises the ingest→seal→query
//! pipeline, and prints VmRSS before and after.
//!
//! Two phases:
//!   * **idle RSS** — a `Db` open on a tempdir with the default builder, no ingest. This is the
//!     §2 "Idle RSS (DB open, no active writer, empty pool)" number (target ≤ 40 MB / hard 64 MB).
//!   * **steady RSS** — after ingesting `records` OTLP log records, sealing them to a Parquet +
//!     Tantivy segment, and running one representative query. This proxies the §2 "Steady RSS
//!     ingesting 10k log records/s (default buffers)" number (target ≤ 200 MB / hard 320 MB).
//!
//! Record count precedence: `argv[1]` > `RSS_PROBE_RECORDS` env > `DEFAULT_RECORDS`. The default
//! is kept modest so a plain `cargo run -p rss-probe` (debug) and the footprint gate both finish
//! in a few seconds; pass a large count (e.g. `cargo run --release -p rss-probe -- 5000000`) for a
//! real soak.
//!
//! Caveat on the metric: imbh budgets *anonymous* RSS — Tantivy indexes and Parquet segments are
//! mmapped, so their file-backed pages are reclaimable page cache, not anonymous heap (OVERVIEW.md
//! §2). `VmRSS` from `/proc/self/status` includes those file-backed pages, so it is an *upper
//! bound* on the anonymous figure the budgets track — a portable, dependency-free proxy that never
//! understates. Treat the numbers as a ceiling, not the exact anonymous RSS.
//!
//! Run: `cargo run -p rss-probe -- [records]`   (default 100_000)

use std::error::Error;

use imbh::Db;
use opentelemetry_proto::tonic::collector::logs::v1::ExportLogsServiceRequest;
use opentelemetry_proto::tonic::common::v1::{AnyValue, KeyValue, any_value};
use opentelemetry_proto::tonic::logs::v1::{LogRecord, ResourceLogs, ScopeLogs};
use opentelemetry_proto::tonic::resource::v1::Resource;
use prost::Message;

/// Modest default: keeps the gate (debug build, no arg) to a few seconds. Override for a soak.
const DEFAULT_RECORDS: usize = 100_000;
const RECORDS_PER_BODY: usize = 200;
const SERVICES: [&str; 4] = ["cart", "checkout", "search", "payments"];
const BODIES: [&str; 4] = [
    "request completed ok",
    "connection error to upstream",
    "cache miss for key",
    "slow query detected",
];

fn main() -> Result<(), Box<dyn Error>> {
    let requested = std::env::args()
        .nth(1)
        .or_else(|| std::env::var("RSS_PROBE_RECORDS").ok())
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(DEFAULT_RECORDS);
    let bodies = requested.div_ceil(RECORDS_PER_BODY).max(1);
    let records = bodies * RECORDS_PER_BODY;

    // ── Phase 1: idle RSS ────────────────────────────────────────────────────────────────────
    // Default builder (durable, default WAL/pool/budget) — the §2 "DB open, no active writer,
    // empty pool" state. Keep `dir` and `db` alive for the whole run so nothing is reclaimed.
    let dir = tempfile::tempdir()?;
    let db = Db::builder(dir.path()).open()?;
    let b = db.blocking();
    let idle_rss_kib = vm_rss_kib()?;
    println!("phase 1 · idle RSS  (Db open, no ingest): {idle_rss_kib} kiB");

    // ── Phase 2: steady RSS ──────────────────────────────────────────────────────────────────
    println!(
        "phase 2 · ingesting {records} log records ({bodies} OTLP bodies × {RECORDS_PER_BODY}) ..."
    );
    for i in 0..bodies {
        b.ingest_otlp_logs(&logs_body(i))?;
    }
    // Seal the buffer to a Parquet + Tantivy segment so the steady figure reflects the full
    // ingest→segment path, then run one representative query (full-text + filter + aggregate).
    b.flush()?;
    let hits =
        b.sql("SELECT count(*) AS c FROM logs WHERE matches(body, 'error') AND service = 'cart'")?;
    let hit_rows: usize = hits.iter().map(|x| x.num_rows()).sum();

    let steady_rss_kib = vm_rss_kib()?;
    let peak_rss_kib = vm_hwm_kib()?;
    println!(
        "phase 2 · steady RSS (post-seal + query): {steady_rss_kib} kiB  (query returned {hit_rows} row(s))"
    );
    println!("          peak RSS (VmHWM) this run:       {peak_rss_kib} kiB");

    // Compact, machine-readable summary line the footprint gate greps.
    println!("RSS_PROBE idle_kib={idle_rss_kib} steady_kib={steady_rss_kib} records={records}");
    Ok(())
}

/// Parse `VmRSS:` (resident set size, in kiB) from `/proc/self/status`. Linux-only; on other
/// platforms the file is absent and this returns an error the caller surfaces.
fn vm_rss_kib() -> Result<u64, Box<dyn Error>> {
    proc_status_kib("VmRSS:")
}

/// Parse `VmHWM:` (peak resident set size, in kiB) — the high-water mark since process start.
fn vm_hwm_kib() -> Result<u64, Box<dyn Error>> {
    proc_status_kib("VmHWM:")
}

/// Tiny parser for a `Key:\t<value> kB` line in `/proc/self/status`. The value is in kiB despite
/// the `kB` label (a longstanding kernel quirk); we report it unchanged as kiB.
fn proc_status_kib(key: &str) -> Result<u64, Box<dyn Error>> {
    let status = std::fs::read_to_string("/proc/self/status")?;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix(key) {
            let kib = rest
                .split_whitespace()
                .next()
                .ok_or("malformed /proc/self/status line")?
                .parse::<u64>()?;
            return Ok(kib);
        }
    }
    Err(format!("{key} not found in /proc/self/status (Linux only)").into())
}

/// One OTLP/logs body of `RECORDS_PER_BODY` records, varied across services/bodies/severities.
/// Mirrors `examples/bench` so the two harnesses exercise the same ingest shape.
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

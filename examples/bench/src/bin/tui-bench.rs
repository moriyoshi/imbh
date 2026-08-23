//! TUI refresh benchmark: what does one metrics-screen refresh and one traces-screen refresh
//! actually cost, and which term grows with the *corpus* rather than with the *query window*?
//!
//! The sibling benches measure single SQL statements. A TUI screen is not one statement — it is a
//! head operation that reads the metric catalog, translates, and only then queries; or one that
//! searches for candidate traces and then fetches each. Those wrapper costs are the whole point
//! here, so this bench drives `imbh_head::exec` rather than `Db` directly.
//!
//! **Three costs it is built to separate.**
//!
//! 1. *The catalog read.* `exec::promql` reads the whole metric catalog per request, and the
//!    catalog query carries no time predicate — so it scans every metric segment ever written. The
//!    `catalog` line reports `segments_pruned` next to `rows_scanned` to show that nothing is ruled
//!    out; the `promql x1` vs `promql x6` pair shows the multiplier a head pays when it sends one
//!    request per checked metric (which `imbh-tui` does, because a batched answer is unattributable).
//! 2. *The TraceQL per-candidate fetch.* `traces().search()` already fetches the spans of every
//!    candidate it ranks, then keeps only the ids; `execute_traceql` re-fetches each trace with its
//!    own query. Timing `search` alone against the full `traceql` isolates that N+1 exactly — the
//!    difference *is* the re-fetch, since both run the same candidate phase.
//! 3. *Window vs corpus.* Every query here uses the same narrow window while the corpus grows with
//!    `segments`. A cost that tracks `segments` is corpus-driven; one that stays flat is
//!    window-driven and already pruned. `--sweep` runs the whole set at several segment counts so
//!    the two are told apart by shape rather than by argument.
//!
//! Detection is exact where it can be: `ScanStats` comes from `stream_with_stats`, so "the catalog
//! pruned nothing" is a counter, not an inference from the clock.
//!
//! Run: `cargo run --release -p bench --bin tui-bench -- [segments] [metrics] [traces_per_segment]`
//!      `cargo run --release -p bench --bin tui-bench -- --sweep`

use std::error::Error;
use std::sync::Arc;
use std::time::Instant;

use futures::StreamExt;
use imbh::{Db, WalMode};
use imbh_head::dto;
use imbh_head::exec;
use opentelemetry_proto::tonic::collector::metrics::v1::ExportMetricsServiceRequest;
use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
use opentelemetry_proto::tonic::common::v1::{AnyValue, KeyValue, any_value};
use opentelemetry_proto::tonic::metrics::v1::{
    Gauge, Metric, NumberDataPoint, ResourceMetrics, ScopeMetrics, Sum, metric, number_data_point,
};
use opentelemetry_proto::tonic::resource::v1::Resource;
use opentelemetry_proto::tonic::trace::v1::{ResourceSpans, ScopeSpans, Span};
use prost::Message;

/// Nanoseconds of event time each sealed segment spans. Segment `i` covers `[i*STEP, (i+1)*STEP)`.
const STEP: u64 = 60_000_000_000; // 60 s
/// Datapoints per series per segment.
const POINTS_PER_SEGMENT: usize = 60;
/// Spans per generated trace.
const SPANS_PER_TRACE: usize = 8;
/// Timed repetitions per measurement, after one warm-up; the best is reported. Best-of is the least
/// noisy estimator for "how fast can this go" and the one least polluted by page-cache jitter.
const REPS: usize = 3;
/// How many metrics a "several metrics checked" refresh asks for — the `imbh-tui` catalog pane
/// makes one request per checked metric, so this is the fan-out factor.
const FANOUT: usize = 6;
/// Segment counts `--sweep` walks. The interesting axis is corpus size at a *fixed* window.
const SWEEP: [usize; 4] = [10, 40, 160, 640];

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = std::env::args().skip(1).collect::<Vec<_>>();
    let sweep = args.first().is_some_and(|a| a == "--sweep");
    if sweep {
        args.remove(0);
    }
    let parse = |i: usize, default: usize| -> usize {
        args.get(i).and_then(|s| s.parse().ok()).unwrap_or(default)
    };
    let metrics = parse(1, 12);
    let traces = parse(2, 40);

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    if sweep {
        println!(
            "imbh TUI refresh bench — sweeping segments over {SWEEP:?}, \
             {metrics} metrics, {traces} traces/segment, best of {REPS}"
        );
        println!(
            "\nThe window is the SAME for every row: the last {} segments' worth of time. A cost\n\
             that grows down a column is corpus-driven; one that stays flat is window-driven.\n",
            2
        );
        header();
        for segments in SWEEP {
            runtime.block_on(run(segments, metrics, traces, true))?;
        }
        println!(
            "\n  cat-sql — the raw `SELECT DISTINCT`, uncached. `pruned=0`: no time predicate, so\n  \
             \u{20}         its cost is the whole corpus. This is the control.\n  \
             cat-api — the same answer through `metrics().catalog()`, which folds each sealed\n  \
             \u{20}         segment in once and rescans only the buffer. Should stay flat.\n  \
             promql  — x1 is one checked metric. `x6/n` sends {FANOUT} separate requests (the old\n  \
             \u{20}         shape); `x6/1` sends them as one batch. Their ratio is the fan-out cost.\n  \
             tq-search — candidate ranking only. `traceql` re-fetches each candidate on top."
        );
    } else {
        let segments = parse(0, 60);
        println!(
            "imbh TUI refresh bench — {segments} segments, {metrics} metrics, \
             {traces} traces/segment, best of {REPS}\n"
        );
        header();
        runtime.block_on(run(segments, metrics, traces, false))?;
    }
    Ok(())
}

fn header() {
    println!(
        "  {:>8}  {:>10}  {:>8}  {:>8}  {:>9}  {:>9}  {:>9}  {:>9}  {:>9}  {:>9}",
        "segments",
        "rows",
        "cat-sql",
        "cat-api",
        "promql x1",
        "prom x6/n",
        "prom x6/1",
        "tq-search",
        "traceql",
        "trace-get"
    );
}

async fn run(
    segments: usize,
    metrics: usize,
    traces: usize,
    compact: bool,
) -> Result<(), Box<dyn Error>> {
    let tmp = tempfile::tempdir()?;
    let db = Db::builder(tmp.path()).wal(WalMode::Off).open()?;

    // One flush per step => one sealed segment per step, which is what makes the segment count the
    // independent variable rather than an emergent property of the seal policy.
    for i in 0..segments {
        db.ingest_otlp_metrics(&metrics_body(i, metrics)).await?;
        db.ingest_otlp_traces(&traces_body(i, traces)).await?;
        db.flush().await?;
    }

    // The window every query below runs over: the last two segments. Deliberately narrow and
    // *fixed*, so anything that grows with `segments` is grow-with-corpus, not grow-with-window.
    let end_ns = (segments as u64 * STEP) as i64;
    let start_ns = end_ns - (2 * STEP) as i64;

    // Known exactly from the generator's own parameters, so the bench needs no counting query (and
    // no arrow dependency) just to label a row.
    let total_rows = segments * (metrics * POINTS_PER_SEGMENT + traces * SPANS_PER_TRACE);

    // ── the catalog read, with exact scan accounting ────────────────────────────────────────────
    // This is `MetricsApi::catalog`'s own SQL for one of the five tables it visits. Reported through
    // `stream_with_stats` because the counters are the evidence: `segments_pruned == 0` proves the
    // absent time predicate, which no amount of timing could establish on its own.
    let (catalog_sql, scan) = timed_stats(
        &db,
        "SELECT DISTINCT metric, unit, temporality FROM metrics_gauge",
    )
    .await?;

    // The same question asked through the public API, which folds each sealed segment in once and
    // rescans only the mutable buffer. `cat-sql` is the unmitigated control it should diverge from:
    // one grows with the corpus, the other should not.
    let catalog_api = timed_async(|| async {
        let out = db.metrics().catalog().await?;
        Ok::<_, Box<dyn Error>>(out.len())
    })
    .await?;

    // ── one metrics-screen refresh ──────────────────────────────────────────────────────────────
    let window = dto::EvalWindow {
        start_ns,
        end_ns,
        step_ns: 30_000_000_000,
        lookback_ns: 300_000_000_000,
    };
    let caps = dto::EvalCaps {
        max_series: Some(100),
        max_samples: Some(100_000),
        max_traces: Some(100),
        ..Default::default()
    };
    let one = vec![metric_name(0)];
    let many = (0..FANOUT.min(metrics))
        .map(metric_name)
        .collect::<Vec<_>>();

    // x1: a single checked metric. x6: what `imbh-tui` actually sends for six of them — six separate
    // requests, each re-reading the catalog, awaited serially exactly as `fetch.rs` does.
    let promql1 = timed_async(|| promql_serial(&db, &one, window, caps)).await?;
    let promql6 = timed_async(|| promql_serial(&db, &many, window, caps)).await?;
    // The same six metrics as ONE request — what the TUI sends now that each returned series names
    // the query it came from. Locally this saves five catalog reads and five session setups; over
    // `--url` it also collapses six HTTP round trips into one.
    let promql6b = timed_async(|| async {
        let request = dto::EvalRequest {
            queries: many.clone(),
            window,
            caps,
        };
        Ok::<_, Box<dyn Error>>(exec::promql(&db, &request).await?.len())
    })
    .await?;

    // ── one traces-screen refresh ───────────────────────────────────────────────────────────────
    // `search` is the candidate phase alone; `traceql` is that same phase plus the per-candidate
    // re-fetch. Both are timed so the difference attributes the N+1 without a second build.
    let search_query = imbh::TraceQuery::new()
        .trace_start_range_inclusive(imbh::Timestamp(start_ns), imbh::Timestamp(end_ns))
        .limit(100);
    let tq_search = timed_async(|| async {
        let out = db.traces().search(search_query.clone()).await?;
        Ok::<_, Box<dyn Error>>(out.len())
    })
    .await?;

    let request = dto::TraceSearchRequest {
        query: "{}".to_owned(),
        start_ns,
        end_ns,
        caps,
        narrow_steps: 0,
    };
    let traceql = timed_async(|| async {
        let out = exec::traceql(&db, &request).await?;
        Ok::<_, Box<dyn Error>>(out.matches.len())
    })
    .await?;

    // ── one trace fetch: what every cursor move on the traces list costs ────────────────────────
    let first = db.traces().search(search_query.clone()).await?;
    let trace_get = match first.first() {
        Some(summary) => {
            let id = summary.trace_id;
            timed_async(|| async {
                let out = db.traces().get(id).await?;
                Ok::<_, Box<dyn Error>>(out.map(|t| t.spans.len()).unwrap_or(0))
            })
            .await?
        }
        None => f64::NAN,
    };

    println!(
        "  {segments:>8}  {total_rows:>10}  {catalog_sql:>8.2}  {catalog_api:>8.2}  \
         {promql1:>9.2}  {promql6:>9.2}  {promql6b:>9.2}  {tq_search:>9.2}  {traceql:>9.2}  \
         {trace_get:>9.2}"
    );
    if !compact {
        println!(
            "\n  cat-sql scan: rows_scanned={} segments_scanned={} pruned={} bytes={}",
            scan.rows_scanned, scan.segments_scanned, scan.segments_pruned, scan.bytes_scanned
        );
        println!(
            "  cat-api / cat-sql = {:.3}x  (the sealed half is folded once; only the buffer is rescanned)",
            catalog_api / catalog_sql
        );
        println!(
            "  prom x6/n / x6/1 = {:.2}x  (what batching saves locally; over --url add 5 round trips)",
            promql6 / promql6b
        );
        println!(
            "  traceql / tq-search = {:.2}x  (the excess is the per-candidate re-fetch)",
            traceql / tq_search
        );
    }

    db.close().await?;
    Ok(())
}

/// Evaluate `queries` the way `imbh-tui` does: one request apiece, awaited in order. Each one
/// re-reads the metric catalog, which is the cost this shape exists to expose.
async fn promql_serial(
    db: &Arc<Db>,
    queries: &[String],
    window: dto::EvalWindow,
    caps: dto::EvalCaps,
) -> Result<usize, Box<dyn Error>> {
    let mut total = 0;
    for query in queries {
        let request = dto::EvalRequest::one(query.clone(), window, caps);
        total += exec::promql(db, &request).await?.len();
    }
    Ok(total)
}

/// Warm up once, then time `REPS` runs and report the best, in milliseconds.
async fn timed_async<F, Fut, T>(mut make: F) -> Result<f64, Box<dyn Error>>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, Box<dyn Error>>>,
{
    std::hint::black_box(make().await?);
    let mut best = f64::MAX;
    for _ in 0..REPS {
        let t = Instant::now();
        let out = make().await?;
        std::hint::black_box(out);
        best = best.min(t.elapsed().as_secs_f64() * 1e3);
    }
    Ok(best)
}

/// Time one SQL statement and return its read-side scan counters alongside. The counters are
/// complete only once the stream is fully drained, so it is drained.
async fn timed_stats(db: &Arc<Db>, sql: &str) -> Result<(f64, imbh::ScanStats), Box<dyn Error>> {
    let _ = db.sql(sql).collect().await?; // warm the page cache
    let mut best = f64::MAX;
    let mut last = None;
    for _ in 0..REPS {
        let t = Instant::now();
        let (mut stream, stats) = db.sql(sql).stream_with_stats().await?;
        while let Some(batch) = stream.next().await {
            std::hint::black_box(batch?.num_rows());
        }
        best = best.min(t.elapsed().as_secs_f64() * 1e3);
        last = Some(stats.get());
    }
    Ok((best, last.expect("REPS >= 1")))
}

fn metric_name(i: usize) -> String {
    format!("bench_metric_{i:03}")
}

fn sv(s: &str) -> AnyValue {
    AnyValue {
        value: Some(any_value::Value::StringValue(s.to_owned())),
    }
}

fn kv(key: &str, value: &str) -> KeyValue {
    KeyValue {
        key: key.to_owned(),
        value: Some(sv(value)),
        ..Default::default()
    }
}

fn point(time_unix_nano: u64, value: f64, attributes: Vec<KeyValue>) -> NumberDataPoint {
    NumberDataPoint {
        time_unix_nano,
        attributes,
        value: Some(number_data_point::Value::AsDouble(value)),
        ..Default::default()
    }
}

/// One OTLP/metrics body for segment `i`: `count` distinct metric names, half gauge and half sum,
/// each carrying `POINTS_PER_SEGMENT` datapoints inside the segment's window.
///
/// The distinct-name count is what the catalog query has to work through, so it is the knob that
/// makes the catalog read expensive independently of raw row count.
fn metrics_body(i: usize, count: usize) -> Vec<u8> {
    let base = i as u64 * STEP;
    let sample = |j: usize| base + (j as u64 + 1) * (STEP / POINTS_PER_SEGMENT as u64);
    let metrics = (0..count)
        .map(|m| {
            let points = (0..POINTS_PER_SEGMENT)
                .map(|j| {
                    point(
                        sample(j),
                        (i * POINTS_PER_SEGMENT + j) as f64,
                        vec![kv("host", &format!("host-{}", m % 4))],
                    )
                })
                .collect::<Vec<_>>();
            if m % 2 == 0 {
                Metric {
                    name: metric_name(m),
                    unit: "1".to_owned(),
                    data: Some(metric::Data::Gauge(Gauge {
                        data_points: points,
                    })),
                    ..Default::default()
                }
            } else {
                Metric {
                    name: metric_name(m),
                    unit: "1".to_owned(),
                    data: Some(metric::Data::Sum(Sum {
                        data_points: points,
                        aggregation_temporality: 2, // CUMULATIVE
                        is_monotonic: true,
                    })),
                    ..Default::default()
                }
            }
        })
        .collect();
    ExportMetricsServiceRequest {
        resource_metrics: vec![ResourceMetrics {
            resource: Some(Resource {
                attributes: vec![kv("service.name", "cart")],
                ..Default::default()
            }),
            scope_metrics: vec![ScopeMetrics {
                metrics,
                ..Default::default()
            }],
            ..Default::default()
        }],
    }
    .encode_to_vec()
}

/// A 16-byte trace id unique to `(segment, index)`.
fn trace_id(segment: usize, index: usize) -> [u8; 16] {
    let mut id = [0u8; 16];
    id[..8].copy_from_slice(&(segment as u64 + 1).to_be_bytes());
    id[8..].copy_from_slice(&(index as u64 + 1).to_be_bytes());
    id
}

/// `count` traces of [`SPANS_PER_TRACE`] spans each, all inside segment `i`'s window.
fn traces_body(i: usize, count: usize) -> Vec<u8> {
    let base = i as u64 * STEP;
    let spans = (0..count)
        .flat_map(|t| {
            let tid = trace_id(i, t).to_vec();
            (0..SPANS_PER_TRACE).map(move |j| {
                let start = base + (t as u64 * 1_000_000) + j as u64;
                Span {
                    trace_id: tid.clone(),
                    span_id: ((t * SPANS_PER_TRACE + j) as u64 + 1)
                        .to_be_bytes()
                        .to_vec(),
                    parent_span_id: if j == 0 {
                        Vec::new()
                    } else {
                        ((t * SPANS_PER_TRACE) as u64 + 1).to_be_bytes().to_vec()
                    },
                    name: format!("op-{j}"),
                    start_time_unix_nano: start,
                    end_time_unix_nano: start + 500_000,
                    attributes: vec![kv("http.route", "/checkout")],
                    ..Default::default()
                }
            })
        })
        .collect();
    ExportTraceServiceRequest {
        resource_spans: vec![ResourceSpans {
            resource: Some(Resource {
                attributes: vec![kv("service.name", "cart")],
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

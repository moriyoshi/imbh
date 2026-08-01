//! Generate an imbh database populated with demo metrics, traces, and logs.
//!
//! It writes a realistic-looking mix of signals timestamped over the last few minutes up to *now*,
//! then seals them to on-disk segments so a separate process can open the directory read-only — in
//! particular the companion explorer. Each log record carries the trace/span id of the span it
//! describes (a valid, structured correlation rather than an id embedded in the message text), so
//! log↔trace drill-down works against the generated data:
//!
//! ```text
//! cargo run -p gen-demo-db -- ./demo-db            # populate ./demo-db
//! cargo run -p imbh-tui   -- ./demo-db             # explore it (read-only)
//! ```
//!
//! Because the explorer queries relative to wall-clock `now()` with a lookback window, the data is
//! deliberately anchored to the current time. Options:
//!
//! ```text
//! gen-demo-db <dir> [--minutes N] [--step-seconds N] [--deep-hops N]
//! ```
//!
//! `--minutes` (default 15) is how far back the demo history reaches; `--step-seconds` (default 15)
//! is the sample spacing; `--deep-hops` (default 5) is how many service hops the deep trace each step
//! chains together, which sets both its span count and its nesting depth. The random signal *content*
//! comes from a fixed-seed PRNG, but trace and span ids are salted with the run's wall-clock anchor so
//! re-running against an already-populated directory appends fresh, non-colliding traces instead of
//! corrupting the earlier run's.

use std::error::Error;
use std::path::PathBuf;

use imbh::{Db, Timestamp};
use opentelemetry_proto::tonic::collector::logs::v1::ExportLogsServiceRequest;
use opentelemetry_proto::tonic::collector::metrics::v1::ExportMetricsServiceRequest;
use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
use opentelemetry_proto::tonic::common::v1::{AnyValue, KeyValue, any_value};
use opentelemetry_proto::tonic::logs::v1::{LogRecord, ResourceLogs, ScopeLogs};
use opentelemetry_proto::tonic::metrics::v1::{
    Gauge, Histogram, HistogramDataPoint, Metric, NumberDataPoint, ResourceMetrics, ScopeMetrics,
    Sum, metric, number_data_point,
};
use opentelemetry_proto::tonic::resource::v1::Resource;
use opentelemetry_proto::tonic::trace::v1::{ResourceSpans, ScopeSpans, Span, Status};
use prost::Message;

const SERVICES: [&str; 4] = ["cart", "checkout", "search", "payments"];
const HOSTS: [&str; 2] = ["host-a", "host-b"];
const ROUTES: [&str; 4] = ["/cart", "/checkout", "/search", "/pay"];
/// Entry route of the deep trace ([`deep_trace_body`]). Distinct from [`ROUTES`] so the deep trace is
/// instantly recognisable in the explorer's trace list.
const DEEP_ROUTE: &str = "/checkout/payment-authorization";
/// Routes the deep trace's downstream handlers serve, cycled per hop.
const DEEP_HOP_ROUTES: [&str; 4] = [
    "/orders/reserve",
    "/payments/authorize",
    "/ledger/post-entry",
    "/notifications/enqueue",
];
/// Cumulative histogram bucket upper bounds (seconds), shared by every latency histogram.
const LATENCY_BOUNDS: [f64; 7] = [0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5];

fn main() -> Result<(), Box<dyn Error>> {
    let config = Config::from_args()?;

    let now = Timestamp::now().0.max(0) as u64;
    let step_ns = config.step_seconds.max(1) * 1_000_000_000;
    let span_ns = config.minutes.max(1) * 60 * 1_000_000_000;
    let start = now.saturating_sub(span_ns);
    let steps: Vec<u64> = (0..)
        .map(|i| start + i * step_ns)
        .take_while(|&t| t <= now)
        .collect();

    let (n_metrics, n_spans, n_logs) = {
        let db = Db::builder(&config.dir).open()?;
        let b = db.blocking();

        // Per-service running state so counters and cumulative histogram buckets stay monotonic
        // across the whole history, as real CUMULATIVE OTLP series are.
        let mut state = ServiceState::new();
        let mut rng = Rng::new(0x0de0_d0de_cafe_1111);
        // Salt the trace/span id sequence with the run's wall-clock anchor. Otherwise every run
        // reuses ids 1, 2, 3…, so re-running against an already-populated directory appends spans
        // that collide with the previous run's ids (same trace/span id, newer timestamps) and leaves
        // inconsistent traces. Ids remain a deterministic function of the anchor, so a single run is
        // reproducible; distinct runs (distinct `now`) occupy disjoint id space and coexist.
        let mut span_seq: u64 = now.max(1);

        let (mut n_metrics, mut n_spans, mut n_logs) = (0usize, 0usize, 0usize);
        for &t in &steps {
            b.ingest_otlp_metrics(&metrics_body(t, &mut state, &mut rng, &mut n_metrics))?;
            // Generate the traces first, then derive the log records from the spans they emitted so
            // each log carries a valid trace/span id.
            let (traces, spans) =
                traces_body(t, &mut rng, &mut span_seq, &mut n_spans, config.deep_hops);
            b.ingest_otlp_traces(&traces)?;
            b.ingest_otlp_logs(&logs_body(t, &spans, &mut rng, &mut n_logs))?;
        }

        // Seal the buffer to on-disk segments so a separate read-only opener sees everything, then
        // close and drop every handle to release the single-writer lock before we reopen below.
        b.flush()?;
        b.close()?;
        (n_metrics, n_spans, n_logs)
    };

    println!(
        "populated {} with {} metric points, {} spans, {} log records across {} steps ({} min @ {}s)",
        config.dir.display(),
        n_metrics,
        n_spans,
        n_logs,
        steps.len(),
        config.minutes,
        config.step_seconds,
    );

    // Verify the data is durable and queryable through a fresh read-only open — the same entry point
    // the explorer uses.
    let readable = Db::open_read_only(&config.dir)?.blocking();
    for table in [
        "metrics_gauge",
        "metrics_sum",
        "metrics_histogram",
        "spans",
        "logs",
    ] {
        let rows: usize = readable
            .sql(&format!("SELECT count(*) FROM {table}"))?
            .iter()
            .flat_map(|batch| {
                use imbh::arrow::array::AsArray;
                use imbh::arrow::datatypes::Int64Type;
                batch
                    .column(0)
                    .as_primitive_opt::<Int64Type>()
                    .map(|c| c.value(0) as usize)
            })
            .sum();
        println!("  readable {table}: {rows} rows");
    }

    println!(
        "explore it read-only: cargo run -p imbh-tui -- {}",
        config.dir.display()
    );
    Ok(())
}

struct Config {
    dir: PathBuf,
    minutes: u64,
    step_seconds: u64,
    /// Service hops in the deep trace emitted each step (see [`deep_trace_body`]). Each hop adds two
    /// nesting levels and four spans, so this is the knob for how deep and how long that trace is.
    deep_hops: u64,
}

impl Config {
    fn from_args() -> Result<Self, Box<dyn Error>> {
        let mut args = std::env::args_os().skip(1);
        let dir = args
            .next()
            .ok_or("usage: gen-demo-db <dir> [--minutes N] [--step-seconds N] [--deep-hops N]")?;
        let mut config = Config {
            dir: PathBuf::from(dir),
            minutes: 15,
            step_seconds: 15,
            deep_hops: 5,
        };
        while let Some(flag) = args.next() {
            let mut value = || -> Result<u64, Box<dyn Error>> {
                Ok(args
                    .next()
                    .ok_or("flag requires an integer value")?
                    .to_string_lossy()
                    .parse()?)
            };
            match flag.to_string_lossy().as_ref() {
                "--minutes" => config.minutes = value()?,
                "--step-seconds" => config.step_seconds = value()?,
                "--deep-hops" => config.deep_hops = value()?,
                other => return Err(format!("unknown argument: {other}").into()),
            }
        }
        Ok(config)
    }
}

/// Per-service accumulators that must not decrease across the generated history.
struct ServiceState {
    /// Cumulative request counter per service.
    requests: [f64; SERVICES.len()],
    /// Cumulative histogram bucket counts per service (`LATENCY_BOUNDS.len() + 1` buckets each).
    buckets: [[u64; LATENCY_BOUNDS.len() + 1]; SERVICES.len()],
}

impl ServiceState {
    fn new() -> Self {
        Self {
            requests: [0.0; SERVICES.len()],
            buckets: [[0; LATENCY_BOUNDS.len() + 1]; SERVICES.len()],
        }
    }
}

// ── metrics ────────────────────────────────────────────────────────────────────────────────────

/// One OTLP/metrics body for timestamp `t`: a `cpu_utilization` gauge per (service, host), a
/// monotonic cumulative `http_requests_total` counter per service, and a cumulative
/// `request_duration_seconds` histogram per service.
fn metrics_body(t: u64, state: &mut ServiceState, rng: &mut Rng, count: &mut usize) -> Vec<u8> {
    let resource_metrics = SERVICES
        .iter()
        .enumerate()
        .map(|(si, service)| {
            let mut metrics = Vec::new();

            // Gauge: cpu utilization in [0, 1], one series per host.
            let gauge_points = HOSTS
                .iter()
                .map(|host| {
                    *count += 1;
                    number_point(t, 0.25 + 0.5 * rng.unit(), vec![kv("host", str_val(host))])
                })
                .collect();
            metrics.push(Metric {
                name: "cpu_utilization".to_owned(),
                unit: "1".to_owned(),
                data: Some(metric::Data::Gauge(Gauge {
                    data_points: gauge_points,
                })),
                ..Default::default()
            });

            // Sum: cumulative, monotonic request counter.
            state.requests[si] += 20.0 + 60.0 * rng.unit();
            *count += 1;
            metrics.push(Metric {
                name: "http_requests_total".to_owned(),
                unit: "1".to_owned(),
                data: Some(metric::Data::Sum(Sum {
                    data_points: vec![number_point(t, state.requests[si], Vec::new())],
                    aggregation_temporality: 2, // CUMULATIVE
                    is_monotonic: true,
                })),
                ..Default::default()
            });

            // Histogram: cumulative latency distribution. Advance each bucket by a random increment
            // so the cumulative counts stay monotonic, matching real CUMULATIVE histograms.
            for bucket in &mut state.buckets[si] {
                *bucket += rng.below(6);
            }
            let counts = state.buckets[si];
            let total: u64 = counts.iter().sum();
            *count += 1;
            metrics.push(Metric {
                name: "request_duration_seconds".to_owned(),
                unit: "s".to_owned(),
                data: Some(metric::Data::Histogram(Histogram {
                    data_points: vec![HistogramDataPoint {
                        time_unix_nano: t,
                        start_time_unix_nano: t.saturating_sub(1_000_000_000),
                        count: total,
                        sum: Some(total as f64 * 0.08),
                        explicit_bounds: LATENCY_BOUNDS.to_vec(),
                        bucket_counts: counts.to_vec(),
                        ..Default::default()
                    }],
                    aggregation_temporality: 2, // CUMULATIVE
                })),
                ..Default::default()
            });

            ResourceMetrics {
                resource: Some(service_resource(service)),
                scope_metrics: vec![ScopeMetrics {
                    metrics,
                    ..Default::default()
                }],
                ..Default::default()
            }
        })
        .collect();

    ExportMetricsServiceRequest { resource_metrics }.encode_to_vec()
}

fn number_point(t: u64, value: f64, attributes: Vec<KeyValue>) -> NumberDataPoint {
    NumberDataPoint {
        time_unix_nano: t,
        value: Some(number_data_point::Value::AsDouble(value)),
        attributes,
        ..Default::default()
    }
}

// ── traces ─────────────────────────────────────────────────────────────────────────────────────

/// The role a span plays in the generated call tree. Drives the log body/attributes derived from it
/// and documents the shape of the tree in one place.
#[derive(Clone, Copy)]
enum Role {
    /// Root SERVER span at the entry service (`GET {route}`).
    Entry,
    /// INTERNAL authorization check, a leaf under the entry span.
    Authz,
    /// CLIENT span at the entry service issuing the downstream call.
    Rpc,
    /// SERVER span at the downstream service, child of the [`Role::Rpc`] span (cross-service).
    Downstream,
    /// CLIENT `db.query` span (either the entry service's own query or the downstream's).
    Db,
    /// INTERNAL `cache.get` span under the downstream span.
    Cache,
}

/// A span emitted in a step, retained so the step's log records can carry a valid trace/span id
/// correlation back to it.
struct SpanRef {
    trace_id: Vec<u8>,
    span_id: Vec<u8>,
    service: &'static str,
    /// The HTTP route of the SERVER span this span belongs to (entry route for entry-service spans,
    /// downstream route for downstream-service spans). Used for the log record's `http.route`.
    route: &'static str,
    role: Role,
    is_error: bool,
}

/// The fields that vary per span; the constant plumbing (trace id, status, timing shape) is filled
/// in by [`build_span`]. Keeps the per-span construction to one named-field literal instead of a
/// nine-argument call.
struct SpanSpec {
    id: Vec<u8>,
    /// Empty for the root span.
    parent: Vec<u8>,
    name: String,
    /// OTLP `SpanKind`: 1 INTERNAL, 2 SERVER, 3 CLIENT.
    kind: i32,
    start: u64,
    dur: u64,
    is_error: bool,
    attributes: Vec<KeyValue>,
}

fn build_span(trace: &[u8], spec: SpanSpec) -> Span {
    Span {
        trace_id: trace.to_vec(),
        span_id: spec.id,
        parent_span_id: spec.parent,
        name: spec.name,
        kind: spec.kind,
        start_time_unix_nano: spec.start,
        end_time_unix_nano: spec.start + spec.dur,
        status: Some(status_for(spec.is_error)),
        attributes: spec.attributes,
        ..Default::default()
    }
}

/// Consume and return the next id-sequence value.
fn bump(seq: &mut u64) -> u64 {
    let n = *seq;
    *seq += 1;
    n
}

/// A sub-window `[start + dur*start_pct%, …]` lasting `dur*dur_pct%`, used to place a child span
/// strictly inside its parent's window (callers keep `start_pct + dur_pct <= 100`).
fn nest(start: u64, dur: u64, start_pct: u64, dur_pct: u64) -> (u64, u64) {
    (start + dur * start_pct / 100, (dur * dur_pct / 100).max(1))
}

/// One OTLP/traces body for timestamp `t`: a few multi-level, cross-service traces. Each trace is a
/// root SERVER span at an entry service with three children — an INTERNAL `authz.check`, a CLIENT
/// `rpc {downstream}` that fans out into a *second* SERVER span at a different service (which itself
/// has `db.query` and `cache.get` children), and the entry service's own CLIENT `db.query`:
///
/// ```text
/// GET {route}            SERVER    entry
/// ├─ authz.check         INTERNAL  entry
/// ├─ rpc {downstream}    CLIENT    entry
/// │  └─ GET {ds_route}   SERVER    downstream
/// │     ├─ db.query      CLIENT    downstream
/// │     └─ cache.get     INTERNAL  downstream
/// └─ db.query            CLIENT    entry
/// ```
///
/// Roughly one trace in eight fails: the failure originates at the downstream `db.query` and its
/// ERROR status propagates up the calling path (downstream SERVER → rpc → entry), while the sibling
/// branches stay OK — as a real bubbled-up error would. Returns the encoded body along with a
/// [`SpanRef`] per emitted span so `logs_body` can correlate log records to real spans.
///
/// Each step also emits one deep, span-heavy trace ([`deep_trace_body`]) alongside these three.
fn traces_body(
    t: u64,
    rng: &mut Rng,
    span_seq: &mut u64,
    count: &mut usize,
    deep_hops: u64,
) -> (Vec<u8>, Vec<SpanRef>) {
    const TRACES_PER_STEP: usize = 3;
    const SPANS_PER_TRACE: usize = 7;
    let mut resource_spans = Vec::with_capacity(TRACES_PER_STEP * 2 + SERVICES.len());
    let mut refs = Vec::with_capacity(TRACES_PER_STEP * SPANS_PER_TRACE + 4 * deep_hops as usize);
    for _ in 0..TRACES_PER_STEP {
        let entry_idx = rng.below(SERVICES.len() as u64) as usize;
        let entry_service = SERVICES[entry_idx];
        // Pick a distinct downstream service so the trace genuinely spans two resources.
        let downstream_service = SERVICES
            [(entry_idx + 1 + rng.below(SERVICES.len() as u64 - 1) as usize) % SERVICES.len()];
        let route = ROUTES[rng.below(ROUTES.len() as u64) as usize];
        let ds_route = ROUTES[rng.below(ROUTES.len() as u64) as usize];
        let is_error = rng.below(8) == 0;

        let trace = trace_id(bump(span_seq));
        let entry_id = span_id(bump(span_seq));
        let authz_id = span_id(bump(span_seq));
        let rpc_id = span_id(bump(span_seq));
        let ds_id = span_id(bump(span_seq));
        let ds_db_id = span_id(bump(span_seq));
        let cache_id = span_id(bump(span_seq));
        let entry_db_id = span_id(bump(span_seq));

        // Lay out each span's window strictly inside its parent's so the tree is time-consistent.
        let root_dur = 20_000_000 + rng.below(120_000_000); // 20–140 ms
        let (authz_start, authz_dur) = nest(t, root_dur, 2, 8);
        let (rpc_start, rpc_dur) = nest(t, root_dur, 12, 60);
        let (ds_start, ds_dur) = nest(rpc_start, rpc_dur, 3, 90);
        let (ds_db_start, ds_db_dur) = nest(ds_start, ds_dur, 5, 55);
        let (cache_start, cache_dur) = nest(ds_start, ds_dur, 62, 30);
        let (entry_db_start, entry_db_dur) = nest(t, root_dur, 78, 18);

        let entry_spans = vec![
            build_span(
                &trace,
                SpanSpec {
                    id: entry_id.clone(),
                    parent: Vec::new(),
                    name: format!("GET {route}"),
                    kind: 2, // SERVER
                    start: t,
                    dur: root_dur,
                    is_error,
                    attributes: vec![
                        kv("http.route", str_val(route)),
                        kv("http.method", str_val("GET")),
                    ],
                },
            ),
            build_span(
                &trace,
                SpanSpec {
                    id: authz_id.clone(),
                    parent: entry_id.clone(),
                    name: "authz.check".to_owned(),
                    kind: 1, // INTERNAL
                    start: authz_start,
                    dur: authz_dur,
                    is_error: false,
                    attributes: vec![kv("authz.decision", str_val("allow"))],
                },
            ),
            build_span(
                &trace,
                SpanSpec {
                    id: rpc_id.clone(),
                    parent: entry_id.clone(),
                    name: format!("rpc {downstream_service}"),
                    kind: 3, // CLIENT
                    start: rpc_start,
                    dur: rpc_dur,
                    is_error,
                    attributes: vec![kv("rpc.service", str_val(downstream_service))],
                },
            ),
            build_span(
                &trace,
                SpanSpec {
                    id: entry_db_id.clone(),
                    parent: entry_id.clone(),
                    name: "db.query".to_owned(),
                    kind: 3, // CLIENT
                    start: entry_db_start,
                    dur: entry_db_dur,
                    is_error: false,
                    attributes: vec![kv("db.system", str_val("postgresql"))],
                },
            ),
        ];
        let downstream_spans = vec![
            build_span(
                &trace,
                SpanSpec {
                    id: ds_id.clone(),
                    parent: rpc_id.clone(),
                    name: format!("GET {ds_route}"),
                    kind: 2, // SERVER
                    start: ds_start,
                    dur: ds_dur,
                    is_error,
                    attributes: vec![kv("http.route", str_val(ds_route))],
                },
            ),
            build_span(
                &trace,
                SpanSpec {
                    id: ds_db_id.clone(),
                    parent: ds_id.clone(),
                    name: "db.query".to_owned(),
                    kind: 3, // CLIENT
                    start: ds_db_start,
                    dur: ds_db_dur,
                    is_error,
                    attributes: vec![
                        kv("db.system", str_val("postgresql")),
                        kv(
                            "db.statement",
                            str_val("SELECT * FROM orders WHERE id = $1"),
                        ),
                    ],
                },
            ),
            build_span(
                &trace,
                SpanSpec {
                    id: cache_id.clone(),
                    parent: ds_id.clone(),
                    name: "cache.get".to_owned(),
                    kind: 1, // INTERNAL
                    start: cache_start,
                    dur: cache_dur,
                    is_error: false,
                    attributes: vec![kv("cache.system", str_val("redis"))],
                },
            ),
        ];
        *count += SPANS_PER_TRACE;

        refs.push(span_ref(
            &trace,
            entry_id,
            entry_service,
            route,
            Role::Entry,
            is_error,
        ));
        refs.push(span_ref(
            &trace,
            authz_id,
            entry_service,
            route,
            Role::Authz,
            false,
        ));
        refs.push(span_ref(
            &trace,
            rpc_id,
            entry_service,
            route,
            Role::Rpc,
            is_error,
        ));
        refs.push(span_ref(
            &trace,
            entry_db_id,
            entry_service,
            route,
            Role::Db,
            false,
        ));
        refs.push(span_ref(
            &trace,
            ds_id,
            downstream_service,
            ds_route,
            Role::Downstream,
            is_error,
        ));
        refs.push(span_ref(
            &trace,
            ds_db_id,
            downstream_service,
            ds_route,
            Role::Db,
            is_error,
        ));
        refs.push(span_ref(
            &trace,
            cache_id,
            downstream_service,
            ds_route,
            Role::Cache,
            false,
        ));

        resource_spans.push(ResourceSpans {
            resource: Some(service_resource(entry_service)),
            scope_spans: vec![ScopeSpans {
                spans: entry_spans,
                ..Default::default()
            }],
            ..Default::default()
        });
        resource_spans.push(ResourceSpans {
            resource: Some(service_resource(downstream_service)),
            scope_spans: vec![ScopeSpans {
                spans: downstream_spans,
                ..Default::default()
            }],
            ..Default::default()
        });
    }

    let (deep_spans, deep_refs) = deep_trace_body(t, rng, span_seq, count, deep_hops);
    resource_spans.extend(deep_spans);
    refs.extend(deep_refs);

    (
        ExportTraceServiceRequest { resource_spans }.encode_to_vec(),
        refs,
    )
}

/// One deep, span-heavy trace: a checkout entry point that fans out locally and then chains
/// `hops` service-to-service calls, each nesting two levels deeper than the last:
///
/// ```text
/// POST /checkout/payment-authorization  SERVER    entry  depth 0
/// ├─ authz.check.entitlements           INTERNAL  entry  depth 1
/// ├─ cart.load.line-items               INTERNAL  entry  depth 1
/// ├─ inventory.check.availability       INTERNAL  entry  depth 1
/// ├─ cache.get.customer-session         INTERNAL  entry  depth 1
/// ├─ db.query.checkout-summary          CLIENT    entry  depth 1
/// └─ rpc svc-1                          CLIENT    entry  depth 1
///    └─ POST /hop-1                     SERVER    svc-1  depth 2
///       ├─ db.query.orders-by-customer  CLIENT    svc-1  depth 3
///       ├─ cache.get.customer-session   INTERNAL  svc-1  depth 3
///       └─ rpc svc-2                    CLIENT    svc-1  depth 3
///          └─ POST /hop-2               SERVER    svc-2  depth 4   … and so on, `hops` times
/// ```
///
/// At the default 5 hops that is 23 spans nested 11 deep — far more than a terminal pane shows at
/// once, which is the point: it is the fixture for the trace detail's sticky waterfall, where the
/// enclosing spans stay pinned as you scroll into the leaves. The names are deliberately longer than
/// the waterfall's name column so its horizontal scrolling has something to reveal.
///
/// Every span reuses an existing [`Role`], so `logs_body` correlates log records to these spans with
/// no special casing. Roughly one deep trace in four fails at the *innermost* `db.query`, with ERROR
/// propagating up the whole chain — the case where pinned ancestors matter most.
fn deep_trace_body(
    t: u64,
    rng: &mut Rng,
    span_seq: &mut u64,
    count: &mut usize,
    hops: u64,
) -> (Vec<ResourceSpans>, Vec<SpanRef>) {
    let entry_idx = rng.below(SERVICES.len() as u64) as usize;
    let entry_service = SERVICES[entry_idx];
    let is_error = rng.below(4) == 0;
    let trace = trace_id(bump(span_seq));

    // The chain revisits services once it is longer than SERVICES, so spans are collected with their
    // service and grouped into one ResourceSpans each at the end rather than per-hop.
    let mut spans: Vec<(&'static str, Span)> = Vec::new();
    let mut refs = Vec::new();

    let root_id = span_id(bump(span_seq));
    let root_dur = 200_000_000 + rng.below(100_000_000); // 200–300 ms
    spans.push((
        entry_service,
        build_span(
            &trace,
            SpanSpec {
                id: root_id.clone(),
                parent: Vec::new(),
                name: format!("POST {DEEP_ROUTE}"),
                kind: 2, // SERVER
                start: t,
                dur: root_dur,
                is_error,
                attributes: vec![
                    kv("http.route", str_val(DEEP_ROUTE)),
                    kv("http.method", str_val("POST")),
                ],
            },
        ),
    ));
    refs.push(span_ref(
        &trace,
        root_id.clone(),
        entry_service,
        DEEP_ROUTE,
        Role::Entry,
        is_error,
    ));

    // The entry service's own leaves, so the root has enough children to fill a pane on its own.
    for (offset, (name, kind, role)) in [
        ("authz.check.entitlements", 1, Role::Authz),
        ("cart.load.line-items", 1, Role::Authz),
        ("inventory.check.availability", 1, Role::Authz),
        ("cache.get.customer-session", 1, Role::Cache),
        ("db.query.checkout-summary", 3, Role::Db),
    ]
    .into_iter()
    .enumerate()
    {
        let id = span_id(bump(span_seq));
        let (start, dur) = nest(t, root_dur, 2 + offset as u64 * 3, 8);
        spans.push((
            entry_service,
            build_span(
                &trace,
                SpanSpec {
                    id: id.clone(),
                    parent: root_id.clone(),
                    name: name.to_owned(),
                    kind,
                    start,
                    dur,
                    is_error: false,
                    attributes: vec![kv("code.namespace", str_val(name))],
                },
            ),
        ));
        refs.push(span_ref(&trace, id, entry_service, DEEP_ROUTE, role, false));
    }

    // The hop chain. Each hop is a CLIENT `rpc` under the previous handler plus the SERVER handler it
    // calls, so every hop nests two levels deeper. `nest` keeps each window inside its parent's; at
    // 90 %/94 % per hop a five-hop chain still retains ~45 % of the root, so no bar collapses to the
    // one-cell floor the waterfall renderer clamps to.
    let (mut parent_id, mut parent_start, mut parent_dur) = (root_id, t, root_dur);
    for hop in 1..=hops {
        let service = SERVICES[(entry_idx + hop as usize) % SERVICES.len()];
        let route: &'static str = DEEP_HOP_ROUTES[(hop as usize - 1) % DEEP_HOP_ROUTES.len()];
        let innermost = hop == hops;

        let rpc_id = span_id(bump(span_seq));
        let (rpc_start, rpc_dur) = nest(parent_start, parent_dur, 5, 90);
        spans.push((
            SERVICES[(entry_idx + hop as usize - 1) % SERVICES.len()],
            build_span(
                &trace,
                SpanSpec {
                    id: rpc_id.clone(),
                    parent: parent_id,
                    name: format!("rpc {service}.checkout-service"),
                    kind: 3, // CLIENT
                    start: rpc_start,
                    dur: rpc_dur,
                    is_error,
                    attributes: vec![kv("rpc.service", str_val(service))],
                },
            ),
        ));
        refs.push(span_ref(
            &trace,
            rpc_id.clone(),
            SERVICES[(entry_idx + hop as usize - 1) % SERVICES.len()],
            route,
            Role::Rpc,
            is_error,
        ));

        let handler_id = span_id(bump(span_seq));
        let (handler_start, handler_dur) = nest(rpc_start, rpc_dur, 3, 94);
        spans.push((
            service,
            build_span(
                &trace,
                SpanSpec {
                    id: handler_id.clone(),
                    parent: rpc_id,
                    name: format!("POST {route}"),
                    kind: 2, // SERVER
                    start: handler_start,
                    dur: handler_dur,
                    is_error,
                    attributes: vec![kv("http.route", str_val(route))],
                },
            ),
        ));
        refs.push(span_ref(
            &trace,
            handler_id.clone(),
            service,
            route,
            Role::Downstream,
            is_error,
        ));

        // Two leaves under each handler. Only the innermost hop's query is the failure's origin.
        for (name, kind, role, failing, start_pct, dur_pct) in [
            (
                "db.query.orders-by-customer",
                3,
                Role::Db,
                innermost && is_error,
                6u64,
                30u64,
            ),
            ("cache.get.customer-session", 1, Role::Cache, false, 40, 30),
        ] {
            let id = span_id(bump(span_seq));
            let (start, dur) = nest(handler_start, handler_dur, start_pct, dur_pct);
            spans.push((
                service,
                build_span(
                    &trace,
                    SpanSpec {
                        id: id.clone(),
                        parent: handler_id.clone(),
                        name: name.to_owned(),
                        kind,
                        start,
                        dur,
                        is_error: failing,
                        attributes: vec![kv("code.namespace", str_val(name))],
                    },
                ),
            ));
            refs.push(span_ref(&trace, id, service, route, role, failing));
        }

        parent_id = handler_id;
        parent_start = handler_start;
        parent_dur = handler_dur;
    }

    *count += spans.len();

    // Group by service: one ResourceSpans per service that appears anywhere in the chain.
    let resource_spans = SERVICES
        .iter()
        .filter_map(|service| {
            let owned: Vec<Span> = spans
                .iter()
                .filter(|(name, _)| name == service)
                .map(|(_, span)| span.clone())
                .collect();
            (!owned.is_empty()).then(|| ResourceSpans {
                resource: Some(service_resource(service)),
                scope_spans: vec![ScopeSpans {
                    spans: owned,
                    ..Default::default()
                }],
                ..Default::default()
            })
        })
        .collect();
    (resource_spans, refs)
}

fn span_ref(
    trace: &[u8],
    span_id: Vec<u8>,
    service: &'static str,
    route: &'static str,
    role: Role,
    is_error: bool,
) -> SpanRef {
    SpanRef {
        trace_id: trace.to_vec(),
        span_id,
        service,
        route,
        role,
        is_error,
    }
}

fn status_for(is_error: bool) -> Status {
    if is_error {
        Status {
            code: 2, // ERROR
            message: "upstream timeout".to_owned(),
        }
    } else {
        Status {
            code: 1, // OK
            message: String::new(),
        }
    }
}

// ── logs ───────────────────────────────────────────────────────────────────────────────────────

/// One OTLP/logs body for timestamp `t`, derived from the spans emitted in the same step: one
/// record per span, carrying that span's `trace_id`/`span_id` so the log correlates back to a real
/// span. Records are grouped under the span's service resource; error spans yield ERROR bodies
/// containing "error" so the full-text path still has something to match.
fn logs_body(t: u64, spans: &[SpanRef], rng: &mut Rng, count: &mut usize) -> Vec<u8> {
    let mut resource_logs = Vec::new();
    for service in SERVICES.iter() {
        let mut records = Vec::new();
        for span in spans.iter().filter(|s| s.service == *service) {
            let (body, severity) = log_line(span, rng);
            // Each log carries an attribute mirroring the span it describes.
            let attr = match span.role {
                Role::Entry | Role::Downstream => kv("http.route", str_val(span.route)),
                Role::Rpc => kv("rpc.system", str_val("grpc")),
                Role::Db => kv("db.system", str_val("postgresql")),
                Role::Authz => kv("authz.decision", str_val("allow")),
                Role::Cache => kv("cache.system", str_val("redis")),
            };
            *count += 1;
            records.push(LogRecord {
                // Spread records within the step so timestamps are distinct.
                time_unix_nano: t + records.len() as u64 * 1_000_000,
                severity_number: severity,
                body: Some(str_val(body)),
                attributes: vec![attr],
                trace_id: span.trace_id.clone(),
                span_id: span.span_id.clone(),
                ..Default::default()
            });
        }
        if !records.is_empty() {
            resource_logs.push(ResourceLogs {
                resource: Some(service_resource(service)),
                scope_logs: vec![ScopeLogs {
                    log_records: records,
                    ..Default::default()
                }],
                ..Default::default()
            });
        }
    }

    ExportLogsServiceRequest { resource_logs }.encode_to_vec()
}

/// Body text + OTLP severity number for a log line describing `span`, chosen by the span's role so
/// the message reads like it came from that layer. Spans on the failing path (`is_error`) produce an
/// ERROR line containing "error"/"failed" so the full-text path still has something to match; the
/// rest produce INFO/WARN variety.
fn log_line(span: &SpanRef, rng: &mut Rng) -> (&'static str, i32) {
    match span.role {
        Role::Entry => {
            if span.is_error {
                ("request failed: upstream error", 17) // ERROR
            } else if rng.below(6) == 0 {
                ("slow request detected", 13) // WARN
            } else {
                ("request completed ok", 9) // INFO
            }
        }
        Role::Rpc => {
            if span.is_error {
                ("downstream call failed", 17) // ERROR
            } else {
                ("downstream call ok", 9) // INFO
            }
        }
        Role::Downstream => {
            if span.is_error {
                ("handler error: query failed", 17) // ERROR
            } else {
                ("handler completed ok", 9) // INFO
            }
        }
        Role::Db => {
            if span.is_error {
                ("query failed: connection reset", 17) // ERROR
            } else if rng.below(3) == 0 {
                ("slow query detected", 13) // WARN
            } else {
                ("query ok", 9) // INFO
            }
        }
        Role::Authz => ("authorization granted", 9), // INFO
        Role::Cache => ("cache miss for key", 9),    // INFO
    }
}

// ── shared helpers ───────────────────────────────────────────────────────────────────────────────

fn service_resource(service: &str) -> Resource {
    Resource {
        attributes: vec![kv("service.name", str_val(service))],
        ..Default::default()
    }
}

fn kv(key: &str, value: AnyValue) -> KeyValue {
    KeyValue {
        key: key.to_owned(),
        value: Some(value),
        ..Default::default()
    }
}

fn str_val(s: &str) -> AnyValue {
    AnyValue {
        value: Some(any_value::Value::StringValue(s.to_owned())),
    }
}

/// A 16-byte non-zero trace id derived deterministically from a sequence number.
fn trace_id(n: u64) -> Vec<u8> {
    let mut id = vec![0u8; 16];
    id[..8].copy_from_slice(&n.to_be_bytes());
    id[8..].copy_from_slice(&n.wrapping_mul(0x9E37_79B9_7F4A_7C15).to_be_bytes());
    id
}

/// An 8-byte non-zero span id derived deterministically from a sequence number.
fn span_id(n: u64) -> Vec<u8> {
    n.wrapping_mul(0xD1B5_4A32_D192_ED03).to_be_bytes().to_vec()
}

/// A small, dependency-free deterministic PRNG (splitmix64) so runs are reproducible without pulling
/// in a `rand` subtree.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Rng(seed)
    }

    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform integer in `[0, n)`; returns 0 if `n == 0`.
    fn below(&mut self, n: u64) -> u64 {
        if n == 0 { 0 } else { self.next_u64() % n }
    }

    /// Uniform float in `[0, 1)`.
    fn unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }
}

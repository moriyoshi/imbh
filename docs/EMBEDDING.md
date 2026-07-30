# Embedding imbh

imbh is a library first (OVERVIEW.md §1). You link it into your process, feed it OTLP, and query it
through typed Rust APIs or SQL. This guide covers the common host-integration paths. The canonical
API reference is `ARCHITECTURE.md` §10; a runnable end-to-end version of everything below is
`examples/embed-in-app`.

## Open a database

```rust
use imbh::{Db, MemoryBudget, Retention, WalMode};

// Durable, on disk:
let db = Db::builder("./telemetry")
    .memory_budget(MemoryBudget::total(128 << 20)) // caps buffer + query pool + writer heap
    .wal(WalMode::Interval(std::time::Duration::from_secs(1)))
    .retention(Retention::days(7).max_disk_bytes(20 << 30))
    .open()?;

// Ephemeral, in-process (no WAL, no segments):
let db = Db::in_memory().open()?;
```

`open()` / `open_read_only()` hand back an `Arc<Db>`, and `Db` is `Send + Sync` — clone the `Arc` to
share one handle across your app. (`Db` itself is not `Clone`; the typed-query namespaces take
`self: &Arc<Self>` so they can keep an owned `'static` share.)

## Ingest

Feed protobuf OTLP export-request bytes (what any OTLP/HTTP exporter sends):

```rust
db.ingest_otlp_logs(&bytes).await?;     // ExportLogsServiceRequest
db.ingest_otlp_traces(&bytes).await?;   // ExportTraceServiceRequest
db.ingest_otlp_metrics(&bytes).await?;  // ExportMetricsServiceRequest (gauge, sum, histogram, exp-histogram, summary)
```

`ingest_*` awaits until the batch is *accepted* (buffered + WAL-appended); under
`WalMode::Always` it also fsyncs, so `receipt.durable == true`. `try_ingest_*` never blocks and
never fsyncs. Confirm durability with `db.durable_through().await >= receipt.lsn` or force it with
`db.flush().await`.

**Self-observation via the SDK exporter** (optional `imbh-otel-exporter` crate): wire an in-process
`opentelemetry_sdk` pipeline to export straight into imbh — no collector, no network hop. Adapters
cover all three signals — traces (`ImbhSpanExporter`), logs (`ImbhLogExporter`), and metrics
(`ImbhMetricExporter`, cumulative by default; `.with_temporality(Temporality::Delta)` to switch) —
each converting the SDK batch to OTLP and hitting the same `ingest_otlp_*` path:

```rust
use opentelemetry_sdk::trace::{SdkTracerProvider, SimpleSpanProcessor};
let provider = SdkTracerProvider::builder()
    .with_span_processor(SimpleSpanProcessor::new(imbh_otel_exporter::ImbhSpanExporter::new(db.clone())))
    .build();
// …emit spans through `provider`; they land in `db`, queryable via the `spans` table.
```

Ingested data is queryable immediately — queries see the mutable buffer unioned with sealed
segments, so there is no flush latency before a row shows up.

## Query

**Typed APIs** (eager; endpoint-shaped, mirroring Loki/Tempo/Mimir):

```rust
// Logs — filter/search, paginate, and break volume down by label:
let page  = db.logs().query(LogQuery::new().service("checkout").matches("timeout error")
                .attr_exists("http.route").limit(100)).await?;
if let Some(cursor) = page.next { /* db.logs().query(LogQuery::new()….after(cursor)) */ }
let vol   = db.logs().volume_by(LogQuery::new(), secs(60), &["http.route"]).await?;
// Attribute matchers (all repeatable, AND-combined; the same set is on TraceQuery):
//   .attr_eq(k, v) .attr_exists(k) .attr_matches(k, text)     // equals / present / term-search
//   .attr_in(k, &["a","b"]) .attr_not_in(k, &["/health"])     // in-set / NULL-aware set-exclusion
let errs  = db.logs().query(LogQuery::new()
                .attr_in("http.status_class", &["4xx", "5xx"])
                .attr_not_in("http.route", &["/health", "/metrics"])).await?;

// Traces — assemble, search (name text + attrs), and RED metrics:
let trace = db.traces().get(trace_id).await?;
let slow  = db.traces().search(TraceQuery::new().min_duration(ms(500)).matches("checkout")).await?;
let red   = db.traces().span_metrics(SpanMetricsQuery::new().service("cart")
                .group_by("http.route").step(secs(60))).await?; // calls/errors/p50·p95·p99

// Metrics — catalog, range/instant, per-second rate, histogram quantiles, series discovery:
let cat   = db.metrics().catalog().await?;                                   // (metric, unit, kind)
let g     = db.metrics().range(MetricQuery::gauge("cpu").group_by("host").step(secs(60))).await?;
let qps   = db.metrics().range(MetricQuery::sum("requests").rate().step(secs(60))).await?; // delta
let bps   = db.metrics().range(MetricQuery::sum("bytes").rate_counter().step(secs(60))).await?; // cumulative
let p95   = db.metrics().histogram_quantile(HistogramQuery::new("http.duration")
                .quantile(0.95).group_by("route").step(secs(60))).await?;   // explicit-bucket
let ep99  = db.metrics().exp_histogram_quantile(ExpHistogramQuery::new("http.duration")
                .quantile(0.99)).await?;                                     // exponential
let series = db.metrics().series("http.duration").await?;                    // distinct label sets
let exs   = db.metrics().exemplars("http.duration").await?;                  // trace links (drill-down)

// Attribute discovery (cross-signal — unions labels across logs, spans, and all metric tables):
let names = db.attrs().names().await?;
let vals  = db.attrs().values("service.name").await?;
```

**SQL** (lazy) over the `logs` / `spans` / `metrics_gauge` / `metrics_sum` / `metrics_histogram` /
`metrics_exp_histogram` / `metrics_summary` tables — with the `matches(col, 'q')` full-text UDF,
`json_get_str(attributes, 'k')` attribute access, `histogram_quantile(phi, explicit_bounds,
bucket_counts)`, and `hex()` for id columns:

```rust
let batches = db.sql(
    "SELECT service, count(*) FROM logs WHERE matches(body, 'error') GROUP BY service"
).collect().await?;
// Metric tables carry List columns (explicit_bounds/bucket_counts, quantiles/values) queryable
// with array_length(...) and the histogram_quantile UDF.
let latency = db.sql(
    "SELECT histogram_quantile(0.95, explicit_bounds, bucket_counts) AS p95 FROM metrics_histogram"
).collect().await?;
```

**Sync hosts** use the blocking facade — the full surface with no `.await`, on an owned runtime:

```rust
let b = db.blocking();
b.ingest_otlp_logs(&bytes)?;
let rows = b.sql("SELECT count(*) FROM logs")?;
```

## Maintenance & ops

Maintenance is inline unless you opt into a scheduler (the "no background threads" guarantee):

```rust
db.maintain().await?;              // seal the buffer + apply retention
db.compact().await?;              // merge small segments per day-partition (+ rebuild search index)
let stats = db.stats().await?;     // per-table rows/segments/time span + buffer_bytes / wal_bytes / durable_lsn
db.snapshot("./backup").await?;    // manifest copy + hard-linked segments (a full, queryable DB dir)
let files = db.segment_files(imbh::Table::Logs); // Parquet paths for zero-copy handoff (e.g. DuckDB)
let ipc   = db.export(imbh::Table::Logs, range).await?; // Arrow-IPC bytes for pandas/polars/DuckDB
```

Opt into a background scheduler with `DbBuilder::maintenance(Maintenance::Background(d))` (an owned
thread) or `Maintenance::Runtime(handle, d)` (a task on your runtime). `d` is the retention cadence;
what makes it *seal* is `DbBuilder::flush(FlushPolicy)`, whose triggers OR together:

```rust
use imbh::{FlushPolicy, Maintenance};
let db = Db::builder("./telemetry")
    .maintenance(Maintenance::Background(Duration::from_secs(300)))  // retention every 5 min
    .flush(
        FlushPolicy::periodic(Duration::from_secs(5))   // …and seal every 5s,
            .at_buffer_rows(50_000)                     // …or at 50k buffered rows,
            .at_wal_bytes(64 << 20)                     // …or once the WAL reaches 64 MiB,
            .after_idle(Duration::from_secs(2)),        // …or 2s after the traffic stops.
    )
    .open()?;
```

Without a policy the buffer seals on the maintenance interval and at the memory-budget-derived byte
threshold; `FlushPolicy::manual()` turns automatic sealing off entirely. The scheduler is also what
honors `WalMode::Interval(d)` — with no scheduler running, an interval-mode WAL is only fsynced by
`flush()`/`close()`. Rows are queryable from the buffer either way; sealing is what puts them in
Parquet and lets the WAL be reclaimed.

The reference `imbhd` server runs that scheduler by default (`IMBH_FLUSH`, default `interval=5s`) and
also exposes these over HTTP (`GET /stats`, `POST /admin/{flush,compact}`).

## Tuning

- **Memory** is governed by one `MemoryBudget` (buffer byte-cap + DataFusion pool + Tantivy writer
  heap). The buffer is bounded by *bytes*, not rows — per-record `attributes` JSON dominates
  (§6.1); promote hot keys or lower the seal threshold if steady RSS bites.
- **Durability vs throughput**: `WalMode::Always` (durable per-ingest, fsync cost) →
  `Interval(1s)` (default) → `Off` (OS-flush only).
- **Compression**: `Compression::Zstd(level)` (default 3) or `Lz4` (pure-Rust) for segments.
- **Footprint knobs** for constrained embedders (documented, not default): building imbh with
  `default-features = false` drops the `search` feature — the whole Tantivy subtree (~59 crates) —
  and `matches()` falls back to a full scan (identical results, no pruning). Per-signal gates
  (`logs`/`traces`/`metrics`) and `sql`-off are planned further levers (§11). DataFusion is ~30 MB
  and is owned as the price of the query engine.

See `ARCHITECTURE.md` §11 for the full footprint engineering story and `.agents/docs/QUALITY_GATE.md` for
the footprint gate (`scripts/footprint-gate.sh`).

# imbh

The IMBH facade: the embeddable `Db` handle wiring OTLP ingest → storage → query.

> **[IMBH](https://github.com/moriyoshi/imbh)** is a small-footprint, embeddable observability
> database for Rust that ingests OpenTelemetry logs, traces, and metrics and answers queries
> through Apache DataFusion (SQL) and Tantivy (full-text search), all in-process with no server or
> network hop. `imbh` is the crate you link against to embed it.

`imbh` is the crate a host links against. `Db` is the root handle: it wires OTLP ingest
([`imbh-otlp`](https://crates.io/crates/imbh-otlp)) into the storage engine
([`imbh-storage`](https://crates.io/crates/imbh-storage)) and answers queries through the query
layer ([`imbh-query`](https://crates.io/crates/imbh-query)). It is a concrete `Send + Sync` struct
(not `Clone`); `open()` / `open_read_only()` hand back an `Arc<Db>` you share across threads and
tasks.

```rust,ignore
let db = imbh::Db::in_memory().open()?;
db.ingest_otlp_logs(&otlp_bytes)?;         // WAL-backed OTLP ingest
let rows = db.sql("SELECT body FROM logs WHERE matches(body, 'error')").collect()?;
```

Surface:

- Builder / open, on-disk or in-memory.
- WAL-backed OTLP **logs, traces, and metrics** ingest with idempotent replay.
- `flush` / `maintain` (seal + retention) and an opt-in background maintenance scheduler.
- Typed query APIs: `logs()`, `traces()`, `metrics()`, `attrs()`.
- `sql(...).collect()` over the `logs` / `spans` / `metrics_*` tables (buffer ∪ segments), with
  the `matches` / `json_get_str` / `hex` / `histogram_quantile` UDFs and the cost-gated Tantivy
  `RowSelection` pushdown.
- `stats` / `snapshot` / `compact` / `segment_files`, span RED metrics, `durable_through`,
  Arrow-IPC `export`, a `blocking()` facade for synchronous hosts, and `close`.

Optional features include `proto` (protobuf query-input mappings), `cdata` (Arrow C Data
Interface), and `tracing-console` (the `imbh::console` stderr renderer).

## Role in the workspace

The center of the graph: `core ← {otlp, storage, index, query} ← imbh ← {exporter, server}`. The
reference `imbhd` HTTP server lives in [`imbh-server`](https://crates.io/crates/imbh-server); the
OpenTelemetry SDK exporter in
[`imbh-otel-exporter`](https://crates.io/crates/imbh-otel-exporter); in-process `tracing` plumbing
in [`imbh-tracing`](https://crates.io/crates/imbh-tracing).

See the design reference [`ARCHITECTURE.md`](https://github.com/moriyoshi/imbh/blob/main/.agents/docs/ARCHITECTURE.md)
§10 (API surface), §12. License: Apache-2.0.

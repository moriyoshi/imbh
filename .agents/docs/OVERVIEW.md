# IMBH Overview

IMBH is a **small-footprint, embeddable observability database suite in Rust** — a library
first, not a server. It ingests OpenTelemetry logs, traces, and metrics; stores them durably
in a compact columnar format; and answers queries through Apache DataFusion (SQL and typed
query plans) and Tantivy (full-text and term search). The whole point is that a host
application can embed a real observability backend without standing up Loki + Tempo + Mimir
separately.

The product is the library. Any HTTP server is wiring the *host* owns; the reference `imbhd`
binary (milestone M5) is one worked example of that wiring, not the deliverable.

> **This document** is the high-level orientation: what IMBH is, why, the pipeline, the crate
> map, and status. The canonical *design* reference — data model, storage engine, search,
> query, and the full public API surface — is [ARCHITECTURE.md](./ARCHITECTURE.md). Section
> numbers here and in ARCHITECTURE.md are **preserved from the original design plan** so the
> many `§N` cross-references in the code and docs stay valid: §1–§4 and §13 live in this file;
> §5–§12, §14, §15, and the appendices live in ARCHITECTURE.md.

## 1. Vision

The SQLite of observability *in embeddability, not in kilobytes* (M0 measured a ~32 MiB
binary floor — the price of a real query engine; see §2 and ARCHITECTURE.md Appendix C): a
single-node, embeddable database for **traces, logs, and metrics** that speaks OpenTelemetry
natively. You link it into your process, feed it OTLP, and query it through typed Rust APIs
modeled on the surfaces of Loki, Tempo, Mimir, SigNoz, and VictoriaMetrics — or through SQL.

Primary use cases, in priority order:

1. **In-process telemetry buffer/store** for edge agents, CLI tools, and appliances that
   cannot ship data to a SaaS (or want a local window before shipping).
2. **Dev-loop observability**: a `docker run`-free local backend for looking at your own app's
   traces/logs/metrics while developing.
3. **Small-fleet sidecar**: one binary per host, days of retention, queried ad hoc.

Non-negotiable constraints:

- Small footprint in **dependency graph**, **binary size**, and **runtime RSS**.
- **Embeddable first**; the library is the product. Any server is host wiring; IMBH ships a
  reference one (`imbhd`), not a mandatory one.
- Query engine is **Apache DataFusion**; full-text/term search is **Tantivy**.
- Data model follows **OpenTelemetry** semantics (OTLP field names, severity numbers, span
  kinds, metric temporality, resource/scope separation).

## 2. Goals and footprint budgets

Footprint is a first-class requirement, not an afterthought. The budgets below are
**M0-measured, not guessed**: a probe (inlined in ARCHITECTURE.md Appendix C) that links and
*exercises* the full trimmed stack (DataFusion query + Parquet round-trip, Tantivy mmap index
build+search, OTLP encode/decode) was built at the shipping profile and measured. The
original draft's numbers (≤18 MB binary, ≤12 MB idle RSS, ≤200 crates) were optimistic by
~1.8–2× and are replaced by the measured reality below.

**M0 baseline** (aarch64-glibc, `opt-level="s"` + fat LTO + strip + panic=abort; datafusion
54.0.0 / tantivy 0.26.1 / opentelemetry-proto 0.32.0, all trimmed): **31.9 MiB** stripped
binary, 36.0 MB exercised anonymous RSS (writer-inflated), 45.6 MB peak RSS, **269 unique
crates** — of which datafusion's subtree is 204.

**Revised budgets** (shipping target x86_64-musl; headroom over the arm-glibc probe floor for
server deps + libc bundling). Items marked *(unmeasured)* become measurement tasks in the
noted milestone:

| Metric (release, stripped, default features)              | Target   | Hard limit | |
|-----------------------------------------------------------|----------|------------|---|
| `imbhd` server binary (x86_64-unknown-linux-musl)         | ≤ 42 MB  | 55 MB      | |
| Size delta when embedding the lib into a host binary      | ≤ 32 MB  | 42 MB      | |
| Idle RSS (DB open, no active writer, empty pool)          | ≤ 40 MB  | 64 MB      | *(clean harness, M1)* |
| Steady RSS ingesting 10k log records/s (default buffers)  | ≤ 200 MB | 320 MB     | *(M1 soak)* |
| Peak RSS, aggregation over ~1 GB of segments, 128 MB pool | ≤ 350 MB | 512 MB     | *(M3)* |
| Cold open → first query                                   | ≤ 100 ms | 300 ms     | *(M1)* |
| Crates in default-feature build (`cargo tree` unique)     | ≤ 275    | 300        | measured 269 |

**Go/no-go outcome:** GO on the architecture and the DataFusion+Tantivy+OTLP engine choice —
the stack trims cleanly, and ~32 MiB for single-binary traces+logs+metrics with SQL *and*
full-text search is a strong story against the alternatives. But DataFusion is 204/269 crates
and most of the binary, with no cheap lever to shrink it (dropping `sql` saves only ~13
crates), so IMBH **owns ~30 MB as the price of the query engine** and tempers the "tiny"
framing. Real size knobs, for constrained embedders only, are per-signal feature gates and
turning `search`/`sql` off (ARCHITECTURE.md §11). Self-observability is a separate opt-in: the
`tracing` feature (off by default) instruments the ingest → storage → query hot paths and adds
**zero** crates to the default graph (the `tracing` facade rides in via DataFusion); only `imbhd`
built with `--features tracing` pulls the `tracing-subscriber` renderer (+5 crates). The Arrow C
Data Interface is a third opt-in on the same footing: the `cdata` feature (off by default) turns on
`arrow/ffi` on the single datafusion-shared arrow crate so the facade can re-export
`FFI_ArrowArray`/`FFI_ArrowSchema`/`FFI_ArrowArrayStream` for a zero-copy FFI binding — it adds
**zero** crates to the graph (measured: 275 → 275) and only the small `arrow::ffi{,_stream}` module
to the binary, so the default shipping graph is unchanged for non-binding embedders. RSS is tracked as *anonymous* RSS: Tantivy
indexes and Parquet files are mmapped, so file-backed pages are reclaimable page cache; the
tuning surface (mutable buffers, DataFusion pool, Tantivy writer heap, Arrow batch sizes) is
all capped by one `MemoryBudget` config.

## 3. Non-goals (v1)

- Distribution, replication, HA, multi-node anything.
- Point deletes / updates. Data is immutable; deletion happens only via retention.
- Dashboards / UI, and any *mandatory* HTTP server: `imbhd` is a reference wiring of the
  library API, nothing more.
- A delete-series admin API: immutability + retention is the contract here.
- Cross-process **writers**. One process owns a DB directory for writing, now enforced by a
  `writer.lock` (a second read-write open fails fast). Cross-process **read-only** opens *are*
  supported: `Db::open_read_only` takes no lock and answers queries from a point-in-time snapshot of
  the manifest's segments unioned with the writer's live WAL tail (near-real-time). The snapshot is
  consistent under the writer's concurrent seals, WAL reclaims, and retention — no drop, no
  double-count — with a bounded retry if retention/compaction unlinks a segment mid-query. Validated
  end-to-end by a two-*process* integration test. See ARCHITECTURE.md §5/§7, with the full design in
  §7.1.
- Alerting, sampling policies, tail-based sampling.

## 4. Prior art we borrow from

- **InfluxDB 3 / IOx**: WAL → in-memory Arrow buffer → Parquet segments, queried via
  DataFusion with a catalog of immutable files. IMBH copies this lifecycle wholesale.
- **Quickwit**: Tantivy index per immutable "split", doc → row alignment, index bundled into
  one file with a hotcache footer. IMBH copies per-segment indexes (single-file bundling is
  post-v1).
- **OpenObserve**: Tantivy-style inverted index used purely as a *row-pruning* accelerator in
  front of Parquet. IMBH copies the "search returns row ids → Parquet `RowSelection`" bridge.
- **GreptimeDB**: DataFusion-native TableProviders with pushdown over a custom engine.
- **SigNoz & VictoriaMetrics**: SigNoz's typed builder-query spec and VM's operational
  endpoints (export, cardinality stats, snapshot, force-merge) shape the library API surface
  (ARCHITECTURE.md §10), alongside the Loki/Tempo/Mimir query paths.
- **DuckDB/SQLite**: the embeddable API shape (open path → handle; blocking facade).

## Pipeline

The lifecycle is IOx-style, immutability-everywhere:

```
OTLP → WAL → Arrow mutable buffer → immutable Parquet segments
                                     + per-segment Tantivy index sidecar → manifest
```

- **WAL** gives durability; replay is idempotent via a per-generation LSN watermark (a crash
  after a seal but before WAL truncation does not double-count already-persisted rows).
- The **mutable buffer** holds per-table rows bounded by *bytes* (per-record `attributes` JSON
  dominates); sealing takes the buffered rows (`mem::take`) and builds an immutable Parquet
  **segment** with a co-located Tantivy index sidecar.
- The **manifest** is a whole-file document written atomically (write-temp → rename);
  immutability collapses crash safety to WAL replay + atomic rename + orphan cleanup on open.
  The manifest — never a directory scan — is the sole source of truth for what is queryable.
- **Queries see the buffer unioned with sealed segments**, so data is queryable immediately on
  ingest, before any flush. (Ingest is inline on the caller's thread by default; the opt-in
  `Ingest::Async` mode offloads the WAL + buffer write to one background worker task, trading that
  immediate visibility + the durable receipt for lower caller latency — ARCHITECTURE.md §5/§10.5.)
- Full-text hits from Tantivy map to Parquet rows through a row-ordinal fast field (the
  **Tantivy→`RowSelection` bridge**), applied only when a cost gate says pruning wins.
- Checksums (WAL, manifest) use XXH3-64 (`xxhash-rust`).

The full mechanics live in ARCHITECTURE.md: architecture overview (§5), data model (§6),
storage engine (§7), search (§8), and query layer (§9).

## Query surfaces

The public API is **endpoint-shaped**: it mirrors the query surfaces of Loki, Tempo,
Mimir/Prometheus, SigNoz, and VictoriaMetrics closely enough that mapping a library method to
an HTTP route is mechanical (ARCHITECTURE.md §10). Logs query + volume; trace get/search +
span (RED) metrics; metric range/instant/series/histogram-quantile/exemplars; cross-signal
attribute discovery; and cross-signal SQL via DataFusion. Native typed builders remain the stable
IMBH API. Optional bounded semantic profiles and translators add the explicitly scoped
`imbh.promql.p1.v1`, `imbh.logql.l1.v1`, and `imbh.traceql.t1.v1` surfaces without changing
native-builder behavior (ARCHITECTURE.md §10.18). The host-integration guide is
[docs/EMBEDDING.md](../../docs/EMBEDDING.md).

## Crate map

Cargo workspace (ARCHITECTURE.md §12); dependency direction
`core ← {otlp, storage, index, query} ← imbh ← {exporter, server}`:

| Crate | Responsibility |
|-------|----------------|
| `imbh-core` | schemas, ids, config, errors, manifest types, canonical JSON + a dependency-free JSON parser, time utils (arrow-free) |
| `imbh-otlp` | OTLP decode → normalized rows (prost types) for logs, traces, and metrics |
| `imbh-storage` | WAL, mutable buffer, seal, Parquet segments, manifest IO, retention, compaction; owns the Arrow schemas |
| `imbh-index` | Tantivy schema/build/search and the row-ordinal bridge (**only crate that knows Tantivy**) |
| `imbh-query` | DataFusion providers, UDFs, session config, typed plans (**only crate that knows DataFusion**) |
| `imbh-lgtm` | LGTM-stack (Loki/Tempo/Mimir) query-language compatibility: PromQL/LogQL/TraceQL parser-independent models + reference evaluators (`model`) and source-positioned translators for the pinned P1/L1/T1 profiles (`syntax`); optional native-IMBH adapters/builders under the `source` feature |
| `imbh-tui` | optional read-only local companion for metrics, traces, logs, and log-derived charts; its binary also hosts the MCP **stdio** transport (`imbh-tui --mcp-stdio`) |
| `imbh-mcp` | the MCP server: protocol dispatch, the 15 read-only tools, and the stdio transport — shared by `imbh-server`'s `POST /mcp` and `imbh-tui`'s stdio mode; adds no crate to the graph |
| `imbh` | the facade embedders use: `Db`, blocking + async API; optional stderr console renderer (`imbh::console`, `tracing-console` feature) |
| `imbh-proto` | protobuf wire types for the query-API inputs (Go/FFI binding surface, `proto` feature); generated from `.proto` via protox, prost-only, optional |
| `imbh-otel-exporter` | opentelemetry-rust SDK exporter adapters (span/log/metric), optional |
| `imbh-server` | reference `imbhd` binary + example HTTP wiring, optional; optional OTLP/gRPC (`grpc`) and a Docker logging-driver plugin (`docker`, Unix-only, zero added crates) |
| `imbh-tracing` | in-process `tracing` plumbing: `DbLayer` sinking `tracing` spans/events into an embedded `Db` (self-observation); depends on `imbh`, optional |

Confining DataFusion to `imbh-query` and Tantivy to `imbh-index` absorbs engine churn
(DataFusion ships breaking majors roughly monthly) behind two crates, upgraded on a deliberate
cadence. **Engine-boundary note:** `imbh-core` is arrow-free; `imbh-storage` owns the Arrow
schema and hands the `SchemaRef` + buffer `RecordBatch` to `imbh-query` *through the facade*,
so `imbh-query` stays the sole DataFusion-aware crate without either sibling depending on the
other.

## 13. Status and milestones

Each milestone below is a shippable pillar; ordering matters more than the estimates (M0 gates
everything). **Status: M0–M6 are complete and v0.1.0 is released** (tag `v0.1.0`, 2026-07-24; all
12 shipping crates on crates.io) — the workspace is durable, searchable, and queryable across all
three signals, with the reference server and SDK exporter built. See
[JOURNAL.md](./JOURNAL.md) for the design deviations and peer-review history, and
[TODO.md](./TODO.md) for tracked remainders and deferred follow-ups.

- **M0 — Walking skeleton + footprint gate. Done.** Workspace scaffold; OTLP → buffer →
  Parquet segment → manifest → `db.sql()` over buffer ∪ segments; trimmed features + release
  profile from day one; the go/no-go footprint measurement (Appendix C).
- **M1 — Logs pillar, durable + searchable. Done.** WAL + idempotent recovery; seal/rotation;
  manifest; retention (age + disk budget); per-segment Tantivy with cost-gated `matches()`
  pushdown; the `json_get_str` UDF; the logs query/volume/count APIs and attribute discovery.
- **M2 — Traces. Done.** Span schema incl. events/links; `trace_id` bloom filters; the traces
  APIs (`get`/`search`, scoped tag discovery, `span_metrics` RED rollups); spans in Tantivy.
- **M3 — Metrics. Done.** All five point tables incl. exp-histograms; temporality recorded;
  `histogram_quantile` and rate helpers; the typed metric queries (`instant`/`range` with
  Matrix/Vector DTOs, catalog, series, exemplars).
- **M4 — Compaction + ops hardening. Done.** Opt-in maintenance scheduler; partition compaction
  with index rebuild; manifest GC + orphan cleanup; `db.stats()`, `db.compact()`,
  `db.snapshot()`.
- **M5 — Reference server + wiring examples. Done.** `imbhd`: OTLP/HTTP ingest on
  `/v1/{logs,traces,metrics}`, a SQL query endpoint `/api/query`, `/stats`,
  `/admin/{flush,compact}`, and `/health` — built on a minimal `std::net` HTTP stack with zero
  heavy deps. (No typed-API HTTP mapping or TOML config; see ARCHITECTURE.md §10.16.)
- **M6 — Polish + v0.1. Done.** Feature-matrix + `cargo-deny` + footprint gates; the embedding
  and PromQL→SQL guides; examples.

Post-v0.1 additions (outside the original milestone plan): the **Docker logging-driver plugin** in
`imbh-server` (optional `docker` feature) — `--log-driver imbh` writes container stdout/stderr into
an embedded `Db` and serves `docker logs` back out of it, with no new crate in the graph
(ARCHITECTURE.md §10.16, [docs/DOCKER_LOG_DRIVER.md](../../docs/DOCKER_LOG_DRIVER.md));
**signal-driven graceful shutdown** for `imbhd` (`SIGINT`/`SIGTERM` → stop accepting, drain in-flight
requests, seal the buffer, exit 0), also with no new crate (ARCHITECTURE.md §10.16); and the **MCP
server** (`imbh-mcp`), which exposes the telemetry as 15 read-only agent tools over both of MCP's
transports — `imbhd`'s `POST /mcp` and `imbh-tui --mcp-stdio` — again with no new crate
(ARCHITECTURE.md §10.16.1, [docs/MCP.md](../../docs/MCP.md)).

Post-v1 candidate tracks: broader query-language profiles; Parquet VARIANT for attributes;
read-only cross-process opens; object-store tiering; downsampling; single-file segment bundles
(Quickwit-style) for inode-constrained hosts.

## Footprint posture

The M0 probe (ARCHITECTURE.md Appendix C) measured the DataFusion + Tantivy + OTLP core
empirically: ~31.9 MiB binary, ~36 MB VmRSS, 269 crates — roughly 1.8× the original guesses,
which is why IMBH is framed as "compact," not "SQLite-tiny." DataFusion dominates the crate
graph (204/269); there is no cheap lever within a full build, so trimming default features
(`default-features = false` with a minimal set) and confining heavy deps to single crates is the
standing strategy. Any dependency change is checked against §2 above and ARCHITECTURE.md §11 /
Appendix C, and enforced by `scripts/footprint-gate.sh` (see [QUALITY_GATE.md](./QUALITY_GATE.md)).

The one *large* lever is the **producer / consumer** feature split (ARCHITECTURE.md §10.13): a host
that only ingests compiles `imbh` with `--no-default-features --features ingest` and drops the whole
DataFusion + sqlparser + tantivy subtree, cutting the graph from **287 unique crates (default) to 104
(−64%)**; a query-only consumer (`--features query`) is 221. The CI `feature-matrix` job asserts these
cuts hold. This is the only way to get materially below the DataFusion floor.

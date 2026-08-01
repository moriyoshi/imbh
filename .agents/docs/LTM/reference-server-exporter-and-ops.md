# Reference `imbhd` Server, `imbh-otel-exporter`, and the Ops/Admin Surface

## Summary

The reference `imbhd` HTTP server (`imbh-server` crate) is host wiring, not the product: an axum/hyper HTTP/1.1 server that drives the embedded `Db` and demonstrates the ingest/query/ops surface a real host would build. Alongside it sit the ops/admin capabilities on `Db` itself (`stats()`, `snapshot()`, `export()`, engine gauges) and the optional `imbh-otel-exporter` crate, which lets an in-process OpenTelemetry SDK pipeline export straight into an embedded `Db` with zero network hops. These pieces share one ingest/validation story with the OTLP/HTTP path and add essentially zero core footprint.

## Key Facts

- Design axiom: any HTTP server is host wiring; `imbhd` is one example wiring, not the product. It runs axum/hyper because that is what a real host would choose — and because the footprint claim never depended on avoiding it (see the next bullet).
- The crate-count budget is measured on the **library** graph — `scripts/footprint-gate.sh` runs `cargo tree -p imbh` — and the dependency direction is `imbh <- imbh-server`. So `imbh-server`'s own dependencies are outside the gated number by construction: the facade is 275 crates with or without axum. What axum/hyper cost is ~17 crates in `imbh-server`'s own graph and ~1.4 MiB of `imbhd` binary (31.2 -> 32.6 MiB, budget 42 MB). Do not re-argue this on crate count; argue it on whether `imbh-server` should stay optional.
- Routes: `POST /v1/logs`·`/v1/traces`·`/v1/metrics` (OTLP/HTTP protobuf, gzip accepted), `POST /api/query` (SQL body → JSON rows), `GET /health`, `GET /stats`, `POST /admin/flush`, `POST /admin/compact`. The table is a plain `axum::Router`, public as `imbh_server::app(db)` so a host can mount it in its own application.
- `imbhd [DB_DIR] [ADDR]` defaults to `./imbh-data` and `127.0.0.1:4318` (the OTLP port). With the optional `grpc` feature a 3rd arg `GRPC_ADDR` defaults to `127.0.0.1:4317`.
- Ops surface on `Db`: `stats() -> DbStats`, `snapshot(dest) -> SnapshotInfo`, `export(table, range) -> Result<Vec<u8>>` (Arrow-IPC stream). `DbStats` carries per-table stats plus engine gauges `buffer_bytes`/`wal_bytes`/`durable_lsn`.
- `imbh-otel-exporter` provides the SDK-exporter trio: `ImbhSpanExporter`, `ImbhLogExporter`, `ImbhMetricExporter`. Footprint verified unchanged: `imbh` = 275 crates, `imbh-otel-exporter` = 276 (275 + the crate itself), tree-diff empty.

## Details

### Reference `imbhd` HTTP server (M5, §10.16)

An axum/hyper HTTP/1.1 server on one shared multi-threaded runtime (migrated 2026-08-01 from a std-`net`, thread-per-connection server). The load-bearing detail is that IMBH's I/O is **blocking** parquet/tantivy inside async fns — there is no `spawn_blocking` anywhere in the library — so a handler that awaited a `Db` future directly would park a runtime worker and starve every other connection. Every `Db` call therefore goes through `offload`, which uses `tokio::task::block_in_place` (+ `Handle::block_on`) so tokio spawns a replacement worker; on a current-thread runtime (`#[tokio::test]`, where `block_in_place` panics) it falls back to a plain `.await`. This is the same blocking-facade insight as before, but it now has to be stated explicitly rather than being implied by one runtime per connection.

`route()` is a pure async fn (`Db` + method + path + body → `Response`), unit-tested without sockets. OTLP body routes call `ingest_otlp_*`. `GET /health` is a liveness probe. `imbhd [DB_DIR] [ADDR]` — point a stock OTel SDK's OTLP/HTTP exporter at it.

The 4xx/5xx split is approximated by matching the `Error` variant (`Ingest`/`Query`/`Config` → 400); it becomes exact with the typed error model + `is_user_error` (§10.3), which `error_response` now uses.

#### Query results → JSON (`batches_to_json`)

Query rows are serialized to JSON via arrow's `ArrayFormatter` (`imbh::arrow::util::display`): numeric columns render as JSON numbers, everything else as escaped JSON strings, nulls as `null`. Example: `SELECT 42 AS answer, 'ok' AS status` → `[{"answer":42,"status":"ok"}]`.

Hardening: `batches_to_json` originally built its `ArrayFormatter`s with `.expect("array formatter")`, so a column type arrow could not format would panic the per-connection thread (the server survives; only that connection dies). Fixed to `.ok()` → `Vec<Option<ArrayFormatter>>`; a column with no formatter (or a null cell) renders `null` — non-panicking graceful degradation. `ArrayFormatter` supports everything IMBH emits (including the `List` metric columns), so the panic was latent; the `.ok()` fallback is pure defense on top.

#### `GET /stats` (the VM `/status/tsdb` analogue)

Returns `db.stats()` (the enriched `DbStats`) as JSON — per-table `segment_count`/`segment_rows`/`buffer_rows`/`min`/`max` time, plus the engine gauges `buffer_bytes`/`wal_bytes`/`durable_lsn`. Hand-serialized (no serde, consistent with the server's zero-heavy-deps stance), reusing `json_string` for the table names. This makes the operational gauges reachable from a host over HTTP. All serialized fields are numeric or `json_string`-escaped static table names → valid JSON, no injection surface; `Option<i64>` times render as `null`/number.

#### Admin maintenance endpoints (§10.1/§10.16)

- `POST /admin/flush` — seal the buffer → `{"flushed":true}`.
- `POST /admin/compact` — force-merge segments → `{"segments_merged":N,"segments_created":N}`.

These are the "force-merge" ops from the plan's reference-server sketch. Documented as **unauthenticated by design** — a real deployment gates `/admin/*` itself. POST-only, safe idempotent maintenance ops, responses are fixed JSON with no user input → no injection.

#### OTLP/gRPC ingest behind the optional `grpc` feature

Added behind a new **off-by-default `grpc` feature** so the default footprint gate stays byte-for-byte identical (feature-gated tonic chosen over hand-rolling HTTP/2).

- `crates/imbh-server/src/grpc.rs` (`#[cfg(feature = "grpc")] pub mod grpc;`): one `OtlpGrpc` handler backs all three OTLP collector services (`LogsService`/`TraceService`/`MetricsService`) from a shared `Arc<Db>`. Each `export` re-encodes the decoded tonic request back to protobuf bytes and calls the existing `Db::ingest_otlp_*(&[u8])` path — one ingest/validation story with the HTTP routes. `to_status` maps IMBH errors via the §10.3 classifiers (not-found → `NotFound`, user-error → `InvalidArgument`, else `Internal`), mirroring HTTP `error_response`.
- `serve_grpc(db, SocketAddr)` (async, for tests) + `serve_grpc_blocking(db, &str)` (builds a multi-thread tokio runtime, for the binary). In `main.rs`: without `grpc`, unchanged — HTTP `serve()` blocks the main thread; with `grpc`, HTTP runs on a background thread and gRPC in the foreground on a second port.
- Deps: workspace pins `tonic = { version = "0.14", default-features = false }` (matches opentelemetry-proto 0.32's `gen-tonic` → tonic 0.14.x). `imbh-server` adds `tonic` (`server`/`router`/`codegen`), `opentelemetry-proto` (`gen-tonic`), and `prost` as **optional** deps pulled only by `grpc = ["dep:tonic", "dep:opentelemetry-proto", "dep:prost", "tokio/rt-multi-thread", "tokio/net", "tokio/time"]`.

Findings:
- The generated OTLP tonic services live in **opentelemetry-proto, not imbh-proto**. The workspace already had `opentelemetry-proto` with `gen-tonic-messages` (messages only); flipping to `gen-tonic` (feature-unioned only when the optional dep is enabled) brought the `*_service_server` modules and `tonic-prost` codec for free — no separate `tonic-build` step. Sharing one handler across the three services needs `LogsServiceServer::from_arc(Arc<T>)`, not `new(T)`.
- `tonic::transport::Server` needs only the `server` feature; `.add_service()`/Router needs `router`. `transport = ["server", "channel"]` would also pull the client `channel` subtree; the server path wants just `server` + `router` + `codegen`. (The e2e test's tonic *client* does need `channel`, which `gen-tonic` supplies — dev/test-only, still inside the feature.)
- tonic 0.14 **routes through axum**, so `--features grpc` always pulled axum/hyper/tower/h2. Once the HTTP listener moved onto that same stack, `grpc` stopped adding it: the full-feature graph is unchanged at 310 crates and `grpc`'s marginal cost is now just tonic + h2 (6 crates over default).
- Table names bite: the traces query is `FROM spans`, not `FROM traces`. The physical table for the trace signal is `spans` (imbh-core enums); metrics split across `metrics_gauge`/`metrics_sum`/`metrics_histogram`/`metrics_exp_histogram`.

Both listeners are on axum/hyper as of 2026-08-01 — the TCP server and the Docker plugin's Unix socket — sharing one `handle`, so body limits, phase deadlines, and decoding are identical on both. The crate's hand-rolled HTTP/1.1 parser is gone. `ReadLogs` keeps its blocking, `io::Write`-generic generator by running it on a `spawn_blocking` task whose sink is a bounded channel that the response body drains; that channel is also the backpressure and disconnect signal, arriving at the generator as ordinary `io::Error`s.

Reference-server follow-ups (deferred): TLS, TOML config, Arrow-IPC query output, the OTLP partial-success response shape, `/admin/*` auth.

### Ops/admin/stats/export surface

#### `Db::stats()` and per-table stats (M4a)

`Storage::stats() -> Vec<TableStats>` — per table (logs, spans, metrics_gauge, metrics_sum): segment count, summed segment rows, buffered rows, and `[min,max]` time span. `Db::stats()` wraps it as `DbStats`. `Table` is part of the public API (`TableStats.table`) — re-exported from the facade.

#### `DbStats` engine gauges (§10.11)

- `imbh-storage`: `Storage::buffer_bytes()` (live mutable-buffer heap across all tables) and `Storage::wal_bytes()` (on-disk WAL file size; 0 for in-memory / WAL-off).
- `imbh`: `DbStats` carries `buffer_bytes: usize`, `wal_bytes: u64`, and `durable_lsn: Option<Lsn>` (`None` = nothing durable yet; `Lsn` is `NonZero<u64>`) alongside the per-table `tables`. `db.stats()` fills them from storage. The `/stats` JSON keeps `durable_lsn` numeric on the wire via `stats.durable_lsn.map_or(0, |l| l.get())`, so 0 still reads as "nothing durable".

These are the operational gauges a host's `/status/tsdb`-style endpoint wants (buffer pressure, WAL growth, durability watermark) — cheap to read (`buffer_bytes` is a counter, `wal_bytes` a single `stat`), no scans. `DbStats.cardinality` (distinct-key estimates from term dicts) remains a follow-up — it needs the Tantivy term-dictionary read path, heavier than these constant-time gauges.

#### Read-only stats fix (reader-aware ops paths)

**A read-only handle holds no live state, so anything that reads `storage.inner` directly is wrong on a reader.** `Db::open_read_only` deliberately initializes every in-memory buffer/segment list *empty* — a reader's query view is derived per call from the on-disk manifest + WAL tail (`read_disk_snapshot`, replayed into a scratch buffer by `reader_tables`). So queries returned data while `Db::stats()` — which delegated to `storage.stats()` reading those empty in-memory lists — reported `rows=0+0 segments=0` for every table. The tell was the asymmetry: data visible in queries but not in stats points straight at "stats reads writer-only state."

The fix mirrors the query path: `reader_stats(&Db)` takes segment counts/rows/time-bounds from the snapshot manifest and gets unsealed buffer rows by replaying the WAL tail into a scratch `Storage` and reading *its* `stats()` (the scratch has no segments, so its `buffer_rows` are exactly the tail). Lesson: a read-only view is not "the writer with writes disabled" — every writer-side accessor (`stats`/`buffer_bytes`/`wal_bytes`) needs a reader-aware path or it silently reports the empty shell.

#### `Db::snapshot(dest)` (M4a)

`Storage::snapshot(dest) -> SnapshotInfo` copies the `MANIFEST` and **hard-links** every segment's Parquet file + `.tidx` sidecar into `dest`. Segments are immutable, so links are safe; `link_or_copy` falls back to a copy across filesystems, `link_dir` recreates the sidecar tree. The snapshot is a complete DB directory: `Db::builder(snap).open()` queries it directly. Snapshot = hard-links, not copies — near-instant and space-free for the immutable segment files, matching VM's `/snapshot` semantics (§10.11); only the tiny manifest is copied.

#### `Db::export` (Arrow-IPC stream, §10.11)

`Db::export(table, range) -> Result<Vec<u8>>` — the copy-out companion to `segment_files` (raw Parquet handoff). Emits a self-describing Arrow-IPC **stream** (schema message + record batches + EOS) that DuckDB / polars / pandas-`pyarrow` load directly. Implemented as `SELECT * FROM <table> WHERE CAST("time" AS BIGINT) BETWEEN range ORDER BY "time"` → `collect()` → `arrow::ipc::writer::StreamWriter`. Mirrored on `BlockingDb::export`. No new dependency (arrow-ipc already in the tree via arrow).

Design notes:
- **One table per call**, not the `&[Table]` of the PLAN.md §10.11 sketch: an IPC stream carries a single schema, and IMBH's tables (`logs`/`spans`/`metrics_*`) have distinct schemas — you cannot interleave them in one stream. A multi-stream or same-schema-union variant can come later.
- **Empty-result schema fallback**: `collect()` can return zero batches, which carries no schema, so an empty export would lose the column list. Fixed by pre-computing the table's schema from `storage.schema*()` (which also doubles as an up-front guard that rejects not-yet-materialized tables with a `Query` error → 400 via `is_user_error`) and using it when the result is empty; a non-empty result uses `batches[0].schema()`.
- The `RecordBatchStream` (bounded-memory streaming) form the plan ultimately wants stays a follow-up — it needs the whole query path to go lazy/streaming, which the eager `collect()` architecture does not yet do.

### `imbh-otel-exporter` crate (SDK-exporter trio, §12)

The optional `crates/imbh-otel-exporter` lets an in-process opentelemetry SDK pipeline export straight into an embedded IMBH `Db` with zero network hops (self-observation). Each adapter is a thin wrapper reusing the OTLP ingest path, so ingest/WAL/query behave identically to an OTLP/HTTP ingest. **Footprint unchanged: `imbh` facade still 275 crates** — the opentelemetry SDK was already transitive via opentelemetry-proto, so this optional crate adds zero core footprint.

The core pattern across all three: convert + prost-encode **synchronously** (before / inside the brief lock hold), then move only the encoded `Vec<u8>` into the returned async block — the future must borrow neither the batch nor a lock across `.await` (and must be `Send`). This sidesteps `ResourceAttributesWithSchema` not being `Clone`.

#### `ImbhSpanExporter` (traces)

An `opentelemetry_sdk::trace::SpanExporter`. Pipeline: `group_spans_by_resource_and_scope(batch, &resource)` → `ExportTraceServiceRequest` → prost-encode → `db.ingest_otlp_traces(&bytes)`. `set_resource` (the SDK sets it once at provider build) is captured behind `Arc<Mutex<…>>` and stamped onto every batch; manual `Debug` impl since `Db` is not `Debug`. Errors map to `OTelSdkError::InternalFailure(String)`.

#### `ImbhLogExporter` (logs)

An `opentelemetry_sdk::logs::LogExporter`. Pipeline: `group_logs_by_resource_and_scope(&batch, &resource)` → `ExportLogsServiceRequest` → prost-encode → `db.ingest_otlp_logs(&bytes)`. `LogExporter::export` takes a **borrowed** `LogBatch<'_>`, so convert+encode must happen synchronously before the async block. Footprint: added the `logs` feature to the exporter's `opentelemetry`/`opentelemetry_sdk`/`opentelemetry-proto` deps; the `logs` feature was already unified on via opentelemetry-proto's workspace config (logs+trace+metrics for imbh-otlp), so this is pure re-use.

#### `ImbhMetricExporter` (metrics)

An `opentelemetry_sdk::metrics::exporter::PushMetricExporter`. The conversion is the *simplest* of the three despite the trait being the fiddliest: `ResourceMetrics` already carries its own `Resource`, so there is no `set_resource` and no resource lock — `ExportMetricsServiceRequest::from(&ResourceMetrics)` (a single `From` impl in opentelemetry-proto) → prost-encode → `db.ingest_otlp_metrics(&bytes)`.

Trait-contract details:
- `export(&self, metrics: &ResourceMetrics)` borrows; convert+encode synchronously before the `async move` so the returned `Send` future borrows neither `metrics` nor any lock.
- `temporality(&self) -> Temporality`: defaults to `Cumulative` (the OTel/SDK default; IMBH's `metrics().rate_counter()` reads cumulative counters). A `with_temporality(Temporality::Delta)` builder lets a host pick delta (paired with `metrics().rate()`) to avoid cumulative baselines across restarts. This is the one real design choice in the crate; Cumulative is the least-surprising, most-interoperable default and matches what a network OTLP exporter sends.
- `force_flush` / `shutdown_with_timeout` return `Ok(())`: each `export` ingests synchronously, so the exporter holds no pending data; and the `Db` is a shared host-owned handle (`Clone`), so shutdown must NOT close it. IMBH's own durability stays the host's concern via `Db::flush`.

Added the `metrics` feature to the exporter's deps (already unified on via opentelemetry-proto's workspace config).

#### Crate review + fixes (post-trio self-assessment)

A focused review over the complete crate (all three adapters) found **no High/Medium defects** — the synchronous-convert / async-ingest split is correct across spans/logs/metrics (futures capture only `Db` + `Vec<u8>`, never the borrowed batch or a lock across `.await`), all three trait contracts are honored, each exporter is `Send + Sync + 'static`, and the tests prove real emit→OTLP→ingest→query round-trips. Three Low findings:

- **Fixed — mutex poison recovery (1.1).** The span/log resource lock is held across the whole transform + encode, so a panic there would poison it permanently and wedge the pipeline for the process lifetime (and in `SimpleSpanProcessor` the re-panic fires on the span-end/drop path). Switched all four lock sites from `.expect(...)` to `.lock().unwrap_or_else(|e| e.into_inner())` — the guarded value is a plain resource snapshot, so poison recovery is safe. (The hold is not "brief" — it spans transform+encode.)
- **Fixed — strengthened metric test (5.1).** `exports_metrics_into_db` now filters `... AND value = 5`, proving the data point (not just the metric name) round-tripped (`metrics_sum.value` is Float64; DataFusion coerces the int literal).
- **Verified — footprint at the feature level (§4).** Cargo feature unification could in principle enable extra `opentelemetry_sdk` features in the *core* graph even with no new crates. `cargo tree -p imbh -e features` shows the core `imbh` graph already resolves `opentelemetry_sdk` with `logs,metrics,trace` (driven by opentelemetry-proto's workspace config), independent of the exporter. So "zero added footprint" holds at the feature level, not just crate count.
- **Deferred to TODO** — post-shutdown `export` error (2.1) (contract nit with a real tradeoff for an embedded sink; no `AtomicBool` shutdown flag) and additive coverage (`set_resource` stamping, error path, `Temporality::Delta`).

## Files

- `crates/imbh-server/` — the reference `imbhd` server crate (bin `imbhd`). `route()` HTTP dispatch, `batches_to_json`, `json_string`, `error_response`, `serve()`.
- `crates/imbh-server/src/grpc.rs` — `#[cfg(feature = "grpc")]` OTLP/gRPC handler (`OtlpGrpc`, `to_status`, `serve_grpc`, `serve_grpc_blocking`).
- `crates/imbh-server/src/main.rs` — `imbhd` binary entry; CLI args `[DB_DIR] [ADDR] [GRPC_ADDR]`.
- `crates/imbh-server/tests/grpc_e2e.rs` — `#![cfg(feature = "grpc")]` loopback HTTP/2 e2e across logs/traces/metrics/histogram.
- `crates/imbh-storage/` — `Storage::stats()`, `Storage::snapshot()`, `Storage::buffer_bytes()`, `Storage::wal_bytes()`.
- `crates/imbh/` — `Db::stats()` / `DbStats` / `TableStats`, `Db::snapshot()` / `SnapshotInfo`, `Db::export()` (and `BlockingDb::export`), `reader_stats(&Db)` / `reader_tables` / `read_disk_snapshot`.
- `crates/imbh-otel-exporter/` — `ImbhSpanExporter`, `ImbhLogExporter`, `ImbhMetricExporter` (optional crate; `logs`/`metrics` features).

## Test Coverage

- `imbh-server` route test (`health_ingest_query`) — covers `POST /v1/*` ingest, `POST /api/query`, `GET /health`, and the `/stats` shape (gauges present, `logs` table entry with `buffer_rows:1`); also exercises `/admin/flush` and `/admin/compact` (no-op on the in-memory test DB).
- `query_list_column_renders` — ingest a histogram via `/v1/metrics`, then `SELECT metric, bucket_counts FROM metrics_histogram` via `/api/query` → 200 with the `List` column serialized end-to-end through the server.
- `grpc_e2e.rs` — real tonic clients over a loopback HTTP/2 socket assert rows land in the shared `Db` via SQL (compiles to nothing without the `grpc` feature).
- `db_stats_engine_gauges` — after `WalMode::Always` ingest: `buffer_bytes > 0`, `wal_bytes > 0`, `durable_lsn == receipt.lsn` (both `Option<Lsn>`); after `flush()`: `buffer_bytes == 0` **and** `wal_bytes == 0` (cross-checks seal's WAL truncation).
- Read-only stats regression test — ingests, asserts the reader counts the unsealed tail as `buffer_rows`, then `flush()`es and asserts the same rows move to `segment_rows` with real min/max time-bounds. Exercises both the sealed and never-sealed states (either half alone would pass a partial fix).
- `export_arrow_ipc_roundtrips` — decodes the bytes with a stock `StreamReader`, asserts the buffer ∪ segment union (3 rows across a sealed segment + live buffer), the empty-range schema-only stream, and the not-yet-materialized-table error path.
- `exports_spans_into_db` — real `SdkTracerProvider` + `SimpleSpanProcessor(exporter)`, `tracer.in_span("checkout", …)`, `force_flush()`, then `SELECT name FROM spans` → 1 row `checkout`.
- `exports_logs_into_db` — real `SdkLoggerProvider` + `SimpleLogProcessor(exporter)`, emit via the raw logs API (`logger.create_log_record()` / `set_body("checkout failed".into())` / `logger.emit(record)`), `force_flush()`, then `SELECT body FROM logs` → 1 row `checkout failed`.
- `exports_metrics_into_db` — real `SdkMeterProvider` + `PeriodicReader(exporter)`, `u64_counter("requests").add(5, &[])`, `provider.force_flush()`, then `SELECT metric FROM metrics_sum WHERE metric='requests' AND value = 5` → 1 row.

## Pitfalls

- **`ArrayFormatter` construction can fail** — build it with `.ok()` (not `.expect`) so an unformattable column renders `null` instead of panicking the per-connection thread.
- **A read-only `Db` is not "the writer with writes disabled."** Every writer-side accessor (`stats`/`buffer_bytes`/`wal_bytes`) reads empty in-memory lists on a reader and silently reports zero — each needs a reader-aware path that derives from the manifest + WAL-tail replay. The tell: data visible in queries but not in stats.
- **`db.export` is one table per call** — an Arrow-IPC stream carries a single schema and IMBH tables have distinct schemas. Also pre-compute the schema from `storage.schema*()` so empty results (zero batches → no schema) still emit the column list.
- **Table names bite in SQL:** the trace signal's physical table is `spans`, not `traces`; metrics split across `metrics_gauge`/`metrics_sum`/`metrics_histogram`/`metrics_exp_histogram`.
- **Exporter futures must not capture the batch or hold a lock across `.await`.** Convert + prost-encode synchronously, move only the `Vec<u8>` into the async block. The borrowed `LogBatch<'_>` case is the easy one to get wrong.
- **Resource-lock poison:** the span/log resource lock is held across transform+encode, so a panic poisons it permanently. Use `.lock().unwrap_or_else(|e| e.into_inner())` — safe because the guarded value is a plain resource snapshot.
- **`set_body` is on the `opentelemetry::logs::LogRecord` trait** — must be imported or the log-exporter test fails E0599 ("method available … trait not in scope").
- **Exporter `shutdown`/`force_flush` must return `Ok(())` without closing the `Db`** — the `Db` is a shared host-owned `Clone` handle; durability is the host's concern via `Db::flush`.
- **`/admin/*` endpoints are unauthenticated by design** — a real deployment must gate them itself.
- **gRPC lives behind an off-by-default `grpc` feature** — the OTLP tonic services come from opentelemetry-proto's `gen-tonic` (not imbh-proto), shared across services via `*_service_server::from_arc(Arc<T>)`; keep tonic to `server`+`router`+`codegen` to avoid pulling the client `channel` subtree into the server path.

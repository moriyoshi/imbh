# Self-observability via `tracing`

## Summary

IMBH is instrumented with tokio's `tracing` across the ingest → storage → query hot paths, feature-gated (`imbh/tracing`, off by default) so the default library build is byte-identical. Collection is severable into three independent opt-ins: emit (`imbh/tracing`), render to stderr console (`imbh/tracing-console`, home of `imbh::console`), and sink back into an embedded `Db` (`imbh_tracing::DbLayer`). The load-bearing footprint result is that the `tracing` facade is already in the default graph via DataFusion, so instrumenting the libraries costs zero crates; only `tracing-subscriber` (the renderer) is genuinely new, and it lives above the library graph.

## Key Facts

- **Policy**: IMBH standardizes on the `tracing` facade (not `log`), feature-gated `tracing`, off by default. This supersedes the former ARCHITECTURE.md §11 line "`log` facade, not `tracing`, in core crates" (that rule was aspirational and never wired). ARCHITECTURE.md §11 and OVERVIEW.md §2 were updated to match.
- **Three severable pieces**, a host picks any subset:
  - `imbh/tracing` — the library *emits* spans/events (off by default).
  - `imbh/tracing-console` — a `fmt` subscriber *renders* them to stderr (`imbh::console`, `crates/imbh/src/console.rs`, off by default, `tracing-console = ["dep:tracing-subscriber"]`). Independent of `tracing`; enable both to render IMBH's own spans/events.
  - `imbh_tracing::DbLayer` — a `tracing_subscriber::Layer` that *sinks* `tracing` back into a `Db`.
- **Footprint**: default facade build unchanged at **275 crates**; the `tracing` facade adds **zero** crates even when on (DataFusion already pulls `tracing`/`tracing-core`/`tracing-attributes` transitively). `imbhd --features tracing` = **282 crates** (+5 over its default graph: `tracing-subscriber`, `matchers`, `sharded-slab`, `thread_local`, `lazy_static`), well under the 300 hard limit. Default release binary byte-identical at 32.0 MiB.
- `imbh-tracing` depends on `imbh` with **default** features (never `imbh/tracing`), so wiring a sink never forces internal instrumentation on, and vice versa. Emit and collect are orthogonal (the same decoupling `tracing`'s global dispatcher gives at runtime, applied at the Cargo-feature layer).
- `imbh-tracing` has **no inner feature gate** — the crate itself is the opt-in boundary (`DbLayer` + the `imbh`/`opentelemetry-proto`/`prost`/`tracing` deps are unconditional). `imbh-tracing` standalone is 281 crates (it always embeds IMBH); the `DbLayer` round-trip test runs under plain `cargo test --workspace`.
- `imbh-server`'s `tracing` feature composes emit+render by forwarding to `imbh/tracing` + `imbh/tracing-console`; it dropped its `imbh-tracing` dependency outright.

## Details

### Emit — internal instrumentation (feature `imbh/tracing`)

Optional `tracing` dep + a `tracing` feature on each instrumented crate (`imbh-otlp`, `imbh-index`, `imbh-storage`, `imbh-query`, and the `imbh` facade, which forwards to all of them with `imbh-index?/tracing` for the search-optional edge). `imbh-core` stays untouched (pure types, nothing to span), so no crate forwards to it. Every instrumentation site is `#[cfg(feature = "tracing")]` / `#[cfg_attr(feature = "tracing", tracing::instrument(...))]`, so the default build is byte-identical; the `Cargo.lock` diff is purely additive.

Spans + events across the hot paths:

- `ingest.{logs,traces,metrics}` (facade cores, fields `bytes`/`accepted`/`lsn`/`durable` via a shared `record_ingest` span-record helper)
- `otlp.decode_*` (row counts)
- `wal.append` (trace-level, `signal`/`lsn`/`durable`) + a WAL-rotation event
- `wal.replay` (open-time span + record count)
- `storage.seal` / `storage.compact` (completion events with watermark / `CompactionReport`)
- `query.run_sql` (fields `sql`/`pool_bytes`, `warn!`/`error!` on plan/execute failure, completion event with row/batch counts + `ScanStats`)
- `index.{build_logs,build_spans,search_body,search_attr_eq}` (hit counts)
- an outer `request` span (method/path/status) in the server `route`

**Deeper query spans.** `imbh-query` emits a per-segment `debug_span!("query.scan_segment")` in `SegmentBatchIter::next` (records `index_hits`/`hit_fraction`/`row_selection`/`pruned` from `row_selection_for`'s cost gate), and records `ScanStats` fields (`segments_scanned`/`segments_pruned`/`rows_scanned`/`bytes_scanned`/`index_searched`) onto the `query.run_sql` span via `span.record`. All under the off-by-default `tracing` feature; clean in default, `--features tracing`, and `--no-default-features --features tracing` (the search-off stub leaves index fields `Empty`).

**Design choices for emission**:

- **Rejected instrumenting the `imbh-core` error constructors**: they are called from unit tests, `From<io::Error>`, and control-flow paths (backpressure that is retried), so an `error!` there would be noisy and misleading. Error events live at the hot-path *surfacing* sites instead (`run_sql` plan/execute `map_err`), where a fault is genuinely being returned and a span is active.
- **Rejected a cross-crate `#[macro_export]` shim** for events: a macro that emits `#[cfg(feature = "tracing")]` has that cfg evaluated in the *calling* crate under feature unification, a gating hazard. The conventional per-crate pattern — `#[cfg_attr(feature = "tracing", tracing::instrument(...))]` for spans, local `#[cfg(feature = "tracing")] tracing::event!` for events, each gated on the crate's own feature — is unambiguous and needs no shim.
- **Late-known span fields** use `tracing::field::Empty` + `Span::current().record(...)` (see `record_ingest` and `wal_append_assign`): declare the field on the `#[instrument]` attribute, fill it after the work. The record block is `#[cfg(feature = "tracing")]` so the values it reads stay live (no unused-variable warnings) in the default build.
- `instrument` needs the `attributes` feature on `tracing` (enabled in the workspace dep) and works on `async fn` (used on `run_sql` and `route`); storage/index spans are plain sync spans and must not assume a tokio context (the maintenance loop can run on an owned OS thread).

### Render — the `imbh::console` collector (feature `imbh/tracing-console`)

The console collector (`init` / `try_init` / `init_with` / `env_filter` / `directives` / `IMBH_TARGETS`) lives in `crates/imbh/src/console.rs`, exposed as `imbh::console`, gated by `tracing-console = ["dep:tracing-subscriber"]`. `imbhd` calls `imbh::console::init()`. `env_filter(level)` = `RUST_LOG` verbatim if set, else every IMBH target at `level`. `tracing_subscriber` is re-exported for full-control hosts.

The console lives in the facade (not the `imbh-tracing` helper) so that "render IMBH's logs to the console" costs exactly `dep:tracing-subscriber` and nothing else, and a console-only host never pulls `imbh-tracing` (which unconditionally embeds `imbh` + opentelemetry-proto/prost for `DbLayer`). `imbh-server` dropping its `imbh-tracing` dependency is the proof. The default `imbh` and `imbh-server` graphs contain neither `tracing-subscriber` nor `imbh-tracing` (verified with `cargo tree -e no-dev`); `imbh --features tracing-console` pulls `tracing-subscriber`.

**End-to-end verification.** Built `imbhd --features tracing`, ran against `POST /api/query`, `POST /v1/logs`, `POST /admin/flush`. Span nesting works as designed — the `fmt` output showed correct parent→child context:

```
request:query.run_sql{sql="SELECT 1 AS x" pool_bytes=134217728}: imbh_query: query complete batches=1 rows=1 stats=ScanStats { … }
request:ingest.logs{bytes=0}:otlp.decode_logs{bytes=0}: imbh_otlp: decoded OTLP/logs rows=0
```

plus `wal.replay … records=0` at open. The outer `request` span (server route) parents the facade ingest/query spans, which parent the `imbh-otlp` decode span — the cross-crate hierarchy is intact through the feature-gated `#[instrument]` attributes.

### Sink — `imbh_tracing::DbLayer` (in-process `tracing` → `Db`)

`DbLayer` is a `tracing_subscriber::Layer` that ingests events → the `logs` table and span closes → the `spans` table of an embedded `Db`, over the same `Db::try_ingest_otlp_*` path `imbh-otel-exporter` uses (build OTLP protobuf → ingest), but sourced straight from `tracing` with no OpenTelemetry SDK. It still needs `tracing-subscriber`'s `registry` layer for `LookupSpan`. This is what "wire tracing subscribers to IMBH in process" means.

**Mapping**:

- `on_event` → one OTLP `LogRecord` (level → severity, the `message` field → body, other fields → attributes, current span's synthesized ids for correlation).
- `on_close` → one OTLP `Span` (name, start/end wall-clock captured in registry extensions, parent link).
- `on_new_span` stashes start time + captured fields + the trace id in the span's extensions (left unguarded — it only stashes, never ingests).

Ingest is synchronous (`try_ingest_*`), so the layer needs no runtime and never blocks the emitter; a closed Db / backpressure drops the record (best-effort telemetry).

**Design findings**:

- **A durable in-process sink still has to go through OTLP bytes.** The WAL stores the raw OTLP frame and replay re-decodes it, so there is no row-level ingest that preserves durability — a `LogRow` / `SpanRow` alone has nothing to put in the WAL. So `DbLayer` builds OTLP protobuf and calls `try_ingest_otlp_*`, exactly like `imbh-otel-exporter`. Reusing that path keeps WAL/replay/query identical for self-observation data.
- **Reentrancy is the defining hazard of self-observation.** With `imbh/tracing` on, ingesting a captured record makes IMBH emit `tracing`, which the layer would ingest, unbounded. A thread-local guard set across `on_event`/`on_close` drops any event/span-close produced on the same thread while a write is in flight — which both prevents recursion and naturally excludes IMBH's ingest-internal telemetry from the captured stream.
- **`tracing` has no OTel ids, so they are synthesized deterministically**: span id = the 8-byte `tracing::Id`; trace id = a per-layer nonce (wall-clock XOR a counter — no `rand` dep) ++ the root span's id, inherited by descendants via the registry extensions. Good enough for in-process correlation; not globally unique across processes (documented).
- **`opentelemetry-proto` 0.32 message structs carry newer fields** (e.g. `KeyValue.key_strindex` for string-table interning), so construct them with `..Default::default()` rather than exhaustive literals — otherwise a proto bump breaks the build on a field the sink never sets.

### Traced example runs

`embed-in-app` + `replay-otlp-file` each gained an opt-in `tracing` feature (`["imbh/tracing", "imbh/tracing-console"]`) calling `imbh::console::init()` under `#[cfg]` at the top of `main` — mirroring the real `imbhd` facade one-liner (subscriber owned by `imbh/src/console.rs` behind `imbh/tracing-console`) rather than duplicating subscriber wiring. Zero new default crates (`tracing-subscriber` 0 default → 1 only with the feature).

## Files

- `crates/imbh/src/console.rs` — the `imbh::console` renderer (`init` / `try_init` / `init_with` / `env_filter` / `directives` / `IMBH_TARGETS`), gated by `imbh/tracing-console`.
- `crates/imbh-tracing/` — helper crate; now `DbLayer`-only. Depends on `imbh` (default features), `opentelemetry-proto`, `prost`, `tracing`, `tracing-subscriber`.
- `crates/imbh-otlp`, `crates/imbh-index`, `crates/imbh-storage`, `crates/imbh-query` — carry the optional `tracing` dep + `tracing` feature and the `#[cfg(feature = "tracing")]` instrumentation sites (`record_ingest`, `wal_append_assign`, `query.run_sql`, `query.scan_segment` in `SegmentBatchIter::next`, etc.).
- `crates/imbh` — the facade `tracing` feature forwards to all instrumented crates (`imbh-index?/tracing` for the search-optional edge); `tracing-console` feature owns the console.
- `crates/imbh-server` — `tracing` feature forwards to `imbh/tracing` + `imbh/tracing-console`; the `route` `request` span; `imbhd` `main` calls `imbh::console::init()`.
- `examples/embed-in-app`, `examples/replay-otlp-file` — opt-in `tracing` feature calling `imbh::console::init()`.
- Canonical docs updated: OVERVIEW.md §2 and crate table; ARCHITECTURE.md §11 (self-observability prose), §12 (workspace-layout comment), the `imbh-tracing` dependency-tier paragraph; README.md crate table + the otel-exporter-vs-tracing FAQ.

## Test Coverage

- `cargo test -p imbh --features tracing` (54) and `-p imbh-storage --features tracing` (23) green — instrumentation does not change behavior.
- Two `imbh::console` unit tests.
- `DbLayer` round-trip test: emits a span containing an event through `registry().with(DbLayer::new(db).with_service(...))`, queries the rows back — asserts the log body/severity/service, the span name/service, and a `logs ⋈ spans` JOIN on `(trace_id, span_id)` that proves in-process correlation survives the round trip. Runs under plain `cargo test --workspace` (no `--features` needed after the inner `db` gate was dropped).
- `imbh-tracing` unit + doc tests.
- Deeper-span work clean in default, `--features tracing`, and `--no-default-features --features tracing`.
- Full gate green in every feature state: `cargo fmt --all --check` clean; `cargo build --workspace` + `cargo clippy --workspace --all-targets -D warnings` clean in both default and `--features tracing`; `cargo test --workspace` green. `./scripts/footprint-gate.sh`: `FOOTPRINT GATE: OK` (275 crates, imbhd 32.0 MiB, idle RSS 14 MB / steady 94.7 MB).

## Pitfalls

- **`tracing_subscriber::fmt()` writes to STDOUT by default**, not stderr. A first smoke test looked like "no output" because stderr was empty while every line had gone to stdout. The console init uses `.with_writer(std::io::stderr)` — diagnostics on stderr is the server convention and keeps stdout clean for piped output.
- **Every crate is its own `RUST_LOG` target.** `imbh=debug` covers ONLY the `imbh` facade — not `imbh_otlp` / `imbh_storage` / `imbh_query` / `imbh_index` / `imbh_server`, and not the `imbhd` bin target (main.rs events log under target `imbhd`, not `imbh_server`). A useful filter enumerates them, e.g. `RUST_LOG="imbhd=info,imbh=debug,imbh_otlp=debug,imbh_storage=trace,imbh_query=debug,imbh_index=debug,imbh_server=info"`. `imbh::console`'s `env_filter`/`directives`/`IMBH_TARGETS` encode this (all IMBH targets at `level` when `RUST_LOG` is unset).
- **A span with no passing event inside emits no line** under the default `fmt` layer (no `with_span_events`). Pure-span sites (`ingest.*`, `seal`, `request`) are only visible when they contain an event that passes the filter, or via their field context decorating a child event. For span open/close timing, add `.with_span_events(FmtSpan::CLOSE)` (opt-in; roughly doubles line volume).
- **A collector helper must NOT depend on the instrumented library's emit feature.** Making "add the crate" both emit and collect (depend on `imbh` with `features=["tracing"]`) flips `imbh/tracing` on for the whole workspace under Cargo feature unification, silently changing what the default `--workspace` build/test compiles. `imbh-tracing` depends on `imbh` with default features; the host enables `imbh/tracing` itself.
- **Workspace-shared dep features union.** `DbLayer` only needs `tracing-subscriber`'s `registry` layer (for `LookupSpan`), but the crate compiles unchanged because the workspace `tracing-subscriber` carries `fmt`+`env-filter` (for the console) and Cargo unions features across all members. `imbh-tracing`'s graph therefore still links the fmt/env-filter code it no longer uses; a truly minimal `imbh-tracing` would need its own non-workspace `tracing-subscriber` pinned to `registry` only.
- **`clippy::needless_doctest_main`** fires on an explicit `fn main() { … }` in a doc example even when the natural shape is "call this in `main`" — drop the wrapper and let the doctest harness supply it (`-D warnings` makes it a hard error).
- **Emit / render / sink are three independent opt-ins — keep them as three features.** Two rebuilds came from conflating them: "wire tracing" was interpreted first as emit-only, then as a console helper crate, then correctly as imbh-as-a-tracing-backend (the `DbLayer` sink). When a request is a single verb over an observability system that both produces and stores telemetry, pin down producer-vs-consumer direction before building.
- **Grep the stale *claim*, not just the crate name.** The obsolete "`imbh-tracing` owns the fmt subscriber / sole direct owner of `tracing-subscriber`" assertion was duplicated across OVERVIEW.md, ARCHITECTURE.md (§11, §12, dependency-tier paragraph), and README.md — sweep for the sentence's meaning (`console collector`, `fmt.subscriber`, `sole direct owner`), not the crate token alone.
- **Don't overwrite methodology-specific measured numbers with an ad-hoc count.** ARCHITECTURE §11's `imbhd 281 / facade 275` counts use Appendix C's method; a quick `cargo tree -e no-dev | grep | sort -u | wc -l` gives different absolutes (288 / 293) but the identical `+5` delta. Verify the claim (the delta, and that the default graph pulls neither `tracing-subscriber` nor `imbh-tracing` — via `cargo tree -e no-dev -i`), and leave the baseline number the documented methodology produced.
- **Resolved gate quirk:** the earlier `footprint-gate.sh` `datafusion: NO` false negative (its `grep 'datafusion v'` over `cargo tree -p imbh --edges normal` missed the edge form) no longer reproduces — datafusion is now a direct top-level facade edge (`├── datafusion v54.0.0`) and the grep matches. No script change needed.

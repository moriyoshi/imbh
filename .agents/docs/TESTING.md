# Testing

The test strategy for IMBH, derived from `OVERVIEW.md` §13 (milestones) and the storage /
search / query design. All three layers below are **built**: per-crate unit suites, the footprint
gate, and a Layer-3 E2E suite (server HTTP wire, crash/recovery, malformed input, tri-signal
lifecycle, exporter-through-SDK, example smoke, interleave stress, and an opt-in RSS soak). Shared
E2E helpers live in the dev-only `imbh-test-support` crate. Keep this document in sync as tests land.

The one-stop "what do I run before I say done" checklist is [QUALITY_GATE.md](./QUALITY_GATE.md);
this file is the deeper strategy.

## Layer 1: per-crate unit and integration tests (`cargo test`)

```sh
cargo test --workspace          # or focused: cargo test -p imbh-storage
```

Must run with **no external daemons and no network** in the default path. Everything IMBH does
is testable in-process against a temp directory. Target coverage per crate:

- **`imbh-otlp`** — OTLP decode → normalized rows. Round-trip prost encode/decode; malformed and
  partial payloads; resource/scope/attribute normalization; severity-number and span-kind
  mapping.
- **`imbh-storage`** — the durability core, where the highest-value tests live:
  - **WAL replay is idempotent** — replay from an LSN watermark must not double-apply. Crash
    injection: truncate the WAL mid-record, kill between seal and manifest append, orphaned
    segment cleanup on recovery.
  - **Seal / freeze-and-swap** — sealing the mutable buffer is an explicit O(1) swap; assert no
    rows are lost or duplicated across a seal boundary.
  - **Parquet segment round-trip** — write a segment, read it back, assert schema and values
    (including the canonical-JSON attribute columns and the promoted `service` column).
  - **Manifest delta-log + checkpoint** — append deltas, checkpoint, recover; atomic-rename
    behavior; retention/compaction correctness.
- **`imbh-index`** — Tantivy schema/build/search and the **Tantivy→`RowSelection` bridge**:
  full-text/term hits map to the correct Parquet row ordinals; the cost gate picks index vs
  scan correctly; the fallback `matches()` path shares the *same* tokenizer as the index (a
  known correctness trap — see `ARCHITECTURE.md` §8/§9.2).
- **`imbh-query`** — DataFusion providers, UDFs, and typed plans: each typed query
  (`LogQuery`, `TraceQuery`, `MetricQuery`, span/RED metrics) compiles to the expected plan and
  returns correct results over fixture segments; cumulative→rate windowing; delta→cumulative
  accumulation (stateful, at ingest).
- **`imbh`** (facade) — the public API contract: async and blocking twins agree;
  `IngestReceipt.durable` semantics; `PageCursor` paging has no tie bugs; `close(&self)` is
  idempotent.

Gate anything that needs privileges or large fixtures behind an opt-in that self-skips (so the
default `cargo test --workspace` stays hermetic).

## Layer 2: footprint regression (the M0 probe)

Footprint is a first-class requirement, so it is tested, not just hoped for. `ARCHITECTURE.md`
Appendix C inlines the M0 probe that links and exercises DataFusion (SQL + Parquet round-trip),
Tantivy (mmap index + search), and OTLP prost encode/decode, then reads `/proc/self/status` for
`VmRSS`/`VmHWM`. Recreate it under `.agents-workspace/tmp` (or as a tracked `examples/` crate)
and compare crate count / binary size / RSS to the Appendix C baseline (~269 crates, ~31.9 MiB,
~36 MB RSS). Wire this as a CI gate at M0 (`OVERVIEW.md` §13). See
[QUALITY_GATE.md](./QUALITY_GATE.md) §2.

## Layer 3: end-to-end / reference-server — built

The reference `imbhd` binary and the `examples/` wirings are the integration surface. The E2E suite
exercises each real boundary end to end; shared builders / harness / a blocking HTTP client / a
`VmRSS` reader live in the dev-only `imbh-test-support` crate (never a dependency of a shipping
crate, so the footprint graph is unchanged). Unless noted, these run in the default
`cargo test --workspace` path:

- **Server HTTP wire** — `crates/imbh-server/tests/http_e2e.rs`. Binds a real `127.0.0.1:0` loopback
  socket, runs `serve()` on a thread, and drives it with the blocking HTTP/1.1 client: OTLP ingest →
  `/api/query` round-trip for all three signals, `/stats` · `/health` · `/admin/*`, and the
  `400` (malformed protobuf / bad SQL) and `404` error paths. Loopback only — no external network or
  daemon, so it stays within the hermetic rule.
- **Crash / recovery** — `crates/imbh/tests/crash_recovery.rs` re-execs a writer process, SIGKILLs it
  after durable ingest, and asserts exactly-once WAL replay on reopen. Deterministic *mid-seal*
  hazards live in `crates/imbh/tests/crash_points.rs`, gated behind imbh-storage's off-by-default
  `fault-injection` feature (segment-on-disk-but-manifest-stale, and manifest-durable-but-WAL-
  un-reclaimed). Run: `cargo test -p imbh --features fault-injection --test crash_points`.
- **Malformed OTLP** — `crates/imbh/tests/malformed_otlp.rs`: garbage/partial protobuf is rejected
  (never panics) across the async and `try_ingest_*` paths; the DB stays usable.
- **Tri-signal lifecycle** — `crates/imbh/tests/lifecycle.rs`: ingest logs+traces+metrics → typed +
  SQL query → seal → reopen → compact → Arrow-IPC export round-trip → idempotent close, with
  async/blocking parity; plus a focused retention-drop test.
- **Exporter through the SDK** — `crates/imbh-otel-exporter/tests/sdk_e2e.rs`: real
  `SdkTracerProvider`/`SdkLoggerProvider`/`SdkMeterProvider` → IMBH exporters → on-disk `Db`, sealed
  and read back through an independent read-only handle.
- **Example smoke** — `examples/*/tests/smoke.rs` run each example binary (via `CARGO_BIN_EXE_*`) and
  assert exit 0 + an expected stdout marker; `bench`/`rss-probe` take a tiny workload arg.
- **Interleave stress** — `crates/imbh/tests/maintenance_stress.rs`: background maintenance + a
  writer thread + concurrent query/compact, asserting `count(*) == count(DISTINCT)` and monotonic
  growth. A heavier variant is `#[ignore]`.
- **RSS soak (opt-in)** — `crates/imbh/tests/soak_rss.rs` (`#[ignore]`, Linux only): a sustained
  ingest→seal→query loop asserts steady-state `VmRSS` stays under a runaway sentinel (closes the
  QUALITY_GATE §2 unmeasured-RSS gap). Run: `cargo test -p imbh --test soak_rss -- --ignored --nocapture`.

## Conventions

- Regression tests ship **with** the fix, in the same change.
- Prefer focused targeting (`cargo test -p <crate>`) while iterating; run the full workspace
  before declaring done.
- Use temp directories (`tempfile`) for anything touching the filesystem; never write test
  artifacts into the tree.

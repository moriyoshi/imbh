# Long-Term Memory Index

Durable, topic-organised project knowledge distilled from `JOURNAL.md` by the `good-sleep` and
`deep-sleep` skills. See the `## LTM Consolidation Record` at the end of
[../JOURNAL.md](../JOURNAL.md) for the section-to-document mapping.

These are topic references, not a chronological log: each merges every journal entry on its subject
into one timeless orientation. They are meant to be edited and refined over time, unlike the
append-only `JOURNAL.md`. For canonical design, [ARCHITECTURE.md](../ARCHITECTURE.md) and
[OVERVIEW.md](../OVERVIEW.md) remain authoritative; these documents capture the implementation
knowledge and findings behind them.

## Synthesis documents

Higher-level syntheses that merge overlapping source topics are produced by the `deep-sleep` skill.
None exist yet — the documents below are all direct topic references.

| Document | Consolidates | Summary |
|----------|--------------|---------|
| _(none yet)_ | | |

## Source documents

| Document | Summary |
|----------|---------|
| [storage-engine.md](storage-engine.md) | WAL with watermark-gated idempotent replay, append-only manifest delta log with checkpoints, immutable-segment compaction, retention deletion, opt-in async ingest with group-commit fsync, and the `Lsn = NonZero<u64>` / `Option<Lsn>` durability contract |
| [full-text-search-tantivy-bridge.md](full-text-search-tantivy-bridge.md) | Per-segment `.tidx` Tantivy index, shared `imbh-core` tokenizer, the cost-gated `Inexact`-pushdown Tantivy→Parquet RowSelection bridge, span-name search, and `NoMergePolicy` |
| [otlp-and-metrics-data-model.md](otlp-and-metrics-data-model.md) | OTLP decode of spans and all five metric types (gauge, sum, explicit/exponential histogram, summary) into queryable tables, with quantile UDFs, rates, cross-series merges, exemplars, and metrics-math hardening |
| [query-engine-and-typed-apis.md](query-engine-and-typed-apis.md) | imbh-query's DataFusion providers, RowSelection bridge, UDFs, bind-params, scan stats and lazy scan; the typed Logs/Metrics/Traces/Attrs APIs and JSON parser; the MatchOp + PromQL matcher vocabulary; attribute promotion; and the serde/proto binding surfaces |
| [traces-and-error-model.md](traces-and-error-model.md) | The typed `traces()` API, span RED `span_metrics`, span-name text search, trace-query correctness (trace_start boundary fix, streaming TraceQL with predicate/numeric pushdown), and imbh-core's typed nested `Error` model |
| [cross-process-concurrency.md](cross-process-concurrency.md) | Single-writer `writer.lock` plus N `Db::open_read_only` readers answering each query from a manifest-segments ∪ live-WAL-tail snapshot, correct under concurrent seal/reclaim/retention |
| [self-observability-tracing.md](self-observability-tracing.md) | Feature-gated (`imbh/tracing`, off by default) internal instrumentation across ingest→storage→query, the three severable collection opt-ins (emit / render console / `imbh_tracing::DbLayer` sink), and their footprint containment |
| [footprint-and-feature-gating.md](footprint-and-feature-gating.md) | The crate-count/binary-size/RSS footprint discipline, the footprint gate + cargo-deny enforcement, the M0 budget baseline, the forced opentelemetry_sdk dependency, and the `search` and producer/consumer (`ingest`/`query`) feature levers |
| [reference-server-exporter-and-ops.md](reference-server-exporter-and-ops.md) | The std-only reference `imbhd` HTTP server (OTLP ingest, query, health, stats, admin flush/compact, optional `grpc`), the Db ops surface (stats/snapshot/export/engine gauges), and the `imbh-otel-exporter` SDK-exporter trio |
| [docker-log-driver-plugin.md](docker-log-driver-plugin.md) | The `docker.logdriver/1.0` managed plugin: its threading shape, the measured Docker networking findings (`network.type: bridge` is a no-op, `host-gateway` semantics, the reachability envelope), runtime bridge-network discovery via the Engine API or `getifaddrs` and the `StartLogging` deadlock that shapes it, `propagatedMount` storage, per-architecture publishing, and the zero-added-crate feature gating |
| [imbh-lgtm-languages-and-arrow-reads.md](imbh-lgtm-languages-and-arrow-reads.md) | The `imbh-lgtm` crate: bounded LogQL/TraceQL/PromQL semantics and translators, the IMBH LogQL `\|?`/`!?` Tantivy dialect, the Arrow-native `*_batches` result surface, the Cow/`self_cell` borrowed-read refactor, the Level-2 raw-Arrow reads, and the upstream boundary-fidelity rules (PromQL left-open lookback, TraceQL absent-attribute negation) |
| [imbh-tui-and-gen-demo-db.md](imbh-tui-and-gen-demo-db.md) | The `imbh-tui` terminal explorer (route-based navigation, focus ring, MC-style chrome, trace/log/metric viewers, time-window controls, East-Asian-width-safe rendering, the "atta" mascot) and the `examples/gen-demo-db` demo-data generator |
| [project-meta-ci-docs-and-testing.md](project-meta-ci-docs-and-testing.md) | The coding-agent harness, the OVERVIEW/ARCHITECTURE doc split and README/comparison matrix, the benchmark and Layer-3 E2E suites, GitHub Actions CI with its license gate, the cargo-release publish harness, and the imbh-go binding prescription |

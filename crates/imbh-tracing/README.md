# imbh-tracing

In-process `tracing` → IMBH plumbing.

> Part of **[IMBH](https://github.com/moriyoshi/imbh)** — a small-footprint, embeddable
> observability database for Rust that ingests OpenTelemetry logs, traces, and metrics and answers
> queries through Apache DataFusion (SQL) and Tantivy (full-text search), all in-process with no
> server or network hop.

`imbh-tracing` provides `DbLayer`, a [`tracing_subscriber::Layer`] that ingests `tracing` events →
the `logs` table and span closes → the `spans` table of an embedded
[`imbh::Db`](https://crates.io/crates/imbh), in-process, over the same OTLP ingest path a network
exporter would hit. This is the self-observation story: your app's (and IMBH's own) `tracing` lands
in IMBH with zero network hops.

```rust,ignore
use tracing_subscriber::prelude::*;
let db = imbh::Db::in_memory().open()?;
tracing_subscriber::registry()
    .with(imbh_tracing::DbLayer::new(db.clone()).with_service("checkout"))
    .init();
// ... emit spans/events anywhere; query them back via `db.sql("SELECT … FROM logs/spans")`.
```

The companion stderr *renderer* (a `fmt` subscriber that prints IMBH's instrumentation to the
terminal) lives in the `imbh` facade as `imbh::console`, behind its off-by-default
`tracing-console` feature — the two are independent and compose on the same registry. To export
from an `opentelemetry_sdk` pipeline instead of the `tracing` crate, see
[`imbh-otel-exporter`](https://crates.io/crates/imbh-otel-exporter).

## Role in the workspace

Depends on the [`imbh`](https://crates.io/crates/imbh) facade and `tracing-subscriber`. A
leaf/companion crate: `imbh ← imbh-tracing`.

See the design reference [`ARCHITECTURE.md`](https://github.com/moriyoshi/imbh/blob/main/.agents/docs/ARCHITECTURE.md)
§11 (self-observability), §12. License: Apache-2.0.

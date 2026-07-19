# imbh-otel-exporter

opentelemetry-rust exporter adapters that write directly into an embedded IMBH `Db`.

> Part of **[IMBH](https://github.com/moriyoshi/imbh)** — a small-footprint, embeddable
> observability database for Rust that ingests OpenTelemetry logs, traces, and metrics and answers
> queries through Apache DataFusion (SQL) and Tantivy (full-text search), all in-process with no
> server or network hop.

Instead of shipping spans to a collector over the network, an in-process `opentelemetry_sdk`
pipeline can export straight into IMBH — self-observation with zero hops. Each adapter converts the
SDK's batch types into OTLP protobuf (reusing `opentelemetry-proto`'s SDK→tonic transforms, already
in the dependency tree) and feeds the bytes to the same `Db::ingest_otlp_*` path a network exporter
would hit, so ingest, WAL, and query behave identically.

```rust,ignore
use opentelemetry_sdk::trace::{SdkTracerProvider, SimpleSpanProcessor};
let db = imbh::Db::in_memory().open()?;
let provider = SdkTracerProvider::builder()
    .with_span_processor(SimpleSpanProcessor::new(
        imbh_otel_exporter::ImbhSpanExporter::new(db.clone()),
    ))
    .build();
// … emit spans through `provider`; they land in `db`.
```

Scope: `ImbhSpanExporter` (traces), `ImbhLogExporter` (logs), and `ImbhMetricExporter` (metrics) —
the full OTLP signal set, each a thin `SDK batch → transform::… → OTLP bytes → ingest_otlp_*`
adapter.

## Role in the workspace

Depends on the [`imbh`](https://crates.io/crates/imbh) facade and the opentelemetry SDK. A
leaf/companion crate: `imbh ← imbh-otel-exporter`. For sinking the `tracing` crate's spans/events
into IMBH instead, see [`imbh-tracing`](https://crates.io/crates/imbh-tracing).

See the design reference [`ARCHITECTURE.md`](https://github.com/moriyoshi/imbh/blob/main/.agents/docs/ARCHITECTURE.md)
§12. License: Apache-2.0.

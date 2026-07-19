# imbh-core

The arrow-free foundation crate for IMBH.

> Part of **[IMBH](https://github.com/moriyoshi/imbh)** — a small-footprint, embeddable
> observability database for Rust that ingests OpenTelemetry logs, traces, and metrics and answers
> queries through Apache DataFusion (SQL) and Tantivy (full-text search), all in-process with no
> server or network hop.

`imbh-core` holds IMBH's pure domain types with **no** heavy dependencies: the OpenTelemetry
value model (`AnyValue`), the one shared canonical-JSON encoder (ARCHITECTURE.md §6.1), the
normalized ingest rows (`LogRow`, `SpanRow`, and the metric row families), ids (`Lsn`, `SpanId`,
`TraceId`), config, the error model, manifest types (`SegmentRef`), the shared tokenizer
(`tokenize` / `matches_terms`), and time types (`TimeRange`, `Timestamp`, `DurationNs`).

It deliberately does **not** depend on arrow, parquet, DataFusion, Tantivy, or serde — those
engine dependencies live behind the `imbh-storage` / `imbh-query` / `imbh-index` boundaries
(ARCHITECTURE.md §12). Keeping the shared vocabulary here is what lets every other crate agree on
one row shape and one JSON encoding without pulling an engine.

## Role in the workspace

The root of the dependency graph: `core ← {otlp, storage, index, query} ← imbh ← {exporter, server}`.
Everything depends on `imbh-core`; it depends on nothing internal. The `imbh` facade
([crates.io](https://crates.io/crates/imbh)) is what a host links against.

See the design reference [`ARCHITECTURE.md`](https://github.com/moriyoshi/imbh/blob/main/.agents/docs/ARCHITECTURE.md)
§6, §12. License: Apache-2.0.

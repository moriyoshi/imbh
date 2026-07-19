# imbh-storage

The IMBH storage engine: mutable buffer, seal → Parquet segment, and manifest.

> Part of **[IMBH](https://github.com/moriyoshi/imbh)** — a small-footprint, embeddable
> observability database for Rust that ingests OpenTelemetry logs, traces, and metrics and answers
> queries through Apache DataFusion (SQL) and Tantivy (full-text search), all in-process with no
> server or network hop.

`imbh-storage` owns the durable write path and the Arrow schema. It provides a WAL with XXH3-64
frames and idempotent replay gated on a manifest watermark; a mutable buffer of normalized rows; a
`seal()` that sorts by time and writes an immutable Parquet segment (plus a `.tidx` Tantivy
sidecar) via temp→rename before bumping the durable watermark; a whole-file manifest carrying that
watermark; and `retain()` (age and disk-budget retention).

This crate touches arrow/parquet directly (no DataFusion), pinned to the exact versions the query
engine resolves so the whole workspace unifies on a single arrow version (ARCHITECTURE.md §9.1);
this lets an ingest-only producer build drop the query engine entirely. It defines the canonical
table schemas (`logs`, `spans`, and the metric tables) that the query layer reads back.

The `search` feature (on by default for standalone builds) wires the per-segment Tantivy index via
[`imbh-index`](https://crates.io/crates/imbh-index).

## Role in the workspace

Depends on [`imbh-core`](https://crates.io/crates/imbh-core) (and
[`imbh-index`](https://crates.io/crates/imbh-index) under `search`). Consumed by the
[`imbh`](https://crates.io/crates/imbh) facade: `core ← imbh-storage ← imbh`.

See the design reference [`ARCHITECTURE.md`](https://github.com/moriyoshi/imbh/blob/main/.agents/docs/ARCHITECTURE.md)
§7 (storage), §5, §9.1, §12. License: Apache-2.0.

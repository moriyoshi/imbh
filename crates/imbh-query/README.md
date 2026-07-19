# imbh-query

The IMBH query layer: DataFusion session config, providers, and SQL over buffer + segments.

> Part of **[IMBH](https://github.com/moriyoshi/imbh)** — a small-footprint, embeddable
> observability database for Rust that ingests OpenTelemetry logs, traces, and metrics and answers
> queries through Apache DataFusion (SQL) and Tantivy (full-text search), all in-process with no
> server or network hop.

`imbh-query` is **the only crate that knows the DataFusion query engine**. It configures the
session per ARCHITECTURE.md §9.1 (`target_partitions = 1`, small `batch_size` favoring RSS over
throughput, a `GreedyMemoryPool` sized from the memory budget), ships the `matches` text-search UDF
and the other query UDFs, and registers the custom table provider that unions the mutable-buffer
snapshot with the sealed Parquet segments.

The provider applies the cost-gated Tantivy → Parquet `RowSelection` bridge (§9.2): when a query's
text predicate is selective enough, term hits from
[`imbh-index`](https://crates.io/crates/imbh-index) prune the Parquet scan; otherwise it falls back
to the row-wise `matches` UDF, which returns the identical row set.

The `search` feature governs the Tantivy pushdown path; without it the crate is a pure DataFusion
SQL layer over Parquet + buffer.

## Role in the workspace

Depends on [`imbh-core`](https://crates.io/crates/imbh-core) (and
[`imbh-index`](https://crates.io/crates/imbh-index) under `search`), plus DataFusion. Consumed by
the [`imbh`](https://crates.io/crates/imbh) facade: `core ← imbh-query ← imbh`.

See the design reference [`ARCHITECTURE.md`](https://github.com/moriyoshi/imbh/blob/main/.agents/docs/ARCHITECTURE.md)
§9 (query), §9.1, §9.2, §9.3, §12. License: Apache-2.0.

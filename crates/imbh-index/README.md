# imbh-index

Full-text / term search for IMBH — the Tantivy integration.

> Part of **[IMBH](https://github.com/moriyoshi/imbh)** — a small-footprint, embeddable
> observability database for Rust that ingests OpenTelemetry logs, traces, and metrics and answers
> queries through Apache DataFusion (SQL) and Tantivy (full-text search), all in-process with no
> server or network hop.

`imbh-index` is **the only crate that knows Tantivy**. It builds one Tantivy index per sealed
`logs` / `spans` segment, aligned to Parquet rows via a `row` u64 fast field (the row ordinal is
stored as data, never inferred from doc order). Nothing is kept in Tantivy's docstore — Parquet is
the store, and the index is purely a row-pruning accelerator.

The body field is tokenized with `imbh_core::tokenize` wrapped as a Tantivy analyzer, so index
hits and the row-wise `matches` fallback in [`imbh-query`](https://crates.io/crates/imbh-query)
return identical row sets. The schema is `body` (tokenized) + `service` / `severity_text` (raw) +
`attrs` (a JSON field of string-valued attributes, indexed verbatim) + `row` (fast). `search_body`
returns sorted row ordinals for a body term query and `search_attr_eq` does the same for an
`attrs.<key> = <value>` exact match, feeding the cost-gated `RowSelection` bridge in the query
layer.

## Role in the workspace

Depends on [`imbh-core`](https://crates.io/crates/imbh-core) and Tantivy. Used by
[`imbh-storage`](https://crates.io/crates/imbh-storage) (index build at seal) and
[`imbh-query`](https://crates.io/crates/imbh-query) (row selection at scan):
`core ← imbh-index ← {storage, query}`.

See the design reference [`ARCHITECTURE.md`](https://github.com/moriyoshi/imbh/blob/main/.agents/docs/ARCHITECTURE.md)
§8 (search), §9.2, §12. License: Apache-2.0.

# imbh-lgtm

LGTM-stack query-language compatibility for IMBH — PromQL, LogQL, and TraceQL.

> Part of **[IMBH](https://github.com/moriyoshi/imbh)** — a small-footprint, embeddable
> observability database for Rust that ingests OpenTelemetry logs, traces, and metrics and answers
> queries through Apache DataFusion (SQL) and Tantivy (full-text search), all in-process with no
> server or network hop.

`imbh-lgtm` implements the query surfaces of the Grafana LGTM observability stack — PromQL
(Prometheus/Mimir), LogQL (Loki), and TraceQL (Tempo) — as bounded, explicitly-versioned
compatibility profiles. It is deliberately stack-specific, not a neutral cross-ecosystem query
layer: constructs outside the advertised profiles are rejected with a stable diagnostic rather than
silently approximated.

Two layers, kept as separate modules:

- `model` — parser- and engine-independent expression models plus the reference evaluators (the
  "semantics"). Depends only on `regex`.
- `syntax` — source-positioned parsers that lower PromQL / LogQL / TraceQL text into an
  `ImbhQueryModel`, emitting a `Diagnostic` for anything outside the advertised profiles.

Under the optional `source` feature the crate also owns the native IMBH source adapters and the
`*SemanticsExt` execution traits, which depend on the
[`imbh`](https://crates.io/crates/imbh) facade (and thus DataFusion/Tantivy). That subtree is
feature-gated so parse-only or evaluate-only consumers stay light.

## Role in the workspace

`model` + `syntax` depend only on `regex`; the `source` feature adds the `imbh` facade. Consumed by
[`imbh-tui`](https://crates.io/crates/imbh-tui) and available to hosts wanting LGTM-compatible query
entry points.

See the design references
[`ARCHITECTURE.md`](https://github.com/moriyoshi/imbh/blob/main/.agents/docs/ARCHITECTURE.md) and
[`OVERVIEW.md`](https://github.com/moriyoshi/imbh/blob/main/.agents/docs/OVERVIEW.md) §13.
License: Apache-2.0.

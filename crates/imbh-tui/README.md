# imbh-tui

A read-only terminal explorer for IMBH metrics, traces, and logs.

> Part of **[IMBH](https://github.com/moriyoshi/imbh)** — a small-footprint, embeddable
> observability database for Rust that ingests OpenTelemetry logs, traces, and metrics and answers
> queries through Apache DataFusion (SQL) and Tantivy (full-text search), all in-process with no
> server or network hop.

`imbh-tui` is a companion TUI (built on ratatui / crossterm) that opens an IMBH database read-only
and lets you browse the three signals interactively: metric charts over selectable relative time
ranges, trace search, and log views. Query entry uses the LGTM-compatible surfaces from
[`imbh-lgtm`](https://crates.io/crates/imbh-lgtm) — PromQL, LogQL, and TraceQL — translated onto the
native IMBH query APIs.

It is a viewer, not a writer: it never ingests, and opening a database read-only means it composes
with a live single writer plus many cross-process readers.

## Role in the workspace

Depends on the [`imbh`](https://crates.io/crates/imbh) facade and
[`imbh-lgtm`](https://crates.io/crates/imbh-lgtm) (with its `source` feature). A leaf binary crate:
`{imbh, imbh-lgtm} ← imbh-tui`.

See the design reference [`OVERVIEW.md`](https://github.com/moriyoshi/imbh/blob/main/.agents/docs/OVERVIEW.md)
§13. License: Apache-2.0.

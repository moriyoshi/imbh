# imbh-otlp

OTLP decode → normalized IMBH rows.

> Part of **[IMBH](https://github.com/moriyoshi/imbh)** — a small-footprint, embeddable
> observability database for Rust that ingests OpenTelemetry logs, traces, and metrics and answers
> queries through Apache DataFusion (SQL) and Tantivy (full-text search), all in-process with no
> server or network hop.

`imbh-otlp` owns the OTLP wire types (prost message types only, no tonic services) and the
normalization step that turns an OpenTelemetry export request into IMBH's row model:
`ExportLogsServiceRequest` → `Vec<LogRow>`, and the equivalent paths for traces and metrics.
Attribute scopes are encoded to canonical JSON here via the one shared encoder in
[`imbh-core`](https://crates.io/crates/imbh-core), so the bytes that reach storage are already
dict-ready (ARCHITECTURE.md §6.1).

Both a decode-from-bytes path (`decode_logs_to_rows`) and a decode-skipping path for already
in-process messages are exposed, so the self-observation exporters and the network ingest path
share one normalization.

## Role in the workspace

Depends only on [`imbh-core`](https://crates.io/crates/imbh-core) (plus `opentelemetry-proto` /
`prost`). Sits on the ingest half of the pipeline: `core ← imbh-otlp ← imbh`. The
[`imbh`](https://crates.io/crates/imbh) facade drives it from `ingest_otlp_*`.

See the design reference [`ARCHITECTURE.md`](https://github.com/moriyoshi/imbh/blob/main/.agents/docs/ARCHITECTURE.md)
§5 (ingest), §6.1, §12. License: Apache-2.0.

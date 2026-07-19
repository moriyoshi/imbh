# imbh-server (`imbhd`)

The reference HTTP server wiring for IMBH — one worked example of host wiring, **not the product**.

> Part of **[IMBH](https://github.com/moriyoshi/imbh)** — a small-footprint, embeddable
> observability database for Rust that ingests OpenTelemetry logs, traces, and metrics and answers
> queries through Apache DataFusion (SQL) and Tantivy (full-text search). The product is the
> **library**; this crate is just one example of exposing it over HTTP.

`imbhd` is a deliberately tiny HTTP/1.1 server over `std::net` (thread-per-connection) that shows
one way a host can expose the [`imbh`](https://crates.io/crates/imbh) library over HTTP. There is
no axum/hyper, so it adds no heavy dependencies and keeps the footprint story intact
(ARCHITECTURE.md §10.16).

Routes:

- `POST /v1/logs` · `/v1/traces` · `/v1/metrics` — OTLP/HTTP protobuf ingest (uncompressed).
- `POST /api/query` — a SQL string body → JSON rows.
- `GET /stats` — DB operational stats (per-table counts + buffer/WAL bytes + durable LSN) as JSON.
- `POST /admin/flush` · `/admin/compact` — maintenance actions. Unauthenticated by design; a real
  deployment gates `/admin/*` itself.
- `GET /health` — liveness.

OTLP/gRPC is available on a second port behind the optional `grpc` feature; the default build
carries no gRPC transport (the whole tonic/hyper subtree is gated off). Not handled here
(follow-ups): gzip request bodies, TLS, and the OTLP partial-success response shape.

## Role in the workspace

A leaf that depends on the [`imbh`](https://crates.io/crates/imbh) facade: `imbh ← imbh-server`.
Ships the `imbhd` binary.

See the design reference [`ARCHITECTURE.md`](https://github.com/moriyoshi/imbh/blob/main/.agents/docs/ARCHITECTURE.md)
§10.16, §10.1, §12. License: Apache-2.0.

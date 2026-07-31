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

## Connection deadlines

The server is thread-per-connection, so an idle client costs a thread. Two deadlines bound that, and
they are deliberately different rules:

- `IMBH_HEADER_TIMEOUT` (default `10s`) bounds the request line + headers **in total**. A client that
  connects and says nothing — or trickles a byte at a time, never idle and never finished — is answered
  `408 Request Timeout` and disconnected.
- `IMBH_BODY_TIMEOUT` (default `30s`) is a **per-read** allowance for the body, and the write allowance
  for the response. A large OTLP body over a slow link is fine as long as it keeps making progress; one
  that stalls mid-transfer gets a 408 and ingests nothing.

`0` on either disables that deadline (`IoTimeouts::DISABLED` is both), which is the right choice behind
a proxy that already sheds slow clients.

These interact with the shutdown drain below, and the defaults do not line up: an idle connection is only
cut off *before* the drain gives up if `IMBH_HEADER_TIMEOUT` is shorter than `IMBH_SHUTDOWN_TIMEOUT`
(stock values are `10s` and `5s`, i.e. the other way round). Set the header timeout under the drain if you
want shutdown to finish early rather than report the connection abandoned.

## Graceful shutdown

`SIGINT`/`SIGTERM` (Ctrl-C, `docker stop`, systemd, `kill`) wind `imbhd` down in order: every listener
stops accepting, in-flight requests get `IMBH_SHUTDOWN_TIMEOUT` (default `5s`) to finish, the Docker
plugin's queued container lines are drained into the database, and `Db::close()` seals the buffer — so
the rows accepted a moment earlier are in Parquet segments and the next start replays nothing. The exit
status is 0; a **second** signal skips all of it and exits with `128 + signum`.

Library-side that is the `Shutdown` token plus `serve_until` / `serve_plugin_until` /
`serve_grpc_until`, which a host can drive on its own schedule (`shutdown.trigger()`) without any
signal handling at all. Signal handling itself is Unix-only and costs **no dependency**: `libc` is
already in the graph. See the `shutdown` module docs for why `accept` is woken rather than polled.

## Role in the workspace

A leaf that depends on the [`imbh`](https://crates.io/crates/imbh) facade: `imbh ← imbh-server`.
Ships the `imbhd` binary.

See the design reference [`ARCHITECTURE.md`](https://github.com/moriyoshi/imbh/blob/main/.agents/docs/ARCHITECTURE.md)
§10.16, §10.1, §12. License: Apache-2.0.

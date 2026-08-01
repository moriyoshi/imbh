# imbh-server (`imbhd`)

The reference HTTP server wiring for IMBH — one worked example of host wiring, **not the product**.

> Part of **[IMBH](https://github.com/moriyoshi/imbh)** — a small-footprint, embeddable
> observability database for Rust that ingests OpenTelemetry logs, traces, and metrics and answers
> queries through Apache DataFusion (SQL) and Tantivy (full-text search). The product is the
> **library**; this crate is just one example of exposing it over HTTP.

`imbhd` is an HTTP/1.1 server on **axum over hyper** that shows one way a host can expose the
[`imbh`](https://crates.io/crates/imbh) library over HTTP (ARCHITECTURE.md §10.16). That costs the
*library* nothing: the footprint budget is written against the `imbh` facade's dependency graph
(`scripts/footprint-gate.sh` counts `cargo tree -p imbh`) and the dependency direction is
`imbh ← imbh-server`, so nothing this crate links reaches it. `imbh-server` is optional and always
was.

Routes:

- `POST /v1/logs` · `/v1/traces` · `/v1/metrics` — OTLP/HTTP protobuf ingest. `Content-Encoding:
  gzip` is accepted, which the OpenTelemetry Collector's `otlphttp` exporter sends by default.
- `POST /api/query` — a SQL string body → JSON rows.
- `POST /mcp` — the Model Context Protocol endpoint (below). `GET`/`DELETE` there answer `405`.
- `GET /stats` — DB operational stats (per-table counts + buffer/WAL bytes + durable LSN) as JSON.
- `POST /admin/flush` · `/admin/compact` — maintenance actions. Unauthenticated by design; a real
  deployment gates `/admin/*` itself.
- `GET /health` — liveness.

OTLP/gRPC is available on a second port behind the optional `grpc` feature; the default build carries
no gRPC transport. It is a cheap feature now — hyper, tower, and axum are already in the default
graph, so `grpc` adds only tonic and h2 on top. Not handled here (follow-ups): TLS and the OTLP
partial-success response shape.

The Docker logging-driver plugin (`docker` feature, Unix only) is a different protocol on a Unix
socket, but runs on this same stack and shares its request handling, so the body limits, deadlines,
and decoding are identical on both sockets. Its one streaming endpoint, `LogDriver.ReadLogs`, is
served as a `Transfer-Encoding: chunked` body fed by a bounded channel.

## MCP endpoint

`POST /mcp` serves the **Model Context Protocol** over its Streamable HTTP transport, so an agent can
search logs, pull traces, and query metrics through the same process that ingests them — no Grafana,
no datasource proxy, no export step:

```sh
claude mcp add --transport http imbh http://127.0.0.1:4318/mcp
```

The 15 tools are read-only (`search_logs`, `search_traces`, `get_trace`, `span_metrics`,
`query_metric_range`, `histogram_quantile`, `query_sql`, attribute discovery, `db_stats`, …) —
nothing there can ingest, flush, compact, or apply retention. The endpoint is on in the default
build and adds **no dependency**: MCP is JSON-RPC over HTTP, and this crate already hand-rolls its
JSON.

Both protocol eras are served: the stateless `2026-07-28` revision (per-request `_meta`,
`server/discover`, validated header mirror) and the `initialize` handshake of `2025-11-25` and
earlier. Nothing streams, so responses are single JSON bodies and no session is kept.

Like the rest of `imbhd` it is unauthenticated, but it does enforce the transport's DNS-rebinding
defence: a browser `Origin` outside the loopback set is refused `403`, widened by
`IMBH_MCP_ALLOWED_ORIGINS` (comma-separated, or `*`). See
[`docs/MCP.md`](https://github.com/moriyoshi/imbh/blob/main/docs/MCP.md).

## Connection deadlines

Two deadlines bound how long a client may hold a connection without making progress, and they are
deliberately different rules:

- `IMBH_HEADER_TIMEOUT` (default `10s`) bounds the request line + headers **in total**. A client that
  connects and says nothing — or trickles a byte at a time, never idle and never finished — is answered
  `408 Request Timeout` and disconnected.
- `IMBH_BODY_TIMEOUT` (default `30s`) is a **per-read** allowance for the body. A large OTLP body over
  a slow link is fine as long as it keeps making progress; one that stalls mid-transfer gets a 408 and
  ingests nothing.

Two size bounds go with them:

- `IMBH_MAX_BODY` (default `64MiB`) is the largest request body accepted, measured *after* any
  `Content-Encoding` is undone, so a compression bomb is refused on its inflated size. An oversized
  `Content-Length` is refused before a byte is read. Over the cap is `413 Payload Too Large`.
- `IMBH_MAX_CONNECTIONS` (default `512`) caps simultaneous connections. Connections are tasks rather
  than threads now, so this guards against descriptor exhaustion — the default sits under the usual
  1024 soft `RLIMIT_NOFILE` to leave room for parquet and tantivy's own descriptors. What bounds
  actual work is the blocking pool every `Db` call is offloaded to.

`0` disables any of the four (`IoTimeouts::DISABLED` is both deadlines), which is the right choice
behind a proxy that already sheds slow clients.

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
already in the graph. Both listeners turn the token into a `oneshot` their async accept loop selects
on, then drain through hyper's `GracefulShutdown`. The plugin's wind-down order differs: its container
readers stop and its ingest queue drains *before* the connection drain, because clearing the stream
registry is also what ends the `docker logs -f` responses still open. See the `shutdown` module docs.

## Role in the workspace

A leaf that depends on the [`imbh`](https://crates.io/crates/imbh) facade: `imbh ← imbh-server`.
Ships the `imbhd` binary.

See the design reference [`ARCHITECTURE.md`](https://github.com/moriyoshi/imbh/blob/main/.agents/docs/ARCHITECTURE.md)
§10.16, §10.1, §12. License: Apache-2.0.

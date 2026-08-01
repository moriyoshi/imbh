# imbh-mcp

The Model Context Protocol server for IMBH: your telemetry as agent tools.

> Part of **[IMBH](https://github.com/moriyoshi/imbh)** — a small-footprint, embeddable
> observability database for Rust that ingests OpenTelemetry logs, traces, and metrics and answers
> queries through Apache DataFusion (SQL) and Tantivy (full-text search), all in-process with no
> server or network hop.

`imbh-mcp` turns an embedded [`imbh::Db`](https://crates.io/crates/imbh) into an **MCP server**: 15
read-only tools an agent can use to search logs, pull traces, and query metrics through the same
process that holds them — no Grafana, no datasource proxy, no export step. Nothing in the tool
surface can ingest, flush, compact, or apply retention.

Two transports, one dispatch:

| Transport | Hosted by | How to run it |
|---|---|---|
| **stdio** — newline-delimited JSON-RPC | the `imbh-tui` binary | `imbh-tui --mcp-stdio ./imbh-data` |
| **Streamable HTTP** — `POST /mcp` | `imbhd` ([`imbh-server`](https://crates.io/crates/imbh-server)) | `imbhd ./imbh-data 127.0.0.1:4318` |

`handle(&db, message, &Transport::…)` is the whole protocol — bytes in, a `Reply` out — so a host
with its own transport can serve MCP without adopting either of those binaries:

```rust,ignore
let db = imbh::Db::in_memory().open()?;
let reply = imbh_mcp::handle(&db, message_bytes, &imbh_mcp::Transport::Stdio).await;
if let Some(body) = reply.body {
    println!("{}", serde_json::to_string(&body)?);
}
```

Both MCP eras are answered: the stateless `2026-07-28` revision (per-request `_meta`,
`server/discover`) and the `initialize` handshake of `2025-11-25` and earlier. Nothing streams, so
there is no SSE and no session.

For a stdio session that must also see what a running `imbhd` still holds in its write buffer, the
`proxy` module forwards each message to that daemon's `POST /mcp` (`imbh-tui --mcp-stdio --url
127.0.0.1:4318`) over hand-written HTTP/1.1 — no HTTP client dependency.

## Role in the workspace

Depends on the [`imbh`](https://crates.io/crates/imbh) facade plus `serde_json` and `base64`, both
of which are already compiled under DataFusion — so the MCP surface costs **no additional crate** in
any dependency graph. A companion crate: `imbh ← imbh-mcp ← {imbh-server, imbh-tui}`.

See [`docs/MCP.md`](https://github.com/moriyoshi/imbh/blob/main/docs/MCP.md) for the tool reference
and the client setup, and the design reference
[`ARCHITECTURE.md`](https://github.com/moriyoshi/imbh/blob/main/.agents/docs/ARCHITECTURE.md)
§10.16.1, §12. License: Apache-2.0.

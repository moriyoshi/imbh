# imbh as an MCP server

`imbhd` — the reference server ([ARCHITECTURE.md §10.16](../.agents/docs/ARCHITECTURE.md)) — serves
the **Model Context Protocol** at `POST /mcp`. An agent connected to it can search logs, pull
traces, and query metrics through the same process that ingests them: no Grafana, no datasource
proxy, no export step, and no second copy of the data.

```
OTLP in ──▶  imbhd  ──▶  imbh Db  ──▶  /mcp (tools)  ──▶  agent
```

This is one worked example of host wiring, like the rest of `imbh-server`. It is on in the default
build and adds **no crate** to the graph: it speaks JSON-RPC through `serde_json` and Base64 through
`base64`, both of which are already compiled under DataFusion (via `arrow-json` and `arrow-cast`), so
the direct dependencies cost nothing.

## Quick start

```sh
imbhd ./imbh-data 127.0.0.1:4318
```

Then point a client at `http://127.0.0.1:4318/mcp`. For Claude Code:

```sh
claude mcp add --transport http imbh http://127.0.0.1:4318/mcp
```

For a client configured by file, the endpoint is an ordinary Streamable HTTP MCP server:

```json
{
  "mcpServers": {
    "imbh": {
      "type": "http",
      "url": "http://127.0.0.1:4318/mcp"
    }
  }
}
```

No handshake is required before calling a tool, and no session is kept, so `curl` is a fine client
too:

```sh
curl -s http://127.0.0.1:4318/mcp \
  -H 'content-type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"tools/list"}'
```

## The tools

All of them are **read-only**: nothing here can ingest, flush, compact, or apply retention. Time
windows default to the last hour; pass `since` (`"15m"`, `"2h"`, `"7d"`) or explicit
`start_unix_nano` / `end_unix_nano` to change that. Every timestamp in and out is epoch nanoseconds.

| Tool | What it answers |
|---|---|
| `db_stats` | Which tables hold data, how many rows/segments, and over what time span. Start here. |
| `list_attribute_keys` · `list_attribute_values` | What can be filtered or grouped by; e.g. the set of `service.name`s. |
| `search_logs` | Log records by service, severity, full-text terms, attributes, or trace id. |
| `count_logs` | How many records match, without materializing them. |
| `log_volume` | Counts per time bucket, optionally broken down by attribute keys. |
| `search_traces` | Traces by service, span name, status, duration, attributes. |
| `get_trace` | Every span of one trace, with attributes, status, and parent links. |
| `span_metrics` | RED metrics from spans: calls, errors, error rate, p50/p95/p99 per bucket. |
| `list_metrics` · `metric_series` | The metric catalogue (name, kind, unit, temporality) and a metric's label sets. |
| `query_metric_range` · `query_metric_instant` | Aggregate a gauge or sum metric over time, or its latest value. |
| `histogram_quantile` | A quantile over time from an explicit-bucket or exponential histogram. |
| `query_sql` | Raw SQL, for anything the typed tools do not cover. |

Prefer the typed tools over `query_sql`: they drive the time and full-text indexes, while raw SQL
scans. `search_logs`'s `matches` argument is the Tantivy-accelerated term search — the cheapest way
to find a phrase in log bodies.

Argument mistakes (a bad duration, an unparseable trace id, a SQL error) come back as tool
*execution* errors, so the model can correct itself and retry rather than seeing a transport
failure.

## Protocol support

The endpoint is **dual-era**, because the protocol changed shape and clients are split across the
two:

- **`2026-07-28`** — the stateless revision: no `initialize`, each request carries its version in
  `params._meta`, `server/discover` reports capabilities, and results carry
  `resultType: "complete"`. The `MCP-Protocol-Version`, `Mcp-Method`, and `Mcp-Name` headers are
  validated against the body, and a mismatch is refused with `-32020`.
- **`2025-11-25`, `2025-06-18`, `2025-03-26`** — the handshake era: `initialize` →
  `notifications/initialized` → `tools/list` / `tools/call`.

`imbhd` picks per request: a `params._meta` protocol version (or a `server/discover` call) selects
the stateless path, anything else the handshake path. A client asking `initialize` for an unknown
revision is answered with the newest handshake-era one.

Responses are always a single `application/json` body — nothing here streams, so no SSE stream is
opened and no session id is minted. `GET /mcp` and `DELETE /mcp` (the older revisions' stream and
session-teardown verbs) answer `405`.

## Exposure

Like the rest of `imbhd`, the endpoint is **unauthenticated** — a real deployment gates it, and the
default `127.0.0.1` bind is the intended posture for a local agent.

What it does enforce is the transport's DNS-rebinding defence: a request carrying a browser `Origin`
outside the loopback set is refused with `403`, so a web page the user merely visits cannot drive
the tools on their loopback `imbhd`. Non-browser clients send no `Origin` and are unaffected. To
allow a browser-based client:

```sh
IMBH_MCP_ALLOWED_ORIGINS='https://app.example.com' imbhd
```

Comma-separate several origins; the single value `*` disables the check.

Remember what the same port also serves: OTLP ingest on `/v1/*` and `POST /admin/flush` ·
`/admin/compact`. Exposing `/mcp` beyond loopback exposes those too unless something in front of
`imbhd` restricts them.

## Reading data another process writes

An MCP client and a writer can share one database directory: `imbhd` holds the write lock, and any
number of read-only opens (`Db::open_read_only`) can query alongside it, seeing the writer's sealed
segments plus its live WAL tail (OVERVIEW.md §3). Only one process may write.

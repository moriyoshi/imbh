# imbh as an MCP server

imbh serves the **Model Context Protocol** over both of MCP's transports. An agent connected to
either one can search logs, pull traces, and query metrics through the same process that holds
them: no Grafana, no datasource proxy, no export step, and no second copy of the data.

```
OTLP in ──▶  imbhd  ──▶  imbh Db  ──▶  tools  ──▶  agent
```

| Transport | Served by | When |
|---|---|---|
| **stdio** | `imbh-tui --mcp-stdio` | The agent runs on the same machine and can spawn a process. No port, no configuration beyond a path. |
| **Streamable HTTP** | `imbhd` at `POST /mcp` | The agent is elsewhere, or the answers must include what the writer has not sealed yet. |

The protocol and the tools are one crate (`imbh-mcp`) behind both, so the two transports cannot
answer differently. Neither adds **any crate** to the dependency graph: JSON-RPC goes through
`serde_json` and Base64 through `base64`, both already compiled under DataFusion (via `arrow-json`
and `arrow-cast`), and the stdio transport's HTTP forwarding mode is hand-written over
`std::net::TcpStream`.

## Quick start: stdio

Point the client at the database directory. Nothing needs to be running — `imbh-tui` opens it
**read-only**, which works whether or not an `imbhd` is writing the same directory:

```sh
imbh-tui --mcp-stdio ./imbh-data
```

For Claude Code:

```sh
claude mcp add imbh -- imbh-tui --mcp-stdio /var/lib/imbh
```

For a client configured by file:

```json
{
  "mcpServers": {
    "imbh": {
      "command": "imbh-tui",
      "args": ["--mcp-stdio", "/var/lib/imbh"]
    }
  }
}
```

A read-only opener sees every segment the writer has sealed plus its live WAL tail, but not what is
still only in the writer's in-memory buffer. When the last few seconds have to be visible, forward
to the running daemon instead of opening the files — same stdio session, same tools:

```sh
imbh-tui --mcp-stdio --url 127.0.0.1:4318
```

The session speaks newline-delimited JSON-RPC on stdin/stdout, so it is scriptable without a client:

```sh
echo '{"jsonrpc":"2.0","id":1,"method":"tools/list"}' | imbh-tui --mcp-stdio ./imbh-data
```

## Quick start: HTTP

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

All but two are **read-only**: nothing else here can ingest, flush, compact, or apply retention. The
exceptions are `set_promoted_attributes` and `run_housekeeping`, and each is offered only where it can
work — see below. Time windows default to the last hour; pass `since` (`"15m"`, `"2h"`, `"7d"`)
or explicit `start_unix_nano` / `end_unix_nano` to change that. Every timestamp in and out is epoch
nanoseconds.

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
| `attribute_stats` | Per attribute key: distinct values, how selective it is within a segment, what a promoted column would cost, and whether to promote or index it. DB-wide and per table. |
| `list_promoted_attributes` · `set_promoted_attributes` | Read and replace the attribute keys stored as columns. |
| `run_housekeeping` · `housekeeping_status` | Queue a seal/commit/retention (and optionally compaction) pass, and poll the job id it returns. |

### The two tools that write

`set_promoted_attributes` replaces the promoted attribute keys: it seals the buffer and changes the
schema every segment written afterwards carries. Segments already on disk keep theirs and stay
queryable, so the change is safe on a live database — but it *is* a change, and it is the only one on
this surface.

It appears in `tools/list` **only when the server opened the database read-write**. A read-only
server — `imbh-tui --mcp-stdio <dir>` against someone else's database — holds no writer lock, so the
tool has nothing to call and is not offered; point the client at the process that writes the database
instead. A deployment serving `/mcp` from `imbhd` is granting an agent that action, and should gate
the endpoint the way it gates `/admin/*`.

`run_housekeeping` is the other, and it answers a **job id** rather than an outcome: a pass costs the
size of the database, not the size of an answer, so it is queued and `housekeeping_status` reports it.
It needs a writable server *and* one that runs a queue — `imbhd` does, `imbh-tui --mcp-stdio` does
not, and a host without one does not advertise the tools. Submitting the same request while one is
still queued returns the waiting job's id rather than piling up passes.

Send the whole set, not a delta: the order is the column order. `attribute_stats` is what to choose it
from — it rates each key's cost and gives a promote verdict — and demotion (sending the set without a
key) is always safe, since a promoted key never leaves the JSON attribute blob.

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
  `resultType: "complete"`. Over HTTP the `MCP-Protocol-Version`, `Mcp-Method`, and `Mcp-Name`
  headers are validated against the body, and a mismatch is refused with `-32020`. Those headers
  are the *HTTP transport's* rule, so a stdio session — where there is no header channel — is
  served on its `_meta` alone. (`--url` mode synthesizes them from the message it forwards.)
- **`2025-11-25`, `2025-06-18`, `2025-03-26`** — the handshake era: `initialize` →
  `notifications/initialized` → `tools/list` / `tools/call`.

The era is chosen per request: a `params._meta` protocol version (or a `server/discover` call)
selects the stateless path, anything else the handshake path. A client asking `initialize` for an
unknown revision is answered with the newest handshake-era one.

Responses are always a single JSON document — nothing here streams, so no SSE stream is opened and
no session id is minted. Over HTTP, `GET /mcp` and `DELETE /mcp` (the older revisions' stream and
session-teardown verbs) answer `405`. Over stdio, one line in is at most one line out: a
notification is answered with nothing, a blank line is skipped, and a malformed line gets a parse
error without ending the session.

## Exposure

Over stdio the question barely arises: the pipe is the authorization, since only the process that
spawned the server can write to it, and the server binds no port. The reach of a session is exactly
the database directory (or the `--url` daemon) it was started with, and every tool is read-only.

Over HTTP, like the rest of `imbhd`, the endpoint is **unauthenticated** — a real deployment gates
it, and the default `127.0.0.1` bind is the intended posture for a local agent.

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

That is what `imbh-tui --mcp-stdio <dir>` does, and it is why a stdio session needs nothing running.
What it cannot see is the writer's unsealed in-memory buffer — so if an agent must be able to ask
about the last few seconds, either shorten `IMBH_FLUSH` on the daemon or point the session at it
with `--mcp-stdio --url 127.0.0.1:4318`, which forwards every message to the writer itself.

## Serving MCP from your own host

Neither binary is the product. `imbh-mcp` is a library: `handle(&db, message, &transport)` takes one
JSON-RPC message and returns the reply, so a host that already owns a transport (a Unix socket, a
WebSocket, an existing HTTP server) can serve the same tools without adopting `imbhd` or `imbh-tui`.
`imbh_mcp::stdio::serve` is itself only a `read_until`/`write_all` loop over that call.

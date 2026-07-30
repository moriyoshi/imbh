# imbh as a Docker logging driver

`imbhd` — the reference server ([ARCHITECTURE.md §10.16](../.agents/docs/ARCHITECTURE.md)) — can run
as a **Docker logging-driver plugin**. Containers started with `--log-driver imbh` have their
stdout/stderr written straight into an embedded imbh database, where the lines are queryable with
SQL, full-text `matches()`, and the typed logs API — and `docker logs` keeps working, served back out
of that same database.

This is one worked example of host wiring, like the rest of `imbh-server`. The interesting part is
that a log driver is a *natural* fit for an embeddable observability database: no collector, no
sidecar, no network hop between the container and storage.

```
container stdout/stderr
        │
   dockerd  ──FIFO──▶  imbhd (plugin socket)  ──OTLP──▶  imbh Db  ──▶  SQL / matches() / docker logs
```

> **Not the same thing as `ghcr.io/moriyoshi/imbh`.** That published image runs `imbhd` as an ordinary
> container you `docker run` and point OTLP exporters at (see README.md "Install the binaries"). A
> logging-driver plugin is a different Docker artifact with a different lifecycle: it is installed with
> `docker plugin install`, the daemon starts it, and it is addressed by `--log-driver` rather than by a
> port. The two can be used together — and the plugin is still built locally, per the quick start
> below; it is not published yet.

The feature is **off by default**. Build it with:

```sh
cargo build --release -p imbh-server --features docker
```

The prebuilt Linux binaries from a GitHub release already include it (`--features docker,grpc,tracing`),
so you can register the plugin without a Rust toolchain if you supply the rootfs yourself; the macOS
builds omit it, since a plugin socket must be reachable by a *local* daemon and on macOS the daemon
runs inside a VM.

It adds no crate to the dependency graph (the protobuf and OTLP message types are already there via
`imbh-otlp`) and is **Unix only** — the module is `#[cfg(unix)]`.

## Quick start

Build and register the managed plugin:

```sh
./crates/imbh-server/docker-plugin/build.sh
docker plugin set    imbh/log-driver:latest data.source=/var/lib/imbh
docker plugin enable imbh/log-driver:latest
```

`build.sh` honors a few environment variables: `PLUGIN` (the plugin name), `DOCKER` (the container
engine — `DOCKER=podman`, or `DOCKER="sudo docker"` to reach a rootful daemon), and `IMBH_BIND` (see
[Networking](#networking)).

Run something that logs:

```sh
docker run --rm --log-driver imbh/log-driver:latest --log-opt imbh-service=web nginx
```

Read it back — either the Docker way:

```sh
docker logs <container>
docker logs -f --tail 100 --since 10m <container>
```

…or the imbh way, over the query endpoint `imbhd` is already serving. `build.sh` binds it to this
daemon's bridge gateway (`172.17.0.1` on a default install — see [Networking](#networking) below):

```sh
curl -s 172.17.0.1:4318/api/query --data \
  "SELECT time, service, severity_text, body FROM logs ORDER BY time DESC LIMIT 20"

# full-text search across every container
curl -s 172.17.0.1:4318/api/query --data \
  "SELECT service, body FROM logs WHERE matches(body, 'timeout upstream')"

# error rate per container, last hour, 5-minute buckets
curl -s 172.17.0.1:4318/api/query --data "
  SELECT date_bin(INTERVAL '5 minutes', time) AS bucket,
         json_get_str(resource, 'container.name') AS container,
         count(*) AS errors
  FROM logs
  WHERE severity_number >= 17 AND time > now() - INTERVAL '1 hour'
  GROUP BY 1, 2 ORDER BY 1"
```

## What a line becomes

Each line is an OTLP log record. Container identity is on the **resource** (OpenTelemetry semantic
conventions), so it survives compaction and is queryable with `json_get_str(resource, …)`:

| Field | Value |
|-------|-------|
| `time` | the container's capture time, nanosecond precision |
| `body` | the line, with one trailing newline removed |
| `severity_number` / `severity_text` | `9`/`INFO` for stdout, `17`/`ERROR` for stderr (configurable) |
| `service` | the `imbh-service` log-opt, else the container name, else its short id |
| `scope` | `docker` — distinguishes driver output from an app's own OTLP |
| `attributes` | `log.iostream` = `stdout` \| `stderr` |
| `resource` | `service.name`, `container.id`, `container.name`, `container.image.name`, `container.image.id`, `container.runtime` = `docker`, plus selected labels/env |

Lines Docker splits (anything over ~16 KiB) are reassembled into one record before storage, so a
long line is one row, not five.

## Log options

Pass with `--log-opt key=value` on `docker run`, or set daemon-wide in `/etc/docker/daemon.json`.

| Option | Effect |
|--------|--------|
| `imbh-service=NAME` | sets `service.name`. Default: the container name, else its short id |
| `labels=a,b` | copy those container labels to resource attributes `container.label.a`, … |
| `env=A,B` | copy those environment variables to `container.env.A`, … |
| `imbh-stdout-severity=LEVEL` | severity for stdout lines. Default `INFO` |
| `imbh-stderr-severity=LEVEL` | severity for stderr lines. Default `ERROR` |

`LEVEL` is an OTel severity name (`TRACE`, `DEBUG`, `INFO`, `WARN`, `ERROR`, `FATAL`) or a raw
1–24 severity number. An unrecognized value falls back to the default rather than failing the
container's start.

Not supported: `labels-regex` / `env-regex` (a regex engine is not worth the footprint here — list
the keys), and `tag` (Docker's `{{.Name}}` template language; use `imbh-service` and the resource
attributes instead).

Labels and environment variables are copied **only** when named — a container's whole environment is
never swept into the database, because it usually contains secrets.

## Networking

The plugin runs in the **host** network namespace, and `imbhd` binds the **docker0 bridge gateway**
(`172.17.0.1:4318` HTTP, `:4317` gRPC on a default daemon). That address is deliberate: containers
on every bridge network can reach it, and the LAN cannot.

`build.sh` does not trust the `172.17.0.1` default -- it asks the daemon what its bridge gateway
actually is and applies it:

```sh
docker network inspect bridge --format '{{range .IPAM.Config}}{{.Gateway}}{{end}}'
```

Because both addresses arrive as **environment variables**, they are re-tunable at any time without
rebuilding the plugin (a plugin's entrypoint arguments are frozen in its `config.json`; `env` entries
declared `settable` are not):

```sh
docker plugin disable imbh/log-driver:latest
docker plugin set     imbh/log-driver:latest IMBH_LISTEN_ADDR=172.17.0.1:4318
docker plugin enable  imbh/log-driver:latest
```

The same mechanism tunes the **flush scheduler**, which decides when buffered lines become Parquet
segments (and when the WAL can be reclaimed). `IMBH_FLUSH` defaults to `interval=5s`; its triggers OR
together, so a plugin capturing bursty containers can add a size or idle trigger, and
`IMBH_MAINTENANCE_INTERVAL` (default `60s`) sets the retention cadence:

```sh
docker plugin set imbh/log-driver:latest IMBH_FLUSH=interval=10s,buffer=32MiB,idle=2s
```

### Sending traces and metrics from an app container

Point a stock OTel SDK at the same address. `--add-host=host.docker.internal:host-gateway` resolves
to the daemon's bridge gateway on **any** network -- including the ones `docker compose` creates --
so one endpoint value works everywhere:

```sh
docker run --add-host=host.docker.internal:host-gateway \
  -e OTEL_EXPORTER_OTLP_ENDPOINT=http://host.docker.internal:4318 \
  -e OTEL_SERVICE_NAME=web \
  --log-driver imbh/log-driver:latest --log-opt imbh-service=web \
  myapp
```

Give `OTEL_SERVICE_NAME` the same value as `--log-opt imbh-service=` and the container's logs,
traces, and metrics all land under one `service`, joinable in a single SQL query. That is the point
of running the driver rather than shipping logs somewhere separate.

The plugin build carries both OTLP transports, so the SDK default (gRPC on 4317) works as-is; for
OTLP/HTTP set `OTEL_EXPORTER_OTLP_PROTOCOL=http/protobuf`. One caveat either way: `imbhd` does not
decompress request bodies. SDKs send uncompressed by default, but the **OpenTelemetry Collector's
OTLP exporters default to `compression: gzip`** -- if you front the plugin with a collector, set
`compression: none` explicitly or every export fails.

### Opening no network port at all

If the driver is only meant to capture logs, give it no TCP listeners:

```sh
IMBH_BIND=none ./crates/imbh-server/docker-plugin/build.sh
# or, on an existing plugin:
docker plugin set imbh/log-driver:latest IMBH_LISTEN_ADDR= IMBH_GRPC_LISTEN_ADDR=
```

Container logging is unaffected -- the plugin protocol and the per-container FIFOs are filesystem
objects, not network ones. Query the database by opening its directory **read-only** from the host
(`imbh-tui`, or a second `imbhd` using `Db::open_read_only`), which needs no network either. What you
give up is OTLP ingest: app containers have no way to reach the plugin, and they cannot use a second
`imbhd` on the same directory instead, because imbh allows only one writer per database.

### Why not `network.type: bridge`?

It sounds like the right answer and it is not. The Docker daemon *accepts* `"bridge"` in a plugin's
`config.json` -- it round-trips through `docker plugin inspect` -- but moby does not implement it for
managed plugins. Measured on Docker 29.2.1 with a probe plugin that dumped its own interfaces:

| `network.type` | What the plugin process sees |
|----------------|------------------------------|
| `host` | `lo`, the host's LAN interface, `docker0`, every `br-*`, a default route |
| `bridge` | **`lo` only -- no addresses, no routes, no veth** |

`bridge` is silently equivalent to `none`: the plugin does not get an IP on docker0. It is a fine
choice if you want the strictest isolation and no OTLP ingest, but then the empty-listener
configuration above achieves the same reachability with a supported setting.

## `docker logs`

The plugin advertises the `ReadLogs` capability, so `docker logs` is answered from the database:
`--tail N`, `--since` / `--until`, and `-f` (follow) all work. Follow polls every 200 ms and ends
once the container's log stream is gone and no new lines have arrived, so `docker logs -f` on a
stopped container returns rather than hanging.

Two caveats worth knowing:

- The newline ingest stripped is restored on the way out, so output matches what the container
  printed. A line that had no trailing newline gets one.
- Follow advances by record timestamp, which is when the container *emitted* the line -- ingest
  stores it up to one batch interval later. Follow accounts for that, except under `--tail 0`, whose
  "only new lines" semantic means a line emitted a moment before the follow started may not appear.
  Two records sharing a nanosecond would also be reported once; Docker's timestamps are wall-clock
  nanoseconds, so that does not occur in practice.

## Running it yourself (without the managed plugin)

The plugin endpoint is just `imbhd` with an environment variable pointing at a socket path — nothing
about it requires the plugin packaging:

```sh
IMBH_DOCKER_PLUGIN_SOCKET=/run/docker/plugins/imbh.sock \
IMBH_LISTEN_ADDR=172.17.0.1:4318 \
  imbhd /var/lib/imbh
```

Docker discovers any socket in `/run/docker/plugins` as a legacy plugin named after the file, so
`--log-driver imbh` then works against a plain host process. That is the easiest way to try the
driver, and the easiest way to debug it. Absent the variable, the plugin endpoint stays off and
`imbhd` behaves exactly as before.

Because the plugin is a normal `imbhd`, everything else it serves is available at the same time:
OTLP/HTTP ingest on `/v1/{logs,traces,metrics}`, `/stats`, `/admin/{flush,compact}`. Application
traces and container logs land in one database.

## Operational notes

- **One writer per database.** imbh enforces a `writer.lock`, so the plugin owns its data directory.
  Other processes can still read it — `Db::open_read_only` sees the plugin's committed segments plus
  its live WAL tail. Point a second, read-only `imbhd` (or `imbh-tui`) at the same directory to query
  without contending with ingest.
- **Retention** is imbh's, not Docker's: configure age and disk budget on the `Db` rather than
  `max-size`/`max-file`. The defaults come from `imbh-core`'s `Retention`.
- **Back-pressure, not loss.** When ingest falls behind, the FIFO reader blocks rather than dropping
  lines, which propagates back into the container's stdout — slow logging instead of missing logs.
- **Batching.** Lines are ingested in batches (512 records or 200 ms, whichever comes first), so the
  cost is one WAL append per batch, not per line.
- **Plugin diagnostics** go to stderr. There is no `docker plugin logs` command -- plugin output is
  captured by the Docker daemon's own log, so read it with `journalctl -u docker` (or wherever your
  daemon logs). The provided Dockerfile builds `--features docker,grpc,tracing`, so `RUST_LOG`
  controls the verbosity.

## Protocol coverage

For reference, the Docker plugin endpoints implemented — the full `docker.logdriver/1.0` interface:

| Endpoint | Status |
|----------|--------|
| `/Plugin.Activate` | ✅ declares `LogDriver` |
| `/LogDriver.StartLogging` | ✅ opens the container's FIFO, starts a reader |
| `/LogDriver.StopLogging` | ✅ |
| `/LogDriver.Capabilities` | ✅ `ReadLogs: true` |
| `/LogDriver.ReadLogs` | ✅ history, `--tail`, `--since`/`--until`, follow |

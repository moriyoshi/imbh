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
> port. The two can be used together, and they are separate registry repositories because they have to
> be: a managed plugin's manifest points at an `application/vnd.docker.plugin.v1+json` config, so
> `docker pull` refuses a plugin and `docker plugin install` refuses an image. One repository, one
> artifact kind, one verb.

The feature is **off by default** in a source build:

```sh
cargo build --release -p imbh-server --features docker,docker-remap
```

The published plugin and the prebuilt Linux binaries from a GitHub release both carry both features
(`--features docker,docker-remap,grpc,tracing`); the macOS builds omit them, since a plugin socket
must be reachable by a *local* daemon and on macOS the daemon runs inside a VM.

`docker` is the plugin itself: **Unix only** (the module is `#[cfg(unix)]`) and it adds no crate to
the dependency graph, since the protobuf and OTLP message types are already there via `imbh-otlp`.
`docker-remap` adds [remapping](#remapping) — parsing a container's JSON, logfmt, klog/glog or
`key=value` output into queryable fields — and it *does* carry a dependency subtree (VRL, +89 crates
and +3.8 MiB). Drop it for a smaller build; lines are then stored exactly as the container printed
them.

## Install

The release publishes the plugin **per architecture**, and you name the one you want:

```sh
docker plugin install --alias imbh ghcr.io/moriyoshi/imbh-log-driver:0.8.0-amd64
```

That is the whole install. **The database directory needs no setting up** — `/var/lib/imbh` is
declared as the plugin's [`propagatedMount`](#where-the-database-lives), so the Docker daemon creates
the storage behind it and mounts it in at enable. There is no host path to create, no permission to
grant for one, and the same command works on every daemon, Docker Desktop included.

Tags are `X.Y.Z-amd64` / `X.Y.Z-arm64`, plus the floating `X.Y-<arch>` and `latest-<arch>`. There is
no architecture-agnostic tag, and that is not an oversight: managed plugins have no manifest-list
support — moby's plugin fetch path resolves a single manifest and does no platform matching, so a
multi-arch tag would have nothing to select from. Install asks to grant the one permission
`config.json` declares (host networking); `--grant-all-permissions` answers it for a script.

### Where the database lives

The plugin stores its database in **storage the daemon provisions for it**, not in a directory you
choose:

```
/var/lib/docker/plugins/<plugin-id>/propagated-mount    # on the host (or, on Docker Desktop, in the VM)
        ↳ mounted at /var/lib/imbh inside the plugin
```

This is a deliberate trade, and the cost side is real:

- ✅ **Nothing to provision.** A bind mount would need a source directory, and a missing bind source
  is the one thing the daemon will *not* create for a plugin — it fails `plugin enable` (not `set`)
  with `error mounting "/var/lib/imbh" to rootfs … no such file or directory`. Measured on Docker
  29.2.1; `propagatedMount` has no such failure mode.
- ✅ **It survives the plugin lifecycle you actually use.** `docker plugin disable` → `set` →
  `enable`, which every settings change requires, keeps the database intact (measured across
  repeated cycles).
- ⚠️ **`docker plugin rm` deletes the database.** Removing the plugin removes
  `/var/lib/docker/plugins/<id>/` whole, propagated mount included — measured, not assumed. There is
  no undo and no prompt. Treat `plugin rm` as `DROP DATABASE`, and see
  [Keeping the data](#keeping-the-data) before you run it.
- ⚠️ **You cannot point it at another disk.** Log volumes grow; if `/var/lib/docker` is on a small
  filesystem, size the [retention](#operational-notes) policy for it rather than expecting to
  relocate the database.
- ⚠️ **The path is root-owned**, so reading it from the host takes root — see below.

#### Keeping the data

The database is a normal imbh directory, so anything that can read the path can copy it. Find it and
work through a container, which needs no `sudo` of your own (the container is root, and its `/` is
the *daemon's* `/` — so this works unchanged on Docker Desktop, where the path lives in the VM):

```sh
ID=$(docker plugin inspect imbh --format '{{.Id}}')

# back it up to the current directory
docker plugin disable imbh                      # seal the buffer first; imbh is single-writer
docker run --rm -v /:/host -v "$PWD:/out" busybox \
  tar czf /out/imbh-backup.tar.gz -C "/host/var/lib/docker/plugins/$ID/propagated-mount" .
docker plugin enable imbh
```

The same shape puts files *in* — which is how you install a remap script for
`IMBH_DOCKER_REMAP=@/var/lib/imbh/remap/app.vrl` (see [Your own script](#your-own-script)):

```sh
docker run --rm -v /:/host -v "$PWD:/in" busybox sh -c \
  "mkdir -p /host/var/lib/docker/plugins/$ID/propagated-mount/remap &&
   cp /in/app.vrl /host/var/lib/docker/plugins/$ID/propagated-mount/remap/"
```

To query the database rather than copy it, prefer the OTLP/SQL endpoint the plugin already serves
(see [Using it](#using-it)) — it is the supported read path and needs none of the above.

`--alias imbh` is what makes the driver addressable as plain `--log-driver imbh` instead of by its
full registry path.

**The OTLP address needs no configuring.** It defaults to `auto`, which binds every bridge network
this daemon actually has, re-checked on a timer — so a custom `bip`, a re-created `docker0`, or a
network created later all work without being told about (see [Networking](#networking)). To see what
it resolved to:

```sh
docker network inspect bridge --format '{{range .IPAM.Config}}{{.Gateway}}{{end}}'
```

To pin one address instead, or to restrict who may connect:

```sh
docker plugin disable imbh
docker plugin set     imbh IMBH_LISTEN_ADDR=172.17.0.1:4318 IMBH_GRPC_LISTEN_ADDR=172.17.0.1:4317
docker plugin enable  imbh
```

The database is not disturbed by that cycle.

### Docker Desktop (macOS / Windows)

> **Not exercised by CI**, which runs Linux daemons only. The storage mechanism is deliberately
> platform-independent — nothing below asks the host filesystem for anything — but reports are
> welcome.

A managed plugin is a *Linux* artifact and Docker Desktop's daemon is Linux, so this is **not** the
same limitation as the macOS binaries omitting the `docker` feature (README.md "Install the
binaries"). That note is about a native `imbhd` on your Mac, which has no local daemon to serve; the
published plugin instead runs inside the VM, next to the daemon that loads it.

Because the database is daemon-provisioned, the install is the same one line as on Linux — there is
no host directory to create and **no reliance on Docker Desktop's host file sharing**, which is a
FUSE-family filesystem where imbh's advisory `flock` on `writer.lock` and its memory-mapped segments
would both be on uncertain ground. The database stays inside the VM, on the VM's own filesystem,
where those primitives behave normally.

Two Desktop-specific things still bite:

- **Pick the tag by the VM's architecture, not your machine's marketing name.** Apple silicon runs an
  arm64 VM, so it is `…-log-driver:0.8.0-arm64`. The `-amd64` tag in the recipe above is the Linux
  x86_64 default, and on an M-series Mac it is the wrong artifact.
- **`curl 172.17.0.1:4318` from your Mac or PC shell will not reach it.** The plugin binds the
  **VM's** bridge gateway, so containers reach it exactly as on Linux, but a managed plugin cannot
  publish ports to your host. Query it from a container on the same daemon.

Everything in [Keeping the data](#keeping-the-data) works unchanged here: those recipes go through a
container, whose `/` is the VM's `/`, so they reach the database without any host sharing and without
`sudo`.

### Building it yourself

Only needed if you are changing imbh:

```sh
./crates/imbh-server/docker-plugin/build.sh
docker plugin enable imbh/log-driver:latest
```

It compiles `imbhd`, packages it into a rootfs, registers the plugin on the local daemon, and points
the OTLP listeners at the bridge gateway it read from that daemon. Environment variables: `PLUGIN`
(the plugin name), `DOCKER` (the container engine — `DOCKER=podman`, or `DOCKER="sudo docker"` to
reach a rootful daemon), `FEATURES` (the `imbhd` feature set), `BASE` (the rootfs base image; raise it
when your host's glibc is newer than `debian:bookworm-slim`'s — the script's smoke test tells you),
and `IMBH_BIND` (see [Networking](#networking)).

## Using it

Run something that logs:

```sh
docker run --rm --log-driver imbh --log-opt imbh-service=web nginx
```

Read it back — either the Docker way:

```sh
docker logs <container>
docker logs -f --tail 100 --since 10m <container>
```

…or the imbh way, over the query endpoint `imbhd` is already serving — on every bridge gateway this
daemon has, which is `172.17.0.1` on a default install (see [Networking](#networking) below):

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
| `time` | the line's own timestamp when [remapping](#remapping) found one, else the container's capture time |
| `observed_time` | the container's capture time, nanosecond precision |
| `body` | the parsed fields when [remapping](#remapping) recognised the line, else the line with one trailing newline removed |
| `severity_number` / `severity_text` | the line's own level when remapping found one, else `9`/`INFO` for stdout and `17`/`ERROR` for stderr (configurable) |
| `service` | the `imbh-service` log-opt, else the container name, else its short id |
| `scope` | `docker` — distinguishes driver output from an app's own OTLP |
| `attributes` | `log.iostream` = `stdout` \| `stderr` |
| `resource` | `service.name`, `container.id`, `container.name`, `container.image.name`, `container.image.id`, `container.runtime` = `docker`, plus selected labels/env and, when available, the container's networks |

When bridge-network discovery can name the networks a container is on — which needs the Engine API,
see [Networking](#networking) — the resource also carries `container.network.names` (an array) and
`container.network.<name>.ip` per attached network. A container that starts between two discovery
refreshes has no network attributes on its first lines and gains them on the rest: the driver never
asks the daemon about a container from inside the handler that is starting it, because `dockerd`
calls that handler while holding the container's lock and a call back can deadlock the daemon against
its own log driver. `IMBH_DOCKER_NETWORK_ATTRS=off` turns the attributes off entirely.

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
| `imbh-remap=SPEC` | the [remap script](#remapping) for this container. Default: the built-in one |

`LEVEL` is an OTel severity name (`TRACE`, `DEBUG`, `INFO`, `WARN`, `ERROR`, `FATAL`) or a raw
1–24 severity number. An unrecognized value falls back to the default rather than failing the
container's start.

Not supported: `labels-regex` / `env-regex` (a regex engine is not worth the footprint here — list
the keys), and `tag` (Docker's `{{.Name}}` template language; use `imbh-service` and the resource
attributes instead).

Labels and environment variables are copied **only** when named — a container's whole environment is
never swept into the database, because it usually contains secrets.

## Remapping

Built with `docker-remap` (as the published plugin is), every line runs through a
[VRL](https://vector.dev/docs/reference/vrl/) program that maps the Docker log-driver data model onto
the [OpenTelemetry logs data model](https://opentelemetry.io/docs/specs/otel/logs/data-model/). The
built-in script recognises **JSON**, **logfmt**, **klog/glog** and **`key=value`** with no
configuration:

```console
$ docker run --rm --log-driver imbh --log-opt imbh-service=demo alpine sh -c '
    echo "{\"level\":\"warn\",\"msg\":\"disk low\",\"disk\":\"/dev/sda\"}"
    echo "level=info msg=\"ready\" port=8080"
    echo "I0413 12:34:56.789012  123 main.go:45] leader elected"
    echo "starting server on port 8080"'
```

```sql
SELECT severity_text, json_get_str(body, 'msg'), json_get_str(body, 'disk')
FROM logs WHERE service = 'demo' ORDER BY observed_time;
-- WARN   disk low                       /dev/sda
-- INFO   ready
-- INFO   leader elected
-- INFO   starting server on port 8080
```

A line the script cannot confidently classify is left alone: prose such as `starting server on port
8080` becomes `{"msg": "starting server on port 8080"}`, never a pile of bogus fields. Every
`key=value` tier is anchored on the line *starting* with `key=`, so `usage: foo --opt=bar` stays
prose too.

What the built-in script does, beyond parsing:

- **One message key.** `message`, `log` and `event` are renamed to `msg` (klog and zerolog say
  `message`; zap, logrus, slog and logfmt say `msg`), so `body->>'msg'` always works.
- **Severity** comes from `level` / `severity` / `lvl` / `loglevel` / `levelname` / `log.level`, by
  name or by number — bunyan/pino `10..60` and syslog `0..7` are both understood. `severity_text` is
  normalised to the OTel band, so it stays a closed set across containers. An unrecognised level is
  left in the body and the stream default applies.
- **Timestamps** from `timestamp` / `ts` / `time` / `@timestamp` / `eventTime` / `asctime` become the
  event `time`, while Docker's capture time stays as `observed_time`. RFC 3339, four common layouts,
  and epoch seconds/millis/micros/nanos are all recognised — but **only within ±26h of the capture
  time**. `docker logs` pages and follows on `time`, so a container with a skewed clock must not be
  able to make `docker logs -f` skip lines.
- **`trace_id` / `span_id`** (also `traceId`, `traceID`, `otelTraceID`, …) are lifted onto the
  record, so a line joins the traces in the same database.

Container identity is **not** the script's business: `service.name`, `container.*`,
`container.label.*`, `container.env.*` and `log.iostream` are set by the driver before the script
runs, and left alone by the built-in one.

### `docker logs` on a remapped container

Because the body is now structured, `docker logs` renders it back as a single logfmt line rather than
the original being stored a second time:

```console
$ docker logs demo
ts=2026-08-06T12:34:56Z level=WARN msg="disk low" disk=/dev/sda
ts=2026-08-06T12:34:56Z level=INFO msg=ready port=8080
```

With `docker-remap` off — or `imbh-remap=off` — bodies are plain strings and `docker logs` is
byte-identical to what the container printed, as before.

### Your own script

`imbh-remap` (per container) and `IMBH_DOCKER_REMAP` (daemon-wide) share one grammar:

| Value | Meaning |
|-------|---------|
| unset, or `default` | the built-in script |
| `off` (or `none`) | no remapping — store lines exactly as printed |
| `@PATH` | read the script from `PATH` **inside the plugin**, e.g. `@/var/lib/imbh/remap/app.vrl` |
| anything else | an inline VRL script |

A per-container `--log-opt` always wins over the daemon-wide default.

```sh
# drop health-check noise before it is ever stored
docker run --log-driver imbh \
  --log-opt imbh-remap='if contains!(.line, "/healthz") { abort }' nginx

# daemon-wide, from a file in the plugin's database directory (no rebuild needed)
docker plugin set imbh IMBH_DOCKER_REMAP=@/var/lib/imbh/remap/app.vrl
```

`@PATH` resolves inside the plugin's own mount namespace, not the host's. `/var/lib/imbh` is the
daemon-provisioned database directory and persists across `disable`/`enable`, so a script there is
the path that works with no extra configuration — see
[Keeping the data](#keeping-the-data) for the one-liner that copies a file into it.

The script receives the Docker log-driver model **and** the OTel record the driver would have stored
on its own, so a script that changes nothing behaves exactly like no script at all:

```coffee
# ── in: the Docker log-driver model ──
.line                       # the line, one trailing newline removed
.source                     # "stdout" | "stderr"
.time_nano                  # Docker's capture time, unix nanoseconds
.partial                    # was this line reassembled from split chunks
.info.container_id          # ...container_name (no leading slash), container_image_name,
.info.container_labels      #    container_image_id, daemon_name, log_path
.info.container_env         # the full map, "K=V" pre-split
.info.config                # the --log-opt map
.info.networks              # { network name: address }, from bridge discovery -- empty until it
                            #   knows (see "Networking"), and it can fill in mid-container

# ── in AND out: the OTel logs model, pre-filled with what the driver would store ──
.timestamp                  # both seeded from .time_nano; move .timestamp, leave the other alone
.observed_timestamp         #   -- `docker logs -f` follows this one (see "the two clocks" below)
.severity_number            # the stdout/stderr default for this stream
.severity_text
.body                       # seeded with .line; any VRL value is accepted
.attributes                 # { "log.iostream": ... }
.resource                   # service.name, container.*, container.label.*, container.env.*
.trace_id / .span_id        # 32 / 16 hex characters
.trace_flags
```

`abort` drops the line. A script that fails at runtime never costs the line — it is stored the
un-remapped way and a rate-limited warning goes to the daemon log. A script that fails to *compile*
fails `docker run` with the VRL diagnostic, so you see the error where you typed the option.

Three things a script cannot break, because `docker logs` depends on them: `container.id` on the
resource is always restored (and cannot be changed to a different container's), `service.name` is
restored if blanked, and `log.iostream` is restored if removed.

The `env`, `system`, network and crypto VRL function groups are **not** compiled in — a remap script
cannot read the host or make network calls. That is what makes it safe for `.info` to expose the
container's full label and environment maps, which the `labels=`/`env=` log-opts deliberately do not.

## Networking

The plugin runs in the **host** network namespace and **discovers the daemon's bridge networks at
run time**, binding one listener per bridge gateway. That is what `IMBH_LISTEN_ADDR=auto` -- the
shipped default -- means. Those addresses are reachable from containers on every bridge network and
have no route from the LAN, so an app container can reach the endpoint and the rest of your network
cannot.

Discovery re-runs every `IMBH_DOCKER_NETWORK_REFRESH` (default `30s`), so a network created by a
`docker compose up` after the plugin started gets a listener within that window, and one destroyed
by `docker compose down` loses its listener the same way. Set it to `0` to discover once at startup
and never look again.

> Earlier versions bound a single address baked in at package time: `build.sh` ran `docker network
> inspect bridge` once and applied the answer with `docker plugin set`, over a hard-coded
> `172.17.0.1` default. Anyone installing straight from the registry -- which is what the documented
> install does -- got that literal default, so a daemon with a custom `bip`, or one whose `docker0`
> had been re-created, ended up with a listener bound to an address it does not have. The failure was
> silent: container logging is filesystem-only and kept working, so the only symptom was a query
> endpoint that never answered.

### How it discovers them

Two backends, tried in that order on every refresh:

| Backend | How | What it sees |
|---------|-----|--------------|
| Engine API | `GET /networks` over the daemon's Unix socket | network names, IPAM gateways and subnets, **and** which containers are on each |
| Interface scan | `getifaddrs` in the host netns | gateways and subnets |

The scan works because the plugin is in the host network namespace, so `docker0` and every `br-*` are
directly visible; Docker programs a bridge interface's address *from* its IPAM gateway, so on a stock
daemon the scan gives the same gateways and subnets the API would. What it cannot do is name the
Docker network, say which containers are attached, or recognise a bridge renamed with
`com.docker.network.bridge.name`.

The Engine API needs the daemon's socket inside the plugin's mount namespace, and **the shipped
plugin does not mount it**: a managed plugin's bind mount source must already exist on the daemon
host, and under rootless Docker the socket is not at `/var/run/docker.sock`, so declaring it would
make `docker plugin enable` fail on those hosts. The managed plugin therefore runs in scan mode --
which covers binding and the allow-list completely -- and forgoes `container.network.*` attributes. A
standalone `imbhd` running next to a daemon gets API mode for free.

`IMBH_DOCKER_API` names the socket: a path, `off`, or `auto` (the default). `auto` looks where the
Docker CLI looks, in the CLI's own order -- `DOCKER_HOST`, then the active context (`DOCKER_CONTEXT`,
else `currentContext` in `$DOCKER_CONFIG`/`~/.docker/config.json`), then `/var/run/docker.sock` and
`/run/docker.sock`. Reading the context store is what makes **rootless** Docker work: its setup tool
offers `export DOCKER_HOST=...` or `docker context use rootless`, and both are honoured. A
`tcp://` or `ssh://` endpoint is deliberately ignored -- this binds gateways that exist on *this*
host, so a remote daemon's networks would be the wrong answer, not a missing one.

### Pinning an address instead

Both listen addresses accept a comma-separated list, and each element is either `auto[:PORT]` or a
literal `HOST:PORT`:

```sh
docker plugin disable imbh
docker plugin set     imbh IMBH_LISTEN_ADDR=172.17.0.1:4318   # one fixed address
docker plugin set     imbh IMBH_LISTEN_ADDR=auto:9000         # every bridge gateway, port 9000
docker plugin set     imbh IMBH_LISTEN_ADDR=auto,127.0.0.1:4318
docker plugin enable  imbh
```

A **literal** address that will not bind is fatal, as it always has been: a port already in use or an
address this host does not have is a configuration error, and starting anyway would leave you
believing in an endpoint nothing serves. A **discovered** address that will not bind is only a
warning -- a bridge that vanished between the scan and the bind is ordinary, and the next refresh
picks it up.

Because the addresses arrive as **environment variables**, they are re-tunable at any time without
rebuilding the plugin (a plugin's entrypoint arguments are frozen in its `config.json`; `env` entries
declared `settable` are not). `IMBH_BIND=<addr>` at build time still pins one address, and
`IMBH_BIND=none` still opens no port.

### Restricting who may connect

`IMBH_ALLOW_FROM` filters peers on accept. It defaults to `any`, which filters nothing.

```sh
docker plugin set imbh IMBH_ALLOW_FROM=docker            # the daemon's bridge subnets + loopback
docker plugin set imbh IMBH_ALLOW_FROM=docker,10.0.0.0/8
docker plugin set imbh IMBH_ALLOW_FROM=172.23.0.0/16     # one compose project's network
```

`docker` expands to the discovered bridge subnets plus loopback, and re-expands as networks come and
go. A refused peer's connection is closed before a byte is read, and nothing is sent back that would
confirm something is listening; refusals are reported at most once a minute with a count.

This is worth setting. Binding a bridge gateway keeps the endpoint off the LAN, but it does **not**
keep it away from other containers on the same box, and `/admin/*` is unauthenticated. Naming one
project's subnet is the narrowest useful setting. Note that an `IMBH_ALLOW_FROM` that resolves to
nothing -- `docker` on a daemon with no bridge networks -- refuses everything except loopback rather
than falling open.

### Other knobs on this listener

The same mechanism tunes the **flush scheduler**, which decides when buffered lines become Parquet
segments (and when the WAL can be reclaimed). `IMBH_FLUSH` defaults to `interval=5s`; its triggers OR
together, so a plugin capturing bursty containers can add a size or idle trigger, and
`IMBH_MAINTENANCE_INTERVAL` (default `60s`) sets the retention cadence:

```sh
docker plugin set imbh IMBH_FLUSH=interval=10s,buffer=32MiB,idle=2s
```

`IMBH_HEADER_TIMEOUT` (default `10s`) and `IMBH_BODY_TIMEOUT` (default `30s`) bound how long one client
of the plugin's *HTTP* listener can hold a connection without making progress — the head in total, a
body read per read — and `IMBH_MAX_BODY` (default `64MiB`) bounds how large a request may be. The
plugin's own Unix socket applies the same defaults and is not tunable: its peer is the local
`dockerd`, which is prompt or gone. A `docker logs -f` whose client stops reading is abandoned after
30s of backpressure rather than held open indefinitely.

Disabling the plugin sends it `SIGTERM`, which it treats as a **graceful stop**: it stops accepting,
stops reading container FIFOs, ingests the lines it has already read, seals the buffer, and exits 0.
`IMBH_SHUTDOWN_TIMEOUT` (default `5s`) bounds how long in-flight requests hold that up; keep it under
the daemon's own patience, and set it to `0` to stop without waiting for anything in flight.

### Sending traces and metrics from an app container

Point a stock OTel SDK at the same address. `--add-host=host.docker.internal:host-gateway` resolves
to the daemon's bridge gateway on **any** network -- including the ones `docker compose` creates --
so one endpoint value works everywhere:

```sh
docker run --add-host=host.docker.internal:host-gateway \
  -e OTEL_EXPORTER_OTLP_ENDPOINT=http://host.docker.internal:4318 \
  -e OTEL_SERVICE_NAME=web \
  --log-driver imbh --log-opt imbh-service=web \
  myapp
```

Give `OTEL_SERVICE_NAME` the same value as `--log-opt imbh-service=` and the container's logs,
traces, and metrics all land under one `service`, joinable in a single SQL query. That is the point
of running the driver rather than shipping logs somewhere separate.

The plugin build carries both OTLP transports, so the SDK default (gRPC on 4317) works as-is; for
OTLP/HTTP set `OTEL_EXPORTER_OTLP_PROTOCOL=http/protobuf`. `Content-Encoding: gzip` on the HTTP
endpoint is handled, so a collector fronting the plugin with the **OTLP exporter's default
`compression: gzip`** works without being reconfigured.

### Opening no network port at all

If the driver is only meant to capture logs, give it no TCP listeners:

```sh
IMBH_BIND=none ./crates/imbh-server/docker-plugin/build.sh
# or, on an existing plugin:
docker plugin set imbh IMBH_LISTEN_ADDR= IMBH_GRPC_LISTEN_ADDR=
```

Container logging is unaffected -- the plugin protocol and the per-container FIFOs are filesystem
objects, not network ones. What you give up is *every* query path: with no listener there is no SQL
endpoint, and the database sits in the plugin's own storage under `/var/lib/docker/plugins/<id>/`
(root-owned, and inside the VM on Docker Desktop), so a read-only `imbh-tui` or
`Db::open_read_only` cannot simply be pointed at it the way it can at an ordinary `imbhd` directory.
Copy it out first -- see [Keeping the data](#keeping-the-data) -- and open the copy. You also give up
OTLP ingest: app containers have no way to reach the plugin, and they cannot run a second `imbhd` on
the same directory instead, because imbh allows only one writer per database.

Choose this when the plugin is a pure capture sink you will query offline. If you want a live query
endpoint *and* no LAN exposure, the discovered bridge gateways already give you that; add
`IMBH_ALLOW_FROM` to narrow which containers may use it.

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

The newline ingest stripped is restored on the way out. Without remapping, output therefore matches
what the container printed exactly; with it, a structured body is re-rendered as one logfmt line
(see [Remapping](#docker-logs-on-a-remapped-container)). Either way a line that had no trailing
newline gets one.

### Follow mode and the two clocks

Every stored record carries two instants, and `docker logs` uses each for a different job:

| Clock | Column | What it is |
|-------|--------|------------|
| event time | `time` | when the container **emitted** the line (or the line's own timestamp, if [remapping](#remapping) lifted one) |
| arrival time | `observed_time` | when the driver **captured** it off the container's stream |

- **What you see is ordered by event time.** History, `--tail N`, and `--since`/`--until` all work on
  `time`, because that is what `docker logs` means and what a json-file driver would print.
- **The follow cursor rides arrival time.** It has to. Ingest batches, so a line emitted just before
  you ran `docker logs -f` can be *stored* just after — an event-time cursor would advance past it
  and the line would never appear. `observed_time` only ever moves forward as rows become visible, so
  a cursor over it cannot step over a late-stored line. `--tail 0` takes its starting cursor the same
  way: one lookup of the newest arrival the container has already recorded, not the wall clock.

Three things this does **not** fix. They are narrow, but they are real:

- **A script can move the arrival clock too.** [Remapping](#remapping) reads `.observed_timestamp`
  from the VRL root, so a script that assigns it replaces the driver's capture time with whatever it
  computed. Set it to a constant, or to a value parsed out of the line, and the follow cursor is no
  longer monotonic in arrival order — with the same consequence as before: `docker logs -f` may skip
  lines. If you want to move a timestamp, move `.timestamp` (the event clock) and leave
  `.observed_timestamp` alone; that is what the built-in script does.
- **Exact ties are still resolved once.** The cursor is a strict `observed_time > last`, so two lines
  stamped in the very same nanosecond, split across two polls, cost the second one. Docker's capture
  times are wall-clock nanoseconds, so this does not arise in practice — but it is the same tie
  hazard the old event-time cursor had, not something the arrival clock removes.
- **`--tail 0` has no uniquely correct answer.** The json-file driver defines it as "seek to the end
  of the file", i.e. by what is already *durably recorded*; imbh batches, so at any instant some
  already-emitted lines are not yet recorded. Cutting at the newest recorded arrival makes the
  boundary **stable and explainable** — everything the database had at the moment you asked is
  history, everything captured after it is new — but it is a choice, not a proof. A line captured in
  the same batch window as the cut still lands on whichever side of it the batch flush decides. If
  you need "nothing before this instant, guaranteed", use `--since` with an explicit timestamp: that
  filters on the event clock and does not depend on when ingest ran.

## Running it yourself (without the managed plugin)

The plugin endpoint is just `imbhd` with an environment variable pointing at a socket path — nothing
about it requires the plugin packaging:

```sh
IMBH_DOCKER_PLUGIN_SOCKET=/run/docker/plugins/imbh.sock \
IMBH_LISTEN_ADDR=auto \
  imbhd /var/lib/imbh
```

Docker discovers any socket in `/run/docker/plugins` as a legacy plugin named after the file, so
`--log-driver imbh` then works against a plain host process. That is the easiest way to try the
driver, and the easiest way to debug it. Absent the variable, the plugin endpoint stays off and
`imbhd` behaves exactly as before.

Run this way, `imbhd` also reaches the Docker socket, so bridge discovery uses the **Engine API**
rather than the interface scan — which is what makes `container.network.*` attributes available. It
is the easiest way to see that half of the feature working.

Because the plugin is a normal `imbhd`, everything else it serves is available at the same time:
OTLP/HTTP ingest on `/v1/{logs,traces,metrics}`, `/stats`, `/admin/{flush,compact}`. Application
traces and container logs land in one database.

## Operational notes

- **`docker plugin rm` destroys the database.** It removes `/var/lib/docker/plugins/<id>/` entire,
  and the plugin's storage lives inside it. No prompt, no undo — back up first if the logs matter
  (see [Keeping the data](#keeping-the-data)). `disable`/`enable`, including the cycle a
  `docker plugin set` requires, is safe.
- **One writer per database.** imbh enforces a `writer.lock`, so the plugin owns its data directory.
  `Db::open_read_only` can still read a copy — it sees committed segments plus a live WAL tail — but
  the plugin's own directory is not at a path a second process can conveniently open
  (see [Where the database lives](#where-the-database-lives)), so the SQL endpoint is the intended
  read path.
- **Retention is the only size control, and it matters more here.** It is imbh's, not Docker's:
  configure age and disk budget on the `Db` rather than `max-size`/`max-file`, with the defaults from
  `imbh-core`'s `Retention`. Since the database cannot be relocated to another disk, set the budget
  against whatever filesystem holds `/var/lib/docker`.
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

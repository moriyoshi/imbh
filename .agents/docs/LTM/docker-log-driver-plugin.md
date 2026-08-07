# The Docker log-driver plugin

The `docker` feature of `imbh-server` turns `imbhd` into a `docker.logdriver/1.0` managed plugin:
`--log-driver imbh` writes a container's stdout/stderr straight into the embedded `Db`, and
`docker logs` is served back out of it. A post-M6 addition, not part of the original plan. Canonical
design lives in ARCHITECTURE.md §10.16; the operator guide is `docs/DOCKER_LOG_DRIVER.md`. This
document holds the implementation knowledge and the measurements behind them.

## Shape

A Docker plugin is an HTTP/1.1 server on a Unix socket. Five endpoints make a log driver
(`Plugin.Activate`, `LogDriver.{StartLogging,StopLogging,Capabilities,ReadLogs}`), all implemented.
Failures are reported *in* the body (`{"Err": "..."}`) with HTTP 200 — a non-200 is only for a
request that cannot be parsed as a plugin request at all.

The endpoint runs on the same axum/hyper stack as the TCP listener, so body limits, phase deadlines
and decoding are shared. Alongside it: one reader thread per container FIFO, reassembling Docker's
split lines, all funnelling into a single batching worker that ingests through `Db::ingest_otlp_logs`
— the same entry point the HTTP/gRPC routes use, so there is exactly one ingest path. Back-pressure,
not loss: a saturated ingest queue blocks the FIFO reader rather than dropping lines.

The plugin endpoint is off unless `IMBH_DOCKER_PLUGIN_SOCKET` names a socket, so a local `imbhd`
built with the feature never touches `/run/docker`.

## Networking: measured, not assumed

Everything here was measured on Docker 29.2.1, and each finding closed off an approach that looked
obviously right.

**`network.type: bridge` is accepted but unimplemented for managed plugins.** It round-trips through
`docker plugin inspect`, so it looks supported; moby just drops the plugin into an empty netns.

| `network.type` | what the plugin process sees |
|---|---|
| `host` | `lo`, the host LAN interface, `docker0`, every `br-*`, a default route |
| `bridge` | **`lo` only — no addresses, no routes, no veth** |

So `bridge` is a synonym for `none`. It does not break the log driver (the plugin socket and the
FIFOs are filesystem objects) but it makes the OTLP/query endpoint reachable by nothing. `host` plus
a bridge-gateway bind is the only container-reachable posture.

**`host-gateway` resolves to the daemon's `host-gateway-ip`, not the per-network gateway.** A
container on a user-defined network whose gateway is `172.23.0.1` still gets
`172.17.0.1  host.docker.internal` in `/etc/hosts`. That is what makes a single endpoint value work
for containers on every bridge network, compose-created ones included.

**Reachability envelope.** Bound to a bridge gateway only (verified with `ss -ltn`): reachable from a
default-bridge container and from a user-defined-network container, refused from the host's LAN
address. "Not outside the computer" holds; **"not reachable by other containers on the box" does
not**, and `/admin/*` is unauthenticated. `IMBH_ALLOW_FROM` is the mitigation, not a fix for the
endpoint being unauthenticated.

**Settable env semantics.** Setting a `settable` var works; setting it to the **empty string** works
(this is the no-TCP posture); a var declared `"settable": null` is refused by the daemon. This is why
every operator-tunable knob arrives as an environment variable: a plugin's `entrypoint` args are
frozen in `config.json`, `env` entries are not.

## Runtime bridge-network discovery

Originally the plugin bound a single address baked in at package time: `build.sh` ran
`docker network inspect bridge --format '{{range .IPAM.Config}}{{.Gateway}}{{end}}'` once and applied
it with `docker plugin set`, over a hard-coded `172.17.0.1` default. That default is wrong on any
daemon with a custom `bip` or a re-created `docker0`, and on **every registry install** — the
documented `docker plugin install` cannot probe your daemon. The failure is silent: logging is
filesystem-only and keeps working, so the only symptom is a query endpoint that never answers.

`IMBH_LISTEN_ADDR=auto` (the shipped default) now resolves at run time to every bridge gateway,
re-resolved every `IMBH_DOCKER_NETWORK_REFRESH` (default 30s, `0` = one-shot).

### Two backends

| Backend | How | Sees |
|---|---|---|
| Engine API | `GET /networks` over the daemon's Unix socket | network names, IPAM gateways/subnets, per-network container attachments |
| Interface scan | `getifaddrs` in the host netns | gateways and subnets |

The scan is viable *because* of the `host` netns finding above. Docker programs a bridge interface's
address **from** its IPAM gateway, so on a stock daemon the scan reproduces the API's gateway/subnet
answer exactly — the same IPAM data read one layer down. Interfaces qualify on name (`docker0`, or
`br-` + 12 hex characters) **and** the existence of `/sys/class/net/<name>/bridge`, which excludes a
veth named like a bridge and libvirt's `virbr*`. The scan cannot name the Docker network, list
attached containers, or see a bridge renamed via `com.docker.network.bridge.name`.

The API probe re-runs on **every** refresh, not once: at `docker plugin enable` during daemon boot
the API socket may not be serving yet, and a one-shot probe would strand the process in scan mode for
life.

### The deadlock that shapes the container-attribute design

**Nothing on the plugin's request path may call the Engine API.** `dockerd` calls `StartLogging`
synchronously during container start while holding that container's lock, and the API's
network-inspect path resolves attached containers — a call back can deadlock the daemon against its
own log driver.

The consequence is designed for rather than hidden: `StartLogging` reads the last published snapshot,
and a container that started between two scans simply has no network attributes yet.
`Container::resource` is an `RwLock<Arc<Resource>>`, and a refresh that learns new attachments swaps a
fuller resource in. Records before the swap lack `container.network.*`; records after carry it.
`encode` reads the `Arc` once per batch *group* (not per record), and `set_networks` is a no-op when
nothing changed — which is what keeps the pointer-equality grouping intact across an idle daemon's
refreshes.

### Packaging posture

The Engine API needs the daemon's socket in the plugin's mount namespace. **No mount is declared.** A
managed plugin's bind source must already exist on the daemon host; under rootless Docker the socket
is not at `/var/run/docker.sock`; and `mounts: []` is guarded by `tests/docker_plugin_config.rs`
because exactly that class of bug shipped once (the `data` bind-mount → `propagatedMount` fix). So the
managed plugin runs in scan mode — complete for binding and the allow-list — and forgoes container
attributes. A standalone `imbhd` next to a daemon gets API mode for free.

## Storage

`/var/lib/imbh` is a **`propagatedMount`**, daemon-provisioned at
`/var/lib/docker/plugins/<plugin-id>/propagated-mount`. It must equal `entrypoint[1]` or the database
silently lands in the rootfs, which is replaced on upgrade. Persistence across `disable`/`enable` is
measured; **`docker plugin rm` destroys the database** — treat it as `DROP DATABASE`. Behaviour under
`docker plugin upgrade` is not yet measured.

## Publishing

Two artifacts, never one tag: a tag cannot be both an image and a plugin
(`application/vnd.docker.plugin.v1+json` vs `…container.image.v1+json`). Managed plugins have **no
manifest lists**, so there is one plugin per architecture and no arch-agnostic tag — `X.Y.Z-amd64`,
`X.Y.Z-arm64`. The plugin store is content-addressed, so CI creates → pushes → removes one tag at a
time. The log-driver feature is deliberately omitted from the macOS builds.

## Footprint discipline

The `docker` feature adds **no crate**: the wire types use prost's derive and OTLP's message types,
already in the default `imbh` graph via `imbh-otlp`, and JSON goes through `imbh::parse_json` rather
than `serde_json` (`docker/json.rs`). Discovery kept that streak — `libc` was already a direct
dependency (signal handling) and `tokio-stream` was already in the default graph via
`datafusion-datasource-json`; the Engine API client is HTTP/1.1 written by hand over
`std::os::unix::net::UnixStream`, and CIDR matching is ~50 lines of `std::net` rather than an `ipnet`
dependency.

`docker-remap` is the one exception in the whole workspace — the only feature that adds crates on
purpose (vrl; +89 crates, +3.8 MiB on the plugin build). Every other feature comment in
`crates/imbh-server/Cargo.toml` ends in "adds no new crate", and that is load-bearing documentation.

Note the gate's blind spot: `scripts/footprint-gate.sh` counts `cargo tree -p imbh` (the facade), and
dependency direction is `imbh ← imbh-server`, so nothing in this crate reaches the number the budget
is written against. It prints an informational, non-failing plugin-build size instead. Feature work
here has to be measured deliberately.

## Feature gating

All of the discovery work lives under the `docker` module — `docker/addr.rs` (CIDR, allow-list, bind
spec, the `Discovery` trait), `docker/networks.rs` (both backends, the refresh thread),
`docker/serve.rs` (the multi-address supervisor). `lib.rs` keeps only two things: `serve_on_listener`
(the pre-existing accept loop, split out from its bind so several listeners can share one runtime)
and `pub(crate) type PeerFilter = Arc<dyn Fn(IpAddr) -> bool + Send + Sync>` — one `Option` the accept
loop checks. Even the rate-limited refusal warning lives inside that closure. A default build's accept
path is what it always was, one branch heavier.

`imbhd`'s `main` wires this through `src/net.rs`, which has two implementations of one shape: the
`docker` one owns discovery, the allow-list and the supervisor; the other is an empty struct whose
`serve_*` methods are the single-address calls `imbhd` always made.

## Known gaps

- Docker's `labels-regex` / `env-regex` and `tag` log-opts are unsupported — a regex engine and a
  template language are not worth the footprint. List the keys; use `imbh-service` instead of `tag`.
- Follow mode advances by timestamp, so two records sharing one nanosecond would be reported once.
  (The `--tail 0 -f` event-time race was closed by moving the follow cursor to `observed_time`.)
- A remap script's `.resource` is frozen at `StartLogging`, so it does not see a resource that a
  later network refresh replaced. `.info.networks` *is* refreshed per line.
- Nothing exercises the packaging path (`build.sh` → `plugin create` → `enable` → a container logging
  through it) automatically. Valid config + valid binary + fails at `enable` is invisible to every
  existing test.

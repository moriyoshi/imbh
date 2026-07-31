# Changelog

All notable changes to IMBH are recorded here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0/).

Every crate in the workspace shares one version and is released together under a single `vX.Y.Z`
tag (see `[workspace.metadata.release]` in the root `Cargo.toml`). On release, cargo-release closes
the `## [Unreleased]` section below into a dated version heading and opens a fresh one; write new
entries under `## [Unreleased]` as you go. The heading must stay present and multiline-anchorable —
the `pre-release-replacements` in `crates/imbh/Cargo.toml` match it with `exactly = 1`, so a
release aborts if it is missing or duplicated.

## [Unreleased]

### Added

- **Per-connection deadlines for `imbhd` (`imbh-server`).** The server is thread-per-connection and its
  parser blocked with no deadline, so a client that connected and said nothing held a thread (and a `Db`
  handle) indefinitely. Two phase deadlines now bound it, with deliberately different rules:
  `IMBH_HEADER_TIMEOUT` (new; default `10s`) caps the request line + headers **in total**, and
  `IMBH_BODY_TIMEOUT` (new; default `30s`) is a **per-read** allowance for the body plus the write
  allowance for the response. So a large OTLP body over a slow link still succeeds — the rule is "do not
  stall", not "do not take a while" — while an idle, trickling, or stalled client is answered
  `408 Request Timeout` and disconnected, having ingested nothing. `0` disables either phase.

  New public API, additive: `IoTimeouts` (with `DISABLED`), `io_timeouts`, `DEFAULT_HEADER_TIMEOUT` /
  `DEFAULT_BODY_TIMEOUT`, and `serve_with_until` (`serve` / `serve_until` use `IoTimeouts::default()`).
  The Docker plugin endpoint applies the defaults to its own socket, which also means a `docker logs -f`
  client that vanishes without closing no longer holds its thread and stream open.

- **Signal handling and graceful shutdown for `imbhd` (`imbh-server`).** `SIGINT`/`SIGTERM` (Ctrl-C,
  `docker stop`, systemd, `kill`) now wind the process down instead of killing it: every listener stops
  accepting, in-flight requests get `IMBH_SHUTDOWN_TIMEOUT` (new; default `5s`, `0` to not wait) to
  finish, the Docker plugin's container readers stop and its ingest queue is drained into the DB, and
  `Db::close()` seals the buffer — so `imbhd` exits 0 and the next start replays nothing instead of
  recovering everything since the last seal from the WAL. A **second** signal exits immediately with
  `128 + signum`.

  New public API on the crate, all additive: `imbh_server::Shutdown` (the token — `trigger`, `wait`,
  `is_triggered`, `on_trigger`, `install_signal_handlers`, `drain_timeout`), `serve_until`,
  `docker::serve_plugin_until` / `serve_plugin_with_until`, `grpc::serve_grpc_until` /
  `serve_grpc_blocking_until`, `shutdown_timeout`, and `docker::ingest::Ingestor::shutdown`. The
  existing `serve` / `serve_plugin` / `serve_grpc*` entry points keep their signatures and their
  "serve until the process exits" contract, so a host that drives its own lifecycle can adopt the token
  at its own pace.

  Notes on the implementation: `accept` is **woken** (one throwaway connection to the listener's own
  address) rather than polled, because each response carries `Connection: close` — a poll tick would
  land on every request's latency. The signal handler does only async-signal-safe work (an atomic store
  plus one byte down a self-pipe); a watcher thread does the rest. Signal handling is Unix-only and
  adds **no crate** to the footprint graph: `libc` (std cannot catch `SIGTERM`) is already there via
  DataFusion, so the gate stays at 275 crates.

## [0.2.0] - 2026-07-30

### Added

- **Prebuilt binaries and a container image on every release (CD).** `imbhd` and `imbh-tui` no longer
  have to be built from source. `.github/workflows/release.yml` now builds both in the release profile
  for five targets — `x86_64`/`aarch64-unknown-linux-gnu` (glibc 2.35 floor, built natively on
  22.04 runners), `aarch64`/`x86_64-apple-darwin`, and `x86_64-pc-windows-msvc` — with the
  `grpc,tracing` feature set (plus `docker` on Linux), smoke-tests each artifact on the runner that
  produced it, and attaches one archive per platform plus a `SHA256SUMS` to the GitHub Release for the
  tag. Each archive carries `LICENSE` and `THIRD-PARTY-NOTICES.txt`. A multi-arch
  (amd64 + arm64) image containing both binaries is published to `ghcr.io/moriyoshi/imbh` as
  `X.Y.Z`, `X.Y`, and `latest`; it copies in the already-built binaries rather than compiling, so the
  arm64 leg costs no emulated fat-LTO build. `workflow_dispatch` runs the whole path as a rehearsal
  that publishes nothing. See README.md "Install the binaries".

- **`docker/Dockerfile` + `scripts/build-image.sh`** for that image, so it is reproducible locally and
  not only in CI: run the script bare and it compiles both binaries for the host architecture with the
  release feature set and builds a single-arch image. The Dockerfile's header states the build-context
  contract that both it and the release workflow satisfy. Distinct from
  `crates/imbh-server/docker-plugin/`, which builds the logging *plugin* rootfs.

- **A flush scheduler with selectable strategies (`FlushPolicy`).** `Maintenance` already chose *who*
  runs the background loop; the new `DbBuilder::flush(FlushPolicy)` chooses *when* it seals the buffer.
  The triggers OR together and are each optional: periodic (`FlushPolicy::periodic(d)`), buffered heap
  (`.at_buffer_bytes(n)`, defaulting to the memory-budget-derived threshold), buffered rows
  (`.at_buffer_rows(n)`), on-disk WAL size (`.at_wal_bytes(n)` — sealing is what lets the WAL be
  reclaimed), and idle (`.after_idle(d)`); `.tick(d)` sets the evaluation cadence and
  `FlushPolicy::manual()` disables automatic sealing entirely. A policy also parses from a spec string
  (`"interval=5s,wal=64MiB"`, or `"manual"`) via `FromStr`. Leaving it unset preserves the previous
  behavior exactly: seal on the `Maintenance` interval and at the byte threshold. See ARCHITECTURE.md
  §5/§10.2.

- **`imbhd` now flushes on its own**, configured by `IMBH_FLUSH` (default `interval=5s`) and
  `IMBH_MAINTENANCE_INTERVAL` (default `60s`, the retention cadence). Previously the reference server
  opened the DB with the library default `Maintenance::Manual`, so **nothing ever sealed** unless an
  operator POSTed `/admin/flush`: rows stayed in the mutable buffer, the WAL was never reclaimed, and
  neither RSS nor disk use was bounded. Both variables are `settable` on the Docker log-driver plugin.
  A malformed spec fails startup rather than silently running a different cadence.

- **`WalMode::Interval(d)` is honored by that scheduler.** It previously fsynced only opportunistically
  on `flush`/`close` (no timer existed), so the default interval mode never delivered its 1s
  durability window on an otherwise idle writer. New `Storage::sync_wal` / `Storage::wal_sync_interval`
  back it; `Storage::flush_gauges` (buffered bytes/rows + idle clock) and `Storage::seal_threshold_bytes`
  expose what the policy's triggers compare against.

- **`imbh-tui`: a full-content trace detail screen, and a per-span drill-down.** The Traces screen
  drew a selected trace's waterfall into a fixed 45% slice of the results area with no scroll offset,
  so any trace deeper than that pane was partly unreachable. Enter on the trace list now opens
  `Route::TraceDetail` — the whole waterfall as a scrolling list with a span cursor, a header
  (trace id, span count, duration, start), and a summary of the cursored span when the area is tall
  enough — and Enter on a waterfall row opens `Route::SpanDetail` with that span's full fields
  (ids/parent, service, kind, status, offset into the trace, the three attribute maps, raw
  events/links). `L` from either correlates Logs by trace id *and* span id, closing the per-span
  drill-down gap. Both follow the existing non-modal detail pattern and cost no extra query: the
  list already materializes the selected trace to draw its preview. The preview pane itself still
  does not scroll, but now reports "Waterfall: N of M spans" instead of silently truncating.

- **`imbh-server`: a Docker logging-driver plugin**, behind the new optional, off-by-default
  `docker` feature (Unix only). `imbhd --features docker` serves the `docker.logdriver/1.0` plugin
  API on a Unix socket, so `docker run --log-driver imbh` writes a container's stdout/stderr
  straight into the embedded database — queryable with SQL, `matches()` full-text search, and the
  typed logs API — while `docker logs` (history, `--tail`, `--since`/`--until`, `-f`) is served back
  out of stored rows. Container identity becomes OTel resource attributes (`container.id`,
  `container.name`, `container.image.*`, `container.runtime`, plus `--log-opt labels=`/`env=`
  selections); stdout/stderr map to configurable severities and the `log.iostream` attribute; lines
  Docker splits are reassembled into one record. The endpoint is inert unless
  `IMBH_DOCKER_PLUGIN_SOCKET` names a socket. Adds **no crate** to the dependency graph. Packaging
  lives in `crates/imbh-server/docker-plugin/`; see
  [docs/DOCKER_LOG_DRIVER.md](./docs/DOCKER_LOG_DRIVER.md) and ARCHITECTURE.md §10.16.

- **`imbhd` listen addresses are configurable by environment**, and individually disableable.
  `IMBH_LISTEN_ADDR` and `IMBH_GRPC_LISTEN_ADDR` back the existing positional arguments (argument >
  environment > default); an **empty** value opens no socket for that transport. This is what lets a
  managed Docker plugin retune its endpoints with `docker plugin set` -- a plugin's entrypoint
  arguments are frozen in its `config.json` -- and what lets an operator run the log driver with no
  network port at all. `main` now runs every configured endpoint on its own thread and parks on all
  of them, so HTTP, gRPC, and the plugin socket are independently optional.

### Fixed

- **`THIRD-PARTY-NOTICES.txt` did not cover the binaries actually distributed.** It was generated for
  `imbh-server` with *default* features, so it attributed none of the tonic/hyper/h2/tower subtree
  that the `grpc` feature links, nothing from `tracing-subscriber`, and nothing of `imbh-tui`'s
  ratatui/crossterm/rand subtree -- while README.md "License" promises those notices ship with every
  binary distribution (Apache-2.0 §4(d)). `scripts/gen-notices.sh` now generates across the whole
  workspace with all features for every published target (267 Apache-2.0 / 94 MIT crates, up from
  210 / 59), and the file ships inside every release archive and in the image at
  `/usr/share/doc/imbh/`.

- **The license gate only ever vetted the host target with default features.** `deny.toml`'s `[graph]`
  now sets `all-features = true` and lists all six shipping targets, so the `grpc`/`docker` subtrees
  and target-specific dependencies (`windows-sys`, `core-foundation`, ...) are covered. This found no
  violations, but the previous configuration could not have found any.

- **Docker log driver: `docker logs -f` dropped the first line.** When the history query came back
  empty, follow mode set its watermark to the wall clock and then asked only for records newer than
  that instant -- but a record's timestamp is when the *container emitted* the line, while ingest
  lands it up to one batch interval later, so lines already emitted and not yet stored were skipped
  permanently. `docker logs -f` on a freshly started container hit this every time. The watermark now
  stays at the request's lower bound until something has actually been written (`--tail 0` still
  jumps to the present, which is that flag's defined semantic). Found by running the plugin against a
  real `dockerd`.

- **The plugin rootfs image could not be built in a working checkout.** Its Dockerfile builds from
  the repository root, so `COPY . .` pulled in `target/` and `.agents-workspace/` -- hundreds of
  gigabytes of build artifacts. Docker transfers the whole context before the first instruction, so
  the build appeared to hang rather than fail. Added a root `.dockerignore`; the context drops from
  614 GB to 4.8 MB.

## [0.1.1] - 2026-07-28

### Changed

- **Every GitHub Actions `uses:` is pinned to a 40-hex commit SHA** (with a trailing `# vX.Y.Z`
  comment) across `ci.yml`, `release.yml`, and `soak.yml`, so a moving tag can no longer change what
  CI — and therefore the release path — executes. The actions were upgraded to their current
  releases at the same time (`actions/checkout` v4 -> v7.0.1, `actions/upload-artifact` v4 -> v7.0.1,
  `Swatinem/rust-cache` v2 -> v2.9.1, `taiki-e/install-action` v2 -> v2.85.0); `dtolnay/rust-toolchain`
  has no usable version tag, so it is pinned to the `stable` branch head with an explicit
  `toolchain: stable` on every step — that freezes the action, not the toolchain.

### Fixed

- **Windows: every on-disk `Db::open` failed** with `storage error: WAL dir fsync: Access is denied.
  (os error 5)`. The durability path fsync'd a directory by opening it as a `File` — a POSIX idiom
  Windows rejects — so no on-disk database could be opened at all on that platform (in-memory was
  unaffected). Both call sites are now compiled out on Windows: the WAL segment create/rotate
  (`imbh-storage`'s `wal.rs`) and the seal/manifest rename (`imbh-storage`'s `lib.rs`), matching what
  SQLite, LMDB, and RocksDB do. File-content durability is unchanged; see ARCHITECTURE.md §7
  "Directory fsync (platform note)" for what is assumed rather than enforced on NTFS. A
  `windows-latest` CI job now guards the on-disk path. ([#3](https://github.com/moriyoshi/imbh/issues/3))

## [0.1.0] - 2026-07-24

### Added

- Initial public workspace: the `imbh` facade plus `imbh-core`, `imbh-otlp`, `imbh-storage`,
  `imbh-index`, `imbh-query`, `imbh-proto`, `imbh-server` (the `imbhd` reference server),
  `imbh-tracing`, `imbh-otel-exporter`, `imbh-lgtm`, and `imbh-tui`. Milestones M0–M6 complete.
  All 12 crates published to crates.io (`imbh-test-support` is dev-only and stays unpublished).

<!-- next-url -->
[0.2.0]: https://github.com/moriyoshi/imbh/releases/tag/v0.2.0
[0.1.1]: https://github.com/moriyoshi/imbh/releases/tag/v0.1.1
[0.1.0]: https://github.com/moriyoshi/imbh/releases/tag/v0.1.0

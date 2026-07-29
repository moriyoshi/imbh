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
[0.1.0]: https://github.com/moriyoshi/imbh/releases/tag/v0.1.0

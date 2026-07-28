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
[0.1.0]: https://github.com/moriyoshi/imbh/releases/tag/v0.1.0

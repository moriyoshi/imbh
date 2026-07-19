# Documents for both humans and coding agents

* [./.agents/docs/ARCHITECTURE.md](./.agents/docs/ARCHITECTURE.md) ... the canonical design reference for imbh (architecture, data model, storage engine, search, query, the full public API surface, footprint engineering, workspace layout, risks, open questions, and the M0 footprint measurements in Appendix C). Human-reader-ready and authoritative; read it before changing any subsystem boundary or the API surface.

# Documents for coding agents

* [./.agents/docs/OVERVIEW.md](./.agents/docs/OVERVIEW.md) ... the canonical high-level overview: vision, goals + footprint budgets, non-goals, the ingest→storage→query pipeline, the crate map, and status/milestones. Start here for orientation.
* [./.agents/docs/JOURNAL.md](./.agents/docs/JOURNAL.md) ... findings, insights, and peer code review history. Append-only.
* [./.agents/docs/LTM/INDEX.md](./.agents/docs/LTM/INDEX.md) ... long-term memory index for durable project knowledge under `./.agents/docs/LTM/`.
* [./.agents/docs/TODO.md](./.agents/docs/TODO.md) ... open to-do items extracted from JOURNAL.md during `good-sleep` consolidation, plus tracked follow-ups. Check and update this file when picking up or finishing work.
* [./.agents/docs/QUALITY_GATE.md](./.agents/docs/QUALITY_GATE.md) ... the standard verification gate to run before declaring a change complete: the Rust gate (`fmt` / `build` / `clippy` / `test`) plus the footprint gate.
* [./.agents/docs/TESTING.md](./.agents/docs/TESTING.md) ... testing strategy: the per-crate unit suites and the crash/round-trip/footprint tests the plan calls for. Read before adding or changing tests.

# Rules and protocols

## General

* imbh is a **small-footprint, embeddable observability database suite in Rust** — a *library* first, powered by Apache DataFusion (SQL/query) and Tantivy (full-text/term search), following OpenTelemetry semantics (logs, traces, metrics). Any HTTP server is wiring the *host* owns; the reference `imbhd` binary (M5) is one example wiring, not the product. Read `./.agents/docs/ARCHITECTURE.md` (and `OVERVIEW.md` for orientation) before changing subsystem boundaries or the public API.
* **Project phase**: M0–M6 are complete (see `OVERVIEW.md` §13) and **v0.1.0 is released** — all 12 shipping crates are published on crates.io. The build/test/lint gate below applies to every Rust change (per `ARCHITECTURE.md` §12 workspace layout). Because the crates are public now, a breaking change to the published API surface needs a semver bump, not a silent edit.
* The intended workspace is a Cargo workspace of focused crates: `imbh-core`, `imbh-otlp`, `imbh-storage`, `imbh-index`, `imbh-query`, the `imbh` facade, and optional `imbh-otel-exporter` / `imbh-server` (see `ARCHITECTURE.md` §12). Dependency direction: `core ← {otlp, storage, index, query} ← imbh ← {exporter, server}`. `imbh-index` is the only crate that knows Tantivy; `imbh-query` the only one that knows DataFusion — keep those engine dependencies confined to those crates.
* Footprint is a first-class requirement (dependency graph, binary size, and runtime RSS). Do not add a heavy dependency subtree without checking it against `OVERVIEW.md` §2 and `ARCHITECTURE.md` §11 / Appendix C, and prefer `default-features = false` with a minimal feature set (as the M0 probe in Appendix C does for DataFusion, Tantivy, and opentelemetry-proto).

## File Management

* When you'd make summary documents for your work, write them under `./.agents/docs`, not under `/tmp`.
* Temporary files should be created under `./.agents-workspace/tmp`, not under `/tmp`.
* ❌ Do not build artifacts into the version-controlled tree. Cargo's `target/` directory stays out of version control (gitignore it when the repo is initialized); scratch binaries and probe crates go under `./.agents-workspace/tmp` (e.g. `cargo build --target-dir ./.agents-workspace/tmp/target`).
* ❌ Never delete user files without permission. Only safe to delete: files YOU created in THIS session that are in `./.agents-workspace/tmp/`. Always ask first if unsure. Assume all pre-existing files belong to the user.

## Building

* Rust is built with `cargo` (stable toolchain). The `rust-analyzer` LSP plugin is enabled in this environment — use the `LSP` tool for go-to-definition, type lookups, and diagnostics rather than grepping when a symbol is available.
* Run `cargo fmt` on every crate you change before running `cargo build`, `cargo clippy`, or `cargo test`, and before reporting a change as done.
* The standard local gate for any Rust change you make — this applies to subagents too:
  ```
  cargo fmt --all --check          # must print nothing / exit 0
  cargo build --workspace
  cargo clippy --workspace --all-targets -- -D warnings
  cargo test --workspace           # or a focused crate: cargo test -p imbh-storage
  ```
  Fix violations and re-run until clean. Do not declare a change complete with a failing build, clippy, or test. See [./.agents/docs/QUALITY_GATE.md](./.agents/docs/QUALITY_GATE.md) for the full gate, including the footprint checks.
* Footprint matters: after a dependency change, sanity-check the crate count and binary/RSS impact (`cargo tree`, and the M0-style probe / `cargo bloat` when relevant) against the budgets in `OVERVIEW.md` §2 and `ARCHITECTURE.md` Appendix C. Prefer trimming default features over accepting a large subtree.

## Testing

* Make sure that regression tests are ready for your fix.
* Tests must run without external daemons or network access in the default `cargo test --workspace` path. Storage, WAL replay, Parquet round-trip, and the Tantivy↔Parquet `RowSelection` bridge are all testable in-process against a temp directory; keep them that way. Gate anything that genuinely needs privileges or large fixtures behind an opt-in and skip otherwise.

## Git Workflow

* The repository has history on the default branch `main`, with the released `v0.1.0` tag. Releases are cut by `cargo release` (see `README.md` "Releasing"); do not run it, tag, or push unless the user explicitly asks.
* ❌ Do not run `git checkout` or `git restore` against the working tree — another agent may be working concurrently in the same directory. ❌ Never make discretionary commits. Commit or push only when the user explicitly asks.

## Documentation

* Try to write your work summary to one of the existing documents under `./.agents/docs`.
* ❌ Avoid editing any existing sections of `JOURNAL.md`. Append new entries to the end. (The sole exception is the `reconcile-journal-ltm` skill, which may remove entries already consolidated into `.agents/docs/LTM/` per the canonical `## LTM Consolidation Record`.)
* ❌ For repo-authored documentation only (e.g. `AGENTS.md`, `.agents/docs/**`, `docs/**`), never use full-width parentheses (`（` `）`). Use half-width parentheses (`(` `)`) with a half-width space before/after when adjacent to a non-whitespace character. This does not apply to generated or third-party reference files under `skills/**/references/**`.
* ❌ For repo-authored documentation only, never use full-width colons (`：`). Use a half-width colon followed by a half-width space. This does not apply to generated or third-party reference files under `skills/**/references/**`.

## Shell Pitfalls (prezto defaults)

The user's shell uses prezto, which sets aliases and options that break non-interactive scripts:

* ❌ `cp src dst` prompts interactively when `dst` exists (prezto aliases `cp` to `cp -i`). Always `rm -f dst` before `cp`. Also kill any process using the destination file first before replacing a binary.
* ❌ `cat > file <<'EOF'` and `echo > file` fail with `file exists` when the target exists (prezto enables `NO_CLOBBER`). Workaround: `rm -f file` before writing, or use `tee` / `/bin/cat`.
* ❌ `rm file` prompts for confirmation on some files (prezto aliases `rm` to `rm -i`). Always use `rm -f` for non-interactive deletion.

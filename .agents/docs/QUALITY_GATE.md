# Quality gate

The standard verification an agent (or human) runs before declaring a change complete. This is
the one-stop reference so you don't have to rediscover the gate from `AGENTS.md`, `TESTING.md`,
and CI each time. Test-strategy detail lives in [TESTING.md](./TESTING.md); this file is the
"what do I run before I say done" checklist.

## Purpose / when to use

Run this gate:

- Before declaring any change complete.
- Before claiming "CI should pass" or "this is green".

Match the depth of the gate to what you touched (see "Pick your level" at the end). At minimum
every Rust change runs section 1.

> **Applicability.** The Cargo workspace (`ARCHITECTURE.md` §12) now exists: `crates/imbh-{core,otlp,
> storage,index,query}`, the `imbh` facade, `imbh-server` (`imbhd`), `examples/`, and the dev-only
> `imbh-test-support` E2E harness crate. The full gate below applies. As of M0–M6 it is green:
> `fmt`/`build`/`clippy -D warnings`/`test` all pass (**159 tests** in the default `--workspace`
> path, plus the opt-in `fault-injection` and `--ignored` soak runs below) and the footprint gate is
> OK. The Layer-3 E2E suite is described in [TESTING.md](./TESTING.md).

## 1. Rust gate (always, once code exists)

The mandatory gate for any Rust change. Run against the whole workspace, or focus on the crate
you touched while iterating:

```sh
cargo fmt --all --check                              # must print nothing / exit 0
cargo build --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace                               # or focused: cargo test -p imbh-storage
```

Run `cargo fmt` (write mode) on every crate you changed before building. Fix violations and
re-run until clean. Do not declare a change complete with a failing build, clippy, or test.

Notes:

- `cargo test --workspace` needs no external daemons or network. WAL replay, Parquet round-trip,
  and the Tantivy↔Parquet `RowSelection` bridge are all in-process against a temp directory.
  Anything that genuinely needs privileges or large fixtures must be opt-in and self-skip.
- Build artifacts NEVER go into the version-controlled tree. Cargo's `target/` is gitignored;
  scratch binaries and probe crates go under `./.agents-workspace/tmp` (e.g.
  `cargo build --target-dir ./.agents-workspace/tmp/target`). Temp files go under
  `./.agents-workspace/tmp`, not `/tmp`.
- `rust-analyzer` LSP is available — use the `LSP` tool for diagnostics/type lookups instead of
  ad-hoc greps when a symbol is in scope.
- Prezto shell pitfalls when scripting: `cp` is aliased to `cp -i` (`rm -f dst` first), `rm` to
  `rm -i` (use `rm -f`), and `NO_CLOBBER` makes `>` / heredocs fail on an existing file
  (`rm -f` first or use `tee`).

## 2. Footprint gate (dependency / feature changes)

Footprint (dependency graph, binary size, runtime RSS) is a first-class requirement
(`OVERVIEW.md` §2 / `ARCHITECTURE.md` §11, Appendix C). After any dependency add/bump or feature-flag change, run the
scripted gate — it checks crate count + the release `imbhd` binary against the §2 budgets and
exits non-zero over a hard limit:

```sh
./scripts/footprint-gate.sh          # crate count + imbhd release binary vs §2 budgets
cargo bloat --release --crates       # where binary size goes (if cargo-bloat installed)
```

The gate also verifies the `search` feature lever (§11): it fails if `imbh --no-default-features`
still links tantivy or no longer compiles, so the footprint knob can't silently break.

Latest measured (2026-07-18, aarch64-glibc, release-small profile): **275** unique crates (≤ 275
target, ≤ 300 hard) and **31.2 MiB** for the `imbhd` binary (≤ 42 MB musl target — a glibc floor,
not the musl number). Both are within budget; the v0.1 footprint exit criterion is met on this
axis. Turning `search` off (`imbh --no-default-features`) drops the tantivy subtree to **216**
crates. Idle/steady RSS now has an opt-in soak gate (`crates/imbh/tests/soak_rss.rs`, Linux,
`#[ignore]`): a sustained ingest→seal→query loop asserts steady `VmRSS` stays under a runaway
sentinel (a recent run: idle ~14 MiB, steady ~172 MiB over 20k rows, budget 512 MiB). Run it with
`cargo test -p imbh --test soak_rss -- --ignored --nocapture`. Peak RSS (VmHWM) is still only
sampled by the `rss-probe` example, not asserted.

- Prefer `default-features = false` plus an explicit minimal feature set (as the M0 probe does
  for DataFusion, Tantivy, and opentelemetry-proto — see Appendix C).
- A regression in crate count / binary size / RSS relative to Appendix C should be justified in
  `JOURNAL.md`, not merged silently. Confine DataFusion to `imbh-query` and Tantivy to
  `imbh-index` — a heavy dep leaking into `imbh-core` widens the whole graph.
- When a change is expected to move RSS or binary size materially, re-run the M0-style probe
  (Appendix C has the source; recreate it under `.agents-workspace/tmp`) and record the numbers.

## 3. License / notice gate (release time)

Two release-time checks, both wrapped in scripts that degrade gracefully when the tool is absent
(they print an install hint and exit 0, so an offline gate never breaks).

### 3a. License-compatibility gate

`deny.toml` (repo root) configures license compatibility + duplicate-version + no-openssl checks.
The allowlist there (`[licenses].allow`) is mirrored by `about.toml`'s `accepted` list — keep the
two in sync whenever either changes. `deny.toml`'s `[graph]` is equally load-bearing: it sets
`all-features = true` and lists all six shipping targets, so the check covers what CD actually ships.
It used to be host-only with default features, which silently exempted the entire tonic/hyper/h2/tower
subtree behind `grpc` and every target-specific dependency. Keep `[graph].targets` in sync with
`about.toml`'s `targets` and with `release.yml`'s build matrix.

```sh
./scripts/license-gate.sh            # cargo deny check licenses (skip-with-note if cargo-deny absent)
cargo deny check                     # full: licenses + bans + (online) advisories
```

`cargo-deny` is installed in the dev container and wired into CI (`ci.yml`'s `licenses` job on
default-branch pushes, plus `release.yml`). The license check itself needs no network (advisories
do, and can be skipped offline). It went green for the v0.1.0 release.

### 3b. Third-party notice generation

`scripts/gen-notices.sh` renders `THIRD-PARTY-NOTICES.txt` for the binaries actually distributed —
`imbhd` **and** `imbh-tui` — via `cargo about generate` using the repo-root `about.toml` + `about.hbs`.

```sh
./scripts/gen-notices.sh             # cargo about generate ... (skip-with-note if cargo-about absent)
```

Scope is `--workspace --all-features` over `about.toml`'s six targets, deliberately a **superset** of
any one build. That is not laziness: this file ships inside every release archive and in the container
image (Apache-2.0 §4(d), README "License"), CD builds with `grpc,tracing` (+ `docker` on Linux), and
`grpc` alone pulls the whole tonic/hyper/h2/tower subtree. Scoping to `crates/imbh-server/Cargo.toml`
with default features — which is what this script did before CD existed — attributed none of that and
nothing of `imbh-tui`'s ratatui/crossterm subtree. Over-attributing is safe; under-attributing is a
licence breach. **Do not narrow this to "just what this build links".**

Offline caveat: `cargo about generate` needs network (it resolves license text from the crates.io
index), so run it in a networked env — `release.yml` installs the tool and does this on every `v*`
tag, and its `build` jobs package *that* freshly generated copy rather than the tracked one. Regenerate
and commit whenever the shipped dependency graph or feature set changes; `release.yml` emits a warning
into the run summary when the generated file differs from the tracked copy. This is the Rust analogue
of cornus's `audit-licenses`.

### 3c. Packaging dry-run (`cargo package --workspace`)

`cargo release` packages and verifies every member before it publishes, so run the dry-run first:

```sh
rm -rf target/package target/debug/.fingerprint/imbh-*   # see the staleness trap below
cargo package --workspace
```

**Staleness trap — read this before debugging a verify failure.** `cargo package --workspace`
stages each member into `target/package/tmp-registry/` and rewrites the internal `path` deps to
registry requirements, so the verify build resolves `imbh-core` and friends from a *local registry
source*, not from `crates/`. Cargo assumes registry sources are immutable: once it has compiled
`imbh-core 0.1.0` from that temp registry, regenerating the `.crate` does **not** invalidate the
compiled unit, because the package id (name + version + source) is unchanged. Since the whole
workspace shares one version that only moves at release time (`shared-version = true`), every
`cargo package` run between releases can link a *stale* rmeta built from an older snapshot of the
source.

The symptom is a verify-only compile error that contradicts the working tree, e.g. a
`cargo build --workspace` that is green while verify reports
``no associated function ... named `new` found for struct `Lsn` `` against the current
`pub type Lsn = std::num::NonZero<u64>` in `crates/imbh-core/src/ids.rs`. Confirm before chasing it
as a source bug:

```sh
# which imbh_core rmeta the verify build links, and whether it matches the current source
cargo package --workspace --verbose 2>&1 | grep -o 'imbh_core=[^ ]*'
strings target/debug/deps/libimbh_core-<hash>.rmeta | grep -c NonZero   # 0 => stale artifact
```

Fix by dropping the cached units, then re-running: `rm -rf target/debug/.fingerprint/imbh-*` (and
the registry-sourced `target/debug/deps/imbh_*` artifacts, identifiable by a dep-info file that
references `~/.cargo/registry/src/`). Nothing under `crates/` needs to change.

## 4. Distribution gate (release time, CI-only)

Everything users install that is not a crates.io crate: the per-platform archives of `imbhd` +
`imbh-tui` and the `ghcr.io/moriyoshi/imbh` container image. This gate lives in `release.yml`'s
`build` / `publish` / `image` jobs and is **not** reproducible in full locally (it needs five runner
platforms), but the two pieces that can be are:

```sh
# imbhd with the shipping feature set — ci.yml lints `docker` and `grpc` separately, never together
cargo build --release -p imbh-server --features docker,grpc,tracing
./scripts/build-image.sh              # host-arch container image (skip-with-note if docker absent)
```

Rehearse the rest with a `workflow_dispatch` run of `release.yml`: with `dry_run` at its default it
builds and smoke-tests all five archives and both image architectures and publishes nothing. Do that
after any change to the matrix, the feature set, `docker/Dockerfile`, or the base image.

Two invariants worth restating, because breaking either produces a *silently* bad release:

- **The glibc floor is a contract between two files.** The Linux legs build on `ubuntu-22.04`
  runners (glibc 2.35) so the binaries also run on `docker/Dockerfile`'s `debian:bookworm-slim` base
  (glibc 2.36). Moving either one moves the other.
- **The image must not compile anything.** It copies binaries the matrix already built and
  smoke-tested. Introducing a `RUN cargo build` would put a fat-LTO compile under QEMU on the release
  path (hours, per architecture). The one `RUN` in the Dockerfile is pinned to `$BUILDPLATFORM`
  precisely so no emulation is needed at all.
- **The build-context layout is defined once, in `scripts/build-image.sh`.** `docker/Dockerfile`
  depends on `linux/<goarch>/{imbhd,imbh-tui}` plus `LICENSE` and `THIRD-PARTY-NOTICES.txt` at the
  context root. The `image` job stages that by calling the same script with `--stage-only --prebuilt
  <goarch>=<dir>`, so a local build and a release build cannot drift apart in a way the Dockerfile
  would notice. Do not re-inline the staging into the workflow.

On caching: the release build job caches **only the cargo registry** (`cache-targets: "false"`), and
that is deliberate — see the comment on the step. GitHub scopes Actions caches by ref and will not
restore one created for a *different tag name*, falling back only to the default branch's scope; since
this job never runs on `main`, the first run of a new tag is cold whatever is stored, while five legs
of fat-LTO `target/` caches would evict the `ci.yml` caches that make every PR fast. Corollary worth
knowing: a `workflow_dispatch` rehearsal **from `main`** writes into main's scope, so it both validates
the pipeline and warms the registry cache for the tag run that follows.

## CI (GitHub Actions)

The gate is wired into `.github/workflows/`, mapping the sections above:

- **`ci.yml`** (push to `main` + every PR) — job `gate` runs §1 (fmt / clippy incl. the
  `fault-injection` lint / build / `test --workspace`, which includes the Layer-3 E2E suite) plus the
  deterministic crash-injection E2E; job `footprint` runs §2 (`scripts/footprint-gate.sh`); job
  `licenses` runs the §3 license-compatibility check (`cargo-deny`) **on default-branch pushes only**
  (skipped on PRs).
- **`soak.yml`** (nightly + `workflow_dispatch`) — the opt-in RSS soak and the long interleave-stress
  variant (both `#[ignore]`, Linux). Kept off the per-push path so they don't slow PRs.
- **`release.yml`** (version tags `v*` + `workflow_dispatch`) — §3 **and** §4. Job `licenses` runs the
  license gate (`cargo-deny`) and notice generation (`cargo-about`), installing both tools so the
  scripts run for real, and uploads `THIRD-PARTY-NOTICES.txt` as an artifact; job `build` (5-leg
  matrix) builds and smoke-tests `imbhd` + `imbh-tui` per target and packages that notices file into
  each archive; job `publish` attaches the archives + `SHA256SUMS` to the tag's GitHub Release; job
  `image` pushes the multi-arch image to GHCR. `build` depends on `licenses` deliberately — no binary
  ships from a run whose license gate failed. A `workflow_dispatch` run with `dry_run` (the default)
  builds everything and publishes nothing.

Locally, still run the level matching what you touched:

## Pick your level

- **Docs-only change** (`.agents/docs/**`, `docs/**`): style rules + internal-link sanity. No
  Rust gate.
- **Single-crate code change**: section 1 focused on that crate (`cargo test -p <crate>`), then a
  full-workspace `cargo build` / `clippy` before declaring done.
- **Dependency or feature change**: section 1 (full workspace) **and** section 2 (footprint).
- **Storage seal / WAL / recovery change**: section 1, plus the deterministic crash-injection tests —
  `cargo test -p imbh --features fault-injection --test crash_points` (off by default, so not in the
  workspace run).
- **Footprint / memory-sensitive change**: also run the opt-in RSS soak —
  `cargo test -p imbh --test soak_rss -- --ignored --nocapture`.
- **Packaging / distribution change** (`release.yml`, `docker/Dockerfile`, `scripts/build-image.sh`,
  the shipped feature set, `about.toml`/`deny.toml` targets): section 4, plus regenerate the notices
  (§3b) if the feature set or target list moved.
- **Release**: sections 1–4, including the §3c packaging dry-run. Sections 1–3 are the human's
  pre-`cargo release` checklist; section 4 then runs itself on the tag push.

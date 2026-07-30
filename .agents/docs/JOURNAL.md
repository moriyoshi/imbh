# IMBH Development Journal

Append-only chronological log of findings, insights, and peer code review history. New entries
go at the **end**; do not edit existing sections (the sole exception is the
`reconcile-journal-ltm` skill). Durable knowledge is periodically consolidated out of here into
`.agents/docs/LTM/` by the `good-sleep` skill, and open follow-ups into `.agents/docs/TODO.md`.

All substantive entries through 2026-07-24 have been consolidated into `.agents/docs/LTM/` and
audited by the `reconcile-journal-ltm` skill, then removed from this file (LTM is now the durable
form). What remains is the single canonical `## LTM Consolidation Record` below; new,
not-yet-consolidated entries are appended after it and swept into LTM by a future `good-sleep`
pass. See `.agents/docs/LTM/INDEX.md` for the topic index.

---

## LTM Consolidation Record

This journal has been audited against `.agents/docs/LTM/` by the `reconcile-journal-ltm` skill
(most recently 2026-07-24, covering everything through the `Lsn`/`NonZero` and `query_batches`
tackle-todos entries): every substantive entry from 2026-07-18 onward has its durable knowledge
represented in an LTM document (or, for open follow-ups, in `.agents/docs/TODO.md`), so the
consolidated entries have been removed from this file. Two earlier audits are folded into this
record — the first patched a partial-coverage gap (the typed `logs().volume_by` API, now in
`query-engine-and-typed-apis.md`), and the second corrected LTM text that still described the
pre-`NonZero` `Lsn` contract as current (`storage-engine.md` async receipts and the `IngestReceipt`
field list, `reference-server-exporter-and-ops.md`'s `DbStats.durable_lsn` type and
`db_stats_engine_gauges` note, and `query-engine-and-typed-apis.md`'s Arrow entry-point name).

No synthesis documents exist yet (those are produced by the `deep-sleep` skill); every LTM document
is a direct topic reference and is listed in the mapping, so none are standalone/unreferenced.

| Journal section (heading) | LTM Document |
|---------------------------|--------------|
| M1a: WAL + idempotent replay (durability) | storage-engine.md |
| M1e: retention + maintenance | storage-engine.md |
| M4b: compaction | storage-engine.md |
| M4c: blocking facade + segment-file handoff | storage-engine.md |
| Backlog: error classifiers + opt-in background maintenance scheduler (scheduler part) | storage-engine.md |
| Backlog: WAL truncation after seal (§7) | storage-engine.md |
| Backlog: orphan-segment cleanup on open (§7) | storage-engine.md |
| Self-assessment fix: compaction now covers the List-column metric tables | storage-engine.md |
| storage-engine durability review + fixes (HIGH-severity data-loss bugs) | storage-engine.md |
| storage HIGH 3 fix: seal no longer loses buffered rows on a write error | storage-engine.md |
| storage MEDIUM 5: compaction I/O moved off the lock | storage-engine.md |
| Opt-in asynchronous ingest (`Ingest::Async`) | storage-engine.md |
| Group-commit fsync for the async-ingest worker | storage-engine.md |
| Append-only manifest delta log + compacted checkpoint (§7 Tier-C) | storage-engine.md |
| `Lsn` is now `NonZero<u64>`; the `Lsn(0)` receipt footgun removed at the type level | storage-engine.md (+ corrections in reference-server-exporter-and-ops.md) |
| Session summary: TODO sweep + reader-path & manifest hardening | storage-engine.md, cross-process-concurrency.md |
| M1b: full-text search (Tantivy) + shared tokenizer | full-text-search-tantivy-bridge.md |
| M1c: cost-gated Tantivy → Parquet RowSelection bridge | full-text-search-tantivy-bridge.md |
| per-segment Tantivy span search | full-text-search-tantivy-bridge.md |
| index merge policy + a real shutdown join | full-text-search-tantivy-bridge.md |
| M1d: typed Logs query API + JSON parser | query-engine-and-typed-apis.md |
| M1f: attribute discovery + logs volume | query-engine-and-typed-apis.md |
| M2b: spans as a real table (multi-table storage + query) | query-engine-and-typed-apis.md |
| M3c: typed Metrics API | query-engine-and-typed-apis.md |
| Backlog: `metrics().series(metric)` | query-engine-and-typed-apis.md |
| Backlog: logs cursor paging (`LogQuery::after`) | query-engine-and-typed-apis.md |
| Backlog: `logs().volume_by` (label-broken-down log volume) | query-engine-and-typed-apis.md |
| Backlog: logs attribute matchers (`attr_exists`/`attr_matches`) | query-engine-and-typed-apis.md |
| Backlog: cross-signal consistency — series() over all tables + trace attr matchers | query-engine-and-typed-apis.md |
| Self-assessment follow-through: JSON parser depth guard; id-inconsistency assessed | query-engine-and-typed-apis.md |
| cross-signal attribute discovery | query-engine-and-typed-apis.md |
| imbh-query correctness review + two trust-boundary guards | query-engine-and-typed-apis.md |
| logs/traces `attr_in`, `attr_not_in` negation matcher | query-engine-and-typed-apis.md |
| `logs().count(query)`; numeric attribute range matchers; regex attribute matcher; PromQL label selectors (Metric/Histogram/ExpHistogram) | query-engine-and-typed-apis.md |
| Api handles, SQL bind-parameters, and Tantivy-pushdown verification | query-engine-and-typed-apis.md |
| Read-side scan stats wired into `QueryStats` | query-engine-and-typed-apis.md |
| Query tree primitives + result DTOs made JSON-serdeable (`serde` feature) | query-engine-and-typed-apis.md |
| Query-binding surface: `proto` feature (protobuf inputs + Arrow-batch outputs) | query-engine-and-typed-apis.md |
| Lazy per-batch query scan (I-4a) + stats-on-stream (I-5) | query-engine-and-typed-apis.md |
| Configurable attribute promotion to typed columns; Promotion Stage 3 (pushdown dispatch) | query-engine-and-typed-apis.md |
| TODO sweep: query-plan-shape tests | query-engine-and-typed-apis.md |
| tackle-todos sweep: `query_batches` `--all-features` E0592 collision → `query_batches_with_stats` | query-engine-and-typed-apis.md |
| M2a: spans decode | otlp-and-metrics-data-model.md |
| M3a: scalar metrics decode; M3b: scalar metrics as queryable tables | otlp-and-metrics-data-model.md |
| Backlog: histogram data model + storage table + `histogram_quantile` UDF + typed surface | otlp-and-metrics-data-model.md |
| Backlog: `rate_delta`; reset-aware `rate_counter` | otlp-and-metrics-data-model.md |
| Backlog: catalog covers histograms + PromQL→SQL recipe doc | otlp-and-metrics-data-model.md |
| Backlog: cross-series/time histogram-quantile merge (`HistogramQuery::step`) | otlp-and-metrics-data-model.md |
| Backlog: exponential histogram data model + storage table + quantile | otlp-and-metrics-data-model.md |
| Backlog: OTLP Summary metric type | otlp-and-metrics-data-model.md |
| Backlog: exp-histogram cross-point scale-aligned merge (`ExpHistogramQuery::step`) | otlp-and-metrics-data-model.md |
| Self-assessment pass: fixed 7 robustness/perf issues across the backlog work | otlp-and-metrics-data-model.md |
| metrics-math hardening (adversarial review of the quantile/rate code) | otlp-and-metrics-data-model.md |
| exemplars on scalar metrics / histogram / exp-histogram / `filtered_attributes` / typed accessor / round-trip guards | otlp-and-metrics-data-model.md |
| M2c: typed Traces API | traces-and-error-model.md |
| Backlog: span RED metrics (`traces().span_metrics`) | traces-and-error-model.md |
| Backlog: error classifiers (classifier part) | traces-and-error-model.md |
| Backlog: trace search by span-name text (`TraceQuery::matches`) | traces-and-error-model.md |
| Typed nested `Error` model + findings distilled from the migration | traces-and-error-model.md |
| Trace candidate-selection boundary bug; TraceQL streaming + pushdown; Trace Level-2 | traces-and-error-model.md |
| TraceQL numeric-attribute pushdown + latent facade correctness fix; session roll-up | traces-and-error-model.md |
| Cross-process concurrency: lockfile + WAL-tailing readers (Phases 1–4) | cross-process-concurrency.md |
| `Db` made a concrete struct; handles shared as `Arc<Db>` | cross-process-concurrency.md |
| Cross-process: WAL-off reader guard | cross-process-concurrency.md |
| Session summary: cross-process concurrency complete | cross-process-concurrency.md |
| Phase 3 WAL-tail perf: incremental reader cursor + refresh knob | cross-process-concurrency.md |
| Self-observability: `tracing` wired through the hot paths + end-to-end verification | self-observability-tracing.md |
| `imbh-tracing` helper crate; `DbLayer` sink; dropped inner `db` feature gate; session summary | self-observability-tracing.md |
| Moved the stderr console renderer into `imbh` (`tracing-console`) + findings | self-observability-tracing.md |
| TODO sweep: tracing polish (part) | self-observability-tracing.md |
| M0 walking skeleton (footprint measurements) | footprint-and-feature-gating.md |
| M6a: footprint gate + cargo-deny + release measurement | footprint-and-feature-gating.md |
| Self-assessment (footprint investigation): opentelemetry_sdk is forced into the tree | footprint-and-feature-gating.md |
| `search` feature (feature-matrix axis) + follow-ups | footprint-and-feature-gating.md |
| milestone-completeness checkpoint + fresh footprint | footprint-and-feature-gating.md |
| OTLP-proto vendoring dismissed (addendum) | footprint-and-feature-gating.md |
| M6c producer/consumer feature gating (Phases 0-2) | footprint-and-feature-gating.md |
| M4a: ops/admin (stats + snapshot) | reference-server-exporter-and-ops.md |
| M5: the reference `imbhd` HTTP server | reference-server-exporter-and-ops.md |
| Backlog: `db.export` (Arrow-IPC stream) | reference-server-exporter-and-ops.md |
| Backlog: `DbStats` engine gauges | reference-server-exporter-and-ops.md |
| `imbhd` `GET /stats` + admin maintenance endpoints; hardened `batches_to_json` | reference-server-exporter-and-ops.md |
| `imbh-otel-exporter`: SpanExporter / LogExporter / MetricExporter + crate review | reference-server-exporter-and-ops.md |
| Read-only stats fix (ops part) | reference-server-exporter-and-ops.md |
| Reference server: OTLP/gRPC ingest behind the optional `grpc` feature | reference-server-exporter-and-ops.md |
| Bounded language semantics, translators, and companion TUI (language part) | imbh-lgtm-languages-and-arrow-reads.md |
| Native semantic query models and adapter ownership | imbh-lgtm-languages-and-arrow-reads.md |
| Consolidated `imbh-semantics` + `imbh-query-language` into `imbh-lgtm` | imbh-lgtm-languages-and-arrow-reads.md |
| Real log search in the TUI: an IMBH LogQL dialect with a Tantivy `\|?` operator | imbh-lgtm-languages-and-arrow-reads.md |
| Arrow-native result surface for imbh-lgtm (Phase 1a/1b/3) | imbh-lgtm-languages-and-arrow-reads.md |
| LGTM borrowed-read refactor (Cow-based LabelSet + self_cell) | imbh-lgtm-languages-and-arrow-reads.md |
| Level-2 (raw-Arrow) LogQL / metric reads; TraceQL streaming evaluation; Trace Level-2 | imbh-lgtm-languages-and-arrow-reads.md |
| Session summary: LGTM borrowed-read + Level-2 Arrow reads + trace correctness | imbh-lgtm-languages-and-arrow-reads.md |
| tackle-todos sweep: PromQL lookback fidelity; TraceQL negated matcher on a missing attribute | imbh-lgtm-languages-and-arrow-reads.md |
| tackle-todos sweep: `imbh-lgtm` examples `required-features = ["source"]` | imbh-lgtm-languages-and-arrow-reads.md |
| Bounded language semantics: companion TUI (TUI part) | imbh-tui-and-gen-demo-db.md |
| `gen-demo-db` demo-data generator; id salting; log↔span correlation; multi-level call trees | imbh-tui-and-gen-demo-db.md |
| All TUI panes/UX passes: traces waterfall, focus ring, router, autocompletion, catalog tree, log viewer, time-window, header/chrome, F9 menu, ASCII mode, mascot, pan/zoom | imbh-tui-and-gen-demo-db.md |
| TUI interaction follow-up (pan/zoom, log paging, cross-signal drill-down, small-terminal) + session roll-up | imbh-tui-and-gen-demo-db.md |
| tackle-todos sweep: TUI terminal teardown gaps + chained panic hook | imbh-tui-and-gen-demo-db.md |
| AI coding agent harness adopted from ../cornus | project-meta-ci-docs-and-testing.md |
| M0 walking skeleton (workspace scaffolding) | project-meta-ci-docs-and-testing.md |
| M6b: embedding example + guide | project-meta-ci-docs-and-testing.md |
| Backlog: benchmark harness (`examples/bench`) | project-meta-ci-docs-and-testing.md |
| Docs: refreshed `EMBEDDING.md`; exporter deferred | project-meta-ci-docs-and-testing.md |
| PLAN.md promoted to OVERVIEW.md + ARCHITECTURE.md | project-meta-ci-docs-and-testing.md |
| TODO sweep / TODO sweep II + distilled findings | project-meta-ci-docs-and-testing.md |
| README.md + LICENSE authored; README comparison matrix | project-meta-ci-docs-and-testing.md |
| README sweep; obsolete-sentence sweep; README refreshes | project-meta-ci-docs-and-testing.md |
| Comprehensive E2E test suite (Layer 3); CI wiring (GitHub Actions) + license check; DESIGN consolidation | project-meta-ci-docs-and-testing.md |
| Zero-copy Go-binding prescription (`imbh-go`) | project-meta-ci-docs-and-testing.md |
| Publish harness (cargo-release) + networked license/notice gate; README Releasing section | project-meta-ci-docs-and-testing.md |
| Subcrate READMEs made self-contained for independent crates.io publishing | project-meta-ci-docs-and-testing.md |

See `.agents/docs/LTM/INDEX.md` for the full index.

## 2026-07-24 — tackle-todos sweep (no-op: backlog is dry)

A full `tackle-todos` pass found no dispatchable work.

**Source-marker scan.** All 84 `*.rs` files in the workspace (excluding `target/` and
`.agents-workspace/`) were scanned for `// TODO`, `// FIXME`, `TODO:`, `FIXME:`, `todo!(`, and
`unimplemented!(`. **Zero matches.** There are no systematic, behavioural, validation,
serialization, or test-only markers left in Rust source.

**`TODO.md` items.** Two open entries, both non-dispatchable after verification:

1. *Optional upstream differential runner* — design category, and explicitly deferred by user
   request. Confirmed still unimplemented (no differential-runner artifacts under `crates/`).
   The skill forbids attempting design items without approval, so it stays open.
2. *Check the git staging split before the next commit* — **stale, now closed.** The condition it
   described (a fully-staged index alongside unstaged `query_batches_with_stats` edits) no longer
   holds: `git status --porcelain` returns nothing and the repository has a single commit
   `c1d3ae5 Initial.` containing every file the note named, with `query_batches_with_stats` present
   at `crates/imbh/src/logs.rs:167` and exercised from `crates/imbh/src/lib.rs:2005`. Marked `[x]`
   in `TODO.md` with the verification recorded inline.

No agents were dispatched, no source files were modified, and no build/clippy/test gate was needed
(no code changed). The consolidated list is at `.agents-workspace/tmp/consolidated-todos.md`.

## 2026-07-24 — `cargo package` verify failure was a stale build artifact, not a source bug

**Symptom.** `cargo package --workspace` failed with four `E0599` errors in `imbh-storage`
(``no associated function or constant named `new` found for struct `Lsn` ``, at the packaged
`src/lib.rs:433/449/538/697`), ending in `error: failed to verify package tarball`. Meanwhile
`cargo build -p imbh-storage` was green, and `crates/imbh-core/src/ids.rs:75` reads
`pub type Lsn = std::num::NonZero<u64>;` — for which `Lsn::new` obviously exists. The
``struct `Lsn` `` wording in the diagnostic was the tell: the compiler was not looking at a
`NonZero` alias at all.

**Root cause.** `cargo package --workspace` stages every member into
`target/package/tmp-registry/` and rewrites internal `path` deps to registry requirements, so the
verify build resolves `imbh-core` from a *local registry source*. Cargo treats registry sources as
immutable: the package id (name + version + source) is unchanged across runs, so regenerating the
`.crate` never invalidates the already-compiled unit. Because `shared-version = true` keeps every
crate at one version between releases, the verify build linked an `imbh-core` rmeta compiled on a
previous day, from a snapshot where `Lsn` was still a `struct`.

**Evidence.** `cargo package --workspace --verbose` showed `Fresh imbh-core` and
`--extern imbh_core=target/debug/deps/libimbh_core-7f0f23a74c4068dc.rmeta`. That rmeta was dated a
day earlier, its dep-info pointed at `~/.cargo/registry/src/-4c5272afa94ed5a5/imbh-core-0.1.0/`,
and `strings … | grep -c NonZero` returned **0** (a same-day path-sourced rmeta returned 1). The
staged tarball itself was correct: index checksum `769fa7f8…` matched `sha256sum` of the `.crate`,
and the extracted `src/ids.rs` was byte-identical to the working tree (`md5 621767df…`). So only
the *compiled* unit was stale, not the source or the tarball.

**Fix.** Deleted the 17 registry-sourced `imbh_*` artifacts in `target/debug/deps` (identified by
dep-info files referencing `~/.cargo/registry/src/`) plus `target/debug/.fingerprint/imbh-*`, then
re-ran `cargo package --workspace` → exit 0, all members verified. **No source file changed.**

**Prevention.** Documented as a new `QUALITY_GATE.md` §3c (packaging dry-run + the diagnosis
recipe) and folded into the README "Releasing" sequence: clear
`target/package target/debug/.fingerprint/imbh-*` before `cargo package --workspace`. Worth
remembering generally — any verify-only compile error that contradicts a green
`cargo build --workspace` should be checked against the linked rmeta before it is treated as a
source bug.

## 2026-07-24 — Post-v0.1.0 obsolete-sentence sweep

**Trigger.** v0.1.0 shipped (tag `v0.1.0`, all 12 publishable crates live on crates.io at 0.1.0;
`imbh-test-support` correctly absent). Docs written before the release still described the project
as unpublished, and several source doc comments still described the M0/M1 walking skeleton.

**Verification method.** The crates.io JSON API (`/api/v1/crates/...`) is blocked by its data-access
policy and returns an error body, not a 404 — useful reminder that it is *not* a publication oracle.
The sparse index (`https://index.crates.io/im/bh/<crate>`) works fine and is the right check;
it confirmed 0.1.0 for all 12 shipping crates.

**Fixed — release-state drift.**
- `README.md` §Status: "Pre-1.0, not yet published to crates.io" → v0.1.0 released, `cargo add imbh`
  works, API still pre-1.0.
- `AGENTS.md`: project phase now records the release; the Git Workflow rule said "no commits yet /
  do not make the initial commit", replaced with the released-`v0.1.0` state plus a
  do-not-run-`cargo release` rule. Added a note that the public API now needs semver discipline.
- `OVERVIEW.md` §13 status line: M0–M6 complete **and v0.1.0 released**.
- `QUALITY_GATE.md` §3a/§3b: both said the dev container lacked `cargo-deny` / `cargo-about` and
  that notices should be generated "before the v0.1 release". Both tools are installed
  (`~/.cargo/bin`), both run in CI, and `THIRD-PARTY-NOTICES.txt` was generated for v0.1.0.
- `LTM/footprint-and-feature-gating.md`: same stale "cargo-deny is not installed" claim.
- `LTM/project-meta-ci-docs-and-testing.md`: the publishing-preflight note (new-crate rate limit,
  "the first publish must be batched") now carries its outcome — the limit is spent.

**Fixed — stale milestone doc comments in source** (none affect behavior; all four contradicted
shipped code): `imbh-core/src/enums.rs` "M0 implements only `Table::Logs`" (all seven tables exist),
`imbh-storage/src/wal.rs` "M1 uses only logs" (all three signal tags are written and replayed),
`imbh/src/lib.rs` `DbBuilder` "WAL/retention/maintenance/promotion land in M1+" (all honored), and
`imbh-otlp/src/lib.rs` "M0 implements logs only".

**Fixed — a doc bug the release surfaced.** `docs/EMBEDDING.md` claimed "`Db` is `Clone` (an `Arc`
inside)". It is not: `open()`/`open_read_only()` return `Arc<Db>` and the typed-query namespaces
take `self: &Arc<Self>`. The identical claim had already been fixed in the README during an earlier
sweep — the EMBEDDING copy was the stale sibling. (LTM's own pitfall note predicted this: "when a
flagged sentence turns out accurate, check for a stale *sibling* saying the same thing worse
elsewhere.")

**CHANGELOG hazard, worth remembering.** The four retried `cargo release` runs each stamped the
changelog, leaving four `## [0.1.0]` headings and four identical link refs. That was cleaned up
concurrently during this session, but the cleanup also removed `## [Unreleased]` entirely — which
would abort the *next* release, since the `pre-release-replacements` in `crates/imbh/Cargo.toml`
match `(?m)^## \[Unreleased\]$` with `exactly = 1` (0 matches fails just as 3 did). Restored the
heading, repointed the compare link at `v0.1.0...HEAD`, and documented the invariant in the
changelog preamble and in LTM. **After any failed or retried release, check the changelog has
exactly one `## [Unreleased]` and one heading per version before re-running.**

**Gate.** `cargo fmt --all --check`, `cargo build --workspace`, `cargo clippy --workspace
--all-targets -- -D warnings`, `cargo test --workspace` — all clean, 0 test failures.

**Note on concurrency.** Git history was amended mid-session by the user (five commits squashed
back to a single amended `Initial.`), so an early `cat CHANGELOG.md` and a later `Read` of the same
file disagreed. Re-read every edit target after noticing the divergence rather than trusting the
earlier snapshot.

## 2026-07-24 — Clippy arm refactor (PR #1) and SHA-pinning the GitHub Actions

**Trigger.** A staged working-tree change in `crates/imbh-tui/src/lib.rs` (a clippy cleanup) needed
to be branched, committed, pushed, and turned into a PR. A follow-on request upgraded every GitHub
Action to its latest release and pinned each one to a commit SHA.

**Finding — the staged clippy "cleanup" was not behavior-preserving.** The exemplar → trace
drill-down arm in `handle_detail_key` originally matched on `app.nearest_exemplar_trace()` with a
`None` branch that only did `return None` — the question-mark-able pattern clippy flags. The staged
rewrite collapsed it correctly but placed `app.push_history()` *before* the fallible
`app.nearest_exemplar_trace()?`. With no exemplar in view, Enter would then push a history entry and
immediately return `None`, where the original arm was a true no-op. Fixed by hoisting the lookup:

```rust
KeyCode::Enter => {
    let trace_id = app.nearest_exemplar_trace()?;
    app.push_history();
    app.focus_trace_id = Some(trace_id);
    switch_screen(app, Screen::Traces, db.clone(), options.clone(), sender.clone());
}
```

**Generalizable rule.** When a `match` → `?` refactor collapses a `None` arm, the `?` must sit at or
before the first side effect the old `None` branch skipped. `?` returns from the *function*, not the
arm, so any statement hoisted above it silently gains an execution path it never had. Worth checking
on every `let x = expr?;` clippy rewrite that touches mutable state.

**Delivered.** Branch `fix/clippy-metric-detail-enter-arm`, one commit, PR
<https://github.com/moriyoshi/imbh/pull/1>. Gate before commit — `cargo fmt --all --check`,
`cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` — all clean.

**Environment finding — SSH push does not work here; use the `gh` token over HTTPS.** `origin` is
`git@github.com:moriyoshi/imbh.git` and pushing over SSH fails with `Permission denied (publickey)`
(no `ssh-agent`, no usable key). `gh` is authenticated with a `repo`-scoped token, so the working
one-off is a push that rewrites the URL and borrows gh's credential helper *without* mutating repo
config:

```
git -c url."https://github.com/".insteadOf="git@github.com:" \
    -c credential.helper='!gh auth git-credential' push -u origin <branch>
```

`gh auth setup-git` would make this permanent, but it edits global git config — left to the user.

**GitHub Actions upgraded and pinned** (`.github/workflows/{ci,release,soak}.yml`). Every `uses:` now
carries a 40-hex SHA plus a `# vX.Y.Z` comment.

| Action | Was | Now | SHA |
| --- | --- | --- | --- |
| `actions/checkout` | `@v4` | v7.0.1 | `3d3c42e5aac5ba805825da76410c181273ba90b1` |
| `actions/upload-artifact` | `@v4` | v7.0.1 | `043fb46d1a93c77aae656e7c1c64a875d1fc6a0a` |
| `Swatinem/rust-cache` | `@v2` | v2.9.1 | `c19371144df3bb44fab255c43d04cbc2ab54d1c4` |
| `taiki-e/install-action` | `@v2` | v2.85.0 | `7572810d7dd469b651bb7793945692cf78da5dd7` |
| `dtolnay/rust-toolchain` | `@stable` | `stable` head | `4cda84d5c5c54efe2404f9d843567869ab1699d4` |

**Finding — `dtolnay/rust-toolchain` has no usable version tag to pin.** Its `v1` tag is 12 commits
behind the `stable` branch, and on `v1` the `toolchain` input is `required: true` with no default —
pinning that tag would have broken all six call sites. The per-toolchain *branches* (`stable`,
`nightly`, `1.x`) are the real interface, and each branch's `action.yml` sets the matching
`toolchain` default. So the pin is the `stable` branch head, with an explicit `toolchain: stable`
added to every step: once the ref is an opaque SHA, nothing else records which toolchain is
installed. This freezes the *action*, not the toolchain — it still resolves `stable` at run time.

**Finding — two three-major jumps, both benign for this repo.** `checkout` v7 blocks fork-PR checkout
under `pull_request_target` / `workflow_run`; these workflows only trigger on `push` and
`pull_request`, so they are unaffected (v4.4.0/v5.1.0/v6.1.0 also backported the
`allow-unsafe-pr-checkout` breaking change, so staying on v4 was not an escape). `upload-artifact`
v6/v7 moved to Node 24 + ESM and require Actions Runner ≥ 2.327.1, satisfied by GitHub-hosted
`ubuntu-latest`; the inputs in use (`name`, `path`, `if-no-files-found`) are unchanged.

**Verification.** All three workflows re-parsed with `yaml.safe_load`, and every pinned SHA was
confirmed to resolve via `gh api repos/<repo>/commits/<sha>` (guards against a typo'd pin, which
fails only at run time). Note that `Swatinem/rust-cache`'s release tag is an *annotated* tag, so
`git/ref/tags/<tag>` yields a tag object — it must be dereferenced through `git/tags/<sha>` to get
the commit. The other four are lightweight tags pointing straight at commits.

**Open follow-up.** SHA pins do not float, so patch updates stop arriving silently. There is no
`.github/dependabot.yml`; a `package-ecosystem: github-actions` entry would keep the pins refreshed
by PR. Not added — offered to the user. The workflow edits are also still uncommitted (deliberately
kept off the PR #1 branch, since they are an unrelated concern).

## 2026-07-28 — Issue #3: on-disk `Db::open` was broken on Windows (directory fsync)

**Symptom.** Every on-disk open on `windows/amd64` failed with `storage error: WAL dir fsync: Access
is denied. (os error 5)`; in-memory DBs were fine. Found by a native Windows CI gate in the `imbh-go`
binding — the triple builds and the whole suite passes there except the five tests that need a
durable DB, all failing at open.

**Cause.** `fsync_dir` opens the *directory* as a `File` to fsync it. That is a Unix idiom: on Windows
`File::open` on a directory returns `ERROR_ACCESS_DENIED` without `FILE_FLAG_BACKUP_SEMANTICS`. The
first call site is `Wal::open_with_rotate`'s fresh-DB branch (creating `wal.00000001.log`), so no
on-disk DB could be created at all.

**Fix.** `fsync_dir` is now `#[cfg(not(windows))]` with an `Ok(())` `#[cfg(windows)]` counterpart.

**The justification has one solid leg and one soft one — worth keeping straight.** Solid: Win32 has no
directory-fsync primitive at all. `FlushFileBuffers` takes a file handle; the only volume-wide flush
(`\\.\C:`) needs administrator rights and flushes the whole volume cache, so it is unavailable to a
library. The choice is therefore no-op vs. no on-disk DB on Windows, not no-op vs. a correct
implementation. Soft: the claim that NTFS's metadata journal makes the directory sync *unnecessary* —
that the create/rename log record is flushed in write-ahead order along with the file's own
`FlushFileBuffers`. That is the prevailing understanding of NTFS and is why RocksDB's
`WinDirectory::Fsync` is a literal `return Status::OK()` and SQLite's Windows VFS has no
directory-sync path, but neither the issue author nor this change measured it. Note journaling
guarantees filesystem *consistency* after a crash, which is not the same claim as *durability at the
time of the call*; the `$LogFile` is itself written lazily. Likewise, that `FlushFileBuffers` rejects
a directory handle opened with `FILE_FLAG_BACKUP_SEMANTICS` is received guidance here, not measured —
though it does not change the outcome, since the volume-flush point already rules the flag route out.

**Residual risk, and it is not uniform across the call sites.** Exposure is hard power loss only (a
process crash is unaffected — the OS page cache still holds the entry). The WAL segment create is the
mild case: a lost entry costs recently acknowledged writes, and for a fresh DB the result is
indistinguishable from the state before the create. The renames are the sharp case — a durable
manifest edit pointing at a seal-path segment whose temp→final rename did not survive is a dangling
reference rather than merely lost data. It degrades better than it sounds (the manifest edit rides the
same journal, and the reader already tolerates torn frames), but it is what to test first given a
Windows host.

**Finding — the issue named one `fsync_dir`; there are two.** `wal.rs:315` (WAL segment create +
rotation, errors as `WalPhase::DirFsync`) and `lib.rs:2448` `pub(crate) fn fsync_dir` (the seal path's
Parquet temp→final rename in `lib.rs:1196`, and the manifest `CURRENT` swap in `manifest.rs:338`,
erroring as `storage_io`). CI only ever reached the first because it fires during open. Both are
fixed; fixing only the reported one would have moved the failure to the first seal.

**Durability contract is unchanged on Windows.** Only the directory-entry ordering step is dropped.
Every file-content sync still runs: the WAL's `sync_data` under the `WalMode` policy, the Parquet
segment's `sync_all` before its rename, and `CURRENT`'s temp `sync_all`. Written up as a new
"Directory fsync (platform note)" bullet in ARCHITECTURE.md §7.

**Finding — cross-compiling to verify locally is blocked by `zstd-sys`, not by the code.** The
`x86_64-pc-windows-gnu` target is installed, but `cargo check --target x86_64-pc-windows-gnu -p
imbh-storage` dies in `zstd-sys`'s build script for want of `x86_64-w64-mingw32-gcc` (parquet's zstd
compression is a C dep). So the Windows arms were verified with a standalone `rustc --target
x86_64-pc-windows-gnu --emit=metadata` probe of the cfg pair instead. That probe also caught the real
compile hazard in this shape of change: an import used *only* by the `not(windows)` arm becomes an
unused-import error under the workspace's `-D warnings`. Not an issue here (`wal.rs` keeps `File` live
via the `Wal::file` field, `lib.rs` fully-qualifies `std::fs::File`), but it is what to check first if
another `#[cfg(windows)]` split is added.

**CI — a `windows-latest` job was added to `ci.yml`.** Every other job is `ubuntu-latest`, which is
why a bug that broke *all* on-disk opens shipped in 0.1.0. The new job is deliberately narrow rather
than a second full gate: `cargo build --workspace`, then the two suites that actually touch the
filesystem — `cargo test -p imbh-storage` (WAL create/rotate, manifest `CURRENT` swap, seal → rename)
and `cargo test -p imbh --test lifecycle` (on-disk open → seal → reopen → compact). This is the
regression gate for #3; no new Rust test is meaningful, since on Linux the bug is unreachable and on
Windows every existing on-disk test failed.

**Open follow-ups.** (1) The Windows job has never run — it is unverified locally for the reason
above, and may surface further Windows-specific issues beyond this one (file locking, deletion of
open/mapped files during compaction and retention are the plausible next candidates). (2) The fix is
in `imbh-storage`, published at 0.1.0 with the bug; users on Windows need a release, which is the
user's call to cut.

## 2026-07-30 — Docker logging-driver plugin (`imbh-server`, `docker` feature)

**What.** `imbhd` can now run as a Docker `docker.logdriver/1.0` plugin: `docker run --log-driver
imbh` writes a container's stdout/stderr into the embedded `Db`, and `docker logs` is served back out
of it. New optional, off-by-default `docker` feature; Unix only. Code in
`crates/imbh-server/src/docker/` (`mod.rs` protocol + FIFO readers, `entry.rs` wire format, `json.rs`
protocol JSON, `ingest.rs` container→OTLP + batching worker, `readlogs.rs` `docker logs`), packaging
in `crates/imbh-server/docker-plugin/`, operator guide in `docs/DOCKER_LOG_DRIVER.md`, design note in
ARCHITECTURE.md §10.16.

**Zero added crates — checked, not assumed.** `cargo tree -e no-dev -p imbh-server [--features
docker] --prefix none | sort -u | wc -l` gives **294 either way**; the feature's two deps show up as
new *direct* edges (`prost`, `opentelemetry-proto`) but both resolve to nodes already in the graph.
Two things made that possible: prost and
opentelemetry-proto's message types are already in the default graph via `imbh-otlp`, and Docker's
`logdriver.LogEntry` schema is five frozen fields, so it is declared with prost's derive instead of
generated from a `.proto` (no `build.rs`, no protox in this crate). Protocol JSON goes through
`imbh::parse_json` — the dependency-free core parser — rather than serde_json. Binary cost, measured
at the shipping release profile by rebuilding both configurations back to back: **33,736,592 →
33,802,128 bytes — +64 KiB** for the whole feature (32.17 → 32.24 MiB). Crate count is unchanged, so
the footprint gate's 275-crate budget is untouched. *Measurement caveat worth remembering:* the gate
script reuses `target/release/imbhd` if it exists rather than rebuilding, so a first run after a
source change reports a stale size — the pre-rebuild run here printed 33,539,984 bytes and would have
made the feature look like +256 KiB. Rebuild explicitly before trusting a size delta. This is the same
containment discipline §11 asks for, applied to a feature that could easily have pulled a web
framework and a date-time crate.

**Finding — the daemon opens the FIFO `O_RDWR` *before* calling `StartLogging`.** moby's
`openPluginStream` does `fifo.OpenFifo(ctx, path, unix.O_RDWR|unix.O_CREAT|unix.O_NONBLOCK, 0700)` and
only then calls the plugin, with the comment "Make sure to also open with read ... to avoid borking
the fifo". Consequence for a Rust plugin: the plugin's `File::open` (O_RDONLY) returns immediately
rather than blocking for a writer, so opening inline is safe — but only because of the daemon's
ordering, which is not part of the documented contract. The implementation does not rely on it: the
open happens on the reader thread and `StartLogging` waits on a channel with a 2 s cap
(`OPEN_TIMEOUT`), so a genuine failure (unopenable path) is still reported in the response body while
an unexpectedly blocking open delays one container's start instead of wedging the daemon.

**Finding — Docker's `line` includes the trailing newline** (this is why `*-json.log` entries read
`"log":"hello\n"`). Storing it verbatim would put a `\n` on the end of every `body`, which is wrong
for `SELECT body`, for `matches()` tokenization, and for anything reading the DTO. Ingest strips one
trailing terminator (`\n` or `\r\n`); `ReadLogs` appends one. Round-trip through `docker logs` is
unchanged; the only lossy case is a final line that never had a newline, which gains one.

**Finding — two spellings of the `ReadLogs` config object.** Docker's published plugin documentation
shows `{"ReadConfig": {...}, "Info": {...}}`; the Go proxy struct marshals the field as `Config`.
Rather than pick one and risk serving *unfiltered* logs to a daemon that used the other, the parser
accepts `Config` with `ReadConfig` as a fallback. `Tail` follows Go's convention: negative = all
history, 0 = none, positive = last N.

**Two daemon dialects for split lines.** Lines over ~16 KiB arrive as multiple frames. Modern daemons
tag every chunk with `partial_log_metadata{id, ordinal, last}`; older ones set `partial = true` on all
but the final chunk. `PartialAssembler` handles both with one state machine (key by metadata id,
empty key for the legacy dialect), caps a reassembled line at 1 MiB, and drains anything unterminated
at end of stream so a container's last line is not lost.

**Back-pressure, not loss.** The FIFO readers feed one batching ingest worker (512 records or 200 ms)
over a bounded channel. On a full queue the reader *blocks* rather than dropping: back-pressure
propagates into the container's stdout pipe, which is the behavior an operator wants from a log
driver. Batching is what keeps this cheap — one WAL append per batch, not per line.

**`ReadLogs` uses the typed `LogQuery` API, not hand-written SQL**, filtering on the `container.id`
*resource* attribute via `LogStringField::ResourceAttribute` + `StringPredicate::Eq`. Container
identity belongs on the resource per OTel semconv, and `attr_eq` only reaches record attributes — the
`string_predicate` surface is what makes resource attributes queryable. History is streamed in
1000-row pages against a time window fixed before the first query, so forward paging cannot be shifted
by rows arriving mid-scan.

**Follow mode termination.** `docker logs -f` polls every 200 ms for records strictly newer than the
last one written. It exits when the container's streams are gone (`is_active` over the FIFO registry)
and five consecutive polls came back empty — otherwise `-f` on a stopped container would hang until
the client disconnected. Timestamp-based advancement means two records sharing one nanosecond would be
reported once; Docker's timestamps are wall-clock ns, so this does not occur in practice. Written up
as a known gap in §10.16.

**Refactor in `lib.rs`.** `read_request` became generic over `BufRead` and `write_response` generic
over `Write`, so the plugin's `AF_UNIX` endpoint reuses the same HTTP/1.1 parser as the TCP server
instead of growing a second one. `json_string` is now `pub(crate)`. No behavior change to the existing
routes.

**Tests.** 36 unit tests across the five modules (framing round-trip, truncated/oversized frames, both
partial dialects, interleaved split ids, resource mapping, log-opt parsing, newline handling, RFC 3339
including Go's zero time and numeric offsets, every protocol endpoint under malformed JSON) plus a
5-case E2E over a **real Unix socket**: handshake, a container stream ingested and asserted *in the
DB*, `ReadLogs` history/`--tail`/`--since`, follow mode delivering a live line and terminating on
stop, and a genuine `mkfifo` FIFO exercising the blocking-open interlock (skipped where `mkfifo` is
unavailable). The E2E also runs the two example queries from `docs/DOCKER_LOG_DRIVER.md`, so the
documented SQL cannot rot. `docker` is off by default, so `ci.yml`'s gate job now lints and tests it
explicitly — otherwise none of this would compile in CI.

**Not implemented (deliberate).** Docker's `labels-regex`/`env-regex` log-opts (a regex engine for two
options is not worth the footprint — name the keys) and `tag` (Docker's `{{.Name}}` template
language). Labels and env are copied only when explicitly named, so a container's environment — which
usually holds secrets — is never swept into the database wholesale.

## 2026-07-30 — Docker plugin networking: measured, not assumed

Follow-up to the log-driver entry above, prompted by "how do containers send traces/metrics to the
plugin, without exposing it outside the machine?". Everything below was measured against the local
daemon (Docker **29.2.1**, native Linux), not inferred.

**Finding — `network.type: bridge` is accepted but unimplemented for managed plugins.** A probe
plugin that dumped its own interfaces to a bind-mounted file, enabled once per setting:

| `network.type` | what the plugin process sees |
|----------------|------------------------------|
| `host` | `lo`, the host LAN interface, `docker0` 172.17.0.1, every `br-*`, a default route |
| `bridge` | **`lo` only — no addresses, no routes, no veth** |

The value round-trips through `docker plugin inspect` (`{"Type":"bridge"}`), so it looks supported;
moby just drops the plugin into an empty netns. `bridge` is therefore a synonym for `none`, *not*
"give the plugin an IP on docker0". It does not break the log driver — the plugin socket and the
per-container FIFOs are filesystem objects — but it makes the OTLP/query endpoint reachable by
nothing. Recorded in §10.16 and in the operator guide, because the setting's name actively misleads.

**Finding — `host-gateway` resolves to the *daemon's* `host-gateway-ip`, not the per-network
gateway.** A container on a user-defined network whose gateway was `172.23.0.1` still got
`172.17.0.1  host.docker.internal` in `/etc/hosts`. That is what makes a single bind address workable:
binding docker0's address serves containers on *every* bridge network, including compose-created
ones, with one endpoint value.

**So the shipping posture is `host` netns + bind the bridge gateway.** Verified end to end: bound to
`172.17.0.1:<port>` only (`ss -ltn`), REACHABLE from a default-bridge container and from a
user-defined-network container, and refused from the host's own LAN address (192.168.10.131). "Not
outside the computer" holds; "not reachable by other containers on the box" does not, and `/admin/*`
is unauthenticated — stated as a caveat rather than papered over.

**`docker plugin set` verified for the three cases that matter**: setting a `settable` env var works;
setting it to the **empty string** works (this is the no-TCP posture); and a var declared
`"settable": null` is refused by the daemon (`"IMBH_DOCKER_PLUGIN_SOCKET" is not settable`) — so the
socket path genuinely cannot be repointed at runtime, which is what that declaration is for.

**Design consequence — listen addresses had to become environment variables.** A managed plugin's
`entrypoint` args are frozen in `config.json`; only `env` is settable. So `IMBH_LISTEN_ADDR` and
`IMBH_GRPC_LISTEN_ADDR` now back the positional args (arg > env > default), with **empty meaning "do
not listen"**. That forced `main` to stop treating one server as the foreground: every configured
endpoint now runs on its own thread and `main` parks on all of them, which is what makes HTTP, gRPC,
and the plugin socket independently optional. `listen_addr()` is a pure function in `lib.rs` with
tests for precedence, emptiness, and trimming (a `docker plugin set` value arrives verbatim, and a
stray space would fail to parse at bind time and take the server down).

**gRPC belongs in the plugin build.** OTLP/gRPC on 4317 is the default transport for most OTel SDKs,
so a plugin without it fails for anyone who does not know to say `http/protobuf`. The plugin
Dockerfile now builds `--features docker,grpc,tracing`. Related trap worth remembering: `imbhd` does
not decompress request bodies, and the **OTel Collector's** OTLP exporters default to
`compression: gzip` (SDKs default to none) — a collector in front of imbh needs `compression: none`.

**Doc bug found and fixed: there is no `docker plugin logs` command.** The first draft of the guide
told operators to use it. Plugin stdout/stderr is captured by the Docker daemon's log
(`journalctl -u docker`). Checked against `docker plugin --help`, which lists create/disable/enable/
inspect/install/ls/push/rm/set/upgrade and nothing else.

**`build.sh` now asks the daemon instead of hard-coding.** `docker network inspect bridge --format
'{{range .IPAM.Config}}{{.Gateway}}{{end}}'` is the authority on its own bridge address (a daemon with
a custom `bip` differs), and the result is applied with `docker plugin set` after create. `IMBH_BIND`
overrides it; `IMBH_BIND=none` disables both listeners. The engine itself is the `DOCKER` variable,
expanded **unquoted** on purpose so `DOCKER="sudo docker"` or `DOCKER=podman` works.

## 2026-07-30 — Docker log driver: runtime verification, delivery state, conventions

Closes out the two entries above (the plugin itself, and the networking measurements). Those record
the design and the findings; this records what was actually *run*, what shipped, and one convention
adopted along the way. No new design content.

**Runtime verification of the reworked `main`.** Making the listeners individually optional changed
process lifetime management, which no unit test covers — `listen_addr()` is pure, but "does the
process stay alive with no TCP listener" is not. Exercised the built binary directly:

| Configuration | Result |
|---------------|--------|
| both listeners empty + `IMBH_DOCKER_PLUGIN_SOCKET` set | process alive, socket created, **0 TCP ports** (`ss -ltnp` by pid) |
| `IMBH_LISTEN_ADDR` from the environment only | serving; `GET /health` → 200 |
| both listeners empty, no plugin socket | refuses to start, exits **1**, "nothing to serve: …" |

The third case is the one worth keeping: an `imbhd` that starts, serves nothing, and sits there is
strictly worse than one that fails loudly, and the empty-listener feature made that state reachable
for the first time.

**Footprint after the restructure: unchanged.** 275 crates, and the release `imbhd` came out at
33,736,592 bytes — byte-identical to before the change. Moving the accept loops onto threads and
adding the env plumbing cost nothing measurable.

**Convention — brace-form shell variable references.** `crates/imbh-server/docker-plugin/build.sh`
uses `${VAR}` everywhere, not bare `$VAR`, on the user's instruction. Braces keep the name boundary
explicit (`"${BIND}:4318"`, `"${HERE}/config.json"`) and stay compatible with the deliberate
unquoted `${DOCKER}` expansion that lets `DOCKER="sudo docker"` split into arguments. Audit with
`grep -nE '\$[A-Za-z_][A-Za-z0-9_]*' file.sh | grep -v '\${'` — it should print nothing. Worth
applying to any new script in this repo; the pre-existing `scripts/*.sh` were left alone.

**Delivered on `feat/docker-log-driver`, PR #6**, as a single SSH-signed commit (verified on GitHub).
It began as three — the plugin, the listen-address rework, then the fixes from real-daemon
verification — squashed at the user's request before merge.

*No SHA is cited here on purpose.* This entry lives inside the commit it would name, so every hash
written into it is invalidated by the act of committing it: a first draft said `36c02c3`, which an
amend immediately turned into a dangling reference, and the later squash would have broken a second
one. The rule that survives: cite a branch's *parent* or a merged commit by hash if you need one, and
cite the commit you are writing inside by subject. Local gate clean throughout: `fmt` /
`build --workspace` / `clippy --workspace --all-targets` / `clippy -p imbh-server --all-features` /
`test --workspace` (52 suites) / `test -p imbh-server --features docker` / `scripts/footprint-gate.sh`.

*Local signature verification is misleading here:* `git log --format=%G?` prints `N` and errors with
"gpg.ssh.allowedSignersFile needs to be configured", because this repo has `gpg.format = ssh` and a
`user.signingkey` but no allowed-signers file. The commits **are** signed — `git cat-file -p HEAD`
shows the `gpgsig` SSH block and the GitHub API reports `verified: true, reason: valid`. Do not read
the local `N` as unsigned.

**Still open** (tracked in `TODO.md`, unchanged): none of this has been driven by a real `dockerd`,
and the rootfs image under `crates/imbh-server/docker-plugin/` has never been built or
`plugin create`d. Every Docker-side fact in these three entries came from probing the local daemon
(29.2.1) with throwaway plugins and containers, not from running imbh itself as a plugin. The
alpine/musl build of `imbhd` (zstd-sys wants a C toolchain) is the most likely thing to break first.

## 2026-07-30 — Docker log driver verified end to end against a real daemon (2 defects found)

The TODO item "never driven by a real `dockerd`" is now closed. Built the plugin with the shipped
`build.sh`, registered it, ran containers through it, and tore it all down again on Docker **29.2.1**
(native Linux, x86_64). Two real defects surfaced — neither reachable by the hermetic suite.

**Defect 1 — no `.dockerignore`: the plugin was unbuildable in a working checkout.** The Dockerfile
builds from the repo root (it must: compiling `imbhd` needs every workspace member manifest, not just
`crates/imbh-server`), so `COPY . .` ingested `target/` (**557 GB**) and `.agents-workspace/` (58 GB).
Docker hashes and transfers the whole context *before* running the first instruction, so the build
produced no output and looked like a hang rather than an error — the worst failure shape. Added a root
`.dockerignore` (`target/`, `**/target/`, `.agents-workspace/`, `.git/`, `.github/`, `*.rs.bk`):
**614 GB → 4.81 MB of context, transferred in 0.0s.** Checked first that no crate does
`include_str!`/`include_bytes!` from `assets/` before deciding what to exclude. Generalizable lesson:
any Dockerfile whose context is a Rust workspace root needs this, and its absence presents as a hang.

**Defect 2 — `docker logs -f` silently dropped the first line.** Against the real daemon, a container
printing `tick 1..5` gave 4 of 5 lines to `docker logs -f`; all 5 were queryable in the DB. Root
cause in `readlogs.rs`: when the history query returned nothing, the follow watermark fell back to
`history_end` (= `Timestamp::now()`), so the loop then asked only for records *strictly newer than
that instant*. But a record's timestamp is when the **container emitted** the line, while ingest lands
it up to one batch interval (200 ms) later — so any line already emitted and not yet stored was
skipped **permanently**. `docker logs -f` on a freshly started container is exactly that race, which
is why it cost the first line every time. Fix: the watermark is `Option<Timestamp>`; `None` (nothing
written yet) keeps the request's *lower* bound rather than jumping to now. `--tail 0` still jumps to
the present, because "only new lines" is its defined semantic, not a race.

*The general shape is worth remembering:* **event time and ingest time are different clocks, and a
tail must never use wall-clock time as a watermark over an event-time column.** Any batching ingest
path has this hazard.

Regression test `follow_delivers_a_line_timestamped_before_the_follow_began` reproduces it without
timing luck — open the follow against an empty DB, *then* start a stream whose record carries an older
timestamp. Verified it fails on the pre-fix code (read timeout, no frame) and passes after. Re-verified
against the real daemon: 5/5 lines with `tick 1` first, three rounds running.

**What passed, on the daemon** (15 checks, plus 6 more in a second pass): plugin `create`/`set`/
`enable`; a container's stdout and stderr stored with the right severities; a 20 000-character line
reassembled into **one** row, not five; `container.name` and a `--log-opt labels=` selection on the
resource; `matches()` full-text search over container output; `docker logs` with `--tail` and
`--since`; `docker logs -f`; OTLP/HTTP **and** OTLP/gRPC listening on the bridge gateway and reachable
from a container via `host-gateway`; **not** reachable on the host's LAN address; and all rows
surviving a plugin `disable`/`enable` (bind mount + WAL replay).

**The shipped Dockerfile builds clean on musl** — `zstd-sys`, tantivy, DataFusion and tonic all
compiled, 7m06s for the release profile (`lto = "fat"`, `codegen-units = 1`) on 20 cores. The
toolchain risk this TODO flagged was not the problem; the build context was.

**`build.sh`'s daemon interrogation works**: it printed `binding OTLP to 172.17.0.1 (this daemon's
bridge gateway)` and applied it via `docker plugin set`, exactly as designed.

Everything created was removed afterwards: plugin, rootfs image, containers, and the root-owned data
directories (deleted from a throwaway container, since the plugin runs as root).

---

## 2026-07-30 — imbh-tui: a trace detail screen (scrollable waterfall + span drill-down)

**The problem, as reported:** on the Traces screen the span waterfall lives in a fixed slice of the
results area (a 55/45 vertical split) and does not scroll, so any trace deeper than ~10 spans is
partly invisible with no way to reach the rest.

**What landed.** Two new full-content routes on the Traces screen, following the existing non-modal
detail pattern (`Route::MetricDetail` / `Route::LogDetail`) exactly:

* `Route::TraceDetail { detail: TraceDetail }` — the whole trace: a header (trace id, span count,
  duration, start; root service/operation in the block title), the complete waterfall rendered as a
  ratatui `List` with a span cursor, and (when the terminal is at least 18 content rows tall) a
  five-line summary of the span under the cursor. Because the waterfall is a `List` with a selected
  index, the widget scrolls it — `↑`/`↓`/PageUp/PageDown/Home/End walk all spans at any terminal
  size, which is the actual fix for the report. Non-OK spans render red.
* `Route::SpanDetail { trace_id, span }` — every stored field of one span (ids, parent, service,
  kind, status + message, absolute start, offset into the trace, duration, the malformed-parent
  note, the three attribute maps, and the raw events/links JSON), scrolled like the log detail.

**Navigation.** `Enter` on the Traces list opens the trace detail; `Enter` on a waterfall row opens
the span detail; `L` from either opens the Logs screen correlated by **trace id *and* span id** —
which closes the "per-span waterfall cursor is the remaining piece" gap left by the 2026-07-23
trace→log drill-down (that one was trace-granular). `←`/Esc/`→` history, `F9`, `1`-`4` and `t` keep
working from inside both views, as for the other detail routes.

**No second fetch.** The Traces list already materializes the selected trace to draw its preview
pane (`request_waterfall` → `traces().get()`). That fetch now returns the structured trace alongside
the pane (`build_trace_detail`, which emits the width-independent `Waterfall` rows *and* an aligned
`Vec<SpanRecord>` in one pass), and the app retains it (`App::trace_detail`). So Enter is a pure
in-memory navigation. The retained trace is dropped the moment the row cursor moves to another trace,
so Enter can never open a detail for a trace the cursor has left. When Enter beats the in-flight
fetch, the intent is remembered (`pending_trace_open`) and the view opens when the result lands —
cleared by any navigation or by a newer fetch, so a late waterfall never yanks the user somewhere.

**The preview pane no longer lies.** It still does not scroll (it is a preview), but when a trace has
more spans than the pane's rows it now titles itself `Waterfall: N of M spans — enter: all` instead
of silently cutting the tail. Silent truncation was the real defect; the full view is one key away.

**Verification.** 95 unit tests in `imbh-tui` (8 new: record/row alignment including the orphan
`!` marker, the open/no-op/pending paths, span cursor clamping + Back restoring it, the span-granular
correlation from both routes, the field lines, duration-unit scaling, and a `TestBackend` render
asserting span 40/40 is on screen at 80×14 — the regression for "the pane doesn't scroll"). Both new
routes joined the `--ascii` render sweep (the µs unit spells `us` there). Full workspace gate green.
Also driven end-to-end against a `gen-demo-db` database in a pty: list → Enter → trace detail → span
cursor → Enter → span detail → `L` → `Logs for trace 18c6edde span e449b973`, then Esc back.

**No dependency change**, so the footprint gate is untouched (`ratatui`'s `List`, `attrs_to_pairs`,
and `push_attr_section` were all already in the graph).

## 2026-07-30 — Flush scheduler (`FlushPolicy`), and `imbhd` never sealed

**The report.** "imbhd never tries to flush the WALs." Confirmed, and worse than the wording
suggests. `crates/imbh-server/src/main.rs` opened the DB with `Db::builder(&dir).open()` — the
library default is `Maintenance::Manual`, which spawns no scheduler at all. So a running `imbhd`:

1. never sealed, so no row ever reached Parquet and the mutable buffer grew for the process's life;
2. never reclaimed the WAL (reclaim happens *after* a seal advances the watermark), so the WAL grew
   with it;
3. never fsynced, because `WalMode::Interval(1s)` — the default — had **no timer anywhere in the
   tree**. The M1 note in `config.rs` said "fsyncs opportunistically on `flush`/`close`; a
   timer-driven flusher is a follow-up", and that follow-up had never landed.

Only `POST /admin/flush` or shutdown broke the cycle. The library behavior was correct-by-design
(embedders get "no background threads unless opted in"); the defect was that the *collector process*,
the one host that should opt in, never did.

**The design: two orthogonal knobs.** `Maintenance` already existed and answers *who runs the loop*
(Manual / owned thread / host runtime) — leave it alone. The new `FlushPolicy` (imbh-core `config`)
answers *when it seals*, with triggers that OR together: periodic `every(d)`, buffered heap
`at_buffer_bytes(n)` (default `FlushSize::Budget` = today's `seal_threshold_bytes`), buffered rows
`at_buffer_rows(n)`, on-disk WAL size `at_wal_bytes(n)`, and `after_idle(d)`. `tick(d)` sets the
evaluation cadence, `manual()` disables all of them.

Deliberately *not* an added variant on `Maintenance`: those crates are published, and a new variant
on a public enum breaks downstream exhaustive matches. A new struct plus a new builder method is
purely additive.

**Backward compatibility hinges on one line.** `resolve_flush` maps "host set no policy" to
`FlushPolicy::default().or_interval(maintenance_interval)`, so an existing
`Maintenance::Background(30s)` user keeps sealing every 30s at the byte threshold, exactly as before.
An *explicit* `FlushPolicy::manual()` never gets that interval grafted on — otherwise "manual" would
silently mean "every 30s". That distinction is the reason the builder field is `Option<FlushPolicy>`
rather than a `FlushPolicy` with a manual default.

**Two things the loop must not do.** (a) The policy tick is clamped to [5ms, 60s], but the loop
sleeps in slices of at most **1s** regardless, because `close()` joins it — a 60s tick would
otherwise make shutdown take up to a minute. The tick then throttles only the *measurement* (one
lock for `flush_gauges`, plus a directory scan for `wal_bytes`, which is why that one is measured
only when a WAL trigger is configured). (b) The idle trigger is guarded on `buffer_rows > 0`, or a
quiet DB would ask for a no-op seal every single tick forever.

**The idle clock starts at open, not at first ingest.** `Inner::last_append` is `None` until an
ingest, and replay deliberately does not set it; `flush_gauges` falls back to `Storage::opened_at`.
A DB reopened with a WAL-replayed buffer is therefore sealable one idle window after open, instead
of waiting for traffic that may never arrive.

**`WalMode::Interval` now means something.** `Storage::sync_wal()` fsyncs and advances `durable_lsn`
whatever the mode (`group_commit` is now a mode check plus a call to it), and
`wal_sync_interval()` reports `Some(d)` only for `Interval` — the scheduler calls it on that clock.
Consequence worth stating: interval durability is a *scheduler* property. A `Maintenance::Manual`
embedder still only fsyncs on `flush`/`close`, which the `WalMode` docs now say outright.

**`imbhd` defaults.** `IMBH_FLUSH` (default `interval=5s`) and `IMBH_MAINTENANCE_INTERVAL` (default
`60s`). Environment rather than argument, for the same reason as the listen addresses: a managed
Docker plugin's entrypoint args are frozen in `config.json` while `settable` env entries are not (both
new variables are declared settable in the plugin's `config.json`). `FlushPolicy: FromStr` parses the
spec (`interval=5s,buffer=16MiB,rows=50000,wal=64MiB,idle=2s,tick=250ms`, or `manual`), `Display`
renders it back in the same syntax for the startup banner, and unknown keys are **errors** — a typo in
a deployment's config must not silently leave the buffer unsealed. Empty means unset (so
`docker plugin set IMBH_FLUSH=` reads like never having set it).

**Verification.** Full workspace gate green. 8 new `imbh-core` config tests (trigger algebra, tick
clamping, spec parse/round-trip, rejected typos), 2 new `imbh-storage` tests (gauges across all six
table buffers + the idle clock; `sync_wal` vs `group_commit` under `Interval`), 7 new `imbh` tests
(one per strategy — size/periodic/rows/WAL/idle/manual — plus the interval-WAL fsync proving the
watermark advances with no seal), and 2 new `imbh-server` tests for the env resolution. The
WAL-trigger test asserts convergence (`wait_until`) rather than a single reading: the scheduler can
seal part-way through the ingest loop, which made a one-shot `wal_bytes < 4096` flake under a loaded
`--workspace` run.

**No dependency change**, so the footprint gate is untouched; the new code is std-only.

### Addendum — end-to-end verification and final numbers

Written after the entry above; the work it covers landed last.

**A socket-level regression for the reported defect.** `crates/imbh-server/tests/http_e2e.rs` gained
`ingested_rows_are_sealed_without_an_admin_flush`: it builds the DB the way `main` does
(`IMBH_FLUSH` → `imbh_server::flush_policy` → `DbBuilder::flush`, plus
`Maintenance::Background(maintenance_interval(None))`), serves it on a loopback port, POSTs one
OTLP/HTTP log, and asserts a segment appears **without any `/admin/flush`** — then reads the row back
through `POST /api/query`. The unit tests pin the *default* (`interval=5s`); this test overrides it to
`interval=100ms` so it stays fast. It is the test that would have failed before this change.

**Run against the real binary.** `IMBH_FLUSH=interval=1s ./target/debug/imbhd <dir> 127.0.0.1:14318`,
one hand-encoded `ExportLogsServiceRequest` POSTed to `/v1/logs`, then `GET /stats` three seconds
later: `buffer_bytes: 0`, `wal_bytes: 0`, `durable_lsn: 1`, one `logs/1970-01-01/*.parquet` on disk.
All three of the failure modes in the entry above (unsealed buffer, un-reclaimed WAL, no fsync) are
observably closed on the shipping wiring, not just in-process. The banner line it printed —
`flush:     interval=1s,buffer=budget,tick=1s  (retention every 60s)` — is `FlushPolicy`'s `Display`,
which is deliberately the same syntax `IMBH_FLUSH` parses, so an operator can copy it back out, change
one trigger, and know exactly what is running.

**Final gate.** `cargo fmt --all --check` clean, `cargo clippy --workspace --all-targets -D warnings`
clean, `cargo test --workspace` **383 passed / 0 failed** (19 new: 8 `imbh-core`, 2 `imbh-storage`,
7 `imbh`, 2 `imbh-server` unit + the e2e above). Two clippy lints were worth noting as house style
here: `manual_is_multiple_of` (use `n.is_multiple_of(m)`, not `n % m == 0`) and `collapsible_if`
(let-chains — `if let Some(d) = … && cond`), both of which the 1.96 toolchain enforces at
`-D warnings`.

**Files touched.** `imbh-core` (`config.rs` + re-exports), `imbh-storage` (gauges, `sync_wal`,
`opened_at`/`last_append`), `imbh` (builder field + setter, `FlushScheduler`, both maintenance loops,
re-exports), `imbh-server` (`flush_policy`/`maintenance_interval`, `main` wiring + banner,
`docker-plugin/config.json`), and the docs: ARCHITECTURE.md §5/§7/§10.2/§10.14/§10.15/§10.16,
README.md, `docs/EMBEDDING.md`, `docs/DOCKER_LOG_DRIVER.md`, CHANGELOG.md. Nothing committed — the
tree is left dirty for the user's review.

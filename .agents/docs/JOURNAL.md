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

## 2026-07-30 — CD: prebuilt binaries + a container image, and the glibc floor that nearly shipped a broken TUI

**Ask.** Prebuilt `imbh-server` binaries and a Docker image carrying that binary plus `imbh-tui`,
attached to GitHub releases. Until now `release.yml` was a release *gate* only (license check + notice
generation, uploading `THIRD-PARTY-NOTICES.txt` as a build artifact); there was no install path for
either binary except `cargo build`, and README.md had no "Install" section at all.

**Shape.** Extended `release.yml` rather than adding a second workflow, so the notices generated by the
existing `licenses` job are the ones packaged: `meta` (resolve version/tag/image once) → `licenses` →
`build` (5-leg matrix) → `publish` (Release assets) + `image` (GHCR). `build` depends on `licenses`
deliberately: no binary ships from a run whose license gate failed. `workflow_dispatch` with a
`dry_run` input (default true) rehearses the entire path and publishes nothing, which is the only way
to exercise five runner platforms without cutting a release.

Targets, chosen with the user: `x86_64`/`aarch64-unknown-linux-gnu`, `aarch64`/`x86_64-apple-darwin`,
`x86_64-pc-windows-msvc` — glibc rather than musl, at the user's direction. Everything builds
**natively** except `x86_64-apple-darwin` (cross-compiled on the arm64 macOS runner, since Apple's
clang is a native cross-compiler and Intel runner labels keep being retired). Native was worth
arranging: the only C in the graph is `zstd-sys` (vendored libzstd via parquet), and a native
toolchain is the one configuration needing no cross C setup. GitHub's arm64 Linux runners are free for
public repositories, so `ubuntu-22.04-arm` covers aarch64 with no `cross`/`zigbuild` layer.

**The finding: the glibc floor is real, and it hit exactly one of the two binaries.** The image copies
the release binaries onto `debian:bookworm-slim` (glibc 2.36) rather than compiling — a fat-LTO build
under QEMU for the arm64 leg would take hours. Testing the built image locally, `imbhd` served fine
(`/health`, `/api/query`, `/stats` all good, DB initialised on the volume by uid 10001), but
`docker exec … imbh-tui` died with:

```
imbh-tui: /lib/aarch64-linux-gnu/libc.so.6: version `GLIBC_2.39' not found (required by imbh-tui)
```

Measured on the host build (glibc 2.39): `imbhd` requires at most `GLIBC_2.34`, `imbh-tui` requires
`GLIBC_2.39`. So building the Linux legs on `ubuntu-24.04` would have published an image whose server
worked and whose explorer was dead on arrival — a failure visible only if someone ran the second
binary. Two consequences, both encoded rather than left as prose:

1. Linux legs build on **`ubuntu-22.04`** (glibc 2.35), under the bookworm base's 2.36.
2. A `Guard the glibc floor` step in the matrix reads the highest `GLIBC_x.y` symbol version each
   binary actually requires (`readelf --dyn-syms`) and fails the build if it exceeds 2.36. This is the
   guard, not the runner label — it also catches a runner-image bump under us, or a new dependency
   reaching for a newer symbol. Verified against the local binaries: it passes `imbhd` (2.34) and
   correctly rejects `imbh-tui` (2.39).

**No QEMU anywhere, verified.** `docker/Dockerfile`'s single `RUN` (useradd + the data dir) lives in a
stage pinned to `--platform=$BUILDPLATFORM`; the per-arch stages only `COPY`. Proved by building the
`linux/amd64` leg on this aarch64 host with the *default* builder, which has no emulators registered
at all — it succeeded, with `${TARGETARCH}` resolving correctly. The full two-platform manifest was
then built on a `docker-container` driver (what `setup-buildx-action` creates), and only one `prep`
stage ran, on the build platform.

A subtlety worth recording: `COPY --from=prep --chown=10001:10001 /staging/var/lib/imbh /var/lib/imbh`
carries **ownership but not mode** — `COPY <dir> <dir>` copies the source's *contents*, so the
destination is created at BuildKit's default 0755, not the `install -m 0750` used in `prep`. Ownership
is what actually matters (it is what lets the unprivileged user write the WAL, and what the declared
`VOLUME` inherits), so the `-m` was dropped rather than chased with a target-stage `chmod` that would
have reintroduced emulation.

**Second finding: `THIRD-PARTY-NOTICES.txt` did not cover what we distribute.** It was generated from
`crates/imbh-server/Cargo.toml` with *default* features, so it attributed none of the
tonic/hyper/h2/tower subtree the `grpc` feature links, nothing from `tracing-subscriber`, and nothing
of `imbh-tui`'s ratatui/crossterm/rand subtree — while README.md "License" promises those notices ship
with every binary distribution (Apache-2.0 §4(d)). Since CD ships `--features grpc,tracing` (+
`docker` on Linux) plus a second binary, this had to be fixed as part of the pipeline, not after it:
`gen-notices.sh` now runs `--workspace --all-features` over all six published targets in `about.toml`
(267 Apache-2.0 / 94 MIT crates, up from 210 / 59), and the file lands in every archive and at
`/usr/share/doc/imbh/` in the image. The scope is a deliberate **superset** of any single build —
over-attributing is safe, under-attributing is a licence breach — and the script says so, because the
tempting "optimisation" is to narrow it back to one binary's real graph.

Relatedly, `deny.toml`'s `[graph]` was host-only with `all-features = false`, so the license gate had
never seen the `grpc`/`docker` subtrees or any target-specific dependency. Now `all-features = true`
with all six targets listed: **still green**, but the previous configuration could not have told us
that. `htmlescape` (via tantivy) shows up as MPL-2.0 in `cargo deny list` and looked briefly like a
copyleft obligation — it is `Apache-2.0 / MIT / MPL-2.0`, a choice of three, and cargo-about takes it
under Apache-2.0. No obligation, and it was already in the default graph.

**Smoke tests that earn their place.** Each leg runs its own artifacts on the runner that built them
(all but the cross-compiled Intel-macOS leg, which would need Rosetta). `imbhd` with both listeners
disabled opens the database and *then* exits with "nothing to serve" — so the check exercises the real
on-disk path (`db.info`, `wal.00000001.log`, `writer.lock` are all created) before failing, which is
the code path that broke on Windows in issue #3. `imbh-tui` with no arguments prints its usage. Both
assertion strings were confirmed against the real binaries locally.

Also verified that `--features docker,grpc,tracing` compiles as a combination: `ci.yml` lints `docker`
and `grpc` separately and never together, so the shipping feature set had no coverage before.

**Files.** `.github/workflows/release.yml` (rewritten), new `docker/Dockerfile` +
`scripts/build-image.sh`, `scripts/gen-notices.sh`, `about.toml`, `about.hbs`, `deny.toml`,
regenerated `THIRD-PARTY-NOTICES.txt`, README.md (new "Install the binaries" section + "Releasing"),
CHANGELOG.md, QUALITY_GATE.md (new §4 distribution gate), TODO.md. No Rust source changed, so the §1
Rust gate is untouched by this work. Nothing committed — the tree is left dirty for the user's review.

## 2026-07-30 — CD review pass: three defects only a tag push would have surfaced

Continuation of the entry above. Once the pipeline was written, a systematic review of
`release.yml` — checking every `needs.*.outputs`, `steps.*.outputs`, and `matrix.*` reference against
what the referenced job, step, or leg actually declares — found three defects. They share the property
that makes them worth recording: YAML validation accepts all three, no local test reaches them, and the
only thing that exercises them is a real tag push, which is the one thing you cannot cheaply retry.

1. **`gh` had no repository context.** The `publish` job deliberately does not check out (it needs only
   the artifacts), so `gh release create` / `gh release upload` had no git remote to infer the
   repository from, and would have failed *after* all five builds succeeded. Fixed with
   `GH_REPO: ${{ github.repository }}` rather than by adding a checkout — cheaper, and it states the
   dependency instead of satisfying it by accident.

2. **`mktemp -d` in the Windows smoke test.** The step is `shell: bash`, so on `windows-latest` it runs
   under MSYS2, which rewrites POSIX-looking paths when passing arguments to native Windows binaries.
   An absolute `/tmp/...` database path handed to `imbhd.exe` is precisely the shape that translation
   mangles. Replaced with a workspace-relative `smoke-db`, which needs no translation. Windows is the
   least locally verifiable leg here, so removing an avoidable platform hazard from it is worth more
   than the tidiness of a temp directory.

3. **The glibc guard could abort instead of diagnosing.** Under `set -euo pipefail` a `grep` that
   matches nothing fails the pipeline, and because that pipeline sits in a command substitution being
   assigned to a variable, `set -e` kills the step. A binary with no versioned glibc symbols would
   therefore have produced a bare non-zero exit rather than an explanation. The pipeline is now wrapped
   in `{ ...; } || true` with the empty case reported explicitly as "the check is broken, not the
   binary" — the distinction that actually matters when a gate fires. Re-verified against four inputs:
   `imbhd` 2.34 accept, `imbh-tui` 2.39 reject, `/bin/sh` 2.38 reject, `/etc/hostname` empty and
   reported as a broken check.

Also removed a dead `arch:` key from the two Linux matrix legs. Nothing read it — the
Go-arch-to-Rust-triple mapping lives in the `image` job's staging step — and leaving it in implied a
data flow that does not exist, since a job's matrix is not visible to a downstream job.

**Ignore-file gap.** The `image` job stages its build context at `docker/ctx/`, which nothing ignored.
Two consequences, both now closed: a stray local copy of a few hundred MB of binaries could be
committed (`.gitignore`), and it would be swept into the *plugin* rootfs's build context, which builds
from the repository root (`.dockerignore`). That is the same failure mode the root `.dockerignore`
already documents at length for `target/`. `scripts/build-image.sh` sidesteps it by staging under
`target/image-ctx`, which the existing `/target/` rule covers.

**A correction worth recording, because it cuts against the obvious inference.** Mid-investigation I
concluded that a binary built against glibc 2.39 could not run on bookworm's 2.36, and that turned out
to be wrong as a general statement: `imbhd` ran fine there. A Rust binary only references the symbol
versions it actually uses, so the host's glibc is an upper bound, not the requirement. The requirement
is per-binary and has to be measured — which is exactly why the guard reads `readelf --dyn-syms` rather
than reasoning from the runner image. `imbh-tui` needing 2.39 while `imbhd` needs only 2.34, from the
same workspace and the same host, is the concrete demonstration.

**Verified here vs left to CI.** Verified on this host: the Dockerfile for both architectures including
the no-emulator cross build, the image running and serving over HTTP, the glibc guard against four
inputs, both smoke assertions against real binaries, `--features docker,grpc,tracing` compiling as a
combination, `cargo --locked` resolving, the full `cargo deny check`, and the notices regeneration. Not
verifiable without CI, and therefore the content of the TODO item asking for a `dry_run` dispatch before
the next release: `x86_64-apple-darwin` cross-compiling `zstd-sys` under Apple clang, `zstd-sys` under
MSVC, and whether the `ubuntu-22.04-arm` label is available to this repository.

**Files, additional to the entry above.** `.gitignore`, `.dockerignore`, `docs/DOCKER_LOG_DRIVER.md`
(disambiguating the published image from the logging plugin), and `.agents/docs/TODO.md` — four
follow-ups: the dependabot item's action count is now nine, the pipeline has never run, the log-driver
plugin is still local-build-only, and the footprint budgets remain unmeasured on the published targets.

## 2026-07-30 — CD caching: why the release build caches the registry and not the target dirs

Review feedback on PR #9: the pipeline did not appear to use the Actions cache, and the staging logic
duplicated `scripts/build-image.sh`. Both were fair; the caching answer turned out to be the opposite
of "add `type=gha` everywhere", so the reasoning is recorded here rather than only in a step comment.

**GitHub scopes Actions caches by ref, and tags do not share.** A run can restore a cache created in
the current ref or in the **default branch**, and explicitly *cannot* restore one created for a
different tag name. `release.yml`'s `build` job only ever runs on a tag or a dispatch, so there is
normally no `main`-scoped entry for its key to fall back to. The consequence is structural: **the first
run of a new tag is cold no matter what is cached**, and that is usually the only run a release gets.

Combine that with the 10 GB per-repository cache budget, evicted LRU and shared with `ci.yml`: five
legs of fat-LTO release `target/` directories would be multiple GB, would almost never be restored, and
would evict the caches that make every PR fast. Paying for a rare release by slowing every push is the
wrong trade. So the build job sets `cache-targets: "false"` (a real `Swatinem/rust-cache` input —
"only the cargo registry will be cached"). The registry cache is small, *does* hit, and still saves
fetching a ~275-crate graph on five runners.

One useful corollary: a `workflow_dispatch` rehearsal **from `main`** writes into main's scope, so the
dry run I already recommend for validation doubles as the thing that warms the registry cache for the
tag run after it. Worth knowing, because it makes the rehearsal strictly better than free.

**The Docker layer cache is bounded, and the comment says so.** Added `cache-from: type=gha` /
`cache-to: type=gha,mode=min`, with the honest caveat that the binary `COPY` layers change on every
build, so a *new* release never reuses them. It pays for re-runs of the same commit — a transient GHCR
push failure, or repeated dry runs. `mode=min` rather than `max` on purpose: the only extra thing `max`
would store is the `prep` stage's `useradd` layer, worth nothing, while the store competes with the
Rust caches for the same 10 GB.

**Staging is now defined once.** `scripts/build-image.sh` grew `--ctx DIR`, `--stage-only`, and a
repeatable `--prebuilt GOARCH=DIR`; the `image` job unpacks the two Linux archives and calls it instead
of open-coding the layout. `docker/Dockerfile`'s contract (`linux/<goarch>/{imbhd,imbh-tui}` plus
`LICENSE` and `THIRD-PARTY-NOTICES.txt` at the context root) therefore lives in exactly one place, and
a local build cannot drift from a release build in a way the Dockerfile would notice. The image build
itself stays in `docker/build-push-action`, which owns registry auth, the GHA cache backend, multi-arch,
and provenance — moving that into the script would mean hand-rolling the cache backend's runtime-token
plumbing for no gain.

Two incidental improvements fell out of the refactor. The docker-absence graceful skip now applies only
to the local path: `--stage-only` must fail loudly, because a CI job that silently skips staging would
hand `build-push-action` an empty context. And the notices in the image are now the artifact the
`licenses` job generated, downloaded into the repository root where the script reads them, replacing a
`mv`/`rm` shuffle that pulled them back out of one of the archives.

Verified: both script paths (compile-for-host and `--stage-only` with two `--prebuilt` arches), the
argument validation and its exit codes (2 for usage, 1 for a missing binary), the graceful skip in
local mode versus no skip in stage-only mode, and a Dockerfile build over a script-staged context on the
emulator-free builder. Workflow references re-validated; all nine actions still SHA-pinned.

## 2026-07-30 — CD: the image job stages inline; build-image.sh is local-only again

**Supersedes the "Staging is now defined once" paragraph in the entry above.** That entry describes
`scripts/build-image.sh` growing `--ctx` / `--stage-only` / `--prebuilt` so the `image` job could call
it. The user's requirement is the opposite and takes precedence: **the CI job must be transparent and
self-contained.** So the staging is inlined back into `release.yml`, and `build-image.sh` returns to
being the local-only tool it started as, with those three flags deleted -- they existed solely to serve
CI, and with CI no longer calling it they were dead surface.

This came from misreading "consider embedding build-image.sh in the workflow steps" as "call the script
from the workflow" when it meant "inline its logic into the steps". Worth recording because the two
readings produce opposite structures and the wrong one survived a whole review cycle.

**How the drift risk is handled now that there are two implementations.** The build-context layout
(`linux/<goarch>/{imbhd,imbh-tui}` plus `LICENSE` and `THIRD-PARTY-NOTICES.txt` at the context root) is
declared in a `BUILD CONTEXT CONTRACT` block in `docker/Dockerfile`'s header -- the *consumer* owns the
contract, and the two producers reference it:

- `scripts/build-image.sh` -- local, single-arch, compiles first.
- `release.yml`'s `image` job -- release, multi-arch, unpacks the archives.

A layout change is made in the Dockerfile header first and then applied to both. Each side has a cheap
detector: a `workflow_dispatch` dry run catches a CI-side mismatch, a bare `./scripts/build-image.sh`
catches a local one. That is a real cost of the inlining and is written down rather than papered over.

Inlining also simplified two things. The `image` job no longer needs `scripts` in its sparse-checkout,
and it no longer downloads the notices artifact separately: LICENSE and the notices are lifted straight
out of one of the unpacked archives, which is a *stronger* guarantee than the previous arrangement --
it makes the attribution in the image byte-identical to what a user downloads, by construction, rather
than by both happening to come from the same run.

**First full run of the local path, and it paid for itself twice.** `./scripts/build-image.sh` had
never been executed end to end (earlier verification used raw buildx over hand-staged contexts). It
works -- and because it builds *release* binaries, it produced two things the debug-based testing could
not:

1. **The glibc finding confirmed on release artifacts**, which is what the guard actually gates:
   `imbhd` requires at most `GLIBC_2.34`, `imbh-tui` requires `GLIBC_2.39` (34 MB and 33 MB
   respectively). The 2.36 threshold therefore rejects the real shipping `imbh-tui` when built on a
   2.39 host, exactly as intended. Previously this was only measured on debug builds.
2. **A usability bug in the image.** `docker run <image> mydata` -- a *relative* database path -- died
   with a bare `Storage(... PermissionDenied)` from deep inside storage, because `imbhd` resolves a
   relative `DB_DIR` against the working directory, which was `/` and is not writable by uid 10001.
   Fixed with `WORKDIR /var/lib/imbh`, so a relative path lands in the writable volume. Verified: the
   same command now opens the database and proceeds to "nothing to serve".

**Verified.** The inlined staging run verbatim against archives built in the exact shape the `Package`
step produces (top-level `imbh-<version>-<target>/`, `--strip-components=1`), yielding the contracted
layout; a Dockerfile build over that context on the emulator-free builder; the simplified
`build-image.sh` end to end twice; the `WORKDIR` fix against the failing command; and the finished image
serving `/health` and `/api/query` with the database initialised on the volume by the unprivileged user.
Workflow references re-validated, all nine actions still SHA-pinned, and the `image` job now contains no
reference to the script outside a comment.

## 2026-07-31 — `imbhd`: signal handling and graceful shutdown, and why `accept` is woken rather than polled

`imbhd` had no signal handling at all. `SIGTERM` — which is what `docker stop`, systemd, and a plain
`kill` send — took the default disposition and killed the process wherever it happened to be. Nothing
was *lost*, because the WAL is the durability contract, but everything since the last seal was
recoverable only by replay: every stop bought a replay on the next start, and an operator watching
`/stats` saw a segment count that never advanced past the last scheduled seal. For a process that owns
a flush scheduler (`FlushPolicy`, this same day's earlier entry), sealing on the way out is the whole
point of owning it.

**The shape.** A `Shutdown` token (`crates/imbh-server/src/shutdown.rs`) that every accept loop
watches, plus `serve_until` / `serve_plugin_until` / `serve_grpc_until` alongside the existing
`serve*` entry points (additive — the crates are published, so the old signatures and their "until the
process exits" contract stay). `main` installs the signal handlers, parks on the token, and on trigger
waits for each endpoint to report *stopped and drained* before calling `Db::close()`.

**Why `accept` is woken, not polled.** The obvious implementation is `set_nonblocking(true)` plus a
poll loop on the flag. It is wrong here: the reply carries `Connection: close`, so this protocol opens
one connection per request, and a 100 ms tick would land on the latency of *every OTLP POST* — up to a
tick between a connection arriving and `accept` noticing it. Instead a listener registers a waker with
the token, and `trigger()` makes one throwaway connection to the listener's own address. Blocking
`accept` stays blocking, idle cost is zero, and shutdown is immediate. Two details the implementation
needs: a wildcard bind (`0.0.0.0`) is not a connectable destination, so the waker aims at loopback on
the same port; and a waker registered *after* the token tripped runs immediately, or a listener that
bound late would park in `accept` forever. The gRPC side is the one place that does poll (a 50 ms tick
inside tonic's `serve_with_shutdown` future) — harmless, because HTTP/2 connections are long-lived, so
that tick is not on any request's path.

**Why the signal handler does almost nothing.** It stores an atomic and writes one byte to a self-pipe;
a watcher thread parked on the read end takes the locks and notifies the condvar. Tripping the token
directly from the handler would take a mutex inside a signal context, against a thread the handler may
have interrupted while holding it. `sigaction` is installed with `SA_RESTART` so the arriving signal
does not surface as `EINTR` in the middle of a FIFO read or a storage write — the news travels by pipe,
never by an interrupted syscall. A **second** signal `_exit(128 + signum)`s from the handler itself
(`_exit` is async-signal-safe and skips every destructor, which is the point).

**Footprint.** std cannot catch `SIGTERM`, so this needs `libc` — which is **already in `imbhd`'s
default graph** via `datafusion-common`, so the direct edge adds no crate. The footprint gate is
unchanged at 275 crates. The dep is `[target.'cfg(unix)'.dependencies]`; on other targets
`install_signal_handlers` reports `Unsupported` and `imbhd` warns and serves on, rather than silently
pretending to handle signals.

**Three things the drain forced open.**

1. **The Docker plugin's ingest worker could not be drained.** `Ingestor` held only a `SyncSender` and
   the worker exited when every sender dropped — which cannot happen while the `Ingestor` is alive
   behind an `Arc`. Lines already read off a container's FIFO would have been stranded in the queue (or
   in the worker's half-full batch) when `Db::close()` ran. Fixed with a `Queued::Stop` sentinel: the
   channel is FIFO, so the sentinel is behind everything already queued, and the worker ingests its
   batch and exits. `Ingestor::shutdown` then joins it, so the drain is *synchronous* with respect to
   the close that follows. A `closing` flag makes `send` refuse afterwards, so a still-parked FIFO
   reader cannot queue behind the sentinel and be dropped.
2. **The container FIFO readers cannot be joined, and must not be.** A reader is parked in a blocking
   read on a FIFO whose writer — the still-running container — holds it open; only the process exiting
   ends that read. Waiting for one would turn `docker plugin disable` into a hang. They get their stop
   flag set (observed between frames) and are then left to die with the process, which is safe
   precisely because `Ingestor::shutdown` refuses their late records. Clearing the stream registry also
   ends any `docker logs -f` this plugin is serving: follow mode stops once a container has no live
   stream, which is what lets those connections drain instead of holding the door open.
3. **`main` must not `join` its listeners.** A client that opens a socket and goes quiet parks a
   connection thread in `read_line` indefinitely; the listener's own drain has a deadline and gives up,
   but `JoinHandle::join` has none. So each thread reports completion on a channel and `main`
   `recv_timeout`s, which means one wedged endpoint cannot hold up the final seal.

**Verified.** Four new tests in `crates/imbh-server/tests/shutdown_e2e.rs` — prompt stop plus the port
becoming re-bindable (a supervisor restart must not hit `AddrInUse`), an in-flight request completing
*after* the trigger, a post-trigger connection getting no answer, and the binary under a real
`libc::kill(SIGTERM)` exiting 0 with its rows in **sealed segments** (run with `IMBH_FLUSH=manual`, so
the shutdown path is the only thing that could have sealed them — the assertion is unreachable without
signal handling). Plus a plugin drain test that configures a 30-second flush interval, so any row that
appears got there via the shutdown drain and nothing else, and asserts the socket is unlinked; a gRPC
stop test; and a unit test that raises `SIGTERM` at the test binary and requires it to become a tripped
token rather than a dead process. Full gate green (fmt/build/clippy `-D warnings`/`cargo test
--workspace`, plus `-p imbh-server` under `grpc,docker,tracing`), footprint gate OK at 275 crates.

**Deliberately not done.** No read/write timeouts on accepted connections: the drain deadline already
bounds shutdown, and a timeout would change behaviour for slow clients posting large OTLP bodies —
which is a separate decision from this one. A listener that dies with an error still `exit(1)`s without
sealing: whatever broke the listener is a poor moment to start writing Parquet, and the WAL covers every
accepted row.

## 2026-08-01 — `imbhd` connection deadlines, and a bug the test design caught before the test did

Closes the follow-up left open by the graceful-shutdown work (2026-07-31): `imbhd` is
thread-per-connection and its hand-rolled parser blocked in `read_line`/`read_exact` with no deadline, so
a client that connected and said nothing held a thread and a `Db` handle for as long as it liked. It also
made the shutdown drain sit out its whole deadline on a connection that was never going to finish.

**Two phases, two different rules.** The recorded intent was "a header/body deadline, not a blanket
`set_read_timeout`", and working through it confirmed why neither rule alone is right:

- The **head** must be bounded *in total*. A per-read allowance lets a client dribble one byte per
  allowance — never idle long enough to trip it, never finishing the request — and hold a thread forever.
- The **body** must be bounded *per read*. A total deadline there cannot distinguish a 50 MiB upload over
  a slow link from a client that stopped mid-body; it would punish the first for its size. The rule that
  matters is "do not stall", not "do not take a while".

So: `IMBH_HEADER_TIMEOUT` (default `10s`, total) and `IMBH_BODY_TIMEOUT` (default `30s`, per read, and
the response's write allowance), `0` disabling either, and a best-effort `408 Request Timeout` when one
blows. The 408 is worth the two lines: a dropped connection is indistinguishable from a crash, and an
OTLP exporter reads 408 as "retry".

**The bug the test design caught.** The first implementation armed the socket's read timeout before each
`read_line` — which reads correctly but is wrong, and the wrongness is invisible in the diff. `read_line`
is a `BufReader` method: it may block on *several* underlying reads, and each one gets whatever timeout
was armed before the call. So a trickling client resets the effective window on every byte and the
"total" budget never expires. This surfaced not from reading the code but from trying to write the
trickle test and working out what it would actually observe.

The fix is placement: an `Armed<S>` reader wrapped **under** the `BufReader`
(`BufReader<Armed<TcpStream>>`), which re-arms the socket against an *absolute* deadline on every real
read. `read_request` flips it to the body phase through `reader.get_mut()` once the head is parsed. Two
consequences worth noting: the phase switch has to be visible to the parser, so `read_request` gave up
being generic over `BufRead` and is now generic over the *socket* (`ReadDeadline + Read`) — which is the
genericity it actually needed, since its two callers are a `TcpStream` and a `UnixStream`; and the body
allowance is a constant, so it is armed once at the switch rather than per read. Steady-state cost is one
extra `setsockopt` per request, because a `BufReader` normally swallows a whole request head in a single
read.

**Verified.** Six tests in `crates/imbh-server/tests/timeouts_e2e.rs`, of which two discriminate against
the bug above rather than merely exercising the feature: a trickling client (one byte per `HEADER/6`,
indefinitely) must be cut off at ~`HEADER` — under the per-`read_line` arming it would have run the full
6 s of trickle, so the assertion and the file's total runtime both fail — and a slow-but-progressing body
whose *total* transfer exceeds `BODY` must still return 200 with the row in the DB. Plus a quiet client
getting 408 at ~`HEADER`, a stalled body getting 408 with **nothing ingested**, `IoTimeouts::DISABLED`
restoring the old never-time-out behaviour, and the shutdown payoff: with a 30 s drain and an idle
connection, `serve_with_until` now returns in well under half of it instead of sitting out the deadline.

A near-miss in the tests themselves: two assertions were written as
`SELECT count(*) … .num_rows() == 1`, which is vacuously true — `count(*)` always returns exactly one
row, so they asserted nothing about the count. Replaced with a helper that reads the `Int64` value, which
is what turned "nothing was ingested from the truncated request" into a real assertion (it is 0, and the
test would now catch a regression that ingested a partial body).

Full gate green (fmt/build/clippy `-D warnings`/`cargo test --workspace`, 54 suites, plus
`-p imbh-server` under `grpc,docker,tracing`); footprint untouched — this is std sockets only, no new
dependency.

**One thing deliberately left as is.** The Docker plugin socket applies the *defaults* and is not
operator-tunable: its peer is the local `dockerd`, which is prompt or gone. That path did gain something
for free, though — the response write deadline means a `docker logs -f` client that vanishes without
closing no longer holds its thread and follow stream open indefinitely.

## 2026-08-01 — Session summary: `imbhd` lifecycle hardening (shutdown + connection deadlines), and the knob interaction it exposed

One arc across two entries above — signal handling and graceful shutdown (2026-07-31), then the
per-connection deadlines that follow-up demanded (2026-08-01). The mechanisms are documented there; this
records the shape of the whole change, the findings that belong to neither entry alone, and one honest
caveat about the defaults.

**What shipped.** `imbhd` now has a lifecycle: `SIGINT`/`SIGTERM` stop every listener, in-flight work
drains, the Docker plugin's queued container lines land in the DB, `Db::close()` seals, exit 0 — and no
client can park a server thread indefinitely while any of that happens. New public surface on
`imbh-server`, all additive (the crate is published, so nothing changed shape):

| Added | For |
|-------|-----|
| `Shutdown` (`trigger`/`wait`/`is_triggered`/`on_trigger`/`install_signal_handlers`/`drain_timeout`/`cause`), `shutdown::signal_name` | the token every endpoint watches |
| `serve_until`, `serve_with_until` | HTTP, with and without explicit deadlines |
| `docker::serve_plugin_until`, `docker::serve_plugin_with_until`, `docker::ingest::Ingestor::shutdown` | the plugin endpoint and its ingest drain |
| `grpc::serve_grpc_until`, `grpc::serve_grpc_blocking_until` | the tonic listener |
| `IoTimeouts` (+ `DISABLED`), `io_timeouts`, `DEFAULT_HEADER_TIMEOUT`, `DEFAULT_BODY_TIMEOUT`, `DEFAULT_SHUTDOWN_TIMEOUT`, `shutdown_timeout` | the knobs, and their env parsers |

Four new environment variables (`IMBH_SHUTDOWN_TIMEOUT`, `IMBH_HEADER_TIMEOUT`, `IMBH_BODY_TIMEOUT` —
plus all three declared `settable` in the managed plugin's `config.json`). Zero new crates: signal
handling rides `libc`, already in the graph via DataFusion; the deadlines are std sockets. Footprint gate
unchanged at 275 crates.

**The caveat: the two knobs interact, and the defaults do not line up.** The follow-up item claimed an
idle connection "makes the shutdown drain wait out its whole deadline". The deadlines fix that only when
the header timeout is *shorter* than the drain — and the stock defaults are the other way round (header
`10s`, drain `5s`), so with defaults an idle connection is still abandoned by the drain rather than cut
off before it. The `timeouts_e2e` test that demonstrates the payoff uses a 600 ms header against a 30 s
drain, i.e. it proves the mechanism, not the default configuration. Defaults were left alone (10s is the
right head budget for a collector on its own; 5s is the right drain against Docker's 10s stop grace), and
the relationship is now documented instead: set `IMBH_HEADER_TIMEOUT` below `IMBH_SHUTDOWN_TIMEOUT` if
you want the drain to end early on idle connections. Worth knowing before treating "connections are
bounded" as "shutdown is always prompt".

**Findings worth keeping.**

- **`Db::close()` seals even under `FlushPolicy::manual`** — confirmed, not assumed: the binary-level
  SIGTERM test runs with `IMBH_FLUSH=manual` and finds three rows in sealed segments afterwards, so the
  shutdown path is provably the only thing that could have sealed them. That property is what makes
  `manual` safe to run in production rather than a data-stranding footgun.
- **A binary can be signal-tested in-package with no fixtures.** `env!("CARGO_BIN_EXE_imbhd")` is
  available to a package's own integration tests, so "spawn the real `imbhd`, `libc::kill` it, assert the
  exit status *and* reopen the data directory read-only" is an ordinary hermetic test. This is the
  strongest shape available for anything whose contract spans the process boundary (exit codes, signal
  dispositions, on-disk state after exit) and it needs no daemon, no network, no privileges — worth
  reaching for before settling on a unit test that can only approximate the property.
- **Verifying a `#[cfg(not(unix))]` fallback without a Windows toolchain.** `cargo check --target
  x86_64-pc-windows-gnu` cannot run here — `zstd-sys` needs `x86_64-w64-mingw32-gcc`, which is absent —
  so the non-Unix branch was checked by *inverting the cfg on a Unix host*: rewrite `#[cfg(unix)]` to
  `#[cfg(SOME_UNSET_CFG)]` and `#[cfg(not(unix))]` to `#[cfg(not(SOME_UNSET_CFG))]`, `cargo check`
  (0 errors), then restore from a backup copy. Cheap, and it catches exactly the class of bug that
  otherwise ships: a fallback arm nobody has ever compiled.
- **Two small compile-time stumbles, recorded so the next reader does not re-derive them.** A local
  `enum Message` collides with prost's `Message` trait under `use prost::Message` (E0255 — the enum is now
  `Queued`), and a bare `handle_signal as libc::sighandler_t` trips the `function_casts_as_integer` lint
  (cast via `as *const ()` first). tonic 0.14's graceful path is
  `tonic::transport::server::Router::serve_with_shutdown(addr, signal)`, which is what the gRPC listener
  hangs its token future on.
- **Tests that discriminate beat tests that exercise.** Both entries above turned on assertions chosen
  to fail against a specific wrong implementation — the trickle test against a per-`read_line` deadline,
  the 30-second-flush-interval plugin test against a missing ingest drain, the `IMBH_FLUSH=manual` binary
  test against absent signal handling. Two assertions written the other way (`count(*)` compared with
  `num_rows()`, vacuously 1) proved nothing until rewritten. The habit that caught both: state what
  wrong implementation the assertion is supposed to reject, before writing it.

## `imbh-server`: hand-rolled HTTP/1.1 → axum/hyper (2026-08-01)

Replaced the reference server's `std::net`, thread-per-connection HTTP/1.1 server with axum over
hyper. The prompt was a design question ("does migrating to axum make sense?"), and the first answer
was *no* — on a footprint argument that turned out to be measuring the wrong thing.

- **The footprint objection was mis-scoped, and that was the whole argument.** `imbh-server` linking
  axum was assumed to spend the §11 crate budget. It does not: `scripts/footprint-gate.sh:25` counts
  `cargo tree -p imbh` — the *facade* — and the dependency direction is `imbh ← imbh-server`. The
  gated number is 275 before and after. Measured cost is ~17 crates in `imbh-server`'s own graph
  (287 → 304 by name) and ~1.4 MiB of `imbhd` binary (31.2 → 32.6 MiB, against a 42 MB target). The
  general lesson: before pricing a dependency against a budget, check which graph the gate actually
  walks. "It is in the workspace" is not "it is in the budget".
- **tonic 0.14 routes through axum**, so `--features grpc` was *already* pulling axum/hyper/tower/h2.
  After the migration `grpc`'s marginal cost is tonic + h2 (6 crates over default) and the
  full-feature graph is unchanged at 310. A feature that used to justify itself as "off by default so
  the subtree stays out" was, on inspection, the reason the subtree was reachable at all.
- **The real cost was the runtime model, not the dependencies.** `Db`'s futures do blocking
  parquet/tantivy I/O *inside themselves* — `grep -rn 'spawn_blocking\|block_in_place'` over
  `imbh`/`imbh-storage`/`imbh-query` returns nothing — which the old design made safe by giving each
  connection its own current-thread runtime on its own thread. On a shared multi-threaded runtime that
  same await parks a worker and starves every other connection. Hence `offload`: `block_in_place` +
  `Handle::block_on`, with a plain `.await` fallback when `Handle::runtime_flavor()` is not
  `MultiThread` (`block_in_place` panics on a current-thread runtime, which is what `#[tokio::test]`
  builds and what the existing socket-free `route()` tests run on). Keeping the fallback is what let
  every one of those tests stay unchanged.
- **hyper reports the header deadline but does not answer it.** `header_read_timeout` yields
  `Kind::HeaderTimeout`, and `proto/h1/role.rs::on_error` maps parse errors to a status but returns
  `None` for that one — so hyper closes silently where the old server sent `408 Request Timeout`, and
  a passing test asserted the 408. Preserved it by `try_clone`ing the accepted socket before handing
  it to hyper and writing the 408 on the duplicate when the connection ends in `e.is_timeout()` with
  no request head ever parsed. The dup shares `O_NONBLOCK` with hyper's fd (file *description* flags),
  so the spare has to become a second `tokio::net::TcpStream` rather than a blocking std one.
- **Four defects fell out of the transport, not out of care.** Chunked bodies read as empty and
  answered `200 {"accepted":0}` (the old parser keyed entirely off `Content-Length`); `vec![0u8;
  content_length]` allocated from an attacker-controlled header before reading a byte; no gzip, which
  the OTel Collector's `otlphttp` exporter sends *by default*; every response carried
  `Connection: close`. Each is now a test in `tests/protocol_e2e.rs`. Worth noting how they hid: the
  chunked one returned a **success** status, so nothing short of asserting on `accepted` could catch
  it, and the gzip one was documented as a known limitation with a workaround (`compression: none`)
  rather than tracked as a bug.
- **gzip cost zero crates.** `flate2` was already in the default graph via parquet — but pinned to the
  `zlib-rs` backend (`parquet/flate2-zlib-rs`), so declaring it with flate2's *default* features would
  have pulled miniz_oxide as a genuinely new crate. When adding a dependency that is already present,
  match its feature selection, not just its name.
- **What did not move.** The Docker plugin endpoint keeps the crate's own HTTP/1.1 parser: it is a
  different protocol (`docker.logdriver/1.0`) on a Unix socket with a streaming `ReadLogs` endpoint
  that writes its own frames. `Armed`/`read_request`/`write_response` are now `#[cfg(all(feature =
  "docker", unix))]`, so the default build carries none of it. Porting it is its own change with its
  own risk — recorded in TODO.md rather than bundled in.

## 2026-08-01 — axum migration, part 2: what self-review caught that the tests could not

Completes the entry above (the design findings and the four transport defects are there; this is the
work summary, the problems found *after* it was passing, and the verification record).

**Shipped.** `crates/imbh-server/src/lib.rs` rewritten around `axum::Router` + `hyper::server::conn::
http1` (the accept loop, `handle`/`read_body`, `offload`, `Limits`); `shutdown.rs` lost
`wake_tcp_listener` and gated `InFlight`/`Busy` to `docker`; `main.rs` threads a `Limits` and prints
it; new `tests/protocol_e2e.rs` (7 tests) plus two `route()` unit tests for the 404→405 change.
Additive public API: `app`, `Limits`, `serve_with_limits_until`, `offload`, `max_body`,
`max_connections`, `DEFAULT_MAX_BODY`, `DEFAULT_MAX_CONNECTIONS`. Docs corrected in six places
(below). `Cargo.lock` grew **9 lines** — the axum/hyper/tower entries were already there for `grpc`.

- **`axum::serve` has no hook for the hyper builder, and that decided the shape of the code.**
  `serve/mod.rs:391` constructs `Builder::new(TokioExecutor::new())` internally with no config
  parameter, so `header_read_timeout` — the thing `IMBH_HEADER_TIMEOUT` maps onto — is unreachable
  through it. Preserving a documented, tested knob therefore meant a manual accept loop over
  `hyper::server::conn::http1::Builder` with `hyper_util`'s `GracefulShutdown` for the drain, rather
  than the three-line `axum::serve(listener, app).with_graceful_shutdown(..)` idiom. Anyone reading
  this loop and wondering why it is not the idiom: that is why. Note `header_read_timeout` is also a
  no-op unless `builder.timer(TokioTimer::new())` is set — it fails silently, not loudly.
- **The 408-preserving trick paid for itself in a resource nothing was measuring.** Writing the
  header-timeout 408 needs a second descriptor on the socket (hyper consumes the first and answers
  nothing), and the first cut took that dup for *every* accepted connection. With the cap at 1024 that
  is 2048 descriptors against a typical 1024 soft `RLIMIT_NOFILE` — the connection cap would have been
  a cap on half as many descriptors as it advertised, and `EMFILE` would have arrived long before the
  limit it was there to enforce. No test would have caught it: none opens hundreds of sockets, and the
  6 timeout tests plus 7 protocol tests all passed. Fixed by taking the dup only when a header
  deadline is configured and releasing it the instant a request head arrives (the service already sets
  `served` for the 408 guard, so the drop point was free), and lowering the default cap to 512 to
  leave headroom for parquet and tantivy descriptors. **The general shape is worth remembering: a
  safety feature that bounds one resource can quietly spend a different one, and the tests written for
  the first resource will stay green.** Budget the fix in the same units as the thing it protects.
- **`let`-chains extend temporaries across the whole `if` body, which turns a `std::sync` guard into a
  `Send` error naming the wrong line.** `if let Err(e) = conn.await && ... && let Some(x) =
  m.lock().unwrap().take() { ... x.write_all(..).await }` keeps the `MutexGuard` alive to the end of
  the block, so the guard straddles the inner `.await` and the compiler reports *"future cannot be
  sent between threads safely"* pointing at the enclosing `tokio::spawn`, not at the lock. The fix is
  a plain `let` statement for the `take()` before the `if`. Cheap once recognised; the diagnostic does
  not point at the cause.
- **One behaviour genuinely regressed, and it is not visible in any test.** `IMBH_BODY_TIMEOUT` used
  to bound the *response write* too (`set_write_timeout` on the socket); hyper exposes no write
  deadline, so a client that stops reading its response now holds a connection until it goes away —
  bounded by `IMBH_MAX_CONNECTIONS`, i.e. by count rather than by time. Recorded in TODO.md rather
  than papered over. Worth stating plainly because the migration is otherwise a strict improvement,
  and "strictly better" is exactly the claim under which a small regression rides along unmentioned.
- **A transport change falsifies prose in places grep-for-the-feature will not find.** Six documents
  carried claims that went false: `ARCHITECTURE.md` §10.16 (three paragraphs, including a whole
  passage on the `Armed` reader that no longer exists), `crates/imbh-server/README.md`,
  `LTM/reference-server-exporter-and-ops.md` (which stated "**No axum/hyper/tower**" as a design
  axiom), `docs/DOCKER_LOG_DRIVER.md` (told operators to set `compression: none`, now wrong *and*
  wrong in the unhelpful direction), `CHANGELOG.md`'s own unreleased entry, and the crate
  description in `Cargo.toml`. What found them was grepping for the *claims* — `thread-per-connection`,
  `std::net`, `hand-rolled`, `no axum` — not for the subsystem. A feature-shaped grep misses every
  sentence that describes the old design without naming it.
- **Verification.** `fmt`/`build`/`clippy -D warnings`/`test` clean on the workspace and on
  `--features docker,grpc,tracing`; **408 tests** pass, including all six pre-existing `timeouts_e2e`
  tests unchanged. Footprint gate OK: facade **275 crates** (unchanged, at target), `imbhd` **32.6
  MiB** (was 31.2, budget 42 MB), search-off lever still 71 crates. Binary smoke-tested end to end —
  banner, keep-alive, `405` on a known path with the wrong method, `404` unknown, `413` on a forged
  `Content-Length: 10GiB`, SQL round-trip, and `SIGTERM` → sealed buffer, exit 0. One gate run
  reported `datafusion: NO` under concurrent cargo processes and was a flake (`cargo tree` output
  raced; the script swallows its stderr with `2>/dev/null`) — re-running shows both engines present.

## 2026-08-01 — the Docker plugin socket onto axum too, and how to migrate a blocking generator

The TCP listener moved to axum/hyper earlier today; this finishes the job by moving the Docker
logging-driver plugin's Unix socket onto the same stack. Both listeners now share one `handle`, so
body limits, phase deadlines, and `Content-Encoding` decoding are identical on both, and the crate's
hand-rolled HTTP/1.1 parser (`Armed`/`ReadDeadline`/`read_request`/`write_response`, ~6.6 KB of
source) is **deleted** rather than merely `#[cfg]`-gated. `shutdown.rs` lost `InFlight`/`Busy` too —
hyper's `GracefulShutdown` is the drain for both now.

- **A blocking generator does not have to become a `Stream` to be served by one.** `ReadLogs` is the
  hard part of this endpoint: `readlogs::stream` is synchronous, generic over `io::Write`, does its
  own paging and follow-mode polling, and under `Follow` runs until the container stops. Rewriting it
  as a `Stream` would have put the paging state machine, the follow watermark, and the idle-exit rule
  all at risk for no behavioural gain. Instead it runs **unchanged** on a `spawn_blocking` task whose
  sink is a bounded `mpsc` channel, with a hand-written `http_body::Body` draining that channel as the
  response. The generator kept every line; only the sink changed. Worth reaching for whenever the
  blocking code is the part that encodes hard-won behaviour.
- **The channel is also the backpressure and disconnect signal, which is what made it safe.** Sending
  is `try_send` in a loop against a 30s stall deadline, so a `docker logs -f` client that stops
  reading surfaces to the generator as `ErrorKind::TimedOut` and one that hangs up as
  `ErrorKind::BrokenPipe` — both already handled, because the generator was written against a socket
  that could do exactly that. This is the same write-deadline property the socket used to provide via
  `set_write_timeout`, recovered rather than lost: notable because the buffered HTTP responses *did*
  lose it (still in TODO.md). A channel sink can see backpressure; a buffered response cannot.
- **`spawn_blocking` threads may create and drive a runtime.** `readlogs::stream` builds its own
  current-thread runtime and `block_on`s the typed query API. Whether that survives inside
  `spawn_blocking` decided the whole design, so it was measured rather than assumed (a 20-line probe):
  both an owned current-thread runtime **and** the outer `Handle::block_on` work there — tokio marks
  those threads as a blocking region, so the "cannot start a runtime from within a runtime" panic does
  not apply. Zero changes to `readlogs.rs` followed from that one check.
- **The wire format changed, and the test client was the only thing that cared.** `ReadLogs` bodies
  are now `Transfer-Encoding: chunked` — hyper frames a body of unknowable length properly, where the
  old server wrote frames raw and closed the socket. Three tests failed with *"log entry frame of
  858918154 bytes exceeds the limit"*: the chunk-size hex line being read as a frame length. The right
  fix was to teach the test to un-chunk, not to fight hyper, because the real client is Docker's Go
  `net/http`, which un-chunks transparently — the hand-rolled test client had been the only consumer
  of a non-standard framing. **Check who the real client is before treating a wire change as a
  regression.**
- **A 15× test speedup fell out, and it was a latent smell.** `docker_plugin_e2e` went from 30.06s to
  1.72s. The plugin now keeps connections alive, so the test client's `read_to_end` was waiting out
  the 10s header deadline for the *next* request before seeing EOF. Adding `Connection: close` to the
  test client fixed it. The old server closed every connection, so that read-to-EOF idiom had always
  worked by accident; keep-alive turned an implicit assumption into 28 seconds of sleeping. A suite
  that got dramatically slower or faster after a transport change is reporting something real.
- **Wind-down order differs between the two listeners, and it is not arbitrary.** The plugin calls
  `plugin.shutdown()` — stopping FIFO readers and draining the ingest queue — *before* the connection
  drain, because clearing the stream registry is also what ends the `docker logs -f` responses still
  open (follow mode exits once its container has no live stream). Draining first would wait out the
  full `IMBH_SHUTDOWN_TIMEOUT` on connections that only stop because of that call.
- **Footprint unchanged, again.** `docker` still adds **zero** crates (304 with and without it), the
  facade stays at 275, `imbhd` at 32.6 MiB. `http-body` became a direct dependency for the hand-written
  `Body` impl and was already in the graph via hyper.
- **Verification.** `fmt`/`build`/`clippy -D warnings` clean on the workspace and on
  `--features docker,grpc,tracing`; 408 workspace tests plus all 7 plugin e2e tests pass. Smoke-tested
  against the real binary over a Unix socket: `Plugin.Activate` and `Capabilities` answer in
  `application/vnd.docker.plugins.v1.1+json`, two calls share one connection, an unknown endpoint is
  404, `ReadLogs` returns `content-type: application/x-json-stream` with `transfer-encoding: chunked`,
  and `SIGTERM` seals the buffer, unlinks the socket, and exits 0.

## 2026-08-01 — Session summary: the axum arc, and a doc sweep that had to be run twice

Closes the three entries above (TCP migration, its self-review, the Docker plugin). Recorded here is
what happened *after* the plugin entry, plus the arc's final state.

- **A hand-listed doc sweep missed a file, and only a second sweep caught it.** The TCP migration's
  entry claims the doc sweep worked by grepping for the *claims* rather than the feature — true, but
  the grep ran over a **hand-written list of paths**, and the repository's root `README.md` was not on
  it. So `README.md` went on describing `imbhd` as "a minimal `std::net` HTTP stack (zero heavy deps)"
  through an entire migration and its verification, and survived a sweep whose whole purpose was to
  catch exactly that. It was found only because the same grep was re-run with a wider file list after
  the plugin change. Two lessons, and the second is the real one: sweep by **repository**, not by a
  remembered list of the files you touched — the stale claims are precisely in the files you did *not*
  touch. And a "documentation updated" claim is worth as much as the file list behind it, which is why
  the sweep is now a grep over the tree rather than an act of memory. (Also caught: a comment in
  `readlogs.rs` still calling the plugin "a blocking thread-per-connection design".)
- **The `datafusion: NO` gate flake reproduced**, confirming the earlier diagnosis rather than leaving
  it as a one-off guess: `scripts/footprint-gate.sh:52` swallows `cargo tree`'s stderr with
  `2>/dev/null`, so under concurrent cargo processes a failed/partial tree reads as "the engine is
  absent" and the gate still prints OK. Worth fixing when the gate is next touched — a check that
  reports a missing engine dependency should fail, not decorate.

**Final state of the arc.** Both listeners on axum/hyper; the hand-rolled HTTP/1.1 parser and
`shutdown.rs`'s `InFlight`/`Busy` deleted outright. Four transport defects fixed (chunked read as
empty, unbounded `Content-Length` allocation, no gzip, no keep-alive) plus two the migration itself
introduced and self-review caught (doubled descriptors per connection, a `MutexGuard` across an
await). New: `tests/protocol_e2e.rs` (7 tests). Public API additive only — `app`, `Limits`,
`serve_with_limits_until`, `offload`, `max_body`/`max_connections` and their defaults; `serve`,
`serve_until`, `serve_with_until`, `route`, `IoTimeouts`, and `Shutdown` unchanged. Client-visible
changes, all in CHANGELOG: keep-alive, `405` for a known path with the wrong method, and
`Transfer-Encoding: chunked` on `LogDriver.ReadLogs`.

Verified: `fmt`/`build`/`clippy -D warnings` clean on the workspace and on
`--features docker,grpc,tracing`; **408** workspace tests and **77** with all features; footprint gate
OK with the facade unchanged at **275 crates** and `imbhd` at **32.6 MiB** (from 31.2, budget 42 MB);
`docker` still adds **zero** crates. Both sockets smoke-tested against the real binary. Nothing is
committed — 17 modified files plus the new test.

Still open from this arc (TODO.md): no write-side deadline on *buffered* responses — the streaming
`ReadLogs` case is covered by its channel sink, which can see backpressure where a buffered response
cannot.

## 2026-08-01 — `imbhd` serves MCP: a gap analysis that chose the smaller build

**Where this started.** The ask was to look at `grafana/mcp-grafana` and report what stands between
it and `imbhd`. The finding that mattered was structural rather than a list of missing endpoints:
**mcp-grafana is a client of the Grafana API, not of a telemetry backend.** Every datasource tool
resolves a UID through `GET /api/datasources/uid/{uid}` and tunnels through Grafana's proxy
(`.../resources`, falling back to `.../proxy` on 403/500). So "use mcp-grafana with imbhd" can only
mean `agent → mcp-grafana → Grafana → datasource proxy → imbhd`, which requires imbhd to be
registerable as a Prometheus *and* Loki datasource and byte-compatible on the wire — the question
ARCHITECTURE.md §15 Q5 left open. Of its ~80 tools only 13 could ever reach imbh data; traces are a
dead end entirely (there is no `tempo.go`, and `prom_backend.go` explicitly errors on datasource type
`"tempo"`), so `imbh.traceql.t1.v1` would get zero leverage from the integration.

That reframed the work: building the Prom+Loki HTTP surface is the *expensive* path to a *smaller*
capability. A native MCP server reaches everything mcp-grafana structurally cannot — SQL, traces,
full-text `matches()`, segment stats — for far less. The user picked that, hosted in `imbh-server`
rather than as a new crate.

**What shipped.** `POST /mcp` on the existing axum router: `crates/imbh-server/src/mcp/{mod,json,
tools}.rs`, 15 read-only tools over §10.5–§10.9, plus `tests/mcp_e2e.rs` (9 tests driving the router
through `tower::oneshot`, which is what lets a test set the headers `route()` cannot). Details worth
keeping:

- **Zero new dependencies, on purpose.** MCP is JSON-RPC over HTTP; requests parse through
  `imbh::parse_json` (imbh-core's dependency-free parser, already in the graph) and responses are
  built with a small `Obj`/`Arr` writer over the existing `json_string`. `serde_json` *is* compiled
  transitively (arrow-json ← arrow ← datafusion), so pulling it in would have been footprint-neutral
  in crate count — but the crate's own convention is hand-rolled JSON, and the writer that respects
  it is ~120 lines.
- **The spec moved.** Revision `2026-07-28` made MCP **stateless**: no `initialize`, per-request
  `params._meta` carrying the version, a mandatory `server/discover`, `resultType: "complete"` on
  every result, and a *validated header mirror* (`MCP-Protocol-Version` / `Mcp-Method` / `Mcp-Name`
  must match the body, `-32020` otherwise). Clients in the field are still on the `initialize`
  handshake of `2025-11-25` and earlier. Implementing either alone would have been unusable by half
  of them, so the endpoint chooses **era per request**: a declared `_meta` version (or
  `server/discover`) selects the stateless path, anything else the handshake path. Reading the spec
  first was load-bearing — the pre-2026 shape was the one in memory, and it is now the *legacy* one.
- **Tool errors are not protocol errors.** A bad duration, an unparseable trace id, or a SQL syntax
  error comes back as `isError: true` in the result, not as a JSON-RPC error, because that is what a
  model can self-correct from. Only an unknown *tool* is a protocol error (`-32602`): no argument
  would fix it.
- **The default window is a real usability trap.** Tools default to a 1h look-back, so a replayed or
  historical database answers "nothing" to every question. Mitigated by pointing `instructions` and
  the tool descriptions at `db_stats` first (it reports each table's true time span) — and the e2e
  tests hit it immediately, since the OTLP fixtures sit at epoch nanosecond ~1 and every assertion
  had to pass explicit `start_unix_nano`/`end_unix_nano`.
- **`Origin` is the one defence implemented.** The endpoint is unauthenticated like the rest of
  `imbhd`, but the transport's DNS-rebinding rule is enforced: a browser `Origin` outside the loopback
  set is refused `403` (`IMBH_MCP_ALLOWED_ORIGINS` widens it). Non-browser clients send no `Origin`,
  so agents are unaffected. Worth remembering that `/mcp` shares a port with OTLP ingest and
  `/admin/*`: exposing one exposes all three.
- **`stats_json` was extracted** from `stats_response` so `GET /stats` and the `db_stats` tool cannot
  describe the same database differently.

**Verified.** `fmt` / `build` / `clippy -D warnings` clean; `cargo test --workspace` green (56 test
targets, no failures), of which 24 are new (15 unit + 9 e2e). No dependency change, so the footprint
gate's inputs are untouched — the crate-count gate measures `cargo tree -p imbh` and the direction is
still `imbh ← imbh-server`. Docs: `docs/MCP.md` (new), ARCHITECTURE.md §10.16.1, plus the root
README, the crate README, and the `imbhd` banner/module docs. Nothing committed.

**Open.** The stdio transport for `imbh-tui` is not built (TODO.md); `mcp::handle` was kept
transport-agnostic (`bytes + headers → Reply`) so it can be lifted without touching protocol logic.

### Addendum — what the smoke test against real data caught

Two defects survived a green `cargo test --workspace` and were found only by running the actual
binary against a `gen-demo-db` database. Both were *description* defects, which is the class this
endpoint is most exposed to: a wrong tool description silently misroutes every model that reads it,
and no assertion about response shape can see it.

1. **`query_sql` listed six of the seven signal tables** — `metrics_summary` was missing, so a model
   would never query it. The test meant to prevent exactly this (`the_sql_tool_lists_every_table`)
   passed, because it iterated a **hand-written** `TABLES` const in the test file that had the same
   omission. Fixed by driving it off `imbh::Table::ALL`, which is the authority. Same lesson as the
   doc sweep two entries up: a check written from memory verifies memory, not reality.

2. **`group_by: ["service.name"]` silently yields one empty-labelled series.** Both my `log_volume`
   and `span_metrics` descriptions used it as the *example*. The cause is library-side and
   pre-existing: `SqlParams::attr_field` (`crates/imbh/src/sql.rs:47`) resolves a key to a real
   column only when it is in the DB's configured `Promote` list, and otherwise emits
   `json_get_str(attributes, key)`. `service.name` lives in the `resource` column and the promoted
   `service` column — never in record `attributes` — so it reads NULL for every row, and the group
   collapses to `{"service.name": ""}` with the counts merged. `service` as a *filter* works fine;
   only grouping is affected, and it affects `LogsApi::volume_by`, `TracesApi::span_metrics`, and the
   metrics group-by equally. Descriptions now say group keys are record attributes and point at the
   `service` filter for per-service splits; the behaviour is pinned by a test so a library-side fix
   surfaces as a failure, and the underlying quirk is in TODO.md.

The first assertion attempt for (2) over-corrected — "no group label may be empty" — and failed
against a fixture where *some* spans lack the key. That is correct SQL semantics for a missing
attribute, not a bug: the assertion, not the code, was wrong. Worth stating because the empty label
means two different things (this record has no such attribute vs. this key is never an attribute)
and only the second is the trap.

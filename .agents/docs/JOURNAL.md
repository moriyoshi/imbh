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

## 2026-08-01 — The MCP endpoint moves onto serde_json + base64 (and why that costs nothing)

The MCP endpoint shipped with hand-rolled JSON — an `Obj`/`Arr` string builder, imbh-core's
`parse_json` for input, and a 60-line Base64 codec — on the reasoning that `imbh-server` had no
`serde_json` edge and the crate's convention was to hand-roll. **On user instruction, that is now
`serde_json` and `base64`.** Worth recording because the footprint reasoning behind the original
choice was wrong in an instructive way, and because the rewrite paid for itself in defect surface.

**The footprint argument never applied.** Both crates were *already compiled in the default graph* —
`serde_json` via `arrow-json`, `base64` via `arrow-cast`, both under DataFusion. Measured before and
after with the gate's own counting method: **275 → 275** on the `imbh` facade, **293 → 293** on
`imbh-server`. The hand-rolled code was avoiding a dependency the build already paid for. The general
lesson: "this crate has no such dependency" is a claim about the *manifest*, and the footprint
question is about the *graph* — check `cargo tree -i` before hand-rolling to protect a budget.

**What the rewrite removed.** ~200 lines: the JSON writer, its string-escaping, the Base64
encode/decode pair, and the `raw`-splicing discipline that came with building JSON as text (every
nested value had to be rendered first, and any mistake produced malformed output rather than a
compile error). Three specific hazards went with it:

- `embed_json` existed only because a span's `events`/`links` column is stored JSON text that had to
  be spliced into a hand-built document; it parsed the blob solely to prove it was safe to splice.
  With `serde_json` it is one `from_str` with a string fallback.
- Non-finite floats needed a bespoke `number()` guard to avoid emitting `NaN` (invalid JSON) from
  p50/p95/p99 over empty buckets. `serde_json::Number::from_f64` rejects exactly those cases, so the
  guard is now a two-line wrapper over the library's own invariant rather than a rule to remember.
- JSON-RPC ids were echoed as pre-rendered *text*; they are now `Value`s, so a string id round-trips
  as a string and a numeric id as a number without the writer having to be told which it was.

**One deliberate behaviour change.** Without `serde_json`'s `preserve_order` feature, `Map` is a
`BTreeMap`, so object keys now serialize **alphabetically** rather than in construction order. Left
that way on purpose: enabling `preserve_order` flips the feature for *every* `serde_json` user in the
graph (DataFusion included) through Cargo feature unification, which is a large blast radius to buy
field order in an agent-facing payload. `POST /api/query` is unaffected — `batches_to_json` is
untouched and still emits rows in schema order; the MCP `query_sql` tool parses that output back into
a value, which is the one place the reorder is visible.

Two smaller consequences, both fine: optional scalars now serialize as `null` instead of being
omitted (a uniform key set is easier for a model to read than a shifting one; empty *collections* are
still omitted, since `{}` on every entry of a 1000-row page is pure noise), and the tests now assert
on parsed structure instead of substrings — `initialize(None)["protocolVersion"]` rather than
`.contains(r#""protocolVersion":"2025-11-25""#)`.

Verified: `fmt`/`build`/`clippy -D warnings` clean; `cargo test --workspace` green across 56 targets.
The nine MCP e2e tests passed **unmodified** through the rewrite before their helpers were converted,
which is the useful signal — the wire behaviour is unchanged, only the machinery under it.

## 2026-08-01 — `imbh-tui` split: one 9,384-line `lib.rs` into 27 modules (and a gate that could not run)

`crates/imbh-tui/src/lib.rs` had grown to 9,384 lines — 6,900 of source and a single 2,480-line
`mod tests`. Split it along the boundaries the code already had, with **no behaviour change**: every
line of the original either moved verbatim or is one of the four accounted-for edits below.

**The seams.** The file was already layered; the split just names the layers.

| module | contents |
| --- | --- |
| `model` | `Route`, `Snapshot`, `Mode`/`Focus`/`Screen`, catalog nodes, `Update`, `Options`, `TIME_RANGES` |
| `app/` | `App` + its state machine, split by concern: `mod` (struct, `apply`, query accessors), `nav`, `window`, `catalog`, `views`, `completion` |
| `ui/` | `draw` and the frame chrome, plus `glyphs`, `metrics`, `logs`, `traces`, `overlays` |
| `keys`, `runtime`, `terminal` | key map, event loop, raw-mode/panic-hook/input-reader |
| `fetch`, `tasks` | the queries behind a refresh; the `request_*` spawners that keep them off the loop |
| `syntax`, `completion`, `promql` | query-language support behind the editor |
| `format`, `time`, `waterfall`, `detail_text`, `chart`, `mascot` | display helpers and the easter egg |

`App`'s ~1,250-line inherent impl became six `impl App` blocks, one per `app/` submodule — Rust lets
inherent impls span modules, so the concerns separate without a trait or a wrapper type. Largest file
is now `mascot.rs` at 885 lines; the median is ~280.

**Tests moved next to what they exercise.** The 95 tests were redistributed into 21 per-module
`#[cfg(test)] mod tests` blocks; the nine shared fixtures (`sample_trace`, `catalog_app`,
`app_with_discovered_dims`, …) live in a new `#[cfg(test)] mod testutil` and are imported by name.

**The four deliberate edits.** (1) Item visibility: cross-module items, struct fields, and inherent
methods became `pub(crate)`; `Options`/`parse_datetime`/`run` stay `pub` and are re-exported from
`lib.rs`. (2) The one `use` block became per-module headers. (3) The two banner comments (mascot
motion foundation, chart geometry) became `//!` module docs. (4) Doc links that crossed a module
boundary were qualified, e.g. ``[`run`](crate::runtime::run)``. Six signatures were rewrapped because
`pub(crate) ` pushed them past 100 columns.

**The gate ran late, and the static checks earned their keep.** The environment had no Rust
toolchain when the split was made (no `~/.cargo`, no rustup, mise carrying only go/node), so the
whole refactor was done and verified *without a compiler* — then the toolchain was installed and the
real gate run. Worth recording what each pass caught, because the ratio is the interesting part.

Verified mechanically, before any compilation:

- **Nothing lost or duplicated**: a multiset diff of every non-blank line, old vs new, with the
  visibility prefix normalised away. The only residue is the four deliberate edits above.
- **Name resolution**: every crate item referenced in a module is defined there or imported; every
  import is referenced (the `-D warnings` failure mode), with trait-by-method-call imports
  (`UnicodeWidthStr`, `SeedableRng`, the three `*SemanticsExt`) checked by their call sites.
- **Visibility**: no private item reachable only across a module boundary, and no `pub(crate)` item
  exposing a more-private type — which is why `MascotMotion`/`MascotIgniter` had to become
  `pub(crate)`: they appear in `Mascot`'s field types, and a private type behind a `pub(crate)` field
  is a `private_interfaces` warning, i.e. an error under the gate.
- **rustfmt shape by emulation**: `use` blocks re-sorted and wrapped following the ordering the
  repo's own formatted files demonstrate (`super` < `crate` < plain idents; `ratatui::Terminal`
  before `ratatui::backend::…`), no code line over 100 columns, no double blank lines.

That pass caught three real defects, each of the same species: **name-scan import inference is
exactly wrong for extension traits and for aliases.** `Table as DbTable` landed in `ui/metrics`
(where it matched *ratatui's* `Table`) instead of `fetch`; the three `*SemanticsExt` traits were
dropped from `fetch` entirely (their only use is a method call, invisible to a name scan); and
`UnicodeWidthChar` was imported into `chart`, where only the `str` trait is used.

The compiler then found a fourth species the scan could not: **enum variants that share a name with
a type.** `Route::MetricDetail { .. }` and `Update::Waterfall { .. }` made `MetricDetail` and
`Waterfall` look imported-and-used in four modules where the *type* was never named — plus a
mirror-image case where `MetricDetail` was dropped from a module header but the module's own tests
constructed it through `use super::*`. Total compiler-only findings: 6 unused imports, 3 redundant
inline `use`s in tests, 2 genuinely missing test-scope imports. Zero behavioural failures.

Gate: `cargo fmt --all --check` clean, `cargo build --workspace` clean, `cargo clippy --workspace
--all-targets -- -D warnings` clean, `cargo test --workspace` green — **95 tests in `imbh-tui`**, all
of them the pre-split tests, unmodified, now distributed across 21 module test blocks. No dependency
change, so the footprint budget is untouched.

The takeaway for the next mechanical refactor: static checks got ~85% of the way and were the only
thing keeping the work honest while no compiler existed, but the last 15% — anything where a *name*
means two things (variant vs type, alias vs original, trait vs method) — is compiler territory. Do
not ship a split of this size on inference alone.

## 2026-08-01 — MCP over stdio: the transport that had to admit it was a transport

The TODO item read like a small one: `mcp::handle` is already transport-agnostic
(`bytes + headers → Reply`), so stdio is "a newline-delimited read/write loop plus a data-access
flag". Two of those three premises held. The third did not, and it is the finding worth keeping.

**`handle` was HTTP-shaped in one place, and it was the load-bearing place.** The stateless
`2026-07-28` revision requires a modern request's method, protocol version, and tool name to appear
as `MCP-Protocol-Version` / `Mcp-Method` / `Mcp-Name` headers *and* to agree with the body — a rule
that exists so a proxy can route without parsing JSON. `validate_modern` enforced it for every modern
request. Over a pipe there are no headers, so the very first `_meta`-carrying message from a modern
stdio client would have been refused `-32020` for a missing header it could never have sent. The
header mirror is a **Streamable HTTP** rule, not a protocol rule, and the dispatch had no way to say
so. Fix: `handle(db, bytes, &Transport)` where `Transport` is `Http(Headers)` or `Stdio`, and the
whole mirror block is skipped when there is no header channel. The version check still runs on both,
reading the version from `_meta` alone over stdio. This is the kind of thing "transport-agnostic"
hides: the *signature* was agnostic, the *behaviour* was not, and only a second transport could tell.

**Lifting the module was forced by dependency direction, and paid for itself.** `imbh-tui` may not
depend on `imbh-server` (§12), so the protocol moved to a new `imbh-mcp` crate that both sit above:
`imbh ← imbh-mcp ← {imbh-server, imbh-tui}`. `tools.rs` reached into its host crate for
`batches_to_json`, `stats_json`, and `offload`, so those came along and `imbh-server` now re-exports
them — which is the right direction anyway: `query_sql` and `POST /api/query` share a row serializer
*because* they must never render the same rows two ways, and that invariant now lives below both
callers rather than beside one of them. `imbh_server::mcp` survives as `pub use imbh_mcp as mcp`, so
the published API surface did not break.

**The `--url` mode is where the header rule came back.** Forwarding a stdio session to a running
`imbhd` means the message arrives with no headers and must leave with the exact mirror the daemon
enforces — so the proxy *derives* the headers from the body it is about to send (`method`,
`params._meta` version, `params.name`, Base64-sentinel-encoded when the tool name is not wire-safe).
That is a satisfying closure: the transport that has no headers is also the one that has to
manufacture them. Encoding needed a new `encode_header_value` mirroring the existing decoder, and its
test is a round trip through both.

**The HTTP client is 150 lines of `std::net`, deliberately.** A client crate would have pulled hyper's
client stack into the TUI binary to send a fixed method to a fixed path with a known-length body and
no redirects, compression, TLS, or pooling. One buffered `POST` per message with `Connection: close`
(so reading to EOF is correct framing-independently) costs a TCP handshake per tool call against a
loopback daemon — not a cost worth a subtree. `Content-Length` and `chunked` are both handled; the
latter only because something might sit in between.

Smaller decisions worth remembering:

- **Blocking `std::io` inside an `async fn` is correct here**, and the comment says why: the loop is
  the only thing on its runtime, and a `Db` query is blocking parquet/tantivy I/O from end to end, so
  concurrent requests would contend for the same disk rather than overlap. Sequential is not a
  simplification, it is the right shape.
- **A malformed line must not end a session.** One bad message from a client is a parse error and a
  continue; EOF and `BrokenPipe` are both `Ok(())`, because "the client went away" is how a stdio
  session ends normally. A notification writes *nothing* — not an empty line.
- **stdout is the transport.** The one-line "serving MCP over stdio from …" banner goes to stderr,
  where clients collect it as the server's log.
- The TUI's argument parsing moved into `cli.rs` (a `Mode` enum) so the combinations that must be
  refused — `--url` without `--mcp-stdio`, two sources at once, `--ascii` in server mode — are tests
  rather than prose. `--db` and `--help` came along.

Footprint: unchanged. `imbh-mcp` is `imbh` plus `serde_json` and `base64`, both already compiled
under DataFusion; the facade stays at 275 crates, and `imbh-server` (300 → 301) and `imbh-tui`
(312 → 313) each gained exactly one *workspace* crate and zero third-party ones. Gate green: fmt,
build, clippy `-D warnings`, `cargo test --workspace`, plus the footprint gate (275 crates, 32.9 MiB
`imbhd`, RSS soak within budget). New coverage: `crates/imbh-mcp/tests/stdio_e2e.rs` — six tests
driving real sessions over in-memory pipes, including a hand-rolled fake `imbhd` on loopback that
asserts the synthesized header mirror *from the receiving side*, which is the only place that
particular bug could have been caught.

## `service.name` is groupable now, not only filterable (2026-08-01)

`SqlParams::attr_field` is the single funnel every typed builder uses to turn a group/filter key
into SQL. It had two branches — configured `Promote` key → `CAST("key" AS VARCHAR)`, everything else
→ `json_get_str(attributes, $key)` — and `service.name` fell into the second one. But `service.name`
is a *resource* attribute, lifted at ingest into the built-in `service` column; it is never a record
`attributes` entry. So the expression was NULL on every row.

The failure mode is the interesting part: **filtering** by it matched nothing, which is at least
plausible to notice, but **grouping** by it produced one `{"service.name": ""}` series with every
count merged — a well-formed, confident-looking answer. Nothing is wrong at the SQL level (a missing
attribute is a legitimate NULL, and NULLs group together), so there is no error to surface. The MCP
tool descriptions had been written *around* the bug ("These are attributes, not `service.name` — to
split by service, call once per service with the `service` filter"), which is how a data bug becomes
a documented API constraint.

The fix is a third branch, `builtin_column`, ahead of the other two: `service` (the column name) and
`service.name` (the OTel semantic convention) both emit `CAST(service AS VARCHAR)`, in `attr_field`
and its numeric twin `attr_num_field`. Ordering matters and is deliberate — the built-in branch wins
over `Promote`. A promoted key can never shadow `service` (`promoted_columns` drops reserved names),
and a promoted `service.name` column would be built by `lookup_promoted` over record `attributes`
and hence be all-NULL for exactly the same reason, so the built-in column is strictly the better
answer for either spelling. That ordering also means the fix needs no storage change: adding
`service.name` to `RESERVED_COLUMNS` would have been the "tidier" fix and would have altered the
on-disk schema of any DB that promotes it, for no gain.

Because `attr_field` is the funnel, one branch fixed every caller at once: `LogsApi::volume_by`,
`TracesApi::span_metrics`, the four metric group-by paths, and every `attr_eq`/`attr_exists`/
`attr_in`/`attr_regex`/`attr_num` matcher on logs and traces. It also *deleted* two workarounds that
had grown around the gap — the `key == "service"` special case in `metrics::label_cond` and the
`service.name` special case in `AttrsApi::values`, both now just `attr_field` calls. Duplicated
special cases at the call sites are a good signal the funnel is missing a branch.

`imbh-lgtm`'s PromQL path is unaffected: it groups over materialized label sets
(`metric_labels_from_batch`, which already folds `service` in), not over `MetricQuery::group_by`.

Testing: the existing pin in `crates/imbh-server/tests/mcp_e2e.rs` was inverted to assert resolution
for both spellings, and a new `grouping_by_service_name_splits_the_series_per_service` seeds *two*
services — the shared `seeded_db` only ever ingests `cart`, and a single-service fixture cannot tell
a working group-by from the collapsed one, which is why the bug survived the original smoke test.
`logs_group_and_filter_by_service_name` (`crates/imbh/src/lib.rs`) covers the same ground library-side
across group-by, `attr_eq`, and `attr_exists`. The MCP `group_by` descriptions now say `service.name`
is groupable.

Footprint: unchanged (no dependency touched). Gate green: `cargo fmt --all --check`,
`cargo build --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`,
`cargo test --workspace`.

## v0.3.0 lost its GitHub Release: create-then-upload is fatal under immutable releases (2026-08-01)

The v0.3.0 CD run built and smoke-tested all five targets, pushed the multi-arch GHCR image, and
then failed in `publish`. Attempt 1 of that job shows the whole mechanism in two lines:

```
https://github.com/moriyoshi/imbh/releases/tag/v0.3.0        <- gh release create succeeded
HTTP 422: Cannot upload assets to an immutable release.      <- gh release upload then failed
```

`release.yml`'s `publish` step did `gh release create` (no assets) followed by `gh release upload`.
That is a mutable-Release assumption. GitHub's immutable releases freeze a Release the moment it is
**published**, so the create published an empty, frozen Release and the upload could never land.

The second, irreversible half followed from a reasonable reaction to that failure: the Release and
the tag were deleted so a clean release could be cut from a fresh tag. But an immutable release
reserves its tag name **permanently** — the reservation survives deleting the Release, deleting the
tag, and turning immutability off (it is anti-repository-resurrection protection). Re-running
`publish` then produced the terminal error:

```
HTTP 422: Validation Failed - tag_name was used by an immutable release
```

`v0.3.0` can never have a GitHub Release in this repository again. Everything else about 0.3.0
shipped: all 12 crates on crates.io, `ghcr.io/moriyoshi/imbh:0.3.0` + `:0.3` + `:latest`. Only the
per-platform archives have nowhere to live. Recorded as-is rather than papered over with a no-op
0.3.1: the version stays where it is, and `README.md` / `CHANGELOG.md` now say plainly that 0.3.0
has no archives and point at the container image, `cargo install`, and the release commit.

Fixes, all in `release.yml`:

- `publish` creates a **draft**, uploads into it, and flips `--draft=false` only once every asset has
  landed. A draft is mutable, re-runnable, and deletable — it carries none of the hazard. `--latest`
  is now explicit at publish time so a prerelease cannot displace the stable release.
- It also branches on the existing Release's state: reuse a draft, refuse a published one with an
  error that says *do not delete it*, and annotate a failed create with what a burned tag name means.
- `meta` checks the Release slot before the build matrix starts, so the unrecoverable case costs
  seconds instead of an hour of fat-LTO builds across five runners.

The generalisable lesson is about failure-recovery instincts, not YAML. Under immutability the
familiar "delete it and retry" is the one move that cannot be undone, and it is exactly what a
half-finished publish invites. Any step that publishes something frozen must therefore be the *last*
step, and the failure message at that step has to tell the operator what not to do — by the time
they are reading it, the destructive option looks like the obvious one.

## Sticky waterfall: the obvious fixpoint cycles, and depth broke two things nobody had rendered (2026-08-01)

The trace detail's waterfall now pins the selected span's scrolled-off ancestors at the top of the
pane, dimmed, so scrolling into a deep trace no longer strands you on a `db.query` at indent 3 with
no way to see what it hangs off. `s` toggles it, bound in the trace-detail arm of `handle_detail_key`
rather than globally; it is on by default and the hint line reads `s sticky:on|off`. The state is an
`App` field alongside `show_mascot` and deliberately *not* in `NavEntry` — a display preference should
survive Back/Forward, not be captured by it.

The shape of it: a pure `sticky_layout(rows, cursor, viewport, enabled) -> StickyLayout { pinned,
offset, height }`, so the whole geometry is unit-testable without a terminal, and the renderer just
splits the block's inner rect into a pinned `Paragraph` and an offset-pinned `List`. The pinned block
is capped at a third of the viewport, keeping the *innermost* ancestors — the nearest context is worth
more than the root when the chain will not fit. `WaterfallRow` gained `parent_row` (a *row* index, not
a span id, and only ever walked upward: spans come back `ORDER BY start_time`, not as a tree, so clock
skew across services can put a parent below its child — walking strictly upward is what makes the walk
both correct and cycle-safe without a visited set) and lost its pre-baked `prefix` in favour of
`marker`/`indent`/`name`, because horizontal scrolling needs the raw text at draw time. Three findings
worth keeping.

**The obvious formulation of "sticky" does not converge.** The natural reading — pin the ancestors of
the *topmost visible row* — is a fixpoint problem, because pinning rows shrinks the window, which
moves the offset, which changes whose ancestors you are pinning. That function is **not monotone**.
With rows `A`, `B` (child of `A`), `C` (child of `B`), `D` (a fresh root), the pinned count runs
`1 → 2 → 0 → 1 → 2 → 0 …` with period 3, so any iteration budget returns a budget-dependent answer and
the block visibly flickers as the user holds `↓`. Anchoring on the **cursor's** ancestor chain instead
makes it monotone: more pinned rows ⇒ shorter window ⇒ larger offset ⇒ weakly more ancestors above it,
so iterating from zero reaches the least fixpoint on `{0..=cap}` in at most `cap + 1` steps. The two
anchors describe the same pane *only* because the offset is stateless here (a fresh `ListState` each
frame ⇒ either `offset == 0` or the cursor sits on the last visible row — one degree of freedom). If
that offset ever becomes stateful, the anchor has to be revisited; the doc comment says so.

The general lesson: when a layout quantity feeds back into the geometry that produces it, the choice
of anchor is not cosmetic — it decides whether the recursion is a fixpoint or an oscillator. Picking
the anchor that makes the function monotone is worth more than any iteration cap. A counterexample
test (`sticky_layout_is_stable_across_a_subtree_boundary`, asserting the invariants at *every* cursor
value over exactly that shape) is the regression guard, and a 20 000-case fuzz over random forests
confirmed cursor-visibility, pinned-above-window, and the cap.

**Depth broke two things that had simply never been rendered.** The waterfall had only ever been
exercised at depth ≤ 4. The indent shares the 20-cell name column at two cells per level, so at depth
8 a name had four readable cells — and `clamp_field` truncated with a hard-coded `…`, meaning
`--ascii` mode leaked a Unicode glyph the moment a trace nested deep enough. The `--ascii` sweep never
caught it because no fixture nested that far, and it runs at 48×10 where the pane is too short for
sticky to engage at all. So: the indent is capped at 5 levels (≥ 10 name cells always, with the
hierarchy the cap gives up now supplied by the pinned ancestors), and `clamp_field` became
`clip_field(text, width, offset)`, which marks clipping with ASCII `<`/`>` written *into* an edge cell
rather than stealing width — the field stays exactly `width` cells, which is what the bar-axis
alignment invariant rests on, and the ASCII leak disappears by construction rather than by a flag.

**The pty caught what 116 unit tests could not.** The name column also scrolls horizontally, following
the cursor. Two failure modes only showed up driving the real binary against a `gen-demo-db` database
in a sized pty (a small Python `pty.fork` + `TIOCSWINSZ` harness; `script` gives a 0×0 window and the
TUI renders nothing). First, pinned ancestors are the *shallowest* rows and so usually the *shortest*
names — sharing the cursor's offset outright scrolled them clean out of their own field and the
context band rendered blank, i.e. the feature silently deleted itself exactly when it mattered. Fixed
by clamping every row to its own maximum useful offset, so the column still moves as one but no row
scrolls into emptiness. Second, and worse: scrolling the column shifted the *indent* off the left
edge, so the whole trace's shape vanished the instant the cursor landed on a long name. `WaterfallRow`
now keeps `indent` and `name` apart and only the name scrolls — the tree is never negotiable. Both
bugs were invisible to substring assertions over a `TestBackend`, because both produced perfectly
well-formed rows; they were only wrong *as a picture*. Render tests prove structure, not legibility.

`examples/gen-demo-db` gained a deep trace per step so any of this is reachable in the demo data: a
checkout entry chaining `--deep-hops` (default 5) service hops, 23 spans nested 11 deep, names longer
than the name column, and ~1 in 4 failing at the innermost `db.query` so the ERROR propagates up the
whole pinned chain. It reuses every existing `Role`, so `logs_body`/`log_line` needed no changes.

Verification: 116 unit tests in `imbh-tui` (16 new — the cycling counterexample, the cap keeping the
innermost ancestors, cycle-safe `ancestor_rows`, degenerate viewports, `clip_field` exactness at every
offset including wide-glyph boundaries, the indent cap, the indent-never-scrolls guard, and two
`TestBackend` renders asserting the pinned rows carry `Modifier::DIM` while the cursor row does not
and that the whole sticky render stays ASCII). The pre-existing
`trace_detail_waterfall_scrolls_to_keep_the_span_cursor_visible` passes unchanged — its 40 spans are
all roots, so sticky is provably inert on a flat trace. Full workspace `fmt`/`build`/`clippy`/`test`
green; no dependency changes, so the footprint gate is a no-op.



## Sticky waterfall follow-up: dim is a bet on terminal support (2026-08-01)

Three user-reported defects on the pinned rows, all one root cause and one consequence of it.

**"Lines in the sticky spans aren't rendered dimmed while the name and status are."** The pinned rows
carried `Modifier::DIM` on the whole line, so the first instinct — a code bug leaving the bar
unstyled — was wrong. Dumping `buffer[(x, y)].modifier` across a whole pinned row showed every cell
*including* the bar carrying `DIM`, and the first scrolling row carrying none. The attribute was
being set correctly and the terminal was ignoring it: many terminals draw box-drawing characters
procedurally, from a built-in geometry renderer rather than the font, and that path honours the cell's
foreground colour but not the faint attribute. Ordinary text (name, duration) dims; `━` does not.

The fix rides three channels instead of one: an explicit `fg` (`DarkGray`, which the geometry renderer
does honour; error rows keep `Red`, since a failing ancestor is still worth seeing), a lighter bar
glyph (`Waterfall::light_marker` — `─`/`-` against `━`/`#`), and `DIM` for terminals that honour it.
The generalisable rule: **an attribute-only visual distinction is a bet on terminal support.** Carry
anything load-bearing on colour or on the glyph itself, and keep the attribute as a bonus.

Choosing the lighter glyph required checking it against the crate's EAW rule, which turned up a
**pre-existing latent bug**: `━` U+2501, the bar glyph the waterfall has always used, is East-Asian-
width *ambiguous* (`width` 1, `width_cjk` 2). The project's own pitfall list forbids exactly this for
width-measured text — under a CJK locale each bar cell renders two columns wide and the axis
desyncs. The substitution is safe because `─` U+2500 is ambiguous in precisely the same way, so the
two can never disagree with each other; but the underlying glyph choice predates this work and is
recorded as a separate finding rather than fixed here.

**"Draw divider (underline) at the bottom of the sticky span rows"**, reversing the earlier
"dim only, no rule" call. Implemented as `UNDERLINED` on the *last pinned row* rather than a `───`
rule row: the rule would cost a viewport row out of a pane whose entire problem is being too short,
and the underline already sits exactly on the boundary.

**"The divider doesn't stretch out to the right border of the pane."** A waterfall line ends after its
duration column, several cells short of the pane's inner width, and `Paragraph` does not pad — so the
styled span ended there and the rule stopped short, which does not read as a rule. Fixed by padding
each pinned row to `inner.width` before styling. Any full-width row styling has this requirement.

The test lesson is the sharpest one here: the original render test asserted on `buffer[(2, y)]` — a
single cell in the *name* column — so it passed while the bar rendered at full intensity, and would
have passed just as happily through all three defects. **A styling assertion has to quantify over
every cell of the row it claims something about.** The test now checks `DIM` across all non-blank
cells, the explicit `fg`, the light-vs-heavy bar glyph, and the divider spanning every column between
the borders while the pane title above it stays clean.

Also worth recording: a crude ANSI replay over a pty capture is *not* a substitute for `TestBackend`
when the question is about attributes. Ratatui writes only changed cells, so SGR state carries across
cursor moves in ways a naive parser mis-attributes — the pty dump reported underline on rows that the
exact buffer assertions prove are clean. Use the pty for *layout* (it caught the blank pinned names
and the scrolled-away indent earlier); use `TestBackend` for *style*.

## Waterfall indent goes relative to the pane, not the trace root (2026-08-01)

Scrolled deep into a subtree, every visible waterfall row carried the same leading indent. It
distinguished nothing between the rows on screen and spent the whole name column saying it — at the
capped depth of 5, ten of twenty cells on every single row.

`visible_indent_base` now takes the minimum indent across the *rendered* rows (the pinned ancestors
plus the scrolling window) and `render_waterfall` subtracts it, so the outermost visible span sits
flush against the name column and the rows below keep only their offsets relative to it. Deep names
gain those ten cells back; measured against the demo database, `<kout-ser>` became
`<s.checkout-ser>`, and the topmost row went from nine readable cells to twenty.

Anchored on the shallowest rendered row rather than literally the topmost one. They are the same row
in the usual case — the pinned block is the cursor's ancestor chain, ordered outermost-first — but a
shallower sibling scrolling into the window underneath would otherwise require a negative shift, and
clamping that at zero would draw two genuinely different depths in the same column. Taking the
minimum keeps every relative distinction on screen intact. That is the whole subtlety, and it has its
own test (`the_indent_base_is_the_shallowest_row_not_merely_the_first`).

This composes with the earlier `WATERFALL_MAX_INDENT` cap rather than replacing it: the cap bounds
the absolute worst case (a pathologically deep chain still cannot eat the column), the relative base
handles the common one (a normally-nested trace scrolled into). Worth noting that the cap alone was
the wrong instinct — it treats depth as the problem, when the problem was only ever *shared* depth
among the rows you can actually see.

The draw-time parameters (`bar_cells`, `name_offset`, `indent_base`) now travel as one
`WaterfallView` named-field literal instead of a fourth and fifth positional argument, following the
`SpanSpec` shape already used in `gen-demo-db` for exactly this reason.

119 unit tests in `imbh-tui` (3 new). Verified against a `gen-demo-db` database in a pty.

## Removing the indent cap: clamp the view, not the fact (2026-08-01)

`WATERFALL_MAX_INDENT` is gone. `WaterfallRow::indent` now stores the span's true depth, and the only
remaining bound is `WATERFALL_MIN_NAME_W` (10) — a floor on the *rendered* indent, applied after
`visible_indent_base` has subtracted the shallowest depth on screen.

The cap was the wrong instinct, and the previous entry said so before this change made it actionable:
it treated *depth* as the problem when the problem was only ever depth **shared** by the rows you can
see. Flattening at level 5 in the row model paid a permanent cost — every trace past that depth lost
its shape — to solve a case the relative base already handles.

But removing it outright would have been wrong too, and checking that was the whole job. Without the
cap, a window that happens to span the root and a deep leaf together renders `depth * 2` cells of
indent into a 20-cell column: at depth 16 that is 32 cells, the name gets `clip_field(name, 0, ..)`,
and the row draws as pure indent with **no name at all**. Not a crash — the `.min(WATERFALL_NAME_W)`
clamp was already there — just a silently nameless row. That case is reachable in ordinary data: a
start-time-ordered waterfall puts a shallow sibling directly under a deep subtree, which is exactly
what drops the base back to zero. So the bound moved rather than vanished, from the stored fact to
the drawn quantity.

The general shape is worth keeping: **prefer clamping a derived, view-dependent quantity at draw time
over truncating the underlying fact at build time.** The model should not lose information the view
merely cannot show today — the view's constraints change (a wider pane, a relative base, a different
column layout) and a truncated model cannot recover what it threw away. Same discipline as storing
the bars as duration *fractions* and resolving cells at draw time, which this crate already had.

Verified against `gen-demo-db --deep-hops 8` (38 spans, depth 17) in a pty: at the top of the pane
true depth now renders where it used to flatten at level 5; scrolled to the bottom, where a shallow
sibling pulls the base to zero and the visible rows span more than five levels, the floor engages and
every row still keeps its ten name cells. 119 unit tests in `imbh-tui`; the cap's test became
`depth_is_stored_uncapped_and_floored_only_at_draw_time`, which asserts the model keeps every level
and that no rendered row can lose its name.

## Correction: the ambiguous bar glyph is not a bug (2026-08-01)

The 2026-08-01 sticky-waterfall follow-up entry above records, as a "pre-existing latent bug", that
`━` U+2501 is East-Asian-width ambiguous and therefore violates this crate's own EAW rule. That
finding is **wrong** and is retracted here; no code changed.

The user's one-line objection was decisive: *"Don't we already use the symbols for pane frames?"* We
do. Probed, every glyph in ratatui's default `border::PLAIN` set — `│ ─ ┌ ┐ └ ┘` — is ambiguous in
exactly the same way as the bar, `width` 1 against `width_cjk` 2. So the bar is not an outlier
violating a rule the rest of the UI obeys; it is consistent with every bordered pane on the screen.

Which means the framing was incoherent from the start. If a terminal really renders ambiguous as two
cells, the frames break identically and the entire UI is already unusable — fixing the bar alone
would buy nothing. And if it does not, nothing was broken. ratatui's cell grid assumes
ambiguous-as-narrow, as does essentially every terminal UI framework; that is why CJK users generally
configure ambiguous-width to 1, and it is what `--ascii` exists for.

The mistake was over-applying a documented rule past its actual scope. The LTM pitfall said
"decorative glyphs must be EAW-unambiguous", with examples (`● • · ▼ ▶`) that are all accent glyphs
inside strings *our own* alignment arithmetic measures — the menu-bar brand, hint separators, tree
markers. Box-drawing chrome that ratatui lays out on its grid was never the target. The tell I walked
straight past: when a "violation" turns out to be everywhere in a codebase that documents the rule it
supposedly violates, the rule almost certainly means something narrower than the reading that
produced the finding. Checking the *neighbours* of a suspected violation is cheap and would have
caught this before it was written down as a defect.

The rule in `LTM/imbh-tui-and-gen-demo-db.md` has been split in two so the next reader cannot repeat
it: an explicit "box-drawing is the deliberate exception, do not fix it" entry, and the original rule
restated with its real scope. The probe result is recorded there too, since it is the useful part of
the exercise: the whole box-drawing *and* block-element repertoire is ambiguous (`━ ─ ═ ┄ █ ▄ ▀ ▂ ▁
■ — ― ‾`), and the only safe bar-ish glyphs are `▬ ╌ − ⎯ ⸺ ¯ ‗` plus ASCII — so there is no drop-in
replacement that preserves the look, which is the second reason not to have gone down this road.

## The log→trace jump stopped at the trace list: an intent with no carrier (2026-08-01)

**Symptom.** Enter on a log entry's detail screen navigated to the Traces *list* instead of the
selected log's trace detail.

**Cause.** The jump is a three-hop dance and the "open the detail" intent had no carrier that
survived it. `handle_detail_key` set `focus_trace_id` and called `switch_screen(Traces)`, which
issues a list refresh; the list result then runs `focus_select_trace` + `request_waterfall`; the
waterfall result finally arrives. `App::pending_trace_open` — the flag the Traces-list Enter uses
for "open as soon as the fetch lands" — is deliberately cleared by *both* intermediate hops
(`switch_screen`, so an intent cannot outlive its list; `request_waterfall`, so an Enter meant for
the trace the cursor just left is abandoned). Correct for that flag, fatal for this one: nothing
was left set by the time the waterfall landed, so the view stayed on the list. The exemplar→trace
jump from a metric detail (`keys.rs`, the `nearest_exemplar_trace` arm) had the identical bug.

**Fix.** A second, focus-scoped intent `App::focus_trace_open`, set alongside `focus_trace_id` by
both jumps and cleared only where the focus itself is abandoned (`clear_trace_focus`, now used by
`move_selection` / Home / End / a non-Traces `switch_screen`, plus `restore_nav`). It is consumed by
`App::open_focused_trace_detail` from two places in the run loop: right after the list result (the
waterfall may still be *retained* from an earlier visit, in which case no fetch is issued and no
`Update::Waterfall` would ever arrive) and on the waterfall arrival otherwise.

**Design note.** `open_focused_trace_detail` opens *without* pushing history — the Traces list is a
way station the user never asked for, so one Enter stays one `←` back to the log detail. That is why
`open_trace_detail` grew an `open_trace_detail_inner(record_history)` core rather than being reused
directly.

**Lesson.** "Clear this intent on navigation" and "carry this intent across a navigation" cannot
share one flag. When a drill-down is implemented as *switch screen, then let the data pull you the
rest of the way*, the intent has to be scoped to the thing it is waiting for (here: the trace focus),
not to the screen it happens to pass through.

## The waterfall name column goes flat: the indent is removed (2026-08-01)

**Change.** Span names in the Traces waterfall are no longer indented by depth. Every row's name
starts at the first cell of the `WATERFALL_NAME_W` (20) column and gets all of it, at any depth.

**What went with it.** The whole draw-time indent apparatus, since none of it had another consumer:
`WaterfallRow::indent` (and the `depth` counter in `build_trace_detail` that fed it — the ancestor
walk stays, it is what flags `malformed`), `WaterfallView::indent_base`, `visible_indent_base`,
`row_indent_cells`, and the `WATERFALL_MIN_NAME_W` floor. `row_name_offset` / `name_offset` lose
their `indent_base` parameter: the scroll window is now unconditionally the full column.

**What still carries the tree.** `WaterfallRow::parent_row` and the sticky pinned-ancestor block are
untouched — the ancestor chain is walked from `parent_row`, never from the depth counter, so pinning,
the divider underline, and the de-emphasis all behave exactly as before. The bars themselves still
show containment. This is why the removal is a rendering change and not a data-model loss.

**Consequences worth noting.** The two earlier entries above — *Waterfall indent goes relative to the
pane* and *Removing the indent cap* — describe machinery that no longer exists; their general lessons
(clamp the view, not the fact; a shared prefix on every visible row distinguishes nothing) survive the
code that occasioned them. Two facts they turned on are now moot rather than wrong: the axis-alignment
invariant no longer depends on `indent + name` summing to a fixed field, because there is no indent to
sum, and deep rows can never lose name cells to a prefix.

**Tests.** `depth_is_stored_uncapped_and_floored_only_at_draw_time` →
`the_name_column_is_flush_at_every_depth` (asserts the field starts with the name and is exactly
`WATERFALL_NAME_W` cells at every depth of a 12-deep chain, and that `ancestor_rows` still walks the
full chain); `the_indent_never_scrolls_away_under_a_long_name` →
`a_short_name_is_never_scrolled_into_blankness` (the per-row clamp is what that test now guards, and
it is the half of the behaviour that survives). The three `indent_base` tests are gone with the
function. `trace_detail_scrolls_the_name_column_to_the_cursors_span_name` now expects
`| work-0-with-a-long->`: the leaf name that used to lose five cells to the prefix.

## The name column stops scrolling: ellipsis instead (2026-08-01)

**Change.** The waterfall's name column no longer scrolls horizontally to chase the cursor row's
name. A name that fits is shown whole; one that does not is cut with a trailing ellipsis. Follows the
flat-column change above.

**`clip_field` → `fit_field`.** `clip_field(text, width, offset)` was used by nothing but the
waterfall, so the offset went with the feature rather than surviving as dead generality:
`fit_field(text, width, ellipsis)`. The bulk of the old function was the marker-overwrite pass — a
cell-by-cell rebuild so a `<`/`>` landing on the second half of a wide glyph replaced the glyph whole
and kept the field exact. Truncation with a *reserved* marker needs none of it: measure the ellipsis,
keep `width - marker` cells of text, pad the one cell an unfittable wide glyph orphans, append.
Roughly 80 lines to 40, and the exact-width invariant is now obvious by construction instead of
resting on that pass.

**`WaterfallView` is gone.** With `name_offset` removed it held only `bar_cells`, so it was a
one-field wrapper standing for a "growing argument list" that had in fact shrunk twice in a row
(`indent_base`, then `name_offset`). `render_waterfall(waterfall, bar_cells)`.

**Where the ellipsis comes from.** `Waterfall::ellipsis: &'static str`, set from the `ascii` flag next
to `marker`/`light_marker` — `"..."` / `"…"`, the same pair `Glyphs::ellipsis` already uses. It rides
the waterfall rather than being read from `Glyphs` at draw time because `render_waterfall` is the one
renderer with no `Glyphs` in scope, and the mode is already known where the other two glyphs are
picked. `…` is EAW-ambiguous, and this *is* a field our own arithmetic measures — the narrow reading
of the EAW rule, not the box-drawing exception. Kept anyway: it is the UI's existing ellipsis, and
`UnicodeWidthStr::width` giving it 1 cell is the same ambiguous-as-narrow assumption every bordered
pane on the screen already makes (see the retraction entry above). `--ascii` remains exact.

**What this buys.** The pane is now genuinely stateless left-to-right: the name column shows the same
thing wherever the cursor is, so the pinned ancestors and the scrolling rows are laid out identically
and moving the cursor no longer shifts text under the user's eye. The clamp that used to keep short
names from scrolling into blankness, and the whole class of bug it existed for, are gone with the
offset. The tail of a long name is still one row away in the span summary pane.

**Tests.** `format`: `clip_field_*` (three) → `fit_field_pads_and_truncates_by_display_width` and
`fit_field_is_exactly_width_cells_for_every_input` (sweeps text × width `0..=8` × both markers,
including widths too narrow for the marker, where it is dropped rather than filling the field).
`waterfall`: `name_offset_follows_the_cursor_row` / `a_short_name_is_never_scrolled_into_blankness` →
`a_long_name_is_truncated_with_an_ellipsis_and_a_short_one_is_left_alone` and
`the_ellipsis_follows_the_ascii_mode_the_trace_was_built_in`. `ui::traces`:
`trace_detail_scrolls_the_name_column_to_the_cursors_span_name` →
`trace_detail_truncates_long_span_names_with_an_ellipsis`, which asserts the column is *identical* at
two cursor positions — the property that replaced the scroll.

## Session summary: the waterfall name column lost two features and got simpler (2026-08-01)

Consolidates the two entries above, which record the changes themselves. This one is the arc and what
is worth carrying forward.

**What was done.** Two user-directed removals from the Traces waterfall's name column, in sequence:
depth indentation, then horizontal scrolling (replaced by ellipsis truncation). Net: **-235 lines**
across `waterfall.rs`, `format.rs`, `ui/traces.rs`, `ui/mod.rs`, with the test suite steady at 117
passing in `imbh-tui` and the full workspace gate clean.

**What came out with them.** `WaterfallRow::indent`, `WaterfallView` (whole struct), `WATERFALL_MIN_NAME_W`,
`visible_indent_base`, `row_indent_cells`, `row_name_offset`, `name_offset`, and `clip_field`'s entire
offset/edge-marker machinery. Every one of these existed *only* to serve the two features; none had a
second consumer. That is the tell worth remembering: when a feature is removed and nothing else in the
crate wants the machinery it accumulated, the machinery was never general — it was one feature spread
across four files.

**Finding: the design weight around a feature is not evidence the feature is needed.** The removed
indent had three journal entries behind it (relative-to-pane basing, cap removal, the min-name floor),
each a genuinely careful piece of reasoning — the relative-indent argument about anchoring on the
*shallowest* rendered row rather than the topmost is still correct, and still subtle. All of it was in
service of fitting a tree prefix into a 20-cell column that the trace's other affordances (pinned
ancestors from `parent_row`, the bars) already conveyed. Sunk design effort reads as justification
after the fact; it isn't. The same held for the scroll: its cleverest part, the per-row clamp keeping
short names from scrolling into blankness, was a fix for a problem the feature itself created.

**Finding: removals compose, so re-check the survivors after each one.** Dropping the indent left
`WaterfallView` with two fields, which still justified the struct; dropping the scroll left it with
one, which did not. Likewise `clip_field` was still load-bearing after the first removal and dead
generality after the second. Neither collapse was visible from inside its own change — both only
showed up on the second pass. Worth a deliberate look at what a removal *leaves* rather than only at
what it takes.

**Finding: the invariant survived both changes, and it was always the real constraint.** Everything
left of the first `|` must sum to a fixed cell count or the bars stop lining up. The indent honoured
it by sharing the column; the scroll by overwriting edge cells rather than stealing width; the
ellipsis now by reserving its own cells. Three quite different mechanisms, one unchanged requirement —
which is why the axis-alignment test needed no rework across either removal, only the removal of the
now-meaningless `offset` loop. When a test survives two feature deletions untouched, it was testing
the right thing.

**Open point, deliberately accepted.** The Unicode ellipsis `…` is EAW-ambiguous and now sits inside a
field this crate's own arithmetic measures — the narrow scope the EAW rule actually targets, not the
box-drawing exception. Kept because it is the UI's existing `Glyphs::ellipsis`, and because treating
ambiguous as one cell is the assumption every bordered pane already makes (see the retraction entry
above). `--ascii` mode uses `...` and stays exact. If a CJK-configured terminal ever shows a truncated
row's bar one cell right of its neighbours', this is the cause and the fix is an ASCII marker in both
modes.

## Backspace walks the screen series, not the visit history (2026-08-01)

**Ask.** Bind Backspace to "go to the previous screen in the screen series it belongs to" —
explicitly *not* the existing `←`/Esc, which pop the browser-style visit history.

**Model.** Each `Screen` owns a fixed chain of views: `Metrics → MetricDetail`,
`Traces → TraceDetail → SpanDetail`, `Logs → LogDetail`, `Overview` alone. `App::series_parent`
maps the current `Route` to the previous rung of that chain (`None` on a list route, which is a
series' first view), and `App::go_up` performs the move. The two axes now read cleanly: `←`/`→`
are *how you got here*, Backspace is *where this view sits*. They diverge whenever a view was
reached by a jump — a trace detail opened by the log→trace drill-down steps up to the Traces list,
where Back returns to the log.

**Rebuilding the parent.** `Route` variants carry their own data, so most rungs are trivial to
mint. `SpanDetail` is the exception: it holds `trace_id` + one span, not the whole trace. The trace
is recovered from `App::trace_detail` (the materialized trace retained behind the Traces preview
pane) and, failing that, from a `TraceDetail` still on the back stack — a *data* lookup only, never
a choice of destination. With neither available the step lands on the Traces list: still up its own
series, just skipping the rung there is no longer data for.

**State hygiene.** `go_up` pushes the departed view (so `←` undoes the step), resets focus/scroll,
and drops the intents scoped to the view being left (`pending_trace_open`, the trace focus, the
metric exemplars) — otherwise a late waterfall could yank the user back down into the detail they
just stepped out of. `span_cursor` is deliberately *kept* when stepping from a span detail up to its
trace (the cursor lands back on the span that was open) and cleared only when leaving the trace
views for a list.

**Lesson.** A keymap change is a hint-string change: the four detail hint bars (`ui/logs.rs`,
`ui/traces.rs` ×2, `ui/metrics.rs`) each grew a `bksp …` item. The global footer did not — Backspace
is a no-op on list routes, and advertising an inert key is worse than not advertising it.

## Follow-up: the Metrics series has a rung that is not a `Route` (2026-08-01)

**Report.** "Backspace doesn't work in the metrics screen series." Reproduced by driving the real
TUI against a `gen-demo-db` database in a tmux session (`tmux send-keys` + `capture-pane` — a
practical way to exercise a key path end-to-end without a pty test harness): the series *detail*
stepped up to the series list fine, but on the series list Backspace did nothing.

**Cause.** `series_parent` was written over `Route`, and the Metrics screen's first rung is not one.
Its chain is catalog → series list → series detail, but the catalog and the series list are *the
same* `Route::Metrics`, told apart by whether the query is empty (`App::on_catalog`) — the
catalog→series drilldown is pure query state, no new route (see the earlier entry on it). So the
series list looked like a series' first view and returned `None`.

**Fix.** The up-step became a small `SeriesUp` enum: `Route(Box<Route>)` for the ordinary rungs plus
a `Catalog` variant that clears the query instead of changing route (the refresh the key handler
already issues then renders the tree). Boxed because `Route` inlines its view's whole data (~340
bytes for a trace detail) while the other variant carries none — clippy's `large_enum_variant` is
right that a 344-byte enum for a one-bit distinction is the wrong trade when one allocation per
keypress buys it back.

**Lesson.** "What view am I on" and "what `enum` variant am I in" are not the same question, and a
navigation feature keyed off the enum will silently skip any view the enum does not name. Before
declaring a per-screen chain complete, enumerate the *rendered* views (what the user sees as a
distinct screen), not the routes — the Metrics screen renders three from two variants. The
divergence was invisible to the unit tests because they, too, were written over `Route`.

## The TUI as a head, over a new head API (2026-08-01)

`imbh-tui --url http://host:4318` now drives the full explorer against a running `imbhd`, over a new
`/api/head/*` surface owned by a new `imbh-head` crate (ARCHITECTURE.md §10.19). The motivation is
what a `Db::open_read_only` view *cannot* see: the writer's unsealed buffer, i.e. the most recent
telemetry of all. As a side effect the database may now live on another machine.

Four design questions came up, and the answers are worth keeping.

**Can the existing endpoints serve a head?** Mostly no, and the gap analysis is the useful artifact.
Of the eleven data operations the TUI performs, `GET /stats` covers one (short three ingest gauges),
five exist only as MCP tools, and **four have no counterpart anywhere**: nothing else in the server
*evaluates* PromQL, LogQL, or TraceQL, and nothing surfaces exemplars. The trap is that
`query_metric_range` / `histogram_quantile` / `search_traces` look like they would do — they are the
**typed-builder** path (`MetricQuery`/`TraceQuery`), not the evaluators, so they cannot express
`sum by (svc) (rate(x[5m]))`, which is literally what the TUI's query box takes. `logs/query`
additionally needs the `PageCursor` and span-id correlation `search_logs` has no room for.

**Could the languages be compiled to SQL and pushed through `POST /api/query`?** No, and the reason
is structural rather than effort: `imbh-lgtm` is *fetch-then-evaluate*, not a SQL compiler. The
`to_sql` methods that exist render only the **fetch** step. Pushing that down and evaluating on the
head founders on TraceQL — `execute_traceql` deliberately streams one complete trace at a time
(`fetch_candidates` then `fetch_trace` per candidate, so peak memory is one trace), which over HTTP
is `max_traces` round trips per refresh — and it inverts the payload everywhere else (a `sum by`
would ship every raw sample instead of a handful of series). It would also couple the head to the
daemon's *physical schema* rather than to a versioned API. Filed here because the idea recurs.

**Why not fold this into `imbh-mcp`?** It is a separate facility. MCP's tools are shaped for a model
and are deliberately lossy — no cursors, no per-sample matrices, no waterfall — and reshaping them
for a UI would change what every agent sees. `imbh-head` sits in the same tier for the same
dependency reason (`imbh ← imbh-head ← {imbh-server, imbh-tui}`; §12 forbids `imbh-tui` reaching into
`imbh-server`), with features splitting the halves so `imbhd` never links the HTTP client.

**JSON is not sound for these responses.** It has no `NaN`/`±Inf`, and `serde_json` writes all three
as `null`, which then fails to deserialize as `f64` — and a PromQL evaluation produces all three
routinely. Row-shaped results therefore answer as **Arrow IPC** (`arrow-ipc` is already compiled
wherever DataFusion is, so this costs no dependency), scalar ones stay JSON. Two things fell out that
were not obvious going in: the encoders take the *materialized* types rather than the `*_batches`
twins, which keeps `exec` at one return type for both backends and keeps `imbh/proto` + protox out of
both binaries; and everything not row-shaped (paging cursor, scan counters, the assembled trace
header, the narrowed window start) rides in the IPC **schema metadata**, so a response stays one
self-describing message. `PageCursor` has a private field, so it travels as its own serde form inside
that metadata — the only way anything outside the facade can construct one.

**The load-bearing invariant** is that `imbh_head::exec` is the *single* implementation over a `Db`:
`imbhd` calls it behind the routes, and the TUI's local backend calls it in-process. So the two modes
cannot diverge on translation, caps, or trace-window narrowing. `head_e2e.rs` asserts it directly —
every operation run both ways over the same `Arc<Db>`, results compared — which is a much stronger
test than checking either path alone.

Two things worth flagging for future work. The trace-window narrowing moved *into* `exec`, so a
remote head spends one round trip where a naive port would have spent up to seven. And an eval
request carries a **list** of queries: the catalog view emits one selector per checked metric, and a
query-apiece request would re-read the metric catalog apiece (the catalog is what PromQL translation
resolves a selector's kind against) — this was a real regression during the port, caught by asking
whether the direct-query path was preserved.

Footprint: the facade gate is unchanged at 275 crates (the head API is downstream of it). `imbhd`
went 293 → 297 crates and 32.6 → 32.9 MiB; `imbh-tui` 313 → 323, paying for reqwest, which the
feature split keeps out of the daemon entirely.

## Head API: what shipped, what the codebase taught us, and how it was verified (2026-08-01)

Companion to the entry above, which records the *design* questions. This one records the inventory,
the facts the codebase forced on the implementation, and the verification — the things a future
change to this surface will want to know before it starts.

### Inventory

New crate `imbh-head` (15th shipping crate), five modules:

| module | what |
| --- | --- |
| `dto` | wire types. Requests are JSON throughout; reuses the facade's own `serde`-gated types (`LogQuery`, `LogPage`, `Trace`, `MetricMeta`, `VolumeBucket`) so a remote head sends the *same value* its local twin hands to `Db` |
| `exec` | the **single** implementation of all eleven operations over a `Db` — both backends call it |
| `ipc` | Arrow IPC codec for the five row-shaped results |
| `client` | `HeadClient`, one async method per operation, reqwest |
| `path` | the eleven route constants, shared by the client that composes them and the server that registers them |

Elsewhere: `crates/imbh-server/src/head.rs` (the HTTP transport, `pub fn routes()` so a host can
mount the surface alone), `crates/imbh-tui/src/backend.rs` (`Backend::{Local,Remote}`), and the
`Arc<Db>` → `Backend` rewiring through `fetch`/`tasks`/`keys`/`runtime`. `metric_context` **moved**
out of `imbh-tui/src/promql.rs` into `exec` — translation belongs where the catalog is, not against a
copy a head would have to keep fresh. The TraceQL narrowing (`narrowing_starts` +
`execute_traceql_adaptive`) moved out of `fetch.rs` into `exec` for the same reason, and its tests
moved with it.

### What the codebase forced

Findings that changed the design mid-flight, each one a fact rather than a preference:

- **`arrow-ipc` is already in the `imbh` graph** (DataFusion pulls it) and `imbh::arrow::ipc::writer::
  StreamWriter` compiles with **no new dependency**. Verified with a throwaway probe before
  committing to the codec. This is what made "Arrow IPC for row-shaped results" free.
- **`imbh-lgtm`'s `trace_matches_to_batch` is `{trace_id, span_ids}` — no `start_time_ns`.** The trace
  list renders *when* each match happened, so the head's own schema is a superset. Deliberately not a
  change to the published lgtm schema.
- **The facade's `*_batches` twins are gated behind `imbh/proto`** (`query_batches_with_stats`,
  `get_batches`), which pulls `imbh-proto` + protox codegen. Encoding from the **materialized** types
  instead avoided that on both binaries *and* kept `exec` at one return type. The server pays one
  materialize-then-encode on a page of at most a few thousand rows.
- **`PageCursor` has a private field** — no public constructor. It crosses as its own serde form
  inside the IPC schema metadata, which is the only way anything outside the facade can build one.
  Same for `QueryStats`, which also has no `Default`.
- **Log/span attributes are stored as canonical-JSON `Utf8` columns**, not Arrow maps
  (`imbh-storage/src/schema.rs`), so `Attributes` ↔ Arrow is `Attributes::from_canonical_json` /
  `imbh_core::canonical_json_object`. This is what made the log-page and trace encoders tractable;
  had they been nested unions it would have been a different decision.
- **`Db`, `LogPage`, `QueryStats` are not `Debug`/`Clone`/`Default`** in the combinations the DTOs
  wanted. `Backend` gets a hand-written `Debug` that prints only which kind it is and what it reads.
- **Cargo:** a member cannot set `default-features = false` on a workspace dependency unless the
  `[workspace.dependencies]` entry sets it too. `imbh-head` is declared `default-features = false`
  there so each consumer names the half it plays.

### Verification

Three layers, because the interesting property is a *relationship* between two paths, not either one:

1. `crates/imbh-head/src/ipc.rs` unit tests — every codec round-trips, including `NaN`/`±Inf`, empty
   results, a `None` cursor, "missing trace" vs "trace with no spans", and a schema-equality test
   pinning the series layout to `imbh_lgtm::prom_matrix_schema`.
2. `crates/imbh-server/tests/head_e2e.rs` — every operation run **both ways over the same
   `Arc<Db>`**, results compared, over a real loopback socket. This is the test that would catch a
   divergence between local and remote; neither path tested alone would.
3. `crates/imbh-tui/src/backend.rs` tests — the direct-query path (`imbh-tui <directory>`) driving
   every screen against an in-process `Db`, so the pre-existing behaviour is pinned independently.

Plus a live run against a real `imbhd` over a generated demo DB: both modes rendered all four
screens, and over an **absolute** `--from/--to` window both reported `60 matching traces`. Worth
recording the trap: an earlier comparison showed 88 vs 76 and looked like a discrepancy — it was the
rolling `last 15m` window sliding between the two runs. Pin the window before comparing modes.

### Found by asking whether the old path still worked

The port initially made `exec::promql` read the metric catalog per query, while the old code read it
once and reused the context across the newline-separated sub-queries the catalog view emits. That is
N catalog reads where there had been 1. Fixed by making an eval request carry a **list** of queries
(`dto::EvalRequest::queries`), which also collapses N round trips to one remotely. Worth noting as a
class of bug: extracting a shared implementation from a caller can silently move a hoisted read
*inside* a loop, and nothing about the types complains.

### Follow-ups (in TODO.md)

- **Semver.** `imbh-head` is a new published crate, and `cli::Mode::Tui` gained a `source` field in
  place of `path` — a `0.4.0` under the 0.x rule. `imbh_tui::run` is *not* breaking: it takes
  `impl Into<Backend>` and `From<Arc<Db>>` keeps `run(db, options)` compiling.
- **`GET /stats`** still cannot be parsed back into a typed value and omits the three ingest gauges.
  The head answers `/api/head/stats` instead of widening it; converging the two would be additive
  except for the `durable_lsn` spelling (`None` is written as `0`).


## Publishing the log-driver plugin: two artifacts, never one tag (2026-08-03)

`release.yml` now has a `plugin` job that publishes the Docker logging-driver plugin to
`ghcr.io/moriyoshi/imbh-log-driver`, closing the TODO left open by the first CD pass. The question
that started it — "can we publish images that are *also* installable as a Docker plugin?" — has a
firm no at its centre, and the shape of everything else follows from it.

### One tag cannot be both

A managed plugin lives in a registry exactly where an image does (same repository namespace, same
blob store) but its manifest points at a config blob of media type
`application/vnd.docker.plugin.v1+json` — the `config.json` itself — over a single flattened-rootfs
layer. `docker plugin install` requires that config type and `docker pull` requires
`application/vnd.docker.container.image.v1+json`, so a tag serves one lifecycle or the other. Verified
against a local `registry:2`: the pushed manifest is
`{manifest: …manifest.v2+json, config: …plugin.v1+json, layers: [rootfs.diff.tar.gzip]}`, and
`docker pull` of that same reference fails with

    Encountered remote "application/vnd.docker.plugin.v1+json"(plugin) when fetching

Hence a separate repository (`imbh-log-driver`), not another tag on `imbh`.

### No manifest lists, so one tag per architecture

`docker manifest create` over plugin tags fails with `did not find plugin config for specified
reference`, and moby's plugin fetch path (`daemon/pkg/plugin/fetch_linux.go`) only unpacks
`ocispec.MediaTypeImageManifest` / `MediaTypeDockerSchema2Manifest` — it lists index media types in
its Accept header but has no platform-selection logic behind them. So even a hand-assembled index
would not be resolved. The published tags are `X.Y.Z-amd64` / `X.Y.Z-arm64` plus floating
`X.Y-<arch>` and `latest-<arch>`; there is deliberately no bare `X.Y.Z`, since it would be silently
wrong for half the users.

### The plugin store is content-addressed — one plugin per rootfs digest

The trap that would have broken the release run. Creating the same rootfs under a second name while
the first still exists fails:

    Error response from daemon: content sha256:937dbf00…: already exists

There is no `docker plugin tag`, and the floating tags are by definition the same content under
another name, so the job creates → pushes → **removes**, one tag at a time. Found only by pushing all
three tags for real against a local registry; a single-tag test would have passed. (Docker 29.2.1.)

### The rootfs stopped compiling, which is what made arm64 free

The old `docker-plugin/Dockerfile` compiled `imbhd` from source on `rust:1-alpine`. Under the release
matrix that is a fat-LTO build under QEMU for the arm64 leg — hours — and musl means it could not
reuse the archives the matrix already produced. It now mirrors `docker/Dockerfile`: a
`debian:bookworm-slim` base and a `COPY` of the prebuilt binary, staged from the release archive,
with the same `linux/<goarch>/` context layout (minus `imbh-tui`, which a plugin has no way to run —
a plugin has exactly one entrypoint, fixed in `config.json`).

Two details worth keeping:

- **No `RUN` step anywhere**, so no leg ever executes a foreign-architecture instruction and binfmt
  stays out of the pipeline. The three directories the plugin runtime needs are created with
  `WORKDIR`, which is a builder-side metadata operation, where `RUN mkdir -p` would have dragged QEMU
  into the arm64 leg for the sake of three `mkdir`s.
- **buildx `--output type=tar` is the rootfs**, directly. CI skips the `docker create` + `docker
  export` round trip entirely; `build.sh` keeps it, because it is BuildKit-only and the local path
  stays engine-agnostic so `DOCKER=podman` keeps working.

Image metadata (`ENTRYPOINT`, `ENV`, `USER`, `VOLUME`) is discarded when the rootfs is flattened, so
the Dockerfile now declares none — `config.json` is the sole source of truth and a second, inert copy
in the Dockerfile would only look authoritative.

### What the local path lost, and the escape hatch

Compiling inside the container meant `build.sh` worked on any host. Staging a host-compiled binary
into bookworm does not: a dev machine whose glibc is newer than 2.36 can produce a binary the plugin
cannot start, and under the plugin lifecycle that surfaces as a bare `docker plugin enable` failure
with the real error buried in the daemon log — there is no `docker plugin logs`. So the base is an
`ARG BASE` (CI never overrides it), and `build.sh` runs the same "nothing to serve" smoke assertion
`release.yml` uses on the archives, inside the rootfs, before it ever reaches `plugin create`. On
this host (glibc 2.39 → bookworm 2.36) the binary in fact links fine; the guard is for when it does
not, and it names `BASE=debian:trixie-slim` in the failure message.

### Verified

End to end on Docker 29.2.1, aarch64: both arch rootfs tars built (the amd64 leg on an arm64 host,
no QEMU), the plugin created from the buildx tar + `config.json`, enabled, a container run under
`--log-driver`, its stdout **and** stderr read back through `docker logs` — served out of the
database — with Parquet segments and a Tantivy `.tidx` on the host mount. Then pushed to a local
`registry:2` under all three tags and re-installed with `docker plugin install`. The one thing that
could not be covered locally is `docker plugin push` to **GHCR** under `${{ github.token }}`; that is
in TODO.md against the first `workflow_dispatch` rehearsal.

## Duplicate metric timestamps: one policy, three answers (issue #27, 2026-08-04)

A reporter's sampler polled `metrics.k8s.io` every 15s while metrics-server republished each kubelet
scrape timestamp unchanged for 15-30s. Both polls exported the source's own timestamp, so the same
`(series, timestamp)` landed twice — and `execute_prom` rejected any materialized series holding two
samples at one instant. 1136 stored rows, every ingest receipt reporting success, and every PromQL
query of that metric returning `400` for the whole retention period.

Three separate defects, which is why the fix is not one change:

1. **Nothing said no at write time.** `IngestReceipt::rejected` was declared-but-dead — both private
   constructors hardcoded `0`, recorded as a Deviation in ARCHITECTURE.md §10.5.
2. **The diagnostic was unactionable.** `SemanticError::Malformed` carries `&'static str`, so it
   structurally could not name the offending series. From the operator's side it read as "the query
   language is broken", and the natural next move — narrow the range — does not isolate it.
3. **There was no recovery.** Nothing can delete the offending points, so the metric stayed
   unqueryable until retention dropped it, whatever the producer did afterwards.

### The latent bug underneath

Duplicates are not only a producer fault. `build_metric_point_queries` issues **two** queries for an
instant selector (`MetricPointsQuery::gauge` then `::sum`) and `MetricsApi::fetch` concatenates both
result sets; `metric_labels_from_batch` derives labels from `service` + `__name__` + string attributes
and nothing distinguishes the two tables. So a metric name recorded as **both** a gauge and a sum
produced byte-identical label sets at one timestamp and `400`d a perfectly well-formed database. That
is now a test (`promql_handles_a_metric_present_in_both_the_gauge_and_sum_tables`), and it failed
before the change for exactly this reason.

### One knob, because the issue asked the right question

`Duplicates { ErrorOnRead (default) | LastWins | Reject { recent } }` in `imbh-core::config`, on
`DbBuilder` and as `IMBH_DUPLICATES`. The default is byte-for-byte today's behavior apart from the
message; `LastWins` is the only remedy for data already written; `Reject` is opt-in because dropping a
producer's data must never happen by accident.

### The two findings worth keeping

**A per-series `last_timestamp` map is the obvious design and the wrong one.** It is what Prometheus
does, and it is order-sensitive: X(ts=100) then Y(ts=50) accepts one point, Y then X accepts two. The
WAL stores the *raw* body and replay re-derives the tail with a guard that starts empty, so `last_ts`
could reject on replay a point the writer had accepted — silent data loss. A bounded `(series, ts)`
**set** is order-commutative, which gives `G_replay ⊆ G_original` at every replayed record and hence
"replay is strictly more permissive, and can never drop a row the writer kept". Commutativity is also
what lets the guard sit at the *decode* site rather than under the `Storage` lock — which is the only
place the async path can report an exact `rejected`, since the queued receipt returns before the
worker runs. As a bonus, `Storage::ingest_metrics` kept its published signature.

Scope was picked against a measurement, not a feeling: `imbhd` flushes on `interval=5s`, so anything
scoped to "what is currently buffered" would have seen ~5s of history against a 15-30s duplicate
window — i.e. would have closed the issue without fixing it. The guard is a `Db`-lifetime two-
generation set instead, preallocated so it never rehashes: fixed ~13 MB at `recent = 262144`.

**Resolving a duplicate by scan order is not deterministic here.** "Keep whichever the scan emitted
last" reads as the obvious rule and matches Prometheus' phrasing, but metric segments carry no
ingest-sequence column and the read SQL is `ORDER BY "time"` alone — so after a flush or a compaction
two identical queries can disagree, and a range query redraws a chart differently on refresh. Silent
nondeterminism is a worse failure than the `400` it replaces. The collapse is therefore a total order
on the *value* (any real number outranks NaN — `f64::total_cmp` alone ranks NaN above `+INFINITY`, so
a naive max would let one NaN row punch a hole in the chart), making the output a pure function of the
fetched sample multiset. `collapse_is_independent_of_input_order` pins it.

### Verified

`fmt`/`build`/`clippy --all-features`/`test --workspace` clean (62 test binaries, 0 failures), plus
`-p imbh-lgtm --features source` — which is **not** in the default workspace run, so the DB-backed
lgtm tests only execute when asked for explicitly. `Cargo.lock` and every manifest are untouched: the
guard is `std`-only, so the producer-only build (`--no-default-features --features ingest`, which has
no DataFusion at all) gains nothing. `imbhd` was run by hand to confirm the banner prints the
effective policy in the syntax `IMBH_DUPLICATES` accepts, and that a typo is fatal at startup rather
than a silent fallback.

Left open deliberately, now in TODO.md: the typed `MetricsApi::range`/`instant` path still `SUM`s
duplicate points. It degrades a number rather than denying service, and a SQL dedup needs the same
ingest-sequence column the log-driver `--tail 0` item wants. One column would close both.

## VRL log remapping for the Docker driver: what the dependency actually cost (2026-08-06)

`imbh-server` gained `docker-remap` — a VRL (`vrl` 0.34) stage between the reassembled wire entry and
the OTLP record, with a built-in script covering JSON, logfmt, klog/glog and `key=value`. It is the
**first feature in this workspace that adds crates on purpose**; every other feature comment in
`crates/imbh-server/Cargo.toml` ends in "adds no new crate". QUALITY_GATE.md says a footprint
regression is justified here rather than merged silently, so:

| axis | before | after | delta |
|---|---|---|---|
| `cargo tree -p imbh` (the gated axis) | 275 | **275** | **0** |
| `cargo build --release -p imbh-server` (default features, what the gate measures) | unchanged | unchanged | **0** |
| `cargo tree -p imbh-server --features docker,grpc,tracing` | 308 | **397** | **+89** |
| release `imbhd --features docker,grpc,tracing` (glibc, fat LTO, stripped) | 35,973,464 B = 34.3 MiB | **39,997,776 B = 38.1 MiB** | **+3.8 MiB** |

The shipped plugin binary is **40.0 MB against the §2 target of 42 MB** and a 55 MB hard limit — so
the feature ships enabled (`release.yml`'s Linux legs, `docker-plugin/build.sh`) without moving a
budget. On glibc; §2 is defined on musl, which runs higher, and the gate already annotates that.

**The binary cost came in far under the estimate, and the reason is worth writing down.** Adding vrl
at `compiler,stdlib-base` puts ~200 stdlib functions and their data tables (grok, the public-suffix
list, ua-parser, a second regex engine) in the *dependency graph*, and a first measurement taken
before any code called `compile()` showed only +1.4 MiB — because fat LTO had stripped essentially
all of it. That number was meaningless and nearly got reported as the answer. The real figure, once
`vrl::stdlib::all()` is referenced and every `Function` impl is therefore live, is +3.8 MiB. The
lesson generalises: **a dependency's graph size and its linked size are different questions here**,
and only the second one is measurable after the code that uses it exists.

**The gate is blind to all of this.** `scripts/footprint-gate.sh` counts `cargo tree -p imbh` — the
facade — and builds `imbh-server` with *default* features. The dependency direction is
`imbh ← imbh-server`, so nothing this crate links can ever reach that number, and `docker-remap` is
off by default so the binary axis does not see it either. Both axes reported "unchanged" while the
shipped plugin grew by 3.8 MiB. The gate now prints an **informational, never-failing** plugin-build
section (crate delta + binary size at `docker,docker-remap,grpc,tracing`) so the cost is visible on
every run instead of at release time. Anything else that lands in `imbh-server` has the same blind
spot.

### Three policy statements this falsified

- **`deny.toml`/`about.toml` licenses.** `borrow-or-share` is **MIT-0** and `quoted_printable` is
  **0BSD**; neither was on the 12-entry allow-list, and `[graph] all-features = true` means the gate
  walks vrl *whether or not the feature is on* — marking it default-off would not have helped. Both
  added to both mirrored lists. `MIT-0` arrives via vrl's **non-optional** `jsonschema` dependency, so
  it is unavoidable at every vrl feature level, `compiler`-only included.
- **ARCHITECTURE.md §11 "C code allowed only for libzstd".** `stdlib-base` force-enables vrl's
  `datadog` feature, which pulls `onig_sys` (vendored oniguruma) — the graph's second C library. Both
  Linux release legs build natively, so no cross-compilation setup changes; the macOS legs never
  enable the feature.
- **ARCHITECTURE.md §11 "`lz4_flex` … is not a dependency".** It is now: vrl depends on it
  non-optionally.

vrl is also the graph's **first MPL-2.0 component** (`grep -c MPL-2.0 THIRD-PARTY-NOTICES.txt` was 0).
MPL §3.3 permits the Larger Work under Apache-2.0, so this is fine, but every embedder inherits the
story. Nine new duplicate-version crates, `prost 0.13` alongside our 0.14 the notable one;
`cargo deny check bans` still passes (duplicates are `warn`).

### Design notes worth keeping

**The event is seeded with the record the driver would have stored anyway.** `Remapper::seed` fills
in Docker's wire fields *and* the finished OTel record — body, attributes, resource, both timestamps,
the stream's severity — so the identity script `.` is byte-for-byte today's behaviour
(`an_identity_script_reproduces_the_unremapped_record` pins it) and the built-in script only ever
*overrides*. It never re-derives `service.name`, `container.*` or `log.iostream`, which is what keeps
the existing metadata mapping in exactly one place.

**Three invariants are re-asserted after every script run**, because a script is operator input and
the rest of the plugin depends on them: `container.id` on the resource is **overwritten**, not
filled-in-if-absent (a *wrong* id would silently merge two containers' `docker logs` histories),
`service.name` is restored only when absent or empty (a deliberate override is legitimate), and
`log.iostream` is re-appended (it is what `readlogs::to_entry` restores the wire `source` from).

**A parsed timestamp is only accepted within ±26h of Docker's capture time.** `readlogs.rs` orders,
pages, applies `--since`/`--until` *and* computes its follow watermark from `row.time`; without the
window a container with a skewed clock could make `docker logs -f` skip lines. This is a correctness
constraint on the script, not a nicety — and it is why klog, which carries no year, keeps the capture
time rather than the year VRL infers.

**`docker logs` re-renders a structured body as logfmt** (`ts=… level=… ` then the body's fields)
rather than the original line being stored a second time. A *string* body still goes out verbatim, so
a `docker`-only build and every OTLP-ingested record are untouched. The RFC 3339 **formatter** this
needed is hand-written next to the parser that was already there, for the same reason and because it
must work without chrono in a `docker`-only build; `civil_from_days` is the exact inverse of the
existing `days_from_civil`.

**Prose must never be chopped into fields.** Every `key=value` tier is anchored on the line *starting*
with `key=`, which alone rejects `starting server on port 8080` and `usage: foo --opt=bar`. Two
further traps: `parse_logfmt` gives a bare word the **boolean** `true` while a real `flag=true` is the
**string** `"true"`, so filtering booleans drops exactly the prose remnants; and the comma-delimited
form has to be tried *before* the space-delimited one and only when the line has no whitespace at all
— otherwise the space-delimited pass swallows `level=warn,msg=retrying` whole as one field named
`level`, and *succeeds*, producing silent nonsense.

**VRL's `??` is error-coalescing only** (`lhs.resolve().or_else(...)`), not null-coalescing; `||` is
what falls through on null. And `exists()` needs a static path, so candidate-key search is an
`if`/`else if` chain over literal paths rather than a loop over a list of names.

### Verified

`fmt` / `build --workspace` / `clippy --workspace --all-targets -D warnings` / `test --workspace`
clean, plus both driver configurations explicitly: `-p imbh-server --features docker` (60 unit + 7
e2e) and `--features docker,docker-remap` (87 unit + 13 e2e). The `docker`-only run matters as much as
the other — the un-remapped path has to keep working, and the e2e suite now starts its mechanics
tests with remapping *off* for exactly that reason. `cargo deny check licenses` passes after the two
allow-list additions; `check bans` passes with the new duplicate warnings.

## VRL remapping, part 2: finishing it, and what the rollout surfaced (2026-08-06)

Companion to the entry above, which recorded the dependency cost while the work was mid-flight. This
one closes it out: the final numbers, the surfaces that had to change beyond `remap.rs`, and three
findings that only appeared once the feature was wired end to end.

### Final footprint

| axis | value | budget |
|---|---|---|
| `cargo tree -p imbh` (gated) | 275 | target 275, hard 300 |
| release `imbhd`, default features (gated) | 34.9 MB / 33.3 MiB | target 42 MB, hard 55 MB |
| `imbh-server --features docker,grpc,tracing` | 308 crates, 36.0 MB | not gated |
| `imbh-server` + `docker-remap` (**the published plugin**) | 397 crates, **40.0 MB / 38.1 MiB** | vs the 42 MB target |

Both gated axes are byte-for-byte what they were. RSS was **not** measured (the gate ran with
`RSS_PROBE=0`); a per-FIFO-thread `Runtime` plus a cloned event object per line makes steady-state
RSS the plausibly-affected number, and it is one thread per container rather than per line. Open.

### A default-on feature silently rewrote an existing test suite

`serve_plugin`'s default became `Source::Builtin`, so the moment the feature compiled, **six of the
seven existing e2e tests failed** — not because anything broke, but because they assert on stored
bodies and `docker logs` output, and both legitimately changed. The fix is the interesting part:
those tests are about *driver mechanics* (framing, the FIFO reader, follow mode, the ingest drain),
so `start_plugin()` now starts with remapping **off** and new tests opt in via
`start_plugin_remapping()`. That keeps the un-remapped path — which is still what a `docker`-only
build ships — genuinely covered rather than incidentally so.

Generalisable: when an opt-in feature becomes on-by-default for a component, the existing suite stops
testing what it was written to test. Re-pointing it at the old behaviour is usually better than
re-baselining its expectations, because the old behaviour is still a shipping configuration.

One test needed a second look rather than a fix. `the_default_script_turns_container_output_into_
queryable_fields` failed on severity order, and the *test* was wrong: the JSON line's own timestamp
had moved it 400s later, so `ORDER BY time` legitimately sorts it last. Ordering by `observed_time`
(Docker's capture time) is what reads in the order the container printed. That the two orderings now
differ is the feature working — it is the OTel event-time/observed-time distinction the driver could
not express before — so the test asserts on both.

### Additive API, because the crate is published

`serve_plugin_with{,_until}` are `pub` on a released crate, so threading a remap default through them
would have been a breaking change needing a semver bump. Instead `PluginConfig { ingest, remap }` plus
one new `serve_plugin_with_config` entry point; the four existing functions keep their shapes and
delegate. Cost is one `#[allow(clippy::needless_update)]` where `..Default::default()` is exhaustive
without the feature — cheaper than either a breaking change or two `cfg`-duplicated call sites.

### A measurement that could corrupt the thing it sits next to

The informational plugin-size section added to `scripts/footprint-gate.sh` originally built into
`target/release/imbhd` — **the same path the gated binary axis reuses when it already exists**. An
interrupted run would have left the plugin build there, and the next gate would have measured it as
if it were the default build, silently reporting ~40 MB against a budget that describes a different
binary. It now builds into `target/footprint-plugin-probe/`. Caught only because a gate run mid-work
printed the plugin's size on the *gated* line.

### Two pre-existing gate defects, found while reading its output

Neither is caused by this change (the only facade edit is a re-export; nothing was added to
`cargo tree -p imbh`), both are now in TODO.md:

- **`scripts/footprint-gate.sh`'s `datafusion: NO` assertion is vacuous.** It greps for `datafusion v`,
  but the graph contains only the split crates (`datafusion-core`, `datafusion-common`, …) and no bare
  `datafusion`. The check has been silently passing-by-printing-NO rather than guarding anything.
- **`QUALITY_GATE.md`'s search-off number is wrong.** It documents 216 crates for
  `imbh --no-default-features`; the gate measures **71**. The doc conflates "search off" with
  "no default features" — the latter also drops `query`, and therefore the whole DataFusion subtree.

### Surfaces changed beyond the driver

`docs/DOCKER_LOG_DRIVER.md` gained a Remapping section (both event shapes, per-format behaviour, the
`@PATH`/`off`/inline grammar, the `docker logs` rendering, the ±26h caveat) and its "what a line
becomes" table now distinguishes `time` from `observed_time`. `docker-plugin/config.json` gained
`IMBH_DOCKER_REMAP` and **no new mount** — a managed plugin's `type: none` mount requires its host
source to exist before `docker plugin enable`, so adding one would have broken enable for every
existing installation; the `data` mount already covers `@/var/lib/imbh/remap/*.vrl`. `release.yml`'s
two Linux legs, `build.sh` and `ci.yml` all name the feature, and `ci.yml` runs **both** driver
configurations because they are different code paths.

`imbh`'s facade gained one re-export, `canonical_json_value` — the natural companion to the
already-exported `parse_json`, needed by the logfmt renderer for nested body values. Additive, no new
dependency, and it keeps `imbh-server` talking to the facade rather than reaching past it to
`imbh-core`.

### Verified

`fmt` / `build --workspace` / `clippy --workspace --all-targets -D warnings` / `test --workspace`
clean, plus `-p imbh-server` clippy **and** tests under `--features docker` and
`--features docker,docker-remap` separately. `cargo deny check licenses` and `check bans` pass;
`THIRD-PARTY-NOTICES.txt` regenerated (it now carries the graph's first MPL-2.0 entries, plus MIT-0
and 0BSD). Footprint gate OK on both gated axes. Nothing committed.

## TODO sweep: four stale items, three fixed, one misdiagnosis (2026-08-06, part 3)

A `tackle-todos` pass over the 14 open items in `TODO.md`. A source scan first: **zero**
`// TODO` / `// FIXME` / `todo!(` / `unimplemented!(` markers across all 141 tracked `*.rs` files,
so `TODO.md` is the whole work list. Worth recording as a property of this repo — the usual
"grep the source for markers" half of the sweep finds nothing here, and the doc is load-bearing
in a way it is not in most codebases.

### Four items were stale, and staleness clustered by kind

Items 1 (head-crate semver bump), 8 (CD has never run), 10 (the `windows-latest` job has never run)
and 11 (release carrying the Windows fix) were all closed without work. The pattern: every one of
them was an item whose resolution came from **something happening in CI or a release**, not from
someone editing code. Four releases (`v0.1.1` … `v0.5.0`) and dozens of CI runs went by, each
quietly answering an item, and nothing brought the news back to `TODO.md`. Code-shaped TODOs get
closed by the person touching the code; process-shaped TODOs have no such trigger and rot silently.
The verification was cheap in every case (`git ls-remote --tags`, `gh run list`,
`gh run view --json jobs`) — worth making the first move of any sweep rather than dispatching agents
against items that no longer exist.

Two of the four left a genuine residue behind, now filed as their own open items rather than being
lost with the parent: the **GHCR package visibility** for `imbh-log-driver` (a package created by
`GITHUB_TOKEN` is not public just because the repo is, and a private one breaks
`docker plugin install` for everyone), and the fact that the green Windows job is deliberately
narrow — it never exercises **retention**, and only partly exercises compaction, which is precisely
the open/mmapped-file-rename shape issue #3 had.

### The `datafusion` assertion: the item's diagnosis was wrong

`TODO.md` claimed the check was vacuous because the graph "contains only the split crates
(`datafusion-core`, …) and no bare `datafusion`". It does contain a bare `datafusion v54.1.0` — line
185 of 999, two matching lines. The grep matched all along.

The check *was* weak, for a reason the item did not name: it printed `datafusion: yes`/`NO` and
**exited 0**. A `NO` that does not fail the gate guards nothing no matter how good the pattern is.
Both engine checks now set `fail=1`, matching the search-lever guard directly below them that
already did. The pattern was broadened to the crate *family* (`datafusion(-[a-z-]+)? v`) anyway —
not because it is broken today, but because DataFusion keeps splitting, and the day `imbh-query`
depends on `datafusion-core` instead of the facade the old pattern would false-alarm on a healthy
tree. That is the failure the item described, arriving later than it thought.

**A reported measurement that did not survive re-check.** The subagent's headline finding was a
`grep -q`-under-`pipefail` SIGPIPE race — `grep -q` exits on first match, the upstream writer gets
EPIPE, and `pipefail` surfaces the writer's 141 despite the match — measured at 63 false negatives
in 400 runs. It does not reproduce: 0 in 900 on re-check, and the mechanism **cannot** fire at the
current size, because the tree is ~55 KiB against a 64 KiB pipe buffer, so `printf` never blocks and
there is no broken pipe to report. The here-strings were kept as cheap insurance for a future larger
tree, and the comment in the script was rewritten to say that rather than to assert a flake rate.
The likelier explanation for whatever was seen: three agents were hammering `cargo` concurrently,
and `cargo tree`'s failure under lock contention is invisible here because `2>/dev/null` swallows it.
Lesson worth keeping: a subagent's empirically-framed claim ("63 of 400 runs") is not
self-validating — this one had a plausible mechanism, a specific number, and a live reproduction
story, and was still wrong.

**Which surfaced a regression the fix itself introduced.** Making the checks hard-fail means an
empty `$tree` — exactly what a `cargo tree` failure produces, silently — now turns a transient
infrastructure hiccup into a red gate reporting that *both* engines were silently dropped. Guarded:
the capture keeps stderr, and a failed/empty tree reports "could not run" once, distinctly, instead
of being read as a footprint result. Distinguishing "we could not measure" from "the answer is no"
is the general point; the gate had conflated them.

### The 216-vs-71 crate count was two knobs conflated

`QUALITY_GATE.md` said turning `search` off drops the tree "to 216"; the gate measured 71. Neither
was the number the sentence wanted. Measured (aarch64-glibc, `--edges normal`, unique): default
`ingest,query,search` = **275**; `--no-default-features --features ingest,query` = **217**, i.e.
-58, which is the tantivy subtree and the actual cost of turning search off; `--no-default-features`
= **71**, i.e. -204, which also drops OTLP decode and the whole DataFusion subtree. The doc's 216
was the search lever's number wearing the bare build's name.

The gate had *taught* the doc the error: its old line printed `tantivy dropped: yes (-204 crates)`,
attributing the entire `--no-default-features` delta to tantivy. A measurement labelled with the
wrong cause propagates into prose and is then very hard to argue with, because it is a real number.
The gate now prints all three, labelled, and checks the precise `ingest,query` lever for tantivy
leakage in addition to the bare build.

### `/stats` converged on one serializer, at the cost of a second breaking change

The item asked for three ingest gauges on `stats_json` plus a `durable_lsn` spelling fix. There are
**four** gauges — `ingest_rejected` arrived in 0.5.0 with the duplicate-timestamp policy — and the
`db_stats` call site that "round-trips" the JSON parses into `serde_json::Value`, so it constrained
nothing beyond well-formedness; the real constraint was `imbh_head::dto::Stats`.

Rather than widening the hand-written writer, `stats_json` now defers to that typed value
(`serde_json::to_string(&dto::Stats::from(stats))`) and `exec::stats()` uses the same conversion, so
there is genuinely **one** serializer instead of two that had to agree by inspection — which is how
the gauges went missing in the first place. Cost: `imbh-mcp` gained an `imbh-head` dependency
(`dto` feature only), +1 workspace crate, +0 third-party, `cargo tree -p imbh` unchanged at 275.

The convergence **forced a second breaking change beyond the one authorised**: `dto::Stats` and
`dto::TableStats` had to drop `skip_serializing_if`, because `/stats` has always emitted absent
per-table bounds as explicit `null` and one serializer means one spelling. So a
`GET /api/head/stats` consumer now sees `null` fields where they were previously omitted. Both
changes are deserialization-compatible in each direction via `#[serde(default)]` and both are in
`CHANGELOG.md` under `[Unreleased]`. Generalisable: "make these two surfaces share one serializer"
is never contract-neutral — it necessarily picks a winner for every spelling the two disagreed on,
and the disagreements are not all visible from the side you set out to change.

### Dependabot: the pinning had disabled the alerts

`.github/dependabot.yml` added — `github-actions` at `directory: "/"` (verified complete: no
composite actions in the tree), weekly, minor+patch grouped so one CI run clears the batch, majors
ungrouped so a renamed input fails on its own. **Ten** distinct actions, not the nine the item
counted (`taiki-e/install-action` was missed).

The finding that changes what this item was *for*: per GitHub's Secure use reference, Dependabot
raises security **alerts** only for actions using semantic versioning, and explicitly **not** for
actions pinned to SHA values. The SHA pinning — added deliberately for supply-chain safety — had
silently opted the repo out of action security alerts altogether. So this file is not the
convenience the item framed it as; it is the only automated channel by which an upstream action fix
reaches `release.yml`, the workflow holding the crates.io token and the registry login. A hardening
measure that disables the alerting for the thing it hardens is worth generalising as a shape to look
for. Recorded in the file as its lead comment. One watch item: `dtolnay/rust-toolchain` is pinned to
a commit on its `stable` *branch*, not a tag, so Dependabot follows branch HEAD and those PRs carry
no version to sanity-check the diff against. No `cargo` entry — filed as an open item with the
tradeoff, since footprint budgets here are load-bearing and Dependabot cannot reason about the
hand-trimmed `default-features = false` sets.

### Verification

`cargo fmt --all --check`, `cargo build --workspace`,
`cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` — all clean (no
failures across the suite). Footprint gate `OK` end to end: 275/275 crates, `imbhd` 33.3 MiB, plugin
feature set 38.1 MiB, both engine checks passing, all three lever numbers printed. The empty-`$tree`
guard was exercised against a simulated `cargo` failure. Nothing committed.

Left open by design, unchanged by this sweep: the `v0.3.0` remote tag (still absent from `origin`;
re-pushing deliberately burns a red 40-minute CD run — the user's call), MCP cost ceilings, the
buffered-response write deadline, the upstream differential runner, the published-target footprint
measurements, the `docker-remap` RSS measurement, and the two items — the `--tail 0 -f` event-time
race and `MetricsApi`'s duplicate metric points — that both want the same ingest-sequence column and
are therefore one serial core-schema change, not two parallel ones.

## TODO sweep, phase 2: the column that should not be built (2026-08-06, part 4)

Continuation of part 3. Seven more items closed, one of them by *not* doing what was approved.

### The headline: an approved design was wrong, and the repo already knew

The sweep's own framing said the `--tail 0 -f` tail race and `MetricsApi`'s duplicate metric points
"both want the same ingest-sequence column", and the user approved adding it. A read-only design pass
found that framing wrong on every limb:

- **`ARCHITECTURE.md` had already considered and rejected exactly this column, twice** — §10.5.1
  (line 738) and line 1311: *"resolved by value, never by scan order: metric segments carry no
  ingest-sequence column … so a positional rule would let two identical queries disagree after a flush
  or compaction."* Ordering the typed metrics dedup by an ingest sequence would have made the typed API
  and PromQL resolve the same duplicate **differently**. The column was not merely unnecessary for that
  consumer; it was the wrong tool, and building it would have broken a shipped invariant.
- **The logs consumer already had its column.** `observed_time` is on the schema, on the DTO, set by
  the Docker driver from dockerd's capture stamp, and deliberately preserved through VRL remap. It was
  never exposed as a query axis — a plumbing gap, not a storage gap.
- **The `Lsn` could not have worked anyway.** It is one per OTLP *request* (`lib.rs:549` says so in a
  comment), not per row, so it cannot break a tie between two rows of the same request.

Generalisable, and the reason this is worth a whole section: **the TODO entry proposed a solution, and
the solution was carried forward as a premise through the sweep and into a user decision.** Nobody
re-derived it. The check that caught it was cheap — read the architecture doc for prior art before
implementing a TODO's suggested fix, because a project that has thought about a problem before has
usually written down why it rejected the obvious answer. The habit to keep: treat the *problem* half of
a TODO as evidence and the *solution* half as a hypothesis.

Two of the three consumers were then closed with no schema change, no WAL change, no on-disk
compatibility question, no semver break, and zero footprint cost.

### A silent data-corruption bug, found sideways

The design pass turned up something unrelated to its brief: `concat_and_sort` concatenated against
`batches[0].schema()`, and arrow-select 58.4's `concat_batches` takes columns **positionally** with no
name validation (`concat.rs:548-556` — a bare `batch.column(i)` loop; the only check is a final
`try_new`, which validates *types*, not meaning). Compacting a UTC-day partition whose segments were
sealed under different `promote` sets therefore gave one of three outcomes by segment order: panic when
the first segment was wider, **silent** truncation when narrower, and **silent** cross-column
concatenation when the widths matched — `env` values and `region` values merged into one column
labelled with whichever name came first. Two of three failed silently and wrote the corrupted result
back as the merged segment.

Reachable through an operation the docs called supported: §6.1 said adding or removing promoted keys is
backward-compatible, which was true of the read path (`coerce` null-fills) and false of the compaction
path. The doc has been corrected on both halves.

Worth noting how it was found: not by a bug hunt, but by a design pass asking "what breaks if a column
is added?" — the compatibility question surfaced a defect that had nothing to do with the column and
everything to do with schema evolution being half-implemented. Asking the compatibility question is
cheap even when the change it was asked for never happens.

### Two probes that overturned their own plan

The metrics work was told to prove two assumptions before building on them. Both failed, in opposite
directions, and the result was better than the plan:

- **`CASE WHEN value = value` does not detect NaN.** DataFusion 54 orders floats by a *total* order,
  not IEEE, so `value = value`, `value >= value` and the `< 0 OR > 0 OR = 0` idiom all return **true**
  for NaN. Had this shipped on the design's assumption, NaN would have silently won every duplicate —
  a wrong-number bug with a passing test suite, since no test would have thought to feed it a NaN.
- **`isnan` is available regardless**, because DataFusion declares `datafusion-functions` as a
  non-optional dependency with default features on, so the workspace's `default-features = false` pin
  cannot remove it. Zero added crates, verified: 275 before and after. The hand-rolled UDF fallback
  was never needed.

A pleasing corollary: since DataFusion's float sort *is* `total_cmp`, plain `value DESC` already
matched PromQL's `duplicate_value_cmp` second clause. Only the NaN demotion had to be stated.

Both anti-regression tests were **mutation-checked** — dropping `resource, scope` from the partition
key, or dropping `isnan`, makes them fail. That check matters more than usual here, because the
partition-key trap is invisible to any test that does not deliberately construct rows differing only in
`resource`: N replicas emitting the same counter at the same instant differ in nothing else, so
partitioning on label-set identity would turn a modest over-count into a large *under*-count on exactly
the counters people alert on.

### Measurement notes

- **The driver RSS soak needed a third column to be interpretable.** Plain `docker` vs `docker-remap`
  alone could not separate "the remapper costs memory" from "the remapped payload is bigger". Adding an
  **identity VRL script `.`** as a control — provably byte-identical output, yet still clones a seed per
  line — split it cleanly: `identity − off` is the machinery (flat across a 100× line-rate range,
  +1.0/+2.3/+1.4 MiB, i.e. noise), `builtin − identity` is payload growth. The item's hypothesis that
  cost scales with container count and not line rate is now *confirmed* rather than assumed:
  4.0 MiB fixed + 0.165 MiB per container, 67 MiB at 100 containers against a 200 MB target.
- **`$GITHUB_STEP_SUMMARY` is unreachable from the REST API.** No summary endpoint exists and the
  check-run `output.summary` is null; job summaries render only in the web UI. Harvesting the CD
  footprint numbers meant downloading the five release archives and sizing the binaries directly, with
  SHA-256 verified against the published `SHA256SUMS`. Worth writing down, because the natural plan
  ("read the numbers CD already prints") does not work.
- **The published footprint margin is thinner than the local gate suggests**: x86_64-linux `imbhd` is
  41.1 MB against a 42 MB target, and the x86_64/aarch64 gap is 5.1 MB — so the aarch64 host figure the
  gate prints is the optimistic end of the range. And no released archive has ever been built with
  `docker-remap`, whose local measurement was +3.8 MiB against that 888 KB of headroom.

### Verification

`cargo fmt --all --check`, `cargo build --workspace`,
`cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` — all clean.
`imbh-server` under `docker` and `docker,docker-remap` separately. Footprint gate OK, 275/275 crates
unchanged throughout. Every behavioural fix in this phase was confirmed non-vacuous by reverting it and
watching the new test fail: the compaction coercion (panic reproduced), the three `readlogs` hunks
(3 of 19 e2e tests time out), and the two metrics anti-regressions (mutation-checked). Nothing
committed.

Known weakness, recorded rather than hidden: the PromQL-agreement test compares against an **inline
mirror** of `collapse_duplicate_samples` rather than the real function, because `imbh-lgtm` depends on
`imbh` and importing it would be a dev-dependency cycle. It is a weaker guarantee than it looks, and a
true cross-crate check would have to live in `imbh-lgtm`.

## Footprint: the gate passes on the arch that isn't the problem (2026-08-06, part 5)

Closing the last two actionable sweep items turned one documentation chore into a live release risk.

### §2's `imbhd` budget named a binary nobody builds

The row read `x86_64-unknown-linux-musl`, and CD builds musl **nowhere** — it survives only in
`about.toml`/`deny.toml` for license coverage. So the budget could never be checked against the thing
it named, and in practice was checked against whatever host ran `scripts/footprint-gate.sh`. Retargeted
to `x86_64-unknown-linux-gnu`, the largest target the archives actually ship, with the measured 41.1 MB
from v0.5.0 beside it. No musl archive added: a sixth fat-LTO leg needs `cross`/`zigbuild` or a
container (no native musl runner, and `zstd-sys`/`onig_sys` build vendored C), and the Alpine case is
already served by the `bookworm-slim` image plus the CI-asserted glibc ≤ 2.36 floor.

### The measurement that matters: an exact delta, and a projection over budget

The `docker-remap` cost had only ever been quoted as "+3.8 MiB" from a single local build. Measured
properly, as the difference between two local aarch64 release builds differing **only** in that
feature: 35,973,488 → 39,997,800 bytes = **+4,024,312 B (+3.84 MiB)**. The local baseline lands within
**24 bytes** of CD's v0.5.0 aarch64 archive (35,973,464 B) — worth noting because an earlier entry
called those two figures "byte-identical", which is very nearly but not exactly true, and the 24 bytes
are the local-vs-CI toolchain difference.

Applying that delta to CD's **x86_64** baseline (41,112,104 B) projects **≈ 45.1 MB against the 42 MB
target** — roughly 3 MB over, still comfortably under the 55 MB hard limit.

Three things make this a live problem rather than a note:

1. **`release.yml`'s Linux legs now carry `docker,docker-remap,grpc,tracing`** (as of `fc70cf8`),
   while v0.5.0 shipped `docker,grpc,tracing`. The next release is the **first** whose x86_64 archive
   contains the VRL subtree at all.
2. **The footprint gate cannot catch it on this host.** On aarch64 the same binary is 5.1 MB smaller,
   so the gate reads 40.0 MB, compares it against 42 MB, and **passes**. The measurement is real and
   the verdict is wrong — for the shipping target, not for the measured one.
3. **The projection errs low.** The delta was measured on aarch64, and x86_64 codegen is demonstrably
   fatter across every target pair in Appendix C, so the true figure is more likely above 45.1 MB.

Generalisable: a gate that measures *a* build and compares it against a budget written for *a
different* build produces a green tick that means nothing. The failure here was not the number, it was
the silent substitution of the host for the shipping target — the same shape as the earlier
216-vs-71 crate-count confusion (part 3), where a measurement of one knob was labelled with another
knob's name. Both were invisible because the output looked like a measurement and *was* a measurement,
just not of the thing the label claimed.

No local confirmation is possible: this host has no x86_64 cross linker. The next step is a
`workflow_dispatch` dry run, which builds and smoke-tests all five archives and publishes nothing —
turning the projection into a number before a release depends on it. Then the decision is one of: raise
the target with justification, trim the VRL subtree, or ship `docker-remap` as a separate artifact so
the default archive stays inside budget.

### Sweep close-out

Twenty-one of the original fourteen-plus-derived items are now closed; four remain open, and every one
of them is open by an explicit decision rather than by neglect: the MCP per-call cost ceiling and the
buffered-response write deadline (both reviewed 2026-08-06 and deliberately left, both still framed as
"if this matters for a deployment"), the upstream differential runner (deferred by standing user
request), and the x86_64 over-budget projection above, which is blocked on a CD run rather than on
work. Nothing committed.

## The second POSIX-shaped assumption, and two verification notes (2026-08-06, part 6)

Gaps in parts 3–5, which recorded the sweep's headline items but skipped these. The first is a real
portability defect and belongs in the record.

### A "test coverage gap" that was hiding a defect

Part 3 closed the "Windows portability job has never run" item as stale — it runs, it is green — and
split out the residue as a coverage chore: the leg never exercises **retention**, and only partly
exercises compaction, so deletion and rename of open or memory-mapped files was untested. That is the
shape issue #3 had.

Investigating the coverage gap found the bug the coverage would have caught.

`Storage::retain` and `Storage::compact` propagated the post-manifest unlink error with `?`. On POSIX
an unlink **always** succeeds regardless of open handles or mappings, so this was silently fine
forever. On Windows it is not: a file with a live memory mapping cannot be deleted **at all**, whatever
share flags its opener passed — the section object pins it. And `imbh-index`'s `search_body` /
`search_body_bool` / `search_attr_eq` hold exactly such mappings, via Tantivy's `MmapDirectory` over
the `.tidx` sidecars, for the life of a `matches()` or attribute-equality pushdown. Queries run on the
tokio runtime while the background maintenance thread calls `retain()`, so the overlap is an ordinary
production interleaving, not a contrived one.

The consequence is worse than a failed cleanup. By the time the unlink runs, both callers have already
persisted the manifest **without** those segments and made it durable — the pass has *succeeded* as far
as every reader is concerned, and the files are dead weight. Propagating the error therefore reported a
successful retention or compaction as a failure, and the `?` abandoned the remaining segments in the
batch mid-loop.

Fixed with a `reclaim_segments` helper making the unlink best-effort, logging under `tracing`. This is
explicitly **not** a new failure mode: a crash in the same window already left orphans, and
`cleanup_orphans` sweeps unreferenced `.parquet`/`.tidx` on the next open, so a refused unlink lands the
process in a state the design already handles. Deliberately not done: a persistent retry queue so a
refused unlink retries on the next pass rather than waiting for a reopen — that is new durable state,
and the orphan sweep already bounds the leak.

**Stated as what it is: a code-reading conclusion, not an observed failure.** The work was done on
Linux, where the bad ordering succeeds silently; no Windows host was available and nothing was
reproduced there. The distinction is worth preserving in the record, because "we reasoned that Windows
rejects this" and "we saw Windows reject this" justify very different confidence.

Two things worth keeping about the tests. They went into `imbh-storage` and `imbh --test lifecycle` —
the two targets the Windows leg **already** runs — so no CI widening was needed; a test placed anywhere
else would simply never run on Windows, which is how the gap survived in the first place. And the
tolerance test was confirmed non-vacuous by temporarily panicking inside `reclaim_segments`: it was the
*only* test in the suite to trip that branch, which also proves the other two delete cleanly on Linux.
`memmap2` is a dev-dependency only and already reaches the graph through tantivy, so the footprint gate
is untouched (275/275 verified).

Generalisable, and the reason this deserves its own section: **this is the second Unix-shaped
assumption in the on-disk path**, after issue #3's fsync of a directory handle. Both have the same
shape — an operation that is unconditional on POSIX is *conditional* on Windows — and both were
invisible to a green test suite running on Linux. Worth treating "what does this syscall refuse to do
on Windows?" as a standing review question for any new filesystem call, rather than waiting for the
next one. The corollary for planning: a coverage gap filed as a chore is worth investigating before it
is worth filling, because the gap usually exists precisely where nobody has exercised the dangerous
ordering.

### Verification notes

**Dependabot, cargo: security updates only.** Added a `cargo` entry with
`open-pull-requests-limit: 0`, which is the *documented mechanism* rather than a trick — GitHub's
options reference states "you can temporarily disable version updates for a package manager by setting
this option to zero" and, separately, that "security update pull requests are not subject to this limit
and do not count toward it". There is no separate "security only" switch. The split is deliberate:
routine Rust version churn stays a measured act because the footprint budgets are load-bearing and the
margin is thin (part 5: 41.1 MB against a 42 MB target on the shipping arch), and Dependabot cannot
reason about the hand-trimmed `default-features = false` sets. But an *advisory* had no automated path
at all — `cargo-deny` fails CI on RUSTSEC, and failing is not fixing; it names a vulnerable crate and
waits for a human. Pairs with part 3's finding that SHA-pinning had already opted the repo out of
Dependabot security alerts for actions.

**GHCR package visibility, verified without the scope.** `gh api user/packages` needs `read:packages`,
which the local token does not carry, so the settings-page route was blocked. The property that
actually matters is testable anonymously and needs no scope at all: request a pull token from
`ghcr.io/token` with no credentials, then `GET /v2/<pkg>/tags/list`. Both `moriyoshi/imbh` and
`moriyoshi/imbh-log-driver` answer `200`, which is precisely the access a stranger's
`docker plugin install` needs. Worth remembering as the general technique — test the capability, not
the configuration that is supposed to grant it.

---

## The typed metrics dedup: the probe that failed was the useful one (2026-08-06, part 7)

Closed the standing asymmetry recorded in `ARCHITECTURE.md` §10.5.1: under `Duplicates::LastWins`,
PromQL collapsed a duplicated instant by value while the typed `MetricsApi::range`/`instant` path went
on `SUM`/`COUNT`-ing both rows, so `sum`/`count` inflated and `avg` skewed. `RateMode::Counter` was
always immune, since `max - min` does not care how many times a value appears. The fix wraps the
existing scan in a `ROW_NUMBER() OVER (PARTITION BY <row identity> ORDER BY <value order>)` subquery,
gated on `Duplicates::collapses_at_read()`, entirely inside `crates/imbh/src/metrics.rs`.

### The probes came first, and one of them was wrong in an instructive way

Two assumptions were probed before any code was written, because both were plausible and neither was
evidenced anywhere in the tree.

**Window functions: available.** There were zero `OVER (...)` clauses in the whole repo, and the
workspace pins `datafusion` with `default-features = false`, so it was genuinely unknown whether
`ROW_NUMBER()` would even plan. It does — including `PARTITION BY` over the `Dictionary(Utf8)` columns
(`service`/`resource`/`scope`) and an unaliased derived table. Nothing to do.

**IEEE `NaN <> NaN`: not available, and not for the reason expected.** The plan was to rank NaN last
with `CASE WHEN value = value THEN 1 ELSE 0 END`, on the theory that `isnan` was missing because
`math_expressions` is off. The probe returned `1` for a NaN row. The first hypothesis was the
DataFusion simplifier folding `expr = expr` to `true` on a non-nullable column — but `value < 0.0 OR
value > 0.0 OR value = 0.0` and `value >= value` also returned `1`. **DataFusion 54's float comparison
kernels implement a total order, not IEEE semantics.** No comparison operator in this engine
distinguishes NaN. That is the durable finding, and it is much broader than this change: any future
code that reaches for a SQL-level NaN test in imbh will be silently wrong.

**The third probe is the one that made the fallback unnecessary.** `isnan(value)` turned out to be
registered after all, which contradicted the premise of the whole exercise, so it was worth checking
whether that was an artifact of the test profile. It is not: `datafusion` 54.1 declares
`[dependencies.datafusion-functions]` with **no** `default-features = false`, so that crate's default
set — which includes `math_expressions`, itself an empty feature list gating no dependency — is on in
every possible build. The workspace's own `default-features = false` pin on `datafusion` cannot remove
it, and it adds nothing to the graph. So no UDF was written.

Worth separating from the above, because it is a *different* claim about the same feature machinery:
the `hex` UDF exists because `datafusion/encoding_expressions` is off, and DataFusion registers
function *packages* according to the **`datafusion` crate's** features. `datafusion-functions` having
compiled `base64`/`hex` does not make `encode` reachable. `isnan` is reachable because math
registration is not gated the same way. "The sub-crate compiled it" and "the session registers it" are
independent questions, and the trimmed-feature reasoning in §9.1 only answers the second.

A pleasant corollary of the total-order finding: since DataFusion's float sort *is* `f64::total_cmp`
(NaN above `+INFINITY`), plain `value DESC` already reproduces the second clause of `imbh-lgtm`'s
`duplicate_value_cmp` exactly. Only the NaN demotion had to be added, giving
`ORDER BY isnan(value) ASC, value DESC`.

### The partition key is row identity, not label-set identity

The trap in this change is not the SQL, it is the `PARTITION BY` list. The instinct is to reach for the
series identity PromQL uses — `service` + `__name__` + string attributes — and that would have been a
correctness disaster. `k8s.pod.name` and `host.name` live in the **resource**, so five replicas
emitting the same counter at the same instant differ in *nothing* except `resource`. Partitioning on
`(time, metric, service, attributes)` collapses a legitimate 5-way `sum` to a single point: a modest
over-count traded for a large **under**-count, on exactly the counters people alert on. Silent
under-counting on an alerting path is strictly worse than the bug being fixed.

The key is therefore `("time", metric, service, resource, scope, attributes)` — byte-identical row
identity. Promoted attribute columns are deliberately absent, and that is safe only because
`imbh-storage` keeps the `attributes` JSON verbatim and treats promoted columns as projections of it
(`crates/imbh-storage/src/lib.rs:1847`); if promotion ever *stripped* the key from the blob, two rows
differing only in a promoted attribute would start collapsing. That dependency is now written down at
the constant, because it is invisible from `metrics.rs`.

The `WHERE` stays on the inner scan in both shapes, so the `TableProvider` pushdown contract (§9.2) and
the `matches()`/bloom paths are untouched by the wrapper. Under the non-collapsing policies the emitted
SQL is byte-identical to what it was before.

### Verification: the mutation check is the part that carried weight

Nine tests, all green, but the two that matter are anti-regressions and a passing anti-regression
proves nothing on its own. So both bugs were reintroduced deliberately: dropping `resource, scope` from
the partition key and dropping `isnan(...)` from the ordering each made exactly the expected tests fail
(the resource test, the NaN test, and the PromQL-agreement test), and restoring made them pass. Without
that step the five-replica test would have been indistinguishable from a test that passes for the wrong
reason — it passes on the *unfixed* code too, since the bug only appears once dedup is switched on.

Gate: `cargo fmt --all --check`, `cargo build --workspace`, `cargo clippy --workspace --all-targets
-- -D warnings`, `cargo test -p imbh`, `cargo test -p imbh-lgtm`, `cargo test --workspace` — all clean.

### Two things deliberately not done

**`ErrorOnRead` parity is deferred, not forgotten.** Full symmetry with PromQL would make the *default*
policy fail on a duplicate, which needs a second detection scan on every typed range query for every
user — including the overwhelming majority who have no duplicates — and is a runtime behaviour break on
published crates at v0.5.0. The asymmetry is also narrower than it looks: PromQL fails because a
duplicated instant has no PromQL meaning, whereas the typed API is a SQL aggregation builder where
`SUM` over two rows is well-defined, merely not what the user wanted. "Opt into `LastWins` for
PromQL-consistent numbers" is a coherent contract. If parity is ever wanted, the cheap form is a
warning surfaced through `QueryStats`, not a hard error.

**The PromQL-agreement test compares against a mirror, not the real function.** `imbh-lgtm` depends on
`imbh`, so importing `collapse_duplicate_samples` into an `imbh` test would be a dev-dependency cycle.
The test therefore carries a verbatim copy of `duplicate_value_cmp`/`collapse_duplicate_samples`,
flagged in a comment. This is a real weakness — it verifies agreement with a *transcription* of the
PromQL rule, so drift in `promql.rs` would not trip it. The honest fix is to move that one test into
`imbh-lgtm`, where both sides are in scope; it was left where it is only because the crate boundary was
outside the change's remit.

### Follow-ups this leaves behind

- `ARCHITECTURE.md` §10.5.1 ends with a "Known asymmetry" note that is now stale in both halves: the
  typed path no longer `SUM`/`COUNT`s duplicates, and the claim that "a SQL window dedup would need an
  ingest-sequence column that does not exist" is backwards — the window dedup is value-ordered
  *precisely because* no such column exists. It needs rewriting, not deleting.
- The corresponding `TODO.md` entry can be closed.
- Consider moving `range_dedup_agrees_with_the_promql_collapse` into `imbh-lgtm` to make it a genuine
  cross-crate check.

### Addendum to part 7 (same day, appended after the fact)

Two of part 7's three follow-ups were already done before it was written — it was composed against a
stale view of the tree, so the record needs correcting rather than acting on:

- **§10.5.1's "Known asymmetry" note was already rewritten**, and along the lines part 7 asks for: the
  block now describes the shipped window dedup, spells out the partition-key trap and the
  `isnan`-vs-comparison hazard, and closes with a *"Remaining asymmetry, deliberate"* note scoped to
  `ErrorOnRead` alone. The old text is gone; `grep "Known asymmetry: the typed"` returns nothing.
- **The `TODO.md` entry was already closed**, with the correction that the item's own proposed fix (an
  ingest-sequence column) was the wrong one — see part 4.

The third follow-up stands and is now tracked in `TODO.md`: moving
`range_dedup_agrees_with_the_promql_collapse` into `imbh-lgtm` so it compares against the real
`collapse_duplicate_samples` instead of a transcription of it.

Worth noting *why* this happened, since it will recur: a long-running agent's view of the tree is
frozen at the point it last read a file, and concurrent work invalidates its follow-up list without
invalidating its findings. The findings in part 7 are first-hand and stand; the follow-ups were
inferences about repo state and did not. Treat those two halves of any agent report differently.

## The plugin could never be installed as documented: bind source vs. propagatedMount (2026-08-06)

A user followed `docs/DOCKER_LOG_DRIVER.md` "Install" verbatim and `docker plugin enable` failed:

```
error mounting "/var/lib/imbh" to rootfs at "/var/lib/imbh":
  stat /var/lib/imbh: no such file or directory
```

Not an environment quirk — **the documented install had never worked on a machine without a
pre-existing `/var/lib/imbh`**, and no step created one. `docker plugin set data.source=...` reports
success (it only records a setting), so the failure lands one command later, in the OCI runtime, with
no `docker plugin logs` to read.

The irony: the 2026-08-06 VRL entry above already recorded the exact rule — "a managed plugin's
`type: none` mount requires its host source to exist before `docker plugin enable`, so adding one
would have broken enable for every existing installation." That reasoning was applied to a
*hypothetical new* mount and never turned on the `data` mount already shipping, which has the same
requirement on any host installing for the first time.

### Why the plugin cannot fix this itself

`imbh-storage` already calls `create_dir_all` (`src/lib.rs:233`), and the rootfs already contains
`/var/lib/imbh` (`Dockerfile`'s `WORKDIR`). Neither helps: the runtime establishes mounts **before**
the entrypoint executes, so the task dies in `runc create` and `imbhd`'s `main` never runs. Any
"provision it at startup" design is unimplementable for a bind source.

### Measured, on Docker 29.2.1, with busybox probe plugins

| config | `plugin enable` |
|---|---|
| bind mount, source missing | ✗ fails in `runc create` at the mount |
| bind mount, source pre-created | ✓ reaches the entrypoint |
| `propagatedMount`, no `mounts` | ✓ reaches the entrypoint; daemon provisions the store |
| no mount at all (rootfs dir) | ✓ reaches the entrypoint |

Two further measurements decided the trade-offs, both via a container bind-mounting the daemon's `/`
(which is also how a non-root user inspects `/var/lib/docker`, and how any of this is reachable on
Docker Desktop, where the path lives in the VM):

- a marker written by the entrypoint **accumulated across three `disable`/`enable` cycles** — the
  propagated mount persists through the lifecycle every `docker plugin set` requires;
- `docker plugin rm` removed `/var/lib/docker/plugins/<id>/` **whole, propagated mount included** —
  the database is destroyed with the plugin. Measured, not inferred.

### What shipped

`config.json` drops the `data` mount (`"mounts": []`) and declares
`"propagatedMount": "/var/lib/imbh"`. Install is now one `docker plugin install` on every daemon, one
fewer permission to grant, and — the reason this beat the alternative of auto-creating the bind
source from `build.sh` — it is **platform-independent**: it asks the host filesystem for nothing, so
Docker Desktop needs neither a VM-internal `mkdir` nor host file sharing, whose FUSE-family semantics
imbh's advisory `flock` on `writer.lock` and its mmap'd segments have no business depending on.

Breaking, and recorded in `CHANGELOG.md` under `[Unreleased]`: the database moves out of the host
path, `plugin rm` now deletes it, and it can no longer be relocated to another disk, so `Retention`
must be sized against the filesystem holding `/var/lib/docker`. Backup/restore and remap-script
installation are documented as container one-liners in "Where the database lives".

### Regression guard

`crates/imbh-server/tests/docker_plugin_config.rs` — three ungated tests parsing the shipped
`config.json`: no bind mount may reappear, `entrypoint[1]` must equal `propagatedMount` (a drift
there is silent data loss into the replaced-on-upgrade rootfs), and `IMBH_DOCKER_PLUGIN_SOCKET` must
match `interface.socket` and stay unsettable. Confirmed to fail when a mount is reintroduced, not
merely to pass today. The file had **no** test coverage before: it is consumed by `docker plugin
create` at package time, so every mistake in it was previously a user-machine discovery.

### Verified

`fmt --all --check` / `clippy --workspace --all-targets -D warnings` / `test --workspace` all clean
(exit 0, 0 failures), plus `-p imbh-server` under `--features docker,docker-remap`. The shipped
`config.json` was additionally packaged into a real plugin against a busybox rootfs and enabled with
nothing provisioned by hand: it now fails only at `exec: "/usr/bin/imbhd"`, i.e. strictly after the
mount stage. Probe plugins and the stray probe directory were removed. Nothing committed.

## Preparing v0.6.0: the release gate was measuring a binary from another feature set (2026-08-06)

Release prep for `v0.6.0` — the minor bump the `propagatedMount` fix has been waiting on (TODO
"Open Items", first entry). Three things came out of it that are not the version bump itself.

### The changelog was missing its headline feature

`[Unreleased]` carried the compaction schema fix, the `--tail 0` arrival-clock fix, the typed-metrics
dedup, `LogQuery::observed_after`, the `/stats` ingest gauges, and the `propagatedMount` break — but
**not** `docker-remap` (`fc70cf8`), the largest change in the release and the one with a
`BREAKING CHANGE:` trailer. The PR that landed it wrote its findings to JOURNAL and its user-facing
docs to `docs/DOCKER_LOG_DRIVER.md`, and the changelog was the one surface it skipped. Nothing catches
this: no gate reads `CHANGELOG.md`, and `git cliff` (below) would have "caught" it only by discarding
the file. Added under `### Added`, with the plugin-visible break — a recognised line's `body` is now
parsed fields, and `docker logs` re-renders it as logfmt — stated in the entry rather than left to the
commit trailer.

The released section is ordered Added → Changed → Fixed (matching `[0.4.0]`), not the
Fixed → Added → Changed order the entries happened to accumulate in.

### The binary-size axis was measuring the plugin build

`scripts/footprint-gate.sh` printed `imbhd: 39997776 bytes = 38.1 MiB` against the ≤ 42 MB target —
comfortably passing, and wrong. That number is **byte-identical** to the plugin-feature build recorded
in this journal for `docker-remap` (2026-08-06). The gate's binary axis was:

```sh
bin=target/release/imbhd
if [ ! -f "$bin" ]; then cargo build --release -p imbh-server; fi
```

`target/release/imbhd` is one path shared by every feature set. Anyone who has run
`cargo build --release -p imbh-server --features docker,docker-remap,grpc,tracing` by hand leaves the
plugin binary at it, and the skip-if-present check then measures that against the *default*-build
budget. Rebuilt with default features, the real number is **34,916,248 B = 33.3 MiB = 34.9 MB** — a
5.1 MB phantom, and 7 MB of margin the gate was not reporting.

The direction that matters is the other one: the same check will report a stale *small* binary after a
real regression, which is a gate that passes by not looking. Fixed by building unconditionally (cargo
no-ops when current), which is the only thing that reconciles the feature set with the path. Note the
plugin probe added in `fc70cf8` already had this exact insight — it builds into its own
`target/footprint-plugin-probe` "deliberately", with a comment saying the gated axis reuses
`target/release/imbhd` when present — and stopped one step short of concluding that the reuse was
itself the bug.

### `cargo release` as configured would destroy the changelog

Two release-machinery findings, both pre-existing, neither fixed here:

- `pre-release-hook = ["git", "cliff", "-o", "CHANGELOG.md", "--tag", "{{version}}"]` in the root
  `Cargo.toml`, and there is **no `cliff.toml` in the repo**. Verified by running it to stdout:
  git-cliff falls back to its built-in default config and emits a conventional-commit digest
  (`### 🚀 Features` / `- *(docker)* [**breaking**] …`) for every tag in history. With `-o` that
  *replaces* `CHANGELOG.md` — the hand-written Keep a Changelog prose, the migration notes, and the
  `<!-- next-url -->` anchors the `pre-release-replacements` in `crates/imbh/Cargo.toml` match with
  `exactly = 1`, all gone. The two mechanisms are mutually exclusive and the repo has both.
- The `pre-release-replacements` under `[workspace.metadata.release]` (the `VERSION=` and
  `ghcr.io/…` strings in `README.md` / `docs/DOCKER_LOG_DRIVER.md`) have never fired — the v0.5.0
  release commit says so explicitly and corrected those strings by hand. They were corrected by hand
  again here.

Consistent with both: `v0.5.0` was prepared as a hand-written commit, not by `cargo release`.

### Verified

`fmt --all --check` clean; `build --workspace`, `clippy --workspace --all-targets -D warnings` and
`test --workspace` all clean (0 failures, 64 suites). `./scripts/license-gate.sh` OK.
`./scripts/gen-notices.sh` regenerated `THIRD-PARTY-NOTICES.txt` — the only delta is the 14 workspace
crates moving to 0.6.0, i.e. the third-party set is unchanged since `fc70cf8`. Footprint gate OK:
275 crates (target 275), `imbhd` 33.3 MiB after the fix, plugin build 397 crates / 38.1 MiB
(informational), idle RSS 14.9 MB, steady RSS 94.4 MB, search-off lever 275 → 217 → 71. The §3c
packaging dry-run (`cargo package --workspace --allow-dirty`, dirty because the bump is uncommitted)
staged and verified all 20 members at `0.6.0`, exit 0. Nothing committed, nothing tagged, nothing
published.

## 2026-08-07 — Runtime bridge-network discovery for the Docker log driver

The plugin's knowledge of Docker networking was a **build-time constant**. `docker-plugin/build.sh`
ran `docker network inspect bridge --format '{{range .IPAM.Config}}{{.Gateway}}{{end}}'` once and
applied the answer with `docker plugin set`, over a hard-coded `172.17.0.1:4318` default in
`config.json`. The Rust code had no network awareness at all: `listen_addr()` returned one string,
`serve_async` bound one `TcpListener` and discarded every peer.

That default is wrong on any daemon with a custom `bip`, on one whose `docker0` was re-created, and
on **every registry install** — the documented `docker plugin install` cannot probe your daemon, so
it gets the literal. The failure is silent in the worst way: container logging is filesystem-only
(the plugin socket and the FIFOs), so logs keep flowing and the only symptom is a query/OTLP endpoint
that never answers.

Now `IMBH_LISTEN_ADDR=auto` (the shipped default) resolves at run time to every bridge gateway the
daemon has, re-resolved every `IMBH_DOCKER_NETWORK_REFRESH` (default 30s).

### Two backends, and why the scan is not a fallback in the apologetic sense

| Backend | How | Sees |
|---|---|---|
| `Source::Api` | `GET /networks` over the daemon's Unix socket | network names, IPAM gateways/subnets, per-network container attachments |
| `Source::Ifaces` | `getifaddrs` in the host netns | gateways and subnets |

The scan works *because* of the 2026-07-30 finding that forced `network.type: host`: the plugin sees
`docker0` and every `br-*` directly. Docker programs a bridge interface's address **from** its IPAM
gateway, so on a stock daemon the scan reproduces the API's gateway/subnet answer exactly — this is
the same IPAM data, read one layer down. Interfaces are matched on `docker0` or `br-` + 12 hex
characters **and** the existence of `/sys/class/net/<name>/bridge`, which is what excludes a veth
named like a bridge and libvirt's `virbr*`. What the scan cannot do: name the Docker network, list
attached containers, or recognise a bridge renamed with `com.docker.network.bridge.name`.

The probe re-runs on **every** refresh, not once. At `docker plugin enable` during daemon boot the
API socket may not be serving yet, and a one-shot probe would strand the process in scan mode for
life.

### The deadlock that shapes the whole container-attributes design

`container.network.*` needs the Engine API. The obvious implementation — inspect the container in
`StartLogging` — is a **daemon deadlock**: `dockerd` calls the log driver synchronously during
container start while holding that container's lock, and the API's network-inspect path resolves
attached containers. So the rule is absolute: *nothing on the plugin's request path ever calls the
daemon.* `StartLogging` reads the last published snapshot and nothing else.

That makes late arrival unavoidable — a container that starts between two scans is not in the last
snapshot. Rather than hide it, `Container::resource` became `RwLock<Arc<Resource>>` and a refresh
that learns new attachments swaps a fuller resource in (`set_networks`). Records before the swap have
no network attributes; records after do. `encode` reads the `Arc` once per batch *group*, not per
record, and `set_networks` is a no-op when nothing changed — which is what keeps the pointer-equality
grouping intact across an idle daemon's 30-second refreshes. `.info.networks` reaches VRL the same
way, cached in the `Remapper` by `Arc::ptr_eq` so a script that reads `.info` rebuilds the object at
most once per refresh, and a script that does not pays nothing (the existing `wants_info` bargain).

### Packaging: what was deliberately *not* done

The Engine API needs the daemon's socket in the plugin's mount namespace. **No mount was added.** A
managed plugin's bind source must already exist on the daemon host, under rootless Docker the socket
is not at `/var/run/docker.sock`, and `tests/docker_plugin_config.rs` guards `mounts: []` precisely
because that class of bug shipped once already (PR #32). The managed plugin therefore runs in scan
mode — which covers binding and the allow-list completely — and forgoes container attributes; a
standalone `imbhd` gets API mode for free. Stated in the operator guide rather than papered over.

Two things remain **unmeasured** and are recorded in TODO.md: whether `docker plugin enable` accepts
a `mounts` entry with a null/settable `source` (which would make API mode an opt-in with no default
privilege change), and whether the daemon's API socket is serving by the time a plugin is enabled at
daemon startup.

### Footprint

**Zero new crates**, measured against `main`: 282 / 304 / 411 for `imbh`, `imbh-server` default, and
`imbh-server --features docker,docker-remap,grpc` — identical before and after. `libc` was already a
direct dependency (signal handling) and `tokio-stream` was already in the *default* graph via
`datafusion-datasource-json`; the latter is now a direct optional dep so the supervised gRPC listener
can serve a socket the supervisor bound. The Engine API client is HTTP/1.1 hand-written over
`std::os::unix::net::UnixStream` with a complete-body chunked decoder, and the JSON goes through
`imbh::parse_json` — the same reasoning that kept `serde_json` out of `docker/json.rs`.

### Where it lives, and why all of it is under `docker`

`docker/addr.rs` (Cidr, AllowFrom/Access, BindSpec, the `Discovery` trait), `docker/networks.rs`
(both backends, the refresh thread), `docker/serve.rs` (the supervisor). `lib.rs` keeps exactly two
things: `serve_on_listener`, the pre-existing accept loop split out from its bind so several
listeners can share a runtime, and `pub(crate) type PeerFilter = Arc<dyn Fn(IpAddr) -> bool + …>` —
one `Option` the accept loop checks. Everything else, including the rate-limited refusal warning,
sits inside the closure under the feature gate. A default build's accept path is what it always was,
one branch heavier.

### Follow-ups

- The gRPC listener now serves a supervisor-bound socket via `serve_with_incoming_shutdown` and a
  hand-written peer-filtering `Stream`, rather than letting tonic bind its own. This is what makes
  "which bind failures are fatal" one rule for both protocols.
- A remap script's view of `.resource` is still frozen at `StartLogging`, so a script sees the
  pre-discovery resource while the *stored* record gets the current one. Harmless today (the built-in
  script never reads `.resource`) but worth knowing before anything depends on it.

## 2026-08-07 — How the Docker client actually resolves its socket (measured)

Review question on the discovery work: where does `WELL_KNOWN_SOCKETS` come from? Honest answer at
the time: convention. `/var/run/docker.sock` because every recipe says so, `/run/docker.sock` guessed
as "a fallback for hosts where `/var/run` is not symlinked". Unlike the rest of that module, nothing
had been measured. So it was measured, against docker 29.2.1.

### The resolution order the CLI really uses

1. `-H/--host` / `--context` flags
2. **`DOCKER_HOST`** — *overrides the active context*. The CLI says so out loud:
   `Warning: DOCKER_HOST environment variable overrides the active context.`
3. **`DOCKER_CONTEXT`**
4. **`currentContext`** in `<config>/config.json`
5. the built-in `default` context → the compiled default `unix:///var/run/docker.sock`

`default` is not a stored context. `docker context ls` describes it as *"Current DOCKER_HOST based
configuration"*, and setting `DOCKER_HOST=unix:///tmp/probe.sock` changed its listed endpoint live.

### The store

`<config>/contexts/meta/<sha256(name)>/meta.json`. Confirmed twice: `DOCKER_CONTEXT=nope` produced a
path ending `ca3704aa…` = `sha256("nope")`, and a throwaway `docker context create imbh-probe` landed
in `42dc3cae…` = `sha256("imbh-probe")`. Its contents, which is what the parser is now written
against rather than guessed at:

```json
{"Name":"imbh-probe",
 "Metadata":{"Description":"..."},
 "Endpoints":{"docker":{"Host":"unix:///tmp/imbh-probe.sock","SkipTLSVerify":false}}}
```

The lookup **scans and matches on the `Name` inside the file** rather than hashing the name. Same
answer, no sha256 to carry (nothing in the graph provides one), and the CLI's choice of digest stays
its own business instead of becoming our ABI.

### What this changed

Both constants survived, but the *reasons* were wrong and are now recorded:
`/var/run/docker.sock` is the **client's** compiled default; `/run/docker.sock` is where the
**daemon** listens — systemd's `docker.socket` carries `ListenStream=/run/docker.sock`, and the two
are one file only because `/var/run` is a symlink to `/run`.

The real defect was elsewhere: `DOCKER_HOST` is one of *four* inputs above the default, and only that
one was read. **Rootless Docker** is the case that bites — `dockerd-rootless-setuptool.sh` offers
`export DOCKER_HOST=…` *or* `docker context use rootless`, and anyone taking the second silently got
the interface-scan backend and no `container.network.*` attributes. Which is doubly awkward, because
rootless is exactly the case cited in the operator guide and in TODO.md as the reason *not* to
hard-code `/var/run/docker.sock` as a plugin mount source.

Two smaller fixes rode along: the probe now requires an actual socket rather than any existing path
(a stray regular file would be chosen and then fail on connect, burning the refresh while a real
socket sat further down the list), and `$DOCKER_CONFIG` is honoured. A `tcp://`/`ssh://` endpoint
still yields no candidate at all, which is *correct* rather than a limitation: this feature binds
gateways that exist on this host, so a remote daemon's networks would be actively wrong. That was
accidental before and is now stated.

Only `currentContext` is parsed out of the CLI's `config.json`. That file also holds registry
credentials, so nothing else is read from it and none of it is ever logged.

### Method note

The throwaway context was created and removed inside the measurement, with the config file's sha256
compared before and after to prove nothing else was touched. `docker context rm` leaves the now-empty
`contexts/meta` directory behind — worth knowing if a future probe checks for the store's existence
rather than its contents.

## Preparing v0.6.1: a patch release that is one feature, and two crate counts that are both right (2026-08-07)

Release prep for `v0.6.1`. Everything since `v0.6.0` is a single feature landing: runtime
bridge-network discovery for the Docker log driver (`444b1b2`) plus its three review follow-ups — the
test-only accessor refactor (`673242a`), the sysfs path-component check (`3ab6aeb`), and the
Docker-socket resolution that reads `DOCKER_CONTEXT` and the CLI's `config.json` the way the client
does (`26ea117`). Nothing else moved, so `[Unreleased]` was empty and the whole `[0.6.1]` section had
to be written from the commits rather than closed from accumulated entries.

### Why a patch bump when the release is a `feat`

The 0.x rule this project uses is that a **minor** bump means something broke — `v0.6.0`'s own commit
message says so ("a minor bump under the 0.x rule because two changes are breaking"). Nothing in
0.6.1 does:

- **No published Rust API moved.** `serve_async` was private and became `pub(crate)
  serve_on_listener` (split from its bind so several listeners can share a runtime);
  `serve_with_limits_until`, the public entry point, keeps its signature and now binds and delegates.
  `PeerFilter` is `pub(crate)`. The one manifest change is `imbh-server`'s `docker`/`grpc` features
  gaining `tokio-stream?/net`, which is additive and adds no crate.
- **The one operator-visible default change is a fix wearing a feature's clothes.** The shipped
  plugin's `IMBH_LISTEN_ADDR` / `IMBH_GRPC_LISTEN_ADDR` move from `172.17.0.1:4318` / `:4317` to
  `auto`. On a stock daemon `auto` resolves to exactly the address the literal named, so the
  documented install is unchanged; on a daemon with a custom `bip` or a re-created `docker0` it is
  the difference between a reachable endpoint and a silent one. Shipping that as a *minor* bump would
  say "this may break you" about a change whose entire purpose is to stop being broken.
- **`IMBH_ALLOW_FROM` defaults to `any`**, so the new accept-time filter changes no existing
  deployment.

### Two crate counts, both correct, and neither is wrong about the thing that matters

`444b1b2` records "Zero new crates: 282/304/411". `scripts/footprint-gate.sh` on the same tree prints
**275** for the `imbh` facade and **308 → 397** for `imbh-server` at `docker,grpc,tracing` →
`+docker-remap`. Neither number is stale: they count different things.

The gate pipes `cargo tree -p imbh --edges normal --prefix none` through
`sed 's/ (\*)//; s/ v[0-9].*//'` — it **strips the version before `sort -u`**, so it counts unique
crate *names*. Counting unique `name v version` pairs on the same graph gives 282. The seven-pair gap
is five names that appear at more than one version: `foldhash`, `getrandom`, `hashbrown`,
`ordered-float`, `syn`. (Dropping `--edges normal` entirely gives 296 pairs, i.e. build/proc-macro
edges are a further 14.)

Worth knowing because the §2 budget is defined against the gate's number and only that one. A commit
message quoting the pair count next to a budget quoting the name count reads as a 7-crate regression
that never happened. The changelog entry for 0.6.1 therefore quotes the gate's numbers, not the
commit's — the claim both were making ("unchanged from before") is true either way.

### The release machinery is still two mechanisms that cannot both run

Unchanged since the v0.6.0 prep, and worth restating because this is the third release in a row that
worked around it by hand:

- `pre-release-hook = ["git", "cliff", "-o", "CHANGELOG.md", …]` in the root `Cargo.toml`, with **no
  `cliff.toml` in the repo**. Running it would replace the hand-written Keep a Changelog file — prose,
  migration notes, and the `<!-- next-url -->` anchors the `pre-release-replacements` in
  `crates/imbh/Cargo.toml` match with `exactly = 1` — with a conventional-commit digest.
- The `pre-release-replacements` under `[workspace.metadata.release]` (the `VERSION=` and `ghcr.io/…`
  strings in `README.md` / `docs/DOCKER_LOG_DRIVER.md`) have still never fired; cargo-release does not
  read replacements from the workspace table. Corrected by hand again, all five strings.

So `v0.6.1`, like `v0.5.0` and `v0.6.0`, is prepared as a hand-written commit rather than by
`cargo release`. Fixing the config — either committing a `cliff.toml` that reproduces the current file
or dropping the hook, and moving those replacements into `crates/imbh/Cargo.toml` where they are read
— is now three releases overdue and is in TODO.md.

### v0.6.0 shipped

TODO.md's long-standing first item — the published 0.5.0 plugin that could not be installed as its own
docs described — is closed. `v0.6.0` was tagged and published 2026-08-07T00:39Z with all five archives,
`SHA256SUMS`, the container image, and both log-driver plugin jobs green. It is replaced by the same
entry for this release.

### Verified

`fmt --all --check` clean; `build --workspace`, `clippy --workspace --all-targets -D warnings` and
`test --workspace` all clean (**64 suites, 585 passed, 0 failed, 4 ignored**).
`./scripts/license-gate.sh` OK. `./scripts/gen-notices.sh` regenerated `THIRD-PARTY-NOTICES.txt` —
its only delta is the 14 workspace crates moving to 0.6.1, so the third-party set is unchanged since
`v0.6.0`. Footprint gate **OK**: 275 crates (target 275), `imbhd` 34,916,248 B = 33.3 MiB, plugin
feature set 397 crates / 40,063,344 B = 38.2 MiB (informational; +65,568 B over v0.6.0's 39,997,776 B,
which is the whole cost of discovery in the shipped plugin), idle RSS 15.0 MB, steady RSS 104.8 MB,
search-off lever 275 → 217 → 71.
The §3c packaging dry-run (`cargo package --workspace --allow-dirty`, dirty because the bump is
uncommitted) staged and verified all 20 members at `0.6.1`, exit 0. Nothing tagged, nothing published.

## 2026-08-07 — `auto` resolved to an address nothing can bind, and both accept loops spun on it

A `docker plugin install` on **Docker Desktop for Mac** put the plugin into a busy loop the moment it
was enabled. The reported symptom was a stream of 8-byte `\x01\x00…` writes to an **eventfd** — which
is mio's waker, i.e. a tokio runtime being unparked over and over, not a task spinning inside one.
That distinction did most of the work: a spinning `accept()` floods `accept4`, so the eventfd traffic
was the *other* workers being woken by a task that kept rescheduling itself.

### Bisecting it with `docker plugin set`

The reporter narrowed it by disabling subsystems from the environment, which is the whole reason every
knob is an env var:

| Setting | Busy loop |
|---|---|
| `IMBH_LISTEN_ADDR=` + `IMBH_GRPC_LISTEN_ADDR=` + `IMBH_DOCKER_NETWORK_REFRESH=0` | no |
| either listener back at `auto` | **yes** |
| both listeners at a literal `HOST:PORT` | no |

Both listeners, independently. `serve_on_listener` (HTTP) and `serve_grpc_on_listener` (gRPC) share no
code — the only thing above both is `docker::serve::supervise`, and the only thing that changes with
`auto` is that the address comes from discovery. So the address itself was the input to blame.

### The bug: `getifaddrs` reports every address, not just the bindable one

`ifaddrs()` accepts `AF_INET6`, and nothing downstream filtered by address family:
`bridges_from_ifaddrs` tested the interface *name* and the sysfs `bridge/` probe only, and
`Snapshot::gateways()` collected whatever came back. A Docker bridge with IPv6 enabled carries an
IPv6 **link-local** (`fe80::/10`) alongside its routable address — Docker Desktop's VM has one on
`docker0`, and so does a plain Linux host with IPv6 on. `BindSpec::resolve` dutifully bracketed it into
`[fe80::…]:4318`, and Linux `inet6_bind` refuses a link-local whose `sockaddr_in6` names no scope.
There is nowhere in an `IpAddr` — or in the `HOST:PORT` string built from one — to carry the interface
that would supply that scope, so the address can never bind, and the supervisor retried it on every
refresh for ever.

### The amplifier: neither accept loop could survive a bad socket

Whatever the address, no accept loop should be able to spin, and both could:

- `lib.rs`'s HTTP loop had `Err(_) => continue` with a comment assuming every accept error is
  per-connection (`ECONNABORTED`, `EMFILE`) and transient. A persistent one retries at full speed.
- `grpc.rs`'s `Allowed` stream passed `Ready(Some(Err(_)))` straight to tonic, which treats it as a
  connection that did not happen and comes back for the next one immediately. Same loop, different
  crate.

That is why *both* listeners spun on one bad address, and why the wake-ups looked like runtime unparks
rather than accept storms.

### The fix, in three parts

1. **`is_usable_gateway`** in `networks.rs`, applied to both discovery backends: drops link-local
   (v4 and v6), loopback, and the unspecified address. `0.0.0.0`/`::` matter beyond bindability —
   binding the unspecified address would serve every interface the host has, which is the exposure
   `auto` exists to avoid. A routable IPv6 (`fd00:d0::1`) is still discovered; the existing test for
   that still passes unchanged.
2. **Exponential accept backoff** shared by both loops (`ACCEPT_RETRY_MIN` 5 ms → `ACCEPT_RETRY_MAX`
   1 s), with the counter cleared by every accept that works — so a healthy listener never touches the
   timer. After `ACCEPT_FAILURES_BEFORE_RETIRING` (64, i.e. over a minute of solid failure) the
   listener stops instead of retrying for ever.
3. **`supervise` retires a finished listener task**, so retiring is what enables recovery rather than
   what prevents it: the address is rebound on the next tick. Without this a socket that stopped
   accepting would sit in `live` for ever and the endpoint would be silently gone.

### Verified

`fmt --all --check` clean; `clippy --workspace --all-targets -D warnings` clean; `clippy -p imbh-server
--features docker,docker-remap,grpc --all-targets` clean; `test --workspace` clean (64 suites);
`test -p imbh-server` with the plugin feature set **233 passed, 0 failed, 1 ignored** (4 new
regression tests: the link-local exclusion, the loopback/
unspecified/v4-link-local set, the API backend's copy of the rule, and the backoff's bounds under
overflow-sized failure counts).

## Preparing v0.6.2: a one-bug patch release, and a binary size that is identical by coincidence (2026-08-07)

Bump the shared workspace version and close the changelog. Everything since v0.6.1 is a single bug
report landing (PR #38): `IMBH_LISTEN_ADDR=auto` resolving to an address nothing can bind, and the two
accept loops that spun on it. `[Unreleased]` was empty, so the whole `[0.6.2]` section is written from
the commits — two `### Fixed` items, the discovery filter and the accept backoff, in the order they
were found rather than the order they were committed, because the second only matters as the reason
the first was fatal rather than merely useless.

### Why a patch bump, and why that is not a judgement call this time

0.6.2 under the same 0.x rule the last three releases used, where a *minor* bump means something
broke. Nothing here can: `is_usable_gateway` is private, `accept_backoff` and
`ACCEPT_FAILURES_BEFORE_RETIRING` are `pub(crate)`, `serve_on_listener` and `bridges_from_ifaddrs`
were already `pub(crate)`, and neither accept loop changed signature. The only behaviour an existing
deployment can observe is `auto` no longer offering an address that never bound — which is the
removal of a listener that did not exist.

Unlike v0.6.1, this needed no argument about whether a fix wearing a feature's clothes deserves a
minor bump. It is a fix wearing a fix's clothes.

### The footprint numbers are identical to v0.6.1, and that is real

The gate reports `imbhd` at **34,916,248 B** and the plugin feature set at **40,063,344 B** — byte for
byte what v0.6.1 measured. That is exactly the shape of a stale-artifact bug, so it was checked rather
than reported:

- `target/release/imbhd` is stamped after the source files, and `strings` finds the new
  `consecutive accept failures` message in it.
- `target/footprint-plugin-probe/release/imbhd` likewise, with all three new strings.

Both binaries are genuinely current; the sizes coincide because the added code is a few hundred bytes
and fat-LTO output lands in the same section-aligned size. Recorded because "unchanged" and "not
rebuilt" are indistinguishable from the number alone, and the gate's own comment (§footprint-gate.sh
line 76) exists because that confusion has bitten this repo before in the other direction.

### Still by hand, for the fourth time

`README.md` (3 strings) and `docs/DOCKER_LOG_DRIVER.md` (2) moved to 0.6.2 by hand again: the
`pre-release-replacements` for them live under `[workspace.metadata.release]`, where cargo-release
does not read them, and the `pre-release-hook` would still run `git cliff -o CHANGELOG.md` with no
`cliff.toml` in the repo. TODO.md's item is updated from "three releases and counting" to four rather
than left to inherit a fifth.

`THIRD-PARTY-NOTICES.txt` is regenerated; its only delta is the 14 workspace crates moving to 0.6.2,
so the third-party set is unchanged since v0.6.0.

TODO.md's first open item — v0.6.1 prepared but not cut — is closed: `v0.6.1` was tagged and published
2026-08-07T07:59Z with six release assets. It is replaced by the same entry for this release.

### Verified

`fmt --all --check` clean; `build --workspace`, `clippy --workspace --all-targets -D warnings` and
`test --workspace` all clean (**64 suites, 586 passed, 0 failed, 4 ignored** — one more than v0.6.1's
585, which is the backoff-bounds test; the other three new tests are behind the `docker` feature and
do not run in the default path). `./scripts/license-gate.sh` OK. Footprint gate **OK**: 275 crates
(target 275), `imbhd` 33.3 MiB, plugin feature set 397 crates / 38.2 MiB (informational, unchanged),
idle RSS 14.9 MB, steady RSS 95.5 MB, search-off lever 275 → 217 → 71.

## The hand-rolled JSON codec goes away, and the emitter turns out to be the harder half (2026-08-08)

`imbh-core` carried two hand-written pieces of JSON: a 313-line recursive-descent parser (`json.rs`)
and a string-builder canonical encoder (`canonical.rs`). Both are now `serde_json`. The interesting
part is that they had *very* different cases against them, and the naive replacement is wrong for
both.

### The parser had a real bug hiding behind a "documented limitation"

`json.rs` carried a module-doc note: "`\uXXXX` surrogate pairs are not recombined (the canonical
encoder never emits them)". That parenthetical is true and irrelevant — the parser is not only fed
imbh's own output. `imbh::parse_json` is also what the Docker log-driver decodes plugin requests and
Engine API responses with. Measured:

```
input = "😀"      (U+1F600, the escaped form Python's json.dumps emits by default)
ours  = None                ← whole document rejected
serde = Some(String("😀"))
```

`char::from_u32(0xd83d)` returns `None`, and the `?` propagates out of `string()` all the way to
`parse`, so *one* astral-plane character anywhere in a document loses the document. Both call sites
swallow it: `Attributes::from_canonical_json` does `unwrap_or_default()`, `docker/json.rs::parse`
degrades to an empty object. Silent empty attribute map, no error anywhere.

The grammar was also lax in the other direction — `+5`, `007`, `1e400` (→ `inf`), and raw control
bytes inside string literals all parsed. Neither direction is defensible for ~300 lines of code.

### The emitter's case was the opposite, and the compatibility break is real

`canonical.rs` is not a JSON emitter; it is a byte-identity spec that dictionary encoding, the
Tantivy feeder, and `json_get_str` all depend on agreeing on. `serde_json` cannot produce it as-is:

| case | old (`f64: Display`) | serde_json |
|---|---|---|
| `1e300` | 301 digits | `1e300` |
| `1.0` | `1` | `1.0` |
| `1e-7` | `0.0000001` | `1e-7` |
| NaN / ±Inf | `{"$f":…}` | `null` |

So switching the emitter **is** a data-format change, and it was taken deliberately (the compatibility
constraint was explicitly waived) rather than discovered. Worth recording the one upside, because it
is not obvious: the old form was *not* round-trip type-preserving — `Double(1.0)` encoded as `1`, and
`parse("1")` gives `Int(1)`. With `1.0` on the wire the fractional marker survives, so `Double` stays
`Double`. There is now a `round_trip_preserves_int_vs_double` test asserting it.

### Neither half goes through `serde_json::Value` — and that is the load-bearing decision

The obvious implementation (`to_string(&Value)` / `from_str::<Value>()` plus a conversion) is wrong
here twice over, both times because of `Value`'s map:

1. **Write side.** `serde_json::Map` is a `BTreeMap` *in this build*. Relying on that for the
   sorted-key invariant means the invariant silently becomes insertion order the day anything
   anywhere in the graph turns on `preserve_order` — Cargo features unify globally, and
   ARCHITECTURE.md §10.16.1 already notes we deliberately leave that feature off for the MCP
   endpoint's sake. Byte-identity is not something to hang on another crate's feature flag. The
   encoder therefore sorts explicitly and streams pairs through `Serializer::collect_map`, which
   writes in *iterator* order and is immune. Bonus: `collect_map` also preserves duplicate keys,
   matching the old encoder, where a `Map` would collapse them.
2. **Read side.** `AnyValue::Map` is an **ordered** pair list — a documented property — and a `Value`
   round trip would re-sort it. So the reader is a `deserialize_any` `Visitor` over a private
   `Parsed(AnyValue)` newtype, which also skips the intermediate tree entirely.

The newtype is not optional, either: under the `serde` feature `AnyValue` already derives an
externally-tagged `Serialize`/`Deserialize` (`{"Str":"x"}`) for the DTO wire form, so inherent impls
would collide. Two JSON representations of one type, and they must stay distinguishable.

The depth guard came out for free: `MAX_DEPTH = 128` was exactly `serde_json`'s own recursion limit,
so the 5000-deep nesting test passes unchanged against the new implementation.

### The dependency is not free, just free where the gate looks

`serde_json` + the `serde` **traits** are now unconditional `imbh-core` deps. Deliberately
`default-features = false, features = ["std"]` on `serde`: the codec needs the traits, not the derive
macro, so `serde_derive` and its proc-macro build stay behind the existing optional `serde` feature
(which changed meaning from `dep:serde` to `serde/derive` — same name, same effect downstream).

| build | before | after |
|---|---|---|
| `imbh` default (the gate axis) | 275 | **275** |
| `imbh --no-default-features` | 71 | 76 |
| `--features ingest` (M6c producer) | 95 | 100 |
| `--features query` | 210 | 211 |

The gate axis is unchanged because `arrow-json` already drags `serde_json` in under DataFusion. The
+5 lands entirely on the trimmed producer/consumer graphs the M6c axis exists to keep small — that is
the actual price of this change, and it is the reason the parser was hand-rolled in the first place.

### Follow-up: the hand-rolled base64 went too, and it was free

The first pass left `base64_into` in place and the rationale written down was "the sentinel, base64,
and key sorting stay imbh's own, since no stock JSON library expresses them". Two thirds right. No
JSON library expresses the `{"$f":…}` sentinel or the key sort — but base64 is not a JSON feature,
and the `base64` crate's `STANDARD` engine is exactly RFC 4648 standard-alphabet-with-padding. Worse,
`imbh-mcp` was *already* encoding the same `AnyValue::Bytes` through that same engine
(`general_purpose::STANDARD`), so the workspace was carrying two implementations of one encoding and
only one of them had tests.

The cost turned out to be zero on **every** axis, not just the default one, which is worth recording
because the instinct in this repo is to assume otherwise: `arrow-cast` and `parquet` both depend on
base64, and `arrow` is a *non-optional* dep of `imbh` and `imbh-storage` — so base64 survives even
`--no-default-features`. Crate counts after the swap are 275 / 76 / 100, unchanged from the
serde_json pass. 20 lines of bit-twiddling deleted for nothing.

The `base64_matches_rfc4648` assertions pass **unchanged**, which is the whole verification argument:
byte-identical output, so unlike the double-formatting change this half is a pure refactor with no
data-format consequence.

### Verified

`fmt --all --check` clean; `build --workspace`, `clippy --workspace --all-targets -D warnings`, and
`test --workspace` all clean (**64 suites, 592 passed, 0 failed, 4 ignored**, up from 586 — six new
tests: surrogate-pair recombination, the strictness matrix, int-vs-double round trip, input-order key
preservation, duplicate-key retention, and one end-to-end
`integral_double_attribute_survives_the_canonical_round_trip` in `imbh` that ingests an OTLP
`DoubleValue(1.0)` beside an `IntValue(1)` and asserts both survive as their own type in
`LogEntry.attributes`). No *pre-existing* test needed changing, which is itself worth noting: nothing
in the suite pinned an integral double's canonical spelling — the nearest test
(`numeric_matchers_match_typed_numeric_attributes`) used 0.1/0.9/0.95, all non-integral — so the
format change would have shipped unnoticed without reading the encoder. That gap is what the new
end-to-end test closes; under the old encoder it fails, reading back `Int(1)`.

Feature matrix rebuilt separately: `--no-default-features`, `--no-default-features --features
ingest`, `-p imbh-core --features serde`, `-p imbh --features serde`, `-p imbh-core --all-features`
all green. `./scripts/license-gate.sh` OK, and `THIRD-PARTY-NOTICES.txt` needs no regeneration —
every crate the codec adds to `imbh-core` (`serde_json`, `serde`, `serde_core`, `itoa`, `zmij`) was
already in the workspace-wide graph, so the third-party set is unchanged.

Footprint gate **OK**: 275 crates (target 275, unchanged), `imbhd` **34,916,248 B = 33.3 MiB —
byte-identical to v0.6.2**, which is the cleanest possible confirmation that the default build was
already linking serde_json. Plugin feature set 397 crates / 38.2 MiB (informational, unchanged), idle
RSS 14.8 MB, steady RSS 104.8 MB. The search-off lever moved exactly where predicted: 275 → 218 → 76,
against v0.6.2's 275 → 217 → 71.

The gate was run twice — once after the serde_json pass, once after the base64 pass — and every
failing axis reported the same numbers both times (275 crates, 34,916,248 B, 275 → 218 → 76); only
the measurement-only idle RSS moved, by 0.1 MB, which is noise. That is the evidence that the base64
swap is footprint-neutral rather than merely assumed to be.

## 2026-08-08 — imbh-tui histogram catalog: discovery, grouping, and a head-API series fuse

Two reported defects in the Metrics catalog for histogram metrics. They share a root cause and turned
up a third, unreported one in the head API on the way.

### 1. A histogram had no dimensions to filter by

`fetch::discover_dims` discovered a metric's groupable axes by evaluating its **bare PromQL
selector** over the metric's whole retained span and reading the label keys/values off the returned
series. That works for a gauge and a sum. It cannot work for a cumulative histogram: `parse_expr` in
`imbh-lgtm` refuses a bare histogram selector outright ("histogram selectors require canonical
`histogram_quantile(sum by (le, ...)(rate(...)))`"), because the buckets are not a scalar series.
`discover_dims` swallows every failure into `Vec::new()`, so a histogram silently got
`dims = Some([])` → the tree rendered `(no dimensions)` → no axis existed to check, and the metric
could never be filtered. Confirmed by reproduction before touching anything: `discover_dims` returned
`[]` for a histogram whose data carries both `route` and `service`.

There is no PromQL phrasing that fixes this — the labels are what you are trying to discover, so you
cannot name them in a `sum by (…)` up front, and `attributes/keys` is cross-signal and says nothing
about which metric carries what. Discovery had to stop being an evaluation. Added
`Db::metrics().dimensions(metric)` (a `SELECT DISTINCT service, attributes` union over the five
metric tables, folding the promoted `service` column in under the name a PromQL label set gives it)
and a head operation over it, so both backends answer identically. It is kind-agnostic, exact,
picker-independent, and bounded by no evaluation cap — all properties the old path only approximated.
`metrics().series()` stays as it was: the raw per-series attribute sets, resource axis excluded.

### 2. Several selected histograms showed as one series

`build_metric_query` emitted `histogram_quantile(0.95, sum by (le) (rate(m_bucket[5m])))`. A quantile
is only expressible as an aggregation, so every label the `sum by (…)` list omits is summed away —
a metric split over N label sets plotted as **one** anonymous `{}` series, where a gauge selected
whole plots N. With defect 1 in force there were no discovered axes to name, so the two defects held
each other up. The grouping now names `le`, `__name__` and the metric's own discovered axes.

`__name__` in the grouping does *not* survive: `LabelSet::by`/`without` drop it unconditionally
(Prometheus semantics). Worth recording — it means **no** aggregation result can carry its metric
name, `rate()` included, so the comment in `fetch.rs` claiming each sub-query's series "keeps its
`__name__`" was wrong for sums too, not just histograms. Only a bare selector keeps it.

So the TUI now runs the catalog's sub-queries **one at a time** rather than as one `EvalRequest`
batch, and synthesizes `__name__=<metric>` onto each result. Only when more than one query runs: a
single (possibly hand-typed) query is already unambiguous, and naming an arbitrary expression from
its leading identifier would be a claim we cannot make. The `_bucket` suffix is trimmed for a
`histogram_quantile` query, which also repairs the metric-detail exemplar lookup for histograms (it
was looking up `latency_bucket`, a name no exemplar is stored under). The batch endpoint stays for
callers that do not need to attribute results; the cost it now documents is provenance, not just the
extra catalog reads it saves.

### 3. (Unreported) a remote head fused two same-labelled series into one

Found while reproducing #2. `ipc::series_from_batch` reconstructed the series boundaries by
**grouping runs of consecutive rows with equal labels**. Within one evaluation that is exact — the
evaluator builds from a `BTreeMap<LabelSet, _>`, so a label set identifies a series. But
`exec::promql` concatenates one evaluation per requested query, and since aggregation drops
`__name__`, two queries answering with identical labels is ordinary rather than pathological. The
decoder merged them: `imbh-tui --url` showed **one** row where the local backend showed two, over the
same data. That is precisely the property `head_e2e.rs` exists to defend, and no case covered it.

Fixed by recording each series' starting row offset in the IPC schema metadata (`imbh.series.starts`)
— the module already documents "anything not row-shaped rides in the schema metadata", and it leaves
the column schema, and so the `prom_matrix_schema` parity test, untouched. A batch without the
metadata falls back to the old grouping, so a new client still reads an old daemon.

### Verified

`fmt --all --check` clean; `build --workspace`, `clippy --workspace --all-targets -D warnings`, and
`test --workspace` all clean (**64 suites, 600 passed, 0 failed, 4 ignored**, up from the 592 of the
serde_json entry above — the eight new tests listed below). No dependency or feature change, so the
footprint gate is not implicated — `Cargo.toml`/`Cargo.lock` are untouched.

Rebased onto the canonical-JSON refactor before merging, and re-run there: the two conflicts were
both append collisions in `CHANGELOG.md` and this file, and the codec swap does not reach this work —
`dimensions()` reads only `AnyValue::Str` attributes, so the `Double` re-spelling that entry calls a
data-format break cannot touch it.

New regression tests: `imbh` `metrics_dimensions_cover_every_kind_and_the_service_axis`;
`imbh-head` `two_series_with_the_same_labels_stay_two_series` and
`a_batch_without_boundary_metadata_falls_back_to_label_runs`; `imbh-server` `head_e2e` gained a
colliding-label batch case and a `metrics/dimensions` parity case; `imbh-tui`
`fetch::histogram_catalog` (dimensions offered, a checked value actually filtering, several
histograms staying distinguishable) plus `a_whole_histogram_keeps_its_identity_and_its_axes` and
`a_query_names_the_metric_it_visualizes`. `imbh-test-support` gained `otlp_hist_labeled` — a
multi-point, attributed cumulative histogram, which no existing fixture provided.

### Open

- `Options::max_series` now doubles as the per-axis value cap for dimension discovery. It is a
  reasonable bound but not the same quantity; if a picker ever needs its own limit, `max_values` is
  already a distinct field on `dto::MetricDimensionsRequest` (and `truncated` is already reported).
- The additions are API-additive across `imbh` and `imbh-head`, so the next release is a minor bump,
  not a patch.

## The TUI query box grows a real caret, and completion had to follow it (2026-08-08)

`imbh-tui`'s two text editors — the query box and the absolute-range form — were append-only:
`Backspace` popped the last byte, a character pushed onto the end, and the block caret was a reversed
space drawn *after* the text. Fixing a typo in the middle of
`http_requests_total{service="checkout",method="POST"}` meant deleting back to it. This adds caret
movement (`←`/`→`, `Ctrl-B`/`Ctrl-F`, `Home`/`End`, `Ctrl-A`/`Ctrl-E`) plus the deletions a mid-string
caret implies (`Delete` / `Ctrl-D` forward, `Ctrl-K` kill-to-end-of-line), and scrolls the box
horizontally when the query outgrows it.

`Ctrl-K` follows Emacs `kill-line` rather than "truncate at the caret", which is not pedantry here:
the box really can hold newline-joined queries (the catalog's multi-metric "visualize" writes them),
so a kill stops at the next `\n`, and a caret sitting *on* the break kills only the break and joins
the two lines. Nothing is stashed — there is no yank to pair a kill ring with.

### The caret is one byte offset, and every read clamps it

`App::query_cursor` is a byte offset into the *active* query — but which buffer is active follows the
screen (`query: [String; 4]`), and Back/Forward, a screen switch, and the catalog's "visualize" all
swap that buffer without the editor's knowledge. Rather than track the caret per buffer, every read
goes through `App::query_caret`, which clamps into the current buffer and floors onto a character
boundary; `begin_editing` (the only door into `Mode::Editing`) parks it at the end. That makes a
stale offset unobservable instead of a panic waiting on a `&query[..caret]` slice. `set_active_query`
is the new way to replace a buffer wholesale, so the two can't drift.

### Completion classifies the prefix, not the buffer

`completion_context` was documented as "the caret is always the end of the string", and three call
sites passed `active_query()`. With a movable caret those become `query_before_caret()`, and
`accept_completion` switches from `truncate` + `push_str` to `replace_range(caret - token_len..caret)`
— it replaces the token *before* the caret and leaves the tail alone. Caret movement also re-derives
the popup, since which token is being completed is a function of where the caret sits. This was the
non-obvious half of the change: the completion machinery silently assumed append-only editing, and
nothing in the type system said so.

A related latent bug fell out: `KeyCode::Char(c)` in edit mode had no modifier guard, so `Ctrl-B`
already typed a literal `b`. Every Emacs binding would have inserted its own letter without the
`!ctrl && !alt` guard now on that arm.

### Rendering a caret inside coloured text

The lexer emits `Vec<Span>` and its output is not char-for-char with its input in one place: a `\n`
(queries are stored newline-joined for multi-metric visualization) renders as a three-column
separator. So `highlight_query` became a thin wrapper over `highlight_spans`, which pairs each span
with the **source byte range** it came from; `highlight_caret` splits the span containing the caret
and reverses exactly that character, falling back to marking the whole span when the rendered width
and the source width disagree (the newline case). While restructuring the lexer's loop into
one-span-per-iteration, the whitespace run stopped swallowing `\n` — previously a newline adjacent to
a space landed in a raw span and reached the single-line `Line` as a literal control character.

### Overflow: scroll by the least amount that keeps the caret visible

`Paragraph::scroll((0, x))` on the unwrapped query line, with `x = caret_column - (inner_width - 1)`,
saturating at 0. The caret column is a display width (`unicode-width`), not a byte offset, with each
newline counted as the separator it renders as. At rest the line stays pinned to its start, which is
the pre-existing behaviour; only editing scrolls. A render test at 48 columns asserts the caret sits
on the last column inside the border with the query's tail visible, and on the first column after
`Home` with its head visible.

### The second editor made the abstraction pay for itself

The absolute-range form (two datetime fields) was the same append-only editor — `pop()` on Backspace,
`push()` on a character, a caret drawn after the text — so "the same treatment" would have been a
second copy of the caret arithmetic. Instead the whole thing moved into a new `textfield` module: a
borrowed `TextField { text: &mut String, caret: &mut usize }`, the eight editing primitives, and one
`handle_edit_key` that owns the key map (cursor keys, the Emacs aliases, `Backspace`, `Delete`/
`Ctrl-D`, `Ctrl-K`, and the guarded character arm). Both editors now bind their *own* keys first and
hand the rest to that function, which reports whether it took the key — so the query box can refresh
its completion popup on exactly the keys that changed something, and the form doesn't have to care.

The form's two fields share one caret (`abs_cursor`), re-seated at the end of whichever field takes
focus, since `Tab`/`↑`/`↓` are field switches. Resuming the old field's offset on the way back would
be the other defensible choice; parking at the end matches how the form opens and how `begin_editing`
behaves, so both editors read the same way.

Its overflow handling had to be done by hand rather than with `Paragraph::scroll`: the two fields and
the hint line share one paragraph, so scrolling the widget would drag the *other* rows sideways too.
`caret_spans` therefore builds one cell per character, accumulates display columns (a column is not an
index once a character is wide), and emits only the window containing the caret. Overflow needs ~19
characters of junk in a 48-column popup to trigger, but it is exactly the case where a user is typing
blind, which is the thing this change set is about.

### Verified

`fmt --all --check` clean; `build --workspace`, `clippy --workspace --all-targets -D warnings`, and
`test --workspace` all clean. `imbh-tui` goes 118 → 141 tests: caret movement and its Emacs aliases,
backspace/delete/kill around the caret, whole-character steps over a multi-byte `é`, the modifier guard
(no `Ctrl-`/`Alt-` letter reaches the buffer, but `Shift` still types), mid-query completion accept,
caret clamping across a buffer swap, the reversed-character assertions on `highlight_caret`, the
horizontal-scroll render test, the `textfield` primitives on their own (including "a declined key
never reaches the buffer"), an end-to-end edit of the range form (in-place minute fix → field switch →
`Ctrl-A`/`Ctrl-K` → a rejected commit → a good one), and the popup field's caret/window spans. The
twelve existing completion tests moved from `*app.active_query_mut() = …` to `set_active_query`, which
is what "the query, caret at the end" now means. No dependency change, so no footprint gate movement
is possible.

## Preparing v0.7.0: a minor bump that no signature justifies, and a lever row nobody had recorded (2026-08-08)

Bump the shared workspace version and close the changelog. Three PRs since v0.6.2: the canonical-JSON
codec swap (#41), the `imbh-tui` histogram catalog fixes (#42), and the query/range editor caret (#43).
Two of the three had already written their own `[Unreleased]` entries, so most of `[0.7.0]` is
inherited rather than reconstructed from commits — the exception is described below.

### Why a minor bump, when nothing in the public API broke

The public surface since v0.6.2 is **purely additive**: `Db::metrics().dimensions()`, the
`POST /api/head/metrics/dimensions` route with its four DTOs, and `HeadClient::metric_dimensions`.
Under the 0.x rule the last four releases used — a *minor* bump means something broke — a signature
audit alone would have said 0.6.3.

What broke is the **stored data**. The canonical-JSON emitter now spells an integral `Double` as
`1.0` where it used to spell it `1`, and large magnitudes in exponent form. Semver has no vocabulary
for that: it constrains the *API*, and a consumer reading the version number cannot see a format
change in a patch digit. So 0.7.0 is chosen for what the number *communicates*, not for what a
signature diff would compute. This was the user's call, and it closes the TODO item that was holding
the release ("Decide the version for the canonical-JSON format change").

That item's part (b) — whether the release notes should say anything about mixed-vintage segments —
is answered in the `[0.7.0]` `### Changed` entry with a short paragraph, and its useful content is
the *negative* result: there is no rewrite path and none is needed. Both spellings decode to the same
`Double`, so only exact-string matching (a dictionary/term equality filter, a `json_get_str`
comparison) can observe the difference, and only for a `Double` whose value is integral. Compaction
is explicitly named as *not* a migration: `compact_partition` concatenates already-encoded columns, it
does not re-encode them, so a compaction pass will not normalise old segments. That is the part a
reader would otherwise assume.

### The caret feature had no changelog entry at all

PR #43 (the query/range editor caret) touched eleven files and 1,082 lines and did not touch
`CHANGELOG.md` — the only one of the three that didn't. `[Unreleased]` therefore described two thirds
of the release, and closing the section as-is would have shipped a user-facing feature silently. It
was caught by diffing `v0.6.2..HEAD --stat` against the sections present, not by reading the
changelog, which is worth noting as the check that works: the changelog cannot tell you what is
*missing* from it. Entry added under `### Added` before the section was stamped.

### `imbhd` is byte-identical to v0.6.2 for the second release running

34,916,248 B again — the same number the v0.6.2 entry flagged as "exactly the shape of a stale-artifact
bug". Coming round twice is more suspicious than once, so it was checked the same way and then some:

- `target/release/imbhd` is stamped 03:19:58 today, after the fat-LTO relink that the gate triggered
  (the version bump alone changes crate metadata, so nothing in the graph could have been reused).
- `strings` finds `/api/head/metrics/dimensions` in it — a route that does not exist in v0.6.2.
- The **plugin feature set did move**: 40,063,344 → 40,128,880 B (+64 KiB). Two binaries built by the
  same gate invocation, one changing and one not, is the corroboration a single unchanged number
  cannot supply.

So the coincidence is real, twice. `-C opt-level=s -C lto=fat` output lands in the same
section-aligned size for a few hundred bytes of added code, and the default `imbhd` gets less of this
release's change than the plugin build does.

### The 217 → 218 row, and a QUALITY_GATE table three releases stale

The gate printed the search-off lever as **275 → 218 → 76** where `QUALITY_GATE.md` §2 documented
275 → 217 → 71. The `-5` on the bare `--no-default-features` build was already recorded (CHANGELOG,
ARCHITECTURE.md §11): the codec swap makes `serde_json` + the `serde` traits unconditional in
`imbh-core`. The `-1` on the `ingest,query` row was not recorded anywhere, so it was measured against
a throwaway worktree at `v0.6.2`:

| config | v0.6.2 | v0.7.0 |
| --- | --- | --- |
| default (`ingest,query,search`) | 275 | 275 |
| `--no-default-features --features ingest,query` | 217 | **218** |
| `--no-default-features --features query` | 210 | 211 |
| `--no-default-features --features ingest` | 95 | 100 |
| `--no-default-features` | 71 | **76** |

The added crate on the `ingest,query` row is exactly one, and it is `serde` itself. `serde_json`
1.0.151 depends on `serde_core`, not `serde`, so arrow's copy of serde_json never pulled the traits
crate into that graph; naming `serde` in `imbh-core` is what puts it there. (On the bare build the
five are `serde`, `serde_core`, `serde_json`, `itoa`, and `zmij` — serde_json's float formatter,
which is why `ryu` is not among them.)

`QUALITY_GATE.md` §2 is updated with all of it, including the derived breakdown (`query` now accounts
for 135 of the 142, `ingest` for 24, overlapping by 17) and the binary-size line, which still read
**31.2 MiB / 2026-07-18** — a number from before v0.5.0, so it had survived three releases of the
gate reporting 33.3 MiB without anyone reconciling the two.

### Still by hand, for the fifth time

`README.md` (3 strings) and `docs/DOCKER_LOG_DRIVER.md` (2) moved to 0.7.0 by hand again, for the
reason TODO.md has recorded since v0.5.0: the `pre-release-replacements` for them live under
`[workspace.metadata.release]`, where cargo-release does not read them, and the `pre-release-hook`
would still run `git cliff -o CHANGELOG.md` with no `cliff.toml` in the repo. The item's count goes
from four releases to five.

`THIRD-PARTY-NOTICES.txt` is regenerated; its only delta is the 14 workspace crates moving to 0.7.0.
The third-party set is unchanged since v0.6.0 — which is itself the confirmation that the codec swap
cost no *new* third-party crate on the shipped `imbhd`/`imbh-tui` graphs, only on the trimmed ones.

TODO.md's first open item — v0.6.2 prepared but not cut — is closed and verified rather than assumed:
the `v0.6.2` Release is published (2026-08-07T12:12Z, six assets) and `ghcr.io/moriyoshi/imbh-log-driver`
carries both `0.6.2-amd64` and `0.6.2-arm64`, so the `auto` listener fix has actually reached
installs. It is replaced by the same entry for this release.

### Verified

`fmt --all --check` clean; `build --workspace`, `clippy --workspace --all-targets -D warnings` and
`test --workspace` all clean (**64 suites, 615 passed, 0 failed, 4 ignored** — up from v0.6.2's 586,
almost all of it `imbh-tui`, which goes to 146 unit tests on the caret and catalog work).
`./scripts/license-gate.sh` OK. Footprint gate **OK**: 275 crates (target 275), `imbhd` 33.3 MiB,
plugin feature set 397 crates / 38.3 MiB (informational), idle RSS 14.9 MB, steady RSS 104.9 MB,
search-off lever 275 → 218 → 76. The §3c packaging dry-run staged and verified **all 20 members at
0.7.0**, exit 0 — with `--allow-dirty`, because the bump is not committed and `cargo package` refuses
a dirty tree; the real release runs it on a committed one, so the flag changes nothing it verifies.
Nothing tagged, nothing published.

## Segment pruning: three stale doc claims, 11.9x on a time-bounded query, and a plan measured into the bin (2026-08-08)

This entry folds in `PROMOTED_ATTR_PUSHDOWN_PLAN.md`, which is deleted — its conclusion was "do not
build this", so it survives here rather than as a standing plan.

It started as a design question — should the backing store move to Tantivy, Quickwit-style? — and the
answer turned out to depend on facts the canonical docs got wrong. Recommending against the migration
was right, but the *reason* offered was wrong, and finding that out is what produced everything else.

### Three things ARCHITECTURE.md asserted that the code did not do

1. **§6.1 item 3 and §8: "the Tantivy `attrs` JSON field is not built."** It is built, and its
   equality push-down is wired. `imbh-index` indexes every string-valued record attribute as
   `attrs.<key> = <value>` with the verbatim tokenizer, `search_attr_eq` resolves it, and
   `provider.rs` pushes it as `Inexact` into the cost-gated `RowSelection`. The design discussion that
   opened this session read those two sections and proposed as *new work* something the tree already
   shipped.
2. **§9.2: "Exact: time-range predicates … segment prune via manifest, row-group/page prune via
   Parquet stats."** Wrong twice over. Nothing in the tree ever returned
   `TableProviderFilterPushDown::Exact`, and there was **no time pruning of any kind**:
   `supports_filters_pushdown` never claimed a time predicate, so none reached `scan()`;
   `Storage::query_snapshot` takes no time argument and returns every segment; the reader built no
   row-group filter. `SegmentRef` has carried `min_time_unix_nano`/`max_time_unix_nano` all along and
   nothing on the query path read them. A `WHERE time > now() - 5m` over 30 days of retention read all
   30 days and threw the rest away in the filter above the scan.
3. **§6.1 item 2: promoted columns materialize "with record `attributes` → `resource` → `scope`
   precedence."** `lookup_promoted` is `json_get(attributes, key)` keeping `AnyValue::Str` — record
   scope only, resource and scope deliberately excluded, and its own doc comment says so. This one was
   load-bearing: record-scope-only is exactly what makes the promoted column and the `attrs` index
   describe the same row set. Had the doc been right, pushing a promoted-column equality into that
   index would have been **unsound** — a row whose value came from `resource` would have a non-null
   column and no index term, and pruning would have silently dropped it.

The pattern is worth naming: all three were *confident* prose, and two of them contradicted other
paragraphs in the same file. §6.1's own push-down paragraph says "the column mirrors the record
`attributes` scope only", four lines under item 2 claiming otherwise.

### What DataFusion actually hands a provider (54.1.0)

Pinned by a throwaway spike that dumped `filters` at both `supports_filters_pushdown` and `scan`,
rather than by reading the optimizer. Every one of these contradicted a guess made in advance:

- `CAST("k" AS VARCHAR) = $1` **survives** the optimizer as a `Cast` — no `unwrap_cast_in_comparison`
  rewrite against a `Dictionary(Int32, Utf8)` column.
- The cast target is **`Utf8View`**, not `Utf8` (§9.1 enables string-view preservation), and in this
  version `Cast` carries a **`field: Field`**, not a `data_type`.
- `Column.relation` is `Some(Bare { table })` in `supports_filters_pushdown` but `None` in `scan` —
  match on `name` only.
- A quoted dotted identifier (`"http.route"`) stays **one** column name; it is not split into
  relation/column. This is the shape nearly every real promoted key takes.
- The bare `col = $1` form arrives with a `ScalarValue::Dictionary(Int32, Utf8(..))` literal.
- **`ShortenInListSimplifier` rewrites an `IN` of ≤ 3 values into an `OR` chain, and
  `SimplifyExpressions` runs before `PushDownFilter`.** Handling `Expr::InList` alone therefore does
  nothing for the common small-k case — the one that matters most for trace search.
- Only filters a provider *claims* reach `scan()`. Verified directly: with the cast-equality
  `Unsupported`, `scan` saw only the `matches` conjunct of a two-conjunct `WHERE`.

### The measurements

`examples/bench --bin prune-bench` (new): 60 segments x 2,000 log rows plus 60 single-trace span
segments, best of 5 after a warm-up, A/B against a pristine worktree at the same commit. It runs
everything through `BlockingDb::sql`, which is what lets one source file compile against both builds.

| query | HEAD | + row-group stats | + manifest range |
|---|---|---|---|
| logs, 1-of-60 time window | 8.71 ms | 2.09 ms | **0.73 ms** |
| logs, full scan | 8.58 ms | 8.82 ms | 8.23 ms |
| trace 2-id fetch, raw `IN` | 7.26 ms | **2.23 ms** | 2.14 ms |
| trace 2-id fetch, `hex()` | 7.35 ms | 7.17 ms | 7.21 ms |
| trace point lookup, raw `=` | 2.16 ms | 2.36 ms | 2.14 ms |

Two HEAD baselines are the diagnosis in one number each: the narrow window cost **1.015x** the full
scan (a time bound bought literally nothing), and raw `IN` cost **0.987x** the `hex()` form, while the
point-lookup `=` shape was already at 0.296x. So the bloom was never broken — only the `InList`/`OR`
shapes were unrecognized, and `TracesApi::search` defeated it anyway by binding `hex(trace_id)`
strings, which yield none of the raw bytes a bloom needs.

**Footer opens were the hidden floor.** Row-group statistics cut the narrow query 4.2x, but a 60x
reduction in rows read should have bought more. The residual was `open_segment` still opening all 60
Parquet footers — roughly 35 us each — and skipping segments on the manifest's declared bounds took
the same query to 0.73 ms, a further 2.9x for **11.9x overall**. Corroborated by the trace point
lookup sitting at the same ~2.1 ms floor, because bloom pruning lives in the footer and still pays the
open. That is the concrete next lead: a manifest-level `trace_id` digest is the same fix one level
down, and should reach the 0.73 ms class.

### The promoted-attribute push-down, measured and rejected

The plan's premise: `attr_field` compiles a promoted key to `CAST("k" AS VARCHAR) = 'v'`, which no
matcher recognized, so promoting a key made its filter **prune less** than leaving it in JSON. True,
and the fix looked small. The conclusion drawn from it — "the un-promoted path is the faster one,
which is the opposite of what `promote` advertises" — was never measured, and is **false**.

`examples/bench --bin attr-bench` sizes it without building it: the push-down would route the column
spelling into the *same* `attrs` index the JSON spelling already uses, so its win is exactly what that
pruning is worth. A DB opened with `promote(["k"])` carries both spellings per row; the pruning
component isolates by re-running with `SELECTIVITY_THRESHOLD = 0.0`.

| selectivity | `count(*)` floor | json + index | json, pruning off | promoted column |
|---|---|---|---|---|
| 50% | 5.34 | 24.10 | 23.82 | **5.47** |
| 10% | 5.02 | 10.47 | 21.55 | **5.53** |
| 1% | 5.13 | 6.13 | 21.35 | **5.68** |
| 0.1% | 5.81 | 5.90 | 21.80 | 6.25 |

Index pruning is worth 11–16 ms on the JSON path, and that saving is almost entirely **avoiding the
JSON parse** on non-matching rows — not I/O. A promoted column has no JSON parse to avoid: removing it
is precisely what promotion already did, so the index arrives with nothing left to save. The promoted
column sits within **0.13–0.55 ms** of the bare `count(*)` floor at every selectivity, and that is the
whole budget the push-down could recover — under 8%, and at 1% it is already faster than the pruned
JSON path. A wide projection (`count(body), count(attributes)`), the case the plan actually claimed,
does not change it.

The only regime where it wins needs ~1,000 distinct values on one key — a high-cardinality key, which
§6.1 tells you not to promote. **The one measured case for the feature requires first making a
promotion decision the design forbids.** Not built.

### Structural wins can be measured on synthetic data; distributional ones cannot

The thing that made all of the above decidable without production data. Time-range pruning saves in
proportion to `segments outside the window / total`, and a bloom in proportion to `segments not
holding the id / total`. Those ratios are set by retention depth and seal cadence, **not** by what the
values look like, so a generated corpus gives a speedup that carries.

Sigma does not have that property. Define sigma(key, value) as the fraction of segments holding at
least one matching row; a segment-granularity index prunes `1 - sigma`. On `gen-demo-db` **every**
sigma is exactly 1.000, across 19 keys and 5 tables — which is a fact about a generator that emits a
fixed label set every step and flushes once per run, not a fact about telemetry. `examples/attr-stats`
(new) measures it over any existing DB with no format change; on the `prune-bench` corpus it reports
`shard` at 60 distinct values and sigma **0.017 = 1/60 exactly**, its first validation against real
variance. Whether a segment-granularity attribute index pays is still open and needs production data.

### Design knowledge that outlived the plan

- **`promote` and a segment index want opposite key sets.** Promotion targets low-cardinality keys
  (dictionary columns compress; promoting a high-cardinality key is the column-explosion trap §6.1
  rejects). Segment pruning pays only for high-cardinality, time-localized values. Scoping a segment
  index to the promote list would cover exactly the keys that cannot prune. Nor does coverage need a
  config at all: a value set has no schema, and `imbh-index` already indexes every string attribute
  unconfigured.
- **Segment-level pruning is all-or-nothing, and the metric layout defeats it.** Metric segments sort
  by time only, so each holds a time slice with every series interleaved; `service`, `env`, or a
  metric name appears in essentially every segment and prunes nothing, forever. The lever is sort
  order — `(series, time)` — not index granularity.
- **`attr-stats`' `hint` column collapses two orthogonal axes.** `shard` is hinted `promote` (60 values
  reads as low cardinality) while its sigma says `segment-index`. Both are right; a key can want a
  promoted column *and* a segment index. It needs a pair of verdicts.
- **Auto-promotion has an unaddressed correctness landmine.** `attr_field` emits the column form iff a
  key is in the *live* promote set, and segments sealed before promotion are null-filled by `coerce`
  — so auto-promoting `k` at time T makes `WHERE k = 'v'` return **nothing** for pre-T segments,
  silently, where it previously worked via `json_get_str`. §6.1's safety argument ("compaction never
  changes an answer") holds only because the promote set is static today. Fixing it needs per-segment
  promote-set metadata, a read-side `coalesce`, or a compaction back-fill.
- **Trace search is a two-phase funnel** — span candidate filter (row-granular, served by the `attrs`
  index) → trace id set → span fetch (segment-granular, served by blooms). Phase 1 stays unpushable:
  its id set is a subquery DataFusion decorrelates, leaving no id predicate on the outer scan.

### Method notes

- **A non-vacuous test for "did not open the file": delete the file.** The manifest-range skip is
  asserted by removing the out-of-range Parquet files before querying, so any `File::open`, footer
  read or `.tidx` search fails loudly. Confirmed to fail when the skip is neutered — where the two
  neighbouring boundary tests still pass, because the footer path covers them.
- **`DbStats` cannot gain a field.** Plain `#[derive(Debug, Clone)]`, no `#[non_exhaustive]`, no
  `Default`; that file has zero such attributes. Adding a field breaks downstream struct literals
  across 12 published crates — which is why the sigma tooling landed as an example binary.
- **`Db::segment_files()` returns empty for read-only handles.** `Storage::open_read_only` leaves the
  in-RAM segment lists unpopulated. A wrong answer with no signal; found, not fixed.

### Verified

`fmt --all --check` clean; `build --workspace`, `clippy --workspace --all-targets -D warnings`, and
`test --workspace` all clean at **640 passing**. No `Cargo.toml`/`Cargo.lock` third-party change, so
no footprint movement is possible. **Breaking for `imbh-query`**: `SegmentInput` gains
`time_range: Option<(i64, i64)>`, `TableInput` gains `time_column: Option<&'static str>`, and
`SegmentTableProvider::new` takes one more argument — public-field structs, so struct-literal
construction breaks. Under Cargo's 0.x rules that needs the minor bump the canonical-JSON change
already puts on the table for 0.7.0; it must not ship as a patch.

## Attribute access: a shipped bug the docs called safe, a 3.2x floor, and two features measured into the bin (2026-08-08)

Second entry of the day. The first ("Segment pruning: three stale doc claims, 11.9x on a time-bounded
query, and a plan measured into the bin") covered segment-level pruning. This one covers the *row*
level — what it costs to read an attribute — under a goal stated mid-session: **the experience should
be decent regardless of which attribute the user picks.**

That framing did most of the work. Promotion is curated by construction — a column per key is the
explosion trap §6.1 rejects — so a demand-driven promoter helps popular keys and never the key
someone picks first time. Optimizing for hot keys is optimizing the case that is already warm. What
serves arbitrary keys is cheap JSON access and indexing; promotion is the last tier, not the first.

### The bug that was already shipped, with a doc asserting the opposite

`promote` is not retroactive: segments sealed before a key was added lack its column and are
null-filled by `coerce`. But `attr_field` emitted only the column form, so `WHERE k = 'v'` matched
**nothing** on every pre-promotion segment — silently, since a missing attribute is a legitimate NULL.
`Promote`'s own doc said "changing the set is backward-compatible". Any host editing their promote
list between runs lost history, guided by that sentence.

Reproduced before fixing: a key promoted between two flushes returned `["late"]` instead of
`["early", "late"]`. Fixed by compiling a promoted key to
`CASE WHEN "k" IS NOT NULL THEN CAST("k" AS VARCHAR) ELSE json_get_str(attributes, $k) END`.

Three things about that spelling, all measured rather than reasoned:

- **`coalesce` is semantically identical** — DataFusion literally rewrites `coalesce` into this
  `CASE` — but costs **+0.24–0.29 ms** against the hand-written form's **+0.04–0.11 ms**, because its
  `WHEN` tests `CAST(...) IS NOT NULL` over the whole batch and the optimizer does *not*
  common-subexpression the cast away. Test the bare dictionary column instead.
- **It is nearly free** because `CaseExpr` takes a whole-batch fast path when the `WHEN` is uniformly
  true or false, and batches never span segments. Post-promotion segments never invoke the JSON arm.
- **The `CAST` itself is pure overhead for filters**: `"k" = 'v'` in dictionary space beats
  `CAST("k" AS VARCHAR) = 'v'` by **0.20–0.31 ms**, *more* than the safety net costs. It earns its
  keep only at projection sites, where dropping it would make a group-by result column's Arrow type
  depend on whether the key happens to be promoted. Filed, not done.

### Tier 1: the floor for any key, on any signal

`imbh_core::json_get` built a `Vec<(String, AnyValue)>` of the entire blob — one allocation per key
and per string value — then linear-searched it for one field. Cost above a `count(*)` floor per 100k
rows: **18.9 ms at 2 attributes/record, 75.1 at 10, 326.7 at 40** — roughly 8 ms per attribute, so a
40-attribute record made an arbitrary filter **36x its own floor**.

Replaced with a targeted walk: skip non-matching values with `IgnoredAny` (no allocation), borrow keys
from the input, fall back to the full parse when that cannot apply. **326.7 → 101.5 ms at width 40**
(3.2x), 75.1 → 31.7 at width 10, 18.9 → 12.4 at width 2.

**The bug in the first version is the durable lesson.** It returned early on finding the key, which
leaves `serde_json`'s parser mid-object; `deserialize_map` then fails to find the closing brace and
**errors**, silently routing into the full-parse fallback. So matching an *early* key cost the aborted
scan *plus* the full parse. It did not fail loudly, it just got slow — a hit on the first key became
~3x slower than a hit on the last. Caught only because the numbers were backwards from the hypothesis
and the anomaly got chased instead of written off as noise. The benchmark was *also* confounded on the
first pass: it varied predicate type along with key position, and `= 'v0'` is the shape the `attrs`
index recognizes, so it carried an index search the `IS NOT NULL` comparison did not.

An early exit on canonical JSON's sorted keys was also considered and rejected: `json_get` is public
and the UDFs can be pointed at any text column, where an unsorted object would report a present key as
missing. Correctness rests on the fallback, not on cleverness —
`json_get_agrees_with_the_full_parse` compares both paths across 13 documents x 4 keys (escaped keys,
surrogate pairs, the `{"$f":…}` sentinel, duplicates, unsorted keys, non-object, malformed).

### What a promoted column actually costs — and the axis that turned out to be wrong

Ran as a hard gate before any policy. 50k log rows + spans + gauges, keys on logs only so the six
all-NULL columns on other signals are included, **one process per count** (sharing a process makes RSS
meaningless: the first count absorbs ~200 MiB of first-touch pages and later ones read ~0; a warm-up
does not fix it, since the allocator does not return pages).

Low-cardinality keys are nearly free: **+1,206 B per key, +2.0% at 20 keys**, bit-exact reproducible.
Seal time and buffer RSS are **below the noise floor** — best-of-5 seal is 487.8 ms at 0 keys versus
**473.3 ms at 20**, and VmRSS is 210,348–210,356 kiB across all counts. (Arithmetic bounds the buffer
cost at 4 bytes/row/key for the `Int32` index array, ~2% of the working set.) Note `DbStats::buffer_bytes`
cannot see promoted columns at all: it sums `Row::approx_bytes()`, the pre-Arrow row size, while
`push_log_batch` builds the batch at ingest.

**Then the framing turned out to be wrong.** "Gate the budget on cardinality" is not right, because
Parquet builds its dictionary **per column chunk** — what costs money is how much a value *repeats
within a segment*. At a fixed 50 segments x 1,000 rows: **+1,206 B/key at 3,125x repetition, +22,067 B
at 50x, +108,842 B at 1x**. The intermediate step is what shows it: spreading 50,000 globally-distinct
values across 50 segments barely helped (+108,842 vs +114,284 B/key), because the values were still
unique per row and segmenting does not reduce how many distinct strings must be stored. The original
`card=50000` run had all 50,000 values inside one segment, so it measured both variables at once.

So a gate on **global** cardinality would reject exactly the keys worth promoting: `pod.name` has
enormous global cardinality but only the currently-running pods appear in any one segment, each on
many rows. Time-locality is what creates the repetition. The quantity is `rows / postings`, and
`attr-stats` already reports both terms.

### `Db::attr_access_stats()` — the half no offline tool can supply

Which keys queries read, how often, by which backend (`Builtin`/`Promoted`/`Json`), most-read first.
Tallied in `attr_field` — the single chokepoint every typed API and LGTM translator funnels through —
at query-*planning* time, once per key per query, never per row. Not persisted: demand is a property
of how a deployment is queried, not of its data. A key accumulating `Json` reads is the promotion
candidate. This also collapsed 21 duplicated `SqlParams::with_promote(...)` call sites onto one
counted constructor.

### Two features measured into the bin

**The promoted-attribute push-down** (rejected earlier the same day, recorded in the first entry): the
`attrs` index buys nothing on a promoted column, because its whole value on the JSON path is avoiding
the JSON parse, and promotion already removed that.

**A Tantivy index on metric segments.** Metric tables have no `.tidx`, so a label matcher is a full
JSON scan — a gap of **+26.0 ms over floor per 100k points** (10 labels/point), *identical* at 50% and
1% selectivity, because JSON cost is per row scanned rather than per row matched. A promoted column
recovers **95–97% at every selectivity**; the index recovers the gap only when the matcher is
selective, and above the ~0.5 hit-fraction gate it declines to prune and returns nothing for its
search. PromQL selectors like `service="api"` are exactly the unselective case. So the index would buy
a Tantivy build at seal on the **highest-volume signal** to help only selective filters on un-promoted
keys. Use `promote` instead — now choosable from data rather than guesswork.

Both rejections share a shape worth remembering: the infrastructure that already existed covered the
case, once the right keys or the right predicate spelling were used. Neither would have been caught by
reasoning; both took a benchmark.

### Verified

`fmt --all --check` clean; `build --workspace`, `clippy --workspace --all-targets -D warnings`, and
`test --workspace` all clean at **643 passing**. No `Cargo.toml`/`Cargo.lock` movement, crate count
unchanged at 282, so no footprint gate movement is possible. `Db::attr_access_stats` plus the
`AttrAccess`/`AttrBackend` types are purely additive; the `CASE` change alters emitted SQL, not any
signature. New harnesses: `examples/bench --bin {jsonattr,promote-cost,metricattr}-bench`, alongside
the `attr-bench` and `prune-bench` from the first entry.

## The spans `.tidx` earns its keep, and locality beats selectivity 5x (2026-08-08)

A short follow-up to the two entries above, answering "could we drop Tantivy from traces/metrics?"
Metrics never had an index, so the question was only about spans — and the answer is **no**, against
a guess of mine that said otherwise.

The spans sidecar measured at **56% of the Parquet it indexes** (logs: **217%**) on a synthetic
corpus, which is what prompted the question. Caveat on those ratios: the corpus has repetitive bodies
that zstd crushes while Tantivy's term dictionaries do not compress the same way, so the direction is
real and the magnitude is inflated.

I then argued from structure that span `name` is low-cardinality, so `matches(name, …)` would be
unselective and the cost gate would decline — making the sidecar dead weight. **That conflated two
different things.** Matching *one* of twenty names is 5% selective. What makes a matcher unselective
is cardinality **1–2**, which is the metric-label shape (`service="api"` in a single-service
deployment), not the span-name shape.

Measured with `examples/bench --bin spanindex-bench`, detecting application exactly rather than by
timing: `ScanStats::rows_scanned` counts rows materialized *after* the `RowSelection`, so
`rows_scanned < total` proves it applied. `stream_with_stats` is the public surface, and its counters
are complete only once the stream is drained.

| scenario | names | /segment | selectivity | rows_scanned | ms | verdict |
|---|---|---|---|---|---|---|
| degenerate | 1 | 1 | 100% | 100,000 | 26.1 | gate declined |
| interleaved, low card | 4 | 4 | 25% | 25,000 | 16.0 | pruned |
| interleaved, mid card | 20 | 20 | 5% | 5,000 | 11.0 | pruned |
| interleaved, high card | 200 | 200 | 0.5% | 500 | 16.9 | pruned |
| localized, high card | 200 | 10 | 0.5% | 500 | **8.5** | pruned |
| localized, very high card | 2,000 | 10 | 0.5% | 500 | **3.3** | pruned |

Two findings, and the second is the one worth carrying:

1. **The gate declines only at cardinality 1–2.** From four names upward the selection applies and
   prunes to exactly the matching rows. The spans index is not dead weight.
2. **At identical selectivity and identical `rows_scanned`, distribution moved wall-clock 5x.** 16.9
   ms interleaved versus 3.3 ms localized, with 500 rows scanned in both. The mechanism is §8's cost
   model: a value confined to a few segments makes the others return an *empty* hit set, so the
   `RowSelection` selects nothing and the segment costs only its open plus index search — an effective
   segment skip. Interleaved, every segment holds matches, so every segment is read and only per-row
   decode is saved. **`rows_scanned` cannot distinguish these**; only time can, which is a caution for
   every counter-based assertion in this codebase.

That is the same time-locality axis that governs promoted-column cost (a key is cheap when its values
repeat within a segment), arriving from the read side. It is also direct evidence that low sigma pays
— constructed rather than observed, so it demonstrates the mechanism without settling whether real
telemetry has that shape, which is still what the segment-digest idea is gated on.

Method note: `examples/bench` gained `tokio` and `futures` as direct dependencies to drive the async
`stream_with_stats` path. Both were already in the graph via `imbh`, so `Cargo.lock` gains no crate
and the facade is untouched.

## Session summary: read-path work, 2026-08-08

Consolidating entry for the day. The three above hold the detail — segment pruning, attribute access,
and the spans-index probe. This one records what shipped, what was measured and *not* built, an audit
that had not been written down, and the method lessons, which are the part most likely to matter next
time.

The day began as a design question — should the backing store move to Tantivy, Quickwit-style? — and
the recommendation against it was right for the wrong reason, because the canonical docs were wrong
about what was built. Chasing that produced everything else.

### Shipped

| change | effect |
|---|---|
| Time-range pruning via Parquet row-group statistics | a 1-of-60-segment window 8.71 → 2.09 ms |
| Manifest-range segment skip, ahead of any file open | same query → **0.73 ms** (11.9x vs baseline) |
| Trace search binds raw `trace_id`; provider learns `IN` and the `OR` chain | 2-id fetch 7.26 → 2.23 ms |
| Promoted keys compile to a JSON-fallback `CASE` | **fixes a shipped wrong-answer bug** |
| `json_get` walks to the key instead of parsing the whole blob | 40-attribute record 326.7 → 101.5 ms |
| `Db::attr_access_stats()` | the demand half of choosing a `promote` list |
| `examples/attr-stats` | the data-shape half — per-key cardinality, coverage, sigma |

Six new measurement harnesses under `examples/bench/src/bin/`: `prune-bench`, `attr-bench`,
`jsonattr-bench`, `promote-cost`, `metricattr-bench`, `spanindex-bench`. They are the reason the
rejections below are decisions rather than opinions, and they are what a future change should re-run
before claiming an improvement.

**Semver.** `imbh-query`'s `SegmentInput` gains `time_range`, `TableInput` gains `time_column`, and
`SegmentTableProvider::new` takes another argument — public-field structs, so this is **breaking** and
needs the 0.7.0 minor bump the canonical-JSON change already puts on the table. It must not ship as a
patch. Everything else is additive: `Db::attr_access_stats` with `AttrAccess`/`AttrBackend`, and the
`CASE` change alters emitted SQL rather than any signature. No third-party dependency moved; crate
count is unchanged at 282.

### Measured and deliberately not built

- **Promoted-attribute push-down.** The `attrs` index buys nothing on a promoted column, because its
  entire value on the JSON path is avoiding the JSON parse — which promotion already removed. The
  promoted column sits within 0.13–0.55 ms of a bare `count(*)` floor at every selectivity, so there
  is nothing left to recover. The plan's own framing ("the un-promoted path is faster") was retracted.
- **A Tantivy index on metric segments.** The metrics gap is +26.0 ms over floor per 100k points and
  is *identical* at 50% and 1% selectivity. A promoted column recovers 95–97% at every selectivity;
  the index recovers it only when the matcher is selective, and PromQL selectors like `service="api"`
  are exactly the unselective case. It would cost a Tantivy build at seal on the highest-volume signal.
- **Dropping the spans `.tidx`.** Considered because it measured at 56% of the Parquet it indexes.
  Rejected: the cost gate declines only at cardinality 1–2, and from four distinct names upward the
  `RowSelection` prunes to exactly the matching rows.

Two of the three rejections share a shape: **the infrastructure that already existed covered the case
once the right keys, or the right predicate spelling, were used.** Neither would have been caught by
reasoning; both took a benchmark.

### Pushdown soundness audit

Not previously written down, and worth having in one place because five mechanisms now prune.
Verified by grep: **zero `TableProviderFilterPushDown::Exact` claims** in the tree. Everything is
`Inexact`, so DataFusion keeps a `FilterExec` above the scan and re-checks every predicate — a
pushdown can cost time, never an answer.

The asymmetry: over-inclusion is always safe, so only over-*exclusion* could be unsound. Each
mechanism excludes only on proof.

- **Blooms** prove absence, never presence. A segment is skipped only when *every* candidate is proven
  absent; `NOT IN` is not claimed, since it proves nothing about absence.
- **Row-group statistics** exclude only when min/max make a match impossible. Value-preserving casts
  only; `!=` never claimed; missing statistics mean read.
- **Manifest-range skip** survives out-of-order ingest, which is what killed the promotion-epoch
  design: overlapping ranges do not stop a segment's declared range from bounding *its own* rows. The
  `p.column == time_column` guard keeps a `duration_ns` or `observed_time` comparison from ever being
  tested against event-time bounds.
- **Tantivy `RowSelection`** uses exact hit sets, with the tokenizer shared between the index and the
  row-wise fallback so both agree byte-for-byte.

**One exclusion rests on an assumption rather than a proof**: that a `.tidx` agrees with its Parquet
file. Structural — built at seal from the same batch, rebuilt wholesale on compaction, with a
defensive guard for out-of-range ordinals — but a genuinely stale index *would* drop rows, which
`parameterized_matches_consults_the_logs_index` proves by building a divergent index on purpose. That
is the sharpest edge in the system.

### Method lessons

The findings will age; these probably will not.

1. **Separate structural wins from distributional ones.** Time-range and bloom pruning save in
   proportion to segments-outside-the-window over total — set by retention depth and seal cadence, not
   by what the values look like — so a synthetic corpus gives a number that carries. Sigma does not
   have that property, which is why `gen-demo-db` reports sigma 1.000 for all 19 keys and why that
   settles nothing. Knowing which kind of question you have decides whether you may fake the data.
2. **Counters can show pruning firing while the query is no faster.** At identical selectivity and
   identical `rows_scanned`, wall-clock moved 5x on distribution alone. Most of this session's tests
   are counter-based; that is a real limit on what they assert.
3. **Chase the anomaly.** The targeted-extractor bug — early return leaving `serde_json` mid-object,
   erroring, and silently falling back to the full parse — showed up only as "the first key is slower
   than the last", which is backwards. Writing it off as noise would have shipped a pessimization that
   never fails loudly.
4. **A benchmark can be confounded in the same way an experiment can.** The first version varied
   predicate *type* along with key position, and `= 'v0'` is the shape the `attrs` index recognizes.
   Two variables, one number.
5. **Measure before implementing when the mechanism already exists somewhere.** Both rejected features
   were sized without writing them, by exercising the existing path they would have reused.
6. **Confident prose in a design doc is not evidence.** Three §-level claims were false, two of them
   contradicted by other paragraphs in the same file. `ARCHITECTURE.md` now carries measured numbers
   where it used to carry assertions.

### Open, with the reasoning attached

`TODO.md` gained items for: the promote set being per-handle rather than durable DB state (a reader
and writer can disagree about the same DB); compaction baking an all-NULL promoted column and
destroying the "predates promotion" signal; the `CAST` being pure overhead for equality filters
(worth 0.20–0.31 ms, more than the `CASE` costs); `attr-stats`' `hint` column collapsing two
orthogonal axes; and the auto-promotion landmine itself, which none of today's work touches.
`AUTO_PROMOTION_PLAN.md` holds the tiered plan, whose remaining steps are gated on pointing
`attr-stats` at production data.

### Verified

`fmt --all --check` clean; `build --workspace`, `clippy --workspace --all-targets -D warnings`, and
`test --workspace` all clean at **643 passing** (from 627 at the session's first green gate). No
dependency movement, so no footprint gate movement is possible. Nothing committed.

## Cardinality is a curve: a window ladder, and a sketch that had to be replaced to make folding work (2026-08-08)

Follow-on to the three read-path entries above. The prompt was a design assertion — *"we need to
observe cardinality distribution over various buckets (short-term, mid-term, long-term) and keep them
as statistics"* — and the work is one feature in `examples/attr-stats` plus one correctness rewrite it
forced, both entirely offline. No shipped crate changed.

### The reframing: sigma is the two-endpoint version of the thing being asked for

A single global distinct-value count cannot separate the two shapes that behave completely
differently at query time. **Interleaved** (`env`, `service`) means every segment carries every value:
cardinality can be large and nothing prunes. **Localized** (`pod.name` across a rolling deploy, a
session id) means cardinality is large *because* values churn, and segment pruning removes almost
everything. Sigma already tells these apart — but at exactly one scale, the segment.

So the generalization is not three separate numbers, it is filling in the middle of a curve that
already had both endpoints. Define `C(w)` = mean distinct values of a key within one window of width
`w`. Then `C(segment)` is `postings/segments` (the number sigma summarises), `C(all)` is the global
distinct count, and the shape between them is the answer:

- `loc = C(all)/C(seg) ~ 1` — interleaved. Nothing prunes at any scale; a promoted column is the only
  lever.
- `loc >> 1` — localized. Pruning removes `1 - 1/loc`, and **the width at which the curve flattens is
  the horizon beyond which segment pruning stops paying** — which is exactly the predictive input the
  reactive cost gate in `imbh-query` lacks.

The report also grew `rep` (`rows / postings`, in-segment repetition), because the promoted-column
cost measurement earlier in the session found that to be the real driver of a promoted dictionary
column's bytes on disk. Cardinality and repetition now sit in one table instead of being conflated.

Shipped as `--windows 1m,1h,24h` (default; `none` to opt out, arbitrary length, strictly increasing),
report section 2, and a `cardinality_curve` array in the JSON view carrying both endpoints.

### Three implementation facts worth keeping

**It costs no extra passes.** Counting distinct *windows* per value reuses the same "last ordinal
seen" dedup the segment count already uses — one `u32` pair per level per tracked value.

**It required globally sorting segments by start time before scanning.** The dedup compares against
the currently-open window rather than a set, so out-of-order feeding reopens a closed window and
double-counts. Sorting once is also what lets the DB-wide unit — which interleaves tables — use the
ladder at all; each per-table unit then sees a sorted subsequence, which is still sorted. A segment
straddling a boundary is attributed to the window of its `min_time`, which only biases widths near the
segment span, where the level is degenerate anyway.

**Degenerate rungs are flagged, not printed as findings.** A width that opened one window has
collapsed onto `C(all)`; one that opened at least as many windows as there are segments has collapsed
onto `C(seg)`. On `gen-demo-db` (one segment per table over 15 minutes) *all three* rungs collapse and
say so. Silently printing them would invite reading a coincidence as a measurement.

### The gating question, and why the first answer was wrong

Persisting this cannot be per-segment: the wider rungs aggregate *across* segments by construction.
The shape that works is a mergeable per-`(segment, key)` sketch folded at read time, so retention
drops a segment's statistics with the segment and no bucket is stored twice. `SampledMap` looked like
it already was that sketch — a deterministic hash threshold that only ever tightens — so
union-then-shrink "should" equal scan-the-union.

That was recorded as unproven, then measured, and it was **half true**:

- Below the cap, a k-way fold equalled a single pass exactly.
- Above the cap the fold was *sound* — complete counters, valid sample, cap honoured — but **could not
  be exact, because there was no single direct-scan answer to be exact to.** The scan was itself
  order-dependent: `shrink` halved whenever the map was full *at the moment a key arrived*, so
  different arrival orders reached different rates, kept different keys, and reported different
  `estimated_total`s over identical input. An exhaustive permutation search over 4,800
  (key-family, n, cap) combinations found **60,012 permutation pairs disagreeing on the estimate**.

That last point was a defect in **already-shipped** behaviour, not something merging introduced:
`distinct`/`postings` were reproducible only while the caps were disengaged — i.e. exactly when they
print without the `~` marker. The module doc asserted "a hash sample is independent of arrival order",
which is false as stated: *membership given a rate* is order-independent, the *rate reached* is not,
and the rate is what reaches the report.

### The fix: bottom-k

`SampledMap` is now a bottom-k sketch — retain the `cap` entries with the smallest `(hash, key)`,
tie-broken by key text so even a 64-bit collision cannot make the outcome order-sensitive. Storage is
`HashMap<Rc<str>, V>` for lookup plus `BTreeSet<(u64, Rc<str>)>` for ordering, sharing key text through
the `Rc` so ordering costs a pointer rather than a second copy of every key. All three properties now
hold and are pinned by tests:

1. **Counters complete** — a key in the final bottom-k was in the bottom-k of every prefix containing
   it, so it was admitted on first sight and never evicted.
2. **Order-independent** — 78,300 permutations, zero disagreements, on the same sweep that previously
   found tens of thousands.
3. **Folding exact, above the cap as well as below** — a 3-part fold and a single pass agree on keys,
   counters, rate *and* estimate.

Verified end-to-end: two runs at `--max-values 3` return byte-identical estimates at a
non-power-of-two rate (0.27363988760093705), where the predecessor could return different numbers for
the same database.

**One deliberate deviation from textbook.** The standard bottom-k estimator divides `k - 1` rather
than `k` by the rate, removing a `k/(k-1)` upward bias. This uses `k`: every other scaled quantity in
the report is a sum over the sample divided by the same rate, and `locality = C(all)/C(seg)` is a
ratio between two of them — a correction on one side only would distort that ratio more than the bias
it fixes. 0.002% at the default 50,000-value cap, documented at the call site.

### Two bugs, and what actually caught them

**The first bottom-k conversion was silently wrong and all 18 tests passed.** `evict_max` did not set
`dropped`, so a map that only ever *evicted* (never refused an arrival) still reported
`sample_rate == 1.0` and `estimated_total == cap` — claiming exactness while discarding keys. The
signal was not a red test: it was the *order-dependence test continuing to pass* when the conversion
should have made it start failing. A green suite was the symptom.

**`rate_note` was quietly overstating coverage.** It printed `1/{round(1/rate)}`, correct while rates
were powers of two. Bottom-k thresholds are not — a measured 0.2736 rendered as `1/4` rather than
`1/3.7`.

Earlier in the same work, a merge that looked broken (merged rate consistently one halving tighter
than a direct scan) turned out not to be the sketch at all: `shrink()` cuts at `len >= cap` because
the scan path calls it *before* inserting one more key, and a merge inserts nothing afterwards.
Reusing it cut one rate further and silently halved the sample.

### Method notes

1. **A clean result from a weak search is not evidence.** The first divergence sweep found nothing and
   very nearly closed the question. The order dependence only surfaced under *exhaustive* permutations
   at small caps; the sequential key family and larger caps I had picked happened to miss it.
2. **When a change should break a test, check that it does.** Inverting the order-dependence
   assertion was what exposed the `dropped` bug. Tests that only ever go green cannot report a
   regression in the property they exist to describe.
3. **A design assertion in the prompt can be right and still need sharpening.** "Buckets" was correct;
   "a curve whose two endpoints already exist" is what made it implementable without new machinery.
4. **Doc claims decay in the same file that disproves them.** As with the three stale
   `ARCHITECTURE.md` claims earlier today, the `SampledMap` doc asserted a property the module's own
   behaviour contradicted. Both are now corrected in place with the measurement named.

### Open

`TODO.md` carries the consolidated entry: what the ladder measures, the merge verification, the
bottom-k conversion, and the two traps. Still open is the persistence itself — sketches written at
seal, plus manifest and retention plumbing. `SampledMap::merge` stays `#[cfg(test)]`: it establishes
the property, and nothing in the tool calls it yet. None of this should be sized before pointing
`attr-stats` at production data — on synthetic corpora every curve is flat by construction, exactly as
every sigma is 1.000. This is the distributional/structural split from the previous entry, unchanged:
the ladder makes one real dataset say much more; it does not remove the need for one.

### Verified

`fmt --all --check` clean; `build --workspace`, `clippy --workspace --all-targets -D warnings`, and
`test --workspace` all clean at **651 passing** (from 643 at the previous entry). Eight new tests, all
in `examples/attr-stats`. No dependency movement, so no footprint gate movement is possible. No
shipped crate touched, so no semver implication. Nothing committed.

## Addendum: the bottom-k sketch did not need two containers (2026-08-08)

Correction to the entry above, which describes a `SampledMap` built from a `HashMap<Rc<str>, V>`
beside a `BTreeSet<(u64, Rc<str>)>`. Prompted by two questions — *how sparse is the ordering map, and
does it need to be a map at all?* — and the answer to both was unflattering.

**The ordering set was never sparse.** It held an entry for *every* retained key, always, exactly 1:1
with the hash map. It was a full shadow index of the same population, maintained to answer one
question — what is the maximum — at a cost of 24 bytes plus a BTree node per entry. Nothing ever
called `range`, `contains`, or ordered iteration, so even as an ordering structure it was the wrong
shape; a heap would have served. Describing it as an "ordering map" made a duplicate index sound like
an index.

**It did not need to be a map, because the value map did not need the key text.** Grepping every read
settles it: every production use of `values.iter()` discards the key (`|(_, v)|` in `KeyAcc::postings`
and in `summarize_unit`), and `values_tracked` only counts. A value's text exists solely to tell
values apart — which the hash already does. Only the *key* map needs a name, because the report prints
attribute keys, and that name belongs in `KeyAcc` rather than in the sketch.

So `SampledMap` is now a single `BTreeMap<u64, V>` keyed by the hash: it is the lookup structure and
the order structure at once, with `pop_last` for eviction. Three things fell out:

- **~88 bytes of identity per value entry became 8.** The old per-entry cost was a 16-byte inline
  `Rc<str>`, a 24-byte tuple in the shadow set, a 16-byte `RcBox` header, and the value text itself;
  the new cost is one `u64` key. At the default 50,000-value cap that is roughly 4.4 MB down to 0.4 MB
  per tracked key, plus one fewer container and one fewer allocation per entry.
- **`MAX_VALUE_BYTES` and its digest folding are gone.** That special case existed only to bound the
  bytes one kilobyte-sized attribute value could occupy. Not storing value text at all subsumes it,
  and the collision bound it already accepted (~1e-10 at the 50k cap) now applies uniformly rather
  than only to values over 128 bytes.
- **Every lookup hashed twice and now hashes once.** `entry` used to call `contains_key` (SipHash over
  the string) *and* `hash` (xxh3 for the sampling predicate). It now hashes once with xxh3 and
  descends a BTree over `u64`s — on the hot path, one hash of every attribute pair of every row.

Tie-breaking by key text, which the previous entry lists as a property, is no longer needed and no
longer exists: keying by hash means a collision merges two names into one entry, which is
deterministic rather than order-sensitive. That is the same trade the digest folding already made.

### Method note

Restoring six tests I deleted by accident is the lesson worth recording. A scripted splice keyed on
"replace from this doc comment to that `#[test]`" swallowed everything between them, and the suite
went from 18 tests to 12 while still reporting **`test result: ok`**. A passing run says nothing about
tests that no longer exist; only the count did. Nothing was committed, so `git` could not restore
them — they had to be rewritten. Same shape as the `dropped` bug in the entry above: the failure was
visible in what the suite *stopped* asserting, not in anything it reported.

### Verified

`fmt --all --check` clean; `build --workspace`, `clippy --workspace --all-targets -D warnings`, and
`test --workspace` clean at **651 passing**, the same 18 in `examples/attr-stats`. Report output
re-checked against a generated demo DB, including a forced `--max-values 3` where estimates carry the
`~` marker. No dependency movement, no shipped crate touched. Nothing committed.

## Attribute archetypes: a corpus that falsified its own headline, and the cost term nobody was measuring (2026-08-08)

The operator has real telemetry but may not be able to use it (contract). So instead of measuring
their data we measured the *decision rule* against the space of shapes their data could have.

### The reframe

Every threshold in `attr-stats` traced back to one cost measurement plus reasoning. What had never
been checked was whether the **verdict matches the backend that is actually fastest and the column
that is actually cheapest**. Synthetic data cannot say what a deployment's telemetry looks like — on
a generator every sigma is 1.000 unless the generator is written otherwise, which is a fact about
the generator. But it *can* bound the space of shapes and test the rule across it. If the rule is
right for every archetype, the remaining unknown shrinks from "what does the data look like" to
"which archetypes are present" — answerable from a handful of parameters, disclosing nothing.

Two parameters came back from the operator and both mattered: request-scoped **and session-scoped**
identifiers do appear as attributes, and concurrent pods are **under 50** in most deployments.

`examples/bench --bin archetype-bench` builds seven archetypes as attributes on the *same* rows, so
the comparison between them is controlled — same segments, same row count, everything varying except
the shape of the value: a constant (`env`), a low-cardinality enum (`http.method`), a rolling
identity (`k8s.pod.name`, 50 live on an 8-segment lifetime), sessions in two layouts, a per-row
unique (`request.id`), and a Zipfian tenant id.

### The corpus falsified its own first result

The first run reported `session.id` at **9,079 B** — cheap despite 1,920 distinct values, apparently
a clean confirmation of the repetition-not-cardinality correction. It was an artefact: the generator
emitted each session's ~25 events *contiguously*, which real concurrent traffic never does. Flagging
that as provisional and fixing it was the single most valuable thing in this work.

Keeping both layouts as a **controlled pair** — same session population, same ~25 events each,
contiguous versus interleaved across 200 concurrent sessions — isolates the term exactly:

| key              | distinct | postings | rep  | disk       |
|------------------|----------|----------|------|------------|
| `session.contig` | 1,920    | 1,920    | 25.0 | **9,079 B**  |
| `session.id`     | 2,000    | 3,791    | 12.7 | **64,252 B** |

**7.1x apart**, of which only ~2x is explained by postings. Interleaving made session ids more
expensive than pod names (64 KB vs 42 KB) — and the gate shipped that morning gave both the identical
verdict, because it could not see the difference at all.

### The cost model was missing its larger term

A promoted column is a Parquet dictionary **plus a per-row `Int32` index array**. The dictionary
scales with distinct-values-per-segment; the index array's compressed size scales with the *entropy
of the value sequence*, which repetition cannot see. `postings/rows` modelled only the first. Ranked
by it, `k8s.pod.name` (42,135 B) was rated cheaper than `session.contig` (9,079 B) — an inversion.

The gate is now

```
est B/row = [ C(seg) * mean_len  +  runs * log2(C(seg)) / 8 ] / rows_per_segment
```

Ranked by this, all seven archetypes come out in **exactly** their measured disk order, across a 26x
range (4,092 B to 105,223 B). Absolute values run 2-5x high because zstd exploits structure the model
does not — `request.id`'s strictly-increasing index compresses far better than its entropy implies —
so it ranks keys rather than sizing budgets, which is what a verdict needs.

Two implementation consequences:

- **`runs` is now measured**, which required `scan.rs` to walk rows in order. The dictionary path
  previously tallied a count per dictionary entry and discarded order entirely; it now coalesces runs
  of equal indices, and `prev` carries across record batches so a run is not falsely restarted at
  every batch boundary. A run starts when a *key's* value differs from the previous row's — not when
  the blob differs, since two blobs can agree on any given key.
- **The estimate is per row, not per segment.** The first threshold was absolute bytes and the
  existing `the_promotion_verdict_follows_repetition_not_cardinality` test rejected it instantly: a
  200-row segment reads as cheap whatever it holds, so `request.id` came out `yes`. Normalising by
  rows per segment makes it scale-free.

### Two harness bugs, both the same species

**The agreement check was comparing the wrong things.** It validated the promote verdict against
*which backend was fastest*. The verdict predicts **disk cost**; at low selectivity the `attrs` index
already takes the JSON path to the floor (5.10 ms against a 5.62 ms floor at 1.18%), so promotion
cannot win however cheap its column is. The `k8s.pod.name` "MISMATCH" was the harness's category
error, not the classifier's. It now ranks verdicts against measured bytes.

**The bench carried its own copy of the classifier.** After the real gate was fixed, the bench kept
reporting the old inversion — because it was still evaluating the superseded rule. A harness that
validates a *duplicate* of the rule validates nothing. It now mirrors the shipped model deliberately,
with a comment saying why.

### What else the corpus showed

- **`attr-stats` recovers ground truth exactly** — distinct (48,000 / 1,000 / 1,920 / 118 / 7 / 1)
  and postings (48,000 / 8,313 / 1,920 / 600 / 84 / 12) both match the generator's own counts, none
  sampled. First time the estimator has been checked against known truth rather than self-consistency.
- **Promotion's speed win is entirely at unselective filters.** `env` at 100%: 19.80 -> 6.93 ms.
  `http.method` at 14%: 10.52 -> 6.92 ms. Everything at or below 1.7%: no win at all, the index is
  already at the floor. That matches the cost gate declining above a ~50% hit fraction, measured
  earlier. So the rule is **promote what is queried unselectively; let the index serve selective
  equality** — which needs the demand signal (`Db::attr_access_stats`) crossed with cost. Cost alone
  cannot express it, and the tiered plan's dispatch table did not say this.
- **`request.id` is where the two verdicts must diverge**, and they do: `costly` to promote (10.26
  est B/row, 105 KB measured) yet `index@ all` (sigma 0.083). A single-label classifier could not
  have said this, which is what the column split earlier in the day was for.

### Method notes

1. **A synthetic corpus can be wrong in a way that flatters the hypothesis.** Contiguous sessions
   were not a simplification, they were the answer the model wanted. The tell was that the result
   was *too* clean — 1,920 distinct values for 9 KB.
2. **Keep the falsified variant as a control.** Deleting `session.contig` once interleaving was
   implemented would have lost the isolation; keeping both is what turned "the caveat was right" into
   a measured 7.1x with everything else held constant.
3. **A harness must evaluate the shipped rule, not a copy of it.** Two of this session's bugs were
   tests or harnesses that agreed with themselves.
4. **State the caveat before running the experiment that tests it.** The interleaving flaw was called
   out as provisional in the same breath as the result; that is what made the follow-up obvious
   rather than embarrassing.

### Open

Production data remains the gate, but a smaller one: the question is no longer "what does the data
look like" but "which of these archetypes are present, and in what proportion". The run-structure
term is also absent from any seal-time/manifest statistics design — the per-segment sketch discussed
earlier stores value presence, not value *order*, so a persisted version of this statistic needs a
run counter alongside it.

### Verified

`fmt --all --check` clean; `build --workspace`, `clippy --workspace --all-targets -D warnings`, and
`test --workspace` clean at **653 passing**. No dependency movement. `attr-stats` changes are
example-crate only; no shipped crate touched in this entry's work. Nothing committed.

## Unblocking out-of-process housekeeping: two silent-answer fixes, a mutable promote set, and a design that splits prepare from commit (2026-08-09)

Covers the read-only `segment_files` fix and the durable promote set (both 2026-08-08, not journalled
at the time) plus today's compaction work. One thread: everything auto-promotion and out-of-process
housekeeping need before they are safe to build.

### `Db::segment_files` returned an empty database on read-only handles

`Storage::open_read_only` deliberately leaves the in-RAM segment lists empty — a reader derives its
view per call — but `segment_files` read those lists, so it returned `[]` for a fully populated
database. Nothing errored: `[]` is indistinguishable from "this table has no segments", so a host
handing the paths to DuckDB `read_parquet` (the documented use, §10.11) got no data and no error.

It now reads a fresh `read_disk_snapshot`, the same source the reader's query path already used, and
is **fallible** — `Result<Vec<PathBuf>>`, breaking, taken in the open 0.7.0 window. The signature is
the point: on a reader the call genuinely performs I/O, and the old type could not distinguish "no
segments" from "could not read the manifest". `BlockingDb` gained the mirror it had never had.

**Scope correction worth recording.** I first reported this as a *family* of accessors — `stats()`,
`snapshot()`, the retention scans — inferred from `inner.segments` being read by `table_stats`. That
was wrong, and the user had already approved the wider scope on the strength of it. Reading the
*callers* rather than the field: `Db::stats` already branches to `reader_stats()` (which uses
`read_disk_snapshot`), and `Db::snapshot` already calls `ensure_writable()?`, so it refuses
explicitly. `segment_files` was the only silent one. Fixing one accessor instead of three was the
correction, not a de-scoping.

### The promoted key set became durable database state

It was per-handle configuration, so a writer opened with `promote(["k"])` and a reader opened without
it disagreed about the column layout of the very same segments. The `CASE` fallback shipped earlier
made that *correct* but never *coherent*, and auto-promotion — where the writer's set changes at run
time and a reader could never learn it — needs coherence.

Now recorded in `db.info` (one escaped `promote\t<key>` line per key, temp→rename) and read at open.
The load-bearing design decision is distinguishing **"the builder said nothing"** from **"the builder
said empty"**: omitting `promote()` inherits the database's set, an explicitly empty `Promote` still
demotes everything. Without that distinction every host that stopped passing its list would have
silently demoted its database, so `DbBuilder`'s field became `Option<Promote>` internally while the
public signature stayed put. Read-only handles adopt the durable set and ignore their own builder.

**The trap:** `Storage::open` calls `write_db_info`, which rewrote the file with `Promote::default()`
— so *opening* a promoted database erased the marker before the facade could read it, and a reader
then saw no promoted columns at all. Open must carry the existing set through. Caught by the
durability test; reasoning had not.

### `set_promote`, and why the barrier works

Batches are encoded at ingest against the set in effect then, and `concat_buffer` later concatenates
them against the *current* schema. `concat_batches` takes columns **positionally** and does not
validate them against the schema it is handed, so a buffer holding both widths can panic (first batch
wider), silently truncate (first batch narrower), or silently concatenate two differently-named
promoted columns into one. That is the hazard §6.1 records for compaction, and making the set mutable
would have reproduced it in the buffer.

`Storage::set_promote` seals, takes the `inner` lock, verifies every buffer is empty, and swaps under
that lock. Correctness rests on a fact checked rather than assumed: **ingest reads the promote set
beneath the same lock it appends under** (`push_log_batch(&mut inner, rows, &self.promote_keys())`),
so once the swap holds `inner` with empty buffers no encode can be in flight against the old set. A
racing ingest between seal and lock costs another round; bounded at 8 attempts rather than spinning
forever inside a public call. `promote` moved behind an `RwLock`; `Storage::promote()` returns an
owned `Promote`, since handing out a reference would pin the lock.

### The housekeeper question, and two premises that did not hold

The request was "make auto compaction a fully optional feature, and let the separate housekeeper
process take care of it". Both halves rested on premises worth checking first:

1. **There is no auto compaction.** `Db::maintain()` is seal + retention only; the `Maintenance`
   scheduler never compacts; there are zero interval/loop/scheduled references to `compact` anywhere
   in the workspace. It runs only on an explicit `Db::compact()`. Compaction is already *stronger*
   than optional — it is entirely manual. Nothing to make optional.
2. **A separate process cannot compact a live database.** `Storage::open` in `ReadWrite` takes an
   exclusive advisory `flock` on `writer.lock` for the handle's lifetime (§5). A second read-write
   open fails outright.

So the recommendation to drive `POST /admin/compact` from an external scheduler was correct for
`imbhd` and useless for the actual case: **an embedded host has no `imbhd` to drive**, and cannot be
asked to stop its writer to compact.

### Why per-segment locks are the wrong instrument

The proposal to shard the writer lock per segment targets a resource that is not contended. Segments
are not; the **manifest** is. Two mutators holding disjoint per-segment locks still serialise their
manifest edits, so the machinery buys nothing the manifest lock would not.

The more useful observation: **the housekeeper never needs an LSN.** Compaction and retention do not
ingest — no WAL append, no LSN allocation, no mutable buffer. So the requirement is not N concurrent
writers but *one ingesting writer plus N non-ingesting mutators*, which is a far smaller ask.
`writer.lock` conflates "owns the WAL and the LSN space" with "may mutate segments", and only the
first genuinely requires exclusivity.

Three properties make a split cheaper than it looks, all read out of the source:

- the manifest is already a versioned replayable delta log (`CURRENT` → `MANIFEST-<N>`, framed
  `len`/`xxh3`/payload records, torn-frame tolerance, readers re-resolving across a roll);
- `ManifestWriter::persist` emits `diff(self.last, view)`, so a seal racing a compaction appends only
  "add X" and replays against `{C}` to give `{C, X}` — **deltas compose**;
- readers already pin segments by open handle, so concurrent deletion is safe (verified by the
  existing `..._pinned_by_open_readers` test).

And two that break:

- **checkpoints clobber.** `write_checkpoint` (the periodic roll) writes a full reset from the
  writer's in-RAM `self.last` and flips `CURRENT`; a roll racing a compaction drops the merged
  segment entirely;
- **the writer's in-RAM segment view is authoritative for its own queries.** After a housekeeper
  merges A,B → C and unlinks them, the writer still opens A and B *by path* — `ENOENT`. Fixing that
  reaches into query-snapshot consistency, `stats()` and `segment_files()`.

Also `append_frame` uses `write_all`, which loops on a short write; `O_APPEND` makes a *single*
`write` atomic against other appenders, not a looping one. Not a live bug, but a multi-writer design
would have to make it a guarantee rather than an observation.

### The design: split by cost profile, not by resource

Compaction is ~99% expensive IO and ~1% atomic bookkeeping, and the halves have completely different
safety requirements. **The housekeeper prepares** — opens read-only (no lock; readers already work
against a live writer), merges, writes the output plus its `.tidx` under a scratch name, and records
its inputs, output and the promote set it built against. **The writer commits** — at its next seal or
`maintain()`, validates and performs the swap itself: one manifest delta, then unlink the inputs.

That removes the manifest commit protocol, the checkpoint clobbering, and the stale-view problem
outright, because the manifest stays single-mutator and the writer is the one changing it. Two things
fall out free: the **offline** case (no writer running → the housekeeper takes `writer.lock` and
commits its own record, same code path) and the **Cargo feature** (the merge half ships only in the
housekeeper binary; the writer links only validate-and-swap, which is what "compaction as a fully
optional feature" should mean).

Written up as `.agents/docs/COMPACTION_HANDOFF.md`, with the costs stated — commit latency, a new
on-disk record to version, wasted work on conflict — and four open questions left open.

### The promote/compaction fix, and a claim I got wrong

**It was never a wrong-answer bug.** The TODO entry always said the `CASE` fallback is immune: a
null-filled column takes the JSON arm exactly as an absent one would. A backlog summary of mine on
2026-08-08 called it the last remaining wrong-answer bug, which the entry itself contradicted.

What it was is a **convergence** defect. `compact_partition` normalised each source batch to the live
promote set with `coerce_to_schema`, which null-fills a column the segment predates — and compaction
is the one operation that rewrites those rows, so null-filling there made the fallback permanent and
every query on that key kept paying a JSON parse over that data for the life of the merged segment.

`backfill_promoted` now projects the column from the retained `attributes` JSON through the same
`build_promoted_columns` / `lookup_promoted` path seal uses, so a back-filled cell and a
sealed-at-ingest cell cannot disagree. Only columns the *source* lacked are derived; a NULL in a
column the source had means the row genuinely carried no string value.

Two things the verification turned up:

- **An existing storage test asserted the old `[None, None, Some("us")]`.** The null-fill was
  deliberate, not accidental, so updating it was part of the change rather than collateral damage.
- **The test written to catch a misalignment does not catch it.** `backfill_promoted` zips against
  `promoted_columns(missing)` rather than `missing` because the former drops reserved names and could
  offset the vectors. Reintroducing the bad zip left the test green. Chasing why: `missing` holds only
  keys *absent* from the source schema, and the reserved names are the built-in columns, present in
  every segment — so the two sets cannot intersect and the bug is unreachable. The guard stays for the
  day the built-in set changes; the test's doc comment now claims only what it proves.

### Method notes

1. **Check whether the callers branch before concluding a field's readers are all affected.** The
   `segment_files` "family" was inferred from `inner.segments` having many readers; two of the three
   already handled the reader case. Reading the field's uses is not reading the code.
2. **Correct the premise before building.** Two requests today rested on premises that did not hold
   (auto compaction exists; a second process can compact a live DB). Building either would have
   produced plausible, useless work.
3. **"Verify the test fails without the fix" paid three times and failed once.** It caught nothing
   wrong with the `segment_files`, barrier and convergence tests — and caught that the misalignment
   test was decorative. That last one is the reason to keep doing it.
4. **Split by cost profile when a resource is contended for two different reasons.** Prepare/commit
   works because compaction's expensive half needs no exclusivity and its cheap half needs total
   exclusivity, and the existing lock granted both to whoever held it.

### Verified

`fmt --all --check` clean; `build --workspace`, `clippy --workspace --all-targets -D warnings`, and
`test --workspace` clean at **658 passing** (from 653 at the previous entry). No dependency movement.
Breaking changes this entry: `Db::segment_files` → `Result`, `Storage::promote()` → owned, the
durable promote set semantics — all recorded in `CHANGELOG.md` under `[Unreleased]`, all inside the
un-cut 0.7.0 window. Nothing committed.

## Addendum: merge and converge are one job, not two (2026-08-09)

Follow-on to the entry above, which recorded the promoted-column projection landing inside
compaction. Generalising the housekeeping design then went through a wrong turn worth recording.

### The wrong turn

Asked to treat the promotion backfill as a housekeeping job rather than a compaction detail, I first
wrote it up as a **second job** alongside compaction: `compact` merges N same-day segments into 1,
`backfill` rewrites 1 segment whose schema lags the promote set. That framing survived exactly one
round of scrutiny.

They are the same job with two triggers. The work is identical either way — read the set, project any
promoted columns the sources lack from their retained `attributes` JSON, concat, sort, write one
Parquet plus its `.tidx`. Merging is that with `|set| > 1`; converging is that with `|set| == 1`.

Splitting them is not merely redundant, it is **incorrect under the handoff design**:

1. Compaction already projects promoted columns while it rewrites (`backfill_promoted`), so a
   separate backfill job rewrites the same bytes a second time for no gain.
2. A backfill record for `A` and a compaction record for `A,B` both **claim `A` as an input**.
   Whichever commits first invalidates the other, so the loser's rewrite is discarded — and a
   scheduler that keeps re-issuing both never converges, it just burns IO.

The second point only becomes visible once the pending-record commit protocol exists on paper. It is
a good argument for writing the design note before the code: the conflict is obvious in the record
model and invisible in the function signatures.

### What the generalisation actually exposed

The gap was real even though the two-job framing was not. `compact_partition` skipped any day
partition holding a single segment — nothing to merge — so a partition that will never gain a second
segment (an old day, a low-volume signal, a database that seals rarely) **never converged after a
`set_promote`, however often compaction ran**. The projection that landed earlier the same day only
ever reached segments compaction was already rewriting for other reasons.

Fixed by changing the admission rule rather than adding a job: a one-segment partition is skipped only
when its schema *already matches*, probed from the Parquet footer (`segment_schema_lags` — no row
groups read, one small seek per segment). `CompactionReport` gained `segments_converged`, so a 1 -> 1
rewrite is not counted as a merge and `segments_merged` stays literally true. Breaking, inside the
open 0.7.0 window.

### The consequence, stated rather than hidden

Because convergence is triggered by *schema lag*, the first `compact()` after a `set_promote` rewrites
**every segment in the database that lacks the new column**. One rewrite per segment, not a repeated
cost — the second pass finds nothing, which the new test asserts explicitly — but a burst driven by a
schema change rather than by data volume.

In-process that burst lands on the host's own thread, which is an argument *for* the out-of-process
handoff rather than against the behaviour. Out of process the housekeeper's planner should pace it
(cap per pass, oldest first). Recorded as an open question whether in-process `compact()` needs a cap
of its own; today it does not have one.

### Method note

**A generalisation that adds a job should be checked against the conflict model, not just the call
graph.** "Backfill is a separate job" reads fine as a list of capabilities and falls apart the moment
you ask which pending records could name the same input segment. The unit to reason about was the
*claim on a segment*, not the *operation being performed*.

### Verified

`fmt --all --check` clean; `clippy --workspace --all-targets -D warnings` clean; `test --workspace` at
**659 passing** (from 658). One new lifecycle test covering a lone lagging segment and the idempotence
of a second pass. Nothing committed.

## The housekeeper lands: prepare/commit implemented, and a bug only a second real run could find (2026-08-09)

Implements the design from the two entries above. `.agents/docs/COMPACTION_HANDOFF.md` is updated to
"implemented, first cut" with its remaining gaps named.

### What shipped

- **`crates/imbh-storage/src/pending.rs`** — the handoff record. Framed like a manifest edit
  (`| len(4) | xxh3(8) | payload |`) so a write torn by a crash fails its checksum and is discarded
  rather than half-applied. The payload is tab-separated text for debuggability, with **total**
  escaping: attribute keys are arbitrary UTF-8 and a tab in a promoted key must not split a field.
- **`imbh_storage::prepare_pending`** — the read-only half. Reads the manifest and the durable
  promoted key set off disk, groups each table by UTC day, and rewrites a group when either trigger
  fires (more than one segment, or a lone segment whose schema lags). Takes no lock, edits no
  manifest, deletes nothing. `max_jobs` bounds the pass.
- **`Storage::commit_pending` / `Db::commit_pending`** — the writer's half: validate, one manifest
  delta, unlink the inputs. `Db::maintain` calls it, and `MaintenanceReport` grew
  `pending_applied` / `pending_discarded`.
- **`imbh-housekeeper`** — a bin on the `imbh` crate behind the off-by-default `housekeeper` feature
  with `required-features`, so a library consumer never builds it. `--commit` is an offline mode that
  takes the writer lock itself; it falls out of the same code path rather than being a second
  implementation.

To make the rewrite callable without `&self`, `write_segment_parquet` and the schema/params lookups
were extracted as free functions (`write_segment_parquet_at`, `table_schema_for`, `rewrite_params`,
`rewrite_segment_set`). `Storage::compact` and the out-of-process preparer now share one rewrite, which
is the point — a divergent copy would eventually disagree about column layout or sort order.

### The subtle correctness case, reasoned out during implementation

Commit's ordering follows `seal`: the manifest is durable **before** the inputs are unlinked, so a
crash in between leaves unreferenced files for `cleanup_orphans` rather than a manifest pointing at a
deleted file. But that leaves a second window — between "manifest durable" and "record deleted" — and
on the next pass the record looks *stale*, because its inputs are legitimately gone.

Discarding a stale record deletes its output. Here that output is by then a **live committed
segment**, so the naive discard would destroy committed data. `commit_pending` therefore removes the
output only when the manifest does not reference it. Not found by a test; found by asking what the
crash window looks like from the next pass's point of view.

### The bug a second real run found

Running the binary against a real database, twice, produced `0 applied, 2 discarded` — everything
rejected. The cause was not the validation logic, which was correct throughout:

**A prepared output is a segment no manifest points at — exactly the shape `cleanup_orphans` reaps at
open.** So opening the writer to commit destroyed the very outputs it was about to commit. The
offline `--commit` mode, which opens the writer *after* preparing, swept away its own work on every
invocation; and any host restart between prepare and commit would have thrown away an
out-of-process preparer's rewrite.

The reaping was **safe** — the commit's digest check rejects a record whose output has vanished, and
the inputs were never touched — which is exactly why nothing caught it. Every unit test passed. The
protocol was correct and useless at the same time.

`cleanup_orphans` now treats a file named by a *valid* pending record as referenced. Once the record
is consumed the file is either a committed segment or an orphan again, so nothing leaks.

### The feature gate, and what it does not do

`housekeeper` keeps the **binary** out of a default build; verified both directions with a cleaned
artifact directory, because a stale binary from the earlier example crate initially made the gate look
broken when it was not.

It does **not** drop the merge machinery from a library build, because `Db::compact` shares it.
Separating those needs compaction itself behind a feature — the deeper split the design note
describes. That limit is written into the feature's own comment rather than left for a reader to
discover: a gate that implies more than it delivers is worse than no gate.

### Method notes

1. **Safe-but-useless is a distinct failure mode from unsafe, and unit tests do not find it.** Every
   validation path was right; the protocol still could not make progress. The only signal was running
   the real binary against a real database and watching the counters.
2. **Run it twice.** The first invocation of anything idempotent-by-design proves nothing. The second
   is where interaction with `open()`, duplicate records, and "already committed" show up — the
   duplicate-record discard in the same run was correct behaviour, and it was the *first* record's
   discard that pointed at the bug.
3. **Ask what a crash window looks like from the next pass's point of view**, not just from the
   crashing process's. The stale-record-with-live-output case is invisible from inside the commit and
   obvious from outside it.
4. **Extract shared machinery rather than copying it across the process boundary.** The in-process
   compactor and the out-of-process preparer must produce byte-identical segments or the promote-set
   validation is theatre; one `rewrite_segment_set` is what makes that structural.

### Verified

`fmt --all --check` clean; `clippy --workspace --all-targets -D warnings` clean, and again with
`-p imbh --features housekeeper`; `test --workspace` at **667 passing** (from 659). End-to-end run on a
real corpus: 3 segments merged to 1 with all 1,500 rows preserved, the duplicate record correctly
discarded, `pending/` left empty. No dependency movement. Nothing committed.

## Retention is not a handoff job — and what was actually missing (2026-08-09)

Asked to make retention a handoff job alongside the segment rewrites. Checking the premise first
changed the deliverable, for the third time today.

### The claim was mine, and it was wrong

`.agents/docs/COMPACTION_HANDOFF.md` said retention "has the same shape (expensive scan, cheap
manifest edit)" and could reuse the handoff. Reading `Storage::retain` rather than trusting the note:
the scan is segment **metadata already in RAM** (`SegmentRef` min/max time and paths) plus one
`stat()` per segment for the disk-budget arm. Everything else is arithmetic.

There is no expensive half to move off-process. A housekeeper would compute a drop list in
microseconds and hand the writer a record the writer could have produced faster than it can read one.

### It would also have added a conflict the design does not have

A "drop A" record and a "merge A,B -> C" record both claim `A`. `pending::list` returns records in
filename order, which is a hash of the output path — arbitrary. Today the ordering is deliberate:
`maintain()` runs `commit_pending()` **before** `retain()`, so a prepared rewrite lands before
retention can invalidate it. Turning retention into a record would dissolve that guarantee into a coin
flip, and buy nothing for it.

This is the same reasoning that collapsed "backfill" and "compact" into one job yesterday, arriving
from the other direction: there the two records would have claimed the same input, so they had to
merge; here the two records would claim the same input, so the second one must not exist.

### What was actually missing

`Retention` was **per-handle builder config, never persisted** — precisely the defect `Promote` had
until the day before. Two handles on one directory could disagree about when data is deleted, and a
housekeeper had no way to learn the host's policy at all: it opened with `Retention::none()` and did
nothing, and taking a policy from its own flags would have let it delete data on rules the host never
chose.

So retention is now durable state in `db.info` beside the promoted set, with the same unset-versus-
explicit distinction (`Option<Retention>` internally, public builder signature unchanged): omitting
`retention()` inherits, passing one sets it. `Retention::from_parts` reconstructs a policy from its
two bounds, and the type gained `PartialEq`/`Eq` so a change can be detected at open.

The housekeeper's offline `--commit` mode now runs `maintain()` rather than `commit_pending()`, so it
applies **the database's own** policy — seal, commit the pending rewrites, then retention. Retention
out-of-process for free, with no record type and no new conflict.

### A small honesty bug of my own making

The first cut of that change shoehorned a `MaintenanceReport` into a `PendingReport` to reuse the
printing code, which silently zeroed the replaced-segment count: the binary reported
`0 input segment(s) replaced` while replacing three. Fixed by carrying the maintenance report through
and adding `pending_segments_replaced` to it. Worth recording because the output *looked* plausible —
a zero is exactly what a no-op run prints, so nothing about it invited a second look.

### Method notes

1. **A claim in a design note is a latent instruction.** "Retention has the same shape" sat in a doc
   for one day and came back as a work request. Design notes get acted on, so an unverified assertion
   in one is more dangerous than the same assertion in a chat message.
2. **Check what a thing costs before deciding where it should run.** The whole prepare/commit split
   exists because compaction is 99% IO; applying it to something that is 99% arithmetic inverts the
   trade — machinery and a conflict surface bought with no saving.
3. **When the answer is "don't build that", find what the request was actually reaching for.** The
   underlying need — a housekeeper that can apply retention — was real; only the instrument was wrong,
   and the fix was a defect that had nothing to do with handoffs.

### Verified

`fmt --all --check` clean; `clippy --workspace --all-targets -D warnings` clean, and again with
`-p imbh --features housekeeper`; `test --workspace` at **668 passing** (from 667). End-to-end offline
run reports `1 applied, 1 discarded, 3 input segment(s) replaced`. `db.info` carries a retention line
only when a policy is set, so a database without one is byte-identical to before. Nothing committed.

## The housekeeper reaches its actual user, and a footprint lever that measures ~0 (2026-08-09)

Two follow-ups from the housekeeper landing, both from its own TODO list.

### The shipped feature did not serve the user it was built for

`imbh-housekeeper` exists for **embedded** hosts. The default `Maintenance::Manual` means such a host
may never call `maintain()` — and `maintain()` was the only thing that committed pending records. So
prepared rewrites sat on disk indefinitely, and worse, `prepare_pending` re-prepared the same
partitions on every pass, because nothing it produced ever landed. An `--every 60` housekeeper against
a host that never maintains is an IO treadmill.

Records are now committed at **`open()`** and **`close()`**. Two ordering constraints, both
load-bearing:

- **After `with_promote`.** `commit_pending` validates each record against the *live* promoted set, so
  committing before the set is applied would reject every record as stale — the failure would have
  looked exactly like "the housekeeper produced nothing usable".
- **After `cleanup_orphans`.** Both run inside open, cleanup first. That turns the restart test from
  an assertion into a *proof*: a merged result can only happen if cleanup left the prepared output
  alone, because a swept output fails the commit's digest check. The test asserts one thing and
  establishes two.

Failure at close is swallowed deliberately — the records stay for the next open, which is exactly the
state a crash would leave. Cost is one `read_dir` of a usually-empty directory, on paths already doing
recovery work.

An existing test broke and deserved to: `a_prepared_rewrite_survives_a_writer_restart` asserted
`commit_pending()` returned `applied: 1`, and it now returns 0 because `open()` already did the work.
The intent held; the expectation was obsolete.

### The `compaction` feature, and a claim of mine that measurement demolished

`compaction` (on by default, like the other footprint levers) now gates `Storage::compact`,
`compact_partition`, `rewrite_segment_set`, `prepare_pending` and helpers. `commit_pending` stays in
every build — applying a record a housekeeper prepared is cheap bookkeeping an embedded host wants.
`housekeeper` implies `compaction`.

`.agents/docs/COMPACTION_HANDOFF.md` justified this by saying the host would stop carrying "Parquet
write, Tantivy build, sort, JSON projection". **Two of those four are wrong**: `seal` already writes
Parquet and already builds the Tantivy sidecar, so both subtrees stay in *any* writer build. Measured:

| configuration | crates (`--edges normal`) | `libimbh.rlib`, release |
|---|---|---|
| default | 381 | 4,413,604 B |
| `--no-default-features --features ingest,query,search` | **381** | 4,303,170 B |

**No dependency leaves the graph.** 110,434 B of code goes — 2.5% of the rlib.

The gate still earns its place, for a reason that is not bytes: a host that does not link the rewrite
**cannot start an unbounded one on its own thread**, by accident or by a well-meaning `compact()`
call. That is an API-surface guarantee, and it is now what the note, the feature comment and the
CHANGELOG all claim — no more. Note this is the second time in two days that the promotion burst
argument ("the first compact after a `set_promote` rewrites every lagging segment") turned out to be
the strongest reason for a design decision, ahead of the one originally given.

### Method notes

1. **A feature that is correct and does not serve its user is still broken.** Second instance in two
   days of the same species — the orphan-cleanup interaction was the first. Both passed every test;
   both were found by asking "what does the intended user's actual sequence of calls look like?"
   rather than by exercising the API.
2. **Measure a footprint lever before claiming one.** The dependency-subtree win I wrote into a design
   note did not exist, because the removed code shares its dependencies with a path the host needs
   anyway. A feature gate's *justification* is as checkable as its behaviour, and I did not check it
   until the gate existed.
3. **Order operations so a test proves rather than asserts.** Commit-after-cleanup means one
   assertion establishes both that cleanup respected the record and that the commit applied it. Free,
   and only available if you notice the ordering while writing the code.

### Verified

`fmt --all --check` clean; `clippy --workspace --all-targets -D warnings` clean, and again for
`-p imbh --features housekeeper` and `-p imbh --no-default-features --features ingest,query,search`;
`test --workspace` at **669 passing** (from 668). Both feature configurations build and are linted.
Nothing committed.

## Consolidation: two design notes folded into ARCHITECTURE and TODO (2026-08-09)

`.agents/docs/AUTO_PROMOTION_PLAN.md` and `.agents/docs/COMPACTION_HANDOFF.md` are **retired**.
Earlier entries reference both by path; this entry is the forwarding address.

### `COMPACTION_HANDOFF.md` -> **ARCHITECTURE.md §7.2**

It described a design; that design is now an implemented subsystem, so it belongs with the rest of the
storage engine rather than in a standalone note. §7.2 "Out-of-process housekeeping: a prepare/commit
handoff" sits directly after §7.1 (single-writer, many-reader), which is the invariant it works
around, and carries: why relaxing single-writer is the wrong instrument and per-segment locks wronger
still; the prepare/commit split by cost profile; the record format and framing; the validation rules
and why rejection is always safe; the two subtleties (discard must not delete a manifest-referenced
output; `cleanup_orphans` respects valid records); pickup at `maintain`/`open`/`close`; one job with
two triggers; why retention is deliberately *not* a handoff job; and the measured feature-gate
numbers.

Its three genuinely open questions went to TODO — record location (own file vs manifest frame type),
whether a `maintain.lock` lease is worth it, and `append_frame`'s looping `write_all` versus
`O_APPEND` atomicity.

**All code and CHANGELOG references were retargeted** to ARCHITECTURE.md §7.2 (7 files). JOURNAL
references could not be, being append-only — hence this entry.

### `AUTO_PROMOTION_PLAN.md` -> **TODO** (what survives), and mostly nothing

Written 2026-08-08, and the intervening two days demolished most of it. Recording *what* was
superseded, because the plan reads authoritatively and a future reader finding it in git history
should know it is wrong:

- **§2.1(a) rejected `coalesce`** on the grounds that "DataFusion evaluates `coalesce` arms as whole
  arrays, not lazily per row". **Refuted** by reading `datafusion-functions`: `short_circuits()` is
  `true` and it is rewritten into a genuinely conditional `CASE`. The `CASE` form shipped.
- **§2's correctness constraint** — promotion silently changing historical answers — **fixed** by that
  same read-side fallback.
- **§5's "unmeasured, and a prerequisite"** per-key cost — **measured** (`promote-cost`), and the
  answer moved twice: not cardinality, then not repetition alone but run structure.
- **§6 staging step 1's promotion epoch** — **rejected**: re-promotion makes a key's validity a set of
  intervals, which an epoch cannot express.
- **§6 step 4 compaction back-fill** — **shipped**.
- **§8's "`attr-stats`' single `hint` column is wrong as it stands"** — **fixed**, split into
  independent `promote` and `index@` verdicts.

What survives is policy for a feature nobody has built, now one TODO item: slow to promote and willing
to demote (demotion is correctness-free, promotion is the direction that needed the fallback);
hysteresis, because promotion is a schema change and flapping multiplies segment schemas; manual
`promote` staying authoritative with automation allowed only to add; the kill criteria; and the
out-of-scope boundaries that other code depends on — record-scope `AnyValue::Str` only, and promotion
is a *projection*, never a relocation, so the key must stay in the JSON.

### Method note

**A plan document ages faster than anything else in a repo, and it does not announce that it has.**
This one contained a confidently-stated falsehood (the `coalesce` rejection) that I had already
disproved and corrected in JOURNAL, while the plan sat unchanged asserting the opposite. Design notes
get acted on — that was the lesson from the retention entry two entries ago, and this is the same
lesson from the other end: an *implemented* design belongs in the canonical doc, and an *overtaken*
plan belongs deleted with its residue extracted, not left to be found.

### Verified

`fmt --all --check` clean; `clippy --workspace --all-targets -D warnings` clean; `test --workspace` at
**669 passing**, unchanged — this entry moves prose, not behaviour. `.agents/docs` no longer contains
either file. Nothing committed.

## `attr-stats` becomes a component: one measurement, four callers (2026-08-09)

`examples/attr-stats` was 2,785 lines of measurement locked inside a binary. It is now
**`crates/imbh-attrstats`**, and the same measurement backs four callers: the CLI (unchanged in
behaviour), `Db::attribute_stats`, `POST /api/head/attributes/stats`, and a new Attributes screen in
the TUI. ARCHITECTURE gained **§10.20** for the measurement and a paragraph in §10.19 for the
operation.

### What moved, and what the split forced me to decide

`accum.rs` and `scan.rs` moved verbatim (`git mv`). `main.rs` split three ways: the report model and
the two verdicts to `report.rs`, the fixed-width renderer to `text.rs` (returning `Vec<String>` rather
than printing, so a pane or a test can take it), and `Options` + `analyze()` to `lib.rs`. What was
left of the binary is 120 lines of argument parsing. The four end-to-end tests moved to
`tests/end_to_end.rs` and now drive the public entry point rather than a private `Config`.

Three decisions the extraction forced, none of which the binary had to make:

**`--last <minutes>` became `Options::range: Option<(i64, i64)>`.** A CLI can ask for "the last N
minutes"; a head asking for statistics over the window it is *displaying* cannot — its window is
absolute, and often historical. Generalizing to an absolute range made the CLI flag a two-line
convenience (`with_last_minutes`) and gave the TUI its cost bound for free: the Attributes screen
measures the segments overlapping the selected time range, so pan/zoom/range changes are the throttle.
This is the change that makes the screen viable at all — an unbounded scan on every screen switch
would not be.

**A mistyped path was silently an empty database.** `read_disk_snapshot` answers "no segments" for a
directory with no manifest, which is correct for a database before its first seal and wrong for
`./demo-dbb`. Both read as a clean measurement of nothing. `analyze` now refuses a path that is not a
directory, and the test pins *both* halves — the refusal, and that an existing empty directory still
measures as empty. Pre-existing behaviour of the old tool; it surfaced only because writing a library
test made me ask what the error cases were.

**The report needed serde derives *and* the hand-written JSON.** The derived `Serialize` is the wire
form (lossless, what the head API sends); `report::to_json` stays as the `--json` document, because it
is flattened for `jq` — verdicts inlined per key, the cardinality curve carrying both endpoints — and
its field names are the ones the write-ups quote. Making the wire form the documented one would have
broken every recorded reading of it.

### The head operation is the first one that scans

Twelve head operations answer a query; this one reads the attribute columns of every sealed segment in
range. Three consequences, all recorded in §10.19: the request body **is** `imbh_attrstats::Options`
(not a wire type describing it, so local and remote measure the same thing under the same caps); the
response is the whole `Report` (a head ranks it itself); and `offload` stops mattering
"for consistency" and starts mattering for real — a multi-second scan on a connection's runtime worker
is a different thing from a 2 ms one. The client sets only a *connect* timeout, so an accepted request
waits as long as the scan takes; I had written the opposite in the doc comment before reading
`HeadClient::new`.

`head_e2e` needed its first **on-disk** database. Every other case there runs on `Db::in_memory()` —
which has no segments, so it cannot measure anything. That is now an asserted behaviour rather than a
gap: an in-memory daemon answers `400` naming the reason, because "no segments to measure" and "no
attributes" are different claims and an empty report would state the wrong one.

### The TUI screen, and two bugs adding a screen exposed

Fifth screen (`5`), no query pane — like Overview it is a report over the database, not an answer to a
query. The table puts **both verdicts immediately after the key**, ahead of the evidence they are
derived from, because a narrow terminal clips the rightmost columns and clipping the conclusion would
leave the screen saying nothing. Caveats that no column can carry — unsealed WAL frames, skipped
segments, engaged caps, "no sealed segments in this range" — go in the detail pane below.

Adding the screen surfaced two latent bugs, and the difference between them is the point:

- **`focus_ring` hard-coded `Menu(0..3)`.** Five screens, four reachable by Tab. The existing test
  looped `1..Screen::ORDER.len()`, so it *caught* it immediately. Fixed by deriving the ring from
  `ORDER` rather than restating it.
- **`App::query` was `[String; 4]`** and `menu_cursor_wraps_over_screens_and_the_range_item` walked the
  screens by name. The array became `[String; Screen::ORDER.len()]`; the test now walks
  `ORDER[Traces+1..]`.

Both were "a constant restated where a constant already existed". The tests that caught them were the
ones written against `ORDER`; the one that had to be edited was the one written against the screen
names. That is a usable rule for a fixed enumeration: assert against the enumeration, not against
today's members of it.

### Feature placement, and the trap I had already hit twice

`imbh-attrstats` is a crate beside the engine crates (`core ← storage ← attrstats`), not a module in
`imbh-storage` and not a layer above the facade — because the CLI must measure a *directory* without
opening a `Db`, while the facade wants a `Db` method. It reaches it through an **off-by-default
`attrstats` feature**: a diagnostic, not part of ingest or query, so an embedded host that never asks
should not carry the scanner.

`imbh-head`, `imbh-server` and `imbh-tui` each name `imbh/attrstats` **explicitly** rather than
inheriting it from a sibling's copy of the dependency. This is the third time in two sessions that a
Cargo feature has broken a build I was not running (the `query`-gated `crate::sql`, then
`imbh-storage`'s `compaction` missing from `default`), and the shape is always the same: `cargo build
--workspace` unifies features, so a crate that *needs* a feature compiles anyway as long as some
sibling turns it on. The standalone build is the one that tells the truth. Same reason
`imbh-attrstats` has `default = ["serde"]` — a standalone `cargo test -p imbh-attrstats` must compile
the JSON view its tests assert on — while the workspace dependency pins `default-features = false` so
consumers still opt in. That combination is exactly how `imbh-storage`'s `search` is arranged, and it
was already the answer.

`--no-default-features` on the new crate then failed on its own test file, which imported `to_json`
unconditionally. The library was fine; only the test was wrong. Gated the import and the two
assertions.

### Measured

| axis | before | after |
|---|---|---|
| `imbh` facade, unique crates (footprint gate) | 275 | **275** (unchanged — feature is off) |
| `imbh` with `--features attrstats` | — | 276 (+1: the crate itself) |
| `imbh-server` unique crates | 297 | **298** (+1) |
| workspace tests | 668 | **683** |

No third-party subtree enters anywhere: imbh-core, imbh-storage, arrow, parquet, xxhash-rust,
serde and serde_json are all already compiled in any build that has storage.

### Verified

`fmt --all --check` clean. `clippy --workspace --all-targets -D warnings` clean, plus, standalone:
producer (`ingest`), consumer (`query`), storage-only, `attrstats` alone, `attrstats,serde`,
`housekeeper`, no-compaction, `imbh-attrstats` with and without default features, `imbh-head` full and
`dto`-only, `imbh-tui`, `imbh-server`. `test --workspace` **683 passing, 0 failing**; `cargo test -p
imbh-attrstats` and `-p imbh --features attrstats --test attribute_stats` green standalone. Smoke-run
against a `gen-demo-db` corpus produces the same report as before the extraction. Nothing committed.

## Addendum: the attribute block belongs on the Overview, and must not block it (2026-08-09)

Two corrections to the entry above, both from the user, and both about the same thing: an expensive
measurement should not get its own screen, and should never be on the critical path of a render.

### The fifth screen is gone

`Screen::Attributes` is reverted — `ORDER` is back to four, the `5` binding and the menu item with
it — and the block now lives at the bottom of the **Overview**, under the database gauges it belongs
with. That is the honest placement: it is a *report about the database*, which is what the Overview
already is, and it was never a fifth peer of Metrics/Traces/Logs (all three answer a query; this
answers none). The screen-count churn from the entry above stays useful anyway — `focus_ring` is
still derived from `ORDER` rather than restating it, which is a latent bug fixed regardless of how
many screens there are.

### Two answers, one pane, arriving independently

The Overview is now the only pane whose content comes from **two requests on different time scales**:
the gauges are a query (milliseconds), the attribute statistics are a scan of every sealed segment's
attribute columns (seconds, on a real corpus). Blocking the first on the second — which the first
implementation did, by making them one `load_snapshot` arm — makes the whole screen as slow as the
scan.

The split needed three pieces:

- **`Snapshot::attr_from`** — the index in `lines` where the asynchronous half begins. The refresh
  ships the gauges plus a placeholder from there down; the measurement *replaces* everything from that
  index rather than appending, so a re-arrival cannot stack two blocks. Modelled on the existing
  `list_from`, which is the same idea (lines below this index are special).
- **`Update::AttributeStats`**, delivered like `Update::Waterfall` — the pattern was already there for
  a fetch that fills a pane after the fact.
- **`App::compose_attr_stats`**, called from *both* arrivals. Order is not guaranteed: an empty
  database measures faster than the gauges query answers. Composing from either side, with the same
  guard, makes the order irrelevant instead of unlikely — and the test drives both.

### The guard is the range *selection*, not the generation

First cut keyed the measurement on the refresh generation, which is what every other async fetch here
uses. That is wrong for this one, and the reason is the cost: auto-refresh bumps the generation every
5 s, so a generation-keyed measurement is invalidated every tick and the corpus is rescanned
**continuously**. Correct behaviour and cheap behaviour turn out to be the same thing here — reuse the
measurement whenever the answer would not differ.

So the key is `(abs_window, lookback)`: the user's *range selection*, not the computed `[start, end]`.
A rolling window's absolute bounds move every tick even though the user has not touched anything;
the selection changes exactly when the answer should. On top of that a 60 s staleness bound (numbers
do drift as segments seal) and `r` clearing the cache outright, so an explicit refresh means "measure
now" rather than "show me the cached minute".

Worth stating as a rule: **a cache key should name what the user chose, not what the code computed
from it.** The computed window is a function of the selection *and the clock*, and the clock is
exactly the part that must not invalidate a corpus scan.

### Verified

`fmt --all --check` clean; `clippy --workspace --all-targets -D warnings` clean; `test --workspace`
**686 passing**. `imbh-tui` alone at 152, including both arrival orders, the wrong-range drop, and the
reuse-across-ticks rule. Footprint gate OK — facade 275 crates (target), `imbhd` 33.4 MiB (target
42 MB), idle RSS 14.9 MB, steady 104.7 MB. Nothing committed.

## Addendum: the attribute block gets its own time range (2026-08-09)

The Overview's attribute statistics now take a window independent of the query range (`a`), because
the two answer different questions over different horizons: a `promote` list is chosen from days of
data, while the panels beside it are read over the last fifteen minutes. Tying the measurement to the
query range meant one of the two was always wrong.

**Reuse the form, add a destination.** The absolute-range form already exists — two UTC datetime
fields, a shared caret, its own parse errors — so it gained an `AbsTarget` (`Query` | `Attributes`)
rather than a second form. `commit_absolute` branches only at the point where the parsed window is
stored, and the attribute branch deliberately does *not* reset the row cursor: it changes one block
inside a pane the user is already reading, not what the pane is showing.

**Clearing both fields is the way back.** An independent window needs an off switch, and an empty form
is already the natural spelling of "no window of my own" — so it means "follow the query range" under
`AbsTarget::Attributes`, and stays a parse error under `Query`, which has no follow state to fall back
to. The hint line says so; nothing else would have suggested an empty field meant anything.

**The block states its window.** Once the two can differ, a reader cannot infer which one produced the
numbers, so the header names the span and says whether it is following the query range or measuring
its own. `Report::range` already carried the window — the caller only has to say which kind it is.

**The cache key needed nothing new.** `attr_key` was already the *selection* rather than the computed
window (the previous addendum's correction), so an independent window simply becomes the selection:
`(Some(window), Duration::ZERO)`. Setting, changing, or clearing it invalidates the measurement for
free, and two identical windows reached by different routes compare equal — which is right, since they
measure the same thing.

### Verified

`fmt --all --check` clean; `clippy --workspace --all-targets -D warnings` clean; `test --workspace`
**688 passing** (`imbh-tui` 154, including the independent-window round trip, the empty-form clear,
and `a` staying inert off the Overview). Nothing committed.

## Addendum: the attribute statistics become a pane, and their range belongs to it (2026-08-09)

Two more corrections from the user, and they turn out to be the same correction: **the attribute
statistics are a thing, so they should have a pane, and their controls should live on it.**

### A pane, not a block appended to the gauges

`Snapshot::attr_from` — the index into `lines` where the asynchronously-filled block began — is gone,
and with it the whole idea of splicing two answers into one list. The Overview now renders two panes,
and the machinery that already existed for that (`Snapshot::detail`) does the work: the measurement
replaces a whole pane rather than a slice of a line vector, so "a re-arrival cannot stack two blocks"
stops being an invariant to maintain and becomes a consequence of assignment.

`DetailPane` gained a `DetailStyle` because the two users of it want opposite things:

- `Preview` (the Traces waterfall) — bare title line, no borders so bars sit flush, no scroll,
  overflow reported in the title, full view one Enter away.
- `Pane` (the attribute statistics) — bordered like the primary, and **it** takes the screen's scroll,
  because it is the long content while the pane above it is a fixed ten lines.

That last point also fixed the split. The old 55/45 gave half the screen to the gauges and cropped the
list that actually needed the room; a `Pane` detail now sizes the primary to its own content and takes
everything else.

### The range control belongs on the pane, not on the global key map

The first cut bound `a` globally and dropped the form under the header's time indicator. Both were
wrong for the same reason: that indicator *is* the query window, and a form appearing there says it
edits the query window whatever its title claims. A global key says the same thing — `t` is global
because the query range is global.

So the attribute pane became a **focus stop** (`Focus::Attributes`, appended to the ring when the pane
is shown), Enter on it opens its range form, and the form is anchored over **the pane**. `a` is gone.
The anchor is passed in rather than hardcoded, and `draw_absolute_range` now hangs the popup from the
anchor's near edge — right-aligned under a narrow header strip, left-aligned inside a full-width pane,
where its title is.

The general rule this is an instance of: **a control that changes one pane belongs to that pane** —
in its focus ring, in its title, and positioned over it. A global binding is for something global, and
the giveaway that this one was not is that it needed a screen guard (`a` only on the Overview) to be
safe.

### Verified

`fmt --all --check` clean; `clippy --workspace --all-targets -D warnings` clean; `test --workspace`
**688 passing**, `imbh-tui` 154 — including the focus ring gaining and losing the stop with the pane,
Enter on a stale `Attributes` focus doing nothing, and `a` no longer being a binding at all. Nothing
committed.

## Addendum: the attribute pane renders as a table again, and the header stops lying (2026-08-09)

Two small corrections, both about the same failure mode as the previous round: making something
*look* like what it is.

### A result set should render as a table

Folding the dedicated screen into a pane had quietly downgraded the keys from a `TableData` — column
widths measured by display width, a cyan/bold/underlined header row — to preformatted text inside a
`Paragraph`. Same characters, but it reads like a log line rather than a result set, and the column
alignment was mine to maintain by hand in a `format!` string.

So `DetailPane` gained an optional `table: Option<TableData>`, and a `Pane`-style detail now renders
as **prose above a table**: the pane's own block, then the notes, then the table with a real header
row. The width and header styling were lifted out of `draw_metric_table` into `column_constraints` /
`table_header` and shared, so the two panes cannot drift apart.

The split of what goes where is deliberate: the prose is what *qualifies* the numbers — the window
measured, unsealed WAL frames, skipped segments — so it sits above the table and does **not** scroll.
A caveat that scrolls out of a long table is a caveat nobody reads. Only the rows scroll, which also
made the scroll arithmetic honest: `max_scroll` is now row count minus viewport minus the header row,
rather than a wrapped-line estimate over the whole pane.

No row selection. The old dedicated screen had one because it was the primary pane, but there is no
per-row action here, and a highlighted row that Enter does nothing with is a false affordance —
Enter on this pane opens its range.

### The header highlighted a window the form was not editing

Opening the attribute range lit up the menu bar's time indicator, because both forms are
`Mode::AbsoluteRange` and the highlight tested only the mode. That indicator *is* the query window, so
it was announcing a change to something the form would not touch — the same category of error as
anchoring the form there in the first place, which the previous round fixed.

The predicate is now `editing_query_window(app)`: the mode **and** `abs_target == Query`. It gates
both the bar-wide focus colour and the range item's cursor chip.

**The general shape, three rounds running: a control's chrome must follow what it changes, not merely
that it is open.** Position, focus stop, highlight — each of them was defaulting to "the query
window" because that used to be the only window there was.

### Verified

`fmt --all --check` clean; `clippy --workspace --all-targets -D warnings` clean; `test --workspace`
**689 passing**, `imbh-tui` 155 — including the pane's table header and row contents, the caveats
staying in the prose above it, and the header highlight distinguishing the two form targets. Nothing
committed.

## Addendum: the attribute pane's range defaults to all of time, and its form drops from its own range line (2026-08-09)

### All of time, and one fewer state

The pane's window no longer follows the query range when unset — `None` now means **every sealed
segment**, and that is the default. Which is the right default for the question: a `promote` list is
chosen from everything the database holds, not from whatever fifteen-minute window the panels beside
it happen to be showing. Following the query range was inherited thinking from when the statistics
were part of a panel.

The pleasant part is what it deleted. `AttrWindow` was `(Option<(i64,i64)>, Duration)` — the absolute
window *and* the relative preset, because "follow the query range" made the answer depend on both.
It is now just `Option<(i64,i64)>`: the window measured **is** the cache key. Three consequences fall
out for free:

- Moving the query range no longer invalidates the measurement, so the corpus is rescanned strictly
  less often.
- The three-state model (all / follow / fixed) collapses to two (all / fixed).
- The form's empty state and the range's unbounded state became the same thing in both directions:
  opening the form on an unbounded range shows empty fields, and submitting empty fields sets an
  unbounded range. Nothing extra to learn, and no third spelling to explain in the hint line.

### The form drops from the value it edits

Anchoring it over the *pane* was still one level too coarse. The pane displays its window on its first
line, so that line is what the form edits, and that is now what it hangs from — `attr_area` publishes
the range line's rect (one row) rather than the whole pane, and the existing "narrow anchor → drop
below it" branch in `draw_absolute_range` does the rest without a special case.

That completes the set from the previous round: **position, focus stop, and highlight all follow what
the control changes.** The last of the three landed here too — the menu bar's time indicator no longer
lights up for a form editing the attribute window, since both forms share `Mode::AbsoluteRange` and
only the target tells them apart.

### Verified

`fmt --all --check` clean; `clippy --workspace --all-targets -D warnings` clean; `test --workspace`
**689 passing**, `imbh-tui` 155 — including the unbounded default, the empty-form round trip in both
directions, and that a query-range change leaves the measurement cached while a pane-range change
invalidates it. Nothing committed.

## An explicit promotion API, and the one place the explorer writes (2026-08-09)

The attribute pane measured what *should* be promoted and could do nothing about it; changing the set
meant restarting the daemon with a different `DbBuilder::promote` list. `GET`/`POST /admin/promote`
closes that loop, and the TUI's attribute pane gained an `on` column and a `p` key.

### Not a head operation, deliberately

`/api/head` is documented read-only, and this writes: `Db::set_promote` seals the buffer so no batch
straddles the change, and every segment sealed afterwards carries the new column set. So it went on
`/admin/*` beside `flush` and `compact`. The point is not taxonomy — it is that a deployment gates a
*prefix*. One read-only prefix and one write prefix is a rule someone can apply; "read-only except one
route" is a rule someone gets wrong.

The client is the exception to the exception: `HeadClient` carries `promoted`/`set_promoted` anyway,
because the transport, the base-URL parsing and the error mapping are all already there and a second
client beside it would be worse. The crate docs name it as the one write, and the path constant lives
next to the head paths with the reason attached.

### The whole set, not a delta

Promotion is a **list** whose order is the column order. `{"keys": [...]}` replaces it wholesale, for
two reasons that are easy to miss: a delta would ask the server to guess *where* in the order a new key
goes, and two concurrent callers each sending "add mine" would each silently lose the other's change.

The response is the set **now in effect**, not an echo. The daemon filters keys that collide with a
built-in column name at schema construction, so what was asked for and what is in force can differ — a
head that assumed its own request had been applied would show a key as promoted that is not. The TUI
re-measures on the answer rather than on the request.

### Disabled where it cannot work, and for a structural reason

A TUI session that opened the directory itself holds a `Db::open_read_only` view: no writer lock, every
write refused by construction. So `Backend::can_promote()` is `matches!(self, Remote(_))` — not a
permission check bolted onto a UI, but the observation that there is no local implementation to call.
Three consequences, all of them the honest version:

- `p` is advertised in the pane title only where it exists.
- Pressed anyway, it reports *what would work* (`imbh-tui --url http://host:4318`) rather than
  surfacing the storage layer's read-only error, which would describe the mechanism instead of the fix.
- `Backend::set_promoted` refuses on the local arm up front rather than attempting and failing, so the
  message is not a race between two explanations.

### The pane's cursor exists now, because there is finally something to do with it

Two rounds ago I left the attribute table unselectable, on the grounds that a highlighted row Enter
does nothing with is a false affordance. `p` is that something, so the pane took a cursor — and only
while focused, since an unfocused pane's highlight would act on nothing. `selectable_bounds` checks the
focused pane first, which is also what routes ↑/↓ to it.

The `on` column sits immediately after the key, ahead of the `promote` verdict: the verdict is only
useful *next to the current state*, and `p` acts on exactly that column. Both are read from the same
task as the measurement, so the two halves of one screen cannot disagree about what is promoted.

### Verified

`fmt --all --check` clean; `clippy --workspace --all-targets -D warnings` clean; `test --workspace`
**691 passing**, `imbh-server`'s `head_e2e` 7 — including the read/replace round trip over a real
socket, the daemon's live set matching the response, demotion, and a malformed body answering `400`
rather than `500`. `imbh-tui` 156, including the local-session refusal naming `--url` and the `p`
paging binding staying intact off the pane. Nothing committed.

## Addendum: the attribute pane breaks down per scan unit (2026-08-09)

The per-table measurement had existed since the CLI — `Report.tables` carries a `UnitReport` per
`logs`/`spans`/the five metric tables — and the pane was showing only `Report.global`. The reasoning
was sound and the omission was not: `promote` is DB-wide, so the roll-up is what the *decision* is made
against, but it is not where the numbers are *defined*, and it silently hides which signal a key even
belongs to.

So the table is now grouped: `ALL TABLES` first, then one section per table with segments in range,
empty tables left out. Three things this gets right that the flat view could not:

- **Sigma is per table by definition** — its denominator is that table's segment count. The roll-up's
  `index@` is a best case *across* tables, which is fine as a summary and misleading as a measurement.
  Each section now answers for itself.
- **Which signal carries the key.** A key on `logs` and on `metrics_gauge` appears in both sections,
  which is the answer to a question the roll-up cannot express.
- **The promote verdict stays DB-wide.** Per-table rows leave it blank rather than repeating a
  judgement made against different totals — a key can look cheap in one table and not in the roll-up,
  and showing both as "promote: yes" would imply a per-table decision that does not exist.

`index_scale` grew a per-unit sibling in `imbh-attrstats` (`index_scale_in`) and became the fold over
it, rather than the TUI re-deriving the `INDEX_MAX_SIGMA` rule with the library's constant.

### Two kinds of row, and `p` has to tell them apart

A grouped table holds section headers as well as key rows, and `p` acts on the row under the cursor.
"Is this a key" therefore has to be a *fact about the row*, not a guess from its text: a header leaves
the **scope** cell empty, and a key row's scope is always one of three non-empty names. One helper,
`attr_row_key`, is the single reader of that invariant — used by the promotion toggle, by the renderer
(which styles headers as chrome), and by the tests. The alternative, a parallel `Vec<Option<String>>`
aligned with the rows like the Metrics catalog's `tree_rows`, would have worked too; this is smaller
and the invariant is genuinely structural rather than incidental.

### Verified

`fmt --all --check` clean; `clippy --workspace --all-targets -D warnings` clean; `test --workspace`
**692 passing**, `imbh-tui` 157 — including a fixture with logs *and* metrics asserting the roll-up
leads, both signals get sections, an empty table gets none, a cross-signal key appears under both, and
no per-table row carries a promote verdict. Nothing committed.

## Addendum: the attribute pane's two focus stops, and per-section headers (2026-08-09)

### One pane, two things to act on, two stops

`Focus::Attributes` became `Focus::AttrRange` and `Focus::AttrTable`. The pane has two things to act
on — the window it was measured over, and the key under the cursor — and one stop made Enter ambiguous
between them: it opened the range while the cursor sat on a row, which is exactly the kind of "acts on
something the screen never marked as selected" the earlier rounds were about.

Split, everything lines up: the range line is highlighted when the ring is on it (so Enter has a
visible target), the row cursor exists only on the table stop, each stop advertises only its own action
in the pane title, and Enter on the table does nothing rather than guessing. `p` is guarded on
`AttrTable` for the same reason.

### Every section is its own table

Each section now carries a title *and its own column header*, rather than one header pinned above all
of them. That is the honest rendering of what a section is: coverage is against **that unit's** rows
and sigma against **that unit's** segments, so a single header floating over every section implies one
set of totals that does not exist.

This is what forced the row-kind question. With headers repeated inside the table, "is this row a key"
could no longer be read off the scope cell — a header row's scope cell says `Scope`. So `PaneTable`
now carries `kinds: Vec<AttrRow>` beside its rows: one struct rather than two fields, so "aligned with
`data.rows`" is a property of the type instead of a comment two call sites have to honour. The
renderer styles by kind, `p` reads it to know whether there is a key under the cursor, and the tests
assert against it.

A width bug fell out of the same change: column widths were measured over *every* row, so a section
title — one long banner cell — stretched the key column to its own length and squeezed the numbers off
the right edge. Widths now come from the key rows only.

### Ordering: verified rather than changed

"The order of the table must follow that in the header" turned out to already hold: `Db::stats()`
(which the gauges pane lists) and `Report.tables` (which the sections follow) are both `Table::ALL`
order, on both the writer and the read-only reader path. "Already holds" is not the same as "cannot
drift", though — they are two independent lists in two crates that do not know about each other, and a
reader scanning down the screen matches them by position. So it is now a test rather than a
coincidence.

### Verified

`fmt --all --check` clean; `clippy --workspace --all-targets -D warnings` clean; `test --workspace`
**693 passing**, `imbh-tui` 158 — including the two stops in reading order, both snapping to `Primary`
off the Overview, Enter doing nothing on the table stop, every section title being followed by its own
column header, and the section order matching the gauges list. Nothing committed.

## Addendum: a banner is not a table cell (2026-08-09)

Two rendering corrections, and the first one is the interesting one because I caused it two changes
earlier and only the user could see it.

### Titles were being clipped to the key column

The section titles live in the first cell of their row, and a ratatui `Table` clips every cell to its
column. So `metrics_gauge - 1 segment, 488 rows, 2 keys` rendered as `metrics_gauge - 1 s>`. I had
*made this worse* in the same round it appeared: measuring widths over key rows only was right (a title
would otherwise stretch the key column to its own length and push the numbers off the pane), but it
also shrank the column the title was being clipped to.

Both constraints are real and a cell-based table cannot satisfy them at once — a cell is clipped to its
column, and there is no column span. So the pane's body is now a `List` of styled `Line`s with columns
padded by hand: same alignment, same header styling, and a title that spans the pane because it is not
in a column at all. Selection and scrolling come from `ListState` exactly as they did from
`TableState`.

The general form: **a banner and a cell want opposite things from a layout, so one of them is not a
cell.** Reaching for the table widget was right for the rows and wrong for the titles, and the fix is
not a wider column.

### Titles are bold, not coloured

Colour in this pane is already doing two jobs — cyan for the column header, cyan-on-black for the
cursor — and a third would compete without meaning anything. A section title marks *structure*, not a
category, so intensity is the right signal and the yellow is gone.

### Verified

`fmt --all --check` clean; `clippy --workspace --all-targets -D warnings` clean; `test --workspace`
**694 passing**, `imbh-tui` 159 — including a regression test on the exact clipping (a title reaching
the pane whole, while the key column stays sized to its data). Nothing committed.

## MCP gets the attribute legs, and a write whose capability follows the handle (2026-08-09)

Three tools: `attribute_stats`, `list_promoted_attributes`, `set_promoted_attributes`. The first two
are ordinary reads. The third is the first **write** on a surface whose crate doc opened with "Nothing
here can write", so it needed the same care the head API's did — and the answer turned out better here
than a prefix split.

### The capability follows the handle, not a flag

`tools/list` is now built from `tools::visible(db)`, which drops write tools when `db.is_read_only()`.
That is not a permission check bolted onto the surface: a read-only handle holds no writer lock and
refuses every write by construction, so the tool has **nothing to call** there. Hiding it means an
agent is never offered an action that cannot succeed, and a client that caches the list never learns
it exists.

The check is repeated inside the tool anyway, because a client may be working from a list it cached
before the server was restarted read-only — and the message it gets then says *why the action is
absent* and what would have it, rather than surfacing the storage layer's read-only error.

`Db::is_read_only()` is new on the facade for this. It is the same question the TUI's
`Backend::can_promote()` answers, and both are better than "try it and see": a UI that knows what it
is connected to beats one that finds out by failing.

### The read is shaped for an agent, and says what it could not cover

`attribute_stats` returns a purpose-built document rather than the report verbatim — MCP results are
deliberately lossy (§10.16.1) and the full report is large. What survives is what a decision needs:
per key, the current promoted state, both verdicts, and the four numbers behind them, capped by `top`
with `truncated` set per unit. `unsealed_wal_frames` and `segments_skipped` ride along, because a
measurement that reads like full coverage when it is not is worse than no measurement.

Its window defaults to **every sealed segment**, unlike every other tool here, which default to an
hour. That is not an oversight to be normalised: a `promote` list is chosen from the whole corpus, and
the window arguments exist to bound the scan, not because a window is the natural unit of the question.

### Verified

`fmt --all --check` clean; `clippy --workspace --all-targets -D warnings` clean. `imbh-server`'s
`mcp_e2e` at 12 — the loop end to end (measure → promote → the measurement now reporting it promoted),
per-table sections giving no promote verdict, and the write tool absent from a read-only server's
`tools/list` while its reads stay. Nothing committed.

## Session consolidation: `attr-stats` from example to product surface (2026-08-09)

Eleven entries above log this session as it happened. This one is the summary a future reader wants
first: what shipped, what it cost, and the findings that outlive the feature.

### What shipped

A measurement that lived inside one example binary is now a component with **five callers**:

| Surface | What it does |
|---|---|
| `crates/imbh-attrstats` | the measurement: sigma, the cardinality curve, run structure, the two verdicts. No third-party dependency; `serde` behind a feature |
| `examples/attr-stats` | unchanged CLI, now ~120 lines of argument parsing |
| `Db::attribute_stats` | facade method behind the off-by-default `attrstats` feature, mirrored on `BlockingDb` |
| `POST /api/head/attributes/stats` | so a head measures the daemon's database |
| `imbh-tui` Overview | a second pane, measured asynchronously, with its own time range |

Plus the action the measurement implies: `GET`/`POST /admin/promote`, `p` on the TUI pane, and three
MCP tools (`attribute_stats`, `list_promoted_attributes`, `set_promoted_attributes`). Reading the
verdicts and acting on them are now one loop instead of a report and a daemon restart.

### Measured

| axis | before | after |
|---|---|---|
| `imbh` facade, unique crates (footprint gate) | 275 | **275** — unchanged, the feature is off |
| `imbh` with `--features attrstats` | — | 276 (+1: the crate itself) |
| `imbh-server` unique crates | 297 | **298** (+1) |
| `imbhd` release binary | — | 33.4 MiB against a 42 MB target |
| workspace tests | 668 | **697** |
| MCP tools | 15 | 18 (17 read-only) |

40 files, +3,614/−1,658. No third-party subtree enters anywhere: imbh-core, imbh-storage, arrow,
parquet, xxhash-rust, serde and serde_json are already compiled in any build that has storage.

### Findings that generalize

**A cache key should name what the user chose, not what the code computed from it.** The attribute
measurement was first keyed on the resolved `[start, end]`. A rolling window's bounds move every tick,
so an auto-refreshing Overview rescanned the whole corpus every five seconds. Keyed on the *selection*
instead, it invalidates exactly when the answer would differ. The computed window is a function of the
selection **and the clock**, and the clock is the part that must not invalidate a scan.

**A control's chrome must follow what it changes, not merely that it is open.** This cost four rounds
of correction, one per aspect: the range form's *position* (anchored under the header's query-range
indicator, then over the pane, finally on the range line it edits), its *key* (a global `a`, then a
focus stop on the pane), the header's *highlight* (lit for a form that did not touch it), and finally
the *focus stop itself* splitting in two once the pane had two things to act on. Each defaulted to
"the query window" because that used to be the only window there was. The tell for the global key was
that it needed a screen guard to be safe.

**A banner and a cell want opposite things from a layout, so one of them is not a cell.** Section
titles were clipped to the key column because a table cell is clipped to its column and there is no
column span — and measuring widths over key rows only (right for the columns) shrank the column doing
the clipping. Both constraints are real; the widget was wrong. Styled lines with hand-padded columns
satisfy both.

**Selection is an affordance, so it should exist only where there is something to select.** The pane
had no cursor while nothing acted on a row; `p` arrived and it got one — and once the table grew
section titles, per-section headers and spacers, the cursor had to step *key-to-key* rather than by row
index. A cursor that can land on structure offers an action with nothing to act on.

**Where a capability comes from the handle, derive it — do not add a flag.** Promotion writes, and a
read-only handle has no writer lock, so there is no local implementation to call: `Backend::can_promote`
is `matches!(self, Remote(_))` and MCP's `tools/list` drops write tools when `db.is_read_only()`. Both
are structural facts rather than policy someone must remember to configure. The corollary is the error
message: refuse up front and say *what would work* (`--url …`, "point the client at the process that
writes"), not what the storage layer would have said.

**Read-only is a property of a prefix, not of a route.** `/api/head` stays read-only and promotion went
to `/admin/*` beside `flush` and `compact`. "One read-only prefix, one write prefix" is a rule a
deployment can apply; "read-only except one route" is a rule someone gets wrong.

**Send the whole set, not a delta**, for anything whose *order* is meaningful. Promotion is a list and
its order is the column order: a delta would ask the server to guess placement, and two concurrent
callers would each silently lose the other's change. And answer with the state now in effect rather
than an echo — the daemon filters keys colliding with built-in column names, so request and result can
differ.

**Assert against the enumeration, not against today's members of it.** Adding a fifth screen (later
reverted) broke two things: `focus_ring` hard-coded `Menu(0..3)`, and a nav test walked the screens by
name. The tests written against `Screen::ORDER` caught their bug immediately; the one written against
the names had to be edited. Same lesson in the ordering check: the attribute sections and the Overview
gauges already agreed, but they are two independent lists in two crates, so agreement is now a test
rather than a coincidence.

**The standalone build is the one that tells the truth.** Third time this session that a Cargo feature
broke a build I was not running — `cargo build --workspace` unifies features, so a crate that *needs* a
feature compiles anyway as long as some sibling enables it. Every consumer now names `imbh/attrstats`
explicitly rather than inheriting it, and `imbh-attrstats` carries `default = ["serde"]` so a
standalone `cargo test -p imbh-attrstats` compiles the JSON view its tests assert on — the same
arrangement as `imbh-storage`'s `search`.

### Open

Nothing committed. The branch still carries the earlier housekeeping work; the CHANGELOG entries are
under `[Unreleased]`. Two UI details landed after this summary was written and have their own addenda
below: a blank line between sections with the cursor skipping it, and a Tab stop per section. `examples/attr-stats` remains the only caller pointed at real data — the TODO
item asking for that is now cheaper to satisfy, since the measurement can be read off a running daemon
or the TUI without shipping a CLI to the machine holding the data.

## Addendum: a Tab stop per section (2026-08-09)

`Focus::AttrTable` became `Focus::AttrTable(usize)` — one stop per section rather than one for the
whole table. The reason is the one that made the sections worth having in the first place: each
section *is* a table, with its own totals and its own sigma, and a single stop made the third one
reachable only by scrolling past every key of the first two.

Three things follow, and each is the honest version of something that was previously implicit:

- **The cursor belongs to a section**, so `attr_key_indices` takes one and `move_attr_cursor` stops at
  that section's ends rather than walking into the next. Tab moves between sections; the arrows move
  within one. Two motions, two keys, no overlap.
- **Tab snaps the cursor into what it focused**, so the pane scrolls to the section the ring landed on
  instead of leaving the highlight somewhere off screen. That is `focus_advance` calling
  `snap_attr_cursor`, which is also what a fresh measurement calls — the same need, one implementation.
- **A section index can outlive its measurement.** A refresh can return fewer sections (a table that
  emptied out of the range), so `effective_focus` maps an out-of-range `AttrTable(n)` to `Primary`,
  exactly as it already did for a stop on a pane that is not shown. Without that, Tab would hand the
  cursor and `p` to a section that no longer exists.

The pane title now names the focused section, because with several stops on one pane the border
highlight alone no longer says *which*.

A test fixture caught the shape change usefully: the promotion-refusal test built a table of one key
row with no section, which used to work and now cannot — the cursor belongs to a section, so a table
without one has no cursor. Shaping the fixture like a real pane (section, header, key) fixed it, and
the failure was the design telling the test it had been under-specified all along.

### Verified

`fmt --all --check` clean; `clippy --workspace --all-targets -D warnings` clean; `imbh-tui` **160
passing**, including one stop per section, every section's cursor staying inside it, Tab landing the
cursor on a key, and a stale section index reading as `Primary`. Nothing committed.

## Addendum: the section edge is a pause, not a wall (2026-08-09)

The arrows now **hop** to the next section when the cursor is already at a section's edge, instead of
stopping there. The previous entry argued for the opposite ("Tab is what moves between sections"), and
that was wrong for a plain reason: it made the arrows unable to reach the pane's second table at all.
A list should not have a dead end that a second key is the only way out of.

The two motions now compose instead of competing — the arrows walk the whole pane top to bottom, Tab
jumps a section at a time — and the focus follows the cursor across a boundary, so `p` and the pane
title stay attached to the section actually being read.

Two details worth their own lines:

- **An overshoot is not a hop.** `PageDown` from the middle of a section lands on that section's last
  key; only a press with nowhere left to go inside the section crosses. Clamping first and hopping
  only when the clamp changed nothing is what gets both, and it is the behaviour every other list in
  the program already has at *its* end.
- **Empty sections are stepped over, not focused.** A stop whose cursor has nowhere to land would
  swallow a keypress and read as the arrows having stopped working.

### A test fixture that could not exercise the rule

Writing the test found that `sealed()` produced scan units with a *single* attribute key, so "moving
within a section" and "hopping between sections" were indistinguishable — every press was a hop. The
fixture now ingests record attributes as well as resource ones. Worth noting as a pattern: a fixture
minimal enough to be readable can be too minimal to tell two behaviours apart, and the test passes
either way until someone asks it the right question.

### Verified

`fmt --all --check` clean; `clippy --workspace --all-targets -D warnings` clean; `imbh-tui` **160
passing**, including a full walk down the pane asserting the visited rows are exactly the key rows in
order, the same walk back up, the focus ending on the last section and then the first, and the
overshoot stopping at a section end before crossing. Nothing committed.

## Closing summary: what the attribute work became, and what the iteration taught (2026-08-09)

The consolidation entry above was written mid-stream and its "Open" section is now stale. This closes
the session: the final state, and the findings that only became visible once the UI work was done.

### Final state

Fifteen entries cover this session. What exists at the end of it:

**A measurement, with five callers.** `crates/imbh-attrstats` holds sigma, the cardinality curve, run
structure and the two verdicts; `examples/attr-stats` is ~120 lines of argument parsing over it;
`Db::attribute_stats` is the facade method behind an off-by-default feature; `POST
/api/head/attributes/stats` is the head operation; the TUI Overview is a second pane.

**An action, with three callers.** `GET`/`POST /admin/promote` on `imbhd`, `p` on the TUI's attribute
pane, and `set_promoted_attributes` over MCP. Reading a verdict and acting on it is one loop now,
where before it was a report and a daemon restart with a different `DbBuilder::promote` list.

| axis | before | after |
|---|---|---|
| `imbh` facade, unique crates (footprint gate) | 275 | **275** — unchanged, the feature is off |
| `imbh` with `--features attrstats` | — | 276 (+1: the crate itself) |
| `imbh-server` unique crates | 297 | **298** (+1) |
| `imbhd` release binary | — | 33.4 MiB against a 42 MB target |
| workspace tests | 668 | **697** (`imbh-tui` 146 → 160) |
| MCP tools | 15 read-only | 18, of which 17 read-only |

47 files, +6,007/−1,658. No third-party subtree enters anywhere.

### The finding the UI rounds produced

Eight user corrections landed on one pane, and in hindsight they are all the same correction:
**folding a screen into a pane turns every implicit "there is only one of these" into a bug.** The
codebase had exactly one time range, one focus stop per pane, one table per screen, one place a range
form could drop from, one header that owned the range highlight. Each of those was true until this
pane existed, and each failed in its own way:

| implicit singular | how it surfaced |
|---|---|
| one time range | the attribute window followed the query window, so one of the two was always wrong |
| one place a form drops from | the range form appeared under the header's *query*-range indicator |
| one range highlight | the header lit up for a form that would not touch it |
| one focus stop per pane | Enter meant "change the range" while the cursor sat on a row |
| one table per pane | a single Tab stop made the third section reachable only by scrolling |
| one kind of row | `p` had to guess whether the cursor was on a key or on a title |
| one column layout | a section title was clipped to the key column |
| one list, contiguous | the cursor could land on structure, and then stopped dead at section edges |

None of these were visible from the code — every one arrived as "this looks wrong" from someone
running it. That is worth recording plainly: **a pane that is genuinely new invalidates assumptions
the rest of the UI never had to name, and the only reliable detector is a person using it.**

### Two reversals, recorded as reversals

Twice I wrote a justification and then had to undo it, and both are more useful as pairs than as
conclusions:

- **"Selection is a false affordance here"** → the pane had no cursor because nothing acted on a row.
  Correct at the time; wrong the moment `p` existed. The rule survives, the conclusion did not.
- **"Tab is what moves between sections"** → written one entry before the arrows had to hop at section
  edges. The reasoning was tidy and the result was a dead end: a list whose only exit is a *different*
  key. Tidy separation of concerns is not worth a dead end.

The general form: a UI decision justified by a property of *today's* feature set needs re-deriving
when the feature set moves, and the justification is what tells you whether it still holds.

### Method notes

**A fixture minimal enough to read can be too minimal to discriminate.** `sealed()` produced scan
units with a single attribute key, so "move within a section" and "hop between sections" were
indistinguishable — every press was a hop, and the test passed either way. It only failed once the
test asked the right question. Minimal fixtures are good; minimal fixtures that collapse two
behaviours into one are a silent gap.

**Assert against the enumeration, not today's members of it.** Two tests written against
`Screen::ORDER` caught their own bug when a screen was added; the one written against screen names had
to be edited. Same shape held for section ordering: the attribute sections and the Overview gauges
already agreed, but they are two independent lists in two crates, so agreement is now a test.

**The standalone build tells the truth.** Three feature breakages this session, all the same shape:
`cargo build --workspace` unifies features, so a crate that *needs* one compiles as long as some
sibling enables it. Every consumer now names `imbh/attrstats` explicitly, and `imbh-attrstats` carries
`default = ["serde"]` so its own tests compile standalone — the arrangement `imbh-storage`'s `search`
already used.

### Open

Nothing committed; the branch still carries the earlier housekeeping work and the CHANGELOG entries
sit under `[Unreleased]`. The one TODO this makes cheaper is pointing the measurement at production
data: it can now be read off a running daemon (`POST /api/head/attributes/stats`), off the TUI, or by
an agent over MCP, so no CLI has to be shipped to the machine holding the data and no database
directory has to leave it.

## Queued housekeeping on `imbhd`: the first endpoint whose answer is a handle (2026-08-09)

`POST /admin/housekeeping` returns `202` with a job id; `GET /admin/housekeeping/<id>` reports the
job; `GET /admin/housekeeping` lists the retained ones. A pass is seal → commit pending rewrites →
retention (`Db::maintain`), plus `Db::compact` under `{"compact": true}`. ARCHITECTURE §10.16.2.

### Why this one and not `/admin/flush`

Every other endpoint on this server answers a question whose cost follows the **answer**; a
housekeeping pass costs the **corpus**. Over a long retention window compaction runs for minutes,
which outlasts what a proxy will hold a connection open for and is far longer than a caller should
wait to learn its request was *accepted*. `/admin/flush` and `/admin/compact` stay synchronous
deliberately — they are the small immediate versions and existing tooling drives them that way. Making
them all async would have been symmetry for its own sake.

The consequence worth stating: **a `200` here would be a lie.** The work has not run when the response
is written, so the status has to be `202` and the body has to be a handle rather than a report.

### Design decisions, and what each is defending against

- **One permit for all passes.** Two concurrent passes over one database contend for the same disk
  and, worse, can each seal or compact segments the other had planned around. Serializing them is what
  makes "housekeeping is running" a state the database is in rather than a race to describe. It also
  means the queue is a queue: a second submission is *accepted* and waits, which is the behaviour a
  caller on a timer needs.
- **Maintenance before compaction inside a pass.** `maintain` commits pending rewrites and applies
  retention, so compaction afterwards works on the segment set that survives instead of merging
  segments retention is about to drop. Same ordering argument as `maintain`'s own internals.
- **Ids carry the process's start time.** Otherwise a restart resets a counter and an id from the old
  process describes somebody else's job. Now it is a `404` — and the message says ids do not survive a
  restart, because "not found" alone would read as "your job vanished".
- **Nothing is resumed across a restart, by design.** Seal, commit and retention are each individually
  crash-safe (§7), so an interrupted pass leaves no partial state to resume and the honest answer is
  "submit it again". A durable queue would add a recovery path for a problem that does not exist.
- **History is bounded, and eviction is oldest-*finished*-first.** A daemon running housekeeping on a
  timer for a year must not accumulate a year of records; but dropping a queued or running job would
  leave a client polling an id that answers `404` while the work it names is still happening. Those
  two requirements together are what makes the eviction rule specific rather than "drop the oldest".
- **Refused on a read-only handle.** A reader holds no writer lock, so the pass could never do
  anything: `400` with the reason beats a job that exists only to fail. Third instance of the same
  rule this session, after the promoted-set write and the TUI's `p`.

### The state change that was not free

The router had no state but `Arc<Db>`, and the queue has to live somewhere. It went into an
`AppState { db, jobs }` rather than a process global, because the queue's whole job is to serialize
passes over *one* database — a global would serialize them across every mounted router while handing
out ids that mean nothing to the others.

That rippled once: `head::routes()` returned `Router<Arc<Db>>` and a `merge` needs both halves to
agree on the state type. Making it generic over `S where Arc<Db>: FromRef<S>` is the idiomatic axum
answer and left every handler in it unchanged — they still ask for `State<Arc<Db>>`.

It also exposed a **latent trap in the test helper**: `imbh_server::route` builds a fresh router per
call, so a job submitted through one call cannot be polled through the next — the second call's
registry never issued that id. Harmless while the server was stateless, silently confusing now. It is
documented on `route` itself, and the e2e test uses a real `serve()` for the polling sequence.

### Verified

`fmt --all --check` clean; `clippy --workspace --all-targets -D warnings` clean; `imbh-server` **67
passing** — the submit/poll round trip over a real socket (`202`, a non-terminal state at submission,
a terminal one after polling, a report carrying both halves, timestamps that bracket the work), the
listing, a `404` for an unissued id, a `400` for a malformed body, an empty body meaning the defaults,
the read-only refusal queueing nothing, and the registry's own bookkeeping (bounded history, live jobs
never evicted, unique process-scoped ids). Nothing committed.

## Addendum: `max_jobs` on the housekeeping endpoint, and the bound it needed underneath (2026-08-09)

The endpoint had no equivalent of `imbh-housekeeper --max-jobs`, and the reason is that
**`Db::compact()` had no bound to expose**: it rewrites every eligible partition across every table in
one call. So a submitted pass could run for as long as the corpus takes, and the only choice was that
or not compacting.

`Storage::compact_bounded(max_partitions)` / `Db::compact_bounded` are the missing half; `compact()`
is now `compact_bounded(usize::MAX)`, so nothing existing changed. `{"max_jobs": N}` on the endpoint
caps the pass.

### Partitions, not segments

The bound counts **partitions rewritten**, because a partition is the unit compaction works in — it
reads a day's segments, concatenates, sorts, and writes one. A segment-count bound would have to stop
*inside* a partition, which is exactly the state the design avoids: the deferred-delete ordering in
§7 is what makes a compaction pass crash-safe, and a half-merged partition is not a state the manifest
can describe.

That is also why partitions past the bound are passed through **untouched** rather than partially
processed. A capped pass is a *slice*, not a partial one: the segments it did not reach are still
listed, still queryable, still exactly as they were. There is nothing to resume, so resuming is
submitting again — and a pass that rewrites nothing is how a drain loop learns it is done.

### Two small decisions that keep the API honest

- **`compaction_complete` rather than a silent truncation.** A capped pass and a pass that finished
  are indistinguishable from the counters alone, and the difference decides whether the caller loops.
  Derived from whether the pass reached its own cap, so it cannot drift from the bound that produced
  it.
- **`max_jobs: 0` is a `400`.** It would otherwise mean "compact, but do no compaction" — which
  `compact: false` already says — and the job would report success for having done nothing under a
  name that promised otherwise.

### Not done, and worth flagging

There is a *second* thing `max_jobs` could have meant: a cap on the **queue depth**. Nothing today
stops a client submitting a thousand passes, and since passes are serialized and housekeeping is
close to idempotent, everything after the second is waste. Coalescing ("a pass is already queued —
here is its id") is probably the better answer than a cap, and both are a different feature from the
one asked for. Recorded rather than guessed at.

### A pre-existing breakage this surfaced

`cargo clippy -p imbh-storage --no-default-features --all-targets` does not build, and did not before
this change either: `pending.rs`'s test module calls `write`, which is `#[cfg(feature = "compaction")]`
while the tests are not, and `lib.rs`'s test module names `ParquetRecordBatchReaderBuilder`, whose
import is feature-gated. CI never runs that combination — it builds `-p imbh --no-default-features` and
`cargo test -p imbh-storage` with defaults — so nothing caught it.

The `pending.rs` half is gated now (its tests write a record in order to read one back, so they need
the feature that produces one). The `lib.rs` half is left alone: it is a test module this change does
not touch, and fixing it properly means auditing which of its tests need which feature, which is a
different task from the one asked for. Every *library* configuration builds clean, which is what a
consumer sees.

### Verified

`fmt --all --check` clean; `clippy --workspace --all-targets -D warnings` clean; **704 passing**
overall. New coverage: at the facade, three day-partitions with two segments each, asserting a bound
of one rewrites exactly one partition, loses no rows, leaves the rest intact, and drains over repeated
calls; through the endpoint, that the bound is reported back, `compaction_complete` distinguishes a
capped pass from a finished one, and zero is refused. Nothing committed.

## Coalescing, and housekeeping over MCP (2026-08-09)

Two additions to the queued endpoint, and one finding about the daemon that came out of a question
while doing them.

### Duplicate submissions join the queued job

A submission matching a job still **queued** answers `200` with that job's id and
`"coalesced": true`, rather than `202` and a second pass. The motivating case is a caller on a timer:
passes are serialized, so a pile-up is pure wait, and every pass after the first does nothing the one
before it did not.

The line is drawn at the queue, and that is the whole design:

- **A queued job has not looked at the database yet**, so it will see everything a submission arriving
  now wants covered. Joining it is exact, not approximate.
- **A running job snapshotted before this submission arrived** and may already be past the segments
  the caller cares about. Joining it would answer a request the pass cannot have covered — so a
  running job is deliberately not a match, and neither is a finished one.

Matching is an **exact parameter match** rather than a subsumption test. A queued `compact: true` pass
would in fact cover a new `compact: false` request, but that rule has to be explained every time
someone reads back a job id they did not expect, and the case that motivates coalescing is the *same*
request repeatedly. Predictable beats clever.

The search and the insert happen under one lock acquisition, because two submissions arriving together
must not both find nothing and both queue a pass — which is precisely the pile-up being prevented.

`coalesced` rides in the response rather than on `Job`: it describes *this submission*, not the job.

### MCP drives the same queue, not one of its own

`run_housekeeping` and `housekeeping_status` are the tools. The interesting part is placement: the
queue is server wiring, and `imbh-mcp` sits **below** `imbh-server` so the stdio transport can share
the tool surface. So the host hands its queue in — `imbh_mcp::Housekeeping`, a three-method sync trait,
implemented by `imbh-server`'s `AppState` — and `handle_with` takes it. `handle` keeps its old
signature and passes `None`.

That falls straight into the rule the surface already had: **a tool is offered only where it can
work.** `visible()` already dropped write tools on a read-only handle; it now also drops
queue-dependent tools when the host runs no queue. `imbh-tui --mcp-stdio` therefore advertises
neither, without a flag anyone has to set.

The alternative — MCP running housekeeping synchronously — was rejected for the reason the endpoint
exists: a pass costs the corpus, and a tool call that waits for one reintroduces exactly the timeout
problem, this time against a model's client rather than a proxy.

### The trap, twice

The MCP e2e test failed on the first run: `call_tool` builds a fresh `app(db)` per call, so each call
got a fresh queue and the second could not find the first's job. This is the same hazard already
documented on `imbh_server::route` — and it had been harmless for as long as the server was stateless.
Fixed by holding one `Router` and cloning it per call (clones share the state `Arc`). Worth the note:
**adding state to something previously stateless invalidates every helper that quietly rebuilds it**,
and the failure mode is a 404 rather than a compile error.

### Finding: `Db::maintain()` is never called by the running daemon

Asked in passing, and the answer is more interesting than expected. `imbhd` configures
`Maintenance::Background(interval)`, whose loop (`FlushScheduler::advance`) calls the *storage*
primitives directly — `seal()`, `sync_wal()`, `retain()`. It does **not** call `Db::maintain()`, and
in particular it never calls `commit_pending()`.

So before this endpoint, the only in-process triggers for `commit_pending` were `open()` and
`close()`. A long-running `imbhd` with an external `imbh-housekeeper` preparing rewrites would apply
them **only at restart** — the prepare/commit handoff of §7.2 works on a daemon that restarts and not
on one that stays up. The new endpoint is the first thing that closes that loop while running.

That is arguably a gap in the scheduler rather than a feature of the endpoint: `advance`'s retention
step could call `commit_pending()` alongside `retain()`. It changes engine behaviour for every
embedder on `Maintenance::Background`, so it is recorded here and left for a decision rather than
taken unilaterally.

### Verified

`fmt --all --check` clean; `clippy --workspace --all-targets -D warnings` clean; **709 passing**. New
coverage: coalescing at the registry (queued joins, running/finished/different-parameters do not, and
the search-and-insert is one critical section), coalescing over HTTP (a burst of identical submissions
where every one is accepted and far fewer passes exist), and over MCP the submit → poll → list round
trip against one router, an unknown id explaining itself, `max_jobs: 0` refused, and a host with no
queue advertising neither tool. Nothing committed.

## The background loop commits pending rewrites — and why it does not call `maintain()` (2026-08-09)

`FlushScheduler::advance` now calls `commit_pending()` on the maintenance interval, immediately before
`retain()`. That closes the gap the previous entry recorded: with only `open()`/`close()` pickup, a
long-running `imbhd` applied an external preparer's rewrites **at restart and at no other time**, so
the preparer kept re-preparing partitions that never landed — which is precisely the failure the
open/close pickup was added to prevent, displaced from "a host that never calls `maintain()`" to "a
host that never restarts".

### Why not just call `maintain()` from the loop

It was the obvious suggestion and it is wrong for one specific reason: **`maintain()` seals
unconditionally**, and deciding whether to seal is the loop's entire job. `FlushPolicy` exists to
answer that — by interval, buffer bytes, row count, WAL bytes, or idle time — and `manual` means "seal
only on `/admin/flush` and shutdown". A loop that called `maintain()` every tick would seal every
tick, producing a tiny segment per second and breaking the one policy whose contract is *not* to.

So the loop performs `maintain()`'s **order** rather than calling it: seal (policy-decided), commit,
retain. That is one ordering rule in two places, which is a real cost — but the alternative is either
breaking the flush policy or splitting `maintain()` into a sealless half whose only caller would be
this loop.

The seal/commit order turns out not to be load-bearing either way: sealing appends a segment and never
removes one, so it cannot invalidate a record, and a record's validation is against the input segments
and the promoted set, neither of which a seal touches. **Commit before retention is** load-bearing,
for the reason `maintain()` documents — retention drops segments, and a rewrite whose inputs it is
about to drop should land first. That is the constraint the placement honours.

### The test is the interesting part

Asserting "the loop did it" means asserting that *nobody else* did. The test prepares a rewrite from
outside against a live writer, then calls nothing at all — no `maintain()`, no `commit_pending()`, no
`flush()` — and waits for the segment count to drop. `FlushPolicy::manual()` is what makes it precise:
the loop's seal step can never fire, so the only thing that can change the manifest is the commit under
test.

### Verified

`fmt --all --check` clean; `clippy --workspace --all-targets -D warnings` clean; **710 passing**.
Nothing committed.

## Closing summary: the housekeeping surface, and what asking "what does it really do" was worth (2026-08-09)

Five entries above cover the second half of this session — the `imbhd` housekeeping work that grew out
of the attribute/promotion arc. This closes it.

### What shipped

| Surface | What it does |
|---|---|
| `POST /admin/housekeeping` | queue a pass; `202` + a job id, never the outcome |
| `GET /admin/housekeeping/<id>` · `GET /admin/housekeeping` | poll one job; list the retained ones |
| `{"compact": true, "max_jobs": N}` | compaction, bounded to N partitions |
| `run_housekeeping` · `housekeeping_status` | the same queue, over MCP |
| `Db::compact_bounded` / `Storage::compact_bounded` | the bound the endpoint needed underneath |
| the background maintenance loop | now commits prepared rewrites, before retention |

Tests: **704 → 711** over this half (668 at the session's start). 53 files changed overall,
+7,971/−1,675. Twenty MCP tools now, eighteen of them read-only.

### Findings

**A response status has to describe what happened, not what was asked.** The work has not run when a
submission is answered, so `202` and a *handle* — a `200` with a report would be a lie. The same rule
produced the `200` on a coalesced submission (nothing was created) and the `400` on a read-only
handle (nothing could be). Each time the status fell out of the truth rather than out of a convention.

**Capability follows the handle.** Fourth and fifth instances this session: the housekeeping endpoint
refuses a read-only handle, and MCP hides queue-dependent tools from a host that runs no queue. Both
are structural facts — a reader holds no writer lock; a host without a queue has nothing for the tool
to call — rather than policy someone must remember to configure. The corollary is the error message:
refuse up front and say *what would work*, never surface the layer below's complaint.

**Bound the unit the work is actually done in.** `max_jobs` counts **partitions**, not segments,
because a partition is what compaction reads, sorts and writes as one. A segment bound would have to
stop *inside* a partition, and a half-merged partition is not a state the manifest can describe. That
also made "capped" mean *slice*, not *partial*: everything past the bound is untouched, so there is
nothing to resume and resuming is submitting again.

**Coalescing's whole design is where the line is drawn.** A **queued** job has not looked at the
database, so it will cover a request arriving now; a **running** one snapshotted before that request
existed and may already be past what it wants. So queued coalesces and running does not — and matching
is an exact parameter match rather than a subsumption test, because a rule that has to be explained
every time someone reads back an unexpected job id is a worse rule than one they can predict.

**One ordering rule in two places beat the alternatives.** The maintenance loop performs `maintain()`'s
order — seal, commit, retain — rather than calling it, because `maintain()` seals *unconditionally*
and deciding whether to seal is the loop's entire job (`FlushPolicy::manual` means "seal only on
`/admin/flush` and shutdown"). Duplication is a real cost; breaking the flush policy, or splitting
`maintain()` into a sealless half whose only caller is this loop, were worse.

**Adding state invalidates every helper that quietly rebuilt it.** `imbh_server::route` and the MCP
test's `call_tool` both construct a fresh router per call. Harmless for as long as the server was
stateless; the moment a queue lived in router state, a job submitted through one call could not be
polled through the next — and the failure mode is a `404`, not a compile error. Documented on `route`,
and the tests now hold one router.

### The finding that came from a question, not from code

Two exchanges did more than any refactor here.

**"I should've asked what `commit_pending()` really does."** It does no rewriting: it validates a
record (inputs still present, promote set unchanged, output digest matches) and swaps an
already-produced file into the manifest. The rewrite — including the projection of promoted columns —
happens in `compact_partition`/`backfill_promoted`, in-process or in a preparer. Which means moving
the commit earlier in the loop would have changed **nothing** for a daemon with no preparer running:
`commit_pending` finds an empty directory forever. A scheduling change aimed at the wrong mechanism.

**"We should want to see the attributes promoted in the next seal."** I believed it already worked and
could have said so. Writing the test instead was the right call and it passed first run — a segment
sealed after `set_promote` carries the column, the one before it does not, and both answer the same
query through the JSON fallback. The belief was right; the *evidence* is now a regression guard that
did not exist, since the existing promotion tests covered the barrier and the compaction projection but
not the seal path.

The transferable form: **"X should happen sooner" is answerable only once you know which mechanism
produces X.** Promotion reaches new rows through append-time encoding (immediate, at the next seal) and
reaches old segments through a rewrite (needs compaction or a preparer). Those are different clocks,
and the fix for one does nothing for the other.

### Open

Nothing committed. Still unbuilt, and recorded rather than guessed at: a **queue-depth** bound
(coalescing removes the common pile-up, but nothing stops a client submitting a thousand *differing*
requests), and convergence of already-sealed segments at seal time rather than only in a rewrite pass —
which would turn a bounded buffer write into an unbounded one, so it needs a decision rather than an
implementation. `cargo clippy -p imbh-storage --no-default-features --all-targets` remains broken on a
test module this work did not touch (pre-existing; every *library* configuration is clean).

## Preparing v0.8.0: the first bump the signatures justify, and a shipped binary that was over budget for two releases (2026-08-09)

Bump the shared workspace version and close the changelog. One PR since v0.7.0 (#45, the segment
housekeeping + attribute statistics work) plus a docs commit, but it is the largest release the
project has cut: 70 files, +17,399 lines, two new workspace members.

### The changelog was complete, and that is the notable part

v0.7.0's entry recorded that PR #43 shipped a user-facing feature with no changelog entry, caught by
diffing `v0.6.2..HEAD --stat` against the sections present rather than by reading the changelog. The
same check was run here (`v0.7.0..HEAD`, 70 files) and found nothing missing — every user-visible
change in the diff has an entry, including the ones that landed as follow-up commits inside the PR.
Worth recording as the counter-example: the check is cheap and it is the only thing that can tell you
what is *absent* from a document.

What it did have was a structural defect the stamp would have frozen. `[Unreleased]` had accumulated
entries at both ends over the PR's life, so the closed section read
`### Fixed` / `### Added` / `### Changed` / `### Fixed` — two `Fixed` headings in one release. Merged
into the Keep a Changelog order (Added / Changed / Fixed), with the maintenance-loop fix moved down
beside the other two. Nothing was reworded; only the headings moved.

### Why a minor bump — and why the reasoning is the opposite of last time

v0.7.0 needed an argument: the public surface was purely additive and a signature audit alone would
have said 0.6.3, so the minor was chosen for what the *number communicates* about a stored-data
change semver has no vocabulary for. v0.8.0 needs no such argument. The release breaks the published
API in five distinct places, all already written up under `### Changed`:

- the promoted key set and the retention policy both became **durable database state**, so omitting
  `DbBuilder::promote` / `DbBuilder::retention` now *inherits* where it used to reset;
- `Db::segment_files` returns `Result<Vec<PathBuf>>`;
- `CompactionReport` gained `segments_converged`;
- `imbh-query`'s `SegmentInput` / `TableInput` gained public fields and
  `SegmentTableProvider::new` takes another argument.

Under the 0.x rule the last five releases have used — a minor means something broke — 0.8.0 is what
the diff computes, not a judgement call.

### `imbh-attrstats` is a first-time publish

The workspace goes from 20 packaged members to 22: `imbh-attrstats` (published) and the
`attr-stats` example (`publish = false` + `[package.metadata.release] release = false`, so it stays
out of cargo-release's plan). The crates.io name is **free** — checked against the API rather than
assumed — and only two things depend on it: the `imbh` facade, optionally, behind its off-by-default
`attrstats` feature, and the unpublished example. So it needs a publish slot after
`imbh-core`/`imbh-storage` and before `imbh`. The crate-count gate does not move (275, unchanged),
because an off-by-default feature is not in the graph `cargo tree -p imbh` measures.

### The finding: the shipped x86_64 `imbhd` has been over the §2 target since v0.6.0

TODO.md has carried a projection since 2026-08-06 — the `docker-remap` VRL subtree would push the
x86_64-linux `imbhd` to ≈45.1 MB against a 42 MB target — flagged as needing a CD dry run to confirm.
No dry run was needed: three releases have shipped since, so the bytes exist. Downloading
`imbh-0.7.0-x86_64-unknown-linux-gnu.tar.gz` and unpacking it gives an `imbhd` of **45,691,560 B =
45.7 MB**, i.e. **3.7 MB over target** and comfortably under the 55 MB hard limit.

Against CD's v0.5.0 x86_64 baseline of 41,112,104 B that is **+4,579,456 B (+4.37 MiB)** for
`docker-remap`, versus the **+4,024,312 B (+3.84 MiB)** measured locally on aarch64. The item's own
caveat — "the projection is conservative in the wrong direction, x86_64 codegen is demonstrably
fatter" — held exactly: the real overage is larger than the projected one.

Two things follow. It is a **standing overage, not a v0.8.0 regression** — it first shipped in v0.6.0,
the first release whose Linux legs carried VRL — so it does not block this release. And the local
footprint gate structurally cannot see it: the same binary is ~5 MB smaller on aarch64, where the gate
reads 33.5 MiB and passes. The decision (raise the target, trim VRL, or split `docker-remap` into its
own artifact) is now unblocked, with a measurement under it instead of an estimate.

### The binary moved this time

`imbhd` 34,916,248 → **35,112,856 B (+196,608 B)**, ending the two-release streak of byte-identical
output that both previous entries had to disprove as a stale-artifact bug. The plugin feature set
moved by all but sixteen bytes of the same amount: 40,128,880 → **40,325,504 B (+196,624 B)**. Two
binaries with different feature sets, one release's worth of code, and near-identical deltas — the
added code is almost entirely in the part both builds share, which is what a storage/query release
should look like.

Steady RSS improved: 104.9 → **93.8 MB** on the rss-probe's 20k-record loop (idle 14.9 → 15.0 MB,
noise). Not investigated; the attribute work touched the JSON reader's allocation behaviour
(`json_get` no longer builds a `Vec` per row), which is the plausible source.

### A published crate whose trimmed test build did not compile

The previous session flagged, as pre-existing and out of scope, that
`cargo clippy -p imbh-storage --no-default-features --all-targets` was broken. It is a published
crate and this is a release, so it was fixed rather than carried: the unit tests referenced
`Storage::compact`, `read_parquet_file`, `coerce_to_schema` and `reconcile_segments`, all of which the
*library* already gates behind the new `compaction` feature, while the tests did not. Four tests gated
on `compaction`, one already gated on `search` widened to `all(search, compaction)`, and the
`ParquetRecordBatchReaderBuilder` import widened to `any(feature = "compaction", test)` — the bloom-filter
test reads a segment back to assert on what *seal* wrote, which has nothing to do with whether
rewriting is compiled in. Test-only; no library code changed, and no changelog entry, since nothing a
consumer links behaves differently.

All six combinations now lint clean under `-D warnings`: `--no-default-features`, `+compaction`,
`+search`, `+search,compaction`, default, `--all-features`. Only the default configuration was ever
covered before, which is why a feature added mid-cycle could break the others unnoticed.

### Still by hand, for the sixth time

`README.md` (3 strings) and `docs/DOCKER_LOG_DRIVER.md` (2) moved to 0.8.0 by hand again, for the two
reasons TODO.md has recorded since v0.5.0 — the `pre-release-replacements` for them sit under
`[workspace.metadata.release]` where cargo-release does not read them, and the `pre-release-hook`
would still run `git cliff -o CHANGELOG.md` with no `cliff.toml` in the repo, replacing this
hand-written file with a commit digest. Deliberately **not** fixed here: changing the release tooling
in the same change that prepares a release means the first thing exercising the fix is the release
itself. The item's count goes from five to six.

`THIRD-PARTY-NOTICES.txt` is regenerated; its only delta is the workspace crates moving to 0.8.0 and
`imbh-attrstats` joining them — 14 entries to 15, and Apache-2.0's count 343 → 344. **No third-party
crate is added or removed**, which independently confirms the release's claim that `imbh-attrstats`
adds no dependency subtree of its own.

`QUALITY_GATE.md` §2's binary-size line and its applicability blockquote are refreshed. The latter
still claimed **159 tests** in the default `--workspace` path, a number from around M6 that survived
five releases; it is 711 across 76 suites now.

### Verified

`fmt --all --check` clean; `build --workspace`, `clippy --workspace --all-targets -D warnings` and
`test --workspace` all clean (**76 suites, 711 passed, 0 failed, 4 ignored** — up from v0.7.0's
64 / 615). Crash-injection E2E (`-p imbh --features fault-injection --test crash_points`) passes, run
because this release rewrites the seal/pending/recovery paths. `./scripts/license-gate.sh` OK.
Footprint gate **OK**: 275 crates (target 275), `imbhd` 33.5 MiB, plugin feature set 398 crates /
38.5 MiB (informational), idle RSS 15.0 MB, steady RSS 93.8 MB, search-off lever 275 → 218 → 76 —
all three lever rows unchanged from v0.7.0. §3b notices regenerated. §3c packaging dry-run staged and
verified **all 22 members at 0.8.0**, exit 0, with `--allow-dirty` because the bump is uncommitted
(the real release runs it on a committed tree, so the flag changes nothing it verifies). §4 local
half: `cargo build --release -p imbh-server --features docker,grpc,tracing` OK and
`./scripts/build-image.sh` builds an image whose `imbhd` passes the documented "nothing to serve"
smoke test.

One §4 gotcha worth recording, since it looks like a release defect and is not: the locally built
image's `imbh-tui` dies with ``GLIBC_2.39' not found``. `build-image.sh` compiles on the host and
copies the result into `debian:bookworm-slim` (glibc 2.36), so a host newer than bookworm produces an
image that cannot run its own binaries. CD is unaffected — its Linux legs build on `ubuntu-22.04`
(glibc 2.35) precisely to stay under the base image's floor. The local script validates the
**context layout and the Dockerfile**, not the binaries' portability.

Nothing committed, tagged or published.

## The TUI's metrics and traces screens: three scans that never pruned, and a renderer that drew the whole trace (2026-08-23)

The report was "the metrics and traces screens get awfully slow once datapoints and spans accumulate."
The instinct behind it — "it almost looks like full scans always happen" — was right, and a benchmark
built before any change made it precise.

`examples/bench/src/bin/tui-bench.rs` drives `imbh_head::exec` rather than `Db`, because a screen
refresh is not one statement: it is a catalog read plus a translation plus a query, or a candidate
search plus a fetch per candidate, and the wrapper costs were the whole story. `--sweep` grows the
corpus while holding the **query window fixed at two segments**, which is what separates a
corpus-driven cost from a window-driven one without needing an argument about it. Scan counters come
from `stream_with_stats`, so "the catalog pruned nothing" is `segments_pruned == 0`, not an inference
from the clock.

### Baseline, and what it showed

Every column grew roughly linearly with corpus size at a fixed window (12 metrics, 40 traces/segment,
best of 3, ms):

| segments | rows | catalog | promql x1 | promql x6 | tq-search | traceql | trace-get |
|---:|---:|---:|---:|---:|---:|---:|---:|
| 10 | 10,400 | 2.80 | 7.58 | 44.57 | 4.23 | 73.90 | 0.87 |
| 640 | 665,600 | 70.34 | 157.57 | 949.14 | 134.22 | 1612.96 | 19.09 |

One metrics refresh with six metrics checked was ~950 ms; one traces refresh ~1.6 s.

**Five causes, three of which were visible in the source and two of which only the sweep found.**

1. **`MetricsApi::catalog` is a whole-corpus scan run once per metric per refresh.** Five
   `SELECT DISTINCT … FROM metrics_*` with no time predicate (nothing prunes, confirmed by
   `pruned=0`), and `exec::promql` reads the catalog per *request* to resolve each selector's kind
   while `imbh-tui` sent one request per checked metric. Six metrics meant thirty unbounded scans.
2. **TraceQL re-fetched every trace the candidate search had already fetched.** `TracesApi::search`
   phase 2 pulls all spans of the top-N candidates; `fetch_candidates` kept only the ids, and
   `execute_traceql` then issued `traces().get(id)` per candidate — a fresh `SessionContext` and a
   full query each. ~101 queries per refresh.
3. **No column projection reaches the Parquet reader** — the partition yields full-schema batches.
   Still true; see the follow-ups.
4. **(sweep-only) `trace_start_range` compiles to a `HAVING`, so the candidate search cannot prune.**
   A `HAVING` runs after the aggregate, so `FROM spans` read every span segment however narrow the
   window; `tq-search` grew 32x across a 64x corpus at a fixed window.
5. **(sweep-only) `traces().get()` has no time predicate**, so the raw-bytes bloom probe can only be
   answered by opening every segment's Parquet footer — 640 x ~35 us ≈ 22 ms, against 19.09 measured.

The negative finding mattered as much: **time-range pruning already worked.**
`manifest_range_excludes` skips a segment with no file opened, and the typed builders emit the
`CAST("time" AS BIGINT)` shape `stats_range_probe` recognizes. So the raw-point scan was already
bounded by the window, and **rollups were not needed** — which is why none were built.

### What changed

**The catalog is folded, not rescanned** (`MetricCatalogCache` in `crates/imbh/src/lib.rs`). A
distinct-union over an append-only segment set is monotone, so a segment's contribution is fixed once
written: new segments are scanned and unioned in, and only *removal* (retention, compaction) forces a
rebuild. The mutable buffer is deliberately excluded and rescanned every call — a metric ingested
seconds ago lives only there, and caching it would hide a new metric until the next seal, which is a
correctness bug wearing a performance costume. `Db::query_tables` was extracted so the sealed and
unsealed halves can be queried as separate narrowed table sets.

**TraceQL fetches in chunks** (`TraceSource::fetch_traces`, `TRACE_FETCH_CHUNK = 32`). The trait
method has a default body that keeps the old per-trace loop, so an external implementor is not
broken; `TracesApi` overrides it with `get_many`, one `trace_id IN (…)` scan per chunk. Chunking
rather than one big query preserves the evaluator's "peak memory is one trace" property in spirit —
resident traces stay capped at the chunk while per-query overhead falls by the chunk factor. The
contract requires preserving the requested id order, because the candidate list is ranked by recency
and that ranking is what the user sees; `get_many` groups by id, so the impl restores it.

**The candidate search prunes, and stays exact.** A `WHERE start_time BETWEEN lo AND hi` was added
alongside the `HAVING`. It is a sound *superset*: a trace that started in the window has its first
span in the window, so no qualifying trace loses its last row before the `GROUP BY`. It is only a
superset because a trace that started earlier and was still running also survives, and its *filtered*
`min(start_time)` then lands inside the window — so the exact test moved to after phase 2, which
refetches every span of each candidate unbounded by time and therefore knows the true start. Phase 1
over-fetches (`2n + 16`) so those false positives come out of slack rather than out of the answer.
`a_trace_that_merely_overlaps_the_window_is_not_a_match` pins it, and was checked against a build with
the recheck disabled to confirm it is not vacuous.

**A batched evaluation is attributable** (`dto::Series::query_index`). This is what let the TUI
collapse its serial per-metric loop into one request. The field also had to reach the Arrow IPC
codec — `META_SERIES_QUERIES`, parallel to the existing `META_SERIES_STARTS` — or remote mode would
have mislabelled a multi-metric selection silently, and only remotely, since the local path hands the
value over untouched. Absent metadata decodes as all-zero, i.e. the single-query reading.

**Rendering is bounded by the viewport, not by the data.** `render_waterfall_window` replaces
`render_waterfall` at both call sites: the preview pane rendered every span of the trace into a
`Vec<String>` and joined it to display ~10 lines, and the detail view did the same for a scrolling
list. The preview also dropped `Wrap`, which could not change what is drawn (rows are pre-fitted to
the pane) but re-measured every line. `TableData` now measures its column widths once at construction
instead of per frame — `draw` takes `&App`, which is why the renderer had nowhere to keep them. And
the per-point `chart_point_cell` projection is gated on `show_mascot`, its only consumer.

### Result

| segments | rows | cat-api | promql x1 | prom x6/n | prom x6/1 | tq-search | traceql | trace-get |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 10 | 10,400 | 1.05 | 4.73 | 28.05 | 22.41 | 3.21 | 6.45 | 0.87 |
| 640 | 665,600 | 2.77 | 9.37 | 55.51 | 41.28 | 38.85 | 46.25 | 19.39 |

At 640 segments: the catalog read went 70.34 → 2.77 ms and is now **flat** rather than linear
(1.05 → 2.77 across a 64x corpus); one metrics refresh 157.57 → 9.37 ms (**17x**); one traces refresh
1612.96 → 46.25 ms (**35x**). The `cat-sql` control column still shows the unmitigated 70 ms, which is
what keeps the comparison honest.

### Verified

`fmt --all --check` clean; `build --workspace`, `clippy --workspace --all-targets -D warnings`, and
`test --workspace` clean — **718 passed, 0 failed** (up from 711), including six new tests: four on
the catalog cache (buffer freshness, accumulation across seals, stability, rebuild on removal), the
overlap-window trace boundary, the windowed-render equivalence, and the `query_index` IPC round trip.
Footprint gate **OK** and unchanged: 275 crates, `imbhd` 33.5 MiB, idle RSS 15.0 MB, steady 105.1 MB,
search-off lever 275 → 218 → 76.

Nothing committed, tagged, or published. **`dto::Series` gained a public field, so this needs a
0.8 → 0.9 bump before publishing** — left to `cargo release`, which owns version numbers here
(`shared-version`, `dependent-version = "upgrade"`, and the README/docs replacements) and which a
by-hand edit would only half-apply.
## 2026-08-24 — Input lock and loading banner for the TUI

### What prompted it

Slow refreshes on a large corpus do not just take time; they make the UI feel broken. Keys pressed
during the wait queue up and replay against a snapshot that has since been replaced, and nothing on
screen says why. The only existing affordance was the menu bar turning `DarkGray`, which nobody
reads as "your input is being ignored".

### What changed (imbh-tui only, no manifest changes)

* **`Refresh::{Interactive, Background}`** (`model.rs`) now travels with every refresh.
  `request_refresh` stays the short name for the ~19 user-initiated call sites; the auto-refresh
  timer in `runtime.rs` is the only caller of `request_refresh_as(Refresh::Background, ..)`.
  `pending_refresh` became `Option<Refresh>` so a coalesced refresh remembers its origin, and an
  interactive request queued behind a timer tick wins the slot.
* **The input lock** (`keys.rs::survives_loading`). An interactive load holds the keyboard until it
  lands; `Mode::Normal` only, because that is the mode where every binding is an operation against
  the snapshot on screen. The overlay modes (`Editing`, `TimeRange`, `AbsoluteRange`, `Menu`) stay
  live — eating keystrokes out of a half-typed query would be worse than anything the lock prevents,
  and their commit paths already coalesce. `q` is never refused: there is no `Ctrl-C` binding, so it
  is the only way out of a query that never lands.
* **The banner** (`overlays.rs::draw_loading_banner`), raised after `LOADING_BANNER_AFTER` (2s), or
  immediately once a key has actually been refused — a swallowed key with no visible cause reads as
  a hang, and that explanation cannot wait for a timer. Its text is spinner, word, elapsed and
  nothing else, identical whether or not the lock is on. Two drafts spelled the lock out
  (`input paused {sep} q quits`, then `keys paused except q`) and both were cut as redundant: the
  first contradicted itself (if input is paused, why does `q` work?), and the second still repeated
  the footer, which lists `q quit` on every frame. A spinner beside the word "Loading" is what
  "wait" looks like; the banner *being there* is the signal, and words about the keyboard only
  restate what the bottom of the screen already says. Centred on the
  screen: it was first docked
  under the menu bar to avoid covering content, but that put it half in the chrome and punched a box
  through the pane's top border, and the user asked for the centre. The content behind it is by
  definition about to be replaced by the result being waited on. Spinner frames are derived from
  elapsed time rather than a counter, so the whole banner is a pure function of state the loop
  already holds.

* **The footer carries the lock signal instead** (`ui/mod.rs`). While `input_locked` is set, every
  key-legend entry after `q quit` is greyed to `DarkGray`. That is the whole indication that input is
  held: it states which key survives without adding a word anywhere, and it cannot go stale against
  the legend because the split point is the `QUIT_HINT` const the legend itself is formatted from.
  Asserted on rendered cell styles rather than on text, since there is no text to assert on.
* **Wake scheduling** (`runtime.rs`): while loading, the loop wakes exactly when the banner becomes
  due and then every `SPINNER_FRAME` (120 ms). Without it the 1s clock tick would show the banner up
  to a second late and animate it one frame per second, which reads as frozen. Both bounds stay
  capped by `until_refresh`, so nothing here delays a refresh.

### Why background refreshes are exempt

The trap this avoids: auto-refresh fires on its own schedule, and locking the keyboard for the
duration of every tick would hold the UI for most of a session on exactly the slow corpus the guard
is for. The user did not ask for that query, so it does not get their keyboard. It still raises the
banner once it drags — worth announcing, just not worth blocking for.

### Verified

`fmt --all --check` clean; `clippy --workspace --all-targets -D warnings` clean; `test --workspace`
**724 passed, 0 failed** (up from 711; `imbh-tui` 160 → 173). Both new behaviours were checked
against a vacuous pass by disabling them (`if false && ..` on the banner call and the gate) and
confirming `a_user_initiated_query_refuses_operations_until_it_lands` and
`a_long_wait_puts_the_banner_in_the_middle_of_the_screen` FAIL, then restoring. The
placement assertions were separately checked by shifting the box off-centre on each axis and
confirming both fail. The banner is also
folded into the existing `ascii_mode_renders_only_ascii_across_the_ui` sweep, so `--ascii` keeps its
no-Unicode guarantee. Footprint: no manifest changed and `imbhd` does not link `imbh-tui`, so both
gated axes are untouched by construction; crate count re-measured at **275** to confirm.

Nothing committed beyond the topic branch.

## Preparing v0.9.0: a small release with two breaking signatures, and a changelog that had gone unwritten (2026-08-24)

Three PRs since v0.8.0: #49 (the metrics/traces scan fixes), #50 (the CI disk fix), #51 (the TUI
input lock and loading banner). 34 files, +2,372/-260 — the smallest release since v0.6.1, and the
first in a while whose whole diff is one crate family.

### What the version has to be

**0.9.0, and the signatures leave no choice.** Two breaking changes, both in `[0.9.0]` `### Changed`:

* `imbh-head`'s `dto::Series` gained a public `query_index` field and became `#[non_exhaustive]`.
  The field is what makes a batched `EvalRequest` usable: `EvalRequest` has always taken a *list* of
  queries, but a PromQL aggregation drops `__name__`, so the concatenated response was
  unattributable and every caller sent one query per request anyway. Naming the sub-query per series
  collapses a six-metric TUI refresh from six HTTP round trips to one.
* `imbh-lgtm`'s `execute_traceql` gained an `S: Sync` bound, required by
  `TraceSource::fetch_traces`'s default body — it holds `&self` across an await, and that future is
  `Send` only if `Self` is `Sync`. Every real implementor already satisfies it.

### The changelog had to be written, not just closed

Unlike v0.8.0, where "the changelog needed no reconstruction", `## [Unreleased]` was **empty**.
Neither #49 nor #51 wrote an entry as it landed, despite the file's own instruction to. So the
`[0.9.0]` section was reconstructed from the two PR diffs rather than closed over existing prose.

Worth naming as a process defect rather than a one-off: the CHANGELOG says "write new entries under
`## [Unreleased]` as you go", and nothing enforces it. v0.7.0 shipped a feature silently for the same
reason (PR #43). The reconstruction here was cheap because the release is three PRs old and both
were freshly in hand; at v0.8.0's scale it would not have been.

One claim in `TODO.md` did not survive checking: it said "the response DTOs now carry
`#[non_exhaustive]`". Only `Series` does. The rest of `imbh-head`'s response structs are still
exhaustive, so the next field added to any of them is another breaking change — corrected in place,
and left as an open item.

### Verified

`fmt --all --check` clean; `clippy --workspace --all-targets -D warnings` clean; `test --workspace`
**78 suites, 732 passed, 0 failed** (up from v0.8.0's 76 / 711). §3a `./scripts/license-gate.sh` OK.
§3b notices regenerated — the diff is 15 version strings and nothing else, which is the expected
shape for a release that changed no dependency. §3c packaging dry-run staged and verified **all 22
members at 0.9.0**, exit 0, with `--allow-dirty` because the bump is uncommitted. Footprint gate
**OK** and unchanged from v0.8.0: 275 crates (target 275), `imbhd` 33.5 MiB, plugin feature set 398
crates / 38.5 MiB (informational), idle RSS 15.0 MB, steady RSS 104.9 MB, search-off lever
275 → 218 → 76.

No new workspace members and no dependency change, so the publish order is exactly v0.8.0's.

### Not done here

Nothing tagged, published or pushed. Note also that `[workspace.metadata.release]` still carries
`pre-release-hook = ["git", "cliff", "-o", "CHANGELOG.md", "--tag", "{{version}}"]` while the repo
has no `cliff.toml` and the changelog is hand-written prose — running `cargo release` end-to-end
would regenerate `CHANGELOG.md` from conventional commits and destroy it, and the
`exactly = 1` replacements would then fail against the regenerated file. Every release since v0.3.0
has been a hand-made bump commit instead, which is why this has never fired. Left alone rather than
fixed blind, but it should either be given a `cliff.toml` that preserves the file or be dropped.

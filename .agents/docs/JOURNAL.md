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

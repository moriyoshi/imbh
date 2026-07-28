# Project To-Dos

Items extracted from JOURNAL.md during `good-sleep` consolidation, plus open follow-ups. Each
item should be resolved or removed once addressed. Design-level open *questions* (as opposed to
actionable work) live in `ARCHITECTURE.md` §15, not here.

Completed items are swept periodically (their durable knowledge lives in `.agents/docs/LTM/` and
git history); this file tracks only what is still open.

## Open Items

- [ ] **Optional upstream differential runner.** Automate the versioned in-process fixture corpus
      against pinned Prometheus/Loki/Tempo daemons behind an opt-in test or script. Default
      workspace tests must remain daemon-free and offline. Deferred by explicit user request. —
      *source: JOURNAL (LGTM differential-testing follow-up)*

- [ ] **Dependabot for the SHA-pinned GitHub Actions.** All five actions in
      `.github/workflows/{ci,release,soak}.yml` are now pinned to commit SHAs, so patch/security
      updates no longer arrive on their own. Add `.github/dependabot.yml` with a
      `package-ecosystem: github-actions` entry so the pins are refreshed by PR. Offered to the
      user, not yet added. — *source: JOURNAL (Actions SHA-pinning, 2026-07-24)*

- [ ] **Windows portability beyond the directory fsync (issue #3 follow-up).** The `windows-latest`
      job added to `ci.yml` has never run — it was written without a Windows host to verify against
      (cross-compiling locally is blocked by `zstd-sys` needing mingw). It may surface further
      Windows-specific issues; deletion/rename of open or memory-mapped files during compaction and
      retention are the plausible next candidates. Watch the first run and fix what it finds. —
      *source: JOURNAL (issue #3, 2026-07-28)*

- [ ] **Release carrying the Windows fix.** `imbh-storage` 0.1.0 on crates.io cannot open an on-disk
      DB on Windows at all. The fix and the shared-version bump to **0.1.1** are on
      `fix/windows-dir-fsync` (PR #4), with the changelog entry staged under `## [Unreleased]`.
      Because the tree already carries 0.1.1, the release run is `cargo release` with **no** level
      argument (`cargo release patch` would bump again, to 0.1.2). Cutting it is the user's call
      (see `README.md` "Releasing"). — *source: JOURNAL (issue #3, 2026-07-28)*

## Recently Swept (2026-07-24 good-sleep)

Six items completed on 2026-07-24 were removed from the open list; their durable knowledge is in
LTM:

| Item | Where the knowledge lives now |
|------|-------------------------------|
| PromQL lookback lower-bound fidelity | `LTM/imbh-lgtm-languages-and-arrow-reads.md` |
| TraceQL negated-matcher-on-missing-attribute | `LTM/imbh-lgtm-languages-and-arrow-reads.md` |
| `imbh-lgtm` examples lack `required-features = ["source"]` | `LTM/imbh-lgtm-languages-and-arrow-reads.md` |
| Async `queued` receipt carried `lsn = Lsn(0)` | `LTM/storage-engine.md` (`Lsn` is `NonZero<u64>`) |
| TUI terminal teardown gaps | `LTM/imbh-tui-and-gen-demo-db.md` |
| `query_batches` duplicate definition under `--all-features` | `LTM/query-engine-and-typed-apis.md` |
| Git staging split before the first commit (resolved; history has since been amended) | git history |

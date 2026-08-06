# Project To-Dos

Items extracted from JOURNAL.md during `good-sleep` consolidation, plus open follow-ups. Each
item should be resolved or removed once addressed. Design-level open *questions* (as opposed to
actionable work) live in `ARCHITECTURE.md` §15, not here.

Completed items are swept periodically (their durable knowledge lives in `.agents/docs/LTM/` and
git history); this file tracks only what is still open.

## Open Items

- [ ] **The published 0.5.0 plugin cannot be installed as its own docs describe — the fix is in
      `[Unreleased]` and needs a release to reach anyone.** `config.json` now uses `propagatedMount`
      instead of a `data` bind mount (JOURNAL 2026-08-06), because a bind source the daemon will not
      create made `docker plugin enable` fail on any host without a pre-existing `/var/lib/imbh`.
      Until a release is cut, `ghcr.io/moriyoshi/imbh-log-driver:0.5.0-*` still carries the broken
      config, and the fixed `docs/DOCKER_LOG_DRIVER.md` in the tree **does not match the plugin those
      tags install** — anyone following main's install steps against the 0.5.0 tag hits the original
      error. Worth considering: a note on the released docs, or prioritising the release. Breaking
      (the database relocates and `plugin rm` now destroys it), so it is a minor bump under the 0.x
      rule, not a patch. Releases are cut only when explicitly asked.

      *(2026-08-06: **v0.6.0 is prepared but not cut.** The workspace is bumped to `0.6.0`, the
      changelog section is closed and dated, notices are regenerated and every gate is green — see
      JOURNAL "Preparing v0.6.0". Still open because nothing is committed, tagged or published, so
      `ghcr.io/moriyoshi/imbh-log-driver:0.5.0-*` still carries the broken config. Close this once
      the `v0.6.0` tag is pushed and the plugin job has published its tags.)*

- [ ] **Confirm `propagatedMount` survives `docker plugin upgrade`.** Persistence across
      `disable`/`enable` and destruction by `plugin rm` were both measured (JOURNAL 2026-08-06);
      `upgrade` was not, because it needs a registry round trip. It decides whether upgrading the
      plugin is a data-preserving operation, which the docs currently do not claim either way.

- [ ] **Nothing exercises the plugin against a real daemon.** `docker_plugin_config.rs` now guards the
      shipped `config.json` statically, but the packaging path (`build.sh` → `plugin create` →
      `enable` → a container logging through it) is still only ever run by hand. This class of bug —
      valid config, valid binary, fails at `enable` — is invisible to every existing test. An opt-in
      test gated like the RSS soak, or a CI leg on a Linux runner with a daemon, would catch it.

- [x] **The head API needs a semver bump before release (`imbh-head` is a new published crate).**
      *(closed 2026-08-06 — stale.)* Shipped: the workspace is at `0.5.0`, `v0.4.0` and `v0.5.0` are
      tagged and released, and `imbh-head` is a workspace member inheriting `version.workspace` /
      `publish.workspace` as the item predicted. Original text below for the record.

      The
      TUI-as-a-head work (ARCHITECTURE.md §10.19, JOURNAL 2026-08-01) added a 15th shipping crate,
      `imbh-head`, and made one **breaking** change to `imbh-tui`'s published surface:
      `cli::Mode::Tui { path: PathBuf, .. }` is now `cli::Mode::Tui { source: Source, .. }`, since the
      explorer takes `--url` as well as a directory. `imbh_tui::run` is *not* breaking — it now takes
      `impl Into<Backend>` and `From<Arc<Db>>` keeps `run(db, options)` compiling. Under the 0.x rule
      that is a `0.4.0`, not a patch. `cargo release` also has to learn the new crate (it inherits
      `version.workspace` and `publish = true`, so it should be picked up automatically — worth
      confirming on the dry run). Not done here: releases are cut only when explicitly asked.

- [x] **`GET /stats` still cannot be parsed back into a typed value, and omits the ingest gauges.**
      *(closed 2026-08-06.)* Resolved by **converging on one serializer** rather than widening the
      hand-written writer: `imbh_mcp::stats_json` is now
      `serde_json::to_string(&imbh_head::dto::Stats::from(stats))`, and `imbh_head::exec::stats()`
      defers to the same `From<&imbh::DbStats>` conversion instead of a hand-rolled mapping. Two
      corrections to the item's premise: there are **four** gauges, not three (`ingest_rejected`
      arrived in 0.5.0 with the duplicate-timestamp policy), and the `db_stats` call site parsed into
      `serde_json::Value`, so it constrained nothing beyond "valid JSON" — the real constraint was
      `dto::Stats`. Two **breaking** spelling changes, both in `CHANGELOG.md` under `[Unreleased]`:
      a `None` durable LSN is now `null` rather than `0` (the change the item flagged, approved by
      the user), and — forced by the convergence — `dto::Stats` / `dto::TableStats` dropped
      `skip_serializing_if`, so their `None` optionals serialize as explicit `null` instead of being
      omitted, which changes what a `GET /api/head/stats` consumer sees. Both are
      deserialization-compatible in each direction via `#[serde(default)]`. `/stats` key order also
      changed (`tables` moved first); no field was removed or renamed. Cost: `imbh-mcp` gained an
      `imbh-head` dependency (`dto` feature only), +1 workspace crate and +0 third-party. Covered by
      three `imbh-mcp` render tests, one `imbh-head` dto test, and widened assertions in
      `imbh-server`'s `health_ingest_query`.

- [x] **Decide whether to restore the `v0.3.0` git tag on the remote.** *(closed 2026-08-06 — won't
      do.)* Decided by the user: **this repository's releases are immutable, so there is nothing
      further to be done here.** The tag name stays permanently reserved by the deleted Release, which
      is precisely why re-pushing could only ever produce a red run; 0.3.0 remains traceable through
      the `CHANGELOG.md` commit link, and crates.io and GHCR are both published (`ghcr.io/moriyoshi/imbh`
      still carries the `0.3.0` and `0.3` tags). Not a deferral — the option the item was weighing does
      not exist. Original text below.

      It was deleted while trying to
      retry the failed CD run, and only the local signed tag at `07b72dd` survives, so nothing on the
      remote marks the commit that produced crates.io 0.3.0 (`CHANGELOG.md` links the commit instead).
      Re-pushing the tag is allowed by the `Version tags` ruleset (signed, no deletion involved), but
      it re-triggers `release.yml`: the `meta` preflight passes (no Release exists for the tag), the
      five build legs run for ~40 minutes, and `publish` then fails at `gh release create --draft`
      because the tag name is permanently reserved by the immutable Release that was deleted. So the
      choice is "one deliberately red run for the sake of a traceable tag" vs. "leave 0.3.0 tagged only
      by the CHANGELOG commit link". Nothing else depends on it — crates.io and GHCR are both
      published. — *source: JOURNAL (v0.3.0 lost its GitHub Release, 2026-08-01)*

- [x] **`service.name` is not groupable, only filterable.** *(closed 2026-08-01)* `SqlParams::attr_field`
      (`crates/imbh/src/sql.rs`) resolved a group/filter key to a real column only when it was in
      the DB's configured `Promote` list, and otherwise emitted `json_get_str(attributes, key)`.
      `service.name` lives in the `resource` column and the built-in promoted `service` column, never
      in record `attributes`, so `LogsApi::volume_by`, `TracesApi::span_metrics`, and the metrics
      group-by all collapsed it to a single `{"service.name": ""}` series with the counts merged —
      silently, since a missing attribute is a legitimate NULL. Filtering by service was unaffected.
      **Fixed** by a `builtin_column` branch ahead of the `Promote` lookup in `attr_field` /
      `attr_num_field`: both `service` and its OTel spelling `service.name` emit
      `CAST(service AS VARCHAR)`. That also removed the ad-hoc `key == "service"` special case in
      `metrics::label_cond` and the `service.name` special case in `AttrsApi::values`. The pinning
      test in `crates/imbh-server/tests/mcp_e2e.rs` now asserts the split (plus a new two-service
      case), joined by `logs_group_and_filter_by_service_name` in `crates/imbh/src/lib.rs`. The MCP
      `log_volume` / `span_metrics` `group_by` descriptions no longer tell models to call once per
      service. — *source: JOURNAL (MCP endpoint smoke test, 2026-08-01)*

- [ ] **MCP tools have no cost ceiling of their own.** Every tool bounds its own result (`limit`
      clamps, `max_rows` on `query_sql`), but nothing bounds the *work*: an agent can ask
      `query_sql` for a full-table aggregate over the whole retention window and park a blocking-pool
      slot for as long as it takes. `IMBH_BODY_TIMEOUT` does not cover it (the body is long since
      read) and the endpoint is unauthenticated. If this matters for a deployment, the fix is a
      per-call deadline around `tools::call` plus a scanned-bytes ceiling from `QueryStats`. —
      *source: JOURNAL (MCP endpoint, 2026-08-01)*
      **Reviewed 2026-08-06 and deliberately left open** (user decision), so a future sweep need not
      re-ask. The "if this matters for a deployment" framing still holds; the fix is described above
      and nothing about it has gone stale.

- [ ] **No write-side deadline on buffered HTTP responses.** `IMBH_BODY_TIMEOUT` used to bound the
      response write as well as body reads, via `set_write_timeout` on the socket; hyper exposes no
      equivalent, so a client that stops reading a *buffered* response holds a connection until it goes
      away. Bounded in practice by `IMBH_MAX_CONNECTIONS` (default `512`) rather than by time. The
      streaming case is already covered — the Docker plugin's `ReadLogs` abandons a stalled client
      after `STREAM_STALL` (30s), because its channel sink can see the backpressure. If the buffered
      case matters, the fix is a `tower` timeout layer around the response future or a connection-level
      deadline. — *source: JOURNAL (axum migration, 2026-08-01)*
      **Reviewed 2026-08-06 and deliberately left open** (user decision). The dependency a `tower`
      timeout layer would add is itself a footprint question, which is part of why this stays a
      conditional item rather than a pending fix.

- [ ] **The PromQL-agreement test compares against a transcription, not the real function.**
      `range_dedup_agrees_with_the_promql_collapse` (`crates/imbh/src/metrics.rs`) verifies that the
      typed metrics dedup resolves duplicates the same way PromQL does — but it carries a **verbatim
      copy** of `duplicate_value_cmp` / `collapse_duplicate_samples` rather than calling them, because
      `imbh-lgtm` depends on `imbh` and importing them into an `imbh` test would be a dev-dependency
      cycle. So it verifies agreement with a *copy* of the rule: drift in
      `crates/imbh-lgtm/src/model/promql.rs` would not trip it, which is precisely the failure the test
      exists to catch. The honest fix is to move that one test into `imbh-lgtm`, where both sides are
      in scope. Left in place only because the crate boundary was outside the change's remit.
      — *source: JOURNAL 2026-08-06 part 7*

- [ ] **Optional upstream differential runner.** Automate the versioned in-process fixture corpus
      against pinned Prometheus/Loki/Tempo daemons behind an opt-in test or script. Default
      workspace tests must remain daemon-free and offline. Deferred by explicit user request. —
      *source: JOURNAL (LGTM differential-testing follow-up)*

- [x] **Dependabot for the SHA-pinned GitHub Actions.** *(closed 2026-08-06.)* `.github/dependabot.yml`
      added with a single `package-ecosystem: github-actions` entry at `directory: "/"` (verified
      complete: there are no composite actions anywhere in the tree, so `/` — which covers
      `.github/workflows` plus a root `action.yml` — reaches everything). Weekly, Monday 09:00
      Asia/Tokyo, `ci:` commit prefix, minor+patch grouped into one PR so a single CI run clears the
      batch while majors stay ungrouped. Two corrections to the item: there are **ten** distinct
      actions, not nine (`taiki-e/install-action` was missed), and more importantly — per GitHub's
      Secure use reference, **Dependabot raises security *alerts* only for actions pinned to semver,
      never for SHA pins**, so the pinning had silently opted this repo out of action alerts
      altogether. This config is therefore not a convenience but the only automated channel by which
      an upstream action fix reaches `release.yml`, which holds the crates.io token and the registry
      login. Field names and enum values were validated against the current
      `dependabot-options-reference` from `github/docs@main`; SHA pins are preserved on update (the
      github-actions manager bumps the SHA and rewrites the trailing version comment — unconditional
      behaviour, no option to set). Watch item recorded in the file: `dtolnay/rust-toolchain` is
      pinned to a commit on its `stable` *branch*, not a tag, so its PRs carry no version to
      sanity-check the diff against. No `cargo` ecosystem entry — see the new open item below.

      Original text: Now **nine** actions across
      `.github/workflows/{ci,release,soak}.yml` are pinned to commit SHAs (the CD work added
      `actions/download-artifact` plus the four `docker/*` actions), so patch/security updates no
      longer arrive on their own. Add `.github/dependabot.yml` with a
      `package-ecosystem: github-actions` entry so the pins are refreshed by PR. Offered to the
      user, not yet added. — *source: JOURNAL (Actions SHA-pinning, 2026-07-24; CD, 2026-07-30)*

- [x] **The CD pipeline has never run.** *(closed 2026-08-06 — stale.)* It has run, successfully and
      repeatedly: full five-platform runs for `v0.2.0` (41m), `v0.4.0` (39m) and `v0.5.0` (39m), plus a
      successful `workflow_dispatch` on 2026-08-03. The specific unknowns the item listed
      (`x86_64-apple-darwin` cross-compile, `zstd-sys` under MSVC, the `ubuntu-22.04-arm` label, and
      the GHCR plugin push) are all answered by those green runs. The one thing still worth a manual
      look is the item's last point — the visibility of the `imbh-log-driver` GHCR package created by
      `GITHUB_TOKEN`, since a private one breaks `docker plugin install` for everyone else. Original
      text below.

      `release.yml`'s `build`/`publish`/`image`/`plugin` jobs were
      written and verified as far as a single host allows (the Dockerfile was built for both arches and
      the image run; the glibc guard, the smoke assertions, and the `docker,grpc,tracing` build were all
      checked locally), but no five-platform run has happened. Before the next release, do a
      `workflow_dispatch` run with `dry_run` left at its default — it builds and smoke-tests all five
      archives, both image arches, and both plugin arches, and publishes nothing. Specific unknowns:
      whether `x86_64-apple-darwin` cross-compiles cleanly (`zstd-sys` under Apple clang with
      `-arch x86_64`), whether `zstd-sys` builds under MSVC on `windows-latest`, and whether the
      `ubuntu-22.04-arm` label is available to this repository. For the `plugin` job specifically, the
      one thing a local run could not cover is `docker plugin push` **to GHCR** under
      `${{ github.token }}` — the artifact, the push, and the install were verified end to end against a
      local `registry:2`, but GHCR's own handling of a
      `application/vnd.docker.plugin.v1+json` config is untested. The first real push also **creates a
      new GHCR package** (`imbh-log-driver`); check its visibility afterwards, since a package created
      by `GITHUB_TOKEN` is not necessarily public just because the repository is, and a private one
      makes `docker plugin install` fail for everyone else. — *source: JOURNAL (CD, 2026-07-30; plugin
      publishing, 2026-08-03)*

- [x] **Measure the footprint budgets on the published targets.** *(closed 2026-08-06.)* Numbers
      harvested from run `31004270880` (tag `v0.5.0`, commit `5ae4259`) and folded into
      `ARCHITECTURE.md` Appendix C as a new subsection. Method matters: the `$GITHUB_STEP_SUMMARY`
      table the item expected to read is **not reachable through the REST API** at all (no summary
      endpoint; the check-run `output.summary` is `null` — it renders only in the web UI), so the five
      `dist-*` artifacts were downloaded, unpacked and sized directly, and their SHA-256 sums verified
      against the published Release's `SHA256SUMS` asset. Those are the shipped bytes, not an estimate,
      and not the artifact-zip sizes (which would have been a compressed zip of a compressed tarball).
      All five matrix targets are built *and* archived — nothing is built-but-unshipped. Two findings
      worth more than the item: **x86_64-linux `imbhd` is 41.1 MB against §2's 42 MB target — about
      888 KB of margin**, and the x86_64/aarch64 gap is 5.1 MB, so the aarch64 host figure the gate
      normally prints is the optimistic end of the range rather than a representative one. Also
      cross-validated: the aarch64-linux number is byte-identical to the pre-`docker-remap` baseline in
      the 2026-08-06 JOURNAL entry. Follow-ups split out below. — *source: JOURNAL (CD, 2026-07-30)*

- [x] **Retarget `OVERVIEW.md` §2's `imbhd` budget from musl to glibc x86_64.** *(closed 2026-08-06.)*
      The `imbhd` row now names `x86_64-unknown-linux-gnu` — the largest target the release archives
      actually ship — and carries the measured **41.1 MB** at v0.5.0 beside it. The old row named
      `x86_64-unknown-linux-musl`, which CD builds nowhere, so the budget named a binary nobody could
      measure and was in practice checked against whatever host ran the gate. The musl recommendation
      stands and was not implemented: no musl archive, because a sixth fat-LTO leg needs
      `cross`/`zigbuild` or a container (no native musl runner; `zstd-sys`/`onig_sys` build vendored C),
      and the Alpine/`scratch` case is already served by the `bookworm-slim` image plus the CI-asserted
      glibc ≤ 2.36 floor. §2's closing paragraph also gained the projection described in the item
      below. Original text below.

      Falls out of the
      Appendix C harvest above, and supersedes the old item's "decide whether a musl archive is worth
      adding". Recommendation from that work, for review rather than already applied: **do not add a
      musl archive** — `x86_64-unknown-linux-musl` is built nowhere in CD (it survives only in
      `about.toml`/`deny.toml` for license coverage), a sixth fat-LTO leg would need `cross`/`zigbuild`
      or a container because there is no native musl runner and `zstd-sys`/`onig_sys` build vendored C,
      and the Alpine/`scratch` demand is already served by the `bookworm-slim` image plus the
      CI-asserted glibc ≤ 2.36 floor. Instead restate §2's `imbhd` row as glibc x86_64 with the
      measured 41.1 MB beside it, so the budget names a target that actually ships and can therefore be
      checked. Deliberately not done as part of the harvest, which was scoped to Appendix C.
      — *source: TODO sweep 2026-08-06*

- [ ] **The next release's x86_64 `imbhd` is projected ~3 MB OVER the §2 target, and the local gate
      cannot see it.** *(quantified 2026-08-06; needs a CD dry run to confirm, then a decision.)* The
      `docker-remap` delta is now measured **exactly** rather than estimated: two local aarch64 release
      builds differing only in the feature, 35,973,488 → 39,997,800 bytes = **+4,024,312 B (+3.84 MiB)**.
      The local baseline lands within **24 bytes** of CD's v0.5.0 aarch64 archive (35,973,464 B), so the
      delta is trustworthy. Applying it to CD's x86_64-linux baseline of 41,112,104 B projects
      **≈ 45.1 MB against a 42 MB target** — over by ~3 MB, still well under the 55 MB hard limit.
      Two reasons this is live rather than theoretical: `release.yml`'s Linux legs carry
      `docker,docker-remap,grpc,tracing` as of `fc70cf8` while v0.5.0 shipped `docker,grpc,tracing`, so
      **the next release is the first whose x86_64 archive contains the VRL subtree at all**; and the
      footprint gate on an aarch64 host reads 40.0 MB and **passes**, because the same binary is 5.1 MB
      smaller there. The projection is conservative in the wrong direction — the delta was measured on
      aarch64, and x86_64 codegen is demonstrably fatter, so the real number is more likely above 45.1
      MB than below. Next step is a `workflow_dispatch` dry run (builds and smoke-tests all five
      archives, publishes nothing) to replace the projection with a measurement; then the decision is
      raise the target, trim the VRL subtree, or ship `docker-remap` only on a separate artifact. No
      local confirmation is possible on this host — there is no x86_64 cross linker.
      — *source: TODO sweep 2026-08-06*

      Original framing: `v0.5.0` was built with
      `docker,grpc,tracing`; `docker-remap` arrived later in `fc70cf8` on the current branch, and
      `release.yml`'s Linux legs now carry it. So every per-target number in Appendix C predates the
      VRL remapper, and the +3.8 MiB the JOURNAL measured locally has never been seen on the thin
      x86_64-linux margin (888 KB). Worth a `workflow_dispatch` dry run before the next release rather
      than discovering it during one. Pairs with the unmeasured-RSS item below.
      — *source: TODO sweep 2026-08-06*

      Original text: `scripts/footprint-gate.sh` still
      measures only the CI host, and `OVERVIEW.md` §2's budgets are musl numbers that nothing has ever
      verified (`x86_64-unknown-linux-musl` is in `about.toml`/`deny.toml` but is not a release-archive
      target). CD's `Package` step now writes per-target binary sizes into the run summary, so the
      first real cross-platform numbers will exist after one dispatch run — fold them into Appendix C,
      and decide whether a musl archive is worth adding alongside the glibc ones. — *source: JOURNAL
      (CD, 2026-07-30)*

- [x] **Windows portability beyond the directory fsync (issue #3 follow-up).** *(closed 2026-08-06 —
      stale.)* The job has run and is green on every recent CI run, including `main`. What it covers is
      deliberately narrow (build the workspace, then `cargo test -p imbh-storage` and
      `cargo test -p imbh --test lifecycle`), so the item's speculative next candidates — deletion and
      rename of open or memory-mapped files during **compaction and retention** — are only partly
      exercised by the lifecycle suite's compact step and not at all for retention. That residue is a
      coverage question for a future test, not an unwatched first run. Original text below.

      The `windows-latest`
      job added to `ci.yml` has never run — it was written without a Windows host to verify against
      (cross-compiling locally is blocked by `zstd-sys` needing mingw). It may surface further
      Windows-specific issues; deletion/rename of open or memory-mapped files during compaction and
      retention are the plausible next candidates. Watch the first run and fix what it finds. —
      *source: JOURNAL (issue #3, 2026-07-28)*

- [x] **Release carrying the Windows fix.** *(closed 2026-08-06 — stale.)* `v0.1.1` is tagged on the
      remote and published; four releases have shipped since (the workspace is at `0.5.0`). Original
      text below.

      `imbh-storage` 0.1.0 on crates.io cannot open an on-disk
      DB on Windows at all. The fix and the shared-version bump to **0.1.1** are on
      `fix/windows-dir-fsync` (PR #4), with the changelog entry staged under `## [Unreleased]`.
      Because the tree already carries 0.1.1, the release run is `cargo release` with **no** level
      argument (`cargo release patch` would bump again, to 0.1.2). Cutting it is the user's call
      (see `README.md` "Releasing"). — *source: JOURNAL (issue #3, 2026-07-28)*

- [x] **Docker log driver: `--tail 0 -f` has an inherent event-time race.** *(closed 2026-08-06 — and
      **without** the ingest-sequence column the item asked for.)* `observed_time` already existed on
      the logs schema (`schema.rs:110`, nullable, reserved at `:53`), was already on `LogEntry`, and the
      Docker driver already set it from dockerd's capture stamp (`ingest.rs:216-219`) and deliberately
      preserved it through VRL remap (`remap.rs:432-433`, test at `:830`). It was simply never exposed
      as a query axis. Added `LogOrder { Time, ObservedTime }` and `LogQuery::observed_after`
      (additive; both fields `#[serde(default)]` so old serialized queries still deserialize), then
      moved the driver's watermark and follow loop onto the arrival clock while leaving `--tail N` and
      full history ordered by **event** time, which is what `docker logs` prints. Only the cursor moved.
      A subtlety worth keeping: neither clock is monotone in the other, so paged watermarks merge by
      **max on each clock independently**, not by last-seen. The race test was confirmed non-vacuous —
      reverting the three behavioural hunks fails 3 of 19 e2e tests with a read timeout. Also added the
      projection-order pin (`projection_order_is_a_wire_contract`) that never existed, guarding
      `imbh-lgtm`'s positional column reads. Three residues are documented in
      `docs/DOCKER_LOG_DRIVER.md` rather than softened: a VRL script can overwrite
      `.observed_timestamp`; an exact nanosecond tie is still broken once by the strict `>`; and
      `--tail 0` has no uniquely correct answer, because json-file's semantic is defined by what is
      durably recorded while imbh batches. Original text below.

      With `--tail 0` the
      follow watermark deliberately jumps to `Timestamp::now()`, because "only new lines" is that
      flag's defined semantic. But a record's timestamp is when the container emitted the line while
      ingest lands it up to one batch interval later, so a line emitted just before the follow starts
      can still be missed under `--tail 0` specifically. The general case was fixed (see JOURNAL
      2026-07-30, defect 2); this residue is a semantics question, not a bug: closing it means either
      accepting it, or tracking an ingest-time column alongside event time so the tail can watermark
      on arrival order. — *source: JOURNAL (E2E against a real dockerd, 2026-07-30)*

- [x] **Typed `MetricsApi` still counts duplicate metric points.** *(closed 2026-08-06 — and the
      item's own proposed fix was the wrong one.)* The item said a window dedup "would need the
      ingest-sequence column the metric schemas do not have". It does not: `ARCHITECTURE.md` §10.5.1
      and line 1311 already require duplicates to be resolved **by value, never by scan order**,
      precisely so two identical queries cannot disagree after a flush or compaction. Ordering by an
      ingest sequence would have made the typed API and PromQL resolve the same duplicate differently.
      Implemented as `ROW_NUMBER() OVER (PARTITION BY "time", metric, service, resource, scope,
      attributes ORDER BY isnan(value) ASC, value DESC)` keeping rank 1, gated on
      `Duplicates::LastWins`; under every other policy the SQL is byte-identical to before, and the
      `WHERE` stays on the inner scan so the §9.2 pushdown contract is untouched. `instant` and
      `range_batches` inherit it free. Two findings that changed the plan mid-flight: **`value = value`
      does not detect NaN** — DataFusion 54 orders floats by a total order, not IEEE, so that idiom and
      every comparison variant return *true* for NaN and a NaN would have silently won every duplicate;
      and **`isnan` is available anyway** despite the `default-features = false` pin, because
      DataFusion declares `datafusion-functions` non-optional with default features on, at zero crate
      cost (verified: count unchanged at 275). Both anti-regression tests were **mutation-checked** —
      dropping `resource, scope` from the partition key, or dropping `isnan`, makes them fail.
      Deferred deliberately, with reasons recorded in §10.5.1: `ErrorOnRead` parity, which would cost a
      detection scan on every typed range query for every user and turn today's numbers into errors on
      published crates. One known weakness: the PromQL-agreement test compares against an **inline
      mirror** of `collapse_duplicate_samples`, not the real function, because `imbh-lgtm` depends on
      `imbh` and importing it would be a dev-dependency cycle; a true cross-crate check would have to
      live in `imbh-lgtm`. Original text below.

      Issue #27 gave PromQL a
      `Duplicates` policy (ARCHITECTURE.md §10.5.1), but `MetricsApi::range`/`instant`
      (`crates/imbh/src/metrics.rs`) still `SUM`/`COUNT` two points sharing a series and a timestamp,
      so `sum`/`count` inflate and `avg` skews (`RateMode::Counter`'s `max - min` is immune). A known
      asymmetry rather than an oversight: that path degrades a number instead of denying service, and
      a SQL dedup would need either `SELECT DISTINCT` (wrong the moment the duplicated *values*
      differ) or `ROW_NUMBER() OVER (…)` with a deterministic ordering key — which needs the
      ingest-sequence column the metric schemas do not have. Note the item above wants that same
      column for the log-driver tail race; one column would close both. — *source: issue #27*

- [x] **The footprint gate's `datafusion` assertion is vacuous.** *(closed 2026-08-06, with the
      diagnosis corrected.)* **The premise was wrong**: the bare `datafusion v54.1.0` crate *is* in
      `imbh`'s graph (2 matching lines; line 185 of 999, alongside 30 split crates), so the pattern
      matched fine and the check was not vacuous in the way stated. The check was still weak for a
      different reason, and was fixed on three axes:
      1. **It printed `yes`/`NO` and exited 0.** A `NO` that does not fail guards nothing regardless of
         the pattern. Both engine checks now set `fail=1`, matching the search-lever guard below them.
         `search` and `query` are on by default, so absence is never a deliberate trim.
      2. **The pattern was pinned to the bare facade.** Now `datafusion(-[a-z-]+)? v` — the crate
         *family* — so it will not false-alarm the day `imbh-query` depends on `datafusion-core`
         instead of the facade.
      3. **`grep -q` under `set -o pipefail`.** All four `grep -q` sites now read from here-strings
         rather than a pipe. `grep -q` exits on first match, which can leave the upstream writer with
         a broken pipe and make the pipeline report SIGPIPE (141) despite the match. **Caveat on the
         numbers**: the subagent reported measuring 63 false negatives in 400 runs, but that did not
         reproduce on re-check (0 in 900) and the mechanism cannot fire at the current size — the tree
         is ~55 KiB, under the 64 KiB pipe buffer, so the writer never blocks. Treat the here-strings
         as cheap insurance against a future larger tree, not as a fix for a measured flake. The
         likelier cause of whatever was observed is `cargo tree` failing under concurrent-cargo lock
         contention (three agents were running at the time) with its stderr swallowed by `2>/dev/null`.
      4. **A regression introduced by (1), caught in review and fixed**: because the checks now fail
         hard, an empty `$tree` from a failed `cargo tree` turned a transient infrastructure hiccup
         into a red gate claiming both engines had vanished. The capture is now guarded and reports
         "could not run" once, distinctly, with cargo's own error. — *source: JOURNAL (2026-08-06,
         part 2)*

- [x] **`QUALITY_GATE.md`'s search-off crate count is wrong (216 vs the measured 71).** *(closed
      2026-08-06.)* Took option (b) — the doc meant the search-only lever, so the genuine measurement
      was added rather than just correcting a digit. Measured on aarch64-glibc with
      `cargo tree -p imbh --edges normal` (unique): default `ingest,query,search` = **275**;
      `--no-default-features --features ingest,query` = **217** (-58, the tantivy subtree — the *real*
      cost of turning search off); `--no-default-features` = **71** (-204, which also drops OTLP decode
      and the whole DataFusion subtree). The doc's 216 was the search lever's number attached to the
      wrong knob. The gate itself was the origin of the confusion — its old
      `tantivy dropped: yes (-204 crates)` line attributed the entire `--no-default-features` delta to
      tantivy — so it now prints all three numbers, labelled, and additionally checks the precise
      `ingest,query` lever for tantivy leakage, not just the bare build. Also added a §1
      scripting-pitfall note. — *source: JOURNAL (2026-08-06, part 2)*

      Original text: The doc says
      "turning `search` off (`imbh --no-default-features`) drops the tantivy subtree to 216 crates";
      the gate measures **71**. The two are not the same operation — `--no-default-features` drops
      `ingest`, `query` *and* `search`, so it also takes the entire DataFusion subtree with it, which
      is most of the difference. Either correct the number and say which knob it describes, or add a
      genuine search-off-only measurement (`--no-default-features --features ingest,query`) if that is
      the lever the doc meant to document. — *source: JOURNAL (2026-08-06, part 2)*

- [x] **RSS is unmeasured for the `docker-remap` build.** *(closed 2026-08-06 — measured on both
      axes; nothing is blown.)* Two separate measurements, because the first one is not the one the
      item was about. **Database** (`RSS_PROBE=1`, `examples/rss-probe`, aarch64 glibc VmRSS): idle
      **15.0 MB**, steady **104.8 MB**, against §2's 40 / 200 MB targets — but `rss-probe` is not even
      built with `docker-remap`, so it is only the baseline. **Driver** (new opt-in soak,
      `crates/imbh-server/tests/soak_docker_rss.rs`, release): the item's actual concern. Idle
      **12 MiB**, worst steady across the whole matrix **93 MiB** (peak 100), and a remapping plugin at
      **100 concurrently-logging containers sits at 67 MiB** — comfortably inside the 200 MB steady
      target. The item's central claim is **confirmed, not assumed**: the remap differential decomposes
      as **4.0 MiB fixed + 0.165 MiB per container**, and holding containers fixed while varying line
      count over a 100× range moves it by +1.0 / +2.3 / +1.4 MiB, i.e. noise. Idle is *identical* with
      and without remapping (12 MiB), because the `Runtime` is built at `StartLogging`.
      Method note worth keeping: the soak measures three columns, not two — plain `docker`,
      `docker-remap` with the **identity script `.`**, and `docker-remap` with the built-in script. The
      identity control is what makes the line-rate number interpretable: it provably yields
      byte-identical records to no script yet still clones a seed per line, so `identity − off` isolates
      the remapper machinery, while `builtin − identity` turns out to be payload growth (a parsed
      kvlist body is simply bigger) rather than remapper state. Also corrected while measuring: the
      seed clone at `remap.rs:385` is allocation **churn, not retained memory**, and the doc comment at
      `remap.rs:319` overstates it by claiming the event object's allocation is recycled — the
      `TargetValue` is reused but the whole `Value` is replaced. Gated `#[ignore]` plus
      `cfg(all(feature = "docker", unix, target_os = "linux"))`, verified to build **0 tests** on the
      default path. Original text below.

      The footprint work for the VRL remapper ran
      the gate with `RSS_PROBE=0`, so §2's idle (40 MB) and steady (200 MB) targets have not been
      checked against a remapping plugin. Crate count and binary size were measured (+89 crates,
      +3.8 MiB, plugin binary 40.0 MB against a 42 MB target). Steady-state is the plausibly-affected
      axis: each container's FIFO thread owns a VRL `Runtime`, and each line clones a seed event
      object — so the cost scales with container count, not line rate. `cargo run --release -p
      rss-probe` plus a driver soak would close it. — *source: JOURNAL (2026-08-06)*

- [x] **Check the `imbh-log-driver` GHCR package's visibility.** *(closed 2026-08-06 — both packages
      are public.)* Verified by the property that actually matters rather than by reading a settings
      page: an **anonymous** pull token from `ghcr.io/token` followed by `GET /v2/<pkg>/tags/list`
      returns `200` for both `moriyoshi/imbh` (tags `0.2.0` … `0.5.0`, `latest`) and
      `moriyoshi/imbh-log-driver` (`0.5.0-amd64` / `0.5.0-arm64` / `0.5-*` / `latest-*`). That is
      exactly the access `docker plugin install` needs from a stranger, so nothing is blocked. The
      plugin carries v0.5.0 tags only, which is consistent — plugin publishing was added 2026-08-03.
      Note for whoever repeats this: `gh api user/packages` needs the `read:packages` scope, which the
      local token does not have; the anonymous registry check needs no scope at all and tests the real
      property. — *source: TODO sweep 2026-08-06*

- [x] **Decide whether Dependabot should also watch `cargo`.** *(closed 2026-08-06 — security updates
      only.)* A `cargo` entry was added at `directory: "/"` with **`open-pull-requests-limit: 0`**,
      which is the documented way to run an ecosystem for security updates alone: per GitHub's options
      reference, "you can temporarily disable version updates for a package manager by setting this
      option to zero", and "security update pull requests are not subject to this limit and do not
      count toward it". There is no separate "security only" switch; this is the mechanism. Rationale
      recorded in the file — routine version churn stays a deliberate `cargo update` with the footprint
      gate re-run (the margin is thin: v0.5.0 x86_64-linux `imbhd` measured 41.1 MB against a 42 MB
      target), but an *advisory* had no automated path at all, since `cargo-deny` fails CI on RUSTSEC
      without fixing anything. Raise the limit above 0 only alongside a decision about who re-runs the
      gate per bump. Original text below.

      Deliberately left out of
      `.github/dependabot.yml` (the item asked only for github-actions), and it is a real tradeoff
      rather than an oversight. *For*: 15 workspace crates and a large graph, where transitive security
      fixes currently arrive only when someone runs `cargo update` by hand — `cargo-deny` catches
      advisories in CI, but catching is not fixing. *Against*: the footprint budgets are load-bearing,
      not advisory (`scripts/footprint-gate.sh` gates crate count at 275/300 and binary size), a patch
      bump can eat gate headroom without tripping it, and Dependabot has no notion of the hand-trimmed
      `default-features = false` sets the project depends on — plus a graph this size on `weekly` means
      steady PR traffic, each PR costing a full multi-job CI run. Middle path if wanted: a `cargo`
      entry restricted to security updates via `allow`, or patch-only grouped with a low
      `open-pull-requests-limit`. — *source: TODO sweep 2026-08-06*

- [x] **Windows coverage stops short of compaction and retention.** *(closed 2026-08-06 — and it was
      hiding a real defect, not just a test gap.)* Filed as a coverage item; investigating it found the
      bug the coverage would have caught. `Storage::retain` and `Storage::compact` propagated the
      post-manifest unlink error with `?`. On POSIX an unlink always succeeds regardless of open
      handles or mappings, so this was silently fine; **on Windows a file with a live memory mapping
      cannot be deleted at all**, whatever share flags its opener passed — and `imbh-index`'s
      `search_body` / `search_attr_eq` hold exactly such mappings via Tantivy's `MmapDirectory` on the
      `.tidx` sidecars, while queries run on the tokio runtime concurrently with the background
      `maintain()` / `retain()` pass. So one concurrent `matches()` pushdown racing maintenance would
      turn a pass that had **already succeeded** (the manifest was durable without those segments) into
      a hard `Err`, and abandon the rest of the batch mid-loop. **This is a code-reading conclusion,
      not an observed failure** — the work was done on Linux, where the bad ordering succeeds silently,
      and no Windows host was available to reproduce it. Fixed by a `reclaim_segments` helper
      (`crates/imbh-storage/src/lib.rs:2601`) making the unlink best-effort with a `tracing` warning:
      not a new failure mode, since a crash in the same window already left orphans and
      `cleanup_orphans` sweeps them on the next open. No public API change, so no semver impact.
      Three tests in `imbh-storage` and one in `imbh --test lifecycle` — both targets the Windows leg
      already runs, so **no CI widening was needed**; the job's comment was extended to name the new
      coverage. The tolerance test pins segments with a live `memmap2::Mmap` and was confirmed
      non-vacuous by temporarily panicking inside `reclaim_segments` (it was the only test in the suite
      to trip that branch). `memmap2` was added as a **dev-dependency only** and already reaches the
      graph via tantivy, so the footprint gate is unaffected — verified at 275/275 crates.
      Not done deliberately: a persistent retry queue so a refused unlink retries on the next pass
      rather than waiting for a reopen — invasive new durable state, and the orphan sweep already
      bounds the leak. — *source: TODO sweep 2026-08-06*

## Closed, awaiting the next sweep

- [x] **MCP over stdio, hosted by `imbh-tui`.** *Closed 2026-08-01.* `imbhd` served MCP over HTTP
      only (§10.16.1); the other half of the plan was a stdio transport in the TUI binary, since stdio
      is what MCP clients "SHOULD support whenever possible" and it needs no listening port. Done as
      the item called for, with one correction to its premise: `mcp::handle` was *not* quite
      transport-agnostic — its header/body agreement check (`MCP-Protocol-Version` / `Mcp-Method` /
      `Mcp-Name`) is a **Streamable HTTP** rule, and over a pipe there is no header channel to agree
      with, so a modern request would have been refused for a missing header it could never carry.
      The dispatch now takes a `Transport` (`Http(Headers)` / `Stdio`) and validates the mirror only
      for HTTP. The protocol module was lifted out of `imbh-server` into the new **`imbh-mcp`** crate,
      per the item's note on dependency direction (`imbh ← imbh-mcp ← {imbh-server, imbh-tui}`); it
      also took `batches_to_json` / `stats_json` / `offload` along, since the tools and the HTTP
      endpoints share them. Both data-access flags shipped: `imbh-tui --mcp-stdio <dir>`
      (`Db::open_read_only`, reads alongside a live writer) and `--mcp-stdio --url <addr>` (forwards
      to a running `imbhd`, synthesizing the header mirror from the message it forwards, over
      hand-written HTTP/1.1 so no HTTP client dependency enters the TUI). Covered by
      `crates/imbh-mcp/tests/stdio_e2e.rs`; footprint unchanged (facade 275 crates; +1 *workspace*
      crate each on `imbh-server` and `imbh-tui`). — *source: JOURNAL (MCP endpoint, 2026-08-01;
      MCP over stdio, 2026-08-01)*

- [x] **`imbhd` connections have no read/write timeout.** *Closed 2026-08-01.* A client that opened a
      socket and sent nothing parked a connection thread in `read_line` forever, costing a thread per
      idle connection and making the shutdown drain (`IMBH_SHUTDOWN_TIMEOUT`) wait out its whole
      deadline instead of finishing early. Resolved as the item called for — a header/body deadline
      rather than a blanket `set_read_timeout`: `IMBH_HEADER_TIMEOUT` (default `10s`) bounds the request
      head *in total*, `IMBH_BODY_TIMEOUT` (default `30s`) is a *per-read* allowance for the body plus
      the response write, `0` disables either, and a blown deadline answers `408`. The per-read rule on
      the body is what preserves the slow-large-upload case the item flagged. Enforcement sits in an
      `Armed` reader *under* the `BufReader`, since arming per `read_line` would silently make the head
      deadline per-read too. — *source: JOURNAL (graceful shutdown, 2026-07-31; connection deadlines,
      2026-08-01)*

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

# Project To-Dos

Items extracted from JOURNAL.md during `good-sleep` consolidation, plus open follow-ups. Each
item should be resolved or removed once addressed. Design-level open *questions* (as opposed to
actionable work) live in `ARCHITECTURE.md` §15, not here.

Completed items are swept periodically (their durable knowledge lives in `.agents/docs/LTM/` and
git history); this file tracks only what is still open.

## Open Items

- [ ] **The head API needs a semver bump before release (`imbh-head` is a new published crate).** The
      TUI-as-a-head work (ARCHITECTURE.md §10.19, JOURNAL 2026-08-01) added a 15th shipping crate,
      `imbh-head`, and made one **breaking** change to `imbh-tui`'s published surface:
      `cli::Mode::Tui { path: PathBuf, .. }` is now `cli::Mode::Tui { source: Source, .. }`, since the
      explorer takes `--url` as well as a directory. `imbh_tui::run` is *not* breaking — it now takes
      `impl Into<Backend>` and `From<Arc<Db>>` keeps `run(db, options)` compiling. Under the 0.x rule
      that is a `0.4.0`, not a patch. `cargo release` also has to learn the new crate (it inherits
      `version.workspace` and `publish = true`, so it should be picked up automatically — worth
      confirming on the dry run). Not done here: releases are cut only when explicitly asked.

- [ ] **`GET /stats` still cannot be parsed back into a typed value, and omits the ingest gauges.** The
      head API answers `GET /api/head/stats` with a complete, serde-round-trippable `Stats` instead of
      widening `/stats`, whose hand-written JSON (`imbh_mcp::stats_json`, shared with the `db_stats`
      tool) is an existing public contract reporting neither `ingest_queue_depth` nor `ingest_dropped`
      nor `ingest_errors`, and spelling a `None` durable LSN as `0`. Adding the three gauges is purely
      additive and would let the two converge on one serializer; the `durable_lsn` spelling is the part
      that would actually change what a current consumer sees.

- [ ] **Decide whether to restore the `v0.3.0` git tag on the remote.** It was deleted while trying to
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

- [ ] **No write-side deadline on buffered HTTP responses.** `IMBH_BODY_TIMEOUT` used to bound the
      response write as well as body reads, via `set_write_timeout` on the socket; hyper exposes no
      equivalent, so a client that stops reading a *buffered* response holds a connection until it goes
      away. Bounded in practice by `IMBH_MAX_CONNECTIONS` (default `512`) rather than by time. The
      streaming case is already covered — the Docker plugin's `ReadLogs` abandons a stalled client
      after `STREAM_STALL` (30s), because its channel sink can see the backpressure. If the buffered
      case matters, the fix is a `tower` timeout layer around the response future or a connection-level
      deadline. — *source: JOURNAL (axum migration, 2026-08-01)*

- [ ] **Optional upstream differential runner.** Automate the versioned in-process fixture corpus
      against pinned Prometheus/Loki/Tempo daemons behind an opt-in test or script. Default
      workspace tests must remain daemon-free and offline. Deferred by explicit user request. —
      *source: JOURNAL (LGTM differential-testing follow-up)*

- [ ] **Dependabot for the SHA-pinned GitHub Actions.** Now **nine** actions across
      `.github/workflows/{ci,release,soak}.yml` are pinned to commit SHAs (the CD work added
      `actions/download-artifact` plus the four `docker/*` actions), so patch/security updates no
      longer arrive on their own. Add `.github/dependabot.yml` with a
      `package-ecosystem: github-actions` entry so the pins are refreshed by PR. Offered to the
      user, not yet added. — *source: JOURNAL (Actions SHA-pinning, 2026-07-24; CD, 2026-07-30)*

- [ ] **The CD pipeline has never run.** `release.yml`'s `build`/`publish`/`image`/`plugin` jobs were
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

- [ ] **Measure the footprint budgets on the published targets.** `scripts/footprint-gate.sh` still
      measures only the CI host, and `OVERVIEW.md` §2's budgets are musl numbers that nothing has ever
      verified (`x86_64-unknown-linux-musl` is in `about.toml`/`deny.toml` but is not a release-archive
      target). CD's `Package` step now writes per-target binary sizes into the run summary, so the
      first real cross-platform numbers will exist after one dispatch run — fold them into Appendix C,
      and decide whether a musl archive is worth adding alongside the glibc ones. — *source: JOURNAL
      (CD, 2026-07-30)*

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

- [ ] **Docker log driver: `--tail 0 -f` has an inherent event-time race.** With `--tail 0` the
      follow watermark deliberately jumps to `Timestamp::now()`, because "only new lines" is that
      flag's defined semantic. But a record's timestamp is when the container emitted the line while
      ingest lands it up to one batch interval later, so a line emitted just before the follow starts
      can still be missed under `--tail 0` specifically. The general case was fixed (see JOURNAL
      2026-07-30, defect 2); this residue is a semantics question, not a bug: closing it means either
      accepting it, or tracking an ingest-time column alongside event time so the tail can watermark
      on arrival order. — *source: JOURNAL (E2E against a real dockerd, 2026-07-30)*

- [ ] **Typed `MetricsApi` still counts duplicate metric points.** Issue #27 gave PromQL a
      `Duplicates` policy (ARCHITECTURE.md §10.5.1), but `MetricsApi::range`/`instant`
      (`crates/imbh/src/metrics.rs`) still `SUM`/`COUNT` two points sharing a series and a timestamp,
      so `sum`/`count` inflate and `avg` skews (`RateMode::Counter`'s `max - min` is immune). A known
      asymmetry rather than an oversight: that path degrades a number instead of denying service, and
      a SQL dedup would need either `SELECT DISTINCT` (wrong the moment the duplicated *values*
      differ) or `ROW_NUMBER() OVER (…)` with a deterministic ordering key — which needs the
      ingest-sequence column the metric schemas do not have. Note the item above wants that same
      column for the log-driver tail race; one column would close both. — *source: issue #27*

- [ ] **The footprint gate's `datafusion` assertion is vacuous.**
      `scripts/footprint-gate.sh` greps `cargo tree -p imbh --edges normal` for `datafusion v`, but the
      graph contains only the split crates (`datafusion-core`, `datafusion-common`,
      `datafusion-physical-plan`, …) and no bare `datafusion`. It therefore prints `datafusion: NO` on
      a perfectly healthy tree and would print the same thing if the engine really did vanish — so it
      guards nothing. Fix is a one-liner (`grep -q 'datafusion[a-z-]* v'`, or assert on
      `datafusion-core`), but pick the pattern deliberately: the point of the check is to catch a
      feature-flag mistake that silently drops the query engine. Note the tantivy check on the next
      line does work, which is why this went unnoticed. — *source: JOURNAL (2026-08-06, part 2)*

- [ ] **`QUALITY_GATE.md`'s search-off crate count is wrong (216 vs the measured 71).** The doc says
      "turning `search` off (`imbh --no-default-features`) drops the tantivy subtree to 216 crates";
      the gate measures **71**. The two are not the same operation — `--no-default-features` drops
      `ingest`, `query` *and* `search`, so it also takes the entire DataFusion subtree with it, which
      is most of the difference. Either correct the number and say which knob it describes, or add a
      genuine search-off-only measurement (`--no-default-features --features ingest,query`) if that is
      the lever the doc meant to document. — *source: JOURNAL (2026-08-06, part 2)*

- [ ] **RSS is unmeasured for the `docker-remap` build.** The footprint work for the VRL remapper ran
      the gate with `RSS_PROBE=0`, so §2's idle (40 MB) and steady (200 MB) targets have not been
      checked against a remapping plugin. Crate count and binary size were measured (+89 crates,
      +3.8 MiB, plugin binary 40.0 MB against a 42 MB target). Steady-state is the plausibly-affected
      axis: each container's FIFO thread owns a VRL `Runtime`, and each line clones a seed event
      object — so the cost scales with container count, not line rate. `cargo run --release -p
      rss-probe` plus a driver soak would close it. — *source: JOURNAL (2026-08-06)*

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

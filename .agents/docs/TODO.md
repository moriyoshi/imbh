# Project To-Dos

Items extracted from JOURNAL.md during `good-sleep` consolidation, plus open follow-ups. Each
item should be resolved or removed once addressed. Design-level open *questions* (as opposed to
actionable work) live in `ARCHITECTURE.md` §15, not here.

Completed items are swept periodically (their durable knowledge lives in `.agents/docs/LTM/` and
git history); this file tracks only what is still open.

## Open Items

- [ ] **`request_waterfall` is not covered by the input lock or the banner.** The per-cursor-move
      trace fetch (`tasks.rs::request_waterfall`) sets no `loading` flag, so a slow waterfall neither
      raises the banner nor pauses input; the preview pane just says "Loading waterfall...". It is a
      different shape of load — one detail pane rather than the whole screen — so folding it into
      `App::loading` would over-lock. Wants its own lighter treatment (a pane-local spinner) once the
      debounce + LRU item below lands, since that changes when the fetch fires at all.

- [ ] **There is no way to cancel an in-flight query.** With input locked, a query that never lands
      leaves `q` as the only exit, which is why `survives_loading` special-cases it. The honest fix is
      an interrupt (`Esc` cancelling the in-flight query, or a `Ctrl-C` binding — the TUI has none
      today) that drops the result by generation and calls `App::end_loading`. Cheap on the client
      side; the backend request keeps running either way until the head-side timeout.
- [x] **`dto::Series` gained `query_index`: 0.8 -> 0.9 before publishing.** *(Closed 2026-08-24 —
      folded into the v0.9.0 preparation below.)* A public field is a breaking change for
      struct-literal callers; the derive of `Default` covers `..Default::default()` users and
      `Series::new` covers the rest. One correction to what this item used to claim: only `Series`
      carries `#[non_exhaustive]`, not "the response DTOs" — the rest of `imbh-head`'s response
      structs are still exhaustive, so the next field added to any of them is another breaking
      change. Worth doing to all of them in some later bump.

- [ ] **No column projection reaches the Parquet reader.** `SegmentPartitionStream` yields
      full-schema batches and lets `StreamingTableExec` project above it
      (`crates/imbh-query/src/provider.rs`), so the wide `spans` attributes/resource/events/links
      JSON is decoded on every traces query even when the plan needs two columns. Pushing a
      `ProjectionMask` into `ParquetRecordBatchReaderBuilder` is the largest remaining constant
      factor on the read path. Three constraints make it the riskiest change in the area, and the
      reason it was not taken with the rest: the buffer snapshot batch must be projected identically;
      pushdown is `Inexact`, so the `FilterExec` above the scan needs its predicate columns to
      *survive* the mask (dropping one is a wrong-results bug, not a slow one); and the Tantivy
      `RowSelection` and row-group subsets must still compose with it. Gate it behind a test that
      runs the existing provider corpus with pushdown on and off and asserts batch-for-batch
      equality.

- [ ] **`traces().get()` still footer-reads every segment.** It carries no time predicate, so the
      raw-bytes bloom probe can only be answered by opening each segment's Parquet footer — measured
      19.4 ms at 640 segments, and paid once per cursor move on the Traces list. `get_many` already
      takes a `not_before` and TraceQL passes it; the single-trace path needs the same, which means
      an optional bound on the head's `TraceGetRequest` (the TUI always knows its search window).
      Note the bound must be a *lower* one: a trace's start is the minimum over its spans, so
      `start_time >= T` for any `T` at or below the true start is exact, whereas an upper bound would
      need the trace's unknown duration and could truncate a long trace.

- [ ] **The TraceQL candidate search is prunable but not flat.** `tq-search` went 134 -> 39 ms at 640
      segments once phase 1 got its `WHERE`, but it still grows (3.2 -> 38.9 across a 64x corpus).
      The residual is phase 2: it fetches `2n + 16` candidates' spans through a bloom-probed
      `IN` list whose selectivity falls as the list grows, and it must stay *unbounded in time* so
      the true trace start is recoverable for the exactness recheck. Worth measuring whether a
      cheaper exact-start source exists (a per-segment min-start sketch?) before adding slack.

- [ ] **One `SessionContext` per query.** Acknowledged at `crates/imbh-query/src/lib.rs:8-10`. UDF
      registration and optimizer setup are per-query costs; providers would stay per-query since they
      capture a snapshot. Note the `GreedyMemoryPool` becomes shared across concurrent queries, which
      is arguably more correct for an RSS budget but changes allocation behaviour — re-run
      `crates/imbh/tests/soak_rss.rs`.

- [ ] **Per-cursor-move trace fetch is unthrottled and single-slot.** Holding down-arrow on the
      Traces list issues one full `traces().get()` per row (`keys.rs` -> `tasks.rs::request_waterfall`).
      A ~120 ms idle debounce plus a small LRU keyed by trace id would collapse a scroll into one
      fetch. Prerequisite: keep `Vec<TraceMatch>` on the snapshot instead of re-parsing the id out of
      the rendered row string (`fetch.rs` <-> `app/views.rs`), which is also worth doing on its own.

- [ ] **`metrics().dimensions()` and `series()` are still unbounded scans.** Same missing time
      predicate the catalog had (`crates/imbh/src/metrics.rs`). `dimensions` is fetched once per
      metric by the TUI so it is not per-refresh, but it is still whole-corpus. Route them through
      the catalog's fold or bound them to the eval window. `exemplars()` likewise takes only a metric
      name and is filtered by window client-side.

- [ ] **v0.9.0 is prepared but not cut.** The workspace is bumped to 0.9.0, the changelog section
      closed and dated 2026-08-24, `README.md` / `docs/DOCKER_LOG_DRIVER.md` version strings stamped,
      notices regenerated and every gate green — see JOURNAL "Preparing v0.9.0". Nothing is
      committed, tagged or published beyond the prep branch; that is the user's call.

      **A minor bump the signatures require.** Two breaking changes, both in the `[0.9.0]`
      `### Changed` entries: `imbh-head`'s `dto::Series` gained a public `query_index` field and
      became `#[non_exhaustive]`, and `imbh-lgtm`'s `execute_traceql` gained an `S: Sync` bound.

      **No new crates and no dependency change**, so the publish order is exactly v0.8.0's and the
      footprint is unmoved (275 crates, `imbhd` 33.5 MiB).

      *(v0.8.0 — the entry this replaces — was tagged and released 2026-08-09.)*

- [x] **Measure the promoted-column cost before setting any auto-promotion budget.** *(Closed
      2026-08-08 — `examples/bench --bin promote-cost`. The gate passes, and it moved the budget from
      "how many keys" to "which keys".)* 50k log rows + 5k spans + 5k gauges, keys present on logs
      only so the six all-NULL columns on the other signals are included; one process per count, since
      sharing a process makes the RSS axis meaningless (the first count absorbs ~200 MiB of
      first-touch pages and later ones measure ~0).

      **Low-cardinality keys — the case promotion exists for — are nearly free.** Disk is bit-exact
      reproducible: 0 keys 1,230,070 B; 1 key +1,221; 5 keys +6,042; 20 keys +24,110. That is
      **~1,206 B per key, +2.0% at 20 keys**. Seal time and buffer RSS are both **below the noise
      floor**: best-of-5 seal is 487.8 ms at 0 keys vs **473.3 ms at 20** (the 20-key run is faster),
      and VmRSS is 210,348–210,356 kiB across all four counts. Arithmetic bounds the buffer cost at
      4 bytes/row/key for the `Int32` index array — 4 MiB at 20 keys x 50k rows, ~2% of the ingest
      working set, which is why it is invisible.

      **A wrong choice costs 95x more.** Re-run with every value distinct (`card=50000`, the
      high-cardinality anti-pattern §6.1 warns against): 0 keys 4,694,514 B → 20 keys 6,980,273 B, i.e.
      **+114,288 B per key and +48.7% total**, with seal 770 → 883 ms (+15%). So the budget must gate
      on **cardinality, not on key count** — 20 well-chosen keys cost +2%, 20 badly-chosen ones nearly
      halve storage efficiency. `attr-stats` already reports per-key distinct-value counts, so the
      classifier has exactly the input this implies. — *source: promote-cost, 2026-08-08*

      **Correction, same day: the driver is in-segment *repetition*, not cardinality of any kind.**
      Parquet builds its dictionary per column chunk, so what a promoted column costs is how much its
      values recur *within a segment*. At a fixed 50 segments x 1,000 rows, per-key disk cost was
      +1,206 B at 3,125x repetition, +22,067 B at 50x, and +108,842 B when every value was unique per
      row. Spreading 50,000 globally-distinct values across 50 segments barely helped (+108,842 vs
      +114,284 B/key) because the values were still unique per row — segmenting does not reduce how
      many distinct strings must be stored. **So a gate on global distinct count is wrong**, and would
      reject exactly the keys worth promoting: `pod.name` has huge global cardinality but only the
      currently-running pods appear in any one segment, each on many rows, so it is cheap.

      **Second correction, same day: repetition is necessary but not sufficient — run structure is
      the larger term.** A promoted column is a dictionary *plus a per-row `Int32` index array*, and
      the index array's compressed size tracks the **entropy of the value sequence**, which
      repetition cannot see. `archetype-bench` isolated it with a controlled pair: the same session
      population, ~25 events each, laid out contiguously versus interleaved across 200 concurrent
      sessions — **9,079 B against 64,252 B**, 7.1x apart, of which only ~2x is postings. A
      `rows/postings` gate rated `k8s.pod.name` (42,135 B) cheaper than the contiguous sessions
      (9,079 B). The model is now

          est B/row = [ C(seg) * mean_len + runs * log2(C(seg)) / 8 ] / rows_per_segment

      which ranks all seven archetypes in exactly their measured disk order across a 26x range.
      Absolute values run 2-5x high (zstd beats the model), so it ranks keys rather than sizing
      budgets. `attr-stats` now counts `runs` — which required `scan.rs` to walk rows in order, since
      the dictionary path previously tallied per-entry counts and discarded order entirely.

- [ ] **Cross the demand signal with the cost signal — cost alone picks the wrong keys.**
      `archetype-bench` measured promotion's *speed* win and it is entirely at **unselective**
      filters: `env` at 100% selectivity went 19.80 -> 6.93 ms and `http.method` at 14% went
      10.52 -> 6.92 ms, while every key at or below 1.7% saw **no win at all** — the `attrs` index
      already has those at the `count(*)` floor (5.10 ms against a 5.62 ms floor). That is the same
      boundary the cost gate shows from the other side, declining to prune above a ~50% hit fraction.

      So a promoted column pays for keys that are queried *unselectively*, and the cost model — even
      the corrected two-term one — cannot express that, because it only answers "what would this
      column cost", never "would anyone benefit".

      **A demand counter existed and was cut before release (2026-08-09).** `Db::attr_access_stats`
      tallied `(key, backend, count)` at SQL-construction time. It was removed rather than shipped
      because nothing consumed it and, more importantly, it recorded the wrong thing:
      - **no selectivity**, which the archetype measurements show is the deciding axis — promotion's
        win is entirely at unselective filters (`env` at 100%: 19.80 -> 6.93 ms; nothing at or below
        1.7%);
      - **filters and projections conflated** — `select.push(format!("{} AS g{i}", p.attr_field(k)))`
        increments identically to a `WHERE` predicate, so a key only ever grouped by is
        indistinguishable from one filtered on, and only the latter bears on the index gate.

      What a replacement needs, so the next attempt starts from the measured requirement: record the
      **role** (filter vs projection) and a **selectivity bucket**, and populate it *after* results are
      known rather than at SQL-build time. Build it together with its consumer — either the promotion
      recommendation, or making the reactive Tantivy `RowSelection` gate predictive (skip the index
      search for keys known unselective; measured waste ~0.28 ms per segment when the gate declines,
      sized at ~1% of a 5.5 ms query, so that alone does not justify it).
      — *source: archetype-bench, 2026-08-08*

- [ ] **The metrics attribute gap: close it with `promote`, not with a metrics index.** Measured with
      `examples/bench --bin metricattr-bench` (20 segments x 5,000 points, 10 labels/point, best of 5).
      Metric tables get no `.tidx` (§8), so an arbitrary label matcher is a full JSON scan. The gap is
      **+26.0 ms over the `count(*)` floor at both selectivities** (33.4 ms vs a 7.4 ms floor), and it
      is the same 26 ms whether the matcher selects 50% or 1% of points — the JSON cost is per row
      scanned, not per row matched.

      **A promoted column recovers 95–97% of it, at every selectivity**: 33.4 → 8.1 ms at 50%
      selectivity, 33.7 → 8.8 ms at 1%. **Extending the `attrs` index to metric segments recovers the
      gap only when the matcher is selective**: at 1% the indexed `logs` path runs the same filter in
      6.2 ms, but at 50% the cost gate declines to prune and it recovers nothing (measured directly in
      `attr-bench`: the index search costs ~0.28 ms and saves 0 when it declines). PromQL selectors
      like `service="api"` in a single-service deployment are exactly the unselective case.

      So the index would cost a Tantivy build at seal on the **highest-volume signal** — which §8
      deliberately avoids — to help only selective filters on un-promoted keys, while promotion helps
      every filter on promoted keys with infrastructure that already exists. **Recommendation: do not
      build a metrics index.** Use `promote`, now that `imbh-attrstats` makes the key set choosable
      from data instead of guesswork. (`Db::attr_access_stats` was cut 2026-08-09 — it contributed to
      no push-down and conflated filters with projections; see the demand-signal item below.)

      Residual, and it is the honest limit: promotion is curated, so an *arbitrary* metric label still
      pays the +26 ms JSON scan (down from ~78 ms before the targeted extractor). Fully serving "any
      attribute" on metrics would need either the index or the still-sigma-gated segment bloom. Note
      the caveat on the cross-signal comparison: `logs` and `metrics_gauge` have different schemas, so
      the +4.1 ms by which the indexed logs path trailed metrics at 50% conflates table width with
      index cost — the clean index-cost number is the 0.28 ms from `attr-bench`.
      — *source: metricattr-bench, 2026-08-08*

- [ ] **The `CAST(col AS VARCHAR)` in `attr_field` is pure overhead for equality filters — measured.**
      A promoted key compiles to `CAST("k" AS VARCHAR)` so the fragment's Arrow type is the same
      whether or not the key is promoted; that matters for the *projection* sites (`… AS g{i}` group-by
      outputs, where dropping it would make a result column's type depend on the promote set) but not
      for filters. Measured with `attr-bench` at all four selectivities: `"k" = 'v0'` in dictionary
      space beats `CAST("k" AS VARCHAR) = 'v0'` by **0.20–0.31 ms** per 100k rows, consistently —
      *more* than the new `CASE` fallback costs (+0.04–0.11 ms). A predicate-level helper
      (`attr_eq_sql(key, value)` beside the value-level `attr_field`) emitting
      `("k" = $v OR ("k" IS NULL AND json_get_str(attributes,$k) = $v))` would be both correct and
      faster than what shipped before the fallback existed. This cannot come from `attr_field` itself:
      it returns a *value* expression, and a `CASE` whose arms are `Dictionary` and `Utf8` is coerced
      back to `Utf8`, reintroducing the cast. ~5% of a 5.5 ms query — real but small; do not extend it
      speculatively to the other matchers. — *source: attr-bench, 2026-08-08*

- [x] **A read-only handle used its own `promote`, not the writer's.** *(Closed 2026-08-08.)* The
      read-only `Storage` was built with the reader's *builder* config and nothing read the writer's
      set off disk, so reader and writer could disagree about the schema of the same segments. The
      `CASE` fallback made that *correct* but not *coherent*, and auto-promotion — where the writer's
      set changes at run time and a reader could never learn it — needed coherence.

      **The promoted set is now durable database state**, recorded in `db.info` (one escaped
      `promote\t<key>` line per key, temp→rename) and read at open:
      - omitting `DbBuilder::promote` **inherits** the database's set rather than resetting it to
        empty. An explicitly empty `Promote` still demotes everything — only *omitting* the call
        inherits, which is why the builder field became `Option<Promote>` internally. Without that
        distinction, every host that stopped passing its list would have silently demoted its DB.
      - a read-only handle adopts the durable set and ignores its own builder outright.
      - `Storage::promote()` returns an owned `Promote` (the set is mutable now, so handing out a
        reference would pin the lock), and `promote` moved behind an `RwLock`.

      **Trap worth keeping:** `Storage::open` calls `write_db_info`, which rewrote `db.info` with
      `Promote::default()` — so *opening* a promoted database erased the marker before the facade
      could read it, and a reader then saw no promoted columns at all. Open must carry the existing
      set through. Caught by the durability test, not by reasoning.
      — *source: design review, 2026-08-08; fixed 2026-08-08*

- [x] **A promote-set change must seal the buffer first.** *(Closed 2026-08-08.)* `concat_buffer`
      concatenates buffered batches against the **live** schema, and `concat_batches` takes columns
      *positionally* without validating them against the schema it is handed — so a buffer holding
      batches encoded on both sides of a promote change can panic (first batch wider), silently
      truncate (first batch narrower), or silently concatenate two differently-named promoted columns
      into one. That is the same hazard §6.1 records for compaction; making the set mutable would have
      reproduced it in the buffer.

      `Storage::set_promote` seals, then takes the `inner` lock, verifies every buffer is empty, and
      swaps under that lock. Correctness rests on ingest reading the promote set **beneath the same
      lock it appends under** (`push_log_batch(&mut inner, rows, &self.promote_keys())`), so once the
      swap holds `inner` with empty buffers, no encode can be in flight against the old set. A racing
      ingest between the seal and the lock costs another round; bounded at 8 attempts rather than
      spinning forever inside a public call. A no-op change short-circuits without sealing.

      Exposed as `Db::set_promote` / `BlockingDb::set_promote`, rejected on read-only handles. The
      regression test was verified to fail with the seal removed, rather than merely passing with it.
      — *source: design review, 2026-08-08; fixed 2026-08-08*

- [x] **Compaction baked an all-NULL promoted column into merged segments.** *(Closed 2026-08-09.)*
      `compact_partition` normalised every source batch to the live promote set with
      `coerce_to_schema`, which null-fills a column the segment predates. **Not a wrong answer** — the
      `CASE` fallback is immune, since a null-filled column takes the JSON arm exactly as an absent
      one would, which remains a decisive point in its favour over any scheme keyed on column
      absence. *(A backlog summary of mine on 2026-08-08 called this the last remaining wrong-answer
      bug. That was wrong; this entry always said otherwise.)*

      It was a **convergence** defect: compaction is the one operation that rewrites these rows, so
      null-filling there made the fallback permanent and every query on the key kept paying a JSON
      parse over that data for the life of the merged segment. `backfill_promoted` now projects the
      column from the retained `attributes` JSON via the same `build_promoted_columns` /
      `lookup_promoted` path seal uses, so a back-filled cell and a sealed-at-ingest cell cannot
      disagree. Only columns the *source* lacked are derived — a column the source had is kept, since
      a NULL there means the row genuinely carried no string value and re-deriving would spend a
      lookup to reproduce it.

      Two notes worth keeping. An existing storage test asserted the old `[None, None, Some]`
      behaviour and had to be updated — the null-fill was deliberate, not accidental. And the zip in
      `backfill_promoted` goes through `promoted_columns(missing)` rather than `missing` defensively;
      the misalignment it guards against looks **unreachable** today (`missing` holds only keys absent
      from the source schema, and the reserved names are the built-in columns, which every segment
      has), so the test written to catch it does not — verified by reintroducing the bad zip and
      watching the test still pass. The guard stays for the day the built-in column set changes.
      — *source: design review, 2026-08-08; fixed 2026-08-09*

- [ ] **`cargo release` still cannot be run as configured — six releases and counting.** Two
      independent defects in the root `Cargo.toml`, worked around by hand for v0.5.0, v0.6.0, v0.6.1,
      v0.6.2, v0.7.0 and v0.8.0 (JOURNAL "Preparing v0.6.0" / "Preparing v0.6.1"). (a) `pre-release-hook = ["git",
      "cliff", "-o", "CHANGELOG.md", …]` with no `cliff.toml` in the repo would replace the
      hand-written Keep a Changelog file — prose, migration notes, and the `<!-- next-url -->` anchors
      `crates/imbh/Cargo.toml` matches with `exactly = 1` — with a conventional-commit digest. Either
      commit a `cliff.toml` that reproduces the current file, or drop the hook and keep the
      `pre-release-replacements` mechanism that already works. (b) The `pre-release-replacements` for
      the `VERSION=` / `ghcr.io/…` strings live under `[workspace.metadata.release]`, where
      cargo-release does not read them; move them into `crates/imbh/Cargo.toml` alongside the
      changelog ones. Until both are fixed, "run `cargo release`" in `README.md` "Releasing" is
      advice that destroys the changelog.

- [ ] **Confirm `propagatedMount` survives `docker plugin upgrade`.** Persistence across
      `disable`/`enable` and destruction by `plugin rm` were both measured (JOURNAL 2026-08-06);
      `upgrade` was not, because it needs a registry round trip. It decides whether upgrading the
      plugin is a data-preserving operation, which the docs currently do not claim either way.

- [ ] **Measure whether a managed plugin accepts a `mounts` entry with a null/settable `source`.**
      Bridge-network discovery (JOURNAL 2026-08-07) has two backends; the Engine API one needs the
      daemon's socket inside the plugin's mount namespace, which the shipped `config.json`
      deliberately does not grant. moby models `PluginConfigMount.Source` as `*string`, which
      *suggests* a nil source is a declared-but-inactive mount an operator could turn on with
      `docker plugin set imbh dockersock.source=/var/run/docker.sock`. If that holds, API mode — and
      with it `container.network.*` attributes — becomes opt-in for the managed plugin with no
      default privilege change. If it does not, the current posture stands and the limitation stays
      documented. **Do not** hard-code `/var/run/docker.sock` as a mount source either way: under
      rootless Docker it is elsewhere, and a missing bind source fails `plugin enable` — the exact
      bug `tests/docker_plugin_config.rs` exists to prevent.

- [ ] **Measure whether the daemon's API socket is serving when a plugin is enabled at daemon boot.**
      Decides whether API-mode discovery can engage on a cold start or only on the first refresh
      afterwards. Not a correctness issue — the probe re-runs every refresh precisely because this is
      unknown — but it is what the operator guide should promise.

- [ ] **A remap script sees a pre-discovery `.resource`.** `Bound`'s resource seed is built once at
      `StartLogging`, so when a later network refresh swaps the container's resource
      (`Container::set_networks`), the *stored* record gets the new one but a script's `.resource`
      still shows the old. Harmless today — the built-in script never reads `.resource`, and
      `.info.networks` *is* refreshed per line — but it is a real inconsistency to fix before
      anything depends on it. (JOURNAL 2026-08-07.)

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

- [ ] **The shipped x86_64 `imbhd` is 3.7 MB OVER the §2 target — measured, not projected, and the
      local gate cannot see it.** *(projection 2026-08-06; **confirmed by measurement 2026-08-09**, so
      what is left is the decision.)* The projection below has been settled by the bytes GitHub is
      serving: `imbh-0.7.0-x86_64-unknown-linux-gnu.tar.gz` unpacks to an `imbhd` of **45,691,560 B =
      45.7 MB against the 42 MB §2 target**, well under the 55 MB hard limit. Against CD's v0.5.0
      x86_64 baseline of 41,112,104 B that is **+4,579,456 B (+4.37 MiB)** for `docker-remap`, versus
      the +4,024,312 B (+3.84 MiB) measured locally on aarch64 — so the item's own caveat held: x86_64
      codegen is fatter, and the real overage (3.7 MB) is larger than the 3.1 MB projected. No CD dry
      run is needed to establish this; **it has been shipping since v0.6.0**, the first release whose
      Linux legs carried VRL. This is therefore a standing overage rather than a v0.8.0 regression, and
      v0.8.0 will land ~0.2 MB above v0.7.0 (the local aarch64 `imbhd` moved 34,916,248 →
      35,112,856 B). The decision is unchanged and now unblocked: raise the §2 target to a number that
      ships, trim the VRL subtree, or move `docker-remap` to a separate artifact.

      Original projection: The
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

- [x] **`ARCHITECTURE.md` says the Tantivy `attrs` field is not built. It is.** *(closed 2026-08-08 —
      §6.1 item 3, the §8 schema list + deviation block, and the §9.2 pushdown contract were all
      rewritten against the code; the `imbh-index` crate docs were already correct. Fixing §9.2
      surfaced two further inaccuracies in the same list, handled as described in the follow-up item
      below.)* §6.1 item 3 and the
      §8 deviation block both state that the original design's Tantivy JSON field for attributes was
      never implemented, so "attribute predicates do not push into Tantivy — they run as UDF scans
      over surviving rows". That is false as of the current tree: `crates/imbh-index/src/lib.rs`
      adds an `attrs` JSON field (raw tokenizer, indexed as `attrs.<key> = <value>`, string values
      only) and exposes `search_attr_eq`, and `crates/imbh-query/src/provider.rs:199`
      (`has_attr_index()` + `attr_eq_predicate`) pushes attribute equality down as `Inexact`,
      resolving it to a cost-gated `RowSelection` through the same row-ordinal bridge as `matches`.
      `has_attr_index()` is `text_column.is_some()`, so the pushdown covers `logs`/`spans` and
      metric tables correctly fall through to a scan. Index-vs-`json_get_str` agreement is pinned by
      `attr_eq_index_matches_rowwise_fallback`. Fix the two prose sites; also check §9.2's pushdown
      contract, which lists "attribute equality (no Tantivy attrs field)" under **Unsupported** and
      is stale for the same reason, and the `imbh-index` crate docs if they disagree. Worth doing
      because these three sites are the canonical design reference: a design discussion on
      2026-08-08 (whether to migrate the backing store to Tantivy, Quickwit-style) reached the wrong
      recommendation from them, proposing as new work something the tree already ships. While
      there, confirm whether the LTM notes (`full-text-search-tantivy-bridge.md`,
      `query-engine-and-typed-apis.md`) carry the same stale claim. *(The LTM check is the one part
      not done — the two notes were not read.)*

- [x] **Trace search defeated its own bloom filters.** *(Closed 2026-08-08.)* `TracesApi::search`
      phase 2 issued `WHERE hex(trace_id) IN (…)`, a shape `bloom_id_eq` explicitly excludes (it never
      yields the raw id bytes a bloom needs), and the extractor only matched `Operator::Eq`, never
      `Expr::InList` — so the most bloom-friendly predicate in the system read every span segment in
      the DB. Fixed on both sides: phase 2 now binds raw `trace_id` bytes
      (`SqlParams::id_bytes` → `ScalarValue::FixedSizeBinary`), and the provider's new `bloom_probe`
      treats a probe as a *candidate set*, skipping a segment only when the blooms prove every
      candidate absent. **The load-bearing discovery**: DataFusion's `ShortenInListSimplifier` rewrites
      an `IN` of ≤ 3 values into an `OR` chain, and `SimplifyExpressions` runs *before* `PushDownFilter`
      — so handling `Expr::InList` alone would have silently done nothing for the common small-k case.
      The `OR` shape is handled too, verified with both 2-value and 4-value queries. Measured: phase-2
      fetch over 3 traces in 3 bloom-carrying segments → 2 pruned, 1 scanned, identical rows to the old
      query (old form measured 0 pruned, 3 scanned in the same test). Phase 1 deliberately unchanged —
      its id set is a subquery that DataFusion decorrelates, so the outer scan carries no id predicate
      for a bloom to probe. No public API change. — *source: design review + implementation, 2026-08-08*

- [x] **Reconsider option (a) — manifest `min/max_time` pruning before the file is opened.**
      *(Closed 2026-08-08 — implemented, and it recovered almost exactly what the measurement
      predicted.)* `SegmentInput` gained `time_range: Option<(i64, i64)>` (manifest bounds, inclusive)
      and `TableInput` gained `time_column: Option<&'static str>` naming the column those bounds
      describe; the scan loop tests pushed range probes against the declared bounds **before**
      `row_selection_for` or `open_segment`, so an excluded segment costs no `File::open`, no footer
      read, and no `.tidx` search. The `time_column` guard is load-bearing: without it a range
      predicate on another INT64 column (`duration_ns`) would be tested against time bounds and skip
      segments that do match. Same bench, same host: the 1-of-60 window went **2.09 ms → 0.73 ms**, a
      further 2.9x on top of option (b) and **11.9x against the HEAD baseline of 8.71 ms**. Both
      mechanisms are kept — the footer-statistics path still handles segments with no declared range
      and still does the row-group narrowing. **Semver: this is a breaking change** to the published
      `imbh-query` (two new public fields on structs with public fields, plus an argument on
      `SegmentTableProvider::new`). Under Cargo's 0.x rules that requires a minor bump, which the
      canonical-JSON item already puts on the table for the next minor — it rides along at no extra cost, but
      it must not ship as a patch. — *source: prune-bench A/B + implementation, 2026-08-08*

      Original finding: It was
      declined during the time-pruning work as a second-order win needing a breaking `SegmentInput`
      change. **Measurement moved it.** `examples/bench --bin prune-bench`, 60 segments x 2,000 rows,
      best of 5, A/B against a pristine HEAD worktree: a 1-of-60-segment time window went
      **8.71 ms → 2.09 ms (4.2x)**. But a 60x reduction in rows read bought only 4.2x, because option
      (b) still opens all 60 Parquet footers — the residual ~2 ms is ~35 us per segment of footer I/O
      and is now the dominant cost of a narrow query. Corroborated by the trace path landing at the
      same floor: a pruned point lookup is 2.12–2.36 ms over the same 60 segments. Option (a) attacks
      exactly that residual and would compound with (b), not replace it. Re-cost it against the
      `SegmentInput` break now that the payoff is a measured number rather than a guess.
      — *source: prune-bench A/B, 2026-08-08*

- [x] **`attr-stats`'s `hint` column collapsed two independent axes.** *(Closed 2026-08-08.)* On the
      `prune-bench` corpus `shard` has 60 distinct values and sigma **0.017** (= 1/60 exactly, every
      value in the lowest histogram bucket) — and the classifier hinted `promote`, because 60 values
      reads as low cardinality. Both readings are defensible and that was the problem: cardinality
      and sigma are orthogonal, so `shard` wants a promoted column (fast filtering) *and* a segment
      index (pruning), and one `hint` produced by an if/else chain could only ever say one.

      Now two independent columns, `promote` and `index@`, and the split fixed a second defect on the
      way. **The promotion gate no longer looks at cardinality at all.** It was `distinct_est <= 256`,
      which the `promote-cost` correction above had already shown to be the wrong axis. It became
      `postings / rows` the same day, and then — after `archetype-bench` found that gate mis-ranking
      keys by up to 7x — the two-term `est B/row` model recorded above. Below
      `PROMOTE_MIN_ROWS_PER_SEGMENT` rows/segment the verdict is `-` rather than a guess, so the
      classifier declines to blame a key for the corpus; and the estimate is normalised **per row**
      because an absolute per-segment threshold calls every key in a short segment cheap (caught by
      the regression test below, which rejected the first threshold immediately).

      `the_promotion_verdict_follows_repetition_not_cardinality` pins the finding as behaviour: over
      6 segments x 200 rows, `env` (1 distinct value) and `pod` (6, a fresh one per segment) have
      **identical** postings, repetition and verdict — 6x apart in cardinality, same cost — while
      `req_id` (1,200, unique per row) is `costly`. And the two verdicts point opposite ways for
      `pod`: `promote yes`, `index@ all`.

      **`index@` reports a scale, not a yes/no**, which is what the window ladder made possible. A
      segment index prunes `1 - sigma`, and sigma depends on the range queried: a value can occupy a
      tiny fraction of a day's segments and all of the segments in the minute it appeared. So the
      column names the widest rung whose mean sigma is still <= 0.25 (`all` when even the whole scan
      qualifies, `-` when no rung does). `sigma(w) = C(seg)/C(w)` falls straight out of the ladder's
      definitions, with `sigma(all) = 1/locality`. Note it is a **mean**, while section 1's
      `p50`/histogram give the distribution at segment scale — both are printed, and a key whose mean
      and median disagree is one whose values differ a lot from each other.
      — *source: prune-bench + attr-stats, 2026-08-08; split and re-gated 2026-08-08*

- [ ] **Point `examples/attr-stats` at a production database.** *(Scope reduced 2026-08-08 — see
      `archetype-bench`. The decision rule is now validated against seven attribute archetypes that
      bound the space of shapes real telemetry occupies, so the open question is no longer "what does
      the data look like" but "which of these archetypes are present, and in what proportion" — which
      a handful of parameters answers without the data leaving anywhere. Two are already known from
      the operator: request- and session-scoped identifiers do appear as attributes, and concurrent
      pods number under 50. Note `attr-stats` output contains no attribute values at all — the
      bottom-k sketch only ever hashes them — so what it emits is key names plus aggregates.
      **Reach extended 2026-08-09:** the measurement is now the `imbh-attrstats` crate rather than an
      example binary, so it can be read off a *running* `imbhd` (`POST /api/head/attributes/stats`) or
      off the TUI Overview's attribute block — no CLI to ship to the machine holding the data, and no
      database directory to hand over.)* The tool is built and green
      (`cargo run -p attr-stats -- <db-dir> [--scope all|attributes] [--last <min>] [--windows 1m,1h,24h]
      [--top N] [--json]`),
      and it is the input to *two* open decisions: whether a segment-granularity attribute index is
      worth building, and whether `promote` could be chosen automatically. It reports, per table and
      per key, distinct-value count, `postings` (the `(key, value, segment)` entries such an index
      would store), the sigma distribution (p50/p90/max/mean, fraction ≤ 0.25, 10-bucket histogram),
      and — restricted to record-`attributes` string values, matching `lookup_promoted` exactly — the
      promotion-candidate stats. **Run on `gen-demo-db` every sigma is exactly 1.000**, for all 19 keys
      across 5 tables, with `frac ≤ 0.25` at 0.00 everywhere. That is the signature of synthetic data,
      not a finding: the generator emits a fixed label set every step and each run flushes one segment,
      so "every value in every segment" is the generator author's choice. The demo DB also contains no
      high-cardinality attribute at all (trace/span ids are columns, not attributes), so it never even
      exercises the sampling path. **Both design questions therefore remain open**, and closing them
      needs this pointed at data with real pod names, build ids, or customer ids. — *source: design
      review + implementation, 2026-08-08*

- [ ] **Decide whether the cardinality curve becomes a *persisted* statistic.** `attr-stats` now
      measures cardinality at a ladder of window widths, not just at the segment (`--windows`,
      default `1m,1h,24h`; section 2 of the report). The motivation is that sigma answers the pruning
      question at exactly one granularity, and the same key can be localized against a day and
      interleaved against a minute — so `C(w)`, the mean distinct values within one window of width
      `w`, is a curve whose *shape* is the answer. Flat (`loc = C(all)/C(seg) ~ 1`) means every
      segment already holds every value and nothing prunes at any scale; rising means values churn,
      and the width where it flattens is the horizon beyond which segment pruning stops paying. The
      report also emits `rep` (`rows / postings`, in-segment repetition), which is the number the
      promoted-column cost measurement identified as the real driver, so the two live side by side.
      **What is still open is where this lives.** Today it is offline and after-the-fact: one pass
      over sealed Parquet, no manifest change, no cost at seal. Making it a statistic the *database*
      keeps means per-segment state at seal — and the wider rungs cannot be per-segment by
      construction, since they aggregate across segments. The shape that works is a **mergeable
      per-`(segment, key)` sketch**, folded at read time to get any window: retention then drops a
      segment's sketch with the segment, and no bucket is stored twice.

      **The merge property was measured, found to half-hold, and then made to hold.**

      As originally written, `SampledMap` bounded itself by adaptive halving: retain
      `hash(k) <= u64::MAX >> shift`, and raise `shift` whenever the map is full. Folding was
      *sound* under that scheme (complete counters, a valid sample, the cap honoured) but could not
      be *exact* — because there was no single direct-scan answer to be exact to. The scan itself
      was order-dependent: it halved whenever the map was full **at the moment a key arrived**, so
      different arrival orders reached different rates, kept different keys, and reported different
      `estimated_total`s over identical input. An exhaustive permutation search over 4,800
      (key-family, n, cap) combinations found 60,012 permutation pairs disagreeing on the *estimate*.
      That was a property of the already-shipped accumulator, not of merging: `distinct`/`postings`
      were reproducible only while the caps were disengaged — i.e. exactly when they print without
      the `~` marker.

      **`SampledMap` is now a bottom-k sketch**: a single `BTreeMap<u64, V>` keyed by the hash of
      the entry's name, which is the lookup structure and the order structure at once (`pop_last` is
      the eviction). The retained set is a pure function of the name *set*,
      so all three properties now hold and are pinned by tests in `accum`:
      - counters are complete, never partial (a key in the final bottom-k was in the bottom-k of
        every prefix containing it, so it was admitted on first sight and never evicted);
      - the sample is independent of arrival order — 78,300 permutations, zero disagreements, on
        the same sweep that previously found tens of thousands;
      - **folding is exact**, above the cap as well as below: a 3-part fold and a single pass agree
        on keys, counters, rate, and estimate. So a persisted per-`(segment, key)` sketch is a
        viable basis for the ladder, and retention drops a segment's statistics with the segment.

      Two traps worth keeping. (1) The predecessor's `shrink()` cut at `len >= cap` because the scan
      path called it *before* inserting one more key; a merge inserts nothing afterwards, so reusing
      it cut one rate further and silently halved the sample. (2) In the bottom-k version, eviction
      and refusal are both drops — marking only refusals leaves a map that evicted freely still
      reporting itself exact. Both were caught by tests that failed loudly, the second by the
      order-dependence test continuing to pass when it should have started failing. (3) The first
      cut kept a `HashMap<Rc<str>, V>` beside a `BTreeSet<(u64, Rc<str>)>` — a shadow index holding
      an entry for every key, 1:1 with the map, to answer only "what is the maximum", and hashing
      every name twice per lookup (SipHash for the probe, xxh3 for the predicate). Keying by hash
      collapsed both containers into one, dropped ~88 bytes of identity per value entry to 8, and
      removed `MAX_VALUE_BYTES` digest folding entirely — value text is never read back, so it is
      never stored.

      **A persisted sketch would also need a run counter.** The fold described here carries value
      *presence* per segment, which is what sigma and the cardinality ladder need — but the dominant
      term in promoted-column cost turned out to be value *order* within a segment (see the
      `archetype-bench` correction above), and order is not something a set union preserves. A
      per-segment `runs` tally is additive across segments and would ride alongside the sketch, but
      it has to be designed in rather than derived from it.

      `SampledMap::merge` remains `#[cfg(test)]`: it establishes the property, and nothing in the
      tool calls it yet. What is still open is the persistence itself — per-segment sketches written
      at seal, and the manifest/retention plumbing. Do not size that before the previous item (real
      data): on synthetic corpora every curve is flat by construction, exactly as every sigma is
      1.000.
      — *source: window ladder + merge verification + bottom-k conversion, 2026-08-08*

- [x] **`Db::segment_files()` returned empty for read-only handles.** *(Closed 2026-08-08.)*
      `Storage::open_read_only` leaves the in-RAM segment lists unpopulated (a reader derives its
      view per query), so the accessor silently reported no segments for a fully populated database —
      `[]` is indistinguishable from "this table has no segments", so a host handing the paths to
      DuckDB `read_parquet` (the documented use, §10.11) got no data and no error.

      Fixed by deriving from `read_disk_snapshot` on read-only handles — the same source the reader's
      query path already uses — and by making the accessor **fallible**: `Db::segment_files` now
      returns `Result<Vec<PathBuf>>`. Breaking for the `imbh` facade, taken in the window after v0.7.0
      (which shipped without it — the change is on the unmerged housekeeping branch),
      and the signature is the point: a reader must be able to report an I/O failure rather than mask
      it as an empty database, which the infallible form could not express. `BlockingDb` gained the
      mirroring `segment_files` it had been missing.

      **Scope correction.** This was initially scoped as a *family* of accessors on the theory that
      anything reading `inner.segments` was affected — `stats()`, `snapshot()`, the
      retention/compaction scans. Reading the callers rather than the field shows otherwise:
      `Db::stats` already branches to `reader_stats()` (which uses `read_disk_snapshot`) and
      `Db::snapshot` already calls `ensure_writable()?`, so it refuses explicitly instead of
      returning something wrong. `segment_files` was the only silent one. Lifting the writer-only
      restriction on `snapshot()` for readers remains open, but it is a feature, not a defect — the
      code comment marks it "for now".

      Regression test `read_only_segment_files_sees_the_writers_segments` covers both handles across
      two seals plus the empty-table cases, and was verified to fail against the old behaviour
      (`[]` vs the writer's one path) rather than merely passing against the new one.
      — *source: attr-stats implementation, 2026-08-08; fixed 2026-08-08*

- [x] **The `housekeeper` feature gated the binary, not the merge machinery.** *(Closed 2026-08-09.)*
      There is now a `compaction` feature (on by default, like the other footprint levers) gating
      `Storage::compact`, `compact_partition`, `rewrite_segment_set`, `prepare_pending` and their
      helpers; `commit_pending` stays in every build, because applying a record is cheap bookkeeping
      an embedded host still wants. `housekeeper` implies `compaction`.

      **Measured, and the design note oversold it.** That note listed "Parquet write, Tantivy build,
      sort, JSON projection" as machinery the host would stop carrying; two of those are wrong, since
      **seal** already writes Parquet and builds the sidecar. Dropping `compaction` removes **no
      dependency at all** — 381 crates either way — and 110,434 B of code, 2.5% of `libimbh.rlib` in
      release. The gate still earns its place as an API-surface guarantee (a host that does not link
      the rewrite cannot start an unbounded one on its own thread), which is what it is now claimed to
      deliver. Corrected in the note and in the feature's own comment.
      — *source: housekeeper implementation, 2026-08-09; closed 2026-08-09*

- [x] **An embedded host that never called `maintain()` never picked up pending records.**
      *(Closed 2026-08-09.)* The default `Maintenance::Manual` means a host may never call
      `maintain()`, so a housekeeper's work sat on disk indefinitely — and the preparer, seeing
      nothing land, re-prepared the same partitions on every pass, burning IO forever.

      Now committed at **`open()`** (after the promoted set is applied — records validate against it,
      so committing earlier would discard every one as stale) and at **`close()`** (swallowing any
      failure, which leaves the records for the next open, exactly the state a crash would produce).
      One `read_dir` of a usually-empty directory, on paths already doing recovery work.

      Note the ordering that makes the restart test meaningful: `cleanup_orphans` runs inside
      `Storage::open` and the commit runs after it, so a merged result *proves* cleanup respected the
      pending record — had it swept the output, the commit would have failed its digest check.
      — *source: housekeeper implementation, 2026-08-09; closed 2026-08-09*

- [ ] **Housekeeping handoff: the questions left open when the design was folded into ARCHITECTURE §7.2.**
      *(From the retired `COMPACTION_HANDOFF.md`, 2026-08-09.)*
      1. The pending record lives in `pending/` as its own file. A manifest frame type would reuse the
         framing and replay path, at the cost of putting non-committed state in the committed log. The
         current choice was made for simplicity, not after weighing that.
      2. No lease stops two housekeepers duplicating work. A `maintain.lock` is cheap and would not
         block the writer. Not a correctness requirement — duplicate work is discarded at commit, as
         the end-to-end test shows — so it is an optimisation.
      3. `append_frame` uses `write_all`, which loops on a short write. `O_APPEND` makes a *single*
         `write` atomic against other appenders; a looping one does not. Not a live bug (the manifest
         has one mutator), but any future multi-mutator design must make it a guarantee rather than an
         observation.
      — *source: design note consolidation, 2026-08-09*

- [ ] **Auto-promotion policy, for when it is built.** *(From the retired `AUTO_PROMOTION_PLAN.md`,
      2026-08-09 — the rest of that plan is superseded: its `coalesce` rejection was refuted, its
      correctness constraint fixed by the `CASE` fallback, its "unmeasured prerequisite" measured, its
      promotion-epoch design rejected because re-promotion makes a key's validity a set of intervals,
      and its compaction back-fill shipped.)* What survives is policy, none of it implemented:
      - **Slow to promote, willing to demote.** Demotion is correctness-free — the key never left the
        JSON blob — while promotion is the direction that needed the read-side fallback. That is the
        opposite of the usual cache intuition.
      - **Hysteresis.** Promotion is a schema change; flapping multiplies distinct segment schemas and
        stresses `coerce` and compaction normalisation. Require sustained evidence, and rate-limit to
        at most one change per compaction cycle.
      - **Manual override stays authoritative.** `DbBuilder::promote` pins its keys; automation may
        only add, never remove. Hosts that know their data should not be second-guessed.
      - **Kill criteria**, worth checking before building rather than after: access counts turning out
        flat (no small key set captures the benefit), or a promoted column proving expensive enough
        that the budget shrinks to a handful of keys and hand-authoring is fine.
      - **Out of scope by construction:** promoting non-string values or `resource`/`scope` keys
        (`lookup_promoted` is record-scope, `AnyValue::Str` only, and the fallback's soundness depends
        on it); and dropping a promoted key from the JSON blob — promotion is a *projection*, not a
        relocation, and every fallback path depends on the JSON still being there.
      — *source: design note consolidation, 2026-08-09*

- [ ] **Pacing is only `--max-jobs`.** Convergence is triggered by schema lag, so the first pass after
      a `set_promote` makes **every** segment lacking the new column eligible. `--max-jobs` bounds one
      pass but nothing orders the work — oldest-first, or smallest-first, would both be defensible and
      neither is implemented; the planner takes tables and days in iteration order. Also worth deciding
      whether in-process `Db::compact()` needs a cap of its own, since it has none.
      — *source: housekeeper implementation, 2026-08-09*

- [ ] **`Compression` is still per-handle config, unlike `promote` and `retention`.** The housekeeper
      writes its output with `Compression::default()`, which may differ from what the host writes.
      Segments are self-describing so this is *correct* — it costs a size difference and nothing more,
      which is what the binary's comment says — but it is the same incoherence that made a reader and
      writer disagree about promoted columns, and the same fix applies (persist it in `db.info`, let
      omitting the builder call inherit). Lower stakes than the other two, hence not done with them.
      — *source: housekeeper implementation, 2026-08-09*

- [ ] **`DbStats` cannot gain a field without a breaking change.** `crates/imbh/src/lib.rs:1892` is a
      plain `#[derive(Debug, Clone)]` struct with no `#[non_exhaustive]` and no `Default` — that file
      has zero `#[non_exhaustive]` attributes anywhere. Adding any field breaks every downstream struct
      literal across 12 published crates. This is why the sigma tooling landed as an example binary
      rather than a `Db` stats API. If telemetry surfaces are ever expected to grow, `#[non_exhaustive]`
      on the stats structs is a one-time breaking change worth making deliberately at the next major,
      instead of paying it per field forever. — *source: attr-stats implementation, 2026-08-08*

- [x] **Promoting a key makes its equality filter prune *less* — measured, and it does not matter.**
      *(Closed 2026-08-08 as WONTFIX. `examples/bench --bin attr-bench`.)* The mechanism claim was
      right and the performance claim built on it was wrong. Measured 20 segments x 5,000 rows, best
      of 5, no time predicate, on a DB with `promote(["k"])` so both spellings exist per row; the
      pruning component isolated by re-running with `SELECTIVITY_THRESHOLD = 0.0`. The promoted
      column beats the *indexed* json path at 50% (5.47 vs 24.10 ms), 10% (5.53 vs 10.47) and 1%
      (5.68 vs 6.13) selectivity, and loses only at 0.1% by 0.35 ms. Reason: index pruning is worth
      11–16 ms on the json path almost entirely by **avoiding the JSON parse** on non-matching rows —
      a promoted column has no JSON parse, because removing it is what promotion already did. The
      promoted path sits within 0.13–0.55 ms of the bare `count(*)` floor at every selectivity, so
      that is the whole budget the push-down could recover: under 8%, and nothing at all at 1%. The
      only regime where it would help needs ~1,000 distinct values on one key — a high-cardinality
      key, which §6.1 says not to promote in the first place. Plan folded into
      JOURNAL.md "Segment pruning: three stale doc claims, 11.9x on a time-bounded query, and a plan measured into the bin" (2026-08-08), which records the numbers and the retraction. — *source: attr-bench, 2026-08-08*

      Original claim: `SqlParams::attr_field`
      (`crates/imbh/src/sql.rs:69`) emits `CAST("k" AS VARCHAR) = $N` for a promoted key, a shape
      `attr_eq_predicate` (`crates/imbh-query/src/provider.rs:653`) does not recognize — so it reaches
      the provider as `Unsupported` and the segment is read in full, while the *same key un-promoted*
      pushes into `search_attr_eq` and prunes. Promotion currently trades a JSON parse for a
      dictionary decode but gives up row pruning, which is the opposite of what it advertises. The fix
      is confined to predicate recognition plus threading the promote set into the provider; the
      `attrs` index already carries the term, since a promoted key stays in the JSON blob. Plan
      written up (since folded into JOURNAL.md, 2026-08-08). **Blocked on
      the doc fix below** — §6.1 item 2's claim that promoted columns merge
      `attributes` → `resource` → `scope` would make the push-down unsound if it were true.
      — *source: design review, 2026-08-08*

- [x] **`ARCHITECTURE.md` §6.1 item 2 states the wrong scope for promoted columns.** *(Closed
      2026-08-08 — item 2 now states record-`attributes`-scope-only, matching `lookup_promoted`, and
      records why the distinction is load-bearing.)* It says each
      promoted column is "materialized at buffer-encode time from the row's canonical-JSON scopes with
      record `attributes` → `resource` → `scope` precedence". The code does no such merge:
      `lookup_promoted` (`crates/imbh-storage/src/lib.rs:2108`) reads the **record `attributes` scope
      only**, and its own doc comment says resource and scope are deliberately excluded because "a
      record-attribute predicate must not see a resource value". §6.1's own pushdown-dispatch
      paragraph, a few lines later, contradicts item 2 and matches the code ("the column mirrors the
      record `attributes` scope only"), as does `crates/imbh/src/sql.rs:64`. Correct item 2. This is
      not cosmetic: the record-scope-only rule is exactly what makes the promoted column and the
      Tantivy `attrs` field describe the same row set, which is the correctness premise of the
      push-down item above. — *source: design review, 2026-08-08*

- [x] **Time-range pruning does not happen at all. Confirmed 2026-08-08 — and fixed the same day.**
      *(Closed 2026-08-08.)* Fixed via option (b): `supports_filters_pushdown` now claims INT64-domain
      comparisons `Inexact` and the per-segment reader tests them against the Parquet row-group
      statistics already in the footer it reads. Option (a) — manifest `min/max_time` pruning before
      the file is opened — was **not** taken: `open_segment` already reads every footer
      unconditionally, so (b) costs zero extra I/O relative to the status quo, while (a) would need a
      breaking change to the published `imbh_query::SegmentInput` for the marginal win of skipping
      that footer read. (b) also covers raw `db.sql()`, which (a) structurally cannot. Measured
      end-to-end through the typed path: `LogQuery::range(200..400)` over three sealed segments →
      `segments_scanned == 1`, `segments_pruned == 2`, where `segments_pruned` was structurally 0 for
      `logs` at any range width before. No public API change. Caveat recorded in ARCHITECTURE.md §9.2:
      the writer emits one row group per segment, so row-group *narrowing* is dormant in practice and
      the whole-segment skip is the entire win; page-index pruning remains unimplemented.
      — *source: design review + implementation, 2026-08-08*

      Original finding: Three independent reads all
      say the same thing: `supports_filters_pushdown` never claims a time predicate, so it never
      reaches `scan()`; the per-segment reader (`crates/imbh-query/src/provider.rs:736`) applies only
      the bloom whole-segment skip and the Tantivy `RowSelection`, never a row-group filter built from
      column statistics; and `Storage::query_snapshot` (`crates/imbh-storage/src/lib.rs:886`) takes **no
      time argument** — it hands back every segment of every table, and `writer_tables` passes them
      all to the provider. `SegmentRef` carries `min_time_unix_nano`/`max_time_unix_nano`
      (`crates/imbh-core/src/manifest.rs:12`) and nothing consults them on the query path. So a
      `WHERE time > now() - 5m` query over a DB holding 30 days of data opens and reads all 30 days of
      segments, and the `FilterExec` above the scan discards the rest. Correct, but the manifest data
      needed to skip them is already in memory. Two fixes, independent: (a) filter segments by
      `min/max_time` against the query's bound before building `TableInput` — needs the bound threaded
      into the snapshot; (b) claim time predicates in `supports_filters_pushdown` and hand them to the
      Parquet reader so row-group statistics and the page index (already written — §7 says page index
      on) do intra-segment pruning. (a) is the larger win and the smaller change. This is likely the
      single biggest read-path performance item in the tree, and it dominates any new index work — see
      the segment-granularity discussion in JOURNAL.md "Segment pruning: three stale doc claims, 11.9x on a time-bounded query, and a plan measured into the bin" (2026-08-08).
      — *source: design review, 2026-08-08*

      Original question: Fixing §9.2 (item above) found that
      `supports_filters_pushdown` claims no time-range predicate at all — the four shapes it claims
      are `matches` / `NOT matches`, the `attrs` equality, and bloom id equality, all `Inexact`. The
      old §9.2 text claimed time-range as **`Exact`**, "segment prune via manifest, row-group/page
      prune via Parquet stats". Since DataFusion hands `scan()` only the filters a provider claims, a
      time bound cannot be reaching the provider by that route, and no `min_time`/`max_time` predicate
      pruning turned up in `imbh-query`, `imbh-storage`, or the facade on a grep. Yet §7
      (Partitioning) describes range-based pruning as real, and the typed APIs all carry time bounds,
      so *something* is presumably narrowing the segment set. Trace the real path and write it down;
      if the answer is that predicate-driven time pruning is simply absent and every query reads every
      segment, that is a performance finding worth its own item, not just a doc edit. §9.2 currently
      says the mechanism is unconfirmed and points here. Also settle whether the `Exact` vs `Inexact`
      distinction was ever intended: nothing in the tree returns `Exact`, and the
      re-check-above-the-scan invariant that makes every pushdown a pure accelerator depends on that
      staying true. — *source: ARCHITECTURE.md §9.2 rewrite, 2026-08-08*

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

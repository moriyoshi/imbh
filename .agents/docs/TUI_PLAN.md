# Companion TUI plan

> **Status entries live in the journal and LTM, not here.** This file is the *plan*; the
> implementation history it accumulated (2026-07-21 status, 2026-07-23 interaction follow-up,
> 2026-07-30 trace detail, 2026-08-01 sticky waterfall) has been folded into
> [JOURNAL.md](./JOURNAL.md) and consolidated in
> [LTM/imbh-tui-and-gen-demo-db.md](./LTM/imbh-tui-and-gen-demo-db.md), which is the current
> description of what `imbh-tui` actually does. Sections 1-10 below are the original plan and are
> kept as the record of intent; where they disagree with the code, the LTM document wins.

> **Prerequisites:** first complete semantic conformance S0-S5 in
> [QUERY_SEMANTICS_CONFORMANCE_PLAN.md](./QUERY_SEMANTICS_CONFORMANCE_PLAN.md), then complete the
> P1/L1/T1 translator track in
> [QUERY_LANGUAGE_TRANSLATORS_PLAN.md](./QUERY_LANGUAGE_TRANSLATORS_PLAN.md). The TUI is not
> implementation-ready until both gates pass.

## 1. Outcome

Build an optional companion terminal application for exploring an IMBH database:

- metric charts and current values;
- trace search, trace details, and a client-side waterfall;
- log search, paging, record details, and periodic snapshot refresh;
- count and rate charts synthesized at query time from matching logs;
- drill-down links between metric exemplars, traces, spans, and logs.

Each signal screen accepts its familiar query language in addition to structured controls. The
semantic conformance and translator layers are independent of the terminal and land first; the TUI
consumes conformant models and diagnostics rather than parsing query languages itself.

The TUI is a host of the `imbh` library, not a new IMBH subsystem. It should live in a top-tier
workspace crate, tentatively `crates/imbh-tui`, with a binary named `imbh-tui`. It may depend on
`imbh`, but no core, storage, index, or query crate may depend on it. Terminal dependencies must not
enter the `imbh` or `imbhd` dependency graphs.

The first supported data source is a local database directory opened with
`Db::open_read_only`. This lets the TUI run beside a writer without owning the writer lock and gives
each query a point-in-time view of sealed segments plus the live WAL tail. An in-process entry point
such as `imbh_tui::run(Arc<Db>, Options)` can follow after the binary is stable. Remote operation is
deferred until there is a typed query transport; `imbhd` currently exposes SQL but not the typed log,
trace, and metric APIs.

## 2. Product boundaries

The first release should be an explorer, not a dashboarding or alerting system.

In scope:

- one database at a time;
- a shared absolute or relative time range and refresh interval;
- interactive filtering, series selection, zooming, tables, details, and cross-signal drill-down;
- bounded result sets and bounded chart resolution;
- query-time log-derived count and rate series;
- read-only operation by default.

Out of scope initially:

- ingestion, retention, compaction, or other mutating administration;
- alerts, recording rules, or persisted derived metrics;
- constructs outside the P1, L1, and T1 translator compatibility profiles;
- remote authentication and transport;
- a lossless live log tail;
- arbitrary dashboard layouts or a plugin system.

Not persisting synthesized metrics is deliberate. A log-derived series is a view over retained logs,
so it always follows the selected time range and does not create a second lifecycle, WAL stream, or
retention policy.

## 3. User experience

Use a stable shell on every screen:

- top bar: database, connection state, selected time range, refresh countdown, and paused/running
  state;
- left or top navigation: Overview, Metrics, Traces, Logs, and Help;
- main pane: chart or result table;
- optional detail pane: selected series, span, trace, or log fields;
- bottom bar: context-sensitive key hints and the latest non-fatal error.

Global interactions should include quit, help, screen switch, focus switch, refresh, pause, time-range
selection, time pan/zoom, filter editor, and command palette. Avoid assigning essential actions only
to function keys. Provide an ASCII rendering option for terminals that do not display Unicode or
braille charts reliably.

### 3.1 Overview

Show database health from `db.stats()`, recent log volume, recent span RED summaries, and a small set
of user-selected metrics. This is a compact launch screen, not a configurable dashboard engine.

### 3.2 Metrics

Use `metrics().catalog()` for metric selection and `metrics().series()` for label discovery. Build
queries with `MetricQuery`, `HistogramQuery`, or `ExpHistogramQuery` according to the selected kind.
The screen should provide:

- line charts for range results and a table for latest values;
- aggregation, group-by labels, label filters, rate mode, quantile, step, and time range controls;
- legend selection and hiding when cardinality is high;
- unit-aware formatting without silently rescaling the underlying values;
- exemplar markers, with Enter opening `traces().get(trace_id)` when a trace id is present.

Cap returned and rendered series. If a query exceeds the cap, report truncation and require the user
to narrow filters rather than selecting an arbitrary invisible subset.

### 3.3 Traces

Use `traces().search(TraceQuery)` for the result list and `traces().get(trace_id)` for details. The
detail view constructs a tree from `parent_span_id` and lays spans out against the trace start and
duration. Orphans and cycles must be displayed as malformed roots rather than dropped.

The trace screen should expose service, operation, status, duration, text, attribute, and time
filters. The detail pane should show status, duration, attributes, resource, scope, events, and links.
Selecting a span should be able to open logs correlated by trace id and optionally span id once the
typed log filter supports those fields.

### 3.4 Logs

Use `logs().query(LogQuery)` for bounded pages, plus `count`, `volume`, and `volume_by` for summary
information. The screen should provide:

- service, minimum severity, text, attribute, and time filters;
- a compact list with time, severity, service, and a single-line body preview;
- a detail pane with the complete body, attributes, resource, scope, trace id, and span id;
- explicit older/newer paging;
- trace drill-down when a log carries a trace id;
- scan statistics so expensive full scans are visible to the user.

The initial refresh action re-runs a point-in-time query. It must not be called `tail` or promise
exactly-once delivery. The current offset cursor can move under concurrent inserts, and the public API
has no streaming tail. A real Follow mode depends on the API work in section 7.

### 3.5 Metrics synthesized from logs

Use the conformant `LogMetricQuery` introduced by the semantics prerequisite as the executable model.
The TUI may wrap it with presentation-only state:

```text
LogMetricRecipe
  title
  query: LogMetricQuery
```

The structured editor's `LogFilterState` compiles into `LogMetricQuery`, while LogQL translation
produces the same model. `Count` plots each bucket count and `RatePerSecond` divides by the exact
step duration; both execute through the facade's log-metric API.

Start with count and rate only. Ratios such as error percentage require aligned numerator and
denominator recipes, missing-series rules, and divide-by-zero semantics; add those only after the
basic recipe model is tested. Recipes are ephemeral in the first increment. Saving named recipes to
a small user config file is a later, separable feature.

## 4. Internal architecture

Keep terminal rendering, application state, and IMBH query execution separate:

```text
terminal events + refresh ticks
             |
             v
       App reducer/state -----> view rendering
             |
       bounded commands
             v
       Query coordinator -----> LocalSource -----> Arc<Db> (read-only)
             |
       versioned results
             v
       App reducer/state
```

Suggested modules:

```text
imbh-tui/src/
  app.rs          state machine, focus, navigation, commands
  event.rs        terminal events, ticks, shutdown
  source.rs       internal DataSource trait and LocalSource
  query.rs        bounded scheduling, generations, result/error envelopes
  time.rs         range, step, pan, zoom, and display formatting
  chart.rs        downsampling and terminal-coordinate transforms
  screens/
    overview.rs
    metrics.rs
    traces.rs
    logs.rs
  terminal.rs     enter/restore raw mode and alternate screen safely
  lib.rs          reusable runner boundary
  main.rs         small CLI and read-only database open
```

Use a reducer-style `App` state so input handling and rendering are deterministic and testable.
Terminal drawing must never await a database query. A dedicated query coordinator owns async query
execution and communicates through bounded channels.

Start with at most one expensive data query in flight. Coalesce refresh requests for the same panel,
prioritize explicit user actions over background refresh, and tag every request with a panel
generation. A late result for an older filter or time range is discarded. This prevents unbounded
DataFusion work and stale charts while respecting IMBH's shared memory budget. Add limited
concurrency only after measurement demonstrates a responsiveness problem.

All terminal setup must have an idempotent restoration guard. Normal exit, query errors, panics, and
signals should restore raw mode, cursor visibility, and the original screen as far as the platform
allows.

## 5. Dependency and footprint posture

Evaluate a small terminal stack in a spike, with Ratatui plus Crossterm as the baseline candidate.
Use minimal feature sets and avoid a large CLI/configuration framework unless it earns its cost. A
hand-written small argument parser is sufficient for the first binary.

Provisional gates for the spike:

- no crate-count or binary-size change to `imbh` and `imbhd`;
- terminal-only dependencies isolated to `imbh-tui`;
- measure the release binary, unique crate count, idle RSS, and RSS during a representative chart
  refresh;
- use the existing IMBH shipping limits as a hard ceiling until a companion-specific baseline is
  recorded;
- reject terminal features that introduce an unexpectedly heavy subtree when a simpler renderer can
  provide the same interaction.

Measured on 2026-07-21 (aarch64 glibc, `release-small`): the normalized normal-edge graph is
**304 unique crates**, 28 above the `imbh` facade's 276-crate graph, and the stripped fat-LTO
`imbh-tui` binary is **34,129,888 bytes (32.5 MiB)**. The shipping `imbh`/`imbhd` gate remains
independent and green; Ratatui/Crossterm do not enter either graph. This 304/32.5 MiB pair is the
accepted companion-specific baseline. Build artifacts were placed under
`.agents-workspace/tmp/tui-target`.

The full TUI binary will still pay IMBH's DataFusion floor. The useful comparison is therefore the
incremental terminal dependency and code cost, not the total binary in isolation.

## 6. Delivery milestones

The semantic milestones S0 through S5 precede translator milestones Q0 through Q5, and both tracks
precede T0. Semantic execution, syntax lowering, and their independent footprints must be established
before terminal dependencies are selected.

### T0: feasibility and contracts

- Add a non-shipping spike under `.agents-workspace/tmp` to measure candidate terminal dependencies.
- Confirm raw-mode restoration, resize handling, and rendering at 80x24 and narrower terminals.
- Exercise `Db::open_read_only` against a concurrently written fixture and measure refresh cost.
- Finalize the internal `DataSource` operations, command/result envelopes, and first-release CLI.
- Record a TUI footprint baseline and decide whether the crate joins default workspace builds.

Exit: an accepted dependency choice, measured footprint, and no change to core IMBH graphs.

### T1: shell and local data source

- Create `crates/imbh-tui` with library and binary targets.
- Implement terminal guard, event loop, resize, help, navigation, shared time range, pause, and manual
  refresh.
- Implement `LocalSource` using read-only open, a bounded query coordinator, generation-based stale
  result rejection, loading states, and recoverable error display.
- Add the Overview database stats panel.

Exit: the TUI opens a live database safely and remains responsive during a slow query.

### T2: logs and derived log metrics

- Implement filter editing, list/detail views, paging, trace-id drill-down, and query statistics.
- Add log volume charts and the `LogMetricRecipe` count/rate model.
- Add chart resolution selection based on terminal width and selected time range.
- Call refresh mode `Refresh`, not `Follow`.

Exit: a user can investigate logs and graph a filtered/grouped log rate without SQL.

### T3: native metrics

- Implement catalog and series discovery, query construction for gauges, sums, both histogram kinds,
  rates, and quantiles.
- Add multi-series charts, latest-value tables, legends, unit formatting, series caps, and exemplar
  trace drill-down.
- Add clear empty, partial, high-cardinality, NaN, and infinity states.

Exit: the standard typed metrics surface is usable without entering SQL.

### T4: traces and cross-signal navigation

- Implement trace search and summary results.
- Build and render the client-side span tree/waterfall with details for spans, events, and links.
- Add log-to-trace, exemplar-to-trace, and trace/span-to-log navigation.
- Preserve the originating filters and time range in a navigation history stack.

Exit: a user can move from a metric anomaly or log line to a trace and its correlated logs.

### T5: hardening and release

- Add optional recipe persistence only if the ephemeral workflow has stabilized.
- Add theme/accessibility settings, ASCII fallback, mouse support only if it does not complicate the
  keyboard path, and platform testing.
- Run the full quality and footprint gates, add usage documentation and screenshots, and publish a
  support matrix.

Exit: clean workspace gates, terminal restoration tests, documented limits, and measured release
footprint.

## 7. Recommended IMBH API follow-ups

The translator prerequisite makes the model changes in its Q1 milestone. Beyond those changes, these additions materially improve the TUI:

1. Add typed `LogQuery::trace_id` and `LogQuery::span_id` filters. This makes trace-to-log
   correlation safe and avoids composing SQL in the TUI.
2. Replace or supplement offset paging with an immutable keyset cursor. This stabilizes browsing
   while new logs arrive.
3. Add a streaming or poll-cursor `logs().tail` contract with explicit resume and duplicate
   semantics before exposing a Follow mode.
4. Complete incremental WAL-tail offsets for read-only readers. The current per-query WAL-tail
   reconstruction is correct but can make frequent refresh expensive at high ingest rates.
5. Consider scoped attribute discovery by signal, time range, and filter. The current cross-signal
   `attrs()` calls can return too much vocabulary for a focused filter editor.
6. Consider cancellation or cooperative abort for eager typed queries if dropping an in-flight query
   does not promptly release work in practice.

Items 1 through 3 change the public API and require updating `ARCHITECTURE.md` before or with their
implementation. None should block T0 or the basic T1/T2 snapshot explorer.

## 8. Verification strategy

Unit tests:

- reducer transitions, focus, navigation history, and stale result rejection;
- time-range pan/zoom and automatic step selection;
- log recipe compilation, bucket alignment, count-to-rate conversion, and missing buckets;
- trace-tree assembly including multiple roots, missing parents, cycles, and zero-duration spans;
- chart downsampling, NaN/infinity handling, clipping, and narrow terminal behavior;
- unit and timestamp formatting.

Rendering tests:

- Ratatui test-backend snapshots for each screen at representative and minimum sizes;
- loading, empty, error, truncation, and high-cardinality states;
- Unicode and ASCII modes.

Integration tests:

- build a temporary tri-signal IMBH database with in-process OTLP fixtures, open it read-only, and
  drive every `LocalSource` operation;
- keep a writer active while the source refreshes across WAL append and seal boundaries;
- verify log-derived count/rate results against direct log counts;
- verify metric exemplar to trace and log to trace navigation;
- verify query coalescing and bounded channels under a refresh storm.

Terminal lifecycle tests should isolate the platform-facing guard where possible and include a small
opt-in pseudo-terminal smoke test for restoration after normal exit and panic. Default workspace tests
must remain daemon-free and network-free.

For every Rust increment, run the standard gate in `QUALITY_GATE.md`. After the dependency spike and
before release, also run the footprint gate and record the companion binary's release size, unique
crate count, idle RSS, and refresh peak RSS.

## 9. Principal risks

- Frequent read-only refresh can repeatedly decode the live WAL tail. Mitigate with conservative
  defaults, pause while editing, coalescing, and the incremental-tail follow-up.
- High-cardinality metric or log groupings can overwhelm both memory and a terminal chart. Enforce
  caps, expose truncation, and make narrowing filters easy.
- Offset log paging is unstable under concurrent inserts. Do not market it as a live stream; pursue a
  keyset cursor.
- A flat trace can contain malformed parent relationships. Build the waterfall defensively and keep
  every span visible.
- Terminal cleanup failures make the application feel unsafe. Treat restoration as a release gate,
  not polish.
- A remote mode built directly against today's SQL endpoint would duplicate typed query compilation
  and result decoding. Wait for a typed transport or introduce a deliberately versioned protocol.

## 10. First-release acceptance criteria

- `imbh-tui --db <path>` opens the database read-only alongside an active writer.
- The interface remains usable at 80x24 and degrades to a clear small-terminal message below its
  supported minimum.
- Users can inspect native metric series, search and open traces, search and page logs, and create a
  count or rate chart from a log filter.
- PromQL P1, LogQL L1, and TraceQL T1 input lowers to the same conformant models as the structured
  editors, with source-positioned errors for unsupported constructs and no approximate results.
- Metric exemplars and log trace ids open the corresponding trace.
- Queries are bounded, refresh requests coalesce, and stale results never replace newer state.
- The application makes no database mutations and never takes the writer lock.
- Exiting normally or after an internal failure restores the terminal.
- The full Rust gate passes, the default IMBH footprint is unchanged, and the TUI footprint baseline
  is documented.

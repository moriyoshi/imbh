# Companion TUI plan

> **Trace detail screen (2026-07-30):** the Traces screen gained two full-content routes, so a trace is
> no longer confined to the non-scrolling half-height preview pane on the list. `Route::TraceDetail`
> shows the whole trace — header (trace id, span count, duration, start; root service/operation in the
> block title), the complete waterfall as a span-selectable ratatui `List` (so `↑`/`↓`/PageUp/PageDown/
> Home/End walk every span and the widget scrolls it, at any terminal size; non-OK spans in red), and,
> when the content area is at least 18 rows tall, a five-line summary of the span under the cursor.
> `Route::SpanDetail` shows one span's full fields (ids/parent, service, kind, status + message,
> absolute start, offset into the trace, duration, the malformed-parent note, the three attribute maps,
> and the raw events/links JSON), scrolled like the log detail. `Enter` on the Traces list opens the
> trace detail, `Enter` on a waterfall row opens the span detail, and `L` from either opens Logs
> correlated by trace id **and** span id — closing the per-span drill-down gap the 2026-07-23 note left
> open (§3.3). No extra query: the list already materializes the selected trace for its preview pane, so
> `build_trace_detail` emits the width-independent `Waterfall` rows plus an aligned `Vec<SpanRecord>` in
> one pass and the app retains it (dropped as soon as the row cursor moves; an Enter that beats the
> in-flight fetch opens when it lands). The preview pane itself still does not scroll, but now reports
> `Waterfall: N of M spans — enter: all` rather than silently cutting deep traces. See JOURNAL
> 2026-07-30.
>
> **Interaction follow-up (2026-07-23):** the five remaining interaction features landed — time
> pan/zoom (`[`/`]` pan, `-`/`+` zoom, via an absolute-window freeze), older/newer log paging (`n`/`p`
> over the facade offset `PageCursor`), trace→log drill-down (`L` on the Traces list, powered by the new
> `LogQuery::trace_id`/`span_id` correlation filter; trace-granular — a per-span waterfall cursor is the
> remaining piece), metric-exemplar→trace (magenta chart markers, `Enter` to the nearest exemplar's
> trace), and the small-terminal resize prompt below 40×10 (§10 acceptance). See JOURNAL 2026-07-23.
>
> **Implementation status (2026-07-21):** the local read-only companion, terminal restoration,
> bounded/coalesced coordinator, overview, PromQL metrics, TraceQL waterfall, native log viewer, and
> LogQL-derived chart are implemented in `imbh-tui`. A modal relative time-range picker (`t`; presets
> 5m-7d, each paired with a bounded step) drives the query window, the query input bar has
> per-language syntax highlighting (strings, numbers/durations, function calls, operators,
> punctuation), and log timestamps render as UTC `YYYY-MM-DD HH:MM:SS.mmm` (hand-rolled, no datetime
> dependency added to the terminal graph). Navigation follows Midnight-Commander conventions: `F9`
> activates the always-visible menu bar (`Mode::Menu`) where `←`/`→`/Tab move a highlight across the
> screens and the trailing time-range item and Enter activates it (Esc/F9 dismiss); number keys `1`-`4`
> still jump to a screen directly. A typed `Route` enum
> (`Overview`/`Metrics`/`MetricDetail{detail}`/`Traces`/`Logs`/`LogDetail{record}`) is the single
> source of truth for the current view (`screen()` derives from it), and `Mode` holds only the
> transient input overlays. `←`/Esc and `→` are browser Back/Forward through a two-stack history
> (`App::back`/`forward` of `NavEntry`, which snapshots the route + query buffers + cursor context);
> forward-to-a-new-view is only ever an explicit action — Enter (catalog→series, series→detail,
> log→detail), Enter/`t`-jump (log detail→trace), or a screen key — so `→` only redoes a Back. The
> detail views are ordinary content, not modal: `draw` always renders the menu bar then dispatches the
> content area by route (details render there, with the range dropdown still floating on top), and
> `handle_detail_key` consumes only the detail's own keys (log-body scroll, chart cursor `h`/`l`/paging,
> Enter→trace) so `F9`/`1`-`4`/`t`/back-forward work from within a detail. `↑`/`↓`/PageUp/PageDown/Home/
> End move the in-view row cursor or scroll the pane (offset clamped against the wrapped row count
> published by the renderer). Catalog selection (checkboxes, expansion, discovered dims) survives
> navigating away and back: `build_metric_tree` carries per-metric state over by name across every
> catalog rebuild. The query editor has
> context-aware autocompletion: `completion_context` classifies the caret position (expression vs.
> inside a `{…}` matcher vs. inside a quoted value vs. suppressed) and offers only the eligible
> vocabulary, filtered by the partial token — metric names (fetched from `metrics().catalog()` on the
> Metrics screen) ahead of per-language function/keyword lists in expression position, label *names*
> inside a matcher, and that label's *values* inside a quoted matcher (label vocabulary reused from the
> catalog's per-metric dimension discovery, with a one-shot lazy fetch for an undiscovered metric).
> `Tab` accepts (appending `(` for functions) and the arrow keys move the selection. The Overview screen omits the query pane entirely
> (that vertical space goes to the results). The Traces screen no longer dead-ends on the trace cap:
> because that cap is enforced on candidate traces in the time window *before* the TraceQL predicate
> runs, a busy window overflows regardless of query selectivity, so the pane auto-narrows the window
> toward `end` (halving the span) to the most recent sub-window that fits and reports the reduction
> loudly (a `TraceQL (narrowed)` title plus a banner); results stay exact for the shown window. The
> Traces results area is split into two stacked panes: the raw trace list on top and the selected
> trace's span waterfall below (via `Snapshot::detail`, a general optional secondary pane). The raw
> trace and log lists are cursor-navigable (a highlighted selected row moved with `↑`/`↓`/PageUp/
> PageDown/Home/End, via `Snapshot::list_from` marking where selectable rows begin); the Metrics screen
> renders as a selectable header-and-column `Table` (`Snapshot::table`: the catalog as
> Metric/Kind/Unit/Temporality, a PromQL result as Series/Latest/Min/Max/Points); other screens keep
> the plain scrolled view. The catalog is a lazy tree (`App::metric_tree`): `Space` expands a metric
> into its groupable dimensions — discovered by evaluating the metric's bare selector as an instant
> over the metric's whole retained span (from `db.stats()`, so discovery is independent of the time
> picker) and reading the returned series' labels, which include the resource `service` and data-point
> attributes like `host` (`__name__`/`le` excluded) — and expands a dimension into its distinct values.
> Each value leaf is an exclusive-per-axis checkbox toggled with `Space` (`DimNode::selected`, radio
> within a dimension), so the checked values across axes form a `{label="value",…}` label filter.
> `Enter` visualizes the matching PromQL via `build_metric_query` — the metric restricted to the
> checked matchers, aggregated `by` the dimension when the cursor is on a `by …` row
> (`avg`/`sum`/`histogram_quantile(…, sum by (…, le) …)` per kind) — and `Esc` clears back to the
> catalog. Selection is implicit and can span several metrics: every metric with at least one checked
> series is visualized together (dimensionless metrics get a `whole_selected` checkbox on the
> `(no dimensions)` row, `TreeRowRef::NoDims`), else the node under the cursor. `visualize_queries`
> builds one PromQL per selected metric, newline-joined into `active_query` and run separately (the
> executor has no `or` and the source requires an exact `__name__`, so metrics can't share one query),
> with the result series concatenated (distinguishable via `__name__`) and the one-line query bar
> rendering `\n` as a ` │ ` separator. The waterfall follows the trace selection: moving the cursor to a different
> trace fetches that trace's waterfall on demand (`request_waterfall` → `Update::Waterfall`, guarded by
> query generation + trace id so stale fetches are dropped) and swaps it into the detail pane. Log rows
> show a short trace id alongside the body, and `Enter` on a selected log opens the detail view
> (`Route::LogDetail{record}`, the `LogRecord` cloned into the route) with all fields — severity,
> service, trace/span id, full body, and attribute/resource/scope sections; from there `Enter`
> jumps to the Traces screen focused on that log's trace (`focus_trace_id` overrides the row selection
> for the waterfall until the cursor moves or the trace is found in the list). The header is a single
> Midnight-Commander-style menu bar: a ` IMBH ⬤ ` brand (the black-hole logo) + the
> `1 Overview  2 Metrics  3 Traces  4 Logs` screen menu on the left (active screen inverted), and a
> right-aligned time-range + live UTC wall-clock selector (`last 5m  ⏲ 2026-07-21 14:23:07`,
> `format_datetime_ns`); the event loop wakes
> at least once a second so the clock ticks, while the auto-refresh timer arm is gated on
> `last_refresh.elapsed() >= refresh_interval` so those 1s redraw wakes never trigger an early query.
> The selector's on-screen `Rect` is computed from the same `UnicodeWidthStr` span-width used to
> right-align it, and its decorative glyphs are East-Asian-width unambiguous (U+2B24 `⬤`, U+23F2 `⏲` —
> not the ambiguous U+25CF/U+00B7 look-alikes) so a CJK terminal cannot desync the width math.
> The time-range picker is a dropdown that drops down from (and right-aligns to) that selector
> rather than a centered modal (`draw_time_range_picker` takes the anchor `Rect`). Beyond the rolling
> presets the window can be an absolute span: the dropdown's trailing `Absolute…` row opens a two-field
> form (`Mode::AbsoluteRange`, start/end UTC `YYYY-MM-DD HH:MM:SS`, Tab/↑↓ switch fields, Enter apply,
> Esc cancel) dropped from the same anchor and prefilled from the current effective window;
> `App::abs_window` holds the committed span (cleared by picking any preset). `Options.window` (public,
> also settable at launch via `--from/--to`, both-or-neither and ordered) plumbs the span to
> `eval_window`, which uses it verbatim with a derived ~120-point step instead of `now - lookback ..
> now`, and `request_refresh` re-applies it so the window stays fixed across refreshes (a refresh
> re-queries the same span, surfacing late data within it rather than sliding). Parsing is hand-rolled
> (`parse_datetime` + `days_from_civil`, the inverse of `civil_from_days`), no datetime dependency; the
> header indicator shows the span (same-day date collapsed) and the status line reads `range=absolute`.
> The span waterfall
> keeps its `|bar|` time axis vertically aligned regardless of span depth or glyph width: the tree
> indent is folded into the span-name column and the pair is clamped to a fixed `NAME_W = 28`-cell
> field (`clamp_field`) measured by East Asian display width (`unicode-width`, footprint-neutral —
> already in the graph via ratatui), so wide (CJK) names occupy the right number of cells and never
> skew the bars. Enter on a selected series in the Metrics result table opens the detailed time-series
> viewer (`Route::MetricDetail{detail}`): a ratatui `Chart` line plot over the query window with
> labeled axes (x `HH:MM:SS`, y min/mid/max), a movable yellow vertical cursor (a second `Dataset`)
> driven by h/l/Home/End/PgUp/PgDn (the arrows are history nav), and a readout of the exact
> `timestamp = value` under the cursor plus
> `min · max · avg · latest · N pts`; `--ascii` (or a series with <2 finite points) falls back to the
> hand-rolled `ascii_chart`. Each PromQL result series' full `(timestamp_ns, value)` history is retained
> in `Snapshot::series` (aligned to the table rows) and cloned into the `Route::MetricDetail` variant on
> open so it survives refreshes. The Overview reports real per-table row counts on the read-only handle it opens
> (`Db::stats()` derives reader stats from the on-disk manifest + WAL tail rather than the reader's
> always-empty live buffers). Background auto-refresh is off by default (Shift+R toggles it; manual `r`,
> query-run, and screen switches always refresh). Rich
> selection, paging, and cross-signal drill-down interactions in T2-T4 remain follow-up UX work rather
> than semantic prerequisites.
>

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

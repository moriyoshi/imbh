//! [`App`] — the whole UI state, and the core of its state machine.
//!
//! The struct lives here together with construction, the active-query accessors, and
//! [`App::apply`] (landing a query result). The rest of the behaviour is split by concern into the
//! submodules: [`nav`] (menu bar, focus ring, back/forward history), [`window`] (the time range and
//! pan/zoom), [`catalog`] (the Metrics catalog tree), [`views`] (route accessors, row selection, and
//! opening the detail views), and [`completion`] (the query completion popup).

mod catalog;
mod completion;
mod nav;
mod views;
mod window;

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use imbh::PageCursor;

use crate::chart::ChartGeometry;
use crate::completion::Completion;
use crate::mascot::Mascot;
use crate::model::{
    ATTR_MEASURE_INTERVAL, AbsTarget, AttrRow, AttrStats, AttrWindow, DetailPane, DetailStyle,
    ExemplarMarker, Focus, LOADING_BANNER_AFTER, LogCorrelation, MetricNode, Mode, NavEntry,
    PaneTable, QueryResult, Refresh, Route, Screen, Snapshot, TreeRowRef,
};
use crate::textfield::{TextField, caret_in};
use crate::waterfall::TraceDetail;

pub(crate) struct App {
    /// The current view (single source of truth); the `screen` is derived from it.
    pub(crate) route: Route,
    /// One query buffer per screen, indexed by [`App::query_index`]. A screen without a query pane
    /// (Overview) keeps an unused empty slot rather than a special case.
    pub(crate) query: [String; Screen::ORDER.len()],
    /// The edit caret in the active query buffer, as a byte offset. Only meaningful in
    /// [`Mode::Editing`], which is only ever entered through `begin_editing` (it parks the caret at the
    /// end of the buffer). Every read goes through [`App::query_caret`], which clamps into the *current*
    /// buffer, so a buffer swapped out from under it (a screen switch, Back/Forward, the catalog's
    /// "visualize") can never leave a dangling offset.
    pub(crate) query_cursor: usize,
    /// Transient input overlay on top of the route (menu / editing / range picker), or `Normal`.
    pub(crate) mode: Mode,
    pub(crate) range_index: usize,
    /// Highlighted candidate while the time-range picker is open; committed to `range_index` on Enter.
    /// Ranges over `0..=TIME_RANGES.len()`, where the last index is the "Absolute…" row.
    pub(crate) range_cursor: usize,
    /// Highlighted item while the menu bar is active (`Mode::Menu`): `0..Screen::ORDER.len()` are the
    /// screens, the last index (`MENU_LEN - 1`) is the time-range selector.
    pub(crate) menu_cursor: usize,
    /// When `Some`, an absolute query window `(start_ns, end_ns)` overriding the rolling preset; set
    /// from the absolute-time form and cleared by picking any relative preset.
    pub(crate) abs_window: Option<(i64, i64)>,
    /// The window the Overview's attribute statistics are measured over, independent of `abs_window`.
    /// `None` — the default — measures **all sealed segments**; set from the same absolute-range form
    /// under [`AbsTarget::Attributes`].
    pub(crate) attr_window: AttrWindow,
    /// Which window the absolute-range form is currently editing.
    pub(crate) abs_target: AbsTarget,
    /// Editable buffers for the absolute-range form (UTC `YYYY-MM-DD HH:MM:SS`) and which field has
    /// focus (0 = start, 1 = end), plus the last parse error to surface in the form.
    pub(crate) abs_start: String,
    pub(crate) abs_end: String,
    pub(crate) abs_field: usize,
    /// The edit caret in the *focused* field, as a byte offset. One caret for both, re-seated at the
    /// end of whichever field takes focus (`focus_abs_field`); reads clamp, like `query_cursor`.
    pub(crate) abs_cursor: usize,
    pub(crate) abs_error: Option<String>,
    /// The Overview's most recent attribute measurement. Held separately from the snapshot because
    /// the two halves of the Overview arrive independently and in either order — see
    /// [`App::compose_attr_stats`] — and because it outlives a refresh: re-scanning the corpus every
    /// auto-refresh tick would cost far more than it tells anyone.
    pub(crate) attr_stats: Option<AttrStats>,
    /// The promoted attribute keys in effect, as last read from the backend. Held so `p` can send the
    /// *whole* set (promotion is a list, and its order is the column order) rather than a delta the
    /// server would have to guess the placement of.
    pub(crate) promoted: Vec<String>,
    /// Whether this session can change the promoted set — i.e. whether it is driving a daemon rather
    /// than reading a directory (see `Backend::can_promote`). Recorded once at startup so `draw`,
    /// which has no backend, can offer the action only where it exists.
    pub(crate) can_promote: bool,
    /// Background auto-refresh, off by default; toggled with space. Manual/query/switch refreshes
    /// always run regardless.
    pub(crate) auto_refresh: bool,
    pub(crate) loading: bool,
    /// When the in-flight query started, so the UI can tell an instant refresh from one the user is
    /// actually waiting on. `None` whenever `loading` is false; the two are set and cleared together.
    pub(crate) loading_since: Option<Instant>,
    /// Whether the in-flight query holds the keyboard. Set for an [`Refresh::Interactive`] load and
    /// left clear for a [`Refresh::Background`] one — see [`Refresh`] for why the timer is exempt.
    pub(crate) input_locked: bool,
    /// Whether a keystroke has already been refused during this load. Latches the banner on for the
    /// rest of the load: once input has visibly stopped responding, the reason has to stay on screen
    /// even if the query lands before [`LOADING_BANNER_AFTER`] would have shown it.
    pub(crate) input_refused: bool,
    /// A refresh that arrived while one was already in flight, replayed when that one lands. Carries
    /// its origin so a coalesced *user* action still locks input and a coalesced timer tick does not.
    pub(crate) pending_refresh: Option<Refresh>,
    pub(crate) generation: u64,
    pub(crate) snapshot: Snapshot,
    pub(crate) last_error: Option<String>,
    pub(crate) last_refresh: Instant,
    /// Selected row index (absolute into `snapshot.lines`) when the primary pane is a navigable list.
    pub(crate) selected: usize,
    /// First result row to display; advanced by the scroll keys, clamped in `draw`.
    pub(crate) scroll: u16,
    /// Bounds published by `draw` (which alone knows the viewport geometry) so the key handler can
    /// clamp scrolling without re-deriving the wrapped row count.
    pub(crate) max_scroll: Cell<u16>,
    pub(crate) page_rows: Cell<u16>,
    /// The Overview attribute pane's rectangle, published by `draw` (which alone knows the geometry)
    /// so the range form can be anchored **over that pane**. The query window's form drops from the
    /// header's time indicator; putting the attribute one in the same place would say it edits the
    /// same thing.
    pub(crate) attr_area: Cell<ratatui::layout::Rect>,
    /// Metric names from the catalog, used as PromQL completion vocabulary. Filled asynchronously.
    pub(crate) metric_names: Vec<String>,
    /// The open completion popup, or `None` when nothing is being suggested.
    pub(crate) completion: Option<Completion>,
    /// Trace id whose waterfall is currently shown in the detail pane (or in flight), so the selected
    /// trace's waterfall is fetched only when the selection actually moves to a different trace.
    pub(crate) detail_trace_id: Option<String>,
    /// The materialized trace behind the Traces preview pane (matching `detail_trace_id`), retained so
    /// Enter opens the full trace detail from memory instead of re-querying. `None` while in flight.
    pub(crate) trace_detail: Option<TraceDetail>,
    /// Set when Enter is pressed on the Traces list before the selected trace has finished loading: the
    /// full trace detail then opens as soon as the fetch lands. Cleared by any navigation.
    pub(crate) pending_trace_open: bool,
    /// The selected span row in an open `Route::TraceDetail` (view context, so the history captures it
    /// while it lives flat here — the same shape as `metric_cursor`).
    pub(crate) span_cursor: usize,
    /// The x-axis cursor index into the `Route::MetricDetail` series' points (view context, so it is
    /// captured by the history but lives flat here rather than inside the route).
    pub(crate) metric_cursor: usize,
    /// When navigating from a log's detail to its trace, the trace id to focus the Traces waterfall on
    /// (overrides the selection until the cursor is moved or a matching row is found).
    pub(crate) focus_trace_id: Option<String>,
    /// Set alongside `focus_trace_id` by a log→trace / exemplar→trace jump: those drill-downs ask for
    /// the trace *detail*, not the Traces list they route through, so the detail is opened as soon as
    /// the focused trace's waterfall is in hand. Cleared by any navigation that abandons the focus.
    pub(crate) focus_trace_open: bool,
    /// The Metrics catalog tree (expansion + lazily-loaded dimensions). Rebuilt whenever the flat
    /// catalog snapshot arrives; drives the catalog table rendering.
    pub(crate) metric_tree: Vec<MetricNode>,
    /// Flattened catalog rows aligned to `snapshot.table.rows`, mapping each row to its tree node.
    pub(crate) tree_rows: Vec<TreeRowRef>,
    /// Discovered log label names — the cross-signal attribute keys (`db.attrs().names()`, which
    /// already folds in the promoted `service.name`), used as the label-name completion vocabulary
    /// inside the Logs `{…}` selector. `None` until fetched; `Some` afterwards (possibly empty).
    pub(crate) log_labels: Option<Vec<String>>,
    /// Whether a log-label-name discovery is in flight, so it fires at most once.
    pub(crate) log_labels_loading: bool,
    /// Discovered distinct values per log label (`db.attrs().values(key)`) — the label-value
    /// completion vocabulary inside a quoted Logs matcher. Filled lazily, one key at a time.
    pub(crate) log_label_values: HashMap<String, Vec<String>>,
    /// Log labels whose value discovery is in flight, so each key fires at most once.
    pub(crate) log_label_values_loading: HashSet<String>,
    /// Browser-style navigation history. A forward navigation (Enter/`t`/screen switch) pushes the
    /// view it leaves onto `back` and clears `forward`; `←` pops `back`, `→` pops `forward`.
    pub(crate) back: Vec<NavEntry>,
    pub(crate) forward: Vec<NavEntry>,
    /// The pane the focus ring is on (drives the pane highlight and what `Enter` activates). Transient
    /// view chrome like `mode`, so it is reset on navigation and excluded from the back/forward history.
    pub(crate) focus: Focus,
    /// Whether the animated mascot is shown. Off by default; toggled with `m` (a no-op on `--ascii`
    /// terminals, where the block-glyph art is never rendered).
    pub(crate) show_mascot: bool,
    /// Whether the trace detail's waterfall pins the selected span's scrolled-off ancestors at the top
    /// of the pane. On by default; toggled with `s` there. A display preference like `show_mascot`
    /// rather than view state, so it is deliberately *not* captured by the navigation history.
    pub(crate) sticky_waterfall: bool,
    /// The mascot controller (position, motions, event igniters). Advanced once per redraw in
    /// [`run`](crate::runtime::run).
    pub(crate) mascot: Mascot,
    /// The metric chart's rendered geometry, published by `draw_metric_detail` for the mascot's chart
    /// ride and consumed in the run loop. `None` off the chart or when there is nothing to plot.
    pub(crate) chart_geom: RefCell<Option<ChartGeometry>>,
    /// Last-seen route identity, so the run loop can emit a mascot `Navigated` event on a change.
    pub(crate) mascot_route_tag: u64,
    /// Last-seen idle state, so the loop emits `Idle`/`Active` only on a transition.
    pub(crate) mascot_idle: bool,
    /// When the user last pressed a key; drives the idle/active distinction.
    pub(crate) mascot_last_input: Instant,
    /// Set when a query result lands, drained by the loop into a mascot `Refreshed` event.
    pub(crate) mascot_refresh_pending: bool,
    /// Older/newer log paging (Logs screen). `log_cursor_stack` holds the resume cursors used to reach
    /// the current page (empty = page 0, most recent); `log_next_cursor` is the cursor for the *next*
    /// older page, echoed from the last Logs result (`None` when the page was short — no older rows).
    /// `log_paging` marks the single refresh a page move drives, so `request_refresh` keeps the stack
    /// instead of resetting to page 0 (which it does on every other refresh).
    pub(crate) log_cursor_stack: Vec<PageCursor>,
    pub(crate) log_next_cursor: Option<PageCursor>,
    pub(crate) log_paging: bool,
    /// Active trace→log drill-down correlation (set when jumping from a trace to its logs); layered onto
    /// the Logs query until the user leaves Logs or runs a fresh query. Captured by the nav history.
    pub(crate) log_correlation: Option<LogCorrelation>,
    /// Exemplar→trace markers for the open metric-detail view: the metric's exemplars that carry a trace
    /// id and fall within the plotted window. `Enter` jumps to the trace of the marker nearest the chart
    /// cursor. Fetched asynchronously on open; guarded by `generation` so a stale fetch is dropped.
    pub(crate) metric_exemplars: Vec<ExemplarMarker>,
}

impl App {
    pub(crate) fn new() -> Self {
        // Default to the 15m window (index 1), matching the historical default lookback.
        let range_index = 1;
        Self {
            route: Route::Overview,
            query: [
                String::new(),
                String::new(),
                "{}".to_owned(),
                // Logs default: a bare selector matching everything (filtered list + volume sparkline).
                "{}".to_owned(),
            ],
            query_cursor: 0,
            mode: Mode::Normal,
            range_index,
            range_cursor: range_index,
            menu_cursor: 0,
            abs_window: None,
            attr_window: None,
            abs_target: AbsTarget::Query,
            abs_start: String::new(),
            abs_end: String::new(),
            abs_field: 0,
            abs_cursor: 0,
            abs_error: None,
            attr_stats: None,
            promoted: Vec::new(),
            can_promote: false,
            auto_refresh: false,
            loading: false,
            loading_since: None,
            input_locked: false,
            input_refused: false,
            pending_refresh: None,
            generation: 0,
            selected: 0,
            snapshot: Snapshot::message("Overview", "Loading..."),
            last_error: None,
            last_refresh: Instant::now(),
            scroll: 0,
            max_scroll: Cell::new(0),
            page_rows: Cell::new(1),
            attr_area: Cell::new(ratatui::layout::Rect::default()),
            metric_names: Vec::new(),
            completion: None,
            detail_trace_id: None,
            trace_detail: None,
            pending_trace_open: false,
            span_cursor: 0,
            metric_cursor: 0,
            focus_trace_id: None,
            focus_trace_open: false,
            metric_tree: Vec::new(),
            tree_rows: Vec::new(),
            log_labels: None,
            log_labels_loading: false,
            log_label_values: HashMap::new(),
            log_label_values_loading: HashSet::new(),
            back: Vec::new(),
            forward: Vec::new(),
            focus: Focus::Primary,
            show_mascot: false,
            sticky_waterfall: true,
            mascot: Mascot::new(),
            chart_geom: RefCell::new(None),
            mascot_route_tag: 0,
            mascot_idle: false,
            mascot_last_input: Instant::now(),
            mascot_refresh_pending: false,
            log_cursor_stack: Vec::new(),
            log_next_cursor: None,
            log_paging: false,
            log_correlation: None,
            metric_exemplars: Vec::new(),
        }
    }

    /// A cheap identity of the current view, so a change (screen switch, opening/leaving a detail, or
    /// selecting a different series) reads as a navigation event for the mascot.
    pub(crate) fn mascot_route_tag(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.route.screen().index().hash(&mut h);
        match &self.route {
            Route::MetricDetail { detail } => {
                1u8.hash(&mut h);
                detail.labels.hash(&mut h);
                detail.query.hash(&mut h);
            }
            Route::LogDetail { .. } => 2u8.hash(&mut h),
            Route::TraceDetail { detail } => {
                3u8.hash(&mut h);
                detail.trace_id.hash(&mut h);
            }
            Route::SpanDetail { trace_id, span } => {
                4u8.hash(&mut h);
                trace_id.hash(&mut h);
                span.span_id.hash(&mut h);
            }
            _ => 0u8.hash(&mut h),
        }
        h.finish()
    }

    /// The screen the current route belongs to.
    pub(crate) fn screen(&self) -> Screen {
        self.route.screen()
    }

    pub(crate) fn query_index(&self) -> usize {
        match self.screen() {
            Screen::Overview => 0,
            Screen::Metrics => 1,
            Screen::Traces => 2,
            Screen::Logs => 3,
        }
    }

    pub(crate) fn active_query(&self) -> &str {
        &self.query[self.query_index()]
    }

    pub(crate) fn active_query_mut(&mut self) -> &mut String {
        let index = self.query_index();
        &mut self.query[index]
    }

    /// Replace the active query wholesale, parking the caret at its end. The way anything but the
    /// editor itself should write the buffer, so `query_cursor` never trails a query it no longer
    /// describes.
    pub(crate) fn set_active_query(&mut self, query: impl Into<String>) {
        *self.active_query_mut() = query.into();
        self.query_cursor = self.active_query().len();
    }

    /// The edit caret as a byte offset into the active query, clamped into the buffer and onto a
    /// character boundary — the only way `query_cursor` should be read.
    pub(crate) fn query_caret(&self) -> usize {
        caret_in(self.active_query(), self.query_cursor)
    }

    /// The active query up to the caret. What completion classifies: the eligible vocabulary follows
    /// from where the caret sits, not from the end of the buffer.
    pub(crate) fn query_before_caret(&self) -> &str {
        &self.active_query()[..self.query_caret()]
    }

    /// The query box as an editable one-line field (buffer + caret), which is what the editing keys
    /// act on.
    pub(crate) fn query_field(&mut self) -> TextField<'_> {
        let index = self.query_index();
        TextField {
            text: &mut self.query[index],
            caret: &mut self.query_cursor,
        }
    }

    /// The focused absolute-range field's text (0 = start, 1 = end).
    pub(crate) fn abs_text(&self) -> &str {
        if self.abs_field == 0 {
            &self.abs_start
        } else {
            &self.abs_end
        }
    }

    /// The caret in the focused absolute-range field, clamped as [`App::query_caret`] is.
    pub(crate) fn abs_caret(&self) -> usize {
        caret_in(self.abs_text(), self.abs_cursor)
    }

    /// The focused absolute-range field as an editable one-line field.
    pub(crate) fn abs_text_field(&mut self) -> TextField<'_> {
        let text = if self.abs_field == 0 {
            &mut self.abs_start
        } else {
            &mut self.abs_end
        };
        TextField {
            text,
            caret: &mut self.abs_cursor,
        }
    }

    /// Focus one of the two absolute-range fields (0 = start, 1 = end), parking the caret at its end.
    /// One caret is shared between the fields, so moving between them has to re-seat it.
    pub(crate) fn focus_abs_field(&mut self, field: usize) {
        self.abs_field = field;
        self.abs_cursor = self.abs_text().len();
    }

    /// The window an attribute measurement made now would belong to — which *is* the cache key, since
    /// the pane's window is its own and depends on nothing else.
    pub(crate) fn attr_key(&self) -> AttrWindow {
        self.attr_window
    }

    /// Whether the attribute statistics need measuring again: never measured, the user moved the
    /// range, or the last measurement has gone stale ([`ATTR_MEASURE_INTERVAL`]). Everything else
    /// reuses the block already held, which is what keeps an auto-refreshing Overview from scanning
    /// the corpus every few seconds.
    pub(crate) fn needs_attr_measure(&self) -> bool {
        match &self.attr_stats {
            None => true,
            Some(stats) => {
                stats.key != self.attr_key() || stats.measured_at.elapsed() >= ATTR_MEASURE_INTERVAL
            }
        }
    }

    /// Record an attribute measurement and fold it into the current snapshot if it is still current.
    pub(crate) fn take_attr_stats(&mut self, key: AttrWindow, pane: DetailPane) {
        self.attr_stats = Some(AttrStats {
            key,
            measured_at: Instant::now(),
            pane,
        });
        self.compose_attr_stats();
    }

    /// Put the measurement in the Overview's attribute pane, when the two belong together.
    ///
    /// The Overview's two panes are two independent requests answered on their own schedules, so this
    /// is called from both arrivals and applies only when the measurement describes the range now
    /// selected. That is what stops a scan issued for a window the user has since left from being
    /// shown under the current one, in either arrival order. The pane is replaced wholesale, so a
    /// re-arrival cannot stack two of them.
    pub(crate) fn compose_attr_stats(&mut self) {
        if self.screen() != Screen::Overview || self.route.is_detail() {
            return;
        }
        let Some(stats) = &self.attr_stats else {
            return;
        };
        if stats.key != self.attr_key() {
            return;
        }
        self.snapshot.detail = Some(stats.pane.clone());
        // Row 0 of a grouped table is a section title, and the rows themselves have just moved.
        self.snap_attr_cursor();
    }

    /// Mark a query as in flight, taking the keyboard if the user is the one who asked.
    ///
    /// The four loading fields only ever move together, through here and [`App::end_loading`], so
    /// there is no state in which the banner is armed but the lock is not (or the reverse).
    pub(crate) fn begin_loading(&mut self, origin: Refresh) {
        self.loading = true;
        self.loading_since = Some(Instant::now());
        self.input_locked = origin == Refresh::Interactive;
        self.input_refused = false;
    }

    /// Release the keyboard and disarm the banner: the query landed (or was abandoned as stale).
    pub(crate) fn end_loading(&mut self) {
        self.loading = false;
        self.loading_since = None;
        self.input_locked = false;
        self.input_refused = false;
    }

    /// How long the current query has been in flight, when that is worth putting on screen.
    ///
    /// `None` means no banner: either nothing is loading, or it started recently enough that the
    /// answer is about to arrive anyway. A refused keystroke overrides the delay — the user has
    /// already seen the UI stop responding, so the explanation cannot wait for a timer.
    pub(crate) fn loading_banner(&self) -> Option<Duration> {
        let elapsed = self.loading_since?.elapsed();
        (self.input_refused || elapsed >= LOADING_BANNER_AFTER).then_some(elapsed)
    }

    pub(crate) fn apply(&mut self, result: QueryResult) {
        self.end_loading();
        if result.generation != self.generation || result.screen != self.screen() {
            return;
        }
        self.last_refresh = Instant::now();
        match result.result {
            Ok(snapshot) => {
                // Echo the next-older-page cursor (Logs only; `None` elsewhere) so the paging keys know
                // whether an older page exists.
                self.log_next_cursor = snapshot.next_cursor;
                self.snapshot = snapshot;
                self.last_error = None;
                // Keep the row cursor within the new result's selectable range (rows can shrink on
                // refresh); starting from 0 this lands the cursor on the first selectable row.
                if let Some((first, last)) = self.selectable_bounds() {
                    self.selected = self.selected.clamp(first, last);
                }
                // The new snapshot ships a placeholder detail; force the selected trace's waterfall to
                // be (re)fetched by clearing what we think is shown.
                self.detail_trace_id = None;
                // Keep an open metric-detail chart live: re-derive its points from the matching series
                // in the fresh result (matched by label set), so a range change / pan / zoom / auto-
                // refresh actually redraws the plotted window instead of showing the frozen open-time
                // snapshot. An empty match (the series left the window) clears the plot honestly.
                if let Some(labels) = match &self.route {
                    Route::MetricDetail { detail } => Some(detail.labels.clone()),
                    _ => None,
                } {
                    let points = self
                        .snapshot
                        .series
                        .iter()
                        .find(|series| series.labels == labels)
                        .map(|series| series.points.clone())
                        .unwrap_or_default();
                    if let Route::MetricDetail { detail } = &mut self.route {
                        detail.points = points;
                    }
                    self.metric_cursor = self.metric_cursor.min(
                        self.route_metric_detail()
                            .map_or(0, |detail| detail.points.len().saturating_sub(1)),
                    );
                }
            }
            Err(error) => self.last_error = Some(error),
        }
        // A measurement that arrived before its snapshot did (an empty database measures faster than
        // the gauges query) is folded in now rather than dropped.
        self.compose_attr_stats();
    }

    /// The inclusive `[first, last]` selection-index range the row cursor may occupy, or `None` when
    /// the primary pane is not navigable. For a table the index is into `table.rows` (`first == 0`);
    /// for a list it is an absolute index into `lines` (`first == list_from`). A screen is only ever
    /// one of the two, so `selected` never mixes interpretations.
    pub(crate) fn selectable_bounds(&self) -> Option<(usize, usize)> {
        // A focused attribute *table* owns the cursor: its rows are the only selectable thing on the
        // Overview, and the pane above it is a fixed block that never needs one. Checked first so a
        // screen that has both (none does today, but the shape allows it) sends the cursor to the
        // pane the user is actually on. The range line is a focus stop of its own and has no rows.
        //
        // The bounds are the **key** rows: a section title, a repeated column header and a spacer are
        // structure, and a cursor that can land on them offers an action (`p`) that has nothing to
        // act on. `move_selection` steps between them; these bounds only clamp.
        if let Focus::AttrTable(section) = self.effective_focus() {
            let keys = self.attr_key_indices(section);
            return match (keys.first(), keys.last()) {
                (Some(first), Some(last)) => Some((*first, *last)),
                _ => None,
            };
        }
        if let Some(table) = &self.snapshot.table {
            return (!table.rows.is_empty()).then(|| (0, table.rows.len() - 1));
        }
        let first = self.snapshot.list_from?;
        let len = self.snapshot.lines.len();
        (first < len).then(|| (first, len - 1))
    }

    /// The attribute pane's table, when one is shown.
    pub(crate) fn attr_table(&self) -> Option<&PaneTable> {
        self.snapshot
            .detail
            .as_ref()
            .filter(|pane| pane.style == DetailStyle::Pane)?
            .table
            .as_ref()
    }

    /// The attribute pane's sections, each as the row index of its title. One per scan unit, in
    /// display order — and one Tab stop apiece.
    pub(crate) fn attr_sections(&self) -> Vec<usize> {
        let Some(table) = self.attr_table() else {
            return Vec::new();
        };
        table
            .kinds
            .iter()
            .enumerate()
            .filter(|(_, kind)| **kind == AttrRow::Section)
            .map(|(index, _)| index)
            .collect()
    }

    /// The scan unit a section names, for the pane title — the leading word of its title row.
    pub(crate) fn attr_section_label(&self, section: usize) -> Option<String> {
        let table = self.attr_table()?;
        let row = table.data.rows.get(*self.attr_sections().get(section)?)?;
        row.first()
            .and_then(|title| title.split_whitespace().next())
            .map(str::to_owned)
    }

    /// Row indices of `section`'s key rows, in display order — the only rows its cursor may land on.
    /// Empty for a section index the current measurement no longer has.
    pub(crate) fn attr_key_indices(&self, section: usize) -> Vec<usize> {
        let Some(table) = self.attr_table() else {
            return Vec::new();
        };
        let starts = self.attr_sections();
        let Some(&start) = starts.get(section) else {
            return Vec::new();
        };
        let end = starts
            .get(section + 1)
            .copied()
            .unwrap_or(table.data.rows.len());
        (start..end)
            .filter(|index| table.key_at(*index).is_some())
            .collect()
    }

    /// The section the cursor belongs to — the focused one, else the first.
    fn focused_attr_section(&self) -> usize {
        match self.effective_focus() {
            Focus::AttrTable(section) => section,
            _ => 0,
        }
    }

    /// Move the cursor by `delta` **key rows**, skipping the structure between them and **hopping to
    /// the next section** when it is already at the edge in that direction. Returns whether it moved.
    ///
    /// Two motions that compose rather than compete: the arrows walk the whole pane top to bottom,
    /// and Tab jumps a section at a time. Stopping dead at a section boundary would have made the
    /// arrows unable to reach the pane's second table at all without reaching for Tab, which is the
    /// kind of dead end a list should not have.
    ///
    /// A move that merely *overshoots* — `PageDown` from the middle of a section — lands on that
    /// section's last key first. Only a press with nowhere left to go inside the section crosses the
    /// boundary, so the edge is a pause rather than a wall.
    pub(crate) fn move_attr_cursor(&mut self, delta: isize) -> bool {
        let section = self.focused_attr_section();
        let keys = self.attr_key_indices(section);
        if keys.is_empty() {
            return self.hop_attr_section(section, delta);
        }
        // Where the cursor is among this section's key rows — or, if it is outside them (Tab has just
        // moved here, or a refresh parked it at 0), the nearest one.
        let current = keys
            .iter()
            .position(|index| *index == self.selected)
            .or_else(|| keys.iter().position(|index| *index >= self.selected))
            .unwrap_or(keys.len() - 1) as isize;
        let next = (current + delta).clamp(0, keys.len() as isize - 1);
        if next != current {
            self.selected = keys[next as usize];
            return true;
        }
        self.hop_attr_section(section, delta)
    }

    /// Move the focus to the nearest section in `delta`'s direction that has keys, parking the cursor
    /// on the key next to the boundary just crossed. `false` at the pane's ends, where the cursor
    /// stays put.
    ///
    /// Sections with no keys are stepped over rather than focused: a stop whose cursor has nowhere to
    /// land would swallow a keypress and look like the arrows had stopped working.
    fn hop_attr_section(&mut self, from: usize, delta: isize) -> bool {
        let total = self.attr_sections().len() as isize;
        let step = if delta >= 0 { 1 } else { -1 };
        let mut index = from as isize + step;
        while index >= 0 && index < total {
            let keys = self.attr_key_indices(index as usize);
            let landing = if step > 0 { keys.first() } else { keys.last() };
            if let Some(&row) = landing {
                self.focus = Focus::AttrTable(index as usize);
                self.selected = row;
                return true;
            }
            index += step;
        }
        false
    }

    /// Park the cursor on a key row of the focused section. Called when a fresh measurement lands
    /// (row 0 of a grouped table is a title, and the rows have moved) and when Tab lands on a section,
    /// so the highlight is always inside the section the ring is on.
    pub(crate) fn snap_attr_cursor(&mut self) {
        let Focus::AttrTable(section) = self.effective_focus() else {
            return;
        };
        let keys = self.attr_key_indices(section);
        if let Some(index) = keys
            .iter()
            .find(|index| **index >= self.selected)
            .or_else(|| keys.first())
        {
            self.selected = *index;
        }
    }

    /// The attribute key under the pane's cursor — what a promotion toggle acts on. `None` on a
    /// section header, which names a scan unit rather than a key.
    pub(crate) fn selected_attr_key(&self) -> Option<String> {
        let table = self.attr_table()?;
        let (first, last) = self.selectable_bounds()?;
        table
            .key_at(self.selected.clamp(first, last))
            .map(str::to_owned)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{MetricDetail, SeriesData};

    #[test]
    fn stale_query_results_are_discarded() {
        let mut app = App::new();
        app.generation = 2;
        app.route = Route::Logs;
        app.apply(QueryResult {
            generation: 1,
            screen: Screen::Logs,
            result: Ok(Snapshot::message("old", "old")),
        });
        assert_ne!(app.snapshot.title, "old");
    }

    #[test]
    fn mascot_is_hidden_by_default() {
        assert!(!App::new().show_mascot);
    }

    #[test]
    fn sticky_waterfall_is_on_by_default() {
        assert!(App::new().sticky_waterfall);
    }

    #[test]
    fn auto_refresh_is_off_by_default() {
        assert!(!App::new().auto_refresh);
    }

    #[test]
    fn the_query_caret_is_clamped_to_the_buffer_it_is_read_against() {
        let mut app = App::new();
        app.route = Route::Logs;
        app.set_active_query("{service.name=\"cart\"}");
        assert_eq!(
            app.query_caret(),
            21,
            "a fresh query parks the caret at its end"
        );

        // The active buffer changes with the screen, so a caret left over from another screen's (longer)
        // query must clamp rather than slice out of bounds.
        app.route = Route::Metrics; // an empty buffer
        assert_eq!(app.query_caret(), 0);
        assert_eq!(app.query_before_caret(), "");

        // And a caret that would land inside a multi-byte character snaps back to its start.
        app.set_active_query("café");
        app.query_cursor = 4; // the middle of the two-byte `é`
        assert_eq!(app.query_caret(), 3);
        assert_eq!(app.query_before_caret(), "caf");
    }

    #[test]
    fn selectable_bounds_track_the_list_region() {
        let mut app = App::new();
        // Not a list -> no selection, keys scroll instead.
        app.snapshot.list_from = None;
        assert_eq!(app.selectable_bounds(), None);

        // Header line 0, three selectable rows at 1..=3.
        app.snapshot.lines = vec!["header".into(), "a".into(), "b".into(), "c".into()];
        app.snapshot.list_from = Some(1);
        assert_eq!(app.selectable_bounds(), Some((1, 3)));

        // Header-only list (0 matches) has no selectable rows.
        app.snapshot.lines = vec!["header".into()];
        app.snapshot.list_from = Some(1);
        assert_eq!(app.selectable_bounds(), None);
    }

    #[test]
    fn apply_lands_the_cursor_on_the_first_selectable_row() {
        let mut app = App::new();
        app.route = Route::Traces;
        app.generation = 7;
        app.selected = 0;
        let snapshot = Snapshot {
            lines: vec!["header".into(), "a".into(), "b".into(), "c".into()],
            list_from: Some(1),
            ..Default::default()
        };
        app.apply(QueryResult {
            generation: 7,
            screen: Screen::Traces,
            result: Ok(snapshot),
        });
        assert_eq!(app.selected, 1);
    }

    #[test]
    fn metric_detail_chart_follows_a_refresh_so_pan_zoom_redraws() {
        let mut app = App::new();
        app.generation = 3;
        // A metric detail opened over an old window (two points).
        app.route = Route::MetricDetail {
            detail: MetricDetail {
                labels: "__name__=m,svc=api".to_owned(),
                query: "m".to_owned(),
                points: vec![(0, 1.0), (10, 2.0)],
            },
        };
        app.metric_cursor = 1;
        // A refresh (as a pan/zoom would trigger) lands a Metrics result whose matching series carries
        // the new window's points.
        let mut snapshot = Snapshot::message("PromQL", "");
        snapshot.series = vec![
            SeriesData {
                labels: "__name__=m,svc=api".to_owned(),
                points: vec![(100, 5.0), (110, 6.0), (120, 7.0)],
            },
            SeriesData {
                labels: "__name__=other".to_owned(),
                points: vec![(100, 9.0)],
            },
        ];
        app.apply(QueryResult {
            generation: 3,
            screen: Screen::Metrics,
            result: Ok(snapshot),
        });
        let detail = app.route_metric_detail().expect("still a metric detail");
        assert_eq!(
            detail.points,
            vec![(100, 5.0), (110, 6.0), (120, 7.0)],
            "the detail chart adopts the matching series' new-window points"
        );
        // The cursor stays in range of the (now longer) series.
        assert!(app.metric_cursor < detail.points.len());

        // A refresh whose window no longer contains the series clears the plot (honest empty).
        let mut empty = Snapshot::message("PromQL", "");
        empty.series = vec![SeriesData {
            labels: "__name__=unrelated".to_owned(),
            points: vec![(0, 1.0)],
        }];
        app.apply(QueryResult {
            generation: 3,
            screen: Screen::Metrics,
            result: Ok(empty),
        });
        assert!(app.route_metric_detail().unwrap().points.is_empty());
    }

    #[test]
    fn a_fast_refresh_never_flashes_the_banner() {
        // The common case: the query lands well inside the delay, so nothing is ever drawn.
        let mut app = App::new();
        app.begin_loading(Refresh::Interactive);
        assert!(app.loading_banner().is_none());
        app.end_loading();
        assert!(app.loading_banner().is_none(), "and nothing lingers after");
    }

    #[test]
    fn a_wait_past_the_delay_raises_the_banner() {
        let mut app = App::new();
        app.begin_loading(Refresh::Interactive);
        // Backdate the start rather than sleeping: the banner is a pure function of elapsed time.
        app.loading_since = Some(Instant::now() - LOADING_BANNER_AFTER);
        let elapsed = app.loading_banner().expect("the delay has passed");
        assert!(elapsed >= LOADING_BANNER_AFTER);
    }

    #[test]
    fn a_refused_key_raises_the_banner_without_waiting_out_the_delay() {
        // A key that visibly did nothing has to be explained immediately — waiting for the timer
        // would leave the user looking at a UI that has apparently frozen for no stated reason.
        let mut app = App::new();
        app.begin_loading(Refresh::Interactive);
        app.input_refused = true;
        assert!(app.loading_banner().is_some());
    }

    #[test]
    fn a_background_load_arms_the_banner_but_not_the_lock() {
        // The timer's tick is still worth announcing once it drags; it just does not take the keys.
        let mut app = App::new();
        app.begin_loading(Refresh::Background);
        assert!(!app.input_locked);
        app.loading_since = Some(Instant::now() - LOADING_BANNER_AFTER);
        assert!(app.loading_banner().is_some());
    }

    #[test]
    fn landing_a_stale_result_still_releases_the_keyboard() {
        // `apply` bails out early on a superseded generation. The release happens first: the query
        // this lock belonged to is over either way, and skipping it would strand the keyboard.
        let mut app = App::new();
        app.begin_loading(Refresh::Interactive);
        app.apply(QueryResult {
            generation: app.generation.wrapping_sub(1),
            screen: Screen::Overview,
            result: Ok(Snapshot::message("Overview", "")),
        });
        assert!(!app.loading && !app.input_locked);
        assert!(app.loading_banner().is_none());
    }
}

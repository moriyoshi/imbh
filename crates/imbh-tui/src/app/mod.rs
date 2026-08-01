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
use std::time::Instant;

use imbh::PageCursor;

use crate::chart::ChartGeometry;
use crate::completion::Completion;
use crate::mascot::Mascot;
use crate::model::{
    ExemplarMarker, Focus, LogCorrelation, MetricNode, Mode, NavEntry, QueryResult, Route, Screen,
    Snapshot, TreeRowRef,
};
use crate::waterfall::TraceDetail;

pub(crate) struct App {
    /// The current view (single source of truth); the `screen` is derived from it.
    pub(crate) route: Route,
    pub(crate) query: [String; 4],
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
    /// Editable buffers for the absolute-range form (UTC `YYYY-MM-DD HH:MM:SS`) and which field has
    /// focus (0 = start, 1 = end), plus the last parse error to surface in the form.
    pub(crate) abs_start: String,
    pub(crate) abs_end: String,
    pub(crate) abs_field: usize,
    pub(crate) abs_error: Option<String>,
    /// Background auto-refresh, off by default; toggled with space. Manual/query/switch refreshes
    /// always run regardless.
    pub(crate) auto_refresh: bool,
    pub(crate) loading: bool,
    pub(crate) pending_refresh: bool,
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
            mode: Mode::Normal,
            range_index,
            range_cursor: range_index,
            menu_cursor: 0,
            abs_window: None,
            abs_start: String::new(),
            abs_end: String::new(),
            abs_field: 0,
            abs_error: None,
            auto_refresh: false,
            loading: false,
            pending_refresh: false,
            generation: 0,
            selected: 0,
            snapshot: Snapshot::message("Overview", "Loading..."),
            last_error: None,
            last_refresh: Instant::now(),
            scroll: 0,
            max_scroll: Cell::new(0),
            page_rows: Cell::new(1),
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

    pub(crate) fn apply(&mut self, result: QueryResult) {
        self.loading = false;
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
    }

    /// The inclusive `[first, last]` selection-index range the row cursor may occupy, or `None` when
    /// the primary pane is not navigable. For a table the index is into `table.rows` (`first == 0`);
    /// for a list it is an absolute index into `lines` (`first == list_from`). A screen is only ever
    /// one of the two, so `selected` never mixes interpretations.
    pub(crate) fn selectable_bounds(&self) -> Option<(usize, usize)> {
        if let Some(table) = &self.snapshot.table {
            return (!table.rows.is_empty()).then(|| (0, table.rows.len() - 1));
        }
        let first = self.snapshot.list_from?;
        let len = self.snapshot.lines.len();
        (first < len).then(|| (first, len - 1))
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
}

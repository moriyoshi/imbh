//! The view model: what the UI shows and the messages that change it.
//!
//! Plain data only — [`Route`] (the navigable view), [`Snapshot`] (a query result rendered for a
//! pane), the Metrics catalog tree nodes, the input [`Mode`]/[`Focus`] enums, and the [`Update`]
//! messages background tasks deliver. The state machine over them lives in [`crate::app`].

use std::time::Duration;

use imbh::PageCursor;

use crate::waterfall::{SpanRecord, TraceDetail, Waterfall};

/// Selectable relative time ranges: `(label, lookback, step)`. The step is paired with each range to
/// keep the sample count bounded regardless of how wide the window is.
pub(crate) const TIME_RANGES: &[(&str, Duration, Duration)] = &[
    ("5m", Duration::from_secs(5 * 60), Duration::from_secs(5)),
    ("15m", Duration::from_secs(15 * 60), Duration::from_secs(30)),
    ("1h", Duration::from_secs(60 * 60), Duration::from_secs(30)),
    (
        "3h",
        Duration::from_secs(3 * 60 * 60),
        Duration::from_secs(120),
    ),
    (
        "6h",
        Duration::from_secs(6 * 60 * 60),
        Duration::from_secs(300),
    ),
    (
        "12h",
        Duration::from_secs(12 * 60 * 60),
        Duration::from_secs(600),
    ),
    (
        "24h",
        Duration::from_secs(24 * 60 * 60),
        Duration::from_secs(900),
    ),
    (
        "7d",
        Duration::from_secs(7 * 24 * 60 * 60),
        Duration::from_secs(3600),
    ),
];

/// How long a query has to stay in flight before the loading banner appears.
///
/// Short enough that a wait the user notices is explained, long enough that the ordinary sub-second
/// refresh never flashes a box on screen. The banner also appears immediately — whatever the elapsed
/// time — once a keystroke has actually been refused, because a swallowed key with no visible cause
/// reads as a hang. See [`App::loading_banner`](crate::app::App::loading_banner).
pub(crate) const LOADING_BANNER_AFTER: Duration = Duration::from_secs(2);

/// How long each frame of the loading banner's spinner is held.
///
/// Also the event loop's wake interval while the banner is up: the spinner is derived from elapsed
/// time rather than a counter, so it only advances when something redraws.
pub(crate) const SPINNER_FRAME: Duration = Duration::from_millis(120);

/// Who asked for a refresh, which decides whether it takes the keyboard.
///
/// The distinction exists for one reason: an [`Interactive`](Refresh::Interactive) load locks input
/// until it lands, and applying that to the auto-refresh timer would be a trap. A background tick the
/// user never asked for would hold the keyboard for the length of every query, and on the slow corpus
/// this guard is for, that is most of the session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Refresh {
    /// The user asked for this one: Enter on a query, a screen switch, a range change, `r`.
    Interactive,
    /// The auto-refresh timer asked for this one. Never locks input.
    Background,
}

/// Transient input overlays on top of the current [`Route`]. Only `Normal` lets background
/// auto-refresh run, and overlays never participate in the navigation history.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    Normal,
    /// The top menu bar is activated (Midnight-Commander F9): the cursor/Tab move a highlight across
    /// the screen menu and the time-range item, Enter activates, Esc/F9 dismisses.
    Menu,
    Editing,
    TimeRange,
    /// The absolute-time window form (two datetime fields), opened from the time-range dropdown.
    AbsoluteRange,
}

/// The stop the keyboard focus ring is on. `Tab`/`Shift+Tab` cycle it in reading order (the menu bar
/// left-to-right, then the content top-to-bottom): `Menu(..)` (the screen items) → `TimeRange` (the
/// menu-bar time selector) → `Query` → `Primary` (main list/table), wrapping. The highlight follows it
/// and `Enter` activates the focused stop. Arrow keys are unaffected — they always drive the main list.
/// `Query` is only reachable on views that have a query pane (see `App::has_query`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Focus {
    /// A screen item in the menu bar, indexed into `Screen::ORDER` (`0..Screen::ORDER.len()`). `Enter`
    /// switches to that screen, like the F9 menu.
    Menu(usize),
    /// The menu bar's rightmost item, the time-window selector.
    TimeRange,
    /// The query editor pane.
    Query,
    /// The main list/table (or a detail route's body).
    Primary,
    /// The Overview attribute pane's **range line**. `Enter` opens the window it is measured over —
    /// a property of the pane, reached through the pane rather than through a key of its own, and
    /// through the line that displays it rather than the pane as a whole.
    AttrRange,
    /// One **section** of the Overview attribute pane's table, by index: the DB-wide roll-up is 0 and
    /// each table that has segments follows. Takes the row cursor within that section, and `p`
    /// promotes or demotes the key under it.
    ///
    /// A stop per section rather than one for the whole table, because each section *is* a table —
    /// its own totals, its own sigma — and Tab is how a reader moves between them without scrolling
    /// past every key of the one above. Separate from [`Focus::AttrRange`] for the same reason at one
    /// level up: Enter cannot mean both "change the range" and "act on this row".
    AttrTable(usize),
}

/// Structured fields of a log entry, kept alongside the rendered rows so the detail view can show the
/// full record and the trace-id jump has a real id to navigate to.
#[derive(Debug, Clone)]
pub(crate) struct LogRecord {
    pub(crate) time_ns: i64,
    pub(crate) severity: String,
    pub(crate) service: Option<String>,
    pub(crate) body: String,
    pub(crate) trace_id: Option<String>,
    pub(crate) span_id: Option<String>,
    pub(crate) attributes: Vec<(String, String)>,
    pub(crate) resource: Vec<(String, String)>,
    pub(crate) scope: Vec<(String, String)>,
}

#[derive(Debug, Clone)]
pub struct Options {
    pub refresh_interval: Duration,
    pub lookback: Duration,
    pub step: Duration,
    /// When `Some((start_ns, end_ns))`, the query window is this fixed absolute span instead of the
    /// rolling `now - lookback .. now`; the step is then derived from the span. Set from the TUI's
    /// absolute-time picker (`App::abs_window`).
    pub window: Option<(i64, i64)>,
    pub max_series: usize,
    pub max_rows: usize,
    pub ascii: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            refresh_interval: Duration::from_secs(5),
            lookback: Duration::from_secs(15 * 60),
            step: Duration::from_secs(30),
            window: None,
            max_series: 100,
            max_rows: 100,
            ascii: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Screen {
    Overview,
    Metrics,
    Traces,
    Logs,
}

impl Screen {
    /// Left-to-right order of the screen menu (the F9 menu bar navigates this plus a trailing
    /// time-range item).
    pub(crate) const ORDER: [Screen; 4] = [
        Screen::Overview,
        Screen::Metrics,
        Screen::Traces,
        Screen::Logs,
    ];

    pub(crate) fn title(self) -> &'static str {
        match self {
            Self::Overview => "Overview",
            Self::Metrics => "Metrics",
            Self::Traces => "Traces",
            Self::Logs => "Logs",
        }
    }

    pub(crate) fn index(self) -> usize {
        Self::ORDER.iter().position(|s| *s == self).unwrap_or(0)
    }
}

/// Number of items on the menu bar: every screen plus the trailing time-range selector. The range
/// item is the last index.
pub(crate) const MENU_LEN: usize = Screen::ORDER.len() + 1;

/// How a [`DetailPane`] is drawn, which follows from what it *is*.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DetailStyle {
    /// A preview strip: a bare title line, no borders — so waterfall bars sit flush against the pane's
    /// edges — and no scroll of its own. Overflow is reported in the title, and the full view is one
    /// Enter away. The Traces screen's waterfall.
    Preview,
    /// A pane in its own right: bordered, given whatever the primary does not need, and **it** takes
    /// the screen's scroll — because it, not the primary, is the long content. The Overview's
    /// attribute statistics.
    Pane,
}

/// What one row of a [`PaneTable`] is.
///
/// A grouped table holds three kinds of row and they are acted on differently — `p` promotes the key
/// under the cursor, and the other two have no key. That has to be a *fact* about the row rather than
/// something inferred from its text, which is why it is carried alongside rather than guessed at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AttrRow {
    /// A scan unit's name and totals — the title above a section.
    Section,
    /// The column names for the section above it. Repeated per section rather than pinned once at the
    /// top: each section is its own table, and its numbers are measured against its own totals.
    Header,
    /// One attribute key, promotable by this name.
    Key(String),
    /// A blank line, separating one section from the one above it. A row of its own rather than an
    /// extra line emitted while rendering a title, so the row indices the cursor and the scroll offset
    /// use stay one-to-one with what is drawn.
    Blank,
}

/// A table plus what each of its rows *is*.
///
/// One struct rather than two fields so "aligned with `data.rows`" is a property of the type instead
/// of a comment two call sites have to honour.
#[derive(Debug, Clone)]
pub(crate) struct PaneTable {
    pub(crate) data: TableData,
    pub(crate) kinds: Vec<AttrRow>,
}

impl PaneTable {
    /// The attribute key row `index` promotes, if it is a key row at all.
    pub(crate) fn key_at(&self, index: usize) -> Option<&str> {
        match self.kinds.get(index) {
            Some(AttrRow::Key(key)) => Some(key.as_str()),
            _ => None,
        }
    }
}

/// A secondary result pane rendered below the primary list. Its own title and lines, rendered from
/// the top; [`DetailStyle`] decides whether it is a preview strip or a pane in its own right.
#[derive(Debug, Clone)]
pub(crate) struct DetailPane {
    pub(crate) title: String,
    pub(crate) lines: Vec<String>,
    /// When set, a trace waterfall whose bars reflow to fill the pane width at draw time; `lines` is
    /// then unused. Placeholders and errors leave this `None` and fall back to `lines`.
    pub(crate) waterfall: Option<Waterfall>,
    /// When set (a [`DetailStyle::Pane`] only), the pane's body is a column-aligned table, rendered
    /// below `lines` and scrolled by the pane's own scroll. `lines` is then the short prose above
    /// it — what was measured, and anything the measurement could not cover.
    pub(crate) table: Option<PaneTable>,
    pub(crate) style: DetailStyle,
}

/// Floor for the pan/zoom query-window span (1 second): zooming in never collapses the window below it.
pub(crate) const MIN_WINDOW_NS: i64 = 1_000_000_000;

/// Ceiling for the pan/zoom query-window span (~1 year): zooming out never widens past it (and keeps
/// the center ± half-span arithmetic well clear of `i64` overflow).
pub(crate) const MAX_WINDOW_NS: i64 = 366 * 24 * 3_600 * 1_000_000_000;

/// Column-aligned tabular data for the primary pane (the Metrics screen). Rendered with a header row
/// and a selectable highlighted row; the selection cursor indexes `rows` directly.
#[derive(Debug, Clone)]
pub(crate) struct TableData {
    pub(crate) header: Vec<String>,
    pub(crate) rows: Vec<Vec<String>>,
    /// Display width of the widest cell per column, measured once here rather than per frame.
    ///
    /// The measurement walks every cell of every row calling `UnicodeWidthStr::width`, and the rows
    /// do not change between frames — but `draw` takes `&App`, so a renderer that measured them had
    /// no way to keep the result. Computing it at construction moves the cost from once-per-frame to
    /// once-per-result, which matters most on the catalog tree, whose row count grows with the
    /// number of metrics and their expanded dimensions.
    pub(crate) widths: Vec<usize>,
}

impl TableData {
    /// Build a table, measuring its column widths up front.
    pub(crate) fn new(header: Vec<String>, rows: Vec<Vec<String>>) -> TableData {
        let widths = crate::ui::metrics::column_widths(&header, rows.iter());
        TableData {
            header,
            rows,
            widths,
        }
    }
}

/// A metric in the catalog tree, expandable to its groupable dimensions.
#[derive(Debug, Clone)]
pub(crate) struct MetricNode {
    pub(crate) name: String,
    pub(crate) kind: String,
    pub(crate) unit: String,
    pub(crate) temporality: String,
    pub(crate) expanded: bool,
    /// Selects the whole metric (no per-dimension filter) for visualization. The only way to select a
    /// metric that has no dimensions to check series under; toggled via the `(no dimensions)` row.
    pub(crate) whole_selected: bool,
    /// `None` until the dimensions have been discovered (by running the metric's base query and
    /// reading the returned series' labels); `Some` afterwards, possibly empty.
    pub(crate) dims: Option<Vec<DimNode>>,
    pub(crate) loading: bool,
}

/// A groupable label dimension under a metric (e.g. `by service`), expandable to its distinct values.
#[derive(Debug, Clone)]
pub(crate) struct DimNode {
    pub(crate) label: String,
    pub(crate) values: Vec<String>,
    pub(crate) expanded: bool,
    /// The checked value (index into `values`), or `None`. Exclusive within the axis — at most one
    /// value is selected per dimension.
    pub(crate) selected: Option<usize>,
}

/// Which tree node a flattened catalog row maps to, so a key press acts on the right node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TreeRowRef {
    Metric(usize),
    Dim(usize, usize),
    Value(usize, usize, usize),
    /// The `(no dimensions)` row under a metric with no dimensions — a checkbox that selects the whole
    /// metric.
    NoDims(usize),
}

/// One PromQL result series retained alongside the rendered table row, so the detailed time-series
/// viewer can plot the selected series' full `(timestamp_ns, value)` history. `series[i]` aligns with
/// the Metrics series table's row `i`.
#[derive(Debug, Clone, Default)]
pub(crate) struct SeriesData {
    pub(crate) labels: String,
    pub(crate) points: Vec<(i64, f64)>,
}

/// The selected series shown in the detailed time-series viewer (the `Route::MetricDetail` content).
/// Cloned on open so it survives background refreshes, like [`LogRecord`] for the log detail view.
#[derive(Debug, Clone)]
pub(crate) struct MetricDetail {
    pub(crate) labels: String,
    pub(crate) query: String,
    pub(crate) points: Vec<(i64, f64)>,
}

/// The current navigable view — the single source of truth for what the content area shows, and the
/// unit the back/forward history moves through. Transient input overlays are [`Mode`], not routes.
/// The detail views carry their own self-contained data so the history captures them for free, and
/// they render as ordinary content beneath the always-visible menu bar (they are not modal).
#[derive(Debug, Clone)]
pub(crate) enum Route {
    Overview,
    /// The Metrics list: the catalog tree (empty query) or the PromQL series table.
    Metrics,
    /// The detailed time-series viewer for one series.
    MetricDetail {
        detail: MetricDetail,
    },
    Traces,
    /// The full-screen view of one trace: a scrollable, span-selectable waterfall plus the selected
    /// span's summary. Opened with Enter on the Traces list.
    TraceDetail {
        detail: TraceDetail,
    },
    /// The full field detail of one span within a trace. Opened with Enter on the trace detail's
    /// waterfall; carries the trace id so the span→log correlation has both ids.
    SpanDetail {
        trace_id: String,
        span: SpanRecord,
    },
    Logs,
    /// The full detail of one log record.
    LogDetail {
        record: LogRecord,
    },
}

impl Route {
    /// The screen this view belongs to (drives the menu highlight and the per-screen query buffer).
    pub(crate) fn screen(&self) -> Screen {
        match self {
            Route::Overview => Screen::Overview,
            Route::Metrics | Route::MetricDetail { .. } => Screen::Metrics,
            Route::Traces | Route::TraceDetail { .. } | Route::SpanDetail { .. } => Screen::Traces,
            Route::Logs | Route::LogDetail { .. } => Screen::Logs,
        }
    }

    /// The list route a screen switch lands on.
    pub(crate) fn list(screen: Screen) -> Route {
        match screen {
            Screen::Overview => Route::Overview,
            Screen::Metrics => Route::Metrics,
            Screen::Traces => Route::Traces,
            Screen::Logs => Route::Logs,
        }
    }

    /// Whether this route renders as full-content detail (no query pane, its own hint bar).
    pub(crate) fn is_detail(&self) -> bool {
        matches!(
            self,
            Route::MetricDetail { .. }
                | Route::LogDetail { .. }
                | Route::TraceDetail { .. }
                | Route::SpanDetail { .. }
        )
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct Snapshot {
    pub(crate) title: String,
    pub(crate) lines: Vec<String>,
    pub(crate) chart: Vec<u64>,
    pub(crate) detail: Option<DetailPane>,
    /// When `Some(n)`, the primary pane is a cursor-navigable list: lines `[0, n)` are header/info
    /// text and lines `[n, len)` are selectable rows. `None` keeps the plain scrolled view.
    pub(crate) list_from: Option<usize>,
    /// Structured log records, aligned to the selectable rows (`log_records[i]` ↔ `lines[list_from+i]`).
    /// Populated only on the Logs screen; drives the detail view and trace-id navigation.
    pub(crate) log_records: Vec<LogRecord>,
    /// When `Some`, the primary pane renders as a selectable table (the Metrics screen). Takes
    /// precedence over `list_from`; the selection cursor then indexes `table.rows`.
    pub(crate) table: Option<TableData>,
    /// PromQL result series aligned to `table.rows` (Metrics screen only); drives the detailed
    /// time-series viewer opened with Enter on a selected series.
    pub(crate) series: Vec<SeriesData>,
    /// Resume cursor for the next (older) log page, echoed from [`imbh::LogPage::next`]. `Some` only on
    /// the Logs screen when a full page was returned (more rows may follow); drives older/newer paging.
    /// `Option<PageCursor>` is `Default` (`None`), so the `#[derive(Default)]` above still holds.
    pub(crate) next_cursor: Option<PageCursor>,
}

/// Which window the range form is editing.
///
/// One form, two destinations. The Overview's attribute statistics answer a different question from
/// the panels — "what does this corpus look like", not "what happened just now" — and the range that
/// makes sense for one rarely makes sense for the other: a `promote` list is chosen from a week, while
/// a log list is read over the last fifteen minutes. So the attribute window is independent, and
/// [`Query`](AbsTarget::Query) is what every other pane follows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AbsTarget {
    /// The query window every panel is evaluated over.
    Query,
    /// The window the Overview's attribute statistics are measured over. Clearing both fields
    /// measures all of time, which is also where it starts.
    Attributes,
}

/// The window the Overview's attribute statistics are measured over, and the key their cached
/// measurement is held under: an absolute `[start_ns, end_ns]`, or `None` for **all sealed segments**,
/// which is the default.
///
/// All of time by default because that is the question the pane answers: a `promote` list is chosen
/// from everything the database holds, not from the last fifteen minutes. It is deliberately *not*
/// derived from the query range — every other pane rolls with the wall clock, and keying a corpus scan
/// on something that moves every tick would turn it into a treadmill. This changes exactly when the
/// user changes it, which is when the answer actually differs.
pub(crate) type AttrWindow = Option<(i64, i64)>;

/// Minimum interval between attribute measurements over one range selection.
///
/// The measurement is a scan of the corpus, not a gauge: at the auto-refresh cadence it would spend
/// its time re-measuring a database that has barely changed. Up to a minute of staleness is the right
/// trade for a diagnostic, and `r` clears the cache outright for anyone who wants it now.
pub(crate) const ATTR_MEASURE_INTERVAL: Duration = Duration::from_secs(60);

/// The Overview's most recent attribute measurement: what it was measured over, when, and the pane it
/// rendered to.
#[derive(Debug, Clone)]
pub(crate) struct AttrStats {
    pub(crate) key: AttrWindow,
    pub(crate) measured_at: std::time::Instant,
    pub(crate) pane: DetailPane,
}

/// A trace/span correlation filter applied on top of a Logs query — the target of a trace→log
/// drill-down. Held in [`App`](crate::app::App) and captured by the navigation history so Back
/// restores the drill-down.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LogCorrelation {
    /// Lowercase-hex trace id (parsed back to [`imbh::TraceId`] when the query is built).
    pub(crate) trace_id: String,
    /// Optional lowercase-hex span id, narrowing the correlation to a single span.
    pub(crate) span_id: Option<String>,
}

/// One metric exemplar reduced to what the exemplar→trace jump needs: when it was recorded and the
/// trace it links to. Only exemplars carrying a trace id become markers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExemplarMarker {
    pub(crate) time_ns: i64,
    /// Lowercase-hex trace id the exemplar points at.
    pub(crate) trace_id: String,
}

impl Snapshot {
    pub(crate) fn message(title: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            lines: vec![message.into()],
            chart: Vec::new(),
            detail: None,
            list_from: None,
            log_records: Vec::new(),
            table: None,
            series: Vec::new(),
            next_cursor: None,
        }
    }
}

pub(crate) struct QueryResult {
    pub(crate) generation: u64,
    pub(crate) screen: Screen,
    pub(crate) result: Result<Snapshot, String>,
}

/// Messages delivered to the event loop from background tasks.
pub(crate) enum Update {
    /// A completed (or failed) panel query.
    Query(QueryResult),
    /// Completion vocabulary (metric names) fetched from the catalog.
    Vocabulary(Vec<String>),
    /// The waterfall for the selected trace, fetched on demand. `generation`/`trace_id` guard against
    /// applying a stale result after the selection or query moved on. `trace` is the materialized trace
    /// behind the pane (`None` when the fetch failed or the trace is gone); retained by the app so
    /// Enter can open the full trace detail without a second fetch.
    Waterfall {
        generation: u64,
        trace_id: String,
        detail: DetailPane,
        trace: Option<TraceDetail>,
    },
    /// The discovered dimensions for a catalog metric, loaded when the metric is first expanded.
    MetricDims { metric: String, dims: Vec<DimNode> },
    /// The discovered log label names (Logs `{…}` selector completion vocabulary), fetched when the
    /// caret first enters a label-name position on the Logs screen.
    LogLabels(Vec<String>),
    /// The discovered distinct values for one log label (Logs quoted-matcher completion vocabulary),
    /// fetched when the caret first enters that label's value position.
    LogLabelValues { label: String, values: Vec<String> },
    /// The Overview's attribute statistics, measured off the event loop because the measurement is a
    /// scan of every sealed segment's attribute columns rather than a query. `key` is the range
    /// selection it was measured for, so a result for a window the user has since left is dropped
    /// rather than shown under the current one.
    AttributeStats { key: AttrWindow, pane: DetailPane },
    /// The promoted attribute keys the daemon ended up with after a `p`, or why it would not change
    /// them. Carried back rather than assumed: the daemon filters keys that collide with a built-in
    /// column name, so what was asked for and what is in effect can differ.
    Promoted(Result<Vec<String>, String>),
    /// Exemplar→trace markers for an open metric-detail view, fetched when it opens. `labels`/`query`
    /// identify the series the fetch was issued for, so a stale result (the view moved on) is dropped.
    Exemplars {
        labels: String,
        query: String,
        markers: Vec<ExemplarMarker>,
    },
}

/// A snapshot of the navigation-relevant state for the browser-style back/forward history: the
/// [`Route`] (which carries the self-contained detail data) plus the per-screen query buffers and the
/// cursor context. Transient overlays, the time range, and the catalog tree are deliberately excluded
/// so Back/Forward move only through views (the tree is preserved separately, by name).
#[derive(Clone)]
pub(crate) struct NavEntry {
    pub(crate) route: Route,
    pub(crate) query: [String; Screen::ORDER.len()],
    pub(crate) metric_cursor: usize,
    pub(crate) focus_trace_id: Option<String>,
    pub(crate) selected: usize,
    pub(crate) scroll: u16,
    /// The selected span row of a `Route::TraceDetail` view (view context, like `metric_cursor`).
    pub(crate) span_cursor: usize,
    /// The trace→log drill-down active in this view, so Back into a correlated Logs view restores it.
    pub(crate) log_correlation: Option<LogCorrelation>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{sample_log_record, sample_trace};
    use crate::waterfall::build_trace_detail;

    #[test]
    fn route_maps_to_screen_and_reports_detail() {
        assert_eq!(Route::Overview.screen(), Screen::Overview);
        assert_eq!(Route::Metrics.screen(), Screen::Metrics);
        assert_eq!(Route::Traces.screen(), Screen::Traces);
        assert_eq!(Route::Logs.screen(), Screen::Logs);
        assert!(!Route::Metrics.is_detail());
        assert!(!Route::Logs.is_detail());

        // Detail routes belong to their parent screen and report `is_detail`.
        let md = Route::MetricDetail {
            detail: MetricDetail {
                labels: "svc=a".into(),
                query: "up".into(),
                points: vec![(1, 1.0)],
            },
        };
        assert_eq!(md.screen(), Screen::Metrics);
        assert!(md.is_detail());
        let ld = Route::LogDetail {
            record: sample_log_record(None),
        };
        assert_eq!(ld.screen(), Screen::Logs);
        assert!(ld.is_detail());
        let trace = build_trace_detail(&sample_trace(), true);
        let span = trace.spans[0].clone();
        let td = Route::TraceDetail { detail: trace };
        assert_eq!(td.screen(), Screen::Traces);
        assert!(td.is_detail());
        let sd = Route::SpanDetail {
            trace_id: "aa".into(),
            span,
        };
        assert_eq!(sd.screen(), Screen::Traces);
        assert!(sd.is_detail());

        // `list` round-trips through `screen`.
        for screen in [
            Screen::Overview,
            Screen::Metrics,
            Screen::Traces,
            Screen::Logs,
        ] {
            assert_eq!(Route::list(screen).screen(), screen);
        }
    }
}

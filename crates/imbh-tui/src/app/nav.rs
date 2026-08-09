//! Navigation state: the menu bar, the keyboard focus ring, the back/forward history, and the
//! walk up a screen's own view series.

use crate::app::App;
use crate::model::{Focus, MENU_LEN, Mode, NavEntry, Route, Screen};
use crate::waterfall::TraceDetail;

/// One step up a screen's view series (see [`App::series_parent`]). Most rungs are a different
/// [`Route`], but the Metrics series' first one is not: the catalog and the series list are the same
/// route told apart by whether the query is empty, so stepping onto it is a query change.
#[derive(Debug, Clone)]
pub(crate) enum SeriesUp {
    /// An earlier route in the chain. Boxed because a `Route` carries its view's whole data (a trace
    /// detail is ~340 bytes) and the other variant carries none — one allocation per `Backspace`.
    Route(Box<Route>),
    /// The Metrics catalog — `Route::Metrics` with the query cleared.
    Catalog,
}

impl SeriesUp {
    fn route(route: Route) -> Self {
        Self::Route(Box::new(route))
    }
}

impl App {
    /// Activate the menu bar (`F9`), starting the highlight on the current screen.
    pub(crate) fn open_menu(&mut self) {
        self.menu_cursor = self.screen().index();
        self.mode = Mode::Menu;
    }

    /// Move the menu highlight by `delta`, wrapping across the screens and the trailing range item.
    pub(crate) fn menu_move(&mut self, delta: isize) {
        self.menu_cursor =
            (self.menu_cursor as isize + delta).rem_euclid(MENU_LEN as isize) as usize;
    }

    /// The screen the menu highlight is on, or `None` when it is on the trailing time-range item.
    pub(crate) fn menu_screen(&self) -> Option<Screen> {
        Screen::ORDER.get(self.menu_cursor).copied()
    }

    /// Whether the current view has a query editor pane. Overview is a report over the whole
    /// database rather than an answer to a query, so it takes none; the detail routes render
    /// full-content and have none either. Also decides whether `Focus::Query` is reachable.
    pub(crate) fn has_query(&self) -> bool {
        self.screen() != Screen::Overview && !self.route.is_detail()
    }

    /// The focus ring for the current view, in reading order (every menu-bar screen item, the time
    /// selector, then the content panes). The query stop is present only when the view has a query
    /// pane; `Tab`/`Shift+Tab` cycle this order (with wraparound).
    ///
    /// Derived from [`Screen::ORDER`] rather than written out, so adding a screen cannot leave its
    /// menu item unreachable by `Tab` — which is exactly what the hand-written list did the first
    /// time a fifth screen was tried.
    pub(crate) fn focus_ring(&self) -> Vec<Focus> {
        let mut ring: Vec<Focus> = (0..Screen::ORDER.len()).map(Focus::Menu).collect();
        ring.push(Focus::TimeRange);
        if self.has_query() {
            ring.push(Focus::Query);
        }
        ring.push(Focus::Primary);
        if self.has_attr_pane() {
            // Reading order within the pane: the range it was measured over, then one stop per
            // section, top to bottom.
            ring.push(Focus::AttrRange);
            ring.extend((0..self.attr_sections().len()).map(Focus::AttrTable));
        }
        ring
    }

    /// Whether the Overview's attribute pane is on screen — and so whether it is a focus stop.
    pub(crate) fn has_attr_pane(&self) -> bool {
        self.screen() == Screen::Overview && !self.route.is_detail()
    }

    /// The focus as it actually applies to the current view: a stored `Query` or attribute-pane focus
    /// snaps to `Primary` on a view without that pane, so the highlight and `Enter` never target a
    /// pane that is not shown.
    pub(crate) fn effective_focus(&self) -> Focus {
        match self.focus {
            Focus::Query if !self.has_query() => Focus::Primary,
            Focus::AttrRange if !self.has_attr_pane() => Focus::Primary,
            // A section index outlives the measurement that produced it: a refresh can return fewer
            // sections (a table emptied out of the range), and a stop pointing past the end must not
            // take the cursor or `p` with it.
            Focus::AttrTable(section) if section >= self.attr_sections().len() => Focus::Primary,
            focus => focus,
        }
    }

    /// Advance the focus ring by `delta` (Tab: +1 down, Shift+Tab: -1 up), wrapping. Anchored on the
    /// effective focus so it steps sensibly even when a stale `Query` focus was snapped to `Primary`.
    pub(crate) fn focus_advance(&mut self, delta: isize) {
        let ring = self.focus_ring();
        let current = ring
            .iter()
            .position(|f| *f == self.effective_focus())
            .unwrap_or(ring.len() - 1) as isize;
        let next = (current + delta).rem_euclid(ring.len() as isize) as usize;
        self.focus = ring[next];
        // Landing on a section puts the cursor inside it, so Tab scrolls the pane to what it focused
        // rather than leaving the highlight somewhere off screen.
        self.snap_attr_cursor();
    }

    /// Move the focus among the menu-bar items only — the screen items and the trailing time
    /// selector — wrapping over `MENU_LEN`. Bound to Left/Right while the ring is on the bar (there they
    /// select rather than navigate history). A no-op unless the focus is already on a menu-bar stop (its
    /// natural precondition), mirroring `menu_move`.
    pub(crate) fn menubar_move(&mut self, delta: isize) {
        let current = match self.effective_focus() {
            Focus::Menu(index) => index,
            Focus::TimeRange => MENU_LEN - 1,
            _ => return,
        };
        let next = (current as isize + delta).rem_euclid(MENU_LEN as isize) as usize;
        self.focus = if next == MENU_LEN - 1 {
            Focus::TimeRange
        } else {
            Focus::Menu(next)
        };
    }

    /// Snapshot the current view for the history.
    pub(crate) fn capture_nav(&self) -> NavEntry {
        NavEntry {
            route: self.route.clone(),
            query: self.query.clone(),
            metric_cursor: self.metric_cursor,
            focus_trace_id: self.focus_trace_id.clone(),
            selected: self.selected,
            scroll: self.scroll,
            span_cursor: self.span_cursor,
            log_correlation: self.log_correlation.clone(),
        }
    }

    /// Restore a captured view (the data pane is reloaded by the caller's refresh). Any transient
    /// overlay is dropped — history moves between views, not input modes.
    pub(crate) fn restore_nav(&mut self, entry: NavEntry) {
        self.route = entry.route;
        self.query = entry.query;
        self.metric_cursor = entry.metric_cursor;
        self.focus_trace_id = entry.focus_trace_id;
        self.selected = entry.selected;
        self.scroll = entry.scroll;
        self.span_cursor = entry.span_cursor;
        self.log_correlation = entry.log_correlation;
        // Exemplar markers are view-specific; a metric detail restored by history refetches them.
        self.metric_exemplars.clear();
        // A trace-open intent belongs to the view it was issued from; history navigation abandons it so
        // a late waterfall never yanks the user into a detail they navigated away from. The restored
        // focus is a plain waterfall focus — the jump that armed it has already been navigated away.
        self.pending_trace_open = false;
        self.focus_trace_open = false;
        self.mode = Mode::Normal;
        self.completion = None;
        self.focus = Focus::Primary;
    }

    /// Record `entry` as the view a forward navigation departs from, invalidating the Forward stack (a
    /// new branch). Used directly when the caller captures the departing view *before* it knows the
    /// navigation will succeed (so the history is only recorded on success).
    pub(crate) fn push_entry(&mut self, entry: NavEntry) {
        const CAP: usize = 64;
        self.back.push(entry);
        if self.back.len() > CAP {
            self.back.remove(0);
        }
        self.forward.clear();
    }

    /// Record the current view before a forward navigation. Called right before mutating to the
    /// destination view.
    pub(crate) fn push_history(&mut self) {
        let entry = self.capture_nav();
        self.push_entry(entry);
    }

    /// Browser Back: restore the previous view (pushing the current one onto Forward). Returns whether
    /// there was history to move through, so the caller can reload the restored view's data.
    pub(crate) fn go_back(&mut self) -> bool {
        if let Some(entry) = self.back.pop() {
            self.forward.push(self.capture_nav());
            self.restore_nav(entry);
            true
        } else {
            false
        }
    }

    /// Browser Forward: redo a Back (no effect unless a Back was taken).
    pub(crate) fn go_forward(&mut self) -> bool {
        if let Some(entry) = self.forward.pop() {
            self.back.push(self.capture_nav());
            self.restore_nav(entry);
            true
        } else {
            false
        }
    }

    /// The materialized trace for `trace_id`: the one retained behind the Traces preview pane, else a
    /// trace detail still sitting on the back stack. Purely a *data* lookup for rebuilding the parent
    /// view — where an up-navigation goes is fixed by the series, never by what was visited.
    fn series_trace(&self, trace_id: &str) -> Option<TraceDetail> {
        if let Some(detail) = self
            .trace_detail
            .as_ref()
            .filter(|detail| detail.trace_id == trace_id)
        {
            return Some(detail.clone());
        }
        self.back.iter().rev().find_map(|entry| match &entry.route {
            Route::TraceDetail { detail } if detail.trace_id == trace_id => Some(detail.clone()),
            _ => None,
        })
    }

    /// The view one step earlier in this screen's *series* — the chain a screen drills through
    /// (`catalog → Metrics → MetricDetail`, `Traces → TraceDetail → SpanDetail`, `Logs → LogDetail`) —
    /// regardless of how the current view was reached. `None` on a series' first view, which has
    /// nothing above it.
    ///
    /// A span detail whose trace can no longer be materialized (see [`Self::series_trace`]) steps to the
    /// Traces list instead: still up its own series, just skipping the rung there is no longer data for.
    pub(crate) fn series_parent(&self) -> Option<SeriesUp> {
        match &self.route {
            Route::MetricDetail { .. } => Some(SeriesUp::route(Route::Metrics)),
            Route::LogDetail { .. } => Some(SeriesUp::route(Route::Logs)),
            Route::TraceDetail { .. } => Some(SeriesUp::route(Route::Traces)),
            Route::SpanDetail { trace_id, .. } => Some(SeriesUp::route(
                self.series_trace(trace_id)
                    .map_or(Route::Traces, |detail| Route::TraceDetail { detail }),
            )),
            // The Metrics series has a rung below its list route: the catalog the series list was built
            // from. `on_catalog` (an empty query) tells the two apart, so a series list steps up to the
            // catalog and only the catalog itself is the series' first view.
            Route::Metrics if !self.on_catalog() => Some(SeriesUp::Catalog),
            Route::Overview | Route::Metrics | Route::Traces | Route::Logs => None,
        }
    }

    /// Walk one step up the screen series (`Backspace`), recording the departed view so `←` undoes the
    /// move. Unlike [`Self::go_back`] this ignores the visit history entirely: a trace detail opened by
    /// a log→trace jump steps up to the Traces list, where Back would return to the log it came from.
    /// Returns whether a move happened, so the caller reloads the destination's data.
    pub(crate) fn go_up(&mut self) -> bool {
        let Some(step) = self.series_parent() else {
            return false;
        };
        self.push_history();
        match step {
            SeriesUp::Route(route) => self.route = *route,
            // The catalog is reached by dropping the query, not by changing route; the refresh the
            // caller issues then renders the tree instead of a series table. The row cursor indexed the
            // series rows, which mean nothing in the catalog, so it restarts at the top.
            SeriesUp::Catalog => {
                self.set_active_query(String::new());
                self.selected = 0;
            }
        }
        self.scroll = 0;
        self.focus = Focus::Primary;
        self.mode = Mode::Normal;
        self.completion = None;
        // Drill-down intents and exemplar markers belong to the view being left; a late waterfall must
        // not yank the user back down into the detail they just stepped out of.
        self.pending_trace_open = false;
        self.clear_trace_focus();
        self.metric_exemplars.clear();
        // Stepping out of the trace views entirely retires the waterfall cursor; stepping from a span
        // detail up to its trace keeps it, so the cursor lands back on the span that was open.
        if !self.route.is_detail() {
            self.span_cursor = 0;
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{LogCorrelation, MetricDetail, Route};
    use crate::testutil::{metrics_app_with_series, sample_log_record, traces_app_with_trace};

    #[test]
    fn menu_cursor_wraps_over_screens_and_the_range_item() {
        let mut app = App::new();
        app.route = Route::Traces;
        app.open_menu();
        assert_eq!(app.mode, Mode::Menu);
        // Starts on the current screen.
        assert_eq!(app.menu_cursor, Screen::Traces.index());
        assert_eq!(app.menu_screen(), Some(Screen::Traces));
        // Right walks the remaining screens, reaches the trailing range item, then wraps to Overview.
        for expected in &Screen::ORDER[Screen::Traces.index() + 1..] {
            app.menu_move(1);
            assert_eq!(app.menu_screen(), Some(*expected));
        }
        app.menu_move(1);
        assert_eq!(app.menu_screen(), None); // the range item
        app.menu_move(1);
        assert_eq!(app.menu_screen(), Some(Screen::Overview));
        // Left from Overview wraps back to the range item.
        app.menu_move(-1);
        assert_eq!(app.menu_screen(), None);
    }

    #[test]
    fn focus_ring_cycles_menu_items_time_selector_and_panes_on_a_list_screen() {
        let mut app = App::new();
        app.route = Route::Metrics; // has a query pane -> the full ring incl. the four menu items
        assert_eq!(app.focus, Focus::Primary);
        // Tab (delta +1) walks reading order and wraps: Primary -> Menu(0..4) -> TimeRange -> Query.
        app.focus_advance(1);
        assert_eq!(app.focus, Focus::Menu(0));
        for expected in 1..Screen::ORDER.len() {
            app.focus_advance(1);
            assert_eq!(app.focus, Focus::Menu(expected));
        }
        app.focus_advance(1);
        assert_eq!(app.focus, Focus::TimeRange);
        app.focus_advance(1);
        assert_eq!(app.focus, Focus::Query);
        app.focus_advance(1);
        assert_eq!(app.focus, Focus::Primary);
        // Shift+Tab (delta -1) walks the other way: Primary -> Query -> TimeRange -> last menu item.
        app.focus_advance(-1);
        assert_eq!(app.focus, Focus::Query);
        app.focus_advance(-1);
        assert_eq!(app.focus, Focus::TimeRange);
        app.focus_advance(-1);
        assert_eq!(app.focus, Focus::Menu(Screen::ORDER.len() - 1));
    }

    #[test]
    fn focus_ring_omits_the_query_stop_without_a_query_pane() {
        // Overview has no query pane, and does have an attribute pane: the ring is the menu items,
        // the time selector, Primary, then the attribute pane.
        let mut app = App::new();
        assert_eq!(app.route.screen(), Screen::Overview);
        assert!(!app.has_query());
        assert!(app.has_attr_pane());
        // Step to the time selector, then one more lands on Primary (no Query stop in between).
        app.focus = Focus::TimeRange;
        app.focus_advance(1);
        assert_eq!(app.focus, Focus::Primary);
        // Then the attribute pane: the range it was measured over, then one stop per section. The
        // range is separate because Enter cannot mean both "change the range" and "act on this row";
        // the sections are separate from each other because each is its own table, and Tab is how a
        // reader reaches the third one without scrolling past the first two.
        app.focus_advance(1);
        assert_eq!(app.focus, Focus::AttrRange);
        // No measurement has landed, so there are no section stops yet — and wrapping forward from
        // the range reaches the first menu item rather than a stop with nothing behind it.
        assert!(app.attr_sections().is_empty());
        app.focus_advance(1);
        assert_eq!(app.focus, Focus::Menu(0));

        // Off the Overview both kinds of stop are gone, and a stale one reads as Primary.
        app.route = Route::Logs;
        assert!(!app.has_attr_pane());
        for stale in [Focus::AttrRange, Focus::AttrTable(0)] {
            app.focus = stale;
            assert_eq!(app.effective_focus(), Focus::Primary);
        }
        app.route = Route::Overview;
        // A section index that outlived its measurement reads as Primary too, rather than taking the
        // cursor past the end of a table that has since shrunk.
        app.focus = Focus::AttrTable(9);
        assert_eq!(app.effective_focus(), Focus::Primary);

        // A detail route also drops the query stop, and a stale Query focus reads as Primary so the
        // highlight and Enter never target a pane that is not shown.
        app.route = Route::LogDetail {
            record: sample_log_record(None),
        };
        app.focus = Focus::Query;
        assert!(!app.has_query());
        assert_eq!(app.effective_focus(), Focus::Primary);
        // Advancing anchors on the effective focus, so it steps to the first menu item, not Query.
        app.focus_advance(1);
        assert_eq!(app.focus, Focus::Menu(0));
    }

    #[test]
    fn menubar_move_cycles_the_screen_items_and_time_selector() {
        let mut app = App::new();
        app.focus = Focus::Menu(0);
        // Right walks the four screen items, then the trailing time selector, then wraps.
        for expected in 1..Screen::ORDER.len() {
            app.menubar_move(1);
            assert_eq!(app.focus, Focus::Menu(expected));
        }
        app.menubar_move(1);
        assert_eq!(app.focus, Focus::TimeRange);
        app.menubar_move(1);
        assert_eq!(app.focus, Focus::Menu(0));
        // Left from the first item wraps back to the time selector.
        app.menubar_move(-1);
        assert_eq!(app.focus, Focus::TimeRange);

        // Inert unless the ring is on a menu-bar stop: on a pane it leaves the focus untouched.
        app.focus = Focus::Primary;
        app.menubar_move(1);
        assert_eq!(app.focus, Focus::Primary);
    }

    #[test]
    fn navigation_resets_focus_to_the_primary_pane() {
        let mut app = App::new();
        app.route = Route::Metrics;
        app.focus = Focus::TimeRange;
        // restore_nav (back/forward) drops transient chrome, including a non-Primary focus.
        let entry = app.capture_nav();
        app.restore_nav(entry);
        assert_eq!(app.focus, Focus::Primary);
    }

    #[test]
    fn back_forward_history_moves_through_visited_views() {
        let mut app = App::new();
        app.route = Route::Metrics; // view A: catalog (empty query)
        assert!(!app.go_back(), "no history yet");

        // Forward A -> B (series list): record A, then mutate to B.
        app.push_history();
        app.query[1] = "up".to_owned();
        app.selected = 3;
        // Forward B -> C (series detail): record B, then mutate to C.
        app.push_history();
        app.route = Route::MetricDetail {
            detail: MetricDetail {
                labels: "svc=a".into(),
                query: "up".into(),
                points: vec![(1, 1.0)],
            },
        };

        // Back C -> B -> A.
        assert!(app.go_back());
        assert!(matches!(app.route, Route::Metrics));
        assert_eq!(app.active_query(), "up");
        assert_eq!(app.selected, 3);
        assert!(app.route_metric_detail().is_none());
        assert!(app.go_back());
        assert_eq!(app.active_query(), "");
        assert!(!app.go_back(), "at the oldest view");

        // Forward A -> B -> C redoes the Backs.
        assert!(app.go_forward());
        assert_eq!(app.active_query(), "up");
        assert!(app.go_forward());
        assert_eq!(
            app.route_metric_detail().map(|d| d.labels.as_str()),
            Some("svc=a")
        );
        assert!(!app.go_forward(), "at the newest view");

        // A fresh forward navigation invalidates the Forward stack (a new branch).
        assert!(app.go_back()); // back to B
        app.push_history();
        app.route = Route::Metrics;
        assert!(
            !app.go_forward(),
            "a new navigation clears the redo history"
        );
    }

    #[test]
    fn up_walks_the_screen_series_regardless_of_how_the_view_was_reached() {
        // The state a log→trace jump leaves: parked on a trace detail with the *log* detail behind it.
        let mut app = traces_app_with_trace();
        app.route = Route::LogDetail {
            record: sample_log_record(Some("aa")),
        };
        app.push_history();
        let detail = app.trace_detail.clone().expect("materialized trace");
        app.route = Route::TraceDetail { detail };

        // Up follows the Traces series (→ the trace list), where Back would return to the log detail.
        assert!(app.go_up());
        assert!(matches!(app.route, Route::Traces));
        // The departed view is recorded, so `←` undoes the step up.
        assert!(app.go_back());
        assert!(app.route_trace_detail().is_some());

        // Back on the log detail, Up follows the *Logs* series instead.
        assert!(app.go_back());
        assert!(app.route_log_record().is_some());
        assert!(app.go_up());
        assert!(matches!(app.route, Route::Logs));
    }

    #[test]
    fn up_from_a_span_detail_lands_on_its_trace_with_the_cursor_on_that_span() {
        let mut app = traces_app_with_trace();
        assert!(app.open_trace_detail());
        app.move_span_cursor(2);
        assert!(app.open_span_detail());

        assert!(app.go_up());
        let detail = app.route_trace_detail().expect("stepped up to the trace");
        assert_eq!(detail.spans.len(), 3);
        assert_eq!(app.span_cursor, 2, "the cursor is back on the open span");

        // One more step up leaves the trace views for the list, retiring the waterfall cursor.
        assert!(app.go_up());
        assert!(matches!(app.route, Route::Traces));
        assert_eq!(app.span_cursor, 0);
    }

    #[test]
    fn up_from_a_span_detail_falls_back_to_the_list_when_its_trace_is_gone() {
        // A span detail whose trace is neither retained nor on the back stack: the trace-detail rung
        // has no data to rebuild from, so the step lands on the Traces list instead.
        let mut app = traces_app_with_trace();
        let span = app.trace_detail.as_ref().unwrap().spans[1].clone();
        app.trace_detail = None;
        app.route = Route::SpanDetail {
            trace_id: "0123456789abcdef0123456789abcdef".to_owned(),
            span,
        };
        assert!(app.go_up());
        assert!(matches!(app.route, Route::Traces));
    }

    #[test]
    fn up_is_a_noop_on_a_series_first_view() {
        let mut app = App::new();
        // The Metrics list route is the *catalog* only while its query is empty, which `App::new` is.
        for route in [Route::Overview, Route::Metrics, Route::Traces, Route::Logs] {
            app.route = route;
            assert!(app.series_parent().is_none());
            assert!(!app.go_up(), "the first view of a series has nothing above");
            assert!(app.back.is_empty(), "a no-op records no history");
        }

        // A metric detail, by contrast, steps up to the series list.
        app.route = Route::MetricDetail {
            detail: MetricDetail {
                labels: "svc=a".into(),
                query: "up".into(),
                points: vec![(1, 1.0)],
            },
        };
        assert!(app.go_up());
        assert!(matches!(app.route, Route::Metrics));
    }

    #[test]
    fn up_walks_the_metrics_series_through_the_series_list_to_the_catalog() {
        // The Metrics series has a rung that is not a route: the catalog the series list was built
        // from, told apart by an empty query. Drill catalog -> series list -> series detail...
        let mut app = metrics_app_with_series();
        app.selected = 0;
        assert!(app.open_metric_detail());
        assert_eq!(app.metric_exemplars.len(), 0);

        // ...then walk back up it. The detail steps to the series list, query intact.
        assert!(app.go_up());
        assert!(matches!(app.route, Route::Metrics));
        assert_eq!(app.active_query(), "up");
        assert!(!app.on_catalog(), "still the series list");

        // The series list steps to the catalog: same route, query dropped.
        assert!(app.go_up());
        assert!(matches!(app.route, Route::Metrics));
        assert_eq!(app.active_query(), "");
        assert!(app.on_catalog());
        assert_eq!(
            app.selected, 0,
            "the series row cursor does not index the tree"
        );

        // The catalog is the series' first view.
        assert!(!app.go_up());

        // Each step is recorded, so `←` walks back down: catalog -> series list -> detail.
        assert!(app.go_back());
        assert_eq!(app.active_query(), "up");
        assert!(app.go_back());
        assert!(app.route_metric_detail().is_some());
    }

    #[test]
    fn history_restores_a_trace_to_log_correlation() {
        let mut app = App::new();
        app.route = Route::Traces;
        // Drill into a trace's logs: capture Traces, then set the correlation on the Logs view.
        app.push_history();
        app.route = Route::Logs;
        app.log_correlation = Some(LogCorrelation {
            trace_id: "0123456789abcdef0123456789abcdef".to_owned(),
            span_id: None,
        });
        // Back to Traces clears it; Forward to the correlated Logs restores it.
        assert!(app.go_back());
        assert_eq!(app.log_correlation, None);
        assert!(app.go_forward());
        assert_eq!(
            app.log_correlation.as_ref().map(|c| c.trace_id.as_str()),
            Some("0123456789abcdef0123456789abcdef")
        );
    }
}

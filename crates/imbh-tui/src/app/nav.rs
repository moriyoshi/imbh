//! Navigation state: the menu bar, the keyboard focus ring, and the back/forward history.

use crate::app::App;
use crate::model::{Focus, MENU_LEN, Mode, NavEntry, Screen};

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

    /// Whether the current view has a query editor pane (every list screen except Overview; the detail
    /// routes render full-content and have none). Also decides whether `Focus::Query` is reachable.
    pub(crate) fn has_query(&self) -> bool {
        self.screen() != Screen::Overview && !self.route.is_detail()
    }

    /// The focus ring for the current view, in reading order (the four menu-bar screen items, the time
    /// selector, then the content panes). The query stop is present only when the view has a query
    /// pane; `Tab`/`Shift+Tab` cycle this order (with wraparound).
    pub(crate) fn focus_ring(&self) -> &'static [Focus] {
        if self.has_query() {
            &[
                Focus::Menu(0),
                Focus::Menu(1),
                Focus::Menu(2),
                Focus::Menu(3),
                Focus::TimeRange,
                Focus::Query,
                Focus::Primary,
            ]
        } else {
            &[
                Focus::Menu(0),
                Focus::Menu(1),
                Focus::Menu(2),
                Focus::Menu(3),
                Focus::TimeRange,
                Focus::Primary,
            ]
        }
    }

    /// The focus as it actually applies to the current view: a stored `Query` focus snaps to `Primary`
    /// on a view with no query pane, so the highlight and `Enter` never target a pane that is not shown.
    pub(crate) fn effective_focus(&self) -> Focus {
        if self.focus == Focus::Query && !self.has_query() {
            Focus::Primary
        } else {
            self.focus
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
    }

    /// Move the focus among the menu-bar items only — the four screen items and the trailing time
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{LogCorrelation, MetricDetail, Route};
    use crate::testutil::sample_log_record;

    #[test]
    fn menu_cursor_wraps_over_screens_and_the_range_item() {
        let mut app = App::new();
        app.route = Route::Traces;
        app.open_menu();
        assert_eq!(app.mode, Mode::Menu);
        // Starts on the current screen.
        assert_eq!(app.menu_cursor, Screen::Traces.index());
        assert_eq!(app.menu_screen(), Some(Screen::Traces));
        // Right past Logs reaches the trailing range item, then wraps to Overview.
        app.menu_move(1);
        assert_eq!(app.menu_screen(), Some(Screen::Logs));
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
        // Overview has no query pane: the ring is the menu items, the time selector, then Primary.
        let mut app = App::new();
        assert_eq!(app.route.screen(), Screen::Overview);
        assert!(!app.has_query());
        // Step to the time selector, then one more lands on Primary (no Query stop in between).
        app.focus = Focus::TimeRange;
        app.focus_advance(1);
        assert_eq!(app.focus, Focus::Primary);
        // Wrapping forward from Primary reaches the first menu item.
        app.focus_advance(1);
        assert_eq!(app.focus, Focus::Menu(0));

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

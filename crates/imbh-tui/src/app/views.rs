//! Route accessors, row selection, and opening the detail views.

use crate::app::App;
use crate::model::{Focus, LogCorrelation, LogRecord, MetricDetail, Route, Screen};
use crate::waterfall::{SpanRecord, TraceDetail};

impl App {
    /// The log record if the current route is the log detail view.
    pub(crate) fn route_log_record(&self) -> Option<&LogRecord> {
        match &self.route {
            Route::LogDetail { record } => Some(record),
            _ => None,
        }
    }

    /// The series if the current route is the detailed time-series viewer.
    pub(crate) fn route_metric_detail(&self) -> Option<&MetricDetail> {
        match &self.route {
            Route::MetricDetail { detail } => Some(detail),
            _ => None,
        }
    }

    /// The trace if the current route is the full-screen trace detail.
    pub(crate) fn route_trace_detail(&self) -> Option<&TraceDetail> {
        match &self.route {
            Route::TraceDetail { detail } => Some(detail),
            _ => None,
        }
    }

    /// The `(trace_id, span)` pair if the current route is the span field detail.
    pub(crate) fn route_span_detail(&self) -> Option<(&str, &SpanRecord)> {
        match &self.route {
            Route::SpanDetail { trace_id, span } => Some((trace_id.as_str(), span)),
            _ => None,
        }
    }

    /// The span the trace detail's waterfall cursor is on (clamped), or `None` off that route / for an
    /// empty trace.
    pub(crate) fn selected_span(&self) -> Option<&SpanRecord> {
        let detail = self.route_trace_detail()?;
        detail
            .spans
            .get(self.span_cursor.min(detail.spans.len().saturating_sub(1)))
    }

    /// Page the Logs list one step older (toward earlier records), if the current page reported a
    /// resume cursor. Returns whether a move happened (the caller then refreshes).
    pub(crate) fn logs_page_older(&mut self) -> bool {
        if self.screen() != Screen::Logs || self.route.is_detail() {
            return false;
        }
        let Some(cursor) = self.log_next_cursor else {
            return false;
        };
        self.log_cursor_stack.push(cursor);
        self.log_paging = true;
        self.scroll = 0;
        self.selected = 0;
        true
    }

    /// Page the Logs list one step newer (toward the most recent records), if not already on page 0.
    /// Returns whether a move happened.
    pub(crate) fn logs_page_newer(&mut self) -> bool {
        if self.screen() != Screen::Logs || self.route.is_detail() {
            return false;
        }
        if self.log_cursor_stack.pop().is_none() {
            return false;
        }
        self.log_paging = true;
        self.scroll = 0;
        self.selected = 0;
        true
    }

    /// The trace id of the exemplar marker nearest the metric-detail chart cursor, if any — the target
    /// of the exemplar→trace jump (`Enter` on the metric detail).
    pub(crate) fn nearest_exemplar_trace(&self) -> Option<String> {
        let detail = self.route_metric_detail()?;
        if detail.points.is_empty() || self.metric_exemplars.is_empty() {
            return None;
        }
        let cursor = self.metric_cursor.min(detail.points.len() - 1);
        let cursor_ns = detail.points[cursor].0;
        self.metric_exemplars
            .iter()
            .min_by_key(|marker| marker.time_ns.abs_diff(cursor_ns))
            .map(|marker| marker.trace_id.clone())
    }

    /// The trace id of the currently selected row on the Traces screen, parsed from the leading
    /// whitespace-delimited token of the row (rows are `"{trace_id} selected=…"`). `None` on other
    /// screens or when nothing is selectable.
    pub(crate) fn selected_trace_id(&self) -> Option<String> {
        if self.screen() != Screen::Traces {
            return None;
        }
        let (first, last) = self.selectable_bounds()?;
        let selected = self.selected.clamp(first, last);
        self.snapshot
            .lines
            .get(selected)?
            .split_whitespace()
            .next()
            .map(str::to_owned)
    }

    /// The structured log record for the currently selected row on the Logs screen.
    pub(crate) fn selected_log_record(&self) -> Option<&LogRecord> {
        if self.screen() != Screen::Logs {
            return None;
        }
        let (first, last) = self.selectable_bounds()?;
        let index = self.selected.clamp(first, last) - first;
        self.snapshot.log_records.get(index)
    }

    /// Open the detailed time-series viewer for the currently selected series (the Metrics result
    /// table). Returns `false` (no-op) when there is no selectable series row (e.g. the catalog, or an
    /// empty result), so the caller records history only on a real navigation. The x-cursor starts at
    /// the latest point.
    pub(crate) fn open_metric_detail(&mut self) -> bool {
        if self.screen() != Screen::Metrics {
            return false;
        }
        let Some((first, last)) = self.selectable_bounds() else {
            return false;
        };
        let index = self.selected.clamp(first, last) - first;
        let Some(series) = self.snapshot.series.get(index) else {
            return false;
        };
        if series.points.is_empty() {
            return false;
        }
        self.metric_cursor = series.points.len() - 1;
        self.route = Route::MetricDetail {
            detail: MetricDetail {
                labels: series.labels.clone(),
                query: self.active_query().to_owned(),
                points: series.points.clone(),
            },
        };
        true
    }

    /// Open the full-screen trace detail for the trace the Traces list is pointing at (the log→trace
    /// focus, else the row cursor). The trace is the one already materialized for the preview pane, so
    /// this is a pure in-memory navigation; it returns `false` when the fetch has not landed yet (or is
    /// for another trace), which the caller turns into a `pending_trace_open` intent. History is
    /// recorded here, so a no-op Enter never disturbs the Forward stack.
    pub(crate) fn open_trace_detail(&mut self) -> bool {
        if !matches!(self.route, Route::Traces) {
            return false;
        }
        let Some(target) = self
            .focus_trace_id
            .clone()
            .or_else(|| self.selected_trace_id())
        else {
            return false;
        };
        let Some(detail) = self
            .trace_detail
            .clone()
            .filter(|detail| detail.trace_id == target)
        else {
            return false;
        };
        self.push_history();
        self.route = Route::TraceDetail { detail };
        self.span_cursor = 0;
        self.scroll = 0;
        self.focus = Focus::Primary;
        true
    }

    /// Move the trace detail's span cursor by `delta` rows, clamped to the trace's span count. A no-op
    /// off the trace detail route.
    pub(crate) fn move_span_cursor(&mut self, delta: isize) {
        let Some(last) = self
            .route_trace_detail()
            .map(|detail| detail.spans.len().saturating_sub(1))
        else {
            return;
        };
        let current = self.span_cursor.min(last) as isize;
        self.span_cursor = current.saturating_add(delta).clamp(0, last as isize) as usize;
    }

    /// The trace/span correlation the span→log drill-down uses: the span under the trace detail's
    /// cursor, or the open span detail's span. `None` off both routes.
    pub(crate) fn span_log_correlation(&self) -> Option<LogCorrelation> {
        match &self.route {
            Route::TraceDetail { detail } => self.selected_span().map(|span| LogCorrelation {
                trace_id: detail.trace_id.clone(),
                span_id: Some(span.span_id.clone()),
            }),
            Route::SpanDetail { trace_id, span } => Some(LogCorrelation {
                trace_id: trace_id.clone(),
                span_id: Some(span.span_id.clone()),
            }),
            _ => None,
        }
    }

    /// Open the field detail of the span under the trace detail's waterfall cursor. Returns `false`
    /// (no-op, no history) off the trace detail or for a trace with no spans.
    pub(crate) fn open_span_detail(&mut self) -> bool {
        let Some(detail) = self.route_trace_detail() else {
            return false;
        };
        let trace_id = detail.trace_id.clone();
        let Some(span) = self.selected_span().cloned() else {
            return false;
        };
        self.push_history();
        self.route = Route::SpanDetail { trace_id, span };
        self.scroll = 0;
        self.focus = Focus::Primary;
        true
    }

    /// After a fresh Traces result, move the cursor onto the focused trace (if it is in the result
    /// set) and clear the focus so the selection drives the waterfall again. When the focused trace is
    /// not listed, the focus is kept so `request_waterfall` still shows its waterfall.
    pub(crate) fn focus_select_trace(&mut self) {
        let Some(focus) = self.focus_trace_id.clone() else {
            return;
        };
        if self.screen() != Screen::Traces {
            return;
        }
        let Some((first, last)) = self.selectable_bounds() else {
            return;
        };
        for index in first..=last {
            if self.snapshot.lines[index].split_whitespace().next() == Some(focus.as_str()) {
                self.selected = index;
                self.focus_trace_id = None;
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use imbh::TraceId;

    use crate::model::{ExemplarMarker, TableData};
    use crate::testutil::{
        metrics_app_with_series, sample_log_record, sample_trace, traces_app_with_trace,
    };
    use crate::waterfall::build_trace_detail;

    #[test]
    fn enter_opens_the_retained_trace_detail_and_records_history() {
        let mut app = traces_app_with_trace();
        assert!(app.open_trace_detail());
        let detail = app.route_trace_detail().expect("trace detail route");
        assert_eq!(detail.spans.len(), 3);
        assert_eq!(app.span_cursor, 0);
        // The departing Traces list is on the back stack, so Esc/← returns to it.
        assert_eq!(app.back.len(), 1);
        assert!(matches!(app.back[0].route, Route::Traces));
    }

    #[test]
    fn opening_a_trace_detail_is_a_noop_until_its_trace_lands() {
        // Fetch still in flight: no route change and no history entry, so the Enter is free to be
        // remembered as a `pending_trace_open` intent instead.
        let mut app = traces_app_with_trace();
        app.trace_detail = None;
        assert!(!app.open_trace_detail());
        assert!(matches!(app.route, Route::Traces));
        assert!(app.back.is_empty());

        // A retained trace for a *different* selection is equally not openable.
        let mut other = build_trace_detail(&sample_trace(), true);
        other.trace_id = TraceId([0xbb; 16]).to_hex();
        app.trace_detail = Some(other);
        assert!(!app.open_trace_detail());
        assert!(matches!(app.route, Route::Traces));
    }

    #[test]
    fn span_cursor_moves_within_the_trace_and_drives_the_span_detail() {
        let mut app = traces_app_with_trace();
        assert!(app.open_trace_detail());

        app.move_span_cursor(1);
        assert_eq!(app.span_cursor, 1);
        app.move_span_cursor(10); // saturates at the last span
        assert_eq!(app.span_cursor, 2);
        app.move_span_cursor(-10); // saturates at the first
        assert_eq!(app.span_cursor, 0);

        // Enter on the cursor's span opens its field detail, carrying the trace id along.
        app.move_span_cursor(2);
        assert_eq!(
            app.selected_span().map(|span| span.name.as_str()),
            Some("orphan")
        );
        assert!(app.open_span_detail());
        let (trace_id, span) = app.route_span_detail().expect("span detail route");
        assert_eq!(trace_id, TraceId([0xaa; 16]).to_hex());
        assert_eq!(span.name, "orphan");
        // Both trace views belong to the Traces screen and render as detail content.
        assert_eq!(app.screen(), Screen::Traces);
        assert!(app.route.is_detail());
        // Back through the span detail lands on the trace detail, then the list.
        assert!(app.go_back());
        assert!(app.route_trace_detail().is_some());
        assert_eq!(app.span_cursor, 2, "the waterfall cursor survives Back");
        assert!(app.go_back());
        assert!(matches!(app.route, Route::Traces));
    }

    #[test]
    fn span_log_correlation_is_span_granular_from_both_trace_views() {
        let mut app = traces_app_with_trace();
        assert_eq!(app.span_log_correlation(), None, "not on a trace view");
        assert!(app.open_trace_detail());
        app.move_span_cursor(1);
        let expected = LogCorrelation {
            trace_id: TraceId([0xaa; 16]).to_hex(),
            span_id: Some(imbh::SpanId([2; 8]).to_hex()),
        };
        assert_eq!(app.span_log_correlation(), Some(expected.clone()));
        // The span detail correlates to the same span.
        assert!(app.open_span_detail());
        assert_eq!(app.span_log_correlation(), Some(expected));
    }

    #[test]
    fn selected_log_record_indexes_by_row() {
        let mut app = App::new();
        app.route = Route::Logs;
        app.snapshot.lines = vec!["header".into(), "row a".into(), "row b".into()];
        app.snapshot.list_from = Some(1);
        app.snapshot.log_records = vec![sample_log_record(Some("dead")), sample_log_record(None)];

        app.selected = 1;
        assert_eq!(
            app.selected_log_record().map(|r| r.trace_id.clone()),
            Some(Some("dead".to_owned()))
        );
        app.selected = 2;
        assert_eq!(
            app.selected_log_record().map(|r| r.trace_id.clone()),
            Some(None)
        );

        // Not the Logs screen -> no record.
        app.route = Route::Traces;
        assert!(app.selected_log_record().is_none());
    }

    #[test]
    fn open_metric_detail_selects_the_row_and_starts_at_latest() {
        let mut app = metrics_app_with_series();
        app.selected = 1; // the second series
        assert!(app.open_metric_detail());
        let detail = app.route_metric_detail().expect("detail route");
        assert_eq!(detail.labels, "svc=b");
        assert_eq!(detail.points.len(), 3);
        assert_eq!(detail.query, "up");
        assert_eq!(app.metric_cursor, 2, "cursor starts at the latest point");
    }

    #[test]
    fn open_metric_detail_is_a_noop_without_a_series() {
        // The catalog view (empty query, no retained series) must not open the viewer.
        let mut app = App::new();
        app.route = Route::Metrics;
        app.snapshot.table = Some(TableData {
            header: vec!["Metric".into()],
            rows: vec![vec!["http.requests".into()]],
        });
        app.selected = 0;
        assert!(!app.open_metric_detail());
        assert!(app.route_metric_detail().is_none());
    }

    #[test]
    fn focus_select_trace_lands_on_the_matching_row_and_clears_focus() {
        let mut app = App::new();
        app.route = Route::Traces;
        app.snapshot.lines = vec![
            "2 matching traces".into(),
            "aaaa selected=1".into(),
            "bbbb selected=2".into(),
        ];
        app.snapshot.list_from = Some(1);

        app.focus_trace_id = Some("bbbb".into());
        app.selected = 1;
        app.focus_select_trace();
        assert_eq!(app.selected, 2);
        assert_eq!(app.focus_trace_id, None);

        // A focus not present in the list is kept (waterfall still shows it) and selection unchanged.
        app.focus_trace_id = Some("zzzz".into());
        app.selected = 1;
        app.focus_select_trace();
        assert_eq!(app.selected, 1);
        assert_eq!(app.focus_trace_id.as_deref(), Some("zzzz"));
    }

    #[test]
    fn selected_trace_id_reads_the_highlighted_row() {
        let mut app = App::new();
        app.route = Route::Traces;
        app.snapshot.lines = vec![
            "2 matching traces".into(),
            "aabbccdd selected=1,2".into(),
            "eeff0011 selected=3".into(),
        ];
        app.snapshot.list_from = Some(1);

        app.selected = 1;
        assert_eq!(app.selected_trace_id().as_deref(), Some("aabbccdd"));
        app.selected = 2;
        assert_eq!(app.selected_trace_id().as_deref(), Some("eeff0011"));

        // Only the Traces screen resolves a trace id from the selected row.
        app.route = Route::Logs;
        assert_eq!(app.selected_trace_id(), None);
    }

    #[test]
    fn log_paging_guards_reject_when_not_applicable() {
        let mut app = App::new();
        app.route = Route::Logs;
        // Page 0 with no next cursor: older is a no-op, newer is a no-op.
        assert!(!app.logs_page_older(), "no next cursor -> no older page");
        assert!(!app.logs_page_newer(), "page 0 -> no newer page");
        // Off the Logs list entirely.
        app.route = Route::Traces;
        assert!(!app.logs_page_older());
        assert!(!app.logs_page_newer());
    }

    #[test]
    fn nearest_exemplar_trace_tracks_the_chart_cursor() {
        let mut app = App::new();
        app.route = Route::MetricDetail {
            detail: MetricDetail {
                labels: "__name__=m".to_owned(),
                query: "m".to_owned(),
                points: vec![(0, 1.0), (10, 2.0), (20, 3.0)],
            },
        };
        app.metric_exemplars = vec![
            ExemplarMarker {
                time_ns: 1,
                trace_id: "aaaa".to_owned(),
            },
            ExemplarMarker {
                time_ns: 19,
                trace_id: "bbbb".to_owned(),
            },
        ];
        app.metric_cursor = 0; // t=0 -> nearest is the t=1 exemplar
        assert_eq!(app.nearest_exemplar_trace().as_deref(), Some("aaaa"));
        app.metric_cursor = 2; // t=20 -> nearest is the t=19 exemplar
        assert_eq!(app.nearest_exemplar_trace().as_deref(), Some("bbbb"));
        // No markers -> nothing to jump to.
        app.metric_exemplars.clear();
        assert_eq!(app.nearest_exemplar_trace(), None);
    }
}

//! Key handling: the detail-route keys, the global key map, and the small navigation helpers they
//! share.

use std::sync::Arc;
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use imbh::Db;
use tokio::sync::mpsc;

use crate::app::App;
use crate::model::{
    Focus, LogCorrelation, Mode, Options, Route, Screen, Snapshot, TIME_RANGES, Update,
};
use crate::tasks::{
    maybe_discover_label_dims, request_metric_dims, request_metric_exemplars, request_refresh,
    request_vocabulary, request_waterfall,
};

/// Whether the event loop should keep running after a key press.
#[derive(PartialEq, Eq)]
pub(crate) enum Control {
    Continue,
    Quit,
}

/// Keys interpreted by the detail *routes* while in `Normal` mode. Returns `Some(Continue)` when the
/// key belongs to the detail (scrolling the log body, moving the chart cursor, or the trace jump) and
/// `None` to let the global handler take it (history nav, screen switch, menu, range) — this is what
/// makes the detail views non-modal. Not a detail route → always `None`.
pub(crate) fn handle_detail_key(
    app: &mut App,
    key: KeyEvent,
    db: &Arc<Db>,
    options: &Options,
    sender: &mpsc::UnboundedSender<Update>,
) -> Option<Control> {
    // Trace detail: the waterfall is a span-selectable list (the cursor scrolls the pane), Enter opens
    // the selected span's fields, and `L` correlates logs to that span.
    if let Some(count) = app.route_trace_detail().map(|detail| detail.spans.len()) {
        let last = count.saturating_sub(1);
        let page = app.page_rows.get().max(1) as isize;
        match key.code {
            KeyCode::Down => app.move_span_cursor(1),
            KeyCode::Up => app.move_span_cursor(-1),
            KeyCode::PageDown => app.move_span_cursor(page),
            KeyCode::PageUp => app.move_span_cursor(-page),
            KeyCode::Home => app.span_cursor = 0,
            KeyCode::End => app.span_cursor = last,
            KeyCode::Enter => {
                app.open_span_detail();
            }
            KeyCode::Char('L') => span_logs_drilldown(app, db, options, sender),
            // Pin/unpin the selected span's scrolled-off ancestors at the top of the waterfall. Bound
            // here rather than globally: it only means anything on this route.
            KeyCode::Char('s') => app.sticky_waterfall = !app.sticky_waterfall,
            _ => return None,
        }
        return Some(Control::Continue);
    }
    // Span detail: a scrolled field dump, plus the span-granular log drill-down.
    if app.route_span_detail().is_some() {
        match key.code {
            KeyCode::Down => app.scroll = (app.scroll + 1).min(app.max_scroll.get()),
            KeyCode::Up => app.scroll = app.scroll.saturating_sub(1),
            KeyCode::PageDown => {
                app.scroll = app
                    .scroll
                    .saturating_add(app.page_rows.get())
                    .min(app.max_scroll.get());
            }
            KeyCode::PageUp => app.scroll = app.scroll.saturating_sub(app.page_rows.get()),
            KeyCode::Home => app.scroll = 0,
            KeyCode::End => app.scroll = app.max_scroll.get(),
            KeyCode::Char('L') | KeyCode::Enter => span_logs_drilldown(app, db, options, sender),
            _ => return None,
        }
        return Some(Control::Continue);
    }
    if app.route_log_record().is_some() {
        match key.code {
            // Enter is the explicit forward navigation to the trace viewer (when the log has a trace):
            // the Traces screen is loaded focused on that trace and its detail opens on arrival.
            KeyCode::Enter => {
                if let Some(trace_id) = app.route_log_record().and_then(|r| r.trace_id.clone()) {
                    app.push_history();
                    app.focus_trace_id = Some(trace_id);
                    app.focus_trace_open = true;
                    switch_screen(
                        app,
                        Screen::Traces,
                        db.clone(),
                        options.clone(),
                        sender.clone(),
                    );
                }
            }
            // Scroll the (possibly long) detail body.
            KeyCode::Down => app.scroll = (app.scroll + 1).min(app.max_scroll.get()),
            KeyCode::Up => app.scroll = app.scroll.saturating_sub(1),
            KeyCode::PageDown => {
                app.scroll = app
                    .scroll
                    .saturating_add(app.page_rows.get())
                    .min(app.max_scroll.get());
            }
            KeyCode::PageUp => app.scroll = app.scroll.saturating_sub(app.page_rows.get()),
            KeyCode::Home => app.scroll = 0,
            KeyCode::End => app.scroll = app.max_scroll.get(),
            _ => return None,
        }
        return Some(Control::Continue);
    }
    if let Some(last) = app
        .route_metric_detail()
        .map(|detail| detail.points.len().saturating_sub(1))
    {
        let page = app.page_rows.get().max(1) as usize;
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        match key.code {
            // The chart x-cursor moves with h/l and Shift+←/Shift+→ (the bare arrows are history nav, so
            // Shift is the modal-free way to drive the cursor with the arrow keys); Home/End/PageUp/
            // PageDown jump. Up/Down are swallowed so they do not stir the underlying list selection.
            KeyCode::Char('h') => app.metric_cursor = app.metric_cursor.saturating_sub(1),
            KeyCode::Char('l') => app.metric_cursor = (app.metric_cursor + 1).min(last),
            KeyCode::Left if shift => app.metric_cursor = app.metric_cursor.saturating_sub(1),
            KeyCode::Right if shift => app.metric_cursor = (app.metric_cursor + 1).min(last),
            KeyCode::PageUp => app.metric_cursor = app.metric_cursor.saturating_sub(page),
            KeyCode::PageDown => app.metric_cursor = (app.metric_cursor + page).min(last),
            KeyCode::Home => app.metric_cursor = 0,
            KeyCode::End => app.metric_cursor = last,
            KeyCode::Up | KeyCode::Down => {}
            // Exemplar → trace drill-down: jump to the trace of the exemplar nearest the chart cursor.
            // With no exemplar in view, fall through (Enter is otherwise inert on a metric detail).
            KeyCode::Enter => {
                let trace_id = app.nearest_exemplar_trace()?;
                app.push_history();
                app.focus_trace_id = Some(trace_id);
                app.focus_trace_open = true;
                switch_screen(
                    app,
                    Screen::Traces,
                    db.clone(),
                    options.clone(),
                    sender.clone(),
                );
            }
            _ => return None,
        }
        return Some(Control::Continue);
    }
    None
}

/// Span → logs drill-down: open the Logs screen correlated to the current span (trace id *and* span
/// id), the span-granular partner of the trace-level `L` on the Traces list. Works from either the
/// trace detail (the span under the waterfall cursor) or the span detail; a no-op elsewhere.
pub(crate) fn span_logs_drilldown(
    app: &mut App,
    db: &Arc<Db>,
    options: &Options,
    sender: &mpsc::UnboundedSender<Update>,
) {
    let Some(correlation) = app.span_log_correlation() else {
        return;
    };
    app.push_history();
    app.log_correlation = Some(correlation);
    switch_screen(
        app,
        Screen::Logs,
        db.clone(),
        options.clone(),
        sender.clone(),
    );
}

/// Apply a single key press to the app, dispatching refreshes/screen switches as needed.
pub(crate) fn handle_key(
    app: &mut App,
    key: KeyEvent,
    db: &Arc<Db>,
    options: &Options,
    sender: &mpsc::UnboundedSender<Update>,
) -> Control {
    if key.kind != KeyEventKind::Press {
        return Control::Continue;
    }
    match app.mode {
        Mode::Editing => {
            match key.code {
                KeyCode::Esc => {
                    app.mode = Mode::Normal;
                    app.completion = None;
                }
                KeyCode::Enter => {
                    app.mode = Mode::Normal;
                    app.completion = None;
                    app.scroll = 0;
                    app.selected = 0;
                    // Explicitly running a Logs query supersedes any trace→log drill-down (the user is
                    // now driving the filter box), so the correlation is cleared.
                    if app.screen() == Screen::Logs {
                        app.log_correlation = None;
                    }
                    // Running the query moves the user's attention to the results, so focus lands there.
                    app.focus = Focus::Primary;
                    request_refresh(app, db.clone(), options.clone(), sender.clone());
                }
                // Tab accepts the highlighted completion; ↑/↓ move within the popup.
                KeyCode::Tab => app.accept_completion(),
                KeyCode::Down => {
                    if let Some(completion) = app.completion.as_mut() {
                        completion.selected =
                            (completion.selected + 1).min(completion.candidates.len() - 1);
                    }
                }
                KeyCode::Up => {
                    if let Some(completion) = app.completion.as_mut() {
                        completion.selected = completion.selected.saturating_sub(1);
                    }
                }
                KeyCode::Backspace => {
                    app.active_query_mut().pop();
                    app.refresh_completion();
                    maybe_discover_label_dims(app, db, options, sender);
                }
                KeyCode::Char(character) => {
                    app.active_query_mut().push(character);
                    app.refresh_completion();
                    maybe_discover_label_dims(app, db, options, sender);
                }
                _ => {}
            }
            return Control::Continue;
        }
        Mode::TimeRange => {
            match key.code {
                KeyCode::Esc => app.mode = Mode::Normal,
                KeyCode::Up | KeyCode::Char('k') => {
                    app.range_cursor = app.range_cursor.saturating_sub(1);
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    // The extra row past the presets is the "Absolute…" entry.
                    app.range_cursor = (app.range_cursor + 1).min(TIME_RANGES.len());
                }
                KeyCode::Enter => {
                    if app.range_cursor == TIME_RANGES.len() {
                        // "Absolute…": switch into the two-field datetime form.
                        app.open_absolute_form();
                    } else {
                        app.mode = Mode::Normal;
                        // Picking a preset returns to a rolling window; refresh if the effective window
                        // changed (a different preset, or leaving an absolute window).
                        let changed =
                            app.range_cursor != app.range_index || app.abs_window.is_some();
                        app.range_index = app.range_cursor;
                        app.abs_window = None;
                        if changed {
                            app.scroll = 0;
                            app.selected = 0;
                            request_refresh(app, db.clone(), options.clone(), sender.clone());
                        }
                    }
                }
                _ => {}
            }
            return Control::Continue;
        }
        Mode::AbsoluteRange => {
            match key.code {
                KeyCode::Esc => app.mode = Mode::Normal,
                KeyCode::Tab => app.abs_field ^= 1,
                KeyCode::Up => app.abs_field = 0,
                KeyCode::Down => app.abs_field = 1,
                KeyCode::Backspace => {
                    if app.abs_field == 0 {
                        app.abs_start.pop();
                    } else {
                        app.abs_end.pop();
                    }
                }
                KeyCode::Char(character) => {
                    if app.abs_field == 0 {
                        app.abs_start.push(character);
                    } else {
                        app.abs_end.push(character);
                    }
                }
                // `commit_absolute` always runs (recording a parse error on failure); the guard only
                // gates the follow-up refresh on a successful commit.
                KeyCode::Enter if app.commit_absolute() => {
                    request_refresh(app, db.clone(), options.clone(), sender.clone());
                }
                _ => {}
            }
            return Control::Continue;
        }
        Mode::Menu => {
            match key.code {
                KeyCode::Esc | KeyCode::F(9) => app.mode = Mode::Normal,
                KeyCode::Left | KeyCode::BackTab | KeyCode::Char('h') => app.menu_move(-1),
                KeyCode::Right | KeyCode::Tab | KeyCode::Char('l') => app.menu_move(1),
                KeyCode::Enter => match app.menu_screen() {
                    // A screen item: switch to it and dismiss the menu.
                    Some(screen) => {
                        app.mode = Mode::Normal;
                        switch_screen_history(
                            app,
                            screen,
                            db.clone(),
                            options.clone(),
                            sender.clone(),
                        );
                    }
                    // The trailing range item: open the time-range dropdown (also parks the focus ring
                    // on the time selector, so closing the picker leaves it focused there).
                    None => {
                        open_time_range(app);
                    }
                },
                _ => {}
            }
            return Control::Continue;
        }
        Mode::Normal => {}
    }
    // Focus ring (Normal mode): Tab/Shift+Tab move the pane highlight; Enter activates the focused
    // pane. Only the TimeRange/Query stops act here — a Primary focus falls through untouched so the
    // detail routes and the per-route Enter arms behave exactly as before.
    match key.code {
        KeyCode::Tab => {
            app.focus_advance(1);
            return Control::Continue;
        }
        KeyCode::BackTab => {
            app.focus_advance(-1);
            return Control::Continue;
        }
        KeyCode::Enter => match app.effective_focus() {
            Focus::Menu(index) => {
                if let Some(&screen) = Screen::ORDER.get(index) {
                    switch_screen_history(app, screen, db.clone(), options.clone(), sender.clone());
                }
                return Control::Continue;
            }
            Focus::TimeRange => {
                open_time_range(app);
                return Control::Continue;
            }
            Focus::Query => {
                begin_editing(app, db, sender);
                return Control::Continue;
            }
            Focus::Primary => {}
        },
        // Left/Right select among the menu-bar items when the ring is on the bar, returning early so
        // they never fall through to history; on a content pane they instead drive Back/Forward below.
        KeyCode::Left if matches!(app.effective_focus(), Focus::Menu(_) | Focus::TimeRange) => {
            app.menubar_move(-1);
            return Control::Continue;
        }
        KeyCode::Right if matches!(app.effective_focus(), Focus::Menu(_) | Focus::TimeRange) => {
            app.menubar_move(1);
            return Control::Continue;
        }
        _ => {}
    }
    // Detail routes interpret a few keys of their own (scroll, chart cursor, trace jump); everything
    // else — history nav, screen switches, the menu, the range picker — falls through to the global
    // handling below, so the detail views are ordinary content, not modal.
    if let Some(control) = handle_detail_key(app, key, db, options, sender) {
        return control;
    }
    match key.code {
        KeyCode::Char('q') => return Control::Quit,
        // Left/Right are browser Back/Forward through the visited views; Esc is an alias for Back — but
        // only while focus is on a content pane. When the focus ring is on a menu-bar item, Left/Right
        // select among the items instead (handled above, returning early) and never reach history.
        // Forward navigation to a *new* view is always an explicit action (Enter, `t`, a screen key),
        // never `→` — so `→` only redoes a Back and never jumps somewhere unvisited.
        KeyCode::Left | KeyCode::Esc => {
            if app.go_back() {
                request_refresh(app, db.clone(), options.clone(), sender.clone());
                // Landing back on a metric detail refetches its exemplar markers (no-op otherwise).
                request_metric_exemplars(app, db, sender);
            }
        }
        KeyCode::Right => {
            if app.go_forward() {
                request_refresh(app, db.clone(), options.clone(), sender.clone());
                request_metric_exemplars(app, db, sender);
            }
        }
        // Logs list → log detail (Enter): open the detail for the selected entry.
        KeyCode::Enter if matches!(app.route, Route::Logs) => {
            if let Some(record) = app.selected_log_record().cloned() {
                app.push_history();
                app.route = Route::LogDetail { record };
                app.scroll = 0;
            }
        }
        // Traces list → trace detail (Enter): open the full-screen waterfall for the selected trace.
        // The trace is normally already materialized for the preview pane, so this is instant; when the
        // fetch is still in flight the intent is remembered and the view opens as soon as it lands.
        KeyCode::Enter if matches!(app.route, Route::Traces) => {
            if !app.open_trace_detail() {
                app.pending_trace_open = app.detail_trace_id.is_some();
            }
        }
        // Space expands/collapses the selected metric or dimension in the catalog tree, lazily
        // fetching a metric's dimensions on first expand.
        KeyCode::Char(' ') if app.on_catalog() => {
            if let Some((name, kind)) = app.toggle_node() {
                // Discovery spans all metric data (picker-independent); only the series cap matters.
                request_metric_dims(name, kind, db.clone(), options.max_series, sender.clone());
            }
        }
        // Catalog → series list (Enter): build the matching PromQL and visualize it — every metric
        // with a checked series (else the node under the cursor: whole metric / group-by / filter).
        // Multiple queries are joined by newlines and run together (the executor has no `or`). The
        // catalog selection is preserved across a Back (`build_metric_tree` carries state by name).
        KeyCode::Enter if app.on_catalog() => {
            let queries = app.visualize_queries();
            if !queries.is_empty() {
                app.push_history();
                *app.active_query_mut() = queries.join("\n");
                app.selected = 0;
                app.scroll = 0;
                request_refresh(app, db.clone(), options.clone(), sender.clone());
            }
        }
        // Series list → series detail (Enter): open the detailed time-series viewer for the selection.
        // Capture the list view first and only record it in history if a detail actually opens, so a
        // no-op Enter never disturbs the Forward stack.
        KeyCode::Enter if matches!(app.route, Route::Metrics) => {
            let departing = app.capture_nav();
            if app.open_metric_detail() {
                app.push_entry(departing);
                // Load the series' exemplar→trace markers for the just-opened detail.
                request_metric_exemplars(app, db, sender);
            }
        }
        // Time pan/zoom: `[`/`]` pan the window earlier/later by half its span; `-`/`+` (or `=`) zoom
        // out/in about the center. Each freezes the window to an absolute span (shown in the header)
        // and re-queries. No-ops (no window change) skip the refresh.
        KeyCode::Char('[') => {
            if app.pan_window(-0.5) {
                request_refresh(app, db.clone(), options.clone(), sender.clone());
            }
        }
        KeyCode::Char(']') => {
            if app.pan_window(0.5) {
                request_refresh(app, db.clone(), options.clone(), sender.clone());
            }
        }
        KeyCode::Char('-') => {
            if app.zoom_window(2.0) {
                request_refresh(app, db.clone(), options.clone(), sender.clone());
            }
        }
        KeyCode::Char('+') | KeyCode::Char('=') => {
            if app.zoom_window(0.5) {
                request_refresh(app, db.clone(), options.clone(), sender.clone());
            }
        }
        // Older/newer log paging (Logs list): `n` = older page, `p` = newer page. No-ops off the Logs
        // list or at the ends (`logs_page_*` guards the screen and the cursor stack).
        KeyCode::Char('n') => {
            if app.logs_page_older() {
                request_refresh(app, db.clone(), options.clone(), sender.clone());
            }
        }
        KeyCode::Char('p') => {
            if app.logs_page_newer() {
                request_refresh(app, db.clone(), options.clone(), sender.clone());
            }
        }
        // Trace → logs drill-down: open the Logs screen filtered to the selected trace's records (the
        // symmetric partner of the log-detail Enter→trace jump). `L` (Shift+l) on the Traces list.
        KeyCode::Char('L') if app.screen() == Screen::Traces && !app.route.is_detail() => {
            if let Some(trace_id) = app.selected_trace_id() {
                app.push_history();
                app.log_correlation = Some(LogCorrelation {
                    trace_id,
                    span_id: None,
                });
                switch_screen(
                    app,
                    Screen::Logs,
                    db.clone(),
                    options.clone(),
                    sender.clone(),
                );
            }
        }
        KeyCode::Char('t') => open_time_range(app),
        KeyCode::Char('1') => switch_screen_history(
            app,
            Screen::Overview,
            db.clone(),
            options.clone(),
            sender.clone(),
        ),
        KeyCode::Char('2') => switch_screen_history(
            app,
            Screen::Metrics,
            db.clone(),
            options.clone(),
            sender.clone(),
        ),
        KeyCode::Char('3') => switch_screen_history(
            app,
            Screen::Traces,
            db.clone(),
            options.clone(),
            sender.clone(),
        ),
        KeyCode::Char('4') => switch_screen_history(
            app,
            Screen::Logs,
            db.clone(),
            options.clone(),
            sender.clone(),
        ),
        KeyCode::Char('e') if app.has_query() => begin_editing(app, db, sender),
        KeyCode::Char('r') => request_refresh(app, db.clone(), options.clone(), sender.clone()),
        // Shift+R (the uppercase char crossterm delivers) toggles background auto-refresh.
        KeyCode::Char('R') => app.auto_refresh = !app.auto_refresh,
        // Toggle the animated mascot (hidden by default). No effect on `--ascii` terminals, where its
        // block-glyph art is never drawn. Reset the motion clock so it does not lurch on first show.
        KeyCode::Char('m') => {
            app.show_mascot = !app.show_mascot;
            if app.show_mascot {
                app.mascot.last_tick = Instant::now();
            }
        }
        // F9 activates the menu bar (Midnight-Commander style); the numbered keys still jump to a
        // screen directly. The cursor/Tab only move between screens once the menu is active.
        KeyCode::F(9) => app.open_menu(),
        // ↑↓ / PageUp / PageDown / Home / End move the row cursor (traces/logs) or scroll the pane.
        KeyCode::Down => move_selection(app, 1),
        KeyCode::Up => move_selection(app, -1),
        KeyCode::PageDown => move_selection(app, app.page_rows.get() as isize),
        KeyCode::PageUp => move_selection(app, -(app.page_rows.get() as isize)),
        KeyCode::Home => {
            app.clear_trace_focus();
            if let Some((first, _)) = app.selectable_bounds() {
                app.selected = first;
            } else {
                app.scroll = 0;
            }
        }
        KeyCode::End => {
            app.clear_trace_focus();
            if let Some((_, last)) = app.selectable_bounds() {
                app.selected = last;
            } else {
                app.scroll = app.max_scroll.get();
            }
        }
        _ => {}
    }
    // If the row cursor moved to a different trace, refresh the waterfall pane (no-op otherwise).
    request_waterfall(app, db, sender, options.ascii);
    Control::Continue
}

/// Move the row cursor by `delta` rows when the primary pane is a navigable list (traces/logs),
/// otherwise scroll the plain pane by the same amount. Moving the cursor releases any log→trace
/// focus so the waterfall follows the selection again.
pub(crate) fn move_selection(app: &mut App, delta: isize) {
    app.clear_trace_focus();
    if let Some((first, last)) = app.selectable_bounds() {
        let current = app.selected.clamp(first, last) as isize;
        app.selected = (current + delta).clamp(first as isize, last as isize) as usize;
    } else if delta >= 0 {
        app.scroll = app
            .scroll
            .saturating_add(delta as u16)
            .min(app.max_scroll.get());
    } else {
        app.scroll = app.scroll.saturating_sub(delta.unsigned_abs() as u16);
    }
}

/// Open the time-range dropdown, seeding the cursor from the current window and moving focus to the
/// menu-bar time selector. Shared by the `t` key and a focus-`Enter` on the time selector.
pub(crate) fn open_time_range(app: &mut App) {
    app.range_cursor = if app.abs_window.is_some() {
        TIME_RANGES.len()
    } else {
        app.range_index
    };
    app.focus = Focus::TimeRange;
    app.mode = Mode::TimeRange;
}

/// Enter query-editing mode, fetching the completion vocabulary on first use and moving focus to the
/// query pane. Shared by the `e` key and a focus-`Enter` on the query pane; the caller guarantees the
/// current view has a query pane.
pub(crate) fn begin_editing(app: &mut App, db: &Arc<Db>, sender: &mpsc::UnboundedSender<Update>) {
    app.mode = Mode::Editing;
    app.focus = Focus::Query;
    if app.screen() == Screen::Metrics && app.metric_names.is_empty() {
        request_vocabulary(app.screen(), db.clone(), sender.clone());
    }
    app.refresh_completion();
}

/// Switch screens as a browser navigation: record the departed view in history first (unless it is
/// the same screen, which is not a navigation). Used by the numbered keys and the menu.
pub(crate) fn switch_screen_history(
    app: &mut App,
    screen: Screen,
    db: Arc<Db>,
    options: Options,
    sender: mpsc::UnboundedSender<Update>,
) {
    if app.screen() != screen {
        app.push_history();
    }
    switch_screen(app, screen, db, options, sender);
}

pub(crate) fn switch_screen(
    app: &mut App,
    screen: Screen,
    db: Arc<Db>,
    options: Options,
    sender: mpsc::UnboundedSender<Update>,
) {
    app.route = Route::list(screen);
    app.scroll = 0;
    app.selected = 0;
    app.span_cursor = 0;
    app.completion = None;
    app.focus = Focus::Primary;
    // A trace-open intent is scoped to the Traces list it was issued from.
    app.pending_trace_open = false;
    // A log→trace jump sets `focus_trace_id` (and the intent to open its detail) just before switching
    // to Traces; drop a stale focus when switching anywhere else.
    if screen != Screen::Traces {
        app.clear_trace_focus();
    }
    // A trace→log drill-down sets `log_correlation` just before switching to Logs; drop it when leaving
    // Logs so an unrelated Logs visit is not silently correlated. Exemplar markers belong to a metric
    // detail, so any list switch clears them.
    if screen != Screen::Logs {
        app.log_correlation = None;
    }
    app.metric_exemplars.clear();
    if screen == Screen::Metrics && app.metric_names.is_empty() {
        request_vocabulary(screen, db.clone(), sender.clone());
    }
    app.snapshot = Snapshot::message(screen.title(), "Loading...");
    request_refresh(app, db, options, sender);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::TableData;

    #[test]
    fn cursor_moves_within_bounds_and_falls_back_to_scroll() {
        let mut app = App::new();
        app.snapshot.lines = vec!["header".into(), "a".into(), "b".into(), "c".into()];
        app.snapshot.list_from = Some(1);
        app.selected = 1; // as `apply` would have placed it (first selectable row)

        move_selection(&mut app, 1);
        assert_eq!(app.selected, 2);
        move_selection(&mut app, 10); // saturates at the last row
        assert_eq!(app.selected, 3);
        move_selection(&mut app, -1);
        assert_eq!(app.selected, 2);
        move_selection(&mut app, -10); // saturates at the first row
        assert_eq!(app.selected, 1);

        // Without a list, movement scrolls instead of selecting.
        app.snapshot.list_from = None;
        app.max_scroll.set(5);
        app.scroll = 0;
        move_selection(&mut app, 3);
        assert_eq!(app.scroll, 3);
        move_selection(&mut app, 9); // clamped to max_scroll
        assert_eq!(app.scroll, 5);
    }

    #[test]
    fn table_selection_bounds_index_the_rows() {
        let mut app = App::new();
        app.route = Route::Metrics;
        app.snapshot.table = Some(TableData {
            header: vec!["Metric".into(), "Kind".into()],
            rows: vec![
                vec!["a".into(), "gauge".into()],
                vec!["b".into(), "sum".into()],
                vec!["c".into(), "sum".into()],
            ],
        });
        // Table rows are indexed from 0 (not offset by a header line as lists are).
        assert_eq!(app.selectable_bounds(), Some((0, 2)));

        app.selected = 0;
        move_selection(&mut app, 2);
        assert_eq!(app.selected, 2);
        move_selection(&mut app, 5); // saturates at the last row
        assert_eq!(app.selected, 2);

        // An empty table has no selectable rows.
        app.snapshot.table = Some(TableData {
            header: vec!["Metric".into()],
            rows: vec![],
        });
        assert_eq!(app.selectable_bounds(), None);
    }
}

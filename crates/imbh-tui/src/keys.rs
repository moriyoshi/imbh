//! Key handling: the detail-route keys, the global key map, and the small navigation helpers they
//! share.

use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use tokio::sync::mpsc;

use crate::app::App;
use crate::backend::Backend;
use crate::model::{
    Focus, LogCorrelation, Mode, Options, Route, Screen, Snapshot, TIME_RANGES, Update,
};
use crate::tasks::{
    maybe_discover_label_dims, request_metric_dims, request_metric_exemplars, request_promotion,
    request_refresh, request_vocabulary, request_waterfall,
};
use crate::textfield::handle_edit_key;

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
    backend: &Backend,
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
            KeyCode::Char('L') => span_logs_drilldown(app, backend, options, sender),
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
            KeyCode::Char('L') | KeyCode::Enter => {
                span_logs_drilldown(app, backend, options, sender)
            }
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
                        backend.clone(),
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
                    backend.clone(),
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
    backend: &Backend,
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
        backend.clone(),
        options.clone(),
        sender.clone(),
    );
}

/// Apply a single key press to the app, dispatching refreshes/screen switches as needed.
pub(crate) fn handle_key(
    app: &mut App,
    key: KeyEvent,
    backend: &Backend,
    options: &Options,
    sender: &mpsc::UnboundedSender<Update>,
) -> Control {
    if key.kind != KeyEventKind::Press {
        return Control::Continue;
    }
    match app.mode {
        Mode::Editing => {
            // The editor's own bindings first; everything else is offered to the text field below, so
            // the caret keys (and the modifier guard that keeps `Ctrl-B` from typing a `b`) are the
            // shared ones.
            match key.code {
                KeyCode::Esc => {
                    app.mode = Mode::Normal;
                    app.completion = None;
                    return Control::Continue;
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
                    request_refresh(app, backend.clone(), options.clone(), sender.clone());
                    return Control::Continue;
                }
                // Tab accepts the highlighted completion (which refreshes the popup itself); ↑/↓ move
                // within the popup and must leave the candidates as they are.
                KeyCode::Tab => {
                    app.accept_completion();
                    maybe_discover_label_dims(app, backend, options, sender);
                    return Control::Continue;
                }
                KeyCode::Down => {
                    if let Some(completion) = app.completion.as_mut() {
                        completion.selected =
                            (completion.selected + 1).min(completion.candidates.len() - 1);
                    }
                    return Control::Continue;
                }
                KeyCode::Up => {
                    if let Some(completion) = app.completion.as_mut() {
                        completion.selected = completion.selected.saturating_sub(1);
                    }
                    return Control::Continue;
                }
                _ => {}
            }
            // A caret move or an edit both change which token the caret sits on, so the popup (and the
            // vocabulary behind it) follows. A key the field declines changes nothing.
            if handle_edit_key(app.query_field(), key) {
                app.refresh_completion();
                maybe_discover_label_dims(app, backend, options, sender);
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
                            request_refresh(app, backend.clone(), options.clone(), sender.clone());
                        }
                    }
                }
                _ => {}
            }
            return Control::Continue;
        }
        Mode::AbsoluteRange => {
            // The form's own bindings (dismiss, field switch, commit) first; the two datetime fields are
            // then edited by the same keys as the query box, caret and all. Tab/↑/↓ re-seat the shared
            // caret on the field they focus.
            match key.code {
                KeyCode::Esc => {
                    app.mode = Mode::Normal;
                    return Control::Continue;
                }
                KeyCode::Tab => {
                    app.focus_abs_field(app.abs_field ^ 1);
                    return Control::Continue;
                }
                KeyCode::Up => {
                    app.focus_abs_field(0);
                    return Control::Continue;
                }
                KeyCode::Down => {
                    app.focus_abs_field(1);
                    return Control::Continue;
                }
                // `commit_absolute` always runs (recording a parse error on failure); the guard only
                // gates the follow-up refresh on a successful commit.
                KeyCode::Enter => {
                    if app.commit_absolute() {
                        request_refresh(app, backend.clone(), options.clone(), sender.clone());
                    }
                    return Control::Continue;
                }
                _ => {}
            }
            handle_edit_key(app.abs_text_field(), key);
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
                            backend.clone(),
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
                    switch_screen_history(
                        app,
                        screen,
                        backend.clone(),
                        options.clone(),
                        sender.clone(),
                    );
                }
                return Control::Continue;
            }
            Focus::TimeRange => {
                open_time_range(app);
                return Control::Continue;
            }
            Focus::Query => {
                begin_editing(app, backend, sender);
                return Control::Continue;
            }
            // The range line's own action. Reached by focusing the line that displays the range,
            // rather than by a global key, because it belongs to this pane and to nothing else — the
            // *query* range already owns the global binding (`t`).
            Focus::AttrRange => {
                app.open_attr_range_form();
                return Control::Continue;
            }
            // The rows have no Enter action: `p` is what acts on one, and Enter here would have to
            // guess which of the pane's two things the user meant.
            Focus::AttrTable(_) => {}
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
    if let Some(control) = handle_detail_key(app, key, backend, options, sender) {
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
                request_refresh(app, backend.clone(), options.clone(), sender.clone());
                // Landing back on a metric detail refetches its exemplar markers (no-op otherwise).
                request_metric_exemplars(app, backend, sender);
            }
        }
        KeyCode::Right => {
            if app.go_forward() {
                request_refresh(app, backend.clone(), options.clone(), sender.clone());
                request_metric_exemplars(app, backend, sender);
            }
        }
        // Backspace walks one step *up the screen series* — the chain a screen drills through (Metrics
        // → MetricDetail, Traces → TraceDetail → SpanDetail, Logs → LogDetail) — rather than through
        // the visit history: a trace detail opened by a log→trace jump steps up to the Traces list,
        // where Esc/← would return to the log it came from. A no-op on a screen's list route.
        KeyCode::Backspace => {
            if app.go_up() {
                request_refresh(app, backend.clone(), options.clone(), sender.clone());
                request_metric_exemplars(app, backend, sender);
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
            if let Some(name) = app.toggle_node() {
                // Discovery spans all metric data (picker-independent); only the value cap matters.
                request_metric_dims(name, backend.clone(), options.max_series, sender.clone());
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
                app.set_active_query(queries.join("\n"));
                app.selected = 0;
                app.scroll = 0;
                request_refresh(app, backend.clone(), options.clone(), sender.clone());
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
                request_metric_exemplars(app, backend, sender);
            }
        }
        // Time pan/zoom: `[`/`]` pan the window earlier/later by half its span; `-`/`+` (or `=`) zoom
        // out/in about the center. Each freezes the window to an absolute span (shown in the header)
        // and re-queries. No-ops (no window change) skip the refresh.
        KeyCode::Char('[') => {
            if app.pan_window(-0.5) {
                request_refresh(app, backend.clone(), options.clone(), sender.clone());
            }
        }
        KeyCode::Char(']') => {
            if app.pan_window(0.5) {
                request_refresh(app, backend.clone(), options.clone(), sender.clone());
            }
        }
        KeyCode::Char('-') => {
            if app.zoom_window(2.0) {
                request_refresh(app, backend.clone(), options.clone(), sender.clone());
            }
        }
        KeyCode::Char('+') | KeyCode::Char('=') => {
            if app.zoom_window(0.5) {
                request_refresh(app, backend.clone(), options.clone(), sender.clone());
            }
        }
        // `p` promotes (or demotes) the attribute key under the pane's cursor. Ahead of the paging
        // binding below and guarded on the focus, so the two never contend: it is the pane's action,
        // it needs the pane's cursor to mean anything, and it is the one key in this program that
        // *writes* to the database.
        KeyCode::Char('p') if matches!(app.effective_focus(), Focus::AttrTable(_)) => {
            toggle_promotion(app, backend, sender);
        }
        // Older/newer log paging (Logs list): `n` = older page, `p` = newer page. No-ops off the Logs
        // list or at the ends (`logs_page_*` guards the screen and the cursor stack).
        KeyCode::Char('n') => {
            if app.logs_page_older() {
                request_refresh(app, backend.clone(), options.clone(), sender.clone());
            }
        }
        KeyCode::Char('p') => {
            if app.logs_page_newer() {
                request_refresh(app, backend.clone(), options.clone(), sender.clone());
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
                    backend.clone(),
                    options.clone(),
                    sender.clone(),
                );
            }
        }
        KeyCode::Char('t') => open_time_range(app),
        KeyCode::Char('1') => switch_screen_history(
            app,
            Screen::Overview,
            backend.clone(),
            options.clone(),
            sender.clone(),
        ),
        KeyCode::Char('2') => switch_screen_history(
            app,
            Screen::Metrics,
            backend.clone(),
            options.clone(),
            sender.clone(),
        ),
        KeyCode::Char('3') => switch_screen_history(
            app,
            Screen::Traces,
            backend.clone(),
            options.clone(),
            sender.clone(),
        ),
        KeyCode::Char('4') => switch_screen_history(
            app,
            Screen::Logs,
            backend.clone(),
            options.clone(),
            sender.clone(),
        ),
        KeyCode::Char('e') if app.has_query() => begin_editing(app, backend, sender),
        KeyCode::Char('r') => {
            // An explicit refresh means "measure again": the Overview's attribute block is otherwise
            // reused across auto-refresh ticks (see `App::needs_attr_measure`), and `r` is how a
            // person asks for the numbers now rather than the ones from up to a minute ago.
            app.attr_stats = None;
            request_refresh(app, backend.clone(), options.clone(), sender.clone())
        }
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
    request_waterfall(app, backend, sender, options.ascii);
    Control::Continue
}

/// Move the row cursor by `delta` rows when the primary pane is a navigable list (traces/logs),
/// otherwise scroll the plain pane by the same amount. Moving the cursor releases any log→trace
/// focus so the waterfall follows the selection again.
pub(crate) fn move_selection(app: &mut App, delta: isize) {
    app.clear_trace_focus();
    // The attribute pane's rows are not contiguous — titles, per-section headers and spacers sit
    // between them — so its cursor steps key-to-key rather than by row index.
    if matches!(app.effective_focus(), Focus::AttrTable(_)) {
        app.move_attr_cursor(delta);
        return;
    }
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

/// Promote or demote the attribute key under the attribute pane's cursor.
///
/// Refused up front where the session cannot write — a local explorer opened the database read-only,
/// so there is nothing to attempt — and the refusal says what *would* work rather than reporting the
/// storage layer's error. Otherwise the whole set is sent (promotion is a *list*, and its order is the
/// column order), and the answer is what the daemon ended up with, so the pane re-renders against the
/// state that actually exists rather than the one this head assumed.
pub(crate) fn toggle_promotion(
    app: &mut App,
    backend: &Backend,
    sender: &mpsc::UnboundedSender<Update>,
) {
    let Some(key) = app.selected_attr_key() else {
        return;
    };
    if !backend.can_promote() {
        app.last_error = Some(
            "promotion changes what the writer writes, and this session opened the database \
             read-only. Point the explorer at the daemon that owns it (`imbh-tui --url \
             http://host:4318`) to change the promoted keys."
                .to_owned(),
        );
        return;
    }
    let mut keys = app.promoted.clone();
    match keys.iter().position(|k| *k == key) {
        Some(at) => {
            keys.remove(at);
        }
        None => keys.push(key),
    }
    request_promotion(keys, backend.clone(), sender.clone());
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
pub(crate) fn begin_editing(
    app: &mut App,
    backend: &Backend,
    sender: &mpsc::UnboundedSender<Update>,
) {
    app.mode = Mode::Editing;
    app.focus = Focus::Query;
    // Editing always opens with the caret at the end of the query, which is also what makes
    // `query_cursor` meaningful for the buffer it is about to edit.
    app.query_field().end();
    if app.screen() == Screen::Metrics && app.metric_names.is_empty() {
        request_vocabulary(app.screen(), backend.clone(), sender.clone());
    }
    app.refresh_completion();
}

/// Switch screens as a browser navigation: record the departed view in history first (unless it is
/// the same screen, which is not a navigation). Used by the numbered keys and the menu.
pub(crate) fn switch_screen_history(
    app: &mut App,
    screen: Screen,
    backend: Backend,
    options: Options,
    sender: mpsc::UnboundedSender<Update>,
) {
    if app.screen() != screen {
        app.push_history();
    }
    switch_screen(app, screen, backend, options, sender);
}

pub(crate) fn switch_screen(
    app: &mut App,
    screen: Screen,
    backend: Backend,
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
        request_vocabulary(screen, backend.clone(), sender.clone());
    }
    app.snapshot = Snapshot::message(screen.title(), "Loading...");
    request_refresh(app, backend, options, sender);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::TableData;
    use crate::testutil::app_with_discovered_dims;

    /// A backend and update channel for driving [`handle_key`]. `Backend::connect` contacts nothing,
    /// and the editing tests below start from an app whose completion vocabulary is already
    /// discovered, so no key they press dispatches a fetch.
    fn harness() -> (
        Backend,
        Options,
        mpsc::UnboundedSender<Update>,
        mpsc::UnboundedReceiver<Update>,
    ) {
        let (sender, receiver) = mpsc::unbounded_channel();
        let backend = Backend::connect("http://127.0.0.1:1").expect("a well-formed url");
        (backend, Options::default(), sender, receiver)
    }

    /// Press a key in the app's current mode.
    fn press(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
        let (backend, options, sender, _receiver) = harness();
        handle_key(
            app,
            KeyEvent::new(code, modifiers),
            &backend,
            &options,
            &sender,
        );
    }

    /// An editing app whose vocabulary is loaded, with `query` in the box and the caret at its end
    /// (where `begin_editing` parks it).
    fn editing(query: &str) -> App {
        let mut app = app_with_discovered_dims();
        app.set_active_query(query);
        app
    }

    #[test]
    fn the_cursor_keys_and_their_emacs_aliases_move_the_query_caret() {
        let mut app = editing("rate(x)");
        assert_eq!(
            app.query_caret(),
            7,
            "editing opens with the caret at the end"
        );

        press(&mut app, KeyCode::Left, KeyModifiers::NONE);
        press(&mut app, KeyCode::Left, KeyModifiers::NONE);
        assert_eq!(app.query_caret(), 5);
        // Typing lands at the caret, not at the end of the buffer.
        press(&mut app, KeyCode::Char('y'), KeyModifiers::NONE);
        assert_eq!(app.active_query(), "rate(yx)");
        assert_eq!(app.query_caret(), 6);

        // Ctrl-B/Ctrl-F are the same movement, and never type their own letter.
        press(&mut app, KeyCode::Char('b'), KeyModifiers::CONTROL);
        assert_eq!((app.active_query(), app.query_caret()), ("rate(yx)", 5));
        press(&mut app, KeyCode::Char('f'), KeyModifiers::CONTROL);
        assert_eq!((app.active_query(), app.query_caret()), ("rate(yx)", 6));

        // Home/End (and Ctrl-A/Ctrl-E) jump to the ends.
        press(&mut app, KeyCode::Home, KeyModifiers::NONE);
        assert_eq!(app.query_caret(), 0);
        press(&mut app, KeyCode::End, KeyModifiers::NONE);
        assert_eq!(app.query_caret(), 8);
        press(&mut app, KeyCode::Char('a'), KeyModifiers::CONTROL);
        assert_eq!((app.active_query(), app.query_caret()), ("rate(yx)", 0));
        press(&mut app, KeyCode::Char('e'), KeyModifiers::CONTROL);
        assert_eq!((app.active_query(), app.query_caret()), ("rate(yx)", 8));

        // Both ends saturate rather than running off the buffer.
        press(&mut app, KeyCode::Right, KeyModifiers::NONE);
        assert_eq!(app.query_caret(), 8);
        press(&mut app, KeyCode::Home, KeyModifiers::NONE);
        press(&mut app, KeyCode::Left, KeyModifiers::NONE);
        assert_eq!(app.query_caret(), 0);
    }

    #[test]
    fn backspace_and_delete_act_around_the_caret() {
        let mut app = editing("rate(x)");
        press(&mut app, KeyCode::Left, KeyModifiers::NONE); // between `x` and `)`
        press(&mut app, KeyCode::Backspace, KeyModifiers::NONE);
        assert_eq!((app.active_query(), app.query_caret()), ("rate()", 5));
        // Delete (and Ctrl-D) take the character *under* the caret.
        press(&mut app, KeyCode::Delete, KeyModifiers::NONE);
        assert_eq!((app.active_query(), app.query_caret()), ("rate(", 5));
        press(&mut app, KeyCode::Char('d'), KeyModifiers::CONTROL);
        assert_eq!(app.active_query(), "rate(", "delete at the end is a no-op");
        press(&mut app, KeyCode::Home, KeyModifiers::NONE);
        press(&mut app, KeyCode::Backspace, KeyModifiers::NONE);
        assert_eq!(
            app.active_query(),
            "rate(",
            "backspace at the start is a no-op"
        );
        press(&mut app, KeyCode::Char('d'), KeyModifiers::CONTROL);
        assert_eq!((app.active_query(), app.query_caret()), ("ate(", 0));
    }

    #[test]
    fn ctrl_k_kills_from_the_caret_to_the_end_of_the_line() {
        let mut app = editing("rate(http_requests_total)");
        press(&mut app, KeyCode::Home, KeyModifiers::NONE);
        press(&mut app, KeyCode::Right, KeyModifiers::NONE);
        press(&mut app, KeyCode::Right, KeyModifiers::NONE);
        press(&mut app, KeyCode::Char('k'), KeyModifiers::CONTROL);
        assert_eq!((app.active_query(), app.query_caret()), ("ra", 2));
        // At the end there is nothing left to kill.
        press(&mut app, KeyCode::Char('k'), KeyModifiers::CONTROL);
        assert_eq!(app.active_query(), "ra");

        // A newline-joined query (what the catalog's multi-metric "visualize" writes) kills one line at
        // a time, and a caret *on* the break kills only the break, joining the two.
        let mut app = editing("up\nrate(x)\ndown");
        press(&mut app, KeyCode::Home, KeyModifiers::NONE);
        press(&mut app, KeyCode::Char('k'), KeyModifiers::CONTROL);
        assert_eq!(app.active_query(), "\nrate(x)\ndown");
        press(&mut app, KeyCode::Char('k'), KeyModifiers::CONTROL);
        assert_eq!(app.active_query(), "rate(x)\ndown");
        press(&mut app, KeyCode::End, KeyModifiers::NONE);
        press(&mut app, KeyCode::Char('k'), KeyModifiers::CONTROL);
        assert_eq!(
            app.active_query(),
            "rate(x)\ndown",
            "a kill at the very end changes nothing"
        );
    }

    /// A successful commit dispatches a refresh, which spawns — hence the runtime.
    #[tokio::test]
    async fn the_absolute_range_form_edits_both_fields_with_the_same_caret_keys() {
        let mut app = App::new();
        app.open_absolute_form();
        assert_eq!(app.abs_field, 0);
        assert_eq!(
            app.abs_caret(),
            app.abs_start.len(),
            "the form opens on the start field with the caret at its end"
        );

        // Fix the minutes in place, as a user would: back over `:00`, delete a digit, type another.
        app.abs_start = "2026-07-21 15:00:00".to_owned();
        app.abs_cursor = app.abs_start.len();
        for _ in 0..3 {
            press(&mut app, KeyCode::Left, KeyModifiers::NONE);
        }
        press(&mut app, KeyCode::Backspace, KeyModifiers::NONE);
        press(&mut app, KeyCode::Char('5'), KeyModifiers::NONE);
        assert_eq!(app.abs_start, "2026-07-21 15:05:00");
        assert_eq!(app.abs_caret(), 16);

        // The caret is shared between the two fields, so moving between them re-seats it at the end of
        // whichever now has focus — and coming back does not resume the old field's offset.
        press(&mut app, KeyCode::Down, KeyModifiers::NONE);
        assert_eq!((app.abs_field, app.abs_caret()), (1, app.abs_end.len()));
        press(&mut app, KeyCode::Up, KeyModifiers::NONE);
        assert_eq!((app.abs_field, app.abs_caret()), (0, app.abs_start.len()));
        press(&mut app, KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!((app.abs_field, app.abs_caret()), (1, app.abs_end.len()));

        // `Ctrl-A` + `Ctrl-K` clears the focused field (and `Ctrl-A` never types an `a`).
        press(&mut app, KeyCode::Char('a'), KeyModifiers::CONTROL);
        press(&mut app, KeyCode::Char('k'), KeyModifiers::CONTROL);
        assert_eq!(app.abs_end, "");
        // Enter on an unparseable field keeps the form open with the error, as before.
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(app.mode, Mode::AbsoluteRange);
        assert!(app.abs_error.is_some());

        // Typing a valid end commits the window and closes the form.
        for character in "2026-07-21 16:00:00".chars() {
            press(&mut app, KeyCode::Char(character), KeyModifiers::NONE);
        }
        assert_eq!(app.abs_end, "2026-07-21 16:00:00");
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(app.mode, Mode::Normal);
        assert!(app.abs_window.is_some());
        assert_eq!(
            app.abs_start, "2026-07-21 15:05:00",
            "the in-place edit is what was committed"
        );
    }

    /// The attribute range is opened *through the line that displays it* — focus it, press Enter —
    /// rather than by a global key or from anywhere in the pane. The query range keeps `t`; nothing
    /// else in the global map opens a range form, so the two cannot be confused for each other.
    #[test]
    fn the_attribute_range_is_opened_through_the_line_that_shows_it() {
        use crate::model::AbsTarget;

        let mut app = App::new();
        assert_eq!(app.screen(), Screen::Overview);
        // The *table* stop has no Enter action — it would have to guess between the pane's two
        // things — so the form opens from the stop that displays the range.
        app.focus = Focus::AttrTable(0);
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(app.mode, Mode::Normal, "the rows have no Enter action");
        app.focus = Focus::AttrRange;
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(app.mode, Mode::AbsoluteRange);
        assert_eq!(app.abs_target, AbsTarget::Attributes);
        // The pane starts unbounded, and an empty form is how the form spells that.
        assert!(app.abs_start.is_empty() && app.abs_end.is_empty());
        press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(app.mode, Mode::Normal);

        // `t` still opens the *query* range picker, and it is the only global range binding.
        press(&mut app, KeyCode::Char('t'), KeyModifiers::NONE);
        assert_eq!(app.mode, Mode::TimeRange);
        assert_eq!(app.abs_target, AbsTarget::Attributes, "not committed yet");
        press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        press(&mut app, KeyCode::Char('a'), KeyModifiers::NONE);
        assert_eq!(app.mode, Mode::Normal, "`a` is not a binding");

        // Off the Overview the stop is gone, so Enter on a stale focus does nothing.
        app.route = Route::Logs;
        app.focus = Focus::AttrRange;
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(
            app.mode,
            Mode::Normal,
            "no attribute pane on the Logs screen, so no form"
        );
    }

    /// Promotion is offered only where it can work. A local session opened the database read-only, so
    /// `p` reports why it cannot rather than attempting a write that the storage layer would refuse —
    /// and the paging binding on the same key is untouched, because the two are guarded apart.
    #[test]
    fn promotion_is_refused_on_a_session_that_only_reads() {
        // A *local* backend, unlike the shared harness: this is the case under test, and it is also
        // the one that spawns no task, so it needs no runtime.
        let local = Backend::from(imbh::Db::in_memory().open().expect("open"));
        assert!(!local.can_promote());
        let (_, options, sender, _receiver) = harness();
        let press = |app: &mut App, code: KeyCode| {
            handle_key(
                app,
                KeyEvent::new(code, KeyModifiers::NONE),
                &local,
                &options,
                &sender,
            );
        };

        let mut app = App::new();
        app.focus = Focus::AttrTable(0);
        app.snapshot.detail = Some(crate::fetch::attribute_placeholder());
        // Shaped like a real pane: a section, its header, then the key. The section is load-bearing —
        // each one is a Tab stop, and the cursor belongs to the focused section.
        app.snapshot.detail.as_mut().expect("pane").table = Some(crate::model::PaneTable {
            data: crate::model::TableData::new(
                vec!["Key".to_owned(), "Scope".to_owned()],
                vec![
                    vec!["ALL TABLES -- 1 segment".to_owned(), String::new()],
                    vec!["Key".to_owned(), "Scope".to_owned()],
                    vec!["service.name".to_owned(), "attributes".to_owned()],
                ],
            ),
            kinds: vec![
                crate::model::AttrRow::Section,
                crate::model::AttrRow::Header,
                crate::model::AttrRow::Key("service.name".to_owned()),
            ],
        });
        app.snap_attr_cursor();
        assert_eq!(app.selected_attr_key().as_deref(), Some("service.name"));

        press(&mut app, KeyCode::Char('p'));
        let error = app.last_error.as_deref().expect("a reason, not a silence");
        assert!(error.contains("read-only"), "{error}");
        assert!(
            error.contains("--url"),
            "the message must name what would work: {error}"
        );
        assert!(
            app.promoted.is_empty(),
            "nothing was promoted locally, not even optimistically"
        );

        // Off the pane, `p` is the Logs paging key again — the guard is what keeps them apart.
        app.route = Route::Logs;
        app.focus = Focus::Primary;
        app.last_error = None;
        press(&mut app, KeyCode::Char('p'));
        assert!(app.last_error.is_none());
    }

    #[test]
    fn caret_editing_steps_over_whole_multi_byte_characters() {
        // The caret is a byte offset, so anything but whole-character steps would panic on a slice.
        let mut app = editing("{svc=\"café\"}");
        press(&mut app, KeyCode::Left, KeyModifiers::NONE); // before `}`
        press(&mut app, KeyCode::Left, KeyModifiers::NONE); // before `"`
        press(&mut app, KeyCode::Backspace, KeyModifiers::NONE);
        assert_eq!(app.active_query(), "{svc=\"caf\"}");
        press(&mut app, KeyCode::Char('é'), KeyModifiers::NONE);
        press(&mut app, KeyCode::Char('s'), KeyModifiers::NONE);
        assert_eq!(app.active_query(), "{svc=\"cafés\"}");
        // And a caret that has stepped over the multi-byte character still splits the query cleanly.
        press(&mut app, KeyCode::Left, KeyModifiers::NONE);
        press(&mut app, KeyCode::Left, KeyModifiers::NONE);
        assert_eq!(app.query_before_caret(), "{svc=\"caf");
    }

    #[test]
    fn a_modified_letter_never_types_itself_into_the_query() {
        // Ctrl-<letter> is a caret command; Alt-<letter> is unbound. Neither may reach the buffer, or
        // every Emacs binding would insert its own letter as a side effect.
        let mut app = editing("");
        for (code, modifiers) in [
            (KeyCode::Char('b'), KeyModifiers::CONTROL),
            (KeyCode::Char('f'), KeyModifiers::CONTROL),
            (KeyCode::Char('a'), KeyModifiers::CONTROL),
            (KeyCode::Char('e'), KeyModifiers::CONTROL),
            (KeyCode::Char('d'), KeyModifiers::CONTROL),
            (KeyCode::Char('k'), KeyModifiers::CONTROL),
            (KeyCode::Char('x'), KeyModifiers::CONTROL),
            (KeyCode::Char('b'), KeyModifiers::ALT),
        ] {
            press(&mut app, code, modifiers);
        }
        assert_eq!(app.active_query(), "");
        // A shifted letter is ordinary input, though.
        press(&mut app, KeyCode::Char('R'), KeyModifiers::SHIFT);
        assert_eq!(app.active_query(), "R");
    }

    #[test]
    fn moving_the_caret_re_derives_the_completion_popup() {
        // The popup completes the token the caret sits on, so walking back into a token reopens it and
        // walking off one closes it.
        let mut app = editing("http_requests_total{ser");
        press(&mut app, KeyCode::Left, KeyModifiers::NONE);
        let completion = app.completion.as_ref().expect("label-name candidates");
        assert_eq!(completion.candidates[0].text, "service");
        // Accepting mid-query replaces only the token before the caret and keeps the tail.
        press(&mut app, KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(app.active_query(), "http_requests_total{servicer");
        assert_eq!(app.query_caret(), "http_requests_total{service".len());
    }

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
        app.snapshot.table = Some(TableData::new(
            vec!["Metric".into(), "Kind".into()],
            vec![
                vec!["a".into(), "gauge".into()],
                vec!["b".into(), "sum".into()],
                vec!["c".into(), "sum".into()],
            ],
        ));
        // Table rows are indexed from 0 (not offset by a header line as lists are).
        assert_eq!(app.selectable_bounds(), Some((0, 2)));

        app.selected = 0;
        move_selection(&mut app, 2);
        assert_eq!(app.selected, 2);
        move_selection(&mut app, 5); // saturates at the last row
        assert_eq!(app.selected, 2);

        // An empty table has no selectable rows.
        app.snapshot.table = Some(TableData::new(vec!["Metric".into()], vec![]));
        assert_eq!(app.selectable_bounds(), None);
    }
}

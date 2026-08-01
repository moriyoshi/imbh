//! Rendering. [`draw`] paints a frame from the [`App`](crate::app::App) state.
//!
//! The per-view painters live in the submodules ([`metrics`], [`logs`], [`traces`], [`overlays`]);
//! this module owns the frame layout, the chrome shared by every view, and the mascot overlay.
//! Rendering never mutates the app — the few things only the renderer knows (scroll bounds, the
//! chart geometry) are published back through `Cell`/`RefCell` fields.

pub(crate) mod glyphs;
pub(crate) mod logs;
pub(crate) mod metrics;
pub(crate) mod overlays;
pub(crate) mod traces;

use imbh::Timestamp;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, List, ListItem, ListState, Paragraph, Sparkline, Wrap};
use unicode_width::UnicodeWidthStr;

use crate::app::App;
use crate::chart::ascii_chart;
use crate::format::wrapped_rows;
use crate::mascot::{MASCOT_ART_HEIGHT, MASCOT_BOTTOM_MARGIN, mascot_art, mascot_phase};
use crate::model::{Focus, MENU_LEN, Mode, Options, Route, Screen};
use crate::syntax::highlight_query;
use crate::time::format_datetime_ns;
use crate::ui::glyphs::Glyphs;
use crate::ui::logs::draw_log_detail;
use crate::ui::metrics::{draw_metric_detail, draw_metric_table};
use crate::ui::overlays::{draw_absolute_range, draw_completion_popup, draw_time_range_picker};
use crate::ui::traces::{draw_span_detail, draw_trace_detail};
use crate::waterfall::{WATERFALL_NAME_W, WATERFALL_SUFFIX_W, WaterfallView, render_waterfall};

/// The smallest terminal the full UI lays out in; below either dimension `draw` shows a resize prompt
/// instead (first-release acceptance criterion, TUI_PLAN.md §10). Kept at/under the smallest size the
/// existing render tests exercise so those still paint the real UI.
pub(crate) const MIN_COLS: u16 = 40;

pub(crate) const MIN_ROWS: u16 = 10;

/// Border style for a pane: a bold cyan outline when it holds the focus ring, the default (dim) border
/// otherwise. Applied via `Block::border_style` so the focused pane reads at a glance.
pub(crate) fn focus_border(focused: bool) -> Style {
    if focused {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    }
}

/// Overlay the animated mascot ("Atta") at the [`Mascot`](crate::mascot::Mascot) controller's
/// current position within `area`.
/// A small borderless overlay floating above the content (its cells are cleared first), whose facing
/// picks the art pair and whose waddle phase picks the frame. Position, facing, and phase are advanced
/// each redraw by [`Mascot::update`](crate::mascot::Mascot::update) in
/// [`run`](crate::runtime::run); here we only blit. The caller gates visibility (hidden
/// by default, toggled with `m`), and the block-glyph art means it is only drawn on non-`--ascii`
/// terminals.
pub(crate) fn draw_mascot(frame: &mut ratatui::Frame<'_>, app: &App, area: Rect) {
    let body = app.mascot.body;
    let art = mascot_art(body.facing, mascot_phase(body.phase_ns));
    let width = art
        .iter()
        .map(|row| UnicodeWidthStr::width(*row) as u16)
        .max()
        .unwrap_or(0);
    // Skip on a terminal too small to host the overlay without colliding with the chrome.
    if width == 0
        || area.width < width + 1
        || area.height < MASCOT_ART_HEIGHT + MASCOT_BOTTOM_MARGIN + 1
    {
        return;
    }
    // Clamp the controller's (possibly sub-cell, possibly out-of-band during a ride) position to a cell
    // that keeps the whole overlay on screen.
    let max_x = area.right().saturating_sub(width);
    let max_y = area.bottom().saturating_sub(MASCOT_ART_HEIGHT);
    let x = (body.x.round() as i64).clamp(area.left() as i64, max_x as i64) as u16;
    let y = (body.y.round() as i64).clamp(area.top() as i64, max_y as i64) as u16;
    let overlay = Rect {
        x,
        y,
        width,
        height: MASCOT_ART_HEIGHT,
    };
    let lines: Vec<Line> = art
        .iter()
        .map(|row| {
            Line::from(Span::styled(
                (*row).to_owned(),
                Style::default().fg(Color::Magenta),
            ))
        })
        .collect();
    frame.render_widget(Clear, overlay);
    frame.render_widget(Paragraph::new(lines), overlay);
}

/// The degraded render for a terminal smaller than [`MIN_COLS`]×[`MIN_ROWS`]: a centered prompt telling
/// the user the required size and the current one. Pure ASCII (no chrome glyphs) so it paints even in
/// the most constrained state, and never overflows the tiny area.
pub(crate) fn draw_too_small(frame: &mut ratatui::Frame<'_>, area: Rect, _ascii: bool) {
    frame.render_widget(Clear, area);
    let lines = vec![
        Line::from("Terminal too small"),
        Line::from(format!("Resize to at least {MIN_COLS}x{MIN_ROWS}")),
        Line::from(format!("(now {}x{})", area.width, area.height)),
    ];
    let paragraph = Paragraph::new(lines)
        .alignment(ratatui::layout::Alignment::Center)
        .wrap(Wrap { trim: true });
    // Vertically center the 3-line message when there is room to spare.
    let top = area.height.saturating_sub(3) / 2;
    let inner = Rect {
        x: area.x,
        y: area.y + top,
        width: area.width,
        height: area.height.saturating_sub(top),
    };
    frame.render_widget(paragraph, inner);
}

pub(crate) fn draw(frame: &mut ratatui::Frame<'_>, app: &App, options: &Options) {
    let area = frame.area();
    // Below the minimum, the paned layout can't be laid out meaningfully (borders overlap, geometry
    // underflows). Show a clear resize prompt instead — the first-release small-terminal criterion
    // (TUI_PLAN.md §10). No chart geometry is published, so the mascot ride has nothing stale to read.
    if area.width < MIN_COLS || area.height < MIN_ROWS {
        app.chart_geom.replace(None);
        draw_too_small(frame, area, options.ascii);
        return;
    }
    // The chart geometry the mascot rides is only valid on the metric diagram; drop it otherwise so a
    // stale line never lingers after navigating away. `draw_metric_detail` repopulates it when shown.
    if !matches!(app.route, Route::MetricDetail { .. }) {
        app.chart_geom.replace(None);
    }
    let g = Glyphs::new(options.ascii);
    // The menu bar is always the top line; the content below it depends on the route (the detail
    // views render as ordinary content there, not as full-screen modal takeovers).
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(0)])
        .split(area);
    let nav_area = outer[0];
    let content_area = outer[1];
    // Midnight-Commander-style one-line header: a brand + screen menu on the left, the time-range and
    // live-clock selector on the right, on a single coloured bar. The range portion is the on-screen
    // anchor the time-range dropdown drops down from.
    let menu_active = app.mode == Mode::Menu;
    // The focus ring's current stop (a stale Query focus already snapped to Primary); drives the pane
    // highlight, the time selector, and — for the menu items — the whole-bar recolour below.
    let focus = app.effective_focus();
    // Cyan is the focus colour (it also draws the focused-pane borders). The whole bar turns cyan the
    // moment the focus ring lands on any menu-bar item — a screen item, the time selector, the open
    // time-range picker, or the F9 menu — so the user notices focus has moved up here. When focus is
    // elsewhere the bar reverts to the readiness colour: a calm blue once the last query has landed,
    // muted grey while one is in flight.
    let menubar_focused = menu_active
        || app.mode == Mode::TimeRange
        || app.mode == Mode::AbsoluteRange
        || matches!(focus, Focus::Menu(_) | Focus::TimeRange);
    let bar_bg = if menubar_focused {
        Color::Cyan
    } else if app.loading {
        Color::DarkGray
    } else {
        Color::Blue
    };
    let bar = Style::default().bg(bar_bg).fg(Color::Black);
    // Two distinct in-bar chips, so the active screen and the focus position never look alike: a solid
    // light-filled chip marks the *active* screen ("you are here"), while a dark chip with cyan text
    // marks the *focus-ring cursor* ("focus is on this item"). Cyan stays reserved for the focus cursor.
    let active_chip = Style::default().bg(Color::Gray).fg(Color::Black);
    let cursor_chip = Style::default()
        .bg(Color::Black)
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD);
    // The black circle is the black hole imbh is named for; it also marks the brand as a logo rather
    // than a selectable menu item. U+2B24 (not the visually similar U+25CF) is East-Asian-width
    // *unambiguous* (one cell everywhere), so it never desynchronizes the header's width math.
    let mut left: Vec<Span<'static>> = vec![
        // Plain black (no BOLD): bold + black renders as bright-black/grey on many terminals, which
        // washes out on the blue bar — so the brand keeps the same true black as the logo circle.
        Span::styled(" IMBH ", bar),
        Span::styled(format!("{} ", g.logo), bar),
    ];
    for (index, (screen, label)) in [
        (Screen::Overview, "1 Overview"),
        (Screen::Metrics, "2 Metrics"),
        (Screen::Traces, "3 Traces"),
        (Screen::Logs, "4 Logs"),
    ]
    .into_iter()
    .enumerate()
    {
        // The focus-ring cursor (the F9 menu cursor, or the Tab focus parked on this item) takes the
        // cursor chip; the current screen keeps its active chip. Both can show at once on different
        // items, and stay distinct when they coincide (the cursor wins).
        let is_cursor = if menu_active {
            app.menu_cursor == index
        } else {
            matches!(focus, Focus::Menu(focused_index) if focused_index == index)
        };
        let is_active = screen == app.screen();
        let style = if is_cursor {
            cursor_chip
        } else if is_active {
            active_chip
        } else {
            bar
        };
        left.push(Span::styled(format!(" {label} "), style));
        left.push(Span::styled(" ", bar));
    }
    let clock = format_datetime_ns(Timestamp::now().0);
    // Auto-refresh only makes sense for a relative window (an absolute one never moves), so the `!`
    // flag rides right after the range text and only when the window is relative.
    let auto = if app.auto_refresh && app.abs_window.is_none() {
        "!"
    } else {
        ""
    };
    // A timer-clock icon (U+23F2, EAW-unambiguous) prefixes the wall clock; dropped in `--ascii`.
    let clock_icon = if g.clock.is_empty() {
        String::new()
    } else {
        format!("{} ", g.clock)
    };
    let range_text = format!(
        " {}{}  {}{} ",
        app.range_summary(&g),
        auto,
        clock_icon,
        clock
    );
    // The time selector is a focus-ring stop but never an "active screen", so it only ever takes the
    // cursor chip — when the menu cursor is on it, the ring is parked on it, or its dropdown/form is
    // open — otherwise the plain bar.
    let range_focused = if menu_active {
        app.menu_cursor == MENU_LEN - 1
    } else {
        app.mode == Mode::TimeRange || app.mode == Mode::AbsoluteRange || focus == Focus::TimeRange
    };
    let range_style = if range_focused { cursor_chip } else { bar };
    let span_width = |spans: &[Span]| -> usize {
        spans
            .iter()
            .map(|span| UnicodeWidthStr::width(span.content.as_ref()))
            .sum()
    };
    let left_width = span_width(&left);
    let right_width = UnicodeWidthStr::width(range_text.as_str());
    // Push the range selector to the right edge; a filler of bar-coloured spaces spans the gap.
    let pad = (nav_area.width as usize).saturating_sub(left_width + right_width);
    let mut spans = left;
    spans.push(Span::styled(" ".repeat(pad), bar));
    spans.push(Span::styled(range_text, range_style));
    frame.render_widget(Paragraph::new(Line::from(spans)).style(bar), nav_area);
    // The range selector's on-screen rect (right-aligned on the one-line header) anchors the dropdown.
    let indicator_x = (left_width + pad).min(nav_area.width as usize) as u16;
    let indicator_area = Rect {
        x: nav_area.x + indicator_x,
        y: nav_area.y,
        width: (right_width as u16).min(nav_area.width.saturating_sub(indicator_x)),
        height: 1,
    };

    // Detail routes own the whole content area (their own header/body/hint, no query pane); the
    // range/menu overlays still apply on top since they are global.
    if let Some(record) = app.route_log_record() {
        draw_log_detail(
            frame,
            app,
            record,
            content_area,
            focus == Focus::Primary,
            &g,
        );
        draw_global_overlays(frame, app, indicator_area, area, options.ascii);
        return;
    }
    if let Some(detail) = app.route_metric_detail() {
        draw_metric_detail(
            frame,
            app,
            detail,
            options,
            content_area,
            focus == Focus::Primary,
        );
        draw_global_overlays(frame, app, indicator_area, area, options.ascii);
        return;
    }
    if let Some(detail) = app.route_trace_detail() {
        draw_trace_detail(
            frame,
            app,
            detail,
            content_area,
            focus == Focus::Primary,
            &g,
        );
        draw_global_overlays(frame, app, indicator_area, area, options.ascii);
        return;
    }
    if let Some((trace_id, span)) = app.route_span_detail() {
        draw_span_detail(
            frame,
            app,
            trace_id,
            span,
            content_area,
            focus == Focus::Primary,
            &g,
        );
        draw_global_overlays(frame, app, indicator_area, area, options.ascii);
        return;
    }

    // List views: query pane (except Overview) + main + status, within the content area.
    let has_query = app.screen() != Screen::Overview;
    let rows = if has_query {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Min(4),
                Constraint::Length(2),
            ])
            .split(content_area)
    } else {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(4), Constraint::Length(2)])
            .split(content_area)
    };
    let (query_area, main_area, status_area) = if has_query {
        (Some(rows[0]), rows[1], rows[2])
    } else {
        (None, rows[0], rows[1])
    };

    if let Some(query_area) = query_area {
        let mut spans = highlight_query(app.screen(), app.active_query(), &g);
        if app.mode == Mode::Editing {
            // A block caret; the global cursor stays hidden so this marks the edit point.
            spans.push(Span::styled(
                " ",
                Style::default().add_modifier(Modifier::REVERSED),
            ));
        }
        let query_title = if app.mode == Mode::Editing {
            format!(
                "Query (Enter: run {s} Tab: complete {s} Esc: cancel)",
                s = g.sep
            )
        } else {
            "Query (e: edit)".to_owned()
        };
        frame.render_widget(
            Paragraph::new(Line::from(spans)).block(
                g.block()
                    .border_style(focus_border(focus == Focus::Query))
                    .title(query_title),
            ),
            query_area,
        );
    }

    let main = if app.snapshot.chart.is_empty() || main_area.height < 9 {
        vec![main_area]
    } else {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(6), Constraint::Min(3)])
            .split(main_area)
            .to_vec()
    };
    if !app.snapshot.chart.is_empty() && main.len() == 2 {
        if options.ascii {
            let chart = ascii_chart(
                &app.snapshot.chart,
                main[0].width.saturating_sub(2) as usize,
                main[0].height.saturating_sub(2) as usize,
            );
            frame.render_widget(
                Paragraph::new(chart).block(g.block().title("Series")),
                main[0],
            );
        } else {
            frame.render_widget(
                Sparkline::default()
                    .block(g.block().title("Series"))
                    .data(&app.snapshot.chart),
                main[0],
            );
        }
    }
    let list_area = *main.last().expect("at least one main area");
    // A snapshot with a detail pane (the Traces waterfall) splits the results region vertically:
    // the primary list on top, the detail below. Both keep full width — waterfall bars are wide.
    let (primary_area, detail) = match &app.snapshot.detail {
        Some(detail) => {
            let parts = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
                .split(list_area);
            (parts[0], Some((parts[1], detail)))
        }
        None => (list_area, None),
    };

    let viewport = primary_area.height.saturating_sub(2);
    app.page_rows.set(viewport.max(1));
    let primary_focused = focus == Focus::Primary;
    if let Some(table) = &app.snapshot.table {
        draw_metric_table(frame, app, table, primary_area, primary_focused, &g);
    } else if let Some(from) = app.snapshot.list_from {
        // Cursor-navigable list: header lines (< from) are dimmed and unselectable; the selected row
        // is highlighted and the List widget scrolls to keep it in view.
        let selection = app.selectable_bounds().map(|(first, last)| {
            let selected = app.selected.clamp(first, last);
            (selected, first, last)
        });
        let items = app
            .snapshot
            .lines
            .iter()
            .enumerate()
            .map(|(index, line)| {
                let style = if index < from {
                    Style::default().fg(Color::DarkGray)
                } else {
                    Style::default()
                };
                ListItem::new(Span::styled(line.clone(), style))
            })
            .collect::<Vec<_>>();
        let title = match selection {
            Some((selected, first, last)) => format!(
                "{}  [{}/{}]",
                app.snapshot.title,
                selected - first + 1,
                last - first + 1
            ),
            None => app.snapshot.title.clone(),
        };
        let mut state = ListState::default();
        state.select(selection.map(|(selected, ..)| selected));
        frame.render_stateful_widget(
            List::new(items)
                .block(
                    g.block()
                        .border_style(focus_border(primary_focused))
                        .title(title),
                )
                .highlight_style(
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
            primary_area,
            &mut state,
        );
    } else {
        // Plain scrolled view. Publish the scroll bounds derived from this frame's geometry so the key
        // handler can clamp. `inner_width` subtracts the block's borders.
        let text = if app.snapshot.lines.is_empty() {
            "No data".to_owned()
        } else {
            app.snapshot.lines.join("\n")
        };
        let inner_width = primary_area.width.saturating_sub(2);
        let total_rows: u16 = app
            .snapshot
            .lines
            .iter()
            .map(|line| wrapped_rows(line, inner_width))
            .sum::<u32>()
            .min(u16::MAX as u32) as u16;
        let max_scroll = total_rows.saturating_sub(viewport);
        app.max_scroll.set(max_scroll);
        let scroll = app.scroll.min(max_scroll);
        let list_title = if max_scroll > 0 {
            format!(
                "{}  [{}/{} {}]",
                app.snapshot.title,
                scroll,
                max_scroll,
                g.scroll()
            )
        } else {
            app.snapshot.title.clone()
        };
        frame.render_widget(
            Paragraph::new(text)
                .wrap(Wrap { trim: false })
                .scroll((scroll, 0))
                .block(
                    g.block()
                        .border_style(focus_border(primary_focused))
                        .title(list_title),
                ),
            primary_area,
        );
    }

    if let Some((detail_area, detail)) = detail {
        // The bare title line costs the pane's first row; the rest is where waterfall rows land.
        let visible = detail_area.height.saturating_sub(1) as usize;
        let mut title = detail.title.clone();
        let detail_text = if let Some(waterfall) = &detail.waterfall {
            // The pane has no side borders, so the full width is usable text. Give the bar every cell
            // left after the fixed prefix (marker + name), the two `|`, and the trailing duration
            // column, so the bars stretch to fill the pane instead of a fixed 40 cells.
            let bar_cells = (detail_area.width as usize)
                .saturating_sub(1 + WATERFALL_NAME_W + 2 + WATERFALL_SUFFIX_W)
                .max(1);
            // No span cursor on this pane and it always shows the trace from its root, so neither the
            // horizontal name scroll nor the relative indent has anything to anchor to.
            let rows = render_waterfall(
                waterfall,
                &WaterfallView {
                    bar_cells,
                    ..WaterfallView::default()
                },
            );
            // This preview pane is a fixed slice of the results area and does not scroll: say so when a
            // deep trace overflows it, so the hidden spans are never silently dropped. The full,
            // scrolling waterfall is one Enter away (`Route::TraceDetail`).
            if rows.len() > visible {
                title = format!(
                    "Waterfall: {} of {} spans {} enter: all",
                    visible,
                    rows.len(),
                    g.dash
                );
            }
            rows.join("\n")
        } else if detail.lines.is_empty() {
            "No data".to_owned()
        } else {
            detail.lines.join("\n")
        };
        frame.render_widget(
            Paragraph::new(detail_text)
                .wrap(Wrap { trim: false })
                // No border box on the waterfall pane: a bare title line keeps the trace id visible
                // while freeing the left/right/bottom edge cells so the bars sit flush against them.
                .block(Block::default().title(title)),
            detail_area,
        );
    }

    let status = if let Some(error) = &app.last_error {
        format!("error: {error}")
    } else if app.mode == Mode::Menu {
        format!(
            "menu | {l}{r}/tab move {s} enter select {s} esc close",
            l = g.left,
            r = g.right,
            s = g.sep
        )
    } else {
        // Readiness rides the menu-bar colour, the range/auto-refresh state ride the header, so the
        // footer is purely the key legend now.
        let sep = g.sep;
        let detail_hint = match app.screen() {
            Screen::Logs => format!(" {sep} enter detail"),
            Screen::Traces => format!(" {sep} enter trace detail {sep} L logs"),
            Screen::Metrics if app.active_query().trim().is_empty() => {
                format!(" {sep} space expand/select series {sep} enter visualize")
            }
            Screen::Metrics => format!(" {sep} enter series detail {sep} {}/esc back", g.left),
            _ => String::new(),
        };
        // The mascot toggle is only advertised on terminals that can render it.
        let mascot_hint = if options.ascii {
            String::new()
        } else {
            format!(" {sep} m mascot")
        };
        format!(
            "q quit {sep} F9 menu {sep} 1-4 screen {sep} tab focus {sep} {l}{r} back/fwd {sep} {scroll} move{detail_hint} {sep} r refresh {sep} R auto-refresh {sep} t range {sep} e edit{mascot_hint}{ascii}",
            l = g.left,
            r = g.right,
            scroll = g.scroll(),
            ascii = if options.ascii {
                format!(" {sep} ASCII")
            } else {
                String::new()
            },
        )
    };
    frame.render_widget(
        Paragraph::new(status).wrap(Wrap { trim: true }),
        status_area,
    );

    // Overlays render last so they sit above the panels.
    draw_global_overlays(frame, app, indicator_area, area, options.ascii);
    if app.mode == Mode::Editing
        && let Some(query_area) = query_area
        && let Some(completion) = app.completion.as_ref()
    {
        draw_completion_popup(frame, completion, query_area, area, &g);
    }
}

/// The route-independent overlays: the time-range dropdown and the absolute-range form, both anchored
/// to the menu bar's range selector, so they can appear over any route (including the detail views).
pub(crate) fn draw_global_overlays(
    frame: &mut ratatui::Frame<'_>,
    app: &App,
    indicator_area: Rect,
    area: Rect,
    ascii: bool,
) {
    let g = Glyphs::new(ascii);
    // The mascot is opt-in (toggle `m`) and never shown on `--ascii` terminals since its art is block
    // glyphs. It floats above the content but below the pickers, so a dropdown/form is never hidden.
    if app.show_mascot && !ascii {
        draw_mascot(frame, app, area);
    }
    if app.mode == Mode::TimeRange {
        draw_time_range_picker(frame, app, indicator_area, area, &g);
    }
    if app.mode == Mode::AbsoluteRange {
        draw_absolute_range(frame, app, indicator_area, area, &g);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;

    use crate::completion::{Candidate, CandidateKind, Completion};
    use crate::mascot::MascotCtx;
    use crate::model::{DetailPane, LogRecord, MetricDetail, Snapshot};
    use crate::testutil::{ascii_trace, nested_trace};
    use crate::waterfall::build_trace_detail;

    #[test]
    fn ascii_mode_renders_only_ascii_across_the_ui() {
        use ratatui::backend::TestBackend;

        // Render `draw` in `--ascii` mode across the states that own the UI chrome (borders, header
        // logo/clock, hint separators, arrows, pickers, detail views) and assert every emitted cell is
        // pure ASCII — the guarantee `--ascii` makes. Content is never rewritten, so the fixtures below
        // deliberately use ASCII-only titles/bodies; a non-ASCII cell can then only be leaked chrome.
        let options = Options {
            ascii: true,
            ..Options::default()
        };

        let log_record = LogRecord {
            time_ns: 0,
            severity: "INFO".to_owned(),
            service: Some("api".to_owned()),
            body: "hello".to_owned(),
            trace_id: Some("abcdef01".to_owned()),
            span_id: Some("0123".to_owned()),
            attributes: vec![("k".to_owned(), "v".to_owned())],
            resource: Vec::new(),
            scope: Vec::new(),
        };
        let metric_detail = MetricDetail {
            labels: "service=api".to_owned(),
            query: "up".to_owned(),
            points: vec![(0, 1.0), (1_000_000_000, 2.0), (2_000_000_000, 3.0)],
        };

        // The states that own the UI chrome, each a fresh App tweaked into the state under test.
        let states: Vec<(&str, App)> = vec![
            ("overview", App::new()),
            ("menu", {
                let mut app = App::new();
                app.mode = Mode::Menu;
                app
            }),
            ("time-range picker", {
                let mut app = App::new();
                app.mode = Mode::TimeRange;
                app
            }),
            ("absolute-range form", {
                let mut app = App::new();
                app.open_absolute_form();
                app.abs_error = Some("start must be before end".to_owned());
                app
            }),
            ("scrolled list", {
                // Many lines in a short terminal forces the `[n/m ^v]` scroll title to render.
                let mut app = App::new();
                app.snapshot.lines = (0..40).map(|i| format!("row {i}")).collect();
                app.scroll = 5;
                app
            }),
            ("metrics query + completion", {
                let mut app = App::new();
                app.route = Route::Metrics;
                app.query[1] = "rate(".to_owned();
                app.mode = Mode::Editing;
                app.completion = Some(Completion {
                    candidates: vec![Candidate {
                        text: "http_requests".to_owned(),
                        kind: CandidateKind::Metric,
                    }],
                    selected: 0,
                });
                app
            }),
            ("log detail", {
                let mut app = App::new();
                app.route = Route::LogDetail {
                    record: log_record.clone(),
                };
                app
            }),
            ("metric detail", {
                let mut app = App::new();
                app.route = Route::MetricDetail {
                    detail: metric_detail.clone(),
                };
                app
            }),
            ("trace detail", {
                let mut app = App::new();
                app.route = Route::TraceDetail {
                    detail: build_trace_detail(&ascii_trace(), true),
                };
                app.span_cursor = 1;
                app
            }),
            ("trace detail with a scrolled name column", {
                // Deeply nested, with names longer than the name column: exercises the indent cap and
                // the horizontally clipped/scrolled name field, whose `<`/`>` markers must stay ASCII.
                let mut app = App::new();
                app.route = Route::TraceDetail {
                    detail: build_trace_detail(&nested_trace(), true),
                };
                app.span_cursor = 17;
                app
            }),
            ("span detail", {
                let mut app = App::new();
                let detail = build_trace_detail(&ascii_trace(), true);
                app.route = Route::SpanDetail {
                    trace_id: detail.trace_id.clone(),
                    span: detail.spans[1].clone(),
                };
                app
            }),
            ("traces list with a waterfall preview", {
                let mut app = App::new();
                let detail = build_trace_detail(&ascii_trace(), true);
                app.route = Route::Traces;
                app.snapshot = Snapshot {
                    title: "TraceQL".to_owned(),
                    lines: vec![
                        "1 matching traces".into(),
                        format!("{}  ts", detail.trace_id),
                    ],
                    list_from: Some(1),
                    // A deeper trace than the short preview pane fits, so the truncation note renders.
                    detail: Some(DetailPane {
                        title: "Waterfall".to_owned(),
                        lines: Vec::new(),
                        waterfall: Some(detail.waterfall.clone()),
                    }),
                    ..Default::default()
                };
                app.selected = 1;
                app
            }),
        ];

        for (label, app) in &states {
            let mut terminal = Terminal::new(TestBackend::new(48, 10)).unwrap();
            terminal.draw(|frame| draw(frame, app, &options)).unwrap();
            let buffer = terminal.backend().buffer();
            for y in 0..buffer.area.height {
                for x in 0..buffer.area.width {
                    let sym = buffer[(x, y)].symbol();
                    assert!(
                        sym.is_ascii(),
                        "non-ASCII cell {sym:?} at ({x},{y}) in --ascii state {label:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn mascot_overlay_renders_at_its_resting_position() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let mut app = App::new();
        let area = Rect::new(0, 0, 40, 12);
        // Place the mascot against a real area (its initial bottom-right resting band), Active so it
        // does not wander off it.
        app.mascot.update(&[], &MascotCtx { area, chart: None });

        let mut terminal = Terminal::new(TestBackend::new(40, 12)).unwrap();
        terminal
            .draw(|frame| draw_mascot(frame, &app, frame.area()))
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        let symbol = |x: u16, y: u16| buffer[(x, y)].symbol().to_owned();

        // place(): x = right - (ART_WIDTH+1) = 31, y = bottom - (ART_HEIGHT+BOTTOM_MARGIN) = 7, so the
        // 8-wide middle art row sits at y=8, spanning cols 31..=38 with a one-column right margin.
        let row: String = (0..buffer.area.width).map(|x| symbol(x, 8)).collect();
        assert!(
            row.contains("▄█▄"),
            "middle art row expected at y=8, got {row:?}"
        );
        assert_ne!(symbol(31, 8), " "); // left edge of the 8-wide art
        assert_ne!(symbol(38, 8), " "); // flush right
        assert_eq!(symbol(39, 8), " "); // one-column right margin
        // The bottom two rows (where the status/hint bar lives) stay clear beneath the mascot.
        assert_eq!(symbol(38, 10), " ");
        assert_eq!(symbol(38, 11), " ");
    }

    #[test]
    fn mascot_overlay_is_skipped_on_a_tiny_terminal() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let app = App::new();
        // Too short to fit the 3 art rows above the 2-row status bar: nothing is drawn (no panic).
        let mut terminal = Terminal::new(TestBackend::new(40, 5)).unwrap();
        terminal
            .draw(|frame| draw_mascot(frame, &app, frame.area()))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let blank = (0..buffer.area.width)
            .flat_map(|x| (0..buffer.area.height).map(move |y| (x, y)))
            .all(|(x, y)| buffer[(x, y)].symbol() == " ");
        assert!(blank, "mascot must not draw when the terminal is too small");
    }

    #[test]
    fn tiny_terminal_shows_a_resize_prompt_and_a_full_one_does_not() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let app = App::new();
        let options = Options::default();
        let text_of = |w: u16, h: u16| {
            let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
            terminal.draw(|frame| draw(frame, &app, &options)).unwrap();
            let buffer = terminal.backend().buffer();
            let mut out = String::new();
            for y in 0..buffer.area.height {
                for x in 0..buffer.area.width {
                    out.push_str(buffer[(x, y)].symbol());
                }
            }
            out
        };
        // Below the minimum: the resize prompt, and none of the normal chrome.
        let tiny = text_of(20, 6);
        assert!(tiny.contains("too small"), "tiny render: {tiny:?}");
        assert!(tiny.contains("40x10"));
        // A comfortable terminal renders the real UI, never the prompt.
        let full = text_of(80, 24);
        assert!(!full.contains("too small"), "full render leaked the prompt");
    }
}

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
use crate::model::{
    AbsTarget, AttrRow, DetailStyle, Focus, MENU_LEN, Mode, Options, PaneTable, Route, Screen,
};
use crate::syntax::{highlight_caret, highlight_query};
use crate::time::format_datetime_ns;
use crate::ui::glyphs::Glyphs;
use crate::ui::logs::draw_log_detail;
use crate::ui::metrics::{column_widths, draw_metric_detail, draw_metric_table, pad_cell};
use crate::ui::overlays::{
    draw_absolute_range, draw_completion_popup, draw_loading_banner, draw_time_range_picker,
};
use crate::ui::traces::{draw_span_detail, draw_trace_detail};
use crate::waterfall::{WATERFALL_NAME_W, WATERFALL_SUFFIX_W, render_waterfall_window};

/// The smallest terminal the full UI lays out in; below either dimension `draw` shows a resize prompt
/// instead (first-release acceptance criterion, TUI_PLAN.md §10). Kept at/under the smallest size the
/// existing render tests exercise so those still paint the real UI.
pub(crate) const MIN_COLS: u16 = 40;

pub(crate) const MIN_ROWS: u16 = 10;

/// The key legend's first entry, and the only one that keeps working while a query holds the
/// keyboard. Named so the footer's "dim everything else" split cannot drift from the text it splits.
pub(crate) const QUIT_HINT: &str = "q quit";

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

/// The column the query editor's caret occupies within the rendered query line: the display width of
/// everything before it, with each newline counted as the separator width it renders as (queries are
/// stored newline-joined but painted on one line).
fn caret_column(app: &App, g: &Glyphs) -> u16 {
    let separator = format!(" {} ", g.vline);
    let before = app.query_before_caret().replace('\n', &separator);
    before.width().min(u16::MAX as usize) as u16
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
        || editing_query_window(app)
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
        app.mode == Mode::TimeRange || editing_query_window(app) || focus == Focus::TimeRange
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

    // List views: query pane (only on the screens that take one) + main + status, within the
    // content area.
    let has_query = app.has_query();
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
        let editing = app.mode == Mode::Editing;
        // The block caret marks the edit point (the terminal's own cursor stays hidden).
        let spans = if editing {
            highlight_caret(app.screen(), app.active_query(), app.query_caret(), &g)
        } else {
            highlight_query(app.screen(), app.active_query(), &g)
        };
        let query_title = if editing {
            format!(
                "Query (Enter: run {s} Tab: complete {s} Esc: cancel)",
                s = g.sep
            )
        } else {
            "Query (e: edit)".to_owned()
        };
        // A query longer than the box would otherwise be clipped at the right edge, hiding the caret as
        // soon as the text outgrows the pane. While editing, scroll horizontally by the least amount
        // that keeps the caret's column inside the box; at rest the line stays pinned to its start.
        let inner_width = query_area.width.saturating_sub(2);
        let offset = if editing {
            caret_column(app, &g).saturating_sub(inner_width.saturating_sub(1))
        } else {
            0
        };
        frame.render_widget(
            Paragraph::new(Line::from(spans)).scroll((0, offset)).block(
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
        // A `Pane`-style detail is a peer, not a preview strip. The primary above it is a short fixed
        // block (the Overview's gauges), so it is sized to its own content and the pane takes
        // everything else — a 55/45 split would waste half the screen on ten lines and crop the list
        // that actually needs the room. Both are floored so a small terminal still shows some of each.
        Some(detail) if detail.style == DetailStyle::Pane => {
            let content = app.snapshot.lines.len().saturating_add(2) as u16;
            let primary_rows = content.clamp(3, list_area.height.saturating_sub(4).max(3));
            let parts = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(primary_rows), Constraint::Min(3)])
                .split(list_area);
            (parts[0], Some((parts[1], detail)))
        }
        Some(detail) => {
            let parts = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
                .split(list_area);
            (parts[0], Some((parts[1], detail)))
        }
        None => (list_area, None),
    };
    // The scroll belongs to whichever pane holds the long content: a `Pane` detail, else the primary.
    let detail_scrolls = matches!(detail, Some((_, pane)) if pane.style == DetailStyle::Pane);

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
        let scroll = if detail_scrolls {
            // The pane below owns the scroll; this one is sized to its content and never needs it.
            0
        } else {
            app.max_scroll.set(max_scroll);
            app.scroll.min(max_scroll)
        };
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

    if let Some((detail_area, detail)) = detail
        && detail.style == DetailStyle::Pane
    {
        // A pane in its own right: bordered like the primary, focusable, and the one that scrolls.
        // Its rect is published so the range form can be anchored over it rather than under the
        // header's time indicator, which belongs to the *query* window.
        // The pane holds two focus stops; either lights its border, and each lights its own half.
        let range_focused = focus == Focus::AttrRange;
        let table_focused = matches!(focus, Focus::AttrTable(_));
        let focused = range_focused || table_focused;
        // The prose sits above the table and does **not** scroll: it qualifies everything below it,
        // and a caveat that scrolls out of a long table is a caveat nobody reads.
        let notes_rows = detail.lines.len().min(6) as u16;
        let body = Rect {
            x: detail_area.x.saturating_add(1),
            y: detail_area.y.saturating_add(1),
            width: detail_area.width.saturating_sub(2),
            height: detail_area.height.saturating_sub(2),
        };
        let parts = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(notes_rows), Constraint::Min(1)])
            .split(body);
        // Every row is one list item, headers included — there is no sticky header to subtract.
        let table_lines = detail.table.as_ref().map(attr_pane_lines);
        let table_rows = table_lines.as_ref().map_or(0, Vec::len) as u16;
        let viewport = parts[1].height;
        app.page_rows.set(viewport.max(1));
        let max_scroll = table_rows.saturating_sub(viewport);
        app.max_scroll.set(max_scroll);
        let scroll = app.scroll.min(max_scroll);

        let mut title = detail.title.clone();
        if max_scroll > 0 {
            title = format!("{title}  [{scroll}/{max_scroll} {}]", g.scroll());
        }
        // Each stop advertises only its own action, and a section stop names the section it is on —
        // Tab moves between them, so the title is what says where the ring has landed.
        if range_focused {
            title = format!("{title}  {} enter: change the range", g.sep);
        } else if let Focus::AttrTable(section) = focus
            && let Some(name) = app.attr_section_label(section)
        {
            title = format!("{title}  {} {name}", g.sep);
        }
        if table_focused && app.can_promote {
            // `p` is offered only where it can work. A local session opened the database read-only,
            // so there is no promotion to advertise — see `Backend::can_promote`.
            title = format!("{title}  {} p: promote/demote", g.sep);
        }
        frame.render_widget(
            g.block().border_style(focus_border(focused)).title(title),
            detail_area,
        );
        // The range form is anchored to the *range line* — the pane's first note, which is where the
        // window it edits is displayed — so the form drops from the value it changes rather than from
        // the pane's corner or, worse, from the header's query-range indicator.
        app.attr_area.set(Rect {
            height: 1,
            ..parts[0]
        });
        // The range line is a focus stop in its own right, so it has to *look* like one when the ring
        // is on it — otherwise Enter would act on something the screen never marked as selected.
        let mut notes: Vec<Line> = detail
            .lines
            .iter()
            .map(|line| {
                Line::from(Span::styled(
                    line.clone(),
                    Style::default().fg(Color::DarkGray),
                ))
            })
            .collect();
        if range_focused && let Some(first) = notes.first_mut() {
            *first = Line::from(Span::styled(
                detail.lines[0].clone(),
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ));
        }
        frame.render_widget(Paragraph::new(notes).wrap(Wrap { trim: false }), parts[0]);
        // Styled *lines* rather than a `Table` widget, for one reason a cell-based table cannot give:
        // a section title has to span the pane. In a `Table` it sits in column 0 and is clipped to the
        // key column's width. Columns are padded here instead — same alignment, same header styling,
        // and titles at full width.
        if let Some(lines) = table_lines {
            // Selectable only while focused, and only because there is something to do with the
            // selection: `p` promotes the key under it. An unfocused pane shows no cursor rather than
            // a highlight that acts on nothing.
            let selection = table_focused
                .then(|| app.selectable_bounds())
                .flatten()
                .map(|(first, last)| app.selected.clamp(first, last));
            let mut state = ListState::default().with_offset(scroll as usize);
            state.select(selection);
            frame.render_stateful_widget(
                List::new(lines).highlight_style(
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                parts[1],
                &mut state,
            );
        }
    } else if let Some((detail_area, detail)) = detail {
        // The bare title line costs the pane's first row; the rest is where waterfall rows land.
        let visible = detail_area.height.saturating_sub(1) as usize;
        let mut title = detail.title.clone();
        let is_waterfall = detail.waterfall.is_some();
        let detail_text = if let Some(waterfall) = &detail.waterfall {
            // The pane has no side borders, so the full width is usable text. Give the bar every cell
            // left after the fixed prefix (marker + name), the two `|`, and the trailing duration
            // column, so the bars stretch to fill the pane instead of a fixed 40 cells.
            let bar_cells = (detail_area.width as usize)
                .saturating_sub(1 + WATERFALL_NAME_W + 2 + WATERFALL_SUFFIX_W)
                .max(1);
            // This preview pane is a fixed slice of the results area and does not scroll, so only
            // the first `visible` rows can ever be seen — rendering the rest was work thrown away on
            // every frame, and a deep trace has thousands of them. The total still comes from the
            // row count, which needs no rendering.
            let total = waterfall.rows.len();
            let rows = render_waterfall_window(waterfall, 0, visible, bar_cells);
            // Say so when a deep trace overflows the pane, so the hidden spans are never silently
            // dropped. The full, scrolling waterfall is one Enter away (`Route::TraceDetail`).
            if total > visible {
                title = format!(
                    "Waterfall: {} of {} spans {} enter: all",
                    visible, total, g.dash
                );
            }
            rows.join("\n")
        } else if detail.lines.is_empty() {
            "No data".to_owned()
        } else {
            detail.lines.join("\n")
        };
        // Waterfall rows are already fitted to the pane width, so wrapping cannot change what is
        // drawn — it would only re-measure every line, and a wrapped bar row would in fact spill
        // onto a second line and push the rest of the waterfall off its axis. Free text still wraps.
        let mut paragraph = Paragraph::new(detail_text)
            // No border box on the waterfall pane: a bare title line keeps the trace id visible
            // while freeing the left/right/bottom edge cells so the bars sit flush against them.
            .block(Block::default().title(title));
        if !is_waterfall {
            paragraph = paragraph.wrap(Wrap { trim: false });
        }
        frame.render_widget(paragraph, detail_area);
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
            // The attribute pane's actions are reached through the pane (tab to it), so what the
            // legend has to say is that the pane is a focus stop at all.
            Screen::Overview => format!(" {sep} tab attributes pane"),
            Screen::Logs => format!(" {sep} enter detail"),
            Screen::Traces => format!(" {sep} enter trace detail {sep} L logs"),
            Screen::Metrics if app.active_query().trim().is_empty() => {
                format!(" {sep} space expand/select series {sep} enter visualize")
            }
            Screen::Metrics => format!(
                " {sep} enter series detail {sep} bksp catalog {sep} {}/esc back",
                g.left
            ),
        };
        // The mascot toggle is only advertised on terminals that can render it.
        let mascot_hint = if options.ascii {
            String::new()
        } else {
            format!(" {sep} m mascot")
        };
        format!(
            "{QUIT_HINT} {sep} F9 menu {sep} 1-4 screen {sep} tab focus {sep} {l}{r} back/fwd {sep} {scroll} move{detail_hint} {sep} r refresh {sep} R auto-refresh {sep} t range {sep} e edit{mascot_hint}{ascii}",
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
    // While the keyboard is held, `q` is the only legend entry that still does anything — so every
    // other one is greyed out. This is the whole signal that input is locked: the banner deliberately
    // says nothing about the keyboard (it would only repeat this line), and dimming states which key
    // survives without adding a word anywhere. Applied only to the key legend, since the error and
    // menu variants of this line do not begin with the quit hint and have no `q` to keep lit.
    let status = match status.strip_prefix(QUIT_HINT) {
        Some(rest) if app.input_locked => Paragraph::new(Line::from(vec![
            Span::raw(QUIT_HINT),
            Span::styled(rest.to_owned(), Style::default().fg(Color::DarkGray)),
        ])),
        _ => Paragraph::new(status),
    };
    frame.render_widget(status.wrap(Wrap { trim: true }), status_area);

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
/// One styled line per row of an attribute pane's table.
///
/// Columns are padded by hand rather than handed to a `Table` widget, because a section title must
/// span the pane: a table cell is clipped to its column, and the title lives in the first one. The
/// header styling matches the other result panes, so this still reads as the same kind of thing.
fn attr_pane_lines(table: &PaneTable) -> Vec<Line<'static>> {
    // Widths over the **key rows only**: a title is a banner whose one long cell would otherwise
    // stretch the key column to its length and squeeze every number out of the pane.
    let widths = column_widths(
        &table.data.header,
        table
            .data
            .rows
            .iter()
            .enumerate()
            .filter(|(index, _)| table.key_at(*index).is_some())
            .map(|(_, row)| row),
    );
    let columns = |row: &[String]| {
        let last = row.len().saturating_sub(1);
        row.iter()
            .enumerate()
            .map(|(index, cell)| {
                if index == last {
                    cell.clone()
                } else {
                    format!(
                        "{}  ",
                        pad_cell(cell, widths.get(index).copied().unwrap_or(0))
                    )
                }
            })
            .collect::<String>()
    };
    table
        .data
        .rows
        .iter()
        .zip(&table.kinds)
        .map(|(row, kind)| match kind {
            // Full width, unclipped: this names the unit every row below it is measured against.
            // Bold, not coloured: the pane already spends colour on the header row and the cursor,
            // and a title is structure rather than a category.
            AttrRow::Section => Line::from(Span::styled(
                row[0].clone(),
                Style::default().add_modifier(Modifier::BOLD),
            )),
            AttrRow::Header => Line::from(Span::styled(
                columns(row),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
            )),
            AttrRow::Key(_) => Line::from(columns(row)),
            AttrRow::Blank => Line::default(),
        })
        .collect()
}

/// Whether the open range form is editing the **query** window — the one this menu bar's indicator
/// shows.
///
/// The same form also edits the Overview attribute pane's window, and lighting up the header for that
/// would say the query range is about to change when it is not. The highlight follows what is being
/// edited, not merely that a form is open.
fn editing_query_window(app: &App) -> bool {
    app.mode == Mode::AbsoluteRange && app.abs_target == AbsTarget::Query
}

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
    // Below the pickers: those are only open because the user opened them, and a banner covering the
    // form they are typing into would be exactly the interruption the banner exists to avoid.
    if let Some(elapsed) = app.loading_banner() {
        draw_loading_banner(frame, elapsed, area, &g);
    }
    if app.mode == Mode::TimeRange {
        draw_time_range_picker(frame, app, indicator_area, area, &g);
    }
    if app.mode == Mode::AbsoluteRange {
        // The form drops from whatever it edits: the header's time indicator for the query window,
        // the attribute pane for the attribute window. Same form, and the anchor is what says which
        // window is about to change.
        let anchor = match app.abs_target {
            AbsTarget::Query => indicator_area,
            AbsTarget::Attributes => app.attr_area.get(),
        };
        draw_absolute_range(frame, app, anchor, area, &g);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;

    use crate::completion::{Candidate, CandidateKind, Completion};
    use crate::mascot::MascotCtx;
    use std::time::Instant;

    use crate::model::{
        DetailPane, LOADING_BANNER_AFTER, LogRecord, MetricDetail, Refresh, Snapshot,
    };
    use crate::testutil::{ascii_trace, nested_trace};
    use crate::waterfall::build_trace_detail;

    /// A section title spans the pane; only the key columns are padded to a width.
    ///
    /// The regression this pins is specific: rendered as a `Table`, the title lives in column 0 and is
    /// clipped to the key column — so a title reading `metrics_gauge - 1 segment, 488 rows, 2 keys`
    /// came out as `metrics_gauge - 1 s>`. Lines have no columns to be clipped to.
    #[test]
    fn a_section_title_is_not_clipped_to_the_key_column() {
        use crate::model::{AttrRow, PaneTable, TableData};

        let table = PaneTable {
            data: TableData::new(
                vec!["Key".to_owned(), "Rows".to_owned()],
                vec![
                    vec![
                        "metrics_gauge - 1 segment, 488 rows, 2 keys".to_owned(),
                        String::new(),
                    ],
                    vec!["Key".to_owned(), "Rows".to_owned()],
                    vec!["env".to_owned(), "488".to_owned()],
                ],
            ),
            kinds: vec![
                AttrRow::Section,
                AttrRow::Header,
                AttrRow::Key("env".to_owned()),
            ],
        };
        let lines = attr_pane_lines(&table);
        let text = |line: &Line| {
            line.spans
                .iter()
                .map(|span| span.content.to_string())
                .collect::<String>()
        };
        assert_eq!(
            text(&lines[0]),
            "metrics_gauge - 1 segment, 488 rows, 2 keys",
            "the title is whole, not cut to the width of the `Key` column"
        );
        // The key column is sized by the *key rows*, so a long title cannot stretch it either.
        assert!(
            text(&lines[2]).starts_with("env  "),
            "columns are padded to the data, not to the banner: {:?}",
            text(&lines[2])
        );
    }

    /// The header's time indicator shows the **query** window, so it must not light up for a form
    /// that edits something else. Both are `Mode::AbsoluteRange`; only the target tells them apart.
    #[test]
    fn the_header_highlights_only_for_the_window_it_shows() {
        let mut app = App::new();
        app.mode = Mode::AbsoluteRange;
        app.abs_target = AbsTarget::Query;
        assert!(editing_query_window(&app));
        app.abs_target = AbsTarget::Attributes;
        assert!(
            !editing_query_window(&app),
            "the attribute pane's range does not change the query window, so the header must stay put"
        );
        app.mode = Mode::Normal;
        assert!(!editing_query_window(&app));
    }

    #[test]
    fn a_held_keyboard_greys_out_every_footer_hint_but_quit() {
        use ratatui::backend::TestBackend;

        // The dimming is the *only* thing on screen that says input is locked — the banner says
        // nothing about the keyboard — so it is asserted on the rendered cell style, not on text.
        let footer_styles = |app: &App| {
            let mut terminal = Terminal::new(TestBackend::new(120, 20)).expect("a test terminal");
            terminal
                .draw(|frame| draw(frame, app, &Options::default()))
                .expect("a draw");
            let buffer = terminal.backend().buffer().clone();
            // The legend starts on the first row below the panels; find it by its first entry.
            let row = (0..20u16)
                .find(|y| {
                    (0..120)
                        .map(|x| buffer[(x, *y)].symbol())
                        .collect::<String>()
                        .trim_start()
                        .starts_with(QUIT_HINT)
                })
                .expect("the key legend is on screen");
            (0..120)
                .map(|x| {
                    let cell = &buffer[(x, row)];
                    (cell.symbol().to_owned(), cell.style().fg)
                })
                .collect::<Vec<_>>()
        };

        // Idle: nothing in the legend is greyed.
        let idle = footer_styles(&App::new());
        assert!(
            !idle.iter().any(|(_, fg)| *fg == Some(Color::DarkGray)),
            "the idle legend should not be dimmed"
        );

        let mut app = App::new();
        app.begin_loading(Refresh::Interactive);
        let locked = footer_styles(&app);
        // `q quit` keeps its normal colour...
        let quit: String = locked
            .iter()
            .take_while(|(symbol, _)| *symbol != " ")
            .map(|(symbol, _)| symbol.as_str())
            .collect();
        assert_eq!(quit, "q");
        assert!(
            locked
                .iter()
                .take(QUIT_HINT.len())
                .all(|(_, fg)| *fg != Some(Color::DarkGray)),
            "the quit hint stayed lit"
        );
        // ...and the rest of the line is greyed.
        let dimmed = locked
            .iter()
            .skip(QUIT_HINT.len())
            .filter(|(symbol, _)| *symbol != " ")
            .collect::<Vec<_>>();
        assert!(!dimmed.is_empty(), "there are other hints to dim");
        assert!(
            dimmed.iter().all(|(_, fg)| *fg == Some(Color::DarkGray)),
            "every other hint is greyed"
        );

        // A background load holds nothing, so the legend stays fully lit.
        let mut app = App::new();
        app.begin_loading(Refresh::Background);
        let free = footer_styles(&app);
        assert!(!free.iter().any(|(_, fg)| *fg == Some(Color::DarkGray)));
    }

    #[test]
    fn a_long_wait_puts_the_banner_in_the_middle_of_the_screen() {
        use ratatui::backend::TestBackend;

        let mut app = App::new();
        app.begin_loading(Refresh::Interactive);
        // Nothing yet: the query has only just been issued.
        let render = |app: &App| {
            let mut terminal = Terminal::new(TestBackend::new(90, 20)).expect("a test terminal");
            terminal
                .draw(|frame| draw(frame, app, &Options::default()))
                .expect("a draw");
            terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .map(|cell| cell.symbol())
                .collect::<String>()
        };
        assert!(
            !render(&app).contains("Loading\u{2026}"),
            "no banner before the delay"
        );

        // Past the delay it is there, centred: in a 20-row terminal the 3-row box occupies rows
        // 8..11, so its text lands on row 9. Centred rather than tucked into the chrome because it
        // is the only thing saying why the keys stopped working.
        app.loading_since = Some(Instant::now() - LOADING_BANNER_AFTER);
        let mut terminal = Terminal::new(TestBackend::new(90, 20)).expect("a test terminal");
        terminal
            .draw(|frame| draw(frame, &app, &Options::default()))
            .expect("a draw");
        let buffer = terminal.backend().buffer().clone();
        let row = |y: u16| (0..90).map(|x| buffer[(x, y)].symbol()).collect::<String>();
        assert!(row(9).contains("Loading\u{2026}"), "row 9: {:?}", row(9));
        // And horizontally centred: past the pane's own border column, the blank runs either side
        // of the box match. Measured in cells rather than by trimming the row string, which would
        // stop at that border and pass for any placement at all.
        let cells: Vec<&str> = (0..90).map(|x| buffer[(x, 9)].symbol()).collect();
        let left = cells[1..].iter().take_while(|cell| **cell == " ").count();
        let right = cells[..89]
            .iter()
            .rev()
            .take_while(|cell| **cell == " ")
            .count();
        assert!(left.abs_diff(right) <= 1, "left {left}, right {right}");
        assert!(left > 0, "the box is not flush against the pane border");

        // A background load renders exactly the same banner: the text never mentioned the lock,
        // so there is nothing here that could contradict an unlocked keyboard.
        let locked = row(9);
        app.input_locked = false;
        let mut terminal = Terminal::new(TestBackend::new(90, 20)).expect("a test terminal");
        terminal
            .draw(|frame| draw(frame, &app, &Options::default()))
            .expect("a draw");
        let buffer = terminal.backend().buffer().clone();
        let free = (0..90).map(|x| buffer[(x, 9)].symbol()).collect::<String>();
        assert_eq!(locked, free);
    }

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
            ("loading banner", {
                // A wait long enough to have raised the banner, with the keyboard held — the state
                // that renders the most banner text (spinner, elapsed, and the paused-input hint).
                let mut app = App::new();
                app.begin_loading(Refresh::Interactive);
                app.loading_since = Some(Instant::now() - LOADING_BANNER_AFTER);
                app.input_refused = true;
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
                app.mode = Mode::Editing;
                app.set_active_query("rate("); // caret at the end, as `begin_editing` leaves it
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
                        table: None,
                        style: DetailStyle::Preview,
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
    fn a_query_longer_than_its_box_scrolls_to_keep_the_caret_visible() {
        use ratatui::backend::TestBackend;

        // 48 columns: the query box's inner width is 46, so this query is comfortably wider than it.
        let query = "http_requests_total{service=\"checkout\",host=\"node-a\",method=\"POST\"}";
        assert!(query.len() > 46);
        let options = Options::default();
        // The row the query box's text lands on: the menu bar, then the box's top border.
        let query_row = 2;
        let text_at = |app: &App| {
            let mut terminal = Terminal::new(TestBackend::new(48, 12)).unwrap();
            terminal.draw(|frame| draw(frame, app, &options)).unwrap();
            let buffer = terminal.backend().buffer().clone();
            let row: String = (0..buffer.area.width)
                .map(|x| buffer[(x, query_row)].symbol().to_owned())
                .collect();
            // The caret is the reversed cell, reported as its column within the row.
            let caret = (0..buffer.area.width).find(|x| {
                buffer[(*x, query_row)]
                    .style()
                    .add_modifier
                    .contains(Modifier::REVERSED)
            });
            (row, caret)
        };

        let mut app = App::new();
        app.route = Route::Metrics;
        app.mode = Mode::Editing;
        app.set_active_query(query); // caret at the end, as `begin_editing` leaves it

        // Caret at the end: the box shows the tail of the query with the caret on the last column
        // inside the border, instead of clipping the query and losing the caret entirely.
        let (row, caret) = text_at(&app);
        assert!(row.contains("method=\"POST\"}"), "query row: {row:?}");
        assert!(!row.contains("http_requests_total"), "query row: {row:?}");
        assert_eq!(
            caret,
            Some(46),
            "the caret sits just inside the right border"
        );

        // Walking the caret back to the start scrolls the window back with it.
        app.query_field().home();
        let (row, caret) = text_at(&app);
        assert!(row.contains("http_requests_total"), "query row: {row:?}");
        assert_eq!(caret, Some(1), "the caret sits just inside the left border");

        // A query that fits is never scrolled.
        app.set_active_query("up");
        let (row, caret) = text_at(&app);
        assert!(row.starts_with("│up"), "query row: {row:?}");
        assert_eq!(caret, Some(3));
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

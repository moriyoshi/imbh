//! The Logs panes: the log-entry detail view.

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};

use crate::app::App;
use crate::detail_text::log_detail_lines;
use crate::format::wrapped_rows;
use crate::model::LogRecord;
use crate::ui::focus_border;
use crate::ui::glyphs::Glyphs;

/// Render the log-entry detail view (a title bar, the scrollable record body, and a hint bar) into the
/// content area beneath the menu bar. Publishes the scroll bounds (via `App`'s cells) so the key
/// handler can clamp `↑/↓/PageDown`.
pub(crate) fn draw_log_detail(
    frame: &mut ratatui::Frame<'_>,
    app: &App,
    record: &LogRecord,
    area: Rect,
    focused: bool,
    g: &Glyphs,
) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(3),
            Constraint::Length(2),
        ])
        .split(area);

    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            " Log entry detail ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )))
        .block(g.block().title("Logs")),
        rows[0],
    );

    let lines = log_detail_lines(record);
    let body_area = rows[1];
    let inner_width = body_area.width.saturating_sub(2);
    let viewport = body_area.height.saturating_sub(2);
    let total_rows: u16 = lines
        .iter()
        .map(|line| wrapped_rows(line, inner_width))
        .sum::<u32>()
        .min(u16::MAX as u32) as u16;
    let max_scroll = total_rows.saturating_sub(viewport);
    app.max_scroll.set(max_scroll);
    app.page_rows.set(viewport.max(1));
    let scroll = app.scroll.min(max_scroll);
    let title = if max_scroll > 0 {
        format!("Detail  [{scroll}/{max_scroll} {}]", g.scroll())
    } else {
        "Detail".to_owned()
    };
    frame.render_widget(
        Paragraph::new(lines.join("\n"))
            .wrap(Wrap { trim: false })
            .scroll((scroll, 0))
            .block(g.block().border_style(focus_border(focused)).title(title)),
        body_area,
    );

    let (sep, left, right, scroll_hint) = (g.sep, g.left, g.right, g.scroll());
    let hint = if record.trace_id.is_some() {
        format!(
            "esc/{left} back {sep} bksp logs list {sep} enter open trace {sep} {right} fwd {sep} \
             {scroll_hint} scroll"
        )
    } else {
        format!(
            "esc/{left} back {sep} bksp logs list {sep} (no trace id) {sep} {scroll_hint} scroll"
        )
    };
    frame.render_widget(Paragraph::new(hint).wrap(Wrap { trim: true }), rows[2]);
}

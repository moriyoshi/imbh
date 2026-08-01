//! The Traces panes: the full-screen trace detail and the span field detail.

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, ListState, Paragraph, Wrap};

use crate::app::App;
use crate::detail_text::{span_detail_lines, span_summary_lines};
use crate::format::wrapped_rows;
use crate::time::{format_duration_ns, format_timestamp_ns};
use crate::ui::focus_border;
use crate::ui::glyphs::Glyphs;
use crate::waterfall::{
    SpanRecord, TraceDetail, WATERFALL_NAME_W, WATERFALL_SUFFIX_W, render_waterfall,
};

/// Height (rows, borders included) the selected-span summary pane claims on the trace detail. Below
/// [`TRACE_DETAIL_SUMMARY_MIN_ROWS`] of content the pane is dropped entirely so the waterfall keeps
/// enough rows to be useful — the same fields are one Enter away in the span detail.
pub(crate) const TRACE_SPAN_SUMMARY_H: u16 = 7;

/// Content height at/above which the trace detail shows the span summary pane (header 3 + summary 7 +
/// hint 2 leaves 6 waterfall rows).
pub(crate) const TRACE_DETAIL_SUMMARY_MIN_ROWS: u16 = 18;

/// Render the full-screen trace detail into the content area beneath the menu bar: a trace header, the
/// whole span waterfall as a scrolling span-selectable list (so a deep trace is fully reachable, unlike
/// the fixed half-height preview pane on the Traces list), and — when the terminal is tall enough — a
/// summary of the span under the cursor. Enter opens that span's full fields.
pub(crate) fn draw_trace_detail(
    frame: &mut ratatui::Frame<'_>,
    app: &App,
    detail: &TraceDetail,
    area: Rect,
    focused: bool,
    g: &Glyphs,
) {
    let with_summary = area.height >= TRACE_DETAIL_SUMMARY_MIN_ROWS && !detail.spans.is_empty();
    let constraints = if with_summary {
        vec![
            Constraint::Length(3),
            Constraint::Min(3),
            Constraint::Length(TRACE_SPAN_SUMMARY_H),
            Constraint::Length(2),
        ]
    } else {
        vec![
            Constraint::Length(3),
            Constraint::Min(3),
            Constraint::Length(2),
        ]
    };
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(area);
    let hint_area = *rows.last().expect("hint row");

    // Header: the trace id and its shape. The root service/operation ride the block title so the id
    // line stays short enough to fit an 80-column terminal.
    let root = match (&detail.root_service, &detail.root_name) {
        (Some(service), Some(name)) => format!("{service} {} {name}", g.sep),
        (Some(service), None) => service.clone(),
        (None, Some(name)) => name.clone(),
        (None, None) => "(no root span)".to_owned(),
    };
    let header = format!(
        "{}  {sep} {} spans  {sep} {}  {sep} {}",
        detail.trace_id,
        detail.spans.len(),
        format_duration_ns(detail.duration_ns, g.ascii),
        format_timestamp_ns(detail.start_time_ns),
        sep = g.sep,
    );
    frame.render_widget(
        Paragraph::new(header)
            .wrap(Wrap { trim: true })
            .block(g.block().title(format!("Trace {} {root}", g.sep))),
        rows[0],
    );

    // The waterfall as a List: the cursor selects a span and the widget scrolls to keep it in view, so
    // the pane is navigable however many spans the trace has. Bars fill the width left after the
    // fixed-width prefix, the two `|`, and the trailing duration column.
    let list_area = rows[1];
    let viewport = list_area.height.saturating_sub(2);
    app.page_rows.set(viewport.max(1));
    let bar_cells = (list_area.width as usize)
        .saturating_sub(2 + 1 + WATERFALL_NAME_W + 2 + WATERFALL_SUFFIX_W)
        .max(1);
    let cursor = app.span_cursor.min(detail.spans.len().saturating_sub(1));
    let items = if detail.spans.is_empty() {
        // A trace with no spans is a real (if degenerate) result, not a blank pane.
        vec![ListItem::new(Span::styled(
            "(no spans)",
            Style::default().fg(Color::DarkGray),
        ))]
    } else {
        render_waterfall(&detail.waterfall, bar_cells)
            .into_iter()
            .enumerate()
            .map(|(index, line)| {
                // Non-OK spans read red so the interesting rows stand out in a long waterfall.
                let style = if detail.spans.get(index).is_some_and(SpanRecord::is_error) {
                    Style::default().fg(Color::Red)
                } else {
                    Style::default()
                };
                ListItem::new(Span::styled(line, style))
            })
            .collect::<Vec<_>>()
    };
    let title = if detail.spans.is_empty() {
        "Spans".to_owned()
    } else {
        format!(
            "Spans  [{}/{} {}]",
            cursor + 1,
            detail.spans.len(),
            g.scroll()
        )
    };
    let mut state = ListState::default();
    state.select((!detail.spans.is_empty()).then_some(cursor));
    frame.render_stateful_widget(
        List::new(items)
            .block(g.block().border_style(focus_border(focused)).title(title))
            .highlight_style(
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
        list_area,
        &mut state,
    );

    if with_summary && let Some(span) = detail.spans.get(cursor) {
        frame.render_widget(
            Paragraph::new(span_summary_lines(span, g).join("\n"))
                .wrap(Wrap { trim: false })
                .block(g.block().title(format!("Span {} enter: fields", g.sep))),
            rows[2],
        );
    }

    let (sep, left, right, scroll_hint) = (g.sep, g.left, g.right, g.scroll());
    frame.render_widget(
        Paragraph::new(format!(
            "esc/{left} back {sep} {scroll_hint} span {sep} enter span fields {sep} L logs for span \
             {sep} {right} fwd"
        ))
        .wrap(Wrap { trim: true }),
        hint_area,
    );
}

/// Render the full field detail of one span (the `Route::SpanDetail` content): a scrollable dump of
/// every stored field, mirroring the log detail's shape.
pub(crate) fn draw_span_detail(
    frame: &mut ratatui::Frame<'_>,
    app: &App,
    trace_id: &str,
    span: &SpanRecord,
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
            format!(" {} ", span.name),
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )))
        .block(g.block().title("Span detail")),
        rows[0],
    );

    let lines = span_detail_lines(trace_id, span, g);
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
        format!("Fields  [{scroll}/{max_scroll} {}]", g.scroll())
    } else {
        "Fields".to_owned()
    };
    frame.render_widget(
        Paragraph::new(lines.join("\n"))
            .wrap(Wrap { trim: false })
            .scroll((scroll, 0))
            .block(g.block().border_style(focus_border(focused)).title(title)),
        body_area,
    );

    let (sep, left, right, scroll_hint) = (g.sep, g.left, g.right, g.scroll());
    frame.render_widget(
        Paragraph::new(format!(
            "esc/{left} back {sep} L logs for this span {sep} {scroll_hint} scroll {sep} {right} fwd"
        ))
        .wrap(Wrap { trim: true }),
        rows[2],
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use imbh::{Timestamp, TraceId};
    use ratatui::Terminal;

    use crate::model::{Options, Route};
    use crate::testutil::waterfall_span;
    use crate::ui::draw;
    use crate::waterfall::build_trace_detail;

    #[test]
    fn trace_detail_waterfall_scrolls_to_keep_the_span_cursor_visible() {
        use ratatui::backend::TestBackend;

        // A trace with far more spans than fit the pane: the row the cursor is on must be on screen,
        // which the fixed preview pane on the Traces list cannot do.
        let spans = (0..40u8)
            .map(|i| waterfall_span(i + 1, None, &format!("span-{i}"), i as i64 * 1_000, 1_000))
            .collect::<Vec<_>>();
        let trace = imbh::Trace {
            trace_id: TraceId([0xaa; 16]),
            root_service: Some("api".to_owned()),
            root_name: Some("root".to_owned()),
            start_time: Timestamp(0),
            duration_ns: imbh::DurationNs(40_000),
            spans,
        };
        let mut app = App::new();
        app.route = Route::TraceDetail {
            detail: build_trace_detail(&trace, true),
        };
        let options = Options {
            ascii: true,
            ..Options::default()
        };

        let rendered = |app: &App| -> String {
            let mut terminal = Terminal::new(TestBackend::new(80, 14)).unwrap();
            terminal.draw(|frame| draw(frame, app, &options)).unwrap();
            let buffer = terminal.backend().buffer().clone();
            (0..buffer.area.height)
                .map(|y| {
                    (0..buffer.area.width)
                        .map(|x| buffer[(x, y)].symbol().to_owned())
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
                .join("\n")
        };

        // At the top the first span shows and the last is far below the fold.
        let top = rendered(&app);
        assert!(top.contains("span-0"), "{top}");
        assert!(!top.contains("span-39"), "{top}");

        // With the cursor on the last span the list has scrolled it into view.
        app.span_cursor = 39;
        let bottom = rendered(&app);
        assert!(bottom.contains("span-39"), "{bottom}");
        assert!(bottom.contains("[40/40"), "row counter: {bottom}");
    }
}

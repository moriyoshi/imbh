//! The Traces panes: the full-screen trace detail and the span field detail.

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, ListState, Paragraph, Wrap};
use unicode_width::UnicodeWidthStr;

use crate::app::App;
use crate::detail_text::{span_detail_lines, span_summary_lines};
use crate::format::wrapped_rows;
use crate::time::{format_duration_ns, format_timestamp_ns};
use crate::ui::focus_border;
use crate::ui::glyphs::Glyphs;
use crate::waterfall::{
    SpanRecord, TraceDetail, WATERFALL_NAME_W, WATERFALL_SUFFIX_W, WaterfallView, name_offset,
    render_waterfall, render_waterfall_row, sticky_layout, visible_indent_base,
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

    // The waterfall as a List: the cursor selects a span and the pane scrolls to keep it in view, so
    // the pane is navigable however many spans the trace has. Bars fill the width left after the
    // marker, the fixed-width name column, the two `|`, and the trailing duration column.
    //
    // The ancestors of the selected span that have scrolled off the top stay pinned above the list
    // ("sticky"), dimmed, so a deep trace never loses the context of the row under the cursor.
    let list_area = rows[1];
    let viewport = list_area.height.saturating_sub(2);
    let bar_cells = (list_area.width as usize)
        .saturating_sub(2 + 1 + WATERFALL_NAME_W + 2 + WATERFALL_SUFFIX_W)
        .max(1);
    let cursor = app.span_cursor.min(detail.spans.len().saturating_sub(1));
    let layout = sticky_layout(
        &detail.waterfall.rows,
        cursor,
        viewport as usize,
        app.sticky_waterfall && !detail.spans.is_empty(),
    );
    // Publish the *scrolling* height, not the raw viewport: PageUp/PageDown should step by rows the
    // user can actually see, or each page silently skips the pinned ones.
    app.page_rows.set((layout.height as u16).max(1));

    // Non-OK spans read red so the interesting rows stand out in a long waterfall.
    let row_style = |index: usize| {
        if detail.spans.get(index).is_some_and(SpanRecord::is_error) {
            Style::default().fg(Color::Red)
        } else {
            Style::default()
        }
    };
    // Indent relative to the shallowest span on screen: scrolled deep into a trace every visible row
    // carries the same leading indent, which buys nothing and costs the name column.
    let indent_base = visible_indent_base(&detail.waterfall.rows, &layout);
    let view = WaterfallView {
        bar_cells,
        name_offset: name_offset(&detail.waterfall.rows, cursor, indent_base),
        indent_base,
    };
    let lines = render_waterfall(&detail.waterfall, &view);
    let items = if detail.spans.is_empty() {
        // A trace with no spans is a real (if degenerate) result, not a blank pane.
        vec![ListItem::new(Span::styled(
            "(no spans)",
            Style::default().fg(Color::DarkGray),
        ))]
    } else {
        lines
            .iter()
            .enumerate()
            .map(|(index, line)| ListItem::new(Span::styled(line.clone(), row_style(index))))
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

    // Render the block ourselves so the pinned rows and the list can share its inner area. Splitting
    // that area arithmetically (rather than through a `Layout`) keeps it exact at every height.
    let block = g.block().border_style(focus_border(focused)).title(title);
    let inner = block.inner(list_area);
    frame.render_widget(block, list_area);
    let pinned_h = layout.pinned.len() as u16;
    if pinned_h > 0 {
        let last_pinned = layout.pinned.last().copied().unwrap_or_default();
        let pinned = layout
            .pinned
            .iter()
            .map(|&index| {
                // De-emphasise through three channels, because `Modifier::DIM` alone does not reach
                // the bar: many terminals draw box-drawing glyphs procedurally and honour only the
                // cell's foreground colour, so a pinned row's name and duration would dim while its
                // bar stayed at full intensity. An explicit colour reaches that renderer, and the
                // lighter bar glyph reads as recessed even where neither is honoured. Error rows keep
                // red — the pinned block is context, but a failing ancestor is still worth seeing.
                let error = detail.spans.get(index).is_some_and(SpanRecord::is_error);
                let mut style = Style::default()
                    .fg(if error { Color::Red } else { Color::DarkGray })
                    .add_modifier(Modifier::DIM);
                // Underline the last pinned row as the divider between the pinned block and the rows
                // that scroll beneath it. An attribute rather than a rule row: a `───` line would cost
                // a viewport row out of a pane whose whole problem is that it is too short, and the
                // underline already sits exactly where the boundary is.
                if index == last_pinned {
                    style = style.add_modifier(Modifier::UNDERLINED);
                }
                let mut text = render_waterfall_row(
                    &detail.waterfall,
                    index,
                    &view,
                    detail.waterfall.light_marker,
                );
                // Pad to the pane's full inner width so the styling covers the whole row. A waterfall
                // line stops after its duration column, several cells short of the right border, and
                // an unpadded row would leave the divider underline stopping short of the edge with
                // it — a rule that does not reach the border does not read as a rule.
                let pad = (inner.width as usize).saturating_sub(UnicodeWidthStr::width(&*text));
                text.extend(std::iter::repeat_n(' ', pad));
                Line::from(Span::styled(text, style))
            })
            .collect::<Vec<_>>();
        // No `Wrap`: a pinned row must clip like a list row, or its bar leaves the axis.
        frame.render_widget(
            Paragraph::new(pinned),
            Rect {
                height: pinned_h,
                ..inner
            },
        );
    }
    let mut state = ListState::default()
        .with_offset(layout.offset)
        .with_selected((!detail.spans.is_empty()).then_some(cursor));
    frame.render_stateful_widget(
        List::new(items).highlight_style(
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Rect {
            y: inner.y + pinned_h,
            height: inner.height.saturating_sub(pinned_h),
            ..inner
        },
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
    let sticky = if app.sticky_waterfall { "on" } else { "off" };
    frame.render_widget(
        Paragraph::new(format!(
            "esc/{left} back {sep} {scroll_hint} span {sep} enter span fields {sep} L logs for span \
             {sep} s sticky:{sticky} {sep} {right} fwd"
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
    use crate::testutil::{nested_trace, waterfall_span};
    use crate::ui::draw;
    use crate::waterfall::build_trace_detail;

    /// Render `app` at `width`x`height` in `--ascii` mode and return the buffer, so a test can assert
    /// on the text *and* on per-cell styling (which the sticky rows' dimming needs).
    fn render_buffer(app: &App, width: u16, height: u16) -> ratatui::buffer::Buffer {
        use ratatui::backend::TestBackend;

        let options = Options {
            ascii: true,
            ..Options::default()
        };
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| draw(frame, app, &options)).unwrap();
        terminal.backend().buffer().clone()
    }

    fn buffer_text(buffer: &ratatui::buffer::Buffer) -> String {
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol().to_owned())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

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

    #[test]
    fn trace_detail_pins_the_selected_spans_ancestors_above_the_waterfall() {
        // A deep trace with the cursor near the bottom: the enclosing spans have scrolled off, so
        // without sticky rows the pane gives no clue what the selected span hangs off.
        let mut app = App::new();
        app.route = Route::TraceDetail {
            detail: build_trace_detail(&nested_trace(), true),
        };
        app.span_cursor = 17;

        let buffer = render_buffer(&app, 80, 24);
        let sticky = buffer_text(&buffer);
        // The two outermost spans are pinned even though they are far above the scrolling window...
        assert!(sticky.contains("zz-root"), "{sticky}");
        assert!(sticky.contains("yy-mid"), "{sticky}");
        // ...the selected span is on screen...
        assert!(sticky.contains("work-15"), "{sticky}");
        // ...and a row from the middle of the trace, which really has scrolled away, is not.
        assert!(!sticky.contains("work-2-"), "{sticky}");

        // The pinned rows are de-emphasised and the scrolling rows are not, so the pinned block reads
        // as context rather than as data. Rows 5 and 6 are the pane's first two inner rows (menu bar 1
        // + header 3 + the pane's top border 1); row 7 is the first scrolling row.
        //
        // Assert across the *whole* row rather than one cell: an earlier version sampled only the name
        // column, and so passed while the bar was still rendering at full intensity.
        // Inside the pane's left/right borders, ignoring blank padding cells.
        fn cells(buffer: &ratatui::buffer::Buffer, y: u16) -> Vec<&ratatui::buffer::Cell> {
            (1..buffer.area.width - 1)
                .map(|x| &buffer[(x, y)])
                .filter(|cell| cell.symbol() != " ")
                .collect()
        }
        let has = |y: u16, m: Modifier| cells(&buffer, y).iter().all(|c| c.modifier.contains(m));
        let none = |y: u16, m: Modifier| !cells(&buffer, y).iter().any(|c| c.modifier.contains(m));
        assert!(has(5, Modifier::DIM), "pinned row 1 must dim:\n{sticky}");
        assert!(has(6, Modifier::DIM), "pinned row 2 must dim:\n{sticky}");
        assert!(none(7, Modifier::DIM), "scrolling rows must not:\n{sticky}");

        // `Modifier::DIM` alone does not reach the bar: many terminals draw box-drawing glyphs
        // procedurally and ignore the faint attribute on them, so a pinned row would show a dim name
        // and duration beside a full-intensity bar. The de-emphasis therefore also rides an explicit
        // foreground colour and a lighter bar glyph, which that renderer does honour.
        assert!(
            cells(&buffer, 5).iter().all(|c| c.fg == Color::DarkGray),
            "pinned rows need an explicit recessed fg:\n{sticky}"
        );
        let row_text = |y: u16| {
            cells(&buffer, y)
                .iter()
                .map(|c| c.symbol().to_owned())
                .collect::<String>()
        };
        assert!(row_text(5).contains('-'), "pinned bar is light:\n{sticky}");
        assert!(!row_text(5).contains('#'), "…and only light:\n{sticky}");
        assert!(
            row_text(7).contains('#'),
            "scrolling bar is heavy:\n{sticky}"
        );

        // The last pinned row is underlined: the divider between the pinned block and the rows
        // scrolling beneath it, drawn without spending a viewport row on a rule.
        assert!(
            has(6, Modifier::UNDERLINED),
            "the last pinned row is the divider:\n{sticky}"
        );
        assert!(
            none(5, Modifier::UNDERLINED),
            "only the last pinned row divides:\n{sticky}"
        );
        assert!(
            none(7, Modifier::UNDERLINED),
            "scrolling rows are not dividers:\n{sticky}"
        );
        // ...and it reaches both borders. A waterfall line stops after its duration column, so without
        // padding the rule stopped short of the right edge and did not read as a rule at all.
        for x in 1..buffer.area.width - 1 {
            assert!(
                buffer[(x, 6)].modifier.contains(Modifier::UNDERLINED),
                "divider breaks at x={x}:\n{sticky}"
            );
        }
        // The pane's own top border (y = 4) must stay clean — the divider belongs between the pinned
        // block and the scrolling rows, not under the title.
        assert!(
            none(4, Modifier::UNDERLINED),
            "the pane title must not be underlined:\n{sticky}"
        );

        // A pinned ancestor is scrolled only as far as it has name to hide, so the context band shows
        // real names even while the scrolling rows below it are shifted well to the right.
        assert!(sticky.contains("| zz-root  "), "{sticky}");

        // The pinned block is the crate's `--ascii` guarantee too. The whole-UI sweep runs at 48x10,
        // too short for sticky to engage, so this is the only place that covers it.
        assert!(
            buffer.content().iter().all(|cell| cell.symbol().is_ascii()),
            "non-ASCII cell in the sticky waterfall:\n{sticky}"
        );

        // Toggled off, the ancestors are gone and the pane scrolls exactly as it did before.
        app.sticky_waterfall = false;
        let plain = buffer_text(&render_buffer(&app, 80, 24));
        assert!(!plain.contains("zz-root"), "{plain}");
        assert!(!plain.contains("yy-mid"), "{plain}");
        assert!(plain.contains("work-15"), "{plain}");
    }

    #[test]
    fn trace_detail_scrolls_the_name_column_to_the_cursors_span_name() {
        // Every name in this fixture is longer than the 20-cell name column, so the column has to
        // scroll to show the selected span's name — with `<` marking what it hid.
        let mut app = App::new();
        app.route = Route::TraceDetail {
            detail: build_trace_detail(&nested_trace(), true),
        };
        app.span_cursor = 17;
        let scrolled = buffer_text(&render_buffer(&app, 80, 24));
        // A waterfall row is a bordered `|...|` line; the `<` in the ASCII hint line ("esc/< back")
        // is not, so counting only bordered rows matches real left-clipped names.
        let clipped = |text: &str| {
            text.lines()
                .filter(|line| line.starts_with('|') && line.contains('<'))
                .count()
        };
        assert!(clipped(&scrolled) > 0, "no clip marker:\n{scrolled}");
        // The cursor row's name is readable right through to its tail.
        assert!(scrolled.contains("with-a-long-name"), "{scrolled}");

        // Back on the root, whose name fits, the column returns to its unscrolled position: names
        // read from their first character and nothing claims to be hidden on the left.
        app.span_cursor = 0;
        let home = buffer_text(&render_buffer(&app, 80, 24));
        assert!(home.contains("| zz-root  "), "{home}");
        // The long leaf names still overflow the column, but from their *start*, marked with `>`.
        assert!(home.contains("work-0-with-a-l>"), "{home}");
        assert_eq!(clipped(&home), 0, "nothing should be clipped left:\n{home}");
    }
}

//! The Metrics panes: the series/catalog table and the detailed time-series viewer.

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols::Marker;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Axis, Chart, Dataset, GraphType, Paragraph, Row, Table, TableState};
use unicode_width::UnicodeWidthStr;

use crate::app::App;
use crate::chart::{ChartGeometry, ascii_chart, chart_graph_area, chart_point_cell, chart_values};
use crate::format::format_metric_value;
use crate::model::{MetricDetail, Options, TableData};
use crate::time::{clock_hms_ns, format_datetime_ns};
use crate::ui::focus_border;
use crate::ui::glyphs::Glyphs;

/// Column widths from the widest cell (header included), each capped so one wide column cannot crowd
/// out the rest; the final column absorbs the remaining space.
///
/// Measured by **display width**, not code-point count, so full-width (CJK) glyphs and other wide
/// characters in names/labels/values size their column to the cells they actually occupy instead of
/// being under-measured and truncated. Consistent with the width-aware header and waterfall.
pub(crate) fn column_widths<'a>(
    header: &[String],
    rows: impl Iterator<Item = &'a Vec<String>>,
) -> Vec<usize> {
    let mut widths = header
        .iter()
        .map(|cell| UnicodeWidthStr::width(cell.as_str()))
        .collect::<Vec<_>>();
    for row in rows {
        for (index, cell) in row.iter().enumerate() {
            if index < widths.len() {
                widths[index] = widths[index].max(UnicodeWidthStr::width(cell.as_str()));
            }
        }
    }
    widths
}

/// [`column_widths`] as ratatui constraints: every column but the last is fixed, and the last absorbs
/// what is left.
pub(crate) fn column_constraints<'a>(
    header: &[String],
    rows: impl Iterator<Item = &'a Vec<String>>,
) -> Vec<Constraint> {
    let column_count = header.len();
    column_widths(header, rows)
        .iter()
        .enumerate()
        .map(|(index, &width)| {
            if index + 1 == column_count {
                Constraint::Min(width.clamp(6, 60) as u16)
            } else {
                Constraint::Length(width.clamp(4, 48) as u16)
            }
        })
        .collect()
}

/// Pad `cell` to `width` display columns, or truncate it to fit with a `>` marking the cut. Display
/// width, not character count, so a CJK glyph occupies the two cells it actually draws in.
pub(crate) fn pad_cell(cell: &str, width: usize) -> String {
    let actual = UnicodeWidthStr::width(cell);
    if actual <= width {
        return format!("{cell}{}", " ".repeat(width - actual));
    }
    let mut out = String::new();
    let mut used = 0usize;
    for character in cell.chars() {
        let next = used + UnicodeWidthStr::width(character.to_string().as_str());
        if next > width.saturating_sub(1) {
            break;
        }
        out.push(character);
        used = next;
    }
    out.push('>');
    format!("{out}{}", " ".repeat(width.saturating_sub(used + 1)))
}

/// The header row style shared by every table pane: the column names, set apart from the data under
/// them.
pub(crate) fn table_header(table: &TableData) -> Row<'static> {
    Row::new(table.header.iter().cloned()).style(
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
    )
}

/// Render the primary pane as a selectable table with a header row and column-aligned cells. The
/// selection cursor (`app.selected`) indexes `table.rows`; `TableState` scrolls to keep it in view.
pub(crate) fn draw_metric_table(
    frame: &mut ratatui::Frame<'_>,
    app: &App,
    table: &TableData,
    area: Rect,
    focused: bool,
    g: &Glyphs,
) {
    let constraints = column_constraints(&table.header, table.rows.iter());
    let header = table_header(table);
    // In the catalog tree the first column carries the branch marker (`v `/`> `). Split that leading
    // marker into a dark-grey span so it reads as chrome rather than content. Only the tree rows carry
    // a marker prefix — checkbox/loading rows and the (non-catalog) series table never match, so they
    // render unchanged.
    let on_catalog = app.on_catalog();
    const BRANCH_MARKERS: [&str; 2] = ["v ", "> "];
    let rows = table
        .rows
        .iter()
        .map(|row| {
            let cells = row
                .iter()
                .enumerate()
                .map(|(col, text)| {
                    if col == 0 && on_catalog {
                        let indent = text.len() - text.trim_start().len();
                        if let Some(marker) = BRANCH_MARKERS
                            .iter()
                            .find(|m| text[indent..].starts_with(**m))
                        {
                            let (head, tail) = text.split_at(indent + marker.len());
                            return Line::from(vec![
                                Span::styled(head.to_owned(), Style::default().fg(Color::DarkGray)),
                                Span::raw(tail.to_owned()),
                            ]);
                        }
                    }
                    Line::from(text.clone())
                })
                .collect::<Vec<_>>();
            Row::new(cells)
        })
        .collect::<Vec<_>>();

    let selection = app
        .selectable_bounds()
        .map(|(first, last)| app.selected.clamp(first, last));
    let title = match selection {
        Some(selected) if !table.rows.is_empty() => {
            format!(
                "{}  [{}/{}]",
                app.snapshot.title,
                selected + 1,
                table.rows.len()
            )
        }
        _ => app.snapshot.title.clone(),
    };
    let mut state = TableState::default();
    state.select(selection);
    frame.render_stateful_widget(
        Table::new(rows, constraints)
            .header(header)
            .column_spacing(2)
            .block(g.block().border_style(focus_border(focused)).title(title))
            // No highlight symbol: the tree rows already begin with `v `/`> ` markers, so a `>` cursor
            // would be ambiguous. The row-highlight style alone marks the selection.
            .row_highlight_style(
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
        area,
        &mut state,
    );
}

/// Render the detailed time-series viewer for one selected metric series into the content area beneath
/// the menu bar: a header, a line chart of the series over the query window (with a movable vertical
/// cursor), and a readout of the point under the cursor plus summary stats. ASCII fallback via
/// `--ascii`.
pub(crate) fn draw_metric_detail(
    frame: &mut ratatui::Frame<'_>,
    app: &App,
    detail: &MetricDetail,
    options: &Options,
    area: Rect,
    focused: bool,
) {
    let g = Glyphs::new(options.ascii);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(5),
            Constraint::Length(4),
        ])
        .split(area);

    // Header: the series labels and the source query.
    let labels = if detail.labels.is_empty() {
        "{}"
    } else {
        detail.labels.as_str()
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                " Series ",
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(format!(" {labels}")),
        ]))
        .block(
            g.block()
                .title(format!("Metric series {} {}", g.dash, detail.query)),
        ),
        rows[0],
    );

    let plot_area = rows[1];
    // Page the x-cursor by roughly a screenful of plot columns.
    app.page_rows.set(plot_area.width.saturating_sub(2).max(1));
    let cursor = app.metric_cursor.min(detail.points.len().saturating_sub(1));

    // Finite samples drive the plot and the stats; a cursor may still land on a gap (NaN) sample.
    let finite: Vec<(f64, f64)> = detail
        .points
        .iter()
        .filter(|(_, value)| value.is_finite())
        .map(|(time_ns, value)| (*time_ns as f64 / 1e9, *value))
        .collect();

    if options.ascii || finite.len() < 2 {
        // ASCII fallback (or too few points for a line): the hand-rolled chart, cursor shown in the
        // readout rather than on the plot.
        let inner_w = plot_area.width.saturating_sub(2) as usize;
        let inner_h = plot_area.height.saturating_sub(2) as usize;
        let body = if finite.is_empty() {
            "no finite samples in this window".to_owned()
        } else {
            ascii_chart(
                &chart_values(finite.iter().map(|(_, value)| *value)),
                inner_w,
                inner_h,
            )
        };
        frame.render_widget(
            Paragraph::new(body)
                .block(g.block().border_style(focus_border(focused)).title("Chart")),
            plot_area,
        );
        // No ratatui line chart here, so nothing for the mascot to ride.
        app.chart_geom.replace(None);
    } else {
        let (x_min, x_max) = (finite.first().unwrap().0, finite.last().unwrap().0);
        let (mut y_min, mut y_max) = finite
            .iter()
            .fold((f64::INFINITY, f64::NEG_INFINITY), |a, p| {
                (a.0.min(p.1), a.1.max(p.1))
            });
        // Pad the y-range (and widen a flat line) so the plot is not glued to the border.
        if (y_max - y_min).abs() < f64::EPSILON {
            y_min -= 1.0;
            y_max += 1.0;
        } else {
            let pad = (y_max - y_min) * 0.05;
            y_min -= pad;
            y_max += pad;
        }
        // A vertical line at the cursor's timestamp, spanning the y-range.
        let cursor_x = detail.points[cursor].0 as f64 / 1e9;
        let cursor_line = [(cursor_x, y_min), (cursor_x, y_max)];
        // Exemplar → trace markers as magenta dots along the plot floor, at each exemplar's timestamp.
        let exemplar_points: Vec<(f64, f64)> = app
            .metric_exemplars
            .iter()
            .map(|marker| (marker.time_ns as f64 / 1e9, y_min))
            .filter(|(x, _)| *x >= x_min && *x <= x_max)
            .collect();
        let mut datasets = vec![
            Dataset::default()
                .marker(Marker::Braille)
                .graph_type(GraphType::Line)
                .style(Style::default().fg(Color::Cyan))
                .data(&finite),
            Dataset::default()
                .marker(Marker::Braille)
                .graph_type(GraphType::Line)
                .style(Style::default().fg(Color::Yellow))
                .data(&cursor_line),
        ];
        if !exemplar_points.is_empty() {
            datasets.push(
                Dataset::default()
                    .marker(Marker::Dot)
                    .graph_type(GraphType::Scatter)
                    .style(Style::default().fg(Color::Magenta))
                    .data(&exemplar_points),
            );
        }
        let x_labels = vec![
            Line::from(clock_hms_ns(detail.points.first().unwrap().0)),
            Line::from(clock_hms_ns(detail.points[cursor].0)),
            Line::from(clock_hms_ns(detail.points.last().unwrap().0)),
        ];
        let y_labels = vec![
            Line::from(format_metric_value(y_min)),
            Line::from(format_metric_value((y_min + y_max) / 2.0)),
            Line::from(format_metric_value(y_max)),
        ];
        let chart = Chart::new(datasets)
            .block(g.block().border_style(focus_border(focused)).title("Chart"))
            .x_axis(
                Axis::default()
                    .style(Style::default().fg(Color::DarkGray))
                    .bounds([x_min, x_max])
                    .labels(x_labels),
            )
            .y_axis(
                Axis::default()
                    .style(Style::default().fg(Color::DarkGray))
                    .bounds([y_min, y_max])
                    .labels(y_labels),
            );
        frame.render_widget(chart, plot_area);

        // Publish where each datapoint actually landed on screen, so the mascot's chart ride can walk
        // the rendered line (see `ChartRide`). Reproduces ratatui's `Chart` graph-area layout exactly.
        let y_label_strs = [
            format_metric_value(y_min),
            format_metric_value((y_min + y_max) / 2.0),
            format_metric_value(y_max),
        ];
        let x_first = clock_hms_ns(detail.points.first().unwrap().0);
        let block_inner = g.block().inner(plot_area);
        let geom = chart_graph_area(block_inner, &y_label_strs, &x_first).map(|graph| {
            let cells = finite
                .iter()
                .filter_map(|&(x, y)| chart_point_cell(graph, x_min, x_max, y_min, y_max, x, y))
                .collect();
            ChartGeometry { graph, cells }
        });
        app.chart_geom.replace(geom);
    }

    // Readout: the point under the cursor, then summary stats over the finite samples. The window can
    // hold no points at all (e.g. after panning/zooming to an empty range), so the cursor line degrades
    // to a placeholder rather than indexing an empty series.
    let cursor_line = match detail.points.get(cursor) {
        Some(&(cursor_ns, cursor_val)) => {
            let cursor_value = if cursor_val.is_finite() {
                format_metric_value(cursor_val)
            } else {
                "n/a".to_owned()
            };
            Line::from(vec![
                Span::styled(
                    format!("cursor [{}/{}] ", cursor + 1, detail.points.len()),
                    Style::default().fg(Color::Yellow),
                ),
                Span::raw(format!(
                    "{}  =  {cursor_value}",
                    format_datetime_ns(cursor_ns)
                )),
            ])
        }
        None => Line::from(Span::styled(
            "no samples in this window",
            Style::default().fg(Color::Yellow),
        )),
    };
    let values: Vec<f64> = finite.iter().map(|(_, value)| *value).collect();
    let stats = if values.is_empty() {
        "no finite samples".to_owned()
    } else {
        let min = values.iter().copied().fold(f64::INFINITY, f64::min);
        let max = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        let avg = values.iter().copied().sum::<f64>() / values.len() as f64;
        let latest = *values.last().unwrap();
        format!(
            "min {} {s} max {} {s} avg {} {s} latest {} {s} {} pts",
            format_metric_value(min),
            format_metric_value(max),
            format_metric_value(avg),
            format_metric_value(latest),
            detail.points.len(),
            s = g.sep,
        )
    };
    // Surface the exemplar→trace markers (magenta dots on the plot floor): count + the Enter action.
    let stats = if app.metric_exemplars.is_empty() {
        stats
    } else {
        format!(
            "{stats} {s} {} exemplars (enter: nearest trace)",
            app.metric_exemplars.len(),
            s = g.sep,
        )
    };
    let readout = vec![
        cursor_line,
        Line::from(Span::styled(stats, Style::default().fg(Color::DarkGray))),
        Line::from(Span::styled(
            format!(
                "esc/{l} back {s} bksp series list {s} {r} fwd {s} h/l or shift+{l}{r} move cursor {s} home/end ends {s} pgup/pgdn page",
                l = g.left,
                r = g.right,
                s = g.sep
            ),
            Style::default().fg(Color::DarkGray),
        )),
    ];
    frame.render_widget(Paragraph::new(readout).block(g.block()), rows[2]);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;

    use crate::model::Route;
    use crate::ui::draw;

    #[test]
    fn metric_detail_with_no_points_renders_without_panicking() {
        use ratatui::backend::TestBackend;

        // Panning/zooming to an empty window can leave the detail with zero points; drawing it must not
        // index the empty series (regression: `detail.points[cursor]` panicked and killed the program).
        let mut app = App::new();
        app.route = Route::MetricDetail {
            detail: MetricDetail {
                labels: "__name__=m".to_owned(),
                query: "m".to_owned(),
                points: Vec::new(),
            },
        };
        app.metric_cursor = 5; // stale cursor from a previously non-empty window
        let options = Options::default();
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|frame| draw(frame, &app, &options))
            .expect("draw must not panic on an empty metric detail");
        let buffer = terminal.backend().buffer();
        let mut text = String::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                text.push_str(buffer[(x, y)].symbol());
            }
        }
        assert!(text.contains("no samples in this window"), "{text:?}");
    }
}

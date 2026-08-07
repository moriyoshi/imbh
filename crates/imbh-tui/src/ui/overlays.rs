//! The dropdown overlays: the time-range picker, the absolute-range form, and the completion
//! popup.

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, List, ListItem, ListState, Paragraph};
use unicode_width::UnicodeWidthStr;

use crate::app::App;
use crate::completion::{CandidateKind, Completion};
use crate::model::TIME_RANGES;
use crate::time::humanize_secs;
use crate::ui::glyphs::Glyphs;

pub(crate) fn draw_time_range_picker(
    frame: &mut ratatui::Frame<'_>,
    app: &App,
    anchor: Rect,
    area: Rect,
    g: &Glyphs,
) {
    // Drop straight down from the indicator box, right-aligned to its right edge so a wide dropdown
    // does not spill past the frame; clamp on every side to stay within the terminal.
    let width = 36u16.min(area.width);
    // One row per preset plus the trailing "Absolute…" row, and the two borders.
    let height = (TIME_RANGES.len() as u16 + 3).min(area.height);
    let x = anchor
        .right()
        .saturating_sub(width)
        .min(area.right().saturating_sub(width))
        .max(area.x);
    let y = anchor.bottom().min(area.bottom().saturating_sub(height));
    let popup = Rect {
        x,
        y,
        width,
        height,
    };
    frame.render_widget(Clear, popup);
    let mut items = TIME_RANGES
        .iter()
        .map(|(label, lookback, step)| {
            ListItem::new(format!(
                "{label:<4} window {:>5}  step {}s",
                humanize_secs(lookback.as_secs()),
                step.as_secs()
            ))
        })
        .collect::<Vec<_>>();
    items.push(ListItem::new(format!(
        "Absolute{}  set explicit start / end",
        g.ellipsis
    )));
    let mut state = ListState::default();
    state.select(Some(app.range_cursor));
    frame.render_stateful_widget(
        List::new(items)
            .block(g.block().title("Time range (Enter: apply, Esc: cancel)"))
            .highlight_style(
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
        popup,
        &mut state,
    );
}

/// Render the absolute-time window form as a dropdown under the indicator box: two labeled datetime
/// fields (the focused one highlighted with a caret) and a hint/parse-error line.
pub(crate) fn draw_absolute_range(
    frame: &mut ratatui::Frame<'_>,
    app: &App,
    anchor: Rect,
    area: Rect,
    g: &Glyphs,
) {
    let width = 48u16.min(area.width);
    // Two borders + two field lines + one hint line.
    let height = 5u16.min(area.height);
    let x = anchor
        .right()
        .saturating_sub(width)
        .min(area.right().saturating_sub(width))
        .max(area.x);
    let y = anchor.bottom().min(area.bottom().saturating_sub(height));
    let popup = Rect {
        x,
        y,
        width,
        height,
    };
    frame.render_widget(Clear, popup);

    // Label + two spaces, then the value; the rest of the inner width is what the value may occupy.
    let label_width = 8u16;
    let value_width = popup.width.saturating_sub(2).saturating_sub(label_width) as usize;
    let field_line = |label: &str, value: &str, focused: bool| {
        let value_style = if focused {
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        let mut spans = vec![Span::styled(
            format!(" {label}  "),
            Style::default().fg(Color::DarkGray),
        )];
        if focused {
            // The focused field carries the block caret (the global terminal cursor stays hidden), and
            // scrolls within its columns so a value longer than the popup cannot hide it.
            spans.extend(caret_spans(
                value,
                app.abs_caret(),
                value_width,
                value_style,
            ));
        } else {
            spans.push(Span::styled(value.to_owned(), value_style));
        }
        Line::from(spans)
    };
    let hint = match &app.abs_error {
        Some(error) => Span::styled(format!(" {error}"), Style::default().fg(Color::Red)),
        None => Span::styled(
            format!(
                " UTC {s} YYYY-MM-DD HH:MM:SS {s} Tab: field {s} Enter: apply",
                s = g.sep
            ),
            Style::default().fg(Color::DarkGray),
        ),
    };
    let text = vec![
        field_line("start", &app.abs_start, app.abs_field == 0),
        field_line("end  ", &app.abs_end, app.abs_field == 1),
        Line::from(hint),
    ];
    frame.render_widget(
        Paragraph::new(text).block(g.block().title("Absolute range (Esc: cancel)")),
        popup,
    );
}

/// A plain (unhighlighted) text field with a block caret on the character at byte offset `caret`, or
/// an appended caret block when it sits at the end.
///
/// The result is windowed to `width` columns around the caret. This is the query pane's
/// `Paragraph::scroll` by hand: these fields share their paragraph with the other field and the hint
/// line, so scrolling the widget would drag those sideways too.
fn caret_spans(value: &str, caret: usize, width: usize, style: Style) -> Vec<Span<'static>> {
    let caret_style = style.add_modifier(Modifier::REVERSED);
    // One cell per character, tagged with whether the caret is on it. A caret at the end has no
    // character to mark, so it becomes a trailing block — and only then is a cell appended.
    let mut cells: Vec<(String, bool)> = value
        .char_indices()
        .map(|(at, character)| (character.to_string(), at == caret))
        .collect();
    if caret >= value.len() {
        cells.push((" ".to_owned(), true));
    }

    // Scroll by the least amount that brings the caret's column inside the window. Column ≠ index for
    // wide characters, so the columns are accumulated rather than counted.
    let mut columns: Vec<usize> = Vec::with_capacity(cells.len());
    let mut column = 0usize;
    for (text, _) in &cells {
        columns.push(column);
        column += text.as_str().width().max(1);
    }
    let caret_column = cells
        .iter()
        .position(|(_, is_caret)| *is_caret)
        .map_or(0, |index| columns[index]);
    let offset = caret_column.saturating_sub(width.saturating_sub(1));

    // Emit the visible window, merging runs of ordinary cells so the caret is the only split.
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut run = String::new();
    for (index, (text, is_caret)) in cells.iter().enumerate() {
        let start = columns[index];
        if start < offset || start >= offset + width {
            continue;
        }
        if *is_caret {
            if !run.is_empty() {
                spans.push(Span::styled(std::mem::take(&mut run), style));
            }
            spans.push(Span::styled(text.clone(), caret_style));
        } else {
            run.push_str(text);
        }
    }
    if !run.is_empty() {
        spans.push(Span::styled(run, style));
    }
    spans
}

/// Render the completion popup anchored just below the query bar. Candidates are coloured by kind
/// (metric/function/keyword) and the highlighted row tracks `completion.selected`.
pub(crate) fn draw_completion_popup(
    frame: &mut ratatui::Frame<'_>,
    completion: &Completion,
    query_area: Rect,
    frame_area: Rect,
    g: &Glyphs,
) {
    const MAX_VISIBLE: usize = 8;
    let longest = completion
        .candidates
        .iter()
        .map(|candidate| candidate.text.chars().count())
        .max()
        .unwrap_or(8);
    // border (2) + highlight symbol "▶ " (2) around the widest candidate.
    let desired_width = longest.clamp(8, 40) as u16 + 4;
    let height = completion.candidates.len().min(MAX_VISIBLE) as u16 + 2;
    // Anchor one cell in from the query box's left edge, directly beneath it.
    let x = query_area.x + 2;
    let y = query_area.y + query_area.height;
    let width = desired_width.min(frame_area.right().saturating_sub(x).max(1));
    let height = height.min(frame_area.bottom().saturating_sub(y).max(1));
    let popup = Rect {
        x,
        y,
        width,
        height,
    };
    frame.render_widget(Clear, popup);
    let items = completion
        .candidates
        .iter()
        .map(|candidate| {
            let style = match candidate.kind {
                CandidateKind::Function => Style::default().fg(Color::Cyan),
                CandidateKind::Keyword => Style::default().fg(Color::Yellow),
                CandidateKind::Metric => Style::default(),
                CandidateKind::Label => Style::default().fg(Color::Green),
                CandidateKind::LabelValue => Style::default().fg(Color::Magenta),
                CandidateKind::Operator => Style::default().fg(Color::Blue),
            };
            ListItem::new(Span::styled(candidate.text.clone(), style))
        })
        .collect::<Vec<_>>();
    let mut state = ListState::default();
    state.select(Some(completion.selected));
    frame.render_stateful_widget(
        List::new(items)
            .block(g.block().title("Tab: complete"))
            .highlight_style(
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
        popup,
        &mut state,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `(rendered text, the character under the caret)`.
    fn rendered(spans: &[Span<'static>]) -> (String, String) {
        let text = spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();
        let caret = spans
            .iter()
            .filter(|span| span.style.add_modifier.contains(Modifier::REVERSED))
            .map(|span| span.content.as_ref())
            .collect::<String>();
        (text, caret)
    }

    #[test]
    fn a_field_marks_the_character_under_its_caret() {
        let style = Style::default();
        // Mid-field: the value is reproduced verbatim with one character reversed.
        let (text, caret) = rendered(&caret_spans("2026-07-21", 5, 40, style));
        assert_eq!(text, "2026-07-21");
        assert_eq!(caret, "0", "the month's leading digit, at byte 5");
        // At the end there is nothing to reverse, so the caret is an appended block.
        let (text, caret) = rendered(&caret_spans("2026", 4, 40, style));
        assert_eq!((text.as_str(), caret.as_str()), ("2026 ", " "));
        // An empty field is just the caret.
        assert_eq!(rendered(&caret_spans("", 0, 40, style)).1, " ");
    }

    #[test]
    fn a_field_longer_than_the_popup_scrolls_to_keep_its_caret_visible() {
        let style = Style::default();
        // Ten columns of room for a 19-character datetime: with the caret at the end, the window shows
        // the tail and the caret's own cell.
        let (text, caret) = rendered(&caret_spans("2026-07-21 15:00:00", 19, 10, style));
        assert_eq!(text, " 15:00:00 ", "the tail, ending in the caret block");
        assert_eq!(caret, " ");
        assert!(text.width() <= 10);
        // Back at the start, the window follows the caret home.
        let (text, caret) = rendered(&caret_spans("2026-07-21 15:00:00", 0, 10, style));
        assert_eq!(text, "2026-07-21");
        assert_eq!(caret, "2");
    }
}

//! The dropdown overlays: the time-range picker, the absolute-range form, and the completion
//! popup.

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, List, ListItem, ListState, Paragraph};

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

    let field_line = |label: &str, value: &str, focused: bool| {
        let value_style = if focused {
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        let mut spans = vec![
            Span::styled(format!(" {label}  "), Style::default().fg(Color::DarkGray)),
            Span::styled(value.to_owned(), value_style),
        ];
        if focused {
            // A block caret marks the edit point (the global terminal cursor stays hidden).
            spans.push(Span::styled(
                " ",
                Style::default().add_modifier(Modifier::REVERSED),
            ));
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

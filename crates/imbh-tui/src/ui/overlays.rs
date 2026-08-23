//! The dropdown overlays: the time-range picker, the absolute-range form, and the completion
//! popup.

use std::time::Duration;

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, List, ListItem, ListState, Paragraph};
use unicode_width::UnicodeWidthStr;

use crate::app::App;
use crate::completion::{CandidateKind, Completion};
use crate::model::{AbsTarget, SPINNER_FRAME, TIME_RANGES};
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
    // Hang from the anchor's near edge: under a narrow header indicator that means right-aligned to
    // it, and over a full-width pane it means the pane's left edge, which is where its title is.
    let x = if anchor.width > width {
        anchor.x.min(area.right().saturating_sub(width))
    } else {
        anchor
            .right()
            .saturating_sub(width)
            .min(area.right().saturating_sub(width))
    }
    .max(area.x);
    // Below the anchor when it is a header strip, just inside it when it is a pane — the form must not
    // land past the bottom of a pane it is meant to belong to.
    let y = if anchor.height > height {
        anchor.y.saturating_add(1)
    } else {
        anchor.bottom()
    }
    .min(area.bottom().saturating_sub(height));
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

/// Render the absolute-time window form as a dropdown under `anchor`: two labeled datetime fields
/// (the focused one highlighted with a caret) and a hint/parse-error line.
///
/// The anchor is the thing being edited — the header's time indicator for the query window, the
/// attribute pane for the attribute window — so the form appears over what it changes rather than
/// always in the header's corner.
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
    let attributes = app.abs_target == AbsTarget::Attributes;
    let hint = match &app.abs_error {
        Some(error) => Span::styled(format!(" {error}"), Style::default().fg(Color::Red)),
        None => Span::styled(
            if attributes {
                // Clearing both fields is the only way back to following the query range, so the form
                // has to say so — nothing else would suggest an empty field means anything.
                format!(
                    " UTC {s} Tab: field {s} Enter: apply {s} empty: follow the query range",
                    s = g.sep
                )
            } else {
                format!(
                    " UTC {s} YYYY-MM-DD HH:MM:SS {s} Tab: field {s} Enter: apply",
                    s = g.sep
                )
            },
            Style::default().fg(Color::DarkGray),
        ),
    };
    let text = vec![
        field_line("start", &app.abs_start, app.abs_field == 0),
        field_line("end  ", &app.abs_end, app.abs_field == 1),
        Line::from(hint),
    ];
    frame.render_widget(
        Paragraph::new(text).block(g.block().title(if attributes {
            "Attribute-statistics range (Esc: cancel)"
        } else {
            "Absolute range (Esc: cancel)"
        })),
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

/// The loading banner's one line of text: spinner, the word, elapsed whole seconds. Nothing else.
///
/// It carries no hint about the keyboard, and does not vary with whether the lock is on. Two earlier
/// drafts spelled the lock out (`input paused {sep} q quits`, then `keys paused except q`) and both
/// were redundant: the footer already lists `q quit` on every frame, and a spinner beside the word
/// "Loading" is what "wait" looks like. The banner's job is to be the visible reason keys are doing
/// nothing; saying so in words adds a clause the user has already read at the bottom of the screen.
///
/// Split out from the drawing so the wording and the spinner's advance can be asserted without a
/// terminal. The frame is derived from `elapsed` rather than from a counter, which keeps the whole
/// banner a pure function of state the event loop already has.
pub(crate) fn loading_banner_text(elapsed: Duration, g: &Glyphs) -> String {
    let frames = g.spinner();
    let frame = (elapsed.as_millis() / SPINNER_FRAME.as_millis()) as usize % frames.len();
    format!(
        "{} Loading{} {}s",
        frames[frame],
        g.ellipsis,
        elapsed.as_secs()
    )
}

/// Draw the loading banner: a small box centred on the screen.
///
/// The centre is where the eye already is, and this is the only thing on screen saying why the keys
/// have stopped working — so it is placed where it cannot be missed rather than tucked into the
/// chrome. It covers a few rows of whatever is underneath, which is the trade: the content behind it
/// is by definition about to be replaced by the result being waited on.
pub(crate) fn draw_loading_banner(
    frame: &mut ratatui::Frame<'_>,
    elapsed: Duration,
    area: Rect,
    g: &Glyphs,
) {
    let text = loading_banner_text(elapsed, g);
    // Two columns of padding inside the borders, and never wider than the terminal.
    let width = (text.width() as u16 + 4).min(area.width);
    let height = 3u16.min(area.height);
    let banner = Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    };
    frame.render_widget(Clear, banner);
    // Yellow reads as "waiting on something", and is distinct from both the cyan focus ring and the
    // red of `last_error` — this is not a failure, and must not be mistaken for one.
    let style = Style::default().fg(Color::Yellow);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            text,
            style.add_modifier(Modifier::BOLD),
        )))
        .block(g.block().border_style(style))
        .alignment(ratatui::layout::Alignment::Center),
        banner,
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

    #[test]
    fn the_banner_says_what_is_happening_and_for_how_long_and_no_more() {
        let g = Glyphs::new(false);
        let text = loading_banner_text(Duration::from_secs(4), &g);
        assert!(text.contains("Loading"), "{text}");
        assert!(text.contains("4s"), "{text}");
        // No keyboard hint: the footer already lists `q quit` on every frame, so spelling the lock
        // out here would repeat what the user has already read at the bottom of the screen.
        assert!(!text.contains("paused"), "{text}");
        assert!(!text.contains('q'), "{text}");
    }

    #[test]
    fn the_spinner_turns_as_the_wait_goes_on() {
        let g = Glyphs::new(false);
        // Derived from elapsed time, so consecutive frames differ and the cycle comes back around.
        let frames = g.spinner();
        // The glyph alone: the rest of the line carries the elapsed seconds, which of course differ.
        let at = |ms: u64| {
            loading_banner_text(Duration::from_millis(ms), &g)
                .chars()
                .next()
                .expect("the spinner leads the line")
                .to_string()
        };
        assert_ne!(at(0), at(SPINNER_FRAME.as_millis() as u64));
        let cycle = SPINNER_FRAME.as_millis() as u64 * frames.len() as u64;
        assert_eq!(at(0), at(cycle), "one full turn returns to the first frame");
    }

    #[test]
    fn the_ascii_banner_stays_ascii() {
        // `--ascii` promises no Unicode anywhere in the chrome, and the banner is chrome.
        let text = loading_banner_text(Duration::from_secs(3), &Glyphs::new(true));
        assert!(text.is_ascii(), "{text}");
    }
}

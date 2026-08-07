//! Query-language knowledge shared by the editor: the keyword/function vocabularies, the trailing
//! identifier the caret sits on, and the lightweight lexer that colours the input bar.

use std::ops::Range;

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;

use crate::model::Screen;
use crate::ui::glyphs::Glyphs;

/// Non-function keywords worth colouring per query language. Anything immediately followed by `(` is
/// treated as a function regardless of this set, so only the "bare word" operators need listing.
/// The LogQL line-filter operators the Logs search box accepts, offered as expression-position
/// completion hints on the Logs screen. `|?` / `!?` are the imbh dialect's Tantivy-accelerated term
/// operators; `|=` / `!=` (substring) and `|~` / `!~` (regex) are standard LogQL. Kept in sync with
/// the parser in `imbh-lgtm` (`syntax/logql.rs`).
pub(crate) const LOGQL_LINE_FILTERS: &[&str] = &["|=", "!=", "|~", "!~", "|?", "!?"];

pub(crate) fn keywords_for(screen: Screen) -> &'static [&'static str] {
    match screen {
        Screen::Metrics => &[
            "by",
            "without",
            "on",
            "ignoring",
            "group_left",
            "group_right",
            "offset",
            "bool",
            "and",
            "or",
            "unless",
            "inf",
            "nan",
        ],
        Screen::Logs => &[
            "by",
            "without",
            "unwrap",
            "json",
            "logfmt",
            "regexp",
            "pattern",
            "line_format",
            "label_format",
            "ip",
            "and",
            "or",
        ],
        Screen::Traces => &[
            "by",
            "select",
            "and",
            "or",
            "duration",
            "status",
            "name",
            "kind",
            "rootName",
            "rootServiceName",
        ],
        Screen::Overview => &[],
    }
}

/// Call-like functions worth completing per language (those that take a `(`). Accepting one appends
/// the opening paren. Bare keywords come from [`keywords_for`]; metric names are dynamic.
pub(crate) fn functions_for(screen: Screen) -> &'static [&'static str] {
    match screen {
        Screen::Metrics => &[
            "abs",
            "absent",
            "absent_over_time",
            "avg",
            "avg_over_time",
            "bottomk",
            "ceil",
            "changes",
            "clamp",
            "clamp_max",
            "clamp_min",
            "count",
            "count_over_time",
            "count_values",
            "delta",
            "deriv",
            "exp",
            "floor",
            "histogram_quantile",
            "increase",
            "irate",
            "label_join",
            "label_replace",
            "last_over_time",
            "ln",
            "log10",
            "log2",
            "max",
            "max_over_time",
            "min",
            "min_over_time",
            "predict_linear",
            "present_over_time",
            "quantile",
            "quantile_over_time",
            "rate",
            "resets",
            "round",
            "scalar",
            "sort",
            "sort_desc",
            "sqrt",
            "stddev",
            "stddev_over_time",
            "stdvar",
            "sum",
            "sum_over_time",
            "time",
            "timestamp",
            "topk",
            "vector",
        ],
        Screen::Logs => &[
            "avg",
            "avg_over_time",
            "bottomk",
            "bytes_over_time",
            "bytes_rate",
            "count",
            "count_over_time",
            "first_over_time",
            "last_over_time",
            "max",
            "max_over_time",
            "min",
            "min_over_time",
            "quantile_over_time",
            "rate",
            "stddev_over_time",
            "stdvar_over_time",
            "sum",
            "sum_over_time",
            "topk",
        ],
        Screen::Traces => &["avg", "count", "histogram", "max", "min", "quantile", "sum"],
        Screen::Overview => &[],
    }
}

/// The identifier token at the very end of the query — the run of `[A-Za-z0-9_:.]` the editor's caret
/// sits on. Callers pass the query *up to the caret*, so "the end of the string" is the caret. This is
/// what completion suggests against.
pub(crate) fn current_token(query: &str) -> &str {
    let mut start = query.len();
    for (index, ch) in query.char_indices().rev() {
        if ch.is_alphanumeric() || matches!(ch, '_' | ':' | '.') {
            start = index;
        } else {
            break;
        }
    }
    &query[start..]
}

/// Whether `ch` can appear in a metric/label identifier (the run [`current_token`] recognises).
pub(crate) fn is_ident_char(ch: char) -> bool {
    ch.is_alphanumeric() || matches!(ch, '_' | ':' | '.')
}

/// The trailing identifier run of `s` (like [`current_token`] but usable on an arbitrary slice).
pub(crate) fn trailing_ident(s: &str) -> &str {
    let mut start = s.len();
    for (index, ch) in s.char_indices().rev() {
        if is_ident_char(ch) {
            start = index;
        } else {
            break;
        }
    }
    &s[start..]
}

/// Tokenize a query into coloured spans for the input bar, each paired with the byte range of `query`
/// it was produced from — what the caret renderer needs to split a span at the edit point. Deliberately
/// a lightweight lexer shared by all three languages (strings, numbers/durations, identifiers/
/// functions, operators, punctuation) rather than a per-grammar parser; it is presentation only and
/// never rejects input.
///
/// Every span's content reproduces its source range verbatim, with one exception: a `\n` renders as a
/// three-column separator (see below), so the two are not interchangeable — [`highlight_caret`] keys
/// off exactly that.
pub(crate) fn highlight_spans(
    screen: Screen,
    query: &str,
    g: &Glyphs,
) -> Vec<(Span<'static>, Range<usize>)> {
    // `?` is included so the imbh LogQL dialect's `|?` / `!?` term operators highlight as a unit.
    const OPERATORS: &[char] = &[
        '=', '!', '~', '<', '>', '|', '&', '+', '-', '*', '/', '^', '?',
    ];
    let keywords = keywords_for(screen);
    let chars: Vec<char> = query.chars().collect();
    // Char index -> byte offset, with a final entry for the end of the string, so each span's char
    // bounds map back to a byte range of `query`.
    let offsets: Vec<usize> = query
        .char_indices()
        .map(|(at, _)| at)
        .chain(std::iter::once(query.len()))
        .collect();
    let mut spans: Vec<(Span<'static>, Range<usize>)> = Vec::new();
    let take = |from: usize, to: usize| chars[from..to].iter().collect::<String>();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let start = i;
        let span = if c == '\n' {
            // Several queries are stored newline-joined (multi-metric visualization); show each break
            // as a visible separator so the single-line bar stays readable.
            i += 1;
            Span::styled(
                format!(" {} ", g.vline),
                Style::default().fg(Color::DarkGray),
            )
        } else if c.is_whitespace() {
            while i < chars.len() && chars[i].is_whitespace() && chars[i] != '\n' {
                i += 1;
            }
            Span::raw(take(start, i))
        } else if c == '"' || c == '\'' || c == '`' {
            let quote = c;
            i += 1;
            while i < chars.len() {
                if chars[i] == '\\' && quote != '`' {
                    i = (i + 2).min(chars.len());
                    continue;
                }
                let ch = chars[i];
                i += 1;
                if ch == quote {
                    break;
                }
            }
            Span::styled(take(start, i), Style::default().fg(Color::Green))
        } else if c.is_ascii_digit() {
            // Numbers and durations (e.g. `5m`, `1h30m`, `0.5`) — trailing unit letters included.
            while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '.') {
                i += 1;
            }
            Span::styled(take(start, i), Style::default().fg(Color::Magenta))
        } else if c.is_alphabetic() || c == '_' || c == ':' {
            while i < chars.len()
                && (chars[i].is_alphanumeric() || matches!(chars[i], '_' | ':' | '.'))
            {
                i += 1;
            }
            let word = take(start, i);
            let mut j = i;
            while j < chars.len() && chars[j].is_whitespace() {
                j += 1;
            }
            let is_call = j < chars.len() && chars[j] == '(';
            let style = if is_call || keywords.contains(&word.as_str()) {
                Style::default().fg(Color::Cyan)
            } else {
                Style::default()
            };
            Span::styled(word, style)
        } else if OPERATORS.contains(&c) {
            while i < chars.len() && OPERATORS.contains(&chars[i]) {
                i += 1;
            }
            Span::styled(take(start, i), Style::default().fg(Color::Yellow))
        } else if matches!(c, '{' | '}' | '[' | ']' | '(' | ')' | ',') {
            i += 1;
            Span::styled(c.to_string(), Style::default().fg(Color::DarkGray))
        } else {
            i += 1;
            Span::raw(c.to_string())
        };
        spans.push((span, offsets[start]..offsets[i]));
    }
    spans
}

/// The coloured spans for the input bar (see [`highlight_spans`]), without the source ranges.
pub(crate) fn highlight_query(screen: Screen, query: &str, g: &Glyphs) -> Vec<Span<'static>> {
    highlight_spans(screen, query, g)
        .into_iter()
        .map(|(span, _)| span)
        .collect()
}

/// The coloured spans with the character at byte offset `caret` drawn reversed — a block caret, since
/// the terminal's own cursor stays hidden. A caret at the end of the query (the usual case while
/// typing) has no character to reverse, so a reversed space is appended instead.
///
/// A `\n` renders wider than its source, so a caret on one marks the whole separator rather than
/// slicing a string that no longer lines up with the input.
pub(crate) fn highlight_caret(
    screen: Screen,
    query: &str,
    caret: usize,
    g: &Glyphs,
) -> Vec<Span<'static>> {
    let reversed = |style: Style| style.add_modifier(Modifier::REVERSED);
    let mut out: Vec<Span<'static>> = Vec::new();
    let mut marked = false;
    for (span, range) in highlight_spans(screen, query, g) {
        if marked || !range.contains(&caret) {
            out.push(span);
            continue;
        }
        marked = true;
        let source = &query[range.start..range.end];
        if span.content.chars().count() != source.chars().count() {
            out.push(Span::styled(span.content, reversed(span.style)));
            continue;
        }
        // The caret's character offset within the span, which the equal char counts above make valid
        // for the rendered content too.
        let rel = source[..caret - range.start].chars().count();
        let content = span.content.as_ref();
        let before: String = content.chars().take(rel).collect();
        let at: String = content.chars().skip(rel).take(1).collect();
        let after: String = content.chars().skip(rel + 1).collect();
        if !before.is_empty() {
            out.push(Span::styled(before, span.style));
        }
        out.push(Span::styled(at, reversed(span.style)));
        if !after.is_empty() {
            out.push(Span::styled(after, span.style));
        }
    }
    if !marked {
        out.push(Span::styled(" ", reversed(Style::default())));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn highlight_reconstructs_the_input_and_colours_calls() {
        let query = "rate({job=\"api\"}[5m])";
        let spans = highlight_query(Screen::Metrics, query, &Glyphs::new(false));
        // Highlighting is presentation-only: concatenating the spans must reproduce the input exactly.
        let rebuilt = spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert_eq!(rebuilt, query);
        // The leading `rate` is a function call (followed by `(`) and must be coloured.
        assert_eq!(spans[0].content.as_ref(), "rate");
        assert_eq!(spans[0].style.fg, Some(Color::Cyan));
        // The quoted value is a string span.
        assert!(
            spans
                .iter()
                .any(|span| span.content.as_ref() == "\"api\""
                    && span.style.fg == Some(Color::Green))
        );
    }

    #[test]
    fn the_caret_reverses_exactly_the_character_it_sits_on() {
        let g = Glyphs::new(false);
        let query = "rate(x)";
        let rebuilt = |spans: &[Span<'static>]| {
            spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        };
        let reversed = |spans: &[Span<'static>]| {
            spans
                .iter()
                .filter(|span| span.style.add_modifier.contains(Modifier::REVERSED))
                .map(|span| span.content.as_ref())
                .collect::<String>()
        };

        // Mid-query: the input is still reproduced verbatim, with just that one character reversed.
        let spans = highlight_caret(Screen::Metrics, query, 5, &g);
        assert_eq!(rebuilt(&spans), query);
        assert_eq!(reversed(&spans), "x");
        // The `(` before it, i.e. the last character of a span rather than the first.
        assert_eq!(
            reversed(&highlight_caret(Screen::Metrics, query, 4, &g)),
            "("
        );
        // Inside a multi-character token.
        assert_eq!(
            reversed(&highlight_caret(Screen::Metrics, query, 2, &g)),
            "t"
        );

        // At the end there is nothing to reverse, so the caret is an appended block.
        let spans = highlight_caret(Screen::Metrics, query, query.len(), &g);
        assert_eq!(rebuilt(&spans), format!("{query} "));
        assert_eq!(reversed(&spans), " ");

        // A multi-byte character is marked whole.
        assert_eq!(
            reversed(&highlight_caret(Screen::Logs, "{a=\"é\"}", 4, &g)),
            "é"
        );

        // A newline renders wider than its source (a separator), so the caret marks all of it rather
        // than slicing a string that no longer lines up with the input.
        let spans = highlight_caret(Screen::Metrics, "a\nb", 1, &g);
        assert_eq!(reversed(&spans), format!(" {} ", g.vline));
    }

    #[test]
    fn current_token_is_the_trailing_identifier() {
        assert_eq!(current_token(""), "");
        assert_eq!(current_token("rate(htt"), "htt");
        assert_eq!(current_token("sum by (inst"), "inst");
        assert_eq!(current_token("{job=\"api\"}"), ""); // ends on punctuation -> no token
        assert_eq!(current_token("http_requests_total"), "http_requests_total");
    }
}

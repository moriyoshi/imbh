//! Query-language knowledge shared by the editor: the keyword/function vocabularies, the trailing
//! identifier the caret sits on, and the lightweight lexer that colours the input bar.

use ratatui::style::{Color, Style};
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
/// (always the end of the string) currently sits on. This is what completion suggests against.
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

/// Tokenize a query into coloured spans for the input bar. Deliberately a lightweight lexer shared by
/// all three languages (strings, numbers/durations, identifiers/functions, operators, punctuation)
/// rather than a per-grammar parser; it is presentation only and never rejects input.
pub(crate) fn highlight_query(screen: Screen, query: &str, g: &Glyphs) -> Vec<Span<'static>> {
    // `?` is included so the imbh LogQL dialect's `|?` / `!?` term operators highlight as a unit.
    const OPERATORS: &[char] = &[
        '=', '!', '~', '<', '>', '|', '&', '+', '-', '*', '/', '^', '?',
    ];
    let keywords = keywords_for(screen);
    let chars: Vec<char> = query.chars().collect();
    let mut spans: Vec<Span<'static>> = Vec::new();
    let take = |from: usize, to: usize| chars[from..to].iter().collect::<String>();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '\n' {
            // Several queries are stored newline-joined (multi-metric visualization); show each break
            // as a visible separator so the single-line bar stays readable.
            spans.push(Span::styled(
                format!(" {} ", g.vline),
                Style::default().fg(Color::DarkGray),
            ));
            i += 1;
            continue;
        }
        if c.is_whitespace() {
            let start = i;
            while i < chars.len() && chars[i].is_whitespace() {
                i += 1;
            }
            spans.push(Span::raw(take(start, i)));
        } else if c == '"' || c == '\'' || c == '`' {
            let quote = c;
            let start = i;
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
            spans.push(Span::styled(
                take(start, i),
                Style::default().fg(Color::Green),
            ));
        } else if c.is_ascii_digit() {
            // Numbers and durations (e.g. `5m`, `1h30m`, `0.5`) — trailing unit letters included.
            let start = i;
            while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '.') {
                i += 1;
            }
            spans.push(Span::styled(
                take(start, i),
                Style::default().fg(Color::Magenta),
            ));
        } else if c.is_alphabetic() || c == '_' || c == ':' {
            let start = i;
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
            spans.push(Span::styled(word, style));
        } else if OPERATORS.contains(&c) {
            let start = i;
            while i < chars.len() && OPERATORS.contains(&chars[i]) {
                i += 1;
            }
            spans.push(Span::styled(
                take(start, i),
                Style::default().fg(Color::Yellow),
            ));
        } else if matches!(c, '{' | '}' | '[' | ']' | '(' | ')' | ',') {
            spans.push(Span::styled(
                c.to_string(),
                Style::default().fg(Color::DarkGray),
            ));
            i += 1;
        } else {
            spans.push(Span::raw(c.to_string()));
            i += 1;
        }
    }
    spans
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
    fn current_token_is_the_trailing_identifier() {
        assert_eq!(current_token(""), "");
        assert_eq!(current_token("rate(htt"), "htt");
        assert_eq!(current_token("sum by (inst"), "inst");
        assert_eq!(current_token("{job=\"api\"}"), ""); // ends on punctuation -> no token
        assert_eq!(current_token("http_requests_total"), "http_requests_total");
    }
}

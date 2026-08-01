//! The completion vocabulary: what the caret position makes eligible, and the ranked candidates for
//! the token being written.

use std::collections::{HashMap, HashSet};

use crate::model::{DimNode, MetricNode, Screen};
use crate::syntax::{
    LOGQL_LINE_FILTERS, current_token, functions_for, keywords_for, trailing_ident,
};

/// What a completion candidate represents, so accepting a function can append its opening paren.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CandidateKind {
    Function,
    Keyword,
    Metric,
    /// A label/attribute name (inside a `{…}` matcher block).
    Label,
    /// A label value (inside a quoted matcher value).
    LabelValue,
    /// A LogQL line-filter operator hint (`|=` / `!=` / `|~` / `!~` / `|?` / `!?`), offered in
    /// expression position on the Logs screen. Inserted verbatim, like a keyword (no trailing paren).
    Operator,
}

/// Where in the query the caret (always the end of the string) sits, which decides *which* vocabulary
/// is eligible. Metric names and functions only make sense in expression position; inside a `{…}`
/// matcher the eligible items are label names, and inside a quoted value they are that label's values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CompletionContext {
    /// Expression position: metric names, functions, keywords.
    Expr,
    /// A label-name position inside a matcher block; `metric` is the selector before the `{` (if any).
    LabelName { metric: Option<String> },
    /// A label-value position inside a quoted string; `label` is the key being matched.
    LabelValue {
        metric: Option<String>,
        label: String,
    },
    /// A position where no vocabulary applies (e.g. an unquoted value after `=`), so no popup opens.
    Suppressed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Candidate {
    pub(crate) text: String,
    pub(crate) kind: CandidateKind,
}

/// The open completion popup: the ranked candidates for the current token and the highlighted row.
#[derive(Debug, Clone)]
pub(crate) struct Completion {
    pub(crate) candidates: Vec<Candidate>,
    pub(crate) selected: usize,
}

/// What log-completion vocabulary the caret's position wants discovered, mirroring the Metrics
/// `completion_dim_request` flow but for the Logs screen's cross-signal attribute source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LogCompletionRequest {
    /// The set of log label names (`db.attrs().names()`), for a `{…}` label-name position.
    Labels,
    /// The distinct values of one label (`db.attrs().values(key)`), for a quoted-value position.
    Values(String),
}

/// Classify the caret position (always the end of `query`) into a [`CompletionContext`] and return the
/// partial token being written there — the metric/label identifier run in expression/label-name
/// position, or the raw text after the opening `"` in value position. The parser is deliberately
/// lightweight (it counts quotes and scans for the last unbalanced `{`) rather than a full grammar; it
/// only decides which vocabulary is eligible, never rejects input.
pub(crate) fn completion_context(query: &str) -> (CompletionContext, &str) {
    // An odd number of `"` means the caret sits inside an open quoted value. Everything after the last
    // `"` is the partial value; the label is the identifier just before the operator before the quote.
    if query.bytes().filter(|&b| b == b'"').count() % 2 == 1 {
        let open = query.rfind('"').expect("odd quote count implies a quote");
        let before = &query[..open];
        let label =
            trailing_ident(before.trim_end_matches(['=', '!', '~', '<', '>', ' '])).to_owned();
        let metric = before
            .rfind('{')
            .map(|brace| trailing_ident(&before[..brace]))
            .filter(|m| !m.is_empty())
            .map(str::to_owned);
        return (
            CompletionContext::LabelValue { metric, label },
            &query[open + 1..],
        );
    }
    // Not in a quote: are we inside an open `{…}` matcher block (last `{` after last `}`)?
    let open_brace = match (query.rfind('{'), query.rfind('}')) {
        (Some(open), close) if close.is_none_or(|c| open > c) => Some(open),
        _ => None,
    };
    if let Some(open) = open_brace {
        let token = current_token(query);
        // The significant character before the token decides key vs. value position: a label name
        // follows `{`, `,`, or whitespace; anything after an operator (`=` etc.) is a value, which
        // PromQL requires to be quoted, so an unquoted value position offers nothing.
        let before_token = &query[..query.len() - token.len()];
        let prev = before_token.trim_end().chars().next_back();
        match prev {
            Some('{') | Some(',') | None => {
                let metric = trailing_ident(&query[..open]);
                let metric = (!metric.is_empty()).then(|| metric.to_owned());
                (CompletionContext::LabelName { metric }, token)
            }
            _ => (CompletionContext::Suppressed, token),
        }
    } else {
        (CompletionContext::Expr, current_token(query))
    }
}

/// Rank completion candidates whose name starts with `token` (case-insensitive), filtered by the
/// caret `context`. In expression position: metric names first (Metrics screen), then functions, then
/// keywords. In a `{…}` matcher: the label names of the referenced metric (or the union across all
/// discovered metrics for a bare selector). In a quoted value: that label's known values. Each group
/// is sorted and deduplicated.
pub(crate) fn completion_candidates(
    screen: Screen,
    metric_names: &[String],
    metric_tree: &[MetricNode],
    log_labels: &[String],
    log_label_values: &HashMap<String, Vec<String>>,
    context: &CompletionContext,
    token: &str,
) -> Vec<Candidate> {
    const MAX_CANDIDATES: usize = 50;
    let lower = token.to_ascii_lowercase();
    let mut out: Vec<Candidate> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    let mut push_group = |mut names: Vec<String>, kind: CandidateKind, out: &mut Vec<Candidate>| {
        names.sort();
        for name in names {
            if name.to_ascii_lowercase().starts_with(&lower) && seen.insert(name.clone()) {
                out.push(Candidate { text: name, kind });
            }
        }
    };

    match context {
        CompletionContext::Suppressed => {}
        CompletionContext::Expr if screen == Screen::Logs => {
            // The Logs box is a LogQL selector + line-filter box, not a metric-query box, so its
            // expression vocabulary is the imbh line-filter operator hints (not the PromQL/range
            // function list) plus the LogQL pipeline keywords.
            let operators = LOGQL_LINE_FILTERS.iter().map(|s| (*s).to_owned()).collect();
            push_group(operators, CandidateKind::Operator, &mut out);
            let keywords = keywords_for(screen)
                .iter()
                .map(|s| (*s).to_owned())
                .collect();
            push_group(keywords, CandidateKind::Keyword, &mut out);
        }
        CompletionContext::Expr => {
            if screen == Screen::Metrics {
                push_group(metric_names.to_vec(), CandidateKind::Metric, &mut out);
            }
            let functions = functions_for(screen)
                .iter()
                .map(|s| (*s).to_owned())
                .collect();
            push_group(functions, CandidateKind::Function, &mut out);
            let keywords = keywords_for(screen)
                .iter()
                .map(|s| (*s).to_owned())
                .collect();
            push_group(keywords, CandidateKind::Keyword, &mut out);
        }
        CompletionContext::LabelName { metric } => {
            if screen == Screen::Logs {
                // The Logs selector's label names come from cross-signal attribute discovery, not a
                // per-metric dimension tree (there is no metric here).
                push_group(log_labels.to_vec(), CandidateKind::Label, &mut out);
            } else {
                // Label keys for the named metric, or the union across all discovered metrics for a
                // bare selector or an as-yet-undiscovered metric.
                let keys = dims_for(metric_tree, metric.as_deref())
                    .flat_map(|dims| dims.iter().map(|dim| dim.label.clone()))
                    .collect();
                push_group(keys, CandidateKind::Label, &mut out);
            }
        }
        CompletionContext::LabelValue { metric, label } => {
            if screen == Screen::Logs {
                // That label's distinct values, discovered per key (empty until they arrive).
                let values = log_label_values.get(label).cloned().unwrap_or_default();
                push_group(values, CandidateKind::LabelValue, &mut out);
            } else {
                let values = dims_for(metric_tree, metric.as_deref())
                    .flat_map(|dims| dims.iter())
                    .filter(|dim| &dim.label == label)
                    .flat_map(|dim| dim.values.iter().cloned())
                    .collect();
                push_group(values, CandidateKind::LabelValue, &mut out);
            }
        }
    }

    out.truncate(MAX_CANDIDATES);
    out
}

/// The discovered dimension lists in scope for label completion: just the named metric's (when known
/// and loaded), otherwise every metric's — so a bare `{…}` selector still offers the full label
/// vocabulary. Only metrics whose dimensions have been discovered contribute.
pub(crate) fn dims_for<'a>(
    metric_tree: &'a [MetricNode],
    metric: Option<&'a str>,
) -> Box<dyn Iterator<Item = &'a [DimNode]> + 'a> {
    if let Some(name) = metric
        && let Some(node) = metric_tree.iter().find(|n| n.name == name)
    {
        // A known metric contributes only once its dimensions are loaded; while `None`, fall through
        // to nothing (the caller triggers discovery) rather than the whole-catalog union.
        return Box::new(node.dims.as_deref().into_iter());
    }
    Box::new(metric_tree.iter().filter_map(|n| n.dims.as_deref()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completion_ranks_metrics_then_functions() {
        let metrics = vec![
            "http_requests_total".to_owned(),
            "http_errors".to_owned(),
            "process_cpu".to_owned(),
        ];
        let candidates = completion_candidates(
            Screen::Metrics,
            &metrics,
            &[],
            &[],
            &HashMap::new(),
            &CompletionContext::Expr,
            "htt",
        );
        let texts = candidates
            .iter()
            .map(|c| c.text.as_str())
            .collect::<Vec<_>>();
        // Both matching metrics, sorted, and no non-matching one.
        assert_eq!(texts, vec!["http_errors", "http_requests_total"]);
        assert!(candidates.iter().all(|c| c.kind == CandidateKind::Metric));

        // Functions are offered when the prefix matches one (`rate`), even with no metric vocabulary.
        let funcs = completion_candidates(
            Screen::Metrics,
            &[],
            &[],
            &[],
            &HashMap::new(),
            &CompletionContext::Expr,
            "rat",
        );
        assert_eq!(funcs.len(), 1);
        assert_eq!(funcs[0].text, "rate");
        assert_eq!(funcs[0].kind, CandidateKind::Function);
    }

    #[test]
    fn completion_context_classifies_the_caret_position() {
        // Expression position.
        assert_eq!(completion_context("rate(htt").0, CompletionContext::Expr);
        // Inside a matcher block, writing a label name after the metric.
        assert_eq!(
            completion_context("http_requests_total{ser").0,
            CompletionContext::LabelName {
                metric: Some("http_requests_total".to_owned())
            }
        );
        // A bare selector has no metric.
        assert_eq!(
            completion_context("{ser").0,
            CompletionContext::LabelName { metric: None }
        );
        // After a comma, still a label name.
        assert_eq!(
            completion_context("m{a=\"1\",ho").0,
            CompletionContext::LabelName {
                metric: Some("m".to_owned())
            }
        );
        // Inside a quoted value, the label is captured.
        assert_eq!(
            completion_context("http_requests_total{service=\"ca").0,
            CompletionContext::LabelValue {
                metric: Some("http_requests_total".to_owned()),
                label: "service".to_owned()
            }
        );
        // Regex-match operator too.
        assert_eq!(
            completion_context("m{service=~\"ca").0,
            CompletionContext::LabelValue {
                metric: Some("m".to_owned()),
                label: "service".to_owned()
            }
        );
        // An unquoted value position (after `=`) offers nothing.
        assert_eq!(
            completion_context("m{service=").0,
            CompletionContext::Suppressed
        );
        // The returned token is the partial being written.
        assert_eq!(completion_context("m{service=\"ca").1, "ca");
        assert_eq!(completion_context("m{ser").1, "ser");
    }

    #[test]
    fn logs_expression_position_offers_operator_hints_not_promql_functions() {
        // Empty token in expression position on the Logs screen: the LogQL line-filter operator hints
        // (and pipeline keywords), never the PromQL/range function list.
        let candidates = completion_candidates(
            Screen::Logs,
            &[],
            &[],
            &[],
            &HashMap::new(),
            &CompletionContext::Expr,
            "",
        );
        let ops = candidates
            .iter()
            .filter(|c| c.kind == CandidateKind::Operator)
            .map(|c| c.text.as_str())
            .collect::<Vec<_>>();
        assert_eq!(ops, vec!["!=", "!?", "!~", "|=", "|?", "|~"]);
        // No PromQL/range function candidates on the Logs box (e.g. `rate`, `count_over_time`).
        assert!(
            candidates.iter().all(|c| c.kind != CandidateKind::Function),
            "Logs expression position must not offer PromQL functions"
        );
        assert!(
            !candidates.iter().any(|c| c.text == "rate"),
            "`rate` must not be offered on the Logs box"
        );
    }
}

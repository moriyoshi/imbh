//! The query completion popup: recomputing candidates, requesting the vocabularies it needs, and
//! accepting a candidate.

use crate::app::App;
use crate::completion::{
    CandidateKind, Completion, CompletionContext, LogCompletionRequest, completion_candidates,
    completion_context,
};
use crate::model::{Mode, Screen};

impl App {
    /// Recompute the completion popup for the identifier token the caret sits on. Clears it outside
    /// edit mode, when the token is empty, or when the only match is the token itself.
    pub(crate) fn refresh_completion(&mut self) {
        if self.mode != Mode::Editing {
            self.completion = None;
            return;
        }
        let (context, token) = completion_context(self.query_before_caret());
        let token = token.to_owned();
        // Expression position waits for at least one character before popping up (metric/function
        // lists are large); label-name/value position offers its (smaller) vocabulary immediately, so
        // an empty token right after `{` or `"` still lists everything eligible. The Logs screen is an
        // exception in expression position: its vocabulary is the short LogQL line-filter operator-hint
        // list, useful the moment the caret sits after a selector, so it pops even on an empty token.
        let suppress_empty = token.is_empty()
            && match context {
                CompletionContext::Suppressed => true,
                CompletionContext::Expr => self.screen() != Screen::Logs,
                _ => false,
            };
        if suppress_empty {
            self.completion = None;
            return;
        }
        let candidates = completion_candidates(
            self.screen(),
            &self.metric_names,
            &self.metric_tree,
            self.log_labels.as_deref().unwrap_or(&[]),
            &self.log_label_values,
            &context,
            &token,
        );
        // Nothing useful to offer if the sole candidate is exactly what's already typed.
        let redundant = matches!(candidates.as_slice(), [only] if only.text == token);
        self.completion = if candidates.is_empty() || redundant {
            None
        } else {
            let selected = self
                .completion
                .as_ref()
                .map(|c| c.selected.min(candidates.len() - 1))
                .unwrap_or(0);
            Some(Completion {
                candidates,
                selected,
            })
        };
    }

    /// When the caret is in a label position for a known-but-undiscovered metric, mark it loading and
    /// return its name so the caller can fetch its dimensions (the label vocabulary). Returns `None`
    /// once loaded/in-flight, or outside a label context, so it fires at most once per metric.
    pub(crate) fn completion_dim_request(&mut self) -> Option<String> {
        if self.mode != Mode::Editing || self.screen() != Screen::Metrics {
            return None;
        }
        let metric = match completion_context(self.query_before_caret()).0 {
            CompletionContext::LabelName { metric }
            | CompletionContext::LabelValue { metric, .. } => metric?,
            _ => return None,
        };
        let node = self.metric_tree.iter_mut().find(|n| n.name == metric)?;
        if node.dims.is_none() && !node.loading {
            node.loading = true;
            Some(node.name.clone())
        } else {
            None
        }
    }

    /// When the caret sits in a `{…}` label position on the Logs screen and the corresponding
    /// vocabulary (label names, or a specific label's values) is not yet discovered, mark it in-flight
    /// and return the request so the caller can fetch it over the `Update` channel. Returns `None` once
    /// loaded/in-flight or outside a Logs label context, so each fetch fires at most once.
    pub(crate) fn completion_log_request(&mut self) -> Option<LogCompletionRequest> {
        if self.mode != Mode::Editing || self.screen() != Screen::Logs {
            return None;
        }
        match completion_context(self.query_before_caret()).0 {
            CompletionContext::LabelName { .. } => {
                if self.log_labels.is_none() && !self.log_labels_loading {
                    self.log_labels_loading = true;
                    Some(LogCompletionRequest::Labels)
                } else {
                    None
                }
            }
            CompletionContext::LabelValue { label, .. } => {
                if !self.log_label_values.contains_key(&label)
                    && !self.log_label_values_loading.contains(&label)
                {
                    self.log_label_values_loading.insert(label.clone());
                    Some(LogCompletionRequest::Values(label))
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    /// Replace the token under the caret with the highlighted candidate, appending `(` for functions.
    /// Only the token *before* the caret is replaced — whatever follows the caret is left untouched,
    /// and the caret lands at the end of the inserted text.
    pub(crate) fn accept_completion(&mut self) {
        let Some(completion) = self.completion.take() else {
            return;
        };
        let Some(candidate) = completion.candidates.get(completion.selected) else {
            return;
        };
        let caret = self.query_caret();
        let token_len = completion_context(self.query_before_caret()).1.len();
        let replacement = if candidate.kind == CandidateKind::Function {
            format!("{}(", candidate.text)
        } else {
            candidate.text.clone()
        };
        self.active_query_mut()
            .replace_range(caret - token_len..caret, &replacement);
        self.query_cursor = caret - token_len + replacement.len();
        // The new token before the caret (empty after a `(`, or the full name) may still have
        // suggestions.
        self.refresh_completion();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Route;
    use crate::testutil::{app_with_discovered_dims, logs_app_with_labels};

    #[test]
    fn accepting_a_function_appends_a_paren_and_a_metric_does_not() {
        let mut app = App::new();
        app.route = Route::Metrics;
        app.mode = Mode::Editing;
        app.metric_names = vec!["http_requests_total".to_owned()];

        // Function completion appends `(`.
        app.set_active_query("rat");
        app.refresh_completion();
        app.accept_completion();
        assert_eq!(app.active_query(), "rate(");

        // Metric completion replaces the token verbatim.
        app.set_active_query("rate(htt");
        app.refresh_completion();
        app.accept_completion();
        assert_eq!(app.active_query(), "rate(http_requests_total");
    }

    #[test]
    fn completion_is_suppressed_outside_edit_mode_and_for_exact_matches() {
        let mut app = App::new();
        app.route = Route::Metrics;
        // Not editing -> never suggest.
        app.set_active_query("rat");
        app.refresh_completion();
        assert!(app.completion.is_none());

        // Editing, but the token already equals the only candidate -> nothing more to offer.
        app.mode = Mode::Editing;
        app.set_active_query("rate");
        app.refresh_completion();
        assert!(app.completion.is_none());
    }

    #[test]
    fn completion_offers_label_names_inside_a_matcher() {
        let mut app = app_with_discovered_dims();
        app.set_active_query("http_requests_total{s");
        app.refresh_completion();
        let completion = app.completion.as_ref().expect("label-name candidates");
        let texts = completion
            .candidates
            .iter()
            .map(|c| c.text.as_str())
            .collect::<Vec<_>>();
        assert_eq!(texts, vec!["service"]); // only the label starting with `s`
        assert!(
            completion
                .candidates
                .iter()
                .all(|c| c.kind == CandidateKind::Label)
        );
    }

    #[test]
    fn completion_offers_label_values_inside_a_quoted_matcher() {
        let mut app = app_with_discovered_dims();
        app.set_active_query("http_requests_total{service=\"c");
        app.refresh_completion();
        let completion = app.completion.as_ref().expect("label-value candidates");
        let texts = completion
            .candidates
            .iter()
            .map(|c| c.text.as_str())
            .collect::<Vec<_>>();
        // Both values start with `c`, sorted, tagged as label values.
        assert_eq!(texts, vec!["cart", "checkout"]);
        assert!(
            completion
                .candidates
                .iter()
                .all(|c| c.kind == CandidateKind::LabelValue)
        );

        // Accepting replaces just the partial value.
        app.accept_completion();
        assert_eq!(app.active_query(), "http_requests_total{service=\"cart");
    }

    #[test]
    fn completion_requests_dims_for_an_undiscovered_metric_once() {
        let mut app = app_with_discovered_dims();
        // Reset the metric to "not discovered yet".
        app.metric_tree[0].dims = None;
        app.set_active_query("http_requests_total{s");
        // No label vocabulary yet -> no popup, but a discovery request is emitted exactly once.
        app.refresh_completion();
        assert!(app.completion.is_none());
        assert_eq!(
            app.completion_dim_request(),
            Some("http_requests_total".to_owned())
        );
        // Marked loading now, so it does not fire again.
        assert_eq!(app.completion_dim_request(), None);
    }

    #[test]
    fn logs_expression_popup_opens_on_an_empty_token() {
        // Unlike Metrics/Traces (whose Expr vocabulary is large and waits for input), the Logs box's
        // short operator-hint list pops immediately after a selector.
        let mut app = logs_app_with_labels();
        app.set_active_query("{}");
        app.refresh_completion();
        let completion = app.completion.as_ref().expect("operator-hint popup");
        assert!(
            completion
                .candidates
                .iter()
                .any(|c| c.kind == CandidateKind::Operator && c.text == "|?")
        );
    }

    #[test]
    fn completion_offers_log_label_names_inside_a_matcher() {
        let mut app = logs_app_with_labels();
        app.set_active_query("{h");
        app.refresh_completion();
        let completion = app.completion.as_ref().expect("log label-name candidates");
        let texts = completion
            .candidates
            .iter()
            .map(|c| c.text.as_str())
            .collect::<Vec<_>>();
        // Only the labels starting with `h`, sorted, tagged as label names.
        assert_eq!(texts, vec!["host", "http.method"]);
        assert!(
            completion
                .candidates
                .iter()
                .all(|c| c.kind == CandidateKind::Label)
        );
    }

    #[test]
    fn completion_offers_log_label_values_inside_a_quoted_matcher() {
        let mut app = logs_app_with_labels();
        app.set_active_query("{service.name=\"c");
        app.refresh_completion();
        let completion = app.completion.as_ref().expect("log label-value candidates");
        let texts = completion
            .candidates
            .iter()
            .map(|c| c.text.as_str())
            .collect::<Vec<_>>();
        assert_eq!(texts, vec!["cart", "checkout"]);
        assert!(
            completion
                .candidates
                .iter()
                .all(|c| c.kind == CandidateKind::LabelValue)
        );
        // Accepting replaces just the partial value.
        app.accept_completion();
        assert_eq!(app.active_query(), "{service.name=\"cart");
    }

    #[test]
    fn completion_requests_log_labels_once_when_undiscovered() {
        let mut app = App::new();
        app.route = Route::Logs;
        app.mode = Mode::Editing;
        // No label vocabulary discovered yet.
        app.set_active_query("{s");
        app.refresh_completion();
        assert!(app.completion.is_none(), "no vocabulary -> no popup yet");
        assert_eq!(
            app.completion_log_request(),
            Some(LogCompletionRequest::Labels)
        );
        // Marked loading now, so it does not fire again.
        assert_eq!(app.completion_log_request(), None);

        // Once the names arrive, the same caret position fills the popup in.
        app.log_labels = Some(vec!["service.name".to_owned()]);
        app.log_labels_loading = false;
        app.refresh_completion();
        let completion = app.completion.as_ref().expect("labels now available");
        assert_eq!(completion.candidates[0].text, "service.name");
    }

    #[test]
    fn completion_requests_log_label_values_once_per_key() {
        let mut app = App::new();
        app.route = Route::Logs;
        app.mode = Mode::Editing;
        app.log_labels = Some(vec!["service.name".to_owned()]);
        // In a quoted value for an as-yet-undiscovered key.
        app.set_active_query("{service.name=\"c");
        app.refresh_completion();
        assert!(app.completion.is_none(), "no values yet -> no popup");
        assert_eq!(
            app.completion_log_request(),
            Some(LogCompletionRequest::Values("service.name".to_owned()))
        );
        // Marked loading, so the same key does not fire again.
        assert_eq!(app.completion_log_request(), None);
    }
}

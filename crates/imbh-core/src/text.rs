//! The shared full-text tokenizer and `matches` semantics (ARCHITECTURE.md §8/§9.2).
//!
//! This is the single standalone tokenizer the plan requires: `imbh-index` wraps it into a
//! Tantivy analyzer for the per-segment index, and `imbh-query`'s row-wise `matches` fallback
//! calls it directly, so buffer results and sealed-segment results are byte-identical. Keeping
//! it here in `imbh-core` (it is pure — no Tantivy) lets both crates share one definition
//! without a sibling dependency.
//!
//! Tokenization: lowercase, split on any non-alphanumeric character. Observability tokens are
//! identifiers, not prose, so there is no stemming and no stopword removal.

/// Split `text` into lowercased alphanumeric tokens.
pub fn tokenize(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for c in text.chars() {
        if c.is_alphanumeric() {
            for lc in c.to_lowercase() {
                cur.push(lc);
            }
        } else if !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// `matches(haystack, query)` semantics (ARCHITECTURE.md §9.2): tokenized-term containment. The row
/// matches iff **every** token of `query` appears as a token of `haystack` (implicit AND). An
/// empty query imposes no constraint (matches everything). Boolean/phrase query syntax is a
/// later refinement; index and fallback share this definition so they never disagree.
pub fn matches_terms(haystack: &str, query: &str) -> bool {
    let terms = tokenize(query);
    if terms.is_empty() {
        return true;
    }
    let tokens: std::collections::HashSet<String> = tokenize(haystack).into_iter().collect();
    terms.iter().all(|t| tokens.contains(t))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_and_lowercases() {
        assert_eq!(
            tokenize("Connection ERROR: timeout!"),
            ["connection", "error", "timeout"]
        );
        assert_eq!(tokenize("http.route=/cart"), ["http", "route", "cart"]);
        assert_eq!(tokenize(""), Vec::<String>::new());
    }

    #[test]
    fn matches_is_term_containment_and() {
        assert!(matches_terms("connection error timeout", "error"));
        assert!(matches_terms("connection error timeout", "timeout error")); // AND, order-free
        assert!(!matches_terms("connection error timeout", "refused"));
        assert!(!matches_terms("request ok", "error"));
        // Substring is NOT a match: "err" is a distinct token from "error".
        assert!(!matches_terms("error", "err"));
        // Empty query imposes no constraint.
        assert!(matches_terms("anything", ""));
    }
}

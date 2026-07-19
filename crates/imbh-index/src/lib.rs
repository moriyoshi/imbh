//! imbh full-text/term search — the Tantivy integration (ARCHITECTURE.md §8).
//!
//! One Tantivy index per sealed `logs`/`spans` segment, aligned to Parquet rows via a `row`
//! u64 fast field (the row ordinal — stored as data, never assumed from doc order). Nothing is
//! stored in Tantivy (empty docstore); Parquet is the store and the index is purely a
//! row-pruning accelerator.
//!
//! The body field is tokenized with [`imbh_core::tokenize`] wrapped as a Tantivy analyzer, so
//! index hits and the row-wise `matches` fallback in `imbh-query` return identical row sets.
//!
//! Schema: `body` (tokenized) + `service`/`severity_text` (raw) + `attrs` (a JSON field of the
//! row's string-valued attributes, indexed verbatim) + `row` (fast). `search_body` returns sorted
//! row ordinals for a body term query; `search_attr_eq` does the same for an `attrs.<key> = <value>`
//! exact match, so the query layer can push attribute equalities through the cost-gated
//! `RowSelection` bridge (§8/§9.2) just like `matches(body, …)`.

use std::collections::BTreeMap;
use std::path::Path;

use imbh_core::{Attributes, Error, LogRow, Result, SpanRow};

use tantivy::collector::DocSetCollector;
use tantivy::merge_policy::NoMergePolicy;
use tantivy::query::{AllQuery, BooleanQuery, Occur, Query, TermQuery};
use tantivy::schema::{
    FAST, Field, IndexRecordOption, JsonObjectOptions, OwnedValue, STRING, Schema,
    TextFieldIndexing, TextOptions,
};
use tantivy::tokenizer::{Token, TokenStream, Tokenizer};
use tantivy::{Index, TantivyDocument, Term};

/// Tantivy `IndexWriter` heap budget, single-threaded (embedded default, ARCHITECTURE.md §8).
const WRITER_HEAP: usize = 32_000_000;

/// The registered name of imbh's shared tokenizer inside a Tantivy index.
const TOKENIZER: &str = "imbh";

/// Tantivy's built-in verbatim tokenizer (registered by default), used for the `attrs` JSON field
/// so attribute string values are indexed exactly — an attribute equality must match byte-for-byte,
/// not be word-tokenized like the `body` field.
const RAW_TOKENIZER: &str = "raw";

/// Field handles for the `logs` index schema.
struct Fields {
    body: Field,
    service: Field,
    severity_text: Field,
    attrs: Field,
    row: Field,
}

fn logs_index_schema() -> (Schema, Fields) {
    let mut sb = Schema::builder();
    let body_opts = TextOptions::default().set_indexing_options(
        TextFieldIndexing::default()
            .set_tokenizer(TOKENIZER)
            // Freqs on (for term-frequency cost estimation, §8), positions off (smaller index).
            .set_index_option(IndexRecordOption::WithFreqs),
    );
    let body = sb.add_text_field("body", body_opts);
    let service = sb.add_text_field("service", STRING);
    let severity_text = sb.add_text_field("severity_text", STRING);
    // The `attrs` JSON field carries each row's string-valued attributes as `attrs.<key> = <value>`
    // pairs, indexed with the `raw` tokenizer (verbatim, no word-splitting or case-folding) so an
    // `attrs.<key>` term query is an exact-equality lookup — the pushdown for `json_get_str(attributes,
    // '<key>') = '<value>'` (ARCHITECTURE.md §8/§9.2). `expand_dots` stays disabled: a flat attribute
    // key with dots (e.g. `http.route`) is one literal path segment, not a nested path.
    let attrs_opts = JsonObjectOptions::default().set_indexing_options(
        TextFieldIndexing::default()
            .set_tokenizer(RAW_TOKENIZER)
            .set_index_option(IndexRecordOption::Basic),
    );
    let attrs = sb.add_json_field("attrs", attrs_opts);
    let row = sb.add_u64_field("row", FAST);
    (
        sb.build(),
        Fields {
            body,
            service,
            severity_text,
            attrs,
            row,
        },
    )
}

fn register_tokenizer(index: &Index) {
    index.tokenizers().register(TOKENIZER, ImbhTokenizer);
}

/// One document to index: the tokenized `body` text (log body or span name), optional raw
/// `service`/`severity_text`, the row's canonical-JSON `attributes` string (indexed into the
/// `attrs` JSON field), and the Parquet row ordinal.
struct IndexDoc<'a> {
    body: &'a str,
    service: Option<&'a str>,
    severity_text: Option<&'a str>,
    attributes: &'a str,
    row: u64,
}

/// Build a Tantivy index into `index_dir` (created if absent) from a stream of [`IndexDoc`]s. The
/// `row` fast field indexes straight back into the Parquet file, so callers must yield docs in
/// Parquet row order. Shared by [`build_logs_index`] and [`build_spans_index`].
fn build_text_index<'a>(index_dir: &Path, docs: impl Iterator<Item = IndexDoc<'a>>) -> Result<()> {
    std::fs::create_dir_all(index_dir).map_err(|e| Error::storage_ctx("create index dir", e))?;
    let (schema, fields) = logs_index_schema();
    let index = Index::create_in_dir(index_dir, schema)
        .map_err(|e| Error::storage_ctx("create tantivy index", e))?;
    register_tokenizer(&index);

    let mut writer = index
        .writer_with_num_threads(1, WRITER_HEAP)
        .map_err(|e| Error::storage_ctx("tantivy writer", e))?;
    // Write-once index: build in a single commit, rebuilt wholesale on compaction — never merged
    // incrementally. Tantivy's default merge policy would spawn its own background merge threads
    // (against imbh's no-background-threads guarantee), whose in-flight work `Drop` then *kills*,
    // leaving abandoned partial-merge files (no Tantivy GC runs). `NoMergePolicy` spawns no merge
    // thread at all, so `Drop` is trivially clean and no seal blocks on a merge; search reads across
    // whatever sub-segments the single commit flushed. (If single-segment indexes are ever wanted
    // for large segments, do one *synchronous* `writer.merge(&ids)` here — no background thread.)
    writer.set_merge_policy(Box::new(NoMergePolicy));
    for d in docs {
        let mut doc = TantivyDocument::new();
        doc.add_text(fields.body, d.body);
        if let Some(s) = d.service {
            doc.add_text(fields.service, s);
        }
        if let Some(s) = d.severity_text {
            doc.add_text(fields.severity_text, s);
        }
        // Index each string-valued attribute as `attrs.<key> = <value>`. Only `Str` values are
        // indexed: `json_get_str` (the pushed predicate) returns a value only for string attributes,
        // so numbers/bools/etc. can never satisfy an attr-equality and are left out of the index.
        let attrs = Attributes::from_canonical_json(d.attributes);
        let obj: BTreeMap<String, OwnedValue> = attrs
            .iter()
            .filter_map(|(k, v)| {
                v.as_str()
                    .map(|s| (k.to_owned(), OwnedValue::Str(s.to_owned())))
            })
            .collect();
        if !obj.is_empty() {
            doc.add_object(fields.attrs, obj);
        }
        doc.add_u64(fields.row, d.row);
        writer
            .add_document(doc)
            .map_err(|e| Error::storage_ctx("tantivy add_document", e))?;
    }
    writer
        .commit()
        .map_err(|e| Error::storage_ctx("tantivy commit", e))?;
    // No `wait_merging_threads` needed: `NoMergePolicy` above means there is no merge thread to
    // await, and `Drop` cleanly joins the single indexing worker.
    Ok(())
}

/// Build a Tantivy index for a sealed `logs` segment (indexes the tokenized `body`). The row
/// ordinal is the row's position in `rows` = Parquet row order.
#[cfg_attr(
    feature = "tracing",
    tracing::instrument(level = "debug", name = "index.build_logs", skip_all, fields(rows = rows.len()))
)]
pub fn build_logs_index(index_dir: &Path, rows: &[LogRow]) -> Result<()> {
    build_text_index(
        index_dir,
        rows.iter().enumerate().map(|(ordinal, r)| IndexDoc {
            body: r.body.as_str(),
            service: r.service.as_deref(),
            severity_text: r.severity_text.as_deref(),
            attributes: r.attributes.as_str(),
            row: ordinal as u64,
        }),
    )
}

/// Build a Tantivy index for a sealed `spans` segment (indexes the tokenized span `name` into the
/// same `body` field, so [`search_body`] serves both). Row ordinal = position in `spans`.
#[cfg_attr(
    feature = "tracing",
    tracing::instrument(level = "debug", name = "index.build_spans", skip_all, fields(spans = spans.len()))
)]
pub fn build_spans_index(index_dir: &Path, spans: &[SpanRow]) -> Result<()> {
    build_text_index(
        index_dir,
        spans.iter().enumerate().map(|(ordinal, s)| IndexDoc {
            body: s.name.as_str(),
            service: s.service.as_deref(),
            severity_text: None,
            attributes: s.attributes.as_str(),
            row: ordinal as u64,
        }),
    )
}

/// Search a `logs` index's `body` field for rows containing **all** `terms` (implicit AND,
/// matching [`imbh_core::matches_terms`]). Returns sorted, deduplicated row ordinals. Empty
/// `terms` returns every row (no constraint), consistent with the fallback semantics.
#[cfg_attr(
    feature = "tracing",
    tracing::instrument(level = "debug", name = "index.search_body", skip_all, fields(terms = terms.len()))
)]
pub fn search_body(index_dir: &Path, terms: &[String]) -> Result<Vec<u64>> {
    search_body_bool(index_dir, terms, &[])
}

/// Boolean body search: the rows containing **all** `must` terms and **none** of the `must_not`
/// terms (`+must -must_not`) — the pushdown twin of `matches(body, …) AND NOT matches(body, …)`, so
/// a chain of the imbh dialect's `|?` / `!?` operators reduces to a single Tantivy query. Empty
/// `must` with some `must_not` matches every row except those a must-not term hits (a match-all base
/// with the excluded terms subtracted); both empty returns every row. Sorted, deduplicated row
/// ordinals, aligned to Parquet row order exactly like [`search_body`].
#[cfg_attr(
    feature = "tracing",
    tracing::instrument(level = "debug", name = "index.search_body_bool", skip_all, fields(must = must.len(), must_not = must_not.len()))
)]
pub fn search_body_bool(
    index_dir: &Path,
    must: &[String],
    must_not: &[String],
) -> Result<Vec<u64>> {
    let index =
        Index::open_in_dir(index_dir).map_err(|e| Error::query_ctx("open tantivy index", e))?;
    register_tokenizer(&index);
    let body = index
        .schema()
        .get_field("body")
        .map_err(|e| Error::query_ctx("index schema", e))?;

    let reader = index
        .reader()
        .map_err(|e| Error::query_ctx("tantivy reader", e))?;
    let searcher = reader.searcher();

    if must.is_empty() && must_not.is_empty() {
        return Ok((0..searcher.num_docs()).collect());
    }

    let mut clauses: Vec<(Occur, Box<dyn Query>)> = Vec::new();
    // A Tantivy `BooleanQuery` matches nothing without a positive clause, so a pure-negation query
    // (`!?` only) starts from every document and subtracts the excluded terms.
    if must.is_empty() {
        clauses.push((Occur::Must, Box::new(AllQuery)));
    }
    for t in must {
        let term = Term::from_field_text(body, t);
        clauses.push((
            Occur::Must,
            Box::new(TermQuery::new(term, IndexRecordOption::Basic)),
        ));
    }
    for t in must_not {
        let term = Term::from_field_text(body, t);
        clauses.push((
            Occur::MustNot,
            Box::new(TermQuery::new(term, IndexRecordOption::Basic)),
        ));
    }
    let query = BooleanQuery::new(clauses);
    let rows = collect_hit_rows(&searcher, &query)?;
    #[cfg(feature = "tracing")]
    tracing::debug!(hits = rows.len(), "body bool search");
    Ok(rows)
}

/// Search the `attrs` JSON field for rows whose attribute `key` has the exact string `value` —
/// the pushdown twin of `json_get_str(attributes, '<key>') = '<value>'` (ARCHITECTURE.md §8/§9.2).
/// Returns sorted, deduplicated row ordinals aligned to Parquet row order, exactly like
/// [`search_body`], so the two intersect cleanly for a combined body+attr `RowSelection`. Only
/// string-valued attributes were indexed (see [`build_text_index`]), matching `json_get_str`'s
/// string-only semantics, so this returns precisely the rows the SQL predicate would keep.
#[cfg_attr(
    feature = "tracing",
    tracing::instrument(level = "debug", name = "index.search_attr_eq", skip_all, fields(key = key))
)]
pub fn search_attr_eq(index_dir: &Path, key: &str, value: &str) -> Result<Vec<u64>> {
    let index =
        Index::open_in_dir(index_dir).map_err(|e| Error::query_ctx("open tantivy index", e))?;
    register_tokenizer(&index);
    let attrs = index
        .schema()
        .get_field("attrs")
        .map_err(|e| Error::query_ctx("index schema", e))?;

    let reader = index
        .reader()
        .map_err(|e| Error::query_ctx("tantivy reader", e))?;
    let searcher = reader.searcher();

    // The path is escaped so a literal dot in the key (e.g. `http.route`) stays a single path
    // segment — `from_field_json_path` runs `split_json_path`, which treats an unescaped `.` as a
    // separator, whereas `add_object` indexed the flat key verbatim (expand_dots disabled). The
    // string type marker + raw value then reproduce exactly the term the raw tokenizer wrote.
    let mut term = Term::from_field_json_path(attrs, &escape_json_path(key), false);
    term.append_type_and_str(value);
    let query = TermQuery::new(term, IndexRecordOption::Basic);
    let rows = collect_hit_rows(&searcher, &query)?;
    #[cfg(feature = "tracing")]
    tracing::debug!(hits = rows.len(), "attr search");
    Ok(rows)
}

/// Backslash-escape `\` and `.` in an attribute key so Tantivy's `split_json_path` yields it as a
/// single literal path segment (mirroring how `add_object` indexed the flat key with `expand_dots`
/// disabled). Without this, a dotted key like `http.route` would query a two-segment nested path
/// and miss the row.
fn escape_json_path(key: &str) -> String {
    let mut out = String::with_capacity(key.len());
    for c in key.chars() {
        if c == '\\' || c == '.' {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Run `query` against `searcher` and return its hits' `row` fast-field ordinals, sorted and
/// deduplicated. Shared by [`search_body`] and [`search_attr_eq`] so both align to Parquet row
/// order identically.
fn collect_hit_rows(searcher: &tantivy::Searcher, query: &dyn Query) -> Result<Vec<u64>> {
    let hits = searcher
        .search(query, &DocSetCollector)
        .map_err(Error::query_search)?;

    // Read each hit's `row` fast field, per segment.
    let row_readers: Vec<_> = searcher
        .segment_readers()
        .iter()
        .map(|sr| sr.fast_fields().u64("row"))
        .collect::<std::result::Result<_, _>>()
        .map_err(|e| Error::query_ctx("open row fast field", e))?;

    let mut rows = Vec::with_capacity(hits.len());
    for addr in hits {
        let col: &tantivy::columnar::Column<u64> = &row_readers[addr.segment_ord as usize];
        // Every indexed doc is written with a `row` value, so `first` is always `Some`. Treat a
        // missing value as an error rather than silently dropping the hit: this is the ground-truth
        // row set the `RowSelection` bridge trusts, and a dropped hit would be an unsound *subset*
        // (rows silently missing from results) that the UDF re-filter cannot recover.
        let v = col
            .first(addr.doc_id)
            .ok_or_else(|| Error::search_missing_row(u64::from(addr.doc_id)))?;
        rows.push(v);
    }
    rows.sort_unstable();
    rows.dedup();
    Ok(rows)
}

/// imbh's shared tokenizer as a Tantivy analyzer — delegates to [`imbh_core::tokenize`] so the
/// index and the row-wise fallback tokenize identically (ARCHITECTURE.md §8).
#[derive(Clone)]
struct ImbhTokenizer;

struct ImbhTokenStream {
    tokens: Vec<Token>,
    index: usize,
}

impl Tokenizer for ImbhTokenizer {
    type TokenStream<'a> = ImbhTokenStream;

    fn token_stream<'a>(&'a mut self, text: &'a str) -> ImbhTokenStream {
        let tokens = imbh_core::tokenize(text)
            .into_iter()
            .enumerate()
            .map(|(position, text)| Token {
                offset_from: 0,
                offset_to: 0,
                position,
                text,
                position_length: 1,
            })
            .collect();
        ImbhTokenStream { tokens, index: 0 }
    }
}

impl TokenStream for ImbhTokenStream {
    fn advance(&mut self) -> bool {
        if self.index < self.tokens.len() {
            self.index += 1;
            true
        } else {
            false
        }
    }

    fn token(&self) -> &Token {
        &self.tokens[self.index - 1]
    }

    fn token_mut(&mut self) -> &mut Token {
        &mut self.tokens[self.index - 1]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(body: &str) -> LogRow {
        row_attr(body, "{}")
    }

    fn row_attr(body: &str, attributes: &str) -> LogRow {
        LogRow {
            time_unix_nano: 0,
            observed_time_unix_nano: None,
            service: Some("svc".to_owned()),
            severity_number: 9,
            severity_text: Some("INFO".to_owned()),
            body: body.to_owned(),
            attributes: attributes.to_owned(),
            resource: "{}".to_owned(),
            scope: "{}".to_owned(),
            trace_id: None,
            span_id: None,
            flags: 0,
        }
    }

    #[test]
    fn search_finds_term_rows() {
        let dir = tempfile::tempdir().unwrap();
        let rows = [
            row("connection error timeout"),
            row("request ok"),
            row("db connection refused"),
        ];
        build_logs_index(dir.path(), &rows).unwrap();

        let hits = search_body(dir.path(), &["connection".to_owned()]).unwrap();
        assert_eq!(hits, vec![0, 2]);

        let both = search_body(dir.path(), &["connection".to_owned(), "error".to_owned()]).unwrap();
        assert_eq!(both, vec![0]);

        let none = search_body(dir.path(), &["absent".to_owned()]).unwrap();
        assert!(none.is_empty());
    }

    /// The §8 guarantee: the Tantivy index and the row-wise fallback return identical row sets.
    #[test]
    fn index_matches_rowwise_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let bodies = [
            "connection error to checkout",
            "upstream timeout",
            "request ok",
            "error error error",
            "checkout connection ok",
        ];
        let rows: Vec<LogRow> = bodies.iter().map(|b| row(b)).collect();
        build_logs_index(dir.path(), &rows).unwrap();

        for query in [
            "error",
            "connection",
            "checkout connection",
            "timeout",
            "absent",
            "ok",
        ] {
            let terms = imbh_core::tokenize(query);
            let via_index = search_body(dir.path(), &terms).unwrap();
            let via_fallback: Vec<u64> = bodies
                .iter()
                .enumerate()
                .filter(|(_, b)| imbh_core::matches_terms(b, query))
                .map(|(i, _)| i as u64)
                .collect();
            assert_eq!(via_index, via_fallback, "query `{query}` diverged");
        }
    }

    #[test]
    fn attr_eq_finds_matching_rows() {
        let dir = tempfile::tempdir().unwrap();
        let rows = [
            row_attr("a", r#"{"env":"prod","http.route":"/cart"}"#),
            row_attr("b", r#"{"env":"staging","http.route":"/cart"}"#),
            row_attr("c", r#"{"env":"prod","http.route":"/pay"}"#),
        ];
        build_logs_index(dir.path(), &rows).unwrap();

        // Plain key.
        assert_eq!(
            search_attr_eq(dir.path(), "env", "prod").unwrap(),
            vec![0, 2]
        );
        // Dotted key: the escape keeps `http.route` a single path segment.
        assert_eq!(
            search_attr_eq(dir.path(), "http.route", "/cart").unwrap(),
            vec![0, 1]
        );
        // Value present but on a different key → no match; absent value → no match.
        assert!(
            search_attr_eq(dir.path(), "env", "/cart")
                .unwrap()
                .is_empty()
        );
        assert!(
            search_attr_eq(dir.path(), "missing", "prod")
                .unwrap()
                .is_empty()
        );
    }

    /// Non-string attribute values are not indexed, matching `json_get_str`'s string-only semantics:
    /// `{"n":3}` never satisfies `json_get_str(attributes,'n') = '3'`.
    #[test]
    fn attr_eq_ignores_non_string_values() {
        let dir = tempfile::tempdir().unwrap();
        let rows = [
            row_attr("a", r#"{"n":3,"ok":true}"#),
            row_attr("b", r#"{"n":"3"}"#),
        ];
        build_logs_index(dir.path(), &rows).unwrap();
        // The int `3` is not indexed; only the string `"3"` in row 1 matches.
        assert_eq!(search_attr_eq(dir.path(), "n", "3").unwrap(), vec![1]);
        assert!(search_attr_eq(dir.path(), "ok", "true").unwrap().is_empty());
    }

    /// The §8 guarantee for attributes: the `attrs` index and the row-wise `json_get_str`-equality
    /// fallback (parse the canonical JSON, compare the string value) return identical row sets.
    #[test]
    fn attr_eq_index_matches_rowwise_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let attrs = [
            r#"{"env":"prod","http.route":"/cart","code":"200"}"#,
            r#"{"env":"staging","http.route":"/cart"}"#,
            r#"{"env":"prod","http.route":"/pay","code":"500"}"#,
            r#"{"env":"prod"}"#,
            r#"{"code":"200","env":"prod"}"#,
        ];
        let rows: Vec<LogRow> = attrs.iter().map(|a| row_attr("body", a)).collect();
        build_logs_index(dir.path(), &rows).unwrap();

        for (key, value) in [
            ("env", "prod"),
            ("env", "staging"),
            ("http.route", "/cart"),
            ("http.route", "/pay"),
            ("code", "200"),
            ("code", "404"),
            ("missing", "x"),
        ] {
            let via_index = search_attr_eq(dir.path(), key, value).unwrap();
            let via_fallback: Vec<u64> = attrs
                .iter()
                .enumerate()
                .filter(|(_, a)| Attributes::from_canonical_json(a).get_str(key) == Some(value))
                .map(|(i, _)| i as u64)
                .collect();
            assert_eq!(via_index, via_fallback, "attr `{key}={value}` diverged");
        }
    }
}

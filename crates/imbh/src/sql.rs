//! Bind-parameter collector for the typed query builders (ARCHITECTURE.md §9/§10).
//!
//! User-supplied values — service/metric names, attribute keys and values, regex patterns, time
//! bounds — must never be interpolated into SQL text. They go through [`SqlParams`], which hands
//! out a DataFusion `$N` placeholder and stores the typed value; the values are bound at execution
//! via `DataFrame::with_param_values`. Only fixed-vocabulary identifiers (table / column / alias /
//! aggregate-function names, `LIMIT`/`OFFSET`, `step` bucket arithmetic) are still interpolated —
//! those are never user input, so they carry no injection surface.

use datafusion::scalar::ScalarValue;

/// The built-in column an attribute key resolves to, if any.
///
/// `service` is a first-class column on every signal table, lifted out of the OTel *resource* at
/// ingest — it is never a record `attributes` entry. So a group/filter key naming it must resolve to
/// the column: routing it through `json_get_str(attributes, …)` yields NULL for every row, which is
/// silent rather than loud (a missing attribute is a legitimate NULL), and collapses a group-by into
/// a single empty-labelled series with all counts merged. Both spellings resolve: `service` (the
/// column name, as PromQL-style selectors and `Db::attrs` label sets use it) and `service.name` (the
/// OTel semantic-convention attribute key, as OTLP and the MCP tools use it).
///
/// This wins over the configured `Promote` list. A promoted key can never shadow `service` —
/// `imbh_storage::promoted_columns` drops reserved names — and a promoted `service.name` column
/// would be built by [`lookup_promoted`](imbh_storage) over record `attributes`, i.e. all-NULL for
/// the same reason, so the built-in column is strictly the better answer for either spelling.
fn builtin_column(key: &str) -> Option<&'static str> {
    match key {
        "service" | "service.name" => Some("service"),
        _ => None,
    }
}

/// A positional bind-parameter collector. Each `bind` returns the placeholder text (`$1`, `$2`, …)
/// and appends the value; [`SqlParams::into_values`] yields the values in `$1..$N` order.
///
/// It also carries the DB's promoted attribute keys (ARCHITECTURE.md §6.1) so the SQL builders can
/// dispatch attribute access via [`SqlParams::attr_field`] without threading a separate set through
/// every `where_sql`/`label_cond`. `new()` promotes nothing (all attribute access is `json_get_str`);
/// `with_promote` records the effective promoted columns for the query.
#[derive(Default)]
pub(crate) struct SqlParams {
    values: Vec<ScalarValue>,
    /// Effective promoted column names (reserved-name collisions/duplicates already dropped by
    /// [`imbh_storage::promoted_columns`], so this matches the on-disk schema exactly).
    promoted: Vec<String>,
}

impl SqlParams {
    /// A collector that dispatches attribute access to promoted columns for `promote`. Pass `&[]` for
    /// no promotion (every attribute access is a `json_get_str` scan).
    pub(crate) fn with_promote(promote: &[String]) -> Self {
        SqlParams {
            values: Vec::new(),
            promoted: imbh_storage::promoted_columns(promote)
                .into_iter()
                .map(str::to_owned)
                .collect(),
        }
    }

    /// A `Utf8` SQL expression reading record-`attributes` key `key`: the built-in column when `key`
    /// names one ([`builtin_column`]), else a promoted-column-with-JSON-fallback `CASE` when `key` is
    /// promoted, else `json_get_str(attributes, $key)`. All three are identical in result — a promoted
    /// key also stays in the JSON blob and the column mirrors the record `attributes` scope only
    /// (§6.1) — so mixing them across one query (some keys promoted, some not) is safe. The `CAST`
    /// normalizes the dict to plain text so `= v` / `IS NOT NULL` / `matches(...)` / `coalesce(...)`
    /// compose the same regardless of branch.
    ///
    /// **Why the promoted branch is a `CASE` and not a bare column.** Promotion is not retroactive:
    /// segments sealed before `key` was promoted have no such column and are null-filled by the
    /// `coerce` schema-evolution path, so a bare `CAST("k" AS VARCHAR) = 'v'` matches *nothing* on
    /// them — a filter on a newly promoted key silently loses all history. The `CASE` falls back to
    /// the JSON blob exactly on the rows where the column is NULL, which is both the pre-promotion
    /// segments and the rows that genuinely lack the key (where JSON yields NULL too, same answer).
    ///
    /// This costs nothing on the rows that matter. DataFusion rewrites the `CASE` to a physical
    /// `CaseExpr` that evaluates the `WHEN` over the batch and then takes a whole-batch fast path when
    /// it is uniformly true or false — and batches never span segments, so a post-promotion segment
    /// takes the column arm with `json_get_str` never invoked, while a pre-promotion segment takes the
    /// JSON arm exactly as it did before promotion. Only a segment where the key is present on *some*
    /// rows pays for both, and there the JSON arm runs on the filtered remainder.
    ///
    /// The `WHEN` deliberately tests the **bare dictionary column**, not the `CAST`: the predicate is
    /// evaluated over the full batch, so casting there would materialize a `Utf8` array from the
    /// dictionary on every batch just to check nullity. `IS NOT NULL` on the dictionary is a null-
    /// buffer read.
    pub(crate) fn attr_field(&mut self, key: &str) -> String {
        if let Some(col) = builtin_column(key) {
            format!("CAST({col} AS VARCHAR)")
        } else if self.promoted.iter().any(|c| c == key) {
            let col = key.replace('"', "\"\"");
            let json = self.str(key);
            format!(
                "CASE WHEN \"{col}\" IS NOT NULL THEN CAST(\"{col}\" AS VARCHAR) \
                 ELSE json_get_str(attributes, {json}) END"
            )
        } else {
            format!("json_get_str(attributes, {})", self.str(key))
        }
    }

    /// A boolean SQL **predicate** for "record-`attributes` key `key` equals `value`" — the
    /// filter-site twin of [`attr_field`](Self::attr_field), for equality filters only. It is
    /// equivalent to `{attr_field(key)} = $value` row for row, NULLs included, but never `CAST`s a
    /// dictionary column to text: the compare runs in dictionary space, which `attr-bench` measured at
    /// 0.20–0.31 ms per 100k rows cheaper than the `CAST` (more than the JSON fallback costs).
    ///
    /// `attr_field` cannot do this itself: it returns a *value*, and a `CASE` whose arms are
    /// `Dictionary` and `Utf8` is coerced back to `Utf8`, reintroducing the cast. Projection sites
    /// (`… AS g{i}`) must keep using `attr_field`, so a result column's type does not depend on the
    /// promote set.
    ///
    /// - built-in column ([`builtin_column`]): `service = $v`.
    /// - promoted key: `(("k" IS NOT NULL AND "k" = $v) OR ("k" IS NULL AND json_get_str(attributes,
    ///   $k) = $v))` — the `attr_field` `CASE` lifted to predicate level. The two `IS [NOT] NULL`
    ///   guards make the arms mutually exclusive exactly as the `WHEN` does: a row with a non-NULL
    ///   column is decided by the column alone (it never additionally matches via the JSON), and a
    ///   row with a NULL column (a segment sealed before promotion, or a row lacking the key) falls
    ///   back to the JSON. The `IS NOT NULL` guard on the column arm is what keeps the three-valued
    ///   result identical too (a NULL-column row yields the JSON compare's own TRUE/FALSE/NULL, not
    ///   `NULL OR FALSE` = NULL), so the helper is safe even under a `NOT`. Batches never span
    ///   segments, so `AND`'s short-circuit skips `json_get_str` on every batch of a post-promotion
    ///   segment (`"k" IS NULL` is uniformly false there).
    /// - otherwise: `json_get_str(attributes, $k) = $v`, unchanged — this exact top-level shape is
    ///   what the provider's `attrs`-index `RowSelection` pushdown recognises.
    ///
    /// `value` is bound once per arm (never interpolated): each arm's `$N` is type-inferred from its
    /// own comparison (`Dictionary` vs `Utf8`), and sharing one placeholder across both would ask
    /// DataFusion to give it two types.
    pub(crate) fn attr_eq_sql(&mut self, key: &str, value: &str) -> String {
        if let Some(col) = builtin_column(key) {
            format!("{col} = {}", self.str(value))
        } else if self.promoted.iter().any(|c| c == key) {
            let col = key.replace('"', "\"\"");
            let json = self.str(key);
            let v_json = self.str(value);
            let v_col = self.str(value);
            format!(
                "((\"{col}\" IS NOT NULL AND \"{col}\" = {v_col}) OR \
                 (\"{col}\" IS NULL AND json_get_str(attributes, {json}) = {v_json}))"
            )
        } else {
            let json = self.str(key);
            format!("json_get_str(attributes, {json}) = {}", self.str(value))
        }
    }

    /// A `Float64` SQL expression reading record-`attributes` key `key` **as a number**, the numeric
    /// twin of [`attr_field`](SqlParams::attr_field) used by the `attr_gt`/`ge`/`lt`/`le` matchers. A
    /// promoted key reads the dictionary column (text) through `TRY_CAST(... AS DOUBLE)` — string-typed
    /// like every promoted column, so a numeric compare must parse it. A non-promoted key routes
    /// through the `json_get_num` UDF, which (unlike `TRY_CAST(json_get_str(...) AS DOUBLE)`) sees
    /// integer- and double-typed JSON scalars, not only numbers that arrived as strings. NULL (⇒ the
    /// comparison is false) for an absent key or a non-numeric value, matching the evaluator: the
    /// resulting predicate is a sound superset of the typed comparison.
    /// (The promoted branch is the same non-retroactive `CASE` as [`attr_field`](Self::attr_field) —
    /// see its docs for why a bare column silently loses pre-promotion segments. Note the two arms
    /// are not merely different spellings of one value here: `json_get_num` sees integer- and
    /// double-typed JSON scalars, while a promoted column is always text, so the fallback is if
    /// anything *more* capable than the column arm.)
    pub(crate) fn attr_num_field(&mut self, key: &str) -> String {
        if let Some(col) = builtin_column(key) {
            format!("TRY_CAST(CAST({col} AS VARCHAR) AS DOUBLE)")
        } else if self.promoted.iter().any(|c| c == key) {
            let col = key.replace('"', "\"\"");
            let json = self.str(key);
            format!(
                "CASE WHEN \"{col}\" IS NOT NULL THEN TRY_CAST(CAST(\"{col}\" AS VARCHAR) AS DOUBLE) \
                 ELSE json_get_num(attributes, {json}) END"
            )
        } else {
            format!("json_get_num(attributes, {})", self.str(key))
        }
    }

    /// Bind a string value, returning its `$N` placeholder.
    pub(crate) fn str(&mut self, v: impl Into<String>) -> String {
        self.bind(ScalarValue::Utf8(Some(v.into())))
    }

    /// Bind a signed-integer value (e.g. a nanosecond time bound), returning its `$N` placeholder.
    pub(crate) fn i64(&mut self, v: i64) -> String {
        self.bind(ScalarValue::Int64(Some(v)))
    }

    /// Bind raw id bytes as a `FixedSizeBinary(len)` value — the on-disk type of the `trace_id` /
    /// `span_id` columns — returning its `$N` placeholder. Comparing the column to *raw bytes*
    /// (rather than `hex(col) = '…'`) is what lets the query provider skip whole segments via their
    /// Parquet bloom filter (ARCHITECTURE.md §8); the width must match the column's exactly, since
    /// DataFusion infers the placeholder's type from the column and then type-checks the bound value.
    pub(crate) fn id_bytes(&mut self, v: &[u8]) -> String {
        self.bind(ScalarValue::FixedSizeBinary(
            v.len() as i32,
            Some(v.to_vec()),
        ))
    }

    fn bind(&mut self, v: ScalarValue) -> String {
        self.values.push(v);
        format!("${}", self.values.len())
    }

    /// The collected values, positionally matching `$1..$N`.
    pub(crate) fn into_values(self) -> Vec<ScalarValue> {
        self.values
    }
}

#[cfg(test)]
mod attr_eq_sql_tests {
    use super::*;

    fn promoted(keys: &[&str]) -> SqlParams {
        let keys: Vec<String> = keys.iter().map(|k| (*k).to_owned()).collect();
        SqlParams::with_promote(&keys)
    }

    /// The point of the helper: a promoted-key equality compares in dictionary space, so no `CAST`
    /// appears anywhere in the predicate — while keeping the JSON fallback for column-NULL rows.
    #[test]
    fn promoted_equality_has_no_cast_and_keeps_the_json_fallback() {
        let mut p = promoted(&["env"]);
        let sql = p.attr_eq_sql("env", "prod");
        assert!(!sql.contains("CAST"), "no cast in a filter: {sql}");
        assert_eq!(
            sql,
            "((\"env\" IS NOT NULL AND \"env\" = $3) OR \
             (\"env\" IS NULL AND json_get_str(attributes, $1) = $2))"
        );
        assert_eq!(
            p.into_values(),
            vec![
                ScalarValue::Utf8(Some("env".into())),
                ScalarValue::Utf8(Some("prod".into())),
                ScalarValue::Utf8(Some("prod".into())),
            ]
        );
    }

    /// The built-in column compares bare (it is a dictionary column too); a non-promoted key keeps the
    /// exact `json_get_str(attributes, $k) = $v` shape the `attrs`-index pushdown recognises, with the
    /// key bound before the value as `attr_field` + `str` did.
    #[test]
    fn builtin_and_unpromoted_keys_keep_their_shapes() {
        let mut p = promoted(&["env"]);
        assert_eq!(p.attr_eq_sql("service.name", "cart"), "service = $1");
        assert_eq!(p.attr_eq_sql("service", "cart"), "service = $2");
        assert_eq!(
            p.attr_eq_sql("http.route", "/x"),
            "json_get_str(attributes, $3) = $4"
        );
        assert_eq!(
            p.into_values()[2..],
            [
                ScalarValue::Utf8(Some("http.route".into())),
                ScalarValue::Utf8(Some("/x".into())),
            ]
        );
    }

    /// Values and keys are bound, never interpolated; the only key text that reaches the SQL is the
    /// promoted column identifier, `"`-escaped exactly as `attr_field` escapes it.
    #[test]
    fn user_input_is_bound_and_identifiers_escaped() {
        let key = "e\"nv') OR TRUE --";
        let value = "x' OR 1 = 1 --";
        let mut p = promoted(&[key]);
        let sql = p.attr_eq_sql(key, value);
        assert!(!sql.contains(value), "{sql}");
        assert!(sql.contains("\"e\"\"nv') OR TRUE --\""), "{sql}");
        let mut p = promoted(&[]);
        let sql = p.attr_eq_sql(key, value);
        assert!(!sql.contains(key) && !sql.contains(value), "{sql}");
    }

    /// Typed query builders route their equality filters through the helper: the full `WHERE` of a
    /// logs query filtering a promoted key by `attr_eq` and by a LogQL-style `=` has no `CAST`.
    #[test]
    fn log_equality_filters_on_a_promoted_key_compile_without_cast() {
        use crate::logs::{LogQuery, LogStringField, StringPredicate};
        let q = LogQuery::new()
            .attr_eq("env", "prod")
            .attr_eq("service.name", "cart")
            .string_predicate(
                LogStringField::Attribute("env".into()),
                StringPredicate::Eq,
                "prod",
            )
            .string_predicate(LogStringField::Service, StringPredicate::Eq, "cart");
        let mut p = promoted(&["env"]);
        let sql = q.where_sql(&mut p);
        assert!(!sql.contains("CAST"), "{sql}");
        assert!(sql.contains("\"env\" = $"), "{sql}");
        // `= ""` must also match a missing key, so it keeps the coalesce (and its cast).
        let q = LogQuery::new().string_predicate(
            LogStringField::Attribute("env".into()),
            StringPredicate::Eq,
            "",
        );
        let sql = q.where_sql(&mut promoted(&["env"]));
        assert!(sql.contains("coalesce("), "{sql}");
    }
}

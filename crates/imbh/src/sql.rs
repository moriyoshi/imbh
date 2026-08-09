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

//! Bind-parameter collector for the typed query builders (ARCHITECTURE.md §9/§10).
//!
//! User-supplied values — service/metric names, attribute keys and values, regex patterns, time
//! bounds — must never be interpolated into SQL text. They go through [`SqlParams`], which hands
//! out a DataFusion `$N` placeholder and stores the typed value; the values are bound at execution
//! via `DataFrame::with_param_values`. Only fixed-vocabulary identifiers (table / column / alias /
//! aggregate-function names, `LIMIT`/`OFFSET`, `step` bucket arithmetic) are still interpolated —
//! those are never user input, so they carry no injection surface.

use datafusion::scalar::ScalarValue;

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

    /// A `Utf8` SQL expression reading record-`attributes` key `key`: the promoted dictionary column
    /// cast to `VARCHAR` (exactly how `service` is read) when `key` is promoted, else
    /// `json_get_str(attributes, $key)`. Both forms are identical in result — a promoted key also
    /// stays in the JSON blob and the column mirrors the record `attributes` scope only (§6.1) — so
    /// mixing them across one query (some keys promoted, some not) is safe. The `CAST` normalizes the
    /// dict to plain text so `= v` / `IS NOT NULL` / `matches(...)` / `coalesce(...)` compose the same
    /// regardless of branch.
    pub(crate) fn attr_field(&mut self, key: &str) -> String {
        if self.promoted.iter().any(|c| c == key) {
            format!("CAST(\"{}\" AS VARCHAR)", key.replace('"', "\"\""))
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
    pub(crate) fn attr_num_field(&mut self, key: &str) -> String {
        if self.promoted.iter().any(|c| c == key) {
            format!(
                "TRY_CAST(CAST(\"{}\" AS VARCHAR) AS DOUBLE)",
                key.replace('"', "\"\"")
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

    fn bind(&mut self, v: ScalarValue) -> String {
        self.values.push(v);
        format!("${}", self.values.len())
    }

    /// The collected values, positionally matching `$1..$N`.
    pub(crate) fn into_values(self) -> Vec<ScalarValue> {
        self.values
    }
}

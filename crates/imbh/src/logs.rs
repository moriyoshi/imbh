//! The typed Logs query API (ARCHITECTURE.md §10.6).
//!
//! `db.logs().query(LogQuery)` compiles a builder into SQL over the `logs` table — a thin
//! `LogicalPlan`-equivalent over the same provider (§9.4) — runs it through the query layer
//! (so it uses the `matches` UDF, the `json_get_str` UDF, and the Tantivy `RowSelection`
//! bridge), and materializes the result batches into owned [`LogEntry`] DTOs.
//!
//! Scope: `service`/`severity_at_least`/`matches`/`attr_eq`/`attr_exists`/`attr_matches`/`attr_in`/
//! `attr_not_in`/`attr_gt`/`attr_ge`/`attr_lt`/`attr_le`/`attr_regex`/`range`/`since`/`limit`/
//! `direction`, `query`, `count`, `volume`, attribute discovery, and OFFSET-based cursor paging
//! (`after`). The `MatchOp` vocabulary is complete; `tail` (live follow) is a later chunk.

use std::time::{Duration, Instant};

use arrow::array::{
    Array, DictionaryArray, FixedSizeBinaryArray, Int64Array, StringArray, StringViewArray,
    TimestampNanosecondArray, UInt8Array, UInt32Array,
};
use arrow::datatypes::Int32Type;
use arrow::record_batch::RecordBatch;

use imbh_core::{
    Attributes, Direction, DurationNs, SeverityNumber, SpanId, TimeRange, Timestamp, TraceId,
};

use std::sync::Arc;

use crate::sql::SqlParams;
use crate::{Db, Error, Result};

/// Per-signal logs query namespace, reached via [`Db::logs`].
pub struct LogsApi {
    pub(crate) db: Arc<Db>,
}

impl LogsApi {
    /// The raw Arrow result of a log query — the same scan as [`query`](Self::query) but *without*
    /// materializing `LogEntry` DTOs. Lets a caller (e.g. the LGTM log source) read only the columns
    /// it needs directly from the batch buffers, skipping the per-row JSON attribute parse. The
    /// column layout is the canonical `logs` projection (ARCHITECTURE.md §6.2 / §10.6), with any
    /// promoted attribute columns appended after `flags`.
    pub async fn query_batches(&self, q: LogQuery) -> Result<Vec<RecordBatch>> {
        let mut params = SqlParams::with_promote(self.db.storage.promote().keys());
        // `SELECT *` (not the fixed 12-column projection) so any promoted attribute columns are
        // present in the batch — that is what lets the reader take the zero-copy dictionary path
        // instead of parsing the JSON blob.
        let dir = match q.direction {
            Direction::Backward => "DESC",
            Direction::Forward => "ASC",
        };
        let offset = if q.offset > 0 {
            format!(" OFFSET {}", q.offset)
        } else {
            String::new()
        };
        let sql = format!(
            "SELECT * FROM logs{} ORDER BY \"time\" {dir} LIMIT {}{offset}",
            q.where_sql(&mut params),
            q.limit
        );
        self.db
            .sql_with_params(sql, params.into_values())
            .collect()
            .await
    }

    pub async fn query(&self, q: LogQuery) -> Result<LogPage> {
        let mut params = SqlParams::with_promote(self.db.storage.promote().keys());
        let sql = q.to_sql(&mut params);
        let started = Instant::now();
        let (_schema, batches, scan) = self
            .db
            .sql_with_params(sql, params.into_values())
            .collect_with_stats()
            .await?;
        let entries = materialize(&batches)?;
        // A full page means more rows may follow → hand back a resume cursor (offset past this page).
        let next = (entries.len() == q.limit).then(|| PageCursor(q.offset + entries.len()));
        let stats = QueryStats {
            segments_scanned: scan.segments_scanned,
            segments_pruned: scan.segments_pruned,
            rows_scanned: scan.rows_scanned,
            rows_returned: entries.len() as u64,
            bytes_scanned: scan.bytes_scanned,
            elapsed: DurationNs(started.elapsed().as_nanos() as u64),
            // `used_index` now means the `.tidx` was actually consulted (a `matches`/attr-eq pushdown
            // over a sealed segment), not merely that the query had a full-text predicate.
            used_index: scan.index_searched,
        };
        Ok(LogPage {
            entries,
            next,
            stats,
        })
    }

    /// Log volume: record counts per `step`-sized time bucket over an optional filter (the Loki
    /// metric-form / `index/volume` shape, ARCHITECTURE.md §10.6). The bucket start is
    /// `floor(time / step) * step` in epoch nanos. Buckets carry no labels — use
    /// [`volume_by`](Self::volume_by) to break the volume down by attribute keys.
    pub async fn volume(&self, filter: LogQuery, step: Duration) -> Result<Vec<VolumeBucket>> {
        self.volume_by(filter, step, &[]).await
    }

    /// Like [`volume`](Self::volume), but counts per `(step-bucket, label set)` — the volume broken
    /// down by the given attribute keys (each [`VolumeBucket`] carries its `labels`). Empty
    /// `group_by` is equivalent to `volume`.
    pub async fn volume_by(
        &self,
        filter: LogQuery,
        step: Duration,
        group_by: &[&str],
    ) -> Result<Vec<VolumeBucket>> {
        let step_ns = (step.as_nanos() as i64).max(1);
        let mut params = SqlParams::with_promote(self.db.storage.promote().keys());
        let mut select = vec![format!(
            "(CAST(\"time\" AS BIGINT) / {step_ns}) * {step_ns} AS bucket"
        )];
        let mut group = vec!["bucket".to_owned()];
        for (i, k) in group_by.iter().enumerate() {
            let field = params.attr_field(k);
            select.push(format!("{field} AS g{i}"));
            group.push(format!("g{i}"));
        }
        select.push("count(*) AS c".to_owned());
        let sql = format!(
            "SELECT {} FROM logs{} GROUP BY {} ORDER BY bucket",
            select.join(", "),
            filter.where_sql(&mut params),
            group.join(", "),
        );
        let batches = self
            .db
            .sql_with_params(sql, params.into_values())
            .collect()
            .await?;
        materialize_volume(&batches, group_by)
    }

    /// The total number of `logs` records matching `filter` — a `count(*)` that ignores
    /// `limit`/paging, for "how many match" dashboards without materializing any rows. The
    /// filter's `limit`/`direction`/`after` are irrelevant to a count and are not applied.
    pub async fn count(&self, filter: LogQuery) -> Result<u64> {
        let mut params = SqlParams::with_promote(self.db.storage.promote().keys());
        let sql = format!("SELECT count(*) FROM logs{}", filter.where_sql(&mut params));
        let batches = self
            .db
            .sql_with_params(sql, params.into_values())
            .collect()
            .await?;
        let n = batches
            .first()
            .filter(|b| b.num_rows() > 0)
            .and_then(|b| b.column(0).as_any().downcast_ref::<Int64Array>())
            .map(|c| c.value(0))
            .unwrap_or(0);
        Ok(n.max(0) as u64)
    }

    /// Run a log query and return the raw Arrow result batches (plus scan stats) **without**
    /// materializing [`LogEntry`] DTOs — the zero-copy-friendly entry point a Go / FFI binding drives
    /// (ARCHITECTURE.md §10.17). Same SQL as [`query`](Self::query), so the batches carry the canonical
    /// `logs` projection in schema order: `time, observed_time, service, severity_number,
    /// severity_text, body, attributes, resource, scope, trace_id, span_id, flags` — ordered by
    /// `time` per the query's `direction`, capped by its `limit`/`offset`.
    #[cfg(feature = "proto")]
    pub async fn query_batches_with_stats(
        &self,
        q: LogQuery,
    ) -> Result<(Vec<RecordBatch>, QueryStats)> {
        let mut params = SqlParams::with_promote(self.db.storage.promote().keys());
        let sql = q.to_sql(&mut params);
        let started = Instant::now();
        let (_schema, batches, scan) = self
            .db
            .sql_with_params(sql, params.into_values())
            .collect_with_stats()
            .await?;
        let stats = batch_query_stats(&scan, &batches, started);
        Ok((batches, stats))
    }
}

/// Build the [`QueryStats`] envelope for a batch-returning query: the provider's scan counters plus
/// the total materialized row count and elapsed time. Shared by the `*_batches` entry points across
/// the logs/traces/metrics namespaces (the Arrow side of the binding, §10.17).
#[cfg(feature = "proto")]
pub(crate) fn batch_query_stats(
    scan: &imbh_query::ScanStats,
    batches: &[RecordBatch],
    started: Instant,
) -> QueryStats {
    QueryStats {
        segments_scanned: scan.segments_scanned,
        segments_pruned: scan.segments_pruned,
        rows_scanned: scan.rows_scanned,
        rows_returned: batches.iter().map(|b| b.num_rows() as u64).sum(),
        bytes_scanned: scan.bytes_scanned,
        elapsed: DurationNs(started.elapsed().as_nanos() as u64),
        used_index: scan.index_searched,
    }
}

/// A numeric-comparison operator on an attribute value (`attr_gt`/`attr_ge`/`attr_lt`/`attr_le`).
/// A serde-friendly stand-in for the SQL operator so the query builders round-trip through JSON
/// (a bare `&'static str` cannot `Deserialize`). Shared by [`LogQuery`] and the traces query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub(crate) enum NumOp {
    Gt,
    Ge,
    Lt,
    Le,
}

impl NumOp {
    /// The SQL comparison operator this maps to.
    pub(crate) fn as_sql(self) -> &'static str {
        match self {
            NumOp::Gt => ">",
            NumOp::Ge => ">=",
            NumOp::Lt => "<",
            NumOp::Le => "<=",
        }
    }
}
/// A string-bearing field addressable by the native log query model.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum LogStringField {
    Body,
    Service,
    Attribute(String),
    ResourceAttribute(String),
}

/// Exact string predicate used by language adapters and native callers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum StringPredicate {
    Eq,
    Ne,
    Regex,
    NotRegex,
    Contains,
    NotContains,
    /// Tokenized term-AND full-text match via the `matches()` UDF (Tantivy-accelerated over a sealed
    /// segment's `.tidx` when the field is the indexed `body`). Distinct from `Contains` (substring).
    Matches,
    /// The negation of [`Matches`](Self::Matches).
    NotMatches,
}

#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
struct LogStringPredicate {
    field: LogStringField,
    op: StringPredicate,
    value: String,
}

/// A log query, built fluently (ARCHITECTURE.md §10.6).
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct LogQuery {
    service: Option<String>,
    min_severity: Option<u8>,
    text: Option<String>,
    attr_eq: Vec<(String, String)>,
    attr_exists: Vec<String>,
    attr_matches: Vec<(String, String)>,
    attr_in: Vec<(String, Vec<String>)>,
    attr_not_in: Vec<(String, Vec<String>)>,
    /// Numeric comparisons on an attribute value: `(key, operator, rhs)`.
    attr_num: Vec<(String, NumOp, f64)>,
    /// Regex match on an attribute value: `(key, pattern)`.
    attr_regex: Vec<(String, String)>,
    string_predicates: Vec<LogStringPredicate>,
    /// Exact `trace_id` correlation filter (raw binary equality). Used by trace→log drill-down.
    trace_id: Option<TraceId>,
    /// Exact `span_id` correlation filter (raw binary equality). Used by span→log drill-down.
    span_id: Option<SpanId>,
    match_none: bool,
    range_end_inclusive: bool,
    range: Option<TimeRange>,
    limit: usize,
    direction: Direction,
    offset: usize,
}

impl Default for LogQuery {
    fn default() -> Self {
        LogQuery {
            service: None,
            min_severity: None,
            text: None,
            attr_eq: Vec::new(),
            attr_exists: Vec::new(),
            attr_matches: Vec::new(),
            attr_in: Vec::new(),
            attr_not_in: Vec::new(),
            attr_num: Vec::new(),
            attr_regex: Vec::new(),
            string_predicates: Vec::new(),
            trace_id: None,
            span_id: None,
            match_none: false,
            range_end_inclusive: false,
            range: None,
            limit: 100,
            direction: Direction::Backward,
            offset: 0,
        }
    }
}

impl LogQuery {
    pub fn new() -> Self {
        Self::default()
    }

    /// Filter by `service.name` (the promoted `service` column).
    pub fn service(mut self, s: &str) -> Self {
        self.service = Some(s.to_owned());
        self
    }

    pub fn severity_at_least(mut self, s: SeverityNumber) -> Self {
        self.min_severity = Some(s.0);
        self
    }

    /// Full-text `matches` over the body (Tantivy-accelerated when sealed).
    pub fn matches(mut self, text: &str) -> Self {
        self.text = Some(text.to_owned());
        self
    }

    /// Correlate to a trace: keep only records carrying this `trace_id`. Filters on the raw binary
    /// column (`trace_id = X'…'`), the same shape [`traces().get`](crate::traces::TracesApi::get)
    /// uses, so a host can jump from a span to its logs. Repeated calls keep the last.
    pub fn trace_id(mut self, id: TraceId) -> Self {
        self.trace_id = Some(id);
        self
    }

    /// Correlate to a single span: keep only records carrying this `span_id` (raw binary equality).
    /// Combine with [`trace_id`](Self::trace_id) for a span-scoped jump. Repeated calls keep the last.
    pub fn span_id(mut self, id: SpanId) -> Self {
        self.span_id = Some(id);
        self
    }

    /// Attribute equality on a (possibly non-promoted) key, via `json_get_str`.
    pub fn attr_eq(mut self, key: &str, value: &str) -> Self {
        self.attr_eq.push((key.to_owned(), value.to_owned()));
        self
    }

    /// Keep only rows that **have** the attribute `key` (any value), via `json_get_str(...) IS NOT
    /// NULL`. Repeatable (all required).
    pub fn attr_exists(mut self, key: &str) -> Self {
        self.attr_exists.push(key.to_owned());
        self
    }

    /// Term-search an attribute value: keep rows where the value of `key` contains all terms of
    /// `text` (tokenized `matches`, like [`matches`](Self::matches) but scoped to one attribute).
    /// Repeatable.
    pub fn attr_matches(mut self, key: &str, text: &str) -> Self {
        self.attr_matches.push((key.to_owned(), text.to_owned()));
        self
    }

    /// Keep rows where the value of `key` is one of `values` (in-set match, e.g. a set of status
    /// codes or services). An empty `values` set matches nothing. Repeatable (all required).
    pub fn attr_in(mut self, key: &str, values: &[&str]) -> Self {
        self.attr_in.push((
            key.to_owned(),
            values.iter().map(|v| (*v).to_owned()).collect(),
        ));
        self
    }

    /// Exclude rows where the value of `key` is one of `values` (e.g. drop noisy routes/services).
    /// Rows that lack `key` are **kept** (their value is not in the excluded set); an empty `values`
    /// set excludes nothing. Repeatable.
    pub fn attr_not_in(mut self, key: &str, values: &[&str]) -> Self {
        self.attr_not_in.push((
            key.to_owned(),
            values.iter().map(|v| (*v).to_owned()).collect(),
        ));
        self
    }

    /// Numeric filter: keep rows where `key`'s value, parsed as a number, is `> n`. A non-numeric or
    /// missing value never matches (`TRY_CAST` → NULL). Combine with [`attr_lt`](Self::attr_lt) for a
    /// range. Repeatable.
    pub fn attr_gt(mut self, key: &str, n: f64) -> Self {
        self.attr_num.push((key.to_owned(), NumOp::Gt, n));
        self
    }
    /// Numeric filter: keep rows where `key`'s value is `>= n` (see [`attr_gt`](Self::attr_gt)).
    pub fn attr_ge(mut self, key: &str, n: f64) -> Self {
        self.attr_num.push((key.to_owned(), NumOp::Ge, n));
        self
    }
    /// Numeric filter: keep rows where `key`'s value is `< n` (see [`attr_gt`](Self::attr_gt)).
    pub fn attr_lt(mut self, key: &str, n: f64) -> Self {
        self.attr_num.push((key.to_owned(), NumOp::Lt, n));
        self
    }
    /// Numeric filter: keep rows where `key`'s value is `<= n` (see [`attr_gt`](Self::attr_gt)).
    pub fn attr_le(mut self, key: &str, n: f64) -> Self {
        self.attr_num.push((key.to_owned(), NumOp::Le, n));
        self
    }

    /// Keep rows where `key`'s value matches the regular expression `pattern` (RE2 syntax, via
    /// DataFusion's `regexp_like`). A missing value never matches. Anchor with `^`/`$` for a full
    /// match. Repeatable.
    pub fn attr_regex(mut self, key: &str, pattern: &str) -> Self {
        self.attr_regex.push((key.to_owned(), pattern.to_owned()));
        self
    }
    /// Apply an exact string predicate. Values, attribute keys, and regex patterns are bound query
    /// parameters by the executor; none are interpolated into SQL text.
    pub fn string_predicate(
        mut self,
        field: LogStringField,
        op: StringPredicate,
        value: impl Into<String>,
    ) -> Self {
        self.string_predicates.push(LogStringPredicate {
            field,
            op,
            value: value.into(),
        });
        self
    }

    /// Construct a query that deterministically returns no rows.
    pub fn match_none(mut self) -> Self {
        self.match_none = true;
        self
    }

    pub fn range(mut self, r: TimeRange) -> Self {
        self.range = Some(r);
        self.range_end_inclusive = false;
        self
    }

    /// Select a closed storage interval, including records exactly on `end`.
    pub fn range_inclusive(mut self, start: Timestamp, end: Timestamp) -> Self {
        self.range = Some(TimeRange::between(start, end));
        self.range_end_inclusive = true;
        self
    }

    pub fn since(mut self, d: Duration) -> Self {
        self.range = Some(TimeRange::since(d));
        self.range_end_inclusive = false;
        self
    }

    pub fn limit(mut self, n: usize) -> Self {
        self.limit = n;
        self
    }

    pub fn direction(mut self, d: Direction) -> Self {
        self.direction = d;
        self
    }

    /// Resume after a previous page's [`LogPage::next`] cursor. Same filters/limit/direction must be
    /// reused for the paging to be coherent.
    pub fn after(mut self, cursor: PageCursor) -> Self {
        self.offset = cursor.0;
        self
    }

    /// The ` WHERE …` fragment for this query's filters (empty when unfiltered). Shared by the
    /// row query and the volume aggregation. Each record-`attributes` access goes through
    /// [`SqlParams::attr_field`], which hits the promoted dictionary column (pushdown) when the key is
    /// promoted, else a `json_get_str` scan — identical results either way (ARCHITECTURE.md §6.1).
    pub(crate) fn where_sql(&self, p: &mut SqlParams) -> String {
        let mut conds: Vec<String> = Vec::new();
        if self.match_none {
            conds.push("FALSE".to_owned());
        }

        if let Some(r) = &self.range {
            if r.start.0 != i64::MIN {
                conds.push(format!("CAST(\"time\" AS BIGINT) >= {}", p.i64(r.start.0)));
            }
            if r.end.0 != i64::MAX {
                let operator = if self.range_end_inclusive { "<=" } else { "<" };
                conds.push(format!(
                    "CAST(\"time\" AS BIGINT) {operator} {}",
                    p.i64(r.end.0)
                ));
            }
        }
        if let Some(s) = &self.service {
            conds.push(format!("service = {}", p.str(s)));
        }
        if let Some(n) = self.min_severity {
            conds.push(format!("severity_number >= {n}"));
        }
        if let Some(t) = &self.text {
            conds.push(format!("matches(body, {})", p.str(t)));
        }
        // Correlation ids filter on the raw binary column (`X'…'`). The hex is machine-derived
        // (only `0-9a-f`), never user text, so inlining it as a binary literal is injection-safe —
        // the same pattern `traces().get` uses to enable Parquet bloom-filter segment pruning (§8).
        if let Some(id) = &self.trace_id {
            conds.push(format!("trace_id = X'{}'", id.to_hex()));
        }
        if let Some(id) = &self.span_id {
            conds.push(format!("span_id = X'{}'", id.to_hex()));
        }
        for (k, v) in &self.attr_eq {
            let field = p.attr_field(k);
            conds.push(format!("{field} = {}", p.str(v)));
        }
        for k in &self.attr_exists {
            let field = p.attr_field(k);
            conds.push(format!("{field} IS NOT NULL"));
        }
        for (k, t) in &self.attr_matches {
            let field = p.attr_field(k);
            conds.push(format!("matches({field}, {})", p.str(t)));
        }
        for (k, values) in &self.attr_in {
            if values.is_empty() {
                conds.push("1 = 0".to_owned()); // value ∈ ∅ → matches nothing
            } else {
                let field = p.attr_field(k);
                let list = values
                    .iter()
                    .map(|v| p.str(v))
                    .collect::<Vec<_>>()
                    .join(", ");
                conds.push(format!("{field} IN ({list})"));
            }
        }
        for (k, values) in &self.attr_not_in {
            if values.is_empty() {
                continue; // value ∉ ∅ is always true → excludes nothing
            }
            let g = p.attr_field(k);
            let list = values
                .iter()
                .map(|v| p.str(v))
                .collect::<Vec<_>>()
                .join(", ");
            // NULL-aware: a row missing the key has a not-in-set value, so keep it (bare
            // `NULL NOT IN (…)` would drop it).
            conds.push(format!("({g} IS NULL OR {g} NOT IN ({list}))"));
        }
        for (k, op, n) in &self.attr_num {
            // `json_get_num` (or `TRY_CAST` for a promoted column) → NULL for a non-numeric/missing
            // value, which fails the comparison (excluded). Unlike the old
            // `TRY_CAST(json_get_str(...) AS DOUBLE)`, this also matches integer/double-typed JSON
            // attributes, not only numbers stored as strings.
            let field = p.attr_num_field(k);
            conds.push(format!("{field} {} {n}", op.as_sql()));
        }
        for (k, pat) in &self.attr_regex {
            // `regexp_like` (RE2, linear-time — no ReDoS); NULL input (missing key) → excluded.
            let field = p.attr_field(k);
            conds.push(format!("regexp_like({field}, {})", p.str(pat)));
        }
        for predicate in &self.string_predicates {
            let field = match &predicate.field {
                LogStringField::Body => "body".to_owned(),
                LogStringField::Service => "CAST(service AS VARCHAR)".to_owned(),
                LogStringField::Attribute(key) => p.attr_field(key),
                LogStringField::ResourceAttribute(key) => {
                    format!("json_get_str(CAST(resource AS VARCHAR), {})", p.str(key))
                }
            };
            let actual = format!("coalesce({field}, '')");
            let expected = p.str(&predicate.value);
            conds.push(match predicate.op {
                StringPredicate::Eq => format!("{actual} = {expected}"),
                StringPredicate::Ne => format!("{actual} <> {expected}"),
                StringPredicate::Regex => format!("regexp_like({actual}, {expected})"),
                StringPredicate::NotRegex => {
                    format!("NOT regexp_like({actual}, {expected})")
                }
                StringPredicate::Contains => format!("strpos({actual}, {expected}) > 0"),
                StringPredicate::NotContains => format!("strpos({actual}, {expected}) = 0"),
                // The `matches()` UDF is pushed to the `.tidx` only when its first argument is a bare
                // indexed column, so render against `field` directly — no coalesce wrapper.
                StringPredicate::Matches => format!("matches({field}, {expected})"),
                StringPredicate::NotMatches => format!("NOT matches({field}, {expected})"),
            });
        }
        if conds.is_empty() {
            String::new()
        } else {
            format!(" WHERE {}", conds.join(" AND "))
        }
    }

    fn to_sql(&self, p: &mut SqlParams) -> String {
        let dir = match self.direction {
            Direction::Backward => "DESC",
            Direction::Forward => "ASC",
        };
        let offset = if self.offset > 0 {
            format!(" OFFSET {}", self.offset)
        } else {
            String::new()
        };
        format!(
            "SELECT \"time\", observed_time, service, severity_number, severity_text, body, \
             attributes, resource, scope, trace_id, span_id, flags \
             FROM logs{} ORDER BY \"time\" {dir} LIMIT {}{offset}",
            self.where_sql(p),
            self.limit
        )
    }
}

/// A page of query results (ARCHITECTURE.md §10.6).
#[derive(Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct LogPage {
    pub entries: Vec<LogEntry>,
    /// Resume token for the next page: `Some` when a full page was returned (more rows may follow),
    /// `None` when the page was short. Pass it to [`LogQuery::after`] to continue.
    pub next: Option<PageCursor>,
    pub stats: QueryStats,
}

/// Opaque page-resume token (ARCHITECTURE.md §10.6). Carries the count of rows already consumed; pass it
/// back via [`LogQuery::after`] to fetch the next page. Treat it as opaque — the offset encoding is
/// an implementation detail (keyset paging may replace it once rows carry a stable key).
#[derive(Debug, Clone, Copy)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct PageCursor(pub(crate) usize);

/// Query execution stats (ARCHITECTURE.md §10.6). M1d fills `rows_returned`, `elapsed`, `used_index`;
/// the segment/row/byte counters are threaded through from the provider in a later chunk.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct QueryStats {
    pub segments_scanned: u64,
    pub segments_pruned: u64,
    pub rows_scanned: u64,
    pub rows_returned: u64,
    pub bytes_scanned: u64,
    pub elapsed: DurationNs,
    pub used_index: bool,
}

/// One materialized log record (ARCHITECTURE.md §10.6).
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct LogEntry {
    pub time: Timestamp,
    pub observed_time: Option<Timestamp>,
    pub severity_number: SeverityNumber,
    pub severity_text: Option<String>,
    pub service: Option<String>,
    pub body: String,
    pub attributes: Attributes,
    pub resource: Attributes,
    pub scope: Attributes,
    pub trace_id: Option<TraceId>,
    pub span_id: Option<SpanId>,
    pub flags: u32,
}

/// One time bucket of a log-volume query (ARCHITECTURE.md §10.6). `labels` is empty unless the query used
/// `volume_by`, in which case it holds the `(key, value)` pairs identifying this bucket's series.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct VolumeBucket {
    pub time: Timestamp,
    pub labels: Vec<(String, String)>,
    pub count: u64,
}

fn materialize_volume(batches: &[RecordBatch], group_by: &[&str]) -> Result<Vec<VolumeBucket>> {
    let count_idx = 1 + group_by.len();
    let mut out = Vec::new();
    for b in batches {
        let bucket = downcast::<Int64Array>(b, 0)?;
        let count = downcast::<Int64Array>(b, count_idx)?;
        for i in 0..b.num_rows() {
            let labels = group_by
                .iter()
                .enumerate()
                .map(|(j, key)| {
                    let v = get_str(b.column(1 + j).as_ref(), i).unwrap_or_default();
                    ((*key).to_owned(), v)
                })
                .collect();
            out.push(VolumeBucket {
                time: Timestamp(bucket.value(i)),
                labels,
                count: count.value(i) as u64,
            });
        }
    }
    Ok(out)
}

fn materialize(batches: &[RecordBatch]) -> Result<Vec<LogEntry>> {
    let mut out = Vec::new();
    for b in batches {
        let times = downcast::<TimestampNanosecondArray>(b, 0)?;
        let observed = downcast::<TimestampNanosecondArray>(b, 1)?;
        let sev = downcast::<UInt8Array>(b, 3)?;
        let trace = downcast::<FixedSizeBinaryArray>(b, 9)?;
        let span = downcast::<FixedSizeBinaryArray>(b, 10)?;
        let flags = downcast::<UInt32Array>(b, 11)?;
        for i in 0..b.num_rows() {
            out.push(LogEntry {
                time: Timestamp(times.value(i)),
                observed_time: (!observed.is_null(i)).then(|| Timestamp(observed.value(i))),
                severity_number: SeverityNumber(sev.value(i)),
                severity_text: get_str(b.column(4).as_ref(), i),
                service: get_str(b.column(2).as_ref(), i),
                body: get_str(b.column(5).as_ref(), i).unwrap_or_default(),
                attributes: Attributes::from_canonical_json(
                    &get_str(b.column(6).as_ref(), i).unwrap_or_default(),
                ),
                resource: Attributes::from_canonical_json(
                    &get_str(b.column(7).as_ref(), i).unwrap_or_default(),
                ),
                scope: Attributes::from_canonical_json(
                    &get_str(b.column(8).as_ref(), i).unwrap_or_default(),
                ),
                trace_id: (!trace.is_null(i))
                    .then(|| TraceId::from_bytes(trace.value(i)))
                    .flatten(),
                span_id: (!span.is_null(i))
                    .then(|| SpanId::from_bytes(span.value(i)))
                    .flatten(),
                flags: flags.value(i),
            });
        }
    }
    Ok(out)
}

pub(crate) fn downcast<T: 'static>(b: &RecordBatch, idx: usize) -> Result<&T> {
    b.column(idx)
        .as_any()
        .downcast_ref::<T>()
        .ok_or_else(|| Error::query_msg(format!("unexpected array type for logs column {idx}")))
}

/// Read a string cell, tolerating `Utf8`, `Utf8View`, or the `Dictionary(Int32, Utf8)` encoding used
/// for the low-cardinality `service`/`resource`/`scope` columns (ARCHITECTURE.md §6.2). Grouped,
/// `DISTINCT`, and `SELECT *` results surface those columns as a `DictionaryArray`.
pub(crate) fn get_str(arr: &dyn Array, i: usize) -> Option<String> {
    if arr.is_null(i) {
        return None;
    }
    if let Some(a) = arr.as_any().downcast_ref::<StringArray>() {
        return Some(a.value(i).to_owned());
    }
    if let Some(a) = arr.as_any().downcast_ref::<StringViewArray>() {
        return Some(a.value(i).to_owned());
    }
    if let Some(d) = arr.as_any().downcast_ref::<DictionaryArray<Int32Type>>() {
        let values = d.values().as_any().downcast_ref::<StringArray>()?;
        let key = d.keys().value(i);
        return Some(values.value(key as usize).to_owned());
    }
    None
}

#[cfg(test)]
mod native_query_tests {
    use super::*;
    use datafusion::scalar::ScalarValue;

    #[test]
    fn semantic_string_predicates_bind_keys_and_values() {
        let key = "label') OR TRUE --";
        let value = "value') OR TRUE --";
        let pattern = ".*') OR TRUE --";
        let query = LogQuery::new()
            .string_predicate(
                LogStringField::Attribute(key.to_owned()),
                StringPredicate::Eq,
                value,
            )
            .string_predicate(LogStringField::Body, StringPredicate::Regex, pattern);
        let mut params = SqlParams::with_promote(&[]);
        let sql = query.to_sql(&mut params);

        assert!(!sql.contains(key));
        assert!(!sql.contains(value));
        assert!(!sql.contains(pattern));
        assert_eq!(
            params.into_values(),
            vec![
                ScalarValue::Utf8(Some(key.to_owned())),
                ScalarValue::Utf8(Some(value.to_owned())),
                ScalarValue::Utf8(Some(pattern.to_owned())),
            ]
        );
    }

    #[test]
    fn correlation_ids_render_as_raw_binary_literals() {
        let trace = TraceId::from_hex("0123456789abcdef0123456789abcdef").unwrap();
        let span = SpanId::from_hex("0011223344556677").unwrap();
        let query = LogQuery::new().trace_id(trace).span_id(span);
        let mut params = SqlParams::with_promote(&[]);
        let sql = query.to_sql(&mut params);
        assert!(
            sql.contains("trace_id = X'0123456789abcdef0123456789abcdef'"),
            "{sql}"
        );
        assert!(sql.contains("span_id = X'0011223344556677'"), "{sql}");
        // Machine-derived hex is inlined, not bound — no params emitted for the ids.
        assert!(params.into_values().is_empty());
    }
}

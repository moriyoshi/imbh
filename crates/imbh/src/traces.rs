//! The typed Traces query API (ARCHITECTURE.md §10.7).
//!
//! `db.traces().get(trace_id)` assembles all spans of a trace into a [`Trace`] (root
//! service/name + duration + the span list). `db.traces().search(TraceQuery)` finds traces with
//! a span matching the filter and returns [`TraceSummary`]s. Both compile to SQL over the
//! `spans` table and materialize the result batches — one query path, two front doors (§9.4).
//! Both filter on the raw `trace_id` bytes — `get` as `trace_id = X'…'`, `search`'s phase-2 span
//! fetch as `trace_id IN ($1, …)` — so the query provider can prune whole span segments via their
//! Parquet bloom filter (§8). `search`'s phase-1 candidate ranking still groups/semi-joins on
//! `hex(trace_id)`: its id set is a subquery, not literals, so no bloom probe exists there anyway.
//!
//! `db.traces().span_metrics(SpanMetricsQuery)` computes RED metrics (calls / error rate /
//! duration quantiles) over a span filter, grouped by attributes, per `step` bucket. Per-segment
//! Tantivy span search remains later performance work.

use std::collections::BTreeMap;
use std::time::Duration;

use arrow::array::{
    Array, FixedSizeBinaryArray, Float64Array, Int64Array, TimestampNanosecondArray, UInt32Array,
    UInt64Array,
};
use arrow::record_batch::RecordBatch;

use imbh_core::{Attributes, DurationNs, SpanId, TimeRange, Timestamp, TraceId};

use std::sync::Arc;

use crate::logs::{NumOp, downcast, get_str};
use crate::sql::SqlParams;
use crate::{Db, Result};

/// The `spans` columns, in schema order, selected for materialization.
const SPAN_COLS: &str = "trace_id, span_id, parent_span_id, name, kind, start_time, duration_ns, \
     status_code, status_message, service, attributes, resource, scope, events, links, \
     trace_state, flags";

/// The phase-2 span fetch of [`TracesApi::search`]: every span of the candidate traces `ids`.
///
/// The predicate is `trace_id IN ($1, …, $n)` over the **raw** id bytes, never `hex(trace_id) IN
/// ('…')`: only the raw binary form lets the query provider probe each segment's Parquet bloom filter
/// and skip the segments that hold none of the candidate ids (ARCHITECTURE.md §8) — the `hex()` UDF
/// form hides the bytes and forces a read of every span segment in the database. Values stay bound
/// (`$N` placeholders, `FixedSizeBinary` scalars), so nothing reaches the SQL text as an interpolated
/// literal. Semantically identical either way — the pushdown is `Inexact`, so DataFusion re-applies
/// the predicate above the scan and pruning can only ever save I/O, never change the answer.
fn spans_of_traces_sql(ids: &[TraceId], params: &mut SqlParams) -> String {
    let in_list = ids
        .iter()
        .map(|id| params.id_bytes(&id.0))
        .collect::<Vec<_>>()
        .join(", ");
    format!("SELECT {SPAN_COLS} FROM spans WHERE trace_id IN ({in_list})")
}

/// Traces query namespace, reached via [`Db::traces`].
pub struct TracesApi {
    pub(crate) db: Arc<Db>,
}

impl TracesApi {
    /// Assemble the trace for `trace_id` (its spans ordered by start time), or `None` if absent.
    pub async fn get(&self, trace_id: TraceId) -> Result<Option<Trace>> {
        // Filter on the raw `trace_id` bytes (`X'…'`), not `hex(trace_id) = '…'`: the raw binary
        // equality is what the query provider recognizes to skip whole span segments via their
        // Parquet bloom filter (ARCHITECTURE.md §8). Semantically identical — DataFusion still
        // applies the predicate to whatever rows are read.
        let sql = format!(
            "SELECT {SPAN_COLS} FROM spans WHERE trace_id = X'{}' ORDER BY start_time",
            trace_id.to_hex()
        );
        let spans = materialize_spans(&self.db.sql(&sql).collect().await?)?;
        if spans.is_empty() {
            Ok(None)
        } else {
            Ok(Some(assemble_trace(trace_id, spans)))
        }
    }

    /// The raw Arrow spans of one trace — the same scan as [`get`](Self::get) but *without*
    /// materializing `Span` DTOs (`materialize_spans` / `assemble_trace`). Column layout is the
    /// `SPAN_COLS` projection. Lets a caller read span fields directly from the batch buffers.
    pub async fn get_batches(&self, trace_id: TraceId) -> Result<Vec<RecordBatch>> {
        let sql = format!(
            "SELECT {SPAN_COLS} FROM spans WHERE trace_id = X'{}' ORDER BY start_time",
            trace_id.to_hex()
        );
        self.db.sql(&sql).collect().await
    }

    /// Find traces with a span matching the query; return per-trace summaries, newest first.
    pub async fn search(&self, q: TraceQuery) -> Result<Vec<TraceSummary>> {
        // Phase 1: rank candidate trace ids by recency.
        let mut params = SqlParams::with_promote(self.db.storage.promote().keys());
        // Span predicates select traces that *contain* a matching span, via a semi-join on the raw
        // predicate (kept in a `WHERE`, where bind-parameter types infer). Being a separate scan, it
        // does not shift the `min/max(start_time)` aggregates below — those stay over ALL of each
        // (matched) trace's spans, so the trace start is the true start.
        let span = q.span_conditions(&mut params);
        let match_filter = if span.is_empty() {
            String::new()
        } else {
            format!(
                " WHERE hex(trace_id) IN (SELECT hex(trace_id) FROM spans WHERE {})",
                span.join(" AND ")
            )
        };
        let having_sql = q.trace_start_having(&mut params);
        let list_sql = format!(
            "SELECT hex(trace_id) AS tid, max(CAST(start_time AS BIGINT)) AS latest \
             FROM spans{match_filter} GROUP BY tid{having_sql} ORDER BY latest DESC LIMIT {}",
            q.limit
        );
        let list = self
            .db
            .sql_with_params(list_sql, params.into_values())
            .collect()
            .await?;
        let mut tids: Vec<String> = Vec::new();
        for b in &list {
            let col = b.column(0);
            for i in 0..b.num_rows() {
                if let Some(s) = get_str(col.as_ref(), i) {
                    tids.push(s);
                }
            }
        }
        if tids.is_empty() {
            return Ok(Vec::new());
        }

        // Phase 2: fetch every span of those traces and assemble summaries in Rust, filtering on the
        // **raw** `trace_id` bytes so the segment bloom filters can prune (see `spans_of_traces_sql`).
        // The candidate ids come back from phase 1 as `hex()` text; decode them to bytes (a
        // machine-derived id always parses — skip anything that somehow does not rather than falling
        // back to an unprunable predicate).
        let ids: Vec<TraceId> = tids.iter().filter_map(|t| TraceId::from_hex(t)).collect();
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut params = SqlParams::with_promote(self.db.storage.promote().keys());
        let sql = spans_of_traces_sql(&ids, &mut params);
        let spans = materialize_spans(
            &self
                .db
                .sql_with_params(sql, params.into_values())
                .collect()
                .await?,
        )?;

        let mut by_trace: BTreeMap<String, Vec<Span>> = BTreeMap::new();
        for s in spans {
            by_trace.entry(s.trace_id.to_hex()).or_default().push(s);
        }
        let mut out = Vec::new();
        for tid in &tids {
            if let Some(spans) = by_trace.remove(tid) {
                let Some(first) = spans.first() else { continue };
                let trace_id = first.trace_id;
                let error = spans.iter().any(|s| s.status_code == "ERROR");
                let trace = assemble_trace(trace_id, spans);
                out.push(TraceSummary {
                    trace_id,
                    root_service: trace.root_service,
                    root_name: trace.root_name,
                    start_time: trace.start_time,
                    duration_ns: trace.duration_ns,
                    span_count: trace.spans.len() as u64,
                    error,
                });
            }
        }
        Ok(out)
    }

    /// Span RED metrics (ARCHITECTURE.md §10.7): rate (calls), error rate, and duration quantiles over a
    /// span filter, grouped by attribute keys, per `step` bucket. The core traces-as-metrics
    /// primitive (Tempo's `/api/metrics/query_range`).
    pub async fn span_metrics(&self, q: SpanMetricsQuery) -> Result<SpanMetrics> {
        let mut params = SqlParams::with_promote(self.db.storage.promote().keys());
        let sql = q.to_sql(&mut params);
        let batches = self
            .db
            .sql_with_params(sql, params.into_values())
            .collect()
            .await?;
        materialize_span_metrics(&batches, &q.group_by)
    }

    /// Span RED metrics returning the raw Arrow batches (plus scan stats) instead of a materialized
    /// [`SpanMetrics`] — the zero-copy-friendly entry point for a Go / FFI binding (ARCHITECTURE.md
    /// §10.17). Same SQL as [`span_metrics`](Self::span_metrics): each batch carries `bucket` (Int64
    /// epoch-nanos), one `g0..gN` (Utf8) column per `group_by` label, then `calls`, `errors`, `p50`,
    /// `p95`, `p99`, ordered by `bucket`.
    #[cfg(feature = "proto")]
    pub async fn span_metrics_batches(
        &self,
        q: SpanMetricsQuery,
    ) -> Result<(Vec<RecordBatch>, crate::QueryStats)> {
        let mut params = SqlParams::with_promote(self.db.storage.promote().keys());
        let sql = q.to_sql(&mut params);
        let started = std::time::Instant::now();
        let (_schema, batches, scan) = self
            .db
            .sql_with_params(sql, params.into_values())
            .collect_with_stats()
            .await?;
        let stats = crate::logs::batch_query_stats(&scan, &batches, started);
        Ok((batches, stats))
    }
}

/// A span-metrics (RED) query (ARCHITECTURE.md §10.7).
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SpanMetricsQuery {
    service: Option<String>,
    name: Option<String>,
    kind: Option<String>,
    status: Option<String>,
    attr_eq: Vec<(String, String)>,
    group_by: Vec<String>,
    range: Option<TimeRange>,
    step: Duration,
}

impl Default for SpanMetricsQuery {
    fn default() -> Self {
        SpanMetricsQuery {
            service: None,
            name: None,
            kind: None,
            status: None,
            attr_eq: Vec::new(),
            group_by: Vec::new(),
            range: None,
            step: Duration::from_secs(60),
        }
    }
}

impl SpanMetricsQuery {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn service(mut self, s: &str) -> Self {
        self.service = Some(s.to_owned());
        self
    }
    pub fn name(mut self, n: &str) -> Self {
        self.name = Some(n.to_owned());
        self
    }
    pub fn kind(mut self, k: &str) -> Self {
        self.kind = Some(k.to_owned());
        self
    }
    pub fn status(mut self, s: &str) -> Self {
        self.status = Some(s.to_owned());
        self
    }
    pub fn attr_eq(mut self, key: &str, value: &str) -> Self {
        self.attr_eq.push((key.to_owned(), value.to_owned()));
        self
    }
    /// Group series by an attribute key (repeatable) — e.g. `http.route` for RED-by-route.
    pub fn group_by(mut self, key: &str) -> Self {
        self.group_by.push(key.to_owned());
        self
    }
    pub fn range(mut self, r: TimeRange) -> Self {
        self.range = Some(r);
        self
    }
    pub fn since(mut self, d: Duration) -> Self {
        self.range = Some(TimeRange::since(d));
        self
    }
    pub fn step(mut self, step: Duration) -> Self {
        self.step = step;
        self
    }

    fn to_sql(&self, p: &mut SqlParams) -> String {
        let step_ns = (self.step.as_nanos() as i64).max(1);
        let mut select = vec![format!(
            "(CAST(start_time AS BIGINT) / {step_ns}) * {step_ns} AS bucket"
        )];
        let mut group = vec!["bucket".to_owned()];
        for (i, k) in self.group_by.iter().enumerate() {
            select.push(format!("{} AS g{i}", p.attr_field(k)));
            group.push(format!("g{i}"));
        }
        select.push("count(*) AS calls".to_owned());
        select.push("sum(CASE WHEN status_code = 'ERROR' THEN 1 ELSE 0 END) AS errors".to_owned());
        select.push("approx_percentile_cont(duration_ns, 0.5) AS p50".to_owned());
        select.push("approx_percentile_cont(duration_ns, 0.95) AS p95".to_owned());
        select.push("approx_percentile_cont(duration_ns, 0.99) AS p99".to_owned());

        let mut conds: Vec<String> = Vec::new();
        if let Some(r) = &self.range {
            if r.start.0 != i64::MIN {
                conds.push(format!(
                    "CAST(start_time AS BIGINT) >= {}",
                    p.i64(r.start.0)
                ));
            }
            if r.end.0 != i64::MAX {
                conds.push(format!("CAST(start_time AS BIGINT) < {}", p.i64(r.end.0)));
            }
        }
        if let Some(s) = &self.service {
            conds.push(format!("service = {}", p.str(s)));
        }
        if let Some(n) = &self.name {
            conds.push(format!("name = {}", p.str(n)));
        }
        if let Some(k) = &self.kind {
            conds.push(format!("kind = {}", p.str(k)));
        }
        if let Some(s) = &self.status {
            conds.push(format!("status_code = {}", p.str(s)));
        }
        for (k, v) in &self.attr_eq {
            let field = p.attr_field(k);
            conds.push(format!("{field} = {}", p.str(v)));
        }
        let where_clause = if conds.is_empty() {
            String::new()
        } else {
            format!(" WHERE {}", conds.join(" AND "))
        };
        format!(
            "SELECT {} FROM spans{} GROUP BY {} ORDER BY bucket",
            select.join(", "),
            where_clause,
            group.join(", ")
        )
    }
}

/// One RED data point for a labeled series over a `step` bucket (ARCHITECTURE.md §10.7).
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SpanMetricPoint {
    pub time: Timestamp,
    pub calls: u64,
    pub errors: u64,
    pub error_rate: f64,
    pub p50_ns: f64,
    pub p95_ns: f64,
    pub p99_ns: f64,
}

/// A labeled RED time series.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SpanMetricSeries {
    pub labels: Vec<(String, String)>,
    pub points: Vec<SpanMetricPoint>,
}

/// The result of a span-metrics query.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SpanMetrics(pub Vec<SpanMetricSeries>);

fn materialize_span_metrics(batches: &[RecordBatch], group_by: &[String]) -> Result<SpanMetrics> {
    let ng = group_by.len();
    let mut order: Vec<Vec<(String, String)>> = Vec::new();
    let mut series: std::collections::HashMap<Vec<(String, String)>, Vec<SpanMetricPoint>> =
        std::collections::HashMap::new();

    for b in batches {
        let bucket = downcast::<Int64Array>(b, 0)?;
        let calls = downcast::<Int64Array>(b, 1 + ng)?;
        let errors = downcast::<Int64Array>(b, 2 + ng)?;
        let p50 = downcast::<Float64Array>(b, 3 + ng)?;
        let p95 = downcast::<Float64Array>(b, 4 + ng)?;
        let p99 = downcast::<Float64Array>(b, 5 + ng)?;
        for i in 0..b.num_rows() {
            let labels: Vec<(String, String)> = group_by
                .iter()
                .enumerate()
                .map(|(j, key)| {
                    (
                        key.clone(),
                        get_str(b.column(1 + j).as_ref(), i).unwrap_or_default(),
                    )
                })
                .collect();
            let calls_v = calls.value(i).max(0) as u64;
            let errors_v = errors.value(i).max(0) as u64;
            let point = SpanMetricPoint {
                time: Timestamp(bucket.value(i)),
                calls: calls_v,
                errors: errors_v,
                error_rate: if calls_v > 0 {
                    errors_v as f64 / calls_v as f64
                } else {
                    0.0
                },
                p50_ns: p50.value(i),
                p95_ns: p95.value(i),
                p99_ns: p99.value(i),
            };
            match series.get_mut(&labels) {
                Some(points) => points.push(point),
                None => {
                    order.push(labels.clone());
                    series.insert(labels, vec![point]);
                }
            }
        }
    }
    Ok(SpanMetrics(
        order
            .into_iter()
            .map(|labels| {
                let points = series.remove(&labels).unwrap_or_default();
                SpanMetricSeries { labels, points }
            })
            .collect(),
    ))
}

/// A trace search query (ARCHITECTURE.md §10.7).
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct TraceQuery {
    service: Option<String>,
    name: Option<String>,
    text: Option<String>,
    min_duration_ns: Option<u64>,
    max_duration_ns: Option<u64>,
    status: Option<String>,
    kind: Option<String>,
    attr_eq: Vec<(String, String)>,
    attr_exists: Vec<String>,
    attr_matches: Vec<(String, String)>,
    attr_in: Vec<(String, Vec<String>)>,
    attr_not_in: Vec<(String, Vec<String>)>,
    attr_num: Vec<(String, NumOp, f64)>,
    attr_regex: Vec<(String, String)>,
    trace_start_range: Option<TimeRange>,
    trace_start_end_inclusive: bool,
    range: Option<TimeRange>,
    limit: usize,
}

impl Default for TraceQuery {
    fn default() -> Self {
        TraceQuery {
            service: None,
            name: None,
            text: None,
            min_duration_ns: None,
            max_duration_ns: None,
            status: None,
            kind: None,
            attr_eq: Vec::new(),
            attr_exists: Vec::new(),
            attr_matches: Vec::new(),
            attr_in: Vec::new(),
            attr_not_in: Vec::new(),
            attr_num: Vec::new(),
            attr_regex: Vec::new(),
            trace_start_range: None,
            trace_start_end_inclusive: false,
            range: None,
            limit: 20,
        }
    }
}

impl TraceQuery {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn service(mut self, s: &str) -> Self {
        self.service = Some(s.to_owned());
        self
    }
    pub fn name(mut self, n: &str) -> Self {
        self.name = Some(n.to_owned());
        self
    }
    /// Full-text `matches` over the span `name` (tokenized term search — a trace matches when any
    /// of its spans' names contain all query terms). Complements the exact-match [`name`](Self::name).
    pub fn matches(mut self, text: &str) -> Self {
        self.text = Some(text.to_owned());
        self
    }
    pub fn min_duration(mut self, d: Duration) -> Self {
        self.min_duration_ns = Some(d.as_nanos() as u64);
        self
    }
    pub fn max_duration(mut self, d: Duration) -> Self {
        self.max_duration_ns = Some(d.as_nanos() as u64);
        self
    }
    /// Filter by span status (`UNSET`/`OK`/`ERROR`).
    pub fn status(mut self, s: &str) -> Self {
        self.status = Some(s.to_owned());
        self
    }
    /// Filter by span kind (`SERVER`/`CLIENT`/…).
    pub fn kind(mut self, k: &str) -> Self {
        self.kind = Some(k.to_owned());
        self
    }
    pub fn attr_eq(mut self, key: &str, value: &str) -> Self {
        self.attr_eq.push((key.to_owned(), value.to_owned()));
        self
    }
    /// Keep traces with a span that **has** the attribute `key` (any value). Repeatable.
    pub fn attr_exists(mut self, key: &str) -> Self {
        self.attr_exists.push(key.to_owned());
        self
    }
    /// Keep traces with a span whose attribute `key` value term-matches `text` (tokenized
    /// `matches`). Repeatable.
    pub fn attr_matches(mut self, key: &str, text: &str) -> Self {
        self.attr_matches.push((key.to_owned(), text.to_owned()));
        self
    }
    /// Keep traces with a span whose attribute `key` value is one of `values` (in-set match). An
    /// empty `values` set matches nothing. Repeatable.
    pub fn attr_in(mut self, key: &str, values: &[&str]) -> Self {
        self.attr_in.push((
            key.to_owned(),
            values.iter().map(|v| (*v).to_owned()).collect(),
        ));
        self
    }
    /// Exclude spans whose attribute `key` value is one of `values`. Spans lacking `key` are kept;
    /// an empty `values` set excludes nothing. Repeatable.
    pub fn attr_not_in(mut self, key: &str, values: &[&str]) -> Self {
        self.attr_not_in.push((
            key.to_owned(),
            values.iter().map(|v| (*v).to_owned()).collect(),
        ));
        self
    }
    /// Numeric filter: keep spans where `key`'s value, parsed as a number, is `> n` (a non-numeric or
    /// missing value never matches). Combine `attr_ge`/`attr_le` for a range. Repeatable.
    pub fn attr_gt(mut self, key: &str, n: f64) -> Self {
        self.attr_num.push((key.to_owned(), NumOp::Gt, n));
        self
    }
    /// Numeric filter: `key`'s value `>= n` (see [`attr_gt`](Self::attr_gt)).
    pub fn attr_ge(mut self, key: &str, n: f64) -> Self {
        self.attr_num.push((key.to_owned(), NumOp::Ge, n));
        self
    }
    /// Numeric filter: `key`'s value `< n` (see [`attr_gt`](Self::attr_gt)).
    pub fn attr_lt(mut self, key: &str, n: f64) -> Self {
        self.attr_num.push((key.to_owned(), NumOp::Lt, n));
        self
    }
    /// Numeric filter: `key`'s value `<= n` (see [`attr_gt`](Self::attr_gt)).
    pub fn attr_le(mut self, key: &str, n: f64) -> Self {
        self.attr_num.push((key.to_owned(), NumOp::Le, n));
        self
    }
    /// Keep spans where `key`'s value matches the regex `pattern` (RE2, via `regexp_like`). A missing
    /// value never matches. Repeatable.
    pub fn attr_regex(mut self, key: &str, pattern: &str) -> Self {
        self.attr_regex.push((key.to_owned(), pattern.to_owned()));
        self
    }
    pub fn range(mut self, r: TimeRange) -> Self {
        self.range = Some(r);
        self
    }
    pub fn since(mut self, d: Duration) -> Self {
        self.range = Some(TimeRange::since(d));
        self
    }
    pub fn limit(mut self, n: usize) -> Self {
        self.limit = n;
        self
    }

    /// Restrict candidates by the assembled trace start (minimum span start), independently of
    /// span-level filters.
    pub fn trace_start_range(mut self, range: TimeRange) -> Self {
        self.trace_start_range = Some(range);
        self.trace_start_end_inclusive = false;
        self
    }
    /// Restrict candidates to a closed assembled-trace start interval.
    pub fn trace_start_range_inclusive(mut self, start: Timestamp, end: Timestamp) -> Self {
        self.trace_start_range = Some(TimeRange::between(start, end));
        self.trace_start_end_inclusive = true;
        self
    }

    /// The `HAVING` on `min(start_time)` **over all of the trace's spans** — the true trace start.
    /// Span predicates are applied separately (a `trace_id IN (…)` semi-join in [`TracesApi::search`])
    /// so they select traces that *contain* a matching span without shifting this aggregate onto only
    /// those spans (a trace whose root is in range but whose matching span is later must not be
    /// dropped — regression test in `tests/trace_search_boundary.rs`).
    fn trace_start_having(&self, params: &mut SqlParams) -> String {
        let Some(range) = &self.trace_start_range else {
            return String::new();
        };
        let mut conditions = Vec::new();
        if range.start.0 != i64::MIN {
            conditions.push(format!(
                "min(CAST(start_time AS BIGINT)) >= {}",
                params.i64(range.start.0)
            ));
        }
        if range.end.0 != i64::MAX {
            let operator = if self.trace_start_end_inclusive {
                "<="
            } else {
                "<"
            };
            conditions.push(format!(
                "min(CAST(start_time AS BIGINT)) {operator} {}",
                params.i64(range.end.0)
            ));
        }
        if conditions.is_empty() {
            String::new()
        } else {
            format!(" HAVING {}", conditions.join(" AND "))
        }
    }

    /// Span-match conditions (same-span conjunction). Returned as raw conditions — the candidate
    /// query applies them as a conditional aggregate, not a `WHERE`, so they select traces that
    /// contain a matching span without shifting the trace-start aggregate onto only those spans.
    fn span_conditions(&self, p: &mut SqlParams) -> Vec<String> {
        let mut c: Vec<String> = Vec::new();
        if let Some(r) = &self.range {
            if r.start.0 != i64::MIN {
                c.push(format!(
                    "CAST(start_time AS BIGINT) >= {}",
                    p.i64(r.start.0)
                ));
            }
            if r.end.0 != i64::MAX {
                c.push(format!("CAST(start_time AS BIGINT) < {}", p.i64(r.end.0)));
            }
        }
        if let Some(s) = &self.service {
            c.push(format!("service = {}", p.str(s)));
        }
        if let Some(n) = &self.name {
            c.push(format!("name = {}", p.str(n)));
        }
        if let Some(t) = &self.text {
            c.push(format!("matches(name, {})", p.str(t)));
        }
        if let Some(d) = self.min_duration_ns {
            c.push(format!("duration_ns >= {d}"));
        }
        if let Some(d) = self.max_duration_ns {
            c.push(format!("duration_ns <= {d}"));
        }
        if let Some(s) = &self.status {
            c.push(format!("status_code = {}", p.str(s)));
        }
        if let Some(k) = &self.kind {
            c.push(format!("kind = {}", p.str(k)));
        }
        for (k, v) in &self.attr_eq {
            let field = p.attr_field(k);
            c.push(format!("{field} = {}", p.str(v)));
        }
        for k in &self.attr_exists {
            let field = p.attr_field(k);
            c.push(format!("{field} IS NOT NULL"));
        }
        for (k, t) in &self.attr_matches {
            let field = p.attr_field(k);
            c.push(format!("matches({field}, {})", p.str(t)));
        }
        for (k, values) in &self.attr_in {
            if values.is_empty() {
                c.push("1 = 0".to_owned()); // value ∈ ∅ → matches nothing
            } else {
                let field = p.attr_field(k);
                let list = values
                    .iter()
                    .map(|v| p.str(v))
                    .collect::<Vec<_>>()
                    .join(", ");
                c.push(format!("{field} IN ({list})"));
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
            // NULL-aware: keep spans missing the key (bare `NULL NOT IN (…)` would drop them).
            c.push(format!("({g} IS NULL OR {g} NOT IN ({list}))"));
        }
        for (k, op, n) in &self.attr_num {
            // `attr_num_field` matches integer/double-typed JSON attributes (via `json_get_num`), not
            // only string-encoded numbers; NULL ⇒ comparison false ⇒ span excluded.
            let field = p.attr_num_field(k);
            c.push(format!("{field} {} {n}", op.as_sql()));
        }
        for (k, pat) in &self.attr_regex {
            let field = p.attr_field(k);
            c.push(format!("regexp_like({field}, {})", p.str(pat)));
        }
        c
    }
}

/// An assembled trace (ARCHITECTURE.md §10.7).
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Trace {
    pub trace_id: TraceId,
    pub root_service: Option<String>,
    pub root_name: Option<String>,
    pub start_time: Timestamp,
    pub duration_ns: DurationNs,
    pub spans: Vec<Span>,
}

/// A trace search result summary.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct TraceSummary {
    pub trace_id: TraceId,
    pub root_service: Option<String>,
    pub root_name: Option<String>,
    pub start_time: Timestamp,
    pub duration_ns: DurationNs,
    pub span_count: u64,
    pub error: bool,
}

/// One materialized span (ARCHITECTURE.md §6.3/§10.7).
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Span {
    pub trace_id: TraceId,
    pub span_id: SpanId,
    pub parent_span_id: Option<SpanId>,
    pub name: String,
    pub kind: String,
    pub start_time: Timestamp,
    pub duration_ns: DurationNs,
    pub status_code: String,
    pub status_message: Option<String>,
    pub service: Option<String>,
    pub attributes: Attributes,
    pub resource: Attributes,
    pub scope: Attributes,
    /// Events/links stay as canonical JSON for M2c.
    pub events: Option<String>,
    pub links: Option<String>,
    pub trace_state: Option<String>,
    pub flags: u32,
}

fn assemble_trace(trace_id: TraceId, spans: Vec<Span>) -> Trace {
    let start = spans.iter().map(|s| s.start_time.0).min().unwrap_or(0);
    // `saturating` guards a crafted-huge duration from overflowing i64 (a real span never does).
    let end = spans
        .iter()
        .map(|s| s.start_time.0.saturating_add(s.duration_ns.0 as i64))
        .max()
        .unwrap_or(0);
    // Root = the (first) span with no parent; fall back to the earliest span.
    let root = spans
        .iter()
        .find(|s| s.parent_span_id.is_none())
        .or_else(|| spans.first());
    Trace {
        trace_id,
        root_service: root.and_then(|s| s.service.clone()),
        root_name: root.map(|s| s.name.clone()),
        start_time: Timestamp(start),
        duration_ns: DurationNs((end - start).max(0) as u64),
        spans,
    }
}

fn materialize_spans(batches: &[RecordBatch]) -> Result<Vec<Span>> {
    let mut out = Vec::new();
    for b in batches {
        let trace_id = downcast::<FixedSizeBinaryArray>(b, 0)?;
        let span_id = downcast::<FixedSizeBinaryArray>(b, 1)?;
        let parent = downcast::<FixedSizeBinaryArray>(b, 2)?;
        let start = downcast::<TimestampNanosecondArray>(b, 5)?;
        let duration = downcast::<UInt64Array>(b, 6)?;
        let flags = downcast::<UInt32Array>(b, 16)?;
        for i in 0..b.num_rows() {
            out.push(Span {
                trace_id: TraceId::from_bytes(trace_id.value(i)).unwrap_or(TraceId([0; 16])),
                span_id: SpanId::from_bytes(span_id.value(i)).unwrap_or(SpanId([0; 8])),
                parent_span_id: (!parent.is_null(i))
                    .then(|| SpanId::from_bytes(parent.value(i)))
                    .flatten(),
                name: get_str(b.column(3).as_ref(), i).unwrap_or_default(),
                kind: get_str(b.column(4).as_ref(), i).unwrap_or_default(),
                start_time: Timestamp(start.value(i)),
                duration_ns: DurationNs(duration.value(i)),
                status_code: get_str(b.column(7).as_ref(), i).unwrap_or_default(),
                status_message: get_str(b.column(8).as_ref(), i),
                service: get_str(b.column(9).as_ref(), i),
                attributes: Attributes::from_canonical_json(
                    &get_str(b.column(10).as_ref(), i).unwrap_or_default(),
                ),
                resource: Attributes::from_canonical_json(
                    &get_str(b.column(11).as_ref(), i).unwrap_or_default(),
                ),
                scope: Attributes::from_canonical_json(
                    &get_str(b.column(12).as_ref(), i).unwrap_or_default(),
                ),
                events: get_str(b.column(13).as_ref(), i),
                links: get_str(b.column(14).as_ref(), i),
                trace_state: get_str(b.column(15).as_ref(), i),
                flags: flags.value(i),
            });
        }
    }
    Ok(out)
}

#[cfg(test)]
mod native_trace_query_tests {
    use super::*;
    use datafusion::scalar::ScalarValue;

    #[test]
    fn spans_of_traces_filters_on_raw_bound_id_bytes() {
        let mut params = SqlParams::with_promote(&[]);
        let sql = spans_of_traces_sql(&[TraceId([0x11; 16]), TraceId([0x22; 16])], &mut params);

        // Raw `trace_id`, not `hex(trace_id)` — the bloom filters can only probe the raw bytes.
        assert!(
            sql.ends_with("FROM spans WHERE trace_id IN ($1, $2)"),
            "unexpected SQL: {sql}"
        );
        assert!(
            !sql.contains("hex("),
            "the hex() form defeats bloom pruning: {sql}"
        );
        // Values stay bound (never interpolated) and carry the column's exact width, which is what
        // DataFusion type-checks the `$N` placeholders against.
        assert_eq!(
            params.into_values(),
            vec![
                ScalarValue::FixedSizeBinary(16, Some(vec![0x11; 16])),
                ScalarValue::FixedSizeBinary(16, Some(vec![0x22; 16])),
            ]
        );
    }

    #[test]
    fn trace_start_bounds_are_bound_and_closed_when_requested() {
        let query = TraceQuery::new()
            .trace_start_range_inclusive(Timestamp(i64::MIN + 7), Timestamp(i64::MAX - 7));
        let mut params = SqlParams::with_promote(&[]);
        let sql = query.trace_start_having(&mut params);

        assert_eq!(
            sql,
            " HAVING min(CAST(start_time AS BIGINT)) >= $1 AND min(CAST(start_time AS BIGINT)) <= $2"
        );
        assert_eq!(
            params.into_values(),
            vec![
                ScalarValue::Int64(Some(i64::MIN + 7)),
                ScalarValue::Int64(Some(i64::MAX - 7))
            ]
        );
    }
}

/// The read-side payoff of the raw-`trace_id` phase-2 predicate: a trace search must skip the span
/// segments that hold none of the candidate ids, and must return exactly what the old `hex()` form
/// returned. Needs the OTLP ingest path to build real (bloom-carrying) segments.
#[cfg(all(test, feature = "ingest"))]
mod trace_search_pruning_tests {
    use super::*;
    use imbh_test_support::otlp::otlp_trace_tree;

    /// Three traces, each sealed into its own bloom-carrying span segment.
    async fn db_with_three_trace_segments(dir: &std::path::Path) -> Arc<Db> {
        let db = Db::builder(dir).open().unwrap();
        for (i, id) in [[0x11u8; 16], [0x22; 16], [0x33; 16]].iter().enumerate() {
            db.ingest_otlp_traces(&otlp_trace_tree(&format!("svc{i}"), *id))
                .await
                .unwrap();
            db.flush().await.unwrap(); // one segment per trace
        }
        db
    }

    /// A stable, comparable rendering of a span set (order-independent).
    fn span_keys(spans: &[Span]) -> Vec<(String, String, String, i64)> {
        let mut keys: Vec<_> = spans
            .iter()
            .map(|s| {
                (
                    s.trace_id.to_hex(),
                    s.span_id.to_hex(),
                    s.name.clone(),
                    s.start_time.0,
                )
            })
            .collect();
        keys.sort();
        keys
    }

    #[tokio::test(flavor = "current_thread")]
    async fn phase2_fetch_prunes_segments_without_changing_the_answer() {
        let dir = tempfile::tempdir().unwrap();
        let db = db_with_three_trace_segments(dir.path()).await;

        // The phase-2 predicate `search` now issues, for a single candidate trace.
        let wanted = TraceId([0x22; 16]);
        let mut params = SqlParams::with_promote(&[]);
        let sql = spans_of_traces_sql(&[wanted], &mut params);
        let (_schema, batches, scan) = db
            .sql_with_params(sql, params.into_values())
            .collect_with_stats()
            .await
            .unwrap();
        let raw_spans = materialize_spans(&batches).unwrap();
        assert_eq!(
            scan.segments_pruned, 2,
            "the two segments holding no candidate id are skipped via their blooms"
        );
        assert_eq!(scan.segments_scanned, 1);

        // The pre-fix predicate: identical rows, but every segment read — the bug being fixed.
        let mut params = SqlParams::with_promote(&[]);
        let hex_sql = format!(
            "SELECT {SPAN_COLS} FROM spans WHERE hex(trace_id) IN ({})",
            params.str(wanted.to_hex())
        );
        let (_schema, hex_batches, hex_scan) = db
            .sql_with_params(hex_sql, params.into_values())
            .collect_with_stats()
            .await
            .unwrap();
        assert_eq!(
            hex_scan.segments_pruned, 0,
            "hex(trace_id) hides the raw bytes, so nothing can be bloom-pruned"
        );
        assert_eq!(hex_scan.segments_scanned, 3);
        assert_eq!(
            span_keys(&raw_spans),
            span_keys(&materialize_spans(&hex_batches).unwrap()),
            "pruning must never change the rows returned"
        );
        assert_eq!(
            raw_spans.len(),
            2,
            "the fixture trace has a root and a child"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn search_returns_the_same_traces_it_always_did() {
        let dir = tempfile::tempdir().unwrap();
        let db = db_with_three_trace_segments(dir.path()).await;

        // A single-candidate search (the maximally prunable case).
        let one = db
            .traces()
            .search(TraceQuery::new().service("svc1"))
            .await
            .unwrap();
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].trace_id, TraceId([0x22; 16]));
        assert_eq!(one[0].span_count, 2);
        assert_eq!(one[0].root_name.as_deref(), Some("GET /cart"));
        assert!(one[0].error, "the fixture root span is ERROR");

        // An unfiltered search: all three traces, each fully assembled. A dropped span (or trace)
        // from over-eager pruning would show up here as a wrong `span_count`/set of ids.
        let all = db.traces().search(TraceQuery::new()).await.unwrap();
        let mut ids: Vec<String> = all.iter().map(|t| t.trace_id.to_hex()).collect();
        ids.sort();
        assert_eq!(
            ids,
            vec![
                TraceId([0x11; 16]).to_hex(),
                TraceId([0x22; 16]).to_hex(),
                TraceId([0x33; 16]).to_hex(),
            ]
        );
        assert!(all.iter().all(|t| t.span_count == 2));

        // A candidate set that matches nothing still comes back empty, not wrong.
        let none = db
            .traces()
            .search(TraceQuery::new().service("nope"))
            .await
            .unwrap();
        assert!(none.is_empty());
    }
}

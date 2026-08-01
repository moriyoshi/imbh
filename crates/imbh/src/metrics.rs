//! The typed Metrics query API (ARCHITECTURE.md §10.8) — a SigNoz-style builder compiled to SQL over the
//! metric tables (`metrics_gauge`/`metrics_sum`/`metrics_histogram`/`metrics_exp_histogram`/
//! `metrics_summary`).
//!
//! `metrics().range(MetricQuery)` → a [`Matrix`] (one series per `group_by` label set, samples per
//! `step` bucket); `metrics().instant(...)` → a [`Vector`] (the last sample per series);
//! `metrics().histogram_quantile(HistogramQuery)` and `.exp_histogram_quantile(ExpHistogramQuery)`
//! → a [`Matrix`] of phi-quantile series over the explicit- and exponential-bucket histogram tables;
//! `metrics().catalog()` → the metric metadata; `metrics().series(metric)` → the distinct label
//! sets. No PromQL in v1 (§3) — this builder plus SQL is the metric query surface.
//!
//! Scope: gauge + sum with avg/sum/min/max/count aggregation, per-second `.rate()` (delta sums) and
//! `.rate_counter()` (cumulative counters), `group_by` attribute keys, attribute filters, and both
//! explicit- and exponential-bucket histogram quantiles. Exemplars, summaries, and delta→cumulative
//! normalization are later M3 work.

use std::time::Duration;

use arrow::array::{
    Array, BooleanArray, Float64Array, Int32Array, Int64Array, ListArray, UInt64Array,
};
use arrow::record_batch::RecordBatch;

use imbh_core::{
    AnyValue, Attributes, SpanId, Table, TimeRange, Timestamp, TraceId, canonical_json_value,
    exp_histogram_quantile, histogram_quantile, parse_json,
};

use std::sync::Arc;

use crate::logs::get_str;
use crate::sql::SqlParams;
use crate::{Db, Result};

/// One OTLP exemplar surfaced from a metric point — the trace link for metric→trace drill-down
/// (ARCHITECTURE.md §6.4). Parsed from the stored `exemplars` JSON column.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Exemplar {
    pub time: Timestamp,
    pub value: f64,
    pub trace_id: Option<TraceId>,
    pub span_id: Option<SpanId>,
    /// The exemplar's `filtered_attributes` as canonical JSON (empty when none).
    pub attributes: String,
}

/// Metrics query namespace, reached via [`Db::metrics`].
pub struct MetricsApi {
    pub(crate) db: Arc<Db>,
}

impl MetricsApi {
    /// The metric catalog: distinct (metric, unit, temporality) per kind (ARCHITECTURE.md §10.8). Covers all
    /// materialized tables — gauge, sum, histogram, exponential histogram, and summary (each carries
    /// the `metric`/`unit`/`temporality` identity columns; summaries leave `temporality` null).
    pub async fn catalog(&self) -> Result<Vec<MetricMeta>> {
        let mut out = Vec::new();
        for (table, kind) in [
            (Table::MetricsGauge, "gauge"),
            (Table::MetricsSum, "sum"),
            (Table::MetricsHistogram, "histogram"),
            (Table::MetricsExpHistogram, "exponential_histogram"),
            (Table::MetricsSummary, "summary"),
        ] {
            let sql = format!(
                "SELECT DISTINCT metric, unit, temporality FROM {}",
                table.as_str()
            );
            for b in &self.db.sql(&sql).collect().await? {
                for i in 0..b.num_rows() {
                    out.push(MetricMeta {
                        metric: get_str(b.column(0).as_ref(), i).unwrap_or_default(),
                        unit: get_str(b.column(1).as_ref(), i).unwrap_or_default(),
                        temporality: get_str(b.column(2).as_ref(), i),
                        kind: kind.to_owned(),
                    });
                }
            }
        }
        Ok(out)
    }

    /// Return raw, unaggregated metric points selected by the native typed query model.
    pub async fn points(&self, query: MetricPointsQuery) -> Result<Vec<MetricPoint>> {
        let mut params = SqlParams::with_promote(self.db.storage.promote().keys());
        let sql = query.to_sql(&mut params);
        let batches = self
            .db
            .sql_with_params(sql, params.into_values())
            .collect()
            .await?;
        materialize_metric_points(&batches, query.table)
    }

    /// The raw Arrow result of a point query — the same scan as [`points`](Self::points) but *without*
    /// materializing `MetricPoint` DTOs. Column layout matches the projection in
    /// `MetricPointsQuery::to_sql`: `point_time`(BIGINT)=0, `metric`=1, `service`=2, `attributes`=3,
    /// `temporality`=4, `is_monotonic`=5, then `value`=6 (scalar) or `explicit_bounds`/`bucket_counts`
    /// =6/7 (histogram). Lets a caller read only the columns it needs from the batch buffers.
    pub async fn points_batches(&self, query: MetricPointsQuery) -> Result<Vec<RecordBatch>> {
        let mut params = SqlParams::with_promote(self.db.storage.promote().keys());
        let sql = query.to_sql(&mut params);
        self.db
            .sql_with_params(sql, params.into_values())
            .collect()
            .await
    }

    /// A range query → a [`Matrix`] of series over `step` buckets.
    pub async fn range(&self, q: MetricQuery) -> Result<Matrix> {
        let mut params = SqlParams::with_promote(self.db.storage.promote().keys());
        let sql = q.range_sql(&mut params);
        let batches = self
            .db
            .sql_with_params(sql, params.into_values())
            .collect()
            .await?;
        materialize_matrix(&batches, &q.group_by)
    }

    /// A range query returning the raw Arrow batches (plus scan stats) instead of a materialized
    /// [`Matrix`] — the zero-copy-friendly entry point for a Go / FFI binding (ARCHITECTURE.md §10.17).
    /// Same SQL as [`range`](Self::range): each batch carries `bucket` (Int64 epoch-nanos), one
    /// `g0..gN` (Utf8) column per `group_by` label, then `v` (Float64) — the labels-as-columns shape
    /// an `arrow-go` consumer reads directly, ordered by `bucket`.
    #[cfg(feature = "proto")]
    pub async fn range_batches(
        &self,
        q: MetricQuery,
    ) -> Result<(Vec<arrow::record_batch::RecordBatch>, crate::QueryStats)> {
        let mut params = SqlParams::with_promote(self.db.storage.promote().keys());
        let sql = q.range_sql(&mut params);
        let started = std::time::Instant::now();
        let (_schema, batches, scan) = self
            .db
            .sql_with_params(sql, params.into_values())
            .collect_with_stats()
            .await?;
        let stats = crate::logs::batch_query_stats(&scan, &batches, started);
        Ok((batches, stats))
    }

    /// An instant query → a [`Vector`] (the last sample per series).
    pub async fn instant(&self, q: MetricQuery) -> Result<Vector> {
        let matrix = self.range(q).await?;
        let mut out = Vec::new();
        for series in matrix.0 {
            if let Some(last) = series.samples.last().cloned() {
                out.push(InstantSample {
                    labels: series.labels,
                    sample: last,
                });
            }
        }
        Ok(Vector(out))
    }

    /// An **exponential**-histogram quantile query over `metrics_exp_histogram` → a [`Matrix`] of
    /// phi-quantile series (ARCHITECTURE.md §10.8). Without [`ExpHistogramQuery::step`] each data point
    /// yields one sample (boundaries reconstructed from `scale`/`offset`). With `.step()`, points in
    /// a (bucket, label set) are scale-aligned (finer down-scaled to the coarsest) and summed before
    /// the quantile — the sound cross-series/time exp-histogram quantile.
    pub async fn exp_histogram_quantile(&self, q: ExpHistogramQuery) -> Result<Matrix> {
        let mut params = SqlParams::with_promote(self.db.storage.promote().keys());
        let sql = q.sql(&mut params);
        let batches = self
            .db
            .sql_with_params(sql, params.into_values())
            .collect()
            .await?;
        match q.step {
            None => materialize_exp_quantile(&batches, q.phi, &q.group_by),
            Some(_) => materialize_exp_merged(&batches, q.phi, &q.group_by),
        }
    }

    /// The distinct data-point attribute sets (series) for a metric, across all five metric tables
    /// (ARCHITECTURE.md §10.8 — the Prometheus `/series` analogue). Returns each unique `attributes` label
    /// set; resource-level dimensions like `service` are separate axes and are not folded in.
    pub async fn series(&self, metric: &str) -> Result<Vec<Attributes>> {
        let mut params = SqlParams::with_promote(self.db.storage.promote().keys());
        let m = params.str(metric);
        let sql = format!(
            "SELECT DISTINCT attributes FROM metrics_gauge WHERE metric = {m} \
             UNION SELECT DISTINCT attributes FROM metrics_sum WHERE metric = {m} \
             UNION SELECT DISTINCT attributes FROM metrics_histogram WHERE metric = {m} \
             UNION SELECT DISTINCT attributes FROM metrics_exp_histogram WHERE metric = {m} \
             UNION SELECT DISTINCT attributes FROM metrics_summary WHERE metric = {m}"
        );
        let mut out = Vec::new();
        for b in &self
            .db
            .sql_with_params(sql, params.into_values())
            .collect()
            .await?
        {
            for i in 0..b.num_rows() {
                if let Some(s) = get_str(b.column(0).as_ref(), i) {
                    out.push(Attributes::from_canonical_json(&s));
                }
            }
        }
        Ok(out)
    }

    /// All exemplars recorded for `metric` — the trace links to drill from a metric spike into an
    /// example trace (ARCHITECTURE.md §6.4). Unions the four point types that carry exemplars (gauge/sum/
    /// histogram/exp-histogram; summaries have none) and parses the stored `exemplars` JSON.
    pub async fn exemplars(&self, metric: &str) -> Result<Vec<Exemplar>> {
        let mut params = SqlParams::with_promote(self.db.storage.promote().keys());
        let m = params.str(metric);
        // Only rows that actually carry exemplars (the column is `'[]'` otherwise — the common case).
        let sql = format!(
            "SELECT exemplars FROM metrics_gauge WHERE metric = {m} AND exemplars <> '[]' \
             UNION ALL SELECT exemplars FROM metrics_sum WHERE metric = {m} AND exemplars <> '[]' \
             UNION ALL SELECT exemplars FROM metrics_histogram WHERE metric = {m} AND exemplars <> '[]' \
             UNION ALL SELECT exemplars FROM metrics_exp_histogram WHERE metric = {m} AND exemplars <> '[]'"
        );
        let mut out = Vec::new();
        for b in &self
            .db
            .sql_with_params(sql, params.into_values())
            .collect()
            .await?
        {
            for i in 0..b.num_rows() {
                if let Some(s) = get_str(b.column(0).as_ref(), i) {
                    parse_exemplars(&s, &mut out);
                }
            }
        }
        Ok(out)
    }

    /// A histogram-quantile query over the `metrics_histogram` table → a [`Matrix`] of
    /// phi-quantile series (ARCHITECTURE.md §10.8). Without [`HistogramQuery::step`] each explicit-bucket
    /// data point contributes one sample (its time, its `histogram_quantile`). With `.step()`, every
    /// data point in a (bucket, label set) is **merged** (bucket vectors summed element-wise) before
    /// the quantile — the correct cross-series/time quantile for the "p95 latency over time"
    /// dashboard.
    pub async fn histogram_quantile(&self, q: HistogramQuery) -> Result<Matrix> {
        match q.step {
            None => {
                let mut params = SqlParams::with_promote(self.db.storage.promote().keys());
                let sql = q.quantile_sql(&mut params);
                let batches = self
                    .db
                    .sql_with_params(sql, params.into_values())
                    .collect()
                    .await?;
                materialize_matrix(&batches, &q.group_by)
            }
            Some(step) => {
                let step_ns = step_nanos(step);
                let mut params = SqlParams::with_promote(self.db.storage.promote().keys());
                let sql = q.merged_sql(step_ns, &mut params);
                let batches = self
                    .db
                    .sql_with_params(sql, params.into_values())
                    .collect()
                    .await?;
                materialize_merged_quantile(&batches, q.phi, &q.group_by)
            }
        }
    }
}

/// Aggregation applied to the metric `value` within each bucket/series (ARCHITECTURE.md §10.8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Aggregation {
    Sum,
    Avg,
    Min,
    Max,
    Count,
}

impl Aggregation {
    fn sql(&self) -> &'static str {
        match self {
            Aggregation::Sum => "sum",
            Aggregation::Avg => "avg",
            Aggregation::Min => "min",
            Aggregation::Max => "max",
            Aggregation::Count => "count",
        }
    }
}

/// How a range query turns per-bucket samples into a value (ARCHITECTURE.md §10.8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
enum RateMode {
    /// Raw `<aggregation>(value)`.
    Off,
    /// `sum(value) / step_seconds` — per-second rate of a delta-temporality sum.
    Delta,
    /// `(max(value) - min(value)) / step_seconds` — per-second rate of a cumulative (monotonic)
    /// counter, i.e. its in-bucket increase over the bucket width.
    Counter,
}

/// A PromQL-style label selector operator (`=` / `!=` / `=~` / `!~`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
enum LabelOp {
    Eq,
    Ne,
    Regex,
    NotRegex,
}

/// Compile one metric-label selector with Prometheus missing-label semantics. [`SqlParams::attr_field`]
/// resolves `service` / `service.name` to the built-in `service` column, a promoted attribute key to
/// its dictionary column (pushdown), and everything else to a `json_get_str` scan — all identical in
/// result (ARCHITECTURE.md §6.1).
fn label_cond(key: &str, op: LabelOp, value: &str, p: &mut SqlParams) -> String {
    let field = p.attr_field(key);
    let actual = format!("coalesce({field}, '')");
    let v = p.str(value);
    match op {
        LabelOp::Eq => format!("{actual} = {v}"),
        LabelOp::Ne => format!("{actual} <> {v}"),
        LabelOp::Regex => format!("regexp_like({actual}, {v})"),
        LabelOp::NotRegex => format!("NOT regexp_like({actual}, {v})"),
    }
}

/// A typed metric query (ARCHITECTURE.md §10.8).
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct MetricQuery {
    table: Table,
    metric: String,
    aggregation: Aggregation,
    group_by: Vec<String>,
    filters: Vec<(String, LabelOp, String)>,
    range: Option<TimeRange>,
    step: Duration,
    rate: RateMode,
}

impl MetricQuery {
    /// Query a gauge metric (default aggregation: avg).
    pub fn gauge(metric: &str) -> Self {
        Self::new(Table::MetricsGauge, metric, Aggregation::Avg)
    }

    /// Query a sum metric (default aggregation: sum).
    pub fn sum(metric: &str) -> Self {
        Self::new(Table::MetricsSum, metric, Aggregation::Sum)
    }

    fn new(table: Table, metric: &str, aggregation: Aggregation) -> Self {
        MetricQuery {
            table,
            metric: metric.to_owned(),
            aggregation,
            group_by: Vec::new(),
            filters: Vec::new(),
            range: None,
            step: Duration::from_secs(60),
            rate: RateMode::Off,
        }
    }

    pub fn aggregation(mut self, a: Aggregation) -> Self {
        self.aggregation = a;
        self
    }
    /// Group series by an attribute key (repeatable).
    pub fn group_by(mut self, key: &str) -> Self {
        self.group_by.push(key.to_owned());
        self
    }
    /// Select series where label `key` equals `value` (PromQL `key="value"`; repeatable).
    pub fn filter(mut self, key: &str, value: &str) -> Self {
        self.filters
            .push((key.to_owned(), LabelOp::Eq, value.to_owned()));
        self
    }
    /// Select series where label `key` does NOT equal `value` (PromQL `key!="value"`). Series lacking
    /// the label are kept. Repeatable.
    pub fn filter_ne(mut self, key: &str, value: &str) -> Self {
        self.filters
            .push((key.to_owned(), LabelOp::Ne, value.to_owned()));
        self
    }
    /// Select series where label `key` matches the regex `pattern` (PromQL `key=~"pattern"`).
    /// Repeatable.
    pub fn filter_regex(mut self, key: &str, pattern: &str) -> Self {
        self.filters
            .push((key.to_owned(), LabelOp::Regex, pattern.to_owned()));
        self
    }
    /// Select series where label `key` does NOT match the regex `pattern` (PromQL `key!~"pattern"`).
    /// Series lacking the label are kept. Repeatable.
    pub fn filter_not_regex(mut self, key: &str, pattern: &str) -> Self {
        self.filters
            .push((key.to_owned(), LabelOp::NotRegex, pattern.to_owned()));
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
    /// Per-second rate of a **delta-temporality** sum (`rate_delta`, ARCHITECTURE.md §10.8): each bucket's
    /// value becomes `sum(value) / step_seconds` — the summed deltas over the bucket divided by its
    /// width (OTLP's common counter export is delta). For a cumulative counter use
    /// [`rate_counter`](Self::rate_counter) instead; `.rate()` on a cumulative sum overcounts.
    pub fn rate(mut self) -> Self {
        self.rate = RateMode::Delta;
        self
    }

    /// Per-second rate of a **cumulative** monotonic counter: each bucket's value becomes
    /// `(max(value) - min(value)) / step_seconds` — the counter's in-bucket increase over the
    /// bucket width. Assumes no counter reset within a bucket (the standard per-bucket estimate).
    pub fn rate_counter(mut self) -> Self {
        self.rate = RateMode::Counter;
        self
    }

    fn range_sql(&self, p: &mut SqlParams) -> String {
        let step_ns = step_nanos(self.step);
        let mut select = vec![format!(
            "(CAST(\"time\" AS BIGINT) / {step_ns}) * {step_ns} AS bucket"
        )];
        let mut group = vec!["bucket".to_owned()];
        for (i, k) in self.group_by.iter().enumerate() {
            select.push(format!("{} AS g{i}", p.attr_field(k)));
            group.push(format!("g{i}"));
        }
        let step_seconds = step_ns as f64 / 1e9;
        match self.rate {
            // CAST to DOUBLE so `count(value)` (Int64) matches the Float64 downcast in
            // `materialize_matrix`; a no-op for min/max/avg/sum which are already Float64.
            RateMode::Off => select.push(format!(
                "CAST({}(value) AS DOUBLE) AS v",
                self.aggregation.sql()
            )),
            RateMode::Delta => select.push(format!("sum(value) / {step_seconds:?} AS v")),
            RateMode::Counter => {
                select.push(format!("(max(value) - min(value)) / {step_seconds:?} AS v"))
            }
        }

        let mut conds = vec![format!("metric = {}", p.str(&self.metric))];
        if let Some(r) = &self.range {
            if r.start.0 != i64::MIN {
                conds.push(format!("CAST(\"time\" AS BIGINT) >= {}", p.i64(r.start.0)));
            }
            if r.end.0 != i64::MAX {
                conds.push(format!("CAST(\"time\" AS BIGINT) < {}", p.i64(r.end.0)));
            }
        }
        for (k, op, v) in &self.filters {
            conds.push(label_cond(k, *op, v, p));
        }

        format!(
            "SELECT {} FROM {} WHERE {} GROUP BY {} ORDER BY bucket",
            select.join(", "),
            self.table.as_str(),
            conds.join(" AND "),
            group.join(", ")
        )
    }
}

/// A typed histogram-quantile query over the `metrics_histogram` table (ARCHITECTURE.md §10.8). Computes
/// the `phi`-quantile of **each** explicit-bucket data point via the `histogram_quantile` UDF and
/// returns one sample (data-point time, quantile) per matching row, grouped into a labeled series
/// per `group_by` set. This is exact for the common one-point-per-series-per-scrape case; merging
/// bucket vectors across points/series (needs a bucket-summing aggregate) is a follow-up.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct HistogramQuery {
    metric: String,
    phi: f64,
    group_by: Vec<String>,
    filters: Vec<(String, LabelOp, String)>,
    range: Option<TimeRange>,
    step: Option<Duration>,
}

impl HistogramQuery {
    /// Query the named histogram metric (default quantile: p95).
    pub fn new(metric: &str) -> Self {
        HistogramQuery {
            metric: metric.to_owned(),
            phi: 0.95,
            group_by: Vec::new(),
            filters: Vec::new(),
            range: None,
            step: None,
        }
    }

    /// The quantile to estimate (0..1).
    pub fn quantile(mut self, phi: f64) -> Self {
        self.phi = phi;
        self
    }
    /// Group series by an attribute key (repeatable).
    pub fn group_by(mut self, key: &str) -> Self {
        self.group_by.push(key.to_owned());
        self
    }
    /// Filter by an attribute equality (repeatable).
    pub fn filter(mut self, key: &str, value: &str) -> Self {
        self.filters
            .push((key.to_owned(), LabelOp::Eq, value.to_owned()));
        self
    }
    /// Series where label `key` != `value` (PromQL `!=`); series lacking the label are kept.
    pub fn filter_ne(mut self, key: &str, value: &str) -> Self {
        self.filters
            .push((key.to_owned(), LabelOp::Ne, value.to_owned()));
        self
    }
    /// Series where label `key` matches regex `pattern` (PromQL `=~`).
    pub fn filter_regex(mut self, key: &str, pattern: &str) -> Self {
        self.filters
            .push((key.to_owned(), LabelOp::Regex, pattern.to_owned()));
        self
    }
    /// Series where label `key` does NOT match regex `pattern` (PromQL `!~`); missing label kept.
    pub fn filter_not_regex(mut self, key: &str, pattern: &str) -> Self {
        self.filters
            .push((key.to_owned(), LabelOp::NotRegex, pattern.to_owned()));
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
    /// Bucket by `step` and **merge** every data point's bucket vector within each (bucket, label
    /// set) before taking the quantile — the correct cross-series/time quantile (the `sum by (le)`
    /// in a PromQL `histogram_quantile`). Without `.step()` each data point yields its own sample.
    pub fn step(mut self, step: Duration) -> Self {
        self.step = Some(step);
        self
    }

    /// The `metric = … [AND range] [AND filters]` predicate shared by both SQL forms.
    fn conds(&self, p: &mut SqlParams) -> Vec<String> {
        let mut conds = vec![format!("metric = {}", p.str(&self.metric))];
        if let Some(r) = &self.range {
            if r.start.0 != i64::MIN {
                conds.push(format!("CAST(\"time\" AS BIGINT) >= {}", p.i64(r.start.0)));
            }
            if r.end.0 != i64::MAX {
                conds.push(format!("CAST(\"time\" AS BIGINT) < {}", p.i64(r.end.0)));
            }
        }
        for (k, op, v) in &self.filters {
            conds.push(label_cond(k, *op, v, p));
        }
        conds
    }

    fn quantile_sql(&self, p: &mut SqlParams) -> String {
        // Column shape matches `materialize_matrix`: bucket(Int64), g0..gN(Utf8), value(Float64).
        let mut select = vec!["CAST(\"time\" AS BIGINT) AS t".to_owned()];
        for (i, k) in self.group_by.iter().enumerate() {
            select.push(format!("{} AS g{i}", p.attr_field(k)));
        }
        // `{:?}` on an f64 always renders a float literal (e.g. `1.0`, `0.95`) so SQL types it as
        // Float64; the UDF also casts, but this keeps the plan's types clean.
        select.push(format!(
            "histogram_quantile({:?}, explicit_bounds, bucket_counts) AS v",
            self.phi
        ));

        format!(
            "SELECT {} FROM {} WHERE {} ORDER BY t",
            select.join(", "),
            Table::MetricsHistogram.as_str(),
            self.conds(p).join(" AND ")
        )
    }

    /// SQL for the `.step()` merge path: the raw bucket vectors per (time-bucket, label set), which
    /// [`materialize_merged_quantile`] sums element-wise before applying the quantile.
    fn merged_sql(&self, step_ns: i64, p: &mut SqlParams) -> String {
        let mut select = vec![format!(
            "(CAST(\"time\" AS BIGINT) / {step_ns}) * {step_ns} AS bucket"
        )];
        for (i, k) in self.group_by.iter().enumerate() {
            select.push(format!("{} AS g{i}", p.attr_field(k)));
        }
        select.push("explicit_bounds".to_owned());
        select.push("bucket_counts".to_owned());
        format!(
            "SELECT {} FROM {} WHERE {} ORDER BY bucket",
            select.join(", "),
            Table::MetricsHistogram.as_str(),
            self.conds(p).join(" AND ")
        )
    }
}

/// A typed exponential-histogram quantile query over `metrics_exp_histogram` (ARCHITECTURE.md §10.8).
/// Per-data-point: each row's base-2 buckets yield one phi-quantile sample.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ExpHistogramQuery {
    metric: String,
    phi: f64,
    group_by: Vec<String>,
    filters: Vec<(String, LabelOp, String)>,
    range: Option<TimeRange>,
    step: Option<Duration>,
}

impl ExpHistogramQuery {
    /// Query the named exponential-histogram metric (default quantile: p95).
    pub fn new(metric: &str) -> Self {
        ExpHistogramQuery {
            metric: metric.to_owned(),
            phi: 0.95,
            group_by: Vec::new(),
            filters: Vec::new(),
            range: None,
            step: None,
        }
    }
    pub fn quantile(mut self, phi: f64) -> Self {
        self.phi = phi;
        self
    }
    pub fn group_by(mut self, key: &str) -> Self {
        self.group_by.push(key.to_owned());
        self
    }
    pub fn filter(mut self, key: &str, value: &str) -> Self {
        self.filters
            .push((key.to_owned(), LabelOp::Eq, value.to_owned()));
        self
    }
    /// Series where label `key` != `value` (PromQL `!=`); series lacking the label are kept.
    pub fn filter_ne(mut self, key: &str, value: &str) -> Self {
        self.filters
            .push((key.to_owned(), LabelOp::Ne, value.to_owned()));
        self
    }
    /// Series where label `key` matches regex `pattern` (PromQL `=~`).
    pub fn filter_regex(mut self, key: &str, pattern: &str) -> Self {
        self.filters
            .push((key.to_owned(), LabelOp::Regex, pattern.to_owned()));
        self
    }
    /// Series where label `key` does NOT match regex `pattern` (PromQL `!~`); missing label kept.
    pub fn filter_not_regex(mut self, key: &str, pattern: &str) -> Self {
        self.filters
            .push((key.to_owned(), LabelOp::NotRegex, pattern.to_owned()));
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
    /// Bucket by `step` and **merge** every data point within each (bucket, label set) — aligning
    /// scales (down-scaling finer points) and summing bucket vectors — before the quantile. Without
    /// `.step()` each data point yields its own sample.
    pub fn step(mut self, step: Duration) -> Self {
        self.step = Some(step);
        self
    }

    fn sql(&self, p: &mut SqlParams) -> String {
        // Column shape consumed by the materializers: t, g0..gN, then the raw base-2 fields.
        let time_col = match self.step {
            Some(step) => {
                let step_ns = step_nanos(step);
                format!("(CAST(\"time\" AS BIGINT) / {step_ns}) * {step_ns} AS t")
            }
            None => "CAST(\"time\" AS BIGINT) AS t".to_owned(),
        };
        let mut select = vec![time_col];
        for (i, k) in self.group_by.iter().enumerate() {
            select.push(format!("{} AS g{i}", p.attr_field(k)));
        }
        select.push("scale".to_owned());
        select.push("zero_count".to_owned());
        select.push("positive_offset".to_owned());
        select.push("positive_counts".to_owned());
        select.push("negative_offset".to_owned());
        select.push("negative_counts".to_owned());

        let mut conds = vec![format!("metric = {}", p.str(&self.metric))];
        if let Some(r) = &self.range {
            if r.start.0 != i64::MIN {
                conds.push(format!("CAST(\"time\" AS BIGINT) >= {}", p.i64(r.start.0)));
            }
            if r.end.0 != i64::MAX {
                conds.push(format!("CAST(\"time\" AS BIGINT) < {}", p.i64(r.end.0)));
            }
        }
        for (k, op, v) in &self.filters {
            conds.push(label_cond(k, *op, v, p));
        }
        format!(
            "SELECT {} FROM {} WHERE {} ORDER BY t",
            select.join(", "),
            Table::MetricsExpHistogram.as_str(),
            conds.join(" AND ")
        )
    }
}

/// A time-value sample.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Sample {
    pub time: Timestamp,
    pub value: f64,
}

/// One labeled time series (ARCHITECTURE.md §10.8).
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct MetricSeries {
    pub labels: Vec<(String, String)>,
    pub samples: Vec<Sample>,
}

/// A range-query result: labeled series over time.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Matrix(pub Vec<MetricSeries>);

/// One labeled instant sample.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct InstantSample {
    pub labels: Vec<(String, String)>,
    pub sample: Sample,
}

/// An instant-query result: one sample per series.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Vector(pub Vec<InstantSample>);

/// Metric metadata (ARCHITECTURE.md §10.8: name, kind, unit, temporality).
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct MetricMeta {
    pub metric: String,
    pub unit: String,
    pub temporality: Option<String>,
    pub kind: String,
}

/// Duration → clamped positive nanoseconds for time bucketing. Avoids the `as i64` truncation an
/// absurdly large `Duration` (> ~292 years) would suffer, and guarantees ≥ 1 so it is always a safe
/// divisor / bucket width.
/// Parse one point's stored `exemplars` JSON array, appending each entry to `out`. Silently skips a
/// malformed blob or a non-object element (the column is engine-written, so this is defensive only).
fn parse_exemplars(json: &str, out: &mut Vec<Exemplar>) {
    let Some(AnyValue::Array(items)) = parse_json(json) else {
        return;
    };
    for item in items {
        let AnyValue::Map(pairs) = item else { continue };
        let get = |k: &str| pairs.iter().find(|(key, _)| key == k).map(|(_, v)| v);
        let time = match get("time_unix_nano") {
            Some(AnyValue::Int(t)) => Timestamp(*t),
            Some(AnyValue::Double(t)) => Timestamp(*t as i64),
            _ => Timestamp(0),
        };
        let value = match get("value") {
            Some(AnyValue::Double(v)) => *v,
            Some(AnyValue::Int(v)) => *v as f64,
            _ => f64::NAN,
        };
        let trace_id = get("trace_id")
            .and_then(AnyValue::as_str)
            .and_then(TraceId::from_hex);
        let span_id = get("span_id")
            .and_then(AnyValue::as_str)
            .and_then(SpanId::from_hex);
        let attributes = match get("attributes") {
            Some(v @ AnyValue::Map(_)) => canonical_json_value(v),
            _ => String::new(),
        };
        out.push(Exemplar {
            time,
            value,
            trace_id,
            span_id,
            attributes,
        });
    }
}

fn step_nanos(step: Duration) -> i64 {
    i64::try_from(step.as_nanos()).unwrap_or(i64::MAX).max(1)
}

/// Read a `List` row `i` into an owned `Vec<f64>` (empty if null / wrong child type).
fn list_f64(col: &ListArray, i: usize) -> Vec<f64> {
    if col.is_null(i) {
        return Vec::new();
    }
    let inner = col.value(i);
    match inner.as_any().downcast_ref::<Float64Array>() {
        Some(a) => (0..a.len()).map(|k| a.value(k)).collect(),
        None => Vec::new(),
    }
}

/// Read a `List` row `i` into an owned `Vec<u64>` (empty if null / wrong child type).
fn list_u64(col: &ListArray, i: usize) -> Vec<u64> {
    if col.is_null(i) {
        return Vec::new();
    }
    let inner = col.value(i);
    match inner.as_any().downcast_ref::<UInt64Array>() {
        Some(a) => (0..a.len()).map(|k| a.value(k)).collect(),
        None => Vec::new(),
    }
}

/// Materialize `SELECT bucket, g0…gN, explicit_bounds, bucket_counts` rows into a [`Matrix`] of
/// merged phi-quantiles: within each (bucket, label set) the bucket-count vectors are summed
/// element-wise (bounds taken from the first row) before applying [`histogram_quantile`]. This is
/// the sound cross-series/time histogram quantile — merge the distributions, then take the quantile.
fn materialize_merged_quantile(
    batches: &[RecordBatch],
    phi: f64,
    group_by: &[String],
) -> Result<Matrix> {
    let g = group_by.len();
    let bounds_idx = 1 + g;
    let counts_idx = 2 + g;

    // labels → (bucket → (bounds, merged_counts)); a Vec preserves first-seen series order.
    type Merged = std::collections::BTreeMap<i64, (Vec<f64>, Vec<u64>)>;
    let mut order: Vec<Vec<(String, String)>> = Vec::new();
    let mut series: std::collections::HashMap<Vec<(String, String)>, Merged> =
        std::collections::HashMap::new();

    for b in batches {
        let bucket = b
            .column(0)
            .as_any()
            .downcast_ref::<Int64Array>()
            .ok_or_else(|| imbh_core::Error::column_type("merged bucket", "Int64", None))?;
        let bounds_col = b
            .column(bounds_idx)
            .as_any()
            .downcast_ref::<ListArray>()
            .ok_or_else(|| {
                imbh_core::Error::column_type("explicit_bounds", "a List column", None)
            })?;
        let counts_col = b
            .column(counts_idx)
            .as_any()
            .downcast_ref::<ListArray>()
            .ok_or_else(|| imbh_core::Error::column_type("bucket_counts", "a List column", None))?;

        for i in 0..b.num_rows() {
            let labels: Vec<(String, String)> = group_by
                .iter()
                .enumerate()
                .map(|(j, key)| {
                    let v = get_str(b.column(1 + j).as_ref(), i).unwrap_or_default();
                    (key.clone(), v)
                })
                .collect();
            let bounds = list_f64(bounds_col, i);
            let counts = list_u64(counts_col, i);

            let buckets = series.entry(labels.clone()).or_insert_with(|| {
                order.push(labels.clone());
                Merged::new()
            });
            let entry = buckets
                .entry(bucket.value(i))
                .or_insert_with(|| (bounds.clone(), Vec::new()));
            // Element-wise count sums only align when the `le` edges match. A metric's
            // explicit_bounds are stable in practice; if a stray point in the same (labels, step)
            // group disagrees, skip it rather than fold mismatched buckets into a silently-wrong
            // quantile. (saturating_add guards adversarial per-bucket totals — see histogram.rs.)
            if entry.0 == bounds {
                if entry.1.len() < counts.len() {
                    entry.1.resize(counts.len(), 0);
                }
                for (slot, c) in entry.1.iter_mut().zip(counts.iter()) {
                    *slot = slot.saturating_add(*c);
                }
            }
        }
    }

    Ok(Matrix(
        order
            .into_iter()
            .map(|labels| {
                let buckets = series.remove(&labels).unwrap_or_default();
                let samples = buckets
                    .into_iter()
                    .map(|(t, (bounds, counts))| Sample {
                        time: Timestamp(t),
                        value: histogram_quantile(phi, &bounds, &counts),
                    })
                    .collect();
                MetricSeries { labels, samples }
            })
            .collect(),
    ))
}

/// Materialize `SELECT t, g0…gN, scale, zero_count, positive_offset, positive_counts,
/// negative_offset, negative_counts` rows into a [`Matrix`] of per-data-point exponential-histogram
/// quantiles (one sample per row, grouped into a series per label set).
fn materialize_exp_quantile(
    batches: &[RecordBatch],
    phi: f64,
    group_by: &[String],
) -> Result<Matrix> {
    let g = group_by.len();
    let (scale_i, zc_i, po_i, pc_i, no_i, nc_i) = (1 + g, 2 + g, 3 + g, 4 + g, 5 + g, 6 + g);

    let mut order: Vec<Vec<(String, String)>> = Vec::new();
    let mut series: std::collections::HashMap<Vec<(String, String)>, Vec<Sample>> =
        std::collections::HashMap::new();

    let i32_col = |b: &RecordBatch, idx: usize| -> Result<Int32Array> {
        b.column(idx)
            .as_any()
            .downcast_ref::<Int32Array>()
            .cloned()
            .ok_or_else(|| imbh_core::Error::query_msg("exp-histogram Int32 column mismatch"))
    };

    for b in batches {
        let t = b
            .column(0)
            .as_any()
            .downcast_ref::<Int64Array>()
            .ok_or_else(|| imbh_core::Error::column_type("exp-histogram time", "Int64", None))?;
        let scale = i32_col(b, scale_i)?;
        let zero_count = b
            .column(zc_i)
            .as_any()
            .downcast_ref::<UInt64Array>()
            .ok_or_else(|| imbh_core::Error::column_type("zero_count", "UInt64", None))?;
        let pos_off = i32_col(b, po_i)?;
        let neg_off = i32_col(b, no_i)?;
        let pos_counts = b
            .column(pc_i)
            .as_any()
            .downcast_ref::<ListArray>()
            .ok_or_else(|| imbh_core::Error::column_type("positive_counts", "a List", None))?;
        let neg_counts = b
            .column(nc_i)
            .as_any()
            .downcast_ref::<ListArray>()
            .ok_or_else(|| imbh_core::Error::column_type("negative_counts", "a List", None))?;

        for i in 0..b.num_rows() {
            let labels: Vec<(String, String)> = group_by
                .iter()
                .enumerate()
                .map(|(j, key)| {
                    let v = get_str(b.column(1 + j).as_ref(), i).unwrap_or_default();
                    (key.clone(), v)
                })
                .collect();
            let value = exp_histogram_quantile(
                phi,
                scale.value(i),
                zero_count.value(i),
                pos_off.value(i),
                &list_u64(pos_counts, i),
                neg_off.value(i),
                &list_u64(neg_counts, i),
            );
            let sample = Sample {
                time: Timestamp(t.value(i)),
                value,
            };
            match series.get_mut(&labels) {
                Some(s) => s.push(sample),
                None => {
                    order.push(labels.clone());
                    series.insert(labels, vec![sample]);
                }
            }
        }
    }

    Ok(Matrix(
        order
            .into_iter()
            .map(|labels| {
                let samples = series.remove(&labels).unwrap_or_default();
                MetricSeries { labels, samples }
            })
            .collect(),
    ))
}

/// One exponential-histogram data point's mergeable components.
struct ExpPoint {
    scale: i32,
    zero_count: u64,
    positive_offset: i32,
    positive_counts: Vec<u64>,
    negative_offset: i32,
    negative_counts: Vec<u64>,
}

/// Turn a sparse `bucket-index → count` map into a dense `(offset, counts)` pair (offset = min
/// index). Returns `None` if the index span is absurdly large — a guard against an OOM / i32-overflow
/// on adversarial bucket offsets; a well-formed histogram spans at most a few hundred buckets, so
/// this never rejects valid data. Span is computed in i64 to avoid overflow at the guard itself.
fn densify(map: &std::collections::BTreeMap<i32, u64>) -> Option<(i32, Vec<u64>)> {
    const MAX_SPAN: i64 = 1 << 20; // ~1M buckets — orders of magnitude beyond any real histogram
    let Some((&min, _)) = map.iter().next() else {
        return Some((0, Vec::new()));
    };
    let &max = map.keys().next_back().unwrap();
    let span = max as i64 - min as i64 + 1;
    if span > MAX_SPAN {
        return None;
    }
    let mut counts = vec![0u64; span as usize];
    for (&k, &v) in map {
        counts[(k - min) as usize] = v;
    }
    Some((min, counts))
}

/// Merge a group of exponential-histogram data points (possibly at different scales) → the
/// phi-quantile of the combined distribution. Down-scales every point to the coarsest scale (bucket
/// index `i` at scale `s` maps to `i >> (s - min_scale)`, i.e. floor-divide by the width ratio),
/// sums the aligned bucket counts, then applies [`exp_histogram_quantile`].
fn exp_merged_quantile(phi: f64, points: &[ExpPoint]) -> f64 {
    use std::collections::BTreeMap;
    let Some(min_scale) = points.iter().map(|p| p.scale).min() else {
        return f64::NAN;
    };
    let mut pos: BTreeMap<i32, u64> = BTreeMap::new();
    let mut neg: BTreeMap<i32, u64> = BTreeMap::new();
    let mut zero = 0u64;
    for p in points {
        zero = zero.saturating_add(p.zero_count);
        // Down-scale amount, in i64 to avoid overflow for extreme scales, clamped to a valid shift
        // (a delta >= 32 collapses buckets to index 0/-1 — the correct floor for such coarse
        // down-scaling — instead of panicking/masking on `>> delta`). Valid OTLP scales keep delta
        // well under 32. `saturating_add` likewise guards adversarial offsets near i32::MAX.
        let delta = (p.scale as i64 - min_scale as i64).clamp(0, 31) as u32;
        for (i, &c) in p.positive_counts.iter().enumerate() {
            if c != 0 {
                let slot = pos
                    .entry(p.positive_offset.saturating_add(i as i32) >> delta)
                    .or_insert(0);
                *slot = slot.saturating_add(c);
            }
        }
        for (i, &c) in p.negative_counts.iter().enumerate() {
            if c != 0 {
                let slot = neg
                    .entry(p.negative_offset.saturating_add(i as i32) >> delta)
                    .or_insert(0);
                *slot = slot.saturating_add(c);
            }
        }
    }
    // Pathological bucket span (adversarial offsets) → can't densify safely; report NaN.
    let (Some((po, pc)), Some((no, nc))) = (densify(&pos), densify(&neg)) else {
        return f64::NAN;
    };
    exp_histogram_quantile(phi, min_scale, zero, po, &pc, no, &nc)
}

/// Materialize the `.step()` merge path for exponential histograms: group data points by
/// (time-bucket, label set), scale-align + sum their bucket vectors, and take one quantile per group.
fn materialize_exp_merged(
    batches: &[RecordBatch],
    phi: f64,
    group_by: &[String],
) -> Result<Matrix> {
    use std::collections::BTreeMap;
    let g = group_by.len();
    let (scale_i, zc_i, po_i, pc_i, no_i, nc_i) = (1 + g, 2 + g, 3 + g, 4 + g, 5 + g, 6 + g);

    let mut order: Vec<Vec<(String, String)>> = Vec::new();
    type Grouped = BTreeMap<i64, Vec<ExpPoint>>;
    let mut series: std::collections::HashMap<Vec<(String, String)>, Grouped> =
        std::collections::HashMap::new();

    let i32_col = |b: &RecordBatch, idx: usize| -> Result<Int32Array> {
        b.column(idx)
            .as_any()
            .downcast_ref::<Int32Array>()
            .cloned()
            .ok_or_else(|| imbh_core::Error::query_msg("exp-histogram Int32 column mismatch"))
    };

    for b in batches {
        let t = b
            .column(0)
            .as_any()
            .downcast_ref::<Int64Array>()
            .ok_or_else(|| imbh_core::Error::column_type("exp-histogram time", "Int64", None))?;
        let scale = i32_col(b, scale_i)?;
        let zero_count = b
            .column(zc_i)
            .as_any()
            .downcast_ref::<UInt64Array>()
            .ok_or_else(|| imbh_core::Error::column_type("zero_count", "UInt64", None))?;
        let pos_off = i32_col(b, po_i)?;
        let neg_off = i32_col(b, no_i)?;
        let pos_counts = b
            .column(pc_i)
            .as_any()
            .downcast_ref::<ListArray>()
            .ok_or_else(|| imbh_core::Error::column_type("positive_counts", "a List", None))?;
        let neg_counts = b
            .column(nc_i)
            .as_any()
            .downcast_ref::<ListArray>()
            .ok_or_else(|| imbh_core::Error::column_type("negative_counts", "a List", None))?;

        for i in 0..b.num_rows() {
            let labels: Vec<(String, String)> = group_by
                .iter()
                .enumerate()
                .map(|(j, key)| {
                    let v = get_str(b.column(1 + j).as_ref(), i).unwrap_or_default();
                    (key.clone(), v)
                })
                .collect();
            let point = ExpPoint {
                scale: scale.value(i),
                zero_count: zero_count.value(i),
                positive_offset: pos_off.value(i),
                positive_counts: list_u64(pos_counts, i),
                negative_offset: neg_off.value(i),
                negative_counts: list_u64(neg_counts, i),
            };
            let buckets = series.entry(labels.clone()).or_insert_with(|| {
                order.push(labels.clone());
                Grouped::new()
            });
            buckets.entry(t.value(i)).or_default().push(point);
        }
    }

    Ok(Matrix(
        order
            .into_iter()
            .map(|labels| {
                let buckets = series.remove(&labels).unwrap_or_default();
                let samples = buckets
                    .into_iter()
                    .map(|(t, points)| Sample {
                        time: Timestamp(t),
                        value: exp_merged_quantile(phi, &points),
                    })
                    .collect();
                MetricSeries { labels, samples }
            })
            .collect(),
    ))
}

/// Materialize `SELECT bucket, g0…gN, v` rows into a [`Matrix`], grouping rows by their label set
/// (the `group_by` values) and ordering samples by bucket.
fn materialize_matrix(batches: &[RecordBatch], group_by: &[String]) -> Result<Matrix> {
    let value_idx = 1 + group_by.len();
    // Preserve first-seen series order with a parallel index.
    let mut order: Vec<Vec<(String, String)>> = Vec::new();
    let mut series: std::collections::HashMap<Vec<(String, String)>, Vec<Sample>> =
        std::collections::HashMap::new();

    for b in batches {
        let bucket = b
            .column(0)
            .as_any()
            .downcast_ref::<Int64Array>()
            .ok_or_else(|| imbh_core::Error::column_type("metric bucket", "Int64", None))?;
        let value = b
            .column(value_idx)
            .as_any()
            .downcast_ref::<Float64Array>()
            .ok_or_else(|| imbh_core::Error::column_type("metric value", "Float64", None))?;

        for i in 0..b.num_rows() {
            let labels: Vec<(String, String)> = group_by
                .iter()
                .enumerate()
                .map(|(j, key)| {
                    let v = get_str(b.column(1 + j).as_ref(), i).unwrap_or_default();
                    (key.clone(), v)
                })
                .collect();
            let sample = Sample {
                time: Timestamp(bucket.value(i)),
                value: value.value(i),
            };
            match series.get_mut(&labels) {
                Some(samples) => samples.push(sample),
                None => {
                    order.push(labels.clone());
                    series.insert(labels, vec![sample]);
                }
            }
        }
    }

    Ok(Matrix(
        order
            .into_iter()
            .map(|labels| {
                let samples = series.remove(&labels).unwrap_or_default();
                MetricSeries { labels, samples }
            })
            .collect(),
    ))
}

/// The raw point table selected by MetricPointsQuery.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetricPointKind {
    Gauge,
    Sum,
    Histogram,
}

impl MetricPointKind {
    fn table(self) -> Table {
        match self {
            Self::Gauge => Table::MetricsGauge,
            Self::Sum => Table::MetricsSum,
            Self::Histogram => Table::MetricsHistogram,
        }
    }
}

/// The value carried by one unaggregated metric point.
#[derive(Debug, Clone, PartialEq)]
pub enum MetricPointValue {
    Number(f64),
    Histogram {
        explicit_bounds: Vec<f64>,
        bucket_counts: Vec<u64>,
    },
}

/// One unaggregated native metric point.
#[derive(Debug, Clone, PartialEq)]
pub struct MetricPoint {
    pub time: Timestamp,
    pub metric: String,
    pub service: Option<String>,
    pub attributes: Attributes,
    pub temporality: Option<String>,
    pub is_monotonic: Option<bool>,
    pub value: MetricPointValue,
}

/// A bounded native query for unaggregated gauge, sum, or explicit-histogram points.
#[derive(Debug, Clone)]
pub struct MetricPointsQuery {
    table: Table,
    metric: String,
    filters: Vec<(String, LabelOp, String)>,
    match_none: bool,
    range: Option<TimeRange>,
    range_end_inclusive: bool,
    limit: usize,
}

impl MetricPointsQuery {
    pub fn gauge(metric: impl Into<String>) -> Self {
        Self::new(MetricPointKind::Gauge, metric)
    }

    pub fn sum(metric: impl Into<String>) -> Self {
        Self::new(MetricPointKind::Sum, metric)
    }

    pub fn histogram(metric: impl Into<String>) -> Self {
        Self::new(MetricPointKind::Histogram, metric)
    }

    fn new(kind: MetricPointKind, metric: impl Into<String>) -> Self {
        Self {
            table: kind.table(),
            metric: metric.into(),
            filters: Vec::new(),
            match_none: false,
            range: None,
            range_end_inclusive: false,
            limit: 100,
        }
    }

    pub fn filter(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.filters.push((key.into(), LabelOp::Eq, value.into()));
        self
    }

    pub fn filter_ne(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.filters.push((key.into(), LabelOp::Ne, value.into()));
        self
    }

    pub fn filter_regex(mut self, key: impl Into<String>, pattern: impl Into<String>) -> Self {
        self.filters
            .push((key.into(), LabelOp::Regex, pattern.into()));
        self
    }

    pub fn filter_not_regex(mut self, key: impl Into<String>, pattern: impl Into<String>) -> Self {
        self.filters
            .push((key.into(), LabelOp::NotRegex, pattern.into()));
        self
    }

    pub fn range(mut self, range: TimeRange) -> Self {
        self.range = Some(range);
        self.range_end_inclusive = false;
        self
    }

    /// Select a closed storage interval, including points exactly on `end`.
    pub fn range_inclusive(mut self, start: Timestamp, end: Timestamp) -> Self {
        self.range = Some(TimeRange::between(start, end));
        self.range_end_inclusive = true;
        self
    }
    /// Force an empty result while retaining a structurally valid native query.
    pub fn match_none(mut self) -> Self {
        self.match_none = true;
        self
    }

    pub fn limit(mut self, limit: usize) -> Self {
        self.limit = limit;
        self
    }

    fn to_sql(&self, params: &mut SqlParams) -> String {
        let mut conditions = vec![format!("metric = {}", params.str(&self.metric))];
        if self.match_none {
            conditions.push("FALSE".to_owned());
        }
        if let Some(range) = &self.range {
            if range.start.0 != i64::MIN {
                conditions.push(format!(
                    "CAST(\"time\" AS BIGINT) >= {}",
                    params.i64(range.start.0)
                ));
            }
            if range.end.0 != i64::MAX {
                let operator = if self.range_end_inclusive { "<=" } else { "<" };
                conditions.push(format!(
                    "CAST(\"time\" AS BIGINT) {operator} {}",
                    params.i64(range.end.0)
                ));
            }
        }
        for (key, op, value) in &self.filters {
            conditions.push(label_cond(key, *op, value, params));
        }
        let projection = if self.table == Table::MetricsHistogram {
            "CAST(\"time\" AS BIGINT) AS point_time, metric, CAST(service AS VARCHAR), attributes, \
             temporality, CAST(NULL AS BOOLEAN) AS is_monotonic, explicit_bounds, bucket_counts"
        } else {
            "CAST(\"time\" AS BIGINT) AS point_time, metric, CAST(service AS VARCHAR), attributes, \
             temporality, is_monotonic, value"
        };
        format!(
            "SELECT {projection} FROM {} WHERE {} ORDER BY \"time\" LIMIT {}",
            self.table.as_str(),
            conditions.join(" AND "),
            self.limit
        )
    }
}

fn materialize_metric_points(batches: &[RecordBatch], table: Table) -> Result<Vec<MetricPoint>> {
    let mut points = Vec::new();
    for batch in batches {
        let times = batch
            .column(0)
            .as_any()
            .downcast_ref::<Int64Array>()
            .ok_or_else(|| imbh_core::Error::column_type("metric point time", "Int64", None))?;
        let monotonic = batch
            .column(5)
            .as_any()
            .downcast_ref::<BooleanArray>()
            .ok_or_else(|| {
                imbh_core::Error::column_type("metric point monotonicity", "Boolean", None)
            })?;
        for row in 0..batch.num_rows() {
            let value = if table == Table::MetricsHistogram {
                let bounds = batch
                    .column(6)
                    .as_any()
                    .downcast_ref::<ListArray>()
                    .ok_or_else(|| {
                        imbh_core::Error::column_type(
                            "metric point explicit bounds",
                            "List<Float64>",
                            None,
                        )
                    })?;
                let counts = batch
                    .column(7)
                    .as_any()
                    .downcast_ref::<ListArray>()
                    .ok_or_else(|| {
                        imbh_core::Error::column_type(
                            "metric point bucket counts",
                            "List<UInt64>",
                            None,
                        )
                    })?;
                MetricPointValue::Histogram {
                    explicit_bounds: list_f64(bounds, row),
                    bucket_counts: list_u64(counts, row),
                }
            } else {
                let values = batch
                    .column(6)
                    .as_any()
                    .downcast_ref::<Float64Array>()
                    .ok_or_else(|| {
                        imbh_core::Error::column_type("metric point value", "Float64", None)
                    })?;
                MetricPointValue::Number(values.value(row))
            };
            points.push(MetricPoint {
                time: Timestamp(times.value(row)),
                metric: get_str(batch.column(1).as_ref(), row).unwrap_or_default(),
                service: get_str(batch.column(2).as_ref(), row),
                attributes: Attributes::from_canonical_json(
                    &get_str(batch.column(3).as_ref(), row).unwrap_or_default(),
                ),
                temporality: get_str(batch.column(4).as_ref(), row),
                is_monotonic: (!monotonic.is_null(row)).then(|| monotonic.value(row)),
                value,
            });
        }
    }
    Ok(points)
}

#[cfg(test)]
mod native_point_query_tests {
    use super::*;
    use datafusion::scalar::ScalarValue;

    #[test]
    fn raw_point_query_binds_metric_label_and_regex_inputs() {
        let metric = "metric' OR TRUE --";
        let key = "label') OR TRUE --";
        let value = "value') OR TRUE --";
        let pattern = ".*') OR TRUE --";
        let query = MetricPointsQuery::sum(metric)
            .filter(key, value)
            .filter_regex(key, pattern)
            .range_inclusive(Timestamp(10), Timestamp(20));
        let mut params = SqlParams::with_promote(&[]);
        let sql = query.to_sql(&mut params);

        for user_input in [metric, key, value, pattern] {
            assert!(!sql.contains(user_input));
        }
        assert!(sql.contains("<= $3"));
        assert_eq!(
            params.into_values(),
            vec![
                ScalarValue::Utf8(Some(metric.to_owned())),
                ScalarValue::Int64(Some(10)),
                ScalarValue::Int64(Some(20)),
                ScalarValue::Utf8(Some(key.to_owned())),
                ScalarValue::Utf8(Some(value.to_owned())),
                ScalarValue::Utf8(Some(key.to_owned())),
                ScalarValue::Utf8(Some(pattern.to_owned())),
            ]
        );
    }
}

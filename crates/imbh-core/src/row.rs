//! The normalized log row — the ingest hand-off type shared by `imbh-otlp` (producer) and
//! `imbh-storage` (consumer). Attribute scopes are already canonical-JSON strings here, so
//! storage just appends them to Arrow builders and query reads them back verbatim.
//!
//! Column semantics follow the `logs` table (ARCHITECTURE.md §6.2).

/// One normalized log record. `attributes`/`resource`/`scope` are canonical JSON objects
/// (produced via [`crate::canonical_json_object`]); `body` is raw text for simple string
/// bodies or canonical JSON for structured bodies.
#[derive(Debug, Clone, PartialEq)]
pub struct LogRow {
    pub time_unix_nano: i64,
    pub observed_time_unix_nano: Option<i64>,
    pub service: Option<String>,
    pub severity_number: u8,
    pub severity_text: Option<String>,
    pub body: String,
    pub attributes: String,
    pub resource: String,
    pub scope: String,
    pub trace_id: Option<[u8; 16]>,
    pub span_id: Option<[u8; 8]>,
    pub flags: u32,
}

impl LogRow {
    /// Approximate in-buffer heap cost, used to bound the mutable buffer by bytes
    /// (ARCHITECTURE.md §6.1/§7 — per-row `attributes` JSON dominates ingest memory).
    pub fn approx_bytes(&self) -> usize {
        const FIXED: usize = 64; // scalars + option/enum overhead, rough.
        FIXED
            + self.service.as_ref().map_or(0, |s| s.len())
            + self.severity_text.as_ref().map_or(0, |s| s.len())
            + self.body.len()
            + self.attributes.len()
            + self.resource.len()
            + self.scope.len()
    }
}

/// One normalized span record — the ingest hand-off type for the `spans` table (ARCHITECTURE.md §6.3).
/// `kind` (`SERVER`/`CLIENT`/…) and `status_code` (`UNSET`/`OK`/`ERROR`) are the OTel string
/// forms; `attributes`/`resource`/`scope`/`events`/`links` are canonical JSON.
#[derive(Debug, Clone, PartialEq)]
pub struct SpanRow {
    pub trace_id: [u8; 16],
    pub span_id: [u8; 8],
    pub parent_span_id: Option<[u8; 8]>,
    pub name: String,
    pub kind: String,
    pub start_time_unix_nano: i64,
    pub duration_ns: u64,
    pub status_code: String,
    pub status_message: Option<String>,
    pub service: Option<String>,
    pub attributes: String,
    pub resource: String,
    pub scope: String,
    pub events: Option<String>,
    pub links: Option<String>,
    pub trace_state: Option<String>,
    pub flags: u32,
}

impl SpanRow {
    /// Approximate in-buffer heap cost (ARCHITECTURE.md §6.1/§7).
    pub fn approx_bytes(&self) -> usize {
        const FIXED: usize = 96;
        FIXED
            + self.name.len()
            + self.kind.len()
            + self.status_code.len()
            + self.status_message.as_ref().map_or(0, |s| s.len())
            + self.service.as_ref().map_or(0, |s| s.len())
            + self.attributes.len()
            + self.resource.len()
            + self.scope.len()
            + self.events.as_ref().map_or(0, |s| s.len())
            + self.links.as_ref().map_or(0, |s| s.len())
            + self.trace_state.as_ref().map_or(0, |s| s.len())
    }
}

/// One normalized scalar metric point — the ingest hand-off type for the `metrics_gauge` and
/// `metrics_sum` tables (ARCHITECTURE.md §6.4). `temporality`/`is_monotonic` are set for sums only.
/// Histogram/exp-histogram/summary points get their own row types in a later M3 chunk.
#[derive(Debug, Clone, PartialEq)]
pub struct ScalarMetricRow {
    /// [`crate::Table::MetricsGauge`] or [`crate::Table::MetricsSum`].
    pub table: crate::Table,
    pub time_unix_nano: i64,
    pub start_time_unix_nano: Option<i64>,
    pub metric: String,
    pub unit: String,
    pub service: Option<String>,
    pub attributes: String,
    pub resource: String,
    pub scope: String,
    pub flags: u32,
    pub value: f64,
    pub temporality: Option<String>,
    pub is_monotonic: Option<bool>,
    /// OTLP exemplars for this point as a canonical-JSON array (`"[]"` when none). Each entry links a
    /// sampled value to a trace: `{"time_unix_nano","value","trace_id","span_id"}` (ARCHITECTURE.md §6.4 —
    /// the metric→trace drill-down). Low volume; stored on the row rather than a side table.
    pub exemplars: String,
}

impl ScalarMetricRow {
    /// Approximate in-buffer heap cost (ARCHITECTURE.md §6.1/§7).
    pub fn approx_bytes(&self) -> usize {
        const FIXED: usize = 80;
        FIXED
            + self.metric.len()
            + self.unit.len()
            + self.service.as_ref().map_or(0, |s| s.len())
            + self.attributes.len()
            + self.resource.len()
            + self.scope.len()
            + self.exemplars.len()
    }
}

/// One normalized explicit-bucket histogram point — the ingest hand-off type for the
/// [`crate::Table::MetricsHistogram`] table (ARCHITECTURE.md §6.4). OTLP explicit-bucket histograms carry
/// `explicit_bounds` (N ascending boundaries) and `bucket_counts` (N+1 per-bucket counts, the last
/// being the `+Inf` overflow bucket): the invariant is `bucket_counts.len() ==
/// explicit_bounds.len() + 1`. `sum`/`min`/`max` are optional in the OTLP data model. The identity
/// fields (`metric`/`service`/`attributes`/`resource`/`scope`) mirror [`ScalarMetricRow`].
#[derive(Debug, Clone, PartialEq)]
pub struct HistogramRow {
    pub time_unix_nano: i64,
    pub start_time_unix_nano: Option<i64>,
    pub metric: String,
    pub unit: String,
    pub service: Option<String>,
    pub attributes: String,
    pub resource: String,
    pub scope: String,
    pub flags: u32,
    pub count: u64,
    pub sum: Option<f64>,
    pub min: Option<f64>,
    pub max: Option<f64>,
    /// N ascending bucket boundaries (upper bounds, exclusive of `+Inf`).
    pub explicit_bounds: Vec<f64>,
    /// N+1 counts per bucket (cumulative-vs-delta follows `temporality`).
    pub bucket_counts: Vec<u64>,
    pub temporality: Option<String>,
    /// OTLP exemplars as a canonical-JSON array (empty when none) — see [`ScalarMetricRow::exemplars`].
    pub exemplars: String,
}

impl HistogramRow {
    /// Approximate in-buffer heap cost (ARCHITECTURE.md §6.1/§7).
    pub fn approx_bytes(&self) -> usize {
        const FIXED: usize = 112;
        FIXED
            + self.metric.len()
            + self.unit.len()
            + self.service.as_ref().map_or(0, |s| s.len())
            + self.attributes.len()
            + self.resource.len()
            + self.scope.len()
            + self.explicit_bounds.len() * 8
            + self.bucket_counts.len() * 8
            + self.exemplars.len()
    }
}

/// One normalized exponential (base-2) histogram point — the ingest hand-off type for the
/// [`crate::Table::MetricsExpHistogram`] table (ARCHITECTURE.md §6.4). OTLP exponential histograms place
/// bucket boundaries at powers of `base = 2^(2^-scale)`: bucket `index` spans
/// `(base^index, base^(index+1)]`. The positive and negative value ranges each have an `offset`
/// (the index of the first bucket) and a dense `bucket_counts` slice; `zero_count` holds values
/// within `zero_threshold` of 0. Identity fields mirror [`ScalarMetricRow`]/[`HistogramRow`].
#[derive(Debug, Clone, PartialEq)]
pub struct ExpHistogramRow {
    pub time_unix_nano: i64,
    pub start_time_unix_nano: Option<i64>,
    pub metric: String,
    pub unit: String,
    pub service: Option<String>,
    pub attributes: String,
    pub resource: String,
    pub scope: String,
    pub flags: u32,
    pub count: u64,
    pub sum: Option<f64>,
    pub min: Option<f64>,
    pub max: Option<f64>,
    /// Resolution: boundaries at powers of `base = 2^(2^-scale)`.
    pub scale: i32,
    pub zero_count: u64,
    pub zero_threshold: f64,
    /// Index of the first positive bucket; `positive_counts[i]` is bucket `positive_offset + i`.
    pub positive_offset: i32,
    pub positive_counts: Vec<u64>,
    /// Index of the first negative bucket (absolute-value mapped, same scale).
    pub negative_offset: i32,
    pub negative_counts: Vec<u64>,
    pub temporality: Option<String>,
    /// OTLP exemplars as a canonical-JSON array (empty when none) — see [`ScalarMetricRow::exemplars`].
    pub exemplars: String,
}

impl ExpHistogramRow {
    /// Approximate in-buffer heap cost (ARCHITECTURE.md §6.1/§7).
    pub fn approx_bytes(&self) -> usize {
        const FIXED: usize = 136;
        FIXED
            + self.metric.len()
            + self.unit.len()
            + self.service.as_ref().map_or(0, |s| s.len())
            + self.attributes.len()
            + self.resource.len()
            + self.scope.len()
            + self.positive_counts.len() * 8
            + self.negative_counts.len() * 8
            + self.exemplars.len()
    }
}

/// One normalized summary point — the ingest hand-off type for the [`crate::Table::MetricsSummary`]
/// table (ARCHITECTURE.md §6.4). OTLP summaries carry precomputed quantiles: `quantiles[i]` (a phi in
/// `[0,1]`) has value `values[i]`, with `quantiles.len() == values.len()`. Identity fields mirror
/// [`ScalarMetricRow`]. Summaries have no aggregation temporality in OTLP.
#[derive(Debug, Clone, PartialEq)]
pub struct SummaryRow {
    pub time_unix_nano: i64,
    pub start_time_unix_nano: Option<i64>,
    pub metric: String,
    pub unit: String,
    pub service: Option<String>,
    pub attributes: String,
    pub resource: String,
    pub scope: String,
    pub flags: u32,
    pub count: u64,
    pub sum: f64,
    /// Quantile levels (phi in `[0,1]`), paired index-for-index with `values`.
    pub quantiles: Vec<f64>,
    pub values: Vec<f64>,
}

impl SummaryRow {
    /// Approximate in-buffer heap cost (ARCHITECTURE.md §6.1/§7).
    pub fn approx_bytes(&self) -> usize {
        const FIXED: usize = 96;
        FIXED
            + self.metric.len()
            + self.unit.len()
            + self.service.as_ref().map_or(0, |s| s.len())
            + self.attributes.len()
            + self.resource.len()
            + self.scope.len()
            + self.quantiles.len() * 8
            + self.values.len() * 8
    }
}

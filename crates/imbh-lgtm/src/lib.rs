//! LGTM-stack query-language compatibility for IMBH.
//!
//! This crate implements the query surfaces of the Grafana LGTM observability stack — PromQL
//! (Prometheus/Mimir), LogQL (Loki), and TraceQL (Tempo) — as bounded, explicitly-versioned
//! compatibility profiles. It is deliberately stack-specific, not a neutral cross-ecosystem query
//! layer; unsupported valid constructs are rejected with a stable diagnostic rather than approximated.
//!
//! Two layers, kept as separate modules:
//!
//! - [`mod@model`] — parser- and engine-independent expression models plus the reference evaluators
//!   (the "semantics"). Depends only on `regex`.
//! - [`mod@syntax`] — source-positioned parsers that lower PromQL/LogQL/TraceQL text into an
//!   [`ImbhQueryModel`], emitting a [`Diagnostic`] for anything outside the advertised profiles.
//!
//! Under the optional `source` feature the crate also owns the native IMBH source adapters and the
//! `*SemanticsExt` execution traits, which depend on the `imbh` facade (and thus DataFusion/Tantivy).
//! That subtree is feature-gated so parse-only or evaluate-only consumers stay light.

#[cfg(feature = "source")]
mod batch;
mod model;
#[cfg(feature = "source")]
mod source;
mod syntax;

// ── model + reference evaluators ────────────────────────────────────────────────────────────────
pub use model::common::{
    Annotation, EvalLimits, EvalRange, FetchBounds, LOGQL_PROFILE, LabelSet, PROMQL_PROFILE,
    SemanticError, SemanticProfile, TRACEQL_PROFILE,
};
pub use model::logql::{
    LogEntry, LogEntryPack, LogEntrySource, LogFetchRequest, LogFilter, LogLabelSource,
    LogPipelineError, LogPipelineState, LogRangeExpr, LogRangeOp, LogSeries, LogStreamSchema,
    LogValue, eval_log_range_reference, execute_log_range, plan_log_fetch,
};
pub use model::promql::{
    ClassicHistogramBucket, FloatSample, Grouping, HistogramPoint, HistogramQuantileResult,
    LabelMatcher, MatchOp, PromAggregate, PromExpr, PromFetchPlan, PromFetchPurpose,
    PromFetchRequest, PromHistogramPack, PromHistogramSeries, PromSeries, PromSeriesPack,
    PromSeriesSource, aggregate_instant, classic_histogram_quantile, eval_prom_reference,
    eval_prom_with_histograms_reference, execute_prom, extrapolated_rate, plan_prom_fetch,
    select_instant, select_range,
};
pub use model::traceql::{
    AttributeScope, Intrinsic, SemanticSpan, SemanticTrace, SemanticValue, SpanCandidateFilter,
    SpanPredicate, SpansetExpr, StructuralOp, TraceCompareOp, TraceFetchRequest, TracePack,
    TraceQueryMatch, TraceSource, TraceSpanset, TypedAttributes, candidate_filters,
    eval_spanset_reference, eval_trace_reference, execute_traceql, plan_trace_fetch,
};

// ── parsers / translators ───────────────────────────────────────────────────────────────────────
pub use syntax::{
    Diagnostic, DiagnosticCode, ImbhQueryModel, MetricKind, MetricResolution, SourceRange,
    TranslateContext, TranslateResult, TranslatedQuery, translate_logql, translate_promql,
    translate_traceql,
};

// ── native source adapters (opt-in) ──────────────────────────────────────────────────────────────
#[cfg(feature = "source")]
pub use batch::{
    log_matrix_schema, log_series_to_batch, prom_histogram_schema, prom_histogram_to_batch,
    prom_matrix_schema, prom_series_to_batch, trace_matches_schema, trace_matches_to_batch,
};
#[cfg(feature = "source")]
pub use source::{
    LogsSemanticsExt, MetricsSemanticsExt, StreamLabelReader, TracesSemanticsExt, build_log_query,
    build_metric_point_queries, build_trace_query, metric_labels_from_batch,
};

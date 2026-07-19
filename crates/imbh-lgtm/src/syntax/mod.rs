//! Source-positioned translators for the conformant IMBH query-language profiles.
//!
//! The parsers intentionally cover only the advertised P1/L1/T1 capabilities. Valid syntax outside
//! those profiles is rejected with a stable diagnostic instead of being approximated. Lowered output
//! is expressed in the [`crate::model`] expression types, re-exported at the crate root.

mod logql;
mod parser;
mod promql;
mod traceql;

use crate::{
    LOGQL_PROFILE, PROMQL_PROFILE, PromExpr, SemanticProfile, SpansetExpr, TRACEQL_PROFILE,
};

pub use logql::translate_logql;
pub use promql::translate_promql;
pub use traceql::translate_traceql;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceRange {
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticCode {
    Syntax,
    Unsupported,
    NeedsResolution,
    SemanticMismatch,
    LimitExceeded,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub code: DiagnosticCode,
    pub range: SourceRange,
    pub message: String,
}

impl Diagnostic {
    pub(crate) fn new(
        code: DiagnosticCode,
        start: usize,
        end: usize,
        message: impl Into<String>,
    ) -> Self {
        Self {
            code,
            range: SourceRange { start, end },
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetricKind {
    Gauge,
    CumulativeCounter,
    CumulativeHistogram,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetricResolution {
    pub query_name: String,
    pub storage_name: String,
    pub kind: MetricKind,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TranslateContext {
    pub metrics: Vec<MetricResolution>,
}

impl TranslateContext {
    pub fn resolve_any_metric(
        &self,
        query_name: &str,
        range: SourceRange,
    ) -> Result<&MetricResolution, Diagnostic> {
        let mut matches = self
            .metrics
            .iter()
            .filter(|metric| metric.query_name == query_name);
        let Some(metric) = matches.next() else {
            return Err(Diagnostic::new(
                DiagnosticCode::NeedsResolution,
                range.start,
                range.end,
                format!("metric {query_name:?} is not resolved"),
            ));
        };
        if matches.next().is_some() {
            return Err(Diagnostic::new(
                DiagnosticCode::NeedsResolution,
                range.start,
                range.end,
                format!("metric {query_name:?} resolves ambiguously"),
            ));
        }
        Ok(metric)
    }
    pub fn resolve_metric(
        &self,
        query_name: &str,
        expected: MetricKind,
        range: SourceRange,
    ) -> Result<&str, Diagnostic> {
        let mut matches = self
            .metrics
            .iter()
            .filter(|metric| metric.query_name == query_name && metric.kind == expected);
        let Some(metric) = matches.next() else {
            return Err(Diagnostic::new(
                DiagnosticCode::NeedsResolution,
                range.start,
                range.end,
                format!("metric {query_name:?} is not resolved as {expected:?}"),
            ));
        };
        if matches.next().is_some() {
            return Err(Diagnostic::new(
                DiagnosticCode::NeedsResolution,
                range.start,
                range.end,
                format!("metric {query_name:?} resolves ambiguously"),
            ));
        }
        Ok(&metric.storage_name)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ImbhQueryModel {
    Prom(PromExpr),
    Log(crate::LogRangeExpr),
    /// A bare LogQL log query (stream selector + line filters, no range aggregation): filters and
    /// returns log lines rather than synthesizing a metric series.
    LogSelector(crate::LogFilter),
    Trace(SpansetExpr),
}

#[derive(Debug, Clone, PartialEq)]
pub struct TranslatedQuery {
    pub model: ImbhQueryModel,
    pub profile: SemanticProfile,
}

impl TranslatedQuery {
    pub(crate) fn prom(model: PromExpr) -> Self {
        Self {
            model: ImbhQueryModel::Prom(model),
            profile: PROMQL_PROFILE,
        }
    }

    pub(crate) fn log(model: crate::LogRangeExpr) -> Self {
        Self {
            model: ImbhQueryModel::Log(model),
            profile: LOGQL_PROFILE,
        }
    }

    pub(crate) fn log_selector(model: crate::LogFilter) -> Self {
        Self {
            model: ImbhQueryModel::LogSelector(model),
            profile: LOGQL_PROFILE,
        }
    }

    pub(crate) fn trace(model: SpansetExpr) -> Self {
        Self {
            model: ImbhQueryModel::Trace(model),
            profile: TRACEQL_PROFILE,
        }
    }
}

pub type TranslateResult = Result<TranslatedQuery, Diagnostic>;

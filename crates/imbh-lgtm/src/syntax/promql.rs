use crate::{Grouping, LabelMatcher, MatchOp, PromAggregate, PromExpr};

use super::parser::Cursor;
use crate::{
    Diagnostic, DiagnosticCode, MetricKind, TranslateContext, TranslateResult, TranslatedQuery,
};

pub fn translate_promql(source: &str, context: &TranslateContext) -> TranslateResult {
    let mut cursor = Cursor::new(source);
    let expression = parse_expression(&mut cursor, context, 0)?;
    cursor.finish()?;
    Ok(TranslatedQuery::prom(expression))
}

fn parse_expression(
    cursor: &mut Cursor<'_>,
    context: &TranslateContext,
    depth: usize,
) -> Result<PromExpr, Diagnostic> {
    if depth >= 128 {
        return Err(cursor.error(
            DiagnosticCode::LimitExceeded,
            "PromQL expression nesting exceeds 128",
        ));
    }
    if cursor.consume("(") {
        let expression = parse_expression(cursor, context, depth + 1)?;
        cursor.expect(")")?;
        return Ok(expression);
    }

    let (name, range) = cursor.identifier()?;
    match name.as_str() {
        "sum" | "avg" | "min" | "max" | "count" => {
            let operation = match name.as_str() {
                "sum" => PromAggregate::Sum,
                "avg" => PromAggregate::Avg,
                "min" => PromAggregate::Min,
                "max" => PromAggregate::Max,
                "count" => PromAggregate::Count,
                _ => unreachable!(),
            };
            let prefix_grouping = parse_optional_grouping(cursor)?;
            cursor.expect("(")?;
            let input = parse_expression(cursor, context, depth + 1)?;
            cursor.expect(")")?;
            let suffix_grouping = parse_optional_grouping(cursor)?;
            if prefix_grouping.is_some() && suffix_grouping.is_some() {
                return Err(cursor.error(
                    DiagnosticCode::Syntax,
                    "aggregation grouping may appear before or after the operand, not both",
                ));
            }
            let grouping = prefix_grouping
                .or(suffix_grouping)
                .unwrap_or_else(|| Grouping::By(Vec::new()));
            Ok(PromExpr::Aggregate {
                op: operation,
                grouping,
                input: Box::new(input),
            })
        }
        "rate" => {
            cursor.expect("(")?;
            let (metric, metric_range) = cursor.identifier()?;
            let storage = context
                .resolve_metric(&metric, MetricKind::CumulativeCounter, metric_range)?
                .to_owned();
            let matchers = parse_selector(cursor, storage)?;
            cursor.expect("[")?;
            let window_ns = cursor.duration_ns()?;
            cursor.expect("]")?;
            cursor.expect(")")?;
            Ok(PromExpr::Rate {
                matchers,
                window_ns,
            })
        }
        "histogram_quantile" => parse_histogram_quantile(cursor, context, depth + 1),
        _ => {
            let metric = context.resolve_any_metric(&name, range)?;
            if metric.kind == MetricKind::CumulativeHistogram {
                return Err(Diagnostic::new(
                    DiagnosticCode::Unsupported,
                    range.start,
                    range.end,
                    "histogram selectors require canonical histogram_quantile(sum by (le, ...)(rate(...)))",
                ));
            }
            Ok(PromExpr::Selector {
                matchers: parse_selector(cursor, metric.storage_name.clone())?,
            })
        }
    }
}

fn parse_histogram_quantile(
    cursor: &mut Cursor<'_>,
    context: &TranslateContext,
    depth: usize,
) -> Result<PromExpr, Diagnostic> {
    if depth >= 128 {
        return Err(cursor.error(
            DiagnosticCode::LimitExceeded,
            "PromQL expression nesting exceeds 128",
        ));
    }
    cursor.expect("(")?;
    let phi = cursor.float()?;
    cursor.expect(",")?;
    let (sum, sum_range) = cursor.identifier()?;
    if sum != "sum" {
        return Err(Diagnostic::new(
            DiagnosticCode::SemanticMismatch,
            sum_range.start,
            sum_range.end,
            "P1 histogram_quantile requires sum by (le, ...)",
        ));
    }
    let prefix_grouping = parse_optional_grouping(cursor)?;
    cursor.expect("(")?;
    let (rate, rate_range) = cursor.identifier()?;
    if rate != "rate" {
        return Err(Diagnostic::new(
            DiagnosticCode::SemanticMismatch,
            rate_range.start,
            rate_range.end,
            "histogram_quantile requires rate() before bucket aggregation",
        ));
    }
    cursor.expect("(")?;
    let (metric, metric_range) = cursor.identifier()?;
    let storage = context
        .resolve_metric(&metric, MetricKind::CumulativeHistogram, metric_range)?
        .to_owned();
    let matchers = parse_selector(cursor, storage)?;
    cursor.expect("[")?;
    let window_ns = cursor.duration_ns()?;
    cursor.expect("]")?;
    cursor.expect(")")?;
    cursor.expect(")")?;
    let suffix_grouping = parse_optional_grouping(cursor)?;
    if prefix_grouping.is_some() && suffix_grouping.is_some() {
        return Err(cursor.error(
            DiagnosticCode::Syntax,
            "aggregation grouping may appear before or after the operand, not both",
        ));
    }
    let grouping = prefix_grouping.or(suffix_grouping).ok_or_else(|| {
        cursor.error(
            DiagnosticCode::SemanticMismatch,
            "histogram_quantile requires sum by (le, ...)",
        )
    })?;
    let Grouping::By(mut labels) = grouping else {
        return Err(cursor.error(
            DiagnosticCode::SemanticMismatch,
            "histogram_quantile requires by(le, ...) grouping",
        ));
    };
    let Some(le_index) = labels.iter().position(|label| label == "le") else {
        return Err(cursor.error(
            DiagnosticCode::SemanticMismatch,
            "histogram_quantile grouping must contain le",
        ));
    };
    labels.remove(le_index);

    cursor.expect(")")?;
    Ok(PromExpr::HistogramQuantile {
        phi,
        matchers,
        window_ns,
        grouping: Grouping::By(labels),
    })
}

fn parse_grouping(cursor: &mut Cursor<'_>) -> Result<Grouping, Diagnostic> {
    let (keyword, range) = cursor.identifier()?;
    let by = match keyword.as_str() {
        "by" => true,
        "without" => false,
        _ => {
            return Err(Diagnostic::new(
                DiagnosticCode::Syntax,
                range.start,
                range.end,
                "expected by or without grouping",
            ));
        }
    };
    cursor.expect("(")?;
    let mut labels = Vec::new();
    if !cursor.consume(")") {
        loop {
            labels.push(cursor.identifier()?.0);
            if cursor.consume(")") {
                break;
            }
            cursor.expect(",")?;
        }
    }
    Ok(if by {
        Grouping::By(labels)
    } else {
        Grouping::Without(labels)
    })
}
fn parse_optional_grouping(cursor: &mut Cursor<'_>) -> Result<Option<Grouping>, Diagnostic> {
    let mut probe = cursor.clone();
    if probe.consume_keyword("by") || probe.consume_keyword("without") {
        parse_grouping(cursor).map(Some)
    } else {
        Ok(None)
    }
}

fn parse_selector(
    cursor: &mut Cursor<'_>,
    storage_metric: String,
) -> Result<Vec<LabelMatcher>, Diagnostic> {
    let mut matchers = vec![LabelMatcher {
        name: "__name__".to_owned(),
        op: MatchOp::Eq,
        value: storage_metric,
    }];
    if !cursor.consume("{") {
        return Ok(matchers);
    }
    if cursor.consume("}") {
        return Ok(matchers);
    }
    loop {
        let (name, _) = cursor.identifier()?;
        let operation = if cursor.consume("=~") {
            MatchOp::Regex
        } else if cursor.consume("!~") {
            MatchOp::NotRegex
        } else if cursor.consume("!=") {
            MatchOp::Ne
        } else if cursor.consume("=") {
            MatchOp::Eq
        } else {
            return Err(cursor.error(
                DiagnosticCode::Syntax,
                "expected PromQL label matcher operator",
            ));
        };
        let value = cursor.quoted_string()?;
        matchers.push(LabelMatcher {
            name,
            op: operation,
            value,
        });
        if cursor.consume("}") {
            break;
        }
        cursor.expect(",")?;
    }
    Ok(matchers)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ImbhQueryModel, MetricResolution};

    fn context() -> TranslateContext {
        TranslateContext {
            metrics: vec![
                MetricResolution {
                    query_name: "requests_total".into(),
                    storage_name: "requests_total".into(),
                    kind: MetricKind::CumulativeCounter,
                },
                MetricResolution {
                    query_name: "request_duration_bucket".into(),
                    storage_name: "request_duration".into(),
                    kind: MetricKind::CumulativeHistogram,
                },
            ],
        }
    }

    #[test]
    fn translates_rate_aggregation() {
        let translated = translate_promql(
            r#"sum by (route) (rate(requests_total{service=~"api.*"}[5m]))"#,
            &context(),
        )
        .unwrap();
        assert_eq!(translated.profile.capability_id, "imbh.promql.p1.v1");
        let ImbhQueryModel::Prom(PromExpr::Aggregate { input, .. }) = translated.model else {
            panic!("unexpected model")
        };
        assert!(matches!(*input, PromExpr::Rate { .. }));
    }
    #[test]
    fn translates_ungrouped_and_suffix_aggregations() {
        for query in [
            "sum(rate(requests_total[1h30m]))",
            "sum(rate(requests_total[5m])) by (route)",
        ] {
            let translated = translate_promql(query, &context()).unwrap();
            assert!(matches!(
                translated.model,
                ImbhQueryModel::Prom(PromExpr::Aggregate { .. })
            ));
        }
    }

    #[test]
    fn translates_histogram_quantile_with_suffix_grouping() {
        translate_promql(
            "histogram_quantile(0.95, sum(rate(request_duration_bucket[5m])) by (le, route))",
            &context(),
        )
        .unwrap();
    }

    #[test]
    fn translates_canonical_histogram_quantile() {
        let translated = translate_promql(
            "histogram_quantile(0.95, sum by (le, route) (rate(request_duration_bucket[5m])))",
            &context(),
        )
        .unwrap();
        let ImbhQueryModel::Prom(PromExpr::HistogramQuantile {
            grouping: Grouping::By(labels),
            ..
        }) = translated.model
        else {
            panic!("unexpected model")
        };
        assert_eq!(labels, vec!["route"]);
    }

    #[test]
    fn rate_requires_catalog_resolution() {
        let error = translate_promql("rate(unknown_total[5m])", &context()).unwrap_err();
        assert_eq!(error.code, DiagnosticCode::NeedsResolution);
    }
}

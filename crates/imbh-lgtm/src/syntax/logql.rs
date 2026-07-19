use crate::{Grouping, LogFilter, LogRangeExpr, LogRangeOp};

use super::parser::Cursor;
use crate::{Diagnostic, DiagnosticCode, TranslateContext, TranslateResult, TranslatedQuery};

pub fn translate_logql(source: &str, _context: &TranslateContext) -> TranslateResult {
    let mut cursor = Cursor::new(source);
    cursor.skip_ws();
    // A bare log query starts with a stream selector `{` or — in the imbh dialect — a leading line
    // filter (`|`/`!`); it filters and returns log lines. Anything else is a range-aggregation metric
    // expression (`rate` / `count_over_time` / `sum by`).
    let translated = if cursor.remaining().starts_with(['{', '|', '!']) {
        TranslatedQuery::log_selector(parse_log_range(&mut cursor)?)
    } else {
        TranslatedQuery::log(parse_expression(&mut cursor)?)
    };
    cursor.finish()?;
    Ok(translated)
}

fn parse_expression(cursor: &mut Cursor<'_>) -> Result<LogRangeExpr, Diagnostic> {
    let (function, range) = cursor.identifier()?;
    let grouping = if function == "sum" {
        let grouping = parse_sum_grouping(cursor)?;
        cursor.expect("(")?;
        Some(grouping)
    } else {
        None
    };
    let function = if grouping.is_some() {
        cursor.identifier()?.0
    } else {
        function
    };
    let operation = match function.as_str() {
        "count_over_time" => LogRangeOp::CountOverTime,
        "rate" => LogRangeOp::Rate,
        _ => {
            return Err(Diagnostic::new(
                DiagnosticCode::Unsupported,
                range.start,
                range.end,
                "L1 supports count_over_time, rate, and sum by around them",
            ));
        }
    };
    cursor.expect("(")?;
    let filter = parse_log_range(cursor)?;
    cursor.expect("[")?;
    let window_ns = cursor.duration_ns()?;
    cursor.expect("]")?;
    let offset_ns = if cursor.consume("offset") {
        cursor.duration_ns()?
    } else {
        0
    };
    cursor.expect(")")?;
    if grouping.is_some() {
        cursor.expect(")")?;
    }
    Ok(LogRangeExpr {
        filter,
        window_ns,
        offset_ns,
        op: operation,
        grouping,
    })
}

fn parse_sum_grouping(cursor: &mut Cursor<'_>) -> Result<Grouping, Diagnostic> {
    let (keyword, range) = cursor.identifier()?;
    if keyword != "by" {
        return Err(Diagnostic::new(
            DiagnosticCode::Unsupported,
            range.start,
            range.end,
            "L1 supports sum by(...), not other vector aggregation groupings",
        ));
    }
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
    Ok(Grouping::By(labels))
}

fn parse_log_range(cursor: &mut Cursor<'_>) -> Result<LogFilter, Diagnostic> {
    let mut filters = Vec::new();
    // Loki requires a `{...}` stream selector; the imbh dialect also allows a bare line-filter search
    // (`|? "timeout"`), in which case the selector is implicitly empty (matches every stream).
    cursor.skip_ws();
    if cursor.remaining().starts_with('{') {
        cursor.expect("{")?;
        if !cursor.consume("}") {
            loop {
                let (name, _) = cursor.identifier()?;
                let filter = if cursor.consume("=~") {
                    LogFilter::LabelRegex(name, cursor.quoted_string()?)
                } else if cursor.consume("!~") {
                    LogFilter::LabelNotRegex(name, cursor.quoted_string()?)
                } else if cursor.consume("!=") {
                    LogFilter::LabelNe(name, cursor.quoted_string()?)
                } else if cursor.consume("=") {
                    LogFilter::LabelEq(name, cursor.quoted_string()?)
                } else {
                    return Err(cursor.error(
                        DiagnosticCode::Syntax,
                        "expected LogQL stream label matcher",
                    ));
                };
                filters.push(filter);
                if cursor.consume("}") {
                    break;
                }
                cursor.expect(",")?;
            }
        }
    }

    loop {
        // `|?` / `!?` are the imbh dialect's Tantivy-accelerated term operators; `|=`/`!=` (substring)
        // and `|~`/`!~` (regex) are standard LogQL. All are distinct two-character tokens.
        let filter = if cursor.consume("|=") {
            Some(LogFilter::LineContains(cursor.quoted_string()?))
        } else if cursor.consume("!=") {
            Some(LogFilter::LineNotContains(cursor.quoted_string()?))
        } else if cursor.consume("|~") {
            Some(LogFilter::LineRegex(cursor.quoted_string()?))
        } else if cursor.consume("!~") {
            Some(LogFilter::LineNotRegex(cursor.quoted_string()?))
        } else if cursor.consume("|?") {
            Some(LogFilter::LineMatches(cursor.quoted_string()?))
        } else if cursor.consume("!?") {
            Some(LogFilter::LineNotMatches(cursor.quoted_string()?))
        } else {
            None
        };
        let Some(filter) = filter else {
            break;
        };
        filters.push(filter);
    }

    Ok(match filters.len() {
        0 => LogFilter::All,
        1 => filters.pop().expect("one filter"),
        _ => LogFilter::And(filters),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ImbhQueryModel;

    #[test]
    fn translates_grouped_log_rate_with_ordered_filters_and_offset() {
        let translated = translate_logql(
            r#"sum by (app) (rate({app=~"api.*"} |= "error" != "ignore" [5m] offset 1m))"#,
            &TranslateContext::default(),
        )
        .unwrap();
        assert_eq!(translated.profile.capability_id, "imbh.logql.l1.v1");
        let ImbhQueryModel::Log(expression) = translated.model else {
            panic!("unexpected model")
        };
        assert_eq!(expression.window_ns, 300_000_000_000);
        assert_eq!(expression.offset_ns, 60_000_000_000);
        assert!(matches!(expression.filter, LogFilter::And(filters) if filters.len() == 3));
    }

    #[test]
    fn rejects_parser_stages_in_l1() {
        let error = translate_logql(
            r#"count_over_time({app="api"} | json [5m])"#,
            &TranslateContext::default(),
        )
        .unwrap_err();
        assert!(matches!(
            error.code,
            DiagnosticCode::Syntax | DiagnosticCode::Unsupported
        ));
    }

    #[test]
    fn translates_a_bare_selector_with_mixed_line_filters() {
        let translated = translate_logql(
            r#"{service="api"} |= "boot" |? "timeout" != "debug" !? "trace""#,
            &TranslateContext::default(),
        )
        .unwrap();
        let ImbhQueryModel::LogSelector(LogFilter::And(filters)) = translated.model else {
            panic!("expected a bare log selector with an AND of filters")
        };
        assert_eq!(
            filters,
            vec![
                LogFilter::LabelEq("service".to_owned(), "api".to_owned()),
                LogFilter::LineContains("boot".to_owned()),
                LogFilter::LineMatches("timeout".to_owned()),
                LogFilter::LineNotContains("debug".to_owned()),
                LogFilter::LineNotMatches("trace".to_owned()),
            ]
        );
    }

    #[test]
    fn translates_a_bare_line_filter_without_a_stream_selector() {
        // imbh dialect: a leading `|?` search with no `{}` implies an empty (match-all) selector.
        let translated = translate_logql(r#"|? "timeout""#, &TranslateContext::default()).unwrap();
        assert!(matches!(
            translated.model,
            ImbhQueryModel::LogSelector(LogFilter::LineMatches(term)) if term == "timeout"
        ));
    }

    #[test]
    fn empty_selector_is_an_all_match_log_query() {
        let translated = translate_logql("{}", &TranslateContext::default()).unwrap();
        assert!(matches!(
            translated.model,
            ImbhQueryModel::LogSelector(LogFilter::All)
        ));
    }

    #[test]
    fn a_range_aggregation_still_parses_as_a_metric_model() {
        let translated =
            translate_logql(r#"rate({app="api"}[5m])"#, &TranslateContext::default()).unwrap();
        assert!(matches!(translated.model, ImbhQueryModel::Log(_)));
    }
}

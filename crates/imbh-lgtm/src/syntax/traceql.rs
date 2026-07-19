use crate::{
    AttributeScope, Intrinsic, SemanticValue, SpanPredicate, SpansetExpr, StructuralOp,
    TraceCompareOp,
};

use super::parser::Cursor;
use crate::{Diagnostic, DiagnosticCode, TranslateContext, TranslateResult, TranslatedQuery};

pub fn translate_traceql(source: &str, _context: &TranslateContext) -> TranslateResult {
    let mut cursor = Cursor::new(source);
    let mut expression = parse_spanset(&mut cursor, 0)?;
    if cursor.consume("|") {
        let (aggregate, range) = cursor.identifier()?;
        if aggregate != "count" {
            return Err(Diagnostic::new(
                DiagnosticCode::Unsupported,
                range.start,
                range.end,
                "T1 supports only count() spanset pipeline filtering",
            ));
        }
        cursor.expect("(")?;
        cursor.expect(")")?;
        let operation = parse_compare_operator(&mut cursor)?;
        if matches!(operation, TraceCompareOp::Regex | TraceCompareOp::NotRegex) {
            return Err(cursor.error(
                DiagnosticCode::SemanticMismatch,
                "count() cannot be compared with a regex",
            ));
        }
        let value = usize::try_from(cursor.unsigned()?).map_err(|_| {
            cursor.error(
                DiagnosticCode::LimitExceeded,
                "count() threshold does not fit usize",
            )
        })?;
        expression = SpansetExpr::Count {
            input: Box::new(expression),
            op: operation,
            value,
        };
    }
    cursor.finish()?;
    Ok(TranslatedQuery::trace(expression))
}

/// Parse a spanset expression with correct TraceQL operator precedence. From lowest to highest
/// binding: `||`, then `&&`, then the structural operators (`>>`, `>`, `~`, and their negations),
/// each left-associative — matching Tempo's grammar. `{a} || {b} && {c}` therefore parses as
/// `{a} || ({b} && {c})`, and `{a} && {b} >> {c}` as `{a} && ({b} >> {c})`. `depth` bounds only the
/// parenthesis-nesting recursion (each `(` re-enters through `parse_primary`); the fixed
/// or → and → structural precedence chain adds a constant, bounded number of frames per level.
fn parse_spanset(cursor: &mut Cursor<'_>, depth: usize) -> Result<SpansetExpr, Diagnostic> {
    if depth >= 128 {
        return Err(cursor.error(
            DiagnosticCode::LimitExceeded,
            "TraceQL expression nesting exceeds 128",
        ));
    }
    parse_or(cursor, depth)
}

fn parse_or(cursor: &mut Cursor<'_>, depth: usize) -> Result<SpansetExpr, Diagnostic> {
    let mut left = parse_and(cursor, depth)?;
    while cursor.consume("||") {
        let right = parse_and(cursor, depth)?;
        left = SpansetExpr::Or(Box::new(left), Box::new(right));
    }
    Ok(left)
}

fn parse_and(cursor: &mut Cursor<'_>, depth: usize) -> Result<SpansetExpr, Diagnostic> {
    let mut left = parse_structural(cursor, depth)?;
    while cursor.consume("&&") {
        let right = parse_structural(cursor, depth)?;
        left = SpansetExpr::And(Box::new(left), Box::new(right));
    }
    Ok(left)
}

fn parse_structural(cursor: &mut Cursor<'_>, depth: usize) -> Result<SpansetExpr, Diagnostic> {
    let mut left = parse_primary(cursor, depth)?;
    while let Some((operation, union)) = parse_structural_operator(cursor) {
        let right = parse_primary(cursor, depth)?;
        left = SpansetExpr::Structural {
            left: Box::new(left),
            op: operation,
            right: Box::new(right),
            union,
        };
    }
    Ok(left)
}

fn parse_primary(cursor: &mut Cursor<'_>, depth: usize) -> Result<SpansetExpr, Diagnostic> {
    if cursor.consume("(") {
        let expression = parse_spanset(cursor, depth + 1)?;
        cursor.expect(")")?;
        Ok(expression)
    } else {
        parse_selector(cursor)
    }
}

/// Consume a single structural operator token (never `&&`/`||`, which bind at their own precedence
/// levels — none of the structural tokens collide with them: `&&`'s second byte is `&`, which no
/// structural token expects there). Longer tokens are tried first so `&>>` is not mis-read as `&>`.
fn parse_structural_operator(cursor: &mut Cursor<'_>) -> Option<(StructuralOp, bool)> {
    let structural = [
        ("&!>>", StructuralOp::NotDescendant, true),
        ("&!>", StructuralOp::NotChild, true),
        ("&!~", StructuralOp::NotSibling, true),
        ("&>>", StructuralOp::Descendant, true),
        ("&<<", StructuralOp::Ancestor, true),
        ("&>", StructuralOp::Child, true),
        ("&<", StructuralOp::Parent, true),
        ("&~", StructuralOp::Sibling, true),
        ("!>>", StructuralOp::NotDescendant, false),
        ("!<<", StructuralOp::NotAncestor, false),
        ("!>", StructuralOp::NotChild, false),
        ("!<", StructuralOp::NotParent, false),
        ("!~", StructuralOp::NotSibling, false),
        (">>", StructuralOp::Descendant, false),
        ("<<", StructuralOp::Ancestor, false),
        (">", StructuralOp::Child, false),
        ("<", StructuralOp::Parent, false),
        ("~", StructuralOp::Sibling, false),
    ];
    for (token, operation, union) in structural {
        if cursor.consume(token) {
            return Some((operation, union));
        }
    }
    None
}

fn parse_selector(cursor: &mut Cursor<'_>) -> Result<SpansetExpr, Diagnostic> {
    cursor.expect("{")?;
    if cursor.consume("}") {
        return Ok(SpansetExpr::Select(SpanPredicate::All));
    }
    let mut predicates = Vec::new();
    loop {
        predicates.push(parse_predicate(cursor)?);
        if cursor.consume("}") {
            break;
        }
        if !cursor.consume("&&") {
            return Err(cursor.error(
                DiagnosticCode::Unsupported,
                "T1 selectors support same-span AND only",
            ));
        }
    }
    Ok(SpansetExpr::Select(if predicates.len() == 1 {
        predicates.pop().expect("one predicate")
    } else {
        SpanPredicate::And(predicates)
    }))
}

fn parse_predicate(cursor: &mut Cursor<'_>) -> Result<SpanPredicate, Diagnostic> {
    let (field, range) = cursor.identifier()?;
    let operation = parse_compare_operator(cursor)?;
    let target = predicate_target(&field, range)?;
    let value = parse_value(cursor, target)?;
    Ok(match target {
        PredicateTarget::Intrinsic(intrinsic) => SpanPredicate::Intrinsic {
            intrinsic,
            op: operation,
            value,
        },
        PredicateTarget::Attribute(scope, key) => SpanPredicate::Attribute {
            scope,
            key: key.to_owned(),
            op: operation,
            value,
        },
    })
}

#[derive(Clone, Copy)]
enum PredicateTarget<'a> {
    Intrinsic(Intrinsic),
    Attribute(AttributeScope, &'a str),
}

fn predicate_target(
    field: &str,
    range: crate::SourceRange,
) -> Result<PredicateTarget<'_>, Diagnostic> {
    let intrinsic = match field {
        "name" | "span:name" => Some(Intrinsic::SpanName),
        "duration" | "span:duration" => Some(Intrinsic::SpanDuration),
        "status" | "span:status" => Some(Intrinsic::SpanStatus),
        "statusMessage" | "span:statusMessage" => Some(Intrinsic::SpanStatusMessage),
        "kind" | "span:kind" => Some(Intrinsic::SpanKind),
        "childCount" | "span:childCount" => Some(Intrinsic::SpanChildCount),
        "span:id" => Some(Intrinsic::SpanId),
        "trace:id" => Some(Intrinsic::TraceId),
        "traceDuration" | "trace:duration" => Some(Intrinsic::TraceDuration),
        "rootServiceName" | "trace:rootService" => Some(Intrinsic::TraceRootService),
        "rootName" | "trace:rootName" => Some(Intrinsic::TraceRootName),
        _ => None,
    };
    if let Some(intrinsic) = intrinsic {
        return Ok(PredicateTarget::Intrinsic(intrinsic));
    }

    let (scope, key) = if let Some(key) = field.strip_prefix("span.") {
        (AttributeScope::Span, key)
    } else if let Some(key) = field.strip_prefix("resource.") {
        (AttributeScope::Resource, key)
    } else if let Some(key) = field.strip_prefix("instrumentation.") {
        (AttributeScope::Instrumentation, key)
    } else if let Some(key) = field.strip_prefix("event.") {
        (AttributeScope::Event, key)
    } else if let Some(key) = field.strip_prefix("link.") {
        (AttributeScope::Link, key)
    } else if let Some(key) = field.strip_prefix('.') {
        (AttributeScope::Span, key)
    } else {
        return Err(Diagnostic::new(
            DiagnosticCode::Unsupported,
            range.start,
            range.end,
            "attribute scope must be explicit",
        ));
    };
    if key.is_empty() {
        return Err(Diagnostic::new(
            DiagnosticCode::Syntax,
            range.start,
            range.end,
            "attribute key is empty",
        ));
    }
    Ok(PredicateTarget::Attribute(scope, key))
}

fn parse_compare_operator(cursor: &mut Cursor<'_>) -> Result<TraceCompareOp, Diagnostic> {
    if cursor.consume("=~") {
        Ok(TraceCompareOp::Regex)
    } else if cursor.consume("!~") {
        Ok(TraceCompareOp::NotRegex)
    } else if cursor.consume(">=") {
        Ok(TraceCompareOp::Ge)
    } else if cursor.consume("<=") {
        Ok(TraceCompareOp::Le)
    } else if cursor.consume("!=") {
        Ok(TraceCompareOp::Ne)
    } else if cursor.consume("=") {
        Ok(TraceCompareOp::Eq)
    } else if cursor.consume(">") {
        Ok(TraceCompareOp::Gt)
    } else if cursor.consume("<") {
        Ok(TraceCompareOp::Lt)
    } else {
        Err(cursor.error(
            DiagnosticCode::Syntax,
            "expected TraceQL comparison operator",
        ))
    }
}

fn parse_value(
    cursor: &mut Cursor<'_>,
    target: PredicateTarget<'_>,
) -> Result<SemanticValue<'static>, Diagnostic> {
    if matches!(
        target,
        PredicateTarget::Intrinsic(Intrinsic::SpanDuration | Intrinsic::TraceDuration)
    ) {
        return cursor.duration_ns().map(SemanticValue::DurationNs);
    }
    if matches!(
        target,
        PredicateTarget::Intrinsic(Intrinsic::SpanChildCount)
    ) {
        let value = cursor.unsigned()?;
        return i64::try_from(value)
            .map(SemanticValue::Integer)
            .map_err(|_| cursor.error(DiagnosticCode::LimitExceeded, "integer is too large"));
    }
    cursor.skip_ws();
    if cursor.remaining().starts_with('"') {
        return cursor
            .quoted_string()
            .map(|value| SemanticValue::String(std::borrow::Cow::Owned(value)));
    }
    if cursor
        .remaining()
        .as_bytes()
        .first()
        .is_some_and(|byte| byte.is_ascii_digit() || matches!(byte, b'-' | b'+'))
    {
        let value = cursor.float()?;
        if value.fract() == 0.0 && value >= i64::MIN as f64 && value <= i64::MAX as f64 {
            return Ok(SemanticValue::Integer(value as i64));
        }
        return Ok(SemanticValue::Float(value));
    }
    let (value, _) = cursor.identifier()?;
    Ok(match value.as_str() {
        "nil" => SemanticValue::Nil,
        "true" => SemanticValue::Boolean(true),
        "false" => SemanticValue::Boolean(false),
        _ => SemanticValue::String(std::borrow::Cow::Owned(value)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ImbhQueryModel;

    #[test]
    fn translates_typed_structure_and_count_pipeline() {
        let translated = translate_traceql(
            r#"{ resource.service.name = "api" && duration >= 10ms } >> { status = error } | count() >= 1"#,
            &TranslateContext::default(),
        )
        .unwrap();
        assert_eq!(translated.profile.capability_id, "imbh.traceql.t1.v1");
        let ImbhQueryModel::Trace(SpansetExpr::Count { input, .. }) = translated.model else {
            panic!("unexpected model")
        };
        assert!(matches!(*input, SpansetExpr::Structural { .. }));
    }

    #[test]
    fn rejects_unscoped_attributes() {
        let error = translate_traceql(r#"{ http.route = "/cart" }"#, &TranslateContext::default())
            .unwrap_err();
        assert_eq!(error.code, DiagnosticCode::Unsupported);
    }

    fn spanset(source: &str) -> SpansetExpr {
        let ImbhQueryModel::Trace(expr) = translate_traceql(source, &TranslateContext::default())
            .unwrap()
            .model
        else {
            panic!("expected a trace model")
        };
        expr
    }

    /// `&&` binds tighter than `||`: `{a} || {b} && {c}` is `{a} || ({b} && {c})`, not
    /// `({a} || {b}) && {c}`.
    #[test]
    fn and_binds_tighter_than_or() {
        let SpansetExpr::Or(left, right) = spanset("{ .a = 1 } || { .b = 2 } && { .c = 3 }") else {
            panic!("top operator must be ||")
        };
        assert!(
            matches!(*left, SpansetExpr::Select(_)),
            "lhs is the bare {{a}}"
        );
        assert!(
            matches!(*right, SpansetExpr::And(_, _)),
            "rhs groups {{b}} && {{c}}"
        );
    }

    /// Structural operators bind tighter than `&&`: `{a} && {b} >> {c}` is `{a} && ({b} >> {c})`.
    #[test]
    fn structural_binds_tighter_than_and() {
        let SpansetExpr::And(left, right) = spanset("{ .a = 1 } && { .b = 2 } >> { .c = 3 }")
        else {
            panic!("top operator must be &&")
        };
        assert!(
            matches!(*left, SpansetExpr::Select(_)),
            "lhs is the bare {{a}}"
        );
        assert!(
            matches!(*right, SpansetExpr::Structural { .. }),
            "rhs groups {{b}} >> {{c}}"
        );
    }

    /// Explicit parentheses still override precedence: `({a} || {b}) && {c}`.
    #[test]
    fn parentheses_override_precedence() {
        let SpansetExpr::And(left, _) = spanset("({ .a = 1 } || { .b = 2 }) && { .c = 3 }") else {
            panic!("top operator must be &&")
        };
        assert!(
            matches!(*left, SpansetExpr::Or(_, _)),
            "parenthesized lhs stays an || group"
        );
    }
}

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::pin::Pin;

use imbh_core::{SpanId, TraceId};

use crate::{EvalLimits, SemanticError};

/// A typed attribute / intrinsic value. String and byte payloads *borrow* from the backing store
/// (`Cow::Borrowed`) when they come from a span, and are owned (`Cow::Owned`) when they are query
/// literals (the AST fixes `SemanticValue<'static>`) or computed (e.g. a hex-encoded id).
#[derive(Debug, Clone, PartialEq)]
pub enum SemanticValue<'a> {
    Nil,
    Boolean(bool),
    Integer(i64),
    Float(f64),
    String(Cow<'a, str>),
    DurationNs(u64),
    Bytes(Cow<'a, [u8]>),
    Array(Vec<SemanticValue<'a>>),
}

impl SemanticValue<'_> {
    /// Lift to `SemanticValue<'static>` by decoding any borrowed payload. Used where the source
    /// value is inherently owned (e.g. decoded from a JSON events/links blob), so it cannot borrow
    /// the backing store.
    pub fn into_owned(self) -> SemanticValue<'static> {
        match self {
            SemanticValue::Nil => SemanticValue::Nil,
            SemanticValue::Boolean(value) => SemanticValue::Boolean(value),
            SemanticValue::Integer(value) => SemanticValue::Integer(value),
            SemanticValue::Float(value) => SemanticValue::Float(value),
            SemanticValue::String(value) => SemanticValue::String(Cow::Owned(value.into_owned())),
            SemanticValue::DurationNs(value) => SemanticValue::DurationNs(value),
            SemanticValue::Bytes(value) => SemanticValue::Bytes(Cow::Owned(value.into_owned())),
            SemanticValue::Array(values) => {
                SemanticValue::Array(values.into_iter().map(SemanticValue::into_owned).collect())
            }
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct TypedAttributes<'a>(BTreeMap<Cow<'a, str>, SemanticValue<'a>>);

impl<'a> TypedAttributes<'a> {
    pub fn new(values: impl IntoIterator<Item = (Cow<'a, str>, SemanticValue<'a>)>) -> Self {
        Self(values.into_iter().collect())
    }

    pub fn get(&self, key: &str) -> Option<&SemanticValue<'a>> {
        self.0.get(key)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &SemanticValue<'a>)> {
        self.0.iter().map(|(key, value)| (key.as_ref(), value))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttributeScope {
    Span,
    Resource,
    Instrumentation,
    Event,
    Link,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TraceCompareOp {
    Eq,
    Ne,
    Gt,
    Ge,
    Lt,
    Le,
    Regex,
    NotRegex,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Intrinsic {
    SpanName,
    SpanDuration,
    SpanStatus,
    SpanStatusMessage,
    SpanKind,
    SpanChildCount,
    SpanId,
    TraceId,
    TraceDuration,
    TraceRootService,
    TraceRootName,
}
#[derive(Debug, Clone, PartialEq)]
pub struct SemanticSpan<'a> {
    /// Raw ids, kept un-encoded through evaluation (they are `Copy` map keys); hex is deferred to
    /// the output boundary ([`TraceSpanset`]), so only *selected* spans pay the encoding.
    pub span_id: SpanId,
    pub parent_span_id: Option<SpanId>,
    pub name: &'a str,
    pub status: &'a str,
    pub status_message: Option<&'a str>,
    pub kind: &'a str,
    pub service: Option<&'a str>,
    pub start_time_ns: i64,
    pub duration_ns: u64,
    pub attributes: TypedAttributes<'a>,
    pub resource: TypedAttributes<'a>,
    pub instrumentation: TypedAttributes<'a>,
    pub events: Vec<TypedAttributes<'a>>,
    pub links: Vec<TypedAttributes<'a>>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SpanPredicate {
    All,
    And(Vec<SpanPredicate>),
    NameEq(String),
    StatusEq(String),
    KindEq(String),
    Duration {
        op: TraceCompareOp,
        value_ns: u64,
    },
    SpanAttrEq(String, String),
    Intrinsic {
        intrinsic: Intrinsic,
        op: TraceCompareOp,
        value: SemanticValue<'static>,
    },
    ResourceAttrEq(String, String),
    Attribute {
        scope: AttributeScope,
        key: String,
        op: TraceCompareOp,
        value: SemanticValue<'static>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StructuralOp {
    Child,
    Parent,
    Descendant,
    Ancestor,
    Sibling,
    NotChild,
    NotParent,
    NotDescendant,
    NotAncestor,
    NotSibling,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SpansetExpr {
    Select(SpanPredicate),
    And(Box<Self>, Box<Self>),
    Or(Box<Self>, Box<Self>),
    Structural {
        left: Box<Self>,
        op: StructuralOp,
        right: Box<Self>,
        union: bool,
    },
    CountAtLeast(Box<Self>, usize),
    Count {
        input: Box<Self>,
        op: TraceCompareOp,
        value: usize,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceSpanset {
    pub selected_span_ids: Vec<String>,
}

/// One complete trace supplied to the semantic evaluator.
#[derive(Debug, Clone, PartialEq)]
pub struct SemanticTrace<'a> {
    pub trace_id: TraceId,
    pub spans: Vec<SemanticSpan<'a>>,
    pub root_service: Option<&'a str>,
    pub root_name: Option<&'a str>,
    pub start_time_ns: i64,
    pub duration_ns: u64,
}

/// A span-predicate leaf that can be pushed to the candidate `search()` as a **necessary** condition
/// on any matching trace. Only positive, single-span-expressible predicates that the storage layer can
/// filter on are represented; anything else is dropped (the Rust evaluator stays authoritative).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpanCandidateFilter {
    Name(String),
    Status(String),
    Kind(String),
    DurationGe(u64),
    DurationLe(u64),
    AttrEq(String, String),
    /// A numeric span-attribute comparison (`.key >= n`), pushed to `attr_ge`/`gt`/`le`/`lt`. The
    /// bound is an `i64` (only integer-valued literals within f64's exact range are lifted — see
    /// [`push_predicate`]) so the whole enum stays `Eq`-comparable; `build_trace_query` widens it to
    /// the `f64` the storage matcher takes. The storage side reads the attribute through `json_get_num`
    /// so it sees integer/double-typed JSON attributes, keeping this a sound necessary condition.
    AttrNumGt(String, i64),
    AttrNumGe(String, i64),
    AttrNumLt(String, i64),
    AttrNumLe(String, i64),
}

/// Extract a *necessary* single-span candidate filter from a spanset expression — a set of predicates
/// that every matching trace must contain **one span** satisfying — so the candidate query can skip
/// traces that cannot possibly match. Sound by construction: it only descends into branches that are
/// necessary for the whole expression, and only lifts positive, pushable predicate leaves. Returns
/// empty (⇒ no pushdown, evaluate every candidate in range) whenever necessity can't be proven:
/// `Or` (neither side necessary), `count(..) <= n` / `== 0` (a zero-match trace can satisfy it),
/// `Ne`/`Regex`/non-`Span`-scope predicates, and computed intrinsics. Positive integer-valued span
/// attribute comparisons (`.key <op> n`) are lifted to numeric candidate filters; float/duration-typed
/// numeric literals stay with the evaluator.
pub fn candidate_filters(expr: &SpansetExpr) -> Vec<SpanCandidateFilter> {
    match expr {
        SpansetExpr::Select(predicate) => {
            let mut out = Vec::new();
            push_predicate(predicate, &mut out);
            out
        }
        // Both operands are necessary; a single-span filter from either is sound (a subset of a
        // necessary condition is still necessary). Prefer whichever actually yields a filter.
        SpansetExpr::And(left, right) | SpansetExpr::Structural { left, right, .. } => {
            let filters = candidate_filters(left);
            if filters.is_empty() {
                candidate_filters(right)
            } else {
                filters
            }
        }
        SpansetExpr::CountAtLeast(inner, count) if *count >= 1 => candidate_filters(inner),
        SpansetExpr::Count { input, op, value }
            if *value >= 1
                && matches!(
                    op,
                    TraceCompareOp::Ge | TraceCompareOp::Gt | TraceCompareOp::Eq
                ) =>
        {
            candidate_filters(input)
        }
        // Or / count<= / count==0 / CountAtLeast(_,0): no necessary single-span condition.
        _ => Vec::new(),
    }
}

fn push_predicate(predicate: &SpanPredicate, out: &mut Vec<SpanCandidateFilter>) {
    match predicate {
        SpanPredicate::All => {}
        // Predicate-level `And` is a single-span conjunction; any pushable subset is necessary.
        SpanPredicate::And(preds) => preds.iter().for_each(|p| push_predicate(p, out)),
        SpanPredicate::NameEq(name) => out.push(SpanCandidateFilter::Name(name.clone())),
        SpanPredicate::StatusEq(status) => out.push(SpanCandidateFilter::Status(status.clone())),
        SpanPredicate::KindEq(kind) => out.push(SpanCandidateFilter::Kind(kind.clone())),
        SpanPredicate::Duration { op, value_ns } => match op {
            // `>=`/`<=` are weaker-but-sound supersets of the strict `>`/`<`.
            TraceCompareOp::Gt | TraceCompareOp::Ge => {
                out.push(SpanCandidateFilter::DurationGe(*value_ns))
            }
            TraceCompareOp::Lt | TraceCompareOp::Le => {
                out.push(SpanCandidateFilter::DurationLe(*value_ns))
            }
            TraceCompareOp::Eq => {
                out.push(SpanCandidateFilter::DurationGe(*value_ns));
                out.push(SpanCandidateFilter::DurationLe(*value_ns));
            }
            _ => {}
        },
        SpanPredicate::SpanAttrEq(key, value) => {
            out.push(SpanCandidateFilter::AttrEq(key.clone(), value.clone()))
        }
        SpanPredicate::Attribute {
            scope: AttributeScope::Span,
            key,
            op: TraceCompareOp::Eq,
            value: SemanticValue::String(value),
        } => out.push(SpanCandidateFilter::AttrEq(key.clone(), value.to_string())),
        SpanPredicate::Attribute {
            scope: AttributeScope::Span,
            key,
            op,
            value: SemanticValue::Integer(value),
        } => push_numeric_attr(key, op, *value, out),
        // Everything else (Ne/Regex, non-Span scope, float/duration numeric compares, `attr`
        // existence, computed intrinsics, resource attributes) is left to the evaluator.
        _ => {}
    }
}

/// Lift an integer-valued single-span numeric comparison (`.key <op> value`) into a candidate filter.
///
/// Only lifted when `value` is within f64's exactly-representable integer range (`|value| < 2^53`), so
/// the `i64 -> f64` widening in `build_trace_query` is lossless and the pushed bound is *exact* — no
/// rounding could turn the necessary condition into a stricter one that drops a real match. Outside
/// that range (astronomically unlikely for a span attribute) nothing is pushed and the evaluator
/// stays authoritative. `Eq` becomes the closed range `>= value AND <= value`; `Ne`/`Regex` are not
/// single-span-pushable and fall through.
fn push_numeric_attr(
    key: &str,
    op: &TraceCompareOp,
    value: i64,
    out: &mut Vec<SpanCandidateFilter>,
) {
    const F64_EXACT_INT: i64 = 1 << 53;
    if value.unsigned_abs() >= F64_EXACT_INT as u64 {
        return;
    }
    match op {
        TraceCompareOp::Gt => out.push(SpanCandidateFilter::AttrNumGt(key.to_owned(), value)),
        TraceCompareOp::Ge => out.push(SpanCandidateFilter::AttrNumGe(key.to_owned(), value)),
        TraceCompareOp::Lt => out.push(SpanCandidateFilter::AttrNumLt(key.to_owned(), value)),
        TraceCompareOp::Le => out.push(SpanCandidateFilter::AttrNumLe(key.to_owned(), value)),
        TraceCompareOp::Eq => {
            out.push(SpanCandidateFilter::AttrNumGe(key.to_owned(), value));
            out.push(SpanCandidateFilter::AttrNumLe(key.to_owned(), value));
        }
        TraceCompareOp::Ne | TraceCompareOp::Regex | TraceCompareOp::NotRegex => {}
    }
}

/// Bounded two-phase TraceQL storage request.
///
/// Implementations first select no more than `max_traces` candidate IDs in `bounds` (optionally
/// narrowed by `candidate` — a sound necessary filter, safe to ignore), then return every span of
/// those traces, stopping when `max_spans` would be exceeded. Complete traces are required for
/// structural operators.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceFetchRequest {
    pub bounds: crate::FetchBounds,
    pub max_traces: usize,
    pub max_spans: usize,
    /// A *necessary* candidate filter derived from the query (see [`candidate_filters`]). Narrows the
    /// candidate scan; ignoring it stays correct (the evaluator re-checks every trace).
    pub candidate: Vec<SpanCandidateFilter>,
}

type SemanticTraceDependent<'a> = SemanticTrace<'a>;

self_cell::self_cell! {
    /// **One** complete trace whose span strings/attributes *borrow* from an owned, type-erased
    /// backing store (the source's materialized `imbh::Trace`). Held only while that single trace is
    /// evaluated, then dropped — TraceQL is per-trace independent, so the evaluator streams one trace
    /// at a time (peak memory = one trace, not the whole result set). The owned output
    /// ([`TraceQueryMatch`], hex ids) is what escapes.
    pub struct TracePack {
        owner: Box<dyn std::any::Any + Send>,
        #[covariant]
        dependent: SemanticTraceDependent,
    }
}

/// A bounded, **streaming** source of complete traces: `fetch_candidates` selects trace ids in
/// storage (cheap; ids only), then the evaluator pulls each trace on demand via `fetch_trace` and
/// drops it before the next. Each trace is returned inside a self-owning [`TracePack`] so its span
/// strings/attributes can borrow the source's backing store for that trace's evaluation.
pub trait TraceSource {
    fn fetch_candidates<'a>(
        &'a self,
        request: &'a TraceFetchRequest,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<TraceId>, SemanticError>> + Send + 'a>>;

    fn fetch_trace<'a>(
        &'a self,
        trace_id: TraceId,
    ) -> Pin<Box<dyn Future<Output = Result<Option<TracePack>, SemanticError>> + Send + 'a>>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceQueryMatch {
    pub trace_id: String,
    /// The trace's earliest span start, in nanoseconds since the Unix epoch (from
    /// [`SemanticTrace::start_time_ns`]) — carried so a caller can show *when* a matched trace
    /// happened without a second fetch.
    pub start_time_ns: i64,
    pub spanset: TraceSpanset,
}

pub fn plan_trace_fetch(
    bounds: crate::FetchBounds,
    limits: EvalLimits,
    expr: &SpansetExpr,
) -> Result<TraceFetchRequest, SemanticError> {
    Ok(TraceFetchRequest {
        bounds: crate::FetchBounds::new(bounds.start_ns, bounds.end_ns)?,
        max_traces: limits.max_traces,
        max_spans: limits.max_spans,
        candidate: candidate_filters(expr),
    })
}

pub async fn execute_traceql<S: TraceSource + ?Sized>(
    source: &S,
    expr: &SpansetExpr,
    bounds: crate::FetchBounds,
    limits: EvalLimits,
) -> Result<Vec<TraceQueryMatch>, SemanticError> {
    let request = plan_trace_fetch(bounds, limits, expr)?;
    // Stream one trace at a time: candidate ids are selected in storage, then each trace is pulled,
    // evaluated in isolation (TraceQL is per-trace independent), and dropped before the next — so
    // peak memory is one trace, not the whole candidate set. Owned output (hex ids) is what escapes.
    let candidates = source.fetch_candidates(&request).await?;
    if candidates.len() > request.max_traces {
        return Err(SemanticError::LimitExceeded("TraceQL source traces"));
    }
    let mut seen = BTreeSet::new();
    let mut span_count = 0usize;
    let mut result = Vec::new();
    for trace_id in candidates {
        if !seen.insert(trace_id) {
            return Err(SemanticError::Malformed(
                "TraceQL source returned a duplicate trace",
            ));
        }
        let Some(pack) = source.fetch_trace(trace_id).await? else {
            continue;
        };
        let trace = pack.borrow_dependent();
        span_count = span_count
            .checked_add(trace.spans.len())
            .ok_or(SemanticError::LimitExceeded("TraceQL source spans"))?;
        if span_count > request.max_spans {
            return Err(SemanticError::LimitExceeded("TraceQL source spans"));
        }
        if let Some(spanset) = eval_trace_reference(trace, expr, limits)? {
            result.push(TraceQueryMatch {
                trace_id: trace_id.to_hex(),
                start_time_ns: trace.start_time_ns,
                spanset,
            });
        }
    }
    Ok(result)
}

pub fn eval_trace_reference(
    trace: &SemanticTrace<'_>,
    expr: &SpansetExpr,
    limits: EvalLimits,
) -> Result<Option<TraceSpanset>, SemanticError> {
    if trace.spans.len() > limits.max_spans {
        return Err(SemanticError::LimitExceeded("spans"));
    }
    let mut ids = BTreeSet::new();
    for span in &trace.spans {
        if !ids.insert(span.span_id) {
            return Err(SemanticError::Malformed("duplicate span id"));
        }
    }
    let selected = eval(&trace.spans, Some(trace), expr, 0, limits)?;
    if selected.is_empty() {
        Ok(None)
    } else {
        Ok(Some(TraceSpanset {
            selected_span_ids: selected.into_iter().map(|id| id.to_hex()).collect(),
        }))
    }
}
pub fn eval_spanset_reference(
    spans: &[SemanticSpan<'_>],
    expr: &SpansetExpr,
    limits: EvalLimits,
) -> Result<Option<TraceSpanset>, SemanticError> {
    if spans.len() > limits.max_spans {
        return Err(SemanticError::LimitExceeded("spans"));
    }
    let mut ids = BTreeSet::new();
    for span in spans {
        if !ids.insert(span.span_id) {
            return Err(SemanticError::Malformed("duplicate span id"));
        }
    }
    let selected = eval(spans, None, expr, 0, limits)?;
    if selected.is_empty() {
        Ok(None)
    } else {
        Ok(Some(TraceSpanset {
            selected_span_ids: selected.into_iter().map(|id| id.to_hex()).collect(),
        }))
    }
}

fn eval<'a>(
    spans: &[SemanticSpan<'a>],
    trace: Option<&SemanticTrace<'a>>,
    expr: &SpansetExpr,
    depth: usize,
    limits: EvalLimits,
) -> Result<BTreeSet<SpanId>, SemanticError> {
    if depth >= limits.max_recursion {
        return Err(SemanticError::LimitExceeded("trace expression recursion"));
    }
    Ok(match expr {
        SpansetExpr::Select(predicate) => {
            let mut selected = BTreeSet::new();
            for span in spans {
                if predicate_matches(span, trace, spans, predicate)? {
                    selected.insert(span.span_id);
                }
            }
            selected
        }
        SpansetExpr::And(left, right) => {
            let left = eval(spans, trace, left, depth + 1, limits)?;
            let right = eval(spans, trace, right, depth + 1, limits)?;
            if left.is_empty() || right.is_empty() {
                BTreeSet::new()
            } else {
                left.union(&right).copied().collect()
            }
        }
        SpansetExpr::Or(left, right) => {
            let left = eval(spans, trace, left, depth + 1, limits)?;
            let right = eval(spans, trace, right, depth + 1, limits)?;
            left.union(&right).copied().collect()
        }
        SpansetExpr::CountAtLeast(inner, count) => {
            let selected = eval(spans, trace, inner, depth + 1, limits)?;
            if selected.len() >= *count {
                selected
            } else {
                BTreeSet::new()
            }
        }
        SpansetExpr::Count { input, op, value } => {
            if matches!(op, TraceCompareOp::Regex | TraceCompareOp::NotRegex) {
                return Err(SemanticError::Malformed(
                    "TraceQL count does not support regex comparison",
                ));
            }
            let selected = eval(spans, trace, input, depth + 1, limits)?;
            let actual = SemanticValue::Integer(
                i64::try_from(selected.len())
                    .map_err(|_| SemanticError::LimitExceeded("TraceQL spanset count"))?,
            );
            let expected = SemanticValue::Integer(
                i64::try_from(*value)
                    .map_err(|_| SemanticError::LimitExceeded("TraceQL spanset count"))?,
            );
            if compare_value(Some(&actual), *op, &expected)? {
                selected
            } else {
                BTreeSet::new()
            }
        }
        SpansetExpr::Structural {
            left,
            op,
            right,
            union,
        } => {
            let left = eval(spans, trace, left, depth + 1, limits)?;
            let right = eval(spans, trace, right, depth + 1, limits)?;
            let matched = structural(spans, &left, &right, *op)?;
            if *union {
                left.union(&matched).copied().collect()
            } else {
                matched
            }
        }
    })
}

fn predicate_matches<'a>(
    span: &SemanticSpan<'a>,
    trace: Option<&SemanticTrace<'a>>,
    spans: &[SemanticSpan<'a>],
    predicate: &SpanPredicate,
) -> Result<bool, SemanticError> {
    Ok(match predicate {
        SpanPredicate::All => true,
        SpanPredicate::And(predicates) => {
            for predicate in predicates {
                if !predicate_matches(span, trace, spans, predicate)? {
                    return Ok(false);
                }
            }
            true
        }
        SpanPredicate::NameEq(value) => span.name == value.as_str(),
        SpanPredicate::StatusEq(value) => span.status == value.as_str(),
        SpanPredicate::KindEq(value) => span.kind == value.as_str(),
        SpanPredicate::Duration { op, value_ns } => compare_value(
            Some(&SemanticValue::DurationNs(span.duration_ns)),
            *op,
            &SemanticValue::DurationNs(*value_ns),
        )?,
        SpanPredicate::Intrinsic {
            intrinsic,
            op,
            value,
        } => {
            let actual = intrinsic_value(*intrinsic, span, trace, spans)?;
            compare_value(actual.as_ref(), *op, value)?
        }
        SpanPredicate::SpanAttrEq(key, value) => compare_value(
            span.attributes.get(key),
            TraceCompareOp::Eq,
            &SemanticValue::String(Cow::Owned(value.clone())),
        )?,
        SpanPredicate::ResourceAttrEq(key, value) => compare_value(
            span.resource.get(key),
            TraceCompareOp::Eq,
            &SemanticValue::String(Cow::Owned(value.clone())),
        )?,
        SpanPredicate::Attribute {
            scope,
            key,
            op,
            value,
        } => match scope {
            AttributeScope::Span => compare_value(span.attributes.get(key), *op, value)?,
            AttributeScope::Resource => compare_value(span.resource.get(key), *op, value)?,
            AttributeScope::Instrumentation => {
                compare_value(span.instrumentation.get(key), *op, value)?
            }
            AttributeScope::Event => span.events.iter().try_fold(false, |matched, event| {
                Ok(matched || compare_value(event.get(key), *op, value)?)
            })?,
            AttributeScope::Link => span.links.iter().try_fold(false, |matched, link| {
                Ok(matched || compare_value(link.get(key), *op, value)?)
            })?,
        },
    })
}

fn intrinsic_value<'a>(
    intrinsic: Intrinsic,
    span: &SemanticSpan<'a>,
    trace: Option<&SemanticTrace<'a>>,
    spans: &[SemanticSpan<'a>],
) -> Result<Option<SemanticValue<'a>>, SemanticError> {
    let value = match intrinsic {
        Intrinsic::SpanName => Some(SemanticValue::String(Cow::Borrowed(span.name))),
        Intrinsic::SpanDuration => Some(SemanticValue::DurationNs(span.duration_ns)),
        Intrinsic::SpanStatus => Some(SemanticValue::String(Cow::Borrowed(span.status))),
        Intrinsic::SpanStatusMessage => span
            .status_message
            .map(|value| SemanticValue::String(Cow::Borrowed(value))),
        Intrinsic::SpanKind => Some(SemanticValue::String(Cow::Borrowed(span.kind))),
        Intrinsic::SpanChildCount => {
            let count = spans
                .iter()
                .filter(|candidate| candidate.parent_span_id == Some(span.span_id))
                .count();
            Some(SemanticValue::Integer(i64::try_from(count).map_err(
                |_| SemanticError::LimitExceeded("TraceQL child count"),
            )?))
        }
        // Ids are the one intrinsic that must materialize: hex-encode on demand (rare path).
        Intrinsic::SpanId => Some(SemanticValue::String(Cow::Owned(span.span_id.to_hex()))),
        Intrinsic::TraceId => Some(SemanticValue::String(Cow::Owned(
            trace
                .ok_or(SemanticError::Incompatible(
                    "trace intrinsic requires complete-trace evaluation",
                ))?
                .trace_id
                .to_hex(),
        ))),
        Intrinsic::TraceDuration => Some(SemanticValue::DurationNs(
            trace
                .ok_or(SemanticError::Incompatible(
                    "trace intrinsic requires complete-trace evaluation",
                ))?
                .duration_ns,
        )),
        Intrinsic::TraceRootService => trace
            .ok_or(SemanticError::Incompatible(
                "trace intrinsic requires complete-trace evaluation",
            ))?
            .root_service
            .map(|value| SemanticValue::String(Cow::Borrowed(value))),
        Intrinsic::TraceRootName => trace
            .ok_or(SemanticError::Incompatible(
                "trace intrinsic requires complete-trace evaluation",
            ))?
            .root_name
            .map(|value| SemanticValue::String(Cow::Borrowed(value))),
    };
    Ok(value)
}
fn compare_value<'a>(
    actual: Option<&SemanticValue<'a>>,
    op: TraceCompareOp,
    expected: &SemanticValue<'a>,
) -> Result<bool, SemanticError> {
    if let Some(SemanticValue::Array(values)) = actual {
        let (element_op, negate) = match op {
            TraceCompareOp::Ne => (TraceCompareOp::Eq, true),
            TraceCompareOp::NotRegex => (TraceCompareOp::Regex, true),
            _ => (op, false),
        };
        let mut any = false;
        for value in values {
            if compare_value(Some(value), element_op, expected)? {
                any = true;
                break;
            }
        }
        return Ok(if negate { !any } else { any });
    }

    // Tempo missing-attribute semantics: a span that does not contain the referenced attribute is
    // NOT matched by the condition, regardless of operator. Unlike SQL/PromQL three-valued NULL
    // logic, a missing attribute makes even negated matchers (`!=`, `!~`) evaluate to `false`
    // (not-matched), staying consistent with the positive operators (`=`, `=~`, `<`, ...) which
    // already fail on a missing attribute. The sole exception is a comparison against the `nil`
    // literal, which explicitly tests presence/absence: `{ .foo = nil }` matches a missing
    // attribute (handled by the `equal` computation below) and `{ .foo != nil }` does not.
    if actual.is_none() && !matches!(expected, SemanticValue::Nil) {
        return Ok(false);
    }

    if matches!(op, TraceCompareOp::Regex | TraceCompareOp::NotRegex) {
        let (Some(SemanticValue::String(actual)), SemanticValue::String(pattern)) =
            (actual, expected)
        else {
            return Ok(false);
        };
        let pattern = format!("^(?:{pattern})$");
        let matched = regex::Regex::new(&pattern)
            .map_err(|_| SemanticError::Malformed("invalid TraceQL regex"))?
            .is_match(actual);
        return Ok(if op == TraceCompareOp::Regex {
            matched
        } else {
            !matched
        });
    }

    let equal = match (actual, expected) {
        (None | Some(SemanticValue::Nil), SemanticValue::Nil) => true,
        (Some(SemanticValue::Integer(left)), SemanticValue::Float(right)) => {
            (*left as f64) == *right
        }
        (Some(SemanticValue::Float(left)), SemanticValue::Integer(right)) => {
            *left == (*right as f64)
        }
        (Some(actual), expected) => actual == expected,
        (None, _) => false,
    };
    match op {
        TraceCompareOp::Eq => Ok(equal),
        TraceCompareOp::Ne => Ok(!equal),
        TraceCompareOp::Gt | TraceCompareOp::Ge | TraceCompareOp::Lt | TraceCompareOp::Le => {
            let ordering = semantic_partial_cmp(actual, expected);
            Ok(matches!(
                (op, ordering),
                (TraceCompareOp::Gt, Some(std::cmp::Ordering::Greater))
                    | (
                        TraceCompareOp::Ge,
                        Some(std::cmp::Ordering::Greater | std::cmp::Ordering::Equal)
                    )
                    | (TraceCompareOp::Lt, Some(std::cmp::Ordering::Less))
                    | (
                        TraceCompareOp::Le,
                        Some(std::cmp::Ordering::Less | std::cmp::Ordering::Equal)
                    )
            ))
        }
        TraceCompareOp::Regex | TraceCompareOp::NotRegex => unreachable!(),
    }
}

fn semantic_partial_cmp<'a>(
    actual: Option<&SemanticValue<'a>>,
    expected: &SemanticValue<'a>,
) -> Option<std::cmp::Ordering> {
    match (actual?, expected) {
        (SemanticValue::Integer(left), SemanticValue::Integer(right)) => left.partial_cmp(right),
        (SemanticValue::Integer(left), SemanticValue::Float(right)) => {
            (*left as f64).partial_cmp(right)
        }
        (SemanticValue::Float(left), SemanticValue::Integer(right)) => {
            left.partial_cmp(&(*right as f64))
        }
        (SemanticValue::Float(left), SemanticValue::Float(right)) => left.partial_cmp(right),
        (SemanticValue::String(left), SemanticValue::String(right)) => left.partial_cmp(right),
        (SemanticValue::DurationNs(left), SemanticValue::DurationNs(right)) => {
            left.partial_cmp(right)
        }
        _ => None,
    }
}
fn structural<'a>(
    spans: &[SemanticSpan<'a>],
    left: &BTreeSet<SpanId>,
    right: &BTreeSet<SpanId>,
    op: StructuralOp,
) -> Result<BTreeSet<SpanId>, SemanticError> {
    let (op, negated) = match op {
        StructuralOp::NotChild => (StructuralOp::Child, true),
        StructuralOp::NotParent => (StructuralOp::Parent, true),
        StructuralOp::NotDescendant => (StructuralOp::Descendant, true),
        StructuralOp::NotAncestor => (StructuralOp::Ancestor, true),
        StructuralOp::NotSibling => (StructuralOp::Sibling, true),
        positive => (positive, false),
    };
    let by_id: BTreeMap<SpanId, &SemanticSpan<'a>> =
        spans.iter().map(|span| (span.span_id, span)).collect();
    let mut out = BTreeSet::new();
    for right_id in right {
        let Some(candidate) = by_id.get(right_id) else {
            continue;
        };
        let matches = match op {
            StructuralOp::Child => candidate
                .parent_span_id
                .is_some_and(|parent| left.contains(&parent)),
            StructuralOp::Parent => left.iter().any(|left_id| {
                by_id.get(left_id).and_then(|span| span.parent_span_id) == Some(*right_id)
            }),
            StructuralOp::Descendant => has_ancestor(candidate, left, &by_id)?,
            StructuralOp::Ancestor => {
                let ancestor = BTreeSet::from([*right_id]);
                let mut found = false;
                for left_id in left {
                    if let Some(span) = by_id.get(left_id)
                        && has_ancestor(span, &ancestor, &by_id)?
                    {
                        found = true;
                        break;
                    }
                }
                found
            }
            StructuralOp::Sibling => candidate.parent_span_id.is_some_and(|parent| {
                left.iter().any(|left_id| {
                    *left_id != *right_id
                        && by_id.get(left_id).and_then(|span| span.parent_span_id) == Some(parent)
                })
            }),
            StructuralOp::NotChild
            | StructuralOp::NotParent
            | StructuralOp::NotDescendant
            | StructuralOp::NotAncestor
            | StructuralOp::NotSibling => unreachable!("negated operator was normalized"),
        };
        let matches = if negated { !matches } else { matches };
        if matches {
            out.insert(*right_id);
        }
    }
    Ok(out)
}

fn has_ancestor<'a>(
    span: &SemanticSpan<'a>,
    ancestors: &BTreeSet<SpanId>,
    by_id: &BTreeMap<SpanId, &SemanticSpan<'a>>,
) -> Result<bool, SemanticError> {
    let mut seen = BTreeSet::new();
    let mut parent = span.parent_span_id;
    while let Some(id) = parent {
        if !seen.insert(id) {
            return Err(SemanticError::Malformed(
                "cycle in span parent relationships",
            ));
        }
        if ancestors.contains(&id) {
            return Ok(true);
        }
        parent = by_id.get(&id).and_then(|span| span.parent_span_id);
    }
    Ok(false)
}

#[cfg(test)]
mod candidate_filter_tests {
    use super::*;

    fn sel(p: SpanPredicate) -> SpansetExpr {
        SpansetExpr::Select(p)
    }

    #[test]
    fn bare_select_lifts_the_pushable_conjunction() {
        // `{ .name="x" && span.k = "v" && .duration > 5 }`
        let expr = sel(SpanPredicate::And(vec![
            SpanPredicate::NameEq("x".into()),
            SpanPredicate::SpanAttrEq("k".into(), "v".into()),
            SpanPredicate::Duration {
                op: TraceCompareOp::Gt,
                value_ns: 5,
            },
        ]));
        assert_eq!(
            candidate_filters(&expr),
            vec![
                SpanCandidateFilter::Name("x".into()),
                SpanCandidateFilter::AttrEq("k".into(), "v".into()),
                SpanCandidateFilter::DurationGe(5), // `>` widened to the sound superset `>=`
            ]
        );
    }

    fn span_attr_num(op: TraceCompareOp, value: SemanticValue<'static>) -> SpansetExpr {
        sel(SpanPredicate::Attribute {
            scope: AttributeScope::Span,
            key: "http.status_code".into(),
            op,
            value,
        })
    }

    #[test]
    fn integer_span_attribute_comparisons_lift_to_numeric_filters() {
        let key = "http.status_code".to_owned();
        // Each ordering operator maps to its numeric filter; the bound stays exact (i64).
        assert_eq!(
            candidate_filters(&span_attr_num(
                TraceCompareOp::Ge,
                SemanticValue::Integer(500)
            )),
            vec![SpanCandidateFilter::AttrNumGe(key.clone(), 500)]
        );
        assert_eq!(
            candidate_filters(&span_attr_num(
                TraceCompareOp::Gt,
                SemanticValue::Integer(500)
            )),
            vec![SpanCandidateFilter::AttrNumGt(key.clone(), 500)]
        );
        assert_eq!(
            candidate_filters(&span_attr_num(
                TraceCompareOp::Lt,
                SemanticValue::Integer(500)
            )),
            vec![SpanCandidateFilter::AttrNumLt(key.clone(), 500)]
        );
        assert_eq!(
            candidate_filters(&span_attr_num(
                TraceCompareOp::Le,
                SemanticValue::Integer(500)
            )),
            vec![SpanCandidateFilter::AttrNumLe(key.clone(), 500)]
        );
        // `=` becomes the closed range `>= v AND <= v` (both necessary conditions).
        assert_eq!(
            candidate_filters(&span_attr_num(
                TraceCompareOp::Eq,
                SemanticValue::Integer(500)
            )),
            vec![
                SpanCandidateFilter::AttrNumGe(key.clone(), 500),
                SpanCandidateFilter::AttrNumLe(key, 500),
            ]
        );
    }

    #[test]
    fn unpushable_numeric_span_attributes_stay_with_the_evaluator() {
        // `!=` is not a single-span necessary condition.
        assert!(
            candidate_filters(&span_attr_num(
                TraceCompareOp::Ne,
                SemanticValue::Integer(500)
            ))
            .is_empty()
        );
        // Float-valued literals are not lifted (only integers, to keep the filter `Eq`-comparable).
        assert!(
            candidate_filters(&span_attr_num(
                TraceCompareOp::Ge,
                SemanticValue::Float(1.5)
            ))
            .is_empty()
        );
        // A bound outside f64's exact integer range would round on the `i64 -> f64` widen; not lifted.
        assert!(
            candidate_filters(&span_attr_num(
                TraceCompareOp::Ge,
                SemanticValue::Integer((1i64 << 53) + 1)
            ))
            .is_empty()
        );
    }

    #[test]
    fn and_structural_count_at_least_push_one_necessary_side() {
        let name_x = sel(SpanPredicate::NameEq("x".into()));
        let status_e = sel(SpanPredicate::StatusEq("error".into()));
        // And / Structural: either side is necessary; the left (which yields a filter) is taken.
        assert_eq!(
            candidate_filters(&SpansetExpr::And(
                Box::new(name_x.clone()),
                Box::new(status_e.clone())
            )),
            vec![SpanCandidateFilter::Name("x".into())]
        );
        assert_eq!(
            candidate_filters(&SpansetExpr::Structural {
                left: Box::new(name_x.clone()),
                op: StructuralOp::Child,
                right: Box::new(status_e),
                union: false,
            }),
            vec![SpanCandidateFilter::Name("x".into())]
        );
        // count(...) >= 1 and countAtLeast(_, >=1) require a matching span → push the input's filter.
        assert_eq!(
            candidate_filters(&SpansetExpr::Count {
                input: Box::new(name_x.clone()),
                op: TraceCompareOp::Ge,
                value: 1,
            }),
            vec![SpanCandidateFilter::Name("x".into())]
        );
        assert_eq!(
            candidate_filters(&SpansetExpr::CountAtLeast(Box::new(name_x), 2)),
            vec![SpanCandidateFilter::Name("x".into())]
        );
    }

    #[test]
    fn unprovable_necessity_pushes_nothing() {
        let name_x = || sel(SpanPredicate::NameEq("x".into()));
        // Or: a match may come from either side, so neither is necessary.
        assert!(
            candidate_filters(&SpansetExpr::Or(Box::new(name_x()), Box::new(name_x()))).is_empty()
        );
        // count(...) <= n and == 0: a trace with zero matching spans can satisfy it.
        for op in [TraceCompareOp::Le, TraceCompareOp::Lt] {
            assert!(
                candidate_filters(&SpansetExpr::Count {
                    input: Box::new(name_x()),
                    op,
                    value: 3,
                })
                .is_empty()
            );
        }
        assert!(
            candidate_filters(&SpansetExpr::Count {
                input: Box::new(name_x()),
                op: TraceCompareOp::Eq,
                value: 0,
            })
            .is_empty()
        );
        // countAtLeast(_, 0) always matches.
        assert!(candidate_filters(&SpansetExpr::CountAtLeast(Box::new(name_x()), 0)).is_empty());
    }

    #[test]
    fn non_pushable_leaves_are_dropped_but_the_rest_of_a_conjunction_survives() {
        // Intrinsic (computed), Ne / regex, non-Span scope, numeric attr compares → not pushable.
        assert!(
            candidate_filters(&sel(SpanPredicate::Intrinsic {
                intrinsic: Intrinsic::SpanChildCount,
                op: TraceCompareOp::Ge,
                value: SemanticValue::Integer(2),
            }))
            .is_empty()
        );
        assert!(
            candidate_filters(&sel(SpanPredicate::ResourceAttrEq("k".into(), "v".into())))
                .is_empty()
        );
        // Partial pushdown of a conjunction is still sound: keep the pushable leaf, drop the rest.
        let expr = sel(SpanPredicate::And(vec![
            SpanPredicate::NameEq("x".into()),
            SpanPredicate::Intrinsic {
                intrinsic: Intrinsic::SpanChildCount,
                op: TraceCompareOp::Ge,
                value: SemanticValue::Integer(2),
            },
        ]));
        assert_eq!(
            candidate_filters(&expr),
            vec![SpanCandidateFilter::Name("x".into())]
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Encode a short test label into a raw span id (left-padded bytes) so ids stay `Copy` map keys;
    /// the evaluator's output is the *hex* of these, so assertions use `sid(..).to_hex()`.
    fn sid(s: &str) -> SpanId {
        let mut b = [0u8; 8];
        let bytes = s.as_bytes();
        let n = bytes.len().min(8);
        b[..n].copy_from_slice(&bytes[..n]);
        SpanId(b)
    }

    fn tid(s: &str) -> TraceId {
        let mut b = [0u8; 16];
        let bytes = s.as_bytes();
        let n = bytes.len().min(16);
        b[..n].copy_from_slice(&bytes[..n]);
        TraceId(b)
    }

    fn span<'a>(id: &str, parent: Option<&str>, name: &'a str) -> SemanticSpan<'a> {
        SemanticSpan {
            span_id: sid(id),
            parent_span_id: parent.map(sid),
            name,
            kind: "unspecified",
            status_message: None,
            duration_ns: 0,
            service: None,
            start_time_ns: 0,
            status: "unset",
            attributes: TypedAttributes::default(),
            resource: TypedAttributes::default(),
            instrumentation: TypedAttributes::default(),
            events: Vec::new(),
            links: Vec::new(),
        }
    }

    #[test]
    fn descendant_returns_matching_right_side() {
        let spans = vec![
            span("root", None, "frontend"),
            span("child", Some("root"), "middleware"),
            span("leaf", Some("child"), "database"),
        ];
        let expr = SpansetExpr::Structural {
            left: Box::new(SpansetExpr::Select(SpanPredicate::NameEq(
                "frontend".into(),
            ))),
            op: StructuralOp::Descendant,
            right: Box::new(SpansetExpr::Select(SpanPredicate::NameEq(
                "database".into(),
            ))),
            union: false,
        };
        assert_eq!(
            eval_spanset_reference(&spans, &expr, EvalLimits::default())
                .unwrap()
                .unwrap()
                .selected_span_ids,
            vec![sid("leaf").to_hex()]
        );
    }

    #[test]
    fn union_structural_returns_both_sides() {
        let spans = vec![span("root", None, "a"), span("child", Some("root"), "b")];
        let expr = SpansetExpr::Structural {
            left: Box::new(SpansetExpr::Select(SpanPredicate::NameEq("a".into()))),
            op: StructuralOp::Child,
            right: Box::new(SpansetExpr::Select(SpanPredicate::NameEq("b".into()))),
            union: true,
        };
        assert_eq!(
            eval_spanset_reference(&spans, &expr, EvalLimits::default())
                .unwrap()
                .unwrap()
                .selected_span_ids,
            vec![sid("child").to_hex(), sid("root").to_hex()]
        );
    }
    #[test]
    fn selector_and_requires_all_conditions_on_the_same_span() {
        let mut first = span("first", None, "one");
        first.attributes =
            TypedAttributes::new([("left".into(), SemanticValue::String("yes".into()))]);
        let mut second = span("second", None, "two");
        second.attributes =
            TypedAttributes::new([("right".into(), SemanticValue::String("yes".into()))]);
        let predicate = SpanPredicate::And(vec![
            SpanPredicate::SpanAttrEq("left".into(), "yes".into()),
            SpanPredicate::SpanAttrEq("right".into(), "yes".into()),
        ]);
        assert!(
            eval_spanset_reference(
                &[first, second],
                &SpansetExpr::Select(predicate),
                EvalLimits::default(),
            )
            .unwrap()
            .is_none()
        );
    }

    #[test]
    fn typed_scopes_numeric_comparison_and_regex_are_preserved() {
        let mut candidate = span("match", None, "request");
        candidate.attributes = TypedAttributes::new([("code".into(), SemanticValue::Integer(503))]);
        candidate.resource = TypedAttributes::new([(
            "deployment.environment".into(),
            SemanticValue::String("production".into()),
        )]);
        candidate.instrumentation =
            TypedAttributes::new([("name".into(), SemanticValue::String("http.server".into()))]);
        candidate.events = vec![TypedAttributes::new([(
            "exception.escaped".into(),
            SemanticValue::Boolean(true),
        )])];
        let predicate = SpanPredicate::And(vec![
            SpanPredicate::Attribute {
                scope: AttributeScope::Span,
                key: "code".into(),
                op: TraceCompareOp::Ge,
                value: SemanticValue::Float(500.0),
            },
            SpanPredicate::Attribute {
                scope: AttributeScope::Resource,
                key: "deployment.environment".into(),
                op: TraceCompareOp::Regex,
                value: SemanticValue::String("prod.*".into()),
            },
            SpanPredicate::Attribute {
                scope: AttributeScope::Instrumentation,
                key: "name".into(),
                op: TraceCompareOp::Eq,
                value: SemanticValue::String("http.server".into()),
            },
            SpanPredicate::Attribute {
                scope: AttributeScope::Event,
                key: "exception.escaped".into(),
                op: TraceCompareOp::Eq,
                value: SemanticValue::Boolean(true),
            },
        ]);
        assert_eq!(
            eval_spanset_reference(
                &[candidate.clone()],
                &SpansetExpr::Select(predicate),
                EvalLimits::default(),
            )
            .unwrap()
            .unwrap()
            .selected_span_ids,
            vec![sid("match").to_hex()]
        );

        let anchored = SpanPredicate::Attribute {
            scope: AttributeScope::Resource,
            key: "deployment.environment".into(),
            op: TraceCompareOp::Regex,
            value: SemanticValue::String("roduction".into()),
        };
        assert!(
            eval_spanset_reference(
                &[candidate],
                &SpansetExpr::Select(anchored),
                EvalLimits::default(),
            )
            .unwrap()
            .is_none()
        );
    }

    #[test]
    fn nil_matches_a_missing_scoped_attribute() {
        let predicate = SpanPredicate::Attribute {
            scope: AttributeScope::Span,
            key: "missing".into(),
            op: TraceCompareOp::Eq,
            value: SemanticValue::Nil,
        };
        assert_eq!(
            eval_spanset_reference(
                &[span("one", None, "request")],
                &SpansetExpr::Select(predicate),
                EvalLimits::default(),
            )
            .unwrap()
            .unwrap()
            .selected_span_ids,
            vec![sid("one").to_hex()]
        );
    }

    #[test]
    fn missing_attribute_does_not_match_negated_matchers() {
        // Tempo semantics: a span that lacks the referenced attribute is NOT matched by any
        // condition, including negated matchers (`!=`, `!~`). A span that HAS the attribute is
        // evaluated normally. `{ .foo = nil }` (presence test) stays covered separately.
        let attr = |op, value: &str| SpanPredicate::Attribute {
            scope: AttributeScope::Span,
            key: "foo".into(),
            op,
            value: SemanticValue::String(value.to_owned().into()),
        };
        let missing = span("missing", None, "request");
        let mut present = span("present", None, "request");
        present.attributes =
            TypedAttributes::new([("foo".into(), SemanticValue::String("baz".into()))]);

        let missing_ref = std::slice::from_ref(&missing);
        let present_ref = std::slice::from_ref(&present);

        // Missing `.foo` must NOT satisfy `{ .foo != "bar" }`.
        assert!(
            eval_spanset_reference(
                missing_ref,
                &SpansetExpr::Select(attr(TraceCompareOp::Ne, "bar")),
                EvalLimits::default(),
            )
            .unwrap()
            .is_none()
        );
        // Missing `.foo` must NOT satisfy `{ .foo !~ "b.*" }`.
        assert!(
            eval_spanset_reference(
                missing_ref,
                &SpansetExpr::Select(attr(TraceCompareOp::NotRegex, "b.*")),
                EvalLimits::default(),
            )
            .unwrap()
            .is_none()
        );

        // A present `.foo = "baz"` is evaluated normally: it satisfies `{ .foo != "bar" }`
        // (baz != bar) and `{ .foo !~ "x.*" }` (baz does not match `x.*`).
        assert_eq!(
            eval_spanset_reference(
                present_ref,
                &SpansetExpr::Select(attr(TraceCompareOp::Ne, "bar")),
                EvalLimits::default(),
            )
            .unwrap()
            .unwrap()
            .selected_span_ids,
            vec![sid("present").to_hex()]
        );
        assert_eq!(
            eval_spanset_reference(
                present_ref,
                &SpansetExpr::Select(attr(TraceCompareOp::NotRegex, "x.*")),
                EvalLimits::default(),
            )
            .unwrap()
            .unwrap()
            .selected_span_ids,
            vec![sid("present").to_hex()]
        );
        // ...and, unlike a missing attribute, a present value that DOES match the pattern makes
        // `!~` evaluate false: `.foo = "baz"` does not satisfy `{ .foo !~ "b.*" }` (baz ~ b.*).
        assert!(
            eval_spanset_reference(
                present_ref,
                &SpansetExpr::Select(attr(TraceCompareOp::NotRegex, "b.*")),
                EvalLimits::default(),
            )
            .unwrap()
            .is_none()
        );

        // Explicit-nil presence test: `{ .foo != nil }` on a missing attribute is not-matched
        // (the attribute does not exist), while `{ .foo = nil }` matches (covered above).
        assert!(
            eval_spanset_reference(
                missing_ref,
                &SpansetExpr::Select(SpanPredicate::Attribute {
                    scope: AttributeScope::Span,
                    key: "foo".into(),
                    op: TraceCompareOp::Ne,
                    value: SemanticValue::Nil,
                }),
                EvalLimits::default(),
            )
            .unwrap()
            .is_none()
        );
    }

    #[test]
    fn negated_parent_returns_right_spans_without_matching_children() {
        let spans = vec![
            span("root", None, "root"),
            span("leaf", Some("root"), "leaf"),
        ];
        let expression = SpansetExpr::Structural {
            left: Box::new(SpansetExpr::Select(SpanPredicate::All)),
            op: StructuralOp::NotParent,
            right: Box::new(SpansetExpr::Select(SpanPredicate::All)),
            union: false,
        };
        assert_eq!(
            eval_spanset_reference(&spans, &expression, EvalLimits::default())
                .unwrap()
                .unwrap()
                .selected_span_ids,
            vec![sid("leaf").to_hex()]
        );
    }

    #[test]
    fn array_positive_matches_any_and_negative_matches_none() {
        let mut candidate = span("array", None, "request");
        candidate.attributes = TypedAttributes::new([(
            "methods".into(),
            SemanticValue::Array(vec![
                SemanticValue::String("GET".into()),
                SemanticValue::String("POST".into()),
            ]),
        )]);
        let predicate = |op, value: &str| SpanPredicate::Attribute {
            scope: AttributeScope::Span,
            key: "methods".into(),
            op,
            value: SemanticValue::String(value.to_owned().into()),
        };
        assert!(
            eval_spanset_reference(
                &[candidate.clone()],
                &SpansetExpr::Select(predicate(TraceCompareOp::Eq, "POST")),
                EvalLimits::default(),
            )
            .unwrap()
            .is_some()
        );
        assert!(
            eval_spanset_reference(
                &[candidate.clone()],
                &SpansetExpr::Select(predicate(TraceCompareOp::Ne, "POST")),
                EvalLimits::default(),
            )
            .unwrap()
            .is_none()
        );
        assert!(
            eval_spanset_reference(
                &[candidate],
                &SpansetExpr::Select(predicate(TraceCompareOp::NotRegex, "PUT")),
                EvalLimits::default(),
            )
            .unwrap()
            .is_some()
        );
    }
    #[test]
    fn complete_trace_intrinsics_are_typed_and_child_count_is_per_span() {
        let trace = SemanticTrace {
            trace_id: tid("trace-one"),
            root_service: Some("checkout"),
            root_name: Some("POST /checkout"),
            start_time_ns: 10,
            duration_ns: 50,
            spans: vec![
                span("root", None, "POST /checkout"),
                span("child", Some("root"), "db"),
            ],
        };
        let expression = SpansetExpr::Select(SpanPredicate::And(vec![
            SpanPredicate::Intrinsic {
                intrinsic: Intrinsic::TraceRootService,
                op: TraceCompareOp::Eq,
                value: SemanticValue::String("checkout".into()),
            },
            SpanPredicate::Intrinsic {
                intrinsic: Intrinsic::TraceDuration,
                op: TraceCompareOp::Ge,
                value: SemanticValue::DurationNs(50),
            },
            SpanPredicate::Intrinsic {
                intrinsic: Intrinsic::SpanChildCount,
                op: TraceCompareOp::Eq,
                value: SemanticValue::Integer(1),
            },
        ]));
        assert_eq!(
            eval_trace_reference(&trace, &expression, EvalLimits::default())
                .unwrap()
                .unwrap()
                .selected_span_ids,
            vec![sid("root").to_hex()]
        );
        assert!(matches!(
            eval_spanset_reference(&trace.spans, &expression, EvalLimits::default()),
            Err(SemanticError::Incompatible(_))
        ));
    }
}

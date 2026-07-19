use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;

use crate::{LabelSet, SemanticError};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FloatSample {
    pub timestamp_ns: i64,
    pub value: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ClassicHistogramBucket {
    pub upper_bound: f64,
    pub cumulative_count: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HistogramQuantileResult {
    pub value: f64,
    pub annotations: Vec<crate::Annotation>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PromSeries<'a> {
    pub labels: LabelSet<'a>,
    pub samples: Vec<FloatSample>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HistogramPoint<'a> {
    pub timestamp_ns: i64,
    /// Bucket boundaries, borrowed from the backing store's `ListArray` values buffer (zero-copy);
    /// read-only in the evaluator, consumed into scalar quantiles before any owned boundary.
    pub explicit_bounds: &'a [f64],
    pub bucket_counts: &'a [u64],
}

#[derive(Debug, Clone, PartialEq)]
pub struct PromHistogramSeries<'a> {
    pub labels: LabelSet<'a>,
    pub points: Vec<HistogramPoint<'a>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchOp {
    Eq,
    Ne,
    Regex,
    NotRegex,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LabelMatcher {
    pub name: String,
    pub op: MatchOp,
    pub value: String,
}

impl LabelMatcher {
    pub fn matches(&self, labels: &LabelSet) -> Result<bool, SemanticError> {
        let actual = labels.get(&self.name).unwrap_or("");
        Ok(match self.op {
            MatchOp::Eq => actual == self.value,
            MatchOp::Ne => actual != self.value,
            MatchOp::Regex | MatchOp::NotRegex => {
                let pattern = format!("^(?:{})$", self.value);
                let matched = regex::Regex::new(&pattern)
                    .map_err(|_| SemanticError::Malformed("invalid PromQL label regex"))?
                    .is_match(actual);
                if self.op == MatchOp::Regex {
                    matched
                } else {
                    !matched
                }
            }
        })
    }
}

fn selector_matches(labels: &LabelSet, matchers: &[LabelMatcher]) -> Result<bool, SemanticError> {
    matchers.iter().try_fold(true, |matched, matcher| {
        Ok(matched && matcher.matches(labels)?)
    })
}

/// Select the latest eligible sample at or before one PromQL evaluation timestamp.
///
/// The lookback lower bound is open and the upper bound is closed: a sample is eligible iff
/// `at_ns - lookback_ns < timestamp_ns <= at_ns`. This mirrors Prometheus' `vectorSelectorSingle`,
/// which drops a candidate point when `t <= refTime - lookbackDelta`.
pub fn select_instant<'a>(
    series: &[PromSeries<'a>],
    matchers: &[LabelMatcher],
    at_ns: i64,
    lookback_ns: u64,
) -> Result<Vec<(LabelSet<'a>, FloatSample)>, SemanticError> {
    let earliest = at_ns.saturating_sub(lookback_ns.min(i64::MAX as u64) as i64);
    let mut out = Vec::new();
    for item in series {
        if !selector_matches(&item.labels, matchers)? {
            continue;
        }
        if let Some(sample) = item
            .samples
            .iter()
            .filter(|sample| sample.timestamp_ns > earliest && sample.timestamp_ns <= at_ns)
            .max_by_key(|sample| sample.timestamp_ns)
        {
            out.push((item.labels.clone(), *sample));
        }
    }
    Ok(out)
}

/// Select one PromQL range vector. The lower bound is open and the upper bound is closed.
pub fn select_range(
    series: &PromSeries<'_>,
    at_ns: i64,
    window_ns: u64,
) -> Result<Vec<FloatSample>, SemanticError> {
    if window_ns == 0 {
        return Err(SemanticError::InvalidRange);
    }
    let start = at_ns.saturating_sub(window_ns.min(i64::MAX as u64) as i64);
    Ok(series
        .samples
        .iter()
        .copied()
        .filter(|sample| sample.timestamp_ns > start && sample.timestamp_ns <= at_ns)
        .collect())
}

#[derive(Debug, Clone, PartialEq)]
pub enum PromExpr {
    Selector {
        matchers: Vec<LabelMatcher>,
    },
    Rate {
        matchers: Vec<LabelMatcher>,
        window_ns: u64,
    },
    HistogramQuantile {
        phi: f64,
        matchers: Vec<LabelMatcher>,
        window_ns: u64,
        grouping: Grouping,
    },
    Aggregate {
        op: PromAggregate,
        grouping: Grouping,
        input: Box<PromExpr>,
    },
}

/// The storage-level sample kind required by an expression leaf.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromFetchPurpose {
    InstantSelector,
    CumulativeCounterRate,
    CumulativeHistogramRate,
}

/// One bounded storage read needed by a PromQL expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromFetchRequest {
    pub bounds: crate::FetchBounds,
    pub purpose: PromFetchPurpose,
    pub matchers: Vec<LabelMatcher>,
    pub max_series: usize,
    pub max_samples: usize,
}

/// Storage reads required to evaluate an expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromFetchPlan {
    pub requests: Vec<PromFetchRequest>,
}

type PromSeriesDependent<'a> = Vec<PromSeries<'a>>;
type PromHistogramDependent<'a> = Vec<PromHistogramSeries<'a>>;

self_cell::self_cell! {
    /// A batch of grouped scalar series whose labels *borrow* from an owned, type-erased backing
    /// store (the source's materialized rows). Kept whole so the borrows stay valid: the evaluator
    /// reads [`PromSeriesPack::borrow_dependent`] and materializes owned labels only at the public
    /// boundary. The owner is `Box<dyn Any + Send>` so this model type stays free of the facade row
    /// type it actually borrows from (the concrete downcast lives in the source builder).
    pub struct PromSeriesPack {
        owner: Box<dyn std::any::Any + Send>,
        #[covariant]
        dependent: PromSeriesDependent,
    }
}

self_cell::self_cell! {
    /// Histogram twin of [`PromSeriesPack`].
    pub struct PromHistogramPack {
        owner: Box<dyn std::any::Any + Send>,
        #[covariant]
        dependent: PromHistogramDependent,
    }
}

/// A bounded source of normalized Prometheus-compatible samples.
///
/// Implementations must apply `bounds` in storage and stop at the supplied limits. Returning a
/// complete metric history is a contract violation, not an evaluator requirement. Series are returned
/// inside a self-owning [`PromSeriesPack`] so their labels can borrow the source's backing store
/// without an owned copy per row.
pub trait PromSeriesSource {
    fn fetch<'a>(
        &'a self,
        request: &'a PromFetchRequest,
    ) -> Pin<Box<dyn Future<Output = Result<PromSeriesPack, SemanticError>> + Send + 'a>>;

    fn fetch_histograms<'a>(
        &'a self,
        _request: &'a PromFetchRequest,
    ) -> Pin<Box<dyn Future<Output = Result<PromHistogramPack, SemanticError>> + Send + 'a>> {
        Box::pin(async { Err(SemanticError::Incompatible("histogram source unavailable")) })
    }
}

/// Derive the smallest storage windows needed by the currently supported expression tree.
pub fn plan_prom_fetch(
    expr: &PromExpr,
    range: crate::EvalRange,
    limits: crate::EvalLimits,
) -> Result<PromFetchPlan, SemanticError> {
    range.instants(limits)?;
    let mut requests = Vec::new();
    collect_fetches(expr, range, limits, &mut requests, 0)?;
    requests.sort_by(|a, b| {
        a.bounds
            .start_ns
            .cmp(&b.bounds.start_ns)
            .then(a.bounds.end_ns.cmp(&b.bounds.end_ns))
            .then(a.matchers.len().cmp(&b.matchers.len()))
    });
    requests.dedup();
    Ok(PromFetchPlan { requests })
}
/// Reference evaluator over an already bounded normalized working set.
/// Production callers should use [`execute_prom`], which plans and enforces bounded storage reads.
pub fn eval_prom_reference<'a>(
    expr: &PromExpr,
    series: &[PromSeries<'a>],
    range: crate::EvalRange,
    limits: crate::EvalLimits,
) -> Result<Vec<PromSeries<'a>>, SemanticError> {
    let instants = range.instants(limits)?;
    eval_prom_at(expr, series, &[], &instants, range.lookback_ns, limits, 0)
}

pub fn eval_prom_with_histograms_reference<'a>(
    expr: &PromExpr,
    series: &[PromSeries<'a>],
    histograms: &[PromHistogramSeries<'a>],
    range: crate::EvalRange,
    limits: crate::EvalLimits,
) -> Result<Vec<PromSeries<'a>>, SemanticError> {
    let instants = range.instants(limits)?;
    eval_prom_at(
        expr,
        series,
        histograms,
        &instants,
        range.lookback_ns,
        limits,
        0,
    )
}

/// Plan bounded reads, fetch them from storage, and evaluate the resulting working set.
pub async fn execute_prom<S: PromSeriesSource + ?Sized>(
    source: &S,
    expr: &PromExpr,
    range: crate::EvalRange,
    limits: crate::EvalLimits,
) -> Result<Vec<PromSeries<'static>>, SemanticError> {
    let plan = plan_prom_fetch(expr, range, limits)?;
    // Fetch every bounded read up front and hold the packs for the whole evaluation: their labels
    // are borrowed from each pack's backing store, so the packs must outlive the working set. Owned
    // labels are materialized exactly once, at the return boundary (`into_owned` below).
    let mut scalar_packs: Vec<(&PromFetchRequest, PromSeriesPack)> = Vec::new();
    let mut histogram_packs: Vec<(&PromFetchRequest, PromHistogramPack)> = Vec::new();
    for request in &plan.requests {
        if request.purpose == PromFetchPurpose::CumulativeHistogramRate {
            histogram_packs.push((request, source.fetch_histograms(request).await?));
        } else {
            scalar_packs.push((request, source.fetch(request).await?));
        }
    }

    let mut by_labels: BTreeMap<LabelSet<'_>, Vec<FloatSample>> = BTreeMap::new();
    let mut histogram_by_labels: BTreeMap<LabelSet<'_>, Vec<HistogramPoint<'_>>> = BTreeMap::new();
    let mut fetched_samples = 0usize;
    for (request, pack) in &histogram_packs {
        let fetched = pack.borrow_dependent();
        if fetched.len() > request.max_series {
            return Err(SemanticError::LimitExceeded(
                "PromQL source histogram series",
            ));
        }
        for series in fetched {
            for point in &series.points {
                if !request.bounds.contains(point.timestamp_ns) {
                    return Err(SemanticError::Malformed(
                        "PromQL source returned a histogram outside requested bounds",
                    ));
                }
                fetched_samples = fetched_samples
                    .checked_add(1)
                    .ok_or(SemanticError::LimitExceeded("PromQL source samples"))?;
                if fetched_samples > limits.max_samples {
                    return Err(SemanticError::LimitExceeded("PromQL source samples"));
                }
                histogram_by_labels
                    .entry(series.labels.clone())
                    .or_default()
                    .push(*point);
            }
        }
    }
    for (request, pack) in &scalar_packs {
        let fetched = pack.borrow_dependent();
        if fetched.len() > request.max_series {
            return Err(SemanticError::LimitExceeded("PromQL source series"));
        }
        for series in fetched {
            for sample in &series.samples {
                if !request.bounds.contains(sample.timestamp_ns) {
                    return Err(SemanticError::Malformed(
                        "PromQL source returned a sample outside requested bounds",
                    ));
                }
                fetched_samples = fetched_samples
                    .checked_add(1)
                    .ok_or(SemanticError::LimitExceeded("PromQL source samples"))?;
                if fetched_samples > limits.max_samples {
                    return Err(SemanticError::LimitExceeded("PromQL source samples"));
                }
                by_labels
                    .entry(series.labels.clone())
                    .or_default()
                    .push(*sample);
            }
        }
    }
    if by_labels.len() > limits.max_series {
        return Err(SemanticError::LimitExceeded("PromQL source series"));
    }
    let mut working_set = Vec::with_capacity(by_labels.len());
    for (labels, mut samples) in by_labels {
        samples.sort_by_key(|sample| sample.timestamp_ns);
        if samples
            .windows(2)
            .any(|pair| pair[0].timestamp_ns == pair[1].timestamp_ns)
        {
            return Err(SemanticError::Malformed(
                "duplicate timestamps in one PromQL series",
            ));
        }
        working_set.push(PromSeries { labels, samples });
    }
    if histogram_by_labels.len() > limits.max_series {
        return Err(SemanticError::LimitExceeded(
            "PromQL source histogram series",
        ));
    }
    let mut histogram_working_set = Vec::with_capacity(histogram_by_labels.len());
    for (labels, mut points) in histogram_by_labels {
        points.sort_by_key(|point| point.timestamp_ns);
        if points
            .windows(2)
            .any(|pair| pair[0].timestamp_ns == pair[1].timestamp_ns)
        {
            return Err(SemanticError::Malformed(
                "duplicate timestamps in one PromQL histogram series",
            ));
        }
        histogram_working_set.push(PromHistogramSeries { labels, points });
    }
    let evaluated = eval_prom_with_histograms_reference(
        expr,
        &working_set,
        &histogram_working_set,
        range,
        limits,
    )?;
    // Single materialization boundary: the borrowed labels (which reference the packs held above)
    // become owned here, so the returned series outlive the packs when they drop at function end.
    Ok(evaluated
        .into_iter()
        .map(|series| PromSeries {
            labels: series.labels.into_owned(),
            samples: series.samples,
        })
        .collect())
}

fn collect_fetches(
    expr: &PromExpr,
    range: crate::EvalRange,
    limits: crate::EvalLimits,
    out: &mut Vec<PromFetchRequest>,
    depth: usize,
) -> Result<(), SemanticError> {
    if depth >= limits.max_recursion {
        return Err(SemanticError::LimitExceeded("PromQL expression recursion"));
    }
    let (matchers, horizon_ns, purpose) = match expr {
        PromExpr::Selector { matchers } => (
            matchers,
            range.lookback_ns,
            PromFetchPurpose::InstantSelector,
        ),
        PromExpr::Rate {
            matchers,
            window_ns,
        } => (
            matchers,
            *window_ns,
            PromFetchPurpose::CumulativeCounterRate,
        ),
        PromExpr::HistogramQuantile {
            matchers,
            window_ns,
            ..
        } => (
            matchers,
            *window_ns,
            PromFetchPurpose::CumulativeHistogramRate,
        ),

        PromExpr::Aggregate { input, .. } => {
            return collect_fetches(input, range, limits, out, depth + 1);
        }
    };
    for matcher in matchers {
        if matches!(matcher.op, MatchOp::Regex | MatchOp::NotRegex) {
            let pattern = format!("^(?:{})$", matcher.value);
            regex::Regex::new(&pattern)
                .map_err(|_| SemanticError::Malformed("invalid PromQL label regex"))?;
        }
    }
    let start_ns = range
        .start_ns
        .saturating_sub(horizon_ns.min(i64::MAX as u64) as i64);
    out.push(PromFetchRequest {
        bounds: crate::FetchBounds::new(start_ns, range.end_ns)?,
        purpose,
        matchers: matchers.clone(),
        max_series: limits.max_series,
        max_samples: limits.max_samples,
    });
    Ok(())
}
fn eval_prom_at<'a>(
    expr: &PromExpr,
    series: &[PromSeries<'a>],
    histograms: &[PromHistogramSeries<'a>],
    instants: &[i64],
    lookback_ns: u64,
    limits: crate::EvalLimits,
    depth: usize,
) -> Result<Vec<PromSeries<'a>>, SemanticError> {
    if depth >= limits.max_recursion {
        return Err(SemanticError::LimitExceeded("PromQL expression recursion"));
    }
    match expr {
        PromExpr::Selector { matchers } => {
            let mut out: BTreeMap<LabelSet<'a>, Vec<FloatSample>> = BTreeMap::new();
            for &at in instants {
                for (labels, sample) in select_instant(series, matchers, at, lookback_ns)? {
                    out.entry(labels).or_default().push(FloatSample {
                        timestamp_ns: at,
                        value: sample.value,
                    });
                }
            }
            bounded_series(out, limits)
        }
        PromExpr::Rate {
            matchers,
            window_ns,
        } => {
            let mut out: BTreeMap<LabelSet<'a>, Vec<FloatSample>> = BTreeMap::new();
            for &at in instants {
                let start = at.saturating_sub((*window_ns).min(i64::MAX as u64) as i64);
                for item in series {
                    if !selector_matches(&item.labels, matchers)? {
                        continue;
                    }
                    let selected = select_range(item, at, *window_ns)?;
                    if let Some(value) = extrapolated_rate(&selected, start, at)? {
                        out.entry(item.labels.without(&[]))
                            .or_default()
                            .push(FloatSample {
                                timestamp_ns: at,
                                value,
                            });
                    }
                }
            }
            bounded_series(out, limits)
        }
        PromExpr::HistogramQuantile {
            phi,
            matchers,
            window_ns,
            grouping,
        } => eval_histogram_quantiles(
            histograms, matchers, *window_ns, *phi, grouping, instants, limits,
        ),
        PromExpr::Aggregate {
            op,
            grouping,
            input,
        } => {
            let input = eval_prom_at(
                input,
                series,
                histograms,
                instants,
                lookback_ns,
                limits,
                depth + 1,
            )?;
            let mut out: BTreeMap<LabelSet<'a>, Vec<FloatSample>> = BTreeMap::new();
            for &at in instants {
                let vector = input
                    .iter()
                    .filter_map(|item| {
                        item.samples
                            .iter()
                            .find(|sample| sample.timestamp_ns == at)
                            .map(|sample| (item.labels.clone(), sample.value))
                    })
                    .collect::<Vec<_>>();
                for (labels, value) in aggregate_instant(&vector, *op, grouping) {
                    out.entry(labels).or_default().push(FloatSample {
                        timestamp_ns: at,
                        value,
                    });
                }
            }
            bounded_series(out, limits)
        }
    }
}

fn eval_histogram_quantiles<'a>(
    histograms: &[PromHistogramSeries<'a>],
    matchers: &[LabelMatcher],
    window_ns: u64,
    phi: f64,
    grouping: &Grouping,
    instants: &[i64],
    limits: crate::EvalLimits,
) -> Result<Vec<PromSeries<'a>>, SemanticError> {
    if window_ns == 0 {
        return Err(SemanticError::InvalidRange);
    }
    let mut output: BTreeMap<LabelSet<'a>, Vec<FloatSample>> = BTreeMap::new();
    for &at in instants {
        let start = at.saturating_sub(window_ns.min(i64::MAX as u64) as i64);
        let mut groups: BTreeMap<LabelSet<'a>, Vec<ClassicHistogramBucket>> = BTreeMap::new();
        for histogram in histograms {
            if !selector_matches(&histogram.labels, matchers)? {
                continue;
            }
            let selected = histogram
                .points
                .iter()
                .filter(|point| point.timestamp_ns > start && point.timestamp_ns <= at)
                .collect::<Vec<_>>();
            if selected.len() < 2 {
                continue;
            }
            if selected
                .windows(2)
                .any(|pair| pair[0].timestamp_ns >= pair[1].timestamp_ns)
            {
                return Err(SemanticError::Malformed(
                    "histogram points are not strictly ordered",
                ));
            }
            let bounds = selected[0].explicit_bounds;
            if selected.iter().any(|point| {
                point.explicit_bounds != bounds
                    || point.bucket_counts.len() != bounds.len().saturating_add(1)
            }) {
                return Err(SemanticError::Incompatible(
                    "histogram bucket boundaries changed within a range",
                ));
            }

            let labels = match grouping {
                Grouping::By(names) => histogram.labels.by(names),
                Grouping::Without(names) => histogram.labels.without(names),
            };
            let group = groups.entry(labels).or_default();
            for bucket_index in 0..=bounds.len() {
                let mut samples = Vec::with_capacity(selected.len());
                for point in &selected {
                    let cumulative_count = point.bucket_counts[..=bucket_index]
                        .iter()
                        .copied()
                        .fold(0_u64, u64::saturating_add);
                    samples.push(FloatSample {
                        timestamp_ns: point.timestamp_ns,
                        value: cumulative_count as f64,
                    });
                }
                if let Some(rate) = extrapolated_rate(&samples, start, at)? {
                    let upper_bound = bounds.get(bucket_index).copied().unwrap_or(f64::INFINITY);
                    if let Some(bucket) = group
                        .iter_mut()
                        .find(|bucket| bucket.upper_bound == upper_bound)
                    {
                        bucket.cumulative_count += rate;
                    } else {
                        group.push(ClassicHistogramBucket {
                            upper_bound,
                            cumulative_count: rate,
                        });
                    }
                }
            }
        }

        for (labels, buckets) in groups {
            let value = classic_histogram_quantile(phi, &buckets)?.value;
            output.entry(labels).or_default().push(FloatSample {
                timestamp_ns: at,
                value,
            });
        }
    }
    bounded_series(output, limits)
}

fn bounded_series<'a>(
    input: BTreeMap<LabelSet<'a>, Vec<FloatSample>>,
    limits: crate::EvalLimits,
) -> Result<Vec<PromSeries<'a>>, SemanticError> {
    if input.len() > limits.max_series {
        return Err(SemanticError::LimitExceeded("PromQL series"));
    }
    let sample_count = input.values().map(Vec::len).sum::<usize>();
    if sample_count > limits.max_samples {
        return Err(SemanticError::LimitExceeded("PromQL samples"));
    }
    Ok(input
        .into_iter()
        .map(|(labels, samples)| PromSeries { labels, samples })
        .collect())
}

/// Prometheus-compatible extrapolated rate for a cumulative monotonic float counter.
///
/// The range is left-open and right-closed at the selector layer. This function expects the selected
/// samples in timestamp order and implements reset correction plus boundary extrapolation.
pub fn extrapolated_rate(
    samples: &[FloatSample],
    range_start_ns: i64,
    range_end_ns: i64,
) -> Result<Option<f64>, SemanticError> {
    if range_end_ns <= range_start_ns {
        return Err(SemanticError::InvalidRange);
    }
    if samples.len() < 2 {
        return Ok(None);
    }
    if samples
        .windows(2)
        .any(|pair| pair[0].timestamp_ns >= pair[1].timestamp_ns)
    {
        return Err(SemanticError::Malformed(
            "counter samples are not strictly ordered",
        ));
    }
    let first = samples[0];
    let last = samples[samples.len() - 1];
    if first.timestamp_ns <= range_start_ns || last.timestamp_ns > range_end_ns {
        return Err(SemanticError::Malformed(
            "counter sample is outside the selected range",
        ));
    }

    let mut increase = last.value - first.value;
    let mut previous = first.value;
    for sample in &samples[1..] {
        if sample.value < previous {
            increase += previous;
        }
        previous = sample.value;
    }

    let sampled_interval = seconds(last.timestamp_ns - first.timestamp_ns);
    if sampled_interval <= 0.0 {
        return Ok(None);
    }
    let average_interval = sampled_interval / (samples.len() - 1) as f64;
    let threshold = average_interval * 1.1;
    let mut to_start = seconds(first.timestamp_ns - range_start_ns);
    let mut to_end = seconds(range_end_ns - last.timestamp_ns);

    if increase > 0.0 && first.value >= 0.0 {
        let to_zero = sampled_interval * (first.value / increase);
        if to_zero < to_start {
            to_start = to_zero;
        }
    }
    if to_start >= threshold {
        to_start = average_interval / 2.0;
    }
    if to_end >= threshold {
        to_end = average_interval / 2.0;
    }

    let range_seconds = seconds(range_end_ns - range_start_ns);
    let factor = (sampled_interval + to_start + to_end) / sampled_interval / range_seconds;
    Ok(Some(increase * factor))
}

fn seconds(ns: i64) -> f64 {
    ns as f64 / 1_000_000_000.0
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromAggregate {
    Sum,
    Avg,
    Min,
    Max,
    Count,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Grouping {
    By(Vec<String>),
    Without(Vec<String>),
}

/// Aggregate one instant vector while applying PromQL label-retention rules.
pub fn aggregate_instant<'a>(
    input: &[(LabelSet<'a>, f64)],
    op: PromAggregate,
    grouping: &Grouping,
) -> Vec<(LabelSet<'a>, f64)> {
    let mut groups: BTreeMap<LabelSet<'a>, (f64, usize)> = BTreeMap::new();
    for (labels, value) in input {
        let key = match grouping {
            Grouping::By(names) => labels.by(names),
            Grouping::Without(names) => labels.without(names),
        };
        let entry = groups.entry(key).or_insert(match op {
            PromAggregate::Min => (f64::INFINITY, 0),
            PromAggregate::Max => (f64::NEG_INFINITY, 0),
            _ => (0.0, 0),
        });
        entry.0 = match op {
            PromAggregate::Sum | PromAggregate::Avg => entry.0 + value,
            PromAggregate::Min => entry.0.min(*value),
            PromAggregate::Max => entry.0.max(*value),
            PromAggregate::Count => entry.0 + 1.0,
        };
        entry.1 += 1;
    }
    groups
        .into_iter()
        .map(|(labels, (value, count))| {
            let value = if op == PromAggregate::Avg {
                value / count as f64
            } else {
                value
            };
            (labels, value)
        })
        .collect()
}

const HISTOGRAM_MONOTONICITY_TOLERANCE: f64 = 1e-12;

/// Prometheus-compatible quantile calculation over classic cumulative histogram buckets.
///
/// Duplicate upper bounds are coalesced, tiny floating-point monotonicity differences are ignored,
/// and material decreases are repaired with an annotation, matching the behavior required after
/// bucket-rate aggregation.
pub fn classic_histogram_quantile(
    phi: f64,
    buckets: &[ClassicHistogramBucket],
) -> Result<HistogramQuantileResult, SemanticError> {
    if phi.is_nan() {
        return Ok(HistogramQuantileResult {
            value: f64::NAN,
            annotations: Vec::new(),
        });
    }
    if phi < 0.0 {
        return Ok(HistogramQuantileResult {
            value: f64::NEG_INFINITY,
            annotations: Vec::new(),
        });
    }
    if phi > 1.0 {
        return Ok(HistogramQuantileResult {
            value: f64::INFINITY,
            annotations: Vec::new(),
        });
    }

    let mut sorted = buckets.to_vec();
    sorted.sort_by(|left, right| left.upper_bound.total_cmp(&right.upper_bound));
    let mut coalesced: Vec<ClassicHistogramBucket> = Vec::with_capacity(sorted.len());
    for bucket in sorted {
        if bucket.upper_bound.is_nan()
            || bucket.cumulative_count.is_nan()
            || bucket.cumulative_count < 0.0
        {
            return Err(SemanticError::Malformed("invalid classic histogram bucket"));
        }
        if let Some(previous) = coalesced.last_mut()
            && previous.upper_bound == bucket.upper_bound
        {
            previous.cumulative_count += bucket.cumulative_count;
            continue;
        }
        coalesced.push(bucket);
    }
    if coalesced.len() < 2
        || !coalesced
            .last()
            .is_some_and(|bucket| bucket.upper_bound == f64::INFINITY)
    {
        return Ok(HistogramQuantileResult {
            value: f64::NAN,
            annotations: Vec::new(),
        });
    }

    let mut forced_monotonicity = false;
    for index in 1..coalesced.len() {
        let previous = coalesced[index - 1].cumulative_count;
        let current = coalesced[index].cumulative_count;
        if current >= previous {
            continue;
        }
        let difference = previous - current;
        let scale = previous.abs() + current.abs();
        if difference > HISTOGRAM_MONOTONICITY_TOLERANCE * scale {
            forced_monotonicity = true;
        }
        coalesced[index].cumulative_count = previous;
    }
    let observations = coalesced.last().expect("checked above").cumulative_count;
    let annotations = forced_monotonicity
        .then(|| crate::Annotation {
            code: "promql_histogram_monotonicity_repaired",
            message: "input to histogram_quantile required monotonicity repair".to_owned(),
        })
        .into_iter()
        .collect();
    if observations == 0.0 {
        return Ok(HistogramQuantileResult {
            value: f64::NAN,
            annotations,
        });
    }

    let rank = phi * observations;
    let bucket_index = coalesced
        .iter()
        .position(|bucket| bucket.cumulative_count >= rank)
        .unwrap_or(coalesced.len() - 1);
    if bucket_index == coalesced.len() - 1 {
        return Ok(HistogramQuantileResult {
            value: coalesced[bucket_index - 1].upper_bound,
            annotations,
        });
    }
    let bucket = coalesced[bucket_index];
    if bucket_index == 0 && bucket.upper_bound <= 0.0 {
        return Ok(HistogramQuantileResult {
            value: bucket.upper_bound,
            annotations,
        });
    }

    let (lower, previous_count) = if bucket_index == 0 {
        (0.0, 0.0)
    } else {
        let previous = coalesced[bucket_index - 1];
        (previous.upper_bound, previous.cumulative_count)
    };
    let bucket_count = bucket.cumulative_count - previous_count;
    let within_bucket = rank - previous_count;
    let value = if bucket_count == 0.0 {
        lower
    } else {
        lower + (bucket.upper_bound - lower) * (within_bucket / bucket_count)
    };
    Ok(HistogramQuantileResult { value, annotations })
}
#[cfg(test)]
mod tests {
    use super::*;

    const S: i64 = 1_000_000_000;

    #[test]
    fn rate_corrects_reset_before_extrapolating() {
        let samples = [
            FloatSample {
                timestamp_ns: 10 * S,
                value: 5.0,
            },
            FloatSample {
                timestamp_ns: 20 * S,
                value: 9.0,
            },
            FloatSample {
                timestamp_ns: 30 * S,
                value: 2.0,
            },
            FloatSample {
                timestamp_ns: 40 * S,
                value: 7.0,
            },
        ];
        let rate = extrapolated_rate(&samples, 0, 50 * S).unwrap().unwrap();
        assert!((rate - 11.0 / 30.0).abs() < 1e-12, "{rate}");
    }

    #[test]
    fn rate_needs_two_samples() {
        let one = [FloatSample {
            timestamp_ns: S,
            value: 1.0,
        }];
        assert_eq!(extrapolated_rate(&one, 0, 2 * S).unwrap(), None);
    }

    #[test]
    fn aggregation_drops_metric_name_and_retains_group() {
        let a = LabelSet::new([
            ("__name__".to_owned(), "requests".to_owned()),
            ("route".to_owned(), "/a".to_owned()),
            ("instance".to_owned(), "one".to_owned()),
        ]);
        let b = LabelSet::new([
            ("__name__".to_owned(), "requests".to_owned()),
            ("route".to_owned(), "/a".to_owned()),
            ("instance".to_owned(), "two".to_owned()),
        ]);
        let out = aggregate_instant(
            &[(a, 2.0), (b, 3.0)],
            PromAggregate::Sum,
            &Grouping::By(vec!["route".to_owned()]),
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].0.iter().collect::<Vec<_>>(), vec![("route", "/a")]);
        assert_eq!(out[0].1, 5.0);
    }

    #[test]
    fn regex_matchers_are_anchored_and_missing_labels_are_empty() {
        let labels = LabelSet::new([("service".to_owned(), "api-server".to_owned())]);
        let anchored = LabelMatcher {
            name: "service".into(),
            op: MatchOp::Regex,
            value: "api".into(),
        };
        assert!(!anchored.matches(&labels).unwrap());
        let absent = LabelMatcher {
            name: "missing".into(),
            op: MatchOp::Regex,
            value: ".*".into(),
        };
        assert!(absent.matches(&labels).unwrap());
    }

    #[test]
    fn selectors_apply_lookback_and_open_range_boundary() {
        let series = PromSeries {
            labels: LabelSet::default(),
            samples: vec![
                FloatSample {
                    timestamp_ns: 10,
                    value: 1.0,
                },
                FloatSample {
                    timestamp_ns: 20,
                    value: 2.0,
                },
                FloatSample {
                    timestamp_ns: 30,
                    value: 3.0,
                },
            ],
        };
        let instant = select_instant(std::slice::from_ref(&series), &[], 31, 10).unwrap();
        assert_eq!(instant[0].1.value, 3.0);
        assert!(
            select_instant(std::slice::from_ref(&series), &[], 50, 10)
                .unwrap()
                .is_empty()
        );

        // Instant lower bound is open, mirroring Prometheus: a sample landing exactly at
        // `at - lookback` is excluded, while one landing one ns later is selected.
        let edge = PromSeries {
            labels: LabelSet::default(),
            samples: vec![
                FloatSample {
                    timestamp_ns: 10,
                    value: 100.0,
                },
                FloatSample {
                    timestamp_ns: 11,
                    value: 200.0,
                },
            ],
        };
        // at=20, lookback=10 => earliest=10; the sample at 10 sits on the excluded edge, so the
        // sample at 11 (11 > 10) is the one selected.
        let picked = select_instant(std::slice::from_ref(&edge), &[], 20, 10).unwrap();
        assert_eq!(picked[0].1.value, 200.0);
        // With only the edge sample present, nothing is eligible.
        let edge_only = PromSeries {
            labels: LabelSet::default(),
            samples: vec![FloatSample {
                timestamp_ns: 10,
                value: 100.0,
            }],
        };
        assert!(
            select_instant(std::slice::from_ref(&edge_only), &[], 20, 10)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            select_range(&series, 30, 20).unwrap(),
            vec![
                FloatSample {
                    timestamp_ns: 20,
                    value: 2.0
                },
                FloatSample {
                    timestamp_ns: 30,
                    value: 3.0
                },
            ]
        );
    }
    #[test]
    fn expression_evaluates_rate_then_groups() {
        let make = |instance: &str, values: [f64; 3]| PromSeries {
            labels: LabelSet::new([
                ("__name__".to_owned(), "requests_total".to_owned()),
                ("route".to_owned(), "/checkout".to_owned()),
                ("instance".to_owned(), instance.to_owned()),
            ]),
            samples: vec![
                FloatSample {
                    timestamp_ns: 10 * S,
                    value: values[0],
                },
                FloatSample {
                    timestamp_ns: 20 * S,
                    value: values[1],
                },
                FloatSample {
                    timestamp_ns: 30 * S,
                    value: values[2],
                },
            ],
        };
        let expr = PromExpr::Aggregate {
            op: PromAggregate::Sum,
            grouping: Grouping::By(vec!["route".into()]),
            input: Box::new(PromExpr::Rate {
                matchers: vec![LabelMatcher {
                    name: "__name__".into(),
                    op: MatchOp::Eq,
                    value: "requests_total".into(),
                }],
                window_ns: 30 * S as u64,
            }),
        };
        let out = eval_prom_reference(
            &expr,
            &[make("a", [0.0, 10.0, 20.0]), make("b", [0.0, 5.0, 10.0])],
            crate::EvalRange::instant(30 * S),
            crate::EvalLimits::default(),
        )
        .unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].labels.get("route"), Some("/checkout"));
        assert!((out[0].samples[0].value - 1.0).abs() < 1e-12);
    }
    #[test]
    fn fetch_plan_reads_only_the_required_rate_horizon() {
        let matcher = LabelMatcher {
            name: "__name__".into(),
            op: MatchOp::Eq,
            value: "requests_total".into(),
        };
        let expr = PromExpr::Aggregate {
            op: PromAggregate::Sum,
            grouping: Grouping::By(vec!["route".into()]),
            input: Box::new(PromExpr::Rate {
                matchers: vec![matcher.clone()],
                window_ns: 30,
            }),
        };
        let plan = plan_prom_fetch(
            &expr,
            crate::EvalRange {
                start_ns: 100,
                end_ns: 200,
                step_ns: 10,
                lookback_ns: 300,
            },
            crate::EvalLimits::default(),
        )
        .unwrap();
        assert_eq!(plan.requests.len(), 1);
        assert_eq!(
            plan.requests[0].bounds,
            crate::FetchBounds {
                start_ns: 70,
                end_ns: 200
            }
        );
        assert_eq!(plan.requests[0].matchers, vec![matcher]);
    }
    #[test]
    fn expression_uses_the_requested_lookback() {
        let series = PromSeries {
            labels: LabelSet::new([("__name__".to_owned(), "temperature".to_owned())]),
            samples: vec![FloatSample {
                timestamp_ns: 10,
                value: 42.0,
            }],
        };
        let expression = PromExpr::Selector {
            matchers: vec![LabelMatcher {
                name: "__name__".into(),
                op: MatchOp::Eq,
                value: "temperature".into(),
            }],
        };
        let evaluate = |lookback_ns| {
            eval_prom_reference(
                &expression,
                std::slice::from_ref(&series),
                crate::EvalRange {
                    start_ns: 20,
                    end_ns: 20,
                    step_ns: 1,
                    lookback_ns,
                },
                crate::EvalLimits::default(),
            )
            .unwrap()
        };
        // Lower bound is open (Prometheus semantics): a lookback of 10 puts the sample at
        // ts=10 exactly on the excluded edge (earliest = 20 - 10 = 10), so it is not selected;
        // a lookback of 11 (earliest = 9) includes it.
        assert!(evaluate(9).is_empty());
        assert!(evaluate(10).is_empty());
        assert_eq!(evaluate(11)[0].samples[0].value, 42.0);
    }
    #[test]
    fn classic_histogram_quantile_interpolates_and_repairs_monotonicity() {
        let buckets = [
            ClassicHistogramBucket {
                upper_bound: 1.0,
                cumulative_count: 2.0,
            },
            ClassicHistogramBucket {
                upper_bound: 2.0,
                cumulative_count: 4.0,
            },
            ClassicHistogramBucket {
                upper_bound: f64::INFINITY,
                cumulative_count: 4.0,
            },
        ];
        assert_eq!(
            classic_histogram_quantile(0.75, &buckets).unwrap().value,
            1.5
        );
        assert_eq!(
            classic_histogram_quantile(1.0, &buckets).unwrap().value,
            2.0
        );

        let repaired = classic_histogram_quantile(
            0.5,
            &[
                ClassicHistogramBucket {
                    upper_bound: 1.0,
                    cumulative_count: 2.0,
                },
                ClassicHistogramBucket {
                    upper_bound: 2.0,
                    cumulative_count: 1.0,
                },
                ClassicHistogramBucket {
                    upper_bound: f64::INFINITY,
                    cumulative_count: 3.0,
                },
            ],
        )
        .unwrap();
        assert_eq!(repaired.annotations.len(), 1);
        assert_eq!(
            repaired.annotations[0].code,
            "promql_histogram_monotonicity_repaired"
        );
    }

    #[test]
    fn classic_histogram_quantile_requires_positive_infinity_bucket() {
        let result = classic_histogram_quantile(
            0.5,
            &[
                ClassicHistogramBucket {
                    upper_bound: 1.0,
                    cumulative_count: 1.0,
                },
                ClassicHistogramBucket {
                    upper_bound: 2.0,
                    cumulative_count: 2.0,
                },
            ],
        )
        .unwrap();
        assert!(result.value.is_nan());
    }
    #[test]
    fn histogram_expression_rates_buckets_before_grouping_and_quantile() {
        let labels = LabelSet::new([
            ("__name__".to_owned(), "request_duration".to_owned()),
            ("route".to_owned(), "/checkout".to_owned()),
        ]);
        let bounds = [1.0, 2.0];
        let counts = [[1u64, 1, 0], [2, 2, 0], [3, 3, 0]];
        let histogram = PromHistogramSeries {
            labels,
            points: vec![
                HistogramPoint {
                    timestamp_ns: 10 * S,
                    explicit_bounds: &bounds,
                    bucket_counts: &counts[0],
                },
                HistogramPoint {
                    timestamp_ns: 20 * S,
                    explicit_bounds: &bounds,
                    bucket_counts: &counts[1],
                },
                HistogramPoint {
                    timestamp_ns: 30 * S,
                    explicit_bounds: &bounds,
                    bucket_counts: &counts[2],
                },
            ],
        };
        let expression = PromExpr::HistogramQuantile {
            phi: 0.75,
            matchers: vec![LabelMatcher {
                name: "__name__".into(),
                op: MatchOp::Eq,
                value: "request_duration".into(),
            }],
            window_ns: 30 * S as u64,
            grouping: Grouping::By(vec!["route".into()]),
        };
        let result = eval_prom_with_histograms_reference(
            &expression,
            &[],
            &[histogram],
            crate::EvalRange::instant(30 * S),
            crate::EvalLimits::default(),
        )
        .unwrap();
        assert_eq!(result[0].labels.get("route"), Some("/checkout"));
        assert!((result[0].samples[0].value - 1.5).abs() < 1e-12);
    }
}

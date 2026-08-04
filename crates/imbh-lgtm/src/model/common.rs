use std::borrow::Cow;
use std::collections::BTreeMap;
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SemanticProfile {
    pub capability_id: &'static str,
    pub upstream_product: &'static str,
    pub upstream_version: &'static str,
}

pub const PROMQL_PROFILE: SemanticProfile = SemanticProfile {
    capability_id: "imbh.promql.p1.v1",
    upstream_product: "Prometheus",
    upstream_version: "3.12.0",
};

pub const LOGQL_PROFILE: SemanticProfile = SemanticProfile {
    capability_id: "imbh.logql.l1.v1",
    upstream_product: "Loki",
    upstream_version: "3.7.2",
};

pub const TRACEQL_PROFILE: SemanticProfile = SemanticProfile {
    capability_id: "imbh.traceql.t1.v1",
    upstream_product: "Tempo",
    upstream_version: "2.10.5",
};

/// A sorted, unique series/stream label set, borrowing its strings from the query's Arrow backing
/// store for the duration `'a`.
///
/// Label values for *promoted* attribute keys are `Cow::Borrowed` slices straight out of the Arrow
/// dictionary buffers (zero-copy); values decoded from the canonical-JSON blob are `Cow::Owned` (JSON
/// escapes force a fresh buffer). Nothing is reference-counted: the backing arrays are already kept
/// alive by the `RecordBatch` the caller holds for the evaluation scope, so a plain borrow is sound
/// and cheaper than an `Arc`. Grouping works on the borrowed form (`BTreeMap<LabelSet, _>` keys,
/// `by`/`without` — the derived `Ord`/`Eq`/`Hash` compare **by content**, so a borrowed and an owned
/// set with equal strings are equal); the owned public outputs (`PromSeries`/`LogSeries`) call
/// [`LabelSet::into_owned`] once at the evaluation boundary to lift to `LabelSet<'static>`. See
/// `.agents/docs/LABELSET_ARROW_REFACTOR.md`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LabelSet<'a>(Box<[(Cow<'a, str>, Cow<'a, str>)]>);

impl Default for LabelSet<'_> {
    fn default() -> Self {
        Self(Box::from([]))
    }
}

impl<'a> LabelSet<'a> {
    pub fn new(
        labels: impl IntoIterator<Item = (impl Into<Cow<'a, str>>, impl Into<Cow<'a, str>>)>,
    ) -> Self {
        Self(
            labels
                .into_iter()
                .map(|(key, value)| (key.into(), value.into()))
                .collect::<BTreeMap<_, _>>()
                .into_iter()
                .collect(),
        )
    }

    pub fn get(&self, name: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(key, _)| key.as_ref() == name)
            .map(|(_, value)| value.as_ref())
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.0
            .iter()
            .map(|(key, value)| (key.as_ref(), value.as_ref()))
    }

    pub fn by(&self, names: &[String]) -> Self {
        Self(
            self.0
                .iter()
                .filter(|(key, _)| {
                    let key = key.as_ref();
                    key != "__name__" && names.iter().any(|n| n.as_str() == key)
                })
                .cloned()
                .collect(),
        )
    }

    pub fn without(&self, names: &[String]) -> Self {
        Self(
            self.0
                .iter()
                .filter(|(key, _)| {
                    let key = key.as_ref();
                    key != "__name__" && !names.iter().any(|n| n.as_str() == key)
                })
                .cloned()
                .collect(),
        )
    }

    /// Lift to an owned `LabelSet<'static>` by decoding every borrowed value, materializing once at
    /// the evaluation boundary so the returned series outlive the Arrow batches they were read from.
    pub fn into_owned(self) -> LabelSet<'static> {
        LabelSet(
            self.0
                .into_vec()
                .into_iter()
                .map(|(key, value)| (Cow::Owned(key.into_owned()), Cow::Owned(value.into_owned())))
                .collect(),
        )
    }
}

/// Inclusive storage bounds required to evaluate an expression.
///
/// A source may return rows exactly on either bound. The semantic kernel still applies the
/// language's precise open/closed selector boundaries. Keeping this contract inclusive makes it
/// safe to push into storage engines without accidentally dropping a boundary sample.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FetchBounds {
    pub start_ns: i64,
    pub end_ns: i64,
}

impl FetchBounds {
    pub fn new(start_ns: i64, end_ns: i64) -> Result<Self, SemanticError> {
        if end_ns < start_ns {
            return Err(SemanticError::InvalidRange);
        }
        Ok(Self { start_ns, end_ns })
    }

    pub fn contains(self, timestamp_ns: i64) -> bool {
        timestamp_ns >= self.start_ns && timestamp_ns <= self.end_ns
    }
}

/// Evaluation timestamps and selector defaults, all in unix nanoseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EvalRange {
    pub start_ns: i64,
    pub end_ns: i64,
    pub step_ns: u64,
    pub lookback_ns: u64,
}

impl EvalRange {
    pub fn instant(at_ns: i64) -> Self {
        Self {
            start_ns: at_ns,
            end_ns: at_ns,
            step_ns: 1,
            lookback_ns: 300_000_000_000,
        }
    }

    pub fn instants(self, limits: EvalLimits) -> Result<Vec<i64>, SemanticError> {
        if self.end_ns < self.start_ns || self.step_ns == 0 {
            return Err(SemanticError::InvalidRange);
        }
        let mut out = Vec::new();
        let mut at = self.start_ns;
        loop {
            if out.len() >= limits.max_evaluation_points {
                return Err(SemanticError::LimitExceeded("evaluation points"));
            }
            out.push(at);
            if at == self.end_ns {
                break;
            }
            let next = at.saturating_add(self.step_ns.min(i64::MAX as u64) as i64);
            if next <= at || next > self.end_ns {
                break;
            }
            at = next;
        }
        Ok(out)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EvalLimits {
    pub max_evaluation_points: usize,
    pub max_series: usize,
    pub max_samples: usize,
    pub max_spans: usize,
    pub max_traces: usize,
    pub max_recursion: usize,
}

impl Default for EvalLimits {
    fn default() -> Self {
        Self {
            max_evaluation_points: 11_000,
            max_series: 10_000,
            max_samples: 1_000_000,
            max_spans: 100_000,
            max_traces: 10_000,
            max_recursion: 128,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Annotation {
    pub code: &'static str,
    pub message: String,
}

/// Marked `#[non_exhaustive]` so a future failure mode that needs its own payload can be added
/// without another breaking change; match with a `_` arm.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum SemanticError {
    InvalidRange,
    LimitExceeded(&'static str),
    Incompatible(&'static str),
    Malformed(&'static str),
    Source(String),
    /// Two metric points share a series **and** a timestamp, which has no PromQL meaning.
    ///
    /// Its own variant, with an owned payload, because the other `Malformed` cases carry a
    /// `&'static str` and this one has to *name* the offending series: without the metric, the label
    /// set and the instant, the failure reads as "the query language is broken" and the natural next
    /// move — narrowing the time range — does not isolate it (issue #27). Build it with
    /// [`SemanticError::duplicate_timestamp`].
    DuplicateTimestamp(String),
}

impl SemanticError {
    /// Build a [`SemanticError::DuplicateTimestamp`] naming the series and the instant.
    ///
    /// `what` distinguishes the scalar and histogram cases. The rendered label set is capped, so a
    /// wide series cannot turn a diagnostic into a pathological response body.
    pub fn duplicate_timestamp(what: &str, labels: &LabelSet<'_>, timestamp_ns: i64) -> Self {
        SemanticError::DuplicateTimestamp(format!(
            "duplicate timestamps in one PromQL {what}: {} at {timestamp_ns}",
            RenderLabels(labels)
        ))
    }
}

/// Renders a label set in PromQL's own `{k="v", …}` syntax for diagnostics, truncating both the
/// number of labels and the length of any single value.
struct RenderLabels<'a, 'b>(&'a LabelSet<'b>);

impl fmt::Display for RenderLabels<'_, '_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        const MAX_LABELS: usize = 8;
        const MAX_VALUE: usize = 64;
        f.write_str("{")?;
        let total = self.0.iter().count();
        for (index, (key, value)) in self.0.iter().take(MAX_LABELS).enumerate() {
            if index > 0 {
                f.write_str(", ")?;
            }
            // Truncate on a char boundary; a label value is arbitrary UTF-8 from the producer.
            let cut = value
                .char_indices()
                .map(|(i, _)| i)
                .chain([value.len()])
                .take_while(|i| *i <= MAX_VALUE)
                .last()
                .unwrap_or(0);
            if cut < value.len() {
                write!(f, "{key}=\"{}…\"", &value[..cut])?;
            } else {
                write!(f, "{key}=\"{value}\"")?;
            }
        }
        if total > MAX_LABELS {
            write!(f, ", …{} more", total - MAX_LABELS)?;
        }
        f.write_str("}")
    }
}

impl fmt::Display for SemanticError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRange => f.write_str("invalid evaluation range"),
            Self::LimitExceeded(what) => write!(f, "{what} limit exceeded"),
            Self::Incompatible(what) => write!(f, "incompatible semantics: {what}"),
            Self::Malformed(what) => write!(f, "malformed semantic input: {what}"),
            Self::Source(what) => write!(f, "semantic data source error: {what}"),
            // Already a complete sentence naming the series; no prefix to add.
            Self::DuplicateTimestamp(what) => f.write_str(what),
        }
    }
}

impl std::error::Error for SemanticError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn label_sets_are_sorted_and_grouped_without_metric_name() {
        let labels = LabelSet::new([
            ("z".to_owned(), "2".to_owned()),
            ("__name__".to_owned(), "m".to_owned()),
            ("a".to_owned(), "1".to_owned()),
        ]);
        assert_eq!(
            labels.iter().collect::<Vec<_>>(),
            vec![("__name__", "m"), ("a", "1"), ("z", "2")]
        );
        assert_eq!(
            labels.by(&["z".to_owned()]).iter().collect::<Vec<_>>(),
            vec![("z", "2")]
        );
        assert_eq!(
            labels.without(&["z".to_owned()]).iter().collect::<Vec<_>>(),
            vec![("a", "1")]
        );
    }

    #[test]
    fn evaluation_instants_are_bounded_and_inclusive() {
        let r = EvalRange {
            start_ns: 10,
            end_ns: 30,
            step_ns: 10,
            lookback_ns: 5,
        };
        assert_eq!(r.instants(EvalLimits::default()).unwrap(), vec![10, 20, 30]);
    }
}

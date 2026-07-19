use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;

use crate::{EvalLimits, EvalRange, FloatSample, Grouping, LabelSet, SemanticError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogEntry<'a> {
    pub timestamp_ns: i64,
    pub line: &'a str,
    pub labels: LabelSet<'a>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum LogValue {
    Boolean(bool),
    Integer(i64),
    Float(f64),
    String(String),
    DurationNs(u64),
    Bytes(u64),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogPipelineError {
    pub stage: usize,
    pub kind: String,
    pub details: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LogPipelineState<'a> {
    pub current_line: String,
    pub stream_labels: LabelSet<'a>,
    pub extracted_labels: LabelSet<'a>,
    pub unwrapped_value: Option<LogValue>,
    pub error: Option<LogPipelineError>,
}

impl<'a> LogPipelineState<'a> {
    pub fn from_entry(entry: &LogEntry<'a>) -> Self {
        Self {
            current_line: entry.line.to_owned(),
            stream_labels: entry.labels.clone(),
            extracted_labels: LabelSet::default(),
            unwrapped_value: None,
            error: None,
        }
    }

    pub fn error_label(&self) -> &str {
        self.error.as_ref().map_or("", |error| error.kind.as_str())
    }

    pub fn set_error(&mut self, stage: usize, kind: impl Into<String>, details: impl Into<String>) {
        self.error = Some(LogPipelineError {
            stage,
            kind: kind.into(),
            details: details.into(),
        });
    }

    pub fn clear_error(&mut self) {
        self.error = None;
    }
}

/// Exact mapping from one Loki stream label to stored OTel data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LogLabelSource {
    Service,
    Attribute(String),
    ResourceAttribute(String),
}

/// Host-supplied definition of Loki stream identity.
///
/// Record and resource attributes are never promoted implicitly. Two entries belong to the same
/// stream exactly when all mapped label values are equal.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LogStreamSchema {
    pub labels: Vec<(String, LogLabelSource)>,
}

impl LogStreamSchema {
    pub fn service_only() -> Self {
        Self {
            labels: vec![("service".to_owned(), LogLabelSource::Service)],
        }
    }

    pub fn source(&self, label: &str) -> Option<&LogLabelSource> {
        self.labels
            .iter()
            .find(|(name, _)| name == label)
            .map(|(_, source)| source)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LogFilter {
    All,
    LabelEq(String, String),
    LabelNe(String, String),
    LabelRegex(String, String),
    LabelNotRegex(String, String),
    LineContains(String),
    LineNotContains(String),
    LineRegex(String),
    LineNotRegex(String),
    /// imbh LogQL dialect: `|?` — tokenized term-AND full-text match over the line (the
    /// Tantivy-accelerated `matches()` semantics), distinct from the substring `LineContains` (`|=`).
    LineMatches(String),
    /// imbh LogQL dialect: `!?` — the negation of [`LineMatches`](Self::LineMatches).
    LineNotMatches(String),
    And(Vec<LogFilter>),
}

impl LogFilter {
    fn matches(&self, entry: &LogEntry<'_>) -> Result<bool, SemanticError> {
        Ok(match self {
            Self::All => true,
            Self::LabelEq(name, value) => entry.labels.get(name).unwrap_or("") == value,
            Self::LabelNe(name, value) => entry.labels.get(name).unwrap_or("") != value,
            Self::LabelRegex(name, pattern) | Self::LabelNotRegex(name, pattern) => {
                let pattern = format!("^(?:{pattern})$");
                let matched = regex::Regex::new(&pattern)
                    .map_err(|_| SemanticError::Malformed("invalid LogQL label regex"))?
                    .is_match(entry.labels.get(name).unwrap_or(""));
                if matches!(self, Self::LabelRegex(_, _)) {
                    matched
                } else {
                    !matched
                }
            }
            Self::LineContains(needle) => entry.line.contains(needle),
            Self::LineNotContains(needle) => !entry.line.contains(needle),
            Self::LineMatches(query) => imbh_core::matches_terms(entry.line, query),
            Self::LineNotMatches(query) => !imbh_core::matches_terms(entry.line, query),
            Self::LineRegex(pattern) | Self::LineNotRegex(pattern) => {
                let matched = regex::Regex::new(pattern)
                    .map_err(|_| SemanticError::Malformed("invalid LogQL line regex"))?
                    .is_match(entry.line);
                if matches!(self, Self::LineRegex(_)) {
                    matched
                } else {
                    !matched
                }
            }
            Self::And(filters) => {
                for filter in filters {
                    if !filter.matches(entry)? {
                        return Ok(false);
                    }
                }
                true
            }
        })
    }

    fn validate(&self) -> Result<(), SemanticError> {
        match self {
            Self::LabelRegex(_, pattern) | Self::LabelNotRegex(_, pattern) => {
                let pattern = format!("^(?:{pattern})$");
                regex::Regex::new(&pattern)
                    .map_err(|_| SemanticError::Malformed("invalid LogQL label regex"))?;
            }
            Self::LineRegex(pattern) | Self::LineNotRegex(pattern) => {
                regex::Regex::new(pattern)
                    .map_err(|_| SemanticError::Malformed("invalid LogQL line regex"))?;
            }
            Self::And(filters) => {
                for filter in filters {
                    filter.validate()?;
                }
            }
            _ => {}
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogRangeOp {
    CountOverTime,
    Rate,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogRangeExpr {
    pub filter: LogFilter,
    pub window_ns: u64,
    pub offset_ns: u64,
    pub op: LogRangeOp,
    pub grouping: Option<Grouping>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LogSeries<'a> {
    pub labels: LabelSet<'a>,
    pub samples: Vec<FloatSample>,
}

/// One bounded storage read needed by a LogQL range expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogFetchRequest {
    pub bounds: crate::FetchBounds,
    pub filter: LogFilter,
    pub max_entries: usize,
}

type LogEntryDependent<'a> = Vec<LogEntry<'a>>;

self_cell::self_cell! {
    /// Normalized log entries whose stream labels *borrow* from an owned, type-erased backing store
    /// (the source's materialized rows). Mirrors [`crate::PromSeriesPack`]: the evaluator reads
    /// [`LogEntryPack::borrow_dependent`] and materializes owned labels only at the public boundary.
    pub struct LogEntryPack {
        owner: Box<dyn std::any::Any + Send>,
        #[covariant]
        dependent: LogEntryDependent,
    }
}

/// A bounded source of normalized log entries. Entries are returned inside a self-owning
/// [`LogEntryPack`] so their stream labels can borrow the source's backing store.
pub trait LogEntrySource {
    fn fetch<'a>(
        &'a self,
        request: &'a LogFetchRequest,
    ) -> Pin<Box<dyn Future<Output = Result<LogEntryPack, SemanticError>> + Send + 'a>>;
}

/// Derive the storage interval for a LogQL sliding-window evaluation.
pub fn plan_log_fetch(
    expr: &LogRangeExpr,
    range: EvalRange,
    limits: EvalLimits,
) -> Result<LogFetchRequest, SemanticError> {
    if expr.window_ns == 0 {
        return Err(SemanticError::InvalidRange);
    }
    expr.filter.validate()?;
    range.instants(limits)?;
    let offset = expr.offset_ns.min(i64::MAX as u64) as i64;
    let window = expr.window_ns.min(i64::MAX as u64) as i64;
    let end_ns = range.end_ns.saturating_sub(offset);
    let start_ns = range.start_ns.saturating_sub(offset).saturating_sub(window);
    Ok(LogFetchRequest {
        bounds: crate::FetchBounds::new(start_ns, end_ns)?,
        filter: expr.filter.clone(),
        max_entries: limits.max_samples,
    })
}

/// Plan a bounded read, fetch it from storage, and evaluate the resulting working set.
pub async fn execute_log_range<S: LogEntrySource + ?Sized>(
    source: &S,
    expr: &LogRangeExpr,
    range: EvalRange,
    limits: EvalLimits,
) -> Result<Vec<LogSeries<'static>>, SemanticError> {
    let request = plan_log_fetch(expr, range, limits)?;
    // Hold the pack for the whole evaluation: entry labels borrow its backing store. Owned labels
    // are materialized once, at the return boundary.
    let pack = source.fetch(&request).await?;
    let entries = pack.borrow_dependent();
    if entries.len() > request.max_entries {
        return Err(SemanticError::LimitExceeded("LogQL source entries"));
    }
    if entries
        .iter()
        .any(|entry| !request.bounds.contains(entry.timestamp_ns))
    {
        return Err(SemanticError::Malformed(
            "LogQL source returned an entry outside requested bounds",
        ));
    }
    let series = eval_log_range_reference(entries, expr, range, limits)?;
    Ok(series
        .into_iter()
        .map(|series| LogSeries {
            labels: series.labels.into_owned(),
            samples: series.samples,
        })
        .collect())
}

/// This is the fixture/reference kernel. Production callers should use [`execute_log_range`],
/// which plans and enforces a bounded storage read.
/// Evaluate a LogQL log-range function at every requested evaluation timestamp.
///
/// Windows are left-open/right-closed. Offset shifts the selected window but not result timestamps.
/// Series with no entries in a window emit no sample.
pub fn eval_log_range_reference<'a>(
    entries: &[LogEntry<'a>],
    expr: &LogRangeExpr,
    range: EvalRange,
    limits: EvalLimits,
) -> Result<Vec<LogSeries<'a>>, SemanticError> {
    if expr.window_ns == 0 {
        return Err(SemanticError::InvalidRange);
    }
    if entries.len() > limits.max_samples {
        return Err(SemanticError::LimitExceeded("log samples"));
    }
    let mut result: BTreeMap<LabelSet<'a>, Vec<FloatSample>> = BTreeMap::new();
    let mut output_samples = 0usize;
    for at in range.instants(limits)? {
        let selection_end = at.saturating_sub(expr.offset_ns.min(i64::MAX as u64) as i64);
        let selection_start =
            selection_end.saturating_sub(expr.window_ns.min(i64::MAX as u64) as i64);
        let mut counts: BTreeMap<LabelSet<'a>, u64> = BTreeMap::new();
        for entry in entries {
            if entry.timestamp_ns > selection_start
                && entry.timestamp_ns <= selection_end
                && expr.filter.matches(entry)?
            {
                *counts.entry(entry.labels.clone()).or_default() += 1;
            }
        }

        let mut evaluated: BTreeMap<LabelSet<'a>, f64> = BTreeMap::new();
        for (labels, count) in counts {
            let labels = match &expr.grouping {
                Some(Grouping::By(names)) => labels.by(names),
                Some(Grouping::Without(names)) => labels.without(names),
                None => labels,
            };
            let value = match expr.op {
                LogRangeOp::CountOverTime => count as f64,
                LogRangeOp::Rate => count as f64 / (expr.window_ns as f64 / 1_000_000_000.0),
            };
            *evaluated.entry(labels).or_default() += value;
        }

        for (labels, value) in evaluated {
            if result.len() >= limits.max_series && !result.contains_key(&labels) {
                return Err(SemanticError::LimitExceeded("log series"));
            }
            output_samples = output_samples
                .checked_add(1)
                .ok_or(SemanticError::LimitExceeded("LogQL output samples"))?;
            if output_samples > limits.max_samples {
                return Err(SemanticError::LimitExceeded("LogQL output samples"));
            }
            result.entry(labels).or_default().push(FloatSample {
                timestamp_ns: at,
                value,
            });
        }
    }
    Ok(result
        .into_iter()
        .map(|(labels, samples)| LogSeries { labels, samples })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn expr(filter: LogFilter, window_ns: u64, op: LogRangeOp) -> LogRangeExpr {
        LogRangeExpr {
            filter,
            window_ns,
            offset_ns: 0,
            op,
            grouping: None,
        }
    }

    #[test]
    fn range_window_is_distinct_from_evaluation_step() {
        let labels = LabelSet::new([("app".to_owned(), "api".to_owned())]);
        let entries = vec![
            LogEntry {
                timestamp_ns: 6,
                line: "ok",
                labels: labels.clone(),
            },
            LogEntry {
                timestamp_ns: 9,
                line: "error",
                labels: labels.clone(),
            },
            LogEntry {
                timestamp_ns: 14,
                line: "error",
                labels,
            },
        ];
        let out = eval_log_range_reference(
            &entries,
            &expr(
                LogFilter::LineContains("error".into()),
                10,
                LogRangeOp::CountOverTime,
            ),
            EvalRange {
                start_ns: 10,
                end_ns: 20,
                step_ns: 10,
                lookback_ns: 0,
            },
            EvalLimits::default(),
        )
        .unwrap();
        assert_eq!(
            out[0]
                .samples
                .iter()
                .map(|sample| sample.value)
                .collect::<Vec<_>>(),
            vec![1.0, 1.0]
        );
    }

    #[test]
    fn windows_are_left_open_right_closed_and_never_reach_past_the_range_end() {
        // Pins the sliding-window boundary. Range [10,20], step 10, window 10 → instants {10,20}
        // with windows (0,10] and (10,20]. A sample at `end+1` (=21) sits in [end, end+window] but
        // no backward window reaches it, so it is never counted — the storage fetch correctly does
        // not need data past the range end. A sample at the open left edge (=0) is likewise excluded.
        let labels = LabelSet::new([("app".to_owned(), "api".to_owned())]);
        let entry = |ts| LogEntry {
            timestamp_ns: ts,
            line: "x",
            labels: labels.clone(),
        };
        let entries = vec![entry(0), entry(10), entry(20), entry(21)];
        let out = eval_log_range_reference(
            &entries,
            &expr(LogFilter::All, 10, LogRangeOp::CountOverTime),
            EvalRange {
                start_ns: 10,
                end_ns: 20,
                step_ns: 10,
                lookback_ns: 0,
            },
            EvalLimits::default(),
        )
        .unwrap();
        // instant 10 counts {10} (0 is open-excluded); instant 20 counts {20} (10 is open-excluded,
        // 21 is past-end). Neither 0 nor 21 ever contributes.
        let samples: Vec<_> = out[0]
            .samples
            .iter()
            .map(|s| (s.timestamp_ns, s.value))
            .collect();
        assert_eq!(samples, vec![(10, 1.0), (20, 1.0)]);
    }

    #[test]
    fn empty_windows_are_absent_not_zero() {
        let out = eval_log_range_reference(
            &[],
            &expr(LogFilter::All, 10, LogRangeOp::Rate),
            EvalRange::instant(20),
            EvalLimits::default(),
        )
        .unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn stream_regex_is_anchored_but_line_regex_is_not() {
        let entry = LogEntry {
            timestamp_ns: 10,
            line: "prefix api suffix",
            labels: LabelSet::new([("app".to_owned(), "api-server".to_owned())]),
        };
        assert!(
            !LogFilter::LabelRegex("app".into(), "api".into())
                .matches(&entry)
                .unwrap()
        );
        assert!(LogFilter::LineRegex("api".into()).matches(&entry).unwrap());
    }

    #[test]
    fn line_matches_is_tokenized_term_and_not_substring() {
        let entry = LogEntry {
            timestamp_ns: 10,
            line: "connection error timeout",
            labels: LabelSet::default(),
        };
        // `|?` is term-AND: every query token must appear as a token of the line, order-free.
        assert!(
            LogFilter::LineMatches("timeout error".into())
                .matches(&entry)
                .unwrap()
        );
        // A partial token is NOT a term match (unlike the substring `|=`).
        assert!(
            !LogFilter::LineMatches("err".into())
                .matches(&entry)
                .unwrap()
        );
        assert!(
            LogFilter::LineContains("err".into())
                .matches(&entry)
                .unwrap()
        );
        // `!?` is the negation of `|?`.
        assert!(
            LogFilter::LineNotMatches("refused".into())
                .matches(&entry)
                .unwrap()
        );
        assert!(
            !LogFilter::LineNotMatches("error".into())
                .matches(&entry)
                .unwrap()
        );
    }

    #[test]
    fn offset_moves_window_and_grouping_sums_streams() {
        let entries = vec![
            LogEntry {
                timestamp_ns: 5,
                line: "x",
                labels: LabelSet::new([
                    ("app".to_owned(), "api".to_owned()),
                    ("instance".to_owned(), "a".to_owned()),
                ]),
            },
            LogEntry {
                timestamp_ns: 6,
                line: "x",
                labels: LabelSet::new([
                    ("app".to_owned(), "api".to_owned()),
                    ("instance".to_owned(), "b".to_owned()),
                ]),
            },
            LogEntry {
                timestamp_ns: 15,
                line: "x",
                labels: LabelSet::new([("app".to_owned(), "api".to_owned())]),
            },
        ];
        let out = eval_log_range_reference(
            &entries,
            &LogRangeExpr {
                filter: LogFilter::All,
                window_ns: 10,
                offset_ns: 9,
                op: LogRangeOp::CountOverTime,
                grouping: Some(Grouping::By(vec!["app".into()])),
            },
            EvalRange::instant(15),
            EvalLimits::default(),
        )
        .unwrap();
        assert_eq!(out[0].labels.get("app"), Some("api"));
        assert_eq!(out[0].samples[0].value, 2.0);
        assert_eq!(out[0].samples[0].timestamp_ns, 15);
    }
    #[test]
    fn fetch_plan_accounts_for_window_and_offset() {
        let expression = LogRangeExpr {
            filter: LogFilter::LineContains("error".into()),
            window_ns: 20,
            offset_ns: 5,
            op: LogRangeOp::CountOverTime,
            grouping: None,
        };
        let request = plan_log_fetch(
            &expression,
            EvalRange {
                start_ns: 100,
                end_ns: 200,
                step_ns: 10,
                lookback_ns: 0,
            },
            EvalLimits::default(),
        )
        .unwrap();
        assert_eq!(
            request.bounds,
            crate::FetchBounds {
                start_ns: 75,
                end_ns: 195
            }
        );
        assert_eq!(request.max_entries, EvalLimits::default().max_samples);
    }
}

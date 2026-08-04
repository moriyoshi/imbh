use std::borrow::Cow;
use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use imbh::arrow::array::{
    Array, BooleanArray, DictionaryArray, Float64Array, Int64Array, ListArray, StringArray,
    StringViewArray, TimestampNanosecondArray, UInt64Array,
};
use imbh::arrow::datatypes::Int32Type;
use imbh::arrow::record_batch::RecordBatch;
use imbh::{
    AnyValue, Attributes, Direction, Duplicates, LogQuery, LogStringField, LogsApi,
    MetricPointsQuery, MetricsApi, StringPredicate, Timestamp, TraceQuery, TracesApi,
};

use crate::{
    EvalLimits, EvalRange, FloatSample, HistogramPoint, LabelMatcher, LabelSet,
    LogEntry as SemanticLogEntry, LogEntryPack, LogEntrySource, LogFetchRequest, LogFilter,
    LogLabelSource, LogRangeExpr, LogSeries, LogStreamSchema, MatchOp, PromExpr, PromFetchPurpose,
    PromFetchRequest, PromHistogramPack, PromHistogramSeries, PromSeries, PromSeriesPack,
    PromSeriesSource, SemanticError, SemanticSpan, SemanticTrace, SemanticValue,
    SpanCandidateFilter, SpansetExpr, TraceFetchRequest, TracePack, TraceQueryMatch, TraceSource,
    TypedAttributes,
};

/// Build bounded native metric point queries for one semantic fetch request.
pub fn build_metric_point_queries(
    request: &PromFetchRequest,
) -> Result<Vec<MetricPointsQuery>, SemanticError> {
    let metric = exact_metric_name(&request.matchers)?;
    let constructors: &[fn(String) -> MetricPointsQuery] = match request.purpose {
        PromFetchPurpose::InstantSelector => &[MetricPointsQuery::gauge, MetricPointsQuery::sum],
        PromFetchPurpose::CumulativeCounterRate => &[MetricPointsQuery::sum],
        PromFetchPurpose::CumulativeHistogramRate => &[MetricPointsQuery::histogram],
    };
    constructors
        .iter()
        .map(|constructor| {
            let mut query = constructor(metric.to_owned())
                .range_inclusive(
                    Timestamp(request.bounds.start_ns),
                    Timestamp(request.bounds.end_ns),
                )
                .limit(request.max_samples.saturating_add(1));
            for matcher in &request.matchers {
                if matcher.name == "__name__" {
                    if !metric_name_matches(metric, matcher)? {
                        query = query.match_none();
                    }
                    continue;
                }
                query = match matcher.op {
                    MatchOp::Eq => query.filter(&matcher.name, &matcher.value),
                    MatchOp::Ne => query.filter_ne(&matcher.name, &matcher.value),
                    MatchOp::Regex => {
                        query.filter_regex(&matcher.name, format!("^(?:{})$", matcher.value))
                    }
                    MatchOp::NotRegex => {
                        query.filter_not_regex(&matcher.name, format!("^(?:{})$", matcher.value))
                    }
                };
            }
            Ok(query)
        })
        .collect()
}

/// Build one bounded native log query for a semantic fetch request.
pub fn build_log_query(
    request: &LogFetchRequest,
    schema: &LogStreamSchema,
) -> Result<LogQuery, SemanticError> {
    validate_log_schema(schema)?;
    let query = LogQuery::new()
        .range_inclusive(
            Timestamp(request.bounds.start_ns),
            Timestamp(request.bounds.end_ns),
        )
        .direction(Direction::Forward)
        .limit(request.max_entries.saturating_add(1));
    apply_log_filter(query, &request.filter, schema)
}

/// Build the native complete-trace candidate query for a semantic fetch request, translating the
/// request's *necessary* candidate filter (a sound superset predicate) into span filters. These
/// narrow the candidate set in storage; because they are necessary conditions, no matching trace is
/// dropped (the evaluator still re-checks every candidate against the full spanset expression).
pub fn build_trace_query(request: &TraceFetchRequest) -> TraceQuery {
    let mut query = TraceQuery::new()
        .trace_start_range_inclusive(
            Timestamp(request.bounds.start_ns),
            Timestamp(request.bounds.end_ns),
        )
        .limit(request.max_traces.saturating_add(1));
    for filter in &request.candidate {
        query = match filter {
            SpanCandidateFilter::Name(name) => query.name(name),
            SpanCandidateFilter::Status(status) => query.status(status),
            SpanCandidateFilter::Kind(kind) => query.kind(kind),
            SpanCandidateFilter::DurationGe(ns) => query.min_duration(Duration::from_nanos(*ns)),
            SpanCandidateFilter::DurationLe(ns) => query.max_duration(Duration::from_nanos(*ns)),
            SpanCandidateFilter::AttrEq(key, value) => query.attr_eq(key, value),
            // The bound is within f64's exact integer range (guaranteed by `push_numeric_attr`), so
            // the widening is lossless. Storage reads the attribute via `json_get_num`, matching
            // integer/double-typed JSON attributes.
            SpanCandidateFilter::AttrNumGt(key, n) => query.attr_gt(key, *n as f64),
            SpanCandidateFilter::AttrNumGe(key, n) => query.attr_ge(key, *n as f64),
            SpanCandidateFilter::AttrNumLt(key, n) => query.attr_lt(key, *n as f64),
            SpanCandidateFilter::AttrNumLe(key, n) => query.attr_le(key, *n as f64),
        };
    }
    query
}

pub trait MetricsSemanticsExt {
    fn execute_promql<'a>(
        &'a self,
        expression: &'a PromExpr,
        range: EvalRange,
        limits: EvalLimits,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<PromSeries<'static>>, SemanticError>> + Send + 'a>>;

    /// Arrow-native twin of [`execute_promql`](MetricsSemanticsExt::execute_promql): the same
    /// PromQL evaluation, returned as a long-form matrix `RecordBatch`
    /// (`{ labels, ts, value }`, one row per sample) instead of `Vec<PromSeries>`.
    fn execute_promql_batches<'a>(
        &'a self,
        expression: &'a PromExpr,
        range: EvalRange,
        limits: EvalLimits,
    ) -> Pin<Box<dyn Future<Output = Result<RecordBatch, SemanticError>> + Send + 'a>>;
}

impl MetricsSemanticsExt for MetricsApi {
    fn execute_promql<'a>(
        &'a self,
        expression: &'a PromExpr,
        range: EvalRange,
        limits: EvalLimits,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<PromSeries<'static>>, SemanticError>> + Send + 'a>>
    {
        // Duplicate timestamps are resolved per the database's own policy (issue #27), not per query.
        Box::pin(crate::execute_prom_with_duplicates(
            self,
            expression,
            range,
            limits,
            self.duplicates(),
        ))
    }

    fn execute_promql_batches<'a>(
        &'a self,
        expression: &'a PromExpr,
        range: EvalRange,
        limits: EvalLimits,
    ) -> Pin<Box<dyn Future<Output = Result<RecordBatch, SemanticError>> + Send + 'a>> {
        Box::pin(async move {
            let series = crate::execute_prom_with_duplicates(
                self,
                expression,
                range,
                limits,
                self.duplicates(),
            )
            .await?;
            Ok(crate::batch::prom_series_to_batch(&series))
        })
    }
}

impl PromSeriesSource for MetricsApi {
    fn fetch<'a>(
        &'a self,
        request: &'a PromFetchRequest,
    ) -> Pin<Box<dyn Future<Output = Result<PromSeriesPack, SemanticError>> + Send + 'a>> {
        let duplicates = self.duplicates();
        Box::pin(async move {
            // Level-2 read: raw Arrow (no `MetricPoint` materialization). The pack owns the batches;
            // grouped series' labels borrow their buffers, and histogram bucket lists are borrowed
            // slices of the scan's `ListArray` values.
            let mut batches = Vec::new();
            let mut total = 0usize;
            for query in build_metric_point_queries(request)? {
                let fetched = self
                    .points_batches(query)
                    .await
                    .map_err(|error| SemanticError::Source(error.to_string()))?;
                total += fetched.iter().map(RecordBatch::num_rows).sum::<usize>();
                if total > request.max_samples {
                    return Err(SemanticError::LimitExceeded("PromQL source samples"));
                }
                batches.extend(fetched);
            }
            PromSeriesPack::try_new(Box::new(batches), |owner| {
                let batches = owner
                    .downcast_ref::<Vec<RecordBatch>>()
                    .expect("PromSeriesPack owner is Vec<RecordBatch>");
                scalar_series(batches, request, duplicates)
            })
        })
    }

    fn fetch_histograms<'a>(
        &'a self,
        request: &'a PromFetchRequest,
    ) -> Pin<Box<dyn Future<Output = Result<PromHistogramPack, SemanticError>> + Send + 'a>> {
        let duplicates = self.duplicates();
        Box::pin(async move {
            let query = build_metric_point_queries(request)?
                .into_iter()
                .next()
                .ok_or(SemanticError::Malformed("missing histogram point query"))?;
            let batches = self
                .points_batches(query)
                .await
                .map_err(|error| SemanticError::Source(error.to_string()))?;
            let total: usize = batches.iter().map(RecordBatch::num_rows).sum();
            if total > request.max_samples {
                return Err(SemanticError::LimitExceeded(
                    "PromQL source histogram points",
                ));
            }
            PromHistogramPack::try_new(Box::new(batches), |owner| {
                let batches = owner
                    .downcast_ref::<Vec<RecordBatch>>()
                    .expect("PromHistogramPack owner is Vec<RecordBatch>");
                histogram_series(batches, request, duplicates)
            })
        })
    }
}

/// The `point_time` column (col 0, `CAST("time" AS BIGINT)`) of a `points_batches` batch.
fn point_times(batch: &RecordBatch) -> Result<&Int64Array, SemanticError> {
    batch
        .column(0)
        .as_any()
        .downcast_ref::<Int64Array>()
        .ok_or(SemanticError::Malformed("metric point_time is not Int64"))
}

/// Whether the row's `temporality` (col 4) is cumulative.
fn is_cumulative(batch: &RecordBatch, row: usize) -> bool {
    col_str(batch.column(4).as_ref(), row)
        .is_some_and(|value| value.eq_ignore_ascii_case("cumulative"))
}

/// One `List` row as a borrowed slice of the child values buffer (zero-copy).
fn list_f64_slice(list: &ListArray, row: usize) -> Result<&[f64], SemanticError> {
    if list.is_null(row) {
        return Ok(&[]);
    }
    let values = list
        .values()
        .as_any()
        .downcast_ref::<Float64Array>()
        .ok_or(SemanticError::Malformed("histogram bounds are not Float64"))?;
    let offsets = list.value_offsets();
    Ok(&values.values()[offsets[row] as usize..offsets[row + 1] as usize])
}

fn list_u64_slice(list: &ListArray, row: usize) -> Result<&[u64], SemanticError> {
    if list.is_null(row) {
        return Ok(&[]);
    }
    let values = list
        .values()
        .as_any()
        .downcast_ref::<UInt64Array>()
        .ok_or(SemanticError::Malformed("histogram counts are not UInt64"))?;
    let offsets = list.value_offsets();
    Ok(&values.values()[offsets[row] as usize..offsets[row + 1] as usize])
}

fn scalar_series<'a>(
    batches: &'a [RecordBatch],
    request: &PromFetchRequest,
    duplicates: Duplicates,
) -> Result<Vec<PromSeries<'a>>, SemanticError> {
    // Group with labels borrowed from the batch buffers: the N input rows collapse to M series with
    // no owned label set per row. Labels stay borrowed in the returned pack (whose owner is the
    // batches); the evaluator materializes owned labels only at its public boundary.
    let mut by_labels: BTreeMap<LabelSet<'a>, Vec<FloatSample>> = BTreeMap::new();
    for batch in batches {
        let times = point_times(batch)?;
        let values = batch
            .column(6)
            .as_any()
            .downcast_ref::<Float64Array>()
            .ok_or(SemanticError::Malformed(
                "scalar value column is not Float64",
            ))?;
        let monotonic = batch.column(5).as_any().downcast_ref::<BooleanArray>();
        for row in 0..batch.num_rows() {
            if request.purpose == PromFetchPurpose::CumulativeCounterRate {
                let mono = monotonic.and_then(|m| (!m.is_null(row)).then(|| m.value(row)));
                if !is_cumulative(batch, row) || mono != Some(true) {
                    return Err(SemanticError::Incompatible(
                        "rate requires a cumulative monotonic sum",
                    ));
                }
            }
            let labels = metric_labels_from_batch(batch, row);
            if !by_labels.contains_key(&labels) && by_labels.len() >= request.max_series {
                return Err(SemanticError::LimitExceeded("PromQL source series"));
            }
            by_labels.entry(labels).or_default().push(FloatSample {
                timestamp_ns: times.value(row),
                value: values.value(row),
            });
        }
    }
    Ok(by_labels
        .into_iter()
        .map(|(labels, mut samples)| {
            // `execute_prom` resolves duplicates again over the merged working set (they can also
            // arise *between* packs), but a `PromSeriesPack` is public and read directly by Level-2
            // callers, so it must not hand out a series that violates the invariant either. Under
            // the erroring policies this is just the sort it always was; the error is raised once,
            // in `execute_prom`, where the whole series is in hand.
            if duplicates.collapses_at_read() {
                crate::collapse_duplicate_samples(&mut samples);
            } else {
                samples.sort_by_key(|sample| sample.timestamp_ns);
            }
            PromSeries { labels, samples }
        })
        .collect())
}

fn histogram_series<'a>(
    batches: &'a [RecordBatch],
    request: &PromFetchRequest,
    duplicates: Duplicates,
) -> Result<Vec<PromHistogramSeries<'a>>, SemanticError> {
    let mut by_labels: BTreeMap<LabelSet<'a>, Vec<HistogramPoint<'a>>> = BTreeMap::new();
    for batch in batches {
        let times = point_times(batch)?;
        let bounds = batch.column(6).as_any().downcast_ref::<ListArray>().ok_or(
            SemanticError::Malformed("explicit_bounds column is not List"),
        )?;
        let counts = batch
            .column(7)
            .as_any()
            .downcast_ref::<ListArray>()
            .ok_or(SemanticError::Malformed("bucket_counts column is not List"))?;
        for row in 0..batch.num_rows() {
            if !is_cumulative(batch, row) {
                return Err(SemanticError::Incompatible(
                    "histogram_quantile rate requires cumulative histograms",
                ));
            }
            let labels = metric_labels_from_batch(batch, row);
            if !by_labels.contains_key(&labels) && by_labels.len() >= request.max_series {
                return Err(SemanticError::LimitExceeded(
                    "PromQL source histogram series",
                ));
            }
            by_labels.entry(labels).or_default().push(HistogramPoint {
                timestamp_ns: times.value(row),
                explicit_bounds: list_f64_slice(bounds, row)?,
                bucket_counts: list_u64_slice(counts, row)?,
            });
        }
    }
    Ok(by_labels
        .into_iter()
        .map(|(labels, mut points)| {
            // See `scalar_series`: the pack keeps the same invariant the working set does.
            if duplicates.collapses_at_read() {
                crate::collapse_duplicate_histogram_points(&mut points);
            } else {
                points.sort_by_key(|point| point.timestamp_ns);
            }
            PromHistogramSeries { labels, points }
        })
        .collect())
}

fn exact_metric_name(matchers: &[LabelMatcher]) -> Result<&str, SemanticError> {
    matchers
        .iter()
        .find_map(|matcher| {
            (matcher.name == "__name__" && matcher.op == MatchOp::Eq)
                .then_some(matcher.value.as_str())
        })
        .filter(|metric| !metric.is_empty())
        .ok_or(SemanticError::Incompatible(
            "IMBH source requires an exact non-empty metric name",
        ))
}

fn metric_name_matches(metric: &str, matcher: &LabelMatcher) -> Result<bool, SemanticError> {
    Ok(match matcher.op {
        MatchOp::Eq => metric == matcher.value,
        MatchOp::Ne => metric != matcher.value,
        MatchOp::Regex | MatchOp::NotRegex => {
            let matched = regex::Regex::new(&format!("^(?:{})$", matcher.value))
                .map_err(|_| SemanticError::Malformed("invalid PromQL metric-name regex"))?
                .is_match(metric);
            if matcher.op == MatchOp::Regex {
                matched
            } else {
                !matched
            }
        }
    })
}
struct ImbhLogSource<'a> {
    api: &'a LogsApi,
    schema: &'a LogStreamSchema,
}

pub trait LogsSemanticsExt {
    fn execute_logql<'a>(
        &'a self,
        expression: &'a LogRangeExpr,
        range: EvalRange,
        limits: EvalLimits,
        schema: &'a LogStreamSchema,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<LogSeries<'static>>, SemanticError>> + Send + 'a>>;

    /// Arrow-native twin of [`execute_logql`](LogsSemanticsExt::execute_logql): the same LogQL
    /// metric evaluation, returned as a long-form matrix `RecordBatch` (`{ labels, ts, value }`,
    /// one row per sample) instead of `Vec<LogSeries>`.
    fn execute_logql_batches<'a>(
        &'a self,
        expression: &'a LogRangeExpr,
        range: EvalRange,
        limits: EvalLimits,
        schema: &'a LogStreamSchema,
    ) -> Pin<Box<dyn Future<Output = Result<RecordBatch, SemanticError>> + Send + 'a>>;
}

impl LogsSemanticsExt for LogsApi {
    fn execute_logql<'a>(
        &'a self,
        expression: &'a LogRangeExpr,
        range: EvalRange,
        limits: EvalLimits,
        schema: &'a LogStreamSchema,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<LogSeries<'static>>, SemanticError>> + Send + 'a>>
    {
        Box::pin(async move {
            validate_log_schema(schema)?;
            let source = ImbhLogSource { api: self, schema };
            crate::execute_log_range(&source, expression, range, limits).await
        })
    }

    fn execute_logql_batches<'a>(
        &'a self,
        expression: &'a LogRangeExpr,
        range: EvalRange,
        limits: EvalLimits,
        schema: &'a LogStreamSchema,
    ) -> Pin<Box<dyn Future<Output = Result<RecordBatch, SemanticError>> + Send + 'a>> {
        Box::pin(async move {
            let series = self
                .execute_logql(expression, range, limits, schema)
                .await?;
            Ok(crate::batch::log_series_to_batch(&series))
        })
    }
}

impl LogEntrySource for ImbhLogSource<'_> {
    fn fetch<'a>(
        &'a self,
        request: &'a LogFetchRequest,
    ) -> Pin<Box<dyn Future<Output = Result<LogEntryPack, SemanticError>> + Send + 'a>> {
        Box::pin(async move {
            // Level-2 read: fetch raw Arrow (no `LogEntry` materialization) and read `time`, `body`,
            // and the stream labels straight from the batch buffers via `StreamLabelReader`. This
            // skips the per-row JSON parse of every attribute/resource/scope blob that the DTO path
            // pays, and reads promoted labels zero-copy from their dictionary columns
            // (`.agents/docs/LABELSET_ARROW_REFACTOR.md`; benchmarked in `examples/logql_level2.rs`).
            let batches = self
                .api
                .query_batches(build_log_query(request, self.schema)?)
                .await
                .map_err(|error| SemanticError::Source(error.to_string()))?;
            let count: usize = batches.iter().map(RecordBatch::num_rows).sum();
            if count > request.max_entries {
                return Err(SemanticError::LimitExceeded("LogQL source entries"));
            }
            let schema = self.schema;
            LogEntryPack::try_new(Box::new(batches), move |owner| {
                let batches = owner
                    .downcast_ref::<Vec<RecordBatch>>()
                    .expect("LogEntryPack owner is Vec<RecordBatch>");
                let mut entries = Vec::with_capacity(count);
                for batch in batches {
                    let reader = StreamLabelReader::new(batch, schema);
                    let times = batch
                        .column(0)
                        .as_any()
                        .downcast_ref::<TimestampNanosecondArray>()
                        .ok_or(SemanticError::Malformed(
                            "logs time column is not Timestamp(ns)",
                        ))?;
                    let body = batch.column(5).as_ref();
                    for row in 0..batch.num_rows() {
                        entries.push(SemanticLogEntry {
                            timestamp_ns: times.value(row),
                            line: col_str(body, row).unwrap_or(""),
                            labels: reader.labels(row),
                        });
                    }
                }
                Ok(entries)
            })
        })
    }
}

fn apply_log_filter(
    mut query: LogQuery,
    filter: &LogFilter,
    schema: &LogStreamSchema,
) -> Result<LogQuery, SemanticError> {
    match filter {
        LogFilter::All => {}
        LogFilter::LabelEq(name, _)
        | LogFilter::LabelNe(name, _)
        | LogFilter::LabelRegex(name, _)
        | LogFilter::LabelNotRegex(name, _) => {
            let Some(field) = schema.source(name).map(log_string_field) else {
                return Ok(if constant_label_matches(filter)? {
                    query
                } else {
                    query.match_none()
                });
            };
            let (op, value) = match filter {
                LogFilter::LabelEq(_, value) => (StringPredicate::Eq, value.clone()),
                LogFilter::LabelNe(_, value) => (StringPredicate::Ne, value.clone()),
                LogFilter::LabelRegex(_, pattern) => {
                    (StringPredicate::Regex, format!("^(?:{pattern})$"))
                }
                LogFilter::LabelNotRegex(_, pattern) => {
                    (StringPredicate::NotRegex, format!("^(?:{pattern})$"))
                }
                _ => unreachable!(),
            };
            query = query.string_predicate(field, op, value);
        }
        LogFilter::LineContains(value) => {
            query = query.string_predicate(LogStringField::Body, StringPredicate::Contains, value);
        }
        LogFilter::LineNotContains(value) => {
            query =
                query.string_predicate(LogStringField::Body, StringPredicate::NotContains, value);
        }
        LogFilter::LineRegex(value) => {
            query = query.string_predicate(LogStringField::Body, StringPredicate::Regex, value);
        }
        LogFilter::LineNotRegex(value) => {
            query = query.string_predicate(LogStringField::Body, StringPredicate::NotRegex, value);
        }
        LogFilter::LineMatches(value) => {
            query = query.string_predicate(LogStringField::Body, StringPredicate::Matches, value);
        }
        LogFilter::LineNotMatches(value) => {
            query =
                query.string_predicate(LogStringField::Body, StringPredicate::NotMatches, value);
        }
        LogFilter::And(filters) => {
            for filter in filters {
                query = apply_log_filter(query, filter, schema)?;
            }
        }
    }
    Ok(query)
}

fn log_string_field(source: &LogLabelSource) -> LogStringField {
    match source {
        LogLabelSource::Service => LogStringField::Service,
        LogLabelSource::Attribute(key) => LogStringField::Attribute(key.clone()),
        LogLabelSource::ResourceAttribute(key) => LogStringField::ResourceAttribute(key.clone()),
    }
}

fn constant_label_matches(filter: &LogFilter) -> Result<bool, SemanticError> {
    Ok(match filter {
        LogFilter::LabelEq(_, expected) => expected.is_empty(),
        LogFilter::LabelNe(_, expected) => !expected.is_empty(),
        LogFilter::LabelRegex(_, pattern) | LogFilter::LabelNotRegex(_, pattern) => {
            let matched = regex::Regex::new(&format!("^(?:{pattern})$"))
                .map_err(|_| SemanticError::Malformed("invalid LogQL stream regex"))?
                .is_match("");
            matches!(filter, LogFilter::LabelRegex(_, _)) == matched
        }
        _ => return Err(SemanticError::Malformed("expected a label filter")),
    })
}

fn validate_log_schema(schema: &LogStreamSchema) -> Result<(), SemanticError> {
    let mut names = BTreeMap::new();
    for (name, _) in &schema.labels {
        if name.is_empty() || names.insert(name, ()).is_some() {
            return Err(SemanticError::Malformed(
                "LogQL stream schema has an empty or duplicate label",
            ));
        }
    }
    Ok(())
}

/// Read a string cell borrowed from the array buffer (`&'a str`), tolerating `Utf8`, `Utf8View`, and
/// the `Dictionary(Int32, Utf8)` encoding used for the low-cardinality columns. The zero-copy twin of
/// the facade's owning `get_str`.
fn col_str(arr: &dyn Array, i: usize) -> Option<&str> {
    if arr.is_null(i) {
        return None;
    }
    if let Some(a) = arr.as_any().downcast_ref::<StringArray>() {
        return Some(a.value(i));
    }
    if let Some(a) = arr.as_any().downcast_ref::<StringViewArray>() {
        return Some(a.value(i));
    }
    if let Some(d) = arr.as_any().downcast_ref::<DictionaryArray<Int32Type>>() {
        let values = d.values().as_any().downcast_ref::<StringArray>()?;
        return Some(values.value(d.keys().value(i) as usize));
    }
    None
}

/// Level-2 metric series labels for one `points_batches` row, read directly from the Arrow columns:
/// `service`(2) and `metric`->`__name__`(1) borrowed zero-copy, plus every string attribute parsed
/// **once** from the canonical-JSON `attributes` blob(3). Skips the `MetricPoint` DTO materialization
/// (which parses the full *typed* attribute map — ints/doubles/bools included — for every row, then
/// re-clones the strings into the label set). PromQL's open label set needs *all* attributes, so the
/// blob parse is unavoidable; this parses it once and lifts only the string entries into the set.
pub fn metric_labels_from_batch(batch: &RecordBatch, row: usize) -> LabelSet<'_> {
    let mut out: Vec<(Cow<'_, str>, Cow<'_, str>)> = Vec::new();
    if let Some(service) = col_str(batch.column(2).as_ref(), row) {
        out.push((Cow::Borrowed("service"), Cow::Borrowed(service)));
    }
    if let Some(metric) = col_str(batch.column(1).as_ref(), row) {
        out.push((Cow::Borrowed("__name__"), Cow::Borrowed(metric)));
    }
    if let Some(json) = col_str(batch.column(3).as_ref(), row)
        && let Some(AnyValue::Map(fields)) = imbh::parse_json(json)
    {
        for (key, value) in fields {
            if let AnyValue::Str(value) = value {
                out.push((Cow::Owned(key), Cow::Owned(value)));
            }
        }
    }
    LabelSet::new(out)
}

/// A JSON blob column and the `(label name, attribute key)` pairs to extract from it — grouped so the
/// blob is parsed *once per row* regardless of how many labels it feeds. Name/key are owned so the
/// reader's lifetime is tied only to the batch it borrows, not to the (shorter-lived) schema.
struct JsonGroup<'a> {
    col: &'a dyn Array,
    keys: Vec<(String, String)>,
}

/// Level-2 log label reader: resolves the stream schema to Arrow columns *once per batch*, then reads
/// each row's labels straight from the buffers — borrowing promoted/service values (`Cow::Borrowed`,
/// no parse) and parsing a blob only for non-promoted attributes. Skips the whole `LogEntry`
/// materialization (which parses every attribute of every row) that the DTO path pays.
///
/// Column layout is the canonical `logs` projection: `service`=2, `attributes`(JSON)=6,
/// `resource`(JSON)=7; a promoted attribute has its own dictionary column, found by name.
pub struct StreamLabelReader<'a> {
    /// `service` / promoted attributes — one dictionary/utf8 column each, read borrowed (zero-copy).
    direct: Vec<(String, &'a dyn Array)>,
    /// Non-promoted attributes, grouped by blob column so each blob is parsed at most once per row.
    json: Vec<JsonGroup<'a>>,
}

impl<'a> StreamLabelReader<'a> {
    pub fn new(batch: &'a RecordBatch, schema: &LogStreamSchema) -> Self {
        let mut direct = Vec::new();
        let mut attr_keys = Vec::new();
        let mut resource_keys = Vec::new();
        for (name, source) in &schema.labels {
            match source {
                LogLabelSource::Service => direct.push((name.clone(), batch.column(2).as_ref())),
                LogLabelSource::Attribute(key) => match batch.schema().index_of(key) {
                    // A promoted attribute has its own dictionary column (record scope, §6.1).
                    Ok(idx) if idx >= 12 => direct.push((name.clone(), batch.column(idx).as_ref())),
                    _ => attr_keys.push((name.clone(), key.clone())),
                },
                LogLabelSource::ResourceAttribute(key) => {
                    resource_keys.push((name.clone(), key.clone()))
                }
            }
        }
        let mut json = Vec::new();
        if !attr_keys.is_empty() {
            json.push(JsonGroup {
                col: batch.column(6).as_ref(),
                keys: attr_keys,
            });
        }
        if !resource_keys.is_empty() {
            json.push(JsonGroup {
                col: batch.column(7).as_ref(),
                keys: resource_keys,
            });
        }
        Self { direct, json }
    }

    fn direct_labels(&self, row: usize, out: &mut Vec<(Cow<'a, str>, Cow<'a, str>)>) {
        for (name, col) in &self.direct {
            if let Some(value) = col_str(*col, row) {
                out.push((Cow::Owned(name.clone()), Cow::Borrowed(value)));
            }
        }
    }

    /// The stream label set for `row`, parsing each JSON blob **once** and extracting all its keys.
    pub fn labels(&self, row: usize) -> LabelSet<'a> {
        let mut out = Vec::new();
        self.direct_labels(row, &mut out);
        for group in &self.json {
            let Some(json) = col_str(group.col, row) else {
                continue;
            };
            let Some(AnyValue::Map(fields)) = imbh::parse_json(json) else {
                continue;
            };
            for (name, key) in &group.keys {
                if let Some(AnyValue::Str(value)) =
                    fields.iter().find(|(k, _)| k == key).map(|(_, v)| v)
                {
                    out.push((Cow::Owned(name.clone()), Cow::Owned(value.clone())));
                }
            }
        }
        LabelSet::new(out)
    }

    /// Diagnostic twin of [`labels`](Self::labels) that parses the blob once **per key** (via
    /// `json_get`) instead of once per blob — the naive fallback whose per-key re-parse regresses the
    /// non-promoted case. Kept to A/B the cost; production reads should use [`labels`](Self::labels).
    pub fn labels_per_key(&self, row: usize) -> LabelSet<'a> {
        let mut out = Vec::new();
        self.direct_labels(row, &mut out);
        for group in &self.json {
            for (name, key) in &group.keys {
                if let Some(json) = col_str(group.col, row)
                    && let Some(AnyValue::Str(value)) = imbh_core::json_get(json, key)
                {
                    out.push((Cow::Owned(name.clone()), Cow::Owned(value)));
                }
            }
        }
        LabelSet::new(out)
    }
}

pub trait TracesSemanticsExt {
    fn execute_traceql<'a>(
        &'a self,
        expression: &'a SpansetExpr,
        bounds: crate::FetchBounds,
        limits: EvalLimits,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<TraceQueryMatch>, SemanticError>> + Send + 'a>>;

    /// Arrow-native twin of [`execute_traceql`](TracesSemanticsExt::execute_traceql): the same
    /// TraceQL evaluation, returned as a `RecordBatch` (`{ trace_id, span_ids }`, one row per
    /// matched trace) instead of `Vec<TraceQueryMatch>`.
    fn execute_traceql_batches<'a>(
        &'a self,
        expression: &'a SpansetExpr,
        bounds: crate::FetchBounds,
        limits: EvalLimits,
    ) -> Pin<Box<dyn Future<Output = Result<RecordBatch, SemanticError>> + Send + 'a>>;
}

impl TracesSemanticsExt for TracesApi {
    fn execute_traceql<'a>(
        &'a self,
        expression: &'a SpansetExpr,
        bounds: crate::FetchBounds,
        limits: EvalLimits,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<TraceQueryMatch>, SemanticError>> + Send + 'a>>
    {
        Box::pin(crate::execute_traceql(self, expression, bounds, limits))
    }

    fn execute_traceql_batches<'a>(
        &'a self,
        expression: &'a SpansetExpr,
        bounds: crate::FetchBounds,
        limits: EvalLimits,
    ) -> Pin<Box<dyn Future<Output = Result<RecordBatch, SemanticError>> + Send + 'a>> {
        Box::pin(async move {
            let matches = crate::execute_traceql(self, expression, bounds, limits).await?;
            Ok(crate::batch::trace_matches_to_batch(&matches))
        })
    }
}

impl TraceSource for TracesApi {
    fn fetch_candidates<'a>(
        &'a self,
        request: &'a TraceFetchRequest,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<imbh_core::TraceId>, SemanticError>> + Send + 'a>>
    {
        Box::pin(async move {
            // Cheap first phase: rank candidate trace ids in storage (ids only, no span data).
            let summaries = self
                .search(build_trace_query(request))
                .await
                .map_err(|error| SemanticError::Source(error.to_string()))?;
            Ok(summaries
                .into_iter()
                .map(|summary| summary.trace_id)
                .collect())
        })
    }

    fn fetch_trace<'a>(
        &'a self,
        trace_id: imbh_core::TraceId,
    ) -> Pin<Box<dyn Future<Output = Result<Option<TracePack>, SemanticError>> + Send + 'a>> {
        Box::pin(async move {
            let Some(trace) = self
                .get(trace_id)
                .await
                .map_err(|error| SemanticError::Source(error.to_string()))?
            else {
                return Ok(None);
            };
            // The pack owns this one trace (type-erased); its semantic spans borrow the trace's
            // strings and attribute values for the duration of its evaluation, then it is dropped.
            let pack = TracePack::try_new(Box::new(trace), |owner| {
                let trace = owner
                    .downcast_ref::<imbh::Trace>()
                    .expect("TracePack owner is imbh::Trace");
                Ok::<_, SemanticError>(semantic_trace(trace))
            })?;
            Ok(Some(pack))
        })
    }
}

fn semantic_trace(trace: &imbh::Trace) -> SemanticTrace<'_> {
    SemanticTrace {
        trace_id: trace.trace_id,
        root_service: trace.root_service.as_deref(),
        root_name: trace.root_name.as_deref(),
        start_time_ns: trace.start_time.0,
        duration_ns: trace.duration_ns.0,
        spans: trace
            .spans
            .iter()
            .map(|span| SemanticSpan {
                span_id: span.span_id,
                parent_span_id: span.parent_span_id,
                name: span.name.as_str(),
                status: span.status_code.as_str(),
                kind: span.kind.as_str(),
                status_message: span.status_message.as_deref(),
                duration_ns: span.duration_ns.0,
                service: span.service.as_deref(),
                start_time_ns: span.start_time.0,
                attributes: typed_attributes(&span.attributes),
                resource: typed_attributes(&span.resource),
                instrumentation: typed_attributes(&span.scope),
                events: typed_scoped_items(span.events.as_deref()),
                links: typed_scoped_items(span.links.as_deref()),
            })
            .collect(),
    }
}

fn typed_attributes(attributes: &Attributes) -> TypedAttributes<'_> {
    TypedAttributes::new(
        attributes.iter().filter_map(|(key, value)| {
            semantic_value(value).map(|value| (Cow::Borrowed(key), value))
        }),
    )
}

/// Events/links come from parsing a JSON blob, so their values are inherently owned
/// (`SemanticValue<'static>`, `Cow::Owned` keys) — they cannot borrow the backing store.
fn typed_scoped_items(json: Option<&str>) -> Vec<TypedAttributes<'static>> {
    let Some(AnyValue::Array(items)) = json.and_then(imbh::parse_json) else {
        return Vec::new();
    };
    items
        .into_iter()
        .filter_map(|item| {
            let AnyValue::Map(fields) = item else {
                return None;
            };
            let mut values = Vec::new();
            for (key, value) in fields {
                if key == "attributes" {
                    if let AnyValue::Map(attributes) = value {
                        values.extend(attributes.into_iter().filter_map(|(key, value)| {
                            semantic_value(&value)
                                .map(|value| (Cow::Owned(key), value.into_owned()))
                        }));
                    }
                } else if let Some(value) = semantic_value(&value) {
                    values.push((Cow::Owned(key), value.into_owned()));
                }
            }
            Some(TypedAttributes::new(values))
        })
        .collect()
}

fn semantic_value(value: &AnyValue) -> Option<SemanticValue<'_>> {
    Some(match value {
        AnyValue::Null => SemanticValue::Nil,
        AnyValue::Str(value) => SemanticValue::String(Cow::Borrowed(value.as_str())),
        AnyValue::Int(value) => SemanticValue::Integer(*value),
        AnyValue::Double(value) => SemanticValue::Float(*value),
        AnyValue::Bool(value) => SemanticValue::Boolean(*value),
        AnyValue::Bytes(value) => SemanticValue::Bytes(Cow::Borrowed(value.as_slice())),
        AnyValue::Array(values) => {
            SemanticValue::Array(values.iter().filter_map(semantic_value).collect())
        }
        AnyValue::Map(_) => return None,
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FetchBounds, LogFilter, MatchOp, PromFetchPurpose};

    const INJECTION: &str = "value' OR TRUE --";

    #[tokio::test(flavor = "current_thread")]
    async fn log_builder_executes_injection_shaped_inputs_as_values() {
        let db = imbh::Db::in_memory().open().unwrap();
        let request = LogFetchRequest {
            bounds: FetchBounds::new(10, 20).unwrap(),
            filter: LogFilter::And(vec![
                LogFilter::LabelEq("service".to_owned(), INJECTION.to_owned()),
                LogFilter::LineContains(INJECTION.to_owned()),
            ]),
            max_entries: 10,
        };

        let page = db
            .logs()
            .query(build_log_query(&request, &LogStreamSchema::service_only()).unwrap())
            .await
            .unwrap();
        assert!(page.entries.is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn metric_builder_executes_injection_shaped_inputs_as_values() {
        let db = imbh::Db::in_memory().open().unwrap();
        let request = PromFetchRequest {
            bounds: FetchBounds::new(10, 20).unwrap(),
            purpose: PromFetchPurpose::InstantSelector,
            matchers: vec![
                LabelMatcher {
                    name: "__name__".to_owned(),
                    op: MatchOp::Eq,
                    value: INJECTION.to_owned(),
                },
                LabelMatcher {
                    name: "label' OR TRUE --".to_owned(),
                    op: MatchOp::Eq,
                    value: INJECTION.to_owned(),
                },
            ],
            max_series: 10,
            max_samples: 10,
        };

        for query in build_metric_point_queries(&request).unwrap() {
            assert!(db.metrics().points(query).await.unwrap().is_empty());
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn logql_selector_filters_the_log_list_with_dialect_and_standard_operators() {
        use imbh_test_support::otlp::otlp_log;

        let db = imbh::Db::in_memory().open().unwrap();
        for (body, t) in [
            ("connection error", 1u64),
            ("upstream timeout", 2),
            ("request ok", 3),
        ] {
            db.ingest_otlp_logs(&otlp_log("cart", body, t))
                .await
                .unwrap();
        }

        // Translate a bare LogQL selector, build the native query through the shared bridge, run it.
        let run = |source: &'static str| {
            let db = db.clone();
            async move {
                let translated =
                    crate::translate_logql(source, &crate::TranslateContext::default()).unwrap();
                let crate::ImbhQueryModel::LogSelector(filter) = translated.model else {
                    panic!("expected a bare log selector for {source:?}");
                };
                let request = LogFetchRequest {
                    bounds: FetchBounds::new(0, 1_000).unwrap(),
                    filter,
                    max_entries: 100,
                };
                let query = build_log_query(&request, &LogStreamSchema::service_only()).unwrap();
                db.logs()
                    .query(query)
                    .await
                    .unwrap()
                    .entries
                    .into_iter()
                    .map(|entry| entry.body)
                    .collect::<Vec<_>>()
            }
        };

        // `|?` (term) matches the `timeout` token; the stream selector narrows to the `cart` service.
        assert_eq!(
            run(r#"{service="cart"} |? "timeout""#).await,
            vec!["upstream timeout".to_owned()]
        );
        // `|=` (substring) matches `err` inside `error`; `|?` (term) does not.
        assert_eq!(
            run(r#"|= "err""#).await,
            vec!["connection error".to_owned()]
        );
        assert!(run(r#"|? "err""#).await.is_empty());
        // A wrong service selector filters everything out.
        assert!(run(r#"{service="checkout"} |? "timeout""#).await.is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn trace_builder_executes_closed_candidate_bounds() {
        let db = imbh::Db::in_memory().open().unwrap();
        let request = TraceFetchRequest {
            bounds: FetchBounds::new(10, 20).unwrap(),
            max_traces: 10,
            max_spans: 10,
            candidate: Vec::new(),
        };

        assert!(
            db.traces()
                .search(build_trace_query(&request))
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn traceql_pushdown_selects_the_trace_across_the_start_boundary() {
        use crate::{SpanPredicate, SpansetExpr};

        let db = imbh::Db::in_memory().open().unwrap();
        // Fixture tree: root "GET /cart" at 1000ns, child "db query" at 1100ns.
        db.ingest_otlp_traces(&imbh_test_support::otlp::otlp_trace_tree("cart", [7u8; 16]))
            .await
            .unwrap();
        let traces = db.traces();

        // `{ .name = "db query" }` lifts a `name` candidate filter. Bounds cover the root (1000) but
        // not the matching span (1100); the whole path — pushdown → semi-join → trace-start over all
        // spans → streamed eval — must still find the trace and select the "db query" span.
        let hits = crate::execute_traceql(
            &traces,
            &SpansetExpr::Select(SpanPredicate::NameEq("db query".to_owned())),
            FetchBounds::new(900, 1050).unwrap(),
            EvalLimits::default(),
        )
        .await
        .unwrap();
        assert_eq!(hits.len(), 1, "trace with a matching span must be found");
        assert_eq!(
            hits[0].spanset.selected_span_ids.len(),
            1,
            "only the matching span is selected",
        );

        // A name matching nothing: the candidate filter excludes it in storage and the evaluator
        // agrees — empty either way (soundness: the pushdown never changes the result).
        let none = crate::execute_traceql(
            &traces,
            &SpansetExpr::Select(SpanPredicate::NameEq("nonexistent".to_owned())),
            FetchBounds::new(0, i64::MAX).unwrap(),
            EvalLimits::default(),
        )
        .await
        .unwrap();
        assert!(none.is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn traceql_numeric_attr_pushdown_matches_typed_attribute() {
        use crate::{AttributeScope, SemanticValue, SpanPredicate, SpansetExpr, TraceCompareOp};

        let db = imbh::Db::in_memory().open().unwrap();
        // Three traces, each one span with an integer-typed `http.status_code` attribute.
        for (seed, status) in [(1u8, 200i64), (2, 500), (3, 503)] {
            db.ingest_otlp_traces(&imbh_test_support::otlp::otlp_trace_int_attr(
                "api",
                "handler",
                1000,
                1500,
                "http.status_code",
                status,
                seed,
            ))
            .await
            .unwrap();
        }
        let traces = db.traces();

        // `{ .http.status_code >= 500 }` lifts a numeric candidate filter that prunes in storage, and
        // the evaluator re-checks. The attribute is a genuine JSON number, so the whole path
        // (json_get_num → attr_ge → semi-join → eval) must find exactly the 500 and 503 traces.
        let ge500 = |op, value| {
            SpansetExpr::Select(SpanPredicate::Attribute {
                scope: AttributeScope::Span,
                key: "http.status_code".to_owned(),
                op,
                value,
            })
        };
        let hits = crate::execute_traceql(
            &traces,
            &ge500(TraceCompareOp::Ge, SemanticValue::Integer(500)),
            FetchBounds::new(0, i64::MAX).unwrap(),
            EvalLimits::default(),
        )
        .await
        .unwrap();
        assert_eq!(hits.len(), 2, "the 500 and 503 traces match .status >= 500");

        // Exact equality selects only the 500 trace (pushed as the closed range >=500 AND <=500).
        let eq = crate::execute_traceql(
            &traces,
            &ge500(TraceCompareOp::Eq, SemanticValue::Integer(500)),
            FetchBounds::new(0, i64::MAX).unwrap(),
            EvalLimits::default(),
        )
        .await
        .unwrap();
        assert_eq!(eq.len(), 1, "only the 500 trace matches .status = 500");
    }

    // ── duplicate timestamps against real stored data (issue #27) ───────────────────────────────

    /// A bare instant selector on `metric`, built by hand so these tests do not also depend on the
    /// translator's catalog kind resolution.
    fn name_selector(metric: &str) -> PromExpr {
        PromExpr::Selector {
            matchers: vec![LabelMatcher {
                name: "__name__".to_owned(),
                op: MatchOp::Eq,
                value: metric.to_owned(),
            }],
        }
    }

    async fn promql_at(
        db: &std::sync::Arc<imbh::Db>,
        metric: &str,
        at: i64,
    ) -> Result<Vec<PromSeries<'static>>, SemanticError> {
        db.metrics()
            .execute_promql(
                &name_selector(metric),
                EvalRange::instant(at),
                EvalLimits::default(),
            )
            .await
    }

    #[tokio::test(flavor = "current_thread")]
    async fn promql_reports_duplicate_points_within_one_table() {
        use imbh_test_support::otlp::otlp_sum;

        // `otlp_sum` maps `(time, value)` one-to-one onto data points, so this is one export
        // carrying two points at the same instant.
        let body = otlp_sum("cart", "m", 2, &[(10, 1.0), (10, 5.0), (20, 7.0)]);

        let db = imbh::Db::in_memory().open().unwrap();
        db.ingest_otlp_metrics(&body).await.unwrap();
        let error = promql_at(&db, "m", 20)
            .await
            .expect_err("the default policy errors");
        let message = error.to_string();
        assert!(
            matches!(error, SemanticError::DuplicateTimestamp(_)),
            "{error:?}"
        );
        assert!(message.contains("__name__=\"m\""), "{message}");
        assert!(
            message.contains("10"),
            "the offending instant is named: {message}"
        );

        let db = imbh::Db::in_memory()
            .duplicates(imbh::Duplicates::LastWins)
            .open()
            .unwrap();
        db.ingest_otlp_metrics(&body).await.unwrap();
        let series = promql_at(&db, "m", 20)
            .await
            .expect("last_wins resolves it");
        assert_eq!(series.len(), 1);
        assert_eq!(
            series[0].samples.len(),
            1,
            "an instant selector reports one point"
        );
        assert!((series[0].samples[0].value - 7.0).abs() < 1e-12);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn promql_handles_a_metric_present_in_both_the_gauge_and_sum_tables() {
        use imbh_test_support::otlp::{otlp_gauge_attrs, otlp_sum};

        // Not a producer fault at all: an instant selector reads *both* tables and concatenates the
        // batches, and the derived label set (service + __name__ + string attributes) does not
        // distinguish them — so a name recorded as both instrument kinds is structurally duplicated.
        let gauge = otlp_gauge_attrs("cart", "m", 10, &[]); // value 1.0
        let sum = otlp_sum("cart", "m", 2, &[(10, 9.0)]);

        let db = imbh::Db::in_memory().open().unwrap();
        db.ingest_otlp_metrics(&gauge).await.unwrap();
        db.ingest_otlp_metrics(&sum).await.unwrap();
        let error = promql_at(&db, "m", 10)
            .await
            .expect_err("the default policy errors");
        assert!(
            matches!(error, SemanticError::DuplicateTimestamp(_)),
            "{error:?}"
        );

        let db = imbh::Db::in_memory()
            .duplicates(imbh::Duplicates::LastWins)
            .open()
            .unwrap();
        db.ingest_otlp_metrics(&gauge).await.unwrap();
        db.ingest_otlp_metrics(&sum).await.unwrap();
        let series = promql_at(&db, "m", 10)
            .await
            .expect("last_wins resolves it");
        assert_eq!(series.len(), 1);
        assert_eq!(series[0].samples.len(), 1);
        assert!(
            (series[0].samples[0].value - 9.0).abs() < 1e-12,
            "{:?}",
            series[0].samples
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn ingest_rejection_keeps_the_metric_queryable_across_exports() {
        use imbh_test_support::otlp::otlp_sum;

        // The reporter's shape: the same reading republished in a *later* export, minutes of LSNs
        // apart from the first. With the guard on, the second export's point never lands, so the
        // read side never sees a duplicate at all.
        let db = imbh::Db::in_memory()
            .duplicates(imbh::Duplicates::reject())
            .open()
            .unwrap();
        let body = otlp_sum("cart", "m", 2, &[(10, 1.0)]);
        let first = db.ingest_otlp_metrics(&body).await.unwrap();
        let second = db.ingest_otlp_metrics(&body).await.unwrap();
        assert_eq!((first.accepted, first.rejected), (1, 0));
        assert_eq!((second.accepted, second.rejected), (0, 1));

        db.ingest_otlp_metrics(&otlp_sum("cart", "m", 2, &[(20, 3.0)]))
            .await
            .unwrap();
        let series = promql_at(&db, "m", 20)
            .await
            .expect("no duplicate reached storage");
        assert_eq!(series.len(), 1);
        assert!((series[0].samples[0].value - 3.0).abs() < 1e-12);
    }
}

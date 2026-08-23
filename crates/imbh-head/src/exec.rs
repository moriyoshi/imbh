//! Executing each head operation against an open [`Db`].
//!
//! This is the one implementation of the head API's semantics. `imbh-server` calls it to answer
//! `POST /api/head/…`; `imbh-tui`'s local backend calls it directly, in-process, with no HTTP in
//! between. A head therefore sees the same answers — including the same query-language translation,
//! the same caps, and the same trace-window narrowing — whether it is pointed at a directory or at a
//! daemon.
//!
//! Every function here is `async` but blocks *inside itself*, exactly like the `Db` futures
//! underneath it (there is no `spawn_blocking` anywhere in the library). Offloading is the caller's
//! decision: `imbhd` wraps these in `imbh_mcp::offload`, the TUI drives them on a runtime whose only
//! other job is the event loop.

use std::sync::Arc;
use std::time::Duration;

use imbh::{Db, MetricMeta, Table, TraceId};
use imbh_lgtm::{
    EvalLimits, EvalRange, FetchBounds, ImbhQueryModel, LogStreamSchema, LogsSemanticsExt,
    MetricKind, MetricResolution, MetricsSemanticsExt, SemanticError, TracesSemanticsExt,
    TranslateContext, translate_logql, translate_promql, translate_traceql,
};

use crate::HeadError;
use crate::dto;

/// Evaluate one or more PromQL queries, concatenating their series in request order.
///
/// The catalog is read here — once, however many queries there are — rather than taken as an
/// argument, because it is what the translation *means*: a bare `foo` is a gauge selector, a counter
/// rate, or a histogram bucket depending on the metric's recorded kind and temporality. Deriving that
/// context on the executing side is what stops a head from having to hold a stale copy of it.
pub async fn promql(
    db: &Arc<Db>,
    request: &dto::EvalRequest,
) -> Result<Vec<dto::Series>, HeadError> {
    let catalog = db.metrics().catalog().await.map_err(HeadError::from_db)?;
    let context = metric_context(&catalog);
    let (range, caps) = (range(request.window), caps(request.caps));
    let mut out = Vec::new();
    for (index, query) in request.queries.iter().enumerate() {
        let translated = translate_promql(query, &context)
            .map_err(|diagnostic| HeadError::bad_request(diagnostic.message))?;
        let ImbhQueryModel::Prom(expression) = translated.model else {
            return Err(HeadError::bad_request(
                "the translator returned a non-metric model for a PromQL query".to_owned(),
            ));
        };
        let series = db
            .metrics()
            .execute_promql(&expression, range, caps)
            .await
            .map_err(HeadError::from_semantic)?;
        out.extend(to_series(
            series.iter().map(|s| (&s.labels, &s.samples)),
            index,
        ));
    }
    Ok(out)
}

/// Evaluate a LogQL *metric* expression (a range aggregation such as `rate({}[5m])`), producing the
/// synthesized series a head plots. A bare selector is not one — it filters a log list, which is
/// [`log_query`]'s job — so it is refused here rather than silently answered with nothing.
pub async fn logql(
    db: &Arc<Db>,
    request: &dto::EvalRequest,
) -> Result<Vec<dto::Series>, HeadError> {
    let schema = LogStreamSchema::service_only();
    let (range, caps) = (range(request.window), caps(request.caps));
    let mut out = Vec::new();
    for (index, query) in request.queries.iter().enumerate() {
        let translated = translate_logql(query, &TranslateContext::default())
            .map_err(|diagnostic| HeadError::bad_request(diagnostic.message))?;
        let ImbhQueryModel::Log(expression) = translated.model else {
            return Err(HeadError::bad_request(
                "a LogQL selector is not a metric expression: wrap it in a range aggregation (e.g. \
                 `rate({…}[5m])`) to evaluate it"
                    .to_owned(),
            ));
        };
        let series = db
            .logs()
            .execute_logql(&expression, range, caps, &schema)
            .await
            .map_err(HeadError::from_semantic)?;
        out.extend(to_series(
            series.iter().map(|s| (&s.labels, &s.samples)),
            index,
        ));
    }
    Ok(out)
}

/// Search traces with TraceQL, narrowing the window toward `end` whenever the trace cap is hit.
///
/// The cap applies to *candidate* traces in the time window, before the TraceQL predicate runs, so a
/// busy window overflows however selective the query is. Rather than dead-ending on "source traces
/// limit exceeded", each retry halves the span measured back from `end_ns`, and the window actually
/// searched comes back in [`dto::TraceSearch::effective_start_ns`] so the head can say so. A failed
/// attempt costs only the candidate search — the cap is checked before complete traces are fetched —
/// so the retries are cheap.
pub async fn traceql(
    db: &Arc<Db>,
    request: &dto::TraceSearchRequest,
) -> Result<dto::TraceSearch, HeadError> {
    let translated = translate_traceql(&request.query, &TranslateContext::default())
        .map_err(|diagnostic| HeadError::bad_request(diagnostic.message))?;
    let ImbhQueryModel::Trace(expression) = translated.model else {
        return Err(HeadError::bad_request(
            "the translator returned a non-trace model for a TraceQL query".to_owned(),
        ));
    };
    let limits = caps(request.caps);

    let mut starts = Vec::with_capacity(request.narrow_steps + 1);
    starts.push(request.start_ns);
    starts.extend(narrowing_starts(
        request.start_ns,
        request.end_ns,
        request.narrow_steps,
    ));
    let mut last = SemanticError::LimitExceeded("TraceQL source traces");
    for start in starts {
        let bounds =
            FetchBounds::new(start, request.end_ns).map_err(HeadError::from_semantic_request)?;
        match db
            .traces()
            .execute_traceql(&expression, bounds, limits)
            .await
        {
            Ok(matches) => {
                return Ok(dto::TraceSearch {
                    matches: matches
                        .into_iter()
                        .map(|item| dto::TraceMatch {
                            trace_id: item.trace_id,
                            start_time_ns: item.start_time_ns,
                            selected_span_ids: item.spanset.selected_span_ids,
                        })
                        .collect(),
                    effective_start_ns: start,
                });
            }
            Err(error @ SemanticError::LimitExceeded(_)) => last = error,
            Err(error) => return Err(HeadError::from_semantic(error)),
        }
    }
    Err(HeadError::from_semantic(last))
}

/// The head's owned form of an evaluated series set.
///
/// Shared by PromQL and LogQL because their results are the same thing: a label set and a run of
/// `(timestamp, value)` samples. The native forms (`PromSeries`/`LogSeries`) borrow their labels
/// from the Arrow batch they were read out of, so the head's copy is owned.
/// `query_index` tags every series with the request query it came from, so a batched evaluation
/// stays attributable (see [`dto::Series::query_index`]).
fn to_series<'a>(
    series: impl IntoIterator<Item = (&'a imbh_lgtm::LabelSet<'a>, &'a Vec<imbh_lgtm::FloatSample>)>,
    query_index: usize,
) -> Vec<dto::Series> {
    series
        .into_iter()
        .map(|(labels, samples)| dto::Series {
            labels: labels
                .iter()
                .map(|(name, value)| dto::Label {
                    name: name.to_string(),
                    value: value.to_string(),
                })
                .collect(),
            samples: samples
                .iter()
                .map(|sample| dto::SamplePoint {
                    timestamp_ns: sample.timestamp_ns,
                    value: sample.value,
                })
                .collect(),
            query_index,
        })
        .collect()
}

/// Candidate window starts to try, most-recent-first, after the full `[start, end]` window overflows
/// the trace cap: each halves the span measured back from `end_ns`, so the searched window shrinks
/// toward the present. Returns only the *narrowed* starts — the caller tries `start_ns` first — and
/// never reaches `end_ns`, which would be an empty window.
pub fn narrowing_starts(start_ns: i64, end_ns: i64, steps: usize) -> Vec<i64> {
    let mut out = Vec::new();
    let mut span = end_ns.saturating_sub(start_ns).max(0);
    for _ in 0..steps {
        span /= 2;
        if span <= 0 {
            break;
        }
        out.push(end_ns.saturating_sub(span));
    }
    out
}

/// Fetch one complete trace by hex id.
pub async fn trace(
    db: &Arc<Db>,
    request: &dto::TraceGetRequest,
) -> Result<Option<imbh::Trace>, HeadError> {
    let trace_id = TraceId::from_hex(&request.trace_id).ok_or_else(|| {
        HeadError::bad_request(format!(
            "`{}` is not a 32-character hex trace id",
            request.trace_id
        ))
    })?;
    db.traces().get(trace_id).await.map_err(HeadError::from_db)
}

/// Run one native log query, page and all.
pub async fn log_query(
    db: &Arc<Db>,
    request: &dto::LogQueryRequest,
) -> Result<imbh::LogPage, HeadError> {
    db.logs()
        .query(request.query.clone())
        .await
        .map_err(HeadError::from_db)
}

/// Bucketed log counts over the query's range — the volume sparkline under a log list.
pub async fn log_volume(
    db: &Arc<Db>,
    request: &dto::LogVolumeRequest,
) -> Result<dto::LogVolumeResult, HeadError> {
    let buckets = db
        .logs()
        .volume(
            request.query.clone(),
            Duration::from_nanos(request.step_ns.max(1)),
        )
        .await
        .map_err(HeadError::from_db)?;
    Ok(dto::LogVolumeResult { buckets })
}

/// The metric catalog: every metric's name, kind, unit, and temporality.
pub async fn metric_catalog(db: &Arc<Db>) -> Result<dto::MetricCatalog, HeadError> {
    let metrics = db.metrics().catalog().await.map_err(HeadError::from_db)?;
    Ok(dto::MetricCatalog { metrics })
}

/// One metric's groupable labels and their distinct values — what a "group/filter by …" picker is
/// built from.
///
/// Read from the metric tables rather than by evaluating a selector, which is the only way it can
/// work for every kind: PromQL has no bare selector for a cumulative histogram (its buckets are
/// reachable only through `histogram_quantile(…)`), so a discovery query phrased in PromQL answers
/// nothing at all for the one family whose labels are hardest to guess. Reading the tables is also
/// independent of the picker's time range and of any evaluation cap.
pub async fn metric_dimensions(
    db: &Arc<Db>,
    request: &dto::MetricDimensionsRequest,
) -> Result<dto::MetricDimensions, HeadError> {
    let dimensions = db
        .metrics()
        .dimensions(&request.metric)
        .await
        .map_err(HeadError::from_db)?;
    let cap = request.max_values.unwrap_or(usize::MAX);
    Ok(dto::MetricDimensions {
        dimensions: dimensions
            .into_iter()
            .map(|(label, mut values)| {
                let truncated = values.len() > cap;
                values.truncate(cap);
                dto::MetricDimension {
                    label,
                    values,
                    truncated,
                }
            })
            .collect(),
    })
}

/// One metric's exemplars — the metric→trace drill-down links.
pub async fn exemplars(
    db: &Arc<Db>,
    request: &dto::ExemplarsRequest,
) -> Result<dto::Exemplars, HeadError> {
    let exemplars = db
        .metrics()
        .exemplars(&request.metric)
        .await
        .map_err(HeadError::from_db)?;
    Ok(dto::Exemplars {
        exemplars: exemplars
            .into_iter()
            .map(|exemplar| dto::ExemplarPoint {
                time_unix_nano: exemplar.time.0,
                value: exemplar.value,
                trace_id: exemplar.trace_id.map(|id| id.to_hex()),
                span_id: exemplar.span_id.map(|id| id.to_hex()),
                attributes: exemplar.attributes,
            })
            .collect(),
    })
}

/// Every attribute key across all signals — the label-name completion vocabulary.
pub async fn attribute_keys(db: &Arc<Db>) -> Result<dto::Names, HeadError> {
    let names = db.attrs().names().await.map_err(HeadError::from_db)?;
    Ok(dto::Names { names })
}

/// One attribute key's distinct values — the label-value completion vocabulary.
pub async fn attribute_values(
    db: &Arc<Db>,
    request: &dto::AttributeValuesRequest,
) -> Result<dto::Names, HeadError> {
    let names = db
        .attrs()
        .values(&request.key)
        .await
        .map_err(HeadError::from_db)?;
    Ok(dto::Names { names })
}

/// Attribute cardinality and per-segment selectivity (sigma) over the daemon's database — the
/// measurement behind a `promote` list.
///
/// The only head operation that **scans**: it reads the attribute columns of every sealed segment in
/// range, so its cost is the corpus, not the answer. It takes no lock and writes nothing, so a
/// writer keeps running underneath it; what a caller owes is a sensible
/// [`range`](dto::AttrStatsRequest::range). `imbhd` wraps it in `offload` like every other operation,
/// which keeps a long scan off the connection's runtime.
pub async fn attribute_stats(
    db: &Arc<Db>,
    request: &dto::AttrStatsRequest,
) -> Result<dto::AttrStats, HeadError> {
    db.attribute_stats(request)
        .await
        .map_err(HeadError::from_db)
}

/// Database statistics: the per-table breakdown plus the engine-wide gauges.
pub async fn stats(db: &Arc<Db>) -> Result<dto::Stats, HeadError> {
    let stats = db.stats().await.map_err(HeadError::from_db)?;
    Ok(dto::Stats::from(&stats))
}

/// The physical table a [`dto::TableStats::table`] name refers to, or `None` for a name this build
/// does not know. Lets a head reason about table *kinds* (metric families, say) without matching on
/// strings itself.
pub fn table_from_name(name: &str) -> Option<Table> {
    Table::ALL.into_iter().find(|table| table.as_str() == name)
}

// ── translation context ─────────────────────────────────────────────────────────────────────────

/// The PromQL translation context derived from a metric catalog.
///
/// PromQL has no metric kinds; OTel does. What a selector *means* — plot the samples, rate a
/// cumulative counter, or read histogram buckets — therefore comes from the catalog's recorded kind
/// and temporality. Delta-temporality sums and histograms have no PromQL counterpart and are left
/// out, so a query against one is refused by name rather than answered wrongly. A cumulative
/// histogram is registered twice: once under its own name and once under the `_bucket` suffix
/// Prometheus spells its buckets with.
pub fn metric_context(catalog: &[MetricMeta]) -> TranslateContext {
    let mut metrics = Vec::new();
    for metric in catalog {
        let cumulative = |temporality: Option<&str>| {
            temporality.is_some_and(|t| t.eq_ignore_ascii_case("cumulative"))
        };
        let kind = match metric.kind.as_str() {
            "gauge" => MetricKind::Gauge,
            "sum" if cumulative(metric.temporality.as_deref()) => MetricKind::CumulativeCounter,
            "histogram" if cumulative(metric.temporality.as_deref()) => {
                MetricKind::CumulativeHistogram
            }
            _ => continue,
        };
        metrics.push(MetricResolution {
            query_name: metric.metric.clone(),
            storage_name: metric.metric.clone(),
            kind,
        });
        if kind == MetricKind::CumulativeHistogram {
            metrics.push(MetricResolution {
                query_name: format!("{}_bucket", metric.metric),
                storage_name: metric.metric.clone(),
                kind,
            });
        }
    }
    TranslateContext { metrics }
}

// ── wire ↔ semantic conversions ─────────────────────────────────────────────────────────────────

/// The evaluation range a [`dto::EvalWindow`] names. `step_ns` is floored at 1: a zero step is not a
/// window, it is a division by zero inside the evaluator.
pub fn range(window: dto::EvalWindow) -> EvalRange {
    EvalRange {
        start_ns: window.start_ns,
        end_ns: window.end_ns,
        step_ns: window.step_ns.max(1),
        lookback_ns: window.lookback_ns,
    }
}

/// The evaluation limits a [`dto::EvalCaps`] names, with the engine's defaults for every cap it
/// leaves out.
pub fn caps(caps: dto::EvalCaps) -> EvalLimits {
    let defaults = EvalLimits::default();
    EvalLimits {
        max_evaluation_points: caps
            .max_evaluation_points
            .unwrap_or(defaults.max_evaluation_points),
        max_series: caps.max_series.unwrap_or(defaults.max_series),
        max_samples: caps.max_samples.unwrap_or(defaults.max_samples),
        max_spans: caps.max_spans.unwrap_or(defaults.max_spans),
        max_traces: caps.max_traces.unwrap_or(defaults.max_traces),
        max_recursion: caps.max_recursion.unwrap_or(defaults.max_recursion),
    }
}

/// The wire form of an [`EvalRange`], for a head building a request out of one.
pub fn window_of(range: EvalRange) -> dto::EvalWindow {
    dto::EvalWindow {
        start_ns: range.start_ns,
        end_ns: range.end_ns,
        step_ns: range.step_ns,
        lookback_ns: range.lookback_ns,
    }
}

/// The wire form of an [`EvalLimits`], for a head building a request out of one. Every cap is sent
/// explicitly: the head asked for these numbers, and inheriting a differently-configured daemon's
/// defaults for some of them would silently answer a different question.
pub fn caps_of(limits: EvalLimits) -> dto::EvalCaps {
    dto::EvalCaps {
        max_evaluation_points: Some(limits.max_evaluation_points),
        max_series: Some(limits.max_series),
        max_samples: Some(limits.max_samples),
        max_spans: Some(limits.max_spans),
        max_traces: Some(limits.max_traces),
        max_recursion: Some(limits.max_recursion),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(metric: &str, kind: &str, temporality: Option<&str>) -> MetricMeta {
        MetricMeta {
            metric: metric.to_owned(),
            unit: String::new(),
            temporality: temporality.map(str::to_owned),
            kind: kind.to_owned(),
        }
    }

    #[test]
    fn narrowing_starts_shrink_the_window_toward_the_end() {
        // Each step halves the span from `end`, so starts increase monotonically toward `end`.
        assert_eq!(narrowing_starts(0, 800, 4), vec![400, 600, 700, 750]);
        // A zero-width window yields nothing to try.
        assert_eq!(narrowing_starts(100, 100, 4), Vec::<i64>::new());
        // Steps stop once the span rounds down to zero rather than emitting `end` (an empty window).
        assert_eq!(narrowing_starts(0, 4, 8), vec![2, 3]);
        // No steps requested is no retry at all.
        assert_eq!(narrowing_starts(0, 800, 0), Vec::<i64>::new());
    }

    #[test]
    fn the_translation_context_follows_kind_and_temporality() {
        let context = metric_context(&[
            meta("temp", "gauge", None),
            meta("reqs", "sum", Some("Cumulative")),
            meta("lat", "histogram", Some("cumulative")),
            // Delta temporality has no PromQL counterpart, so these are left unresolvable rather
            // than answered as if they were cumulative.
            meta("deltas", "sum", Some("delta")),
            meta("dhist", "histogram", Some("delta")),
            meta("untyped", "summary", None),
        ]);
        let names: Vec<&str> = context
            .metrics
            .iter()
            .map(|m| m.query_name.as_str())
            .collect();
        assert_eq!(names, vec!["temp", "reqs", "lat", "lat_bucket"]);
        // The `_bucket` alias resolves back to the metric it is stored under.
        let bucket = context
            .metrics
            .iter()
            .find(|m| m.query_name == "lat_bucket")
            .expect("bucket alias");
        assert_eq!(bucket.storage_name, "lat");
        assert_eq!(bucket.kind, MetricKind::CumulativeHistogram);
    }

    #[test]
    fn caps_fill_in_only_what_the_head_left_out() {
        let defaults = EvalLimits::default();
        let partial = caps(dto::EvalCaps {
            max_series: Some(5),
            ..dto::EvalCaps::default()
        });
        assert_eq!(partial.max_series, 5);
        assert_eq!(partial.max_traces, defaults.max_traces);
        // A round trip through the wire form is lossless.
        assert_eq!(caps(caps_of(defaults)), defaults);
    }

    #[test]
    fn a_zero_step_cannot_reach_the_evaluator() {
        let window = dto::EvalWindow {
            start_ns: 0,
            end_ns: 10,
            step_ns: 0,
            lookback_ns: 1,
        };
        assert_eq!(range(window).step_ns, 1);
    }

    #[test]
    fn table_names_map_back_to_tables() {
        assert_eq!(table_from_name("metrics_gauge"), Some(Table::MetricsGauge));
        assert_eq!(table_from_name("logs"), Some(Table::Logs));
        assert_eq!(table_from_name("nope"), None);
    }
}

//! The queries behind a refresh: evaluating the panel query for a screen and the ancillary lookups
//! (trace waterfalls, catalog dimensions, the evaluation window).

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use imbh::{PageCursor, SpanId, Table as DbTable, Timestamp, TraceId};
use imbh_head::HeadError;
use imbh_head::exec::table_from_name;
use imbh_lgtm::{
    EvalLimits, EvalRange, FetchBounds, ImbhQueryModel, LogFetchRequest, LogFilter,
    LogStreamSchema, TranslateContext, build_log_query, translate_logql,
};

use crate::backend::Backend;
use crate::chart::chart_values;
use crate::format::{attrs_to_pairs, format_metric_value, severity_label};
use crate::model::{
    DetailPane, DimNode, LogCorrelation, LogRecord, Options, Screen, SeriesData, Snapshot,
    TableData,
};
use crate::promql::discovery_promql;
use crate::time::{format_timestamp_ns, humanize_secs};
use crate::ui::glyphs::Glyphs;
use crate::waterfall::{TraceDetail, build_trace_detail};

/// The `[min, max]` timestamp span across all metric tables, from the backend's stats. Falls back to
/// a wide window ending at `now` if no metric data has a recorded span. Makes catalog dimension
/// discovery independent of the selected time range.
pub(crate) async fn metric_time_span(backend: &Backend) -> (i64, i64) {
    const WIDE_NS: i64 = 3_600_000_000_000 * 24 * 365 * 30; // ~30 years
    let now = Timestamp::now().0;
    let fallback = (now.saturating_sub(WIDE_NS), now);
    let Ok(stats) = backend.stats().await else {
        return fallback;
    };
    // The wire carries the physical table *name*; map it back so the metric families are matched as
    // tables rather than by string prefix.
    let is_metric = |name: &str| {
        matches!(
            table_from_name(name),
            Some(
                DbTable::MetricsGauge
                    | DbTable::MetricsSum
                    | DbTable::MetricsHistogram
                    | DbTable::MetricsExpHistogram
                    | DbTable::MetricsSummary
            )
        )
    };
    let min = stats
        .tables
        .iter()
        .filter(|t| is_metric(&t.table))
        .filter_map(|t| t.min_time_unix_nano)
        .min();
    let max = stats
        .tables
        .iter()
        .filter(|t| is_metric(&t.table))
        .filter_map(|t| t.max_time_unix_nano)
        .max();
    match (min, max) {
        (Some(min), Some(max)) => (min, max),
        _ => fallback,
    }
}

/// Discover a metric's groupable dimensions by evaluating its bare selector as an instant over the
/// metric's whole retained span (picker-independent), collecting the label keys/values from the
/// returned series (labels include the resource `service` and data-point attributes; `__name__`/`le`
/// are internal and excluded). Empty on any failure.
pub(crate) async fn discover_dims(
    backend: &Backend,
    name: &str,
    kind: &str,
    max_series: usize,
) -> Vec<DimNode> {
    let (span_start, span_end) = metric_time_span(backend).await;
    // One instant just past the last sample, looking back across the whole span.
    let at = span_end.saturating_add(1);
    let eval_range = EvalRange {
        start_ns: at,
        end_ns: at,
        step_ns: 1,
        lookback_ns: (at.saturating_sub(span_start).max(1) as u128).min(u64::MAX as u128) as u64,
    };
    let limits = EvalLimits {
        max_series,
        ..EvalLimits::default()
    };
    let Ok(series) = backend
        .promql(&[discovery_promql(name, kind)], eval_range, limits)
        .await
    else {
        return Vec::new();
    };
    let mut by_label: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for item in &series {
        for label in &item.labels {
            if label.name == "__name__" || label.name == "le" {
                continue;
            }
            by_label
                .entry(label.name.clone())
                .or_default()
                .insert(label.value.clone());
        }
    }
    by_label
        .into_iter()
        .map(|(label, values)| DimNode {
            label,
            values: values.into_iter().collect(),
            expanded: false,
            selected: None,
        })
        .collect()
}

/// Fetch a single trace by hex id and render its waterfall into a detail pane, degrading to a short
/// message on a missing/invalid id or a source error. The materialized trace is returned alongside the
/// pane so the app can open the full trace detail (`Route::TraceDetail`) without a second fetch.
pub(crate) async fn build_waterfall_detail(
    backend: &Backend,
    trace_id_hex: &str,
    ascii: bool,
) -> (DetailPane, Option<TraceDetail>) {
    // Success carries a structured `Waterfall` so the bars reflow to the pane width at draw time;
    // the miss/error branches only have a message, delivered through `lines`.
    let (lines, trace) = match backend.trace(trace_id_hex).await {
        Ok(Some(trace)) => (Vec::new(), Some(build_trace_detail(&trace, ascii))),
        Ok(None) => (vec!["trace not found.".to_owned()], None),
        Err(error) => (vec![format!("error: {error}")], None),
    };
    let pane = DetailPane {
        title: format!("Waterfall: {trace_id_hex}  (enter: detail)"),
        lines,
        waterfall: trace.as_ref().map(|trace| trace.waterfall.clone()),
    };
    (pane, trace)
}

/// Turn the residual trace-limit failure (the window could not be narrowed enough) into actionable
/// guidance; pass every other failure through unchanged.
pub(crate) fn trace_limit_message(error: &HeadError, cap: usize) -> String {
    if error.is_limit_exceeded() {
        format!(
            "too many traces even in the most recent sub-window (cap {cap}). Add filters (e.g. \
             status=error, duration>Nms) or pick a shorter time range."
        )
    } else {
        error.to_string()
    }
}

/// The evaluation window `(start_ns, end_ns, range, limits)` for `now - lookback .. now` at the given
/// step and caps. Shared by the panel query and the catalog dimension-discovery task.
pub(crate) fn eval_window(options: &Options) -> (i64, i64, EvalRange, EvalLimits) {
    // An absolute window is a fixed span with a step derived to keep the sample count bounded; a
    // rolling window is `now - lookback .. now` at the preset step.
    let (start, end, step_ns) = match options.window {
        Some((start, end)) => {
            let (start, end) = if start <= end {
                (start, end)
            } else {
                (end, start)
            };
            // ~120 points across the span, whole seconds, at least 1s.
            let span_secs = ((end.saturating_sub(start)).max(0) / 1_000_000_000) as u64;
            let step_secs = (span_secs / 120).max(1);
            (start, end, step_secs.saturating_mul(1_000_000_000))
        }
        None => {
            let end = Timestamp::now().0;
            let start =
                end.saturating_sub(options.lookback.as_nanos().min(i64::MAX as u128) as i64);
            let step_ns = options.step.as_nanos().min(u64::MAX as u128) as u64;
            (start, end, step_ns)
        }
    };
    let eval_range = EvalRange {
        start_ns: start,
        end_ns: end,
        step_ns: step_ns.max(1),
        lookback_ns: 300_000_000_000,
    };
    let span_secs = (end.saturating_sub(start)).max(0) as u64 / 1_000_000_000;
    let step_secs = (step_ns / 1_000_000_000).max(1);
    let limits = EvalLimits {
        max_series: options.max_series,
        max_samples: options
            .max_series
            .saturating_mul(
                usize::try_from(span_secs / step_secs)
                    .unwrap_or(1)
                    .saturating_add(2),
            )
            .max(1),
        max_traces: options.max_rows,
        ..EvalLimits::default()
    };
    (start, end, eval_range, limits)
}

pub(crate) async fn load_snapshot(
    backend: Backend,
    screen: Screen,
    query: &str,
    options: &Options,
    // Older/newer paging cursor for the Logs list (the older page's [`imbh::LogPage::next`], or `None`
    // for page 0). Ignored off the Logs screen.
    after: Option<PageCursor>,
    // Trace/span correlation filter layered onto the Logs query for a trace→log drill-down. Ignored
    // off the Logs screen.
    correlation: Option<LogCorrelation>,
) -> Result<Snapshot, String> {
    let (start, end, eval_range, limits) = eval_window(options);
    // Chrome glyphs woven into snapshot text (titles, the truncation warning) follow `--ascii` too.
    let g = Glyphs::new(options.ascii);
    match screen {
        Screen::Overview => {
            let stats = backend.stats().await.map_err(|error| error.to_string())?;
            let mut lines = vec![
                format!("buffer: {} bytes", stats.buffer_bytes),
                format!("WAL: {} bytes", stats.wal_bytes),
                format!("ingest queue: {}", stats.ingest_queue_depth),
            ];
            lines.extend(stats.tables.into_iter().map(|table| {
                format!(
                    "{:<24} rows={}+{} segments={}",
                    table.table, table.segment_rows, table.buffer_rows, table.segment_count
                )
            }));
            Ok(Snapshot {
                title: "Database overview".to_owned(),
                lines,
                chart: Vec::new(),
                detail: None,
                list_from: None,
                log_records: Vec::new(),
                table: None,
                series: Vec::new(),
                next_cursor: None,
            })
        }
        Screen::Metrics => {
            if query.trim().is_empty() {
                // Only the catalog listing needs the catalog itself; an evaluation gets its
                // translation context from the executing side, which reads it there.
                let catalog = backend
                    .metric_catalog()
                    .await
                    .map_err(|error| error.to_string())?;
                let rows = catalog
                    .iter()
                    .map(|metric| {
                        vec![
                            metric.metric.clone(),
                            metric.kind.clone(),
                            metric.unit.clone(),
                            metric.temporality.clone().unwrap_or_else(|| "-".to_owned()),
                        ]
                    })
                    .collect::<Vec<_>>();
                return Ok(Snapshot {
                    title: format!(
                        "Metric catalog {d} {} metrics (Space: expand/select series {s} Enter: visualize selected {s} e: PromQL)",
                        rows.len(),
                        d = g.dash,
                        s = g.sep,
                    ),
                    lines: Vec::new(),
                    chart: Vec::new(),
                    detail: None,
                    list_from: None,
                    log_records: Vec::new(),
                    table: Some(TableData {
                        header: vec![
                            "Metric".to_owned(),
                            "Kind".to_owned(),
                            "Unit".to_owned(),
                            "Temporality".to_owned(),
                        ],
                        rows,
                    }),
                    series: Vec::new(),
                    next_cursor: None,
                });
            }
            // One or more newline-separated PromQL queries (the catalog joins several when multiple
            // metrics are checked; the executor has no `or`, so each runs on its own). Their result
            // series are concatenated — each keeps its `__name__` label, so they stay distinguishable.
            let sub_queries = query
                .split('\n')
                .map(str::trim)
                .filter(|q| !q.is_empty())
                .map(str::to_owned)
                .collect::<Vec<_>>();
            let series = backend
                .promql(&sub_queries, eval_range, limits)
                .await
                .map_err(|error| error.to_string())?;
            // Build the summary rows and, in the same pass, retain each series' full
            // `(timestamp_ns, value)` history so the detailed viewer can plot the selected one.
            let mut rows = Vec::with_capacity(series.len());
            let mut series_data = Vec::with_capacity(series.len());
            for item in &series {
                let labels = if item.labels.is_empty() {
                    "{}".to_owned()
                } else {
                    item.labels
                        .iter()
                        .map(|label| format!("{}={}", label.name, label.value))
                        .collect::<Vec<_>>()
                        .join(",")
                };
                let values = item
                    .samples
                    .iter()
                    .map(|sample| sample.value)
                    .filter(|value| value.is_finite())
                    .collect::<Vec<_>>();
                let latest = item.samples.last().map_or(f64::NAN, |sample| sample.value);
                let (min, max) = if values.is_empty() {
                    (f64::NAN, f64::NAN)
                } else {
                    (
                        values.iter().copied().fold(f64::INFINITY, f64::min),
                        values.iter().copied().fold(f64::NEG_INFINITY, f64::max),
                    )
                };
                rows.push(vec![
                    labels.clone(),
                    format_metric_value(latest),
                    format_metric_value(min),
                    format_metric_value(max),
                    item.samples.len().to_string(),
                ]);
                series_data.push(SeriesData {
                    labels,
                    points: item
                        .samples
                        .iter()
                        .map(|sample| (sample.timestamp_ns, sample.value))
                        .collect(),
                });
            }
            // Title shows the single query, or a metric count when several are combined.
            let title_query = if sub_queries.len() > 1 {
                format!("{} metrics", sub_queries.len())
            } else {
                sub_queries.first().map_or(query, String::as_str).to_owned()
            };
            Ok(Snapshot {
                title: format!(
                    "PromQL: {title_query} {} {} series (Enter: view series)",
                    g.dash,
                    rows.len()
                ),
                chart: chart_values(
                    series
                        .first()
                        .map_or(&[][..], |series| series.samples.as_slice())
                        .iter()
                        .map(|sample| sample.value),
                ),
                lines: Vec::new(),
                detail: None,
                list_from: None,
                log_records: Vec::new(),
                table: Some(TableData {
                    header: vec![
                        "Series".to_owned(),
                        "Latest".to_owned(),
                        "Min".to_owned(),
                        "Max".to_owned(),
                        "Points".to_owned(),
                    ],
                    rows,
                }),
                series: series_data,
                next_cursor: None,
            })
        }
        Screen::Traces => {
            // The trace cap is applied to candidate traces in the time *window*, before the TraceQL
            // predicate runs, so a busy window overflows however selective the query is. Rather than
            // dead-end on "TraceQL source traces limit exceeded", the executing side focuses on the
            // most recent sub-window that fits and reports which one — and we say so loudly.
            let search = backend
                .traceql(query, start, end, limits)
                .await
                .map_err(|error| trace_limit_message(&error, limits.max_traces))?;
            let effective_start = search.effective_start_ns;
            let narrowed = effective_start > start;
            let mut lines = Vec::new();
            if narrowed {
                let window_secs = ((end - effective_start).max(0) / 1_000_000_000) as u64;
                lines.push(format!(
                    "{} full range had more than {} traces {} showing the most recent {} ({} .. {}).",
                    g.warn,
                    limits.max_traces,
                    g.dash,
                    humanize_secs(window_secs.max(1)),
                    format_timestamp_ns(effective_start),
                    format_timestamp_ns(end),
                ));
                lines.push(
                    "  add filters (e.g. status=error, duration>Nms) or shorten the time range to \
                     search the whole window."
                        .to_owned(),
                );
                lines.push(String::new());
            }
            lines.push(format!("{} matching traces", search.matches.len()));
            // Rows below the header count are the selectable trace entries.
            let list_from = lines.len();
            // The trace id stays the leading whitespace-delimited token (`selected_trace_id` /
            // `focus_select_trace` parse it), so the trace's start time is appended after it.
            lines.extend(search.matches.into_iter().map(|item| {
                format!(
                    "{}  {}  selected={}",
                    item.trace_id,
                    format_timestamp_ns(item.start_time_ns),
                    item.selected_span_ids.join(",")
                )
            }));
            // The waterfall is fetched on demand for the selected trace (`request_waterfall`); ship a
            // placeholder so the split layout is stable until it arrives.
            let has_rows = list_from < lines.len();
            let detail = Some(DetailPane {
                title: "Waterfall".to_owned(),
                lines: vec![if has_rows {
                    "Loading waterfall...".to_owned()
                } else {
                    "No trace selected.".to_owned()
                }],
                waterfall: None,
            });
            Ok(Snapshot {
                title: if narrowed {
                    format!("TraceQL (narrowed): {query}")
                } else {
                    format!("TraceQL: {query}")
                },
                chart: Vec::new(),
                lines,
                detail,
                list_from: Some(list_from),
                log_records: Vec::new(),
                table: None,
                series: Vec::new(),
                next_cursor: None,
            })
        }
        Screen::Logs => {
            let schema = LogStreamSchema::service_only();
            // The box accepts either a bare LogQL selector (`{service="api"} |? "timeout"`), which
            // filters the list, or a range-aggregation metric expression (`rate({}[5m])`), which also
            // drives the sparkline. Both forms yield a `LogFilter` that filters the displayed list; an
            // empty box means "all logs". `|?`/`!?` (imbh dialect) push down to the Tantivy `.tidx`.
            // Only the *filter* is derived here: it is what builds the native `LogQuery` the list is
            // paged with, correlation and cursor included, and that query travels to the backend as
            // itself. A range expression additionally drives the sparkline, which the backend
            // evaluates from the same query text (`Backend::logql`) rather than from a re-sent AST.
            let (filter, is_range_expr) = if query.trim().is_empty() {
                (LogFilter::All, false)
            } else {
                let translated = translate_logql(query, &TranslateContext::default())
                    .map_err(|diagnostic| diagnostic.message)?;
                match translated.model {
                    ImbhQueryModel::LogSelector(filter) => (filter, false),
                    ImbhQueryModel::Log(expression) => (expression.filter.clone(), true),
                    _ => return Err("translator returned a non-log model".to_owned()),
                }
            };
            // Filter the list through the shared `LogFilter` → native `LogQuery` bridge, then restore
            // the viewer's most-recent-first ordering and exact page size (the bridge defaults to
            // ascending + one-over for its own paging).
            let bounds = FetchBounds::new(start, end).map_err(|error| error.to_string())?;
            let request = LogFetchRequest {
                bounds,
                filter: filter.clone(),
                max_entries: options.max_rows,
            };
            let mut list_query = build_log_query(&request, &schema)
                .map_err(|error| error.to_string())?
                .direction(imbh::Direction::Backward)
                .limit(options.max_rows);
            // Layer a trace→log drill-down correlation (raw-binary id equality) onto the query. A
            // malformed hex id is ignored rather than failing the whole panel.
            if let Some(correlation) = &correlation {
                if let Some(trace) = TraceId::from_hex(&correlation.trace_id) {
                    list_query = list_query.trace_id(trace);
                }
                if let Some(span) = correlation.span_id.as_deref().and_then(SpanId::from_hex) {
                    list_query = list_query.span_id(span);
                }
            }
            // Older/newer paging: resume past the previous pages' rows. The volume sparkline below is
            // unpaged (it covers the whole window), so it is built from an unpaged clone taken first.
            let volume_query = list_query.clone();
            if let Some(cursor) = after {
                list_query = list_query.after(cursor);
            }
            let page = backend
                .log_query(list_query.clone())
                .await
                .map_err(|error| error.to_string())?;
            let page_next = page.next;
            // The sparkline: the synthesized metric for a range expression, else the log volume of the
            // filtered set over the same window.
            let chart = if is_range_expr {
                let derived = backend
                    .logql(query, eval_range, limits)
                    .await
                    .map_err(|error| error.to_string())?;
                chart_values(
                    derived
                        .first()
                        .map_or(&[][..], |series| series.samples.as_slice())
                        .iter()
                        .map(|sample| sample.value),
                )
            } else {
                let step = Duration::from_nanos(eval_range.step_ns.max(1));
                let buckets = backend
                    .log_volume(volume_query, step)
                    .await
                    .map_err(|error| error.to_string())?;
                chart_values(buckets.iter().map(|bucket| bucket.count as f64))
            };
            let mut lines = vec![format!(
                "viewer rows={} scanned={} bytes={} index={}",
                page.entries.len(),
                page.stats.rows_scanned,
                page.stats.bytes_scanned,
                page.stats.used_index
            )];
            // Rows below the stat header are the selectable log entries. Each row shows a short trace
            // id (or `--------` when absent) so the log↔trace linkage is visible in the list; the full
            // record is kept in `log_records` for the detail view and trace-id navigation.
            let list_from = lines.len();
            let mut log_records = Vec::with_capacity(page.entries.len());
            for entry in page.entries {
                let trace_id = entry.trace_id.map(|id| id.to_hex());
                let short_trace = trace_id.as_deref().map_or_else(
                    || "--------".to_owned(),
                    |hex| hex[..hex.len().min(8)].to_owned(),
                );
                lines.push(format!(
                    "{} {} {:<8} {}",
                    format_timestamp_ns(entry.time.0),
                    short_trace,
                    entry.service.as_deref().unwrap_or("-"),
                    entry.body.replace('\n', " ")
                ));
                log_records.push(LogRecord {
                    time_ns: entry.time.0,
                    severity: entry
                        .severity_text
                        .clone()
                        .unwrap_or_else(|| severity_label(entry.severity_number)),
                    service: entry.service.clone(),
                    body: entry.body.clone(),
                    trace_id,
                    span_id: entry.span_id.map(|id| id.to_hex()),
                    attributes: attrs_to_pairs(&entry.attributes),
                    resource: attrs_to_pairs(&entry.resource),
                    scope: attrs_to_pairs(&entry.scope),
                });
            }
            // The title reflects any active trace→log drill-down and whether an older page is shown; the
            // `n`/`p` keys page older/newer (see `handle_key`).
            let paged = if after.is_some() { " [older]" } else { "" };
            let title = match (&correlation, is_range_expr) {
                (Some(correlation), _) => {
                    let short = &correlation.trace_id[..correlation.trace_id.len().min(8)];
                    let span = correlation
                        .span_id
                        .as_deref()
                        .map(|id| format!(" span {}", &id[..id.len().min(8)]))
                        .unwrap_or_default();
                    format!("Logs for trace {short}{span} {} n/p: page{paged}", g.dash)
                }
                (None, true) => format!("Log search + synthesized metric: {query}{paged}"),
                (None, false) => format!("Log search: {query}{paged}"),
            };
            Ok(Snapshot {
                title,
                chart,
                lines,
                detail: None,
                list_from: Some(list_from),
                log_records,
                table: None,
                series: Vec::new(),
                next_cursor: page_next,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::time::parse_datetime;

    #[test]
    fn eval_window_honors_the_absolute_window() {
        let start = parse_datetime("2026-07-21 00:00:00").unwrap();
        let end = parse_datetime("2026-07-21 02:00:00").unwrap();
        let options = Options {
            window: Some((start, end)),
            ..Options::default()
        };
        let (got_start, got_end, range, _limits) = eval_window(&options);
        assert_eq!((got_start, got_end), (start, end));
        assert_eq!(range.start_ns, start);
        assert_eq!(range.end_ns, end);
        // 2h span -> ~120 points -> 60s step, and never below 1s.
        assert_eq!(range.step_ns, 60 * 1_000_000_000);
    }
}

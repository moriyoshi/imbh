//! The queries behind a refresh: evaluating the panel query for a screen and the ancillary lookups
//! (trace waterfalls, catalog dimensions, the evaluation window).

use std::time::Duration;

use imbh::{PageCursor, SpanId, Timestamp, TraceId};
use imbh_head::{HeadError, dto};
use imbh_lgtm::{
    EvalLimits, EvalRange, FetchBounds, ImbhQueryModel, LogFetchRequest, LogFilter,
    LogStreamSchema, TranslateContext, build_log_query, translate_logql,
};

use crate::backend::Backend;
use crate::chart::chart_values;
use crate::format::{attrs_to_pairs, format_metric_value, severity_label};
use crate::model::{
    AttrRow, DetailPane, DetailStyle, DimNode, LogCorrelation, LogRecord, Options, PaneTable,
    Screen, SeriesData, Snapshot, TableData,
};
use crate::promql::query_metric_name;
use crate::time::{format_datetime_ns, format_timestamp_ns, humanize_secs};
use crate::ui::glyphs::Glyphs;
use crate::waterfall::{TraceDetail, build_trace_detail};

/// Discover a metric's groupable dimensions — the axes the catalog tree offers to filter and group
/// by. Read straight from the metric tables (see [`Backend::metric_dimensions`]), so it is
/// independent of the selected time range and answers for *every* metric kind.
///
/// It used to evaluate the metric's bare PromQL selector and read the labels off the returned
/// series, which silently discovered nothing for a histogram: PromQL has no bare selector for one
/// (`latency_bucket` is refused — buckets are reachable only through `histogram_quantile(…)`), so
/// every histogram showed up in the tree as "(no dimensions)" and could never be filtered.
///
/// Empty on any failure.
pub(crate) async fn discover_dims(
    backend: &Backend,
    name: &str,
    max_values: usize,
) -> Vec<DimNode> {
    let Ok(dimensions) = backend.metric_dimensions(name, max_values).await else {
        return Vec::new();
    };
    dimensions
        .into_iter()
        .map(|dimension| DimNode {
            label: dimension.label,
            values: dimension.values,
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
        table: None,
        style: DetailStyle::Preview,
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
                // The attribute measurement is a *scan*, not a query: it reads every sealed segment's
                // attribute columns in range. It is therefore its own task
                // (`request_attribute_stats`) filling its own pane, so the gauges above are on screen
                // in milliseconds whatever the corpus costs to measure.
                detail: Some(attribute_placeholder()),
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
                    table: Some(TableData::new(
                        vec![
                            "Metric".to_owned(),
                            "Kind".to_owned(),
                            "Unit".to_owned(),
                            "Temporality".to_owned(),
                        ],
                        rows,
                    )),
                    series: Vec::new(),
                    next_cursor: None,
                });
            }
            // One or more newline-separated PromQL queries (the catalog joins several when multiple
            // metrics are checked; the executor has no `or`, so each runs on its own).
            let sub_queries = query
                .split('\n')
                .map(str::trim)
                .filter(|q| !q.is_empty())
                .map(str::to_owned)
                .collect::<Vec<_>>();
            // All sub-queries in one request. PromQL aggregation drops `__name__` on purpose
            // (`LabelSet::by`/`without` both do, which is Prometheus semantics), so a `sum by (…)`
            // or `histogram_quantile(…)` series cannot say which metric produced it — but each
            // returned series carries the index of the query that produced it, which is what puts
            // the name back below. One round trip and one metric-catalog read for the whole refresh,
            // rather than one of each per checked metric.
            let evaluated = backend
                .promql(&sub_queries, eval_range, limits)
                .await
                .map_err(|error| error.to_string())?;
            // Only a multi-metric selection needs the name put back; a single query (including
            // anything hand-typed) is already unambiguous, and naming it from a guess at its
            // leading identifier would be a claim we cannot make about an arbitrary expression.
            let names = if sub_queries.len() > 1 {
                sub_queries
                    .iter()
                    .map(|q| query_metric_name(q))
                    .collect::<Vec<_>>()
            } else {
                vec![None; sub_queries.len()]
            };
            let series: Vec<(Option<String>, dto::Series)> = evaluated
                .into_iter()
                .map(|item| {
                    // A peer that predates `query_index` sends 0 for every series, which degrades to
                    // the single-query behaviour rather than mislabelling anything.
                    let name = names.get(item.query_index).cloned().flatten();
                    (name, item)
                })
                .collect();
            // Build the summary rows and, in the same pass, retain each series' full
            // `(timestamp_ns, value)` history so the detailed viewer can plot the selected one.
            let mut rows = Vec::with_capacity(series.len());
            let mut series_data = Vec::with_capacity(series.len());
            for (name, item) in &series {
                let named = name
                    .as_deref()
                    .filter(|_| item.labels.iter().all(|label| label.name != "__name__"))
                    .map(|name| format!("__name__={name}"));
                let labels = named
                    .into_iter()
                    .chain(
                        item.labels
                            .iter()
                            .map(|label| format!("{}={}", label.name, label.value)),
                    )
                    .collect::<Vec<_>>()
                    .join(",");
                let labels = if labels.is_empty() {
                    "{}".to_owned()
                } else {
                    labels
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
                        .map_or(&[][..], |(_, series)| series.samples.as_slice())
                        .iter()
                        .map(|sample| sample.value),
                ),
                lines: Vec::new(),
                detail: None,
                list_from: None,
                log_records: Vec::new(),
                table: Some(TableData::new(
                    vec![
                        "Series".to_owned(),
                        "Latest".to_owned(),
                        "Min".to_owned(),
                        "Max".to_owned(),
                        "Points".to_owned(),
                    ],
                    rows,
                )),
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
                table: None,
                style: DetailStyle::Preview,
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

/// The attribute pane before the scan that fills it has landed.
///
/// A *placeholder*, not a spinner: the pane above it is already a complete answer, so this only has to
/// say why this one is empty — rather than imply there is nothing to measure.
pub(crate) fn attribute_placeholder() -> DetailPane {
    DetailPane {
        title: "Attributes".to_owned(),
        lines: vec!["measuring (scanning the sealed segments in range)...".to_owned()],
        waterfall: None,
        table: None,
        style: DetailStyle::Pane,
    }
}

/// The Overview's attribute-statistics block: one line per attribute key, DB-wide, with both
/// verdicts.
///
/// The **DB-wide** unit is the one shown, not a per-table breakdown, because that is the scope the
/// decision is made at — `promote` is one list for the whole database, so a key's coverage and cost
/// have to be measured across every table it appears in. The per-table sigma the report also carries
/// is folded into the two verdict columns rather than given lines of its own.
///
/// Keys are ordered by **cost**, most expensive promoted column first, so the ones worth arguing
/// about are at the top rather than the ones that merely occur most often. The two verdicts sit
/// immediately after the key, ahead of the evidence they are drawn from: a narrow terminal truncates
/// on the right, and truncating the conclusion would leave the block saying nothing.
pub(crate) fn attribute_pane(report: &imbh::attrstats::Report, promoted: &[String]) -> DetailPane {
    let global = &report.global;
    DetailPane {
        title: format!(
            "Attributes: {} key{} over {} sealed segment{}, {} rows{}",
            global.keys.len(),
            if global.keys.len() == 1 { "" } else { "s" },
            global.segments,
            if global.segments == 1 { "" } else { "s" },
            imbh::attrstats::text::num(global.rows),
            if global.keys_sample_rate < 1.0 {
                " (key map sampled)"
            } else {
                ""
            },
        ),
        lines: attribute_notes(report),
        waterfall: None,
        table: Some(attribute_table(report, promoted)),
        style: DetailStyle::Pane,
    }
}

/// The attribute pane's prose: what was measured, and anything the measurement could not cover.
///
/// Above the table rather than below it, because these lines *qualify* the numbers — a truncated
/// measurement that reads like full coverage would be worse than no measurement, and a caveat under a
/// scrolling table is a caveat nobody sees.
pub(crate) fn attribute_notes(report: &imbh::attrstats::Report) -> Vec<String> {
    let global = &report.global;
    // The window is stated, not assumed: it is the pane's own and unrelated to the range the panels
    // beside it are queried over, so a reader given only numbers would assume the wrong one. This is
    // also what the range form is anchored to.
    let mut notes = vec![match report.range {
        Some((start, end)) => format!(
            "range: {} - {}",
            format_datetime_ns(start),
            format_datetime_ns(end),
        ),
        None => "range: all sealed segments".to_owned(),
    }];
    if global.segments == 0 {
        notes.push(
            "No sealed segments in this range. Widen the time range, or flush the writer."
                .to_owned(),
        );
    }
    if report.pending_wal_frames > 0 {
        notes.push(format!(
            "NOT MEASURED: {} unsealed WAL frame(s) - buffered rows are in no segment yet.",
            report.pending_wal_frames
        ));
    }
    for skipped in &report.segments_skipped {
        notes.push(format!("SEGMENT SKIPPED: {skipped}"));
    }
    notes
}

/// A row that spans the table with one label in its first cell.
fn banner_row(columns: usize, label: String) -> Vec<String> {
    let mut row = vec![String::new(); columns];
    row[0] = label;
    row
}

/// The attribute pane's table: the keys of every scan unit, grouped, with both verdicts.
///
/// **`ALL` first, then one section per table.** The DB-wide roll-up leads because that is the scope
/// the decision is made at — `promote` is one list for the whole database, so a key's coverage and
/// cost have to be weighed across every table it appears in, and `p` promotes DB-wide wherever the
/// cursor happens to be. The per-table sections are where the numbers are actually *defined*: sigma's
/// denominator is a table's segment count, so a per-table sigma is the primary measurement and the
/// roll-up's is a best-case over it. They also answer the question the roll-up hides — whether a key
/// is a log attribute, a span attribute, or a metric label.
///
/// Empty tables are left out entirely rather than shown with zeroes; a table with no segments in range
/// has nothing to say and would only lengthen the scroll.
///
/// Within a section, rows are ordered by **cost**, most expensive promoted column first, so the keys
/// worth arguing about are at the top rather than the ones that merely occur most often. The current
/// state and the verdict sit immediately after the key, ahead of the evidence they are drawn from: a
/// narrow terminal drops the rightmost columns, and dropping the conclusion would leave the table
/// saying nothing.
pub(crate) fn attribute_table(report: &imbh::attrstats::Report, promoted: &[String]) -> PaneTable {
    use imbh::attrstats::text::num;

    let header = vec![
        "Key".to_owned(),
        "Scope".to_owned(),
        "on".to_owned(),
        "promote".to_owned(),
        "index@".to_owned(),
        "Cov".to_owned(),
        "Distinct".to_owned(),
        "sigma p50".to_owned(),
        "est B/row".to_owned(),
        "Rows".to_owned(),
    ];
    let columns = header.len();
    let mut rows = Vec::new();
    let mut kinds = Vec::new();
    for (unit, db_wide) in std::iter::once((&report.global, true)).chain(
        report
            .tables
            .iter()
            .filter(|unit| unit.segments > 0)
            .map(|unit| (unit, false)),
    ) {
        // A blank line before every section but the first, so the sections read as separate tables
        // rather than one long list. Not before the first: a leading blank would spend the pane's top
        // row on nothing.
        if !rows.is_empty() {
            rows.push(banner_row(columns, String::new()));
            kinds.push(AttrRow::Blank);
        }
        // Title, then this section's own column names. The header is repeated per section rather than
        // pinned once at the top because each section *is* a table: its coverage is against its own
        // rows and its sigma against its own segments, and a single header floating above all of them
        // would suggest one set of totals.
        rows.push(banner_row(
            columns,
            format!(
                "{} {} {} segment{}, {} rows, {} keys",
                if db_wide { "ALL TABLES" } else { &unit.label },
                if db_wide { "--" } else { "-" },
                unit.segments,
                if unit.segments == 1 { "" } else { "s" },
                num(unit.rows),
                unit.keys.len(),
            ),
        ));
        kinds.push(AttrRow::Section);
        rows.push(header.clone());
        kinds.push(AttrRow::Header);
        for row in unit_rows(report, unit, db_wide, promoted) {
            kinds.push(AttrRow::Key(row[0].clone()));
            rows.push(row);
        }
    }
    PaneTable {
        data: TableData::new(header, rows),
        kinds,
    }
}

/// One scan unit's key rows.
///
/// The two verdict columns are answered **within this unit**. `promote` is DB-wide configuration, so
/// only the roll-up gives a verdict and a per-table row leaves it blank rather than repeating a
/// judgement made elsewhere against different totals. `index@`, by contrast, is only defined per
/// table — sigma's denominator is that table's segment count — so each section answers for itself and
/// the roll-up shows the best case across them.
fn unit_rows(
    report: &imbh::attrstats::Report,
    unit: &imbh::attrstats::UnitReport,
    db_wide: bool,
    promoted: &[String],
) -> Vec<Vec<String>> {
    use imbh::attrstats::AttrScope;
    use imbh::attrstats::text::{est, num};

    let mut keys: Vec<&imbh::attrstats::KeyReport> = unit.keys.iter().collect();
    keys.sort_by(|a, b| {
        b.est_bytes_per_row
            .partial_cmp(&a.est_bytes_per_row)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.name.cmp(&b.name))
    });
    keys.iter()
        .map(|key| {
            vec![
                key.name.clone(),
                key.scope.column().to_owned(),
                if promoted.contains(&key.name) {
                    "yes".to_owned()
                } else {
                    String::new()
                },
                // `promote` covers the record-attribute scope only, so a `resource:`/`scope:` key has
                // no promotion verdict to give — "-" beats implying it could be promoted.
                if db_wide && key.scope == AttrScope::Attributes {
                    report.promote_verdict(key).to_owned()
                } else {
                    "-".to_owned()
                },
                if db_wide {
                    report.index_scale(&key.name)
                } else {
                    report.index_scale_in(unit, &key.name)
                }
                .unwrap_or_else(|| "-".to_owned()),
                format!("{:.1}%", key.coverage(unit.rows) * 100.0),
                est(key.distinct_est, key.values_sample_rate),
                key.sigma
                    .as_ref()
                    .map(|sigma| format!("{:.3}", sigma.p50))
                    .unwrap_or_else(|| "-".to_owned()),
                format!("{:.2}", key.est_bytes_per_row),
                num(key.rows_present),
            ]
        })
        .collect()
}

#[cfg(test)]
mod histogram_catalog {
    //! The Metrics catalog against a database of histograms — the kind whose picker was broken.
    //!
    //! Two cumulative histograms (`latency`, split over two `route` values, and `rtt`), each with
    //! three points so a `rate()` window has something to extrapolate over.

    use std::sync::Arc;

    use imbh::Db;
    use imbh_test_support::otlp::otlp_hist_labeled;

    use super::*;
    use crate::app::App;
    use crate::model::{QueryResult, Route};

    async fn backend() -> Backend {
        let db: Arc<Db> = Db::in_memory().open().expect("open in-memory db");
        for (metric, route) in [("latency", "get"), ("latency", "post"), ("rtt", "get")] {
            db.ingest_otlp_metrics(&otlp_hist_labeled(
                "api",
                metric,
                &[("route", route)],
                &[1.0, 5.0],
                &[
                    (1_000_000_000, &[1, 2, 3][..]),
                    (2_000_000_000, &[2, 4, 6][..]),
                    (3_000_000_000, &[3, 6, 9][..]),
                ],
            ))
            .await
            .expect("ingest metrics");
        }
        Backend::from(db)
    }

    fn options() -> Options {
        Options {
            window: Some((1_000_000_000, 4_000_000_000)),
            ..Options::default()
        }
    }

    /// The catalog tree, with every metric expanded and its dimensions discovered — the state the
    /// user is in when they start checking series.
    async fn catalog(backend: &Backend) -> App {
        let mut app = App::new();
        app.route = Route::Metrics;
        let snapshot = load_snapshot(backend.clone(), Screen::Metrics, "", &options(), None, None)
            .await
            .expect("catalog listing");
        app.apply(QueryResult {
            generation: 0,
            screen: Screen::Metrics,
            result: Ok(snapshot),
        });
        app.build_metric_tree();
        for index in 0..app.metric_tree.len() {
            let name = app.metric_tree[index].name.clone();
            let dims = discover_dims(backend, &name, 1000).await;
            app.apply_metric_dims(&name, dims);
            app.metric_tree[index].expanded = true;
        }
        app
    }

    async fn rows(backend: &Backend, queries: &[String]) -> Vec<String> {
        load_snapshot(
            backend.clone(),
            Screen::Metrics,
            &queries.join("\n"),
            &options(),
            None,
            None,
        )
        .await
        .expect("evaluation")
        .table
        .expect("the series list renders as a table")
        .rows
        .into_iter()
        .map(|row| row[0].clone())
        .collect()
    }

    /// A histogram's dimensions used to be discovered by evaluating its bare selector, which PromQL
    /// refuses for a histogram — so every histogram showed "(no dimensions)" and could not be
    /// filtered at all.
    #[tokio::test]
    async fn a_histogram_offers_its_dimensions_like_any_other_kind() {
        let backend = backend().await;
        let dims = discover_dims(&backend, "latency", 1000).await;
        let axes = dims
            .iter()
            .map(|dim| (dim.label.as_str(), dim.values.clone()))
            .collect::<Vec<_>>();
        assert_eq!(
            axes,
            vec![
                ("route", vec!["get".to_owned(), "post".to_owned()]),
                ("service", vec!["api".to_owned()]),
            ],
            "the resource axis and the data-point attribute are both groupable"
        );
    }

    /// Checking a dimension value must actually narrow the histogram's series, the way it does for a
    /// gauge or a sum.
    #[tokio::test]
    async fn checking_a_dimension_value_filters_a_histogram() {
        let backend = backend().await;
        let mut app = catalog(&backend).await;
        let latency = app
            .metric_tree
            .iter()
            .position(|node| node.name == "latency")
            .expect("latency is catalogued");

        // Unfiltered: one quantile series per label set, not one lump for the whole metric.
        assert_eq!(
            rows(&backend, &[app.metric_node_query(latency, None)]).await,
            vec!["route=get,service=api", "route=post,service=api"],
        );

        // Check `route=get` (what Space on that value row does).
        let route = app.metric_tree[latency]
            .dims
            .as_mut()
            .expect("dimensions discovered")
            .iter_mut()
            .find(|dim| dim.label == "route")
            .expect("the route axis");
        route.selected = Some(0);
        assert_eq!(route.values[0], "get");

        assert_eq!(
            rows(&backend, &app.visualize_queries()).await,
            vec!["route=get,service=api"],
            "the checked value must narrow the histogram, not be ignored"
        );
    }

    /// Selecting several metrics used to collapse to a single indistinguishable row: a histogram's
    /// quantile is an aggregation, so grouping by `le` alone summed every label away, and PromQL
    /// drops `__name__` on aggregation so the concatenated result could not name its metrics either.
    #[tokio::test]
    async fn several_selected_histograms_stay_distinguishable() {
        let backend = backend().await;
        let mut app = catalog(&backend).await;
        for node in &mut app.metric_tree {
            node.whole_selected = true;
        }
        let queries = app.visualize_queries();
        assert_eq!(queries.len(), 2, "both metrics are visualized: {queries:?}");

        assert_eq!(
            rows(&backend, &queries).await,
            vec![
                "__name__=latency,route=get,service=api",
                "__name__=latency,route=post,service=api",
                "__name__=rtt,route=get,service=api",
            ],
            "every underlying series survives, and each says which metric it belongs to"
        );
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

#[cfg(test)]
mod attribute_stats {
    //! The Overview's attribute statistics, against a real on-disk database.
    //!
    //! It needs one: the measurement is defined over *sealed segments*, so unlike every other pane it
    //! cannot be driven from an in-memory `Db`. Two properties are worth holding. The block says what
    //! it could not cover — a database with nothing sealed must report that rather than show an empty
    //! table, which would read as "this database has no attributes". And it arrives **separately**
    //! from the gauges above it, so the Overview is usable while the scan runs.

    use std::sync::Arc;

    use imbh::Db;
    use imbh_test_support::otlp::otlp_log;

    use super::*;
    use crate::app::App;
    use crate::model::{QueryResult, Route};

    /// A directory-backed database with three log records over two services, sealed.
    async fn sealed(dir: &std::path::Path) -> (Arc<Db>, Backend) {
        let db: Arc<Db> = Db::builder(dir).open().expect("open db");
        for (service, body, ts) in [
            ("cart", "checkout failed", 1_000),
            ("cart", "checkout retried", 2_000),
            ("api", "ok", 3_000),
        ] {
            // Record attributes as well as the resource ones, so a scan unit holds more than one key
            // — a section with a single row cannot exercise moving *within* a section.
            db.ingest_otlp_logs(&imbh_test_support::otlp::otlp_rich(
                service,
                body,
                ts,
                9,
                &[("http.route", "/checkout"), ("http.method", "GET")],
            ))
            .await
            .expect("ingest");
        }
        db.flush().await.expect("flush");
        (Arc::clone(&db), Backend::from(db))
    }

    fn options() -> Options {
        Options {
            window: Some((0, 1_000_000_000)),
            ..Options::default()
        }
    }

    async fn overview(backend: &Backend) -> Snapshot {
        load_snapshot(
            backend.clone(),
            Screen::Overview,
            "",
            &options(),
            None,
            None,
        )
        .await
        .expect("overview snapshot")
    }

    /// The Overview answers immediately with the database gauges in one pane and a placeholder in the
    /// other; the measurement is not part of that answer.
    #[tokio::test]
    async fn the_overview_does_not_wait_for_the_measurement() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (db, backend) = sealed(dir.path()).await;

        let snapshot = overview(&backend).await;
        assert!(
            snapshot.lines.iter().any(|line| line.starts_with("logs")),
            "the gauges are answered synchronously: {:?}",
            snapshot.lines
        );
        let pane = snapshot.detail.as_ref().expect("the attribute pane");
        assert_eq!(pane.style, DetailStyle::Pane, "a peer pane, not a preview");
        assert!(
            pane.lines.iter().any(|line| line.contains("measuring")),
            "and it says why it is empty: {:?}",
            pane.lines
        );
        db.close().await.expect("close");
    }

    /// The measurement fills the pane — in either arrival order, and only for the range selection it
    /// was made over.
    #[tokio::test]
    async fn the_measurement_fills_the_pane_whichever_arrives_first() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (db, backend) = sealed(dir.path()).await;
        let report = backend
            .attribute_stats(Some((0, 1_000_000_000)))
            .await
            .expect("measure");
        let measured = attribute_pane(&report, &[]);

        let pane_of = |app: &App| {
            app.snapshot
                .detail
                .as_ref()
                .expect("the attribute pane")
                .clone()
        };
        let pane_lines = |app: &App| pane_of(app).lines;
        let pane_keys = |app: &App| {
            pane_of(app)
                .table
                .map(|table| {
                    table
                        .data
                        .rows
                        .iter()
                        .enumerate()
                        .filter_map(|(index, row)| table.key_at(index).map(|_| row[0].clone()))
                        .collect()
                })
                .unwrap_or_else(Vec::new)
        };

        // Snapshot first, then the measurement — the ordinary order.
        let mut app = App::new();
        app.route = Route::Overview;
        app.apply(QueryResult {
            generation: app.generation,
            screen: Screen::Overview,
            result: Ok(overview(&backend).await),
        });
        app.take_attr_stats(app.attr_key(), measured.clone());
        let composed = pane_lines(&app);
        let keys: Vec<String> = pane_keys(&app);
        assert!(
            !composed.iter().any(|line| line.contains("measuring")),
            "the placeholder is replaced: {composed:?}"
        );
        assert!(
            keys.iter().any(|key| key == "resource:service.name"),
            "{keys:?}"
        );
        assert!(
            app.snapshot
                .lines
                .iter()
                .any(|line| line.starts_with("logs")),
            "the gauges pane is untouched: {:?}",
            app.snapshot.lines
        );
        // A second arrival replaces the pane rather than stacking a second one.
        app.take_attr_stats(app.attr_key(), measured.clone());
        assert_eq!(pane_lines(&app), composed);
        assert_eq!(pane_keys(&app), keys);

        // Measurement first, then the snapshot — an empty database can measure faster than the
        // gauges query answers.
        let mut app = App::new();
        app.route = Route::Overview;
        app.take_attr_stats(app.attr_key(), measured.clone());
        app.apply(QueryResult {
            generation: app.generation,
            screen: Screen::Overview,
            result: Ok(overview(&backend).await),
        });
        assert_eq!(pane_lines(&app), composed, "same result, either order");
        assert_eq!(pane_keys(&app), keys);

        // A measurement over a range the user has left is dropped rather than shown under the
        // current one: the numbers would describe a different window than the pane claims.
        let mut app = App::new();
        app.route = Route::Overview;
        app.apply(QueryResult {
            generation: app.generation,
            screen: Screen::Overview,
            result: Ok(overview(&backend).await),
        });
        let elsewhere = Some((1_000, 2_000));
        assert_ne!(elsewhere, app.attr_key());
        app.take_attr_stats(elsewhere, measured);
        assert!(
            pane_lines(&app)
                .iter()
                .any(|line| line.contains("measuring")),
            "a measurement over another range must not fill the pane: {:?}",
            pane_lines(&app)
        );
        db.close().await.expect("close");
    }

    /// The table is grouped by scan unit: the DB-wide roll-up first (the scope `promote` is decided
    /// at), then one section per table that has segments — which is where sigma is actually defined,
    /// and what answers whether a key is a log attribute or a metric label.
    #[tokio::test]
    async fn the_table_breaks_down_per_scan_unit() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (db, backend) = sealed(dir.path()).await;
        // Metrics as well as logs, so there is more than one table with segments to break down.
        db.ingest_otlp_metrics(&imbh_test_support::otlp::otlp_metrics("cart"))
            .await
            .expect("metrics");
        db.flush().await.expect("flush");

        let report = backend.attribute_stats(None).await.expect("measure");
        let table = attribute_pane(&report, &[]).table.expect("a table");
        let sections: Vec<&str> = table
            .data
            .rows
            .iter()
            .zip(&table.kinds)
            .filter(|(_, kind)| **kind == AttrRow::Section)
            .map(|(row, _)| row[0].as_str())
            .collect();
        assert!(
            sections
                .first()
                .is_some_and(|s| s.starts_with("ALL TABLES")),
            "the roll-up leads, because that is the scope promotion is decided at: {sections:?}"
        );

        // Every section is its own table: a title, then that section's own column names, then its
        // keys. The header is repeated rather than pinned once at the top, because each section's
        // numbers are measured against its own totals.
        for (index, kind) in table.kinds.iter().enumerate() {
            if *kind != AttrRow::Section {
                continue;
            }
            assert_eq!(
                table.kinds.get(index + 1),
                Some(&AttrRow::Header),
                "a title is followed by its own column header: {:?}",
                table.data.rows[index]
            );
            assert_eq!(
                table.data.rows[index + 1],
                table.data.header,
                "and the header row names the columns the rows below it are in"
            );
        }
        assert!(
            sections.iter().any(|s| s.starts_with("logs ")),
            "{sections:?}"
        );
        assert!(
            sections.iter().any(|s| s.starts_with("metrics_gauge ")),
            "a metric label is a different question from a log attribute: {sections:?}"
        );
        assert!(
            !sections.iter().any(|s| s.starts_with("metrics_summary")),
            "a table with no segments has nothing to say and is left out: {sections:?}"
        );

        // Every section names its own segment and row counts — the numbers the sigma in that section
        // is measured against.
        let logs = sections
            .iter()
            .find(|s| s.starts_with("logs "))
            .expect("logs section");
        assert!(logs.contains("segment"), "{logs}");

        // A key present in two signals appears under both, and only the roll-up carries the promote
        // verdict — a per-table row would be judging DB-wide configuration against one table's totals.
        let service_rows: Vec<&Vec<String>> = table
            .data
            .rows
            .iter()
            .enumerate()
            .filter(|(index, _)| table.key_at(*index) == Some("resource:service.name"))
            .map(|(_, row)| row)
            .collect();
        assert!(
            service_rows.len() >= 2,
            "service.name is on logs and on metrics: {} row(s)",
            service_rows.len()
        );
        assert!(
            service_rows.iter().all(|row| row[3] == "-"),
            "a resource-scoped key is never promotable, in any section"
        );
        db.close().await.expect("close");
    }

    /// The sections appear in the same order as the gauges pane above lists the tables.
    ///
    /// Two independent orderings today — `Db::stats()` builds one list and `Report.tables` another —
    /// and both happen to be `Table::ALL`. "Happen to" is the problem: a reader scanning down the
    /// screen matches the two by position, so this pins the agreement rather than leaving it to two
    /// files that do not know about each other.
    #[tokio::test]
    async fn the_sections_are_in_the_same_order_as_the_gauges_above_them() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (db, backend) = sealed(dir.path()).await;
        db.ingest_otlp_metrics(&imbh_test_support::otlp::otlp_metrics("cart"))
            .await
            .expect("metrics");
        db.flush().await.expect("flush");

        let gauges: Vec<String> = backend
            .stats()
            .await
            .expect("stats")
            .tables
            .into_iter()
            .map(|table| table.table)
            .collect();
        let report = backend.attribute_stats(None).await.expect("measure");
        let table = attribute_pane(&report, &[]).table.expect("a table");
        let sections: Vec<String> = table
            .data
            .rows
            .iter()
            .zip(&table.kinds)
            .filter(|(_, kind)| **kind == AttrRow::Section)
            .map(|(row, _)| {
                row[0]
                    .split_whitespace()
                    .next()
                    .unwrap_or_default()
                    .to_owned()
            })
            .collect();

        assert_eq!(sections.first().map(String::as_str), Some("ALL"));
        // The rest is the gauges list with the empty tables left out, in that order.
        let expected: Vec<&String> = gauges
            .iter()
            .filter(|name| sections.iter().any(|section| section == *name))
            .collect();
        let actual: Vec<&String> = sections.iter().skip(1).collect();
        assert_eq!(actual, expected, "gauges: {gauges:?}");
        db.close().await.expect("close");
    }

    /// Each section is its own Tab stop, and the cursor lands only on key rows — a title, a repeated
    /// column header and the spacer between sections are structure, and a cursor on one of them would
    /// offer `p` with nothing to promote. At a section's edge the arrows **hop** to the next section
    /// rather than stopping, so they walk the whole pane; Tab jumps a section at a time.
    #[tokio::test]
    async fn the_cursor_steps_between_keys_and_never_onto_structure() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (db, backend) = sealed(dir.path()).await;
        db.ingest_otlp_metrics(&imbh_test_support::otlp::otlp_metrics("cart"))
            .await
            .expect("metrics");
        db.flush().await.expect("flush");

        let report = backend.attribute_stats(None).await.expect("measure");
        let mut app = App::new();
        app.route = Route::Overview;
        app.focus = crate::model::Focus::AttrTable(0);
        app.apply(QueryResult {
            generation: app.generation,
            screen: Screen::Overview,
            result: Ok(overview(&backend).await),
        });
        app.take_attr_stats(app.attr_key(), attribute_pane(&report, &[]));

        // The measurement lands with the cursor at 0 — a section title — and it is snapped onto the
        // first key of the focused section rather than left there.
        assert!(app.selected_attr_key().is_some(), "snapped off the title");

        // One stop per section, and every section has keys to land on.
        let sections = app.attr_sections();
        assert!(sections.len() > 2, "the roll-up plus logs plus metrics");
        assert_eq!(
            app.focus_ring()
                .iter()
                .filter(|focus| matches!(focus, crate::model::Focus::AttrTable(_)))
                .count(),
            sections.len(),
        );

        for section in 0..sections.len() {
            app.focus = crate::model::Focus::AttrTable(section);
            app.snap_attr_cursor();
            assert!(
                !app.attr_key_indices(section).is_empty(),
                "section has keys"
            );
            assert!(
                app.selected_attr_key().is_some(),
                "Tab put the cursor inside section {section}"
            );
        }

        // Walking down from the top never lands on structure, crosses every section boundary, and
        // stops at the pane's last key rather than wrapping.
        let all_keys: Vec<usize> = (0..sections.len())
            .flat_map(|section| app.attr_key_indices(section))
            .collect();
        app.focus = crate::model::Focus::AttrTable(0);
        app.selected = 0;
        app.snap_attr_cursor();
        assert_eq!(app.selected, all_keys[0]);
        let mut visited = vec![app.selected];
        while app.move_attr_cursor(1) {
            assert!(
                app.selected_attr_key().is_some(),
                "row {} is structure: {:?}",
                app.selected,
                app.snapshot.detail.as_ref().and_then(|pane| pane
                    .table
                    .as_ref()
                    .map(|table| table.data.rows[app.selected].clone()))
            );
            visited.push(app.selected);
        }
        assert_eq!(
            visited, all_keys,
            "the arrows walk the whole pane, in order"
        );
        assert_eq!(
            app.focus,
            crate::model::Focus::AttrTable(sections.len() - 1),
            "and the focus followed the cursor across the boundaries"
        );

        // The same walk back up.
        let mut back = vec![app.selected];
        while app.move_attr_cursor(-1) {
            assert!(app.selected_attr_key().is_some());
            back.push(app.selected);
        }
        back.reverse();
        assert_eq!(back, all_keys);
        assert_eq!(app.focus, crate::model::Focus::AttrTable(0));

        // An overshoot lands on the section's own last key first: the edge is a pause, not a wall.
        // Needs a section with room to overshoot *within*, which a single-key section has not.
        let (wide, keys) = (0..sections.len())
            .map(|section| (section, app.attr_key_indices(section)))
            .find(|(section, keys)| keys.len() > 1 && *section + 1 < sections.len())
            .expect("a multi-key section with another after it");
        app.focus = crate::model::Focus::AttrTable(wide);
        app.selected = keys[0];
        assert!(app.move_attr_cursor(500));
        assert_eq!(app.selected, *keys.last().expect("keys"));
        assert_eq!(
            app.focus,
            crate::model::Focus::AttrTable(wide),
            "an overshoot stops at the section's own end"
        );
        assert!(app.move_attr_cursor(500), "and the next press crosses");
        assert_eq!(app.focus, crate::model::Focus::AttrTable(wide + 1));
        db.close().await.expect("close");
    }

    /// A measurement outlives the refresh that produced it: an auto-refreshing Overview must not
    /// rescan the corpus every few seconds for a number that barely moves. It is invalidated by the
    /// two things that change the answer — the user moving the range, and `r`.
    #[test]
    fn the_measurement_is_reused_across_auto_refresh_ticks() {
        let mut app = App::new();
        assert!(app.needs_attr_measure(), "nothing measured yet");
        app.take_attr_stats(app.attr_key(), attribute_placeholder());
        assert!(
            !app.needs_attr_measure(),
            "an auto-refresh tick over the same range reuses the block"
        );

        // Moving the *pane's* range invalidates it: the numbers would describe a different window.
        app.attr_window = Some((1_000, 2_000));
        assert!(app.needs_attr_measure());
        app.attr_window = None;
        assert!(!app.needs_attr_measure(), "and moving back reuses it again");
        // Moving the *query* range does not: the two windows are unrelated.
        app.abs_window = Some((1_000, 2_000));
        assert!(!app.needs_attr_measure());

        // `r` clears the cache outright — that is how a person asks for the numbers now.
        app.attr_stats = None;
        assert!(app.needs_attr_measure());
    }

    /// The pane's content: a real table of keys with both verdicts, next to the key and ahead of the
    /// evidence, plus the prose that qualifies it.
    #[tokio::test]
    async fn the_pane_carries_a_table_of_keys_with_both_verdicts() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (db, backend) = sealed(dir.path()).await;
        let report = backend
            .attribute_stats(Some((0, 1_000_000_000)))
            .await
            .expect("measure");
        let pane = attribute_pane(&report, &[]);

        // A table, not preformatted text: the columns are data the renderer aligns and styles.
        let table = pane.table.as_ref().expect("a column-aligned table");
        assert_eq!(
            table.data.header,
            vec![
                "Key",
                "Scope",
                "on",
                "promote",
                "index@",
                "Cov",
                "Distinct",
                "sigma p50",
                "est B/row",
                "Rows",
            ],
            "state and verdict sit next to the key, where a narrow terminal cannot drop them"
        );
        let row_for = |table: &PaneTable, key: &str| {
            table
                .data
                .rows
                .iter()
                .enumerate()
                .find(|(index, _)| table.key_at(*index) == Some(key))
                .unwrap_or_else(|| panic!("{key} should be measured: {:?}", table.data.rows))
                .1
                .clone()
        };
        let service = row_for(table, "resource:service.name");
        assert_eq!(service[1], "resource");
        assert_eq!(service[6], "2", "cart and api");
        // `promote` covers the record-attribute scope only, so a resource key has no verdict to give.
        assert_eq!(service[3], "-");
        assert_eq!(service[2], "", "nothing promoted in this fixture");

        // The `on` column reflects the *live* promoted set, so the verdict is read beside the state
        // it would change — and `p` acts on exactly that column.
        let promoted = attribute_pane(&report, &["resource:service.name".to_owned()]);
        let promoted_table = promoted.table.as_ref().expect("a table");
        assert_eq!(row_for(promoted_table, "resource:service.name")[2], "yes");

        // The window is stated, not assumed: it is the pane's own and unrelated to the range the
        // panels beside it are queried over, so a reader given only numbers would assume the wrong
        // one. It is also what the range form is anchored to.
        assert!(
            pane.lines
                .first()
                .is_some_and(|line| line.starts_with("range: ")),
            "the pane leads with the window it measured: {:?}",
            pane.lines
        );
        let unbounded = attribute_pane(
            &backend
                .attribute_stats(None)
                .await
                .expect("measure all of time"),
            &[],
        );
        assert_eq!(
            unbounded.lines.first().map(String::as_str),
            Some("range: all sealed segments"),
            "the default is every segment, and it says so"
        );
        db.close().await.expect("close");
    }

    /// Rows still in the buffer are in no segment. The block says that outright instead of listing
    /// nothing, which would read as "no attributes here".
    #[tokio::test]
    async fn unsealed_rows_are_reported_rather_than_shown_as_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db: Arc<Db> = Db::builder(dir.path()).open().expect("open db");
        db.ingest_otlp_logs(&otlp_log("cart", "still buffered", 1_000))
            .await
            .expect("ingest");
        let backend = Backend::from(Arc::clone(&db));
        let report = backend
            .attribute_stats(Some((0, 1_000_000_000)))
            .await
            .expect("measure");
        let pane = attribute_pane(&report, &[]);
        // In the prose above the table, not in it: a caveat that scrolls away with the rows is a
        // caveat nobody reads.
        assert!(
            pane.lines
                .iter()
                .any(|line| line.contains("NOT MEASURED") && line.contains("WAL")),
            "{:?}",
            pane.lines
        );
        assert!(
            pane.lines
                .iter()
                .any(|line| line.contains("No sealed segments")),
            "{:?}",
            pane.lines
        );
        // One section (the DB-wide roll-up) and no key rows: nothing sealed, so nothing to tabulate.
        let table = pane.table.as_ref().expect("a table");
        assert!(
            !table
                .kinds
                .iter()
                .any(|kind| matches!(kind, AttrRow::Key(_))),
            "{:?}",
            table.data.rows
        );
        db.close().await.expect("close");
    }

    /// An in-memory database has no segments at all. The failure belongs in the block, not in the
    /// panel: the gauges above it are a valid answer and must survive.
    #[tokio::test]
    async fn an_in_memory_backend_keeps_the_overview_and_says_why() {
        let db: Arc<Db> = Db::in_memory().open().expect("open");
        let backend = Backend::from(db);
        let snapshot = overview(&backend).await;
        assert!(
            snapshot.lines.iter().any(|line| line.starts_with("logs")),
            "the gauges still answer: {:?}",
            snapshot.lines
        );
        let error = backend
            .attribute_stats(Some((0, 1_000_000_000)))
            .await
            .expect_err("no directory, no segments");
        assert!(error.to_string().contains("in-memory"), "{error}");
    }
}

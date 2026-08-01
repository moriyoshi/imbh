//! Background fetches dispatched from the event loop.
//!
//! Each `request_*` spawns the work off the event-loop thread and delivers it back as an
//! [`Update`](crate::model::Update), so a slow query never blocks a redraw.

use std::sync::Arc;

use imbh::Db;
use tokio::sync::mpsc;

use crate::app::App;
use crate::completion::LogCompletionRequest;
use crate::fetch::{build_waterfall_detail, discover_dims, load_snapshot};
use crate::model::{ExemplarMarker, Options, QueryResult, Screen, Update};
use crate::promql::metric_name_from_detail;

pub(crate) fn request_refresh(
    app: &mut App,
    db: Arc<Db>,
    mut options: Options,
    sender: mpsc::UnboundedSender<Update>,
) {
    // Drive the effective window from the interactively selected time range rather than the static
    // launch defaults.
    options.lookback = app.lookback();
    options.step = app.step();
    options.window = app.abs_window;
    if app.loading {
        // Keep `log_paging` intact so the coalesced refresh below still sees the paging intent.
        app.pending_refresh = true;
        return;
    }
    // Log paging is coherent only against a fixed query/window (offset cursors shift otherwise), so any
    // refresh that is *not* an explicit older/newer page move drops back to page 0. The paging keys set
    // `log_paging` to carry the cursor stack across this one refresh.
    if app.log_paging {
        app.log_paging = false;
    } else {
        app.log_cursor_stack.clear();
        app.log_next_cursor = None;
    }
    app.loading = true;
    app.generation = app.generation.wrapping_add(1);
    let generation = app.generation;
    let screen = app.screen();
    let query = app.active_query().to_owned();
    let after = app.log_cursor_stack.last().copied();
    let correlation = app.log_correlation.clone();
    tokio::spawn(async move {
        let result = load_snapshot(db, screen, &query, &options, after, correlation).await;
        let _ = sender.send(Update::Query(QueryResult {
            generation,
            screen,
            result,
        }));
    });
}

/// Fetch a metric's dimensions off the event-loop thread and deliver them as `Update::MetricDims`.
/// Discovery is time-range independent, so only the series cap is threaded through.
pub(crate) fn request_metric_dims(
    name: String,
    kind: String,
    db: Arc<Db>,
    max_series: usize,
    sender: mpsc::UnboundedSender<Update>,
) {
    tokio::spawn(async move {
        let dims = discover_dims(&db, &name, &kind, max_series).await;
        let _ = sender.send(Update::MetricDims { metric: name, dims });
    });
}

/// If the completion caret sits in a label position for a metric whose dimensions (the label
/// vocabulary) are not yet discovered, kick off that discovery so the popup can fill in on arrival
/// (`Update::MetricDims` re-runs `refresh_completion`). Fires at most once per metric.
pub(crate) fn maybe_discover_label_dims(
    app: &mut App,
    db: &Arc<Db>,
    options: &Options,
    sender: &mpsc::UnboundedSender<Update>,
) {
    if let Some((name, kind)) = app.completion_dim_request() {
        request_metric_dims(name, kind, db.clone(), options.max_series, sender.clone());
    }
    // The Logs screen's `{…}` selector draws its vocabulary from cross-signal attribute discovery
    // rather than a per-metric tree, so it has its own (analogous) fetch path.
    match app.completion_log_request() {
        Some(LogCompletionRequest::Labels) => request_log_labels(db.clone(), sender.clone()),
        Some(LogCompletionRequest::Values(label)) => {
            request_log_label_values(label, db.clone(), sender.clone())
        }
        None => {}
    }
}

/// Fetch the log label names (cross-signal attribute keys) off the event-loop thread and deliver them
/// as `Update::LogLabels`. This is the Logs `{…}` selector's label-name completion vocabulary.
pub(crate) fn request_log_labels(db: Arc<Db>, sender: mpsc::UnboundedSender<Update>) {
    tokio::spawn(async move {
        let names = db.attrs().names().await.unwrap_or_default();
        let _ = sender.send(Update::LogLabels(names));
    });
}

/// Fetch one log label's distinct values off the event-loop thread and deliver them as
/// `Update::LogLabelValues`. This is the Logs quoted-matcher label-value completion vocabulary.
pub(crate) fn request_log_label_values(
    label: String,
    db: Arc<Db>,
    sender: mpsc::UnboundedSender<Update>,
) {
    tokio::spawn(async move {
        let values = db.attrs().values(&label).await.unwrap_or_default();
        let _ = sender.send(Update::LogLabelValues { label, values });
    });
}

/// Fetch the completion vocabulary for a screen off the event-loop thread. Only the Metrics screen
/// has a dynamic vocabulary (metric names from the catalog); other screens complete against static
/// function/keyword lists and need no fetch.
pub(crate) fn request_vocabulary(
    screen: Screen,
    db: Arc<Db>,
    sender: mpsc::UnboundedSender<Update>,
) {
    if screen != Screen::Metrics {
        return;
    }
    tokio::spawn(async move {
        if let Ok(catalog) = db.metrics().catalog().await {
            let mut names = catalog
                .iter()
                .map(|metric| metric.metric.clone())
                .collect::<Vec<_>>();
            names.sort();
            names.dedup();
            let _ = sender.send(Update::Vocabulary(names));
        }
    });
}

/// If the selected trace differs from the one shown in the waterfall pane, fetch its waterfall off
/// the event-loop thread and deliver it as an `Update::Waterfall`. No-op on non-Traces screens or
/// when the selected trace is already shown/in flight.
pub(crate) fn request_waterfall(
    app: &mut App,
    db: &Arc<Db>,
    sender: &mpsc::UnboundedSender<Update>,
    ascii: bool,
) {
    // A pending log→trace focus wins over the row selection until the cursor moves or the focused
    // trace is found in the list.
    let Some(trace_id) = app
        .focus_trace_id
        .clone()
        .or_else(|| app.selected_trace_id())
    else {
        return;
    };
    if app.detail_trace_id.as_deref() == Some(trace_id.as_str()) {
        return;
    }
    app.detail_trace_id = Some(trace_id.clone());
    // The retained trace belongs to the *previous* selection; drop it so Enter cannot open a detail for
    // a trace the cursor has already left. A fetch is only issued when the selection actually moved, so
    // this is also where a pending Enter intent is abandoned (it was meant for the trace just left).
    app.trace_detail = None;
    app.pending_trace_open = false;
    let generation = app.generation;
    let db = db.clone();
    let sender = sender.clone();
    tokio::spawn(async move {
        let (detail, trace) = build_waterfall_detail(&db, &trace_id, ascii).await;
        let _ = sender.send(Update::Waterfall {
            generation,
            trace_id,
            detail,
            trace,
        });
    });
}

/// Fetch the exemplar→trace markers for the open metric-detail view (the metric's exemplars carrying a
/// trace id, within the plotted window) off the event-loop thread. Clears any previous markers first;
/// a no-op off a metric detail or when the metric name cannot be determined.
pub(crate) fn request_metric_exemplars(
    app: &mut App,
    db: &Arc<Db>,
    sender: &mpsc::UnboundedSender<Update>,
) {
    app.metric_exemplars.clear();
    let Some(detail) = app.route_metric_detail() else {
        return;
    };
    let Some(name) = metric_name_from_detail(detail) else {
        return;
    };
    let (win_start, win_end) = match (detail.points.first(), detail.points.last()) {
        (Some(first), Some(last)) => (first.0.min(last.0), first.0.max(last.0)),
        _ => return,
    };
    let labels = detail.labels.clone();
    let query = detail.query.clone();
    let db = db.clone();
    let sender = sender.clone();
    tokio::spawn(async move {
        let markers: Vec<ExemplarMarker> = match db.metrics().exemplars(&name).await {
            Ok(exemplars) => exemplars
                .into_iter()
                .filter_map(|exemplar| {
                    let trace = exemplar.trace_id?;
                    let time_ns = exemplar.time.0;
                    (win_start..=win_end)
                        .contains(&time_ns)
                        .then(|| ExemplarMarker {
                            time_ns,
                            trace_id: trace.to_hex(),
                        })
                })
                .collect(),
            Err(_) => Vec::new(),
        };
        if !markers.is_empty() {
            let _ = sender.send(Update::Exemplars {
                labels,
                query,
                markers,
            });
        }
    });
}

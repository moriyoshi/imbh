//! Shared fixtures for the unit tests across the crate's modules.

use imbh::{Attributes, Timestamp, TraceId};

use crate::app::App;
use crate::model::{DimNode, LogRecord, MetricNode, Mode, Route, SeriesData, Snapshot, TableData};
use crate::waterfall::build_trace_detail;

/// A flat, ASCII-named trace with more spans than a short pane fits — the fixture for the `--ascii`
/// render sweep over the trace views (their own chrome must stay ASCII, including the µs unit and
/// the preview pane's truncation note).
pub(crate) fn ascii_trace() -> imbh::Trace {
    let spans = (0..10u8)
        .map(|i| waterfall_span(i + 1, None, &format!("span-{i}"), i as i64 * 1_000, 900))
        .collect::<Vec<_>>();
    imbh::Trace {
        trace_id: TraceId([0xaa; 16]),
        root_service: Some("api".to_owned()),
        root_name: Some("root".to_owned()),
        start_time: Timestamp(0),
        duration_ns: imbh::DurationNs(10_000),
        spans,
    }
}

/// A nested trace shaped like a real waterfall: a root, one span beneath it, and sixteen leaves
/// beneath *that* — enough rows to overflow a pane, so scrolling to the leaves pushes both enclosing
/// spans off the top. The two ancestors fit under the sticky depth cap, so both stay pinned (the
/// straight-chain case, where the cap has to drop the outermost, is covered by the pure unit tests).
///
/// The leaf names are deliberately longer than `WATERFALL_NAME_W` while the two ancestors' names fit,
/// so the name column's horizontal scrolling is exercised *and* has somewhere to scroll back to.
/// `root_service`/`root_name` are `None` so the header cannot leak those names into a render
/// assertion about the waterfall rows.
pub(crate) fn nested_trace() -> imbh::Trace {
    // 1: root, 2: the span under it, 3..=18: leaves, all children of 2.
    let mut spans = vec![
        waterfall_span(1, None, "zz-root", 0, 100_000),
        waterfall_span(2, Some(1), "yy-mid", 1_000, 98_000),
    ];
    for id in 3..=18u8 {
        spans.push(waterfall_span(
            id,
            Some(2),
            &format!("work-{}-with-a-long-name", id - 3),
            id as i64 * 1_000,
            90_000 - id as u64 * 1_000,
        ));
    }
    imbh::Trace {
        trace_id: TraceId([0xaa; 16]),
        root_service: None,
        root_name: None,
        start_time: Timestamp(0),
        duration_ns: imbh::DurationNs(100_000),
        spans,
    }
}

pub(crate) fn waterfall_span(
    id: u8,
    parent: Option<u8>,
    name: &str,
    start_ns: i64,
    dur_ns: u64,
) -> imbh::Span {
    imbh::Span {
        trace_id: TraceId([0xaa; 16]),
        span_id: imbh::SpanId([id; 8]),
        parent_span_id: parent.map(|p| imbh::SpanId([p; 8])),
        name: name.to_owned(),
        kind: "internal".to_owned(),
        start_time: Timestamp(start_ns),
        duration_ns: imbh::DurationNs(dur_ns),
        status_code: "OK".to_owned(),
        status_message: None,
        service: None,
        attributes: Attributes::new(),
        resource: Attributes::new(),
        scope: Attributes::new(),
        events: None,
        links: None,
        trace_state: None,
        flags: 0,
    }
}

/// A three-span trace: a root, a nested child, and an orphan (its parent is not in the trace) that
/// carries an error status, attributes, and events — enough to exercise every detail section.
pub(crate) fn sample_trace() -> imbh::Trace {
    let mut child = waterfall_span(2, Some(1), "db.query", 200_000, 400_000);
    child.service = Some("api".to_owned());
    child.attributes = Attributes::from_canonical_json(r#"{"db.system":"postgres"}"#);
    let mut orphan = waterfall_span(3, Some(9), "orphan", 600_000, 100_000);
    orphan.status_code = "ERROR".to_owned();
    orphan.status_message = Some("boom".to_owned());
    orphan.events = Some(r#"[{"name":"exception"}]"#.to_owned());
    imbh::Trace {
        trace_id: TraceId([0xaa; 16]),
        root_service: Some("api".to_owned()),
        root_name: Some("GET /users".to_owned()),
        start_time: Timestamp(0),
        duration_ns: imbh::DurationNs(1_000_000),
        spans: vec![waterfall_span(1, None, "root", 0, 1_000_000), child, orphan],
    }
}

/// An App parked on the Traces list with one result row whose trace is already materialized — the
/// state Enter opens the trace detail from.
pub(crate) fn traces_app_with_trace() -> App {
    let detail = build_trace_detail(&sample_trace(), true);
    let mut app = App::new();
    app.route = Route::Traces;
    app.snapshot = Snapshot {
        lines: vec![
            "1 matching traces".into(),
            format!("{}  ts", detail.trace_id),
        ],
        list_from: Some(1),
        ..Default::default()
    };
    app.selected = 1;
    app.detail_trace_id = Some(detail.trace_id.clone());
    app.trace_detail = Some(detail);
    app
}

pub(crate) fn sample_log_record(trace: Option<&str>) -> LogRecord {
    LogRecord {
        time_ns: 1_609_459_200_000_000_000,
        severity: "INFO (9)".into(),
        service: Some("api".into()),
        body: "hello\nworld".into(),
        trace_id: trace.map(str::to_owned),
        span_id: Some("aabbccdd11223344".into()),
        attributes: vec![("http.method".into(), "GET".into())],
        resource: vec![("service.name".into(), "api".into())],
        scope: vec![],
    }
}

pub(crate) fn catalog_app() -> App {
    let mut app = App::new();
    app.route = Route::Metrics;
    app.query[1] = String::new(); // empty query -> catalog is showing
    app.snapshot.table = Some(TableData {
        header: vec![
            "Metric".into(),
            "Kind".into(),
            "Unit".into(),
            "Temporality".into(),
        ],
        rows: vec![
            vec!["cpu".into(), "gauge".into(), "1".into(), "-".into()],
            vec!["reqs".into(), "sum".into(), "1".into(), "cumulative".into()],
        ],
    });
    app.build_metric_tree();
    app
}

pub(crate) fn metrics_app_with_series() -> App {
    let mut app = App::new();
    app.route = Route::Metrics;
    app.query[1] = "up".to_owned(); // non-empty query -> series view (not the catalog)
    app.snapshot.table = Some(TableData {
        header: vec!["Series".into(), "Latest".into()],
        rows: vec![vec!["a".into()], vec!["b".into()]],
    });
    app.snapshot.series = vec![
        SeriesData {
            labels: "svc=a".into(),
            points: vec![(10, 1.0), (20, 2.0)],
        },
        SeriesData {
            labels: "svc=b".into(),
            points: vec![(10, 3.0), (20, 4.0), (30, 5.0)],
        },
    ];
    app
}

pub(crate) fn app_with_discovered_dims() -> App {
    let mut app = App::new();
    app.route = Route::Metrics;
    app.mode = Mode::Editing;
    app.metric_names = vec!["http_requests_total".to_owned()];
    app.metric_tree = vec![MetricNode {
        name: "http_requests_total".to_owned(),
        kind: "sum".to_owned(),
        unit: String::new(),
        temporality: String::new(),
        expanded: false,
        whole_selected: false,
        dims: Some(vec![
            DimNode {
                label: "service".to_owned(),
                values: vec!["cart".to_owned(), "checkout".to_owned()],
                expanded: false,
                selected: None,
            },
            DimNode {
                label: "host".to_owned(),
                values: vec!["node-a".to_owned()],
                expanded: false,
                selected: None,
            },
        ]),
        loading: false,
    }];
    app
}

/// A Logs app in edit mode with a discovered label vocabulary (label names + one label's values),
/// mirroring `app_with_discovered_dims` but for the Logs screen's cross-signal attribute source.
pub(crate) fn logs_app_with_labels() -> App {
    let mut app = App::new();
    app.route = Route::Logs;
    app.mode = Mode::Editing;
    app.log_labels = Some(vec![
        "service.name".to_owned(),
        "http.method".to_owned(),
        "host".to_owned(),
    ]);
    app.log_label_values.insert(
        "service.name".to_owned(),
        vec!["cart".to_owned(), "checkout".to_owned()],
    );
    app
}

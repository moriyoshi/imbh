//! The read-only tool surface an MCP client sees.
//!
//! Each tool is a thin wrapper over one imbh library call (ARCHITECTURE.md §10.5–§10.9): the typed
//! log/trace/metric query builders, attribute discovery, DB stats, and raw SQL. Nothing here can
//! write — no ingest, no flush/compact, no retention — so the endpoint is safe to expose to an agent
//! that is only meant to *look* at telemetry.
//!
//! Every tool answers with a JSON document in a single `text` content block. Argument problems and
//! query failures come back as tool-execution errors (`isError: true`) rather than JSON-RPC errors,
//! because those are the ones a model can act on and retry (MCP `server/tools` — Error Handling).

use std::sync::Arc;
use std::time::Duration;

use imbh::{
    Aggregation, Db, Direction, HistogramQuery, LogEntry, LogQuery, MetricQuery, SeverityNumber,
    SpanMetricsQuery, TimeRange, Timestamp, TraceId, TraceQuery, parse_duration,
};

use super::json::{Args, Arr, Obj, any_value, attributes, labels, number};
use crate::{batches_to_json, offload, stats_json};

/// One tool as `tools/list` describes it.
pub(crate) struct Tool {
    pub(crate) name: &'static str,
    pub(crate) title: &'static str,
    pub(crate) description: &'static str,
    /// The tool's JSON Schema, verbatim. Written by hand (this crate has no serde) and covered by a
    /// test that parses every one of them.
    pub(crate) input_schema: &'static str,
}

/// The tool table, in a stable order — MCP asks for deterministic ordering so clients can cache the
/// list and model prompt caches stay warm (`server/tools` — Capabilities).
pub(crate) const TOOLS: &[Tool] = &[
    Tool {
        name: "query_sql",
        title: "Run SQL",
        description: "Run a read-only SQL query against the imbh database and return JSON rows. \
             Tables: `logs`, `spans`, `metrics_gauge`, `metrics_sum`, `metrics_histogram`, \
             `metrics_exp_histogram`, `metrics_summary`. Timestamps are epoch-nanosecond integers. \
             Attribute maps are JSON columns — read them with \
             `json_get_str(attributes, 'http.route')`. Use this when \
             no purpose-built tool fits; prefer the typed tools otherwise, since they apply the \
             indexes.",
        input_schema: r#"{"type":"object","properties":{
            "sql":{"type":"string","description":"The SQL query to run."},
            "max_rows":{"type":"integer","minimum":1,"description":"Row cap for the response (default 200, max 5000). Rows beyond it are dropped and `truncated` is set."}
        },"required":["sql"]}"#,
    },
    Tool {
        name: "search_logs",
        title: "Search logs",
        description: "Search log records by service, severity, full-text terms, and attributes, \
             newest first by default. `matches` is a tokenized term-AND full-text search accelerated \
             by the Tantivy index and is the cheapest way to find a phrase in log bodies; \
             `body_contains` is a literal substring match with no index behind it.",
        input_schema: r#"{"type":"object","properties":{
            "service":{"type":"string","description":"Exact `service.name` to match."},
            "severity_at_least":{"type":["string","integer"],"description":"Minimum severity: trace|debug|info|warn|error|fatal, or an OTel severity number (1-24)."},
            "matches":{"type":"string","description":"Full-text term-AND match over the log body (index-accelerated)."},
            "body_contains":{"type":"string","description":"Literal substring the body must contain."},
            "attributes":{"type":"object","description":"Attribute equality filters, e.g. {\"http.route\":\"/cart\"}. Matches log, resource, or scope attributes."},
            "trace_id":{"type":"string","description":"32-char hex trace id; returns only logs correlated with that trace."},
            "since":{"type":"string","description":"Look-back window ending now, e.g. \"15m\", \"2h\", \"7d\" (default \"1h\"). Ignored when start_unix_nano/end_unix_nano are given."},
            "start_unix_nano":{"type":"integer","description":"Absolute window start, epoch nanoseconds (inclusive)."},
            "end_unix_nano":{"type":"integer","description":"Absolute window end, epoch nanoseconds (exclusive)."},
            "limit":{"type":"integer","minimum":1,"description":"Maximum entries to return (default 50, max 1000)."},
            "direction":{"type":"string","enum":["backward","forward"],"description":"backward = newest first (default); forward = oldest first."}
        },"additionalProperties":false}"#,
    },
    Tool {
        name: "count_logs",
        title: "Count logs",
        description: "Count the log records matching a filter, without materializing any of them. \
             Takes the same filter arguments as `search_logs` (minus `limit`/`direction`). Use it to \
             size a query before running it, or to answer \"how many errors\" directly.",
        input_schema: r#"{"type":"object","properties":{
            "service":{"type":"string"},
            "severity_at_least":{"type":["string","integer"],"description":"trace|debug|info|warn|error|fatal, or an OTel severity number (1-24)."},
            "matches":{"type":"string","description":"Full-text term-AND match over the log body."},
            "body_contains":{"type":"string"},
            "attributes":{"type":"object","description":"Attribute equality filters."},
            "trace_id":{"type":"string"},
            "since":{"type":"string","description":"Look-back window ending now (default \"1h\")."},
            "start_unix_nano":{"type":"integer"},
            "end_unix_nano":{"type":"integer"}
        },"additionalProperties":false}"#,
    },
    Tool {
        name: "log_volume",
        title: "Log volume over time",
        description: "Count matching log records per time bucket — the shape you want for \"is this \
             spiking?\". Optionally break the volume down by attribute keys, which yields one series \
             per label set. Takes the same filter arguments as `search_logs`.",
        input_schema: r#"{"type":"object","properties":{
            "service":{"type":"string"},
            "severity_at_least":{"type":["string","integer"],"description":"trace|debug|info|warn|error|fatal, or an OTel severity number (1-24)."},
            "matches":{"type":"string"},
            "body_contains":{"type":"string"},
            "attributes":{"type":"object"},
            "trace_id":{"type":"string"},
            "since":{"type":"string","description":"Look-back window ending now (default \"1h\")."},
            "start_unix_nano":{"type":"integer"},
            "end_unix_nano":{"type":"integer"},
            "step":{"type":"string","description":"Bucket width, e.g. \"1m\" (default), \"5m\", \"1h\"."},
            "group_by":{"type":"array","items":{"type":"string"},"description":"Record attribute keys to break the volume down by, e.g. [\"http.route\"]. These are attributes, not `service.name` — to split by service, call once per service with the `service` filter."}
        },"additionalProperties":false}"#,
    },
    Tool {
        name: "search_traces",
        title: "Search traces",
        description: "Find traces by service, span name, status, duration, and attributes. Returns \
             one summary per trace (root service/name, start, duration, span count, error flag) — \
             call `get_trace` with a returned `trace_id` for the spans.",
        input_schema: r#"{"type":"object","properties":{
            "service":{"type":"string","description":"Exact `service.name` on any span in the trace."},
            "name":{"type":"string","description":"Exact span name to match."},
            "status":{"type":"string","description":"Span status code, e.g. \"ERROR\", \"OK\", \"UNSET\"."},
            "kind":{"type":"string","description":"Span kind, e.g. \"SERVER\", \"CLIENT\", \"INTERNAL\", \"PRODUCER\", \"CONSUMER\"."},
            "matches":{"type":"string","description":"Full-text term-AND match over span names/attributes."},
            "min_duration":{"type":"string","description":"Only traces at least this long, e.g. \"500ms\", \"2s\"."},
            "max_duration":{"type":"string","description":"Only traces no longer than this."},
            "attributes":{"type":"object","description":"Span-attribute equality filters, e.g. {\"http.status_code\":\"500\"}."},
            "since":{"type":"string","description":"Look-back window ending now (default \"1h\")."},
            "start_unix_nano":{"type":"integer"},
            "end_unix_nano":{"type":"integer"},
            "limit":{"type":"integer","minimum":1,"description":"Maximum traces to return (default 20, max 200)."}
        },"additionalProperties":false}"#,
    },
    Tool {
        name: "get_trace",
        title: "Get a trace",
        description: "Fetch every span of one trace by id, with attributes, status, and parent \
             links, ordered as stored. Returns `found: false` when the trace is outside retention or \
             was never ingested.",
        input_schema: r#"{"type":"object","properties":{
            "trace_id":{"type":"string","description":"32-character hex trace id."}
        },"required":["trace_id"]}"#,
    },
    Tool {
        name: "span_metrics",
        title: "RED metrics for spans",
        description: "Rate/errors/duration per time bucket, computed from spans: call count, error \
             count, error rate, and p50/p95/p99 latency in nanoseconds. Group by attribute keys \
             (e.g. `http.route`) for one series per label set. This is the fastest way to find what \
             is slow or failing.",
        input_schema: r#"{"type":"object","properties":{
            "service":{"type":"string"},
            "name":{"type":"string","description":"Exact span name."},
            "kind":{"type":"string"},
            "status":{"type":"string"},
            "attributes":{"type":"object","description":"Span-attribute equality filters."},
            "group_by":{"type":"array","items":{"type":"string"},"description":"Span attribute keys to group series by, e.g. [\"http.route\"]. These are attributes, not `service.name` — to split by service, call once per service with the `service` filter."},
            "since":{"type":"string","description":"Look-back window ending now (default \"1h\")."},
            "start_unix_nano":{"type":"integer"},
            "end_unix_nano":{"type":"integer"},
            "step":{"type":"string","description":"Bucket width (default \"1m\")."}
        },"additionalProperties":false}"#,
    },
    Tool {
        name: "list_metrics",
        title: "List metrics",
        description: "List every metric name in the database with its kind (gauge, sum, histogram, \
             exponential histogram), unit, and aggregation temporality. Start here before querying a \
             metric — the `kind` decides which query tool applies.",
        input_schema: r#"{"type":"object","additionalProperties":false}"#,
    },
    Tool {
        name: "metric_series",
        title: "List a metric's series",
        description: "List the distinct label sets (series) reported for one metric, so you know \
             what you can filter or group by.",
        input_schema: r#"{"type":"object","properties":{
            "metric":{"type":"string","description":"Metric name, as returned by `list_metrics`."}
        },"required":["metric"]}"#,
    },
    Tool {
        name: "query_metric_range",
        title: "Query a metric over time",
        description: "Aggregate a gauge or sum metric into a time series, one value per `step` \
             bucket. Set `rate` for the per-second rate of a counter (sum metrics). Returns one \
             series per group-by label set.",
        input_schema: r#"{"type":"object","properties":{
            "metric":{"type":"string","description":"Metric name, as returned by `list_metrics`."},
            "kind":{"type":"string","enum":["gauge","sum"],"description":"Which metric table to read (default \"gauge\"). Use `list_metrics` to check."},
            "aggregation":{"type":"string","enum":["sum","avg","min","max","count"],"description":"How to combine points inside a bucket. Default: avg for gauge, sum for sum."},
            "group_by":{"type":"array","items":{"type":"string"},"description":"Label keys to group series by."},
            "labels":{"type":"object","description":"Label equality filters, e.g. {\"service.name\":\"cart\"}."},
            "rate":{"type":"boolean","description":"Return the per-second rate of the (monotonic) counter instead of its raw value."},
            "since":{"type":"string","description":"Look-back window ending now (default \"1h\")."},
            "start_unix_nano":{"type":"integer"},
            "end_unix_nano":{"type":"integer"},
            "step":{"type":"string","description":"Bucket width (default \"1m\")."}
        },"required":["metric"]}"#,
    },
    Tool {
        name: "query_metric_instant",
        title: "Query a metric's latest value",
        description: "One value per series for a gauge or sum metric — the latest bucket in the \
             window. The instant-query counterpart of `query_metric_range`.",
        input_schema: r#"{"type":"object","properties":{
            "metric":{"type":"string"},
            "kind":{"type":"string","enum":["gauge","sum"],"description":"Which metric table to read (default \"gauge\")."},
            "aggregation":{"type":"string","enum":["sum","avg","min","max","count"]},
            "group_by":{"type":"array","items":{"type":"string"}},
            "labels":{"type":"object","description":"Label equality filters."},
            "rate":{"type":"boolean","description":"Per-second rate of a counter instead of its raw value."},
            "since":{"type":"string","description":"Look-back window ending now (default \"5m\")."},
            "start_unix_nano":{"type":"integer"},
            "end_unix_nano":{"type":"integer"},
            "step":{"type":"string","description":"Bucket width used to compute the instant value (default \"1m\")."}
        },"required":["metric"]}"#,
    },
    Tool {
        name: "histogram_quantile",
        title: "Histogram quantile",
        description: "Quantile (e.g. p95 latency) over time from a histogram metric. Set \
             `exponential` for an exponential-histogram metric — `list_metrics` reports which kind a \
             metric is.",
        input_schema: r#"{"type":"object","properties":{
            "metric":{"type":"string","description":"Histogram metric name."},
            "quantile":{"type":"number","minimum":0,"maximum":1,"description":"Quantile in [0,1] (default 0.95)."},
            "exponential":{"type":"boolean","description":"Set for an exponential histogram (kind \"exp_histogram\") rather than an explicit-bucket one."},
            "group_by":{"type":"array","items":{"type":"string"}},
            "labels":{"type":"object","description":"Label equality filters."},
            "since":{"type":"string","description":"Look-back window ending now (default \"1h\")."},
            "start_unix_nano":{"type":"integer"},
            "end_unix_nano":{"type":"integer"},
            "step":{"type":"string","description":"Bucket width (default \"1m\")."}
        },"required":["metric"]}"#,
    },
    Tool {
        name: "list_attribute_keys",
        title: "List attribute keys",
        description: "List every attribute key present on any signal (logs, spans, metrics), plus \
             `service.name`. Use it to discover what you can filter or group by.",
        input_schema: r#"{"type":"object","additionalProperties":false}"#,
    },
    Tool {
        name: "list_attribute_values",
        title: "List attribute values",
        description: "List the distinct string values of one attribute key across every signal — \
             e.g. the set of `service.name`s, or every `http.route` seen.",
        input_schema: r#"{"type":"object","properties":{
            "key":{"type":"string","description":"Attribute key, e.g. \"service.name\"."}
        },"required":["key"]}"#,
    },
    Tool {
        name: "db_stats",
        title: "Database stats",
        description: "Operational stats for the database: per-table segment/row counts and time \
             span, buffered rows, WAL bytes, and the durable LSN. Use it to see what data exists and \
             over what period before querying.",
        input_schema: r#"{"type":"object","additionalProperties":false}"#,
    },
];

/// Run one tool. `None` means the tool name is unknown, which is a *protocol* error (`-32602`), not
/// a tool-execution error. `Some(Err(message))` is a tool-execution error the model should see.
pub(crate) async fn call(db: &Arc<Db>, name: &str, args: &Args) -> Option<Result<String, String>> {
    Some(match name {
        "query_sql" => query_sql(db, args).await,
        "search_logs" => search_logs(db, args).await,
        "count_logs" => count_logs(db, args).await,
        "log_volume" => log_volume(db, args).await,
        "search_traces" => search_traces(db, args).await,
        "get_trace" => get_trace(db, args).await,
        "span_metrics" => span_metrics(db, args).await,
        "list_metrics" => list_metrics(db).await,
        "metric_series" => metric_series(db, args).await,
        "query_metric_range" => query_metric(db, args, false).await,
        "query_metric_instant" => query_metric(db, args, true).await,
        "histogram_quantile" => histogram_quantile(db, args).await,
        "list_attribute_keys" => list_attribute_keys(db).await,
        "list_attribute_values" => list_attribute_values(db, args).await,
        "db_stats" => db_stats(db).await,
        _ => return None,
    })
}

// ── shared argument handling ────────────────────────────────────────────────────────────────────

/// Resolve a tool's time window: explicit epoch-nanosecond bounds win, then `since`, then the
/// tool's default look-back. An unbounded query is deliberately not reachable from an argument —
/// every tool answers over a window.
fn window(args: &Args, default_since: &str) -> Result<TimeRange, String> {
    let start = args.i64("start_unix_nano")?;
    let end = args.i64("end_unix_nano")?;
    if start.is_some() || end.is_some() {
        return Ok(TimeRange::between(
            Timestamp(start.unwrap_or(i64::MIN)),
            Timestamp(end.unwrap_or(i64::MAX)),
        ));
    }
    let since = match args.duration("since")? {
        Some(d) => d,
        // The defaults are compile-time literals from this file, so a parse failure here is a bug in
        // the table above, not user input.
        None => parse_duration(default_since).map_err(|e| e.to_string())?,
    };
    Ok(TimeRange::since(since))
}

fn step(args: &Args, default: &str) -> Result<Duration, String> {
    match args.duration("step")? {
        Some(d) if d.is_zero() => Err("argument `step` must be greater than zero".to_owned()),
        Some(d) => Ok(d),
        None => parse_duration(default).map_err(|e| e.to_string()),
    }
}

/// Parse a severity as either an OTel severity number or one of the six named levels.
fn severity(args: &Args, key: &str) -> Result<Option<SeverityNumber>, String> {
    if let Some(n) = args.i64(key).unwrap_or(None) {
        return match u8::try_from(n) {
            Ok(n) if (1..=24).contains(&n) => Ok(Some(SeverityNumber(n))),
            _ => Err(format!(
                "argument `{key}` must be an OTel severity number in 1..=24, got {n}"
            )),
        };
    }
    let Some(name) = args.str(key)? else {
        return Ok(None);
    };
    match name.to_ascii_lowercase().as_str() {
        "trace" => Ok(Some(SeverityNumber::TRACE)),
        "debug" => Ok(Some(SeverityNumber::DEBUG)),
        "info" => Ok(Some(SeverityNumber::INFO)),
        "warn" | "warning" => Ok(Some(SeverityNumber::WARN)),
        "error" => Ok(Some(SeverityNumber::ERROR)),
        "fatal" | "critical" => Ok(Some(SeverityNumber::FATAL)),
        other => Err(format!(
            "argument `{key}` must be trace|debug|info|warn|error|fatal or a severity number, got {other:?}"
        )),
    }
}

fn trace_id(hex: &str) -> Result<TraceId, String> {
    TraceId::from_hex(hex).ok_or_else(|| {
        format!(
            "`{hex}` is not a trace id: expected 32 hexadecimal characters, got {} characters",
            hex.len()
        )
    })
}

/// The filter half of a log query — shared by `search_logs`, `count_logs`, and `log_volume` so the
/// three cannot drift apart.
fn log_filter(args: &Args) -> Result<LogQuery, String> {
    let mut q = LogQuery::new().range(window(args, "1h")?);
    if let Some(service) = args.str("service")? {
        q = q.service(service);
    }
    if let Some(sev) = severity(args, "severity_at_least")? {
        q = q.severity_at_least(sev);
    }
    if let Some(text) = args.str("matches")? {
        q = q.matches(text);
    }
    if let Some(text) = args.str("body_contains")? {
        q = q.string_predicate(
            imbh::LogStringField::Body,
            imbh::StringPredicate::Contains,
            text,
        );
    }
    if let Some(id) = args.str("trace_id")? {
        q = q.trace_id(trace_id(id)?);
    }
    for (k, v) in args.string_map("attributes")? {
        q = q.attr_eq(&k, &v);
    }
    Ok(q)
}

// ── tools ───────────────────────────────────────────────────────────────────────────────────────

async fn query_sql(db: &Arc<Db>, args: &Args) -> Result<String, String> {
    let sql = args.req_str("sql")?;
    let max_rows = args.limit("max_rows", 200, 5000)?;

    let batches = offload(db.sql(sql).collect())
        .await
        .map_err(|e| e.to_string())?;

    // Truncate by slicing rather than by rewriting the SQL: wrapping a caller's query in a
    // `LIMIT` subselect changes its meaning (and breaks on statements that already have one).
    let total: usize = batches.iter().map(|b| b.num_rows()).sum();
    let mut kept = Vec::new();
    let mut remaining = max_rows;
    for batch in &batches {
        if remaining == 0 {
            break;
        }
        if batch.num_rows() <= remaining {
            remaining -= batch.num_rows();
            kept.push(batch.clone());
        } else {
            kept.push(batch.slice(0, remaining));
            remaining = 0;
        }
    }
    let rows = String::from_utf8(batches_to_json(&kept)).map_err(|e| e.to_string())?;

    Ok(Obj::new()
        .raw("rows", &rows)
        .uint("row_count", total.min(max_rows) as u64)
        .bool("truncated", total > max_rows)
        .finish())
}

async fn search_logs(db: &Arc<Db>, args: &Args) -> Result<String, String> {
    let mut q = log_filter(args)?.limit(args.limit("limit", 50, 1000)?);
    if let Some(direction) = args.str("direction")? {
        q = q.direction(match direction {
            "backward" => Direction::Backward,
            "forward" => Direction::Forward,
            other => {
                return Err(format!(
                    "argument `direction` must be \"backward\" or \"forward\", got {other:?}"
                ));
            }
        });
    }

    let page = offload(db.logs().query(q))
        .await
        .map_err(|e| e.to_string())?;

    let mut entries = Arr::new();
    for entry in &page.entries {
        entries.raw(&log_entry_json(entry));
    }
    let stats = Obj::new()
        .uint("rows_scanned", page.stats.rows_scanned)
        .uint("segments_scanned", page.stats.segments_scanned)
        .uint("segments_pruned", page.stats.segments_pruned)
        .uint("elapsed_ns", page.stats.elapsed.0)
        .bool("used_index", page.stats.used_index)
        .finish();
    Ok(Obj::new()
        .raw("entries", &entries.finish())
        .uint("entry_count", page.entries.len() as u64)
        // The cursor itself is opaque and not resumable across calls here, so report only whether
        // more rows exist — a model reading `has_more` will narrow its window or raise the limit.
        .bool("has_more", page.next.is_some())
        .raw("stats", &stats)
        .finish())
}

async fn count_logs(db: &Arc<Db>, args: &Args) -> Result<String, String> {
    let count = offload(db.logs().count(log_filter(args)?))
        .await
        .map_err(|e| e.to_string())?;
    Ok(Obj::new().uint("count", count).finish())
}

async fn log_volume(db: &Arc<Db>, args: &Args) -> Result<String, String> {
    let filter = log_filter(args)?;
    let step = step(args, "1m")?;
    let group_by = args.string_list("group_by")?;
    let keys: Vec<&str> = group_by.iter().map(|k| k.as_str()).collect();

    let buckets = offload(db.logs().volume_by(filter, step, &keys))
        .await
        .map_err(|e| e.to_string())?;

    let mut out = Arr::new();
    for bucket in &buckets {
        let mut obj = Obj::new();
        obj.int("time_unix_nano", bucket.time.0);
        if !bucket.labels.is_empty() {
            obj.raw("labels", &labels(&bucket.labels));
        }
        out.raw(&obj.uint("count", bucket.count).finish());
    }
    Ok(Obj::new()
        .uint("step_ns", step.as_nanos().min(u64::MAX as u128) as u64)
        .raw("buckets", &out.finish())
        .finish())
}

async fn search_traces(db: &Arc<Db>, args: &Args) -> Result<String, String> {
    let mut q = TraceQuery::new()
        .range(window(args, "1h")?)
        .limit(args.limit("limit", 20, 200)?);
    if let Some(service) = args.str("service")? {
        q = q.service(service);
    }
    if let Some(name) = args.str("name")? {
        q = q.name(name);
    }
    if let Some(status) = args.str("status")? {
        q = q.status(status);
    }
    if let Some(kind) = args.str("kind")? {
        q = q.kind(kind);
    }
    if let Some(text) = args.str("matches")? {
        q = q.matches(text);
    }
    if let Some(d) = args.duration("min_duration")? {
        q = q.min_duration(d);
    }
    if let Some(d) = args.duration("max_duration")? {
        q = q.max_duration(d);
    }
    for (k, v) in args.string_map("attributes")? {
        q = q.attr_eq(&k, &v);
    }

    let traces = offload(db.traces().search(q))
        .await
        .map_err(|e| e.to_string())?;

    let mut out = Arr::new();
    for t in &traces {
        out.raw(
            &Obj::new()
                .str("trace_id", &t.trace_id.to_hex())
                .opt_str("root_service", t.root_service.as_deref())
                .opt_str("root_name", t.root_name.as_deref())
                .int("start_time_unix_nano", t.start_time.0)
                .uint("duration_ns", t.duration_ns.0)
                .uint("span_count", t.span_count)
                .bool("error", t.error)
                .finish(),
        );
    }
    Ok(Obj::new()
        .raw("traces", &out.finish())
        .uint("trace_count", traces.len() as u64)
        .finish())
}

async fn get_trace(db: &Arc<Db>, args: &Args) -> Result<String, String> {
    let id = trace_id(args.req_str("trace_id")?)?;
    let trace = offload(db.traces().get(id))
        .await
        .map_err(|e| e.to_string())?;

    let Some(trace) = trace else {
        return Ok(Obj::new()
            .bool("found", false)
            .str("trace_id", &id.to_hex())
            .finish());
    };

    let mut spans = Arr::new();
    for s in &trace.spans {
        let mut obj = Obj::new();
        obj.str("span_id", &s.span_id.to_hex())
            .opt_str(
                "parent_span_id",
                s.parent_span_id.map(|p| p.to_hex()).as_deref(),
            )
            .str("name", &s.name)
            .str("kind", &s.kind)
            .opt_str("service", s.service.as_deref())
            .int("start_time_unix_nano", s.start_time.0)
            .uint("duration_ns", s.duration_ns.0)
            .str("status_code", &s.status_code)
            .opt_str("status_message", s.status_message.as_deref());
        if !s.attributes.is_empty() {
            obj.raw("attributes", &attributes(&s.attributes));
        }
        if !s.resource.is_empty() {
            obj.raw("resource", &attributes(&s.resource));
        }
        embed_json(&mut obj, "events", s.events.as_deref());
        embed_json(&mut obj, "links", s.links.as_deref());
        spans.raw(&obj.finish());
    }

    Ok(Obj::new()
        .bool("found", true)
        .str("trace_id", &trace.trace_id.to_hex())
        .opt_str("root_service", trace.root_service.as_deref())
        .opt_str("root_name", trace.root_name.as_deref())
        .int("start_time_unix_nano", trace.start_time.0)
        .uint("duration_ns", trace.duration_ns.0)
        .raw("spans", &spans.finish())
        .uint("span_count", trace.spans.len() as u64)
        .finish())
}

async fn span_metrics(db: &Arc<Db>, args: &Args) -> Result<String, String> {
    let mut q = SpanMetricsQuery::new()
        .range(window(args, "1h")?)
        .step(step(args, "1m")?);
    if let Some(service) = args.str("service")? {
        q = q.service(service);
    }
    if let Some(name) = args.str("name")? {
        q = q.name(name);
    }
    if let Some(kind) = args.str("kind")? {
        q = q.kind(kind);
    }
    if let Some(status) = args.str("status")? {
        q = q.status(status);
    }
    for (k, v) in args.string_map("attributes")? {
        q = q.attr_eq(&k, &v);
    }
    for key in args.string_list("group_by")? {
        q = q.group_by(&key);
    }

    let metrics = offload(db.traces().span_metrics(q))
        .await
        .map_err(|e| e.to_string())?;

    let mut series = Arr::new();
    for s in &metrics.0 {
        let mut points = Arr::new();
        for p in &s.points {
            points.raw(
                &Obj::new()
                    .int("time_unix_nano", p.time.0)
                    .uint("calls", p.calls)
                    .uint("errors", p.errors)
                    .float("error_rate", p.error_rate)
                    .float("p50_ns", p.p50_ns)
                    .float("p95_ns", p.p95_ns)
                    .float("p99_ns", p.p99_ns)
                    .finish(),
            );
        }
        series.raw(
            &Obj::new()
                .raw("labels", &labels(&s.labels))
                .raw("points", &points.finish())
                .finish(),
        );
    }
    Ok(Obj::new().raw("series", &series.finish()).finish())
}

async fn list_metrics(db: &Arc<Db>) -> Result<String, String> {
    let catalog = offload(db.metrics().catalog())
        .await
        .map_err(|e| e.to_string())?;
    let mut out = Arr::new();
    for m in &catalog {
        out.raw(
            &Obj::new()
                .str("metric", &m.metric)
                .str("kind", &m.kind)
                .str("unit", &m.unit)
                .opt_str("temporality", m.temporality.as_deref())
                .finish(),
        );
    }
    Ok(Obj::new().raw("metrics", &out.finish()).finish())
}

async fn metric_series(db: &Arc<Db>, args: &Args) -> Result<String, String> {
    let metric = args.req_str("metric")?;
    let series = offload(db.metrics().series(metric))
        .await
        .map_err(|e| e.to_string())?;
    let mut out = Arr::new();
    for labels in &series {
        out.raw(&attributes(labels));
    }
    Ok(Obj::new()
        .str("metric", metric)
        .raw("series", &out.finish())
        .uint("series_count", series.len() as u64)
        .finish())
}

async fn query_metric(db: &Arc<Db>, args: &Args, instant: bool) -> Result<String, String> {
    let metric = args.req_str("metric")?;
    let kind = args.str("kind")?.unwrap_or("gauge");
    let mut q = match kind {
        "gauge" => MetricQuery::gauge(metric),
        "sum" | "counter" => MetricQuery::sum(metric),
        other => {
            return Err(format!(
                "argument `kind` must be \"gauge\" or \"sum\", got {other:?} — call `list_metrics` to see a metric's kind"
            ));
        }
    };
    if let Some(agg) = args.str("aggregation")? {
        q = q.aggregation(match agg {
            "sum" => Aggregation::Sum,
            "avg" | "mean" => Aggregation::Avg,
            "min" => Aggregation::Min,
            "max" => Aggregation::Max,
            "count" => Aggregation::Count,
            other => {
                return Err(format!(
                    "argument `aggregation` must be one of sum|avg|min|max|count, got {other:?}"
                ));
            }
        });
    }
    for key in args.string_list("group_by")? {
        q = q.group_by(&key);
    }
    for (k, v) in args.string_map("labels")? {
        q = q.filter(&k, &v);
    }
    if args.bool("rate")?.unwrap_or(false) {
        // `rate_counter` is the monotonic-counter reading; the plain `rate` is the delta form. A sum
        // metric is the counter case, which is what "rate" means to anyone asking for one.
        q = if kind == "gauge" {
            q.rate()
        } else {
            q.rate_counter()
        };
    }
    q = q
        .range(window(args, if instant { "5m" } else { "1h" })?)
        .step(step(args, "1m")?);

    if instant {
        let vector = offload(db.metrics().instant(q))
            .await
            .map_err(|e| e.to_string())?;
        let mut out = Arr::new();
        for s in &vector.0 {
            out.raw(
                &Obj::new()
                    .raw("labels", &labels(&s.labels))
                    .int("time_unix_nano", s.sample.time.0)
                    .float("value", s.sample.value)
                    .finish(),
            );
        }
        return Ok(Obj::new()
            .str("metric", metric)
            .raw("samples", &out.finish())
            .finish());
    }

    let matrix = offload(db.metrics().range(q))
        .await
        .map_err(|e| e.to_string())?;
    Ok(Obj::new()
        .str("metric", metric)
        .raw("series", &matrix_json(&matrix))
        .finish())
}

async fn histogram_quantile(db: &Arc<Db>, args: &Args) -> Result<String, String> {
    let metric = args.req_str("metric")?;
    let phi = args.f64("quantile")?.unwrap_or(0.95);
    if !(0.0..=1.0).contains(&phi) {
        return Err(format!(
            "argument `quantile` must be between 0 and 1, got {}",
            number(phi)
        ));
    }
    let range = window(args, "1h")?;
    let step = step(args, "1m")?;
    let group_by = args.string_list("group_by")?;
    let filters = args.string_map("labels")?;

    // The two histogram flavours are separate storage tables with separate query types, so the
    // branch has to be duplicated rather than abstracted over.
    let matrix = if args.bool("exponential")?.unwrap_or(false) {
        let mut q = imbh::ExpHistogramQuery::new(metric)
            .quantile(phi)
            .range(range)
            .step(step);
        for key in &group_by {
            q = q.group_by(key);
        }
        for (k, v) in &filters {
            q = q.filter(k, v);
        }
        offload(db.metrics().exp_histogram_quantile(q)).await
    } else {
        let mut q = HistogramQuery::new(metric)
            .quantile(phi)
            .range(range)
            .step(step);
        for key in &group_by {
            q = q.group_by(key);
        }
        for (k, v) in &filters {
            q = q.filter(k, v);
        }
        offload(db.metrics().histogram_quantile(q)).await
    }
    .map_err(|e| e.to_string())?;

    Ok(Obj::new()
        .str("metric", metric)
        .float("quantile", phi)
        .raw("series", &matrix_json(&matrix))
        .finish())
}

async fn list_attribute_keys(db: &Arc<Db>) -> Result<String, String> {
    let keys = offload(db.attrs().names())
        .await
        .map_err(|e| e.to_string())?;
    let mut out = Arr::new();
    for key in &keys {
        out.str(key);
    }
    Ok(Obj::new().raw("keys", &out.finish()).finish())
}

async fn list_attribute_values(db: &Arc<Db>, args: &Args) -> Result<String, String> {
    let key = args.req_str("key")?;
    let values = offload(db.attrs().values(key))
        .await
        .map_err(|e| e.to_string())?;
    let mut out = Arr::new();
    for value in &values {
        out.str(value);
    }
    Ok(Obj::new()
        .str("key", key)
        .raw("values", &out.finish())
        .uint("value_count", values.len() as u64)
        .finish())
}

async fn db_stats(db: &Arc<Db>) -> Result<String, String> {
    let stats = offload(db.stats()).await.map_err(|e| e.to_string())?;
    Ok(stats_json(&stats))
}

// ── result rendering ────────────────────────────────────────────────────────────────────────────

fn log_entry_json(e: &LogEntry) -> String {
    let mut obj = Obj::new();
    obj.int("time_unix_nano", e.time.0)
        .int("severity_number", e.severity_number.0 as i64)
        .opt_str("severity_text", e.severity_text.as_deref())
        .opt_str("service", e.service.as_deref())
        .str("body", &e.body);
    if !e.attributes.is_empty() {
        obj.raw("attributes", &attributes(&e.attributes));
    }
    if !e.resource.is_empty() {
        obj.raw("resource", &attributes(&e.resource));
    }
    obj.opt_str("trace_id", e.trace_id.map(|t| t.to_hex()).as_deref())
        .opt_str("span_id", e.span_id.map(|s| s.to_hex()).as_deref())
        .finish()
}

fn matrix_json(matrix: &imbh::Matrix) -> String {
    let mut series = Arr::new();
    for s in &matrix.0 {
        let mut samples = Arr::new();
        for sample in &s.samples {
            samples.raw(
                &Obj::new()
                    .int("time_unix_nano", sample.time.0)
                    .float("value", sample.value)
                    .finish(),
            );
        }
        series.raw(
            &Obj::new()
                .raw("labels", &labels(&s.labels))
                .raw("samples", &samples.finish())
                .finish(),
        );
    }
    series.finish()
}

/// Embed a stored canonical-JSON blob (a span's `events`/`links`) as real JSON.
///
/// The column is engine-written, so it parses — but a corrupt segment must not be able to splice
/// arbitrary text into the response, so anything that does not parse is carried as a string instead.
fn embed_json(obj: &mut Obj, name: &str, raw: Option<&str>) {
    let Some(raw) = raw else { return };
    match imbh::parse_json(raw) {
        Some(value) => {
            obj.raw(name, &any_value(&value));
        }
        None => {
            obj.str(name, raw);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use imbh::parse_json;

    #[test]
    fn tool_names_are_unique_and_wire_safe() {
        let mut seen = Vec::new();
        for tool in TOOLS {
            assert!(!seen.contains(&tool.name), "duplicate tool {}", tool.name);
            seen.push(tool.name);
            // MCP constrains tool names to [A-Za-z0-9_.-]; ours are plain snake_case, which also
            // keeps the transport's `Mcp-Name` header out of its Base64 sentinel path.
            assert!(
                tool.name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'.' || b == b'-'),
                "tool name {} is not wire-safe",
                tool.name
            );
            assert!(!tool.description.is_empty());
            assert!(!tool.title.is_empty());
        }
    }

    #[test]
    fn every_input_schema_is_valid_json() {
        for tool in TOOLS {
            let parsed = parse_json(tool.input_schema)
                .unwrap_or_else(|| panic!("tool {} has a malformed input schema", tool.name));
            let AnyValueMap(pairs) = AnyValueMap::of(&parsed)
                .unwrap_or_else(|| panic!("tool {} schema is not an object", tool.name));
            let ty = pairs
                .iter()
                .find(|(k, _)| k == "type")
                .and_then(|(_, v)| v.as_str());
            assert_eq!(
                ty,
                Some("object"),
                "tool {} schema is not an object schema",
                tool.name
            );
        }
    }

    /// A tiny destructuring helper so the schema test can read the parsed object without pulling in
    /// a JSON DOM crate.
    struct AnyValueMap<'a>(&'a Vec<(String, imbh::AnyValue)>);
    impl<'a> AnyValueMap<'a> {
        fn of(v: &'a imbh::AnyValue) -> Option<Self> {
            match v {
                imbh::AnyValue::Map(pairs) => Some(AnyValueMap(pairs)),
                _ => None,
            }
        }
    }

    #[test]
    fn the_sql_tool_lists_every_table() {
        // Every signal table must appear in the description a model reads, or it cannot query it.
        // Driven off `Table::ALL` rather than a list written here: a hand-written list is exactly how
        // `metrics_summary` went missing from the description while this test still passed.
        for table in imbh::Table::ALL {
            let name = table.as_str();
            assert!(
                TOOLS[0].description.contains(name),
                "query_sql's description does not mention the `{name}` table"
            );
        }
    }

    #[test]
    fn severities_parse_by_name_and_number() {
        let args = |json: &str| Args::new(parse_json(json).as_ref());
        assert_eq!(
            severity(&args(r#"{"s":"error"}"#), "s").unwrap(),
            Some(SeverityNumber::ERROR)
        );
        assert_eq!(
            severity(&args(r#"{"s":"WARN"}"#), "s").unwrap(),
            Some(SeverityNumber::WARN)
        );
        assert_eq!(
            severity(&args(r#"{"s":13}"#), "s").unwrap(),
            Some(SeverityNumber(13))
        );
        assert_eq!(severity(&args("{}"), "s").unwrap(), None);
        assert!(severity(&args(r#"{"s":"loud"}"#), "s").is_err());
        assert!(severity(&args(r#"{"s":99}"#), "s").is_err());
    }

    #[test]
    fn windows_prefer_explicit_bounds_over_since() {
        let args = |json: &str| Args::new(parse_json(json).as_ref());
        let r = window(&args(r#"{"start_unix_nano":10,"end_unix_nano":20}"#), "1h").unwrap();
        assert_eq!((r.start.0, r.end.0), (10, 20));

        // A one-sided bound leaves the other end open rather than silently applying `since`.
        let r = window(&args(r#"{"start_unix_nano":10,"since":"5m"}"#), "1h").unwrap();
        assert_eq!((r.start.0, r.end.0), (10, i64::MAX));

        let r = window(&args(r#"{"since":"5m"}"#), "1h").unwrap();
        assert!(r.end.0 - r.start.0 >= 300_000_000_000);
        assert!(r.end.0 - r.start.0 < 301_000_000_000);

        // The default applies when nothing is given.
        let r = window(&args("{}"), "5m").unwrap();
        assert!(r.end.0 - r.start.0 >= 300_000_000_000);
        assert!(window(&args(r#"{"since":"nope"}"#), "1h").is_err());
    }

    #[test]
    fn trace_ids_must_be_hex() {
        assert!(trace_id("0123456789abcdef0123456789abcdef").is_ok());
        assert!(trace_id("abc").unwrap_err().contains("32 hexadecimal"));
        assert!(trace_id("zz23456789abcdef0123456789abcdef").is_err());
    }

    #[test]
    fn steps_reject_zero() {
        let args = |json: &str| Args::new(parse_json(json).as_ref());
        assert_eq!(step(&args("{}"), "1m").unwrap(), Duration::from_secs(60));
        assert_eq!(
            step(&args(r#"{"step":"30s"}"#), "1m").unwrap(),
            Duration::from_secs(30)
        );
        assert!(step(&args(r#"{"step":"0s"}"#), "1m").is_err());
    }
}

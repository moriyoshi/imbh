//! The tool surface an MCP client sees.
//!
//! Each tool is a thin wrapper over one imbh library call (ARCHITECTURE.md §10.5–§10.9): the typed
//! log/trace/metric query builders, attribute discovery and statistics, DB stats, and raw SQL. All
//! but one are reads — no ingest, no flush/compact, no retention.
//!
//! The exception is `set_promoted_attributes`, which replaces the promoted attribute keys (§6.1): it
//! seals the buffer and changes the schema every segment written afterwards carries. It is marked
//! [`Tool::writes`] and [`visible`] hides it from a **read-only** handle, so a client driving a
//! reader is never offered it — a handle that holds no writer lock has nothing for it to call. A
//! deployment serving `/mcp` from the *writer* is granting an agent that action, and should gate the
//! endpoint accordingly.
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
use serde_json::{Value, json};

use crate::json::{Args, attributes, labels, number};
use crate::{Housekeeping, batches_to_json, offload, stats_json};

/// One tool as `tools/list` describes it.
pub(crate) struct Tool {
    pub(crate) name: &'static str,
    pub(crate) title: &'static str,
    pub(crate) description: &'static str,
    /// Whether the tool **changes the database**. Read-only tools are the rule and this is the
    /// exception, so it is a field rather than a naming convention: [`visible`] hides a write tool
    /// from a read-only handle, and a client is never offered something that cannot work.
    pub(crate) writes: bool,
    /// Whether the tool needs the host's **housekeeping queue**. Hosts that run one (`imbhd`) offer
    /// these; hosts that do not (`imbh-tui --mcp-stdio`) do not, for the same reason a read-only
    /// handle is not offered the writes — there is nothing for the tool to call.
    pub(crate) queue: bool,
    /// The tool's JSON Schema as text. Kept as a literal because a schema reads far better written
    /// out than assembled from `json!` calls; [`Tool::schema`] parses it for the wire.
    pub(crate) input_schema: &'static str,
}

impl Tool {
    /// The parsed `inputSchema` for `tools/list`.
    ///
    /// The schemas are compile-time literals covered by a test that parses every one of them, so the
    /// fallback is unreachable — but a malformed schema must degrade to an unhelpful tool rather
    /// than panic inside a request handler.
    pub(crate) fn schema(&self) -> Value {
        serde_json::from_str(self.input_schema).unwrap_or_else(|_| json!({"type": "object"}))
    }
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
        writes: false,
        queue: false,
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
        writes: false,
        queue: false,
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
        writes: false,
        queue: false,
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
        writes: false,
        queue: false,
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
            "group_by":{"type":"array","items":{"type":"string"},"description":"Keys to break the volume down by, e.g. [\"http.route\"]. Record attribute keys, plus `service.name` to split by service. A record missing the key groups under \"\"."}
        },"additionalProperties":false}"#,
    },
    Tool {
        name: "search_traces",
        title: "Search traces",
        description: "Find traces by service, span name, status, duration, and attributes. Returns \
             one summary per trace (root service/name, start, duration, span count, error flag) — \
             call `get_trace` with a returned `trace_id` for the spans.",
        writes: false,
        queue: false,
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
        writes: false,
        queue: false,
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
        writes: false,
        queue: false,
        input_schema: r#"{"type":"object","properties":{
            "service":{"type":"string"},
            "name":{"type":"string","description":"Exact span name."},
            "kind":{"type":"string"},
            "status":{"type":"string"},
            "attributes":{"type":"object","description":"Span-attribute equality filters."},
            "group_by":{"type":"array","items":{"type":"string"},"description":"Keys to group series by, e.g. [\"http.route\"]. Span attribute keys, plus `service.name` to split by service. A span missing the key groups under \"\"."},
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
        writes: false,
        queue: false,
        input_schema: r#"{"type":"object","additionalProperties":false}"#,
    },
    Tool {
        name: "metric_series",
        title: "List a metric's series",
        description: "List the distinct label sets (series) reported for one metric, so you know \
             what you can filter or group by.",
        writes: false,
        queue: false,
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
        writes: false,
        queue: false,
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
        writes: false,
        queue: false,
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
        writes: false,
        queue: false,
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
        writes: false,
        queue: false,
        input_schema: r#"{"type":"object","additionalProperties":false}"#,
    },
    Tool {
        name: "list_attribute_values",
        title: "List attribute values",
        description: "List the distinct string values of one attribute key across every signal — \
             e.g. the set of `service.name`s, or every `http.route` seen.",
        writes: false,
        queue: false,
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
        writes: false,
        queue: false,
        input_schema: r#"{"type":"object","additionalProperties":false}"#,
    },
    Tool {
        name: "attribute_stats",
        title: "Attribute statistics",
        description: "Measure attribute keys: how many distinct values each has, how much of that \
             value space sits inside one segment (sigma — a segment index prunes `1 - sigma`), what a \
             promoted column would cost per row, and two independent verdicts — whether to promote \
             the key, and the widest query window over which segment pruning still pays. Reported \
             DB-wide and per table, since sigma is only defined against a table's own segments. Use \
             it to choose a `promote` list from data instead of guesswork. This **scans** every \
             sealed segment's attribute columns in range, so it costs the corpus rather than the \
             answer — narrow the window on a large database. Buffered rows are in no segment yet and \
             are excluded; `unsealed_wal_frames` says how many were skipped.",
        writes: false,
        queue: false,
        input_schema: r#"{"type":"object","properties":{
            "since":{"type":"string","description":"Measure segments overlapping the last duration (`15m`, `24h`, `7d`). Omit all three window arguments to measure every sealed segment, which is the default and usually what a `promote` list should be chosen from."},
            "start_unix_nano":{"type":"integer","description":"Window start, epoch nanoseconds. Overrides `since`."},
            "end_unix_nano":{"type":"integer","description":"Window end, epoch nanoseconds."},
            "top":{"type":"integer","minimum":1,"description":"Keys reported per scan unit, most expensive promoted column first (default 20, max 200). `truncated` is set on a unit whose keys were cut."}
        },"additionalProperties":false}"#,
    },
    Tool {
        name: "list_promoted_attributes",
        title: "List promoted attributes",
        description: "The attribute keys currently promoted to columns, in column order. A promoted \
             key is stored as a real column as well as in the JSON attribute blob, so filters on it \
             hit a column instead of a JSON scan. Read this before changing the set —              `set_promoted_attributes` replaces it wholesale.",
        writes: false,
        queue: false,
        input_schema: r#"{"type":"object","additionalProperties":false}"#,
    },
    Tool {
        name: "set_promoted_attributes",
        title: "Set promoted attributes",
        description: "Replace the promoted attribute keys, answering with the set now in effect. \
             **This writes.** It seals the buffer and changes the schema every segment written \
             afterwards carries; segments already on disk keep theirs and stay queryable either way. \
             Send the whole set, not a delta — the order is the column order. Use \
             `attribute_stats` to choose it: promote keys it rates cheap and widely present, and \
             demote by sending the set without them. Demotion is always safe (the key never left the \
             JSON blob); promotion is the direction worth being slow about, since it is a schema \
             change.",
        writes: true,
        queue: false,
        input_schema: r#"{"type":"object","properties":{
            "keys":{"type":"array","items":{"type":"string"},"description":"The complete set of attribute keys to promote, in the column order wanted. An empty array promotes nothing."}
        },"required":["keys"],"additionalProperties":false}"#,
    },
    Tool {
        name: "run_housekeeping",
        title: "Run housekeeping",
        description: "Queue a housekeeping pass and return its **job id** — the work has not run \
             when this answers. A pass seals the write buffer, commits any prepared segment \
             rewrites, applies retention, and (with `compact: true`) merges each day's segments. \
             Poll `housekeeping_status` with the id until `state` is `succeeded` or `failed`. A pass \
             costs the size of the database rather than the size of an answer, which is why it is \
             queued rather than performed. Submitting the same request twice while one is still \
             waiting returns the waiting job's id rather than queueing a second pass.",
        writes: true,
        queue: true,
        input_schema: r#"{"type":"object","properties":{
            "compact":{"type":"boolean","description":"Also merge each day-partition's segments. The expensive half; false by default."},
            "max_jobs":{"type":"integer","minimum":1,"description":"Cap the partitions this pass rewrites, so a large database can be compacted a slice at a time. Omit for no cap; the report's `compaction_complete` says whether anything was left."}
        },"additionalProperties":false}"#,
    },
    Tool {
        name: "housekeeping_status",
        title: "Housekeeping status",
        description: "What a housekeeping job did, by the id `run_housekeeping` returned. `state` \
             is `queued`, `running`, `succeeded` or `failed`; `report` carries the counts on \
             success and `error` the reason on failure. Omit `job_id` to list the recent jobs, \
             newest first — ids do not survive a restart of the server.",
        writes: false,
        queue: true,
        input_schema: r#"{"type":"object","properties":{
            "job_id":{"type":"string","description":"The id to look up. Omitted, the recent jobs are listed instead."}
        },"additionalProperties":false}"#,
    },
];

/// The tools a client is offered against `db`.
///
/// A read-only handle holds no writer lock and refuses every write by construction, so a tool that
/// writes is not merely disallowed there — it has nothing to call. Hiding it is the honest answer: an
/// agent should not be offered an action that cannot succeed, and a client that caches `tools/list`
/// then never learns it exists.
pub(crate) fn visible(
    db: &Arc<Db>,
    housekeeping: Option<&Arc<dyn Housekeeping>>,
) -> impl Iterator<Item = &'static Tool> {
    let read_only = db.is_read_only();
    let queued = housekeeping.is_some();
    TOOLS
        .iter()
        .filter(move |tool| !(tool.writes && read_only) && !(tool.queue && !queued))
}

/// Run one tool. `None` means the tool name is unknown, which is a *protocol* error (`-32602`), not
/// a tool-execution error. `Some(Err(message))` is a tool-execution error the model should see.
pub(crate) async fn call(
    db: &Arc<Db>,
    housekeeping: Option<&Arc<dyn Housekeeping>>,
    name: &str,
    args: &Args,
) -> Option<Result<Value, String>> {
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
        "attribute_stats" => attribute_stats(db, args).await,
        "list_promoted_attributes" => Ok(promoted_json(db)),
        "set_promoted_attributes" => set_promoted_attributes(db, args).await,
        "run_housekeeping" => run_housekeeping(db, housekeeping, args),
        "housekeeping_status" => housekeeping_status(housekeeping, args),
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

async fn query_sql(db: &Arc<Db>, args: &Args) -> Result<Value, String> {
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
    // `batches_to_json` is the same serializer `POST /api/query` answers with, so SQL rows look
    // identical on both surfaces. It renders straight to bytes, hence the parse back into a value —
    // bounded by `max_rows`, and worth it to keep one row serializer rather than two.
    let rows: Value = serde_json::from_slice(&batches_to_json(&kept)).map_err(|e| e.to_string())?;

    Ok(json!({
        "rows": rows,
        "row_count": total.min(max_rows),
        "truncated": total > max_rows,
    }))
}

async fn search_logs(db: &Arc<Db>, args: &Args) -> Result<Value, String> {
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

    let entries: Vec<Value> = page.entries.iter().map(log_entry_json).collect();
    Ok(json!({
        "entries": entries,
        "entry_count": page.entries.len(),
        // The cursor itself is opaque and not resumable across calls here, so report only whether
        // more rows exist — a model reading `has_more` will narrow its window or raise the limit.
        "has_more": page.next.is_some(),
        "stats": {
            "rows_scanned": page.stats.rows_scanned,
            "segments_scanned": page.stats.segments_scanned,
            "segments_pruned": page.stats.segments_pruned,
            "elapsed_ns": page.stats.elapsed.0,
            "used_index": page.stats.used_index,
        },
    }))
}

async fn count_logs(db: &Arc<Db>, args: &Args) -> Result<Value, String> {
    let count = offload(db.logs().count(log_filter(args)?))
        .await
        .map_err(|e| e.to_string())?;
    Ok(json!({ "count": count }))
}

async fn log_volume(db: &Arc<Db>, args: &Args) -> Result<Value, String> {
    let filter = log_filter(args)?;
    let step = step(args, "1m")?;
    let group_by = args.string_list("group_by")?;
    let keys: Vec<&str> = group_by.iter().map(|k| k.as_str()).collect();

    let buckets = offload(db.logs().volume_by(filter, step, &keys))
        .await
        .map_err(|e| e.to_string())?;

    let out: Vec<Value> = buckets
        .iter()
        .map(|bucket| {
            let mut obj = json!({"time_unix_nano": bucket.time.0, "count": bucket.count});
            if !bucket.labels.is_empty() {
                obj["labels"] = labels(&bucket.labels);
            }
            obj
        })
        .collect();
    Ok(json!({
        "step_ns": step.as_nanos().min(u64::MAX as u128) as u64,
        "buckets": out,
    }))
}

async fn search_traces(db: &Arc<Db>, args: &Args) -> Result<Value, String> {
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

    let out: Vec<Value> = traces
        .iter()
        .map(|t| {
            json!({
                "trace_id": t.trace_id.to_hex(),
                "root_service": t.root_service,
                "root_name": t.root_name,
                "start_time_unix_nano": t.start_time.0,
                "duration_ns": t.duration_ns.0,
                "span_count": t.span_count,
                "error": t.error,
            })
        })
        .collect();
    Ok(json!({"traces": out, "trace_count": traces.len()}))
}

async fn get_trace(db: &Arc<Db>, args: &Args) -> Result<Value, String> {
    let id = trace_id(args.req_str("trace_id")?)?;
    let trace = offload(db.traces().get(id))
        .await
        .map_err(|e| e.to_string())?;

    let Some(trace) = trace else {
        return Ok(json!({"found": false, "trace_id": id.to_hex()}));
    };

    let spans: Vec<Value> = trace
        .spans
        .iter()
        .map(|s| {
            // Absent *scalars* serialize as null (a uniform key set is easier to read than a
            // shifting one); absent *collections* are omitted, since an empty object is pure noise
            // on every span of a large trace.
            let mut obj = json!({
                "span_id": s.span_id.to_hex(),
                "parent_span_id": s.parent_span_id.map(|p| p.to_hex()),
                "name": s.name,
                "kind": s.kind,
                "service": s.service,
                "start_time_unix_nano": s.start_time.0,
                "duration_ns": s.duration_ns.0,
                "status_code": s.status_code,
                "status_message": s.status_message,
            });
            if !s.attributes.is_empty() {
                obj["attributes"] = attributes(&s.attributes);
            }
            if !s.resource.is_empty() {
                obj["resource"] = attributes(&s.resource);
            }
            embed_json(&mut obj, "events", s.events.as_deref());
            embed_json(&mut obj, "links", s.links.as_deref());
            obj
        })
        .collect();

    Ok(json!({
        "found": true,
        "trace_id": trace.trace_id.to_hex(),
        "root_service": trace.root_service,
        "root_name": trace.root_name,
        "start_time_unix_nano": trace.start_time.0,
        "duration_ns": trace.duration_ns.0,
        "span_count": trace.spans.len(),
        "spans": spans,
    }))
}

async fn span_metrics(db: &Arc<Db>, args: &Args) -> Result<Value, String> {
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

    let series: Vec<Value> = metrics
        .0
        .iter()
        .map(|s| {
            let points: Vec<Value> = s
                .points
                .iter()
                .map(|p| {
                    json!({
                        "time_unix_nano": p.time.0,
                        "calls": p.calls,
                        "errors": p.errors,
                        // Latency percentiles over an empty bucket are NaN, which JSON cannot spell;
                        // `number` renders those as null rather than producing invalid JSON.
                        "error_rate": number(p.error_rate),
                        "p50_ns": number(p.p50_ns),
                        "p95_ns": number(p.p95_ns),
                        "p99_ns": number(p.p99_ns),
                    })
                })
                .collect();
            json!({"labels": labels(&s.labels), "points": points})
        })
        .collect();
    Ok(json!({ "series": series }))
}

async fn list_metrics(db: &Arc<Db>) -> Result<Value, String> {
    let catalog = offload(db.metrics().catalog())
        .await
        .map_err(|e| e.to_string())?;
    let out: Vec<Value> = catalog
        .iter()
        .map(|m| {
            json!({
                "metric": m.metric,
                "kind": m.kind,
                "unit": m.unit,
                "temporality": m.temporality,
            })
        })
        .collect();
    Ok(json!({ "metrics": out }))
}

async fn metric_series(db: &Arc<Db>, args: &Args) -> Result<Value, String> {
    let metric = args.req_str("metric")?;
    let series = offload(db.metrics().series(metric))
        .await
        .map_err(|e| e.to_string())?;
    let out: Vec<Value> = series.iter().map(attributes).collect();
    Ok(json!({
        "metric": metric,
        "series": out,
        "series_count": series.len(),
    }))
}

async fn query_metric(db: &Arc<Db>, args: &Args, instant: bool) -> Result<Value, String> {
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
        let out: Vec<Value> = vector
            .0
            .iter()
            .map(|s| {
                json!({
                    "labels": labels(&s.labels),
                    "time_unix_nano": s.sample.time.0,
                    "value": number(s.sample.value),
                })
            })
            .collect();
        return Ok(json!({"metric": metric, "samples": out}));
    }

    let matrix = offload(db.metrics().range(q))
        .await
        .map_err(|e| e.to_string())?;
    Ok(json!({"metric": metric, "series": matrix_json(&matrix)}))
}

async fn histogram_quantile(db: &Arc<Db>, args: &Args) -> Result<Value, String> {
    let metric = args.req_str("metric")?;
    let phi = args.f64("quantile")?.unwrap_or(0.95);
    if !(0.0..=1.0).contains(&phi) {
        return Err(format!(
            "argument `quantile` must be between 0 and 1, got {phi}"
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

    Ok(json!({
        "metric": metric,
        "quantile": number(phi),
        "series": matrix_json(&matrix),
    }))
}

async fn list_attribute_keys(db: &Arc<Db>) -> Result<Value, String> {
    let keys = offload(db.attrs().names())
        .await
        .map_err(|e| e.to_string())?;
    Ok(json!({ "keys": keys }))
}

async fn list_attribute_values(db: &Arc<Db>, args: &Args) -> Result<Value, String> {
    let key = args.req_str("key")?;
    let values = offload(db.attrs().values(key))
        .await
        .map_err(|e| e.to_string())?;
    Ok(json!({
        "key": key,
        "value_count": values.len(),
        "values": values,
    }))
}

async fn db_stats(db: &Arc<Db>) -> Result<Value, String> {
    let stats = offload(db.stats()).await.map_err(|e| e.to_string())?;
    // `stats_json` is the same serializer `GET /stats` answers with, so the two surfaces cannot
    // describe one database differently; it renders to text, hence the parse back into a value.
    serde_json::from_str(&stats_json(&stats)).map_err(|e| e.to_string())
}

/// Measure the attribute keys (ARCHITECTURE.md §10.20).
///
/// The window defaults to **every sealed segment**, unlike every other tool here: a `promote` list is
/// chosen from everything the database holds, not from a recent slice. The narrowing arguments are
/// there because this scans, so a large corpus can be bounded — not because a window is the natural
/// unit of the question.
async fn attribute_stats(db: &Arc<Db>, args: &Args) -> Result<Value, String> {
    use imbh::attrstats::AttrScope;

    let top = args.limit("top", 20, 200)?;
    // Explicit bounds win, then `since`; all absent means unbounded, which `Options::range = None` is.
    let range = match (
        args.i64("start_unix_nano")?,
        args.i64("end_unix_nano")?,
        args.duration("since")?,
    ) {
        (None, None, None) => None,
        (start, end, since) => {
            let now = Timestamp::now().0;
            let start = start.or_else(|| {
                since.map(|d| now.saturating_sub(d.as_nanos().min(i64::MAX as u128) as i64))
            });
            Some((start.unwrap_or(i64::MIN), end.unwrap_or(i64::MAX)))
        }
    };
    let options = imbh::attrstats::Options {
        range,
        ..Default::default()
    };
    let report = offload(db.attribute_stats(&options))
        .await
        .map_err(|e| e.to_string())?;
    let promote = db.promote();
    let promoted = promote.keys();

    let unit = |unit: &imbh::attrstats::UnitReport, db_wide: bool| {
        let mut keys: Vec<&imbh::attrstats::KeyReport> = unit.keys.iter().collect();
        // Most expensive promoted column first: the keys worth arguing about, not the ones that
        // merely occur most often.
        keys.sort_by(|a, b| {
            b.est_bytes_per_row
                .partial_cmp(&a.est_bytes_per_row)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.name.cmp(&b.name))
        });
        let truncated = keys.len() > top;
        json!({
            "unit": if db_wide { "all_tables" } else { unit.label.as_str() },
            "segments": unit.segments,
            "rows": unit.rows,
            "keys_measured": unit.keys.len(),
            "truncated": truncated,
            "keys": keys.iter().take(top).map(|key| json!({
                "key": key.name,
                "scope": key.scope.column(),
                "promoted": promoted.contains(&key.name),
                // `promote` is DB-wide configuration, so only the roll-up gives a verdict: a
                // per-table one would judge it against one table's totals.
                "promote": (db_wide && key.scope == AttrScope::Attributes)
                    .then(|| report.promote_verdict(key)),
                // Sigma is defined against a *table's* segment count, so each unit answers for
                // itself and the roll-up reports the best case across them.
                "index_scale": if db_wide {
                    report.index_scale(&key.name)
                } else {
                    report.index_scale_in(unit, &key.name)
                },
                "coverage": number(key.coverage(unit.rows)),
                "distinct_values_estimated": number(key.distinct_est),
                "sigma_p50": key.sigma.as_ref().map(|sigma| number(sigma.p50)),
                "estimated_bytes_per_row": number(key.est_bytes_per_row),
                "rows_present": key.rows_present,
                "sampled": key.is_sampled(),
            })).collect::<Vec<_>>(),
        })
    };

    Ok(json!({
        "range_unix_nanos": report.range.map(|(start, end)| json!([start, end])),
        "promoted": promoted,
        // Anything the measurement could not cover, stated rather than left to be inferred from a
        // short list: a truncated result that reads like full coverage is worse than no result.
        "unsealed_wal_frames": report.pending_wal_frames,
        "segments_skipped": report.segments_skipped,
        "all_tables": unit(&report.global, true),
        "tables": report.tables.iter().filter(|unit| unit.segments > 0)
            .map(|u| unit(u, false)).collect::<Vec<_>>(),
    }))
}

/// The promoted set as both tools answer it, so the read and the write cannot describe it
/// differently.
fn promoted_json(db: &Arc<Db>) -> Value {
    json!({ "promoted": db.promote().keys() })
}

/// Replace the promoted attribute keys — the one tool here that writes (ARCHITECTURE.md §6.1).
///
/// Hidden from `tools/list` on a read-only handle ([`visible`]), but re-checked here: a client may
/// have cached an older list, and the message it gets should say why the action does not exist rather
/// than surface the storage layer's read-only error.
async fn set_promoted_attributes(db: &Arc<Db>, args: &Args) -> Result<Value, String> {
    if db.is_read_only() {
        return Err(
            "this server opened the database read-only, so it cannot change the promoted \
                    set. Point the MCP client at the process that writes the database."
                .to_owned(),
        );
    }
    let keys = args.string_list("keys")?;
    offload(db.set_promote(imbh::Promote::new(keys)))
        .await
        .map_err(|e| e.to_string())?;
    // The set now in effect, not the set requested: keys colliding with a built-in column name are
    // filtered at schema construction, so the two can differ.
    Ok(promoted_json(db))
}

/// Queue a housekeeping pass and answer with its job id.
///
/// Synchronous by design: submitting is the fast part, and the record it returns is a *handle*. A tool
/// that waited for the pass would be the thing the queue exists to avoid.
fn run_housekeeping(
    db: &Arc<Db>,
    housekeeping: Option<&Arc<dyn Housekeeping>>,
    args: &Args,
) -> Result<Value, String> {
    let Some(queue) = housekeeping else {
        return Err("this server runs no housekeeping queue".to_owned());
    };
    if db.is_read_only() {
        return Err(
            "this server opened the database read-only, so it cannot run housekeeping. \
                    Point the client at the process that writes the database."
                .to_owned(),
        );
    }
    let compact = args.bool("compact")?.unwrap_or(false);
    // Zero would mean "compact, but compact nothing", which `compact: false` already says.
    let max_jobs = match args.i64("max_jobs")? {
        None => None,
        Some(n) if n > 0 => Some(n as usize),
        Some(_) => {
            return Err(
                "`max_jobs` must be a positive integer — the number of partitions this pass \
                        may rewrite. Omit it for an unbounded pass, or set `compact` to false to \
                        skip compaction."
                    .to_owned(),
            );
        }
    };
    Ok(queue.submit(compact, max_jobs))
}

/// One housekeeping job, or the recent ones when no id is given.
fn housekeeping_status(
    housekeeping: Option<&Arc<dyn Housekeeping>>,
    args: &Args,
) -> Result<Value, String> {
    let Some(queue) = housekeeping else {
        return Err("this server runs no housekeeping queue".to_owned());
    };
    match args.str("job_id")? {
        Some(id) => queue.get(id).ok_or_else(|| {
            format!(
                "no housekeeping job {id}. Ids do not survive a restart of the server, and only the \
                 most recent jobs are retained."
            )
        }),
        None => Ok(json!({ "jobs": queue.recent() })),
    }
}

// ── result rendering ────────────────────────────────────────────────────────────────────────────

fn log_entry_json(e: &LogEntry) -> Value {
    let mut obj = json!({
        "time_unix_nano": e.time.0,
        "severity_number": e.severity_number.0,
        "severity_text": e.severity_text,
        "service": e.service,
        "body": e.body,
        "trace_id": e.trace_id.map(|t| t.to_hex()),
        "span_id": e.span_id.map(|s| s.to_hex()),
    });
    // Empty attribute maps are omitted rather than sent as `{}` — on a page of 1000 entries that is
    // pure noise in the model's context.
    if !e.attributes.is_empty() {
        obj["attributes"] = attributes(&e.attributes);
    }
    if !e.resource.is_empty() {
        obj["resource"] = attributes(&e.resource);
    }
    obj
}

fn matrix_json(matrix: &imbh::Matrix) -> Value {
    let series: Vec<Value> = matrix
        .0
        .iter()
        .map(|s| {
            let samples: Vec<Value> = s
                .samples
                .iter()
                .map(|sample| {
                    json!({
                        "time_unix_nano": sample.time.0,
                        "value": number(sample.value),
                    })
                })
                .collect();
            json!({"labels": labels(&s.labels), "samples": samples})
        })
        .collect();
    Value::Array(series)
}

/// Embed a stored canonical-JSON blob (a span's `events`/`links`) as real JSON.
///
/// The column is engine-written, so it parses — but a corrupt segment must not be able to put
/// anything but a value where a value belongs, so text that does not parse is carried as a string.
fn embed_json(obj: &mut Value, name: &str, raw: Option<&str>) {
    let Some(raw) = raw else { return };
    obj[name] = serde_json::from_str(raw).unwrap_or_else(|_| Value::String(raw.to_owned()));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build an `Args` from a JSON literal, the way a `tools/call` body would.
    fn args(json: &str) -> Args {
        let value: Value = serde_json::from_str(json).expect("test json");
        Args::new(Some(&value))
    }

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
        // This is what makes `Tool::schema`'s fallback unreachable: a malformed literal fails here
        // rather than reaching a client as a tool with no arguments.
        for tool in TOOLS {
            let parsed: Value = serde_json::from_str(tool.input_schema)
                .unwrap_or_else(|e| panic!("tool {} has a malformed input schema: {e}", tool.name));
            assert_eq!(
                parsed.get("type").and_then(Value::as_str),
                Some("object"),
                "tool {} schema is not an object schema",
                tool.name
            );
            assert_eq!(tool.schema(), parsed);
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
        assert_eq!(step(&args("{}"), "1m").unwrap(), Duration::from_secs(60));
        assert_eq!(
            step(&args(r#"{"step":"30s"}"#), "1m").unwrap(),
            Duration::from_secs(30)
        );
        assert!(step(&args(r#"{"step":"0s"}"#), "1m").is_err());
    }
}

//! imbhd — the reference HTTP server (ARCHITECTURE.md §10.16).
//!
//! A deliberately tiny HTTP/1.1 server over `std::net` (thread-per-connection), showing one way a
//! host can expose the imbh library over HTTP. It is **reference wiring, not the product** (§10.1):
//! no axum/hyper, so it adds no heavy dependencies and keeps the footprint story intact.
//!
//! Routes:
//! - `POST /v1/logs` · `/v1/traces` · `/v1/metrics` — OTLP/HTTP protobuf ingest (uncompressed).
//! - `POST /api/query` — a SQL string body → JSON rows.
//! - `GET /stats` — DB operational stats (per-table counts + buffer/WAL bytes + durable LSN) as JSON.
//! - `POST /admin/flush` · `/admin/compact` — maintenance actions (seal the buffer; force-merge
//!   segments). These are unauthenticated by design — a real deployment gates `/admin/*` itself.
//! - `GET /health` — liveness.
//!
//! OTLP/gRPC is available on a second port behind the optional `grpc` feature (see [`grpc`]); the
//! default build carries no gRPC transport. A Docker logging-driver plugin endpoint is available
//! behind the optional `docker` feature (see [`docker`]). Not handled here (follow-ups): gzip request
//! bodies, TLS, and the OTLP partial-success response shape.

#[cfg(all(feature = "docker", unix))]
pub mod docker;
#[cfg(feature = "grpc")]
pub mod grpc;

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};

use std::sync::Arc;

use imbh::Db;
use imbh::arrow::array::Array;
use imbh::arrow::record_batch::RecordBatch;
use imbh::arrow::util::display::{ArrayFormatter, FormatOptions};

/// A minimal HTTP response.
pub struct Response {
    pub status: u16,
    pub content_type: String,
    pub body: Vec<u8>,
}

impl Response {
    fn text(status: u16, s: &str) -> Self {
        Response {
            status,
            content_type: "text/plain".to_owned(),
            body: s.as_bytes().to_vec(),
        }
    }
    fn json(status: u16, body: Vec<u8>) -> Self {
        Response {
            status,
            content_type: "application/json".to_owned(),
            body,
        }
    }
    /// A response with an explicit content type — used by the Docker plugin endpoint, which must
    /// answer in `application/vnd.docker.plugins.v1.1+json`.
    #[cfg(all(feature = "docker", unix))]
    pub(crate) fn with_content_type(status: u16, content_type: &str, body: Vec<u8>) -> Self {
        Response {
            status,
            content_type: content_type.to_owned(),
            body,
        }
    }
}

/// Resolve `imbhd`'s HTTP listen address from the positional argument and the `IMBH_LISTEN_ADDR`
/// environment variable, in that order of precedence, falling back to `default`.
///
/// The environment variable exists for the Docker plugin (ARCHITECTURE.md §10.16). A managed
/// plugin's `entrypoint` args are baked into its `config.json` and cannot be changed without
/// rebuilding the plugin, whereas `env` entries declared `settable` can be changed at any time with
/// `docker plugin set` — so the listen address has to arrive as an environment variable to be
/// operator-tunable at all.
///
/// An **empty** value (`IMBH_LISTEN_ADDR=`) is not a missing value: it means *do not listen on TCP*.
/// That is the private posture — the log-driver plugin keeps working over its Unix socket while the
/// process opens no network port at all. Returns `None` in that case.
pub fn listen_addr(arg: Option<String>, env: Option<String>, default: &str) -> Option<String> {
    let chosen = arg
        .or(env)
        .unwrap_or_else(|| default.to_owned())
        .trim()
        .to_owned();
    (!chosen.is_empty()).then_some(chosen)
}

/// Serve `db` on `addr` (e.g. `127.0.0.1:4318`) until the process exits. Thread-per-connection;
/// each connection drives the async `Db` API on its own current-thread runtime.
pub fn serve(db: Arc<Db>, addr: &str) -> std::io::Result<()> {
    let listener = TcpListener::bind(addr)?;
    for stream in listener.incoming() {
        let stream = match stream {
            Ok(s) => s,
            Err(_) => continue,
        };
        let db = db.clone();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .build()
                .expect("build connection runtime");
            let _ = rt.block_on(handle_conn(db, stream));
        });
    }
    Ok(())
}

async fn handle_conn(db: Arc<Db>, mut stream: TcpStream) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let Some((method, path, body)) = read_request(&mut reader)? else {
        return Ok(());
    };
    let resp = route(&db, &method, &path, &body).await;
    write_response(&mut stream, &resp)
}

/// Dispatch a request to the imbh library. Exposed for testing without sockets.
#[cfg_attr(
    feature = "tracing",
    tracing::instrument(
        level = "info",
        name = "request",
        skip_all,
        fields(method, path, status = tracing::field::Empty)
    )
)]
pub async fn route(db: &Arc<Db>, method: &str, path: &str, body: &[u8]) -> Response {
    let resp = match (method, path) {
        ("GET", "/health") | ("GET", "/") => Response::text(200, "ok"),
        ("POST", "/v1/logs") => ingest_response(db.ingest_otlp_logs(body).await),
        ("POST", "/v1/traces") => ingest_response(db.ingest_otlp_traces(body).await),
        ("POST", "/v1/metrics") => ingest_response(db.ingest_otlp_metrics(body).await),
        ("POST", "/api/query") => query_response(db, body).await,
        ("GET", "/stats") => stats_response(db).await,
        ("POST", "/admin/flush") => match db.flush().await {
            Ok(()) => Response::json(200, b"{\"flushed\":true}".to_vec()),
            Err(e) => error_response(&e),
        },
        ("POST", "/admin/compact") => match db.compact().await {
            Ok(r) => Response::json(
                200,
                format!(
                    "{{\"segments_merged\":{},\"segments_created\":{}}}",
                    r.segments_merged, r.segments_created
                )
                .into_bytes(),
            ),
            Err(e) => error_response(&e),
        },
        _ => Response::text(404, "not found"),
    };
    #[cfg(feature = "tracing")]
    tracing::Span::current().record("status", resp.status);
    resp
}

/// `GET /stats` — the DB's operational stats as JSON (VM `/status/tsdb` analogue): per-table
/// segment/row/buffer counts and time span, plus buffer bytes, WAL bytes, and the durable LSN.
async fn stats_response(db: &Arc<Db>) -> Response {
    let stats = match db.stats().await {
        Ok(s) => s,
        Err(e) => return error_response(&e),
    };
    let opt = |v: Option<i64>| v.map_or("null".to_owned(), |n| n.to_string());
    let mut tables = String::from("[");
    for (i, t) in stats.tables.iter().enumerate() {
        if i > 0 {
            tables.push(',');
        }
        use std::fmt::Write as _;
        let _ = write!(
            tables,
            "{{\"table\":{},\"segment_count\":{},\"segment_rows\":{},\"buffer_rows\":{},\
             \"min_time_unix_nano\":{},\"max_time_unix_nano\":{}}}",
            json_string(t.table.as_str()),
            t.segment_count,
            t.segment_rows,
            t.buffer_rows,
            opt(t.min_time_unix_nano),
            opt(t.max_time_unix_nano),
        );
    }
    tables.push(']');
    let body = format!(
        "{{\"buffer_bytes\":{},\"wal_bytes\":{},\"durable_lsn\":{},\"tables\":{}}}",
        stats.buffer_bytes,
        stats.wal_bytes,
        stats.durable_lsn.map_or(0, |l| l.get()),
        tables,
    );
    Response::json(200, body.into_bytes())
}

fn ingest_response(result: imbh::Result<imbh::IngestReceipt>) -> Response {
    match result {
        Ok(r) => Response::json(
            200,
            format!(
                "{{\"accepted\":{},\"rejected\":{},\"durable\":{},\"queued\":{}}}",
                r.accepted,
                r.rejected,
                r.durable,
                r.is_queued()
            )
            .into_bytes(),
        ),
        Err(e) => error_response(&e),
    }
}

async fn query_response(db: &Arc<Db>, body: &[u8]) -> Response {
    let sql = match std::str::from_utf8(body) {
        Ok(s) => s,
        Err(_) => return Response::text(400, "query body is not UTF-8"),
    };
    match db.sql(sql).collect().await {
        Ok(batches) => Response::json(200, batches_to_json(&batches)),
        Err(e) => error_response(&e),
    }
}

/// Map an imbh error to an HTTP status using the §10.3 classifiers: 404 not-found, 400 user
/// error, 500 otherwise.
fn error_response(e: &imbh::Error) -> Response {
    let status = if e.is_not_found() {
        404
    } else if e.is_user_error() {
        400
    } else {
        500
    };
    Response::json(
        status,
        format!("{{\"error\":{}}}", json_string(&e.to_string())).into_bytes(),
    )
}

// ── JSON serialization of query results ─────────────────────────────────────────────────

/// Serialize result batches into a JSON array of row objects. Numeric columns render as JSON
/// numbers; everything else as JSON strings (via arrow's value formatter); nulls as `null`.
fn batches_to_json(batches: &[RecordBatch]) -> Vec<u8> {
    let mut out = String::from("[");
    let opts = FormatOptions::default();
    let mut first_row = true;
    for batch in batches {
        let names: Vec<String> = batch
            .schema()
            .fields()
            .iter()
            .map(|f| f.name().clone())
            .collect();
        // A column whose type arrow can't build a formatter for renders as `null` rather than
        // panicking the connection (`.ok()` instead of `.expect(...)`). Every type imbh emits is
        // supported, so this is defensive.
        let formatters: Vec<Option<ArrayFormatter>> = batch
            .columns()
            .iter()
            .map(|c| ArrayFormatter::try_new(c, &opts).ok())
            .collect();
        for row in 0..batch.num_rows() {
            if !first_row {
                out.push(',');
            }
            first_row = false;
            out.push('{');
            for (col, name) in names.iter().enumerate() {
                if col > 0 {
                    out.push(',');
                }
                out.push_str(&json_string(name));
                out.push(':');
                let array = batch.column(col);
                match formatters[col].as_ref() {
                    Some(f) if !array.is_null(row) => {
                        let value = f.value(row).to_string();
                        if is_numeric(array.data_type()) {
                            out.push_str(&value);
                        } else {
                            out.push_str(&json_string(&value));
                        }
                    }
                    _ => out.push_str("null"),
                }
            }
            out.push('}');
        }
    }
    out.push(']');
    out.into_bytes()
}

fn is_numeric(dt: &imbh::arrow::datatypes::DataType) -> bool {
    use imbh::arrow::datatypes::DataType::*;
    matches!(
        dt,
        Int8 | Int16 | Int32 | Int64 | UInt8 | UInt16 | UInt32 | UInt64 | Float32 | Float64
    )
}

/// JSON-quote and escape a string.
pub(crate) fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                use std::fmt::Write as _;
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

// ── minimal HTTP/1.1 ────────────────────────────────────────────────────────────────────

type ParsedRequest = (String, String, Vec<u8>);

/// Parse one HTTP/1.1 request (method, path, body) off `reader`; `None` on a clean EOF before the
/// request line. Generic over the reader so the same parser serves the TCP server and the Unix-socket
/// Docker plugin endpoint (`docker`), which speaks the same HTTP/1.1 dialect.
pub(crate) fn read_request<R: BufRead>(reader: &mut R) -> std::io::Result<Option<ParsedRequest>> {
    let mut request_line = String::new();
    if reader.read_line(&mut request_line)? == 0 {
        return Ok(None);
    }
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_owned();
    let raw_path = parts.next().unwrap_or_default();
    let path = raw_path.split('?').next().unwrap_or_default().to_owned();

    let mut content_length = 0usize;
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header)? == 0 {
            break;
        }
        let trimmed = header.trim_end();
        if trimmed.is_empty() {
            break;
        }
        if let Some((name, value)) = trimmed.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            content_length = value.trim().parse().unwrap_or(0);
        }
    }

    let mut body = vec![0u8; content_length];
    reader.read_exact(&mut body)?;
    Ok(Some((method, path, body)))
}

pub(crate) fn write_response<W: Write>(stream: &mut W, resp: &Response) -> std::io::Result<()> {
    let reason = match resp.status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        500 => "Internal Server Error",
        _ => "OK",
    };
    let header = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        resp.status,
        reason,
        resp.content_type,
        resp.body.len()
    );
    stream.write_all(header.as_bytes())?;
    stream.write_all(&resp.body)?;
    stream.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn listen_address_precedence() {
        let s = |v: &str| Some(v.to_owned());

        // Positional argument wins over the environment.
        assert_eq!(
            listen_addr(s("0.0.0.0:1"), s("10.0.0.1:2"), "127.0.0.1:4318"),
            s("0.0.0.0:1")
        );
        // Environment fills in when there is no argument — the Docker plugin's path, since a managed
        // plugin cannot change its entrypoint args without a rebuild.
        assert_eq!(
            listen_addr(None, s("172.17.0.1:4318"), "127.0.0.1:4318"),
            s("172.17.0.1:4318")
        );
        // Neither → the default.
        assert_eq!(
            listen_addr(None, None, "127.0.0.1:4318"),
            s("127.0.0.1:4318")
        );
    }

    #[test]
    fn an_empty_listen_address_means_do_not_listen() {
        // `IMBH_DOCKER_PLUGIN_SOCKET=… IMBH_LISTEN_ADDR= imbhd` is the private posture: the plugin
        // serves over its Unix socket and no TCP port is opened. Whitespace counts as empty so a
        // `docker plugin set IMBH_LISTEN_ADDR=" "` does not silently bind something.
        assert_eq!(
            listen_addr(None, Some(String::new()), "127.0.0.1:4318"),
            None
        );
        assert_eq!(
            listen_addr(None, Some("  ".to_owned()), "127.0.0.1:4318"),
            None
        );
        assert_eq!(
            listen_addr(Some(String::new()), None, "127.0.0.1:4318"),
            None
        );
        // An empty *default* with nothing else set is the same statement.
        assert_eq!(listen_addr(None, None, ""), None);
    }

    #[test]
    fn listen_address_is_trimmed() {
        // `docker plugin set` values arrive verbatim; a stray space would fail to parse as a socket
        // address and take the whole server down at bind time.
        assert_eq!(
            listen_addr(None, Some(" 172.17.0.1:4318\n".to_owned()), "x"),
            Some("172.17.0.1:4318".to_owned())
        );
    }

    fn otlp_log(service: &str, body_text: &str, time: u64) -> Vec<u8> {
        use opentelemetry_proto::tonic::collector::logs::v1::ExportLogsServiceRequest;
        use opentelemetry_proto::tonic::common::v1::{AnyValue, KeyValue, any_value};
        use opentelemetry_proto::tonic::logs::v1::{LogRecord, ResourceLogs, ScopeLogs};
        use opentelemetry_proto::tonic::resource::v1::Resource;
        use prost::Message;

        let sv = |s: &str| AnyValue {
            value: Some(any_value::Value::StringValue(s.to_owned())),
        };
        ExportLogsServiceRequest {
            resource_logs: vec![ResourceLogs {
                resource: Some(Resource {
                    attributes: vec![KeyValue {
                        key: "service.name".to_owned(),
                        value: Some(sv(service)),
                        ..Default::default()
                    }],
                    ..Default::default()
                }),
                scope_logs: vec![ScopeLogs {
                    log_records: vec![LogRecord {
                        time_unix_nano: time,
                        severity_number: 9,
                        body: Some(sv(body_text)),
                        ..Default::default()
                    }],
                    ..Default::default()
                }],
                ..Default::default()
            }],
        }
        .encode_to_vec()
    }

    /// A one-point OTLP explicit-bucket histogram (for exercising `List` columns over HTTP).
    fn otlp_hist(metric: &str, bounds: &[f64], counts: &[u64]) -> Vec<u8> {
        use opentelemetry_proto::tonic::collector::metrics::v1::ExportMetricsServiceRequest;
        use opentelemetry_proto::tonic::metrics::v1::{
            Histogram, HistogramDataPoint, Metric, ResourceMetrics, ScopeMetrics, metric,
        };
        use prost::Message;

        ExportMetricsServiceRequest {
            resource_metrics: vec![ResourceMetrics {
                scope_metrics: vec![ScopeMetrics {
                    metrics: vec![Metric {
                        name: metric.to_owned(),
                        data: Some(metric::Data::Histogram(Histogram {
                            data_points: vec![HistogramDataPoint {
                                time_unix_nano: 1,
                                count: counts.iter().sum(),
                                explicit_bounds: bounds.to_vec(),
                                bucket_counts: counts.to_vec(),
                                ..Default::default()
                            }],
                            aggregation_temporality: 2,
                        })),
                        ..Default::default()
                    }],
                    ..Default::default()
                }],
                ..Default::default()
            }],
        }
        .encode_to_vec()
    }

    #[tokio::test(flavor = "current_thread")]
    async fn query_list_column_renders() {
        let db = Db::in_memory().open().unwrap();
        assert_eq!(
            route(
                &db,
                "POST",
                "/v1/metrics",
                &otlp_hist("lat", &[1.0, 5.0], &[2, 3, 2])
            )
            .await
            .status,
            200
        );
        // A List column (`bucket_counts`) must serialize to JSON without panicking the connection.
        let q = route(
            &db,
            "POST",
            "/api/query",
            b"SELECT metric, bucket_counts FROM metrics_histogram",
        )
        .await;
        assert_eq!(q.status, 200);
        let json = String::from_utf8(q.body).unwrap();
        assert!(json.contains("\"metric\":\"lat\""), "got {json}");
        assert!(json.contains("bucket_counts"), "got {json}");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn health_ingest_query() {
        let db = Db::in_memory().open().unwrap();

        assert_eq!(route(&db, "GET", "/health", b"").await.status, 200);
        assert_eq!(route(&db, "GET", "/nope", b"").await.status, 404);

        // OTLP/HTTP logs ingest.
        let r = route(&db, "POST", "/v1/logs", &otlp_log("cart", "hello", 1)).await;
        assert_eq!(r.status, 200);
        assert!(
            String::from_utf8(r.body)
                .unwrap()
                .contains("\"accepted\":1")
        );

        // SQL query → JSON rows.
        let q = route(
            &db,
            "POST",
            "/api/query",
            b"SELECT service, count(*) AS c FROM logs GROUP BY service",
        )
        .await;
        assert_eq!(q.status, 200);
        let json = String::from_utf8(q.body).unwrap();
        assert!(json.contains("\"service\":\"cart\""), "got {json}");
        assert!(json.contains("\"c\":1"), "got {json}");

        // A bad query → 400.
        let bad = route(&db, "POST", "/api/query", b"SELECT nope FROM missing").await;
        assert_eq!(bad.status, 400);

        // GET /stats → operational JSON with the engine gauges and a logs table entry.
        let s = route(&db, "GET", "/stats", b"").await;
        assert_eq!(s.status, 200);
        let stats = String::from_utf8(s.body).unwrap();
        assert!(stats.contains("\"buffer_bytes\":"), "got {stats}");
        assert!(stats.contains("\"wal_bytes\":"), "got {stats}");
        assert!(stats.contains("\"durable_lsn\":"), "got {stats}");
        assert!(stats.contains("\"table\":\"logs\""), "got {stats}");
        assert!(stats.contains("\"buffer_rows\":1"), "got {stats}");

        // Admin maintenance actions return JSON results (no-op on this in-memory DB).
        let f = route(&db, "POST", "/admin/flush", b"").await;
        assert_eq!(f.status, 200);
        assert!(
            String::from_utf8(f.body)
                .unwrap()
                .contains("\"flushed\":true")
        );
        let c = route(&db, "POST", "/admin/compact", b"").await;
        assert_eq!(c.status, 200);
        assert!(
            String::from_utf8(c.body)
                .unwrap()
                .contains("\"segments_merged\":0")
        );
    }
}

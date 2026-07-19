//! [`DbLayer`] — a `tracing_subscriber::Layer` that sinks `tracing` into an embedded imbh [`Db`].
//!
//! Events become `logs` rows and closed spans become `spans` rows, built as OTLP protobuf and fed to
//! the same `Db::try_ingest_otlp_*` path a network exporter hits (mirroring `imbh-otel-exporter`, but
//! sourced from `tracing` directly — no OpenTelemetry SDK). Ingest is synchronous and non-blocking
//! (`try_ingest_*`), so `on_event`/`on_close` never need a runtime and never block the emitter.
//!
//! ## Reentrancy
//!
//! imbh's own crates emit `tracing` when built with `imbh/tracing`. Ingesting a captured record calls
//! into imbh, which emits more `tracing` — a feedback loop. A thread-local guard makes the layer
//! ignore any event/span-close produced on the same thread while it is already ingesting, so imbh's
//! internal telemetry during a sink write is dropped rather than re-ingested.
//!
//! ## Trace/span ids
//!
//! `tracing` has no OTel trace ids, so they are synthesized: a span's 8-byte span id is its
//! `tracing::Id`, and its 16-byte trace id is a per-layer nonce ++ the root span's id, inherited by
//! every descendant. Events carry the current span's trace/span ids so logs correlate to spans.

use std::cell::Cell;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use imbh::Db;
use opentelemetry_proto::tonic::collector::logs::v1::ExportLogsServiceRequest;
use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
use opentelemetry_proto::tonic::common::v1::{AnyValue, InstrumentationScope, KeyValue, any_value};
use opentelemetry_proto::tonic::logs::v1::{LogRecord, ResourceLogs, ScopeLogs};
use opentelemetry_proto::tonic::resource::v1::Resource;
use opentelemetry_proto::tonic::trace::v1::{ResourceSpans, ScopeSpans, Span};
use prost::Message as _;
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id};
use tracing::{Event, Level, Metadata, Subscriber};
use tracing_subscriber::layer::{Context, Layer};
use tracing_subscriber::registry::LookupSpan;

thread_local! {
    /// Set while this thread is ingesting a captured record, so nested imbh telemetry is dropped.
    static IN_INGEST: Cell<bool> = const { Cell::new(false) };
}

/// RAII reentrancy guard. `enter()` yields `None` (caller should bail) if the thread is already
/// ingesting; otherwise it sets the flag and clears it on drop.
struct Reentrancy;

impl Reentrancy {
    fn enter() -> Option<Self> {
        IN_INGEST.with(|c| {
            if c.get() {
                None
            } else {
                c.set(true);
                Some(Reentrancy)
            }
        })
    }
}

impl Drop for Reentrancy {
    fn drop(&mut self) {
        IN_INGEST.with(|c| c.set(false));
    }
}

/// A [`tracing_subscriber::Layer`] that ingests `tracing` spans and events into an embedded imbh
/// [`Db`] in-process. Compose it onto a `Registry` (see the crate docs); clone-and-share the `Db`
/// freely, the layer holds its own handle.
pub struct DbLayer {
    db: Arc<Db>,
    /// Prebuilt OTLP resource (carries `service.name` when configured), cloned into each request.
    resource: Resource,
    /// High 8 bytes of every synthesized trace id — distinguishes traces created by this layer.
    nonce: [u8; 8],
}

impl DbLayer {
    /// Build a layer writing into `db`, with no resource attributes.
    pub fn new(db: Arc<Db>) -> Self {
        DbLayer {
            db,
            resource: Resource::default(),
            nonce: gen_nonce(),
        }
    }

    /// Stamp `service.name = name` onto every ingested row's `resource` (→ the `service` column).
    pub fn with_service(mut self, name: impl Into<String>) -> Self {
        self.resource.attributes = vec![kv_str("service.name", &name.into())];
        self
    }

    /// The 16-byte trace id for a new root span: `nonce ++ span-id`.
    fn root_trace_id(&self, id: &Id) -> [u8; 16] {
        let mut t = [0u8; 16];
        t[..8].copy_from_slice(&self.nonce);
        t[8..].copy_from_slice(&id.into_u64().to_be_bytes());
        t
    }
}

impl std::fmt::Debug for DbLayer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DbLayer")
    }
}

/// Per-span state stashed in the registry's extensions at `on_new_span` and read at `on_close`.
struct SpanState {
    trace_id: [u8; 16],
    start_unix_nano: u64,
    attrs: Vec<KeyValue>,
}

impl<S> Layer<S> for DbLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(id) else { return };
        // Inherit the parent's trace id (walk is implicit — the parent already inherited its own),
        // or mint a fresh one for a root span.
        let trace_id = span
            .parent()
            .and_then(|p| p.extensions().get::<SpanState>().map(|s| s.trace_id))
            .unwrap_or_else(|| self.root_trace_id(id));

        let mut visitor = FieldVisitor::default();
        attrs.record(&mut visitor);

        span.extensions_mut().insert(SpanState {
            trace_id,
            start_unix_nano: now_unix_nano(),
            attrs: visitor.attrs,
        });
    }

    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        // Bail if already ingesting on this thread (drops imbh's own telemetry emitted mid-write).
        let Some(_guard) = Reentrancy::enter() else {
            return;
        };

        let mut visitor = FieldVisitor::default();
        event.record(&mut visitor);

        // Correlate to the span the event fired in, if any.
        let (trace_id, span_id) = match ctx.event_span(event) {
            Some(span) => {
                let tid = span.extensions().get::<SpanState>().map(|s| s.trace_id);
                (tid, Some(span.id().into_u64().to_be_bytes()))
            }
            None => (None, None),
        };

        let resource_logs = build_log(
            &self.resource,
            event.metadata(),
            visitor.message,
            visitor.attrs,
            trace_id,
            span_id,
        );
        let bytes = ExportLogsServiceRequest {
            resource_logs: vec![resource_logs],
        }
        .encode_to_vec();
        // Best-effort: a closed Db or backpressure drops the record rather than disrupt the emitter.
        let _ = self.db.try_ingest_otlp_logs(&bytes);
    }

    fn on_close(&self, id: Id, ctx: Context<'_, S>) {
        let Some(_guard) = Reentrancy::enter() else {
            return;
        };
        let Some(span) = ctx.span(&id) else { return };
        let end = now_unix_nano();

        // Copy everything out of the extensions before ingesting so no registry lock is held across
        // the Db write.
        let (trace_id, start, attrs) = {
            let ext = span.extensions();
            match ext.get::<SpanState>() {
                Some(s) => (s.trace_id, s.start_unix_nano, s.attrs.clone()),
                None => return,
            }
        };
        let span_id = id.into_u64().to_be_bytes();
        let parent_span_id = span.parent().map(|p| p.id().into_u64().to_be_bytes());

        let resource_spans = build_span(
            &self.resource,
            span.metadata(),
            trace_id,
            span_id,
            parent_span_id,
            start,
            end,
            attrs,
        );
        let bytes = ExportTraceServiceRequest {
            resource_spans: vec![resource_spans],
        }
        .encode_to_vec();
        let _ = self.db.try_ingest_otlp_traces(&bytes);
    }
}

// ── tracing field capture ─────────────────────────────────────────────────────────────────────────

/// Collects a span's or event's fields into OTLP [`KeyValue`]s, pulling the reserved `message`
/// field out as the log body.
#[derive(Default)]
struct FieldVisitor {
    message: Option<String>,
    attrs: Vec<KeyValue>,
}

impl FieldVisitor {
    fn put(&mut self, field: &Field, value: AnyValue) {
        self.attrs.push(KeyValue {
            key: field.name().to_owned(),
            value: Some(value),
            ..Default::default()
        });
    }
}

impl Visit for FieldVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        let s = format!("{value:?}");
        if field.name() == "message" {
            self.message = Some(s);
        } else {
            self.put(field, any_str(&s));
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message = Some(value.to_owned());
        } else {
            self.put(field, any_str(value));
        }
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.put(field, any_int(value));
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        // OTLP ints are i64; fall back to a double for the rare value above i64::MAX.
        match i64::try_from(value) {
            Ok(v) => self.put(field, any_int(v)),
            Err(_) => self.put(field, any_double(value as f64)),
        }
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.put(
            field,
            AnyValue {
                value: Some(any_value::Value::BoolValue(value)),
            },
        );
    }

    fn record_f64(&mut self, field: &Field, value: f64) {
        self.put(field, any_double(value));
    }
}

// ── OTLP construction ─────────────────────────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn build_span(
    resource: &Resource,
    meta: &Metadata<'_>,
    trace_id: [u8; 16],
    span_id: [u8; 8],
    parent_span_id: Option<[u8; 8]>,
    start_unix_nano: u64,
    end_unix_nano: u64,
    attrs: Vec<KeyValue>,
) -> ResourceSpans {
    let span = Span {
        trace_id: trace_id.to_vec(),
        span_id: span_id.to_vec(),
        parent_span_id: parent_span_id.map(|p| p.to_vec()).unwrap_or_default(),
        name: meta.name().to_owned(),
        kind: 1, // SPAN_KIND_INTERNAL — tracing spans have no OTel kind.
        start_time_unix_nano: start_unix_nano,
        end_time_unix_nano: end_unix_nano,
        attributes: attrs,
        ..Default::default()
    };
    ResourceSpans {
        resource: Some(resource.clone()),
        scope_spans: vec![ScopeSpans {
            scope: Some(scope(meta)),
            spans: vec![span],
            ..Default::default()
        }],
        ..Default::default()
    }
}

fn build_log(
    resource: &Resource,
    meta: &Metadata<'_>,
    message: Option<String>,
    attrs: Vec<KeyValue>,
    trace_id: Option<[u8; 16]>,
    span_id: Option<[u8; 8]>,
) -> ResourceLogs {
    let ts = now_unix_nano();
    let record = LogRecord {
        time_unix_nano: ts,
        observed_time_unix_nano: ts,
        severity_number: severity_number(meta.level()),
        severity_text: meta.level().as_str().to_owned(),
        body: Some(any_str(&message.unwrap_or_default())),
        attributes: attrs,
        trace_id: trace_id.map(|t| t.to_vec()).unwrap_or_default(),
        span_id: span_id.map(|s| s.to_vec()).unwrap_or_default(),
        ..Default::default()
    };
    ResourceLogs {
        resource: Some(resource.clone()),
        scope_logs: vec![ScopeLogs {
            scope: Some(scope(meta)),
            log_records: vec![record],
            ..Default::default()
        }],
        ..Default::default()
    }
}

/// The `tracing` target (e.g. `imbh_query`, or the caller's module path) as the OTLP instrumentation
/// scope — lands in imbh's `scope` column.
fn scope(meta: &Metadata<'_>) -> InstrumentationScope {
    InstrumentationScope {
        name: meta.target().to_owned(),
        ..Default::default()
    }
}

/// Map a `tracing` level to an OTLP severity number (ARCHITECTURE.md §6.2 severity model).
fn severity_number(level: &Level) -> i32 {
    match *level {
        Level::ERROR => 17,
        Level::WARN => 13,
        Level::INFO => 9,
        Level::DEBUG => 5,
        Level::TRACE => 1,
    }
}

fn any_str(v: &str) -> AnyValue {
    AnyValue {
        value: Some(any_value::Value::StringValue(v.to_owned())),
    }
}

fn any_int(v: i64) -> AnyValue {
    AnyValue {
        value: Some(any_value::Value::IntValue(v)),
    }
}

fn any_double(v: f64) -> AnyValue {
    AnyValue {
        value: Some(any_value::Value::DoubleValue(v)),
    }
}

fn kv_str(key: &str, value: &str) -> KeyValue {
    KeyValue {
        key: key.to_owned(),
        value: Some(any_str(value)),
        ..Default::default()
    }
}

fn now_unix_nano() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

/// A process-unique-ish 8-byte nonce for the high half of synthesized trace ids. Derived from the
/// wall clock XOR a monotonic counter, avoiding a `rand` dependency; uniqueness within a process is
/// what matters for correlating a trace's spans (the span-id low half already differs per span).
fn gen_nonce() -> [u8; 8] {
    static CTR: AtomicU64 = AtomicU64::new(0);
    let t = now_unix_nano();
    let c = CTR.fetch_add(1, Ordering::Relaxed);
    (t ^ c.rotate_left(32)).to_be_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracing_subscriber::prelude::*;

    #[test]
    fn events_and_spans_land_in_db() {
        let db = Db::in_memory().open().unwrap();

        // Emit one span containing one event, through a registry with the Db sink installed.
        let subscriber = tracing_subscriber::registry()
            .with(DbLayer::new(db.clone()).with_service("checkout-svc"));
        tracing::subscriber::with_default(subscriber, || {
            let span = tracing::info_span!("checkout", order = 42i64);
            let _entered = span.enter();
            tracing::info!(item = "book", "processing order");
            // `_entered` then `span` drop at block end → on_close ingests the span row.
        });

        // Query the rows back through the same tables an OTLP ingest would fill.
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let count = |sql: &str| -> usize {
            rt.block_on(async {
                db.sql(sql)
                    .collect()
                    .await
                    .unwrap()
                    .iter()
                    .map(|b| b.num_rows())
                    .sum()
            })
        };

        // The event landed in `logs` with its body, level→severity, and configured service.
        assert_eq!(
            count(
                "SELECT 1 FROM logs \
                 WHERE body = 'processing order' AND service = 'checkout-svc' AND severity_number = 9"
            ),
            1,
            "the tracing event became a log row"
        );
        // …correlated to its span (trace/span ids attached).
        assert_eq!(
            count("SELECT 1 FROM logs WHERE span_id IS NOT NULL AND trace_id IS NOT NULL"),
            1,
            "the log row carries the current span's ids"
        );
        // The closed span landed in `spans` with its name and service.
        assert_eq!(
            count("SELECT 1 FROM spans WHERE name = 'checkout' AND service = 'checkout-svc'"),
            1,
            "the tracing span became a span row"
        );
        // The log's span id matches the span's span id (in-process correlation holds).
        assert_eq!(
            count(
                "SELECT 1 FROM logs l JOIN spans s ON l.span_id = s.span_id \
                 WHERE l.trace_id = s.trace_id"
            ),
            1,
            "log and span share the synthesized trace/span ids"
        );
    }
}

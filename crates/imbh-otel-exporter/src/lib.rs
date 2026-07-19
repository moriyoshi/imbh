//! opentelemetry-rust exporter adapters that write directly into an embedded imbh [`Db`] (ARCHITECTURE.md
//! §12). Instead of shipping spans to a collector over the network, an in-process
//! `opentelemetry_sdk` pipeline can export straight into imbh — self-observation with zero hops.
//!
//! Each adapter converts the SDK's batch types into OTLP protobuf (reusing `opentelemetry-proto`'s
//! SDK→tonic transforms, already in the dependency tree) and feeds the bytes to the same
//! `Db::ingest_otlp_*` path a network exporter would hit — so ingest, WAL, and query behave
//! identically.
//!
//! Wiring (traces):
//! ```ignore
//! use opentelemetry_sdk::trace::{SdkTracerProvider, SimpleSpanProcessor};
//! let db = imbh::Db::in_memory().open()?;
//! let provider = SdkTracerProvider::builder()
//!     .with_span_processor(SimpleSpanProcessor::new(imbh_otel_exporter::ImbhSpanExporter::new(db.clone())))
//!     .build();
//! // …emit spans through `provider`; they land in `db`.
//! ```
//!
//! Scope: [`ImbhSpanExporter`] (traces), [`ImbhLogExporter`] (logs), and [`ImbhMetricExporter`]
//! (metrics) — the full OTLP signal set, each a thin `SDK batch → transform::… → OTLP bytes →
//! ingest_otlp_*` adapter.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use std::time::Duration;

use imbh::Db;
use opentelemetry_proto::tonic::collector::logs::v1::ExportLogsServiceRequest;
use opentelemetry_proto::tonic::collector::metrics::v1::ExportMetricsServiceRequest;
use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
use opentelemetry_proto::transform::common::tonic::ResourceAttributesWithSchema;
use opentelemetry_proto::transform::logs::tonic::group_logs_by_resource_and_scope;
use opentelemetry_proto::transform::trace::tonic::group_spans_by_resource_and_scope;
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::error::{OTelSdkError, OTelSdkResult};
use opentelemetry_sdk::logs::{LogBatch, LogExporter};
use opentelemetry_sdk::metrics::Temporality;
use opentelemetry_sdk::metrics::data::ResourceMetrics;
use opentelemetry_sdk::metrics::exporter::PushMetricExporter;
use opentelemetry_sdk::trace::{SpanData, SpanExporter};
use prost::Message;

/// An `opentelemetry_sdk` [`SpanExporter`] that ingests spans into an embedded imbh [`Db`].
///
/// Clone-and-share the `Db` freely; the exporter holds its own handle. The SDK's `Resource` (set
/// once at provider build) is captured via [`SpanExporter::set_resource`] and stamped onto every
/// exported batch.
pub struct ImbhSpanExporter {
    db: Arc<Db>,
    resource: Arc<Mutex<ResourceAttributesWithSchema>>,
    // Set by `shutdown`; once set, `export` rejects instead of ingesting (SDK trait contract).
    // `Arc`-shared so a future `Clone` would share one shutdown state (mirrors the resource handle).
    shutdown: Arc<AtomicBool>,
}

impl ImbhSpanExporter {
    /// Build an exporter writing into `db`.
    pub fn new(db: Arc<Db>) -> Self {
        ImbhSpanExporter {
            db,
            resource: Arc::new(Mutex::new(ResourceAttributesWithSchema::default())),
            shutdown: Arc::new(AtomicBool::new(false)),
        }
    }
}

impl std::fmt::Debug for ImbhSpanExporter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ImbhSpanExporter")
    }
}

impl SpanExporter for ImbhSpanExporter {
    fn export(
        &self,
        batch: Vec<SpanData>,
    ) -> impl std::future::Future<Output = OTelSdkResult> + Send {
        let db = self.db.clone();
        // Convert + encode synchronously (the lock spans the transform + encode), then move only the
        // bytes into the async ingest — `ResourceAttributesWithSchema` is not `Clone`, and this keeps
        // the lock off the await. Poison-recover (`into_inner`) rather than `.expect`: the guarded
        // value is a plain resource snapshot, so a panic elsewhere must not wedge the pipeline.
        // After `shutdown`, skip the transform entirely and reject the batch (`bytes` = `None`).
        let bytes = if self.shutdown.load(Ordering::Relaxed) {
            None
        } else {
            let resource = self.resource.lock().unwrap_or_else(|e| e.into_inner());
            let resource_spans = group_spans_by_resource_and_scope(batch, &resource);
            Some(ExportTraceServiceRequest { resource_spans }.encode_to_vec())
        };
        async move {
            match bytes {
                None => Err(OTelSdkError::AlreadyShutdown),
                Some(bytes) => db
                    .ingest_otlp_traces(&bytes)
                    .await
                    .map(|_| ())
                    .map_err(|e| OTelSdkError::InternalFailure(e.to_string())),
            }
        }
    }

    fn set_resource(&mut self, resource: &Resource) {
        *self.resource.lock().unwrap_or_else(|e| e.into_inner()) =
            ResourceAttributesWithSchema::from(resource);
    }

    fn shutdown_with_timeout(&self, _timeout: Duration) -> OTelSdkResult {
        // Mark shut down so later `export` calls reject. Idempotent: the SDK may call `shutdown`
        // more than once, so we simply (re-)set the flag and return `Ok`.
        self.shutdown.store(true, Ordering::Relaxed);
        Ok(())
    }
}

/// An `opentelemetry_sdk` [`LogExporter`] that ingests log records into an embedded imbh [`Db`].
/// Same shape as [`ImbhSpanExporter`], via the OTLP logs path.
pub struct ImbhLogExporter {
    db: Arc<Db>,
    resource: Arc<Mutex<ResourceAttributesWithSchema>>,
    // Set by `shutdown`; once set, `export` rejects instead of ingesting (SDK trait contract).
    shutdown: Arc<AtomicBool>,
}

impl ImbhLogExporter {
    /// Build a log exporter writing into `db`.
    pub fn new(db: Arc<Db>) -> Self {
        ImbhLogExporter {
            db,
            resource: Arc::new(Mutex::new(ResourceAttributesWithSchema::default())),
            shutdown: Arc::new(AtomicBool::new(false)),
        }
    }
}

impl std::fmt::Debug for ImbhLogExporter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ImbhLogExporter")
    }
}

impl LogExporter for ImbhLogExporter {
    fn export(
        &self,
        batch: LogBatch<'_>,
    ) -> impl std::future::Future<Output = OTelSdkResult> + Send {
        let db = self.db.clone();
        // Convert + encode synchronously so the returned future borrows neither `batch` nor the lock.
        // After `shutdown`, skip the transform entirely and reject the batch (`bytes` = `None`).
        let bytes = if self.shutdown.load(Ordering::Relaxed) {
            None
        } else {
            let resource = self.resource.lock().unwrap_or_else(|e| e.into_inner());
            let resource_logs = group_logs_by_resource_and_scope(&batch, &resource);
            Some(ExportLogsServiceRequest { resource_logs }.encode_to_vec())
        };
        async move {
            match bytes {
                None => Err(OTelSdkError::AlreadyShutdown),
                Some(bytes) => db
                    .ingest_otlp_logs(&bytes)
                    .await
                    .map(|_| ())
                    .map_err(|e| OTelSdkError::InternalFailure(e.to_string())),
            }
        }
    }

    fn set_resource(&mut self, resource: &Resource) {
        *self.resource.lock().unwrap_or_else(|e| e.into_inner()) =
            ResourceAttributesWithSchema::from(resource);
    }

    fn shutdown_with_timeout(&self, _timeout: Duration) -> OTelSdkResult {
        // Mark shut down so later `export` calls reject. Idempotent (see `ImbhSpanExporter`).
        self.shutdown.store(true, Ordering::Relaxed);
        Ok(())
    }
}

/// An `opentelemetry_sdk` [`PushMetricExporter`] that ingests metrics into an embedded imbh [`Db`].
///
/// Simpler than the span/log adapters: `ResourceMetrics` already carries its own `Resource`, so
/// there is no `set_resource` and no resource lock — the OTLP conversion is a single
/// `From<&ResourceMetrics>`.
///
/// Temporality defaults to [`Temporality::Cumulative`] (the OTel/SDK default; imbh's
/// `metrics().rate_counter()` reads cumulative counters). Use [`with_temporality`] to request
/// [`Temporality::Delta`] instead (paired with `metrics().rate()`), e.g. to avoid cumulative
/// baselines across restarts.
///
/// [`with_temporality`]: ImbhMetricExporter::with_temporality
pub struct ImbhMetricExporter {
    db: Arc<Db>,
    temporality: Temporality,
    // Set by `shutdown`; once set, `export` rejects instead of ingesting (the `PushMetricExporter`
    // contract: "After Shutdown is called, calls to Export will … return an error").
    shutdown: Arc<AtomicBool>,
}

impl ImbhMetricExporter {
    /// Build a metric exporter writing into `db`, requesting cumulative temporality.
    pub fn new(db: Arc<Db>) -> Self {
        ImbhMetricExporter {
            db,
            temporality: Temporality::Cumulative,
            shutdown: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Request a specific [`Temporality`] from the SDK's aggregation (default `Cumulative`).
    pub fn with_temporality(mut self, temporality: Temporality) -> Self {
        self.temporality = temporality;
        self
    }
}

impl std::fmt::Debug for ImbhMetricExporter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ImbhMetricExporter")
    }
}

impl PushMetricExporter for ImbhMetricExporter {
    fn export(
        &self,
        metrics: &ResourceMetrics,
    ) -> impl std::future::Future<Output = OTelSdkResult> + Send {
        let db = self.db.clone();
        // Convert + encode synchronously so the returned future does not borrow `metrics`.
        // After `shutdown`, skip the transform entirely and reject the batch (`bytes` = `None`).
        let bytes = if self.shutdown.load(Ordering::Relaxed) {
            None
        } else {
            Some(ExportMetricsServiceRequest::from(metrics).encode_to_vec())
        };
        async move {
            match bytes {
                None => Err(OTelSdkError::AlreadyShutdown),
                Some(bytes) => db
                    .ingest_otlp_metrics(&bytes)
                    .await
                    .map(|_| ())
                    .map_err(|e| OTelSdkError::InternalFailure(e.to_string())),
            }
        }
    }

    // Each `export` ingests immediately, so the exporter itself holds nothing to flush; imbh's own
    // durability is the host's concern via `Db::flush`.
    fn force_flush(&self) -> OTelSdkResult {
        Ok(())
    }

    fn shutdown_with_timeout(&self, _timeout: Duration) -> OTelSdkResult {
        // Mark shut down so later `export` calls reject. Idempotent (see `ImbhSpanExporter`).
        self.shutdown.store(true, Ordering::Relaxed);
        Ok(())
    }

    fn temporality(&self) -> Temporality {
        self.temporality
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use opentelemetry::trace::{Tracer, TracerProvider};
    use opentelemetry_sdk::trace::{SdkTracerProvider, SimpleSpanProcessor};

    #[tokio::test(flavor = "current_thread")]
    async fn exports_spans_into_db() {
        let db = Db::in_memory().open().unwrap();
        let provider = SdkTracerProvider::builder()
            .with_span_processor(SimpleSpanProcessor::new(ImbhSpanExporter::new(db.clone())))
            .build();

        // Emit a span; `SimpleSpanProcessor` exports it into `db` when it ends (block_on(export)).
        let tracer = provider.tracer("imbh-otel-exporter-test");
        tracer.in_span("checkout", |_cx| {});
        provider.force_flush().unwrap();

        // The span is now queryable through the same `spans` table an OTLP/HTTP ingest would fill.
        let batches = db.sql("SELECT name FROM spans").collect().await.unwrap();
        let total: usize = batches.iter().map(|b| b.num_rows()).sum();
        assert_eq!(total, 1, "the SDK span landed in imbh via the exporter");
        assert_eq!(
            crate::tests::first_string(&batches),
            Some("checkout".to_owned())
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn exports_logs_into_db() {
        use opentelemetry::logs::{LogRecord, Logger, LoggerProvider};
        use opentelemetry_sdk::logs::{SdkLoggerProvider, SimpleLogProcessor};

        let db = Db::in_memory().open().unwrap();
        let provider = SdkLoggerProvider::builder()
            .with_log_processor(SimpleLogProcessor::new(ImbhLogExporter::new(db.clone())))
            .build();

        // Emit a log record; `SimpleLogProcessor` exports it into `db` on emit (block_on(export)).
        let logger = provider.logger("imbh-otel-exporter-test");
        let mut record = logger.create_log_record();
        record.set_body("checkout failed".into());
        logger.emit(record);
        provider.force_flush().unwrap();

        // The record is now queryable through the same `logs` table an OTLP/HTTP ingest would fill.
        let batches = db.sql("SELECT body FROM logs").collect().await.unwrap();
        let total: usize = batches.iter().map(|b| b.num_rows()).sum();
        assert_eq!(total, 1, "the SDK log landed in imbh via the exporter");
        assert_eq!(
            crate::tests::first_string(&batches),
            Some("checkout failed".to_owned())
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn exports_metrics_into_db() {
        use opentelemetry::metrics::MeterProvider;
        use opentelemetry_sdk::metrics::{PeriodicReader, SdkMeterProvider};

        let db = Db::in_memory().open().unwrap();
        let reader = PeriodicReader::builder(ImbhMetricExporter::new(db.clone())).build();
        let provider = SdkMeterProvider::builder().with_reader(reader).build();

        // Record a monotonic counter; `force_flush` drives a collect+export cycle (block_on(export)).
        let meter = provider.meter("imbh-otel-exporter-test");
        let counter = meter.u64_counter("requests").build();
        counter.add(5, &[]);
        provider.force_flush().unwrap();

        // A cumulative counter exports as a monotonic Sum → the `metrics_sum` table. Filtering on
        // `value = 5` proves the data point (not just the metric name) round-tripped intact.
        let batches = db
            .sql("SELECT metric FROM metrics_sum WHERE metric = 'requests' AND value = 5")
            .collect()
            .await
            .unwrap();
        let total: usize = batches.iter().map(|b| b.num_rows()).sum();
        assert_eq!(
            total, 1,
            "the SDK counter (value 5) landed in imbh via the metric exporter"
        );
        assert_eq!(
            crate::tests::first_string(&batches),
            Some("requests".to_owned())
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn set_resource_stamps_service_on_spans() {
        // A provider-level `Resource` reaches the exporter via `SpanExporter::set_resource`; assert it
        // is stamped onto the exported row (the `service.name` attribute lands in the `service`
        // column). Without the resource plumbing, `service` would be null and the filter match zero.
        let db = Db::in_memory().open().unwrap();
        let provider = SdkTracerProvider::builder()
            .with_span_processor(SimpleSpanProcessor::new(ImbhSpanExporter::new(db.clone())))
            .with_resource(
                Resource::builder()
                    .with_service_name("checkout-svc")
                    .build(),
            )
            .build();

        let tracer = provider.tracer("imbh-otel-exporter-test");
        tracer.in_span("checkout", |_cx| {});
        provider.force_flush().unwrap();

        let batches = db
            .sql("SELECT service FROM spans WHERE service = 'checkout-svc'")
            .collect()
            .await
            .unwrap();
        let total: usize = batches.iter().map(|b| b.num_rows()).sum();
        assert_eq!(
            total, 1,
            "the provider resource (service.name) stamped the exported span row"
        );
        assert_eq!(
            crate::tests::first_string(&batches),
            Some("checkout-svc".to_owned())
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn set_resource_stamps_service_on_logs() {
        use opentelemetry::logs::{LogRecord, Logger, LoggerProvider};
        use opentelemetry_sdk::logs::{SdkLoggerProvider, SimpleLogProcessor};

        let db = Db::in_memory().open().unwrap();
        let provider = SdkLoggerProvider::builder()
            .with_log_processor(SimpleLogProcessor::new(ImbhLogExporter::new(db.clone())))
            .with_resource(
                Resource::builder()
                    .with_service_name("checkout-svc")
                    .build(),
            )
            .build();

        let logger = provider.logger("imbh-otel-exporter-test");
        let mut record = logger.create_log_record();
        record.set_body("checkout failed".into());
        logger.emit(record);
        provider.force_flush().unwrap();

        let batches = db
            .sql("SELECT service FROM logs WHERE service = 'checkout-svc'")
            .collect()
            .await
            .unwrap();
        let total: usize = batches.iter().map(|b| b.num_rows()).sum();
        assert_eq!(
            total, 1,
            "the provider resource (service.name) stamped the exported log row"
        );
        assert_eq!(
            crate::tests::first_string(&batches),
            Some("checkout-svc".to_owned())
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn resource_stamps_service_on_metrics() {
        // The metric exporter has no `set_resource`: `ResourceMetrics` carries its own `Resource`.
        // Assert the meter-provider resource still lands in the `service` column of the exported row.
        use opentelemetry::metrics::MeterProvider;
        use opentelemetry_sdk::metrics::{PeriodicReader, SdkMeterProvider};

        let db = Db::in_memory().open().unwrap();
        let reader = PeriodicReader::builder(ImbhMetricExporter::new(db.clone())).build();
        let provider = SdkMeterProvider::builder()
            .with_reader(reader)
            .with_resource(
                Resource::builder()
                    .with_service_name("checkout-svc")
                    .build(),
            )
            .build();

        let meter = provider.meter("imbh-otel-exporter-test");
        let counter = meter.u64_counter("requests").build();
        counter.add(5, &[]);
        provider.force_flush().unwrap();

        let batches = db
            .sql(
                "SELECT service FROM metrics_sum \
                 WHERE metric = 'requests' AND service = 'checkout-svc'",
            )
            .collect()
            .await
            .unwrap();
        let total: usize = batches.iter().map(|b| b.num_rows()).sum();
        assert_eq!(
            total, 1,
            "the meter-provider resource (service.name) stamped the exported metric row"
        );
        assert_eq!(
            crate::tests::first_string(&batches),
            Some("checkout-svc".to_owned())
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn export_after_shutdown_rejects() {
        // The SDK exporter contract: after `shutdown`, `export` must return an error and must not
        // ingest. Exercise each of the three exporters directly and assert `AlreadyShutdown`.
        let db = Db::in_memory().open().unwrap();

        let span_exporter = ImbhSpanExporter::new(db.clone());
        span_exporter.shutdown().unwrap();
        let err = span_exporter.export(Vec::new()).await.unwrap_err();
        assert!(
            matches!(err, OTelSdkError::AlreadyShutdown),
            "span export after shutdown returned {err:?}"
        );

        let log_exporter = ImbhLogExporter::new(db.clone());
        log_exporter.shutdown().unwrap();
        let err = log_exporter.export(LogBatch::new(&[])).await.unwrap_err();
        assert!(
            matches!(err, OTelSdkError::AlreadyShutdown),
            "log export after shutdown returned {err:?}"
        );

        let metric_exporter = ImbhMetricExporter::new(db.clone());
        metric_exporter.shutdown().unwrap();
        let err = metric_exporter
            .export(&ResourceMetrics::default())
            .await
            .unwrap_err();
        assert!(
            matches!(err, OTelSdkError::AlreadyShutdown),
            "metric export after shutdown returned {err:?}"
        );

        // The rejected exports must not have ingested anything.
        for table in ["spans", "logs", "metrics_sum"] {
            let batches = db
                .sql(&format!("SELECT * FROM {table}"))
                .collect()
                .await
                .unwrap();
            let total: usize = batches.iter().map(|b| b.num_rows()).sum();
            assert_eq!(total, 0, "no rows ingested into `{table}` after shutdown");
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn exports_delta_metrics_into_db() {
        // Mirror `exports_metrics_into_db`, but request delta temporality: the reader configures the
        // SDK aggregation to match, so the counter exports as a delta Sum (`temporality = 'DELTA'`)
        // rather than the default cumulative. Asserts the delta path lands in `metrics_sum` intact.
        use opentelemetry::metrics::MeterProvider;
        use opentelemetry_sdk::metrics::{PeriodicReader, SdkMeterProvider};

        let db = Db::in_memory().open().unwrap();
        let reader = PeriodicReader::builder(
            ImbhMetricExporter::new(db.clone()).with_temporality(Temporality::Delta),
        )
        .build();
        let provider = SdkMeterProvider::builder().with_reader(reader).build();

        let meter = provider.meter("imbh-otel-exporter-test");
        let counter = meter.u64_counter("requests").build();
        counter.add(5, &[]);
        provider.force_flush().unwrap();

        let batches = db
            .sql(
                "SELECT metric FROM metrics_sum \
                 WHERE metric = 'requests' AND value = 5 AND temporality = 'DELTA'",
            )
            .collect()
            .await
            .unwrap();
        let total: usize = batches.iter().map(|b| b.num_rows()).sum();
        assert_eq!(
            total, 1,
            "the SDK delta counter (value 5, DELTA) landed in imbh via the metric exporter"
        );
        assert_eq!(
            crate::tests::first_string(&batches),
            Some("requests".to_owned())
        );
    }

    fn first_string(batches: &[imbh::arrow::record_batch::RecordBatch]) -> Option<String> {
        use imbh::arrow::array::{Array, StringArray};
        use imbh::arrow::datatypes::DataType;
        let b = batches.first()?;
        // `service`/`resource`/`scope` are dict-encoded (`Dictionary(Int32, Utf8)`), so cast column 0
        // to `Utf8` first — this reads either a plain `StringArray` or a dictionary column uniformly.
        let col = imbh::arrow::compute::cast(b.column(0), &DataType::Utf8).ok()?;
        let col = col.as_any().downcast_ref::<StringArray>()?;
        (!col.is_empty() && !col.is_null(0)).then(|| col.value(0).to_owned())
    }
}

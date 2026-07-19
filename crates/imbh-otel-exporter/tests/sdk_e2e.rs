//! End-to-end: a self-observing application wires all three `opentelemetry_sdk` providers (traces,
//! logs, metrics) to the imbh exporters over a single **on-disk** `Db`, emits one of each signal,
//! seals, and reads them back through an **independent read-only handle** — proving the SDK →
//! exporter → ingest → WAL → seal → durable-read path holds for every signal at once, with a shared
//! provider `Resource`. The crate's unit tests already cover each exporter against an in-memory DB;
//! this adds on-disk durability + tri-signal in one setup + an independent reader.

use std::sync::Arc;

use imbh::{Db, WalMode};
use imbh_otel_exporter::{ImbhLogExporter, ImbhMetricExporter, ImbhSpanExporter};
use imbh_test_support::assert::int_at;

use opentelemetry::logs::{LogRecord, Logger, LoggerProvider};
use opentelemetry::metrics::MeterProvider;
use opentelemetry::trace::{Tracer, TracerProvider};
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::logs::{SdkLoggerProvider, SimpleLogProcessor};
use opentelemetry_sdk::metrics::{PeriodicReader, SdkMeterProvider};
use opentelemetry_sdk::trace::{SdkTracerProvider, SimpleSpanProcessor};

async fn count(db: &Arc<Db>, sql: &str) -> i64 {
    let batches = db.sql(sql).collect().await.expect("count query");
    int_at(&batches[0], 0)
}

fn service_resource() -> Resource {
    Resource::builder()
        .with_service_name("self-observing")
        .build()
}

#[tokio::test(flavor = "current_thread")]
async fn sdk_providers_export_all_signals_and_survive_reopen() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();

    let db: Arc<Db> = Db::builder(dir).wal(WalMode::Always).open().unwrap();

    // Three SDK providers, each exporting into the same on-disk Db, sharing one service resource.
    let tracer_provider = SdkTracerProvider::builder()
        .with_span_processor(SimpleSpanProcessor::new(ImbhSpanExporter::new(db.clone())))
        .with_resource(service_resource())
        .build();
    let logger_provider = SdkLoggerProvider::builder()
        .with_log_processor(SimpleLogProcessor::new(ImbhLogExporter::new(db.clone())))
        .with_resource(service_resource())
        .build();
    let reader = PeriodicReader::builder(ImbhMetricExporter::new(db.clone())).build();
    let meter_provider = SdkMeterProvider::builder()
        .with_reader(reader)
        .with_resource(service_resource())
        .build();

    // Emit one of each signal.
    tracer_provider.tracer("app").in_span("checkout", |_cx| {});
    let logger = logger_provider.logger("app");
    let mut record = logger.create_log_record();
    record.set_body("app started".into());
    logger.emit(record);
    meter_provider
        .meter("app")
        .u64_counter("requests")
        .build()
        .add(7, &[]);

    // Drive the export cycles.
    tracer_provider.force_flush().unwrap();
    logger_provider.force_flush().unwrap();
    meter_provider.force_flush().unwrap();

    // All three landed, queryable from the in-RAM buffer, stamped with the service.
    assert_eq!(
        count(
            &db,
            "SELECT count(*) AS c FROM spans WHERE service = 'self-observing'"
        )
        .await,
        1
    );
    assert_eq!(
        count(
            &db,
            "SELECT count(*) AS c FROM logs WHERE service = 'self-observing'"
        )
        .await,
        1
    );
    assert_eq!(
        count(
            &db,
            "SELECT count(*) AS c FROM metrics_sum WHERE metric = 'requests' AND value = 7"
        )
        .await,
        1
    );

    // Seal every signal to Parquet segments.
    db.flush().await.unwrap();

    // Verify durability through an **independent read-only handle** (before provider shutdown, since a
    // cumulative meter re-exports its counter on shutdown). A reader does not take the writer.lock, so
    // it opens the live directory and must see every sealed signal via the manifest + segments — the
    // same path a separate reader *process* uses. This exercises SDK → exporter → ingest → seal →
    // durable-read end to end without racing the writer's OS lock.
    let reader: Arc<Db> = Db::open_read_only(dir).expect("open read-only reader");
    assert_eq!(
        count(&reader, "SELECT count(*) AS c FROM spans").await,
        1,
        "span sealed + readable"
    );
    assert_eq!(
        count(&reader, "SELECT count(*) AS c FROM logs").await,
        1,
        "log sealed + readable"
    );
    assert_eq!(
        count(
            &reader,
            "SELECT count(*) AS c FROM metrics_sum WHERE metric = 'requests'"
        )
        .await,
        1,
        "metric sealed + readable"
    );
    drop(reader);

    tracer_provider.shutdown().unwrap();
    logger_provider.shutdown().unwrap();
    meter_provider.shutdown().unwrap();
    db.close().await.unwrap();
}

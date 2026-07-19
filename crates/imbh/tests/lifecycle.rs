//! Tri-signal lifecycle E2E: drive logs + traces + metrics through the full path — ingest → typed
//! and SQL query → seal → reopen (recover from segments) → compact (merge without loss) → Arrow-IPC
//! export round-trip → idempotent close — over one on-disk DB, and check the async and `blocking()`
//! twins agree. A focused second test exercises retention dropping aged segments.

use std::sync::Arc;

use imbh::arrow::ipc::reader::StreamReader;
use imbh::{Db, LogQuery, Retention, Table, TimeRange, WalMode};
use imbh_test_support::assert::int_at;
use imbh_test_support::otlp::{otlp_metrics, otlp_rich, otlp_trace_tree};
use imbh_test_support::rt::ct_rt;

async fn count_sql(db: &Arc<Db>, sql: &str) -> i64 {
    let batches = db.sql(sql).collect().await.expect("count query");
    int_at(&batches[0], 0)
}

#[test]
fn tri_signal_lifecycle_ingest_seal_reopen_compact_export() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let rt = ct_rt();

    // ── Ingest all three signals, query via typed APIs and SQL, then seal. ──────────────────
    let db: Arc<Db> = Db::builder(dir).wal(WalMode::Always).open().unwrap();
    rt.block_on(async {
        db.ingest_otlp_rich_ok("cart", "hello", 1, &[("env", "prod")])
            .await;
        db.ingest_otlp_rich_ok("checkout", "world", 2, &[]).await;
        // A root + child span sharing a trace id.
        db.ingest_otlp_traces(&otlp_trace_tree("cart", [0xcd; 16]))
            .await
            .unwrap();
        // cpu (gauge) + requests (cumulative sum).
        db.ingest_otlp_metrics(&otlp_metrics("cart")).await.unwrap();

        // Typed queries over the in-RAM buffer.
        assert_eq!(db.logs().count(LogQuery::new()).await.unwrap(), 2);
        assert!(
            !db.metrics().catalog().await.unwrap().is_empty(),
            "metric catalog lists the ingested metrics"
        );
        let attr_names = db.attrs().names().await.unwrap();
        assert!(
            attr_names.iter().any(|n| n == "env"),
            "attr discovery finds env: {attr_names:?}"
        );

        // SQL across every populated table.
        assert_eq!(count_sql(&db, "SELECT count(*) AS c FROM logs").await, 2);
        assert_eq!(count_sql(&db, "SELECT count(*) AS c FROM spans").await, 2);
        assert_eq!(
            count_sql(&db, "SELECT count(*) AS c FROM metrics_gauge").await,
            1
        );
        assert_eq!(
            count_sql(&db, "SELECT count(*) AS c FROM metrics_sum").await,
            1
        );

        db.flush().await.unwrap(); // seal buffers → Parquet segments (+ .tidx sidecars)
        db.close().await.unwrap();
    });
    // Drop the handle to release `writer.lock` (close() joins the maintenance worker; the OS advisory
    // lock is held until the last `Arc<Db>` is dropped) before reopening read-write.
    drop(db);

    // ── Reopen: everything recovers from the sealed segments via the manifest. ───────────────
    let db: Arc<Db> = Db::builder(dir).wal(WalMode::Always).open().unwrap();
    rt.block_on(async {
        assert_eq!(count_sql(&db, "SELECT count(*) AS c FROM logs").await, 2);
        assert_eq!(count_sql(&db, "SELECT count(*) AS c FROM spans").await, 2);

        // A second seal, then compact merges segments without losing rows.
        db.ingest_otlp_rich_ok("cart", "again", 3, &[]).await;
        db.flush().await.unwrap();
        db.compact().await.unwrap();
        assert_eq!(
            count_sql(&db, "SELECT count(*) AS c FROM logs").await,
            3,
            "compaction preserves rows"
        );

        // Arrow-IPC export of the logs table round-trips to the same row count.
        let bytes = db.export(Table::Logs, TimeRange::all()).await.unwrap();
        let reader = StreamReader::try_new(&bytes[..], None).unwrap();
        let exported: usize = reader.map(|b| b.unwrap().num_rows()).sum();
        assert_eq!(exported, 3, "exported Arrow IPC holds every logs row");
    });

    // ── Blocking facade agrees with the async path (called from sync context, no nested rt). ──
    let blocking = db.blocking();
    let batches = blocking.sql("SELECT count(*) AS c FROM logs").unwrap();
    assert_eq!(
        int_at(&batches[0], 0),
        3,
        "blocking twin agrees with async count"
    );

    // ── close() is idempotent. ──────────────────────────────────────────────────────────────
    rt.block_on(async {
        db.close().await.unwrap();
        db.close().await.unwrap();
    });
}

#[test]
fn retention_drops_aged_segments() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let rt = ct_rt();

    // Retain only the last day. The ingested rows are timestamped near the epoch (ancient), so once
    // sealed their segment is far older than the window and retention drops it.
    let db: Arc<Db> = Db::builder(dir)
        .wal(WalMode::Always)
        .retention(Retention::days(1))
        .open()
        .unwrap();
    rt.block_on(async {
        db.ingest_otlp_rich_ok("cart", "ancient", 1, &[]).await;
        db.flush().await.unwrap();
        assert_eq!(count_sql(&db, "SELECT count(*) AS c FROM logs").await, 1);

        // maintain() = seal + retention; the aged segment is reclaimed.
        db.maintain().await.unwrap();
        assert_eq!(
            count_sql(&db, "SELECT count(*) AS c FROM logs").await,
            0,
            "aged segment dropped by retention"
        );
        db.close().await.unwrap();
    });
}

/// Small ingest helper so the test bodies read cleanly.
trait IngestRich {
    async fn ingest_otlp_rich_ok(
        &self,
        service: &str,
        body: &str,
        time: u64,
        attrs: &[(&str, &str)],
    );
}

impl IngestRich for Arc<Db> {
    async fn ingest_otlp_rich_ok(
        &self,
        service: &str,
        body: &str,
        time: u64,
        attrs: &[(&str, &str)],
    ) {
        self.ingest_otlp_logs(&otlp_rich(service, body, time, 9, attrs))
            .await
            .expect("ingest logs");
    }
}

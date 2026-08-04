//! End-to-end coverage for the opt-in **async ingest** mode (`Ingest::Async`, ARCHITECTURE.md
//! §5/§10.5). The protobuf decode stays on the caller; the WAL + buffer write is offloaded to one
//! background worker task. These tests drive the worker on a current-thread runtime (the facade's
//! tokio has no `rt-multi-thread`), which also makes the overflow policies deterministic: the sync
//! `try_ingest_*` path never yields, so a queue filled by it stays full until the runtime is driven.

use std::sync::Arc;

use imbh::{Db, Ingest, Overflow, WalMode};
use imbh_test_support::assert::int_at;
use imbh_test_support::otlp::{otlp_log, otlp_metrics, otlp_trace_tree};
use imbh_test_support::rt::ct_rt;

async fn count(db: &Arc<Db>, sql: &str) -> i64 {
    int_at(&db.sql(sql).collect().await.expect("count query")[0], 0)
}

/// Async ingest across all three signals: every call returns a *queued* receipt (real `accepted`, no
/// `lsn`/`durable`), and `close()` drains the worker so nothing enqueued is lost — the reopened DB
/// sees every row.
#[test]
fn async_ingest_queues_then_close_drains_every_signal() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let rt = ct_rt();

    let db: Arc<Db> = Db::builder(dir)
        .wal(WalMode::Always)
        .ingest(Ingest::Async {
            handle: rt.handle().clone(),
            capacity: 8, // small, so the Block policy actually backpressures under the loop below
            overflow: Overflow::Block,
        })
        .open()
        .unwrap();

    rt.block_on(async {
        // 50 logs (more than the queue depth → the async path awaits slots as the worker drains).
        for i in 0..50u64 {
            let r = db
                .ingest_otlp_logs(&otlp_log("cart", "hello", i + 1))
                .await
                .unwrap();
            assert!(r.is_queued(), "async receipt is a queued ack");
            assert_eq!(r.accepted, 1, "decode still reports accepted on the caller");
            assert!(
                !r.durable && r.lsn.is_none(),
                "queued receipt carries no lsn/durable"
            );
        }
        // A two-span trace tree + one gauge + one sum, through the same queue.
        db.ingest_otlp_traces(&otlp_trace_tree("cart", [0xcd; 16]))
            .await
            .unwrap();
        db.ingest_otlp_metrics(&otlp_metrics("cart")).await.unwrap();

        // close() closes the channel, awaits the worker (drains the backlog), then seals.
        db.close().await.unwrap();
    });
    drop(db); // release the writer lock before reopening the same directory

    // Reopen inline (default sync mode) and confirm the worker persisted everything.
    let reopened: Arc<Db> = Db::builder(dir).wal(WalMode::Always).open().unwrap();
    rt.block_on(async {
        assert_eq!(count(&reopened, "SELECT count(*) c FROM logs").await, 50);
        assert_eq!(count(&reopened, "SELECT count(*) c FROM spans").await, 2);
        assert_eq!(
            count(&reopened, "SELECT count(*) c FROM metrics_gauge").await,
            1
        );
        assert_eq!(
            count(&reopened, "SELECT count(*) c FROM metrics_sum").await,
            1
        );
        reopened.close().await.unwrap();
    });
}

/// Under `WalMode::Always`, the async worker appends a drained burst without per-job fsync and then
/// group-commits once — so `durable_through` advances off the caller as the worker processes, and by
/// `close()` (which drains + commits + seals) covers every enqueued LSN.
#[test]
fn async_ingest_group_commit_advances_durable_under_always() {
    let tmp = tempfile::tempdir().unwrap();
    let rt = ct_rt();
    let db: Arc<Db> = Db::builder(tmp.path())
        .wal(WalMode::Always)
        .ingest(Ingest::Async {
            handle: rt.handle().clone(),
            capacity: 64,
            overflow: Overflow::Block,
        })
        .open()
        .unwrap();

    rt.block_on(async {
        for i in 0..10u64 {
            // Queued acks carry no lsn/durable — durability is confirmed globally, below.
            let r = db
                .ingest_otlp_logs(&otlp_log("s", "x", i + 1))
                .await
                .unwrap();
            assert!(r.is_queued() && !r.durable);
        }
        // The worker runs on this same runtime; yield until it drains the burst and group-commits.
        // Bounded so a real regression fails instead of hanging.
        let mut durable = db.durable_through().await;
        for _ in 0..1_000 {
            if durable.is_some() {
                break;
            }
            tokio::task::yield_now().await;
            durable = db.durable_through().await;
        }
        assert!(
            durable.is_some(),
            "async worker group-commits under WalMode::Always → durable_through advances off-caller"
        );

        // close() drains any remainder, group-commits, and seals — every enqueued LSN is now durable.
        db.close().await.unwrap();
    });
}

/// `Overflow::Fail`: once the bounded queue is full, the non-blocking `try_ingest_*` path fails fast
/// with a backpressure error. Deterministic because the runtime is never driven, so the worker never
/// pops.
#[test]
fn async_ingest_fail_policy_sheds_when_full() {
    let tmp = tempfile::tempdir().unwrap();
    let rt = ct_rt();
    let db: Arc<Db> = Db::builder(tmp.path())
        .ingest(Ingest::Async {
            handle: rt.handle().clone(),
            capacity: 2,
            overflow: Overflow::Fail,
        })
        .open()
        .unwrap();

    // Runtime not driven → worker idle → the 2-slot queue fills and stays full.
    assert!(
        db.try_ingest_otlp_logs(&otlp_log("s", "a", 1))
            .unwrap()
            .is_queued()
    );
    assert!(
        db.try_ingest_otlp_logs(&otlp_log("s", "b", 2))
            .unwrap()
            .is_queued()
    );
    let err = db.try_ingest_otlp_logs(&otlp_log("s", "c", 3)).unwrap_err();
    assert!(
        err.is_backpressure(),
        "full Fail queue → backpressure error: {err}"
    );

    rt.block_on(db.close()).unwrap();
}

/// `Overflow::DropOldest`: overflow evicts the oldest un-processed job (never errors), and the
/// eviction count surfaces in `stats().ingest_dropped`.
#[test]
fn async_ingest_drop_oldest_counts_evictions() {
    let tmp = tempfile::tempdir().unwrap();
    let rt = ct_rt();
    let db: Arc<Db> = Db::builder(tmp.path())
        .ingest(Ingest::Async {
            handle: rt.handle().clone(),
            capacity: 1,
            overflow: Overflow::DropOldest,
        })
        .open()
        .unwrap();

    // Three non-driven pushes into a depth-1 queue → two evictions, none rejected.
    for (i, tag) in [(1u64, "a"), (2, "b"), (3, "c")] {
        assert!(
            db.try_ingest_otlp_logs(&otlp_log("s", tag, i))
                .unwrap()
                .is_queued()
        );
    }
    let stats = rt.block_on(db.stats()).unwrap();
    assert_eq!(stats.ingest_dropped, 2, "two oldest jobs evicted");

    rt.block_on(db.close()).unwrap();
}

/// The default `Ingest::Sync` mode is unchanged: receipts are inline (`queued == false`, real `lsn`)
/// and rows are queryable immediately, before any flush.
#[test]
fn sync_mode_is_inline_and_immediately_visible() {
    let tmp = tempfile::tempdir().unwrap();
    let rt = ct_rt();
    let db: Arc<Db> = Db::builder(tmp.path()).wal(WalMode::Always).open().unwrap();
    rt.block_on(async {
        let r = db.ingest_otlp_logs(&otlp_log("s", "a", 1)).await.unwrap();
        assert!(!r.is_queued(), "sync mode is not queued");
        assert!(r.durable, "WalMode::Always awaiting path is durable");
        assert!(r.lsn.is_some(), "sync receipt carries a real lsn");
        assert_eq!(
            count(&db, "SELECT count(*) c FROM logs").await,
            1,
            "visible now"
        );
        let s = db.stats().await.unwrap();
        assert_eq!((s.ingest_queue_depth, s.ingest_dropped), (0, 0));
        db.close().await.unwrap();
    });
}

/// The duplicate guard runs at *decode* time, on the caller's thread, before the job is enqueued —
/// so a queued receipt still carries an exact `rejected` count even though nothing has been written
/// yet (issue #27). Putting the guard in the worker instead would make this impossible: the receipt
/// is returned before the worker ever runs.
#[test]
fn async_ingest_receipt_reports_rejected() {
    use imbh::Duplicates;
    use imbh_test_support::otlp::otlp_sum;

    let tmp = tempfile::tempdir().unwrap();
    let rt = ct_rt();
    let db: Arc<Db> = Db::builder(tmp.path())
        .wal(WalMode::Always)
        .duplicates(Duplicates::reject())
        .ingest(Ingest::Async {
            handle: rt.handle().clone(),
            capacity: 8,
            overflow: Overflow::Block,
        })
        .open()
        .unwrap();

    rt.block_on(async {
        let body = otlp_sum("cart", "m", 2, &[(10, 1.0)]);
        let first = db.ingest_otlp_metrics(&body).await.unwrap();
        let second = db.ingest_otlp_metrics(&body).await.unwrap();
        assert!(first.is_queued() && second.is_queued());
        assert_eq!((first.accepted, first.rejected), (1, 0));
        assert_eq!((second.accepted, second.rejected), (0, 1));
        db.close().await.unwrap();
    });
    // Reopen to count: `close()` drains the worker and seals, but a closed handle still rejects
    // queries — and still holds `writer.lock` until it is dropped.
    drop(db);
    let db: Arc<Db> = Db::builder(tmp.path()).open().unwrap();
    assert_eq!(
        rt.block_on(count(&db, "SELECT count(*) c FROM metrics_sum")),
        1
    );
}

/// Sync and async must make the same accept/reject decisions over the same input: both route
/// through the one `decode_metrics` choke point.
#[test]
fn async_ingest_dedup_matches_sync() {
    use imbh::Duplicates;
    use imbh_test_support::otlp::otlp_sum;

    let exports = || {
        [
            otlp_sum("cart", "m", 2, &[(10, 1.0), (10, 2.0)]),
            otlp_sum("cart", "m", 2, &[(10, 3.0)]),
            otlp_sum("cart", "m", 2, &[(20, 4.0)]),
        ]
    };
    let rt = ct_rt();

    let sync_tmp = tempfile::tempdir().unwrap();
    let sync_db: Arc<Db> = Db::builder(sync_tmp.path())
        .duplicates(Duplicates::reject())
        .open()
        .unwrap();
    let async_tmp = tempfile::tempdir().unwrap();
    let async_db: Arc<Db> = Db::builder(async_tmp.path())
        .duplicates(Duplicates::reject())
        .ingest(Ingest::Async {
            handle: rt.handle().clone(),
            capacity: 8,
            overflow: Overflow::Block,
        })
        .open()
        .unwrap();

    rt.block_on(async {
        let mut sync_receipts = Vec::new();
        let mut async_receipts = Vec::new();
        for body in exports() {
            sync_receipts.push(sync_db.ingest_otlp_metrics(&body).await.unwrap());
            async_receipts.push(async_db.ingest_otlp_metrics(&body).await.unwrap());
        }
        for (s, a) in sync_receipts.iter().zip(&async_receipts) {
            assert_eq!((s.accepted, s.rejected), (a.accepted, a.rejected));
        }
        sync_db.close().await.unwrap();
        async_db.close().await.unwrap();
    });
    // Drop both handles before reopening: the writer lock lives with the handle, not with `close()`.
    drop(sync_db);
    drop(async_db);

    let sql = "SELECT count(*) c FROM metrics_sum";
    let sync_db: Arc<Db> = Db::builder(sync_tmp.path()).open().unwrap();
    let async_db: Arc<Db> = Db::builder(async_tmp.path()).open().unwrap();
    let (sync_rows, async_rows) =
        rt.block_on(async { (count(&sync_db, sql).await, count(&async_db, sql).await) });
    assert_eq!(
        sync_rows, async_rows,
        "sync and async made the same decisions"
    );
    assert_eq!(sync_rows, 2);
}

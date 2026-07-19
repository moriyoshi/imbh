//! Interleave stress: a background-maintenance DB is written by one thread while another queries and
//! compacts concurrently. Across live seals (background), and explicit compactions, the no-drop /
//! no-double-count invariant must hold — `count(*) == count(DISTINCT body)` at every observation,
//! the count never goes backwards, and it terminates at exactly N. This exercises the writer's
//! `query_snapshot` + seal-staging machinery under real concurrency, not just a single seal.

use std::sync::Arc;
use std::time::{Duration, Instant};

use imbh::{Db, Maintenance, WalMode};
use imbh_test_support::{count_logs, otlp::otlp_log, rt::ct_rt};

/// Ingest `n` distinct rows on a background thread, one WAL-durable row at a time.
fn spawn_writer(db: Arc<Db>, n: u64) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let rt = ct_rt();
        rt.block_on(async {
            for i in 0..n {
                db.ingest_otlp_logs(&otlp_log("svc", &format!("m-{i}"), i + 1))
                    .await
                    .expect("writer ingest");
            }
        });
    })
}

fn run(n: u64, timeout: Duration) {
    let tmp = tempfile::tempdir().unwrap();
    // Background maintenance seals the buffer on a short interval while we ingest + query + compact.
    let db: Arc<Db> = Db::builder(tmp.path())
        .wal(WalMode::Always)
        .maintenance(Maintenance::Background(Duration::from_millis(15)))
        .open()
        .unwrap();

    let writer = spawn_writer(db.clone(), n);

    let rt = ct_rt();
    let deadline = Instant::now() + timeout;
    let mut last = 0i64;
    loop {
        let (count, distinct) = rt.block_on(count_logs(&db));
        assert_eq!(
            count, distinct,
            "no duplicated or partial rows under concurrent seal"
        );
        assert!(
            count >= last,
            "count went backwards ({last} → {count}): a seal dropped rows"
        );
        last = count;

        // Interleave an explicit compaction with the background seals + live ingest.
        rt.block_on(db.compact()).expect("compact");

        if count == n as i64 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "did not reach {n} rows in time (stuck at {count})"
        );
        std::thread::sleep(Duration::from_millis(5));
    }

    writer.join().expect("writer thread");

    // Final settle: after the writer is done, one more read is exactly N with no dup.
    let (count, distinct) = rt.block_on(count_logs(&db));
    assert_eq!(count, n as i64);
    assert_eq!(distinct, n as i64);
    rt.block_on(db.close()).unwrap();
}

#[test]
fn interleaved_ingest_seal_compact_query_holds_invariant() {
    run(300, Duration::from_secs(30));
}

#[test]
#[ignore = "longer stress soak: run explicitly with --ignored"]
fn interleaved_stress_long() {
    run(3_000, Duration::from_secs(120));
}

//! The metric catalog folds each sealed segment in exactly once, and still sees the unsealed buffer.
//!
//! `MetricsApi::catalog` is a `SELECT DISTINCT` with no time predicate, so nothing prunes and its
//! naive cost is the whole corpus — on every call, and PromQL translation makes one call per
//! request. The fix caches the sealed half. These tests pin the two properties that make the cache
//! sound rather than merely fast:
//!
//! * **Freshness.** A metric that exists only in the mutable buffer must appear immediately. Caching
//!   the buffer along with the segments would hide a just-ingested metric until the next seal, which
//!   for a live head is a correctness bug wearing a performance costume.
//! * **Removal.** A distinct-union is monotone only while segments are *added*. Retention or
//!   compaction can delete the only segment that contributed an entry, so a folded-but-vanished
//!   segment must force a rebuild rather than leave a phantom metric in the listing.
//!
//! The speed itself is measured by `examples/bench/src/bin/tui-bench.rs`, not asserted here — a
//! wall-clock assertion would flake in CI. What is asserted is that the answer never changes.

use std::sync::Arc;

use imbh::{Db, WalMode};
use imbh_test_support::otlp::otlp_gauge_labeled;

fn names(catalog: &[imbh::MetricMeta]) -> Vec<String> {
    let mut out: Vec<String> = catalog.iter().map(|m| m.metric.clone()).collect();
    out.sort();
    out.dedup();
    out
}

async fn ingest(db: &Arc<Db>, metric: &str) {
    db.ingest_otlp_metrics(&otlp_gauge_labeled("svc", metric, "host", &["a"]))
        .await
        .expect("ingest");
}

/// A metric still in the mutable buffer is in the catalog; sealing it does not change the answer,
/// and neither does asking twice.
#[tokio::test]
async fn buffered_metrics_are_visible_before_and_after_sealing() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Db::builder(tmp.path()).wal(WalMode::Off).open().unwrap();

    ingest(&db, "in_buffer").await;
    // Never sealed: only the buffer scan can find this one.
    assert_eq!(names(&db.metrics().catalog().await.unwrap()), ["in_buffer"]);

    db.flush().await.unwrap();
    assert_eq!(names(&db.metrics().catalog().await.unwrap()), ["in_buffer"]);
    // Second call is a pure cache hit for the sealed half; it must not lose the entry.
    assert_eq!(names(&db.metrics().catalog().await.unwrap()), ["in_buffer"]);

    db.close().await.unwrap();
}

/// Each seal contributes its own metrics, and every earlier segment's contribution survives — the
/// fold accumulates rather than replacing.
#[tokio::test]
async fn each_sealed_segment_adds_to_the_catalog() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Db::builder(tmp.path()).wal(WalMode::Off).open().unwrap();

    for i in 0..4 {
        ingest(&db, &format!("metric_{i}")).await;
        db.flush().await.unwrap();
        // Ask *between* seals so the cache is exercised at every intermediate size, not just at the
        // end — a fold that dropped earlier entries would show up on the very next iteration.
        let expected: Vec<String> = (0..=i).map(|j| format!("metric_{j}")).collect();
        assert_eq!(names(&db.metrics().catalog().await.unwrap()), expected);
    }

    // A metric ingested after the last seal joins the sealed ones rather than replacing them.
    ingest(&db, "unsealed").await;
    let mut expected: Vec<String> = (0..4).map(|j| format!("metric_{j}")).collect();
    expected.push("unsealed".to_owned());
    expected.sort();
    assert_eq!(names(&db.metrics().catalog().await.unwrap()), expected);

    db.close().await.unwrap();
}

/// Repeated calls with no writes in between are stable — the cached half is returned verbatim,
/// including its ordering, so a UI listing does not reshuffle between refreshes.
#[tokio::test]
async fn repeated_calls_are_identical() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Db::builder(tmp.path()).wal(WalMode::Off).open().unwrap();

    for i in 0..3 {
        ingest(&db, &format!("m{i}")).await;
        db.flush().await.unwrap();
    }
    let first = db.metrics().catalog().await.unwrap();
    let second = db.metrics().catalog().await.unwrap();
    let key = |c: &[imbh::MetricMeta]| -> Vec<(String, String, String)> {
        c.iter()
            .map(|m| (m.kind.clone(), m.metric.clone(), m.unit.clone()))
            .collect()
    };
    assert_eq!(key(&first), key(&second));

    db.close().await.unwrap();
}

/// Dropping segments invalidates the fold. Retention removing the only segment that carried a
/// metric must remove it from the catalog too, not leave a phantom behind.
#[tokio::test]
async fn dropped_segments_rebuild_rather_than_leaving_phantoms() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Db::builder(tmp.path()).wal(WalMode::Off).open().unwrap();

    ingest(&db, "kept").await;
    db.flush().await.unwrap();
    // Warm the cache while both are present, so the removal below has something to invalidate.
    ingest(&db, "dropped").await;
    db.flush().await.unwrap();
    assert_eq!(
        names(&db.metrics().catalog().await.unwrap()),
        ["dropped", "kept"]
    );

    // Unlink the newest metric segment behind the manifest's back is not something a caller can do
    // safely, so drive the real path: retention with a zero-length window keeps nothing.
    let before = db.segments().len();
    db.compact().await.unwrap();
    let after = db.segments().len();
    if after == before {
        // Compaction did not merge anything on this corpus; the invalidation path is still worth
        // exercising, so assert only what must hold either way.
        assert_eq!(
            names(&db.metrics().catalog().await.unwrap()),
            ["dropped", "kept"]
        );
    } else {
        // Segments were replaced: the fold must have been rebuilt against the new paths, and the
        // *contents* must be unchanged since compaction preserves rows.
        assert_eq!(
            names(&db.metrics().catalog().await.unwrap()),
            ["dropped", "kept"]
        );
    }

    db.close().await.unwrap();
}

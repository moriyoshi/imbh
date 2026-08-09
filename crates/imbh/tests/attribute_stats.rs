//! [`Db::attribute_stats`] over an open database (the `attrstats` feature).
//!
//! The measurement itself is covered where it lives (`imbh-attrstats`'s `end_to_end` suite, against
//! hand-built segment fixtures). What this pins is the *facade contract*: which handles can answer,
//! what a handle with nothing on disk says, and that a running writer is neither disturbed by the
//! scan nor required to stop for it.
//!
//! Run standalone with `cargo test -p imbh --features attrstats`; the workspace build has the
//! feature on, since `imbh-head` (and so `imbhd` and the TUI) depends on it.
#![cfg(feature = "attrstats")]

use std::sync::Arc;

use imbh::attrstats::{AttrScope, Options};
use imbh::{Db, Table};
use imbh_test_support::otlp::otlp_log;

/// A writer with one sealed segment of `service=cart` logs.
async fn writer(dir: &std::path::Path) -> Arc<Db> {
    let db = Db::builder(dir).open().expect("open");
    for i in 0..3 {
        db.ingest_otlp_logs(&otlp_log("cart", "checkout failed", 1_000 + i))
            .await
            .expect("ingest");
    }
    db.flush().await.expect("flush");
    db
}

/// The writer measures its own database — no lock, no flush, no pause. The keys that come back are
/// the ones in the sealed segments, scoped exactly as `promote` would see them.
#[tokio::test]
async fn a_writer_measures_its_own_database() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = writer(dir.path()).await;

    let report = db
        .attribute_stats(&Options::default())
        .await
        .expect("attribute stats");
    assert_eq!(
        report.dir,
        dir.path().display().to_string(),
        "the report names the database it measured"
    );
    let logs = report.table(Table::Logs.as_str()).expect("logs unit");
    assert!(logs.segments >= 1, "the flush sealed a segment");
    assert!(logs.rows >= 3);
    let service = logs
        .key("resource:service.name")
        .expect("the resource scope is read and prefixed");
    assert_eq!(service.scope, AttrScope::Resource);
    assert_eq!(service.distinct_est, 1.0, "one service in the fixture");

    // Ingest still works afterwards: the scan took no lock and left the database alone.
    db.ingest_otlp_logs(&otlp_log("cart", "after the scan", 2_000))
        .await
        .expect("ingest after the scan");
    db.close().await.expect("close");
}

/// A read-only handle answers the same question the writer does — that is what lets a head, a CLI,
/// or a housekeeper measure a database someone else is writing.
#[tokio::test]
async fn a_read_only_handle_measures_the_same_database() {
    let dir = tempfile::tempdir().expect("tempdir");
    let writer = writer(dir.path()).await;

    let reader = Db::open_read_only(dir.path()).expect("open read-only");
    let from_reader = reader
        .attribute_stats(&Options::default())
        .await
        .expect("read-only attribute stats");
    let from_writer = writer
        .attribute_stats(&Options::default())
        .await
        .expect("writer attribute stats");
    assert_eq!(
        from_reader.global.rows, from_writer.global.rows,
        "both derive their segment set from the same on-disk manifest"
    );
    assert_eq!(from_reader.global.keys.len(), from_writer.global.keys.len());
    writer.close().await.expect("close");
}

/// Rows that are still in the buffer are in no segment, so they cannot be selective within one.
/// The report says how many WAL frames it skipped instead of quietly counting them as absent.
#[tokio::test]
async fn buffered_rows_are_excluded_and_the_report_says_so() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = Db::builder(dir.path()).open().expect("open");
    db.ingest_otlp_logs(&otlp_log("cart", "still buffered", 1_000))
        .await
        .expect("ingest");

    let before = db
        .attribute_stats(&Options::default())
        .await
        .expect("stats");
    assert_eq!(before.global.segments, 0, "nothing sealed yet");
    assert!(
        before.pending_wal_frames > 0,
        "the unsealed WAL tail must be reported, not silently omitted"
    );

    db.flush().await.expect("flush");
    let after = db
        .attribute_stats(&Options::default())
        .await
        .expect("stats");
    assert!(after.global.segments >= 1, "the flush made them measurable");
    assert!(after.global.rows >= 1);
    db.close().await.expect("close");
}

/// An in-memory database has no segments to measure. Saying so is the honest answer: an empty
/// report would read as "this database has no attributes", which is a different claim.
#[tokio::test]
async fn an_in_memory_database_is_refused_rather_than_reported_empty() {
    let db = Db::in_memory().open().expect("open");
    db.ingest_otlp_logs(&otlp_log("cart", "in memory", 1_000))
        .await
        .expect("ingest");
    let error = db
        .attribute_stats(&Options::default())
        .await
        .expect_err("no directory, no segments");
    assert!(error.is_user_error(), "a 400, not a 500: {error}");
    assert!(error.to_string().contains("in-memory"), "{error}");
}

/// Options that cannot produce a curve are refused before any segment is opened, so a caller gets
/// the diagnostic rather than numbers that cannot be compared to each other.
#[tokio::test]
async fn invalid_options_are_refused_before_the_scan() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = writer(dir.path()).await;
    let mut options = Options::default();
    options.windows.reverse();
    let error = db
        .attribute_stats(&options)
        .await
        .expect_err("a decreasing ladder is not a curve");
    assert!(error.is_user_error(), "{error}");
    db.close().await.expect("close");
}

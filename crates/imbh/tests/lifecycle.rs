//! Tri-signal lifecycle E2E: drive logs + traces + metrics through the full path — ingest → typed
//! and SQL query → seal → reopen (recover from segments) → compact (merge without loss) → Arrow-IPC
//! export round-trip → idempotent close — over one on-disk DB, and check the async and `blocking()`
//! twins agree. A focused second test exercises retention dropping aged segments.

use std::sync::Arc;

use imbh::arrow::ipc::reader::StreamReader;
use imbh::{
    Compression, Db, LogQuery, Promote, Retention, Table, TimeRange, WalMode, prepare_pending,
};
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

/// Every regular file that makes up a sealed segment: the Parquet file plus everything inside its
/// `.tidx` sidecar directory.
fn segment_parts(parquet: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = vec![parquet.to_path_buf()];
    let mut stack = vec![parquet.with_extension("tidx")];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                stack.push(p);
            } else {
                out.push(p);
            }
        }
    }
    out
}

/// Open (and keep open) every file backing `segments` — the handles a query in flight holds while
/// it streams a segment's Parquet and consults its `.tidx`.
fn pin(segments: &[std::path::PathBuf]) -> Vec<std::fs::File> {
    let mut held = Vec::new();
    for seg in segments {
        for part in segment_parts(seg) {
            held.push(std::fs::File::open(&part).unwrap_or_else(|e| panic!("open {part:?}: {e}")));
        }
    }
    held
}

/// **Windows portability (issue #3 shape), through the facade.** `compact()` and `maintain()` both
/// finish by unlinking segment files the manifest no longer names. POSIX allows unlinking a file
/// that is still open; Windows only tolerates it for handles opened with delete sharing, and refuses
/// outright for a file that is memory-mapped (which a Tantivy searcher over a `.tidx` sidecar does).
/// So the ordering that matters is a reader alive *across* the reclaim, not a reclaim on an idle DB
/// — this test stages it for both passes over one on-disk database. (The mapped-file half of the
/// hazard is covered by `imbh-storage`'s own reclaim tests, which run on the same Windows CI leg.)
#[test]
fn compaction_and_retention_reclaim_segments_pinned_by_open_readers() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let rt = ct_rt();

    // Disk budget 0 → once maintenance runs, retention drops every sealed segment.
    let db: Arc<Db> = Db::builder(dir)
        .wal(WalMode::Always)
        .retention(Retention::none().max_disk_bytes(0))
        .open()
        .unwrap();
    rt.block_on(async {
        // Two seals in the same UTC-day partition → two compactable source segments.
        db.ingest_otlp_rich_ok("cart", "alpha", 1, &[("env", "prod")])
            .await;
        db.flush().await.unwrap();
        db.ingest_otlp_rich_ok("cart", "beta", 2, &[("env", "prod")])
            .await;
        db.flush().await.unwrap();
        let sources = db.segment_files(Table::Logs).unwrap();
        assert_eq!(sources.len(), 2, "two sealed logs segments to merge");

        // Pin both sources BEFORE compaction and hold them across it.
        let held = pin(&sources);
        assert!(
            held.len() > sources.len(),
            ".tidx sidecar files are held too"
        );
        db.compact()
            .await
            .expect("compaction must not fail because a reader holds the sources");
        assert_eq!(
            count_sql(&db, "SELECT count(*) AS c FROM logs").await,
            2,
            "compaction preserves rows while the sources are pinned"
        );
        drop(held);

        // Same story for retention: pin the merged segment, then let maintenance reclaim it.
        let merged = db.segment_files(Table::Logs).unwrap();
        assert_eq!(merged.len(), 1, "the two sources merged into one segment");
        let held = pin(&merged);
        let report = db
            .maintain()
            .await
            .expect("retention must not fail because a reader holds the segment");
        assert!(
            report.segments_dropped >= 1,
            "the pinned segment is dropped"
        );
        assert_eq!(
            count_sql(&db, "SELECT count(*) AS c FROM logs").await,
            0,
            "retention dropped the segment while it was pinned"
        );
        drop(held);
        db.close().await.unwrap();
    });
    drop(db);

    // Reopen: nothing dangles. Any file the OS refused to unlink is an orphan the open sweeps.
    let db: Arc<Db> = Db::builder(dir).wal(WalMode::Always).open().unwrap();
    rt.block_on(async {
        assert_eq!(count_sql(&db, "SELECT count(*) AS c FROM logs").await, 0);
        assert!(
            db.segment_files(Table::Logs).unwrap().is_empty(),
            "no segment survives the reclaim"
        );
        db.close().await.unwrap();
    });
    let mut leftovers = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for entry in entries.flatten() {
            let p = entry.path();
            match p.extension().and_then(|e| e.to_str()) {
                Some("parquet") | Some("tidx") => leftovers.push(p),
                _ if p.is_dir() => stack.push(p),
                _ => {}
            }
        }
    }
    assert!(
        leftovers.is_empty(),
        "no segment bytes are left behind after the reclaim + orphan sweep: {leftovers:?}"
    );
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

/// **Compaction converges promoted history instead of baking a NULL hole.**
///
/// `promote` is not retroactive: a segment sealed before a key was promoted has no column for it, and
/// normalising it to the live schema null-fills that column. Answers stay correct either way —
/// `attr_field` emits a `CASE` whose NULL arm reads the key back out of the retained `attributes`
/// JSON — so this is a *convergence* defect, not a wrong answer. But compaction is the one operation
/// that rewrites those rows, and null-filling there makes the fallback permanent: every query on the
/// key pays a JSON parse over that data for the life of the merged segment.
///
/// After compaction the column must be **populated** from the JSON, so the merged segment is what it
/// would have been had the key been promoted from the start.
#[test]
fn compaction_projects_promoted_columns_from_retained_json() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let rt = ct_rt();
    let db: Arc<Db> = Db::builder(dir).open().unwrap();
    rt.block_on(async {
        // Sealed BEFORE `env` is promoted: no column, the value lives only in the JSON blob.
        db.ingest_otlp_rich_ok("cart", "before", 1, &[("env", "prod")])
            .await;
        db.flush().await.unwrap();

        // Promote, then seal a second segment in the same UTC-day partition WITH the column.
        db.set_promote(Promote::new(["env"])).await.unwrap();
        db.ingest_otlp_rich_ok("cart", "after", 2, &[("env", "prod")])
            .await;
        db.flush().await.unwrap();
        assert_eq!(db.segment_files(Table::Logs).unwrap().len(), 2);

        // Both rows already answer correctly through the JSON fallback — the pre-existing guarantee.
        let via_json =
            "SELECT count(*) AS c FROM logs WHERE json_get_str(attributes,'env') = 'prod'";
        assert_eq!(count_sql(&db, via_json).await, 2);

        db.compact().await.unwrap();
        assert_eq!(db.segment_files(Table::Logs).unwrap().len(), 1);

        // Reading the BARE column — no CASE, no JSON fallback — is what distinguishes a converged
        // segment from a null-filled one.
        assert_eq!(
            count_sql(&db, "SELECT count(*) AS c FROM logs WHERE \"env\" = 'prod'").await,
            2,
            "the pre-promotion row must have been projected from its retained JSON"
        );
        assert_eq!(
            count_sql(&db, "SELECT count(*) AS c FROM logs WHERE \"env\" IS NULL").await,
            0,
            "no NULL hole survives compaction"
        );
        // And the answer through the normal path is unchanged.
        assert_eq!(count_sql(&db, via_json).await, 2);
        db.close().await.unwrap();
    });
}

/// Two promoted keys projected together must each land in their own column, for the back-filled row
/// as much as the sealed one.
///
/// It also passes a reserved name (`service`) through the promote set, which `promoted_columns`
/// filters out — the projection must stay in step with that filtering rather than assuming the built
/// column vector lines up with the keys it was asked for.
///
/// Note what this does **not** prove. The zip in `backfill_promoted` goes through
/// `promoted_columns(missing)` rather than `missing` defensively, but the misalignment it guards
/// against appears to be unreachable today: `missing` holds only keys *absent* from the source
/// schema, and `promoted_columns` drops only *reserved* names, which are the built-in columns and are
/// therefore present in every segment of the table. A key cannot be in both sets at once. That
/// changes if the built-in column set ever does, which is why the guard stays — but this test
/// exercises the ordinary two-key case, not that one.
#[test]
fn compaction_projects_each_key_into_its_own_column() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let rt = ct_rt();
    let db: Arc<Db> = Db::builder(dir).open().unwrap();
    rt.block_on(async {
        db.ingest_otlp_rich_ok("cart", "one", 1, &[("az", "az-a"), ("tier", "gold")])
            .await;
        db.flush().await.unwrap();
        db.set_promote(Promote::new(["az", "service", "tier"]))
            .await
            .unwrap();
        db.ingest_otlp_rich_ok("cart", "two", 2, &[("az", "az-b"), ("tier", "silver")])
            .await;
        db.flush().await.unwrap();
        db.compact().await.unwrap();

        // Each value must land in its own column, for the back-filled row as much as the sealed one.
        for (col, val) in [
            ("az", "az-a"),
            ("tier", "gold"),
            ("az", "az-b"),
            ("tier", "silver"),
        ] {
            assert_eq!(
                count_sql(
                    &db,
                    &format!("SELECT count(*) AS c FROM logs WHERE \"{col}\" = '{val}'")
                )
                .await,
                1,
                "{col} must carry its own value, not a neighbour's"
            );
        }
        db.close().await.unwrap();
    });
}

/// **A single-segment partition converges too.** Merging and converging are the same pass.
///
/// `compact_partition` used to skip any day partition holding one segment — nothing to merge. But a
/// partition that will never gain a second segment (an old day, a low-volume signal, a database that
/// seals rarely) would then never converge after a `set_promote`, and would keep paying the JSON
/// fallback for the life of the data no matter how often compaction ran.
///
/// Doing it in the same pass rather than as a separate "backfill" job is deliberate: compaction
/// already projects promoted columns while it rewrites, so a separate job would rewrite the same
/// bytes twice — and under the out-of-process design would produce two pending records claiming the
/// same input segment.
#[test]
fn compaction_converges_a_lone_segment_whose_schema_lags() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let rt = ct_rt();
    let db: Arc<Db> = Db::builder(dir).open().unwrap();
    rt.block_on(async {
        // Exactly ONE segment, sealed before `env` is promoted. Nothing to merge, ever.
        db.ingest_otlp_rich_ok("cart", "lonely", 1, &[("env", "prod")])
            .await;
        db.flush().await.unwrap();
        db.set_promote(Promote::new(["env"])).await.unwrap();
        assert_eq!(db.segment_files(Table::Logs).unwrap().len(), 1);

        // Correct but unconverged: the column is null-filled, the JSON arm answers.
        assert_eq!(
            count_sql(&db, "SELECT count(*) AS c FROM logs WHERE \"env\" IS NULL").await,
            1
        );

        let report = db.compact().await.unwrap();
        assert_eq!(
            report.segments_converged, 1,
            "the lone segment was rewritten"
        );
        assert_eq!(report.segments_merged, 0, "and nothing was merged");
        assert_eq!(report.segments_created, 1);
        assert_eq!(db.segment_files(Table::Logs).unwrap().len(), 1);

        assert_eq!(
            count_sql(&db, "SELECT count(*) AS c FROM logs WHERE \"env\" = 'prod'").await,
            1,
            "the column is now populated from the retained JSON"
        );

        // Idempotent: a second pass has nothing left to converge and must not rewrite again.
        let again = db.compact().await.unwrap();
        assert_eq!(again.segments_converged, 0);
        assert_eq!(again.segments_created, 0);
        db.close().await.unwrap();
    });
}

/// **Out-of-process housekeeping, end to end, against a live writer.**
///
/// The design in ARCHITECTURE.md §7.2: a separate process cannot take `writer.lock`,
/// so it does the expensive rewrite from a *read-only* view and leaves a record; the writer performs
/// the swap. This drives both halves in one test — `prepare_pending` is exactly what the
/// `imbh-housekeeper` binary calls, and it runs here while the writer handle is open and ingesting.
#[test]
fn a_separate_preparer_rewrites_and_the_writer_commits() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let rt = ct_rt();
    let db: Arc<Db> = Db::builder(dir).wal(WalMode::Always).open().unwrap();
    rt.block_on(async {
        // Two same-day segments to merge, plus rows still in the buffer.
        db.ingest_otlp_rich_ok("cart", "alpha", 1, &[("env", "prod")])
            .await;
        db.flush().await.unwrap();
        db.ingest_otlp_rich_ok("cart", "beta", 2, &[("env", "prod")])
            .await;
        db.flush().await.unwrap();
        db.ingest_otlp_rich_ok("cart", "unsealed", 3, &[("env", "prod")])
            .await;
        assert_eq!(db.segment_files(Table::Logs).unwrap().len(), 2);
        assert_eq!(count_sql(&db, "SELECT count(*) AS c FROM logs").await, 3);

        // PREPARE — the housekeeper's half. The writer is open and holds `writer.lock`; this takes
        // no lock, touches no manifest, and deletes nothing.
        let prepared = prepare_pending(dir, Compression::default(), 4).unwrap();
        let logs_job = prepared
            .iter()
            .find(|r| r.table == Table::Logs)
            .expect("a logs rewrite was prepared");
        assert_eq!(logs_job.inputs.len(), 2, "the two same-day segments");
        assert_eq!(logs_job.output.rows, 2);

        // Nothing has changed yet: the manifest still points at the inputs, answers are unaffected.
        assert_eq!(db.segment_files(Table::Logs).unwrap().len(), 2);
        assert_eq!(count_sql(&db, "SELECT count(*) AS c FROM logs").await, 3);

        // COMMIT — the writer's half.
        let report = db.commit_pending().await.unwrap();
        assert!(report.applied >= 1, "the logs rewrite was applied");
        assert_eq!(report.discarded, 0);
        assert_eq!(
            db.segment_files(Table::Logs).unwrap().len(),
            1,
            "two input segments replaced by one"
        );
        // Every row survives, including the one that was still in the buffer during the rewrite.
        assert_eq!(count_sql(&db, "SELECT count(*) AS c FROM logs").await, 3);

        // A second commit has nothing to do — the record was consumed, not left to reapply.
        let again = db.commit_pending().await.unwrap();
        assert_eq!(again.applied, 0);
        assert_eq!(again.discarded, 0);
        db.close().await.unwrap();
    });

    // Drop the handle to release `writer.lock` before reopening read-write (the advisory lock is
    // held until the last `Arc<Db>` is dropped, not merely until `close()`).
    drop(db);

    // The merged data survives a reopen, so the swap really was durable in the manifest.
    let db: Arc<Db> = Db::builder(dir).open().unwrap();
    rt.block_on(async {
        assert_eq!(count_sql(&db, "SELECT count(*) AS c FROM logs").await, 3);
        db.close().await.unwrap();
    });
}

/// **A record built against a different promoted set is discarded, not applied.**
///
/// The output's column layout was decided when it was written. If the writer has since run
/// `set_promote`, committing it would put a segment into the manifest whose schema disagrees with
/// what the manifest implies. Discarding is safe — the inputs were never touched — and costs only
/// the preparer's work.
#[test]
fn a_stale_pending_record_is_discarded_rather_than_committed() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let rt = ct_rt();
    let db: Arc<Db> = Db::builder(dir).open().unwrap();
    rt.block_on(async {
        db.ingest_otlp_rich_ok("cart", "alpha", 1, &[("env", "prod")])
            .await;
        db.flush().await.unwrap();
        db.ingest_otlp_rich_ok("cart", "beta", 2, &[("env", "prod")])
            .await;
        db.flush().await.unwrap();

        let prepared = prepare_pending(dir, Compression::default(), 4).unwrap();
        assert!(!prepared.is_empty());

        // The promoted set moves under the prepared record.
        db.set_promote(Promote::new(["env"])).await.unwrap();

        let report = db.commit_pending().await.unwrap();
        assert_eq!(report.applied, 0, "nothing was committed");
        assert!(report.discarded >= 1, "the stale record was rejected");
        assert_eq!(
            db.segment_files(Table::Logs).unwrap().len(),
            2,
            "the inputs are untouched"
        );
        assert_eq!(count_sql(&db, "SELECT count(*) AS c FROM logs").await, 2);

        // And redoing the work under the new set commits cleanly.
        prepare_pending(dir, Compression::default(), 4).unwrap();
        let report = db.commit_pending().await.unwrap();
        assert_eq!(report.applied, 1);
        assert_eq!(db.segment_files(Table::Logs).unwrap().len(), 1);
        assert_eq!(count_sql(&db, "SELECT count(*) AS c FROM logs").await, 2);
        db.close().await.unwrap();
    });
}

/// **A writer restart between prepare and commit must not destroy the prepared work.**
///
/// A prepared output is a segment no manifest points at yet — exactly the shape `cleanup_orphans`
/// reaps at open. Left alone, an out-of-process preparer would lose its rewrite to every host
/// restart, and the offline `--commit` mode (which opens the writer *after* preparing) would sweep
/// away its own output before committing it. Reaping is *safe* — the digest check would reject the
/// record — but it would make the handoff useless in practice, which is a different kind of broken.
#[test]
fn a_prepared_rewrite_survives_a_writer_restart() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let rt = ct_rt();
    let db: Arc<Db> = Db::builder(dir).open().unwrap();
    rt.block_on(async {
        db.ingest_otlp_rich_ok("cart", "alpha", 1, &[]).await;
        db.flush().await.unwrap();
        db.ingest_otlp_rich_ok("cart", "beta", 2, &[]).await;
        db.flush().await.unwrap();
        db.close().await.unwrap();
    });
    drop(db);

    // Prepare with no writer running at all — the offline shape.
    let prepared = prepare_pending(dir, Compression::default(), 4).unwrap();
    assert_eq!(prepared.len(), 1);
    let output = dir.join(&prepared[0].output.relative_path);
    assert!(output.is_file());

    // Reopening runs orphan cleanup — where the prepared output used to be deleted — and *then*
    // commits pending records, both inside `open()` and in that order. So a merged result is itself
    // proof that cleanup left the file alone: had it been swept, the commit would have failed its
    // digest check and discarded the record, leaving two segments.
    let db: Arc<Db> = Db::builder(dir).open().unwrap();
    assert!(
        output.is_file(),
        "orphan cleanup must respect a file named by a valid pending record"
    );
    rt.block_on(async {
        assert_eq!(
            db.segment_files(Table::Logs).unwrap().len(),
            1,
            "the record survived the restart and was applied at open"
        );
        assert_eq!(count_sql(&db, "SELECT count(*) AS c FROM logs").await, 2);
        // The explicit call is now a no-op: `open()` already consumed the record.
        let report = db.commit_pending().await.unwrap();
        assert_eq!(report.applied, 0);
        assert_eq!(report.discarded, 0);
        db.close().await.unwrap();
    });
    drop(db);

    // With the record consumed, the file is a normal committed segment and the next open leaves it
    // alone; nothing else lingers under pending/.
    assert_eq!(std::fs::read_dir(dir.join("pending")).unwrap().count(), 0);
    let db: Arc<Db> = Db::builder(dir).open().unwrap();
    rt.block_on(async {
        assert_eq!(count_sql(&db, "SELECT count(*) AS c FROM logs").await, 2);
        db.close().await.unwrap();
    });
}

/// **A host that never calls `maintain()` still picks up prepared work.**
///
/// The housekeeper exists for embedded hosts, and the default `Maintenance::Manual` means such a host
/// may never call `maintain()` at all. Without a pickup on `open()`/`close()` its prepared rewrites
/// would sit on disk forever — and the preparer, seeing nothing land, would re-prepare the same
/// partitions on every pass, burning IO indefinitely.
#[test]
fn prepared_work_lands_without_the_host_ever_calling_maintain() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let rt = ct_rt();

    let db: Arc<Db> = Db::builder(dir).open().unwrap();
    rt.block_on(async {
        db.ingest_otlp_rich_ok("cart", "alpha", 1, &[]).await;
        db.flush().await.unwrap();
        db.ingest_otlp_rich_ok("cart", "beta", 2, &[]).await;
        db.flush().await.unwrap();
        db.close().await.unwrap();
    });
    drop(db);

    // A housekeeper runs while the host is down. Nothing calls maintain() anywhere in this test.
    let prepared = prepare_pending(dir, Compression::default(), 4).unwrap();
    assert_eq!(prepared.len(), 1);
    assert_eq!(std::fs::read_dir(dir.join("pending")).unwrap().count(), 1);

    // Merely opening the database lands it.
    let db: Arc<Db> = Db::builder(dir).open().unwrap();
    rt.block_on(async {
        assert_eq!(
            db.segment_files(Table::Logs).unwrap().len(),
            1,
            "the prepared merge was committed at open"
        );
        assert_eq!(count_sql(&db, "SELECT count(*) AS c FROM logs").await, 2);
        db.close().await.unwrap();
    });
    drop(db);
    assert_eq!(
        std::fs::read_dir(dir.join("pending")).unwrap().count(),
        0,
        "the record was consumed, so a preparer will not redo the work"
    );

    // And a record prepared *during* a handle's lifetime lands at close, not only at the next open.
    let db: Arc<Db> = Db::builder(dir).open().unwrap();
    rt.block_on(async {
        db.ingest_otlp_rich_ok("cart", "gamma", 3, &[]).await;
        db.flush().await.unwrap();
        // Two segments again; a housekeeper prepares while this handle is open and ingesting.
        assert_eq!(db.segment_files(Table::Logs).unwrap().len(), 2);
        let prepared = prepare_pending(dir, Compression::default(), 4).unwrap();
        assert_eq!(prepared.len(), 1);
        db.close().await.unwrap();
    });
    drop(db);
    assert_eq!(
        std::fs::read_dir(dir.join("pending")).unwrap().count(),
        0,
        "close() committed it"
    );
    let db: Arc<Db> = Db::builder(dir).open().unwrap();
    rt.block_on(async {
        assert_eq!(db.segment_files(Table::Logs).unwrap().len(), 1);
        assert_eq!(count_sql(&db, "SELECT count(*) AS c FROM logs").await, 3);
        db.close().await.unwrap();
    });
}

/// **A bounded compaction pass is a slice, not a partial one.**
///
/// `compact_bounded(n)` rewrites at most `n` partitions and leaves the rest exactly as they were —
/// untouched, still listed, still queryable — so an operator can make incremental progress on a
/// corpus too large to compact in one call. Draining is repeated calls; the pass that rewrites
/// nothing is how a caller learns it is done.
///
/// Three day-partitions, two segments each, built by sealing twice per day. A bound of one therefore
/// has strictly more work available than it may do, which is the only way to tell a bound that works
/// from a bound that happens to be larger than the corpus.
#[test]
fn a_bounded_compaction_does_a_slice_and_leaves_the_rest_intact() {
    const DAY: u64 = 24 * 3_600 * 1_000_000_000;
    let tmp = tempfile::tempdir().unwrap();
    let rt = ct_rt();
    let db: Arc<Db> = Db::builder(tmp.path()).open().unwrap();

    rt.block_on(async {
        // Two segments in each of three day-partitions: six segments, three merges available.
        for day in 1..=3u64 {
            for n in 0..2u64 {
                db.ingest_otlp_rich_ok("cart", "hello", day * DAY + n, &[])
                    .await;
                db.flush().await.unwrap();
            }
        }
        let before = db.segments().len();
        assert_eq!(before, 6, "two segments per day-partition");

        // One partition's worth of work, and no more.
        let first = db.compact_bounded(1).await.unwrap();
        assert_eq!(first.segments_created, 1, "exactly one partition rewritten");
        assert_eq!(first.segments_merged, 2, "the two segments of that day");
        assert_eq!(
            db.segments().len(),
            before - 1,
            "one partition collapsed 2 -> 1; the other two are untouched"
        );
        assert_eq!(
            count_sql(&db, "SELECT count(*) AS c FROM logs").await,
            6,
            "a bounded pass loses no rows"
        );

        // Draining: each call takes another slice, and the one that finds nothing says so.
        let second = db.compact_bounded(1).await.unwrap();
        assert_eq!(second.segments_created, 1);
        let third = db.compact_bounded(8).await.unwrap();
        assert_eq!(
            third.segments_created, 1,
            "a bound larger than the work left does the work left"
        );
        let drained = db.compact_bounded(8).await.unwrap();
        assert_eq!(
            drained.segments_created, 0,
            "nothing left to rewrite — what a drain loop stops on"
        );
        assert_eq!(db.segments().len(), 3, "one segment per day-partition");
        assert_eq!(count_sql(&db, "SELECT count(*) AS c FROM logs").await, 6);

        // And the unbounded call is the same thing with no ceiling.
        assert_eq!(db.compact().await.unwrap().segments_created, 0);
        db.close().await.unwrap();
    });
}

/// **A running writer picks up a preparer's rewrites on its own**, without anyone calling
/// `maintain()`.
///
/// The prepare/commit handoff (ARCHITECTURE.md §7.2) is only useful on a daemon that stays up, and
/// until the background loop committed, the in-process triggers were `open()` and `close()` — so a
/// long-running writer applied an external housekeeper's work *only at restart*. This drives the
/// gap: prepare against a live writer, touch nothing else, and wait for the loop.
///
/// `Maintenance::Background` with a short interval is what `imbhd` runs, at a cadence a test can
/// wait out.
#[test]
fn the_background_loop_commits_a_preparers_rewrite_without_maintain() {
    use std::time::{Duration, Instant};

    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let rt = ct_rt();
    let db: Arc<Db> = Db::builder(dir)
        .wal(WalMode::Always)
        // Short enough to wait out; `Manual` flush so the loop's *seal* never fires and the only
        // thing under test is the commit step.
        .maintenance(imbh::Maintenance::Background(Duration::from_millis(50)))
        .flush(imbh::FlushPolicy::manual())
        .open()
        .unwrap();

    rt.block_on(async {
        // Two same-day segments for the preparer to merge.
        db.ingest_otlp_rich_ok("cart", "alpha", 1, &[]).await;
        db.flush().await.unwrap();
        db.ingest_otlp_rich_ok("cart", "beta", 2, &[]).await;
        db.flush().await.unwrap();
        assert_eq!(db.segment_files(Table::Logs).unwrap().len(), 2);
    });

    // The housekeeper's half, from outside: no lock, no manifest edit, no deletion.
    let prepared = prepare_pending(dir, Compression::default(), 4).unwrap();
    assert!(
        prepared.iter().any(|r| r.table == Table::Logs),
        "a logs rewrite was prepared"
    );

    // Nobody calls `maintain()` or `commit_pending()` from here on — the loop is the only actor.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if db.segment_files(Table::Logs).unwrap().len() == 1 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the background loop never committed the prepared rewrite"
        );
        std::thread::sleep(Duration::from_millis(20));
    }

    rt.block_on(async {
        assert_eq!(
            count_sql(&db, "SELECT count(*) AS c FROM logs").await,
            2,
            "the swap preserved every row"
        );
        db.close().await.unwrap();
    });
}

/// **Promotion takes effect at the next seal.**
///
/// `set_promote` seals as a barrier, so no buffered batch straddles the change; every row ingested
/// afterwards is encoded against the new set and reaches disk in the *very next* segment — not on a
/// maintenance tick, and not only after a compaction pass. Segments sealed before the change keep
/// their old schema, which reads correctly through the JSON fallback until something rewrites them.
#[test]
fn promotion_reaches_the_next_seal_not_a_later_pass() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let rt = ct_rt();
    let db: Arc<Db> = Db::builder(dir).open().unwrap();

    let promoted_columns = |db: &Arc<Db>| -> Vec<Vec<String>> {
        db.segment_files(Table::Logs)
            .unwrap()
            .iter()
            .map(|path| {
                let file = std::fs::File::open(path).expect("segment");
                let builder =
                    imbh::parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder::try_new(
                        file,
                    )
                    .expect("parquet");
                builder
                    .schema()
                    .fields()
                    .iter()
                    .map(|f| f.name().clone())
                    .collect()
            })
            .collect()
    };

    rt.block_on(async {
        // A segment sealed *before* promotion: no column, and none is expected.
        db.ingest_otlp_rich_ok("cart", "before", 1, &[("env", "prod")])
            .await;
        db.flush().await.unwrap();
        assert!(
            !promoted_columns(&db)[0].iter().any(|c| c == "env"),
            "nothing is promoted yet"
        );

        // Promote. The barrier seals whatever is buffered under the old schema.
        db.set_promote(Promote::new(["env"])).await.unwrap();
        assert_eq!(db.promote().keys(), ["env"]);

        // The very next seal carries the column — no tick, no compaction, no housekeeper.
        db.ingest_otlp_rich_ok("cart", "after", 2, &[("env", "prod")])
            .await;
        db.flush().await.unwrap();
        let schemas = promoted_columns(&db);
        assert!(
            schemas.iter().any(|cols| cols.iter().any(|c| c == "env")),
            "the segment sealed after promotion has the column: {schemas:?}"
        );
        assert!(
            schemas.iter().any(|cols| !cols.iter().any(|c| c == "env")),
            "and the one sealed before it still does not: {schemas:?}"
        );

        // Both answer the same question, because the SQL builder falls back to the JSON blob
        // wherever the column is absent.
        assert_eq!(
            count_sql(
                &db,
                "SELECT count(*) AS c FROM logs WHERE json_get_str(attributes, 'env') = 'prod'"
            )
            .await,
            2,
            "the promoted and unpromoted segments both answer"
        );
        db.close().await.unwrap();
    });
}

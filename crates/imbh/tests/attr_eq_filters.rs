//! Equality filters on a **promoted** attribute key compile to the cast-free predicate
//! `SqlParams::attr_eq_sql` (`"k" = $v`, with a guarded `json_get_str` fallback) instead of
//! `CASE … CAST("k" AS VARCHAR) … END = $v`. The rewrite is a pure performance change, so these tests
//! pin that every typed equality site still returns exactly what the old `CASE` form returns across
//! all three places a row can live:
//!
//! - (a) a segment sealed **after** promotion, which carries the `env` dictionary column;
//! - (b) a segment sealed **before** promotion, which has no such column (null-filled at query time)
//!   and must fall back to the JSON blob;
//! - (c) rows still in the in-memory buffer.
//!
//! Each phase also writes a non-matching value and a row lacking the key, so a predicate that
//! over-matches (e.g. a NULL column treated as a wildcard) is caught as surely as one that loses
//! history.
#![cfg(all(feature = "ingest", feature = "query"))]

use std::sync::Arc;
use std::time::Duration;

use imbh::{
    Aggregation, Db, LogQuery, LogStringField, MetricQuery, Promote, SpanMetricsQuery,
    StringPredicate, TraceQuery,
};
use imbh_test_support::otlp::{otlp_gauge_attrs, otlp_rich, otlp_trace_wide};

/// The old (pre-`attr_eq_sql`) compiled form of `env = 'prod'` for a promoted key, verbatim from
/// `attr_field` — the "before" every typed answer is checked against.
const OLD_ENV_PROD: &str = "CASE WHEN \"env\" IS NOT NULL THEN CAST(\"env\" AS VARCHAR) \
                            ELSE json_get_str(attributes, 'env') END = 'prod'";

/// Ingest one phase's worth of rows: per signal, one `env=prod`, one `env=dev`, one with no `env`.
/// `phase` (0/1/2) keeps bodies, trace ids and timestamps distinct across phases.
async fn ingest_phase(db: &Arc<Db>, phase: u8) {
    let tag = ["a", "b", "c"][phase as usize];
    let t = 1_000 * (u64::from(phase) + 1);
    for (i, attrs) in [&[("env", "prod")][..], &[("env", "dev")][..], &[][..]]
        .into_iter()
        .enumerate()
    {
        let body = format!("{tag}-{}", ["prod", "dev", "none"][i]);
        db.ingest_otlp_logs(&otlp_rich("cart", &body, t + i as u64, 9, attrs))
            .await
            .unwrap();
        // Two spans per trace, so a span-level count distinguishes "matched the trace" from
        // "matched every span".
        let trace_id = [phase * 3 + i as u8 + 1; 16];
        db.ingest_otlp_traces(&otlp_trace_wide("cart", trace_id, 2, attrs))
            .await
            .unwrap();
        db.ingest_otlp_metrics(&otlp_gauge_attrs("cart", "cpu", t + i as u64, attrs))
            .await
            .unwrap();
    }
}

/// A DB holding phases (b) pre-promotion sealed, (a) post-promotion sealed, (c) buffered.
async fn three_phase_db(dir: &std::path::Path) -> Arc<Db> {
    let db = Db::builder(dir).open().unwrap();
    ingest_phase(&db, 0).await; // (b): sealed before `env` is promoted → no `env` column
    db.flush().await.unwrap();
    db.set_promote(Promote::new(["env"])).await.unwrap();
    ingest_phase(&db, 1).await; // (a): sealed with the column
    db.flush().await.unwrap();
    ingest_phase(&db, 2).await; // (c): left in the buffer
    db
}

async fn count(db: &Arc<Db>, sql: &str) -> i64 {
    let batches = db.sql(sql).collect().await.unwrap();
    let col = batches[0].column(0);
    col.as_any()
        .downcast_ref::<imbh::arrow::array::Int64Array>()
        .expect("count(*) is Int64")
        .value(0)
}

async fn log_bodies(db: &Arc<Db>, q: LogQuery) -> Vec<String> {
    let page = db.logs().query(q.limit(100)).await.unwrap();
    let mut bodies: Vec<String> = page.entries.into_iter().map(|e| e.body).collect();
    bodies.sort_unstable();
    bodies
}

#[tokio::test(flavor = "current_thread")]
async fn promoted_equality_filters_agree_with_the_case_form_in_every_segment_kind() {
    let dir = tempfile::tempdir().unwrap();
    let db = three_phase_db(dir.path()).await;
    let prod = vec!["a-prod", "b-prod", "c-prod"];

    // The old CASE form, run as raw SQL on the same DB, is the "before".
    assert_eq!(
        count(
            &db,
            &format!("SELECT count(*) FROM logs WHERE {OLD_ENV_PROD}")
        )
        .await,
        3
    );
    assert_eq!(
        count(
            &db,
            &format!("SELECT count(*) FROM spans WHERE {OLD_ENV_PROD}")
        )
        .await,
        6
    );
    assert_eq!(
        count(
            &db,
            &format!("SELECT count(*) FROM metrics_gauge WHERE {OLD_ENV_PROD}")
        )
        .await,
        3
    );

    // logs: `attr_eq` and the LogQL-style `string_predicate(=)`.
    assert_eq!(
        log_bodies(&db, LogQuery::new().attr_eq("env", "prod")).await,
        prod
    );
    assert_eq!(
        log_bodies(
            &db,
            LogQuery::new().string_predicate(
                LogStringField::Attribute("env".into()),
                StringPredicate::Eq,
                "prod"
            )
        )
        .await,
        prod
    );
    // `= ""` keeps its "absent label" meaning (it is not routed through the equality predicate).
    assert_eq!(
        log_bodies(
            &db,
            LogQuery::new().string_predicate(
                LogStringField::Attribute("env".into()),
                StringPredicate::Eq,
                ""
            )
        )
        .await,
        vec!["a-none", "b-none", "c-none"]
    );
    // The built-in `service` column, by both spellings and through the `Service` field.
    assert_eq!(
        log_bodies(&db, LogQuery::new().attr_eq("service.name", "cart"))
            .await
            .len(),
        9
    );
    assert_eq!(
        log_bodies(
            &db,
            LogQuery::new().string_predicate(LogStringField::Service, StringPredicate::Eq, "cart")
        )
        .await
        .len(),
        9
    );
    assert!(
        log_bodies(&db, LogQuery::new().attr_eq("service", "nope"))
            .await
            .is_empty()
    );
    assert_eq!(
        db.logs()
            .count(LogQuery::new().attr_eq("env", "prod"))
            .await
            .unwrap(),
        3
    );

    // traces: search (one hit per trace) and span metrics (one call per span).
    let mut ids: Vec<u8> = db
        .traces()
        .search(TraceQuery::new().attr_eq("env", "prod"))
        .await
        .unwrap()
        .into_iter()
        .map(|s| s.trace_id.0[0])
        .collect();
    ids.sort_unstable();
    assert_eq!(ids, vec![1, 4, 7]);
    let red = db
        .traces()
        .span_metrics(
            SpanMetricsQuery::new()
                .attr_eq("env", "prod")
                .step(Duration::from_secs(3600)),
        )
        .await
        .unwrap();
    let calls: u64 = red
        .0
        .iter()
        .flat_map(|s| s.points.iter())
        .map(|p| p.calls)
        .sum();
    assert_eq!(calls, 6);

    // metrics: a PromQL-style `=` label filter, and `=""` matching the label-less points.
    let gauge_count = |value: &'static str| {
        MetricQuery::gauge("cpu")
            .filter("env", value)
            .aggregation(Aggregation::Count)
            .step(Duration::from_secs(3600))
    };
    let total = |m: imbh::Matrix| -> f64 {
        m.0.iter()
            .flat_map(|s| s.samples.iter())
            .map(|s| s.value)
            .sum()
    };
    assert_eq!(
        total(db.metrics().range(gauge_count("prod")).await.unwrap()),
        3.0
    );
    assert_eq!(
        total(db.metrics().range(gauge_count("")).await.unwrap()),
        3.0
    );
    assert_eq!(
        total(db.metrics().range(gauge_count("nope")).await.unwrap()),
        0.0
    );
}

/// The same filters, re-checked after the buffered phase is sealed too — so the (c) rows are now read
/// from a post-promotion segment instead of the buffer, and nothing may move.
#[tokio::test(flavor = "current_thread")]
async fn promoted_equality_filters_are_unchanged_by_sealing_the_buffer() {
    let dir = tempfile::tempdir().unwrap();
    let db = three_phase_db(dir.path()).await;
    let before = log_bodies(&db, LogQuery::new().attr_eq("env", "prod")).await;
    db.flush().await.unwrap();
    let after = log_bodies(&db, LogQuery::new().attr_eq("env", "prod")).await;
    assert_eq!(before, after);
    assert_eq!(after, vec!["a-prod", "b-prod", "c-prod"]);
    assert_eq!(
        count(
            &db,
            &format!("SELECT count(*) FROM logs WHERE {OLD_ENV_PROD}")
        )
        .await,
        3
    );
}

//! `imbh`'s typed metrics path and PromQL resolve duplicated timestamps the same way.
//!
//! `MetricsApi::range` collapses points that share a series and an instant in SQL (ARCHITECTURE.md
//! §10.5.1, issue #27); PromQL evaluation collapses them with [`collapse_duplicate_samples`]. The two
//! must agree on *which* sample survives, or a dashboard and a PromQL query over the same data show
//! different values. This test lives here, not in `imbh`, because only here are both sides in scope
//! (`imbh-lgtm` depends on `imbh`, so `imbh` importing this crate would be a dependency cycle) — and it
//! calls the real function, so a change to the PromQL rule that the SQL path does not follow fails it.

use std::cmp::Ordering;
use std::time::Duration;

use imbh::{Aggregation, Db, Duplicates, MetricQuery};
use imbh_lgtm::{FloatSample, collapse_duplicate_samples};
use imbh_test_support::otlp::otlp_gauge_points;

#[tokio::test(flavor = "current_thread")]
async fn range_dedup_agrees_with_the_promql_collapse() {
    // One series (one resource, one service, no datapoint attributes) with duplicated instants
    // covering every branch of the PromQL value order: plain ties, a strict ordering, NaN vs a real
    // number, NaN vs +INFINITY, and negative values.
    let raw: &[(u64, f64)] = &[
        (10, 7.0),
        (10, 7.0),
        (20, 2.0),
        (20, 9.0),
        (20, -1.0),
        (30, f64::NAN),
        (30, 4.0),
        (40, f64::NAN),
        (40, f64::INFINITY),
        (50, f64::NAN),
        (50, f64::NAN),
        (60, -5.0),
        (60, -2.0),
        (70, 1.0),
    ];

    let db = Db::in_memory()
        .duplicates(Duplicates::LastWins)
        .open()
        .unwrap();
    db.ingest_otlp_metrics(&otlp_gauge_points("svc", "cpu", raw))
        .await
        .unwrap();

    // One bucket per nanosecond → the SQL bucket key is the raw timestamp, so each bucket's value is
    // exactly "the surviving sample at that instant".
    let m = db
        .metrics()
        .range(
            MetricQuery::gauge("cpu")
                .aggregation(Aggregation::Sum)
                .step(Duration::from_nanos(1)),
        )
        .await
        .unwrap();
    assert_eq!(m.0.len(), 1);
    let actual: Vec<(i64, f64)> = m.0[0].samples.iter().map(|s| (s.time.0, s.value)).collect();

    let mut expected: Vec<FloatSample> = raw
        .iter()
        .map(|(t, v)| FloatSample {
            timestamp_ns: *t as i64,
            value: *v,
        })
        .collect();
    let dropped = collapse_duplicate_samples(&mut expected);
    assert_eq!(dropped, raw.len() - expected.len());

    assert_eq!(actual.len(), expected.len(), "{actual:?} vs {expected:?}");
    for (a, e) in actual.iter().zip(expected.iter()) {
        assert_eq!(
            a.0, e.timestamp_ns,
            "timestamps differ: {actual:?} vs {expected:?}"
        );
        assert!(
            a.1.total_cmp(&e.value) == Ordering::Equal,
            "value at {}: SQL {} vs PromQL {}",
            a.0,
            a.1,
            e.value
        );
    }
}

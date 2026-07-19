//! Regression: a span predicate combined with `trace_start_range` must not corrupt the trace-start
//! computation. `search()` selects candidate traces via `GROUP BY trace_id HAVING min(start_time)…`;
//! if the span predicate lands in the `WHERE`, `min(start_time)` is taken over only the *matching*
//! spans, so a trace whose true start (root span) is in range but whose matching span is later gets
//! silently dropped. The trace-start range must be evaluated over *all* the trace's spans.

use imbh::{Db, Timestamp, TraceQuery};
use imbh_test_support::otlp::otlp_trace_tree;

#[tokio::test(flavor = "current_thread")]
async fn trace_start_range_uses_true_start_not_matching_span_start() {
    let db = Db::in_memory().open().unwrap();
    // Fixture tree: root "GET /cart" starts at 1000ns; child "db query" starts at 1100ns.
    db.ingest_otlp_traces(&otlp_trace_tree("cart", [7u8; 16]))
        .await
        .unwrap();

    // Predicate matches the child (starts at 1100), but the trace-start window [900,1050] covers only
    // the trace's TRUE start (root at 1000). The trace started in range and contains a matching span,
    // so it must be returned.
    let hits = db
        .traces()
        .search(
            TraceQuery::new()
                .name("db query")
                .trace_start_range_inclusive(Timestamp(900), Timestamp(1050)),
        )
        .await
        .unwrap();

    assert_eq!(
        hits.len(),
        1,
        "trace whose root is in range must be found even when the matching span starts later",
    );
}

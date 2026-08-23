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

/// The mirror image, and the one the prunable candidate scan could have broken.
///
/// `search` narrows phase 1 with a `WHERE` on individual span start times so segments outside the
/// window can be skipped from the manifest. That predicate keeps any span in the window — including
/// spans of a trace that *started earlier and was still running*. For such a trace the filtered
/// `min(start_time)` lands inside the window even though its true start does not, so it sails
/// through the `HAVING` as a false positive. Only the post-assembly check on the true start removes
/// it; without that check this test returns 1 instead of 0.
#[tokio::test(flavor = "current_thread")]
async fn a_trace_that_merely_overlaps_the_window_is_not_a_match() {
    let db = Db::in_memory().open().unwrap();
    // Root starts at 1000ns, child at 1100ns.
    db.ingest_otlp_traces(&otlp_trace_tree("cart", [9u8; 16]))
        .await
        .unwrap();

    // The window opens *after* the root but before the child, so the trace overlaps it without
    // having started in it. Its true start (1000) is outside [1050, 2000]; the child (1100) is not.
    let hits = db
        .traces()
        .search(TraceQuery::new().trace_start_range_inclusive(Timestamp(1050), Timestamp(2000)))
        .await
        .unwrap();

    assert!(
        hits.is_empty(),
        "a trace that started before the window must not match on the strength of a later span; \
         got {:?}",
        hits.iter().map(|h| h.start_time.0).collect::<Vec<_>>()
    );

    // And the exact boundary still matches: a window that opens exactly on the true start.
    let hits = db
        .traces()
        .search(TraceQuery::new().trace_start_range_inclusive(Timestamp(1000), Timestamp(2000)))
        .await
        .unwrap();
    assert_eq!(hits.len(), 1, "the trace start itself is inside the range");
}

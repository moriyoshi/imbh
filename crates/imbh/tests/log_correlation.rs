//! Trace→log correlation E2E: `LogQuery::trace_id`/`span_id` filter the `logs` table by the raw
//! binary id columns, the drill-down the companion TUI drives (span waterfall → correlated logs).

use std::sync::Arc;

use imbh::{Db, LogQuery, SpanId, TraceId};
use imbh_test_support::otlp::otlp_log_correlated;
use imbh_test_support::rt::ct_rt;

#[test]
fn log_query_filters_by_trace_and_span_id() {
    let tmp = tempfile::tempdir().unwrap();
    let rt = ct_rt();
    let db: Arc<Db> = Db::builder(tmp.path()).open().unwrap();

    let trace_a = [0xab; 16];
    let trace_b = [0xcd; 16];
    let span_1 = [0x01; 8];
    let span_2 = [0x02; 8];

    rt.block_on(async {
        // Two records on trace A (distinct spans) and one on trace B.
        db.ingest_otlp_logs(&otlp_log_correlated("api", "a-span1", 10, trace_a, span_1))
            .await
            .unwrap();
        db.ingest_otlp_logs(&otlp_log_correlated("api", "a-span2", 20, trace_a, span_2))
            .await
            .unwrap();
        db.ingest_otlp_logs(&otlp_log_correlated("api", "b-span1", 30, trace_b, span_1))
            .await
            .unwrap();

        let tid_a = TraceId::from_bytes(&trace_a).unwrap();
        let tid_b = TraceId::from_bytes(&trace_b).unwrap();
        let sid_2 = SpanId::from_bytes(&span_2).unwrap();

        // trace_id alone: both of trace A's records, neither of B's.
        let page = db
            .logs()
            .query(LogQuery::new().trace_id(tid_a))
            .await
            .unwrap();
        assert_eq!(page.entries.len(), 2, "trace A has two records");
        assert!(page.entries.iter().all(|e| e.trace_id == Some(tid_a)));

        // trace_id + span_id: narrows to the single span-2 record of trace A.
        let page = db
            .logs()
            .query(LogQuery::new().trace_id(tid_a).span_id(sid_2))
            .await
            .unwrap();
        assert_eq!(page.entries.len(), 1, "one record on trace A / span 2");
        assert_eq!(page.entries[0].body, "a-span2");

        // A different trace: exactly its one record.
        assert_eq!(
            db.logs()
                .count(LogQuery::new().trace_id(tid_b))
                .await
                .unwrap(),
            1
        );

        // Survives a seal (segment path, raw-binary equality pushed to Parquet the same as the buffer).
        db.flush().await.unwrap();
        assert_eq!(
            db.logs()
                .count(LogQuery::new().trace_id(tid_a))
                .await
                .unwrap(),
            2
        );
    });
}

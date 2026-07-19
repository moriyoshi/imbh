//! Malformed / partial OTLP into the facade ingest paths must fail gracefully — an `Err`, never a
//! panic — and must not wedge the DB: a well-formed request after a rejected one still ingests. This
//! complements the over-the-wire `400` checks in `imbh-server/tests/http_e2e.rs` by exercising the
//! in-process `ingest_otlp_*` and the fail-fast `try_ingest_otlp_*` twins directly.

use std::sync::Arc;

use imbh::Db;
use imbh_test_support::{count_logs, otlp::otlp_log, rt::ct_rt};

/// Wire-invalid protobuf: field 1, varint wire type, with a continuation byte and no continuation —
/// prost rejects it with a decode error (unlike an empty body, which is a valid empty request).
const GARBAGE: &[u8] = &[0x08, 0x80];

#[test]
fn malformed_otlp_is_rejected_without_panicking() {
    let db: Arc<Db> = Db::in_memory().open().unwrap();

    ct_rt().block_on(async {
        // Every async ingest path rejects the garbage body with an error, not a panic.
        assert!(
            db.ingest_otlp_logs(GARBAGE).await.is_err(),
            "logs reject garbage"
        );
        assert!(
            db.ingest_otlp_traces(GARBAGE).await.is_err(),
            "traces reject garbage"
        );
        assert!(
            db.ingest_otlp_metrics(GARBAGE).await.is_err(),
            "metrics reject garbage"
        );

        // The fail-fast (never-fsync) twins reject it too.
        assert!(db.try_ingest_otlp_logs(GARBAGE).is_err());
        assert!(db.try_ingest_otlp_traces(GARBAGE).is_err());
        assert!(db.try_ingest_otlp_metrics(GARBAGE).is_err());

        // An empty body is a well-formed *empty* request: accepted 0, no error.
        let empty = db.ingest_otlp_logs(&[]).await.expect("empty body is valid");
        assert_eq!(empty.accepted, 0);
        assert_eq!(empty.rejected, 0);

        // The DB is not wedged: a good request after the rejected ones ingests normally.
        db.ingest_otlp_logs(&otlp_log("svc", "good", 1))
            .await
            .expect("valid ingest after rejections");
        let (count, _distinct) = count_logs(&db).await;
        assert_eq!(count, 1, "only the one valid row landed");
    });
}

//! Level-2 (raw-Arrow) PromQL metric-label read vs the existing DTO path.
//!
//! Run: `cargo run -p imbh-lgtm --features source --example metric_level2 --release`
//!
//! Path A (existing): `MetricsApi::points` materializes a `MetricPoint` per row (parsing the full
//! *typed* attribute map), then `metric_labels` re-clones the string attributes into the set.
//! Path B (Level 2): `MetricsApi::points_batches` returns raw Arrow, and `metric_labels_from_batch`
//! parses the `attributes` blob once per row, lifting only the string entries.
//!
//! Unlike LogQL, PromQL's label set is *open* (every string attribute is a label), so the blob parse
//! is unavoidable and promoted dictionary columns do not help the source. The win here is skipping the
//! `MetricPoint` DTO materialization, not the parse. Decomposed (fetch vs labels) with a counting
//! allocator so the difference is visible.

use std::alloc::{GlobalAlloc, Layout, System};
use std::hint::black_box;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::time::Instant;

use imbh::{AnyValue, Db, MetricPoint, MetricPointsQuery, Promote, Timestamp};
use imbh_lgtm::{LabelSet, metric_labels_from_batch};
use imbh_test_support::otlp::otlp_gauge_attrs;

static ALLOCS: AtomicU64 = AtomicU64::new(0);

struct Counting;
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Relaxed);
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Relaxed);
        unsafe { System.alloc_zeroed(l) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

const N: usize = 5_000;
const METRIC: &str = "http_requests_total";

fn query() -> MetricPointsQuery {
    MetricPointsQuery::gauge(METRIC)
        .range_inclusive(Timestamp(0), Timestamp(i64::MAX))
        .limit(N)
}

/// The existing-surface label read: off the materialized `MetricPoint` DTO (owned), matching
/// `source::metric_labels`.
fn dto_labels(point: &MetricPoint) -> LabelSet<'static> {
    let mut out: Vec<(String, String)> = point
        .attributes
        .iter()
        .filter_map(|(key, value)| match value {
            AnyValue::Str(value) => Some((key.to_owned(), value.clone())),
            _ => None,
        })
        .collect();
    if let Some(service) = &point.service {
        out.push(("service".to_owned(), service.clone()));
    }
    out.push(("__name__".to_owned(), point.metric.clone()));
    LabelSet::new(out)
}

async fn run(title: &str, promote: &[&str]) {
    let db = Db::in_memory()
        .promote(Promote::new(promote.iter().copied()))
        .open()
        .unwrap();
    let routes = ["/a", "/b", "/c", "/d", "/checkout"];
    let methods = ["GET", "POST"];
    let regions = ["us-east", "eu-west", "ap-south"];
    for i in 0..N {
        let body = otlp_gauge_attrs(
            "checkout",
            METRIC,
            i as u64 + 1,
            &[
                ("http.route", routes[i % routes.len()]),
                ("http.method", methods[i % methods.len()]),
                ("region", regions[i % regions.len()]),
                ("noise.a", "x"),
                ("noise.b", "y"),
            ],
        );
        db.ingest_otlp_metrics(&body).await.unwrap();
    }

    // Warm up.
    let warm = db.metrics().points(query()).await.unwrap();
    for point in &warm {
        black_box(dto_labels(point));
    }
    let warm_b = db.metrics().points_batches(query()).await.unwrap();
    for batch in &warm_b {
        for row in 0..batch.num_rows() {
            black_box(metric_labels_from_batch(batch, row));
        }
    }

    // Fetch, measured separately.
    let m = ALLOCS.load(Relaxed);
    let batches = db.metrics().points_batches(query()).await.unwrap();
    let fetch_b = ALLOCS.load(Relaxed) - m;
    let m = ALLOCS.load(Relaxed);
    let points = db.metrics().points(query()).await.unwrap();
    let fetch_a = ALLOCS.load(Relaxed) - m; // SQL scan + MetricPoint materialization

    // Label extraction only.
    let (m, t) = (ALLOCS.load(Relaxed), Instant::now());
    let dto: Vec<LabelSet> = points.iter().map(dto_labels).collect();
    let (lab_a, tim_a) = (ALLOCS.load(Relaxed) - m, t.elapsed());
    black_box(&dto);

    let (m, t) = (ALLOCS.load(Relaxed), Instant::now());
    let mut l2: Vec<LabelSet> = Vec::with_capacity(N);
    for batch in &batches {
        for row in 0..batch.num_rows() {
            l2.push(metric_labels_from_batch(batch, row));
        }
    }
    let (lab_b, tim_b) = (ALLOCS.load(Relaxed) - m, t.elapsed());
    black_box(&l2);

    assert_eq!(dto.len(), l2.len(), "row count mismatch");
    assert!(
        dto.iter().zip(&l2).all(|(a, b)| a.iter().eq(b.iter())),
        "label content mismatch",
    );

    println!("── {title} ({N} rows) ─────────────────────────────");
    println!(
        "  fetch  : points_batches {fetch_b:>8} allocs   |   points(+materialize) {fetch_a:>8} allocs"
    );
    println!("  labels : DTO           {lab_a:>8} allocs {tim_a:>9.2?}");
    println!("           L2 batch      {lab_b:>8} allocs {tim_b:>9.2?}");
    let a = fetch_a + lab_a;
    let b = fetch_b + lab_b;
    println!(
        "  end-to-end (fetch+labels): DTO {a} vs L2 {b}   →  {:.2}×",
        a as f64 / b.max(1) as f64
    );
    println!();
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    run("not promoted", &[]).await;
    run("fully promoted", &["http.route", "http.method", "region"]).await;
}

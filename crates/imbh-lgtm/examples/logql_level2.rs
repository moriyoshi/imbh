//! Level-2 (raw-Arrow) LogQL stream-label read vs the existing DTO path.
//!
//! Run: `cargo run -p imbh-lgtm --features source --example logql_level2 --release`
//!
//! Path A (existing surface): `LogsApi::query` materializes a `LogEntry` per row (parsing every
//! attribute of every row), then reads the stream labels off the DTO.
//! Path B (Level 2): `LogsApi::query_batches` returns raw Arrow, and `StreamLabelReader` reads the
//! declared labels straight from the buffers — borrowing promoted/service values with no parse.
//!
//! A process-global counting allocator reports allocations + bytes; both paths include the identical
//! SQL scan, so the delta is the materialize + label work. Measured for a fully-promoted schema (where
//! Level 2 pays) and a non-promoted one (where it falls back to the JSON parse, so it should not).

use std::alloc::{GlobalAlloc, Layout, System};
use std::hint::black_box;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::time::Instant;

use imbh::{Db, Direction, LogQuery, Promote, Timestamp};
use imbh_lgtm::{LabelSet, LogLabelSource, LogStreamSchema, StreamLabelReader};
use imbh_test_support::otlp::otlp_rich;

static ALLOCS: AtomicU64 = AtomicU64::new(0);
static BYTES: AtomicU64 = AtomicU64::new(0);

struct Counting;
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Relaxed);
        BYTES.fetch_add(l.size() as u64, Relaxed);
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Relaxed);
        BYTES.fetch_add(l.size() as u64, Relaxed);
        unsafe { System.alloc_zeroed(l) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

const N: usize = 5_000;

fn schema() -> LogStreamSchema {
    LogStreamSchema {
        labels: vec![
            ("service".to_owned(), LogLabelSource::Service),
            (
                "http.route".to_owned(),
                LogLabelSource::Attribute("http.route".to_owned()),
            ),
            (
                "http.method".to_owned(),
                LogLabelSource::Attribute("http.method".to_owned()),
            ),
            (
                "region".to_owned(),
                LogLabelSource::Attribute("region".to_owned()),
            ),
        ],
    }
}

fn query() -> LogQuery {
    LogQuery::new()
        .range_inclusive(Timestamp(0), Timestamp(i64::MAX))
        .direction(Direction::Forward)
        .limit(N)
}

/// The existing-surface label read: off the materialized `LogEntry` DTO (owned).
fn dto_labels(entry: &imbh::LogEntry, schema: &LogStreamSchema) -> LabelSet<'static> {
    LabelSet::new(schema.labels.iter().filter_map(|(name, source)| {
        let value = match source {
            LogLabelSource::Service => entry.service.as_deref(),
            LogLabelSource::Attribute(key) => entry.attributes.get_str(key),
            LogLabelSource::ResourceAttribute(key) => entry.resource.get_str(key),
        };
        value.map(|value| (name.clone(), value.to_owned()))
    }))
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
        let body = otlp_rich(
            "checkout",
            "request served",
            i as u64 + 1,
            9,
            &[
                ("http.route", routes[i % routes.len()]),
                ("http.method", methods[i % methods.len()]),
                ("region", regions[i % regions.len()]),
                ("noise.a", "x"),
                ("noise.b", "y"),
            ],
        );
        db.ingest_otlp_logs(&body).await.unwrap();
    }
    let schema = schema();

    // Warm up both paths (fill DataFusion caches) so the measured runs compare steady state.
    let warm = db.logs().query(query()).await.unwrap();
    for entry in &warm.entries {
        black_box(dto_labels(entry, &schema));
    }
    let warm_b = db.logs().query_batches(query()).await.unwrap();
    for batch in &warm_b {
        let reader = StreamLabelReader::new(batch, &schema);
        for row in 0..batch.num_rows() {
            black_box(reader.labels(row));
        }
    }

    // ── Fetch, measured on its own so it doesn't mask the label-extraction difference ──
    let m = ALLOCS.load(Relaxed);
    let batches = db.logs().query_batches(query()).await.unwrap();
    let fetch_b = ALLOCS.load(Relaxed) - m;
    let m = ALLOCS.load(Relaxed);
    let page = db.logs().query(query()).await.unwrap();
    let fetch_a = ALLOCS.load(Relaxed) - m; // SQL scan + LogEntry materialization

    // ── Label extraction only, given already-fetched data ──
    let (m, t) = (ALLOCS.load(Relaxed), Instant::now());
    let dto: Vec<LabelSet> = page
        .entries
        .iter()
        .map(|e| dto_labels(e, &schema))
        .collect();
    let (lab_dto, tim_dto) = (ALLOCS.load(Relaxed) - m, t.elapsed());
    black_box(&dto);

    let (m, t) = (ALLOCS.load(Relaxed), Instant::now());
    let mut per_key: Vec<LabelSet> = Vec::with_capacity(N);
    for batch in &batches {
        let reader = StreamLabelReader::new(batch, &schema);
        for row in 0..batch.num_rows() {
            per_key.push(reader.labels_per_key(row));
        }
    }
    let (lab_pk, tim_pk) = (ALLOCS.load(Relaxed) - m, t.elapsed());
    black_box(&per_key);

    let (m, t) = (ALLOCS.load(Relaxed), Instant::now());
    let mut parse_once: Vec<LabelSet> = Vec::with_capacity(N);
    for batch in &batches {
        let reader = StreamLabelReader::new(batch, &schema);
        for row in 0..batch.num_rows() {
            parse_once.push(reader.labels(row));
        }
    }
    let (lab_po, tim_po) = (ALLOCS.load(Relaxed) - m, t.elapsed());
    black_box(&parse_once);

    // Correctness: all three paths produce identical label content.
    assert_eq!(dto.len(), per_key.len());
    assert_eq!(dto.len(), parse_once.len());
    assert!(dto.iter().zip(&per_key).all(|(a, b)| a.iter().eq(b.iter())));
    assert!(
        dto.iter()
            .zip(&parse_once)
            .all(|(a, b)| a.iter().eq(b.iter()))
    );

    println!("── {title} ({N} rows) ─────────────────────────────");
    println!(
        "  fetch  : query_batches {fetch_b:>8} allocs   |   query(+materialize) {fetch_a:>8} allocs"
    );
    println!("  labels : DTO           {lab_dto:>8} allocs {tim_dto:>9.2?}");
    println!(
        "           L2 per-key    {lab_pk:>8} allocs {tim_pk:>9.2?}   (json_get once per KEY)"
    );
    println!(
        "           L2 parse-once {lab_po:>8} allocs {tim_po:>9.2?}   (parse blob once per ROW)"
    );
    println!();
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    run("fully promoted", &["http.route", "http.method", "region"]).await;
    run("not promoted", &[]).await;
}

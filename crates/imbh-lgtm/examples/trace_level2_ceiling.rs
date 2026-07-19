//! Ceiling measurement for trace Level-2: how much does `Span`/`Trace` DTO materialization cost per
//! trace, vs. the shared SQL scan? That delta is the *most* a raw-Arrow `fetch_trace` could save
//! (minus the attribute parse it would still do). Traces stream one at a time, so this is per-trace.
//!
//! Run: `cargo run -p imbh-lgtm --features source --example trace_level2_ceiling --release`

use std::alloc::{GlobalAlloc, Layout, System};
use std::hint::black_box;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

use imbh::{Db, TraceId};
use imbh_test_support::otlp::otlp_trace_wide;

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

#[tokio::main(flavor = "current_thread")]
async fn main() {
    const SPANS: usize = 100;
    let db = Db::in_memory().open().unwrap();
    db.ingest_otlp_traces(&otlp_trace_wide(
        "cart",
        [7u8; 16],
        SPANS,
        &[
            ("http.route", "/checkout"),
            ("http.method", "POST"),
            ("region", "us-east"),
            ("noise.a", "x"),
            ("noise.b", "y"),
        ],
    ))
    .await
    .unwrap();
    let tid = TraceId([7u8; 16]);

    // Warm up both paths.
    black_box(db.traces().get_batches(tid).await.unwrap());
    black_box(db.traces().get(tid).await.unwrap());

    let m = ALLOCS.load(Relaxed);
    let batches = db.traces().get_batches(tid).await.unwrap();
    let scan = ALLOCS.load(Relaxed) - m;
    black_box(&batches);

    let m = ALLOCS.load(Relaxed);
    let trace = db.traces().get(tid).await.unwrap().unwrap();
    let full = ALLOCS.load(Relaxed) - m;
    black_box(&trace);

    let ceiling = full.saturating_sub(scan);
    println!("── one {SPANS}-span trace, 5 attrs/span ──────────────────");
    println!("  get_batches (SQL scan only)      : {scan:>8} allocs");
    println!("  get (+ materialize + assemble)   : {full:>8} allocs");
    println!(
        "  materialize ceiling (the delta)  : {ceiling:>8} allocs  ({:.0}% of get)",
        100.0 * ceiling as f64 / full.max(1) as f64
    );
    println!(
        "  ⇒ Level-2 fetch_trace could save at most this delta (minus the attribute parse it keeps),\n     per streamed trace."
    );
}

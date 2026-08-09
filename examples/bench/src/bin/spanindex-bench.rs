//! Does `matches(name, …)` on spans ever clear the cost gate — i.e. is the spans `.tidx` earning its
//! keep?
//!
//! The spans Tantivy sidecar measured at **56% of the Parquet it indexes** on a synthetic corpus, and
//! it costs an index build at every seal. It serves two things: `search_attr_eq` (attribute equality
//! pruning, which a promoted column also covers) and `matches(name, …)` (tokenized term search, which
//! nothing else covers). So the question for the second is simply whether the cost-gated
//! `RowSelection` ever actually applies.
//!
//! **Detection is exact, not inferred from timing.** `ScanStats::rows_scanned` counts rows
//! materialized *after* the `RowSelection` pruned. So `rows_scanned < total rows` proves the
//! selection was applied; `rows_scanned == total` with `index_searched == true` proves the index was
//! consulted and the gate declined — the case where the sidecar costs a search and returns nothing.
//! `stream_with_stats` is the public surface for this, and the counters are complete only once the
//! stream is fully drained.
//!
//! The variable is the number of distinct span names, because selectivity is roughly `1/cardinality`
//! and the gate applies the selection below a ~0.5 hit fraction. A prior guess that "span names are
//! low-cardinality, therefore matches is unselective" conflated two different things: matching *one*
//! of 20 names is 5% selective. Cardinality 1–2 is what makes a matcher unselective, and that is the
//! metric-label case, not the span-name case.
//!
//! Run: `cargo run --release -p bench --bin spanindex-bench -- [segments] [spans_per_segment]`

use std::error::Error;
use std::time::Instant;

use imbh::{Db, WalMode};
use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
use opentelemetry_proto::tonic::common::v1::{AnyValue, KeyValue, any_value};
use opentelemetry_proto::tonic::resource::v1::Resource;
use opentelemetry_proto::tonic::trace::v1::{ResourceSpans, ScopeSpans, Span};
use prost::Message;

use futures::StreamExt;

/// The scenarios worth separating. `names` is the global distinct count; `per_seg` is how many of
/// them appear in any one segment. When those are equal the values are **interleaved** (every segment
/// carries every name); when `per_seg` is smaller the values are **temporally localized** (a rotating
/// window, the `pod.name` shape), so most segments contain no match at all.
///
/// Row counts alone cannot tell these apart, which is why wall-clock is reported too: §8's honest
/// cost model says a `RowSelection` skips whole Parquet pages only when a page has *zero* matches, and
/// otherwise saves per-row decode within a touched page. Interleaving guarantees every page holds a
/// match, so it is the case where pruning shows up in `rows_scanned` but least in time.
const SCENARIOS: [(&str, usize, usize); 6] = [
    ("degenerate", 1, 1),
    ("interleaved, low card", 4, 4),
    ("interleaved, mid card", 20, 20),
    ("interleaved, high card", 200, 200),
    ("localized, high card", 200, 10),
    ("localized, very high card", 2000, 10),
];

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn Error>> {
    let mut args = std::env::args().skip(1);
    let segments: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(20);
    let per_seg: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(5_000);
    let total = segments * per_seg;
    const SQL: &str = "SELECT name FROM spans WHERE matches(name, 'op0')";

    println!("imbh spans index cost-gate probe — {segments} segments x {per_seg} spans\n");
    println!(
        "  {:<26} {:>6} {:>8} {:>7}  {:>12}  {:>9}  verdict",
        "scenario", "names", "/seg", "sel", "rows_scanned", "ms"
    );

    for (label, names, names_per_seg) in SCENARIOS {
        let dir = tempfile::tempdir()?;
        let db = Db::builder(dir.path()).wal(WalMode::Off).open()?;
        for i in 0..segments {
            db.ingest_otlp_traces(&traces_body(i, per_seg, names, names_per_seg))
                .await?;
            db.flush().await?;
        }

        // Search for one specific operation name. `op0` is a whole token under imbh's tokenizer
        // (split on non-alphanumerics), so this is a clean single-term query.
        // Warm the page cache so the timing compares CPU, not first-read I/O.
        let _ = db.sql(SQL).collect().await?;
        let t = Instant::now();
        let (mut stream, stats) = db.sql(SQL).stream_with_stats().await?;
        let mut returned = 0usize;
        while let Some(batch) = stream.next().await {
            returned += batch?.num_rows();
        }
        let ms = t.elapsed().as_secs_f64() * 1e3;
        let s = stats.get();
        let applied = s.rows_scanned < total as u64;
        println!(
            "  {label:<26} {names:>6} {names_per_seg:>8} {:>6.2}%  {:>12}  {ms:>9.1}  {}",
            100.0 * returned as f64 / total as f64,
            s.rows_scanned,
            if applied {
                format!("PRUNED ({returned} matched)")
            } else {
                format!("gate DECLINED ({returned} matched)")
            }
        );
        db.close().await?;
    }
    println!(
        "\n  `rows_scanned < total` means the RowSelection applied. `index_searched` true with\n  \
         rows_scanned == total is the sidecar costing a search and returning nothing."
    );
    Ok(())
}

fn sv(s: &str) -> AnyValue {
    AnyValue {
        value: Some(any_value::Value::StringValue(s.to_owned())),
    }
}

/// One segment's spans. Segment `seg` draws from a window of `names_per_seg` operation names starting
/// at `seg * names_per_seg` (mod `names`), so `names_per_seg == names` interleaves everything into
/// every segment while a smaller value localizes each name to a few consecutive segments.
fn traces_body(seg: usize, rows: usize, names: usize, names_per_seg: usize) -> Vec<u8> {
    let base = (seg * rows) as u64;
    let spans = (0..rows)
        .map(|j| Span {
            trace_id: ((base + j as u64) as u128 + 1).to_be_bytes().to_vec(),
            span_id: (base + j as u64 + 1).to_be_bytes().to_vec(),
            name: format!("op{}", (seg * names_per_seg + (j % names_per_seg)) % names),
            start_time_unix_nano: base + j as u64 + 1,
            end_time_unix_nano: base + j as u64 + 500,
            ..Default::default()
        })
        .collect();
    ExportTraceServiceRequest {
        resource_spans: vec![ResourceSpans {
            resource: Some(Resource {
                attributes: vec![KeyValue {
                    key: "service.name".to_owned(),
                    value: Some(sv("cart")),
                    ..Default::default()
                }],
                ..Default::default()
            }),
            scope_spans: vec![ScopeSpans {
                spans,
                ..Default::default()
            }],
            ..Default::default()
        }],
    }
    .encode_to_vec()
}

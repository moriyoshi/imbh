//! Does the promotion/index classifier actually pick the winning backend?
//!
//! Every threshold in `examples/attr-stats` traces back to a single cost measurement
//! (`promote-cost`) plus reasoning. What has never been checked is the **decision rule**: for a key
//! with a given shape, does the verdict match the backend that is measurably fastest? This builds a
//! corpus of attribute *archetypes* — the shapes real OTel telemetry actually contains — and, for
//! each, measures all three backends and compares the winner against what the classifier says.
//!
//! **This is not an attempt to fake realistic data.** Synthetic input cannot tell us what a given
//! deployment's telemetry looks like; on a generator every sigma is 1.000 by construction unless the
//! generator is written to do otherwise, which is a fact about the generator. What it *can* do is
//! bound the space of shapes and test the rule across it: if the classifier picks the winner for
//! every archetype, the rule is sound *given* the shape, and the only remaining unknown is which
//! shapes a deployment has — a much smaller question, answerable from a few parameters rather than
//! from the data itself.
//!
//! ## The archetypes
//!
//! | key              | shape                                        | why it is here                         |
//! |------------------|----------------------------------------------|----------------------------------------|
//! | `env`            | one value, everywhere                        | control: maximal repetition            |
//! | `http.method`    | 7 values, uniformly mixed                    | control: low card, sigma 1             |
//! | `k8s.pod.name`   | ~50 live, rolling window                     | high global card, cheap per segment    |
//! | `session.id`     | ~25 events, 200 concurrent, interleaved      | **the middle case** (see below)        |
//! | `session.contig` | the same sessions, contiguous runs           | control isolating **run structure**    |
//! | `request.id`     | unique per row                               | promote costly, pruning ideal          |
//! | `tenant.id`      | Zipfian over 1,000                           | mean and median sigma diverge          |
//!
//! Three of these are adversarial rather than illustrative:
//!
//! - **`session.id` is the case the whole repetition-not-cardinality correction predicts.** Global
//!   cardinality is enormous, like a request id — but a session emits many events, so it repeats
//!   heavily *within* a segment while never recurring across them. The prediction is `promote`
//!   cheap **and** `index@` strong at once, which a global-cardinality gate would have rejected
//!   outright. `EVENTS_PER_SESSION` puts it deliberately near the `marginal`/`yes` boundary.
//! - **`request.id` is where the two verdicts must diverge**: unique per row is the +108 KB/key
//!   promotion regime, yet each value lives in exactly one segment, so pruning is near-perfect. A
//!   classifier that emits one label cannot express this, which is why the column was split.
//! - **`tenant.id` is where this tool's own reporting can lie.** `index@` is a *mean* sigma; a
//!   Zipfian key's mean and median come apart badly, so if the rule misfires anywhere it is here.
//!
//! Run: `cargo run --release -p bench --bin archetype-bench -- [segments] [rows_per_segment] [dir]`
//!
//! Passing `dir` keeps the unpromoted corpus on disk so `attr-stats` can be pointed at the very same
//! bytes — this prints ground truth (exact, by construction), that prints its estimate, and the two
//! should agree.

use std::collections::HashSet;
use std::error::Error;
use std::path::{Path, PathBuf};
use std::time::Instant;

use imbh::{Db, Promote, WalMode};
use opentelemetry_proto::tonic::collector::logs::v1::ExportLogsServiceRequest;
use opentelemetry_proto::tonic::common::v1::{AnyValue, KeyValue, any_value};
use opentelemetry_proto::tonic::logs::v1::{LogRecord, ResourceLogs, ScopeLogs};
use opentelemetry_proto::tonic::resource::v1::Resource;
use prost::Message;

const REPS: usize = 5;
/// Concurrently-live pods. The operator's figure: workloads scale, but under 50 in most deployments.
const LIVE_PODS: u64 = 50;
/// Segments a pod survives before the rolling window retires it.
const POD_LIFETIME_SEGMENTS: u64 = 8;
/// Events one session emits. Chosen to land near the classifier's `marginal`/`yes` boundary
/// (`1/25 = 0.04` against a 0.02 cheap threshold) — a threshold is only worth testing at its edge.
const EVENTS_PER_SESSION: u64 = 25;
/// Sessions alive at the same time. `session.id` round-robins over this many concurrent sessions,
/// which is what real traffic does; `session.contig` emits the same sessions in contiguous runs
/// instead. The two are a **controlled pair**: near-identical distinct counts, postings and
/// repetition, differing only in whether a session's events are interleaved with its neighbours'.
/// Any disk difference between them is attributable to run structure alone.
const LIVE_SESSIONS: u64 = 200;
/// Distinct tenants in the Zipfian tail.
const TENANTS: u64 = 1_000;
const METHODS: [&str; 7] = ["GET", "POST", "PUT", "DELETE", "PATCH", "HEAD", "OPTIONS"];

/// splitmix64 — a deterministic per-row bit source, so a run is reproducible without a seed and
/// without pulling in an RNG crate.
fn rnd(x: u64) -> u64 {
    let mut z = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Zipf rank for row `g`: weight `1/r` over [`TENANTS`] ranks, sampled by inverse CDF. Rank 1 takes
/// ~13% of traffic at 1,000 tenants, and the tail appears in only a handful of segments — which is
/// the whole point of including it.
fn zipf_rank(g: u64, cdf: &[f64]) -> u64 {
    let u = (rnd(g ^ 0x5EED) as f64 / u64::MAX as f64) * cdf[cdf.len() - 1];
    match cdf.binary_search_by(|p| p.partial_cmp(&u).expect("no NaN in the CDF")) {
        Ok(i) => i as u64 + 1,
        Err(i) => (i as u64 + 1).min(TENANTS),
    }
}

/// The attribute set for one row. Every archetype rides the same rows, so the comparison between
/// them is controlled — same segments, same row count, same everything but the shape of the value.
fn attrs_for(seg: u64, row: u64, rows_per_seg: u64, cdf: &[f64]) -> Vec<(&'static str, String)> {
    let g = seg * rows_per_seg + row;
    // The live pod window slides forward continuously rather than replacing all 50 at once, which is
    // what a rolling deploy or an autoscaler actually does.
    let pod_base = seg * LIVE_PODS / POD_LIFETIME_SEGMENTS;
    vec![
        ("env", "prod".to_owned()),
        ("http.method", METHODS[(g % 7) as usize].to_owned()),
        (
            "k8s.pod.name",
            format!("pod-{}", pod_base + rnd(g) % LIVE_PODS),
        ),
        // Interleaved: `LIVE_SESSIONS` sessions are active at once and a row belongs to a random
        // one of them. A generation lasts `LIVE_SESSIONS * EVENTS_PER_SESSION` rows, after which
        // every slot is recycled to a fresh session — so a session still emits ~25 events and never
        // recurs, but its events are scattered across ~5,000 rows rather than adjacent.
        (
            "session.id",
            format!(
                "sess-{}",
                (g / (LIVE_SESSIONS * EVENTS_PER_SESSION)) * LIVE_SESSIONS
                    + rnd(g ^ 0x5E55) % LIVE_SESSIONS
            ),
        ),
        // The same session population emitted in contiguous runs — the unrealistic model, kept as
        // the control so the run-structure term can be read off directly.
        ("session.contig", format!("sc-{}", g / EVENTS_PER_SESSION)),
        ("request.id", format!("req-{g}")),
        ("tenant.id", format!("tenant-{}", zipf_rank(g, cdf))),
    ]
}

/// Ground truth for one key, counted during generation rather than estimated. `attr-stats` has to
/// sample; here the generator knows.
struct Truth {
    name: &'static str,
    rows: u64,
    distinct: u64,
    /// Distinct `(value, segment)` pairs — the segment-index posting count.
    postings: u64,
    probe: String,
    probe_rows: u64,
    /// Times the value differed from the previous row's — the run count. Counted exactly here,
    /// estimated by `attr-stats` from the same corpus.
    runs: u64,
    /// Sum of value lengths over all rows, for the mean.
    len_sum: u64,
}

impl Truth {
    /// Mean rows per `(value, segment)` posting: in-segment repetition, which is what a promoted
    /// dictionary column costs.
    fn repetition(&self) -> f64 {
        self.rows as f64 / self.postings.max(1) as f64
    }

    /// Mean sigma over values: the fraction of segments an average value occupies.
    fn sigma_mean(&self, segments: u64) -> f64 {
        self.postings as f64 / (self.distinct.max(1) * segments) as f64
    }

    /// Estimated bytes per row a promoted column would occupy — **the shipped model**, mirrored from
    /// `attr-stats`: a Parquet dictionary (`C(seg) x mean length`) plus a per-row `Int32` index array
    /// (`runs x log2(C(seg)) / 8`), normalised by rows per segment so the figure is scale-free.
    ///
    /// The index term is the one a `postings/rows` gate cannot see, and it is frequently the larger.
    fn est_bytes_per_row(&self, segments: u64, rows_per_seg: u64) -> f64 {
        let c_seg = self.postings as f64 / segments as f64;
        let len = self.len_sum as f64 / self.rows.max(1) as f64;
        let runs_per_seg = self.runs as f64 / segments as f64;
        let bits = if c_seg > 1.0 { c_seg.log2() } else { 0.0 };
        (c_seg * len + runs_per_seg * bits / 8.0) / rows_per_seg as f64
    }

    /// The classifier from `examples/attr-stats`, applied to exact inputs. Every key here is
    /// string-valued and on every row, so only the cost estimate and sigma discriminate.
    ///
    /// Kept in step with `attr-stats` deliberately: this harness exists to check that the *shipped*
    /// rule orders keys the way disk does, so a divergent copy here would validate nothing.
    fn verdict(&self, segments: u64, rows_per_seg: u64) -> (&'static str, &'static str) {
        let est = self.est_bytes_per_row(segments, rows_per_seg);
        let promote = if est > 2.0 {
            "costly"
        } else if est > 0.5 {
            "marginal"
        } else {
            "yes"
        };
        let index = if self.sigma_mean(segments) <= 0.25 {
            "yes"
        } else {
            "no"
        };
        (promote, index)
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = std::env::args().skip(1);
    let segments: u64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(12);
    let rows: u64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(4_000);
    let keep: Option<PathBuf> = args.next().map(PathBuf::from);
    let total = segments * rows;

    let mut cdf = Vec::with_capacity(TENANTS as usize);
    let mut acc = 0.0;
    for r in 1..=TENANTS {
        acc += 1.0 / r as f64;
        cdf.push(acc);
    }

    println!(
        "imbh attribute archetypes — {segments} segments x {rows} rows ({total} total), best of {REPS}\n"
    );

    let truth = ground_truth(segments, rows, &cdf);

    println!("== GROUND TRUTH (counted during generation, not estimated) ==");
    println!(
        "  {:<16} {:>10} {:>11} {:>8} {:>9} {:>9} {:>9}  verdict (promote/index)",
        "key", "distinct", "postings", "rep", "runs/row", "est B/row", "sigma~"
    );
    for t in &truth {
        let (p, i) = t.verdict(segments, rows);
        println!(
            "  {:<16} {:>10} {:>11} {:>8.1} {:>9.2} {:>9.2} {:>9.4}  {p} / {i}",
            t.name,
            t.distinct,
            t.postings,
            t.repetition(),
            t.runs as f64 / t.rows as f64,
            t.est_bytes_per_row(segments, rows),
            t.sigma_mean(segments),
        );
    }

    // The unpromoted corpus: the JSON path, and the one `attr-stats` gets pointed at.
    let base = tempfile::tempdir()?;
    let dir = keep.as_deref().unwrap_or(base.path());
    if keep.is_some() {
        let _ = std::fs::remove_dir_all(dir);
        std::fs::create_dir_all(dir)?;
    }
    let plain = build(dir, segments, rows, &cdf, &[])?;

    println!("\n== QUERY COST PER BACKEND (ms, best of {REPS}) ==");
    println!(
        "  {:<16} {:>9} {:>9} {:>10} {:>10} {:>9}  measured best",
        "key", "sel%", "floor", "json", "promoted", "disk +B"
    );
    let floor = bench(&plain, "SELECT count(*) AS c FROM logs")?;
    let plain_bytes = dir_bytes(dir)?;
    drop(plain);

    let mut rows_out: Vec<(&str, &str, (&str, &str), i64)> = Vec::new();
    for t in &truth {
        let json_sql = format!(
            "SELECT count(*) AS c FROM logs WHERE json_get_str(attributes, '{}') = '{}'",
            t.name, t.probe
        );
        let plain = build(dir, segments, rows, &cdf, &[])?;
        let json_ms = bench(&plain, &json_sql)?;
        drop(plain);

        // One DB per key so the disk delta is attributable to that key alone.
        let promoted_dir = tempfile::tempdir()?;
        let db = build(promoted_dir.path(), segments, rows, &cdf, &[t.name])?;
        let promoted_sql = format!(
            "SELECT count(*) AS c FROM logs WHERE CASE WHEN \"{0}\" IS NOT NULL \
             THEN CAST(\"{0}\" AS VARCHAR) ELSE json_get_str(attributes, '{0}') END = '{1}'",
            t.name, t.probe
        );
        let promoted_ms = bench(&db, &promoted_sql)?;
        drop(db);
        let delta = dir_bytes(promoted_dir.path())? as i64 - plain_bytes as i64;

        let best = if promoted_ms < json_ms * 0.9 {
            "promoted"
        } else if json_ms < promoted_ms * 0.9 {
            "json"
        } else {
            "tie"
        };
        println!(
            "  {:<16} {:>8.2}% {:>9.2} {:>10.2} {:>10.2} {:>9}  {best}",
            t.name,
            100.0 * t.probe_rows as f64 / total as f64,
            floor,
            json_ms,
            promoted_ms,
            delta,
        );
        rows_out.push((t.name, best, t.verdict(segments, rows), delta));
    }

    // The promote verdict predicts **disk cost**, so validate it against disk — not against which
    // backend was fastest. Comparing it to speed (an earlier version of this section) is a category
    // error: at low selectivity the `attrs` index already takes the JSON path to the floor, so
    // promotion cannot win however cheap its column is.
    println!("\n== DOES THE PROMOTE VERDICT PREDICT DISK COST? ==");
    println!("  Ranked by measured bytes. The verdict claims to order these; an inversion is a");
    println!("  key rated cheaper than one that actually costs less.");
    rows_out.sort_by_key(|(_, _, _, delta)| *delta);
    let rank = |v: &str| match v {
        "yes" => 0,
        "marginal" => 1,
        _ => 2,
    };
    let mut worst = 0;
    let mut inversions = 0;
    println!("  {:<16} {:>10}  promote", "key", "disk +B");
    for (name, _, (promote, _), delta) in &rows_out {
        let r = rank(promote);
        let flag = if r < worst {
            inversions += 1;
            "INVERSION — rated cheaper than a costlier-rated key below it"
        } else {
            worst = worst.max(r);
            ""
        };
        println!("  {name:<16} {delta:>10}  {promote:<9}  {flag}");
    }
    if inversions == 0 {
        println!("\n  No inversions: the verdict orders these keys the way disk does.");
    } else {
        println!(
            "\n  {inversions} inversion(s). The gate is `postings/rows`, which models the Parquet\n               dictionary but not the per-row index array — whose compressed size depends on the\n               ENTROPY of the value sequence within a segment. Compare `session.id` against\n               `session.contig`: same population, same ~25 events each, but interleaved vs contiguous."
        );
    }

    println!("\n== WHICH BACKEND WAS ACTUALLY FASTEST (a different question) ==");
    for (name, best, (_, index), _) in &rows_out {
        println!("  {name:<16} fastest={best:<9} index-verdict={index}");
    }

    if let Some(kept) = keep {
        let plain = build(&kept, segments, rows, &cdf, &[])?;
        drop(plain);
        println!(
            "\n  corpus kept at {} — point attr-stats at it and compare against GROUND TRUTH above:\n    cargo run --release -p attr-stats -- {} --scope attributes",
            kept.display(),
            kept.display()
        );
    }
    Ok(())
}

/// Count exact per-key statistics by replaying the same generator the corpus uses.
fn ground_truth(segments: u64, rows: u64, cdf: &[f64]) -> Vec<Truth> {
    let names: Vec<&'static str> = attrs_for(0, 0, rows, cdf).iter().map(|(k, _)| *k).collect();
    let mut values: Vec<HashSet<String>> = vec![HashSet::new(); names.len()];
    let mut postings: Vec<HashSet<(String, u64)>> = vec![HashSet::new(); names.len()];
    let mut runs: Vec<u64> = vec![0; names.len()];
    let mut len_sum: Vec<u64> = vec![0; names.len()];
    // Row order matters: a run starts wherever a key's value differs from the previous row's, which
    // is what the index-array term costs. `prev` carries across segments, as it does on disk.
    let mut prev: Vec<Option<String>> = vec![None; names.len()];
    for seg in 0..segments {
        for row in 0..rows {
            for (i, (_, v)) in attrs_for(seg, row, rows, cdf).into_iter().enumerate() {
                if prev[i].as_deref() != Some(v.as_str()) {
                    runs[i] += 1;
                }
                len_sum[i] += v.len() as u64;
                prev[i] = Some(v.clone());
                postings[i].insert((v.clone(), seg));
                values[i].insert(v);
            }
        }
    }
    // Probe value: whatever the midpoint row carries, so every key is filtered on a value that
    // genuinely occurs rather than a synthetic one.
    let mid = attrs_for(segments / 2, rows / 2, rows, cdf);
    names
        .iter()
        .enumerate()
        .map(|(i, name)| {
            let probe = mid[i].1.clone();
            let mut probe_rows = 0u64;
            for seg in 0..segments {
                for row in 0..rows {
                    if attrs_for(seg, row, rows, cdf)[i].1 == probe {
                        probe_rows += 1;
                    }
                }
            }
            Truth {
                name,
                rows: segments * rows,
                distinct: values[i].len() as u64,
                postings: postings[i].len() as u64,
                probe,
                probe_rows,
                runs: runs[i],
                len_sum: len_sum[i],
            }
        })
        .collect()
}

fn build(
    dir: &Path,
    segments: u64,
    rows: u64,
    cdf: &[f64],
    promote: &[&str],
) -> Result<imbh::BlockingDb, Box<dyn Error>> {
    let _ = std::fs::remove_dir_all(dir);
    std::fs::create_dir_all(dir)?;
    let db = Db::builder(dir)
        .wal(WalMode::Off)
        .promote(Promote::new(promote.iter().copied()))
        .open()?;
    let b = db.blocking();
    for seg in 0..segments {
        b.ingest_otlp_logs(&logs_body(seg, rows, cdf))?;
        b.flush()?;
    }
    Ok(b)
}

fn bench(b: &imbh::BlockingDb, sql: &str) -> Result<f64, Box<dyn Error>> {
    let _ = b.sql(sql)?;
    let mut best = f64::MAX;
    for _ in 0..REPS {
        let t = Instant::now();
        let out = b.sql(sql)?;
        std::hint::black_box(&out);
        best = best.min(t.elapsed().as_secs_f64() * 1e3);
    }
    Ok(best)
}

fn dir_bytes(dir: &Path) -> Result<u64, Box<dyn Error>> {
    fn walk(dir: &Path, acc: &mut u64) -> Result<(), Box<dyn Error>> {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let meta = entry.metadata()?;
            if meta.is_dir() {
                walk(&entry.path(), acc)?;
            } else {
                *acc += meta.len();
            }
        }
        Ok(())
    }
    let mut total = 0;
    walk(dir, &mut total)?;
    Ok(total)
}

fn sv(s: &str) -> AnyValue {
    AnyValue {
        value: Some(any_value::Value::StringValue(s.to_owned())),
    }
}

fn logs_body(seg: u64, rows: u64, cdf: &[f64]) -> Vec<u8> {
    let records = (0..rows)
        .map(|r| LogRecord {
            time_unix_nano: seg * rows + r + 1,
            severity_number: 9,
            body: Some(sv("request completed")),
            attributes: attrs_for(seg, r, rows, cdf)
                .into_iter()
                .map(|(k, v)| KeyValue {
                    key: k.to_owned(),
                    value: Some(sv(&v)),
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        })
        .collect();
    ExportLogsServiceRequest {
        resource_logs: vec![ResourceLogs {
            resource: Some(Resource {
                attributes: vec![KeyValue {
                    key: "service.name".to_owned(),
                    value: Some(sv("cart")),
                    ..Default::default()
                }],
                ..Default::default()
            }),
            scope_logs: vec![ScopeLogs {
                log_records: records,
                ..Default::default()
            }],
            ..Default::default()
        }],
    }
    .encode_to_vec()
}

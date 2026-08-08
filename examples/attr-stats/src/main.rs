//! Measure attribute **cardinality** and per-segment **selectivity** over an existing imbh database.
//!
//! Three open design questions need the same statistic, and this tool produces it:
//!
//! 1. *Is a segment-granularity attribute index worth building?* — a per-segment "does this segment
//!    contain `key = value`" structure that prunes whole segments before any Parquet page is read.
//!    Metric tables have no Tantivy `.tidx` sidecar, so they are the case that matters. The deciding
//!    number is **sigma**: for one `(key, value)` pair, the fraction of a table's segments that
//!    contain at least one matching row. Pruning saves `1 - sigma`. Sigma near 1 (a label every
//!    segment carries, like `env`) means the index buys nothing; sigma near `1/N` (a trace id, a
//!    short-lived pod name) means it prunes almost everything.
//! 2. *Could `promote = [...]` become automatic?* — promotion turns a listed attribute key into a
//!    nullable `Dictionary(Int32,Utf8)` column so filters hit a column instead of a `json_get_str`
//!    JSON scan. Picking that list by hand needs knowledge of the data. The same statistic
//!    classifies keys for both mechanisms: **low** cardinality favours a promoted column, **high**
//!    cardinality with **low** sigma favours a segment index, and neither favours leaving it in JSON.
//! 3. *At what time scale does either mechanism pay?* — sigma answers (1) at exactly one granularity,
//!    the segment, but the same key can be localized against a day and interleaved against a minute.
//!    So cardinality is reported as a **curve** over a ladder of window widths (`--windows`), and its
//!    shape — flat or rising — is what separates "every segment already holds every value, nothing
//!    prunes" from "values churn, and pruning removes almost everything". See `accum` for the model.
//!
//! ```text
//! cargo run -p attr-stats -- ./demo-db
//! cargo run -p attr-stats -- ./demo-db --scope attributes --top 20
//! cargo run -p attr-stats -- ./demo-db --json > attrs.json
//! ```
//!
//! **It reads and changes nothing.** The segment set comes from the manifest via
//! `imbh_storage::read_disk_snapshot` (no writer lock, so it runs against a live database), and each
//! segment is opened read-only with just its attribute columns projected. No new column, no sidecar,
//! no manifest edit — this measures the database you already have.
//!
//! **What it does not cover.** Only *sealed* segments: rows still in the mutable buffer or the
//! unsealed WAL tail have no segment to be selective within, so they are excluded (the header
//! reports how many WAL frames were skipped — `flush()` first if you want them counted). Promoted
//! keys that are *already* columns still appear here, because the key stays in the JSON blob too.
//!
//! Both map levels are hash-sampled to bound memory (see `accum::SampledMap`, a bottom-k sketch);
//! whenever a cap engages, the affected row is marked and the sample rate is printed. A truncated
//! result that reads like full coverage would be worse than no result at all. The sample is a pure
//! function of the data, not of the order it was read in, so two runs over the same database report
//! the same numbers even where the caps engaged.

mod accum;
mod scan;

use std::error::Error;
use std::path::PathBuf;

use accum::{Acc, AttrScope, SigmaSummary, summarize};
use imbh_core::{SegmentRef, Table};

const USAGE: &str = "\
attr-stats <db-dir> [options]

  --scope <all|attributes>  attribute scopes to read (default: all). `attributes` restricts the
                            scan to the record-attribute column — the only scope `promote` covers.
                            `all` also reads `resource:`/`scope:`-prefixed keys, which a segment
                            index could cover too.
  --last <minutes>          only consider segments overlapping the last N minutes (default: all)
  --windows <d,..>          window widths for the cardinality-vs-time-scale ladder, innermost
                            first, strictly increasing (default: 1m,1h,24h). Suffixes s/m/h/d.
                            `--windows none` skips the ladder and its per-value memory.
  --top <n>                 keys listed per table, by descending index cost (default: 25)
  --max-keys <n>            per-scan-unit key cap before hash sampling engages (default: 4096)
  --max-values <n>          per-key distinct-value cap before hash sampling engages (default: 50000)
  --batch-size <n>          Parquet read batch size (default: 8192)
  --json                    emit JSON instead of the text report
  -h, --help                this message
";

const NANOS_PER_SEC: i64 = 1_000_000_000;

/// Parse `30s` / `5m` / `1h` / `7d` (bare digits are seconds) into nanoseconds.
fn parse_duration(s: &str) -> Result<i64, Box<dyn Error>> {
    let s = s.trim();
    let (digits, mult) = match s.chars().last() {
        Some('s') => (&s[..s.len() - 1], 1),
        Some('m') => (&s[..s.len() - 1], 60),
        Some('h') => (&s[..s.len() - 1], 3_600),
        Some('d') => (&s[..s.len() - 1], 86_400),
        _ => (s, 1),
    };
    let n: i64 = digits
        .parse()
        .map_err(|_| format!("bad duration {s:?} — expected e.g. 30s, 5m, 1h, 7d"))?;
    if n <= 0 {
        return Err(format!("duration {s:?} must be positive").into());
    }
    Ok(n * mult * NANOS_PER_SEC)
}

struct Config {
    dir: PathBuf,
    scopes: Vec<AttrScope>,
    last_minutes: Option<u64>,
    /// `(label, width_nanos)`, innermost first. Empty disables the ladder.
    windows: Vec<(String, i64)>,
    top: usize,
    max_keys: usize,
    max_values: usize,
    batch_size: usize,
    json: bool,
}

impl Config {
    fn for_dir(dir: PathBuf) -> Self {
        Self {
            dir,
            scopes: vec![AttrScope::Attributes, AttrScope::Resource, AttrScope::Scope],
            last_minutes: None,
            windows: ["1m", "1h", "24h"]
                .iter()
                .map(|d| {
                    (
                        (*d).to_owned(),
                        parse_duration(d).expect("literal duration"),
                    )
                })
                .collect(),
            top: 25,
            max_keys: 4096,
            max_values: 50_000,
            batch_size: 8192,
            json: false,
        }
    }

    fn widths(&self) -> Vec<i64> {
        self.windows.iter().map(|(_, w)| *w).collect()
    }

    fn from_args() -> Result<Option<Self>, Box<dyn Error>> {
        let mut args = std::env::args().skip(1);
        let mut dir: Option<PathBuf> = None;
        let mut cfg = Config::for_dir(PathBuf::new());
        while let Some(arg) = args.next() {
            let mut value = || -> Result<String, Box<dyn Error>> {
                args.next()
                    .ok_or_else(|| format!("{arg} needs a value").into())
            };
            match arg.as_str() {
                "-h" | "--help" => return Ok(None),
                "--scope" => {
                    cfg.scopes = match value()?.as_str() {
                        "all" => vec![AttrScope::Attributes, AttrScope::Resource, AttrScope::Scope],
                        "attributes" => vec![AttrScope::Attributes],
                        other => return Err(format!("unknown --scope {other}").into()),
                    }
                }
                "--last" => cfg.last_minutes = Some(value()?.parse()?),
                "--windows" => {
                    let spec = value()?;
                    cfg.windows = if spec == "none" {
                        Vec::new()
                    } else {
                        spec.split(',')
                            .map(|d| parse_duration(d).map(|w| (d.trim().to_owned(), w)))
                            .collect::<Result<Vec<_>, _>>()?
                    };
                    // The ladder is read as a curve, so the widths must increase.
                    if cfg.windows.windows(2).any(|p| p[0].1 >= p[1].1) {
                        return Err("--windows must be strictly increasing".into());
                    }
                }
                "--top" => cfg.top = value()?.parse()?,
                "--max-keys" => cfg.max_keys = value()?.parse()?,
                "--max-values" => cfg.max_values = value()?.parse()?,
                "--batch-size" => cfg.batch_size = value()?.parse::<usize>()?.max(1),
                "--json" => cfg.json = true,
                other if other.starts_with('-') => {
                    return Err(format!("unknown option {other}").into());
                }
                other => dir = Some(PathBuf::from(other)),
            }
        }
        cfg.dir = dir.ok_or("missing <db-dir>")?;
        Ok(Some(cfg))
    }

    /// Segment selection: keep segments whose `[min_time, max_time]` overlaps the requested window.
    /// Sigma is defined over "the segments in a time range", so narrowing the range is part of the
    /// measurement, not a shortcut.
    fn selects(&self, seg: &SegmentRef) -> bool {
        match self.last_minutes {
            None => true,
            Some(minutes) => {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos() as i64)
                    .unwrap_or(i64::MAX);
                let start = now.saturating_sub((minutes as i64).saturating_mul(60_000_000_000));
                seg.max_time_unix_nano >= start
            }
        }
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let Some(cfg) = Config::from_args()? else {
        print!("{USAGE}");
        return Ok(());
    };
    let report = analyze(&cfg)?;
    if cfg.json {
        println!("{}", serde_json::to_string_pretty(&report.to_json())?);
    } else {
        report.print(cfg.top);
    }
    Ok(())
}

// ── the scan ────────────────────────────────────────────────────────────────────────────────

fn analyze(cfg: &Config) -> Result<Report, Box<dyn Error>> {
    let snapshot = imbh_storage::read_disk_snapshot(&cfg.dir)?;
    let widths = cfg.widths();
    // The DB-wide unit backs the promotion report: `promote` is one DB-wide list, so a key's
    // cardinality and coverage must be measured across every table, not per table.
    let mut global = Acc::new("ALL", cfg.max_keys, cfg.max_values, &widths);
    let mut units: Vec<Acc> = Table::ALL
        .iter()
        .map(|t| Acc::new(t.as_str(), cfg.max_keys, cfg.max_values, &widths))
        .collect();
    let mut segments_skipped: Vec<String> = Vec::new();

    // Every segment in one list, **sorted by start time**, because the window ladder dedups against
    // the currently-open window rather than a set (`Acc::begin_segment`). Sorting once here is what
    // lets the DB-wide unit — which interleaves tables — use the ladder at all; each per-table unit
    // then sees a sorted subsequence, which is still sorted.
    let mut work: Vec<(usize, &SegmentRef)> = Vec::new();
    for (idx, table) in Table::ALL.iter().enumerate() {
        let all: &[SegmentRef] = match table {
            Table::Logs => &snapshot.logs_segments,
            Table::Spans => &snapshot.spans_segments,
            other => snapshot
                .metric_segments
                .get(other)
                .map(|v| v.as_slice())
                .unwrap_or(&[]),
        };
        work.extend(all.iter().filter(|s| cfg.selects(s)).map(|s| (idx, s)));
    }
    work.sort_by(|(_, a), (_, b)| {
        a.min_time_unix_nano
            .cmp(&b.min_time_unix_nano)
            .then_with(|| a.relative_path.cmp(&b.relative_path))
    });

    for (idx, seg) in work {
        let path = snapshot.dir.join(&seg.relative_path);
        units[idx].begin_segment(seg.min_time_unix_nano, seg.max_time_unix_nano);
        global.begin_segment(seg.min_time_unix_nano, seg.max_time_unix_nano);
        let mut sinks: [&mut Acc; 2] = [&mut units[idx], &mut global];
        if let Err(e) = scan::scan_segment(&path, &cfg.scopes, cfg.batch_size, &mut sinks) {
            // A segment can vanish under us (retention/compaction on a live writer). Report it
            // rather than aborting the whole measurement or silently under-counting.
            segments_skipped.push(format!("{}: {e}", seg.relative_path));
        }
    }

    let levels: Vec<LevelReport> = cfg
        .windows
        .iter()
        .enumerate()
        .map(|(i, (label, width))| LevelReport {
            label: label.clone(),
            width_nanos: *width,
            windows: global.windows[i],
        })
        .collect();

    Ok(Report {
        dir: cfg.dir.display().to_string(),
        scopes: cfg.scopes.clone(),
        max_keys: cfg.max_keys,
        max_values: cfg.max_values,
        last_minutes: cfg.last_minutes,
        levels,
        pending_wal_frames: snapshot.pending.len(),
        segments_skipped,
        tables: units.iter().map(summarize_unit).collect(),
        global: summarize_unit(&global),
    })
}

// ── the report model ────────────────────────────────────────────────────────────────────────

struct KeyReport {
    name: String,
    scope: AttrScope,
    rows_present: u64,
    rows_string: u64,
    str_len_avg: f64,
    str_len_max: u32,
    segments_present: u32,
    /// Distinct values actually held in memory.
    values_tracked: usize,
    /// `1.0` while the value map was exact; below it once the cap forced hash sampling — the flag
    /// that makes `distinct_est`/`postings_est` estimates rather than counts.
    values_sample_rate: f64,
    /// `values_tracked / values_sample_rate`: the estimated true distinct-value count, which is also
    /// the distinct `(key, value)` pair count for this key.
    distinct_est: f64,
    /// `(key, value, segment)` entries a segment index would hold for this key — the direct size
    /// bound, and equal to `distinct * mean_sigma * segments`.
    postings_est: f64,
    sigma: Option<SigmaSummary>,
    /// `C(w)`: mean distinct values of this key within one window, one entry per configured level,
    /// innermost first. `None` for a level that opened no window.
    curve: Vec<Option<f64>>,
    /// `C(segment)` — the innermost point of the same curve, and `postings / segments`.
    c_segment: Option<f64>,
    /// Mean rows per `(value, segment)` posting: **in-segment repetition**.
    repetition: f64,
    /// Times this key's value differed from the previous row's, over the whole scan.
    runs: u64,
    /// Estimated bytes **per row** a promoted column would occupy, before compression.
    ///
    /// A promoted column is a Parquet **dictionary** plus a per-row **`Int32` index array**, and the
    /// two scale differently:
    ///
    /// - dictionary  ~ `C(seg) * mean value length` — distinct values held per segment;
    /// - index array ~ `runs_per_segment * log2(C(seg)) / 8` — each run start costs about
    ///   `log2(distinct)` bits, and rows inside a run cost ~nothing once run-length and zstd have
    ///   had them.
    ///
    /// The second term is the one `postings/rows` alone could not see, and it is frequently the
    /// larger. Measured against `archetype-bench`: `k8s.pod.name` (few distinct per segment but
    /// randomly interleaved) costs 42,135 B while `session.contig` (more distinct per segment,
    /// arriving in runs) costs 9,079 B — an ordering the dictionary term alone gets backwards.
    ///
    /// Divided by rows per segment so the figure is **scale-free**: a 200-row segment cannot hold
    /// much whatever its contents, and an absolute per-segment threshold would call every key in a
    /// small corpus cheap.
    ///
    /// Absolute values run 2-5x high because zstd exploits structure this model does not
    /// (`request.id`'s strictly-increasing index compresses far better than its entropy suggests).
    /// It is used for **ordering** keys by cost, which is what a verdict needs, not for predicting
    /// bytes — and on `archetype-bench`'s seven archetypes the ordering it produces matches measured
    /// disk exactly.
    est_bytes_per_row: f64,
}

impl KeyReport {
    fn is_sampled(&self) -> bool {
        self.values_sample_rate < 1.0
    }

    /// `C(all) / C(segment)`: how much bigger the key's value space is over the whole scan than
    /// within one segment. ~1 means interleaved (every segment already holds every value, so nothing
    /// prunes); large means localized (values churn, and segment pruning removes `1 - 1/locality`).
    fn locality(&self) -> Option<f64> {
        let c_seg = self.c_segment?;
        (c_seg > 0.0).then(|| self.distinct_est / c_seg)
    }
}

struct LevelReport {
    label: String,
    width_nanos: i64,
    /// Distinct windows this width opened over the scan. `1` means the width covers everything (the
    /// level has collapsed onto `C(all)`); a value near the segment count means it has collapsed onto
    /// `C(segment)`. Either way the level says nothing new, and the report flags it.
    windows: u32,
}

struct UnitReport {
    label: String,
    segments: u32,
    rows: u64,
    keys_tracked: usize,
    keys_sample_rate: f64,
    keys_est: f64,
    /// Windows opened per level within *this* unit — the `C(w)` denominator, which differs per table.
    windows: Vec<u32>,
    span_nanos: i64,
    keys: Vec<KeyReport>,
}

impl UnitReport {
    /// Mean rows per segment — the ceiling on how much any value can repeat within one.
    fn rows_per_segment(&self) -> f64 {
        if self.segments == 0 {
            0.0
        } else {
            self.rows as f64 / f64::from(self.segments)
        }
    }
}

struct Report {
    dir: String,
    scopes: Vec<AttrScope>,
    max_keys: usize,
    max_values: usize,
    last_minutes: Option<u64>,
    levels: Vec<LevelReport>,
    pending_wal_frames: usize,
    segments_skipped: Vec<String>,
    tables: Vec<UnitReport>,
    global: UnitReport,
}

fn summarize_unit(acc: &Acc) -> UnitReport {
    let mut keys: Vec<KeyReport> = acc
        .keys
        .iter()
        .map(|(_, k)| {
            let rate = k.values.sample_rate();
            let sigma = summarize(k.values.iter().map(|(_, v)| v.segments), acc.segments);
            let postings_est = k.postings() as f64 / rate;
            // Every point of the curve is `postings at that scale / windows at that scale`, scaled
            // by the same sample rate the distinct estimate uses, so the whole curve is consistently
            // an estimate or consistently exact.
            let curve = (0..acc.levels())
                .map(|level| {
                    let windows = acc.windows[level];
                    (windows > 0)
                        .then(|| k.window_postings(level) as f64 / rate / f64::from(windows))
                })
                .collect();
            KeyReport {
                name: k.name.clone(),
                scope: k.scope,
                rows_present: k.rows_present,
                rows_string: k.rows_string,
                str_len_avg: if k.rows_string == 0 {
                    0.0
                } else {
                    k.str_len_sum as f64 / k.rows_string as f64
                },
                str_len_max: k.str_len_max,
                segments_present: k.segments_present,
                values_tracked: k.values.tracked(),
                values_sample_rate: rate,
                distinct_est: k.values.estimated_total(),
                postings_est,
                sigma,
                curve,
                c_segment: (acc.segments > 0)
                    .then(|| postings_est / f64::from(acc.segments))
                    .filter(|c| *c > 0.0),
                repetition: if postings_est > 0.0 {
                    k.rows_present as f64 / postings_est
                } else {
                    0.0
                },
                runs: k.runs,
                est_bytes_per_row: {
                    let segs = f64::from(acc.segments.max(1));
                    let rows_per_seg = (acc.rows as f64 / segs).max(1.0);
                    let c_seg = postings_est / segs;
                    let len = if k.rows_string == 0 {
                        0.0
                    } else {
                        k.str_len_sum as f64 / k.rows_string as f64
                    };
                    let runs_per_seg = k.runs as f64 / segs;
                    let bits = if c_seg > 1.0 { c_seg.log2() } else { 0.0 };
                    (c_seg * len + runs_per_seg * bits / 8.0) / rows_per_seg
                },
            }
        })
        .collect();
    // Descending index cost: the keys whose postings dominate a segment index come first.
    keys.sort_by(|a, b| {
        b.postings_est
            .partial_cmp(&a.postings_est)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.name.cmp(&b.name))
    });
    UnitReport {
        label: acc.label.clone(),
        segments: acc.segments,
        rows: acc.rows,
        keys_tracked: acc.keys.tracked(),
        keys_sample_rate: acc.keys.sample_rate(),
        keys_est: acc.keys.estimated_total(),
        windows: acc.windows.clone(),
        span_nanos: if acc.segments == 0 {
            0
        } else {
            acc.max_time.saturating_sub(acc.min_time)
        },
        keys,
    }
}

/// Estimated bytes per row at or below which a promoted column is cheap.
///
/// **Not cardinality, and not repetition alone.** Parquet builds its dictionary per column chunk, so
/// a promoted column costs whatever its values fail to recur within a segment — that much was
/// established by `promote-cost` (+1,206 B/key at 3,125x repetition, +108,842 B/key at 1x). But a
/// column is a dictionary *plus a per-row index array*, and the index array's compressed size tracks
/// the **entropy of the value sequence**, which repetition alone cannot see.
///
/// `archetype-bench` isolated it with a controlled pair: the same session population, ~25 events
/// each, emitted contiguously versus interleaved across 200 concurrent sessions. Near-identical
/// distinct counts and postings; **9,079 B against 64,252 B on disk**. A gate on `postings/rows`
/// gave both the same verdict, and rated `k8s.pod.name` (42,135 B) cheaper than the contiguous
/// sessions (9,079 B) — an inversion.
///
/// So the gate is now [`KeyReport::est_bytes_per_row`], which carries both terms. The estimate runs
/// 2-5x high in absolute terms, so these are *relative* boundaries calibrated against that run
/// rather than a byte budget. Ranked by this figure, the seven archetypes come out in exactly their
/// measured disk order.
const PROMOTE_MAX_EST_BYTES_PER_ROW: f64 = 0.5;
/// Estimated bytes per row above which the column is firmly in the expensive regime.
const PROMOTE_POOR_EST_BYTES_PER_ROW: f64 = 2.0;
/// Rows per segment below which nothing about repetition or run structure can be resolved — a
/// segment shorter than this cannot exhibit the recurrence a cheap verdict requires, so reporting
/// "expensive" would describe the corpus rather than the key.
const PROMOTE_MIN_ROWS_PER_SEGMENT: f64 = 50.0;
/// Fraction of a key's values that must be strings for a promoted column to be worth anything —
/// `lookup_promoted` leaves every non-string cell NULL.
const PROMOTE_MIN_STRING_FRACTION: f64 = 0.9;
/// Share of all rows a promoted column must be non-NULL on to count as "widely present".
const PROMOTE_MIN_COVERAGE: f64 = 0.01;
/// Sigma at or below which segment pruning saves most of the scan.
const INDEX_MAX_SIGMA: f64 = 0.25;

/// Would a promoted column pay for this key, and what would it cost?
///
/// This is one of **two independent verdicts**, not a branch of one. The two mechanisms answer
/// different questions and a key can want both: on the `prune-bench` corpus `shard` has 60 distinct
/// values *and* sigma 0.017, so it wants a promoted column (fast filtering) and a segment index
/// (pruning). The predecessor was an if/else chain and could only ever say one, which is why it is
/// now split.
///
/// - `yes` — string-valued, repeats enough within a segment to be cheap, and present on enough rows.
/// - `yes?` — cheap and string-valued but rare (see `coverage`): the column is mostly NULL, which
///   costs little but only pays if the key is actually queried.
/// - `costly` — the column would be near-unique per row, the +108 KB/key regime.
/// - `no` — too few string values for the column to populate at all.
/// - `-` — no rows, or segments too small for the dictionary fraction to mean anything.
fn promote_verdict(key: &KeyReport, total_rows: u64, rows_per_segment: f64) -> &'static str {
    if key.rows_present == 0 {
        return "-";
    }
    let string_frac = key.rows_string as f64 / key.rows_present as f64;
    let coverage = key.rows_string as f64 / total_rows.max(1) as f64;
    if string_frac < PROMOTE_MIN_STRING_FRACTION {
        return "no";
    }
    if rows_per_segment < PROMOTE_MIN_ROWS_PER_SEGMENT {
        // Too few rows per segment for repetition to be observable. Saying "costly" here would
        // describe the corpus, not the key.
        return "-";
    }
    if key.est_bytes_per_row > PROMOTE_POOR_EST_BYTES_PER_ROW {
        return "costly";
    }
    if key.est_bytes_per_row > PROMOTE_MAX_EST_BYTES_PER_ROW {
        return "marginal";
    }
    if coverage >= PROMOTE_MIN_COVERAGE {
        "yes"
    } else {
        "yes?"
    }
}

impl KeyReport {
    /// Mean sigma at each rung of the window ladder: the fraction of a window's segments that an
    /// average value of this key occupies.
    ///
    /// `sigma(w) = C(seg) / C(w)`, which falls out of the definitions — a window of width `w` holds
    /// `C(w)` distinct values across `segments/windows` segments, and `C(seg) = sigma(w) * C(w)`.
    /// The outermost rung is `C(seg)/C(all) = 1/locality`, and the innermost is 1 by construction
    /// (within a single segment, a value that is present occupies all of it).
    ///
    /// This is a **mean**, while section 1's `p50`/histogram give the *distribution* at segment
    /// scale. Both are printed; a key whose mean and median disagree is one whose values differ a
    /// lot from each other, which the histogram shows and this cannot.
    fn sigma_by_scale(&self) -> Vec<(usize, f64)> {
        let Some(c_seg) = self.c_segment else {
            return Vec::new();
        };
        self.curve
            .iter()
            .enumerate()
            .filter_map(|(i, c)| c.filter(|c| *c > 0.0).map(|c| (i, c_seg / c)))
            .collect()
    }
}

// ── text output ─────────────────────────────────────────────────────────────────────────────

impl Report {
    fn print(&self, top: usize) {
        println!("imbh attribute statistics — {}", self.dir);
        let scopes: Vec<&str> = self.scopes.iter().map(|s| s.column()).collect();
        println!(
            "  scopes: {}   segments: {}   rows: {}",
            scopes.join(", "),
            self.global.segments,
            num(self.global.rows),
        );
        match self.last_minutes {
            Some(m) => println!("  segment window: segments overlapping the last {m} minutes"),
            None => println!("  segment window: all sealed segments"),
        }
        println!(
            "  caps: {} keys/unit, {} values/key, sampling engages beyond (see `sample` column)",
            num(self.max_keys as u64),
            num(self.max_values as u64),
        );
        if self.levels.is_empty() {
            println!("  window ladder: disabled (--windows none)");
        } else {
            let ladder: Vec<String> = self
                .levels
                .iter()
                .map(|l| {
                    format!(
                        "{} ({} window{})",
                        l.label,
                        l.windows,
                        if l.windows == 1 { "" } else { "s" }
                    )
                })
                .collect();
            println!(
                "  window ladder: segment < {} < all  — over {}",
                ladder.join(" < "),
                dur(self.global.span_nanos),
            );
        }
        if self.pending_wal_frames > 0 {
            println!(
                "  NOT MEASURED: {} unsealed WAL frame(s) — buffered rows are in no segment yet",
                self.pending_wal_frames
            );
        }
        for skipped in &self.segments_skipped {
            println!("  SEGMENT SKIPPED: {skipped}");
        }

        println!();
        println!("== 1. SEGMENT-PRUNING POTENTIAL (sigma) ==");
        println!(
            "  sigma(key,value) = fraction of this table's segments holding >=1 matching row; a"
        );
        println!(
            "  segment index prunes 1 - sigma. One sample per DISTINCT value, unweighted by how"
        );
        println!("  often it occurs. sigma ~ 1 => the index buys nothing for that value.");
        println!(
            "  postings = (key, value, segment) entries such an index would store for this key."
        );
        println!(
            "  hist = 10 sigma buckets [0,.1)..[.9,1]; '.' = empty, otherwise digits of count."
        );

        for table in &self.tables {
            println!();
            if table.segments == 0 {
                println!("  -- {} -- no segments in range", table.label);
                continue;
            }
            println!(
                "  -- {} -- {} segment{}, {} rows, {} keys{}",
                table.label,
                table.segments,
                if table.segments == 1 { "" } else { "s" },
                num(table.rows),
                table.keys_tracked,
                sampled_note(table.keys_sample_rate, table.keys_est),
            );
            if table.segments < 2 {
                println!("     (1 segment: sigma is 1.0 by construction and says nothing)");
            }
            println!(
                "     {:<34} {:>10} {:>11} {:>6} {:>6} {:>6} {:>6} {:>6}  {:<10} sample",
                "key", "distinct", "postings", "p50", "p90", "max", "mean", "<=.25", "hist",
            );
            for key in table.keys.iter().take(top) {
                let Some(sigma) = &key.sigma else { continue };
                println!(
                    "     {:<34} {:>10} {:>11} {:>6.3} {:>6.3} {:>6.3} {:>6.3} {:>6.2}  {:<10} {}",
                    clip(&key.name, 34),
                    est(key.distinct_est, key.values_sample_rate),
                    est(key.postings_est, key.values_sample_rate),
                    sigma.p50,
                    sigma.p90,
                    sigma.max,
                    sigma.mean,
                    sigma.frac_le_25,
                    histogram(&sigma.histogram),
                    rate_note(key.values_sample_rate),
                );
            }
            if table.keys.len() > top {
                println!(
                    "     ... {} more keys (raise --top)",
                    table.keys.len() - top
                );
            }
        }

        self.print_locality(top);

        println!();
        println!("== 3. PROMOTION CANDIDATES (record `attributes` scope, string values only) ==");
        println!(
            "  Scope matches `lookup_promoted` exactly: json_get(attributes, key) kept only when"
        );
        println!(
            "  AnyValue::Str. `resource`/`scope` keys are excluded here (a different scope) even"
        );
        println!("  when the sigma section above lists them.");
        println!(
            "  coverage = rows a promoted column would be non-NULL on / all rows in the database."
        );
        println!(
            "  rep = rows per (value, segment) posting; runs/row = how often the value changes"
        );
        println!("  from one row to the next (~1 = interleaved, low = arrives in runs).");
        println!(
            "  est B/row = [dictionary (C(seg) x len) + index (runs x log2(C(seg))/8)] / rows-per-seg"
        );
        println!(
            "  — the two terms a promoted column pays. Cardinality is NOT one of them, and neither"
        );
        println!(
            "  is repetition alone: the SAME session population measured 9,079 B contiguous and"
        );
        println!(
            "  64,252 B interleaved (archetype-bench). Estimates run 2-5x high because zstd beats"
        );
        println!("  the model — use them to RANK keys, not to size a budget.");
        println!("  column costs: Parquet dictionaries are per column chunk, so measured cost was");
        println!(
            "  +1,206 B/key at 3,125x repetition and +108,842 B/key at 1x (promote-cost bench)."
        );
        println!(
            "  promote / index@ are INDEPENDENT verdicts — a key can want both. index@ is the"
        );
        println!(
            "  widest query range over which pruning still pays (mean sigma <= 0.25); `-` means"
        );
        println!("  none of the ladder's rungs qualify.");
        println!();
        println!(
            "  {:<34} {:>9} {:>7} {:>10} {:>8} {:>7} {:>9}  {:<9} index@",
            "key", "coverage", "str%", "distinct", "rep", "runs/row", "est B/row", "promote",
        );
        let mut promo: Vec<&KeyReport> = self
            .global
            .keys
            .iter()
            .filter(|k| k.scope == AttrScope::Attributes)
            .collect();
        promo.sort_by(|a, b| b.rows_string.cmp(&a.rows_string).then(a.name.cmp(&b.name)));
        let total_rows = self.global.rows.max(1) as f64;
        for key in promo.iter().take(top) {
            println!(
                "  {:<34} {:>9.3} {:>7.2} {:>10} {:>8.1} {:>7.2} {:>9.2}  {:<9} {}",
                clip(&key.name, 34),
                key.rows_string as f64 / total_rows,
                key.rows_string as f64 / key.rows_present.max(1) as f64,
                est(key.distinct_est, key.values_sample_rate),
                key.repetition,
                key.runs as f64 / key.rows_present.max(1) as f64,
                key.est_bytes_per_row,
                promote_verdict(key, self.global.rows, self.global.rows_per_segment()),
                self.index_scale(&key.name).unwrap_or_else(|| "-".into()),
            );
        }
        if promo.len() > top {
            println!("  ... {} more keys (raise --top)", promo.len() - top);
        }

        let sampled: Vec<&str> = self
            .tables
            .iter()
            .flat_map(|t| t.keys.iter())
            .filter(|k| k.is_sampled())
            .map(|k| k.name.as_str())
            .collect();
        println!();
        println!("== WHAT WAS CAPPED ==");
        if sampled.is_empty() && self.tables.iter().all(|t| t.keys_sample_rate == 1.0) {
            println!("  Nothing. Every key and every distinct value was counted exactly.");
        } else {
            let mut names: Vec<&str> = sampled;
            names.sort_unstable();
            names.dedup();
            if !names.is_empty() {
                println!(
                    "  Value maps fell back to hash sampling for: {}. Their `distinct`/`postings`",
                    names.join(", ")
                );
                println!(
                    "  are estimates (tracked / sample-rate) and their sigma distribution is over an"
                );
                println!(
                    "  unbiased sample of values, not all of them. Raise --max-values to tighten."
                );
            }
            for table in &self.tables {
                if table.keys_sample_rate < 1.0 {
                    println!(
                        "  {}: key map sampled at {} — {} keys existed, {} kept. Raise --max-keys.",
                        table.label,
                        rate_note(table.keys_sample_rate),
                        est(table.keys_est, table.keys_sample_rate),
                        table.keys_tracked,
                    );
                }
            }
        }
        if self.pending_wal_frames > 0 {
            println!(
                "  {} unsealed WAL frame(s) were not measured (no segment to be selective within).",
                self.pending_wal_frames
            );
        }
    }

    /// Section 2: cardinality as a function of window width.
    ///
    /// Sigma (section 1) answers the question at exactly one scale — the segment. This answers it at
    /// several, because the same key can be localized against a day and interleaved against a
    /// minute, and which one governs depends on the range the user queries over. The reading is the
    /// *shape*, not any single column.
    fn print_locality(&self, top: usize) {
        println!();
        println!("== 2. CARDINALITY vs TIME SCALE (locality) ==");
        if self.levels.is_empty() {
            println!(
                "  Disabled (--windows none). Only the segment and whole-scan endpoints exist."
            );
            return;
        }
        println!(
            "  C(w) = mean distinct values of the key within one window of width w. C(seg) is"
        );
        println!(
            "  the innermost point (= postings/segments, the same number sigma summarises) and"
        );
        println!("  C(all) the outermost (= global distinct count). loc = C(all)/C(seg).");
        println!(
            "    loc ~ 1        interleaved: every segment already holds every value. Nothing"
        );
        println!("                   prunes at any scale; a promoted column is the only lever.");
        println!(
            "    loc >> 1       localized: values churn, and segment pruning removes 1 - 1/loc."
        );
        println!(
            "                   The width where the curve flattens is the horizon beyond which"
        );
        println!("                   pruning stops paying — read it off the C(w) columns.");
        println!("  rep = rows per (value, segment) posting: in-segment repetition, which is what");
        println!("  drives a promoted dictionary column's bytes on disk (not global cardinality).");

        for table in &self.tables {
            if table.segments == 0 {
                continue;
            }
            println!();
            println!(
                "  -- {} -- {} segment{} over {}",
                table.label,
                table.segments,
                if table.segments == 1 { "" } else { "s" },
                dur(table.span_nanos),
            );
            // A level that opened one window has collapsed onto C(all); one that opened about as
            // many windows as there are segments has collapsed onto C(seg). Either way it is not an
            // independent point on the curve, and silently printing it would invite reading a
            // coincidence as a finding.
            let degenerate: Vec<String> = self
                .levels
                .iter()
                .enumerate()
                .filter_map(|(i, level)| {
                    let w = table.windows[i];
                    match w {
                        0 => None,
                        1 => Some(format!("{} (1 window = C(all))", level.label)),
                        w if w >= table.segments => Some(format!(
                            "{} ({w} windows >= segments = C(seg))",
                            level.label
                        )),
                        _ => None,
                    }
                })
                .collect();
            if !degenerate.is_empty() {
                println!("     collapsed at this scale: {}", degenerate.join(", "));
            }
            let mut header = format!("     {:<34} {:>8} {:>9}", "key", "rep", "C(seg)");
            for level in &self.levels {
                header.push_str(&format!(" {:>9}", format!("C({})", level.label)));
            }
            header.push_str(&format!(" {:>9} {:>8}", "C(all)", "loc"));
            println!("{header}");

            // Most localized first: the keys a segment-granularity index would actually serve.
            let mut keys: Vec<&KeyReport> = table.keys.iter().collect();
            keys.sort_by(|a, b| {
                b.locality()
                    .unwrap_or(0.0)
                    .partial_cmp(&a.locality().unwrap_or(0.0))
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| a.name.cmp(&b.name))
            });
            for key in keys.iter().take(top) {
                let mut row = format!(
                    "     {:<34} {:>8.1} {:>9}",
                    clip(&key.name, 34),
                    key.repetition,
                    key.c_segment
                        .map(|c| est(c, key.values_sample_rate))
                        .unwrap_or_else(|| "-".into()),
                );
                for point in &key.curve {
                    row.push_str(&format!(
                        " {:>9}",
                        point
                            .map(|c| est(c, key.values_sample_rate))
                            .unwrap_or_else(|| "-".into())
                    ));
                }
                row.push_str(&format!(
                    " {:>9} {:>8}",
                    est(key.distinct_est, key.values_sample_rate),
                    key.locality()
                        .map(|l| format!("{l:.1}x"))
                        .unwrap_or_else(|| "-".into()),
                ));
                println!("{row}");
            }
            if table.keys.len() > top {
                println!(
                    "     ... {} more keys (raise --top)",
                    table.keys.len() - top
                );
            }
        }
    }

    /// **The widest query range over which a segment index would still prune for `name`.**
    ///
    /// A segment index prunes `1 - sigma` of the segments it is asked about, and sigma depends on
    /// the range the query covers: a value can occupy a small fraction of a day's segments and all
    /// of the segments in the minute it appeared. So the useful verdict is not "index: yes/no" but
    /// *up to what width*. This walks the ladder outwards and returns the widest rung whose mean
    /// sigma is still at or below [`INDEX_MAX_SIGMA`], with `"all"` when even the whole scan
    /// qualifies and `None` when no rung does.
    ///
    /// Taken across tables: the widest scale any table carrying the key achieves, matching
    /// `best_p50`'s "best case wins" convention. Tables with one segment are skipped, where sigma is
    /// 1.0 by construction and says nothing.
    fn index_scale(&self, name: &str) -> Option<String> {
        let mut best: Option<usize> = None;
        let mut all = false;
        for table in self.tables.iter().filter(|t| t.segments >= 2) {
            let Some(key) = table.keys.iter().find(|k| k.name == name) else {
                continue;
            };
            if key.locality().is_some_and(|l| 1.0 / l <= INDEX_MAX_SIGMA) {
                all = true;
            }
            for (level, sigma) in key.sigma_by_scale() {
                if sigma <= INDEX_MAX_SIGMA && best.is_none_or(|b| level > b) {
                    best = Some(level);
                }
            }
        }
        if all {
            return Some("all".to_owned());
        }
        best.map(|level| self.levels[level].label.clone())
    }

    /// Lowest median sigma for `name` across the tables that carry it and have >= 2 segments.
    fn best_p50(&self, name: &str) -> Option<f64> {
        self.tables
            .iter()
            .filter(|t| t.segments >= 2)
            .filter_map(|t| t.keys.iter().find(|k| k.name == name))
            .filter_map(|k| k.sigma.as_ref().map(|s| s.p50))
            .min_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
    }

    fn to_json(&self) -> serde_json::Value {
        let unit = |u: &UnitReport| {
            serde_json::json!({
                "label": u.label,
                "segments": u.segments,
                "rows": u.rows,
                "keys_tracked": u.keys_tracked,
                "keys_sample_rate": u.keys_sample_rate,
                "keys_estimated": u.keys_est,
                "span_nanos": u.span_nanos,
                "windows_per_level": self.levels.iter().enumerate()
                    .map(|(i, l)| serde_json::json!({ "window": l.label, "windows": u.windows[i] }))
                    .collect::<Vec<_>>(),
                "keys": u.keys.iter().map(|k| serde_json::json!({
                    "key": k.name,
                    "scope": k.scope.column(),
                    "rows_present": k.rows_present,
                    "rows_string": k.rows_string,
                    "str_len_avg": k.str_len_avg,
                    "str_len_max": k.str_len_max,
                    "segments_present": k.segments_present,
                    "values_tracked": k.values_tracked,
                    "values_sample_rate": k.values_sample_rate,
                    "distinct_values_estimated": k.distinct_est,
                    "index_postings_estimated": k.postings_est,
                    "in_segment_repetition": k.repetition,
                    "runs": k.runs,
                    "estimated_bytes_per_row": k.est_bytes_per_row,
                    "locality": k.locality(),
                    // The full C(w) curve, innermost (one segment) to outermost (the whole scan).
                    "cardinality_curve": std::iter::once(serde_json::json!({
                            "window": "segment", "distinct_values": k.c_segment }))
                        .chain(self.levels.iter().zip(&k.curve).map(|(l, c)| serde_json::json!({
                            "window": l.label,
                            "window_nanos": l.width_nanos,
                            "distinct_values": c })))
                        .chain(std::iter::once(serde_json::json!({
                            "window": "all", "distinct_values": k.distinct_est })))
                        .collect::<Vec<_>>(),
                    "sigma": k.sigma.as_ref().map(|s| serde_json::json!({
                        "p50": s.p50,
                        "p90": s.p90,
                        "max": s.max,
                        "mean": s.mean,
                        "frac_le_0_25": s.frac_le_25,
                        "histogram": s.histogram,
                        "values": s.count,
                    })),
                })).collect::<Vec<_>>(),
            })
        };
        let promotion: Vec<serde_json::Value> = self
            .global
            .keys
            .iter()
            .filter(|k| k.scope == AttrScope::Attributes)
            .map(|k| {
                let best = self.best_p50(&k.name);
                serde_json::json!({
                    "key": k.name,
                    "rows_string": k.rows_string,
                    "coverage": k.rows_string as f64 / self.global.rows.max(1) as f64,
                    "string_fraction": k.rows_string as f64 / k.rows_present.max(1) as f64,
                    "distinct_values_estimated": k.distinct_est,
                    "value_len_avg": k.str_len_avg,
                    "value_len_max": k.str_len_max,
                    "best_sigma_p50": best,
                    "promote_hint": promote_verdict(k, self.global.rows, self.global.rows_per_segment()),
                    "index_scale": self.index_scale(&k.name),
                    "in_segment_repetition": k.repetition,
                })
            })
            .collect();
        serde_json::json!({
            "dir": self.dir,
            "scopes": self.scopes.iter().map(|s| s.column()).collect::<Vec<_>>(),
            "caps": { "max_keys": self.max_keys, "max_values": self.max_values },
            "window_levels": self.levels.iter().map(|l| serde_json::json!({
                "window": l.label,
                "window_nanos": l.width_nanos,
                "windows": l.windows,
            })).collect::<Vec<_>>(),
            "last_minutes": self.last_minutes,
            "unsealed_wal_frames": self.pending_wal_frames,
            "segments_skipped": self.segments_skipped,
            "tables": self.tables.iter().map(unit).collect::<Vec<_>>(),
            "all_tables": unit(&self.global),
            "promotion_candidates": promotion,
        })
    }
}

// ── formatting helpers ──────────────────────────────────────────────────────────────────────

fn num(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// A count that may have been scaled up from a sample. Printed bare when the underlying map was
/// exact, and prefixed `~` the moment sampling engaged — the `~` is the only thing standing between
/// a reader and mistaking an extrapolation for a measurement, so it is driven by the sample rate,
/// never by whether the number happens to be whole (a scaled count always is).
fn est(x: f64, sample_rate: f64) -> String {
    let n = num(x.round().max(0.0) as u64);
    if sample_rate >= 1.0 {
        n
    } else {
        format!("~{n}")
    }
}

/// A nanosecond span as the coarsest unit that keeps it readable — this is a header line, not a
/// measurement, so one decimal is plenty.
fn dur(nanos: i64) -> String {
    let secs = nanos as f64 / NANOS_PER_SEC as f64;
    if secs < 1.0 {
        format!("{:.0}ms", secs * 1e3)
    } else if secs < 90.0 {
        format!("{secs:.0}s")
    } else if secs < 5_400.0 {
        format!("{:.1}m", secs / 60.0)
    } else if secs < 172_800.0 {
        format!("{:.1}h", secs / 3_600.0)
    } else {
        format!("{:.1}d", secs / 86_400.0)
    }
}

/// The sample rate as `1/N`. Bottom-k thresholds are not powers of two, so `N` is generally not a
/// whole number and is printed with a decimal unless it lands on one — rounding `1/3.65` to `1/4`
/// would overstate the coverage the estimates were scaled by.
fn rate_note(rate: f64) -> String {
    if rate >= 1.0 {
        return "exact".to_owned();
    }
    let n = 1.0 / rate;
    if (n - n.round()).abs() < 0.05 {
        format!("1/{}", n.round() as u64)
    } else {
        format!("1/{n:.1}")
    }
}

fn sampled_note(rate: f64, estimated: f64) -> String {
    if rate >= 1.0 {
        String::new()
    } else {
        format!(
            " (key map sampled {}, {} existed)",
            rate_note(rate),
            est(estimated, rate)
        )
    }
}

/// One character per sigma bucket: `.` for empty, otherwise the number of digits in the count
/// (`1` = 1-9 values, `2` = 10-99, ...). Compact enough to sit in a table column.
fn histogram(buckets: &[u32; 10]) -> String {
    buckets
        .iter()
        .map(|&c| {
            if c == 0 {
                '.'
            } else {
                char::from_digit((c.ilog10() + 1).min(9), 10).unwrap_or('9')
            }
        })
        .collect()
}

fn clip(s: &str, width: usize) -> String {
    if s.chars().count() <= width {
        s.to_owned()
    } else {
        let keep: String = s.chars().take(width.saturating_sub(1)).collect();
        format!("{keep}>")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use imbh_core::{
        AnyValue, Compression, LogRow, MemoryBudget, Retention, WalMode, canonical_json_object,
    };
    use imbh_storage::Storage;

    fn attrs(pairs: &[(&str, &str)]) -> String {
        let owned: Vec<(String, AnyValue)> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), AnyValue::Str((*v).to_owned())))
            .collect();
        canonical_json_object(&owned)
    }

    fn log_row(time: i64, attributes: String, resource: String) -> LogRow {
        LogRow {
            time_unix_nano: time,
            observed_time_unix_nano: None,
            service: Some("cart".to_owned()),
            severity_number: 9,
            severity_text: None,
            body: "hello".to_owned(),
            attributes,
            resource,
            scope: "{}".to_owned(),
            trace_id: None,
            span_id: None,
            flags: 0,
        }
    }

    /// End-to-end over a real, hand-built four-segment database: the tool must walk the manifest,
    /// read the Parquet attribute columns (plain `Utf8`) and the dictionary-encoded `resource`
    /// column, and land on the sigma values the fixture was built to have.
    ///
    /// By construction, across 4 sealed `logs` segments:
    /// - `env=prod`            in all 4 -> sigma 1.00
    /// - `pod=pod-<i>`         in 1 each -> sigma 0.25
    /// - `resource:host.name`  2 distinct values, each in 2 segments -> sigma 0.50
    #[test]
    fn measures_a_four_segment_database_end_to_end() {
        let dir = tempfile::tempdir().expect("tempdir");
        {
            let storage = Storage::open(
                dir.path(),
                Compression::default(),
                WalMode::Off,
                Retention::none(),
                MemoryBudget::default(),
            )
            .expect("open storage");
            for seg in 0..4i64 {
                let host = format!("host-{}", seg % 2);
                let resource = attrs(&[("host.name", &host)]);
                let rows: Vec<LogRow> = (0..5)
                    .map(|r| {
                        log_row(
                            seg * 1_000 + r,
                            attrs(&[("env", "prod"), ("pod", &format!("pod-{seg}"))]),
                            resource.clone(),
                        )
                    })
                    .collect();
                storage.append_logs(rows);
                storage.seal().expect("seal");
            }
        }

        let cfg = Config::for_dir(dir.path().to_path_buf());
        let report = analyze(&cfg).expect("analyze");
        let logs = report
            .tables
            .iter()
            .find(|t| t.label == "logs")
            .expect("logs unit");
        assert_eq!(logs.segments, 4, "one segment per seal");
        assert_eq!(logs.rows, 20);

        let key = |name: &str| {
            logs.keys
                .iter()
                .find(|k| k.name == name)
                .unwrap_or_else(|| panic!("{name} missing from the report"))
        };

        let pod = key("pod");
        let sigma = pod.sigma.as_ref().expect("pod sigma");
        assert_eq!(pod.distinct_est, 4.0, "one pod name per segment");
        assert_eq!(sigma.max, 0.25, "a value in 1 of 4 segments has sigma 0.25");
        assert_eq!(sigma.p50, 0.25);
        assert_eq!(sigma.mean, 0.25);
        assert_eq!(pod.postings_est, 4.0, "4 values x 1 segment each");
        assert_eq!(pod.rows_string, 20);

        let env = key("env");
        assert_eq!(env.distinct_est, 1.0);
        assert_eq!(env.sigma.as_ref().expect("env sigma").max, 1.0);
        assert_eq!(env.postings_est, 4.0, "1 value present in all 4 segments");

        // The dictionary-encoded `resource` column must be read, and its keys prefixed.
        let host = key("resource:host.name");
        assert_eq!(host.scope, AttrScope::Resource);
        assert_eq!(host.distinct_est, 2.0);
        assert_eq!(host.sigma.as_ref().expect("host sigma").mean, 0.5);
        assert_eq!(host.rows_string, 20);

        // Promotion is DB-wide and record-`attributes`-scoped: `host.name` lives in `resource`, so
        // it must NOT appear as a promotion candidate under its bare name.
        let promo: Vec<&str> = report
            .global
            .keys
            .iter()
            .filter(|k| k.scope == AttrScope::Attributes)
            .map(|k| k.name.as_str())
            .collect();
        assert!(promo.contains(&"env"));
        assert!(promo.contains(&"pod"));
        assert!(!promo.iter().any(|k| k.contains("host.name")));

        // 5 rows per segment cannot exhibit the repetition the cheap verdict needs, so the
        // classifier declines to judge rather than blaming the key for the corpus.
        let rps = report.global.rows_per_segment();
        assert_eq!(rps, 5.0);
        assert_eq!(promote_verdict(key("env"), report.global.rows, rps), "-");
        // `env=prod` is in all 4 segments, so no index scale prunes it.
        assert_eq!(report.index_scale("env"), None);
        assert_eq!(report.pending_wal_frames, 0);
        assert!(report.segments_skipped.is_empty());
        // The JSON view must be buildable and carry the same numbers.
        let json = report.to_json();
        assert_eq!(json["tables"][0]["label"], "logs");
    }

    /// The window ladder end-to-end, through the manifest and real Parquet files rather than the
    /// accumulator's own API.
    ///
    /// 8 sealed `logs` segments, one every 30s. `env=prod` is in all of them; `pod` takes a fresh
    /// value in each. Against a 60s / 120s ladder that pins every point of both curves:
    ///
    /// | key | C(seg) | C(60s) | C(120s) | C(all) | loc |
    /// |-----|--------|--------|---------|--------|-----|
    /// | env | 1      | 1      | 1       | 1      | 1.0 |
    /// | pod | 1      | 2      | 4       | 8      | 8.0 |
    ///
    /// `env` and `pod` have the *same* per-segment cardinality, so C(seg) alone cannot tell them
    /// apart — which is the whole reason the ladder exists.
    #[test]
    fn the_cardinality_curve_separates_a_flat_key_from_a_localized_one() {
        const SEC: i64 = 1_000_000_000;
        let dir = tempfile::tempdir().expect("tempdir");
        {
            let storage = Storage::open(
                dir.path(),
                Compression::default(),
                WalMode::Off,
                Retention::none(),
                MemoryBudget::default(),
            )
            .expect("open storage");
            for seg in 0..8i64 {
                let rows: Vec<LogRow> = (0..5)
                    .map(|r| {
                        log_row(
                            seg * 30 * SEC + r,
                            attrs(&[("env", "prod"), ("pod", &format!("pod-{seg}"))]),
                            attrs(&[("host.name", "host-a")]),
                        )
                    })
                    .collect();
                storage.append_logs(rows);
                storage.seal().expect("seal");
            }
        }

        let mut cfg = Config::for_dir(dir.path().to_path_buf());
        cfg.scopes = vec![AttrScope::Attributes];
        cfg.windows = vec![("60s".to_owned(), 60 * SEC), ("120s".to_owned(), 120 * SEC)];
        let report = analyze(&cfg).expect("analyze");
        let logs = report.tables.iter().find(|t| t.label == "logs").unwrap();
        assert_eq!(logs.segments, 8);
        assert_eq!(logs.windows, vec![4, 2], "240s span = 4 x 60s = 2 x 120s");
        assert_eq!(logs.span_nanos, 7 * 30 * SEC + 4);

        let key = |name: &str| logs.keys.iter().find(|k| k.name == name).expect(name);
        let env = key("env");
        assert_eq!(env.c_segment, Some(1.0));
        assert_eq!(env.curve, vec![Some(1.0), Some(1.0)]);
        assert_eq!(env.distinct_est, 1.0);
        assert_eq!(env.locality(), Some(1.0), "flat: nothing prunes");

        let pod = key("pod");
        assert_eq!(pod.c_segment, Some(1.0), "same per-segment count as env");
        assert_eq!(
            pod.curve,
            vec![Some(2.0), Some(4.0)],
            "grows with the window"
        );
        assert_eq!(pod.distinct_est, 8.0);
        assert_eq!(pod.locality(), Some(8.0), "localized: pruning removes 7/8");

        // 5 rows per segment, one posting per segment, so repetition is 5 for both keys.
        assert_eq!(pod.repetition, 5.0);
        assert_eq!(env.repetition, 5.0);

        // The curve must survive into the JSON view, innermost to outermost, with both endpoints.
        let json = report.to_json();
        let curve = &json["tables"][0]["keys"]
            .as_array()
            .unwrap()
            .iter()
            .find(|k| k["key"] == "pod")
            .expect("pod in json")["cardinality_curve"];
        let windows: Vec<&str> = curve
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["window"].as_str().unwrap())
            .collect();
        assert_eq!(windows, vec!["segment", "60s", "120s", "all"]);
        assert_eq!(curve[3]["distinct_values"], 8.0);

        // The report renders without panicking, ladder on and off.
        report.print(5);
        cfg.windows.clear();
        analyze(&cfg).expect("analyze without a ladder").print(5);
    }

    /// `--windows` parsing: the units, and the two ways a ladder can be malformed.
    #[test]
    fn window_specs_are_parsed_and_validated() {
        assert_eq!(parse_duration("30s").unwrap(), 30 * NANOS_PER_SEC);
        assert_eq!(parse_duration("5m").unwrap(), 300 * NANOS_PER_SEC);
        assert_eq!(parse_duration("2h").unwrap(), 7_200 * NANOS_PER_SEC);
        assert_eq!(parse_duration("7d").unwrap(), 604_800 * NANOS_PER_SEC);
        assert_eq!(parse_duration("90").unwrap(), 90 * NANOS_PER_SEC);
        assert!(
            parse_duration("0s").is_err(),
            "a zero window has no meaning"
        );
        assert!(parse_duration("-1m").is_err());
        assert!(parse_duration("later").is_err());
        // The default ladder is what the usage text advertises.
        let cfg = Config::for_dir(PathBuf::new());
        let labels: Vec<&str> = cfg.windows.iter().map(|(l, _)| l.as_str()).collect();
        assert_eq!(labels, vec!["1m", "1h", "24h"]);
        assert_eq!(
            cfg.widths(),
            [60, 3_600, 86_400]
                .iter()
                .map(|s| s * NANOS_PER_SEC)
                .collect::<Vec<_>>()
        );
    }

    /// **The promotion verdict gates on in-segment repetition, not on global cardinality** — the
    /// correction the `promote-cost` bench forced, pinned as behaviour.
    ///
    /// Three keys over 6 segments x 200 rows, chosen so that cardinality and cost disagree:
    ///
    /// | key      | distinct | postings | repetition | verdict  |
    /// |----------|----------|----------|------------|----------|
    /// | `env`    | 1        | 6        | 200        | `yes`    |
    /// | `pod`    | 6        | 6        | 200        | `yes`    |
    /// | `req_id` | 1,200    | 1,200    | 1          | `costly` |
    ///
    /// `env` and `pod` differ 6x in global cardinality and cost **exactly the same** — one value on
    /// 200 rows within any given segment, either way. `pod` is the case that matters: its values
    /// never recur across segments, so a global-distinct gate scaled to production (`pod.name` over
    /// weeks) would reject it, yet it is precisely the cheap case. `req_id` is expensive not because
    /// its cardinality is high but because its values never repeat *within* a segment either.
    ///
    /// The index verdict runs the other way: `pod` is confined to one segment in six, so pruning
    /// pays; `env` is everywhere, so it never does.
    #[test]
    fn the_promotion_verdict_follows_repetition_not_cardinality() {
        let dir = tempfile::tempdir().expect("tempdir");
        {
            let storage = Storage::open(
                dir.path(),
                Compression::default(),
                WalMode::Off,
                Retention::none(),
                MemoryBudget::default(),
            )
            .expect("open storage");
            for seg in 0..6i64 {
                let rows: Vec<LogRow> = (0..200)
                    .map(|r| {
                        log_row(
                            seg * 10_000 + r,
                            attrs(&[
                                ("env", "prod"),
                                ("pod", &format!("pod-{seg}")),
                                ("req_id", &format!("r-{seg}-{r}")),
                            ]),
                            attrs(&[("host.name", "host-a")]),
                        )
                    })
                    .collect();
                storage.append_logs(rows);
                storage.seal().expect("seal");
            }
        }
        let mut cfg = Config::for_dir(dir.path().to_path_buf());
        cfg.scopes = vec![AttrScope::Attributes];
        let report = analyze(&cfg).expect("analyze");
        let rps = report.global.rows_per_segment();
        assert_eq!(rps, 200.0);
        let key = |name: &str| {
            report
                .global
                .keys
                .iter()
                .find(|k| k.name == name)
                .expect(name)
        };

        assert_eq!(key("env").distinct_est, 1.0);
        assert_eq!(key("pod").distinct_est, 6.0);
        assert_eq!(key("req_id").distinct_est, 1_200.0);

        // 1,200 rows over 6 postings either way: `env` is one value in six segments, `pod` is six
        // values in one segment each. Identical cost, 6x apart in cardinality.
        assert_eq!(key("env").postings_est, 6.0);
        assert_eq!(key("pod").postings_est, 6.0);
        assert_eq!(key("env").repetition, 200.0);
        assert_eq!(key("pod").repetition, 200.0);
        assert_eq!(key("req_id").repetition, 1.0);

        assert_eq!(promote_verdict(key("env"), report.global.rows, rps), "yes");
        assert_eq!(
            promote_verdict(key("pod"), report.global.rows, rps),
            "yes",
            "a fresh value per segment is the CHEAP case, whatever its global cardinality"
        );
        assert_eq!(
            promote_verdict(key("req_id"), report.global.rows, rps),
            "costly",
            "unique per row is the +108 KB/key regime"
        );

        // The index verdict is independent and points the other way for `pod`.
        assert_eq!(
            report.index_scale("env"),
            None,
            "in every segment: no pruning"
        );
        assert_eq!(
            report.index_scale("pod").as_deref(),
            Some("all"),
            "one segment in six: sigma 0.167, pruning pays at every scale"
        );
    }

    /// `--scope attributes` must restrict the scan to the one column `promote` covers.
    #[test]
    fn scope_flag_restricts_to_record_attributes() {
        let dir = tempfile::tempdir().expect("tempdir");
        {
            let storage = Storage::open(
                dir.path(),
                Compression::default(),
                WalMode::Off,
                Retention::none(),
                MemoryBudget::default(),
            )
            .expect("open storage");
            storage.append_logs(vec![log_row(
                1,
                attrs(&[("env", "prod")]),
                attrs(&[("host.name", "host-a")]),
            )]);
            storage.seal().expect("seal");
        }
        let mut cfg = Config::for_dir(dir.path().to_path_buf());
        cfg.scopes = vec![AttrScope::Attributes];
        let report = analyze(&cfg).expect("analyze");
        let logs = report.tables.iter().find(|t| t.label == "logs").unwrap();
        let names: Vec<&str> = logs.keys.iter().map(|k| k.name.as_str()).collect();
        assert_eq!(names, vec!["env"]);
    }

    #[test]
    fn formatting_helpers() {
        assert_eq!(num(0), "0");
        assert_eq!(num(1_234_567), "1,234,567");
        assert_eq!(est(4.0, 1.0), "4");
        assert_eq!(est(4096.0, 0.25), "~4,096");
        assert_eq!(rate_note(1.0), "exact");
        assert_eq!(rate_note(0.25), "1/4");
        // A bottom-k threshold rarely lands on a power of two; it must not be rounded to one.
        assert_eq!(rate_note(0.2736), "1/3.7");
        assert_eq!(histogram(&[0, 1, 9, 10, 0, 0, 0, 0, 0, 1234]), ".112.....4");
        assert_eq!(clip("short", 10), "short");
        assert_eq!(clip("a-very-long-key-name", 6), "a-ver>");
    }
}

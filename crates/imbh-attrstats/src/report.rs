//! The measurement's result: one row per attribute key, per table and DB-wide, plus the two
//! verdicts derived from it.
//!
//! Everything here is plain data with `f64` fields, so a report crosses a process boundary intact
//! (the head API sends one over HTTP) and a consumer can rank, filter, or re-render it without
//! re-deriving anything. [`crate::text`] is one such consumer, not the model's own opinion.

use crate::accum::{Acc, AttrScope, SigmaSummary, summarize};

/// One attribute key's measurement within one scan unit.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct KeyReport {
    pub name: String,
    pub scope: AttrScope,
    pub rows_present: u64,
    pub rows_string: u64,
    pub str_len_avg: f64,
    pub str_len_max: u32,
    pub segments_present: u32,
    /// Distinct values actually held in memory.
    pub values_tracked: usize,
    /// `1.0` while the value map was exact; below it once the cap forced hash sampling — the flag
    /// that makes `distinct_est`/`postings_est` estimates rather than counts.
    pub values_sample_rate: f64,
    /// `values_tracked / values_sample_rate`: the estimated true distinct-value count, which is also
    /// the distinct `(key, value)` pair count for this key.
    pub distinct_est: f64,
    /// `(key, value, segment)` entries a segment index would hold for this key — the direct size
    /// bound, and equal to `distinct * mean_sigma * segments`.
    pub postings_est: f64,
    pub sigma: Option<SigmaSummary>,
    /// `C(w)`: mean distinct values of this key within one window, one entry per configured level,
    /// innermost first. `None` for a level that opened no window.
    pub curve: Vec<Option<f64>>,
    /// `C(segment)` — the innermost point of the same curve, and `postings / segments`.
    pub c_segment: Option<f64>,
    /// Mean rows per `(value, segment)` posting: **in-segment repetition**.
    pub repetition: f64,
    /// Times this key's value differed from the previous row's, over the whole scan.
    pub runs: u64,
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
    pub est_bytes_per_row: f64,
}

impl KeyReport {
    pub fn is_sampled(&self) -> bool {
        self.values_sample_rate < 1.0
    }

    /// `C(all) / C(segment)`: how much bigger the key's value space is over the whole scan than
    /// within one segment. ~1 means interleaved (every segment already holds every value, so nothing
    /// prunes); large means localized (values churn, and segment pruning removes `1 - 1/locality`).
    pub fn locality(&self) -> Option<f64> {
        let c_seg = self.c_segment?;
        (c_seg > 0.0).then(|| self.distinct_est / c_seg)
    }

    /// Mean sigma at each rung of the window ladder: the fraction of a window's segments that an
    /// average value of this key occupies.
    ///
    /// `sigma(w) = C(seg) / C(w)`, which falls out of the definitions — a window of width `w` holds
    /// `C(w)` distinct values across `segments/windows` segments, and `C(seg) = sigma(w) * C(w)`.
    /// The outermost rung is `C(seg)/C(all) = 1/locality`, and the innermost is 1 by construction
    /// (within a single segment, a value that is present occupies all of it).
    ///
    /// This is a **mean**, while [`KeyReport::sigma`] gives the *distribution* at segment scale.
    /// Both are reported; a key whose mean and median disagree is one whose values differ a lot from
    /// each other, which the histogram shows and this cannot.
    pub fn sigma_by_scale(&self) -> Vec<(usize, f64)> {
        let Some(c_seg) = self.c_segment else {
            return Vec::new();
        };
        self.curve
            .iter()
            .enumerate()
            .filter_map(|(i, c)| c.filter(|c| *c > 0.0).map(|c| (i, c_seg / c)))
            .collect()
    }

    /// Rows a promoted column would be non-NULL on, as a share of every row in the scan unit.
    pub fn coverage(&self, total_rows: u64) -> f64 {
        self.rows_string as f64 / total_rows.max(1) as f64
    }

    /// Share of the key's occurrences whose value is a string — the only ones `lookup_promoted`
    /// would put in a column.
    pub fn string_fraction(&self) -> f64 {
        self.rows_string as f64 / self.rows_present.max(1) as f64
    }

    /// How often the value changes from one row to the next: ~1 is interleaved, low means it arrives
    /// in runs. The term the dictionary size alone cannot see.
    pub fn runs_per_row(&self) -> f64 {
        self.runs as f64 / self.rows_present.max(1) as f64
    }
}

/// One rung of the ladder, as measured.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct LevelReport {
    pub label: String,
    pub width_nanos: i64,
    /// Distinct windows this width opened over the scan. `1` means the width covers everything (the
    /// level has collapsed onto `C(all)`); a value near the segment count means it has collapsed onto
    /// `C(segment)`. Either way the level says nothing new, and the report flags it.
    pub windows: u32,
}

/// One scan unit — a single table, or the whole database.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct UnitReport {
    pub label: String,
    pub segments: u32,
    pub rows: u64,
    pub keys_tracked: usize,
    pub keys_sample_rate: f64,
    pub keys_est: f64,
    /// Windows opened per level within *this* unit — the `C(w)` denominator, which differs per table.
    pub windows: Vec<u32>,
    pub span_nanos: i64,
    /// Keys, most expensive segment index first (descending `postings_est`).
    pub keys: Vec<KeyReport>,
}

impl UnitReport {
    /// Mean rows per segment — the ceiling on how much any value can repeat within one.
    pub fn rows_per_segment(&self) -> f64 {
        if self.segments == 0 {
            0.0
        } else {
            self.rows as f64 / f64::from(self.segments)
        }
    }

    pub fn key(&self, name: &str) -> Option<&KeyReport> {
        self.keys.iter().find(|k| k.name == name)
    }
}

/// A whole measurement: every table, the DB-wide roll-up, and what the run could not cover.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Report {
    /// The database directory the measurement ran over, as the measuring process saw it. A remote
    /// head shows the *daemon's* path, which is the honest answer to "what was measured".
    pub dir: String,
    pub scopes: Vec<AttrScope>,
    pub max_keys: usize,
    pub max_values: usize,
    /// The absolute `[start_ns, end_ns]` segment window, when the scan was restricted to one.
    pub range: Option<(i64, i64)>,
    pub levels: Vec<LevelReport>,
    /// Unsealed WAL frames at the time of the scan: rows in no segment yet, and so not measured.
    pub pending_wal_frames: usize,
    /// Segments that could not be read (retention or compaction removed them mid-scan), with the
    /// reason. A skipped segment under-counts rather than aborting the run, so it must be visible.
    pub segments_skipped: Vec<String>,
    pub tables: Vec<UnitReport>,
    pub global: UnitReport,
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
pub const PROMOTE_MAX_EST_BYTES_PER_ROW: f64 = 0.5;
/// Estimated bytes per row above which the column is firmly in the expensive regime.
pub const PROMOTE_POOR_EST_BYTES_PER_ROW: f64 = 2.0;
/// Rows per segment below which nothing about repetition or run structure can be resolved — a
/// segment shorter than this cannot exhibit the recurrence a cheap verdict requires, so reporting
/// "expensive" would describe the corpus rather than the key.
pub const PROMOTE_MIN_ROWS_PER_SEGMENT: f64 = 50.0;
/// Fraction of a key's values that must be strings for a promoted column to be worth anything —
/// `lookup_promoted` leaves every non-string cell NULL.
pub const PROMOTE_MIN_STRING_FRACTION: f64 = 0.9;
/// Share of all rows a promoted column must be non-NULL on to count as "widely present".
pub const PROMOTE_MIN_COVERAGE: f64 = 0.01;
/// Sigma at or below which segment pruning saves most of the scan.
pub const INDEX_MAX_SIGMA: f64 = 0.25;
/// The `index_scale` answer meaning "every rung of the ladder qualifies, and so does the whole scan":
/// pruning pays at any query width, so there is no horizon to name.
pub const ALL_SCALES: &str = "all";

/// Would a promoted column pay for this key, and what would it cost?
///
/// This is one of **two independent verdicts**, not a branch of one. The two mechanisms answer
/// different questions and a key can want both: on the `prune-bench` corpus `shard` has 60 distinct
/// values *and* sigma 0.017, so it wants a promoted column (fast filtering) and a segment index
/// (pruning). The predecessor was an if/else chain and could only ever say one, which is why it is
/// now split.
///
/// - `yes` — string-valued, repeats enough within a segment to be cheap, and present on enough rows.
/// - `yes?` — cheap and string-valued but rare (see [`KeyReport::coverage`]): the column is mostly
///   NULL, which costs little but only pays if the key is actually queried.
/// - `costly` — the column would be near-unique per row, the +108 KB/key regime.
/// - `no` — too few string values for the column to populate at all.
/// - `-` — no rows, or segments too small for the dictionary fraction to mean anything.
pub fn promote_verdict(key: &KeyReport, total_rows: u64, rows_per_segment: f64) -> &'static str {
    if key.rows_present == 0 {
        return "-";
    }
    if key.string_fraction() < PROMOTE_MIN_STRING_FRACTION {
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
    if key.coverage(total_rows) >= PROMOTE_MIN_COVERAGE {
        "yes"
    } else {
        "yes?"
    }
}

impl Report {
    /// The promotion candidates, DB-wide and record-`attributes`-scoped — exactly the keys a
    /// `promote = [...]` list may name — ordered by how many rows a column would actually populate.
    pub fn promotion_candidates(&self) -> Vec<&KeyReport> {
        let mut keys: Vec<&KeyReport> = self
            .global
            .keys
            .iter()
            .filter(|k| k.scope == AttrScope::Attributes)
            .collect();
        keys.sort_by(|a, b| b.rows_string.cmp(&a.rows_string).then(a.name.cmp(&b.name)));
        keys
    }

    /// [`promote_verdict`] against the DB-wide unit, which is the scope `promote` applies at.
    pub fn promote_verdict(&self, key: &KeyReport) -> &'static str {
        promote_verdict(key, self.global.rows, self.global.rows_per_segment())
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
    /// [`Report::best_p50`]'s "best case wins" convention. Tables with one segment are skipped, where
    /// sigma is 1.0 by construction and says nothing.
    pub fn index_scale(&self, name: &str) -> Option<String> {
        let mut best: Option<usize> = None;
        for table in self.tables.iter().filter(|t| t.segments >= 2) {
            let Some(label) = self.index_scale_in(table, name) else {
                continue;
            };
            if label == ALL_SCALES {
                return Some(ALL_SCALES.to_owned());
            }
            if let Some(level) = self.levels.iter().position(|l| l.label == label)
                && best.is_none_or(|b| level > b)
            {
                best = Some(level);
            }
        }
        best.map(|level| self.levels[level].label.clone())
    }

    /// [`Report::index_scale`] within **one** table.
    ///
    /// This is the scale that is actually defined: sigma's denominator is a table's segment count, so
    /// a per-table answer is the primary one and [`Report::index_scale`] is the roll-up over it. A
    /// table with fewer than two segments answers `None` — sigma is 1.0 there by construction and says
    /// nothing about pruning.
    pub fn index_scale_in(&self, unit: &UnitReport, name: &str) -> Option<String> {
        if unit.segments < 2 {
            return None;
        }
        let key = unit.key(name)?;
        if key.locality().is_some_and(|l| 1.0 / l <= INDEX_MAX_SIGMA) {
            return Some(ALL_SCALES.to_owned());
        }
        key.sigma_by_scale()
            .into_iter()
            .filter(|(_, sigma)| *sigma <= INDEX_MAX_SIGMA)
            .map(|(level, _)| level)
            .max()
            .map(|level| self.levels[level].label.clone())
    }

    /// Lowest median sigma for `name` across the tables that carry it and have >= 2 segments.
    pub fn best_p50(&self, name: &str) -> Option<f64> {
        self.tables
            .iter()
            .filter(|t| t.segments >= 2)
            .filter_map(|t| t.key(name))
            .filter_map(|k| k.sigma.as_ref().map(|s| s.p50))
            .min_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
    }

    pub fn table(&self, label: &str) -> Option<&UnitReport> {
        self.tables.iter().find(|t| t.label == label)
    }

    /// Keys whose value map fell back to hash sampling, deduplicated and sorted. Their
    /// `distinct`/`postings` are estimates rather than counts, which a reader must be told.
    pub fn sampled_keys(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self
            .tables
            .iter()
            .flat_map(|t| t.keys.iter())
            .filter(|k| k.is_sampled())
            .map(|k| k.name.as_str())
            .collect();
        names.sort_unstable();
        names.dedup();
        names
    }

    /// Whether every key and every distinct value was counted exactly.
    pub fn is_exact(&self) -> bool {
        self.sampled_keys().is_empty() && self.tables.iter().all(|t| t.keys_sample_rate == 1.0)
    }
}

/// Reduce one accumulator to its report.
pub fn summarize_unit(acc: &Acc) -> UnitReport {
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

// ── the documented JSON view ────────────────────────────────────────────────────────────────────

/// The `attr-stats --json` document.
///
/// Deliberately *not* `serde_json::to_value(report)`: this view is flattened for reading with `jq`
/// (the cardinality curve carries both endpoints inline, every key already has its verdict) and its
/// field names are the ones the write-ups quote. The derived `Serialize` is the wire form instead —
/// lossless, and what the head API sends.
#[cfg(feature = "serde")]
pub fn to_json(report: &Report) -> serde_json::Value {
    let unit = |u: &UnitReport| {
        serde_json::json!({
            "label": u.label,
            "segments": u.segments,
            "rows": u.rows,
            "keys_tracked": u.keys_tracked,
            "keys_sample_rate": u.keys_sample_rate,
            "keys_estimated": u.keys_est,
            "span_nanos": u.span_nanos,
            "windows_per_level": report.levels.iter().enumerate()
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
                    .chain(report.levels.iter().zip(&k.curve).map(|(l, c)| serde_json::json!({
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
    let promotion: Vec<serde_json::Value> = report
        .promotion_candidates()
        .iter()
        .map(|k| {
            serde_json::json!({
                "key": k.name,
                "rows_string": k.rows_string,
                "coverage": k.coverage(report.global.rows),
                "string_fraction": k.string_fraction(),
                "distinct_values_estimated": k.distinct_est,
                "value_len_avg": k.str_len_avg,
                "value_len_max": k.str_len_max,
                "best_sigma_p50": report.best_p50(&k.name),
                "promote_hint": report.promote_verdict(k),
                "index_scale": report.index_scale(&k.name),
                "in_segment_repetition": k.repetition,
            })
        })
        .collect();
    serde_json::json!({
        "dir": report.dir,
        "scopes": report.scopes.iter().map(|s| s.column()).collect::<Vec<_>>(),
        "caps": { "max_keys": report.max_keys, "max_values": report.max_values },
        "window_levels": report.levels.iter().map(|l| serde_json::json!({
            "window": l.label,
            "window_nanos": l.width_nanos,
            "windows": l.windows,
        })).collect::<Vec<_>>(),
        "range_unix_nanos": report.range.map(|(start, end)| serde_json::json!([start, end])),
        "unsealed_wal_frames": report.pending_wal_frames,
        "segments_skipped": report.segments_skipped,
        "tables": report.tables.iter().map(unit).collect::<Vec<_>>(),
        "all_tables": unit(&report.global),
        "promotion_candidates": promotion,
    })
}

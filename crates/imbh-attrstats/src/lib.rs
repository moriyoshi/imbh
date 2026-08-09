//! Measure attribute **cardinality** and per-segment **selectivity** over an imbh database.
//!
//! Three open design questions need the same statistic, and this crate produces it:
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
//!    So cardinality is reported as a **curve** over a ladder of window widths ([`Options::windows`]),
//!    and its shape — flat or rising — is what separates "every segment already holds every value,
//!    nothing prunes" from "values churn, and pruning removes almost everything". See [`accum`] for
//!    the model.
//!
//! ```no_run
//! let report = imbh_attrstats::analyze("./demo-db", &imbh_attrstats::Options::default())?;
//! for line in imbh_attrstats::text::render(&report, 25) {
//!     println!("{line}");
//! }
//! # Ok::<(), imbh_core::Error>(())
//! ```
//!
//! **It reads and changes nothing.** The segment set comes from the manifest via
//! [`imbh_storage::read_disk_snapshot`] (no writer lock, so it runs against a live database), and each
//! segment is opened read-only with just its attribute columns projected. No new column, no sidecar,
//! no manifest edit — this measures the database you already have. That is also what lets the same
//! measurement run from three places: the `attr-stats` CLI over a directory, `imbhd` over the database
//! it is writing (`POST /api/head/attributes/stats`), and `imbh-tui`'s Overview over either.
//!
//! **What it does not cover.** Only *sealed* segments: rows still in the mutable buffer or the
//! unsealed WAL tail have no segment to be selective within, so they are excluded ([`Report`] reports
//! how many WAL frames were skipped — flush first if you want them counted). Promoted keys that are
//! *already* columns still appear here, because the key stays in the JSON blob too.
//!
//! Both map levels are hash-sampled to bound memory (see [`accum::SampledMap`], a bottom-k sketch);
//! whenever a cap engages, the affected row is marked and the sample rate is reported. A truncated
//! result that reads like full coverage would be worse than no result at all. The sample is a pure
//! function of the data, not of the order it was read in, so two runs over the same database report
//! the same numbers even where the caps engaged.

pub mod accum;
pub mod report;
pub mod scan;
pub mod text;

use std::path::Path;

use imbh_core::{Error, Result, SegmentRef, Table};

pub use accum::{AttrScope, SigmaSummary};
pub use report::{KeyReport, LevelReport, Report, UnitReport, promote_verdict};

const NANOS_PER_SEC: i64 = 1_000_000_000;

/// One rung of the cardinality-vs-time-scale ladder: a window width, and the label the report prints
/// it under.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Window {
    /// How the rung is named in the report (`1m`, `24h`). Free-form: it is a label, not a parse key.
    pub label: String,
    pub width_nanos: i64,
}

impl Window {
    /// A rung from a duration spec — `30s` / `5m` / `1h` / `7d`, bare digits being seconds — labelled
    /// with the spec itself.
    pub fn parse(spec: &str) -> Result<Window> {
        Ok(Window {
            label: spec.trim().to_owned(),
            width_nanos: parse_duration(spec)?,
        })
    }
}

/// Parse `30s` / `5m` / `1h` / `7d` (bare digits are seconds) into nanoseconds.
pub fn parse_duration(s: &str) -> Result<i64> {
    let s = s.trim();
    let (digits, mult) = match s.chars().last() {
        Some('s') => (&s[..s.len() - 1], 1),
        Some('m') => (&s[..s.len() - 1], 60),
        Some('h') => (&s[..s.len() - 1], 3_600),
        Some('d') => (&s[..s.len() - 1], 86_400),
        _ => (s, 1),
    };
    let n: i64 = digits.parse().map_err(|_| {
        Error::config_msg(format!(
            "bad duration {s:?} — expected e.g. 30s, 5m, 1h, 7d"
        ))
    })?;
    if n <= 0 {
        return Err(Error::config_msg(format!(
            "duration {s:?} must be positive"
        )));
    }
    Ok(n * mult * NANOS_PER_SEC)
}

/// What to measure, and how much memory to spend measuring it.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(default))]
pub struct Options {
    /// Which attribute columns to read. [`AttrScope::Attributes`] alone is the scope `promote`
    /// covers; the other two only a segment index could.
    pub scopes: Vec<AttrScope>,
    /// Restrict the scan to segments overlapping this absolute `[start_ns, end_ns]` window. Sigma is
    /// defined over "the segments in a time range", so narrowing the range is part of the
    /// measurement, not a shortcut — and it is also what bounds the work on a large database.
    pub range: Option<(i64, i64)>,
    /// The cardinality ladder, innermost first and strictly increasing. Empty disables it (and the
    /// per-value memory it costs).
    pub windows: Vec<Window>,
    /// Per-scan-unit key cap before hash sampling engages.
    pub max_keys: usize,
    /// Per-key distinct-value cap before hash sampling engages.
    pub max_values: usize,
    /// Parquet read batch size.
    pub batch_size: usize,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            scopes: vec![AttrScope::Attributes, AttrScope::Resource, AttrScope::Scope],
            range: None,
            windows: ["1m", "1h", "24h"]
                .iter()
                .map(|spec| Window::parse(spec).expect("literal duration"))
                .collect(),
            max_keys: 4096,
            max_values: 50_000,
            batch_size: 8192,
        }
    }
}

impl Options {
    /// Replace the ladder from a comma-separated spec (`1m,1h,24h`); `none` clears it.
    pub fn with_window_spec(mut self, spec: &str) -> Result<Self> {
        self.windows = if spec.trim() == "none" {
            Vec::new()
        } else {
            spec.split(',')
                .map(Window::parse)
                .collect::<Result<Vec<_>>>()?
        };
        Ok(self)
    }

    /// Segments overlapping the last `minutes` minutes, measured from now.
    pub fn with_last_minutes(mut self, minutes: u64) -> Self {
        let now = imbh_core::Timestamp::now().0;
        let start = now.saturating_sub((minutes as i64).saturating_mul(60 * NANOS_PER_SEC));
        self.range = Some((start, i64::MAX));
        self
    }

    /// Reject a request that cannot be measured, before any segment is opened. The ladder is read as
    /// a *curve*, so its widths must increase; a zero cap would sample nothing at all.
    pub fn validate(&self) -> Result<()> {
        if self
            .windows
            .windows(2)
            .any(|p| p[0].width_nanos >= p[1].width_nanos)
        {
            return Err(Error::config_msg(
                "window widths must be strictly increasing, innermost first",
            ));
        }
        if self.windows.iter().any(|w| w.width_nanos <= 0) {
            return Err(Error::config_msg("window widths must be positive"));
        }
        if self.max_keys == 0 || self.max_values == 0 || self.batch_size == 0 {
            return Err(Error::config_msg(
                "max_keys, max_values and batch_size must all be non-zero",
            ));
        }
        if self.scopes.is_empty() {
            return Err(Error::config_msg(
                "at least one attribute scope is required",
            ));
        }
        Ok(())
    }

    fn widths(&self) -> Vec<i64> {
        self.windows.iter().map(|w| w.width_nanos).collect()
    }

    /// Segment selection: keep segments whose `[min_time, max_time]` overlaps the requested window.
    fn selects(&self, seg: &SegmentRef) -> bool {
        match self.range {
            None => true,
            Some((start, end)) => seg.max_time_unix_nano >= start && seg.min_time_unix_nano <= end,
        }
    }
}

/// Measure `dir`, an imbh database directory.
///
/// Takes no lock and writes nothing, so it runs against a database a writer has open. Fails only if
/// the manifest cannot be replayed — a segment that vanishes mid-scan (retention or compaction under
/// a live writer) is recorded in [`Report::segments_skipped`] rather than aborting the measurement.
pub fn analyze(dir: impl AsRef<Path>, options: &Options) -> Result<Report> {
    options.validate()?;
    let dir = dir.as_ref();
    // `read_disk_snapshot` answers "no segments" for a directory with no manifest, which is the
    // right answer for a database that has never sealed one — and the wrong one for a mistyped path,
    // where it is indistinguishable from a database with no attributes. A path that is not a
    // directory at all is never the former, so it is refused here rather than measured as empty.
    if !dir.is_dir() {
        return Err(Error::config_msg(format!(
            "{} is not a database directory",
            dir.display()
        )));
    }
    let snapshot = imbh_storage::read_disk_snapshot(dir)?;
    let widths = options.widths();
    // The DB-wide unit backs the promotion report: `promote` is one DB-wide list, so a key's
    // cardinality and coverage must be measured across every table, not per table.
    let mut global = accum::Acc::new("ALL", options.max_keys, options.max_values, &widths);
    let mut units: Vec<accum::Acc> = Table::ALL
        .iter()
        .map(|t| accum::Acc::new(t.as_str(), options.max_keys, options.max_values, &widths))
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
        work.extend(all.iter().filter(|s| options.selects(s)).map(|s| (idx, s)));
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
        let mut sinks: [&mut accum::Acc; 2] = [&mut units[idx], &mut global];
        if let Err(e) = scan::scan_segment(&path, &options.scopes, options.batch_size, &mut sinks) {
            // A segment can vanish under us (retention/compaction on a live writer). Report it
            // rather than aborting the whole measurement or silently under-counting.
            segments_skipped.push(format!("{}: {e}", seg.relative_path));
        }
    }

    let levels: Vec<LevelReport> = options
        .windows
        .iter()
        .enumerate()
        .map(|(i, window)| LevelReport {
            label: window.label.clone(),
            width_nanos: window.width_nanos,
            windows: global.windows[i],
        })
        .collect();

    Ok(Report {
        dir: dir.display().to_string(),
        scopes: options.scopes.clone(),
        max_keys: options.max_keys,
        max_values: options.max_values,
        range: options.range,
        levels,
        pending_wal_frames: snapshot.pending.len(),
        segments_skipped,
        tables: units.iter().map(report::summarize_unit).collect(),
        global: report::summarize_unit(&global),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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

        // The default ladder is what the CLI usage text advertises.
        let options = Options::default();
        let labels: Vec<&str> = options.windows.iter().map(|w| w.label.as_str()).collect();
        assert_eq!(labels, vec!["1m", "1h", "24h"]);
        assert_eq!(
            options.widths(),
            [60, 3_600, 86_400]
                .iter()
                .map(|s| s * NANOS_PER_SEC)
                .collect::<Vec<_>>()
        );
        options.validate().expect("the default ladder is valid");
        assert!(
            Options::default()
                .with_window_spec("none")
                .unwrap()
                .windows
                .is_empty()
        );
    }

    /// The ladder is read as a curve, so a request that could not produce one is refused before any
    /// segment is opened rather than answered with numbers that cannot be compared.
    #[test]
    fn a_ladder_that_is_not_a_curve_is_refused() {
        let mut options = Options::default();
        options.windows.reverse();
        assert!(options.validate().is_err(), "decreasing widths");
        options.windows = vec![
            Window {
                label: "dup".to_owned(),
                width_nanos: 60,
            };
            2
        ];
        assert!(options.validate().is_err(), "equal widths");
        assert!(
            Options {
                max_values: 0,
                ..Options::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            Options {
                scopes: Vec::new(),
                ..Options::default()
            }
            .validate()
            .is_err()
        );
    }

    /// The range filter keeps every segment that *overlaps* the window — a segment straddling the
    /// start boundary carries rows inside it.
    #[test]
    fn the_range_filter_keeps_overlapping_segments() {
        let seg = |min: i64, max: i64| SegmentRef {
            relative_path: "s.parquet".into(),
            min_time_unix_nano: min,
            max_time_unix_nano: max,
            rows: 1,
        };
        let options = Options {
            range: Some((100, 200)),
            ..Options::default()
        };
        assert!(options.selects(&seg(150, 160)), "wholly inside");
        assert!(options.selects(&seg(50, 150)), "straddles the start");
        assert!(options.selects(&seg(150, 250)), "straddles the end");
        assert!(options.selects(&seg(0, 300)), "spans the whole window");
        assert!(!options.selects(&seg(0, 99)), "entirely before");
        assert!(!options.selects(&seg(201, 300)), "entirely after");
        // No range keeps everything.
        assert!(Options::default().selects(&seg(0, 1)));
    }
}

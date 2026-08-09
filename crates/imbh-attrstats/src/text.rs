//! The report as fixed-width text — the `attr-stats` CLI's output, and any other consumer that
//! wants the same three sections without re-deriving them.
//!
//! [`render`] returns lines rather than printing, so a terminal UI can put them in a pane and a test
//! can assert on them. The sections are ordered by what a reader decides with: sigma (can a segment
//! index prune?), the cardinality curve (at what time scale?), then the promotion candidates (is a
//! column cheaper?), and finally what the caps truncated — which comes last but is never omitted,
//! because a truncated result that reads like full coverage is worse than no result at all.

use crate::accum::AttrScope;
use crate::report::{KeyReport, Report};

const NANOS_PER_SEC: i64 = 1_000_000_000;

/// Render the whole report, listing at most `top` keys per section.
pub fn render(report: &Report, top: usize) -> Vec<String> {
    let mut out = Vec::new();
    header(report, &mut out);
    sigma_section(report, top, &mut out);
    locality_section(report, top, &mut out);
    promotion_section(report, top, &mut out);
    capped_section(report, &mut out);
    out
}

fn header(report: &Report, out: &mut Vec<String>) {
    out.push(format!("imbh attribute statistics — {}", report.dir));
    let scopes: Vec<&str> = report.scopes.iter().map(|s| s.column()).collect();
    out.push(format!(
        "  scopes: {}   segments: {}   rows: {}",
        scopes.join(", "),
        report.global.segments,
        num(report.global.rows),
    ));
    match report.range {
        Some((start, end)) => out.push(format!(
            "  segment window: segments overlapping [{start}, {end}] ({} wide)",
            dur(end.saturating_sub(start)),
        )),
        None => out.push("  segment window: all sealed segments".to_owned()),
    }
    out.push(format!(
        "  caps: {} keys/unit, {} values/key, sampling engages beyond (see `sample` column)",
        num(report.max_keys as u64),
        num(report.max_values as u64),
    ));
    if report.levels.is_empty() {
        out.push("  window ladder: disabled".to_owned());
    } else {
        let ladder: Vec<String> = report
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
        out.push(format!(
            "  window ladder: segment < {} < all  — over {}",
            ladder.join(" < "),
            dur(report.global.span_nanos),
        ));
    }
    if report.pending_wal_frames > 0 {
        out.push(format!(
            "  NOT MEASURED: {} unsealed WAL frame(s) — buffered rows are in no segment yet",
            report.pending_wal_frames
        ));
    }
    for skipped in &report.segments_skipped {
        out.push(format!("  SEGMENT SKIPPED: {skipped}"));
    }
}

/// The reading instructions above section 1. Held as one block rather than a run of `push` calls so
/// the prose stays legible at its printed width in the source too.
const SIGMA_PREAMBLE: &str = "\
== 1. SEGMENT-PRUNING POTENTIAL (sigma) ==
  sigma(key,value) = fraction of this table's segments holding >=1 matching row; a
  segment index prunes 1 - sigma. One sample per DISTINCT value, unweighted by how
  often it occurs. sigma ~ 1 => the index buys nothing for that value.
  postings = (key, value, segment) entries such an index would store for this key.
  hist = 10 sigma buckets [0,.1)..[.9,1]; '.' = empty, otherwise digits of count.";

fn sigma_section(report: &Report, top: usize, out: &mut Vec<String>) {
    out.push(String::new());
    out.extend(SIGMA_PREAMBLE.lines().map(str::to_owned));

    for table in &report.tables {
        out.push(String::new());
        if table.segments == 0 {
            out.push(format!("  -- {} -- no segments in range", table.label));
            continue;
        }
        out.push(format!(
            "  -- {} -- {} segment{}, {} rows, {} keys{}",
            table.label,
            table.segments,
            if table.segments == 1 { "" } else { "s" },
            num(table.rows),
            table.keys_tracked,
            sampled_note(table.keys_sample_rate, table.keys_est),
        ));
        if table.segments < 2 {
            out.push("     (1 segment: sigma is 1.0 by construction and says nothing)".to_owned());
        }
        out.push(format!(
            "     {:<34} {:>10} {:>11} {:>6} {:>6} {:>6} {:>6} {:>6}  {:<10} sample",
            "key", "distinct", "postings", "p50", "p90", "max", "mean", "<=.25", "hist",
        ));
        for key in table.keys.iter().take(top) {
            let Some(sigma) = &key.sigma else { continue };
            out.push(format!(
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
            ));
        }
        if table.keys.len() > top {
            out.push(format!("     ... {} more keys", table.keys.len() - top));
        }
    }
}

/// Section 2: cardinality as a function of window width.
///
/// Sigma (section 1) answers the question at exactly one scale — the segment. This answers it at
/// several, because the same key can be localized against a day and interleaved against a minute,
/// and which one governs depends on the range the user queries over. The reading is the *shape*, not
/// any single column.
// No `\` line-continuation on the opening line: that escape swallows the next line's leading
// whitespace, which is this block's indentation.
const LOCALITY_PREAMBLE: &str =
    "  C(w) = mean distinct values of the key within one window of width w. C(seg) is
  the innermost point (= postings/segments, the same number sigma summarises) and
  C(all) the outermost (= global distinct count). loc = C(all)/C(seg).
    loc ~ 1        interleaved: every segment already holds every value. Nothing
                   prunes at any scale; a promoted column is the only lever.
    loc >> 1       localized: values churn, and segment pruning removes 1 - 1/loc.
                   The width where the curve flattens is the horizon beyond which
                   pruning stops paying — read it off the C(w) columns.
  rep = rows per (value, segment) posting: in-segment repetition, which is what
  drives a promoted dictionary column's bytes on disk (not global cardinality).";

fn locality_section(report: &Report, top: usize, out: &mut Vec<String>) {
    out.push(String::new());
    out.push("== 2. CARDINALITY vs TIME SCALE (locality) ==".to_owned());
    if report.levels.is_empty() {
        out.push("  Disabled. Only the segment and whole-scan endpoints exist.".to_owned());
        return;
    }
    out.extend(LOCALITY_PREAMBLE.lines().map(str::to_owned));

    for table in &report.tables {
        if table.segments == 0 {
            continue;
        }
        out.push(String::new());
        out.push(format!(
            "  -- {} -- {} segment{} over {}",
            table.label,
            table.segments,
            if table.segments == 1 { "" } else { "s" },
            dur(table.span_nanos),
        ));
        // A level that opened one window has collapsed onto C(all); one that opened about as many
        // windows as there are segments has collapsed onto C(seg). Either way it is not an
        // independent point on the curve, and silently printing it would invite reading a
        // coincidence as a finding.
        let degenerate: Vec<String> = report
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
            out.push(format!(
                "     collapsed at this scale: {}",
                degenerate.join(", ")
            ));
        }
        let mut header = format!("     {:<34} {:>8} {:>9}", "key", "rep", "C(seg)");
        for level in &report.levels {
            header.push_str(&format!(" {:>9}", format!("C({})", level.label)));
        }
        header.push_str(&format!(" {:>9} {:>8}", "C(all)", "loc"));
        out.push(header);

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
            out.push(row);
        }
        if table.keys.len() > top {
            out.push(format!("     ... {} more keys", table.keys.len() - top));
        }
    }
}

const PROMOTION_PREAMBLE: &str = "\
== 3. PROMOTION CANDIDATES (record `attributes` scope, string values only) ==
  Scope matches `lookup_promoted` exactly: json_get(attributes, key) kept only when
  AnyValue::Str. `resource`/`scope` keys are excluded here (a different scope) even
  when the sigma section above lists them.
  coverage = rows a promoted column would be non-NULL on / all rows in the database.
  rep = rows per (value, segment) posting; runs/row = how often the value changes
  from one row to the next (~1 = interleaved, low = arrives in runs).
  est B/row = [dictionary (C(seg) x len) + index (runs x log2(C(seg))/8)] / rows-per-seg
  — the two terms a promoted column pays. Cardinality is NOT one of them, and neither
  is repetition alone: the SAME session population measured 9,079 B contiguous and
  64,252 B interleaved (archetype-bench). Estimates run 2-5x high because zstd beats
  the model — use them to RANK keys, not to size a budget.
  column costs: Parquet dictionaries are per column chunk, so measured cost was
  +1,206 B/key at 3,125x repetition and +108,842 B/key at 1x (promote-cost bench).
  promote / index@ are INDEPENDENT verdicts — a key can want both. index@ is the
  widest query range over which pruning still pays (mean sigma <= 0.25); `-` means
  none of the ladder's rungs qualify.";

fn promotion_section(report: &Report, top: usize, out: &mut Vec<String>) {
    out.push(String::new());
    out.extend(PROMOTION_PREAMBLE.lines().map(str::to_owned));
    out.push(String::new());
    out.push(format!(
        "  {:<34} {:>9} {:>7} {:>10} {:>8} {:>7} {:>9}  {:<9} index@",
        "key", "coverage", "str%", "distinct", "rep", "runs/row", "est B/row", "promote",
    ));
    let promo = report.promotion_candidates();
    for key in promo.iter().take(top) {
        out.push(format!(
            "  {:<34} {:>9.3} {:>7.2} {:>10} {:>8.1} {:>7.2} {:>9.2}  {:<9} {}",
            clip(&key.name, 34),
            key.coverage(report.global.rows),
            key.string_fraction(),
            est(key.distinct_est, key.values_sample_rate),
            key.repetition,
            key.runs_per_row(),
            key.est_bytes_per_row,
            report.promote_verdict(key),
            report.index_scale(&key.name).unwrap_or_else(|| "-".into()),
        ));
    }
    if promo.len() > top {
        out.push(format!("  ... {} more keys", promo.len() - top));
    }
}

fn capped_section(report: &Report, out: &mut Vec<String>) {
    out.push(String::new());
    out.push("== WHAT WAS CAPPED ==".to_owned());
    if report.is_exact() {
        out.push("  Nothing. Every key and every distinct value was counted exactly.".to_owned());
    } else {
        let names = report.sampled_keys();
        if !names.is_empty() {
            out.push(format!(
                "  Value maps fell back to hash sampling for: {}. Their `distinct`/`postings`",
                names.join(", ")
            ));
            out.extend(
                "  are estimates (tracked / sample-rate) and their sigma distribution is over an\n  \
                 unbiased sample of values, not all of them. Raise the value cap to tighten."
                    .lines()
                    .map(str::to_owned),
            );
        }
        for table in &report.tables {
            if table.keys_sample_rate < 1.0 {
                out.push(format!(
                    "  {}: key map sampled at {} — {} keys existed, {} kept. Raise the key cap.",
                    table.label,
                    rate_note(table.keys_sample_rate),
                    est(table.keys_est, table.keys_sample_rate),
                    table.keys_tracked,
                ));
            }
        }
    }
    if report.pending_wal_frames > 0 {
        out.push(format!(
            "  {} unsealed WAL frame(s) were not measured (no segment to be selective within).",
            report.pending_wal_frames
        ));
    }
}

// ── formatting helpers ──────────────────────────────────────────────────────────────────────────

/// A count with thousands separators.
pub fn num(n: u64) -> String {
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
pub fn est(x: f64, sample_rate: f64) -> String {
    let n = num(x.round().max(0.0) as u64);
    if sample_rate >= 1.0 {
        n
    } else {
        format!("~{n}")
    }
}

/// A nanosecond span as the coarsest unit that keeps it readable — this is a header line, not a
/// measurement, so one decimal is plenty.
pub fn dur(nanos: i64) -> String {
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
pub fn rate_note(rate: f64) -> String {
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
pub fn histogram(buckets: &[u32; 10]) -> String {
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

/// Truncate to `width` display columns, marking the cut with `>`.
pub fn clip(s: &str, width: usize) -> String {
    if s.chars().count() <= width {
        s.to_owned()
    } else {
        let keep: String = s.chars().take(width.saturating_sub(1)).collect();
        format!("{keep}>")
    }
}

/// The scope's column name, for a consumer rendering a scope column.
pub fn scope_label(scope: AttrScope) -> &'static str {
    scope.column()
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(scope_label(AttrScope::Resource), "resource");
    }
}

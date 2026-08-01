//! Trace waterfalls: the width-independent row model and its renderer.
//!
//! [`build_trace_detail`] reduces an [`imbh::Trace`] to [`Waterfall`] rows plus the structured
//! [`SpanRecord`]s behind them; [`render_waterfall`] paints those rows at whatever width the pane is
//! given at draw time.

use std::collections::{HashMap, HashSet};

use crate::format::{attrs_to_pairs, clamp_field};

/// A trace's spans reduced to width-independent pieces, so the bars can be re-rendered to whatever
/// width the detail pane is given at draw time (`render_waterfall`) rather than baked to a fixed size.
#[derive(Debug, Clone)]
pub(crate) struct Waterfall {
    pub(crate) rows: Vec<WaterfallRow>,
    /// The bar glyph (`━`, or `#` in ASCII mode).
    pub(crate) marker: char,
}

/// One waterfall row: a fixed-width prefix and trailing column, plus the bar's position as fractions
/// of the trace duration. `render_waterfall` maps `start`/`frac` onto the available bar cells.
#[derive(Debug, Clone)]
pub(crate) struct WaterfallRow {
    /// Status marker + depth indent + `WATERFALL_NAME_W`-clamped name: the constant-width prefix
    /// before the bar, so every bar starts at the same column regardless of depth.
    pub(crate) prefix: String,
    /// Bar start as a fraction of the trace duration, in `[0, 1)`.
    pub(crate) start: f64,
    /// Bar length as a fraction of the trace duration, in `(0, 1]`.
    pub(crate) frac: f64,
    /// The trailing `  12.345ms OK` (duration + status) column, rendered as-is after the closing bar.
    pub(crate) suffix: String,
}

/// One span of a materialized trace, cloned out of [`imbh::Span`] into display-ready fields so an open
/// [`Route::TraceDetail`](crate::model::Route::TraceDetail) /
/// [`Route::SpanDetail`](crate::model::Route::SpanDetail) is self-contained (like
/// [`LogRecord`](crate::model::LogRecord)) and survives
/// background refreshes. Aligned to the trace's [`Waterfall`] rows, one record per row.
#[derive(Debug, Clone)]
pub(crate) struct SpanRecord {
    pub(crate) name: String,
    pub(crate) span_id: String,
    pub(crate) parent_span_id: Option<String>,
    pub(crate) service: Option<String>,
    pub(crate) kind: String,
    pub(crate) status_code: String,
    pub(crate) status_message: Option<String>,
    pub(crate) start_time_ns: i64,
    /// Offset of the span's start from the trace start, so the detail can place it within the trace.
    pub(crate) offset_ns: i64,
    pub(crate) duration_ns: u64,
    pub(crate) attributes: Vec<(String, String)>,
    pub(crate) resource: Vec<(String, String)>,
    pub(crate) scope: Vec<(String, String)>,
    /// Events/links as the canonical JSON the storage layer keeps them in (ARCHITECTURE.md §6.3).
    pub(crate) events: Option<String>,
    pub(crate) links: Option<String>,
    /// Whether the span's parent chain is broken (an orphan or a cycle). Such spans stay visible as
    /// malformed roots rather than being dropped (TUI_PLAN.md §3.3).
    pub(crate) malformed: bool,
}

impl SpanRecord {
    /// Whether the span carries a non-OK status (drives the red waterfall row).
    pub(crate) fn is_error(&self) -> bool {
        self.status_code.eq_ignore_ascii_case("error")
    }
}

/// A whole materialized trace: the width-independent [`Waterfall`] plus the structured per-span
/// records behind it. Cloned into [`Route::TraceDetail`](crate::model::Route::TraceDetail) when the
/// full-screen trace view opens, so the
/// view keeps its data across refreshes and the navigation history captures it for free.
#[derive(Debug, Clone)]
pub(crate) struct TraceDetail {
    /// Lowercase-hex trace id.
    pub(crate) trace_id: String,
    pub(crate) root_service: Option<String>,
    pub(crate) root_name: Option<String>,
    pub(crate) start_time_ns: i64,
    pub(crate) duration_ns: u64,
    /// Rows aligned to `spans` (`waterfall.rows[i]` ↔ `spans[i]`).
    pub(crate) waterfall: Waterfall,
    pub(crate) spans: Vec<SpanRecord>,
}

/// Fixed width (terminal cells) of the waterfall name column; the status marker prepends one more.
pub(crate) const WATERFALL_NAME_W: usize = 20;

/// Cells kept to the right of the bar for the ` 12.345ms STATUS` column, so the bar never crowds it.
pub(crate) const WATERFALL_SUFFIX_W: usize = 20;

/// Reduce a trace to width-independent waterfall rows (see [`WaterfallRow`]) plus the structured
/// per-span records behind them (aligned index-for-index). The bars are stored as fractions of the
/// trace duration; [`render_waterfall`] paints them onto the pane's actual width. Spans whose parent
/// chain is broken (orphans, cycles) stay visible as malformed roots rather than being dropped.
pub(crate) fn build_trace_detail(trace: &imbh::Trace, ascii: bool) -> TraceDetail {
    let parents = trace
        .spans
        .iter()
        .map(|span| {
            (
                span.span_id.to_hex(),
                span.parent_span_id.map(|parent| parent.to_hex()),
            )
        })
        .collect::<HashMap<_, _>>();
    let duration = trace.duration_ns.0.max(1) as f64;
    let (rows, spans): (Vec<WaterfallRow>, Vec<SpanRecord>) = trace
        .spans
        .iter()
        .map(|span| {
            let id = span.span_id.to_hex();
            let mut parent = span.parent_span_id.map(|parent| parent.to_hex());
            let mut seen = HashSet::from([id.clone()]);
            let mut depth = 0usize;
            let mut malformed = false;
            while let Some(parent_id) = parent {
                if !seen.insert(parent_id.clone()) {
                    malformed = true;
                    break;
                }
                let Some(next) = parents.get(&parent_id) else {
                    malformed = true;
                    break;
                };
                depth = depth.saturating_add(1).min(16);
                parent = next.clone();
            }
            let offset_ns = span.start_time.0.saturating_sub(trace.start_time.0).max(0);
            // The bar's position and length as fractions of the trace duration — resolution-free, so
            // the same row renders correctly at any pane width.
            let start = (offset_ns as f64 / duration).clamp(0.0, 1.0);
            let frac = (span.duration_ns.0 as f64 / duration).clamp(0.0, 1.0);
            // Fold the depth indent into the name and clamp the pair to WATERFALL_NAME_W (char-aware,
            // with an ellipsis) so the fixed-width prefix keeps every bar starting at the same column.
            let label = clamp_field(
                &format!("{}{}", "  ".repeat(depth), span.name),
                WATERFALL_NAME_W,
            );
            let row = WaterfallRow {
                prefix: format!("{}{label}", if malformed { "!" } else { " " }),
                start,
                frac,
                suffix: format!(
                    "{:>8.3}ms {}",
                    span.duration_ns.0 as f64 / 1_000_000.0,
                    span.status_code
                ),
            };
            let record = SpanRecord {
                name: span.name.clone(),
                span_id: id,
                parent_span_id: span.parent_span_id.map(|parent| parent.to_hex()),
                service: span.service.clone(),
                kind: span.kind.clone(),
                status_code: span.status_code.clone(),
                status_message: span.status_message.clone(),
                start_time_ns: span.start_time.0,
                offset_ns,
                duration_ns: span.duration_ns.0,
                attributes: attrs_to_pairs(&span.attributes),
                resource: attrs_to_pairs(&span.resource),
                scope: attrs_to_pairs(&span.scope),
                events: span.events.clone(),
                links: span.links.clone(),
                malformed,
            };
            (row, record)
        })
        .unzip();
    TraceDetail {
        trace_id: trace.trace_id.to_hex(),
        root_service: trace.root_service.clone(),
        root_name: trace.root_name.clone(),
        start_time_ns: trace.start_time.0,
        duration_ns: trace.duration_ns.0,
        waterfall: Waterfall {
            rows,
            marker: if ascii { '#' } else { '━' },
        },
        spans,
    }
}

/// Paint a [`Waterfall`] into text lines whose bars span exactly `bar_cells` cells, so both the
/// opening and closing `|` line up in a column and the bars stretch to fill the pane.
pub(crate) fn render_waterfall(waterfall: &Waterfall, bar_cells: usize) -> Vec<String> {
    let cells = bar_cells.max(1);
    waterfall
        .rows
        .iter()
        .map(|row| {
            let start = ((row.start * cells as f64) as usize).min(cells - 1);
            let width = ((row.frac * cells as f64).round() as usize)
                .max(1)
                .min(cells - start);
            let mut bar = String::with_capacity(cells);
            bar.extend(std::iter::repeat_n(' ', start));
            bar.extend(std::iter::repeat_n(waterfall.marker, width));
            bar.extend(std::iter::repeat_n(' ', cells - start - width));
            format!("{}|{}|{}", row.prefix, bar, row.suffix)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use imbh::{Timestamp, TraceId};
    use unicode_width::UnicodeWidthStr;

    use crate::testutil::{sample_trace, waterfall_span};

    #[test]
    fn waterfall_bars_align_regardless_of_depth_or_wide_names() {
        // A root plus a nested child with a CJK name: the child indents, but the `|bar|` axis must
        // start at the same terminal column on both rows.
        let trace = imbh::Trace {
            trace_id: TraceId([0xaa; 16]),
            root_service: None,
            root_name: Some("root".to_owned()),
            start_time: Timestamp(0),
            duration_ns: imbh::DurationNs(1_000_000),
            spans: vec![
                waterfall_span(1, None, "root", 0, 1_000_000),
                waterfall_span(2, Some(1), "データベース照会", 200_000, 400_000),
            ],
        };
        // Render the width-independent rows at two different bar widths: alignment must hold at any
        // size, and the bar must actually stretch to the width it is given.
        let waterfall = build_trace_detail(&trace, true).waterfall;
        for cells in [40usize, 77] {
            let lines = render_waterfall(&waterfall, cells);
            assert_eq!(lines.len(), 2);
            // Everything before the first `|` is a constant width across rows, so the bars line up:
            // marker (1) + name field (WATERFALL_NAME_W == 20).
            let axis = |line: &str| UnicodeWidthStr::width(line.split('|').next().unwrap());
            assert_eq!(axis(&lines[0]), axis(&lines[1]));
            assert_eq!(axis(&lines[0]), 1 + 20);
            // The bar (between the two `|`) fills exactly `cells` cells, so the closing `|` and the
            // trailing duration column also line up.
            let bar = |line: &str| UnicodeWidthStr::width(line.split('|').nth(1).unwrap());
            assert_eq!(bar(&lines[0]), cells);
            assert_eq!(bar(&lines[1]), cells);
        }
    }

    #[test]
    fn trace_detail_records_align_with_the_waterfall_rows() {
        let detail = build_trace_detail(&sample_trace(), true);
        assert_eq!(detail.spans.len(), detail.waterfall.rows.len());
        assert_eq!(detail.trace_id, TraceId([0xaa; 16]).to_hex());
        assert_eq!(detail.root_name.as_deref(), Some("GET /users"));
        assert_eq!(detail.duration_ns, 1_000_000);

        // The root: no parent, at the trace start, and the row is not flagged malformed.
        assert_eq!(detail.spans[0].parent_span_id, None);
        assert_eq!(detail.spans[0].offset_ns, 0);
        assert!(!detail.spans[0].malformed);
        assert!(detail.waterfall.rows[0].prefix.starts_with(' '));

        // The child: parented, offset into the trace, and indented one level in its row prefix.
        assert_eq!(
            detail.spans[1].parent_span_id.as_deref(),
            Some(imbh::SpanId([1; 8]).to_hex().as_str())
        );
        assert_eq!(detail.spans[1].offset_ns, 200_000);
        assert_eq!(
            detail.spans[1].attributes,
            vec![("db.system".to_owned(), "postgres".to_owned())]
        );
        assert!(detail.waterfall.rows[1].prefix.starts_with("   "));

        // The orphan stays visible, flagged malformed (`!` marker) rather than dropped, and reads as an
        // error so the waterfall row can be coloured.
        assert!(detail.spans[2].malformed);
        assert!(detail.spans[2].is_error());
        assert!(detail.waterfall.rows[2].prefix.starts_with('!'));
    }
}

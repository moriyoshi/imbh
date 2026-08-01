//! Trace waterfalls: the width-independent row model and its renderer.
//!
//! [`build_trace_detail`] reduces an [`imbh::Trace`] to [`Waterfall`] rows plus the structured
//! [`SpanRecord`]s behind them; [`render_waterfall`] paints those rows at whatever width the pane is
//! given at draw time.

use std::collections::{HashMap, HashSet};

use unicode_width::UnicodeWidthStr;

use crate::format::{attrs_to_pairs, clip_field};

/// A trace's spans reduced to width-independent pieces, so the bars can be re-rendered to whatever
/// width the detail pane is given at draw time (`render_waterfall`) rather than baked to a fixed size.
#[derive(Debug, Clone)]
pub(crate) struct Waterfall {
    pub(crate) rows: Vec<WaterfallRow>,
    /// The bar glyph (`━`, or `#` in ASCII mode).
    pub(crate) marker: char,
    /// The de-emphasised bar glyph for pinned rows (`─`, or `-` in ASCII mode).
    ///
    /// A lighter *glyph* is used rather than relying on `Modifier::DIM` alone because many terminals
    /// draw box-drawing characters procedurally — from a built-in geometry renderer rather than the
    /// font — and that path honours the cell's foreground colour but ignores the faint attribute. The
    /// symptom is a pinned row whose name and duration dim while its bar stays at full intensity. Both
    /// glyphs are EAW-ambiguous in exactly the same way (`width` 1, `width_cjk` 2), so substituting one
    /// for the other cannot shift the `|bar|` axis relative to the scrolling rows.
    pub(crate) light_marker: char,
}

/// One waterfall row: the name column's raw pieces and trailing column, plus the bar's position as
/// fractions of the trace duration. `render_waterfall` maps `start`/`frac` onto the available bar
/// cells and lays `indent`/`name` into the fixed-width name column.
#[derive(Debug, Clone)]
pub(crate) struct WaterfallRow {
    /// The status marker before the name column: `' '`, or `'!'` when the parent chain is broken.
    pub(crate) marker: char,
    /// Nesting depth in *levels* (two cells each), already capped at [`WATERFALL_MAX_INDENT`].
    ///
    /// Kept apart from `name` because the indent must **never** scroll: it is the only thing showing
    /// the trace's shape, so scrolling it away to read a long name would trade the whole tree for one
    /// row's tail. The name scrolls inside whatever the indent leaves it.
    pub(crate) indent: usize,
    /// The span name, **unclipped**. [`render_waterfall`] clips it into the cells the indent leaves
    /// within the `WATERFALL_NAME_W`-wide column at draw time, so the column can scroll horizontally
    /// without the rows being rebuilt.
    pub(crate) name: String,
    /// Row index of this span's parent span, or `None` for a root or a parent absent from the trace.
    ///
    /// Rows are start-time ordered, not a DFS of the tree (`crates/imbh/src/traces.rs`), so clock
    /// skew across services can put a parent *after* its child. [`ancestor_rows`] therefore only ever
    /// walks strictly upward, which is also what makes that walk terminate without a cycle set.
    ///
    /// This is deliberately independent of `indent`: that comes from an id-chain walk which survives
    /// out-of-order parents, so the two can legitimately disagree (the indent says depth 3 while only
    /// two rows are pinnable). The disagreement is cosmetically invisible.
    pub(crate) parent_row: Option<usize>,
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

/// Deepest nesting level the row indent renders. Past it every row indents the same, because the
/// indent shares [`WATERFALL_NAME_W`] with the name: at two cells per level an uncapped indent leaves
/// only four readable name cells at depth 8, exactly where a deep trace needs them most. The
/// hierarchy signal the cap gives up is what the pinned ancestor rows ([`sticky_layout`]) restore.
pub(crate) const WATERFALL_MAX_INDENT: usize = 5;

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
    // Span id -> the row it lands on, so a row can point at its parent's row for the sticky-ancestor
    // walk. `or_insert` keeps the *first* occurrence: malformed data does contain duplicate span ids,
    // and the earlier row is the one more likely to precede the children that name it.
    let mut row_of: HashMap<String, usize> = HashMap::with_capacity(trace.spans.len());
    for (index, span) in trace.spans.iter().enumerate() {
        row_of.entry(span.span_id.to_hex()).or_insert(index);
    }
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
            // The indent is capped so a deep span keeps a readable name; the name is laid into the
            // cells it leaves at draw time, where the column width and its scroll offset are known.
            let row = WaterfallRow {
                marker: if malformed { '!' } else { ' ' },
                indent: depth.min(WATERFALL_MAX_INDENT),
                name: span.name.clone(),
                parent_row: span
                    .parent_span_id
                    .and_then(|parent| row_of.get(&parent.to_hex()).copied()),
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
            light_marker: if ascii { '-' } else { '─' },
        },
        spans,
    }
}

/// The draw-time parameters a [`Waterfall`] is painted with: everything that depends on the pane's
/// width and on where it is scrolled to. Kept as one named-field literal rather than a growing
/// argument list (the same shape `SpanSpec` uses in `gen-demo-db`).
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct WaterfallView {
    /// Cells the `|bar|` axis spans.
    pub(crate) bar_cells: usize,
    /// Horizontal scroll of the name column (see [`name_offset`]).
    pub(crate) name_offset: usize,
    /// Indent *levels* subtracted from every row, so the shallowest span on screen sits flush against
    /// the name column and the rows below keep their offsets relative to it (see
    /// [`visible_indent_base`]). `0` renders absolute depth.
    pub(crate) indent_base: usize,
}

/// Paint a [`Waterfall`] into text lines whose bars span exactly `view.bar_cells` cells, so both the
/// opening and closing `|` line up in a column and the bars stretch to fill the pane.
pub(crate) fn render_waterfall(waterfall: &Waterfall, view: &WaterfallView) -> Vec<String> {
    (0..waterfall.rows.len())
        .map(|index| render_waterfall_row(waterfall, index, view, waterfall.marker))
        .collect()
}

/// One [`render_waterfall`] line, painted with `marker` as the bar glyph — so a caller that wants a
/// de-emphasised row (the pinned ancestors, via [`Waterfall::light_marker`]) can paint it without
/// re-rendering the whole waterfall. Panics on an out-of-range index.
pub(crate) fn render_waterfall_row(
    waterfall: &Waterfall,
    index: usize,
    view: &WaterfallView,
    marker: char,
) -> String {
    let cells = view.bar_cells.max(1);
    let row = &waterfall.rows[index];
    let start = ((row.start * cells as f64) as usize).min(cells - 1);
    let width = ((row.frac * cells as f64).round() as usize)
        .max(1)
        .min(cells - start);
    let mut bar = String::with_capacity(cells);
    bar.extend(std::iter::repeat_n(' ', start));
    bar.extend(std::iter::repeat_n(marker, width));
    bar.extend(std::iter::repeat_n(' ', cells - start - width));
    // The indent is laid down verbatim and the name scrolls inside what it leaves, so the trace's
    // shape survives however far the column has scrolled. Clamping to the row's own offset keeps a
    // row that has nothing left to hide from scrolling into blankness.
    let indent = row_indent_cells(row, view.indent_base);
    format!(
        "{}{}{}|{}|{}",
        row.marker,
        " ".repeat(indent),
        clip_field(
            &row.name,
            WATERFALL_NAME_W - indent,
            view.name_offset.min(row_name_offset(row, view.indent_base))
        ),
        bar,
        row.suffix
    )
}

/// Cells of leading indent a row renders, once the on-screen base is subtracted.
fn row_indent_cells(row: &WaterfallRow, indent_base: usize) -> usize {
    (row.indent.saturating_sub(indent_base) * 2).min(WATERFALL_NAME_W)
}

/// How far this row's name may usefully scroll: enough to bring its tail to the last cell of the
/// window the indent leaves it, and no further (scrolling past that only pads with blanks).
fn row_name_offset(row: &WaterfallRow, indent_base: usize) -> usize {
    let window = WATERFALL_NAME_W.saturating_sub(row_indent_cells(row, indent_base));
    UnicodeWidthStr::width(row.name.as_str()).saturating_sub(window)
}

/// The indent level every rendered row is shifted left by: the shallowest indent currently on screen.
/// Scrolling into a deep subtree otherwise spends the whole name column on indentation that is
/// identical on every visible row, so the outermost visible span sits flush against the column and the
/// rows below keep only their offsets *relative* to it.
///
/// Anchored on the shallowest **rendered** row rather than literally the topmost one. They are the
/// same row in the usual case — the pinned block is the cursor's ancestor chain, so it is ordered
/// outermost-first — but a shallower sibling scrolling into the window underneath would otherwise
/// force a negative shift, and clamping that at zero would collapse two different depths onto the same
/// column. Taking the minimum keeps every relative distinction on screen intact.
pub(crate) fn visible_indent_base(rows: &[WaterfallRow], layout: &StickyLayout) -> usize {
    let window = layout.offset..(layout.offset + layout.height).min(rows.len());
    layout
        .pinned
        .iter()
        .copied()
        .chain(window)
        .filter_map(|index| rows.get(index).map(|row| row.indent))
        .min()
        .unwrap_or(0)
}

/// How far the name column scrolls horizontally to reveal the tail of the name under the cursor.
/// `0` when that name already fits — so the column returns home as soon as the selection moves to a
/// short name, which is what makes the scroll feel automatic rather than sticky.
///
/// Every row is then rendered at this offset, clamped to its own [`row_name_offset`], so the column
/// scrolls as one while no row is ever scrolled past its own text into an empty field. That clamp is
/// what keeps the pinned ancestors — the shallowest rows, and so usually the shortest names —
/// readable while the deep rows below them are shifted well to the right.
pub(crate) fn name_offset(rows: &[WaterfallRow], cursor: usize, indent_base: usize) -> usize {
    rows.get(cursor)
        .map_or(0, |row| row_name_offset(row, indent_base))
}

/// The rows of `row`'s ancestors, outermost first.
///
/// Walks [`WaterfallRow::parent_row`] upward, requiring each step to *decrease* the index. That makes
/// the sequence strictly decreasing, so the walk is cycle-safe by construction — a malformed trace
/// whose parent links form a loop terminates rather than hanging — and it yields a chain whose
/// indices are strictly increasing once reversed, which [`sticky_layout`] relies on.
pub(crate) fn ancestor_rows(rows: &[WaterfallRow], row: usize) -> Vec<usize> {
    let mut chain = Vec::new();
    let mut current = row;
    while let Some(parent) = rows.get(current).and_then(|row| row.parent_row) {
        if parent >= current {
            break;
        }
        chain.push(parent);
        current = parent;
    }
    chain.reverse();
    chain
}

/// The waterfall pane's vertical geometry: which ancestor rows stay pinned at the top, and the
/// scrolling window left beneath them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StickyLayout {
    /// Row indices pinned above the scrolling window, outermost first. Always strictly below
    /// `offset`, so a row is never drawn twice and the cursor row is never pinned.
    pub(crate) pinned: Vec<usize>,
    /// First row of the scrolling window.
    pub(crate) offset: usize,
    /// Height (rows) of the scrolling window: the viewport less the pinned rows.
    pub(crate) height: usize,
}

/// Lay the waterfall pane out for `cursor` in a `viewport`-row pane: pin the ancestors of the
/// selected span that have scrolled above the window, so a deep trace never loses its context.
///
/// The pinned block is capped at a third of the viewport, keeping the *innermost* ancestors (the
/// nearest context) when the chain is longer. `enabled == false` pins nothing and reproduces the
/// pane's plain scrolling exactly.
///
/// # Why the cursor is the anchor
///
/// The pane's scroll offset is stateless: a fresh `ListState` each frame means it is `0` while the
/// cursor is within the first screenful and `cursor - height + 1` afterwards, i.e. there is exactly
/// one degree of freedom and the cursor sits on the last visible row whenever the pane has scrolled.
/// So "the ancestors of the topmost visible row" and "the ancestors of the cursor" describe the same
/// pane — but only the latter is a *monotone* function of the pinned count, and monotonicity is what
/// makes the fixpoint below converge.
///
/// Anchoring on the topmost row instead does not converge: with rows `A`, `B` (child of `A`), `C`
/// (child of `B`), `D` (a fresh root) the pinned count cycles `1 → 2 → 0 → 1 → …`, so any iteration
/// budget yields a budget-dependent answer and the block visibly flickers as the user holds `↓`.
///
/// **If the offset ever becomes stateful** — remembered across frames, with the cursor free to sit
/// mid-pane — this anchor has to be revisited, because the two descriptions stop coinciding.
pub(crate) fn sticky_layout(
    rows: &[WaterfallRow],
    cursor: usize,
    viewport: usize,
    enabled: bool,
) -> StickyLayout {
    let viewport = viewport.max(1);
    let cap = if enabled { viewport / 3 } else { 0 };
    let chain = ancestor_rows(rows, cursor);

    // `f(pinned) = min(cap, |{a in chain : a < offset(pinned)}|)` is monotone non-decreasing on the
    // finite lattice `0..=cap`: more pinned rows leave a shorter window, which pushes the offset down
    // the trace, which can only expose more ancestors above it. Iterating from zero therefore reaches
    // the least fixpoint within `cap + 1` steps.
    let mut pinned: Vec<usize> = Vec::new();
    for _ in 0..=cap {
        let height = viewport.saturating_sub(pinned.len()).max(1);
        let offset = if cursor < height {
            0
        } else {
            cursor - height + 1
        };
        // `chain` is sorted ascending, so `take_while` is the same set as a filter — and the tail is
        // the innermost `cap` ancestors, the ones worth keeping when the chain overflows the cap.
        let next: Vec<usize> = chain.iter().copied().take_while(|&a| a < offset).collect();
        let next = next[next.len().saturating_sub(cap)..].to_vec();
        let settled = next.len() == pinned.len();
        pinned = next;
        if settled {
            break;
        }
    }

    // Re-derive the window from the pinned count we settled on. In the stable case this is a no-op;
    // it is here so the two invariants the renderer depends on — the cursor is inside the window, and
    // every pinned row is above it — hold unconditionally, even if the reasoning above is ever broken.
    let height = viewport.saturating_sub(pinned.len()).max(1);
    let offset = if cursor < height {
        0
    } else {
        cursor - height + 1
    };
    pinned.retain(|&row| row < offset);
    StickyLayout {
        height: viewport - pinned.len(),
        pinned,
        offset,
    }
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
        // Alignment must also survive the name column being scrolled horizontally, since that shifts
        // every row's name field and can land a clip marker on half of a wide glyph.
        for cells in [40usize, 77] {
            for offset in [0usize, 1, 5, 9] {
                let lines = render_waterfall(
                    &waterfall,
                    &WaterfallView {
                        bar_cells: cells,
                        name_offset: offset,
                        indent_base: 0,
                    },
                );
                assert_eq!(lines.len(), 2);
                // Everything before the first `|` is a constant width across rows, so the bars line
                // up: marker (1) + name field (WATERFALL_NAME_W == 20).
                let axis = |line: &str| UnicodeWidthStr::width(line.split('|').next().unwrap());
                assert_eq!(axis(&lines[0]), axis(&lines[1]), "offset {offset}");
                assert_eq!(axis(&lines[0]), 1 + 20, "offset {offset}");
                // The bar (between the two `|`) fills exactly `cells` cells, so the closing `|` and
                // the trailing duration column also line up.
                let bar = |line: &str| UnicodeWidthStr::width(line.split('|').nth(1).unwrap());
                assert_eq!(bar(&lines[0]), cells);
                assert_eq!(bar(&lines[1]), cells);
            }
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
        assert_eq!(detail.waterfall.rows[0].marker, ' ');
        assert_eq!(detail.waterfall.rows[0].parent_row, None);

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
        assert_eq!(detail.waterfall.rows[1].indent, 1);
        assert_eq!(detail.waterfall.rows[1].name, "db.query");
        // ...and its `parent_row` indexes the root's row, which is what the sticky walk follows.
        assert_eq!(detail.waterfall.rows[1].parent_row, Some(0));

        // The orphan stays visible, flagged malformed (`!` marker) rather than dropped, and reads as an
        // error so the waterfall row can be coloured. Its parent is not in the trace, so no row link.
        assert!(detail.spans[2].malformed);
        assert!(detail.spans[2].is_error());
        assert_eq!(detail.waterfall.rows[2].marker, '!');
        assert_eq!(detail.waterfall.rows[2].parent_row, None);
    }

    /// A straight chain `0 -> 1 -> ... -> n-1`, each row the child of the one above it.
    fn chain_rows(n: usize) -> Vec<WaterfallRow> {
        (0..n)
            .map(|index| WaterfallRow {
                marker: ' ',
                indent: index.min(WATERFALL_MAX_INDENT),
                name: format!("span-{index}"),
                parent_row: index.checked_sub(1),
                start: 0.0,
                frac: 1.0,
                suffix: String::new(),
            })
            .collect()
    }

    fn row_at_depth(name: &str, indent: usize, parent_row: Option<usize>) -> WaterfallRow {
        WaterfallRow {
            indent,
            ..row_with_parent(name, parent_row)
        }
    }

    fn row_with_parent(name: &str, parent_row: Option<usize>) -> WaterfallRow {
        WaterfallRow {
            marker: ' ',
            indent: 0,
            name: name.to_owned(),
            parent_row,
            start: 0.0,
            frac: 1.0,
            suffix: String::new(),
        }
    }

    #[test]
    fn parent_row_is_ignored_when_the_parent_is_listed_after_its_child() {
        // Rows come back ordered by start time, not as a tree, so clock skew across services can put
        // a parent *below* its child. Such a link must not be walked: it would point downward, and
        // pinning a row that is already on screen (or below the cursor) is nonsense.
        let rows = vec![
            row_with_parent("child", Some(1)),
            row_with_parent("parent", None),
        ];
        assert_eq!(ancestor_rows(&rows, 0), Vec::<usize>::new());
    }

    #[test]
    fn ancestor_rows_survives_a_malformed_parent_cycle() {
        // A self-reference and a 2-cycle: both point at an index that is not strictly above, so the
        // walk stops instead of looping forever.
        let rows = vec![
            row_with_parent("self", Some(0)),
            row_with_parent("a", Some(2)),
            row_with_parent("b", Some(1)),
        ];
        assert_eq!(ancestor_rows(&rows, 0), Vec::<usize>::new());
        assert_eq!(ancestor_rows(&rows, 1), Vec::<usize>::new());
        assert_eq!(ancestor_rows(&rows, 2), vec![1]);
    }

    #[test]
    fn sticky_layout_pins_the_scrolled_off_ancestors() {
        // A 20-deep chain in a 9-row pane with the cursor at the bottom: the window shrinks by the
        // pinned rows, and the pinned rows are the cursor's ancestors that fell above it.
        let rows = chain_rows(20);
        let layout = sticky_layout(&rows, 19, 9, true);
        assert_eq!(layout.height, 9 - layout.pinned.len());
        assert_eq!(layout.offset, 19 - layout.height + 1);
        // Outermost first, and every pinned row is strictly above the scrolling window.
        assert!(layout.pinned.windows(2).all(|w| w[0] < w[1]));
        assert!(layout.pinned.iter().all(|&row| row < layout.offset));
        assert!(layout.offset <= 19 && 19 < layout.offset + layout.height);
    }

    #[test]
    fn sticky_layout_pins_nothing_near_the_top() {
        // While the cursor is within the first screenful nothing has scrolled off, so the pane must
        // render exactly as it did before the feature existed.
        let rows = chain_rows(20);
        let layout = sticky_layout(&rows, 2, 9, true);
        assert_eq!(layout.pinned, Vec::<usize>::new());
        assert_eq!(layout.offset, 0);
        assert_eq!(layout.height, 9);
    }

    #[test]
    fn sticky_layout_caps_pinned_rows_at_a_third_of_the_viewport() {
        // A 12-deep chain would pin 11 ancestors and leave nothing to scroll; the cap keeps the pane
        // usable, and keeps the *innermost* ancestors because those are the nearest context.
        let rows = chain_rows(13);
        let layout = sticky_layout(&rows, 12, 9, true);
        assert_eq!(layout.pinned.len(), 3, "cap is viewport / 3");
        let innermost = *layout.pinned.last().unwrap();
        assert_eq!(layout.pinned, vec![innermost - 2, innermost - 1, innermost]);
        assert!(layout.pinned.iter().all(|&row| row < layout.offset));
    }

    #[test]
    fn sticky_layout_is_inert_when_disabled() {
        // Sticky off must reproduce the plain list scrolling: cursor pinned to the last visible row.
        let rows = chain_rows(20);
        for cursor in 0..20 {
            let layout = sticky_layout(&rows, cursor, 9, false);
            assert_eq!(layout.pinned, Vec::<usize>::new());
            assert_eq!(layout.height, 9);
            assert_eq!(layout.offset, if cursor < 9 { 0 } else { cursor - 8 });
        }
    }

    #[test]
    fn sticky_layout_is_stable_across_a_subtree_boundary() {
        // The counterexample that breaks the obvious formulation: anchoring the pinned set on the
        // *topmost visible row* makes the pinned count cycle 1 -> 2 -> 0 -> 1 ... across this shape,
        // so the block flickers as the cursor advances one row at a time. Anchoring on the cursor is
        // monotone and settles. Assert the invariants the renderer depends on at every cursor.
        let rows = vec![
            row_with_parent("a-root", None),
            row_with_parent("b-child-of-a", Some(0)),
            row_with_parent("c-child-of-b", Some(1)),
            row_with_parent("d-root", None),
            row_with_parent("e-child-of-d", Some(3)),
            row_with_parent("f-child-of-e", Some(4)),
        ];
        for viewport in 1..=8 {
            for cursor in 0..rows.len() {
                let layout = sticky_layout(&rows, cursor, viewport, true);
                assert!(
                    layout.offset <= cursor && cursor < layout.offset + layout.height,
                    "cursor {cursor} hidden at viewport {viewport}: {layout:?}"
                );
                assert!(
                    layout.pinned.iter().all(|&row| row < layout.offset),
                    "pinned row inside the window at viewport {viewport}: {layout:?}"
                );
                assert!(layout.pinned.len() <= viewport / 3);
                assert_eq!(layout.height, viewport - layout.pinned.len());
            }
        }
    }

    #[test]
    fn sticky_layout_handles_a_degenerate_viewport() {
        // A pane too short to spare a row for context still has to render something.
        let rows = chain_rows(20);
        for viewport in [0usize, 1, 2] {
            let layout = sticky_layout(&rows, 19, viewport, true);
            assert_eq!(layout.pinned, Vec::<usize>::new(), "viewport {viewport}");
            assert!(layout.height >= 1);
            assert!(layout.offset <= 19 && 19 < layout.offset + layout.height);
        }
    }

    #[test]
    fn the_indent_cap_keeps_a_deep_span_name_readable() {
        // Two cells of indent per level would leave four name cells at depth 8; the cap holds the
        // indent at WATERFALL_MAX_INDENT levels so the name always has room.
        let mut spans = vec![waterfall_span(1, None, "root", 0, 1_000_000)];
        for id in 2..=12u8 {
            spans.push(waterfall_span(
                id,
                Some(id - 1),
                "db.query.orders",
                0,
                500_000,
            ));
        }
        let trace = imbh::Trace {
            trace_id: TraceId([0xaa; 16]),
            root_service: None,
            root_name: None,
            start_time: Timestamp(0),
            duration_ns: imbh::DurationNs(1_000_000),
            spans,
        };
        let rows = build_trace_detail(&trace, true).waterfall.rows;
        // Depth 0..=WATERFALL_MAX_INDENT indents exactly as it always has.
        for (depth, row) in rows.iter().enumerate().take(WATERFALL_MAX_INDENT + 1) {
            assert_eq!(row.indent, depth);
        }
        // Past the cap every row shares the deepest indent, leaving >= 10 cells for the name.
        let last = rows.len() - 1;
        assert_eq!(rows[last].indent, WATERFALL_MAX_INDENT);
        assert!(WATERFALL_NAME_W - rows[last].indent * 2 >= 10);
        // ...and the parent chain is still fully walkable however deep the trace goes, so the pinned
        // ancestors carry the hierarchy the capped indent stops showing.
        assert_eq!(ancestor_rows(&rows, last), (0..last).collect::<Vec<_>>());
    }

    #[test]
    fn the_indent_is_relative_to_the_shallowest_row_on_screen() {
        // A 12-deep chain: scrolled to the bottom, every visible row sits at the capped indent, so an
        // absolute indent would spend ten cells of every row's name column saying the same thing.
        let rows = chain_rows(13);
        let layout = sticky_layout(&rows, 12, 9, true);
        let base = visible_indent_base(&rows, &layout);
        // The shallowest row on screen is the outermost pinned ancestor...
        let shallowest = layout
            .pinned
            .iter()
            .chain(
                (layout.offset..layout.offset + layout.height)
                    .collect::<Vec<_>>()
                    .iter(),
            )
            .map(|&i| rows[i].indent)
            .min()
            .unwrap();
        assert_eq!(base, shallowest);
        // ...and it renders flush against the name column, with the rows below keeping their offsets
        // relative to it rather than their absolute depth.
        assert_eq!(rows[layout.pinned[0]].indent.saturating_sub(base), 0);

        // Nothing is shifted while the pane is at the top, so an unscrolled trace looks unchanged.
        let top = sticky_layout(&rows, 0, 9, true);
        assert_eq!(visible_indent_base(&rows, &top), 0);
    }

    #[test]
    fn the_indent_base_is_the_shallowest_row_not_merely_the_first() {
        // A deep row followed by a shallower sibling scrolling in underneath it. Anchoring literally on
        // the topmost row would need a negative shift for the sibling; clamping that at zero would draw
        // two different depths in the same column. The minimum keeps them distinct.
        let rows = vec![
            row_at_depth("deep", 4, None),
            row_at_depth("deeper", 5, Some(0)),
            row_at_depth("shallow-sibling", 1, None),
        ];
        let layout = StickyLayout {
            pinned: vec![],
            offset: 0,
            height: 3,
        };
        let base = visible_indent_base(&rows, &layout);
        assert_eq!(base, 1, "the shallowest rendered row anchors the column");
        let indents = rows
            .iter()
            .map(|row| row.indent.saturating_sub(base))
            .collect::<Vec<_>>();
        assert_eq!(indents, vec![3, 4, 0], "relative depths stay distinct");
    }

    #[test]
    fn the_relative_indent_hands_cells_back_to_the_name() {
        // The point of the shift: a deep row's name gets the cells the shared indent was consuming.
        let rows = vec![row_at_depth("db.query.orders-by-customer", 5, None)];
        let waterfall = Waterfall {
            rows,
            marker: '#',
            light_marker: '-',
        };
        let absolute = render_waterfall(
            &waterfall,
            &WaterfallView {
                bar_cells: 10,
                ..WaterfallView::default()
            },
        );
        let relative = render_waterfall(
            &waterfall,
            &WaterfallView {
                bar_cells: 10,
                indent_base: 5,
                ..WaterfallView::default()
            },
        );
        // Absolute: 10 cells of indent leave 10 for the name, so it truncates hard.
        assert!(
            absolute[0].starts_with("           db.query"),
            "{:?}",
            absolute[0]
        );
        // Relative: the whole 20-cell column is the name.
        assert!(
            relative[0].starts_with(" db.query.orders-by-"),
            "{:?}",
            relative[0]
        );
        // The axis is unmoved either way — the shift happens inside the fixed-width column.
        let axis = |line: &str| UnicodeWidthStr::width(line.split('|').next().unwrap());
        assert_eq!(axis(&absolute[0]), axis(&relative[0]));
    }

    #[test]
    fn name_offset_follows_the_cursor_row() {
        let rows = vec![
            row_with_parent("short", None),
            row_with_parent("a-very-long-span-name-indeed", Some(0)),
        ];
        // A name that fits leaves the column unscrolled.
        assert_eq!(name_offset(&rows, 0, 0), 0);
        // A longer one scrolls just enough to bring its tail to the field's last cell: the whole
        // field is text apart from the `<` marker, with nothing wasted on padding.
        let offset = name_offset(&rows, 1, 0);
        assert_eq!(
            offset,
            "a-very-long-span-name-indeed".len() - WATERFALL_NAME_W
        );
        let field = crate::format::clip_field(&rows[1].name, WATERFALL_NAME_W, offset);
        assert!(field.starts_with('<'), "{field:?}");
        assert!(field.ends_with("indeed"), "{field:?}");
        // An out-of-range cursor is a no-op rather than a panic.
        assert_eq!(name_offset(&rows, 99, 0), 0);
    }

    #[test]
    fn the_indent_never_scrolls_away_under_a_long_name() {
        // The indent is the only thing showing the trace's shape, so scrolling the column to read one
        // long name must not take the tree with it: every row keeps its leading indent, and the name
        // scrolls inside whatever the indent leaves.
        let mut spans = vec![waterfall_span(1, None, "root", 0, 1_000_000)];
        for id in 2..=5u8 {
            spans.push(waterfall_span(
                id,
                Some(id - 1),
                "db.query.orders-by-customer-id",
                0,
                500_000,
            ));
        }
        let waterfall = build_trace_detail(
            &imbh::Trace {
                trace_id: TraceId([0xaa; 16]),
                root_service: None,
                root_name: None,
                start_time: Timestamp(0),
                duration_ns: imbh::DurationNs(1_000_000),
                spans,
            },
            true,
        )
        .waterfall;
        // Scrolled hard right by the deepest row's long name...
        let offset = name_offset(&waterfall.rows, 4, 0);
        assert!(offset > 0);
        let lines = render_waterfall(
            &waterfall,
            &WaterfallView {
                bar_cells: 20,
                name_offset: offset,
                indent_base: 0,
            },
        );
        // ...the indent of each row still steps by two cells after the status marker.
        for (depth, line) in lines.iter().enumerate() {
            let field = &line[1..line.find('|').unwrap()];
            assert_eq!(
                field.len() - field.trim_start().len(),
                depth * 2,
                "row {depth} lost its indent: {line:?}"
            );
        }
        // The root's short name fits, so it is not scrolled into blankness by the column's offset.
        assert!(lines[0].contains("root"), "{:?}", lines[0]);
    }
}

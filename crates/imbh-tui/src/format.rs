//! Rendering values as display text: field padding, metric values, severity bands, and attributes.

use imbh::{AnyValue, Attributes, SeverityNumber};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Left-align a horizontally scrolled window of `text` into a field that is exactly `width`
/// *terminal cells* wide, showing the text from `offset` cells in. Honors East Asian width: wide
/// glyphs (CJK, etc.) count as two cells, so the window is measured by display width rather than by
/// `char` count, and a wide glyph straddling either edge is dropped in favour of a pad space so the
/// total stays exact.
///
/// Clipping is marked in-band: a `<` in the first cell means text is hidden to the left, a `>` in
/// the last cell means text is hidden to the right. Both *overwrite* an edge cell rather than
/// stealing width, so the field is always exactly `width` cells — which is what keeps the
/// waterfall's `|bar|` axis aligned across rows of any depth. Both markers are ASCII, so this is
/// also what keeps a truncated name from leaking a non-ASCII glyph in `--ascii` mode.
pub(crate) fn clip_field(text: &str, width: usize, offset: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let total = UnicodeWidthStr::width(text);
    // Walk the text once, keeping the cells that fall inside the `[offset, offset + width)` window.
    // A glyph straddling either edge is dropped and its cells are padded, so `used` stays exact.
    let mut out = String::new();
    let mut at = 0usize; // cells consumed from `text` so far
    let mut used = 0usize; // cells written into the window so far
    for ch in text.chars() {
        let w = UnicodeWidthChar::width(ch).unwrap_or(0);
        if at + w > offset + width {
            break;
        }
        if at >= offset {
            out.push(ch);
            used += w;
        } else if at + w > offset {
            // A wide glyph straddling the left edge: pad the cells that fall inside the window.
            let inside = at + w - offset;
            out.extend(std::iter::repeat_n(' ', inside));
            used += inside;
        }
        at += w;
    }
    out.extend(std::iter::repeat_n(' ', width - used));

    // Overwrite the edge cells with the clip markers, replacing whatever glyph sits there. Done on
    // the char sequence (not by byte index) so a multi-byte glyph at an edge is replaced whole.
    let hidden_left = offset > 0 && total > 0;
    let hidden_right = total > offset + width;
    if !hidden_left && !hidden_right {
        return out;
    }
    let mut cells: Vec<String> = Vec::with_capacity(width);
    for ch in out.chars() {
        let w = UnicodeWidthChar::width(ch).unwrap_or(0);
        cells.push(ch.to_string());
        // A wide glyph owns two cells; the trailing one is a placeholder we can drop if a marker
        // lands on it, in which case the glyph itself is replaced by a pad space.
        for _ in 1..w {
            cells.push(String::new());
        }
    }
    let mark = |cells: &mut Vec<String>, index: usize, marker: char| {
        // Walking back to the owning glyph keeps the field exact when a marker lands on the second
        // half of a wide glyph: the glyph goes, a marker plus a pad space take its two cells.
        let mut owner = index;
        while owner > 0 && cells[owner].is_empty() {
            owner -= 1;
        }
        let span = 1 + cells[owner + 1..]
            .iter()
            .take_while(|c| c.is_empty())
            .count();
        cells[owner] = if owner == index {
            marker.to_string()
        } else {
            " ".to_owned()
        };
        for (slot, cell) in cells[owner + 1..owner + span].iter_mut().enumerate() {
            *cell = if owner + 1 + slot == index {
                marker.to_string()
            } else {
                " ".to_owned()
            };
        }
    };
    if hidden_left {
        mark(&mut cells, 0, '<');
    }
    if hidden_right {
        mark(&mut cells, width - 1, '>');
    }
    cells.concat()
}

/// Compact display of a metric value: integers without a fractional part, non-integers to 4 dp, and
/// explicit `NaN`/`+Inf`/`-Inf` rather than Rust's default `inf`.
pub(crate) fn format_metric_value(value: f64) -> String {
    if value.is_nan() {
        "NaN".to_owned()
    } else if value.is_infinite() {
        if value > 0.0 { "+Inf" } else { "-Inf" }.to_owned()
    } else if value == value.trunc() && value.abs() < 1e15 {
        format!("{value:.0}")
    } else {
        format!("{value:.4}")
    }
}

/// OTel severity number to a `BAND (n)` label (e.g. `INFO (9)`).
pub(crate) fn severity_label(severity: SeverityNumber) -> String {
    let band = match severity.0 {
        0 => "UNSET",
        1..=4 => "TRACE",
        5..=8 => "DEBUG",
        9..=12 => "INFO",
        13..=16 => "WARN",
        17..=20 => "ERROR",
        _ => "FATAL",
    };
    format!("{band} ({})", severity.0)
}

/// Render an attribute value as a single display string (arrays/maps compacted, bytes as hex).
pub(crate) fn render_value(value: &AnyValue) -> String {
    match value {
        AnyValue::Null => "null".to_owned(),
        AnyValue::Str(text) => text.clone(),
        AnyValue::Int(int) => int.to_string(),
        AnyValue::Double(double) => double.to_string(),
        AnyValue::Bool(boolean) => boolean.to_string(),
        AnyValue::Bytes(bytes) => {
            let mut out = String::with_capacity(2 + bytes.len() * 2);
            out.push_str("0x");
            for byte in bytes {
                out.push_str(&format!("{byte:02x}"));
            }
            out
        }
        AnyValue::Array(items) => {
            let inner = items
                .iter()
                .map(render_value)
                .collect::<Vec<_>>()
                .join(", ");
            format!("[{inner}]")
        }
        AnyValue::Map(entries) => {
            let inner = entries
                .iter()
                .map(|(key, value)| format!("{key}: {}", render_value(value)))
                .collect::<Vec<_>>()
                .join(", ");
            format!("{{{inner}}}")
        }
    }
}

/// Flatten an attribute map into displayable `(key, value)` pairs.
pub(crate) fn attrs_to_pairs(attributes: &Attributes) -> Vec<(String, String)> {
    attributes
        .iter()
        .map(|(key, value)| (key.to_owned(), render_value(value)))
        .collect()
}

/// Approximate the number of terminal rows a logical line occupies once wrapped to `width` columns.
/// Uses the character count (not display width), which is an adequate estimate for clamping the
/// result-pane scroll; an off-by-a-row on unusually wide glyphs is harmless.
pub(crate) fn wrapped_rows(line: &str, width: u16) -> u32 {
    if width == 0 {
        return 1;
    }
    (line.chars().count().max(1) as u32).div_ceil(width as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clip_field_pads_and_truncates_by_display_width() {
        // ASCII: padded to the exact cell count, no markers when nothing is hidden.
        assert_eq!(clip_field("ab", 5, 0), "ab   ");
        assert_eq!(UnicodeWidthStr::width(clip_field("ab", 5, 0).as_str()), 5);
        // Wide (CJK) glyphs count as two cells each: 3 chars == 6 cells, padded to 8.
        let jp = clip_field("あいう", 8, 0);
        assert_eq!(UnicodeWidthStr::width(jp.as_str()), 8);
        assert!(jp.starts_with("あいう"));
        // Truncation keeps the field exactly `width` cells including the marker, never over. A wide
        // glyph straddling the right boundary is dropped and its cells are space-padded.
        let cut = clip_field("あいうえお", 6, 0);
        assert_eq!(UnicodeWidthStr::width(cut.as_str()), 6);
        assert!(cut.ends_with('>'), "{cut:?}");
        // An odd width makes the marker land on the second half of a wide glyph: the glyph goes and
        // a pad space plus the marker take its two cells, keeping the field exact.
        let cut_odd = clip_field("あいうえお", 5, 0);
        assert_eq!(UnicodeWidthStr::width(cut_odd.as_str()), 5);
        assert!(cut_odd.ends_with('>'), "{cut_odd:?}");
    }

    #[test]
    fn clip_field_scrolls_horizontally_and_marks_both_edges() {
        // Scrolled into the middle: both edges hide text, so both markers show.
        let mid = clip_field("abcdefghij", 5, 3);
        assert_eq!(mid, "<efg>");
        // Scrolled to the tail: nothing hidden on the right, so only `<`.
        let tail = clip_field("abcdefghij", 5, 5);
        assert_eq!(tail, "<ghij");
        // Offset past the end of the text: an honest all-blank window that still marks the left.
        let past = clip_field("abc", 4, 9);
        assert_eq!(past, "<   ");
        // A zero offset over text that fits leaves the field marker-free.
        assert_eq!(clip_field("abc", 4, 0), "abc ");

        // The field is exactly `width` cells at every offset, for wide glyphs too, and a marker only
        // ever claims an edge cell — the invariant the waterfall's bar alignment rests on.
        for text in ["abcdefghij", "あいうえお", "aあbいc"] {
            for width in 1..=8usize {
                for offset in 0..12usize {
                    let out = clip_field(text, width, offset);
                    assert_eq!(
                        UnicodeWidthStr::width(out.as_str()),
                        width,
                        "clip_field({text:?}, {width}, {offset}) == {out:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn clip_field_marks_an_edge_only_when_text_is_really_hidden() {
        // Text exactly filling the window: no marker on either side.
        assert_eq!(clip_field("abcde", 5, 0), "abcde");
        // One cell over on the right only.
        assert_eq!(clip_field("abcdef", 5, 0), "abcd>");
        // Empty text never claims to hide anything, whatever the offset.
        assert_eq!(clip_field("", 3, 4), "   ");
    }

    #[test]
    fn metric_values_format_compactly() {
        assert_eq!(format_metric_value(42.0), "42");
        assert_eq!(format_metric_value(2.53125), "2.5312");
        assert_eq!(format_metric_value(f64::NAN), "NaN");
        assert_eq!(format_metric_value(f64::INFINITY), "+Inf");
        assert_eq!(format_metric_value(f64::NEG_INFINITY), "-Inf");
    }

    #[test]
    fn severity_labels_map_number_bands() {
        assert_eq!(severity_label(SeverityNumber(9)), "INFO (9)");
        assert_eq!(severity_label(SeverityNumber(17)), "ERROR (17)");
        assert_eq!(severity_label(SeverityNumber(0)), "UNSET (0)");
    }

    #[test]
    fn wrapped_rows_accounts_for_width() {
        assert_eq!(wrapped_rows("", 10), 1); // empty line still occupies a row
        assert_eq!(wrapped_rows("hello", 10), 1);
        assert_eq!(wrapped_rows("0123456789abc", 10), 2); // 13 chars over 10 cols -> 2 rows
        assert_eq!(wrapped_rows("anything", 0), 1); // zero width degrades gracefully
    }
}

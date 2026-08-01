//! Rendering values as display text: field padding, metric values, severity bands, and attributes.

use imbh::{AnyValue, Attributes, SeverityNumber};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Left-align `text` into a field that is exactly `width` *terminal cells* wide, padding when it fits
/// and truncating it with a trailing `ellipsis` when it does not. Honors East Asian width: wide
/// glyphs (CJK, etc.) count as two cells, so the field is measured by display width rather than by
/// `char` count, and a wide glyph straddling the cut is dropped in favour of a pad space so the total
/// stays exact.
///
/// The `ellipsis` is *reserved out of* the field rather than overwriting its last cell, so the result
/// is always exactly `width` cells — which is what keeps the waterfall's `|bar|` axis aligned across
/// rows. Pass the caller's mode-appropriate marker ([`Glyphs::ellipsis`](crate::ui::glyphs::Glyphs),
/// `"..."` in `--ascii` mode) so a truncated name cannot leak a non-ASCII glyph.
///
/// A field too narrow to hold the marker at all drops it: an exact-width field matters more than
/// announcing the truncation, and a field that is *all* marker says nothing anyway.
pub(crate) fn fit_field(text: &str, width: usize, ellipsis: &str) -> String {
    if width == 0 {
        return String::new();
    }
    let total = UnicodeWidthStr::width(text);
    if total <= width {
        let mut out = text.to_owned();
        out.extend(std::iter::repeat_n(' ', width - total));
        return out;
    }
    let marker = UnicodeWidthStr::width(ellipsis);
    let (keep, marker) = if marker < width {
        (width - marker, marker)
    } else {
        (width, 0)
    };
    let mut out = String::new();
    let mut used = 0usize; // cells written so far
    for ch in text.chars() {
        let w = UnicodeWidthChar::width(ch).unwrap_or(0);
        if used + w > keep {
            break;
        }
        out.push(ch);
        used += w;
    }
    // A wide glyph straddling the cut is dropped whole, so pad the single cell it would have half
    // filled — dropping it silently would leave the field a cell short and shift the axis.
    out.extend(std::iter::repeat_n(' ', keep - used));
    if marker > 0 {
        out.push_str(ellipsis);
    }
    out
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
    fn fit_field_pads_and_truncates_by_display_width() {
        // ASCII: padded to the exact cell count, no marker when nothing is hidden.
        assert_eq!(fit_field("ab", 5, "…"), "ab   ");
        assert_eq!(fit_field("abcde", 5, "…"), "abcde", "an exact fit is bare");
        // Wide (CJK) glyphs count as two cells each: 3 chars == 6 cells, padded to 8.
        let jp = fit_field("あいう", 8, "…");
        assert_eq!(UnicodeWidthStr::width(jp.as_str()), 8);
        assert!(jp.starts_with("あいう"));
        // Truncation keeps the field exactly `width` cells including the marker, never over.
        assert_eq!(fit_field("abcdef", 5, "…"), "abcd…");
        assert_eq!(fit_field("abcdefgh", 5, "..."), "ab...");
        // A wide glyph straddling the cut is dropped whole and its orphaned cell padded.
        let cut = fit_field("あいうえお", 6, "…");
        assert_eq!(UnicodeWidthStr::width(cut.as_str()), 6);
        assert_eq!(cut, "あい …", "the odd cell is padded, not half a glyph");
    }

    #[test]
    fn fit_field_is_exactly_width_cells_for_every_input() {
        // The invariant the waterfall's bar alignment rests on, over narrow fields, wide glyphs, and
        // both markers — including widths too small to hold the marker at all, where it is dropped.
        for text in ["", "abcdefghij", "あいうえお", "aあbいc"] {
            for width in 0..=8usize {
                for ellipsis in ["…", "..."] {
                    let out = fit_field(text, width, ellipsis);
                    assert_eq!(
                        UnicodeWidthStr::width(out.as_str()),
                        width,
                        "fit_field({text:?}, {width}, {ellipsis:?}) == {out:?}"
                    );
                }
            }
        }
        // Too narrow for the marker: the text is cut bare rather than replaced by a marker that
        // would fill the field and say nothing.
        assert_eq!(fit_field("abcdef", 3, "..."), "abc");
        // Empty text never claims to hide anything.
        assert_eq!(fit_field("", 3, "…"), "   ");
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

//! Rendering values as display text: field padding, metric values, severity bands, and attributes.

use imbh::{AnyValue, Attributes, SeverityNumber};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Left-align `text` into a field that is exactly `width` *terminal cells* wide, honoring East Asian
/// width: wide glyphs (CJK, etc.) count as two cells, so the field pads/truncates by display width
/// rather than by `char` count. Short strings are space-padded; long ones are truncated with a
/// trailing `…`. If a wide glyph would straddle the boundary, an extra space keeps the total exact.
/// Used to keep the waterfall's name column a constant width so the `|bar|` axis stays aligned.
pub(crate) fn clamp_field(text: &str, width: usize) -> String {
    let total = UnicodeWidthStr::width(text);
    if total <= width {
        let mut out = String::from(text);
        out.extend(std::iter::repeat_n(' ', width - total));
        return out;
    }
    // Truncate to leave one cell for the ellipsis, stopping before a glyph would overflow.
    let budget = width.saturating_sub(1);
    let mut out = String::new();
    let mut used = 0usize;
    for ch in text.chars() {
        let w = UnicodeWidthChar::width(ch).unwrap_or(0);
        if used + w > budget {
            break;
        }
        out.push(ch);
        used += w;
    }
    out.push('…');
    // A wide glyph landing on an odd boundary can leave the field one cell short; pad it out.
    out.extend(std::iter::repeat_n(' ', width.saturating_sub(used + 1)));
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
    fn clamp_field_pads_and_truncates_by_display_width() {
        // ASCII: padded to the exact cell count.
        assert_eq!(clamp_field("ab", 5), "ab   ");
        assert_eq!(UnicodeWidthStr::width(clamp_field("ab", 5).as_str()), 5);
        // Wide (CJK) glyphs count as two cells each: 3 chars == 6 cells, padded to 8.
        let jp = clamp_field("あいう", 8);
        assert_eq!(UnicodeWidthStr::width(jp.as_str()), 8);
        assert!(jp.starts_with("あいう"));
        // Truncation keeps the field exactly `width` cells including the ellipsis, never over. A wide
        // glyph straddling the boundary is dropped and the leftover cell is space-padded, so the
        // ellipsis is present but may be followed by a pad space rather than ending the string.
        let cut = clamp_field("あいうえお", 6);
        assert_eq!(UnicodeWidthStr::width(cut.as_str()), 6);
        assert!(cut.contains('…'));
        // An odd width leaves room for the ellipsis right at the end (no straddle).
        let cut_odd = clamp_field("あいうえお", 5);
        assert_eq!(UnicodeWidthStr::width(cut_odd.as_str()), 5);
        assert!(cut_odd.ends_with('…'));
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

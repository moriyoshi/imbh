//! The single shared canonical-JSON encoder (ARCHITECTURE.md §6.1).
//!
//! This is the invariant dictionary encoding relies on: equal attribute maps **must**
//! serialize byte-identically. The spec:
//!
//! - UTF-8 output.
//! - Object keys sorted by Unicode code point (Rust's `str` `Ord` is byte order over
//!   valid UTF-8, which equals code-point order).
//! - No insignificant whitespace.
//! - Integers as minimal decimal.
//! - Doubles via shortest round-trip decimal (Rust's `f64` `Display` guarantees the
//!   shortest string that round-trips; it never uses scientific notation).
//! - `bytes` → base64 (standard alphabet, padded) as a JSON string.
//! - Non-finite doubles (NaN/±Inf, which OTel permits and JSON forbids) → the reserved
//!   sentinel object `{"$f":"nan"|"inf"|"-inf"}`.
//! - Nested arrays/maps recursively canonicalized.
//!
//! The same function backs the segment writer (dict-equality), the Tantivy JSON feeder
//! (§8), and the `json_get_*` UDFs (§9.3) so all three agree byte-for-byte.

use crate::value::AnyValue;

/// Canonicalize an ordered attribute map into a JSON object string.
pub fn canonical_json_object(entries: &[(String, AnyValue)]) -> String {
    let mut out = String::new();
    encode_object(&mut out, entries);
    out
}

/// Canonicalize a single [`AnyValue`] into its JSON text form.
pub fn canonical_json_value(v: &AnyValue) -> String {
    let mut out = String::new();
    encode_value(&mut out, v);
    out
}

fn encode_object(out: &mut String, entries: &[(String, AnyValue)]) {
    // Sort by key code point. Stable sort keeps the last-writer order for duplicate keys,
    // which OTel maps should not contain anyway.
    let mut refs: Vec<&(String, AnyValue)> = entries.iter().collect();
    refs.sort_by(|a, b| a.0.cmp(&b.0));
    out.push('{');
    for (i, (k, v)) in refs.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        encode_string(out, k);
        out.push(':');
        encode_value(out, v);
    }
    out.push('}');
}

fn encode_value(out: &mut String, v: &AnyValue) {
    match v {
        AnyValue::Null => out.push_str("null"),
        AnyValue::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        AnyValue::Int(i) => {
            use std::fmt::Write as _;
            let _ = write!(out, "{i}");
        }
        AnyValue::Double(d) => encode_double(out, *d),
        AnyValue::Str(s) => encode_string(out, s),
        AnyValue::Bytes(b) => {
            out.push('"');
            base64_into(out, b);
            out.push('"');
        }
        AnyValue::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                encode_value(out, item);
            }
            out.push(']');
        }
        AnyValue::Map(entries) => encode_object(out, entries),
    }
}

fn encode_double(out: &mut String, d: f64) {
    if d.is_nan() {
        out.push_str("{\"$f\":\"nan\"}");
    } else if d.is_infinite() {
        out.push_str(if d.is_sign_negative() {
            "{\"$f\":\"-inf\"}"
        } else {
            "{\"$f\":\"inf\"}"
        });
    } else {
        // Rust's f64 Display is shortest-round-trip decimal, no scientific notation.
        use std::fmt::Write as _;
        let _ = write!(out, "{d}");
    }
}

fn encode_string(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => {
                use std::fmt::Write as _;
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Standard base64 (RFC 4648) with padding, written directly into `out`.
fn base64_into(out: &mut String, data: &[u8]) {
    const ALPHA: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    for chunk in data.chunks(3) {
        let b0 = chunk[0];
        let b1 = chunk.get(1).copied().unwrap_or(0);
        let b2 = chunk.get(2).copied().unwrap_or(0);
        let n = ((b0 as u32) << 16) | ((b1 as u32) << 8) | (b2 as u32);
        out.push(ALPHA[((n >> 18) & 0x3f) as usize] as char);
        out.push(ALPHA[((n >> 12) & 0x3f) as usize] as char);
        out.push(if chunk.len() > 1 {
            ALPHA[((n >> 6) & 0x3f) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHA[(n & 0x3f) as usize] as char
        } else {
            '='
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(pairs: Vec<(&str, AnyValue)>) -> Vec<(String, AnyValue)> {
        pairs.into_iter().map(|(k, v)| (k.to_owned(), v)).collect()
    }

    #[test]
    fn key_order_is_invariant() {
        // canon(map) == canon(shuffle(map)) across the value-type matrix (ARCHITECTURE.md §6.1).
        let a = m(vec![
            ("z", AnyValue::Str("s".into())),
            ("a", AnyValue::Int(-7)),
            ("m", AnyValue::Double(1.5)),
            ("b", AnyValue::Bool(true)),
            ("n", AnyValue::Null),
            ("y", AnyValue::Bytes(vec![0, 1, 2, 253, 254, 255])),
            (
                "arr",
                AnyValue::Array(vec![AnyValue::Int(1), AnyValue::Str("x".into())]),
            ),
            (
                "nest",
                AnyValue::Map(m(vec![("q", AnyValue::Int(2)), ("p", AnyValue::Int(1))])),
            ),
        ]);
        let mut b = a.clone();
        b.reverse();
        assert_eq!(canonical_json_object(&a), canonical_json_object(&b));
    }

    #[test]
    fn nested_map_is_sorted() {
        let v = canonical_json_object(&m(vec![(
            "nest",
            AnyValue::Map(m(vec![("q", AnyValue::Int(2)), ("p", AnyValue::Int(1))])),
        )]));
        assert_eq!(v, r#"{"nest":{"p":1,"q":2}}"#);
    }

    #[test]
    fn non_finite_doubles_use_sentinel() {
        assert_eq!(
            canonical_json_value(&AnyValue::Double(f64::NAN)),
            r#"{"$f":"nan"}"#
        );
        assert_eq!(
            canonical_json_value(&AnyValue::Double(f64::INFINITY)),
            r#"{"$f":"inf"}"#
        );
        assert_eq!(
            canonical_json_value(&AnyValue::Double(f64::NEG_INFINITY)),
            r#"{"$f":"-inf"}"#
        );
    }

    #[test]
    fn integer_valued_double_round_trips() {
        assert_eq!(canonical_json_value(&AnyValue::Double(1.0)), "1");
        assert_eq!(canonical_json_value(&AnyValue::Double(-0.5)), "-0.5");
    }

    #[test]
    fn strings_escape_minimally() {
        assert_eq!(
            canonical_json_value(&AnyValue::Str("a\"b\\c\n\u{01}é".into())),
            "\"a\\\"b\\\\c\\n\\u0001é\""
        );
    }

    #[test]
    fn base64_matches_rfc4648() {
        assert_eq!(
            canonical_json_value(&AnyValue::Bytes(b"foobar".to_vec())),
            r#""Zm9vYmFy""#
        );
        assert_eq!(
            canonical_json_value(&AnyValue::Bytes(b"fo".to_vec())),
            r#""Zm8=""#
        );
        assert_eq!(
            canonical_json_value(&AnyValue::Bytes(b"f".to_vec())),
            r#""Zg==""#
        );
    }
}

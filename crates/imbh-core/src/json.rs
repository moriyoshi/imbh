//! A minimal, dependency-free JSON parser — the inverse of the canonical encoder (§6.1).
//!
//! It exists so DTOs and the `json_get_*` UDFs can read attribute values back out of the
//! canonical-JSON columns without pulling `serde_json` into core (ARCHITECTURE.md §10.4). It parses the
//! canonical subset plus ordinary JSON: objects, arrays, strings (with `\uXXXX`), int/float
//! numbers, booleans, null, and the non-finite sentinel `{"$f":"nan|inf|-inf"}` → `Double`.
//!
//! Limitation: `\uXXXX` surrogate pairs are not recombined (the canonical encoder never emits
//! them — it only escapes control characters below 0x20). Base64 `bytes` come back as `Str`,
//! since a JSON string carries no type tag.

use crate::value::AnyValue;

/// Maximum object/array nesting depth. Guards against a stack overflow on pathologically nested
/// input; far deeper than any real attribute document (OTLP itself is prost-recursion-limited).
const MAX_DEPTH: usize = 128;

/// Parse a complete JSON document into an [`AnyValue`]. Returns `None` on malformed input (including
/// nesting deeper than [`MAX_DEPTH`]).
pub fn parse(input: &str) -> Option<AnyValue> {
    let mut p = Parser {
        b: input.as_bytes(),
        i: 0,
        depth: 0,
    };
    p.skip_ws();
    let v = p.value()?;
    p.skip_ws();
    if p.i == p.b.len() { Some(v) } else { None }
}

/// Parse a JSON object into ordered key/value pairs; `None` if the input is not an object.
pub fn parse_object(input: &str) -> Option<Vec<(String, AnyValue)>> {
    match parse(input)? {
        AnyValue::Map(pairs) => Some(pairs),
        _ => None,
    }
}

struct Parser<'a> {
    b: &'a [u8],
    i: usize,
    depth: usize,
}

impl Parser<'_> {
    fn skip_ws(&mut self) {
        while let Some(&c) = self.b.get(self.i) {
            if c == b' ' || c == b'\t' || c == b'\n' || c == b'\r' {
                self.i += 1;
            } else {
                break;
            }
        }
    }

    fn value(&mut self) -> Option<AnyValue> {
        self.skip_ws();
        match self.b.get(self.i)? {
            // Containers recurse; bound the nesting at one place to guard the stack.
            c @ (b'{' | b'[') => {
                self.depth += 1;
                if self.depth > MAX_DEPTH {
                    return None;
                }
                let v = if *c == b'{' {
                    self.object()
                } else {
                    self.array()
                };
                self.depth -= 1;
                v
            }
            b'"' => self.string().map(AnyValue::Str),
            b't' | b'f' => self.boolean(),
            b'n' => self.null(),
            _ => self.number(),
        }
    }

    fn object(&mut self) -> Option<AnyValue> {
        self.i += 1; // '{'
        let mut pairs = Vec::new();
        self.skip_ws();
        if self.b.get(self.i) == Some(&b'}') {
            self.i += 1;
            return Some(AnyValue::Map(pairs));
        }
        loop {
            self.skip_ws();
            let key = self.string()?;
            self.skip_ws();
            if self.b.get(self.i) != Some(&b':') {
                return None;
            }
            self.i += 1;
            let val = self.value()?;
            pairs.push((key, val));
            self.skip_ws();
            match self.b.get(self.i) {
                Some(&b',') => self.i += 1,
                Some(&b'}') => {
                    self.i += 1;
                    return Some(sentinel_or_map(pairs));
                }
                _ => return None,
            }
        }
    }

    fn array(&mut self) -> Option<AnyValue> {
        self.i += 1; // '['
        let mut items = Vec::new();
        self.skip_ws();
        if self.b.get(self.i) == Some(&b']') {
            self.i += 1;
            return Some(AnyValue::Array(items));
        }
        loop {
            let v = self.value()?;
            items.push(v);
            self.skip_ws();
            match self.b.get(self.i) {
                Some(&b',') => self.i += 1,
                Some(&b']') => {
                    self.i += 1;
                    return Some(AnyValue::Array(items));
                }
                _ => return None,
            }
        }
    }

    fn string(&mut self) -> Option<String> {
        if self.b.get(self.i) != Some(&b'"') {
            return None;
        }
        self.i += 1;
        let mut out = String::new();
        loop {
            let c = *self.b.get(self.i)?;
            self.i += 1;
            match c {
                b'"' => return Some(out),
                b'\\' => {
                    let e = *self.b.get(self.i)?;
                    self.i += 1;
                    match e {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'b' => out.push('\u{08}'),
                        b'f' => out.push('\u{0c}'),
                        b'u' => {
                            let cp = self.hex4()?;
                            out.push(char::from_u32(cp as u32)?);
                        }
                        _ => return None,
                    }
                }
                // A UTF-8 continuation/lead byte: copy the whole code point verbatim.
                _ => {
                    let start = self.i - 1;
                    let len = utf8_len(c)?;
                    let end = start + len;
                    let s = std::str::from_utf8(self.b.get(start..end)?).ok()?;
                    out.push_str(s);
                    self.i = end;
                }
            }
        }
    }

    fn hex4(&mut self) -> Option<u16> {
        let slice = self.b.get(self.i..self.i + 4)?;
        let s = std::str::from_utf8(slice).ok()?;
        let v = u16::from_str_radix(s, 16).ok()?;
        self.i += 4;
        Some(v)
    }

    fn boolean(&mut self) -> Option<AnyValue> {
        if self.b.get(self.i..self.i + 4) == Some(b"true") {
            self.i += 4;
            Some(AnyValue::Bool(true))
        } else if self.b.get(self.i..self.i + 5) == Some(b"false") {
            self.i += 5;
            Some(AnyValue::Bool(false))
        } else {
            None
        }
    }

    fn null(&mut self) -> Option<AnyValue> {
        if self.b.get(self.i..self.i + 4) == Some(b"null") {
            self.i += 4;
            Some(AnyValue::Null)
        } else {
            None
        }
    }

    fn number(&mut self) -> Option<AnyValue> {
        let start = self.i;
        let mut is_float = false;
        while let Some(&c) = self.b.get(self.i) {
            match c {
                b'0'..=b'9' | b'-' | b'+' => self.i += 1,
                b'.' | b'e' | b'E' => {
                    is_float = true;
                    self.i += 1;
                }
                _ => break,
            }
        }
        let text = std::str::from_utf8(self.b.get(start..self.i)?).ok()?;
        if text.is_empty() {
            return None;
        }
        if is_float {
            text.parse::<f64>().ok().map(AnyValue::Double)
        } else {
            match text.parse::<i64>() {
                Ok(n) => Some(AnyValue::Int(n)),
                Err(_) => text.parse::<f64>().ok().map(AnyValue::Double),
            }
        }
    }
}

/// Recognize the non-finite sentinel `{"$f":"nan|inf|-inf"}`; otherwise it is a plain map.
fn sentinel_or_map(pairs: Vec<(String, AnyValue)>) -> AnyValue {
    if pairs.len() == 1
        && pairs[0].0 == "$f"
        && let AnyValue::Str(tag) = &pairs[0].1
    {
        match tag.as_str() {
            "nan" => return AnyValue::Double(f64::NAN),
            "inf" => return AnyValue::Double(f64::INFINITY),
            "-inf" => return AnyValue::Double(f64::NEG_INFINITY),
            _ => {}
        }
    }
    AnyValue::Map(pairs)
}

/// Length in bytes of the UTF-8 sequence beginning with `lead`.
fn utf8_len(lead: u8) -> Option<usize> {
    match lead {
        0x00..=0x7f => Some(1),
        0xc0..=0xdf => Some(2),
        0xe0..=0xef => Some(3),
        0xf0..=0xf7 => Some(4),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canonical::canonical_json_value;

    #[test]
    fn round_trips_canonical() {
        let v = AnyValue::Map(vec![
            ("a".into(), AnyValue::Int(-7)),
            ("b".into(), AnyValue::Double(1.5)),
            ("c".into(), AnyValue::Str("hi\n\"x\"".into())),
            ("d".into(), AnyValue::Bool(true)),
            ("e".into(), AnyValue::Null),
            (
                "f".into(),
                AnyValue::Array(vec![AnyValue::Int(1), AnyValue::Double(f64::INFINITY)]),
            ),
        ]);
        let encoded = canonical_json_value(&v);
        let parsed = parse(&encoded).unwrap();
        assert_eq!(canonical_json_value(&parsed), encoded);
    }

    #[test]
    fn parses_object_keys() {
        let pairs = parse_object(r#"{"http.route":"/cart","n":3}"#).unwrap();
        assert_eq!(pairs.len(), 2);
        assert_eq!(pairs[0].0, "http.route");
        assert_eq!(pairs[0].1, AnyValue::Str("/cart".into()));
        assert_eq!(pairs[1].1, AnyValue::Int(3));
    }

    #[test]
    fn nan_sentinel_parses_to_double() {
        assert!(matches!(parse(r#"{"$f":"nan"}"#), Some(AnyValue::Double(d)) if d.is_nan()));
    }

    #[test]
    fn rejects_trailing_garbage() {
        assert!(parse("123 456").is_none());
        assert!(parse("{").is_none());
    }

    #[test]
    fn depth_guard_rejects_deep_nesting_without_overflow() {
        // A document nested far past MAX_DEPTH parses to None (guard), not a stack overflow.
        let deep = format!("{}{}", "[".repeat(5000), "]".repeat(5000));
        assert!(parse(&deep).is_none());
        // Nesting within the limit still parses fine.
        let ok = format!("{}1{}", "[".repeat(100), "]".repeat(100));
        assert!(parse(&ok).is_some());
    }
}

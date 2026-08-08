//! The canonical-JSON reader — the inverse of the [`crate::canonical`] encoder (§6.1).
//!
//! It exists so DTOs and the `json_get_*` UDFs can read attribute values back out of the
//! canonical-JSON columns (ARCHITECTURE.md §10.4). It reads the canonical subset plus ordinary
//! JSON: objects, arrays, strings, int/float numbers, booleans, null, and the non-finite sentinel
//! `{"$f":"nan|inf|-inf"}` → `Double`.
//!
//! The grammar is `serde_json`'s (strict RFC 8259, so `\uXXXX` surrogate pairs recombine
//! correctly), but the *value model* is [`AnyValue`], not `serde_json::Value`. That is why this is a
//! hand-written [`Visitor`] over `deserialize_any` rather than a `Value` → `AnyValue` conversion:
//! `AnyValue::Map` is an **ordered** pair list, and a `serde_json::Value` map is a `BTreeMap` that
//! would silently re-sort the input. Going through the visitor also skips the intermediate tree.
//!
//! Nesting depth is bounded by `serde_json`'s own 128-deep recursion limit, which rejects
//! pathologically nested input instead of overflowing the stack.
//!
//! Limitation: base64 `bytes` come back as `Str`, since a JSON string carries no type tag.

use std::fmt;

use serde::de::{
    Deserialize, DeserializeSeed, Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor,
};

use crate::value::AnyValue;

/// Parse a complete JSON document into an [`AnyValue`]. Returns `None` on malformed input (including
/// trailing garbage and nesting past `serde_json`'s recursion limit).
pub fn parse(input: &str) -> Option<AnyValue> {
    serde_json::from_str::<Parsed>(input).ok().map(|p| p.0)
}

/// Parse a JSON object into ordered key/value pairs; `None` if the input is not an object.
pub fn parse_object(input: &str) -> Option<Vec<(String, AnyValue)>> {
    match parse(input)? {
        AnyValue::Map(pairs) => Some(pairs),
        _ => None,
    }
}

/// Pull **one** key's value out of a JSON object without materializing the rest of it.
///
/// This is the hot path behind the `json_get_*` UDFs: an unpromoted attribute filter evaluates it
/// once per row, so what it *avoids* matters more than what it does. [`parse_object`] builds a
/// `Vec<(String, AnyValue)>` — one allocation per key and per string value — and then linear-searches
/// it for a single field. Measured, that costs roughly 8 ms per attribute per 100k rows, so a record
/// carrying 40 attributes made an unpromoted filter ~36x its own `count(*)` floor.
///
/// Two properties do the work. Non-matching values are skipped with `IgnoredAny`, which walks their
/// tokens without allocating anything; and keys are borrowed out of the input rather than copied.
///
/// **It deliberately walks the whole object even after finding the key**, for two separate reasons,
/// and both were learned the hard way:
///
/// 1. Returning early from `visit_map` leaves `serde_json`'s parser positioned mid-object, so its
///    `deserialize_map` then fails to find the closing brace and errors — which silently routes the
///    caller into the full-parse fallback. Matching an *early* key would then cost the aborted scan
///    **plus** the full parse, making a hit on the first key ~3x slower than a hit on the last. That
///    is the opposite of the intended behaviour and it does not fail loudly, it just gets slow.
/// 2. Stopping once the keys pass `key` lexicographically would be valid for canonical JSON, which
///    sorts them (§6.1) — but this is reachable from the public `json_get` and hence from the
///    `json_get_*` UDFs, which a caller can point at any text column. An unsorted object would then
///    report a key it does contain as missing.
///
/// Skipping value allocation is where the win is anyway; early termination was never the point.
///
/// Returns `Err` rather than `None` when the fast path cannot apply — notably a key containing an
/// escape, which `serde_json` cannot hand back as a borrowed `&str`. Callers fall back to
/// [`parse_object`], which is why correctness never rests on this function's cleverness.
pub(crate) fn pick_field_public(input: &str, key: &str) -> Result<Option<AnyValue>, ()> {
    let mut de = serde_json::Deserializer::from_str(input);
    Pick { key }.deserialize(&mut de).map_err(|_| ())
}

/// The seed carrying the key being looked for. Doubles as its own [`Visitor`].
struct Pick<'k> {
    key: &'k str,
}

impl<'de> DeserializeSeed<'de> for Pick<'_> {
    type Value = Option<AnyValue>;

    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<Self::Value, D::Error> {
        d.deserialize_map(self)
    }
}

impl<'de> Visitor<'de> for Pick<'_> {
    type Value = Option<AnyValue>;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "a JSON object")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let mut found = None;
        // `&str` keys borrow from the input; an escaped key makes this fail, and the caller falls
        // back to the full parse.
        while let Some(k) = map.next_key::<&str>()? {
            // Keep the *first* match, mirroring `parse_object(..).find(..)` on a document with
            // duplicate keys.
            if found.is_none() && k == self.key {
                found = Some(map.next_value::<Parsed>()?.0);
            } else {
                map.next_value::<IgnoredAny>()?;
            }
        }
        Ok(found)
    }
}

/// Newtype so the deserializer targets imbh's canonical form. A `Deserialize` impl on `AnyValue`
/// itself would collide with the externally-tagged one its optional `serde` feature derives.
struct Parsed(AnyValue);

impl<'de> Deserialize<'de> for Parsed {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        d.deserialize_any(AnyValueVisitor).map(Parsed)
    }
}

struct AnyValueVisitor;

impl<'de> Visitor<'de> for AnyValueVisitor {
    type Value = AnyValue;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a JSON value")
    }

    fn visit_unit<E>(self) -> Result<AnyValue, E> {
        Ok(AnyValue::Null)
    }

    fn visit_bool<E>(self, v: bool) -> Result<AnyValue, E> {
        Ok(AnyValue::Bool(v))
    }

    fn visit_i64<E>(self, v: i64) -> Result<AnyValue, E> {
        Ok(AnyValue::Int(v))
    }

    fn visit_u64<E>(self, v: u64) -> Result<AnyValue, E> {
        // `i64` is the OTel integer width, so anything past it degrades to `Double` rather than
        // failing the whole document.
        Ok(i64::try_from(v).map_or(AnyValue::Double(v as f64), AnyValue::Int))
    }

    fn visit_f64<E>(self, v: f64) -> Result<AnyValue, E> {
        Ok(AnyValue::Double(v))
    }

    fn visit_str<E>(self, v: &str) -> Result<AnyValue, E> {
        Ok(AnyValue::Str(v.to_owned()))
    }

    fn visit_string<E>(self, v: String) -> Result<AnyValue, E> {
        Ok(AnyValue::Str(v))
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<AnyValue, A::Error> {
        let mut items = Vec::with_capacity(seq.size_hint().unwrap_or(0));
        while let Some(Parsed(v)) = seq.next_element()? {
            items.push(v);
        }
        Ok(AnyValue::Array(items))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<AnyValue, A::Error> {
        let mut pairs = Vec::with_capacity(map.size_hint().unwrap_or(0));
        while let Some((k, Parsed(v))) = map.next_entry::<String, Parsed>()? {
            pairs.push((k, v));
        }
        Ok(sentinel_or_map(pairs))
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
    fn round_trip_preserves_int_vs_double() {
        // `Double(1.0)` encodes as `1.0`, so it must not come back as `Int(1)`.
        for v in [
            AnyValue::Int(1),
            AnyValue::Double(1.0),
            AnyValue::Double(-0.0),
        ] {
            assert_eq!(parse(&canonical_json_value(&v)).unwrap(), v);
        }
    }

    #[test]
    fn parses_object_keys_in_input_order() {
        // Order is part of `AnyValue::Map`'s contract — a `serde_json::Value` round trip would
        // re-sort these.
        let pairs = parse_object(r#"{"z":1,"http.route":"/cart","n":3}"#).unwrap();
        let keys: Vec<&str> = pairs.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, ["z", "http.route", "n"]);
        assert_eq!(pairs[1].1, AnyValue::Str("/cart".into()));
        assert_eq!(pairs[2].1, AnyValue::Int(3));
    }

    #[test]
    fn nan_sentinel_parses_to_double() {
        assert!(matches!(parse(r#"{"$f":"nan"}"#), Some(AnyValue::Double(d)) if d.is_nan()));
    }

    #[test]
    fn recombines_surrogate_pairs() {
        // U+1F600 written as the escaped surrogate pair every `ensure_ascii`-style encoder emits
        // (Python's `json.dumps` by default). The hand-rolled parser this replaced could not
        // recombine the pair and rejected the whole document.
        let hi = r"\ud83d";
        let lo = r"\ude00";
        assert_eq!(
            parse(&format!("\"{hi}{lo}\"")),
            Some(AnyValue::Str("\u{1f600}".into()))
        );
        // A lone surrogate is still not a character, so the document is still rejected.
        assert!(parse(r#""\ud83d""#).is_none());
    }

    #[test]
    fn rejects_trailing_garbage() {
        assert!(parse("123 456").is_none());
        assert!(parse("{").is_none());
    }

    #[test]
    fn rejects_non_json_the_hand_rolled_parser_accepted() {
        for bad in ["+5", "007", "1e400", "\"a\u{01}b\"", "{\"a\":1,}"] {
            assert!(parse(bad).is_none(), "should reject {bad:?}");
        }
    }

    #[test]
    fn big_integers_degrade_to_double() {
        assert!(matches!(
            parse("18446744073709551615"),
            Some(AnyValue::Double(_))
        ));
        assert_eq!(parse("9223372036854775807"), Some(AnyValue::Int(i64::MAX)));
    }

    #[test]
    fn depth_guard_rejects_deep_nesting_without_overflow() {
        // A document nested far past serde_json's recursion limit parses to None (guard), not a
        // stack overflow.
        let deep = format!("{}{}", "[".repeat(5000), "]".repeat(5000));
        assert!(parse(&deep).is_none());
        // Nesting within the limit still parses fine.
        let ok = format!("{}1{}", "[".repeat(100), "]".repeat(100));
        assert!(parse(&ok).is_some());
    }
}

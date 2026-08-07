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
//! - Doubles via `serde_json`'s shortest round-trip decimal, which is why `Double(1.0)` encodes
//!   as `1.0` and not `1`: the trailing `.0` is what lets [`crate::json`] read the value back as a
//!   `Double` rather than an `Int`. Large/small magnitudes use exponent form (`1e300`).
//! - `bytes` → base64 (standard alphabet, padded) as a JSON string.
//! - Non-finite doubles (NaN/±Inf, which OTel permits and JSON forbids) → the reserved
//!   sentinel object `{"$f":"nan"|"inf"|"-inf"}`.
//! - Nested arrays/maps recursively canonicalized.
//!
//! The same function backs the segment writer (dict-equality), the Tantivy JSON feeder
//! (§8), and the `json_get_*` UDFs (§9.3) so all three agree byte-for-byte.
//!
//! The encoder is a [`Serialize`] impl over `serde_json`'s writer rather than a hand-rolled string
//! builder, but it does **not** go through `serde_json::Value`: a `Value` map is a `BTreeMap` whose
//! ordering would silently become insertion order if anything in the dependency graph ever turned on
//! serde_json's `preserve_order` feature (features unify globally). Sorting explicitly and streaming
//! the pairs through `collect_map` makes the key-order invariant independent of that.
//!
//! What is written by hand here is only what no library can express: the key sort and the `{"$f":…}`
//! sentinel. Escaping, float formatting, and structure come from `serde_json`; `bytes` come from the
//! `base64` crate, the same engine `imbh-mcp` encodes them with.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde::Serialize;
use serde::ser::{SerializeMap, Serializer};

use crate::value::AnyValue;

/// Canonicalize an ordered attribute map into a JSON object string.
pub fn canonical_json_object(entries: &[(String, AnyValue)]) -> String {
    encode(&CanonicalObject(entries))
}

/// Canonicalize a single [`AnyValue`] into its JSON text form.
pub fn canonical_json_value(v: &AnyValue) -> String {
    encode(&Canonical(v))
}

/// Serialization is infallible for this value model: every map key is a `String`, every `f64` that
/// `serde_json` would reject is intercepted by [`encode_double`], and the writer is an in-memory
/// `String`. A failure here would mean a `serde_json` bug, not bad data.
fn encode<T: Serialize>(v: &T) -> String {
    serde_json::to_string(v).expect("canonical JSON encoding is infallible")
}

/// An [`AnyValue`] in canonical form. A newtype, not a `Serialize` impl on `AnyValue` itself: under
/// the `serde` feature `AnyValue` derives its own externally-tagged representation (`{"Str":"x"}`),
/// which is the DTO wire form and deliberately not this.
struct Canonical<'a>(&'a AnyValue);

/// An attribute map in canonical form, for callers that hold the pairs rather than an `AnyValue`.
struct CanonicalObject<'a>(&'a [(String, AnyValue)]);

impl Serialize for Canonical<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self.0 {
            AnyValue::Null => s.serialize_unit(),
            AnyValue::Bool(b) => s.serialize_bool(*b),
            AnyValue::Int(i) => s.serialize_i64(*i),
            AnyValue::Double(d) => encode_double(s, *d),
            AnyValue::Str(v) => s.serialize_str(v),
            AnyValue::Bytes(b) => s.serialize_str(&base64(b)),
            AnyValue::Array(items) => s.collect_seq(items.iter().map(Canonical)),
            AnyValue::Map(entries) => CanonicalObject(entries).serialize(s),
        }
    }
}

impl Serialize for CanonicalObject<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        // Sort by key code point. Stable sort keeps the last-writer order for duplicate keys,
        // which OTel maps should not contain anyway.
        let mut refs: Vec<&(String, AnyValue)> = self.0.iter().collect();
        refs.sort_by(|a, b| a.0.cmp(&b.0));
        s.collect_map(refs.into_iter().map(|(k, v)| (k, Canonical(v))))
    }
}

fn encode_double<S: Serializer>(s: S, d: f64) -> Result<S::Ok, S::Error> {
    // JSON has no NaN/Inf and `serde_json` writes them as `null`, which loses the value. OTel
    // permits them, so they take the reserved sentinel object instead.
    let tag = if d.is_nan() {
        "nan"
    } else if d.is_infinite() {
        if d.is_sign_negative() { "-inf" } else { "inf" }
    } else {
        return s.serialize_f64(d);
    };
    let mut map = s.serialize_map(Some(1))?;
    map.serialize_entry("$f", tag)?;
    map.end()
}

/// Standard base64 (RFC 4648) with padding — the same engine `imbh-mcp` encodes these bytes with.
fn base64(data: &[u8]) -> String {
    BASE64.encode(data)
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
    fn duplicate_keys_are_kept_not_collapsed() {
        // `serde_json::Map` would collapse these to the last writer; the explicit `collect_map`
        // over sorted pairs does not, matching the pre-serde_json encoder.
        assert_eq!(
            canonical_json_object(&m(vec![("a", AnyValue::Int(1)), ("a", AnyValue::Int(2))])),
            r#"{"a":1,"a":2}"#
        );
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
    fn doubles_keep_their_fractional_marker() {
        // The `.0` is load-bearing: it is the only thing distinguishing `Double(1.0)` from
        // `Int(1)` on the way back through `crate::json::parse`.
        assert_eq!(canonical_json_value(&AnyValue::Double(1.0)), "1.0");
        assert_eq!(canonical_json_value(&AnyValue::Double(-0.5)), "-0.5");
        assert_eq!(canonical_json_value(&AnyValue::Int(1)), "1");
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

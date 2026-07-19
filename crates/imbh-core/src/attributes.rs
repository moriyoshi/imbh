//! Owned, materialized attribute maps for row DTOs (ARCHITECTURE.md §10.4).
//!
//! `Attributes` is the owned form carried by `LogEntry`/`Span`/etc: it is built by parsing a
//! segment's canonical-JSON column. The zero-copy `AttributesView` for the columnar/streaming
//! path is a later addition.

use crate::json;
use crate::value::AnyValue;

/// An owned, key-ordered attribute map. Under the `serde` feature it (de)serializes as an array of
/// `[key, value]` pairs (preserving order and duplicate keys, unlike a JSON object).
#[derive(Debug, Clone, Default, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Attributes(Vec<(String, AnyValue)>);

impl Attributes {
    /// Empty map.
    pub fn new() -> Self {
        Attributes(Vec::new())
    }

    /// Build from ordered pairs (as produced by the JSON parser).
    pub fn from_pairs(pairs: Vec<(String, AnyValue)>) -> Self {
        Attributes(pairs)
    }

    /// Parse a canonical-JSON object column value. A non-object or malformed value yields an
    /// empty map (attribute columns are always written as objects, defaulting to `{}`).
    pub fn from_canonical_json(s: &str) -> Self {
        Attributes(json::parse_object(s).unwrap_or_default())
    }

    pub fn get(&self, key: &str) -> Option<&AnyValue> {
        self.0.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    pub fn get_str(&self, key: &str) -> Option<&str> {
        match self.get(key)? {
            AnyValue::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn get_i64(&self, key: &str) -> Option<i64> {
        match self.get(key)? {
            AnyValue::Int(i) => Some(*i),
            _ => None,
        }
    }

    pub fn get_f64(&self, key: &str) -> Option<f64> {
        match self.get(key)? {
            AnyValue::Double(d) => Some(*d),
            AnyValue::Int(i) => Some(*i as f64),
            _ => None,
        }
    }

    pub fn get_bool(&self, key: &str) -> Option<bool> {
        match self.get(key)? {
            AnyValue::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &AnyValue)> {
        self.0.iter().map(|(k, v)| (k.as_str(), v))
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// Extract a value for `key` from a canonical-JSON object string (used by the `json_get_*`
/// UDFs, ARCHITECTURE.md §9.3).
pub fn json_get(json: &str, key: &str) -> Option<AnyValue> {
    json::parse_object(json)?
        .into_iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_accessors() {
        let a =
            Attributes::from_canonical_json(r#"{"http.route":"/cart","n":3,"ok":true,"x":1.5}"#);
        assert_eq!(a.len(), 4);
        assert_eq!(a.get_str("http.route"), Some("/cart"));
        assert_eq!(a.get_i64("n"), Some(3));
        assert_eq!(a.get_bool("ok"), Some(true));
        assert_eq!(a.get_f64("x"), Some(1.5));
        assert_eq!(a.get_str("missing"), None);
    }

    #[test]
    fn json_get_extracts() {
        assert_eq!(
            json_get(r#"{"peer.service":"cart"}"#, "peer.service"),
            Some(AnyValue::Str("cart".into()))
        );
        assert_eq!(json_get("{}", "k"), None);
    }
}

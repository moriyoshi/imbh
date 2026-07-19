//! The OTel `AnyValue` model — imbh's value type for attribute values and structured
//! log bodies. Mirrors OTLP's `AnyValue`. The default build is serde-free (ARCHITECTURE.md §10.4):
//! canonical-JSON handling lives in [`crate::json`], never through `serde_json`. `Serialize`/
//! `Deserialize` are derived only under the optional `serde` feature (an externally-tagged enum,
//! e.g. `{"Str":"x"}` / `{"Int":5}`), so hosts can round-trip DTOs without taxing the default graph.

/// A value in an OTel attribute map or a structured log body.
///
/// `Map` preserves insertion order as it arrives; canonicalization (key sorting,
/// byte-identical encoding) happens in [`crate::canonical_json_value`], never here.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum AnyValue {
    Null,
    Str(String),
    Int(i64),
    Double(f64),
    Bool(bool),
    Bytes(Vec<u8>),
    Array(Vec<AnyValue>),
    /// Ordered key/value pairs. Canonicalized (keys sorted) only on encode.
    Map(Vec<(String, AnyValue)>),
}

impl AnyValue {
    /// The plain string, if this is a `Str`.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            AnyValue::Str(s) => Some(s),
            _ => None,
        }
    }
}

impl From<&str> for AnyValue {
    fn from(s: &str) -> Self {
        AnyValue::Str(s.to_owned())
    }
}
impl From<String> for AnyValue {
    fn from(s: String) -> Self {
        AnyValue::Str(s)
    }
}
impl From<i64> for AnyValue {
    fn from(v: i64) -> Self {
        AnyValue::Int(v)
    }
}
impl From<f64> for AnyValue {
    fn from(v: f64) -> Self {
        AnyValue::Double(v)
    }
}
impl From<bool> for AnyValue {
    fn from(v: bool) -> Self {
        AnyValue::Bool(v)
    }
}

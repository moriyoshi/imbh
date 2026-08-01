//! JSON plumbing for the MCP endpoint: typed access to a tool call's arguments, conversions from
//! imbh's value types into `serde_json`, and the Base64 sentinel the transport's header validation
//! needs.
//!
//! Both crates are footprint-neutral here — `serde_json` is already compiled in the default graph
//! via `arrow-json` and `base64` via `arrow-cast`, both under DataFusion — so the MCP endpoint costs
//! no crate despite speaking JSON-RPC (ARCHITECTURE.md §10.16.1).
//!
//! Note that `serde_json::Map` is a `BTreeMap` in this build (no `preserve_order` feature), so object
//! keys serialize alphabetically. That is invisible to a JSON reader and deliberately not overridden:
//! turning `preserve_order` on would flip the feature for *every* `serde_json` user in the graph,
//! DataFusion included, to buy nothing but field order.

use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use imbh::{AnyValue, Attributes, parse_duration};
use serde_json::{Map, Value, json};

// ── imbh values → JSON ──────────────────────────────────────────────────────────────────────────

/// Encode a float. JSON has no NaN or infinity, so a non-finite value becomes `null` rather than the
/// `{"$f":…}` sentinel imbh's *canonical* encoder uses: an MCP client is a generic JSON reader, not a
/// reader of imbh's storage convention. `Number::from_f64` rejects exactly the non-finite cases.
pub(crate) fn number(value: f64) -> Value {
    serde_json::Number::from_f64(value).map_or(Value::Null, Value::Number)
}

/// Encode an [`AnyValue`]. `Bytes` becomes a Base64 string, matching imbh's canonical encoding
/// (§6.1) — a JSON string carries no type tag either way.
pub(crate) fn any_value(value: &AnyValue) -> Value {
    match value {
        AnyValue::Null => Value::Null,
        AnyValue::Str(s) => Value::String(s.clone()),
        AnyValue::Int(i) => json!(i),
        AnyValue::Double(d) => number(*d),
        AnyValue::Bool(b) => Value::Bool(*b),
        AnyValue::Bytes(b) => Value::String(BASE64.encode(b)),
        AnyValue::Array(items) => Value::Array(items.iter().map(any_value).collect()),
        AnyValue::Map(pairs) => Value::Object(
            pairs
                .iter()
                .map(|(k, v)| (k.clone(), any_value(v)))
                .collect(),
        ),
    }
}

/// Encode an attribute map as a JSON object. Duplicate keys (which `Attributes` permits) collapse
/// the way any JSON reader would collapse them — last wins.
pub(crate) fn attributes(attrs: &Attributes) -> Value {
    Value::Object(
        attrs
            .iter()
            .map(|(k, v)| (k.to_owned(), any_value(v)))
            .collect(),
    )
}

/// Encode `(key, value)` label pairs as a JSON object.
pub(crate) fn labels(pairs: &[(String, String)]) -> Value {
    Value::Object(
        pairs
            .iter()
            .map(|(k, v)| (k.clone(), Value::String(v.clone())))
            .collect(),
    )
}

// ── reading tool arguments ──────────────────────────────────────────────────────────────────────

/// A tool call's `arguments` object, with typed accessors.
///
/// Every accessor that can fail returns the message a *model* should see: MCP reports argument
/// problems as tool-execution errors (`isError: true`), not protocol errors, precisely so the model
/// can correct itself and retry (MCP `server/tools` — Error Handling).
pub(crate) struct Args(Map<String, Value>);

impl Args {
    /// Wrap a parsed `arguments` value. A missing or non-object `arguments` is an empty map — the
    /// per-argument accessors then report exactly which required argument is missing, which is more
    /// useful than one blanket "arguments must be an object".
    pub(crate) fn new(value: Option<&Value>) -> Self {
        match value {
            Some(Value::Object(map)) => Args(map.clone()),
            _ => Args(Map::new()),
        }
    }

    /// An explicit `null` reads as absent, so a client that fills every optional field with null
    /// still works.
    fn get(&self, key: &str) -> Option<&Value> {
        self.0.get(key).filter(|v| !v.is_null())
    }

    /// An optional string argument. A non-string value is an error rather than a coercion: silently
    /// stringifying `{"service": 5}` would search for the service literally named "5".
    pub(crate) fn str(&self, key: &str) -> Result<Option<&str>, String> {
        match self.get(key) {
            None => Ok(None),
            Some(Value::String(s)) => Ok(Some(s.as_str())),
            Some(_) => Err(format!("argument `{key}` must be a string")),
        }
    }

    pub(crate) fn req_str(&self, key: &str) -> Result<&str, String> {
        self.str(key)?
            .ok_or_else(|| format!("argument `{key}` is required"))
    }

    pub(crate) fn i64(&self, key: &str) -> Result<Option<i64>, String> {
        match self.get(key) {
            None => Ok(None),
            // A JSON writer that emits `1e9` or `1.0` still means an integer here.
            Some(Value::Number(n)) => n
                .as_i64()
                .or_else(|| n.as_f64().filter(|d| d.fract() == 0.0).map(|d| d as i64))
                .map(Some)
                .ok_or_else(|| format!("argument `{key}` must be an integer")),
            Some(_) => Err(format!("argument `{key}` must be an integer")),
        }
    }

    pub(crate) fn f64(&self, key: &str) -> Result<Option<f64>, String> {
        match self.get(key) {
            None => Ok(None),
            Some(Value::Number(n)) => n
                .as_f64()
                .map(Some)
                .ok_or_else(|| format!("argument `{key}` must be a number")),
            Some(_) => Err(format!("argument `{key}` must be a number")),
        }
    }

    pub(crate) fn bool(&self, key: &str) -> Result<Option<bool>, String> {
        match self.get(key) {
            None => Ok(None),
            Some(Value::Bool(b)) => Ok(Some(*b)),
            Some(_) => Err(format!("argument `{key}` must be a boolean")),
        }
    }

    /// A bounded row/series limit: defaulted when absent, clamped to `max` so one tool call cannot
    /// ask for the whole database.
    pub(crate) fn limit(&self, key: &str, default: usize, max: usize) -> Result<usize, String> {
        match self.i64(key)? {
            None => Ok(default),
            Some(n) if n < 1 => Err(format!("argument `{key}` must be at least 1")),
            Some(n) => Ok((n as usize).min(max)),
        }
    }

    /// A duration argument in imbh's spec form (`30s`, `15m`, `2h`, `7d`).
    pub(crate) fn duration(&self, key: &str) -> Result<Option<Duration>, String> {
        match self.str(key)? {
            None => Ok(None),
            Some(s) => parse_duration(s)
                .map(Some)
                .map_err(|e| format!("argument `{key}` is not a duration ({s:?}): {e}")),
        }
    }

    /// An object of string→string pairs, e.g. `{"http.route": "/cart"}`. Non-string scalars are
    /// rendered rather than rejected so `{"http.status_code": 500}` works the way a model expects.
    pub(crate) fn string_map(&self, key: &str) -> Result<Vec<(String, String)>, String> {
        match self.get(key) {
            None => Ok(Vec::new()),
            Some(Value::Object(map)) => Ok(map
                .iter()
                .map(|(k, v)| {
                    let v = match v {
                        Value::String(s) => s.clone(),
                        // `to_string` on a non-string scalar is its JSON text, which for numbers and
                        // booleans is exactly the value; containers keep their JSON form, which is
                        // the only sensible reading of an attribute compared against one.
                        other => other.to_string(),
                    };
                    (k.clone(), v)
                })
                .collect()),
            Some(_) => Err(format!(
                "argument `{key}` must be an object of key/value pairs"
            )),
        }
    }

    /// An array of strings.
    pub(crate) fn string_list(&self, key: &str) -> Result<Vec<String>, String> {
        match self.get(key) {
            None => Ok(Vec::new()),
            Some(Value::Array(items)) => items
                .iter()
                .map(|item| match item {
                    Value::String(s) => Ok(s.clone()),
                    _ => Err(format!("argument `{key}` must be an array of strings")),
                })
                .collect(),
            // A bare string is the obvious intent for a one-element list; accept it.
            Some(Value::String(s)) => Ok(vec![s.clone()]),
            Some(_) => Err(format!("argument `{key}` must be an array of strings")),
        }
    }
}

// ── the transport's `=?base64?…?=` header sentinel ──────────────────────────────────────────────

/// Decode a Streamable HTTP header value, undoing the `=?base64?…?=` sentinel the transport defines
/// for values that cannot travel as plain ASCII (MCP `basic/transports/streamable-http` — Value
/// Encoding). A plain value is returned as-is; a malformed sentinel yields `None`, which the caller
/// reports as a header mismatch.
pub(crate) fn decode_header_value(value: &str) -> Option<String> {
    let Some(inner) = value
        .strip_prefix("=?base64?")
        .and_then(|v| v.strip_suffix("?="))
    else {
        return Some(value.to_owned());
    };
    String::from_utf8(BASE64.decode(inner).ok()?).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(json: &str) -> Args {
        let value: Value = serde_json::from_str(json).expect("test json");
        Args::new(Some(&value))
    }

    #[test]
    fn non_finite_floats_become_null() {
        // JSON cannot spell NaN, and a p99 over an empty bucket is exactly where one shows up.
        assert_eq!(number(f64::NAN), Value::Null);
        assert_eq!(number(f64::INFINITY), Value::Null);
        assert_eq!(number(1.5).to_string(), "1.5");
    }

    #[test]
    fn any_values_encode() {
        assert_eq!(any_value(&AnyValue::Null), Value::Null);
        assert_eq!(any_value(&AnyValue::Int(7)).to_string(), "7");
        // Base64 per RFC 4648, which is what imbh's canonical encoder emits for `Bytes`.
        assert_eq!(
            any_value(&AnyValue::Bytes(b"foobar".to_vec())),
            json!("Zm9vYmFy")
        );
        assert_eq!(
            any_value(&AnyValue::Array(vec![
                AnyValue::Bool(false),
                AnyValue::Str("x".into())
            ])),
            json!([false, "x"])
        );
        // Escaping is serde_json's problem now, not a hand-rolled writer's.
        assert_eq!(
            any_value(&AnyValue::Str("cart\"api\n".into())).to_string(),
            r#""cart\"api\n""#
        );
    }

    #[test]
    fn typed_arguments_read_and_reject() {
        let a = args(r#"{"service":"cart","limit":5,"ratio":1.5,"on":true,"nulled":null}"#);
        assert_eq!(a.str("service").unwrap(), Some("cart"));
        assert_eq!(a.str("absent").unwrap(), None);
        // An explicit null reads as absent, so a client that fills every field with null still works.
        assert_eq!(a.str("nulled").unwrap(), None);
        assert_eq!(a.i64("limit").unwrap(), Some(5));
        assert_eq!(a.f64("ratio").unwrap(), Some(1.5));
        assert_eq!(a.bool("on").unwrap(), Some(true));
        assert!(a.str("limit").is_err());
        assert!(a.i64("service").is_err());
        assert_eq!(a.req_str("service").unwrap(), "cart");
        assert!(a.req_str("absent").unwrap_err().contains("required"));
    }

    #[test]
    fn integers_accept_the_float_spellings_a_json_writer_emits() {
        let a = args(r#"{"whole":1.0,"exp":1e3,"fractional":1.5}"#);
        assert_eq!(a.i64("whole").unwrap(), Some(1));
        assert_eq!(a.i64("exp").unwrap(), Some(1000));
        assert!(a.i64("fractional").is_err());
    }

    #[test]
    fn limits_default_and_clamp() {
        let a = args(r#"{"a":5,"big":100000,"zero":0}"#);
        assert_eq!(a.limit("absent", 20, 1000).unwrap(), 20);
        assert_eq!(a.limit("a", 20, 1000).unwrap(), 5);
        // Clamped, not rejected: a model asking for everything gets the cap, not an error.
        assert_eq!(a.limit("big", 20, 1000).unwrap(), 1000);
        assert!(a.limit("zero", 20, 1000).is_err());
    }

    #[test]
    fn durations_and_maps_read() {
        let a = args(
            r#"{"since":"15m","bad":"soon","attrs":{"code":500,"route":"/cart"},"keys":["a","b"],"one":"z"}"#,
        );
        assert_eq!(a.duration("since").unwrap(), Some(Duration::from_secs(900)));
        assert!(a.duration("bad").is_err());
        assert_eq!(
            a.string_map("attrs").unwrap(),
            vec![
                ("code".to_owned(), "500".to_owned()),
                ("route".to_owned(), "/cart".to_owned())
            ]
        );
        assert_eq!(a.string_list("keys").unwrap(), vec!["a", "b"]);
        // A bare string is a one-element list.
        assert_eq!(a.string_list("one").unwrap(), vec!["z"]);
    }

    #[test]
    fn header_sentinel_decodes() {
        assert_eq!(
            decode_header_value("search_logs").as_deref(),
            Some("search_logs")
        );
        assert_eq!(
            decode_header_value("=?base64?SGVsbG8sIOS4lueVjA==?=").as_deref(),
            Some("Hello, 世界")
        );
        // Malformed sentinels are refused rather than silently passed through as literals.
        assert_eq!(decode_header_value("=?base64?not!base64?="), None);
        assert_eq!(decode_header_value("=?base64?Zm8?="), None); // unpadded
    }
}

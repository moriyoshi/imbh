//! JSON plumbing for the MCP endpoint: a writer for building responses, a reader for tool
//! arguments, and the Base64 sentinel codec the transport's header validation needs.
//!
//! `imbhd` hand-rolls its JSON everywhere else (ARCHITECTURE.md §10.16 — `serde_json` is
//! deliberately not a dependency of this crate), and the MCP endpoint keeps to that: parsing goes
//! through [`imbh::parse_json`] (imbh-core's dependency-free parser) and writing through the
//! builders below. Serving MCP therefore adds **no** crate to imbhd's graph.

use std::time::Duration;

use imbh::{AnyValue, Attributes, parse_duration};

use crate::json_string;

// ── writing ─────────────────────────────────────────────────────────────────────────────────────

/// A JSON object under construction. Fields are appended in call order (JSON objects are unordered,
/// but a stable order keeps the golden tests and the wire output readable).
pub(crate) struct Obj {
    buf: String,
    empty: bool,
}

impl Obj {
    pub(crate) fn new() -> Self {
        Obj {
            buf: String::from("{"),
            empty: true,
        }
    }

    /// Append `name: value`, where `value` is already-encoded JSON text.
    pub(crate) fn raw(&mut self, name: &str, value: &str) -> &mut Self {
        if !self.empty {
            self.buf.push(',');
        }
        self.empty = false;
        self.buf.push_str(&json_string(name));
        self.buf.push(':');
        self.buf.push_str(value);
        self
    }

    pub(crate) fn str(&mut self, name: &str, value: &str) -> &mut Self {
        self.raw(name, &json_string(value))
    }

    /// Append `name: value` only when `value` is `Some` — absent beats `null` for optional fields a
    /// model has to read.
    pub(crate) fn opt_str(&mut self, name: &str, value: Option<&str>) -> &mut Self {
        match value {
            Some(v) => self.str(name, v),
            None => self,
        }
    }

    pub(crate) fn int(&mut self, name: &str, value: i64) -> &mut Self {
        self.raw(name, &value.to_string())
    }

    pub(crate) fn uint(&mut self, name: &str, value: u64) -> &mut Self {
        self.raw(name, &value.to_string())
    }

    pub(crate) fn float(&mut self, name: &str, value: f64) -> &mut Self {
        self.raw(name, &number(value))
    }

    pub(crate) fn bool(&mut self, name: &str, value: bool) -> &mut Self {
        self.raw(name, if value { "true" } else { "false" })
    }

    pub(crate) fn finish(&mut self) -> String {
        let mut out = std::mem::take(&mut self.buf);
        out.push('}');
        out
    }
}

/// A JSON array under construction.
pub(crate) struct Arr {
    buf: String,
    empty: bool,
}

impl Arr {
    pub(crate) fn new() -> Self {
        Arr {
            buf: String::from("["),
            empty: true,
        }
    }

    /// Append an already-encoded JSON element.
    pub(crate) fn raw(&mut self, value: &str) -> &mut Self {
        if !self.empty {
            self.buf.push(',');
        }
        self.empty = false;
        self.buf.push_str(value);
        self
    }

    pub(crate) fn str(&mut self, value: &str) -> &mut Self {
        self.raw(&json_string(value))
    }

    pub(crate) fn finish(&mut self) -> String {
        let mut out = std::mem::take(&mut self.buf);
        out.push(']');
        out
    }
}

/// Encode a float as JSON. JSON has no NaN or infinity, so a non-finite value becomes `null` rather
/// than the `{"$f":…}` sentinel imbh's *canonical* encoder uses: an MCP client is a generic JSON
/// reader, not a reader of imbh's storage convention.
pub(crate) fn number(value: f64) -> String {
    if value.is_finite() {
        let mut s = value.to_string();
        // `f64::to_string` renders whole floats without a fraction ("1"), which is valid JSON but
        // reads as an integer; leave it — JSON numbers carry no type tag anyway.
        if s == "-0" {
            s = "0".to_owned();
        }
        s
    } else {
        "null".to_owned()
    }
}

/// Encode an [`AnyValue`] as JSON. `Bytes` becomes a Base64 string, matching imbh's canonical
/// encoding (imbh-core §6.1) — a JSON string carries no type tag either way.
pub(crate) fn any_value(value: &AnyValue) -> String {
    match value {
        AnyValue::Null => "null".to_owned(),
        AnyValue::Str(s) => json_string(s),
        AnyValue::Int(i) => i.to_string(),
        AnyValue::Double(d) => number(*d),
        AnyValue::Bool(b) => b.to_string(),
        AnyValue::Bytes(b) => json_string(&base64_encode(b)),
        AnyValue::Array(items) => {
            let mut arr = Arr::new();
            for item in items {
                arr.raw(&any_value(item));
            }
            arr.finish()
        }
        AnyValue::Map(pairs) => {
            let mut obj = Obj::new();
            for (k, v) in pairs {
                obj.raw(k, &any_value(v));
            }
            obj.finish()
        }
    }
}

/// Encode an attribute map as a JSON object. Duplicate keys (which `Attributes` permits) collapse
/// the way any JSON reader would collapse them — last wins.
pub(crate) fn attributes(attrs: &Attributes) -> String {
    let mut obj = Obj::new();
    for (k, v) in attrs.iter() {
        obj.raw(k, &any_value(v));
    }
    obj.finish()
}

/// Encode `(key, value)` label pairs as a JSON object.
pub(crate) fn labels(pairs: &[(String, String)]) -> String {
    let mut obj = Obj::new();
    for (k, v) in pairs {
        obj.str(k, v);
    }
    obj.finish()
}

// ── reading ─────────────────────────────────────────────────────────────────────────────────────

/// A tool call's `arguments` object, with typed accessors.
///
/// Every accessor that can fail returns the message a *model* should see: MCP reports argument
/// problems as tool-execution errors (`isError: true`), not protocol errors, precisely so the model
/// can correct itself and retry (MCP `server/tools` — Error Handling).
pub(crate) struct Args(Vec<(String, AnyValue)>);

impl Args {
    /// Wrap a parsed `arguments` value. A missing or non-object `arguments` is an empty map — the
    /// per-argument accessors then report exactly which required argument is missing, which is more
    /// useful than one blanket "arguments must be an object".
    pub(crate) fn new(value: Option<&AnyValue>) -> Self {
        match value {
            Some(AnyValue::Map(pairs)) => Args(pairs.clone()),
            _ => Args(Vec::new()),
        }
    }

    fn get(&self, key: &str) -> Option<&AnyValue> {
        self.0
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v)
            .filter(|v| !matches!(v, AnyValue::Null))
    }

    /// An optional string argument. A non-string value is an error rather than a coercion: silently
    /// stringifying `{"service": 5}` would search for the service literally named "5".
    pub(crate) fn str(&self, key: &str) -> Result<Option<&str>, String> {
        match self.get(key) {
            None => Ok(None),
            Some(AnyValue::Str(s)) => Ok(Some(s.as_str())),
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
            Some(AnyValue::Int(i)) => Ok(Some(*i)),
            // A JSON writer that emits `1e9` or `1.0` still means an integer here.
            Some(AnyValue::Double(d)) if d.fract() == 0.0 && d.is_finite() => Ok(Some(*d as i64)),
            Some(_) => Err(format!("argument `{key}` must be an integer")),
        }
    }

    pub(crate) fn f64(&self, key: &str) -> Result<Option<f64>, String> {
        match self.get(key) {
            None => Ok(None),
            Some(AnyValue::Double(d)) => Ok(Some(*d)),
            Some(AnyValue::Int(i)) => Ok(Some(*i as f64)),
            Some(_) => Err(format!("argument `{key}` must be a number")),
        }
    }

    pub(crate) fn bool(&self, key: &str) -> Result<Option<bool>, String> {
        match self.get(key) {
            None => Ok(None),
            Some(AnyValue::Bool(b)) => Ok(Some(*b)),
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

    /// An object of string→string pairs, e.g. `{"http.route": "/cart"}`. Non-string values are
    /// rendered rather than rejected so `{"http.status_code": 500}` works the way a model expects.
    pub(crate) fn string_map(&self, key: &str) -> Result<Vec<(String, String)>, String> {
        match self.get(key) {
            None => Ok(Vec::new()),
            Some(AnyValue::Map(pairs)) => Ok(pairs
                .iter()
                .map(|(k, v)| {
                    let v = match v {
                        AnyValue::Str(s) => s.clone(),
                        AnyValue::Int(i) => i.to_string(),
                        AnyValue::Double(d) => number(*d),
                        AnyValue::Bool(b) => b.to_string(),
                        other => any_value(other),
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
            Some(AnyValue::Array(items)) => items
                .iter()
                .map(|item| match item {
                    AnyValue::Str(s) => Ok(s.clone()),
                    _ => Err(format!("argument `{key}` must be an array of strings")),
                })
                .collect(),
            // A bare string is the obvious intent for a one-element list; accept it.
            Some(AnyValue::Str(s)) => Ok(vec![s.clone()]),
            Some(_) => Err(format!("argument `{key}` must be an array of strings")),
        }
    }
}

// ── Base64 (the transport's `=?base64?…?=` header sentinel) ─────────────────────────────────────

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// RFC 4648 Base64 with padding.
pub(crate) fn base64_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(B64[(n >> 18) as usize & 63] as char);
        out.push(B64[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            B64[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            B64[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

/// RFC 4648 Base64 decode; `None` on any malformed input.
pub(crate) fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let b = s.as_bytes();
    if !b.len().is_multiple_of(4) {
        return None;
    }
    let mut out = Vec::with_capacity(b.len() / 4 * 3);
    for chunk in b.chunks(4) {
        let mut n: u32 = 0;
        let mut pad = 0;
        for (i, &c) in chunk.iter().enumerate() {
            let v = match c {
                b'A'..=b'Z' => c - b'A',
                b'a'..=b'z' => c - b'a' + 26,
                b'0'..=b'9' => c - b'0' + 52,
                b'+' => 62,
                b'/' => 63,
                // Padding is only legal in the last two positions, and never before a data byte.
                b'=' if i >= 2 => {
                    pad += 1;
                    0
                }
                _ => return None,
            };
            if pad > 0 && c != b'=' {
                return None;
            }
            n = (n << 6) | v as u32;
        }
        out.push((n >> 16) as u8);
        if pad < 2 {
            out.push((n >> 8) as u8);
        }
        if pad < 1 {
            out.push(n as u8);
        }
    }
    Some(out)
}

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
    String::from_utf8(base64_decode(inner)?).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use imbh::parse_json;

    fn args(json: &str) -> Args {
        Args::new(parse_json(json).as_ref())
    }

    #[test]
    fn objects_and_arrays_encode() {
        let mut obj = Obj::new();
        assert_eq!(Obj::new().finish(), "{}");
        assert_eq!(Arr::new().finish(), "[]");
        let body = obj
            .str("service", "cart\"api")
            .int("count", -3)
            .float("ratio", 0.5)
            .bool("ok", true)
            .opt_str("missing", None)
            .raw("nested", &Arr::new().str("a").finish())
            .finish();
        assert_eq!(
            body,
            r#"{"service":"cart\"api","count":-3,"ratio":0.5,"ok":true,"nested":["a"]}"#
        );
    }

    #[test]
    fn non_finite_floats_become_null() {
        // JSON cannot spell NaN, and a p99 over an empty bucket is exactly where one shows up.
        assert_eq!(number(f64::NAN), "null");
        assert_eq!(number(f64::INFINITY), "null");
        assert_eq!(number(-0.0), "0");
        assert_eq!(number(1.5), "1.5");
    }

    #[test]
    fn any_values_encode() {
        assert_eq!(any_value(&AnyValue::Null), "null");
        assert_eq!(any_value(&AnyValue::Int(7)), "7");
        assert_eq!(
            any_value(&AnyValue::Bytes(b"foobar".to_vec())),
            r#""Zm9vYmFy""#
        );
        assert_eq!(
            any_value(&AnyValue::Array(vec![
                AnyValue::Bool(false),
                AnyValue::Str("x".into())
            ])),
            r#"[false,"x"]"#
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
    fn base64_round_trips_and_rejects() {
        for input in ["", "f", "fo", "foo", "foob", "fooba", "foobar"] {
            let encoded = base64_encode(input.as_bytes());
            assert_eq!(base64_decode(&encoded).as_deref(), Some(input.as_bytes()));
        }
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64_decode("Zm8"), None); // unpadded
        assert_eq!(base64_decode("Zm8*"), None); // out of alphabet
        assert_eq!(base64_decode("Z=8="), None); // padding before data
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
        assert_eq!(decode_header_value("=?base64?not!base64?="), None);
    }
}

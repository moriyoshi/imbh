//! Just enough JSON to speak the plugin protocol.
//!
//! Docker's plugin requests and responses are small, flat JSON documents. Reading them goes through
//! `imbh::parse_json` — the facade's dependency-free parser (ARCHITECTURE.md §10.4) — and writing
//! them is string concatenation over [`crate::json_string`]. No `serde_json`: the `docker` feature
//! must not add a crate to the graph, and the shapes here are a handful of known keys.

use imbh::AnyValue;

/// Parse a request body. A malformed or non-object body yields an empty object rather than an
/// error, so every accessor below just returns `None` and the handler answers with a plugin-level
/// `Err` instead of dropping the connection.
pub fn parse(body: &[u8]) -> AnyValue {
    std::str::from_utf8(body)
        .ok()
        .and_then(imbh::parse_json)
        .filter(|v| matches!(v, AnyValue::Map(_)))
        .unwrap_or_else(|| AnyValue::Map(Vec::new()))
}

/// Parse a document that may be an **array** at the top level, unlike [`parse`], which is for the
/// plugin protocol's always-object requests. The Engine API's `/networks` is a JSON array; anything
/// unparseable degrades to `Null`, which every accessor below reads as absent.
pub fn parse_any(body: &[u8]) -> AnyValue {
    std::str::from_utf8(body)
        .ok()
        .and_then(imbh::parse_json)
        .unwrap_or(AnyValue::Null)
}

/// The elements of a JSON array, or an empty slice for anything else.
pub fn items(v: &AnyValue) -> &[AnyValue] {
    match v {
        AnyValue::Array(items) => items,
        _ => &[],
    }
}

/// The value at `key` of a JSON object.
pub fn field<'a>(v: &'a AnyValue, key: &str) -> Option<&'a AnyValue> {
    match v {
        AnyValue::Map(pairs) => pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v),
        _ => None,
    }
}

/// The string at `key`, or `""` when absent or not a string. Docker omits empty fields, so
/// "absent" and "empty" mean the same thing everywhere in this protocol.
pub fn string(v: &AnyValue, key: &str) -> String {
    match field(v, key) {
        Some(AnyValue::Str(s)) => s.clone(),
        _ => String::new(),
    }
}

/// The integer at `key`. Accepts a JSON double too — Go marshals whole numbers as integers, but a
/// hand-rolled client (or a test) may not.
pub fn int(v: &AnyValue, key: &str) -> Option<i64> {
    match field(v, key)? {
        AnyValue::Int(i) => Some(*i),
        AnyValue::Double(d) => Some(*d as i64),
        _ => None,
    }
}

/// The boolean at `key`, defaulting to `false`.
pub fn bool_at(v: &AnyValue, key: &str) -> bool {
    matches!(field(v, key), Some(AnyValue::Bool(true)))
}

/// The `map[string]string` at `key` as ordered pairs (Docker's `Config` / `ContainerLabels`).
/// Non-string values are skipped rather than stringified — they cannot occur in this protocol.
pub fn string_map(v: &AnyValue, key: &str) -> Vec<(String, String)> {
    match field(v, key) {
        Some(AnyValue::Map(pairs)) => pairs
            .iter()
            .filter_map(|(k, v)| match v {
                AnyValue::Str(s) => Some((k.clone(), s.clone())),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// The `[]string` at `key` (Docker's `ContainerEnv` / `ContainerArgs`).
pub fn string_list(v: &AnyValue, key: &str) -> Vec<String> {
    match field(v, key) {
        Some(AnyValue::Array(items)) => items
            .iter()
            .filter_map(|v| match v {
                AnyValue::Str(s) => Some(s.clone()),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// The plugin protocol's universal reply: `{"Err": ""}` on success, the message otherwise.
pub fn err_response(message: Option<&str>) -> Vec<u8> {
    format!("{{\"Err\":{}}}", crate::json_string(message.unwrap_or(""))).into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    const INFO: &str = r#"{
        "File": "/run/docker/logging/abc",
        "Info": {
            "ContainerID": "deadbeef",
            "ContainerName": "/web",
            "ContainerLabels": {"app": "cart", "tier": "front"},
            "ContainerEnv": ["A=1", "B=2"],
            "Config": {"imbh-service": "checkout"}
        },
        "Config": {"Tail": 100, "Follow": true}
    }"#;

    #[test]
    fn reads_the_shapes_the_protocol_uses() {
        let v = parse(INFO.as_bytes());
        assert_eq!(string(&v, "File"), "/run/docker/logging/abc");

        let info = field(&v, "Info").expect("Info object");
        assert_eq!(string(info, "ContainerID"), "deadbeef");
        assert_eq!(string(info, "ContainerName"), "/web");
        assert_eq!(string(info, "Missing"), "");
        assert_eq!(
            string_map(info, "ContainerLabels"),
            vec![
                ("app".to_owned(), "cart".to_owned()),
                ("tier".to_owned(), "front".to_owned())
            ]
        );
        assert_eq!(string_list(info, "ContainerEnv"), vec!["A=1", "B=2"]);
        assert_eq!(string_map(info, "Config").len(), 1);

        let config = field(&v, "Config").expect("Config object");
        assert_eq!(int(config, "Tail"), Some(100));
        assert!(bool_at(config, "Follow"));
        assert!(!bool_at(config, "Absent"));
    }

    #[test]
    fn malformed_bodies_degrade_to_an_empty_object() {
        for body in [&b"not json"[..], b"[1,2,3]", b"", b"\xff\xfe"] {
            let v = parse(body);
            assert!(matches!(&v, AnyValue::Map(m) if m.is_empty()));
            assert_eq!(string(&v, "File"), "");
        }
    }

    #[test]
    fn err_response_escapes_the_message() {
        assert_eq!(err_response(None), br#"{"Err":""}"#.to_vec());
        assert_eq!(
            err_response(Some("bad \"path\"")),
            br#"{"Err":"bad \"path\""}"#.to_vec()
        );
    }
}

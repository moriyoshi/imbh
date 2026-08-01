//! The head API's wire types.
//!
//! Three kinds of type appear here, and the split is deliberate:
//!
//! * **Reused facade types.** [`imbh::LogQuery`], [`imbh::LogPage`], [`imbh::Trace`],
//!   [`imbh::MetricMeta`] and [`imbh::VolumeBucket`] already carry `Serialize`/`Deserialize` behind
//!   the facade's `serde` feature (ARCHITECTURE.md §10.13), so a remote head ships the *same value*
//!   its local twin would have handed to `Db` — there is no second description of a log query to
//!   drift out of step with the first.
//! * **Mirrors of `imbh-lgtm` types** ([`EvalWindow`], [`EvalCaps`]) — plain-data copies of
//!   `EvalRange`/`EvalLimits`, which carry no serde derives. Mirroring rather than deriving on them
//!   keeps the semantic crate free of a wire contract it would then have to keep.
//! * **Head-owned results** ([`Series`], [`TraceSearch`], [`Stats`], [`ExemplarPoint`]) — flattened,
//!   owned shapes for results whose native forms borrow from the Arrow batch they were read out of
//!   (`PromSeries<'a>`, `LogSeries<'a>`) or hold no derive at all (`DbStats`).
//!
//! # Why floats are not plain JSON numbers
//!
//! JSON has no `NaN`, `Infinity`, or `-Infinity`, and `serde_json` writes all three as `null` — which
//! then fails to *deserialize* back into an `f64`. A PromQL evaluation produces all three routinely
//! (`histogram_quantile` over an empty window, a division by zero), so encoding a sample value as a
//! bare number would turn an ordinary query result into a transport error. Every float on this wire
//! therefore goes through [`float`]: a finite value is a JSON number, and the three specials are the
//! strings `"NaN"`, `"Inf"`, and `"-Inf"` — the same spelling Prometheus' own JSON API uses.

use serde::{Deserialize, Serialize};

/// Evaluation timestamps and selector defaults for a PromQL/LogQL range evaluation, in unix
/// nanoseconds. Mirrors `imbh_lgtm::EvalRange`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvalWindow {
    pub start_ns: i64,
    pub end_ns: i64,
    pub step_ns: u64,
    pub lookback_ns: u64,
}

/// The caps an evaluation runs under. Mirrors `imbh_lgtm::EvalLimits`; every field is optional on
/// the wire so a head may send only the caps it cares about and inherit the engine's defaults for
/// the rest.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvalCaps {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_evaluation_points: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_series: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_samples: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_spans: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_traces: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_recursion: Option<usize>,
}

// ── requests ────────────────────────────────────────────────────────────────────────────────────

/// `POST /api/head/metrics/promql` and `POST /api/head/logs/logql`: evaluate queries over a window.
///
/// The queries are *source text*, not translated expressions — translation is part of what the head
/// delegates, so a head and the daemon can never disagree about what a query means.
///
/// Several at once because a head routinely asks for several: the metric catalog emits one selector
/// per checked metric, and the evaluator has no `or`, so each must run on its own. Sending them
/// together is one request and, more to the point, **one** metric-catalog read — the catalog is what
/// PromQL translation resolves a selector's kind against, so a query apiece would re-read it apiece.
/// Their result series are concatenated in request order; each keeps its own `__name__` label, so
/// they stay distinguishable.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalRequest {
    pub queries: Vec<String>,
    pub window: EvalWindow,
    #[serde(default)]
    pub caps: EvalCaps,
}

impl EvalRequest {
    /// The common case: one query.
    pub fn one(query: impl Into<String>, window: EvalWindow, caps: EvalCaps) -> EvalRequest {
        EvalRequest {
            queries: vec![query.into()],
            window,
            caps,
        }
    }
}

/// `POST /api/head/traces/search`: a TraceQL query over `[start_ns, end_ns]`.
///
/// `narrow_steps` asks the daemon to retry inside progressively more recent sub-windows when the
/// full one overflows [`EvalCaps::max_traces`] — the trace cap applies to *candidate* traces in the
/// window, before the predicate runs, so a busy window overflows however selective the query is.
/// Zero disables the retry and surfaces the limit error instead.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceSearchRequest {
    pub query: String,
    pub start_ns: i64,
    pub end_ns: i64,
    #[serde(default)]
    pub caps: EvalCaps,
    #[serde(default)]
    pub narrow_steps: usize,
}

/// `POST /api/head/traces/get`: one trace by hex id.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceGetRequest {
    pub trace_id: String,
}

/// `POST /api/head/logs/query`: the facade's own [`imbh::LogQuery`], verbatim.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogQueryRequest {
    pub query: imbh::LogQuery,
}

/// `POST /api/head/logs/volume`: log counts per `step_ns` bucket over the query's range.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogVolumeRequest {
    pub query: imbh::LogQuery,
    pub step_ns: u64,
}

/// `POST /api/head/metrics/exemplars`: one metric's exemplars.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExemplarsRequest {
    pub metric: String,
}

/// `POST /api/head/attributes/values`: the distinct values of one attribute key. A `POST` rather
/// than a query string because an attribute key is arbitrary user data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttributeValuesRequest {
    pub key: String,
}

// ── responses ───────────────────────────────────────────────────────────────────────────────────

/// One evaluated series, from either PromQL or LogQL. Labels are a sorted `(name, value)` list — the
/// owned form of `imbh_lgtm::LabelSet`, which borrows from the Arrow batch it was read out of.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Series {
    pub labels: Vec<Label>,
    pub samples: Vec<SamplePoint>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Label {
    pub name: String,
    pub value: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SamplePoint {
    pub timestamp_ns: i64,
    #[serde(with = "float")]
    pub value: f64,
}

/// One trace matched by a TraceQL query.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TraceMatch {
    pub trace_id: String,
    /// The trace's earliest span start, so a head can show *when* a match happened without a second
    /// fetch.
    pub start_time_ns: i64,
    /// The hex span ids the query's final spanset selected.
    pub selected_span_ids: Vec<String>,
}

/// The answer to `POST /api/head/traces/search`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TraceSearch {
    pub matches: Vec<TraceMatch>,
    /// The window start actually searched. Equal to the request's `start_ns` unless the trace cap
    /// forced a narrower, more recent sub-window — which is the one thing a head must say out loud,
    /// since the answer is then not "no more matches" but "not the whole range".
    pub effective_start_ns: i64,
}

/// The answer to `POST /api/head/logs/volume`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogVolumeResult {
    pub buckets: Vec<imbh::VolumeBucket>,
}

/// The answer to `GET /api/head/metrics/catalog`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricCatalog {
    pub metrics: Vec<imbh::MetricMeta>,
}

/// One exemplar, flattened to hex ids. The facade's [`imbh::Exemplar`] would serialize directly, but
/// its `value` is a bare `f64` — see the float note in the module docs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExemplarPoint {
    pub time_unix_nano: i64,
    #[serde(with = "float")]
    pub value: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub span_id: Option<String>,
    /// The exemplar's `filtered_attributes` as canonical JSON (empty when none).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub attributes: String,
}

/// The answer to `POST /api/head/metrics/exemplars`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Exemplars {
    pub exemplars: Vec<ExemplarPoint>,
}

/// The answer to `GET /api/head/attributes/keys` and `POST /api/head/attributes/values`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Names {
    pub names: Vec<String>,
}

/// The answer to `GET /api/head/stats` — [`imbh::DbStats`], which carries no derive of its own.
///
/// Deliberately not `GET /stats`: that endpoint's hand-written JSON is an existing public contract
/// that reports neither the ingest-queue gauges nor anything a head could parse back into a typed
/// value, and widening it would change what every current consumer sees.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stats {
    pub tables: Vec<TableStats>,
    pub buffer_bytes: u64,
    pub wal_bytes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub durable_lsn: Option<u64>,
    pub ingest_queue_depth: u64,
    pub ingest_dropped: u64,
    pub ingest_errors: u64,
}

/// Per-table counts and time span. `table` is the physical table name (`logs`, `metrics_gauge`, …),
/// the same spelling `imbh::Table::as_str` produces and SQL uses.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TableStats {
    pub table: String,
    pub segment_count: u64,
    pub segment_rows: u64,
    pub buffer_rows: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_time_unix_nano: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_time_unix_nano: Option<i64>,
}

/// The body every head failure arrives in, matching the `{"error": ...}` shape the rest of `imbhd`
/// answers with. `kind` is what lets a head tell a *cap* from any other failure — the trace-search
/// narrowing has to retry on one and give up on the other.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorBody {
    pub error: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
}

/// [`ErrorBody::kind`] for a failure caused by an evaluation cap rather than by the query itself.
pub const KIND_LIMIT_EXCEEDED: &str = "limit_exceeded";

// ── float codec ─────────────────────────────────────────────────────────────────────────────────

/// `f64` on the wire: a JSON number when finite, and `"NaN"` / `"Inf"` / `"-Inf"` otherwise. See the
/// module docs for why a bare number will not do.
pub mod float {
    use std::fmt;

    use serde::de::{Deserializer, Error, Unexpected, Visitor};
    use serde::ser::Serializer;

    pub fn serialize<S: Serializer>(value: &f64, serializer: S) -> Result<S::Ok, S::Error> {
        if value.is_finite() {
            serializer.serialize_f64(*value)
        } else if value.is_nan() {
            serializer.serialize_str("NaN")
        } else if *value > 0.0 {
            serializer.serialize_str("Inf")
        } else {
            serializer.serialize_str("-Inf")
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<f64, D::Error> {
        deserializer.deserialize_any(FloatVisitor)
    }

    struct FloatVisitor;

    impl Visitor<'_> for FloatVisitor {
        type Value = f64;

        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("a JSON number, null, or one of \"NaN\", \"Inf\", \"-Inf\"")
        }

        fn visit_f64<E: Error>(self, value: f64) -> Result<f64, E> {
            Ok(value)
        }

        fn visit_i64<E: Error>(self, value: i64) -> Result<f64, E> {
            Ok(value as f64)
        }

        fn visit_u64<E: Error>(self, value: u64) -> Result<f64, E> {
            Ok(value as f64)
        }

        fn visit_str<E: Error>(self, value: &str) -> Result<f64, E> {
            match value {
                "NaN" => Ok(f64::NAN),
                "Inf" | "+Inf" => Ok(f64::INFINITY),
                "-Inf" => Ok(f64::NEG_INFINITY),
                other => Err(E::invalid_value(
                    Unexpected::Str(other),
                    &"one of \"NaN\", \"Inf\", \"-Inf\"",
                )),
            }
        }

        /// `null` is what `serde_json` writes for a non-finite float, so a peer that encoded one as
        /// a plain number reads back as `NaN` rather than as a transport failure.
        fn visit_unit<E: Error>(self) -> Result<f64, E> {
            Ok(f64::NAN)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_finite_sample_values_survive_the_round_trip() {
        let series = Series {
            labels: vec![Label {
                name: "__name__".to_owned(),
                value: "up".to_owned(),
            }],
            samples: vec![
                SamplePoint {
                    timestamp_ns: 1,
                    value: 1.5,
                },
                SamplePoint {
                    timestamp_ns: 2,
                    value: f64::NAN,
                },
                SamplePoint {
                    timestamp_ns: 3,
                    value: f64::INFINITY,
                },
                SamplePoint {
                    timestamp_ns: 4,
                    value: f64::NEG_INFINITY,
                },
            ],
        };
        let json = serde_json::to_string(&series).expect("serialize");
        // A finite value stays a JSON number; the three specials are spelled the way Prometheus' own
        // JSON API spells them, so the payload stays readable by eye.
        assert!(json.contains("\"value\":1.5"), "{json}");
        assert!(json.contains("\"NaN\""), "{json}");
        assert!(json.contains("\"Inf\""), "{json}");
        assert!(json.contains("\"-Inf\""), "{json}");

        let back: Series = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.labels, series.labels);
        assert_eq!(back.samples[0].value, 1.5);
        assert!(back.samples[1].value.is_nan());
        assert_eq!(back.samples[2].value, f64::INFINITY);
        assert_eq!(back.samples[3].value, f64::NEG_INFINITY);
    }

    #[test]
    fn a_null_float_reads_back_as_nan() {
        // What a peer that serialized a non-finite float as a bare number would have written. It
        // must not fail the whole response.
        let point: SamplePoint =
            serde_json::from_str(r#"{"timestamp_ns":1,"value":null}"#).expect("null value");
        assert!(point.value.is_nan());
        // Anything else in that position is still an error, so a typo is not silently a NaN.
        assert!(
            serde_json::from_str::<SamplePoint>(r#"{"timestamp_ns":1,"value":"nope"}"#).is_err()
        );
    }

    #[test]
    fn caps_are_optional_field_by_field() {
        // A head that only cares about the series cap sends only that one, and the daemon fills in
        // the engine defaults for the rest.
        let caps: EvalCaps = serde_json::from_str(r#"{"max_series":7}"#).expect("partial caps");
        assert_eq!(caps.max_series, Some(7));
        assert_eq!(caps.max_traces, None);
        assert_eq!(
            serde_json::to_string(&caps).expect("serialize"),
            r#"{"max_series":7}"#
        );
        // And an absent `caps` object entirely is the all-defaults case.
        let request: EvalRequest = serde_json::from_str(
            r#"{"queries":["up"],"window":{"start_ns":0,"end_ns":1,"step_ns":1,"lookback_ns":1}}"#,
        )
        .expect("no caps");
        assert_eq!(request.caps, EvalCaps::default());
    }

    #[test]
    fn an_error_body_carries_the_kind_a_head_retries_on() {
        let body = ErrorBody {
            error: "TraceQL source traces limit exceeded".to_owned(),
            kind: Some(KIND_LIMIT_EXCEEDED.to_owned()),
        };
        let json = serde_json::to_string(&body).expect("serialize");
        assert_eq!(
            json,
            r#"{"error":"TraceQL source traces limit exceeded","kind":"limit_exceeded"}"#
        );
        // The plain `{"error": ...}` shape the rest of imbhd answers with parses too, with no kind.
        let plain: ErrorBody = serde_json::from_str(r#"{"error":"nope"}"#).expect("plain");
        assert_eq!(plain.kind, None);
    }
}

//! The **response** half of OTLP/HTTP ingest (ARCHITECTURE.md §10.16).
//!
//! `POST /v1/{logs,traces,metrics}` answers what the OTLP specification prescribes, which is not a
//! shape this server gets to choose: on success an `Export<signal>ServiceResponse` message, on
//! failure a `google.rpc.Status` message, in the **same encoding the request arrived in** and with
//! the matching `Content-Type`. A full success is therefore a zero-byte body — an empty protobuf
//! message — and `partial_success` is populated only when records were actually rejected, because
//! the spec says a client may log a partial success and one on every export is noise.
//!
//! This module exists because the endpoint used to answer its own `{"accepted":…}` JSON to every
//! request regardless of encoding. A stock exporter then fails to parse the body: in
//! `@opentelemetry/exporter-logs-otlp-proto` the export is still reported successful, but every
//! batch logs *"Export succeeded but could not deserialize response - is the response specification
//! compliant?"* — and, worse than the noise, a rejected-record count had nowhere to go, so the
//! duplicate signal OTLP/gRPC reports in `partial_success` was invisible to every HTTP client.
//!
//! The receipt's own counters (`accepted`/`rejected`/`durable`/`queued`) did not stop being useful,
//! so they moved to `x-imbh-*` **response headers** ([`ACCEPTED`] and friends): a `curl` user and
//! this crate's own tests keep the whole receipt, and the body stays the one the spec defines.
//!
//! Dependency note: `prost` and `opentelemetry-proto` are already in this crate's default graph
//! (through `imbh` → `imbh-otlp`), so naming them directly adds **no crate** — measured 298 → 298.

use opentelemetry_proto::tonic::collector::logs::v1::{
    ExportLogsPartialSuccess, ExportLogsServiceResponse,
};
use opentelemetry_proto::tonic::collector::metrics::v1::{
    ExportMetricsPartialSuccess, ExportMetricsServiceResponse,
};
use opentelemetry_proto::tonic::collector::trace::v1::{
    ExportTracePartialSuccess, ExportTraceServiceResponse,
};
use prost::Message;
use serde_json::json;

use axum::http::{HeaderMap, header};

/// `Content-Type: application/x-protobuf` — the binary encoding, and the only one this server
/// *decodes*. The spec requires the client and the server to set it on a protobuf payload.
pub(crate) const PROTOBUF: &str = "application/x-protobuf";
/// `Content-Type: application/json` — OTLP's JSON encoding (protobuf's canonical JSON mapping).
pub(crate) const JSON: &str = "application/json";

/// The receipt counters, as response headers. Not part of OTLP: a header is the one place left to
/// say something the spec's response message has no field for, and a header is also ignorable, which
/// is the right property for a value no OTLP client knows to look for.
pub(crate) const ACCEPTED: &str = "x-imbh-accepted";
pub(crate) const REJECTED: &str = "x-imbh-rejected";
pub(crate) const DURABLE: &str = "x-imbh-durable";
pub(crate) const QUEUED: &str = "x-imbh-queued";

/// Why a record was rejected, reported in `partial_success.error_message`. Shared with the OTLP/gRPC
/// handler (`grpc.rs`) so both transports explain the same rejection the same way.
pub(crate) const REJECTED_MESSAGE: &str =
    "duplicate (series, timestamp) rejected by the database's duplicate policy";

/// The encoding an OTLP/HTTP request declared — and therefore the encoding its response must use.
/// "The server MUST use the same `Content-Type` in the response as it received in the request."
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Encoding {
    Protobuf,
    Json,
}

impl Encoding {
    /// Read the request's encoding off its `Content-Type`.
    ///
    /// Anything that is not JSON is protobuf, **including a request with no `Content-Type` at all**:
    /// binary protobuf is the only payload this server decodes, so it is the only thing a body that
    /// got as far as a response could have been. (The spec requires the client to send the header;
    /// one that does not is already outside it, and guessing protobuf is the guess that matches the
    /// bytes we just parsed.)
    pub(crate) fn of(headers: &HeaderMap) -> Encoding {
        let content_type = headers
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok());
        match content_type {
            Some(value) if crate::is_json_content_type(value) => Encoding::Json,
            _ => Encoding::Protobuf,
        }
    }

    pub(crate) fn content_type(self) -> &'static str {
        match self {
            Encoding::Protobuf => PROTOBUF,
            Encoding::Json => JSON,
        }
    }
}

/// Which signal a `/v1/*` route ingests: which `Export<signal>ServiceResponse` it must answer with,
/// and which `rejected_*` field of that message a partial success goes in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Signal {
    Logs,
    Traces,
    Metrics,
}

impl Signal {
    /// The success response body: an `Export<signal>ServiceResponse`, with `partial_success` set
    /// only when `rejected > 0`.
    ///
    /// A full success encodes to **zero bytes** — an empty protobuf message is empty on the wire —
    /// which is exactly what a compliant collector answers and what a client's deserializer expects.
    pub(crate) fn response(self, encoding: Encoding, rejected: u64) -> Vec<u8> {
        match encoding {
            Encoding::Protobuf => self.response_protobuf(rejected),
            Encoding::Json => self.response_json(rejected),
        }
    }

    fn response_protobuf(self, rejected: u64) -> Vec<u8> {
        // `rejected` is a count of records and cannot realistically exceed `i64::MAX`, but a
        // saturating cast beats a wrap into a negative rejection count.
        let rejected = rejected.min(i64::MAX as u64) as i64;
        let partial = rejected > 0;
        match self {
            Signal::Logs => ExportLogsServiceResponse {
                partial_success: partial.then(|| ExportLogsPartialSuccess {
                    rejected_log_records: rejected,
                    error_message: REJECTED_MESSAGE.to_owned(),
                }),
            }
            .encode_to_vec(),
            Signal::Traces => ExportTraceServiceResponse {
                partial_success: partial.then(|| ExportTracePartialSuccess {
                    rejected_spans: rejected,
                    error_message: REJECTED_MESSAGE.to_owned(),
                }),
            }
            .encode_to_vec(),
            Signal::Metrics => ExportMetricsServiceResponse {
                partial_success: partial.then(|| ExportMetricsPartialSuccess {
                    rejected_data_points: rejected,
                    error_message: REJECTED_MESSAGE.to_owned(),
                }),
            }
            .encode_to_vec(),
        }
    }

    /// The same message in protobuf's canonical JSON mapping: `lowerCamelCase` field names, a field
    /// left out when unset, and an `int64` rendered as a **string** (the mapping's rule — a JSON
    /// number cannot carry every `int64` exactly, so every conformant encoder quotes them).
    fn response_json(self, rejected: u64) -> Vec<u8> {
        if rejected == 0 {
            return b"{}".to_vec();
        }
        let count = rejected.to_string();
        let partial = match self {
            Signal::Logs => json!({"rejectedLogRecords": count, "errorMessage": REJECTED_MESSAGE}),
            Signal::Traces => json!({"rejectedSpans": count, "errorMessage": REJECTED_MESSAGE}),
            Signal::Metrics => {
                json!({"rejectedDataPoints": count, "errorMessage": REJECTED_MESSAGE})
            }
        };
        // Serializing an object of strings and numbers cannot fail.
        serde_json::to_vec(&json!({ "partialSuccess": partial })).unwrap_or_else(|_| b"{}".to_vec())
    }
}

/// The subset of `google.rpc.Status` an OTLP failure body needs: the spec requires every `4xx`/`5xx`
/// response to carry one. Declared here rather than pulled from `tonic-types` because that crate is
/// not in the graph (and the `grpc` feature's `tonic::Status` is the *gRPC* status, a different
/// thing); the two fields below are wire-compatible with the real message, and `details` — the only
/// field left out — is optional and empty in every status this server produces.
#[derive(Clone, PartialEq, Message)]
pub(crate) struct Status {
    #[prost(int32, tag = "1")]
    pub(crate) code: i32,
    #[prost(string, tag = "2")]
    pub(crate) message: String,
}

/// `google.rpc.Code` values for the §10.3 error classifiers, matching what `grpc.rs` maps the same
/// errors to: a client reading the HTTP body and one reading a gRPC trailer see the same code.
pub(crate) const CODE_INVALID_ARGUMENT: i32 = 3;
pub(crate) const CODE_NOT_FOUND: i32 = 5;
pub(crate) const CODE_INTERNAL: i32 = 13;

impl Status {
    pub(crate) fn new(code: i32, message: &str) -> Status {
        Status {
            code,
            message: message.to_owned(),
        }
    }

    /// The failure body in the request's encoding.
    pub(crate) fn body(&self, encoding: Encoding) -> Vec<u8> {
        match encoding {
            Encoding::Protobuf => self.encode_to_vec(),
            // `code` is an `int32`, which the JSON mapping renders as a plain number (unlike the
            // `int64` counts above).
            Encoding::Json => {
                serde_json::to_vec(&json!({"code": self.code, "message": self.message}))
                    .unwrap_or_else(|_| b"{}".to_vec())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The property a client depends on: a full success is an empty message, so the body is empty
    /// and `partial_success` is absent once decoded.
    #[test]
    fn a_full_success_is_an_empty_message() {
        for signal in [Signal::Logs, Signal::Traces, Signal::Metrics] {
            assert!(
                signal.response(Encoding::Protobuf, 0).is_empty(),
                "{signal:?}"
            );
            assert_eq!(signal.response(Encoding::Json, 0), b"{}", "{signal:?}");
        }
        let decoded = ExportLogsServiceResponse::decode(
            Signal::Logs.response(Encoding::Protobuf, 0).as_slice(),
        )
        .expect("an empty body decodes as the response message");
        assert!(decoded.partial_success.is_none());
    }

    /// A rejection lands in the signal's own `rejected_*` field — the field a client reads to learn
    /// that some of what it sent did not make it.
    #[test]
    fn a_rejection_populates_the_signals_partial_success() {
        let logs = ExportLogsServiceResponse::decode(
            Signal::Logs.response(Encoding::Protobuf, 3).as_slice(),
        )
        .expect("decode");
        let partial = logs.partial_success.expect("partial success");
        assert_eq!(partial.rejected_log_records, 3);
        assert_eq!(partial.error_message, REJECTED_MESSAGE);

        let traces = ExportTraceServiceResponse::decode(
            Signal::Traces.response(Encoding::Protobuf, 2).as_slice(),
        )
        .expect("decode");
        assert_eq!(traces.partial_success.expect("partial").rejected_spans, 2);

        let metrics = ExportMetricsServiceResponse::decode(
            Signal::Metrics.response(Encoding::Protobuf, 1).as_slice(),
        )
        .expect("decode");
        assert_eq!(
            metrics
                .partial_success
                .expect("partial")
                .rejected_data_points,
            1
        );
    }

    /// The JSON mapping: `lowerCamelCase` keys, and an `int64` count as a **string**.
    #[test]
    fn the_json_mapping_quotes_int64_counts() {
        let body = Signal::Metrics.response(Encoding::Json, 7);
        let value: serde_json::Value = serde_json::from_slice(&body).expect("json");
        assert_eq!(value["partialSuccess"]["rejectedDataPoints"], "7");
        assert_eq!(value["partialSuccess"]["errorMessage"], REJECTED_MESSAGE);
    }

    /// A `Status` is a real `google.rpc.Status` on the wire: field 1 the code, field 2 the message.
    #[test]
    fn a_status_is_wire_compatible() {
        let encoded = Status::new(CODE_INVALID_ARGUMENT, "bad request").body(Encoding::Protobuf);
        let decoded = Status::decode(encoded.as_slice()).expect("decode");
        assert_eq!(decoded.code, CODE_INVALID_ARGUMENT);
        assert_eq!(decoded.message, "bad request");
        // Field 1 as a varint (tag 0x08), then field 2 as a length-delimited string (tag 0x12).
        assert_eq!(encoded[0], 0x08);
        assert_eq!(encoded[2], 0x12);

        let json: serde_json::Value =
            serde_json::from_slice(&Status::new(CODE_NOT_FOUND, "gone").body(Encoding::Json))
                .expect("json");
        assert_eq!(json, json!({"code": 5, "message": "gone"}));
        assert_eq!(CODE_INTERNAL, 13);
    }

    /// The encoding is the request's, and a request that declares nothing is protobuf — the only
    /// payload this server decodes.
    #[test]
    fn the_encoding_follows_the_request() {
        let with = |value: &str| {
            let mut headers = HeaderMap::new();
            headers.insert(header::CONTENT_TYPE, value.parse().unwrap());
            Encoding::of(&headers)
        };
        assert_eq!(Encoding::of(&HeaderMap::new()), Encoding::Protobuf);
        assert_eq!(with("application/x-protobuf"), Encoding::Protobuf);
        assert_eq!(
            with("application/x-protobuf; charset=utf-8"),
            Encoding::Protobuf
        );
        assert_eq!(with("application/json"), Encoding::Json);
        assert_eq!(with("application/json; charset=utf-8"), Encoding::Json);
        assert_eq!(Encoding::Protobuf.content_type(), PROTOBUF);
        assert_eq!(Encoding::Json.content_type(), JSON);
    }
}

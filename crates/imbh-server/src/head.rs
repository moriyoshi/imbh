//! `GET`/`POST /api/head/…` — the head API's HTTP transport (ARCHITECTURE.md §10.19).
//!
//! Every handler here is the same three steps: decode a JSON request body, run the matching
//! [`imbh_head::exec`] function under [`offload`], encode the result. Requests are always JSON;
//! responses are split by shape — the row-shaped results (the PromQL/LogQL matrices, the TraceQL
//! matches, a log page, a trace) go out as Arrow IPC, the small scalar ones as JSON. See
//! [`imbh_head::ipc`] for why: JSON has no `NaN` and no `±Inf`, which a PromQL evaluation produces
//! routinely. Failures are JSON either way, so a client has one error shape to read.
//!
//! The *semantics* — the query-language translation, the evaluation caps, the trace-window
//! narrowing — all live in `imbh-head`, because `imbh-tui`'s local backend calls exactly those
//! functions with no HTTP in between. A head therefore gets the same answers from a directory and
//! from a daemon; this module only carries them.
//!
//! Read-only: nothing below `/api/head` ingests, flushes, compacts, or applies retention. Like the
//! rest of `imbhd` (§10.16) it is unauthenticated, so a real deployment gates the prefix — which is
//! part of why the head API has a prefix of its own rather than being scattered through `/api/*`.

use std::sync::Arc;

use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::routing::{get, post};
use imbh::Db;
use imbh::arrow::array::RecordBatch;
use imbh_head::{HeadError, dto, exec, ipc, path};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::{Response, offload};

/// The head API's route table, over a shared `Db`.
///
/// Split out of [`app`](crate::app) so a host that wants only this surface — a UI's backing daemon
/// with no ingest and no admin actions — can mount it alone, and so a deployment that wants none of
/// it can leave it out.
pub fn routes() -> Router<Arc<Db>> {
    Router::new()
        .route(path::STATS, get(stats))
        .route(path::METRICS_CATALOG, get(metric_catalog))
        .route(path::METRICS_PROMQL, post(promql))
        .route(path::METRICS_EXEMPLARS, post(exemplars))
        .route(path::TRACES_SEARCH, post(traceql))
        .route(path::TRACES_GET, post(trace))
        .route(path::LOGS_QUERY, post(log_query))
        .route(path::LOGS_VOLUME, post(log_volume))
        .route(path::LOGS_LOGQL, post(logql))
        .route(path::ATTRIBUTES_KEYS, get(attribute_keys))
        .route(path::ATTRIBUTES_VALUES, post(attribute_values))
}

// ── handlers ────────────────────────────────────────────────────────────────────────────────────

async fn stats(State(db): State<Arc<Db>>) -> Response {
    respond(offload(exec::stats(&db)).await)
}

async fn metric_catalog(State(db): State<Arc<Db>>) -> Response {
    respond(offload(exec::metric_catalog(&db)).await)
}

async fn attribute_keys(State(db): State<Arc<Db>>) -> Response {
    respond(offload(exec::attribute_keys(&db)).await)
}

async fn promql(State(db): State<Arc<Db>>, body: Bytes) -> Response {
    let request: dto::EvalRequest = match decode(&body) {
        Ok(request) => request,
        Err(response) => return response,
    };
    respond_ipc(offload(exec::promql(&db, &request)).await, |value| {
        Ok(ipc::series_to_batch(value))
    })
}

async fn logql(State(db): State<Arc<Db>>, body: Bytes) -> Response {
    let request: dto::EvalRequest = match decode(&body) {
        Ok(request) => request,
        Err(response) => return response,
    };
    respond_ipc(offload(exec::logql(&db, &request)).await, |value| {
        Ok(ipc::series_to_batch(value))
    })
}

async fn exemplars(State(db): State<Arc<Db>>, body: Bytes) -> Response {
    let request: dto::ExemplarsRequest = match decode(&body) {
        Ok(request) => request,
        Err(response) => return response,
    };
    respond(offload(exec::exemplars(&db, &request)).await)
}

async fn traceql(State(db): State<Arc<Db>>, body: Bytes) -> Response {
    let request: dto::TraceSearchRequest = match decode(&body) {
        Ok(request) => request,
        Err(response) => return response,
    };
    respond_ipc(offload(exec::traceql(&db, &request)).await, |value| {
        Ok(ipc::trace_search_to_batch(value))
    })
}

async fn trace(State(db): State<Arc<Db>>, body: Bytes) -> Response {
    let request: dto::TraceGetRequest = match decode(&body) {
        Ok(request) => request,
        Err(response) => return response,
    };
    respond_ipc(offload(exec::trace(&db, &request)).await, |value| {
        Ok(ipc::trace_to_batch(value.as_ref()))
    })
}

async fn log_query(State(db): State<Arc<Db>>, body: Bytes) -> Response {
    let request: dto::LogQueryRequest = match decode(&body) {
        Ok(request) => request,
        Err(response) => return response,
    };
    respond_ipc(offload(exec::log_query(&db, &request)).await, |value| {
        ipc::log_page_to_batch(value)
    })
}

async fn log_volume(State(db): State<Arc<Db>>, body: Bytes) -> Response {
    let request: dto::LogVolumeRequest = match decode(&body) {
        Ok(request) => request,
        Err(response) => return response,
    };
    respond(offload(exec::log_volume(&db, &request)).await)
}

async fn attribute_values(State(db): State<Arc<Db>>, body: Bytes) -> Response {
    let request: dto::AttributeValuesRequest = match decode(&body) {
        Ok(request) => request,
        Err(response) => return response,
    };
    respond(offload(exec::attribute_values(&db, &request)).await)
}

// ── shared plumbing ─────────────────────────────────────────────────────────────────────────────

/// Decode a request body, or the `400` describing why it could not be. serde's message names the
/// offending field and position, which is what a head author needs and what a `400 Bad Request`
/// alone would not say.
fn decode<T: DeserializeOwned>(body: &Bytes) -> Result<T, Response> {
    serde_json::from_slice(body).map_err(|e| {
        error(&HeadError::bad_request(format!(
            "request body is not a valid head request: {e}"
        )))
    })
}

/// Encode an operation's row-shaped outcome as an Arrow IPC stream.
fn respond_ipc<T>(
    result: Result<T, HeadError>,
    to_batch: impl FnOnce(&T) -> Result<RecordBatch, HeadError>,
) -> Response {
    let encoded = result.and_then(|value| ipc::encode(&to_batch(&value)?));
    match encoded {
        Ok(body) => Response {
            status: 200,
            content_type: ipc::CONTENT_TYPE.to_owned(),
            body,
        },
        Err(e) => error(&e),
    }
}

/// Serialize an operation's scalar outcome as JSON. A serialization failure is this server's bug
/// rather than the caller's, so it is a `500` carrying the reason instead of a panic on a
/// connection task.
fn respond<T: Serialize>(result: Result<T, HeadError>) -> Response {
    match result {
        Ok(value) => match serde_json::to_vec(&value) {
            Ok(body) => Response::json(200, body),
            Err(e) => error(&HeadError::Api {
                status: 500,
                kind: None,
                message: format!("cannot serialize the head response: {e}"),
            }),
        },
        Err(e) => error(&e),
    }
}

/// A failure, in the same `{"error": …}` shape as every other `imbhd` endpoint — plus the `kind` a
/// head branches on (see [`dto::ErrorBody`]).
fn error(e: &HeadError) -> Response {
    let body = serde_json::to_vec(&e.body()).unwrap_or_else(|_| {
        // `ErrorBody` is two strings; this cannot fail, but a bare message beats an empty body.
        format!("{{\"error\":{}}}", crate::json_string(e.message())).into_bytes()
    });
    Response::json(e.status(), body)
}

//! The client half: driving a running `imbhd`'s head API over HTTP.
//!
//! One method per operation in [`exec`](crate::exec), with the same signature and the same result
//! type, so a head can hold either backend behind one interface and never branch on which it has.
//!
//! # Two response codecs
//!
//! Requests are always JSON — they are query *descriptions*, not tables. Responses are split: the
//! row-shaped results (the PromQL/LogQL matrices, the TraceQL matches, a log page, a trace) arrive
//! as [Arrow IPC](crate::ipc), and the small scalar ones (stats, the catalog, exemplars, attribute
//! vocabularies) as JSON. See [`ipc`](crate::ipc) for why: JSON cannot represent `NaN` or `±Inf`,
//! which a PromQL evaluation produces routinely. Failures are JSON in both cases, so a head has one
//! error shape to read whatever the success codec would have been.
//!
//! # No TLS
//!
//! An `https://` URL is refused rather than silently downgraded. `imbhd` serves plain HTTP
//! (ARCHITECTURE.md §10.16), and the deployment this is for is a head talking to a daemon on its own
//! machine or inside its own network. Terminating TLS in front of `imbhd` and pointing a head at the
//! terminator is the supported way to cross an untrusted network — which is why the refusal names
//! that option instead of just saying no.

use imbh::arrow::array::RecordBatch;
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::{HeadError, dto, path};

/// The port `imbhd` listens on unless told otherwise, used when a `--url` names no port.
const DEFAULT_PORT: u16 = 4318;

/// How long to wait for the TCP handshake. Bounded because a wrong `--url` should say so rather than
/// hang a head's first refresh. The *response* is deliberately unbounded: a legitimate query over a
/// large retention window can take a while, and a client-side deadline would turn a slow answer into
/// a wrong one.
const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// A head's connection to one running `imbhd`.
///
/// Cheap to clone — the underlying `reqwest::Client` is a handle onto a shared connection pool, and
/// pooling is the point: a single screen refresh issues several of these calls, and a head that
/// reconnected per call would pay a handshake for each.
#[derive(Debug, Clone)]
pub struct HeadClient {
    http: reqwest::Client,
    /// Everything before the operation path: `http://host:port`, plus any mount prefix the URL
    /// named. Never ends in `/`.
    base: String,
}

impl HeadClient {
    /// Connect to the `imbhd` a `--url` names.
    ///
    /// Accepts what a person would actually type: `127.0.0.1:4318`, `http://127.0.0.1:4318`,
    /// `localhost`, `[::1]:4318`, or a mount point behind a reverse proxy
    /// (`http://gateway/imbh`). A missing scheme is `http://` and a missing port is
    /// [`DEFAULT_PORT`], so the common case is just the address `imbhd` was started on.
    ///
    /// Nothing is contacted here; the first request is what discovers whether anything is listening.
    pub fn new(url: &str) -> Result<HeadClient, String> {
        let base = parse_base(url, DEFAULT_PORT)?;
        let http = reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .build()
            .map_err(|e| format!("cannot build an HTTP client: {e}"))?;
        Ok(HeadClient { http, base })
    }

    /// The daemon this head is pointed at, for the banner a person reads when a session starts.
    pub fn url(&self) -> &str {
        &self.base
    }

    /// A `POST` whose answer is JSON.
    async fn post_json<Q: Serialize, R: DeserializeOwned>(
        &self,
        path: &str,
        request: &Q,
    ) -> Result<R, HeadError> {
        let body = self.post(path, request).await?;
        self.json(path, &body)
    }

    /// A `POST` whose answer is an Arrow IPC stream, handed to `read` to turn back into a result.
    async fn post_ipc<Q: Serialize, R>(
        &self,
        path: &str,
        request: &Q,
        read: impl Fn(&RecordBatch) -> Result<R, HeadError>,
    ) -> Result<R, HeadError> {
        let body = self.post(path, request).await?;
        read(&crate::ipc::decode(&body)?)
    }

    async fn get_json<R: DeserializeOwned>(&self, path: &str) -> Result<R, HeadError> {
        let response = self
            .http
            .get(format!("{}{path}", self.base))
            .send()
            .await
            .map_err(|e| self.unreachable(path, &e))?;
        let body = self.body(path, response).await?;
        self.json(path, &body)
    }

    async fn post<Q: Serialize>(&self, path: &str, request: &Q) -> Result<Vec<u8>, HeadError> {
        let response = self
            .http
            .post(format!("{}{path}", self.base))
            .json(request)
            .send()
            .await
            .map_err(|e| self.unreachable(path, &e))?;
        self.body(path, response).await
    }

    /// The response body on success, or the failure it describes. Failures are JSON whatever the
    /// success codec is, so this is the one place a status is turned into a [`HeadError`].
    async fn body(&self, path: &str, response: reqwest::Response) -> Result<Vec<u8>, HeadError> {
        let status = response.status().as_u16();
        let body = response
            .bytes()
            .await
            .map_err(|e| HeadError::Transport(format!("{}{path}: {e}", self.base)))?;
        if (200..300).contains(&status) {
            return Ok(body.into());
        }
        // The daemon's own `{"error": ...}` is the best message there is; anything else (a proxy's
        // error page, a 404 from an older `imbhd` that has no head API) is reported as this
        // request's failure, with enough of the body to recognise it by.
        Err(match serde_json::from_slice::<dto::ErrorBody>(&body) {
            Ok(error) => HeadError::Api {
                status,
                kind: error.kind,
                message: error.error,
            },
            Err(_) => HeadError::Api {
                status,
                kind: None,
                message: format!(
                    "{}{path} answered HTTP {status}: {}",
                    self.base,
                    truncate(&String::from_utf8_lossy(&body))
                ),
            },
        })
    }

    fn json<R: DeserializeOwned>(&self, path: &str, body: &[u8]) -> Result<R, HeadError> {
        serde_json::from_slice(body).map_err(|e| {
            HeadError::Transport(format!(
                "{}{path} answered something this head cannot read: {e}",
                self.base
            ))
        })
    }

    /// A connect/send failure. Worth its own message because the overwhelmingly common cause is that
    /// nothing is listening — which a bare "connection refused" does not tell a person how to fix.
    fn unreachable(&self, path: &str, error: &reqwest::Error) -> HeadError {
        HeadError::Transport(format!(
            "cannot reach the imbh head API at {}{path}: {error}",
            self.base
        ))
    }

    // ── operations ──────────────────────────────────────────────────────────────────────────────

    /// Database statistics. See [`exec::stats`](crate::exec::stats).
    pub async fn stats(&self) -> Result<dto::Stats, HeadError> {
        self.get_json(path::STATS).await
    }

    /// The metric catalog. See [`exec::metric_catalog`](crate::exec::metric_catalog).
    pub async fn metric_catalog(&self) -> Result<dto::MetricCatalog, HeadError> {
        self.get_json(path::METRICS_CATALOG).await
    }

    /// Evaluate a PromQL query. See [`exec::promql`](crate::exec::promql).
    pub async fn promql(&self, request: &dto::EvalRequest) -> Result<Vec<dto::Series>, HeadError> {
        self.post_ipc(path::METRICS_PROMQL, request, crate::ipc::series_from_batch)
            .await
    }

    /// One metric's groupable labels. See
    /// [`exec::metric_dimensions`](crate::exec::metric_dimensions).
    pub async fn metric_dimensions(
        &self,
        request: &dto::MetricDimensionsRequest,
    ) -> Result<dto::MetricDimensions, HeadError> {
        self.post_json(path::METRICS_DIMENSIONS, request).await
    }

    /// One metric's exemplars. See [`exec::exemplars`](crate::exec::exemplars).
    pub async fn exemplars(
        &self,
        request: &dto::ExemplarsRequest,
    ) -> Result<dto::Exemplars, HeadError> {
        self.post_json(path::METRICS_EXEMPLARS, request).await
    }

    /// Search traces with TraceQL. See [`exec::traceql`](crate::exec::traceql).
    pub async fn traceql(
        &self,
        request: &dto::TraceSearchRequest,
    ) -> Result<dto::TraceSearch, HeadError> {
        self.post_ipc(
            path::TRACES_SEARCH,
            request,
            crate::ipc::trace_search_from_batch,
        )
        .await
    }

    /// Fetch one complete trace. See [`exec::trace`](crate::exec::trace).
    pub async fn trace(
        &self,
        request: &dto::TraceGetRequest,
    ) -> Result<Option<imbh::Trace>, HeadError> {
        self.post_ipc(path::TRACES_GET, request, crate::ipc::trace_from_batch)
            .await
    }

    /// Run one native log query. See [`exec::log_query`](crate::exec::log_query).
    pub async fn log_query(
        &self,
        request: &dto::LogQueryRequest,
    ) -> Result<imbh::LogPage, HeadError> {
        self.post_ipc(path::LOGS_QUERY, request, crate::ipc::log_page_from_batch)
            .await
    }

    /// Bucketed log counts. See [`exec::log_volume`](crate::exec::log_volume).
    pub async fn log_volume(
        &self,
        request: &dto::LogVolumeRequest,
    ) -> Result<dto::LogVolumeResult, HeadError> {
        self.post_json(path::LOGS_VOLUME, request).await
    }

    /// Evaluate a LogQL metric expression. See [`exec::logql`](crate::exec::logql).
    pub async fn logql(&self, request: &dto::EvalRequest) -> Result<Vec<dto::Series>, HeadError> {
        self.post_ipc(path::LOGS_LOGQL, request, crate::ipc::series_from_batch)
            .await
    }

    /// Every attribute key. See [`exec::attribute_keys`](crate::exec::attribute_keys).
    pub async fn attribute_keys(&self) -> Result<dto::Names, HeadError> {
        self.get_json(path::ATTRIBUTES_KEYS).await
    }

    /// One attribute key's values. See [`exec::attribute_values`](crate::exec::attribute_values).
    pub async fn attribute_values(
        &self,
        request: &dto::AttributeValuesRequest,
    ) -> Result<dto::Names, HeadError> {
        self.post_json(path::ATTRIBUTES_VALUES, request).await
    }
}

/// Normalize a `--url` into the prefix every operation path is appended to.
///
/// A path in the URL is kept as a mount prefix rather than replaced, which is what lets `imbhd` be
/// addressed behind a reverse proxy that mounts it below the site root.
fn parse_base(url: &str, default_port: u16) -> Result<String, String> {
    let spec = url.trim();
    if spec.is_empty() {
        return Err("--url is empty".to_owned());
    }
    let rest = match spec.split_once("://") {
        Some(("http", rest)) => rest,
        Some((scheme, _)) => {
            return Err(format!(
                "--url scheme `{scheme}` is not supported: the imbh head API speaks plain HTTP \
                 only (imbhd serves no TLS). Terminate TLS in front of it and point --url at the \
                 terminator, or point --url at the daemon directly."
            ));
        }
        None => spec,
    };
    // Everything from the first `/` is the mount prefix; what precedes it is the authority.
    let (authority, mount) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    if authority.is_empty() {
        return Err(format!("--url `{spec}` names no host"));
    }
    if authority.contains('@') {
        return Err(format!(
            "--url `{spec}` carries credentials, which the imbh head API does not use"
        ));
    }
    // The `]` dance keeps an IPv6 literal's own colons from reading as a port separator.
    let has_port = match authority.rfind(']') {
        Some(bracket) => authority[bracket..].contains(':'),
        None => authority.contains(':'),
    };
    let authority = if has_port {
        authority.to_owned()
    } else {
        format!("{authority}:{default_port}")
    };
    Ok(format!("http://{authority}{}", mount.trim_end_matches('/')))
}

/// Keep a foreign error body (a proxy's HTML error page, say) from filling a head's status line.
fn truncate(text: &str) -> String {
    const MAX: usize = 200;
    let text = text.trim();
    if text.chars().count() <= MAX {
        return text.to_owned();
    }
    let kept: String = text.chars().take(MAX).collect();
    format!("{kept}…")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_parse_the_way_a_person_types_them() {
        let base = |spec: &str| parse_base(spec, 4318).expect(spec);
        assert_eq!(base("127.0.0.1:4318"), "http://127.0.0.1:4318");
        assert_eq!(base("http://127.0.0.1:4318"), "http://127.0.0.1:4318");
        assert_eq!(base("http://127.0.0.1:4318/"), "http://127.0.0.1:4318");
        // A missing port is imbhd's default, and an IPv6 literal's own colons are not a port.
        assert_eq!(base("localhost"), "http://localhost:4318");
        assert_eq!(base("[::1]"), "http://[::1]:4318");
        assert_eq!(base("http://[::1]:9"), "http://[::1]:9");
        // A mount prefix is kept, so every operation path composes below it.
        assert_eq!(base("http://gateway/imbh"), "http://gateway:4318/imbh");
        assert_eq!(base("http://gateway:80/imbh/"), "http://gateway:80/imbh");
    }

    #[test]
    fn operation_paths_compose_onto_the_base() {
        let base = parse_base("http://gateway:8080/imbh", 4318).expect("url");
        assert_eq!(
            format!("{base}{}", path::METRICS_PROMQL),
            "http://gateway:8080/imbh/api/head/metrics/promql"
        );
    }

    #[test]
    fn unsupported_urls_are_refused_with_a_reason() {
        // Silently downgrading https to http would send a head's queries in the clear, so the
        // refusal names the supported way across an untrusted network instead.
        let e = parse_base("https://example.com", 4318).unwrap_err();
        assert!(e.contains("Terminate TLS in front of it"), "{e}");
        assert!(parse_base("", 4318).is_err());
        assert!(parse_base("http:///api", 4318).is_err());
        assert!(parse_base("http://user:pw@host:4318", 4318).is_err());
    }

    #[test]
    fn a_foreign_error_body_is_truncated() {
        assert_eq!(truncate(&"x".repeat(500)).chars().count(), 201);
        assert_eq!(truncate("  short  "), "short");
    }
}

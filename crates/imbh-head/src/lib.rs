//! The **head API**: the read-only query surface a *head* drives a running `imbhd` over.
//!
//! A head is a user interface with no database of its own — `imbh-tui` is the one that ships, but
//! nothing here is specific to a terminal. Locally a head opens the directory itself; as a head it
//! asks a daemon, which matters because the daemon is the **writer**: a `Db::open_read_only` view
//! sees only what that writer has already sealed, while `imbhd` can answer out of its live buffer
//! too. The head API is also what lets the database live on another machine.
//!
//! ```text
//!   imbh-tui (local)                 imbh-tui --url http://host:4318
//!        │                                       │
//!        │ exec::*(db, req)             client::HeadClient (HTTP/1.1 + JSON)
//!        ▼                                       ▼
//!      Db                              imbhd  ──►  exec::*(db, req)  ──►  Db
//! ```
//!
//! # The three layers
//!
//! * [`dto`] — the wire types. Where the facade already has a `serde`-gated type for something
//!   (`LogQuery`, `LogPage`, `Trace`, `MetricMeta`), the wire *is* that type, so a remote head sends
//!   the same value its local twin would have handed to `Db`.
//! * [`exec`] — the operations, executed against an open [`Db`](imbh::Db). Both backends call it:
//!   `imbh-server` behind `POST /api/head/…`, and a local head directly. That is what makes the two
//!   modes answer identically rather than merely similarly — the query-language translation, the
//!   caps, and the trace-window narrowing all live on this side of the boundary.
//! * [`client`] — the HTTP client a remote head uses, one method per operation.
//!
//! # Not the MCP endpoint
//!
//! `POST /mcp` (ARCHITECTURE.md §10.16.1) answers the same database for an *agent*: its tools are
//! shaped for a model (a `since` window, prose-y descriptions, one JSON document per call), and its
//! results are deliberately lossy — no paging cursors, no per-sample matrices, no waterfall. A head
//! needs the typed surface instead. The two share the `Db` and nothing else.
//!
//! # Exposure
//!
//! Read-only: nothing here ingests, flushes, compacts, or applies retention. Like the rest of
//! `imbhd` (ARCHITECTURE.md §10.16) the endpoints are unauthenticated, so a real deployment gates
//! them; keep `imbhd` bound to `127.0.0.1` when the head is on the same machine.

#[cfg(feature = "client")]
pub mod client;
#[cfg(feature = "dto")]
pub mod dto;
#[cfg(feature = "exec")]
pub mod exec;
#[cfg(feature = "dto")]
pub mod ipc;

use std::fmt;

/// The paths the head API is served at, shared by the client that composes them and the server that
/// registers them so the two cannot drift.
pub mod path {
    /// Everything below this prefix is the head API, and nothing else is.
    pub const PREFIX: &str = "/api/head";

    pub const STATS: &str = "/api/head/stats";
    pub const METRICS_CATALOG: &str = "/api/head/metrics/catalog";
    pub const METRICS_PROMQL: &str = "/api/head/metrics/promql";
    pub const METRICS_EXEMPLARS: &str = "/api/head/metrics/exemplars";
    pub const TRACES_SEARCH: &str = "/api/head/traces/search";
    pub const TRACES_GET: &str = "/api/head/traces/get";
    pub const LOGS_QUERY: &str = "/api/head/logs/query";
    pub const LOGS_VOLUME: &str = "/api/head/logs/volume";
    pub const LOGS_LOGQL: &str = "/api/head/logs/logql";
    pub const ATTRIBUTES_KEYS: &str = "/api/head/attributes/keys";
    pub const ATTRIBUTES_VALUES: &str = "/api/head/attributes/values";
}

/// Why a head operation did not answer.
///
/// The split matters to a head's error handling: an [`Api`](HeadError::Api) failure is the
/// *database's* answer and reads the same locally and remotely, while
/// [`Transport`](HeadError::Transport) can only happen to a remote head and means the daemon was
/// never reached at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HeadError {
    /// The operation ran and failed. `status` is the HTTP status the head API answers with (and the
    /// one a local head would have got, had it gone over HTTP); `kind` is the machine-readable
    /// discriminator from [`dto::ErrorBody`], present only where a head needs to branch on it.
    Api {
        status: u16,
        kind: Option<String>,
        message: String,
    },
    /// The endpoint could not be reached, or answered something that is not a head response.
    Transport(String),
}

impl HeadError {
    /// A malformed request: a query that does not parse, an id that is not hex, a window that runs
    /// backwards. `400`, because no retry of the same request would do better.
    pub fn bad_request(message: impl Into<String>) -> Self {
        HeadError::Api {
            status: 400,
            kind: None,
            message: message.into(),
        }
    }

    /// The HTTP status this failure is answered with; `503` for a transport failure, which is what
    /// a proxy in front of an unreachable daemon would say.
    pub fn status(&self) -> u16 {
        match self {
            HeadError::Api { status, .. } => *status,
            HeadError::Transport(_) => 503,
        }
    }

    /// Whether an evaluation cap — not the query — is what failed. The trace search retries on this
    /// and gives up on anything else.
    pub fn is_limit_exceeded(&self) -> bool {
        matches!(
            self,
            HeadError::Api { kind: Some(kind), .. } if kind == "limit_exceeded"
        )
    }

    /// The message a head shows, without the transport framing.
    pub fn message(&self) -> &str {
        match self {
            HeadError::Api { message, .. } | HeadError::Transport(message) => message,
        }
    }
}

impl fmt::Display for HeadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.message())
    }
}

impl std::error::Error for HeadError {}

#[cfg(feature = "dto")]
impl HeadError {
    /// Map an imbh error using the §10.3 classifiers, exactly as `imbhd`'s other endpoints do: 404
    /// not-found, 400 user error, 500 otherwise.
    pub fn from_db(error: imbh::Error) -> Self {
        let status = if error.is_not_found() {
            404
        } else if error.is_user_error() {
            400
        } else {
            500
        };
        HeadError::Api {
            status,
            kind: None,
            message: error.to_string(),
        }
    }

    /// The body this failure is serialized as.
    pub fn body(&self) -> dto::ErrorBody {
        dto::ErrorBody {
            error: self.message().to_owned(),
            kind: match self {
                HeadError::Api { kind, .. } => kind.clone(),
                HeadError::Transport(_) => None,
            },
        }
    }
}

#[cfg(feature = "exec")]
impl HeadError {
    /// Map a semantic (query-language) error. A blown cap carries
    /// [`dto::KIND_LIMIT_EXCEEDED`] so a head can tell "ask for less" apart from "that query is
    /// wrong"; `Source` is the storage layer failing underneath, which is a `500`.
    pub fn from_semantic(error: imbh_lgtm::SemanticError) -> Self {
        use imbh_lgtm::SemanticError::*;
        let (status, kind) = match &error {
            LimitExceeded(_) => (400, Some(dto::KIND_LIMIT_EXCEEDED.to_owned())),
            InvalidRange | Incompatible(_) | Malformed(_) => (400, None),
            Source(_) => (500, None),
        };
        HeadError::Api {
            status,
            kind,
            message: error.to_string(),
        }
    }

    /// A semantic error raised while *building* the request (a backwards window), which is always
    /// the caller's fault whatever the variant says.
    pub fn from_semantic_request(error: imbh_lgtm::SemanticError) -> Self {
        HeadError::bad_request(error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_cap_failure_is_retryable() {
        let capped = HeadError::Api {
            status: 400,
            kind: Some("limit_exceeded".to_owned()),
            message: "too many traces".to_owned(),
        };
        assert!(capped.is_limit_exceeded());
        assert!(!HeadError::bad_request("bad query").is_limit_exceeded());
        assert!(!HeadError::Transport("connection refused".to_owned()).is_limit_exceeded());
    }

    #[test]
    fn an_unreachable_daemon_is_not_a_database_answer() {
        let down = HeadError::Transport("connection refused".to_owned());
        assert_eq!(down.status(), 503);
        assert_eq!(down.to_string(), "connection refused");
    }
}

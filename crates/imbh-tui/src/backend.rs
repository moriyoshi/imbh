//! Where the explorer's answers come from.
//!
//! The TUI is a **head**: a user interface with no database of its own. [`Backend`] is the one place
//! that knows which kind it is talking to, and every query in [`fetch`](crate::fetch) and
//! [`tasks`](crate::tasks) goes through it.
//!
//! * [`Backend::Local`] opens the directory in-process with `Db::open_read_only`. It takes no writer
//!   lock, so it reads *alongside* a running `imbhd` and needs nothing running at all — but what it
//!   cannot see is that writer's unsealed buffer, i.e. the most recent telemetry of all.
//! * [`Backend::Remote`] asks a running `imbhd` over the head API (ARCHITECTURE.md §10.19), which
//!   *can* see the live buffer, and which may be on another machine entirely.
//!
//! Both arms call the same functions. `imbh_head::exec` is the single implementation of every
//! operation over a `Db`; locally this module calls it directly, remotely `imbhd` calls it on the
//! other side of one HTTP request. That is deliberate and it is the whole reason the head API exists
//! as a crate rather than as a pile of routes: the query-language translation, the evaluation caps,
//! and the trace-window narrowing all happen in the same code either way, so the two modes cannot
//! answer the same question differently.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use imbh::{Db, LogPage, LogQuery, MetricMeta, Trace, VolumeBucket};
use imbh_head::client::HeadClient;
use imbh_head::{HeadError, dto, exec};
use imbh_lgtm::{EvalLimits, EvalRange};

/// How many progressively narrower windows to try after a TraceQL window overflows the trace cap.
/// Passed on every search; see [`imbh_head::exec::traceql`] for what the retries do.
pub(crate) const TRACE_NARROW_STEPS: usize = 6;

/// The explorer's data source.
///
/// Cheap to clone — both arms are an `Arc` — which is what lets the event loop hand a copy to every
/// background fetch it spawns.
#[derive(Clone)]
pub enum Backend {
    /// An imbh database directory, opened read-only in this process.
    Local(Arc<Db>),
    /// A running `imbhd`, reached over the head API.
    Remote(Arc<HeadClient>),
}

/// `Db` is not `Debug`, and a head never needs more than which kind of backend it holds and what
/// that backend is reading — which is exactly what [`Backend::describe`] says.
impl std::fmt::Debug for Backend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple(match self {
            Backend::Local(_) => "Backend::Local",
            Backend::Remote(_) => "Backend::Remote",
        })
        .field(&self.describe())
        .finish()
    }
}

impl From<Arc<Db>> for Backend {
    fn from(db: Arc<Db>) -> Backend {
        Backend::Local(db)
    }
}

impl Backend {
    /// Open a database directory read-only. Takes no writer lock, so this runs alongside a live
    /// `imbhd` on the same directory.
    pub fn open(path: impl AsRef<Path>) -> Result<Backend, imbh::Error> {
        Ok(Backend::Local(Db::open_read_only(path)?))
    }

    /// Point at a running `imbhd`. Nothing is contacted here — the first refresh is what discovers
    /// whether anything is listening, and it reports the failure in the panel rather than at
    /// startup, so a daemon that is merely slow to boot does not abort the session.
    pub fn connect(url: &str) -> Result<Backend, String> {
        Ok(Backend::Remote(Arc::new(HeadClient::new(url)?)))
    }

    /// What this session is reading, for the banner and the header.
    pub fn describe(&self) -> String {
        match self {
            Backend::Local(_) => "local database".to_owned(),
            Backend::Remote(client) => client.url().to_owned(),
        }
    }

    /// Whether answers include a live writer's unsealed buffer. Only a remote head sees one; a
    /// read-only opener is limited to what has been sealed to Parquet.
    pub fn sees_live_buffer(&self) -> bool {
        matches!(self, Backend::Remote(_))
    }

    // ── operations ──────────────────────────────────────────────────────────────────────────────

    pub(crate) async fn stats(&self) -> Result<dto::Stats, HeadError> {
        match self {
            Backend::Local(db) => exec::stats(db).await,
            Backend::Remote(client) => client.stats().await,
        }
    }

    pub(crate) async fn metric_catalog(&self) -> Result<Vec<MetricMeta>, HeadError> {
        match self {
            Backend::Local(db) => exec::metric_catalog(db).await,
            Backend::Remote(client) => client.metric_catalog().await,
        }
        .map(|catalog| catalog.metrics)
    }

    /// One metric's groupable labels and their values — the catalog tree's dimension axes. Read from
    /// the metric tables rather than evaluated, which is what makes it answer for a histogram too
    /// (see [`imbh_head::exec::metric_dimensions`]).
    pub(crate) async fn metric_dimensions(
        &self,
        metric: &str,
        max_values: usize,
    ) -> Result<Vec<dto::MetricDimension>, HeadError> {
        let request = dto::MetricDimensionsRequest {
            metric: metric.to_owned(),
            max_values: Some(max_values),
        };
        match self {
            Backend::Local(db) => exec::metric_dimensions(db, &request).await,
            Backend::Remote(client) => client.metric_dimensions(&request).await,
        }
        .map(|result| result.dimensions)
    }

    /// Evaluate one PromQL query.
    ///
    /// The head API takes a batch (and a batch costs one metric-catalog read rather than one
    /// apiece), but a batch answers with the series *concatenated* and PromQL aggregation drops
    /// `__name__`, so the caller could no longer tell which query produced which series. The catalog
    /// screen visualizes several metrics at once and must, so it issues them one at a time — a
    /// handful of round trips on an interactive refresh, in exchange for a series list that names
    /// what it is showing.
    pub(crate) async fn promql(
        &self,
        query: &str,
        range: EvalRange,
        limits: EvalLimits,
    ) -> Result<Vec<dto::Series>, HeadError> {
        let request = eval_request(std::slice::from_ref(&query.to_owned()), range, limits);
        match self {
            Backend::Local(db) => exec::promql(db, &request).await,
            Backend::Remote(client) => client.promql(&request).await,
        }
    }

    /// Evaluate a LogQL *metric* expression (a range aggregation). A bare selector filters a list
    /// instead and never reaches here — see [`Backend::log_query`].
    pub(crate) async fn logql(
        &self,
        query: &str,
        range: EvalRange,
        limits: EvalLimits,
    ) -> Result<Vec<dto::Series>, HeadError> {
        let request = eval_request(std::slice::from_ref(&query.to_owned()), range, limits);
        match self {
            Backend::Local(db) => exec::logql(db, &request).await,
            Backend::Remote(client) => client.logql(&request).await,
        }
    }

    /// Search traces with TraceQL, letting the executing side narrow the window toward `end` if the
    /// full one overflows the trace cap. The window actually searched comes back in
    /// [`dto::TraceSearch::effective_start_ns`], which is what the panel warns about.
    pub(crate) async fn traceql(
        &self,
        query: &str,
        start_ns: i64,
        end_ns: i64,
        limits: EvalLimits,
    ) -> Result<dto::TraceSearch, HeadError> {
        let request = dto::TraceSearchRequest {
            query: query.to_owned(),
            start_ns,
            end_ns,
            caps: exec::caps_of(limits),
            narrow_steps: TRACE_NARROW_STEPS,
        };
        match self {
            Backend::Local(db) => exec::traceql(db, &request).await,
            Backend::Remote(client) => client.traceql(&request).await,
        }
    }

    pub(crate) async fn trace(&self, trace_id_hex: &str) -> Result<Option<Trace>, HeadError> {
        let request = dto::TraceGetRequest {
            trace_id: trace_id_hex.to_owned(),
        };
        match self {
            Backend::Local(db) => exec::trace(db, &request).await,
            Backend::Remote(client) => client.trace(&request).await,
        }
    }

    pub(crate) async fn log_query(&self, query: LogQuery) -> Result<LogPage, HeadError> {
        let request = dto::LogQueryRequest { query };
        match self {
            Backend::Local(db) => exec::log_query(db, &request).await,
            Backend::Remote(client) => client.log_query(&request).await,
        }
    }

    pub(crate) async fn log_volume(
        &self,
        query: LogQuery,
        step: Duration,
    ) -> Result<Vec<VolumeBucket>, HeadError> {
        let request = dto::LogVolumeRequest {
            query,
            step_ns: u64::try_from(step.as_nanos()).unwrap_or(u64::MAX),
        };
        match self {
            Backend::Local(db) => exec::log_volume(db, &request).await,
            Backend::Remote(client) => client.log_volume(&request).await,
        }
        .map(|result| result.buckets)
    }

    pub(crate) async fn exemplars(
        &self,
        metric: &str,
    ) -> Result<Vec<dto::ExemplarPoint>, HeadError> {
        let request = dto::ExemplarsRequest {
            metric: metric.to_owned(),
        };
        match self {
            Backend::Local(db) => exec::exemplars(db, &request).await,
            Backend::Remote(client) => client.exemplars(&request).await,
        }
        .map(|result| result.exemplars)
    }

    pub(crate) async fn attribute_keys(&self) -> Result<Vec<String>, HeadError> {
        match self {
            Backend::Local(db) => exec::attribute_keys(db).await,
            Backend::Remote(client) => client.attribute_keys().await,
        }
        .map(|result| result.names)
    }

    pub(crate) async fn attribute_values(&self, key: &str) -> Result<Vec<String>, HeadError> {
        let request = dto::AttributeValuesRequest {
            key: key.to_owned(),
        };
        match self {
            Backend::Local(db) => exec::attribute_values(db, &request).await,
            Backend::Remote(client) => client.attribute_values(&request).await,
        }
        .map(|result| result.names)
    }
}

/// Every cap the panel computed travels explicitly: they are what the *user's* time range and row
/// limits mean, and inheriting a differently-configured daemon's defaults for some of them would
/// silently answer a different question than the one the header claims to show.
fn eval_request(queries: &[String], range: EvalRange, limits: EvalLimits) -> dto::EvalRequest {
    dto::EvalRequest {
        queries: queries.to_vec(),
        window: exec::window_of(range),
        caps: exec::caps_of(limits),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fetch::{build_waterfall_detail, load_snapshot};
    use crate::model::Screen;
    use imbh_test_support::otlp::{otlp_log, otlp_metrics, otlp_trace};

    /// An in-process database holding one of each signal, behind the same `Backend` the binary
    /// builds for a directory argument.
    async fn local() -> Backend {
        let db: Arc<Db> = Db::in_memory().open().expect("open in-memory db");
        db.ingest_otlp_logs(&otlp_log("cart", "checkout failed", 1_000))
            .await
            .expect("logs");
        db.ingest_otlp_traces(&otlp_trace("api", "GET /cart", 2, 1_000, 5_000, 2))
            .await
            .expect("traces");
        db.ingest_otlp_metrics(&otlp_metrics("cart"))
            .await
            .expect("metrics");
        // `From<Arc<Db>>` is the conversion an embedder calling `run(db, options)` goes through.
        Backend::from(db)
    }

    /// A window wide enough to contain the fixtures, whose timestamps are small absolute nanosecond
    /// values rather than "now".
    fn options() -> crate::model::Options {
        crate::model::Options {
            window: Some((0, 1_000_000_000)),
            ..crate::model::Options::default()
        }
    }

    async fn snapshot(
        backend: &Backend,
        screen: Screen,
        query: &str,
    ) -> Result<crate::model::Snapshot, String> {
        load_snapshot(backend.clone(), screen, query, &options(), None, None).await
    }

    /// `imbh-tui <directory>` opens the database in this process and answers out of it — no socket,
    /// no serialization, no daemon required. That path predates the head API and must survive it, so
    /// this drives every screen against an in-process `Db`.
    ///
    /// The remote half is covered by `imbh-server`'s `head_e2e`, which asserts the stronger
    /// property: that a head talking to a daemon sees exactly what this path sees.
    #[tokio::test]
    async fn a_local_backend_answers_out_of_the_process_it_runs_in() {
        let backend = local().await;
        assert!(
            !backend.sees_live_buffer(),
            "a read-only opener sees sealed segments only — that is what `--url` is for"
        );
        assert_eq!(backend.describe(), "local database");

        let overview = snapshot(&backend, Screen::Overview, "")
            .await
            .expect("overview");
        assert!(
            overview.lines.iter().any(|line| line.starts_with("logs")),
            "the overview lists the physical tables: {:?}",
            overview.lines
        );

        // An empty query is the metric catalog listing.
        let catalog = snapshot(&backend, Screen::Metrics, "")
            .await
            .expect("catalog");
        let table = catalog.table.expect("the catalog renders as a table");
        assert!(
            table.rows.iter().any(|row| row[0] == "cpu"),
            "{:?}",
            table.rows
        );

        // A PromQL evaluation — translated and executed in-process against the catalog read there.
        let metrics = snapshot(&backend, Screen::Metrics, "cpu")
            .await
            .expect("promql");
        assert!(!metrics.series.is_empty(), "`cpu` should plot");
        assert!(metrics.series[0].points.iter().any(|(_, v)| *v == 0.5));

        // TraceQL, and the waterfall the list drills into.
        let traces = snapshot(&backend, Screen::Traces, "{}")
            .await
            .expect("traceql");
        let list_from = traces.list_from.expect("the trace list is selectable");
        assert!(
            traces.lines.len() > list_from,
            "one trace should match: {:?}",
            traces.lines
        );
        let trace_id = traces.lines[list_from]
            .split_whitespace()
            .next()
            .expect("the row leads with the trace id")
            .to_owned();
        let (pane, detail) = build_waterfall_detail(&backend, &trace_id, true).await;
        assert!(pane.waterfall.is_some(), "{:?}", pane.lines);
        assert_eq!(detail.expect("materialized trace").spans.len(), 1);

        // Logs, including the volume sparkline under the list.
        let logs = snapshot(&backend, Screen::Logs, "").await.expect("logs");
        assert_eq!(logs.log_records.len(), 1);
        assert_eq!(logs.log_records[0].body, "checkout failed");
        assert_eq!(logs.log_records[0].service.as_deref(), Some("cart"));

        // The completion vocabularies, which are ancillary lookups rather than panel queries.
        let keys = backend.attribute_keys().await.expect("keys");
        assert!(keys.iter().any(|k| k == "service.name"), "{keys:?}");
        let values = backend
            .attribute_values("service.name")
            .await
            .expect("values");
        assert!(values.iter().any(|v| v == "cart"), "{values:?}");
    }

    #[tokio::test]
    async fn a_local_backend_reports_a_bad_query_rather_than_failing_the_session() {
        let backend = local().await;
        let error = snapshot(&backend, Screen::Metrics, "not promql {{{")
            .await
            .expect_err("a malformed query is a panel message, not a panic");
        assert!(!error.is_empty(), "the diagnostic must say something");
    }

    #[test]
    fn a_remote_backend_reports_what_it_is_reading() {
        let backend = Backend::connect("127.0.0.1:4318").expect("url");
        assert_eq!(backend.describe(), "http://127.0.0.1:4318");
        // Only a daemon can answer out of a live writer's unsealed buffer; a read-only opener sees
        // sealed segments only, which is the whole reason `--url` exists.
        assert!(backend.sees_live_buffer());
    }

    #[test]
    fn a_bad_url_is_refused_before_any_session_starts() {
        let e = Backend::connect("https://example.com").expect_err("no TLS");
        assert!(e.contains("Terminate TLS in front of it"), "{e}");
        assert!(Backend::connect("").is_err());
    }
}

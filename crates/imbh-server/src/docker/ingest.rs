//! Container identity → OTLP resource, and the batching ingest worker.
//!
//! Every FIFO reader thread turns wire entries into OTLP `LogRecord`s and hands them, with their
//! container's shared [`Container`] context, to one [`Ingestor`] worker. The worker groups a batch
//! by container, encodes a single `ExportLogsServiceRequest`, and pushes it through
//! `Db::ingest_otlp_logs` — the same entry point the HTTP and gRPC routes use, so the plugin has no
//! private ingest path.
//!
//! Batching is what makes this cheap: one WAL append and one buffer write per batch instead of per
//! line. A batch closes on whichever comes first — [`IngestConfig::batch_max`] records or
//! [`IngestConfig::flush_interval`] since the batch opened.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use imbh::{AnyValue as ImbhValue, Db};
use opentelemetry_proto::tonic::collector::logs::v1::ExportLogsServiceRequest;
use opentelemetry_proto::tonic::common::v1::{
    AnyValue, ArrayValue, InstrumentationScope, KeyValue, any_value,
};
use opentelemetry_proto::tonic::logs::v1::{LogRecord, ResourceLogs, ScopeLogs};
use opentelemetry_proto::tonic::resource::v1::Resource;
use prost::Message;

use super::entry::LogEntry;
use super::json;

/// The OTLP instrumentation scope every record from this driver carries, so `scope` distinguishes
/// container output from an application's own OTLP.
const SCOPE_NAME: &str = "docker";

/// Tunables for the ingest worker.
#[derive(Debug, Clone)]
pub struct IngestConfig {
    /// Records per ingest batch.
    pub batch_max: usize,
    /// How long a partially-filled batch waits for more records before being flushed.
    pub flush_interval: Duration,
    /// Bound on the queue between the FIFO readers and the worker. Reached only when ingest cannot
    /// keep up; see [`Ingestor::send`] for what happens then.
    pub queue_capacity: usize,
}

impl Default for IngestConfig {
    fn default() -> Self {
        IngestConfig {
            batch_max: 512,
            flush_interval: Duration::from_millis(200),
            queue_capacity: 8192,
        }
    }
}

/// A container's stable, per-stream context: the OTLP resource to stamp on its records, plus the
/// severity mapping its log-opts asked for. Built once in `StartLogging` and shared by `Arc` with
/// every record, so the per-line cost is a pointer clone.
pub struct Container {
    pub id: String,
    pub name: String,
    pub service: String,
    /// The attributes that come from the `StartLogging` document alone. Kept so a later network
    /// refresh can rebuild the resource without re-parsing it.
    base: Vec<KeyValue>,
    /// The resource stamped on this container's records.
    ///
    /// Behind a lock because bridge-network discovery can only fill `container.network.*` in *after*
    /// the container started: `dockerd` calls `StartLogging` synchronously while it holds that
    /// container's lock, so asking the daemon which networks it is on from that handler risks
    /// deadlocking the daemon against its own log driver (`networks.rs`). The plugin therefore reads
    /// the last snapshot at start, and swaps a fuller resource in when the next refresh knows more.
    ///
    /// Read once per batch *group* in [`encode`], not per record, so the cost is a single uncontended
    /// read lock per flush.
    resource: std::sync::RwLock<Arc<Resource>>,
    /// The same network attachments in their raw form, for the remap event's `.info.networks`.
    /// Shared by `Arc` so a remapper can tell "unchanged since the last line" by pointer.
    networks: std::sync::RwLock<Arc<Vec<(String, std::net::IpAddr)>>>,
    stdout_severity: (i32, &'static str),
    stderr_severity: (i32, &'static str),
    /// The compiled remap binding, when this container has one. `None` — always, without the
    /// `docker-remap` feature — is the pre-feature behaviour: [`Container::record`] and nothing else.
    #[cfg(feature = "docker-remap")]
    remap: Option<Arc<super::remap::Bound>>,
}

impl Container {
    /// Build the context from the `Info` object of a `StartLogging` request.
    pub fn from_info(info: &ImbhValue) -> Self {
        let id = json::string(info, "ContainerID");
        // Docker reports the name with a leading slash (`/web`); the bare name is what users type.
        let name = json::string(info, "ContainerName")
            .trim_start_matches('/')
            .to_owned();
        let opts = json::string_map(info, "Config");
        let opt = |key: &str| {
            opts.iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.as_str())
                .filter(|v| !v.is_empty())
        };

        // `service.name` is the axis every imbh query starts from, so it gets a real value even when
        // the operator sets nothing: the container name, else the (short) container id.
        let service =
            opt("imbh-service")
                .map(str::to_owned)
                .unwrap_or_else(|| match name.is_empty() {
                    false => name.clone(),
                    true => id.chars().take(12).collect(),
                });

        let mut attrs = vec![
            kv("service.name", &service),
            kv("container.runtime", "docker"),
        ];
        for (key, value) in [
            ("container.id", id.as_str()),
            ("container.name", name.as_str()),
            (
                "container.image.name",
                &json::string(info, "ContainerImageName"),
            ),
            (
                "container.image.id",
                &json::string(info, "ContainerImageID"),
            ),
        ] {
            if !value.is_empty() {
                attrs.push(kv(key, value));
            }
        }

        // `--log-opt labels=a,b` / `--log-opt env=A,B` select which of the container's labels and
        // environment variables become resource attributes. Both are namespaced so a label named
        // `service.name` cannot shadow the real one.
        let labels = json::string_map(info, "ContainerLabels");
        for want in selected(opt("labels")) {
            if let Some((_, v)) = labels.iter().find(|(k, _)| *k == want) {
                attrs.push(kv(&format!("container.label.{want}"), v));
            }
        }
        let env = json::string_list(info, "ContainerEnv");
        for want in selected(opt("env")) {
            if let Some(value) = env.iter().find_map(|e| e.strip_prefix(&format!("{want}="))) {
                attrs.push(kv(&format!("container.env.{want}"), value));
            }
        }

        Container {
            id,
            name,
            service,
            resource: std::sync::RwLock::new(Arc::new(Resource {
                attributes: attrs.clone(),
                ..Default::default()
            })),
            base: attrs,
            networks: std::sync::RwLock::new(Arc::new(Vec::new())),
            stdout_severity: severity(opt("imbh-stdout-severity")).unwrap_or((9, "INFO")),
            stderr_severity: severity(opt("imbh-stderr-severity")).unwrap_or((17, "ERROR")),
            #[cfg(feature = "docker-remap")]
            remap: None,
        }
    }

    /// Compile and attach the remap script this container asked for.
    ///
    /// Separate from [`Container::from_info`] on purpose. `from_info` cannot fail — an unparseable
    /// log-opt falls back to a default rather than refusing to start the container — but a script
    /// that does not compile *must* fail `StartLogging` loudly, because `docker run` is the only
    /// place the operator will see the diagnostic.
    ///
    /// Precedence: the `imbh-remap` log-opt, then the daemon-wide default, then the built-in script.
    #[cfg(feature = "docker-remap")]
    pub fn bind_remap(
        &mut self,
        info: &ImbhValue,
        default: &super::remap::Source,
        cache: &super::remap::Cache,
    ) -> Result<(), String> {
        let opts = json::string_map(info, "Config");
        let source = opts
            .iter()
            .find(|(k, _)| k == "imbh-remap")
            .map(|(_, v)| super::remap::Source::parse(v))
            .unwrap_or_else(|| default.clone());

        let Some(source) = source.read()? else {
            return Ok(()); // `off`
        };
        let script = cache.get(&source)?;
        self.remap = Some(Arc::new(super::remap::Bound::new(script, self, info)));
        Ok(())
    }

    /// A fresh per-thread remapper, or `None` when this container has no script. One is built per
    /// FIFO reader thread, because a VRL `Runtime` needs `&mut` while `Container` is shared.
    #[cfg(feature = "docker-remap")]
    pub fn remapper(&self) -> Option<super::remap::Remapper> {
        self.remap.clone().map(super::remap::Remapper::new)
    }

    /// The severity pair this container assigns to a stream. Pulled out of [`Container::record`] so
    /// the remapper can seed its event with exactly what `record` would have produced.
    pub(crate) fn severity_for(&self, stderr: bool) -> (i32, &'static str) {
        match stderr {
            true => self.stderr_severity,
            false => self.stdout_severity,
        }
    }

    /// This container's current resource.
    pub(crate) fn resource(&self) -> Arc<Resource> {
        self.resource
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Record which bridge networks this container is attached to, as resource attributes.
    ///
    /// Called at `StartLogging` from whatever the last discovery snapshot knew, and again whenever a
    /// refresh changes it — a container that started between two scans has no network attributes on
    /// its first lines and gains them on the rest, which is the honest outcome of never calling the
    /// daemon from the `StartLogging` handler.
    ///
    /// A no-op when nothing changed, so the common refresh (an idle daemon, every 30 seconds) does
    /// not churn allocations or break `encode`'s pointer-equality grouping.
    pub fn set_networks(&self, networks: &[(String, std::net::IpAddr)]) {
        {
            let mut current = self
                .networks
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if current.as_slice() != networks {
                *current = Arc::new(networks.to_vec());
            }
        }
        let mut attrs = self.base.clone();
        if !networks.is_empty() {
            attrs.push(KeyValue {
                key: "container.network.names".to_owned(),
                value: Some(AnyValue {
                    value: Some(any_value::Value::ArrayValue(ArrayValue {
                        values: networks
                            .iter()
                            .map(|(name, _)| AnyValue {
                                value: Some(any_value::Value::StringValue(name.clone())),
                            })
                            .collect(),
                    })),
                }),
                ..Default::default()
            });
            // Namespaced per network, the way `container.label.<k>` already is: a container on two
            // networks has two addresses, and flattening them to one would pick arbitrarily.
            for (name, ip) in networks {
                attrs.push(kv(&format!("container.network.{name}.ip"), &ip.to_string()));
            }
        }

        let mut current = self
            .resource
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if current.attributes == attrs {
            return;
        }
        *current = Arc::new(Resource {
            attributes: attrs,
            ..Default::default()
        });
    }

    /// This container's network attachments, for the remap event. The `Arc` is stable while they do
    /// not change, which is what lets a remapper skip rebuilding `.info.networks` per line.
    #[cfg(feature = "docker-remap")]
    pub(crate) fn networks(&self) -> Arc<Vec<(String, std::net::IpAddr)>> {
        self.networks
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// How to name this container in a log message: its name, falling back to its id.
    pub fn name_or_id(&self) -> &str {
        match self.name.is_empty() {
            false => &self.name,
            true => &self.id,
        }
    }

    /// Map one reassembled wire entry onto an OTLP `LogRecord`.
    ///
    /// The line's trailing newline is stripped: Docker hands the framing byte to the driver, but a
    /// body ending in `\n` makes every `SELECT body` and every full-text term awkward. `ReadLogs`
    /// puts it back, so `docker logs` output is unchanged (`docs/DOCKER_LOG_DRIVER.md`).
    pub fn record(&self, entry: &LogEntry) -> LogRecord {
        let stderr = entry.source == "stderr";
        let (severity_number, severity_text) = self.severity_for(stderr);
        let time = entry.time_nano.max(0) as u64;
        LogRecord {
            time_unix_nano: time,
            observed_time_unix_nano: time,
            severity_number,
            severity_text: severity_text.to_owned(),
            body: Some(any_str(&String::from_utf8_lossy(strip_newline(
                &entry.line,
            )))),
            // `log.iostream` is the OTel semantic convention for which stream a line came from, and
            // it is what `ReadLogs` reads back to restore `source` exactly.
            attributes: vec![kv("log.iostream", if stderr { "stderr" } else { "stdout" })],
            ..Default::default()
        }
    }
}

/// Strip one trailing line terminator (`\n` or `\r\n`).
pub(crate) fn strip_newline(line: &[u8]) -> &[u8] {
    match line.strip_suffix(b"\n") {
        Some(rest) => rest.strip_suffix(b"\r").unwrap_or(rest),
        None => line,
    }
}

/// Split a comma-separated log-opt value into trimmed, non-empty keys.
fn selected(value: Option<&str>) -> Vec<&str> {
    value
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect()
}

/// Parse a severity log-opt: an OTel severity name (`INFO`, `WARN`, `ERROR`, …) or a raw
/// 1–24 severity number. Unrecognized values fall back to the default rather than failing the
/// container's start.
fn severity(value: Option<&str>) -> Option<(i32, &'static str)> {
    let raw = value?.trim();
    if let Ok(n) = raw.parse::<i32>() {
        return (1..=24).contains(&n).then(|| (n, name_for(n)));
    }
    let n = match raw.to_ascii_uppercase().as_str() {
        "TRACE" => 1,
        "DEBUG" => 5,
        "INFO" => 9,
        "WARN" | "WARNING" => 13,
        "ERROR" => 17,
        "FATAL" => 21,
        _ => return None,
    };
    Some((n, name_for(n)))
}

/// The OTel severity band a number falls in (§6.2 stores both the number and this text).
fn name_for(n: i32) -> &'static str {
    match n {
        1..=4 => "TRACE",
        5..=8 => "DEBUG",
        9..=12 => "INFO",
        13..=16 => "WARN",
        17..=20 => "ERROR",
        _ => "FATAL",
    }
}

pub(crate) fn kv(key: &str, value: &str) -> KeyValue {
    KeyValue {
        key: key.to_owned(),
        value: Some(any_str(value)),
        ..Default::default()
    }
}

fn any_str(s: &str) -> AnyValue {
    AnyValue {
        value: Some(any_value::Value::StringValue(s.to_owned())),
    }
}

/// One queued record and the container it belongs to.
struct Item {
    container: Arc<Container>,
    /// The resource a remap script produced for this line, when it differs from the container's own.
    /// `None` — the only possibility without the `docker-remap` feature — means "use the
    /// container's resource", which is the common case even with remapping on.
    resource: Option<Arc<Resource>>,
    record: LogRecord,
}

/// Two overrides belong to one group when they are the same allocation — the usual case, because a
/// container's `Remapper` interns the resource it produces — or, failing that, the same value, so a
/// script that rebuilds an identical object every line still yields one `ResourceLogs`.
fn same_resource(a: &Option<Arc<Resource>>, b: &Option<Arc<Resource>>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => Arc::ptr_eq(a, b) || a == b,
        _ => false,
    }
}

/// Groups beyond which [`encode`] stops looking for an existing one and just opens another.
///
/// Without a cap, a pathological script emitting a distinct resource per line would make grouping
/// quadratic in the batch size. Giving up costs a few extra `ResourceLogs` — which the wire format
/// and the ingest path both handle fine — and is strictly better than a stalled reader thread.
const MAX_GROUPS: usize = 64;

/// One `ResourceLogs` under construction: the container, the resource override that distinguishes it
/// from that container's other groups (`None` = the container's own), and the records so far.
type Group = (Arc<Container>, Option<Arc<Resource>>, Vec<LogRecord>);

/// What travels the queue between the FIFO readers and the worker. (Not `Message`: that name belongs
/// to the prost trait this module encodes with.)
enum Queued {
    /// One log record with its container context.
    Record(Item),
    /// Wind down: ingest the batch in hand and exit. Sent by [`Ingestor::shutdown`], so — the channel
    /// being FIFO — everything queued before it is ingested before the worker leaves.
    Stop,
}

/// Handle on the batching ingest worker. Dropping it closes the queue, which drains the worker and
/// ends its thread; [`Ingestor::shutdown`] does the same *synchronously*, which is what a shutting
/// down plugin needs before the `Db` is closed under it.
pub struct Ingestor {
    tx: SyncSender<Queued>,
    /// Set by [`Ingestor::shutdown`] so no record queues up behind the sentinel and gets dropped.
    closing: AtomicBool,
    worker: Mutex<Option<JoinHandle<()>>>,
}

impl Ingestor {
    /// Start the worker thread.
    pub fn start(db: Arc<Db>, config: IngestConfig) -> Ingestor {
        let (tx, rx) = std::sync::mpsc::sync_channel(config.queue_capacity);
        let worker = std::thread::Builder::new()
            .name("imbh-docker-ingest".to_owned())
            .spawn(move || run(db, rx, config))
            .expect("spawn docker ingest worker");
        Ingestor {
            tx,
            closing: AtomicBool::new(false),
            worker: Mutex::new(Some(worker)),
        }
    }

    /// Queue one record. Returns `false` once the worker is gone or shutting down — the caller's cue
    /// to stop reading its FIFO.
    ///
    /// A full queue **blocks** the calling FIFO reader rather than dropping the line: back-pressure
    /// propagates into the container's stdout pipe, which is what an operator wants from a log
    /// driver — slow logging, not silently missing logs.
    pub fn send(
        &self,
        container: Arc<Container>,
        resource: Option<Arc<Resource>>,
        record: LogRecord,
    ) -> bool {
        if self.closing.load(Ordering::Relaxed) {
            return false;
        }
        match self.tx.try_send(Queued::Record(Item {
            container,
            resource,
            record,
        })) {
            Ok(()) => true,
            Err(TrySendError::Full(item)) => self.tx.send(item).is_ok(),
            Err(TrySendError::Disconnected(_)) => false,
        }
    }

    /// Drain the queue into the DB and join the worker. Idempotent.
    ///
    /// Called while the plugin winds down, *before* the caller closes the `Db`: every line a
    /// container already wrote is ingested, and anything a still-parked FIFO reader produces after
    /// this is refused by [`Ingestor::send`] rather than ingested into a closing DB.
    pub fn shutdown(&self) {
        // Refuse new records first: a reader that queues behind the sentinel would be dropped.
        self.closing.store(true, Ordering::SeqCst);
        // Blocks only if the queue is full, i.e. until the worker has made room by ingesting.
        let _ = self.tx.send(Queued::Stop);
        let worker = self
            .worker
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(worker) = worker {
            let _ = worker.join();
        }
    }
}

/// The worker loop: block for a record, keep filling until the batch is full or the flush interval
/// expires, ingest, repeat. Exits when every sender is dropped, or on the [`Queued::Stop`] sentinel.
fn run(db: Arc<Db>, rx: Receiver<Queued>, config: IngestConfig) {
    let db = db.blocking();
    loop {
        let Ok(Queued::Record(first)) = rx.recv() else {
            return; // disconnected, or asked to stop with nothing in hand
        };
        let mut batch = vec![first];
        let mut stopping = false;
        let deadline = Instant::now() + config.flush_interval;
        while batch.len() < config.batch_max {
            let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                break;
            };
            match rx.recv_timeout(left) {
                Ok(Queued::Record(item)) => batch.push(item),
                // Stop: ingest what this batch holds, then leave — everything queued ahead of the
                // sentinel is in `batch` already.
                Ok(Queued::Stop) => {
                    stopping = true;
                    break;
                }
                // Disconnected: flush what we have, then the next `recv` ends the loop.
                Err(_) => break,
            }
        }
        if let Err(e) = db.ingest_otlp_logs(&encode(batch)) {
            super::warn(&format!("ingest failed: {e}"));
        }
        if stopping {
            return;
        }
    }
}

/// Encode a batch as one OTLP request, one `ResourceLogs` per (container, resource). Records keep
/// their arrival order within a group, which is the order Docker wrote them to the FIFO.
///
/// The container pointer alone was a sufficient key until a remap script could rewrite `.resource`
/// per line; now both halves matter. With no override — always, without `docker-remap` — the extra
/// test is one discriminant match on top of the pointer compare that was already here.
fn encode(batch: Vec<Item>) -> Vec<u8> {
    let mut groups: Vec<Group> = Vec::new();
    for item in batch {
        let existing = match groups.len() < MAX_GROUPS {
            true => groups.iter_mut().find(|(container, resource, _)| {
                Arc::ptr_eq(container, &item.container) && same_resource(resource, &item.resource)
            }),
            false => None,
        };
        match existing {
            Some((_, _, records)) => records.push(item.record),
            None => groups.push((item.container, item.resource, vec![item.record])),
        }
    }
    ExportLogsServiceRequest {
        resource_logs: groups
            .into_iter()
            .map(|(container, resource, log_records)| ResourceLogs {
                resource: Some(match resource {
                    Some(overridden) => (*overridden).clone(),
                    None => (*container.resource()).clone(),
                }),
                scope_logs: vec![ScopeLogs {
                    scope: Some(InstrumentationScope {
                        name: SCOPE_NAME.to_owned(),
                        ..Default::default()
                    }),
                    log_records,
                    ..Default::default()
                }],
                ..Default::default()
            })
            .collect(),
    }
    .encode_to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(json_text: &str) -> ImbhValue {
        json::parse(json_text.as_bytes())
    }

    fn attr(c: &Container, key: &str) -> Option<String> {
        c.resource()
            .attributes
            .iter()
            .find(|kv| kv.key == key)
            .and_then(|kv| match &kv.value {
                Some(AnyValue {
                    value: Some(any_value::Value::StringValue(s)),
                }) => Some(s.clone()),
                _ => None,
            })
    }

    fn entry(source: &str, line: &str) -> LogEntry {
        LogEntry {
            source: source.to_owned(),
            time_nano: 42,
            line: line.as_bytes().to_vec(),
            partial: false,
            partial_log_metadata: None,
        }
    }

    #[test]
    fn container_identity_becomes_otel_resource_attributes() {
        let c = Container::from_info(&info(
            r#"{"ContainerID":"abc123def456789","ContainerName":"/web",
                "ContainerImageName":"nginx:1.27","ContainerImageID":"sha256:aaa"}"#,
        ));
        assert_eq!(c.name, "web");
        assert_eq!(c.service, "web");
        assert_eq!(attr(&c, "service.name").as_deref(), Some("web"));
        assert_eq!(attr(&c, "container.id").as_deref(), Some("abc123def456789"));
        assert_eq!(attr(&c, "container.name").as_deref(), Some("web"));
        assert_eq!(
            attr(&c, "container.image.name").as_deref(),
            Some("nginx:1.27")
        );
        assert_eq!(
            attr(&c, "container.image.id").as_deref(),
            Some("sha256:aaa")
        );
        assert_eq!(attr(&c, "container.runtime").as_deref(), Some("docker"));
    }

    #[test]
    fn service_falls_back_to_the_short_id_for_an_unnamed_container() {
        let c = Container::from_info(&info(r#"{"ContainerID":"abcdef0123456789abcdef"}"#));
        assert_eq!(c.service, "abcdef012345");
    }

    #[test]
    fn log_opts_pick_the_service_labels_and_env() {
        let c = Container::from_info(&info(
            r#"{"ContainerID":"abc","ContainerName":"/web",
                "ContainerLabels":{"app":"cart","secret":"nope"},
                "ContainerEnv":["REGION=eu-1","TOKEN=nope","EMPTY="],
                "Config":{"imbh-service":"checkout","labels":"app, missing","env":"REGION,EMPTY"}}"#,
        ));
        assert_eq!(c.service, "checkout");
        assert_eq!(attr(&c, "service.name").as_deref(), Some("checkout"));
        assert_eq!(attr(&c, "container.label.app").as_deref(), Some("cart"));
        assert_eq!(attr(&c, "container.env.REGION").as_deref(), Some("eu-1"));
        assert_eq!(attr(&c, "container.env.EMPTY").as_deref(), Some(""));
        // Unselected label/env values must not leak into the resource.
        assert_eq!(attr(&c, "container.label.secret").as_deref(), None);
        assert_eq!(attr(&c, "container.env.TOKEN").as_deref(), None);
        assert_eq!(attr(&c, "container.label.missing").as_deref(), None);
    }

    #[test]
    fn streams_map_to_severities_and_the_iostream_attribute() {
        let c = Container::from_info(&info(r#"{"ContainerID":"abc"}"#));

        let out = c.record(&entry("stdout", "hello\n"));
        assert_eq!(out.severity_number, 9);
        assert_eq!(out.severity_text, "INFO");
        assert_eq!(out.attributes[0].key, "log.iostream");

        let err = c.record(&entry("stderr", "boom\n"));
        assert_eq!(err.severity_number, 17);
        assert_eq!(err.severity_text, "ERROR");
    }

    #[test]
    fn severity_log_opts_accept_names_and_numbers() {
        let c = Container::from_info(&info(
            r#"{"ContainerID":"abc",
                "Config":{"imbh-stdout-severity":"debug","imbh-stderr-severity":"13"}}"#,
        ));
        assert_eq!(c.record(&entry("stdout", "x\n")).severity_number, 5);
        assert_eq!(c.record(&entry("stderr", "x\n")).severity_text, "WARN");

        // Nonsense falls back to the defaults instead of failing the container's start.
        let bad = Container::from_info(&info(
            r#"{"ContainerID":"abc","Config":{"imbh-stdout-severity":"loud","imbh-stderr-severity":"99"}}"#,
        ));
        assert_eq!(bad.record(&entry("stdout", "x\n")).severity_number, 9);
        assert_eq!(bad.record(&entry("stderr", "x\n")).severity_number, 17);
    }

    #[test]
    fn bodies_lose_exactly_one_trailing_newline() {
        let c = Container::from_info(&info(r#"{"ContainerID":"abc"}"#));
        let body = |line: &str| match c.record(&entry("stdout", line)).body {
            Some(AnyValue {
                value: Some(any_value::Value::StringValue(s)),
            }) => s,
            other => panic!("expected a string body, got {other:?}"),
        };
        assert_eq!(body("hello\n"), "hello");
        assert_eq!(body("hello\r\n"), "hello");
        assert_eq!(body("hello"), "hello");
        assert_eq!(body("hello\n\n"), "hello\n");
        assert_eq!(body(""), "");
    }

    #[test]
    fn a_batch_encodes_to_one_resource_logs_per_container() {
        let a = Arc::new(Container::from_info(&info(
            r#"{"ContainerID":"a","ContainerName":"/one"}"#,
        )));
        let b = Arc::new(Container::from_info(&info(
            r#"{"ContainerID":"b","ContainerName":"/two"}"#,
        )));
        let batch = vec![item(&a, "a1"), item(&b, "b1"), item(&a, "a2")];

        let decoded = ExportLogsServiceRequest::decode(&encode(batch)[..]).expect("decode");
        assert_eq!(decoded.resource_logs.len(), 2);
        assert_eq!(decoded.resource_logs[0].scope_logs[0].log_records.len(), 2);
        assert_eq!(decoded.resource_logs[1].scope_logs[0].log_records.len(), 1);
        assert_eq!(
            decoded.resource_logs[0].scope_logs[0]
                .scope
                .as_ref()
                .unwrap()
                .name,
            SCOPE_NAME
        );
    }

    /// One un-remapped line of `container`.
    fn item(container: &Arc<Container>, line: &str) -> Item {
        Item {
            container: container.clone(),
            resource: None,
            record: container.record(&entry("stdout", &format!("{line}\n"))),
        }
    }

    fn resource_with(marker: &str) -> Arc<Resource> {
        Arc::new(Resource {
            attributes: vec![kv("deployment.environment", marker)],
            ..Default::default()
        })
    }

    #[test]
    fn one_container_splits_into_a_group_per_distinct_resource_override() {
        let c = Arc::new(Container::from_info(&info(r#"{"ContainerID":"a"}"#)));
        let prod = resource_with("prod");
        let staging = resource_with("staging");

        let with = |resource: Option<Arc<Resource>>, line: &str| Item {
            container: c.clone(),
            resource,
            record: c.record(&entry("stdout", line)),
        };
        let batch = vec![
            with(None, "plain-1\n"),
            with(Some(prod.clone()), "prod-1\n"),
            // The same allocation the interner would hand back: must join the group above.
            with(Some(prod.clone()), "prod-2\n"),
            // Equal by value but a different allocation: must ALSO join it, so a script that
            // rebuilds an identical object every line does not fragment the batch.
            with(Some(resource_with("prod")), "prod-3\n"),
            with(Some(staging), "staging-1\n"),
            with(None, "plain-2\n"),
        ];

        let decoded = ExportLogsServiceRequest::decode(&encode(batch)[..]).expect("decode");
        let sizes: Vec<usize> = decoded
            .resource_logs
            .iter()
            .map(|rl| rl.scope_logs[0].log_records.len())
            .collect();
        // container-default (2), prod (3), staging (1) — and `None` never merges with `Some`.
        assert_eq!(sizes, vec![2, 3, 1]);
        let env = |rl: &ResourceLogs| {
            rl.resource
                .as_ref()
                .unwrap()
                .attributes
                .iter()
                .find(|kv| kv.key == "deployment.environment")
                .and_then(|kv| match &kv.value {
                    Some(AnyValue {
                        value: Some(any_value::Value::StringValue(s)),
                    }) => Some(s.clone()),
                    _ => None,
                })
        };
        assert_eq!(env(&decoded.resource_logs[0]), None);
        assert_eq!(env(&decoded.resource_logs[1]).as_deref(), Some("prod"));
        assert_eq!(env(&decoded.resource_logs[2]).as_deref(), Some("staging"));
    }

    #[test]
    fn a_pathological_script_cannot_make_grouping_quadratic() {
        // 200 distinct resources in one batch: grouping stops searching at MAX_GROUPS and opens a
        // new group per record after that, rather than scanning an ever-growing list.
        let c = Arc::new(Container::from_info(&info(r#"{"ContainerID":"a"}"#)));
        let batch: Vec<Item> = (0..200)
            .map(|i| Item {
                container: c.clone(),
                resource: Some(resource_with(&format!("env-{i}"))),
                record: c.record(&entry("stdout", "x\n")),
            })
            .collect();

        let decoded = ExportLogsServiceRequest::decode(&encode(batch)[..]).expect("decode");
        assert_eq!(decoded.resource_logs.len(), 200);
        // Nothing is lost when the cap is hit — every record still reaches the request.
        let total: usize = decoded
            .resource_logs
            .iter()
            .map(|rl| rl.scope_logs[0].log_records.len())
            .sum();
        assert_eq!(total, 200);
    }
}

#[cfg(test)]
mod network_tests {
    use super::*;
    use std::net::IpAddr;

    fn container() -> Container {
        Container::from_info(&json::parse(
            br#"{"ContainerID":"abc123","ContainerName":"/web"}"#,
        ))
    }

    fn ip(s: &str) -> IpAddr {
        s.parse().expect("an IP address")
    }

    fn attrs(c: &Container) -> Vec<(String, String)> {
        c.resource()
            .attributes
            .iter()
            .map(|kv| (kv.key.clone(), render(kv)))
            .collect()
    }

    fn render(kv: &KeyValue) -> String {
        match kv.value.as_ref().and_then(|v| v.value.as_ref()) {
            Some(any_value::Value::StringValue(s)) => s.clone(),
            Some(any_value::Value::ArrayValue(a)) => a
                .values
                .iter()
                .filter_map(|v| match &v.value {
                    Some(any_value::Value::StringValue(s)) => Some(s.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join(","),
            _ => String::new(),
        }
    }

    fn get(c: &Container, key: &str) -> Option<String> {
        attrs(c).into_iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    /// A container on no known network must look exactly as it did before discovery existed — no
    /// empty `container.network.names`, no placeholder.
    #[test]
    fn a_container_with_no_known_networks_gains_no_attributes() {
        let c = container();
        let before = attrs(&c);
        c.set_networks(&[]);
        assert_eq!(attrs(&c), before);
        assert!(
            !before
                .iter()
                .any(|(k, _)| k.starts_with("container.network"))
        );
    }

    #[test]
    fn attachments_become_resource_attributes() {
        let c = container();
        c.set_networks(&[
            ("bridge".to_owned(), ip("172.17.0.2")),
            ("myproj_default".to_owned(), ip("172.23.0.7")),
        ]);
        assert_eq!(
            get(&c, "container.network.names").as_deref(),
            Some("bridge,myproj_default")
        );
        // Namespaced per network: a container on two networks has two addresses, and flattening
        // them to one `container.ip` would have to pick arbitrarily.
        assert_eq!(
            get(&c, "container.network.bridge.ip").as_deref(),
            Some("172.17.0.2")
        );
        assert_eq!(
            get(&c, "container.network.myproj_default.ip").as_deref(),
            Some("172.23.0.7")
        );
        // The identity attributes every query depends on are untouched.
        assert_eq!(get(&c, "container.id").as_deref(), Some("abc123"));
        assert_eq!(get(&c, "service.name").as_deref(), Some("web"));
    }

    /// THE property the late fill rests on: rewriting the same networks must not produce a new
    /// resource, or `encode`'s pointer-equality grouping would split every batch that spans a
    /// refresh, and an idle daemon would churn an allocation per container every 30 seconds.
    #[test]
    fn republishing_the_same_networks_keeps_the_same_allocation() {
        let c = container();
        let networks = [("bridge".to_owned(), ip("172.17.0.2"))];
        c.set_networks(&networks);
        let first = c.resource();
        c.set_networks(&networks);
        assert!(Arc::ptr_eq(&first, &c.resource()));

        // A real change does swap it.
        c.set_networks(&[("bridge".to_owned(), ip("172.17.0.9"))]);
        assert!(!Arc::ptr_eq(&first, &c.resource()));
    }

    /// A container that leaves every network loses the attributes rather than keeping a stale
    /// address — the base attributes are the floor, not a starting point that only grows.
    #[test]
    fn detaching_removes_the_attributes_again() {
        let c = container();
        c.set_networks(&[("bridge".to_owned(), ip("172.17.0.2"))]);
        assert!(get(&c, "container.network.names").is_some());
        c.set_networks(&[]);
        assert!(get(&c, "container.network.names").is_none());
        assert_eq!(get(&c, "container.id").as_deref(), Some("abc123"));
    }

    /// Records encoded after a refresh must carry the new resource; the grouping still has to
    /// collapse a run of lines from one container into one `ResourceLogs`.
    #[test]
    fn encoded_records_carry_the_current_resource() {
        let c = Arc::new(container());
        c.set_networks(&[("bridge".to_owned(), ip("172.17.0.2"))]);
        let batch: Vec<Item> = (0..3)
            .map(|_| Item {
                container: Arc::clone(&c),
                resource: None,
                record: LogRecord::default(),
            })
            .collect();
        let encoded = ExportLogsServiceRequest::decode(encode(batch).as_slice()).expect("decode");
        assert_eq!(encoded.resource_logs.len(), 1, "one container, one group");
        let attributes = &encoded.resource_logs[0]
            .resource
            .as_ref()
            .expect("a resource")
            .attributes;
        assert!(
            attributes
                .iter()
                .any(|kv| kv.key == "container.network.bridge.ip"),
            "the encoded resource must be the one the last refresh produced"
        );
    }
}

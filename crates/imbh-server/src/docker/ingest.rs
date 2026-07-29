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

use std::sync::Arc;
use std::sync::mpsc::{Receiver, SyncSender, TrySendError};
use std::time::{Duration, Instant};

use imbh::{AnyValue as ImbhValue, Db};
use opentelemetry_proto::tonic::collector::logs::v1::ExportLogsServiceRequest;
use opentelemetry_proto::tonic::common::v1::{AnyValue, InstrumentationScope, KeyValue, any_value};
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
    resource: Resource,
    stdout_severity: (i32, &'static str),
    stderr_severity: (i32, &'static str),
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
            resource: Resource {
                attributes: attrs,
                ..Default::default()
            },
            stdout_severity: severity(opt("imbh-stdout-severity")).unwrap_or((9, "INFO")),
            stderr_severity: severity(opt("imbh-stderr-severity")).unwrap_or((17, "ERROR")),
        }
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
        let (severity_number, severity_text) = match stderr {
            true => self.stderr_severity,
            false => self.stdout_severity,
        };
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
fn strip_newline(line: &[u8]) -> &[u8] {
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

fn kv(key: &str, value: &str) -> KeyValue {
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
    record: LogRecord,
}

/// Handle on the batching ingest worker. Dropping it closes the queue, which drains the worker and
/// ends its thread.
pub struct Ingestor {
    tx: SyncSender<Item>,
}

impl Ingestor {
    /// Start the worker thread.
    pub fn start(db: Arc<Db>, config: IngestConfig) -> Ingestor {
        let (tx, rx) = std::sync::mpsc::sync_channel(config.queue_capacity);
        std::thread::Builder::new()
            .name("imbh-docker-ingest".to_owned())
            .spawn(move || run(db, rx, config))
            .expect("spawn docker ingest worker");
        Ingestor { tx }
    }

    /// Queue one record. Returns `false` once the worker is gone.
    ///
    /// A full queue **blocks** the calling FIFO reader rather than dropping the line: back-pressure
    /// propagates into the container's stdout pipe, which is what an operator wants from a log
    /// driver — slow logging, not silently missing logs.
    pub fn send(&self, container: Arc<Container>, record: LogRecord) -> bool {
        match self.tx.try_send(Item { container, record }) {
            Ok(()) => true,
            Err(TrySendError::Full(item)) => self.tx.send(item).is_ok(),
            Err(TrySendError::Disconnected(_)) => false,
        }
    }
}

/// The worker loop: block for a record, keep filling until the batch is full or the flush interval
/// expires, ingest, repeat. Exits when every sender is dropped.
fn run(db: Arc<Db>, rx: Receiver<Item>, config: IngestConfig) {
    let db = db.blocking();
    loop {
        let Ok(first) = rx.recv() else { return };
        let mut batch = vec![first];
        let deadline = Instant::now() + config.flush_interval;
        while batch.len() < config.batch_max {
            let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                break;
            };
            match rx.recv_timeout(left) {
                Ok(item) => batch.push(item),
                // Disconnected: flush what we have, then the next `recv` ends the loop.
                Err(_) => break,
            }
        }
        if let Err(e) = db.ingest_otlp_logs(&encode(batch)) {
            super::warn(&format!("ingest failed: {e}"));
        }
    }
}

/// Encode a batch as one OTLP request, one `ResourceLogs` per container. Records keep their arrival
/// order within a container, which is the order Docker wrote them to the FIFO.
fn encode(batch: Vec<Item>) -> Vec<u8> {
    let mut groups: Vec<(Arc<Container>, Vec<LogRecord>)> = Vec::new();
    for item in batch {
        match groups
            .iter_mut()
            .find(|(c, _)| Arc::ptr_eq(c, &item.container))
        {
            Some((_, records)) => records.push(item.record),
            None => groups.push((item.container, vec![item.record])),
        }
    }
    ExportLogsServiceRequest {
        resource_logs: groups
            .into_iter()
            .map(|(container, log_records)| ResourceLogs {
                resource: Some(container.resource.clone()),
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

    fn attr<'a>(c: &'a Container, key: &str) -> Option<&'a str> {
        c.resource
            .attributes
            .iter()
            .find(|kv| kv.key == key)
            .and_then(|kv| match &kv.value {
                Some(AnyValue {
                    value: Some(any_value::Value::StringValue(s)),
                }) => Some(s.as_str()),
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
        assert_eq!(attr(&c, "service.name"), Some("web"));
        assert_eq!(attr(&c, "container.id"), Some("abc123def456789"));
        assert_eq!(attr(&c, "container.name"), Some("web"));
        assert_eq!(attr(&c, "container.image.name"), Some("nginx:1.27"));
        assert_eq!(attr(&c, "container.image.id"), Some("sha256:aaa"));
        assert_eq!(attr(&c, "container.runtime"), Some("docker"));
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
        assert_eq!(attr(&c, "service.name"), Some("checkout"));
        assert_eq!(attr(&c, "container.label.app"), Some("cart"));
        assert_eq!(attr(&c, "container.env.REGION"), Some("eu-1"));
        assert_eq!(attr(&c, "container.env.EMPTY"), Some(""));
        // Unselected label/env values must not leak into the resource.
        assert_eq!(attr(&c, "container.label.secret"), None);
        assert_eq!(attr(&c, "container.env.TOKEN"), None);
        assert_eq!(attr(&c, "container.label.missing"), None);
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
        let batch = vec![
            Item {
                container: a.clone(),
                record: a.record(&entry("stdout", "a1\n")),
            },
            Item {
                container: b.clone(),
                record: b.record(&entry("stdout", "b1\n")),
            },
            Item {
                container: a.clone(),
                record: a.record(&entry("stdout", "a2\n")),
            },
        ];

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
}

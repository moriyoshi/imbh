//! VRL remapping of container log lines (the optional `docker-remap` feature).
//!
//! One container's lines run through one compiled VRL program. The program is immutable and shared
//! by `Arc` ([`Script`]); the *runtime* is not — `Runtime::resolve` takes `&mut self` while
//! `Container::record` takes `&self` behind an `Arc` — so every mutable piece lives in a
//! [`Remapper`] owned by that container's FIFO reader thread. Nothing here is locked, and nothing
//! here is on the query path.
//!
//! The event handed to a script carries **both** models at once: Docker's log-driver fields
//! (`.line`, `.source`, `.time_nano`, `.partial`, `.info.*`) and the OTel log record the driver
//! would have stored if no script existed (`.timestamp`, `.severity_number`, `.body`,
//! `.attributes`, `.resource`, …). That seeding is what makes the identity script `.` reproduce
//! today's behaviour exactly, and it is why the built-in script only ever has to *override* — it
//! never re-derives `service.name`, `container.*` or `log.iostream`.
//!
//! The release profile sets `panic = "abort"`, so there is no `catch_unwind` isolation available: a
//! panic inside VRL takes the process with it. The error model below is therefore built on VRL's
//! own [`Terminate`] rather than on unwinding.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use opentelemetry_proto::tonic::common::v1::{
    AnyValue, ArrayValue, KeyValue, KeyValueList, any_value,
};
use opentelemetry_proto::tonic::logs::v1::LogRecord;
use opentelemetry_proto::tonic::resource::v1::Resource;
use vrl::compiler::runtime::{Runtime, Terminate};
use vrl::compiler::{ExpressionError, Program, TargetValue, TimeZone, compile};
use vrl::diagnostic::Formatter;
use vrl::path::{OwnedSegment, PathPrefix};
use vrl::value::{ObjectMap, Secrets, Value};

use imbh::AnyValue as ImbhValue;

use super::entry::LogEntry;
use super::ingest::Container;
use super::json;

/// The script every container gets unless a log-opt or the daemon-wide default says otherwise.
///
/// A real file rather than an inline string literal, so an operator can read it, diff against it,
/// and paste an edited copy into `--log-opt imbh-remap=…`.
pub const DEFAULT_SCRIPT: &str = include_str!("default.vrl");

/// How an operator asked for a script. One grammar, shared by the `imbh-remap` log-opt and the
/// `IMBH_DOCKER_REMAP` environment variable, so the two are documented once.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Source {
    /// No remapping: `Container::record` alone, exactly as before this feature existed.
    Off,
    /// The built-in [`DEFAULT_SCRIPT`]. The default: a driver built with this feature parses out of
    /// the box, and an operator who wants the old behaviour asks for `off` explicitly.
    #[default]
    Builtin,
    /// A script read from this path — **inside the plugin's mount namespace**, not the host's. A
    /// managed plugin sees its own rootfs, so the database directory (`/var/lib/imbh`, which the
    /// daemon provisions and which persists) is the path that works without reconfiguring the
    /// plugin.
    File(String),
    /// A script given inline.
    Inline(String),
}

impl Source {
    /// Parse the shared grammar. Anything unrecognized is a script, not an error: VRL source is
    /// arbitrary text, so there is nothing to validate until it reaches the compiler.
    pub fn parse(value: &str) -> Source {
        match value.trim() {
            "" | "default" => Source::Builtin,
            "off" | "none" => Source::Off,
            rest => match rest.strip_prefix('@') {
                Some(path) => Source::File(path.trim().to_owned()),
                None => Source::Inline(rest.to_owned()),
            },
        }
    }

    /// The script text, or `None` for [`Source::Off`]. Reading a [`Source::File`] happens here, so
    /// an unreadable path fails the same way a syntax error does.
    pub(crate) fn read(&self) -> Result<Option<String>, String> {
        match self {
            Source::Off => Ok(None),
            Source::Builtin => Ok(Some(DEFAULT_SCRIPT.to_owned())),
            Source::Inline(source) => Ok(Some(source.clone())),
            Source::File(path) => std::fs::read_to_string(path)
                .map(Some)
                .map_err(|e| format!("cannot read remap script {path}: {e}")),
        }
    }
}

/// A compiled remap script, shared by every container that resolved to the same source text.
pub struct Script {
    program: Program,
    /// Whether the program ever reads `.info`. A script that never looks at container metadata
    /// should not pay to have a whole environment built into its event on every line, and
    /// `ProgramInfo::target_queries` answers this exactly, at compile time, for free.
    wants_info: bool,
}

impl Script {
    /// Compile `source`, rendering a failure as the VRL diagnostic an operator needs to fix it.
    ///
    /// The diagnostic is deliberately the full multi-line `Formatter` output rather than a one-line
    /// summary: it carries the offending line, column and a caret, and `StartLogging` hands it
    /// straight to `docker run`, which is the only place the operator will look.
    pub fn compile(source: &str) -> Result<Script, String> {
        let functions = vrl::stdlib::all();
        let result = compile(source, &functions)
            .map_err(|diagnostics| Formatter::new(source, diagnostics).to_string())?;
        Ok(Script {
            wants_info: reads_info(&result.program),
            program: result.program,
        })
    }
}

/// Does this program ever read `.info`?
///
/// A query at the event root — `.`, `merge(., …)`, `keys(.)` — counts, because it can see
/// everything. Erring toward `true` costs one object clone per line; erring toward `false` would
/// silently hide container metadata from a script that asked for it.
fn reads_info(program: &Program) -> bool {
    program.info().target_queries.iter().any(|path| {
        path.prefix == PathPrefix::Event
            && match path.path.segments.first() {
                None => true,
                Some(OwnedSegment::Field(field)) => field.as_str() == "info",
                Some(OwnedSegment::Index(_)) => true,
            }
    })
}

/// Compiled scripts keyed by their source text.
///
/// A restart storm of 200 containers sharing one `--log-opt imbh-remap=…` must compile that script
/// once, not 200 times. Failures are cached too, so a broken script does not re-run the compiler on
/// every `StartLogging` retry.
#[derive(Default)]
pub struct Cache {
    entries: Mutex<Vec<CacheEntry>>,
}

/// One cached compilation: the source text it was keyed by, and its outcome — a compiled script or
/// the diagnostic that explains why there is not one.
type CacheEntry = (String, Result<Arc<Script>, String>);

/// Distinct scripts kept before the cache starts evicting. Far above any real deployment — this is
/// a bound on a pathological caller, not a tuning knob.
const CACHE_CAPACITY: usize = 32;

impl Cache {
    /// The compiled form of `source`, compiling it on first sight.
    pub fn get(&self, source: &str) -> Result<Arc<Script>, String> {
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((_, cached)) = entries.iter().find(|(key, _)| key == source) {
            return cached.clone();
        }
        let compiled = Script::compile(source).map(Arc::new);
        if entries.len() >= CACHE_CAPACITY {
            // Oldest first. The evicted entry's `Result` is the cached compile outcome, not a
            // fresh fallible operation, so there is nothing to handle here.
            drop(entries.remove(0));
        }
        entries.push((source.to_owned(), compiled.clone()));
        compiled
    }
}

/// A container's binding to a script: the program, plus the part of the event that does not vary
/// from line to line.
pub struct Bound {
    script: Arc<Script>,
    /// `.info`, `.resource` and the trace fields — cloned, not rebuilt, for every line.
    seed: Value,
    /// `.resource` as the script receives it, so "the script did not touch the resource" is one
    /// comparison rather than a rebuild.
    seed_resource: Value,
}

impl Bound {
    /// Bind `script` to a container, building the constant half of its event once.
    pub fn new(script: Arc<Script>, container: &Container, info: &ImbhValue) -> Bound {
        let mut root = ObjectMap::new();
        if script.wants_info {
            root.insert("info".into(), container_info(info));
        }
        let resource = resource_to_vrl(&container.resource());
        root.insert("resource".into(), resource.clone());
        Bound {
            script,
            seed: Value::Object(root),
            seed_resource: resource,
        }
    }
}

/// The `StartLogging` `Info` object as the script sees it: snake_case, with Docker's two awkward
/// shapes normalized (the container name loses its leading slash, and `ContainerEnv`'s `"K=V"`
/// strings become a map).
///
/// The labels and the environment are exposed in **full**, unlike the `labels=`/`env=` log-opts,
/// which still control what reaches `.resource`. That is a deliberate widening: a script cannot be
/// useful without them, and the vrl feature set here excludes the network, env and system function
/// groups, so a script has no way to send what it sees anywhere.
fn container_info(info: &ImbhValue) -> Value {
    let mut out = ObjectMap::new();
    for (key, source) in [
        ("container_id", "ContainerID"),
        ("container_image_id", "ContainerImageID"),
        ("container_image_name", "ContainerImageName"),
        ("daemon_name", "DaemonName"),
        ("log_path", "LogPath"),
    ] {
        out.insert(key.into(), Value::from(json::string(info, source)));
    }
    out.insert(
        "container_name".into(),
        Value::from(
            json::string(info, "ContainerName")
                .trim_start_matches('/')
                .to_owned(),
        ),
    );
    out.insert(
        "container_labels".into(),
        string_map_to_vrl(&json::string_map(info, "ContainerLabels")),
    );
    out.insert(
        "config".into(),
        string_map_to_vrl(&json::string_map(info, "Config")),
    );
    // `["REGION=eu-1"]` is a list on the wire and a map to anyone using it. A variable whose value
    // itself contains `=` splits on the FIRST one, which is what the shell does.
    let mut env = ObjectMap::new();
    for entry in json::string_list(info, "ContainerEnv") {
        let (name, value) = match entry.split_once('=') {
            Some((name, value)) => (name.to_owned(), value.to_owned()),
            None => (entry, String::new()),
        };
        env.insert(name.into(), Value::from(value));
    }
    out.insert("container_env".into(), Value::Object(env));
    Value::Object(out)
}

/// A container's bridge-network attachments, exactly as `Container` holds them.
type Attachments = Arc<Vec<(String, std::net::IpAddr)>>;

/// A container's bridge-network attachments as a VRL object: network name → address.
fn networks_to_vrl(networks: &[(String, std::net::IpAddr)]) -> Value {
    Value::Object(
        networks
            .iter()
            .map(|(name, ip)| (name.as_str().into(), Value::from(ip.to_string())))
            .collect(),
    )
}

fn string_map_to_vrl(pairs: &[(String, String)]) -> Value {
    Value::Object(
        pairs
            .iter()
            .map(|(key, value)| (key.as_str().into(), Value::from(value.clone())))
            .collect(),
    )
}

/// An OTLP `Resource` as a VRL object, so a script can read and extend it by attribute name.
fn resource_to_vrl(resource: &Resource) -> Value {
    Value::Object(
        resource
            .attributes
            .iter()
            .map(|kv| {
                (
                    kv.key.as_str().into(),
                    kv.value.as_ref().map_or(Value::Null, vrl_of),
                )
            })
            .collect(),
    )
}

/// The inverse of [`any`]: an OTLP value as a VRL value.
fn vrl_of(value: &AnyValue) -> Value {
    use any_value::Value as V;
    match &value.value {
        None => Value::Null,
        Some(V::StringValue(s)) => Value::from(s.clone()),
        Some(V::BoolValue(b)) => Value::Boolean(*b),
        Some(V::IntValue(i)) => Value::Integer(*i),
        Some(V::DoubleValue(d)) => Value::from_f64_or_zero(*d),
        Some(V::BytesValue(b)) => Value::from(b.as_slice()),
        Some(V::ArrayValue(a)) => Value::Array(a.values.iter().map(vrl_of).collect()),
        Some(V::KvlistValue(list)) => Value::Object(
            list.values
                .iter()
                .map(|kv| {
                    (
                        kv.key.as_str().into(),
                        kv.value.as_ref().map_or(Value::Null, vrl_of),
                    )
                })
                .collect(),
        ),
        // Experimental OTLP string-table index — unresolvable without the referenced table, which
        // imbh does not carry. Treated as absent, matching `imbh-otlp`'s `pb_to_any`.
        Some(V::StringValueStrindex(_)) => Value::Null,
    }
}

/// What one line's remap produced.
pub enum Outcome {
    /// The record, plus a resource override when the script changed `.resource`.
    Record(LogRecord, Option<Arc<Resource>>),
    /// The script called `abort`: drop the line. This is how a script filters health-check spam.
    Drop,
    /// The script failed. The caller stores the line the un-remapped way instead.
    Failed,
}

/// Quietest useful cadence for runtime-failure warnings: the first one, then at most one a minute,
/// carrying the count since the last report. A script that fails on every line of a container doing
/// 10k lines/s must cost one log line a minute, not 600k.
const REPORT_INTERVAL: Duration = Duration::from_secs(60);

/// One FIFO reader thread's remap state. Never shared, so nothing here needs a lock or an atomic.
pub struct Remapper {
    bound: Arc<Bound>,
    runtime: Runtime,
    /// Reused across lines so the event object's allocation is recycled.
    target: TargetValue,
    /// The last resource the script *changed* and the OTLP value it became, so a run of lines
    /// producing the same resource shares one allocation — which is what keeps `encode`'s grouping
    /// a pointer comparison (see `ingest::encode`).
    interned: Option<(Value, Arc<Resource>)>,
    /// The container's network attachments as VRL saw them last, keyed by the `Arc` they came from.
    /// Networks change on a discovery refresh, not per line, so this is rebuilt at most that often.
    networks: Option<(Attachments, Value)>,
    failures: u64,
    last_report: Option<Instant>,
}

impl Remapper {
    pub fn new(bound: Arc<Bound>) -> Remapper {
        Remapper {
            bound,
            runtime: Runtime::default(),
            target: TargetValue {
                value: Value::Object(ObjectMap::new()),
                metadata: Value::Object(ObjectMap::new()),
                secrets: Secrets::default(),
            },
            interned: None,
            networks: None,
            failures: 0,
            last_report: None,
        }
    }

    /// Run this container's script over one reassembled line.
    pub fn apply(&mut self, container: &Container, entry: &LogEntry) -> Outcome {
        let stderr = entry.source == "stderr";
        let stream = if stderr { "stderr" } else { "stdout" };
        let (number, band) = container.severity_for(stderr);
        let nanos = entry.time_nano.max(0);

        self.seed(container, entry, stream, nanos, number, band);

        let resolved = self.runtime.resolve(
            &mut self.target,
            &self.bound.script.program,
            &TimeZone::default(),
        );
        // Local variables must not survive into the next line.
        self.runtime.clear();

        match resolved {
            Ok(_) => self.build(container, stream, nanos, number, band),
            // An explicit `abort` is the script saying "drop this line", not a malfunction.
            Err(Terminate::Abort(ExpressionError::Abort { .. })) => Outcome::Drop,
            Err(Terminate::Abort(e) | Terminate::Error(e)) => {
                self.report(container, &e);
                Outcome::Failed
            }
        }
    }

    /// Rebuild the event: the constant half cloned, the per-line half overwritten.
    ///
    /// Both models are seeded at once — Docker's wire fields and the OTel record the driver would
    /// have stored on its own. That is what makes the identity script `.` a no-op.
    fn seed(
        &mut self,
        container: &Container,
        entry: &LogEntry,
        stream: &'static str,
        nanos: i64,
        number: i32,
        band: &'static str,
    ) {
        self.target.value = self.bound.seed.clone();
        // `.info.networks` is the one part of `.info` that is not fixed at `StartLogging`: bridge
        // discovery fills it in afterwards (`Container::set_networks`), because the driver must never
        // ask the daemon about a container from inside the handler that is starting it.
        if self.bound.script.wants_info {
            let networks = container.networks();
            let stale = !matches!(&self.networks, Some((seen, _)) if Arc::ptr_eq(seen, &networks));
            if stale {
                self.networks = Some((networks.clone(), networks_to_vrl(&networks)));
            }
            if let Some((_, value)) = &self.networks
                && let Value::Object(root) = &mut self.target.value
                && let Some(Value::Object(info)) = root.get_mut("info")
            {
                info.insert("networks".into(), value.clone());
            }
        }
        let Value::Object(root) = &mut self.target.value else {
            return;
        };

        let text = String::from_utf8_lossy(super::ingest::strip_newline(&entry.line)).into_owned();
        let stamp = Value::Timestamp(chrono::DateTime::from_timestamp_nanos(nanos));

        // Docker's log-driver model.
        root.insert("source".into(), Value::from(stream));
        root.insert("time_nano".into(), Value::Integer(nanos));
        root.insert("partial".into(), Value::Boolean(entry.partial));
        root.insert("line".into(), Value::from(text.clone()));

        // The OTel record this line would have become with no script at all.
        root.insert("body".into(), Value::from(text));
        root.insert("timestamp".into(), stamp.clone());
        root.insert("observed_timestamp".into(), stamp);
        root.insert("severity_number".into(), Value::Integer(number as i64));
        root.insert("severity_text".into(), Value::from(band));
        root.insert(
            "attributes".into(),
            Value::Object(ObjectMap::from([(
                "log.iostream".into(),
                Value::from(stream),
            )])),
        );

        // A script must not be able to smuggle state between lines.
        self.target.metadata = Value::Object(ObjectMap::new());
        self.target.secrets = Secrets::default();
    }

    /// Turn the resolved event back into an OTLP record, re-asserting the invariants the rest of
    /// the plugin depends on.
    fn build(
        &mut self,
        container: &Container,
        stream: &'static str,
        fallback_nanos: i64,
        number: i32,
        band: &'static str,
    ) -> Outcome {
        let Value::Object(root) = &self.target.value else {
            return Outcome::Failed;
        };

        let observed = nanos(root.get("observed_timestamp")).unwrap_or(fallback_nanos);
        let time = nanos(root.get("timestamp")).unwrap_or(observed);
        let severity_number = integer(root.get("severity_number"))
            .unwrap_or(number as i64)
            .clamp(0, 24) as i32;
        let severity_text = text(root.get("severity_text")).unwrap_or_else(|| band.to_owned());

        // A script that deleted the body would store empty rows and print empty `docker logs`
        // lines. The raw line is still on the event, so use it rather than storing nothing.
        let body = root
            .get("body")
            .filter(|value| !matches!(value, Value::Null))
            .or_else(|| root.get("line"))
            .map(any);

        let mut attributes = match root.get("attributes") {
            Some(Value::Object(object)) => kvs(object),
            _ => Vec::new(),
        };
        // INVARIANT: `readlogs::to_entry` restores the wire `source` from `log.iostream`. Without it
        // every line of this container comes back out of `docker logs` as stdout.
        if !attributes.iter().any(|kv| kv.key == "log.iostream") {
            attributes.push(super::ingest::kv("log.iostream", stream));
        }

        let record = LogRecord {
            time_unix_nano: time.max(0) as u64,
            observed_time_unix_nano: observed.max(0) as u64,
            severity_number,
            severity_text,
            body,
            attributes,
            trace_id: hex_id(root.get("trace_id"), 16).unwrap_or_default(),
            span_id: hex_id(root.get("span_id"), 8).unwrap_or_default(),
            flags: integer(root.get("trace_flags"))
                .unwrap_or(0)
                .clamp(0, u32::MAX as i64) as u32,
            ..Default::default()
        };

        let resource = match root.get("resource") {
            Some(value) if *value != self.bound.seed_resource => {
                Some(self.intern(value.clone(), container))
            }
            _ => None,
        };
        Outcome::Record(record, resource)
    }

    /// Turn the script's `.resource` into an OTLP `Resource`, enforcing the two resource invariants
    /// and interning the result.
    fn intern(&mut self, value: Value, container: &Container) -> Arc<Resource> {
        if let Some((last, resource)) = &self.interned
            && *last == value
        {
            // The overwhelmingly common case: the same resource as the line before.
            return resource.clone();
        }
        let mut attributes = match &value {
            Value::Object(object) => kvs(object),
            _ => Vec::new(),
        };

        // INVARIANT: `container.id` is what `ReadLogs` filters history on (`readlogs.rs`). A script
        // may ADD to the resource; it may not remove this and it may not change it — either would
        // orphan the container's own `docker logs`, and a *wrong* id would silently merge two
        // containers' histories, so this overwrites rather than filling in a gap.
        force(&mut attributes, "container.id", &container.id);
        // INVARIANT: `service.name` backs `LogRow.service`, the axis every imbh query starts from.
        // Unlike the id, a deliberate per-line override is legitimate — only absent or empty is not.
        if !attributes
            .iter()
            .any(|kv| kv.key == "service.name" && !is_empty(kv))
        {
            force(&mut attributes, "service.name", &container.service);
        }

        let resource = Arc::new(Resource {
            attributes,
            ..Default::default()
        });
        self.interned = Some((value, resource.clone()));
        resource
    }

    /// Report a runtime failure, rate-limited.
    fn report(&mut self, container: &Container, error: &ExpressionError) {
        self.failures = self.failures.saturating_add(1);
        let now = Instant::now();
        if !self
            .last_report
            .is_none_or(|last| now.duration_since(last) >= REPORT_INTERVAL)
        {
            return;
        }
        let count = std::mem::take(&mut self.failures);
        self.last_report = Some(now);
        super::warn(&format!(
            "remap script failed for {} ({count} line(s) since the last report, stored un-remapped): {error}",
            container.name_or_id()
        ));
    }
}

/// Set `key` to `value`, replacing any existing entry.
fn force(attributes: &mut Vec<KeyValue>, key: &str, value: &str) {
    attributes.retain(|kv| kv.key != key);
    attributes.push(super::ingest::kv(key, value));
}

fn is_empty(kv: &KeyValue) -> bool {
    match kv.value.as_ref().and_then(|v| v.value.as_ref()) {
        Some(any_value::Value::StringValue(s)) => s.is_empty(),
        None => true,
        _ => false,
    }
}

/// Convert a VRL value into an OTLP `AnyValue`.
///
/// Total over every [`Value`] variant on purpose: a script may put anything at all in `.body` or in
/// an attribute, and whatever it chose has to survive to the database rather than being silently
/// dropped on the way. `imbh-otlp` already understands the whole OTLP value space — a `KvlistValue`
/// body becomes canonical JSON in the `body` column (`imbh-otlp/src/lib.rs`, `body_to_string`) — so
/// nothing downstream needs to change to accept structured output.
pub(crate) fn any(value: &Value) -> AnyValue {
    use any_value::Value as V;
    let value = match value {
        // The overwhelmingly common case. Non-UTF-8 bytes go to OTLP's own bytes type instead of
        // being lossily stringified; imbh stores those base64 (ARCHITECTURE.md §6.1).
        Value::Bytes(b) => match std::str::from_utf8(b) {
            Ok(s) => V::StringValue(s.to_owned()),
            Err(_) => V::BytesValue(b.to_vec()),
        },
        // A regex has no OTLP shape. VRL itself renders one as its source text everywhere outside
        // VRL, so a script that leaks one into an attribute gets the pattern rather than an error.
        Value::Regex(r) => V::StringValue(r.to_string()),
        Value::Integer(i) => V::IntValue(*i),
        Value::Float(f) => V::DoubleValue(f.into_inner()),
        Value::Boolean(b) => V::BoolValue(*b),
        // RFC 3339 with nanosecond precision — the same text `docker logs` renders from a stored
        // record, so a timestamp-valued attribute reads back the way it was written.
        Value::Timestamp(t) => {
            V::StringValue(t.to_rfc3339_opts(chrono::SecondsFormat::Nanos, true))
        }
        Value::Object(o) => V::KvlistValue(KeyValueList { values: kvs(o) }),
        Value::Array(a) => V::ArrayValue(ArrayValue {
            values: a.iter().map(any).collect(),
        }),
        // OTLP has no null. An `AnyValue` with no `value` is what `imbh-otlp`'s `pb_to_any` decodes
        // back as `AnyValue::Null`, so the round trip is exact.
        Value::Null => return AnyValue { value: None },
    };
    AnyValue { value: Some(value) }
}

/// Convert a VRL object into OTLP key/value pairs.
///
/// Null-valued keys are dropped rather than stored. VRL's `del`/`remove` leave no null behind, but
/// `.attributes.foo = null` is easy to write by accident, and keeping it would put a `null` into
/// every affected row's canonical-JSON attribute blob for no query value.
pub(crate) fn kvs(object: &ObjectMap) -> Vec<KeyValue> {
    object
        .iter()
        .filter(|(_, value)| !matches!(value, Value::Null))
        .map(|(key, value)| KeyValue {
            key: key.as_str().to_owned(),
            value: Some(any(value)),
            ..Default::default()
        })
        .collect()
}

/// Epoch nanoseconds from a timestamp field.
///
/// Accepts either shape a script can produce: a real `Value::Timestamp` (what `parse_timestamp`,
/// `from_unix_timestamp` and `now` return) or a bare integer of epoch nanoseconds (what a script
/// that did the arithmetic itself returns). Anything else — including a *string* that merely looks
/// like a timestamp — is `None`, so the caller falls back rather than storing an epoch-zero row.
pub(crate) fn nanos(value: Option<&Value>) -> Option<i64> {
    match value? {
        Value::Timestamp(t) => t.timestamp_nanos_opt(),
        Value::Integer(n) => Some(*n),
        _ => None,
    }
}

/// An integer field, accepting the float a script gets from arithmetic on a parsed JSON number.
pub(crate) fn integer(value: Option<&Value>) -> Option<i64> {
    match value? {
        Value::Integer(n) => Some(*n),
        Value::Float(f) => Some(f.into_inner() as i64),
        _ => None,
    }
}

/// A string field. Only `Bytes` counts: coercing an integer or a map here would turn a script's
/// type error into a plausible-looking severity text rather than a fallback.
pub(crate) fn text(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::Bytes(b) => std::str::from_utf8(b).ok().map(str::to_owned),
        _ => None,
    }
}

/// Decode a hex trace or span id into exactly `len` bytes.
///
/// A malformed or wrong-length id is dropped rather than stored truncated or zero-padded: a 12-byte
/// trace id would join nothing, and an all-zero one is what OTLP already means by "absent", so a
/// half-decoded value is strictly worse than no value.
pub(crate) fn hex_id(value: Option<&Value>, len: usize) -> Option<Vec<u8>> {
    let text = text(value)?;
    let text = text.trim();
    if text.len() != len * 2 {
        return None;
    }
    let mut out = Vec::with_capacity(len);
    let bytes = text.as_bytes();
    for pair in bytes.chunks(2) {
        let hi = (pair[0] as char).to_digit(16)?;
        let lo = (pair[1] as char).to_digit(16)?;
        out.push((hi * 16 + lo) as u8);
    }
    // An all-zero id is OTLP's "unset"; storing it would claim a trace association that is not there.
    out.iter().any(|b| *b != 0).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A container built from a `StartLogging` `Info` document, with `script` bound to it.
    fn bind(info_json: &str, script: &str) -> (Container, Arc<Bound>) {
        let info = json::parse(info_json.as_bytes());
        let container = Container::from_info(&info);
        let compiled = Arc::new(Script::compile(script).expect("script compiles"));
        let bound = Arc::new(Bound::new(compiled, &container, &info));
        (container, bound)
    }

    fn entry(source: &str, line: &str) -> LogEntry {
        LogEntry {
            source: source.to_owned(),
            time_nano: 1_700_000_000_000_000_000,
            line: line.as_bytes().to_vec(),
            partial: false,
            partial_log_metadata: None,
        }
    }

    /// Run `script` over one stdout line of a minimal container.
    fn run(script: &str, line: &str) -> Outcome {
        run_on(
            r#"{"ContainerID":"abc123","ContainerName":"/web"}"#,
            script,
            "stdout",
            line,
        )
    }

    fn run_on(info_json: &str, script: &str, stream: &str, line: &str) -> Outcome {
        let (container, bound) = bind(info_json, script);
        Remapper::new(bound).apply(&container, &entry(stream, line))
    }

    fn record_of(outcome: Outcome) -> (LogRecord, Option<Arc<Resource>>) {
        match outcome {
            Outcome::Record(record, resource) => (record, resource),
            Outcome::Drop => panic!("expected a record, got Drop"),
            Outcome::Failed => panic!("expected a record, got Failed"),
        }
    }

    fn attr<'a>(attributes: &'a [KeyValue], key: &str) -> Option<&'a str> {
        attributes.iter().find(|kv| kv.key == key).and_then(|kv| {
            match kv.value.as_ref()?.value.as_ref()? {
                any_value::Value::StringValue(s) => Some(s.as_str()),
                _ => None,
            }
        })
    }

    fn body_str(record: &LogRecord) -> Option<&str> {
        match record.body.as_ref()?.value.as_ref()? {
            any_value::Value::StringValue(s) => Some(s.as_str()),
            _ => None,
        }
    }

    /// THE seeding contract: the driver pre-fills the event with the record it would have stored on
    /// its own, so a script that changes nothing must be indistinguishable from no script at all.
    /// Every other behaviour in this module rests on this.
    #[test]
    fn an_identity_script_reproduces_the_unremapped_record() {
        let info = json::parse(
            br#"{"ContainerID":"abc123","ContainerName":"/web","ContainerImageName":"nginx:1.27"}"#,
        );
        let container = Container::from_info(&info);
        let compiled = Arc::new(Script::compile(".").expect("identity compiles"));
        let bound = Arc::new(Bound::new(compiled, &container, &info));

        for (stream, line) in [("stdout", "hello\n"), ("stderr", "boom\n")] {
            let wire = entry(stream, line);
            let want = container.record(&wire);
            let (got, resource) = record_of(Remapper::new(bound.clone()).apply(&container, &wire));

            assert_eq!(got.time_unix_nano, want.time_unix_nano);
            assert_eq!(got.observed_time_unix_nano, want.observed_time_unix_nano);
            assert_eq!(got.severity_number, want.severity_number);
            assert_eq!(got.severity_text, want.severity_text);
            assert_eq!(got.body, want.body);
            assert_eq!(got.attributes, want.attributes);
            assert_eq!(got.trace_id, want.trace_id);
            assert_eq!(got.span_id, want.span_id);
            assert_eq!(got.flags, want.flags);
            // An untouched resource must not become an override — that is what keeps `encode`'s
            // per-container grouping working for the common case.
            assert!(
                resource.is_none(),
                "identity must not override the resource"
            );
        }
    }

    #[test]
    fn the_event_carries_the_docker_model_in_snake_case() {
        let script = r#"
            .body = {
                "line": .line, "source": .source, "time_nano": .time_nano, "partial": .partial,
                "id": .info.container_id, "name": .info.container_name,
                "image": .info.container_image_name,
                "label": .info.container_labels.app,
                "env": .info.container_env.REGION,
                "opt": .info.config."imbh-service"
            }
        "#;
        let info = r#"{"ContainerID":"abc123","ContainerName":"/web",
            "ContainerImageName":"nginx:1.27",
            "ContainerLabels":{"app":"cart"},
            "ContainerEnv":["REGION=eu-1"],
            "Config":{"imbh-service":"checkout"}}"#;
        let (record, _) = record_of(run_on(info, script, "stderr", "hi\n"));
        let Some(any_value::Value::KvlistValue(list)) = &record.body.as_ref().unwrap().value else {
            panic!("expected a kvlist body");
        };
        let got = |key: &str| attr(&list.values, key).map(str::to_owned);
        assert_eq!(got("line").as_deref(), Some("hi"));
        assert_eq!(got("source").as_deref(), Some("stderr"));
        // The name loses Docker's leading slash, and `K=V` env entries arrive pre-split.
        assert_eq!(got("name").as_deref(), Some("web"));
        assert_eq!(got("id").as_deref(), Some("abc123"));
        assert_eq!(got("image").as_deref(), Some("nginx:1.27"));
        assert_eq!(got("label").as_deref(), Some("cart"));
        assert_eq!(got("env").as_deref(), Some("eu-1"));
        assert_eq!(got("opt").as_deref(), Some("checkout"));
    }

    /// `.info.networks` is the one part of `.info` that is not fixed at `StartLogging`: bridge
    /// discovery fills it in afterwards, so the remapper has to read it from the container per line
    /// rather than from the frozen seed.
    #[test]
    fn a_script_sees_the_containers_networks_and_sees_them_change() {
        let (container, bound) = bind(
            r#"{"ContainerID":"abc","ContainerName":"/web"}"#,
            r#".body = .info.networks"#,
        );
        let mut remapper = Remapper::new(bound);

        // Before discovery knows anything, the map is present and empty rather than missing — a
        // script can write `.info.networks.bridge` without a fallible lookup either way.
        let (record, _) = record_of(remapper.apply(&container, &entry("stdout", "x\n")));
        let Some(any_value::Value::KvlistValue(list)) = &record.body.as_ref().unwrap().value else {
            panic!("expected a map body");
        };
        assert!(list.values.is_empty());

        container.set_networks(&[
            ("bridge".to_owned(), "172.17.0.2".parse().unwrap()),
            ("myproj_default".to_owned(), "172.23.0.7".parse().unwrap()),
        ]);
        let (record, _) = record_of(remapper.apply(&container, &entry("stdout", "x\n")));
        let Some(any_value::Value::KvlistValue(list)) = &record.body.as_ref().unwrap().value else {
            panic!("expected a map body");
        };
        assert_eq!(attr(&list.values, "bridge"), Some("172.17.0.2"));
        assert_eq!(attr(&list.values, "myproj_default"), Some("172.23.0.7"));
    }

    /// A script that never mentions `.info` must not pay for the networks object at all — the same
    /// `wants_info` bargain the rest of `.info` already gets.
    #[test]
    fn a_script_that_ignores_info_is_not_charged_for_the_networks() {
        let (container, bound) = bind(r#"{"ContainerID":"abc"}"#, ".body = .line");
        container.set_networks(&[("bridge".to_owned(), "172.17.0.2".parse().unwrap())]);
        let mut remapper = Remapper::new(bound);
        record_of(remapper.apply(&container, &entry("stdout", "x\n")));
        assert!(
            remapper.networks.is_none(),
            "the networks object must not be built for a script that cannot see it"
        );
    }

    #[test]
    fn the_full_label_and_env_maps_are_visible_even_when_the_log_opts_select_none() {
        // The `labels=`/`env=` log-opts govern what reaches `.resource`; a script sees everything.
        let info = r#"{"ContainerID":"abc","ContainerLabels":{"secret":"s"},
            "ContainerEnv":["TOKEN=t"]}"#;
        let script =
            r#".body = {"l": .info.container_labels.secret, "e": .info.container_env.TOKEN}"#;
        let (record, _) = record_of(run_on(info, script, "stdout", "x\n"));
        let Some(any_value::Value::KvlistValue(list)) = &record.body.as_ref().unwrap().value else {
            panic!("expected a kvlist body");
        };
        assert_eq!(attr(&list.values, "l"), Some("s"));
        assert_eq!(attr(&list.values, "e"), Some("t"));
    }

    #[test]
    fn an_env_value_containing_an_equals_sign_splits_only_once() {
        let info = r#"{"ContainerID":"abc","ContainerEnv":["DSN=postgres://u:p@h/db?x=1"]}"#;
        let (record, _) = record_of(run_on(
            info,
            ".body = .info.container_env.DSN",
            "stdout",
            "x\n",
        ));
        assert_eq!(body_str(&record), Some("postgres://u:p@h/db?x=1"));
    }

    #[test]
    fn a_script_can_set_every_otel_field() {
        let script = r#"
            .timestamp = from_unix_timestamp!(1234567890, unit: "seconds")
            .severity_number = 13
            .severity_text = "WARN"
            .body = "rewritten"
            .attributes.extra = "yes"
            .trace_id = "0123456789ABCDEF0123456789abcdef"
            .span_id = "0123456789abcdef"
            .trace_flags = 1
        "#;
        let (record, _) = record_of(run(script, "original\n"));
        assert_eq!(record.time_unix_nano, 1_234_567_890_000_000_000);
        // observed_timestamp is untouched, so it keeps Docker's capture time.
        assert_eq!(record.observed_time_unix_nano, 1_700_000_000_000_000_000);
        assert_eq!(record.severity_number, 13);
        assert_eq!(record.severity_text, "WARN");
        assert_eq!(body_str(&record), Some("rewritten"));
        assert_eq!(attr(&record.attributes, "extra"), Some("yes"));
        assert_eq!(record.flags, 1);
        assert_eq!(record.trace_id.len(), 16);
        // Hex case is normalized by the decoder, not preserved as text.
        assert_eq!(record.trace_id[0], 0x01);
        assert_eq!(record.span_id.len(), 8);
    }

    #[test]
    fn severity_numbers_outside_the_otel_range_are_clamped() {
        let (high, _) = record_of(run(".severity_number = 99", "x\n"));
        assert_eq!(high.severity_number, 24);
        let (low, _) = record_of(run(".severity_number = -5", "x\n"));
        assert_eq!(low.severity_number, 0);
    }

    // ── invariants ────────────────────────────────────────────────────────────────────────

    #[test]
    fn deleting_the_body_falls_back_to_the_raw_line() {
        let (record, _) = record_of(run("del(.body)", "still here\n"));
        assert_eq!(body_str(&record), Some("still here"));
    }

    #[test]
    fn deleting_log_iostream_restores_it() {
        let (record, _) = record_of(run_on(
            r#"{"ContainerID":"abc"}"#,
            "del(.attributes)",
            "stderr",
            "x\n",
        ));
        assert_eq!(attr(&record.attributes, "log.iostream"), Some("stderr"));
    }

    #[test]
    fn a_script_cannot_drop_container_id_or_service_name_from_the_resource() {
        let (_, resource) = record_of(run_on(
            r#"{"ContainerID":"abc123","ContainerName":"/web"}"#,
            ".resource = {}",
            "stdout",
            "x\n",
        ));
        let resource = resource.expect("emptying the resource is an override");
        assert_eq!(attr(&resource.attributes, "container.id"), Some("abc123"));
        assert_eq!(attr(&resource.attributes, "service.name"), Some("web"));
    }

    #[test]
    fn a_script_cannot_forge_a_different_container_id() {
        // Not merely restored when absent: a WRONG id would silently merge two containers'
        // `docker logs` histories, so the real one always wins.
        let (_, resource) = record_of(run_on(
            r#"{"ContainerID":"abc123","ContainerName":"/web"}"#,
            r#".resource."container.id" = "somebody-else""#,
            "stdout",
            "x\n",
        ));
        let resource = resource.expect("rewriting the resource is an override");
        assert_eq!(attr(&resource.attributes, "container.id"), Some("abc123"));
    }

    #[test]
    fn a_script_may_override_service_name_but_not_blank_it() {
        let (_, overridden) = record_of(run_on(
            r#"{"ContainerID":"abc","ContainerName":"/web"}"#,
            r#".resource."service.name" = "checkout""#,
            "stdout",
            "x\n",
        ));
        assert_eq!(
            attr(&overridden.expect("override").attributes, "service.name"),
            Some("checkout")
        );

        let (_, blanked) = record_of(run_on(
            r#"{"ContainerID":"abc","ContainerName":"/web"}"#,
            r#".resource."service.name" = """#,
            "stdout",
            "x\n",
        ));
        assert_eq!(
            attr(&blanked.expect("override").attributes, "service.name"),
            Some("web")
        );
    }

    #[test]
    fn a_script_may_add_resource_attributes() {
        let (_, resource) = record_of(run(r#".resource."deployment.environment" = "prod""#, "x\n"));
        let resource = resource.expect("adding an attribute is an override");
        assert_eq!(
            attr(&resource.attributes, "deployment.environment"),
            Some("prod")
        );
        assert_eq!(attr(&resource.attributes, "container.id"), Some("abc123"));
    }

    // ── control flow ──────────────────────────────────────────────────────────────────────

    #[test]
    fn abort_drops_the_line() {
        assert!(matches!(
            run(
                r#"if contains!(.line, "healthz") { abort }"#,
                "GET /healthz\n"
            ),
            Outcome::Drop
        ));
        // ...and only the lines it names.
        assert!(matches!(
            run(
                r#"if contains!(.line, "healthz") { abort }"#,
                "GET /orders\n"
            ),
            Outcome::Record(_, _)
        ));
    }

    #[test]
    fn a_runtime_error_falls_back_rather_than_losing_the_line() {
        assert!(matches!(
            run(r#".x = to_int!("not a number")"#, "x\n"),
            Outcome::Failed
        ));
    }

    #[test]
    fn a_compile_error_reports_a_vrl_diagnostic() {
        // `expect_err` would need `Script: Debug`, and a compiled `Program` has no useful one.
        let Err(error) = Script::compile(".body = to_int(\"5\")") else {
            panic!("an unhandled fallible call must not compile");
        };
        // The full formatter output, not a one-line summary: it is what `docker run` shows.
        assert!(error.contains("error"), "{error}");
        assert!(
            error.contains('^'),
            "expected a caret-annotated diagnostic, got: {error}"
        );
    }

    // ── the wants_info optimization ───────────────────────────────────────────────────────

    #[test]
    fn reads_info_matches_what_the_program_touches() {
        let wants = |source: &str| Script::compile(source).expect("compiles").wants_info;
        assert!(wants("."), "a root query sees everything");
        assert!(wants(".body = .info.container_id"));
        assert!(!wants(".body = .line"));
        assert!(!wants(".severity_number = 5"));
    }

    // ── caching and sources ───────────────────────────────────────────────────────────────

    #[test]
    fn the_source_grammar_covers_all_four_shapes() {
        assert_eq!(Source::parse(""), Source::Builtin);
        assert_eq!(Source::parse("  default "), Source::Builtin);
        assert_eq!(Source::parse("off"), Source::Off);
        assert_eq!(Source::parse("none"), Source::Off);
        assert_eq!(
            Source::parse("@/var/lib/imbh/remap/app.vrl"),
            Source::File("/var/lib/imbh/remap/app.vrl".to_owned())
        );
        assert_eq!(
            Source::parse(".body = .line"),
            Source::Inline(".body = .line".to_owned())
        );
    }

    #[test]
    fn an_unreadable_script_file_reports_the_path() {
        let error = Source::File("/nonexistent/imbh/remap.vrl".to_owned())
            .read()
            .expect_err("must fail");
        assert!(error.contains("/nonexistent/imbh/remap.vrl"), "{error}");
    }

    #[test]
    fn the_cache_returns_one_compilation_per_source_and_remembers_failures() {
        let cache = Cache::default();
        let first = cache.get(".").expect("compiles");
        let second = cache.get(".").expect("compiles");
        assert!(
            Arc::ptr_eq(&first, &second),
            "the same source must compile once"
        );
        assert!(cache.get(".body = to_int(\"5\")").is_err());
        // A cached failure is still a failure, not a silently-succeeding second attempt.
        assert!(cache.get(".body = to_int(\"5\")").is_err());
    }

    #[test]
    fn a_container_with_remap_off_gets_no_binding() {
        let info = json::parse(br#"{"ContainerID":"abc","Config":{"imbh-remap":"off"}}"#);
        let mut container = Container::from_info(&info);
        container
            .bind_remap(&info, &Source::Builtin, &Cache::default())
            .expect("off is not an error");
        assert!(container.remapper().is_none());
    }

    #[test]
    fn a_per_container_log_opt_overrides_the_daemon_default() {
        let info = json::parse(
            br#"{"ContainerID":"abc","Config":{"imbh-remap":".severity_number = 21"}}"#,
        );
        let mut container = Container::from_info(&info);
        container
            .bind_remap(&info, &Source::Off, &Cache::default())
            .expect("compiles");
        let mut remapper = container.remapper().expect("the log-opt wins over `off`");
        let (record, _) = record_of(remapper.apply(&container, &entry("stdout", "x\n")));
        assert_eq!(record.severity_number, 21);
    }

    #[test]
    fn a_bad_per_container_script_fails_the_binding_with_a_diagnostic() {
        let info = json::parse(
            br#"{"ContainerID":"abc","Config":{"imbh-remap":".body = to_int(\"5\")"}}"#,
        );
        let mut container = Container::from_info(&info);
        let error = container
            .bind_remap(&info, &Source::Builtin, &Cache::default())
            .expect_err("a broken script must not start the container silently");
        assert!(error.contains('^'), "{error}");
    }

    #[test]
    fn the_builtin_script_compiles() {
        // The single highest-value test here: without it, a typo in `default.vrl` ships.
        Script::compile(DEFAULT_SCRIPT).expect("the built-in remap script must compile");
    }

    // ── the built-in script ───────────────────────────────────────────────────────────────

    /// Run [`DEFAULT_SCRIPT`] over one line of a minimal container.
    fn builtin(line: &str) -> LogRecord {
        builtin_on(
            r#"{"ContainerID":"abc123","ContainerName":"/web"}"#,
            "stdout",
            line,
        )
    }

    fn builtin_on(info_json: &str, stream: &str, line: &str) -> LogRecord {
        record_of(run_on(info_json, DEFAULT_SCRIPT, stream, line)).0
    }

    /// The body's fields. Every remapped record has a map body, so a string body is a bug.
    fn body_fields(record: &LogRecord) -> Vec<KeyValue> {
        match &record.body.as_ref().expect("a body").value {
            Some(any_value::Value::KvlistValue(list)) => list.values.clone(),
            other => panic!("expected a map body, got {other:?}"),
        }
    }

    fn field<'a>(fields: &'a [KeyValue], key: &str) -> Option<&'a str> {
        attr(fields, key)
    }

    fn keys(fields: &[KeyValue]) -> Vec<&str> {
        fields.iter().map(|kv| kv.key.as_str()).collect()
    }

    #[test]
    fn json_lines_become_structured_bodies() {
        let record = builtin(r#"{"level":"warn","msg":"disk low","disk":"/dev/sda","free_mb":42}"#);
        let fields = body_fields(&record);
        assert_eq!(record.severity_number, 13);
        assert_eq!(record.severity_text, "WARN");
        assert_eq!(field(&fields, "msg"), Some("disk low"));
        assert_eq!(field(&fields, "disk"), Some("/dev/sda"));
        // Non-string JSON values keep their type rather than being stringified.
        assert_eq!(
            fields.iter().find(|kv| kv.key == "free_mb").unwrap().value,
            Some(AnyValue {
                value: Some(any_value::Value::IntValue(42))
            })
        );
        // The level was lifted onto the record, so `docker logs` will not print it twice.
        assert!(!keys(&fields).contains(&"level"));
    }

    #[test]
    fn a_bare_json_scalar_is_not_a_structured_record() {
        // `parse_json` would happily accept these; the brace guard is what stops them.
        for line in ["12\n", "\"hi\"\n", "null\n"] {
            let record = builtin(line);
            let fields = body_fields(&record);
            assert_eq!(keys(&fields), vec!["msg"], "{line:?} must stay prose");
        }
    }

    #[test]
    fn logfmt_lines_become_structured_bodies() {
        let record = builtin(r#"level=info msg="ready" port=8080"#);
        let fields = body_fields(&record);
        assert_eq!(record.severity_number, 9);
        assert_eq!(field(&fields, "msg"), Some("ready"));
        assert_eq!(field(&fields, "port"), Some("8080"));
    }

    #[test]
    fn logfmt_with_an_unquoted_trailing_message_still_parses() {
        // The strict pass rejects this on the bare words; the loose tier is what recovers it.
        let record = builtin("ts=2026-08-06T12:00:00Z level=error msg=connection refused by peer");
        let fields = body_fields(&record);
        assert_eq!(record.severity_number, 17);
        assert_eq!(field(&fields, "msg"), Some("connection"));
        // The trailing bare words are dropped rather than becoming `refused: true` attributes.
        assert!(!keys(&fields).contains(&"refused"));
        assert!(!keys(&fields).contains(&"peer"));
    }

    #[test]
    fn klog_lines_become_structured_bodies() {
        let record = builtin("I0505 17:59:40.692994   28133 klog.go:70] leader elected");
        let fields = body_fields(&record);
        assert_eq!(record.severity_number, 9);
        assert_eq!(record.severity_text, "INFO");
        // klog calls it `message`; it arrives as `msg`.
        assert_eq!(field(&fields, "msg"), Some("leader elected"));
        assert_eq!(field(&fields, "file"), Some("klog.go"));
    }

    #[test]
    fn glog_lines_become_structured_bodies() {
        let record = builtin("E20260505 17:59:40.692994   28133 glog.cc:70] cache miss storm");
        let fields = body_fields(&record);
        assert_eq!(record.severity_number, 17);
        assert_eq!(field(&fields, "msg"), Some("cache miss storm"));
    }

    #[test]
    fn comma_separated_key_values_become_structured_bodies() {
        let record = builtin("level=warn,msg=retrying,attempt=3");
        let fields = body_fields(&record);
        assert_eq!(record.severity_number, 13);
        assert_eq!(field(&fields, "msg"), Some("retrying"));
        assert_eq!(field(&fields, "attempt"), Some("3"));
    }

    /// A line carrying BOTH separators cannot be split without guessing, so it stays prose. Worth
    /// pinning: the tempting fix — trying commas whenever one is present — makes the space-delimited
    /// pass swallow `level=warn,msg=x` whole as a single field named `level`, which parses
    /// "successfully" and silently produces nonsense.
    #[test]
    fn a_line_with_both_separators_is_left_alone_rather_than_guessed_at() {
        let fields = body_fields(&builtin("level=warn,msg=hello world"));
        assert_eq!(keys(&fields), vec!["msg"]);
        assert_eq!(field(&fields, "msg"), Some("level=warn,msg=hello world"));
    }

    /// The acceptance criterion for the whole heuristic: prose must survive untouched.
    #[test]
    fn prose_lines_are_never_chopped_into_fields() {
        for line in [
            "starting server on port 8080",
            "usage: foo --opt=bar and more",
            "Listening on 0.0.0.0:8080 (press Ctrl-C)",
            "  indented note with an = sign in it",
            "GET /orders 200 12ms",
            "",
        ] {
            let record = builtin(line);
            let fields = body_fields(&record);
            assert_eq!(
                keys(&fields),
                vec!["msg"],
                "{line:?} must stay one message, got {:?}",
                keys(&fields)
            );
            assert_eq!(field(&fields, "msg"), Some(line.trim_end_matches('\n')));
            // Untouched severity means the stream default still applies.
            assert_eq!(record.severity_number, 9);
        }
    }

    #[test]
    fn the_message_key_is_normalised_to_msg() {
        for (line, want) in [
            (r#"{"message":"a","k":"1"}"#, "a"),
            (r#"{"log":"b","k":"1"}"#, "b"),
            (r#"{"event":"c","k":"1"}"#, "c"),
        ] {
            let fields = body_fields(&builtin(line));
            assert_eq!(field(&fields, "msg"), Some(want), "{line}");
            for old in ["message", "log", "event"] {
                assert!(
                    !keys(&fields).contains(&old),
                    "{old} should have been renamed"
                );
            }
        }

        // A line carrying BOTH keeps its own `msg` and does not lose the other.
        let fields = body_fields(&builtin(r#"{"msg":"mine","message":"theirs"}"#));
        assert_eq!(field(&fields, "msg"), Some("mine"));
        assert_eq!(field(&fields, "message"), Some("theirs"));
    }

    #[test]
    fn numeric_levels_map_to_otel_severities() {
        // pino/bunyan (10..60) and syslog (0..7) do not overlap, so each is unambiguous.
        for (level, want) in [
            (30, 9),  // bunyan info
            (40, 13), // bunyan warn
            (50, 17), // pino error
            (60, 21), // pino fatal
            (3, 17),  // syslog err
            (4, 13),  // syslog warning
            (7, 5),   // syslog debug
        ] {
            let record = builtin(&format!(r#"{{"level":{level},"msg":"x"}}"#));
            assert_eq!(record.severity_number, want, "level={level}");
        }
    }

    #[test]
    fn an_unrecognized_level_keeps_the_stream_default_and_stays_in_the_body() {
        let record = builtin(r#"{"level":"NOTICE_SPECIAL","msg":"x"}"#);
        // The stream default stands...
        assert_eq!(record.severity_number, 9);
        // ...and the producer's own value is not silently discarded.
        assert_eq!(
            field(&body_fields(&record), "level"),
            Some("NOTICE_SPECIAL")
        );
    }

    #[test]
    fn the_stream_severity_log_opts_still_apply_to_an_unlevelled_line() {
        let info = r#"{"ContainerID":"abc","Config":{"imbh-stdout-severity":"debug",
            "imbh-stderr-severity":"warn"}}"#;
        assert_eq!(builtin_on(info, "stdout", "plain line").severity_number, 5);
        assert_eq!(builtin_on(info, "stderr", "plain line").severity_number, 13);
    }

    #[test]
    fn a_lines_own_timestamp_becomes_the_event_time_and_docker_keeps_observed() {
        // The remapper seeds both from Docker's capture time (1_700_000_000e9 in these tests);
        // a line's own timestamp must move only `timestamp`.
        let record = builtin(r#"{"ts":"2023-11-14T22:20:00Z","msg":"x"}"#);
        assert_eq!(record.observed_time_unix_nano, 1_700_000_000_000_000_000);
        assert_eq!(record.time_unix_nano, 1_700_000_400_000_000_000);
        // Lifted out of the body, so `docker logs` prints it once.
        assert!(!keys(&body_fields(&record)).contains(&"ts"));
    }

    #[test]
    fn a_timestamp_far_from_the_capture_time_is_refused() {
        // `docker logs` pages and computes its follow watermark from `time`, so a skewed clock
        // must not be able to move it arbitrarily.
        let record = builtin(r#"{"ts":"1970-01-02T00:00:00Z","msg":"x"}"#);
        assert_eq!(record.time_unix_nano, 1_700_000_000_000_000_000);
        // Refused, so it stays in the body rather than disappearing.
        assert_eq!(
            field(&body_fields(&record), "ts"),
            Some("1970-01-02T00:00:00Z")
        );
    }

    #[test]
    fn epoch_timestamps_are_recognised_at_every_scale() {
        for raw in [
            "1700000400",          // seconds
            "1700000400000",       // milliseconds
            "1700000400000000",    // microseconds
            "1700000400000000000", // nanoseconds
        ] {
            let record = builtin(&format!(r#"{{"ts":{raw},"msg":"x"}}"#));
            assert_eq!(
                record.time_unix_nano, 1_700_000_400_000_000_000,
                "epoch {raw} should resolve to the same instant"
            );
        }
    }

    #[test]
    fn trace_and_span_ids_are_lifted_onto_the_record() {
        let record = builtin(
            r#"{"msg":"x","trace_id":"0123456789abcdef0123456789abcdef","span_id":"0123456789abcdef"}"#,
        );
        assert_eq!(record.trace_id.len(), 16);
        assert_eq!(record.span_id.len(), 8);
        // Kept in the body too — a correlation id is worth having in the text a human reads.
        assert!(keys(&body_fields(&record)).contains(&"trace_id"));

        // camelCase spellings work as well.
        let camel = builtin(r#"{"msg":"x","traceID":"0123456789abcdef0123456789abcdef"}"#);
        assert_eq!(camel.trace_id.len(), 16);
    }

    #[test]
    fn the_builtin_script_leaves_the_resource_and_iostream_alone() {
        let (record, resource) = record_of(run_on(
            r#"{"ContainerID":"abc123","ContainerName":"/web"}"#,
            DEFAULT_SCRIPT,
            "stderr",
            r#"{"level":"error","msg":"boom"}"#,
        ));
        // Never an override: the driver's container mapping is untouched, so batching keeps
        // grouping by container pointer alone.
        assert!(resource.is_none());
        assert_eq!(attr(&record.attributes, "log.iostream"), Some("stderr"));
    }

    // ── converters ────────────────────────────────────────────────────────────────────────

    fn string_of(v: &AnyValue) -> Option<&str> {
        match &v.value {
            Some(any_value::Value::StringValue(s)) => Some(s.as_str()),
            _ => None,
        }
    }

    #[test]
    fn every_value_variant_converts() {
        assert_eq!(string_of(&any(&Value::from("hi"))), Some("hi"));
        assert_eq!(
            any(&Value::Integer(7)).value,
            Some(any_value::Value::IntValue(7))
        );
        assert_eq!(
            any(&Value::Boolean(true)).value,
            Some(any_value::Value::BoolValue(true))
        );
        assert_eq!(
            any(&Value::from_f64_or_zero(1.5)).value,
            Some(any_value::Value::DoubleValue(1.5))
        );
        // Null is an AnyValue with no value at all — OTLP has no null variant.
        assert_eq!(any(&Value::Null).value, None);
    }

    #[test]
    fn non_utf8_bytes_stay_bytes_instead_of_being_mangled() {
        let raw = Value::Bytes(vec![0xff, 0xfe, 0x00].into());
        assert_eq!(
            any(&raw).value,
            Some(any_value::Value::BytesValue(vec![0xff, 0xfe, 0x00]))
        );
    }

    #[test]
    fn timestamps_render_as_rfc_3339_nanos() {
        let t = chrono::DateTime::from_timestamp_nanos(1_700_000_000_123_456_789);
        assert_eq!(
            string_of(&any(&Value::Timestamp(t))),
            Some("2023-11-14T22:13:20.123456789Z")
        );
    }

    #[test]
    fn nested_objects_and_arrays_survive() {
        let inner = ObjectMap::from([("k".into(), Value::Integer(1))]);
        let value = Value::Array(vec![Value::Object(inner), Value::from("x")]);
        let Some(any_value::Value::ArrayValue(array)) = any(&value).value else {
            panic!("expected an array");
        };
        assert_eq!(array.values.len(), 2);
        let Some(any_value::Value::KvlistValue(list)) = &array.values[0].value else {
            panic!("expected a kvlist");
        };
        assert_eq!(list.values[0].key, "k");
    }

    #[test]
    fn null_attributes_are_dropped_rather_than_stored() {
        let object = ObjectMap::from([
            ("kept".into(), Value::from("yes")),
            ("dropped".into(), Value::Null),
        ]);
        let pairs = kvs(&object);
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].key, "kept");
    }

    #[test]
    fn timestamps_decode_from_both_shapes_a_script_can_produce() {
        let t = chrono::DateTime::from_timestamp_nanos(42);
        assert_eq!(nanos(Some(&Value::Timestamp(t))), Some(42));
        assert_eq!(nanos(Some(&Value::Integer(42))), Some(42));
        // A string that merely looks like a timestamp is not one.
        assert_eq!(nanos(Some(&Value::from("2023-11-14T22:13:20Z"))), None);
        assert_eq!(nanos(None), None);
    }

    #[test]
    fn hex_ids_decode_and_malformed_ones_are_dropped() {
        let trace = Value::from("0123456789abcdef0123456789abcdef");
        assert_eq!(
            hex_id(Some(&trace), 16),
            Some(vec![
                0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef, 0x01, 0x23, 0x45, 0x67, 0x89, 0xab,
                0xcd, 0xef
            ])
        );
        // Wrong length, non-hex, and the all-zero "unset" id are all refused.
        assert_eq!(hex_id(Some(&Value::from("abcd")), 16), None);
        assert_eq!(hex_id(Some(&Value::from("z".repeat(32))), 16), None);
        assert_eq!(hex_id(Some(&Value::from("0".repeat(32))), 16), None);
        assert_eq!(hex_id(Some(&Value::Integer(5)), 16), None);
    }

    #[test]
    fn span_ids_use_their_own_width() {
        let span = Value::from("0123456789abcdef");
        assert_eq!(hex_id(Some(&span), 8).map(|b| b.len()), Some(8));
        // A trace-sized id is not a valid span id.
        assert_eq!(hex_id(Some(&span), 16), None);
    }
}

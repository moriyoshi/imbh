//! Runtime discovery of the Docker daemon's bridge networks.
//!
//! Until this module existed, the plugin's knowledge of Docker networking was a **build-time
//! constant**: `docker-plugin/build.sh` ran `docker network inspect bridge` once and baked the
//! answer into `IMBH_LISTEN_ADDR`, with a hard-coded `172.17.0.1` as the fallback. A daemon with a
//! custom `bip`, a re-created `docker0`, or an install where `docker plugin set` was never re-run
//! then bound a listener to an address that does not exist — silently, because the log-driver half
//! of the plugin is filesystem-only and keeps working regardless.
//!
//! Two backends answer the same question, tried in order on every refresh:
//!
//! | Backend | How | Sees |
//! |---|---|---|
//! | [`Source::Api`] | `GET /networks` over the daemon's Unix socket | network names, IPAM gateways and subnets, **and** each network's attached containers |
//! | [`Source::Ifaces`] | `getifaddrs` in the host netns | gateways and subnets only |
//!
//! The scan backend works because a managed plugin runs in the **host** network namespace
//! (`config.json` `"network": {"type": "host"}`, forced by the measured finding that
//! `network.type: bridge` is accepted but unimplemented — see `.agents/docs/JOURNAL.md`
//! 2026-07-30), so `docker0` and every `br-*` are directly visible. Docker programs a bridge
//! interface's address *from* its IPAM gateway, so for a stock daemon the scan reproduces the API's
//! gateway/subnet answer exactly. What it cannot do is name the Docker network, list the containers
//! on it, or recognise a bridge renamed with `com.docker.network.bridge.name` — which is what the
//! API backend is for.
//!
//! The API backend needs the daemon's socket inside the plugin's mount namespace, which the shipped
//! `config.json` does not grant (`mounts: []` is an invariant guarded by
//! `tests/docker_plugin_config.rs`). So the managed plugin runs in scan mode unless an operator
//! opts in; a standalone `imbhd` alongside a daemon gets API mode for free.
//!
//! The probe runs on **every** refresh rather than once at startup: at `docker plugin enable`
//! during daemon boot the API socket may not be serving yet, and a one-shot probe would strand the
//! process in scan mode for its whole life.
//!
//! ## The one thing this module must never do
//!
//! **Never call the Engine API from `StartLogging`.** `dockerd` calls the log driver synchronously
//! during container start while holding that container's lock, and the API's network-inspect path
//! resolves attached containers — so a call back into the daemon from that handler can deadlock the
//! daemon against its own log driver. Everything on the plugin's request path reads the last
//! published [`Snapshot`] and nothing else; the refresh thread is the only caller that talks to the
//! daemon.
//!
//! ## Footprint
//!
//! No new crate. `libc` is already a direct dependency of this crate (signal handling in
//! `shutdown.rs`) and already in the default graph via DataFusion; the Engine API client is
//! HTTP/1.1 written by hand over `std::os::unix::net::UnixStream`, and the JSON goes through
//! [`super::json`], which is `imbh::parse_json`. The refresh loop is a plain thread, so none of
//! this touches a tokio runtime.

use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::mpsc::RecvTimeoutError;
use std::sync::{Arc, Mutex, PoisonError, RwLock};
use std::time::Duration;

use imbh::AnyValue;

use crate::shutdown::Shutdown;

use super::addr::{Cidr, Discovery};
use super::json;

/// How long one Engine API call may take, connect to last byte. Short on purpose: this runs on a
/// timer, a slow answer is worth skipping rather than waiting out, and the scan backend is standing
/// by with an answer that is almost always identical.
const API_TIMEOUT: Duration = Duration::from_secs(2);

/// Largest Engine API response read. A daemon with thousands of networks would otherwise let a
/// background thread allocate without bound.
const MAX_RESPONSE: u64 = 8 * 1024 * 1024;

/// How often the daemon's networks are re-read when no interval is configured.
pub const DEFAULT_REFRESH: Duration = Duration::from_secs(30);

/// Resolve the rescan cadence from `IMBH_DOCKER_NETWORK_REFRESH` (a duration such as `30s`),
/// falling back to [`DEFAULT_REFRESH`]. Empty means unset; a malformed value is an error, for the
/// same reason a malformed flush spec is — quietly running a different cadence than the deployment
/// asked for is worse than refusing to start.
///
/// `0` is meaningful and kept: discover once at startup and never look again, which is the
/// zero-timer posture for a host whose networks do not change.
pub fn refresh_interval(env: Option<String>) -> imbh::Result<Duration> {
    let value = env.unwrap_or_default();
    let value = value.trim();
    match value.is_empty() {
        true => Ok(DEFAULT_REFRESH),
        false => imbh::parse_duration(value),
    }
}

/// The socket paths tried by [`Api::Auto`], after `DOCKER_HOST`.
const WELL_KNOWN_SOCKETS: [&str; 2] = ["/var/run/docker.sock", "/run/docker.sock"];

/// Where the Engine API is, if it is anywhere.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Api {
    /// Probe `DOCKER_HOST` and the well-known socket paths on every refresh.
    #[default]
    Auto,
    /// This socket and no other.
    Socket(PathBuf),
    /// Do not talk to the daemon at all; the interface scan is the only backend.
    Off,
}

impl Api {
    /// Parse the `IMBH_DOCKER_API` grammar: `auto`, `off`/`none`, or a socket path.
    pub fn parse(value: &str) -> Api {
        match value.trim() {
            "" | "auto" => Api::Auto,
            "off" | "none" => Api::Off,
            path => Api::Socket(PathBuf::from(path)),
        }
    }

    /// The socket to try this round, if any. `Auto` re-probes every time so API mode can engage
    /// after a daemon that was not ready at plugin-enable time comes up.
    fn socket(&self) -> Option<PathBuf> {
        match self {
            Api::Off => None,
            Api::Socket(path) => Some(path.clone()),
            Api::Auto => {
                let from_env = std::env::var("DOCKER_HOST")
                    .ok()
                    .and_then(|host| host.strip_prefix("unix://").map(PathBuf::from));
                from_env
                    .into_iter()
                    .chain(WELL_KNOWN_SOCKETS.iter().map(PathBuf::from))
                    .find(|path| path.exists())
            }
        }
    }
}

/// Which backend produced a snapshot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// Nothing has been discovered yet — the state before the first refresh completes.
    Unknown,
    /// The Engine API. Container attachments are populated.
    Api,
    /// The host-netns interface scan. Container attachments are empty and `name` is the interface.
    Ifaces,
}

/// One bridge network.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bridge {
    /// The Docker network name (`bridge`, `myproj_default`). In scan mode this is the interface
    /// name instead, because the daemon is the only thing that knows the mapping.
    pub name: String,
    /// The host interface (`docker0`, `br-1a2b3c4d5e6f`). Empty when the API did not say and it
    /// cannot be derived.
    pub iface: String,
    /// The IPAM gateway — the address a listener binds to be reachable from this network.
    pub gateway: IpAddr,
    /// The IPAM subnet, normalized.
    pub subnet: Cidr,
    /// Containers attached to this network, by id. Empty in scan mode.
    pub containers: Vec<(String, IpAddr)>,
}

/// Everything known about the daemon's bridge networks at one instant. Immutable once published,
/// so every consumer sees a consistent view without holding a lock.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub bridges: Vec<Bridge>,
    pub source: Source,
}

impl Snapshot {
    fn empty() -> Snapshot {
        Snapshot {
            bridges: Vec::new(),
            source: Source::Unknown,
        }
    }

    /// Every distinct gateway, in discovery order — what `auto` binds.
    pub fn gateways(&self) -> Vec<IpAddr> {
        let mut out: Vec<IpAddr> = Vec::new();
        for bridge in &self.bridges {
            if !out.contains(&bridge.gateway) {
                out.push(bridge.gateway);
            }
        }
        out
    }

    /// Every distinct subnet — what the `docker` allow-list token expands to.
    pub fn subnets(&self) -> Vec<Cidr> {
        let mut out: Vec<Cidr> = Vec::new();
        for bridge in &self.bridges {
            if !out.contains(&bridge.subnet) {
                out.push(bridge.subnet);
            }
        }
        out
    }

    /// The networks a container is attached to, as `(network name, address)` pairs. Always empty in
    /// scan mode, and empty for a container the last refresh did not see.
    pub fn container_networks(&self, id: &str) -> Vec<(String, IpAddr)> {
        self.bridges
            .iter()
            .filter_map(|bridge| {
                bridge
                    .containers
                    .iter()
                    .find(|(container, _)| container == id)
                    .map(|(_, ip)| (bridge.name.clone(), *ip))
            })
            .collect()
    }
}

/// Something to run after a refresh that *changed* the snapshot. This is how a container's resource
/// picks up network attributes it could not have had when it started (`ingest::Container`).
type Listener = Box<dyn Fn(&Arc<Snapshot>) + Send + Sync>;

/// The published view of the daemon's networks, plus the thread that keeps it fresh.
pub struct Networks {
    api: Api,
    snapshot: RwLock<Arc<Snapshot>>,
    listeners: Mutex<Vec<Listener>>,
}

impl Networks {
    /// A view that has discovered nothing yet. Call [`Networks::refresh`] to fill it, or
    /// [`Networks::spawn`] to keep it filled.
    pub fn new(api: Api) -> Arc<Networks> {
        Arc::new(Networks {
            api,
            snapshot: RwLock::new(Arc::new(Snapshot::empty())),
            listeners: Mutex::new(Vec::new()),
        })
    }

    /// The last published snapshot.
    pub fn snapshot(&self) -> Arc<Snapshot> {
        self.snapshot
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Register a callback for snapshot changes. Called on the refresh thread, so it must be quick
    /// and must not block on anything the refresh thread could be waiting for.
    pub fn on_change(&self, listener: impl Fn(&Arc<Snapshot>) + Send + Sync + 'static) {
        self.listeners
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(Box::new(listener));
    }

    /// Discover once and publish. Returns the current snapshot, changed or not.
    pub fn refresh(&self) -> Arc<Snapshot> {
        self.publish(discover(&self.api))
    }

    /// Publish `next`, notifying listeners only if anything actually changed — a refresh every 30
    /// seconds on an idle daemon must not rebuild every container's resource.
    fn publish(&self, next: Snapshot) -> Arc<Snapshot> {
        let next = Arc::new(next);
        let changed = {
            let mut current = self
                .snapshot
                .write()
                .unwrap_or_else(PoisonError::into_inner);
            let changed = **current != *next;
            if changed {
                *current = next.clone();
            }
            changed
        };
        if !changed {
            return self.snapshot();
        }
        for listener in self
            .listeners
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
        {
            listener(&next);
        }
        next
    }

    /// Refresh now, then every `interval` until `shutdown` trips. `interval` of zero means one-shot:
    /// discover once and never look again.
    ///
    /// The first refresh happens on the calling thread, so a listener that starts right after this
    /// returns already has addresses to bind rather than coming up empty and filling in a tick
    /// later.
    pub fn spawn(self: &Arc<Self>, interval: Duration, shutdown: &Arc<Shutdown>) {
        self.refresh();
        if interval.is_zero() {
            return;
        }
        let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();
        shutdown.on_trigger(move || {
            let _ = stop_tx.send(());
        });
        let networks = Arc::clone(self);
        std::thread::Builder::new()
            .name("imbh-netdisc".to_owned())
            .spawn(move || {
                // A timeout is a tick; anything else is shutdown (or a dropped token), and either
                // way there is nothing left to refresh for.
                while let Err(RecvTimeoutError::Timeout) = stop_rx.recv_timeout(interval) {
                    networks.refresh();
                }
            })
            // A refresh thread that cannot start is not fatal: the first refresh already ran, so
            // the process serves the addresses it found and simply stops noticing changes.
            .map(|_| ())
            .unwrap_or_else(|e| super::warn(&format!("no network refresh thread: {e}")));
    }
}

impl Discovery for Networks {
    fn gateways(&self) -> Vec<IpAddr> {
        self.snapshot().gateways()
    }

    fn subnets(&self) -> Vec<Cidr> {
        self.snapshot().subnets()
    }
}

/// Try the Engine API, fall back to the interface scan.
fn discover(api: &Api) -> Snapshot {
    // A failure here is deliberately silent: a plugin with no socket mounted is the *expected*
    // shipping configuration, the scan below answers the same question, and this runs on a timer —
    // warning would mean one line every refresh interval, for ever.
    if let Some(bridges) = api.socket().and_then(|s| api_bridges(&s).ok()) {
        return Snapshot {
            bridges,
            source: Source::Api,
        };
    }
    Snapshot {
        bridges: scan_bridges(),
        source: Source::Ifaces,
    }
}

// ── the Engine API backend ───────────────────────────────────────────────────────────────

/// `GET /networks`, decoded into the bridge networks it describes.
fn api_bridges(socket: &Path) -> Result<Vec<Bridge>, String> {
    Ok(bridges_from_networks_json(&get(socket, "/networks")?))
}

/// Decode the Engine API's `/networks` document. Split out from the transport so the whole decode
/// is testable against captured daemon output.
pub(crate) fn bridges_from_networks_json(body: &[u8]) -> Vec<Bridge> {
    let mut out = Vec::new();
    for network in json::items(&json::parse_any(body)) {
        if json::string(network, "Driver") != "bridge" {
            continue;
        }
        let name = json::string(network, "Name");
        let iface = api_iface(network, &name);
        let containers = api_containers(network);
        let ipam = json::field(network, "IPAM")
            .cloned()
            .unwrap_or(AnyValue::Null);
        for config in json::items(json::field(&ipam, "Config").unwrap_or(&AnyValue::Null)) {
            let (Ok(subnet), Ok(gateway)) = (
                Cidr::parse(&json::string(config, "Subnet")),
                json::string(config, "Gateway").parse::<IpAddr>(),
            ) else {
                // A network with no gateway (`--internal`, or IPv6 configured without one) has no
                // address a listener could bind. Nothing to do with it.
                continue;
            };
            out.push(Bridge {
                name: name.clone(),
                iface: iface.clone(),
                gateway,
                subnet: subnet.normalized(),
                containers: containers
                    .iter()
                    .filter(|(_, ip)| subnet.contains(*ip))
                    .cloned()
                    .collect(),
            });
        }
    }
    out
}

/// The host interface backing a network: what the operator named it, else Docker's own convention
/// — `docker0` for the default bridge, `br-` plus the first 12 characters of the network id.
fn api_iface(network: &AnyValue, name: &str) -> String {
    let options = json::field(network, "Options")
        .cloned()
        .unwrap_or(AnyValue::Null);
    let named = json::string(&options, "com.docker.network.bridge.name");
    if !named.is_empty() {
        return named;
    }
    if name == "bridge" {
        return "docker0".to_owned();
    }
    let id = json::string(network, "Id");
    match id.len() >= 12 {
        true => format!("br-{}", &id[..12]),
        false => String::new(),
    }
}

/// The `Containers` map: container id → its address on this network. Docker writes the address as
/// `IPv4Address: "172.17.0.2/16"`, so the prefix has to come off.
fn api_containers(network: &AnyValue) -> Vec<(String, IpAddr)> {
    let Some(AnyValue::Map(entries)) = json::field(network, "Containers") else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (id, endpoint) in entries {
        for key in ["IPv4Address", "IPv6Address"] {
            let raw = json::string(endpoint, key);
            let addr = raw.split('/').next().unwrap_or_default();
            if let Ok(ip) = addr.parse::<IpAddr>() {
                out.push((id.clone(), ip));
            }
        }
    }
    out
}

/// One HTTP/1.1 `GET` over a Unix socket, returning the response body.
///
/// `Connection: close` keeps this to a read-to-EOF with no keep-alive bookkeeping; the daemon
/// answers `Transfer-Encoding: chunked` regardless, which [`dechunk`] handles.
fn get(socket: &Path, path: &str) -> Result<Vec<u8>, String> {
    let fail = |what: &str, e: std::io::Error| format!("{}: {what}: {e}", socket.display());

    let mut stream = UnixStream::connect(socket).map_err(|e| fail("connect", e))?;
    stream
        .set_read_timeout(Some(API_TIMEOUT))
        .and_then(|()| stream.set_write_timeout(Some(API_TIMEOUT)))
        .map_err(|e| fail("timeouts", e))?;
    // No API version prefix: the daemon then answers with its own default version, which is the
    // only choice that cannot go stale against a daemon older or newer than this build.
    let head = format!(
        "GET {path} HTTP/1.1\r\nHost: docker\r\nAccept: application/json\r\nConnection: close\r\n\r\n"
    );
    stream
        .write_all(head.as_bytes())
        .and_then(|()| stream.flush())
        .map_err(|e| fail("write", e))?;

    let mut raw = Vec::new();
    (&stream)
        .take(MAX_RESPONSE)
        .read_to_end(&mut raw)
        .map_err(|e| fail("read", e))?;
    let (head, body) = split_response(&raw).ok_or_else(|| {
        format!(
            "{}: {path}: not an HTTP response ({} bytes)",
            socket.display(),
            raw.len()
        )
    })?;

    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse::<u16>().ok())
        .unwrap_or(0);
    if status != 200 {
        return Err(format!("{}: {path}: HTTP {status}", socket.display()));
    }
    match header_says_chunked(&head) {
        false => Ok(body.to_vec()),
        true => dechunk(body)
            .ok_or_else(|| format!("{}: {path}: malformed chunked body", socket.display())),
    }
}

/// Split a raw response at the header terminator, returning the head as text and the body as bytes.
fn split_response(raw: &[u8]) -> Option<(String, &[u8])> {
    let split = raw.windows(4).position(|w| w == b"\r\n\r\n")?;
    Some((
        String::from_utf8_lossy(&raw[..split]).into_owned(),
        &raw[split + 4..],
    ))
}

fn header_says_chunked(head: &str) -> bool {
    head.split("\r\n").any(|line| {
        line.to_ascii_lowercase().starts_with("transfer-encoding:") && line.contains("chunked")
    })
}

/// Decode a complete `Transfer-Encoding: chunked` body. `None` if it is truncated or malformed.
fn dechunk(raw: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut rest = raw;
    loop {
        let eol = rest.windows(2).position(|w| w == b"\r\n")?;
        // A chunk extension (`1a;name=value`) is legal and carries nothing this cares about.
        let size = std::str::from_utf8(&rest[..eol])
            .ok()?
            .split(';')
            .next()?
            .trim();
        let size = usize::from_str_radix(size, 16).ok()?;
        rest = rest.get(eol + 2..)?;
        if size == 0 {
            return Some(out);
        }
        out.extend_from_slice(rest.get(..size)?);
        // Skip the chunk's own trailing CRLF.
        rest = rest.get(size + 2..)?;
    }
}

// ── the interface-scan backend ───────────────────────────────────────────────────────────

/// Bridge networks as the host netns shows them.
fn scan_bridges() -> Vec<Bridge> {
    bridges_from_ifaddrs(&ifaddrs(), is_bridge_device)
}

/// Turn `(interface, address, netmask)` triples into bridges. Pure, with the "is this really a
/// bridge device" test injected, so the whole filter is testable without a network namespace.
pub(crate) fn bridges_from_ifaddrs(
    ifaddrs: &[(String, IpAddr, IpAddr)],
    is_bridge: impl Fn(&str) -> bool,
) -> Vec<Bridge> {
    ifaddrs
        .iter()
        .filter(|(name, _, _)| looks_like_docker_bridge(name) && is_bridge(name))
        .map(|(name, addr, mask)| Bridge {
            // The daemon is the only thing that knows the Docker network name; the interface is the
            // most useful stand-in, and it is what an operator sees in `ip link` anyway.
            name: name.clone(),
            iface: name.clone(),
            gateway: *addr,
            subnet: Cidr::from_mask(*addr, *mask).normalized(),
            containers: Vec::new(),
        })
        .collect()
}

/// Does this interface name look like one Docker made?
///
/// `docker0` is the default bridge; a user-defined network gets `br-` plus the first 12 hex
/// characters of its id. A bridge renamed through `com.docker.network.bridge.name` is missed —
/// there is nothing in the netns that identifies it as Docker's, and guessing wider would mean
/// binding a listener on somebody else's bridge. That is the gap the API backend covers.
fn looks_like_docker_bridge(name: &str) -> bool {
    if name == "docker0" {
        return true;
    }
    match name.strip_prefix("br-") {
        Some(id) => id.len() == 12 && id.bytes().all(|b| b.is_ascii_hexdigit()),
        None => false,
    }
}

/// Is this interface an actual bridge device? Only a bridge has `bridge/` in its sysfs directory,
/// which rules out a veth or a tap that happens to be named like one.
///
/// Non-Linux unices have no sysfs and no Docker bridge networks either, so the name test stands
/// alone there.
///
/// `name` is interpolated into a path, so it is rejected unless it is a **single ordinary path
/// component** — no `/`, no `..`, no absolute prefix. Two things already make a traversal
/// unreachable here: interface names come from the kernel, which does not permit `/` in one, and
/// [`bridges_from_ifaddrs`] runs [`looks_like_docker_bridge`] first, which admits only `docker0` or
/// `br-` plus 12 hex digits. But neither is visible from this function, and the second is an
/// ordering dependency inside a `&&` — reorder that expression, or call this from somewhere new,
/// and the only thing standing between a name and `/sys/class/net/../../..` would be gone. The
/// check below costs nothing and does not depend on either.
fn is_bridge_device(name: &str) -> bool {
    if !is_path_component(name) {
        return false;
    }
    match cfg!(target_os = "linux") {
        true => Path::new(&format!("/sys/class/net/{name}/bridge")).is_dir(),
        false => true,
    }
}

/// Is `name` exactly one ordinary path component — something that can only ever name a child of the
/// directory it is joined to?
///
/// Parsed rather than pattern-matched against a deny-list of `/` and `..`, so platform path syntax
/// is the authority on what a component is.
fn is_path_component(name: &str) -> bool {
    let mut components = Path::new(name).components();
    // `Normal` excludes `.`, `..`, and any root or prefix; comparing it back to `name` additionally
    // rejects anything the parser normalized away, such as a trailing slash.
    let single = matches!(
        components.next(),
        Some(std::path::Component::Normal(only)) if only.to_str() == Some(name)
    );
    single && components.next().is_none()
}

/// Every `(interface, address, netmask)` this host has, IPv4 and IPv6.
fn ifaddrs() -> Vec<(String, IpAddr, IpAddr)> {
    let mut out = Vec::new();
    let mut list: *mut libc::ifaddrs = std::ptr::null_mut();
    // SAFETY: `getifaddrs` allocates a linked list we own and must free exactly once. Every node is
    // null-checked before it is read, `ifa_next` is followed only while non-null, and the `sockaddr`
    // pointers are read through `read_unaligned` into owned values before use — so nothing borrows
    // the list past `freeifaddrs`.
    unsafe {
        if libc::getifaddrs(&mut list) != 0 {
            return out;
        }
        let mut node = list;
        while !node.is_null() {
            let entry = &*node;
            node = entry.ifa_next;
            if entry.ifa_name.is_null() {
                continue;
            }
            let (Some(addr), Some(mask)) =
                (sockaddr_ip(entry.ifa_addr), sockaddr_ip(entry.ifa_netmask))
            else {
                continue;
            };
            let name = std::ffi::CStr::from_ptr(entry.ifa_name)
                .to_string_lossy()
                .into_owned();
            out.push((name, addr, mask));
        }
        libc::freeifaddrs(list);
    }
    out
}

/// The IP inside a `sockaddr`, for the two families that carry one.
///
/// # Safety
/// `sa` must be null or point at a `sockaddr` whose family field is initialized and whose storage is
/// at least as large as the variant that family names.
unsafe fn sockaddr_ip(sa: *const libc::sockaddr) -> Option<IpAddr> {
    if sa.is_null() {
        return None;
    }
    // SAFETY: the caller guarantees `sa` points at an initialized `sockaddr`; reading it unaligned
    // is sound whatever the platform's alignment for the larger variants.
    unsafe {
        match libc::c_int::from(std::ptr::read_unaligned(sa).sa_family) {
            libc::AF_INET => {
                let v4 = std::ptr::read_unaligned(sa.cast::<libc::sockaddr_in>());
                Some(IpAddr::V4(Ipv4Addr::from(u32::from_be(v4.sin_addr.s_addr))))
            }
            libc::AF_INET6 => {
                let v6 = std::ptr::read_unaligned(sa.cast::<libc::sockaddr_in6>());
                Some(IpAddr::V6(Ipv6Addr::from(v6.sin6_addr.s6_addr)))
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().expect("an IP address")
    }

    fn cidr(s: &str) -> Cidr {
        Cidr::parse(s).expect("a CIDR")
    }

    /// Two networks as `GET /networks` really answers: the default bridge and a compose-created
    /// one, alongside a `host` network that must be ignored.
    const NETWORKS: &str = r#"[
      {
        "Name": "bridge",
        "Id": "aaaaaaaaaaaabbbbbbbbbbbbccccccccccccdddddddddddd000011112222",
        "Driver": "bridge",
        "IPAM": {"Driver": "default", "Config": [{"Subnet": "172.17.0.0/16", "Gateway": "172.17.0.1"}]},
        "Containers": {
          "deadbeefcafe": {"Name": "web", "IPv4Address": "172.17.0.2/16", "IPv6Address": ""}
        },
        "Options": {"com.docker.network.bridge.default_bridge": "true"}
      },
      {
        "Name": "myproj_default",
        "Id": "1a2b3c4d5e6f778899aabbccddeeff00112233445566778899aabbccddeeff",
        "Driver": "bridge",
        "IPAM": {"Config": [{"Subnet": "172.23.0.0/16", "Gateway": "172.23.0.1"}]},
        "Containers": {
          "deadbeefcafe": {"Name": "web", "IPv4Address": "172.23.0.7/16"},
          "0badc0ffee11": {"Name": "db", "IPv4Address": "172.23.0.8/16"}
        }
      },
      {"Name": "host", "Id": "hhhh", "Driver": "host", "IPAM": {"Config": []}}
    ]"#;

    #[test]
    fn the_engine_api_document_decodes_to_bridges() {
        let bridges = bridges_from_networks_json(NETWORKS.as_bytes());
        assert_eq!(bridges.len(), 2, "the host network must be filtered out");

        assert_eq!(bridges[0].name, "bridge");
        assert_eq!(bridges[0].iface, "docker0");
        assert_eq!(bridges[0].gateway, ip("172.17.0.1"));
        assert_eq!(bridges[0].subnet, cidr("172.17.0.0/16"));

        // A user-defined network's interface follows Docker's `br-<first 12 of id>` convention.
        assert_eq!(bridges[1].name, "myproj_default");
        assert_eq!(bridges[1].iface, "br-1a2b3c4d5e6f");
        assert_eq!(bridges[1].gateway, ip("172.23.0.1"));
    }

    #[test]
    fn container_attachments_are_read_and_the_prefix_is_stripped() {
        let snapshot = Snapshot {
            bridges: bridges_from_networks_json(NETWORKS.as_bytes()),
            source: Source::Api,
        };
        // One container on two networks comes back with both, named.
        assert_eq!(
            snapshot.container_networks("deadbeefcafe"),
            vec![
                ("bridge".to_owned(), ip("172.17.0.2")),
                ("myproj_default".to_owned(), ip("172.23.0.7")),
            ]
        );
        assert_eq!(
            snapshot.container_networks("0badc0ffee11"),
            vec![("myproj_default".to_owned(), ip("172.23.0.8"))]
        );
        assert!(snapshot.container_networks("nobody").is_empty());
    }

    #[test]
    fn an_operator_named_bridge_wins_over_the_derived_name() {
        let json = r#"[{"Name":"n","Id":"1a2b3c4d5e6f00","Driver":"bridge",
            "Options":{"com.docker.network.bridge.name":"mybr0"},
            "IPAM":{"Config":[{"Subnet":"10.9.0.0/24","Gateway":"10.9.0.1"}]}}]"#;
        assert_eq!(
            bridges_from_networks_json(json.as_bytes())[0].iface,
            "mybr0"
        );
    }

    #[test]
    fn a_network_without_a_gateway_is_skipped_rather_than_bound_to_nothing() {
        // `--internal` networks and IPv6 pools configured without a gateway both look like this.
        let json = r#"[{"Name":"internal","Id":"x","Driver":"bridge",
            "IPAM":{"Config":[{"Subnet":"10.8.0.0/24"}]}}]"#;
        assert!(bridges_from_networks_json(json.as_bytes()).is_empty());
    }

    #[test]
    fn a_malformed_or_empty_document_yields_no_bridges_rather_than_an_error() {
        for body in [&b""[..], b"not json", b"{}", b"[]", b"[null, 7]"] {
            assert!(bridges_from_networks_json(body).is_empty(), "{body:?}");
        }
    }

    #[test]
    fn a_container_address_outside_the_subnet_does_not_attach_to_it() {
        // A dual-stack network lists both addresses per container; each must land on the config
        // entry whose subnet actually contains it, not on both.
        let json = r#"[{"Name":"dual","Id":"x","Driver":"bridge","IPAM":{"Config":[
            {"Subnet":"172.30.0.0/16","Gateway":"172.30.0.1"},
            {"Subnet":"fd00:30::/64","Gateway":"fd00:30::1"}]},
            "Containers":{"abc":{"IPv4Address":"172.30.0.2/16","IPv6Address":"fd00:30::2/64"}}}]"#;
        let bridges = bridges_from_networks_json(json.as_bytes());
        assert_eq!(bridges.len(), 2);
        assert_eq!(
            bridges[0].containers,
            vec![("abc".to_owned(), ip("172.30.0.2"))]
        );
        assert_eq!(
            bridges[1].containers,
            vec![("abc".to_owned(), ip("fd00:30::2"))]
        );
    }

    // ── the scan backend ─────────────────────────────────────────────────────────────────

    #[test]
    fn the_scan_keeps_docker_bridges_and_nothing_else() {
        let ifaddrs = vec![
            ("lo".to_owned(), ip("127.0.0.1"), ip("255.0.0.0")),
            ("eth0".to_owned(), ip("192.168.10.131"), ip("255.255.255.0")),
            ("docker0".to_owned(), ip("172.17.0.1"), ip("255.255.0.0")),
            (
                "br-1a2b3c4d5e6f".to_owned(),
                ip("172.23.0.1"),
                ip("255.255.0.0"),
            ),
            // A libvirt bridge is a real bridge device, but not Docker's.
            (
                "virbr0".to_owned(),
                ip("192.168.122.1"),
                ip("255.255.255.0"),
            ),
            // The right shape, wrong length — not Docker's naming.
            ("br-short".to_owned(), ip("10.0.0.1"), ip("255.255.255.0")),
            // Right shape, but not hex.
            (
                "br-zzzzzzzzzzzz".to_owned(),
                ip("10.0.1.1"),
                ip("255.255.255.0"),
            ),
        ];
        let bridges = bridges_from_ifaddrs(&ifaddrs, |_| true);
        assert_eq!(
            bridges.iter().map(|b| b.iface.as_str()).collect::<Vec<_>>(),
            vec!["docker0", "br-1a2b3c4d5e6f"]
        );
        assert_eq!(bridges[0].gateway, ip("172.17.0.1"));
        assert_eq!(bridges[0].subnet, cidr("172.17.0.0/16"));
        assert_eq!(bridges[1].subnet, cidr("172.23.0.0/16"));
        // The daemon is the only thing that knows the Docker network name, so scan mode uses the
        // interface for both.
        assert_eq!(bridges[1].name, "br-1a2b3c4d5e6f");
        assert!(bridges[0].containers.is_empty());
    }

    #[test]
    fn a_veth_named_like_a_bridge_is_rejected_by_the_device_test() {
        let ifaddrs = vec![(
            "br-1a2b3c4d5e6f".to_owned(),
            ip("172.23.0.1"),
            ip("255.255.0.0"),
        )];
        assert!(bridges_from_ifaddrs(&ifaddrs, |_| false).is_empty());
        assert_eq!(bridges_from_ifaddrs(&ifaddrs, |_| true).len(), 1);
    }

    #[test]
    fn an_ipv6_bridge_address_is_discovered_too() {
        let ifaddrs = vec![(
            "docker0".to_owned(),
            ip("fd00:d0::1"),
            ip("ffff:ffff:ffff:ffff::"),
        )];
        let bridges = bridges_from_ifaddrs(&ifaddrs, |_| true);
        assert_eq!(bridges[0].subnet, cidr("fd00:d0::/64"));
    }

    /// `is_bridge_device` interpolates its argument into a path, so it must refuse anything that is
    /// not a single ordinary component — independently of the name filter that happens to run
    /// before it today.
    #[test]
    fn the_sysfs_probe_refuses_a_name_that_could_escape_its_directory() {
        for name in [
            "../../../etc",
            "..",
            ".",
            "",
            "/etc/passwd",
            "docker0/../../..",
            "a/b",
            "docker0/",
            "./docker0",
        ] {
            assert!(
                !is_bridge_device(name),
                "{name:?} must not reach the filesystem"
            );
            assert!(!is_path_component(name), "{name:?}");
        }
        // An ordinary name still is one. (Whether the device exists is the host's business; this
        // asserts only that the guard does not reject it.)
        for name in ["docker0", "br-1a2b3c4d5e6f", "eth0"] {
            assert!(is_path_component(name), "{name:?}");
        }
    }

    /// The two filters are independent: neither may be load-bearing for the other's job.
    #[test]
    fn the_name_filter_and_the_device_probe_do_not_rely_on_each_other() {
        // A traversal that would pass the *name* filter if it were ever reordered or removed.
        assert!(!is_bridge_device("br-../../../etc"));
        // ...and the name filter rejects it too, on its own.
        assert!(!looks_like_docker_bridge("br-../../../etc"));
    }

    /// The real thing, on whatever host runs the tests. It must not panic, and everything it
    /// returns must be self-consistent — that is all that can be asserted without a daemon.
    #[test]
    fn scanning_the_real_host_is_safe_and_self_consistent() {
        for bridge in scan_bridges() {
            assert!(looks_like_docker_bridge(&bridge.iface), "{bridge:?}");
            assert!(
                bridge.subnet.contains(bridge.gateway),
                "a gateway must be inside its own subnet: {bridge:?}"
            );
        }
    }

    // ── snapshots ────────────────────────────────────────────────────────────────────────

    fn bridge(name: &str, gateway: &str, subnet: &str) -> Bridge {
        Bridge {
            name: name.to_owned(),
            iface: name.to_owned(),
            gateway: ip(gateway),
            subnet: cidr(subnet),
            containers: Vec::new(),
        }
    }

    #[test]
    fn a_snapshot_deduplicates_gateways_and_subnets() {
        // A dual-stack bridge is two entries with one interface; a listener must not bind the same
        // address twice because of it.
        let snapshot = Snapshot {
            bridges: vec![
                bridge("docker0", "172.17.0.1", "172.17.0.0/16"),
                bridge("docker0", "172.17.0.1", "172.17.0.0/16"),
                bridge("br-1", "172.23.0.1", "172.23.0.0/16"),
            ],
            source: Source::Ifaces,
        };
        assert_eq!(
            snapshot.gateways(),
            vec![ip("172.17.0.1"), ip("172.23.0.1")]
        );
        assert_eq!(
            snapshot.subnets(),
            vec![cidr("172.17.0.0/16"), cidr("172.23.0.0/16")]
        );
    }

    #[test]
    fn listeners_fire_only_when_the_snapshot_actually_changes() {
        let networks = Networks::new(Api::Off);
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen = Arc::clone(&calls);
        networks.on_change(move |_| {
            seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        });

        let one = Snapshot {
            bridges: vec![bridge("docker0", "172.17.0.1", "172.17.0.0/16")],
            source: Source::Ifaces,
        };
        networks.publish(one.clone());
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        // Republishing the same view is what an idle daemon does every 30 seconds; it must not
        // rebuild anything.
        networks.publish(one.clone());
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);

        let mut two = one;
        two.bridges
            .push(bridge("br-1", "172.23.0.1", "172.23.0.0/16"));
        networks.publish(two);
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 2);
        assert_eq!(networks.gateways().len(), 2);
    }

    #[test]
    fn discovery_with_the_api_off_falls_straight_through_to_the_scan() {
        let snapshot = discover(&Api::Off);
        assert_eq!(snapshot.source, Source::Ifaces);
        // And a socket that is not there is the same story, not an error.
        let snapshot = discover(&Api::Socket(PathBuf::from("/nonexistent/imbh/docker.sock")));
        assert_eq!(snapshot.source, Source::Ifaces);
    }

    #[test]
    fn the_api_setting_parses() {
        assert_eq!(Api::parse(""), Api::Auto);
        assert_eq!(Api::parse(" auto "), Api::Auto);
        assert_eq!(Api::parse("off"), Api::Off);
        assert_eq!(Api::parse("none"), Api::Off);
        assert_eq!(
            Api::parse("/var/run/docker.sock"),
            Api::Socket(PathBuf::from("/var/run/docker.sock"))
        );
        assert_eq!(Api::Off.socket(), None);
    }

    // ── the HTTP/1.1 client ──────────────────────────────────────────────────────────────

    #[test]
    fn chunked_bodies_decode() {
        assert_eq!(dechunk(b"4\r\nabcd\r\n0\r\n\r\n"), Some(b"abcd".to_vec()));
        // Several chunks, a chunk extension, and a trailer section after the terminator.
        assert_eq!(
            dechunk(b"2\r\nab\r\n3;x=y\r\ncde\r\n0\r\nX-T: 1\r\n\r\n"),
            Some(b"abcde".to_vec())
        );
        assert_eq!(dechunk(b"0\r\n\r\n"), Some(Vec::new()));
    }

    #[test]
    fn a_truncated_or_malformed_chunked_body_is_refused_rather_than_half_decoded() {
        for raw in [
            &b"4\r\nab"[..],   // short data
            b"zz\r\nabcd\r\n", // size is not hex
            b"4\r\nabcd",      // no terminator
            b"",               // nothing at all
        ] {
            assert_eq!(dechunk(raw), None, "{raw:?}");
        }
    }

    #[test]
    fn responses_split_at_the_header_terminator() {
        let (head, body) = split_response(b"HTTP/1.1 200 OK\r\nX: 1\r\n\r\nbody").expect("splits");
        assert!(head.starts_with("HTTP/1.1 200 OK"));
        assert_eq!(body, b"body");
        assert!(split_response(b"no terminator here").is_none());
        assert!(header_says_chunked(
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked"
        ));
        assert!(!header_says_chunked("HTTP/1.1 200 OK\r\nContent-Length: 4"));
    }

    /// The transport end to end, against a fake daemon on a real Unix socket — the same shape as
    /// `tests/docker_plugin_e2e.rs`, which fakes `dockerd` the other way round. No daemon, no
    /// network, so this stays inside the hermetic `cargo test` rule (TESTING.md).
    #[test]
    fn the_client_talks_to_a_fake_daemon() {
        let dir = std::env::temp_dir().join(format!("imbh-netdisc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let socket = dir.join("docker.sock");
        let _ = std::fs::remove_file(&socket);
        let listener = std::os::unix::net::UnixListener::bind(&socket).expect("bind");

        let server = std::thread::spawn(move || {
            for _ in 0..2 {
                let Ok((mut stream, _)) = listener.accept() else {
                    return;
                };
                // Read the request head so the client's write does not race the close.
                let mut head = Vec::new();
                let mut byte = [0u8; 1];
                while !head.ends_with(b"\r\n\r\n") {
                    match std::io::Read::read(&mut stream, &mut byte) {
                        Ok(1) => head.push(byte[0]),
                        _ => break,
                    }
                }
                let request = String::from_utf8_lossy(&head).into_owned();
                let reply: Vec<u8> = match request.starts_with("GET /networks ") {
                    true => {
                        // Chunked, exactly as the daemon frames it.
                        let body = NETWORKS.as_bytes();
                        let mut out = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                             Transfer-Encoding: chunked\r\n\r\n"
                            .to_vec();
                        out.extend_from_slice(format!("{:x}\r\n", body.len()).as_bytes());
                        out.extend_from_slice(body);
                        out.extend_from_slice(b"\r\n0\r\n\r\n");
                        out
                    }
                    false => b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n".to_vec(),
                };
                let _ = stream.write_all(&reply);
                let _ = stream.flush();
            }
        });

        let bridges = api_bridges(&socket).expect("the fake daemon answers");
        assert_eq!(bridges.len(), 2);
        assert_eq!(bridges[0].gateway, ip("172.17.0.1"));

        // A non-200 is an error, not an empty answer — otherwise discovery would report "this
        // daemon has no bridges" whenever the API says something unexpected.
        let error = get(&socket, "/nope").expect_err("404 must fail");
        assert!(error.contains("404"), "{error}");

        server.join().expect("the fake daemon thread");
        let _ = std::fs::remove_file(&socket);
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn a_socket_that_is_not_there_is_an_error_the_caller_can_fall_back_from() {
        assert!(api_bridges(Path::new("/nonexistent/imbh/docker.sock")).is_err());
    }
}

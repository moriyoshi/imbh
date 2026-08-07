//! The Docker Desktop VM's host-facing interface, and why `auto` has to include it.
//!
//! On a Linux host, "every bridge gateway this daemon has" is the whole answer: the daemon runs on
//! the machine the containers run on, so a listener on `172.17.0.1` is reachable by every container
//! and by nothing outside the box. On **Docker Desktop** the daemon runs inside a VM, and that VM
//! has one more interface that matters — the link to the machine you are sitting at, on
//! `192.168.65.0/24` by default. A managed plugin binds only bridge gateways, so nothing it serves
//! answers on that link, and a container pointed at Docker Desktop's own `gateway.docker.internal`
//! name reaches an address no listener holds.
//!
//! This module is the missing address: when the process is running inside a Docker Desktop VM,
//! [`addresses`] reports the address(es) of the interface the VM's default route leaves by, and
//! [`super::networks::Snapshot::gateways`] hands them to `auto` alongside the bridge gateways.
//!
//! ## Why this is gated rather than always on
//!
//! The default-route interface of an ordinary Linux server is its **LAN** interface. Binding it
//! would publish an unauthenticated `/admin/*` on the office network — the exact exposure `auto`
//! exists to avoid (`ARCHITECTURE.md` §10.16, and `IMBH_ALLOW_FROM` for why the endpoint is not
//! self-defending). So the address is offered only where the far end of that interface is a
//! hypervisor host rather than a network: inside a Docker Desktop VM, recognised by
//! [`is_docker_desktop`], or when an operator asserts it with `IMBH_DOCKER_VM_NET=on`.
//!
//! ## Why netlink rather than `/proc/net/route`
//!
//! `/proc/net/route` lists the **main** table only. A host using policy routing can have its
//! default route in another table selected by an `ip rule`, and then that file either says nothing
//! or names the wrong interface. [`netlink`] asks the kernel the question `ip route get` asks —
//! resolve this destination, rules included — so the answer is the one a packet would actually get.
//!
//! ## Footprint
//!
//! No new crate. The netlink exchange is `socket`/`send`/`recv` through `libc`, which this crate
//! already depends on (`shutdown.rs`, and `networks.rs`'s `getifaddrs`), and the messages are
//! encoded and decoded as bytes — the rtnetlink layouts are ABI, so there is nothing to bind to.

use std::net::IpAddr;

/// Whether `auto` also binds the VM's host-facing interface.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum VmNet {
    /// Only when this process is running inside a Docker Desktop VM. The default: it costs nothing
    /// on a Linux host, where the test never passes.
    #[default]
    Auto,
    /// Always. For a VM imbh does not recognise — the operator is asserting that the default-route
    /// interface faces a hypervisor host and not a LAN.
    On,
    /// Never: `auto` means bridge gateways and nothing else, exactly as it did before this existed.
    Off,
}

impl VmNet {
    /// Parse the `IMBH_DOCKER_VM_NET` grammar. A typo is an error rather than a silent `off`, for
    /// the same reason a malformed flush spec is fatal: quietly serving a different address set
    /// than the deployment asked for is the failure this feature exists to end.
    pub fn parse(value: &str) -> Result<VmNet, String> {
        match value.trim() {
            "" | "auto" => Ok(VmNet::Auto),
            "on" | "true" => Ok(VmNet::On),
            "off" | "false" => Ok(VmNet::Off),
            other => Err(format!("expected `auto`, `on` or `off`, got `{other}`")),
        }
    }
}

/// Addresses `auto` gains from the VM's host-facing interface, or empty when there is no such
/// interface to offer.
///
/// `daemon_os` is the Engine API's `OperatingSystem` string when the API backend answered this
/// refresh, and empty otherwise — the interface scan is the shipped plugin's backend, so detection
/// cannot depend on it.
pub(crate) fn addresses(setting: VmNet, daemon_os: &str) -> Vec<IpAddr> {
    if !wanted(setting, daemon_os) {
        return Vec::new();
    }
    let found = on_default_route(
        &netlink::default_route_ifaces(),
        &super::networks::ifaddrs(),
    );
    announce(&found);
    found
}

/// Does this deployment want the VM interface bound at all?
fn wanted(setting: VmNet, daemon_os: &str) -> bool {
    match setting {
        VmNet::Off => false,
        VmNet::On => true,
        VmNet::Auto => is_docker_desktop(&Markers::read(daemon_os)),
    }
}

/// The bindable addresses of the interfaces `ifaces` names.
///
/// Pure, so the whole selection is testable without a netns: [`netlink`] supplies the interface
/// names and [`super::networks::ifaddrs`] the addresses. A link-local, loopback or unspecified
/// address is rejected by the same [`super::networks::is_usable_gateway`] rule the bridge gateways
/// go through — a VM's uplink carries an IPv6 link-local just as a bridge does, and it can no more
/// be bound here than there.
fn on_default_route(ifaces: &[String], ifaddrs: &[(String, IpAddr, IpAddr)]) -> Vec<IpAddr> {
    let mut out: Vec<IpAddr> = Vec::new();
    for (name, addr, _mask) in ifaddrs {
        if ifaces.iter().any(|iface| iface == name)
            && super::networks::is_usable_gateway(*addr)
            && !out.contains(addr)
        {
            out.push(*addr);
        }
    }
    out
}

/// Say once that the VM interface joined the listen set.
///
/// This is a feature whose failure mode is invisible — the endpoint is simply not where the
/// operator expects it — so the one line that says it engaged is worth more than the silence. Once,
/// not per refresh: discovery runs on a timer for the life of the process.
fn announce(addrs: &[IpAddr]) {
    use std::sync::atomic::{AtomicBool, Ordering};
    static SAID: AtomicBool = AtomicBool::new(false);

    if addrs.is_empty() || SAID.swap(true, Ordering::Relaxed) {
        return;
    }
    let list = addrs
        .iter()
        .map(|a| a.to_string())
        .collect::<Vec<_>>()
        .join(", ");
    super::warn(&format!(
        "Docker Desktop VM: `auto` also binds the VM's host-facing address(es) {list} \
         (IMBH_DOCKER_VM_NET=off to stop)"
    ));
}

// ── recognising the VM ───────────────────────────────────────────────────────────────────

/// What a Docker Desktop VM is recognised by. Read from the kernel and from the daemon rather than
/// from the filesystem: a managed plugin has its own rootfs and `mounts: []` (an invariant guarded
/// by `tests/docker_plugin_config.rs`), so nothing of the VM's `/` is visible to it. `/proc` is —
/// the runtime mounts it — and it is where both host-wide facts below come from.
#[derive(Debug, Default)]
pub(crate) struct Markers {
    /// `/proc/version`.
    pub kernel: String,
    /// `/proc/sys/kernel/hostname`.
    pub hostname: String,
    /// `OperatingSystem` from the Engine API's `GET /info`, when there is a socket to ask.
    pub daemon_os: String,
}

impl Markers {
    fn read(daemon_os: &str) -> Markers {
        let read = |path: &str| std::fs::read_to_string(path).unwrap_or_default();
        Markers {
            kernel: read("/proc/version"),
            hostname: read("/proc/sys/kernel/hostname"),
            daemon_os: daemon_os.to_owned(),
        }
    }
}

/// Is this process running inside a Docker Desktop VM?
///
/// Three independent signals, because no single one covers both backends:
///
/// * The daemon's own `OperatingSystem` reads `Docker Desktop` — authoritative, and available only
///   when a socket is mounted, which the shipped plugin does not have.
/// * The kernel is a LinuxKit build. Docker Desktop's VM on macOS and on the Hyper-V backend runs
///   one, and `/proc/version` names it whatever namespaces the process is in.
/// * The UTS name is `docker-desktop`, which is what the WSL 2 backend's distro is called — there
///   the kernel is an ordinary WSL one and says nothing.
///
/// A false negative costs the extra listener and nothing else; `IMBH_DOCKER_VM_NET=on` is the
/// override. A false positive is the expensive direction, which is why none of these matches a
/// plain Linux server.
pub(crate) fn is_docker_desktop(markers: &Markers) -> bool {
    markers.daemon_os.contains("Docker Desktop")
        || markers.kernel.to_ascii_lowercase().contains("linuxkit")
        || markers.hostname.trim() == "docker-desktop"
}

// ── the netlink route lookup ─────────────────────────────────────────────────────────────

#[cfg(target_os = "linux")]
mod netlink {
    //! Just enough of `NETLINK_ROUTE` to answer "which interface would a packet leave by?".
    //!
    //! Two queries, in order:
    //!
    //! 1. A **route get** — one `RTM_GETROUTE` naming a destination, which makes the kernel run the
    //!    real FIB lookup, `ip rule` policy included, and reply with the route it selected. This is
    //!    what `ip route get` does, and it is the reason this module exists rather than a reader for
    //!    `/proc/net/route`, which only ever shows the main table.
    //! 2. A **dump**, if that answered nothing: every route in every table, keeping those with a
    //!    zero-length destination. A blackhole or a missing route for the probe destination is what
    //!    lands here.
    //!
    //! Nothing is ever sent to the probe destinations — they are documentation-range addresses
    //! (RFC 5737, RFC 3849) resolved against the routing table and then discarded. Documentation
    //! space is chosen precisely because a host is unlikely to carry a specific route for it, so
    //! the lookup returns the default path rather than somebody's special case.

    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    // `linux/netlink.h` and `linux/rtnetlink.h`. Spelled out here rather than taken from `libc`,
    // whose rtnetlink coverage varies by version and target; these are kernel ABI and cannot drift.
    const NLMSG_HEADER: usize = 16;
    const NLMSG_ERROR: u16 = 2;
    const NLMSG_DONE: u16 = 3;
    const NLM_F_REQUEST: u16 = 0x001;
    /// `NLM_F_ROOT | NLM_F_MATCH`, i.e. "dump everything that matches".
    const NLM_F_DUMP: u16 = 0x300;
    const RTM_NEWROUTE: u16 = 24;
    const RTM_GETROUTE: u16 = 26;
    /// `struct rtmsg`: eight `u8`s and a `u32`.
    const RTMSG: usize = 12;
    /// `struct rtattr`: two `u16`s.
    const RTA_HEADER: usize = 4;
    const RTA_DST: u16 = 1;
    const RTA_OIF: u16 = 4;
    /// `RTN_UNICAST` — a route that actually goes somewhere, as opposed to a blackhole, a
    /// prohibit, or an unreachable, none of which name an interface worth binding.
    const RTN_UNICAST: u8 = 1;

    /// The FIB probes. Never contacted: `RTM_GETROUTE` is a question about the routing table.
    const PROBE_V4: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 1);
    const PROBE_V6: Ipv6Addr = Ipv6Addr::new(0x2001, 0x0db8, 0, 0, 0, 0, 0, 1);

    /// How long one netlink reply may take. Generous for a call that never leaves the kernel; it
    /// exists so a lost reply cannot park the discovery thread.
    const TIMEOUT_SECS: i64 = 1;

    /// Reads a dump may take before it is abandoned — a backstop against a pathological routing
    /// table, not a real bound. 64 × 8 KiB of routes is far past any host this runs on.
    const MAX_READS: usize = 64;

    /// The interfaces the kernel would send a packet out of, most-specific answer first.
    pub(super) fn default_route_ifaces() -> Vec<String> {
        let mut indices: Vec<u32> = Vec::new();
        for probe in [IpAddr::V4(PROBE_V4), IpAddr::V6(PROBE_V6)] {
            collect_oifs(
                &query(&route_get_payload(probe), 0, false),
                false,
                &mut indices,
            );
        }
        // No answer for either probe: a blackhole, or a host with no route to documentation space.
        // Fall back to every default route the kernel has, whichever table it lives in.
        if indices.is_empty() {
            collect_oifs(
                &query(&dump_payload(), NLM_F_DUMP, true),
                true,
                &mut indices,
            );
        }
        indices
            .iter()
            .filter_map(|index| iface_name(*index))
            .collect()
    }

    /// One request/response exchange on a fresh `NETLINK_ROUTE` socket.
    ///
    /// A socket per query, so there is no state to disambiguate and a fixed sequence number is
    /// enough. `dump` decides how the read ends: a dump is terminated by `NLMSG_DONE`, while a
    /// route get is answered by exactly one message and no terminator — reading for one more would
    /// mean waiting out [`TIMEOUT_SECS`] on every refresh.
    fn query(payload: &[u8], flags: u16, dump: bool) -> Vec<u8> {
        let Some(socket) = Socket::open() else {
            return Vec::new();
        };
        if !socket.send(&message(RTM_GETROUTE, NLM_F_REQUEST | flags, payload)) {
            return Vec::new();
        }
        let mut out = Vec::new();
        let mut buf = [0u8; 8192];
        for _ in 0..MAX_READS {
            let Some(read) = socket.recv(&mut buf) else {
                break;
            };
            out.extend_from_slice(&buf[..read]);
            if !dump || terminated(&buf[..read]) {
                break;
            }
        }
        out
    }

    /// Does this buffer carry the end of a dump — either the terminator or an error?
    fn terminated(buf: &[u8]) -> bool {
        let mut rest = buf;
        while let Some((kind, _body, len)) = next_message(rest) {
            if kind == NLMSG_DONE || kind == NLMSG_ERROR {
                return true;
            }
            match rest.get(align4(len)..) {
                Some(next) => rest = next,
                None => break,
            }
        }
        false
    }

    /// Walk a netlink buffer, appending the output interface of every route it describes.
    ///
    /// `only_default` keeps routes whose destination prefix is empty, which is what a dump has to
    /// filter for; a route get's reply carries the destination that was asked about, so there it is
    /// off. Split from the transport so the whole decode is testable against synthetic messages.
    pub(super) fn collect_oifs(buf: &[u8], only_default: bool, out: &mut Vec<u32>) {
        let mut rest = buf;
        while let Some((kind, body, len)) = next_message(rest) {
            if kind == NLMSG_DONE || kind == NLMSG_ERROR {
                return;
            }
            if kind == RTM_NEWROUTE
                && let Some(oif) = route_oif(body, only_default)
                && !out.contains(&oif)
            {
                out.push(oif);
            }
            match rest.get(align4(len)..) {
                Some(next) => rest = next,
                None => return,
            }
        }
    }

    /// The next `(type, body, length)` in a netlink buffer, or `None` when what is left is not a
    /// whole message — a truncated tail is the end of the walk, not a decode failure.
    fn next_message(buf: &[u8]) -> Option<(u16, &[u8], usize)> {
        if buf.len() < NLMSG_HEADER {
            return None;
        }
        let len = u32::from_ne_bytes(buf[0..4].try_into().ok()?) as usize;
        if len < NLMSG_HEADER || len > buf.len() {
            return None;
        }
        let kind = u16::from_ne_bytes(buf[4..6].try_into().ok()?);
        Some((kind, &buf[NLMSG_HEADER..len], len))
    }

    /// The `RTA_OIF` of one `RTM_NEWROUTE` body, when it is a route worth taking an interface from.
    fn route_oif(body: &[u8], only_default: bool) -> Option<u32> {
        if body.len() < RTMSG {
            return None;
        }
        // `struct rtmsg`: family, dst_len, src_len, tos, table, protocol, scope, type, flags.
        let dst_len = body[1];
        let route_type = body[7];
        if route_type != RTN_UNICAST || (only_default && dst_len != 0) {
            return None;
        }
        let mut rest = &body[RTMSG..];
        while rest.len() >= RTA_HEADER {
            let len = u16::from_ne_bytes(rest[0..2].try_into().ok()?) as usize;
            let kind = u16::from_ne_bytes(rest[2..4].try_into().ok()?);
            if len < RTA_HEADER || len > rest.len() {
                return None;
            }
            if kind == RTA_OIF && len >= RTA_HEADER + 4 {
                return Some(u32::from_ne_bytes(rest[4..8].try_into().ok()?));
            }
            rest = rest.get(align4(len)..)?;
        }
        None
    }

    /// A netlink message: the header, then the payload.
    fn message(kind: u16, flags: u16, payload: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(NLMSG_HEADER + payload.len());
        out.extend_from_slice(&((NLMSG_HEADER + payload.len()) as u32).to_ne_bytes());
        out.extend_from_slice(&kind.to_ne_bytes());
        out.extend_from_slice(&flags.to_ne_bytes());
        // Sequence and port: one exchange per socket, so neither has to be tracked. A zero port
        // asks the kernel to assign one on the first send.
        out.extend_from_slice(&1u32.to_ne_bytes());
        out.extend_from_slice(&0u32.to_ne_bytes());
        out.extend_from_slice(payload);
        out
    }

    /// An `RTM_GETROUTE` body asking "how would this destination be reached?".
    fn route_get_payload(dst: IpAddr) -> Vec<u8> {
        let (family, addr): (u8, Vec<u8>) = match dst {
            IpAddr::V4(v4) => (libc::AF_INET as u8, v4.octets().to_vec()),
            IpAddr::V6(v6) => (libc::AF_INET6 as u8, v6.octets().to_vec()),
        };
        let mut out = vec![0u8; RTMSG];
        out[0] = family;
        // A full-width prefix: this asks about one address, not about a network.
        out[1] = (addr.len() * 8) as u8;
        attribute(&mut out, RTA_DST, &addr);
        out
    }

    /// An `RTM_GETROUTE` body asking for every route of every family.
    fn dump_payload() -> Vec<u8> {
        let mut out = vec![0u8; RTMSG];
        out[0] = libc::AF_UNSPEC as u8;
        out
    }

    /// Append one `struct rtattr` and its payload, padded to the next 4-byte boundary.
    fn attribute(out: &mut Vec<u8>, kind: u16, data: &[u8]) {
        let len = RTA_HEADER + data.len();
        out.extend_from_slice(&(len as u16).to_ne_bytes());
        out.extend_from_slice(&kind.to_ne_bytes());
        out.extend_from_slice(data);
        out.resize(out.len() + (align4(len) - len), 0);
    }

    /// `NLMSG_ALIGN`/`RTA_ALIGN`: everything in a netlink message starts on a 4-byte boundary.
    fn align4(len: usize) -> usize {
        len.div_ceil(4) * 4
    }

    /// The name of an interface index, if the kernel still has one by that number.
    fn iface_name(index: u32) -> Option<String> {
        let mut buf = [0 as libc::c_char; libc::IF_NAMESIZE];
        // SAFETY: `if_indextoname` writes at most `IF_NAMESIZE` bytes including the terminator into
        // the buffer, which is exactly that large, and returns null rather than writing on failure.
        // The `CStr` is built from the same buffer while it is still in scope and copied out of.
        unsafe {
            if libc::if_indextoname(index, buf.as_mut_ptr()).is_null() {
                return None;
            }
            std::ffi::CStr::from_ptr(buf.as_ptr())
                .to_str()
                .ok()
                .map(str::to_owned)
        }
    }

    /// An owned `NETLINK_ROUTE` socket.
    struct Socket(libc::c_int);

    impl Socket {
        /// Open one, with a receive timeout so no call here can park the discovery thread.
        ///
        /// A failure is not reported: a kernel or a sandbox without netlink is a host where this
        /// feature simply does not apply, and discovery runs on a timer — warning would mean one
        /// line every refresh interval, for ever (the same reasoning as the Engine API probe).
        fn open() -> Option<Socket> {
            // SAFETY: a plain socket(2) with constant arguments; the returned descriptor is owned by
            // the `Socket` below, which closes it exactly once.
            let fd = unsafe {
                libc::socket(
                    libc::AF_NETLINK,
                    libc::SOCK_RAW | libc::SOCK_CLOEXEC,
                    libc::NETLINK_ROUTE,
                )
            };
            if fd < 0 {
                return None;
            }
            let socket = Socket(fd);
            let timeout = libc::timeval {
                tv_sec: TIMEOUT_SECS,
                tv_usec: 0,
            };
            // SAFETY: `timeout` is a live `timeval` for the duration of the call and its size is
            // what `SO_RCVTIMEO` expects. A failure leaves the default (blocking) timeout, which the
            // read cap still bounds.
            unsafe {
                libc::setsockopt(
                    socket.0,
                    libc::SOL_SOCKET,
                    libc::SO_RCVTIMEO,
                    std::ptr::from_ref(&timeout).cast(),
                    std::mem::size_of::<libc::timeval>() as libc::socklen_t,
                );
            }
            Some(socket)
        }

        /// Send one whole message to the kernel. A short write is a failure: a partial netlink
        /// message is not a message.
        fn send(&self, message: &[u8]) -> bool {
            // SAFETY: `message` is a live slice for the duration of the call, and its length is what
            // is passed as the count.
            let sent = unsafe { libc::send(self.0, message.as_ptr().cast(), message.len(), 0) };
            sent == message.len() as isize
        }

        /// Read one datagram, or `None` on timeout, error, or an orderly end.
        fn recv(&self, buf: &mut [u8]) -> Option<usize> {
            // SAFETY: `buf` is a live, exclusively-borrowed slice, and its length bounds the write.
            let read = unsafe { libc::recv(self.0, buf.as_mut_ptr().cast(), buf.len(), 0) };
            (read > 0).then_some(read as usize)
        }
    }

    impl Drop for Socket {
        fn drop(&mut self) {
            // SAFETY: this type owns the descriptor and is the only thing that closes it.
            unsafe { libc::close(self.0) };
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// Build an `RTM_NEWROUTE` the way the kernel frames one, so the decoder is tested against
        /// the layout it will actually meet rather than against itself.
        fn route(dst_len: u8, route_type: u8, oif: Option<u32>) -> Vec<u8> {
            let mut body = vec![0u8; RTMSG];
            body[0] = libc::AF_INET as u8;
            body[1] = dst_len;
            body[7] = route_type;
            // An attribute the decoder must step over to reach the one it wants.
            attribute(&mut body, RTA_DST, &[192, 0, 2, 1]);
            if let Some(oif) = oif {
                attribute(&mut body, RTA_OIF, &oif.to_ne_bytes());
            }
            message(RTM_NEWROUTE, 0, &body)
        }

        fn oifs(buf: &[u8], only_default: bool) -> Vec<u32> {
            let mut out = Vec::new();
            collect_oifs(buf, only_default, &mut out);
            out
        }

        #[test]
        fn a_route_get_reply_yields_its_output_interface() {
            // The reply to `ip route get 192.0.2.1` carries the *queried* prefix length, so the
            // default-only filter must be off for it — with it on, the answer would be dropped.
            let reply = route(32, RTN_UNICAST, Some(7));
            assert_eq!(oifs(&reply, false), vec![7]);
            assert!(oifs(&reply, true).is_empty());
        }

        #[test]
        fn a_dump_keeps_default_routes_and_nothing_else() {
            let mut dump = route(24, RTN_UNICAST, Some(2)); // an on-link subnet
            dump.extend(route(0, RTN_UNICAST, Some(3))); // the default route
            dump.extend(route(0, RTN_UNICAST, Some(3))); // a second table's, same interface
            dump.extend(route(0, RTN_UNICAST, Some(4))); // ...and one via another interface
            assert_eq!(oifs(&dump, true), vec![3, 4], "deduplicated, in order");
        }

        #[test]
        fn a_blackholed_default_route_names_no_interface() {
            // `RTN_BLACKHOLE` is 6, `RTN_UNREACHABLE` 7: routes that go nowhere, and whose
            // interface — if they even carry one — is not somewhere to bind.
            for route_type in [6u8, 7] {
                assert!(oifs(&route(0, route_type, Some(9)), true).is_empty());
            }
            // ...and a route with no RTA_OIF at all is simply not an answer.
            assert!(oifs(&route(0, RTN_UNICAST, None), true).is_empty());
        }

        #[test]
        fn the_walk_stops_at_the_dump_terminator_and_at_an_error() {
            for end in [NLMSG_DONE, NLMSG_ERROR] {
                let mut buf = route(0, RTN_UNICAST, Some(3));
                buf.extend(message(end, 0, &[0u8; 4]));
                buf.extend(route(0, RTN_UNICAST, Some(4)));
                assert_eq!(oifs(&buf, true), vec![3], "nothing past the terminator");
                assert!(terminated(&buf));
            }
            assert!(!terminated(&route(0, RTN_UNICAST, Some(3))));
        }

        /// Netlink comes off a socket, so every prefix of a message is a buffer this may see. None
        /// of them may panic, and none may invent a route.
        #[test]
        fn a_truncated_or_malformed_buffer_is_refused_rather_than_half_decoded() {
            let whole = route(0, RTN_UNICAST, Some(3));
            for cut in 0..whole.len() {
                let _ = oifs(&whole[..cut], true);
                let _ = terminated(&whole[..cut]);
            }
            // A length field that lies about the message it heads.
            let mut lying = whole.clone();
            lying[0..4].copy_from_slice(&4u32.to_ne_bytes());
            assert!(oifs(&lying, true).is_empty());
            // ...and an attribute whose length runs past the message.
            let mut overrun = whole.clone();
            let attrs = NLMSG_HEADER + RTMSG;
            overrun[attrs..attrs + 2].copy_from_slice(&9999u16.to_ne_bytes());
            assert!(oifs(&overrun, true).is_empty());
            assert!(oifs(&[], true).is_empty());
            assert!(oifs(&[0u8; 3], true).is_empty());
        }

        #[test]
        fn requests_are_framed_the_way_the_kernel_expects() {
            let request = message(
                RTM_GETROUTE,
                NLM_F_REQUEST,
                &route_get_payload(PROBE_V4.into()),
            );
            assert_eq!(
                u32::from_ne_bytes(request[0..4].try_into().unwrap()) as usize,
                request.len(),
                "nlmsg_len must cover the whole message"
            );
            assert_eq!(
                u16::from_ne_bytes(request[4..6].try_into().unwrap()),
                RTM_GETROUTE
            );
            let body = &request[NLMSG_HEADER..];
            assert_eq!(body[0], libc::AF_INET as u8);
            assert_eq!(body[1], 32, "a host route, not a network");
            assert_eq!(body.len() % 4, 0, "everything is 4-byte aligned");
            // The v6 probe is the same shape with a wider address.
            let v6 = route_get_payload(PROBE_V6.into());
            assert_eq!(v6[0], libc::AF_INET6 as u8);
            assert_eq!(v6[1], 128);
            assert_eq!(dump_payload()[0], libc::AF_UNSPEC as u8);
        }

        #[test]
        fn attributes_are_padded_to_the_alignment() {
            let mut out = Vec::new();
            // 5 bytes of payload: the attribute is 9 long and occupies 12.
            attribute(&mut out, RTA_DST, &[1, 2, 3, 4, 5]);
            assert_eq!(u16::from_ne_bytes(out[0..2].try_into().unwrap()), 9);
            assert_eq!(out.len(), 12);
            assert_eq!((align4(0), align4(1), align4(4), align4(5)), (0, 4, 4, 8));
        }

        /// The real kernel, on whatever host runs the tests. There is no fixed answer to assert —
        /// a build sandbox may have no default route at all — so this asserts what must hold either
        /// way: it does not panic, it does not hang, and every name it returns is a real interface.
        ///
        /// Both queries are exercised, because only the first one runs when the host answers: a
        /// dump that never terminated would otherwise show up as a wedged refresh in production
        /// rather than as a failing test here.
        #[test]
        fn asking_the_real_kernel_is_safe_and_self_consistent() {
            let ifaddrs = crate::docker::networks::ifaddrs();
            let real = |iface: &String| ifaddrs.iter().any(|(name, _, _)| name == iface);

            for iface in &default_route_ifaces() {
                assert!(real(iface), "{iface} is not an interface this host has");
            }

            let mut dumped = Vec::new();
            collect_oifs(&query(&dump_payload(), NLM_F_DUMP, true), true, &mut dumped);
            for iface in dumped.iter().filter_map(|index| iface_name(*index)) {
                assert!(real(&iface), "{iface} is not an interface this host has");
            }
        }
    }
}

/// Netlink is Linux. Everywhere else there is no Docker Desktop VM to be inside of either, so the
/// answer is "no interface", and the whole exchange above compiles out.
#[cfg(not(target_os = "linux"))]
mod netlink {
    pub(super) fn default_route_ifaces() -> Vec<String> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().expect("an IP address")
    }

    fn markers(kernel: &str, hostname: &str, daemon_os: &str) -> Markers {
        Markers {
            kernel: kernel.to_owned(),
            hostname: hostname.to_owned(),
            daemon_os: daemon_os.to_owned(),
        }
    }

    /// The three shapes a Docker Desktop VM shows up as, measured strings rather than paraphrases.
    #[test]
    fn a_docker_desktop_vm_is_recognised_by_any_of_its_three_markers() {
        // macOS / Hyper-V backend: a LinuxKit kernel.
        assert!(is_docker_desktop(&markers(
            "Linux version 6.10.14-linuxkit (root@buildkitsandbox) #1 SMP Sun Jan 1 00:00:00 UTC 2026",
            "abc123def456",
            "",
        )));
        // WSL 2 backend: an ordinary WSL kernel, but the distro is named.
        assert!(is_docker_desktop(&markers(
            "Linux version 5.15.167.4-microsoft-standard-WSL2 (root@build) #1 SMP",
            "docker-desktop\n",
            "",
        )));
        // Whatever the kernel says, the daemon knows what it is.
        assert!(is_docker_desktop(&markers(
            "Linux version 6.17.0-1021-nvidia",
            "workstation",
            "Docker Desktop",
        )));
    }

    /// The expensive direction: a plain Linux host must never be taken for a VM, because there the
    /// default-route interface is the LAN and `/admin/*` is unauthenticated.
    #[test]
    fn an_ordinary_linux_host_is_not_a_docker_desktop_vm() {
        let host = markers(
            "Linux version 6.17.0-1021-nvidia (buildd@lcy02) #21-Ubuntu SMP",
            "workstation",
            "Ubuntu 24.04.3 LTS",
        );
        assert!(!is_docker_desktop(&host));
        assert!(!is_docker_desktop(&Markers::default()));
        // ...and a host whose *hostname* merely mentions docker is not one either.
        assert!(!is_docker_desktop(&markers("", "docker-desktop-build", "")));
    }

    #[test]
    fn the_setting_decides_whether_detection_is_even_consulted() {
        // `off` never looks; `on` never asks. Neither touches the kernel, so neither can be wrong
        // about a host imbh does not recognise.
        assert!(!wanted(VmNet::Off, "Docker Desktop"));
        assert!(wanted(VmNet::On, "Ubuntu 24.04.3 LTS"));
        assert!(wanted(VmNet::Auto, "Docker Desktop"));
        assert!(addresses(VmNet::Off, "Docker Desktop").is_empty());
    }

    #[test]
    fn the_vm_net_setting_parses() {
        assert_eq!(VmNet::parse(""), Ok(VmNet::Auto));
        assert_eq!(VmNet::parse(" auto "), Ok(VmNet::Auto));
        assert_eq!(VmNet::parse("on"), Ok(VmNet::On));
        assert_eq!(VmNet::parse("true"), Ok(VmNet::On));
        assert_eq!(VmNet::parse("off"), Ok(VmNet::Off));
        assert_eq!(VmNet::parse("false"), Ok(VmNet::Off));
        assert_eq!(VmNet::default(), VmNet::Auto);
        // A typo is refused rather than silently meaning `off`.
        assert!(VmNet::parse("yes").is_err());
        assert!(VmNet::parse("1").is_err());
    }

    /// The selection: only the default-route interface, and only addresses that can be bound.
    #[test]
    fn only_the_default_route_interface_contributes_addresses() {
        let ifaddrs = vec![
            ("lo".to_owned(), ip("127.0.0.1"), ip("255.0.0.0")),
            // The Docker Desktop VM's uplink, as it looks inside the VM.
            ("eth0".to_owned(), ip("192.168.65.3"), ip("255.255.255.0")),
            // ...carrying an IPv6 link-local, which `bind(2)` refuses without a scope.
            (
                "eth0".to_owned(),
                ip("fe80::5054:ff:fe12:3456"),
                ip("ffff:ffff:ffff:ffff::"),
            ),
            // A bridge gateway: already discovered as one, and not on the default route.
            ("docker0".to_owned(), ip("172.17.0.1"), ip("255.255.0.0")),
            // Another interface entirely, which the default route does not leave by.
            ("eth1".to_owned(), ip("10.4.0.9"), ip("255.255.255.0")),
        ];
        let ifaces = vec!["eth0".to_owned()];
        assert_eq!(
            on_default_route(&ifaces, &ifaddrs),
            vec![ip("192.168.65.3")]
        );

        // A dual-stack uplink offers both, in the order the netns reports them.
        let dual = vec![
            ("eth0".to_owned(), ip("192.168.65.3"), ip("255.255.255.0")),
            (
                "eth0".to_owned(),
                ip("fd00:65::3"),
                ip("ffff:ffff:ffff:ffff::"),
            ),
        ];
        assert_eq!(
            on_default_route(&ifaces, &dual),
            vec![ip("192.168.65.3"), ip("fd00:65::3")]
        );

        // No default route, or none of its interfaces carrying an address, is simply nothing to add.
        assert!(on_default_route(&[], &ifaddrs).is_empty());
        assert!(on_default_route(&["eth9".to_owned()], &ifaddrs).is_empty());
    }

    /// The same address reported twice by `getifaddrs` must not become two listen addresses.
    #[test]
    fn duplicate_addresses_collapse() {
        let ifaddrs = vec![
            ("eth0".to_owned(), ip("192.168.65.3"), ip("255.255.255.0")),
            ("eth0".to_owned(), ip("192.168.65.3"), ip("255.255.255.0")),
        ];
        assert_eq!(
            on_default_route(&["eth0".to_owned()], &ifaddrs),
            vec![ip("192.168.65.3")]
        );
    }
}

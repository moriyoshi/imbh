//! Addressing primitives for the discovered-network listeners: CIDR matching, the accept-side
//! allow-list, and the bind specification that lets a listen address be *discovered* at run time
//! rather than configured up front.
//!
//! Nothing in this file knows about Docker in particular — the one seam is [`Discovery`], which
//! [`super::networks::Networks`] implements. It lives under the `docker` module all the same,
//! because everything that *drives* it does: the default `imbhd` build has no source of runtime
//! addresses and no reason to carry the machinery for using one (ARCHITECTURE.md §11 — the
//! footprint gate builds this crate with default features).
//!
//! No new crate: everything below is `std::net` plus string parsing. A CIDR type is ~50 lines and
//! an `ipnet` dependency is 1 crate more than this workspace is willing to spend.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// An IP network — an address and a prefix length, e.g. `172.17.0.0/16`.
///
/// A bare address parses as a host route (`/32`, or `/128` for IPv6), so an operator can write
/// `IMBH_ALLOW_FROM=10.1.2.3` and mean that one peer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cidr {
    addr: IpAddr,
    prefix: u8,
}

impl Cidr {
    /// Build a network, clamping `prefix` to the address family's width.
    pub fn new(addr: IpAddr, prefix: u8) -> Cidr {
        let width = match addr {
            IpAddr::V4(_) => 32,
            IpAddr::V6(_) => 128,
        };
        Cidr {
            addr,
            prefix: prefix.min(width),
        }
    }

    /// The network an interface address and netmask describe — how a bridge's subnet is recovered
    /// from `getifaddrs`, which reports a mask rather than a prefix length.
    ///
    /// A non-contiguous mask cannot come from Docker (or from any IPAM driver), so the leading-ones
    /// count is taken as the prefix without validating the rest.
    pub fn from_mask(addr: IpAddr, mask: IpAddr) -> Cidr {
        let prefix = match mask {
            IpAddr::V4(m) => leading_ones(&m.octets()),
            IpAddr::V6(m) => leading_ones(&m.octets()),
        };
        Cidr::new(addr, prefix)
    }

    /// Parse `ADDR/PREFIX`, or a bare `ADDR` as a host route.
    pub fn parse(value: &str) -> Result<Cidr, String> {
        let value = value.trim();
        let (addr, prefix) = match value.split_once('/') {
            Some((addr, prefix)) => {
                let prefix: u8 = prefix
                    .trim()
                    .parse()
                    .map_err(|_| format!("{value}: prefix length is not a number"))?;
                (addr.trim(), Some(prefix))
            }
            None => (value, None),
        };
        let addr: IpAddr = addr
            .parse()
            .map_err(|_| format!("{value}: not an IP address"))?;
        let width = match addr {
            IpAddr::V4(_) => 32,
            IpAddr::V6(_) => 128,
        };
        let prefix = prefix.unwrap_or(width);
        if prefix > width {
            return Err(format!("{value}: prefix length is wider than the address"));
        }
        Ok(Cidr { addr, prefix })
    }

    /// The network address with the host bits cleared, so two `Cidr`s describing the same network
    /// compare equal however they were written.
    pub fn normalized(&self) -> Cidr {
        let addr = match self.addr {
            IpAddr::V4(a) => {
                let mut octets = a.octets();
                clear_host_bits(&mut octets, self.prefix);
                IpAddr::V4(Ipv4Addr::from(octets))
            }
            IpAddr::V6(a) => {
                let mut octets = a.octets();
                clear_host_bits(&mut octets, self.prefix);
                IpAddr::V6(Ipv6Addr::from(octets))
            }
        };
        Cidr {
            addr,
            prefix: self.prefix,
        }
    }

    /// Does this network contain `ip`?
    pub fn contains(&self, ip: IpAddr) -> bool {
        match (self.addr, ip) {
            (IpAddr::V4(net), IpAddr::V4(ip)) => {
                prefix_eq(&net.octets(), &ip.octets(), self.prefix)
            }
            (IpAddr::V6(net), IpAddr::V6(ip)) => {
                prefix_eq(&net.octets(), &ip.octets(), self.prefix)
            }
            // A dual-stack listener reports an IPv4 peer as `::ffff:a.b.c.d`, so an IPv4 rule has to
            // see through the mapping or it would never match a real connection.
            (IpAddr::V4(_), IpAddr::V6(ip)) => ip
                .to_ipv4_mapped()
                .is_some_and(|v4| self.contains(IpAddr::V4(v4))),
            (IpAddr::V6(_), IpAddr::V4(_)) => false,
        }
    }
}

impl std::fmt::Display for Cidr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.addr, self.prefix)
    }
}

/// Leading one-bits of a netmask.
fn leading_ones(mask: &[u8]) -> u8 {
    let mut bits = 0u8;
    for byte in mask {
        let ones = byte.leading_ones() as u8;
        bits += ones;
        if ones != 8 {
            break;
        }
    }
    bits
}

/// Do `a` and `b` agree on their first `prefix` bits?
fn prefix_eq(a: &[u8], b: &[u8], prefix: u8) -> bool {
    let whole = (prefix / 8) as usize;
    if a[..whole] != b[..whole] {
        return false;
    }
    match prefix % 8 {
        0 => true,
        bits => {
            let mask = 0xffu8 << (8 - bits);
            a[whole] & mask == b[whole] & mask
        }
    }
}

fn clear_host_bits(octets: &mut [u8], prefix: u8) {
    let whole = (prefix / 8) as usize;
    if let Some(partial) = octets.get_mut(whole) {
        *partial &= match prefix % 8 {
            0 => 0,
            bits => 0xffu8 << (8 - bits),
        };
    }
    for byte in octets.iter_mut().skip(whole + 1) {
        *byte = 0;
    }
}

// ── the accept-side allow-list ───────────────────────────────────────────────────────────

/// Loopback, always allowed by the `docker` token: an operator on the host has to be able to reach
/// the query endpoint, and a listener bound to a bridge gateway is not reachable *from* loopback
/// anyway — so this costs nothing and saves a class of self-inflicted lockout.
const LOOPBACK: [&str; 2] = ["127.0.0.0/8", "::1/128"];

/// What `IMBH_ALLOW_FROM` asked for, before the Docker subnets are known.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum AllowFrom {
    /// No filtering: every peer that can reach the socket is served. The default, so an existing
    /// deployment's behaviour is unchanged by this feature existing.
    #[default]
    Any,
    /// Only these networks, plus the daemon's bridge subnets when `docker` is `true`.
    List { docker: bool, nets: Vec<Cidr> },
}

impl AllowFrom {
    /// Parse the `IMBH_ALLOW_FROM` grammar: `any`, or a comma-separated list of CIDRs in which the
    /// word `docker` stands for "every bridge subnet this daemon has, plus loopback".
    pub fn parse(value: &str) -> Result<AllowFrom, String> {
        let value = value.trim();
        if value.is_empty() || value.eq_ignore_ascii_case("any") || value == "*" {
            return Ok(AllowFrom::Any);
        }
        let mut docker = false;
        let mut nets = Vec::new();
        for token in value.split(',') {
            let token = token.trim();
            if token.is_empty() {
                continue;
            }
            if token.eq_ignore_ascii_case("docker") {
                docker = true;
                continue;
            }
            nets.push(Cidr::parse(token)?);
        }
        if !docker && nets.is_empty() {
            return Err(format!("{value}: names no network to allow"));
        }
        Ok(AllowFrom::List { docker, nets })
    }

    /// Resolve against the currently known bridge subnets. `None` means "allow everything" — the
    /// cheap representation of [`AllowFrom::Any`], so the accept path has nothing to check.
    pub fn resolve(&self, subnets: &[Cidr]) -> Option<AllowList> {
        match self {
            AllowFrom::Any => None,
            AllowFrom::List { docker, nets } => {
                let mut all = nets.clone();
                if *docker {
                    all.extend(subnets.iter().copied());
                    all.extend(LOOPBACK.iter().filter_map(|c| Cidr::parse(c).ok()));
                }
                Some(AllowList { nets: all })
            }
        }
    }

    /// Whether resolution depends on discovery, and so has to be re-run when the snapshot changes.
    pub fn needs_discovery(&self) -> bool {
        matches!(self, AllowFrom::List { docker: true, .. })
    }
}

/// A resolved allow-list.
///
/// An **empty** list denies everything rather than allowing everything: it means an operator asked
/// for `docker` on a daemon with no bridge networks, and failing closed is the only safe reading of
/// a security control whose input went missing. The listener warns about it on every refresh.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AllowList {
    nets: Vec<Cidr>,
}

impl AllowList {
    pub fn new(nets: Vec<Cidr>) -> AllowList {
        AllowList { nets }
    }

    pub fn allows(&self, ip: IpAddr) -> bool {
        self.nets.iter().any(|net| net.contains(ip))
    }

    pub fn is_empty(&self) -> bool {
        self.nets.is_empty()
    }

    pub fn nets(&self) -> &[Cidr] {
        &self.nets
    }
}

/// The live allow-list every listener consults on accept, and the rule that produced it.
///
/// One instance is shared by the HTTP and gRPC listeners; discovery swaps the resolved list under
/// them with [`Access::refresh`] as bridge networks come and go. The common configuration is
/// [`AllowFrom::Any`], which resolves to `None` and makes [`Access::allows`] a single read of an
/// uncontended lock returning `true`.
pub struct Access {
    from: AllowFrom,
    list: std::sync::RwLock<Option<AllowList>>,
}

impl Access {
    /// Build from a parsed rule and the subnets known so far.
    pub fn new(from: AllowFrom, subnets: &[Cidr]) -> std::sync::Arc<Access> {
        let list = from.resolve(subnets);
        std::sync::Arc::new(Access {
            from,
            list: std::sync::RwLock::new(list),
        })
    }

    /// The permissive default: no filtering at all.
    pub fn unrestricted() -> std::sync::Arc<Access> {
        Access::new(AllowFrom::Any, &[])
    }

    /// Re-resolve against a new set of bridge subnets. A no-op unless the rule mentions `docker`.
    pub fn refresh(&self, subnets: &[Cidr]) {
        if !self.from.needs_discovery() {
            return;
        }
        let next = self.from.resolve(subnets);
        *self
            .list
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = next;
    }

    /// May this peer be served?
    pub fn allows(&self, ip: IpAddr) -> bool {
        match &*self
            .list
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
        {
            None => true,
            Some(list) => list.allows(ip),
        }
    }

    /// Whether anything is being filtered at all — used to keep the accept path's cost visible and
    /// to decide whether a startup banner has anything to say.
    pub fn is_filtering(&self) -> bool {
        !matches!(self.from, AllowFrom::Any)
    }

    /// This access rule as the accept loop's peer predicate, or `None` when nothing is filtered —
    /// which is what keeps an unfiltered listener's accept path exactly what it was before this
    /// module existed, down to the absent branch.
    ///
    /// The refusal warning lives inside the closure rather than in the accept loop, so `lib.rs`
    /// carries no part of this feature beyond the `Option` it checks.
    pub fn filter(self: &std::sync::Arc<Self>) -> Option<crate::PeerFilter> {
        if !self.is_filtering() {
            return None;
        }
        let access = std::sync::Arc::clone(self);
        Some(std::sync::Arc::new(move |ip: IpAddr| {
            let allowed = access.allows(ip);
            if !allowed {
                refused(ip);
            }
            allowed
        }))
    }

    /// How the current list reads, for a banner or a warning.
    pub fn describe(&self) -> String {
        match &*self
            .list
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
        {
            None => "any".to_owned(),
            Some(list) if list.is_empty() => "nothing (no networks resolved)".to_owned(),
            Some(list) => list
                .nets()
                .iter()
                .map(|net| net.to_string())
                .collect::<Vec<_>>()
                .join(", "),
        }
    }
}

/// Quietest useful cadence for allow-list refusals: the first one, then at most one a minute
/// carrying the count since. A scanner sweeping a bridge subnet would otherwise write one line per
/// probe — the same reasoning as the remap engine's runtime-failure reporting.
const REFUSAL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60);

/// Report a connection the allow-list turned away, rate-limited.
fn refused(ip: IpAddr) {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SINCE: AtomicU64 = AtomicU64::new(0);
    static LAST: std::sync::Mutex<Option<std::time::Instant>> = std::sync::Mutex::new(None);

    let count = SINCE.fetch_add(1, Ordering::Relaxed) + 1;
    let now = std::time::Instant::now();
    let mut last = LAST
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if last.is_some_and(|last| now.duration_since(last) < REFUSAL_INTERVAL) {
        return;
    }
    *last = Some(now);
    drop(last);
    SINCE.store(0, Ordering::Relaxed);
    super::warn(&format!(
        "refused {ip}: outside IMBH_ALLOW_FROM ({count} connection(s) since the last report)"
    ));
}

// ── the bind specification ───────────────────────────────────────────────────────────────

/// The sentinel that stands for "every bridge gateway this daemon has".
const AUTO: &str = "auto";

/// Where a listener should bind: some fixed addresses, and some ports to open on every address
/// discovery reports.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BindSpec {
    /// Literal `host:port` entries, bound exactly as written — the pre-existing behaviour, and
    /// still what a single unadorned address means.
    pub literals: Vec<String>,
    /// Ports to bind on every discovered gateway.
    pub auto_ports: Vec<u16>,
}

impl BindSpec {
    /// Parse a comma-separated list whose elements are either a literal address or `auto[:PORT]`.
    ///
    /// `default_port` is what a bare `auto` means, so the HTTP and gRPC listeners can share the
    /// grammar while keeping their own default ports.
    pub fn parse(value: &str, default_port: u16) -> Result<BindSpec, String> {
        let mut spec = BindSpec::default();
        for token in value.split(',') {
            let token = token.trim();
            if token.is_empty() {
                continue;
            }
            let auto = match token.split_once(':') {
                Some((head, port)) if head.eq_ignore_ascii_case(AUTO) => Some(
                    port.trim()
                        .parse::<u16>()
                        .map_err(|_| format!("{token}: not a port number"))?,
                ),
                _ if token.eq_ignore_ascii_case(AUTO) => Some(default_port),
                _ => None,
            };
            match auto {
                Some(port) if !spec.auto_ports.contains(&port) => spec.auto_ports.push(port),
                Some(_) => {}
                None if !spec.literals.contains(&token.to_owned()) => {
                    spec.literals.push(token.to_owned())
                }
                None => {}
            }
        }
        Ok(spec)
    }

    /// Nothing to bind at all — the "do not listen" posture, which stays a supported configuration
    /// because the log-driver plugin needs no TCP port.
    pub fn is_empty(&self) -> bool {
        self.literals.is_empty() && self.auto_ports.is_empty()
    }

    /// Whether this spec has to be re-resolved when discovery finds different gateways.
    pub fn is_dynamic(&self) -> bool {
        !self.auto_ports.is_empty()
    }

    /// This spec as the operator wrote it, for a banner or a warning.
    pub fn describe(&self) -> String {
        self.literals
            .iter()
            .cloned()
            .chain(self.auto_ports.iter().map(|port| format!("{AUTO}:{port}")))
            .collect::<Vec<_>>()
            .join(",")
    }

    /// Was this address written out by the operator, rather than derived from discovery?
    ///
    /// The distinction is what a failed bind means. A literal that will not bind is a configuration
    /// error — a port already in use, an address this host does not have — and has always been
    /// fatal. A discovered one that will not bind is a bridge that went away between the scan and
    /// the `bind`, which is ordinary and worth only a warning and a retry on the next tick.
    pub fn is_required(&self, addr: &str) -> bool {
        self.literals.iter().any(|literal| literal == addr)
    }

    /// The addresses to bind right now, given the gateways discovery currently reports.
    ///
    /// Literals stay strings: `TcpListener::bind` resolves them, so a hostname keeps working the
    /// way it does today.
    pub fn resolve(&self, gateways: &[IpAddr]) -> Vec<String> {
        let mut out = self.literals.clone();
        for port in &self.auto_ports {
            for gateway in gateways {
                let addr = match gateway {
                    IpAddr::V4(v4) => format!("{v4}:{port}"),
                    IpAddr::V6(v6) => format!("[{v6}]:{port}"),
                };
                if !out.contains(&addr) {
                    out.push(addr);
                }
            }
        }
        out
    }
}

// ── the discovery seam ───────────────────────────────────────────────────────────────────

/// A source of runtime addressing facts. Implemented by the Docker bridge-network scanner; kept as
/// a trait so the listeners in `lib.rs`/`grpc.rs` compile without the optional `docker` feature.
pub trait Discovery: Send + Sync {
    /// Addresses `auto` currently resolves to.
    fn gateways(&self) -> Vec<IpAddr>;
    /// Networks the `docker` allow-list token currently expands to.
    fn subnets(&self) -> Vec<Cidr>;
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

    #[test]
    fn cidrs_match_on_their_prefix() {
        let net = cidr("172.17.0.0/16");
        assert!(net.contains(ip("172.17.0.1")));
        assert!(net.contains(ip("172.17.255.254")));
        assert!(!net.contains(ip("172.18.0.1")));

        // A prefix that does not land on a byte boundary is the interesting case.
        let net = cidr("10.1.2.0/23");
        assert!(net.contains(ip("10.1.2.255")));
        assert!(net.contains(ip("10.1.3.1")));
        assert!(!net.contains(ip("10.1.4.1")));
    }

    #[test]
    fn the_degenerate_prefixes_work() {
        assert!(cidr("0.0.0.0/0").contains(ip("8.8.8.8")));
        assert!(cidr("10.0.0.1/32").contains(ip("10.0.0.1")));
        assert!(!cidr("10.0.0.1/32").contains(ip("10.0.0.2")));
        // A bare address is a host route.
        assert_eq!(cidr("10.0.0.1"), cidr("10.0.0.1/32"));
        assert_eq!(cidr("fd00::1"), cidr("fd00::1/128"));
    }

    #[test]
    fn ipv6_matches_and_does_not_cross_families() {
        let net = cidr("fd00:dead:beef::/48");
        assert!(net.contains(ip("fd00:dead:beef::1")));
        assert!(!net.contains(ip("fd00:dead:bee0::1")));
        // An IPv6 rule never matches an IPv4 peer...
        assert!(!net.contains(ip("172.17.0.1")));
        // ...but an IPv4 rule must see through the v4-mapped form a dual-stack listener reports,
        // or an allow-list would silently reject every real connection.
        assert!(cidr("172.17.0.0/16").contains(ip("::ffff:172.17.0.2")));
        assert!(!cidr("172.17.0.0/16").contains(ip("::ffff:10.0.0.2")));
    }

    #[test]
    fn malformed_cidrs_are_rejected_with_the_text_that_failed() {
        for bad in ["", "nonsense", "172.17.0.0/", "172.17.0.0/33", "1.2.3.4/x"] {
            let error = Cidr::parse(bad).expect_err("must not parse");
            assert!(!error.is_empty(), "{bad}");
        }
    }

    #[test]
    fn a_netmask_becomes_a_prefix() {
        assert_eq!(
            Cidr::from_mask(ip("172.17.0.1"), ip("255.255.0.0")).normalized(),
            cidr("172.17.0.0/16")
        );
        assert_eq!(
            Cidr::from_mask(ip("10.1.2.1"), ip("255.255.254.0")).normalized(),
            cidr("10.1.2.0/23")
        );
        assert_eq!(
            Cidr::from_mask(ip("fd00::1"), ip("ffff:ffff:ffff::")).normalized(),
            cidr("fd00::/48")
        );
    }

    #[test]
    fn normalizing_clears_the_host_bits() {
        assert_eq!(cidr("172.17.3.9/16").normalized(), cidr("172.17.0.0/16"));
        assert_eq!(cidr("10.1.3.9/23").normalized(), cidr("10.1.2.0/23"));
        assert_eq!(cidr("1.2.3.4/0").normalized(), cidr("0.0.0.0/0"));
    }

    // ── allow-list ───────────────────────────────────────────────────────────────────────

    #[test]
    fn the_default_allow_from_filters_nothing() {
        for value in ["", "  ", "any", "ANY", "*"] {
            assert_eq!(AllowFrom::parse(value), Ok(AllowFrom::Any), "{value:?}");
        }
        // `Any` resolves to no list at all, so the accept path has nothing to check.
        assert!(AllowFrom::Any.resolve(&[]).is_none());
        assert!(!AllowFrom::Any.needs_discovery());
    }

    #[test]
    fn the_docker_token_expands_to_the_discovered_subnets_and_loopback() {
        let from = AllowFrom::parse("docker").expect("parses");
        assert!(from.needs_discovery());
        let list = from
            .resolve(&[cidr("172.17.0.0/16"), cidr("172.23.0.0/16")])
            .expect("a list");
        assert!(list.allows(ip("172.17.0.5")));
        assert!(list.allows(ip("172.23.9.9")));
        assert!(list.allows(ip("127.0.0.1")));
        assert!(list.allows(ip("::1")));
        assert!(!list.allows(ip("192.168.10.131")));
    }

    #[test]
    fn explicit_cidrs_mix_with_the_docker_token() {
        let from = AllowFrom::parse("docker, 10.0.0.0/8").expect("parses");
        let list = from.resolve(&[cidr("172.17.0.0/16")]).expect("a list");
        assert!(list.allows(ip("10.9.9.9")));
        assert!(list.allows(ip("172.17.0.2")));

        // Without the token, discovery is irrelevant and only the literals count.
        let fixed = AllowFrom::parse("10.0.0.0/8").expect("parses");
        assert!(!fixed.needs_discovery());
        let list = fixed.resolve(&[cidr("172.17.0.0/16")]).expect("a list");
        assert!(list.allows(ip("10.9.9.9")));
        assert!(!list.allows(ip("172.17.0.2")));
    }

    #[test]
    fn an_allow_list_that_resolved_to_nothing_denies_everything() {
        // `docker` on a daemon with no bridges. Failing closed is the only safe reading — the
        // alternative silently turns a security control into a no-op.
        let list = AllowFrom::parse("docker")
            .expect("parses")
            .resolve(&[])
            .expect("a list");
        assert!(!list.allows(ip("172.17.0.2")));
        // Loopback still gets in, so an operator on the host is never locked out by this.
        assert!(list.allows(ip("127.0.0.1")));

        assert!(AllowList::default().is_empty());
        assert!(!AllowList::default().allows(ip("127.0.0.1")));
    }

    #[test]
    fn an_allow_from_naming_nothing_is_an_error_rather_than_a_silent_deny_all() {
        assert!(AllowFrom::parse(",,").is_err());
        assert!(AllowFrom::parse("not-a-cidr").is_err());
    }

    // ── bind spec ────────────────────────────────────────────────────────────────────────

    fn spec(value: &str) -> BindSpec {
        BindSpec::parse(value, 4318).expect("parses")
    }

    #[test]
    fn a_literal_address_behaves_exactly_as_it_did_before() {
        let s = spec("172.17.0.1:4318");
        assert_eq!(s.literals, vec!["172.17.0.1:4318"]);
        assert!(!s.is_dynamic());
        assert_eq!(s.resolve(&[]), vec!["172.17.0.1:4318"]);
    }

    #[test]
    fn auto_resolves_to_every_gateway() {
        let s = spec("auto");
        assert_eq!(s.auto_ports, vec![4318]);
        assert!(s.is_dynamic());
        assert_eq!(
            s.resolve(&[ip("172.17.0.1"), ip("172.23.0.1")]),
            vec!["172.17.0.1:4318", "172.23.0.1:4318"]
        );
        // No gateways yet is not an error; it is simply nothing to bind.
        assert!(s.resolve(&[]).is_empty());
    }

    #[test]
    fn auto_takes_a_port_and_mixes_with_literals() {
        let s = spec("auto:9000, 127.0.0.1:4318");
        assert_eq!(s.auto_ports, vec![9000]);
        assert_eq!(s.literals, vec!["127.0.0.1:4318"]);
        assert_eq!(
            s.resolve(&[ip("172.17.0.1")]),
            vec!["127.0.0.1:4318", "172.17.0.1:9000"]
        );
    }

    #[test]
    fn an_ipv6_gateway_gets_bracketed() {
        assert_eq!(
            spec("auto").resolve(&[ip("fd00::1")]),
            vec!["[fd00::1]:4318"]
        );
    }

    #[test]
    fn duplicates_collapse_so_two_spellings_do_not_bind_twice() {
        let s = spec("auto, auto:4318, 127.0.0.1:1, 127.0.0.1:1");
        assert_eq!(s.auto_ports, vec![4318]);
        assert_eq!(s.literals, vec!["127.0.0.1:1"]);
        // The same address reached both ways is bound once.
        assert_eq!(
            spec("auto, 172.17.0.1:4318").resolve(&[ip("172.17.0.1")]),
            vec!["172.17.0.1:4318"]
        );
    }

    #[test]
    fn an_empty_spec_means_do_not_listen() {
        assert!(spec("").is_empty());
        assert!(spec("  ,  ").is_empty());
        assert!(!spec("auto").is_empty());
    }

    #[test]
    fn a_bad_auto_port_is_an_error_rather_than_a_hostname() {
        assert!(BindSpec::parse("auto:not-a-port", 4318).is_err());
        // ...but a real hostname with a port is still a literal.
        assert_eq!(spec("db.internal:4318").literals, vec!["db.internal:4318"]);
    }
}

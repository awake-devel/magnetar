//! Which addresses we may connect to when the target came from someone else (D112).
//!
//! Tracker URLs, relay URLs and DHT `nodes` reach us from Nostr events and DHT peers. A hostile
//! one can point at the home router (192.168.1.1), Postgres on 127.0.0.1, a tailnet service on
//! 100.64/10 or an IPv6 ULA host, directly or through a DNS name that resolves there. The check
//! runs on the resolved address, right before the connect, never only on the URL string.
//!
//! [`is_public`] is the one address policy: the DHT crawler's filter (`dht::addr`), tracker
//! scrapes, the liveness DHT walk, untrusted relay connects and the librqbit peer blocklist all
//! use it. [`AddrPolicy`] adds the only escape hatch, loopback for in-process test servers, which
//! no configuration can turn on.
//!
//! Public addresses that are our own are refused too ([`OwnAddrs`]): the line's WAN address (a
//! connect to it hairpins into the router and its port forwards) and the global IPv6 prefixes of
//! this host's interfaces (where the other LAN hosts live). A hostile tracker or relay sees our
//! source address on the first request and could publish it back.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::RwLock;
use std::time::{Duration, Instant};

use crate::error::{Error, Result};

/// IPv4 ranges never contacted (network, prefix length): "this network", RFC 1918, CGNAT and
/// Tailscale (100.64/10), loopback, link-local, IETF protocol assignments (192.0.0/24), the three
/// documentation nets, the deprecated 6to4 relay anycast (192.88.99/24), benchmarking
/// (198.18/15), multicast and reserved/broadcast (240/4).
pub const BLOCKED_V4: &[(Ipv4Addr, u8)] = &[
    (Ipv4Addr::new(0, 0, 0, 0), 8),
    (Ipv4Addr::new(10, 0, 0, 0), 8),
    (Ipv4Addr::new(100, 64, 0, 0), 10),
    (Ipv4Addr::new(127, 0, 0, 0), 8),
    (Ipv4Addr::new(169, 254, 0, 0), 16),
    (Ipv4Addr::new(172, 16, 0, 0), 12),
    (Ipv4Addr::new(192, 0, 0, 0), 24),
    (Ipv4Addr::new(192, 0, 2, 0), 24),
    (Ipv4Addr::new(192, 88, 99, 0), 24),
    (Ipv4Addr::new(192, 168, 0, 0), 16),
    (Ipv4Addr::new(198, 18, 0, 0), 15),
    (Ipv4Addr::new(198, 51, 100, 0), 24),
    (Ipv4Addr::new(203, 0, 113, 0), 24),
    (Ipv4Addr::new(224, 0, 0, 0), 4),
    (Ipv4Addr::new(240, 0, 0, 0), 4),
];

/// IPv6 ranges never contacted, whatever they embed: ::/8 (unspecified, loopback, the deprecated
/// IPv4-compatible form), the discard prefix, local-use NAT64 (64:ff9b:1::/48, the operator's
/// own network), Teredo, benchmarking, ORCHID, the documentation nets, ULA (fc00::/7, where
/// fd00::/8 home networks live), link-local, the deprecated site-local and multicast. IPv4-mapped
/// (::ffff:0:0/96), well-known NAT64 (64:ff9b::/96) and 6to4 (2002::/16) are judged by the IPv4
/// address inside them instead ([`embedded_v4`]).
pub const BLOCKED_V6: &[(Ipv6Addr, u8)] = &[
    (Ipv6Addr::new(0, 0, 0, 0, 0, 0, 0, 0), 8),
    (Ipv6Addr::new(0x0100, 0, 0, 0, 0, 0, 0, 0), 64),
    (Ipv6Addr::new(0x0064, 0xff9b, 1, 0, 0, 0, 0, 0), 48),
    (Ipv6Addr::new(0x2001, 0, 0, 0, 0, 0, 0, 0), 32),
    (Ipv6Addr::new(0x2001, 0x0002, 0, 0, 0, 0, 0, 0), 48),
    (Ipv6Addr::new(0x2001, 0x0010, 0, 0, 0, 0, 0, 0), 28),
    (Ipv6Addr::new(0x2001, 0x0db8, 0, 0, 0, 0, 0, 0), 32),
    (Ipv6Addr::new(0x3fff, 0, 0, 0, 0, 0, 0, 0), 20),
    (Ipv6Addr::new(0xfc00, 0, 0, 0, 0, 0, 0, 0), 7),
    (Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 0), 10),
    (Ipv6Addr::new(0xfec0, 0, 0, 0, 0, 0, 0, 0), 10),
    (Ipv6Addr::new(0xff00, 0, 0, 0, 0, 0, 0, 0), 8),
];

fn in_v4(ip: Ipv4Addr, net: Ipv4Addr, len: u8) -> bool {
    let mask = u32::MAX.checked_shl(32 - u32::from(len)).unwrap_or(0);
    u32::from(ip) & mask == u32::from(net)
}

fn in_v6(ip: Ipv6Addr, net: Ipv6Addr, len: u8) -> bool {
    let mask = u128::MAX.checked_shl(128 - u32::from(len)).unwrap_or(0);
    u128::from(ip) & mask == u128::from(net)
}

/// Whether `ip` is a globally routable unicast IPv4 address (not in [`BLOCKED_V4`]).
pub fn is_public_v4(ip: Ipv4Addr) -> bool {
    !BLOCKED_V4.iter().any(|&(net, len)| in_v4(ip, net, len))
}

/// The IPv4 address an IPv6 address carries to IPv4 hosts: IPv4-mapped, well-known NAT64
/// (64:ff9b::/96, RFC 6052) or 6to4 (2002:V4ADDR::/48, RFC 3056).
pub fn embedded_v4(ip: Ipv6Addr) -> Option<Ipv4Addr> {
    if let Some(v4) = ip.to_ipv4_mapped() {
        return Some(v4);
    }
    let s = ip.segments();
    let low = |a: u16, b: u16| Ipv4Addr::from((u32::from(a) << 16) | u32::from(b));
    if s[..6] == [0x0064, 0xff9b, 0, 0, 0, 0] {
        return Some(low(s[6], s[7]));
    }
    (s[0] == 0x2002).then(|| low(s[1], s[2]))
}

/// Whether `ip` is a globally routable unicast IPv6 address: not in [`BLOCKED_V6`], and any IPv4
/// address it embeds is public too.
pub fn is_public_v6(ip: Ipv6Addr) -> bool {
    if let Some(v4) = embedded_v4(ip) {
        return is_public_v4(v4);
    }
    !BLOCKED_V6.iter().any(|&(net, len)| in_v6(ip, net, len))
}

/// The address policy: true only for globally routable unicast addresses.
pub fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_public_v4(v4),
        IpAddr::V6(v6) => is_public_v6(v6),
    }
}

/// `net/len` contains `ip` (families must match).
fn net_contains(net: IpAddr, len: u8, ip: IpAddr) -> bool {
    match (net, ip) {
        (IpAddr::V4(n), IpAddr::V4(i)) => in_v4(i, n, len.min(32)),
        (IpAddr::V6(n), IpAddr::V6(i)) => in_v6(i, n, len.min(128)),
        _ => false,
    }
}

fn mask_net(ip: IpAddr, len: u8) -> IpAddr {
    match ip {
        IpAddr::V4(v4) => {
            let mask = u32::MAX
                .checked_shl(32 - u32::from(len.min(32)))
                .unwrap_or(0);
            IpAddr::V4(Ipv4Addr::from(u32::from(v4) & mask))
        }
        IpAddr::V6(v6) => {
            let mask = u128::MAX
                .checked_shl(128 - u32::from(len.min(128)))
                .unwrap_or(0);
            IpAddr::V6(Ipv6Addr::from(u128::from(v6) & mask))
        }
    }
}

/// Prefix denied around a public IPv6 address of ours: its /64, the on-link LAN of a home line
/// (an interface prefix wider than /64 is kept as is).
pub const OWN_V6_PREFIX: u8 = 64;

/// How long interface addresses are reused before `getifaddrs` is read again.
pub const IFACE_TTL: Duration = Duration::from_secs(60);

/// Learned addresses kept (newest win): the DHT vote and UPnP can each report a new WAN address.
const MAX_LEARNED: usize = 4;

/// The networks our own addresses stand for: an IPv4 address alone, an IPv6 address's
/// [`OWN_V6_PREFIX`] (or its interface prefix when wider). Non-public addresses are dropped:
/// the policy refuses them anyway, and loopback must stay reachable through the test seam.
/// `addrs` is (address, interface prefix length if known).
pub fn own_nets(addrs: impl IntoIterator<Item = (IpAddr, Option<u8>)>) -> Vec<(IpAddr, u8)> {
    let mut out: Vec<(IpAddr, u8)> = Vec::new();
    for (ip, len) in addrs {
        let ip = ip.to_canonical();
        if !is_public(ip) {
            continue;
        }
        let len = match ip {
            IpAddr::V4(_) => 32,
            IpAddr::V6(_) => len.unwrap_or(OWN_V6_PREFIX).min(OWN_V6_PREFIX),
        };
        let net = (mask_net(ip, len), len);
        if !out.contains(&net) {
            out.push(net);
        }
    }
    out
}

/// Parse `MAGNETAR_OWN_ADDRS` items: addresses and networks (`203.0.113.7`,
/// `2a01:db8:1::/56`). A bare IPv6 address stands for its [`OWN_V6_PREFIX`].
pub fn parse_nets<S: AsRef<str>>(items: &[S]) -> Result<Vec<(IpAddr, u8)>> {
    let mut out = Vec::new();
    for item in items {
        let item = item.as_ref().trim();
        if item.is_empty() {
            continue;
        }
        let bad = || Error::Invalid(format!("own address {item:?}: expected an IP or IP/len"));
        let (ip, len) = match item.split_once('/') {
            Some((ip, len)) => (ip, Some(len.trim().parse::<u8>().map_err(|_| bad())?)),
            None => (item, None),
        };
        let ip: IpAddr = ip.trim().parse().map_err(|_| bad())?;
        let len = match (len, ip) {
            (Some(l), IpAddr::V4(_)) if l > 32 => return Err(bad()),
            (Some(l), _) if l > 128 => return Err(bad()),
            (Some(l), _) => l,
            (None, IpAddr::V4(_)) => 32,
            (None, IpAddr::V6(_)) => OWN_V6_PREFIX,
        };
        out.push((mask_net(ip, len), len));
    }
    Ok(out)
}

/// Whether `ip`, or the IPv4 address it embeds (mapped, NAT64, 6to4), is in one of `nets`.
pub fn in_nets(nets: &[(IpAddr, u8)], ip: IpAddr) -> bool {
    let ip = ip.to_canonical();
    let inner = match ip {
        IpAddr::V6(v6) => embedded_v4(v6).map(IpAddr::V4),
        IpAddr::V4(_) => None,
    };
    nets.iter().any(|&(net, len)| {
        net_contains(net, len, ip) || inner.is_some_and(|v4| net_contains(net, len, v4))
    })
}

#[derive(Debug, Default)]
struct OwnState {
    /// Reported at run time (DHT external-address vote, UPnP), newest last.
    learned: Vec<(IpAddr, u8)>,
    /// `MAGNETAR_OWN_ADDRS`, or what a test put there.
    configured: Vec<(IpAddr, u8)>,
    /// Public interface addresses and IPv6 prefixes, from `getifaddrs`.
    ifaces: Vec<(IpAddr, u8)>,
    read_at: Option<Instant>,
}

/// Our own public addresses, refused by every [`AddrPolicy`] that uses this set. The process
/// has one ([`OWN`]); tests build their own with [`OwnAddrs::fixed`].
#[derive(Debug)]
pub struct OwnAddrs {
    state: RwLock<OwnState>,
    read_interfaces: bool,
}

/// The process-wide own-address set: interface addresses (re-read every [`IFACE_TTL`]), the
/// addresses the DHT crawler and UPnP learn, and `MAGNETAR_OWN_ADDRS`.
pub static OWN: OwnAddrs = OwnAddrs {
    state: RwLock::new(OwnState {
        learned: Vec::new(),
        configured: Vec::new(),
        ifaces: Vec::new(),
        read_at: None,
    }),
    read_interfaces: true,
};

impl OwnAddrs {
    /// A set holding only `nets`, with no interface reads (tests; leaked, so keep it small).
    /// Unlike learned and interface addresses, these apply to any address, loopback included,
    /// so a test can refuse 127.0.0.2 while 127.0.0.1 stays reachable.
    pub fn fixed(nets: Vec<(IpAddr, u8)>) -> &'static Self {
        Box::leak(Box::new(Self {
            state: RwLock::new(OwnState {
                configured: nets,
                ..OwnState::default()
            }),
            read_interfaces: false,
        }))
    }

    /// Replace the configured networks ([`parse_nets`]).
    pub fn configure(&self, nets: Vec<(IpAddr, u8)>) {
        self.state
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .configured = nets;
    }

    /// Record an address reported as ours (DHT vote, UPnP). Non-public ones are ignored.
    pub fn learn(&self, ip: IpAddr) {
        let Some(net) = own_nets([(ip, None)]).into_iter().next() else {
            return;
        };
        let mut st = self.state.write().unwrap_or_else(|e| e.into_inner());
        st.learned.retain(|n| *n != net);
        st.learned.push(net);
        let over = st.learned.len().saturating_sub(MAX_LEARNED);
        st.learned.drain(..over);
    }

    /// Whether `ip` is one of ours.
    pub fn denies(&self, ip: IpAddr) -> bool {
        self.refresh(Instant::now());
        let st = self.state.read().unwrap_or_else(|e| e.into_inner());
        in_nets(&st.configured, ip) || in_nets(&st.learned, ip) || in_nets(&st.ifaces, ip)
    }

    /// Every network in the set now (for the librqbit blocklist, written once per session).
    pub fn snapshot(&self) -> Vec<(IpAddr, u8)> {
        self.refresh(Instant::now());
        let st = self.state.read().unwrap_or_else(|e| e.into_inner());
        let mut out = st.configured.clone();
        for n in st.learned.iter().chain(&st.ifaces) {
            if !out.contains(n) {
                out.push(*n);
            }
        }
        out
    }

    fn refresh(&self, now: Instant) {
        if !self.read_interfaces {
            return;
        }
        let fresh = |st: &OwnState| {
            st.read_at
                .is_some_and(|t| now.saturating_duration_since(t) < IFACE_TTL)
        };
        if fresh(&self.state.read().unwrap_or_else(|e| e.into_inner())) {
            return;
        }
        let ifaces = own_nets(interface_addrs());
        let mut st = self.state.write().unwrap_or_else(|e| e.into_inner());
        st.ifaces = ifaces;
        st.read_at = Some(now);
    }
}

/// This host's interface addresses with their prefix lengths (local `getifaddrs`, no traffic).
#[cfg(any(target_os = "linux", target_os = "android"))]
fn interface_addrs() -> Vec<(IpAddr, Option<u8>)> {
    let Ok(list) = nix::ifaddrs::getifaddrs() else {
        return Vec::new();
    };
    list.filter_map(|ifa| {
        let addr = ifa.address?;
        if let Some(a) = addr.as_sockaddr_in6() {
            let len = ifa
                .netmask
                .as_ref()
                .and_then(|m| m.as_sockaddr_in6())
                .map(|m| u128::from(m.ip()).count_ones() as u8);
            Some((IpAddr::V6(a.ip()), len))
        } else {
            addr.as_sockaddr_in().map(|a| (IpAddr::V4(a.ip()), None))
        }
    })
    .collect()
}

#[cfg(not(any(target_os = "linux", target_os = "android")))]
fn interface_addrs() -> Vec<(IpAddr, Option<u8>)> {
    Vec::new()
}

/// [`is_public`] minus our own addresses ([`OwnAddrs`]), plus the test seam. The default (and
/// [`AddrPolicy::PUBLIC`]) refuses every non-public address and every address of ours; no
/// setting lets a private range through.
#[derive(Clone, Copy, Debug, Default)]
pub struct AddrPolicy {
    allow_loopback: bool,
    /// None is the process-wide [`OWN`].
    own: Option<&'static OwnAddrs>,
}

impl PartialEq for AddrPolicy {
    fn eq(&self, other: &Self) -> bool {
        self.allow_loopback == other.allow_loopback && std::ptr::eq(self.own(), other.own())
    }
}

impl Eq for AddrPolicy {}

impl AddrPolicy {
    /// Public addresses only, minus our own (production).
    pub const PUBLIC: Self = Self {
        allow_loopback: false,
        own: None,
    };

    /// Test seam: also lets loopback through, so in-process fake trackers, relays and DHT nodes
    /// on 127.0.0.1 can be reached. Every other private range stays refused. Never built from
    /// configuration.
    pub const fn loopback_for_tests() -> Self {
        Self {
            allow_loopback: true,
            own: None,
        }
    }

    /// The same policy with `own` as its own-address set instead of [`OWN`] (tests).
    pub const fn with_own(self, own: &'static OwnAddrs) -> Self {
        Self {
            allow_loopback: self.allow_loopback,
            own: Some(own),
        }
    }

    fn own(&self) -> &'static OwnAddrs {
        self.own.unwrap_or(&OWN)
    }

    /// Why `ip` is refused, None if it is allowed.
    fn reason(&self, ip: IpAddr) -> Option<&'static str> {
        let open = is_public(ip) || (self.allow_loopback && ip.to_canonical().is_loopback());
        if !open {
            Some("is not a public address")
        } else if self.own().denies(ip) {
            Some("is our own address")
        } else {
            None
        }
    }

    pub fn allows(&self, ip: IpAddr) -> bool {
        self.reason(ip).is_none()
    }

    /// [`Error::Refused`] naming `what` unless `ip` is allowed.
    pub fn check(&self, what: &str, ip: IpAddr) -> Result<()> {
        match self.reason(ip) {
            None => Ok(()),
            Some(why) => Err(refused(what, ip, why)),
        }
    }

    /// Resolve `host` (a name or an IP literal: IPv6 with or without brackets, IPv4 in any
    /// spelling [`literal_ip`] accepts) and keep only the allowed addresses, so the caller
    /// connects to an address that was checked: a name whose records point at 192.168.1.1
    /// gives an error, not a connect. A name with both public and private records keeps the
    /// public ones. A literal never reaches the resolver.
    pub async fn resolve(
        &self,
        what: &str,
        host: &str,
        port: u16,
        timeout: Duration,
    ) -> Result<Vec<SocketAddr>> {
        if let Some(ip) = literal_ip(host) {
            self.check(what, ip)?;
            return Ok(vec![SocketAddr::new(ip, port)]);
        }
        let found: Vec<SocketAddr> =
            tokio::time::timeout(timeout, tokio::net::lookup_host((host, port)))
                .await
                .map_err(|_| Error::Timeout(format!("{what}: DNS lookup")))?
                .map_err(|e| Error::Network(format!("{what}: {e}")))?
                .collect();
        let Some(&first) = found.first() else {
            return Err(Error::Network(format!("{what}: no address")));
        };
        let ok: Vec<SocketAddr> = found.into_iter().filter(|a| self.allows(a.ip())).collect();
        match ok.is_empty() {
            true => Err(self
                .check(what, first.ip())
                .err()
                .unwrap_or_else(|| refused(what, first.ip(), "was refused"))),
            false => Ok(ok),
        }
    }
}

fn refused(what: &str, ip: IpAddr, why: &str) -> Error {
    Error::Refused(format!("{what}: {ip} {why}"))
}

/// reqwest DNS resolver that drops non-allowed addresses ([`AddrPolicy::resolve`]). reqwest
/// connects to IP-literal URLs without asking the resolver, so callers also check a literal host
/// themselves before sending.
#[derive(Clone, Copy, Debug)]
pub struct PolicyResolver {
    pub policy: AddrPolicy,
    pub timeout: Duration,
}

impl reqwest::dns::Resolve for PolicyResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let this = *self;
        Box::pin(async move {
            let host = name.as_str().to_string();
            // reqwest sets the port itself; 0 is a placeholder.
            let addrs = this
                .policy
                .resolve(&host, &host, 0, this.timeout)
                .await
                .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)?;
            Ok(Box::new(addrs.into_iter()) as reqwest::dns::Addrs)
        })
    }
}

/// The IP address a host string spells, if it spells one: plain IPv4 and IPv6 (brackets
/// optional) and every IPv4 form the WHATWG URL parser accepts (as `inet_aton` does): `127.1`,
/// `2130706433`, `0x7f.0.0.1`, `0177.0.0.1`, and a trailing dot (`127.0.0.1.`). A resolver
/// turns such a host into that address without asking DNS, so it is judged as the address.
pub fn literal_ip(host: &str) -> Option<IpAddr> {
    let bare = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);
    if let Ok(ip) = bare.parse::<IpAddr>() {
        return Some(ip);
    }
    if bare.is_empty() || bare.contains(['/', '?', '#', '@', ':', '[', ']', '\\', '%']) {
        return None;
    }
    // The special-scheme host parser applies the WHATWG IPv4 rules.
    match url::Url::parse(&format!("http://{bare}/")).ok()?.host()? {
        url::Host::Ipv4(ip) => Some(IpAddr::V4(ip)),
        url::Host::Ipv6(ip) => Some(IpAddr::V6(ip)),
        url::Host::Domain(_) => None,
    }
}

/// The IP literal a URL's host is, if it is one. Non-special schemes (`udp://`) keep any host,
/// an IPv4 literal included, as an opaque string, so it goes through [`literal_ip`].
pub fn url_ip(url: &url::Url) -> Option<IpAddr> {
    match url.host()? {
        url::Host::Ipv4(ip) => Some(IpAddr::V4(ip)),
        url::Host::Ipv6(ip) => Some(IpAddr::V6(ip)),
        url::Host::Domain(d) => literal_ip(d),
    }
}

/// A URL host that can only be local: a non-public IP literal or a `localhost` name. Other
/// names are judged when they are resolved.
pub fn is_local_host(url: &url::Url) -> bool {
    if let Some(ip) = url_ip(url) {
        return !is_public(ip);
    }
    let d = url.host_str().unwrap_or_default().trim_end_matches('.');
    d.eq_ignore_ascii_case("localhost") || d.to_ascii_lowercase().ends_with(".localhost")
}

/// Check a URL's literal host (IP literals never reach a resolver). Names pass here and are
/// checked when they are resolved.
pub fn check_url_host(policy: AddrPolicy, what: &str, url: &url::Url) -> Result<()> {
    if url.host().is_none() {
        return Err(Error::Invalid(format!("{what}: url without host")));
    }
    match url_ip(url) {
        Some(ip) => policy.check(what, ip),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn refuses_every_local_range() {
        for s in [
            // IPv4: this network, RFC 1918, CGNAT/tailnet, loopback, link-local, IETF, docs,
            // 6to4 relay, benchmarking, multicast, reserved, broadcast.
            "0.0.0.0",
            "0.1.2.3",
            "10.0.0.1",
            "100.64.0.1",
            "100.100.100.100",
            "100.127.255.255",
            "127.0.0.1",
            "127.255.255.254",
            "169.254.169.254",
            "172.16.0.1",
            "172.18.0.2",
            "172.31.255.255",
            "192.0.0.8",
            "192.0.2.1",
            "192.88.99.1",
            "192.168.1.1",
            "198.18.0.1",
            "198.19.255.255",
            "198.51.100.7",
            "203.0.113.9",
            "224.0.0.251",
            "239.255.255.250",
            "240.0.0.1",
            "255.255.255.255",
            // IPv6: unspecified, loopback, v4-compatible, discard, local NAT64, Teredo,
            // benchmarking, ORCHID, docs, ULA, link-local, site-local, multicast.
            "::",
            "::1",
            "::192.168.1.1",
            "100::1",
            "64:ff9b:1::1",
            "2001::1",
            "2001:2::1",
            "2001:10::1",
            "2001:db8::1",
            "3fff::1",
            "fc00::1",
            "fd00::1",
            "fd7a:115c:a1e0::1",
            "fe80::1",
            "fec0::1",
            "ff02::1",
            // Private IPv4 inside IPv6: mapped, NAT64, 6to4.
            "::ffff:127.0.0.1",
            "::ffff:192.168.1.1",
            "::ffff:100.64.0.1",
            "64:ff9b::c0a8:101",
            "64:ff9b::7f00:1",
            "2002:c0a8:101::1",
            "2002:7f00:1::",
        ] {
            assert!(!is_public(ip(s)), "{s} must be refused");
        }
    }

    #[test]
    fn accepts_public_addresses() {
        for s in [
            "1.1.1.1",
            "9.9.9.9",
            "87.98.162.88",
            "100.63.255.255",
            "100.128.0.0",
            "172.15.255.255",
            "172.32.0.0",
            "192.0.1.1",
            "192.169.0.1",
            "198.17.255.255",
            "198.20.0.0",
            "223.255.255.255",
            "2606:4700::1111",
            "2a00:1450:4007:80b::200e",
            "2001:4860:4860::8888",
            "::ffff:1.1.1.1",
            "64:ff9b::101:101",
            "2002:101:101::1",
        ] {
            assert!(is_public(ip(s)), "{s} must be allowed");
        }
    }

    #[test]
    fn loopback_only_through_the_test_seam() {
        let p = AddrPolicy::PUBLIC;
        assert_eq!(p, AddrPolicy::default());
        assert!(!p.allows(ip("127.0.0.1")));
        assert!(!p.allows(ip("::1")));
        let t = AddrPolicy::loopback_for_tests();
        assert!(t.allows(ip("127.0.0.1")));
        assert!(t.allows(ip("::1")));
        assert!(t.allows(ip("::ffff:127.0.0.1")));
        assert!(!t.allows(ip("192.168.1.1")));
        assert!(!t.allows(ip("100.64.0.1")));
        assert!(!t.allows(ip("fd00::1")));
    }

    fn net(s: &str) -> (IpAddr, u8) {
        let (a, l) = s.split_once('/').unwrap();
        (ip(a), l.parse().unwrap())
    }

    /// D112: our WAN address and our IPv6 prefix are public, and refused anyway, in every
    /// spelling that reaches the same host (IPv4-mapped, 6to4). Neighbours outside stay allowed.
    #[test]
    fn own_addresses_are_refused() {
        let own = OwnAddrs::fixed(vec![net("11.22.33.44/32"), net("2a01:e0a:1:2::/64")]);
        let p = AddrPolicy::PUBLIC.with_own(own);
        for s in [
            "11.22.33.44",
            "::ffff:11.22.33.44",
            "2002:b16:212c::1",
            "64:ff9b::b16:212c",
            "2a01:e0a:1:2::1",
            "2a01:e0a:1:2:ffff:ffff:ffff:ffff",
        ] {
            assert!(!p.allows(ip(s)), "{s} is ours");
            let e = p.check("t", ip(s)).unwrap_err();
            assert!(e.to_string().contains("our own"), "{e}");
        }
        for s in ["11.22.33.45", "2a01:e0a:1:3::1", "1.1.1.1"] {
            assert!(p.allows(ip(s)), "{s} is not ours");
        }
        // Private ranges stay refused for their own reason.
        let e = p.check("t", ip("192.168.1.1")).unwrap_err();
        assert!(e.to_string().contains("not a public"), "{e}");
    }

    #[test]
    fn learned_addresses_are_refused_and_bounded() {
        let own = OwnAddrs::fixed(Vec::new());
        let p = AddrPolicy::PUBLIC.with_own(own);
        own.learn(ip("192.168.1.1"));
        own.learn(ip("127.0.0.1"));
        assert!(own.snapshot().is_empty(), "non-public reports are ignored");
        own.learn(ip("11.22.33.44"));
        assert!(!p.allows(ip("11.22.33.44")));
        own.learn(ip("2a01:e0a:1:2::7"));
        assert!(
            !p.allows(ip("2a01:e0a:1:2::8")),
            "the /64 of a learned IPv6"
        );
        for i in 1..=MAX_LEARNED as u8 {
            own.learn(IpAddr::V4(Ipv4Addr::new(11, 0, 0, i)));
        }
        assert!(
            p.allows(ip("11.22.33.44")),
            "oldest learned address dropped"
        );
        assert_eq!(own.snapshot().len(), MAX_LEARNED);
        // The test seam is not affected by the process set: loopback is never learned.
        assert!(AddrPolicy::loopback_for_tests().allows(ip("127.0.0.1")));
    }

    #[test]
    fn interface_addresses_become_own_networks() {
        let got = own_nets([
            (ip("192.168.1.10"), Some(24)),
            (ip("127.0.0.1"), Some(8)),
            (ip("::1"), Some(128)),
            (ip("fe80::1"), Some(64)),
            (ip("fd00::5"), Some(64)),
            (ip("88.1.2.3"), Some(24)),
            (ip("2a01:e0a:1:2:aaaa::5"), Some(64)),
            (ip("2a01:e0a:9::1"), Some(128)),
            (ip("2a01:e0a:7::1"), Some(56)),
            (ip("2a01:e0a:1:2::6"), None),
        ]);
        assert_eq!(
            got,
            vec![
                net("88.1.2.3/32"),
                net("2a01:e0a:1:2::/64"),
                net("2a01:e0a:9::/64"),
                net("2a01:e0a:7::/56"),
            ]
        );
    }

    /// `getifaddrs` works here (lo is always up); no traffic is involved.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    #[test]
    fn interface_addresses_are_read() {
        assert!(
            interface_addrs().iter().any(|(a, _)| a.is_loopback()),
            "{:?}",
            interface_addrs()
        );
    }

    #[test]
    fn parses_own_address_settings() {
        assert_eq!(
            parse_nets(&["203.0.113.7", " 2a01:db8:1:2::9 ", "2a01:db8:1::77/56", ""]).unwrap(),
            vec![
                net("203.0.113.7/32"),
                net("2a01:db8:1:2::/64"),
                net("2a01:db8:1::/56"),
            ]
        );
        for bad in ["host.example", "1.2.3.4/33", "::1/129", "1.2.3.4/x"] {
            assert!(parse_nets(&[bad]).is_err(), "{bad}");
        }
    }

    /// D112: every IPv4 spelling a resolver accepts is judged as the address, also in opaque
    /// `udp://` hosts, so `udp://127.1:6969` is not stored and never reaches a resolver.
    #[test]
    fn odd_ipv4_spellings_are_literals() {
        for s in [
            "127.1",
            "2130706433",
            "0x7f000001",
            "0x7f.0.0.1",
            "0x7f.1",
            "0177.0.0.1",
            "127.0.0.1.",
            "127.0.0.1",
        ] {
            assert_eq!(literal_ip(s), Some(ip("127.0.0.1")), "{s}");
            let u = url::Url::parse(&format!("udp://{s}:6969/announce")).unwrap();
            assert_eq!(url_ip(&u), Some(ip("127.0.0.1")), "{u}");
            assert!(is_local_host(&u), "{u}");
        }
        assert_eq!(literal_ip("[::1]"), Some(ip("::1")));
        assert_eq!(literal_ip("3232235777"), Some(ip("192.168.1.1")));
        for s in ["tracker.example", "localhost", "1.2.3.example", "", "a b"] {
            assert_eq!(literal_ip(s), None, "{s}");
        }
        let u = url::Url::parse("udp://tracker.example:6969/announce").unwrap();
        assert!(!is_local_host(&u));
    }

    #[tokio::test]
    async fn resolve_checks_literals_and_names() {
        let p = AddrPolicy::PUBLIC;
        let t = Duration::from_secs(2);
        let e = p.resolve("t", "192.168.1.1", 80, t).await.unwrap_err();
        assert!(matches!(e, Error::Refused(_)), "{e}");
        // LAN, tailnet and ULA literals are refused here, without a connect: connect tests use
        // loopback targets only.
        for host in ["[fd00::1]", "fd00::1", "100.100.1.1", "100.64.0.1"] {
            let e = p.resolve("t", host, 80, t).await.unwrap_err();
            assert!(matches!(e, Error::Refused(_)), "{host}: {e}");
        }
        // `localhost` resolves locally (no DNS traffic) to loopback only.
        let e = p.resolve("t", "localhost", 80, t).await.unwrap_err();
        assert!(matches!(e, Error::Refused(_)), "{e}");
        let ok = AddrPolicy::loopback_for_tests()
            .resolve("t", "localhost", 80, t)
            .await
            .unwrap();
        assert!(ok.iter().all(|a| a.ip().is_loopback() && a.port() == 80));
        assert_eq!(
            p.resolve("t", "1.1.1.1", 443, t).await.unwrap(),
            vec!["1.1.1.1:443".parse().unwrap()]
        );
    }
}

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

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;

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

/// [`is_public`] plus the test seam. The default (and [`AddrPolicy::PUBLIC`]) refuses every
/// non-public address; there is no setting that changes it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AddrPolicy {
    allow_loopback: bool,
}

impl AddrPolicy {
    /// Public addresses only (production).
    pub const PUBLIC: Self = Self {
        allow_loopback: false,
    };

    /// Test seam: also lets loopback through, so in-process fake trackers, relays and DHT nodes
    /// on 127.0.0.1 can be reached. Every other private range stays refused. Never built from
    /// configuration.
    pub const fn loopback_for_tests() -> Self {
        Self {
            allow_loopback: true,
        }
    }

    pub fn allows(&self, ip: IpAddr) -> bool {
        is_public(ip) || (self.allow_loopback && ip.to_canonical().is_loopback())
    }

    /// [`Error::Refused`] naming `what` unless `ip` is allowed.
    pub fn check(&self, what: &str, ip: IpAddr) -> Result<()> {
        if self.allows(ip) {
            Ok(())
        } else {
            Err(refused(what, ip))
        }
    }

    /// Resolve `host` (a name or an IP literal, IPv6 with or without brackets) and keep only the
    /// allowed addresses, so the caller connects to an address that was checked: a name whose
    /// records point at 192.168.1.1 gives an error, not a connect. A name with both public and
    /// private records keeps the public ones.
    pub async fn resolve(
        &self,
        what: &str,
        host: &str,
        port: u16,
        timeout: Duration,
    ) -> Result<Vec<SocketAddr>> {
        let host = host.trim_start_matches('[').trim_end_matches(']');
        if let Ok(ip) = host.parse::<IpAddr>() {
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
        if ok.is_empty() {
            return Err(refused(what, first.ip()));
        }
        Ok(ok)
    }
}

fn refused(what: &str, ip: IpAddr) -> Error {
    Error::Refused(format!("{what}: {ip} is not a public address"))
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

/// The IP literal a URL's host is, if it is one. Non-special schemes (`udp://`) keep an IPv4
/// literal as an opaque host string, so that form is parsed too.
pub fn url_ip(url: &url::Url) -> Option<IpAddr> {
    match url.host()? {
        url::Host::Ipv4(ip) => Some(IpAddr::V4(ip)),
        url::Host::Ipv6(ip) => Some(IpAddr::V6(ip)),
        url::Host::Domain(d) => d.parse().ok(),
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

    #[tokio::test]
    async fn resolve_checks_literals_and_names() {
        let p = AddrPolicy::PUBLIC;
        let t = Duration::from_secs(2);
        let e = p.resolve("t", "192.168.1.1", 80, t).await.unwrap_err();
        assert!(matches!(e, Error::Refused(_)), "{e}");
        let e = p.resolve("t", "[fd00::1]", 80, t).await.unwrap_err();
        assert!(matches!(e, Error::Refused(_)), "{e}");
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

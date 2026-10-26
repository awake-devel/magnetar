//! Which addresses the crawler may send to (docs/DHT.md §3.8, D55).
//!
//! A hostile node can return `nodes`/`values` that point at loopback (Postgres on
//! 127.0.0.1:5432), the router's admin page or other LAN hosts; the sampler and the peer lookups
//! would then spray them. Every address is checked before a UDP send (and stage 2 checks it
//! before a TCP connect): non-global IPv4 ranges and our own external address are refused.

use std::net::{Ipv4Addr, SocketAddrV4};

/// Ranges never contacted: 0/8, 10/8, 100.64/10, 127/8, 169.254/16, 172.16/12, 192.0.0/24,
/// 192.168/16, 198.18/15, 224/4 (multicast) and 240/4 (reserved, broadcast).
const BLOCKED: &[(u32, u8)] = &[
    (0x0000_0000, 8),
    (0x0a00_0000, 8),
    (0x6440_0000, 10),
    (0x7f00_0000, 8),
    (0xa9fe_0000, 16),
    (0xac10_0000, 12),
    (0xc000_0000, 24),
    (0xc0a8_0000, 16),
    (0xc612_0000, 15),
    (0xe000_0000, 4),
    (0xf000_0000, 4),
];

/// Whether `ip` is a globally routable unicast IPv4 address by the list above.
pub fn is_global(ip: Ipv4Addr) -> bool {
    let v = u32::from(ip);
    !BLOCKED
        .iter()
        .any(|&(net, len)| v & (u32::MAX << (32 - u32::from(len))) == net)
}

/// The address check, with our own external address once known.
#[derive(Clone, Debug, Default)]
pub struct AddrFilter {
    own: Option<Ipv4Addr>,
    /// Test seam: lets 127.0.0.0/8 through so in-process fake DHT nodes can be reached. Never
    /// set from configuration.
    allow_loopback: bool,
}

impl AddrFilter {
    pub fn new(allow_loopback: bool) -> Self {
        Self {
            own: None,
            allow_loopback,
        }
    }

    pub fn set_own(&mut self, ip: Option<Ipv4Addr>) {
        self.own = ip;
    }

    /// Whether we may send to `addr` (port 0 is never valid).
    pub fn allows(&self, addr: &SocketAddrV4) -> bool {
        let ip = *addr.ip();
        if addr.port() == 0 || Some(ip) == self.own {
            return false;
        }
        is_global(ip) || (self.allow_loopback && ip.is_loopback())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(s: &str) -> SocketAddrV4 {
        s.parse().unwrap()
    }

    #[test]
    fn refuses_local_and_reserved() {
        let f = AddrFilter::new(false);
        for s in [
            "0.1.2.3:6881",
            "10.0.0.1:6881",
            "100.64.0.1:6881",
            "100.127.255.255:6881",
            "127.0.0.1:5432",
            "169.254.1.1:6881",
            "172.16.0.1:6881",
            "172.31.255.255:6881",
            "192.0.0.8:6881",
            "192.168.1.1:80",
            "198.18.0.1:6881",
            "198.19.255.255:6881",
            "224.0.0.251:5353",
            "239.255.255.250:1900",
            "240.0.0.1:6881",
            "255.255.255.255:6881",
        ] {
            assert!(!f.allows(&a(s)), "{s}");
        }
        for s in [
            "1.1.1.1:6881",
            "100.63.255.255:6881",
            "100.128.0.0:6881",
            "172.15.255.255:6881",
            "172.32.0.0:6881",
            "192.0.1.1:6881",
            "198.17.255.255:6881",
            "198.20.0.0:6881",
            "223.255.255.255:6881",
            "87.98.162.88:6881",
        ] {
            assert!(f.allows(&a(s)), "{s}");
        }
    }

    #[test]
    fn refuses_port_zero_and_own_address() {
        let mut f = AddrFilter::new(false);
        assert!(!f.allows(&a("1.2.3.4:0")));
        f.set_own(Some("1.2.3.4".parse().unwrap()));
        assert!(!f.allows(&a("1.2.3.4:6881")));
        assert!(f.allows(&a("1.2.3.5:6881")));
    }

    #[test]
    fn loopback_only_with_the_test_seam() {
        assert!(!AddrFilter::new(false).allows(&a("127.0.0.1:7000")));
        let t = AddrFilter::new(true);
        assert!(t.allows(&a("127.0.0.1:7000")));
        assert!(!t.allows(&a("192.168.1.1:7000")));
    }
}

//! Relay URL normalizer (wot-relays.md §3). Owner: nostr agent.

use std::net::{Ipv4Addr, Ipv6Addr};

use url::{Host, Url};

pub const MAX_RELAY_URL_LEN: usize = 255;

/// TLDs that never resolve publicly: special-use or private-use names (RFC 6761 test/invalid/
/// example, RFC 9476 alt, ICANN's .internal, the never-delegated home/corp/mail) and common LAN
/// suffixes. None is in the IANA root zone (tlds-alpha-by-domain.txt 2026102500); `arpa` is, but
/// only for infrastructure (home.arpa, in-addr.arpa). Seen live: `ws://apollo.home:3355` in a
/// NIP-65 list.
const PRIVATE_TLDS: &[&str] = &[
    "alt",
    "arpa",
    "corp",
    "example",
    "gateway",
    "home",
    "internal",
    "intranet",
    "invalid",
    "lan",
    "localdomain",
    "mail",
    "priv",
    "private",
    "router",
    "test",
    "workgroup",
];

/// ws/wss only, no userinfo, lowercase host, default port dropped, no query/fragment, trailing
/// slashes stripped, ≤255 chars, no localhost/.local/private TLDs/private IPs, onion only if allowed.
pub fn normalize_relay(raw: &str, allow_onion: bool) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty() || raw.len() > 2 * MAX_RELAY_URL_LEN || raw.contains(char::is_whitespace) {
        return None;
    }
    // `url` lowercases the scheme and host and drops :443 / :80 for ws(s), which are special schemes.
    let mut u = Url::parse(raw).ok()?;
    if !matches!(u.scheme(), "ws" | "wss") {
        return None;
    }
    if !u.username().is_empty() || u.password().is_some() {
        return None;
    }
    match u.host()? {
        Host::Domain(d) => {
            let d = d.trim_end_matches('.');
            let label_ok = |l: &str| {
                !l.is_empty()
                    && !l.starts_with('-')
                    && !l.ends_with('-')
                    && l.bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            };
            // Single-label hosts never resolve publicly.
            if !d.contains('.') || !d.split('.').all(label_ok) {
                return None;
            }
            if d == "localhost" || d.ends_with(".localhost") || d.ends_with(".local") {
                return None;
            }
            if d.ends_with(".onion") && !allow_onion {
                return None;
            }
            if PRIVATE_TLDS.contains(&d.rsplit('.').next().unwrap_or_default()) {
                return None;
            }
        }
        Host::Ipv4(ip) if !public_v4(ip) => return None,
        Host::Ipv6(ip) if !public_v6(ip) => return None,
        _ => {}
    }
    u.set_fragment(None);
    u.set_query(None);
    let mut s = u.to_string();
    // Strip trailing slashes, including the root "/" that `url` always adds.
    while s.ends_with('/') && s.matches('/').count() > 2 {
        s.pop();
    }
    (s.len() <= MAX_RELAY_URL_LEN).then_some(s)
}

fn public_v4(ip: Ipv4Addr) -> bool {
    let [a, b, ..] = ip.octets();
    !(ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_unspecified()
        || ip.is_broadcast()
        || ip.is_documentation()
        || ip.is_multicast()
        || a == 0
        // 100.64.0.0/10 carrier-grade NAT (also Tailscale).
        || (a == 100 && (64..128).contains(&b)))
}

fn public_v6(ip: Ipv6Addr) -> bool {
    if let Some(v4) = ip.to_ipv4_mapped() {
        return public_v4(v4);
    }
    let first = ip.segments()[0];
    !(ip.is_loopback()
        || ip.is_unspecified()
        || ip.is_multicast()
        || (first & 0xfe00) == 0xfc00 // unique local
        || (first & 0xffc0) == 0xfe80) // link local
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(s: &str) -> Option<String> {
        normalize_relay(s, false)
    }

    #[test]
    fn research_vectors() {
        // wot-relays.md §3.
        assert_eq!(
            n("wss://relay.damus.io/").as_deref(),
            Some("wss://relay.damus.io")
        );
        assert_eq!(
            n("WSS://Relay.Damus.IO:443/").as_deref(),
            Some("wss://relay.damus.io")
        );
        assert_eq!(n("https://x.com"), None);
    }

    #[test]
    fn paths_ports_and_cleanup() {
        assert_eq!(n("wss://nos.lol").as_deref(), Some("wss://nos.lol"));
        assert_eq!(n("  wss://nos.lol//  ").as_deref(), Some("wss://nos.lol"));
        assert_eq!(
            n("wss://nostr.wine/inbox/").as_deref(),
            Some("wss://nostr.wine/inbox")
        );
        assert_eq!(
            n("wss://relay.x.com/haven-marble-ivory").as_deref(),
            Some("wss://relay.x.com/haven-marble-ivory")
        );
        assert_eq!(
            n("ws://relay.x.com:80").as_deref(),
            Some("ws://relay.x.com")
        );
        assert_eq!(
            n("wss://relay.x.com:8443/").as_deref(),
            Some("wss://relay.x.com:8443")
        );
        assert_eq!(
            n("wss://relay.x.com/?token=1#frag").as_deref(),
            Some("wss://relay.x.com")
        );
    }

    #[test]
    fn rejects() {
        for bad in [
            "",
            "nos.lol",
            "http://nos.lol",
            "wss://user:pw@nos.lol",
            "wss://localhost:7777",
            "ws://127.0.0.1:8080",
            "ws://10.0.0.2",
            "ws://192.168.1.10",
            "ws://100.100.1.1",
            "ws://[::1]:7777",
            "ws://[fd00::1]",
            "ws://printer.local",
            "ws://apollo.home:3355",
            "wss://nas.lan",
            "wss://relay.corp.internal",
            "ws://pi.home.arpa",
            "wss://relay.test",
            "wss://relay",
            "wss://relay .com",
            "wss://nos.lol,wss://x.com",
            "wss://xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx.onion",
        ] {
            assert_eq!(n(bad), None, "{bad:?} should be rejected");
        }
        let long = format!("wss://relay.example.com/{}", "a".repeat(260));
        assert_eq!(n(&long), None);
    }

    #[test]
    fn onion_and_public_ips() {
        let onion = "ws://xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx.onion";
        assert_eq!(normalize_relay(onion, true).as_deref(), Some(onion));
        assert_eq!(n("wss://1.1.1.1/").as_deref(), Some("wss://1.1.1.1"));
        assert_eq!(
            n("wss://[2606:4700::1111]").as_deref(),
            Some("wss://[2606:4700::1111]")
        );
    }
}

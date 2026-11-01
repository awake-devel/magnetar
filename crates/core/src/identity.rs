//! How Magnetar names itself on the wire (docs/DHT.md §3.3, D115). Every protocol carries the
//! same honest client name, derived from the crate version, so DHT nodes, peers, trackers and
//! relays can tell it is an indexing crawler (it fetches metadata and never uploads) and block
//! it if they want to. Never impersonate another client.

/// The project page, given in the HTTP User-Agent so an operator can look the crawler up.
pub const URL: &str = env!("CARGO_PKG_REPOSITORY");

/// HTTP and WebSocket User-Agent: `magnetar/<version> (+<repository>)`.
pub const USER_AGENT: &str = concat!(
    "magnetar/",
    env!("CARGO_PKG_VERSION"),
    " (+",
    env!("CARGO_PKG_REPOSITORY"),
    ")"
);

/// BEP 10 extended handshake `v`: `Magnetar <version>`.
pub const CLIENT_NAME: &str = concat!("Magnetar ", env!("CARGO_PKG_VERSION"));

const MAJOR: u8 = digits(env!("CARGO_PKG_VERSION_MAJOR"));
const MINOR: u8 = digits(env!("CARGO_PKG_VERSION_MINOR"));
const PATCH: u8 = digits(env!("CARGO_PKG_VERSION_PATCH"));

const fn digits(s: &str) -> u8 {
    let b = s.as_bytes();
    let mut i = 0;
    let mut n: u32 = 0;
    while i < b.len() {
        assert!(b[i].is_ascii_digit(), "version part is not a number");
        n = n * 10 + (b[i] - b'0') as u32;
        assert!(n <= 255, "version part above 255");
        i += 1;
    }
    n as u8
}

/// One Azureus-style version character: 0–9, then A–Z (BEP 20 clients use letters past 9).
const fn version_char(n: u8) -> u8 {
    assert!(n < 36, "version part too large for a peer id character");
    if n < 10 { b'0' + n } else { b'A' + (n - 10) }
}

/// KRPC `v` (BEP 5): `MG` (unregistered in BEP 20's list, checked 2026-10-26), then the major
/// version and `minor << 4 | patch` (0.3.1 → `MG 00 31`).
pub const KRPC_VERSION: [u8; 4] = {
    assert!(MINOR < 16 && PATCH < 16, "minor/patch do not fit a nibble");
    [b'M', b'G', MAJOR, (MINOR << 4) | PATCH]
};

/// BEP 20 Azureus-style peer id prefix: `-MG` + major, minor, patch + `0` + `-` (0.3.1 →
/// `-MG0310-`). The 12 bytes after it are random per fetcher.
pub const PEER_ID_PREFIX: [u8; 8] = [
    b'-',
    b'M',
    b'G',
    version_char(MAJOR),
    version_char(MINOR),
    version_char(PATCH),
    b'0',
    b'-',
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_tag_follows_the_crate_version() {
        let v: Vec<u8> = env!("CARGO_PKG_VERSION")
            .split('.')
            .map(|p| p.parse().unwrap())
            .collect();
        assert_eq!(KRPC_VERSION, [b'M', b'G', v[0], (v[1] << 4) | v[2]]);
        let id = format!("-MG{}{}{}0-", v[0], v[1], v[2]);
        assert_eq!(&PEER_ID_PREFIX, id.as_bytes());
        assert_eq!(
            USER_AGENT,
            format!(
                "magnetar/{} (+https://github.com/awake-devel/magnetar)",
                env!("CARGO_PKG_VERSION")
            )
        );
        assert_eq!(
            CLIENT_NAME,
            format!("Magnetar {}", env!("CARGO_PKG_VERSION"))
        );
    }

    #[test]
    fn version_parts_parse() {
        assert_eq!(digits("0"), 0);
        assert_eq!(digits("31"), 31);
        assert_eq!(version_char(9), b'9');
        assert_eq!(version_char(10), b'A');
    }
}

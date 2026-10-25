//! Infohash normalisation and magnet URIs (bittorrent.md §6–7). Owner: nostr agent.

use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, percent_decode_str, utf8_percent_encode};

use crate::error::{Error, Result};

/// Fallback trackers added when an event has none (dtan defaults, live-checked in bittorrent.md §1).
pub const DEFAULT_TRACKERS: &[&str] = &[
    "udp://tracker.opentrackr.org:1337/announce",
    "udp://open.stealth.si:80/announce",
    "udp://tracker.torrent.eu.org:451/announce",
];

/// Trackers kept in a magnet (keeps the URI short enough for Android intents).
pub const MAX_MAGNET_TRACKERS: usize = 10;

/// Everything except RFC 3986 unreserved characters, so `&=+#%:/` and spaces are always encoded.
const COMPONENT: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

/// Extract a v1 infohash as lowercase 40-hex from a tag value: first [0-9a-fA-F]{40} run, else a
/// 32-char base32 token decoded. None otherwise.
pub fn normalize_infohash(s: &str) -> Option<String> {
    // A run must be exactly 40 long: a 64-hex v2 hash contains 40-hex substrings that are not v1.
    if let Some(run) = runs(s, |c| c.is_ascii_hexdigit()).find(|r| r.len() == 40) {
        return Some(run.to_ascii_lowercase());
    }
    runs(s, |c| c.is_ascii_alphanumeric())
        .filter(|r| r.len() == 32)
        .find_map(|r| {
            let bytes = data_encoding::BASE32_NOPAD
                .decode(r.to_ascii_uppercase().as_bytes())
                .ok()?;
            (bytes.len() == 20).then(|| hex::encode(bytes))
        })
}

/// A BEP 52 v2 infohash (64 hex, optionally as a `1220`-prefixed multihash) as lowercase hex.
pub fn normalize_infohash_v2(s: &str) -> Option<String> {
    runs(s, |c| c.is_ascii_hexdigit()).find_map(|r| match r.len() {
        64 => Some(r.to_ascii_lowercase()),
        68 if r.starts_with("1220") => Some(r[4..].to_ascii_lowercase()),
        _ => None,
    })
}

/// Maximal runs of characters matching `pred`.
fn runs(s: &str, pred: impl Fn(char) -> bool + Copy) -> impl Iterator<Item = &str> {
    s.split(move |c: char| !pred(c)).filter(|r| !r.is_empty())
}

/// magnet:?xt=urn:btih:<hex>&dn=..&tr=..(max 10)[&xl=..]
///
/// Trackers are deduplicated in order; when none are given the [`DEFAULT_TRACKERS`] are used.
pub fn build_magnet(
    infohash: &str,
    name: Option<&str>,
    trackers: &[String],
    total: Option<u64>,
) -> String {
    let ih = normalize_infohash(infohash).unwrap_or_else(|| infohash.trim().to_ascii_lowercase());
    let mut s = format!("magnet:?xt=urn:btih:{ih}");
    if let Some(n) = name.map(str::trim).filter(|n| !n.is_empty()) {
        s.push_str("&dn=");
        s.extend(utf8_percent_encode(n, COMPONENT));
    }
    let given: Vec<&str> = trackers
        .iter()
        .map(|t| t.trim())
        .filter(|t| !t.is_empty())
        .collect();
    let list = if given.is_empty() {
        DEFAULT_TRACKERS.to_vec()
    } else {
        given
    };
    let mut seen: Vec<&str> = Vec::new();
    for t in list {
        if seen.len() == MAX_MAGNET_TRACKERS {
            break;
        }
        if seen.contains(&t) {
            continue;
        }
        seen.push(t);
        s.push_str("&tr=");
        s.extend(utf8_percent_encode(t, COMPONENT));
    }
    if let Some(x) = total.filter(|x| *x > 0) {
        s.push_str(&format!("&xl={x}"));
    }
    s
}

#[derive(Clone, Debug, PartialEq)]
pub struct MagnetInfo {
    pub infohash: String,
    pub name: Option<String>,
    pub trackers: Vec<String>,
}

/// Parse a pasted magnet (btih hex or base32).
pub fn parse_magnet(uri: &str) -> Result<MagnetInfo> {
    let uri = uri.trim();
    let query = uri
        .get(..7)
        .filter(|p| p.eq_ignore_ascii_case("magnet:"))
        .map(|_| uri[7..].trim_start_matches('?'))
        .ok_or_else(|| Error::Invalid("not a magnet URI".into()))?;

    let decode = |v: &str| percent_decode_str(v).decode_utf8_lossy().trim().to_string();
    let mut infohash = None;
    let mut name = None;
    let mut trackers: Vec<String> = Vec::new();
    for pair in query.split('&') {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        let key = key.to_ascii_lowercase();
        match key.as_str() {
            // `xt.1`, `xt.2` … are the BEP 9 multi-xt form.
            k if k == "xt" || k.starts_with("xt.") => {
                let v = decode(value).to_ascii_lowercase();
                if infohash.is_none()
                    && let Some(h) = v.strip_prefix("urn:btih:")
                    && matches!(h.len(), 32 | 40)
                {
                    infohash = normalize_infohash(h);
                }
            }
            // `+` is a space in form-encoded names, which some magnet producers emit.
            "dn" => name = Some(decode(&value.replace('+', " "))).filter(|n| !n.is_empty()),
            "tr" => {
                let t = decode(value);
                if !t.is_empty() && !trackers.contains(&t) {
                    trackers.push(t);
                }
            }
            _ => {}
        }
    }
    let infohash =
        infohash.ok_or_else(|| Error::Invalid("magnet has no valid urn:btih infohash".into()))?;
    Ok(MagnetInfo {
        infohash,
        name,
        trackers,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEX: &str = "77dcbd4597cb43f549fd1433d516a696db30044c";

    #[test]
    fn infohash_hex_variants() {
        assert_eq!(normalize_infohash(HEX).as_deref(), Some(HEX));
        assert_eq!(
            normalize_infohash(&HEX.to_ascii_uppercase()).as_deref(),
            Some(HEX)
        );
        assert_eq!(
            normalize_infohash(&format!(" {HEX}&dn")).as_deref(),
            Some(HEX)
        );
        assert_eq!(
            normalize_infohash(&format!("urn:btih:{HEX}")).as_deref(),
            Some(HEX)
        );
        // 41 hex chars is not a v1 hash; neither is a 64-hex v2 hash.
        assert_eq!(normalize_infohash(&format!("{HEX}a")), None);
        let v2 = "84ccd75d383a24368dd8723d3bd62e564cc06a2ac09dbc7b88b37e4369c1290b";
        assert_eq!(normalize_infohash(v2), None);
        assert_eq!(normalize_infohash_v2(v2).as_deref(), Some(v2));
        assert_eq!(
            normalize_infohash_v2(&format!("1220{}", v2.to_uppercase())).as_deref(),
            Some(v2)
        );
        assert_eq!(normalize_infohash(""), None);
        assert_eq!(normalize_infohash("not a hash"), None);
    }

    #[test]
    fn infohash_base32() {
        let bytes = hex::decode(HEX).unwrap();
        let b32 = data_encoding::BASE32_NOPAD.encode(&bytes);
        assert_eq!(b32.len(), 32);
        assert_eq!(normalize_infohash(&b32).as_deref(), Some(HEX));
        assert_eq!(
            normalize_infohash(&b32.to_ascii_lowercase()).as_deref(),
            Some(HEX)
        );
        // 32 alphanumerics outside the base32 alphabet.
        assert_eq!(normalize_infohash("0189018901890189018901890189018x"), None);
    }

    #[test]
    fn magnet_encoding_and_caps() {
        let trackers: Vec<String> = (0..15)
            .map(|i| format!("udp://t{i}.example:80/announce"))
            .chain(["udp://t0.example:80/announce".to_string()])
            .collect();
        let m = build_magnet(HEX, Some("A & B=C+D #1/é"), &trackers, Some(1234));
        assert!(m.starts_with(&format!("magnet:?xt=urn:btih:{HEX}&dn=")));
        assert!(m.contains("&dn=A%20%26%20B%3DC%2BD%20%231%2F%C3%A9&"));
        assert_eq!(m.matches("&tr=").count(), MAX_MAGNET_TRACKERS);
        assert!(m.contains("&tr=udp%3A%2F%2Ft0.example%3A80%2Fannounce"));
        assert!(m.ends_with("&xl=1234"));

        let parsed = parse_magnet(&m).unwrap();
        assert_eq!(parsed.infohash, HEX);
        assert_eq!(parsed.name.as_deref(), Some("A & B=C+D #1/é"));
        assert_eq!(parsed.trackers.len(), MAX_MAGNET_TRACKERS);
        assert_eq!(parsed.trackers[0], "udp://t0.example:80/announce");
    }

    #[test]
    fn magnet_defaults_when_no_trackers() {
        let m = build_magnet(&HEX.to_uppercase(), None, &[], None);
        assert!(m.starts_with(&format!("magnet:?xt=urn:btih:{HEX}&tr=")));
        assert_eq!(m.matches("&tr=").count(), DEFAULT_TRACKERS.len());
        assert!(!m.contains("&dn=") && !m.contains("&xl="));
        assert!(!build_magnet(HEX, Some("  "), &[], Some(0)).contains("&xl="));
    }

    #[test]
    fn parse_pasted_magnets() {
        let bytes = hex::decode(HEX).unwrap();
        let b32 = data_encoding::BASE32_NOPAD.encode(&bytes);
        let m = parse_magnet(&format!(
            "MAGNET:?xt=urn:btmh:1220{}&xt=urn:btih:{b32}&dn=Some+Name&tr=udp%3A%2F%2Fa%3A1&tr=udp://a:1",
            "ab".repeat(32)
        ))
        .unwrap();
        assert_eq!(m.infohash, HEX);
        assert_eq!(m.name.as_deref(), Some("Some Name"));
        assert_eq!(m.trackers, vec!["udp://a:1".to_string()]);

        assert!(parse_magnet("https://example.com").is_err());
        assert!(parse_magnet("magnet:?dn=x").is_err());
        assert!(parse_magnet(&format!("magnet:?xt=urn:btmh:1220{}", "ab".repeat(32))).is_err());
        assert!(parse_magnet(&format!("magnet:?xt=urn:btih:{HEX}xx")).is_err());
    }
}

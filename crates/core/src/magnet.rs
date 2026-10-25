//! Infohash normalisation and magnet URIs (bittorrent.md §6–7). Owner: nostr agent.

use crate::error::Result;

/// Fallback trackers added when an event has none (dtan defaults, live-checked in bittorrent.md §1).
pub const DEFAULT_TRACKERS: &[&str] = &[
    "udp://tracker.opentrackr.org:1337/announce",
    "udp://open.stealth.si:80/announce",
    "udp://tracker.torrent.eu.org:451/announce",
];

/// Extract a v1 infohash as lowercase 40-hex from a tag value: first [0-9a-fA-F]{40} run, else a
/// 32-char base32 token decoded. None otherwise.
pub fn normalize_infohash(s: &str) -> Option<String> {
    let _ = s;
    todo!("magnet::normalize_infohash")
}

/// magnet:?xt=urn:btih:<hex>&dn=..&tr=..(max 10)[&xl=..]
pub fn build_magnet(
    infohash: &str,
    name: Option<&str>,
    trackers: &[String],
    total: Option<u64>,
) -> String {
    let _ = (infohash, name, trackers, total);
    todo!("magnet::build_magnet")
}

#[derive(Clone, Debug, PartialEq)]
pub struct MagnetInfo {
    pub infohash: String,
    pub name: Option<String>,
    pub trackers: Vec<String>,
}

/// Parse a pasted magnet (btih hex or base32).
pub fn parse_magnet(uri: &str) -> Result<MagnetInfo> {
    let _ = uri;
    todo!("magnet::parse_magnet")
}

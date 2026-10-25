//! Relay URL normalizer (wot-relays.md §3). Owner: nostr agent.

/// ws/wss only, no userinfo, lowercase host, default port dropped, no query/fragment, trailing
/// slashes stripped, ≤255 chars, no localhost/.local/private IPs, onion only if allowed.
pub fn normalize_relay(raw: &str, allow_onion: bool) -> Option<String> {
    let _ = (raw, allow_onion);
    todo!("relayurl::normalize_relay")
}

//! Extraction of WoT data from replaceable/report events (wot-relays.md §2–6). Owner: trust-sync agent.
//! Lenient own parsers (the nostr crate's typed parsers drop whole tags on junk).

use crate::model::RawEvent;

/// Followed pubkeys of a kind 3 (lowercase hex, deduped, self removed, capped 10 000).
pub fn follows_of(ev: &RawEvent) -> Vec<String> {
    let _ = ev;
    todo!("wot::follows_of")
}

/// Public muted pubkeys of a kind 10000, or of a 30007 with d="2003".
pub fn mutes_of(ev: &RawEvent) -> Vec<String> {
    let _ = ev;
    todo!("wot::mutes_of")
}

/// (target pubkey, report type) of a kind 1984.
pub fn report_of(ev: &RawEvent) -> Option<(String, String)> {
    let _ = ev;
    todo!("wot::report_of")
}

#[derive(Clone, Debug, PartialEq)]
pub struct RelayListEntry {
    pub url: String,
    pub read: bool,
    pub write: bool,
}

/// Relay list of a kind 10002 (normalized, flags ORed on duplicates); `oversized` if > 50 r tags.
/// For a kind 3 with legacy JSON content, parses that map instead.
pub fn relay_list_of(ev: &RawEvent) -> (Vec<RelayListEntry>, bool) {
    let _ = ev;
    todo!("wot::relay_list_of")
}

/// Display name from a kind 0 (name | display_name), if any.
pub fn profile_name(ev: &RawEvent) -> Option<String> {
    let _ = ev;
    todo!("wot::profile_name")
}

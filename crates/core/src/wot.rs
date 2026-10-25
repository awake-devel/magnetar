//! Extraction of WoT data from replaceable/report events (wot-relays.md §2–6). Owner: trust-sync agent.
//! Lenient own parsers (the nostr crate's typed parsers drop whole tags on junk).

use std::collections::HashSet;

use crate::model::{
    KIND_CONTACTS, KIND_KIND_MUTE_SET, KIND_MUTE_LIST, KIND_PROFILE, KIND_RELAY_LIST, KIND_REPORT,
    RawEvent,
};

/// Policy cap on follows per list (real max seen ~2000).
pub const MAX_FOLLOWS: usize = 10_000;
/// A relay list with more `r` entries than this is spam: flagged, never used for writes.
pub const MAX_RELAY_LIST: usize = 50;

/// `tag[1]` as a lowercase hex pubkey, if it is one (case-insensitive input).
fn hex_pubkey(tag: &[String]) -> Option<String> {
    let h = tag.get(1)?.trim();
    (h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit())).then(|| h.to_ascii_lowercase())
}

fn is_tag(t: &[String], name: &str) -> bool {
    t.first().map(String::as_str) == Some(name)
}

/// Lowercase hex pubkeys of every `p` tag, deduped (first occurrence kept), self removed.
/// Relay hints and petnames are ignored, so junk there never drops the entry.
fn p_tags(ev: &RawEvent, cap: usize) -> Vec<String> {
    let me = ev.pubkey.to_ascii_lowercase();
    let mut seen = HashSet::new();
    ev.tags
        .iter()
        .filter(|t| is_tag(t, "p"))
        .filter_map(|t| hex_pubkey(t))
        .filter(|pk| *pk != me && seen.insert(pk.clone()))
        .take(cap)
        .collect()
}

/// Followed pubkeys of a kind 3 (lowercase hex, deduped, self removed, capped 10 000).
pub fn follows_of(ev: &RawEvent) -> Vec<String> {
    if ev.kind != KIND_CONTACTS {
        return Vec::new();
    }
    p_tags(ev, MAX_FOLLOWS)
}

/// Public muted pubkeys of a kind 10000, or of a 30007 with d="2003".
pub fn mutes_of(ev: &RawEvent) -> Vec<String> {
    let applies = match ev.kind {
        KIND_MUTE_LIST => true,
        KIND_KIND_MUTE_SET => ev
            .tag("d")
            .and_then(|t| t.get(1))
            .is_some_and(|d| d.trim() == "2003"),
        _ => false,
    };
    if !applies {
        return Vec::new();
    }
    p_tags(ev, MAX_FOLLOWS)
}

/// (target pubkey, report type) of a kind 1984.
///
/// NIP-56 puts the type on the `p` tag; when it is missing we take it from an `e` tag, else "other".
pub fn report_of(ev: &RawEvent) -> Option<(String, String)> {
    if ev.kind != KIND_REPORT {
        return None;
    }
    let p = ev
        .tags
        .iter()
        .find(|t| is_tag(t, "p") && hex_pubkey(t).is_some())?;
    let target = hex_pubkey(p)?;
    if target == ev.pubkey.to_ascii_lowercase() {
        return None;
    }
    let report_type = |t: &Vec<String>| {
        t.get(2)
            .map(|s| s.trim().to_ascii_lowercase())
            .filter(|s| !s.is_empty())
    };
    let ty = report_type(p)
        .or_else(|| {
            ev.tags
                .iter()
                .filter(|t| is_tag(t, "e"))
                .find_map(report_type)
        })
        .unwrap_or_else(|| "other".to_string());
    Some((target, ty))
}

#[derive(Clone, Debug, PartialEq)]
pub struct RelayListEntry {
    pub url: String,
    pub read: bool,
    pub write: bool,
}

/// Normalizes URLs and ORs the flags of duplicates (order of first sight kept).
#[derive(Default)]
struct RelayListBuilder {
    entries: Vec<RelayListEntry>,
}

impl RelayListBuilder {
    fn add(&mut self, raw_url: &str, read: bool, write: bool) {
        if !read && !write {
            return;
        }
        let Some(url) = crate::relayurl::normalize_relay(raw_url, false) else {
            return;
        };
        match self.entries.iter_mut().find(|e| e.url == url) {
            Some(e) => {
                e.read |= read;
                e.write |= write;
            }
            None => self.entries.push(RelayListEntry { url, read, write }),
        }
    }
}

/// Relay list of a kind 10002 (normalized, flags ORed on duplicates); `oversized` if > 50 r tags.
/// For a kind 3 with legacy JSON content, parses that map instead.
///
/// Entries of an oversized list are still returned; callers must not use them as write relays.
pub fn relay_list_of(ev: &RawEvent) -> (Vec<RelayListEntry>, bool) {
    let mut b = RelayListBuilder::default();
    let raw_count;
    match ev.kind {
        KIND_RELAY_LIST => {
            let r_tags: Vec<&Vec<String>> = ev.tags.iter().filter(|t| is_tag(t, "r")).collect();
            raw_count = r_tags.len();
            for t in r_tags {
                let Some(url) = t.get(1) else { continue };
                let marker = t.get(2).map(|m| m.trim().to_ascii_lowercase());
                let (read, write) = match marker.as_deref() {
                    None | Some("") => (true, true),
                    Some("read") => (true, false),
                    Some("write") => (false, true),
                    Some(_) => continue,
                };
                b.add(url, read, write);
            }
        }
        KIND_CONTACTS => {
            // Pre-NIP-65 map: {"wss://x/": {"read": true, "write": true}, ...}
            let Ok(serde_json::Value::Object(map)) =
                serde_json::from_str::<serde_json::Value>(ev.content.trim())
            else {
                return (Vec::new(), false);
            };
            raw_count = map.len();
            for (url, flags) in &map {
                let (read, write) = match flags {
                    serde_json::Value::Object(f) => {
                        let flag = |k: &str| f.get(k).and_then(|v| v.as_bool()).unwrap_or(false);
                        (flag("read"), flag("write"))
                    }
                    _ => (true, true),
                };
                b.add(url, read, write);
            }
        }
        _ => return (Vec::new(), false),
    }
    (b.entries, raw_count > MAX_RELAY_LIST)
}

/// Display name from a kind 0 (name | display_name), if any.
pub fn profile_name(ev: &RawEvent) -> Option<String> {
    if ev.kind != KIND_PROFILE {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(&ev.content).ok()?;
    ["name", "display_name", "displayName"]
        .iter()
        .filter_map(|k| v.get(*k)?.as_str())
        .map(str::trim)
        .find(|s| !s.is_empty())
        .map(|s| s.chars().take(100).collect())
}

//! In-memory Store with just what ingest and the crawler need; the rest returns defaults.

use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;

use magnetar_core::Result;
use magnetar_core::model::*;
use magnetar_core::query::{SearchPage, SearchQuery};
use magnetar_core::store::Store;

#[derive(Default)]
pub struct Inner {
    pub events: HashMap<String, RawEvent>,
    pub torrents: HashMap<String, ParsedTorrent>,
    pub comments: HashMap<String, Comment>,
    /// (pubkey, kind, d) -> newest event.
    pub replaceable: HashMap<(String, u16, String), RawEvent>,
    pub reports: HashMap<String, RawEvent>,
    pub relays: BTreeMap<String, RelayInfo>,
    pub cursors: HashMap<(String, u16), Cursor>,
    pub settings: HashMap<String, String>,
    /// Every put_cursor call, in order.
    pub cursor_writes: Vec<Cursor>,
}

#[derive(Default)]
pub struct MemStore {
    pub inner: Mutex<Inner>,
}

impl MemStore {
    pub fn with<R>(&self, f: impl FnOnce(&mut Inner) -> R) -> R {
        f(&mut self.inner.lock().unwrap())
    }
    pub fn torrent_count(&self) -> usize {
        self.with(|i| i.torrents.len())
    }
    pub fn comment_count(&self) -> usize {
        self.with(|i| i.comments.len())
    }
    pub fn cursor(&self, relay: &str, kind: u16) -> Option<Cursor> {
        self.with(|i| i.cursors.get(&(relay.to_string(), kind)).cloned())
    }
    pub fn relay(&self, url: &str) -> Option<RelayInfo> {
        self.with(|i| i.relays.get(url).cloned())
    }
}

fn d_tag(raw: &RawEvent) -> String {
    raw.tag("d")
        .and_then(|t| t.get(1))
        .cloned()
        .unwrap_or_default()
}

const MAX_RELAY_LIST: usize = 50;

#[async_trait::async_trait]
impl Store for MemStore {
    async fn put_torrent(&self, t: &ParsedTorrent, raw: &RawEvent) -> Result<bool> {
        self.with(|i| {
            if i.torrents.contains_key(&raw.id) {
                return Ok(false);
            }
            i.events.insert(raw.id.clone(), raw.clone());
            i.torrents.insert(raw.id.clone(), t.clone());
            Ok(true)
        })
    }
    async fn put_comment(&self, c: &Comment, raw: &RawEvent) -> Result<bool> {
        self.with(|i| {
            if i.comments.contains_key(&raw.id) {
                return Ok(false);
            }
            i.events.insert(raw.id.clone(), raw.clone());
            i.comments.insert(raw.id.clone(), c.clone());
            Ok(true)
        })
    }
    async fn put_replaceable(&self, raw: &RawEvent) -> Result<bool> {
        self.with(|i| {
            let key = (raw.pubkey.clone(), raw.kind, d_tag(raw));
            let newer = match i.replaceable.get(&key) {
                None => true,
                Some(old) => {
                    raw.created_at > old.created_at
                        || (raw.created_at == old.created_at && raw.id < old.id)
                }
            };
            if newer {
                i.events.insert(raw.id.clone(), raw.clone());
                i.replaceable.insert(key, raw.clone());
            }
            Ok(newer)
        })
    }
    async fn put_report(&self, raw: &RawEvent) -> Result<bool> {
        self.with(|i| {
            i.events.insert(raw.id.clone(), raw.clone());
            Ok(i.reports.insert(raw.id.clone(), raw.clone()).is_none())
        })
    }
    async fn has_event(&self, id: &str) -> Result<bool> {
        self.with(|i| Ok(i.events.contains_key(id)))
    }
    async fn known_torrent_event(&self, id: &str) -> Result<bool> {
        self.with(|i| Ok(i.torrents.contains_key(id)))
    }
    async fn get_raw_events(&self, ids: &[String]) -> Result<Vec<RawEvent>> {
        self.with(|i| {
            Ok(ids
                .iter()
                .filter_map(|id| i.events.get(id).cloned())
                .collect())
        })
    }

    async fn upsert_relay(&self, url: &str, source: &str) -> Result<()> {
        self.with(|i| {
            i.relays
                .entry(url.to_string())
                .or_insert_with(|| RelayInfo {
                    url: url.to_string(),
                    source: source.to_string(),
                    enabled: true,
                    last_ok: None,
                    last_err: None,
                    consecutive_failures: 0,
                    next_try_at: 0,
                    events_found: 0,
                    supports_neg: None,
                });
            Ok(())
        })
    }
    async fn list_relays(&self) -> Result<Vec<RelayInfo>> {
        self.with(|i| Ok(i.relays.values().cloned().collect()))
    }
    async fn record_relay_result(
        &self,
        url: &str,
        ok: bool,
        err: Option<&str>,
        found: u64,
        now: i64,
    ) -> Result<()> {
        self.upsert_relay(url, "manual").await?;
        self.with(|i| {
            let r = i.relays.get_mut(url).unwrap();
            r.events_found += found;
            if ok {
                r.consecutive_failures = 0;
                r.last_ok = Some(now);
                r.next_try_at = now;
            } else {
                r.consecutive_failures += 1;
                r.last_err = err.map(str::to_string);
                let backoff = (60i64 << r.consecutive_failures.min(12)).min(86_400);
                r.next_try_at = now + backoff;
            }
            Ok(())
        })
    }
    async fn set_relay_enabled(&self, url: &str, enabled: bool) -> Result<()> {
        self.with(|i| {
            if let Some(r) = i.relays.get_mut(url) {
                r.enabled = enabled;
            }
            Ok(())
        })
    }
    async fn set_relay_supports_neg(&self, url: &str, supports: bool) -> Result<()> {
        self.with(|i| {
            if let Some(r) = i.relays.get_mut(url) {
                r.supports_neg = Some(supports);
            }
            Ok(())
        })
    }
    async fn get_cursor(&self, relay: &str, kind: u16) -> Result<Option<Cursor>> {
        Ok(self.cursor(relay, kind))
    }
    async fn put_cursor(&self, c: &Cursor) -> Result<()> {
        self.with(|i| {
            i.cursor_writes.push(c.clone());
            i.cursors.insert((c.relay.clone(), c.kind), c.clone());
            Ok(())
        })
    }
    async fn publishers(&self, limit: usize) -> Result<Vec<String>> {
        self.with(|i| {
            let mut counts: HashMap<&str, usize> = HashMap::new();
            for t in i.torrents.values() {
                *counts.entry(t.pubkey.as_str()).or_default() += 1;
            }
            let mut v: Vec<(&str, usize)> = counts.into_iter().collect();
            v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
            Ok(v.into_iter()
                .take(limit)
                .map(|(p, _)| p.to_string())
                .collect())
        })
    }
    async fn relay_lists_for(
        &self,
        pubkeys: &[String],
    ) -> Result<Vec<(String, String, bool, bool)>> {
        self.with(|i| {
            let mut out = Vec::new();
            for pk in pubkeys {
                let Some(ev) = i
                    .replaceable
                    .get(&(pk.clone(), KIND_RELAY_LIST, String::new()))
                else {
                    continue;
                };
                let rs: Vec<&Vec<String>> = ev
                    .tags
                    .iter()
                    .filter(|t| t.first().map(String::as_str) == Some("r") && t.len() >= 2)
                    .collect();
                if rs.len() > MAX_RELAY_LIST {
                    continue;
                }
                for t in rs {
                    let (read, write) = match t.get(2).map(|m| m.to_ascii_lowercase()).as_deref() {
                        None | Some("") => (true, true),
                        Some("read") => (true, false),
                        Some("write") => (false, true),
                        _ => continue,
                    };
                    out.push((pk.clone(), t[1].clone(), read, write));
                }
            }
            Ok(out)
        })
    }
    async fn relay_list_oversized(&self, pubkey: &str) -> Result<bool> {
        self.with(|i| {
            Ok(i.replaceable
                .get(&(pubkey.to_string(), KIND_RELAY_LIST, String::new()))
                .is_some_and(|ev| {
                    ev.tags
                        .iter()
                        .filter(|t| t.first().map(String::as_str) == Some("r"))
                        .count()
                        > MAX_RELAY_LIST
                }))
        })
    }

    async fn search(&self, _q: &SearchQuery, _now: i64) -> Result<SearchPage> {
        Ok(SearchPage {
            hits: Vec::new(),
            total: 0,
            source: "local".into(),
        })
    }
    async fn torrent_detail(&self, _infohash: &str, _now: i64) -> Result<Option<TorrentDetail>> {
        Ok(None)
    }
    async fn liveness_due(&self, _now: i64, _limit: usize) -> Result<Vec<(String, Vec<String>)>> {
        Ok(Vec::new())
    }
    async fn put_liveness(&self, _l: &Liveness) -> Result<()> {
        Ok(())
    }
    async fn get_liveness(&self, _infohash: &str) -> Result<Option<Liveness>> {
        Ok(None)
    }

    async fn follows_of(&self, pubkey: &str) -> Result<Vec<String>> {
        self.with(|i| {
            Ok(i.replaceable
                .get(&(pubkey.to_string(), KIND_CONTACTS, String::new()))
                .map(|ev| {
                    ev.tags
                        .iter()
                        .filter(|t| t.first().map(String::as_str) == Some("p"))
                        .filter_map(|t| t.get(1).cloned())
                        .filter(|p| is_hex32(p))
                        .collect()
                })
                .unwrap_or_default())
        })
    }
    async fn followers_among(
        &self,
        _targets: &[String],
        _among: &[String],
    ) -> Result<Vec<(String, Vec<String>)>> {
        Ok(Vec::new())
    }
    async fn mutes_by(&self, _muters: &[String]) -> Result<Vec<(String, String)>> {
        Ok(Vec::new())
    }
    async fn reports_by(&self, _reporters: &[String]) -> Result<Vec<(String, String, String)>> {
        Ok(Vec::new())
    }
    async fn comment_authors_on(
        &self,
        _publisher: &str,
        _authors: &[String],
    ) -> Result<Vec<String>> {
        Ok(Vec::new())
    }
    async fn publisher_stats(&self, _pubkey: &str) -> Result<PublisherStats> {
        Ok(PublisherStats::default())
    }
    async fn put_trust(&self, _s: &TrustScore) -> Result<()> {
        Ok(())
    }
    async fn get_trust(&self, _pubkey: &str) -> Result<Option<TrustScore>> {
        Ok(None)
    }
    async fn graph_version(&self) -> Result<i64> {
        Ok(0)
    }
    async fn bump_graph_version(&self) -> Result<i64> {
        Ok(1)
    }
    async fn set_override(&self, _pubkey: &str, _action: Option<OverrideAction>) -> Result<()> {
        Ok(())
    }
    async fn overrides(&self) -> Result<Vec<(String, OverrideAction)>> {
        Ok(Vec::new())
    }

    async fn get_setting(&self, key: &str) -> Result<Option<String>> {
        self.with(|i| Ok(i.settings.get(key).cloned()))
    }
    async fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        self.with(|i| {
            i.settings.insert(key.to_string(), value.to_string());
            Ok(())
        })
    }

    async fn list_saved_searches(&self) -> Result<Vec<SavedSearch>> {
        Ok(Vec::new())
    }
    async fn put_saved_search(&self, _s: &SavedSearch) -> Result<i64> {
        Ok(0)
    }
    async fn delete_saved_search(&self, _id: i64) -> Result<()> {
        Ok(())
    }
    async fn stats(&self) -> Result<StoreStats> {
        Ok(StoreStats::default())
    }
}

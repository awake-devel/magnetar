//! In-memory `Store` with just enough behaviour for trust / notify tests.
#![allow(dead_code)]

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use magnetar_core::Result;
use magnetar_core::model::*;
use magnetar_core::query::{SearchPage, SearchQuery};
use magnetar_core::store::Store;

#[derive(Default)]
pub struct Inner {
    pub settings: HashMap<String, String>,
    pub follows: HashMap<String, Vec<String>>,
    /// (muter, target)
    pub mutes: Vec<(String, String)>,
    /// (reporter, target, type)
    pub reports: Vec<(String, String, String)>,
    /// (publisher, comment author)
    pub comments: Vec<(String, String)>,
    pub stats: HashMap<String, PublisherStats>,
    pub oversized: HashSet<String>,
    pub trust: HashMap<String, TrustScore>,
    pub put_trust_calls: usize,
    pub graph_version: i64,
    pub overrides: HashMap<String, OverrideAction>,
    pub publishers: Vec<String>,
    pub saved: Vec<SavedSearch>,
    pub torrents: Vec<TorrentSummary>,
}

#[derive(Default)]
pub struct MemStore(pub Mutex<Inner>);

impl MemStore {
    pub fn with<R>(&self, f: impl FnOnce(&mut Inner) -> R) -> R {
        f(&mut self.0.lock().unwrap())
    }
}

pub fn summary(infohash: &str, title: &str, created_at: i64) -> TorrentSummary {
    TorrentSummary {
        infohash: infohash.into(),
        event_id: format!("{infohash:0<64}"),
        title: title.into(),
        category: Category::Other,
        total_size: None,
        file_count: 1,
        created_at,
        pubkey: "11".repeat(32),
        npub: String::new(),
        seeders: None,
        leechers: None,
        dead: None,
        trust: 50,
        pow_bits: 0,
        publisher_count: 1,
        source_url: None,
        rank: 0.0,
        pinned: false,
        hidden: false,
        media_title: None,
        media_year: None,
        media_kind: None,
        matched_file: None,
        media_id: None,
        media_rating: None,
        attrs: None,
        origin: Default::default(),
    }
}

const UNUSED: &str = "not used by trust/notify tests";

#[async_trait::async_trait]
impl Store for MemStore {
    async fn put_torrent(&self, _: &ParsedTorrent, _: &RawEvent) -> Result<bool> {
        unimplemented!("{UNUSED}")
    }
    async fn put_comment(&self, _: &Comment, _: &RawEvent) -> Result<bool> {
        unimplemented!("{UNUSED}")
    }
    async fn put_replaceable(&self, _: &RawEvent) -> Result<bool> {
        unimplemented!("{UNUSED}")
    }
    async fn put_report(&self, _: &RawEvent) -> Result<bool> {
        unimplemented!("{UNUSED}")
    }
    async fn has_event(&self, _: &str) -> Result<bool> {
        unimplemented!("{UNUSED}")
    }
    async fn known_torrent_event(&self, _: &str) -> Result<bool> {
        unimplemented!("{UNUSED}")
    }
    async fn get_raw_events(&self, _: &[String]) -> Result<Vec<RawEvent>> {
        unimplemented!("{UNUSED}")
    }
    async fn upsert_relay(&self, _: &str, _: &str) -> Result<()> {
        unimplemented!("{UNUSED}")
    }
    async fn list_relays(&self) -> Result<Vec<RelayInfo>> {
        unimplemented!("{UNUSED}")
    }
    async fn record_relay_result(
        &self,
        _: &str,
        _: bool,
        _: Option<&str>,
        _: u64,
        _: i64,
    ) -> Result<()> {
        unimplemented!("{UNUSED}")
    }
    async fn set_relay_enabled(&self, _: &str, _: bool) -> Result<()> {
        unimplemented!("{UNUSED}")
    }
    async fn set_relay_supports_neg(&self, _: &str, _: bool) -> Result<()> {
        unimplemented!("{UNUSED}")
    }
    async fn get_cursor(&self, _: &str, _: u16) -> Result<Option<Cursor>> {
        unimplemented!("{UNUSED}")
    }
    async fn put_cursor(&self, _: &Cursor) -> Result<()> {
        unimplemented!("{UNUSED}")
    }
    async fn publishers(&self, limit: usize) -> Result<Vec<String>> {
        Ok(self.with(|i| i.publishers.iter().take(limit).cloned().collect()))
    }
    async fn relay_lists_for(&self, _: &[String]) -> Result<Vec<(String, String, bool, bool)>> {
        unimplemented!("{UNUSED}")
    }
    async fn relay_list_oversized(&self, pubkey: &str) -> Result<bool> {
        Ok(self.with(|i| i.oversized.contains(pubkey)))
    }
    async fn search(&self, q: &SearchQuery, _now: i64) -> Result<SearchPage> {
        let text = q.text.to_lowercase();
        let mut hits: Vec<TorrentSummary> = self.with(|i| {
            i.torrents
                .iter()
                .filter(|t| text.is_empty() || t.title.to_lowercase().contains(&text))
                .filter(|t| q.since.is_none_or(|s| t.created_at >= s))
                .filter(|t| q.until.is_none_or(|u| t.created_at <= u))
                .cloned()
                .collect()
        });
        hits.sort_by_key(|t| std::cmp::Reverse(t.created_at));
        let total = hits.len() as u64;
        hits.truncate(q.effective_limit() as usize);
        Ok(SearchPage {
            hits,
            total,
            source: "local".into(),
            total_is_lower_bound: false,
        })
    }
    async fn torrent_detail(&self, _: &str, _: i64) -> Result<Option<TorrentDetail>> {
        unimplemented!("{UNUSED}")
    }
    async fn liveness_due(&self, _: i64, _: usize) -> Result<Vec<(String, Vec<String>)>> {
        unimplemented!("{UNUSED}")
    }
    async fn put_liveness(&self, _: &Liveness) -> Result<()> {
        unimplemented!("{UNUSED}")
    }
    async fn get_liveness(&self, _: &str) -> Result<Option<Liveness>> {
        unimplemented!("{UNUSED}")
    }
    async fn follows_of(&self, pubkey: &str) -> Result<Vec<String>> {
        Ok(self.with(|i| i.follows.get(pubkey).cloned().unwrap_or_default()))
    }
    async fn followers_among(
        &self,
        targets: &[String],
        among: &[String],
    ) -> Result<Vec<(String, Vec<String>)>> {
        Ok(self.with(|i| {
            targets
                .iter()
                .filter_map(|t| {
                    let f: Vec<String> = among
                        .iter()
                        .filter(|a| i.follows.get(*a).is_some_and(|l| l.contains(t)))
                        .cloned()
                        .collect();
                    (!f.is_empty()).then(|| (t.clone(), f))
                })
                .collect()
        }))
    }
    async fn mutes_by(&self, muters: &[String]) -> Result<Vec<(String, String)>> {
        Ok(self.with(|i| {
            i.mutes
                .iter()
                .filter(|(m, _)| muters.contains(m))
                .cloned()
                .collect()
        }))
    }
    async fn reports_by(&self, reporters: &[String]) -> Result<Vec<(String, String, String)>> {
        Ok(self.with(|i| {
            i.reports
                .iter()
                .filter(|(r, _, _)| reporters.contains(r))
                .cloned()
                .collect()
        }))
    }
    async fn comment_authors_on(&self, publisher: &str, authors: &[String]) -> Result<Vec<String>> {
        Ok(self.with(|i| {
            let mut v: Vec<String> = i
                .comments
                .iter()
                .filter(|(p, a)| p == publisher && authors.contains(a))
                .map(|(_, a)| a.clone())
                .collect();
            v.sort();
            v.dedup();
            v
        }))
    }
    async fn publisher_stats(&self, pubkey: &str) -> Result<PublisherStats> {
        Ok(self.with(|i| i.stats.get(pubkey).cloned().unwrap_or_default()))
    }
    async fn put_trust(&self, s: &TrustScore) -> Result<()> {
        self.with(|i| {
            i.put_trust_calls += 1;
            i.trust.insert(s.pubkey.clone(), s.clone());
        });
        Ok(())
    }
    async fn get_trust(&self, pubkey: &str) -> Result<Option<TrustScore>> {
        Ok(self.with(|i| i.trust.get(pubkey).cloned()))
    }
    async fn graph_version(&self) -> Result<i64> {
        Ok(self.with(|i| i.graph_version))
    }
    async fn bump_graph_version(&self) -> Result<i64> {
        Ok(self.with(|i| {
            i.graph_version += 1;
            i.graph_version
        }))
    }
    async fn set_override(&self, pubkey: &str, action: Option<OverrideAction>) -> Result<()> {
        self.with(|i| match action {
            Some(a) => i.overrides.insert(pubkey.into(), a),
            None => i.overrides.remove(pubkey),
        });
        Ok(())
    }
    async fn overrides(&self) -> Result<Vec<(String, OverrideAction)>> {
        Ok(self.with(|i| i.overrides.iter().map(|(k, v)| (k.clone(), *v)).collect()))
    }
    async fn get_setting(&self, key: &str) -> Result<Option<String>> {
        Ok(self.with(|i| i.settings.get(key).cloned()))
    }
    async fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        self.with(|i| i.settings.insert(key.into(), value.into()));
        Ok(())
    }
    async fn list_saved_searches(&self) -> Result<Vec<SavedSearch>> {
        Ok(self.with(|i| i.saved.clone()))
    }
    async fn put_saved_search(&self, s: &SavedSearch) -> Result<i64> {
        Ok(self.with(|i| {
            let mut s = s.clone();
            if s.id == 0 {
                s.id = i.saved.iter().map(|x| x.id).max().unwrap_or(0) + 1;
            }
            let id = s.id;
            match i.saved.iter_mut().find(|x| x.id == id) {
                Some(x) => *x = s,
                None => i.saved.push(s),
            }
            id
        }))
    }
    async fn delete_saved_search(&self, id: i64) -> Result<()> {
        self.with(|i| i.saved.retain(|s| s.id != id));
        Ok(())
    }
    async fn stats(&self) -> Result<StoreStats> {
        unimplemented!("{UNUSED}")
    }
}

//! Hand-built events for store tests (nip35/ingest belong to another agent; no signatures needed
//! because the store never verifies).
#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use magnetar_core::model::*;
use magnetar_core::query::{SearchQuery, Sort};
use magnetar_ffi::SqliteStore;

static SEQ: AtomicU64 = AtomicU64::new(1);

pub fn hex64(n: u64) -> String {
    format!("{n:064x}")
}

pub fn ih(n: u64) -> String {
    format!("{n:040x}")
}

pub fn pk(n: u64) -> String {
    format!("{:064x}", 0xabc0_0000 + n)
}

/// Fresh, unique event id.
pub fn next_id() -> String {
    hex64(0x1000_0000 + SEQ.fetch_add(1, Ordering::Relaxed))
}

pub struct T {
    pub parsed: ParsedTorrent,
    pub raw: RawEvent,
}

pub fn torrent(infohash: &str, title: &str, pubkey: &str, created_at: i64) -> T {
    let id = next_id();
    let parsed = ParsedTorrent {
        event_id: id.clone(),
        pubkey: pubkey.into(),
        created_at,
        infohash: infohash.into(),
        infohash_v2: None,
        title: title.into(),
        description: String::new(),
        files: vec![TorrentFile {
            path: format!("{title}.mkv"),
            size: Some(1000),
        }],
        total_size: Some(1000),
        trackers: vec!["udp://tracker.opentrackr.org:1337/announce".into()],
        category: Category::Video,
        category_path: vec!["video".into()],
        hashtags: vec![],
        external_ids: vec![],
        source_url: None,
        published_at: None,
        pow_bits: 0,
        pow_committed: None,
    };
    T {
        raw: raw_for(&parsed),
        parsed,
    }
}

pub fn raw_for(p: &ParsedTorrent) -> RawEvent {
    RawEvent {
        id: p.event_id.clone(),
        pubkey: p.pubkey.clone(),
        created_at: p.created_at,
        kind: KIND_TORRENT,
        tags: vec![
            vec!["title".into(), p.title.clone()],
            vec!["x".into(), p.infohash.clone()],
        ],
        content: p.description.clone(),
        sig: "00".repeat(64),
    }
}

impl T {
    pub fn with(mut self, f: impl FnOnce(&mut ParsedTorrent)) -> Self {
        f(&mut self.parsed);
        self.raw = raw_for(&self.parsed);
        self
    }
    pub fn put(&self, s: &SqliteStore) -> bool {
        s.put_torrent_sync(&self.parsed, &self.raw).unwrap()
    }
    pub fn id(&self) -> String {
        self.parsed.event_id.clone()
    }
}

pub fn comment(on: &str, author: &str, created_at: i64, text: &str) -> (Comment, RawEvent) {
    let id = next_id();
    let c = Comment {
        event_id: id.clone(),
        pubkey: author.into(),
        created_at,
        kind: KIND_TORRENT_COMMENT,
        torrent_event_id: on.into(),
        content: text.into(),
    };
    let raw = RawEvent {
        id,
        pubkey: author.into(),
        created_at,
        kind: KIND_TORRENT_COMMENT,
        tags: vec![vec!["e".into(), on.into()]],
        content: text.into(),
        sig: "00".repeat(64),
    };
    (c, raw)
}

pub fn raw(kind: u16, pubkey: &str, created_at: i64, tags: Vec<Vec<&str>>) -> RawEvent {
    RawEvent {
        id: next_id(),
        pubkey: pubkey.into(),
        created_at,
        kind,
        tags: tags
            .into_iter()
            .map(|t| t.into_iter().map(String::from).collect())
            .collect(),
        content: String::new(),
        sig: "00".repeat(64),
    }
}

/// Query that sees everything (trust 0) sorted by date desc.
pub fn q(text: &str) -> SearchQuery {
    SearchQuery {
        text: text.into(),
        min_trust: Some(0),
        sort: Sort::Relevance,
        descending: true,
        ..Default::default()
    }
}

pub fn titles(s: &SqliteStore, q: &SearchQuery) -> Vec<String> {
    s.search_sync(q, 1_000_000)
        .unwrap()
        .hits
        .into_iter()
        .map(|h| h.title)
        .collect()
}

pub fn liveness(infohash: &str, seeders: Option<u32>, checked_at: i64, ttl: i64) -> Liveness {
    Liveness {
        infohash: infohash.into(),
        seeders,
        leechers: seeders.map(|_| 0),
        completed: None,
        dht_peers_est: None,
        source: LivenessSource::Udp,
        trackers_ok: 1,
        trackers_failed: 0,
        checked_at,
        expires_at: checked_at + ttl,
    }
}

/// Unique temp dir removed on drop.
pub struct TempDir(pub PathBuf);

impl TempDir {
    pub fn new(tag: &str) -> Self {
        let p = std::env::temp_dir().join(format!(
            "magnetar-ffi-{tag}-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&p).unwrap();
        TempDir(p)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

//! Crawler (D8–D10). Owner: nostr agent. Generic over Store; used by server daemon and phone.

use std::time::Duration;

use crate::store::Store;

pub const DEFAULT_SEED_RELAYS: &[&str] = &[
    "wss://nostr.oxtr.dev",
    "wss://relay.nostr.net",
    "wss://nos.lol",
    "wss://relay.primal.net",
    "wss://relay.damus.io",
    "wss://relay.snort.social",
    "wss://nostr.mom",
    "wss://nostr21.com",
    "wss://nostr.wine",
    "wss://nostr.land",
    "wss://offchain.pub",
    "wss://nostr.bitcoiner.social",
];
pub const DEFAULT_INDEXERS: &[&str] = &[
    "wss://purplepag.es",
    "wss://user.kindpag.es",
    "wss://indexer.coracle.social",
];

#[derive(Clone, Debug)]
pub struct CrawlConfig {
    pub seed_relays: Vec<String>,
    pub indexers: Vec<String>,
    /// Max relays touched per run (server 30, phone 8).
    pub max_relays: usize,
    /// Wall-clock budget for one run.
    pub budget: Duration,
    /// Max pages per relay per run for backfill.
    pub max_backfill_pages: usize,
    pub page_limit: usize,
    pub overlap_secs: i64,
    /// Relays crawled concurrently.
    pub concurrency: usize,
    /// Also fetch kinds 0/3/10000/10002/1984/30007 for root + D1 + publishers from indexers.
    pub fetch_metadata: bool,
    /// Trust root (hex) whose follow graph is fetched when fetch_metadata.
    pub root: Option<String>,
}

impl CrawlConfig {
    pub fn server_default() -> Self {
        Self {
            seed_relays: DEFAULT_SEED_RELAYS.iter().map(|s| s.to_string()).collect(),
            indexers: DEFAULT_INDEXERS.iter().map(|s| s.to_string()).collect(),
            max_relays: 30,
            budget: Duration::from_secs(600),
            max_backfill_pages: 200,
            page_limit: 500,
            overlap_secs: 3600,
            concurrency: 6,
            fetch_metadata: true,
            root: None,
        }
    }
    pub fn phone_default() -> Self {
        Self {
            max_relays: 8,
            budget: Duration::from_secs(120),
            max_backfill_pages: 10,
            concurrency: 3,
            ..Self::server_default()
        }
    }
}

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct CrawlReport {
    pub relays_tried: u32,
    pub relays_ok: u32,
    pub events_seen: u64,
    pub inserted: u64,
    pub duplicates: u64,
    pub rejected: u64,
    pub comments: u64,
    pub metadata_events: u64,
    pub duration_ms: u64,
    pub errors: Vec<String>,
}

/// One crawl pass: relay discovery, forward sweep from cursors, bounded backfill, comments for new
/// torrents, optional metadata. Must respect `budget` and never panic on relay misbehaviour.
pub async fn crawl_once<S: Store + ?Sized>(
    store: &S,
    cfg: &CrawlConfig,
) -> crate::Result<CrawlReport> {
    let _ = (store, cfg);
    todo!("crawler::crawl_once")
}

//! Storage abstraction implemented by `PgStore` (server) and `SqliteStore` (ffi). ARCHITECTURE.md §4.

use crate::error::Result;
use crate::model::*;
use crate::query::{SearchPage, SearchQuery};

/// Setting keys (values are JSON-encoded strings).
pub mod keys {
    pub const TRUST_ROOT: &str = "trust.root";
    pub const TRUST_THRESHOLD: &str = "trust.threshold";
    pub const SEED_RELAYS: &str = "crawl.seed_relays";
    pub const LIVENESS_ENABLED: &str = "liveness.enabled";
    pub const NTFY_URL: &str = "ntfy.url";
    pub const NTFY_TOPIC: &str = "ntfy.topic";
}

pub const DEFAULT_TRUST_THRESHOLD: u8 = 20;

#[async_trait::async_trait]
pub trait Store: Send + Sync {
    // ---- ingest (idempotent; true = newly inserted). Inputs are already verified by core::ingest.
    async fn put_torrent(&self, t: &ParsedTorrent, raw: &RawEvent) -> Result<bool>;
    async fn put_comment(&self, c: &Comment, raw: &RawEvent) -> Result<bool>;
    /// Kinds 0/3/10000/30007/10002: keep newest per (pubkey, kind[, d]) — lowest id wins a tie —
    /// and rebuild derived rows (profiles / follows / mutes / relay_lists) in one transaction.
    async fn put_replaceable(&self, raw: &RawEvent) -> Result<bool>;
    async fn put_report(&self, raw: &RawEvent) -> Result<bool>;
    async fn has_event(&self, id: &str) -> Result<bool>;
    async fn known_torrent_event(&self, id: &str) -> Result<bool>;
    async fn get_raw_events(&self, ids: &[String]) -> Result<Vec<RawEvent>>;

    // ---- relays + cursors
    /// Insert if missing. Never changes `enabled` or a "manual" source of an existing row.
    async fn upsert_relay(&self, url: &str, source: &str) -> Result<()>;
    async fn list_relays(&self) -> Result<Vec<RelayInfo>>;
    /// ok: reset failures, set last_ok; !ok: failures+1, next_try_at = now + backoff(failures) (max 24 h).
    async fn record_relay_result(
        &self,
        url: &str,
        ok: bool,
        err: Option<&str>,
        found: u64,
        now: i64,
    ) -> Result<()>;
    async fn set_relay_enabled(&self, url: &str, enabled: bool) -> Result<()>;
    async fn set_relay_supports_neg(&self, url: &str, supports: bool) -> Result<()>;
    async fn get_cursor(&self, relay: &str, kind: u16) -> Result<Option<Cursor>>;
    async fn put_cursor(&self, c: &Cursor) -> Result<()>;
    /// Distinct pubkeys with ≥1 torrent, most torrents first.
    async fn publishers(&self, limit: usize) -> Result<Vec<String>>;
    /// (pubkey, url, read, write) from stored kind-10002 lists (non-oversized only).
    async fn relay_lists_for(
        &self,
        pubkeys: &[String],
    ) -> Result<Vec<(String, String, bool, bool)>>;
    async fn relay_list_oversized(&self, pubkey: &str) -> Result<bool>;

    // ---- search / detail
    async fn search(&self, q: &SearchQuery, now: i64) -> Result<SearchPage>;
    async fn torrent_detail(&self, infohash: &str, now: i64) -> Result<Option<TorrentDetail>>;

    // ---- liveness
    /// Infohashes whose liveness is missing or expired, newest torrents first, with their trackers.
    async fn liveness_due(&self, now: i64, limit: usize) -> Result<Vec<(String, Vec<String>)>>;
    async fn put_liveness(&self, l: &Liveness) -> Result<()>;
    async fn get_liveness(&self, infohash: &str) -> Result<Option<Liveness>>;

    // ---- trust graph
    async fn follows_of(&self, pubkey: &str) -> Result<Vec<String>>;
    /// For each target: which of `among` follow it (targets with no such follower are omitted).
    async fn followers_among(
        &self,
        targets: &[String],
        among: &[String],
    ) -> Result<Vec<(String, Vec<String>)>>;
    /// (muter, target) for public mutes (10000, 30007 d=2003) by the given muters.
    async fn mutes_by(&self, muters: &[String]) -> Result<Vec<(String, String)>>;
    /// (reporter, target, report_type) for kind-1984 reports by the given reporters.
    async fn reports_by(&self, reporters: &[String]) -> Result<Vec<(String, String, String)>>;
    /// Authors (among `authors`) who commented on torrents published by `publisher`.
    async fn comment_authors_on(&self, publisher: &str, authors: &[String]) -> Result<Vec<String>>;
    async fn publisher_stats(&self, pubkey: &str) -> Result<PublisherStats>;
    /// Store the publisher score and refresh denormalised `trust = clamp(score + pow_bonus)` on its torrents.
    async fn put_trust(&self, s: &TrustScore) -> Result<()>;
    async fn get_trust(&self, pubkey: &str) -> Result<Option<TrustScore>>;
    /// Bumped whenever the root's or a D1 member's kind 3/10000 head changes, or the root changes.
    async fn graph_version(&self) -> Result<i64>;
    async fn bump_graph_version(&self) -> Result<i64>;
    async fn set_override(&self, pubkey: &str, action: Option<OverrideAction>) -> Result<()>;
    async fn overrides(&self) -> Result<Vec<(String, OverrideAction)>>;

    // ---- settings
    async fn get_setting(&self, key: &str) -> Result<Option<String>>;
    async fn set_setting(&self, key: &str, value: &str) -> Result<()>;

    // ---- saved searches
    async fn list_saved_searches(&self) -> Result<Vec<SavedSearch>>;
    /// id 0 = insert; returns the id.
    async fn put_saved_search(&self, s: &SavedSearch) -> Result<i64>;
    async fn delete_saved_search(&self, id: i64) -> Result<()>;

    async fn stats(&self) -> Result<StoreStats>;
}

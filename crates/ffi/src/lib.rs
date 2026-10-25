//! magnetar-ffi: SQLite store + uniffi API for the Android app (ARCHITECTURE.md §8.1).
//!
//! This file is the Kotlin-facing CONTRACT: names and fields here become `app.magnetar.ffi.*`.
//! Bodies are filled by the ffi agent; signatures change only together with the Android app.

uniffi::setup_scaffolding!();

use std::sync::Arc;

#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum FfiError {
    #[error("database: {msg}")]
    Db { msg: String },
    #[error("network: {msg}")]
    Network { msg: String },
    #[error("invalid: {msg}")]
    Invalid { msg: String },
    #[error("server {status}: {msg}")]
    Server { status: u16, msg: String },
    #[error("{msg}")]
    Other { msg: String },
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct FfiConfig {
    /// e.g. "http://127.0.0.1:8787"; None = offline only.
    pub server_url: Option<String>,
    pub api_token: Option<String>,
    /// npub / hex / nprofile.
    pub root_pubkey: Option<String>,
    pub trust_threshold: u8,
    pub cache_cap_mb: u32,
    /// Empty = core defaults.
    pub seed_relays: Vec<String>,
    /// App-private dir for phone downloads (getExternalFilesDir(DOWNLOADS)/magnetar).
    pub download_dir: String,
    /// App-private dir for librqbit session + DHT state.
    pub state_dir: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum FfiCategory {
    Video,
    Audio,
    Application,
    Game,
    Porn,
    Anime,
    Book,
    Picture,
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum FfiSort {
    Relevance,
    Date,
    Size,
    Seeders,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct FfiSearchQuery {
    pub text: String,
    pub categories: Vec<FfiCategory>,
    pub min_size: Option<u64>,
    pub max_size: Option<u64>,
    pub since: Option<i64>,
    pub until: Option<i64>,
    pub min_seeders: Option<u32>,
    pub hide_dead: bool,
    /// None = config trust_threshold.
    pub min_trust: Option<u8>,
    pub sort: FfiSort,
    pub descending: bool,
    pub limit: u32,
    pub offset: u32,
    pub include_hidden: bool,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct FfiTorrentSummary {
    pub infohash: String,
    pub event_id: String,
    pub title: String,
    pub category: FfiCategory,
    pub total_size: Option<u64>,
    pub file_count: u32,
    pub created_at: i64,
    pub pubkey: String,
    pub npub: String,
    pub seeders: Option<u32>,
    pub leechers: Option<u32>,
    /// None = unknown.
    pub dead: Option<bool>,
    pub trust: u8,
    pub pow_bits: u8,
    pub publisher_count: u32,
    pub source_url: Option<String>,
    pub rank: f32,
    pub pinned: bool,
    pub hidden: bool,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct FfiSearchPage {
    pub hits: Vec<FfiTorrentSummary>,
    pub total: u64,
    /// "server" | "local"
    pub source: String,
    /// Set when the server was preferred but failed (UI shows "offline results").
    pub server_error: Option<String>,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct FfiFile {
    pub path: String,
    pub size: Option<u64>,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct FfiKv {
    pub key: String,
    pub value: String,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct FfiLiveness {
    pub seeders: Option<u32>,
    pub leechers: Option<u32>,
    pub completed: Option<u32>,
    pub dht_peers_est: Option<u32>,
    /// "udp" | "http" | "dht" | "dht_values" | "none"
    pub source: String,
    pub trackers_ok: u16,
    pub trackers_failed: u16,
    pub checked_at: i64,
    pub expires_at: i64,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct FfiTrustFactor {
    pub code: String,
    pub delta: i32,
    pub detail: String,
    pub evidence: Vec<String>,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct FfiTrust {
    pub pubkey: String,
    pub score: u8,
    pub factors: Vec<FfiTrustFactor>,
    pub root: Option<String>,
    pub computed_at: i64,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct FfiEventRef {
    pub event_id: String,
    pub pubkey: String,
    pub npub: String,
    pub created_at: i64,
    pub title: String,
    pub trust: u8,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct FfiComment {
    pub event_id: String,
    pub pubkey: String,
    pub npub: String,
    pub created_at: i64,
    pub content: String,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct FfiTorrentDetail {
    pub summary: FfiTorrentSummary,
    pub description: String,
    pub files: Vec<FfiFile>,
    pub trackers: Vec<String>,
    pub hashtags: Vec<String>,
    pub category_path: Vec<String>,
    pub external_ids: Vec<FfiKv>,
    pub magnet: String,
    pub liveness: Option<FfiLiveness>,
    pub trust: FfiTrust,
    pub events: Vec<FfiEventRef>,
    pub comments: Vec<FfiComment>,
    /// "server" | "local"
    pub source: String,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct FfiCrawlReport {
    pub relays_tried: u32,
    pub relays_ok: u32,
    pub events_seen: u64,
    pub inserted: u64,
    pub duplicates: u64,
    pub rejected: u64,
    pub duration_ms: u64,
    pub errors: Vec<String>,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct FfiSyncReport {
    /// "negentropy" | "batch" | "none"
    pub method: String,
    pub have: u64,
    pub uploaded: u64,
    pub duplicates: u64,
    pub failed: u64,
    pub error: Option<String>,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct FfiCleanupReport {
    pub evicted: u64,
    pub bytes_before: u64,
    pub bytes_after: u64,
    pub unsynced_kept: u64,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct FfiSavedSearch {
    /// 0 = new.
    pub id: i64,
    pub name: String,
    pub query: FfiSearchQuery,
    pub notify: bool,
    pub last_checked: Option<i64>,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct FfiSavedSearchHit {
    pub search: FfiSavedSearch,
    pub new_hits: Vec<FfiTorrentSummary>,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct FfiStats {
    pub torrents: u64,
    pub events: u64,
    pub publishers: u64,
    pub comments: u64,
    pub relays: u64,
    pub unsynced: u64,
    pub db_bytes: u64,
    pub last_crawl: Option<i64>,
    pub last_sync: Option<i64>,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct FfiRelay {
    pub url: String,
    pub source: String,
    pub enabled: bool,
    pub last_ok: Option<i64>,
    pub last_err: Option<String>,
    pub events_found: u64,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct FfiDownload {
    pub id: u64,
    pub name: String,
    pub infohash: String,
    /// "initializing" | "live" | "paused" | "error" | "finished"
    pub state: String,
    pub progress: f32,
    pub downloaded: u64,
    pub total: u64,
    pub down_bps: u64,
    pub up_bps: u64,
    pub peers: u32,
    pub error: Option<String>,
    pub output_dir: String,
}

/// One per process. Thread-safe; all heavy work runs on an internal tokio runtime.
#[derive(uniffi::Object)]
pub struct Magnetar {
    _private: (),
}

#[uniffi::export(async_runtime = "tokio")]
impl Magnetar {
    #[uniffi::constructor]
    pub fn open(db_path: String, config: FfiConfig) -> Result<Arc<Self>, FfiError> {
        let _ = (db_path, config);
        todo!()
    }
    pub fn set_config(&self, config: FfiConfig) -> Result<(), FfiError> {
        let _ = config;
        todo!()
    }
    pub fn config(&self) -> FfiConfig {
        todo!()
    }
    /// Server first when `prefer_server` and configured; local FTS5 fallback.
    pub async fn search(
        &self,
        query: FfiSearchQuery,
        prefer_server: bool,
    ) -> Result<FfiSearchPage, FfiError> {
        let _ = (query, prefer_server);
        todo!()
    }
    pub async fn detail(
        &self,
        infohash: String,
        prefer_server: bool,
    ) -> Result<Option<FfiTorrentDetail>, FfiError> {
        let _ = (infohash, prefer_server);
        todo!()
    }
    pub async fn crawl_once(&self, max_seconds: u32) -> Result<FfiCrawlReport, FfiError> {
        let _ = max_seconds;
        todo!()
    }
    pub async fn sync_now(&self) -> Result<FfiSyncReport, FfiError> {
        todo!()
    }
    pub fn cleanup(&self) -> Result<FfiCleanupReport, FfiError> {
        todo!()
    }
    /// Local liveness check (trackers + DHT); also stored in the cache.
    pub async fn check_liveness(&self, infohash: String) -> Result<Option<FfiLiveness>, FfiError> {
        let _ = infohash;
        todo!()
    }
    pub fn set_pinned(&self, infohash: String, pinned: bool) -> Result<(), FfiError> {
        let _ = (infohash, pinned);
        todo!()
    }
    pub fn set_hidden(&self, infohash: String, hidden: bool) -> Result<(), FfiError> {
        let _ = (infohash, hidden);
        todo!()
    }
    /// Local override + server override when reachable.
    pub async fn block_publisher(&self, pubkey: String, blocked: bool) -> Result<(), FfiError> {
        let _ = (pubkey, blocked);
        todo!()
    }
    pub fn saved_searches(&self) -> Result<Vec<FfiSavedSearch>, FfiError> {
        todo!()
    }
    pub fn save_search(&self, search: FfiSavedSearch) -> Result<i64, FfiError> {
        let _ = search;
        todo!()
    }
    pub fn delete_saved_search(&self, id: i64) -> Result<(), FfiError> {
        let _ = id;
        todo!()
    }
    /// New hits since the last poll for notify=true searches (server if reachable, else local).
    pub async fn poll_saved_searches(&self) -> Result<Vec<FfiSavedSearchHit>, FfiError> {
        todo!()
    }
    pub fn stats(&self) -> Result<FfiStats, FfiError> {
        todo!()
    }
    pub fn relays(&self) -> Result<Vec<FfiRelay>, FfiError> {
        todo!()
    }
    pub fn set_relay_enabled(&self, url: String, enabled: bool) -> Result<(), FfiError> {
        let _ = (url, enabled);
        todo!()
    }
    pub async fn server_health(&self) -> Result<bool, FfiError> {
        todo!()
    }
    /// Phone engine (on_server=false) or server engine. Phone errors with Invalid if librqbit is unavailable.
    pub async fn download_start(
        &self,
        magnet: String,
        on_server: bool,
    ) -> Result<FfiDownload, FfiError> {
        let _ = (magnet, on_server);
        todo!()
    }
    pub async fn downloads(&self, on_server: bool) -> Result<Vec<FfiDownload>, FfiError> {
        let _ = on_server;
        todo!()
    }
    /// action: "pause" | "resume" | "delete"
    pub async fn download_action(
        &self,
        id: u64,
        action: String,
        on_server: bool,
    ) -> Result<(), FfiError> {
        let _ = (id, action, on_server);
        todo!()
    }
    /// Pause every phone download and persist (foreground-service onTimeout).
    pub async fn pause_all_downloads(&self) {
        todo!()
    }
}

#[uniffi::export]
pub fn npub_encode(hex: String) -> Result<String, FfiError> {
    let _ = hex;
    todo!()
}

/// npub / hex / nprofile / nostr: URI -> hex.
#[uniffi::export]
pub fn pubkey_decode(input: String) -> Result<String, FfiError> {
    let _ = input;
    todo!()
}

#[uniffi::export]
pub fn build_magnet(infohash: String, name: Option<String>, trackers: Vec<String>) -> String {
    let _ = (infohash, name, trackers);
    todo!()
}

#[uniffi::export]
pub fn version() -> String {
    magnetar_core::VERSION.to_string()
}

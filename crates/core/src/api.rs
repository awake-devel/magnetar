//! HTTP API DTOs + client (ARCHITECTURE.md §6). Owner: trust-sync agent (DTOs are contracts).

use serde::{Deserialize, Serialize};

use crate::model::*;
use crate::query::{SearchPage, SearchQuery};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Health {
    pub status: String,
    pub version: String,
    pub db: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ApiErrorBody {
    pub error: String,
    pub message: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct BatchRequest {
    pub events: Vec<RawEvent>,
}
pub const MAX_BATCH: usize = 500;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct BatchResult {
    pub id: Hex32,
    pub accepted: bool,
    pub duplicate: bool,
    pub message: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct BatchResponse {
    pub results: Vec<BatchResult>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct CrawlerStatus {
    pub running: bool,
    pub last_run: Option<i64>,
    pub last_report: Option<crate::crawler::CrawlReport>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct StatsResponse {
    #[serde(flatten)]
    pub store: StoreStats,
    pub crawler: CrawlerStatus,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct TrustSettings {
    /// npub or hex on input; hex on output. None = no root.
    pub root: Option<String>,
    pub threshold: u8,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct RelayUpdate {
    pub url: String,
    pub enabled: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct OverrideRequest {
    pub action: Option<OverrideAction>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct DownloadRequest {
    pub magnet: String,
}

/// Typed client for the server API. All methods send `Authorization: Bearer <token>`.
#[derive(Clone)]
pub struct ApiClient {
    pub base_url: String,
    token: Option<String>,
    http: reqwest::Client,
}

impl ApiClient {
    pub fn new(base_url: &str, token: Option<String>) -> crate::Result<Self> {
        Ok(Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            token,
            http: crate::http::client(std::time::Duration::from_secs(20))?,
        })
    }
    /// ws(s)://…/relay?token=… derived from base_url.
    pub fn relay_ws_url(&self) -> String {
        todo!()
    }
    pub async fn health(&self) -> crate::Result<Health> {
        todo!()
    }
    pub async fn search(&self, q: &SearchQuery) -> crate::Result<SearchPage> {
        let _ = (q, &self.token, &self.http);
        todo!()
    }
    pub async fn torrent(&self, infohash: &str) -> crate::Result<Option<TorrentDetail>> {
        let _ = infohash;
        todo!()
    }
    pub async fn refresh_liveness(&self, infohash: &str) -> crate::Result<Liveness> {
        let _ = infohash;
        todo!()
    }
    pub async fn post_batch(&self, events: &[RawEvent]) -> crate::Result<BatchResponse> {
        let _ = events;
        todo!()
    }
    pub async fn stats(&self) -> crate::Result<StatsResponse> {
        todo!()
    }
    pub async fn relays(&self) -> crate::Result<Vec<RelayInfo>> {
        todo!()
    }
    pub async fn set_relay(&self, u: &RelayUpdate) -> crate::Result<Vec<RelayInfo>> {
        let _ = u;
        todo!()
    }
    pub async fn trust(&self, pubkey: &str) -> crate::Result<TrustScore> {
        let _ = pubkey;
        todo!()
    }
    pub async fn trust_settings(&self) -> crate::Result<TrustSettings> {
        todo!()
    }
    pub async fn set_trust_settings(&self, s: &TrustSettings) -> crate::Result<TrustSettings> {
        let _ = s;
        todo!()
    }
    pub async fn set_override(
        &self,
        pubkey: &str,
        action: Option<OverrideAction>,
    ) -> crate::Result<()> {
        let _ = (pubkey, action);
        todo!()
    }
    pub async fn saved_searches(&self) -> crate::Result<Vec<SavedSearch>> {
        todo!()
    }
    pub async fn put_saved_search(&self, s: &SavedSearch) -> crate::Result<SavedSearch> {
        let _ = s;
        todo!()
    }
    pub async fn delete_saved_search(&self, id: i64) -> crate::Result<()> {
        let _ = id;
        todo!()
    }
    pub async fn downloads(&self) -> crate::Result<Vec<DownloadInfo>> {
        todo!()
    }
    pub async fn add_download(&self, magnet: &str) -> crate::Result<DownloadInfo> {
        let _ = magnet;
        todo!()
    }
    /// action: "pause" | "resume" | "delete"
    pub async fn download_action(&self, id: u64, action: &str) -> crate::Result<()> {
        let _ = (id, action);
        todo!()
    }
}

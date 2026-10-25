//! HTTP API DTOs + client (ARCHITECTURE.md §6). Owner: trust-sync agent (DTOs are contracts).

use reqwest::Method;
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
    /// Unix time the last pass ended.
    pub last_run: Option<i64>,
    pub last_report: Option<crate::crawler::CrawlReport>,
    /// Passes finished (ok or failed) since the server started.
    #[serde(default)]
    pub passes: u64,
    /// Error of the last pass, None if it succeeded.
    #[serde(default)]
    pub last_error: Option<String>,
    /// Unix time the next pass is due, while the loop is pausing.
    #[serde(default)]
    pub next_run: Option<i64>,
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
        let base = &self.base_url;
        let ws = if let Some(rest) = base.strip_prefix("https://") {
            format!("wss://{rest}")
        } else if let Some(rest) = base.strip_prefix("http://") {
            format!("ws://{rest}")
        } else if base.starts_with("ws://") || base.starts_with("wss://") {
            base.clone()
        } else {
            format!("ws://{base}")
        };
        match self.token.as_deref().filter(|t| !t.is_empty()) {
            Some(t) => format!("{ws}/relay?token={}", encode(t)),
            None => format!("{ws}/relay"),
        }
    }
    pub async fn health(&self) -> crate::Result<Health> {
        self.json(self.req(Method::GET, "/health")).await
    }
    pub async fn search(&self, q: &SearchQuery) -> crate::Result<SearchPage> {
        self.json(self.req(Method::POST, "/api/v1/search").json(q))
            .await
    }
    pub async fn torrent(&self, infohash: &str) -> crate::Result<Option<TorrentDetail>> {
        let resp = self
            .send(self.req(
                Method::GET,
                &format!("/api/v1/torrents/{}", encode(infohash)),
            ))
            .await;
        match resp {
            Err(crate::Error::Server { status: 404, .. }) => Ok(None),
            Err(e) => Err(e),
            Ok(r) => Ok(Some(r.json().await?)),
        }
    }
    pub async fn refresh_liveness(&self, infohash: &str) -> crate::Result<Liveness> {
        let path = format!("/api/v1/torrents/{}/liveness", encode(infohash));
        self.json(self.req(Method::POST, &path)).await
    }
    /// Posts in chunks of `MAX_BATCH` and concatenates the results.
    pub async fn post_batch(&self, events: &[RawEvent]) -> crate::Result<BatchResponse> {
        let mut out = BatchResponse::default();
        for chunk in events.chunks(MAX_BATCH) {
            let body = BatchRequest {
                events: chunk.to_vec(),
            };
            let r: BatchResponse = self
                .json(self.req(Method::POST, "/api/v1/events/batch").json(&body))
                .await?;
            out.results.extend(r.results);
        }
        Ok(out)
    }
    pub async fn stats(&self) -> crate::Result<StatsResponse> {
        self.json(self.req(Method::GET, "/api/v1/stats")).await
    }
    pub async fn relays(&self) -> crate::Result<Vec<RelayInfo>> {
        self.json(self.req(Method::GET, "/api/v1/relays")).await
    }
    pub async fn set_relay(&self, u: &RelayUpdate) -> crate::Result<Vec<RelayInfo>> {
        self.json(self.req(Method::PUT, "/api/v1/relays").json(u))
            .await
    }
    pub async fn trust(&self, pubkey: &str) -> crate::Result<TrustScore> {
        let path = format!("/api/v1/trust/{}", encode(pubkey));
        self.json(self.req(Method::GET, &path)).await
    }
    pub async fn trust_settings(&self) -> crate::Result<TrustSettings> {
        self.json(self.req(Method::GET, "/api/v1/settings/trust"))
            .await
    }
    pub async fn set_trust_settings(&self, s: &TrustSettings) -> crate::Result<TrustSettings> {
        self.json(self.req(Method::PUT, "/api/v1/settings/trust").json(s))
            .await
    }
    pub async fn set_override(
        &self,
        pubkey: &str,
        action: Option<OverrideAction>,
    ) -> crate::Result<()> {
        let path = format!("/api/v1/overrides/{}", encode(pubkey));
        self.send(
            self.req(Method::PUT, &path)
                .json(&OverrideRequest { action }),
        )
        .await
        .map(drop)
    }
    pub async fn saved_searches(&self) -> crate::Result<Vec<SavedSearch>> {
        self.json(self.req(Method::GET, "/api/v1/saved-searches"))
            .await
    }
    pub async fn put_saved_search(&self, s: &SavedSearch) -> crate::Result<SavedSearch> {
        self.json(self.req(Method::POST, "/api/v1/saved-searches").json(s))
            .await
    }
    pub async fn delete_saved_search(&self, id: i64) -> crate::Result<()> {
        let path = format!("/api/v1/saved-searches/{id}");
        self.send(self.req(Method::DELETE, &path)).await.map(drop)
    }
    pub async fn downloads(&self) -> crate::Result<Vec<DownloadInfo>> {
        self.json(self.req(Method::GET, "/api/v1/downloads")).await
    }
    pub async fn add_download(&self, magnet: &str) -> crate::Result<DownloadInfo> {
        let body = DownloadRequest {
            magnet: magnet.to_string(),
        };
        self.json(self.req(Method::POST, "/api/v1/downloads").json(&body))
            .await
    }
    /// action: "pause" | "resume" | "delete"
    pub async fn download_action(&self, id: u64, action: &str) -> crate::Result<()> {
        let req = match action {
            "pause" | "resume" => {
                self.req(Method::POST, &format!("/api/v1/downloads/{id}/{action}"))
            }
            "delete" => self.req(Method::DELETE, &format!("/api/v1/downloads/{id}")),
            other => {
                return Err(crate::Error::Invalid(format!(
                    "unknown download action {other:?}"
                )));
            }
        };
        self.send(req).await.map(drop)
    }

    fn req(&self, method: Method, path: &str) -> reqwest::RequestBuilder {
        let rb = self
            .http
            .request(method, format!("{}{path}", self.base_url));
        match &self.token {
            Some(t) => rb.bearer_auth(t),
            None => rb,
        }
    }

    /// Send; any non-2xx becomes `Error::Server` with the message from the error body.
    async fn send(&self, rb: reqwest::RequestBuilder) -> crate::Result<reqwest::Response> {
        let resp = rb.send().await?;
        let status = resp.status();
        if status.is_success() {
            return Ok(resp);
        }
        let body = resp.text().await.unwrap_or_default();
        let msg = match serde_json::from_str::<ApiErrorBody>(&body) {
            Ok(e) if !e.message.is_empty() => e.message,
            Ok(e) => e.error,
            Err(_) if body.trim().is_empty() => {
                status.canonical_reason().unwrap_or("error").to_string()
            }
            Err(_) => body.chars().take(500).collect(),
        };
        Err(crate::Error::Server {
            status: status.as_u16(),
            msg,
        })
    }

    async fn json<T: serde::de::DeserializeOwned>(
        &self,
        rb: reqwest::RequestBuilder,
    ) -> crate::Result<T> {
        Ok(self.send(rb).await?.json().await?)
    }
}

/// Percent-encode one path segment / query value.
fn encode(s: &str) -> String {
    percent_encoding::utf8_percent_encode(s, percent_encoding::NON_ALPHANUMERIC).to_string()
}

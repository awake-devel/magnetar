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
    /// What this server supports beyond the base API (`features::*`); older servers send none,
    /// so a client hides what needs them.
    #[serde(default)]
    pub features: Vec<String>,
}

/// `Health::features` values.
pub mod features {
    /// The release/media filters of `SearchQuery` (languages, resolutions, …, genres,
    /// min_rating, years) and `Sort::Rating` (D44).
    pub const FILTERS: &str = "filters";
    /// `POST /api/v1/facets` (several disjunctive facets at once, D44). Still 503 while the
    /// search index builds.
    pub const FACET_SET: &str = "facet_set";
    /// `GET|POST /api/v1/media/{id}/releases` (D44).
    pub const MEDIA_RELEASES: &str = "media_releases";
    pub const ALL: &[&str] = &[FILTERS, FACET_SET, MEDIA_RELEASES];
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
    /// The server's search index (D43); None from older servers or when it is disabled.
    #[serde(default)]
    pub search_index: Option<SearchIndexStatus>,
}

/// State of the server's Tantivy search index (D43).
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct SearchIndexStatus {
    /// "building" (searches use Postgres meanwhile) | "ready" | "error".
    pub state: String,
    /// Documents (infohashes) searchable now.
    pub docs: u64,
    /// Full build progress, infohashes done of total (while building).
    pub build_done: u64,
    pub build_total: u64,
    /// Seconds the last full build took.
    pub build_secs: Option<u64>,
    /// Unix time of the last index commit.
    pub last_commit: Option<i64>,
    /// Changes queued for the index, and the age in seconds of the oldest one.
    pub pending: u64,
    pub lag_secs: Option<i64>,
    pub bytes: u64,
    pub error: Option<String>,
}

/// One value of a facet and how many results have it.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct FacetCount {
    pub value: String,
    pub count: u64,
}

/// `GET /api/v1/facets?field=…` (D43): value counts over the results of the search parameters.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct FacetsResponse {
    pub field: String,
    /// Results the counts are taken over.
    pub total: u64,
    /// Most frequent first (ties by value).
    pub counts: Vec<FacetCount>,
}

/// `POST /api/v1/facets` body (D44): counts of several facets over one search.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct FacetsRequest {
    pub query: SearchQuery,
    /// Facet names (`category`, `language`, `resolution`, …; docs/FILTERS.md).
    pub fields: Vec<String>,
    /// Values per facet, default 20, at most 200.
    pub size: u32,
}

/// Counts of several facets. Each facet's counts apply every filter of the query except its
/// own (so the other values of a filtered field still show what they would give).
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct FacetSet {
    /// Results of the query with every filter.
    pub total: u64,
    pub facets: Vec<FacetsResponse>,
}

/// Releases of one movie/series (`GET /api/v1/media/{id}/releases`, D44), grouped by
/// season/episode, then resolution, then language.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct MediaReleases {
    #[serde(default)]
    pub media: Option<MediaInfo>,
    /// Releases found (all of them, also beyond `MAX_RELEASES`).
    pub total: u64,
    /// More than `MAX_RELEASES` releases: the least seeded were left out.
    #[serde(default)]
    pub truncated: bool,
    pub groups: Vec<crate::release::ReleaseGroup>,
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

/// Result of a conditional binary GET (`ApiClient::get_bytes`).
#[derive(Clone, Debug, PartialEq)]
pub enum Fetched {
    /// 304: the copy matching the sent ETag is still current.
    NotModified,
    Body {
        bytes: Vec<u8>,
        content_type: Option<String>,
        etag: Option<String>,
    },
}

/// A server-relative `/api/v1/…` path (what `MediaInfo.poster` carries), nothing else.
pub fn is_api_path(path: &str) -> bool {
    path.starts_with("/api/v1/") && !path.contains("..") && !path.contains("://")
}

/// Largest binary body `get_bytes` accepts (posters are ~30 KiB).
pub const MAX_BYTES_BODY: usize = 4 << 20;

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
    /// Other releases of one identified title (`GET /api/v1/media/{id}/torrents`, D40),
    /// best seeded first.
    pub async fn media_torrents(
        &self,
        media_id: i64,
        limit: u32,
        offset: u32,
    ) -> crate::Result<SearchPage> {
        let path = format!("/api/v1/media/{media_id}/torrents");
        let query = [
            ("sort", "seeders".to_string()),
            ("limit", limit.to_string()),
            ("offset", offset.to_string()),
        ];
        self.json(self.req(Method::GET, &path).query(&query)).await
    }
    /// GET a server-relative binary resource (e.g. `MediaInfo.poster`) with the bearer token.
    /// Only `/api/v1/` paths are accepted, so a URL from the server cannot point elsewhere.
    /// Ok(None) on 404.
    pub async fn get_bytes(
        &self,
        path: &str,
        etag: Option<&str>,
    ) -> crate::Result<Option<Fetched>> {
        if !is_api_path(path) {
            return Err(crate::Error::Invalid(format!(
                "not a server API path: {path:?}"
            )));
        }
        let mut rb = self.req(Method::GET, path);
        if let Some(e) = etag {
            rb = rb.header(reqwest::header::IF_NONE_MATCH, e);
        }
        let resp = match self.send(rb).await {
            Err(crate::Error::Server { status: 404, .. }) => return Ok(None),
            // `send` treats every non-2xx as an error, 304 included.
            Err(crate::Error::Server { status: 304, .. }) => return Ok(Some(Fetched::NotModified)),
            Err(e) => return Err(e),
            Ok(r) => r,
        };
        let header = |h: reqwest::header::HeaderName| {
            resp.headers()
                .get(h)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string)
        };
        let content_type = header(reqwest::header::CONTENT_TYPE);
        let etag = header(reqwest::header::ETAG);
        if resp
            .content_length()
            .is_some_and(|n| n > MAX_BYTES_BODY as u64)
        {
            return Err(crate::Error::Invalid(format!("{path}: body too large")));
        }
        let bytes = resp.bytes().await?;
        if bytes.len() > MAX_BYTES_BODY {
            return Err(crate::Error::Invalid(format!("{path}: body too large")));
        }
        Ok(Some(Fetched::Body {
            bytes: bytes.to_vec(),
            content_type,
            etag,
        }))
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
    /// Value counts of one facet over the results of `text` (D43). Servers without a ready
    /// search index answer 503 (`Error::Server`), older ones 404.
    pub async fn facets(
        &self,
        field: &str,
        text: &str,
        size: u32,
    ) -> crate::Result<FacetsResponse> {
        let query = [
            ("field", field.to_string()),
            ("q", text.to_string()),
            ("size", size.to_string()),
        ];
        self.json(self.req(Method::GET, "/api/v1/facets").query(&query))
            .await
    }
    /// Disjunctive counts of several facets over a full query (D44, `features::FACET_SET`).
    pub async fn facet_set(&self, r: &FacetsRequest) -> crate::Result<FacetSet> {
        self.json(self.req(Method::POST, "/api/v1/facets").json(r))
            .await
    }
    /// Releases of one media grouped by season/episode, quality and language (D44); `q`
    /// filters them (text, languages, resolutions, …).
    pub async fn media_releases(
        &self,
        media_id: i64,
        q: &SearchQuery,
    ) -> crate::Result<MediaReleases> {
        let path = format!("/api/v1/media/{media_id}/releases");
        self.json(self.req(Method::POST, &path).json(q)).await
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

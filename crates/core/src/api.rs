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
    /// `GET /api/v1/torrents/{infohash}/liveness-history` (D45).
    pub const LIVENESS_HISTORY: &str = "liveness_history";
    /// `/api/v1/stats/breakdown`, `/stats/timeseries`, `/stats/health` (D47). Breakdowns are
    /// still 503 while the search index builds.
    pub const STATS: &str = "stats";
    /// `/api/v1/graphs/{wot,relays,media/{id},swarm/{infohash}}` (D47).
    pub const GRAPHS: &str = "graphs";
    /// `SearchQuery.include_adult` (D52): adult documents are left out unless it is set or
    /// `porn` is selected. Older servers ignore it; the app then drops `porn` hits itself.
    pub const ADULT_FILTER: &str = "adult_filter";
    /// `SearchQuery.origins`, `TorrentSummary.origin`, `TorrentDetail.dht` and the `origin`
    /// facet / breakdown (D51): results carry their source (Nostr, DHT or both).
    pub const ORIGIN: &str = "origin";
    /// The DHT crawler runs on this server (`MAGNETAR_DHT` ≠ off): DHT results can appear and
    /// `StatsHealth.dht` / `StatsResponse.dht` are filled. Not in `ALL` (it depends on the
    /// configuration).
    pub const DHT: &str = "dht";
    /// `GET /api/v1/saved-searches/poll` (new hits since a client-held watermark) and
    /// `GET|PUT /api/v1/saved-searches/{id}` (D77): saved searches and their notifications
    /// from a thin client.
    pub const SAVED_SEARCH_POLL: &str = "saved_search_poll";
    /// The empty query hides DHT-only torrents no seed check found peers for (D76).
    pub const BROWSE_CHECKED_DHT: &str = "browse_checked_dht";
    pub const ALL: &[&str] = &[
        FILTERS,
        FACET_SET,
        MEDIA_RELEASES,
        LIVENESS_HISTORY,
        STATS,
        GRAPHS,
        ADULT_FILTER,
        ORIGIN,
        SAVED_SEARCH_POLL,
        BROWSE_CHECKED_DHT,
    ];
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
    /// The seed-check backlog (D45); None from older servers.
    #[serde(default)]
    pub liveness: Option<LivenessStats>,
    /// The DHT crawler (D58); None when it is off or the server is older.
    #[serde(default)]
    pub dht: Option<DhtStats>,
}

/// The DHT crawler on the server (D58, docs/DHT.md §10). Rates are over the last ~10 minutes
/// of the running crawler; `*_total` counts come from the database. The external address is
/// reported as a class only, never the address itself.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct DhtStats {
    /// "polite" | "aggressive".
    pub mode: String,
    /// False while it waits to restart after an error (`last_error`).
    pub running: bool,
    pub uptime_secs: u64,
    pub virtual_nodes: u32,
    pub ports: Vec<u16>,
    /// "public" | "private" | "cgnat" | "unknown" (BEP 42 vote or UPnP).
    pub external_ip: String,
    /// Contacts in the routing tables (all virtual nodes) and in the crawl pool.
    pub routing_nodes: u64,
    pub pool_nodes: u64,
    /// Pool nodes known to answer `sample_infohashes` (BEP 51).
    pub pool_bep51: u64,
    pub queries_per_sec: f64,
    /// Share of our queries that got an answer.
    pub answered_share: f64,
    /// The adaptive brake (D54/D66) halves the rates.
    pub brake: bool,
    /// Queries other nodes sent us per minute (aggressive: reachability shows here).
    pub incoming_per_min: f64,
    /// Discovery: new unique infohashes per minute, and since the start.
    pub hashes_per_min: f64,
    pub hashes_seen: u64,
    /// Hashes waiting for peer collection (in memory), by lane: announce, sample, retry.
    pub queue_len: u64,
    pub queue_lanes: Vec<u64>,
    /// Sampling paused by backpressure (D62).
    pub sampling_paused: bool,
    /// Metadata (BEP 9) verified per minute, and success share of finished fetches.
    pub metadata_per_min: f64,
    pub fetch_success_rate: f64,
    /// Since the start: verified, failed, stored, blocked (D53, counted only).
    pub fetch_ok: u64,
    pub fetch_failed: u64,
    pub stored: u64,
    pub blocked: u64,
    /// Database totals: DHT torrents stored, of which adult (D52), hashes blocked (any
    /// origin), hashes waiting in `dht_seen` (queued) and given up / failed.
    pub stored_total: u64,
    pub adult_total: u64,
    pub blocked_total: u64,
    pub seen_queued: u64,
    pub seen_failed: u64,
    pub upnp: Option<UpnpStatus>,
    pub last_error: Option<String>,
    // --- D93 (all serde default: older servers leave them 0) ---
    /// Since the start: hashes given to peer collection, how many got peers from the node
    /// that sampled them, how many needed an iterative lookup, and fetches handed a peer that
    /// announced the hash to us.
    pub collected: u64,
    pub from_source: u64,
    pub looked_up: u64,
    pub announce_fetches: u64,
    /// Announces that reached a hash already seen (seen filter) and upgraded it to the
    /// announce lane, or gave their peer to a collection in flight.
    pub announce_upgraded: u64,
    /// Incoming queries since the start, per type.
    pub incoming_ping: u64,
    pub incoming_find_node: u64,
    pub incoming_get_peers: u64,
    pub incoming_announce_peer: u64,
    pub incoming_sample_infohashes: u64,
    pub incoming_unknown: u64,
    /// Peer attempts skipped by the fake-peer reputation since the start, and peers skipped
    /// right now or carrying strikes.
    pub fake_peers_skipped: u64,
    pub fake_peers_banned: u64,
    pub fake_peers_tracked: u64,
    /// `get_peers` lookups over the rate window: mean wall time and queries per lookup, and
    /// the share of their queries that went slow (no answer within the lookup timeout).
    pub lookup_mean_ms: f64,
    pub queries_per_lookup: f64,
    pub lookup_slow_share: f64,
    /// Answer share over the rate window by how long the queried node has been in the crawl
    /// pool (< 5 min, older, not in it) and for pool nodes asked again after answering a
    /// sample: old/repeat falling while young holds points at nodes rate-limiting us.
    pub answered_share_young: f64,
    pub answered_share_old: f64,
    pub answered_share_unpooled: f64,
    pub answered_share_repeat: f64,
}

/// Router port mappings for the crawler (D57, D72).
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct UpnpStatus {
    /// "off" | "searching" | "mapped" | "partial" | "failed".
    pub state: String,
    /// The gateway's LAN address that answered (`host:port` of its description).
    pub gateway: Option<String>,
    pub mappings: Vec<UpnpMapping>,
    /// Unix time of the last successful (re)mapping.
    pub renewed_at: Option<i64>,
    pub last_error: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct UpnpMapping {
    /// "udp" | "tcp".
    pub protocol: String,
    pub port: u16,
    /// 0 = permanent (the router refused a lease).
    pub lease_secs: u32,
}

/// Seed checks (liveness) on the server (D45). Counts are distinct infohashes.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct LivenessStats {
    /// False while the `liveness.enabled` setting turns the checker off.
    pub enabled: bool,
    pub torrents: u64,
    pub never_checked: u64,
    /// Due now (never checked included).
    pub due_now: u64,
    /// Checked (answered or not) in the last 24 h / the last hour.
    pub checked_24h: u64,
    pub checks_per_hour: u64,
    /// Latest result: someone seeds / answered with nobody seeding / no tracker or DHT answer.
    pub alive: u64,
    pub dead: u64,
    pub unknown: u64,
    /// Trackers the checker knows, of which backing off after failures; DHT nodes cached.
    pub trackers: u64,
    pub trackers_backed_off: u64,
    pub dht_nodes: u64,
    pub last_batch: Option<crate::liveness::BatchReport>,
    /// Unix time the last batch ended.
    pub last_batch_at: Option<i64>,
}

/// One history point: what a check found (D45). None = that source did not answer.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct LivenessPoint {
    pub at: i64,
    pub seeders: Option<u32>,
    pub leechers: Option<u32>,
    /// DHT estimate (BEP 33), when the DHT was asked.
    #[serde(default)]
    pub peers_est: Option<u32>,
}

/// `GET /api/v1/torrents/{infohash}/liveness-history` (D45): oldest point first. Points are
/// at least ~6 h apart and an unchanged value is repeated only weekly, so a chart should hold
/// each value until the next point (step line).
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct LivenessHistory {
    pub infohash: InfoHash,
    pub points: Vec<LivenessPoint>,
    /// How far back the server keeps points.
    pub keep_days: u32,
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

/// `GET /api/v1/saved-searches/poll` (D77): what the server's saved searches found since the
/// client's watermark. Stateless on the server: the client keeps `watermark` and sends it back
/// as `since`, so several clients and the server's own ntfy notifier never take each other's
/// new hits.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct SavedSearchPoll {
    /// Send this as `since` on the next poll: the newest `created_at` seen (or `since`), or
    /// the server's clock when `since` was not given.
    pub watermark: i64,
    /// One entry per polled search with at least one new hit, in id order.
    pub results: Vec<SavedSearchHits>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct SavedSearchHits {
    pub search: SavedSearch,
    /// Newest first, at most `query::MAX_LIMIT`.
    pub hits: Vec<TorrentSummary>,
    /// More new hits than returned.
    #[serde(default)]
    pub truncated: bool,
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

/// `POST /api/v1/stats/breakdown` body (D47): top values of several fields over one search.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct BreakdownRequest {
    /// The current filters (cross-filtering): each field is counted with every filter but its
    /// own, like the facets of D44.
    pub query: SearchQuery,
    /// `language`, `resolution`, `source`, `codec`, `hdr`, `audio`, `category`, `subcategory`,
    /// `genre`, `year`, `media_kind`, … (docs/STATS.md).
    pub fields: Vec<String>,
    /// Values shown per field, default 10, at most 50; the rest is summed into `other`.
    pub top: u32,
}

/// Top values of one field and what is left.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Breakdown {
    pub field: String,
    /// Results counted for this field (every filter but its own).
    pub total: u64,
    /// Most frequent first. Multi-valued fields (languages, HDR, audio, genres) count a result
    /// once per value, so the counts can add up to more than `total`.
    pub counts: Vec<FacetCount>,
    /// Sum of the counts of the values after the top ones, and how many such values there are.
    pub other: u64,
    pub other_values: u64,
    /// Results without any value for this field (unknown language, no media link, …).
    pub missing: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct BreakdownSet {
    /// Results of the query with every filter.
    pub total: u64,
    pub breakdowns: Vec<Breakdown>,
    /// Unix time the counts were taken (answers are cached for a minute or two).
    pub computed_at: i64,
}

/// One bucket of a time series: its start (unix time) and value.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct TimeseriesPoint {
    pub at: i64,
    pub value: i64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Timeseries {
    pub metric: String,
    /// "counter" (events per bucket, empty buckets are 0) | "gauge" (last sample of the
    /// bucket; buckets without a sample are left out).
    pub kind: String,
    /// "hour" | "day": a metric kept per day only (`torrents_published`) says "day" even when
    /// hours were asked for.
    pub bucket: String,
    /// Oldest first.
    pub points: Vec<TimeseriesPoint>,
}

/// `GET /api/v1/stats/timeseries?metric=…&bucket=hour|day&since=…` (D47).
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct TimeseriesSet {
    pub since: i64,
    pub until: i64,
    pub series: Vec<Timeseries>,
}

/// Relays the server crawls, by state.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct RelayHealth {
    pub total: u64,
    pub enabled: u64,
    /// Answered in the last 24 h.
    pub ok_24h: u64,
    /// Failing now (backing off).
    pub failing: u64,
    /// Ever delivered a torrent.
    pub productive: u64,
}

/// `GET /api/v1/stats/health` (D47): how complete and how fresh the index is.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct StatsHealth {
    pub torrents: u64,
    /// Seed checks: alive / dead / unknown and the backlog (D45).
    pub liveness: Option<LivenessStats>,
    /// Index backlog (`pending`, `lag_secs`) and state (D43).
    pub search_index: Option<SearchIndexStatus>,
    pub relays: RelayHealth,
    pub crawler: CrawlerStatus,
    /// Infohashes linked to a movie/series/anime (D40/D41), shown or not.
    pub media_linked: u64,
    /// Infohashes whose release attributes (D44) are still to be parsed.
    pub attrs_missing: u64,
    pub computed_at: i64,
    /// The DHT crawler (D58); None when it is off.
    pub dht: Option<DhtStats>,
}

/// A node of a network graph (D47). What `id` is depends on `kind`: a pubkey (hex) for
/// `root`/`follow`/`publisher`, a URL for `relay`/`tracker`, an infohash for
/// `torrent`/`release`, a media id for `media`, the facet value for `quality`/`language`/
/// `group`/`season` (docs/STATS.md), `dht` for the DHT.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct GraphNode {
    pub id: String,
    pub kind: String,
    pub label: String,
    /// Size: torrents, seeders, publishers… (≥ 0; the app scales it).
    pub weight: f64,
    /// Trust score of a publisher (0–100), when known.
    pub score: Option<u8>,
    /// Lines shown when the node is tapped ("followed by you", "+25 first seen 400 days ago").
    pub detail: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct GraphEdge {
    /// Indexes into `Graph::nodes`.
    pub source: u32,
    pub target: u32,
    pub weight: f64,
    /// `follows`, `writes_to`, `quality`, `language`, `group`, `season`, `tracker`, `dht`,
    /// `published`.
    pub kind: String,
}

/// `GET /api/v1/graphs/…` (D47): at most [`MAX_GRAPH_NODES`] nodes.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Graph {
    /// `wot` | `relays` | `media` | `swarm`.
    pub kind: String,
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<GraphEdge>,
    /// Nodes were left out to stay under the cap.
    pub truncated: bool,
    /// What the graph cannot show (no trust root set, no relay lists known, …).
    pub note: Option<String>,
}

/// Node cap of every graph endpoint: what a phone lays out and draws smoothly.
pub const MAX_GRAPH_NODES: usize = 200;

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
    /// Seeders/leechers over the last `days` (D45). None: unknown infohash, or a server older
    /// than D45 (both answer 404).
    pub async fn liveness_history(
        &self,
        infohash: &str,
        days: u32,
    ) -> crate::Result<Option<LivenessHistory>> {
        let path = format!(
            "/api/v1/torrents/{}/liveness-history?days={days}",
            encode(infohash)
        );
        match self.json(self.req(Method::GET, &path)).await {
            Err(crate::Error::Server { status: 404, .. }) => Ok(None),
            r => r.map(Some),
        }
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
    /// Top values of several fields under the current filters (D47, `features::STATS`).
    /// 503 while the server's search index builds, 404 from older servers.
    pub async fn stats_breakdown(&self, r: &BreakdownRequest) -> crate::Result<BreakdownSet> {
        self.json(self.req(Method::POST, "/api/v1/stats/breakdown").json(r))
            .await
    }
    /// Activity per hour or day (D47); `since` None = the server's default window.
    pub async fn stats_timeseries(
        &self,
        metrics: &[String],
        bucket: &str,
        since: Option<i64>,
    ) -> crate::Result<TimeseriesSet> {
        let mut query = vec![
            ("metric", metrics.join(",")),
            ("bucket", bucket.to_string()),
        ];
        if let Some(s) = since {
            query.push(("since", s.to_string()));
        }
        self.json(
            self.req(Method::GET, "/api/v1/stats/timeseries")
                .query(&query),
        )
        .await
    }
    pub async fn stats_health(&self) -> crate::Result<StatsHealth> {
        self.json(self.req(Method::GET, "/api/v1/stats/health"))
            .await
    }
    /// A network graph (D47): `wot`, `relays`, `media/{id}` or `swarm/{infohash}`. None on
    /// 404 (unknown media/infohash, or a server older than D47).
    pub async fn graph(&self, path: &str) -> crate::Result<Option<Graph>> {
        let ok = match path.split_once('/') {
            None => matches!(path, "wot" | "relays"),
            Some(("media", id)) => id.parse::<i64>().is_ok(),
            Some(("swarm", ih)) => ih.len() == 40 && ih.bytes().all(|b| b.is_ascii_hexdigit()),
            Some(_) => false,
        };
        if !ok {
            return Err(crate::Error::Invalid(format!("unknown graph {path:?}")));
        }
        let path = format!("/api/v1/graphs/{path}");
        match self.json(self.req(Method::GET, &path)).await {
            Err(crate::Error::Server { status: 404, .. }) => Ok(None),
            r => r.map(Some),
        }
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
    /// `GET /api/v1/saved-searches/{id}` (D77).
    pub async fn saved_search(&self, id: i64) -> crate::Result<SavedSearch> {
        let path = format!("/api/v1/saved-searches/{id}");
        self.json(self.req(Method::GET, &path)).await
    }
    /// `PUT /api/v1/saved-searches/{id}` (D77): name, query, notify; the server keeps its
    /// notifier watermarks.
    pub async fn update_saved_search(&self, s: &SavedSearch) -> crate::Result<SavedSearch> {
        let path = format!("/api/v1/saved-searches/{}", s.id);
        self.json(self.req(Method::PUT, &path).json(s)).await
    }
    /// `GET /api/v1/saved-searches/poll` (D77): new hits since `since` (None = first poll).
    pub async fn poll_saved_searches(
        &self,
        since: Option<i64>,
        include_adult: bool,
    ) -> crate::Result<SavedSearchPoll> {
        let mut query: Vec<(&str, String)> = Vec::new();
        if let Some(s) = since {
            query.push(("since", s.to_string()));
        }
        if include_adult {
            query.push(("adult", "1".into()));
        }
        self.json(
            self.req(Method::GET, "/api/v1/saved-searches/poll")
                .query(&query),
        )
        .await
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

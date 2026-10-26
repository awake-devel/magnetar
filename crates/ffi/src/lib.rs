//! magnetar-ffi: SQLite store + uniffi API for the Android app (ARCHITECTURE.md §8.1).
//!
//! This file is the Kotlin-facing CONTRACT: names and fields here become `app.magnetar.ffi.*`.
//! Bodies are filled by the ffi agent; signatures change only together with the Android app.

uniffi::setup_scaffolding!();

use std::sync::Arc;

use magnetar_core::liveness::{self, LivenessConfig};
use magnetar_core::model::{OverrideAction, SavedSearch, is_hex32, npub, parse_pubkey};
use magnetar_core::{crawler, magnet, notify, sync, trust};

mod app;
mod convert;
pub mod media;
mod schema;
pub mod store;

use app::{Inner, LAST_CRAWL, LAST_SYNC, TRUST_MAX_AGE_SECS, norm_infohash};
pub use store::SqliteStore;

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
    /// Rating of the linked media (D44); servers without `features::FILTERS` reject it.
    Rating,
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
    /// Release/media filters (D44, `core::query::SearchQuery`): any value within a field,
    /// all fields together. Codes as in docs/FILTERS.md.
    #[uniffi(default)]
    pub languages: Vec<String>,
    #[uniffi(default)]
    pub subtitles: Vec<String>,
    #[uniffi(default)]
    pub resolutions: Vec<String>,
    #[uniffi(default)]
    pub sources: Vec<String>,
    #[uniffi(default)]
    pub video_codecs: Vec<String>,
    #[uniffi(default)]
    pub hdr: Vec<String>,
    #[uniffi(default)]
    pub audio: Vec<String>,
    #[uniffi(default)]
    pub genres: Vec<String>,
    #[uniffi(default)]
    pub media_kinds: Vec<FfiMediaKind>,
    #[uniffi(default)]
    pub min_rating: Option<f32>,
    #[uniffi(default)]
    pub min_year: Option<i32>,
    #[uniffi(default)]
    pub max_year: Option<i32>,
}

/// `core::release::ReleaseAttrs` (D44): what the name and files say about a release.
#[derive(Clone, Debug, uniffi::Record)]
pub struct FfiReleaseAttrs {
    pub resolution: Option<String>,
    pub source: Option<String>,
    pub video_codec: Option<String>,
    pub hdr: Vec<String>,
    pub bit_depth: Option<u8>,
    pub audio: Vec<String>,
    pub channels: Option<String>,
    pub languages: Vec<String>,
    pub subtitles: Vec<String>,
    /// French scene tags as written ("multi", "vff", "vostfr", …).
    pub french: Vec<String>,
    pub group: Option<String>,
    pub season: Option<i32>,
    pub season_end: Option<i32>,
    pub episode: Option<i32>,
    pub episode_end: Option<i32>,
    pub complete: bool,
    pub year: Option<i32>,
    pub editions: Vec<String>,
    pub bitrate: Option<String>,
    pub sample_rate: Option<String>,
    pub formats: Vec<String>,
    /// Language group ("multi-vff", "vostfr", "en", …) and its label ("MULTi (VFF)").
    pub language_key: String,
    pub language_label: String,
    /// Short labels for a result row: "2160p", "WEB-DL", "H.265/HEVC", "Dolby Vision",
    /// "MULTi (VFF)", "DD+ (E-AC-3) 5.1", …
    pub badges: Vec<String>,
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
    /// Identified media (D40), when the server matched this torrent.
    pub media_title: Option<String>,
    pub media_year: Option<i32>,
    pub media_kind: Option<FfiMediaKind>,
    /// The file name the search matched when the title did not (server index, D43).
    pub matched_file: Option<String>,
    /// Media row behind `media_title`: opens the title page (`media_releases`, D44).
    pub media_id: Option<i64>,
    pub media_rating: Option<f32>,
    pub attrs: Option<FfiReleaseAttrs>,
}

/// One facet value with its count (D44) and display label.
#[derive(Clone, Debug, uniffi::Record)]
pub struct FfiFacetCount {
    pub value: String,
    pub label: String,
    pub count: u64,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct FfiFacet {
    pub field: String,
    /// Results this facet's counts are taken over (every filter but its own).
    pub total: u64,
    pub counts: Vec<FfiFacetCount>,
}

/// Counts for the filter sheet (`Magnetar::facets`, D44).
#[derive(Clone, Debug, uniffi::Record)]
pub struct FfiFacetSet {
    pub total: u64,
    pub facets: Vec<FfiFacet>,
}

/// One group of the title page: season/episode (series), resolution, language.
#[derive(Clone, Debug, uniffi::Record)]
pub struct FfiReleaseGroup {
    pub season: Option<i32>,
    pub episode: Option<i32>,
    pub resolution: Option<String>,
    pub language: String,
    pub language_label: String,
    pub releases: Vec<FfiTorrentSummary>,
}

/// The title page (`Magnetar::media_releases`, D44).
#[derive(Clone, Debug, uniffi::Record)]
pub struct FfiMediaReleases {
    pub media: Option<FfiMediaInfo>,
    pub total: u64,
    pub truncated: bool,
    pub groups: Vec<FfiReleaseGroup>,
    /// "server" | "local"
    pub source: String,
    /// Set when the server was asked and failed (the groups come from the local cache).
    pub server_error: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum FfiMediaKind {
    Movie,
    Series,
    Anime,
}

/// `core::model::MediaInfo` (D40) with TMDB decoration (D42).
#[derive(Clone, Debug, uniffi::Record)]
pub struct FfiMediaInfo {
    pub media_id: i64,
    pub kind: FfiMediaKind,
    pub title: String,
    pub original_title: Option<String>,
    pub year: Option<i32>,
    pub end_year: Option<i32>,
    pub genres: Vec<String>,
    pub rating: Option<f32>,
    pub votes: Option<u32>,
    pub runtime_min: Option<i32>,
    pub season: Option<i32>,
    pub episode: Option<i32>,
    pub episode_title: Option<String>,
    /// "tt…".
    pub imdb: Option<String>,
    /// https://www.imdb.com/title/tt…/ when `imdb` is known.
    pub imdb_url: Option<String>,
    /// Movie or Series (TMDB /movie or /tv).
    pub tmdb_kind: Option<FfiMediaKind>,
    pub tmdb_id: Option<u32>,
    pub mal: Option<u32>,
    pub anilist: Option<u32>,
    pub method: String,
    pub confidence: f32,
    pub overview: Option<String>,
    /// Server-relative poster URL; load it with `Magnetar::media_poster`.
    pub poster: Option<String>,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct FfiSearchPage {
    pub hits: Vec<FfiTorrentSummary>,
    pub total: u64,
    /// `total` is a lower bound: show "1,000+" (D43).
    pub total_is_lower_bound: bool,
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
    /// Identified media; offline it comes from the phone's copy of the last server answer.
    pub media: Option<FfiMediaInfo>,
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

/// The server's seed-check backlog (D45), for the Status screen.
#[derive(Clone, Debug, uniffi::Record)]
pub struct FfiLivenessStats {
    pub enabled: bool,
    pub torrents: u64,
    pub never_checked: u64,
    pub due_now: u64,
    pub checked_24h: u64,
    pub checks_per_hour: u64,
    pub alive: u64,
    pub dead: u64,
    pub unknown: u64,
    pub trackers: u64,
    pub trackers_backed_off: u64,
    pub dht_nodes: u64,
    /// Unix time the last batch ended, its size and duration.
    pub last_batch_at: Option<i64>,
    pub last_batch_size: u64,
    pub last_batch_ms: u64,
}

/// One point of a torrent's seeders/leechers history (D45).
#[derive(Clone, Debug, uniffi::Record)]
pub struct FfiLivenessPoint {
    pub at: i64,
    pub seeders: Option<u32>,
    pub leechers: Option<u32>,
    pub peers_est: Option<u32>,
}

/// Oldest point first; hold each value until the next point (step line).
#[derive(Clone, Debug, uniffi::Record)]
pub struct FfiLivenessHistory {
    pub infohash: String,
    pub points: Vec<FfiLivenessPoint>,
    pub keep_days: u32,
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
    inner: Arc<Inner>,
    handle: tokio::runtime::Handle,
    /// Taken in Drop and shut down in the background (dropping a Runtime inside an async
    /// context would panic).
    runtime: Option<tokio::runtime::Runtime>,
}

impl Drop for Magnetar {
    fn drop(&mut self) {
        if let Some(rt) = self.runtime.take() {
            rt.shutdown_background();
        }
    }
}

#[uniffi::export(async_runtime = "tokio")]
impl Magnetar {
    #[uniffi::constructor]
    pub fn open(db_path: String, config: FfiConfig) -> Result<Arc<Self>, FfiError> {
        magnetar_core::http::install_crypto_provider();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .thread_name("magnetar")
            .enable_all()
            .build()
            .map_err(|e| FfiError::Other {
                msg: format!("tokio runtime: {e}"),
            })?;
        if let Some(dir) = std::path::Path::new(&db_path).parent()
            && !dir.as_os_str().is_empty()
        {
            std::fs::create_dir_all(dir).map_err(|e| FfiError::Db {
                msg: format!("{}: {e}", dir.display()),
            })?;
        }
        let store = Arc::new(SqliteStore::open(&db_path)?);
        let posters = std::path::Path::new(&db_path)
            .parent()
            .map(|d| d.join("posters"))
            .unwrap_or_else(|| "posters".into());
        let inner = Arc::new(Inner::new(store, config.clone()).with_poster_dir(posters));
        // Release attributes of rows cached before D44 or by an older parser, in chunks off the
        // caller's thread.
        let backfill = inner.store.clone();
        runtime.spawn_blocking(move || {
            let mut after = String::new();
            loop {
                match backfill.backfill_attrs(&mut after, store::ATTRS_CHUNK) {
                    Ok(0) => break,
                    Ok(_) => {}
                    Err(e) => {
                        tracing::warn!("release attributes: {e}");
                        break;
                    }
                }
            }
        });
        let this = Magnetar {
            inner,
            handle: runtime.handle().clone(),
            runtime: Some(runtime),
        };
        this.set_config(config)?;
        Ok(Arc::new(this))
    }

    pub fn set_config(&self, config: FfiConfig) -> Result<(), FfiError> {
        // reqwest wants a runtime context while building its connector.
        let _guard = self.handle.enter();
        self.inner.apply_config(config)
    }

    pub fn config(&self) -> FfiConfig {
        self.inner.state().config.clone()
    }

    /// Server first when `prefer_server` and configured; local FTS5 fallback.
    pub async fn search(
        &self,
        query: FfiSearchQuery,
        prefer_server: bool,
    ) -> Result<FfiSearchPage, FfiError> {
        let q = self.inner.query(query);
        self.run(move |inner| async move {
            let mut server_error = None;
            if prefer_server
                && let Some(api) = inner.api()
                && inner.server_can_run(&api, &q).await
            {
                match api.search(&q).await {
                    Ok(mut p) => {
                        p.source = "server".into();
                        inner.overlay_flags(&mut p, q.include_hidden)?;
                        return Ok(convert::page(p, None));
                    }
                    Err(e) => server_error = Some(e.to_string()),
                }
            }
            let p = inner.store.search_sync(&q, magnetar_core::now())?;
            Ok(convert::page(p, server_error))
        })
        .await
    }

    pub async fn detail(
        &self,
        infohash: String,
        prefer_server: bool,
    ) -> Result<Option<FfiTorrentDetail>, FfiError> {
        let ih = norm_infohash(&infohash)?;
        self.run(move |inner| async move {
            let now = magnetar_core::now();
            if prefer_server && let Some(api) = inner.api() {
                match api.torrent(&ih).await {
                    Ok(Some(mut d)) => {
                        // Not cached as events; only the local flags and LRU are applied.
                        let flags = inner.store.flags(std::slice::from_ref(&ih))?;
                        let (pinned, hidden) = flags.get(&ih).copied().unwrap_or_default();
                        d.summary.pinned = pinned;
                        d.summary.hidden = hidden;
                        inner.store.touch(&ih, now)?;
                        // Kept for offline viewing (the torrent body itself is not cached here).
                        inner.store.put_media(&ih, d.media.as_ref(), now)?;
                        return Ok(Some(convert::detail(d, "server")));
                    }
                    Ok(None) => {}
                    Err(e) => tracing::warn!("server detail {ih}: {e}; using local cache"),
                }
            }
            Ok(inner
                .store
                .torrent_detail_sync(&ih, now)?
                .map(|d| convert::detail(d, "local")))
        })
        .await
    }

    /// Local file holding the poster of a media (`FfiMediaInfo.poster` is its server URL).
    /// Fetched from the server with the bearer token and cached (LRU, ≤ 30 MB); revalidated by
    /// ETag after 30 days. Offline, or when the server has none, the cached copy or None.
    pub async fn media_poster(
        &self,
        media_id: i64,
        poster_url: String,
    ) -> Result<Option<String>, FfiError> {
        self.run(move |inner| async move { inner.media_poster(media_id, &poster_url).await })
            .await
    }

    /// Other releases of the same media: the server's list (`GET /api/v1/media/{id}/torrents`,
    /// enrich/imdb), else the cached torrents linked to it, under the configured trust threshold
    /// like any other local search.
    pub async fn media_torrents(&self, media_id: i64) -> Result<FfiSearchPage, FfiError> {
        self.run(move |inner| async move {
            let mut server_error = None;
            if let Some(api) = inner.api() {
                match api.media_torrents(media_id, media::MEDIA_RELEASES, 0).await {
                    Ok(mut p) => {
                        p.source = "server".into();
                        inner.overlay_flags(&mut p, false)?;
                        return Ok(convert::page(p, None));
                    }
                    Err(e) => server_error = Some(e.to_string()),
                }
            }
            let q = magnetar_core::query::SearchQuery {
                text: format!("media:{media_id}"),
                min_trust: Some(inner.state().config.trust_threshold),
                sort: magnetar_core::query::Sort::Seeders,
                descending: true,
                limit: media::MEDIA_RELEASES,
                ..Default::default()
            };
            let p = inner.store.search_sync(&q, magnetar_core::now())?;
            Ok(convert::page(p, server_error))
        })
        .await
    }

    /// Counts for the filter sheet (D44): `fields` are facet names ("language", "resolution",
    /// "source", "codec", "hdr", "audio", "genre", "media_kind", …), each counted over `query`
    /// without its own filter. Server only: `Invalid` offline, `Server{503}` while its index
    /// builds, `Server{404|405}` from servers without `features::FACET_SET`.
    pub async fn facets(
        &self,
        query: FfiSearchQuery,
        fields: Vec<String>,
    ) -> Result<FfiFacetSet, FfiError> {
        let q = self.inner.query(query);
        let api = self.inner.require_api()?;
        self.run(move |_| async move {
            let r = magnetar_core::api::FacetsRequest {
                query: q,
                fields,
                size: 50,
            };
            Ok(api.facet_set(&r).await?.into())
        })
        .await
    }

    /// The title page (D44): the releases of one media grouped by season/episode, resolution
    /// and language, best seeded first; `query`'s filters apply. Server first; offline (or a
    /// server without `features::MEDIA_RELEASES`), the cached torrents linked to it.
    pub async fn media_releases(
        &self,
        media_id: i64,
        query: FfiSearchQuery,
    ) -> Result<FfiMediaReleases, FfiError> {
        let q = self.inner.query(query);
        self.run(move |inner| async move {
            let mut server_error = None;
            if let Some(api) = inner.api() {
                match api.media_releases(media_id, &q).await {
                    Ok(mut r) => {
                        for g in &mut r.groups {
                            let mut page = magnetar_core::query::SearchPage {
                                hits: std::mem::take(&mut g.releases),
                                ..Default::default()
                            };
                            inner.overlay_flags(&mut page, q.include_hidden)?;
                            g.releases = page.hits;
                        }
                        r.groups.retain(|g| !g.releases.is_empty());
                        return Ok(convert::releases(r, "server", None));
                    }
                    Err(e) => server_error = Some(e.to_string()),
                }
            }
            let now = magnetar_core::now();
            let mut lq = q.clone();
            lq.text = format!("media:{media_id} {}", q.text);
            lq.sort = magnetar_core::query::Sort::Seeders;
            lq.descending = true;
            lq.limit = magnetar_core::query::MAX_LIMIT;
            let mut hits = Vec::new();
            let mut total = 0;
            while hits.len() < magnetar_core::release::MAX_RELEASES {
                lq.offset = hits.len() as u32;
                let page = inner.store.search_sync(&lq, now)?;
                total = page.total;
                let n = page.hits.len();
                hits.extend(page.hits);
                if n < lq.limit as usize {
                    break;
                }
            }
            let media = inner.store.media_by_id(media_id, now)?;
            let by_episode = match &media {
                Some(m) => m.kind != magnetar_core::model::MediaKind::Movie,
                None => hits
                    .iter()
                    .any(|h| h.attrs.as_ref().is_some_and(|a| a.season.is_some())),
            };
            let truncated = total > hits.len() as u64;
            let r = magnetar_core::api::MediaReleases {
                media,
                total,
                truncated,
                groups: magnetar_core::release::group_releases(hits, by_episode),
            };
            Ok(convert::releases(r, "local", server_error))
        })
        .await
    }

    /// What the configured server supports (`core::api::features`, D44). None: no server
    /// configured or it did not answer (searches then use the local cache); an empty list:
    /// a server older than D44. The app hides the filters the answer lacks.
    pub async fn server_features(&self) -> Option<Vec<String>> {
        let api = self.inner.api()?;
        self.run(move |inner| async move {
            let features = api.health().await?.features;
            inner.note_features(&features);
            Ok(features)
        })
        .await
        .ok()
    }

    pub async fn crawl_once(&self, max_seconds: u32) -> Result<FfiCrawlReport, FfiError> {
        let cfg = self.inner.crawl_config(max_seconds);
        self.run(move |inner| async move {
            let mut report = crawler::crawl_once(&*inner.store, &cfg).await?;
            let now = magnetar_core::now();
            if let Err(e) = trust::recompute(&*inner.store, now, TRUST_MAX_AGE_SECS).await {
                report.errors.push(format!("trust: {e}"));
            }
            inner.store.set_setting_sync(LAST_CRAWL, &now.to_string())?;
            Ok(report.into())
        })
        .await
    }

    pub async fn sync_now(&self) -> Result<FfiSyncReport, FfiError> {
        let Some(api) = self.inner.api() else {
            return Ok(FfiSyncReport {
                method: "none".into(),
                have: 0,
                uploaded: 0,
                duplicates: 0,
                failed: 0,
                error: None,
            });
        };
        self.run(move |inner| async move {
            let report = sync::sync_up(&*inner.store, &api.relay_ws_url(), &api).await?;
            if report.error.is_none() {
                inner
                    .store
                    .set_setting_sync(LAST_SYNC, &magnetar_core::now().to_string())?;
            }
            Ok(report.into())
        })
        .await
    }

    pub fn cleanup(&self) -> Result<FfiCleanupReport, FfiError> {
        let cap_mb = self.inner.state().config.cache_cap_mb as u64;
        let report = self.inner.store.cleanup(cap_mb * 1024 * 1024)?;
        // TMDB content kept at most media::TMDB_MAX_AGE_SECS (D42).
        let (_, files) = self.inner.store.expire_tmdb(magnetar_core::now())?;
        for f in files {
            let _ = std::fs::remove_file(f);
        }
        Ok(report.into())
    }

    /// Local liveness check (trackers + DHT); also stored in the cache.
    pub async fn check_liveness(&self, infohash: String) -> Result<Option<FfiLiveness>, FfiError> {
        let ih = norm_infohash(&infohash)?;
        self.run(move |inner| async move {
            // The server knows the torrent's own trackers even when the phone cache does not
            // (and keeps the result for everyone); scrape locally only without one.
            if let Some(api) = inner.api() {
                match api.refresh_liveness(&ih).await {
                    Ok(l) => {
                        inner.store.put_liveness_sync(&l)?;
                        return Ok(Some(l.into()));
                    }
                    Err(e) => tracing::warn!("server liveness {ih}: {e}; scraping from the phone"),
                }
            }
            let mut trackers = inner.store.trackers_of(&ih)?;
            if trackers.is_empty() {
                trackers = magnet::DEFAULT_TRACKERS
                    .iter()
                    .map(|s| s.to_string())
                    .collect();
            }
            let now = magnetar_core::now();
            let results =
                liveness::check_batch(&[(ih.clone(), trackers)], &LivenessConfig::default(), now)
                    .await;
            let Some(l) = results.into_iter().find(|l| l.infohash == ih) else {
                return Ok(None);
            };
            inner.store.put_liveness_sync(&l)?;
            Ok(Some(l.into()))
        })
        .await
    }

    pub fn set_pinned(&self, infohash: String, pinned: bool) -> Result<(), FfiError> {
        Ok(self
            .inner
            .store
            .set_pinned(&norm_infohash(&infohash)?, pinned)?)
    }

    pub fn set_hidden(&self, infohash: String, hidden: bool) -> Result<(), FfiError> {
        Ok(self
            .inner
            .store
            .set_hidden(&norm_infohash(&infohash)?, hidden)?)
    }

    /// Local override + server override when reachable.
    pub async fn block_publisher(&self, pubkey: String, blocked: bool) -> Result<(), FfiError> {
        let hex = parse_pubkey(&pubkey)?;
        let action = blocked.then_some(OverrideAction::Block);
        self.inner.store.set_override_sync(&hex, action)?;
        // Best effort from here on: rescore (refreshes denormalised trust), then tell the server.
        let store = self.inner.store.clone();
        let pk = hex.clone();
        let rescore = self
            .handle
            .spawn(async move { trust::score_publisher(&*store, &pk, magnetar_core::now()).await });
        match rescore.await {
            Ok(Ok(_)) => {}
            Ok(Err(e)) => tracing::warn!("rescore {hex}: {e}"),
            Err(e) => tracing::warn!("rescore {hex}: {e}"),
        }
        if let Some(api) = self.inner.api() {
            let pk = hex.clone();
            let remote = self
                .handle
                .spawn(async move { api.set_override(&pk, action).await });
            match remote.await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => tracing::warn!("server override {hex}: {e}"),
                Err(e) => tracing::warn!("server override {hex}: {e}"),
            }
        }
        Ok(())
    }

    pub fn saved_searches(&self) -> Result<Vec<FfiSavedSearch>, FfiError> {
        Ok(self
            .inner
            .store
            .list_saved_searches_sync()?
            .into_iter()
            .map(Into::into)
            .collect())
    }

    pub fn save_search(&self, search: FfiSavedSearch) -> Result<i64, FfiError> {
        let existing = match search.id {
            0 => None,
            id => self.inner.store.get_saved_search(id)?,
        };
        let s = SavedSearch {
            id: search.id,
            name: search.name,
            query: search.query.into(),
            notify: search.notify,
            last_checked: search.last_checked,
            last_seen_created_at: existing.and_then(|e| e.last_seen_created_at),
        };
        Ok(self.inner.store.put_saved_search_sync(&s)?)
    }

    pub fn delete_saved_search(&self, id: i64) -> Result<(), FfiError> {
        Ok(self.inner.store.delete_saved_search_sync(id)?)
    }

    /// New hits since the last poll for notify=true searches (server if reachable, else local).
    pub async fn poll_saved_searches(&self) -> Result<Vec<FfiSavedSearchHit>, FfiError> {
        self.run(move |inner| async move {
            let now = magnetar_core::now();
            let api = match inner.api() {
                Some(api) => match api.health().await {
                    Ok(h) => {
                        inner.note_features(&h.features);
                        Some(api)
                    }
                    Err(_) => None,
                },
                None => None,
            };
            let results = match api {
                Some(api) => inner.poll_via_server(&api, now).await?,
                None => notify::poll_saved_searches(&*inner.store, now).await?,
            };
            Ok(results
                .into_iter()
                .filter(|(_, hits)| !hits.is_empty())
                .map(|(s, hits)| FfiSavedSearchHit {
                    search: s.into(),
                    new_hits: hits.into_iter().map(Into::into).collect(),
                })
                .collect())
        })
        .await
    }

    pub fn stats(&self) -> Result<FfiStats, FfiError> {
        let store = &self.inner.store;
        let s = store.stats_sync()?;
        let ts = |key: &str| -> Result<Option<i64>, FfiError> {
            Ok(store
                .get_setting_sync(key)?
                .and_then(|v| v.trim().parse().ok()))
        };
        Ok(FfiStats {
            torrents: s.torrents,
            events: s.events,
            publishers: s.publishers,
            comments: s.comments,
            relays: s.relays,
            unsynced: s.unsynced,
            db_bytes: s.db_bytes,
            last_crawl: ts(LAST_CRAWL)?,
            last_sync: ts(LAST_SYNC)?,
        })
    }

    pub fn relays(&self) -> Result<Vec<FfiRelay>, FfiError> {
        Ok(self
            .inner
            .store
            .list_relays_sync()?
            .into_iter()
            .map(Into::into)
            .collect())
    }

    pub fn set_relay_enabled(&self, url: String, enabled: bool) -> Result<(), FfiError> {
        let url = url.trim();
        if url.is_empty() {
            return Err(FfiError::Invalid {
                msg: "empty relay url".into(),
            });
        }
        Ok(self.inner.store.set_relay_enabled_sync(url, enabled)?)
    }

    /// The server's seed-check backlog (D45). None: offline, unreachable, or a server older
    /// than D45 (the Status screen then leaves the card out).
    pub async fn server_liveness(&self) -> Option<FfiLivenessStats> {
        let api = self.inner.api()?;
        self.run(move |_| async move { Ok(api.stats().await?.liveness) })
            .await
            .ok()
            .flatten()
            .map(Into::into)
    }

    /// Seeders/leechers of one torrent over the last `days` (server only, D45). None: no
    /// server, an unknown infohash, or a server older than D45.
    pub async fn liveness_history(
        &self,
        infohash: String,
        days: u32,
    ) -> Result<Option<FfiLivenessHistory>, FfiError> {
        let ih = norm_infohash(&infohash)?;
        let Some(api) = self.inner.api() else {
            return Ok(None);
        };
        self.run(move |_| async move { Ok(api.liveness_history(&ih, days).await?.map(Into::into)) })
            .await
    }

    /// Ok(false) when the server is unreachable or unhealthy. `GET /health` needs no token, so a
    /// healthy server is also asked one authenticated question: a rejected token is an error
    /// (`Server{401}`), not "reachable".
    pub async fn server_health(&self) -> Result<bool, FfiError> {
        let Some(api) = self.inner.api() else {
            return Ok(false);
        };
        self.run(move |_| async move {
            match api.health().await {
                Ok(h) if h.status == "ok" => {}
                _ => return Ok(false),
            }
            api.trust_settings().await?;
            Ok(true)
        })
        .await
    }

    /// Phone engine (on_server=false) or server engine. Phone errors with Invalid if librqbit is unavailable.
    pub async fn download_start(
        &self,
        magnet: String,
        on_server: bool,
    ) -> Result<FfiDownload, FfiError> {
        if on_server {
            let api = self.inner.require_api()?;
            return self
                .run(move |_| async move { Ok(api.add_download(&magnet).await?.into()) })
                .await;
        }
        self.run(move |inner| async move {
            let engine = inner.downloader().await?;
            Ok(engine.add_magnet(&magnet).await?.into())
        })
        .await
    }

    pub async fn downloads(&self, on_server: bool) -> Result<Vec<FfiDownload>, FfiError> {
        if on_server {
            let api = self.inner.require_api()?;
            return self
                .run(move |_| async move {
                    Ok(api.downloads().await?.into_iter().map(Into::into).collect())
                })
                .await;
        }
        self.run(move |inner| async move {
            let engine = inner.downloader().await?;
            Ok(engine.list().into_iter().map(Into::into).collect())
        })
        .await
    }

    /// action: "pause" | "resume" | "delete"
    pub async fn download_action(
        &self,
        id: u64,
        action: String,
        on_server: bool,
    ) -> Result<(), FfiError> {
        if !matches!(action.as_str(), "pause" | "resume" | "delete") {
            return Err(FfiError::Invalid {
                msg: format!("unknown download action {action:?}"),
            });
        }
        if on_server {
            let api = self.inner.require_api()?;
            return self
                .run(move |_| async move { Ok(api.download_action(id, &action).await?) })
                .await;
        }
        self.run(move |inner| async move {
            let engine = inner.downloader().await?;
            match action.as_str() {
                "pause" => engine.pause(id).await?,
                "resume" => engine.resume(id).await?,
                // Keep the files: deleting user data needs its own explicit action.
                _ => engine.remove(id, false).await?,
            }
            Ok(())
        })
        .await
    }

    /// Pause every phone download and persist (foreground-service onTimeout).
    pub async fn pause_all_downloads(&self) {
        let _ = self
            .run(move |inner| async move {
                inner.pause_all().await;
                Ok(())
            })
            .await;
    }
}

impl Magnetar {
    /// Run `f` on our multi-thread runtime. uniffi's tokio support only wraps the future in
    /// async-compat (a single-threaded fallback runtime polled from the foreign thread), so all
    /// real work is spawned here and the JoinHandle is awaited. Panics become `FfiError::Other`.
    async fn run<T, F, Fut>(&self, f: F) -> Result<T, FfiError>
    where
        T: Send + 'static,
        F: FnOnce(Arc<Inner>) -> Fut,
        Fut: Future<Output = Result<T, FfiError>> + Send + 'static,
    {
        match self.handle.spawn(f(self.inner.clone())).await {
            Ok(r) => r,
            Err(e) => Err(FfiError::Other {
                msg: format!("task failed: {e}"),
            }),
        }
    }

    /// The store behind this instance (tests, diagnostics).
    pub fn store(&self) -> &Arc<SqliteStore> {
        &self.inner.store
    }
}

#[uniffi::export]
pub fn npub_encode(hex: String) -> Result<String, FfiError> {
    let hex = hex.trim().to_ascii_lowercase();
    let out = npub(&hex);
    if !is_hex32(&hex) || out == hex {
        return Err(FfiError::Invalid {
            msg: "pubkey must be 64 hex chars".into(),
        });
    }
    Ok(out)
}

/// npub / hex / nprofile / nostr: URI -> hex.
#[uniffi::export]
pub fn pubkey_decode(input: String) -> Result<String, FfiError> {
    Ok(parse_pubkey(&input)?)
}

#[uniffi::export]
pub fn build_magnet(infohash: String, name: Option<String>, trackers: Vec<String>) -> String {
    magnet::build_magnet(infohash.trim(), name.as_deref(), &trackers, None)
}

/// Display label of a filter value (D44): `facet_label("language", "fr-ca")` = "French
/// (Québec)", `facet_label("source", "web-dl")` = "WEB-DL".
#[uniffi::export]
pub fn facet_label(field: String, value: String) -> String {
    magnetar_core::release::facet_label(&field, &value)
}

/// The values offered for a filter when no counts are available (offline, index building):
/// "language", "subtitles", "resolution", "source", "codec", "hdr", "audio", "media_kind".
#[uniffi::export]
pub fn filter_options(field: String) -> Vec<String> {
    use magnetar_core::release as r;
    let list: &[&str] = match field.as_str() {
        "language" => &[
            "fr", "fr-fr", "fr-ca", "original", "multi", "en", "ja", "es", "es-419", "de", "it",
            "pt", "pt-br", "ko", "zh", "ru", "hi",
        ],
        "subtitles" => &[
            "fr", "en", "multi", "es", "de", "it", "pt", "ja", "zh", "ru",
        ],
        "resolution" => r::RESOLUTIONS,
        "source" => r::SOURCES,
        "codec" => r::VIDEO_CODECS,
        "hdr" => r::HDR_FORMATS,
        "audio" => r::AUDIO_CODECS,
        "media_kind" => &["movie", "series", "anime"],
        _ => &[],
    };
    list.iter().map(|s| s.to_string()).collect()
}

#[uniffi::export]
pub fn version() -> String {
    magnetar_core::VERSION.to_string()
}

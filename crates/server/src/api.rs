//! HTTP API (ARCHITECTURE.md §6): bearer auth on everything but `/health`, JSON errors.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::rejection::JsonRejection;
use axum::extract::{DefaultBodyLimit, Path, RawQuery, Request, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use magnetar_core::api::*;
use magnetar_core::downloads::Downloader;
use magnetar_core::error::Error;
use magnetar_core::ingest::{self, IngestOutcome};
use magnetar_core::liveness;
use magnetar_core::model::*;
use magnetar_core::query::{SearchPage, SearchQuery, Sort};
use magnetar_core::store::{DEFAULT_TRUST_THRESHOLD, Store, keys};
use magnetar_core::{relayurl, trust};
use tokio::sync::watch;
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::timeout::TimeoutLayer;
use tower_http::trace::TraceLayer;

use crate::dht::status::DhtService;
use crate::liveness::{LivenessService, PRIO_SEARCH, PRIO_VIEWED};
use crate::stats::StatsService;
use crate::store::{PgStore, json_u8};
use crate::tmdb::Tmdb;
use crate::{graphs, stats};

/// Longest a poster request waits for a first download from TMDB.
pub const POSTER_FETCH_TIMEOUT: Duration = Duration::from_secs(15);
/// Posters are revalidated by ETag after this.
pub const POSTER_MAX_AGE: u32 = 7 * 86400;

pub const BODY_LIMIT: usize = 8 << 20;
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
/// A cached trust score older than this is recomputed on read.
pub const TRUST_MAX_AGE: i64 = 6 * 3600;

#[derive(Clone)]
pub struct AppState {
    pub store: Arc<PgStore>,
    pub token: Arc<str>,
    pub downloader: Option<Arc<Downloader>>,
    pub crawler: Arc<Mutex<CrawlerStatus>>,
    /// Seed checker state (D45): forced checks use its config, searches and detail views
    /// boost what they show.
    pub liveness: Arc<LivenessService>,
    /// Flips to true on shutdown; long-lived WebSocket sessions watch it.
    pub shutdown: watch::Receiver<bool>,
    /// TMDB decoration (D42); None when `MAGNETAR_TMDB_TOKEN` is unset.
    pub tmdb: Option<Arc<Tmdb>>,
    /// Cached chart and graph answers (D47).
    pub stats: Arc<StatsService>,
    /// The DHT crawler's live state (D58); None when `MAGNETAR_DHT` is off.
    pub dht: Option<Arc<DhtService>>,
    /// Crawl workers (D96): tokens, limits, per-worker counters. No tokens = disabled.
    pub fleet: Arc<crate::fleet::Fleet>,
    /// Crawl map counters and geo database (D101–D105).
    pub crawlmap: Arc<crate::crawlmap::CrawlMap>,
}

impl AppState {
    /// State without downloads or a shutdown signal (see `with_shutdown`).
    pub fn new(store: Arc<PgStore>, token: &str) -> Self {
        let (_, rx) = watch::channel(false);
        Self {
            store,
            token: token.into(),
            downloader: None,
            crawler: Arc::default(),
            liveness: Arc::default(),
            shutdown: rx,
            tmdb: None,
            stats: Arc::default(),
            dht: None,
            fleet: Arc::default(),
            crawlmap: Arc::default(),
        }
    }

    pub fn with_crawlmap(mut self, c: Arc<crate::crawlmap::CrawlMap>) -> Self {
        self.crawlmap = c;
        self
    }

    pub fn with_fleet(mut self, f: Arc<crate::fleet::Fleet>) -> Self {
        self.fleet = f;
        self
    }

    pub fn with_dht(mut self, d: Option<Arc<DhtService>>) -> Self {
        self.dht = d;
        self
    }

    pub fn with_shutdown(mut self, rx: watch::Receiver<bool>) -> Self {
        self.shutdown = rx;
        self
    }

    pub fn with_downloader(mut self, d: Option<Arc<Downloader>>) -> Self {
        self.downloader = d;
        self
    }

    pub fn with_tmdb(mut self, t: Option<Arc<Tmdb>>) -> Self {
        self.tmdb = t;
        self
    }

    pub fn with_liveness(mut self, l: Arc<LivenessService>) -> Self {
        self.liveness = l;
        self
    }
}

/// `{"error": code, "message": ...}` with a status.
#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub code: &'static str,
    pub message: String,
}

impl ApiError {
    pub fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
        }
    }
    fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "invalid", message)
    }
    fn not_found(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, "not_found", message)
    }
}

impl From<Error> for ApiError {
    fn from(e: Error) -> Self {
        let (status, code) = match &e {
            Error::Invalid(_) | Error::Rejected(_) => (StatusCode::BAD_REQUEST, "invalid"),
            Error::Network(_) | Error::Relay { .. } | Error::Server { .. } => {
                (StatusCode::BAD_GATEWAY, "upstream")
            }
            Error::Timeout(_) => (StatusCode::GATEWAY_TIMEOUT, "timeout"),
            Error::Store(_) => (StatusCode::INTERNAL_SERVER_ERROR, "store"),
            Error::Other(_) => (StatusCode::INTERNAL_SERVER_ERROR, "internal"),
        };
        if status.is_server_error() {
            tracing::warn!("api error: {e}");
        }
        Self::new(status, code, e.to_string())
    }
}

impl From<JsonRejection> for ApiError {
    fn from(e: JsonRejection) -> Self {
        Self::new(e.status(), "invalid_body", e.body_text())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = ApiErrorBody {
            error: self.code.to_string(),
            message: self.message,
        };
        (self.status, Json(body)).into_response()
    }
}

type ApiResult<T> = Result<T, ApiError>;

/// Length-independent-ish constant-time comparison (length itself is not secret).
pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

pub(crate) fn bearer(headers: &HeaderMap) -> Option<&str> {
    let v = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, token) = v.split_once(' ')?;
    scheme.eq_ignore_ascii_case("bearer").then(|| token.trim())
}

fn query_token(query: Option<&str>) -> Option<String> {
    url::form_urlencoded::parse(query?.as_bytes())
        .find(|(k, _)| k == "token")
        .map(|(_, v)| v.into_owned())
}

async fn auth(State(st): State<AppState>, req: Request, next: Next) -> Response {
    let header_ok = bearer(req.headers()).is_some_and(|t| ct_eq(t.as_bytes(), st.token.as_bytes()));
    // The WebSocket endpoint also takes `?token=` (browsers/clients can't always set headers).
    let query_ok = !header_ok
        && req.uri().path() == "/relay"
        && query_token(req.uri().query()).is_some_and(|t| ct_eq(t.as_bytes(), st.token.as_bytes()));
    if header_ok || query_ok {
        next.run(req).await
    } else {
        ApiError::new(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "missing or bad bearer token",
        )
        .into_response()
    }
}

pub fn router(state: AppState) -> Router {
    let protected = Router::new()
        .route("/api/v1/search", get(search_get).post(search_post))
        .route("/api/v1/torrents/{infohash}", get(torrent))
        .route("/api/v1/media/{id}/poster", get(media_poster))
        .route(
            "/api/v1/torrents/{infohash}/liveness",
            post(refresh_liveness),
        )
        .route(
            "/api/v1/torrents/{infohash}/liveness-history",
            get(liveness_history),
        )
        .route("/api/v1/media/{id}/torrents", get(media_torrents))
        .route("/api/v1/facets", get(facets).post(facet_set))
        .route(
            "/api/v1/media/{id}/releases",
            get(media_releases_get).post(media_releases_post),
        )
        .route("/api/v1/events/batch", post(batch))
        .route("/api/v1/stats", get(stats))
        .route(
            "/api/v1/stats/breakdown",
            get(breakdown_get).post(breakdown_post),
        )
        .route("/api/v1/stats/timeseries", get(timeseries))
        .route("/api/v1/stats/health", get(stats_health))
        .route("/api/v1/stats/sources", get(stats_sources))
        .route("/api/v1/stats/map", get(stats_map))
        .route("/api/v1/graphs/wot", get(graph_wot))
        .route("/api/v1/graphs/relays", get(graph_relays))
        .route("/api/v1/graphs/media/{id}", get(graph_media))
        .route("/api/v1/graphs/swarm/{infohash}", get(graph_swarm))
        .route("/api/v1/relays", get(relays).put(set_relay))
        .route("/api/v1/trust/{pubkey}", get(trust_of))
        .route(
            "/api/v1/settings/trust",
            get(trust_settings).put(set_trust_settings),
        )
        .route("/api/v1/overrides/{pubkey}", put(set_override))
        .route(
            "/api/v1/saved-searches",
            get(saved_searches).post(put_saved_search),
        )
        .route("/api/v1/saved-searches/poll", get(poll_saved_searches))
        .route(
            "/api/v1/saved-searches/{id}",
            get(saved_search)
                .put(update_saved_search)
                .delete(delete_saved_search),
        )
        .route("/api/v1/downloads", get(downloads).post(add_download))
        .route("/api/v1/downloads/{id}", delete(remove_download))
        .route("/api/v1/downloads/{id}/{action}", post(download_action))
        .route("/relay", get(crate::relay::ws_handler))
        .route_layer(middleware::from_fn_with_state(state.clone(), auth));
    // Worker endpoints (D96): their own token check and a larger body limit (info dicts).
    let workers = crate::fleet::routes()
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            crate::fleet::worker_auth,
        ))
        .layer(DefaultBodyLimit::max(crate::fleet::BODY_LIMIT))
        .layer(RequestBodyLimitLayer::new(crate::fleet::BODY_LIMIT));
    Router::new()
        .route("/health", get(health))
        .merge(protected)
        .layer(DefaultBodyLimit::max(BODY_LIMIT))
        .layer(RequestBodyLimitLayer::new(BODY_LIMIT))
        .merge(workers)
        .fallback(|| async { ApiError::not_found("no such route") })
        .layer(TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            REQUEST_TIMEOUT,
        ))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

async fn health(State(st): State<AppState>) -> Json<Health> {
    Json(Health {
        status: "ok".into(),
        version: magnetar_core::VERSION.into(),
        db: st.store.ping().await,
        features: magnetar_core::api::features::ALL
            .iter()
            .copied()
            .chain(
                st.dht
                    .is_some()
                    .then_some(magnetar_core::api::features::DHT),
            )
            .chain(
                st.fleet
                    .enabled()
                    .then_some(magnetar_core::api::features::WORKERS),
            )
            .map(str::to_string)
            .collect(),
    })
}

fn parse_bool(v: &str) -> Result<bool, ApiError> {
    match v.to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" | "" => Ok(false),
        _ => Err(ApiError::bad_request(format!("bad boolean {v:?}"))),
    }
}

fn parse_num<T: std::str::FromStr>(k: &str, v: &str) -> Result<T, ApiError> {
    v.trim()
        .parse()
        .map_err(|_| ApiError::bad_request(format!("bad value for {k}: {v:?}")))
}

/// Comma-separated, repeatable list parameter (`lang=fr,en&lang=ja`), lowercased, deduplicated.
fn push_list(list: &mut Vec<String>, v: &str) {
    for x in v.split(',').map(|x| x.trim().to_lowercase()) {
        if !x.is_empty() && !list.contains(&x) {
            list.push(x);
        }
    }
}

/// Query-string form of `SearchQuery` (ARCHITECTURE.md §6). `desc` defaults to true.
pub fn parse_search_query(raw: Option<&str>) -> ApiResult<SearchQuery> {
    let mut q = SearchQuery {
        descending: true,
        ..Default::default()
    };
    for (k, v) in url::form_urlencoded::parse(raw.unwrap_or_default().as_bytes()) {
        match k.as_ref() {
            "q" => q.text = v.into_owned(),
            "cat" => {
                for c in v.split(',').map(str::trim).filter(|c| !c.is_empty()) {
                    let c: Category = c
                        .parse()
                        .map_err(|e: Error| ApiError::bad_request(e.to_string()))?;
                    if !q.categories.contains(&c) {
                        q.categories.push(c);
                    }
                }
            }
            "min_size" => q.min_size = Some(parse_num(&k, &v)?),
            "max_size" => q.max_size = Some(parse_num(&k, &v)?),
            "since" => q.since = Some(parse_num(&k, &v)?),
            "until" => q.until = Some(parse_num(&k, &v)?),
            "min_seeders" => q.min_seeders = Some(parse_num(&k, &v)?),
            "hide_dead" => q.hide_dead = parse_bool(&v)?,
            "adult" => q.include_adult = parse_bool(&v)?,
            "min_trust" => q.min_trust = Some(parse_num(&k, &v)?),
            "sort" => {
                q.sort = match v.as_ref() {
                    "relevance" => Sort::Relevance,
                    "date" => Sort::Date,
                    "size" => Sort::Size,
                    "seeders" => Sort::Seeders,
                    "rating" => Sort::Rating,
                    other => return Err(ApiError::bad_request(format!("bad sort {other:?}"))),
                }
            }
            "desc" => q.descending = parse_bool(&v)?,
            "limit" => q.limit = parse_num(&k, &v)?,
            "offset" => q.offset = parse_num(&k, &v)?,
            // D44 release/media filters.
            "lang" => push_list(&mut q.languages, &v),
            "sub" => push_list(&mut q.subtitles, &v),
            "res" => push_list(&mut q.resolutions, &v),
            "source" => push_list(&mut q.sources, &v),
            "codec" => push_list(&mut q.video_codecs, &v),
            "hdr" => push_list(&mut q.hdr, &v),
            "audio" => push_list(&mut q.audio, &v),
            "genre" => push_list(&mut q.genres, &v),
            // D51 source chip (`source` already means the release source, D44).
            "origin" => {
                push_list(&mut q.origins, &v);
                if let Some(bad) = q
                    .origins
                    .iter()
                    .find(|o| !matches!(o.as_str(), "nostr" | "dht"))
                {
                    return Err(ApiError::bad_request(format!(
                        "bad origin {bad:?} (nostr, dht)"
                    )));
                }
            }
            "kind" => {
                for k in v.split(',').map(str::trim).filter(|k| !k.is_empty()) {
                    let k: MediaKind = k
                        .parse()
                        .map_err(|e: Error| ApiError::bad_request(e.to_string()))?;
                    if !q.media_kinds.contains(&k) {
                        q.media_kinds.push(k);
                    }
                }
            }
            "min_rating" => {
                let r: f32 = parse_num(&k, &v)?;
                if !r.is_finite() {
                    return Err(ApiError::bad_request(format!("bad value for {k}: {v:?}")));
                }
                q.min_rating = Some(r);
            }
            "min_year" => q.min_year = Some(parse_num(&k, &v)?),
            "max_year" => q.max_year = Some(parse_num(&k, &v)?),
            _ => {}
        }
    }
    Ok(q)
}

async fn search_get(
    State(st): State<AppState>,
    RawQuery(raw): RawQuery,
) -> ApiResult<Json<SearchPage>> {
    let q = parse_search_query(raw.as_deref())?;
    search_and_boost(&st, &q).await
}

async fn search_post(
    State(st): State<AppState>,
    body: Result<Json<SearchQuery>, JsonRejection>,
) -> ApiResult<Json<SearchPage>> {
    let Json(q) = body?;
    search_and_boost(&st, &q).await
}

/// Search; what the page shows is checked sooner (D45).
async fn search_and_boost(st: &AppState, q: &SearchQuery) -> ApiResult<Json<SearchPage>> {
    let page = st.store.search(q, magnetar_core::now()).await?;
    st.liveness
        .boost(page.hits.iter().map(|h| h.infohash.as_str()), PRIO_SEARCH);
    Ok(Json(page))
}

/// Torrents linked to one media row (D40): the search query string applies (sort, limit, …);
/// `q` narrows within the title's releases.
async fn media_torrents(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    RawQuery(raw): RawQuery,
) -> ApiResult<Json<SearchPage>> {
    let mut q = parse_search_query(raw.as_deref())?;
    q.text = format!("media:{id} {}", q.text);
    Ok(Json(st.store.search(&q, magnetar_core::now()).await?))
}

/// `GET /api/v1/facets?field=category&size=20&<search parameters>` (D43): value counts of one
/// facet over the results of the search parameters. 503 until the search index is built.
async fn facets(
    State(st): State<AppState>,
    RawQuery(raw): RawQuery,
) -> ApiResult<Json<FacetsResponse>> {
    let q = parse_search_query(raw.as_deref())?;
    let mut field = None;
    let mut size = 20usize;
    for (k, v) in url::form_urlencoded::parse(raw.as_deref().unwrap_or_default().as_bytes()) {
        match k.as_ref() {
            "field" => field = Some(v.into_owned()),
            "size" => size = parse_num(&k, &v)?,
            _ => {}
        }
    }
    let field = field.ok_or_else(|| ApiError::bad_request("field is required"))?;
    st.store
        .facets(&q, &field, size, magnetar_core::now())
        .await?
        .map(Json)
        .ok_or_else(index_unavailable)
}

fn index_unavailable() -> ApiError {
    ApiError::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "search_index_unavailable",
        "the search index is disabled or still building",
    )
}

/// `POST /api/v1/facets` with `FacetsRequest` (D44): several facets over a full `SearchQuery`,
/// each counted without its own filter. 503 until the search index is built.
async fn facet_set(
    State(st): State<AppState>,
    body: Result<Json<FacetsRequest>, JsonRejection>,
) -> ApiResult<Json<FacetSet>> {
    let Json(r) = body?;
    let size = match r.size {
        0 => 20,
        n => n as usize,
    };
    st.store
        .facet_set(&r.query, &r.fields, size, magnetar_core::now())
        .await?
        .map(Json)
        .ok_or_else(index_unavailable)
}

/// `GET /api/v1/media/{id}/releases?<search parameters>` (D44): the releases of one
/// movie/series grouped by season/episode, resolution and language, with seeders.
async fn media_releases_get(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    RawQuery(raw): RawQuery,
) -> ApiResult<Json<MediaReleases>> {
    let q = parse_search_query(raw.as_deref())?;
    media_releases(&st, id, q).await
}

async fn media_releases_post(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    body: Result<Json<SearchQuery>, JsonRejection>,
) -> ApiResult<Json<MediaReleases>> {
    let Json(q) = body?;
    media_releases(&st, id, q).await
}

async fn media_releases(st: &AppState, id: i64, q: SearchQuery) -> ApiResult<Json<MediaReleases>> {
    let now = magnetar_core::now();
    let mut r = st
        .store
        .media_releases(id, &q, now)
        .await?
        .ok_or_else(|| ApiError::not_found("unknown media"))?;
    // Cached TMDB fields only (poster, overview), as for a torrent detail (D42).
    if let (Some(t), Some(m)) = (&st.tmdb, r.media.as_mut()) {
        t.decorate(m, now).await;
    }
    Ok(Json(r))
}

fn infohash_param(s: &str) -> ApiResult<String> {
    let s = s.trim().to_ascii_lowercase();
    if s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit()) {
        Ok(s)
    } else {
        Err(ApiError::bad_request("infohash must be 40 hex chars"))
    }
}

fn pubkey_param(s: &str) -> ApiResult<String> {
    Ok(parse_pubkey(s)?)
}

async fn torrent(
    State(st): State<AppState>,
    Path(infohash): Path<String>,
) -> ApiResult<Json<TorrentDetail>> {
    let ih = infohash_param(&infohash)?;
    let now = magnetar_core::now();
    let mut d = st
        .store
        .torrent_detail(&ih, now)
        .await?
        .ok_or_else(|| ApiError::not_found("unknown infohash"))?;
    st.liveness.boost([ih.as_str()], PRIO_VIEWED);
    // Cached TMDB fields only; anything missing is fetched in the background (D42).
    if let (Some(t), Some(m)) = (&st.tmdb, d.media.as_mut()) {
        t.decorate(m, now).await;
    }
    Ok(Json(d))
}

/// `GET /api/v1/media/{id}/poster`: the cached TMDB poster (w342), never a redirect to TMDB.
async fn media_poster(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let t = st
        .tmdb
        .as_ref()
        .ok_or_else(|| ApiError::not_found("TMDB is not configured"))?;
    let found = tokio::time::timeout(POSTER_FETCH_TIMEOUT, t.poster(id, magnetar_core::now()))
        .await
        .map_err(|_| ApiError::new(StatusCode::GATEWAY_TIMEOUT, "timeout", "poster download"))??;
    let (img, file) = found.ok_or_else(|| ApiError::not_found("no poster"))?;
    let etag = format!("\"{}\"", img.sha256);
    let cache = format!("private, max-age={POSTER_MAX_AGE}");
    let matches = headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.split(',').any(|t| t.trim() == etag || t.trim() == "*"));
    if matches {
        return Ok((
            StatusCode::NOT_MODIFIED,
            [(header::ETAG, etag), (header::CACHE_CONTROL, cache)],
        )
            .into_response());
    }
    let bytes = tokio::fs::read(&file)
        .await
        .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", e.to_string()))?;
    Ok((
        [
            (header::CONTENT_TYPE, img.content_type),
            (header::ETAG, etag),
            (header::CACHE_CONTROL, cache),
        ],
        bytes,
    )
        .into_response())
}

async fn refresh_liveness(
    State(st): State<AppState>,
    Path(infohash): Path<String>,
) -> ApiResult<Json<Liveness>> {
    let ih = infohash_param(&infohash)?;
    let trackers = st
        .store
        .trackers_for(&ih)
        .await?
        .ok_or_else(|| ApiError::not_found("unknown infohash"))?;
    let now = magnetar_core::now();
    // One-shot: a user waiting should not queue behind the checker's rate limits.
    let l = liveness::check_batch(&[(ih.clone(), trackers)], st.liveness.config(), now)
        .await
        .into_iter()
        .find(|l| l.infohash == ih)
        .ok_or_else(|| ApiError::new(StatusCode::BAD_GATEWAY, "upstream", "no liveness result"))?;
    crate::liveness::put_one(st.store.pool(), &l, crate::liveness::Expiry::Scheduled).await?;
    // What is stored now: the next due time as `expires_at`, or the previous answer when this
    // check got none (D45).
    Ok(Json(st.store.get_liveness(&ih).await?.unwrap_or(l)))
}

/// `GET /api/v1/torrents/{infohash}/liveness-history?days=N` (D45, default and max 90).
async fn liveness_history(
    State(st): State<AppState>,
    Path(infohash): Path<String>,
    RawQuery(raw): RawQuery,
) -> ApiResult<Json<LivenessHistory>> {
    let ih = infohash_param(&infohash)?;
    let mut days = crate::liveness::HISTORY_KEEP_DAYS;
    for (k, v) in url::form_urlencoded::parse(raw.as_deref().unwrap_or_default().as_bytes()) {
        if k == "days" {
            days = parse_num(&k, &v)?;
        }
    }
    crate::liveness::history(st.store.pool(), &ih, days, magnetar_core::now())
        .await?
        .map(Json)
        .ok_or_else(|| ApiError::not_found("unknown infohash"))
}

/// One NIP-01 style verdict per event (shared with the relay endpoint).
pub async fn ingest_one(store: &PgStore, raw: RawEvent, now: i64) -> BatchResult {
    let id = raw.id.clone();
    match ingest::ingest_into(store, raw, now).await {
        Ok(IngestOutcome::Inserted) => BatchResult {
            id,
            accepted: true,
            duplicate: false,
            message: String::new(),
        },
        Ok(IngestOutcome::Duplicate) => BatchResult {
            id,
            accepted: true,
            duplicate: true,
            message: "duplicate: already have this event".into(),
        },
        Ok(IngestOutcome::Rejected(r)) => BatchResult {
            id,
            accepted: false,
            duplicate: false,
            message: r.ok_message().into(),
        },
        Err(Error::Rejected(r)) => BatchResult {
            id,
            accepted: false,
            duplicate: false,
            message: r.ok_message().into(),
        },
        Err(e) => {
            tracing::warn!("ingest {id}: {e}");
            BatchResult {
                id,
                accepted: false,
                duplicate: false,
                message: format!("error: {e}"),
            }
        }
    }
}

async fn batch(
    State(st): State<AppState>,
    body: Result<Json<BatchRequest>, JsonRejection>,
) -> ApiResult<Json<BatchResponse>> {
    let Json(req) = body?;
    if req.events.len() > MAX_BATCH {
        return Err(ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "too_many_events",
            format!("at most {MAX_BATCH} events per batch"),
        ));
    }
    let now = magnetar_core::now();
    let mut results = Vec::with_capacity(req.events.len());
    for raw in req.events {
        results.push(ingest_one(&st.store, raw, now).await);
    }
    Ok(Json(BatchResponse { results }))
}

async fn stats(State(st): State<AppState>) -> ApiResult<Json<StatsResponse>> {
    let store = st.store.stats().await?;
    let crawler = st.crawler.lock().unwrap().clone();
    let search_index = st.store.search_index_status().await;
    let liveness = match st.liveness.stats(&st.store, magnetar_core::now()).await {
        Ok(l) => Some(l),
        Err(e) => {
            tracing::warn!("stats: liveness: {e}");
            None
        }
    };
    let dht = match &st.dht {
        Some(d) => Some(d.stats(st.store.pool()).await),
        None => None,
    };
    let workers = st.fleet.stats(st.store.pool(), magnetar_core::now()).await;
    Ok(Json(StatsResponse {
        store,
        crawler,
        search_index,
        liveness,
        dht,
        workers,
    }))
}

/// `GET /api/v1/stats/breakdown?field=language,resolution&top=10&<search parameters>` (D47).
async fn breakdown_get(
    State(st): State<AppState>,
    RawQuery(raw): RawQuery,
) -> ApiResult<Json<BreakdownSet>> {
    let query = parse_search_query(raw.as_deref())?;
    let mut r = BreakdownRequest {
        query,
        ..Default::default()
    };
    for (k, v) in url::form_urlencoded::parse(raw.as_deref().unwrap_or_default().as_bytes()) {
        match k.as_ref() {
            "field" => push_list(&mut r.fields, &v),
            "top" => r.top = parse_num(&k, &v)?,
            _ => {}
        }
    }
    breakdown(&st, &r).await
}

/// `POST /api/v1/stats/breakdown` with `BreakdownRequest` (D47).
async fn breakdown_post(
    State(st): State<AppState>,
    body: Result<Json<BreakdownRequest>, JsonRejection>,
) -> ApiResult<Json<BreakdownSet>> {
    let Json(r) = body?;
    breakdown(&st, &r).await
}

/// Top values per field, each counted under every filter but its own; 503 until the search
/// index is built.
async fn breakdown(st: &AppState, r: &BreakdownRequest) -> ApiResult<Json<BreakdownSet>> {
    stats::breakdown(&st.store, &st.stats, r, magnetar_core::now())
        .await?
        .map(|s| Json((*s).clone()))
        .ok_or_else(index_unavailable)
}

/// `GET /api/v1/stats/timeseries?metric=a,b&bucket=hour|day&since=<unix>` (D47).
async fn timeseries(
    State(st): State<AppState>,
    RawQuery(raw): RawQuery,
) -> ApiResult<Json<TimeseriesSet>> {
    let mut metrics = Vec::new();
    let mut bucket = "day".to_string();
    let mut since = None;
    for (k, v) in url::form_urlencoded::parse(raw.as_deref().unwrap_or_default().as_bytes()) {
        match k.as_ref() {
            "metric" => push_list(&mut metrics, &v),
            "bucket" => bucket = v.into_owned(),
            "since" => since = Some(parse_num(&k, &v)?),
            _ => {}
        }
    }
    let s = stats::timeseries(
        st.store.pool(),
        &st.stats,
        &metrics,
        &bucket,
        since,
        magnetar_core::now(),
    )
    .await?;
    Ok(Json((*s).clone()))
}

/// `GET /api/v1/stats/health` (D47): seed checks, index backlog, relays, crawler.
async fn stats_health(State(st): State<AppState>) -> ApiResult<Json<StatsHealth>> {
    let h = stats::health(
        &st.store,
        &st.stats,
        &st.liveness,
        &st.crawler,
        magnetar_core::now(),
    )
    .await?;
    let mut h = (*h).clone();
    // Live (the health answer is cached for a minute; the crawler's totals have their own).
    if let Some(d) = &st.dht {
        h.dht = Some(d.stats(st.store.pool()).await);
    }
    Ok(Json(h))
}

/// The server itself, then each crawl worker (D102). Live counters since the start.
pub fn crawl_machines(
    dht: Option<&DhtService>,
    liveness: &LivenessService,
    fleet: &crate::fleet::Fleet,
    now: i64,
) -> Vec<MachineSources> {
    let mut server = dht.and_then(|d| d.machine(now)).unwrap_or(MachineSources {
        name: "server".into(),
        kind: "server".into(),
        ..Default::default()
    });
    server.seed_checks = liveness.checked_total();
    let mut out = vec![server];
    out.extend(fleet.machines(now));
    out
}

fn store_error(e: sqlx::Error) -> ApiError {
    ApiError::new(
        StatusCode::INTERNAL_SERVER_ERROR,
        "store",
        crate::dht::persist::error_class(&e),
    )
}

/// `GET /api/v1/stats/sources` (D102).
async fn stats_sources(State(st): State<AppState>) -> ApiResult<Json<CrawlSources>> {
    let now = magnetar_core::now();
    let machines = crawl_machines(st.dht.as_deref(), &st.liveness, &st.fleet, now);
    let s = st
        .crawlmap
        .sources(st.store.pool(), machines, now)
        .await
        .map_err(store_error)?;
    Ok(Json(s))
}

/// `GET /api/v1/stats/map?layer=dht|nostr&window=day|all` (D103).
async fn stats_map(
    State(st): State<AppState>,
    RawQuery(raw): RawQuery,
) -> ApiResult<Json<magnetar_core::api::CrawlMap>> {
    let mut layer = crate::crawlmap::Layer::Dht;
    let mut day = true;
    for (k, v) in url::form_urlencoded::parse(raw.unwrap_or_default().as_bytes()) {
        match (k.as_ref(), v.as_ref()) {
            ("layer", "dht") => layer = crate::crawlmap::Layer::Dht,
            ("layer", "nostr") => layer = crate::crawlmap::Layer::Nostr,
            ("window", "day") => day = true,
            ("window", "all") => day = false,
            ("layer", v) => {
                return Err(ApiError::bad_request(format!(
                    "bad layer {v:?} (dht, nostr)"
                )));
            }
            ("window", v) => {
                return Err(ApiError::bad_request(format!(
                    "bad window {v:?} (day, all)"
                )));
            }
            _ => {}
        }
    }
    let m = st
        .crawlmap
        .map(st.store.pool(), layer, day, magnetar_core::now())
        .await
        .map_err(store_error)?;
    Ok(Json(m))
}

/// A graph through the shared cache; None (unknown media/infohash) answers 404.
async fn cached_graph<F, Fut>(
    cache: &stats::Cache<Option<Graph>>,
    key: String,
    make: F,
) -> ApiResult<Json<Graph>>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = magnetar_core::Result<Option<Graph>>>,
{
    cache
        .get_or(key, make)
        .await?
        .as_ref()
        .clone()
        .map(Json)
        .ok_or_else(|| ApiError::not_found("unknown media or infohash"))
}

async fn graph_wot(State(st): State<AppState>) -> ApiResult<Json<Graph>> {
    let store = st.store.clone();
    cached_graph(&st.stats.graphs, "wot".into(), || async move {
        graphs::wot(&store).await.map(Some)
    })
    .await
}

async fn graph_relays(State(st): State<AppState>) -> ApiResult<Json<Graph>> {
    let store = st.store.clone();
    cached_graph(&st.stats.graphs, "relays".into(), || async move {
        graphs::relays(&store, magnetar_core::now()).await.map(Some)
    })
    .await
}

async fn graph_media(State(st): State<AppState>, Path(id): Path<i64>) -> ApiResult<Json<Graph>> {
    let store = st.store.clone();
    cached_graph(&st.stats.graphs, format!("media/{id}"), || async move {
        graphs::media(&store, id, magnetar_core::now()).await
    })
    .await
}

async fn graph_swarm(
    State(st): State<AppState>,
    Path(infohash): Path<String>,
) -> ApiResult<Json<Graph>> {
    let ih = infohash_param(&infohash)?;
    let (store, live) = (st.store.clone(), st.liveness.clone());
    cached_graph(&st.stats.swarms, format!("swarm/{ih}"), || async move {
        graphs::swarm(&store, &live, &ih, magnetar_core::now()).await
    })
    .await
}

async fn relays(State(st): State<AppState>) -> ApiResult<Json<Vec<RelayInfo>>> {
    Ok(Json(st.store.list_relays().await?))
}

async fn set_relay(
    State(st): State<AppState>,
    body: Result<Json<RelayUpdate>, JsonRejection>,
) -> ApiResult<Json<Vec<RelayInfo>>> {
    let Json(u) = body?;
    let url = relayurl::normalize_relay(&u.url, true)
        .ok_or_else(|| ApiError::bad_request(format!("not a usable relay url: {}", u.url)))?;
    st.store.upsert_relay(&url, "manual").await?;
    st.store.set_relay_enabled(&url, u.enabled).await?;
    Ok(Json(st.store.list_relays().await?))
}

async fn trust_of(
    State(st): State<AppState>,
    Path(pubkey): Path<String>,
) -> ApiResult<Json<TrustScore>> {
    let pk = pubkey_param(&pubkey)?;
    let now = magnetar_core::now();
    let gv = st.store.graph_version().await?;
    if let Some(s) = st.store.get_trust(&pk).await?
        && s.graph_version >= gv
        && now - s.computed_at < TRUST_MAX_AGE
    {
        return Ok(Json(s));
    }
    Ok(Json(trust::score_publisher(&*st.store, &pk, now).await?))
}

async fn read_trust_settings(store: &PgStore) -> ApiResult<TrustSettings> {
    let root = store
        .get_setting(keys::TRUST_ROOT)
        .await?
        .and_then(|v| serde_json::from_str::<Option<String>>(&v).ok().flatten())
        .filter(|r| !r.is_empty());
    let threshold = store
        .get_setting(keys::TRUST_THRESHOLD)
        .await?
        .and_then(|v| json_u8(&v))
        .unwrap_or(DEFAULT_TRUST_THRESHOLD);
    Ok(TrustSettings { root, threshold })
}

async fn trust_settings(State(st): State<AppState>) -> ApiResult<Json<TrustSettings>> {
    Ok(Json(read_trust_settings(&st.store).await?))
}

async fn set_trust_settings(
    State(st): State<AppState>,
    body: Result<Json<TrustSettings>, JsonRejection>,
) -> ApiResult<Json<TrustSettings>> {
    let Json(s) = body?;
    if s.threshold > 100 {
        return Err(ApiError::bad_request("threshold must be 0..=100"));
    }
    let root = match s.root.as_deref().map(str::trim).filter(|r| !r.is_empty()) {
        Some(r) => Some(pubkey_param(r)?),
        None => None,
    };
    st.store
        .set_setting(
            keys::TRUST_ROOT,
            &serde_json::to_string(&root).expect("json"),
        )
        .await?;
    st.store
        .set_setting(keys::TRUST_THRESHOLD, &s.threshold.to_string())
        .await?;
    Ok(Json(read_trust_settings(&st.store).await?))
}

async fn set_override(
    State(st): State<AppState>,
    Path(pubkey): Path<String>,
    body: Result<Json<OverrideRequest>, JsonRejection>,
) -> ApiResult<StatusCode> {
    let Json(req) = body?;
    let pk = pubkey_param(&pubkey)?;
    st.store.set_override(&pk, req.action).await?;
    // Cached scores (and denormalised torrent trust) are refreshed by the trust daemon.
    st.store.bump_graph_version().await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn saved_searches(State(st): State<AppState>) -> ApiResult<Json<Vec<SavedSearch>>> {
    Ok(Json(st.store.list_saved_searches().await?))
}

async fn put_saved_search(
    State(st): State<AppState>,
    body: Result<Json<SavedSearch>, JsonRejection>,
) -> ApiResult<Json<SavedSearch>> {
    let Json(mut s) = body?;
    if s.name.trim().is_empty() {
        return Err(ApiError::bad_request("name must not be empty"));
    }
    s.id = st.store.put_saved_search(&s).await?;
    Ok(Json(s))
}

async fn saved_search(
    State(st): State<AppState>,
    Path(id): Path<i64>,
) -> ApiResult<Json<SavedSearch>> {
    st.store
        .list_saved_searches()
        .await?
        .into_iter()
        .find(|s| s.id == id)
        .map(Json)
        .ok_or_else(|| ApiError::not_found("no such saved search"))
}

/// `PUT /api/v1/saved-searches/{id}` (D77): name, query and notify from the body; the server's
/// own notifier watermarks (`last_checked`, `last_seen_created_at`) are kept, so editing a
/// search from a client never replays or skips its ntfy notifications.
async fn update_saved_search(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    body: Result<Json<SavedSearch>, JsonRejection>,
) -> ApiResult<Json<SavedSearch>> {
    let Json(body) = body?;
    if body.name.trim().is_empty() {
        return Err(ApiError::bad_request("name must not be empty"));
    }
    let Some(old) = st
        .store
        .list_saved_searches()
        .await?
        .into_iter()
        .find(|s| s.id == id)
    else {
        return Err(ApiError::not_found("no such saved search"));
    };
    // A new query starts over: its first notifier run records the newest match. Only the
    // edited columns are written, so a notifier run finishing meanwhile keeps its watermark.
    let reset = old.query != body.query;
    st.store
        .edit_saved_search(id, &body.name, &body.query, body.notify, reset)
        .await?
        .map(Json)
        .ok_or_else(|| ApiError::not_found("no such saved search"))
}

/// `GET /api/v1/saved-searches/poll?since=&adult=&id=&all=` (D77): the new hits of the saved
/// searches (those with `notify`, or all with `all=1`; `id` = comma list) since the client's
/// watermark. Stateless: see `core::notify::new_hits_since`.
async fn poll_saved_searches(
    State(st): State<AppState>,
    RawQuery(raw): RawQuery,
) -> ApiResult<Json<SavedSearchPoll>> {
    let mut since = None;
    let mut adult = false;
    let mut all = false;
    let mut ids: Vec<i64> = Vec::new();
    for (k, v) in url::form_urlencoded::parse(raw.unwrap_or_default().as_bytes()) {
        match k.as_ref() {
            "since" => since = Some(parse_num::<i64>(&k, &v)?),
            "adult" => adult = parse_bool(&v)?,
            "all" => all = parse_bool(&v)?,
            "id" => {
                for x in v.split(',').map(str::trim).filter(|x| !x.is_empty()) {
                    ids.push(parse_num(&k, x)?);
                }
            }
            // Unknown parameters are ignored, as in the search query string.
            _ => {}
        }
    }
    let searches: Vec<SavedSearch> = st
        .store
        .list_saved_searches()
        .await?
        .into_iter()
        .filter(|s| (all || s.notify) && (ids.is_empty() || ids.contains(&s.id)))
        .collect();
    let now = magnetar_core::now();
    let poll =
        magnetar_core::notify::new_hits_since(&*st.store, searches, since, adult, now).await?;
    Ok(Json(poll))
}

async fn delete_saved_search(
    State(st): State<AppState>,
    Path(id): Path<i64>,
) -> ApiResult<StatusCode> {
    st.store.delete_saved_search(id).await?;
    Ok(StatusCode::NO_CONTENT)
}

fn downloader(st: &AppState) -> ApiResult<&Downloader> {
    st.downloader.as_deref().ok_or_else(|| {
        ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "downloads_unavailable",
            "the download engine is not running",
        )
    })
}

async fn downloads(State(st): State<AppState>) -> ApiResult<Json<Vec<DownloadInfo>>> {
    Ok(Json(downloader(&st)?.list()))
}

async fn add_download(
    State(st): State<AppState>,
    body: Result<Json<DownloadRequest>, JsonRejection>,
) -> ApiResult<Json<DownloadInfo>> {
    let Json(req) = body?;
    Ok(Json(downloader(&st)?.add_magnet(&req.magnet).await?))
}

async fn download_action(
    State(st): State<AppState>,
    Path((id, action)): Path<(u64, String)>,
) -> ApiResult<StatusCode> {
    let d = downloader(&st)?;
    match action.as_str() {
        "pause" => d.pause(id).await?,
        "resume" => d.resume(id).await?,
        other => return Err(ApiError::not_found(format!("unknown action {other}"))),
    }
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /api/v1/downloads/{id}?delete_files=true` (files are kept by default).
async fn remove_download(
    State(st): State<AppState>,
    Path(id): Path<u64>,
    RawQuery(raw): RawQuery,
) -> ApiResult<StatusCode> {
    let delete_files = url::form_urlencoded::parse(raw.unwrap_or_default().as_bytes())
        .find(|(k, _)| k == "delete_files")
        .map(|(_, v)| parse_bool(&v))
        .transpose()?
        .unwrap_or(false);
    downloader(&st)?.remove(id, delete_files).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constant_time_eq() {
        assert!(ct_eq(b"secret", b"secret"));
        assert!(!ct_eq(b"secret", b"secreT"));
        assert!(!ct_eq(b"secret", b"secret2"));
    }

    #[test]
    fn search_query_string() {
        let q = parse_search_query(Some(
            "q=big+buck&cat=video,audio&cat=video&min_size=10&hide_dead=1&sort=size&desc=false&limit=5&offset=10&min_trust=0&min_seeders=2&since=1&until=2&max_size=99",
        ))
        .unwrap();
        assert_eq!(q.text, "big buck");
        assert_eq!(q.categories, vec![Category::Video, Category::Audio]);
        assert_eq!((q.min_size, q.max_size), (Some(10), Some(99)));
        assert!(q.hide_dead && !q.descending);
        assert_eq!(q.sort, Sort::Size);
        assert_eq!(
            (q.limit, q.offset, q.min_trust, q.min_seeders),
            (5, 10, Some(0), Some(2))
        );
        assert_eq!((q.since, q.until), (Some(1), Some(2)));
        let d = parse_search_query(None).unwrap();
        assert!(d.descending && d.text.is_empty() && d.min_trust.is_none());
        assert!(parse_search_query(Some("cat=nope")).is_err());
        assert!(parse_search_query(Some("limit=abc")).is_err());
        assert!(parse_search_query(Some("sort=random")).is_err());
    }

    #[test]
    fn tokens() {
        let mut h = HeaderMap::new();
        h.insert(header::AUTHORIZATION, "Bearer abc ".parse().unwrap());
        assert_eq!(bearer(&h), Some("abc"));
        h.insert(header::AUTHORIZATION, "Basic abc".parse().unwrap());
        assert_eq!(bearer(&h), None);
        assert_eq!(query_token(Some("a=1&token=x%2By")), Some("x+y".into()));
        assert_eq!(query_token(None), None);
    }
}

//! TMDB decoration (D42, docs/ENRICHMENT-TMDB.md): overviews, episode names and posters for
//! media already identified offline (IMDb datasets / anime lists). TMDB is only asked **by ID**
//! (`/find/{imdb}` once, then `/movie|tv/{id}`), lazily when a torrent detail is opened plus a
//! small background prefetch. Results live in `media_extra` (≤ 6 months, TMDB terms §1.C) and
//! posters on disk, content-addressed; the phone only talks to this server.
//!
//! Never republished anywhere (TMDB data is not redistributable).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use magnetar_core::error::{Error, Result};
use magnetar_core::model::{MediaInfo, MediaKind};
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::types::Json;
use sqlx::{FromRow, PgPool};
use tokio::sync::{mpsc, watch};

use crate::config::Config;
use crate::enrich::{ImdbAdoption, adopt_tmdb_imdb};
use crate::store::db;

pub const API_BASE: &str = "https://api.themoviedb.org/3";
pub const IMAGE_BASE: &str = "https://image.tmdb.org/t/p";
/// Poster variant cached and served (TMDB sizes: w92 w154 w185 w342 w500 w780 original).
pub const POSTER_SIZE: &str = "w342";
/// Details and episode names older than this are refetched when viewed or prefetched.
pub const REFRESH_SECS: i64 = 90 * 86400;
/// Hard limit: anything older is deleted (TMDB API terms §1.C: no caching beyond 6 months).
pub const PURGE_SECS: i64 = 180 * 86400;
/// A `/find` that matched nothing is retried after this.
pub const NEGATIVE_SECS: i64 = 30 * 86400;
/// The same job is not queued again within this window (covers failures too).
const REQUEUE_SECS: u64 = 15 * 60;
const MAX_IMAGE_BYTES: usize = 2 << 20;
const MAX_ATTEMPTS: u32 = 4;
const MAX_RETRY_WAIT: Duration = Duration::from_secs(60);
const QUEUE: usize = 256;
const WORKERS: usize = 2;
const PREFETCH_TICK: Duration = Duration::from_secs(10 * 60);
const PURGE_EVERY: Duration = Duration::from_secs(6 * 3600);

pub const SRC_DETAILS: &str = "tmdb";
pub const SRC_FIND: &str = "tmdb_find";
pub const SRC_IMAGE: &str = "tmdb_image";
/// D48: the IMDb id TMDB gave an IMDb-less row (`ImdbSeen`, lang ''), so the backfill does not
/// ask again before [`NEGATIVE_SECS`].
pub const SRC_IMDB: &str = "tmdb_imdb";
/// Rows per backfill query (D48).
pub const BACKFILL_BATCH: i64 = 100;
/// Pause between backfill passes (new TMDB-only rows keep appearing).
const BACKFILL_EVERY: Duration = Duration::from_secs(6 * 3600);
/// Wait after start before the first pass, so the enrichers and the index go first.
const BACKFILL_DELAY: Duration = Duration::from_secs(120);
/// A pass stops after this many failures in a row (bad token, TMDB down).
const BACKFILL_MAX_FAILURES: u32 = 5;

pub fn season_source(season: i32) -> String {
    format!("tmdb_season:{season}")
}

/// Everything the TMDB client needs; built from env by [`TmdbConfig::from_config`].
#[derive(Clone, Debug)]
pub struct TmdbConfig {
    pub token: String,
    /// TMDB language tags, first = preferred ("en-US", "fr-FR").
    pub langs: Vec<String>,
    pub rps: u32,
    pub image_dir: PathBuf,
    pub prefetch_per_hour: u32,
    pub api_base: String,
    pub image_base: String,
}

impl TmdbConfig {
    /// None when `MAGNETAR_TMDB_TOKEN` is unset or empty (TMDB off).
    pub fn from_config(cfg: &Config) -> Option<Self> {
        let token = cfg
            .tmdb_token
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty())?;
        let mut langs: Vec<String> = cfg
            .tmdb_langs
            .iter()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect();
        langs.dedup();
        if langs.is_empty() {
            langs.push("en-US".into());
        }
        Some(Self {
            token: token.to_string(),
            langs,
            rps: cfg.tmdb_rps.max(1),
            image_dir: cfg.image_cache_dir(),
            prefetch_per_hour: cfg.tmdb_prefetch_per_hour,
            api_base: API_BASE.into(),
            image_base: IMAGE_BASE.into(),
        })
    }
}

/// Token bucket: `rate` per second, bursts of `cap`.
struct Bucket {
    rate: f64,
    cap: f64,
    state: tokio::sync::Mutex<(f64, Instant)>,
}

impl Bucket {
    fn new(rps: u32) -> Self {
        let cap = f64::from(rps.clamp(1, 5));
        Self {
            rate: f64::from(rps.max(1)),
            cap,
            state: tokio::sync::Mutex::new((cap, Instant::now())),
        }
    }

    async fn take(&self) {
        loop {
            let wait = {
                let mut s = self.state.lock().await;
                let now = Instant::now();
                s.0 = (s.0 + now.duration_since(s.1).as_secs_f64() * self.rate).min(self.cap);
                s.1 = now;
                if s.0 >= 1.0 {
                    s.0 -= 1.0;
                    return;
                }
                Duration::from_secs_f64((1.0 - s.0) / self.rate)
            };
            tokio::time::sleep(wait).await;
        }
    }
}

/// `Retry-After` in seconds (HTTP-date form is not used by TMDB; it falls back to backoff).
fn retry_after(h: &reqwest::header::HeaderMap) -> Option<Duration> {
    let v = h.get(reqwest::header::RETRY_AFTER)?.to_str().ok()?;
    v.trim().parse::<u64>().ok().map(Duration::from_secs)
}

fn backoff(attempt: u32) -> Duration {
    Duration::from_secs(1 << attempt.min(5)).min(MAX_RETRY_WAIT)
}

/// One language's text for a movie or show (stored as `media_extra(source='tmdb', lang)`).
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct TmdbText {
    /// "movie" | "tv"
    pub tmdb_kind: String,
    pub tmdb_id: u32,
    pub title: Option<String>,
    pub overview: Option<String>,
    pub tagline: Option<String>,
    pub poster_path: Option<String>,
    pub backdrop_path: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Episode {
    pub episode: i32,
    pub name: Option<String>,
    pub overview: Option<String>,
    pub air_date: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct SeasonText {
    pub episodes: Vec<Episode>,
}

/// `/find` outcome (lang ''): the ids TMDB returned for the IMDb id, none = no match.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct FindResult {
    pub movie: Option<u32>,
    pub tv: Option<u32>,
}

/// What TMDB said about the IMDb id of an IMDb-less row (lang '', D48).
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct ImdbSeen {
    pub imdb: Option<String>,
}

/// The IMDb id in a details body: movies carry `imdb_id`, shows only through
/// `append_to_response=external_ids` (`external_ids.imdb_id`). Empty or malformed = None.
pub fn imdb_from_details(body: &Value) -> Option<u32> {
    [&body["imdb_id"], &body["external_ids"]["imdb_id"]]
        .into_iter()
        .filter_map(|v| v.as_str())
        .find_map(|s| magnetar_core::enrich::ids::imdb_num(s.trim()))
        .filter(|n| *n > 0)
}

/// A cached poster file (lang '').
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ImageRef {
    pub path: String,
    pub size: String,
    pub sha256: String,
    pub content_type: String,
    pub bytes: u64,
}

fn non_empty(v: &Value) -> Option<String> {
    v.as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// "fr-FR" -> ("fr", Some("FR")).
fn split_lang(lang: &str) -> (String, Option<String>) {
    match lang.split_once('-') {
        Some((l, r)) => (l.to_ascii_lowercase(), Some(r.to_ascii_uppercase())),
        None => (lang.to_ascii_lowercase(), None),
    }
}

/// Per configured language text from a `/movie|tv/{id}?append_to_response=translations`
/// body. The body itself is in `langs[0]`; the others come from `translations`, exact region
/// first, then any region of the same language.
pub fn texts_from_details(
    body: &Value,
    tmdb_kind: &str,
    tmdb_id: u32,
    langs: &[String],
) -> Vec<(String, TmdbText)> {
    let poster_path = non_empty(&body["poster_path"]);
    let backdrop_path = non_empty(&body["backdrop_path"]);
    let translations = body["translations"]["translations"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let find = |lang: &str| -> Option<&Value> {
        let (l, region) = split_lang(lang);
        let same_lang = |t: &&Value| t["iso_639_1"].as_str() == Some(l.as_str());
        translations
            .iter()
            .filter(same_lang)
            .find(|t| {
                region
                    .as_deref()
                    .is_some_and(|r| t["iso_3166_1"].as_str() == Some(r))
            })
            .or_else(|| translations.iter().find(same_lang))
            .map(|t| &t["data"])
    };
    let title_of = |v: &Value| non_empty(&v["title"]).or_else(|| non_empty(&v["name"]));
    langs
        .iter()
        .enumerate()
        .map(|(i, lang)| {
            let tr = find(lang);
            let pick = |k: &str| -> Option<String> {
                let main = (i == 0).then(|| non_empty(&body[k])).flatten();
                main.or_else(|| tr.and_then(|d| non_empty(&d[k])))
            };
            let title = if i == 0 {
                title_of(body).or_else(|| tr.and_then(title_of))
            } else {
                tr.and_then(title_of)
            };
            let text = TmdbText {
                tmdb_kind: tmdb_kind.to_string(),
                tmdb_id,
                title,
                overview: pick("overview"),
                tagline: pick("tagline"),
                poster_path: poster_path.clone(),
                backdrop_path: backdrop_path.clone(),
            };
            (lang.clone(), text)
        })
        .collect()
}

pub fn season_from_body(body: &Value) -> SeasonText {
    let episodes = body["episodes"]
        .as_array()
        .map(|eps| {
            eps.iter()
                .filter_map(|e| {
                    Some(Episode {
                        episode: i32::try_from(e["episode_number"].as_i64()?).ok()?,
                        name: non_empty(&e["name"]),
                        overview: non_empty(&e["overview"]),
                        air_date: non_empty(&e["air_date"]),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    SeasonText { episodes }
}

/// Which TMDB entry an IMDb id is, preferring the kind the offline matcher decided.
pub fn find_from_body(body: &Value, kind: MediaKind) -> FindResult {
    let first = |k: &str, id: &str| -> Option<u32> {
        body[k]
            .as_array()?
            .first()
            .and_then(|v| v[id].as_u64())
            .and_then(|v| u32::try_from(v).ok())
    };
    let movie = first("movie_results", "id");
    // An episode's IMDb id resolves to its show.
    let tv = first("tv_results", "id").or_else(|| first("tv_episode_results", "show_id"));
    match kind {
        MediaKind::Movie => FindResult {
            movie,
            tv: movie.is_none().then_some(tv).flatten(),
        },
        MediaKind::Series | MediaKind::Anime => FindResult {
            movie: tv.is_none().then_some(movie).flatten(),
            tv,
        },
    }
}

#[derive(FromRow)]
struct ExtraRow {
    source: String,
    lang: String,
    json: Json<Value>,
    fetched_at: i64,
}

/// The `media_extra` rows of one media, decoded.
#[derive(Default, Debug)]
pub struct Extras {
    pub details: HashMap<String, (TmdbText, i64)>,
    pub seasons: HashMap<(i32, String), (SeasonText, i64)>,
    pub find: Option<(FindResult, i64)>,
    pub image: Option<(ImageRef, i64)>,
}

impl Extras {
    fn from_rows(rows: Vec<ExtraRow>) -> Self {
        let mut x = Extras::default();
        for r in rows {
            let v = r.json.0;
            match r.source.as_str() {
                SRC_DETAILS => {
                    if let Ok(t) = serde_json::from_value(v) {
                        x.details.insert(r.lang, (t, r.fetched_at));
                    }
                }
                SRC_FIND => x.find = serde_json::from_value(v).ok().map(|f| (f, r.fetched_at)),
                SRC_IMAGE => x.image = serde_json::from_value(v).ok().map(|i| (i, r.fetched_at)),
                s => {
                    if let Some(n) = s.strip_prefix("tmdb_season:").and_then(|n| n.parse().ok())
                        && let Ok(t) = serde_json::from_value(v)
                    {
                        x.seasons.insert((n, r.lang), (t, r.fetched_at));
                    }
                }
            }
        }
        x
    }

    /// Oldest fetch time of the details rows (None = never fetched).
    fn details_age(&self) -> Option<i64> {
        self.details.values().map(|(_, at)| *at).min()
    }

    fn any_details(&self) -> Option<&TmdbText> {
        self.details.values().map(|(t, _)| t).next()
    }
}

pub async fn load_extras(pool: &PgPool, media_id: i64) -> Result<Extras> {
    let rows: Vec<ExtraRow> = sqlx::query_as(
        "SELECT source, lang, json, fetched_at FROM media_extra
         WHERE media_id = $1 AND source LIKE 'tmdb%'",
    )
    .bind(media_id)
    .fetch_all(pool)
    .await
    .map_err(db)?;
    Ok(Extras::from_rows(rows))
}

async fn put_extra<T: Serialize>(
    pool: &PgPool,
    media_id: i64,
    source: &str,
    lang: &str,
    v: &T,
    now: i64,
) -> Result<()> {
    let json = serde_json::to_value(v).map_err(|e| Error::Other(e.to_string()))?;
    sqlx::query(
        "INSERT INTO media_extra (media_id, source, lang, json, fetched_at)
         VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT (media_id, source, lang)
         DO UPDATE SET json = EXCLUDED.json, fetched_at = EXCLUDED.fetched_at",
    )
    .bind(media_id)
    .bind(source)
    .bind(lang)
    .bind(Json(json))
    .bind(now)
    .execute(pool)
    .await
    .map_err(db)?;
    Ok(())
}

#[derive(FromRow)]
struct MediaIds {
    kind: String,
    imdb: Option<String>,
    tmdb_movie: Option<i32>,
    tmdb_tv: Option<i32>,
}

/// What to fetch: a media, and the season whose episode names are wanted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Job {
    pub media_id: i64,
    pub season: Option<i32>,
}

/// What a refresh did (tests and the live measurement).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Refreshed {
    pub tmdb: Option<(MediaKind, u32)>,
    pub details: bool,
    pub season: bool,
    pub poster: bool,
    /// D48: an IMDb-less row learned its IMDb id from the details (set, or merged away).
    pub imdb: Option<ImdbAdoption>,
}

/// What one backfill run did (D48).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BackfillReport {
    pub checked: u64,
    pub set: u64,
    pub merged: u64,
    /// TMDB has no IMDb id for the entry (or no entry any more).
    pub no_imdb: u64,
    /// IMDb id held by an anime row, or the row changed meanwhile.
    pub skipped: u64,
    pub failed: u64,
    /// No candidate left.
    pub done: bool,
}

pub struct Tmdb {
    pub cfg: TmdbConfig,
    pool: PgPool,
    http: reqwest::Client,
    bucket: Bucket,
    queue: mpsc::Sender<Job>,
    queue_rx: Mutex<Option<mpsc::Receiver<Job>>>,
    recent: Mutex<HashMap<Job, Instant>>,
    requests: AtomicU64,
}

impl Tmdb {
    pub fn new(cfg: TmdbConfig, pool: PgPool) -> Result<Arc<Self>> {
        let (tx, rx) = mpsc::channel(QUEUE);
        Ok(Arc::new(Self {
            bucket: Bucket::new(cfg.rps),
            http: magnetar_core::http::client(Duration::from_secs(20))?,
            cfg,
            pool,
            queue: tx,
            queue_rx: Mutex::new(Some(rx)),
            recent: Mutex::new(HashMap::new()),
            requests: AtomicU64::new(0),
        }))
    }

    /// HTTP requests sent to TMDB (API + images, retries included) since start.
    pub fn requests(&self) -> u64 {
        self.requests.load(Ordering::Relaxed)
    }

    fn authorize(&self, rb: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        let t = &self.cfg.token;
        // A v3 API key is 32 hex chars; anything else is a v4 read access token.
        if t.len() == 32 && t.bytes().all(|b| b.is_ascii_hexdigit()) {
            rb.query(&[("api_key", t.as_str())])
        } else {
            rb.bearer_auth(t)
        }
    }

    /// GET with rate limit and retries (429/5xx/network, honouring Retry-After).
    /// Ok(None) on 404.
    async fn get(
        &self,
        url: &str,
        query: &[(&str, &str)],
        api: bool,
    ) -> Result<Option<reqwest::Response>> {
        let mut attempt = 0;
        loop {
            attempt += 1;
            self.bucket.take().await;
            let n = self.requests.fetch_add(1, Ordering::Relaxed) + 1;
            // Path only: never the query (a v3 key would travel there).
            tracing::debug!(
                "tmdb request #{n}: GET {}",
                url.split('?').next().unwrap_or("")
            );
            let mut rb = self.http.get(url).query(query);
            if api {
                rb = self.authorize(rb);
            }
            // `without_url` everywhere a reqwest error is kept: its Display appends the request
            // URL, query included, and a v3 key travels in the query.
            let wait = match rb.send().await.map_err(reqwest::Error::without_url) {
                Ok(r) if r.status().is_success() => return Ok(Some(r)),
                Ok(r) if r.status() == StatusCode::NOT_FOUND => return Ok(None),
                Ok(r)
                    if r.status() == StatusCode::TOO_MANY_REQUESTS
                        || r.status().is_server_error() =>
                {
                    let wait = retry_after(r.headers()).unwrap_or_else(|| backoff(attempt));
                    if attempt >= MAX_ATTEMPTS {
                        return Err(Error::Server {
                            status: r.status().as_u16(),
                            msg: format!("tmdb: gave up after {attempt} attempts"),
                        });
                    }
                    tracing::debug!("tmdb {}: retry in {wait:?}", r.status());
                    wait.min(MAX_RETRY_WAIT)
                }
                Ok(r) => {
                    let status = r.status().as_u16();
                    let body: String = r
                        .text()
                        .await
                        .unwrap_or_default()
                        .chars()
                        .take(200)
                        .collect();
                    return Err(Error::Server {
                        status,
                        msg: format!("tmdb: {body}"),
                    });
                }
                Err(e) if attempt < MAX_ATTEMPTS => {
                    tracing::debug!("tmdb: {e}; retrying");
                    backoff(attempt)
                }
                Err(e) => return Err(e.into()),
            };
            tokio::time::sleep(wait).await;
        }
    }

    async fn get_json(&self, path: &str, query: &[(&str, &str)]) -> Result<Option<Value>> {
        let url = format!("{}{path}", self.cfg.api_base);
        match self.get(&url, query, true).await? {
            Some(r) => Ok(Some(r.json().await.map_err(reqwest::Error::without_url)?)),
            None => Ok(None),
        }
    }

    /// Fill overview / poster / episode name from the cache and queue a fetch when something
    /// is missing or older than [`REFRESH_SECS`]. Never waits for TMDB.
    pub async fn decorate(&self, m: &mut MediaInfo, now: i64) {
        let x = match load_extras(&self.pool, m.media_id).await {
            Ok(x) => x,
            Err(e) => {
                tracing::debug!("media_extra {}: {e}", m.media_id);
                return;
            }
        };
        apply_extras(m, &x, &self.cfg.langs);
        let fresh = |at: i64| now - at < REFRESH_SECS;
        let details_ok = x.details_age().is_some_and(fresh)
            && self.cfg.langs.iter().all(|l| x.details.contains_key(l));
        let negative = x
            .find
            .as_ref()
            .is_some_and(|(f, at)| f.movie.is_none() && f.tv.is_none() && now - at < NEGATIVE_SECS);
        let season = m
            .season
            .filter(|_| m.episode.is_some() && m.kind != MediaKind::Movie);
        let season_ok = season.is_none_or(|s| {
            self.cfg.langs.iter().all(|l| {
                x.seasons
                    .get(&(s, l.clone()))
                    .is_some_and(|(_, at)| fresh(*at))
            })
        });
        if !negative && (!details_ok || !season_ok) {
            self.enqueue(Job {
                media_id: m.media_id,
                season,
            });
        }
    }

    /// Queue a background fetch; false if it is already queued/recent or the queue is full.
    pub fn enqueue(&self, job: Job) -> bool {
        {
            let mut recent = self.recent.lock().unwrap();
            let window = Duration::from_secs(REQUEUE_SECS);
            if recent.get(&job).is_some_and(|t| t.elapsed() < window) {
                return false;
            }
            if recent.len() > 10_000 {
                recent.retain(|_, t| t.elapsed() < window);
            }
            recent.insert(job, Instant::now());
        }
        self.queue.try_send(job).is_ok()
    }

    /// Start the queue workers (once).
    pub fn spawn_workers(
        self: &Arc<Self>,
        shutdown: watch::Receiver<bool>,
    ) -> Vec<tokio::task::JoinHandle<()>> {
        let Some(rx) = self.queue_rx.lock().unwrap().take() else {
            return Vec::new();
        };
        let rx = Arc::new(tokio::sync::Mutex::new(rx));
        (0..WORKERS)
            .map(|_| {
                let this = self.clone();
                let rx = rx.clone();
                let mut stop = shutdown.clone();
                tokio::spawn(async move {
                    loop {
                        let job = tokio::select! {
                            j = async { rx.lock().await.recv().await } => j,
                            _ = crate::daemons::stop_requested(&mut stop) => None,
                        };
                        let Some(job) = job else { break };
                        if let Err(e) = this.refresh(job, magnetar_core::now()).await {
                            tracing::warn!("tmdb media {}: {e}", job.media_id);
                        }
                    }
                })
            })
            .collect()
    }

    /// TMDB id of a media: stored id, cached `/find`, or a new `/find` by IMDb id.
    async fn resolve(
        &self,
        media_id: i64,
        now: i64,
        x: &Extras,
    ) -> Result<Option<(MediaKind, u32)>> {
        let ids: Option<MediaIds> =
            sqlx::query_as("SELECT kind, imdb, tmdb_movie, tmdb_tv FROM media WHERE id = $1")
                .bind(media_id)
                .fetch_optional(&self.pool)
                .await
                .map_err(db)?;
        let Some(ids) = ids else { return Ok(None) };
        let kind: MediaKind = ids.kind.parse().unwrap_or(MediaKind::Movie);
        let pos = |v: Option<i32>| v.and_then(|v| u32::try_from(v).ok()).filter(|v| *v > 0);
        let pick = |movie: Option<u32>, tv: Option<u32>| match kind {
            MediaKind::Movie => movie
                .map(|i| (MediaKind::Movie, i))
                .or(tv.map(|i| (MediaKind::Series, i))),
            _ => tv
                .map(|i| (MediaKind::Series, i))
                .or(movie.map(|i| (MediaKind::Movie, i))),
        };
        if let Some(t) = pick(pos(ids.tmdb_movie), pos(ids.tmdb_tv)) {
            return Ok(Some(t));
        }
        if let Some((f, at)) = &x.find
            && (f.movie.is_some() || f.tv.is_some() || now - at < NEGATIVE_SECS)
        {
            return Ok(pick(f.movie, f.tv));
        }
        let Some(imdb) = ids.imdb.filter(|i| i.starts_with("tt")) else {
            return Ok(None);
        };
        let body = self
            .get_json(&format!("/find/{imdb}"), &[("external_source", "imdb_id")])
            .await?
            .unwrap_or(Value::Null);
        let f = find_from_body(&body, kind);
        put_extra(&self.pool, media_id, SRC_FIND, "", &f, now).await?;
        // Record the id on the shared row too (enables `tmdb:` search), unless another media
        // already holds it (partial unique index) — then the media_extra row is enough.
        let (col, id) = match (f.movie, f.tv) {
            (Some(id), _) => ("tmdb_movie", id),
            (None, Some(id)) => ("tmdb_tv", id),
            (None, None) => return Ok(None),
        };
        let sql = format!("UPDATE media SET {col} = $1 WHERE id = $2 AND {col} IS NULL");
        if let Err(e) = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(id as i32)
            .bind(media_id)
            .execute(&self.pool)
            .await
        {
            tracing::debug!("media {media_id} {col}={id}: {e}");
        }
        Ok(pick(f.movie, f.tv))
    }

    /// Fetch what `job` needs that is missing or stale. One `/find` at most, one details call
    /// for every language (`append_to_response=translations`), one season call per language,
    /// one poster download.
    pub async fn refresh(&self, job: Job, now: i64) -> Result<Refreshed> {
        self.refresh_with(job, now, false).await
    }

    /// [`Tmdb::refresh`]; `backfill` (D48) forces the details call (it carries the IMDb id)
    /// and skips the poster.
    async fn refresh_with(&self, job: Job, now: i64, backfill: bool) -> Result<Refreshed> {
        let x = load_extras(&self.pool, job.media_id).await?;
        let mut out = Refreshed::default();
        let Some((kind, id)) = self.resolve(job.media_id, now, &x).await? else {
            return Ok(out);
        };
        out.tmdb = Some((kind, id));
        let (path_kind, kind_str) = match kind {
            MediaKind::Movie => ("movie", "movie"),
            _ => ("tv", "tv"),
        };
        let fresh = |at: i64| now - at < REFRESH_SECS;
        let same_target = x
            .any_details()
            .is_some_and(|t| t.tmdb_id == id && t.tmdb_kind == kind_str);
        let details_ok = same_target
            && x.details_age().is_some_and(fresh)
            && self.cfg.langs.iter().all(|l| x.details.contains_key(l));
        let mut poster_path = x
            .any_details()
            .filter(|_| same_target)
            .and_then(|t| t.poster_path.clone());
        if !details_ok || backfill {
            let lang = self.cfg.langs[0].as_str();
            // Movie details carry `imdb_id`; a show's IMDb id is in its external ids.
            let append = match kind {
                MediaKind::Movie => "translations",
                _ => "translations,external_ids",
            };
            let body = self
                .get_json(
                    &format!("/{path_kind}/{id}"),
                    &[("language", lang), ("append_to_response", append)],
                )
                .await?;
            if let Some(body) = &body {
                let texts = texts_from_details(body, kind_str, id, &self.cfg.langs);
                poster_path = texts.first().and_then(|(_, t)| t.poster_path.clone());
                for (lang, t) in texts {
                    put_extra(&self.pool, job.media_id, SRC_DETAILS, &lang, &t, now).await?;
                }
                out.details = true;
            }
            out.imdb = self.learn_imdb(job.media_id, body.as_ref(), now).await?;
            if let Some(ImdbAdoption::Merged(keep)) = out.imdb {
                // This row is gone (its media_extra with it); decorate the row it joined.
                if !backfill {
                    self.enqueue(Job {
                        media_id: keep,
                        season: job.season,
                    });
                }
                return Ok(out);
            }
        }
        if let Some(season) = job.season.filter(|_| kind != MediaKind::Movie) {
            let src = season_source(season);
            for lang in &self.cfg.langs {
                if x.seasons
                    .get(&(season, lang.clone()))
                    .is_some_and(|(_, at)| fresh(*at))
                {
                    continue;
                }
                let body = self
                    .get_json(&format!("/tv/{id}/season/{season}"), &[("language", lang)])
                    .await?;
                // A missing season is cached empty so it is not asked again until refresh.
                let s = body.as_ref().map(season_from_body).unwrap_or_default();
                put_extra(&self.pool, job.media_id, &src, lang, &s, now).await?;
                out.season = true;
            }
        }
        if let Some(p) = poster_path.filter(|_| !backfill) {
            let cached = x
                .image
                .as_ref()
                .is_some_and(|(i, at)| i.path == p && fresh(*at) && self.image_file(i).is_file());
            if !cached {
                out.poster = self.download_poster(job.media_id, &p, now).await?.is_some();
            }
        }
        Ok(out)
    }

    /// D48: give an IMDb-less, non-anime row the IMDb id of its TMDB details (`body`; None =
    /// TMDB answered 404), merging it into the row that already has that id. Records what was
    /// seen ([`SRC_IMDB`]) so the backfill does not ask again soon.
    async fn learn_imdb(
        &self,
        media_id: i64,
        body: Option<&Value>,
        now: i64,
    ) -> Result<Option<ImdbAdoption>> {
        let wanted: Option<bool> =
            sqlx::query_scalar("SELECT imdb IS NULL AND kind <> 'anime' FROM media WHERE id = $1")
                .bind(media_id)
                .fetch_optional(&self.pool)
                .await
                .map_err(db)?;
        if wanted != Some(true) {
            return Ok(None);
        }
        let tt = body.and_then(imdb_from_details);
        let seen = ImdbSeen {
            imdb: tt.map(|n| magnetar_core::enrich::ids::format_imdb(u64::from(n))),
        };
        put_extra(&self.pool, media_id, SRC_IMDB, "", &seen, now).await?;
        let Some(tt) = tt else { return Ok(None) };
        let r = adopt_tmdb_imdb(&self.pool, media_id, tt, now)
            .await
            .map_err(|e| Error::Other(format!("media {media_id} imdb tt{tt}: {e:#}")))?;
        tracing::debug!("tmdb: media {media_id} is tt{tt}: {r:?}");
        Ok(Some(r))
    }

    /// IMDb-less, non-anime rows with a TMDB id that TMDB was not asked about recently, by id
    /// after `after` (D48 backfill).
    pub async fn backfill_candidates(&self, now: i64, after: i64, limit: i64) -> Result<Vec<i64>> {
        sqlx::query_scalar(
            "SELECT m.id FROM media m
             WHERE m.imdb IS NULL AND (m.tmdb_movie IS NOT NULL OR m.tmdb_tv IS NOT NULL)
               AND m.kind <> 'anime' AND m.id > $1
               AND NOT EXISTS (SELECT 1 FROM media_extra e WHERE e.media_id = m.id
                   AND e.source = 'tmdb_imdb' AND e.fetched_at > $2)
             ORDER BY m.id LIMIT $3",
        )
        .bind(after)
        .bind(now - NEGATIVE_SECS)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(db)
    }

    /// D48 backfill: one details call per IMDb-less row with a TMDB id (paced by the shared
    /// token bucket), at most `max` rows. Resumable: a row asked is marked ([`SRC_IMDB`]) or
    /// leaves the candidates (IMDb id set, or merged away), so a later run continues where an
    /// interrupted one stopped. Stops with an error after [`BACKFILL_MAX_FAILURES`] failures
    /// in a row.
    pub async fn backfill_imdb(&self, now: i64, max: u64) -> Result<BackfillReport> {
        let mut rep = BackfillReport::default();
        let mut after = 0i64;
        let mut failures = 0u32;
        loop {
            let left = max.saturating_sub(rep.checked);
            if left == 0 {
                return Ok(rep);
            }
            let ids = self
                .backfill_candidates(
                    now,
                    after,
                    BACKFILL_BATCH.min(i64::try_from(left).unwrap_or(i64::MAX)),
                )
                .await?;
            if ids.is_empty() {
                rep.done = true;
                return Ok(rep);
            }
            for media_id in ids {
                after = media_id;
                rep.checked += 1;
                let job = Job {
                    media_id,
                    season: None,
                };
                match self.refresh_with(job, now, true).await {
                    Ok(r) => {
                        failures = 0;
                        match r.imdb {
                            Some(ImdbAdoption::Set) => rep.set += 1,
                            Some(ImdbAdoption::Merged(_)) => rep.merged += 1,
                            Some(ImdbAdoption::Skipped) => rep.skipped += 1,
                            None if r.tmdb.is_some() => rep.no_imdb += 1,
                            None => rep.skipped += 1,
                        }
                    }
                    Err(e) => {
                        rep.failed += 1;
                        failures += 1;
                        tracing::debug!("tmdb backfill: media {media_id}: {e}");
                        if failures >= BACKFILL_MAX_FAILURES {
                            return Err(Error::Other(format!(
                                "tmdb backfill: {failures} failures in a row, last: {e}"
                            )));
                        }
                    }
                }
            }
        }
    }

    pub fn image_file(&self, i: &ImageRef) -> PathBuf {
        image_path(&self.cfg.image_dir, &i.sha256, &i.content_type)
    }

    /// Download `{image_base}/w342{path}` into the content-addressed cache.
    async fn download_poster(
        &self,
        media_id: i64,
        path: &str,
        now: i64,
    ) -> Result<Option<ImageRef>> {
        if !path.starts_with('/') || path.contains("..") {
            return Err(Error::Invalid(format!("bad poster path {path:?}")));
        }
        let url = format!("{}/{POSTER_SIZE}{path}", self.cfg.image_base);
        let Some(resp) = self.get(&url, &[], false).await? else {
            return Ok(None);
        };
        let content_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(|v| {
                v.split(';')
                    .next()
                    .unwrap_or("")
                    .trim()
                    .to_ascii_lowercase()
            })
            .unwrap_or_default();
        if !matches!(
            content_type.as_str(),
            "image/jpeg" | "image/png" | "image/webp"
        ) {
            return Err(Error::Invalid(format!(
                "poster content-type {content_type:?}"
            )));
        }
        let bytes = resp.bytes().await.map_err(reqwest::Error::without_url)?;
        if bytes.is_empty() || bytes.len() > MAX_IMAGE_BYTES {
            return Err(Error::Invalid(format!("poster of {} bytes", bytes.len())));
        }
        let sha = hex::encode(Sha256::digest(&bytes));
        let file = image_path(&self.cfg.image_dir, &sha, &content_type);
        if !file.is_file() {
            write_atomic(&file, &bytes).await?;
        }
        let r = ImageRef {
            path: path.to_string(),
            size: POSTER_SIZE.into(),
            sha256: sha,
            content_type,
            bytes: bytes.len() as u64,
        };
        put_extra(&self.pool, media_id, SRC_IMAGE, "", &r, now).await?;
        Ok(Some(r))
    }

    /// The cached poster of a media; when only its TMDB path is known, download it now.
    pub async fn poster(&self, media_id: i64, now: i64) -> Result<Option<(ImageRef, PathBuf)>> {
        let x = load_extras(&self.pool, media_id).await?;
        let path = x.any_details().and_then(|t| t.poster_path.clone());
        if let Some((i, _)) = &x.image
            && path.as_deref().is_none_or(|p| p == i.path)
        {
            let f = self.image_file(i);
            if f.is_file() {
                return Ok(Some((i.clone(), f)));
            }
        }
        let Some(path) = path else { return Ok(None) };
        Ok(self.download_poster(media_id, &path, now).await?.map(|i| {
            let f = self.image_file(&i);
            (i, f)
        }))
    }

    /// Recently matched media without fresh TMDB data, most releases first.
    pub async fn prefetch_candidates(&self, now: i64, limit: i64) -> Result<Vec<i64>> {
        let ids: Vec<(i64,)> = sqlx::query_as(
            "SELECT tm.media_id FROM torrent_media tm JOIN media m ON m.id = tm.media_id
             WHERE tm.matched_at > $1 AND tm.confidence >= $5
               AND (m.imdb IS NOT NULL OR m.tmdb_movie IS NOT NULL OR m.tmdb_tv IS NOT NULL)
               AND NOT EXISTS (SELECT 1 FROM media_extra e WHERE e.media_id = tm.media_id
                   AND ((e.source = 'tmdb' AND e.fetched_at > $2)
                     OR (e.source = 'tmdb_find' AND e.fetched_at > $3)))
             GROUP BY tm.media_id
             ORDER BY count(*) DESC, max(m.votes) DESC NULLS LAST, max(tm.matched_at) DESC
             LIMIT $4",
        )
        .bind(now - 7 * 86400)
        .bind(now - REFRESH_SECS)
        .bind(now - NEGATIVE_SECS)
        .bind(limit)
        .bind(magnetar_core::enrich::MIN_DISPLAY_CONFIDENCE)
        .fetch_all(&self.pool)
        .await
        .map_err(db)?;
        Ok(ids.into_iter().map(|(id,)| id).collect())
    }

    /// Delete TMDB rows older than [`PURGE_SECS`] and poster files no row references.
    pub async fn purge(&self, now: i64) -> Result<(u64, u64)> {
        let rows =
            sqlx::query("DELETE FROM media_extra WHERE source LIKE 'tmdb%' AND fetched_at < $1")
                .bind(now - PURGE_SECS)
                .execute(&self.pool)
                .await
                .map_err(db)?
                .rows_affected();
        let keep: Vec<(String,)> = sqlx::query_as(
            "SELECT DISTINCT json->>'sha256' FROM media_extra WHERE source = 'tmdb_image'",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(db)?;
        let keep: std::collections::HashSet<String> = keep.into_iter().map(|(s,)| s).collect();
        let dir = self.cfg.image_dir.clone();
        let files = tokio::task::spawn_blocking(move || remove_unreferenced(&dir, &keep))
            .await
            .map_err(|e| Error::Other(e.to_string()))?;
        Ok((rows, files))
    }
}

/// Overview (first language that has one), poster URL, episode name and TMDB id from cache.
pub fn apply_extras(m: &mut MediaInfo, x: &Extras, langs: &[String]) {
    let texts = || {
        langs
            .iter()
            .filter_map(|l| x.details.get(l).map(|(t, _)| t))
    };
    if let Some(o) = texts().find_map(|t| t.overview.clone()) {
        m.overview = Some(o);
    }
    if texts().any(|t| t.poster_path.is_some()) || x.image.is_some() {
        m.poster = Some(format!("/api/v1/media/{}/poster", m.media_id));
    }
    if m.tmdb.is_none()
        && let Some(t) = texts().next()
    {
        let k = if t.tmdb_kind == "movie" {
            MediaKind::Movie
        } else {
            MediaKind::Series
        };
        m.tmdb = Some((k, t.tmdb_id));
    }
    if let (Some(s), Some(e)) = (m.season, m.episode) {
        m.episode_title = langs.iter().find_map(|l| {
            x.seasons
                .get(&(s, l.clone()))
                .and_then(|(st, _)| st.episodes.iter().find(|ep| ep.episode == e))
                .and_then(|ep| ep.name.clone())
        });
    }
}

fn ext_of(content_type: &str) -> &'static str {
    match content_type {
        "image/png" => "png",
        "image/webp" => "webp",
        _ => "jpg",
    }
}

/// `<dir>/<sha[0..2]>/<sha>.<ext>`.
pub fn image_path(dir: &Path, sha: &str, content_type: &str) -> PathBuf {
    let shard = sha.get(..2).unwrap_or("00");
    dir.join(shard)
        .join(format!("{sha}.{}", ext_of(content_type)))
}

async fn write_atomic(file: &Path, bytes: &[u8]) -> Result<()> {
    let io = |e: std::io::Error| Error::Other(format!("{}: {e}", file.display()));
    if let Some(dir) = file.parent() {
        tokio::fs::create_dir_all(dir).await.map_err(io)?;
    }
    let tmp = file.with_extension(format!("tmp{}", rand::random::<u32>()));
    tokio::fs::write(&tmp, bytes).await.map_err(io)?;
    tokio::fs::rename(&tmp, file).await.map_err(io)
}

fn remove_unreferenced(dir: &Path, keep: &std::collections::HashSet<String>) -> u64 {
    let mut removed = 0;
    let Ok(shards) = std::fs::read_dir(dir) else {
        return 0;
    };
    for shard in shards.flatten() {
        let Ok(files) = std::fs::read_dir(shard.path()) else {
            continue;
        };
        for f in files.flatten() {
            let p = f.path();
            let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("");
            if !keep.contains(stem) && std::fs::remove_file(&p).is_ok() {
                removed += 1;
            }
        }
    }
    removed
}

/// D48: backfill pass shortly after start, then every [`BACKFILL_EVERY`] (rows created from
/// TMDB-only releases since then). Only runs with a TMDB token (no `Tmdb` otherwise).
pub async fn backfill_loop(tmdb: Arc<Tmdb>, mut shutdown: watch::Receiver<bool>) {
    let mut wait = BACKFILL_DELAY;
    loop {
        tokio::select! {
            _ = tokio::time::sleep(wait) => {}
            _ = crate::daemons::stop_requested(&mut shutdown) => return,
        }
        wait = BACKFILL_EVERY;
        let started = Instant::now();
        let run = tmdb.backfill_imdb(magnetar_core::now(), u64::MAX);
        tokio::select! {
            r = run => match r {
                Ok(r) if r.checked > 0 => tracing::info!(
                    checked = r.checked, set = r.set, merged = r.merged, no_imdb = r.no_imdb,
                    skipped = r.skipped, failed = r.failed,
                    "tmdb imdb backfill: done in {:.0} s", started.elapsed().as_secs_f64()
                ),
                Ok(_) => {}
                Err(e) => tracing::warn!("{e}"),
            },
            _ = crate::daemons::stop_requested(&mut shutdown) => return,
        }
    }
}

/// Background prefetch of recently matched popular media plus the 6-month purge.
pub async fn prefetch_loop(tmdb: Arc<Tmdb>, mut shutdown: watch::Receiver<bool>) {
    let per_tick = (i64::from(tmdb.cfg.prefetch_per_hour) + 5) / 6;
    let mut last_purge: Option<Instant> = None;
    loop {
        let now = magnetar_core::now();
        if last_purge.is_none_or(|t| t.elapsed() >= PURGE_EVERY) {
            match tmdb.purge(now).await {
                Ok((rows, files)) if rows + files > 0 => {
                    tracing::info!("tmdb purge: {rows} rows, {files} images older than 6 months")
                }
                Ok(_) => {}
                Err(e) => tracing::warn!("tmdb purge: {e}"),
            }
            last_purge = Some(Instant::now());
        }
        if per_tick > 0 {
            match tmdb.prefetch_candidates(now, per_tick).await {
                Ok(ids) => {
                    for media_id in ids {
                        tmdb.enqueue(Job {
                            media_id,
                            season: None,
                        });
                    }
                }
                Err(e) => tracing::debug!("tmdb prefetch: {e}"),
            }
        }
        tokio::select! {
            _ = tokio::time::sleep(PREFETCH_TICK) => {}
            _ = crate::daemons::stop_requested(&mut shutdown) => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn langs() -> Vec<String> {
        vec!["en-US".into(), "fr-FR".into()]
    }

    #[test]
    fn details_per_language() {
        let body = json!({
            "id": 27205, "title": "Inception", "overview": "A thief…", "tagline": "Your mind",
            "poster_path": "/p.jpg", "backdrop_path": "/b.jpg",
            "translations": {"translations": [
                {"iso_639_1": "fr", "iso_3166_1": "CA", "data": {"title": "Origine", "overview": "QC"}},
                {"iso_639_1": "fr", "iso_3166_1": "FR", "data": {"title": "Inception", "overview": "Dom Cobb…", "tagline": ""}},
                {"iso_639_1": "en", "iso_3166_1": "US", "data": {"title": "", "overview": "en tr"}}
            ]}
        });
        let t = texts_from_details(&body, "movie", 27205, &langs());
        assert_eq!(t[0].0, "en-US");
        assert_eq!(t[0].1.overview.as_deref(), Some("A thief…"));
        assert_eq!(t[0].1.title.as_deref(), Some("Inception"));
        assert_eq!(t[1].1.overview.as_deref(), Some("Dom Cobb…"));
        assert_eq!(t[1].1.tagline, None);
        assert_eq!(t[1].1.poster_path.as_deref(), Some("/p.jpg"));
        // Same language, other region, when the exact one is missing.
        let t = texts_from_details(&body, "movie", 1, &["fr-BE".to_string()]);
        assert_eq!(t[0].1.overview.as_deref(), Some("A thief…"));
        let t = texts_from_details(&body, "movie", 1, &["en-US".into(), "fr-BE".into()]);
        assert_eq!(t[1].1.overview.as_deref(), Some("QC"));
        // TV uses `name`; empty main overview falls back to the translation.
        let tv = json!({"name": "Dark", "overview": "", "translations": {"translations": [
            {"iso_639_1": "en", "iso_3166_1": "US", "data": {"name": "Dark", "overview": "Kids vanish"}}]}});
        let t = texts_from_details(&tv, "tv", 70523, &langs());
        assert_eq!(t[0].1.title.as_deref(), Some("Dark"));
        assert_eq!(t[0].1.overview.as_deref(), Some("Kids vanish"));
        assert_eq!(t[1].1.overview, None);
    }

    #[test]
    fn find_prefers_matched_kind() {
        let b = json!({"movie_results": [{"id": 5}], "tv_results": [{"id": 7}], "tv_episode_results": []});
        assert_eq!(
            find_from_body(&b, MediaKind::Movie),
            FindResult {
                movie: Some(5),
                tv: None
            }
        );
        assert_eq!(
            find_from_body(&b, MediaKind::Series),
            FindResult {
                movie: None,
                tv: Some(7)
            }
        );
        let ep = json!({"movie_results": [], "tv_results": [], "tv_episode_results": [{"id": 1, "show_id": 1399}]});
        assert_eq!(find_from_body(&ep, MediaKind::Series).tv, Some(1399));
        assert_eq!(
            find_from_body(&Value::Null, MediaKind::Anime),
            FindResult::default()
        );
    }

    #[test]
    fn seasons_and_apply() {
        let s = season_from_body(&json!({"episodes": [
            {"episode_number": 1, "name": "Winter Is Coming", "overview": "x"},
            {"episode_number": 2, "name": ""}, {"name": "no number"}]}));
        assert_eq!(s.episodes.len(), 2);
        assert_eq!(s.episodes[1].name, None);
        let mut x = Extras::default();
        x.seasons.insert((1, "fr-FR".into()), (s, 0));
        x.details.insert(
            "fr-FR".into(),
            (
                TmdbText {
                    tmdb_kind: "tv".into(),
                    tmdb_id: 1399,
                    overview: Some("FR".into()),
                    poster_path: Some("/p.jpg".into()),
                    ..Default::default()
                },
                0,
            ),
        );
        x.details.insert(
            "en-US".into(),
            (
                TmdbText {
                    tmdb_kind: "tv".into(),
                    tmdb_id: 1399,
                    ..Default::default()
                },
                0,
            ),
        );
        let mut m = MediaInfo {
            kind: MediaKind::Series,
            title: "Game of Thrones".into(),
            original_title: None,
            year: Some(2011),
            end_year: Some(2019),
            genres: vec![],
            rating: None,
            votes: None,
            runtime_min: None,
            season: Some(1),
            episode: Some(1),
            imdb: Some("tt0944947".into()),
            tmdb: None,
            mal: None,
            anilist: None,
            method: "itag".into(),
            confidence: 1.0,
            overview: None,
            poster: None,
            media_id: 42,
            episode_title: None,
        };
        apply_extras(&mut m, &x, &langs());
        // en-US has no overview: the next language's is used.
        assert_eq!(m.overview.as_deref(), Some("FR"));
        assert_eq!(m.poster.as_deref(), Some("/api/v1/media/42/poster"));
        assert_eq!(m.tmdb, Some((MediaKind::Series, 1399)));
        assert_eq!(m.episode_title.as_deref(), Some("Winter Is Coming"));
    }

    #[tokio::test]
    async fn bucket_rate() {
        let b = Bucket::new(20);
        let t = Instant::now();
        for _ in 0..25 {
            b.take().await;
        }
        // 5 burst + 20 at 20/s ≈ 1 s.
        let e = t.elapsed();
        assert!(
            e >= Duration::from_millis(900) && e < Duration::from_millis(1600),
            "{e:?}"
        );
    }

    #[test]
    fn paths_and_retry() {
        let p = image_path(Path::new("/c"), "abcdef", "image/png");
        assert_eq!(p, PathBuf::from("/c/ab/abcdef.png"));
        let mut h = reqwest::header::HeaderMap::new();
        h.insert(reqwest::header::RETRY_AFTER, "3".parse().unwrap());
        assert_eq!(retry_after(&h), Some(Duration::from_secs(3)));
        h.insert(
            reqwest::header::RETRY_AFTER,
            "Wed, 21 Oct 2015 07:28:00 GMT".parse().unwrap(),
        );
        assert_eq!(retry_after(&h), None);
        assert_eq!(backoff(1), Duration::from_secs(2));
        assert_eq!(backoff(9), Duration::from_secs(32));
    }
}

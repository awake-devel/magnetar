//! Server side of crawl workers (D96–D100, docs/WORKERS.md): worker tokens, the
//! `/api/v1/worker/*` endpoints, seed-check leases, DHT triage and uploads, and the per-worker
//! stats in `/api/v1/stats.workers`.
//!
//! The server stays the single source of truth: workers hold no database. They lease due seed
//! checks (`liveness_leases`), ask which DHT hashes to fetch (the server's own `dht_seen`
//! triage, so the fleet never fetches a hash twice) and upload raw info dicts, which the server
//! verifies against the infohash, classifies and screens itself before storing them exactly as
//! its own crawler would.

pub mod ingest;
pub mod proto;

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use anyhow::Context;
use axum::extract::{DefaultBodyLimit, Request, State};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Extension, Json, Router};
use magnetar_core::api::WorkerStats;
use magnetar_core::model::Liveness;

use crate::api::{ApiError, AppState, bearer, ct_eq};
use crate::dht::screen::{self, Screen};
use crate::liveness;
use proto::*;

/// Largest worker request body: one info dict at the 64 MiB `MAGNETAR_DHT_METADATA_MAX`
/// ceiling would not fit, but the default 16 MiB does with base64 and JSON around it.
pub const BODY_LIMIT: usize = 32 << 20;
pub const MAX_TRIAGE: usize = 2000;
pub const MAX_RESULTS: usize = 1000;
/// Upload requests (`dht/results`, `liveness/results`) read at once, server-wide (D113). One at
/// the [`BODY_LIMIT`] holds about 90 MB while it is parsed (the body, its JSON strings, the
/// decoded dicts), so 4 bound uploads to ~360 MB on the home server; a worker's uploader sends
/// one batch at a time, so the two crawl workers never wait on each other in normal running.
pub const UPLOAD_CONCURRENCY: usize = 4;
/// Upload requests of one worker read at once: its DHT uploader and its seed-check results.
pub const UPLOAD_CONCURRENCY_PER_WORKER: usize = 2;
/// Body limit of the other two non-upload routes, `dht/triage` and `liveness/release` (D113):
/// a full triage (2000 items, and the crawl map's node counts and AS names the worker sends
/// with it, at most [`crate::crawlmap::UPLOAD_ROWS_PER_TRIAGE`] /
/// [`crate::crawlmap::UPLOAD_NAMES_PER_TRIAGE`]) is under ~800 KB; a release of 2000 hashes is
/// ~90 KB. The 32 MiB [`BODY_LIMIT`] is for the two upload routes only.
pub const TRIAGE_BODY_LIMIT: usize = 1 << 20;
/// Non-upload requests (hello, heartbeat, lease, release, triage) of one worker read at once
/// (D113). An honest worker has at most ~4 in flight (heartbeat, lease, triage, hello).
pub const SMALL_CONCURRENCY_PER_WORKER: usize = 6;
/// The same, server-wide: with at most [`TRIAGE_BODY_LIMIT`] each, ~16 MiB of bodies.
pub const SMALL_CONCURRENCY: usize = 16;
/// `Retry-After` of a 503 when a worker's small-request slots are full.
pub const SMALL_RETRY_AFTER_SECS: u64 = 1;
/// Default `MAGNETAR_WORKER_TRIAGE_NEW_PER_HOUR` (D113): new `dht_seen` rows one worker's
/// triage may create per hour. The 2026-10-27 laptop worker (aggressive, docs/WORKERS.md §6)
/// queued ~11 100 new hashes in 600 s (~66 k/h).
pub const DEFAULT_TRIAGE_NEW_PER_HOUR: u64 = 100_000;
/// `Retry-After` of a 503 when the upload slots are full: about one batch's processing time.
pub const UPLOAD_RETRY_AFTER_SECS: u64 = 5;
/// Default `MAGNETAR_WORKER_LEASE_SECS`.
pub const DEFAULT_LEASE_SECS: u64 = 900;
/// Body limit of the small worker calls (hello, heartbeat, lease) (D111): their JSON is a few
/// hundred bytes; the 32 MiB [`BODY_LIMIT`] is for info dict uploads only.
pub const SMALL_BODY_LIMIT: usize = 64 << 10;
/// Longest `version` / `arch` / DHT `mode` kept from a worker (D111): real ones are
/// `0.3.1`, `aarch64`, `polite`. Longer text is cut, control characters are dropped.
pub const MAX_WORKER_LABEL: usize = 64;
/// Most DHT ports kept from a heartbeat or hello (D111): a worker runs one port per shard.
pub const MAX_WORKER_PORTS: usize = 64;
/// A worker heard from this recently counts as connected.
const CONNECTED_SECS: i64 = 120;
/// Rates are averaged over this many one-minute buckets.
const RATE_MINUTES: i64 = 10;

/// One `name:token` of `MAGNETAR_WORKER_TOKENS`.
#[derive(Clone)]
pub struct WorkerToken {
    pub name: String,
    token: String,
}

impl std::fmt::Debug for WorkerToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "WorkerToken({})", self.name)
    }
}

fn valid_name(n: &str) -> bool {
    !n.is_empty()
        && n.len() <= 32
        && n.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
        && n.as_bytes()[0].is_ascii_alphanumeric()
}

/// Parse `MAGNETAR_WORKER_TOKENS` (`name:token,name:token`). Names: 1–32 of `a-z0-9_-`;
/// tokens: at least 24 printable ASCII characters, no `,`; names and tokens unique, and no
/// token equal to the API token (a phone token must never push data, a worker token never read).
/// Errors never quote a token.
pub fn parse_tokens(s: &str, api_token: Option<&str>) -> anyhow::Result<Vec<WorkerToken>> {
    let mut out: Vec<WorkerToken> = Vec::new();
    for (i, part) in s.split(',').map(str::trim).enumerate() {
        if part.is_empty() {
            continue;
        }
        let (name, token) = part.split_once(':').with_context(|| {
            format!(
                "MAGNETAR_WORKER_TOKENS entry {}: expected name:token",
                i + 1
            )
        })?;
        let (name, token) = (name.trim(), token.trim());
        anyhow::ensure!(
            valid_name(name),
            "MAGNETAR_WORKER_TOKENS entry {}: name must be 1-32 of a-z, 0-9, '-', '_'",
            i + 1
        );
        anyhow::ensure!(
            token.len() >= 24 && token.bytes().all(|b| b.is_ascii_graphic()),
            "MAGNETAR_WORKER_TOKENS entry {} ({name}): token must be at least 24 printable characters",
            i + 1
        );
        anyhow::ensure!(
            !out.iter().any(|w| w.name == name),
            "MAGNETAR_WORKER_TOKENS: worker name {name} appears twice"
        );
        anyhow::ensure!(
            !out.iter().any(|w| w.token == token),
            "MAGNETAR_WORKER_TOKENS: two workers share a token"
        );
        anyhow::ensure!(
            api_token.is_none_or(|a| a.trim() != token),
            "MAGNETAR_WORKER_TOKENS ({name}): a worker token must differ from MAGNETAR_API_TOKEN"
        );
        out.push(WorkerToken {
            name: name.to_string(),
            token: token.to_string(),
        });
    }
    Ok(out)
}

#[derive(Clone, Debug)]
pub struct FleetConfig {
    pub tokens: Vec<WorkerToken>,
    /// Seed-check lease length.
    pub lease_secs: i64,
    /// Most infohashes per lease (the server's `MAGNETAR_LIVENESS_BATCH`).
    pub max_lease: usize,
    /// The server's `MAGNETAR_DHT_METADATA_MAX`: uploads above it are refused.
    pub metadata_max: usize,
    pub fetch_known: bool,
    /// `MAGNETAR_WORKER_TRIAGE_NEW_PER_HOUR`: new hashes one worker's triage may queue per
    /// hour (token bucket, burst = one hour's worth); 0 = no cap.
    pub triage_new_per_hour: u64,
}

impl Default for FleetConfig {
    fn default() -> Self {
        Self {
            tokens: Vec::new(),
            lease_secs: DEFAULT_LEASE_SECS as i64,
            max_lease: 2000,
            metadata_max: 16 << 20,
            fetch_known: false,
            triage_new_per_hour: DEFAULT_TRIAGE_NEW_PER_HOUR,
        }
    }
}

impl FleetConfig {
    pub fn from_config(cfg: &crate::config::Config) -> anyhow::Result<Self> {
        let tokens = match &cfg.worker_tokens {
            Some(s) => parse_tokens(s, cfg.api_token.as_deref())?,
            None => Vec::new(),
        };
        Ok(Self {
            tokens,
            lease_secs: cfg.worker_lease_secs as i64,
            max_lease: cfg.liveness_batch_size(),
            metadata_max: cfg.dht_metadata_max as usize,
            fetch_known: cfg.dht_fetch_known,
            triage_new_per_hour: cfg.worker_triage_new_per_hour,
        })
    }
}

/// A count per minute over the last [`RATE_MINUTES`].
#[derive(Debug, Default)]
struct Rate {
    buckets: VecDeque<(i64, u64)>,
}

impl Rate {
    fn add(&mut self, now: i64, n: u64) {
        let m = now.div_euclid(60);
        match self.buckets.back_mut() {
            Some((bm, c)) if *bm == m => *c += n,
            _ => self.buckets.push_back((m, n)),
        }
        while self
            .buckets
            .front()
            .is_some_and(|(bm, _)| *bm <= m - RATE_MINUTES)
        {
            self.buckets.pop_front();
        }
    }

    /// Per minute over the window, or since `since` when that is shorter (≥ 1 minute).
    fn per_min(&self, now: i64, since: i64) -> f64 {
        let m = now.div_euclid(60);
        let total: u64 = self
            .buckets
            .iter()
            .filter(|(bm, _)| *bm > m - RATE_MINUTES)
            .map(|(_, c)| c)
            .sum();
        let span = ((now - since) as f64 / 60.0).clamp(1.0, RATE_MINUTES as f64);
        total as f64 / span
    }
}

#[derive(Debug, Default)]
struct Entry {
    hello: Option<Hello>,
    first_seen: Option<i64>,
    last_seen: Option<i64>,
    heartbeat: Option<Heartbeat>,
    liveness_results: u64,
    triaged: u64,
    /// Of `triaged`, per route: sample, get_peers, announce (D102).
    triaged_by_route: [u64; 3],
    queued: u64,
    metadata: u64,
    stored: u64,
    blocked: u64,
    rejected: u64,
    results_rate: Rate,
    metadata_rate: Rate,
    /// New hashes its triage may still queue ([`Fleet::reserve_new`]), and when it was filled.
    triage_budget: Option<(f64, i64)>,
    /// New hashes its triage was refused by the hourly cap, and when that was last logged.
    triage_capped: u64,
    capped_logged_at: i64,
}

/// What the fleet endpoints share: tokens, limits, and what each worker did.
pub struct Fleet {
    cfg: FleetConfig,
    entries: Mutex<HashMap<String, Entry>>,
    /// The server's own content screen (the process-wide blocklist); None stores nothing named.
    screen: Option<Arc<dyn Screen>>,
    /// One verification batch at a time (bounded memory: D67 pricing per dict).
    verify: tokio::sync::Semaphore,
    /// Upload requests being read and processed, server-wide (D113: [`UPLOAD_CONCURRENCY`]).
    uploads: Arc<tokio::sync::Semaphore>,
    /// The same per worker ([`UPLOAD_CONCURRENCY_PER_WORKER`]), created on first use.
    worker_uploads: Mutex<HashMap<String, Arc<tokio::sync::Semaphore>>>,
    /// Non-upload requests being read and processed ([`SMALL_CONCURRENCY`]), and per worker.
    small: Arc<tokio::sync::Semaphore>,
    worker_small: Mutex<HashMap<String, Arc<tokio::sync::Semaphore>>>,
}

/// Held while one worker request is read and processed ([`Fleet::upload_permit`],
/// [`Fleet::small_permit`]).
#[derive(Debug)]
pub struct UploadPermit {
    _global: tokio::sync::OwnedSemaphorePermit,
    _worker: tokio::sync::OwnedSemaphorePermit,
}

impl Default for Fleet {
    fn default() -> Self {
        Self::new(FleetConfig::default(), None)
    }
}

impl std::fmt::Debug for Fleet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Fleet")
            .field("workers", &self.cfg.tokens.len())
            .finish()
    }
}

/// The worker a request authenticated as (set by [`worker_auth`]).
#[derive(Clone, Debug)]
pub struct WorkerName(pub String);

impl Fleet {
    pub fn new(cfg: FleetConfig, screen: Option<Arc<dyn Screen>>) -> Self {
        Self {
            cfg,
            entries: Mutex::default(),
            screen,
            verify: tokio::sync::Semaphore::new(1),
            uploads: Arc::new(tokio::sync::Semaphore::new(UPLOAD_CONCURRENCY)),
            worker_uploads: Mutex::default(),
            small: Arc::new(tokio::sync::Semaphore::new(SMALL_CONCURRENCY)),
            worker_small: Mutex::default(),
        }
    }

    /// A slot for one non-upload request of `worker` ([`SMALL_CONCURRENCY_PER_WORKER`],
    /// [`SMALL_CONCURRENCY`]), or None when full (503, as for uploads).
    pub fn small_permit(&self, worker: &str) -> Option<UploadPermit> {
        let w = self
            .worker_small
            .lock()
            .unwrap()
            .entry(worker.to_string())
            .or_insert_with(|| Arc::new(tokio::sync::Semaphore::new(SMALL_CONCURRENCY_PER_WORKER)))
            .clone();
        let worker = w.try_acquire_owned().ok()?;
        let global = self.small.clone().try_acquire_owned().ok()?;
        Some(UploadPermit {
            _global: global,
            _worker: worker,
        })
    }

    /// Reserve up to `want` new hashes of `worker`'s hourly triage cap (D113); `None` = no cap.
    /// The caller gives back what it did not use with [`Fleet::refund_new`], so concurrent
    /// triages of one token cannot both spend the same budget.
    pub fn reserve_new(&self, worker: &str, want: usize, now: i64) -> Option<usize> {
        let cap = self.cfg.triage_new_per_hour;
        if cap == 0 {
            return None;
        }
        Some(self.with(worker, |e| {
            let (tokens, at) = e.triage_budget.get_or_insert((cap as f64, now));
            let elapsed = (now - *at).max(0) as f64;
            *tokens = (*tokens + elapsed * cap as f64 / 3600.0).min(cap as f64);
            *at = now;
            let n = (tokens.floor() as usize).min(want);
            *tokens -= n as f64;
            n
        }))
    }

    /// Give back reserved new-hash budget that a triage did not use.
    pub fn refund_new(&self, worker: &str, unused: usize) {
        let cap = self.cfg.triage_new_per_hour as f64;
        if cap == 0.0 || unused == 0 {
            return;
        }
        self.with(worker, |e| {
            if let Some((tokens, _)) = &mut e.triage_budget {
                *tokens = (*tokens + unused as f64).min(cap);
            }
        });
    }

    /// Count new hashes refused by the cap; true when it is time to log it (once a minute).
    fn note_capped(&self, worker: &str, n: u64, now: i64) -> bool {
        self.with(worker, |e| {
            e.triage_capped += n;
            if now - e.capped_logged_at >= 60 {
                e.capped_logged_at = now;
                true
            } else {
                false
            }
        })
    }

    /// A slot to read one upload of `worker`, or None when the server-wide or the worker's
    /// slots are all taken (the caller answers 503 at once: waiting would hold the connection
    /// and its body anyway).
    pub fn upload_permit(&self, worker: &str) -> Option<UploadPermit> {
        let w = self
            .worker_uploads
            .lock()
            .unwrap()
            .entry(worker.to_string())
            .or_insert_with(|| Arc::new(tokio::sync::Semaphore::new(UPLOAD_CONCURRENCY_PER_WORKER)))
            .clone();
        let worker = w.try_acquire_owned().ok()?;
        let global = self.uploads.clone().try_acquire_owned().ok()?;
        Some(UploadPermit {
            _global: global,
            _worker: worker,
        })
    }

    /// From the configuration, with the process-wide screen.
    pub fn from_config(cfg: &crate::config::Config) -> anyhow::Result<Self> {
        Ok(Self::new(
            FleetConfig::from_config(cfg)?,
            screen::global_screen(),
        ))
    }

    pub fn enabled(&self) -> bool {
        !self.cfg.tokens.is_empty()
    }

    pub fn config(&self) -> &FleetConfig {
        &self.cfg
    }

    /// The worker whose token this is. Every configured token is compared (constant time each,
    /// no early exit), so the timing does not tell which one nearly matched.
    pub fn authenticate(&self, token: &str) -> Option<String> {
        let mut found = None;
        for w in &self.cfg.tokens {
            if ct_eq(token.as_bytes(), w.token.as_bytes()) && found.is_none() {
                found = Some(w.name.clone());
            }
        }
        found
    }

    fn with<R>(&self, name: &str, f: impl FnOnce(&mut Entry) -> R) -> R {
        let mut m = self.entries.lock().unwrap();
        f(m.entry(name.to_string()).or_default())
    }

    fn touch(&self, name: &str, now: i64) {
        self.with(name, |e| {
            e.first_seen.get_or_insert(now);
            e.last_seen = Some(now);
        });
    }

    /// Per configured worker, for `/api/v1/stats`.
    pub async fn stats(&self, pool: &sqlx::PgPool, now: i64) -> Vec<WorkerStats> {
        if !self.enabled() {
            return Vec::new();
        }
        let held = liveness::leases_held(pool, now).await.unwrap_or_else(|e| {
            tracing::warn!("stats: worker leases: {e}");
            HashMap::new()
        });
        let m = self.entries.lock().unwrap();
        self.cfg
            .tokens
            .iter()
            .map(|t| {
                let mut s = WorkerStats {
                    name: t.name.clone(),
                    leases_held: held.get(&t.name).copied().unwrap_or(0),
                    ..Default::default()
                };
                if let Some(e) = m.get(&t.name) {
                    let since = e.first_seen.unwrap_or(now);
                    s.connected = e.last_seen.is_some_and(|l| now - l <= CONNECTED_SECS);
                    s.version = e.hello.as_ref().map(|h| h.version.clone());
                    s.arch = e.hello.as_ref().map(|h| h.arch.clone());
                    s.first_seen = e.first_seen;
                    s.last_seen = e.last_seen;
                    s.liveness_results_total = e.liveness_results;
                    s.liveness_results_per_min = e.results_rate.per_min(now, since);
                    s.triaged_total = e.triaged;
                    s.queued_total = e.queued;
                    s.metadata_total = e.metadata;
                    s.metadata_per_min = e.metadata_rate.per_min(now, since);
                    s.stored_total = e.stored;
                    s.blocked_total = e.blocked;
                    s.rejected_total = e.rejected;
                    if let Some(h) = &e.heartbeat {
                        s.uptime_secs = Some(h.uptime_secs);
                        s.rss_mb = h.rss_mb;
                        s.mem_available_mb = h.mem_available_mb;
                        s.pressure = h.pressure;
                        s.dht = h.dht.clone();
                    }
                }
                s
            })
            .collect()
    }
}

impl Fleet {
    /// One entry per configured worker for `/api/v1/stats/sources` (D102); `stored_per_min`
    /// is filled by the crawl map from its samples.
    pub fn machines(&self, now: i64) -> Vec<magnetar_core::api::MachineSources> {
        let m = self.entries.lock().unwrap();
        self.cfg
            .tokens
            .iter()
            .map(|t| {
                let mut s = magnetar_core::api::MachineSources {
                    name: t.name.clone(),
                    kind: "worker".into(),
                    ..Default::default()
                };
                if let Some(e) = m.get(&t.name) {
                    s.connected = e.last_seen.is_some_and(|l| now - l <= CONNECTED_SECS);
                    s.since = e.first_seen;
                    s.dht_triaged = e.triaged;
                    s.dht_queued = e.queued;
                    s.dht_metadata = e.metadata;
                    s.dht_stored = e.stored;
                    s.dht_triaged_by_route = magnetar_core::api::RouteTriple {
                        sample: e.triaged_by_route[0],
                        get_peers: e.triaged_by_route[1],
                        announce: e.triaged_by_route[2],
                    };
                    s.seed_checks = e.liveness_results;
                }
                s
            })
            .collect()
    }
}

/// Worker-supplied text as it is logged, stored and shown in `/api/v1/stats` (D111): control
/// characters (newlines that would forge log lines, ANSI escapes) dropped, at most
/// [`MAX_WORKER_LABEL`] characters.
pub fn clean_label(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_control())
        .take(MAX_WORKER_LABEL)
        .collect()
}

fn clean_hello(mut h: Hello) -> Hello {
    h.version = clean_label(&h.version);
    h.arch = clean_label(&h.arch);
    h.dht = h.dht.as_deref().map(clean_label);
    h.dht_ports.truncate(MAX_WORKER_PORTS);
    h
}

fn clean_heartbeat(mut h: Heartbeat) -> Heartbeat {
    if let Some(d) = &mut h.dht {
        d.mode = clean_label(&d.mode);
        d.ports.truncate(MAX_WORKER_PORTS);
    }
    h
}

fn api_error(status: StatusCode, code: &'static str, msg: impl Into<String>) -> Response {
    ApiError::new(status, code, msg).into_response()
}

/// Auth for `/api/v1/worker/*`: a worker token (never the API token) and the same
/// [`WORKER_API`] (else 409 with both versions named, so the worker can say what to upgrade).
pub async fn worker_auth(State(st): State<AppState>, mut req: Request, next: Next) -> Response {
    if !st.fleet.enabled() {
        return api_error(
            StatusCode::FORBIDDEN,
            "workers_disabled",
            "this server accepts no crawl workers (MAGNETAR_WORKER_TOKENS is not set)",
        );
    }
    let Some(name) = bearer(req.headers()).and_then(|t| st.fleet.authenticate(t)) else {
        return api_error(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "missing or bad worker token",
        );
    };
    let theirs = req
        .headers()
        .get(API_HEADER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u32>().ok());
    if theirs != Some(WORKER_API) {
        let theirs = theirs.map_or_else(|| "none".to_string(), |v| v.to_string());
        return api_error(
            StatusCode::CONFLICT,
            "worker_api_mismatch",
            format!(
                "server magnetar-server {} speaks worker API {WORKER_API}, this worker speaks {theirs}: run the same Magnetar version on the server and its workers",
                magnetar_core::VERSION
            ),
        );
    }
    st.fleet.touch(&name, magnetar_core::now());
    // Uploads take a slot before their body is read (D113), held until the answer is built.
    let path = req.uri().path();
    // Every other worker route takes a small slot (its body is at most 1 MiB, D113).
    let upload = path == DHT_RESULTS || path == LIVENESS_RESULTS;
    let permit = if upload {
        st.fleet.upload_permit(&name)
    } else {
        st.fleet.small_permit(&name)
    };
    let Some(_permit) = permit else {
        tracing::debug!(worker = %name, upload, "worker request refused: slots full");
        let mut r = api_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "busy",
            "the server is processing other requests of this worker; retry later",
        );
        let after = if upload {
            UPLOAD_RETRY_AFTER_SECS
        } else {
            SMALL_RETRY_AFTER_SECS
        };
        r.headers_mut().insert(
            axum::http::header::RETRY_AFTER,
            axum::http::HeaderValue::from(after),
        );
        return r;
    };
    req.extensions_mut().insert(WorkerName(name));
    next.run(req).await
}

/// The worker routes (auth and body limits are added by `api::router`).
pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            HELLO,
            post(hello).layer(DefaultBodyLimit::max(SMALL_BODY_LIMIT)),
        )
        .route(
            HEARTBEAT,
            post(heartbeat).layer(DefaultBodyLimit::max(SMALL_BODY_LIMIT)),
        )
        .route(
            LEASE,
            post(lease).layer(DefaultBodyLimit::max(SMALL_BODY_LIMIT)),
        )
        .route(LIVENESS_RESULTS, post(liveness_results))
        .route(
            RELEASE,
            post(release).layer(DefaultBodyLimit::max(TRIAGE_BODY_LIMIT)),
        )
        .route(
            TRIAGE,
            post(ingest::triage).layer(DefaultBodyLimit::max(TRIAGE_BODY_LIMIT)),
        )
        .route(DHT_RESULTS, post(ingest::dht_results))
}

type ApiResult<T> = Result<T, ApiError>;

fn json<T>(r: Result<Json<T>, axum::extract::rejection::JsonRejection>) -> ApiResult<T> {
    r.map(|Json(v)| v).map_err(ApiError::from)
}

async fn hello(
    State(st): State<AppState>,
    Extension(WorkerName(name)): Extension<WorkerName>,
    body: Result<Json<Hello>, axum::extract::rejection::JsonRejection>,
) -> ApiResult<Json<HelloResponse>> {
    let h = clean_hello(json(body)?);
    tracing::info!(
        worker = %name,
        version = %h.version,
        arch = %h.arch,
        liveness = h.liveness,
        dht = h.dht.as_deref().unwrap_or("off"),
        "worker connected"
    );
    st.fleet.with(&name, |e| e.hello = Some(h));
    let c = st.fleet.config();
    Ok(Json(HelloResponse {
        name,
        server_version: magnetar_core::VERSION.into(),
        worker_api: WORKER_API,
        lease_secs: c.lease_secs as u64,
        max_lease: c.max_lease as u32,
        max_triage: MAX_TRIAGE as u32,
        max_results: MAX_RESULTS as u32,
        max_body: BODY_LIMIT as u64,
        metadata_max: c.metadata_max as u64,
    }))
}

async fn heartbeat(
    State(st): State<AppState>,
    Extension(WorkerName(name)): Extension<WorkerName>,
    body: Result<Json<Heartbeat>, axum::extract::rejection::JsonRejection>,
) -> ApiResult<StatusCode> {
    let h = clean_heartbeat(json(body)?);
    st.fleet.with(&name, |e| e.heartbeat = Some(h));
    Ok(StatusCode::NO_CONTENT)
}

async fn lease(
    State(st): State<AppState>,
    Extension(WorkerName(name)): Extension<WorkerName>,
    body: Result<Json<LeaseRequest>, axum::extract::rejection::JsonRejection>,
) -> ApiResult<Json<Lease>> {
    let r = json(body)?;
    let c = st.fleet.config();
    let max = (r.max as usize).clamp(1, c.max_lease.max(1));
    let now = magnetar_core::now();
    // Worker names cannot be empty, so a worker never takes the server's own holder name.
    let rows = liveness::claim(st.store.pool(), now, max, &name, c.lease_secs).await?;
    Ok(Json(Lease {
        expires_at: now + c.lease_secs.max(1),
        items: rows
            .into_iter()
            .map(|r| LeaseItem {
                infohash: r.infohash,
                trackers: r.trackers,
            })
            .collect(),
    }))
}

fn valid_infohash(h: &str) -> bool {
    h.len() == 40
        && h.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

async fn liveness_results(
    State(st): State<AppState>,
    Extension(WorkerName(name)): Extension<WorkerName>,
    body: Result<Json<LivenessResults>, axum::extract::rejection::JsonRejection>,
) -> ApiResult<Json<Ack>> {
    let r = json(body)?;
    let c = st.fleet.config();
    if r.results.len() > c.max_lease.max(MAX_RESULTS) {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid",
            "too many results in one request",
        ));
    }
    let now = magnetar_core::now();
    let mut by: HashMap<String, Liveness> = HashMap::new();
    let mut ignored = 0u32;
    for l in r.results {
        let ih = l.infohash.to_ascii_lowercase();
        if !valid_infohash(&ih) {
            ignored += 1;
            continue;
        }
        // The worker's clock is trusted only within the lease window.
        let checked_at = l.checked_at.clamp(now - c.lease_secs - 300, now);
        by.insert(
            ih.clone(),
            Liveness {
                infohash: ih,
                checked_at,
                ..l
            },
        );
    }
    let ihs: Vec<String> = by.keys().cloned().collect();
    let rows = liveness::held_rows(st.store.pool(), &name, &ihs).await?;
    let results: Vec<Liveness> = rows.iter().map(|row| by[&row.infohash].clone()).collect();
    ignored += (by.len() - rows.len()) as u32;
    if !rows.is_empty() {
        // Exactly the server's own write path: results, schedule, streaks, history (D45).
        liveness::write_results(
            st.store.pool(),
            &rows,
            &results,
            now,
            liveness::Expiry::Scheduled,
        )
        .await?;
        crate::stats::bump(
            st.store.pool(),
            crate::stats::LIVENESS_CHECKS,
            rows.len() as i64,
        )
        .await;
    }
    let accepted = rows.len() as u32;
    st.fleet.with(&name, |e| {
        e.liveness_results += u64::from(accepted);
        e.results_rate.add(now, u64::from(accepted));
    });
    Ok(Json(Ack { accepted, ignored }))
}

async fn release(
    State(st): State<AppState>,
    Extension(WorkerName(name)): Extension<WorkerName>,
    body: Result<Json<Release>, axum::extract::rejection::JsonRejection>,
) -> ApiResult<Json<Ack>> {
    let r = json(body)?;
    let ihs: Vec<String> = r
        .infohashes
        .iter()
        .map(|h| h.to_ascii_lowercase())
        .filter(|h| valid_infohash(h))
        .take(MAX_RESULTS.max(st.fleet.config().max_lease))
        .collect();
    let n = liveness::release(st.store.pool(), &name, &ihs, magnetar_core::now()).await?;
    Ok(Json(Ack {
        accepted: n as u32,
        ignored: (r.infohashes.len() as u64).saturating_sub(n) as u32,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    const T1: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const T2: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    #[test]
    fn tokens_parse_and_validate() {
        let t = parse_tokens(&format!("eee:{T1}, rpi:{T2},"), Some("phone")).unwrap();
        assert_eq!(
            t.iter().map(|w| w.name.as_str()).collect::<Vec<_>>(),
            ["eee", "rpi"]
        );
        let f = Fleet::new(
            FleetConfig {
                tokens: t,
                ..Default::default()
            },
            None,
        );
        assert_eq!(f.authenticate(T2).as_deref(), Some("rpi"));
        assert_eq!(f.authenticate("phone"), None);
        assert_eq!(f.authenticate(&T1[1..]), None);
        for bad in [
            format!("eee{T1}"),
            format!("EEE:{T1}"),
            "eee:short".to_string(),
            format!("eee:{T1},eee:{T2}"),
            format!("eee:{T1},rpi:{T1}"),
            format!(":{T1}"),
        ] {
            let e = parse_tokens(&bad, None).unwrap_err().to_string();
            assert!(!e.contains(T1) && !e.contains(T2), "token in error: {e}");
        }
        let e = parse_tokens(&format!("eee:{T1}"), Some(T1)).unwrap_err();
        assert!(e.to_string().contains("differ"), "{e}");
        assert!(parse_tokens("", None).unwrap().is_empty());
    }

    /// An API router whose database is never reachable (lazy pool to a closed port, short
    /// acquire timeout): enough for everything decided before a query runs. No network beyond
    /// loopback.
    pub(crate) fn offline_state(fleet: Fleet) -> AppState {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .acquire_timeout(std::time::Duration::from_millis(300))
            .connect_lazy("postgres://nobody@127.0.0.1:1/none")
            .unwrap();
        AppState::new(
            Arc::new(crate::store::PgStore::from_pool(pool)),
            "phone-token",
        )
        .with_fleet(Arc::new(fleet))
    }

    async fn serve(state: AppState) -> std::net::SocketAddr {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = crate::api::router(state);
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        addr
    }

    /// An upload whose body never finishes: it holds its slot while the server waits for it.
    async fn stalled_upload(addr: std::net::SocketAddr, token: &str) -> tokio::net::TcpStream {
        use tokio::io::AsyncWriteExt;
        let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
        let head = format!(
            "POST {DHT_RESULTS} HTTP/1.1\r\nhost: x\r\nauthorization: Bearer {token}\r\n\
             {API_HEADER}: {WORKER_API}\r\ncontent-type: application/json\r\n\
             content-length: 1000\r\n\r\n{{\"items\": ["
        );
        s.write_all(head.as_bytes()).await.unwrap();
        s
    }

    async fn upload_status(addr: std::net::SocketAddr, token: &str) -> (u16, Option<String>) {
        let r = reqwest::Client::new()
            .post(format!("http://{addr}{DHT_RESULTS}"))
            .bearer_auth(token)
            .header(API_HEADER, WORKER_API.to_string())
            .json(&serde_json::json!({"items": []}))
            .send()
            .await
            .unwrap();
        let ra = r
            .headers()
            .get(axum::http::header::RETRY_AFTER)
            .map(|v| v.to_str().unwrap().to_string());
        (r.status().as_u16(), ra)
    }

    /// D113: uploads are bounded per worker and server-wide before their bodies are read; a
    /// full server answers 503 with Retry-After at once, and a freed slot is usable again.
    #[tokio::test]
    async fn uploads_are_bounded_before_their_bodies_are_read() {
        let t3 = "cccccccccccccccccccccccccccccccc";
        let tokens = parse_tokens(&format!("w1:{T1},w2:{T2},w3:{t3}"), None).unwrap();
        let addr = serve(offline_state(Fleet::new(
            FleetConfig {
                tokens,
                ..Default::default()
            },
            None,
        )))
        .await;
        // w1 fills its own slots: its next upload is refused, another worker's is not.
        let mut held = Vec::new();
        for _ in 0..UPLOAD_CONCURRENCY_PER_WORKER {
            held.push(stalled_upload(addr, T1).await);
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        let (code, ra) = upload_status(addr, T1).await;
        assert_eq!(code, 503, "w1 is over its own limit");
        assert_eq!(ra, Some(UPLOAD_RETRY_AFTER_SECS.to_string()));
        // w2 fills the rest of the server-wide slots: now w3 is refused too.
        for _ in UPLOAD_CONCURRENCY_PER_WORKER..UPLOAD_CONCURRENCY {
            held.push(stalled_upload(addr, T2).await);
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        let (code, ra) = upload_status(addr, t3).await;
        assert_eq!(code, 503, "the server-wide slots are full");
        assert!(ra.is_some());
        // A stalled upload goes away: its slot comes back (the request then fails on the
        // unreachable database, which is past the limit).
        drop(held.pop());
        let mut code = 503;
        for _ in 0..50 {
            code = upload_status(addr, t3).await.0;
            if code != 503 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        assert_ne!(code, 503, "a freed slot is reused");
    }

    /// A small worker request whose body never finishes: it holds a small slot.
    async fn stalled_small(addr: std::net::SocketAddr, token: &str) -> tokio::net::TcpStream {
        use tokio::io::AsyncWriteExt;
        let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
        let head = format!(
            "POST {TRIAGE} HTTP/1.1\r\nhost: x\r\nauthorization: Bearer {token}\r\n\
             {API_HEADER}: {WORKER_API}\r\ncontent-type: application/json\r\n\
             content-length: 1000\r\n\r\n{{\"items\": ["
        );
        s.write_all(head.as_bytes()).await.unwrap();
        s
    }

    async fn worker_post(
        addr: std::net::SocketAddr,
        path: &str,
        token: &str,
        body: Vec<u8>,
    ) -> (u16, Option<String>) {
        let r = reqwest::Client::new()
            .post(format!("http://{addr}{path}"))
            .bearer_auth(token)
            .header(API_HEADER, WORKER_API.to_string())
            .header("content-type", "application/json")
            .body(body)
            .send()
            .await
            .unwrap();
        let ra = r
            .headers()
            .get(axum::http::header::RETRY_AFTER)
            .map(|v| v.to_str().unwrap().to_string());
        (r.status().as_u16(), ra)
    }

    /// D113: triage and release bodies are limited to 1 MiB (the 32 MiB limit is for the two
    /// upload routes), and every non-upload request takes a small slot before its body is read.
    #[tokio::test]
    async fn small_routes_have_small_bodies_and_slots() {
        let tokens = parse_tokens(&format!("w1:{T1},w2:{T2}"), None).unwrap();
        let addr = serve(offline_state(Fleet::new(
            FleetConfig {
                tokens,
                ..Default::default()
            },
            None,
        )))
        .await;
        // Valid JSON just over the limit: refused before it is parsed (the old limit read it,
        // then went to the database).
        let big = |key: &str| {
            let pad = "x".repeat(TRIAGE_BODY_LIMIT + 1);
            match key {
                "triage" => format!(r#"{{"items":[],"as_names":[{{"asn":1,"org":"{pad}"}}]}}"#),
                _ => format!(r#"{{"infohashes":["{pad}"]}}"#),
            }
            .into_bytes()
        };
        assert_eq!(worker_post(addr, TRIAGE, T1, big("triage")).await.0, 413);
        assert_eq!(worker_post(addr, RELEASE, T1, big("release")).await.0, 413);

        // w1 fills its small slots: its next triage is refused at once, w2's is not.
        let mut held = Vec::new();
        for _ in 0..SMALL_CONCURRENCY_PER_WORKER {
            held.push(stalled_small(addr, T1).await);
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        let (code, ra) = worker_post(addr, TRIAGE, T1, br#"{"items":[]}"#.to_vec()).await;
        assert_eq!(code, 503, "w1 is over its small-request limit");
        assert_eq!(ra, Some(SMALL_RETRY_AFTER_SECS.to_string()));
        let (code, _) = worker_post(addr, HEARTBEAT, T1, b"{}".to_vec()).await;
        assert_eq!(code, 503, "every non-upload route counts");
        let (code, _) = worker_post(addr, HEARTBEAT, T2, b"{}".to_vec()).await;
        assert_eq!(code, 204, "another worker is not held back");
        drop(held);
    }

    /// D113: the hourly new-hash budget of a worker's triage: reserved before the query (two
    /// concurrent triages cannot spend it twice), refunded when unused, refilled over the hour.
    #[test]
    fn triage_budget_is_reserved_refunded_and_refilled() {
        let f = Fleet::new(
            FleetConfig {
                triage_new_per_hour: 10,
                ..Default::default()
            },
            None,
        );
        let t0 = 100_000;
        assert_eq!(f.reserve_new("w1", 8, t0), Some(8));
        assert_eq!(f.reserve_new("w1", 8, t0), Some(2), "only 2 left");
        assert_eq!(f.reserve_new("w2", 8, t0), Some(8), "per worker");
        f.refund_new("w1", 2);
        assert_eq!(f.reserve_new("w1", 8, t0), Some(2));
        assert_eq!(f.reserve_new("w1", 8, t0 + 359), Some(0));
        assert_eq!(
            f.reserve_new("w1", 8, t0 + 720),
            Some(2),
            "10 an hour (fractions kept)"
        );
        assert_eq!(
            f.reserve_new("w1", 100, t0 + 100_000),
            Some(10),
            "burst = 1 h"
        );
        let off = Fleet::new(
            FleetConfig {
                triage_new_per_hour: 0,
                ..Default::default()
            },
            None,
        );
        assert_eq!(off.reserve_new("w1", 8, t0), None, "0 = no cap");
    }

    #[test]
    fn rates_over_the_window() {
        let mut r = Rate::default();
        let t0 = 6000;
        r.add(t0, 10);
        assert_eq!(r.per_min(t0 + 30, t0), 10.0, "at least one minute");
        r.add(t0 + 120, 20);
        assert_eq!(r.per_min(t0 + 180, t0), 10.0);
        // Long after: the window holds the last 10 minutes only.
        r.add(t0 + 3600, 5);
        assert_eq!(r.per_min(t0 + 3600, t0), 0.5);
        assert!(r.buckets.len() == 1);
    }
}

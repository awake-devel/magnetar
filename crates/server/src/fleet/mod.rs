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
use axum::extract::{Request, State};
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
/// Default `MAGNETAR_WORKER_LEASE_SECS`.
pub const DEFAULT_LEASE_SECS: u64 = 900;
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
}

impl Default for FleetConfig {
    fn default() -> Self {
        Self {
            tokens: Vec::new(),
            lease_secs: DEFAULT_LEASE_SECS as i64,
            max_lease: 2000,
            metadata_max: 16 << 20,
            fetch_known: false,
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
    queued: u64,
    metadata: u64,
    stored: u64,
    blocked: u64,
    rejected: u64,
    results_rate: Rate,
    metadata_rate: Rate,
}

/// What the fleet endpoints share: tokens, limits, and what each worker did.
pub struct Fleet {
    cfg: FleetConfig,
    entries: Mutex<HashMap<String, Entry>>,
    /// The server's own content screen (the process-wide blocklist); None stores nothing named.
    screen: Option<Arc<dyn Screen>>,
    /// One verification batch at a time (bounded memory: D67 pricing per dict).
    verify: tokio::sync::Semaphore,
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
        }
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
    req.extensions_mut().insert(WorkerName(name));
    next.run(req).await
}

/// The worker routes (auth and body limits are added by `api::router`).
pub fn routes() -> Router<AppState> {
    Router::new()
        .route(HELLO, post(hello))
        .route(HEARTBEAT, post(heartbeat))
        .route(LEASE, post(lease))
        .route(LIVENESS_RESULTS, post(liveness_results))
        .route(RELEASE, post(release))
        .route(TRIAGE, post(ingest::triage))
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
    let h = json(body)?;
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
    let h = json(body)?;
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

//! A worker's DHT crawler (D97): its own `core::dht` nodes (own BEP 42 ids and ports), the
//! server's queue, seen filter, health probe and metadata fetcher, but the database replaced
//! by the server's worker API:
//!
//! sightings → seen filter → **triage at the server** (batches ≤ 1000 / 1 s; the server's
//! `dht_seen` decides and leases, so no two fleet members fetch the same hash) → fetch queue →
//! peer collection → BEP 9 fetch (raw bytes kept) → classify + **blocklist here** → upload
//! (blocked: hash only; else the raw dict, which the server verifies, classifies and screens
//! again) or a status (`no_peers`, failures, private, …).
//!
//! Memory is bounded for small machines: the pool, queue and seen filter sizes come from the
//! options, uploads wait in a byte-capped buffer (fetching stops when it is full), and the
//! [`Governor`] pauses discovery under memory or conntrack pressure. Nothing here logs a name.

use std::collections::HashSet;
use std::net::SocketAddrV4;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use magnetar_core::api::WorkerDhtState;
use magnetar_core::dht::{Dht, EngineConfig, EngineStats, Mode, Sighting, Via, collect_peers};
use magnetar_core::metainfo::{FetchConfig, FetchStats, Fetcher, Outcome};
use magnetar_core::model::TorrentFile;
use serde::Serialize;
use tokio::sync::{Notify, OwnedSemaphorePermit, Semaphore, mpsc, watch};
use tokio::task::JoinSet;
use tokio::time::Instant;

use super::client::Client;
use super::governor::Governor;
use super::seeds::with_retry;
use super::{Abort, state};
use crate::daemons::stop_requested;
use crate::dht::health::{self, HealthStats};
use crate::dht::queue::{FetchItem, FetchQueue};
use crate::dht::screen::Verdict;
use crate::dht::seen::{self, SeenFilter};
use crate::dht::{DhtOptions, classify, upnp};
use crate::fleet::proto::*;

const BATCH_MAX: usize = 1000;
const BATCH_WAIT: Duration = Duration::from_secs(1);
const TRIAGE_BACKLOG_MAX: usize = 50_000;
const LOOKUP_BUDGET: Duration = Duration::from_secs(10);
const UPLOAD_WAIT: Duration = Duration::from_secs(2);
/// Bytes of encoded dicts per upload request (a larger single dict goes alone).
const UPLOAD_BYTES: usize = 4 << 20;
const PERSIST_EVERY: Duration = Duration::from_secs(600);
const CONTROL_EVERY: Duration = Duration::from_millis(500);
/// Sampling pauses at this many queued hashes (or half the queue) and resumes at half (D62).
const QUEUE_HIGH: usize = 10_000;
/// Brake length while the governor reports pressure (renewed while it lasts).
const PRESSURE_BRAKE: Duration = Duration::from_secs(30);

#[derive(Default)]
struct Counters {
    sightings: AtomicU64,
    unique: AtomicU64,
    triaged: AtomicU64,
    queued: AtomicU64,
    triage_known: AtomicU64,
    triage_blocked: AtomicU64,
    triage_errors: AtomicU64,
    triage_dropped: AtomicU64,
    collected: AtomicU64,
    with_peers: AtomicU64,
    no_peers: AtomicU64,
    verified: AtomicU64,
    blocked_here: AtomicU64,
    uploaded: AtomicU64,
    upload_requests: AtomicU64,
    upload_errors: AtomicU64,
    upload_backlog: AtomicU64,
    server_stored: AtomicU64,
    server_not_new: AtomicU64,
    server_blocked: AtomicU64,
    server_rejected: AtomicU64,
    server_ignored: AtomicU64,
    unscreened: AtomicU64,
    paused_ms: AtomicU64,
}

fn add(a: &AtomicU64, n: u64) {
    a.fetch_add(n, Ordering::Relaxed);
}

fn get(a: &AtomicU64) -> u64 {
    a.load(Ordering::Relaxed)
}

/// What the worker's crawler did (JSON report at stop).
#[derive(Clone, Debug, Default, Serialize)]
pub struct CrawlReport {
    pub mode: String,
    pub ports: Vec<u16>,
    pub seconds: f64,
    pub sightings: u64,
    pub unique_hashes: u64,
    pub triaged: u64,
    /// Hashes the server told this worker to fetch.
    pub queued_by_server: u64,
    pub known_at_server: u64,
    pub blocked_at_server: u64,
    pub triage_errors: u64,
    pub triage_dropped: u64,
    pub collected: u64,
    pub with_peers: u64,
    pub no_peers: u64,
    /// Verified info dicts (fetcher), and of those stopped by this worker's blocklist.
    pub verified: u64,
    pub blocked_here: u64,
    /// Results handed to the server (dicts and statuses), requests, failed requests.
    pub uploaded: u64,
    pub upload_requests: u64,
    pub upload_errors: u64,
    /// What the server answered.
    pub server_stored: u64,
    pub server_not_new: u64,
    pub server_blocked: u64,
    pub server_rejected: u64,
    pub server_ignored: u64,
    pub sampling_paused_secs: f64,
    pub queries_per_sec: f64,
    pub answered_share: f64,
    pub fetch: FetchStats,
    pub health: HealthStats,
    pub engine: EngineStats,
}

struct Pending {
    item: DhtResult,
    _permit: Option<OwnedSemaphorePermit>,
    bytes: usize,
}

/// Rates for the heartbeat: counters at the previous call.
#[derive(Default)]
struct Snap {
    at: Option<Instant>,
    queries: u64,
    answered: u64,
    unique: u64,
    verified: u64,
}

pub struct Crawl {
    client: Client,
    opts: DhtOptions,
    queue: Mutex<FetchQueue>,
    notify: Notify,
    fetcher: Fetcher,
    c: Counters,
    health: Arc<Mutex<HealthStats>>,
    governor: Arc<Governor>,
    /// Upload buffer, in KiB units.
    buffer: Arc<Semaphore>,
    buffer_units: u32,
    screen_slots: Semaphore,
    dht: Mutex<Option<Dht>>,
    snap: Mutex<Snap>,
    max_results: usize,
}

impl Crawl {
    fn enqueue(&self, item: FetchItem) {
        if self.queue.lock().unwrap().push(item) {
            self.notify.notify_one();
        }
    }

    /// The heartbeat's DHT section (rates since the previous call).
    pub fn state(&self) -> Option<WorkerDhtState> {
        let dht = self.dht.lock().unwrap().clone()?;
        let e = dht.stats();
        let f = self.fetcher.stats();
        let verified = f.ok + f.private + f.v2_only + f.invalid;
        let unique = get(&self.c.unique);
        let now = Instant::now();
        let mut s = self.snap.lock().unwrap();
        let secs =
            s.at.map_or(0.0, |a| now.duration_since(a).as_secs_f64())
                .max(1.0);
        let d = |a: u64, b: u64| a.saturating_sub(b) as f64;
        let st = WorkerDhtState {
            mode: match self.opts.mode {
                Mode::Polite => "polite",
                Mode::Aggressive => "aggressive",
            }
            .into(),
            nodes: dht.virtual_nodes() as u32,
            ports: dht.local_ports(),
            qps: d(e.queries_sent, s.queries) / secs,
            answered_share: if e.queries_sent > s.queries {
                d(e.answered, s.answered) / d(e.queries_sent, s.queries)
            } else {
                0.0
            },
            brake_active: e.brake_active,
            health_brakes: self.health.lock().unwrap().brakes,
            sampling_paused: dht.sample_share() <= 0.0,
            routing_nodes: e.routing.iter().sum::<usize>() as u64,
            pool: e.pool.size as u64,
            queue: self.queue.lock().unwrap().len() as u64,
            unique_per_min: d(unique, s.unique) * 60.0 / secs,
            metadata_per_min: d(verified, s.verified) * 60.0 / secs,
            upload_backlog: get(&self.c.upload_backlog),
            external_ip_known: e.external_ip_known,
        };
        *s = Snap {
            at: Some(now),
            queries: e.queries_sent,
            answered: e.answered,
            unique,
            verified,
        };
        Some(st)
    }
}

fn via_text(v: Via) -> &'static str {
    match v {
        Via::Sample => "sample",
        Via::Announce => "announce",
        Via::GetPeers => "get_peers",
    }
}

async fn triage_loop(
    p: Arc<Crawl>,
    mut rx: mpsc::Receiver<Sighting>,
    batches: mpsc::Sender<Vec<Sighting>>,
    mut stop: watch::Receiver<bool>,
) {
    let mut seen = SeenFilter::new(p.opts.bloom_bits, seen::DEFAULT_K, seen::ROTATE_EVERY);
    let mut batch: Vec<Sighting> = Vec::new();
    let mut flush_at = Instant::now() + BATCH_WAIT;
    let hand = |batch: &mut Vec<Sighting>| -> bool {
        if batch.is_empty() {
            return true;
        }
        match batches.try_send(std::mem::take(batch)) {
            Err(mpsc::error::TrySendError::Full(b)) => {
                *batch = b;
                false
            }
            _ => true,
        }
    };
    loop {
        tokio::select! {
            s = rx.recv() => match s {
                Some(s) => {
                    add(&p.c.sightings, 1);
                    if batch.len() >= TRIAGE_BACKLOG_MAX {
                        add(&p.c.triage_dropped, 1);
                    } else if seen.check_and_insert(&s.info_hash, Instant::now()) {
                        add(&p.c.unique, 1);
                        if batch.is_empty() {
                            flush_at = Instant::now() + BATCH_WAIT;
                        }
                        batch.push(s);
                        if batch.len() >= BATCH_MAX {
                            hand(&mut batch);
                        }
                    } else if let (Via::Announce, Some(peer)) = (s.via, s.peer) {
                        // A live peer for a hash we may be holding (D93).
                        let _ = p.queue.lock().unwrap().attach_announce(s.info_hash, peer, Instant::now());
                    }
                }
                None => break,
            },
            _ = tokio::time::sleep_until(flush_at), if !batch.is_empty() => {
                if !hand(&mut batch) {
                    flush_at = Instant::now() + Duration::from_millis(200);
                }
            }
            _ = stop_requested(&mut stop) => break,
        }
    }
}

/// Asks the server about each batch (one request at a time) and queues what it hands out.
async fn triage_task(
    p: Arc<Crawl>,
    mut rx: mpsc::Receiver<Vec<Sighting>>,
    abort: Abort,
    mut stop: watch::Receiver<bool>,
) {
    while let Some(batch) = rx.recv().await {
        let req = TriageRequest {
            items: batch
                .iter()
                .map(|s| TriageItem {
                    h: hex::encode(s.info_hash),
                    via: via_text(s.via).into(),
                })
                .collect(),
        };
        add(&p.c.triaged, batch.len() as u64);
        let r = with_retry(3, &mut stop, || {
            p.client.post::<_, TriageResponse>(TRIAGE, &req)
        })
        .await;
        let resp = match r {
            Ok(r) => r,
            Err(e) => {
                if e.is_fatal() {
                    abort.fatal(&e);
                    return;
                }
                if get(&p.c.triage_errors).is_multiple_of(50) {
                    tracing::warn!("worker: triage: {e}");
                }
                add(&p.c.triage_errors, 1);
                continue;
            }
        };
        add(&p.c.triage_known, u64::from(resp.known));
        add(&p.c.triage_blocked, u64::from(resp.blocked));
        let fetch: HashSet<[u8; 20]> = resp.fetch.iter().filter_map(|h| parse_hash(h)).collect();
        add(&p.c.queued, fetch.len() as u64);
        let now = Instant::now();
        let mut done = HashSet::new();
        let mut ordered = batch;
        ordered.sort_by_key(|s| s.via != Via::Announce);
        for s in ordered {
            if fetch.contains(&s.info_hash) && done.insert(s.info_hash) {
                p.enqueue(FetchItem {
                    info_hash: s.info_hash,
                    via: s.via,
                    source: s.source,
                    peer: s.peer,
                    retry: false,
                    queued_at: now,
                });
            }
        }
    }
}

struct Job {
    info_hash: [u8; 20],
    via: Via,
    peers: Vec<SocketAddrV4>,
}

async fn collector(
    p: Arc<Crawl>,
    dht: Dht,
    jobs: mpsc::Sender<Job>,
    out: mpsc::Sender<Pending>,
    mut stop: watch::Receiver<bool>,
) {
    let want = p.opts.peers_per_hash.max(1);
    loop {
        let item = p.queue.lock().unwrap().pop_for_collection();
        let Some(item) = item else {
            tokio::select! {
                _ = p.notify.notified() => {}
                _ = tokio::time::sleep(Duration::from_secs(1)) => {}
                _ = stop_requested(&mut stop) => return,
            }
            continue;
        };
        let c = tokio::select! {
            c = collect_peers(&dht, item.info_hash, item.peer, item.source, LOOKUP_BUDGET, want) => c,
            _ = stop_requested(&mut stop) => return,
        };
        let mut peers = c.peers;
        let mut via = item.via;
        if let Some(peer) = p.queue.lock().unwrap().collected(&item.info_hash)
            && dht.allows(&peer)
        {
            peers.retain(|x| *x != peer);
            peers.insert(0, peer);
            via = Via::Announce;
        }
        add(&p.c.collected, 1);
        if peers.is_empty() {
            add(&p.c.no_peers, 1);
            let r = Pending {
                item: DhtResult {
                    sighted: hex::encode(item.info_hash),
                    via: via_text(via).into(),
                    peers_seen: 0,
                    outcome: DhtOutcome::Failed {
                        code: "no_peers".into(),
                    },
                },
                _permit: None,
                bytes: 64,
            };
            if out.send(r).await.is_err() {
                return;
            }
            continue;
        }
        add(&p.c.with_peers, 1);
        let job = Job {
            info_hash: item.info_hash,
            via,
            peers,
        };
        tokio::select! {
            r = jobs.send(job) => if r.is_err() { return },
            _ = stop_requested(&mut stop) => return,
        }
    }
}

/// Classify and screen a verified dict here, before anything leaves the machine.
fn outcome_item(p: &Crawl, job: &Job, outcome: Outcome) -> Option<DhtOutcome> {
    Some(match outcome {
        Outcome::Ok(f) => {
            let Some(screen) = p.opts.screen.as_deref() else {
                // No blocklist loaded: nothing named may leave (fail closed). The server's
                // lease brings the hash back later.
                add(&p.c.unscreened, 1);
                return None;
            };
            let files: Vec<TorrentFile> = f
                .info
                .files
                .iter()
                .map(|x| TorrentFile {
                    path: x.path.replace('\0', ""),
                    size: Some(x.size),
                })
                .collect();
            let class = classify::classify(&f.info.name, &files);
            match screen.verdict(&f.info.all_names, class.adult()) {
                Verdict::Block(tier) => {
                    add(&p.c.blocked_here, 1);
                    DhtOutcome::Blocked {
                        v1: (f.v1 != job.info_hash).then(|| hex::encode(f.v1)),
                        tier: tier.as_str().into(),
                        rule_version: screen.rule_version(),
                    }
                }
                Verdict::Allow => {
                    let raw = f.raw.as_deref()?;
                    DhtOutcome::Metadata {
                        raw: data_encoding::BASE64.encode(raw),
                    }
                }
            }
        }
        Outcome::Private => DhtOutcome::Private,
        Outcome::V2Only => DhtOutcome::V2Only,
        Outcome::Invalid(e) => DhtOutcome::Invalid {
            code: e.code().into(),
        },
    })
}

fn cpu<R>(f: impl FnOnce() -> R) -> R {
    match tokio::runtime::Handle::try_current() {
        Ok(h) if h.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
            tokio::task::block_in_place(f)
        }
        _ => f(),
    }
}

async fn fetch_worker(
    p: Arc<Crawl>,
    jobs: Arc<tokio::sync::Mutex<mpsc::Receiver<Job>>>,
    out: mpsc::Sender<Pending>,
    mut stop: watch::Receiver<bool>,
) {
    loop {
        let job = tokio::select! {
            j = async { jobs.lock().await.recv().await } => match j {
                Some(j) => j,
                None => return,
            },
            _ = stop_requested(&mut stop) => return,
        };
        let result = tokio::select! {
            r = p.fetcher.fetch(job.info_hash, &job.peers) => r,
            _ = stop_requested(&mut stop) => return,
        };
        let outcome = match result {
            Ok(o) => {
                add(&p.c.verified, 1);
                let Ok(_slot) = p.screen_slots.acquire().await else {
                    return;
                };
                match cpu(|| outcome_item(&p, &job, o)) {
                    Some(o) => o,
                    None => continue,
                }
            }
            Err(f) if f.oversize => DhtOutcome::Oversize,
            Err(f) => DhtOutcome::Failed {
                code: f.code().into(),
            },
        };
        let bytes = match &outcome {
            DhtOutcome::Metadata { raw } => raw.len() + 128,
            _ => 128,
        };
        // A dict waits for room in the upload buffer (this is what bounds the memory).
        let units = (bytes / 1024).max(1).min(p.buffer_units as usize) as u32;
        let permit = tokio::select! {
            r = p.buffer.clone().acquire_many_owned(units) => match r {
                Ok(x) => x,
                Err(_) => return,
            },
            _ = stop_requested(&mut stop) => return,
        };
        let pending = Pending {
            item: DhtResult {
                sighted: hex::encode(job.info_hash),
                via: via_text(job.via).into(),
                peers_seen: job.peers.len() as u32,
                outcome,
            },
            _permit: Some(permit),
            bytes,
        };
        if out.send(pending).await.is_err() {
            return;
        }
    }
}

async fn upload(p: &Crawl, batch: &mut Vec<Pending>, abort: &Abort) {
    if batch.is_empty() {
        return;
    }
    let pending = std::mem::take(batch);
    let n = pending.len() as u64;
    let mut permits = Vec::with_capacity(pending.len());
    let mut items = Vec::with_capacity(pending.len());
    for x in pending {
        items.push(x.item);
        permits.push(x._permit);
    }
    let body = DhtResults { items };
    add(&p.c.upload_requests, 1);
    // Retry until it goes through: the buffer fills meanwhile and fetching waits. Never
    // given up while running (the hashes' server leases would expire for nothing).
    let mut wait = Duration::from_secs(1);
    loop {
        match p.client.post::<_, DhtAck>(DHT_RESULTS, &body).await {
            Ok(a) => {
                add(&p.c.uploaded, n);
                add(&p.c.server_stored, u64::from(a.stored));
                add(&p.c.server_not_new, u64::from(a.not_new));
                add(&p.c.server_blocked, u64::from(a.blocked));
                add(&p.c.server_rejected, u64::from(a.rejected));
                add(&p.c.server_ignored, u64::from(a.ignored));
                break;
            }
            Err(e) if e.is_fatal() => {
                abort.fatal(&e);
                return;
            }
            Err(e) => {
                add(&p.c.upload_errors, 1);
                if get(&p.c.upload_errors).is_multiple_of(20) || wait.as_secs() <= 1 {
                    tracing::warn!("worker: upload: {e}; retrying");
                }
                if abort.stopping() && wait >= Duration::from_secs(8) {
                    tracing::warn!(
                        "worker: stopping with {n} results not uploaded (their leases expire)"
                    );
                    return;
                }
                tokio::time::sleep(wait).await;
                wait = (wait * 2).min(Duration::from_secs(60));
            }
        }
    }
    drop(permits);
}

/// Batches results into uploads (≤ `max_results` items, ≤ 4 MiB of dicts, or 2 s). Runs until
/// every sender is gone, so results finished before a stop are still sent.
async fn uploader(p: Arc<Crawl>, mut rx: mpsc::Receiver<Pending>, abort: Abort) {
    let mut batch: Vec<Pending> = Vec::new();
    let mut bytes = 0usize;
    let mut flush_at = Instant::now() + UPLOAD_WAIT;
    loop {
        tokio::select! {
            r = rx.recv() => match r {
                Some(x) => {
                    if batch.is_empty() {
                        flush_at = Instant::now() + UPLOAD_WAIT;
                    }
                    if !batch.is_empty() && bytes + x.bytes > UPLOAD_BYTES {
                        upload(&p, &mut batch, &abort).await;
                        bytes = 0;
                    }
                    bytes += x.bytes;
                    batch.push(x);
                    p.c.upload_backlog.store(batch.len() as u64, Ordering::Relaxed);
                    if batch.len() >= p.max_results {
                        upload(&p, &mut batch, &abort).await;
                        bytes = 0;
                    }
                }
                None => break,
            },
            _ = tokio::time::sleep_until(flush_at), if !batch.is_empty() => {
                upload(&p, &mut batch, &abort).await;
                bytes = 0;
            }
        }
        p.c.upload_backlog
            .store(batch.len() as u64, Ordering::Relaxed);
    }
    upload(&p, &mut batch, &abort).await;
}

/// Sampling share and brakes: queue backpressure (D62) and the governor (D100).
async fn control(p: Arc<Crawl>, dht: Dht, mut stop: watch::Receiver<bool>) {
    let high = (p.opts.queue_cap / 2).clamp(1, QUEUE_HIGH);
    let low = high / 2;
    let mut queue_paused = false;
    loop {
        tokio::select! {
            _ = tokio::time::sleep(CONTROL_EVERY) => {}
            _ = stop_requested(&mut stop) => return,
        }
        let len = p.queue.lock().unwrap().len();
        if queue_paused && len <= low {
            queue_paused = false;
        } else if !queue_paused && len >= high {
            queue_paused = true;
        }
        let pressure = p.governor.pressure();
        let paused = queue_paused || pressure;
        if paused {
            add(&p.c.paused_ms, CONTROL_EVERY.as_millis() as u64);
        }
        let share = if paused { 0.0 } else { 1.0 };
        if dht.sample_share() != share {
            dht.set_sample_share(share);
        }
        if pressure || p.governor.conntrack_full() {
            let until = Instant::now() + PRESSURE_BRAKE;
            dht.brake_until(until);
            p.fetcher.brake_until(until);
        }
    }
}

fn save_state(dir: &std::path::Path, dht: &Dht, ports: &[u16]) {
    if let Err(e) = state::save(
        dir,
        dht.identity().as_ref(),
        ports,
        &dht.nodes_to_save(1000),
    ) {
        tracing::warn!("worker: saving DHT state: {e}");
    }
}

pub struct CrawlSetup {
    pub opts: DhtOptions,
    pub state_dir: PathBuf,
    pub upload_buffer: usize,
    pub max_results: usize,
}

/// Start the crawler; it runs until `stop` and returns its report.
pub async fn start(
    setup: CrawlSetup,
    client: Client,
    governor: Arc<Governor>,
    abort: Abort,
    stop: watch::Receiver<bool>,
    engine_tweak: impl FnOnce(&mut EngineConfig),
) -> anyhow::Result<(Arc<Crawl>, tokio::task::JoinHandle<CrawlReport>)> {
    let opts = setup.opts;
    let mut cfg = EngineConfig::new(opts.mode);
    cfg.ports = opts.ports.clone();
    cfg.qps = opts.qps;
    cfg.pool_cap = opts.pool_cap;
    cfg.lookup_timeout = opts.lookup_timeout;
    let (identity, nodes) = state::load(&setup.state_dir, &opts.ports);
    cfg.identity = identity;
    cfg.known_nodes = nodes;
    engine_tweak(&mut cfg);
    if cfg.bootstrap.is_empty() && !cfg.allow_loopback {
        cfg.bootstrap = crate::dht::resolve_bootstrap().await;
    }
    anyhow::ensure!(
        !cfg.bootstrap.is_empty() || !cfg.known_nodes.is_empty(),
        "DHT: no bootstrap router resolved and no saved node"
    );
    let (dht, rx) = Dht::start(cfg.clone())
        .await
        .map_err(|e| anyhow::anyhow!("DHT: binding UDP {:?}: {e}", opts.ports))?;
    tracing::info!(
        "worker DHT {:?}: {} node(s) on UDP {:?}, {} q/s, pool {}, {} fetches, TCP {}/s ≤ {}{}",
        opts.mode,
        dht.virtual_nodes(),
        dht.local_ports(),
        opts.qps,
        opts.pool_cap,
        opts.fetch_concurrency,
        opts.tcp_rate,
        opts.tcp_max,
        if cfg.identity.is_some() {
            ", saved identity"
        } else {
            ""
        }
    );
    let fetcher = Fetcher::new(FetchConfig {
        metadata_max: opts.metadata_max,
        // In flight: room for a few maximal dicts, at most the server's 256 MiB.
        inflight_max: (opts.metadata_max * 4).clamp(8 << 20, 256 << 20),
        decode_parallel: 2,
        connects_per_sec: opts.tcp_rate,
        max_connections: opts.tcp_max,
        allow_loopback: cfg.allow_loopback,
        max_peers: opts.peers_per_hash.max(1),
        keep_raw: true,
        ..FetchConfig::default()
    });
    let units = u32::try_from((setup.upload_buffer / 1024).max(64)).unwrap_or(u32::MAX >> 4);
    let p = Arc::new(Crawl {
        client,
        queue: Mutex::new(FetchQueue::new(opts.queue_cap)),
        notify: Notify::new(),
        fetcher,
        c: Counters::default(),
        health: Arc::default(),
        governor,
        buffer: Arc::new(Semaphore::new(units as usize)),
        buffer_units: units,
        screen_slots: Semaphore::new(2),
        dht: Mutex::new(Some(dht.clone())),
        snap: Mutex::default(),
        max_results: setup.max_results.clamp(1, crate::fleet::MAX_RESULTS),
        opts,
    });
    let started = Instant::now();
    let state_dir = setup.state_dir;
    let p2 = p.clone();
    let handle = tokio::spawn(async move {
        let p = p2;
        let (int_tx, int_rx) = watch::channel(false);
        let mut tasks = JoinSet::new();
        let (batch_tx, batch_rx) = mpsc::channel::<Vec<Sighting>>(4);
        let triage = tokio::spawn(triage_task(
            p.clone(),
            batch_rx,
            abort.clone(),
            int_rx.clone(),
        ));
        tasks.spawn(triage_loop(p.clone(), rx, batch_tx, int_rx.clone()));
        let workers = p.opts.fetch_concurrency.max(1);
        let (jobs_tx, jobs_rx) = mpsc::channel::<Job>(workers);
        let jobs_rx = Arc::new(tokio::sync::Mutex::new(jobs_rx));
        let (out_tx, out_rx) = mpsc::channel::<Pending>(workers * 2 + 64);
        let up = tokio::spawn(uploader(p.clone(), out_rx, abort.clone()));
        for _ in 0..workers {
            tasks.spawn(fetch_worker(
                p.clone(),
                jobs_rx.clone(),
                out_tx.clone(),
                int_rx.clone(),
            ));
        }
        for _ in 0..p.opts.peer_lookups.max(1) {
            tasks.spawn(collector(
                p.clone(),
                dht.clone(),
                jobs_tx.clone(),
                out_tx.clone(),
                int_rx.clone(),
            ));
        }
        drop(jobs_tx);
        drop(out_tx);
        tasks.spawn(health::health_loop(
            p.opts.probe.clone(),
            dht.clone(),
            p.fetcher.clone(),
            p.health.clone(),
            int_rx.clone(),
        ));
        tasks.spawn(control(p.clone(), dht.clone(), int_rx.clone()));
        let ports = p.opts.ports.clone();
        {
            let (dht, dir, ports, mut stop) = (
                dht.clone(),
                state_dir.clone(),
                ports.clone(),
                int_rx.clone(),
            );
            tasks.spawn(async move {
                loop {
                    tokio::select! {
                        _ = tokio::time::sleep(PERSIST_EVERY) => save_state(&dir, &dht, &ports),
                        _ = stop_requested(&mut stop) => return,
                    }
                }
            });
        }
        let mapper = p.opts.upnp.clone().map(|cfg| {
            let wanted: Vec<(upnp::Protocol, u16)> = dht
                .local_ports()
                .into_iter()
                .filter(|port| *port != 0)
                .map(|port| (upnp::Protocol::Udp, port))
                .collect();
            tokio::spawn(upnp::upnp_loop(
                cfg,
                wanted,
                upnp::Status::default(),
                int_rx.clone(),
            ))
        });
        let mut stop = stop;
        stop_requested(&mut stop).await;
        let _ = int_tx.send(true);
        let deadline = Instant::now() + Duration::from_secs(4);
        while let Ok(Some(_)) = tokio::time::timeout_at(deadline, tasks.join_next()).await {}
        tasks.abort_all();
        while tasks.join_next().await.is_some() {}
        let _ = tokio::time::timeout(Duration::from_secs(5), triage).await;
        // Every result sender is gone: the uploader sends what is left and ends.
        let _ = tokio::time::timeout(Duration::from_secs(20), up).await;
        if let Some(m) = mapper {
            let _ = tokio::time::timeout(Duration::from_secs(3), m).await;
        }
        save_state(&state_dir, &dht, &ports);
        let rep = report(&p, &dht, started);
        *p.dht.lock().unwrap() = None;
        dht.shutdown().await;
        rep
    });
    Ok((p, handle))
}

fn report(p: &Crawl, dht: &Dht, started: Instant) -> CrawlReport {
    let secs = started.elapsed().as_secs_f64().max(0.001);
    let e = dht.stats();
    CrawlReport {
        mode: match p.opts.mode {
            Mode::Polite => "polite".into(),
            Mode::Aggressive => "aggressive".into(),
        },
        ports: dht.local_ports(),
        seconds: secs,
        sightings: get(&p.c.sightings),
        unique_hashes: get(&p.c.unique),
        triaged: get(&p.c.triaged),
        queued_by_server: get(&p.c.queued),
        known_at_server: get(&p.c.triage_known),
        blocked_at_server: get(&p.c.triage_blocked),
        triage_errors: get(&p.c.triage_errors),
        triage_dropped: get(&p.c.triage_dropped),
        collected: get(&p.c.collected),
        with_peers: get(&p.c.with_peers),
        no_peers: get(&p.c.no_peers),
        verified: get(&p.c.verified),
        blocked_here: get(&p.c.blocked_here),
        uploaded: get(&p.c.uploaded),
        upload_requests: get(&p.c.upload_requests),
        upload_errors: get(&p.c.upload_errors),
        server_stored: get(&p.c.server_stored),
        server_not_new: get(&p.c.server_not_new),
        server_blocked: get(&p.c.server_blocked),
        server_rejected: get(&p.c.server_rejected),
        server_ignored: get(&p.c.server_ignored),
        sampling_paused_secs: get(&p.c.paused_ms) as f64 / 1000.0,
        queries_per_sec: e.queries_sent as f64 / secs,
        answered_share: if e.queries_sent > 0 {
            e.answered as f64 / e.queries_sent as f64
        } else {
            0.0
        },
        fetch: p.fetcher.stats(),
        health: p.health.lock().unwrap().clone(),
        engine: e,
    }
}

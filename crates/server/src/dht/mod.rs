//! DHT crawler daemon (docs/DHT.md §3, D49–D55): the `core::dht` node, triage of sighted
//! infohashes (seen filter → Postgres), the persisted queue (`dht_seen`), peer collection for the
//! metadata stage, identity/contact persistence and a JSON report.
//!
//! Each queued hash gets its peers (announcer, the node that sampled it, or an iterative
//! lookup); a hash with no peer at all is marked `failed` (`no_peers`) and backs off. Hashes with
//! peers go to the BEP 9 fetchers (`core::metainfo`, stage 2), whose results are written in
//! batches by [`persist`]: verified torrents pass the content [`screen`] first, and without one
//! nothing named is stored (fail closed until the stage 3 blocklist exists).

pub mod health;
pub mod persist;
pub mod queue;
pub mod screen;
pub mod seen;
pub mod store;

use std::collections::HashSet;
use std::net::{SocketAddr, SocketAddrV4};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use magnetar_core::dht::{Dht, EngineConfig, EngineStats, Mode, Sighting, Via, collect_peers};
use magnetar_core::liveness::dht::BOOTSTRAP;
use magnetar_core::metainfo::{FetchConfig, FetchStats, Fetcher};
use serde::Serialize;
use sqlx::PgPool;
use tokio::sync::{Notify, mpsc, watch};
use tokio::task::JoinSet;
use tokio::time::Instant;

use crate::config::{Config, DhtMode};
use crate::daemons::stop_requested;
use persist::{FetchResult, PersistOutcome};
use queue::{FetchItem, FetchQueue};
use screen::Screen;
use seen::SeenFilter;

const BATCH_MAX: usize = 1000;
const BATCH_WAIT: Duration = Duration::from_secs(1);
const RETRY_EVERY: Duration = Duration::from_secs(60);
const PERSIST_EVERY: Duration = Duration::from_secs(600);
/// Budget of one iterative peer lookup (docs/DHT.md §4 step 1).
const LOOKUP_BUDGET: Duration = Duration::from_secs(10);
/// Stop a lookup once this many peers are known: the metadata stage tries ≤ 12 per hash (D56).
const WANT_PEERS: usize = 12;
/// Backpressure (D62): sampling pauses while this many hashes wait for peer collection and
/// resumes below half of it, so discovery does not outrun consumption.
const QUEUE_HIGH: usize = 10_000;
const BACKPRESSURE_EVERY: Duration = Duration::from_millis(500);
const PEER_SAMPLE_CAP: usize = 100_000;

#[derive(Clone, Debug)]
pub struct DhtOptions {
    /// Metadata fetches in flight (`MAGNETAR_DHT_FETCH_CONCURRENCY`).
    pub fetch_concurrency: usize,
    /// Largest info dict accepted, bytes (`MAGNETAR_DHT_METADATA_MAX`).
    pub metadata_max: usize,
    /// The content screen (D53). None: verified metadata is never stored (fail closed).
    pub screen: Option<Arc<dyn Screen>>,
    /// New TCP connection attempts per second (`MAGNETAR_DHT_TCP_RATE`, D66).
    pub tcp_rate: u32,
    /// TCP connections open at once (`MAGNETAR_DHT_TCP_MAX`, D66).
    pub tcp_max: usize,
    /// Router health probe targets (`MAGNETAR_DHT_PROBE`; empty = off, D66).
    pub probe: Vec<SocketAddr>,
    pub mode: Mode,
    pub ports: Vec<u16>,
    pub qps: u32,
    pub pool_cap: usize,
    pub fetch_known: bool,
    pub peer_lookups: usize,
    /// No database: dedupe in memory only, nothing read or written.
    pub dry_run: bool,
    pub bloom_bits: u64,
    pub queue_cap: usize,
}

impl DhtOptions {
    /// Options for `mode` (None for `off`) from the configuration (docs/DHT.md §3.2, §8).
    pub fn from_config(cfg: &Config, mode: DhtMode) -> Option<Self> {
        let mode = match mode {
            DhtMode::Off => return None,
            DhtMode::Polite => Mode::Polite,
            DhtMode::Aggressive => Mode::Aggressive,
        };
        let aggressive = mode == Mode::Aggressive;
        let nodes = cfg
            .dht_nodes
            .unwrap_or(if aggressive { 4 } else { 1 })
            .clamp(1, 8);
        let ports = (0..nodes)
            .map(|i| {
                if aggressive {
                    cfg.dht_port + u16::from(i)
                } else {
                    0
                }
            })
            .collect();
        Some(Self {
            mode,
            ports,
            qps: cfg.dht_qps.unwrap_or(if aggressive { 200 } else { 50 }),
            pool_cap: cfg.dht_pool.map(|p| p as usize).unwrap_or(if aggressive {
                1_000_000
            } else {
                200_000
            }),
            fetch_known: cfg.dht_fetch_known,
            peer_lookups: cfg
                .dht_peer_lookups
                .map(|n| n as usize)
                .unwrap_or(if aggressive { 64 } else { 8 }),
            dry_run: false,
            bloom_bits: seen::DEFAULT_BITS,
            queue_cap: queue::DEFAULT_CAP,
            fetch_concurrency: cfg
                .dht_fetch_concurrency
                .map(|n| n as usize)
                .unwrap_or(if aggressive { 128 } else { 16 }),
            metadata_max: cfg.dht_metadata_max as usize,
            screen: None,
            tcp_rate: cfg.dht_tcp_rate,
            tcp_max: cfg.dht_tcp_max as usize,
            probe: health::parse_targets(&cfg.dht_probe),
        })
    }
}

/// Resolve the public bootstrap routers (IPv4, global addresses only).
pub async fn resolve_bootstrap() -> Vec<SocketAddrV4> {
    let lookups = BOOTSTRAP.iter().map(|host| async move {
        match tokio::time::timeout(Duration::from_secs(3), tokio::net::lookup_host(*host)).await {
            Ok(Ok(addrs)) => addrs
                .filter_map(|a| match a {
                    SocketAddr::V4(v) if magnetar_core::dht::addr::is_global(*v.ip()) => Some(v),
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        }
    });
    let mut out: Vec<SocketAddrV4> = futures::future::join_all(lookups)
        .await
        .into_iter()
        .flatten()
        .collect();
    out.sort();
    out.dedup();
    out
}

#[derive(Default)]
struct Counters {
    sightings: AtomicU64,
    unique: AtomicU64,
    bloom_skipped: AtomicU64,
    triaged: AtomicU64,
    blocked: AtomicU64,
    known: AtomicU64,
    queued: AtomicU64,
    retried: AtomicU64,
    triage_errors: AtomicU64,
    collected: AtomicU64,
    with_peers: AtomicU64,
    peers_total: AtomicU64,
    from_announcer: AtomicU64,
    from_source: AtomicU64,
    looked_up: AtomicU64,
    lookup_queries: AtomicU64,
    marked_failed: AtomicU64,
    paused_ms: AtomicU64,
    persist_errors: AtomicU64,
}

fn add(a: &AtomicU64, n: u64) {
    a.fetch_add(n, Ordering::Relaxed);
}

fn get(a: &AtomicU64) -> u64 {
    a.load(Ordering::Relaxed)
}

/// A hash with its peers, ready for the metadata fetcher.
#[derive(Clone, Debug)]
pub struct FetchJob {
    pub info_hash: [u8; 20],
    pub via: Via,
    pub peers: Vec<SocketAddrV4>,
}

struct Pipeline {
    pool: Option<PgPool>,
    opts: DhtOptions,
    queue: Mutex<FetchQueue>,
    notify: Notify,
    c: Counters,
    /// Peers per collected hash (a sample for the report's percentiles).
    peer_counts: Mutex<Vec<u32>>,
    fetcher: Fetcher,
    persisted: Mutex<PersistOutcome>,
    health: Arc<Mutex<health::HealthStats>>,
}

impl Pipeline {
    fn enqueue(&self, item: FetchItem) {
        if self.queue.lock().unwrap().push(item) {
            self.notify.notify_one();
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct PeerStats {
    pub hashes: u64,
    pub with_peers: u64,
    pub share_with_peers: f64,
    pub mean: f64,
    pub median: u32,
    pub p90: u32,
    pub max: u32,
    pub from_announcer: u64,
    pub from_sampling_node: u64,
    pub iterative_lookups: u64,
    pub queries_per_hash: f64,
}

#[derive(Clone, Debug, Serialize)]
pub struct Bandwidth {
    /// UDP payload bytes (KRPC) and TCP bytes (peer wire), headers not included.
    pub udp_in: u64,
    pub udp_out: u64,
    pub tcp_in: u64,
    pub tcp_out: u64,
    pub mbit_per_sec_in: f64,
    pub mbit_per_sec_out: f64,
}

#[derive(Clone, Debug, Serialize)]
pub struct Report {
    pub mode: Mode,
    pub dry_run: bool,
    pub seconds: f64,
    pub virtual_nodes: usize,
    pub ports: Vec<u16>,
    pub sightings: u64,
    pub unique_hashes: u64,
    pub unique_hashes_per_min: f64,
    pub bloom_skipped: u64,
    pub triaged: u64,
    pub blocked: u64,
    pub known: u64,
    pub queued: u64,
    pub queued_per_min: f64,
    pub retried: u64,
    pub marked_no_peers: u64,
    pub triage_errors: u64,
    pub queue_len: usize,
    pub queue_lanes: [usize; 3],
    pub queue_overflow: u64,
    /// Seconds the sampler was paused by backpressure (D62).
    pub sampling_paused_secs: f64,
    pub peers: PeerStats,
    pub fetch: FetchStats,
    /// Verified info dicts per minute (stored or not: the screen decides that).
    pub metadata_per_min: f64,
    /// Verified share of finished fetches that had at least one peer.
    pub fetch_success_rate: f64,
    pub persist: PersistOutcome,
    pub persist_errors: u64,
    pub bandwidth: Bandwidth,
    /// Router health probe (D66).
    pub health: health::HealthStats,
    pub engine: EngineStats,
    pub answered_share: f64,
    pub queries_per_sec: f64,
    pub cpu_seconds: Option<f64>,
    pub rss_peak_mb: Option<f64>,
    pub rss_mb: Option<f64>,
}

fn proc_status_kb(field: &str) -> Option<f64> {
    let s = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = s.lines().find(|l| l.starts_with(field))?;
    line.split_whitespace().nth(1)?.parse::<f64>().ok()
}

/// utime + stime of this process from /proc/self/stat (clock ticks of 1/100 s, the Linux
/// USER_HZ on every mainstream architecture).
fn proc_cpu_seconds() -> Option<f64> {
    let s = std::fs::read_to_string("/proc/self/stat").ok()?;
    let rest = s.get(s.rfind(')')? + 2..)?;
    let f: Vec<&str> = rest.split_whitespace().collect();
    let ut: f64 = f.get(11)?.parse().ok()?;
    let st: f64 = f.get(12)?.parse().ok()?;
    Some((ut + st) / 100.0)
}

fn peer_stats(p: &Pipeline) -> PeerStats {
    let mut v = p.peer_counts.lock().unwrap().clone();
    v.sort_unstable();
    let n = get(&p.c.collected);
    let with = get(&p.c.with_peers);
    let pick = |q: f64| -> u32 {
        if v.is_empty() {
            0
        } else {
            v[((v.len() - 1) as f64 * q).round() as usize]
        }
    };
    PeerStats {
        hashes: n,
        with_peers: with,
        share_with_peers: if n > 0 { with as f64 / n as f64 } else { 0.0 },
        mean: if n > 0 {
            get(&p.c.peers_total) as f64 / n as f64
        } else {
            0.0
        },
        median: pick(0.5),
        p90: pick(0.9),
        max: v.last().copied().unwrap_or(0),
        from_announcer: get(&p.c.from_announcer),
        from_sampling_node: get(&p.c.from_source),
        iterative_lookups: get(&p.c.looked_up),
        queries_per_hash: if n > 0 {
            get(&p.c.lookup_queries) as f64 / n as f64
        } else {
            0.0
        },
    }
}

fn report(p: &Pipeline, dht: &Dht, started: Instant) -> Report {
    let secs = started.elapsed().as_secs_f64().max(0.001);
    let mins = secs / 60.0;
    let engine = dht.stats();
    let fetch = p.fetcher.stats();
    let verified = fetch.ok + fetch.private + fetch.v2_only + fetch.invalid;
    let finished = verified + fetch.failed + fetch.oversize;
    let bandwidth = Bandwidth {
        udp_in: engine.udp_bytes_in,
        udp_out: engine.udp_bytes_out,
        tcp_in: fetch.tcp_bytes_in,
        tcp_out: fetch.tcp_bytes_out,
        mbit_per_sec_in: (engine.udp_bytes_in + fetch.tcp_bytes_in) as f64 * 8.0 / secs / 1e6,
        mbit_per_sec_out: (engine.udp_bytes_out + fetch.tcp_bytes_out) as f64 * 8.0 / secs / 1e6,
    };
    let q = p.queue.lock().unwrap();
    Report {
        mode: p.opts.mode,
        dry_run: p.opts.dry_run,
        seconds: secs,
        virtual_nodes: dht.virtual_nodes(),
        ports: dht.local_ports(),
        sightings: get(&p.c.sightings),
        unique_hashes: get(&p.c.unique),
        unique_hashes_per_min: get(&p.c.unique) as f64 / mins,
        bloom_skipped: get(&p.c.bloom_skipped),
        triaged: get(&p.c.triaged),
        blocked: get(&p.c.blocked),
        known: get(&p.c.known),
        queued: get(&p.c.queued),
        queued_per_min: get(&p.c.queued) as f64 / mins,
        retried: get(&p.c.retried),
        marked_no_peers: get(&p.c.marked_failed),
        triage_errors: get(&p.c.triage_errors),
        queue_len: q.len(),
        queue_lanes: q.lane_lens(),
        queue_overflow: q.overflow,
        sampling_paused_secs: get(&p.c.paused_ms) as f64 / 1000.0,
        peers: peer_stats(p),
        metadata_per_min: verified as f64 / mins,
        fetch_success_rate: if finished > 0 {
            verified as f64 / finished as f64
        } else {
            0.0
        },
        fetch,
        persist: p.persisted.lock().unwrap().clone(),
        persist_errors: get(&p.c.persist_errors),
        bandwidth,
        health: p.health.lock().unwrap().clone(),
        answered_share: if engine.queries_sent > 0 {
            engine.answered as f64 / engine.queries_sent as f64
        } else {
            0.0
        },
        queries_per_sec: engine.queries_sent as f64 / secs,
        engine,
        cpu_seconds: proc_cpu_seconds(),
        rss_peak_mb: proc_status_kb("VmHWM:").map(|k| k / 1024.0),
        rss_mb: proc_status_kb("VmRSS:").map(|k| k / 1024.0),
    }
}

async fn flush(p: &Pipeline, batch: &mut Vec<Sighting>) {
    if batch.is_empty() {
        return;
    }
    let items = std::mem::take(batch);
    add(&p.c.triaged, items.len() as u64);
    let queued: HashSet<[u8; 20]> = match &p.pool {
        Some(pool) if !p.opts.dry_run => {
            let b: Vec<([u8; 20], Via)> = items.iter().map(|s| (s.info_hash, s.via)).collect();
            match store::triage(pool, &b, magnetar_core::now(), p.opts.fetch_known).await {
                Ok(o) => {
                    add(&p.c.blocked, o.blocked as u64);
                    add(&p.c.known, o.known as u64);
                    o.queued.into_iter().collect()
                }
                Err(e) => {
                    if get(&p.c.triage_errors).is_multiple_of(100) {
                        tracing::warn!("DHT triage: {e}");
                    }
                    add(&p.c.triage_errors, 1);
                    return;
                }
            }
        }
        _ => items.iter().map(|s| s.info_hash).collect(),
    };
    add(&p.c.queued, queued.len() as u64);
    let now = Instant::now();
    // The announce (with its peer) wins over other sightings of the same hash in the batch.
    let mut done = HashSet::new();
    let mut ordered = items;
    ordered.sort_by_key(|s| s.via != Via::Announce);
    for s in ordered {
        if queued.contains(&s.info_hash) && done.insert(s.info_hash) {
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

async fn triage_loop(
    p: Arc<Pipeline>,
    mut rx: mpsc::Receiver<Sighting>,
    mut stop: watch::Receiver<bool>,
) {
    let mut seen = SeenFilter::new(p.opts.bloom_bits, seen::DEFAULT_K, seen::ROTATE_EVERY);
    let mut batch: Vec<Sighting> = Vec::new();
    let mut flush_at = Instant::now() + BATCH_WAIT;
    loop {
        tokio::select! {
            s = rx.recv() => match s {
                Some(s) => {
                    add(&p.c.sightings, 1);
                    if seen.check_and_insert(&s.info_hash, Instant::now()) {
                        add(&p.c.unique, 1);
                        if batch.is_empty() {
                            flush_at = Instant::now() + BATCH_WAIT;
                        }
                        batch.push(s);
                        if batch.len() >= BATCH_MAX {
                            flush(&p, &mut batch).await;
                        }
                    } else {
                        add(&p.c.bloom_skipped, 1);
                    }
                }
                None => break,
            },
            _ = tokio::time::sleep_until(flush_at), if !batch.is_empty() => flush(&p, &mut batch).await,
            _ = stop_requested(&mut stop) => break,
        }
    }
    // Hashes of a partial batch still reach dht_seen, so the next run does not lose them.
    flush(&p, &mut batch).await;
}

async fn collector(
    p: Arc<Pipeline>,
    dht: Dht,
    jobs: mpsc::Sender<FetchJob>,
    mut stop: watch::Receiver<bool>,
) {
    loop {
        let item = p.queue.lock().unwrap().pop();
        let Some(item) = item else {
            tokio::select! {
                _ = p.notify.notified() => {}
                _ = tokio::time::sleep(Duration::from_secs(1)) => {}
                _ = stop_requested(&mut stop) => return,
            }
            continue;
        };
        let c = tokio::select! {
            c = collect_peers(&dht, item.info_hash, item.peer, item.source, LOOKUP_BUDGET, WANT_PEERS) => c,
            _ = stop_requested(&mut stop) => return,
        };
        add(&p.c.collected, 1);
        add(&p.c.peers_total, c.peers.len() as u64);
        add(&p.c.lookup_queries, u64::from(c.queries));
        if !c.peers.is_empty() {
            add(&p.c.with_peers, 1);
        } else if let Some(pool) = &p.pool {
            // No swarm found: back off (1 h, 6 h, 24 h, then never) instead of holding the lease.
            match store::mark_failed(pool, &item.info_hash, "no_peers", magnetar_core::now()).await
            {
                Ok(()) => add(&p.c.marked_failed, 1),
                Err(e) => tracing::warn!("DHT: marking a fetch failed: {e}"),
            }
        }
        if item.peer.is_some() {
            add(&p.c.from_announcer, 1);
        }
        if c.from_source {
            add(&p.c.from_source, 1);
        }
        if c.looked_up {
            add(&p.c.looked_up, 1);
        }
        {
            let mut counts = p.peer_counts.lock().unwrap();
            if counts.len() < PEER_SAMPLE_CAP {
                counts.push(c.peers.len() as u32);
            }
        }
        if c.peers.is_empty() {
            continue;
        }
        let job = FetchJob {
            info_hash: item.info_hash,
            via: item.via,
            peers: c.peers,
        };
        // Blocks while every fetcher is busy: collection never runs far ahead of fetching.
        tokio::select! {
            r = jobs.send(job) => if r.is_err() { return },
            _ = stop_requested(&mut stop) => return,
        }
    }
}

/// One metadata fetch at a time; `fetch_concurrency` of these run.
async fn fetch_worker(
    p: Arc<Pipeline>,
    jobs: Arc<tokio::sync::Mutex<mpsc::Receiver<FetchJob>>>,
    results: mpsc::Sender<FetchResult>,
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
        // A fetch cut short by the stop keeps its dht_seen lease and comes back later.
        let result = tokio::select! {
            r = p.fetcher.fetch(job.info_hash, &job.peers) => r,
            _ = stop_requested(&mut stop) => return,
        };
        let r = FetchResult {
            sighted: job.info_hash,
            via: job.via,
            peers_seen: job.peers.len(),
            result,
        };
        if results.send(r).await.is_err() {
            return;
        }
    }
}

async fn persist_flush(p: &Pipeline, pool: &PgPool, batch: &mut Vec<FetchResult>) {
    if batch.is_empty() {
        return;
    }
    let items = std::mem::take(batch);
    let screen = p.opts.screen.as_deref();
    match persist::persist_batch(
        pool,
        &items,
        screen,
        p.opts.fetch_known,
        magnetar_core::now(),
    )
    .await
    {
        Ok(o) => {
            let mut t = p.persisted.lock().unwrap();
            t.stored += o.stored;
            t.not_new += o.not_new;
            t.alias += o.alias;
            t.blocked += o.blocked;
            t.unscreened += o.unscreened;
            t.private += o.private;
            t.v2_only += o.v2_only;
            t.invalid += o.invalid;
            t.oversize += o.oversize;
            t.failed += o.failed;
        }
        Err(e) => {
            // The hashes keep their dht_seen lease: the retry loop brings them back.
            if get(&p.c.persist_errors).is_multiple_of(100) {
                tracing::warn!("DHT persist: {e}");
            }
            add(&p.c.persist_errors, 1);
        }
    }
}

/// Batches fetch results into transactions (≤ 500 results or 1 s). Runs until every fetch
/// worker has dropped its sender, so results finished before a stop are still written.
async fn persist_loop(p: Arc<Pipeline>, mut rx: mpsc::Receiver<FetchResult>) {
    let mut batch = Vec::new();
    let mut flush_at = Instant::now() + persist::BATCH_WAIT;
    loop {
        tokio::select! {
            r = rx.recv() => match r {
                Some(r) => {
                    if p.pool.is_none() {
                        continue; // dry run: the fetcher's own counters are the report
                    }
                    if batch.is_empty() {
                        flush_at = Instant::now() + persist::BATCH_WAIT;
                    }
                    batch.push(r);
                    if batch.len() >= persist::BATCH_MAX
                        && let Some(pool) = &p.pool
                    {
                        persist_flush(&p, pool, &mut batch).await;
                    }
                }
                None => break,
            },
            _ = tokio::time::sleep_until(flush_at), if !batch.is_empty() => {
                if let Some(pool) = &p.pool {
                    persist_flush(&p, pool, &mut batch).await;
                }
            }
        }
    }
    if let Some(pool) = &p.pool {
        persist_flush(&p, pool, &mut batch).await;
    }
}

/// Pause sampling above the high-water mark, resume below half of it (hysteresis).
async fn backpressure(p: Arc<Pipeline>, dht: Dht, mut stop: watch::Receiver<bool>) {
    let high = (p.opts.queue_cap / 2).clamp(1, QUEUE_HIGH);
    let low = high / 2;
    loop {
        tokio::select! {
            _ = tokio::time::sleep(BACKPRESSURE_EVERY) => {}
            _ = stop_requested(&mut stop) => return,
        }
        let len = p.queue.lock().unwrap().len();
        let paused = dht.sample_share() <= 0.0;
        if paused {
            add(&p.c.paused_ms, BACKPRESSURE_EVERY.as_millis() as u64);
        }
        if let Some(share) = next_share(paused, len, high, low) {
            dht.set_sample_share(share);
        }
    }
}

/// The sampler share to switch to, if any: pause at `high` queued hashes, resume at `low`.
fn next_share(paused: bool, len: usize, high: usize, low: usize) -> Option<f64> {
    match (paused, len) {
        (false, l) if l >= high => Some(0.0),
        (true, l) if l <= low => Some(1.0),
        _ => None,
    }
}

async fn retry_loop(p: Arc<Pipeline>, pool: PgPool, mut stop: watch::Receiver<bool>) {
    loop {
        tokio::select! {
            _ = tokio::time::sleep(RETRY_EVERY) => {}
            _ = stop_requested(&mut stop) => return,
        }
        match store::take_due(&pool, magnetar_core::now(), store::RETRY_BATCH).await {
            Ok(rows) => {
                add(&p.c.retried, rows.len() as u64);
                let now = Instant::now();
                for (h, via) in rows {
                    p.enqueue(FetchItem {
                        info_hash: h,
                        via: store::via_from_code(via),
                        source: None,
                        peer: None,
                        retry: true,
                        queued_at: now,
                    });
                }
            }
            Err(e) => tracing::warn!("DHT retry: {e}"),
        }
    }
}

/// Saves the identity under the **configured** ports (0 = ephemeral), the key it is loaded by.
async fn persist(pool: &PgPool, dht: &Dht, ports: &[u16]) {
    if let Some(id) = dht.identity()
        && let Err(e) = store::save_identity(pool, &id, ports).await
    {
        tracing::warn!("DHT: saving identity: {e}");
    }
    let nodes = dht.nodes_to_save(1000);
    if !nodes.is_empty()
        && let Err(e) = store::save_nodes(pool, &nodes).await
    {
        tracing::warn!("DHT: saving nodes: {e}");
    }
}

async fn identity_loop(pool: PgPool, dht: Dht, ports: Vec<u16>, mut stop: watch::Receiver<bool>) {
    loop {
        tokio::select! {
            _ = tokio::time::sleep(PERSIST_EVERY) => persist(&pool, &dht, &ports).await,
            _ = stop_requested(&mut stop) => return,
        }
    }
}

/// Run the crawler until `stop` (or `budget`) and return the report. With `pool` = None or
/// `dry_run`, nothing touches the database.
pub async fn run(
    pool: Option<PgPool>,
    opts: DhtOptions,
    mut stop: watch::Receiver<bool>,
    budget: Option<Duration>,
    engine_tweak: impl FnOnce(&mut EngineConfig),
) -> anyhow::Result<Report> {
    let db = pool.filter(|_| !opts.dry_run);
    let mut cfg = EngineConfig::new(opts.mode);
    cfg.ports = opts.ports.clone();
    cfg.qps = opts.qps;
    cfg.pool_cap = opts.pool_cap;
    if let Some(pool) = &db {
        cfg.known_nodes = store::load_nodes(pool).await?;
        cfg.identity = store::load_identity(pool, &opts.ports).await?;
    }
    // Tests replace the bootstrap list (and open the loopback seam) here.
    engine_tweak(&mut cfg);
    if cfg.bootstrap.is_empty() && !cfg.allow_loopback {
        cfg.bootstrap = resolve_bootstrap().await;
    }
    if cfg.bootstrap.is_empty() && cfg.known_nodes.is_empty() {
        anyhow::bail!("DHT: no bootstrap router resolved and no saved node");
    }
    let (dht, rx) = Dht::start(cfg.clone())
        .await
        .map_err(|e| anyhow::anyhow!("DHT: binding UDP {:?}: {e}", opts.ports))?;
    tracing::info!(
        "DHT crawler {:?}: {} virtual node(s) on UDP {:?}, {} q/s, {} bootstrap + {} saved nodes{}",
        opts.mode,
        dht.virtual_nodes(),
        dht.local_ports(),
        opts.qps,
        cfg.bootstrap.len(),
        cfg.known_nodes.len(),
        if cfg.identity.is_some() {
            ", saved identity"
        } else {
            ""
        }
    );
    let started = Instant::now();
    let fetcher = Fetcher::new(FetchConfig {
        metadata_max: opts.metadata_max,
        connects_per_sec: opts.tcp_rate,
        max_connections: opts.tcp_max,
        // The engine's loopback seam (fake networks in tests) opens the fetcher's too.
        allow_loopback: cfg.allow_loopback,
        ..FetchConfig::default()
    });
    let p = Arc::new(Pipeline {
        pool: db.clone(),
        queue: Mutex::new(FetchQueue::new(opts.queue_cap)),
        notify: Notify::new(),
        c: Counters::default(),
        peer_counts: Mutex::default(),
        fetcher,
        persisted: Mutex::default(),
        health: Arc::default(),
        opts,
    });
    let (int_tx, int_rx) = watch::channel(false);
    let mut tasks = JoinSet::new();
    tasks.spawn(triage_loop(p.clone(), rx, int_rx.clone()));
    let workers = p.opts.fetch_concurrency.max(1);
    let (jobs_tx, jobs_rx) = mpsc::channel::<FetchJob>(workers);
    let jobs_rx = Arc::new(tokio::sync::Mutex::new(jobs_rx));
    let (res_tx, res_rx) = mpsc::channel::<FetchResult>(workers * 2);
    let persister = tokio::spawn(persist_loop(p.clone(), res_rx));
    for _ in 0..workers {
        tasks.spawn(fetch_worker(
            p.clone(),
            jobs_rx.clone(),
            res_tx.clone(),
            int_rx.clone(),
        ));
    }
    drop(res_tx);
    for _ in 0..p.opts.peer_lookups.max(1) {
        tasks.spawn(collector(
            p.clone(),
            dht.clone(),
            jobs_tx.clone(),
            int_rx.clone(),
        ));
    }
    drop(jobs_tx);
    tasks.spawn(health::health_loop(
        p.opts.probe.clone(),
        dht.clone(),
        p.fetcher.clone(),
        p.health.clone(),
        int_rx.clone(),
    ));
    tasks.spawn(backpressure(p.clone(), dht.clone(), int_rx.clone()));
    if let Some(pool) = &db {
        tasks.spawn(retry_loop(p.clone(), pool.clone(), int_rx.clone()));
        tasks.spawn(identity_loop(
            pool.clone(),
            dht.clone(),
            p.opts.ports.clone(),
            int_rx.clone(),
        ));
    }
    let until = budget.map(|b| started + b);
    tokio::select! {
        _ = stop_requested(&mut stop) => {}
        _ = async { match until { Some(u) => tokio::time::sleep_until(u).await, None => std::future::pending().await } } => {}
    }
    let rep = report(&p, &dht, started);
    let _ = int_tx.send(true);
    // Inside serve's 10 s daemon grace: tasks get 4 s, then the final persist and shutdown.
    let deadline = Instant::now() + Duration::from_secs(4);
    while let Ok(Some(_)) = tokio::time::timeout_at(deadline, tasks.join_next()).await {}
    tasks.abort_all();
    while tasks.join_next().await.is_some() {}
    // Every result sender is gone now: the persister writes what is left and ends.
    let _ = tokio::time::timeout(Duration::from_secs(3), persister).await;
    let mut rep = rep;
    rep.persist = p.persisted.lock().unwrap().clone();
    rep.persist_errors = get(&p.c.persist_errors);
    if let Some(pool) = &db {
        persist(pool, &dht, &p.opts.ports).await;
    }
    dht.shutdown().await;
    tracing::info!(
        "DHT crawler stopped: {} unique hashes, {} queued, {} collected, {} metadata verified, {} stored",
        rep.unique_hashes,
        rep.queued,
        rep.peers.hashes,
        rep.fetch.ok,
        rep.persist.stored
    );
    Ok(rep)
}

/// Wait before restarting the crawler after its `n`-th consecutive failure (30 s doubling,
/// capped at 15 min).
fn restart_delay(n: u32) -> Duration {
    Duration::from_secs(30u64.saturating_mul(1 << n.min(5)).min(900))
}

/// `serve` daemon: runs while `MAGNETAR_DHT` ≠ off; errors are logged, never fatal to `serve`.
/// A start that fails (no bootstrap router resolved yet, a UDP port still taken, a database
/// error) is retried with a backoff instead of leaving the crawler off until the next restart.
pub async fn dht_loop(pool: PgPool, cfg: Arc<Config>, mut stop: watch::Receiver<bool>) {
    let Some(opts) = DhtOptions::from_config(&cfg, cfg.dht) else {
        return;
    };
    let mut failures = 0u32;
    loop {
        let started = Instant::now();
        match run(Some(pool.clone()), opts.clone(), stop.clone(), None, |_| {}).await {
            Ok(_) => return, // stopped on request
            Err(e) => {
                if started.elapsed() > Duration::from_secs(3600) {
                    failures = 0;
                }
                let wait = restart_delay(failures);
                failures = failures.saturating_add(1);
                tracing::error!(
                    "DHT crawler stopped: {e:#}; restarting in {} s",
                    wait.as_secs()
                );
                tokio::select! {
                    _ = tokio::time::sleep(wait) => {}
                    _ = stop_requested(&mut stop) => return,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{next_share, restart_delay};
    use std::time::Duration;

    #[test]
    fn restart_backs_off_to_a_cap() {
        let d: Vec<u64> = (0..8).map(|n| restart_delay(n).as_secs()).collect();
        assert_eq!(d, [30, 60, 120, 240, 480, 900, 900, 900]);
        assert_eq!(restart_delay(u32::MAX), Duration::from_secs(900));
    }

    #[test]
    fn backpressure_hysteresis() {
        assert_eq!(next_share(false, 9_999, 10_000, 5_000), None);
        assert_eq!(next_share(false, 10_000, 10_000, 5_000), Some(0.0));
        assert_eq!(
            next_share(true, 7_000, 10_000, 5_000),
            None,
            "stays paused in between"
        );
        assert_eq!(next_share(true, 5_000, 10_000, 5_000), Some(1.0));
        assert_eq!(next_share(false, 7_000, 10_000, 5_000), None);
    }
}

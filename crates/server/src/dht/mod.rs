//! DHT crawler daemon (docs/DHT.md §3, D49–D55): the `core::dht` node, triage of sighted
//! infohashes (seen filter → Postgres), the persisted queue (`dht_seen`), peer collection for the
//! metadata stage, identity/contact persistence and a JSON report.
//!
//! Stage 1 ends at peer collection: each queued hash gets its peers (announcer, the node that
//! sampled it, or an iterative lookup) and is handed to [`PeerSink`]. The BEP 9 fetcher (stage 2)
//! will consume those jobs; until then the sink only measures them, and the `dht_seen` lease
//! brings each hash back through the retry loop. A hash with no peer at all is marked `failed`
//! (`no_peers`) and backs off.

pub mod queue;
pub mod seen;
pub mod store;

use std::collections::HashSet;
use std::net::{SocketAddr, SocketAddrV4};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use magnetar_core::dht::{Dht, EngineConfig, EngineStats, Mode, Sighting, Via, collect_peers};
use magnetar_core::liveness::dht::BOOTSTRAP;
use serde::Serialize;
use sqlx::PgPool;
use tokio::sync::{Notify, mpsc, watch};
use tokio::task::JoinSet;
use tokio::time::Instant;

use crate::config::{Config, DhtMode};
use crate::daemons::stop_requested;
use queue::{FetchItem, FetchQueue};
use seen::SeenFilter;

const BATCH_MAX: usize = 1000;
const BATCH_WAIT: Duration = Duration::from_secs(1);
const RETRY_EVERY: Duration = Duration::from_secs(60);
const PERSIST_EVERY: Duration = Duration::from_secs(600);
/// Budget of one iterative peer lookup (docs/DHT.md §4 step 1).
const LOOKUP_BUDGET: Duration = Duration::from_secs(10);
const WANT_PEERS: usize = 16;
const PEER_SAMPLE_CAP: usize = 100_000;

#[derive(Clone, Debug)]
pub struct DhtOptions {
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
                .unwrap_or(if aggressive { 16 } else { 4 }),
            dry_run: false,
            bloom_bits: seen::DEFAULT_BITS,
            queue_cap: queue::DEFAULT_CAP,
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
}

fn add(a: &AtomicU64, n: u64) {
    a.fetch_add(n, Ordering::Relaxed);
}

fn get(a: &AtomicU64) -> u64 {
    a.load(Ordering::Relaxed)
}

/// A hash with its peers, ready for the metadata fetcher (stage 2).
#[derive(Clone, Debug)]
pub struct FetchJob {
    pub info_hash: [u8; 20],
    pub via: Via,
    pub peers: Vec<SocketAddrV4>,
}

/// Where collected jobs go. Stage 1: measured, then dropped (the `dht_seen` lease remains).
#[derive(Default)]
pub struct PeerSink {
    counts: Mutex<Vec<u32>>,
}

impl PeerSink {
    fn accept(&self, job: FetchJob) {
        let mut c = self.counts.lock().unwrap();
        if c.len() < PEER_SAMPLE_CAP {
            c.push(job.peers.len() as u32);
        }
    }
}

struct Pipeline {
    pool: Option<PgPool>,
    opts: DhtOptions,
    queue: Mutex<FetchQueue>,
    notify: Notify,
    c: Counters,
    sink: PeerSink,
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
    pub peers: PeerStats,
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
    let rest = &s[s.rfind(')')? + 2..];
    let f: Vec<&str> = rest.split_whitespace().collect();
    let ut: f64 = f.get(11)?.parse().ok()?;
    let st: f64 = f.get(12)?.parse().ok()?;
    Some((ut + st) / 100.0)
}

fn peer_stats(p: &Pipeline) -> PeerStats {
    let mut v = p.sink.counts.lock().unwrap().clone();
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
        peers: peer_stats(p),
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

async fn collector(p: Arc<Pipeline>, dht: Dht, mut stop: watch::Receiver<bool>) {
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
        p.sink.accept(FetchJob {
            info_hash: item.info_hash,
            via: item.via,
            peers: c.peers,
        });
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

async fn persist_loop(pool: PgPool, dht: Dht, ports: Vec<u16>, mut stop: watch::Receiver<bool>) {
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
    let p = Arc::new(Pipeline {
        pool: db.clone(),
        queue: Mutex::new(FetchQueue::new(opts.queue_cap)),
        notify: Notify::new(),
        c: Counters::default(),
        sink: PeerSink::default(),
        opts,
    });
    let (int_tx, int_rx) = watch::channel(false);
    let mut tasks = JoinSet::new();
    tasks.spawn(triage_loop(p.clone(), rx, int_rx.clone()));
    for _ in 0..p.opts.peer_lookups.max(1) {
        tasks.spawn(collector(p.clone(), dht.clone(), int_rx.clone()));
    }
    if let Some(pool) = &db {
        tasks.spawn(retry_loop(p.clone(), pool.clone(), int_rx.clone()));
        tasks.spawn(persist_loop(
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
    if let Some(pool) = &db {
        persist(pool, &dht, &p.opts.ports).await;
    }
    dht.shutdown().await;
    tracing::info!(
        "DHT crawler stopped: {} unique hashes, {} queued, {} collected",
        rep.unique_hashes,
        rep.queued,
        rep.peers.hashes
    );
    Ok(rep)
}

/// `serve` daemon: runs while `MAGNETAR_DHT` ≠ off; errors are logged, never fatal to `serve`.
pub async fn dht_loop(pool: PgPool, cfg: Arc<Config>, stop: watch::Receiver<bool>) {
    let Some(opts) = DhtOptions::from_config(&cfg, cfg.dht) else {
        return;
    };
    if let Err(e) = run(Some(pool), opts, stop, None, |_| {}).await {
        tracing::error!("DHT crawler stopped: {e:#}");
    }
}

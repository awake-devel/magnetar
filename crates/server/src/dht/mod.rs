//! DHT crawler daemon (docs/DHT.md §3, D49–D55): the `core::dht` node, triage of sighted
//! infohashes (seen filter → Postgres), the persisted queue (`dht_seen`), peer collection for the
//! metadata stage, identity/contact persistence and a JSON report.
//!
//! Each queued hash gets its peers (announcer, the node that sampled it, or an iterative
//! lookup); a hash with no peer at all is marked `failed` (`no_peers`) and backs off. Hashes with
//! peers go to the BEP 9 fetchers (`core::metainfo`, stage 2), whose results are written in
//! batches by [`persist`]: verified torrents pass the content [`screen`] first, and without one
//! nothing named is stored (fail closed until the stage 3 blocklist exists).

pub mod classify;
pub mod health;
pub mod persist;
pub mod queue;
pub mod screen;
pub mod seen;
pub mod status;
pub mod store;
pub mod upnp;

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
use persist::{FetchResult, PersistOutcome, ReclassifyOutcome};
use queue::{FetchItem, FetchQueue};
use screen::Screen;
use seen::SeenFilter;

const BATCH_MAX: usize = 1000;
const BATCH_WAIT: Duration = Duration::from_secs(1);
const RETRY_EVERY: Duration = Duration::from_secs(60);
const PERSIST_EVERY: Duration = Duration::from_secs(600);
/// Budget of one iterative peer lookup (docs/DHT.md §4 step 1).
const LOOKUP_BUDGET: Duration = Duration::from_secs(10);
/// Default peers tried per hash by the fetcher, and where a lookup stops (D56, D93).
pub const PEERS_PER_HASH: usize = 24;
/// Status writes from the collectors (D93): flushed every 500 rows or 1 s; the channel lets a
/// collector run this far ahead of a slow database before it waits.
const STATUS_BATCH_MAX: usize = 500;
const STATUS_BATCH_WAIT: Duration = Duration::from_secs(1);
const STATUS_CHANNEL: usize = 65_536;
const PROGRESS_EVERY: Duration = Duration::from_secs(60);
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
    /// Soft per-query timeout of lookups (`MAGNETAR_DHT_LOOKUP_TIMEOUT_MS`, D93).
    pub lookup_timeout: Duration,
    /// Peers tried per hash, and where a lookup stops (`MAGNETAR_DHT_PEERS_PER_HASH`).
    pub peers_per_hash: usize,
    /// No database: dedupe in memory only, nothing read or written.
    pub dry_run: bool,
    pub bloom_bits: u64,
    pub queue_cap: usize,
    /// Router port mappings for the UDP ports (D57, D72); None = no UPnP.
    pub upnp: Option<upnp::UpnpConfig>,
    /// Live state for the API (`serve`); None for the bounded `dht` command.
    pub service: Option<Arc<status::DhtService>>,
    /// Crawl map counters (D101): None = no counting by country / network.
    pub geo: Option<Arc<crate::crawlmap::CrawlMap>>,
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
            .clamp(1, magnetar_core::dht::engine::MAX_VIRTUAL_NODES as u8);
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
            lookup_timeout: Duration::from_millis(u64::from(cfg.dht_lookup_timeout_ms)),
            peers_per_hash: cfg.dht_peers_per_hash as usize,
            dry_run: false,
            bloom_bits: seen::DEFAULT_BITS,
            queue_cap: queue::DEFAULT_CAP,
            fetch_concurrency: cfg
                .dht_fetch_concurrency
                .map(|n| n as usize)
                .unwrap_or(if aggressive { 128 } else { 16 }),
            metadata_max: cfg.dht_metadata_max as usize,
            // The process-wide blocklist (installed at start); None stores nothing.
            screen: screen::global_screen(),
            tcp_rate: cfg.dht_tcp_rate,
            tcp_max: cfg.dht_tcp_max as usize,
            probe: health::parse_targets(&cfg.dht_probe),
            // Polite mode uses an ephemeral port: nothing to map. `serve` maps by default in
            // aggressive mode; the bounded `dht` command clears this unless asked (D72).
            upnp: (aggressive && cfg.dht_upnp.unwrap_or(true)).then(|| {
                upnp::UpnpConfig::from_settings(
                    cfg.dht_upnp_gateway.as_deref(),
                    cfg.dht_upnp_local_ip,
                )
            }),
            service: None,
            geo: None,
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
    /// D93: seen announces that moved a queued hash to the announce lane, gave their peer to a
    /// running collection, or went to Postgres triage again; fetches given an announcer.
    announce_upgraded: AtomicU64,
    announce_attached: AtomicU64,
    announce_retriaged: AtomicU64,
    fetch_announce: AtomicU64,
    status_errors: AtomicU64,
    triage_flushes: AtomicU64,
    triage_dropped: AtomicU64,
    triage_ms: AtomicU64,
    triage_max_ms: AtomicU64,
    /// Unique sightings per route: sample, get_peers, announce (D102).
    by_route: [AtomicU64; 3],
}

fn add(a: &AtomicU64, n: u64) {
    a.fetch_add(n, Ordering::Relaxed);
}

fn get(a: &AtomicU64) -> u64 {
    a.load(Ordering::Relaxed)
}

/// A per-hash status write handed to the writer task (D93).
#[derive(Clone, Debug)]
pub enum StatusOp {
    /// `store::mark_failed` (`error` code, unix time).
    Failed([u8; 20], &'static str, i64),
    /// `store::set_via_announce`.
    ViaAnnounce([u8; 20]),
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
    /// D93: `get_peers` lookups (the engine's count, walks dropped early included), their mean
    /// wall time, queries per lookup, share of their queries that went slow, slow ones that
    /// answered late.
    pub lookups: u64,
    pub lookup_mean_ms: f64,
    pub queries_per_lookup: f64,
    pub lookup_slow_share: f64,
    pub lookup_late: u64,
    /// Seen announces: upgraded a queued hash, joined a running collection, re-triaged.
    pub announce_upgraded: u64,
    pub announce_attached: u64,
    pub announce_retriaged: u64,
    /// Fetches handed a peer that announced the hash to us.
    pub announce_fetches: u64,
    pub status_write_errors: u64,
    /// D95: share of lookup queries answered, share of seeds that answered within 15 min,
    /// `get_peers` queries sent per torrent stored, lookup candidates skipped as dead.
    pub lookup_answered_share: f64,
    pub lookup_seeds_fresh_share: f64,
    pub queries_per_stored: f64,
    pub lookup_dead_skipped: u64,
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
    /// Postgres triage statements, their mean and longest time (D93).
    pub triage_flushes: u64,
    pub triage_mean_ms: f64,
    pub triage_max_ms: u64,
    /// Sightings dropped because the triage backlog was full.
    pub triage_dropped: u64,
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
    /// Rows of an older classifier version re-classified (and re-screened) at start (D69).
    pub reclassified: ReclassifyOutcome,
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

fn peer_stats(p: &Pipeline, e: &EngineStats) -> PeerStats {
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
        lookups: e.lookups,
        lookup_mean_ms: e.lookup_mean_ms(),
        queries_per_lookup: e.queries_per_lookup(),
        lookup_slow_share: if e.lookup_queries > 0 {
            e.lookup_slow as f64 / e.lookup_queries as f64
        } else {
            0.0
        },
        lookup_late: e.lookup_late,
        announce_upgraded: get(&p.c.announce_upgraded),
        announce_attached: get(&p.c.announce_attached),
        announce_retriaged: get(&p.c.announce_retriaged),
        announce_fetches: get(&p.c.fetch_announce),
        status_write_errors: get(&p.c.status_errors),
        lookup_answered_share: e.lookup_answered_share(),
        lookup_seeds_fresh_share: e.lookup_seeds_fresh_share(),
        queries_per_stored: {
            let stored = p.persisted.lock().unwrap().stored;
            if stored > 0 {
                e.sent_get_peers as f64 / stored as f64
            } else {
                0.0
            }
        },
        lookup_dead_skipped: e.lookup_dead_skipped,
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
        triage_flushes: get(&p.c.triage_flushes),
        triage_mean_ms: if get(&p.c.triage_flushes) > 0 {
            get(&p.c.triage_ms) as f64 / get(&p.c.triage_flushes) as f64
        } else {
            0.0
        },
        triage_max_ms: get(&p.c.triage_max_ms),
        triage_dropped: get(&p.c.triage_dropped),
        queue_len: q.len(),
        queue_lanes: q.lane_lens(),
        queue_overflow: q.overflow,
        sampling_paused_secs: get(&p.c.paused_ms) as f64 / 1000.0,
        peers: peer_stats(p, &engine),
        metadata_per_min: verified as f64 / mins,
        fetch_success_rate: if finished > 0 {
            verified as f64 / finished as f64
        } else {
            0.0
        },
        fetch,
        persist: p.persisted.lock().unwrap().clone(),
        reclassified: ReclassifyOutcome::default(),
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

async fn flush(p: &Pipeline, batch: &mut Vec<Sighting>, status: &mpsc::Sender<StatusOp>) {
    if batch.is_empty() {
        return;
    }
    let items = std::mem::take(batch);
    add(&p.c.triaged, items.len() as u64);
    let queued: HashSet<[u8; 20]> = match &p.pool {
        Some(pool) if !p.opts.dry_run => {
            let b: Vec<([u8; 20], Via)> = items.iter().map(|s| (s.info_hash, s.via)).collect();
            // D101: the telling node → country / network, counted and kept as codes only.
            let geo = match &p.opts.geo {
                Some(g) => g.dht_hashes(items.iter().map(|s| s.from), true),
                None => Vec::new(),
            };
            match store::triage_geo(pool, &b, &geo, magnetar_core::now(), p.opts.fetch_known).await
            {
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
        } else if let (Via::Announce, Some(peer), false) =
            (s.via, s.peer, queued.contains(&s.info_hash))
        {
            // Its get_peers sighting may have been in the previous batch (triaged just before
            // this one): that queued the hash, so the announce joins it now.
            let a = p
                .queue
                .lock()
                .unwrap()
                .attach_announce(s.info_hash, peer, now);
            if a != queue::Attach::No {
                add(&p.c.announce_upgraded, 1);
                if p.pool.is_some() {
                    let _ = status.send(StatusOp::ViaAnnounce(s.info_hash)).await;
                }
            }
        }
    }
}

/// Sightings kept while the triage task is behind (one batch then grows past `BATCH_MAX`).
const TRIAGE_BACKLOG_MAX: usize = 100_000;

/// Hand a batch to the triage writer without waiting (D93). The loop used to await each
/// Postgres triage statement (up to 28 s measured on a busy disk) while the engine's
/// 10 000-sighting channel overflowed (15 k–153 k sightings lost per 10 min). When the writer
/// is 16 batches behind, the batch stays here and keeps growing; past
/// [`TRIAGE_BACKLOG_MAX`] new sightings are dropped and counted.
/// Returns false when the batch stayed here.
fn hand(batches: &mpsc::Sender<Vec<Sighting>>, batch: &mut Vec<Sighting>) -> bool {
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
}

/// Runs the Postgres triage of each batch, one at a time, until the loop is gone.
async fn triage_writer(
    p: Arc<Pipeline>,
    mut rx: mpsc::Receiver<Vec<Sighting>>,
    status: mpsc::Sender<StatusOp>,
) {
    while let Some(mut b) = rx.recv().await {
        let t = Instant::now();
        flush(&p, &mut b, &status).await;
        let ms = t.elapsed().as_millis() as u64;
        add(&p.c.triage_flushes, 1);
        add(&p.c.triage_ms, ms);
        p.c.triage_max_ms.fetch_max(ms, Ordering::Relaxed);
    }
}

async fn triage_loop(
    p: Arc<Pipeline>,
    mut rx: mpsc::Receiver<Sighting>,
    status: mpsc::Sender<StatusOp>,
    batches: mpsc::Sender<Vec<Sighting>>,
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
                    if let Some(g) = &p.opts.geo {
                        g.dht_node(s.from, magnetar_core::now());
                    }
                    if batch.len() >= TRIAGE_BACKLOG_MAX {
                        add(&p.c.triage_dropped, 1);
                    } else if seen.check_and_insert(&s.info_hash, Instant::now()) {
                        add(&p.c.unique, 1);
                        add(&p.c.by_route[crate::fleet::ingest::route_index(s.via)], 1);
                        if batch.is_empty() {
                            flush_at = Instant::now() + BATCH_WAIT;
                        }
                        batch.push(s);
                        if batch.len() >= BATCH_MAX {
                            let _ = hand(&batches, &mut batch);
                        }
                    } else if let (Via::Announce, Some(peer)) = (s.via, s.peer) {
                        // D93: an announce carries a live peer; the seen filter must not drop
                        // it (the announcer's own get_peers usually came first).
                        let a = p.queue.lock().unwrap().attach_announce(s.info_hash, peer, Instant::now());
                        match a {
                            queue::Attach::Queued | queue::Attach::Collecting => {
                                add(if a == queue::Attach::Queued {
                                    &p.c.announce_upgraded
                                } else {
                                    &p.c.announce_attached
                                }, 1);
                                if p.pool.is_some() {
                                    let _ = status.send(StatusOp::ViaAnnounce(s.info_hash)).await;
                                }
                            }
                            queue::Attach::No => {
                                // Postgres decides: a failed row > 10 min old is retried.
                                add(&p.c.announce_retriaged, 1);
                                if batch.is_empty() {
                                    flush_at = Instant::now() + BATCH_WAIT;
                                }
                                batch.push(s);
                                if batch.len() >= BATCH_MAX {
                                    let _ = hand(&batches, &mut batch);
                                }
                            }
                        }
                    } else {
                        add(&p.c.bloom_skipped, 1);
                    }
                }
                None => break,
            },
            _ = tokio::time::sleep_until(flush_at), if !batch.is_empty() => {
                if !hand(&batches, &mut batch) {
                    flush_at = Instant::now() + Duration::from_millis(100);
                }
            }
            _ = stop_requested(&mut stop) => break,
        }
    }
    // Hashes of a partial batch still reach dht_seen, so the next run does not lose them.
    if !batch.is_empty() {
        let _ = batches.send(batch).await;
    }
}

async fn collector(
    p: Arc<Pipeline>,
    dht: Dht,
    jobs: mpsc::Sender<FetchJob>,
    status: mpsc::Sender<StatusOp>,
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
        let mut c = c;
        let mut via = item.via;
        // A peer that announced the hash while we collected goes first (D93).
        if let Some(peer) = p.queue.lock().unwrap().collected(&item.info_hash)
            && dht.allows(&peer)
        {
            c.peers.retain(|x| *x != peer);
            c.peers.insert(0, peer);
            via = Via::Announce;
        }
        add(&p.c.collected, 1);
        add(&p.c.peers_total, c.peers.len() as u64);
        add(&p.c.lookup_queries, u64::from(c.queries));
        if !c.peers.is_empty() {
            add(&p.c.with_peers, 1);
        } else if p.pool.is_some() {
            // No swarm found: back off (1 h, 6 h, 24 h, then never) instead of holding the
            // lease. Written by the status writer in batches: the collector does not wait.
            let op = StatusOp::Failed(item.info_hash, "no_peers", magnetar_core::now());
            if status.send(op).await.is_err() {
                return;
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
        if via == Via::Announce {
            add(&p.c.fetch_announce, 1);
        }
        let job = FetchJob {
            info_hash: item.info_hash,
            via,
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
            if let Some(g) = &p.opts.geo {
                g.add_stored(&o.stored_geo);
            }
            let mut t = p.persisted.lock().unwrap();
            t.stored += o.stored;
            t.not_new += o.not_new;
            t.alias += o.alias;
            t.blocked += o.blocked;
            for (k, v) in o.blocked_by_tier {
                *t.blocked_by_tier.entry(k).or_default() += v;
            }
            for (k, v) in o.categories {
                *t.categories.entry(k).or_default() += v;
            }
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
                // Only the error class: a database message could quote a bound value, and
                // these statements carry torrent names (D53: names never reach the logs).
                tracing::warn!("DHT persist: {}", persist::error_class(&e));
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

async fn status_flush(p: &Pipeline, pool: &PgPool, batch: &mut Vec<StatusOp>) {
    if batch.is_empty() {
        return;
    }
    let mut failed = Vec::new();
    let mut via = Vec::new();
    for op in batch.drain(..) {
        match op {
            StatusOp::Failed(h, e, at) => failed.push((h, e.to_string(), at)),
            StatusOp::ViaAnnounce(h) => via.push(h),
        }
    }
    // A row keeps its lease when a write fails: the retry loop brings it back (as before).
    match store::mark_failed_batch(pool, &failed).await {
        Ok(_) => add(&p.c.marked_failed, failed.len() as u64),
        Err(e) => {
            if get(&p.c.status_errors).is_multiple_of(100) {
                tracing::warn!("DHT: marking fetches failed: {}", persist::error_class(&e));
            }
            add(&p.c.status_errors, 1);
        }
    }
    if let Err(e) = store::set_via_announce(pool, &via).await {
        if get(&p.c.status_errors).is_multiple_of(100) {
            tracing::warn!("DHT: recording announces: {}", persist::error_class(&e));
        }
        add(&p.c.status_errors, 1);
    }
}

/// Writes the collectors' per-hash outcomes in batches (≤ 500 or 1 s, D93). Runs until every
/// sender is gone, so outcomes from before a stop are still written.
async fn status_writer(p: Arc<Pipeline>, mut rx: mpsc::Receiver<StatusOp>) {
    let mut batch = Vec::new();
    let mut flush_at = Instant::now() + STATUS_BATCH_WAIT;
    loop {
        tokio::select! {
            r = rx.recv() => match r {
                Some(op) => {
                    let Some(pool) = &p.pool else { continue };
                    if batch.is_empty() {
                        flush_at = Instant::now() + STATUS_BATCH_WAIT;
                    }
                    batch.push(op);
                    if batch.len() >= STATUS_BATCH_MAX {
                        status_flush(&p, pool, &mut batch).await;
                    }
                }
                None => break,
            },
            _ = tokio::time::sleep_until(flush_at), if !batch.is_empty() => {
                if let Some(pool) = &p.pool {
                    status_flush(&p, pool, &mut batch).await;
                }
            }
        }
    }
    if let Some(pool) = &p.pool {
        status_flush(&p, pool, &mut batch).await;
    }
}

/// One log line a minute for the bounded `dht` command: rates over that minute (counts only).
async fn progress_loop(p: Arc<Pipeline>, dht: Dht, mut stop: watch::Receiver<bool>) {
    let mut last = (dht.stats(), p.fetcher.stats(), get(&p.c.collected));
    let share = |a: u64, b: u64| if b > 0 { a as f64 / b as f64 } else { 0.0 };
    loop {
        tokio::select! {
            _ = tokio::time::sleep(PROGRESS_EVERY) => {}
            _ = stop_requested(&mut stop) => return,
        }
        let e = dht.stats();
        let f = p.fetcher.stats();
        let col = get(&p.c.collected);
        let (e0, f0, c0) = &last;
        let d = |a: u64, b: u64| a.saturating_sub(b);
        let a = &e.by_node_age;
        let a0 = &e0.by_node_age;
        let mins = PROGRESS_EVERY.as_secs_f64() / 60.0;
        tracing::info!(
            "DHT minute: {:.0} q/s, answered {:.3} (young {:.3}, old {:.3}, unpooled {:.3}, repeat {:.3}), collected {:.0}/min, lookups {:.0}/min at {:.0} ms and {:.1} queries (answered {:.3}, fresh seeds {:.3}), metadata {:.0}/min, fake skipped {}",
            d(e.queries_sent, e0.queries_sent) as f64 / PROGRESS_EVERY.as_secs_f64(),
            share(
                d(e.answered, e0.answered),
                d(e.queries_sent, e0.queries_sent)
            ),
            share(
                d(a.young_answered, a0.young_answered),
                d(a.young_sent, a0.young_sent)
            ),
            share(
                d(a.old_answered, a0.old_answered),
                d(a.old_sent, a0.old_sent)
            ),
            share(
                d(a.unpooled_answered, a0.unpooled_answered),
                d(a.unpooled_sent, a0.unpooled_sent)
            ),
            share(
                d(a.repeat_answered, a0.repeat_answered),
                d(a.repeat_sent, a0.repeat_sent)
            ),
            d(col, *c0) as f64 / mins,
            d(e.lookups, e0.lookups) as f64 / mins,
            share(
                d(e.lookup_ms_total, e0.lookup_ms_total),
                d(e.lookups, e0.lookups)
            ),
            share(
                d(e.lookup_queries, e0.lookup_queries),
                d(e.lookups, e0.lookups)
            ),
            share(
                d(e.lookup_answered, e0.lookup_answered),
                d(e.lookup_queries, e0.lookup_queries)
            ),
            share(
                d(e.lookup_seeds_fresh, e0.lookup_seeds_fresh),
                d(
                    e.lookup_seeds_fresh + e.lookup_seeds_unknown + e.lookup_seeds_struck,
                    e0.lookup_seeds_fresh + e0.lookup_seeds_unknown + e0.lookup_seeds_struck
                )
            ),
            d(
                f.ok + f.private + f.v2_only + f.invalid,
                f0.ok + f0.private + f0.v2_only + f0.invalid
            ) as f64
                / mins,
            d(f.fake_skipped, f0.fake_skipped),
        );
        last = (e, f, col);
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
    cfg.lookup_timeout = opts.lookup_timeout;
    if let Some(pool) = &db {
        cfg.known_nodes = store::load_nodes(pool).await?;
        cfg.identity = store::load_identity(pool, &opts.ports).await?;
    }
    let reclassified = match (&db, opts.screen.as_deref()) {
        (Some(pool), Some(screen)) => reclassify_all(pool, screen).await?,
        _ => ReclassifyOutcome::default(),
    };
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
        max_peers: opts.peers_per_hash.max(1),
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
    let (status_tx, status_rx) = mpsc::channel::<StatusOp>(STATUS_CHANNEL);
    let status_task = tokio::spawn(status_writer(p.clone(), status_rx));
    let (batch_tx, batch_rx) = mpsc::channel::<Vec<Sighting>>(16);
    let triage_task = tokio::spawn(triage_writer(p.clone(), batch_rx, status_tx.clone()));
    tasks.spawn(triage_loop(
        p.clone(),
        rx,
        status_tx.clone(),
        batch_tx,
        int_rx.clone(),
    ));
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
            status_tx.clone(),
            int_rx.clone(),
        ));
    }
    drop(jobs_tx);
    drop(status_tx);
    if p.opts.service.is_none() {
        tasks.spawn(progress_loop(p.clone(), dht.clone(), int_rx.clone()));
    }
    tasks.spawn(health::health_loop(
        p.opts.probe.clone(),
        dht.clone(),
        p.fetcher.clone(),
        p.health.clone(),
        int_rx.clone(),
    ));
    tasks.spawn(backpressure(p.clone(), dht.clone(), int_rx.clone()));
    if let Some(svc) = &p.opts.service {
        svc.attach(p.clone(), dht.clone(), started);
        tasks.spawn(status::sampler(svc.clone(), db.clone(), int_rx.clone()));
    }
    // UPnP outlives the task grace below: it removes its mappings after the stop.
    let mapper = p.opts.upnp.clone().map(|cfg| {
        let wanted: Vec<(upnp::Protocol, u16)> = dht
            .local_ports()
            .into_iter()
            .filter(|port| *port != 0)
            .map(|port| (upnp::Protocol::Udp, port))
            .collect();
        let status = p
            .opts
            .service
            .as_ref()
            .map(|s| s.upnp.clone())
            .unwrap_or_default();
        tokio::spawn(upnp::upnp_loop(cfg, wanted, status, int_rx.clone()))
    });
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
    // The last partial triage batch still reaches dht_seen (the next run does not lose it).
    let _ = tokio::time::timeout(Duration::from_secs(3), triage_task).await;
    let _ = tokio::time::timeout(Duration::from_secs(3), status_task).await;
    if let Some(m) = mapper {
        // ≤ 2 s per mapping removal; the stop already reached it.
        let _ = tokio::time::timeout(Duration::from_secs(3), m).await;
    }
    if let Some(svc) = &p.opts.service {
        svc.detach();
    }
    let mut rep = rep;
    rep.persist = p.persisted.lock().unwrap().clone();
    if rep.persist.stored > 0 {
        rep.peers.queries_per_stored = rep.engine.sent_get_peers as f64 / rep.persist.stored as f64;
    }
    rep.persist_errors = get(&p.c.persist_errors);
    rep.marked_no_peers = get(&p.c.marked_failed);
    rep.reclassified = reclassified;
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

/// Re-classify every row of an older classifier version, 500 per transaction (D69).
pub async fn reclassify_all(
    pool: &PgPool,
    screen: &dyn Screen,
) -> anyhow::Result<ReclassifyOutcome> {
    let mut total = ReclassifyOutcome::default();
    let mut after = String::new();
    loop {
        let o = persist::reclassify_batch(pool, screen, &after, 500, magnetar_core::now())
            .await
            .map_err(|e| anyhow::anyhow!("DHT reclassify: {}", persist::error_class(&e)))?;
        total.rows += o.rows;
        total.changed += o.changed;
        total.blocked += o.blocked;
        match o.last {
            Some(last) if o.rows == 500 => after = last,
            _ => break,
        }
    }
    if total.rows > 0 {
        tracing::info!(
            "DHT: re-classified {} rows ({} with a category, {} blocked)",
            total.rows,
            total.changed,
            total.blocked
        );
    }
    Ok(total)
}

/// Wait before restarting the crawler after its `n`-th consecutive failure (30 s doubling,
/// capped at 15 min).
fn restart_delay(n: u32) -> Duration {
    Duration::from_secs(30u64.saturating_mul(1 << n.min(5)).min(900))
}

/// `serve` daemon: runs while `MAGNETAR_DHT` ≠ off; errors are logged, never fatal to `serve`.
/// A start that fails (no bootstrap router resolved yet, a UDP port still taken, a database
/// error) is retried with a backoff instead of leaving the crawler off until the next restart.
pub async fn dht_loop(
    pool: PgPool,
    cfg: Arc<Config>,
    svc: Arc<status::DhtService>,
    geo: Option<Arc<crate::crawlmap::CrawlMap>>,
    mut stop: watch::Receiver<bool>,
) {
    let Some(mut opts) = DhtOptions::from_config(&cfg, cfg.dht) else {
        return;
    };
    opts.service = Some(svc.clone());
    opts.geo = geo;
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
                svc.set_error(&format!("{e:#}"));
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
    use super::{Sighting, Via, hand, next_share, restart_delay};
    use std::time::Duration;
    use tokio::sync::mpsc;

    /// D93: the triage loop never waits for Postgres; a batch the busy writer cannot take yet
    /// stays (and grows) instead of blocking the loop.
    #[test]
    fn hand_off_never_blocks() {
        let s = |b: u8| Sighting {
            info_hash: [b; 20],
            via: Via::Sample,
            source: None,
            peer: None,
            from: None,
        };
        let (tx, mut rx) = mpsc::channel(1);
        let mut batch = vec![s(1)];
        assert!(hand(&tx, &mut batch));
        assert!(batch.is_empty());
        batch.push(s(2));
        assert!(!hand(&tx, &mut batch), "writer behind");
        batch.push(s(3));
        assert_eq!(batch.len(), 2, "kept, growing");
        assert_eq!(rx.try_recv().unwrap().len(), 1);
        assert!(hand(&tx, &mut batch));
        assert_eq!(rx.try_recv().unwrap().len(), 2);
        assert!(hand(&tx, &mut Vec::new()), "nothing to hand");
    }

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

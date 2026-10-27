//! The crawler node: one UDP socket per virtual node, transactions, the BEP 5/51 responder
//! (aggressive), the BEP 51 sampler and maintenance (docs/DHT.md §3.2–§3.6, D54, D55).
//!
//! Concurrency: one receive task per socket, one sampler task, one maintenance task; queries are
//! futures that register a transaction keyed by (virtual node, tid, remote address) and wait for
//! the matching answer for up to [`QUERY_TIMEOUT`]. Shared state lives behind one `std` mutex that
//! is never held across an `.await`.

use std::collections::{HashMap, HashSet};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rand::Rng;
use serde::Serialize;
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::{JoinHandle, JoinSet};
use tokio::time::Instant;

use super::addr::AddrFilter;
use super::id::{NodeId, bep42_id, common_prefix_bits, random_id_in_bucket, spread_r_values};
use super::krpc::{
    self, Body, CompactNode, ERR_METHOD_UNKNOWN, ERR_PROTOCOL, Id, MAX_DATAGRAM, Message, Query,
    Response,
};
use super::limiter::{Brake, Bucket, IP_BUCKETS_CAP, Pace, PerIp};
use super::peerstore::{self, PeerStore};
use super::pool::{Pool, PoolCounts, Tier};
use super::routing::{K, RoutingTable, Seen};

pub const QUERY_TIMEOUT: Duration = Duration::from_secs(4);
/// Lookup queries count as slow after this (D93): the lookup stops waiting for them and asks the
/// next candidate, but still takes a late answer until [`QUERY_TIMEOUT`]. Default 4 s = no widening:
/// at production load 1.5 s halved the torrents stored per minute (docs/DHT.md §18.5).
pub const LOOKUP_TIMEOUT: Duration = Duration::from_millis(4000);
/// `v` = "MG" (unregistered in BEP 20, checked 2026-10-26) + 2 version bytes.
pub const VERSION: [u8; 4] = [b'M', b'G', 0, 2];
/// Ids sharing this many leading bits with a target were made for it (D45's node-farm rule).
pub const FARM_PREFIX_BITS: u32 = 64;
const SECRET_ROTATION: Duration = Duration::from_secs(300);
const SAMPLE_INTERVAL_SERVED: i64 = 3600;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// BEP 51 sampling only, every query `ro=1`, incoming queries ignored (BEP 43).
    Polite,
    /// Sampling and passive capture as a full BEP 5 node.
    Aggressive,
}

/// External IP and the BEP 42 ids derived from it, persisted between runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Identity {
    pub external_ip: Ipv4Addr,
    pub ids: Vec<NodeId>,
}

#[derive(Clone, Debug)]
pub struct EngineConfig {
    pub mode: Mode,
    pub bind: Ipv4Addr,
    /// One per virtual node; 0 = ephemeral.
    pub ports: Vec<u16>,
    pub qps: u32,
    pub pool_cap: usize,
    pub bootstrap: Vec<SocketAddrV4>,
    /// Nodes saved by an earlier run (BEP 5: "the routing table should be saved").
    pub known_nodes: Vec<CompactNode>,
    pub identity: Option<Identity>,
    /// Test seam: lets 127.0.0.0/8 through the address filter. Never set from configuration.
    pub allow_loopback: bool,
    pub query_timeout: Duration,
    /// Soft per-query timeout of iterative lookups (≤ `query_timeout`, D93).
    pub lookup_timeout: Duration,
    pub sighting_capacity: usize,
    /// Run the BEP 51 sampler (tests may drive queries by hand).
    pub sample: bool,
    pub per_ip_qps: f64,
    pub response_rate: f64,
    pub response_per_ip: f64,
    /// External IP vote: agreeing responders and distinct /16s needed.
    pub vote_responders: usize,
    pub vote_prefixes: usize,
}

impl EngineConfig {
    pub fn new(mode: Mode) -> Self {
        let (ports, qps, pool) = match mode {
            Mode::Polite => (vec![0], 50, 200_000),
            Mode::Aggressive => (vec![6891, 6892, 6893, 6894], 200, 1_000_000),
        };
        Self {
            mode,
            bind: Ipv4Addr::UNSPECIFIED,
            ports,
            qps,
            pool_cap: pool,
            bootstrap: Vec::new(),
            known_nodes: Vec::new(),
            identity: None,
            allow_loopback: false,
            query_timeout: QUERY_TIMEOUT,
            lookup_timeout: LOOKUP_TIMEOUT,
            sighting_capacity: 10_000,
            sample: true,
            per_ip_qps: 1.0,
            response_rate: 300.0,
            response_per_ip: 5.0,
            vote_responders: 8,
            vote_prefixes: 4,
        }
    }
}

/// How an infohash was seen.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Via {
    Sample,
    Announce,
    GetPeers,
}

impl Via {
    /// `dht_seen.via` / `dht_torrents.discovered_via` codes (docs/DHT.md §2.3).
    pub fn code(self) -> i16 {
        match self {
            Via::Sample => 0,
            Via::Announce => 1,
            Via::GetPeers => 2,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Via::Sample => "sample",
            Via::Announce => "announce",
            Via::GetPeers => "get_peers",
        }
    }
}

/// An infohash seen on the DHT.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sighting {
    pub info_hash: Id,
    pub via: Via,
    /// The node whose sample held it (it stores peers for exactly that hash).
    pub source: Option<SocketAddrV4>,
    /// The announcing peer (a peer that has the torrent).
    pub peer: Option<SocketAddrV4>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QueryError {
    Filtered,
    RateLimited,
    Busy,
    Timeout,
    Remote(i64),
    Io,
    Stopped,
}

enum Reply {
    Response(Response),
    Error(i64),
}

#[derive(Default)]
struct Counters {
    sent: [AtomicU64; 5],
    answered: AtomicU64,
    errors: AtomicU64,
    timeouts: AtomicU64,
    unmatched: AtomicU64,
    decode_errors: AtomicU64,
    too_long: AtomicU64,
    filtered: AtomicU64,
    rate_limited: AtomicU64,
    busy: AtomicU64,
    incoming: [AtomicU64; 6],
    incoming_ignored: AtomicU64,
    responses_sent: AtomicU64,
    responses_dropped: AtomicU64,
    bad_tokens: AtomicU64,
    samples: AtomicU64,
    sightings: [AtomicU64; 3],
    sightings_dropped: AtomicU64,
    nodes_learnt: AtomicU64,
    nodes_filtered: AtomicU64,
    nodes_farm: AtomicU64,
    ip_changes: AtomicU64,
    udp_bytes_in: AtomicU64,
    udp_bytes_out: AtomicU64,
    /// `get_peers` lookups (D93): count, wall time, queries sent, queries that went slow and
    /// slow queries answered late.
    lookups: AtomicU64,
    lookup_ms: AtomicU64,
    lookup_queries: AtomicU64,
    lookup_slow: AtomicU64,
    lookup_late: AtomicU64,
    /// Queries sent / answered by destination: not in the pool, in the pool < 5 min, older
    /// (index [`AGE_UNPOOLED`] …); and to pool nodes that answered a sample before.
    age_sent: [AtomicU64; 3],
    age_answered: [AtomicU64; 3],
    repeat_sent: AtomicU64,
    repeat_answered: AtomicU64,
    /// D95: lookup queries answered, lookup seeds by tier (fresh, unknown, struck) and
    /// candidates a walk skipped as dead.
    lookup_answered: AtomicU64,
    lookup_seeds: [AtomicU64; 3],
    lookup_dead_skipped: AtomicU64,
}

/// Lookup seeds are ranked among this many closest known contacts per seed wanted (D95):
/// 4 × 16 = 64 pool candidates, about two prefix bits further out than the 16 closest.
pub const SEED_SPREAD: usize = 4;

/// Pool nodes younger than this count as "young" in [`NodeAgeStats`].
pub const YOUNG_NODE: Duration = Duration::from_secs(300);
const AGE_UNPOOLED: usize = 0;
const AGE_YOUNG: usize = 1;
const AGE_OLD: usize = 2;

fn bump(a: &AtomicU64) {
    a.fetch_add(1, Ordering::Relaxed);
}

fn query_kind(q: &Query) -> usize {
    match q {
        Query::Ping => 0,
        Query::FindNode { .. } => 1,
        Query::GetPeers { .. } => 2,
        Query::AnnouncePeer { .. } => 3,
        Query::SampleInfohashes { .. } => 4,
        Query::Unknown(_) => 5,
    }
}

/// Counts and gauges for reports and `/api/v1/stats` (never addresses or names).
#[derive(Clone, Debug, Default, Serialize)]
pub struct EngineStats {
    pub queries_sent: u64,
    pub sent_ping: u64,
    pub sent_find_node: u64,
    pub sent_get_peers: u64,
    pub sent_sample_infohashes: u64,
    pub answered: u64,
    pub errors: u64,
    pub timeouts: u64,
    pub unmatched: u64,
    pub decode_errors: u64,
    pub too_long: u64,
    pub filtered: u64,
    pub rate_limited: u64,
    pub busy: u64,
    pub incoming_ping: u64,
    pub incoming_find_node: u64,
    pub incoming_get_peers: u64,
    pub incoming_announce_peer: u64,
    pub incoming_sample_infohashes: u64,
    pub incoming_unknown: u64,
    pub incoming_ignored: u64,
    pub responses_sent: u64,
    pub responses_dropped: u64,
    pub bad_tokens: u64,
    pub samples_received: u64,
    pub sightings_sample: u64,
    pub sightings_announce: u64,
    pub sightings_get_peers: u64,
    pub sightings_dropped: u64,
    pub nodes_learnt: u64,
    pub nodes_filtered: u64,
    pub nodes_farm: u64,
    pub external_ip_known: bool,
    pub external_ip_changes: u64,
    pub brake_active: bool,
    /// Sampler's share of the QPS budget (D62).
    pub sample_share: f64,
    pub brake_engaged: u64,
    pub pending: usize,
    pub routing: Vec<usize>,
    pub pool: PoolStats,
    pub peer_store_hashes: usize,
    pub peer_store_peers: usize,
    /// UDP payload bytes received / sent by every virtual node (bandwidth, IP/UDP headers not
    /// included).
    pub udp_bytes_in: u64,
    pub udp_bytes_out: u64,
    /// Iterative `get_peers` lookups finished (or cut short), their total wall time in ms, the
    /// queries they sent, how many of those went slow (no answer within the lookup timeout)
    /// and how many slow ones still answered (D93).
    pub lookups: u64,
    pub lookup_ms_total: u64,
    pub lookup_queries: u64,
    pub lookup_slow: u64,
    pub lookup_late: u64,
    /// Answer share by how long the queried node has been known (rate limiting or a pool that
    /// wears out shows as old/repeat shares falling while young ones hold).
    pub by_node_age: NodeAgeStats,
    /// D95: lookup queries answered (of `lookup_queries`), lookup seeds by tier (answered
    /// within 15 min / not known to be silent / silent since their last answer), and walk
    /// candidates skipped because they never answered [`super::pool::DEAD_AFTER`] queries.
    pub lookup_answered: u64,
    pub lookup_seeds_fresh: u64,
    pub lookup_seeds_unknown: u64,
    pub lookup_seeds_struck: u64,
    pub lookup_dead_skipped: u64,
}

/// Queries sent and answered by destination (answers = responses, not error replies).
#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
pub struct NodeAgeStats {
    /// Not in the crawl pool (bootstrap routers, nodes only seen in lookups).
    pub unpooled_sent: u64,
    pub unpooled_answered: u64,
    /// In the pool for less than [`YOUNG_NODE`].
    pub young_sent: u64,
    pub young_answered: u64,
    pub old_sent: u64,
    pub old_answered: u64,
    /// Pool nodes that answered a `sample_infohashes` before (the same nodes, asked again).
    pub repeat_sent: u64,
    pub repeat_answered: u64,
}

impl EngineStats {
    /// Mean wall time of a `get_peers` lookup, ms.
    pub fn lookup_mean_ms(&self) -> f64 {
        ratio(self.lookup_ms_total, self.lookups)
    }

    /// Queries sent per `get_peers` lookup.
    pub fn queries_per_lookup(&self) -> f64 {
        ratio(self.lookup_queries, self.lookups)
    }

    /// Share of `get_peers` lookup queries answered (late answers included, D95).
    pub fn lookup_answered_share(&self) -> f64 {
        ratio(self.lookup_answered, self.lookup_queries)
    }

    /// Share of lookup seeds that answered within the last 15 minutes (D95).
    pub fn lookup_seeds_fresh_share(&self) -> f64 {
        ratio(
            self.lookup_seeds_fresh,
            self.lookup_seeds_fresh + self.lookup_seeds_unknown + self.lookup_seeds_struck,
        )
    }
}

fn ratio(a: u64, b: u64) -> f64 {
    if b > 0 { a as f64 / b as f64 } else { 0.0 }
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct PoolStats {
    pub size: usize,
    pub bep51_yes: usize,
    pub bep51_no: usize,
    pub bep51_unknown: usize,
    pub evicted: u64,
    pub dropped_silent: u64,
    /// D95: dropped after never answering, and such addresses remembered now.
    pub dropped_dead: u64,
    pub dead_known: usize,
}

impl From<PoolCounts> for PoolStats {
    fn from(c: PoolCounts) -> Self {
        Self {
            size: c.size,
            bep51_yes: c.bep51_yes,
            bep51_no: c.bep51_no,
            bep51_unknown: c.bep51_unknown,
            evicted: c.evicted,
            dropped_silent: c.dropped_silent,
            dropped_dead: c.dropped_dead,
            dead_known: c.dead_known,
        }
    }
}

struct VNode {
    routing: RoutingTable,
    ro: bool,
}

#[derive(Default)]
struct Vote {
    responders: HashSet<Ipv4Addr>,
    prefixes: HashSet<u16>,
}

/// Candidates tracked at once by the external-address vote.
const VOTE_CANDIDATES: usize = 64;
/// Responders counted per candidate.
const VOTE_RESPONDERS_CAP: usize = 64;
/// The vote restarts this often, so stale or junk `ip` claims never add up over a long run.
const VOTE_WINDOW: Duration = Duration::from_secs(600);

/// BEP 42 external address vote over the `ip` field of answers (attacker-controlled).
///
/// A candidate wins with ≥ `need_responders` distinct responders from ≥ `need_prefixes` /16s
/// **and**, once an address is known, more responders than that address got in the same window
/// (a real change stops the votes for the old address). A full table evicts the weakest other
/// candidate, so junk claims cannot lock a real change out; [`Votes::clear`] restarts the window.
#[derive(Default)]
struct Votes {
    by_ip: HashMap<Ipv4Addr, Vote>,
}

impl Votes {
    /// Record that `responder` saw us as `seen_as`; returns the address to switch to, if any.
    fn record(
        &mut self,
        seen_as: Ipv4Addr,
        responder: Ipv4Addr,
        current: Option<Ipv4Addr>,
        need_responders: usize,
        need_prefixes: usize,
    ) -> Option<Ipv4Addr> {
        if !self.by_ip.contains_key(&seen_as) && self.by_ip.len() >= VOTE_CANDIDATES {
            let weakest = self
                .by_ip
                .iter()
                .filter(|(ip, _)| Some(**ip) != current)
                .min_by_key(|(_, v)| v.responders.len())
                .map(|(ip, _)| *ip)?;
            self.by_ip.remove(&weakest);
        }
        let v = self.by_ip.entry(seen_as).or_default();
        if v.responders.len() < VOTE_RESPONDERS_CAP && v.responders.insert(responder) {
            let o = responder.octets();
            v.prefixes.insert(u16::from_be_bytes([o[0], o[1]]));
        }
        if Some(seen_as) == current {
            return None;
        }
        let n = v.responders.len();
        let incumbent = current
            .and_then(|ip| self.by_ip.get(&ip))
            .map_or(0, |c| c.responders.len());
        let v = &self.by_ip[&seen_as];
        (n >= need_responders && v.prefixes.len() >= need_prefixes && n > incumbent)
            .then_some(seen_as)
    }

    fn clear(&mut self) {
        self.by_ip.clear();
    }
}

struct State {
    nodes: Vec<VNode>,
    pool: Pool,
    peers: PeerStore,
    pending: HashMap<PendingKey, oneshot::Sender<Reply>>,
    pending_cap: usize,
    pace: Pace,
    per_ip: PerIp,
    brake: Brake,
    resp_global: Bucket,
    resp_ip: PerIp,
    filter: AddrFilter,
    external_ip: Option<Ipv4Addr>,
    votes: Votes,
    votes_since: Instant,
    secrets: [[u8; 16]; 2],
    rr: usize,
    /// Share of the QPS budget the sampler may use, 0–1 (D62: the server lowers it while the
    /// fetch queue is backlogged, so discovery does not outrun what peer collection consumes).
    sample_share: f64,
    sample_next: Instant,
}

struct Inner {
    cfg: EngineConfig,
    sockets: Vec<Arc<UdpSocket>>,
    local_ports: Vec<u16>,
    state: Mutex<State>,
    c: Counters,
    sightings: mpsc::Sender<Sighting>,
    stop: watch::Receiver<bool>,
}

/// Handle to a running crawler node (cheap to clone).
#[derive(Clone)]
pub struct Dht {
    inner: Arc<Inner>,
    stop_tx: Arc<watch::Sender<bool>>,
    tasks: Arc<Mutex<Vec<JoinHandle<()>>>>,
}

fn token_for(secret: &[u8; 16], ip: Ipv4Addr) -> [u8; 8] {
    let mut h = sha1_smol::Sha1::new();
    h.update(secret);
    h.update(&ip.octets());
    let d = h.digest().bytes();
    std::array::from_fn(|i| d[i])
}

fn ids_for(ip: Ipv4Addr, n: usize) -> Vec<NodeId> {
    let mut rng = rand::rng();
    spread_r_values(ip, n)
        .into_iter()
        .map(|r| {
            let rand_byte = (rng.random::<u8>() & 0xf8) | r;
            bep42_id(ip, rand_byte, rng.random())
        })
        .collect()
}

type PendingKey = (usize, [u8; 2], SocketAddrV4);

/// Owns a query's receiver; on drop removes the transaction unless it already belongs to a newer
/// query with the same key (its sender is then still open).
///
/// A query dropped while it waits for its answer (a lookup that ended) counts as a silence
/// for the pool (D95) once it waited the lookup timeout: with the lookup timeout equal to the
/// hard one (the default), a walk often ends on the soft timer just before the hard one fires,
/// and those silences would otherwise never be recorded.
struct PendingGuard<'a> {
    dht: &'a Dht,
    key: PendingKey,
    rx: Option<oneshot::Receiver<Reply>>,
    /// Set once the datagram is sent, cleared when an answer or the hard timeout is handled.
    waiting_since: Option<Instant>,
}

impl Drop for PendingGuard<'_> {
    fn drop(&mut self) {
        drop(self.rx.take());
        let mut s = self.dht.inner.state.lock().unwrap();
        if s.pending.get(&self.key).is_some_and(|tx| tx.is_closed()) {
            s.pending.remove(&self.key);
        }
        if self
            .waiting_since
            .is_some_and(|t| t.elapsed() >= self.dht.lookup_timeout())
        {
            s.pool.silent(&self.key.2);
        }
    }
}

impl Dht {
    /// Bind the sockets and start the node. Returns the handle and the stream of sightings.
    pub async fn start(mut cfg: EngineConfig) -> std::io::Result<(Dht, mpsc::Receiver<Sighting>)> {
        // At least one socket (an empty port list means one ephemeral port): every virtual node
        // owns a socket, indexed like `nodes`.
        if cfg.ports.is_empty() {
            cfg.ports.push(0);
        }
        let n = cfg.ports.len().min(8);
        let mut sockets = Vec::with_capacity(n);
        let mut local_ports = Vec::with_capacity(n);
        for &port in cfg.ports.iter().take(n) {
            let s = UdpSocket::bind(SocketAddrV4::new(cfg.bind, port)).await?;
            local_ports.push(s.local_addr()?.port());
            sockets.push(Arc::new(s));
        }
        let now = Instant::now();
        let (ids, external_ip) = match &cfg.identity {
            Some(id) if id.ids.len() >= n => (id.ids[..n].to_vec(), Some(id.external_ip)),
            Some(id) => (ids_for(id.external_ip, n), Some(id.external_ip)),
            None => ((0..n).map(|_| rand::random()).collect(), None),
        };
        let ro = |known: bool| cfg.mode == Mode::Polite || !known;
        let mut filter = AddrFilter::new(cfg.allow_loopback);
        if let Some(ip) = external_ip
            && !ip.is_loopback()
        {
            filter.set_own(Some(ip));
        }
        let qps = cfg.qps.max(1);
        let state = State {
            nodes: ids
                .iter()
                .map(|id| VNode {
                    routing: RoutingTable::new(*id, now),
                    ro: ro(external_ip.is_some()),
                })
                .collect(),
            pool: Pool::new(cfg.pool_cap),
            peers: PeerStore::new(peerstore::CAP),
            pending: HashMap::new(),
            pending_cap: (qps as usize * cfg.query_timeout.as_secs().max(1) as usize * 2).max(64),
            pace: Pace::new(qps, now),
            per_ip: PerIp::new(
                cfg.per_ip_qps,
                (cfg.per_ip_qps * 2.0).max(2.0),
                IP_BUCKETS_CAP,
            ),
            brake: Brake::default(),
            resp_global: Bucket::new(cfg.response_rate, cfg.response_rate, now),
            resp_ip: PerIp::new(cfg.response_per_ip, cfg.response_per_ip, IP_BUCKETS_CAP),
            filter,
            external_ip,
            votes: Votes::default(),
            votes_since: now,
            secrets: [rand::random(), rand::random()],
            rr: 0,
            sample_share: 1.0,
            sample_next: now,
        };
        let (tx, rx) = mpsc::channel(cfg.sighting_capacity.max(1));
        let (stop_tx, stop) = watch::channel(false);
        let inner = Arc::new(Inner {
            cfg,
            sockets,
            local_ports,
            state: Mutex::new(state),
            c: Counters::default(),
            sightings: tx,
            stop,
        });
        let dht = Dht {
            inner,
            stop_tx: Arc::new(stop_tx),
            tasks: Arc::new(Mutex::new(Vec::new())),
        };
        let mut tasks = Vec::new();
        for i in 0..dht.inner.sockets.len() {
            let d = dht.clone();
            tasks.push(tokio::spawn(async move { d.recv_loop(i).await }));
        }
        let d = dht.clone();
        tasks.push(tokio::spawn(async move { d.maintenance_loop().await }));
        if dht.inner.cfg.sample {
            let d = dht.clone();
            tasks.push(tokio::spawn(async move { d.sampler_loop().await }));
        }
        *dht.tasks.lock().unwrap() = tasks;
        Ok((dht, rx))
    }

    /// Stop every task and wait for them (bounded).
    pub async fn shutdown(&self) {
        let _ = self.stop_tx.send(true);
        let tasks: Vec<_> = std::mem::take(&mut *self.tasks.lock().unwrap());
        for t in tasks {
            let abort = t.abort_handle();
            if tokio::time::timeout(Duration::from_secs(5), t)
                .await
                .is_err()
            {
                abort.abort();
            }
        }
    }

    pub fn is_stopped(&self) -> bool {
        *self.inner.stop.borrow()
    }

    pub fn local_ports(&self) -> Vec<u16> {
        self.inner.local_ports.clone()
    }

    pub fn virtual_nodes(&self) -> usize {
        self.inner.sockets.len()
    }

    pub fn mode(&self) -> Mode {
        self.inner.cfg.mode
    }

    pub fn ids(&self) -> Vec<NodeId> {
        let s = self.inner.state.lock().unwrap();
        s.nodes.iter().map(|n| n.routing.own_id()).collect()
    }

    /// The voted (or persisted) external IP and current ids.
    pub fn identity(&self) -> Option<Identity> {
        let s = self.inner.state.lock().unwrap();
        s.external_ip.map(|ip| Identity {
            external_ip: ip,
            ids: s.nodes.iter().map(|n| n.routing.own_id()).collect(),
        })
    }

    /// Good routing contacts and recently answering pool nodes, for the next start.
    pub fn nodes_to_save(&self, n: usize) -> Vec<CompactNode> {
        let s = self.inner.state.lock().unwrap();
        let now = Instant::now();
        let mut out: Vec<CompactNode> = Vec::new();
        let mut seen = HashSet::new();
        for v in &s.nodes {
            for c in v.routing.good(now, n) {
                if seen.insert(c.addr) {
                    out.push(c);
                }
            }
        }
        for c in s.pool.answered_recently(now, Duration::from_secs(3600), n) {
            if out.len() >= n {
                break;
            }
            if seen.insert(c.addr) {
                out.push(c);
            }
        }
        out.truncate(n);
        out
    }

    /// The `k` closest routing contacts over every virtual node.
    pub fn closest(&self, target: &Id, k: usize) -> Vec<CompactNode> {
        let s = self.inner.state.lock().unwrap();
        let mut all: Vec<CompactNode> = s
            .nodes
            .iter()
            .flat_map(|v| v.routing.closest(target, k))
            .collect();
        all.sort_by_key(|c| super::id::distance(&c.id, target));
        all.dedup_by_key(|c| c.addr);
        all.truncate(k);
        all
    }

    /// Lookup seeds: the `k` closest contacts of the routing tables **and** the crawl pool
    /// (D61), nearest first, one per address.
    pub fn closest_known(&self, target: &Id, k: usize) -> Vec<CompactNode> {
        let s = self.inner.state.lock().unwrap();
        let mut all: Vec<CompactNode> = s
            .nodes
            .iter()
            .flat_map(|v| v.routing.closest(target, k))
            .chain(s.pool.closest(target, k))
            .collect();
        drop(s);
        all.sort_by_key(|c| (super::id::distance(&c.id, target), c.addr));
        let mut seen = std::collections::HashSet::new();
        all.retain(|c| seen.insert(c.addr));
        all.truncate(k);
        all
    }

    /// Lookup seeds (D95): the [`SEED_SPREAD`] × `k` closest contacts of the crawl pool and the
    /// `k` closest of each routing table, ranked by [`super::lookup::rank_seeds`] (nodes that
    /// answered recently first, then untried ones, then silent ones; dead ones never), with
    /// their tiers. The pool's tier wins for a contact both know (it sees every query).
    pub fn lookup_seeds(&self, target: &Id, k: usize) -> Vec<super::lookup::SeedCand> {
        use super::lookup::SeedCand;
        let now = Instant::now();
        let s = self.inner.state.lock().unwrap();
        let mut all: Vec<SeedCand> = s
            .pool
            .closest_ranked(target, k * SEED_SPREAD, now)
            .into_iter()
            .map(|(node, tier)| SeedCand {
                node,
                tier,
                limited: false,
            })
            .collect();
        for v in &s.nodes {
            for (node, t) in v.routing.closest_ranked(target, k, now) {
                all.push(SeedCand {
                    node,
                    tier: s.pool.tier_of(&node.addr, now).unwrap_or(t),
                    limited: false,
                });
            }
        }
        for c in &mut all {
            c.limited = !s.per_ip.available(c.node.addr.ip(), now);
        }
        drop(s);
        let out = super::lookup::rank_seeds(all, target, k);
        for c in &out {
            match c.tier {
                Tier::Fresh => bump(&self.inner.c.lookup_seeds[0]),
                Tier::Unknown => bump(&self.inner.c.lookup_seeds[1]),
                Tier::Struck => bump(&self.inner.c.lookup_seeds[2]),
                Tier::Dead => {}
            }
        }
        out
    }

    /// Whether our per-IP limit would refuse a query to each address right now (D95).
    pub fn limited(&self, addrs: &[SocketAddrV4]) -> Vec<bool> {
        let now = Instant::now();
        let s = self.inner.state.lock().unwrap();
        addrs
            .iter()
            .map(|a| !s.per_ip.available(a.ip(), now))
            .collect()
    }

    /// The pool's tier of each address (unknown to the pool → [`Tier::Unknown`]), one lock.
    pub fn tiers_of(&self, addrs: &[SocketAddrV4]) -> Vec<Tier> {
        let now = Instant::now();
        let s = self.inner.state.lock().unwrap();
        addrs
            .iter()
            .map(|a| s.pool.tier_of(a, now).unwrap_or(Tier::Unknown))
            .collect()
    }

    /// Halve the KRPC query rate until `until` (router health probe, D66). Extends an active
    /// brake, never shortens it.
    pub fn brake_until(&self, until: Instant) {
        self.inner.state.lock().unwrap().brake.engage_until(until);
    }

    /// Set the sampler's share of the QPS budget (clamped to 0–1; 0 pauses sampling, the
    /// rest of the node keeps running).
    pub fn set_sample_share(&self, share: f64) {
        let mut s = self.inner.state.lock().unwrap();
        let share = if share.is_nan() {
            1.0
        } else {
            share.clamp(0.0, 1.0)
        };
        if share != s.sample_share {
            s.sample_share = share;
            s.sample_next = Instant::now();
        }
    }

    pub fn sample_share(&self) -> f64 {
        self.inner.state.lock().unwrap().sample_share
    }

    pub fn bootstrap_addrs(&self) -> &[SocketAddrV4] {
        &self.inner.cfg.bootstrap
    }

    /// Round-robin virtual node for the next query.
    pub fn next_node(&self) -> usize {
        let mut s = self.inner.state.lock().unwrap();
        s.rr = s.rr.wrapping_add(1);
        s.rr % s.nodes.len()
    }

    pub fn stats(&self) -> EngineStats {
        let c = &self.inner.c;
        let g = |a: &AtomicU64| a.load(Ordering::Relaxed);
        let s = self.inner.state.lock().unwrap();
        let now = Instant::now();
        EngineStats {
            queries_sent: c.sent.iter().map(g).sum(),
            sent_ping: g(&c.sent[0]),
            sent_find_node: g(&c.sent[1]),
            sent_get_peers: g(&c.sent[2]),
            sent_sample_infohashes: g(&c.sent[4]),
            answered: g(&c.answered),
            errors: g(&c.errors),
            timeouts: g(&c.timeouts),
            unmatched: g(&c.unmatched),
            decode_errors: g(&c.decode_errors),
            too_long: g(&c.too_long),
            filtered: g(&c.filtered),
            rate_limited: g(&c.rate_limited),
            busy: g(&c.busy),
            incoming_ping: g(&c.incoming[0]),
            incoming_find_node: g(&c.incoming[1]),
            incoming_get_peers: g(&c.incoming[2]),
            incoming_announce_peer: g(&c.incoming[3]),
            incoming_sample_infohashes: g(&c.incoming[4]),
            incoming_unknown: g(&c.incoming[5]),
            incoming_ignored: g(&c.incoming_ignored),
            responses_sent: g(&c.responses_sent),
            responses_dropped: g(&c.responses_dropped),
            bad_tokens: g(&c.bad_tokens),
            samples_received: g(&c.samples),
            sightings_sample: g(&c.sightings[0]),
            sightings_announce: g(&c.sightings[1]),
            sightings_get_peers: g(&c.sightings[2]),
            sightings_dropped: g(&c.sightings_dropped),
            nodes_learnt: g(&c.nodes_learnt),
            nodes_filtered: g(&c.nodes_filtered),
            nodes_farm: g(&c.nodes_farm),
            external_ip_known: s.external_ip.is_some(),
            external_ip_changes: g(&c.ip_changes),
            brake_active: s.brake.active(now),
            sample_share: s.sample_share,
            brake_engaged: s.brake.engaged,
            pending: s.pending.len(),
            routing: s.nodes.iter().map(|n| n.routing.len()).collect(),
            pool: s.pool.counts().into(),
            peer_store_hashes: s.peers.hashes(),
            peer_store_peers: s.peers.peers(),
            udp_bytes_in: g(&c.udp_bytes_in),
            udp_bytes_out: g(&c.udp_bytes_out),
            lookups: g(&c.lookups),
            lookup_ms_total: g(&c.lookup_ms),
            lookup_queries: g(&c.lookup_queries),
            lookup_slow: g(&c.lookup_slow),
            lookup_late: g(&c.lookup_late),
            by_node_age: NodeAgeStats {
                unpooled_sent: g(&c.age_sent[AGE_UNPOOLED]),
                unpooled_answered: g(&c.age_answered[AGE_UNPOOLED]),
                young_sent: g(&c.age_sent[AGE_YOUNG]),
                young_answered: g(&c.age_answered[AGE_YOUNG]),
                old_sent: g(&c.age_sent[AGE_OLD]),
                old_answered: g(&c.age_answered[AGE_OLD]),
                repeat_sent: g(&c.repeat_sent),
                repeat_answered: g(&c.repeat_answered),
            },
            lookup_answered: g(&c.lookup_answered),
            lookup_seeds_fresh: g(&c.lookup_seeds[0]),
            lookup_seeds_unknown: g(&c.lookup_seeds[1]),
            lookup_seeds_struck: g(&c.lookup_seeds[2]),
            lookup_dead_skipped: g(&c.lookup_dead_skipped),
        }
    }

    /// Soft per-query timeout of lookups (clamped to the hard query timeout).
    pub fn lookup_timeout(&self) -> Duration {
        let c = &self.inner.cfg;
        c.lookup_timeout
            .clamp(Duration::from_millis(1), c.query_timeout)
    }

    /// Lookup accounting (D93), see [`EngineStats::lookups`].
    pub(crate) fn note_lookup(&self, elapsed: Duration) {
        bump(&self.inner.c.lookups);
        self.inner
            .c
            .lookup_ms
            .fetch_add(elapsed.as_millis() as u64, Ordering::Relaxed);
    }

    pub(crate) fn note_lookup_query(&self) {
        bump(&self.inner.c.lookup_queries);
    }

    pub(crate) fn note_lookup_slow(&self) {
        bump(&self.inner.c.lookup_slow);
    }

    pub(crate) fn note_lookup_late(&self) {
        bump(&self.inner.c.lookup_late);
    }

    pub(crate) fn note_lookup_answered(&self) {
        bump(&self.inner.c.lookup_answered);
    }

    pub(crate) fn note_dead_skipped(&self, n: u64) {
        if n > 0 {
            self.inner
                .c
                .lookup_dead_skipped
                .fetch_add(n, Ordering::Relaxed);
        }
    }

    /// Keep only the addresses we may hand the fetcher (one lock for the whole list).
    pub fn retain_allowed(&self, v: &mut Vec<SocketAddrV4>) {
        let s = self.inner.state.lock().unwrap();
        v.retain(|a| {
            s.filter.allows(a)
                && !(a.ip().is_loopback() && self.inner.local_ports.contains(&a.port()))
        });
    }

    fn sight(&self, s: Sighting) {
        bump(&self.inner.c.sightings[s.via.code() as usize]);
        if self.inner.sightings.try_send(s).is_err() {
            bump(&self.inner.c.sightings_dropped);
        }
    }

    /// Send one query and wait for its answer. Paced by the global QPS budget.
    pub async fn query(
        &self,
        node: usize,
        to: SocketAddrV4,
        q: Query,
    ) -> Result<Response, QueryError> {
        self.query_inner(node, to, q, true, || {}).await
    }

    /// [`query`](Self::query), calling `on_sent` once the datagram is on the wire (after the
    /// pace wait): lookups start their soft timeout there (D93).
    pub async fn query_then(
        &self,
        node: usize,
        to: SocketAddrV4,
        q: Query,
        on_sent: impl FnOnce(),
    ) -> Result<Response, QueryError> {
        self.query_inner(node, to, q, true, on_sent).await
    }

    async fn query_inner(
        &self,
        node: usize,
        to: SocketAddrV4,
        q: Query,
        pace: bool,
        on_sent: impl FnOnce(),
    ) -> Result<Response, QueryError> {
        let c = &self.inner.c;
        if self.is_stopped() {
            return Err(QueryError::Stopped);
        }
        let kind = query_kind(&q);
        let (node, at, age, repeat) = {
            let mut s = self.inner.state.lock().unwrap();
            let node = node % s.nodes.len();
            let now = Instant::now();
            if !s.filter.allows(&to)
                || (to.ip().is_loopback() && self.inner.local_ports.contains(&to.port()))
            {
                bump(&c.filtered);
                return Err(QueryError::Filtered);
            }
            if !s.per_ip.try_take(*to.ip(), now) {
                bump(&c.rate_limited);
                return Err(QueryError::RateLimited);
            }
            if s.pending.len() >= s.pending_cap {
                bump(&c.busy);
                return Err(QueryError::Busy);
            }
            let (age, repeat) = match s.pool.get(&to) {
                None => (AGE_UNPOOLED, false),
                Some(e) => (
                    if now.saturating_duration_since(e.added) < YOUNG_NODE {
                        AGE_YOUNG
                    } else {
                        AGE_OLD
                    },
                    e.last_answer.is_some(),
                ),
            };
            let braked = s.brake.active(now);
            let at = if pace {
                s.pace.reserve(now, braked)
            } else {
                now
            };
            (node, at, age, repeat)
        };
        // Wait for the pace slot *before* the transaction exists (D93): the pending map then
        // holds only queries on the wire (≤ QPS × timeout), not the pace queue as well (which,
        // with many lookups, filled the cap and turned queries into `Busy`).
        if at > Instant::now() {
            let mut stop = self.inner.stop.clone();
            tokio::select! {
                _ = tokio::time::sleep_until(at) => {}
                _ = stop.wait_for(|s| *s) => return Err(QueryError::Stopped),
            }
        }
        let (key, rx, wire) = {
            let mut s = self.inner.state.lock().unwrap();
            if s.pending.len() >= s.pending_cap {
                bump(&c.busy);
                return Err(QueryError::Busy);
            }
            let mut tid: [u8; 2] = rand::random();
            while s.pending.contains_key(&(node, tid, to)) {
                tid = rand::random();
            }
            let (tx, rx) = oneshot::channel();
            s.pending.insert((node, tid, to), tx);
            let v = &s.nodes[node];
            let wire = krpc::encode(&Message {
                tid: tid.to_vec(),
                body: Body::Query {
                    id: v.routing.own_id(),
                    query: q,
                    ro: v.ro,
                },
                ip: None,
                version: Some(VERSION.to_vec()),
            });
            ((node, tid, to), rx, wire)
        };
        // Removes the transaction however this future ends, cancellation included (a lookup
        // that stops early drops its in-flight queries; without this they leaked until the
        // map's cap turned every later query into `Busy`).
        let mut guard = PendingGuard {
            dht: self,
            key,
            rx: Some(rx),
            waiting_since: None,
        };
        if self.inner.sockets[key.0]
            .send_to(&wire, SocketAddr::V4(to))
            .await
            .is_err()
        {
            return Err(QueryError::Io);
        }
        guard.waiting_since = Some(Instant::now());
        on_sent();
        bump(&c.sent[kind.min(4)]);
        c.udp_bytes_out
            .fetch_add(wire.len() as u64, Ordering::Relaxed);
        self.inner.state.lock().unwrap().brake.sent();
        bump(&c.age_sent[age]);
        if repeat {
            bump(&c.repeat_sent);
        }
        let rx = guard.rx.as_mut().expect("receiver until drop");
        let r = tokio::time::timeout(self.inner.cfg.query_timeout, rx).await;
        guard.waiting_since = None;
        if let Ok(Ok(_)) = &r {
            // Any answer, error replies included, shows the node is alive (D95).
            self.inner
                .state
                .lock()
                .unwrap()
                .pool
                .heard(&to, Instant::now());
        }
        match r {
            Ok(Ok(Reply::Response(r))) => {
                bump(&c.age_answered[age]);
                if repeat {
                    bump(&c.repeat_answered);
                }
                Ok(r)
            }
            Ok(Ok(Reply::Error(code))) => Err(QueryError::Remote(code)),
            Ok(Err(_)) => Err(QueryError::Stopped),
            Err(_) => {
                drop(guard);
                bump(&c.timeouts);
                let mut s = self.inner.state.lock().unwrap();
                s.nodes[key.0].routing.failed(&to);
                s.pool.silent(&to);
                Err(QueryError::Timeout)
            }
        }
    }

    /// Learnt contacts → the crawl pool, after the address filter and the node-farm rule.
    fn learn(&self, s: &mut State, nodes: &[CompactNode], target: &Id, now: Instant) {
        for n in nodes {
            if !s.filter.allows(&n.addr)
                || (n.addr.ip().is_loopback() && self.inner.local_ports.contains(&n.addr.port()))
            {
                bump(&self.inner.c.nodes_filtered);
            } else if common_prefix_bits(&n.id, target) >= FARM_PREFIX_BITS {
                bump(&self.inner.c.nodes_farm);
            } else if s.pool.add(*n, now) {
                bump(&self.inner.c.nodes_learnt);
            }
        }
    }

    /// Record contacts learnt from a lookup answer (pool only; routing takes nodes that answer).
    pub fn learn_nodes(&self, nodes: &[CompactNode], target: &Id) {
        let mut s = self.inner.state.lock().unwrap();
        self.learn(&mut s, nodes, target, Instant::now());
    }

    /// [`learn_nodes`](Self::learn_nodes), returning each contact's tier afterwards (D95), in
    /// the same lock.
    pub fn learn_ranked(&self, nodes: &[CompactNode], target: &Id) -> Vec<Tier> {
        let now = Instant::now();
        let mut s = self.inner.state.lock().unwrap();
        self.learn(&mut s, nodes, target, now);
        nodes
            .iter()
            .map(|n| s.pool.tier_of(&n.addr, now).unwrap_or(Tier::Unknown))
            .collect()
    }

    async fn recv_loop(&self, i: usize) {
        let sock = self.inner.sockets[i].clone();
        let mut stop = self.inner.stop.clone();
        let mut buf = vec![0u8; MAX_DATAGRAM + 1];
        loop {
            let (n, src) = tokio::select! {
                _ = stop.wait_for(|s| *s) => return,
                r = sock.recv_from(&mut buf) => match r {
                    Ok(x) => x,
                    // ICMP errors surface as recv errors on Linux; keep going.
                    Err(_) => continue,
                },
            };
            self.inner
                .c
                .udp_bytes_in
                .fetch_add(n as u64, Ordering::Relaxed);
            let SocketAddr::V4(src) = src else { continue };
            if n > MAX_DATAGRAM {
                bump(&self.inner.c.too_long);
                continue;
            }
            match krpc::decode(&buf[..n]) {
                Ok(msg) => self.handle(i, src, msg),
                Err(_) => bump(&self.inner.c.decode_errors),
            }
        }
    }

    fn handle(&self, i: usize, src: SocketAddrV4, msg: Message) {
        let c = &self.inner.c;
        let reply = match msg.body {
            Body::Query { id, query, ro } => return self.respond(i, src, msg.tid, id, query, ro),
            Body::Response(r) => Reply::Response(r),
            Body::Error { code, .. } => Reply::Error(code),
        };
        let Ok(tid) = <[u8; 2]>::try_from(msg.tid.as_slice()) else {
            bump(&c.unmatched);
            return;
        };
        let mut s = self.inner.state.lock().unwrap();
        let Some(tx) = s.pending.remove(&(i, tid, src)) else {
            bump(&c.unmatched);
            return;
        };
        s.brake.answered();
        match &reply {
            Reply::Response(r) => {
                bump(&c.answered);
                let now = Instant::now();
                s.nodes[i].routing.insert(
                    CompactNode {
                        id: r.id,
                        addr: src,
                    },
                    Seen::Answered,
                    now,
                );
                if let Some(ip) = msg.ip {
                    self.vote(&mut s, *ip.ip(), *src.ip(), now);
                }
            }
            Reply::Error(_) => bump(&c.errors),
        }
        drop(s);
        let _ = tx.send(reply);
    }

    /// Whether we may send to (or hand the fetcher) this address.
    pub fn allows(&self, a: &SocketAddrV4) -> bool {
        let s = self.inner.state.lock().unwrap();
        s.filter.allows(a) && !(a.ip().is_loopback() && self.inner.local_ports.contains(&a.port()))
    }

    /// BEP 42 external IP vote (see [`Votes`]).
    fn vote(&self, s: &mut State, seen_as: Ipv4Addr, responder: Ipv4Addr, now: Instant) {
        let usable = super::addr::is_global(seen_as)
            || (self.inner.cfg.allow_loopback && seen_as.is_loopback());
        if !usable {
            return;
        }
        if now.saturating_duration_since(s.votes_since) >= VOTE_WINDOW {
            s.votes.clear();
            s.votes_since = now;
        }
        let current = s.external_ip;
        if let Some(ip) = s.votes.record(
            seen_as,
            responder,
            current,
            self.inner.cfg.vote_responders,
            self.inner.cfg.vote_prefixes,
        ) {
            self.set_external(s, ip, now);
        }
    }

    fn set_external(&self, s: &mut State, ip: Ipv4Addr, now: Instant) {
        let ids = ids_for(ip, s.nodes.len());
        let polite = self.inner.cfg.mode == Mode::Polite;
        for (v, id) in s.nodes.iter_mut().zip(ids) {
            v.routing.rebuild(id, now);
            v.ro = polite;
        }
        if !ip.is_loopback() {
            s.filter.set_own(Some(ip));
        }
        s.external_ip = Some(ip);
        s.votes.clear();
        s.votes_since = now;
        bump(&self.inner.c.ip_changes);
        tracing::info!("DHT: external address voted, BEP 42 ids derived");
    }

    fn respond(&self, i: usize, src: SocketAddrV4, tid: Vec<u8>, id: Id, q: Query, ro: bool) {
        let c = &self.inner.c;
        bump(&c.incoming[query_kind(&q)]);
        if self.inner.cfg.mode == Mode::Polite {
            // BEP 43: a read-only node "no longer responds to 'query' messages".
            bump(&c.incoming_ignored);
            return;
        }
        let now = Instant::now();
        let mut sighting = None;
        let body = {
            let mut s = self.inner.state.lock().unwrap();
            if !s.filter.allows(&src) {
                bump(&c.incoming_ignored);
                return;
            }
            if !s.resp_global.try_take(now) || !s.resp_ip.try_take(*src.ip(), now) {
                bump(&c.responses_dropped);
                return;
            }
            let sender = CompactNode { id, addr: src };
            if !ro {
                s.nodes[i].routing.insert(sender, Seen::Queried, now);
                if s.pool.add(sender, now) {
                    bump(&c.nodes_learnt);
                }
            }
            let own = s.nodes[i].routing.own_id();
            let resp = |nodes: Vec<CompactNode>| Response {
                id: own,
                nodes,
                ..Default::default()
            };
            match q {
                Query::Ping => Body::Response(resp(Vec::new())),
                Query::FindNode { target } => {
                    Body::Response(resp(s.nodes[i].routing.closest(&target, K)))
                }
                Query::GetPeers { info_hash } => {
                    sighting = Some(Sighting {
                        info_hash,
                        via: Via::GetPeers,
                        source: None,
                        peer: None,
                    });
                    let mut r = resp(s.nodes[i].routing.closest(&info_hash, K));
                    r.token = Some(token_for(&s.secrets[0], *src.ip()).to_vec());
                    r.values = s.peers.get(&info_hash, now, krpc::MAX_VALUES);
                    Body::Response(r)
                }
                Query::AnnouncePeer {
                    info_hash,
                    port,
                    implied_port,
                    token,
                } => {
                    let valid = s
                        .secrets
                        .iter()
                        .any(|sec| token_for(sec, *src.ip()).as_slice() == token.as_slice());
                    let port = if implied_port { src.port() } else { port };
                    if !valid || port == 0 {
                        bump(&c.bad_tokens);
                        Body::Error {
                            code: ERR_PROTOCOL,
                            message: "bad token".into(),
                        }
                    } else {
                        let peer = SocketAddrV4::new(*src.ip(), port);
                        s.peers.announce(info_hash, peer, now);
                        sighting = Some(Sighting {
                            info_hash,
                            via: Via::Announce,
                            source: None,
                            peer: Some(peer),
                        });
                        Body::Response(resp(Vec::new()))
                    }
                }
                Query::SampleInfohashes { target } => {
                    let mut r = resp(s.nodes[i].routing.closest(&target, K));
                    r.samples = Some(s.peers.sample(krpc::MAX_SAMPLES));
                    r.interval = Some(SAMPLE_INTERVAL_SERVED);
                    r.num = Some(s.peers.hashes() as i64);
                    Body::Response(r)
                }
                Query::Unknown(_) => Body::Error {
                    code: ERR_METHOD_UNKNOWN,
                    message: "Method Unknown".into(),
                },
            }
        };
        let wire = krpc::encode(&Message {
            tid,
            body,
            ip: Some(src),
            version: Some(VERSION.to_vec()),
        });
        if self.inner.sockets[i]
            .try_send_to(&wire, SocketAddr::V4(src))
            .is_ok()
        {
            bump(&c.responses_sent);
            c.udp_bytes_out
                .fetch_add(wire.len() as u64, Ordering::Relaxed);
        }
        if let Some(sg) = sighting {
            self.sight(sg);
        }
    }

    async fn sampler_loop(&self) {
        let mut stop = self.inner.stop.clone();
        let mut inflight: JoinSet<()> = JoinSet::new();
        let qps = f64::from(self.inner.cfg.qps.max(1));
        loop {
            while inflight.try_join_next().is_some() {}
            // The sampler's own share of the budget (paused at 0), then a global pace slot.
            let own = {
                let mut s = self.inner.state.lock().unwrap();
                let now = Instant::now();
                if s.sample_share <= 0.0 {
                    None
                } else {
                    let at = s.sample_next.max(now);
                    if s.sample_share < 1.0 {
                        s.sample_next = at + Duration::from_secs_f64(1.0 / (qps * s.sample_share));
                    }
                    Some(at)
                }
            };
            let Some(own) = own else {
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_millis(250)) => {}
                    _ = stop.wait_for(|s| *s) => break,
                }
                continue;
            };
            if own > Instant::now() {
                tokio::select! {
                    _ = tokio::time::sleep_until(own) => {}
                    _ = stop.wait_for(|s| *s) => break,
                }
            }
            let at = {
                let mut s = self.inner.state.lock().unwrap();
                let now = Instant::now();
                let braked = s.brake.active(now);
                s.pace.reserve(now, braked)
            };
            tokio::select! {
                _ = tokio::time::sleep_until(at) => {}
                _ = stop.wait_for(|s| *s) => break,
            }
            let now = Instant::now();
            let (due, grow) = {
                let mut s = self.inner.state.lock().unwrap();
                match s.pool.pop_due(now) {
                    Some(e) => (Some(e), None),
                    None => {
                        let mut seeds = s.pool.random(1);
                        if seeds.is_empty() {
                            let t: Id = rand::random();
                            seeds = s
                                .nodes
                                .iter()
                                .flat_map(|v| v.routing.closest(&t, 1))
                                .collect();
                        }
                        (None, seeds.first().map(|n| n.addr))
                    }
                }
            };
            let node = self.next_node();
            if let Some(e) = due {
                let d = self.clone();
                inflight.spawn(async move { d.sample_one(node, e.node.addr).await });
            } else if let Some(addr) = grow {
                let d = self.clone();
                inflight.spawn(async move {
                    let target: Id = rand::random();
                    if let Ok(r) = d
                        .query_inner(node, addr, Query::FindNode { target }, false, || {})
                        .await
                    {
                        d.learn_nodes(&r.nodes, &target);
                    }
                });
            } else {
                // Nothing known yet: wait for the bootstrap.
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_millis(500)) => {}
                    _ = stop.wait_for(|s| *s) => break,
                }
            }
        }
        inflight.shutdown().await;
    }

    async fn sample_one(&self, node: usize, addr: SocketAddrV4) {
        let target: Id = rand::random();
        let r = self
            .query_inner(node, addr, Query::SampleInfohashes { target }, false, || {})
            .await;
        let now = Instant::now();
        let mut found = Vec::new();
        {
            let mut s = self.inner.state.lock().unwrap();
            match r {
                Ok(r) => {
                    s.pool
                        .answered(&addr, r.id, r.samples.is_some(), r.interval, now);
                    self.learn(&mut s, &r.nodes, &target, now);
                    found = r.samples.unwrap_or_default();
                }
                Err(QueryError::Remote(_)) => s.pool.unsupported(&addr, now),
                Err(QueryError::Timeout) => {
                    s.pool.failed(&addr, now);
                }
                Err(QueryError::Filtered) => s.pool.remove(&addr),
                Err(QueryError::Stopped) => {}
                Err(_) => s.pool.reschedule(&addr, now + Duration::from_secs(2)),
            }
        }
        self.inner
            .c
            .samples
            .fetch_add(found.len() as u64, Ordering::Relaxed);
        for h in found {
            self.sight(Sighting {
                info_hash: h,
                via: Via::Sample,
                source: Some(addr),
                peer: None,
            });
        }
    }

    async fn maintenance_loop(&self) {
        let mut stop = self.inner.stop.clone();
        let started = Instant::now();
        let mut last_minute = started;
        let mut last_rotation = started;
        // Bootstrap: find the nodes closest to each own id (BEP 5).
        let mut boot = JoinSet::new();
        for i in 0..self.inner.sockets.len() {
            let d = self.clone();
            boot.spawn(async move { d.self_lookup(i).await });
        }
        let mut tick = tokio::time::interval(Duration::from_secs(1));
        loop {
            tokio::select! {
                _ = tick.tick() => {}
                _ = stop.wait_for(|s| *s) => break,
            }
            while boot.try_join_next().is_some() {}
            let now = Instant::now();
            {
                let mut s = self.inner.state.lock().unwrap();
                if s.brake.tick(now) {
                    tracing::warn!(
                        "DHT: answered share collapsed, halving the query rate for 5 min"
                    );
                }
                if now.saturating_duration_since(last_rotation) >= SECRET_ROTATION {
                    s.secrets[1] = s.secrets[0];
                    s.secrets[0] = rand::random();
                    last_rotation = now;
                }
            }
            if now.saturating_duration_since(last_minute) < Duration::from_secs(60) {
                continue;
            }
            last_minute = now;
            let mut work: Vec<(usize, SocketAddrV4, Query)> = Vec::new();
            let mut thin = Vec::new();
            {
                let mut s = self.inner.state.lock().unwrap();
                s.peers.purge(now);
                for (i, v) in s.nodes.iter_mut().enumerate() {
                    if v.routing.len() < 16 {
                        thin.push(i);
                    }
                    for bits in v.routing.stale_buckets(now).into_iter().take(4) {
                        let target = random_id_in_bucket(&v.routing.own_id(), bits, rand::random());
                        if let Some(n) = v.routing.closest(&target, 1).first() {
                            work.push((i, n.addr, Query::FindNode { target }));
                        }
                        v.routing.touch_bucket(bits, now);
                    }
                    for addr in v.routing.questionable(now, 4) {
                        work.push((i, addr, Query::Ping));
                    }
                }
            }
            for i in thin {
                if boot.len() < self.inner.sockets.len() {
                    let d = self.clone();
                    boot.spawn(async move { d.self_lookup(i).await });
                }
            }
            for (i, addr, q) in work {
                let d = self.clone();
                boot.spawn(async move {
                    let target = match &q {
                        Query::FindNode { target } => Some(*target),
                        _ => None,
                    };
                    if let (Ok(r), Some(t)) = (d.query(i, addr, q).await, target) {
                        d.learn_nodes(&r.nodes, &t);
                    }
                });
            }
        }
        boot.shutdown().await;
    }

    /// Iterative `find_node` towards a virtual node's own id from the bootstrap routers, the
    /// saved nodes and the routing table.
    async fn self_lookup(&self, i: usize) {
        let own = self.ids()[i];
        let mut seeds: Vec<CompactNode> = self.closest(&own, 16);
        let mut extra: Vec<SocketAddrV4> = self.inner.cfg.bootstrap.clone();
        extra.extend(self.inner.cfg.known_nodes.iter().map(|n| n.addr));
        if seeds.len() >= 16 {
            extra.clear();
        }
        seeds.retain(|n| !extra.contains(&n.addr));
        let r = super::lookup::lookup(
            self,
            i,
            own,
            super::lookup::Method::FindNode,
            seeds,
            extra,
            Duration::from_secs(15),
            0,
        )
        .await;
        tracing::debug!(
            "DHT: self lookup of node {i}: {} answered of {} queried",
            r.answered,
            r.queried
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> Ipv4Addr {
        s.parse().unwrap()
    }

    /// Distinct responders, one per /16.
    fn responder(i: u16) -> Ipv4Addr {
        let [a, b] = i.to_be_bytes();
        Ipv4Addr::new(20 + a, b, 0, 1)
    }

    #[test]
    fn vote_needs_responders_and_prefixes() {
        let mut v = Votes::default();
        let me = ip("81.2.3.4");
        for i in 0..7 {
            assert_eq!(v.record(me, responder(i), None, 8, 4), None);
        }
        assert_eq!(v.record(me, responder(7), None, 8, 4), Some(me));
        // The same responder again does not count twice.
        let mut v = Votes::default();
        for _ in 0..20 {
            assert_eq!(v.record(me, responder(0), None, 8, 4), None);
        }
    }

    /// Junk claims fill the table but never lock a real address out (the old table refused
    /// every new candidate once 64 were held, forever).
    #[test]
    fn junk_claims_cannot_block_the_vote() {
        let mut v = Votes::default();
        for i in 0..200u32 {
            let junk = Ipv4Addr::from(0x0b00_0000 + i);
            assert_eq!(v.record(junk, responder(i as u16), None, 8, 4), None);
        }
        assert!(v.by_ip.len() <= VOTE_CANDIDATES);
        let me = ip("81.2.3.4");
        let won = (0..8).find_map(|i| v.record(me, responder(1000 + i), None, 8, 4));
        assert_eq!(won, Some(me));
    }

    /// Once an address is known, a minority claim cannot flip it; after a real change (a new
    /// window with votes only for the new address) it does.
    #[test]
    fn known_address_needs_a_majority_to_change() {
        let mut v = Votes::default();
        let me = ip("81.2.3.4");
        let other = ip("92.5.6.7");
        for i in 0..30 {
            assert_eq!(v.record(me, responder(i), Some(me), 8, 4), None);
        }
        for i in 100..120 {
            assert_eq!(v.record(other, responder(i), Some(me), 8, 4), None);
        }
        v.clear();
        let won = (200..208).find_map(|i| v.record(other, responder(i), Some(me), 8, 4));
        assert_eq!(won, Some(other));
    }

    /// An empty port list used to start one virtual node with no socket: the first query
    /// indexed past the socket list and panicked.
    #[tokio::test]
    async fn empty_port_list_gets_one_ephemeral_socket() {
        let mut cfg = EngineConfig::new(Mode::Polite);
        cfg.ports.clear();
        cfg.bind = Ipv4Addr::LOCALHOST;
        cfg.allow_loopback = true;
        cfg.sample = false;
        cfg.query_timeout = Duration::from_millis(200);
        let (d, _rx) = Dht::start(cfg).await.unwrap();
        assert_eq!(d.virtual_nodes(), 1);
        assert_eq!(d.local_ports().len(), 1);
        let to = SocketAddrV4::new(Ipv4Addr::LOCALHOST, 9);
        assert_eq!(
            d.query(d.next_node(), to, Query::Ping).await.err(),
            Some(QueryError::Timeout)
        );
        d.shutdown().await;
    }
}

//! The crawler node: one UDP socket per virtual node, transactions, the BEP 5/51 responder
//! (aggressive), the BEP 51 sampler and maintenance (docs/DHT.md §3.2–§3.6, D54, D55).
//!
//! Concurrency (D106): one shard per virtual node — its socket, receive task, routing table and
//! transaction map behind its own lock — plus a pacer task, one sampler task and one maintenance
//! task. Queries are futures that wait for a permit of the global pacer, register a transaction
//! keyed by (tid, remote address) in their shard and wait for the matching answer for up to
//! [`QUERY_TIMEOUT`]. State shared by every shard (crawl pool, per-IP limits in 16 shards, brake,
//! votes, peer store) has a lock of its own; no `std` lock is held across an `.await` and no
//! two are held at once.

use std::collections::{HashMap, HashSet};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use rand::Rng;
use serde::Serialize;
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::{JoinHandle, JoinSet};
use tokio::time::Instant;

use super::addr::AddrFilter;
use super::budget::SampleBudget;
use super::id::{NodeId, bep42_id, common_prefix_bits, random_id_in_bucket, spread_r_values};
use super::krpc::{
    self, Body, CompactNode, ERR_METHOD_UNKNOWN, ERR_PROTOCOL, Id, MAX_DATAGRAM, Message, Query,
    Response,
};
use super::limiter::{Brake, Bucket, IP_BUCKETS_CAP, IpLimits, Pacer, PerIp};
use super::peerstore::{self, PeerStore};
use super::pool::{Pool, PoolCounts, Tier};
use super::routing::{K, RoutingTable, Seen};

/// Virtual nodes (sockets) at most (D106; 8 before: one per BEP 42 prefix of an IPv4 address).
pub const MAX_VIRTUAL_NODES: usize = 32;
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
/// The sampler asks the same pool node for more nodes (`find_node`, when nothing is due for
/// sampling) at most once per this interval (docs/DHT.md §20.5).
pub const GROW_REASK: Duration = Duration::from_secs(10);
/// Pool nodes looked at per sampler pick (due ones, then grow candidates).
const GROW_CANDIDATES: usize = 8;
/// Grow times remembered (one entry per node asked within `GROW_REASK`).
const GROW_MEMORY: usize = 65_536;
/// A due node whose IP has no per-IP token now is due again after this.
const LIMITED_RETRY: Duration = Duration::from_secs(1);
/// Longest sampler wait when it has nothing to send.
const SAMPLER_IDLE: Duration = Duration::from_millis(250);
/// Sampler wait while no node is known at all.
const BOOTSTRAP_WAIT: Duration = Duration::from_millis(500);

/// The sampler's next query.
enum SamplerWork {
    /// `sample_infohashes` to a node due for it.
    Sample(SocketAddrV4),
    /// `find_node` (random target) to grow the pool.
    Grow(SocketAddrV4),
    /// Nothing to send before this instant.
    Idle(Instant),
}

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
    /// The node that told us about the hash: the sampled node, the node whose `get_peers`
    /// query reached us, or the announcer. Only for counting by country / network (D101): the
    /// server resolves it in memory and never stores, logs or sends the address.
    pub from: Option<SocketAddrV4>,
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

/// One virtual node (D106): its socket, receive task, routing table and transactions. Each has
/// its own lock, so answers arriving on different sockets and queries sent from different
/// virtual nodes do not wait for each other.
struct Shard {
    socket: Arc<UdpSocket>,
    st: Mutex<ShardState>,
}

struct ShardState {
    routing: RoutingTable,
    ro: bool,
    /// Queries on the wire from this node, keyed by (tid, remote address).
    pending: HashMap<PendingKey, oneshot::Sender<Reply>>,
}

/// The address filter and our external address (read on every send, written on an IP change).
struct Ext {
    filter: AddrFilter,
    external_ip: Option<Ipv4Addr>,
}

struct VoteState {
    votes: Votes,
    since: Instant,
}

/// Caps on the responses we send (D54 rule 4).
struct Responder {
    global: Bucket,
    per_ip: PerIp,
}

/// Engine state, split by what is used together (D106). Lock order: never hold two of these
/// locks at once, except [`Dht::set_external`] (the vote lock is released first).
struct Inner {
    cfg: EngineConfig,
    shards: Vec<Shard>,
    local_ports: Vec<u16>,
    /// The crawl pool spans the keyspace and every virtual node: one lock, short sections.
    pool: Mutex<Pool>,
    peers: Mutex<PeerStore>,
    /// 1 q/s per remote IP across every virtual node, sharded by address (exact per IP).
    per_ip: IpLimits,
    /// The global QPS cap over every virtual node (one FIFO, cancel-safe).
    pacer: Pacer,
    /// Transactions on the wire over every shard, against `pending_cap`.
    pending: AtomicUsize,
    pending_cap: usize,
    brake: Mutex<Brake>,
    /// Sent / answered in the current second, folded into `brake` once a second.
    brake_sent: AtomicU32,
    brake_answered: AtomicU32,
    resp: Mutex<Responder>,
    ext: RwLock<Ext>,
    votes: Mutex<VoteState>,
    secrets: RwLock<[[u8; 16]; 2]>,
    rr: AtomicUsize,
    /// The sampling / lookup split of the budget (D62): see [`super::budget`].
    budget: Mutex<SampleBudget>,
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

/// BEP 42 ids for `n` virtual nodes. Beyond the 8 prefixes an IPv4 address allows, the `r`
/// values repeat: such nodes share a 21-bit prefix with another one (D106), the rest of the id
/// is random.
fn ids_for(ip: Ipv4Addr, n: usize) -> Vec<NodeId> {
    let mut rng = rand::rng();
    let rs = spread_r_values(ip, n.min(8));
    (0..n)
        .map(|i| {
            let r = rs[i % rs.len()];
            let rand_byte = (rng.random::<u8>() & 0xf8) | r;
            bep42_id(ip, rand_byte, rng.random())
        })
        .collect()
}

/// (tid, remote address) within one shard.
type PendingKey = ([u8; 2], SocketAddrV4);

/// Owns a query's receiver; on drop removes the transaction unless it already belongs to a newer
/// query with the same key (its sender is then still open).
///
/// A query dropped while it waits for its answer (a lookup that ended) counts as a silence
/// for the pool (D95) once it waited the lookup timeout: with the lookup timeout equal to the
/// hard one (the default), a walk often ends on the soft timer just before the hard one fires,
/// and those silences would otherwise never be recorded.
struct PendingGuard<'a> {
    dht: &'a Dht,
    node: usize,
    key: PendingKey,
    rx: Option<oneshot::Receiver<Reply>>,
    /// Set once the datagram is sent, cleared when an answer or the hard timeout is handled.
    waiting_since: Option<Instant>,
}

impl Drop for PendingGuard<'_> {
    fn drop(&mut self) {
        drop(self.rx.take());
        let inner = &self.dht.inner;
        {
            let mut s = inner.shards[self.node].st.lock().unwrap();
            if s.pending.get(&self.key).is_some_and(|tx| tx.is_closed()) {
                s.pending.remove(&self.key);
                inner.pending.fetch_sub(1, Ordering::Relaxed);
            }
        }
        if self
            .waiting_since
            .is_some_and(|t| t.elapsed() >= self.dht.lookup_timeout())
        {
            inner.pool.lock().unwrap().silent(&self.key.1);
        }
    }
}

impl Dht {
    /// Bind the sockets and start the node. Returns the handle and the stream of sightings.
    pub async fn start(mut cfg: EngineConfig) -> std::io::Result<(Dht, mpsc::Receiver<Sighting>)> {
        // At least one socket (an empty port list means one ephemeral port): every virtual node
        // owns a socket.
        if cfg.ports.is_empty() {
            cfg.ports.push(0);
        }
        let n = cfg.ports.len().min(MAX_VIRTUAL_NODES);
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
        let ro = cfg.mode == Mode::Polite || external_ip.is_none();
        let mut filter = AddrFilter::new(cfg.allow_loopback);
        if let Some(ip) = external_ip
            && !ip.is_loopback()
        {
            filter.set_own(Some(ip));
        }
        let qps = cfg.qps.max(1);
        let shards = sockets
            .into_iter()
            .zip(&ids)
            .map(|(socket, id)| Shard {
                socket,
                st: Mutex::new(ShardState {
                    routing: RoutingTable::new(*id, now),
                    ro,
                    pending: HashMap::new(),
                }),
            })
            .collect();
        let (tx, rx) = mpsc::channel(cfg.sighting_capacity.max(1));
        let (stop_tx, stop) = watch::channel(false);
        let inner = Arc::new(Inner {
            shards,
            local_ports,
            pool: Mutex::new(Pool::new(cfg.pool_cap)),
            peers: Mutex::new(PeerStore::new(peerstore::CAP)),
            per_ip: IpLimits::new(
                cfg.per_ip_qps,
                (cfg.per_ip_qps * 2.0).max(2.0),
                IP_BUCKETS_CAP,
            ),
            pacer: Pacer::new(qps, now),
            pending: AtomicUsize::new(0),
            pending_cap: (qps as usize * cfg.query_timeout.as_secs().max(1) as usize * 2).max(64),
            brake: Mutex::new(Brake::default()),
            brake_sent: AtomicU32::new(0),
            brake_answered: AtomicU32::new(0),
            resp: Mutex::new(Responder {
                global: Bucket::new(cfg.response_rate, cfg.response_rate, now),
                per_ip: PerIp::new(cfg.response_per_ip, cfg.response_per_ip, IP_BUCKETS_CAP),
            }),
            ext: RwLock::new(Ext {
                filter,
                external_ip,
            }),
            votes: Mutex::new(VoteState {
                votes: Votes::default(),
                since: now,
            }),
            secrets: RwLock::new([rand::random(), rand::random()]),
            rr: AtomicUsize::new(0),
            budget: Mutex::new(SampleBudget::new(now)),
            cfg,
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
        for i in 0..dht.inner.shards.len() {
            let d = dht.clone();
            tasks.push(tokio::spawn(async move { d.recv_loop(i).await }));
        }
        let d = dht.clone();
        tasks.push(tokio::spawn(async move {
            let stop = d.inner.stop.clone();
            d.inner.pacer.run(stop).await
        }));
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
        self.inner.shards.len()
    }

    pub fn mode(&self) -> Mode {
        self.inner.cfg.mode
    }

    pub fn ids(&self) -> Vec<NodeId> {
        self.inner
            .shards
            .iter()
            .map(|s| s.st.lock().unwrap().routing.own_id())
            .collect()
    }

    /// The voted (or persisted) external IP and current ids.
    pub fn identity(&self) -> Option<Identity> {
        let ip = self.inner.ext.read().unwrap().external_ip?;
        Some(Identity {
            external_ip: ip,
            ids: self.ids(),
        })
    }

    /// Good routing contacts and recently answering pool nodes, for the next start.
    pub fn nodes_to_save(&self, n: usize) -> Vec<CompactNode> {
        let now = Instant::now();
        let mut out: Vec<CompactNode> = Vec::new();
        let mut seen = HashSet::new();
        for sh in &self.inner.shards {
            for c in sh.st.lock().unwrap().routing.good(now, n) {
                if seen.insert(c.addr) {
                    out.push(c);
                }
            }
        }
        let recent =
            self.inner
                .pool
                .lock()
                .unwrap()
                .answered_recently(now, Duration::from_secs(3600), n);
        for c in recent {
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

    /// The `k` closest routing contacts of every virtual node.
    fn routing_closest(&self, target: &Id, k: usize) -> Vec<CompactNode> {
        self.inner
            .shards
            .iter()
            .flat_map(|s| s.st.lock().unwrap().routing.closest(target, k))
            .collect()
    }

    /// The `k` closest routing contacts over every virtual node.
    pub fn closest(&self, target: &Id, k: usize) -> Vec<CompactNode> {
        let mut all = self.routing_closest(target, k);
        all.sort_by_key(|c| super::id::distance(&c.id, target));
        all.dedup_by_key(|c| c.addr);
        all.truncate(k);
        all
    }

    /// Lookup seeds: the `k` closest contacts of the routing tables **and** the crawl pool
    /// (D61), nearest first, one per address.
    pub fn closest_known(&self, target: &Id, k: usize) -> Vec<CompactNode> {
        let mut all = self.routing_closest(target, k);
        all.extend(self.inner.pool.lock().unwrap().closest(target, k));
        all.sort_by_key(|c| (super::id::distance(&c.id, target), c.addr));
        let mut seen = std::collections::HashSet::new();
        all.retain(|c| seen.insert(c.addr));
        all.truncate(k);
        all
    }

    /// Lookup seeds (D95): the [`SEED_SPREAD`] × `k` closest contacts of the crawl pool and the
    /// `k` closest of each routing table, ranked by [`super::lookup::rank_seeds`] (by distance,
    /// contacts silent since their last answer and rate-limited ones after the rest, dead ones
    /// never), with their tiers. The pool's tier wins for a contact both know (it sees every query).
    pub fn lookup_seeds(&self, target: &Id, k: usize) -> Vec<super::lookup::SeedCand> {
        use super::lookup::SeedCand;
        let now = Instant::now();
        let mut routed: Vec<(CompactNode, Tier)> = Vec::new();
        for sh in &self.inner.shards {
            routed.extend(sh.st.lock().unwrap().routing.closest_ranked(target, k, now));
        }
        let mut all: Vec<SeedCand> = {
            let pool = self.inner.pool.lock().unwrap();
            let mut all: Vec<SeedCand> = pool
                .closest_ranked(target, k * SEED_SPREAD, now)
                .into_iter()
                .map(|(node, tier)| SeedCand {
                    node,
                    tier,
                    limited: false,
                })
                .collect();
            all.extend(routed.into_iter().map(|(node, t)| SeedCand {
                node,
                tier: pool.tier_of(&node.addr, now).unwrap_or(t),
                limited: false,
            }));
            all
        };
        for c in &mut all {
            c.limited = !self.inner.per_ip.available(c.node.addr.ip(), now);
        }
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
        addrs
            .iter()
            .map(|a| !self.inner.per_ip.available(a.ip(), now))
            .collect()
    }

    /// The pool's tier of each address (unknown to the pool → [`Tier::Unknown`]), one lock.
    pub fn tiers_of(&self, addrs: &[SocketAddrV4]) -> Vec<Tier> {
        let now = Instant::now();
        let pool = self.inner.pool.lock().unwrap();
        addrs
            .iter()
            .map(|a| pool.tier_of(a, now).unwrap_or(Tier::Unknown))
            .collect()
    }

    /// Halve the KRPC query rate until `until` (router health probe, D66). Extends an active
    /// brake, never shortens it.
    pub fn brake_until(&self, until: Instant) {
        let mut b = self.inner.brake.lock().unwrap();
        b.engage_until(until);
        self.inner.pacer.set_braked(b.active(Instant::now()));
    }

    /// Set the sampler's share of the QPS budget (clamped to 0–1; 0 pauses sampling, the
    /// rest of the node keeps running). The split itself lives in [`super::budget`].
    pub fn set_sample_share(&self, share: f64) {
        self.inner
            .budget
            .lock()
            .unwrap()
            .set_share(share, Instant::now());
    }

    /// Reserve `priority` (0–1) of the QPS budget for the sampler, sent ahead of the pacer's
    /// queue (D106; 0 = the sampler queues like every query). See [`super::budget`].
    pub fn set_sample_priority(&self, priority: f64) {
        self.inner
            .budget
            .lock()
            .unwrap()
            .set_priority(priority, Instant::now());
    }

    pub fn sample_share(&self) -> f64 {
        self.inner.budget.lock().unwrap().share()
    }

    pub fn bootstrap_addrs(&self) -> &[SocketAddrV4] {
        &self.inner.cfg.bootstrap
    }

    /// Round-robin virtual node for the next query.
    pub fn next_node(&self) -> usize {
        self.inner
            .rr
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_add(1)
            % self.inner.shards.len()
    }

    pub fn stats(&self) -> EngineStats {
        let c = &self.inner.c;
        let g = |a: &AtomicU64| a.load(Ordering::Relaxed);
        let now = Instant::now();
        let routing: Vec<usize> = self
            .inner
            .shards
            .iter()
            .map(|s| s.st.lock().unwrap().routing.len())
            .collect();
        let (brake_active, brake_engaged) = {
            let b = self.inner.brake.lock().unwrap();
            (b.active(now), b.engaged)
        };
        let (peer_store_hashes, peer_store_peers) = {
            let p = self.inner.peers.lock().unwrap();
            (p.hashes(), p.peers())
        };
        let pool = self.inner.pool.lock().unwrap().counts().into();
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
            external_ip_known: self.inner.ext.read().unwrap().external_ip.is_some(),
            external_ip_changes: g(&c.ip_changes),
            brake_active,
            sample_share: self.sample_share(),
            brake_engaged,
            pending: self.inner.pending.load(Ordering::Relaxed),
            routing,
            pool,
            peer_store_hashes,
            peer_store_peers,
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

    /// Our own sockets on loopback (tests) are never a destination.
    fn is_own(&self, a: &SocketAddrV4) -> bool {
        a.ip().is_loopback() && self.inner.local_ports.contains(&a.port())
    }

    /// Keep only the addresses we may hand the fetcher (one lock for the whole list).
    pub fn retain_allowed(&self, v: &mut Vec<SocketAddrV4>) {
        let ext = self.inner.ext.read().unwrap();
        v.retain(|a| ext.filter.allows(a) && !self.is_own(a));
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
        let inner = &*self.inner;
        let c = &inner.c;
        if self.is_stopped() {
            return Err(QueryError::Stopped);
        }
        let kind = query_kind(&q);
        let node = node % inner.shards.len();
        let now = Instant::now();
        if !self.allows(&to) {
            bump(&c.filtered);
            return Err(QueryError::Filtered);
        }
        if !inner.per_ip.try_take(*to.ip(), now) {
            bump(&c.rate_limited);
            return Err(QueryError::RateLimited);
        }
        if inner.pending.load(Ordering::Relaxed) >= inner.pending_cap {
            bump(&c.busy);
            return Err(QueryError::Busy);
        }
        let (age, repeat) = match inner.pool.lock().unwrap().get(&to) {
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
        // Wait for a send permit *before* the transaction exists (D93): the pending map then
        // holds only queries on the wire. A query dropped while it waits gives its turn to the
        // next one (D106).
        if pace {
            let mut stop = inner.stop.clone();
            tokio::select! {
                _ = inner.pacer.acquire() => {}
                _ = stop.wait_for(|s| *s) => return Err(QueryError::Stopped),
            }
        }
        if inner.pending.load(Ordering::Relaxed) >= inner.pending_cap {
            bump(&c.busy);
            return Err(QueryError::Busy);
        }
        let shard = &inner.shards[node];
        let (tid, rx, own, ro) = {
            let mut s = shard.st.lock().unwrap();
            let mut tid: [u8; 2] = rand::random();
            while s.pending.contains_key(&(tid, to)) {
                tid = rand::random();
            }
            let (tx, rx) = oneshot::channel();
            s.pending.insert((tid, to), tx);
            (tid, rx, s.routing.own_id(), s.ro)
        };
        inner.pending.fetch_add(1, Ordering::Relaxed);
        let wire = krpc::encode(&Message {
            tid: tid.to_vec(),
            body: Body::Query {
                id: own,
                query: q,
                ro,
            },
            ip: None,
            version: Some(VERSION.to_vec()),
        });
        // Removes the transaction however this future ends, cancellation included (a lookup
        // that stops early drops its in-flight queries; without this they leaked until the
        // map's cap turned every later query into `Busy`).
        let mut guard = PendingGuard {
            dht: self,
            node,
            key: (tid, to),
            rx: Some(rx),
            waiting_since: None,
        };
        if shard
            .socket
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
        inner.brake_sent.fetch_add(1, Ordering::Relaxed);
        bump(&c.age_sent[age]);
        if repeat {
            bump(&c.repeat_sent);
        }
        let rx = guard.rx.as_mut().expect("receiver until drop");
        let r = tokio::time::timeout(inner.cfg.query_timeout, rx).await;
        guard.waiting_since = None;
        if let Ok(Ok(_)) = &r {
            // Any answer, error replies included, shows the node is alive (D95).
            inner.pool.lock().unwrap().heard(&to, Instant::now());
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
                shard.st.lock().unwrap().routing.failed(&to);
                inner.pool.lock().unwrap().silent(&to);
                Err(QueryError::Timeout)
            }
        }
    }

    /// Learnt contacts → the crawl pool, after the address filter and the node-farm rule.
    fn learn(&self, pool: &mut Pool, nodes: &[CompactNode], target: &Id, now: Instant) {
        let ext = self.inner.ext.read().unwrap();
        for n in nodes {
            if !ext.filter.allows(&n.addr) || self.is_own(&n.addr) {
                bump(&self.inner.c.nodes_filtered);
            } else if common_prefix_bits(&n.id, target) >= FARM_PREFIX_BITS {
                bump(&self.inner.c.nodes_farm);
            } else if pool.add(*n, now) {
                bump(&self.inner.c.nodes_learnt);
            }
        }
    }

    /// Record contacts learnt from a lookup answer (pool only; routing takes nodes that answer).
    pub fn learn_nodes(&self, nodes: &[CompactNode], target: &Id) {
        let mut pool = self.inner.pool.lock().unwrap();
        self.learn(&mut pool, nodes, target, Instant::now());
    }

    /// [`learn_nodes`](Self::learn_nodes), returning each contact's tier afterwards (D95), in
    /// the same lock.
    pub fn learn_ranked(&self, nodes: &[CompactNode], target: &Id) -> Vec<Tier> {
        let now = Instant::now();
        let mut pool = self.inner.pool.lock().unwrap();
        self.learn(&mut pool, nodes, target, now);
        nodes
            .iter()
            .map(|n| pool.tier_of(&n.addr, now).unwrap_or(Tier::Unknown))
            .collect()
    }

    async fn recv_loop(&self, i: usize) {
        let sock = self.inner.shards[i].socket.clone();
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
        let now = Instant::now();
        let tx = {
            let mut s = self.inner.shards[i].st.lock().unwrap();
            let Some(tx) = s.pending.remove(&(tid, src)) else {
                bump(&c.unmatched);
                return;
            };
            if let Reply::Response(r) = &reply {
                s.routing.insert(
                    CompactNode {
                        id: r.id,
                        addr: src,
                    },
                    Seen::Answered,
                    now,
                );
            }
            tx
        };
        self.inner.pending.fetch_sub(1, Ordering::Relaxed);
        self.inner.brake_answered.fetch_add(1, Ordering::Relaxed);
        match &reply {
            Reply::Response(_) => {
                bump(&c.answered);
                if let Some(ip) = msg.ip {
                    self.vote(*ip.ip(), *src.ip(), now);
                }
            }
            Reply::Error(_) => bump(&c.errors),
        }
        let _ = tx.send(reply);
    }

    /// Whether we may send to (or hand the fetcher) this address.
    pub fn allows(&self, a: &SocketAddrV4) -> bool {
        self.inner.ext.read().unwrap().filter.allows(a) && !self.is_own(a)
    }

    /// BEP 42 external IP vote (see [`Votes`]).
    fn vote(&self, seen_as: Ipv4Addr, responder: Ipv4Addr, now: Instant) {
        let usable = super::addr::is_global(seen_as)
            || (self.inner.cfg.allow_loopback && seen_as.is_loopback());
        if !usable {
            return;
        }
        let current = self.inner.ext.read().unwrap().external_ip;
        let switch = {
            let mut v = self.inner.votes.lock().unwrap();
            if now.saturating_duration_since(v.since) >= VOTE_WINDOW {
                v.votes.clear();
                v.since = now;
            }
            v.votes.record(
                seen_as,
                responder,
                current,
                self.inner.cfg.vote_responders,
                self.inner.cfg.vote_prefixes,
            )
        };
        if let Some(ip) = switch {
            self.set_external(ip, now);
        }
    }

    fn set_external(&self, ip: Ipv4Addr, now: Instant) {
        {
            let mut ext = self.inner.ext.write().unwrap();
            // Two answers may carry the winning vote at once: the first one switches.
            if ext.external_ip == Some(ip) {
                return;
            }
            if !ip.is_loopback() {
                ext.filter.set_own(Some(ip));
            }
            ext.external_ip = Some(ip);
        }
        let ids = ids_for(ip, self.inner.shards.len());
        let polite = self.inner.cfg.mode == Mode::Polite;
        for (sh, id) in self.inner.shards.iter().zip(ids) {
            let mut s = sh.st.lock().unwrap();
            s.routing.rebuild(id, now);
            s.ro = polite;
        }
        {
            let mut v = self.inner.votes.lock().unwrap();
            v.votes.clear();
            v.since = now;
        }
        bump(&self.inner.c.ip_changes);
        tracing::info!("DHT: external address voted, BEP 42 ids derived");
    }

    fn respond(&self, i: usize, src: SocketAddrV4, tid: Vec<u8>, id: Id, q: Query, ro: bool) {
        let inner = &*self.inner;
        let c = &inner.c;
        bump(&c.incoming[query_kind(&q)]);
        if inner.cfg.mode == Mode::Polite {
            // BEP 43: a read-only node "no longer responds to 'query' messages".
            bump(&c.incoming_ignored);
            return;
        }
        let now = Instant::now();
        if !inner.ext.read().unwrap().filter.allows(&src) {
            bump(&c.incoming_ignored);
            return;
        }
        {
            let mut r = inner.resp.lock().unwrap();
            if !r.global.try_take(now) || !r.per_ip.try_take(*src.ip(), now) {
                bump(&c.responses_dropped);
                return;
            }
        }
        let sender = CompactNode { id, addr: src };
        // The routing table of the node queried: insert the sender, read what the answer needs.
        let near = |target: Option<&Id>| -> (Id, Vec<CompactNode>) {
            let mut s = inner.shards[i].st.lock().unwrap();
            if !ro {
                s.routing.insert(sender, Seen::Queried, now);
            }
            let nodes = target.map(|t| s.routing.closest(t, K)).unwrap_or_default();
            (s.routing.own_id(), nodes)
        };
        let target = match &q {
            Query::FindNode { target } | Query::SampleInfohashes { target } => Some(*target),
            Query::GetPeers { info_hash } => Some(*info_hash),
            _ => None,
        };
        let (own, nodes) = near(target.as_ref());
        if !ro && inner.pool.lock().unwrap().add(sender, now) {
            bump(&c.nodes_learnt);
        }
        let resp = |nodes: Vec<CompactNode>| Response {
            id: own,
            nodes,
            ..Default::default()
        };
        let mut sighting = None;
        let body = match q {
            Query::Ping => Body::Response(resp(Vec::new())),
            Query::FindNode { .. } => Body::Response(resp(nodes)),
            Query::GetPeers { info_hash } => {
                sighting = Some(Sighting {
                    info_hash,
                    via: Via::GetPeers,
                    source: None,
                    peer: None,
                    from: Some(src),
                });
                let mut r = resp(nodes);
                let secret = inner.secrets.read().unwrap()[0];
                r.token = Some(token_for(&secret, *src.ip()).to_vec());
                r.values = inner
                    .peers
                    .lock()
                    .unwrap()
                    .get(&info_hash, now, krpc::MAX_VALUES);
                Body::Response(r)
            }
            Query::AnnouncePeer {
                info_hash,
                port,
                implied_port,
                token,
            } => {
                let secrets = *inner.secrets.read().unwrap();
                let valid = secrets
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
                    inner.peers.lock().unwrap().announce(info_hash, peer, now);
                    sighting = Some(Sighting {
                        info_hash,
                        via: Via::Announce,
                        source: None,
                        peer: Some(peer),
                        from: Some(src),
                    });
                    Body::Response(resp(Vec::new()))
                }
            }
            Query::SampleInfohashes { .. } => {
                let mut r = resp(nodes);
                let p = inner.peers.lock().unwrap();
                r.samples = Some(p.sample(krpc::MAX_SAMPLES));
                r.interval = Some(SAMPLE_INTERVAL_SERVED);
                r.num = Some(p.hashes() as i64);
                Body::Response(r)
            }
            Query::Unknown(_) => Body::Error {
                code: ERR_METHOD_UNKNOWN,
                message: "Method Unknown".into(),
            },
        };
        let wire = krpc::encode(&Message {
            tid,
            body,
            ip: Some(src),
            version: Some(VERSION.to_vec()),
        });
        if inner.shards[i]
            .socket
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
        let mut grown: HashMap<SocketAddrV4, Instant> = HashMap::new();
        loop {
            while inflight.try_join_next().is_some() {}
            // The sampler's own share of the budget (paused at 0, `super::budget`).
            let braked = if self.inner.pacer.braked() { 0.5 } else { 1.0 };
            let own = self
                .inner
                .budget
                .lock()
                .unwrap()
                .next_slot(qps * braked, Instant::now());
            let Some((own, ahead)) = own else {
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
            // What to send is chosen before a pace turn is taken, and only a query that can go
            // out takes one (docs/DHT.md §20.5): a node due for sampling, else a pool node not
            // asked for more nodes within `GROW_REASK`; both with a per-IP token free.
            let work = self.sampler_pick(Instant::now(), &mut grown);
            if let SamplerWork::Idle(until) = work {
                tokio::select! {
                    _ = tokio::time::sleep_until(until) => {}
                    _ = stop.wait_for(|s| *s) => break,
                }
                continue;
            }
            // A permit of the global pacer, or a send ahead of its queue charged to it (priority).
            if ahead {
                self.inner.pacer.take_ahead();
            } else {
                tokio::select! {
                    _ = self.inner.pacer.acquire() => {}
                    _ = stop.wait_for(|s| *s) => break,
                }
            }
            let node = self.next_node();
            let d = self.clone();
            match work {
                SamplerWork::Sample(addr) => {
                    inflight.spawn(async move { d.sample_one(node, addr).await });
                }
                SamplerWork::Grow(addr) => {
                    inflight.spawn(async move {
                        let target: Id = rand::random();
                        if let Ok(r) = d
                            .query_inner(node, addr, Query::FindNode { target }, false, || {})
                            .await
                        {
                            d.learn_nodes(&r.nodes, &target);
                        }
                    });
                }
                SamplerWork::Idle(_) => unreachable!("handled above"),
            }
        }
        inflight.shutdown().await;
    }

    /// The sampler's next query (see [`SamplerWork`]). Takes no lock while holding another.
    fn sampler_pick(
        &self,
        now: Instant,
        grown: &mut HashMap<SocketAddrV4, Instant>,
    ) -> SamplerWork {
        let inner = &*self.inner;
        // Due nodes first. One whose IP has no token now (a lookup just asked it) waits a
        // second instead of taking a turn that the per-IP limit would refuse.
        for _ in 0..GROW_CANDIDATES {
            let Some(e) = inner.pool.lock().unwrap().pop_due(now) else {
                break;
            };
            let addr = e.node.addr;
            if inner.per_ip.available(addr.ip(), now) {
                return SamplerWork::Sample(addr);
            }
            inner
                .pool
                .lock()
                .unwrap()
                .reschedule(&addr, now + LIMITED_RETRY);
        }
        // Nothing due: ask a pool node for more nodes. The same node at most once per
        // `GROW_REASK` (a small pool used to be re-asked on every pace turn, docs/DHT.md §20.5).
        let (cands, next_due) = {
            let pool = inner.pool.lock().unwrap();
            (pool.random(GROW_CANDIDATES), pool.next_due())
        };
        let cands = if cands.is_empty() {
            let t: Id = rand::random();
            self.routing_closest(&t, 1)
        } else {
            cands
        };
        if cands.is_empty() {
            // Nothing known yet: wait for the bootstrap.
            return SamplerWork::Idle(now + BOOTSTRAP_WAIT);
        }
        for c in cands {
            let a = c.addr;
            if grown
                .get(&a)
                .is_some_and(|t| now.saturating_duration_since(*t) < GROW_REASK)
                || !inner.per_ip.available(a.ip(), now)
            {
                continue;
            }
            if grown.len() >= GROW_MEMORY {
                grown.retain(|_, t| now.saturating_duration_since(*t) < GROW_REASK);
                if grown.len() >= GROW_MEMORY {
                    grown.clear();
                }
            }
            grown.insert(a, now);
            return SamplerWork::Grow(a);
        }
        // Nothing to send: wait for the next due node, at most `SAMPLER_IDLE`.
        let until = now + SAMPLER_IDLE;
        SamplerWork::Idle(next_due.map_or(until, |t| t.clamp(now, until)))
    }

    async fn sample_one(&self, node: usize, addr: SocketAddrV4) {
        let target: Id = rand::random();
        let r = self
            .query_inner(node, addr, Query::SampleInfohashes { target }, false, || {})
            .await;
        let now = Instant::now();
        let mut found = Vec::new();
        {
            let mut pool = self.inner.pool.lock().unwrap();
            match r {
                Ok(r) => {
                    pool.answered(&addr, r.id, r.samples.is_some(), r.interval, now);
                    self.learn(&mut pool, &r.nodes, &target, now);
                    found = r.samples.unwrap_or_default();
                }
                Err(QueryError::Remote(_)) => pool.unsupported(&addr, now),
                Err(QueryError::Timeout) => {
                    pool.failed(&addr, now);
                }
                Err(QueryError::Filtered) => pool.remove(&addr),
                Err(QueryError::Stopped) => {}
                Err(_) => pool.reschedule(&addr, now + Duration::from_secs(2)),
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
                from: Some(addr),
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
        for i in 0..self.inner.shards.len() {
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
                let sent = self.inner.brake_sent.swap(0, Ordering::Relaxed);
                let answered = self.inner.brake_answered.swap(0, Ordering::Relaxed);
                let mut b = self.inner.brake.lock().unwrap();
                b.record(sent, answered);
                if b.tick(now) {
                    tracing::warn!(
                        "DHT: answered share collapsed, halving the query rate for 5 min"
                    );
                }
                self.inner.pacer.set_braked(b.active(now));
            }
            if now.saturating_duration_since(last_rotation) >= SECRET_ROTATION {
                let mut s = self.inner.secrets.write().unwrap();
                s[1] = s[0];
                s[0] = rand::random();
                last_rotation = now;
            }
            if now.saturating_duration_since(last_minute) < Duration::from_secs(60) {
                continue;
            }
            last_minute = now;
            self.inner.peers.lock().unwrap().purge(now);
            let mut work: Vec<(usize, SocketAddrV4, Query)> = Vec::new();
            let mut thin = Vec::new();
            for (i, sh) in self.inner.shards.iter().enumerate() {
                let mut v = sh.st.lock().unwrap();
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
            for i in thin {
                if boot.len() < self.inner.shards.len() {
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
        let own = self.inner.shards[i].st.lock().unwrap().routing.own_id();
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

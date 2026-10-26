//! BEP 33 DHT scrape (get_peers + scrape=1), client-only.
//!
//! A single IPv4 socket with a random node id walks towards the infohash: each `get_peers` answer
//! adds closer nodes, `values` peers and the `BFsd`/`BFpe` bloom filters. Queries carry BEP 43
//! `ro=1` and we never answer queries, so we stay out of other nodes' routing tables.
//!
//! A long-lived checker keeps a [`NodeCache`] of nodes that answered earlier walks and starts
//! each walk from the cached nodes closest to the target (D45): walks are a few hops shorter and
//! the public bootstrap routers are only asked while the cache is nearly empty.

use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, SocketAddr};
use std::sync::Mutex;
use std::time::Duration;

use tokio::net::UdpSocket;
use tokio::time::Instant;

use super::bencode::{self, Value};
use crate::error::{Error, Result};

pub const BOOTSTRAP: &[&str] = &[
    "router.bittorrent.com:6881",
    "dht.transmissionbt.com:6881",
    "dht.libtorrent.org:25401",
    "router.utorrent.com:6881",
];

/// Bloom estimates saturate around here (BEP 33), so callers show "8000+".
pub const ESTIMATE_CAP: u32 = 8000;
/// Nodes queried in parallel are picked among this many closest known ones.
const WALK_WIDTH: usize = 16;
/// The walk stops once this many closest known nodes have answered (BEP 5's K: the nodes that
/// store the swarm). Waiting for all of `WALK_WIDTH` let most walks run into their budget
/// (D45: 9.5 s average against the live DHT, 4.5 s with K).
const WALK_DONE: usize = 8;
const MAX_IN_FLIGHT: usize = 8;
const RPC_TIMEOUT: Duration = Duration::from_millis(1200);
const MAX_NODES: usize = 2000;
const BLOOM_BITS: usize = 2048;

#[derive(Clone, Debug, PartialEq)]
pub struct Bloom256(pub [u8; 256]);

impl Bloom256 {
    pub fn new() -> Self {
        Bloom256([0; 256])
    }
    /// BEP 33: sha1 of the raw address bytes (no port), bits from bytes 0–1 and 2–3 (LE) mod 2048.
    pub fn insert_ip(&mut self, ip: &std::net::IpAddr) {
        let h = match ip {
            IpAddr::V4(v) => sha1_smol::Sha1::from(v.octets()).digest().bytes(),
            IpAddr::V6(v) => sha1_smol::Sha1::from(v.octets()).digest().bytes(),
        };
        for i in [
            (h[0] as usize | (h[1] as usize) << 8) % BLOOM_BITS,
            (h[2] as usize | (h[3] as usize) << 8) % BLOOM_BITS,
        ] {
            self.0[i / 8] |= 1 << (i % 8);
        }
    }
    pub fn union(&mut self, other: &[u8]) {
        for (a, b) in self.0.iter_mut().zip(other) {
            *a |= *b;
        }
    }
    /// BEP 33 size estimate: `ln(c/m) / (k·ln(1 − 1/m))` with `c = min(m − 1, zero bits)`.
    pub fn estimate(&self) -> f64 {
        let m = BLOOM_BITS as f64;
        let zeros = self.0.iter().map(|b| b.count_zeros()).sum::<u32>() as f64;
        let c = zeros.min(m - 1.0);
        (c / m).ln() / (2.0 * (1.0 - 1.0 / m).ln())
    }
    /// Rounded estimate capped at [`ESTIMATE_CAP`]; an empty filter is 0 (the formula gives 0.5).
    pub fn count(&self) -> u32 {
        if self.0.iter().all(|&b| b == 0) {
            return 0;
        }
        self.estimate().round().min(ESTIMATE_CAP as f64) as u32
    }
}

impl Default for Bloom256 {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct DhtScrape {
    pub seeds_est: u32,
    pub peers_est: u32,
    /// union(BFsd, BFpe) estimate, capped 8000.
    pub total_est: u32,
    pub values_ips: u32,
    pub nodes_queried: u32,
    pub bf_responses: u32,
    /// Answers with swarm data left out as node-farm / made-for-the-target ids (D45).
    pub ignored_answers: u32,
}

/// DHT scrape from the public bootstrap nodes. `nodes_queried` counts nodes that answered;
/// `peers_est`/`total_est` also include the `values` IPs (BEP 33's advice for nodes without BF).
pub async fn scrape(infohash: &[u8; 20], budget: std::time::Duration) -> crate::Result<DhtScrape> {
    let deadline = Instant::now() + budget;
    let bootstrap = resolve_bootstrap().await?;
    let seeds = bootstrap.into_iter().map(|a| (a, None)).collect();
    Ok(walk(seeds, infohash, deadline).await?.0)
}

async fn resolve_bootstrap() -> Result<Vec<SocketAddr>> {
    let lookups = BOOTSTRAP.iter().map(|host| async move {
        let found = tokio::time::timeout(Duration::from_secs(3), tokio::net::lookup_host(*host));
        match found.await {
            Ok(Ok(addrs)) => addrs.filter(SocketAddr::is_ipv4).collect(),
            _ => Vec::new(),
        }
    });
    let bootstrap: Vec<SocketAddr> = futures::future::join_all(lookups)
        .await
        .into_iter()
        .flatten()
        .collect();
    if bootstrap.is_empty() {
        return Err(Error::Network("DHT: no bootstrap node resolved".into()));
    }
    Ok(bootstrap)
}

/// Same walk from explicit bootstrap addresses (IPv4), e.g. a cached node list or a test node.
pub async fn scrape_from(
    bootstrap: &[SocketAddr],
    infohash: &[u8; 20],
    budget: Duration,
) -> Result<DhtScrape> {
    let seeds = bootstrap.iter().map(|a| (*a, None)).collect();
    Ok(walk(seeds, infohash, Instant::now() + budget).await?.0)
}

/// Nodes a walk starts from when the cache has them.
const CACHE_SEEDS: usize = 24;
/// Below this many cached nodes a walk also asks the bootstrap routers.
const CACHE_MIN: usize = 64;

/// Nodes that answered earlier walks, by address, with their id and when they last answered.
/// Bounded: the longest-silent entries go first. Nodes that time out are dropped.
pub struct NodeCache {
    cap: usize,
    nodes: Mutex<HashMap<SocketAddr, ([u8; 20], Instant)>>,
    bootstrap: Vec<SocketAddr>,
}

impl NodeCache {
    pub fn new(cap: usize) -> Self {
        Self::with_bootstrap(cap, Vec::new())
    }

    /// A cache that falls back to these addresses instead of resolving [`BOOTSTRAP`] (tests).
    pub fn with_bootstrap(cap: usize, bootstrap: Vec<SocketAddr>) -> Self {
        Self {
            cap: cap.max(CACHE_SEEDS),
            nodes: Mutex::new(HashMap::new()),
            bootstrap,
        }
    }

    pub fn len(&self) -> usize {
        self.nodes.lock().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The `k` cached nodes closest (XOR) to `target`.
    fn closest(&self, target: &[u8; 20], k: usize) -> Vec<(SocketAddr, [u8; 20])> {
        let nodes = self.nodes.lock().unwrap();
        let mut all: Vec<([u8; 20], SocketAddr, [u8; 20])> = nodes
            .iter()
            .map(|(a, (id, _))| (std::array::from_fn(|i| id[i] ^ target[i]), *a, *id))
            .collect();
        if all.len() > k {
            all.select_nth_unstable(k - 1);
            all.truncate(k);
        }
        all.into_iter().map(|(_, a, id)| (a, id)).collect()
    }

    fn update(&self, answered: &[(SocketAddr, [u8; 20])], failed: &[SocketAddr]) {
        let now = Instant::now();
        let mut nodes = self.nodes.lock().unwrap();
        for a in failed {
            nodes.remove(a);
        }
        for (a, id) in answered {
            nodes.insert(*a, (*id, now));
        }
        if nodes.len() > self.cap {
            let mut by_age: Vec<(Instant, SocketAddr)> =
                nodes.iter().map(|(a, (_, at))| (*at, *a)).collect();
            let excess = nodes.len() - self.cap;
            by_age.select_nth_unstable(excess - 1);
            for (_, a) in &by_age[..excess] {
                nodes.remove(a);
            }
        }
    }
}

/// DHT scrape starting from the cached nodes closest to `infohash` (plus the bootstrap routers
/// while the cache is small); every node that answered goes back into the cache.
pub async fn scrape_cached(
    cache: &NodeCache,
    infohash: &[u8; 20],
    budget: Duration,
) -> Result<DhtScrape> {
    let deadline = Instant::now() + budget;
    let mut seeds: Vec<(SocketAddr, Option<[u8; 20]>)> = cache
        .closest(infohash, CACHE_SEEDS)
        .into_iter()
        .map(|(a, id)| (a, Some(id)))
        .collect();
    if seeds.len() < CACHE_MIN.min(cache.cap) {
        let bootstrap = if cache.bootstrap.is_empty() {
            resolve_bootstrap().await
        } else {
            Ok(cache.bootstrap.clone())
        };
        match bootstrap {
            Ok(b) => seeds.extend(b.into_iter().map(|a| (a, None))),
            Err(e) if seeds.is_empty() => return Err(e),
            Err(_) => {}
        }
    }
    let (scrape, answered, failed) = walk(seeds, infohash, deadline).await?;
    cache.update(&answered, &failed);
    Ok(scrape)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    New,
    InFlight { tid: [u8; 2], until: Instant },
    Answered,
    Failed,
}

struct Node {
    /// Unknown for bootstrap nodes until they answer: those sort last.
    id: Option<[u8; 20]>,
    state: State,
}

/// Swarm data one node sent (values and/or BEP 33 filters).
struct SwarmAnswer {
    src: SocketAddr,
    id: Option<[u8; 20]>,
    values: Vec<IpAddr>,
    bf: Vec<Vec<u8>>,
    seeds_bf: Option<Vec<u8>>,
}

/// An id sharing this many leading bits with the target was made for it: honest nodes near a
/// target share about log2(DHT size) ≈ 23 bits.
const SYBIL_PREFIX_BITS: u32 = 64;
/// One IP answering from this many ports in one walk is a node farm.
const SYBIL_PORTS_PER_IP: usize = 3;

struct Walk {
    target: [u8; 20],
    nodes: HashMap<SocketAddr, Node>,
    swarm: Vec<SwarmAnswer>,
    answered: u32,
}

impl Walk {
    fn distance(&self, n: &Node) -> [u8; 20] {
        match n.id {
            Some(id) => std::array::from_fn(|i| id[i] ^ self.target[i]),
            None => [0xff; 20],
        }
    }

    /// Non-failed nodes, closest first, limited to the walk window. Suspicious nodes are left
    /// out: they sit "close" to every target and would end the walk before the real ones.
    fn window(&self) -> Vec<SocketAddr> {
        let ports = self.ports_per_ip();
        let mut live: Vec<_> = self
            .nodes
            .iter()
            .filter(|(a, n)| n.state != State::Failed && !self.suspicious(a, n.id, &ports))
            .map(|(a, n)| (self.distance(n), *a))
            .collect();
        live.sort_unstable();
        live.into_iter().take(WALK_WIDTH).map(|(_, a)| a).collect()
    }

    fn add_node(&mut self, addr: SocketAddr, id: [u8; 20]) {
        if addr.port() == 0 || addr.ip().is_unspecified() || self.nodes.len() >= MAX_NODES {
            return;
        }
        self.nodes.entry(addr).or_insert(Node {
            id: Some(id),
            state: State::New,
        });
    }

    fn handle(&mut self, src: SocketAddr, msg: &[u8]) {
        let Ok(msg) = bencode::decode(msg) else {
            return;
        };
        let Some(node) = self.nodes.get_mut(&src) else {
            return;
        };
        let tid = msg.get(b"t").and_then(Value::as_bytes);
        if !matches!(node.state, State::InFlight { tid: t, .. } if Some(&t[..]) == tid) {
            return;
        }
        let r = match msg.get(b"y").and_then(Value::as_bytes) {
            Some(b"r") => msg.get(b"r"),
            _ => None,
        };
        let Some(r) = r else {
            node.state = State::Failed;
            return;
        };
        node.state = State::Answered;
        if let Some(id) = r.get(b"id").and_then(Value::as_bytes)
            && let Ok(id) = <[u8; 20]>::try_from(id)
        {
            node.id = Some(id);
        }
        self.answered += 1;

        if let Some(nodes) = r.get(b"nodes").and_then(Value::as_bytes) {
            for c in nodes.as_chunks::<26>().0 {
                let id: [u8; 20] = c[..20].try_into().expect("20 bytes");
                let ip = std::net::Ipv4Addr::new(c[20], c[21], c[22], c[23]);
                let port = u16::from_be_bytes([c[24], c[25]]);
                self.add_node((ip, port).into(), id);
            }
        }
        let mut answer = SwarmAnswer {
            src,
            id: self.nodes.get(&src).and_then(|n| n.id),
            values: Vec::new(),
            bf: Vec::new(),
            seeds_bf: None,
        };
        for v in r
            .get(b"values")
            .and_then(Value::as_list)
            .unwrap_or_default()
        {
            match v.as_bytes() {
                Some(p) if p.len() == 6 => answer
                    .values
                    .push(IpAddr::from(<[u8; 4]>::try_from(&p[..4]).expect("4"))),
                Some(p) if p.len() == 18 => answer
                    .values
                    .push(IpAddr::from(<[u8; 16]>::try_from(&p[..16]).expect("16"))),
                _ => {}
            }
        }
        if let Some(bf) = r.get(b"BFsd").and_then(Value::as_bytes)
            && bf.len() == 256
        {
            answer.seeds_bf = Some(bf.to_vec());
        }
        if let Some(bf) = r.get(b"BFpe").and_then(Value::as_bytes)
            && bf.len() == 256
        {
            answer.bf.push(bf.to_vec());
        }
        if !answer.values.is_empty() || answer.seeds_bf.is_some() || !answer.bf.is_empty() {
            self.swarm.push(answer);
        }
    }

    fn ports_per_ip(&self) -> HashMap<IpAddr, usize> {
        let mut ports: HashMap<IpAddr, usize> = HashMap::new();
        for a in self.nodes.keys() {
            *ports.entry(a.ip()).or_default() += 1;
        }
        ports
    }

    /// Node farms and ids made for the target answer every lookup with made-up peers (seen on
    /// the live DHT, D45): they are not walked through and their swarm data is left out.
    fn suspicious(
        &self,
        addr: &SocketAddr,
        id: Option<[u8; 20]>,
        ports: &HashMap<IpAddr, usize>,
    ) -> bool {
        let prefix = id.map_or(0, |id| {
            let mut bits = 0;
            for (x, t) in id.iter().zip(&self.target) {
                let d = x ^ t;
                bits += d.leading_zeros();
                if d != 0 {
                    break;
                }
            }
            bits
        });
        prefix >= SYBIL_PREFIX_BITS
            || ports.get(&addr.ip()).copied().unwrap_or(0) >= SYBIL_PORTS_PER_IP
    }

    fn result(&self) -> DhtScrape {
        let mut seeds = Bloom256::new();
        let mut peers = Bloom256::new();
        let mut values: HashSet<IpAddr> = HashSet::new();
        let (mut bf_responses, mut ignored) = (0, 0);
        let ports = self.ports_per_ip();
        for a in &self.swarm {
            if self.suspicious(&a.src, a.id, &ports) {
                ignored += 1;
                continue;
            }
            values.extend(&a.values);
            if let Some(bf) = &a.seeds_bf {
                seeds.union(bf);
            }
            for bf in &a.bf {
                peers.union(bf);
            }
            if a.seeds_bf.is_some() || !a.bf.is_empty() {
                bf_responses += 1;
            }
        }
        for ip in &values {
            peers.insert_ip(ip);
        }
        let mut total = peers.clone();
        total.union(&seeds.0);
        DhtScrape {
            seeds_est: seeds.count(),
            peers_est: peers.count(),
            total_est: total.count(),
            values_ips: values.len() as u32,
            nodes_queried: self.answered,
            bf_responses,
            ignored_answers: ignored,
        }
    }
}

/// Walk from `seeds` (address, id if known). Also returns the nodes that answered (with their
/// ids) and those that timed out, for the [`NodeCache`].
async fn walk(
    seeds: Vec<(SocketAddr, Option<[u8; 20]>)>,
    infohash: &[u8; 20],
    deadline: Instant,
) -> Result<(DhtScrape, Vec<(SocketAddr, [u8; 20])>, Vec<SocketAddr>)> {
    let sock = UdpSocket::bind(SocketAddr::from(([0u8; 4], 0)))
        .await
        .map_err(|e| Error::Network(format!("DHT bind: {e}")))?;
    let my_id: [u8; 20] = rand::random();
    let mut next_tid: u16 = rand::random();
    let mut w = Walk {
        target: *infohash,
        nodes: seeds
            .into_iter()
            .filter(|(a, _)| a.is_ipv4())
            .map(|(a, id)| {
                (
                    a,
                    Node {
                        id,
                        state: State::New,
                    },
                )
            })
            .collect(),
        swarm: Vec::new(),
        answered: 0,
    };
    let mut buf = vec![0u8; 4096];

    loop {
        let now = Instant::now();
        if now >= deadline {
            break;
        }
        for n in w.nodes.values_mut() {
            if matches!(n.state, State::InFlight { until, .. } if until <= now) {
                n.state = State::Failed;
            }
        }
        let window = w.window();
        if !window.is_empty()
            && window
                .iter()
                .take(WALK_DONE)
                .all(|a| w.nodes[a].state == State::Answered)
        {
            break;
        }
        let mut in_flight = w
            .nodes
            .values()
            .filter(|n| matches!(n.state, State::InFlight { .. }))
            .count();
        for addr in &window {
            if in_flight >= MAX_IN_FLIGHT {
                break;
            }
            if w.nodes[addr].state != State::New {
                continue;
            }
            let tid = next_tid.to_be_bytes();
            next_tid = next_tid.wrapping_add(1);
            let state = match sock.send_to(&get_peers(&my_id, infohash, &tid), addr).await {
                Ok(_) => {
                    in_flight += 1;
                    State::InFlight {
                        tid,
                        until: now + RPC_TIMEOUT,
                    }
                }
                Err(_) => State::Failed,
            };
            w.nodes.get_mut(addr).expect("in window").state = state;
        }
        if in_flight == 0 {
            break;
        }
        let wake = w
            .nodes
            .values()
            .filter_map(|n| match n.state {
                State::InFlight { until, .. } => Some(until),
                _ => None,
            })
            .min()
            .map_or(deadline, |u| u.min(deadline));
        if let Ok(Ok((n, src))) = tokio::time::timeout_at(wake, sock.recv_from(&mut buf)).await {
            w.handle(src, &buf[..n]);
        }
    }

    if w.answered == 0 {
        return Err(Error::Timeout("DHT: no node answered".into()));
    }
    let mut answered = Vec::new();
    let mut failed = Vec::new();
    // Node farms stay out of the cache, or later walks would start from them.
    let ports = w.ports_per_ip();
    for (a, n) in &w.nodes {
        match (n.state, n.id) {
            (State::Answered, Some(id)) if w.suspicious(a, Some(id), &ports) => {}
            (State::Answered, Some(id)) => answered.push((*a, id)),
            (State::Failed, _) => failed.push(*a),
            _ => {}
        }
    }
    Ok((w.result(), answered, failed))
}

fn get_peers(my_id: &[u8; 20], infohash: &[u8; 20], tid: &[u8; 2]) -> Vec<u8> {
    let b = |s: &[u8]| Value::Bytes(s.to_vec());
    bencode::encode(&Value::Dict(vec![
        (
            b"a".to_vec(),
            Value::Dict(vec![
                (b"id".to_vec(), b(my_id)),
                (b"info_hash".to_vec(), b(infohash)),
                (b"scrape".to_vec(), Value::Int(1)),
            ]),
        ),
        (b"q".to_vec(), b(b"get_peers")),
        (b"ro".to_vec(), Value::Int(1)),
        (b"t".to_vec(), b(tid)),
        (b"y".to_vec(), b(b"q")),
    ]))
}

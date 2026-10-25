//! BEP 33 DHT scrape (get_peers + scrape=1), client-only.
//!
//! A single IPv4 socket with a random node id walks towards the infohash: each `get_peers` answer
//! adds closer nodes, `values` peers and the `BFsd`/`BFpe` bloom filters. Queries carry BEP 43
//! `ro=1` and we never answer queries, so we stay out of other nodes' routing tables.

use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, SocketAddr};
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
/// The walk stops once this many closest known nodes have answered.
const WALK_WIDTH: usize = 16;
const MAX_IN_FLIGHT: usize = 8;
const RPC_TIMEOUT: Duration = Duration::from_secs(2);
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
}

/// DHT scrape from the public bootstrap nodes. `nodes_queried` counts nodes that answered;
/// `peers_est`/`total_est` also include the `values` IPs (BEP 33's advice for nodes without BF).
pub async fn scrape(infohash: &[u8; 20], budget: std::time::Duration) -> crate::Result<DhtScrape> {
    let deadline = Instant::now() + budget;
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
    walk(&bootstrap, infohash, deadline).await
}

/// Same walk from explicit bootstrap addresses (IPv4), e.g. a cached node list or a test node.
pub async fn scrape_from(
    bootstrap: &[SocketAddr],
    infohash: &[u8; 20],
    budget: Duration,
) -> Result<DhtScrape> {
    walk(bootstrap, infohash, Instant::now() + budget).await
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

struct Walk {
    target: [u8; 20],
    nodes: HashMap<SocketAddr, Node>,
    seeds: Bloom256,
    peers: Bloom256,
    values: HashSet<IpAddr>,
    answered: u32,
    bf_responses: u32,
}

impl Walk {
    fn distance(&self, n: &Node) -> [u8; 20] {
        match n.id {
            Some(id) => std::array::from_fn(|i| id[i] ^ self.target[i]),
            None => [0xff; 20],
        }
    }

    /// Non-failed nodes, closest first, limited to the walk window.
    fn window(&self) -> Vec<SocketAddr> {
        let mut live: Vec<_> = self
            .nodes
            .iter()
            .filter(|(_, n)| n.state != State::Failed)
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
        for v in r
            .get(b"values")
            .and_then(Value::as_list)
            .unwrap_or_default()
        {
            match v.as_bytes() {
                Some(p) if p.len() == 6 => {
                    self.values
                        .insert(IpAddr::from(<[u8; 4]>::try_from(&p[..4]).expect("4")));
                }
                Some(p) if p.len() == 18 => {
                    self.values
                        .insert(IpAddr::from(<[u8; 16]>::try_from(&p[..16]).expect("16")));
                }
                _ => {}
            }
        }
        let mut got_bf = false;
        for (key, bloom) in [(&b"BFsd"[..], &mut self.seeds), (b"BFpe", &mut self.peers)] {
            if let Some(bf) = r.get(key).and_then(Value::as_bytes)
                && bf.len() == 256
            {
                bloom.union(bf);
                got_bf = true;
            }
        }
        if got_bf {
            self.bf_responses += 1;
        }
    }

    fn result(&self) -> DhtScrape {
        let mut peers = self.peers.clone();
        for ip in &self.values {
            peers.insert_ip(ip);
        }
        let mut total = peers.clone();
        total.union(&self.seeds.0);
        DhtScrape {
            seeds_est: self.seeds.count(),
            peers_est: peers.count(),
            total_est: total.count(),
            values_ips: self.values.len() as u32,
            nodes_queried: self.answered,
            bf_responses: self.bf_responses,
        }
    }
}

async fn walk(
    bootstrap: &[SocketAddr],
    infohash: &[u8; 20],
    deadline: Instant,
) -> Result<DhtScrape> {
    let sock = UdpSocket::bind(SocketAddr::from(([0u8; 4], 0)))
        .await
        .map_err(|e| Error::Network(format!("DHT bind: {e}")))?;
    let my_id: [u8; 20] = rand::random();
    let mut next_tid: u16 = rand::random();
    let mut w = Walk {
        target: *infohash,
        nodes: bootstrap
            .iter()
            .filter(|a| a.is_ipv4())
            .map(|a| {
                (
                    *a,
                    Node {
                        id: None,
                        state: State::New,
                    },
                )
            })
            .collect(),
        seeds: Bloom256::new(),
        peers: Bloom256::new(),
        values: HashSet::new(),
        answered: 0,
        bf_responses: 0,
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
        if !window.is_empty() && window.iter().all(|a| w.nodes[a].state == State::Answered) {
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
    Ok(w.result())
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

//! Iterative lookups (BEP 5 `find_node` / `get_peers`) through the crawler's own sockets and rate
//! limits, and peer collection for the metadata stage (docs/DHT.md §3.5, §4 step 1).
//!
//! Generalised from `liveness::dht::walk`: the walk keeps the 16 closest candidates, has 4
//! queries in flight, and stops once the 8 closest known nodes answered, when the budget runs
//! out, or (for `get_peers`) once enough peers are known. Node-farm rules (D45): ids sharing ≥ 64
//! bits with the target and IPs answering from ≥ 3 ports are not walked through and their
//! `values` are ignored.

use std::collections::{HashMap, HashSet};
use std::net::{Ipv4Addr, SocketAddrV4};
use std::time::Duration;

use futures::StreamExt;
use futures::stream::FuturesUnordered;
use serde::Serialize;
use tokio::time::Instant;

use super::engine::{Dht, FARM_PREFIX_BITS, QueryError};
use super::id::{common_prefix_bits, distance};
use super::krpc::{CompactNode, Id, Query, Response};

const WIDTH: usize = 16;
const DONE: usize = 8;
const ALPHA: usize = 4;
const MAX_CANDIDATES: usize = 512;
const FARM_PORTS: usize = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    FindNode,
    GetPeers,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum St {
    New,
    InFlight,
    Answered,
    Failed,
}

struct Cand {
    id: Option<Id>,
    st: St,
    values: Vec<SocketAddrV4>,
}

#[derive(Clone, Debug, Default)]
pub struct LookupResult {
    pub peers: Vec<SocketAddrV4>,
    pub queried: u32,
    pub answered: u32,
    /// Closest answering nodes, nearest first.
    pub closest: Vec<CompactNode>,
}

/// Walk towards `target` from `seeds` (known ids) and `extra` (addresses without ids, e.g. the
/// bootstrap routers). `want_peers` = 0 never stops early on peers.
#[allow(clippy::too_many_arguments)]
pub async fn lookup(
    dht: &Dht,
    node: usize,
    target: Id,
    method: Method,
    seeds: Vec<CompactNode>,
    extra: Vec<SocketAddrV4>,
    budget: Duration,
    want_peers: usize,
) -> LookupResult {
    let deadline = Instant::now() + budget;
    let mut cands: HashMap<SocketAddrV4, Cand> = HashMap::new();
    for s in seeds {
        cands.insert(
            s.addr,
            Cand {
                id: Some(s.id),
                st: St::New,
                values: Vec::new(),
            },
        );
    }
    for a in extra {
        cands.entry(a).or_insert(Cand {
            id: None,
            st: St::New,
            values: Vec::new(),
        });
    }
    let mut res = LookupResult::default();
    let mut inflight = FuturesUnordered::new();
    let ports_of = |cands: &HashMap<SocketAddrV4, Cand>| {
        let mut m: HashMap<Ipv4Addr, usize> = HashMap::new();
        for a in cands.keys() {
            *m.entry(*a.ip()).or_default() += 1;
        }
        m
    };
    let suspicious = |ports: &HashMap<Ipv4Addr, usize>, a: &SocketAddrV4, id: Option<Id>| {
        ports.get(a.ip()).copied().unwrap_or(0) >= FARM_PORTS
            || id.is_some_and(|id| common_prefix_bits(&id, &target) >= FARM_PREFIX_BITS)
    };
    loop {
        let window: Vec<SocketAddrV4> = {
            let ports = ports_of(&cands);
            let mut live: Vec<([u8; 20], SocketAddrV4)> = cands
                .iter()
                .filter(|(a, c)| c.st != St::Failed && !suspicious(&ports, a, c.id))
                .map(|(a, c)| (c.id.map_or([0xff; 20], |id| distance(&id, &target)), *a))
                .collect();
            live.sort_unstable();
            live.into_iter().take(WIDTH).map(|(_, a)| a).collect()
        };
        let peers_enough = want_peers > 0 && distinct_peers(&cands) >= want_peers;
        let closest_done = !window.is_empty()
            && window
                .iter()
                .take(DONE)
                .all(|a| cands[a].st == St::Answered);
        if peers_enough || closest_done || Instant::now() >= deadline || dht.is_stopped() {
            break;
        }
        for a in &window {
            if inflight.len() >= ALPHA {
                break;
            }
            let c = cands.get_mut(a).expect("window from cands");
            if c.st != St::New {
                continue;
            }
            c.st = St::InFlight;
            res.queried += 1;
            let q = match method {
                Method::FindNode => Query::FindNode { target },
                Method::GetPeers => Query::GetPeers { info_hash: target },
            };
            let a = *a;
            inflight.push(async move { (a, dht.query(node, a, q).await) });
        }
        if inflight.is_empty() {
            break;
        }
        let next = tokio::time::timeout_at(deadline, inflight.next()).await;
        let Ok(Some((addr, r))) = next else { break };
        let r: Result<Response, QueryError> = r;
        match r {
            Ok(r) => {
                res.answered += 1;
                if let Some(c) = cands.get_mut(&addr) {
                    c.st = St::Answered;
                    c.id = Some(r.id);
                    c.values = r.values;
                }
                dht.learn_nodes(&r.nodes, &target);
                for n in r.nodes {
                    if cands.len() >= MAX_CANDIDATES {
                        break;
                    }
                    cands.entry(n.addr).or_insert(Cand {
                        id: Some(n.id),
                        st: St::New,
                        values: Vec::new(),
                    });
                }
            }
            Err(_) => {
                if let Some(c) = cands.get_mut(&addr) {
                    c.st = St::Failed;
                }
            }
        }
    }
    let mut peers: Vec<SocketAddrV4> = Vec::new();
    let mut seen = HashSet::new();
    let ports = ports_of(&cands);
    for (a, c) in &cands {
        if suspicious(&ports, a, c.id) {
            continue;
        }
        for p in &c.values {
            if seen.insert(*p) {
                peers.push(*p);
            }
        }
    }
    let mut answered: Vec<CompactNode> = cands
        .iter()
        .filter(|(_, c)| c.st == St::Answered)
        .filter_map(|(a, c)| c.id.map(|id| CompactNode { id, addr: *a }))
        .collect();
    answered.sort_by_key(|n| distance(&n.id, &target));
    answered.truncate(K_RESULT);
    res.closest = answered;
    res.peers = peers;
    res
}

const K_RESULT: usize = 8;

fn distinct_peers(cands: &HashMap<SocketAddrV4, Cand>) -> usize {
    let set: HashSet<&SocketAddrV4> = cands.values().flat_map(|c| c.values.iter()).collect();
    set.len()
}

/// Peers found for one infohash, and how.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Collected {
    pub peers: Vec<SocketAddrV4>,
    /// Peers came from the node that sampled the hash.
    pub from_source: bool,
    /// An iterative lookup ran.
    pub looked_up: bool,
    pub queries: u32,
}

/// Collect peers for `info_hash`: the announcer when known, then a `get_peers` to the node that
/// sampled the hash (it stores peers for exactly that hash), then an iterative lookup bounded by
/// `budget` when that gave nothing. Peers on non-global addresses are dropped (the fetcher must
/// never be pointed at the LAN or loopback, docs/DHT.md §3.8).
pub async fn collect_peers(
    dht: &Dht,
    info_hash: Id,
    announcer: Option<SocketAddrV4>,
    source: Option<SocketAddrV4>,
    budget: Duration,
    want: usize,
) -> Collected {
    let mut out = Collected::default();
    let mut seen = HashSet::new();
    let allow = |p: &SocketAddrV4| dht.allows(p);
    let mut push = |out: &mut Collected, p: SocketAddrV4| {
        if allow(&p) && seen.insert(p) && out.peers.len() < 200 {
            out.peers.push(p);
        }
    };
    if let Some(p) = announcer {
        push(&mut out, p);
        return out;
    }
    let node = dht.next_node();
    if let Some(src) = source {
        out.queries += 1;
        if let Ok(r) = dht.query(node, src, Query::GetPeers { info_hash }).await {
            for p in r.values {
                push(&mut out, p);
            }
            out.from_source = !out.peers.is_empty();
            dht.learn_nodes(&r.nodes, &info_hash);
        }
    }
    if out.peers.is_empty() && budget > Duration::ZERO {
        out.looked_up = true;
        let seeds = dht.closest_known(&info_hash, WIDTH);
        let extra = if seeds.len() < DONE {
            dht.bootstrap_addrs().to_vec()
        } else {
            Vec::new()
        };
        let r = lookup(
            dht,
            node,
            info_hash,
            Method::GetPeers,
            seeds,
            extra,
            budget,
            want,
        )
        .await;
        out.queries += r.queried;
        for p in r.peers {
            push(&mut out, p);
        }
    }
    out
}

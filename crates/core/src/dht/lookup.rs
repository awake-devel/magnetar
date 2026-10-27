//! Iterative lookups (BEP 5 `find_node` / `get_peers`) through the crawler's own sockets and rate
//! limits, and peer collection for the metadata stage (docs/DHT.md §3.5, §4 step 1).
//!
//! Generalised from `liveness::dht::walk`: the walk keeps the 16 closest candidates, has 4
//! queries in flight, and stops once the 8 closest known nodes answered, when the budget runs
//! out, or (for `get_peers`) once enough peers are known. Node-farm rules (D45): ids sharing ≥ 64
//! bits with the target and IPs answering from ≥ 3 ports are not walked through and their
//! `values` are ignored.

use std::collections::{HashMap, HashSet, VecDeque};
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
/// Queries a lookup has out at once, slow ones included (D76): widening never piles more than
/// this onto the global pace queue.
const MAX_OUT: usize = 2 * ALPHA;
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
    /// No answer within the lookup timeout: no longer waited for (its slot goes to the next
    /// candidate, and it does not hold up the end of the walk), but a late answer is taken.
    Slow,
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
    /// Queries that went slow, and slow ones that still answered.
    pub slow: u32,
    pub late: u32,
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
    let mut res = LookupResult::default();
    lookup_into(
        dht, node, target, method, seeds, extra, budget, want_peers, &mut res,
    )
    .await;
    res
}

/// Records a `get_peers` lookup's wall time when it ends, also when it is dropped mid-walk.
struct Meter<'a> {
    dht: &'a Dht,
    started: Instant,
    on: bool,
}

impl Drop for Meter<'_> {
    fn drop(&mut self) {
        if self.on {
            self.dht.note_lookup(self.started.elapsed());
        }
    }
}

/// [`lookup`] writing into `res` as it goes, so a caller that drops the walk early (the
/// sampling node answered after all) still has its counts.
///
/// Branch widening (D76, like libtorrent's short timeout): at most [`ALPHA`] queries are
/// *waited for* at once. A query unanswered after `Dht::lookup_timeout` (1.5 s by default) turns
/// slow: it frees its slot, so the next candidate is asked, and the walk no longer waits for it
/// to finish; its answer is still used if it comes before the hard query timeout (4 s). Answers
/// arrive within 1 s 99.75 % of the time, so a silent node (35–52 % of them) used to hold a
/// slot for 4 s and most walks ran their whole 10 s budget.
#[allow(clippy::too_many_arguments)]
pub async fn lookup_into(
    dht: &Dht,
    node: usize,
    target: Id,
    method: Method,
    seeds: Vec<CompactNode>,
    extra: Vec<SocketAddrV4>,
    budget: Duration,
    want_peers: usize,
    res: &mut LookupResult,
) -> usize {
    let started = Instant::now();
    let _meter = Meter {
        dht,
        started,
        on: method == Method::GetPeers,
    };
    let deadline = started + budget;
    let soft = dht.lookup_timeout();
    let mut cands: HashMap<SocketAddrV4, Cand> = HashMap::new();
    // Candidates per IP, kept as candidates are added (the node-farm port rule).
    let mut ports: HashMap<Ipv4Addr, usize> = HashMap::new();
    let add = |cands: &mut HashMap<SocketAddrV4, Cand>,
               ports: &mut HashMap<Ipv4Addr, usize>,
               a: SocketAddrV4,
               id: Option<Id>| {
        if let std::collections::hash_map::Entry::Vacant(v) = cands.entry(a) {
            v.insert(Cand {
                id,
                st: St::New,
                values: Vec::new(),
            });
            *ports.entry(*a.ip()).or_default() += 1;
        }
    };
    for s in seeds {
        add(&mut cands, &mut ports, s.addr, Some(s.id));
    }
    for a in extra {
        add(&mut cands, &mut ports, a, None);
    }
    let suspicious = |ports: &HashMap<Ipv4Addr, usize>, a: &SocketAddrV4, id: Option<Id>| {
        ports.get(a.ip()).copied().unwrap_or(0) >= FARM_PORTS
            || id.is_some_and(|id| common_prefix_bits(&id, &target) >= FARM_PREFIX_BITS)
    };
    let mut inflight = FuturesUnordered::new();
    // Sends report here: the soft timeout runs from the datagram leaving, not from the query
    // joining the global pace queue (with many lookups that queue alone can take seconds).
    let (sent_tx, mut sent_rx) = tokio::sync::mpsc::unbounded_channel::<SocketAddrV4>();
    // Queries waited for (in flight and not slow), and when each turns slow (send order, so
    // the front is always the next one due).
    let mut waiting = 0usize;
    let mut due: VecDeque<(Instant, SocketAddrV4)> = VecDeque::new();
    // Usable peers so far (non-farm answerers, addresses we may connect to).
    let mut good: HashSet<SocketAddrV4> = HashSet::new();
    loop {
        let window: Vec<SocketAddrV4> = {
            let mut live: Vec<([u8; 20], SocketAddrV4)> = cands
                .iter()
                .filter(|(a, c)| {
                    !matches!(c.st, St::Failed | St::Slow) && !suspicious(&ports, a, c.id)
                })
                .map(|(a, c)| (c.id.map_or([0xff; 20], |id| distance(&id, &target)), *a))
                .collect();
            if live.len() > WIDTH {
                live.select_nth_unstable(WIDTH);
                live.truncate(WIDTH);
            }
            live.sort_unstable();
            live.into_iter().map(|(_, a)| a).collect()
        };
        let peers_enough = want_peers > 0 && good.len() >= want_peers;
        let closest_done = !window.is_empty()
            && window
                .iter()
                .take(DONE)
                .all(|a| cands[a].st == St::Answered);
        if peers_enough || closest_done || Instant::now() >= deadline || dht.is_stopped() {
            break;
        }
        for a in &window {
            if waiting >= ALPHA || inflight.len() >= MAX_OUT {
                break;
            }
            let c = cands.get_mut(a).expect("window from cands");
            if c.st != St::New {
                continue;
            }
            c.st = St::InFlight;
            waiting += 1;
            res.queried += 1;
            if method == Method::GetPeers {
                dht.note_lookup_query();
            }
            let q = match method {
                Method::FindNode => Query::FindNode { target },
                Method::GetPeers => Query::GetPeers { info_hash: target },
            };
            let a = *a;
            let tx = sent_tx.clone();
            inflight.push(async move {
                let r = dht
                    .query_then(node, a, q, move || {
                        let _ = tx.send(a);
                    })
                    .await;
                (a, r)
            });
        }
        if inflight.is_empty() {
            break;
        }
        while due
            .front()
            .is_some_and(|(_, a)| cands.get(a).is_none_or(|c| c.st != St::InFlight))
        {
            due.pop_front();
        }
        let wake = due.front().map_or(deadline, |(t, _)| (*t).min(deadline));
        let next = tokio::select! {
            Some(a) = sent_rx.recv() => {
                due.push_back((Instant::now() + soft, a));
                continue;
            }
            n = inflight.next() => n,
            _ = tokio::time::sleep_until(wake) => {
                let now = Instant::now();
                while let Some(&(t, a)) = due.front() {
                    if t > now {
                        break;
                    }
                    due.pop_front();
                    if let Some(c) = cands.get_mut(&a)
                        && c.st == St::InFlight
                    {
                        c.st = St::Slow;
                        waiting -= 1;
                        res.slow += 1;
                        if method == Method::GetPeers {
                            dht.note_lookup_slow();
                        }
                    }
                }
                continue;
            }
        };
        let Some((addr, r)) = next else { break };
        let r: Result<Response, QueryError> = r;
        let was = cands.get(&addr).map(|c| c.st);
        if was == Some(St::InFlight) {
            waiting -= 1;
        }
        match r {
            Ok(r) => {
                res.answered += 1;
                if was == Some(St::Slow) {
                    res.late += 1;
                    if method == Method::GetPeers {
                        dht.note_lookup_late();
                    }
                }
                let mut values = r.values;
                if let Some(c) = cands.get_mut(&addr) {
                    c.st = St::Answered;
                    c.id = Some(r.id);
                }
                if !values.is_empty() && !suspicious(&ports, &addr, Some(r.id)) {
                    let mut usable = values.clone();
                    dht.retain_allowed(&mut usable);
                    good.extend(usable);
                }
                if let Some(c) = cands.get_mut(&addr) {
                    c.values = std::mem::take(&mut values);
                }
                dht.learn_nodes(&r.nodes, &target);
                for n in r.nodes {
                    if cands.len() >= MAX_CANDIDATES {
                        break;
                    }
                    add(&mut cands, &mut ports, n.addr, Some(n.id));
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
    res.peers.len()
}

const K_RESULT: usize = 8;

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
///
/// The sampling node gets the lookup timeout too (D76): when it has not answered by then the
/// lookup starts, and a late answer with peers still wins (the lookup is dropped then).
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
    let mut pending = None;
    if let Some(src) = source {
        out.queries += 1;
        let (sent_tx, sent_rx) = tokio::sync::oneshot::channel::<()>();
        let mut q = Box::pin(
            dht.query_then(node, src, Query::GetPeers { info_hash }, move || {
                let _ = sent_tx.send(());
            }),
        );
        // The lookup timeout runs from the send (after the global pace wait).
        let soft = dht.lookup_timeout();
        let first = tokio::select! {
            r = &mut q => Ok(r),
            _ = async {
                match sent_rx.await {
                    Ok(()) => tokio::time::sleep(soft).await,
                    Err(_) => std::future::pending().await, // not sent: q ends at once
                }
            } => Err(()),
        };
        match first {
            Ok(Ok(r)) => {
                for p in r.values {
                    push(&mut out, p);
                }
                out.from_source = !out.peers.is_empty();
                dht.learn_nodes(&r.nodes, &info_hash);
            }
            Ok(Err(_)) => {}
            Err(_) => pending = Some(q),
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
        let mut lr = LookupResult::default();
        let mut late: Option<Response> = None;
        {
            let walk = lookup_into(
                dht,
                node,
                info_hash,
                Method::GetPeers,
                seeds,
                extra,
                budget,
                want,
                &mut lr,
            );
            tokio::pin!(walk);
            let mut walked = false;
            loop {
                tokio::select! {
                    r = async { pending.as_mut().expect("guarded").await }, if pending.is_some() => {
                        pending = None;
                        if let Ok(r) = r {
                            dht.learn_nodes(&r.nodes, &info_hash);
                            let mut v = r.values.clone();
                            dht.retain_allowed(&mut v);
                            if !v.is_empty() {
                                late = Some(r);
                                break; // the walk is dropped: the sampling node had peers
                            }
                        }
                        if walked {
                            break;
                        }
                    }
                    found = &mut walk, if !walked => {
                        walked = true;
                        // A walk that found nothing still waits for the sampling node.
                        if found > 0 || pending.is_none() {
                            break;
                        }
                    }
                }
            }
        }
        out.queries += lr.queried;
        match late {
            Some(r) => {
                for p in r.values {
                    push(&mut out, p);
                }
                out.from_source = !out.peers.is_empty();
            }
            None => {
                for p in std::mem::take(&mut lr.peers) {
                    push(&mut out, p);
                }
            }
        }
    }
    out
}

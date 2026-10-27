//! Iterative lookups (BEP 5 `find_node` / `get_peers`) through the crawler's own sockets and rate
//! limits, and peer collection for the metadata stage (docs/DHT.md §3.5, §4 step 1).
//!
//! Generalised from `liveness::dht::walk`: the walk keeps the 16 closest candidates, has 4
//! queries in flight, and stops once the 8 closest known nodes answered, when the budget runs
//! out, or (for `get_peers`) once enough peers are known. Node-farm rules (D45): ids sharing ≥ 64
//! bits with the target and IPs answering from ≥ 3 ports are not walked through and their
//! `values` are ignored.
//!
//! Responsiveness (D95): seeds are ranked by [`rank_seeds`] (nodes that answered recently first),
//! within the walk's window candidates are asked in [`Tier`] order (then by distance), and
//! contacts the pool dropped as dead (never answered, silent twice) are not walked at all.

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
use super::pool::Tier;

const WIDTH: usize = 16;
const DONE: usize = 8;
const ALPHA: usize = 4;
/// Queries a lookup has out at once, slow ones included (D93): widening never piles more than
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
    tier: Tier,
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
    /// Contacts not walked because the pool dropped them as dead (D95).
    pub dead_skipped: u32,
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
/// Branch widening (D93, like libtorrent's short timeout): at most [`ALPHA`] queries are
/// *waited for* at once. A query unanswered after `Dht::lookup_timeout` (4 s by default; 1.5 s widens the walk) turns
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
    // Dead contacts are not added (D95); returns 1 when one was skipped.
    let add = |cands: &mut HashMap<SocketAddrV4, Cand>,
               ports: &mut HashMap<Ipv4Addr, usize>,
               a: SocketAddrV4,
               id: Option<Id>,
               tier: Tier|
     -> u64 {
        if tier == Tier::Dead {
            return u64::from(!cands.contains_key(&a));
        }
        if let std::collections::hash_map::Entry::Vacant(v) = cands.entry(a) {
            v.insert(Cand {
                id,
                st: St::New,
                tier,
                values: Vec::new(),
            });
            *ports.entry(*a.ip()).or_default() += 1;
        }
        0
    };
    let mut dead = 0u64;
    let first: Vec<(SocketAddrV4, Option<Id>)> = seeds
        .iter()
        .map(|s| (s.addr, Some(s.id)))
        .chain(extra.iter().map(|a| (*a, None)))
        .collect();
    let addrs: Vec<SocketAddrV4> = first.iter().map(|(a, _)| *a).collect();
    for ((a, id), t) in first.into_iter().zip(dht.tiers_of(&addrs)) {
        dead += add(&mut cands, &mut ports, a, id, t);
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
        // Within the window, contacts we may query now and likely to answer go first (D95);
        // the window itself and the end test stay by distance.
        let fresh: Vec<SocketAddrV4> = window
            .iter()
            .filter(|a| cands[*a].st == St::New)
            .copied()
            .collect();
        let limited = dht.limited(&fresh);
        let mut order: Vec<(bool, Tier, SocketAddrV4)> = fresh
            .iter()
            .zip(limited)
            .map(|(a, l)| (l, cands[a].tier, *a))
            .collect();
        order.sort_by_key(|(l, t, _)| (*l, *t)); // stable: distance order within
        let order: Vec<SocketAddrV4> = order.into_iter().map(|(_, _, a)| a).collect();
        for a in &order {
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
                if method == Method::GetPeers {
                    dht.note_lookup_answered();
                }
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
                let tiers = dht.learn_ranked(&r.nodes, &target);
                for (n, t) in r.nodes.into_iter().zip(tiers) {
                    if cands.len() >= MAX_CANDIDATES {
                        break;
                    }
                    dead += add(&mut cands, &mut ports, n.addr, Some(n.id), t);
                }
            }
            Err(_) => {
                if let Some(c) = cands.get_mut(&addr) {
                    c.st = St::Failed;
                }
            }
        }
    }
    dht.note_dead_skipped(dead);
    res.dead_skipped += dead as u32;
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

/// A lookup seed candidate (D95).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SeedCand {
    pub node: CompactNode,
    pub tier: Tier,
    /// Our per-IP limit (D54) has no token for its IP right now: a query would be refused.
    pub limited: bool,
}

/// Pick `k` lookup seeds (D95): dead ones never, one per address (its best tier), then
/// contacts we may query right now before rate-limited ones, then by tier (answered recently,
/// untried, silent since), and within that by distance to `target`. The candidates are the
/// closest few dozen known contacts, so a responsive seed is at most a couple of prefix bits
/// further out than the nearest one, and the walk regains that in its first round; an
/// unanswered seed costs a query of the budget. The per-IP key matters: ranking by tier alone
/// sent the concurrent lookups to the same few responsive nodes, and the 1 q/s per-IP limit
/// refused 3× more queries than were sent (first live run, §19).
pub fn rank_seeds(mut cands: Vec<SeedCand>, target: &Id, k: usize) -> Vec<SeedCand> {
    cands.retain(|c| c.tier != Tier::Dead);
    cands.sort_by_key(|c| (c.node.addr, c.tier));
    cands.dedup_by_key(|c| c.node.addr);
    cands.sort_by_key(|c| (c.limited, c.tier, distance(&c.node.id, target), c.node.addr));
    cands.truncate(k);
    cands
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
///
/// The sampling node gets the lookup timeout too (D93): when it has not answered by then the
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
        let seeds: Vec<CompactNode> = dht
            .lookup_seeds(&info_hash, WIDTH)
            .into_iter()
            .map(|c| c.node)
            .collect();
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

#[cfg(test)]
mod tests {
    use super::*;

    fn cn(id: Id, i: u32) -> CompactNode {
        CompactNode {
            id,
            addr: SocketAddrV4::new(Ipv4Addr::from(0x0200_0000 + i), 6881),
        }
    }

    fn sc(node: CompactNode, tier: Tier, limited: bool) -> SeedCand {
        SeedCand {
            node,
            tier,
            limited,
        }
    }

    /// An id sharing `bits` leading bits with `t`.
    fn near(t: &Id, bits: usize) -> Id {
        let mut id = *t;
        id[bits / 8] ^= 0x80 >> (bits % 8);
        id
    }

    #[test]
    fn seeds_rank_by_tier_then_distance() {
        let t: Id = rand::random();
        let cands = vec![
            sc(cn(near(&t, 30), 1), Tier::Unknown, false),
            sc(cn(near(&t, 40), 2), Tier::Struck, false),
            sc(cn(near(&t, 50), 3), Tier::Dead, false),
            sc(cn(near(&t, 10), 4), Tier::Fresh, false),
            sc(cn(near(&t, 20), 5), Tier::Fresh, false),
            sc(cn(near(&t, 25), 6), Tier::Unknown, false),
            sc(cn(near(&t, 60), 7), Tier::Fresh, true),
        ];
        let got: Vec<(u32, Tier)> = rank_seeds(cands.clone(), &t, 16)
            .into_iter()
            .map(|c| (u32::from(*c.node.addr.ip()) - 0x0200_0000, c.tier))
            .collect();
        assert_eq!(
            got,
            vec![
                (5, Tier::Fresh),
                (4, Tier::Fresh),
                (1, Tier::Unknown),
                (6, Tier::Unknown),
                (2, Tier::Struck),
                (7, Tier::Fresh),
            ],
            "queryable first, then fresh (nearest first), dead never"
        );
        assert_eq!(rank_seeds(cands, &t, 2).len(), 2);
    }

    #[test]
    fn a_contact_listed_twice_keeps_its_best_tier() {
        let t: Id = rand::random();
        let a = cn(near(&t, 30), 1);
        let b = cn(near(&t, 31), 2);
        let got = rank_seeds(
            vec![
                sc(a, Tier::Struck, false),
                sc(b, Tier::Unknown, false),
                sc(a, Tier::Fresh, false),
            ],
            &t,
            16,
        );
        assert_eq!(
            got,
            vec![sc(a, Tier::Fresh, false), sc(b, Tier::Unknown, false)]
        );
        assert!(rank_seeds(Vec::new(), &t, 16).is_empty());
    }
}

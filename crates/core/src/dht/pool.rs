//! The crawl pool (docs/DHT.md §3.5, D55): nodes to sample with BEP 51, separate from the
//! routing tables, keyed by address, bounded.
//!
//! - New nodes are due at once. A node that answers with `samples` is due again after its
//!   `interval` clamped to [300, 21600] s (BEP 51 range, 0 → 300 so nobody makes us hot-loop).
//! - A node answering without `samples` (or with error 204) is `bep51 = no`, kept, re-probed after
//!   24 h, never evicted for that alone.
//! - A node silent 3 times in a row is dropped; at most 2 entries per IP.
//! - When full, a new node replaces the worst of 8 random entries (most failures, then
//!   non-supporters), never one with a query in flight.
//! - Responsiveness for lookups (D95): every query of ours to a pool node updates `heard` (last
//!   answer of any kind) and `silent` (unanswered queries in a row). A node that never answered
//!   and stayed silent [`DEAD_AFTER`] times is dropped and remembered in a bounded FIFO set, so
//!   answers that name it again do not bring it back and lookups skip it ([`Tier`]).
//! - A 16-bit id-prefix index gives lookups seeds close to any target (D61): the routing tables
//!   hold ~100 contacts per virtual node, nearly all far from a random infohash, so lookups seeded
//!   from them alone all start at the same few nodes and hit the per-IP limit.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet, VecDeque};
use std::net::{Ipv4Addr, SocketAddrV4};
use std::time::Duration;

use rand::Rng;
use tokio::time::Instant;

use super::id::NodeId;
use super::krpc::CompactNode;

pub const INTERVAL_MIN: i64 = 300;
pub const INTERVAL_MAX: i64 = 21_600;
pub const NO_BEP51_RETRY: Duration = Duration::from_secs(24 * 3600);
const FAILURE_RETRY: Duration = Duration::from_secs(300);
const DROP_AFTER_FAILURES: u8 = 3;
pub const PER_IP: u8 = 2;
const EVICTION_SAMPLE: usize = 8;
/// A node that never answered is dropped after this many unanswered queries in a row (D95).
/// Re-asked nodes answer 94–97 % of the time (docs/DHT.md §18.2), so two silences from a node
/// that never answered leave well under a 1 % chance that it is alive.
pub const DEAD_AFTER: u8 = 2;
/// An answer this recent makes a node [`Tier::Fresh`] (the BEP 5 "good" window).
pub const FRESH_FOR: Duration = Duration::from_secs(15 * 60);
/// Dropped never-answering addresses remembered (FIFO), bounds: a quarter of the pool cap.
const DEAD_MIN: usize = 1024;
const DEAD_MAX: usize = 262_144;

/// How likely a contact is to answer a lookup query, best first (D95).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Tier {
    /// Answered within [`FRESH_FOR`] and not silent since.
    Fresh,
    /// Never asked, or answered long ago and not silent since.
    Unknown,
    /// Silent at least once since its last answer (or since it was learnt).
    Struck,
    /// Never answered, silent [`DEAD_AFTER`] times: dropped, not asked by lookups.
    Dead,
}

/// Tier from an answer time and a silence count.
pub fn tier(heard: Option<Instant>, silent: u8, now: Instant) -> Tier {
    if silent >= DEAD_AFTER && heard.is_none() {
        Tier::Dead
    } else if silent > 0 {
        Tier::Struck
    } else if heard.is_some_and(|t| now.saturating_duration_since(t) < FRESH_FOR) {
        Tier::Fresh
    } else {
        Tier::Unknown
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bep51 {
    Unknown,
    Yes,
    No,
}

#[derive(Clone, Debug)]
pub struct PoolEntry {
    pub node: CompactNode,
    pub bep51: Bep51,
    pub next_sample_at: Instant,
    pub failures: u8,
    pub last_answer: Option<Instant>,
    pub in_flight: bool,
    /// When the node entered the pool (answer share by node age, D93).
    pub added: Instant,
    /// Last answer (response or error reply) to any query of ours, and unanswered queries in
    /// a row since (D95).
    pub heard: Option<Instant>,
    pub silent: u8,
}

impl PoolEntry {
    pub fn tier(&self, now: Instant) -> Tier {
        tier(self.heard, self.silent, now)
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct PoolCounts {
    pub size: usize,
    pub bep51_yes: usize,
    pub bep51_no: usize,
    pub bep51_unknown: usize,
    pub evicted: u64,
    pub dropped_silent: u64,
    /// Dropped after never answering [`DEAD_AFTER`] queries (D95), and how many such addresses
    /// are remembered now.
    pub dropped_dead: u64,
    pub dead_known: usize,
}

/// Clamp a BEP 51 `interval` (absent → the minimum).
pub fn clamp_interval(interval: Option<i64>) -> Duration {
    Duration::from_secs(
        interval
            .unwrap_or(INTERVAL_MIN)
            .clamp(INTERVAL_MIN, INTERVAL_MAX) as u64,
    )
}

#[derive(Debug)]
pub struct Pool {
    cap: usize,
    slots: Vec<PoolEntry>,
    index: HashMap<SocketAddrV4, usize>,
    per_ip: HashMap<Ipv4Addr, u8>,
    due: BinaryHeap<Reverse<(Instant, SocketAddrV4)>>,
    /// Addresses by the first 16 bits of their id (65 536 buckets, ~8 B per entry).
    by_prefix: Vec<Vec<SocketAddrV4>>,
    evicted: u64,
    dropped_silent: u64,
    dropped_dead: u64,
    dead: DeadSet,
}

/// Bounded FIFO set of addresses dropped as dead.
#[derive(Debug)]
struct DeadSet {
    cap: usize,
    set: HashSet<SocketAddrV4>,
    order: VecDeque<SocketAddrV4>,
}

impl DeadSet {
    fn insert(&mut self, a: SocketAddrV4) {
        if self.set.insert(a) {
            self.order.push_back(a);
            while self.order.len() > self.cap {
                if let Some(old) = self.order.pop_front() {
                    self.set.remove(&old);
                }
            }
        }
    }

    /// Forget `a` (it answered after all); its FIFO slot ages out on its own.
    fn remove(&mut self, a: &SocketAddrV4) {
        self.set.remove(a);
    }
}

fn prefix16(id: &NodeId) -> usize {
    usize::from(u16::from_be_bytes([id[0], id[1]]))
}

impl Pool {
    pub fn new(cap: usize) -> Self {
        Self {
            cap: cap.max(1),
            slots: Vec::new(),
            index: HashMap::new(),
            per_ip: HashMap::new(),
            due: BinaryHeap::new(),
            by_prefix: vec![Vec::new(); 1 << 16],
            evicted: 0,
            dropped_silent: 0,
            dropped_dead: 0,
            dead: DeadSet {
                cap: (cap / 4).clamp(DEAD_MIN, DEAD_MAX),
                set: HashSet::new(),
                order: VecDeque::new(),
            },
        }
    }

    pub fn len(&self) -> usize {
        self.slots.len()
    }

    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    pub fn get(&self, addr: &SocketAddrV4) -> Option<&PoolEntry> {
        self.index.get(addr).map(|&i| &self.slots[i])
    }

    pub fn counts(&self) -> PoolCounts {
        let mut c = PoolCounts {
            size: self.slots.len(),
            evicted: self.evicted,
            dropped_silent: self.dropped_silent,
            dropped_dead: self.dropped_dead,
            dead_known: self.dead.set.len(),
            ..Default::default()
        };
        for e in &self.slots {
            match e.bep51 {
                Bep51::Yes => c.bep51_yes += 1,
                Bep51::No => c.bep51_no += 1,
                Bep51::Unknown => c.bep51_unknown += 1,
            }
        }
        c
    }

    fn schedule(&mut self, i: usize, at: Instant) {
        self.slots[i].next_sample_at = at;
        let addr = self.slots[i].node.addr;
        self.due.push(Reverse((at, addr)));
        // Stale heap entries are skipped on pop; rebuild when they dominate.
        if self.due.len() > 4 * self.slots.len() + 1024 {
            self.due = self
                .slots
                .iter()
                .filter(|e| !e.in_flight)
                .map(|e| Reverse((e.next_sample_at, e.node.addr)))
                .collect();
        }
    }

    fn remove_at(&mut self, i: usize) -> PoolEntry {
        let e = self.slots.swap_remove(i);
        self.index.remove(&e.node.addr);
        self.unindex(&e.node);
        if let Some(moved) = self.slots.get(i) {
            self.index.insert(moved.node.addr, i);
        }
        let ip = *e.node.addr.ip();
        if let Some(n) = self.per_ip.get_mut(&ip) {
            *n -= 1;
            if *n == 0 {
                self.per_ip.remove(&ip);
            }
        }
        e
    }

    /// Add a node (the caller has checked the address). Returns true when it is new.
    pub fn add(&mut self, node: CompactNode, now: Instant) -> bool {
        if let Some(&i) = self.index.get(&node.addr) {
            self.set_id(i, node.id);
            return false;
        }
        if self.per_ip.get(node.addr.ip()).copied().unwrap_or(0) >= PER_IP
            || self.dead.set.contains(&node.addr)
        {
            return false;
        }
        if self.slots.len() >= self.cap && !self.evict() {
            return false;
        }
        let i = self.slots.len();
        self.slots.push(PoolEntry {
            node,
            bep51: Bep51::Unknown,
            next_sample_at: now,
            failures: 0,
            last_answer: None,
            in_flight: false,
            added: now,
            heard: None,
            silent: 0,
        });
        self.index.insert(node.addr, i);
        self.by_prefix[prefix16(&node.id)].push(node.addr);
        *self.per_ip.entry(*node.addr.ip()).or_default() += 1;
        self.schedule(i, now);
        true
    }

    fn evict(&mut self) -> bool {
        let mut rng = rand::rng();
        let worst = (0..EVICTION_SAMPLE)
            .map(|_| rng.random_range(0..self.slots.len()))
            .filter(|&i| !self.slots[i].in_flight)
            .max_by_key(|&i| {
                let e = &self.slots[i];
                (
                    e.failures.max(e.silent),
                    e.bep51 == Bep51::No,
                    e.last_answer.is_none(),
                )
            });
        match worst {
            Some(i) => {
                self.remove_at(i);
                self.evicted += 1;
                true
            }
            None => false,
        }
    }

    /// The next due node, marked in flight. None when nothing is due at `now`.
    pub fn pop_due(&mut self, now: Instant) -> Option<PoolEntry> {
        while let Some(Reverse((at, addr))) = self.due.peek().copied() {
            if at > now {
                return None;
            }
            self.due.pop();
            let Some(&i) = self.index.get(&addr) else {
                continue;
            };
            let e = &mut self.slots[i];
            if e.in_flight || e.next_sample_at != at {
                continue;
            }
            e.in_flight = true;
            return Some(e.clone());
        }
        None
    }

    /// When the next node comes due (None: nothing scheduled).
    pub fn next_due(&self) -> Option<Instant> {
        self.due.peek().map(|Reverse((at, _))| *at)
    }

    /// The node answered a `sample_infohashes`; `samples` says whether the field was present.
    pub fn answered(
        &mut self,
        addr: &SocketAddrV4,
        id: NodeId,
        samples: bool,
        interval: Option<i64>,
        now: Instant,
    ) {
        let Some(&i) = self.index.get(addr) else {
            return;
        };
        self.set_id(i, id);
        let e = &mut self.slots[i];
        e.in_flight = false;
        e.failures = 0;
        e.last_answer = Some(now);
        let next = if samples {
            e.bep51 = Bep51::Yes;
            now + clamp_interval(interval)
        } else {
            e.bep51 = Bep51::No;
            now + NO_BEP51_RETRY
        };
        self.schedule(i, next);
    }

    /// Error 204 (method unknown) or another error answer: not a supporter.
    pub fn unsupported(&mut self, addr: &SocketAddrV4, now: Instant) {
        if let Some(&i) = self.index.get(addr) {
            let e = &mut self.slots[i];
            e.in_flight = false;
            e.failures = 0;
            e.last_answer = Some(now);
            e.bep51 = Bep51::No;
            self.schedule(i, now + NO_BEP51_RETRY);
        }
    }

    /// No answer. Returns true when the node was dropped (silent 3 times in a row).
    pub fn failed(&mut self, addr: &SocketAddrV4, now: Instant) -> bool {
        let Some(&i) = self.index.get(addr) else {
            return false;
        };
        let e = &mut self.slots[i];
        e.in_flight = false;
        e.failures = e.failures.saturating_add(1);
        if e.failures >= DROP_AFTER_FAILURES {
            self.remove_at(i);
            self.dropped_silent += 1;
            return true;
        }
        let back_off = FAILURE_RETRY * u32::from(self.slots[i].failures);
        self.schedule(i, now + back_off);
        false
    }

    /// Any answer of `addr` to a query of ours (response or error reply, D95).
    pub fn heard(&mut self, addr: &SocketAddrV4, now: Instant) {
        if let Some(&i) = self.index.get(addr) {
            let e = &mut self.slots[i];
            e.heard = Some(now);
            e.silent = 0;
        } else {
            self.dead.remove(addr);
        }
    }

    /// A query of ours to `addr` went unanswered (hard timeout, D95). Returns true when the
    /// node was dropped as dead (never answered, silent [`DEAD_AFTER`] times).
    pub fn silent(&mut self, addr: &SocketAddrV4) -> bool {
        let Some(&i) = self.index.get(addr) else {
            return false;
        };
        let e = &mut self.slots[i];
        e.silent = e.silent.saturating_add(1);
        if e.silent >= DEAD_AFTER && e.heard.is_none() {
            self.remove_at(i);
            self.dead.insert(*addr);
            self.dropped_dead += 1;
            return true;
        }
        false
    }

    /// The lookup tier of `addr`: its pool entry's, [`Tier::Dead`] when it was dropped as dead,
    /// None when the pool does not know it.
    pub fn tier_of(&self, addr: &SocketAddrV4, now: Instant) -> Option<Tier> {
        match self.index.get(addr) {
            Some(&i) => Some(self.slots[i].tier(now)),
            None => self.dead.set.contains(addr).then_some(Tier::Dead),
        }
    }

    /// Not sent (rate limit, filter, busy): try again at `at`, no failure counted.
    pub fn reschedule(&mut self, addr: &SocketAddrV4, at: Instant) {
        if let Some(&i) = self.index.get(addr) {
            self.slots[i].in_flight = false;
            self.schedule(i, at);
        }
    }

    pub fn remove(&mut self, addr: &SocketAddrV4) {
        if let Some(&i) = self.index.get(addr) {
            self.remove_at(i);
        }
    }

    fn unindex(&mut self, node: &CompactNode) {
        let b = &mut self.by_prefix[prefix16(&node.id)];
        if let Some(k) = b.iter().position(|a| *a == node.addr) {
            b.swap_remove(k);
        }
    }

    fn set_id(&mut self, i: usize, id: NodeId) {
        let old = self.slots[i].node;
        if old.id != id {
            self.unindex(&old);
            self.slots[i].node.id = id;
            self.by_prefix[prefix16(&id)].push(old.addr);
        }
    }

    /// Up to `k` entries closest to `target` (XOR), from the buckets sharing the longest prefix
    /// with it: scans the target's 16-bit bucket, then the 2, 4, … buckets around it until `k`
    /// candidates are found.
    pub fn closest(&self, target: &NodeId, k: usize) -> Vec<CompactNode> {
        let p = prefix16(target);
        let mut out: Vec<CompactNode> = Vec::new();
        for shift in 0..=16 {
            out.clear();
            let lo = (p >> shift) << shift;
            for b in &self.by_prefix[lo..lo + (1 << shift)] {
                for a in b {
                    if let Some(&i) = self.index.get(a) {
                        out.push(self.slots[i].node);
                    }
                }
            }
            if out.len() >= k {
                break;
            }
        }
        out.sort_by_key(|n| super::id::distance(&n.id, target));
        out.truncate(k);
        out
    }

    /// [`Pool::closest`] with each entry's [`Tier`] (D95).
    pub fn closest_ranked(
        &self,
        target: &NodeId,
        k: usize,
        now: Instant,
    ) -> Vec<(CompactNode, Tier)> {
        self.closest(target, k)
            .into_iter()
            .map(|n| {
                let t = self.tier_of(&n.addr, now).unwrap_or(Tier::Unknown);
                (n, t)
            })
            .collect()
    }

    /// `n` random entries (to grow the pool with `find_node` when nothing is due).
    pub fn random(&self, n: usize) -> Vec<CompactNode> {
        if self.slots.is_empty() {
            return Vec::new();
        }
        let mut rng = rand::rng();
        (0..n)
            .map(|_| self.slots[rng.random_range(0..self.slots.len())].node)
            .collect()
    }

    /// Nodes that answered recently (persisted with the routing nodes between runs).
    pub fn answered_recently(&self, now: Instant, within: Duration, n: usize) -> Vec<CompactNode> {
        self.slots
            .iter()
            .filter(|e| {
                e.last_answer
                    .is_some_and(|t| now.saturating_duration_since(t) < within)
            })
            .take(n)
            .map(|e| e.node)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(i: u32, port: u16) -> CompactNode {
        CompactNode {
            id: rand::random(),
            addr: SocketAddrV4::new(Ipv4Addr::from(0x0200_0000 + i), port),
        }
    }

    #[test]
    fn due_order_interval_and_failures() {
        let now = Instant::now();
        let mut p = Pool::new(100);
        let a = n(1, 1);
        let b = n(2, 1);
        assert!(p.add(a, now));
        assert!(p.add(b, now));
        assert!(!p.add(a, now));
        let first = p.pop_due(now).unwrap();
        let second = p.pop_due(now).unwrap();
        assert!(p.pop_due(now).is_none(), "both in flight");
        // Answered with samples and interval 0 → clamped to 300 s.
        p.answered(&first.node.addr, first.node.id, true, Some(0), now);
        assert_eq!(
            p.get(&first.node.addr).unwrap().next_sample_at,
            now + Duration::from_secs(300)
        );
        // Interval above the BEP 51 maximum → 21600.
        p.answered(&first.node.addr, first.node.id, true, Some(999_999), now);
        assert_eq!(
            p.get(&first.node.addr).unwrap().next_sample_at,
            now + Duration::from_secs(21_600)
        );
        // No samples field: bep51 = no, re-probed after 24 h, kept.
        p.answered(&second.node.addr, second.node.id, false, None, now);
        assert_eq!(p.get(&second.node.addr).unwrap().bep51, Bep51::No);
        assert_eq!(p.len(), 2);
        assert!(p.pop_due(now + Duration::from_secs(3600)).is_none());
        assert!(p.pop_due(now + NO_BEP51_RETRY).is_some());
        // Three silences drop a node.
        let c = n(3, 1);
        p.add(c, now);
        let mut t = now;
        for k in 0..3 {
            let e = loop {
                let e = p.pop_due(t).unwrap();
                if e.node.addr == c.addr {
                    break e;
                }
                p.reschedule(&e.node.addr, t + NO_BEP51_RETRY * 10);
            };
            assert_eq!(p.failed(&e.node.addr, t), k == 2);
            t += FAILURE_RETRY * 3;
        }
        assert!(p.get(&c.addr).is_none());
        assert_eq!(p.counts().dropped_silent, 1);
    }

    #[test]
    fn two_per_ip_and_capped() {
        let now = Instant::now();
        let mut p = Pool::new(10);
        assert!(p.add(n(1, 1), now));
        assert!(p.add(n(1, 2), now));
        assert!(!p.add(n(1, 3), now));
        for i in 2..50 {
            p.add(n(i, 1), now);
        }
        assert_eq!(p.len(), 10);
        assert!(p.counts().evicted > 0);
        // All in flight: nothing can be evicted, the new node is refused.
        while p.pop_due(now).is_some() {}
        assert!(!p.add(n(999, 1), now));
        assert_eq!(p.len(), 10);
        assert_eq!(p.index.len(), 10);
        for (addr, &i) in &p.index {
            assert_eq!(p.slots[i].node.addr, *addr);
        }
    }

    #[test]
    fn closest_uses_the_prefix_index() {
        let now = Instant::now();
        let mut p = Pool::new(10_000);
        let mut all = Vec::new();
        for i in 0..5000 {
            let node = n(i, 1);
            p.add(node, now);
            all.push(node);
        }
        // Ids change on answers; the index follows.
        let moved = all[7];
        let mut new_id = moved.id;
        new_id[0] ^= 0x80;
        p.answered(&moved.addr, new_id, true, None, now);
        all[7].id = new_id;
        // Evictions/removals leave no stale entries.
        p.remove(&all[8].addr);
        all.remove(8);
        for _ in 0..50 {
            let t: NodeId = rand::random();
            let got = p.closest(&t, 8);
            let mut want = all.clone();
            want.sort_by_key(|n| super::super::id::distance(&n.id, &t));
            assert_eq!(got.len(), 8);
            assert_eq!(got[..], want[..8]);
        }
        let total: usize = p.by_prefix.iter().map(Vec::len).sum();
        assert_eq!(total, p.len());
        assert!(Pool::new(10).closest(&[0; 20], 8).is_empty());
    }

    #[test]
    fn tiers_follow_answers_and_silences() {
        let now = Instant::now();
        assert_eq!(tier(None, 0, now), Tier::Unknown);
        assert_eq!(tier(Some(now), 0, now), Tier::Fresh);
        assert_eq!(tier(Some(now), 0, now + FRESH_FOR), Tier::Unknown);
        assert_eq!(tier(Some(now), 1, now), Tier::Struck);
        assert_eq!(tier(None, 1, now), Tier::Struck);
        assert_eq!(tier(None, DEAD_AFTER, now), Tier::Dead);
        // A node that answered once is never dead, only struck.
        assert_eq!(tier(Some(now), 200, now), Tier::Struck);
    }

    #[test]
    fn never_answering_nodes_are_dropped_and_not_relearnt() {
        let now = Instant::now();
        let mut p = Pool::new(100);
        let (a, b, c) = (n(1, 1), n(2, 1), n(3, 1));
        for x in [a, b, c] {
            p.add(x, now);
        }
        assert_eq!(p.tier_of(&a.addr, now), Some(Tier::Unknown));
        // a: silent twice, never answered → dropped and remembered.
        assert!(!p.silent(&a.addr));
        assert_eq!(p.tier_of(&a.addr, now), Some(Tier::Struck));
        assert!(p.silent(&a.addr));
        assert!(p.get(&a.addr).is_none());
        assert_eq!(p.tier_of(&a.addr, now), Some(Tier::Dead));
        assert!(!p.add(a, now), "a dead address is not learnt again");
        // b answered once: silences only strike it.
        p.heard(&b.addr, now);
        assert_eq!(p.tier_of(&b.addr, now), Some(Tier::Fresh));
        for _ in 0..5 {
            assert!(!p.silent(&b.addr));
        }
        assert_eq!(p.tier_of(&b.addr, now), Some(Tier::Struck));
        p.heard(&b.addr, now);
        assert_eq!(p.tier_of(&b.addr, now), Some(Tier::Fresh));
        // Unknown to the pool: no tier; a dead address that answers after all is forgotten.
        assert_eq!(p.tier_of(&n(9, 9).addr, now), None);
        p.heard(&a.addr, now);
        assert_eq!(p.tier_of(&a.addr, now), None);
        assert!(p.add(a, now));
        let cs = p.counts();
        assert_eq!((cs.dropped_dead, cs.dead_known, cs.size), (1, 0, 3));
        // c untouched.
        assert_eq!(p.tier_of(&c.addr, now), Some(Tier::Unknown));
    }

    #[test]
    fn dead_set_is_bounded() {
        let now = Instant::now();
        let mut p = Pool::new(100);
        for i in 0..(DEAD_MIN as u32 + 500) {
            let x = n(i, 1);
            p.add(x, now);
            p.silent(&x.addr);
            p.silent(&x.addr);
        }
        assert_eq!(p.counts().dropped_dead, DEAD_MIN as u64 + 500);
        assert_eq!(p.dead.set.len(), DEAD_MIN);
        assert_eq!(p.dead.order.len(), DEAD_MIN);
        assert!(p.is_empty());
        // The oldest were forgotten, the newest are still known.
        assert_eq!(p.tier_of(&n(0, 1).addr, now), None);
        assert_eq!(
            p.tier_of(&n(DEAD_MIN as u32 + 499, 1).addr, now),
            Some(Tier::Dead)
        );
    }

    #[test]
    fn heap_stays_bounded() {
        let now = Instant::now();
        let mut p = Pool::new(10);
        let a = n(1, 1);
        p.add(a, now);
        for k in 0..10_000u64 {
            p.reschedule(&a.addr, now + Duration::from_millis(k));
        }
        assert!(p.due.len() <= 4 * p.len() + 1025);
        assert!(p.pop_due(now + Duration::from_secs(100)).is_some());
    }
}

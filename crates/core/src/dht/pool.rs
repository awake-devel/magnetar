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

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};
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
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct PoolCounts {
    pub size: usize,
    pub bep51_yes: usize,
    pub bep51_no: usize,
    pub bep51_unknown: usize,
    pub evicted: u64,
    pub dropped_silent: u64,
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
    evicted: u64,
    dropped_silent: u64,
}

impl Pool {
    pub fn new(cap: usize) -> Self {
        Self {
            cap: cap.max(1),
            slots: Vec::new(),
            index: HashMap::new(),
            per_ip: HashMap::new(),
            due: BinaryHeap::new(),
            evicted: 0,
            dropped_silent: 0,
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
            self.slots[i].node.id = node.id;
            return false;
        }
        if self.per_ip.get(node.addr.ip()).copied().unwrap_or(0) >= PER_IP {
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
        });
        self.index.insert(node.addr, i);
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
                (e.failures, e.bep51 == Bep51::No, e.last_answer.is_none())
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
        let e = &mut self.slots[i];
        e.in_flight = false;
        e.failures = 0;
        e.last_answer = Some(now);
        e.node.id = id;
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

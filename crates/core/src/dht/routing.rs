//! BEP 5 routing table of one virtual node (docs/DHT.md §3.5).
//!
//! Buckets are indexed by the number of leading bits a contact shares with our id (0–159). That
//! is exactly the table BEP 5 builds by "splitting only the bucket our id falls in": every split
//! leaves one bucket per prefix length on the far side. K = 8 per bucket. A node is **good** when
//! it answered one of our queries within 15 minutes, **questionable** after that, **bad** after 2
//! failed queries in a row. A full bucket takes a new node only in place of a bad one, or (for a
//! node that just answered) in place of a never-answered or questionable one. One entry per IP
//! (libtorrent's `restrict_routing_ips`).

use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddrV4};
use std::time::Duration;

use tokio::time::Instant;

use super::id::{NodeId, common_prefix_bits, distance};
use super::krpc::CompactNode;

pub const K: usize = 8;
pub const GOOD_FOR: Duration = Duration::from_secs(15 * 60);
const BAD_AFTER: u8 = 2;
const BUCKETS: usize = 160;

#[derive(Clone, Debug)]
pub struct Entry {
    pub node: CompactNode,
    pub last_answer: Option<Instant>,
    pub last_query_from: Option<Instant>,
    pub fails: u8,
}

impl Entry {
    pub fn is_good(&self, now: Instant) -> bool {
        self.fails == 0
            && self.last_answer.is_some()
            && (self
                .last_answer
                .is_some_and(|t| now.saturating_duration_since(t) < GOOD_FOR)
                || self
                    .last_query_from
                    .is_some_and(|t| now.saturating_duration_since(t) < GOOD_FOR))
    }

    pub fn is_bad(&self) -> bool {
        self.fails >= BAD_AFTER
    }
}

#[derive(Clone, Debug)]
struct Bucket {
    entries: Vec<Entry>,
    changed: Instant,
}

#[derive(Debug)]
pub struct RoutingTable {
    own: NodeId,
    buckets: Vec<Bucket>,
    ips: HashMap<Ipv4Addr, usize>,
}

/// What we learnt about a contact.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Seen {
    /// It answered one of our queries.
    Answered,
    /// It sent us a query (BEP 43: never called for `ro=1` senders).
    Queried,
}

impl RoutingTable {
    pub fn new(own: NodeId, now: Instant) -> Self {
        Self {
            own,
            buckets: vec![
                Bucket {
                    entries: Vec::new(),
                    changed: now,
                };
                BUCKETS
            ],
            ips: HashMap::new(),
        }
    }

    pub fn own_id(&self) -> NodeId {
        self.own
    }

    pub fn len(&self) -> usize {
        self.ips.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ips.is_empty()
    }

    fn bucket_of(&self, id: &NodeId) -> Option<usize> {
        let b = common_prefix_bits(&self.own, id) as usize;
        (b < BUCKETS).then_some(b)
    }

    /// Record a contact. Returns true when it is in the table afterwards.
    pub fn insert(&mut self, node: CompactNode, seen: Seen, now: Instant) -> bool {
        let Some(b) = self.bucket_of(&node.id) else {
            return false; // our own id
        };
        let ip = *node.addr.ip();
        // Known address: refresh in place (a changed id moves it).
        if let Some(&ob) = self.ips.get(&ip) {
            let bucket = &mut self.buckets[ob];
            if let Some(e) = bucket.entries.iter_mut().find(|e| e.node.addr.ip() == &ip) {
                if e.node == node {
                    match seen {
                        Seen::Answered => {
                            e.last_answer = Some(now);
                            e.fails = 0;
                            bucket.changed = now;
                        }
                        Seen::Queried => e.last_query_from = Some(now),
                    }
                    return true;
                }
                if seen != Seen::Answered {
                    return false; // another port/id on a known IP: keep the one we know
                }
            }
            self.remove_ip(ip);
        }
        let entry = Entry {
            node,
            last_answer: (seen == Seen::Answered).then_some(now),
            last_query_from: (seen == Seen::Queried).then_some(now),
            fails: 0,
        };
        let bucket = &mut self.buckets[b];
        if bucket.entries.len() < K {
            bucket.entries.push(entry);
            bucket.changed = now;
            self.ips.insert(ip, b);
            return true;
        }
        let victim = bucket.entries.iter().position(Entry::is_bad).or_else(|| {
            (seen == Seen::Answered)
                .then(|| {
                    bucket
                        .entries
                        .iter()
                        .enumerate()
                        .filter(|(_, e)| !e.is_good(now))
                        .min_by_key(|(_, e)| e.last_answer)
                        .map(|(i, _)| i)
                })
                .flatten()
        });
        let Some(i) = victim else {
            return false;
        };
        let old = std::mem::replace(&mut bucket.entries[i], entry);
        bucket.changed = now;
        self.ips.remove(old.node.addr.ip());
        self.ips.insert(ip, b);
        true
    }

    fn remove_ip(&mut self, ip: Ipv4Addr) {
        if let Some(b) = self.ips.remove(&ip) {
            self.buckets[b].entries.retain(|e| e.node.addr.ip() != &ip);
        }
    }

    /// A query to `addr` timed out. Bad entries are dropped once a bucket needs the room;
    /// here they are dropped right away when the bucket is full.
    pub fn failed(&mut self, addr: &SocketAddrV4) {
        let Some(&b) = self.ips.get(addr.ip()) else {
            return;
        };
        let bucket = &mut self.buckets[b];
        if let Some(e) = bucket.entries.iter_mut().find(|e| &e.node.addr == addr) {
            e.fails = e.fails.saturating_add(1);
        }
    }

    /// The `k` known contacts closest to `target` that are not bad, good ones first at equal
    /// distance is not needed: distance decides.
    pub fn closest(&self, target: &NodeId, k: usize) -> Vec<CompactNode> {
        let mut all: Vec<(NodeId, CompactNode)> = self
            .buckets
            .iter()
            .flat_map(|b| b.entries.iter())
            .filter(|e| !e.is_bad())
            .map(|e| (distance(&e.node.id, target), e.node))
            .collect();
        all.sort_unstable_by_key(|(d, _)| *d);
        all.into_iter().take(k).map(|(_, n)| n).collect()
    }

    /// [`RoutingTable::closest`] with a lookup tier (D95): good → fresh, a failure since the
    /// last answer → struck, otherwise unknown.
    pub fn closest_ranked(
        &self,
        target: &NodeId,
        k: usize,
        now: Instant,
    ) -> Vec<(CompactNode, super::pool::Tier)> {
        use super::pool::Tier;
        let mut all: Vec<(NodeId, CompactNode, Tier)> = self
            .buckets
            .iter()
            .flat_map(|b| b.entries.iter())
            .filter(|e| !e.is_bad())
            .map(|e| {
                let t = if e.fails > 0 {
                    Tier::Struck
                } else if e.is_good(now) {
                    Tier::Fresh
                } else {
                    Tier::Unknown
                };
                (distance(&e.node.id, target), e.node, t)
            })
            .collect();
        all.sort_unstable_by_key(|(d, _, _)| *d);
        all.into_iter().take(k).map(|(_, n, t)| (n, t)).collect()
    }

    /// Up to `n` good contacts (for persistence between runs, BEP 5).
    pub fn good(&self, now: Instant, n: usize) -> Vec<CompactNode> {
        self.buckets
            .iter()
            .flat_map(|b| b.entries.iter())
            .filter(|e| e.is_good(now))
            .take(n)
            .map(|e| e.node)
            .collect()
    }

    /// Questionable entries (answered once, silent for 15 min), least recently seen first.
    pub fn questionable(&self, now: Instant, n: usize) -> Vec<SocketAddrV4> {
        let mut q: Vec<&Entry> = self
            .buckets
            .iter()
            .flat_map(|b| b.entries.iter())
            .filter(|e| !e.is_good(now) && !e.is_bad())
            .collect();
        q.sort_by_key(|e| e.last_answer);
        q.into_iter().take(n).map(|e| e.node.addr).collect()
    }

    /// Prefix lengths of the buckets unchanged for 15 minutes, up to the deepest non-empty one
    /// (deeper buckets cannot fill: the DHT has about 2^23 nodes).
    pub fn stale_buckets(&self, now: Instant) -> Vec<u32> {
        let deepest = self
            .buckets
            .iter()
            .rposition(|b| !b.entries.is_empty())
            .unwrap_or(0);
        (0..=deepest)
            .filter(|&i| now.saturating_duration_since(self.buckets[i].changed) >= GOOD_FOR)
            .map(|i| i as u32)
            .collect()
    }

    pub fn touch_bucket(&mut self, bits: u32, now: Instant) {
        if let Some(b) = self.buckets.get_mut(bits as usize) {
            b.changed = now;
        }
    }

    /// Every entry, for a rebuild under a new id.
    pub fn entries(&self) -> Vec<Entry> {
        self.buckets
            .iter()
            .flat_map(|b| b.entries.iter().cloned())
            .collect()
    }

    /// The same contacts under a new own id (the external IP changed, BEP 42).
    pub fn rebuild(&mut self, own: NodeId, now: Instant) {
        let old = self.entries();
        *self = RoutingTable::new(own, now);
        for e in old {
            if let Some(b) = self.bucket_of(&e.node.id) {
                let bucket = &mut self.buckets[b];
                if bucket.entries.len() < K && !self.ips.contains_key(e.node.addr.ip()) {
                    self.ips.insert(*e.node.addr.ip(), b);
                    bucket.entries.push(e);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dht::id::random_id_in_bucket;

    fn node(id: NodeId, i: u32) -> CompactNode {
        CompactNode {
            id,
            addr: SocketAddrV4::new(Ipv4Addr::from(0x0101_0000 + i), 6881),
        }
    }

    #[test]
    fn buckets_hold_k_and_replace_bad_or_stale() {
        let now = Instant::now();
        let own = [0u8; 20];
        let mut t = RoutingTable::new(own, now);
        // Bucket 0 = ids with the top bit set.
        for i in 0..K as u32 {
            let id = random_id_in_bucket(&own, 0, rand::random());
            assert!(t.insert(node(id, i), Seen::Answered, now));
        }
        assert_eq!(t.len(), K);
        let extra = node(random_id_in_bucket(&own, 0, rand::random()), 100);
        // Full of good nodes: discarded (BEP 5).
        assert!(!t.insert(extra, Seen::Answered, now));
        // One goes bad after two failures: replaced.
        let victim = t.closest(&[0xff; 20], 1)[0].addr;
        t.failed(&victim);
        assert!(!t.insert(extra, Seen::Queried, now));
        t.failed(&victim);
        assert!(t.insert(extra, Seen::Queried, now));
        assert_eq!(t.len(), K);
        // After 15 min everyone is questionable: a node that answered takes a slot,
        // one that only queried us does not.
        let later = now + GOOD_FOR + Duration::from_secs(1);
        let q = node(random_id_in_bucket(&own, 0, rand::random()), 200);
        assert!(!t.insert(q, Seen::Queried, later));
        assert!(t.insert(q, Seen::Answered, later));
        assert_eq!(t.len(), K);
        assert!(!t.questionable(later, 16).is_empty());
    }

    #[test]
    fn one_entry_per_ip_and_closest() {
        let now = Instant::now();
        let own = [0x55u8; 20];
        let mut t = RoutingTable::new(own, now);
        let a = node([1; 20], 1);
        assert!(t.insert(a, Seen::Answered, now));
        let mut same_ip = a;
        same_ip.addr.set_port(7000);
        same_ip.id = [2; 20];
        assert!(!t.insert(same_ip, Seen::Queried, now));
        assert!(t.insert(same_ip, Seen::Answered, now));
        assert_eq!(t.len(), 1);
        for i in 2..40u32 {
            t.insert(node(rand::random(), i), Seen::Answered, now);
        }
        let target: NodeId = rand::random();
        let c = t.closest(&target, 8);
        assert_eq!(c.len(), 8);
        for w in c.windows(2) {
            assert!(distance(&w[0].id, &target) <= distance(&w[1].id, &target));
        }
        assert!(!t.insert(node(own, 999), Seen::Answered, now));
    }

    #[test]
    fn rebuild_keeps_contacts() {
        let now = Instant::now();
        let mut t = RoutingTable::new([0; 20], now);
        for i in 0..30u32 {
            t.insert(node(rand::random(), i), Seen::Answered, now);
        }
        let before = t.len();
        t.rebuild([0xff; 20], now);
        assert!(!t.is_empty() && t.len() <= before);
        assert_eq!(t.own_id(), [0xff; 20]);
        assert!(t.stale_buckets(now + GOOD_FOR).contains(&0));
        assert_eq!(t.good(now, 1000).len(), t.len());
    }
}

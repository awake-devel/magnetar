//! In-memory fetch queue (docs/DHT.md §3.7): bounded, three priorities — 2 announce with a peer,
//! 1 sampled, 0 incoming `get_peers` and retries. The highest non-empty lane is served newest
//! first (a freshly sampled hash is the one whose swarm and sampling node are most likely still
//! there); overflow drops the **oldest** entry of the lowest-priority lane, which keeps its
//! `dht_seen` lease and comes back through the retry loop (D60).

use std::collections::{HashMap, VecDeque};
use std::net::SocketAddrV4;

use magnetar_core::dht::{Id, Via};
use tokio::time::Instant;

pub const DEFAULT_CAP: usize = 100_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FetchItem {
    pub info_hash: Id,
    pub via: Via,
    /// The node that sampled the hash.
    pub source: Option<SocketAddrV4>,
    /// The announcing peer.
    pub peer: Option<SocketAddrV4>,
    pub retry: bool,
    pub queued_at: Instant,
}

impl FetchItem {
    pub fn priority(&self) -> usize {
        match (self.retry, self.via, self.peer.is_some()) {
            (true, _, _) => 0,
            (_, Via::Announce, true) => 2,
            (_, Via::Sample, _) => 1,
            _ => 0,
        }
    }
}

/// Where an announce for an already-seen hash went (D93).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Attach {
    /// The queued item moved to the announce lane with the peer.
    Queued,
    /// Its peer collection is running: the peer is added to what it finds.
    Collecting,
    /// Neither queued nor being collected (Postgres triage decides).
    No,
}

#[derive(Debug)]
pub struct FetchQueue {
    lanes: [VecDeque<FetchItem>; 3],
    /// Queued hashes and their lane.
    members: HashMap<Id, usize>,
    /// Hashes whose peer collection runs now, with a peer that announced meanwhile.
    collecting: HashMap<Id, Option<SocketAddrV4>>,
    cap: usize,
    pub overflow: u64,
}

impl FetchQueue {
    pub fn new(cap: usize) -> Self {
        Self {
            lanes: Default::default(),
            members: HashMap::new(),
            collecting: HashMap::new(),
            cap: cap.max(1),
            overflow: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.members.len()
    }

    pub fn is_empty(&self) -> bool {
        self.members.is_empty()
    }

    pub fn lane_lens(&self) -> [usize; 3] {
        [
            self.lanes[0].len(),
            self.lanes[1].len(),
            self.lanes[2].len(),
        ]
    }

    fn take(&mut self, h: &Id, lane: usize) -> Option<FetchItem> {
        let i = self.lanes[lane].iter().position(|it| it.info_hash == *h)?;
        let it = self.lanes[lane].remove(i)?;
        self.members.remove(h);
        Some(it)
    }

    /// Queue an item; false when it was already queued (at the same or a higher priority) or
    /// lost to overflow. A higher-priority sighting of a queued hash moves it up (D93: an
    /// incoming `get_peers` queued it at 0, the sampler or an announce sees it next), keeping
    /// the sampling node and announcer it already had.
    pub fn push(&mut self, mut item: FetchItem) -> bool {
        let prio = item.priority();
        if let Some(&lane) = self.members.get(&item.info_hash) {
            if lane >= prio {
                return false;
            }
            if let Some(old) = self.take(&item.info_hash, lane) {
                item.source = item.source.or(old.source);
                item.peer = item.peer.or(old.peer);
                item.queued_at = old.queued_at.min(item.queued_at);
            }
            let prio = item.priority();
            self.members.insert(item.info_hash, prio);
            self.lanes[prio].push_back(item);
            return true;
        }
        if self.members.len() >= self.cap {
            let lowest = (0..3).find(|&l| !self.lanes[l].is_empty()).unwrap_or(prio);
            if lowest > prio {
                self.overflow += 1;
                return false; // everything queued outranks it
            }
            if let Some(old) = self.lanes[lowest].pop_front() {
                self.members.remove(&old.info_hash);
                self.overflow += 1;
            }
        }
        self.members.insert(item.info_hash, prio);
        self.lanes[prio].push_back(item);
        true
    }

    pub fn pop(&mut self) -> Option<FetchItem> {
        let item = self.lanes.iter_mut().rev().find_map(VecDeque::pop_back)?;
        self.members.remove(&item.info_hash);
        Some(item)
    }

    /// [`pop`](Self::pop) for peer collection: the hash is marked as being collected (under the
    /// same lock, so an announce never falls between the queue and the collection).
    pub fn pop_for_collection(&mut self) -> Option<FetchItem> {
        let item = self.pop()?;
        self.collecting.insert(item.info_hash, None);
        Some(item)
    }

    /// The collection of `h` ended; returns a peer that announced it meanwhile.
    pub fn collected(&mut self, h: &Id) -> Option<SocketAddrV4> {
        self.collecting.remove(h).flatten()
    }

    pub fn collecting(&self) -> usize {
        self.collecting.len()
    }

    /// An announce with `peer` for a hash the seen filter already had (D93: before, it was
    /// dropped there, so a hash whose announcer first sent `get_peers` stayed in the lowest
    /// lane without its peer).
    pub fn attach_announce(&mut self, h: Id, peer: SocketAddrV4, now: Instant) -> Attach {
        if let Some(&lane) = self.members.get(&h) {
            let Some(mut it) = self.take(&h, lane) else {
                return Attach::No;
            };
            if it.peer.is_none() {
                it.peer = Some(peer);
            }
            it.via = Via::Announce;
            it.retry = false;
            it.queued_at = it.queued_at.min(now);
            let prio = it.priority();
            self.members.insert(h, prio);
            self.lanes[prio].push_back(it);
            return Attach::Queued;
        }
        if let Some(p) = self.collecting.get_mut(&h) {
            p.get_or_insert(peer);
            return Attach::Collecting;
        }
        Attach::No
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(b: u8, via: Via, peer: bool) -> FetchItem {
        FetchItem {
            info_hash: [b; 20],
            via,
            source: None,
            peer: peer.then(|| "1.2.3.4:5".parse().unwrap()),
            retry: false,
            queued_at: Instant::now(),
        }
    }

    #[test]
    fn priorities_lifo_and_overflow() {
        let mut q = FetchQueue::new(3);
        assert!(q.push(item(1, Via::Sample, false)));
        assert!(q.push(item(2, Via::GetPeers, false)));
        assert!(q.push(item(3, Via::Sample, false)));
        assert!(!q.push(item(3, Via::Sample, false)), "already queued");
        // Full: an announce evicts the oldest lowest-priority entry (the get_peers one).
        assert!(q.push(item(4, Via::Announce, true)));
        assert_eq!(q.lane_lens(), [0, 2, 1]);
        // A get_peers sighting cannot displace sampled ones.
        assert!(!q.push(item(5, Via::GetPeers, false)));
        // Another sample displaces the oldest sample.
        assert!(q.push(item(6, Via::Sample, false)));
        assert_eq!(q.overflow, 3);
        assert_eq!(q.pop().unwrap().info_hash, [4; 20]);
        assert_eq!(q.pop().unwrap().info_hash, [6; 20], "newest first");
        assert_eq!(q.pop().unwrap().info_hash, [3; 20]);
        assert!(q.pop().is_none());
        assert!(q.is_empty());
        let mut r = item(7, Via::Announce, true);
        r.retry = true;
        assert_eq!(r.priority(), 0);
    }

    #[test]
    fn higher_priority_sightings_move_a_hash_up() {
        let mut q = FetchQueue::new(10);
        assert!(q.push(item(1, Via::GetPeers, false)));
        let mut s = item(1, Via::Sample, false);
        s.source = Some("5.6.7.8:9".parse().unwrap());
        assert!(q.push(s), "sample outranks get_peers");
        assert_eq!(q.lane_lens(), [0, 1, 0]);
        assert!(!q.push(item(1, Via::GetPeers, false)), "never down");
        assert!(q.push(item(1, Via::Announce, true)));
        assert_eq!((q.lane_lens(), q.len()), ([0, 0, 1], 1));
        let it = q.pop().unwrap();
        assert!(
            it.peer.is_some() && it.source.is_some(),
            "kept both: {it:?}"
        );
    }

    /// The live bug (0 rows via = announce in production): the announcer's own get_peers
    /// queued the hash at the lowest lane, its announce was dropped by the seen filter.
    #[test]
    fn announce_attaches_to_queued_or_collecting_hashes() {
        let now = Instant::now();
        let peer: SocketAddrV4 = "9.9.9.9:6881".parse().unwrap();
        let mut q = FetchQueue::new(10);
        q.push(item(1, Via::GetPeers, false));
        q.push(item(2, Via::Sample, false));
        assert_eq!(q.attach_announce([1; 20], peer, now), Attach::Queued);
        assert_eq!(q.lane_lens(), [0, 1, 1]);
        let it = q.pop_for_collection().unwrap();
        assert_eq!(
            (it.info_hash, it.via, it.peer),
            ([1; 20], Via::Announce, Some(peer))
        );
        q.collected(&it.info_hash);
        // While hash 2 is being collected, an announce hands its peer over.
        let it = q.pop_for_collection().unwrap();
        assert_eq!(q.collecting(), 1);
        assert_eq!(q.attach_announce([2; 20], peer, now), Attach::Collecting);
        assert_eq!(q.collected(&it.info_hash), Some(peer));
        assert_eq!(q.collecting(), 0);
        assert_eq!(q.attach_announce([3; 20], peer, now), Attach::No);
        assert!(q.is_empty());
    }
}

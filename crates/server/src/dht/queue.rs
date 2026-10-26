//! In-memory fetch queue (docs/DHT.md §3.7): bounded, three priorities — 2 announce with a peer,
//! 1 sampled, 0 incoming `get_peers` and retries. The highest non-empty lane is served newest
//! first (a freshly sampled hash is the one whose swarm and sampling node are most likely still
//! there); overflow drops the **oldest** entry of the lowest-priority lane, which keeps its
//! `dht_seen` lease and comes back through the retry loop (D60).

use std::collections::{HashSet, VecDeque};
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

#[derive(Debug)]
pub struct FetchQueue {
    lanes: [VecDeque<FetchItem>; 3],
    members: HashSet<Id>,
    cap: usize,
    pub overflow: u64,
}

impl FetchQueue {
    pub fn new(cap: usize) -> Self {
        Self {
            lanes: Default::default(),
            members: HashSet::new(),
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

    /// Queue an item; false when it was already queued or lost to overflow.
    pub fn push(&mut self, item: FetchItem) -> bool {
        if self.members.contains(&item.info_hash) {
            return false;
        }
        let prio = item.priority();
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
        self.members.insert(item.info_hash);
        self.lanes[prio].push_back(item);
        true
    }

    pub fn pop(&mut self) -> Option<FetchItem> {
        let item = self.lanes.iter_mut().rev().find_map(VecDeque::pop_back)?;
        self.members.remove(&item.info_hash);
        Some(item)
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
}

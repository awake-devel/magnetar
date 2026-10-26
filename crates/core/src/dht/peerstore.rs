//! Honest store of announced peers (BEP 5: "the queried node should store the IP address of the
//! querying node and the supplied port number under the infohash"), docs/DHT.md §3.6.
//!
//! Only peers that announced to us with a valid token are stored, kept 45 minutes, at most
//! 100 000 in total and 64 per infohash. They are served back in `get_peers` (`values`) and
//! sampled in `sample_infohashes` answers. Nothing is invented.

use std::collections::HashMap;
use std::net::SocketAddrV4;
use std::time::Duration;

use rand::seq::IndexedRandom;
use tokio::time::Instant;

use super::krpc::Id;

pub const TTL: Duration = Duration::from_secs(45 * 60);
pub const CAP: usize = 100_000;
const PER_HASH: usize = 64;

#[derive(Debug, Default)]
pub struct PeerStore {
    peers: HashMap<Id, Vec<(SocketAddrV4, Instant)>>,
    total: usize,
    cap: usize,
    /// Keys for sampling: new hashes are appended, `purge` rebuilds it (capped like the store).
    keys: Vec<Id>,
}

impl PeerStore {
    pub fn new(cap: usize) -> Self {
        Self {
            cap,
            ..Default::default()
        }
    }

    pub fn hashes(&self) -> usize {
        self.peers.len()
    }

    pub fn peers(&self) -> usize {
        self.total
    }

    /// Store an announce. Returns false when the store is full.
    pub fn announce(&mut self, info_hash: Id, peer: SocketAddrV4, now: Instant) -> bool {
        let list = self.peers.entry(info_hash).or_default();
        if let Some(p) = list.iter_mut().find(|(a, _)| *a == peer) {
            p.1 = now + TTL;
            return true;
        }
        if self.total >= self.cap {
            if list.is_empty() {
                self.peers.remove(&info_hash);
            }
            return false;
        }
        if list.len() >= PER_HASH {
            // Replace the one expiring first.
            if let Some(oldest) = list.iter_mut().min_by_key(|(_, t)| *t) {
                *oldest = (peer, now + TTL);
            }
            return true;
        }
        if list.is_empty() && self.keys.len() < self.cap {
            self.keys.push(info_hash);
        }
        list.push((peer, now + TTL));
        self.total += 1;
        true
    }

    /// Up to `n` live peers for `info_hash`.
    pub fn get(&self, info_hash: &Id, now: Instant, n: usize) -> Vec<SocketAddrV4> {
        self.peers
            .get(info_hash)
            .map(|l| {
                l.iter()
                    .filter(|(_, t)| *t > now)
                    .take(n)
                    .map(|(a, _)| *a)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Drop expired peers and refresh the sampling snapshot (called every minute).
    pub fn purge(&mut self, now: Instant) {
        let mut total = 0;
        self.peers.retain(|_, l| {
            l.retain(|(_, t)| *t > now);
            total += l.len();
            !l.is_empty()
        });
        self.total = total;
        self.keys = self.peers.keys().copied().collect();
    }

    /// Up to `n` random infohashes we hold peers for.
    pub fn sample(&self, n: usize) -> Vec<Id> {
        self.keys
            .choose_multiple(&mut rand::rng(), n)
            .filter(|k| self.peers.contains_key(*k))
            .copied()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stores_expires_and_caps() {
        let now = Instant::now();
        let mut s = PeerStore::new(3);
        let p = |i: u8| SocketAddrV4::new([1, 1, 1, i].into(), 6881);
        assert!(s.announce([1; 20], p(1), now));
        assert!(s.announce([1; 20], p(1), now)); // refresh, not a second entry
        assert!(s.announce([1; 20], p(2), now));
        assert!(s.announce([2; 20], p(3), now));
        assert!(!s.announce([3; 20], p(4), now));
        assert_eq!((s.hashes(), s.peers()), (2, 3));
        assert_eq!(s.get(&[1; 20], now, 50).len(), 2);
        s.purge(now);
        assert_eq!(s.sample(20).len(), 2);
        assert!(s.get(&[1; 20], now + TTL, 50).is_empty());
        s.purge(now + TTL);
        assert_eq!((s.hashes(), s.peers()), (0, 0));
        assert!(s.sample(20).is_empty());
    }
}

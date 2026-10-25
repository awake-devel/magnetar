//! Liveness (D16–D17, bittorrent.md §8). Owner: liveness agent.

pub mod bencode;
pub mod dht;
pub mod http;
pub mod udp;

use std::time::Duration;

use crate::model::Liveness;
use crate::store::Store;

pub const TTL_ALIVE: i64 = 6 * 3600;
pub const TTL_DEAD: i64 = 24 * 3600;
pub const TTL_ERROR: i64 = 3600;

#[derive(Clone, Debug)]
pub struct LivenessConfig {
    pub tracker_timeout: Duration,
    pub tracker_concurrency: usize,
    pub use_dht: bool,
    pub dht_budget: Duration,
    pub dht_concurrency: usize,
    /// Added to every torrent's own trackers.
    pub extra_trackers: Vec<String>,
}

impl Default for LivenessConfig {
    fn default() -> Self {
        Self {
            tracker_timeout: Duration::from_secs(3),
            tracker_concurrency: 8,
            use_dht: true,
            dht_budget: Duration::from_secs(12),
            dht_concurrency: 2,
            extra_trackers: Vec::new(),
        }
    }
}

/// Check many infohashes at once: group by tracker, UDP/HTTP scrape, DHT only where no tracker
/// answered. Aggregate = max over trackers. Every input gets a result (source None on total failure).
pub async fn check_batch(
    items: &[(String, Vec<String>)],
    cfg: &LivenessConfig,
    now: i64,
) -> Vec<Liveness> {
    let _ = (items, cfg, now);
    todo!("liveness::check_batch")
}

/// Pull due infohashes from the store, check them, store results. Returns the number checked.
pub async fn run_due<S: Store + ?Sized>(
    store: &S,
    cfg: &LivenessConfig,
    now: i64,
    limit: usize,
) -> crate::Result<usize> {
    let _ = (store, cfg, now, limit);
    todo!("liveness::run_due")
}

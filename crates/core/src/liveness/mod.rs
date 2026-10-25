//! Liveness (D16–D17, bittorrent.md §8). Owner: liveness agent.

pub mod bencode;
pub mod dht;
pub mod http;
pub mod udp;

use std::collections::HashMap;
use std::time::Duration;

use futures::StreamExt;

use crate::model::{Liveness, LivenessSource};
use crate::store::Store;
use udp::ScrapeStats;

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
    let raw: Vec<Option<[u8; 20]>> = items.iter().map(|(ih, _)| parse_hash(ih)).collect();

    // normalized tracker -> distinct hashes, and for each hash the items it belongs to.
    let mut groups: HashMap<Tracker, Vec<[u8; 20]>> = HashMap::new();
    let mut members: HashMap<[u8; 20], Vec<usize>> = HashMap::new();
    for (i, ((_, trackers), hash)) in items.iter().zip(&raw).enumerate() {
        let Some(hash) = *hash else { continue };
        let first = !members.contains_key(&hash);
        members.entry(hash).or_default().push(i);
        if !first {
            continue;
        }
        let mut seen = Vec::new();
        for t in trackers.iter().chain(&cfg.extra_trackers) {
            if let Some(t) = normalize_tracker(t)
                && !seen.contains(&t)
            {
                groups.entry(t.clone()).or_default().push(hash);
                seen.push(t);
            }
        }
    }

    let client = crate::http::client(cfg.tracker_timeout * 2).ok();
    type Answer = (
        Vec<[u8; 20]>,
        LivenessSource,
        Option<Vec<Option<ScrapeStats>>>,
    );
    let answers: Vec<Answer> = futures::stream::iter(groups)
        .map(|(tracker, hashes)| {
            let client = client.as_ref();
            async move {
                let (source, res) = match &tracker {
                    Tracker::Udp(url) => (
                        LivenessSource::Udp,
                        udp::scrape(url, &hashes, cfg.tracker_timeout).await,
                    ),
                    Tracker::Http(url) => (
                        LivenessSource::Http,
                        match client {
                            Some(c) => http::scrape(c, url, &hashes).await,
                            None => Err(crate::Error::Other("no http client".into())),
                        },
                    ),
                };
                if let Err(e) = &res {
                    tracing::debug!(tracker = tracker.url(), error = %e, "scrape failed");
                }
                (hashes, source, res.ok())
            }
        })
        .buffer_unordered(cfg.tracker_concurrency.max(1))
        .collect()
        .await;

    let mut acc: Vec<Acc> = vec![Acc::default(); items.len()];
    for (hashes, source, res) in answers {
        for (pos, hash) in hashes.iter().enumerate() {
            let stats = res.as_ref().and_then(|r| r.get(pos).copied().flatten());
            let stats = stats.map(|s| (s, source));
            for &i in &members[hash] {
                acc[i].add(stats);
            }
        }
    }

    let mut out: Vec<Liveness> = items
        .iter()
        .zip(&acc)
        .map(|((ih, _), a)| a.liveness(ih, now))
        .collect();

    if cfg.use_dht {
        // One DHT walk per distinct hash that no tracker answered.
        let mut pending: Vec<[u8; 20]> = Vec::new();
        for (i, hash) in raw.iter().enumerate() {
            if let Some(h) = hash
                && out[i].source == LivenessSource::None
                && !pending.contains(h)
            {
                pending.push(*h);
            }
        }
        let dht: Vec<([u8; 20], crate::Result<dht::DhtScrape>)> = futures::stream::iter(pending)
            .map(|h| async move { (h, dht::scrape(&h, cfg.dht_budget).await) })
            .buffer_unordered(cfg.dht_concurrency.max(1))
            .collect()
            .await;
        for (hash, res) in dht {
            match res {
                Ok(d) => {
                    for &i in &members[&hash] {
                        apply_dht(&mut out[i], &d, now);
                    }
                }
                Err(e) => tracing::debug!(infohash = hex::encode(hash), error = %e, "dht failed"),
            }
        }
    }
    out
}

/// Pull due infohashes from the store, check them, store results. Returns the number checked.
pub async fn run_due<S: Store + ?Sized>(
    store: &S,
    cfg: &LivenessConfig,
    now: i64,
    limit: usize,
) -> crate::Result<usize> {
    let due = store.liveness_due(now, limit).await?;
    if due.is_empty() {
        return Ok(0);
    }
    let results = check_batch(&due, cfg, now).await;
    for l in &results {
        store.put_liveness(l).await?;
    }
    Ok(results.len())
}

/// Below this many answering nodes an empty DHT walk proves nothing, so the result stays unknown.
const DHT_MIN_NODES_FOR_DEAD: u32 = 16;

fn apply_dht(l: &mut Liveness, d: &dht::DhtScrape, now: i64) {
    let est = if d.bf_responses > 0 {
        l.source = LivenessSource::Dht;
        d.total_est
    } else if d.values_ips > 0 {
        l.source = LivenessSource::DhtValues;
        d.values_ips.min(dht::ESTIMATE_CAP)
    } else if d.nodes_queried >= DHT_MIN_NODES_FOR_DEAD {
        l.source = LivenessSource::Dht;
        0
    } else {
        return;
    };
    l.dht_peers_est = Some(est);
    l.checked_at = now;
    l.expires_at = now + if est > 0 { TTL_ALIVE } else { TTL_DEAD };
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Tracker {
    Udp(String),
    Http(String),
}

impl Tracker {
    fn url(&self) -> &str {
        match self {
            Tracker::Udp(u) | Tracker::Http(u) => u,
        }
    }
}

/// Group key for a tracker URL: `udp://host:port` (the path is irrelevant to BEP 15) or the full
/// http(s) announce URL (lowercase scheme/host, no fragment, query kept for passkeys). Trackers we
/// cannot scrape (wss, http without an `announce` segment, garbage) are dropped.
fn normalize_tracker(raw: &str) -> Option<Tracker> {
    let mut url = url::Url::parse(raw.trim()).ok()?;
    let host = url.host_str()?.to_string();
    match url.scheme() {
        "udp" => Some(Tracker::Udp(format!("udp://{host}:{}", url.port()?))),
        "http" | "https" => {
            url.set_fragment(None);
            http::scrape_url(url.as_str())?;
            Some(Tracker::Http(url.into()))
        }
        _ => None,
    }
}

fn parse_hash(ih: &str) -> Option<[u8; 20]> {
    let mut out = [0u8; 20];
    hex::decode_to_slice(ih.trim(), &mut out).ok()?;
    Some(out)
}

#[derive(Clone, Copy, Default)]
struct Acc {
    best: Option<(ScrapeStats, LivenessSource)>,
    ok: u16,
    failed: u16,
}

impl Acc {
    fn add(&mut self, stats: Option<(ScrapeStats, LivenessSource)>) {
        let Some((s, src)) = stats else {
            self.failed = self.failed.saturating_add(1);
            return;
        };
        self.ok = self.ok.saturating_add(1);
        self.best = Some(match self.best {
            None => (s, src),
            Some((b, bsrc)) => (
                ScrapeStats {
                    seeders: b.seeders.max(s.seeders),
                    leechers: b.leechers.max(s.leechers),
                    completed: b.completed.max(s.completed),
                },
                // The source is the tracker that reported the most seeders.
                if s.seeders > b.seeders { src } else { bsrc },
            ),
        });
    }

    fn liveness(&self, infohash: &str, now: i64) -> Liveness {
        let (stats, source) = match self.best {
            Some((s, src)) => (Some(s), src),
            None => (None, LivenessSource::None),
        };
        let ttl = match stats {
            None => TTL_ERROR,
            Some(s) if s.seeders > 0 => TTL_ALIVE,
            Some(_) => TTL_DEAD,
        };
        Liveness {
            infohash: infohash.to_string(),
            seeders: stats.map(|s| s.seeders),
            leechers: stats.map(|s| s.leechers),
            completed: stats.map(|s| s.completed),
            dht_peers_est: None,
            source,
            trackers_ok: self.ok,
            trackers_failed: self.failed,
            checked_at: now,
            expires_at: now + ttl,
        }
    }
}

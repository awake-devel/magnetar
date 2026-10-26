//! Liveness (D16–D17, D45, bittorrent.md §8). Owner: liveness agent.
//!
//! [`check_batch`] is the one-shot form (the phone, a forced check). A long-lived checker (the
//! server) keeps one [`Scraper`]: per-tracker sockets, connection ids, rate limits and backoff,
//! and the DHT node cache survive between batches.

pub mod bencode;
pub mod dht;
pub mod http;
pub mod udp;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::StreamExt;
use serde::{Deserialize, Serialize};
use tokio::time::Instant;

use crate::model::{Liveness, LivenessSource};
use crate::store::Store;
use udp::ScrapeStats;

pub const TTL_ALIVE: i64 = 6 * 3600;
pub const TTL_DEAD: i64 = 24 * 3600;
pub const TTL_ERROR: i64 = 3600;

/// Scheduled re-checks (D45): torrents up to 7 days old (and hot ones: viewed, searched, saved
/// search matches) every 6 h, up to 90 days every 2 days, older every 4 days; dead 3 times in a
/// row every 7 days; no answer at all after 1 h, doubling up to 1 day.
pub const TTL_NEW: i64 = 6 * 3600;
pub const TTL_RECENT: i64 = 2 * 86400;
pub const TTL_OLD: i64 = 4 * 86400;
pub const TTL_DEAD_LONG: i64 = 7 * 86400;
pub const TTL_ERROR_MAX: i64 = 86400;
pub const AGE_NEW: i64 = 7 * 86400;
pub const AGE_RECENT: i64 = 90 * 86400;
pub const DEAD_STREAK_LONG: u16 = 3;
/// Scheduled intervals are spread by up to ±10 % so a backlog checked in one go does not come
/// due in one go.
pub const TTL_JITTER: f64 = 0.10;

/// A tracker that failed is skipped for 2 min, doubling per failure in a row up to 6 h.
pub const TRACKER_BACKOFF: Duration = Duration::from_secs(120);
pub const TRACKER_BACKOFF_MAX: Duration = Duration::from_secs(6 * 3600);
/// Tracker state unused this long is forgotten.
const TRACKER_IDLE_FORGET: Duration = Duration::from_secs(12 * 3600);
/// DHT nodes kept between walks.
pub const DHT_NODE_CACHE: usize = 4096;

#[derive(Clone, Debug)]
pub struct LivenessConfig {
    pub tracker_timeout: Duration,
    /// Trackers scraped at the same time.
    pub tracker_concurrency: usize,
    /// Requests per second per tracker (one request = up to 74 UDP / 50 HTTP hashes).
    pub tracker_rps: f64,
    pub use_dht: bool,
    pub dht_budget: Duration,
    /// DHT walks at the same time (only hashes no tracker answered get one).
    pub dht_concurrency: usize,
    /// Added to every torrent's own trackers.
    pub extra_trackers: Vec<String>,
    /// Scraped for torrents without any usable tracker of their own. Their answers only count
    /// when they report seeders: zero from a tracker the torrent never named means "not
    /// registered there", so the DHT is asked instead (D45).
    pub fallback_trackers: Vec<String>,
}

impl Default for LivenessConfig {
    fn default() -> Self {
        Self {
            tracker_timeout: Duration::from_secs(3),
            tracker_concurrency: 8,
            tracker_rps: 2.0,
            use_dht: true,
            dht_budget: Duration::from_secs(12),
            dht_concurrency: 2,
            extra_trackers: Vec::new(),
            fallback_trackers: Vec::new(),
        }
    }
}

/// What one batch did (counts are per distinct infohash unless named otherwise).
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct BatchReport {
    pub items: u64,
    pub hashes: u64,
    /// Tracker groups (distinct trackers) in the batch, and those skipped while backing off.
    pub trackers: u64,
    pub trackers_backed_off: u64,
    /// Scrape requests sent (UDP packets / HTTP GETs) and trackers that failed.
    pub tracker_requests: u64,
    pub tracker_failures: u64,
    /// Hashes with a usable tracker answer.
    pub tracker_answered: u64,
    pub dht_walks: u64,
    pub dht_answered: u64,
    pub alive: u64,
    pub dead: u64,
    pub unknown: u64,
    pub ms: u64,
}

/// Check many infohashes at once: group by tracker, UDP/HTTP scrape, DHT only where no tracker
/// answered. Aggregate = max over trackers. Every input gets a result (source None on total
/// failure). One-shot: nothing is kept for the next call (see [`Scraper`]).
pub async fn check_batch(
    items: &[(String, Vec<String>)],
    cfg: &LivenessConfig,
    now: i64,
) -> Vec<Liveness> {
    Scraper::new(cfg.clone()).check(items, now).await.0
}

/// Tracker and DHT state shared by every batch of a long-lived checker.
pub struct Scraper {
    cfg: LivenessConfig,
    http: Option<reqwest::Client>,
    trackers: Mutex<HashMap<Tracker, Arc<tokio::sync::Mutex<Slot>>>>,
    dht_nodes: dht::NodeCache,
}

/// One tracker's state: socket + connection id (UDP), rate limit, backoff.
struct Slot {
    udp: Option<udp::UdpTracker>,
    next_send: Instant,
    failures: u32,
    skip_until: Option<Instant>,
    last_used: Instant,
}

impl Slot {
    fn new() -> Self {
        let now = Instant::now();
        Self {
            udp: None,
            next_send: now,
            failures: 0,
            skip_until: None,
            last_used: now,
        }
    }
}

/// Scrape answers of one tracker group, positional to its hashes.
struct GroupAnswer {
    hashes: Vec<usize>,
    source: LivenessSource,
    stats: Vec<Option<ScrapeStats>>,
    requests: u64,
    failed: bool,
    backed_off: bool,
}

fn backoff(failures: u32) -> Duration {
    let exp = failures.saturating_sub(1).min(16);
    (TRACKER_BACKOFF * 2u32.pow(exp)).min(TRACKER_BACKOFF_MAX)
}

impl Scraper {
    pub fn new(cfg: LivenessConfig) -> Self {
        Self::with_dht_nodes(cfg, dht::NodeCache::new(DHT_NODE_CACHE))
    }

    /// With an explicit DHT node cache (tests point it at local nodes).
    pub fn with_dht_nodes(cfg: LivenessConfig, nodes: dht::NodeCache) -> Self {
        let http = crate::http::client(cfg.tracker_timeout * 2).ok();
        Self {
            cfg,
            http,
            trackers: Mutex::new(HashMap::new()),
            dht_nodes: nodes,
        }
    }

    pub fn config(&self) -> &LivenessConfig {
        &self.cfg
    }

    /// (trackers known, trackers backing off now, DHT nodes cached).
    pub fn state(&self) -> (usize, usize, usize) {
        let now = Instant::now();
        let trackers = self.trackers.lock().unwrap();
        let backed_off = trackers
            .values()
            .filter(|s| {
                s.try_lock()
                    .is_ok_and(|s| s.skip_until.is_some_and(|u| u > now))
            })
            .count();
        (trackers.len(), backed_off, self.dht_nodes.len())
    }

    fn slot(&self, t: &Tracker) -> Arc<tokio::sync::Mutex<Slot>> {
        let mut trackers = self.trackers.lock().unwrap();
        trackers
            .entry(t.clone())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(Slot::new())))
            .clone()
    }

    /// Forget trackers nobody used for a while (a slot in use is never dropped).
    fn forget_idle(&self) {
        let now = Instant::now();
        self.trackers.lock().unwrap().retain(|_, s| {
            Arc::strong_count(s) > 1
                || s.try_lock().map_or(true, |s| {
                    now.duration_since(s.last_used) < TRACKER_IDLE_FORGET
                })
        });
    }

    /// Scrape one tracker's hashes in requests of 74 (UDP) / 50 (HTTP), at most `tracker_rps`
    /// per second. The first failure stops the group (the rest stays unanswered) and starts or
    /// extends the tracker's backoff; while it lasts the tracker is not asked at all.
    async fn scrape_group(&self, t: Tracker, hashes: Vec<usize>, raw: &[[u8; 20]]) -> GroupAnswer {
        let (source, per_request) = match &t {
            Tracker::Udp(_) => (LivenessSource::Udp, udp::UDP_SCRAPE_BATCH),
            Tracker::Http(_) => (LivenessSource::Http, http::HTTP_SCRAPE_BATCH),
        };
        let mut ans = GroupAnswer {
            stats: Vec::with_capacity(hashes.len()),
            hashes,
            source,
            requests: 0,
            failed: false,
            backed_off: false,
        };
        let slot = self.slot(&t);
        let mut slot = slot.lock().await;
        slot.last_used = Instant::now();
        if slot.skip_until.is_some_and(|u| u > Instant::now()) {
            ans.backed_off = true;
            ans.stats.resize(ans.hashes.len(), None);
            return ans;
        }
        let interval = Duration::from_secs_f64(1.0 / self.cfg.tracker_rps.max(0.01));
        let chunks: Vec<Vec<[u8; 20]>> = ans
            .hashes
            .chunks(per_request)
            .map(|c| c.iter().map(|&i| raw[i]).collect())
            .collect();
        for chunk in chunks {
            tokio::time::sleep_until(slot.next_send).await;
            slot.next_send = slot.next_send.max(Instant::now()) + interval;
            ans.requests += 1;
            let res = match &t {
                Tracker::Udp(url) => {
                    let timeout = self.cfg.tracker_timeout;
                    slot.udp
                        .get_or_insert_with(|| udp::UdpTracker::new(url))
                        .scrape_chunk(&chunk, timeout)
                        .await
                }
                Tracker::Http(url) => match &self.http {
                    Some(c) => http::scrape_chunk(c, url, &chunk).await,
                    None => Err(crate::Error::Other("no http client".into())),
                },
            };
            match res {
                Ok(stats) => ans.stats.extend(stats),
                Err(e) => {
                    slot.failures = slot.failures.saturating_add(1);
                    let wait = backoff(slot.failures);
                    slot.skip_until = Some(Instant::now() + wait);
                    tracing::debug!(
                        tracker = t.url(),
                        error = %e,
                        failures = slot.failures,
                        "scrape failed, backing off {} s",
                        wait.as_secs()
                    );
                    ans.failed = true;
                    break;
                }
            }
        }
        if !ans.failed {
            slot.failures = 0;
            slot.skip_until = None;
        }
        ans.stats.resize(ans.hashes.len(), None);
        ans
    }

    /// Check a batch (see [`check_batch`]). A hash goes to the DHT as soon as every one of its
    /// trackers is done without a usable answer, so DHT walks overlap the tracker scrapes.
    pub async fn check(
        &self,
        items: &[(String, Vec<String>)],
        now: i64,
    ) -> (Vec<Liveness>, BatchReport) {
        let started = std::time::Instant::now();
        self.forget_idle();
        let cfg = &self.cfg;
        let mut report = BatchReport {
            items: items.len() as u64,
            ..Default::default()
        };

        // Distinct hashes; for each its items and how many tracker groups still have to answer.
        let mut index: HashMap<[u8; 20], usize> = HashMap::new();
        let mut raw: Vec<[u8; 20]> = Vec::new();
        let mut item_hash: Vec<Option<usize>> = Vec::with_capacity(items.len());
        let mut groups: HashMap<Tracker, Vec<usize>> = HashMap::new();
        let mut pending: Vec<u32> = Vec::new();
        let mut fallback: Vec<bool> = Vec::new();
        for (ih, trackers) in items {
            let Some(hash) = parse_hash(ih) else {
                item_hash.push(None);
                continue;
            };
            if let Some(&i) = index.get(&hash) {
                item_hash.push(Some(i));
                continue;
            }
            let i = raw.len();
            index.insert(hash, i);
            raw.push(hash);
            item_hash.push(Some(i));
            let mut seen: Vec<Tracker> = Vec::new();
            for t in trackers.iter().chain(&cfg.extra_trackers) {
                if let Some(t) = normalize_tracker(t)
                    && !seen.contains(&t)
                {
                    seen.push(t);
                }
            }
            let mut fb = false;
            if seen.is_empty() {
                for t in &cfg.fallback_trackers {
                    if let Some(t) = normalize_tracker(t)
                        && !seen.contains(&t)
                    {
                        seen.push(t);
                        fb = true;
                    }
                }
            }
            pending.push(seen.len() as u32);
            fallback.push(fb);
            for t in seen {
                groups.entry(t).or_default().push(i);
            }
        }
        report.hashes = raw.len() as u64;
        report.trackers = groups.len() as u64;

        let mut acc: Vec<Acc> = fallback
            .iter()
            .map(|&fb| Acc {
                fallback: fb,
                ..Default::default()
            })
            .collect();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<usize>();
        if cfg.use_dht {
            for (i, &p) in pending.iter().enumerate() {
                if p == 0 {
                    let _ = tx.send(i);
                }
            }
        }

        let raw_ref = &raw;
        let trackers = async {
            let mut answers = futures::stream::iter(groups)
                .map(|(t, hashes)| self.scrape_group(t, hashes, raw_ref))
                .buffer_unordered(cfg.tracker_concurrency.max(1));
            while let Some(ans) = answers.next().await {
                report.tracker_requests += ans.requests;
                report.tracker_failures += u64::from(ans.failed);
                report.trackers_backed_off += u64::from(ans.backed_off);
                for (&i, stats) in ans.hashes.iter().zip(&ans.stats) {
                    acc[i].add(stats.map(|s| (s, ans.source)));
                    pending[i] -= 1;
                    if pending[i] == 0 && cfg.use_dht && !acc[i].answered() {
                        let _ = tx.send(i);
                    }
                }
            }
            drop(tx);
        };
        let dht_nodes = &self.dht_nodes;
        let walks = async {
            futures::stream::poll_fn(|cx| rx.poll_recv(cx))
                .map(|i| async move {
                    (
                        i,
                        dht::scrape_cached(dht_nodes, &raw_ref[i], cfg.dht_budget).await,
                    )
                })
                .buffer_unordered(cfg.dht_concurrency.max(1))
                .collect::<Vec<_>>()
                .await
        };
        let ((), dht) = futures::join!(trackers, walks);

        report.tracker_answered = acc.iter().filter(|a| a.answered()).count() as u64;
        let mut per_hash: Vec<Liveness> = acc
            .iter()
            .zip(&raw)
            .map(|(a, h)| a.liveness(&hex::encode(h), now))
            .collect();
        report.dht_walks = dht.len() as u64;
        for (i, res) in dht {
            match res {
                Ok(d) => {
                    if apply_dht(&mut per_hash[i], &d, now) {
                        report.dht_answered += 1;
                    }
                }
                Err(e) => tracing::debug!(infohash = hex::encode(raw[i]), error = %e, "dht failed"),
            }
        }
        for l in &per_hash {
            match l.best_peers() {
                _ if l.source == LivenessSource::None => report.unknown += 1,
                Some(0) | None => report.dead += 1,
                Some(_) => report.alive += 1,
            }
        }
        let out = items
            .iter()
            .zip(&item_hash)
            .map(|((ih, _), h)| match h {
                Some(i) => Liveness {
                    infohash: ih.clone(),
                    ..per_hash[*i].clone()
                },
                None => Acc::default().liveness(ih, now),
            })
            .collect();
        report.ms = started.elapsed().as_millis() as u64;
        (out, report)
    }
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

/// Dead / error streaks carried from one check to the next (D45).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Streaks {
    /// Answered checks in a row with nobody seeding.
    pub dead: u16,
    /// Checks in a row without any answer.
    pub error: u16,
}

/// When to check again after result `l` (D45): by the torrent's age (`born` = first published),
/// shortened to [`TTL_NEW`] while it is `hot`, [`TTL_DEAD_LONG`] once dead
/// [`DEAD_STREAK_LONG`] times in a row, exponential from [`TTL_ERROR`] without an answer.
/// `jitter` in −1..=1 spreads the interval by ±[`TTL_JITTER`].
pub fn next_check(
    l: &Liveness,
    born: i64,
    prev: Streaks,
    hot: bool,
    now: i64,
    jitter: f64,
) -> (i64, Streaks) {
    let (ttl, streaks) = if l.source == LivenessSource::None {
        let error = prev.error.saturating_add(1);
        let ttl = (TTL_ERROR << (error - 1).min(16)).min(TTL_ERROR_MAX);
        (ttl, Streaks { error, ..prev })
    } else {
        let age = now - born;
        let by_age = if age <= AGE_NEW {
            TTL_NEW
        } else if age <= AGE_RECENT {
            TTL_RECENT
        } else {
            TTL_OLD
        };
        let dead = match l.best_peers() {
            Some(p) if p > 0 => 0,
            _ => prev.dead.saturating_add(1),
        };
        let ttl = if dead >= DEAD_STREAK_LONG {
            TTL_DEAD_LONG
        } else {
            by_age
        };
        let ttl = if hot { ttl.min(TTL_NEW) } else { ttl };
        (ttl, Streaks { dead, error: 0 })
    };
    let spread = 1.0 + TTL_JITTER * jitter.clamp(-1.0, 1.0);
    (now + (ttl as f64 * spread).round() as i64, streaks)
}

/// Below this many answering nodes an empty DHT walk proves nothing, so the result stays unknown.
const DHT_MIN_NODES_FOR_DEAD: u32 = 16;

/// Fold a DHT result in; false when it proves nothing (the result stays as it was).
fn apply_dht(l: &mut Liveness, d: &dht::DhtScrape, now: i64) -> bool {
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
        return false;
    };
    l.dht_peers_est = Some(est);
    l.checked_at = now;
    l.expires_at = now + if est > 0 { TTL_ALIVE } else { TTL_DEAD };
    true
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
    /// Only fallback trackers were asked: an answer without seeders does not count.
    fallback: bool,
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

    /// The usable answer: any tracker of the torrent's own, or a fallback tracker with seeders.
    fn usable(&self) -> Option<(ScrapeStats, LivenessSource)> {
        self.best.filter(|(s, _)| !self.fallback || s.seeders > 0)
    }

    fn answered(&self) -> bool {
        self.usable().is_some()
    }

    fn liveness(&self, infohash: &str, now: i64) -> Liveness {
        let (stats, source) = match self.usable() {
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
            // Fallback trackers that answered without seeders did not answer for this torrent.
            trackers_ok: if stats.is_some() || !self.fallback {
                self.ok
            } else {
                0
            },
            trackers_failed: self.failed,
            checked_at: now,
            expires_at: now + ttl,
        }
    }
}

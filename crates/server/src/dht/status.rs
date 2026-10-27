//! Live state of the `serve` crawler for `/api/v1/stats`, `/api/v1/stats/health` and
//! `/health` (D58, docs/DHT.md §10): the running pipeline's counters, rates over the last ~10
//! minutes, the UPnP state, and database totals cached for a minute.
//!
//! Every minute the sampler also adds the counters' growth to `stats_series` (`dht_hashes_seen`,
//! `dht_fetch_ok`, `dht_fetch_fail`, `dht_blocked`; `dht_torrents_added` comes from the insert
//! trigger), so the app's activity chart shows the crawler.

use std::collections::VecDeque;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use magnetar_core::api::{DhtStats, UpnpStatus};
use magnetar_core::dht::{Dht, Mode};
use sqlx::{FromRow, PgPool};
use tokio::sync::watch;
use tokio::time::Instant;

use super::{Pipeline, get};
use crate::daemons::stop_requested;
use crate::stats;

/// Rates are computed over at most this many one-minute samples (10 minutes).
const WINDOW: usize = 11;
pub const SAMPLE_EVERY: Duration = Duration::from_secs(60);
const TOTALS_TTL: Duration = Duration::from_secs(60);
/// The totals are whole-table counts (`dht_seen` grows without a cap): bounded per refresh.
const TOTALS_TIMEOUT_MS: u32 = 5_000;

pub const DHT_HASHES_SEEN: &str = "dht_hashes_seen";
pub const DHT_FETCH_OK: &str = "dht_fetch_ok";
pub const DHT_FETCH_FAIL: &str = "dht_fetch_fail";
pub const DHT_BLOCKED: &str = "dht_blocked";
pub const DHT_TORRENTS_ADDED: &str = "dht_torrents_added";

/// Counters the rates are computed from.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Snap {
    unique: u64,
    verified: u64,
    failed: u64,
    blocked: u64,
    incoming: u64,
    queries: u64,
    answered: u64,
    lookups: u64,
    lookup_ms: u64,
    lookup_queries: u64,
    lookup_slow: u64,
    /// Sent / answered by node age: young, old, unpooled, repeat (D93).
    age_sent: [u64; 4],
    age_answered: [u64; 4],
    /// D95: lookup queries answered, seeds (fresh, all), `get_peers` sent, torrents stored.
    lookup_answered: u64,
    seeds_fresh: u64,
    seeds: u64,
    get_peers: u64,
    stored: u64,
}

struct Running {
    p: Arc<Pipeline>,
    dht: Dht,
    started: Instant,
    window: VecDeque<(Instant, Snap)>,
    /// What the last `stats_series` bump covered.
    bumped: Snap,
}

#[derive(Clone, Debug, Default, FromRow)]
struct Totals {
    stored: i64,
    adult: i64,
    blocked: i64,
    queued: i64,
    failed: i64,
}

#[derive(Default)]
struct Inner {
    running: Option<Running>,
    last_error: Option<String>,
    totals: Option<(Instant, Totals)>,
}

/// Shared between the crawler (`run` attaches its pipeline) and the API.
pub struct DhtService {
    mode: Mode,
    pub upnp: super::upnp::Status,
    inner: Mutex<Inner>,
    /// One totals refresh at a time; concurrent requests serve the stale value meanwhile.
    refreshing: tokio::sync::Mutex<()>,
}

fn snap(p: &Pipeline, dht: &Dht) -> Snap {
    let e = dht.stats();
    let f = p.fetcher.stats();
    let persisted = p.persisted.lock().unwrap();
    Snap {
        unique: get(&p.c.unique),
        verified: f.ok + f.private + f.v2_only + f.invalid,
        failed: f.failed + f.oversize,
        blocked: persisted.blocked,
        incoming: e.incoming_ping
            + e.incoming_find_node
            + e.incoming_get_peers
            + e.incoming_announce_peer
            + e.incoming_sample_infohashes
            + e.incoming_unknown,
        queries: e.queries_sent,
        answered: e.answered,
        lookups: e.lookups,
        lookup_ms: e.lookup_ms_total,
        lookup_queries: e.lookup_queries,
        lookup_slow: e.lookup_slow,
        age_sent: [
            e.by_node_age.young_sent,
            e.by_node_age.old_sent,
            e.by_node_age.unpooled_sent,
            e.by_node_age.repeat_sent,
        ],
        age_answered: [
            e.by_node_age.young_answered,
            e.by_node_age.old_answered,
            e.by_node_age.unpooled_answered,
            e.by_node_age.repeat_answered,
        ],
        lookup_answered: e.lookup_answered,
        seeds_fresh: e.lookup_seeds_fresh,
        seeds: e.lookup_seeds_fresh + e.lookup_seeds_unknown + e.lookup_seeds_struck,
        get_peers: e.sent_get_peers,
        stored: persisted.stored,
    }
}

fn share(a: u64, b: u64) -> f64 {
    if b > 0 { a as f64 / b as f64 } else { 0.0 }
}

impl std::fmt::Debug for DhtService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DhtService")
            .field("mode", &self.mode)
            .finish()
    }
}

impl DhtService {
    pub fn new(mode: Mode) -> Arc<Self> {
        Arc::new(Self {
            mode,
            upnp: Arc::new(Mutex::new(UpnpStatus {
                state: "off".into(),
                ..Default::default()
            })),
            inner: Mutex::default(),
            refreshing: tokio::sync::Mutex::new(()),
        })
    }

    pub(super) fn attach(&self, p: Arc<Pipeline>, dht: Dht, started: Instant) {
        let s = snap(&p, &dht);
        let mut i = self.inner.lock().unwrap();
        i.running = Some(Running {
            p,
            dht,
            started,
            window: VecDeque::from([(started, s)]),
            bumped: Snap::default(),
        });
        i.last_error = None;
    }

    pub(super) fn detach(&self) {
        self.inner.lock().unwrap().running = None;
    }

    pub(super) fn set_error(&self, e: &str) {
        self.inner.lock().unwrap().last_error = Some(e.to_string());
    }

    /// Record a sample; returns the growth since the last bump (for `stats_series`).
    fn sample(&self) -> Option<Snap> {
        let mut i = self.inner.lock().unwrap();
        let r = i.running.as_mut()?;
        let s = snap(&r.p, &r.dht);
        r.window.push_back((Instant::now(), s));
        while r.window.len() > WINDOW {
            r.window.pop_front();
        }
        let d = Snap {
            unique: s.unique.saturating_sub(r.bumped.unique),
            verified: s.verified.saturating_sub(r.bumped.verified),
            failed: s.failed.saturating_sub(r.bumped.failed),
            blocked: s.blocked.saturating_sub(r.bumped.blocked),
            ..Snap::default()
        };
        r.bumped = s;
        Some(d)
    }

    /// The live part of `DhtStats` (no database).
    pub fn live(&self) -> DhtStats {
        let i = self.inner.lock().unwrap();
        let mut out = DhtStats {
            mode: match self.mode {
                Mode::Polite => "polite",
                Mode::Aggressive => "aggressive",
            }
            .into(),
            last_error: i.last_error.clone(),
            upnp: Some(self.upnp.lock().unwrap().clone()),
            ..Default::default()
        };
        let Some(r) = &i.running else {
            return out;
        };
        let now = Instant::now();
        let cur = snap(&r.p, &r.dht);
        let (t0, old) = r.window.front().copied().unwrap_or((r.started, cur));
        let mins = now.duration_since(t0).as_secs_f64().max(1.0) / 60.0;
        let per_min = |a: u64, b: u64| a.saturating_sub(b) as f64 / mins;
        let e = r.dht.stats();
        let f = r.p.fetcher.stats();
        let q = r.p.queue.lock().unwrap();
        let persisted = r.p.persisted.lock().unwrap();
        let queries = cur.queries.saturating_sub(old.queries);
        let finished =
            cur.verified.saturating_sub(old.verified) + cur.failed.saturating_sub(old.failed);
        out.running = true;
        out.uptime_secs = now.duration_since(r.started).as_secs();
        out.virtual_nodes = r.dht.virtual_nodes() as u32;
        out.ports = r.dht.local_ports();
        out.external_ip = match r.dht.identity().map(|id| IpAddr::V4(id.external_ip)) {
            Some(ip) => super::upnp::ip_class(ip).into(),
            None => "unknown".into(),
        };
        out.routing_nodes = e.routing.iter().sum::<usize>() as u64;
        out.pool_nodes = e.pool.size as u64;
        out.pool_bep51 = e.pool.bep51_yes as u64;
        out.queries_per_sec = queries as f64 / (mins * 60.0);
        out.answered_share = if queries > 0 {
            cur.answered.saturating_sub(old.answered) as f64 / queries as f64
        } else {
            0.0
        };
        out.brake = e.brake_active;
        out.incoming_per_min = per_min(cur.incoming, old.incoming);
        out.hashes_per_min = per_min(cur.unique, old.unique);
        out.hashes_seen = cur.unique;
        out.queue_len = q.len() as u64;
        out.queue_lanes = q.lane_lens().iter().map(|n| *n as u64).collect();
        out.sampling_paused = e.sample_share <= 0.0;
        out.metadata_per_min = per_min(cur.verified, old.verified);
        out.fetch_success_rate = if finished > 0 {
            cur.verified.saturating_sub(old.verified) as f64 / finished as f64
        } else {
            0.0
        };
        out.fetch_ok = f.ok;
        out.fetch_failed = f.failed + f.oversize;
        out.stored = persisted.stored;
        out.blocked = persisted.blocked;
        // D93.
        let c = &r.p.c;
        out.collected = get(&c.collected);
        out.from_source = get(&c.from_source);
        out.looked_up = get(&c.looked_up);
        out.announce_fetches = get(&c.fetch_announce);
        out.announce_upgraded = get(&c.announce_upgraded) + get(&c.announce_attached);
        out.incoming_ping = e.incoming_ping;
        out.incoming_find_node = e.incoming_find_node;
        out.incoming_get_peers = e.incoming_get_peers;
        out.incoming_announce_peer = e.incoming_announce_peer;
        out.incoming_sample_infohashes = e.incoming_sample_infohashes;
        out.incoming_unknown = e.incoming_unknown;
        out.fake_peers_skipped = f.fake_skipped;
        out.fake_peers_banned = f.fake_banned;
        out.fake_peers_tracked = f.fake_tracked as u64;
        let d = |a: u64, b: u64| a.saturating_sub(b);
        let lookups = d(cur.lookups, old.lookups);
        out.lookup_mean_ms = share(d(cur.lookup_ms, old.lookup_ms), lookups);
        out.queries_per_lookup = share(d(cur.lookup_queries, old.lookup_queries), lookups);
        out.lookup_slow_share = share(
            d(cur.lookup_slow, old.lookup_slow),
            d(cur.lookup_queries, old.lookup_queries),
        );
        let age = |i: usize| {
            share(
                d(cur.age_answered[i], old.age_answered[i]),
                d(cur.age_sent[i], old.age_sent[i]),
            )
        };
        out.answered_share_young = age(0);
        out.answered_share_old = age(1);
        out.answered_share_unpooled = age(2);
        out.answered_share_repeat = age(3);
        // D95.
        out.lookup_answered_share = share(
            d(cur.lookup_answered, old.lookup_answered),
            d(cur.lookup_queries, old.lookup_queries),
        );
        out.lookup_seeds_fresh_share =
            share(d(cur.seeds_fresh, old.seeds_fresh), d(cur.seeds, old.seeds));
        out.queries_per_stored = share(d(cur.get_peers, old.get_peers), d(cur.stored, old.stored));
        out.pool_dropped_dead = e.pool.dropped_dead;
        out.lookup_dead_skipped = e.lookup_dead_skipped;
        out
    }

    /// `live()` plus the database totals (cached for a minute: `dht_seen` can be large).
    pub async fn stats(&self, pool: &PgPool) -> DhtStats {
        let mut out = self.live();
        let cached = || {
            let i = self.inner.lock().unwrap();
            i.totals
                .as_ref()
                .map(|(at, t)| (at.elapsed() < TOTALS_TTL, t.clone()))
        };
        let totals = match cached() {
            Some((true, t)) => Some(t),
            stale => match self.refreshing.try_lock() {
                // Another request is counting: the stale value (or none yet) meanwhile.
                Err(_) => stale.map(|(_, t)| t),
                Ok(_guard) => match totals(pool).await {
                    Ok(t) => {
                        self.inner.lock().unwrap().totals = Some((Instant::now(), t.clone()));
                        Some(t)
                    }
                    Err(e) => {
                        tracing::warn!("DHT stats: {e}");
                        // Do not retry on every request: keep the old value for another TTL.
                        if let Some((_, t)) = &stale {
                            self.inner.lock().unwrap().totals = Some((Instant::now(), t.clone()));
                        }
                        stale.map(|(_, t)| t)
                    }
                },
            },
        };
        if let Some(t) = totals {
            let u = |v: i64| u64::try_from(v).unwrap_or(0);
            out.stored_total = u(t.stored);
            out.adult_total = u(t.adult);
            out.blocked_total = u(t.blocked);
            out.seen_queued = u(t.queued);
            out.seen_failed = u(t.failed);
        }
        out
    }
}

async fn totals(pool: &PgPool) -> Result<Totals, sqlx::Error> {
    let mut tx = pool.begin().await?;
    sqlx::query("SELECT set_config('statement_timeout', $1, true)")
        .bind(format!("{TOTALS_TIMEOUT_MS}ms"))
        .execute(&mut *tx)
        .await?;
    let t = sqlx::query_as(
        "SELECT (SELECT count(*) FROM dht_torrents) AS stored,
                (SELECT count(*) FROM dht_torrents WHERE category = 'porn') AS adult,
                (SELECT count(*) FROM blocked_infohashes) AS blocked,
                (SELECT count(*) FROM dht_seen WHERE status = 0) AS queued,
                (SELECT count(*) FROM dht_seen WHERE status = 2) AS failed",
    )
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(t)
}

/// Every minute: one rate sample, and the growth of the counters into `stats_series`.
pub(super) async fn sampler(
    svc: Arc<DhtService>,
    pool: Option<PgPool>,
    mut stop: watch::Receiver<bool>,
) {
    loop {
        tokio::select! {
            _ = tokio::time::sleep(SAMPLE_EVERY) => {}
            _ = stop_requested(&mut stop) => return,
        }
        let Some(d) = svc.sample() else { continue };
        if let Some(pool) = &pool {
            for (metric, n) in [
                (DHT_HASHES_SEEN, d.unique),
                (DHT_FETCH_OK, d.verified),
                (DHT_FETCH_FAIL, d.failed),
                (DHT_BLOCKED, d.blocked),
            ] {
                if n > 0 {
                    stats::bump(pool, metric, i64::try_from(n).unwrap_or(i64::MAX)).await;
                }
            }
        }
    }
}

impl DhtService {
    /// The server's line in `/api/v1/stats/sources.machines` (D102): counters of the running
    /// crawler since it started. None while it is not running.
    pub fn machine(&self, now: i64) -> Option<magnetar_core::api::MachineSources> {
        let i = self.inner.lock().unwrap();
        let r = i.running.as_ref()?;
        let c = &r.p.c;
        let f = r.p.fetcher.stats();
        let route = |k: usize| get(&c.by_route[k]);
        Some(magnetar_core::api::MachineSources {
            name: "server".into(),
            kind: "server".into(),
            connected: true,
            since: Some(now - r.started.elapsed().as_secs() as i64),
            dht_triaged: get(&c.unique),
            dht_queued: get(&c.queued),
            dht_metadata: f.ok + f.private + f.v2_only + f.invalid,
            dht_stored: r.p.persisted.lock().unwrap().stored,
            dht_triaged_by_route: magnetar_core::api::RouteTriple {
                sample: route(0),
                get_peers: route(1),
                announce: route(2),
            },
            ..Default::default()
        })
    }
}

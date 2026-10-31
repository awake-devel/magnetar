//! Seed checks at scale (D45, docs/LIVENESS.md): the schedule, the checker loop, history and
//! backlog stats. The scraping itself is `magnetar_core::liveness::Scraper`.
//!
//! Every infohash has a `liveness_schedule` row (created by a trigger on `torrents`). The loop
//! takes the due rows lane by lane (viewed, saved-search matches, search results, the rest),
//! checks them as one batch, then writes results, next due times and history points in one
//! transaction, rows in infohash order. A check without any answer keeps the previous result
//! (only the schedule moves), so a tracker outage does not wipe seeders from search results.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use magnetar_core::api::{LivenessHistory, LivenessPoint, LivenessStats};
use magnetar_core::error::Result;
use magnetar_core::liveness::{self, BatchReport, LivenessConfig, Scraper, Streaks};
use magnetar_core::magnet::DEFAULT_TRACKERS;
use magnetar_core::model::{Liveness, LivenessSource};
use magnetar_core::store::{Store, keys};
use sqlx::{FromRow, PgPool};
use tokio::sync::watch;

use crate::config::Config;
use crate::daemons::stop_requested;
use crate::store::{PgStore, db, liveness_source_str};

/// Lanes of the due query, highest first.
pub const PRIO_VIEWED: i16 = 3;
pub const PRIO_SAVED: i16 = 2;
pub const PRIO_SEARCH: i16 = 1;
pub const PRIO_NORMAL: i16 = 0;
/// A boost makes a torrent due at once when its last check is older than this.
const STALE_VIEWED: i64 = 3600;
const STALE_SAVED: i64 = 6 * 3600;
const STALE_SEARCH: i64 = 12 * 3600;
/// How long a boost keeps a torrent hot (re-checked every `liveness::TTL_NEW`).
pub const HOT_FOR: i64 = 3 * 86400;
/// Boosts waiting for the next flush; more are dropped (the next search brings them back).
const MAX_PENDING_BOOSTS: usize = 50_000;

/// History: one point per infohash at most this often (a 6 h re-check with −10 % jitter still
/// gets one), an unchanged value (see `moved`) at most weekly, 90 days kept.
pub const HISTORY_STEP: i64 = 5 * 3600;
pub const HISTORY_UNCHANGED_EVERY: i64 = 7 * 86400;
pub const HISTORY_KEEP_DAYS: u32 = 90;
/// Pruning, saved-search boosts: at most this often.
const MAINTENANCE_EVERY: Duration = Duration::from_secs(3600);
/// Saved-search results boosted per search.
const SAVED_SEARCH_BOOST: u32 = 100;
/// Pause between batches while more is due than one batch takes.
const BACKLOG_PAUSE: Duration = Duration::from_secs(1);
/// Longest DHT walk of the checker (most end after 1-7 s, D45).
const SERVER_DHT_BUDGET: Duration = Duration::from_secs(10);
/// `/api/v1/stats` reuses counts this fresh (two scans of ~0.5 M narrow rows each).
const STATS_MAX_AGE: Duration = Duration::from_secs(30);
/// Advisory lock taken by every claim of due rows (server batches and worker leases, D98), so
/// two holders never take the same infohash.
pub const LEASE_LOCK: i64 = 0x6d61_676e_6c65_6173; // "magnleas"
/// Holder name of the server's own checker in `liveness_leases`.
pub const SERVER_HOLDER: &str = "";
/// How long the server's own batch holds its rows (a crash returns them after this).
pub const SERVER_CLAIM_SECS: i64 = 1800;
/// Lease rows this far past their end are removed by the hourly maintenance.
const LEASE_KEEP_SECS: i64 = 86400;

/// The checker's shared state: scraper (tracker sockets, rate limits, backoff, DHT nodes),
/// pending boosts from the API, the last batch report.
pub struct LivenessService {
    scraper: Scraper,
    boosts: Mutex<HashMap<String, i16>>,
    last: Mutex<Option<(i64, BatchReport)>>,
    stats: tokio::sync::Mutex<Option<(Instant, LivenessStats)>>,
    /// Infohashes the server's own batches checked since the start (D102).
    checked: std::sync::atomic::AtomicU64,
}

impl Default for LivenessService {
    fn default() -> Self {
        Self::new(LivenessConfig::default())
    }
}

impl LivenessService {
    pub fn new(cfg: LivenessConfig) -> Self {
        Self {
            scraper: Scraper::new(cfg),
            boosts: Mutex::default(),
            last: Mutex::default(),
            stats: tokio::sync::Mutex::default(),
            checked: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// Seed checks written by the server's own batches since the start.
    pub fn checked_total(&self) -> u64 {
        self.checked.load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn config(&self) -> &LivenessConfig {
        self.scraper.config()
    }

    /// (failures in a row, backing off now) of one tracker, None if never asked (D47 swarm view).
    pub fn tracker_state(&self, url: &str) -> Option<(u32, bool)> {
        self.scraper.tracker_state(url)
    }

    /// Ask for these infohashes to be checked sooner (lane `prio`); applied by the loop
    /// before its next batch, so the API never writes for it.
    pub fn boost<'a>(&self, infohashes: impl IntoIterator<Item = &'a str>, prio: i16) {
        let mut b = self.boosts.lock().unwrap();
        for ih in infohashes {
            if b.len() >= MAX_PENDING_BOOSTS && !b.contains_key(ih) {
                break;
            }
            let p = b.entry(ih.to_string()).or_insert(prio);
            *p = (*p).max(prio);
        }
    }

    /// Write pending boosts to the schedule.
    pub async fn flush_boosts(&self, pool: &PgPool, now: i64) -> Result<usize> {
        let mut pending: Vec<(String, i16)> = std::mem::take(&mut *self.boosts.lock().unwrap())
            .into_iter()
            .collect();
        if pending.is_empty() {
            return Ok(0);
        }
        pending.sort();
        let stale = |p: i16| match p {
            PRIO_VIEWED => STALE_VIEWED,
            PRIO_SAVED => STALE_SAVED,
            _ => STALE_SEARCH,
        };
        let ihs: Vec<&str> = pending.iter().map(|(h, _)| h.as_str()).collect();
        let prios: Vec<i16> = pending.iter().map(|(_, p)| *p).collect();
        let stales: Vec<i64> = pending.iter().map(|(_, p)| stale(*p)).collect();
        let n = sqlx::query(
            "UPDATE liveness_schedule s SET prio = greatest(s.prio, b.prio),
                 hot_until = greatest(coalesce(s.hot_until, 0), $4),
                 due_at = CASE WHEN s.checked_at IS NULL THEN s.due_at
                               ELSE least(s.due_at, s.checked_at + b.stale) END
             FROM unnest($1::text[], $2::smallint[], $3::bigint[]) AS b(infohash, prio, stale)
             WHERE s.infohash = b.infohash",
        )
        .bind(&ihs)
        .bind(&prios)
        .bind(&stales)
        .bind(now + HOT_FOR)
        .execute(pool)
        .await
        .map_err(db)?
        .rows_affected();
        Ok(n as usize)
    }

    /// One batch: due rows → check → results. Returns how many were checked. The rows are
    /// claimed first (D98): crawl workers never get them while this batch runs.
    pub async fn run_batch(&self, pool: &PgPool, now: i64, limit: usize) -> Result<usize> {
        let rows = claim(pool, now, limit, SERVER_HOLDER, SERVER_CLAIM_SECS).await?;
        if rows.is_empty() {
            return Ok(0);
        }
        let items: Vec<(String, Vec<String>)> = rows
            .iter()
            .map(|r| (r.infohash.clone(), r.trackers.clone()))
            .collect();
        let (results, report) = self.scraper.check(&items, now).await;
        // Scheduled from the batch's start, like the results' `checked_at`.
        write_results(pool, &rows, &results, now, Expiry::Scheduled).await?;
        self.checked
            .fetch_add(rows.len() as u64, std::sync::atomic::Ordering::Relaxed);
        crate::stats::bump(pool, crate::stats::LIVENESS_CHECKS, rows.len() as i64).await;
        tracing::info!(
            checked = report.hashes,
            alive = report.alive,
            dead = report.dead,
            unknown = report.unknown,
            requests = report.tracker_requests,
            tracker_failures = report.tracker_failures,
            backed_off = report.trackers_backed_off,
            dht_walks = report.dht_walks,
            "liveness: batch in {} ms",
            report.ms
        );
        *self.last.lock().unwrap() = Some((magnetar_core::now(), report));
        Ok(rows.len())
    }

    /// Backlog counts for `/api/v1/stats`, cached for [`STATS_MAX_AGE`].
    pub async fn stats(&self, store: &PgStore, now: i64) -> Result<LivenessStats> {
        let mut cache = self.stats.lock().await;
        let mut s = match cache.as_ref() {
            Some((at, s)) if at.elapsed() < STATS_MAX_AGE => s.clone(),
            _ => {
                let s = count_stats(store.pool(), now).await?;
                *cache = Some((Instant::now(), s.clone()));
                s
            }
        };
        drop(cache);
        s.enabled = enabled(store).await;
        let (trackers, backed_off, nodes) = self.scraper.state();
        s.trackers = trackers as u64;
        s.trackers_backed_off = backed_off as u64;
        s.dht_nodes = nodes as u64;
        if let Some((at, r)) = self.last.lock().unwrap().clone() {
            s.last_batch_at = Some(at);
            s.last_batch = Some(r);
        }
        Ok(s)
    }
}

/// Server checker config from `MAGNETAR_LIVENESS_*` (docs/CONFIG.md). Torrents without a
/// tracker are scraped on the trackers their magnet links get (`magnet::DEFAULT_TRACKERS`).
/// DHT walks wait for permits of `pacer`, the process's shared DHT pacer (D109).
pub fn liveness_config(
    cfg: &Config,
    pacer: Option<Arc<magnetar_core::dht::limiter::Pacer>>,
) -> LivenessConfig {
    LivenessConfig {
        dht_pacer: pacer.map(|p| p as Arc<dyn magnetar_core::liveness::dht::WalkPacer>),
        tracker_concurrency: cfg.liveness_tracker_concurrency.max(1),
        tracker_rps: cfg.liveness_tracker_rps,
        use_dht: cfg.liveness_dht,
        dht_concurrency: cfg.liveness_dht_concurrency.max(1),
        dht_budget: SERVER_DHT_BUDGET,
        fallback_trackers: DEFAULT_TRACKERS.iter().map(|t| t.to_string()).collect(),
        ..LivenessConfig::default()
    }
}

/// A due infohash with what the scheduler needs to know about it.
#[derive(Clone, Debug, FromRow)]
pub struct DueRow {
    pub infohash: String,
    pub born: i64,
    pub hot_until: Option<i64>,
    pub dead_streak: i16,
    pub error_streak: i16,
    pub history_at: Option<i64>,
    /// The stored result (None: never answered).
    pub has_result: bool,
    pub seeders: Option<i32>,
    pub leechers: Option<i32>,
    pub dht_peers_est: Option<i32>,
    pub trackers: Vec<String>,
}

const DUE_COLUMNS: &str = "s.infohash, s.born, s.hot_until, s.dead_streak, s.error_streak,
    s.history_at, l.infohash IS NOT NULL AS has_result, l.seeders, l.leechers, l.dht_peers_est,
    coalesce((SELECT array_agg(DISTINCT u) FROM torrents t, unnest(t.trackers) AS u
              WHERE t.infohash = s.infohash), '{}') AS trackers";

/// Up to `limit` due infohashes: lane by lane (viewed, saved, search, rest), each in due order.
/// Each branch is a range scan of `liveness_schedule_due` that stops at the limit.
pub async fn due<'c, E: sqlx::PgExecutor<'c>>(
    pool: E,
    now: i64,
    limit: usize,
) -> Result<Vec<DueRow>> {
    let lanes = [PRIO_VIEWED, PRIO_SAVED, PRIO_SEARCH, PRIO_NORMAL]
        .iter()
        .map(|p| {
            format!(
                "(SELECT infohash, {p} AS prio, due_at FROM liveness_schedule
                  WHERE prio = {p} AND due_at <= $1 ORDER BY due_at LIMIT $2)"
            )
        })
        .collect::<Vec<_>>()
        .join(" UNION ALL ");
    let sql = format!(
        "WITH picked AS MATERIALIZED (SELECT * FROM ({lanes}) lanes LIMIT $2)
         SELECT {DUE_COLUMNS}
         FROM picked p JOIN liveness_schedule s ON s.infohash = p.infohash
         LEFT JOIN liveness l ON l.infohash = p.infohash
         ORDER BY p.prio DESC, p.due_at"
    );
    // Only constants are formatted in.
    sqlx::query_as(sqlx::AssertSqlSafe(sql))
        .bind(now)
        .bind(limit as i64)
        .fetch_all(pool)
        .await
        .map_err(db)
}

/// Take up to `limit` due rows for `holder` until `now + ttl` (D98): under [`LEASE_LOCK`], the
/// due query, then `due_at` moved to the lease's end and the holder recorded in
/// `liveness_leases`, in one transaction. Rows nobody answers for are due again at the end.
pub async fn claim(
    pool: &PgPool,
    now: i64,
    limit: usize,
    holder: &str,
    ttl: i64,
) -> Result<Vec<DueRow>> {
    let mut tx = pool.begin().await.map_err(db)?;
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(LEASE_LOCK)
        .execute(&mut *tx)
        .await
        .map_err(db)?;
    let rows = due(&mut *tx, now, limit).await?;
    if !rows.is_empty() {
        let mut ihs: Vec<&str> = rows.iter().map(|r| r.infohash.as_str()).collect();
        ihs.sort_unstable();
        ihs.dedup();
        let until = now + ttl.max(1);
        sqlx::query(
            "UPDATE liveness_schedule s SET due_at = $2
             FROM unnest($1::text[]) AS u(infohash) WHERE s.infohash = u.infohash",
        )
        .bind(&ihs)
        .bind(until)
        .execute(&mut *tx)
        .await
        .map_err(db)?;
        sqlx::query(
            "INSERT INTO liveness_leases (infohash, worker, leased_at, expires_at)
             SELECT u, $2, $3, $4 FROM unnest($1::text[]) AS u ORDER BY u
             ON CONFLICT (infohash) DO UPDATE SET worker = EXCLUDED.worker,
                 leased_at = EXCLUDED.leased_at, expires_at = EXCLUDED.expires_at",
        )
        .bind(&ihs)
        .bind(holder)
        .bind(now)
        .bind(until)
        .execute(&mut *tx)
        .await
        .map_err(db)?;
    }
    tx.commit().await.map_err(db)?;
    Ok(rows)
}

/// Schedule rows of the infohashes `holder` still holds a lease on (expired or not, as long as
/// nobody took them since), for writing its results.
pub async fn held_rows(pool: &PgPool, holder: &str, infohashes: &[String]) -> Result<Vec<DueRow>> {
    let sql = format!(
        "SELECT {DUE_COLUMNS} FROM liveness_schedule s
         JOIN liveness_leases k ON k.infohash = s.infohash AND k.worker = $2
         LEFT JOIN liveness l ON l.infohash = s.infohash
         WHERE s.infohash = ANY($1)"
    );
    sqlx::query_as(sqlx::AssertSqlSafe(sql))
        .bind(infohashes)
        .bind(holder)
        .fetch_all(pool)
        .await
        .map_err(db)
}

/// Give back leases unchecked: due at `now`, lease rows gone. Returns how many.
pub async fn release(pool: &PgPool, holder: &str, infohashes: &[String], now: i64) -> Result<u64> {
    let mut ihs: Vec<&str> = infohashes.iter().map(String::as_str).collect();
    ihs.sort_unstable();
    ihs.dedup();
    let mut tx = pool.begin().await.map_err(db)?;
    let n = sqlx::query(
        "UPDATE liveness_schedule s SET due_at = least(s.due_at, $3)
         FROM liveness_leases k
         WHERE k.infohash = s.infohash AND k.worker = $2 AND s.infohash = ANY($1)",
    )
    .bind(&ihs)
    .bind(holder)
    .bind(now)
    .execute(&mut *tx)
    .await
    .map_err(db)?
    .rows_affected();
    sqlx::query("DELETE FROM liveness_leases WHERE infohash = ANY($1) AND worker = $2")
        .bind(&ihs)
        .bind(holder)
        .execute(&mut *tx)
        .await
        .map_err(db)?;
    tx.commit().await.map_err(db)?;
    Ok(n)
}

/// Unexpired leases per holder.
pub async fn leases_held(pool: &PgPool, now: i64) -> Result<HashMap<String, u64>> {
    let rows: Vec<(String, i64)> = sqlx::query_as(
        "SELECT worker, count(*) FROM liveness_leases WHERE expires_at > $1 GROUP BY worker",
    )
    .bind(now)
    .fetch_all(pool)
    .await
    .map_err(db)?;
    Ok(rows
        .into_iter()
        .map(|(w, n)| (w, u64::try_from(n).unwrap_or(0)))
        .collect())
}

/// Drop lease rows a day past their end (their torrent was deleted, or nobody took the
/// infohash again yet; either way the row says nothing any more).
pub async fn prune_leases(pool: &PgPool, now: i64) -> Result<u64> {
    let r = sqlx::query("DELETE FROM liveness_leases WHERE expires_at < $1")
        .bind(now - LEASE_KEEP_SECS)
        .execute(pool)
        .await
        .map_err(db)?;
    Ok(r.rows_affected())
}

/// The schedule row of one infohash (for a single forced check).
pub async fn due_row(pool: &PgPool, infohash: &str) -> Result<Option<DueRow>> {
    let sql = format!(
        "SELECT {DUE_COLUMNS} FROM liveness_schedule s
         LEFT JOIN liveness l ON l.infohash = s.infohash WHERE s.infohash = $1"
    );
    sqlx::query_as(sqlx::AssertSqlSafe(sql))
        .bind(infohash)
        .fetch_optional(pool)
        .await
        .map_err(db)
}

fn opt_i32(v: Option<u32>) -> Option<i32> {
    v.map(|v| i32::try_from(v).unwrap_or(i32::MAX))
}

fn small(v: u16) -> i16 {
    i16::try_from(v).unwrap_or(i16::MAX)
}

/// A history-worthy change: dead ↔ alive, or more than ±1 and ±10 % (smaller wobble waits
/// for the weekly point, D45).
fn moved(now: Option<i32>, before: Option<i32>) -> bool {
    match (now, before) {
        (Some(a), Some(b)) => (a == 0) != (b == 0) || (a - b).abs() > 1.max(a.max(b) / 10),
        (None, None) => false,
        _ => true,
    }
}

struct SchedUpdate<'a> {
    infohash: &'a str,
    due_at: i64,
    dead: i16,
    error: i16,
    history_at: Option<i64>,
}

struct HistPoint<'a> {
    infohash: &'a str,
    seeders: Option<i32>,
    leechers: Option<i32>,
    peers_est: Option<i32>,
}

/// What a stored result's `expires_at` (how long it counts as fresh) is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Expiry {
    /// The next scheduled check: a result stays fresh until it is replaced.
    Scheduled,
    /// The caller's value, also as the next check (`Store::put_liveness` stores what it is
    /// given).
    AsGiven,
}

/// Store `results` (positional to `rows`): the result itself when it answered (or when there
/// was none yet), the next due time and streaks always, a history point when one is due.
pub async fn write_results(
    pool: &PgPool,
    rows: &[DueRow],
    results: &[Liveness],
    now: i64,
    expiry: Expiry,
) -> Result<()> {
    let mut order: Vec<usize> = (0..rows.len().min(results.len())).collect();
    order.sort_by(|&a, &b| rows[a].infohash.cmp(&rows[b].infohash));
    order.dedup_by(|a, b| rows[*a].infohash == rows[*b].infohash);

    let mut live: Vec<Liveness> = Vec::new();
    let mut sched: Vec<SchedUpdate> = Vec::new();
    let mut hist: Vec<HistPoint> = Vec::new();
    for i in order {
        let (r, l) = (&rows[i], &results[i]);
        let hot = r.hot_until.is_some_and(|h| h > now);
        let prev = Streaks {
            dead: r.dead_streak.max(0) as u16,
            error: r.error_streak.max(0) as u16,
        };
        let jitter = rand::random::<f64>() * 2.0 - 1.0;
        let (scheduled, streaks) = liveness::next_check(l, r.born, prev, hot, now, jitter);
        let due_at = match expiry {
            Expiry::Scheduled => scheduled,
            Expiry::AsGiven => l.expires_at,
        };
        let answered = l.source != LivenessSource::None;
        let mut history_at = None;
        if answered {
            let point = (
                opt_i32(l.seeders),
                opt_i32(l.leechers),
                opt_i32(l.dht_peers_est),
            );
            let changed = !r.has_result
                || moved(point.0, r.seeders)
                || moved(point.1, r.leechers)
                || moved(point.2, r.dht_peers_est);
            let since = r.history_at.map(|h| now - h);
            let due_point = match since {
                None => true,
                Some(s) => s >= HISTORY_STEP && (changed || s >= HISTORY_UNCHANGED_EVERY),
            };
            if due_point {
                hist.push(HistPoint {
                    infohash: &r.infohash,
                    seeders: point.0,
                    leechers: point.1,
                    peers_est: point.2,
                });
                history_at = Some(now);
            }
        }
        if answered || !r.has_result {
            live.push(Liveness {
                infohash: r.infohash.clone(),
                expires_at: due_at,
                ..l.clone()
            });
        }
        sched.push(SchedUpdate {
            infohash: &r.infohash,
            due_at,
            dead: small(streaks.dead),
            error: small(streaks.error),
            history_at,
        });
    }

    let mut tx = pool.begin().await.map_err(db)?;
    // A written result ends any lease on the infohash (D98): a worker's late answer for it is
    // then ignored.
    sqlx::query("DELETE FROM liveness_leases WHERE infohash = ANY($1)")
        .bind(sched.iter().map(|s| s.infohash).collect::<Vec<_>>())
        .execute(&mut *tx)
        .await
        .map_err(db)?;
    if !live.is_empty() {
        let col = |f: &dyn Fn(&Liveness) -> Option<i32>| live.iter().map(f).collect::<Vec<_>>();
        sqlx::query(
            "INSERT INTO liveness (infohash, seeders, leechers, completed, dht_peers_est, source,
                trackers_ok, trackers_failed, checked_at, expires_at)
             SELECT * FROM unnest($1::text[], $2::int[], $3::int[], $4::int[], $5::int[],
                                  $6::text[], $7::int[], $8::int[], $9::bigint[], $10::bigint[])
             ON CONFLICT (infohash) DO UPDATE SET seeders = EXCLUDED.seeders,
               leechers = EXCLUDED.leechers, completed = EXCLUDED.completed,
               dht_peers_est = EXCLUDED.dht_peers_est, source = EXCLUDED.source,
               trackers_ok = EXCLUDED.trackers_ok, trackers_failed = EXCLUDED.trackers_failed,
               checked_at = EXCLUDED.checked_at, expires_at = EXCLUDED.expires_at",
        )
        .bind(live.iter().map(|l| l.infohash.as_str()).collect::<Vec<_>>())
        .bind(col(&|l| opt_i32(l.seeders)))
        .bind(col(&|l| opt_i32(l.leechers)))
        .bind(col(&|l| opt_i32(l.completed)))
        .bind(col(&|l| opt_i32(l.dht_peers_est)))
        .bind(
            live.iter()
                .map(|l| liveness_source_str(l.source))
                .collect::<Vec<_>>(),
        )
        .bind(
            live.iter()
                .map(|l| i32::from(l.trackers_ok))
                .collect::<Vec<_>>(),
        )
        .bind(
            live.iter()
                .map(|l| i32::from(l.trackers_failed))
                .collect::<Vec<_>>(),
        )
        .bind(live.iter().map(|l| l.checked_at).collect::<Vec<_>>())
        .bind(live.iter().map(|l| l.expires_at).collect::<Vec<_>>())
        .execute(&mut *tx)
        .await
        .map_err(db)?;
    }
    // A boost that arrived meanwhile keeps its lane; an expired one goes back to lane 0.
    sqlx::query(
        "UPDATE liveness_schedule s SET due_at = u.due_at, checked_at = $6,
             dead_streak = u.dead, error_streak = u.err,
             history_at = coalesce(u.history_at, s.history_at),
             prio = CASE WHEN s.hot_until > $6 THEN s.prio ELSE 0 END,
             hot_until = CASE WHEN s.hot_until > $6 THEN s.hot_until END
         FROM unnest($1::text[], $2::bigint[], $3::smallint[], $4::smallint[], $5::bigint[])
              AS u(infohash, due_at, dead, err, history_at)
         WHERE s.infohash = u.infohash",
    )
    .bind(sched.iter().map(|s| s.infohash).collect::<Vec<_>>())
    .bind(sched.iter().map(|s| s.due_at).collect::<Vec<_>>())
    .bind(sched.iter().map(|s| s.dead).collect::<Vec<_>>())
    .bind(sched.iter().map(|s| s.error).collect::<Vec<_>>())
    .bind(sched.iter().map(|s| s.history_at).collect::<Vec<_>>())
    .bind(now)
    .execute(&mut *tx)
    .await
    .map_err(db)?;
    if !hist.is_empty() {
        sqlx::query(
            // Joined so an infohash deleted since the due query (its schedule row went with
            // it) is skipped instead of failing the whole batch on the foreign key.
            "INSERT INTO liveness_history (infohash, at, seeders, leechers, peers_est)
             SELECT u.* FROM unnest($1::text[], $2::bigint[], $3::int[], $4::int[], $5::int[])
                  AS u(infohash, at, seeders, leechers, peers_est)
             JOIN torrent_infohashes ti ON ti.infohash = u.infohash
             ON CONFLICT DO NOTHING",
        )
        .bind(hist.iter().map(|h| h.infohash).collect::<Vec<_>>())
        .bind(vec![now; hist.len()])
        .bind(hist.iter().map(|h| h.seeders).collect::<Vec<_>>())
        .bind(hist.iter().map(|h| h.leechers).collect::<Vec<_>>())
        .bind(hist.iter().map(|h| h.peers_est).collect::<Vec<_>>())
        .execute(&mut *tx)
        .await
        .map_err(db)?;
    }
    tx.commit().await.map_err(db)?;
    Ok(())
}

/// Store one result through the schedule (forced checks, `Store::put_liveness`). An infohash
/// without a schedule row (no torrent) only gets its result row, as before D45.
pub async fn put_one(pool: &PgPool, l: &Liveness, expiry: Expiry) -> Result<()> {
    match due_row(pool, &l.infohash).await? {
        Some(row) => {
            write_results(pool, &[row], std::slice::from_ref(l), l.checked_at, expiry).await
        }
        None => {
            // No schedule row to update and no history (it references torrent_infohashes).
            let mut tx = pool.begin().await.map_err(db)?;
            sqlx::query(
                "INSERT INTO liveness (infohash, seeders, leechers, completed, dht_peers_est,
                    source, trackers_ok, trackers_failed, checked_at, expires_at)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
                 ON CONFLICT (infohash) DO UPDATE SET seeders = EXCLUDED.seeders,
                   leechers = EXCLUDED.leechers, completed = EXCLUDED.completed,
                   dht_peers_est = EXCLUDED.dht_peers_est, source = EXCLUDED.source,
                   trackers_ok = EXCLUDED.trackers_ok, trackers_failed = EXCLUDED.trackers_failed,
                   checked_at = EXCLUDED.checked_at, expires_at = EXCLUDED.expires_at",
            )
            .bind(&l.infohash)
            .bind(opt_i32(l.seeders))
            .bind(opt_i32(l.leechers))
            .bind(opt_i32(l.completed))
            .bind(opt_i32(l.dht_peers_est))
            .bind(liveness_source_str(l.source))
            .bind(i32::from(l.trackers_ok))
            .bind(i32::from(l.trackers_failed))
            .bind(l.checked_at)
            .bind(l.expires_at)
            .execute(&mut *tx)
            .await
            .map_err(db)?;
            tx.commit().await.map_err(db)
        }
    }
}

#[derive(FromRow)]
struct PointRow {
    at: i64,
    seeders: Option<i32>,
    leechers: Option<i32>,
    peers_est: Option<i32>,
}

/// History of one infohash over the last `days` (None: unknown infohash).
pub async fn history(
    pool: &PgPool,
    infohash: &str,
    days: u32,
    now: i64,
) -> Result<Option<LivenessHistory>> {
    let known: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM torrent_infohashes WHERE infohash = $1)")
            .bind(infohash)
            .fetch_one(pool)
            .await
            .map_err(db)?;
    if !known {
        return Ok(None);
    }
    let since = now - i64::from(days.clamp(1, HISTORY_KEEP_DAYS)) * 86400;
    let rows: Vec<PointRow> = sqlx::query_as(
        "SELECT at, seeders, leechers, peers_est FROM liveness_history
         WHERE infohash = $1 AND at >= $2 ORDER BY at",
    )
    .bind(infohash)
    .bind(since)
    .fetch_all(pool)
    .await
    .map_err(db)?;
    let u = |v: Option<i32>| v.and_then(|v| u32::try_from(v).ok());
    Ok(Some(LivenessHistory {
        infohash: infohash.to_string(),
        points: rows
            .into_iter()
            .map(|r| LivenessPoint {
                at: r.at,
                seeders: u(r.seeders),
                leechers: u(r.leechers),
                peers_est: u(r.peers_est),
            })
            .collect(),
        keep_days: HISTORY_KEEP_DAYS,
    }))
}

/// Drop history older than [`HISTORY_KEEP_DAYS`].
pub async fn prune_history(pool: &PgPool, now: i64) -> Result<u64> {
    let cutoff = now - i64::from(HISTORY_KEEP_DAYS) * 86400;
    let r = sqlx::query("DELETE FROM liveness_history WHERE at < $1")
        .bind(cutoff)
        .execute(pool)
        .await
        .map_err(db)?;
    Ok(r.rows_affected())
}

#[derive(FromRow)]
struct CountRow {
    torrents: i64,
    never_checked: i64,
    due_now: i64,
    checked_24h: i64,
    checked_1h: i64,
    alive: i64,
    dead: i64,
    unknown: i64,
}

async fn count_stats(pool: &PgPool, now: i64) -> Result<LivenessStats> {
    let r: CountRow = sqlx::query_as(
        "SELECT s.*, l.* FROM
           (SELECT count(*) AS torrents,
                   count(*) FILTER (WHERE checked_at IS NULL) AS never_checked,
                   count(*) FILTER (WHERE due_at <= $1) AS due_now,
                   count(*) FILTER (WHERE checked_at > $1 - 86400) AS checked_24h,
                   count(*) FILTER (WHERE checked_at > $1 - 3600) AS checked_1h
            FROM liveness_schedule) s,
           (SELECT count(*) FILTER (WHERE greatest(seeders, dht_peers_est) > 0) AS alive,
                   count(*) FILTER (WHERE greatest(seeders, dht_peers_est) = 0) AS dead,
                   count(*) FILTER (WHERE seeders IS NULL AND dht_peers_est IS NULL) AS unknown
            FROM liveness) l",
    )
    .bind(now)
    .fetch_one(pool)
    .await
    .map_err(db)?;
    let u = |v: i64| u64::try_from(v).unwrap_or(0);
    Ok(LivenessStats {
        torrents: u(r.torrents),
        never_checked: u(r.never_checked),
        due_now: u(r.due_now),
        checked_24h: u(r.checked_24h),
        checks_per_hour: u(r.checked_1h),
        alive: u(r.alive),
        dead: u(r.dead),
        unknown: u(r.unknown),
        ..Default::default()
    })
}

async fn enabled(store: &PgStore) -> bool {
    store
        .get_setting(keys::LIVENESS_ENABLED)
        .await
        .ok()
        .flatten()
        .is_none_or(|v| v.trim().trim_matches('"') != "false")
}

/// Boost the first results of every saved search (they are what the notifier reports).
async fn boost_saved_searches(store: &PgStore, svc: &LivenessService, now: i64) {
    let searches = match store.list_saved_searches().await {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("liveness: saved searches: {e}");
            return;
        }
    };
    for s in searches {
        let mut q = s.query.clone();
        q.limit = SAVED_SEARCH_BOOST;
        q.offset = 0;
        match store.search(&q, now).await {
            Ok(page) => svc.boost(page.hits.iter().map(|h| h.infohash.as_str()), PRIO_SAVED),
            Err(e) => tracing::debug!("liveness: saved search {}: {e}", s.id),
        }
    }
}

/// `magnetar-server liveness --limit N`: batches until `limit` infohashes were checked or
/// nothing is due; one JSON report per batch on stdout (with the time the write took).
pub async fn run_once(store: &PgStore, cfg: &Config, limit: usize) -> anyhow::Result<()> {
    // A bounded run paces its walks too, at the seed checker's own rate (D109).
    let pacer = crate::dht::shared_pacer(cfg, false, true);
    let (_stop_tx, stop_rx) = watch::channel(false);
    if let Some(p) = pacer.clone() {
        tokio::spawn(async move { p.run(stop_rx).await });
    }
    let svc = LivenessService::new(liveness_config(cfg, pacer));
    let batch = cfg.liveness_batch_size();
    let mut done = 0;
    while done < limit {
        let started = Instant::now();
        let n = svc
            .run_batch(store.pool(), magnetar_core::now(), batch.min(limit - done))
            .await?;
        if n == 0 {
            break;
        }
        done += n;
        let report = svc.last.lock().unwrap().clone().map(|(_, r)| r);
        let (trackers, backed_off, dht_nodes) = svc.scraper.state();
        println!(
            "{}",
            serde_json::json!({
                "report": report,
                "total_ms": started.elapsed().as_millis() as u64,
                "trackers": trackers,
                "trackers_backed_off": backed_off,
                "dht_nodes": dht_nodes,
            })
        );
    }
    Ok(())
}

/// `serve` daemon: batches back to back while a full batch is due, else every `interval`;
/// history pruning and saved-search boosts hourly.
pub async fn liveness_loop(
    store: Arc<PgStore>,
    svc: Arc<LivenessService>,
    batch: usize,
    interval: Duration,
    mut shutdown: watch::Receiver<bool>,
) {
    let c = svc.config();
    tracing::info!(
        "liveness: batches of {batch}, {} req/s per tracker, {} trackers and {} DHT walks at once{}",
        c.tracker_rps,
        c.tracker_concurrency,
        c.dht_concurrency,
        if c.use_dht { "" } else { " (DHT off)" }
    );
    let mut last_maintenance: Option<Instant> = None;
    loop {
        let mut full = false;
        if enabled(&store).await {
            let now = magnetar_core::now();
            let work = async {
                if last_maintenance.is_none_or(|t| t.elapsed() >= MAINTENANCE_EVERY) {
                    last_maintenance = Some(Instant::now());
                    match prune_history(store.pool(), now).await {
                        Ok(n) if n > 0 => tracing::info!("liveness: pruned {n} history points"),
                        Ok(_) => {}
                        Err(e) => tracing::warn!("liveness: prune history: {e}"),
                    }
                    if let Err(e) = prune_leases(store.pool(), now).await {
                        tracing::warn!("liveness: prune leases: {e}");
                    }
                    boost_saved_searches(&store, &svc, now).await;
                }
                if let Err(e) = svc.flush_boosts(store.pool(), now).await {
                    tracing::warn!("liveness: boosts: {e}");
                }
                svc.run_batch(store.pool(), now, batch).await
            };
            tokio::select! {
                res = work => match res {
                    Ok(n) => full = n >= batch,
                    Err(e) => tracing::warn!("liveness: {e}"),
                },
                _ = stop_requested(&mut shutdown) => return,
            }
        }
        let pause = if full { BACKLOG_PAUSE } else { interval };
        tokio::select! {
            _ = tokio::time::sleep(pause) => {}
            _ = stop_requested(&mut shutdown) => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_changes() {
        assert!(!moved(Some(50), Some(54)), "wobble");
        assert!(moved(Some(50), Some(56)));
        assert!(!moved(Some(3), Some(4)));
        assert!(moved(Some(3), Some(5)));
        assert!(moved(Some(1), Some(0)), "alive again");
        assert!(moved(Some(0), Some(1)), "dead now");
        assert!(moved(Some(2), None) && moved(None, Some(2)));
        assert!(!moved(None, None));
    }
}

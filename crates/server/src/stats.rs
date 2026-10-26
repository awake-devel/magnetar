//! Charts for the app (ARCHITECTURE.md D47, docs/STATS.md): breakdowns counted by the search
//! index's facets (cross-filtered like D44), activity series from `stats_series`, a health
//! summary. Answers are small and cached for a minute or two; the series are written by
//! triggers (new infohashes, new relays), the daemons (`bump`) and [`stats_loop`].

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use magnetar_core::api::{
    Breakdown, BreakdownRequest, BreakdownSet, CrawlerStatus, Graph, RelayHealth, StatsHealth,
    Timeseries, TimeseriesPoint, TimeseriesSet,
};
use magnetar_core::error::{Error, Result};
use sqlx::{FromRow, PgPool};
use tokio::sync::watch;

use crate::daemons::stop_requested;
use crate::liveness::LivenessService;
use crate::search_index::collect::FacetTally;
use crate::search_index::sorted_counts;
use crate::store::{PgStore, db};

/// Counters (events per hour).
pub const TORRENTS_ADDED: &str = "torrents_added";
pub const RELAYS_DISCOVERED: &str = "relays_discovered";
pub const CRAWL_EVENTS: &str = "crawl_events";
pub const LIVENESS_CHECKS: &str = "liveness_checks";
/// Per day only, recomputed from `liveness_schedule.born` (first publication of an infohash).
pub const TORRENTS_PUBLISHED: &str = "torrents_published";
/// Gauges.
pub const DB_SIZE: &str = "db_size";
/// Derived: the current infohash count minus what was added since each bucket.
pub const TORRENTS_TOTAL: &str = "torrents_total";

pub const METRICS: &[&str] = &[
    TORRENTS_ADDED,
    TORRENTS_TOTAL,
    TORRENTS_PUBLISHED,
    RELAYS_DISCOVERED,
    CRAWL_EVENTS,
    LIVENESS_CHECKS,
    DB_SIZE,
];

/// Breakdown field → search index facet (`schema::FACETS`).
pub const BREAKDOWN_FIELDS: &[(&str, &str)] = &[
    ("language", "language"),
    ("subtitles", "subtitles"),
    ("resolution", "resolution"),
    ("source", "source"),
    ("codec", "codec"),
    ("hdr", "hdr"),
    ("audio", "audio"),
    ("channels", "channels"),
    ("category", "category"),
    ("subcategory", "category_path"),
    ("genre", "genre"),
    ("year", "year"),
    ("media_kind", "media_kind"),
    ("edition", "edition"),
    ("format", "format"),
    ("group", "group"),
];
pub const DEFAULT_TOP: u32 = 10;
pub const MAX_TOP: u32 = 50;
const MAX_FIELDS: usize = 16;

pub const BREAKDOWN_TTL: Duration = Duration::from_secs(120);
pub const TIMESERIES_TTL: Duration = Duration::from_secs(60);
pub const HEALTH_TTL: Duration = Duration::from_secs(30);
pub const GRAPH_TTL: Duration = Duration::from_secs(300);
/// A swarm graph shows seed-check state, which a "check now" on the detail page changes.
pub const SWARM_TTL: Duration = Duration::from_secs(30);
/// Distinct answers kept per cache (filters make keys); the oldest go first.
const CACHE_ENTRIES: usize = 64;

const HOUR: i64 = 3600;
const DAY: i64 = 86400;
/// Default and widest windows of the series.
const HOUR_DEFAULT: i64 = 48 * HOUR;
const HOUR_MAX: i64 = 31 * DAY;
const DAY_DEFAULT: i64 = 90 * DAY;
const DAY_MAX: i64 = 3650 * DAY;
/// Hourly rows are kept this long (the day view sums them).
pub const KEEP_DAYS: i64 = 800;

/// Gauges are sampled this often; the daily `torrents_published` and pruning run hourly.
const SAMPLE_EVERY: Duration = Duration::from_secs(600);
const RECOMPUTE_EVERY: Duration = Duration::from_secs(3600);

/// A small TTL cache of shared answers. Two requests missing at once both compute (no
/// stampede guard: answers are cheap and identical).
pub struct Cache<V> {
    ttl: Duration,
    map: Mutex<HashMap<String, (Instant, Arc<V>)>>,
}

impl<V> Cache<V> {
    pub fn new(ttl: Duration) -> Self {
        Self {
            ttl,
            map: Mutex::default(),
        }
    }

    pub async fn get_or<F, Fut>(&self, key: String, compute: F) -> Result<Arc<V>>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<V>>,
    {
        if let Some((at, v)) = self.map.lock().unwrap().get(&key)
            && at.elapsed() < self.ttl
        {
            return Ok(v.clone());
        }
        let v = Arc::new(compute().await?);
        let mut map = self.map.lock().unwrap();
        if map.len() >= CACHE_ENTRIES && !map.contains_key(&key) {
            map.retain(|_, (at, _)| at.elapsed() < self.ttl);
            if map.len() >= CACHE_ENTRIES
                && let Some(oldest) = map
                    .iter()
                    .min_by_key(|(_, (at, _))| *at)
                    .map(|(k, _)| k.clone())
            {
                map.remove(&oldest);
            }
        }
        map.insert(key, (Instant::now(), v.clone()));
        Ok(v)
    }
}

/// The API's caches.
pub struct StatsService {
    pub breakdowns: Cache<BreakdownSet>,
    pub series: Cache<TimeseriesSet>,
    pub health: Cache<StatsHealth>,
    pub graphs: Cache<Option<Graph>>,
    pub swarms: Cache<Option<Graph>>,
}

impl Default for StatsService {
    fn default() -> Self {
        Self {
            breakdowns: Cache::new(BREAKDOWN_TTL),
            series: Cache::new(TIMESERIES_TTL),
            health: Cache::new(HEALTH_TTL),
            graphs: Cache::new(GRAPH_TTL),
            swarms: Cache::new(SWARM_TTL),
        }
    }
}

/// Add `n` to the current hour of counter `metric`. A failure is logged, never returned: a
/// chart is not worth failing a crawl or a seed check for.
pub async fn bump(pool: &PgPool, metric: &str, n: i64) {
    if n <= 0 {
        return;
    }
    if let Err(e) = sqlx::query("SELECT stats_bump($1, $2)")
        .bind(metric)
        .bind(n)
        .execute(pool)
        .await
    {
        tracing::warn!("stats: {metric}: {e}");
    }
}

/// Top `top` values of one tally, the rest summed.
pub fn to_breakdown(field: &str, t: FacetTally, top: usize) -> Breakdown {
    let mut counts = sorted_counts(t.values);
    let rest = counts.split_off(top.min(counts.len()));
    Breakdown {
        field: field.to_string(),
        total: t.counted,
        counts,
        other: rest.iter().map(|c| c.count).sum(),
        other_values: rest.len() as u64,
        missing: t.missing,
    }
}

/// Validated field list: known names, each once, at most [`MAX_FIELDS`].
pub fn breakdown_fields(fields: &[String]) -> Result<Vec<(String, &'static str)>> {
    let mut out: Vec<(String, &'static str)> = Vec::new();
    for f in fields {
        let name = f.trim().to_lowercase();
        let Some((_, facet)) = BREAKDOWN_FIELDS.iter().find(|(n, _)| *n == name) else {
            return Err(Error::Invalid(format!("unknown breakdown field {f:?}")));
        };
        if !out.iter().any(|(n, _)| *n == name) {
            out.push((name, facet));
        }
    }
    if out.is_empty() {
        return Err(Error::Invalid("field is required".into()));
    }
    if out.len() > MAX_FIELDS {
        return Err(Error::Invalid(format!("at most {MAX_FIELDS} fields")));
    }
    Ok(out)
}

/// `POST /api/v1/stats/breakdown`. Ok(None) while the search index is not ready.
pub async fn breakdown(
    store: &PgStore,
    svc: &StatsService,
    r: &BreakdownRequest,
    now: i64,
) -> Result<Option<Arc<BreakdownSet>>> {
    let fields = breakdown_fields(&r.fields)?;
    let top = match r.top {
        0 => DEFAULT_TOP,
        n => n.min(MAX_TOP),
    } as usize;
    // Paging and sorting do not change counts: left out of the key.
    let mut q = r.query.clone();
    q.limit = 0;
    q.offset = 0;
    q.sort = Default::default();
    q.descending = true;
    let key = serde_json::to_string(&(&q, &fields, top)).expect("json");
    if !store.index_ready() {
        return Ok(None);
    }
    let set = svc
        .breakdowns
        .get_or(key, || async {
            // The attribute filters are left out of their own counts by the collector (D44);
            // categories and years are plain range filters there, so those two fields are
            // counted in a pass of their own without their filter.
            let (main, own): (Vec<_>, Vec<_>) = fields
                .iter()
                .cloned()
                .partition(|(_, facet)| !matches!(*facet, "category" | "year"));
            let names: Vec<String> = main.iter().map(|(_, f)| f.to_string()).collect();
            // Only if the index went away since the check above.
            let gone = || Error::Other("search index not ready".into());
            let own_passes = own.iter().map(|(_, facet)| {
                let mut q2 = q.clone();
                if *facet == "category" {
                    q2.categories.clear();
                } else {
                    q2.min_year = None;
                    q2.max_year = None;
                }
                async move {
                    let (_, mut t) = store
                        .facet_tallies(&q2, &[facet.to_string()], now)
                        .await?
                        .ok_or_else(gone)?;
                    Ok::<_, Error>(t.pop().unwrap_or_default())
                }
            });
            // The passes run side by side on the blocking pool.
            let (first, own_tallies) = futures::future::try_join(
                async { store.facet_tallies(&q, &names, now).await?.ok_or_else(gone) },
                futures::future::try_join_all(own_passes),
            )
            .await?;
            let (total, tallies) = first;
            let mut out: Vec<(String, FacetTally)> =
                main.into_iter().map(|(n, _)| n).zip(tallies).collect();
            out.extend(own.into_iter().map(|(n, _)| n).zip(own_tallies));
            Ok(BreakdownSet {
                total,
                // In the order asked for.
                breakdowns: fields
                    .iter()
                    .filter_map(|(name, _)| {
                        let i = out.iter().position(|(n, _)| n == name)?;
                        let (n, t) = out.swap_remove(i);
                        Some(to_breakdown(&n, t, top))
                    })
                    .collect(),
                computed_at: now,
            })
        })
        .await?;
    Ok(Some(set))
}

fn bucket_secs(bucket: &str) -> Result<i64> {
    match bucket {
        "hour" => Ok(HOUR),
        "day" => Ok(DAY),
        other => Err(Error::Invalid(format!("bad bucket {other:?} (hour|day)"))),
    }
}

/// The window actually served: `since` defaulted, clamped and floored to the bucket; `until`
/// is the end of the current bucket.
pub fn window(bucket: i64, since: Option<i64>, now: i64) -> (i64, i64) {
    let (default, max) = if bucket == HOUR {
        (HOUR_DEFAULT, HOUR_MAX)
    } else {
        (DAY_DEFAULT, DAY_MAX)
    };
    let since = since.unwrap_or(now - default).max(now - max).max(0);
    (
        since.div_euclid(bucket) * bucket,
        now.div_euclid(bucket) * bucket + bucket,
    )
}

/// Every bucket start from `since` to `until` (exclusive), each with the summed value or 0.
fn fill(since: i64, until: i64, step: i64, rows: &[(i64, i64)]) -> Vec<TimeseriesPoint> {
    let by: HashMap<i64, i64> = rows.iter().copied().collect();
    (0..)
        .map(|i| since + i * step)
        .take_while(|at| *at < until)
        .map(|at| TimeseriesPoint {
            at,
            value: by.get(&at).copied().unwrap_or(0),
        })
        .collect()
}

/// Running total ending at `current`: each point is the count at the end of its bucket.
pub fn running_total(added: &[TimeseriesPoint], current: i64) -> Vec<TimeseriesPoint> {
    let mut left = current - added.iter().map(|p| p.value).sum::<i64>();
    added
        .iter()
        .map(|p| {
            left += p.value;
            TimeseriesPoint {
                at: p.at,
                value: left.max(0),
            }
        })
        .collect()
}

async fn counter_rows(
    pool: &PgPool,
    metric: &str,
    step: i64,
    since: i64,
) -> Result<Vec<(i64, i64)>> {
    sqlx::query_as(
        "SELECT (bucket / $2) * $2 AS at, sum(value)::bigint FROM stats_series
         WHERE metric = $1 AND bucket >= $3 GROUP BY 1 ORDER BY 1",
    )
    .bind(metric)
    .bind(step)
    .bind(since)
    .fetch_all(pool)
    .await
    .map_err(db)
}

/// Last sample of each bucket.
async fn gauge_rows(pool: &PgPool, metric: &str, step: i64, since: i64) -> Result<Vec<(i64, i64)>> {
    sqlx::query_as(
        "SELECT DISTINCT ON (bucket / $2) (bucket / $2) * $2 AS at, value FROM stats_series
         WHERE metric = $1 AND bucket >= $3 ORDER BY bucket / $2, bucket DESC",
    )
    .bind(metric)
    .bind(step)
    .bind(since)
    .fetch_all(pool)
    .await
    .map_err(db)
}

/// `GET /api/v1/stats/timeseries`.
pub async fn timeseries(
    pool: &PgPool,
    svc: &StatsService,
    metrics: &[String],
    bucket: &str,
    since: Option<i64>,
    now: i64,
) -> Result<Arc<TimeseriesSet>> {
    let step = bucket_secs(bucket)?;
    let mut names: Vec<&'static str> = Vec::new();
    for m in metrics {
        let Some(name) = METRICS.iter().find(|n| **n == m.trim()) else {
            return Err(Error::Invalid(format!("unknown metric {m:?}")));
        };
        if !names.contains(name) {
            names.push(name);
        }
    }
    if names.is_empty() {
        return Err(Error::Invalid("metric is required".into()));
    }
    let (since, until) = window(step, since, now);
    let key = format!("{}|{bucket}|{since}", names.join(","));
    svc.series
        .get_or(key, || async move {
            let mut series = Vec::with_capacity(names.len());
            for name in names {
                series.push(one_series(pool, name, step, since, until).await?);
            }
            Ok(TimeseriesSet {
                since,
                until,
                series,
            })
        })
        .await
}

async fn one_series(
    pool: &PgPool,
    name: &str,
    step: i64,
    since: i64,
    until: i64,
) -> Result<Timeseries> {
    let bucket = |s: i64| if s == HOUR { "hour" } else { "day" }.to_string();
    let counter = |points| Timeseries {
        metric: name.to_string(),
        kind: "counter".into(),
        bucket: bucket(step),
        points,
    };
    Ok(match name {
        TORRENTS_PUBLISHED => {
            // Kept per day: an hour view gets days.
            let since = since.div_euclid(DAY) * DAY;
            let rows = counter_rows(pool, name, DAY, since).await?;
            Timeseries {
                bucket: "day".into(),
                ..counter(fill(since, until, DAY, &rows))
            }
        }
        DB_SIZE => Timeseries {
            metric: name.to_string(),
            kind: "gauge".into(),
            bucket: bucket(step),
            points: gauge_rows(pool, name, step, since)
                .await?
                .into_iter()
                .map(|(at, value)| TimeseriesPoint { at, value })
                .collect(),
        },
        TORRENTS_TOTAL => {
            let rows = counter_rows(pool, TORRENTS_ADDED, step, since).await?;
            let current: i64 = sqlx::query_scalar(
                "SELECT coalesce((SELECT value FROM meta WHERE key = 'infohash_count'), 0)",
            )
            .fetch_one(pool)
            .await
            .map_err(db)?;
            Timeseries {
                kind: "gauge".into(),
                ..counter(running_total(&fill(since, until, step, &rows), current))
            }
        }
        _ => counter(fill(
            since,
            until,
            step,
            &counter_rows(pool, name, step, since).await?,
        )),
    })
}

#[derive(FromRow)]
struct RelayRow {
    total: i64,
    enabled: i64,
    ok_24h: i64,
    failing: i64,
    productive: i64,
}

#[derive(FromRow)]
struct CountsRow {
    torrents: i64,
    attrs: i64,
    media_linked: i64,
}

/// `GET /api/v1/stats/health`.
pub async fn health(
    store: &PgStore,
    svc: &StatsService,
    liveness: &LivenessService,
    crawler: &Mutex<CrawlerStatus>,
    now: i64,
) -> Result<Arc<StatsHealth>> {
    svc.health
        .get_or(String::new(), || async {
            let pool = store.pool();
            let r: RelayRow = sqlx::query_as(
                "SELECT count(*) AS total, count(*) FILTER (WHERE enabled) AS enabled,
                        count(*) FILTER (WHERE last_ok > $1 - 86400) AS ok_24h,
                        count(*) FILTER (WHERE enabled AND consecutive_failures > 0
                                         AND next_try_at > $1) AS failing,
                        count(*) FILTER (WHERE events_found > 0) AS productive
                 FROM relays",
            )
            .bind(now)
            .fetch_one(pool)
            .await
            .map_err(db)?;
            // Index-only counts of two narrow tables (the infohash count is trigger-kept).
            let c: CountsRow = sqlx::query_as(
                "SELECT coalesce((SELECT value FROM meta WHERE key = 'infohash_count'), 0) AS torrents,
                        (SELECT count(*) FROM torrent_attrs) AS attrs,
                        (SELECT count(*) FROM torrent_media) AS media_linked",
            )
            .fetch_one(pool)
            .await
            .map_err(db)?;
            let u = |v: i64| u64::try_from(v).unwrap_or(0);
            let live = match liveness.stats(store, now).await {
                Ok(l) => Some(l),
                Err(e) => {
                    tracing::warn!("stats health: liveness: {e}");
                    None
                }
            };
            Ok(StatsHealth {
                torrents: u(c.torrents),
                liveness: live,
                search_index: store.search_index_status().await,
                relays: RelayHealth {
                    total: u(r.total),
                    enabled: u(r.enabled),
                    ok_24h: u(r.ok_24h),
                    failing: u(r.failing),
                    productive: u(r.productive),
                },
                crawler: crawler.lock().unwrap().clone(),
                media_linked: u(c.media_linked),
                attrs_missing: u(c.torrents - c.attrs),
                computed_at: now,
            })
        })
        .await
}

/// Torrents added before the counters existed (D47 migration), from `events.received_at` of
/// each infohash's first event. Runs once (marked `stats_backfilled` in `meta`).
pub async fn backfill(pool: &PgPool) -> Result<Option<u64>> {
    let mut tx = pool.begin().await.map_err(db)?;
    // Locks the marker row, so two servers on one database cannot both count.
    let since: Option<i64> =
        sqlx::query_scalar("SELECT value FROM meta WHERE key = 'stats_since' FOR UPDATE")
            .fetch_optional(&mut *tx)
            .await
            .map_err(db)?;
    let done: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM meta WHERE key = 'stats_backfilled')")
            .fetch_one(&mut *tx)
            .await
            .map_err(db)?;
    let Some(since) = since.filter(|_| !done) else {
        return Ok(None);
    };
    let n = sqlx::query(
        "INSERT INTO stats_series AS s (metric, bucket, value)
         SELECT 'torrents_added', (f.at / 3600) * 3600, count(*)
         FROM (SELECT min(e.received_at) AS at FROM torrents t JOIN events e ON e.id = t.event_id
               GROUP BY t.infohash) f
         WHERE f.at < $1 GROUP BY 2
         ON CONFLICT (metric, bucket) DO UPDATE SET value = s.value + EXCLUDED.value",
    )
    .bind(since)
    .execute(&mut *tx)
    .await
    .map_err(db)?
    .rows_affected();
    sqlx::query("INSERT INTO meta (key, value) VALUES ('stats_backfilled', $1)")
        .bind(magnetar_core::now())
        .execute(&mut *tx)
        .await
        .map_err(db)?;
    tx.commit().await.map_err(db)?;
    Ok(Some(n))
}

/// Replace the daily `torrents_published` counts (an old event arriving late moves a day).
pub async fn recompute_published(pool: &PgPool) -> Result<()> {
    let mut tx = pool.begin().await.map_err(db)?;
    sqlx::query("DELETE FROM stats_series WHERE metric = $1")
        .bind(TORRENTS_PUBLISHED)
        .execute(&mut *tx)
        .await
        .map_err(db)?;
    sqlx::query(
        "INSERT INTO stats_series (metric, bucket, value)
         SELECT $1, (born / 86400) * 86400, count(*) FROM liveness_schedule GROUP BY 2",
    )
    .bind(TORRENTS_PUBLISHED)
    .execute(&mut *tx)
    .await
    .map_err(db)?;
    tx.commit().await.map_err(db)
}

/// Gauges for the current hour (replacing an earlier sample of the same hour).
pub async fn sample_gauges(pool: &PgPool, now: i64) -> Result<()> {
    sqlx::query(
        "INSERT INTO stats_series (metric, bucket, value)
         VALUES ($1, $2, pg_database_size(current_database()))
         ON CONFLICT (metric, bucket) DO UPDATE SET value = EXCLUDED.value",
    )
    .bind(DB_SIZE)
    .bind(now.div_euclid(HOUR) * HOUR)
    .execute(pool)
    .await
    .map_err(db)?;
    Ok(())
}

pub async fn prune(pool: &PgPool, now: i64) -> Result<u64> {
    Ok(
        sqlx::query("DELETE FROM stats_series WHERE bucket < $1 AND metric <> $2")
            .bind(now - KEEP_DAYS * DAY)
            .bind(TORRENTS_PUBLISHED)
            .execute(pool)
            .await
            .map_err(db)?
            .rows_affected(),
    )
}

/// `serve` daemon: the one-off backfill, then gauges every 10 min, the daily publication
/// counts and pruning hourly.
pub async fn stats_loop(pool: PgPool, mut shutdown: watch::Receiver<bool>) {
    let work = async {
        match backfill(&pool).await {
            Ok(Some(n)) => tracing::info!("stats: backfilled torrents_added ({n} hours)"),
            Ok(None) => {}
            Err(e) => tracing::warn!("stats: backfill: {e}"),
        }
    };
    tokio::select! {
        _ = work => {}
        _ = stop_requested(&mut shutdown) => return,
    }
    let mut last_recompute: Option<Instant> = None;
    loop {
        let now = magnetar_core::now();
        let work = async {
            if let Err(e) = sample_gauges(&pool, now).await {
                tracing::warn!("stats: gauges: {e}");
            }
            if last_recompute.is_none_or(|t| t.elapsed() >= RECOMPUTE_EVERY) {
                last_recompute = Some(Instant::now());
                if let Err(e) = recompute_published(&pool).await {
                    tracing::warn!("stats: torrents_published: {e}");
                }
                if let Err(e) = prune(&pool, now).await {
                    tracing::warn!("stats: prune: {e}");
                }
            }
        };
        tokio::select! {
            _ = work => {}
            _ = stop_requested(&mut shutdown) => return,
        }
        tokio::select! {
            _ = tokio::time::sleep(SAMPLE_EVERY) => {}
            _ = stop_requested(&mut shutdown) => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn top_and_rest() {
        let t = FacetTally {
            counted: 10,
            missing: 2,
            values: [("fr", 5), ("en", 4), ("de", 1), ("ja", 1), ("it", 1)]
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect(),
        };
        let b = to_breakdown("language", t, 2);
        let got: Vec<(&str, u64)> = b
            .counts
            .iter()
            .map(|c| (c.value.as_str(), c.count))
            .collect();
        assert_eq!(got, vec![("fr", 5), ("en", 4)]);
        assert_eq!((b.other, b.other_values, b.missing, b.total), (3, 3, 2, 10));
        let few = to_breakdown("x", FacetTally::default(), 10);
        assert!(few.counts.is_empty() && few.other == 0);
    }

    #[test]
    fn fields_are_checked() {
        let f = breakdown_fields(&["Language".into(), "subcategory".into(), "language".into()])
            .unwrap();
        assert_eq!(
            f,
            vec![
                ("language".to_string(), "language"),
                ("subcategory".to_string(), "category_path")
            ]
        );
        assert!(breakdown_fields(&["nope".into()]).is_err());
        assert!(breakdown_fields(&[]).is_err());
    }

    #[test]
    fn windows_and_totals() {
        let now = 100 * DAY + 5 * HOUR + 17;
        assert_eq!(
            window(HOUR, None, now),
            (now / HOUR * HOUR - HOUR_DEFAULT, now / HOUR * HOUR + HOUR)
        );
        // Clamped to the widest window, never before 0.
        assert_eq!(window(DAY, Some(-5), now).0, 0);
        assert_eq!(window(HOUR, Some(0), now).0, (now - HOUR_MAX) / HOUR * HOUR);
        let pts = fill(0, 4 * HOUR, HOUR, &[(HOUR, 3), (3 * HOUR, 2)]);
        assert_eq!(
            pts.iter().map(|p| p.value).collect::<Vec<_>>(),
            vec![0, 3, 0, 2]
        );
        let total = running_total(&pts, 100);
        assert_eq!(
            total.iter().map(|p| p.value).collect::<Vec<_>>(),
            vec![95, 98, 98, 100]
        );
    }
}

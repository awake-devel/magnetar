//! Enrichment loop (D40): links infohashes to `media` rows.
//!
//! Per infohash (all its events, canonical first):
//! 1. Strong IDs (`core::enrich::ids`) from `i` tags and description links, on **every**
//!    category: method `itag` / `desc_link`, confidence 1.0 (0.9 for conflicting links).
//! 2. Otherwise, for `video` only: offline IMDb title match (`offline_title`), with a pg_trgm
//!    fallback capped below 0.6. Anime is left to the anime enricher (D41), which owns
//!    `offline_title` links on anime torrents.
//!
//! Ownership rules for `torrent_media` (shared with D41/D42): `manual` is never touched; on
//! non-anime torrents this loop replaces or deletes its own methods (`itag`, `desc_link`,
//! `offline_title`); on anime torrents it only writes a strong ID whose confidence is at least
//! the existing link's, and never deletes.
//!
//! `enrich_checked` remembers what was looked at with which `MATCHER_VERSION`; a new event for
//! an infohash clears its row (PgStore::put_torrent), and unmatched title lookups are retried
//! after each dataset import.

pub mod imdb;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Context;
use magnetar_core::enrich::ids::{self, MediaRef, StrongIds};
use magnetar_core::enrich::imdb_match::{
    self, Candidate, MATCHER_VERSION, fuzzy_confidence, normalize, query_keys, rank,
};
use magnetar_core::enrich::parse::{ParsedName, parse_torrent};
use magnetar_core::enrich::{MIN_DISPLAY_CONFIDENCE, method};
use magnetar_core::model::{Category, MediaInfo, MediaKind, TorrentFile};
use serde::Serialize;
use sqlx::types::Json;
use sqlx::{FromRow, PgConnection, PgPool, Postgres, QueryBuilder};
use tokio::sync::watch;

use crate::config::Config;
use crate::daemons::stop_requested;
use crate::store::PgStore;

/// Infohashes per batch (one read of their events, one write transaction).
pub const BATCH: usize = 200;
/// Pause when there is nothing to do.
pub const IDLE: Duration = Duration::from_secs(60);
/// Retry delay after a failed dataset refresh.
pub const REFRESH_RETRY: Duration = Duration::from_secs(3600);
/// pg_trgm similarity floor for the fuzzy fallback.
pub const FUZZY_MIN_SIMILARITY: f32 = 0.6;

/// What the loop needs from `Config`.
#[derive(Clone, Debug)]
pub struct EnrichConfig {
    /// None = MAGNETAR_IMDB=false (strong IDs only).
    pub imdb: Option<imdb::ImdbConfig>,
    pub refresh: Duration,
    /// pg_trgm fallback for unmatched video titles.
    pub fuzzy: bool,
}

impl EnrichConfig {
    pub fn from_config(cfg: &Config) -> Self {
        Self {
            imdb: cfg.imdb.then(|| imdb::ImdbConfig {
                dir: cfg.dataset_dir(),
                akas: cfg.imdb_akas,
                base_url: imdb::BASE_URL.into(),
                min_titles: imdb::MIN_TITLES,
            }),
            refresh: Duration::from_secs(cfg.imdb_refresh_hours.max(1) * 3600),
            fuzzy: true,
        }
    }
}

/// Counters of one or more batches.
#[derive(Clone, Debug, Default, Serialize)]
pub struct EnrichReport {
    pub checked: u64,
    pub strong: u64,
    pub title: u64,
    pub fuzzy: u64,
    pub unmatched: u64,
    /// Anime torrents without a strong ID (left to the anime enricher).
    pub anime_skipped: u64,
    pub ms: u64,
    /// Time spent in the prefix / pg_trgm fallbacks (part of `ms`).
    pub fallback_ms: u64,
}

impl EnrichReport {
    fn add(&mut self, o: &EnrichReport) {
        self.checked += o.checked;
        self.strong += o.strong;
        self.title += o.title;
        self.fuzzy += o.fuzzy;
        self.unmatched += o.unmatched;
        self.anime_skipped += o.anime_skipped;
        self.ms += o.ms;
        self.fallback_ms += o.fallback_ms;
    }
}

#[derive(FromRow)]
struct EventRow {
    infohash: String,
    title: String,
    description: String,
    files: Json<Vec<TorrentFile>>,
    external_ids: Json<Vec<(String, String)>>,
    category: String,
}

/// One infohash with its events, canonical first.
struct Group {
    infohash: String,
    category: Category,
    title: String,
    files: Vec<TorrentFile>,
    ids: StrongIds,
}

/// A decided link (or none).
#[derive(Clone, Debug)]
enum Decision {
    Strong(StrongIds, Option<ParsedName>),
    Title {
        imdb: u32,
        confidence: f32,
        parsed: ParsedName,
        fuzzy: bool,
    },
    /// Nothing found; `pending` = title matching applies and may succeed with a newer dataset.
    None {
        pending: bool,
    },
    AnimeSkipped,
}

async fn due(pool: &PgPool, retry_before: i64, limit: usize) -> anyhow::Result<Vec<String>> {
    Ok(sqlx::query_scalar(
        "SELECT DISTINCT t.infohash FROM torrents t
         WHERE NOT EXISTS (
             SELECT 1 FROM enrich_checked c WHERE c.infohash = t.infohash
               AND c.matcher_version >= $1 AND NOT (c.pending_title AND c.checked_at <= $2))
         LIMIT $3",
    )
    .bind(MATCHER_VERSION)
    .bind(retry_before)
    .bind(i64::try_from(limit).unwrap_or(i64::MAX))
    .fetch_all(pool)
    .await?)
}

async fn load_groups(pool: &PgPool, infohashes: &[String]) -> anyhow::Result<Vec<Group>> {
    let rows: Vec<EventRow> = sqlx::query_as(
        "SELECT infohash, title, description, files, external_ids, category FROM torrents
         WHERE infohash = ANY($1) ORDER BY infohash, trust DESC, created_at ASC, event_id ASC",
    )
    .bind(infohashes)
    .fetch_all(pool)
    .await?;
    let mut out: Vec<Group> = Vec::new();
    for r in rows {
        let ids = StrongIds::from_torrent(&r.external_ids.0, &r.description);
        match out.last_mut() {
            Some(g) if g.infohash == r.infohash => g.ids.merge(ids),
            _ => out.push(Group {
                category: r.category.parse().unwrap_or(Category::Other),
                infohash: r.infohash,
                title: r.title,
                files: r.files.0,
                ids,
            }),
        }
    }
    Ok(out)
}

/// The parsed names of a torrent, with a later name (largest video file) filling the first
/// one's missing year/season/episode when both name the same title.
pub fn parsed_names(title: &str, files: &[TorrentFile]) -> Vec<ParsedName> {
    let mut parsed = parse_torrent(title, files);
    if parsed.len() > 1 {
        let (head, tail) = parsed.split_at_mut(1);
        let p0 = &mut head[0];
        let n0 = normalize(&p0.title);
        for p in tail.iter() {
            if normalize(&p.title) == n0 {
                p0.year = p0.year.or(p.year);
                if p0.season.is_none() && p0.episode.is_none() {
                    p0.season = p.season;
                    p0.episode = p.episode;
                }
                p0.is_series |= p.is_series;
            }
        }
    }
    parsed
}

/// Exact title match over every parsed name; the most confident one wins (first on ties).
fn best_title(
    parsed: &[ParsedName],
    cands: &HashMap<String, Vec<Candidate>>,
) -> Option<(usize, imdb_match::Ranked)> {
    let mut best: Option<(usize, imdb_match::Ranked)> = None;
    for (i, p) in parsed.iter().enumerate() {
        let main: Vec<String> = imdb_match::name_keys(&p.title);
        let mut list: Vec<Candidate> = Vec::new();
        for k in query_keys(p) {
            for c in cands.get(&k).into_iter().flatten() {
                let mut c = c.clone();
                c.main_title = main.contains(&k);
                match list.iter_mut().find(|x| x.id == c.id) {
                    Some(x) => x.main_title |= c.main_title,
                    None => list.push(c),
                }
            }
        }
        if let Some(r) = rank(p, &list)
            && best
                .as_ref()
                .is_none_or(|(_, b)| r.confidence > b.confidence)
        {
            best = Some((i, r));
        }
    }
    best
}

/// An offline title match.
#[derive(Clone, Debug)]
pub struct TitleMatch {
    /// tconst number.
    pub imdb: u32,
    pub confidence: f32,
    pub parsed: ParsedName,
    pub fuzzy: bool,
}

/// Exact match over the parsed names (candidates already fetched for their `query_keys`), else
/// the pg_trgm fallback on the first name when it has a year.
pub async fn title_match(
    pool: &PgPool,
    ps: &[ParsedName],
    cands: &HashMap<String, Vec<Candidate>>,
    fuzzy: bool,
) -> anyhow::Result<Option<TitleMatch>> {
    if let Some((i, r)) = best_title(ps, cands) {
        return Ok(Some(TitleMatch {
            imdb: r.id,
            confidence: r.confidence,
            parsed: ps[i].clone(),
            fuzzy: false,
        }));
    }
    let Some(p) = ps.first().filter(|p| p.year.is_some()) else {
        return Ok(None);
    };
    // Subtitled names ("Wake Up Dead Man A Knives Out Mystery" for IMDb's "Wake Up Dead Man"):
    // the longest word prefix (≥ 2 words) with a same-type, same-year title.
    let key = normalize(&p.title);
    let words: Vec<&str> = key.split(' ').collect();
    if words.len() >= 3 {
        let prefixes: Vec<String> = (2..words.len())
            .rev()
            .map(|n| words[..n].join(" "))
            .collect();
        let pc = imdb::candidates(pool, &prefixes).await?;
        for k in &prefixes {
            let Some(list) = pc.get(k) else { continue };
            if let Some(conf) = imdb_match::prefix_confidence(p, list) {
                return Ok(Some(TitleMatch {
                    imdb: conf.id,
                    confidence: conf.confidence,
                    parsed: p.clone(),
                    fuzzy: true,
                }));
            }
        }
    }
    if !fuzzy || key.chars().count() < 4 {
        return Ok(None);
    }
    let mut found: Option<TitleMatch> = None;
    for (c, sim) in imdb::fuzzy_candidates(pool, &key, FUZZY_MIN_SIMILARITY).await? {
        if let Some(conf) = fuzzy_confidence(p, &c, sim)
            && found.as_ref().is_none_or(|f| conf > f.confidence)
        {
            found = Some(TitleMatch {
                imdb: c.id,
                confidence: conf,
                parsed: p.clone(),
                fuzzy: true,
            });
        }
    }
    Ok(found)
}

/// Title match for one torrent name (+ files), fetching its candidates (evaluation, tools).
pub async fn match_one(
    pool: &PgPool,
    title: &str,
    files: &[TorrentFile],
    fuzzy: bool,
) -> anyhow::Result<Option<TitleMatch>> {
    let ps = parsed_names(title, files);
    let mut keys: Vec<String> = Vec::new();
    for p in &ps {
        for k in query_keys(p) {
            if !keys.contains(&k) {
                keys.push(k);
            }
        }
    }
    let cands = imdb::candidates(pool, &keys).await?;
    title_match(pool, &ps, &cands, fuzzy).await
}

/// Decide links for a batch (reads only).
async fn decide(
    pool: &PgPool,
    groups: &[Group],
    imdb_ready: bool,
    fuzzy: bool,
) -> anyhow::Result<(Vec<Decision>, u64)> {
    let mut fallback_ms = 0u64;
    let parsed: Vec<Vec<ParsedName>> = groups
        .iter()
        .map(|g| parsed_names(&g.title, &g.files))
        .collect();
    let wants_title = |g: &Group| g.ids.is_empty() && g.category == Category::Video;
    let mut keys: Vec<String> = Vec::new();
    if imdb_ready {
        for (g, ps) in groups.iter().zip(&parsed) {
            if wants_title(g) {
                for p in ps {
                    for k in query_keys(p) {
                        if !keys.contains(&k) {
                            keys.push(k);
                        }
                    }
                }
            }
        }
    }
    let cands = imdb::candidates(pool, &keys).await?;
    let mut out = Vec::with_capacity(groups.len());
    for (g, ps) in groups.iter().zip(parsed) {
        if !g.ids.is_empty() {
            out.push(Decision::Strong(g.ids.clone(), ps.into_iter().next()));
            continue;
        }
        if g.category == Category::Anime {
            out.push(Decision::AnimeSkipped);
            continue;
        }
        if !wants_title(g) {
            out.push(Decision::None { pending: false });
            continue;
        }
        if !imdb_ready || ps.is_empty() {
            out.push(Decision::None {
                pending: !ps.is_empty(),
            });
            continue;
        }
        let t0 = Instant::now();
        let m = title_match(pool, &ps, &cands, fuzzy).await?;
        if m.as_ref().is_none_or(|m| m.fuzzy) {
            fallback_ms += t0.elapsed().as_millis() as u64;
        }
        out.push(match m {
            Some(m) => Decision::Title {
                imdb: m.imdb,
                confidence: m.confidence,
                parsed: m.parsed,
                fuzzy: m.fuzzy,
            },
            None => Decision::None { pending: true },
        });
    }
    Ok((out, fallback_ms))
}

/// Fields for a media row created from a torrent (no dataset row available).
struct Fallback<'a> {
    kind: MediaKind,
    title: &'a str,
    year: Option<i32>,
}

/// (find the row holding an id, set the id where unset) per external id column, in
/// `id_values` order. Static strings: the column names are not data.
const ID_SQL: [(&str, &str); 6] = [
    (
        "SELECT id, imdb IS NOT NULL FROM media WHERE tmdb_movie = $1",
        "UPDATE media SET tmdb_movie = $1 WHERE id = $2 AND tmdb_movie IS NULL",
    ),
    (
        "SELECT id, imdb IS NOT NULL FROM media WHERE tmdb_tv = $1",
        "UPDATE media SET tmdb_tv = $1 WHERE id = $2 AND tmdb_tv IS NULL",
    ),
    (
        "SELECT id, imdb IS NOT NULL FROM media WHERE tvdb = $1",
        "UPDATE media SET tvdb = $1 WHERE id = $2 AND tvdb IS NULL",
    ),
    (
        "SELECT id, imdb IS NOT NULL FROM media WHERE mal = $1",
        "UPDATE media SET mal = $1 WHERE id = $2 AND mal IS NULL",
    ),
    (
        "SELECT id, imdb IS NOT NULL FROM media WHERE anilist = $1",
        "UPDATE media SET anilist = $1 WHERE id = $2 AND anilist IS NULL",
    ),
    (
        "SELECT id, imdb IS NOT NULL FROM media WHERE anidb = $1",
        "UPDATE media SET anidb = $1 WHERE id = $2 AND anidb IS NULL",
    ),
];

fn id_values(ids: &StrongIds) -> [Option<u32>; 6] {
    [
        ids.tmdb_movie,
        ids.tmdb_tv,
        ids.tvdb,
        ids.mal,
        ids.anilist,
        ids.anidb,
    ]
}

fn to_i32(v: u32) -> i32 {
    i32::try_from(v).unwrap_or(i32::MAX)
}

/// Get or create the media row for an IMDb title. An episode id resolves to its series.
/// Returns (media id, season, episode).
async fn media_for_imdb(
    conn: &mut PgConnection,
    tt: u32,
    fb: &Fallback<'_>,
    now: i64,
) -> anyhow::Result<(i64, Option<i32>, Option<i32>)> {
    if let Some((row, s, e)) = imdb::resolve_title(conn, tt).await? {
        let kind = imdb_match::TitleType::from_code(row.title_type)
            .map(|t| t.media_kind())
            .unwrap_or(fb.kind);
        let id: i64 = sqlx::query_scalar(
            "INSERT INTO media (kind, imdb, title, original_title, year, end_year, genres, rating,
                               votes, runtime_min, updated_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
             ON CONFLICT (imdb) DO UPDATE SET imdb = excluded.imdb
             RETURNING id",
        )
        .bind(kind.as_str())
        .bind(ids::format_imdb(u64::from(
            u32::try_from(row.id).unwrap_or(0),
        )))
        .bind(&row.title)
        .bind(&row.original_title)
        .bind(row.year.map(i32::from))
        .bind(row.end_year.map(i32::from))
        .bind(&row.genres)
        .bind(row.rating)
        .bind(row.votes)
        .bind(row.runtime_min)
        .bind(now)
        .fetch_one(&mut *conn)
        .await?;
        return Ok((id, s, e));
    }
    // Not in the dataset (disabled, or newer than the last import): torrent-derived row,
    // refreshed by the next import.
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO media (kind, imdb, title, year, updated_at) VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT (imdb) DO UPDATE SET imdb = excluded.imdb RETURNING id",
    )
    .bind(fb.kind.as_str())
    .bind(ids::format_imdb(u64::from(tt)))
    .bind(fb.title)
    .bind(fb.year)
    .bind(now)
    .fetch_one(&mut *conn)
    .await?;
    Ok((id, None, None))
}

/// Attach the other external ids to `media_id`. An id already on a row without IMDb id is
/// merged into an IMDb-backed `media_id` (its links move over, the row is deleted); an id on a
/// row with another IMDb id is a conflict and is left alone.
async fn attach_ids(conn: &mut PgConnection, media_id: i64, ids: &StrongIds) -> anyhow::Result<()> {
    let has_imdb: bool = sqlx::query_scalar("SELECT imdb IS NOT NULL FROM media WHERE id = $1")
        .bind(media_id)
        .fetch_one(&mut *conn)
        .await?;
    for ((find, set), v) in ID_SQL.iter().zip(id_values(ids)) {
        let Some(v) = v.map(to_i32) else { continue };
        let other: Option<(i64, bool)> = sqlx::query_as(*find)
            .bind(v)
            .fetch_optional(&mut *conn)
            .await?;
        match other {
            None => {
                sqlx::query(*set)
                    .bind(v)
                    .bind(media_id)
                    .execute(&mut *conn)
                    .await?;
            }
            Some((o, _)) if o == media_id => {}
            Some((o, false)) if has_imdb => merge_into(conn, media_id, o).await?,
            Some(_) => {}
        }
    }
    Ok(())
}

/// tmdb_movie, tmdb_tv, tvdb, mal, anilist, anidb.
type IdRow = (
    Option<i32>,
    Option<i32>,
    Option<i32>,
    Option<i32>,
    Option<i32>,
    Option<i32>,
);

/// Move `drop`'s links and external ids onto `keep`, then delete `drop`.
async fn merge_into(conn: &mut PgConnection, keep: i64, drop: i64) -> anyhow::Result<()> {
    let row: IdRow = sqlx::query_as(
        "SELECT tmdb_movie, tmdb_tv, tvdb, mal, anilist, anidb FROM media WHERE id = $1",
    )
    .bind(drop)
    .fetch_one(&mut *conn)
    .await?;
    sqlx::query("UPDATE torrent_media SET media_id = $1 WHERE media_id = $2")
        .bind(keep)
        .bind(drop)
        .execute(&mut *conn)
        .await?;
    sqlx::query("DELETE FROM media WHERE id = $1")
        .bind(drop)
        .execute(&mut *conn)
        .await?;
    sqlx::query(
        "UPDATE media SET tmdb_movie = coalesce(tmdb_movie, $2), tmdb_tv = coalesce(tmdb_tv, $3),
                tvdb = coalesce(tvdb, $4), mal = coalesce(mal, $5), anilist = coalesce(anilist, $6),
                anidb = coalesce(anidb, $7)
         WHERE id = $1",
    )
    .bind(keep)
    .bind(row.0)
    .bind(row.1)
    .bind(row.2)
    .bind(row.3)
    .bind(row.4)
    .bind(row.5)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Get or create the media row for strong IDs without an IMDb id.
async fn media_for_ids(
    conn: &mut PgConnection,
    ids: &StrongIds,
    fb: &Fallback<'_>,
    now: i64,
) -> anyhow::Result<i64> {
    let vals = id_values(ids).map(|v| v.map(to_i32));
    let existing: Option<i64> = sqlx::query_scalar(
        "SELECT id FROM media WHERE tmdb_movie = $1 OR tmdb_tv = $2 OR tvdb = $3 OR mal = $4
            OR anilist = $5 OR anidb = $6
         ORDER BY (imdb IS NOT NULL) DESC, id LIMIT 1",
    )
    .bind(vals[0])
    .bind(vals[1])
    .bind(vals[2])
    .bind(vals[3])
    .bind(vals[4])
    .bind(vals[5])
    .fetch_optional(&mut *conn)
    .await?;
    if let Some(id) = existing {
        attach_ids(conn, id, ids).await?;
        return Ok(id);
    }
    Ok(sqlx::query_scalar(
        "INSERT INTO media (kind, tmdb_movie, tmdb_tv, tvdb, mal, anilist, anidb, title, year,
                            updated_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) RETURNING id",
    )
    .bind(fb.kind.as_str())
    .bind(vals[0])
    .bind(vals[1])
    .bind(vals[2])
    .bind(vals[3])
    .bind(vals[4])
    .bind(vals[5])
    .bind(fb.title)
    .bind(fb.year)
    .bind(now)
    .fetch_one(&mut *conn)
    .await?)
}

/// Kind for a row created from a torrent.
fn fallback_kind(category: Category, ids: &StrongIds, parsed: Option<&ParsedName>) -> MediaKind {
    if category == Category::Anime
        || ids.mal.is_some()
        || ids.anilist.is_some()
        || ids.anidb.is_some()
    {
        MediaKind::Anime
    } else if ids.tmdb_movie.is_some() {
        MediaKind::Movie
    } else if ids.tmdb_tv.is_some() || ids.tvdb.is_some() || parsed.is_some_and(|p| p.is_series) {
        MediaKind::Series
    } else {
        MediaKind::Movie
    }
}

/// Upsert one link under the ownership rules (module docs): never over `manual`; over another
/// link only if at least as confident, except our own methods on non-anime torrents.
#[allow(clippy::too_many_arguments)]
async fn write_link(
    conn: &mut PgConnection,
    infohash: &str,
    media_id: i64,
    season: Option<i32>,
    episode: Option<i32>,
    method: &str,
    confidence: f32,
    anime: bool,
    now: i64,
) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT INTO torrent_media (infohash, media_id, season, episode, method, confidence,
                                    matcher_version, matched_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
         ON CONFLICT (infohash) DO UPDATE SET media_id = excluded.media_id,
             season = excluded.season, episode = excluded.episode, method = excluded.method,
             confidence = excluded.confidence, matcher_version = excluded.matcher_version,
             matched_at = excluded.matched_at
         WHERE torrent_media.method <> 'manual'
           AND (excluded.confidence >= torrent_media.confidence
                OR (NOT $9 AND torrent_media.method IN ('itag', 'desc_link', 'offline_title')))
           AND (torrent_media.media_id, torrent_media.season, torrent_media.episode,
                torrent_media.method, torrent_media.confidence, torrent_media.matcher_version)
               IS DISTINCT FROM
               (excluded.media_id, excluded.season, excluded.episode, excluded.method,
                excluded.confidence, excluded.matcher_version)",
    )
    .bind(infohash)
    .bind(media_id)
    .bind(season)
    .bind(episode)
    .bind(method)
    .bind(confidence)
    .bind(MATCHER_VERSION)
    .bind(now)
    .bind(anime)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Apply decisions in one transaction.
async fn apply(
    pool: &PgPool,
    groups: &[Group],
    decisions: Vec<Decision>,
    now: i64,
) -> anyhow::Result<EnrichReport> {
    let mut rep = EnrichReport::default();
    let mut tx = pool.begin().await?;
    let mut imdb_cache: HashMap<u32, (i64, Option<i32>, Option<i32>)> = HashMap::new();
    for (g, d) in groups.iter().zip(decisions) {
        rep.checked += 1;
        let anime = g.category == Category::Anime;
        let mut pending = false;
        match d {
            Decision::Strong(ids, parsed) => {
                rep.strong += 1;
                let fb = Fallback {
                    kind: fallback_kind(g.category, &ids, parsed.as_ref()),
                    title: parsed
                        .as_ref()
                        .map(|p| p.title.as_str())
                        .filter(|t| !t.is_empty())
                        .unwrap_or(g.title.as_str()),
                    year: parsed.as_ref().and_then(|p| p.year),
                };
                let (media_id, s, e) = match ids.imdb.as_deref().and_then(ids::imdb_num) {
                    Some(tt) => {
                        let r = match imdb_cache.get(&tt) {
                            Some(r) => *r,
                            None => {
                                let r = media_for_imdb(&mut tx, tt, &fb, now).await?;
                                imdb_cache.insert(tt, r);
                                r
                            }
                        };
                        attach_ids(&mut tx, r.0, &ids).await?;
                        r
                    }
                    None => (media_for_ids(&mut tx, &ids, &fb, now).await?, None, None),
                };
                // Episode from the linked IMDb episode, else a TMDB episode link, else the name.
                let (season, episode) = match (s, e, ids.season) {
                    (Some(_), _, _) | (_, Some(_), _) => (s, e),
                    (None, None, Some(_)) => (ids.season, ids.episode),
                    _ => (
                        parsed.as_ref().and_then(|p| p.season),
                        parsed.as_ref().and_then(|p| p.episode),
                    ),
                };
                write_link(
                    &mut tx,
                    &g.infohash,
                    media_id,
                    season,
                    episode,
                    ids.method,
                    ids.confidence(),
                    anime,
                    now,
                )
                .await?;
            }
            Decision::Title {
                imdb,
                confidence,
                parsed,
                fuzzy,
            } => {
                if fuzzy {
                    rep.fuzzy += 1;
                } else {
                    rep.title += 1;
                }
                let fb = Fallback {
                    kind: if parsed.is_series {
                        MediaKind::Series
                    } else {
                        MediaKind::Movie
                    },
                    title: &parsed.title,
                    year: parsed.year,
                };
                let (media_id, _, _) = match imdb_cache.get(&imdb) {
                    Some(r) => *r,
                    None => {
                        let r = media_for_imdb(&mut tx, imdb, &fb, now).await?;
                        imdb_cache.insert(imdb, r);
                        r
                    }
                };
                write_link(
                    &mut tx,
                    &g.infohash,
                    media_id,
                    parsed.season,
                    parsed.episode,
                    method::OFFLINE_TITLE,
                    confidence,
                    anime,
                    now,
                )
                .await?;
            }
            Decision::None { pending: p } => {
                rep.unmatched += 1;
                pending = p;
                // Our own stale links go (e.g. a newer matcher found nothing).
                sqlx::query(
                    "DELETE FROM torrent_media WHERE infohash = $1
                       AND method IN ('itag', 'desc_link', 'offline_title')",
                )
                .bind(&g.infohash)
                .execute(&mut *tx)
                .await?;
            }
            Decision::AnimeSkipped => rep.anime_skipped += 1,
        }
        sqlx::query(
            "INSERT INTO enrich_checked (infohash, matcher_version, checked_at, pending_title)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT (infohash) DO UPDATE SET matcher_version = excluded.matcher_version,
                 checked_at = excluded.checked_at, pending_title = excluded.pending_title",
        )
        .bind(&g.infohash)
        .bind(MATCHER_VERSION)
        .bind(now)
        .bind(pending)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(rep)
}

/// One batch of due infohashes. `checked == 0` means nothing is due.
pub async fn enrich_batch(
    store: &PgStore,
    cfg: &EnrichConfig,
    limit: usize,
) -> anyhow::Result<EnrichReport> {
    let started = Instant::now();
    let pool = store.pool();
    let imported = match cfg.imdb {
        Some(_) => imdb::imported_at(pool).await?,
        None => None,
    };
    // Same-second checks are retried too (`<=`): a retry is cheap, a missed one waits a week.
    let infohashes = due(pool, imported.unwrap_or(-1), limit).await?;
    if infohashes.is_empty() {
        return Ok(EnrichReport::default());
    }
    let groups = load_groups(pool, &infohashes).await?;
    let (decisions, fallback_ms) = decide(pool, &groups, imported.is_some(), cfg.fuzzy).await?;
    let now = magnetar_core::now();
    let mut rep = apply(pool, &groups, decisions, now).await?;
    rep.fallback_ms = fallback_ms;
    rep.ms = started.elapsed().as_millis() as u64;
    Ok(rep)
}

/// Run batches until nothing is due (the serve loop cancels it on shutdown; a cancelled batch
/// rolls back and is redone later).
pub async fn drain(store: &PgStore, cfg: &EnrichConfig) -> anyhow::Result<EnrichReport> {
    let mut total = EnrichReport::default();
    loop {
        let r = enrich_batch(store, cfg, BATCH).await?;
        if r.checked == 0 {
            break;
        }
        total.add(&r);
        if total.checked % (BATCH as u64 * 50) == 0 {
            tracing::info!("enrich: {} checked so far", total.checked);
        }
    }
    Ok(total)
}

/// `serve` daemon: new torrents within a minute, the backlog at startup, dataset refresh every
/// `MAGNETAR_IMDB_REFRESH_HOURS` (first check right after the first drain).
pub async fn enrich_loop(
    store: Arc<PgStore>,
    cfg: Arc<Config>,
    mut shutdown: watch::Receiver<bool>,
) {
    let ecfg = EnrichConfig::from_config(&cfg);
    match &ecfg.imdb {
        Some(i) => tracing::info!(
            "enrich: IMDb datasets in {} (akas {}), refresh every {} h",
            i.dir.display(),
            i.akas,
            ecfg.refresh.as_secs() / 3600
        ),
        None => tracing::info!("enrich: IMDb off (MAGNETAR_IMDB=false); strong IDs only"),
    }
    let mut next_refresh = Instant::now();
    loop {
        let run = drain(&store, &ecfg);
        tokio::select! {
            res = run => match res {
                Ok(r) if r.checked > 0 => tracing::info!(
                    checked = r.checked, strong = r.strong, title = r.title, fuzzy = r.fuzzy,
                    unmatched = r.unmatched, anime_skipped = r.anime_skipped,
                    "enrich: done in {} ms", r.ms
                ),
                Ok(_) => {}
                Err(e) => tracing::warn!("enrich: {e:#}"),
            },
            _ = stop_requested(&mut shutdown) => return,
        }
        if let Some(icfg) = &ecfg.imdb
            && Instant::now() >= next_refresh
        {
            let run = imdb::refresh(store.pool(), icfg, false);
            tokio::select! {
                res = run => match res {
                    Ok(_) => next_refresh = Instant::now() + ecfg.refresh,
                    Err(e) => {
                        tracing::warn!("imdb refresh: {e:#}");
                        next_refresh = Instant::now() + REFRESH_RETRY.min(ecfg.refresh);
                    }
                },
                _ = stop_requested(&mut shutdown) => return,
            }
            // Re-run at once: an import makes pending title lookups due.
            continue;
        }
        tokio::select! {
            _ = tokio::time::sleep(IDLE) => {},
            _ = stop_requested(&mut shutdown) => return,
        }
    }
}

/// `magnetar-server enrich`: what one run did.
#[derive(Clone, Debug, Serialize)]
pub struct RunReport {
    pub import: Option<imdb::ImportReport>,
    pub enrich: EnrichReport,
    pub seconds: f64,
    pub coverage: Vec<CoverageRow>,
}

/// Refresh the datasets (when enabled), then drain everything that is due.
pub async fn run_once(
    store: &PgStore,
    cfg: &Config,
    reimport: bool,
    rematch: bool,
) -> anyhow::Result<RunReport> {
    let started = Instant::now();
    let ecfg = EnrichConfig::from_config(cfg);
    let import = match &ecfg.imdb {
        Some(i) => imdb::refresh(store.pool(), i, reimport).await?,
        None => None,
    };
    if rematch {
        sqlx::query("DELETE FROM enrich_checked")
            .execute(store.pool())
            .await?;
    }
    let enrich = drain(store, &ecfg).await?;
    Ok(RunReport {
        import,
        enrich,
        seconds: started.elapsed().as_secs_f64(),
        coverage: coverage(store.pool()).await?,
    })
}

/// Linked share of torrents per category and method (for reports and docs).
#[derive(Clone, Debug, Serialize, FromRow)]
pub struct CoverageRow {
    pub category: String,
    pub method: Option<String>,
    pub infohashes: i64,
}

pub async fn coverage(pool: &PgPool) -> anyhow::Result<Vec<CoverageRow>> {
    sqlx::query_as(
        "WITH canon AS (SELECT DISTINCT ON (infohash) infohash, category FROM torrents
                        ORDER BY infohash, trust DESC, created_at ASC, event_id ASC)
         SELECT c.category, CASE WHEN tm.confidence < $1 THEN tm.method || '<' || $1::text
                                 ELSE tm.method END AS method, count(*) AS infohashes
         FROM canon c LEFT JOIN torrent_media tm ON tm.infohash = c.infohash
         GROUP BY 1, 2 ORDER BY 1, 3 DESC",
    )
    .bind(MIN_DISPLAY_CONFIDENCE)
    .fetch_all(pool)
    .await
    .context("coverage")
}

// ---------------------------------------------------------------- reads for the API

#[derive(FromRow)]
struct MediaRow {
    id: i64,
    kind: String,
    imdb: Option<String>,
    tmdb_movie: Option<i32>,
    tmdb_tv: Option<i32>,
    mal: Option<i32>,
    anilist: Option<i32>,
    title: String,
    original_title: Option<String>,
    year: Option<i32>,
    end_year: Option<i32>,
    genres: Vec<String>,
    rating: Option<f32>,
    votes: Option<i32>,
    runtime_min: Option<i32>,
    season: Option<i32>,
    episode: Option<i32>,
    method: String,
    confidence: f32,
}

fn u32_of(v: Option<i32>) -> Option<u32> {
    v.and_then(|v| u32::try_from(v).ok())
}

impl From<MediaRow> for MediaInfo {
    fn from(r: MediaRow) -> Self {
        let tmdb = match (r.tmdb_movie, r.tmdb_tv) {
            (Some(m), _) => u32::try_from(m).ok().map(|m| (MediaKind::Movie, m)),
            (None, Some(t)) => u32::try_from(t).ok().map(|t| (MediaKind::Series, t)),
            _ => None,
        };
        MediaInfo {
            media_id: r.id,
            kind: r.kind.parse().unwrap_or(MediaKind::Movie),
            title: r.title,
            original_title: r.original_title,
            year: r.year,
            end_year: r.end_year,
            genres: r.genres,
            rating: r.rating,
            votes: u32_of(r.votes),
            runtime_min: r.runtime_min,
            season: r.season,
            episode: r.episode,
            imdb: r.imdb,
            tmdb,
            mal: u32_of(r.mal),
            anilist: u32_of(r.anilist),
            method: r.method,
            confidence: r.confidence,
            // Filled by the TMDB decorator (D42) from media_extra.
            overview: None,
            poster: None,
        }
    }
}

/// The displayed media of an infohash (None below `MIN_DISPLAY_CONFIDENCE`).
pub async fn media_for_infohash(
    pool: &PgPool,
    infohash: &str,
) -> magnetar_core::Result<Option<MediaInfo>> {
    let row: Option<MediaRow> = sqlx::query_as(
        "SELECT m.id, m.kind, m.imdb, m.tmdb_movie, m.tmdb_tv, m.mal, m.anilist, m.title,
                m.original_title, m.year, m.end_year, m.genres, m.rating, m.votes, m.runtime_min,
                tm.season, tm.episode, tm.method, tm.confidence
         FROM torrent_media tm JOIN media m ON m.id = tm.media_id
         WHERE tm.infohash = $1 AND tm.confidence >= $2",
    )
    .bind(infohash)
    .bind(MIN_DISPLAY_CONFIDENCE)
    .fetch_optional(pool)
    .await
    .map_err(crate::store::db)?;
    Ok(row.map(Into::into))
}

/// `AND t.infohash IN (…)` for a media search token (D40).
pub(crate) fn push_media_filter(qb: &mut QueryBuilder<Postgres>, r: &MediaRef) {
    qb.push(
        " AND t.infohash IN (SELECT tm.infohash FROM torrent_media tm JOIN media m \
         ON m.id = tm.media_id WHERE tm.confidence >= ",
    )
    .push_bind(MIN_DISPLAY_CONFIDENCE)
    .push(" AND ");
    let int = |v: u32| i32::try_from(v).unwrap_or(-1);
    match r {
        MediaRef::Imdb(tt) => qb.push("m.imdb = ").push_bind(tt.clone()),
        MediaRef::TmdbMovie(n) => qb.push("m.tmdb_movie = ").push_bind(int(*n)),
        MediaRef::TmdbTv(n) => qb.push("m.tmdb_tv = ").push_bind(int(*n)),
        MediaRef::Tvdb(n) => qb.push("m.tvdb = ").push_bind(int(*n)),
        MediaRef::Mal(n) => qb.push("m.mal = ").push_bind(int(*n)),
        MediaRef::Anilist(n) => qb.push("m.anilist = ").push_bind(int(*n)),
        MediaRef::Anidb(n) => qb.push("m.anidb = ").push_bind(int(*n)),
        MediaRef::Media(id) => qb.push("m.id = ").push_bind(*id),
    };
    qb.push(")");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fallback_kinds() {
        let tv = StrongIds {
            tmdb_tv: Some(1),
            ..Default::default()
        };
        assert_eq!(fallback_kind(Category::Video, &tv, None), MediaKind::Series);
        assert_eq!(fallback_kind(Category::Anime, &tv, None), MediaKind::Anime);
        let mut ids = StrongIds {
            tmdb_movie: Some(2),
            ..Default::default()
        };
        assert_eq!(fallback_kind(Category::Video, &ids, None), MediaKind::Movie);
        ids.mal = Some(3);
        assert_eq!(fallback_kind(Category::Video, &ids, None), MediaKind::Anime);
    }
}

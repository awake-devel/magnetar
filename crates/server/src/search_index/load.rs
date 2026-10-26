//! Postgres side of the index: documents for a set of infohashes, the change log written by
//! the triggers of migration `20261028000000_search_index.sql`, media token resolution.

use std::collections::HashMap;

use magnetar_core::enrich::MIN_DISPLAY_CONFIDENCE;
use magnetar_core::enrich::ids::MediaRef;
use magnetar_core::error::Result;
use magnetar_core::model::Category;
use sqlx::types::Json;
use sqlx::{FromRow, PgPool};

use super::schema::{DESCRIPTION_CHARS, DocMedia, FILES_CHARS, IndexDoc, TRUSTED};
use crate::store::db;

#[derive(FromRow)]
struct EventRow {
    infohash: String,
    event_id: String,
    pubkey: String,
    created_at: i64,
    trust: i16,
    title: String,
    description: String,
    files_text: String,
    file_count: i32,
    total_size: Option<i64>,
    category: String,
    category_path: Vec<String>,
    pow_bits: i16,
    source_url: Option<String>,
    trusted: bool,
}

#[derive(FromRow)]
struct LiveRow {
    infohash: String,
    seeders: Option<i32>,
    leechers: Option<i32>,
    dht_peers_est: Option<i32>,
    expires_at: i64,
}

#[derive(FromRow)]
struct MediaRow {
    infohash: String,
    id: i64,
    kind: String,
    title: String,
    year: Option<i32>,
    rating: Option<f32>,
    genres: Vec<String>,
}

fn publisher_trust(trust: i16, trusted: bool) -> i64 {
    if trusted { TRUSTED } else { i64::from(trust) }
}

fn u32_of(v: Option<i32>) -> Option<u32> {
    v.and_then(|v| u32::try_from(v).ok())
}

/// One document per infohash that still has a non-blocked event; the others are absent (the
/// caller deletes them from the index).
pub async fn load_docs(pool: &PgPool, infohashes: &[String]) -> Result<Vec<IndexDoc>> {
    if infohashes.is_empty() {
        return Ok(Vec::new());
    }
    // Blocked publishers' events are left out entirely, as the Postgres search does (D43).
    let rows: Vec<EventRow> = sqlx::query_as(
        "SELECT t.infohash, t.event_id, t.pubkey, t.created_at, t.trust, t.title,
                left(t.description, $2) AS description, left(t.files_text, $3) AS files_text,
                t.file_count, t.total_size, t.category, t.category_path, t.pow_bits, t.source_url,
                coalesce(o.action = 'trust', false) AS trusted
         FROM torrents t LEFT JOIN local_overrides o ON o.pubkey = t.pubkey
         WHERE t.infohash = ANY($1) AND o.action IS DISTINCT FROM 'block'
         ORDER BY t.infohash, t.trust DESC, t.created_at ASC, t.event_id ASC",
    )
    .bind(infohashes)
    .bind(DESCRIPTION_CHARS)
    .bind(FILES_CHARS)
    .fetch_all(pool)
    .await
    .map_err(db)?;
    let live: Vec<LiveRow> = sqlx::query_as(
        "SELECT infohash, seeders, leechers, dht_peers_est, expires_at FROM liveness
         WHERE infohash = ANY($1)",
    )
    .bind(infohashes)
    .fetch_all(pool)
    .await
    .map_err(db)?;
    let media: Vec<MediaRow> = sqlx::query_as(
        "SELECT tm.infohash, m.id, m.kind, m.title, m.year, m.rating, m.genres
         FROM torrent_media tm JOIN media m ON m.id = tm.media_id
         WHERE tm.infohash = ANY($1) AND tm.confidence >= $2",
    )
    .bind(infohashes)
    .bind(MIN_DISPLAY_CONFIDENCE)
    .fetch_all(pool)
    .await
    .map_err(db)?;
    let mut live: HashMap<String, LiveRow> =
        live.into_iter().map(|l| (l.infohash.clone(), l)).collect();
    let mut media: HashMap<String, MediaRow> =
        media.into_iter().map(|m| (m.infohash.clone(), m)).collect();

    let mut out: Vec<IndexDoc> = Vec::new();
    for r in rows {
        if let Some(d) = out.last_mut()
            && d.infohash == r.infohash
        {
            // A later (lower-ranked) event of the same infohash. Rows come by trust, so a
            // publisher's first row holds its best trust.
            if !d.publishers.contains(&r.pubkey) {
                d.publishers.push(r.pubkey);
                d.publisher_trust.push(publisher_trust(r.trust, r.trusted));
            }
            if !d.titles.contains(&r.title) {
                d.titles.push(r.title);
            }
            continue;
        }
        let l = live.remove(&r.infohash);
        let best_peers = l.as_ref().and_then(|l| match (l.seeders, l.dht_peers_est) {
            (None, None) => None,
            (s, d) => u32_of(Some(s.unwrap_or(0).max(d.unwrap_or(0)))),
        });
        let m = media.remove(&r.infohash).and_then(|m| {
            Some(DocMedia {
                id: m.id,
                kind: m.kind.parse().ok()?,
                title: m.title,
                year: m.year,
                rating: m.rating,
                genres: m.genres,
            })
        });
        out.push(IndexDoc {
            event_id: r.event_id,
            publishers: vec![r.pubkey.clone()],
            publisher_trust: vec![publisher_trust(r.trust, r.trusted)],
            pubkey: r.pubkey,
            titles: vec![r.title],
            files_text: r.files_text,
            description: r.description,
            category: r.category.parse().unwrap_or(Category::Other),
            category_path: r.category_path,
            created_at: r.created_at,
            total_size: r.total_size.and_then(|s| u64::try_from(s).ok()),
            file_count: u32::try_from(r.file_count).unwrap_or(0),
            trust: r.trust.clamp(0, 100) as u8,
            pow_bits: r.pow_bits.clamp(0, 255) as u8,
            source_url: r.source_url,
            seeders: l.as_ref().and_then(|l| u32_of(l.seeders)),
            leechers: l.as_ref().and_then(|l| u32_of(l.leechers)),
            best_peers,
            expires_at: l.map(|l| l.expires_at),
            media: m,
            infohash: r.infohash,
        });
    }
    Ok(out)
}

/// Infohashes after `cursor` in byte order (the full build's keyset walk).
pub async fn infohashes_after(pool: &PgPool, cursor: &str, limit: i64) -> Result<Vec<String>> {
    sqlx::query_scalar(
        "SELECT infohash FROM torrent_infohashes WHERE infohash > $1 ORDER BY infohash LIMIT $2",
    )
    .bind(cursor)
    .bind(limit)
    .fetch_all(pool)
    .await
    .map_err(db)
}

/// Distinct infohashes (the `infohash_count` counter kept by the triggers).
pub async fn infohash_count(pool: &PgPool) -> Result<u64> {
    let n: Option<i64> = sqlx::query_scalar("SELECT value FROM meta WHERE key = 'infohash_count'")
        .fetch_optional(pool)
        .await
        .map_err(db)?;
    Ok(n.and_then(|n| u64::try_from(n).ok()).unwrap_or(0))
}

/// The oldest queued changes: (log ids, distinct infohashes).
pub async fn pending_changes(pool: &PgPool, limit: i64) -> Result<(Vec<i64>, Vec<String>)> {
    let rows: Vec<(i64, String)> =
        sqlx::query_as("SELECT id, infohash FROM search_changes ORDER BY id LIMIT $1")
            .bind(limit)
            .fetch_all(pool)
            .await
            .map_err(db)?;
    let mut ids = Vec::with_capacity(rows.len());
    let mut ihs: Vec<String> = Vec::with_capacity(rows.len());
    for (id, ih) in rows {
        ids.push(id);
        ihs.push(ih);
    }
    ihs.sort_unstable();
    ihs.dedup();
    Ok((ids, ihs))
}

/// Forget exactly the log rows that were indexed (a row committed late, with a lower id than
/// some of these, stays and is picked up next time).
pub async fn ack_changes(pool: &PgPool, ids: &[i64]) -> Result<()> {
    sqlx::query("DELETE FROM search_changes WHERE id = ANY($1)")
        .bind(ids)
        .execute(pool)
        .await
        .map_err(db)?;
    Ok(())
}

/// Queued changes and the age of the oldest one (seconds), for `/api/v1/stats`.
pub async fn backlog(pool: &PgPool, now: i64) -> Result<(u64, Option<i64>)> {
    let (n, oldest): (i64, Option<i64>) =
        sqlx::query_as("SELECT count(*), min(queued_at) FROM search_changes")
            .fetch_one(pool)
            .await
            .map_err(db)?;
    Ok((
        u64::try_from(n).unwrap_or(0),
        oldest.map(|t| (now - t).max(0)),
    ))
}

/// Drop the whole log (the index is disabled, so nothing will read it).
pub async fn clear_changes(pool: &PgPool) -> Result<u64> {
    let r = sqlx::query("DELETE FROM search_changes")
        .execute(pool)
        .await
        .map_err(db)?;
    Ok(r.rows_affected())
}

/// The id this database was given by the migration (an index from elsewhere is rebuilt).
pub async fn database_id(pool: &PgPool) -> Result<String> {
    let v: Option<String> =
        sqlx::query_scalar("SELECT value FROM settings WHERE key = 'search.index_id'")
            .fetch_optional(pool)
            .await
            .map_err(db)?;
    Ok(v.map(|v| serde_json::from_str::<String>(&v).unwrap_or(v))
        .unwrap_or_default())
}

/// The media row a search token names, if it exists and anything links to it (D40).
pub async fn resolve_media(pool: &PgPool, r: &MediaRef) -> Result<Option<i64>> {
    let int = |v: u32| i32::try_from(v).unwrap_or(-1);
    let q = match r {
        MediaRef::Media(id) => return Ok(Some(*id)),
        MediaRef::Imdb(tt) => {
            return sqlx::query_scalar("SELECT id FROM media WHERE imdb = $1")
                .bind(tt)
                .fetch_optional(pool)
                .await
                .map_err(db);
        }
        MediaRef::TmdbMovie(n) => ("tmdb_movie", int(*n)),
        MediaRef::TmdbTv(n) => ("tmdb_tv", int(*n)),
        MediaRef::Tvdb(n) => ("tvdb", int(*n)),
        MediaRef::Mal(n) => ("mal", int(*n)),
        MediaRef::Anilist(n) => ("anilist", int(*n)),
        MediaRef::Anidb(n) => ("anidb", int(*n)),
    };
    // Column names come from the match above, never from input.
    let sql = format!("SELECT id FROM media WHERE {} = $1", q.0);
    sqlx::query_scalar(sqlx::AssertSqlSafe(sql))
        .bind(q.1)
        .fetch_optional(pool)
        .await
        .map_err(db)
}

/// Paths looked at per hit for the "matched in file" line (packs list up to 5000 files).
pub const MATCHED_FILE_PATHS: i32 = 1000;

/// The first `MATCHED_FILE_PATHS` file paths of the given events.
pub async fn event_files(
    pool: &PgPool,
    event_ids: &[String],
) -> Result<HashMap<String, Vec<String>>> {
    if event_ids.is_empty() {
        return Ok(HashMap::new());
    }
    let rows: Vec<(String, Json<Vec<String>>)> = sqlx::query_as(
        "SELECT event_id, jsonb_path_query_array(files, ('$[0 to ' || $2 || '].path')::jsonpath)
         FROM torrents WHERE event_id = ANY($1)",
    )
    .bind(event_ids)
    .bind(MATCHED_FILE_PATHS - 1)
    .fetch_all(pool)
    .await
    .map_err(db)?;
    Ok(rows.into_iter().map(|(id, f)| (id, f.0)).collect())
}

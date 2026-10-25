//! Reading the shared enrichment tables (`media`, `torrent_media`, contract D40).
//!
//! enrich/imdb owns these tables and fills `TorrentDetail.media` in `PgStore::torrent_detail`
//! (`crate::enrich::media_for_infohash`). This module is a stand-in for that read on a branch
//! without it, with the same rule: a link below [`MIN_DISPLAY_CONFIDENCE`] is never shown, so
//! never decorated either. `fill_if_missing` is a no-op as soon as the store already set
//! `media`. When merging with enrich/imdb, delete this module and its call in `api::torrent`
//! (see docs/ENRICHMENT-TMDB.md, "Merging").

use magnetar_core::error::Result;
use magnetar_core::model::{MediaInfo, MediaKind, TorrentDetail};
use sqlx::{FromRow, PgPool};

use crate::store::db;

/// Links below this are kept for review but never displayed or decorated. Mirror of
/// `magnetar_core::enrich::MIN_DISPLAY_CONFIDENCE` (enrich/imdb); use that one after the merge.
pub const MIN_DISPLAY_CONFIDENCE: f32 = 0.5;

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

fn pos(v: Option<i32>) -> Option<u32> {
    v.and_then(|v| u32::try_from(v).ok()).filter(|v| *v > 0)
}

impl From<MediaRow> for MediaInfo {
    fn from(r: MediaRow) -> Self {
        let kind = r.kind.parse().unwrap_or(MediaKind::Movie);
        let tmdb = match (pos(r.tmdb_movie), pos(r.tmdb_tv)) {
            (Some(id), _) if kind == MediaKind::Movie => Some((MediaKind::Movie, id)),
            (_, Some(id)) => Some((MediaKind::Series, id)),
            (Some(id), None) => Some((MediaKind::Movie, id)),
            (None, None) => None,
        };
        MediaInfo {
            kind,
            title: r.title,
            original_title: r.original_title,
            year: r.year,
            end_year: r.end_year,
            genres: r.genres,
            rating: r.rating,
            votes: r.votes.and_then(|v| u32::try_from(v).ok()),
            runtime_min: r.runtime_min,
            season: r.season,
            episode: r.episode,
            imdb: r.imdb,
            tmdb,
            mal: pos(r.mal),
            anilist: pos(r.anilist),
            method: r.method,
            confidence: r.confidence,
            overview: None,
            poster: None,
            media_id: r.id,
            episode_title: None,
        }
    }
}

/// The displayed media of one infohash (None when unlinked or below [`MIN_DISPLAY_CONFIDENCE`]).
pub async fn media_for(pool: &PgPool, infohash: &str) -> Result<Option<MediaInfo>> {
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
    .map_err(db)?;
    Ok(row.map(Into::into))
}

/// Set `detail.media` from the contract tables when the store left it empty. Failures (for
/// example the tables not existing yet) are logged and leave the detail unchanged.
pub async fn fill_if_missing(pool: &PgPool, detail: &mut TorrentDetail) {
    if detail.media.is_some() {
        return;
    }
    match media_for(pool, &detail.summary.infohash).await {
        Ok(m) => {
            if let Some(m) = &m {
                detail.summary.media_title = Some(m.title.clone());
                detail.summary.media_year = m.year;
                detail.summary.media_kind = Some(m.kind);
            }
            detail.media = m;
        }
        Err(e) => tracing::debug!("media for {}: {e}", detail.summary.infohash),
    }
}

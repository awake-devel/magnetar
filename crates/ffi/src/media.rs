//! Phone-side cache of identified media (D42): the `MediaInfo` the server attached to a detail is
//! kept per infohash for offline viewing, and posters fetched from the server are kept on disk
//! (LRU, capped). The phone never contacts TMDB itself.

use magnetar_core::error::Result;
use magnetar_core::model::{MediaInfo, MediaKind, TorrentDetail};
use rusqlite::{Connection, OptionalExtension, params};

use crate::store::{DbExt, SqliteStore};

/// Poster files kept on the phone.
pub const POSTER_CACHE_BYTES: u64 = 30 << 20;
/// A cached poster is revalidated (ETag) after this.
pub const POSTER_REFRESH_SECS: i64 = 30 * 86400;
/// TMDB-derived content (overview, episode name, poster) is kept on the phone at most this
/// long after the server last sent or revalidated it. The server refreshes its own copy after
/// 90 days and purges it after 180 (TMDB API terms §1.C, D42), so what it serves is at most
/// ~90 days old: 90 more on the phone keeps the whole chain within six months. IMDb fields
/// (title, year, rating, ids) are not TMDB content and stay.
pub const TMDB_MAX_AGE_SECS: i64 = 90 * 86400;
/// Most "other releases" listed.
pub const MEDIA_RELEASES: u32 = 100;

/// A media filter typed into the search box (`imdb:tt0944947`, `tmdb:movie:27205`, `tmdb:tv:1399`,
/// `mal:5114`), same tokens as the server search (D40).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MediaToken {
    Imdb(String),
    Tmdb(MediaKind, u32),
    Mal(u32),
    /// `media:N`, phone-only: torrents cached as linked to media row N (offline "other releases").
    MediaId(i64),
}

/// Split the media tokens out of a free-text query.
pub fn split_media_tokens(text: &str) -> (String, Vec<MediaToken>) {
    let mut rest = Vec::new();
    let mut tokens = Vec::new();
    for w in text.split_whitespace() {
        let lw = w.to_ascii_lowercase();
        let tok = if let Some(id) = lw.strip_prefix("imdb:") {
            let digits = id.strip_prefix("tt").unwrap_or(id);
            (!digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
                .then(|| MediaToken::Imdb(format!("tt{digits}")))
        } else if let Some(r) = lw.strip_prefix("tmdb:") {
            let (k, id) = r.split_once(':').unwrap_or(("movie", r));
            let kind = match k {
                "movie" => Some(MediaKind::Movie),
                "tv" | "series" => Some(MediaKind::Series),
                _ => None,
            };
            kind.zip(id.parse().ok())
                .map(|(k, id)| MediaToken::Tmdb(k, id))
        } else if let Some(id) = lw.strip_prefix("mal:") {
            id.parse().ok().map(MediaToken::Mal)
        } else if let Some(id) = lw.strip_prefix("media:") {
            id.parse().ok().map(MediaToken::MediaId)
        } else {
            None
        };
        match tok {
            Some(t) => tokens.push(t),
            None => rest.push(w),
        }
    }
    (rest.join(" "), tokens)
}

/// SQL condition (on alias `c` = canonical torrent row) + its argument for one token.
pub fn token_condition(t: &MediaToken) -> (String, rusqlite::types::Value) {
    use rusqlite::types::Value;
    match t {
        MediaToken::Imdb(id) => (
            // Cached link, or the publisher's own i-tag.
            "(c.infohash IN (SELECT infohash FROM media_cache WHERE imdb = ?) \
             OR c.external_ids LIKE '%\"' || ? || '\"%')"
                .into(),
            Value::Text(id.clone()),
        ),
        MediaToken::Tmdb(k, id) => (
            format!(
                "c.infohash IN (SELECT infohash FROM media_cache WHERE \
                 json_extract(json, '$.tmdb[0]') = '{}' AND json_extract(json, '$.tmdb[1]') = ?)",
                k.as_str()
            ),
            Value::Integer(i64::from(*id)),
        ),
        MediaToken::MediaId(id) => (
            "c.infohash IN (SELECT infohash FROM media_cache WHERE media_id = ?)".into(),
            Value::Integer(*id),
        ),
        MediaToken::Mal(id) => (
            "c.infohash IN (SELECT infohash FROM media_cache WHERE json_extract(json, '$.mal') = ?)"
                .into(),
            Value::Integer(i64::from(*id)),
        ),
    }
}

/// Drop the TMDB-derived fields of a media.
pub fn strip_tmdb(m: &mut MediaInfo) {
    m.overview = None;
    m.episode_title = None;
    m.poster = None;
}

/// Cached media of one infohash; TMDB fields older than [`TMDB_MAX_AGE_SECS`] are left out.
pub fn cached_media(conn: &Connection, infohash: &str, now: i64) -> Result<Option<MediaInfo>> {
    let row: Option<(String, i64)> = conn
        .query_row(
            "SELECT json, fetched_at FROM media_cache WHERE infohash = ?1",
            [infohash],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .db()?;
    Ok(row.and_then(|(j, at)| {
        let mut m: MediaInfo = serde_json::from_str(&j).ok()?;
        if now - at >= TMDB_MAX_AGE_SECS {
            strip_tmdb(&mut m);
        }
        Some(m)
    }))
}

/// Media of a detail from the cache (offline path).
pub fn attach_cached(conn: &Connection, d: &mut TorrentDetail, now: i64) -> Result<()> {
    if d.media.is_none() {
        d.media = cached_media(conn, &d.summary.infohash, now)?;
    }
    if let Some(m) = &d.media {
        d.summary.media_title = Some(m.title.clone());
        d.summary.media_year = m.year;
        d.summary.media_kind = Some(m.kind);
    }
    Ok(())
}

/// Drop cached media of torrents no longer in the cache.
pub fn prune(conn: &Connection) -> Result<u64> {
    let n = conn
        .execute(
            "DELETE FROM media_cache WHERE infohash NOT IN (SELECT infohash FROM torrents)",
            [],
        )
        .db()?;
    Ok(n as u64)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CachedPoster {
    pub file: String,
    pub etag: Option<String>,
    pub fetched_at: i64,
}

impl SqliteStore {
    /// Keep the server's media for an infohash (None clears a link the server dropped).
    pub fn put_media(&self, infohash: &str, m: Option<&MediaInfo>, now: i64) -> Result<()> {
        let conn = self.conn();
        match m {
            Some(m) => {
                let json = serde_json::to_string(m).unwrap_or_else(|_| "null".into());
                conn.execute(
                    "INSERT INTO media_cache(infohash, media_id, imdb, title, year, kind, json, fetched_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                     ON CONFLICT(infohash) DO UPDATE SET media_id = excluded.media_id,
                        imdb = excluded.imdb, title = excluded.title, year = excluded.year,
                        kind = excluded.kind, json = excluded.json, fetched_at = excluded.fetched_at",
                    params![infohash, m.media_id, m.imdb, m.title, m.year, m.kind.as_str(), json, now],
                )
                .db()?;
            }
            None => {
                conn.execute("DELETE FROM media_cache WHERE infohash = ?1", [infohash])
                    .db()?;
            }
        }
        Ok(())
    }

    pub fn media_of(&self, infohash: &str, now: i64) -> Result<Option<MediaInfo>> {
        cached_media(&self.conn(), infohash, now)
    }

    /// A cached media row by id (any torrent linked to it), without the per-torrent
    /// season/episode: the offline title page (D44).
    pub fn media_by_id(&self, media_id: i64, now: i64) -> Result<Option<MediaInfo>> {
        let conn = self.conn();
        let ih: Option<String> = conn
            .query_row(
                "SELECT infohash FROM media_cache WHERE media_id = ?1 ORDER BY fetched_at DESC LIMIT 1",
                [media_id],
                |r| r.get(0),
            )
            .optional()
            .db()?;
        let Some(ih) = ih else { return Ok(None) };
        Ok(cached_media(&conn, &ih, now)?.map(|mut m| {
            m.season = None;
            m.episode = None;
            m.episode_title = None;
            m
        }))
    }

    /// Enforce [`TMDB_MAX_AGE_SECS`] at rest: blank the TMDB fields of stale `media_cache` rows
    /// and drop stale poster rows. Returns the poster files to delete (the caller owns them).
    pub fn expire_tmdb(&self, now: i64) -> Result<(u64, Vec<String>)> {
        let conn = self.conn();
        let cutoff = now - TMDB_MAX_AGE_SECS;
        let blanked = conn
            .execute(
                "UPDATE media_cache
                 SET json = json_remove(json, '$.overview', '$.episode_title', '$.poster')
                 WHERE fetched_at <= ?1
                   AND (json_extract(json, '$.overview') IS NOT NULL
                     OR json_extract(json, '$.episode_title') IS NOT NULL
                     OR json_extract(json, '$.poster') IS NOT NULL)",
                [cutoff],
            )
            .db()? as u64;
        let mut st = conn
            .prepare("DELETE FROM poster_cache WHERE fetched_at <= ?1 RETURNING file")
            .db()?;
        let files = st
            .query_map([cutoff], |r| r.get(0))
            .db()?
            .collect::<rusqlite::Result<Vec<String>>>()
            .db()?;
        Ok((blanked, files))
    }

    pub fn poster(&self, media_id: i64, now: i64) -> Result<Option<CachedPoster>> {
        let conn = self.conn();
        let p = conn
            .query_row(
                "SELECT file, etag, fetched_at FROM poster_cache WHERE media_id = ?1",
                [media_id],
                |r| {
                    Ok(CachedPoster {
                        file: r.get(0)?,
                        etag: r.get(1)?,
                        fetched_at: r.get(2)?,
                    })
                },
            )
            .optional()
            .db()?;
        if p.is_some() {
            conn.execute(
                "UPDATE poster_cache SET last_access = ?2 WHERE media_id = ?1",
                params![media_id, now],
            )
            .db()?;
        }
        Ok(p)
    }

    pub fn put_poster(
        &self,
        media_id: i64,
        file: &str,
        etag: Option<&str>,
        bytes: u64,
        now: i64,
    ) -> Result<()> {
        self.conn()
            .execute(
                "INSERT INTO poster_cache(media_id, file, etag, bytes, fetched_at, last_access)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?5)
                 ON CONFLICT(media_id) DO UPDATE SET file = excluded.file, etag = excluded.etag,
                    bytes = excluded.bytes, fetched_at = excluded.fetched_at,
                    last_access = excluded.last_access",
                params![media_id, file, etag, bytes as i64, now],
            )
            .db()?;
        Ok(())
    }

    /// Mark a cached poster as revalidated (304).
    pub fn touch_poster(&self, media_id: i64, now: i64) -> Result<()> {
        self.conn()
            .execute(
                "UPDATE poster_cache SET fetched_at = ?2, last_access = ?2 WHERE media_id = ?1",
                params![media_id, now],
            )
            .db()?;
        Ok(())
    }

    pub fn forget_poster(&self, media_id: i64) -> Result<()> {
        self.conn()
            .execute("DELETE FROM poster_cache WHERE media_id = ?1", [media_id])
            .db()?;
        Ok(())
    }

    /// Least recently used poster rows to drop so the total stays under `cap`; returns their
    /// files (the caller deletes them) after removing the rows.
    pub fn evict_posters(&self, cap: u64) -> Result<Vec<String>> {
        let conn = self.conn();
        let mut st = conn
            .prepare("SELECT media_id, file, bytes FROM poster_cache ORDER BY last_access DESC")
            .db()?;
        let rows: Vec<(i64, String, i64)> = st
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .db()?
            .collect::<rusqlite::Result<_>>()
            .db()?;
        drop(st);
        let mut total = 0u64;
        let mut out = Vec::new();
        for (id, file, bytes) in rows {
            total += bytes.max(0) as u64;
            if total > cap {
                conn.execute("DELETE FROM poster_cache WHERE media_id = ?1", [id])
                    .db()?;
                out.push(file);
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens() {
        let (rest, t) = split_media_tokens(
            "imdb:tt0944947 1080p IMDB:0133093 tmdb:tv:1399 mal:5114 tmdb:x:1 mal:abc",
        );
        assert_eq!(rest, "1080p tmdb:x:1 mal:abc");
        assert_eq!(
            t,
            vec![
                MediaToken::Imdb("tt0944947".into()),
                MediaToken::Imdb("tt0133093".into()),
                MediaToken::Tmdb(MediaKind::Series, 1399),
                MediaToken::Mal(5114),
            ]
        );
        assert_eq!(
            split_media_tokens("tmdb:27205").1,
            vec![MediaToken::Tmdb(MediaKind::Movie, 27205)]
        );
    }
}

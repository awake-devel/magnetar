//! Offline anime identification on the server (D41, docs/ENRICHMENT-ANIME.md).
//!
//! 1. **Datasets**: anime-offline-database (AOD, cedya77 weekly release, `.jsonl.zst`) and Fribb
//!    `anime-list-full.json` are fetched with conditional GET (ETag / Last-Modified), validated
//!    (size, entry count, header) and imported into the private `anime_*` tables in one
//!    transaction; a failed or suspicious download keeps the previous import. Checked weekly.
//! 2. **Matching**: torrents in the anime category (not live-action / music / books / AMV) are
//!    parsed with `parse-torrent-title` and `anitomy-ng` (both titles kept as candidates, plus
//!    alternative titles in brackets), then identified by [`AnimeIndex::identify`]: MAL / AniList
//!    / AniDB ids from `i` tags or description links first, else the offline title match with
//!    the Fribb season remap. DHT-style input (no description, maybe no usable name) falls back
//!    to the largest video file's name.
//! 3. **Results** go to the shared `media` (kind `anime`) and `torrent_media` tables. A `manual`
//!    row is never touched; another matcher's row is replaced only by a more confident match.
//!
//! Enriched data stays on this server: it is never published to Nostr (AOD is ODbL share-alike,
//! Fribb has no licence file).

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anitomy_ng::ElementKind;
use magnetar_core::enrich_anime::{
    self as anime, AnimeIndex, AnimeMatch, AnimeType, AodEntry, FribbEntry, FribbPair, FribbTmdb,
    Release,
};
use magnetar_core::error::{Error, Result};
use magnetar_core::model::TorrentFile;
use serde::Serialize;
use sha2::{Digest, Sha256};
use sqlx::types::Json;
use sqlx::{FromRow, PgConnection, PgPool, Postgres, QueryBuilder};
use tokio::sync::watch;

use crate::config::Config;
use crate::daemons::stop_requested;
use crate::store::db;

/// Latest weekly release of the maintained continuation (manami-project was archived
/// 2026-07-04). GitHub redirects `latest` to `download/<year>-<week>/…`, which is recorded as
/// the imported version; a `…/download/2026-43/…` URL pins one release.
pub const DEFAULT_AOD_URL: &str = "https://github.com/cedya77/anime-offline-database/releases/latest/download/anime-offline-database.jsonl.zst";
pub const DEFAULT_FRIBB_URL: &str =
    "https://raw.githubusercontent.com/Fribb/anime-lists/master/anime-list-full.json";

/// Datasets are re-checked (conditional GET) this often.
pub const REFRESH_SECS: i64 = 7 * 24 * 3600;
/// How often the loop looks at the dataset age and for new torrents to match.
pub const LOOP_EVERY: Duration = Duration::from_secs(10 * 60);
/// Pause before the first pass so startup (migrations, first crawl) is not slowed down.
pub const STARTUP_DELAY: Duration = Duration::from_secs(30);
/// Torrents per matching transaction.
pub const BATCH: i64 = 500;
/// Title matches below this are not written (measured precision < 75 %, see the doc).
pub const MIN_CONFIDENCE: f32 = 0.6;
/// Download cap (the compressed AOD is ~6 MB, Fribb ~8 MB, the plain AOD ~61 MB).
pub const MAX_DOWNLOAD: usize = 128 << 20;
/// The AOD release is compressed with `--long` (128 MiB window, above ruzstd's 100 MiB
/// default). The decoder buffer never exceeds the ~61 MB of decompressed JSONL.
pub const MAX_ZSTD_WINDOW: u64 = 256 << 20;
/// Sanity floors: a truncated or wrong file must not replace a good import.
pub const MIN_AOD_ENTRIES: usize = 10_000;
pub const MIN_FRIBB_ENTRIES: usize = 10_000;
/// `category_path` values inside the anime category that are not anime video.
pub const EXCLUDED_PATHS: &[&str] = &[
    "live-action",
    "literature",
    "audio",
    "lossless",
    "pictures",
    "amv",
    "manga",
    "music",
];
/// AOD tags kept as `media.genres`.
const GENRES: &[&str] = &[
    "action",
    "adventure",
    "comedy",
    "drama",
    "ecchi",
    "fantasy",
    "harem",
    "horror",
    "isekai",
    "josei",
    "mahou shoujo",
    "mecha",
    "music",
    "mystery",
    "psychological",
    "romance",
    "sci-fi",
    "science fiction",
    "seinen",
    "shoujo",
    "shounen",
    "slice of life",
    "sports",
    "supernatural",
    "thriller",
];

pub fn aod_source(cfg: &Config) -> String {
    cfg.anime_aod_url
        .clone()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_AOD_URL.into())
}

pub fn fribb_source(cfg: &Config) -> String {
    cfg.anime_fribb_url
        .clone()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_FRIBB_URL.into())
}

/// `MAGNETAR_ANIME_FRIBB_URL=none` runs on AOD alone (no season remap, no IMDb/TMDB/TVDB ids).
pub fn fribb_enabled(cfg: &Config) -> bool {
    !fribb_source(cfg).eq_ignore_ascii_case("none")
}

// ---------------------------------------------------------------- release names

fn push_title(titles: &mut Vec<String>, t: &str) {
    for piece in std::iter::once(t).chain(t.split(" | ")) {
        let piece = piece.trim();
        if !piece.is_empty() && !titles.iter().any(|x| x == piece) {
            titles.push(piece.to_string());
        }
    }
}

/// Parse a release name: `parse-torrent-title` first (season ranges, years), `anitomy-ng` as a
/// second title candidate and gap filler (it reads fansub names like `[Yameii] X - S01E02
/// [English Dub]` better), then the alternative titles found in brackets.
pub fn parse_release(name: &str) -> Release {
    let p = parse_torrent_title::parse(name);
    let els = anitomy_ng::parse(name, anitomy_ng::Options::default());
    let get = |k: ElementKind| {
        els.iter()
            .find(|e| e.kind == k)
            .map(|e| e.value.trim().to_string())
            .filter(|v| !v.is_empty())
    };
    let num = |k: ElementKind| get(k).and_then(|v| v.parse::<i32>().ok());
    let mut titles = Vec::new();
    push_title(&mut titles, p.title());
    if let Some(t) = get(ElementKind::Title) {
        push_title(&mut titles, &t);
    }
    for t in anime::alt_titles(name) {
        push_title(&mut titles, &t);
    }
    let int = |v: Option<i64>| v.and_then(|v| i32::try_from(v).ok());
    // `X Season 3 - 10` reads as seasons 3..=10 to parse-torrent-title; anitomy-ng sees
    // season 3, episode 10. A real range (`S01-S03`) gives anitomy no single season.
    let seasons = p.seasons().unwrap_or_default();
    let season = match (seasons.len(), num(ElementKind::Season)) {
        (n, Some(s)) if n > 1 && p.episode().is_none() && num(ElementKind::Episode).is_some() => {
            Some(s)
        }
        _ => int(seasons.iter().max().copied()),
    };
    Release {
        titles,
        season: season.or_else(|| num(ElementKind::Season)),
        episode: int(p.episode()).or_else(|| num(ElementKind::Episode)),
        year: int(p.year().and_then(|y| y.as_int())).or_else(|| num(ElementKind::Year)),
    }
}

/// Identify one torrent: strong ids, then the name, then (DHT input, odd names) the largest
/// video file's name.
pub fn identify(
    ix: &AnimeIndex,
    title: &str,
    description: &str,
    files: &[TorrentFile],
    external_ids: &[(String, String)],
) -> Option<AnimeMatch> {
    let ids = anime::extract_ids(description, external_ids);
    let name = anime::release_name(title, files);
    let release = parse_release(name);
    if let Some(m) = ix.identify(&ids, &release) {
        return Some(m);
    }
    let file = anime::release_name("", files);
    if !file.is_empty() && file != name {
        return ix.match_release(&parse_release(file));
    }
    None
}

// ---------------------------------------------------------------- datasets

#[derive(Clone, Debug, FromRow)]
pub struct DatasetRow {
    pub name: String,
    pub source: String,
    pub version: Option<String>,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub sha256: String,
    pub rows: i32,
    pub imported_at: i64,
    pub checked_at: i64,
}

pub async fn dataset_rows(pool: &PgPool) -> Result<Vec<DatasetRow>> {
    sqlx::query_as::<_, DatasetRow>(
        "SELECT name, source, version, etag, last_modified, sha256, rows, imported_at, checked_at
         FROM anime_datasets ORDER BY name",
    )
    .fetch_all(pool)
    .await
    .map_err(db)
}

enum Fetched {
    NotModified,
    Body {
        bytes: Vec<u8>,
        etag: Option<String>,
        last_modified: Option<String>,
        final_url: String,
    },
}

fn is_url(s: &str) -> bool {
    s.starts_with("https://") || s.starts_with("http://")
}

async fn fetch(client: &reqwest::Client, src: &str, prev: Option<&DatasetRow>) -> Result<Fetched> {
    if !is_url(src) {
        let path = src.strip_prefix("file://").unwrap_or(src);
        let bytes = tokio::fs::read(path)
            .await
            .map_err(|e| Error::Other(format!("read {path}: {e}")))?;
        if bytes.len() > MAX_DOWNLOAD {
            return Err(Error::Invalid(format!(
                "{path}: larger than {MAX_DOWNLOAD} bytes"
            )));
        }
        return Ok(Fetched::Body {
            bytes,
            etag: None,
            last_modified: None,
            final_url: src.to_string(),
        });
    }
    let mut req = client.get(src);
    if let Some(p) = prev.filter(|p| p.source == src) {
        if let Some(e) = &p.etag {
            req = req.header(reqwest::header::IF_NONE_MATCH, e);
        }
        if let Some(l) = &p.last_modified {
            req = req.header(reqwest::header::IF_MODIFIED_SINCE, l);
        }
    }
    let mut resp = req.send().await?;
    if resp.status() == reqwest::StatusCode::NOT_MODIFIED {
        return Ok(Fetched::NotModified);
    }
    if !resp.status().is_success() {
        return Err(Error::Server {
            status: resp.status().as_u16(),
            msg: format!("GET {src}"),
        });
    }
    let header = |r: &reqwest::Response, h| {
        r.headers()
            .get(h)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string)
    };
    let etag = header(&resp, reqwest::header::ETAG);
    let last_modified = header(&resp, reqwest::header::LAST_MODIFIED);
    let final_url = resp.url().to_string();
    if resp
        .content_length()
        .is_some_and(|n| n as usize > MAX_DOWNLOAD)
    {
        return Err(Error::Invalid(format!(
            "{src}: larger than {MAX_DOWNLOAD} bytes"
        )));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = resp.chunk().await? {
        if bytes.len() + chunk.len() > MAX_DOWNLOAD {
            return Err(Error::Invalid(format!(
                "{src}: larger than {MAX_DOWNLOAD} bytes"
            )));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(Fetched::Body {
        bytes,
        etag,
        last_modified,
        final_url,
    })
}

fn sha256_hex(b: &[u8]) -> String {
    hex::encode(Sha256::digest(b))
}

/// `2026-43` from `…/releases/download/2026-43/…`.
pub fn release_tag(url: &str) -> Option<String> {
    let rest = url.split_once("/releases/download/")?.1;
    let tag = rest.split('/').next()?;
    (!tag.is_empty()).then(|| tag.to_string())
}

/// Decode AOD bytes (zstd-compressed or plain JSONL), streaming line by line.
pub fn decode_aod(bytes: &[u8]) -> Result<(Option<anime::AodHeader>, Vec<AodEntry>, usize)> {
    const ZSTD_MAGIC: [u8; 4] = [0x28, 0xb5, 0x2f, 0xfd];
    let reader: Box<dyn Read + '_> = if bytes.starts_with(&ZSTD_MAGIC) {
        Box::new(
            ruzstd::decoding::StreamingDecoder::new_with_max_window_size(bytes, MAX_ZSTD_WINDOW)
                .map_err(|e| Error::Invalid(format!("aod zstd: {e}")))?,
        )
    } else {
        Box::new(bytes)
    };
    let mut header = None;
    let mut entries = Vec::new();
    let mut bad = 0;
    for (i, line) in BufReader::new(reader).lines().enumerate() {
        let line = line.map_err(|e| Error::Invalid(format!("aod line {i}: {e}")))?;
        if i == 0 && line.contains("\"$schema\"") {
            header = anime::parse_aod_header(&line);
            continue;
        }
        match anime::parse_aod_line(&line) {
            Ok(Some(e)) => entries.push(e),
            Ok(None) => {}
            Err(_) => bad += 1,
        }
    }
    Ok((header, entries, bad))
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct DatasetReport {
    pub name: String,
    /// "imported" | "unchanged" | "fresh" (not due) | "failed"
    pub outcome: String,
    pub version: Option<String>,
    pub rows: usize,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct RefreshReport {
    pub datasets: Vec<DatasetReport>,
    pub changed: bool,
}

async fn touch_dataset(
    pool: &PgPool,
    name: &str,
    etag: Option<&str>,
    last_modified: Option<&str>,
    now: i64,
) -> Result<()> {
    sqlx::query(
        "UPDATE anime_datasets SET checked_at = $2,
                etag = COALESCE($3, etag), last_modified = COALESCE($4, last_modified)
         WHERE name = $1",
    )
    .bind(name)
    .bind(now)
    .bind(etag)
    .bind(last_modified)
    .execute(pool)
    .await
    .map_err(db)?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn put_dataset_row(
    conn: &mut PgConnection,
    name: &str,
    source: &str,
    version: Option<&str>,
    etag: Option<&str>,
    last_modified: Option<&str>,
    sha: &str,
    rows: usize,
    now: i64,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO anime_datasets
             (name, source, version, etag, last_modified, sha256, rows, imported_at, checked_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $8)
         ON CONFLICT (name) DO UPDATE SET source = EXCLUDED.source, version = EXCLUDED.version,
             etag = EXCLUDED.etag, last_modified = EXCLUDED.last_modified,
             sha256 = EXCLUDED.sha256, rows = EXCLUDED.rows,
             imported_at = EXCLUDED.imported_at, checked_at = EXCLUDED.checked_at",
    )
    .bind(name)
    .bind(source)
    .bind(version)
    .bind(etag)
    .bind(last_modified)
    .bind(sha)
    .bind(rows as i32)
    .bind(now)
    .execute(&mut *conn)
    .await
    .map_err(db)?;
    Ok(())
}

fn opt_i32(v: Option<u32>) -> Option<i32> {
    v.and_then(|v| i32::try_from(v).ok())
}

/// Replace `anime_entries` with `entries` inside the caller's transaction.
async fn import_aod(conn: &mut PgConnection, entries: &[AodEntry]) -> Result<()> {
    sqlx::query("DELETE FROM anime_entries")
        .execute(&mut *conn)
        .await
        .map_err(db)?;
    for (n, chunk) in entries.chunks(1000).enumerate() {
        let mut qb: QueryBuilder<Postgres> = QueryBuilder::new(
            "INSERT INTO anime_entries (id, mal, anilist, anidb, kitsu, title, synonyms, kind,
                 episodes, season, year, status, score, duration_secs, tags, picture) ",
        );
        qb.push_values(chunk.iter().enumerate(), |mut b, (i, e)| {
            b.push_bind((n * 1000 + i) as i32)
                .push_bind(opt_i32(e.mal))
                .push_bind(opt_i32(e.anilist))
                .push_bind(opt_i32(e.anidb))
                .push_bind(opt_i32(e.kitsu))
                .push_bind(e.title.replace('\0', ""))
                .push_bind(
                    e.synonyms
                        .iter()
                        .map(|s| s.replace('\0', ""))
                        .collect::<Vec<_>>(),
                )
                .push_bind(e.kind.as_str())
                .push_bind(opt_i32(e.episodes))
                .push_bind(e.season.as_deref())
                .push_bind(e.year)
                .push_bind(e.status.as_deref())
                .push_bind(e.score)
                .push_bind(opt_i32(e.duration_secs))
                .push_bind(&e.tags)
                .push_bind(e.picture.as_deref());
        });
        qb.build().execute(&mut *conn).await.map_err(db)?;
    }
    Ok(())
}

/// Replace `anime_mappings` with `entries` inside the caller's transaction.
async fn import_fribb(conn: &mut PgConnection, entries: &[FribbEntry]) -> Result<()> {
    sqlx::query("DELETE FROM anime_mappings")
        .execute(&mut *conn)
        .await
        .map_err(db)?;
    for (n, chunk) in entries.chunks(2000).enumerate() {
        let mut qb: QueryBuilder<Postgres> = QueryBuilder::new(
            "INSERT INTO anime_mappings (id, kind, mal, anilist, anidb, imdb, tmdb_tv, tmdb_movie,
                 tvdb, season_tvdb, season_tmdb, offset_tvdb, offset_tmdb) ",
        );
        qb.push_values(chunk.iter().enumerate(), |mut b, (i, f)| {
            let movies: Vec<i32> = f
                .tmdb
                .iter()
                .flat_map(|t| t.movie.iter())
                .filter_map(|&m| i32::try_from(m).ok())
                .collect();
            b.push_bind((n * 2000 + i) as i32)
                .push_bind(f.kind.as_deref())
                .push_bind(opt_i32(f.mal))
                .push_bind(opt_i32(f.anilist))
                .push_bind(opt_i32(f.anidb))
                .push_bind(&f.imdb)
                .push_bind(opt_i32(f.tmdb_tv()))
                .push_bind(movies)
                .push_bind(opt_i32(f.tvdb))
                .push_bind(f.season.and_then(|s| s.tvdb))
                .push_bind(f.season.and_then(|s| s.tmdb))
                .push_bind(f.episode_offset.and_then(|s| s.tvdb))
                .push_bind(f.episode_offset.and_then(|s| s.tmdb));
        });
        qb.build().execute(&mut *conn).await.map_err(db)?;
    }
    Ok(())
}

/// Fetch, validate and import both datasets when due (or always with `force`). Each dataset is
/// independent: a failed Fribb download does not block an AOD update.
pub async fn refresh(pool: &PgPool, cfg: &Config, force: bool) -> Result<RefreshReport> {
    let now = magnetar_core::now();
    let prev = dataset_rows(pool).await?;
    let client = magnetar_core::http::client(Duration::from_secs(300))?;
    let mut report = RefreshReport::default();
    let mut sources = vec![("aod", aod_source(cfg))];
    if fribb_enabled(cfg) {
        sources.push(("fribb", fribb_source(cfg)));
    }
    for (name, src) in sources {
        let p = prev.iter().find(|r| r.name == name);
        let mut r = DatasetReport {
            name: name.into(),
            version: p.and_then(|p| p.version.clone()),
            rows: p.map_or(0, |p| p.rows as usize),
            ..Default::default()
        };
        if !force && p.is_some_and(|p| p.source == src && now - p.checked_at < REFRESH_SECS) {
            r.outcome = "fresh".into();
            report.datasets.push(r);
            continue;
        }
        match refresh_one(pool, &client, name, &src, p, now).await {
            Ok((outcome, version, rows)) => {
                if outcome == "imported" {
                    report.changed = true;
                    tracing::info!(
                        "anime: imported {name} {} ({rows} rows) from {src}",
                        version.as_deref().unwrap_or("-")
                    );
                }
                r.outcome = outcome.into();
                if version.is_some() {
                    r.version = version;
                }
                if rows > 0 {
                    r.rows = rows;
                }
            }
            Err(e) => {
                tracing::warn!("anime: {name} from {src}: {e}");
                r.outcome = "failed".into();
                r.error = Some(e.to_string());
            }
        }
        report.datasets.push(r);
    }
    Ok(report)
}

async fn refresh_one(
    pool: &PgPool,
    client: &reqwest::Client,
    name: &str,
    src: &str,
    prev: Option<&DatasetRow>,
    now: i64,
) -> Result<(&'static str, Option<String>, usize)> {
    let (bytes, etag, last_modified, final_url) = match fetch(client, src, prev).await? {
        Fetched::NotModified => {
            touch_dataset(pool, name, None, None, now).await?;
            return Ok(("unchanged", None, 0));
        }
        Fetched::Body {
            bytes,
            etag,
            last_modified,
            final_url,
        } => (bytes, etag, last_modified, final_url),
    };
    let sha = sha256_hex(&bytes);
    if prev.is_some_and(|p| p.sha256 == sha) {
        touch_dataset(pool, name, etag.as_deref(), last_modified.as_deref(), now).await?;
        return Ok(("unchanged", None, 0));
    }
    let mut tx = pool.begin().await.map_err(db)?;
    let (version, rows) = if name == "aod" {
        let (header, entries, bad) = tokio::task::spawn_blocking(move || decode_aod(&bytes))
            .await
            .map_err(|e| Error::Other(e.to_string()))??;
        let Some(header) = header else {
            return Err(Error::Invalid("aod: no metadata header line".into()));
        };
        if entries.len() < MIN_AOD_ENTRIES || bad > entries.len() / 100 {
            return Err(Error::Invalid(format!(
                "aod: {} entries, {bad} unreadable lines: refusing to replace the import",
                entries.len()
            )));
        }
        let version = release_tag(&final_url)
            .map(|t| format!("{t} ({})", header.last_update.as_deref().unwrap_or("?")))
            .or(header.last_update);
        import_aod(&mut tx, &entries).await?;
        (version, entries.len())
    } else {
        let entries = tokio::task::spawn_blocking(move || {
            let text = String::from_utf8_lossy(&bytes);
            anime::parse_fribb(&text)
        })
        .await
        .map_err(|e| Error::Other(e.to_string()))??;
        if entries.len() < MIN_FRIBB_ENTRIES {
            return Err(Error::Invalid(format!(
                "fribb: {} entries: refusing to replace the import",
                entries.len()
            )));
        }
        import_fribb(&mut tx, &entries).await?;
        (Some(sha[..12].to_string()), entries.len())
    };
    put_dataset_row(
        &mut tx,
        name,
        src,
        version.as_deref(),
        etag.as_deref(),
        last_modified.as_deref(),
        &sha,
        rows,
        now,
    )
    .await?;
    tx.commit().await.map_err(db)?;
    Ok(("imported", version, rows))
}

// ---------------------------------------------------------------- index

#[derive(FromRow)]
struct EntryRow {
    mal: Option<i32>,
    anilist: Option<i32>,
    anidb: Option<i32>,
    kitsu: Option<i32>,
    title: String,
    synonyms: Vec<String>,
    kind: String,
    episodes: Option<i32>,
    season: Option<String>,
    year: Option<i32>,
    status: Option<String>,
    score: Option<f32>,
    duration_secs: Option<i32>,
    tags: Vec<String>,
    picture: Option<String>,
}

#[derive(FromRow)]
struct MappingRow {
    kind: Option<String>,
    mal: Option<i32>,
    anilist: Option<i32>,
    anidb: Option<i32>,
    imdb: Vec<String>,
    tmdb_tv: Option<i32>,
    tmdb_movie: Vec<i32>,
    tvdb: Option<i32>,
    season_tvdb: Option<i32>,
    season_tmdb: Option<i32>,
    offset_tvdb: Option<i32>,
    offset_tmdb: Option<i32>,
}

fn opt_u32(v: Option<i32>) -> Option<u32> {
    v.and_then(|v| u32::try_from(v).ok())
}

fn pair(tvdb: Option<i32>, tmdb: Option<i32>) -> Option<FribbPair> {
    (tvdb.is_some() || tmdb.is_some()).then_some(FribbPair { tvdb, tmdb })
}

/// The matching index from the imported tables, with the dataset version it was built from.
/// None until the datasets are imported (AOD, and Fribb unless `with_fribb` is false).
pub async fn load_index(pool: &PgPool, with_fribb: bool) -> Result<Option<(AnimeIndex, String)>> {
    let rows = dataset_rows(pool).await?;
    let sha = |n: &str| {
        rows.iter()
            .find(|r| r.name == n)
            .map(|r| r.sha256[..12].to_string())
    };
    let Some(a) = sha("aod") else {
        return Ok(None);
    };
    let f = if with_fribb {
        let Some(f) = sha("fribb") else {
            return Ok(None);
        };
        f
    } else {
        "none".to_string()
    };
    let entries: Vec<AodEntry> = sqlx::query_as::<_, EntryRow>(
        "SELECT mal, anilist, anidb, kitsu, title, synonyms, kind, episodes, season, year, status,
                score, duration_secs, tags, picture
         FROM anime_entries ORDER BY id",
    )
    .fetch_all(pool)
    .await
    .map_err(db)?
    .into_iter()
    .map(|r| AodEntry {
        mal: opt_u32(r.mal),
        anilist: opt_u32(r.anilist),
        anidb: opt_u32(r.anidb),
        kitsu: opt_u32(r.kitsu),
        title: r.title,
        synonyms: r.synonyms,
        kind: AnimeType::parse(&r.kind),
        episodes: opt_u32(r.episodes),
        season: r.season,
        year: r.year,
        status: r.status,
        score: r.score,
        duration_secs: opt_u32(r.duration_secs),
        tags: r.tags,
        picture: r.picture,
    })
    .collect();
    let fribb: Vec<FribbEntry> = if !with_fribb {
        Vec::new()
    } else {
        sqlx::query_as::<_, MappingRow>(
            "SELECT kind, mal, anilist, anidb, imdb, tmdb_tv, tmdb_movie, tvdb, season_tvdb,
                season_tmdb, offset_tvdb, offset_tmdb
         FROM anime_mappings ORDER BY id",
        )
        .fetch_all(pool)
        .await
        .map_err(db)?
        .into_iter()
        .map(|r| FribbEntry {
            kind: r.kind,
            mal: opt_u32(r.mal),
            anilist: opt_u32(r.anilist),
            anidb: opt_u32(r.anidb),
            imdb: r.imdb,
            tmdb: (r.tmdb_tv.is_some() || !r.tmdb_movie.is_empty()).then(|| FribbTmdb {
                tv: opt_u32(r.tmdb_tv),
                movie: r
                    .tmdb_movie
                    .into_iter()
                    .filter_map(|m| u32::try_from(m).ok())
                    .collect(),
            }),
            tvdb: opt_u32(r.tvdb),
            season: pair(r.season_tvdb, r.season_tmdb),
            episode_offset: pair(r.offset_tvdb, r.offset_tmdb),
        })
        .collect()
    };
    if entries.is_empty() {
        return Ok(None);
    }
    let version = format!("aod:{a}+fribb:{f}");
    let ix = tokio::task::spawn_blocking(move || AnimeIndex::new(entries, fribb))
        .await
        .map_err(|e| Error::Other(e.to_string()))?;
    Ok(Some((ix, version)))
}

// ---------------------------------------------------------------- matching

/// True when the shared enrichment tables (IMDb migration, contract) exist.
pub async fn contract_tables_exist(pool: &PgPool) -> Result<bool> {
    sqlx::query_scalar::<_, bool>(
        "SELECT to_regclass('media') IS NOT NULL AND to_regclass('torrent_media') IS NOT NULL",
    )
    .fetch_one(pool)
    .await
    .map_err(db)
}

#[derive(FromRow)]
struct PendingRow {
    infohash: String,
    title: String,
    description: String,
    files: Json<Vec<TorrentFile>>,
    external_ids: Json<Vec<(String, String)>>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct MatchReport {
    pub scanned: u64,
    pub matched: u64,
    pub by_method: HashMap<String, u64>,
    pub low_confidence: u64,
    pub unmatched: u64,
    /// Rows kept because they are `manual` or a more confident match by another matcher.
    pub kept: u64,
    pub media_rows: u64,
    pub duration_ms: u64,
}

impl MatchReport {
    fn add(&mut self, o: &MatchReport) {
        self.scanned += o.scanned;
        self.matched += o.matched;
        self.low_confidence += o.low_confidence;
        self.unmatched += o.unmatched;
        self.kept += o.kept;
        self.media_rows += o.media_rows;
        self.duration_ms += o.duration_ms;
        for (k, v) in &o.by_method {
            *self.by_method.entry(k.clone()).or_default() += v;
        }
    }
}

fn genres(tags: &[String]) -> Vec<String> {
    let mut g: Vec<String> = Vec::new();
    for t in tags {
        let t = t.to_lowercase();
        if !GENRES.contains(&t.as_str()) {
            continue;
        }
        let t = if t == "science fiction" {
            "sci-fi".to_string()
        } else {
            t
        };
        if !g.contains(&t) {
            g.push(t);
        }
    }
    g
}

#[derive(FromRow)]
struct MediaIds {
    id: i64,
    kind: String,
    mal: Option<i32>,
    anilist: Option<i32>,
    anidb: Option<i32>,
    imdb: Option<String>,
    tmdb_movie: Option<i32>,
    tmdb_tv: Option<i32>,
    tvdb: Option<i32>,
}

/// Find or create the `media` row of an AOD entry. Ids are only ever added, never moved: an id
/// already held by another row stays there (and is left out here), so the unique indexes on
/// external ids cannot fail. A row created by another matcher for the same work (same IMDb /
/// TMDB / TVDB id, no anime ids yet) is adopted and gets the anime ids; its title and kind are
/// left to its owner.
async fn upsert_media(conn: &mut PgConnection, ix: &AnimeIndex, entry: usize) -> Result<i64> {
    let e = &ix.entries[entry];
    let c = ix.cross_ids(entry).owned();
    let (mal, anilist, anidb) = (opt_i32(e.mal), opt_i32(e.anilist), opt_i32(e.anidb));
    let (tmdb_movie, tmdb_tv, tvdb) = (opt_i32(c.tmdb_movie), opt_i32(c.tmdb_tv), opt_i32(c.tvdb));
    let rows = sqlx::query_as::<_, MediaIds>(
        "SELECT id, kind, mal, anilist, anidb, imdb, tmdb_movie, tmdb_tv, tvdb FROM media
         WHERE mal = $1 OR anilist = $2 OR anidb = $3 OR imdb = $4 OR tmdb_movie = $5
            OR tmdb_tv = $6 OR tvdb = $7",
    )
    .bind(mal)
    .bind(anilist)
    .bind(anidb)
    .bind(c.imdb.as_deref())
    .bind(tmdb_movie)
    .bind(tmdb_tv)
    .bind(tvdb)
    .fetch_all(&mut *conn)
    .await
    .map_err(db)?;
    let own = rows
        .iter()
        .find(|r| mal.is_some() && r.mal == mal)
        .or_else(|| {
            rows.iter()
                .find(|r| anilist.is_some() && r.anilist == anilist)
        })
        .or_else(|| rows.iter().find(|r| anidb.is_some() && r.anidb == anidb))
        .or_else(|| {
            rows.iter().find(|r| {
                r.mal.is_none()
                    && r.anilist.is_none()
                    && r.anidb.is_none()
                    && ((c.imdb.is_some() && r.imdb == c.imdb)
                        || (tmdb_movie.is_some() && r.tmdb_movie == tmdb_movie)
                        || (tmdb_tv.is_some() && r.tmdb_tv == tmdb_tv)
                        || (tvdb.is_some() && r.tvdb == tvdb))
            })
        });
    let own_id = own.map(|r| r.id);
    let taken = |f: &dyn Fn(&MediaIds) -> bool| rows.iter().any(|r| Some(r.id) != own_id && f(r));
    let mal = mal.filter(|_| !taken(&|r| r.mal == mal));
    let anilist = anilist.filter(|_| !taken(&|r| r.anilist == anilist));
    let anidb = anidb.filter(|_| !taken(&|r| r.anidb == anidb));
    let imdb = c.imdb.clone().filter(|_| !taken(&|r| r.imdb == c.imdb));
    let tmdb_movie = tmdb_movie.filter(|_| !taken(&|r| r.tmdb_movie == tmdb_movie));
    let tmdb_tv = tmdb_tv.filter(|_| !taken(&|r| r.tmdb_tv == tmdb_tv));
    let tvdb = tvdb.filter(|_| !taken(&|r| r.tvdb == tvdb));
    let now = magnetar_core::now();
    let runtime = e
        .duration_secs
        .filter(|&d| d >= 30)
        .map(|d| ((d + 30) / 60) as i32);
    let genres = genres(&e.tags);
    let title = e.title.replace('\0', "");
    match own {
        Some(r) => {
            // Our own row: refresh the descriptive fields. Someone else's: only add ids.
            let ours = r.kind == "anime";
            sqlx::query(
                "UPDATE media SET mal = COALESCE(mal, $2), anilist = COALESCE(anilist, $3),
                     anidb = COALESCE(anidb, $4), imdb = COALESCE(imdb, $5),
                     tmdb_movie = COALESCE(tmdb_movie, $6), tmdb_tv = COALESCE(tmdb_tv, $7),
                     tvdb = COALESCE(tvdb, $8),
                     title = CASE WHEN $9 THEN $10 ELSE title END,
                     year = CASE WHEN $9 THEN $11 ELSE COALESCE(year, $11) END,
                     genres = CASE WHEN $9 THEN $12 ELSE genres END,
                     rating = CASE WHEN $9 THEN $13 ELSE rating END,
                     runtime_min = CASE WHEN $9 THEN $14 ELSE COALESCE(runtime_min, $14) END,
                     updated_at = $15
                 WHERE id = $1",
            )
            .bind(r.id)
            .bind(mal)
            .bind(anilist)
            .bind(anidb)
            .bind(imdb)
            .bind(tmdb_movie)
            .bind(tmdb_tv)
            .bind(tvdb)
            .bind(ours)
            .bind(&title)
            .bind(e.year)
            .bind(&genres)
            .bind(e.score)
            .bind(runtime)
            .bind(now)
            .execute(&mut *conn)
            .await
            .map_err(db)?;
            Ok(r.id)
        }
        None => sqlx::query_scalar::<_, i64>(
            "INSERT INTO media (kind, imdb, tmdb_movie, tmdb_tv, tvdb, mal, anilist, anidb, title,
                 year, genres, rating, runtime_min, updated_at)
             VALUES ('anime', $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)
             RETURNING id",
        )
        .bind(imdb)
        .bind(tmdb_movie)
        .bind(tmdb_tv)
        .bind(tvdb)
        .bind(mal)
        .bind(anilist)
        .bind(anidb)
        .bind(&title)
        .bind(e.year)
        .bind(&genres)
        .bind(e.score)
        .bind(runtime)
        .bind(now)
        .fetch_one(&mut *conn)
        .await
        .map_err(db),
    }
}

/// `upsert_media` in a savepoint, retried once: a concurrent matcher may insert the same
/// external id between our SELECT and INSERT.
async fn upsert_media_retry(conn: &mut PgConnection, ix: &AnimeIndex, entry: usize) -> Result<i64> {
    for attempt in 0..2 {
        sqlx::query("SAVEPOINT anime_media")
            .execute(&mut *conn)
            .await
            .map_err(db)?;
        match upsert_media(conn, ix, entry).await {
            Ok(id) => {
                sqlx::query("RELEASE SAVEPOINT anime_media")
                    .execute(&mut *conn)
                    .await
                    .map_err(db)?;
                return Ok(id);
            }
            Err(e) => {
                sqlx::query("ROLLBACK TO SAVEPOINT anime_media")
                    .execute(&mut *conn)
                    .await
                    .map_err(db)?;
                if attempt == 1 {
                    return Err(e);
                }
            }
        }
    }
    unreachable!("loop returns")
}

/// Match the next `limit` anime torrents not yet tried with this dataset + matcher version.
pub async fn match_pending(
    pool: &PgPool,
    ix: &Arc<AnimeIndex>,
    dataset: &str,
    limit: i64,
) -> Result<MatchReport> {
    let started = Instant::now();
    let excluded: Vec<String> = EXCLUDED_PATHS.iter().map(|s| s.to_string()).collect();
    let rows = sqlx::query_as::<_, PendingRow>(
        "SELECT DISTINCT ON (t.infohash) t.infohash, t.title, t.description, t.files, t.external_ids
         FROM torrents t
         WHERE t.category = 'anime' AND NOT (t.category_path && $1::text[])
           AND NOT EXISTS (SELECT 1 FROM anime_attempts a WHERE a.infohash = t.infohash
                           AND a.dataset = $2 AND a.matcher_version = $3)
         ORDER BY t.infohash, t.trust DESC, t.created_at, t.event_id
         LIMIT $4",
    )
    .bind(&excluded)
    .bind(dataset)
    .bind(anime::MATCHER_VERSION)
    .bind(limit)
    .fetch_all(pool)
    .await
    .map_err(db)?;
    let mut report = MatchReport {
        scanned: rows.len() as u64,
        ..Default::default()
    };
    if rows.is_empty() {
        return Ok(report);
    }
    let ix2 = ix.clone();
    let results: Vec<(String, Option<AnimeMatch>)> = tokio::task::spawn_blocking(move || {
        rows.into_iter()
            .map(|r| {
                let m = identify(
                    &ix2,
                    &r.title,
                    &r.description,
                    &r.files.0,
                    &r.external_ids.0,
                );
                (r.infohash, m)
            })
            .collect()
    })
    .await
    .map_err(|e| Error::Other(e.to_string()))?;

    let now = magnetar_core::now();
    let mut tx = pool.begin().await.map_err(db)?;
    let hashes: Vec<&str> = results.iter().map(|(h, _)| h.as_str()).collect();
    // Existing links we must not replace: manual ones, and other matchers' at least as
    // confident as ours (the upsert below re-checks this, for races).
    let existing: HashMap<String, (String, f32, String)> =
        sqlx::query_as::<_, (String, String, f32, String)>(
            "SELECT tm.infohash, tm.method, tm.confidence, m.kind FROM torrent_media tm
         JOIN media m ON m.id = tm.media_id WHERE tm.infohash = ANY($1)",
        )
        .bind(&hashes)
        .fetch_all(&mut *tx)
        .await
        .map_err(db)?
        .into_iter()
        .map(|(h, method, conf, kind)| (h, (method, conf, kind)))
        .collect();
    let mut media_ids: HashMap<usize, i64> = HashMap::new();
    for (infohash, m) in &results {
        let m = m.as_ref().filter(|m| {
            let ok = m.confidence >= MIN_CONFIDENCE;
            if !ok {
                report.low_confidence += 1;
            }
            ok
        });
        let keep = existing.get(infohash).is_some_and(|(method, conf, kind)| {
            method == "manual" || (kind != "anime" && m.is_none_or(|m| *conf >= m.confidence))
        });
        if keep {
            report.kept += 1;
            if m.is_none() {
                report.unmatched += 1;
            }
            continue;
        }
        match m {
            Some(m) => {
                let media_id = match media_ids.get(&m.entry) {
                    Some(&id) => id,
                    None => {
                        let id = upsert_media_retry(&mut tx, ix, m.entry).await?;
                        media_ids.insert(m.entry, id);
                        id
                    }
                };
                let written = sqlx::query(
                    "INSERT INTO torrent_media (infohash, media_id, season, episode, method,
                         confidence, matcher_version, matched_at)
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
                     ON CONFLICT (infohash) DO UPDATE SET media_id = EXCLUDED.media_id,
                         season = EXCLUDED.season, episode = EXCLUDED.episode,
                         method = EXCLUDED.method, confidence = EXCLUDED.confidence,
                         matcher_version = EXCLUDED.matcher_version,
                         matched_at = EXCLUDED.matched_at
                     WHERE torrent_media.method <> 'manual'
                       AND (EXCLUDED.confidence > torrent_media.confidence
                            OR EXISTS (SELECT 1 FROM media m WHERE m.id = torrent_media.media_id
                                       AND m.kind = 'anime'))",
                )
                .bind(infohash)
                .bind(media_id)
                .bind(m.season)
                .bind(m.episode.filter(|&e| e > 0))
                .bind(m.how.method())
                .bind(m.confidence)
                .bind(anime::MATCHER_VERSION)
                .bind(now)
                .execute(&mut *tx)
                .await
                .map_err(db)?
                .rows_affected();
                if written == 0 {
                    report.kept += 1;
                } else {
                    report.matched += 1;
                    *report
                        .by_method
                        .entry(format!("{:?}", m.how).to_lowercase())
                        .or_default() += 1;
                }
            }
            None => {
                report.unmatched += 1;
                // A title guess of ours from an older dataset / matcher no longer holds.
                sqlx::query(
                    "DELETE FROM torrent_media tm USING media m
                     WHERE tm.infohash = $1 AND tm.media_id = m.id AND m.kind = 'anime'
                       AND tm.method = 'offline_title'",
                )
                .bind(infohash)
                .execute(&mut *tx)
                .await
                .map_err(db)?;
            }
        }
    }
    let mut qb: QueryBuilder<Postgres> = QueryBuilder::new(
        "INSERT INTO anime_attempts (infohash, dataset, matcher_version, matched, tried_at) ",
    );
    qb.push_values(&results, |mut b, (infohash, m)| {
        b.push_bind(infohash)
            .push_bind(dataset)
            .push_bind(anime::MATCHER_VERSION)
            .push_bind(m.as_ref().is_some_and(|m| m.confidence >= MIN_CONFIDENCE))
            .push_bind(now);
    });
    qb.push(
        " ON CONFLICT (infohash) DO UPDATE SET dataset = EXCLUDED.dataset,
              matcher_version = EXCLUDED.matcher_version, matched = EXCLUDED.matched,
              tried_at = EXCLUDED.tried_at",
    );
    qb.build().execute(&mut *tx).await.map_err(db)?;
    tx.commit().await.map_err(db)?;
    report.media_rows = media_ids.len() as u64;
    report.duration_ms = started.elapsed().as_millis() as u64;
    Ok(report)
}

/// Match every pending torrent (batches of [`BATCH`]).
pub async fn match_all(pool: &PgPool, ix: &Arc<AnimeIndex>, dataset: &str) -> Result<MatchReport> {
    let mut total = MatchReport::default();
    loop {
        let r = match_pending(pool, ix, dataset, BATCH).await?;
        total.add(&r);
        if r.scanned < BATCH as u64 {
            return Ok(total);
        }
    }
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct AnimeRunReport {
    pub refresh: RefreshReport,
    pub index_entries: usize,
    pub dataset: Option<String>,
    pub matching: MatchReport,
}

/// One full run (CLI `enrich-anime`): refresh datasets when due (or `force`), then match all.
pub async fn run_once(pool: &PgPool, cfg: &Config, force: bool) -> Result<AnimeRunReport> {
    if !contract_tables_exist(pool).await? {
        return Err(Error::Store(
            "media / torrent_media tables missing (they come with the IMDb enrichment migration)"
                .into(),
        ));
    }
    let mut out = AnimeRunReport {
        refresh: refresh(pool, cfg, force).await?,
        ..Default::default()
    };
    let Some((ix, version)) = load_index(pool, fribb_enabled(cfg)).await? else {
        return Ok(out);
    };
    out.index_entries = ix.len();
    let ix = Arc::new(ix);
    out.matching = match_all(pool, &ix, &version).await?;
    out.dataset = Some(version);
    Ok(out)
}

/// Daemon run by `serve` when `MAGNETAR_ANIME_DB` is on (default).
pub async fn anime_loop(pool: PgPool, cfg: Arc<Config>, mut shutdown: watch::Receiver<bool>) {
    if !cfg.anime_db {
        return;
    }
    tokio::select! {
        _ = tokio::time::sleep(STARTUP_DELAY) => {}
        _ = stop_requested(&mut shutdown) => return,
    }
    let mut index: Option<(Arc<AnimeIndex>, String)> = None;
    let mut warned = false;
    loop {
        let pass = async {
            match contract_tables_exist(&pool).await {
                Ok(true) => {}
                Ok(false) => {
                    if !warned {
                        tracing::warn!("anime: media/torrent_media tables missing, matching off");
                        warned = true;
                    }
                    return;
                }
                Err(e) => {
                    tracing::warn!("anime: {e}");
                    return;
                }
            }
            match refresh(&pool, &cfg, false).await {
                Ok(r) if r.changed => index = None,
                Ok(_) => {}
                Err(e) => tracing::warn!("anime: dataset refresh: {e}"),
            }
            if index.is_none() {
                match load_index(&pool, fribb_enabled(&cfg)).await {
                    Ok(Some((ix, v))) => {
                        tracing::info!("anime: index of {} entries ({v})", ix.len());
                        index = Some((Arc::new(ix), v));
                    }
                    Ok(None) => {}
                    Err(e) => tracing::warn!("anime: load index: {e}"),
                }
            }
            if let Some((ix, v)) = &index {
                match match_all(&pool, ix, v).await {
                    Ok(r) if r.scanned > 0 => tracing::info!(
                        scanned = r.scanned,
                        matched = r.matched,
                        unmatched = r.unmatched,
                        kept = r.kept,
                        "anime: matched in {} ms",
                        r.duration_ms
                    ),
                    Ok(_) => {}
                    Err(e) => tracing::warn!("anime: matching: {e}"),
                }
            }
        };
        tokio::select! {
            _ = pass => {}
            _ = stop_requested(&mut shutdown) => return,
        }
        tokio::select! {
            _ = tokio::time::sleep(LOOP_EVERY) => {}
            _ = stop_requested(&mut shutdown) => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_fansub_names() {
        let r = parse_release(
            "[Yameii] Rent-a-Girlfriend - S05E04 [English Dub] [CR WEB-DL 720p H264 AAC] [F7D24D3B] (Kanojo, Okarishimasu Season 5 | S5)",
        );
        assert_eq!(r.titles[0], "Rent-a-Girlfriend");
        assert!(
            r.titles
                .iter()
                .any(|t| t == "Kanojo, Okarishimasu Season 5")
        );
        assert_eq!((r.season, r.episode), (Some(5), Some(4)));
        let r = parse_release(
            "[Erai-raws] Fumetsu no Anata e Season 3 - 10 [1080p CR WEB-DL AVC AAC][MultiSub][7ECB6534]",
        );
        assert_eq!((r.season, r.episode), (Some(3), Some(10)));
        let r = parse_release("[SubsPlease] Golden Kamuy - 38 (1080p) [F1434F11].mkv");
        assert_eq!(r.titles[0], "Golden Kamuy");
        assert_eq!((r.season, r.episode), (None, Some(38)));
        let r = parse_release("[Buzz-Subs] Black Clover (2017) S01E158-S01E170 (BD 1080p AV1 AAC)");
        assert_eq!(r.year, Some(2017));
        assert_eq!(r.season, Some(1));
        let r = parse_release(
            "Sousou no Frieren TV-2 | Фрирен [ТВ-2] | Frieren: Beyond Journey's End 2 [2026] [03 of 10]",
        );
        assert!(
            r.titles
                .iter()
                .any(|t| t == "Frieren: Beyond Journey's End 2")
        );
    }

    #[test]
    fn release_tags() {
        assert_eq!(
            release_tag("https://github.com/x/y/releases/download/2026-43/a.jsonl.zst").as_deref(),
            Some("2026-43")
        );
        assert_eq!(release_tag(DEFAULT_FRIBB_URL), None);
    }

    #[test]
    fn decodes_plain_and_zstd_aod() {
        let doc = concat!(
            r#"{"$schema":"x","license":{"name":"ODbL"},"lastUpdate":"2026-10-25"}"#,
            "\n",
            r#"{"sources":["https://myanimelist.net/anime/1"],"title":"Cowboy Bebop","type":"TV","episodes":26}"#,
            "\n"
        );
        let (h, e, bad) = decode_aod(doc.as_bytes()).unwrap();
        assert_eq!(h.unwrap().last_update.as_deref(), Some("2026-10-25"));
        assert_eq!((e.len(), bad), (1, 0));
        // zstd frame of the same document, produced by `zstd -19` (ruzstd only decodes).
        let z = include_bytes!("../tests/data/aod-sample.jsonl.zst");
        let (h, e, bad) = decode_aod(z).unwrap();
        assert!(h.is_some());
        assert_eq!(e[0].title, "Cowboy Bebop");
        assert_eq!(bad, 0);
    }

    #[test]
    fn genre_filter() {
        let tags = ["Action", "female protagonist", "science fiction", "action"]
            .map(String::from)
            .to_vec();
        assert_eq!(genres(&tags), vec!["action", "sci-fi"]);
    }

    #[test]
    fn sources_default_and_override() {
        use clap::Parser;
        let cli = crate::config::Cli::try_parse_from(["magnetar-server", "serve"]).unwrap();
        assert!(cli.config.anime_db);
        assert_eq!(aod_source(&cli.config), DEFAULT_AOD_URL);
        assert_eq!(fribb_source(&cli.config), DEFAULT_FRIBB_URL);
        assert!(fribb_enabled(&cli.config));
        let cli = crate::config::Cli::try_parse_from([
            "magnetar-server",
            "--anime-db",
            "false",
            "--anime-aod-url",
            "/data/aod.jsonl.zst",
            "--anime-fribb-url",
            "None",
            "serve",
        ])
        .unwrap();
        assert!(!cli.config.anime_db);
        assert_eq!(aod_source(&cli.config), "/data/aod.jsonl.zst");
        assert!(!fribb_enabled(&cli.config));
    }
}

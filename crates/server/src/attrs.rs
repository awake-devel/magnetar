//! Release attributes (D44, docs/FILTERS.md): `core::release` parses a torrent's name and files;
//! `PgStore::put_torrent` stores the result for new infohashes in `torrent_attrs`, and the
//! backfill here parses every infohash without a row or with one from an older
//! `PARSER_VERSION`, walking `torrent_infohashes` in chunks with the parsing on blocking threads.

use std::time::{Duration, Instant};

use magnetar_core::error::Result;
use magnetar_core::model::{Category, TorrentFile};
use magnetar_core::release::{PARSER_VERSION, ReleaseAttrs, parse_release};
use serde::Serialize;
use sqlx::types::Json;
use sqlx::{FromRow, PgPool};
use tokio::sync::watch;

use crate::daemons::stop_requested;
use crate::store::db;

/// Infohashes looked at per chunk (one keyset read, one event read, one write).
pub const CHUNK: i64 = 2000;
/// Pause between chunks: the backfill shares Postgres with the crawler and the API.
pub const CHUNK_PAUSE: Duration = Duration::from_millis(20);

#[derive(Clone, Debug, Default, Serialize)]
pub struct BackfillReport {
    /// Infohashes walked.
    pub scanned: u64,
    /// Rows written (missing or older parser).
    pub parsed: u64,
    pub secs: f64,
}

#[derive(FromRow)]
struct EventRow {
    infohash: String,
    title: String,
    category: String,
    files: Json<Vec<TorrentFile>>,
}

/// Whether anything needs parsing: an infohash without attributes or with an older parser's.
pub async fn pending(pool: &PgPool) -> Result<bool> {
    sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM torrent_attrs WHERE parser_version < $1)
             OR EXISTS (SELECT 1 FROM torrent_infohashes ti
                        WHERE NOT EXISTS (SELECT 1 FROM torrent_attrs a
                                          WHERE a.infohash = ti.infohash))",
    )
    .bind(PARSER_VERSION)
    .fetch_one(pool)
    .await
    .map_err(db)
}

/// Write parsed attributes; a row from the current parser is only replaced when `force`.
async fn upsert(pool: &PgPool, rows: &[(String, ReleaseAttrs)], force: bool) -> Result<u64> {
    if rows.is_empty() {
        return Ok(0);
    }
    let body: Vec<serde_json::Value> = rows
        .iter()
        .map(|(ih, a)| serde_json::json!({ "ih": ih, "a": a }))
        .collect();
    let r = sqlx::query(
        "INSERT INTO torrent_attrs (infohash, parser_version, attrs)
         SELECT x->>'ih', $2, x->'a' FROM jsonb_array_elements($1) x
         ORDER BY 1
         ON CONFLICT (infohash) DO UPDATE
             SET parser_version = EXCLUDED.parser_version, attrs = EXCLUDED.attrs
             WHERE torrent_attrs.parser_version < EXCLUDED.parser_version OR $3",
    )
    .bind(Json(body))
    .bind(PARSER_VERSION)
    .bind(force)
    .execute(pool)
    .await
    .map_err(db)?;
    Ok(r.rows_affected())
}

/// Parse every infohash that needs it (all of them with `force`). Stops early, with what it
/// did so far, when `stop` resolves.
pub async fn backfill(
    pool: &PgPool,
    force: bool,
    mut shutdown: Option<watch::Receiver<bool>>,
) -> Result<BackfillReport> {
    let started = Instant::now();
    let mut report = BackfillReport::default();
    let mut cursor = String::new();
    loop {
        let page: Vec<String> = sqlx::query_scalar(
            "SELECT infohash FROM torrent_infohashes WHERE infohash > $1
             ORDER BY infohash LIMIT $2",
        )
        .bind(&cursor)
        .bind(CHUNK)
        .fetch_all(pool)
        .await
        .map_err(db)?;
        let Some(last) = page.last().cloned() else {
            break;
        };
        report.scanned += page.len() as u64;
        // The first event of each infohash that needs parsing, as at ingest.
        let rows: Vec<EventRow> = sqlx::query_as(
            "SELECT DISTINCT ON (t.infohash) t.infohash, t.title, t.category, t.files
             FROM torrents t LEFT JOIN torrent_attrs a ON a.infohash = t.infohash
             WHERE t.infohash = ANY($1) AND (a.infohash IS NULL OR a.parser_version < $2 OR $3)
             ORDER BY t.infohash, t.created_at, t.event_id",
        )
        .bind(&page)
        .bind(PARSER_VERSION)
        .bind(force)
        .fetch_all(pool)
        .await
        .map_err(db)?;
        if !rows.is_empty() {
            let parsed = tokio::task::spawn_blocking(move || {
                rows.into_iter()
                    .map(|r| {
                        let cat = r.category.parse().unwrap_or(Category::Other);
                        let a = parse_release(&r.title, &r.files.0, cat);
                        (r.infohash, a)
                    })
                    .collect::<Vec<_>>()
            })
            .await
            .map_err(|e| magnetar_core::Error::Other(format!("attrs: {e}")))?;
            report.parsed += upsert(pool, &parsed, force).await?;
        }
        cursor = last;
        let stop = async {
            match shutdown.as_mut() {
                Some(s) => stop_requested(s).await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            _ = tokio::time::sleep(CHUNK_PAUSE) => {}
            _ = stop => break,
        }
    }
    report.secs = started.elapsed().as_secs_f64();
    Ok(report)
}

/// Wait before retrying a backfill that failed (Postgres restarting, …).
pub const RETRY_PAUSE: Duration = Duration::from_secs(300);

/// `serve`: one backfill at startup when something is pending (after a parser change, or rows
/// written before D44), retried after an error. New torrents are parsed at ingest, so nothing
/// runs once it has finished.
pub async fn backfill_loop(pool: PgPool, mut shutdown: watch::Receiver<bool>) {
    loop {
        match pending(&pool).await {
            Ok(false) => return,
            Ok(true) => {
                tracing::info!("release attributes: parsing torrents (parser v{PARSER_VERSION})");
                match backfill(&pool, false, Some(shutdown.clone())).await {
                    Ok(r) => {
                        tracing::info!(
                            "release attributes: {} parsed of {} infohashes in {:.0} s",
                            r.parsed,
                            r.scanned,
                            r.secs
                        );
                        return;
                    }
                    Err(e) => tracing::warn!("release attributes backfill: {e}"),
                }
            }
            Err(e) => tracing::warn!("release attributes: {e}"),
        }
        tokio::select! {
            _ = tokio::time::sleep(RETRY_PAUSE) => {}
            _ = stop_requested(&mut shutdown) => return,
        }
    }
}

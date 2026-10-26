//! Fetch results → Postgres (docs/DHT.md §2.4, D50, D53): verified torrents into `dht_torrents`
//! and `torrent_attrs`, final statuses and backoff into `dht_seen`, blocked hashes into
//! `blocked_infohashes`. One transaction per batch (at most [`BATCH_MAX`] results or
//! [`BATCH_WAIT`]).
//!
//! Order per verified torrent (D53): the content screen runs **before** any statement that
//! carries its name; a block writes only hashes; no screen at all → nothing named is written
//! (fail closed, `screen.rs`). Nothing here logs a name or path.

use std::time::Duration;

use magnetar_core::dht::Via;
use magnetar_core::metainfo::{FetchFailure, Fetched, Outcome, PeerError};
use magnetar_core::model::{Category, TorrentFile};
use magnetar_core::release;
use serde::Serialize;
use sqlx::types::Json;
use sqlx::{PgPool, Postgres, Transaction};

use super::screen::{Screen, UNSCREENED_RETRY_SECS, Verdict};
use super::store::BACKOFF_SECS;

pub const BATCH_MAX: usize = 500;
pub const BATCH_WAIT: Duration = Duration::from_secs(1);
/// `dht_torrents.files` keeps the first this many files (`file_count` has the real count).
pub const FILES_MAX: usize = 5000;
/// `dht_torrents.files_text` keeps the first this many bytes of the paths.
pub const FILES_TEXT_MAX: usize = 64 * 1024;
/// `classifier_version` of rows stored before `core::classify` exists (stage 3): category
/// `other`, re-classified by the first classifier version.
pub const UNCLASSIFIED: i16 = 0;

pub const STATUS_INVALID: i16 = 3;
pub const STATUS_PRIVATE: i16 = 4;
pub const STATUS_V2_ONLY: i16 = 5;
pub const STATUS_OVERSIZE: i16 = 6;
pub const STATUS_ALIAS: i16 = 8;

/// One finished fetch, keyed by the hash it was triaged under.
#[derive(Debug)]
pub struct FetchResult {
    pub sighted: [u8; 20],
    pub via: Via,
    /// Distinct peers known when the fetch started.
    pub peers_seen: usize,
    pub result: Result<Outcome, FetchFailure>,
}

#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
pub struct PersistOutcome {
    /// New `dht_torrents` rows.
    pub stored: u64,
    /// Verified, but a row for the hash already existed (or a hybrid's v1 is known/blocked).
    pub not_new: u64,
    /// Hybrids sighted under their truncated v2 hash (sighted row kept as `alias`).
    pub alias: u64,
    pub blocked: u64,
    /// Verified but dropped: no content screen installed (stage 2, fail closed).
    pub unscreened: u64,
    pub private: u64,
    pub v2_only: u64,
    pub invalid: u64,
    pub oversize: u64,
    pub failed: u64,
}

/// A loggable description of a persist error that cannot carry a bound value (a torrent name):
/// the SQLSTATE for database errors, the error kind otherwise.
pub fn error_class(e: &sqlx::Error) -> String {
    match e {
        sqlx::Error::Database(d) => format!(
            "database error {}",
            d.code().as_deref().unwrap_or("(no SQLSTATE)")
        ),
        sqlx::Error::PoolTimedOut => "pool timed out".into(),
        sqlx::Error::PoolClosed => "pool closed".into(),
        sqlx::Error::Io(e) => format!("io: {}", e.kind()),
        sqlx::Error::Encode(_) => "encode error".into(),
        _ => "other sqlx error".into(),
    }
}

fn via_text(v: Via) -> &'static str {
    match v {
        Via::Sample => "sample",
        Via::Announce => "announce",
        Via::GetPeers => "get_peers",
    }
}

fn clean(s: &str) -> String {
    s.replace('\0', "")
}

/// Paths joined by `\n`, cut at [`FILES_TEXT_MAX`] bytes on a character boundary.
pub fn files_text(files: &[TorrentFile]) -> String {
    let mut s = String::new();
    for f in files {
        if !s.is_empty() {
            s.push('\n');
        }
        s.push_str(&f.path);
        if s.len() >= FILES_TEXT_MAX {
            let mut cut = FILES_TEXT_MAX;
            while !s.is_char_boundary(cut) {
                cut -= 1;
            }
            s.truncate(cut);
            break;
        }
    }
    s
}

async fn set_final(
    tx: &mut Transaction<'_, Postgres>,
    h: &[u8; 20],
    status: i16,
    code: &str,
) -> sqlx::Result<()> {
    sqlx::query(
        "UPDATE dht_seen SET status = $2, attempts = attempts + 1, next_attempt_at = NULL,
             last_error = left($3, 32)
         WHERE infohash = $1",
    )
    .bind(h.as_slice())
    .bind(status)
    .bind(code)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Same backoff as `store::mark_failed` (1 h, 6 h, 24 h, then never), inside the batch.
async fn set_failed(
    tx: &mut Transaction<'_, Postgres>,
    h: &[u8; 20],
    code: &str,
    now: i64,
) -> sqlx::Result<()> {
    sqlx::query(
        r#"
        UPDATE dht_seen SET status = 2, attempts = attempts + 1, last_error = left($2, 32),
            next_attempt_at = CASE attempts + 1 WHEN 1 THEN $3 + $4 WHEN 2 THEN $3 + $5
                                                WHEN 3 THEN $3 + $6 ELSE NULL END
        WHERE infohash = $1
        "#,
    )
    .bind(h.as_slice())
    .bind(code)
    .bind(now)
    .bind(BACKOFF_SECS[0])
    .bind(BACKOFF_SECS[1])
    .bind(BACKOFF_SECS[2])
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn block(
    tx: &mut Transaction<'_, Postgres>,
    f: &Fetched,
    rule_version: i16,
    now: i64,
) -> sqlx::Result<()> {
    let mut hashes = vec![hex::encode(f.v1)];
    if f.sighted != f.v1 {
        hashes.push(hex::encode(f.sighted));
    }
    sqlx::query(
        "INSERT INTO blocked_infohashes (infohash, blocked_at, rule_version, origin)
         SELECT h, $2, $3, 'dht' FROM unnest($1::text[]) AS u(h) ORDER BY h
         ON CONFLICT (infohash) DO NOTHING",
    )
    .bind(&hashes)
    .bind(now)
    .bind(rule_version)
    .execute(&mut **tx)
    .await?;
    // A row stored meanwhile under the same hash (a race with another batch) goes too.
    sqlx::query("DELETE FROM dht_torrents WHERE infohash = ANY($1)")
        .bind(&hashes)
        .execute(&mut **tx)
        .await?;
    sqlx::query("DELETE FROM dht_seen WHERE infohash = $1")
        .bind(f.sighted.as_slice())
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// Insert a screened torrent. Returns whether a new row was written.
async fn store(
    tx: &mut Transaction<'_, Postgres>,
    f: &Fetched,
    via: Via,
    peers_seen: usize,
    fetch_known: bool,
    now: i64,
) -> sqlx::Result<bool> {
    let v1 = hex::encode(f.v1);
    let title = clean(&f.info.name);
    let files: Vec<TorrentFile> = f
        .info
        .files
        .iter()
        .map(|x| TorrentFile {
            path: clean(&x.path),
            size: Some(x.size),
        })
        .collect();
    let kept = &files[..files.len().min(FILES_MAX)];
    let text = files_text(kept);
    // A hybrid sighted by its v2 hash was never triaged under its SHA-1: the same checks as a
    // sighting apply (docs/DHT.md §4 step 5) — blocked always, known unless fetch_known.
    let skip_known_check = f.sighted == f.v1 || fetch_known;
    let inserted = sqlx::query(
        r#"
        INSERT INTO dht_torrents (infohash, infohash_v2, title, files, file_count, files_text,
            total_size, category, metadata_size, piece_length, first_seen, fetched_at,
            discovered_via, peers_seen, classifier_version)
        SELECT $1, $2, $3, $4, $5, $6, $7, $8, $9, $10,
               COALESCE((SELECT first_seen FROM dht_seen WHERE infohash = $11), $12), $12,
               $13, $14, $15
        WHERE NOT EXISTS (SELECT 1 FROM blocked_infohashes b WHERE b.infohash = $1)
          AND ($16 OR NOT EXISTS (SELECT 1 FROM torrent_infohashes t WHERE t.infohash = $1))
        ON CONFLICT (infohash) DO NOTHING
        "#,
    )
    .bind(&v1)
    .bind(f.v2.map(hex::encode))
    .bind(&title)
    .bind(Json(kept))
    .bind(i32::try_from(files.len()).unwrap_or(i32::MAX))
    .bind(&text)
    .bind(i64::try_from(f.info.total_size).unwrap_or(i64::MAX))
    .bind(Category::Other.as_str())
    .bind(i32::try_from(f.metadata_size).unwrap_or(i32::MAX))
    .bind(f.info.piece_length.and_then(|p| i32::try_from(p).ok()))
    .bind(f.sighted.as_slice())
    .bind(now)
    .bind(via_text(via))
    .bind(i32::try_from(peers_seen).unwrap_or(i32::MAX))
    .bind(UNCLASSIFIED)
    .bind(skip_known_check)
    .execute(&mut **tx)
    .await?
    .rows_affected()
        == 1;
    if inserted {
        // Release attributes (D44) from the DHT name and files; first writer wins, as for
        // Nostr (a Nostr event stored later keeps these unless the parser version moved).
        let attrs = release::parse_release(&title, kept, Category::Other);
        sqlx::query(
            "INSERT INTO torrent_attrs (infohash, parser_version, attrs) VALUES ($1, $2, $3)
             ON CONFLICT (infohash) DO NOTHING",
        )
        .bind(&v1)
        .bind(release::PARSER_VERSION)
        .bind(Json(&attrs))
        .execute(&mut **tx)
        .await?;
    }
    if f.sighted == f.v1 {
        sqlx::query("DELETE FROM dht_seen WHERE infohash = $1")
            .bind(f.sighted.as_slice())
            .execute(&mut **tx)
            .await?;
    } else {
        // Keep the truncated v2 hash as a final alias row: deleting it would let it be fetched
        // again every time it is sampled (it is not in torrent_infohashes), docs/DHT.md §2.4.
        sqlx::query(
            "UPDATE dht_seen SET status = $2, next_attempt_at = NULL, last_error = NULL
             WHERE infohash = $1",
        )
        .bind(f.sighted.as_slice())
        .bind(STATUS_ALIAS)
        .execute(&mut **tx)
        .await?;
    }
    Ok(inserted)
}

/// Write one batch of results in one transaction.
pub async fn persist_batch(
    pool: &PgPool,
    items: &[FetchResult],
    screen: Option<&dyn Screen>,
    fetch_known: bool,
    now: i64,
) -> sqlx::Result<PersistOutcome> {
    let mut o = PersistOutcome::default();
    let mut tx = pool.begin().await?;
    for it in items {
        match &it.result {
            Ok(Outcome::Ok(f)) => {
                let Some(screen) = screen else {
                    sqlx::query(
                        "UPDATE dht_seen SET status = 0, next_attempt_at = $2,
                             last_error = 'unscreened'
                         WHERE infohash = $1",
                    )
                    .bind(it.sighted.as_slice())
                    .bind(now + UNSCREENED_RETRY_SECS)
                    .execute(&mut *tx)
                    .await?;
                    o.unscreened += 1;
                    continue;
                };
                match screen.verdict(&f.info.all_names) {
                    Verdict::Block => {
                        block(&mut tx, f, screen.rule_version(), now).await?;
                        o.blocked += 1;
                    }
                    Verdict::Allow => {
                        if store(&mut tx, f, it.via, it.peers_seen, fetch_known, now).await? {
                            o.stored += 1;
                        } else {
                            o.not_new += 1;
                        }
                        if f.sighted != f.v1 {
                            o.alias += 1;
                        }
                    }
                }
            }
            Ok(Outcome::Private) => {
                set_final(&mut tx, &it.sighted, STATUS_PRIVATE, "private").await?;
                o.private += 1;
            }
            Ok(Outcome::V2Only) => {
                set_final(&mut tx, &it.sighted, STATUS_V2_ONLY, "v2_only").await?;
                o.v2_only += 1;
            }
            Ok(Outcome::Invalid(e)) => {
                set_final(&mut tx, &it.sighted, STATUS_INVALID, e.code()).await?;
                o.invalid += 1;
            }
            Err(f) if f.oversize => {
                set_final(
                    &mut tx,
                    &it.sighted,
                    STATUS_OVERSIZE,
                    PeerError::Oversize.code(),
                )
                .await?;
                o.oversize += 1;
            }
            Err(f) => {
                set_failed(&mut tx, &it.sighted, f.code(), now).await?;
                o.failed += 1;
            }
        }
    }
    tx.commit().await?;
    Ok(o)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_text_caps_on_a_char_boundary() {
        let f = |p: &str| TorrentFile {
            path: p.into(),
            size: Some(1),
        };
        assert_eq!(files_text(&[f("a/b"), f("c")]), "a/b\nc");
        let long = "é".repeat(FILES_TEXT_MAX); // 2 bytes each
        let t = files_text(&[f("x"), f(&long)]);
        assert!(t.len() <= FILES_TEXT_MAX && t.len() > FILES_TEXT_MAX - 2);
        assert!(t.starts_with("x\né"));
    }
}

//! `SqliteStore`: the phone cache implementing `core::store::Store` (ARCHITECTURE.md §4–§5, §7).
//!
//! Every method is a short synchronous SQLite call behind one `Mutex<Connection>` (WAL); the async
//! trait methods simply wrap the sync ones.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Mutex, MutexGuard};

use magnetar_core::error::{Error, Result};
use magnetar_core::model::*;
use magnetar_core::query::{SearchPage, SearchQuery, Sort, fts5_match};
use magnetar_core::store::{DEFAULT_TRUST_THRESHOLD, Store, keys};
use magnetar_core::sync::SyncSource;
use magnetar_core::{magnet, trust, wot};
use rusqlite::types::Value;
use rusqlite::{Connection, OptionalExtension, Row, Transaction, params, params_from_iter};

use crate::schema;

/// Longest relay backoff (ARCHITECTURE.md §4: max 24 h).
pub const MAX_BACKOFF_SECS: i64 = 24 * 3600;
const BACKOFF_BASE_SECS: i64 = 60;
const REPLACEABLE_KINDS: &[u16] = &[
    KIND_PROFILE,
    KIND_CONTACTS,
    KIND_MUTE_LIST,
    KIND_RELAY_LIST,
    KIND_KIND_MUTE_SET,
];

/// Relay retry delay after `failures` consecutive failures: 60 s doubling, capped at 24 h.
pub fn backoff_secs(failures: u32) -> i64 {
    let exp = failures.saturating_sub(1).min(20);
    (BACKOFF_BASE_SECS << exp).min(MAX_BACKOFF_SECS)
}

/// Same rule as `core::trust::event_trust`: publisher score + PoW bonus, with 0/100 (overrides,
/// root mute) ignoring PoW. Unscored publishers count as 0 + PoW bonus.
pub fn event_trust(score: Option<u8>, pow_bits: u8) -> u8 {
    match score {
        Some(s @ (0 | 100)) => s,
        Some(s) => s.saturating_add(trust::pow_bonus(pow_bits)).min(100),
        None => trust::pow_bonus(pow_bits).min(100),
    }
}

/// SQL twin of `event_trust` for a known score bound as ?1.
const EVENT_TRUST_SQL: &str = "CASE WHEN ?1 IN (0, 100) THEN ?1 \
     ELSE MIN(100, ?1 + (25 * MAX(0, MIN(16, pow_bits - 8))) / 16) END";

const BEST_PEERS_SQL: &str = "CASE WHEN l.seeders IS NULL AND l.dht_peers_est IS NULL THEN NULL \
     ELSE MAX(COALESCE(l.seeders, 0), COALESCE(l.dht_peers_est, 0)) END";

const BLOCKED_SQL: &str = "SELECT pubkey FROM overrides WHERE action = 'block'";

#[derive(Clone, Debug, Default, PartialEq)]
pub struct CleanupReport {
    pub evicted: u64,
    pub bytes_before: u64,
    pub bytes_after: u64,
    pub unsynced_kept: u64,
}

pub struct SqliteStore {
    conn: Mutex<Connection>,
}

trait DbExt<T> {
    fn db(self) -> Result<T>;
}

impl<T> DbExt<T> for rusqlite::Result<T> {
    fn db(self) -> Result<T> {
        self.map_err(|e| Error::Store(e.to_string()))
    }
}

fn json<T: serde::Serialize>(v: &T) -> String {
    serde_json::to_string(v).unwrap_or_else(|_| "null".into())
}

fn from_json<T: serde::de::DeserializeOwned + Default>(s: &str) -> T {
    serde_json::from_str(s).unwrap_or_default()
}

fn u64_of(v: Option<i64>) -> Option<u64> {
    v.map(|n| n.max(0) as u64)
}

fn u32_of(v: Option<i64>) -> Option<u32> {
    v.map(|n| n.clamp(0, u32::MAX as i64) as u32)
}

fn i64_of(v: u64) -> i64 {
    v.min(i64::MAX as u64) as i64
}

fn liveness_source_str(s: LivenessSource) -> &'static str {
    match s {
        LivenessSource::Udp => "udp",
        LivenessSource::Http => "http",
        LivenessSource::Dht => "dht",
        LivenessSource::DhtValues => "dht_values",
        LivenessSource::None => "none",
    }
}

fn liveness_source_of(s: &str) -> LivenessSource {
    match s {
        "udp" => LivenessSource::Udp,
        "http" => LivenessSource::Http,
        "dht" => LivenessSource::Dht,
        "dht_values" => LivenessSource::DhtValues,
        _ => LivenessSource::None,
    }
}

fn action_str(a: OverrideAction) -> &'static str {
    match a {
        OverrideAction::Block => "block",
        OverrideAction::Trust => "trust",
    }
}

fn liveness_from_row(r: &Row, at: usize) -> rusqlite::Result<Option<Liveness>> {
    let Some(infohash) = r.get::<_, Option<String>>(at)? else {
        return Ok(None);
    };
    Ok(Some(Liveness {
        infohash,
        seeders: u32_of(r.get(at + 1)?),
        leechers: u32_of(r.get(at + 2)?),
        completed: u32_of(r.get(at + 3)?),
        dht_peers_est: u32_of(r.get(at + 4)?),
        source: liveness_source_of(&r.get::<_, String>(at + 5)?),
        trackers_ok: r.get::<_, i64>(at + 6)?.clamp(0, u16::MAX as i64) as u16,
        trackers_failed: r.get::<_, i64>(at + 7)?.clamp(0, u16::MAX as i64) as u16,
        checked_at: r.get(at + 8)?,
        expires_at: r.get(at + 9)?,
    }))
}

const LIVENESS_COLS: &str = "l.infohash, l.seeders, l.leechers, l.completed, l.dht_peers_est, \
     l.source, l.trackers_ok, l.trackers_failed, l.checked_at, l.expires_at";

/// `dead` for presentation: None unless a fresh answer exists (D17: unknown is never dead).
fn dead_of(l: Option<&Liveness>, now: i64) -> Option<bool> {
    let l = l?;
    if now >= l.expires_at {
        return None;
    }
    l.best_peers().map(|p| p == 0)
}

/// Full torrent row (detail + events list).
struct TorrentRow {
    event_id: String,
    pubkey: String,
    created_at: i64,
    title: String,
    description: String,
    files: Vec<TorrentFile>,
    file_count: u32,
    total_size: Option<u64>,
    trackers: Vec<String>,
    category: Category,
    category_path: Vec<String>,
    hashtags: Vec<String>,
    external_ids: Vec<(String, String)>,
    source_url: Option<String>,
    pow_bits: u8,
    trust: u8,
}

const TORRENT_COLS: &str = "event_id, pubkey, created_at, title, description, files, file_count, \
     total_size, trackers, category, category_path, hashtags, external_ids, source_url, pow_bits, trust";

fn torrent_from_row(r: &Row) -> rusqlite::Result<TorrentRow> {
    Ok(TorrentRow {
        event_id: r.get(0)?,
        pubkey: r.get(1)?,
        created_at: r.get(2)?,
        title: r.get(3)?,
        description: r.get(4)?,
        files: from_json(&r.get::<_, String>(5)?),
        file_count: u32_of(r.get(6)?).unwrap_or(0),
        total_size: u64_of(r.get(7)?),
        trackers: from_json(&r.get::<_, String>(8)?),
        category: r.get::<_, String>(9)?.parse().unwrap_or(Category::Other),
        category_path: from_json(&r.get::<_, String>(10)?),
        hashtags: from_json(&r.get::<_, String>(11)?),
        external_ids: from_json(&r.get::<_, String>(12)?),
        source_url: r.get(13)?,
        pow_bits: r.get::<_, i64>(14)?.clamp(0, 255) as u8,
        trust: r.get::<_, i64>(15)?.clamp(0, 100) as u8,
    })
}

impl SqliteStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let conn = Connection::open(path).db()?;
        Self::from_conn(conn)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::from_conn(Connection::open_in_memory().db()?)
    }

    fn from_conn(mut conn: Connection) -> Result<Self> {
        schema::migrate(&mut conn).db()?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn conn(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    // ---------------------------------------------------------------- ingest

    pub fn put_torrent_sync(&self, t: &ParsedTorrent, raw: &RawEvent) -> Result<bool> {
        let mut conn = self.conn();
        let tx = conn.transaction().db()?;
        if known_id(&tx, &raw.id)? {
            return Ok(false);
        }
        let score: Option<i64> = tx
            .query_row(
                "SELECT score FROM trust_cache WHERE pubkey = ?1",
                [&t.pubkey],
                |r| r.get(0),
            )
            .optional()
            .db()?;
        let trust = event_trust(score.map(|s| s.clamp(0, 100) as u8), t.pow_bits);
        insert_event(&tx, raw)?;
        tx.execute(
            "INSERT INTO torrents(event_id, infohash, pubkey, created_at, infohash_v2, title,
                description, files_text, files, file_count, total_size, trackers, category,
                category_path, hashtags, external_ids, source_url, published_at, pow_bits,
                pow_committed, trust)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17,
                ?18, ?19, ?20, ?21)",
            params![
                t.event_id,
                t.infohash,
                t.pubkey,
                t.created_at,
                t.infohash_v2,
                t.title,
                t.description,
                t.files_text(),
                json(&t.files),
                t.files.len() as i64,
                t.total_size.map(i64_of),
                json(&t.trackers),
                t.category.as_str(),
                json(&t.category_path),
                json(&t.hashtags),
                json(&t.external_ids),
                t.source_url,
                t.published_at,
                t.pow_bits,
                t.pow_committed,
                trust,
            ],
        )
        .db()?;
        tx.execute(
            "INSERT INTO torrent_flags(infohash, last_access) VALUES (?1, ?2)
             ON CONFLICT(infohash) DO NOTHING",
            params![t.infohash, magnetar_core::now()],
        )
        .db()?;
        insert_sync_id(&tx, raw)?;
        tx.commit().db()?;
        Ok(true)
    }

    pub fn put_comment_sync(&self, c: &Comment, raw: &RawEvent) -> Result<bool> {
        let mut conn = self.conn();
        let tx = conn.transaction().db()?;
        if known_id(&tx, &raw.id)? {
            return Ok(false);
        }
        insert_event(&tx, raw)?;
        tx.execute(
            "INSERT INTO comments(event_id, pubkey, created_at, kind, torrent_event_id, content)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                c.event_id,
                c.pubkey,
                c.created_at,
                c.kind,
                c.torrent_event_id,
                c.content
            ],
        )
        .db()?;
        insert_sync_id(&tx, raw)?;
        tx.commit().db()?;
        Ok(true)
    }

    pub fn put_replaceable_sync(&self, raw: &RawEvent) -> Result<bool> {
        if !REPLACEABLE_KINDS.contains(&raw.kind) {
            return Err(Error::Invalid(format!(
                "kind {} is not replaceable",
                raw.kind
            )));
        }
        let d = if raw.kind == KIND_KIND_MUTE_SET {
            raw.tag("d")
                .and_then(|t| t.get(1))
                .cloned()
                .unwrap_or_default()
        } else {
            String::new()
        };
        // Derived rows are computed before taking the lock (pure parsing).
        let derived = Derived::of(raw);

        let mut conn = self.conn();
        let tx = conn.transaction().db()?;
        let head: Option<(String, i64)> = tx
            .query_row(
                "SELECT event_id, created_at FROM replaceable WHERE pubkey = ?1 AND kind = ?2 AND d = ?3",
                params![raw.pubkey, raw.kind, d],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .db()?;
        if let Some((id, at)) = &head {
            // Newest wins; on a created_at tie the lowest id wins.
            if *at > raw.created_at || (*at == raw.created_at && *id <= raw.id) {
                return Ok(false);
            }
            tx.execute("DELETE FROM events WHERE id = ?1", [id]).db()?;
        }
        insert_event(&tx, raw)?;
        tx.execute(
            "INSERT INTO replaceable(pubkey, kind, d, event_id, created_at) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(pubkey, kind, d) DO UPDATE SET event_id = excluded.event_id,
                created_at = excluded.created_at",
            params![raw.pubkey, raw.kind, d, raw.id, raw.created_at],
        )
        .db()?;
        let graph_member = matches!(
            raw.kind,
            KIND_CONTACTS | KIND_MUTE_LIST | KIND_KIND_MUTE_SET
        ) && is_root_or_d1(&tx, &raw.pubkey)?;
        derived.apply(&tx, raw)?;
        if graph_member {
            bump_graph(&tx)?;
        }
        tx.commit().db()?;
        Ok(true)
    }

    pub fn put_report_sync(&self, raw: &RawEvent) -> Result<bool> {
        let report = wot::report_of(raw);
        let mut conn = self.conn();
        let tx = conn.transaction().db()?;
        if known_id(&tx, &raw.id)? {
            return Ok(false);
        }
        insert_event(&tx, raw)?;
        if let Some((target, kind)) = report {
            tx.execute(
                "INSERT OR IGNORE INTO reports(event_id, reporter, target, report_type, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![raw.id, raw.pubkey, target, kind, raw.created_at],
            )
            .db()?;
        }
        tx.commit().db()?;
        Ok(true)
    }

    /// Known locally, including torrent/comment bodies evicted by `cleanup` (kept in sync_ids).
    pub fn has_event_sync(&self, id: &str) -> Result<bool> {
        known_id(&self.conn(), id)
    }

    pub fn known_torrent_event_sync(&self, id: &str) -> Result<bool> {
        self.conn()
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM torrents WHERE event_id = ?1)
                     OR EXISTS(SELECT 1 FROM sync_ids WHERE id = ?1 AND kind = 2003)",
                [id],
                |r| r.get(0),
            )
            .db()
    }

    pub fn get_raw_events_sync(&self, ids: &[String]) -> Result<Vec<RawEvent>> {
        let conn = self.conn();
        let mut st = conn
            .prepare_cached("SELECT json FROM events WHERE id = ?1")
            .db()?;
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(js) = st
                .query_row([id], |r| r.get::<_, String>(0))
                .optional()
                .db()?
                && let Ok(ev) = RawEvent::from_json(&js)
            {
                out.push(ev);
            }
        }
        Ok(out)
    }

    // ---------------------------------------------------------------- relays + cursors

    pub fn upsert_relay_sync(&self, url: &str, source: &str) -> Result<()> {
        self.conn()
            .execute(
                "INSERT INTO relays(url, source) VALUES (?1, ?2) ON CONFLICT(url) DO NOTHING",
                params![url, source],
            )
            .db()?;
        Ok(())
    }

    pub fn list_relays_sync(&self) -> Result<Vec<RelayInfo>> {
        let conn = self.conn();
        let mut st = conn
            .prepare_cached(
                "SELECT url, source, enabled, last_ok, last_err, consecutive_failures, next_try_at,
                    events_found, supports_neg
                 FROM relays ORDER BY enabled DESC, events_found DESC, url",
            )
            .db()?;
        st.query_map([], |r| {
            Ok(RelayInfo {
                url: r.get(0)?,
                source: r.get(1)?,
                enabled: r.get(2)?,
                last_ok: r.get(3)?,
                last_err: r.get(4)?,
                consecutive_failures: u32_of(r.get(5)?).unwrap_or(0),
                next_try_at: r.get(6)?,
                events_found: u64_of(r.get(7)?).unwrap_or(0),
                supports_neg: r.get(8)?,
            })
        })
        .db()?
        .collect::<rusqlite::Result<_>>()
        .db()
    }

    pub fn record_relay_result_sync(
        &self,
        url: &str,
        ok: bool,
        err: Option<&str>,
        found: u64,
        now: i64,
    ) -> Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction().db()?;
        tx.execute(
            "INSERT INTO relays(url, source) VALUES (?1, 'hint') ON CONFLICT(url) DO NOTHING",
            [url],
        )
        .db()?;
        if ok {
            tx.execute(
                "UPDATE relays SET consecutive_failures = 0, last_ok = ?2, last_err = NULL,
                    next_try_at = ?2, events_found = events_found + ?3 WHERE url = ?1",
                params![url, now, i64_of(found)],
            )
            .db()?;
        } else {
            let failures: i64 = tx
                .query_row(
                    "SELECT consecutive_failures FROM relays WHERE url = ?1",
                    [url],
                    |r| r.get(0),
                )
                .db()?;
            let failures = failures.max(0) as u32 + 1;
            tx.execute(
                "UPDATE relays SET consecutive_failures = ?2, last_err = ?3, next_try_at = ?4,
                    events_found = events_found + ?5 WHERE url = ?1",
                params![
                    url,
                    failures,
                    err,
                    now + backoff_secs(failures),
                    i64_of(found)
                ],
            )
            .db()?;
        }
        tx.commit().db()
    }

    pub fn set_relay_enabled_sync(&self, url: &str, enabled: bool) -> Result<()> {
        self.conn()
            .execute(
                "INSERT INTO relays(url, source, enabled) VALUES (?1, 'manual', ?2)
                 ON CONFLICT(url) DO UPDATE SET enabled = excluded.enabled",
                params![url, enabled],
            )
            .db()?;
        Ok(())
    }

    pub fn set_relay_supports_neg_sync(&self, url: &str, supports: bool) -> Result<()> {
        self.conn()
            .execute(
                "UPDATE relays SET supports_neg = ?2 WHERE url = ?1",
                params![url, supports],
            )
            .db()?;
        Ok(())
    }

    pub fn get_cursor_sync(&self, relay: &str, kind: u16) -> Result<Option<Cursor>> {
        self.conn()
            .query_row(
                "SELECT newest_seen, backfill_until, backfill_done FROM cursors
                 WHERE relay = ?1 AND kind = ?2",
                params![relay, kind],
                |r| {
                    Ok(Cursor {
                        relay: relay.to_string(),
                        kind,
                        newest_seen: r.get(0)?,
                        backfill_until: r.get(1)?,
                        backfill_done: r.get(2)?,
                    })
                },
            )
            .optional()
            .db()
    }

    pub fn put_cursor_sync(&self, c: &Cursor) -> Result<()> {
        self.conn()
            .execute(
                "INSERT INTO cursors(relay, kind, newest_seen, backfill_until, backfill_done)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(relay, kind) DO UPDATE SET newest_seen = excluded.newest_seen,
                    backfill_until = excluded.backfill_until, backfill_done = excluded.backfill_done",
                params![
                    c.relay,
                    c.kind,
                    c.newest_seen,
                    c.backfill_until,
                    c.backfill_done
                ],
            )
            .db()?;
        Ok(())
    }

    pub fn publishers_sync(&self, limit: usize) -> Result<Vec<String>> {
        let conn = self.conn();
        let mut st = conn
            .prepare_cached(
                "SELECT pubkey FROM torrents GROUP BY pubkey ORDER BY COUNT(*) DESC, pubkey LIMIT ?1",
            )
            .db()?;
        st.query_map([limit as i64], |r| r.get(0))
            .db()?
            .collect::<rusqlite::Result<_>>()
            .db()
    }

    pub fn relay_lists_for_sync(
        &self,
        pubkeys: &[String],
    ) -> Result<Vec<(String, String, bool, bool)>> {
        let conn = self.conn();
        let mut st = conn
            .prepare_cached(
                "SELECT r.pubkey, r.url, r.read, r.write FROM relay_lists r
                 LEFT JOIN relay_list_meta m ON m.pubkey = r.pubkey
                 WHERE r.pubkey = ?1 AND COALESCE(m.oversized, 0) = 0 ORDER BY r.url",
            )
            .db()?;
        let mut out = Vec::new();
        for pk in pubkeys {
            let rows = st
                .query_map([pk], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
                .db()?;
            for row in rows {
                out.push(row.db()?);
            }
        }
        Ok(out)
    }

    pub fn relay_list_oversized_sync(&self, pubkey: &str) -> Result<bool> {
        Ok(self
            .conn()
            .query_row(
                "SELECT oversized FROM relay_list_meta WHERE pubkey = ?1",
                [pubkey],
                |r| r.get::<_, bool>(0),
            )
            .optional()
            .db()?
            .unwrap_or(false))
    }

    // ---------------------------------------------------------------- search / detail

    fn threshold(conn: &Connection) -> Result<u8> {
        let v: Option<String> = conn
            .query_row(
                "SELECT value FROM settings WHERE key = ?1",
                [keys::TRUST_THRESHOLD],
                |r| r.get(0),
            )
            .optional()
            .db()?;
        Ok(v.and_then(|s| serde_json::from_str::<u8>(s.trim()).ok())
            .unwrap_or(DEFAULT_TRUST_THRESHOLD))
    }

    pub fn search_sync(&self, q: &SearchQuery, now: i64) -> Result<SearchPage> {
        let conn = self.conn();
        let min_trust = match q.min_trust {
            Some(t) => t,
            None => Self::threshold(&conn)?,
        };
        let matcher = fts5_match(&q.text);
        let mut args: Vec<Value> = vec![Value::Integer(min_trust as i64)];
        let eligible = format!("t.trust >= ?1 AND t.pubkey NOT IN ({BLOCKED_SQL})");

        let mut sql = String::new();
        let rank_expr;
        if let Some(m) = &matcher {
            args.push(Value::Text(m.clone()));
            sql.push_str(&format!(
                "WITH hits AS MATERIALIZED (
                    SELECT rowid AS rid, bm25(torrents_fts, 10.0, 3.0, 1.0) AS score
                    FROM torrents_fts WHERE torrents_fts MATCH ?2
                 ),
                 matched AS (
                    SELECT t.infohash, MIN(h.score) AS score FROM hits h
                    JOIN torrents t ON t.rowid = h.rid
                    WHERE {eligible} GROUP BY t.infohash
                 ),
                 canon AS (
                    SELECT t.*, ROW_NUMBER() OVER (PARTITION BY t.infohash
                        ORDER BY t.trust DESC, t.created_at ASC, t.event_id ASC) AS rn
                    FROM torrents t WHERE {eligible}
                        AND t.infohash IN (SELECT infohash FROM matched)
                 ) "
            ));
            rank_expr = "-m.score";
        } else {
            sql.push_str(&format!(
                "WITH canon AS (
                    SELECT t.*, ROW_NUMBER() OVER (PARTITION BY t.infohash
                        ORDER BY t.trust DESC, t.created_at ASC, t.event_id ASC) AS rn
                    FROM torrents t WHERE {eligible}
                 ) "
            ));
            rank_expr = "0.0";
        }
        let best = BEST_PEERS_SQL;
        sql.push_str(&format!(
            "SELECT c.infohash, c.event_id, c.title, c.category, c.total_size, c.file_count,
                c.created_at, c.pubkey, c.trust, c.pow_bits, c.source_url, {rank_expr} AS rank,
                COALESCE(f.pinned, 0), COALESCE(f.hidden, 0),
                (SELECT COUNT(DISTINCT t.pubkey) FROM torrents t
                    WHERE t.infohash = c.infohash AND {eligible}) AS pubs,
                {LIVENESS_COLS}, {best} AS best
             FROM canon c "
        ));
        if matcher.is_some() {
            sql.push_str("JOIN matched m ON m.infohash = c.infohash ");
        }
        sql.push_str(
            "LEFT JOIN liveness l ON l.infohash = c.infohash
             LEFT JOIN torrent_flags f ON f.infohash = c.infohash
             WHERE c.rn = 1",
        );

        fn push(sql: &mut String, args: &mut Vec<Value>, cond: &str, v: Value) {
            args.push(v);
            sql.push_str(&cond.replace('?', &format!("?{}", args.len())));
        }
        if !q.categories.is_empty() {
            let mut list = Vec::new();
            for c in &q.categories {
                args.push(Value::Text(c.as_str().into()));
                list.push(format!("?{}", args.len()));
            }
            sql.push_str(&format!(" AND c.category IN ({})", list.join(", ")));
        }
        if let Some(v) = q.min_size {
            push(
                &mut sql,
                &mut args,
                " AND c.total_size >= ?",
                Value::Integer(i64_of(v)),
            );
        }
        if let Some(v) = q.max_size {
            push(
                &mut sql,
                &mut args,
                " AND c.total_size <= ?",
                Value::Integer(i64_of(v)),
            );
        }
        if let Some(v) = q.since {
            push(
                &mut sql,
                &mut args,
                " AND c.created_at >= ?",
                Value::Integer(v),
            );
        }
        if let Some(v) = q.until {
            push(
                &mut sql,
                &mut args,
                " AND c.created_at <= ?",
                Value::Integer(v),
            );
        }
        if let Some(v) = q.min_seeders {
            push(
                &mut sql,
                &mut args,
                &format!(" AND COALESCE({best}, 0) >= ?"),
                Value::Integer(v as i64),
            );
        }
        if q.hide_dead {
            push(
                &mut sql,
                &mut args,
                &format!(" AND NOT (COALESCE(l.expires_at, 0) > ? AND COALESCE({best}, -1) = 0)"),
                Value::Integer(now),
            );
        }
        if !q.include_hidden {
            sql.push_str(" AND COALESCE(f.hidden, 0) = 0");
        }

        let total: i64 = conn
            .query_row(
                &format!("SELECT COUNT(*) FROM ({sql})"),
                params_from_iter(args.iter()),
                |r| r.get(0),
            )
            .db()?;

        let dir = if q.descending { "DESC" } else { "ASC" };
        let order = match q.effective_sort() {
            Sort::Relevance => format!("rank {dir}, c.created_at DESC"),
            Sort::Date => format!("c.created_at {dir}"),
            Sort::Size => format!("c.total_size IS NULL, c.total_size {dir}, c.created_at DESC"),
            Sort::Seeders => format!("best IS NULL, best {dir}, c.created_at DESC"),
        };
        sql.push_str(&format!(" ORDER BY {order}, c.infohash"));
        args.push(Value::Integer(q.effective_limit() as i64));
        sql.push_str(&format!(" LIMIT ?{}", args.len()));
        args.push(Value::Integer(q.offset as i64));
        sql.push_str(&format!(" OFFSET ?{}", args.len()));

        let mut st = conn.prepare(&sql).db()?;
        let hits = st
            .query_map(params_from_iter(args.iter()), |r| {
                let liveness = liveness_from_row(r, 15)?;
                let pubkey: String = r.get(7)?;
                Ok(TorrentSummary {
                    infohash: r.get(0)?,
                    event_id: r.get(1)?,
                    title: r.get(2)?,
                    category: r.get::<_, String>(3)?.parse().unwrap_or(Category::Other),
                    total_size: u64_of(r.get(4)?),
                    file_count: u32_of(r.get(5)?).unwrap_or(0),
                    created_at: r.get(6)?,
                    npub: npub(&pubkey),
                    pubkey,
                    seeders: liveness.as_ref().and_then(|l| l.seeders),
                    leechers: liveness.as_ref().and_then(|l| l.leechers),
                    dead: dead_of(liveness.as_ref(), now),
                    trust: r.get::<_, i64>(8)?.clamp(0, 100) as u8,
                    pow_bits: r.get::<_, i64>(9)?.clamp(0, 255) as u8,
                    publisher_count: u32_of(r.get(14)?).unwrap_or(0),
                    source_url: r.get(10)?,
                    rank: r.get::<_, f64>(11)? as f32,
                    pinned: r.get(12)?,
                    hidden: r.get(13)?,
                })
            })
            .db()?
            .collect::<rusqlite::Result<Vec<_>>>()
            .db()?;
        Ok(SearchPage {
            hits,
            total: total.max(0) as u64,
            source: "local".into(),
        })
    }

    /// Grouped detail for one infohash; also bumps its LRU `last_access`.
    pub fn torrent_detail_sync(&self, infohash: &str, now: i64) -> Result<Option<TorrentDetail>> {
        let conn = self.conn();
        let blocked: HashSet<String> = {
            let mut st = conn.prepare_cached(BLOCKED_SQL).db()?;
            st.query_map([], |r| r.get(0))
                .db()?
                .collect::<rusqlite::Result<_>>()
                .db()?
        };
        let mut rows: Vec<TorrentRow> = {
            let mut st = conn
                .prepare_cached(&format!(
                    "SELECT {TORRENT_COLS} FROM torrents WHERE infohash = ?1"
                ))
                .db()?;
            st.query_map([infohash], torrent_from_row)
                .db()?
                .collect::<rusqlite::Result<_>>()
                .db()?
        };
        if rows.is_empty() {
            return Ok(None);
        }
        // Canonical first: highest trust, then earliest (D6). Blocked publishers sort last.
        rows.sort_by(|a, b| {
            blocked
                .contains(&a.pubkey)
                .cmp(&blocked.contains(&b.pubkey))
                .then(b.trust.cmp(&a.trust))
                .then(a.created_at.cmp(&b.created_at))
                .then(a.event_id.cmp(&b.event_id))
        });
        let liveness = conn
            .query_row(
                &format!("SELECT {LIVENESS_COLS} FROM liveness l WHERE l.infohash = ?1"),
                [infohash],
                |r| liveness_from_row(r, 0),
            )
            .optional()
            .db()?
            .flatten();
        let (pinned, hidden): (bool, bool) = conn
            .query_row(
                "SELECT pinned, hidden FROM torrent_flags WHERE infohash = ?1",
                [infohash],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .db()?
            .unwrap_or((false, false));
        let canon = &rows[0];
        let trust = conn
            .query_row(
                "SELECT score, factors, root, computed_at, graph_version FROM trust_cache
                 WHERE pubkey = ?1",
                [&canon.pubkey],
                |r| trust_from_row(&canon.pubkey, r),
            )
            .optional()
            .db()?
            .unwrap_or_else(|| TrustScore {
                pubkey: canon.pubkey.clone(),
                score: 0,
                factors: Vec::new(),
                root: None,
                computed_at: 0,
                graph_version: 0,
            });
        let comments = {
            let mut st = conn
                .prepare_cached(
                    "SELECT c.event_id, c.pubkey, c.created_at, c.kind, c.torrent_event_id, c.content
                     FROM comments c JOIN torrents t ON t.event_id = c.torrent_event_id
                     WHERE t.infohash = ?1 ORDER BY c.created_at, c.event_id",
                )
                .db()?;
            st.query_map([infohash], |r| {
                Ok(Comment {
                    event_id: r.get(0)?,
                    pubkey: r.get(1)?,
                    created_at: r.get(2)?,
                    kind: r.get(3)?,
                    torrent_event_id: r.get(4)?,
                    content: r.get(5)?,
                })
            })
            .db()?
            .collect::<rusqlite::Result<Vec<_>>>()
            .db()?
        };
        conn.execute(
            "INSERT INTO torrent_flags(infohash, last_access) VALUES (?1, ?2)
             ON CONFLICT(infohash) DO UPDATE SET last_access = excluded.last_access",
            params![infohash, now],
        )
        .db()?;
        drop(conn);

        let publishers: HashSet<&str> = rows
            .iter()
            .filter(|r| !blocked.contains(&r.pubkey))
            .map(|r| r.pubkey.as_str())
            .collect();
        let mut trackers: Vec<String> = Vec::new();
        for r in &rows {
            for t in &r.trackers {
                if !trackers.contains(t) {
                    trackers.push(t.clone());
                }
            }
        }
        let magnet =
            magnet::build_magnet(infohash, Some(&canon.title), &trackers, canon.total_size);
        let summary = TorrentSummary {
            infohash: infohash.to_string(),
            event_id: canon.event_id.clone(),
            title: canon.title.clone(),
            category: canon.category,
            total_size: canon.total_size,
            file_count: canon.file_count,
            created_at: canon.created_at,
            pubkey: canon.pubkey.clone(),
            npub: npub(&canon.pubkey),
            seeders: liveness.as_ref().and_then(|l| l.seeders),
            leechers: liveness.as_ref().and_then(|l| l.leechers),
            dead: dead_of(liveness.as_ref(), now),
            trust: canon.trust,
            pow_bits: canon.pow_bits,
            publisher_count: publishers.len() as u32,
            source_url: canon.source_url.clone(),
            rank: 0.0,
            pinned,
            hidden,
        };
        let events = rows
            .iter()
            .map(|r| EventRef {
                event_id: r.event_id.clone(),
                pubkey: r.pubkey.clone(),
                npub: npub(&r.pubkey),
                created_at: r.created_at,
                title: r.title.clone(),
                trust: r.trust,
            })
            .collect();
        let canon = rows.swap_remove(0);
        Ok(Some(TorrentDetail {
            summary,
            description: canon.description,
            files: canon.files,
            trackers,
            hashtags: canon.hashtags,
            category_path: canon.category_path,
            external_ids: canon.external_ids,
            magnet,
            liveness,
            trust,
            events,
            comments,
        }))
    }

    // ---------------------------------------------------------------- liveness

    pub fn liveness_due_sync(&self, now: i64, limit: usize) -> Result<Vec<(String, Vec<String>)>> {
        let conn = self.conn();
        let hashes: Vec<String> = {
            let mut st = conn
                .prepare_cached(
                    "SELECT t.infohash FROM torrents t
                     LEFT JOIN liveness l ON l.infohash = t.infohash
                     WHERE l.infohash IS NULL OR l.expires_at <= ?1
                     GROUP BY t.infohash ORDER BY MAX(t.created_at) DESC, t.infohash LIMIT ?2",
                )
                .db()?;
            st.query_map(params![now, limit as i64], |r| r.get(0))
                .db()?
                .collect::<rusqlite::Result<_>>()
                .db()?
        };
        let mut st = conn
            .prepare_cached("SELECT trackers FROM torrents WHERE infohash = ?1 ORDER BY created_at")
            .db()?;
        let mut out = Vec::with_capacity(hashes.len());
        for ih in hashes {
            let mut trackers: Vec<String> = Vec::new();
            let lists = st.query_map([&ih], |r| r.get::<_, String>(0)).db()?;
            for list in lists {
                for t in from_json::<Vec<String>>(&list.db()?) {
                    if !trackers.contains(&t) {
                        trackers.push(t);
                    }
                }
            }
            out.push((ih, trackers));
        }
        Ok(out)
    }

    pub fn put_liveness_sync(&self, l: &Liveness) -> Result<()> {
        self.conn()
            .execute(
                "INSERT INTO liveness(infohash, seeders, leechers, completed, dht_peers_est, source,
                    trackers_ok, trackers_failed, checked_at, expires_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                 ON CONFLICT(infohash) DO UPDATE SET seeders = excluded.seeders,
                    leechers = excluded.leechers, completed = excluded.completed,
                    dht_peers_est = excluded.dht_peers_est, source = excluded.source,
                    trackers_ok = excluded.trackers_ok, trackers_failed = excluded.trackers_failed,
                    checked_at = excluded.checked_at, expires_at = excluded.expires_at",
                params![
                    l.infohash,
                    l.seeders,
                    l.leechers,
                    l.completed,
                    l.dht_peers_est,
                    liveness_source_str(l.source),
                    l.trackers_ok,
                    l.trackers_failed,
                    l.checked_at,
                    l.expires_at
                ],
            )
            .db()?;
        Ok(())
    }

    pub fn get_liveness_sync(&self, infohash: &str) -> Result<Option<Liveness>> {
        Ok(self
            .conn()
            .query_row(
                &format!("SELECT {LIVENESS_COLS} FROM liveness l WHERE l.infohash = ?1"),
                [infohash],
                |r| liveness_from_row(r, 0),
            )
            .optional()
            .db()?
            .flatten())
    }

    /// Trackers of every stored event for this infohash (deduped, order kept).
    pub fn trackers_of(&self, infohash: &str) -> Result<Vec<String>> {
        let conn = self.conn();
        let mut st = conn
            .prepare_cached("SELECT trackers FROM torrents WHERE infohash = ?1 ORDER BY created_at")
            .db()?;
        let mut out: Vec<String> = Vec::new();
        for list in st.query_map([infohash], |r| r.get::<_, String>(0)).db()? {
            for t in from_json::<Vec<String>>(&list.db()?) {
                if !out.contains(&t) {
                    out.push(t);
                }
            }
        }
        Ok(out)
    }

    // ---------------------------------------------------------------- trust graph

    pub fn follows_of_sync(&self, pubkey: &str) -> Result<Vec<String>> {
        let conn = self.conn();
        let mut st = conn
            .prepare_cached("SELECT target FROM follows WHERE follower = ?1 ORDER BY target")
            .db()?;
        st.query_map([pubkey], |r| r.get(0))
            .db()?
            .collect::<rusqlite::Result<_>>()
            .db()
    }

    pub fn followers_among_sync(
        &self,
        targets: &[String],
        among: &[String],
    ) -> Result<Vec<(String, Vec<String>)>> {
        if targets.is_empty() || among.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.conn();
        let among: HashSet<&str> = among.iter().map(String::as_str).collect();
        let mut st = conn
            .prepare_cached("SELECT follower FROM follows WHERE target = ?1 ORDER BY follower")
            .db()?;
        let mut out = Vec::new();
        for t in targets {
            let followers: Vec<String> = st
                .query_map([t], |r| r.get::<_, String>(0))
                .db()?
                .filter_map(|r| r.ok())
                .filter(|f| among.contains(f.as_str()))
                .collect();
            if !followers.is_empty() {
                out.push((t.clone(), followers));
            }
        }
        Ok(out)
    }

    pub fn mutes_by_sync(&self, muters: &[String]) -> Result<Vec<(String, String)>> {
        let conn = self.conn();
        let mut st = conn
            .prepare_cached("SELECT DISTINCT target FROM mutes WHERE muter = ?1 ORDER BY target")
            .db()?;
        let mut out = Vec::new();
        for m in muters {
            for t in st.query_map([m], |r| r.get::<_, String>(0)).db()? {
                out.push((m.clone(), t.db()?));
            }
        }
        Ok(out)
    }

    pub fn reports_by_sync(&self, reporters: &[String]) -> Result<Vec<(String, String, String)>> {
        let conn = self.conn();
        let mut st = conn
            .prepare_cached(
                "SELECT target, report_type FROM reports WHERE reporter = ?1 ORDER BY created_at",
            )
            .db()?;
        let mut out = Vec::new();
        for rep in reporters {
            for row in st
                .query_map([rep], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
                })
                .db()?
            {
                let (target, kind) = row.db()?;
                out.push((rep.clone(), target, kind));
            }
        }
        Ok(out)
    }

    pub fn comment_authors_on_sync(
        &self,
        publisher: &str,
        authors: &[String],
    ) -> Result<Vec<String>> {
        let conn = self.conn();
        let mut st = conn
            .prepare_cached(
                "SELECT DISTINCT c.pubkey FROM comments c
                 JOIN torrents t ON t.event_id = c.torrent_event_id
                 WHERE t.pubkey = ?1 ORDER BY c.pubkey",
            )
            .db()?;
        let wanted: HashSet<&str> = authors.iter().map(String::as_str).collect();
        Ok(st
            .query_map([publisher], |r| r.get::<_, String>(0))
            .db()?
            .filter_map(|r| r.ok())
            .filter(|a| wanted.contains(a.as_str()))
            .collect())
    }

    pub fn publisher_stats_sync(&self, pubkey: &str) -> Result<PublisherStats> {
        let conn = self.conn();
        let (first_seen, count, bridge): (Option<i64>, i64, bool) = conn
            .query_row(
                "SELECT MIN(created_at), COUNT(*), COALESCE(MAX(source_url IS NOT NULL), 0)
                 FROM torrents WHERE pubkey = ?1",
                [pubkey],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .db()?;
        let max_per_hour: i64 = conn
            .query_row(
                "SELECT COALESCE(MAX(n), 0) FROM (
                    SELECT COUNT(*) AS n FROM torrents a JOIN torrents b
                        ON b.pubkey = a.pubkey AND b.created_at >= a.created_at
                           AND b.created_at < a.created_at + 3600
                    WHERE a.pubkey = ?1 GROUP BY a.rowid)",
                [pubkey],
                |r| r.get(0),
            )
            .db()?;
        let head = |kind: u16| -> Result<bool> {
            conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM replaceable WHERE pubkey = ?1 AND kind = ?2)",
                params![pubkey, kind],
                |r| r.get(0),
            )
            .db()
        };
        Ok(PublisherStats {
            first_seen,
            torrent_count: count.max(0) as u64,
            has_profile: head(KIND_PROFILE)?,
            has_relay_list: head(KIND_RELAY_LIST)?,
            max_per_hour: max_per_hour.clamp(0, u32::MAX as i64) as u32,
            is_bridge: bridge,
            wot_comments: 0,
        })
    }

    pub fn put_trust_sync(&self, s: &TrustScore) -> Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction().db()?;
        tx.execute(
            "INSERT INTO trust_cache(pubkey, score, factors, root, computed_at, graph_version)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(pubkey) DO UPDATE SET score = excluded.score, factors = excluded.factors,
                root = excluded.root, computed_at = excluded.computed_at,
                graph_version = excluded.graph_version",
            params![
                s.pubkey,
                s.score,
                json(&s.factors),
                s.root,
                s.computed_at,
                s.graph_version
            ],
        )
        .db()?;
        tx.execute(
            &format!("UPDATE torrents SET trust = {EVENT_TRUST_SQL} WHERE pubkey = ?2"),
            params![s.score.min(100), s.pubkey],
        )
        .db()?;
        tx.commit().db()
    }

    pub fn get_trust_sync(&self, pubkey: &str) -> Result<Option<TrustScore>> {
        self.conn()
            .query_row(
                "SELECT score, factors, root, computed_at, graph_version FROM trust_cache
                 WHERE pubkey = ?1",
                [pubkey],
                |r| trust_from_row(pubkey, r),
            )
            .optional()
            .db()
    }

    pub fn graph_version_sync(&self) -> Result<i64> {
        self.conn()
            .query_row(
                "SELECT value FROM meta WHERE key = 'graph_version'",
                [],
                |r| r.get(0),
            )
            .db()
    }

    pub fn bump_graph_version_sync(&self) -> Result<i64> {
        bump_graph(&self.conn())
    }

    pub fn set_override_sync(&self, pubkey: &str, action: Option<OverrideAction>) -> Result<()> {
        let conn = self.conn();
        match action {
            Some(a) => conn.execute(
                "INSERT INTO overrides(pubkey, action) VALUES (?1, ?2)
                 ON CONFLICT(pubkey) DO UPDATE SET action = excluded.action",
                params![pubkey, action_str(a)],
            ),
            None => conn.execute("DELETE FROM overrides WHERE pubkey = ?1", [pubkey]),
        }
        .db()?;
        Ok(())
    }

    pub fn overrides_sync(&self) -> Result<Vec<(String, OverrideAction)>> {
        let conn = self.conn();
        let mut st = conn
            .prepare_cached("SELECT pubkey, action FROM overrides ORDER BY pubkey")
            .db()?;
        st.query_map([], |r| {
            let action: String = r.get(1)?;
            Ok((
                r.get(0)?,
                if action == "trust" {
                    OverrideAction::Trust
                } else {
                    OverrideAction::Block
                },
            ))
        })
        .db()?
        .collect::<rusqlite::Result<_>>()
        .db()
    }

    // ---------------------------------------------------------------- settings

    pub fn get_setting_sync(&self, key: &str) -> Result<Option<String>> {
        self.conn()
            .query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| {
                r.get(0)
            })
            .optional()
            .db()
    }

    /// Changing `trust.root` bumps the graph version.
    pub fn set_setting_sync(&self, key: &str, value: &str) -> Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction().db()?;
        let old: Option<String> = tx
            .query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| {
                r.get(0)
            })
            .optional()
            .db()?;
        if old.as_deref() == Some(value) {
            return Ok(());
        }
        tx.execute(
            "INSERT INTO settings(key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )
        .db()?;
        if key == keys::TRUST_ROOT {
            bump_graph(&tx)?;
        }
        tx.commit().db()
    }

    // ---------------------------------------------------------------- saved searches

    pub fn list_saved_searches_sync(&self) -> Result<Vec<SavedSearch>> {
        let conn = self.conn();
        let mut st = conn
            .prepare_cached(
                "SELECT id, name, query, notify, last_checked, last_seen_created_at
                 FROM saved_searches ORDER BY id",
            )
            .db()?;
        st.query_map([], |r| {
            Ok(SavedSearch {
                id: r.get(0)?,
                name: r.get(1)?,
                query: from_json(&r.get::<_, String>(2)?),
                notify: r.get(3)?,
                last_checked: r.get(4)?,
                last_seen_created_at: r.get(5)?,
            })
        })
        .db()?
        .collect::<rusqlite::Result<_>>()
        .db()
    }

    pub fn get_saved_search(&self, id: i64) -> Result<Option<SavedSearch>> {
        Ok(self
            .list_saved_searches_sync()?
            .into_iter()
            .find(|s| s.id == id))
    }

    pub fn put_saved_search_sync(&self, s: &SavedSearch) -> Result<i64> {
        let conn = self.conn();
        let q = json(&s.query);
        if s.id == 0 {
            conn.execute(
                "INSERT INTO saved_searches(name, query, notify, last_checked, last_seen_created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![s.name, q, s.notify, s.last_checked, s.last_seen_created_at],
            )
            .db()?;
            Ok(conn.last_insert_rowid())
        } else {
            conn.execute(
                "INSERT INTO saved_searches(id, name, query, notify, last_checked, last_seen_created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(id) DO UPDATE SET name = excluded.name, query = excluded.query,
                    notify = excluded.notify, last_checked = excluded.last_checked,
                    last_seen_created_at = excluded.last_seen_created_at",
                params![
                    s.id,
                    s.name,
                    q,
                    s.notify,
                    s.last_checked,
                    s.last_seen_created_at
                ],
            )
            .db()?;
            Ok(s.id)
        }
    }

    pub fn delete_saved_search_sync(&self, id: i64) -> Result<()> {
        self.conn()
            .execute("DELETE FROM saved_searches WHERE id = ?1", [id])
            .db()?;
        Ok(())
    }

    // ---------------------------------------------------------------- stats

    pub fn db_bytes(&self) -> Result<u64> {
        db_bytes(&self.conn())
    }

    pub fn stats_sync(&self) -> Result<StoreStats> {
        let conn = self.conn();
        let count = |sql: &str| -> Result<u64> {
            conn.query_row(sql, [], |r| r.get::<_, i64>(0))
                .db()
                .map(|n| n.max(0) as u64)
        };
        Ok(StoreStats {
            torrents: count("SELECT COUNT(DISTINCT infohash) FROM torrents")?,
            events: count("SELECT COUNT(*) FROM events")?,
            publishers: count("SELECT COUNT(DISTINCT pubkey) FROM torrents")?,
            comments: count("SELECT COUNT(*) FROM comments")?,
            relays: count("SELECT COUNT(*) FROM relays")?,
            unsynced: count("SELECT COUNT(*) FROM sync_ids WHERE synced = 0")?,
            db_bytes: db_bytes(&conn)?,
        })
    }

    // ---------------------------------------------------------------- phone extras

    pub fn set_pinned(&self, infohash: &str, pinned: bool) -> Result<()> {
        self.set_flag(infohash, "pinned", pinned)
    }

    pub fn set_hidden(&self, infohash: &str, hidden: bool) -> Result<()> {
        self.set_flag(infohash, "hidden", hidden)
    }

    fn set_flag(&self, infohash: &str, col: &str, on: bool) -> Result<()> {
        self.conn()
            .execute(
                &format!(
                    "INSERT INTO torrent_flags(infohash, {col}, last_access) VALUES (?1, ?2, ?3)
                     ON CONFLICT(infohash) DO UPDATE SET {col} = excluded.{col}"
                ),
                params![infohash, on, magnetar_core::now()],
            )
            .db()?;
        Ok(())
    }

    /// (pinned, hidden) for these infohashes (missing = both false).
    pub fn flags(&self, infohashes: &[String]) -> Result<HashMap<String, (bool, bool)>> {
        let conn = self.conn();
        let mut st = conn
            .prepare_cached("SELECT pinned, hidden FROM torrent_flags WHERE infohash = ?1")
            .db()?;
        let mut out = HashMap::new();
        for ih in infohashes {
            if let Some(f) = st
                .query_row([ih], |r| Ok((r.get(0)?, r.get(1)?)))
                .optional()
                .db()?
            {
                out.insert(ih.clone(), f);
            }
        }
        Ok(out)
    }

    /// LRU touch.
    pub fn touch(&self, infohash: &str, now: i64) -> Result<()> {
        self.conn()
            .execute(
                "UPDATE torrent_flags SET last_access = ?2 WHERE infohash = ?1",
                params![infohash, now],
            )
            .db()?;
        Ok(())
    }

    pub fn unsynced_sync(&self, limit: usize) -> Result<Vec<RawEvent>> {
        let conn = self.conn();
        let mut st = conn
            .prepare_cached(
                "SELECT e.json FROM sync_ids s JOIN events e ON e.id = s.id
                 WHERE s.synced = 0 ORDER BY s.failures, s.created_at, s.id LIMIT ?1",
            )
            .db()?;
        Ok(st
            .query_map([limit as i64], |r| r.get::<_, String>(0))
            .db()?
            .filter_map(|r| r.ok())
            .filter_map(|js| RawEvent::from_json(&js).ok())
            .collect())
    }

    pub fn unsynced_count(&self) -> Result<u64> {
        self.conn()
            .query_row("SELECT COUNT(*) FROM sync_ids WHERE synced = 0", [], |r| {
                r.get::<_, i64>(0)
            })
            .db()
            .map(|n| n.max(0) as u64)
    }

    pub fn mark_synced_sync(&self, ids: &[String]) -> Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction().db()?;
        {
            let mut st = tx
                .prepare_cached("UPDATE sync_ids SET synced = 1, failure = NULL WHERE id = ?1")
                .db()?;
            for id in ids {
                st.execute([id]).db()?;
            }
        }
        tx.commit().db()
    }

    pub fn record_failure_sync(&self, id: &str, reason: &str) -> Result<()> {
        self.conn()
            .execute(
                "UPDATE sync_ids SET failures = failures + 1, failure = ?2 WHERE id = ?1",
                params![id, reason],
            )
            .db()?;
        Ok(())
    }

    pub fn neg_items_sync(&self) -> Result<Vec<(u64, [u8; 32])>> {
        let conn = self.conn();
        let mut st = conn
            .prepare_cached("SELECT created_at, id FROM sync_ids ORDER BY created_at, id")
            .db()?;
        let rows = st
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))
            .db()?;
        let mut out = Vec::new();
        for row in rows {
            let (at, id) = row.db()?;
            let mut bytes = [0u8; 32];
            if hex::decode_to_slice(&id, &mut bytes).is_ok() {
                out.push((at.max(0) as u64, bytes));
            }
        }
        Ok(out)
    }

    /// Evict torrent bodies (+ their comments, files, FTS rows, raw JSON) until the database is
    /// under `cap_bytes`: only events with `synced = 1` on non-pinned infohashes, least recently
    /// accessed first. `sync_ids` is never touched and unsynced rows are never deleted.
    /// `cap_bytes = 0` means no cap.
    pub fn cleanup(&self, cap_bytes: u64) -> Result<CleanupReport> {
        let mut conn = self.conn();
        let bytes_before = db_bytes(&conn)?;
        let mut evicted = 0u64;
        if cap_bytes > 0 {
            for _ in 0..8 {
                let size = db_bytes(&conn)?;
                if size <= cap_bytes {
                    break;
                }
                let rows: i64 = conn
                    .query_row("SELECT COUNT(*) FROM torrents", [], |r| r.get(0))
                    .db()?;
                if rows == 0 {
                    break;
                }
                // Evict roughly the excess at the average row cost, at least a small batch.
                let per_row = (size / rows as u64).max(1);
                let want = ((size - cap_bytes) / per_row + 1).clamp(16, 5000);
                let n = evict_batch(&mut conn, want)?;
                if n == 0 {
                    break;
                }
                evicted += n;
                compact(&conn)?;
            }
        }
        let bytes_after = db_bytes(&conn)?;
        let unsynced_kept = conn
            .query_row("SELECT COUNT(*) FROM sync_ids WHERE synced = 0", [], |r| {
                r.get::<_, i64>(0)
            })
            .db()?
            .max(0) as u64;
        Ok(CleanupReport {
            evicted,
            bytes_before,
            bytes_after,
            unsynced_kept,
        })
    }
}

fn trust_from_row(pubkey: &str, r: &Row) -> rusqlite::Result<TrustScore> {
    Ok(TrustScore {
        pubkey: pubkey.to_string(),
        score: r.get::<_, i64>(0)?.clamp(0, 100) as u8,
        factors: from_json(&r.get::<_, String>(1)?),
        root: r.get(2)?,
        computed_at: r.get(3)?,
        graph_version: r.get(4)?,
    })
}

/// A JSON-encoded string setting; tolerates a bare (unquoted) value.
pub fn setting_str(v: &str) -> Option<String> {
    match serde_json::from_str::<Option<String>>(v) {
        Ok(s) => s.filter(|s| !s.is_empty()),
        Err(_) => Some(v.trim().to_string()).filter(|s| !s.is_empty()),
    }
}

fn known_id(conn: &Connection, id: &str) -> Result<bool> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM events WHERE id = ?1)
             OR EXISTS(SELECT 1 FROM sync_ids WHERE id = ?1)",
        [id],
        |r| r.get(0),
    )
    .db()
}

fn insert_event(tx: &Transaction, raw: &RawEvent) -> Result<()> {
    tx.execute(
        "INSERT OR REPLACE INTO events(id, pubkey, created_at, kind, json) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![raw.id, raw.pubkey, raw.created_at, raw.kind, raw.to_json()],
    )
    .db()?;
    Ok(())
}

fn insert_sync_id(tx: &Transaction, raw: &RawEvent) -> Result<()> {
    tx.execute(
        "INSERT OR IGNORE INTO sync_ids(id, created_at, kind) VALUES (?1, ?2, ?3)",
        params![raw.id, raw.created_at, raw.kind],
    )
    .db()?;
    Ok(())
}

fn bump_graph(conn: &Connection) -> Result<i64> {
    conn.query_row(
        "UPDATE meta SET value = value + 1 WHERE key = 'graph_version' RETURNING value",
        [],
        |r| r.get(0),
    )
    .db()
}

/// Is `pubkey` the configured root or followed by it (D1)?
fn is_root_or_d1(conn: &Connection, pubkey: &str) -> Result<bool> {
    let root: Option<String> = conn
        .query_row(
            "SELECT value FROM settings WHERE key = ?1",
            [keys::TRUST_ROOT],
            |r| r.get(0),
        )
        .optional()
        .db()?;
    let Some(root) = root.and_then(|v| setting_str(&v)) else {
        return Ok(false);
    };
    if root == pubkey {
        return Ok(true);
    }
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM follows WHERE follower = ?1 AND target = ?2)",
        params![root, pubkey],
        |r| r.get(0),
    )
    .db()
}

/// Rows derived from a replaceable head.
enum Derived {
    Profile(Option<String>),
    Follows(Vec<String>),
    Mutes(Vec<String>),
    RelayList(Vec<wot::RelayListEntry>, bool),
}

impl Derived {
    fn of(raw: &RawEvent) -> Self {
        match raw.kind {
            KIND_PROFILE => Derived::Profile(wot::profile_name(raw)),
            KIND_CONTACTS => Derived::Follows(wot::follows_of(raw)),
            KIND_RELAY_LIST => {
                let (list, oversized) = wot::relay_list_of(raw);
                Derived::RelayList(list, oversized)
            }
            _ => Derived::Mutes(wot::mutes_of(raw)),
        }
    }

    fn apply(&self, tx: &Transaction, raw: &RawEvent) -> Result<()> {
        let pk = &raw.pubkey;
        match self {
            Derived::Profile(name) => {
                tx.execute(
                    "INSERT INTO profiles(pubkey, name) VALUES (?1, ?2)
                     ON CONFLICT(pubkey) DO UPDATE SET name = excluded.name",
                    params![pk, name],
                )
                .db()?;
            }
            Derived::Follows(targets) => {
                tx.execute("DELETE FROM follows WHERE follower = ?1", [pk])
                    .db()?;
                let mut st = tx
                    .prepare_cached(
                        "INSERT OR IGNORE INTO follows(follower, target) VALUES (?1, ?2)",
                    )
                    .db()?;
                for t in targets {
                    st.execute(params![pk, t]).db()?;
                }
            }
            Derived::Mutes(targets) => {
                tx.execute(
                    "DELETE FROM mutes WHERE muter = ?1 AND kind = ?2",
                    params![pk, raw.kind],
                )
                .db()?;
                let mut st = tx
                    .prepare_cached(
                        "INSERT OR IGNORE INTO mutes(muter, kind, target) VALUES (?1, ?2, ?3)",
                    )
                    .db()?;
                for t in targets {
                    st.execute(params![pk, raw.kind, t]).db()?;
                }
            }
            Derived::RelayList(list, oversized) => {
                tx.execute("DELETE FROM relay_lists WHERE pubkey = ?1", [pk])
                    .db()?;
                let mut st = tx
                    .prepare_cached(
                        "INSERT INTO relay_lists(pubkey, url, read, write) VALUES (?1, ?2, ?3, ?4)
                         ON CONFLICT(pubkey, url) DO UPDATE SET read = read OR excluded.read,
                            write = write OR excluded.write",
                    )
                    .db()?;
                for e in list {
                    st.execute(params![pk, e.url, e.read, e.write]).db()?;
                }
                tx.execute(
                    "INSERT INTO relay_list_meta(pubkey, oversized) VALUES (?1, ?2)
                     ON CONFLICT(pubkey) DO UPDATE SET oversized = excluded.oversized",
                    params![pk, oversized],
                )
                .db()?;
            }
        }
        Ok(())
    }
}

fn db_bytes(conn: &Connection) -> Result<u64> {
    let pages: i64 = conn.query_row("PRAGMA page_count", [], |r| r.get(0)).db()?;
    let size: i64 = conn.query_row("PRAGMA page_size", [], |r| r.get(0)).db()?;
    Ok((pages.max(0) * size.max(0)) as u64)
}

/// Delete up to `want` evictable torrent events. Returns how many were removed.
fn evict_batch(conn: &mut Connection, want: u64) -> Result<u64> {
    let tx = conn.transaction().db()?;
    let victims: Vec<String> = {
        let mut st = tx
            .prepare(
                "SELECT t.event_id FROM torrents t
                 JOIN sync_ids s ON s.id = t.event_id AND s.synced = 1
                 LEFT JOIN torrent_flags f ON f.infohash = t.infohash
                 WHERE COALESCE(f.pinned, 0) = 0
                 ORDER BY COALESCE(f.last_access, 0), t.created_at, t.event_id
                 LIMIT ?1",
            )
            .db()?;
        st.query_map([want as i64], |r| r.get(0))
            .db()?
            .collect::<rusqlite::Result<_>>()
            .db()?
    };
    {
        // Only synced comments go; an unsynced comment outlives its evicted torrent body.
        let mut del_comment_events = tx
            .prepare_cached(
                "DELETE FROM events WHERE id IN (
                    SELECT c.event_id FROM comments c JOIN sync_ids s ON s.id = c.event_id
                    WHERE c.torrent_event_id = ?1 AND s.synced = 1)",
            )
            .db()?;
        let mut del_comments = tx
            .prepare_cached(
                "DELETE FROM comments WHERE torrent_event_id = ?1 AND event_id IN (
                    SELECT id FROM sync_ids WHERE synced = 1)",
            )
            .db()?;
        let mut del_torrent = tx
            .prepare_cached("DELETE FROM torrents WHERE event_id = ?1")
            .db()?;
        let mut del_event = tx.prepare_cached("DELETE FROM events WHERE id = ?1").db()?;
        for id in &victims {
            del_comment_events.execute([id]).db()?;
            del_comments.execute([id]).db()?;
            del_torrent.execute([id]).db()?;
            del_event.execute([id]).db()?;
        }
    }
    tx.execute(
        "DELETE FROM torrent_flags WHERE pinned = 0 AND hidden = 0
            AND infohash NOT IN (SELECT infohash FROM torrents)",
        [],
    )
    .db()?;
    tx.commit().db()?;
    Ok(victims.len() as u64)
}

/// Merge FTS segments (deletes only add tombstones) and give free pages back.
fn compact(conn: &Connection) -> Result<()> {
    conn.execute(
        "INSERT INTO torrents_fts(torrents_fts) VALUES ('optimize')",
        [],
    )
    .db()?;
    let auto_vacuum: i64 = conn
        .query_row("PRAGMA auto_vacuum", [], |r| r.get(0))
        .db()?;
    if auto_vacuum == 2 {
        // Frees one page per step: run it to completion (execute_batch steps only once).
        let mut st = conn.prepare("PRAGMA incremental_vacuum").db()?;
        let mut rows = st.query([]).db()?;
        while rows.next().db()?.is_some() {}
    } else {
        conn.execute_batch("VACUUM").db()?;
    }
    // Shrink the WAL file too; ignore "database is locked" style failures.
    let _ = conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)");
    Ok(())
}

#[async_trait::async_trait]
impl Store for SqliteStore {
    async fn put_torrent(&self, t: &ParsedTorrent, raw: &RawEvent) -> Result<bool> {
        self.put_torrent_sync(t, raw)
    }
    async fn put_comment(&self, c: &Comment, raw: &RawEvent) -> Result<bool> {
        self.put_comment_sync(c, raw)
    }
    async fn put_replaceable(&self, raw: &RawEvent) -> Result<bool> {
        self.put_replaceable_sync(raw)
    }
    async fn put_report(&self, raw: &RawEvent) -> Result<bool> {
        self.put_report_sync(raw)
    }
    async fn has_event(&self, id: &str) -> Result<bool> {
        self.has_event_sync(id)
    }
    async fn known_torrent_event(&self, id: &str) -> Result<bool> {
        self.known_torrent_event_sync(id)
    }
    async fn get_raw_events(&self, ids: &[String]) -> Result<Vec<RawEvent>> {
        self.get_raw_events_sync(ids)
    }
    async fn upsert_relay(&self, url: &str, source: &str) -> Result<()> {
        self.upsert_relay_sync(url, source)
    }
    async fn list_relays(&self) -> Result<Vec<RelayInfo>> {
        self.list_relays_sync()
    }
    async fn record_relay_result(
        &self,
        url: &str,
        ok: bool,
        err: Option<&str>,
        found: u64,
        now: i64,
    ) -> Result<()> {
        self.record_relay_result_sync(url, ok, err, found, now)
    }
    async fn set_relay_enabled(&self, url: &str, enabled: bool) -> Result<()> {
        self.set_relay_enabled_sync(url, enabled)
    }
    async fn set_relay_supports_neg(&self, url: &str, supports: bool) -> Result<()> {
        self.set_relay_supports_neg_sync(url, supports)
    }
    async fn get_cursor(&self, relay: &str, kind: u16) -> Result<Option<Cursor>> {
        self.get_cursor_sync(relay, kind)
    }
    async fn put_cursor(&self, c: &Cursor) -> Result<()> {
        self.put_cursor_sync(c)
    }
    async fn publishers(&self, limit: usize) -> Result<Vec<String>> {
        self.publishers_sync(limit)
    }
    async fn relay_lists_for(
        &self,
        pubkeys: &[String],
    ) -> Result<Vec<(String, String, bool, bool)>> {
        self.relay_lists_for_sync(pubkeys)
    }
    async fn relay_list_oversized(&self, pubkey: &str) -> Result<bool> {
        self.relay_list_oversized_sync(pubkey)
    }
    async fn search(&self, q: &SearchQuery, now: i64) -> Result<SearchPage> {
        self.search_sync(q, now)
    }
    async fn torrent_detail(&self, infohash: &str, now: i64) -> Result<Option<TorrentDetail>> {
        self.torrent_detail_sync(infohash, now)
    }
    async fn liveness_due(&self, now: i64, limit: usize) -> Result<Vec<(String, Vec<String>)>> {
        self.liveness_due_sync(now, limit)
    }
    async fn put_liveness(&self, l: &Liveness) -> Result<()> {
        self.put_liveness_sync(l)
    }
    async fn get_liveness(&self, infohash: &str) -> Result<Option<Liveness>> {
        self.get_liveness_sync(infohash)
    }
    async fn follows_of(&self, pubkey: &str) -> Result<Vec<String>> {
        self.follows_of_sync(pubkey)
    }
    async fn followers_among(
        &self,
        targets: &[String],
        among: &[String],
    ) -> Result<Vec<(String, Vec<String>)>> {
        self.followers_among_sync(targets, among)
    }
    async fn mutes_by(&self, muters: &[String]) -> Result<Vec<(String, String)>> {
        self.mutes_by_sync(muters)
    }
    async fn reports_by(&self, reporters: &[String]) -> Result<Vec<(String, String, String)>> {
        self.reports_by_sync(reporters)
    }
    async fn comment_authors_on(&self, publisher: &str, authors: &[String]) -> Result<Vec<String>> {
        self.comment_authors_on_sync(publisher, authors)
    }
    async fn publisher_stats(&self, pubkey: &str) -> Result<PublisherStats> {
        self.publisher_stats_sync(pubkey)
    }
    async fn put_trust(&self, s: &TrustScore) -> Result<()> {
        self.put_trust_sync(s)
    }
    async fn get_trust(&self, pubkey: &str) -> Result<Option<TrustScore>> {
        self.get_trust_sync(pubkey)
    }
    async fn graph_version(&self) -> Result<i64> {
        self.graph_version_sync()
    }
    async fn bump_graph_version(&self) -> Result<i64> {
        self.bump_graph_version_sync()
    }
    async fn set_override(&self, pubkey: &str, action: Option<OverrideAction>) -> Result<()> {
        self.set_override_sync(pubkey, action)
    }
    async fn overrides(&self) -> Result<Vec<(String, OverrideAction)>> {
        self.overrides_sync()
    }
    async fn get_setting(&self, key: &str) -> Result<Option<String>> {
        self.get_setting_sync(key)
    }
    async fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        self.set_setting_sync(key, value)
    }
    async fn list_saved_searches(&self) -> Result<Vec<SavedSearch>> {
        self.list_saved_searches_sync()
    }
    async fn put_saved_search(&self, s: &SavedSearch) -> Result<i64> {
        self.put_saved_search_sync(s)
    }
    async fn delete_saved_search(&self, id: i64) -> Result<()> {
        self.delete_saved_search_sync(id)
    }
    async fn stats(&self) -> Result<StoreStats> {
        self.stats_sync()
    }
}

#[async_trait::async_trait]
impl SyncSource for SqliteStore {
    async fn neg_items(&self) -> Result<Vec<(u64, [u8; 32])>> {
        self.neg_items_sync()
    }
    async fn load_events(&self, ids: &[String]) -> Result<Vec<RawEvent>> {
        self.get_raw_events_sync(ids)
    }
    async fn unsynced(&self, limit: usize) -> Result<Vec<RawEvent>> {
        self.unsynced_sync(limit)
    }
    async fn mark_synced(&self, ids: &[String]) -> Result<()> {
        self.mark_synced_sync(ids)
    }
    async fn record_failure(&self, id: &str, reason: &str) -> Result<()> {
        self.record_failure_sync(id, reason)
    }
}

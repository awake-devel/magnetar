//! SQLite schema and `user_version` migrations for the phone cache.

use rusqlite::Connection;

/// Migration N (1-based) brings `user_version` from N-1 to N. Append only.
const MIGRATIONS: &[&str] = &[V1, V2];

const V1: &str = r#"
CREATE TABLE events(
    id TEXT PRIMARY KEY,
    pubkey TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    kind INTEGER NOT NULL,
    json TEXT NOT NULL
);

-- One row per kind-2003 event; presentation groups by infohash (D6).
CREATE TABLE torrents(
    rowid INTEGER PRIMARY KEY,
    event_id TEXT NOT NULL UNIQUE,
    infohash TEXT NOT NULL,
    pubkey TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    infohash_v2 TEXT,
    title TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    files_text TEXT NOT NULL DEFAULT '',
    files TEXT NOT NULL DEFAULT '[]',
    file_count INTEGER NOT NULL DEFAULT 0,
    total_size INTEGER,
    trackers TEXT NOT NULL DEFAULT '[]',
    category TEXT NOT NULL,
    category_path TEXT NOT NULL DEFAULT '[]',
    hashtags TEXT NOT NULL DEFAULT '[]',
    external_ids TEXT NOT NULL DEFAULT '[]',
    source_url TEXT,
    published_at INTEGER,
    pow_bits INTEGER NOT NULL DEFAULT 0,
    pow_committed INTEGER,
    trust INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX torrents_infohash ON torrents(infohash);
CREATE INDEX torrents_pubkey ON torrents(pubkey, created_at);
CREATE INDEX torrents_created ON torrents(created_at);

CREATE VIRTUAL TABLE torrents_fts USING fts5(
    title, files_text, description,
    content='torrents', content_rowid='rowid',
    tokenize="unicode61 remove_diacritics 2 separators '._-[]()'"
);
CREATE TRIGGER torrents_ai AFTER INSERT ON torrents BEGIN
    INSERT INTO torrents_fts(rowid, title, files_text, description)
    VALUES (new.rowid, new.title, new.files_text, new.description);
END;
CREATE TRIGGER torrents_ad AFTER DELETE ON torrents BEGIN
    INSERT INTO torrents_fts(torrents_fts, rowid, title, files_text, description)
    VALUES ('delete', old.rowid, old.title, old.files_text, old.description);
END;
-- Only text columns: trust refreshes must not rewrite the index.
CREATE TRIGGER torrents_au AFTER UPDATE OF title, files_text, description ON torrents BEGIN
    INSERT INTO torrents_fts(torrents_fts, rowid, title, files_text, description)
    VALUES ('delete', old.rowid, old.title, old.files_text, old.description);
    INSERT INTO torrents_fts(rowid, title, files_text, description)
    VALUES (new.rowid, new.title, new.files_text, new.description);
END;

-- Phone-local flags per infohash; last_access drives LRU eviction.
CREATE TABLE torrent_flags(
    infohash TEXT PRIMARY KEY,
    pinned INTEGER NOT NULL DEFAULT 0,
    hidden INTEGER NOT NULL DEFAULT 0,
    last_access INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE comments(
    event_id TEXT PRIMARY KEY,
    pubkey TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    kind INTEGER NOT NULL,
    torrent_event_id TEXT NOT NULL,
    content TEXT NOT NULL
);
CREATE INDEX comments_torrent ON comments(torrent_event_id);

-- Heads of replaceable kinds 0/3/10000/10002 (d = '') and 30007 (d = tag).
CREATE TABLE replaceable(
    pubkey TEXT NOT NULL,
    kind INTEGER NOT NULL,
    d TEXT NOT NULL DEFAULT '',
    event_id TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (pubkey, kind, d)
);
CREATE TABLE follows(follower TEXT NOT NULL, target TEXT NOT NULL, PRIMARY KEY (follower, target));
CREATE INDEX follows_target ON follows(target);
CREATE TABLE mutes(
    muter TEXT NOT NULL, kind INTEGER NOT NULL, target TEXT NOT NULL,
    PRIMARY KEY (muter, kind, target)
);
CREATE TABLE reports(
    event_id TEXT PRIMARY KEY,
    reporter TEXT NOT NULL,
    target TEXT NOT NULL,
    report_type TEXT NOT NULL,
    created_at INTEGER NOT NULL
);
CREATE INDEX reports_reporter ON reports(reporter);
CREATE TABLE relay_lists(
    pubkey TEXT NOT NULL, url TEXT NOT NULL, read INTEGER NOT NULL, write INTEGER NOT NULL,
    PRIMARY KEY (pubkey, url)
);
CREATE TABLE relay_list_meta(pubkey TEXT PRIMARY KEY, oversized INTEGER NOT NULL);
CREATE TABLE profiles(pubkey TEXT PRIMARY KEY, name TEXT);

CREATE TABLE trust_cache(
    pubkey TEXT PRIMARY KEY,
    score INTEGER NOT NULL,
    factors TEXT NOT NULL,
    root TEXT,
    computed_at INTEGER NOT NULL,
    graph_version INTEGER NOT NULL
);
CREATE TABLE overrides(pubkey TEXT PRIMARY KEY, action TEXT NOT NULL);

CREATE TABLE liveness(
    infohash TEXT PRIMARY KEY,
    seeders INTEGER, leechers INTEGER, completed INTEGER, dht_peers_est INTEGER,
    source TEXT NOT NULL,
    trackers_ok INTEGER NOT NULL, trackers_failed INTEGER NOT NULL,
    checked_at INTEGER NOT NULL, expires_at INTEGER NOT NULL
);

CREATE TABLE relays(
    url TEXT PRIMARY KEY,
    source TEXT NOT NULL,
    enabled INTEGER NOT NULL DEFAULT 1,
    last_ok INTEGER,
    last_err TEXT,
    consecutive_failures INTEGER NOT NULL DEFAULT 0,
    next_try_at INTEGER NOT NULL DEFAULT 0,
    events_found INTEGER NOT NULL DEFAULT 0,
    supports_neg INTEGER
);
CREATE TABLE cursors(
    relay TEXT NOT NULL, kind INTEGER NOT NULL,
    newest_seen INTEGER, backfill_until INTEGER, backfill_done INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (relay, kind)
);

CREATE TABLE settings(key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE meta(key TEXT PRIMARY KEY, value INTEGER NOT NULL);
INSERT INTO meta(key, value) VALUES ('graph_version', 0);

CREATE TABLE saved_searches(
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL,
    query TEXT NOT NULL,
    notify INTEGER NOT NULL,
    last_checked INTEGER,
    last_seen_created_at INTEGER
);

-- Every 2003/2004/1111 ever stored (negentropy set, D14). Never pruned, also after eviction.
CREATE TABLE sync_ids(
    id TEXT PRIMARY KEY,
    created_at INTEGER NOT NULL,
    kind INTEGER NOT NULL,
    synced INTEGER NOT NULL DEFAULT 0,
    failures INTEGER NOT NULL DEFAULT 0,
    failure TEXT
);
CREATE INDEX sync_ids_unsynced ON sync_ids(synced, created_at);
"#;

/// Recompute denormalised `torrents.trust`: V1 builds treated any publisher score of 0 as an
/// override and dropped the PoW bonus (`store::event_trust` has the corrected rule).
const V2: &str = r#"
UPDATE torrents SET trust = MIN(100,
        COALESCE((SELECT c.score FROM trust_cache c WHERE c.pubkey = torrents.pubkey), 0)
        + (25 * MAX(0, MIN(16, pow_bits - 8))) / 16)
WHERE NOT EXISTS (
    SELECT 1 FROM trust_cache c
    WHERE c.pubkey = torrents.pubkey
      AND json_array_length(c.factors) = 1
      AND json_extract(c.factors, '$[0].code') IN ('BLOCKED', 'TRUSTED', 'ROOT_MUTED'));
"#;

/// Bring the database to the latest schema. A brand-new file gets incremental auto-vacuum so
/// `cleanup` can give pages back to the filesystem without a full VACUUM.
pub fn migrate(conn: &mut Connection) -> rusqlite::Result<()> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if version == 0 {
        let pages: i64 = conn.query_row("PRAGMA page_count", [], |r| r.get(0))?;
        if pages == 0 {
            conn.execute_batch("PRAGMA auto_vacuum = INCREMENTAL")?;
        }
    }
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    for (i, sql) in MIGRATIONS.iter().enumerate().skip(version.max(0) as usize) {
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", (i + 1) as i64)?;
        tx.commit()?;
    }
    Ok(())
}

//! D144: finding the due infohashes through the `enrich_due` queue and `enrich_checked_pending`
//! gives the same set as the old anti-join, at a cost that follows the due rows.

use crate::common;

use common::*;
use magnetar_core::enrich::imdb_match::MATCHER_VERSION;
use magnetar_core::model::*;
use magnetar_server::enrich::{self, EnrichConfig};
use sqlx::PgPool;

/// The retry boundary (the dataset import time) of these tests.
const T: i64 = 1000;

/// The query `enrich::due` was before D144: the oracle.
async fn oracle(pool: &PgPool, retry_before: i64) -> Vec<String> {
    let mut v: Vec<String> = sqlx::query_scalar(
        "SELECT t.infohash FROM torrent_infohashes t
         WHERE NOT EXISTS (
             SELECT 1 FROM enrich_checked c WHERE c.infohash = t.infohash
               AND c.matcher_version >= $1 AND NOT (c.pending_title AND c.checked_at <= $2))",
    )
    .bind(MATCHER_VERSION)
    .bind(retry_before)
    .fetch_all(pool)
    .await
    .unwrap();
    v.sort();
    v
}

async fn due(pool: &PgPool, retry_before: i64) -> Vec<String> {
    let mut conn = pool.acquire().await.unwrap();
    let mut v = enrich::due(&mut conn, retry_before, 1_000_000)
        .await
        .unwrap();
    v.sort();
    v
}

async fn checked(pool: &PgPool, n: u64, version: i32, at: i64, pending: bool) {
    sqlx::query(
        "INSERT INTO enrich_checked (infohash, matcher_version, checked_at, pending_title)
         VALUES ($1, $2, $3, $4)
         ON CONFLICT (infohash) DO UPDATE SET matcher_version = $2, checked_at = $3,
             pending_title = $4",
    )
    .bind(ih(n))
    .bind(version)
    .bind(at)
    .bind(pending)
    .execute(pool)
    .await
    .unwrap();
}

async fn dht(pool: &PgPool, n: u64) {
    sqlx::query(
        "INSERT INTO dht_torrents (infohash, title, files, file_count, files_text, total_size,
             category, metadata_size, first_seen, fetched_at, discovered_via, classifier_version)
         VALUES ($1, 'Some.Name', '[]', 0, '', 1, 'video', 10, $2, $2, 'sample', 1)",
    )
    .bind(ih(n))
    .bind(NOW)
    .execute(pool)
    .await
    .unwrap();
}

async fn unput(pool: &PgPool, table: &str, n: u64) {
    let sql = format!("DELETE FROM {table} WHERE infohash = $1");
    sqlx::query(sqlx::AssertSqlSafe(sql))
        .bind(ih(n))
        .execute(pool)
        .await
        .unwrap();
}

fn names(ns: &[u64]) -> Vec<String> {
    let mut v: Vec<String> = ns.iter().map(|n| ih(*n)).collect();
    v.sort();
    v
}

/// Every state of the matrix gives what the old query gives, then again after a forced full
/// recheck and after a matcher upgrade.
#[tokio::test]
async fn due_equals_the_old_anti_join() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let pool = s.pool();
    let cur = MATCHER_VERSION;
    let n = |i: u64| 100 + i;

    put(s, torrent(1, 1, NOW, n(1), "new nostr")).await; // 1: no check
    dht(pool, n(2)).await; // 2: DHT-only, no check
    put(s, torrent(3, 1, NOW, n(3), "checked")).await; // 3: checked, current
    checked(pool, n(3), cur, 5, false).await;
    put(s, torrent(4, 1, NOW, n(4), "old matcher")).await; // 4: older matcher
    checked(pool, n(4), cur - 1, 5, false).await;
    for (i, at) in [(5, T - 10), (6, T + 10), (7, T)] {
        put(s, torrent(i, 1, NOW, n(i), "pending")).await; // 5, 7 due; 6 (after the import) not
        checked(pool, n(i), cur, at, true).await;
    }
    put(s, torrent(8, 1, NOW, n(8), "forced by put_torrent")).await; // 8
    checked(pool, n(8), cur, 5, false).await;
    put(s, torrent(80, 2, NOW + 1, n(8), "a new event")).await;
    dht(pool, n(9)).await; // 9: recheck forced by a DELETE (heal, rematch)
    checked(pool, n(9), cur, 5, false).await;
    unput(pool, "enrich_checked", n(9)).await;
    put(s, torrent(10, 1, NOW, n(10), "gone")).await; // 10: checked, then removed
    checked(pool, n(10), cur, 5, false).await;
    unput(pool, "torrents", n(10)).await;
    put(s, torrent(11, 1, NOW, n(11), "orphan")).await; // 11: pending retry of a removed torrent
    checked(pool, n(11), cur, T - 10, true).await;
    unput(pool, "torrents", n(11)).await;
    dht(pool, n(12)).await; // 12: older matcher, removed and back
    checked(pool, n(12), cur - 1, 5, false).await;
    unput(pool, "dht_torrents", n(12)).await;
    dht(pool, n(12)).await;
    dht(pool, n(13)).await; // 13: current check, removed and back: still not due
    checked(pool, n(13), cur, 5, false).await;
    unput(pool, "dht_torrents", n(13)).await;
    dht(pool, n(13)).await;
    put(s, torrent(14, 1, NOW, n(14), "newer matcher")).await; // 14: newer matcher than ours
    checked(pool, n(14), cur + 1, 5, false).await;
    put(s, torrent(15, 1, NOW, n(15), "no retry")).await; // 15: old, not pending
    checked(pool, n(15), cur, T - 10, false).await;
    // Nostr and DHT on one infohash.
    put(s, torrent(16, 1, NOW, n(16), "both")).await;
    dht(pool, n(16)).await;
    checked(pool, n(16), cur, 5, false).await;

    let expect = |ns: &[u64]| names(&ns.iter().map(|i| n(*i)).collect::<Vec<_>>());
    let want = oracle(pool, T).await;
    assert_eq!(
        want,
        expect(&[1, 2, 4, 5, 7, 8, 9, 12]),
        "the oracle itself"
    );
    assert_eq!(due(pool, T).await, want);
    // Idempotent, and the clean-up of stale rows (10, 11, 13) changed nothing.
    assert_eq!(due(pool, T).await, want);
    // No import yet: no retries.
    assert_eq!(due(pool, -1).await, oracle(pool, -1).await);

    // The check of the removed torrent is gone, the queue row of the other too.
    let left: i64 = sqlx::query_scalar(
        "SELECT (SELECT count(*) FROM enrich_checked WHERE infohash = $1)
              + (SELECT count(*) FROM enrich_due WHERE infohash = ANY($2))",
    )
    .bind(ih(n(11)))
    .bind(vec![ih(n(10)), ih(n(11)), ih(n(13))])
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(left, 0, "stale rows stay forever");

    // Rematch: DELETE FROM enrich_checked.
    sqlx::query("DELETE FROM enrich_checked")
        .execute(pool)
        .await
        .unwrap();
    let all = oracle(pool, T).await;
    assert_eq!(all.len(), 14, "every present torrent");
    assert_eq!(due(pool, T).await, all);

    // Matcher upgrade: a check of an older version, written before the sync, is due once the
    // stored version differs.
    for i in [1, 2, 4] {
        checked(pool, n(i), cur, 5, false).await;
    }
    checked(pool, n(3), cur - 1, 5, false).await;
    sqlx::query("UPDATE meta SET value = $1 WHERE key = 'enrich_queue_version'")
        .bind(i64::from(cur) - 1)
        .execute(pool)
        .await
        .unwrap();
    let want = oracle(pool, T).await;
    assert!(want.contains(&ih(n(3))));
    assert_eq!(due(pool, T).await, want);
}

/// A batch that is not applied stays due; an applied one leaves the queue (same transaction as
/// its checks); nothing loops.
#[tokio::test]
async fn due_survives_a_cancelled_batch_and_empties_after_apply() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let pool = s.pool();
    for i in 1..=5 {
        put(s, torrent(i, 1, NOW, 200 + i, "some title")).await;
    }
    let first = due(pool, -1).await;
    assert_eq!(first.len(), 5);
    assert_eq!(due(pool, -1).await, first, "reading does not consume");
    let cfg = EnrichConfig {
        imdb: None,
        refresh: std::time::Duration::from_secs(3600),
        fuzzy: false,
    };
    let r = enrich::drain(s, &cfg).await.unwrap();
    assert_eq!(r.checked, 5);
    assert!(due(pool, -1).await.is_empty());
    let queued: i64 = sqlx::query_scalar("SELECT count(*) FROM enrich_due")
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(queued, 0);
    // A new event after the check: due again, once.
    put(s, torrent(60, 2, NOW + 1, 203, "more")).await;
    assert_eq!(due(pool, -1).await, names(&[203]));
    assert_eq!(enrich::drain(s, &cfg).await.unwrap().checked, 1);
    assert!(due(pool, -1).await.is_empty());
}

/// New torrents do not wait behind a dataset import's retries, and the retries still all happen.
#[tokio::test]
async fn retries_and_new_torrents_share_the_batch() {
    let Some(db) = test_db().await else { return };
    let pool = db.store.pool();
    let cur = MATCHER_VERSION;
    sqlx::query(
        "INSERT INTO torrent_infohashes (infohash, events)
         SELECT lpad(to_hex(g), 40, '0'), 1 FROM generate_series(1, 160) g",
    )
    .execute(pool)
    .await
    .unwrap();
    // 1..=100 pending retries (checked before the import), 101..=160 never checked.
    sqlx::query("DELETE FROM enrich_due WHERE infohash <= lpad(to_hex(100), 40, '0')")
        .execute(pool)
        .await
        .unwrap();
    for i in 1..=100 {
        checked(pool, i, cur, T - 1, true).await;
    }
    let mut conn = pool.acquire().await.unwrap();
    let mut seen = std::collections::HashSet::new();
    let mut first = true;
    loop {
        let batch = enrich::due(&mut conn, T, 20).await.unwrap();
        if batch.is_empty() {
            break;
        }
        assert!(batch.len() <= 20);
        if first {
            let retries = batch
                .iter()
                .filter(|h| u64::from_str_radix(h, 16).unwrap() <= 100)
                .count();
            assert_eq!(retries, 5, "a quarter of the batch, while new ones wait");
            first = false;
        }
        for h in &batch {
            assert!(seen.insert(h.clone()), "due again after being checked");
            sqlx::query(
                "INSERT INTO enrich_checked (infohash, matcher_version, checked_at, pending_title)
                 VALUES ($1, $2, $3, false)
                 ON CONFLICT (infohash) DO UPDATE SET matcher_version = $2, checked_at = $3,
                     pending_title = false",
            )
            .bind(h)
            .bind(cur)
            .bind(T + 50)
            .execute(&mut *conn)
            .await
            .unwrap();
            sqlx::query("DELETE FROM enrich_due WHERE infohash = $1")
                .bind(h)
                .execute(&mut *conn)
                .await
                .unwrap();
        }
    }
    assert_eq!(seen.len(), 160, "every retry and every new torrent");
}

/// Buffer accesses on the two tables and their indexes, as this transaction has made them.
async fn buffers(tx: &mut sqlx::PgConnection) -> i64 {
    sqlx::query_scalar(
        "SELECT coalesce(sum(pg_stat_get_xact_blocks_fetched(c.oid)), 0)::bigint FROM pg_class c
         WHERE c.relnamespace = current_schema()::regnamespace
           AND c.relname IN ('torrent_infohashes', 'torrent_infohashes_pkey', 'enrich_checked',
                             'enrich_checked_pkey', 'enrich_checked_pending')",
    )
    .fetch_one(tx)
    .await
    .unwrap()
}

/// With 150 000 rows in each table and 13 due, the lookup touches a few hundred buffers (the old
/// anti-join walked both primary keys: tens of thousands at this size; D144). Buffer counts of
/// one transaction are exact, so the bound is not timing dependent.
#[tokio::test]
async fn due_cost_does_not_follow_the_table_size() {
    use sqlx::Connection;
    const N: i64 = 150_000;
    let Some(db) = test_db().await else { return };
    let pool = db.store.pool();
    let cur = MATCHER_VERSION;
    // Seed without queueing (the steady state: everything checked), then the due rows.
    sqlx::query("ALTER TABLE torrent_infohashes DISABLE TRIGGER enrich_due_new")
        .execute(pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO torrent_infohashes (infohash, events)
         SELECT lpad(to_hex(g), 40, '0'), 1 FROM generate_series(1, $1) g",
    )
    .bind(N)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query("ALTER TABLE torrent_infohashes ENABLE TRIGGER enrich_due_new")
        .execute(pool)
        .await
        .unwrap();
    // 10 % pending after the import (not due), the rest plainly current.
    sqlx::query(
        "INSERT INTO enrich_checked (infohash, matcher_version, checked_at, pending_title)
         SELECT lpad(to_hex(g), 40, '0'), $2, $3 + 100, g % 10 = 0 FROM generate_series(1, $1) g",
    )
    .bind(N)
    .bind(cur)
    .bind(T)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query("DELETE FROM enrich_due")
        .execute(pool)
        .await
        .unwrap();
    // The meta row `sync_queue` keeps is already current.
    drop(due(pool, T).await);
    sqlx::query("ANALYZE torrent_infohashes, enrich_checked")
        .execute(pool)
        .await
        .unwrap();
    // 5 new torrents, 5 forced rechecks, 3 retries.
    sqlx::query(
        "INSERT INTO torrent_infohashes (infohash, events)
         SELECT lpad(to_hex(g), 40, '0'), 1 FROM generate_series($1 + 1, $1 + 5) g",
    )
    .bind(N)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "DELETE FROM enrich_checked WHERE infohash IN
         (SELECT lpad(to_hex(g), 40, '0') FROM generate_series(1, 5) g)",
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "UPDATE enrich_checked SET checked_at = $1 - 1, pending_title = true
         WHERE infohash IN (SELECT lpad(to_hex(g), 40, '0') FROM generate_series(100, 102) g)",
    )
    .bind(T)
    .execute(pool)
    .await
    .unwrap();
    let want = {
        let mut v: Vec<String> = sqlx::query_scalar(
            "SELECT lpad(to_hex(g), 40, '0') FROM generate_series(1, 5) g
             UNION ALL SELECT lpad(to_hex(g), 40, '0') FROM generate_series($1 + 1, $1 + 5) g
             UNION ALL SELECT lpad(to_hex(g), 40, '0') FROM generate_series(100, 102) g",
        )
        .bind(N)
        .fetch_all(pool)
        .await
        .unwrap();
        v.sort();
        v
    };
    let mut conn = pool.acquire().await.unwrap();
    let mut tx = conn.begin().await.unwrap();
    let before = buffers(&mut tx).await;
    let mut got = enrich::due(&mut tx, T, 200).await.unwrap();
    let used = buffers(&mut tx).await - before;
    got.sort();
    tx.rollback().await.unwrap();
    assert_eq!(got, want);
    eprintln!("due touched {used} buffers at {N} rows per table");
    assert!(
        used < 400,
        "{used} buffers: the lookup scales with the tables"
    );
}

/// What was due before the migration is due after it: the migration file is run again on a
/// database that has the old state and no queue (the production situation).
#[tokio::test]
async fn migration_backfills_what_was_due() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let pool = s.pool();
    let cur = MATCHER_VERSION;
    put(s, torrent(1, 1, NOW, 301, "never checked")).await;
    dht(pool, 302).await;
    put(s, torrent(3, 1, NOW, 303, "current")).await;
    checked(pool, 303, cur, 5, false).await;
    put(s, torrent(4, 1, NOW, 304, "older matcher")).await;
    checked(pool, 304, cur - 1, 5, false).await;
    put(s, torrent(5, 1, NOW, 305, "pending")).await;
    checked(pool, 305, cur, T - 5, true).await;
    put(s, torrent(6, 1, NOW, 306, "pending, after the import")).await;
    checked(pool, 306, cur, T + 5, true).await;
    put(s, torrent(7, 1, NOW, 307, "removed")).await;
    unput(pool, "torrents", 307).await;
    let want = oracle(pool, T).await;
    assert_eq!(want, names(&[301, 302, 304, 305]));

    // Back to the schema before the migration, then the migration as shipped.
    for sql in [
        "DROP TRIGGER enrich_due_new ON torrent_infohashes",
        "DROP TRIGGER enrich_due_recheck ON enrich_checked",
        "DROP FUNCTION enrich_due_new()",
        "DROP FUNCTION enrich_due_recheck()",
        "DROP TABLE enrich_due",
        "DROP SEQUENCE enrich_due_seq",
        "DELETE FROM meta WHERE key = 'enrich_queue_version'",
    ] {
        sqlx::query(sql).execute(pool).await.unwrap();
    }
    sqlx::raw_sql(include_str!(
        "../../migrations/20261112000000_enrich_due.sql"
    ))
    .execute(pool)
    .await
    .unwrap();
    assert_eq!(due(pool, T).await, want);
    // And the triggers are live: a torrent that arrives right after is due.
    put(s, torrent(8, 1, NOW, 308, "after")).await;
    let mut want = want;
    want.push(ih(308));
    want.sort();
    assert_eq!(due(pool, T).await, want);
    assert_eq!(due(pool, T).await, oracle(pool, T).await);
}

fn itag(mut t: common::T, k: &str, v: &str) -> common::T {
    t.p.external_ids.push((k.into(), v.into()));
    t
}

/// A heal query inside `apply` deletes the check of a member applied earlier in the same batch;
/// the trigger re-queues it and the batch's own queue delete must not take it off again.
#[tokio::test]
async fn a_heal_inside_a_batch_keeps_the_healed_torrent_due() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let pool = s.pool();
    let cfg = EnrichConfig {
        imdb: None,
        refresh: std::time::Duration::from_secs(3600),
        fuzzy: false,
    };
    // 7 sorts first: deliberate tags imdb tt4444444 + tmdb movie:999.
    put(
        s,
        itag(
            itag(
                torrent(7, 100, NOW, 7, "Tagged.Film.2025.1080p"),
                "imdb",
                "tt4444444",
            ),
            "tmdb",
            "movie:999",
        )
        .cat(Category::Video),
    )
    .await;
    // 8: the same TMDB id next to another IMDb id: heals 7 in the same batch.
    put(
        s,
        torrent(8, 100, NOW, 8, "Third.Film.2025.1080p")
            .cat(Category::Video)
            .desc("https://www.imdb.com/title/tt3333333/ https://www.themoviedb.org/movie/999"),
    )
    .await;
    assert_eq!(enrich::enrich_batch(s, &cfg, 200).await.unwrap().checked, 2);
    let want = oracle(pool, -1).await;
    assert_eq!(want, names(&[7]), "the heal made 7 due again");
    assert_eq!(due(pool, -1).await, want);
    assert_eq!(enrich::drain(s, &cfg).await.unwrap().checked, 1);
    assert!(due(pool, -1).await.is_empty());
    assert_eq!(oracle(pool, -1).await, Vec::<String>::new());
}

/// A long run of stale queue rows (torrents removed) neither ends a drain early nor hides the
/// due rows behind it.
#[tokio::test]
async fn a_long_run_of_stale_rows_does_not_hide_due_ones() {
    let Some(db) = test_db().await else { return };
    let pool = db.store.pool();
    sqlx::query(
        "INSERT INTO torrent_infohashes (infohash, events)
         SELECT lpad(to_hex(g), 40, '0'), 1 FROM generate_series(1, 1000) g",
    )
    .execute(pool)
    .await
    .unwrap();
    // 1..=990 vanish (rows stay in the queue), 991..=1000 remain.
    sqlx::query("DELETE FROM torrent_infohashes WHERE infohash <= lpad(to_hex(990), 40, '0')")
        .execute(pool)
        .await
        .unwrap();
    let mut conn = pool.acquire().await.unwrap();
    let got = enrich::due(&mut conn, -1, 4).await.unwrap();
    assert!(!got.is_empty(), "the queue is read past 990 stale rows");
    assert!(got.iter().all(|h| h.as_str() > ih(990).as_str()));
    drop(conn);
    assert_eq!(due(pool, -1).await, oracle(pool, -1).await);
}

/// The retry share is not lost to new torrents when no retry is due.
#[tokio::test]
async fn unused_retry_share_goes_to_the_queue() {
    let Some(db) = test_db().await else { return };
    let pool = db.store.pool();
    sqlx::query(
        "INSERT INTO torrent_infohashes (infohash, events)
         SELECT lpad(to_hex(g), 40, '0'), 1 FROM generate_series(1, 40) g",
    )
    .execute(pool)
    .await
    .unwrap();
    let mut conn = pool.acquire().await.unwrap();
    assert_eq!(enrich::due(&mut conn, T, 20).await.unwrap().len(), 20);
    assert_eq!(enrich::due(&mut conn, -1, 20).await.unwrap().len(), 20);
}

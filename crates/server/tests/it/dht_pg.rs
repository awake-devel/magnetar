//! DHT index, Postgres side (docs/DHT.md §2.3, §3.7): the migration's triggers and view, triage
//! against blocked / known / already-seen hashes, and the persisted queue (leases, backoff,
//! announce retries). Needs MAGNETAR_TEST_DATABASE_URL (else SKIPPED).

use crate::common;

use common::*;
use magnetar_core::dht::Via;
use magnetar_core::metainfo::{FetchFailure, PeerError};
use magnetar_core::store::Store;
use magnetar_server::dht::persist::{FetchResult, persist_batch};
use magnetar_server::dht::store as dstore;
use sqlx::PgPool;

fn hx(h: &[u8; 20]) -> String {
    hex::encode(h)
}

async fn seen_row(
    pool: &PgPool,
    h: &[u8; 20],
) -> Option<(i16, i16, i16, Option<i64>, Option<String>)> {
    sqlx::query_as(
        "SELECT status, via, attempts, next_attempt_at, last_error FROM dht_seen WHERE infohash = $1",
    )
    .bind(h.as_slice())
    .fetch_optional(pool)
    .await
    .unwrap()
}

async fn insert_dht_torrent(pool: &PgPool, infohash: &str, title: &str, first_seen: i64) {
    sqlx::query(
        "INSERT INTO dht_torrents (infohash, title, files, file_count, files_text, total_size,
             category, metadata_size, first_seen, fetched_at, discovered_via, classifier_version)
         VALUES ($1, $2, '[]', 1, 'a.mkv', 1000, 'video', 300, $3, $3, 'sample', 1)",
    )
    .bind(infohash)
    .bind(title)
    .bind(first_seen)
    .execute(pool)
    .await
    .unwrap();
}

async fn meta(pool: &PgPool, key: &str) -> i64 {
    sqlx::query_scalar("SELECT value FROM meta WHERE key = $1")
        .bind(key)
        .fetch_one(pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn migration_triggers_and_view() {
    let Some(db) = test_db().await else { return };
    let pool = db.store.pool();
    let before = meta(pool, "infohash_count").await;

    // A Nostr torrent and a DHT torrent on the same hash, plus a DHT-only one.
    db.store
        .put_torrent(
            &torrent(1, 1, NOW, 0xaa, "Nostr title").p,
            &torrent(1, 1, NOW, 0xaa, "x").raw(),
        )
        .await
        .unwrap();
    assert_eq!(meta(pool, "infohash_count").await, before + 1);
    insert_dht_torrent(pool, &ih(0xaa), "dht name", NOW - 100).await;
    insert_dht_torrent(pool, &ih(0xbb), "dht only", NOW - 50).await;
    assert_eq!(
        meta(pool, "infohash_count").await,
        before + 2,
        "shared hash counted once"
    );
    let flags: Vec<(String, i32, bool)> = sqlx::query_as(
        "SELECT infohash, events, dht FROM torrent_infohashes WHERE infohash IN ($1, $2) ORDER BY 1",
    )
    .bind(ih(0xaa))
    .bind(ih(0xbb))
    .fetch_all(pool)
    .await
    .unwrap();
    assert_eq!(flags, vec![(ih(0xaa), 1, true), (ih(0xbb), 0, true)]);
    // The DHT-only hash is scheduled for seed checks, newest first.
    let due: i64 = sqlx::query_scalar("SELECT due_at FROM liveness_schedule WHERE infohash = $1")
        .bind(ih(0xbb))
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(due, -(NOW - 50));
    // The index log saw both inserts.
    let logged: i64 = sqlx::query_scalar(
        "SELECT count(DISTINCT infohash) FROM search_changes WHERE infohash IN ($1, $2)",
    )
    .bind(ih(0xaa))
    .bind(ih(0xbb))
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(logged, 2);
    // View: Nostr first (trust DESC), DHT rows with trust -1 and origin 'dht'.
    let rows: Vec<(String, String, i16, String)> = sqlx::query_as(
        "SELECT event_id, origin, trust, title FROM torrent_rows WHERE infohash = $1
         ORDER BY trust DESC",
    )
    .bind(ih(0xaa))
    .fetch_all(pool)
    .await
    .unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].1, "nostr");
    assert_eq!(
        (rows[1].0.as_str(), rows[1].1.as_str(), rows[1].2),
        ("", "dht", -1)
    );

    // Deleting the Nostr event keeps the hash the DHT still holds.
    sqlx::query("DELETE FROM events WHERE id = $1")
        .bind(hex64(1))
        .execute(pool)
        .await
        .unwrap();
    let left: Option<(i32, bool)> =
        sqlx::query_as("SELECT events, dht FROM torrent_infohashes WHERE infohash = $1")
            .bind(ih(0xaa))
            .fetch_optional(pool)
            .await
            .unwrap();
    assert_eq!(left, Some((0, true)));
    assert_eq!(meta(pool, "infohash_count").await, before + 2);
    // Deleting the DHT rows drops both hashes.
    sqlx::query("DELETE FROM dht_torrents")
        .execute(pool)
        .await
        .unwrap();
    let n: i64 =
        sqlx::query_scalar("SELECT count(*) FROM torrent_infohashes WHERE infohash IN ($1, $2)")
            .bind(ih(0xaa))
            .bind(ih(0xbb))
            .fetch_one(pool)
            .await
            .unwrap();
    assert_eq!(n, 0);
    assert_eq!(meta(pool, "infohash_count").await, before);
    assert_eq!(meta(pool, "dht_blocklist_version").await, 0);
}

#[tokio::test]
async fn triage_dedupes_against_blocked_known_and_seen() {
    let Some(db) = test_db().await else { return };
    let pool = db.store.pool();
    let fresh = [1u8; 20];
    let blocked = [2u8; 20];
    let known = [3u8; 20];
    sqlx::query("INSERT INTO blocked_infohashes VALUES ($1, $2, 1, 'dht')")
        .bind(hx(&blocked))
        .bind(NOW)
        .execute(pool)
        .await
        .unwrap();
    // Known = already in torrent_infohashes (here through a stored DHT row).
    insert_dht_torrent(pool, &hx(&known), "stored", NOW).await;

    let batch = vec![
        (fresh, Via::Sample),
        (fresh, Via::Announce), // duplicate in one batch: one row, the announce wins
        (blocked, Via::Sample),
        (known, Via::GetPeers),
    ];
    let o = dstore::triage(pool, &batch, NOW, false).await.unwrap();
    assert_eq!((o.blocked, o.known), (1, 1));
    assert_eq!(o.queued, vec![fresh]);
    let (status, via, attempts, next, _) = seen_row(pool, &fresh).await.unwrap();
    assert_eq!((status, via, attempts), (0, 1, 0));
    assert_eq!(
        next,
        Some(NOW + dstore::LEASE_SECS),
        "a queued row holds a lease"
    );
    assert!(
        seen_row(pool, &blocked).await.is_none(),
        "blocked hashes never reach dht_seen"
    );

    // Seen again within the lease: not queued twice (dedupe across restarts).
    let o = dstore::triage(pool, &[(fresh, Via::Sample)], NOW + 60, false)
        .await
        .unwrap();
    assert!(o.queued.is_empty());
    // After the lease expired (the fetch never ran): queued again.
    let later = NOW + dstore::LEASE_SECS + 1;
    let o = dstore::triage(pool, &[(fresh, Via::Sample)], later, false)
        .await
        .unwrap();
    assert_eq!(o.queued, vec![fresh]);

    // fetch_known lets a known hash through.
    let o = dstore::triage(pool, &[(known, Via::Sample)], NOW, true)
        .await
        .unwrap();
    assert_eq!((o.known, o.queued.len()), (0, 1));
}

#[tokio::test]
async fn failures_back_off_and_announces_retry() {
    let Some(db) = test_db().await else { return };
    let pool = db.store.pool();
    let h = [7u8; 20];
    dstore::triage(pool, &[(h, Via::Sample)], NOW, false)
        .await
        .unwrap();
    let mut t = NOW;
    let mut nexts = Vec::new();
    for _ in 0..4 {
        // A failed fetch, as the fleet ingest writes one a worker reported.
        let failed = FetchResult {
            sighted: h,
            via: Via::Sample,
            peers_seen: 1,
            result: Err(FetchFailure {
                error: PeerError::Timeout,
                peers_tried: 1,
                oversize: false,
                skipped: 0,
            }),
        };
        let o = persist_batch(pool, &[failed], None, false, t)
            .await
            .unwrap();
        assert_eq!(o.failed, 1);
        let (status, _, _, next, err) = seen_row(pool, &h).await.unwrap();
        assert_eq!((status, err.as_deref()), (2, Some("timeout")));
        nexts.push(next.map(|n| n - t));
        t += 10;
    }
    assert_eq!(
        nexts,
        vec![Some(3600), Some(6 * 3600), Some(24 * 3600), None]
    );
    // Final: a sample does not bring it back...
    let o = dstore::triage(pool, &[(h, Via::Sample)], t + 7 * 86400, false)
        .await
        .unwrap();
    assert!(o.queued.is_empty());
    // ...but an announce (a live peer) does, once the last attempt is > 10 min old.
    let o = dstore::triage(pool, &[(h, Via::Announce)], t + 60, false)
        .await
        .unwrap();
    assert!(o.queued.is_empty(), "too soon");
    let o = dstore::triage(pool, &[(h, Via::Announce)], t + 7 * 86400, false)
        .await
        .unwrap();
    assert_eq!(o.queued, vec![h]);
    assert_eq!(seen_row(pool, &h).await.unwrap().0, 0);
}

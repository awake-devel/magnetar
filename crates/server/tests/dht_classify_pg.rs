//! DHT stage 3 (docs/DHT.md §6, §7; D52, D53, D68, D69) against Postgres: the hard blocklist on
//! Nostr ingest (title, files, description, hashtags; context tier only for adult torrents;
//! the hash and id remembered, later events for that hash rejected), re-classification of
//! stage 2 rows, DHT rows in the seed-check schedule, and fail-closed loading of
//! `MAGNETAR_BLOCKLIST_FILE`. Needs MAGNETAR_TEST_DATABASE_URL (else SKIPPED).
//!
//! Only neutral placeholder terms are used (`zorblax` hard, `wibblewob` context): no real term
//! list appears in tests.

mod common;

use std::sync::Arc;

use clap::Parser;
use common::*;
use magnetar_core::blocklist::{self, Blocklist};
use magnetar_core::classify::CLASSIFIER_VERSION;
use magnetar_core::ingest::{self, IngestOutcome};
use magnetar_core::model::Rejection;
use magnetar_server::config::Cli;
use magnetar_server::dht::persist::reclassify_batch;
use magnetar_server::dht::screen::{self, BlocklistScreen};
use sqlx::PgPool;

const EXTRA: &str = "zorblax\ncontext:wibblewob\n";

fn test_list() -> Blocklist {
    Blocklist::builtin().unwrap().with_extra(EXTRA).unwrap()
}

/// The process-wide list with the placeholders (installed once per test binary).
fn install() {
    blocklist::install(test_list());
    let none: [&str; 0] = [];
    assert!(
        blocklist::global()
            .unwrap()
            .check(&["zorblax"], &none, false)
            .is_block(),
        "placeholder list installed"
    );
}

fn keys() -> nostr::prelude::Keys {
    nostr::prelude::Keys::generate()
}

fn event(
    k: &nostr::prelude::Keys,
    infohash: &str,
    title: &str,
    cat: &str,
    file: &str,
    content: &str,
    hashtag: Option<&str>,
) -> magnetar_core::model::RawEvent {
    let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    let mut tags = vec![
        s(&["title", title]),
        s(&["x", infohash]),
        s(&["file", file, "900000000"]),
        s(&["t", cat]),
    ];
    if let Some(h) = hashtag {
        tags.push(s(&["t", h]));
    }
    signed(k, 2003, tags, content)
}

async fn count(pool: &PgPool, sql: &str) -> i64 {
    sqlx::query_scalar(sqlx::AssertSqlSafe(sql.to_string()))
        .fetch_one(pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn nostr_ingest_applies_the_blocklist() {
    install();
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let pool = s.pool();
    let k = keys();
    let now = magnetar_core::now();
    let ingest = async |raw| ingest::ingest_into(s, raw, now).await.unwrap();
    let blocked = IngestOutcome::Rejected(Rejection::Blocked);

    // Hard tier: title, file name, description and hashtag each block on their own.
    let h1 = ih(1);
    let e = event(&k, &h1, "Some.Zorblax.Name", "video", "a.mkv", "", None);
    let id1 = e.id.clone();
    assert_eq!(ingest(e).await, blocked);
    assert_eq!(
        ingest(event(
            &k,
            &ih(2),
            "Fine",
            "video",
            "dir/z0rbl4x.mkv",
            "",
            None
        ))
        .await,
        blocked,
        "file name, substitution variant"
    );
    assert_eq!(
        ingest(event(
            &k,
            &ih(3),
            "Fine",
            "video",
            "a.mkv",
            "about zorblax",
            None
        ))
        .await,
        blocked,
        "description"
    );
    assert_eq!(
        ingest(event(
            &k,
            &ih(4),
            "Fine",
            "video",
            "a.mkv",
            "",
            Some("zorblax")
        ))
        .await,
        blocked,
        "hashtag"
    );
    // Context tier: only adult torrents.
    assert_eq!(
        ingest(event(
            &k,
            &ih(5),
            "Wibblewob.Film.1080p",
            "video",
            "a.mkv",
            "",
            None
        ))
        .await,
        IngestOutcome::Inserted
    );
    assert_eq!(
        ingest(event(
            &k,
            &ih(6),
            "Wibblewob.Clip",
            "porn",
            "a.mp4",
            "",
            None
        ))
        .await,
        blocked,
        "publisher filed it under porn"
    );
    assert_eq!(
        ingest(event(
            &k,
            &ih(7),
            "Studio.XXX.Wibblewob",
            "video",
            "a.mp4",
            "",
            None
        ))
        .await,
        blocked,
        "adult signal from the name"
    );
    assert_eq!(
        ingest(event(
            &k,
            &ih(8),
            "Wibblewob",
            "video",
            "a.mp4",
            "wibblewob",
            None
        ))
        .await,
        IngestOutcome::Inserted,
        "context terms are not checked in descriptions"
    );
    // A harmless later event for a blocked hash, and the same blocked event again.
    assert_eq!(
        ingest(event(&k, &h1, "Harmless", "video", "a.mkv", "", None)).await,
        blocked
    );
    // Only hashes and ids were kept; nothing named.
    assert_eq!(
        count(
            pool,
            "SELECT count(*) FROM blocked_infohashes WHERE origin = 'nostr'"
        )
        .await,
        6
    );
    assert_eq!(count(pool, "SELECT count(*) FROM blocked_events").await, 6);
    let ids: Vec<String> = sqlx::query_scalar("SELECT id FROM blocked_events")
        .fetch_all(pool)
        .await
        .unwrap();
    assert!(ids.contains(&id1));
    assert_eq!(count(pool, "SELECT count(*) FROM torrents").await, 2);
    assert_eq!(
        count(
            pool,
            "SELECT count(*) FROM events WHERE raw ILIKE '%orblax%' OR raw ILIKE '%z0rbl4x%'"
        )
        .await,
        0
    );
    // The relay verdict text.
    assert_eq!(Rejection::Blocked.ok_message(), "blocked: content policy");
}

#[tokio::test]
async fn nostr_block_removes_a_dht_row_of_the_same_hash() {
    install();
    let Some(db) = test_db().await else { return };
    let pool = db.store.pool();
    insert_dht(pool, &ih(9), "Harmless.Dht.Name", "video", 1).await;
    let e = event(&keys(), &ih(9), "zorblax", "video", "a.mkv", "", None);
    assert_eq!(
        ingest::ingest_into(&db.store, e, magnetar_core::now())
            .await
            .unwrap(),
        IngestOutcome::Rejected(Rejection::Blocked)
    );
    assert_eq!(count(pool, "SELECT count(*) FROM dht_torrents").await, 0);
}

async fn insert_dht(pool: &PgPool, infohash: &str, title: &str, cat: &str, version: i16) {
    sqlx::query(
        "INSERT INTO dht_torrents (infohash, title, files, file_count, files_text, total_size,
             category, metadata_size, first_seen, fetched_at, discovered_via, classifier_version)
         VALUES ($1, $2, jsonb_build_array(jsonb_build_object('path', $2 || '.mp4',
                 'size', 900000000)), 1, $2 || '.mp4', 900000000, $3, 1000, $4, $4, 'sample', $5)",
    )
    .bind(infohash)
    .bind(title)
    .bind(cat)
    .bind(NOW)
    .bind(version)
    .execute(pool)
    .await
    .unwrap();
}

#[tokio::test]
async fn stage2_rows_are_reclassified_and_rescreened() {
    let Some(db) = test_db().await else { return };
    let pool = db.store.pool();
    insert_dht(pool, &ih(1), "Studio.XXX.Scene.2024.1080p", "other", 0).await;
    insert_dht(pool, &ih(2), "Some.Film.2020.1080p.BluRay.x264", "other", 0).await;
    insert_dht(pool, &ih(3), "Studio.XXX.Wibblewob.Scene", "other", 0).await;
    insert_dht(pool, &ih(4), "Current.Row", "video", CLASSIFIER_VERSION).await;
    let screen = BlocklistScreen(Arc::new(test_list()));
    let o = reclassify_batch(pool, &screen, 500, NOW).await.unwrap();
    assert_eq!((o.rows, o.blocked), (3, 1), "{o:?}");
    let rows: Vec<(String, String, i16)> = sqlx::query_as(
        "SELECT infohash, category, classifier_version FROM dht_torrents ORDER BY infohash",
    )
    .fetch_all(pool)
    .await
    .unwrap();
    assert_eq!(
        rows,
        vec![
            (ih(1), "porn".into(), CLASSIFIER_VERSION),
            (ih(2), "video".into(), CLASSIFIER_VERSION),
            (ih(4), "video".into(), CLASSIFIER_VERSION),
        ]
    );
    let origin: String =
        sqlx::query_scalar("SELECT origin FROM blocked_infohashes WHERE infohash = $1")
            .bind(ih(3))
            .fetch_one(pool)
            .await
            .unwrap();
    assert_eq!(origin, "sweep");
    assert_eq!(
        reclassify_batch(pool, &screen, 500, NOW)
            .await
            .unwrap()
            .rows,
        0,
        "done"
    );
}

#[tokio::test]
async fn dht_rows_enter_the_seed_check_schedule() {
    let Some(db) = test_db().await else { return };
    let pool = db.store.pool();
    insert_dht(pool, &ih(5), "Some.Film.2020.1080p", "video", 1).await;
    let due = magnetar_server::liveness::due(pool, i64::MAX / 2, 10)
        .await
        .unwrap();
    let row = due.iter().find(|r| r.infohash == ih(5)).expect("scheduled");
    assert!(
        row.trackers.is_empty(),
        "no trackers: the checker uses the default trackers, then the DHT"
    );
}

#[test]
fn blocklist_file_loads_additively_and_fails_closed() {
    let dir = std::env::var("HOME").unwrap() + "/.cache/magnetar-dht-scratch/test-blocklist";
    std::fs::create_dir_all(&dir).unwrap();
    let bad = format!("{dir}/bad-{:x}.txt", rand::random::<u64>());
    std::fs::write(&bad, "one two three four\n").unwrap();
    let cli = Cli::try_parse_from(["magnetar-server", "--blocklist-file", &bad, "serve"]).unwrap();
    let e = screen::install_from_config(&cli.config).unwrap_err();
    let msg = format!("{e:#}");
    assert!(msg.contains("malformed line 1"), "{msg}");
    assert!(
        !msg.contains("four"),
        "the line itself is never echoed: {msg}"
    );
    let missing = Cli::try_parse_from([
        "magnetar-server",
        "--blocklist-file",
        "/nonexistent/blocklist.txt",
        "serve",
    ])
    .unwrap();
    assert!(screen::install_from_config(&missing.config).is_err());
    let _ = std::fs::remove_file(bad);
}

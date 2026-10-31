//! DHT crawler, Postgres side (docs/DHT.md §2.3, §3.7): the migration's triggers and view, triage
//! against blocked / known / already-seen hashes, the persisted queue (leases, retries, backoff),
//! identity and contact persistence, and the whole pipeline against a local fake DHT (several
//! in-process nodes on loopback addresses). Needs MAGNETAR_TEST_DATABASE_URL (else SKIPPED).

use crate::common;

use std::collections::HashSet;
use std::net::{Ipv4Addr, SocketAddrV4};
use std::time::Duration;

use common::*;
use magnetar_core::dht::krpc::Query;
use magnetar_core::dht::{CompactNode, Dht, EngineConfig, Identity, Mode, Via};
use magnetar_core::store::Store;
use magnetar_server::dht::{self, DhtOptions, store as dstore};
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
        dstore::mark_failed(pool, &h, "no_peers", t).await.unwrap();
        let (status, _, _, next, err) = seen_row(pool, &h).await.unwrap();
        assert_eq!((status, err.as_deref()), (2, Some("no_peers")));
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

/// D93: the batched write (one statement, the collectors no longer wait on it) ends every row
/// exactly as the single-row calls did, a hash listed twice included.
#[tokio::test]
async fn batched_failures_match_single_writes() {
    let Some(db) = test_db().await else { return };
    let pool = db.store.pool();
    let hs: Vec<[u8; 20]> = (1..=4u8).map(|i| [i; 20]).collect();
    let b = [9u8; 20];
    let mut all = hs.clone();
    all.push(b);
    let input: Vec<([u8; 20], Via)> = all.iter().map(|h| (*h, Via::Sample)).collect();
    dstore::triage(pool, &input, NOW, false).await.unwrap();
    // Reference: single writes, hash i failed i times.
    for (i, h) in hs.iter().enumerate() {
        for k in 0..=i {
            dstore::mark_failed(pool, h, "no_peers", NOW + k as i64)
                .await
                .unwrap();
        }
    }
    let want: Vec<_> = futures::future::join_all(hs.iter().map(|h| seen_row(pool, h))).await;
    // Same history through the batch, on a fresh copy of each row.
    sqlx::query(
        "UPDATE dht_seen SET status = 0, attempts = 0, last_error = NULL, next_attempt_at = $1",
    )
    .bind(NOW + 3600)
    .execute(pool)
    .await
    .unwrap();
    let mut batch = Vec::new();
    for (i, h) in hs.iter().enumerate() {
        for k in 0..=i {
            batch.push((*h, "no_peers".to_string(), NOW + k as i64));
        }
    }
    batch.push(([0xEE; 20], "no_peers".into(), NOW)); // no row: ignored
    let n = dstore::mark_failed_batch(pool, &batch).await.unwrap();
    assert_eq!(n, 4);
    let got: Vec<_> = futures::future::join_all(hs.iter().map(|h| seen_row(pool, h))).await;
    assert_eq!(got, want);
    assert_eq!(
        got.iter()
            .map(|r| r.as_ref().unwrap().4.as_deref())
            .collect::<Vec<_>>(),
        vec![Some("no_peers"); 4]
    );
    let nexts: Vec<Option<i64>> = got.iter().map(|r| r.as_ref().unwrap().3).collect();
    assert_eq!(
        nexts,
        vec![
            Some(NOW + 3600),
            Some(NOW + 1 + 6 * 3600),
            Some(NOW + 2 + 24 * 3600),
            None
        ]
    );
    // The untouched row is still queued; the via upgrade records the announce on any row.
    assert_eq!(seen_row(pool, &b).await.unwrap().0, 0);
    assert_eq!(
        dstore::set_via_announce(pool, &[b, hs[0]]).await.unwrap(),
        2
    );
    assert_eq!(
        dstore::set_via_announce(pool, &[b]).await.unwrap(),
        0,
        "already"
    );
    assert_eq!(seen_row(pool, &b).await.unwrap().1, 1);
    assert_eq!(seen_row(pool, &hs[0]).await.unwrap().1, 1);
}

#[tokio::test]
async fn retry_loop_takes_due_rows_and_settles_known_and_blocked() {
    let Some(db) = test_db().await else { return };
    let pool = db.store.pool();
    let (a, b, c, d) = ([0xa1u8; 20], [0xb2u8; 20], [0xc3u8; 20], [0xd4u8; 20]);
    let batch: Vec<_> = [a, b, c, d].iter().map(|h| (*h, Via::GetPeers)).collect();
    assert_eq!(
        dstore::triage(pool, &batch, NOW, false)
            .await
            .unwrap()
            .queued
            .len(),
        4
    );
    // b was stored meanwhile, c got blocked, d is not due yet (fresh lease).
    insert_dht_torrent(pool, &hx(&b), "stored", NOW).await;
    sqlx::query("INSERT INTO blocked_infohashes VALUES ($1, $2, 1, 'dht')")
        .bind(hx(&c))
        .bind(NOW)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("UPDATE dht_seen SET next_attempt_at = $2 WHERE infohash = $1")
        .bind(d.as_slice())
        .bind(NOW + 10 * dstore::LEASE_SECS)
        .execute(pool)
        .await
        .unwrap();
    let now = NOW + dstore::LEASE_SECS;
    let due = dstore::take_due(pool, now, 1000).await.unwrap();
    assert_eq!(due, vec![(a, 2)]);
    assert_eq!(
        seen_row(pool, &a).await.unwrap().3,
        Some(now + dstore::LEASE_SECS),
        "new lease"
    );
    let (status, _, _, next, _) = seen_row(pool, &b).await.unwrap();
    assert_eq!((status, next), (dstore::STATUS_SKIPPED_KNOWN, None));
    assert!(seen_row(pool, &c).await.is_none());
    assert!(dstore::take_due(pool, now, 1000).await.unwrap().is_empty());
}

#[tokio::test]
async fn identity_and_contacts_round_trip() {
    let Some(db) = test_db().await else { return };
    let pool = db.store.pool();
    assert!(
        dstore::load_identity(pool, &[6891])
            .await
            .unwrap()
            .is_none()
    );
    let id = Identity {
        external_ip: Ipv4Addr::new(203, 0, 113, 7),
        ids: vec![[5; 20], [6; 20]],
    };
    dstore::save_identity(pool, &id, &[6891, 6892])
        .await
        .unwrap();
    assert_eq!(
        dstore::load_identity(pool, &[6891, 6892]).await.unwrap(),
        Some(id)
    );
    assert!(
        dstore::load_identity(pool, &[7000, 7001])
            .await
            .unwrap()
            .is_none(),
        "another port set derives new ids"
    );
    let nodes: Vec<CompactNode> = (0..1200u32)
        .map(|i| CompactNode {
            id: [i as u8; 20],
            addr: SocketAddrV4::new(Ipv4Addr::from(0x0101_0000 + i), 6881),
        })
        .collect();
    dstore::save_nodes(pool, &nodes).await.unwrap();
    let back = dstore::load_nodes(pool).await.unwrap();
    assert_eq!(back.len(), 1000);
    assert_eq!(back[..], nodes[..1000]);
    // Garbage in settings is ignored, not fatal.
    sqlx::query("UPDATE settings SET value = 'not json' WHERE key = $1")
        .bind(dstore::NODES_KEY)
        .execute(pool)
        .await
        .unwrap();
    assert!(dstore::load_nodes(pool).await.unwrap().is_empty());
}

// ---------------------------------------------------------------------------------------------
// The whole pipeline against a local fake DHT.

fn fake_cfg(ip: Ipv4Addr, bootstrap: Vec<SocketAddrV4>) -> EngineConfig {
    let mut c = EngineConfig::new(Mode::Aggressive);
    c.bind = ip;
    c.ports = vec![0];
    c.qps = 400;
    c.pool_cap = 10_000;
    c.bootstrap = bootstrap;
    c.allow_loopback = true;
    c.query_timeout = Duration::from_millis(500);
    c.per_ip_qps = 100.0;
    c.response_per_ip = 1000.0;
    c.response_rate = 10_000.0;
    c.vote_responders = 3;
    c.vote_prefixes = 1;
    c.sample = false;
    c
}

async fn announce(from: &Dht, to: SocketAddrV4, hash: [u8; 20]) {
    // The fake network answers well within the query timeout, but the shared test process
    // (D78) can be busy: a lost query is retried.
    let mut r = None;
    for _ in 0..5 {
        if let Ok(x) = from.query(0, to, Query::GetPeers { info_hash: hash }).await {
            r = Some(x);
            break;
        }
    }
    let token = r.expect("get_peers").token.expect("token");
    let mut announced = false;
    for _ in 0..5 {
        let q = Query::AnnouncePeer {
            info_hash: hash,
            port: 7777,
            implied_port: false,
            token: token.clone(),
        };
        if from.query(0, to, q).await.is_ok() {
            announced = true;
            break;
        }
    }
    assert!(announced, "announce");
}

fn opts(ports: usize) -> DhtOptions {
    DhtOptions {
        mode: Mode::Aggressive,
        ports: vec![0; ports],
        qps: 400,
        pool_cap: 10_000,
        fetch_known: false,
        peer_lookups: 8,
        lookup_timeout: Duration::from_millis(1500),
        peers_per_hash: 12,
        sample_priority: 0.0,
        dry_run: false,
        bloom_bits: 1 << 20,
        queue_cap: 1000,
        fetch_concurrency: 8,
        metadata_max: 1 << 20,
        tcp_rate: 1000,
        tcp_max: 350,
        probe: Vec::new(),
        screen: None,
        upnp: None,
        service: None,
        geo: None,
        pacer: None,
        health: None,
    }
}

fn tweak(ip: Ipv4Addr, boot: SocketAddrV4) -> impl FnOnce(&mut EngineConfig) {
    move |c: &mut EngineConfig| {
        c.bind = ip;
        c.bootstrap = vec![boot];
        c.allow_loopback = true;
        c.query_timeout = Duration::from_millis(500);
        c.per_ip_qps = 100.0;
        c.response_per_ip = 1000.0;
        c.response_rate = 10_000.0;
        c.vote_responders = 3;
        c.vote_prefixes = 1;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pipeline_against_a_fake_dht() {
    let Some(db) = test_db().await else { return };
    let pool = db.store.pool().clone();
    // 10 nodes on 127.0.5.x; a client announces 40 torrents to two nodes each.
    let mut net = Vec::new();
    let mut boot = Vec::new();
    for i in 0..10u8 {
        let ip = Ipv4Addr::new(127, 0, 5, 10 + i);
        let (d, _rx) = Dht::start(fake_cfg(ip, boot.clone())).await.unwrap();
        let a = SocketAddrV4::new(ip, d.local_ports()[0]);
        if boot.len() < 2 {
            boot.push(a);
        }
        net.push((d, a));
    }
    let client_ip = Ipv4Addr::new(127, 0, 6, 1);
    let (client, _crx) = Dht::start(fake_cfg(client_ip, vec![net[0].1]))
        .await
        .unwrap();
    let hashes: Vec<[u8; 20]> = (0..40).map(|_| rand::random()).collect();
    for (k, h) in hashes.iter().enumerate() {
        for j in [k % net.len(), (k * 3 + 1) % net.len()] {
            announce(&client, net[j].1, *h).await;
        }
    }
    // One of them is already known from Nostr (stored DHT row stands in for it).
    insert_dht_torrent(&pool, &hx(&hashes[0]), "already here", NOW).await;

    let crawler_ip = Ipv4Addr::new(127, 0, 7, 1);
    let (_tx, stop) = tokio::sync::watch::channel(false);
    let rep = dht::run(
        Some(pool.clone()),
        opts(2),
        stop,
        Some(Duration::from_secs(8)),
        tweak(crawler_ip, net[0].1),
    )
    .await
    .unwrap();
    eprintln!("{}", serde_json::to_string_pretty(&rep).unwrap());
    assert_eq!(rep.virtual_nodes, 2);
    assert!(rep.unique_hashes >= 40, "{rep:?}");
    assert!(rep.known >= 1, "{rep:?}");
    assert_eq!(rep.triage_errors, 0);
    // Every announced hash but the known one is persisted as queued, with its via.
    let rows: Vec<(Vec<u8>, i16, i16)> =
        sqlx::query_as("SELECT infohash, status, via FROM dht_seen")
            .fetch_all(&pool)
            .await
            .unwrap();
    let persisted: HashSet<Vec<u8>> = rows.iter().map(|r| r.0.clone()).collect();
    for h in &hashes[1..] {
        assert!(persisted.contains(h.as_slice()), "missing {}", hx(h));
    }
    assert!(
        !persisted.contains(hashes[0].as_slice()),
        "known hash queued"
    );
    assert!(rows.iter().all(|r| r.1 == 0 || r.1 == 2));
    // Peers were collected for most of them, all pointing at the announcing client.
    assert!(rep.peers.hashes >= 30, "{:?}", rep.peers);
    assert!(
        rep.peers.with_peers as f64 >= 0.8 * rep.peers.hashes as f64,
        "{:?}",
        rep.peers
    );
    // The BEP 42 identity and contacts were persisted on shutdown.
    let id = dstore::load_identity(&pool, &[0, 0])
        .await
        .unwrap()
        .expect("identity saved");
    assert_eq!(id.external_ip, crawler_ip);
    assert!(!dstore::load_nodes(&pool).await.unwrap().is_empty());

    // A second run (fresh Bloom filter) dedupes against dht_seen: nothing queued again.
    let (_tx, stop) = tokio::sync::watch::channel(false);
    let rep2 = dht::run(
        Some(pool.clone()),
        opts(2),
        stop,
        Some(Duration::from_secs(5)),
        tweak(crawler_ip, net[0].1),
    )
    .await
    .unwrap();
    assert!(rep2.unique_hashes >= 30, "{rep2:?}");
    assert_eq!(rep2.queued, 0, "{rep2:?}");

    // Clean shutdown on request: returns promptly, sockets released.
    let (tx, stop) = tokio::sync::watch::channel(false);
    let t0 = std::time::Instant::now();
    let h = tokio::spawn(dht::run(
        Some(pool.clone()),
        opts(1),
        stop,
        None,
        tweak(crawler_ip, net[0].1),
    ));
    tokio::time::sleep(Duration::from_secs(1)).await;
    tx.send(true).unwrap();
    let rep3 = tokio::time::timeout(Duration::from_secs(8), h)
        .await
        .expect("stops")
        .unwrap()
        .unwrap();
    assert!(t0.elapsed() < Duration::from_secs(8));
    assert_eq!(rep3.virtual_nodes, 1);

    client.shutdown().await;
    for (d, _) in net {
        d.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dry_run_touches_no_database() {
    // No database at all: the pipeline dedupes in memory and still reports.
    let mut net = Vec::new();
    let mut boot = Vec::new();
    for i in 0..4u8 {
        let ip = Ipv4Addr::new(127, 0, 8, 10 + i);
        let (d, _rx) = Dht::start(fake_cfg(ip, boot.clone())).await.unwrap();
        let a = SocketAddrV4::new(ip, d.local_ports()[0]);
        boot.push(a);
        net.push((d, a));
    }
    let (client, _crx) = Dht::start(fake_cfg(Ipv4Addr::new(127, 0, 8, 1), vec![net[0].1]))
        .await
        .unwrap();
    for k in 0..40usize {
        announce(&client, net[k % 4].1, rand::random()).await;
    }
    // A tiny queue: the burst of 40 overflows it (in a dry run nothing keeps a lease).
    let mut o = opts(1);
    o.dry_run = true;
    o.queue_cap = 8;
    o.peer_lookups = 1;
    let (_tx, stop) = tokio::sync::watch::channel(false);
    let rep = dht::run(
        None,
        o,
        stop,
        Some(Duration::from_secs(4)),
        tweak(Ipv4Addr::new(127, 0, 8, 2), net[0].1),
    )
    .await
    .unwrap();
    assert!(rep.dry_run);
    assert!(rep.unique_hashes >= 8, "{rep:?}");
    assert!(rep.queue_overflow > 0, "{rep:?}");
    assert!(rep.peers.hashes >= 1, "{rep:?}");
    client.shutdown().await;
    for (d, _) in net {
        d.shutdown().await;
    }
}

/// D93 bug: a client sends get_peers, then announce_peer for the same hash (what every client
/// does). The get_peers sighting passed the seen filter first, so the announce, the only one
/// carrying a live peer, was dropped there: 0 rows `via = announce` in production. Now the
/// queued hash moves to the announce lane with that peer (or the peer joins its collection),
/// and the fetch is handed the announcer.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn announce_after_get_peers_keeps_its_peer() {
    let Some(db) = test_db().await else { return };
    let pool = db.store.pool().clone();
    let crawler_ip = Ipv4Addr::new(127, 0, 9, 1);
    // Nothing else on the network: only the announces can give peers.
    let mut o = opts(1);
    o.peer_lookups = 1;
    o.fetch_concurrency = 1;
    let (_tx, stop) = tokio::sync::watch::channel(false);
    // A fixed port for the crawler, so the client can reach it.
    let port = {
        let s = std::net::UdpSocket::bind((crawler_ip, 0)).unwrap();
        s.local_addr().unwrap().port()
    };
    o.ports = vec![port];
    let run = tokio::spawn(dht::run(
        Some(pool.clone()),
        o,
        stop,
        Some(Duration::from_secs(8)),
        move |c: &mut EngineConfig| {
            c.bind = crawler_ip;
            c.bootstrap = vec![SocketAddrV4::new(Ipv4Addr::new(127, 0, 9, 99), 9)];
            c.allow_loopback = true;
            c.query_timeout = Duration::from_secs(3);
            c.lookup_timeout = Duration::from_millis(100);
            c.per_ip_qps = 100.0;
            c.response_per_ip = 1000.0;
            c.response_rate = 10_000.0;
            c.vote_responders = 3;
            c.vote_prefixes = 1;
        },
    ));
    tokio::time::sleep(Duration::from_millis(500)).await;
    let (client, _crx) = Dht::start(fake_cfg(Ipv4Addr::new(127, 0, 9, 2), vec![]))
        .await
        .unwrap();
    let to = SocketAddrV4::new(crawler_ip, port);
    let hashes: Vec<[u8; 20]> = (0..6).map(|_| rand::random()).collect();
    let get_peers = |h: [u8; 20]| {
        let client = client.clone();
        async move {
            client
                .query(0, to, Query::GetPeers { info_hash: h })
                .await
                .expect("get_peers")
                .token
                .expect("token")
        }
    };
    let announce_with = |h: [u8; 20], token: Vec<u8>| {
        let client = client.clone();
        async move {
            client
                .query(
                    0,
                    to,
                    Query::AnnouncePeer {
                        info_hash: h,
                        port: 7777,
                        implied_port: false,
                        token,
                    },
                )
                .await
                .expect("announce");
        }
    };
    // Three get_peers first: after the triage batch (1 s) the first is being collected (its
    // lookup waits 3 s on a silent router), the other two wait in the lowest lane...
    let mut tokens = Vec::new();
    for h in &hashes[..3] {
        tokens.push(get_peers(*h).await);
    }
    tokio::time::sleep(Duration::from_millis(1500)).await;
    // ...when their announces arrive (one joins the collection, two move up a lane).
    for (h, t) in hashes[..3].iter().zip(tokens) {
        announce_with(*h, t).await;
    }
    // Three more back to back: both sightings land in the same triage batch.
    for h in &hashes[3..] {
        let t = get_peers(*h).await;
        announce_with(*h, t).await;
    }
    let rep = run.await.unwrap().unwrap();
    eprintln!("{}", serde_json::to_string_pretty(&rep.peers).unwrap());
    let p = &rep.peers;
    assert_eq!(
        (
            p.announce_attached,
            p.announce_upgraded,
            p.announce_retriaged
        ),
        (1, 2, 3),
        "{p:?}"
    );
    assert_eq!(
        p.announce_fetches, 6,
        "every hash fetched from its announcer: {p:?}"
    );
    assert_eq!(rep.fetch.hashes, 6, "{:?}", rep.fetch);
    // Fetches fail (nothing listens on 7777): the rows record their announce origin.
    let vias: Vec<i16> = sqlx::query_scalar("SELECT via FROM dht_seen")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(vias.len(), 6);
    assert!(vias.iter().all(|v| *v == 1), "{vias:?}");
    client.shutdown().await;
}

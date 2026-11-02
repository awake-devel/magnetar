//! Header-probe backfill (D118, docs/PROBES.md §10) against a scratch schema: the queue is
//! filled from every indexed infohash (DHT and Nostr-only) without probed, blocked or `porn`
//! rows, in value order (known seeders first, then newest), the full adult rule is applied at
//! lease time, leases are exclusive and come back on release; a leased hash counts as held for
//! `dht/results` (the dict is stored for a Nostr-only torrent, an existing DHT row is never
//! rewritten, the probe is stored and the lease closed), unreachable hashes are retried with
//! backoff then closed; then an in-process worker with DHT discovery off and backfill on probes
//! a Nostr-only torrent and a stored DHT torrent from loopback fake seeders on a fake DHT, with
//! no sampling and no triage. Needs MAGNETAR_TEST_DATABASE_URL (else SKIPPED). Loopback only.

use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddrV4};
use std::time::Duration;

use magnetar_core::dht::Dht;
use magnetar_core::probe::ProbeConfig;
use magnetar_server::fleet::proto::*;
use magnetar_server::probe_backfill::{self as bf, Answer};
use magnetar_server::store::PgStore;
use magnetar_server::worker::client::Client;
use magnetar_server::worker::{self};
use serde_json::Value as J;

use crate::common::*;
use crate::dht_fetch_pg::{announce, fake_cfg, info, sha1};
use crate::fake_seeder::{self as fs, Opts, Torrent};
use crate::probe_pg::{detail, dht_row, mkv, mkv_file, report, with_probe};
use crate::workers::{W1, W2, count, http, opts, spawn, tweak, upload, worker_dht};

async fn seeders(s: &PgStore, h: &str, n: Option<i32>) {
    sqlx::query(
        "INSERT INTO liveness (infohash, seeders, leechers, completed, dht_peers_est, source,
             trackers_ok, trackers_failed, checked_at, expires_at)
         VALUES ($1, $2, 0, 0, NULL, 'tracker', 1, 0, $3, $3)",
    )
    .bind(h)
    .bind(n)
    .bind(NOW)
    .execute(s.pool())
    .await
    .unwrap();
}

/// A Nostr torrent from publisher 7, at a trust a default search shows (50 >= 20).
async fn nostr(s: &PgStore, id: u64, h: &str, title: &str, created: i64, files: &[(&str, u64)]) {
    nostr_from(s, id, 7, 50, h, title, created, files).await;
}

#[allow(clippy::too_many_arguments)]
async fn nostr_from(
    s: &PgStore,
    id: u64,
    pubkey: u64,
    trust: i16,
    h: &str,
    title: &str,
    created: i64,
    files: &[(&str, u64)],
) {
    let mut t = torrent(id, pubkey, created, 0, title).files(files);
    t.p.infohash = h.to_string();
    let (p, _) = put(s, t).await;
    sqlx::query("UPDATE torrents SET trust = $2 WHERE event_id = $1")
        .bind(&p.event_id)
        .bind(trust)
        .execute(s.pool())
        .await
        .unwrap();
}

async fn block_publisher(s: &PgStore, pubkey: u64) {
    sqlx::query(
        "INSERT INTO local_overrides (pubkey, action, created_at) VALUES ($1, 'block', $2)",
    )
    .bind(hex64(pubkey))
    .bind(NOW)
    .execute(s.pool())
    .await
    .unwrap();
}

async fn search(base: &str, q: &str) -> Vec<J> {
    let j: J = http()
        .get(format!("{base}/api/v1/search?{q}"))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    j["hits"].as_array().cloned().unwrap_or_default()
}

async fn series(s: &PgStore, metric: &str) -> i64 {
    sqlx::query_scalar("SELECT coalesce(sum(value), 0)::bigint FROM stats_series WHERE metric = $1")
        .bind(metric)
        .fetch_one(s.pool())
        .await
        .unwrap()
}

async fn queue_row(
    s: &PgStore,
    h: &str,
) -> Option<(i64, Option<i64>, i16, Option<String>, Option<String>)> {
    sqlx::query_as(
        "SELECT rank, due_at, attempts, last_outcome, worker FROM probe_backfill WHERE infohash = $1",
    )
    .bind(h)
    .fetch_optional(s.pool())
    .await
    .unwrap()
}

async fn probed(s: &PgStore, h: &str) {
    sqlx::query(
        "INSERT INTO probe_attrs (infohash, probe_version, probed_at, status, files_probed,
             files_verified, bytes_in) VALUES ($1, 1, $2, 'ok', 1, 1, 1000)",
    )
    .bind(h)
    .bind(NOW)
    .execute(s.pool())
    .await
    .unwrap();
}

/// The queue: who is in it, in which order, what a lease closes, exclusivity and release.
#[tokio::test]
async fn queue_fills_in_value_order_and_leases_exclusively() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let now = magnetar_core::now();
    let (a, b, c, d, e, f, g, dead, newer) = (
        ih(0xa1),
        ih(0xb1),
        ih(0xc1),
        ih(0xd1),
        ih(0xe1),
        ih(0xf1),
        ih(0x91),
        ih(0x81),
        ih(0x71),
    );
    // a: Nostr only, 50 seeders; b: DHT, 3 seeders; newer: DHT, 3 seeders, born later;
    // c: DHT, never checked; dead: Nostr, 0 seeders.
    nostr(
        s,
        1,
        &a,
        "Some.Film.2020.1080p",
        NOW - 5000,
        &[("f.mkv", 10)],
    )
    .await;
    seeders(s, &a, Some(50)).await;
    dht_row(s, &b, "Dht.Show.S01", "video", &[("e1.mkv", 10)]).await;
    seeders(s, &b, Some(3)).await;
    dht_row(s, &newer, "Dht.Show.S02", "video", &[("e1.mkv", 10)]).await;
    sqlx::query("UPDATE liveness_schedule SET born = born + 100 WHERE infohash = $1")
        .bind(&newer)
        .execute(s.pool())
        .await
        .unwrap();
    seeders(s, &newer, Some(2)).await;
    dht_row(s, &c, "Dht.Album.2019", "audio", &[("01.flac", 10)]).await;
    nostr(s, 2, &dead, "Dead.Film.1999", NOW - 9000, &[]).await;
    seeders(s, &dead, Some(0)).await;
    // d: Nostr, adult by its name only (not category porn): queued, closed at lease time.
    nostr(s, 3, &d, "Studio.XXX.Scene.2024", NOW - 100, &[]).await;
    // e: DHT, category porn; f: blocked; g: already probed. Never queued.
    dht_row(s, &e, "Something", "porn", &[("x.mp4", 10)]).await;
    nostr(s, 4, &f, "Blocked.Film", NOW, &[]).await;
    sqlx::query(
        "INSERT INTO blocked_infohashes (infohash, blocked_at, rule_version, origin)
         VALUES ($1, $2, 1, 'nostr')",
    )
    .bind(&f)
    .bind(NOW)
    .execute(s.pool())
    .await
    .unwrap();
    dht_row(s, &g, "Probed.Film", "video", &[("p.mkv", 10)]).await;
    probed(s, &g).await;

    let n = bf::fill_pass(s.pool()).await.unwrap();
    assert_eq!(n, 6, "a, b, newer, c, dead, d");
    for h in [&e, &f, &g] {
        assert!(queue_row(s, h).await.is_none(), "{h} not queued");
    }
    // The SQL rank is the documented one.
    let born = |h: &str| {
        let h = h.to_string();
        async move {
            sqlx::query_scalar::<_, i64>("SELECT born FROM liveness_schedule WHERE infohash = $1")
                .bind(h)
                .fetch_one(s.pool())
                .await
                .unwrap()
        }
    };
    for (h, peers) in [(&a, Some(50)), (&b, Some(3)), (&c, None), (&dead, Some(0))] {
        let (rank, due, ..) = queue_row(s, h).await.unwrap();
        assert_eq!(rank, bf::rank(peers, born(h).await), "{h}");
        assert_eq!(due, Some(rank));
    }
    // Filling again changes nothing.
    assert_eq!(bf::fill_pass(s.pool()).await.unwrap(), 0);

    // Value order: 50 seeders, then the 2–3 tier (newest first), never checked, dead.
    let l = bf::lease(s.pool(), "w1", now, 4, 900).await.unwrap();
    assert_eq!(
        l.items,
        vec![a.clone(), newer.clone(), b.clone(), c.clone()]
    );
    let l2 = bf::lease(s.pool(), "w2", now, 10, 900).await.unwrap();
    assert_eq!(l2.items, vec![dead.clone()], "never a hash leased to w1");
    assert_eq!(l2.closed, 1, "the adult row is closed, not leased");
    let (_, due, _, out, w) = queue_row(s, &d).await.unwrap();
    assert_eq!((due, out.as_deref(), w), (None, Some("adult"), None));
    assert!(
        bf::lease(s.pool(), "w2", now, 10, 900)
            .await
            .unwrap()
            .items
            .is_empty()
    );

    // Holds: only the worker it was leased to.
    let raw = |h: &str| -> [u8; 20] { parse_hash(h).unwrap() };
    let held = bf::held(s.pool(), "w1", &[raw(&a), raw(&dead)])
        .await
        .unwrap();
    assert!(held.contains(&raw(&a)) && !held.contains(&raw(&dead)));
    // A release puts a never-tried hash back in its first place; another worker takes it.
    assert_eq!(
        bf::release(s.pool(), "w2", std::slice::from_ref(&c), now)
            .await
            .unwrap(),
        0,
        "not w2's"
    );
    assert_eq!(
        bf::release(s.pool(), "w1", std::slice::from_ref(&c), now)
            .await
            .unwrap(),
        1
    );
    let (rank, due, ..) = queue_row(s, &c).await.unwrap();
    assert_eq!(due, Some(rank));
    assert_eq!(
        bf::lease(s.pool(), "w2", now, 10, 900).await.unwrap().items,
        vec![c.clone()]
    );
    // Expired leases come back for anyone.
    let later = bf::lease(s.pool(), "w2", now + 901, 10, 900).await.unwrap();
    assert_eq!(later.items.len(), 5, "{later:?}");

    // A probe stored meanwhile (a new find's probe): the row is deleted at the next lease.
    let p = ih(0x61);
    dht_row(s, &p, "Late.Probe", "video", &[("l.mkv", 10)]).await;
    bf::fill_pass(s.pool()).await.unwrap();
    probed(s, &p).await;
    let l = bf::lease(s.pool(), "w1", now + 2000, 10, 900)
        .await
        .unwrap();
    assert!(!l.items.contains(&p));
    assert!(queue_row(s, &p).await.is_none());

    // Retries: backoff after each attempt, closed after the last one; answered once.
    let r = bf::record(
        s.pool(),
        "w2",
        &[(dead.clone(), Answer::Retry("no_peers".into()))],
        now,
    )
    .await
    .unwrap();
    assert_eq!(
        (r.retried, r.closed),
        (0, 0),
        "dead is leased to w1 now, not w2"
    );
    let mut t = now + 3000;
    for attempt in 1..=bf::MAX_ATTEMPTS {
        let l = bf::lease(s.pool(), "w1", t, 10, 900).await.unwrap();
        assert!(l.items.contains(&dead), "attempt {attempt}: {l:?}");
        let ans = vec![(dead.clone(), Answer::Retry("no_peers".into()))];
        let r = bf::record(s.pool(), "w1", &ans, t).await.unwrap();
        let (_, due, n, out, w) = queue_row(s, &dead).await.unwrap();
        assert_eq!((n, out.as_deref(), w), (attempt, Some("no_peers"), None));
        if attempt < bf::MAX_ATTEMPTS {
            assert_eq!(r.retried, 1);
            assert_eq!(
                due,
                Some(t + bf::BACKOFF_SECS[(attempt as usize - 1).min(2)])
            );
            assert_eq!(
                bf::record(s.pool(), "w1", &ans, t).await.unwrap().retried,
                0,
                "once"
            );
            t = due.unwrap();
        } else {
            assert_eq!((r.closed, due), (1, None), "closed after the last attempt");
        }
    }
    let st = bf::stats(s.pool(), t).await.unwrap();
    assert!(
        st.closed.iter().any(|(o, n)| o == "no_peers" && *n == 1),
        "{st:?}"
    );
    assert!(
        st.closed.iter().any(|(o, n)| o == "adult" && *n == 1),
        "{st:?}"
    );
    assert_eq!(st.probed, 2);
}

/// `probe/lease` and the hold rule on `dht/results`: a Nostr-only torrent gets its DHT row and
/// probe, an existing DHT row keeps its data and gets the probe, a dead hash is retried later;
/// another worker's upload for the same hashes is ignored, and so is a second answer.
#[tokio::test]
async fn leased_hashes_are_held_stored_probed_and_closed() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let (base, _) = spawn(s, 900).await;
    let c1 = Client::new(&base, W1).unwrap();
    let c2 = Client::new(&base, W2).unwrap();

    // n: Nostr only; x: a DHT row stored long ago (its title differs from the dict's name);
    // dead: Nostr only, nobody seeds it.
    let n_raw = info("Backfill.Nostr.Film.2021.1080p", "0");
    let n = hex::encode(sha1(&n_raw));
    nostr(
        s,
        11,
        &n,
        "Backfill Nostr Film (2021)",
        NOW,
        &[("Backfill.Nostr.Film.2021.1080p.mkv", 1_500_000_000)],
    )
    .await;
    let x_raw = info("Backfill.Dht.Film.2022.2160p", "0");
    let x = hex::encode(sha1(&x_raw));
    dht_row(
        s,
        &x,
        "Stored.Title.Kept",
        "video",
        &[("Backfill.Dht.Film.2022.2160p.mkv", 1_500_000_000)],
    )
    .await;
    let dead = ih(0xdead);
    nostr(s, 12, &dead, "Nobody.Seeds.This", NOW, &[]).await;

    // The first lease starts the fill in the background: lease until it is done.
    let mut got: Vec<String> = Vec::new();
    for _ in 0..100 {
        let l: ProbeLease = c1
            .post(PROBE_LEASE, &LeaseRequest { max: 10 })
            .await
            .unwrap();
        got.extend(l.infohashes);
        if got.len() >= 3 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    got.sort();
    let mut want = vec![n.clone(), x.clone(), dead.clone()];
    want.sort();
    assert_eq!(got, want);
    let none: ProbeLease = c2
        .post(PROBE_LEASE, &LeaseRequest { max: 10 })
        .await
        .unwrap();
    assert!(none.infohashes.is_empty(), "all leased to w1");

    let probe = |raw: &[u8]| with_probe(sha1(raw), raw, report(vec![mkv_file(0, 1_500_000_000)]));
    // w2 holds none of them: everything is ignored.
    let a = upload(&c2, vec![probe(&n_raw)]).await;
    assert_eq!((a.ignored, a.stored, a.probe_stored), (1, 0, 0));

    let a = upload(
        &c1,
        vec![
            probe(&n_raw),
            probe(&x_raw),
            DhtResult {
                sighted: dead.clone(),
                via: "get_peers".into(),
                peers_seen: 0,
                outcome: DhtOutcome::Failed {
                    code: "no_peers".into(),
                },
                probe: None,
            },
        ],
    )
    .await;
    assert_eq!(
        (
            a.stored,
            a.backfill_stored,
            a.not_new,
            a.probe_stored,
            a.applied,
            a.ignored
        ),
        (0, 1, 1, 2, 1, 0),
        "{a:?}"
    );
    // The Nostr-only torrent has its DHT row now (origin both), supplied by w1.
    let (title, by): (String, Option<String>) =
        sqlx::query_as("SELECT title, supplied_by FROM dht_torrents WHERE infohash = $1")
            .bind(&n)
            .fetch_one(s.pool())
            .await
            .unwrap();
    assert_eq!(
        (title.as_str(), by.as_deref()),
        ("Backfill.Nostr.Film.2021.1080p", Some("w1"))
    );
    // The stored row is not rewritten.
    let (title, by): (String, Option<String>) =
        sqlx::query_as("SELECT title, supplied_by FROM dht_torrents WHERE infohash = $1")
            .bind(&x)
            .fetch_one(s.pool())
            .await
            .unwrap();
    assert_eq!((title.as_str(), by), ("Stored.Title.Kept", None));
    // Both probed and shown; their queue rows are gone.
    for h in [&n, &x] {
        assert_eq!(
            count(s, "SELECT count(*) FROM probe_attrs WHERE infohash = $1", h).await,
            1,
            "{h}"
        );
        assert!(queue_row(s, h).await.is_none());
    }
    let d = detail(&base, &n).await;
    assert_eq!(d["probe"]["files"][0]["label"], "video.mkv", "{d}");
    assert_eq!(d["probe"]["files"][0]["index"], 0);
    // The dead hash waits for a retry; a second answer for it is ignored (lease consumed).
    let (_, due, attempts, out, w) = queue_row(s, &dead).await.unwrap();
    assert_eq!((attempts, out.as_deref(), w), (1, Some("no_peers"), None));
    assert!(due.unwrap() > magnetar_core::now() + bf::BACKOFF_SECS[0] - 60);
    let again = DhtResult {
        sighted: dead.clone(),
        via: "get_peers".into(),
        peers_seen: 0,
        outcome: DhtOutcome::Failed {
            code: "no_peers".into(),
        },
        probe: None,
    };
    assert_eq!(upload(&c1, vec![again]).await.ignored, 1);
    // A backfill status never reaches dht_seen.
    assert_eq!(
        count(
            s,
            "SELECT count(*) FROM dht_seen WHERE encode(infohash, 'hex') = $1",
            &dead
        )
        .await,
        0
    );

    // Nothing else is due.
    let l: ProbeLease = c1
        .post(PROBE_LEASE, &LeaseRequest { max: 10 })
        .await
        .unwrap();
    assert!(l.infohashes.is_empty(), "nothing due: {l:?}");

    // Progress in /api/v1/stats.
    let st: J = http()
        .get(format!("{base}/api/v1/stats"))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let pb = &st["probe_backfill"];
    assert_eq!(pb["retrying"], 1, "{pb}");
    assert_eq!(pb["remaining"], 1);
    assert_eq!(pb["probed"], 2);
    let w1 = st["workers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["name"] == "w1")
        .unwrap();
    assert_eq!(w1["backfill_leased_total"], 3, "{w1}");
    assert_eq!(w1["backfill_probed_total"], 2);
    assert_eq!(w1["backfill_unprobed_total"], 1);
}

/// Review of D118: a backfill DHT row must not bring back a torrent no default search shows
/// (blocked publisher, below the trust threshold), must keep the torrent's Nostr age (no new
/// hit for `since` or the saved-search poll), and is not a DHT discovery (stats series, the
/// worker's `stored_total`).
#[tokio::test]
async fn backfill_keeps_moderation_age_and_discovery_counts() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let (base, _) = spawn(s, 900).await;
    let c1 = Client::new(&base, W1).unwrap();
    let old = NOW - 10_000_000;

    // ok: shown (trust 50); late: shown at lease time, its publisher blocked before the upload;
    // spam: blocked publisher; low: below the threshold; trusted: low trust, trusted publisher.
    let ok_raw = info("Review.Okfilm.2020.1080p", "0");
    let ok = hex::encode(sha1(&ok_raw));
    nostr_from(
        s,
        21,
        21,
        50,
        &ok,
        "Review Okfilm",
        old,
        &[("Review.Okfilm.2020.1080p.mkv", 1_500_000_000)],
    )
    .await;
    let late_raw = info("Review.Latefilm.2020.1080p", "0");
    let late = hex::encode(sha1(&late_raw));
    nostr_from(
        s,
        22,
        22,
        50,
        &late,
        "Review Latefilm",
        old,
        &[("Review.Latefilm.2020.1080p.mkv", 1_500_000_000)],
    )
    .await;
    let spam = ih(0x5a);
    nostr_from(s, 23, 23, 50, &spam, "Review Spamfilm", old, &[]).await;
    block_publisher(s, 23).await;
    let low = ih(0x5b);
    nostr_from(s, 24, 24, 5, &low, "Review Lowfilm", old, &[]).await;
    let trusted = ih(0x5c);
    nostr_from(s, 25, 25, 5, &trusted, "Review Trustedfilm", old, &[]).await;
    sqlx::query(
        "INSERT INTO local_overrides (pubkey, action, created_at) VALUES ($1, 'trust', $2)",
    )
    .bind(hex64(25))
    .bind(NOW)
    .execute(s.pool())
    .await
    .unwrap();

    // Only shown torrents are queued.
    bf::fill_pass(s.pool()).await.unwrap();
    for h in [&spam, &low] {
        assert!(queue_row(s, h).await.is_none(), "{h} not queued");
    }
    for h in [&ok, &late, &trusted] {
        assert!(queue_row(s, h).await.is_some(), "{h} queued");
    }
    // A row queued before its torrent was hidden leaves the queue at lease time.
    sqlx::query("INSERT INTO probe_backfill (infohash, rank, due_at) VALUES ($1, -5, -5)")
        .bind(&low)
        .execute(s.pool())
        .await
        .unwrap();
    let mut got: Vec<String> = Vec::new();
    for _ in 0..100 {
        let l: ProbeLease = c1
            .post(PROBE_LEASE, &LeaseRequest { max: 10 })
            .await
            .unwrap();
        got.extend(l.infohashes);
        if got.len() >= 3 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    got.sort();
    let mut want = vec![ok.clone(), late.clone(), trusted.clone()];
    want.sort();
    assert_eq!(got, want);
    assert!(
        queue_row(s, &low).await.is_none(),
        "hidden: dropped from the queue"
    );

    // late's publisher is blocked while it is leased: its dict is not stored.
    block_publisher(s, 22).await;
    let probe = |raw: &[u8]| with_probe(sha1(raw), raw, report(vec![mkv_file(0, 1_500_000_000)]));
    let a = upload(&c1, vec![probe(&ok_raw), probe(&late_raw)]).await;
    assert_eq!(
        (a.stored, a.backfill_stored, a.probe_stored, a.applied),
        (0, 1, 1, 1),
        "{a:?}"
    );
    assert_eq!(
        count(
            s,
            "SELECT count(*) FROM dht_torrents WHERE infohash = $1",
            &late
        )
        .await,
        0
    );
    assert!(
        queue_row(s, &late).await.is_none(),
        "dropped, queued again once shown"
    );
    for q in ["q=latefilm", "q=spamfilm", "q=lowfilm"] {
        assert!(search(&base, q).await.is_empty(), "{q}");
    }

    // ok keeps its Nostr age: the DHT row's first_seen is the event's, so it is no new hit.
    let first: i64 = sqlx::query_scalar("SELECT first_seen FROM dht_torrents WHERE infohash = $1")
        .bind(&ok)
        .fetch_one(s.pool())
        .await
        .unwrap();
    assert_eq!(first, old);
    assert_eq!(search(&base, "q=okfilm").await.len(), 1);
    let since = format!("q=okfilm&since={}", NOW - 1000);
    assert!(search(&base, &since).await.is_empty(), "not new");

    // Not a discovery: its own series, not dht_torrents_added / dht_stored_*; not stored_total.
    assert_eq!(series(s, bf::DHT_ROWS_SERIES).await, 1);
    assert_eq!(series(s, "dht_torrents_added").await, 0);
    assert_eq!(series(s, "dht_stored_get_peers").await, 0);
    let st: J = http()
        .get(format!("{base}/api/v1/stats"))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let w1 = st["workers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["name"] == "w1")
        .unwrap();
    assert_eq!(w1["stored_total"], 0, "{w1}");
}

/// Review of D118: a worker whose metadata limit is below the server's reports oversize for a
/// leased torrent; the lease is retried (for a worker with room), not closed for good.
#[tokio::test]
async fn small_worker_oversize_is_retried_not_closed() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let (base, _) = spawn(s, 900).await;
    let c1 = Client::new(&base, W1).unwrap();
    let _: HelloResponse = c1
        .post(
            HELLO,
            &Hello {
                version: env!("CARGO_PKG_VERSION").into(),
                metadata_max: 1 << 10,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let big = ih(0xb16);
    nostr(s, 31, &big, "Big.Dict.Film", NOW, &[]).await;
    let mut got: Vec<String> = Vec::new();
    for _ in 0..100 {
        let l: ProbeLease = c1
            .post(PROBE_LEASE, &LeaseRequest { max: 10 })
            .await
            .unwrap();
        got.extend(l.infohashes);
        if !got.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(got, vec![big.clone()]);
    let item = DhtResult {
        sighted: big.clone(),
        via: "get_peers".into(),
        peers_seen: 3,
        outcome: DhtOutcome::Oversize,
        probe: None,
    };
    assert_eq!(upload(&c1, vec![item]).await.applied, 1);
    let (_, due, attempts, out, w) = queue_row(s, &big).await.unwrap();
    assert_eq!((attempts, out.as_deref(), w), (1, Some("oversize"), None));
    assert!(due.is_some(), "retried later, not closed");
}

/// One in-process worker with DHT discovery off and backfill on, on a fake DHT with two
/// loopback seeders: a Nostr-only torrent and a stored DHT one are leased, looked up, fetched,
/// probed and stored; the worker never samples and never triages.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn worker_backfills_without_discovering() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let (base, bodies) = spawn(s, 900).await;

    let mut net = Vec::new();
    let mut boot = Vec::new();
    for i in 0..6u8 {
        let ip = Ipv4Addr::new(127, 0, 71, 10 + i);
        let (d, _rx) = Dht::start(fake_cfg(ip, boot.clone())).await.unwrap();
        let a = SocketAddrV4::new(ip, d.local_ports()[0]);
        if boot.len() < 2 {
            boot.push(a);
        }
        net.push((d, a));
    }
    let seeder_ip = Ipv4Addr::new(127, 0, 72, 1);
    let kib = 1024u64;
    let from_nostr = Torrent::new(
        "x",
        vec![("Backfill.Nostr.2024.2160p.mkv", mkv(2_000_000))],
        256 * kib,
    );
    let from_dht = Torrent::new(
        "x",
        vec![("Backfill.Dht.2023.1080p.mkv", mkv(2_000_000))],
        256 * kib,
    );
    let mut seeders = HashMap::new();
    for (name, t) in [("nostr", &from_nostr), ("dht", &from_dht)] {
        seeders.insert(
            name,
            (
                t.hash,
                fs::start_at(seeder_ip, t.clone(), Opts::default()).await,
            ),
        );
    }
    let hex_of = |n: &str| hex::encode(seeders[n].0);
    nostr(
        s,
        21,
        &hex_of("nostr"),
        "Backfill Nostr 2024",
        NOW,
        &[("Backfill.Nostr.2024.2160p.mkv", 2_000_000)],
    )
    .await;
    dht_row(
        s,
        &hex_of("dht"),
        "Backfill.Dht.2023.1080p.mkv",
        "video",
        &[("Backfill.Dht.2023.1080p.mkv", 2_000_000)],
    )
    .await;
    let (client, _crx) = Dht::start(fake_cfg(seeder_ip, vec![net[0].1]))
        .await
        .unwrap();
    for (k, (h, sd)) in seeders.values().enumerate() {
        announce(&client, net[k % net.len()].1, *h, sd.addr.port()).await;
    }

    let mut o = opts(&base, W1, Some(worker_dht(vec![0])), None);
    o.discover = false;
    o.backfill = true;
    o.probe = Some(ProbeConfig {
        per_min: 6_000_000,
        parallel: 4,
        unchoke_wait: Duration::from_secs(2),
        stall: Duration::from_secs(2),
        session: Duration::from_secs(8),
        second_peer_after: Duration::from_millis(300),
        upload_linger: Duration::from_millis(300),
        ..ProbeConfig::default()
    });
    let dir = o.state_dir.clone();
    let (_tx, stop) = tokio::sync::watch::channel(false);
    let r = worker::run(o, stop, Some(Duration::from_secs(18)), {
        let all: Vec<SocketAddrV4> = net.iter().map(|(_, a)| *a).collect();
        let base = tweak(Ipv4Addr::new(127, 0, 73, 1), net[0].1);
        move |c: &mut magnetar_core::dht::EngineConfig| {
            base(c);
            c.bootstrap = all;
        }
    })
    .await
    .unwrap();
    client.shutdown().await;
    let rep = serde_json::to_string(&r).unwrap();
    assert!(r.error.is_none(), "{rep}");
    let dht = r.dht.as_ref().unwrap();
    assert!(!dht.discover);
    assert_eq!(dht.engine.sent_sample_infohashes, 0, "no sampling: {rep}");
    assert_eq!(dht.triaged, 0, "no triage");
    let b = dht.backfill.as_ref().expect("backfill stats");
    assert!(b.leased >= 2 && b.probed >= 2, "{rep}");
    for n in ["nostr", "dht"] {
        assert_eq!(
            count(
                s,
                "SELECT count(*) FROM probe_attrs WHERE infohash = $1",
                &hex_of(n)
            )
            .await,
            1,
            "{n}: {rep}"
        );
        assert!(queue_row(s, &hex_of(n)).await.is_none(), "{n} lease closed");
        assert!(seeders[n].1.log().requests > 0);
    }
    let by: Option<String> =
        sqlx::query_scalar("SELECT supplied_by FROM dht_torrents WHERE infohash = $1")
            .bind(hex_of("nostr"))
            .fetch_one(s.pool())
            .await
            .unwrap();
    assert_eq!(by.as_deref(), Some("w1"));
    // On the wire: no triage request at all, the dicts went up on dht/results with probes.
    let bodies = bodies.lock().unwrap();
    assert!(bodies.iter().all(|(p, _)| p != TRIAGE), "a triage was sent");
    assert!(
        bodies
            .iter()
            .filter(|(p, _)| p == DHT_RESULTS)
            .flat_map(|(_, b)| serde_json::from_slice::<DhtResults>(b).unwrap().items)
            .any(|i| i.probe.is_some())
    );
    let _ = std::fs::remove_dir_all(dir);
}

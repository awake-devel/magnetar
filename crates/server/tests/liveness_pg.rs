//! Seed-check scheduling, history, stats and API against Postgres (D45). Trackers are
//! in-process fakes. Needs MAGNETAR_TEST_DATABASE_URL (else SKIPPED).

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use common::*;
use magnetar_core::api::ApiClient;
use magnetar_core::liveness::{LivenessConfig, TTL_ERROR, TTL_NEW, TTL_OLD};
use magnetar_core::model::*;
use magnetar_core::query::{SearchQuery, Sort};
use magnetar_core::store::Store;
use magnetar_server::PgStore;
use magnetar_server::liveness::{
    self as live, Expiry, HISTORY_STEP, LivenessService, PRIO_SAVED, PRIO_SEARCH, PRIO_VIEWED,
};
use magnetar_server::search_index::{self, SearchIndex, load};
use tokio::net::UdpSocket;

const DAY: i64 = 86400;

/// BEP 15 fake: answers every hash with `seeders(hash)` seeders; counts scrape packets.
async fn fake_tracker(seeders: fn(&[u8]) -> u32) -> (String, Arc<AtomicUsize>) {
    let sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let url = format!("udp://{}/announce", sock.local_addr().unwrap());
    let scrapes = Arc::new(AtomicUsize::new(0));
    let n = scrapes.clone();
    tokio::spawn(async move {
        let mut buf = [0u8; 4096];
        loop {
            let (len, src) = sock.recv_from(&mut buf).await.unwrap();
            let req = &buf[..len];
            let action = u32::from_be_bytes(req[8..12].try_into().unwrap());
            let mut resp = Vec::new();
            resp.extend_from_slice(&action.to_be_bytes());
            resp.extend_from_slice(&req[12..16]);
            if action == 0 {
                resp.extend_from_slice(&7u64.to_be_bytes());
            } else {
                n.fetch_add(1, Ordering::SeqCst);
                for h in req[16..].chunks(20) {
                    resp.extend_from_slice(&seeders(h).to_be_bytes());
                    resp.extend_from_slice(&0u32.to_be_bytes());
                    resp.extend_from_slice(&2u32.to_be_bytes());
                }
            }
            sock.send_to(&resp, src).await.unwrap();
        }
    });
    (url, scrapes)
}

/// Seeders = last byte of the infohash (`ih(n)` ends in n).
fn by_last_byte(h: &[u8]) -> u32 {
    u32::from(h[19])
}

fn service() -> LivenessService {
    LivenessService::new(LivenessConfig {
        use_dht: false,
        tracker_rps: 100.0,
        tracker_timeout: std::time::Duration::from_millis(300),
        ..Default::default()
    })
}

async fn put_with(s: &PgStore, id: u64, created_at: i64, n: u64, tracker: &str) {
    let mut t = torrent(id, 1, created_at, n, &format!("torrent {n}"));
    t.p.trackers = vec![tracker.to_string()];
    put(s, t).await;
}

async fn schedule(s: &PgStore, n: u64) -> (i64, i64, i16, Option<i64>, i16, i16) {
    sqlx::query_as(
        "SELECT born, due_at, prio, checked_at, dead_streak, error_streak
         FROM liveness_schedule WHERE infohash = $1",
    )
    .bind(ih(n))
    .fetch_one(s.pool())
    .await
    .unwrap()
}

async fn history_len(s: &PgStore, n: u64) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM liveness_history WHERE infohash = $1")
        .bind(ih(n))
        .fetch_one(s.pool())
        .await
        .unwrap()
}

fn due_ihs(rows: &[live::DueRow]) -> Vec<String> {
    rows.iter().map(|r| r.infohash.clone()).collect()
}

#[tokio::test]
async fn ingest_schedules_newest_first_and_boost_lanes_go_first() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let t = "udp://127.0.0.1:9/announce";
    for n in 1..=5u64 {
        put_with(s, n, NOW - 1000 * n as i64, n, t).await;
    }
    // An older event of infohash 1 moves its birth back (and its place: newest first).
    put_with(s, 10, NOW - 9000, 1, t).await;
    let (born, due_at, prio, checked, _, _) = schedule(s, 1).await;
    assert_eq!(
        (born, due_at, prio, checked),
        (NOW - 9000, -(NOW - 9000), 0, None)
    );

    let rows = live::due(s.pool(), NOW, 10).await.unwrap();
    assert_eq!(due_ihs(&rows), [2, 3, 4, 5, 1].map(ih));
    assert_eq!(rows[0].trackers, vec![t.to_string()]);
    assert_eq!(
        due_ihs(&live::due(s.pool(), NOW, 2).await.unwrap()),
        [2, 3].map(ih)
    );

    let svc = service();
    svc.boost([ih(5).as_str()], PRIO_SEARCH);
    svc.boost([ih(4).as_str(), ih(5).as_str()], PRIO_SAVED);
    svc.boost([ih(1).as_str()], PRIO_VIEWED);
    svc.boost([ih(99).as_str()], PRIO_VIEWED); // unknown: ignored
    assert_eq!(svc.flush_boosts(s.pool(), NOW).await.unwrap(), 3);
    assert_eq!(svc.flush_boosts(s.pool(), NOW).await.unwrap(), 0, "flushed");
    let rows = live::due(s.pool(), NOW, 10).await.unwrap();
    assert_eq!(due_ihs(&rows), [1, 4, 5, 2, 3].map(ih));
    assert_eq!(schedule(s, 5).await.2, PRIO_SAVED, "the higher boost wins");
}

#[tokio::test]
async fn batch_writes_results_schedule_and_history() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let (tracker, scrapes) = fake_tracker(by_last_byte).await;
    put_with(s, 1, NOW - 3600, 3, &tracker).await; // new, alive
    put_with(s, 2, NOW - 400 * DAY, 4, &tracker).await; // old, alive
    put_with(s, 3, NOW - 400 * DAY, 0x100, &tracker).await; // ends in 00: dead
    let svc = service();
    assert_eq!(svc.run_batch(s.pool(), NOW, 100).await.unwrap(), 3);
    assert_eq!(
        scrapes.load(Ordering::SeqCst),
        1,
        "one packet for all three"
    );

    let l = s.get_liveness(&ih(3)).await.unwrap().unwrap();
    assert_eq!((l.seeders, l.source), (Some(3), LivenessSource::Udp));
    let (_, due_new, _, checked, _, _) = schedule(s, 3).await;
    assert!(checked.is_some());
    // The result stays fresh until its next check; ±10 % jitter.
    assert_eq!(l.expires_at, due_new);
    let after = |due: i64, ttl: i64| (due - NOW - ttl).abs() <= ttl / 10 + 5;
    assert!(after(due_new, TTL_NEW), "new torrent: 6 h");
    assert!(
        after(schedule(s, 4).await.1, TTL_OLD),
        "old torrent: 4 days"
    );
    let (_, _, _, _, dead_streak, _) = schedule(s, 0x100).await;
    assert_eq!(dead_streak, 1);
    assert!(
        s.get_liveness(&ih(0x100))
            .await
            .unwrap()
            .unwrap()
            .is_dead(NOW)
    );
    for n in [3, 4, 0x100] {
        assert_eq!(history_len(s, n).await, 1);
    }
    assert!(live::due(s.pool(), NOW + 60, 10).await.unwrap().is_empty());

    // Stats: everything checked, nothing due.
    let st = svc.stats(s, NOW).await.unwrap();
    assert_eq!((st.torrents, st.never_checked, st.due_now), (3, 0, 0));
    assert_eq!((st.alive, st.dead, st.unknown), (2, 1, 0));
    assert_eq!(st.last_batch.as_ref().map(|b| b.hashes), Some(3));
    assert!(st.enabled && st.trackers == 1);
}

#[tokio::test]
async fn no_answer_keeps_the_last_result_and_backs_off() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    put_with(s, 1, NOW - 400 * DAY, 7, "udp://127.0.0.1:9/announce").await;
    let row = || async { live::due_row(s.pool(), &ih(7)).await.unwrap().unwrap() };
    let res = |seeders: Option<u32>, source, at: i64| Liveness {
        infohash: ih(7),
        seeders,
        leechers: seeders,
        completed: None,
        dht_peers_est: None,
        source,
        trackers_ok: 1,
        trackers_failed: 0,
        checked_at: at,
        expires_at: at + 1,
    };
    let ok = res(Some(5), LivenessSource::Udp, NOW);
    live::write_results(s.pool(), &[row().await], &[ok], NOW, Expiry::Scheduled)
        .await
        .unwrap();
    assert_eq!(history_len(s, 7).await, 1);

    // Nothing answered an hour later: the 5 seeders stay, only the schedule moves.
    let later = NOW + 3600;
    let none = res(None, LivenessSource::None, later);
    live::write_results(
        s.pool(),
        &[row().await],
        std::slice::from_ref(&none),
        later,
        Expiry::Scheduled,
    )
    .await
    .unwrap();
    let l = s.get_liveness(&ih(7)).await.unwrap().unwrap();
    assert_eq!((l.seeders, l.checked_at), (Some(5), NOW));
    let (_, due_at, _, checked, _, errors) = schedule(s, 7).await;
    assert_eq!((checked, errors), (Some(later), 1));
    assert!((due_at - later - TTL_ERROR).abs() <= TTL_ERROR / 10 + 1);
    live::write_results(s.pool(), &[row().await], &[none], later, Expiry::Scheduled)
        .await
        .unwrap();
    assert_eq!(schedule(s, 7).await.5, 2, "errors in a row");
    assert_eq!(history_len(s, 7).await, 1, "no point without an answer");

    // Same value again soon: no point; a changed one after the step: a point; the same one
    // after the step: none until a week passed.
    let at = NOW + 600;
    let ok = res(Some(5), LivenessSource::Udp, at);
    live::write_results(s.pool(), &[row().await], &[ok], at, Expiry::Scheduled)
        .await
        .unwrap();
    assert_eq!(
        schedule(s, 7).await.5,
        0,
        "an answer resets the error streak"
    );
    let at = NOW + HISTORY_STEP + 1;
    let changed = res(Some(9), LivenessSource::Udp, at);
    live::write_results(s.pool(), &[row().await], &[changed], at, Expiry::Scheduled)
        .await
        .unwrap();
    assert_eq!(history_len(s, 7).await, 2);
    let at = at + HISTORY_STEP + 1;
    // ±1 is wobble, not a change.
    let same = res(Some(8), LivenessSource::Udp, at);
    live::write_results(s.pool(), &[row().await], &[same], at, Expiry::Scheduled)
        .await
        .unwrap();
    assert_eq!(history_len(s, 7).await, 2, "unchanged");
    let at = at + 8 * DAY;
    let same = res(Some(8), LivenessSource::Udp, at);
    live::write_results(s.pool(), &[row().await], &[same], at, Expiry::Scheduled)
        .await
        .unwrap();
    assert_eq!(history_len(s, 7).await, 3, "weekly even unchanged");

    let h = live::history(s.pool(), &ih(7), 90, at)
        .await
        .unwrap()
        .unwrap();
    let seeders: Vec<_> = h.points.iter().map(|p| p.seeders).collect();
    assert_eq!(seeders, [Some(5), Some(9), Some(8)]);
    assert!(h.points.windows(2).all(|w| w[0].at < w[1].at));
    assert_eq!(
        live::history(s.pool(), &ih(7), 1, at)
            .await
            .unwrap()
            .unwrap()
            .points
            .len(),
        1
    );
    assert!(
        live::history(s.pool(), &ih(8), 90, at)
            .await
            .unwrap()
            .is_none()
    );

    // Pruning: everything older than 90 days goes.
    assert_eq!(
        live::prune_history(s.pool(), at + 85 * DAY).await.unwrap(),
        2
    );
    assert_eq!(history_len(s, 7).await, 1);
}

#[tokio::test]
async fn api_history_stats_and_boosts() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let (tracker, _) = fake_tracker(by_last_byte).await;
    put_with(s, 1, NOW, 3, &tracker).await;
    put_with(s, 2, NOW, 4, &tracker).await;
    let svc = Arc::new(service());
    svc.run_batch(s.pool(), magnetar_core::now(), 10)
        .await
        .unwrap();
    let state =
        magnetar_server::AppState::new(Arc::new(s.clone()), TOKEN).with_liveness(svc.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(listener, magnetar_server::router(state))
            .await
            .unwrap();
    });
    let api = ApiClient::new(&base, Some(TOKEN.into())).unwrap();

    let h = api.liveness_history(&ih(3), 90).await.unwrap().unwrap();
    assert_eq!(h.points.len(), 1);
    assert_eq!(h.points[0].seeders, Some(3));
    assert!(api.liveness_history(&ih(9), 90).await.unwrap().is_none());
    let health = api.health().await.unwrap();
    assert!(health.features.iter().any(|f| f == "liveness_history"));

    let st = api.stats().await.unwrap().liveness.unwrap();
    assert_eq!((st.torrents, st.never_checked, st.alive), (2, 0, 2));

    // A search boosts what it shows, a detail view more; applied on the next flush.
    let q = SearchQuery {
        min_trust: Some(0),
        sort: Sort::Date,
        ..Default::default()
    };
    assert_eq!(api.search(&q).await.unwrap().hits.len(), 2);
    api.torrent(&ih(4)).await.unwrap().unwrap();
    svc.flush_boosts(s.pool(), magnetar_core::now())
        .await
        .unwrap();
    assert_eq!(schedule(s, 3).await.2, PRIO_SEARCH);
    assert_eq!(schedule(s, 4).await.2, PRIO_VIEWED);
}

#[tokio::test]
async fn seeder_updates_reach_the_search_index() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let (tracker, _) = fake_tracker(by_last_byte).await;
    put_with(s, 1, NOW, 3, &tracker).await;
    put_with(s, 2, NOW, 9, &tracker).await;
    let dir = std::env::temp_dir().join(format!("magnetar-ix-{:016x}", rand::random::<u64>()));
    let id = load::database_id(s.pool()).await.unwrap();
    let idx = SearchIndex::open_async(dir.clone(), id).await.unwrap();
    search_index::build(s.pool(), &idx).await.unwrap();
    s.attach_search_index(idx.clone());

    service()
        .run_batch(s.pool(), magnetar_core::now(), 10)
        .await
        .unwrap();
    while search_index::apply_changes(s.pool(), &idx).await.unwrap() > 0 {}
    let q = SearchQuery {
        min_trust: Some(0),
        sort: Sort::Seeders,
        descending: true,
        min_seeders: Some(5),
        ..Default::default()
    };
    let page = s.search(&q, magnetar_core::now()).await.unwrap();
    let got: Vec<_> = page
        .hits
        .iter()
        .map(|h| (h.infohash.clone(), h.seeders))
        .collect();
    assert_eq!(got, vec![(ih(9), Some(9))]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn a_torrent_deleted_during_a_batch_does_not_fail_it() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let t = "udp://127.0.0.1:9/announce";
    put_with(s, 1, NOW - 3600, 1, t).await;
    put_with(s, 2, NOW - 3600, 2, t).await;
    let rows = live::due(s.pool(), NOW, 10).await.unwrap();
    assert_eq!(rows.len(), 2);
    // Deleted while the batch was out scraping: its schedule row goes, history would fail
    // the foreign key.
    sqlx::query("DELETE FROM torrents WHERE infohash = $1")
        .bind(ih(1))
        .execute(s.pool())
        .await
        .unwrap();
    let results: Vec<Liveness> = rows
        .iter()
        .map(|r| Liveness {
            infohash: r.infohash.clone(),
            seeders: Some(5),
            leechers: Some(1),
            completed: Some(0),
            dht_peers_est: None,
            source: LivenessSource::Udp,
            trackers_ok: 1,
            trackers_failed: 0,
            checked_at: NOW,
            expires_at: NOW + TTL_NEW,
        })
        .collect();
    live::write_results(s.pool(), &rows, &results, NOW, Expiry::Scheduled)
        .await
        .unwrap();
    assert_eq!(history_len(s, 1).await, 0);
    assert_eq!(history_len(s, 2).await, 1);
    assert_eq!(schedule(s, 2).await.3, Some(NOW));
}

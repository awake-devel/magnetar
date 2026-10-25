//! PgStore integration tests (need MAGNETAR_TEST_DATABASE_URL, else SKIPPED).

mod common;

use common::*;
use magnetar_core::model::*;
use magnetar_core::query::{SearchQuery, Sort};
use magnetar_core::store::{Store, keys};
use magnetar_server::filter::Filter;
use serde_json::json;

fn q(text: &str) -> SearchQuery {
    SearchQuery {
        text: text.into(),
        min_trust: Some(0),
        descending: true,
        ..Default::default()
    }
}

fn titles(page: &magnetar_core::query::SearchPage) -> Vec<String> {
    page.hits.iter().map(|h| h.title.clone()).collect()
}

fn live(infohash: u64, seeders: Option<u32>, dht: Option<u32>, expires_at: i64) -> Liveness {
    Liveness {
        infohash: ih(infohash),
        seeders,
        leechers: seeders.map(|_| 1),
        completed: None,
        dht_peers_est: dht,
        source: LivenessSource::Udp,
        trackers_ok: 1,
        trackers_failed: 0,
        checked_at: NOW - 10,
        expires_at,
    }
}

#[tokio::test]
async fn crud_and_idempotent_ingest() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let (p, raw) = torrent(1, 100, NOW - 50, 1, "Some.Release.2024")
        .files(&[("a/b.mkv", 10), ("c.nfo", 2)])
        .size(12)
        .build();
    assert!(s.put_torrent(&p, &raw).await.unwrap());
    assert!(
        !s.put_torrent(&p, &raw).await.unwrap(),
        "second insert is a duplicate"
    );
    assert!(s.has_event(&raw.id).await.unwrap());
    assert!(s.known_torrent_event(&raw.id).await.unwrap());
    assert!(!s.has_event(&hex64(999)).await.unwrap());
    assert_eq!(
        s.get_raw_events(std::slice::from_ref(&raw.id))
            .await
            .unwrap(),
        vec![raw.clone()]
    );

    put(s, torrent(2, 100, NOW - 40, 2, "Other")).await;
    put(s, torrent(3, 200, NOW - 30, 3, "Third")).await;
    assert_eq!(
        s.publishers(10).await.unwrap(),
        vec![hex64(100), hex64(200)]
    );
    assert_eq!(s.publishers(1).await.unwrap().len(), 1);

    let c = Comment {
        event_id: hex64(50),
        pubkey: hex64(300),
        created_at: NOW - 5,
        kind: 2004,
        torrent_event_id: raw.id.clone(),
        content: "thanks\0!".into(),
    };
    let craw = RawEvent {
        id: c.event_id.clone(),
        pubkey: c.pubkey.clone(),
        created_at: c.created_at,
        kind: 2004,
        tags: vec![vec!["e".into(), raw.id.clone()]],
        content: c.content.clone(),
        sig: "0".repeat(128),
    };
    assert!(s.put_comment(&c, &craw).await.unwrap());
    assert!(!s.put_comment(&c, &craw).await.unwrap());
    assert_eq!(
        s.comment_authors_on(&hex64(100), &[hex64(300), hex64(301)])
            .await
            .unwrap(),
        vec![hex64(300)]
    );
    // The raw event keeps the NUL; the comment row drops it.
    assert_eq!(
        s.get_raw_events(std::slice::from_ref(&craw.id))
            .await
            .unwrap()[0],
        craw
    );

    let st = s.stats().await.unwrap();
    assert_eq!(
        (st.torrents, st.events, st.publishers, st.comments),
        (3, 4, 2, 1)
    );
    assert!(st.db_bytes > 0);
}

#[tokio::test]
async fn text_search_release_names_prefix_typo() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    put(
        s,
        torrent(1, 1, NOW - 100, 1, "Big.Buck.Bunny.2008.1080p.x264-GRP"),
    )
    .await;
    put(s, torrent(2, 1, NOW - 90, 2, "Ubuntu 24.04 desktop amd64")).await;
    put(
        s,
        torrent(3, 1, NOW - 80, 3, "Unrelated").desc("A story about rabbits and carrots"),
    )
    .await;
    put(
        s,
        torrent(4, 1, NOW - 70, 4, "Pack").files(&[("Season.01/Show.S01E01.mkv", 1)]),
    )
    .await;

    let page = s.search(&q("bunny 1080p"), NOW).await.unwrap();
    assert_eq!(titles(&page), vec!["Big.Buck.Bunny.2008.1080p.x264-GRP"]);
    assert_eq!(page.total, 1);
    assert_eq!(page.source, "server");
    assert!(
        page.hits[0].rank > 0.5,
        "full-text hits rank above fuzzy ones"
    );

    // Prefix on the last token.
    assert_eq!(
        titles(&s.search(&q("Big Buck Bun"), NOW).await.unwrap()),
        vec!["Big.Buck.Bunny.2008.1080p.x264-GRP"]
    );
    // Typo via trigram word_similarity.
    let typo = s.search(&q("ubunto"), NOW).await.unwrap();
    assert_eq!(titles(&typo), vec!["Ubuntu 24.04 desktop amd64"]);
    assert!(typo.hits[0].rank > 0.0 && typo.hits[0].rank <= 0.5);
    // English stemming on the description.
    assert_eq!(
        titles(&s.search(&q("rabbit"), NOW).await.unwrap()),
        vec!["Unrelated"]
    );
    // File paths are indexed with separators split.
    assert_eq!(
        titles(&s.search(&q("s01e01"), NOW).await.unwrap()),
        vec!["Pack"]
    );
    // Sanitized junk does not error.
    assert_eq!(s.search(&q("'):*&|!"), NOW).await.unwrap().total, 4);
    assert_eq!(s.search(&q("zzzzqqq"), NOW).await.unwrap().total, 0);
}

#[tokio::test]
async fn search_filters() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    put(
        s,
        torrent(1, 1, NOW - 300, 1, "video small")
            .cat(Category::Video)
            .size(100),
    )
    .await;
    put(
        s,
        torrent(2, 1, NOW - 200, 2, "video big")
            .cat(Category::Video)
            .size(10_000),
    )
    .await;
    put(
        s,
        torrent(3, 1, NOW - 100, 3, "audio unknown size").cat(Category::Audio),
    )
    .await;
    put(
        s,
        torrent(4, 2, NOW - 50, 4, "pow bot")
            .cat(Category::Anime)
            .pow(21),
    )
    .await;

    let mut f = q("");
    f.categories = vec![Category::Video];
    assert_eq!(s.search(&f, NOW).await.unwrap().total, 2);
    f.categories = vec![Category::Video, Category::Audio];
    assert_eq!(s.search(&f, NOW).await.unwrap().total, 3);

    let mut f = q("");
    f.min_size = Some(500);
    assert_eq!(titles(&s.search(&f, NOW).await.unwrap()), vec!["video big"]);
    f.min_size = None;
    f.max_size = Some(500);
    assert_eq!(
        titles(&s.search(&f, NOW).await.unwrap()),
        vec!["video small"]
    );

    let mut f = q("");
    f.since = Some(NOW - 200);
    f.until = Some(NOW - 100);
    let mut got = titles(&s.search(&f, NOW).await.unwrap());
    got.sort();
    assert_eq!(got, vec!["audio unknown size", "video big"]);

    // Trust: default threshold 20 from settings; pow 21 => bonus 20 passes, the rest is 0.
    let mut f = q("");
    f.min_trust = None;
    assert_eq!(titles(&s.search(&f, NOW).await.unwrap()), vec!["pow bot"]);
    s.set_setting(keys::TRUST_THRESHOLD, "0").await.unwrap();
    assert_eq!(s.search(&f, NOW).await.unwrap().total, 4);
    f.min_trust = Some(21);
    assert_eq!(s.search(&f, NOW).await.unwrap().total, 0);

    // Blocked publishers are excluded even with min_trust 0; trusted ones pass any threshold.
    s.set_override(&hex64(1), Some(OverrideAction::Block))
        .await
        .unwrap();
    assert_eq!(
        titles(&s.search(&q(""), NOW).await.unwrap()),
        vec!["pow bot"]
    );
    s.set_override(&hex64(1), Some(OverrideAction::Trust))
        .await
        .unwrap();
    let mut f = q("");
    f.min_trust = Some(90);
    assert_eq!(s.search(&f, NOW).await.unwrap().total, 3);
    assert_eq!(
        s.overrides().await.unwrap(),
        vec![(hex64(1), OverrideAction::Trust)]
    );
    s.set_override(&hex64(1), None).await.unwrap();
    assert!(s.overrides().await.unwrap().is_empty());
}

#[tokio::test]
async fn search_liveness_filters() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    for (n, title) in [
        (1, "dead fresh"),
        (2, "dead stale"),
        (3, "alive"),
        (4, "unknown"),
        (5, "dht only"),
    ] {
        put(s, torrent(n, 1, NOW - n as i64, n, title)).await;
    }
    s.put_liveness(&live(1, Some(0), None, NOW + 100))
        .await
        .unwrap();
    s.put_liveness(&live(2, Some(0), None, NOW - 1))
        .await
        .unwrap();
    s.put_liveness(&live(3, Some(7), None, NOW + 100))
        .await
        .unwrap();
    s.put_liveness(&live(5, None, Some(3), NOW + 100))
        .await
        .unwrap();
    assert_eq!(
        s.get_liveness(&ih(3)).await.unwrap().unwrap().seeders,
        Some(7)
    );

    let all = s.search(&q(""), NOW).await.unwrap();
    let dead: Vec<_> = all
        .hits
        .iter()
        .map(|h| (h.title.as_str(), h.dead))
        .collect();
    assert!(dead.contains(&("dead fresh", Some(true))));
    assert!(dead.contains(&("dead stale", None)));
    assert!(dead.contains(&("alive", Some(false))));
    assert!(dead.contains(&("unknown", None)));

    let mut f = q("");
    f.hide_dead = true;
    let mut got = titles(&s.search(&f, NOW).await.unwrap());
    got.sort();
    assert_eq!(got, vec!["alive", "dead stale", "dht only", "unknown"]);

    let mut f = q("");
    f.min_seeders = Some(1);
    let mut got = titles(&s.search(&f, NOW).await.unwrap());
    got.sort();
    assert_eq!(got, vec!["alive", "dht only"]);

    // Seeders sort: unknown last in both directions.
    let mut f = q("");
    f.sort = Sort::Seeders;
    let got = titles(&s.search(&f, NOW).await.unwrap());
    assert_eq!(&got[..2], &["alive", "dht only"]);
    assert_eq!(got.last().map(String::as_str), Some("unknown"));
    f.descending = false;
    let got = titles(&s.search(&f, NOW).await.unwrap());
    assert_eq!(got.last().map(String::as_str), Some("unknown"));

    // liveness_due: missing or expired, newest torrents first, with trackers.
    let due = s.liveness_due(NOW, 10).await.unwrap();
    let due_ih: Vec<_> = due.iter().map(|(h, _)| h.clone()).collect();
    assert_eq!(due_ih, vec![ih(2), ih(4)]);
    assert_eq!(
        due[0].1,
        vec!["udp://tracker.opentrackr.org:1337/announce".to_string()]
    );
    assert_eq!(s.liveness_due(NOW, 1).await.unwrap().len(), 1);
}

#[tokio::test]
async fn search_sorts_and_paging() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    put(s, torrent(1, 1, NOW - 30, 1, "linux alpha").size(300)).await;
    put(s, torrent(2, 1, NOW - 20, 2, "linux beta")).await;
    put(s, torrent(3, 1, NOW - 10, 3, "linux gamma linux").size(100)).await;

    let mut f = q("");
    f.sort = Sort::Date;
    assert_eq!(
        titles(&s.search(&f, NOW).await.unwrap()),
        vec!["linux gamma linux", "linux beta", "linux alpha"]
    );
    f.descending = false;
    assert_eq!(
        titles(&s.search(&f, NOW).await.unwrap()),
        vec!["linux alpha", "linux beta", "linux gamma linux"]
    );

    let mut f = q("");
    f.sort = Sort::Size;
    assert_eq!(
        titles(&s.search(&f, NOW).await.unwrap()),
        vec!["linux alpha", "linux gamma linux", "linux beta"]
    );
    f.descending = false;
    assert_eq!(
        titles(&s.search(&f, NOW).await.unwrap()),
        vec!["linux gamma linux", "linux alpha", "linux beta"]
    );

    // Relevance (default sort with text): ts_rank_cd favours the repeated term.
    let page = s.search(&q("linux"), NOW).await.unwrap();
    assert_eq!(page.hits[0].title, "linux gamma linux");
    assert!(page.hits.windows(2).all(|w| w[0].rank >= w[1].rank));
    // Relevance without text falls back to date.
    let mut f = q("  ");
    f.sort = Sort::Relevance;
    assert_eq!(
        s.search(&f, NOW).await.unwrap().hits[0].title,
        "linux gamma linux"
    );

    let mut f = q("");
    f.limit = 2;
    let p1 = s.search(&f, NOW).await.unwrap();
    assert_eq!((p1.hits.len(), p1.total), (2, 3));
    f.offset = 2;
    let p2 = s.search(&f, NOW).await.unwrap();
    assert_eq!((p2.hits.len(), p2.total), (1, 3));
    assert_eq!(p2.hits[0].title, "linux alpha");
}

#[tokio::test]
async fn grouping_by_infohash() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    // Same infohash published three times; publisher 2 is earliest, publisher 3 has PoW.
    put(s, torrent(1, 1, NOW - 10, 7, "copy late")).await;
    put(s, torrent(2, 2, NOW - 50, 7, "copy early")).await;
    put(s, torrent(3, 3, NOW - 5, 7, "copy pow").pow(24)).await;
    put(s, torrent(4, 1, NOW - 1, 8, "different")).await;

    let page = s.search(&q(""), NOW).await.unwrap();
    assert_eq!(page.total, 2);
    let g = page.hits.iter().find(|h| h.infohash == ih(7)).unwrap();
    assert_eq!(g.title, "copy pow", "highest trust wins");
    assert_eq!(g.publisher_count, 3);
    assert_eq!(g.trust, 25);
    assert_eq!(g.npub, npub(&hex64(3)));
    assert!(g.npub.starts_with("npub1"));

    // Without the PoW copy (blocked) the earliest wins.
    s.set_override(&hex64(3), Some(OverrideAction::Block))
        .await
        .unwrap();
    let page = s.search(&q("copy"), NOW).await.unwrap();
    assert_eq!(titles(&page), vec!["copy early"]);
    assert_eq!(page.hits[0].publisher_count, 2);
}

#[tokio::test]
async fn trust_denormalisation_and_graph_version() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    put(s, torrent(1, 1, NOW - 10, 1, "no pow")).await;
    put(s, torrent(2, 1, NOW - 9, 2, "pow 16").pow(16)).await;
    put(s, torrent(3, 2, NOW - 8, 3, "other publisher")).await;

    let score = |score: u8, code: &str| TrustScore {
        pubkey: hex64(1),
        score,
        factors: vec![TrustFactor {
            code: code.into(),
            delta: i32::from(score),
            detail: "d".into(),
            evidence: vec![hex64(9)],
        }],
        root: Some(hex64(9)),
        computed_at: NOW,
        graph_version: 1,
    };
    s.put_trust(&score(40, "FOLLOW_D2")).await.unwrap();
    assert_eq!(
        s.get_trust(&hex64(1)).await.unwrap(),
        Some(score(40, "FOLLOW_D2"))
    );
    assert_eq!(s.get_trust(&hex64(2)).await.unwrap(), None);
    let trust_of = |title: &'static str| async move {
        let page = s.search(&q(title), NOW).await.unwrap();
        page.hits
            .into_iter()
            .find(|h| h.title == title)
            .unwrap()
            .trust
    };
    assert_eq!(trust_of("no pow").await, 40);
    assert_eq!(trust_of("pow 16").await, 52); // 40 + 25*8/16
    assert_eq!(trust_of("other publisher").await, 0);

    // A pinned score ignores PoW.
    s.put_trust(&score(0, "BLOCKED")).await.unwrap();
    assert_eq!(trust_of("pow 16").await, 0);
    s.put_trust(&score(95, "FOLLOW_D1")).await.unwrap();
    assert_eq!(trust_of("pow 16").await, 100);

    // New torrents pick up the cached score immediately.
    put(s, torrent(4, 1, NOW - 1, 4, "fresh one").pow(8)).await;
    assert_eq!(trust_of("fresh one").await, 95);

    let v = s.graph_version().await.unwrap();
    assert_eq!(s.bump_graph_version().await.unwrap(), v + 1);
    s.set_setting(keys::TRUST_ROOT, &json!(hex64(9)).to_string())
        .await
        .unwrap();
    assert_eq!(s.graph_version().await.unwrap(), v + 2, "root change bumps");
    s.set_setting(keys::TRUST_ROOT, &json!(hex64(9)).to_string())
        .await
        .unwrap();
    assert_eq!(
        s.graph_version().await.unwrap(),
        v + 2,
        "same root does not"
    );
    assert_eq!(
        s.get_setting(keys::TRUST_ROOT).await.unwrap(),
        Some(json!(hex64(9)).to_string())
    );
    assert_eq!(s.get_setting("nope").await.unwrap(), None);
}

#[tokio::test]
async fn publisher_stats_window() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    // 3 events within one hour, a 4th two hours later.
    for (i, t) in [0i64, 1000, 3599, 7200].into_iter().enumerate() {
        put(s, torrent(i as u64 + 1, 1, NOW + t, i as u64 + 1, "x")).await;
    }
    put(s, torrent(10, 2, NOW, 10, "bridged").bridge()).await;
    let st = s.publisher_stats(&hex64(1)).await.unwrap();
    assert_eq!(st.first_seen, Some(NOW));
    assert_eq!(st.torrent_count, 4);
    assert_eq!(st.max_per_hour, 3);
    assert!(!st.is_bridge && !st.has_profile && !st.has_relay_list);
    assert!(s.publisher_stats(&hex64(2)).await.unwrap().is_bridge);
    assert_eq!(
        s.publisher_stats(&hex64(3)).await.unwrap(),
        PublisherStats::default()
    );
}

#[tokio::test]
async fn relays_cursors_backoff() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let url = "wss://relay.example.com";
    s.upsert_relay(url, "seed").await.unwrap();
    s.set_relay_enabled(url, false).await.unwrap();
    s.upsert_relay(url, "nip65").await.unwrap();
    let r = &s.list_relays().await.unwrap()[0];
    assert_eq!(
        (r.source.as_str(), r.enabled, r.next_try_at),
        ("seed", false, 0)
    );
    s.upsert_relay(url, "manual").await.unwrap();
    s.upsert_relay(url, "hint").await.unwrap();
    let r = &s.list_relays().await.unwrap()[0];
    assert_eq!(
        (r.source.as_str(), r.enabled),
        ("manual", false),
        "manual sticks, enabled untouched"
    );

    s.record_relay_result(url, false, Some("timeout"), 0, NOW)
        .await
        .unwrap();
    s.record_relay_result(url, false, Some("timeout again"), 2, NOW)
        .await
        .unwrap();
    let r = &s.list_relays().await.unwrap()[0];
    assert_eq!(r.consecutive_failures, 2);
    assert_eq!(r.next_try_at, NOW + 600);
    assert_eq!(r.last_err.as_deref(), Some("timeout again"));
    for _ in 0..20 {
        s.record_relay_result(url, false, None, 0, NOW)
            .await
            .unwrap();
    }
    assert_eq!(
        s.list_relays().await.unwrap()[0].next_try_at,
        NOW + 24 * 3600
    );
    s.record_relay_result(url, true, None, 5, NOW + 1)
        .await
        .unwrap();
    let r = &s.list_relays().await.unwrap()[0];
    assert_eq!(
        (r.consecutive_failures, r.last_ok, r.events_found),
        (0, Some(NOW + 1), 7)
    );
    assert_eq!(r.last_err, None);
    assert_eq!(r.supports_neg, None);
    s.set_relay_supports_neg(url, true).await.unwrap();
    assert_eq!(s.list_relays().await.unwrap()[0].supports_neg, Some(true));

    assert_eq!(s.get_cursor(url, 2003).await.unwrap(), None);
    let c = Cursor {
        relay: url.into(),
        kind: 2003,
        newest_seen: Some(NOW),
        backfill_until: Some(NOW - 1000),
        backfill_done: false,
    };
    s.put_cursor(&c).await.unwrap();
    assert_eq!(s.get_cursor(url, 2003).await.unwrap(), Some(c.clone()));
    let c2 = Cursor {
        backfill_done: true,
        backfill_until: None,
        ..c
    };
    s.put_cursor(&c2).await.unwrap();
    assert_eq!(s.get_cursor(url, 2003).await.unwrap(), Some(c2));
    assert_eq!(s.get_cursor(url, 2004).await.unwrap(), None);

    assert!(s.relay_lists_for(&[hex64(1)]).await.unwrap().is_empty());
    assert!(!s.relay_list_oversized(&hex64(1)).await.unwrap());
}

#[tokio::test]
async fn saved_searches_crud() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let mut ss = SavedSearch {
        id: 0,
        name: "ubuntu isos".into(),
        query: SearchQuery {
            text: "ubuntu".into(),
            categories: vec![Category::Application],
            ..Default::default()
        },
        notify: true,
        last_checked: None,
        last_seen_created_at: None,
    };
    let id = s.put_saved_search(&ss).await.unwrap();
    assert!(id > 0);
    ss.id = id;
    assert_eq!(s.list_saved_searches().await.unwrap(), vec![ss.clone()]);
    ss.last_checked = Some(NOW);
    ss.last_seen_created_at = Some(NOW - 5);
    assert_eq!(s.put_saved_search(&ss).await.unwrap(), id);
    assert_eq!(s.list_saved_searches().await.unwrap(), vec![ss.clone()]);
    assert!(
        s.put_saved_search(&SavedSearch {
            id: id + 100,
            ..ss.clone()
        })
        .await
        .is_err()
    );
    s.delete_saved_search(id).await.unwrap();
    assert!(s.list_saved_searches().await.unwrap().is_empty());
}

#[tokio::test]
async fn graph_queries() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    // Graph tables are normally filled by put_replaceable (needs core::wot); seed them directly.
    for (a, b) in [(1, 2), (1, 3), (2, 4), (3, 4), (5, 4)] {
        sqlx::query("INSERT INTO follows VALUES ($1, $2)")
            .bind(hex64(a))
            .bind(hex64(b))
            .execute(s.pool())
            .await
            .unwrap();
    }
    sqlx::query("INSERT INTO mutes VALUES ($1, $2, 10000), ($1, $2, 30007)")
        .bind(hex64(2))
        .bind(hex64(6))
        .execute(s.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO reports VALUES ($1, $2, $3, 'spam', 1)")
        .bind(hex64(77))
        .bind(hex64(3))
        .bind(hex64(6))
        .execute(s.pool())
        .await
        .unwrap();
    assert_eq!(
        s.follows_of(&hex64(1)).await.unwrap(),
        vec![hex64(2), hex64(3)]
    );
    assert_eq!(
        s.followers_among(&[hex64(4), hex64(9)], &[hex64(2), hex64(3)])
            .await
            .unwrap(),
        vec![(hex64(4), vec![hex64(2), hex64(3)])]
    );
    assert_eq!(
        s.mutes_by(&[hex64(2)]).await.unwrap(),
        vec![(hex64(2), hex64(6))]
    );
    assert_eq!(
        s.reports_by(&[hex64(3)]).await.unwrap(),
        vec![(hex64(3), hex64(6), "spam".to_string())]
    );
}

#[tokio::test]
async fn relay_filter_queries() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let (_, a) = put(s, torrent(1, 1, NOW - 30, 1, "a").hashtag("movie")).await;
    let (_, b) = put(s, torrent(2, 2, NOW - 20, 2, "b")).await;
    let (_, c) = put(s, torrent(3, 1, NOW - 20, 3, "c").hashtag("movie")).await;

    let f = |v: serde_json::Value| Filter::from_json(&v).unwrap();
    let ids = |evs: Vec<RawEvent>| evs.into_iter().map(|e| e.id).collect::<Vec<_>>();
    // Newest first, ties by lowest id.
    assert_eq!(
        ids(s.query_events(&f(json!({"kinds":[2003]}))).await.unwrap()),
        vec![b.id.clone(), c.id.clone(), a.id.clone()]
    );
    assert_eq!(
        ids(s.query_events(&f(json!({"#t":["movie"]}))).await.unwrap()),
        vec![c.id.clone(), a.id.clone()]
    );
    assert_eq!(
        ids(s.query_events(&f(json!({"#x":[ih(2)]}))).await.unwrap()),
        vec![b.id.clone()]
    );
    assert_eq!(
        ids(s
            .query_events(&f(json!({"authors":[hex64(1)],"limit":1})))
            .await
            .unwrap()),
        vec![c.id.clone()]
    );
    assert_eq!(
        ids(s
            .query_events(&f(json!({"ids":[a.id],"since":NOW-30,"until":NOW-30})))
            .await
            .unwrap()),
        vec![a.id.clone()]
    );
    assert!(
        s.query_events(&f(json!({"kinds":[]})))
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        s.query_events(&f(json!({"limit":0})))
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        s.query_events(&f(json!({"kinds":[1]})))
            .await
            .unwrap()
            .is_empty()
    );

    let items = s.neg_items(&f(json!({"kinds":[2003]})), 10).await.unwrap();
    assert_eq!(items.len(), 3);
    assert_eq!(items[0].0, (NOW - 30) as u64);
    assert_eq!(hex::encode(items[0].1), a.id);
    assert_eq!(
        s.neg_items(&f(json!({})), 1).await.unwrap().len(),
        2,
        "max + 1 sentinel"
    );
    assert_eq!(s.trackers_for(&ih(1)).await.unwrap().unwrap().len(), 1);
    assert_eq!(s.trackers_for(&ih(99)).await.unwrap(), None);
}

#[tokio::test]
async fn live_feed_broadcasts_new_events_only() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let mut rx = s.subscribe();
    let (p, raw) = torrent(1, 1, NOW, 1, "live").build();
    assert!(s.put_torrent(&p, &raw).await.unwrap());
    assert!(!s.put_torrent(&p, &raw).await.unwrap());
    assert_eq!(rx.recv().await.unwrap(), raw);
    assert!(rx.try_recv().is_err());
}

// ---- post-merge: need core::wot / core::magnet / core::ingest / nip35 from other agents ----

fn replaceable(id: u64, pubkey: u64, kind: u16, created_at: i64, tags: Vec<Vec<&str>>) -> RawEvent {
    RawEvent {
        id: hex64(id),
        pubkey: hex64(pubkey),
        created_at,
        kind,
        tags: tags
            .into_iter()
            .map(|t| t.into_iter().map(String::from).collect())
            .collect(),
        content: String::new(),
        sig: "0".repeat(128),
    }
}

#[tokio::test]
#[ignore = "post-merge"]
async fn replaceable_newer_wins_tie_lowest_id() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let (a, b, c) = (hex64(2), hex64(3), hex64(4));
    let v1 = replaceable(20, 1, 3, NOW - 100, vec![vec!["p", &a]]);
    let v2 = replaceable(30, 1, 3, NOW - 50, vec![vec!["p", &b]]);
    let tie_high = replaceable(40, 1, 3, NOW - 50, vec![vec!["p", &c]]);
    let tie_low = replaceable(25, 1, 3, NOW - 50, vec![vec!["p", &c]]);
    let old = replaceable(10, 1, 3, NOW - 500, vec![vec!["p", &a]]);
    assert!(s.put_replaceable(&v1).await.unwrap());
    assert!(s.put_replaceable(&v2).await.unwrap());
    assert!(!s.put_replaceable(&old).await.unwrap());
    assert!(!s.put_replaceable(&tie_high).await.unwrap());
    assert!(!s.put_replaceable(&v2).await.unwrap());
    assert_eq!(s.follows_of(&hex64(1)).await.unwrap(), vec![b.clone()]);
    assert!(s.put_replaceable(&tie_low).await.unwrap());
    assert_eq!(s.follows_of(&hex64(1)).await.unwrap(), vec![c.clone()]);
    // Only the head is kept for re-serving.
    assert!(!s.has_event(&v2.id).await.unwrap());
    assert!(s.has_event(&tie_low.id).await.unwrap());

    // Graph version: bumps for the root's and D1 members' lists only.
    s.set_setting(keys::TRUST_ROOT, &json!(hex64(1)).to_string())
        .await
        .unwrap();
    let v = s.graph_version().await.unwrap();
    assert!(
        s.put_replaceable(&replaceable(50, 1, 3, NOW, vec![vec!["p", &a]]))
            .await
            .unwrap()
    );
    assert_eq!(s.graph_version().await.unwrap(), v + 1);
    assert!(
        s.put_replaceable(&replaceable(51, 2, 10000, NOW, vec![vec!["p", &c]]))
            .await
            .unwrap()
    );
    assert_eq!(
        s.graph_version().await.unwrap(),
        v + 2,
        "D1 member mute list"
    );
    assert_eq!(
        s.mutes_by(std::slice::from_ref(&a)).await.unwrap(),
        vec![(a.clone(), c.clone())]
    );
    assert!(
        s.put_replaceable(&replaceable(52, 9, 3, NOW, vec![vec!["p", &a]]))
            .await
            .unwrap()
    );
    assert_eq!(s.graph_version().await.unwrap(), v + 2, "stranger");

    // Profiles and relay lists.
    let mut prof = replaceable(60, 5, 0, NOW, vec![]);
    prof.content = r#"{"name":"alice"}"#.into();
    assert!(s.put_replaceable(&prof).await.unwrap());
    let rl = replaceable(
        61,
        5,
        10002,
        NOW,
        vec![
            vec!["r", "wss://nos.lol"],
            vec!["r", "wss://relay.damus.io", "read"],
        ],
    );
    assert!(s.put_replaceable(&rl).await.unwrap());
    let st = s.publisher_stats(&hex64(5)).await.unwrap();
    assert!(st.has_profile && st.has_relay_list);
    let lists = s.relay_lists_for(&[hex64(5)]).await.unwrap();
    assert_eq!(lists.len(), 2);
    assert!(lists.contains(&(hex64(5), "wss://relay.damus.io".into(), true, false)));
}

#[tokio::test]
#[ignore = "post-merge"]
async fn reports_are_extracted() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let target = hex64(6);
    let r = replaceable(70, 3, 1984, NOW, vec![vec!["p", &target, "spam"]]);
    assert!(s.put_report(&r).await.unwrap());
    assert!(!s.put_report(&r).await.unwrap());
    assert_eq!(
        s.reports_by(&[hex64(3)]).await.unwrap(),
        vec![(hex64(3), target, "spam".into())]
    );
}

#[tokio::test]
#[ignore = "post-merge"]
async fn torrent_detail_groups_events() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    put(
        s,
        torrent(1, 1, NOW - 10, 7, "late")
            .files(&[("a.mkv", 5)])
            .size(5),
    )
    .await;
    put(s, torrent(2, 2, NOW - 50, 7, "early")).await;
    s.put_liveness(&live(7, Some(4), None, NOW + 100))
        .await
        .unwrap();
    let d = s.torrent_detail(&ih(7), NOW).await.unwrap().unwrap();
    assert_eq!(d.summary.title, "early");
    assert_eq!(d.summary.publisher_count, 2);
    assert_eq!(d.summary.seeders, Some(4));
    assert_eq!(d.summary.dead, Some(false));
    assert_eq!(d.events.len(), 2);
    assert!(
        d.magnet
            .starts_with(&format!("magnet:?xt=urn:btih:{}", ih(7)))
    );
    assert_eq!(d.trust.score, 0);
    assert!(s.torrent_detail(&ih(8), NOW).await.unwrap().is_none());
}

#[tokio::test]
#[ignore = "post-merge"]
async fn ingest_fixtures() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let now = magnetar_core::now();
    let mut inserted = 0;
    for f in ["kind2003_sample.jsonl", "kind2004_sample.jsonl"] {
        for ev in fixture_events(f) {
            let out = magnetar_core::ingest::ingest_into(s, ev.clone(), now)
                .await
                .unwrap();
            if out == magnetar_core::ingest::IngestOutcome::Inserted {
                inserted += 1;
                assert_eq!(
                    magnetar_core::ingest::ingest_into(s, ev, now)
                        .await
                        .unwrap(),
                    magnetar_core::ingest::IngestOutcome::Duplicate
                );
            }
        }
    }
    let st = s.stats().await.unwrap();
    assert!(
        inserted >= 52,
        "every kind 2003 fixture verifies and parses ({inserted})"
    );
    assert!(st.torrents > 0);
    let page = s
        .search(
            &SearchQuery {
                min_trust: Some(0),
                ..Default::default()
            },
            now,
        )
        .await
        .unwrap();
    assert_eq!(page.total, st.torrents);
}

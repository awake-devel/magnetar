//! Crawler against in-process fake relays: pagination, cursors, dedup, resume, comments,
//! budget, failures, discovery and metadata. One live test (ignored).

mod support;

use std::collections::HashSet;
use std::time::{Duration, Instant};

use magnetar_core::crawler::{CrawlConfig, METADATA_KINDS, crawl_once};
use magnetar_core::model::*;
use magnetar_core::store::Store;
use nostr::prelude::Keys;
use serde_json::Value;
use support::fake_relay::{Behaviour, FakeRelay};
use support::store::MemStore;
use support::*;

fn cfg(seeds: &[&str]) -> CrawlConfig {
    CrawlConfig {
        seed_relays: seeds.iter().map(|s| s.to_string()).collect(),
        indexers: Vec::new(),
        max_relays: 5,
        explore_relays: 0,
        budget: Duration::from_secs(30),
        max_backfill_pages: 2,
        page_limit: 500,
        overlap_secs: 3600,
        concurrency: 2,
        fetch_metadata: false,
        root: None,
    }
}

/// Sorted like a relay: newest first, ties by lowest id.
fn relay_order(events: &[RawEvent]) -> Vec<RawEvent> {
    let mut v = events.to_vec();
    v.sort_by(|a, b| b.created_at.cmp(&a.created_at).then(a.id.cmp(&b.id)));
    v
}

fn kinds_of(f: &Value) -> Vec<u64> {
    f["kinds"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_u64).collect())
        .unwrap_or_default()
}

#[tokio::test]
async fn paginates_dedups_and_resumes() {
    let publisher = Keys::generate();
    let commenter = Keys::generate();
    let t0 = now();
    // 250 torrents, three per second, so ties straddle the 100-event page boundaries.
    let generated: Vec<RawEvent> = (0..250)
        .map(|i| torrent(&publisher, i, t0 - 1000 - i64::from(i / 3)))
        .collect();
    let mut comments: Vec<RawEvent> = Vec::new();
    for i in 0..25usize {
        let target = &generated[i * 10].id;
        let tag: Vec<&str> = match i % 3 {
            0 => vec!["e", target, "", "root"],
            1 => vec!["e", target],
            _ => vec!["e", target, "", "", "ab", "root"],
        };
        comments.push(sign(&commenter, 2004, t0 - 500 - i as i64, &[&tag], "nice"));
    }
    for i in 0..6usize {
        let target = &generated[i * 40].id;
        comments.push(sign(
            &commenter,
            1111,
            t0 - 400 - i as i64,
            &[
                &["E", target],
                &["K", "2003"],
                &["e", target],
                &["k", "2003"],
            ],
            "nip-22",
        ));
    }
    // An unrelated NIP-22 comment the relay must never return for #K=2003.
    comments.push(sign(
        &commenter,
        1111,
        t0 - 300,
        &[&["E", &"ee".repeat(32)], &["K", "1"]],
        "x",
    ));

    let fixtures = torrents_fixture();
    let fixture_comments = comments_fixture();
    let mut events = generated.clone();
    events.extend(fixtures.iter().cloned());
    events.extend(comments.iter().cloned());
    events.extend(fixture_comments.iter().cloned());

    let behaviour = Behaviour {
        closed_reqs: vec![3],
        ..Behaviour::default()
    };
    let relay = FakeRelay::start(events, behaviour).await;
    let store = MemStore::default();
    let url = relay.url.clone();

    // ---- run 1: two backfill pages only.
    let r1 = crawl_once(&store, &cfg(&[&url])).await.unwrap();
    let sorted = relay_order(&generated);
    let m1 = sorted[99].created_at;
    let first_tie = sorted.iter().position(|e| e.created_at == m1).unwrap();
    let expected1 = first_tie + 100;
    assert_eq!(store.torrent_count(), expected1, "{r1:?}");
    let stored: HashSet<String> = store.with(|i| i.torrents.keys().cloned().collect());
    for e in &sorted[..expected1] {
        assert!(stored.contains(&e.id));
    }
    let c = store.cursor(&url, KIND_TORRENT).unwrap();
    assert_eq!(c.newest_seen, Some(sorted[0].created_at));
    assert_eq!(c.backfill_until, Some(sorted[expected1 - 1].created_at));
    assert!(!c.backfill_done);
    // Comments on stored torrents came through the targeted #e/#E queries; the rest are orphans.
    let on_stored = comments
        .iter()
        .filter(|c| {
            magnetar_core::nip35::parse_comment(c)
                .is_some_and(|p| stored.contains(&p.torrent_event_id))
                && !(c.kind == 1111 && c.tags.iter().any(|t| t[0] == "K" && t[1] == "1"))
        })
        .count();
    assert!(on_stored > 0 && on_stored < 31);
    assert_eq!(store.comment_count(), on_stored);
    assert_eq!(r1.inserted as usize, expected1 + on_stored);
    assert_eq!(r1.comments as usize, on_stored);
    assert_eq!((r1.relays_tried, r1.relays_ok), (1, 1));
    assert!(r1.errors.is_empty(), "{:?}", r1.errors);
    let info = store.relay(&url).unwrap();
    assert_eq!(info.source, "seed");
    assert_eq!(info.events_found, r1.inserted);
    assert!(info.last_ok.is_some());

    // Every REQ carries a limit; 2003 pages walk `until` strictly downwards.
    let reqs = relay.reqs();
    assert!(reqs.iter().all(|f| f["limit"].as_u64() == Some(500)));
    let untils: Vec<Option<i64>> = reqs
        .iter()
        .filter(|f| kinds_of(f) == [2003])
        .map(|f| f["until"].as_i64())
        .collect();
    assert_eq!(untils, vec![None, Some(m1)]);
    // REQ #3 got CLOSED (rate-limited) and was retried with the same filter.
    assert_eq!(reqs[2], reqs[3]);
    // Comment lookups: one REQ with both the #e and the #E filter, ≤ 100 ids each.
    let all = relay.all_filters();
    assert!(all.iter().any(|f| f.get("#e").is_some()));
    assert!(all.iter().any(|f| f.get("#E").is_some()));
    assert!(
        all.iter()
            .filter_map(|f| f.get("#e").or(f.get("#E")))
            .all(|ids| ids.as_array().unwrap().len() <= 100)
    );
    // Comment history waits for the torrent history: no generic 2004/1111 sweep yet.
    assert!(
        !reqs
            .iter()
            .any(|f| kinds_of(f) == [2004] || f.get("#K").is_some())
    );
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(relay.state.lock().unwrap().closes, reqs.len() - 1);

    // ---- run 2: newer events appear; forward sweep picks them up, backfill finishes.
    let newer: Vec<RawEvent> = (1000..1030)
        .map(|i| torrent(&publisher, i, t0 - 100 - i64::from((i - 1000) / 2)))
        .collect();
    let late_comment = sign(
        &commenter,
        2004,
        t0 - 50,
        &[&["e", &newer[0].id, "", "root"]],
        "new",
    );
    relay.add(newer.iter().cloned());
    relay.add([late_comment]);
    let mut cfg2 = cfg(&[&url]);
    cfg2.max_backfill_pages = 10;
    let before = relay.reqs().len();
    let r2 = crawl_once(&store, &cfg2).await.unwrap();
    assert!(r2.errors.is_empty(), "{:?}", r2.errors);
    let valid_fixtures = 51;
    assert_eq!(store.torrent_count(), 250 + valid_fixtures + 30);
    // 25 + 6 generated comments, plus the late one; never the K=1 comment.
    assert_eq!(store.comment_count(), 32);
    assert!(r2.duplicates > 0, "{r2:?}");
    // History complete: the comment streams were swept, the squatting 2004s rejected as orphans.
    assert!(
        relay.reqs()[before..]
            .iter()
            .any(|f| f["#K"] == serde_json::json!(["2003"]))
    );
    assert!(r2.rejected >= fixture_comments.len() as u64);
    assert!(
        store
            .cursor(&url, KIND_TORRENT_COMMENT)
            .unwrap()
            .backfill_done
    );
    let c = store.cursor(&url, KIND_TORRENT).unwrap();
    assert!(c.backfill_done);
    assert_eq!(c.newest_seen, Some(relay_order(&newer)[0].created_at));
    // The forward sweep started at newest_seen - overlap.
    let first_forward = &relay.reqs()[before];
    assert_eq!(kinds_of(first_forward), [2003]);
    assert_eq!(
        first_forward["since"].as_i64(),
        Some(sorted[0].created_at - 3600)
    );
    assert!(first_forward.get("until").is_none());

    // ---- run 3: nothing new.
    let r3 = crawl_once(&store, &cfg2).await.unwrap();
    assert_eq!(r3.inserted, 0, "{r3:?}");
    assert!(r3.duplicates > 0);
    assert_eq!(store.torrent_count(), 331);

    // Backfill cursors were written after every page.
    let writes = store.with(|i| {
        i.cursor_writes
            .iter()
            .filter(|c| c.kind == KIND_TORRENT && !c.backfill_done)
            .count()
    });
    assert!(writes >= 4, "{writes}");
}

#[tokio::test]
async fn budget_backoff_and_disabled_relays() {
    let silent = FakeRelay::start(
        vec![torrent(&Keys::generate(), 1, now() - 10)],
        Behaviour {
            silent: true,
            ..Behaviour::default()
        },
    )
    .await;
    let dead = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        format!("ws://{}", l.local_addr().unwrap())
    };
    let store = MemStore::default();
    let mut c = cfg(&[&silent.url, &dead]);
    c.budget = Duration::from_secs(3);
    let started = Instant::now();
    let r = crawl_once(&store, &c).await.unwrap();
    let took = started.elapsed();
    assert!(
        took >= Duration::from_secs(2) && took < Duration::from_secs(8),
        "{took:?}"
    );
    assert_eq!((r.relays_tried, r.relays_ok), (2, 1), "{r:?}");
    assert_eq!(r.inserted, 0);
    assert!(
        r.errors.iter().any(|e| e.starts_with(&dead)),
        "{:?}",
        r.errors
    );
    let d = store.relay(&dead).unwrap();
    assert_eq!(d.consecutive_failures, 1);
    assert!(d.next_try_at > now());
    // The silent relay never answered: its cursor did not move.
    assert!(store.cursor(&silent.url, KIND_TORRENT).is_none());

    // Second run: the dead relay is backing off, the disabled one is skipped.
    store.set_relay_enabled(&silent.url, false).await.unwrap();
    let r = crawl_once(&store, &c).await.unwrap();
    assert_eq!(r.relays_tried, 0, "{r:?}");
}

/// A relay that refuses every REQ (NIP-01 `restricted:`) is backed off like a dead one, so a
/// continuous crawl does not revisit it every pass (D32).
#[tokio::test]
async fn refusing_relay_backs_off() {
    let refusing = FakeRelay::start(
        vec![torrent(&Keys::generate(), 1, now() - 10)],
        Behaviour {
            closed_reqs: (1..=50).collect(),
            closed_reason: "restricted: not a member",
            ..Behaviour::default()
        },
    )
    .await;
    let store = MemStore::default();
    let c = cfg(&[&refusing.url]);
    let r = crawl_once(&store, &c).await.unwrap();
    assert_eq!((r.relays_tried, r.relays_ok), (1, 0), "{r:?}");
    // The first backfill REQ and its one retry; then nothing more is asked of it this run.
    assert_eq!(refusing.reqs().len(), 2);
    assert!(
        r.errors
            .iter()
            .any(|e| e.contains("restricted: not a member")),
        "{:?}",
        r.errors
    );
    let info = store.relay(&refusing.url).unwrap();
    assert_eq!(info.consecutive_failures, 1);
    assert!(info.next_try_at > now());
    assert!(info.last_ok.is_none());

    // The next pass skips it.
    let r = crawl_once(&store, &c).await.unwrap();
    assert_eq!(r.relays_tried, 0, "{r:?}");
    assert_eq!(refusing.state.lock().unwrap().connections, 1);
}

#[tokio::test]
async fn metadata_hints_and_discovery() {
    let root = Keys::generate();
    let publisher = Keys::generate();
    let (root_hex, pub_hex) = (root.public_key().to_hex(), publisher.public_key().to_hex());
    let t0 = now();

    let mut torrents: Vec<RawEvent> = (0..3)
        .map(|i| torrent(&publisher, i, t0 - 100 - i64::from(i)))
        .collect();
    torrents.push(sign(
        &publisher,
        2003,
        t0 - 200,
        &[
            &["x", "77dcbd4597cb43f549fd1433d516a696db30044c"],
            &["p", &root_hex, "wss://hint.example.com/"],
        ],
        "",
    ));
    let seed = FakeRelay::start(torrents, Behaviour::default()).await;

    let meta = vec![
        sign(
            &root,
            3,
            t0 - 50,
            &[&["p", &pub_hex]],
            r#"{"wss://legacy.example.net/":{"read":true,"write":true}}"#,
        ),
        sign(
            &root,
            1984,
            t0 - 40,
            &[&["p", &"dd".repeat(32), "spam"]],
            "",
        ),
        sign(
            &root,
            30007,
            t0 - 30,
            &[&["d", "2003"], &["p", &"dd".repeat(32)]],
            "",
        ),
        sign(
            &publisher,
            10002,
            t0 - 20,
            &[
                &["r", "wss://relay.example.org/"],
                &["r", "wss://read.example.org", "read"],
                &["r", "ws://127.0.0.1:1", "write"],
            ],
            "",
        ),
        sign(&publisher, 0, t0 - 10, &[], r#"{"name":"pub"}"#),
        // Not requested kind: never stored.
        sign(&publisher, 1, t0 - 5, &[], "note"),
    ];
    let indexer = FakeRelay::start(meta, Behaviour::default()).await;

    let store = MemStore::default();
    let mut c = cfg(&[&seed.url]);
    c.max_relays = 1;
    c.indexers = vec![indexer.url.clone()];
    c.fetch_metadata = true;
    c.root = Some(root_hex.clone());
    let r = crawl_once(&store, &c).await.unwrap();
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert_eq!(store.torrent_count(), 4);
    assert_eq!(r.metadata_events, 5, "{r:?}");
    assert_eq!(
        store.follows_of(&root_hex).await.unwrap(),
        vec![pub_hex.clone()]
    );

    let reqs = indexer.reqs();
    let wanted: Vec<u64> = METADATA_KINDS.iter().map(|k| u64::from(*k)).collect();
    assert!(reqs.iter().all(|f| kinds_of(f) == wanted));
    assert!(
        reqs.iter()
            .all(|f| f["authors"].as_array().unwrap().len() <= 250)
    );
    assert_eq!(reqs[0]["authors"], serde_json::json!([root_hex]));
    assert!(
        reqs.iter()
            .any(|f| f["authors"] == serde_json::json!([pub_hex]))
    );

    let relays = store.list_relays().await.unwrap();
    let source = |u: &str| relays.iter().find(|r| r.url == u).map(|r| r.source.clone());
    assert_eq!(source("wss://relay.example.org").as_deref(), Some("nip65"));
    assert_eq!(source("wss://legacy.example.net").as_deref(), Some("kind3"));
    assert_eq!(source("wss://hint.example.com").as_deref(), Some("hint"));
    assert_eq!(source("wss://read.example.org"), None);
    assert_eq!(source("ws://127.0.0.1:1"), None);

    // Next run: the discovered write relay is a crawl candidate after the seeds.
    // (Disabled here so the test stays offline; the seed alone is crawled.)
    for r in &relays {
        if r.url != seed.url {
            store.set_relay_enabled(&r.url, false).await.unwrap();
        }
    }
    c.max_relays = 5;
    c.fetch_metadata = false;
    let r = crawl_once(&store, &c).await.unwrap();
    assert_eq!(r.relays_tried, 1);
}

#[tokio::test]
#[ignore = "network: live relays"]
async fn live_crawl_oxtr_and_nos_lol() {
    magnetar_core::http::install_crypto_provider();
    let store = MemStore::default();
    let mut c = CrawlConfig::phone_default();
    c.seed_relays = vec!["wss://nostr.oxtr.dev".into(), "wss://nos.lol".into()];
    c.max_relays = 2;
    c.budget = Duration::from_secs(60);
    let started = Instant::now();
    let r = crawl_once(&store, &c).await.unwrap();
    println!("live crawl report: {r:#?}");
    println!(
        "elapsed {:?}, torrents {}, comments {}, relays known {}",
        started.elapsed(),
        store.torrent_count(),
        store.comment_count(),
        store.list_relays().await.unwrap().len()
    );
    for (relay, kind) in [("wss://nostr.oxtr.dev", 2003), ("wss://nos.lol", 2003)] {
        println!("cursor {relay}: {:?}", store.cursor(relay, kind));
    }
    assert!(started.elapsed() < Duration::from_secs(70));
    assert!(r.relays_ok >= 1);
    assert!(store.torrent_count() > 0);
}

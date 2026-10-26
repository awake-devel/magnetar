//! Crawler against in-process fake relays: pagination, cursors, dedup, resume, comments,
//! budget, failures, discovery and metadata. One live test (ignored).

use crate::support;

use std::collections::HashSet;
use std::time::{Duration, Instant};

use magnetar_core::crawler::{CrawlConfig, METADATA_KINDS, crawl_once, select_relays};
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
/// continuous crawl does not revisit it every pass (D34).
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

// ---- relay selection (D32)

/// Pretend `url` was crawled at `at` with `found` new events (ok) or failed.
async fn crawled(store: &MemStore, url: &str, ok: bool, found: u64, at: i64) {
    store
        .record_relay_result(url, ok, (!ok).then_some("boom"), found, at)
        .await
        .unwrap();
}

fn assert_capped(sel: &[String], max: usize) {
    assert!(sel.len() <= max, "{sel:?} exceeds {max}");
    let unique: HashSet<&String> = sel.iter().collect();
    assert_eq!(unique.len(), sel.len(), "duplicates in {sel:?}");
}

fn sel_cfg(seeds: &[&str], max: usize, explore: usize) -> CrawlConfig {
    let mut c = cfg(seeds);
    c.max_relays = max;
    c.explore_relays = explore;
    c
}

#[tokio::test]
async fn select_caps_dedups_and_clamps_explore() {
    let store = MemStore::default();
    for i in 0..10 {
        store
            .upsert_relay(&format!("wss://r{i}.example.com"), "hint")
            .await
            .unwrap();
    }
    let seeds = ["wss://nos.lol", "wss://NOS.lol/", " wss://nos.lol "];
    for (max, explore) in [(0, 0), (0, 3), (1, 0), (3, 7), (5, 5), (8, 2), (40, 100)] {
        let sel = select_relays(&store, &sel_cfg(&seeds, max, explore), 1000)
            .await
            .unwrap();
        assert_capped(&sel, max);
        assert_eq!(
            sel.len(),
            max.min(11),
            "max {max} explore {explore}: {sel:?}"
        );
        if max > 0 {
            // Fresh install: nothing proven yet, the seed leads.
            assert_eq!(sel[0], "wss://nos.lol");
        }
    }
}

#[tokio::test]
async fn seeds_beyond_the_cap_are_rotated_in_not_cut() {
    // The phone shape: 12 default seeds, 8 slots, 2 of them for exploration.
    let store = MemStore::default();
    let c = CrawlConfig::phone_default();
    assert!(c.seed_relays.len() > c.max_relays);
    let first = select_relays(&store, &c, 1000).await.unwrap();
    assert_eq!(
        first,
        c.seed_relays[..8].to_vec(),
        "fresh install: seeds in order"
    );
    // Half of them yield, the rest connect fine with nothing new.
    for (i, url) in first.iter().enumerate() {
        crawled(&store, url, true, if i % 2 == 0 { 10 } else { 0 }, 1000).await;
    }
    let second = select_relays(&store, &c, 2000).await.unwrap();
    assert_capped(&second, 8);
    // 4 proven seeds fill the 6 exploit slots as far as they go; the 4 never-tried seeds follow.
    for url in first.iter().step_by(2).chain(&c.seed_relays[8..]) {
        assert!(second.contains(url), "{url} missing from {second:?}");
    }
    let all: HashSet<&String> = first.iter().chain(&second).collect();
    assert_eq!(all.len(), 12, "every seed crawled within two passes");
}

#[tokio::test]
async fn top_yielders_stay_while_zero_yield_relays_rotate() {
    let store = MemStore::default();
    let top = ["wss://big.example.com", "wss://mid.example.com"];
    for (url, found) in top.iter().zip([500, 20]) {
        store.upsert_relay(url, "hint").await.unwrap();
        crawled(&store, url, true, found, 10).await;
    }
    let zero: Vec<String> = (0..21)
        .map(|i| format!("wss://z{i:02}.example.com"))
        .collect();
    for url in &zero {
        store.upsert_relay(url, "hint").await.unwrap();
    }
    let c = sel_cfg(&[], 5, 2);
    let mut seen: Vec<String> = Vec::new();
    let mut t = 100;
    // 21 zero-yield relays, 3 non-proven slots per pass (2 explore + 1 spare exploit slot).
    for pass in 0..7 {
        let sel = select_relays(&store, &c, t).await.unwrap();
        assert_capped(&sel, 5);
        assert_eq!(sel.len(), 5);
        assert_eq!(sel[..2], top, "pass {pass}: top yielders first");
        for url in &sel[2..] {
            assert!(
                !seen.contains(url),
                "pass {pass}: {url} again before the others"
            );
            seen.push(url.clone());
            crawled(&store, url, true, 0, t).await;
        }
        for url in &sel[..2] {
            crawled(&store, url, true, 0, t).await;
        }
        t += 100;
        if seen.len() == zero.len() {
            break;
        }
    }
    seen.sort();
    assert_eq!(seen, zero, "all zero-yield relays tried");
    // Second round: the least recently looked at go first again (z00.. were tried first).
    let sel = select_relays(&store, &c, t).await.unwrap();
    assert_eq!(
        sel[2..],
        [
            "wss://z00.example.com",
            "wss://z01.example.com",
            "wss://z02.example.com"
        ]
    );

    // A failing relay sits out its backoff, then queues behind relays looked at before it ended.
    crawled(&store, "wss://z00.example.com", false, 0, t).await;
    let backoff_end = store
        .list_relays()
        .await
        .unwrap()
        .into_iter()
        .find(|r| r.url == "wss://z00.example.com")
        .unwrap()
        .next_try_at;
    let sel = select_relays(&store, &c, t).await.unwrap();
    assert!(
        !sel.contains(&"wss://z00.example.com".to_string()),
        "{sel:?}"
    );
    let sel = select_relays(&store, &c, backoff_end).await.unwrap();
    assert!(
        !sel.contains(&"wss://z00.example.com".to_string()),
        "{sel:?}"
    );
    assert_eq!(
        sel[2..],
        [
            "wss://z01.example.com",
            "wss://z02.example.com",
            "wss://z03.example.com"
        ]
    );
}

#[tokio::test]
async fn proven_relays_fill_unused_explore_slots_and_vice_versa() {
    let store = MemStore::default();
    let proven: Vec<String> = (0..6).map(|i| format!("wss://p{i}.example.com")).collect();
    for (i, url) in proven.iter().enumerate() {
        store.upsert_relay(url, "hint").await.unwrap();
        crawled(&store, url, true, 100 - i as u64, 10).await;
    }
    // No exploration candidate at all: every slot goes to the ranked list.
    let sel = select_relays(&store, &sel_cfg(&[], 4, 3), 100)
        .await
        .unwrap();
    assert_eq!(sel, proven[..4]);
    // One candidate: it takes one explore slot, the other two return to the ranked list.
    store
        .upsert_relay("wss://new.example.com", "hint")
        .await
        .unwrap();
    let sel = select_relays(&store, &sel_cfg(&[], 4, 3), 100)
        .await
        .unwrap();
    assert_eq!(
        sel,
        [
            "wss://p0.example.com",
            "wss://new.example.com",
            "wss://p1.example.com",
            "wss://p2.example.com"
        ]
    );
    // explore = max: only unproven relays, then proven ones as filler.
    let sel = select_relays(&store, &sel_cfg(&[], 2, 2), 100)
        .await
        .unwrap();
    assert_eq!(sel, ["wss://new.example.com", "wss://p0.example.com"]);
}

#[tokio::test]
async fn proven_relays_below_the_cutoff_still_rotate() {
    // The review case: 3 exploit slots, 4 proven relays, 10 zero-yield relays that connect fine
    // every pass. p3 must not be starved by the never-ending supply of zero-yield relays.
    let store = MemStore::default();
    let proven: Vec<String> = (0..4).map(|i| format!("wss://p{i}.example.com")).collect();
    for (url, found) in proven.iter().zip([100, 50, 10, 1]) {
        store.upsert_relay(url, "hint").await.unwrap();
        crawled(&store, url, true, found, 10).await;
    }
    for i in 0..10 {
        store
            .upsert_relay(&format!("wss://z{i}.example.com"), "hint")
            .await
            .unwrap();
    }
    let c = sel_cfg(&[], 5, 2);
    let p3 = &proven[3];
    let mut picks = Vec::new();
    let mut t = 100;
    for pass in 0..50 {
        let sel = select_relays(&store, &c, t).await.unwrap();
        assert_capped(&sel, 5);
        assert_eq!(sel[..3], proven[..3], "pass {pass}: top yielders first");
        if sel.contains(p3) {
            picks.push(pass);
        }
        for url in &sel {
            crawled(&store, url, true, 0, t).await;
        }
        t += 100;
    }
    // 11 relays share 2 rotating slots: the 10 never tried go first (passes 0-4), then p3, which
    // was looked at longest ago, then everyone in turn (one turn every 5-6 passes).
    assert_eq!(picks[0], 5, "{picks:?}");
    assert!(
        picks.windows(2).all(|w| w[1] - w[0] <= 6),
        "p3 starved: {picks:?}"
    );
    assert!(picks.len() >= 8, "{picks:?}");

    // A relay discovered later still goes before anything already crawled.
    store
        .upsert_relay("wss://new.example.com", "nip65")
        .await
        .unwrap();
    let sel = select_relays(&store, &c, t).await.unwrap();
    assert_eq!(sel[3], "wss://new.example.com", "{sel:?}");
}

#[tokio::test]
async fn select_skips_disabled_backed_off_and_private_rows() {
    let store = MemStore::default();
    for (url, source) in [
        ("wss://ok.example.com", "hint"),
        ("wss://off.example.com", "nip65"),
        ("wss://down.example.com", "kind3"),
        // Stored before the normalizer rejected private TLDs.
        ("ws://apollo.home:3355", "nip65"),
        ("ws://192.168.1.5:7777", "hint"),
        // Trusted sources may point anywhere.
        ("ws://nas.lan:7777", "manual"),
    ] {
        store.upsert_relay(url, source).await.unwrap();
    }
    store
        .set_relay_enabled("wss://off.example.com", false)
        .await
        .unwrap();
    crawled(&store, "wss://down.example.com", false, 0, 1000).await;
    let c = sel_cfg(&["ws://127.0.0.1:7777"], 10, 2);
    let mut sel = select_relays(&store, &c, 1000).await.unwrap();
    sel.sort();
    assert_eq!(
        sel,
        [
            "ws://127.0.0.1:7777",
            "ws://nas.lan:7777",
            "wss://ok.example.com"
        ]
    );
}

#[tokio::test]
async fn nip65_relays_discovered_this_pass_are_selected() {
    let store = MemStore::default();
    let publisher = Keys::generate();
    let t0 = now();
    for ev in [
        torrent(&publisher, 1, t0 - 100),
        sign(
            &publisher,
            10002,
            t0 - 50,
            &[
                &["r", "wss://fresh.example.org/"],
                &["r", "ws://apollo.home:3355"],
                &["r", "wss://inbox.example.org", "read"],
            ],
            "",
        ),
    ] {
        magnetar_core::ingest::ingest_into(&store, ev, t0)
            .await
            .unwrap();
    }
    // Plenty of proven relays: exploration still makes room for the new one.
    for i in 0..5 {
        let url = format!("wss://p{i}.example.com");
        store.upsert_relay(&url, "hint").await.unwrap();
        crawled(&store, &url, true, 50, t0 - 10).await;
    }
    let sel = select_relays(&store, &sel_cfg(&[], 3, 1), t0)
        .await
        .unwrap();
    assert_eq!(sel.len(), 3);
    assert!(
        sel.contains(&"wss://fresh.example.org".to_string()),
        "{sel:?}"
    );
    let relays = store.list_relays().await.unwrap();
    let known = |u: &str| relays.iter().find(|r| r.url == u).map(|r| r.source.clone());
    assert_eq!(known("wss://fresh.example.org").as_deref(), Some("nip65"));
    assert_eq!(known("ws://apollo.home:3355"), None);
    assert_eq!(known("wss://inbox.example.org"), None);
}

#[tokio::test]
async fn continuous_crawl_rotates_through_new_and_idle_relays() {
    let publisher = Keys::generate();
    let t0 = now();
    let busy = FakeRelay::start(
        (0..5)
            .map(|i| torrent(&publisher, i, t0 - 100 - i64::from(i)))
            .collect(),
        Behaviour::default(),
    )
    .await;
    let mut idle = Vec::new();
    for _ in 0..4 {
        idle.push(FakeRelay::start(Vec::new(), Behaviour::default()).await);
    }
    let store = MemStore::default();
    for r in &idle {
        // Loopback URLs only pass selection as trusted rows.
        store.upsert_relay(&r.url, "manual").await.unwrap();
    }
    let mut c = cfg(&[&busy.url]);
    c.max_relays = 3;
    c.explore_relays = 2;
    let conns = |r: &FakeRelay| r.state.lock().unwrap().connections;

    // Pass 1: nothing proven yet; the seed leads, two idle relays fill the rest.
    let r = crawl_once(&store, &c).await.unwrap();
    assert_eq!(r.relays_tried, 3, "{r:?}");
    assert_eq!(conns(&busy), 1);
    assert_eq!(store.torrent_count(), 5);
    // Pass 2: the busy relay is proven and stays; the two untried idle relays are next.
    let r = crawl_once(&store, &c).await.unwrap();
    assert_eq!(r.relays_tried, 3, "{r:?}");
    assert_eq!(conns(&busy), 2);
    assert!(
        idle.iter().all(|r| conns(r) == 1),
        "every idle relay tried once"
    );

    // A relay discovered between passes is crawled on the very next pass.
    let late = FakeRelay::start(Vec::new(), Behaviour::default()).await;
    store.upsert_relay(&late.url, "manual").await.unwrap();
    let r = crawl_once(&store, &c).await.unwrap();
    assert_eq!(r.relays_tried, 3, "{r:?}");
    assert_eq!(conns(&busy), 3);
    assert_eq!(conns(&late), 1);
    assert_eq!(idle.iter().map(conns).sum::<usize>(), 5);
}

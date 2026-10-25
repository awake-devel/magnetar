//! NIP-66 discovery: classification of real kind-30166 events (docs/research/fixtures) and the
//! fetch + upsert path against in-process fake relays.

mod support;

use std::time::Duration;

use magnetar_core::model::RawEvent;
use magnetar_core::nip66::{self, Nip66Config, Nip66Report, Observation, Skip, Verdict, classify};
use magnetar_core::store::Store;
use nostr::prelude::Keys;
use serde_json::Value;
use support::fake_relay::{Behaviour, FakeRelay};
use support::store::MemStore;
use support::*;

fn verdict_of(events: &[RawEvent], prefix: &str) -> Verdict {
    let ev = events.iter().find(|e| e.id.starts_with(prefix)).unwrap();
    classify(ev)
}

#[test]
fn real_fixtures_verify_and_classify() {
    let events = load_jsonl("kind30166_sample.jsonl");
    assert_eq!(events.len(), 39);
    for ev in &events {
        ev.verify().unwrap_or_else(|e| panic!("{}: {e}", ev.id));
        assert_eq!(ev.kind, nip66::KIND_RELAY_DISCOVERY);
    }
    let relay = |s: &str| Verdict::Relay(s.to_string());
    let restricted = |s: &str| Verdict::Restricted(s.to_string());
    // Trailing slash dropped; n=clearnet, R !auth/!payment/!pow.
    assert_eq!(
        verdict_of(&events, "8b0bd0d8"),
        relay("wss://relay.21mil.me")
    );
    // Monitor that sets neither n nor R.
    assert_eq!(
        verdict_of(&events, "7ada01b9"),
        relay("wss://poe.feeds.relay.tools")
    );
    // Paths and ports survive, case of the path too.
    assert_eq!(
        verdict_of(&events, "141ef784"),
        relay("wss://o-gits.com:8443/relay")
    );
    assert_eq!(
        verdict_of(&events, "18a44104"),
        relay("wss://mysats.xyz/nostrrelay/Relai")
    );
    // Plain ws:// on a public IP is a usable clearnet relay.
    assert_eq!(
        verdict_of(&events, "8b2e4146"),
        relay("ws://89.167.52.249:7000")
    );
    // R auth / payment.
    assert_eq!(
        verdict_of(&events, "a75142f8"),
        restricted("wss://podsystems.nostr1.com")
    );
    assert_eq!(
        verdict_of(&events, "a6e5425f"),
        restricted("wss://filter.nostr.wine")
    );
    assert_eq!(
        verdict_of(&events, "5ee879c5"),
        restricted("wss://relay.nostrview.com")
    );
    // tor and i2p.
    for id in ["5fc17bc8", "66a58186", "307eb8bb", "97d5825a"] {
        assert_eq!(
            verdict_of(&events, id),
            Verdict::Skip(Skip::Network),
            "{id}"
        );
    }

    let mut report = Nip66Report::default();
    let obs: Vec<Observation> = events
        .iter()
        .map(|e| {
            Observation::of(
                e,
                nip66::DEFAULT_TRUSTED_MONITORS.contains(&e.pubkey.as_str()),
            )
        })
        .collect();
    let urls: Vec<String> = nip66::candidates(&obs, &mut report)
        .into_iter()
        .map(|c| c.url)
        .collect();
    // podsystems is reported restricted by two monitors and must not come back.
    assert!(!urls.contains(&"wss://podsystems.nostr1.com".to_string()));
    assert!(urls.contains(&"wss://relay.21mil.me".to_string()));
    assert_eq!(report.skipped, 7);
    assert_eq!(report.restricted, 7);
    assert_eq!(report.candidates as usize, urls.len());
    assert_eq!(urls.len(), 24);
}

#[test]
fn real_announcements_verify() {
    let events = load_jsonl("kind10166_sample.jsonl");
    assert_eq!(events.len(), 4);
    for ev in &events {
        ev.verify().unwrap();
        assert_eq!(ev.kind, nip66::KIND_MONITOR_ANNOUNCEMENT);
        assert!(ev.tag("frequency").is_some());
    }
}

fn cfg(relays: &[&FakeRelay]) -> Nip66Config {
    Nip66Config {
        monitor_relays: relays.iter().map(|r| r.url.clone()).collect(),
        page_timeout: Duration::from_secs(5),
        budget: Duration::from_secs(20),
        trusted_monitors: Vec::new(),
        max_new_open: 10_000,
        ..Nip66Config::default()
    }
}

fn discovery(keys: &Keys, created_at: i64, url: &str, extra: &[&[&str]]) -> RawEvent {
    let d = ["d", url];
    let mut tags: Vec<&[&str]> = vec![&d, &["n", "clearnet"]];
    tags.extend_from_slice(extra);
    sign(keys, nip66::KIND_RELAY_DISCOVERY, created_at, &tags, "")
}

#[tokio::test]
async fn discovers_from_announced_monitors_only() {
    let t0 = now();
    let monitor = Keys::generate();
    let other_monitor = Keys::generate();
    let stranger = Keys::generate();
    let announce = |k: &Keys| {
        sign(
            k,
            nip66::KIND_MONITOR_ANNOUNCEMENT,
            t0 - 90 * 86_400,
            &[&["frequency", "3600"], &["c", "open"]],
            "",
        )
    };

    // 250 relays from one monitor, several per second, so pages of 100 straddle ties.
    let mut a_events: Vec<RawEvent> = (0..250)
        .map(|i| {
            discovery(
                &monitor,
                t0 - 100 - i / 4,
                &format!("wss://r{i}.example.com/"),
                &[],
            )
        })
        .collect();
    a_events.push(announce(&monitor));
    a_events.push(discovery(
        &monitor,
        t0 - 50,
        "wss://paid.example.com/",
        &[&["R", "payment"]],
    ));
    a_events.push(discovery(
        &monitor,
        t0 - 8 * 86_400,
        "wss://stale.example.com",
        &[],
    ));
    a_events.push(discovery(&stranger, t0 - 10, "wss://spam.example.com", &[]));
    let mut forged = discovery(&monitor, t0 - 10, "wss://forged.example.com", &[]);
    forged.tags[0][1] = "wss://evil.example.com".into();
    a_events.push(forged);

    // The second monitor announces on relay B only but publishes on relay A too; relay B also
    // repeats an event relay A has.
    let r0_again = a_events[0].clone();
    a_events.push(discovery(
        &other_monitor,
        t0 - 20,
        "wss://r7.example.com",
        &[],
    ));
    let b_events = vec![
        announce(&other_monitor),
        r0_again,
        discovery(&other_monitor, t0 - 30, "wss://only-b.example.com", &[]),
        discovery(
            &other_monitor,
            t0 - 30,
            "wss://paid.example.com",
            &[&["R", "!payment"]],
        ),
    ];
    let a = FakeRelay::start(a_events, Behaviour::default()).await;
    let b = FakeRelay::start(b_events, Behaviour::default()).await;

    let store = MemStore::default();
    store
        .upsert_relay("wss://r1.example.com", "manual")
        .await
        .unwrap();
    store
        .set_relay_enabled("wss://r1.example.com", false)
        .await
        .unwrap();

    let report = nip66::discover(&store, &cfg(&[&a, &b])).await.unwrap();
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert_eq!(report.relays_ok, 2);
    assert_eq!(report.monitors, 2);
    assert_eq!(report.rejected, 1, "the forged event");
    // 250 + paid + r7 from A, only-b + paid from B (the duplicate r0 counted once).
    assert_eq!(report.events, 254);
    assert_eq!(report.restricted, 1);
    // r0..r249 (r7 from both monitors) and only-b.
    assert_eq!(report.candidates, 251);
    assert_eq!(report.inserted, 250, "r1 already existed");

    let relays = store.list_relays().await.unwrap();
    let get = |u: &str| relays.iter().find(|r| r.url == u);
    let r1 = get("wss://r1.example.com").unwrap();
    assert_eq!((r1.source.as_str(), r1.enabled), ("manual", false));
    assert_eq!(get("wss://r249.example.com").unwrap().source, "nip66");
    assert!(get("wss://only-b.example.com").is_some());
    for missing in [
        "wss://paid.example.com",
        "wss://stale.example.com",
        "wss://spam.example.com",
        "wss://evil.example.com",
        "wss://forged.example.com",
    ] {
        assert!(get(missing).is_none(), "{missing}");
    }

    // Requests: one 10166 REQ, then bounded 30166 pages by the announced authors only.
    let reqs = a.reqs();
    assert_eq!(reqs[0]["kinds"][0], 10166);
    let pages: Vec<&Value> = reqs.iter().filter(|f| f["kinds"][0] == 30166).collect();
    assert!(pages.len() >= 3, "{} pages", pages.len());
    for f in &pages {
        let authors = f["authors"].as_array().unwrap();
        assert_eq!(authors.len(), 2);
        assert!(f["since"].as_i64().unwrap() <= t0 - 7 * 86_400 + 5);
        assert_eq!(f["limit"], 500);
    }
    assert!(pages[1]["until"].is_i64());

    // A second run inserts nothing new.
    let again = nip66::discover(&store, &cfg(&[&a, &b])).await.unwrap();
    assert_eq!(again.inserted, 0);
}

#[tokio::test]
async fn caps_insertions_and_survives_dead_relays() {
    let t0 = now();
    let monitor = Keys::generate();
    let mut events: Vec<RawEvent> = (0..20)
        .map(|i| {
            discovery(
                &monitor,
                t0 - 100 - i,
                &format!("wss://r{i}.example.com"),
                &[],
            )
        })
        .collect();
    events.push(sign(
        &monitor,
        nip66::KIND_MONITOR_ANNOUNCEMENT,
        t0 - 1000,
        &[&["frequency", "3600"]],
        "",
    ));
    let good = FakeRelay::start(events, Behaviour::default()).await;
    let silent = FakeRelay::start(
        Vec::new(),
        Behaviour {
            silent: true,
            ..Behaviour::default()
        },
    )
    .await;
    let store = MemStore::default();
    let mut c = cfg(&[&good, &silent]);
    c.monitor_relays.push("ws://127.0.0.1:1".into());
    c.page_timeout = Duration::from_secs(1);
    c.max_new = 5;
    let report = nip66::discover(&store, &c).await.unwrap();
    assert_eq!(report.relays_ok, 1);
    assert_eq!(report.errors.len(), 2, "{:?}", report.errors);
    assert_eq!(report.candidates, 20);
    assert_eq!(report.inserted, 5);
    // Newest first when every relay has one monitor.
    let urls: Vec<String> = store
        .list_relays()
        .await
        .unwrap()
        .into_iter()
        .map(|r| r.url)
        .collect();
    for i in 0..5 {
        assert!(
            urls.contains(&format!("wss://r{i}.example.com")),
            "{urls:?}"
        );
    }
}

/// The eviction attack: hundreds of fresh 10166s from throwaway keys next to a trusted monitor
/// whose own announcement is months old (as the nostr.watch fleet's is).
#[tokio::test]
async fn sybil_monitors_cannot_evict_or_veto_trusted_ones() {
    let t0 = now();
    let real = Keys::generate();
    let real_hex = real.public_key().to_hex();
    let mut events = vec![sign(
        &real,
        nip66::KIND_MONITOR_ANNOUNCEMENT,
        t0 - 120 * 86_400,
        &[&["frequency", "14400"]],
        "",
    )];
    events.extend((0..30).map(|i| {
        discovery(
            &real,
            t0 - 200 - i,
            &format!("wss://real{i}.example.com"),
            &[],
        )
    }));
    for i in 0..300 {
        let k = Keys::generate();
        events.push(sign(
            &k,
            nip66::KIND_MONITOR_ANNOUNCEMENT,
            t0 - 5,
            &[&["frequency", "60"]],
            "",
        ));
        events.push(discovery(
            &k,
            t0 - 10,
            &format!("wss://junk{i}.example.com"),
            &[],
        ));
        // Newer than every trusted event, and trying to hide one of them.
        events.push(discovery(
            &k,
            t0 - 10,
            &format!("wss://real{}.example.com", i % 30),
            &[&["R", "auth"]],
        ));
    }
    let relay = FakeRelay::start(
        events,
        Behaviour {
            page_clamp: 500,
            ..Behaviour::default()
        },
    )
    .await;
    let store = MemStore::default();
    let mut c = cfg(&[&relay]);
    c.trusted_monitors = vec![real_hex.clone()];
    c.max_new_open = 7;
    let report = nip66::discover(&store, &c).await.unwrap();
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert_eq!(report.monitors, 251, "the trusted one plus 250 open");
    assert_eq!(report.trusted, 1);
    assert_eq!(report.inserted, 37);
    let urls: Vec<String> = store
        .list_relays()
        .await
        .unwrap()
        .into_iter()
        .map(|r| r.url)
        .collect();
    for i in 0..30 {
        assert!(
            urls.contains(&format!("wss://real{i}.example.com")),
            "real{i}"
        );
    }
    assert_eq!(urls.iter().filter(|u| u.contains("junk")).count(), 7);

    // The trusted monitor has its own filter, sent before any open one.
    let pages: Vec<Value> = relay
        .reqs()
        .into_iter()
        .filter(|f| f["kinds"][0] == 30166)
        .collect();
    assert_eq!(pages[0]["authors"], serde_json::json!([real_hex]));
    let open = pages
        .iter()
        .find(|f| f["authors"].as_array().unwrap().len() > 1)
        .unwrap();
    assert_eq!(open["authors"].as_array().unwrap().len(), 250);
    assert!(
        !open["authors"]
            .as_array()
            .unwrap()
            .contains(&Value::from(real_hex))
    );
}

#[tokio::test]
#[ignore = "network"]
async fn live_monitors() {
    let store = MemStore::default();
    let report = nip66::discover(&store, &Nip66Config::default())
        .await
        .unwrap();
    eprintln!("{report:#?}");
    assert!(report.monitors > 0);
    // 12 of 12 answered on 2026-10-25; fewer means DEFAULT_TRUSTED_MONITORS needs a look.
    assert!(report.trusted >= 6, "{}", report.trusted);
    assert!(report.candidates > 100);
}

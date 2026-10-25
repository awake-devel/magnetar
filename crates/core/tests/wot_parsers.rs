//! WoT parsers on real fixtures (docs/research/fixtures) and synthetic edge cases.

use std::collections::HashSet;

use magnetar_core::model::{RawEvent, is_hex32};
use magnetar_core::wot::{
    RelayListEntry, follows_of, mutes_of, profile_name, relay_list_of, report_of,
};

fn fixture(name: &str) -> Vec<RawEvent> {
    let path = format!(
        "{}/../../docs/research/fixtures/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{path}: {e}"))
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| RawEvent::from_json(l).expect("fixture line parses"))
        .collect()
}

fn by_id<'a>(evs: &'a [RawEvent], prefix: &str) -> &'a RawEvent {
    evs.iter()
        .find(|e| e.id.starts_with(prefix))
        .unwrap_or_else(|| panic!("no fixture {prefix}"))
}

fn ev(kind: u16, tags: Vec<Vec<&str>>, content: &str) -> RawEvent {
    RawEvent {
        id: "ab".repeat(32),
        pubkey: "aa".repeat(32),
        created_at: 1_700_000_000,
        kind,
        tags: tags
            .into_iter()
            .map(|t| t.into_iter().map(String::from).collect())
            .collect(),
        content: content.into(),
        sig: "00".repeat(64),
    }
}

fn entry(url: &str, read: bool, write: bool) -> RelayListEntry {
    RelayListEntry {
        url: url.into(),
        read,
        write,
    }
}

#[test]
fn fixtures_are_real_signed_events() {
    for (name, kind, min) in [
        ("kind3_sample.jsonl", 3, 20),
        ("kind10000_sample.jsonl", 10000, 20),
        ("kind10002_sample.jsonl", 10002, 20),
    ] {
        let evs = fixture(name);
        assert!(evs.len() >= min, "{name}: {}", evs.len());
        for e in &evs {
            assert_eq!(e.kind, kind, "{name} {}", e.id);
            e.verify()
                .unwrap_or_else(|err| panic!("{name} {}: {err}", e.id));
        }
    }
}

#[test]
fn follows_from_real_kind3() {
    let evs = fixture("kind3_sample.jsonl");
    let mut total = 0;
    for e in &evs {
        let f = follows_of(e);
        assert!(f.iter().all(|p| is_hex32(p)), "{}", e.id);
        assert!(!f.contains(&e.pubkey), "self follow kept in {}", e.id);
        assert_eq!(f.iter().collect::<HashSet<_>>().len(), f.len(), "dupes");
        total += f.len();
    }
    assert!(total > 1000, "{total}");
    // d7607464… (a kind-2003 publisher): 1785 distinct p tags.
    let big = by_id(&evs, "0b9123a9812f");
    assert_eq!(follows_of(big).len(), 1785);
    // Many real clients add a self-follow (aef25970… from 91bea5cd…, a publisher).
    let selfy = by_id(&evs, "aef259706530");
    assert!(
        selfy
            .tags
            .iter()
            .any(|t| t[0] == "p" && t[1] == selfy.pubkey)
    );
    assert!(!follows_of(selfy).contains(&selfy.pubkey));
}

#[test]
fn follows_lenient_on_junk() {
    let me = "aa".repeat(32);
    let a = "0A".repeat(32);
    let b = "0b".repeat(32);
    let e = ev(
        3,
        vec![
            vec!["p", &a, "not a url", "bob"],
            vec!["p", &b, ""],
            vec!["p", &a.to_lowercase()],
            vec!["p", "deadbeef"],
            vec!["p"],
            vec!["p", &me],
            vec!["t", &b],
            vec!["e", &b],
        ],
        "",
    );
    assert_eq!(follows_of(&e), vec!["0a".repeat(32), b.clone()]);
    // Not a kind 3.
    assert!(follows_of(&ev(10000, vec![vec!["p", &b]], "")).is_empty());
}

#[test]
fn mutes_from_real_kind10000() {
    let evs = fixture("kind10000_sample.jsonl");
    // bb62335a…: a junk ["p","Mute List"] tag next to 24 valid pubkeys.
    let m = mutes_of(by_id(&evs, "bb62335a92c3"));
    assert_eq!(m.len(), 24);
    assert!(m.iter().all(|p| is_hex32(p)));
    // ffb44b65…: a p tag whose "relay hint" is a timestamp still counts.
    let e = by_id(&evs, "ffb44b65ae50");
    let m = mutes_of(e);
    assert_eq!(m.len(), 114);
    assert!(m.contains(&"fa67ba8278ccf76b436e18993dbeca830e3fc51d55bfa7c08c8c0596234392ba".into()));
    for e in &evs {
        assert!(mutes_of(e).iter().all(|p| is_hex32(p) && *p != e.pubkey));
    }
}

#[test]
fn kind_mute_set_only_for_torrents() {
    let target = "cc".repeat(32);
    let torrents = ev(30007, vec![vec!["d", "2003"], vec!["p", &target]], "");
    assert_eq!(mutes_of(&torrents), vec![target.clone()]);
    let notes = ev(30007, vec![vec!["d", "1"], vec!["p", &target]], "");
    assert!(mutes_of(&notes).is_empty());
    let no_d = ev(30007, vec![vec!["p", &target]], "");
    assert!(mutes_of(&no_d).is_empty());
    assert!(mutes_of(&ev(3, vec![vec!["p", &target]], "")).is_empty());
}

#[test]
fn reports() {
    let t = "cc".repeat(32);
    let e = ev(1984, vec![vec!["p", &t, "SPAM "]], "");
    assert_eq!(report_of(&e), Some((t.clone(), "spam".into())));
    // Type on the e tag (NIP-56 note report).
    let e = ev(
        1984,
        vec![vec!["e", &"dd".repeat(32), "malware"], vec!["p", &t]],
        "",
    );
    assert_eq!(report_of(&e), Some((t.clone(), "malware".into())));
    let e = ev(1984, vec![vec!["p", "junk", "spam"], vec!["p", &t]], "");
    assert_eq!(report_of(&e), Some((t.clone(), "other".into())));
    assert_eq!(report_of(&ev(1984, vec![vec!["e", &t, "spam"]], "")), None);
    // Self-reports and other kinds are ignored.
    assert_eq!(
        report_of(&ev(1984, vec![vec!["p", &"aa".repeat(32), "spam"]], "")),
        None
    );
    assert_eq!(report_of(&ev(1, vec![vec!["p", &t, "spam"]], "")), None);
}

#[test]
fn profile_names() {
    assert_eq!(
        profile_name(&ev(0, vec![], r#"{"name":" nmirror ","about":"x"}"#)),
        Some("nmirror".into())
    );
    assert_eq!(
        profile_name(&ev(0, vec![], r#"{"name":"","display_name":"Bob"}"#)),
        Some("Bob".into())
    );
    assert_eq!(
        profile_name(&ev(0, vec![], r#"{"displayName":"Eve"}"#)),
        Some("Eve".into())
    );
    assert_eq!(profile_name(&ev(0, vec![], "not json")), None);
    assert_eq!(profile_name(&ev(0, vec![], r#"{"name":5}"#)), None);
    assert_eq!(profile_name(&ev(3, vec![], r#"{"name":"x"}"#)), None);
    let long = format!(r#"{{"name":"{}"}}"#, "é".repeat(300));
    assert_eq!(
        profile_name(&ev(0, vec![], &long)).unwrap().chars().count(),
        100
    );
}

#[test]
fn not_a_relay_list() {
    assert_eq!(
        relay_list_of(&ev(1, vec![vec!["r", "wss://x.com"]], "")),
        (vec![], false)
    );
    // kind 3 without a JSON map (the normal case) has no relay list.
    assert_eq!(relay_list_of(&ev(3, vec![], "")), (vec![], false));
    assert_eq!(relay_list_of(&ev(3, vec![], "hello")), (vec![], false));
    assert_eq!(relay_list_of(&ev(3, vec![], "[1,2]")), (vec![], false));
}

#[test]
#[ignore = "post-merge"]
fn relay_list_markers_and_dupes() {
    let e = ev(
        10002,
        vec![
            vec!["r", "wss://relay.damus.io"],
            vec!["r", "wss://relay.damus.io/", "read"],
            vec!["r", "WSS://Nos.LOL:443/", "WRITE"],
            vec!["r", "wss://nos.lol", " Read "],
            vec!["r", "wss://inbox.nostr.wine/", "write"],
            vec!["r", "wss://nostr.wine/inbox"],
            vec!["r", "wss://bad.marker", "both"],
            vec!["r", "https://not-a-relay.com"],
            vec!["r"],
            vec!["client", "x"],
            vec!["published_at", "1"],
        ],
        "",
    );
    let (list, oversized) = relay_list_of(&e);
    assert!(!oversized);
    assert_eq!(
        list,
        vec![
            entry("wss://relay.damus.io", true, true),
            entry("wss://nos.lol", true, true),
            entry("wss://inbox.nostr.wine", false, true),
            entry("wss://nostr.wine/inbox", true, true),
        ]
    );
}

#[test]
#[ignore = "post-merge"]
fn relay_list_real_fixtures() {
    let evs = fixture("kind10002_sample.jsonl");
    // 90cbca96…: wss://relay.0xchat.com/ listed as read and as write → one read+write entry.
    let (list, oversized) = relay_list_of(by_id(&evs, "7e2fd1159973"));
    assert!(!oversized);
    assert_eq!(list.len(), 8);
    assert!(list.contains(&entry("wss://relay.0xchat.com", true, true)));
    assert!(list.contains(&entry("wss://nostr.mom", false, true)));
    assert!(list.iter().all(|e| !e.url.ends_with('/')));
    // Publishers 3117aca0… (2430 r tags) and 6aeb5506… (431): oversized spam lists.
    for id in ["4a6b923d2d03", "93013fcf9ced"] {
        let (list, oversized) = relay_list_of(by_id(&evs, id));
        assert!(oversized, "{id}");
        assert!(!list.is_empty());
    }
    let normal = evs.iter().filter(|e| !relay_list_of(e).1).count();
    assert_eq!(normal, evs.len() - 2);
    for e in &evs {
        let (list, _) = relay_list_of(e);
        let urls: HashSet<_> = list.iter().map(|e| &e.url).collect();
        assert_eq!(urls.len(), list.len(), "dupes in {}", e.id);
        assert!(list.iter().all(|e| e.read || e.write));
    }
}

#[test]
#[ignore = "post-merge"]
fn kind3_legacy_relay_map_fallback() {
    let evs = fixture("kind3_sample.jsonl");
    // d7607464… (publisher): 14-entry legacy content map, all read+write.
    let (list, oversized) = relay_list_of(by_id(&evs, "0b9123a9812f"));
    assert!(!oversized);
    assert_eq!(list.len(), 14);
    assert!(list.contains(&entry("wss://relay.damus.io", true, true)));
    let with_map = evs
        .iter()
        .filter(|e| !relay_list_of(e).0.is_empty())
        .count();
    assert!(with_map >= 10, "{with_map}");

    let e = ev(
        3,
        vec![],
        r#"{"wss://a.com/":{"read":true,"write":false},"wss://a.com":{"write":true},
            "wss://b.com":{"read":false,"write":false},"wss://c.com":true,"nope":{"read":true}}"#,
    );
    assert_eq!(
        relay_list_of(&e),
        (
            vec![
                entry("wss://a.com", true, true),
                entry("wss://c.com", true, true)
            ],
            false
        )
    );
}

#[test]
#[ignore = "post-merge"]
fn oversized_threshold_is_over_50() {
    let urls: Vec<String> = (0..51).map(|i| format!("wss://r{i}.example.com")).collect();
    let tags = |n: usize| urls[..n].iter().map(|u| vec!["r", u.as_str()]).collect();
    assert!(!relay_list_of(&ev(10002, tags(50), "")).1);
    let (list, oversized) = relay_list_of(&ev(10002, tags(51), ""));
    assert!(oversized);
    assert_eq!(list.len(), 51);
}

/// Live: fetch kind 3/10000 of fixture publishers from purplepag.es, verify and parse them.
#[tokio::test]
#[ignore = "network"]
async fn live_purplepages_wot_lists() {
    use futures::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;

    let authors: Vec<String> = fixture("kind3_sample.jsonl")
        .into_iter()
        .map(|e| e.pubkey)
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    let connector = tokio_tungstenite::Connector::Rustls(std::sync::Arc::new(
        magnetar_core::http::tls_config(),
    ));
    let cfg = tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default()
        .max_message_size(Some(16 << 20));
    let (mut ws, _) = tokio_tungstenite::connect_async_tls_with_config(
        "wss://purplepag.es",
        Some(cfg),
        true,
        Some(connector),
    )
    .await
    .expect("connect purplepag.es");
    let req = serde_json::json!(["REQ", "w", {"kinds": [3, 10000], "authors": authors}]);
    ws.send(Message::text(req.to_string())).await.unwrap();
    let (mut follows, mut lists) = (0, 0);
    while let Ok(Some(Ok(msg))) =
        tokio::time::timeout(std::time::Duration::from_secs(20), ws.next()).await
    {
        let Message::Text(t) = msg else { continue };
        let v: serde_json::Value = serde_json::from_str(t.as_str()).unwrap();
        match v[0].as_str() {
            Some("EVENT") => {
                let e: RawEvent = serde_json::from_value(v[2].clone()).unwrap();
                e.verify().unwrap();
                lists += 1;
                follows += follows_of(&e).len() + mutes_of(&e).len();
            }
            Some("EOSE") => break,
            _ => {}
        }
    }
    println!("live: {lists} lists, {follows} follow/mute edges");
    assert!(lists >= 5 && follows > 1000, "{lists} {follows}");
}

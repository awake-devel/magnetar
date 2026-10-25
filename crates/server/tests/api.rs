//! HTTP API + /relay WebSocket against an in-process server (needs MAGNETAR_TEST_DATABASE_URL).

mod common;

use std::time::Duration;

use common::*;
use futures::{SinkExt, StreamExt};
use magnetar_core::api::*;
use magnetar_core::model::*;
use magnetar_core::query::SearchPage;
use magnetar_core::store::Store;
use reqwest::StatusCode;
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

fn http() -> reqwest::Client {
    magnetar_core::http::client(Duration::from_secs(20)).unwrap()
}

async fn get(base: &str, path: &str) -> reqwest::Response {
    http()
        .get(format!("{base}{path}"))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap()
}

async fn error_code(resp: reqwest::Response) -> (StatusCode, String) {
    let status = resp.status();
    let body: ApiErrorBody = resp.json().await.unwrap();
    assert!(!body.message.is_empty());
    (status, body.error)
}

#[tokio::test]
async fn auth_required_everywhere_but_health() {
    let Some(db) = test_db().await else { return };
    let base = spawn_server(&db.store).await;
    let c = http();

    let h: Health = c
        .get(format!("{base}/health"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(h.status, "ok");
    assert!(h.db);
    assert_eq!(h.version, magnetar_core::VERSION);

    for path in [
        "/api/v1/stats",
        "/api/v1/search",
        "/api/v1/relays",
        "/relay",
        "/api/v1/downloads",
    ] {
        let r = c.get(format!("{base}{path}")).send().await.unwrap();
        assert_eq!(
            error_code(r).await,
            (StatusCode::UNAUTHORIZED, "unauthorized".into()),
            "{path}"
        );
        let r = c
            .get(format!("{base}{path}"))
            .bearer_auth("wrong")
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::UNAUTHORIZED, "{path}");
    }
    // ?token= is only honoured on /relay.
    let r = c
        .get(format!("{base}/api/v1/stats?token={TOKEN}"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
    let r = c
        .post(format!("{base}/api/v1/events/batch"))
        .json(&json!({"events": []}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::UNAUTHORIZED);

    let st: StatsResponse = get(&base, "/api/v1/stats").await.json().await.unwrap();
    assert_eq!(st.store.torrents, 0);
    assert!(!st.crawler.running);
    // Lowercase scheme works too.
    let r = c
        .get(format!("{base}/api/v1/stats"))
        .header("authorization", format!("bearer {TOKEN}"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
}

#[tokio::test]
async fn search_and_errors() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    put(
        s,
        torrent(1, 1, NOW - 10, 1, "Big.Buck.Bunny.2008.1080p")
            .cat(Category::Video)
            .size(10),
    )
    .await;
    put(
        s,
        torrent(2, 1, NOW - 5, 2, "Ubuntu 24.04").cat(Category::Application),
    )
    .await;
    let base = spawn_server(s).await;

    let page: SearchPage = get(&base, "/api/v1/search?q=bunny+1080p&min_trust=0")
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(page.total, 1);
    assert_eq!(page.hits[0].infohash, ih(1));
    let page: SearchPage = get(
        &base,
        "/api/v1/search?cat=application,audio&min_trust=0&sort=date&desc=true",
    )
    .await
    .json()
    .await
    .unwrap();
    assert_eq!(page.hits.len(), 1);
    assert_eq!(page.hits[0].title, "Ubuntu 24.04");
    // Default threshold 20 hides both (trust 0).
    let page: SearchPage = get(&base, "/api/v1/search").await.json().await.unwrap();
    assert_eq!(page.total, 0);

    let page: SearchPage = http()
        .post(format!("{base}/api/v1/search"))
        .bearer_auth(TOKEN)
        .json(&json!({"text": "ubuntu", "min_trust": 0}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(page.hits[0].title, "Ubuntu 24.04");

    assert_eq!(
        error_code(get(&base, "/api/v1/search?sort=bogus").await)
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        error_code(get(&base, "/api/v1/search?cat=nope").await)
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    let r = http()
        .post(format!("{base}/api/v1/search"))
        .bearer_auth(TOKEN)
        .header("content-type", "application/json")
        .body("{not json")
        .send()
        .await
        .unwrap();
    assert_eq!(
        error_code(r).await,
        (StatusCode::BAD_REQUEST, "invalid_body".into())
    );
    assert_eq!(
        error_code(get(&base, "/api/v1/torrents/xyz").await).await.0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        error_code(get(&base, &format!("/api/v1/torrents/{}", ih(99))).await).await,
        (StatusCode::NOT_FOUND, "not_found".into())
    );
    assert_eq!(
        error_code(get(&base, "/api/v1/nope").await).await.0,
        StatusCode::NOT_FOUND
    );
    let r = http()
        .post(format!("{base}/api/v1/torrents/{}/liveness", ih(99)))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::NOT_FOUND);

    // Body limit: 8 MiB.
    let big = format!("{{\"text\":\"{}\"}}", "a".repeat(9 << 20));
    let r = http()
        .post(format!("{base}/api/v1/search"))
        .bearer_auth(TOKEN)
        .header("content-type", "application/json")
        .body(big)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn settings_overrides_saved_searches_relays_downloads() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let base = spawn_server(s).await;
    let c = http();

    let ts: TrustSettings = get(&base, "/api/v1/settings/trust")
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(
        ts,
        TrustSettings {
            root: None,
            threshold: 20
        }
    );
    let root = hex64(0xabc);
    let ts: TrustSettings = c
        .put(format!("{base}/api/v1/settings/trust"))
        .bearer_auth(TOKEN)
        .json(&json!({"root": npub(&root), "threshold": 35}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        ts,
        TrustSettings {
            root: Some(root.clone()),
            threshold: 35
        }
    );
    let r = c
        .put(format!("{base}/api/v1/settings/trust"))
        .bearer_auth(TOKEN)
        .json(&json!({"root": null, "threshold": 101}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::BAD_REQUEST);
    let ts: TrustSettings = c
        .put(format!("{base}/api/v1/settings/trust"))
        .bearer_auth(TOKEN)
        .json(&json!({"root": null, "threshold": 0}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        ts,
        TrustSettings {
            root: None,
            threshold: 0
        }
    );

    let gv = s.graph_version().await.unwrap();
    let r = c
        .put(format!("{base}/api/v1/overrides/{}", npub(&hex64(5))))
        .bearer_auth(TOKEN)
        .json(&json!({"action": "block"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        s.overrides().await.unwrap(),
        vec![(hex64(5), OverrideAction::Block)]
    );
    assert!(s.graph_version().await.unwrap() > gv);
    let r = c
        .put(format!("{base}/api/v1/overrides/garbage"))
        .bearer_auth(TOKEN)
        .json(&json!({"action": null}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::BAD_REQUEST);

    let saved: SavedSearch = c
        .post(format!("{base}/api/v1/saved-searches"))
        .bearer_auth(TOKEN)
        .json(
            &json!({"id": 0, "name": "linux", "query": {"text": "linux"}, "notify": true,
                      "last_checked": null, "last_seen_created_at": null}),
        )
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(saved.id > 0);
    let list: Vec<SavedSearch> = get(&base, "/api/v1/saved-searches")
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(list, vec![saved.clone()]);
    let r = c
        .delete(format!("{base}/api/v1/saved-searches/{}", saved.id))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::NO_CONTENT);
    let list: Vec<SavedSearch> = get(&base, "/api/v1/saved-searches")
        .await
        .json()
        .await
        .unwrap();
    assert!(list.is_empty());

    s.upsert_relay("wss://nos.lol", "seed").await.unwrap();
    let relays: Vec<RelayInfo> = get(&base, "/api/v1/relays").await.json().await.unwrap();
    assert_eq!(relays.len(), 1);

    let r = get(&base, "/api/v1/downloads").await;
    assert_eq!(
        error_code(r).await,
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "downloads_unavailable".into()
        )
    );

    let r = c
        .post(format!("{base}/api/v1/events/batch"))
        .bearer_auth(TOKEN)
        .json(&BatchRequest {
            events: vec![torrent(1, 1, NOW, 1, "x").raw(); MAX_BATCH + 1],
        })
        .send()
        .await
        .unwrap();
    assert_eq!(error_code(r).await.0, StatusCode::PAYLOAD_TOO_LARGE);
    let empty: BatchResponse = c
        .post(format!("{base}/api/v1/events/batch"))
        .bearer_auth(TOKEN)
        .json(&BatchRequest::default())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(empty.results.is_empty());
}

// ---- /relay ----

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn ws_connect(base: &str) -> Ws {
    let url = format!("{}/relay?token={TOKEN}", base.replacen("http", "ws", 1));
    tokio_tungstenite::connect_async(url).await.unwrap().0
}

async fn send(ws: &mut Ws, v: Value) {
    ws.send(Message::text(v.to_string())).await.unwrap();
}

async fn recv(ws: &mut Ws) -> Value {
    loop {
        let m = tokio::time::timeout(Duration::from_secs(10), ws.next())
            .await
            .expect("relay answered in time")
            .unwrap()
            .unwrap();
        if let Message::Text(t) = m {
            return serde_json::from_str(t.as_str()).unwrap();
        }
    }
}

#[tokio::test]
async fn relay_req_eose_live_close() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let (_, a) = put(s, torrent(1, 1, NOW - 30, 1, "a").hashtag("movie")).await;
    let (_, b) = put(s, torrent(2, 2, NOW - 20, 2, "b")).await;
    let base = spawn_server(s).await;

    // Header auth works too.
    let mut req = format!("{}/relay", base.replacen("http", "ws", 1))
        .into_client_request()
        .unwrap();
    req.headers_mut()
        .insert("authorization", format!("Bearer {TOKEN}").parse().unwrap());
    let (mut hdr_ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    send(&mut hdr_ws, json!(["REQ", "h", {"ids": [a.id]}])).await;
    assert_eq!(recv(&mut hdr_ws).await[2]["id"], a.id);
    assert_eq!(recv(&mut hdr_ws).await, json!(["EOSE", "h"]));

    let mut ws = ws_connect(&base).await;
    send(
        &mut ws,
        json!(["REQ", "s1", {"kinds": [2003]}, {"#t": ["movie"]}]),
    )
    .await;
    let e1 = recv(&mut ws).await;
    assert_eq!(e1[0], "EVENT");
    assert_eq!(e1[1], "s1");
    let got: RawEvent = serde_json::from_value(e1[2].clone()).unwrap();
    assert_eq!(got, b, "newest first, verbatim");
    assert_eq!(
        recv(&mut ws).await[2]["id"],
        a.id,
        "deduplicated across filters"
    );
    assert_eq!(recv(&mut ws).await, json!(["EOSE", "s1"]));

    send(
        &mut ws,
        json!(["REQ", "only-movies", {"#t": ["movie"], "since": NOW}]),
    )
    .await;
    assert_eq!(recv(&mut ws).await, json!(["EOSE", "only-movies"]));

    // Live: stored after EOSE, delivered to matching subscriptions only.
    let (_, c) = put(s, torrent(3, 3, NOW, 3, "c")).await;
    let live = recv(&mut ws).await;
    assert_eq!(
        (live[0].as_str(), live[1].as_str()),
        (Some("EVENT"), Some("s1"))
    );
    assert_eq!(live[2]["id"], c.id);
    let (_, d) = put(s, torrent(4, 3, NOW, 4, "d").hashtag("movie")).await;
    let mut subs = [recv(&mut ws).await, recv(&mut ws).await];
    subs.sort_by_key(|m| m[1].as_str().unwrap().to_string());
    assert_eq!(subs[0][1], "only-movies");
    assert_eq!(subs[1][1], "s1");
    assert!(subs.iter().all(|m| m[2]["id"] == d.id));

    // CLOSE stops live delivery for that sub.
    send(&mut ws, json!(["CLOSE", "s1"])).await;
    send(&mut ws, json!(["CLOSE", "only-movies"])).await;
    // Round trip so the CLOSEs are processed before the next store write.
    send(&mut ws, json!(["REQ", "sync", {"ids": [hex64(0xbeef)]}])).await;
    assert_eq!(recv(&mut ws).await, json!(["EOSE", "sync"]));
    put(s, torrent(5, 3, NOW, 5, "e").hashtag("movie")).await;
    send(&mut ws, json!(["REQ", "probe", {"ids": [hex64(0xdead)]}])).await;
    assert_eq!(
        recv(&mut ws).await,
        json!(["EOSE", "probe"]),
        "nothing for closed subs"
    );

    // Errors.
    send(&mut ws, json!(["REQ", "bad", {"search": "x"}])).await;
    let m = recv(&mut ws).await;
    assert_eq!(m[0], "CLOSED");
    assert!(m[2].as_str().unwrap().starts_with("unsupported:"));
    send(&mut ws, json!(["REQ", "bad2", {"authors": ["NOTHEX"]}])).await;
    assert!(
        recv(&mut ws).await[2]
            .as_str()
            .unwrap()
            .starts_with("invalid:")
    );
    send(&mut ws, json!(["REQ", "x".repeat(65), {}])).await;
    assert_eq!(recv(&mut ws).await[0], "NOTICE");
    send(&mut ws, json!({"not": "an array"})).await;
    assert_eq!(recv(&mut ws).await[0], "NOTICE");
    ws.send(Message::text("garbage")).await.unwrap();
    assert_eq!(recv(&mut ws).await[0], "NOTICE");
    send(&mut ws, json!(["COUNT", "c", {}])).await;
    assert_eq!(recv(&mut ws).await[0], "NOTICE");
    send(&mut ws, json!(["EVENT", {"id": "abc"}])).await;
    assert_eq!(
        recv(&mut ws).await,
        json!(["OK", "abc", false, "invalid: bad event json"])
    );

    // NEG: filters beyond kinds/authors/since/until/ids are refused before any reconciliation.
    send(&mut ws, json!(["NEG-OPEN", "n1", {"#t": ["movie"]}, "61"])).await;
    assert_eq!(
        recv(&mut ws).await,
        json!(["NEG-ERR", "n1", "blocked: unsupported filter"])
    );
    send(&mut ws, json!(["NEG-OPEN", "n2", {"kinds": [2003]}, "zz"])).await;
    assert_eq!(
        recv(&mut ws).await,
        json!(["NEG-ERR", "n2", "invalid: bad hex"])
    );
    send(&mut ws, json!(["NEG-MSG", "n3", "61"])).await;
    assert_eq!(recv(&mut ws).await[0], "NEG-ERR");
    // The connection survives all of the above.
    send(&mut ws, json!(["REQ", "last", {"ids": [a.id]}])).await;
    assert_eq!(recv(&mut ws).await[2]["id"], a.id);
}

#[tokio::test]
async fn relay_rejects_missing_token() {
    let Some(db) = test_db().await else { return };
    let base = spawn_server(&db.store).await;
    let url = format!("{}/relay?token=wrong", base.replacen("http", "ws", 1));
    assert!(tokio_tungstenite::connect_async(url).await.is_err());
}

// ---- post-merge: need core::ingest / nip35 / sync::NegResponder ----

#[tokio::test]
#[ignore = "post-merge"]
async fn batch_ingest_fixtures() {
    let Some(db) = test_db().await else { return };
    let base = spawn_server(&db.store).await;
    let events = fixture_events("kind2003_sample.jsonl");
    let n = events.len();
    let req = BatchRequest {
        events: events.clone(),
    };
    let send = || async {
        http()
            .post(format!("{base}/api/v1/events/batch"))
            .bearer_auth(TOKEN)
            .json(&req)
            .send()
            .await
            .unwrap()
            .json::<BatchResponse>()
            .await
            .unwrap()
    };
    let first = send().await;
    assert_eq!(first.results.len(), n);
    assert!(
        first.results.iter().all(|r| r.accepted),
        "{:?}",
        first.results
    );
    let second = send().await;
    assert!(
        second
            .results
            .iter()
            .all(|r| r.accepted && r.duplicate && r.message.starts_with("duplicate:"))
    );
    // Tampered event is refused.
    let mut bad = events[0].clone();
    bad.content.push('!');
    let r = http()
        .post(format!("{base}/api/v1/events/batch"))
        .bearer_auth(TOKEN)
        .json(&BatchRequest { events: vec![bad] })
        .send()
        .await
        .unwrap()
        .json::<BatchResponse>()
        .await
        .unwrap();
    assert!(!r.results[0].accepted && r.results[0].message.starts_with("invalid:"));
}

#[tokio::test]
#[ignore = "post-merge"]
async fn relay_event_ok_prefixes() {
    let Some(db) = test_db().await else { return };
    let base = spawn_server(&db.store).await;
    let keys = nostr::prelude::Keys::generate();
    let ev = signed(
        &keys,
        2003,
        vec![
            vec!["title".into(), "Signed torrent".into()],
            vec!["x".into(), ih(42)],
        ],
        "desc",
    );
    let mut ws = ws_connect(&base).await;
    send(&mut ws, json!(["REQ", "live", {"kinds": [2003]}])).await;
    assert_eq!(recv(&mut ws).await[0], "EOSE");
    send(&mut ws, json!(["EVENT", ev])).await;
    let mut msgs = [recv(&mut ws).await, recv(&mut ws).await];
    msgs.sort_by_key(|m| m[0].as_str().unwrap().to_string());
    assert_eq!(msgs[0][0], "EVENT");
    assert_eq!(msgs[1], json!(["OK", ev.id, true, ""]));
    send(&mut ws, json!(["EVENT", ev])).await;
    let dup = recv(&mut ws).await;
    assert_eq!(dup[2], true);
    assert!(dup[3].as_str().unwrap().starts_with("duplicate:"));
    let mut bad = ev.clone();
    bad.content = "tampered".into();
    send(&mut ws, json!(["EVENT", bad])).await;
    let r = recv(&mut ws).await;
    assert_eq!(r[2], false);
    assert!(r[3].as_str().unwrap().starts_with("invalid:"));
    let note = signed(&keys, 1, vec![], "kind 1");
    send(&mut ws, json!(["EVENT", note])).await;
    assert!(
        recv(&mut ws).await[3]
            .as_str()
            .unwrap()
            .starts_with("blocked:")
    );
}

/// NIP-77 interop with nostr-sdk as the oracle: Down (server → client) then Up (client → server).
#[tokio::test]
#[ignore = "post-merge"]
async fn negentropy_interop_nostr_sdk() {
    use nostr_sdk::prelude::*;
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let base = spawn_server(s).await;
    let relay_url = format!("{}/relay?token={TOKEN}", base.replacen("http", "ws", 1));

    // Server-side events: real signatures, stored directly (no ingest needed for Down).
    let keys = Keys::generate();
    let mut server_ids = Vec::new();
    for i in 0..40u64 {
        let raw = signed(
            &keys,
            2003,
            vec![
                vec!["title".into(), format!("srv {i}")],
                vec!["x".into(), ih(1000 + i)],
            ],
            "",
        );
        let mut t = torrent(0, 0, raw.created_at, 1000 + i, &format!("srv {i}"));
        t.p.event_id = raw.id.clone();
        t.p.pubkey = raw.pubkey.clone();
        assert!(s.put_torrent(&t.p, &raw).await.unwrap());
        server_ids.push(raw.id);
    }

    let client = Client::builder()
        .database(nostr_memory::MemoryDatabase::unbounded())
        .build();
    client.add_relay(&relay_url).await.unwrap();
    client.connect().and_wait(Duration::from_secs(5)).await;
    // Client-only events.
    let mine = Keys::generate();
    for i in 0..25u64 {
        let ev = EventBuilder::new(Kind::Torrent, "")
            .tag(Tag::parse(["title".to_string(), format!("cli {i}")]).unwrap())
            .tag(Tag::parse(["x".to_string(), ih(2000 + i)]).unwrap())
            .finalize(&mine)
            .unwrap();
        client.database().save_event(&ev).await.unwrap();
    }

    let filter = Filter::new().kinds([Kind::Torrent]);
    let down = client
        .sync(filter.clone())
        .with([relay_url.as_str()])
        .opts(SyncOptions::new().direction(SyncDirection::Down))
        .await
        .unwrap();
    assert!(down.failed.is_empty(), "{:?}", down.failed);
    assert_eq!(down.value.received.len(), 40);

    let up = client
        .sync(filter.clone())
        .with([relay_url.as_str()])
        .opts(SyncOptions::new().direction(SyncDirection::Up))
        .await
        .unwrap();
    assert!(up.failed.is_empty(), "{:?}", up.failed);
    assert_eq!(up.value.sent.len(), 25);
    assert!(up.value.send_failures.is_empty());
    assert_eq!(s.stats().await.unwrap().events, 65);

    // Both sides converged: a second Both sync is a no-op.
    let again = client
        .sync(filter)
        .with([relay_url.as_str()])
        .opts(SyncOptions::new().direction(SyncDirection::Both))
        .await
        .unwrap();
    assert!(again.value.sent.is_empty() && again.value.received.is_empty());
    client.shutdown().await;
}

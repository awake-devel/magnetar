//! RelayConn against the in-process fake relay (and one live relay, ignored by default).

mod support;

use std::time::{Duration, Instant};

use magnetar_core::error::Error;
use magnetar_core::relay::{RelayConn, RelayMsg};
use nostr::prelude::Keys;
use serde_json::json;
use support::fake_relay::{Behaviour, FakeRelay};
use support::*;

const T: Duration = Duration::from_secs(5);

fn generated(n: u32) -> Vec<magnetar_core::model::RawEvent> {
    let keys = Keys::generate();
    let base = now() - 1000;
    (0..n)
        .map(|i| torrent(&keys, i, base - i64::from(i / 3)))
        .collect()
}

#[tokio::test]
async fn fetch_page_clamps_and_closes() {
    let relay = FakeRelay::start(generated(150), Behaviour::default()).await;
    let mut conn = RelayConn::connect(&relay.url, T).await.unwrap();
    assert!(conn.is_open());

    // The welcome NOTICE arrives first and is skipped by fetch_page.
    let page = conn
        .fetch_page(json!({"kinds": [2003], "limit": 500}), T)
        .await
        .unwrap();
    assert_eq!(page.events.len(), 100);
    assert!(page.closed.is_none() && !page.timed_out);
    let min = page.events.iter().map(|e| e.created_at).min().unwrap();

    // Inclusive until: the boundary second comes back.
    let page2 = conn
        .fetch_page(json!({"kinds": [2003], "limit": 500, "until": min}), T)
        .await
        .unwrap();
    assert!(page2.events.iter().all(|e| e.created_at <= min));
    assert!(page2.events.iter().any(|e| e.created_at == min));

    let none = conn.fetch_page(json!({"kinds": [2004]}), T).await.unwrap();
    assert!(none.events.is_empty());
    conn.close().await;

    tokio::time::sleep(Duration::from_millis(100)).await;
    let st = relay.state.lock().unwrap();
    assert_eq!(st.reqs.len(), 3);
    assert_eq!(st.closes, 3);
}

#[tokio::test]
async fn closed_page_keeps_connection() {
    let behaviour = Behaviour {
        closed_reqs: vec![2],
        ..Behaviour::default()
    };
    let relay = FakeRelay::start(generated(10), behaviour).await;
    let mut conn = RelayConn::connect(&relay.url, T).await.unwrap();
    let ok = conn.fetch_page(json!({"kinds": [2003]}), T).await.unwrap();
    assert_eq!(ok.events.len(), 10);
    let closed = conn.fetch_page(json!({"kinds": [2003]}), T).await.unwrap();
    assert_eq!(closed.closed.as_deref(), Some("rate-limited: slow down"));
    assert!(closed.events.is_empty());
    // The connection is still usable after a CLOSED.
    assert_eq!(
        conn.fetch_page(json!({"kinds": [2003]}), T)
            .await
            .unwrap()
            .events
            .len(),
        10
    );
    conn.close().await;
}

#[tokio::test]
async fn notices_and_recv_timeout() {
    let relay = FakeRelay::start(Vec::new(), Behaviour::default()).await;
    let mut conn = RelayConn::connect(&relay.url, T).await.unwrap();
    assert_eq!(
        conn.recv(T).await.unwrap(),
        Some(RelayMsg::Notice("welcome to the fake relay".into()))
    );
    conn.send_json(&json!(["COUNT", "c1", {"kinds": [2003]}]))
        .await
        .unwrap();
    assert!(
        matches!(conn.recv(T).await.unwrap(), Some(RelayMsg::Notice(m)) if m.contains("unknown cmd"))
    );
    // Nothing more to read: recv times out.
    let started = Instant::now();
    assert!(matches!(
        conn.recv(Duration::from_millis(300)).await,
        Err(Error::Timeout(_))
    ));
    assert!(started.elapsed() < Duration::from_secs(2));
    conn.close().await;
}

#[tokio::test]
async fn silent_relay_times_out_page() {
    let behaviour = Behaviour {
        silent: true,
        notice_on_connect: false,
        ..Behaviour::default()
    };
    let relay = FakeRelay::start(generated(5), behaviour).await;
    let mut conn = RelayConn::connect(&relay.url, T).await.unwrap();
    let started = Instant::now();
    let page = conn
        .fetch_page(json!({"kinds": [2003]}), Duration::from_millis(500))
        .await
        .unwrap();
    assert!(page.timed_out);
    assert!(started.elapsed() < Duration::from_secs(2));
    conn.close().await;
}

#[tokio::test]
async fn publish_waits_for_ok() {
    let relay = FakeRelay::start(Vec::new(), Behaviour::default()).await;
    let mut conn = RelayConn::connect(&relay.url, T).await.unwrap();
    let ev = generated(1).pop().unwrap();
    assert_eq!(conn.publish(&ev, T).await.unwrap(), (true, String::new()));
    let (accepted, msg) = conn.publish(&ev, T).await.unwrap();
    assert!(accepted && msg.starts_with("duplicate:"));
    conn.close().await;
    assert_eq!(relay.state.lock().unwrap().published.len(), 1);
}

#[tokio::test]
async fn connect_failures() {
    // Nothing listens on this port once the listener is dropped.
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let err = RelayConn::connect(&format!("ws://127.0.0.1:{port}"), T)
        .await
        .err()
        .unwrap();
    assert!(matches!(err, Error::Relay { .. }), "{err:?}");
    assert!(RelayConn::connect("not a url", T).await.is_err());
    assert!(RelayConn::connect("https://example.com", T).await.is_err());

    // A TCP server that never answers the handshake.
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", l.local_addr().unwrap());
    let _hold = tokio::spawn(async move {
        let mut socks = Vec::new();
        while let Ok((s, _)) = l.accept().await {
            socks.push(s);
        }
    });
    let started = Instant::now();
    let err = RelayConn::connect(&url, Duration::from_millis(500))
        .await
        .err()
        .unwrap();
    assert!(matches!(err, Error::Timeout(_)), "{err:?}");
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[tokio::test]
#[ignore = "network: live relay"]
async fn live_fetch_page_tls() {
    let mut conn = RelayConn::connect("wss://nos.lol", Duration::from_secs(10))
        .await
        .unwrap();
    let page = conn
        .fetch_page(
            json!({"kinds": [2003], "limit": 20}),
            Duration::from_secs(20),
        )
        .await
        .unwrap();
    println!(
        "nos.lol: {} events, closed={:?}, timed_out={}",
        page.events.len(),
        page.closed,
        page.timed_out
    );
    assert!(!page.events.is_empty());
    for ev in &page.events {
        ev.verify().unwrap();
        assert_eq!(ev.kind, 2003);
    }
    conn.close().await;
}

#[tokio::test]
#[ignore = "network: live relays"]
async fn live_comment_lookup_two_filters() {
    // Real dtan comments (fixtures) on torrents; nos.lol stores no 2004 (nostr-nips.md §2.3), the
    // two archives do.
    let comments: Vec<_> = ["09b1fb02", "3a996454", "0116b532"].map(fixture).to_vec();
    let roots: Vec<String> = comments.iter().map(|c| c.tags[0][1].clone()).collect();
    let mut found = 0;
    for url in ["wss://nostr.oxtr.dev", "wss://relay.nostr.net"] {
        let Ok(mut conn) = RelayConn::connect(url, Duration::from_secs(10)).await else {
            println!("{url}: connect failed");
            continue;
        };
        let page = conn
            .fetch_page_multi(
                vec![
                    json!({"kinds": [2004, 1111], "#e": roots, "limit": 500}),
                    json!({"kinds": [1111], "#E": roots, "limit": 500}),
                ],
                Duration::from_secs(20),
            )
            .await
            .unwrap();
        let hits = comments
            .iter()
            .filter(|c| page.events.iter().any(|e| e.id == c.id))
            .count();
        println!(
            "{url}: {} events, {hits}/3 fixture comments (closed={:?})",
            page.events.len(),
            page.closed
        );
        found += hits;
        conn.close().await;
    }
    assert!(found > 0);
}

//! Size caps on what remote servers send us (D111): HTTP tracker scrape bodies and Nostr relay
//! pages. Loopback fakes only.

use std::time::Duration;

use futures::{SinkExt, StreamExt};
use magnetar_core::liveness::http;
use magnetar_core::relay::{self, RelayConn};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;

fn h(seed: u8) -> [u8; 20] {
    std::array::from_fn(|i| seed.wrapping_add(i as u8 * 7))
}

/// HTTP tracker on loopback: answers every request with `head` then `body`, then holds the
/// connection open (a reader that waits for the end of the body never gets it).
async fn hostile_http_tracker(head: String, body: Vec<u8>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        while let Ok((mut conn, _)) = listener.accept().await {
            let (head, body) = (head.clone(), body.clone());
            tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                let _ = conn.read(&mut buf).await;
                if conn.write_all(head.as_bytes()).await.is_err() {
                    return;
                }
                let _ = conn.write_all(&body).await;
                tokio::time::sleep(Duration::from_secs(60)).await;
            });
        }
    });
    base
}

/// A tracker streaming a body past MAX_SCRAPE_BODY (no Content-Length, never ending) is cut off
/// at the cap instead of being buffered until the client timeout.
#[tokio::test]
async fn http_scrape_body_is_capped() {
    let mut body = b"d5:filesd".to_vec();
    body.resize(http::MAX_SCRAPE_BODY + 64 * 1024, b'x');
    let base = hostile_http_tracker(
        "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nConnection: close\r\n\r\n".into(),
        body,
    )
    .await;
    // The client timeout is long: only the cap can end this read quickly.
    let client = http::TrackerClient::new(
        Duration::from_secs(30),
        magnetar_core::liveness::AddrPolicy::loopback_for_tests(),
    )
    .unwrap();
    let res = tokio::time::timeout(
        Duration::from_secs(5),
        http::scrape(&client, &format!("{base}/announce"), &[h(1)]),
    )
    .await
    .expect("scrape kept reading past the cap");
    let err = res.unwrap_err().to_string();
    assert!(err.contains("larger than"), "{err}");

    // An announced Content-Length above the cap fails before the body is read.
    let base = hostile_http_tracker(
        format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            1u64 << 32
        ),
        b"d5:filesdee".to_vec(),
    )
    .await;
    let res = tokio::time::timeout(
        Duration::from_secs(5),
        http::scrape(&client, &format!("{base}/announce"), &[h(1)]),
    )
    .await
    .expect("scrape waited for an oversized body");
    let err = res.unwrap_err().to_string();
    assert!(err.contains("larger than"), "{err}");
}

/// A small answer that decodes to many values (`le` costs ~30× its size once decoded) is priced
/// with count_values and refused before the decode.
#[tokio::test]
async fn http_scrape_prices_the_decode() {
    let mut body = b"d5:filesde4:junkl".to_vec();
    body.extend(b"le".repeat(http::MAX_SCRAPE_VALUES));
    body.extend_from_slice(b"ee");
    assert!(body.len() < http::MAX_SCRAPE_BODY);
    let base = hostile_http_tracker(
        format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        ),
        body,
    )
    .await;
    let client = http::TrackerClient::new(
        Duration::from_secs(5),
        magnetar_core::liveness::AddrPolicy::loopback_for_tests(),
    )
    .unwrap();
    let err = http::scrape(&client, &format!("{base}/announce"), &[h(1)])
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("values"), "{err}");
}

/// Relay on loopback that answers the first REQ with `events` (raw event JSON objects) and never
/// sends EOSE.
async fn flooding_relay(events: Vec<Value>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        while let Ok((tcp, _)) = listener.accept().await {
            let events = events.clone();
            tokio::spawn(async move {
                let Ok(mut ws) = tokio_tungstenite::accept_async(tcp).await else {
                    return;
                };
                while let Some(Ok(msg)) = ws.next().await {
                    let Message::Text(t) = msg else { continue };
                    let Ok(Value::Array(arr)) = serde_json::from_str::<Value>(t.as_str()) else {
                        continue;
                    };
                    if arr.first().and_then(Value::as_str) != Some("REQ") {
                        continue;
                    }
                    let sub = arr[1].clone();
                    for ev in &events {
                        let text = json!(["EVENT", sub, ev]).to_string();
                        if ws.send(Message::Text(text.into())).await.is_err() {
                            return;
                        }
                    }
                }
            });
        }
    });
    url
}

fn event(i: usize, tags: Vec<Vec<String>>, content: String) -> Value {
    json!({
        "id": format!("{i:064x}"),
        "pubkey": "b".repeat(64),
        "created_at": 1_700_000_000 - i as i64,
        "kind": 2003,
        "tags": tags,
        "content": content,
        "sig": "c".repeat(128),
    })
}

/// One page stops once the events it kept pass MAX_PAGE_BYTES, whatever the event count: a
/// relay streaming large events for the whole page timeout cannot make it buffer GBs.
#[tokio::test]
async fn relay_page_has_a_byte_budget() {
    // ~200 KiB of content each (under the per-event cap), 2x the page budget in all.
    let content = "x".repeat(200 << 10);
    let n = 2 * relay::MAX_PAGE_BYTES / content.len() + 1;
    let events: Vec<Value> = (0..n).map(|i| event(i, vec![], content.clone())).collect();
    let url = flooding_relay(events).await;
    let mut conn = RelayConn::connect(&url, Duration::from_secs(5))
        .await
        .unwrap();
    let page = conn
        .fetch_page(json!({"kinds": [2003]}), Duration::from_secs(20))
        .await
        .unwrap();
    let kept: usize = page.events.iter().map(|e| e.content.len()).sum();
    assert!(!page.events.is_empty());
    assert!(
        // The page ends on the message that reaches the budget.
        page.events.len() < n && kept <= relay::MAX_PAGE_BYTES + relay::MAX_MESSAGE_BYTES,
        "kept {} events, {kept} bytes",
        page.events.len()
    );
    assert!(
        !page.timed_out,
        "the page should end on the budget, not the timeout"
    );
}

/// A message above MAX_MESSAGE_BYTES ends the connection instead of being buffered (the old
/// 16 MiB limit let one frame cost 16 MiB plus its parse).
#[tokio::test]
async fn relay_message_size_is_capped() {
    let big = "x".repeat(relay::MAX_MESSAGE_BYTES + 1024);
    let url = flooding_relay(vec![event(0, vec![], big)]).await;
    let mut conn = RelayConn::connect(&url, Duration::from_secs(5))
        .await
        .unwrap();
    let res = conn
        .fetch_page(json!({"kinds": [2003]}), Duration::from_secs(5))
        .await;
    match res {
        Err(e) => assert!(e.to_string().contains("recv"), "{e}"),
        Ok(page) => panic!("oversized message accepted: {} events", page.events.len()),
    }
    assert!(!conn.is_open());
}

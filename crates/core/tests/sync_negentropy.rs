//! NegResponder against an in-process negentropy initiator.

use std::collections::HashSet;

use magnetar_core::sync::{FRAME_SIZE_LIMIT, NegResponder};
use negentropy::{Id, Negentropy, NegentropyStorageVector};

/// Deterministic distinct ids: sha256(i).
fn item(i: u32) -> (u64, [u8; 32]) {
    use sha2::Digest;
    let id: [u8; 32] = sha2::Sha256::digest(i.to_be_bytes()).into();
    (1_700_000_000 + (i as u64 % 50_000) * 7, id)
}

fn initiator(items: &[(u64, [u8; 32])]) -> Negentropy<'static, NegentropyStorageVector> {
    let mut s = NegentropyStorageVector::with_capacity(items.len());
    for (ts, id) in items {
        s.insert(*ts, Id::from_byte_array(*id)).unwrap();
    }
    s.seal().unwrap();
    Negentropy::owned(s, FRAME_SIZE_LIMIT).unwrap()
}

#[test]
fn reconciles_20k_vs_25k_exactly() {
    // client: 0..20 000; server: 5 000..30 000 → client-only 5 000, server-only 10 000.
    let client: Vec<_> = (0..20_000).map(item).collect();
    let server: Vec<_> = (5_000..30_000).map(item).collect();
    let mut resp = NegResponder::new(server.clone()).unwrap();
    let mut neg = initiator(&client);

    let mut msg = hex::encode(neg.initiate().unwrap());
    let (mut have, mut need) = (Vec::new(), Vec::new());
    let mut rounds = 0;
    loop {
        let reply = resp.reconcile_hex(&msg).unwrap();
        assert!(reply.len() / 2 <= FRAME_SIZE_LIMIT as usize, "frame {}", reply.len() / 2);
        rounds += 1;
        assert!(rounds < 50, "no convergence");
        match neg
            .reconcile_with_ids(&hex::decode(reply).unwrap(), &mut have, &mut need)
            .unwrap()
        {
            Some(next) => {
                assert!(next.len() <= FRAME_SIZE_LIMIT as usize);
                msg = hex::encode(next);
            }
            None => break,
        }
    }
    let have: HashSet<[u8; 32]> = have.iter().map(|i| i.to_bytes()).collect();
    let need: HashSet<[u8; 32]> = need.iter().map(|i| i.to_bytes()).collect();
    let want_have: HashSet<[u8; 32]> = (0..5_000).map(|i| item(i).1).collect();
    let want_need: HashSet<[u8; 32]> = (20_000..30_000).map(|i| item(i).1).collect();
    assert_eq!(have.len(), 5_000);
    assert_eq!(need.len(), 10_000);
    assert_eq!(have, want_have);
    assert_eq!(need, want_need);
    assert!(rounds > 1, "expected a multi-round sync, got {rounds}");
}

#[test]
fn identical_sets_finish_in_one_round() {
    let items: Vec<_> = (0..3_000).map(item).collect();
    let mut resp = NegResponder::new(items.clone()).unwrap();
    let mut neg = initiator(&items);
    let reply = resp
        .reconcile_hex(&hex::encode(neg.initiate().unwrap()))
        .unwrap();
    let (mut have, mut need) = (Vec::new(), Vec::new());
    let next = neg
        .reconcile_with_ids(&hex::decode(reply).unwrap(), &mut have, &mut need)
        .unwrap();
    assert!(next.is_none());
    assert!(have.is_empty() && need.is_empty());
}

#[test]
fn probe_and_bad_input() {
    // Empty-set initiate message (negentropy.md §2): the responder answers with its own
    // (empty) IdList, which the probe uses to detect NIP-77 support.
    let mut resp = NegResponder::new(vec![]).unwrap();
    assert_eq!(resp.reconcile_hex("6100000200").unwrap(), "6100000200");
    // Unknown in-range version → reply with our version byte only.
    let mut resp = NegResponder::new(vec![item(1)]).unwrap();
    assert_eq!(resp.reconcile_hex("62").unwrap(), "61");
    let mut resp = NegResponder::new(vec![item(1)]).unwrap();
    assert!(resp.reconcile_hex("zz").is_err());
    let mut resp = NegResponder::new(vec![item(1)]).unwrap();
    assert!(resp.reconcile_hex("610").is_err());
    let mut resp = NegResponder::new(vec![item(1)]).unwrap();
    assert!(resp.reconcile_hex("01").is_err());
    // Duplicate items are deduped by seal().
    let mut resp = NegResponder::new(vec![item(1), item(1)]).unwrap();
    assert!(resp.reconcile_hex("6100000200").is_ok());
}

/// Live: full initiator run (empty local set) against a NIP-77 relay; counts its kind-2003 ids.
#[tokio::test]
#[ignore = "network"]
async fn live_negentropy_oxtr() {
    use futures::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;

    let connector =
        tokio_tungstenite::Connector::Rustls(std::sync::Arc::new(magnetar_core::http::tls_config()));
    let cfg = tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default()
        .max_message_size(Some(16 << 20))
        .max_frame_size(Some(16 << 20));
    let (mut ws, _) = tokio_tungstenite::connect_async_tls_with_config(
        "wss://nostr.oxtr.dev",
        Some(cfg),
        true,
        Some(connector),
    )
    .await
    .expect("connect");
    let mut neg = initiator(&[]);
    let init = hex::encode(neg.initiate().unwrap());
    let open = serde_json::json!(["NEG-OPEN", "live", {"kinds": [2003]}, init]);
    ws.send(Message::text(open.to_string())).await.unwrap();
    let (mut have, mut need) = (Vec::new(), Vec::new());
    let mut rounds = 0;
    let mut max_frame = 0;
    loop {
        let msg = tokio::time::timeout(std::time::Duration::from_secs(20), ws.next())
            .await
            .expect("relay answered")
            .unwrap()
            .unwrap();
        let Message::Text(t) = msg else { continue };
        let v: serde_json::Value = serde_json::from_str(t.as_str()).unwrap();
        assert_eq!(v[0], "NEG-MSG", "{v}");
        let bytes = hex::decode(v[2].as_str().unwrap()).unwrap();
        max_frame = max_frame.max(bytes.len());
        rounds += 1;
        match neg.reconcile_with_ids(&bytes, &mut have, &mut need).unwrap() {
            Some(next) => {
                let m = serde_json::json!(["NEG-MSG", "live", hex::encode(next)]);
                ws.send(Message::text(m.to_string())).await.unwrap();
            }
            None => break,
        }
    }
    let close = serde_json::json!(["NEG-CLOSE", "live"]);
    ws.send(Message::text(close.to_string())).await.unwrap();
    println!(
        "live oxtr: {rounds} rounds, max relay frame {max_frame} B, need {} kind-2003 ids",
        need.len()
    );
    assert!(have.is_empty());
    assert!(need.len() > 10_000);
}

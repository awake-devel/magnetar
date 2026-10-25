//! sync_up against an in-process fake relay (NIP-77 via NegResponder) and a fake batch endpoint.

mod api_support;

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use futures::{SinkExt, StreamExt};
use magnetar_core::api::{ApiClient, BatchRequest, BatchResponse, BatchResult};
use magnetar_core::model::RawEvent;
use magnetar_core::sync::{NegResponder, SyncSource, sync_up};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;

fn mk_event(i: u32) -> RawEvent {
    use sha2::Digest;
    let id: [u8; 32] = sha2::Sha256::digest(format!("ev{i}")).into();
    RawEvent {
        id: hex::encode(id),
        pubkey: "11".repeat(32),
        created_at: 1_790_000_000 + i as i64,
        kind: 2003,
        tags: vec![vec!["x".into(), "ab".repeat(20)]],
        content: format!("torrent {i}"),
        sig: "00".repeat(64),
    }
}

fn neg_item(e: &RawEvent) -> (u64, [u8; 32]) {
    (e.created_at as u64, e.id_bytes().unwrap())
}

#[derive(Default)]
struct MemSource {
    events: Vec<RawEvent>,
    /// Ids in sync_ids whose body was evicted (still part of the negentropy set).
    evicted: Vec<(u64, [u8; 32])>,
    synced: Mutex<HashSet<String>>,
    failures: Mutex<HashMap<String, String>>,
}

impl MemSource {
    fn new(events: Vec<RawEvent>) -> Self {
        Self {
            events,
            ..Default::default()
        }
    }
    fn synced(&self) -> HashSet<String> {
        self.synced.lock().unwrap().clone()
    }
    fn failures(&self) -> HashMap<String, String> {
        self.failures.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl SyncSource for MemSource {
    async fn neg_items(&self) -> magnetar_core::Result<Vec<(u64, [u8; 32])>> {
        Ok(self
            .events
            .iter()
            .map(neg_item)
            .chain(self.evicted.iter().copied())
            .collect())
    }
    async fn load_events(&self, ids: &[String]) -> magnetar_core::Result<Vec<RawEvent>> {
        Ok(self
            .events
            .iter()
            .filter(|e| ids.contains(&e.id))
            .cloned()
            .collect())
    }
    async fn unsynced(&self, limit: usize) -> magnetar_core::Result<Vec<RawEvent>> {
        let synced = self.synced.lock().unwrap();
        Ok(self
            .events
            .iter()
            .filter(|e| !synced.contains(&e.id))
            .take(limit)
            .cloned()
            .collect())
    }
    async fn mark_synced(&self, ids: &[String]) -> magnetar_core::Result<()> {
        self.synced.lock().unwrap().extend(ids.iter().cloned());
        Ok(())
    }
    async fn record_failure(&self, id: &str, reason: &str) -> magnetar_core::Result<()> {
        self.failures
            .lock()
            .unwrap()
            .insert(id.into(), reason.into());
        Ok(())
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Negentropy,
    NegErr,
    Notice,
    Silent,
}

struct FakeRelay {
    url: String,
    /// Every EVENT id received, in order.
    received: Arc<Mutex<Vec<String>>>,
    /// Raw query strings of the WS upgrade requests.
    queries: Arc<Mutex<Vec<String>>>,
}

/// NIP-77 relay holding `server` events; EVENT for an id in `reject` gets OK false, in `dup_msg`
/// gets OK true "duplicate:", for an id the relay already holds also "duplicate:".
async fn fake_relay(
    mode: Mode,
    server: Vec<RawEvent>,
    reject: HashSet<String>,
    dup_msg: HashSet<String>,
) -> FakeRelay {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/relay?token=s3cr%2Bt", listener.local_addr().unwrap());
    let received = Arc::new(Mutex::new(Vec::new()));
    let queries = Arc::new(Mutex::new(Vec::new()));
    // id -> negentropy item of every event the relay holds (grows with accepted EVENTs).
    let store: Arc<Mutex<HashMap<String, (u64, [u8; 32])>>> = Arc::new(Mutex::new(
        server.iter().map(|e| (e.id.clone(), neg_item(e))).collect(),
    ));
    let (rx_log, q_log) = (received.clone(), queries.clone());
    tokio::spawn(async move {
        while let Ok((sock, _)) = listener.accept().await {
            let (store, reject, dup_msg) = (store.clone(), reject.clone(), dup_msg.clone());
            let (rx_log, q_log) = (rx_log.clone(), q_log.clone());
            tokio::spawn(async move {
                let cb = |req: &tokio_tungstenite::tungstenite::handshake::server::Request,
                          resp| {
                    q_log
                        .lock()
                        .unwrap()
                        .push(req.uri().query().unwrap_or_default().to_string());
                    Ok(resp)
                };
                let mut ws = tokio_tungstenite::accept_hdr_async(sock, cb).await.unwrap();
                let mut neg: Option<NegResponder> = None;
                while let Some(Ok(msg)) = ws.next().await {
                    let Message::Text(t) = msg else { continue };
                    let v: Value = serde_json::from_str(t.as_str()).unwrap();
                    let out: Vec<Value> = match v[0].as_str().unwrap() {
                        "NEG-OPEN" => {
                            assert_eq!(v[2], json!({"kinds": [2003, 2004, 1111]}));
                            match mode {
                                Mode::Negentropy => {
                                    let items = store.lock().unwrap().values().copied().collect();
                                    let mut r = NegResponder::new(items).unwrap();
                                    let reply = r.reconcile_hex(v[3].as_str().unwrap()).unwrap();
                                    neg = Some(r);
                                    vec![json!(["NEG-MSG", v[1], reply])]
                                }
                                Mode::NegErr => {
                                    vec![json!(["NEG-ERR", v[1], "blocked: this query is too big"])]
                                }
                                Mode::Notice => {
                                    vec![json!(["NOTICE", "ERROR: bad msg: negentropy disabled"])]
                                }
                                Mode::Silent => vec![],
                            }
                        }
                        "NEG-MSG" => {
                            let r = neg.as_mut().unwrap();
                            vec![json!(["NEG-MSG", v[1], r.reconcile_hex(v[2].as_str().unwrap()).unwrap()])]
                        }
                        "NEG-CLOSE" => {
                            neg = None;
                            vec![]
                        }
                        "EVENT" => {
                            let ev: RawEvent = serde_json::from_value(v[1].clone()).unwrap();
                            rx_log.lock().unwrap().push(ev.id.clone());
                            let mut st = store.lock().unwrap();
                            let ok = if reject.contains(&ev.id) {
                                json!(["OK", ev.id, false, "invalid: rejected by test"])
                            } else if dup_msg.contains(&ev.id) || st.contains_key(&ev.id) {
                                st.insert(ev.id.clone(), neg_item(&ev));
                                json!(["OK", ev.id, true, "duplicate: already have this event"])
                            } else {
                                st.insert(ev.id.clone(), neg_item(&ev));
                                json!(["OK", ev.id, true, ""])
                            };
                            vec![ok]
                        }
                        other => panic!("unexpected {other}"),
                    };
                    for m in out {
                        if ws.send(Message::text(m.to_string())).await.is_err() {
                            return;
                        }
                    }
                }
            });
        }
    });
    FakeRelay {
        url,
        received,
        queries,
    }
}

/// Fake `/api/v1/events/batch`: accepts all but `reject`, `dup` flagged duplicate.
async fn fake_batch(reject: HashSet<String>, dup: HashSet<String>) -> api_support::FakeHttp {
    api_support::serve(move |req| {
        assert_eq!(req.method, "POST");
        assert_eq!(req.path, "/api/v1/events/batch");
        assert_eq!(req.header("authorization"), Some("Bearer tok"));
        let body: BatchRequest = serde_json::from_slice(&req.body).unwrap();
        assert!(body.events.len() <= 500);
        let results = body
            .events
            .iter()
            .map(|e| BatchResult {
                id: e.id.clone(),
                accepted: !reject.contains(&e.id),
                duplicate: dup.contains(&e.id),
                message: if reject.contains(&e.id) {
                    "invalid: bad signature".into()
                } else {
                    String::new()
                },
            })
            .collect();
        (200, serde_json::to_string(&BatchResponse { results }).unwrap())
    })
    .await
}

fn ids(evs: &[RawEvent]) -> HashSet<String> {
    evs.iter().map(|e| e.id.clone()).collect()
}

#[tokio::test]
async fn negentropy_upload_marks_only_ok_true() {
    let all: Vec<RawEvent> = (0..80).map(mk_event).collect();
    // Phone: 0..60 (all unsynced) + evicted body of 70. Server: 40..80.
    let mut src = MemSource::new(all[..60].to_vec());
    src.evicted.push(neg_item(&all[70]));
    let server = all[40..].to_vec();
    let reject: HashSet<String> = ids(&all[0..3]);
    let dup: HashSet<String> = ids(&all[3..5]);
    let relay = fake_relay(Mode::Negentropy, server, reject.clone(), dup).await;
    // Batch endpoint must not be touched on the negentropy path.
    let http = api_support::serve(|_| (500, "{}".into())).await;
    let api = ApiClient::new(&http.base, Some("tok".into())).unwrap();

    let r = sync_up(&src, &relay.url, &api).await.unwrap();
    assert_eq!(r.method, "negentropy");
    assert_eq!(r.error, None);
    assert_eq!(r.have, 40, "phone-only ids 0..40");
    assert_eq!(r.need_ignored, 19, "server-only 60..80 minus the evicted 70");
    assert_eq!(r.failed, 3);
    // 40 have − 3 rejected − 2 duplicate-on-OK = 35 accepted;
    // 40..60 were unsynced but already on the server → re-sent, OK duplicate.
    assert_eq!(r.uploaded, 35);
    assert_eq!(r.duplicates, 2 + 20);

    let synced = src.synced();
    let want: HashSet<String> = ids(&all[3..60]);
    assert_eq!(synced, want);
    assert!(synced.is_disjoint(&reject));
    let failures = src.failures();
    assert_eq!(failures.keys().cloned().collect::<HashSet<_>>(), reject);
    assert!(failures.values().all(|m| m == "invalid: rejected by test"));
    assert!(http.requests().is_empty());
    assert_eq!(relay.queries.lock().unwrap().as_slice(), ["token=s3cr%2Bt"]);
    let received = relay.received.lock().unwrap().clone();
    assert_eq!(received.len(), 60);
    assert!(!received.contains(&all[70].id), "evicted body is never sent");

    // Second run: only the 3 rejected rows are still unsynced and nothing else differs.
    let r = sync_up(&src, &relay.url, &api).await.unwrap();
    assert_eq!(
        (r.method.as_str(), r.have, r.uploaded, r.duplicates, r.failed),
        ("negentropy", 3, 0, 0, 3)
    );
    assert_eq!(src.synced().len(), 57);
}

#[tokio::test]
async fn neg_err_falls_back_to_batch() {
    let all: Vec<RawEvent> = (0..1_200).map(mk_event).collect();
    let src = MemSource::new(all.clone());
    let relay = fake_relay(Mode::NegErr, vec![], HashSet::new(), HashSet::new()).await;
    let reject = ids(&all[10..12]);
    let dup = ids(&all[20..30]);
    let http = fake_batch(reject.clone(), dup).await;
    let api = ApiClient::new(&http.base, Some("tok".into())).unwrap();

    let r = sync_up(&src, &relay.url, &api).await.unwrap();
    assert_eq!(r.method, "batch");
    assert!(r.error.as_deref().unwrap().contains("NEG-ERR: blocked"), "{r:?}");
    assert_eq!((r.have, r.uploaded, r.duplicates, r.failed), (1_200, 1_188, 10, 2));
    let posts = http.requests();
    assert_eq!(posts.len(), 3, "1 200 events in chunks of ≤ 500");
    let synced = src.synced();
    assert_eq!(synced.len(), 1_198);
    assert!(synced.is_disjoint(&reject));
    assert_eq!(
        src.failures().keys().cloned().collect::<HashSet<_>>(),
        reject
    );
    assert!(relay.received.lock().unwrap().is_empty());
}

#[tokio::test]
async fn notice_negentropy_disabled_falls_back_to_batch() {
    let all: Vec<RawEvent> = (0..30).map(mk_event).collect();
    let src = MemSource::new(all.clone());
    let relay = fake_relay(Mode::Notice, vec![], HashSet::new(), HashSet::new()).await;
    let http = fake_batch(HashSet::new(), ids(&all[..5])).await;
    let api = ApiClient::new(&http.base, Some("tok".into())).unwrap();
    let r = sync_up(&src, &relay.url, &api).await.unwrap();
    assert_eq!(r.method, "batch");
    assert_eq!(
        r.error.as_deref(),
        Some("NOTICE: ERROR: bad msg: negentropy disabled")
    );
    assert_eq!((r.uploaded, r.duplicates, r.failed), (25, 5, 0));
    assert_eq!(src.synced(), ids(&all));
}

#[tokio::test]
async fn silent_relay_times_out_then_batch() {
    let all: Vec<RawEvent> = (0..3).map(mk_event).collect();
    let src = MemSource::new(all.clone());
    let relay = fake_relay(Mode::Silent, vec![], HashSet::new(), HashSet::new()).await;
    let http = fake_batch(HashSet::new(), HashSet::new()).await;
    let api = ApiClient::new(&http.base, Some("tok".into())).unwrap();
    let t = std::time::Instant::now();
    let r = sync_up(&src, &relay.url, &api).await.unwrap();
    assert!(t.elapsed() >= std::time::Duration::from_secs(10));
    assert_eq!(r.method, "batch");
    assert_eq!(r.error.as_deref(), Some("no NEG-MSG within 10 s"));
    assert_eq!(r.uploaded, 3);
}

#[tokio::test]
async fn unreachable_relay_falls_back_and_batch_errors_surface() {
    let src = MemSource::new((0..2).map(mk_event).collect());
    let err = api_support::serve(|_| {
        (
            401,
            r#"{"error":"unauthorized","message":"bad token"}"#.into(),
        )
    })
    .await;
    let api = ApiClient::new(&err.base, Some("tok".into())).unwrap();
    // Nothing listens on port 9 → connect fails → batch → 401.
    match sync_up(&src, "ws://127.0.0.1:9/relay", &api).await {
        Err(magnetar_core::Error::Server { status, msg }) => {
            assert_eq!((status, msg.as_str()), (401, "bad token"));
        }
        other => panic!("{other:?}"),
    }
    assert!(src.synced().is_empty());
}

#[tokio::test]
async fn nothing_to_sync() {
    let src = MemSource::default();
    let api = ApiClient::new("http://127.0.0.1:9", None).unwrap();
    let r = sync_up(&src, "ws://127.0.0.1:9/relay", &api).await.unwrap();
    assert_eq!(r.method, "none");
}

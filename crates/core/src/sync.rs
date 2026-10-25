//! Phone → server sync (D13–D15, ARCHITECTURE.md §7). Owner: trust-sync agent.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use negentropy::{Id, Negentropy, NegentropyStorageVector};
use serde_json::{Value, json};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::{Connector, MaybeTlsStream, WebSocketStream};

use crate::api::{ApiClient, MAX_BATCH};
use crate::error::Error;
use crate::model::RawEvent;

pub const FRAME_SIZE_LIMIT: u64 = 60_000;
pub const SYNC_KINDS: &[u16] = &[2003, 2004, 1111];
/// Max EVENTs awaiting their OK.
pub const UPLOAD_WINDOW: usize = 50;
/// No NEG-MSG within this → the relay does not do NIP-77 (D15).
pub const NEG_TIMEOUT: Duration = Duration::from_secs(10);
/// Later reconciliation rounds and OKs may take longer on a busy server.
const ROUND_TIMEOUT: Duration = Duration::from_secs(30);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_MESSAGE_BYTES: usize = 16 << 20;
/// Unsynced rows the server already holds never show up in `have`; re-send up to this many per
/// run so their `OK duplicate:` marks them synced.
const RESEND_UNSYNCED: usize = 500;
const SUB_ID: &str = "magnetar-up";

#[async_trait::async_trait]
pub trait SyncSource: Send + Sync {
    /// (created_at, id) for every event ever stored locally (sync_ids; includes evicted bodies).
    async fn neg_items(&self) -> crate::Result<Vec<(u64, [u8; 32])>>;
    async fn load_events(&self, ids: &[String]) -> crate::Result<Vec<RawEvent>>;
    async fn unsynced(&self, limit: usize) -> crate::Result<Vec<RawEvent>>;
    async fn mark_synced(&self, ids: &[String]) -> crate::Result<()>;
    async fn record_failure(&self, id: &str, reason: &str) -> crate::Result<()>;
}

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct SyncReport {
    /// "negentropy" | "batch" | "none"
    pub method: String,
    pub have: u64,
    pub uploaded: u64,
    pub duplicates: u64,
    pub failed: u64,
    pub need_ignored: u64,
    pub error: Option<String>,
}

impl SyncReport {
    fn ok(&mut self, msg: &str) {
        if msg.starts_with("duplicate:") {
            self.duplicates += 1;
        } else {
            self.uploaded += 1;
        }
    }
}

/// Negentropy Up-sync over `relay_ws_url` (e.g. ws://127.0.0.1:8787/relay?token=..), falling back to
/// `api.post_batch` with `unsynced()`. Marks synced only on OK true (incl. duplicate:).
pub async fn sync_up<S: SyncSource + ?Sized>(
    src: &S,
    relay_ws_url: &str,
    api: &ApiClient,
) -> crate::Result<SyncReport> {
    let items = src.neg_items().await?;
    let mut report = SyncReport::default();
    if items.is_empty() && src.unsynced(1).await?.is_empty() {
        report.method = "none".into();
        return Ok(report);
    }
    match reconcile(relay_ws_url, items).await {
        Ok((mut ws, have, need)) => {
            report.method = "negentropy".into();
            report.have = have.len() as u64;
            report.need_ignored = need;
            let mut queue: Vec<String> = have;
            let mut queued: HashSet<String> = queue.iter().cloned().collect();
            for ev in src.unsynced(RESEND_UNSYNCED).await? {
                if queued.insert(ev.id.clone()) {
                    queue.push(ev.id);
                }
            }
            if let Err(e) = upload(&mut ws, src, queue, &mut report).await {
                report.error = Some(e.to_string());
            }
            ws.close().await;
        }
        Err(reason) => {
            tracing::info!(%reason, "negentropy sync unavailable, using batch upload");
            report.method = "batch".into();
            report.error = Some(reason);
            batch_upload(src, api, &mut report).await?;
        }
    }
    Ok(report)
}

/// Run the initiator side to completion. Ok = (open socket, have ids, need count);
/// Err = why negentropy is unusable here (caller falls back to batch).
async fn reconcile(
    url: &str,
    items: Vec<(u64, [u8; 32])>,
) -> Result<(Ws, Vec<String>, u64), String> {
    let mut storage = NegentropyStorageVector::with_capacity(items.len());
    for (ts, id) in items {
        storage
            .insert(ts, Id::from_byte_array(id))
            .map_err(|e| format!("negentropy: {e}"))?;
    }
    storage.seal().map_err(|e| format!("negentropy: {e}"))?;
    let mut neg =
        Negentropy::owned(storage, FRAME_SIZE_LIMIT).map_err(|e| format!("negentropy: {e}"))?;
    let init = neg.initiate().map_err(|e| format!("negentropy: {e}"))?;

    let mut ws = Ws::connect(url).await.map_err(|e| e.to_string())?;
    ws.send(json!(["NEG-OPEN", SUB_ID, {"kinds": SYNC_KINDS}, hex::encode(init)]))
        .await
        .map_err(|e| e.to_string())?;

    let mut have = Vec::new();
    let mut need = 0u64;
    let mut timeout = NEG_TIMEOUT;
    loop {
        let msg = match ws.recv(timeout).await {
            Ok(Some(m)) => m,
            Ok(None) => return Err("relay closed the connection during negentropy".into()),
            Err(Error::Timeout(_)) => {
                return Err(format!("no NEG-MSG within {} s", timeout.as_secs()));
            }
            Err(e) => return Err(e.to_string()),
        };
        match msg {
            RelayMsg::NegMsg { sub, hex: msg } if sub == SUB_ID => {
                let bytes = hex::decode(&msg).map_err(|e| format!("bad NEG-MSG hex: {e}"))?;
                let (mut h, mut n) = (Vec::new(), Vec::new());
                let next = neg
                    .reconcile_with_ids(&bytes, &mut h, &mut n)
                    .map_err(|e| format!("negentropy: {e}"))?;
                have.extend(h.iter().map(|id| hex::encode(id.as_bytes())));
                need += n.len() as u64;
                match next {
                    Some(next) => ws
                        .send(json!(["NEG-MSG", SUB_ID, hex::encode(next)]))
                        .await
                        .map_err(|e| e.to_string())?,
                    None => {
                        ws.send(json!(["NEG-CLOSE", SUB_ID]))
                            .await
                            .map_err(|e| e.to_string())?;
                        return Ok((ws, have, need));
                    }
                }
                timeout = ROUND_TIMEOUT;
            }
            RelayMsg::NegErr { sub, msg } if sub == SUB_ID => {
                return Err(format!("NEG-ERR: {msg}"));
            }
            RelayMsg::Notice(msg) => return Err(format!("NOTICE: {msg}")),
            _ => {}
        }
    }
}

/// Publish `ids` with at most `UPLOAD_WINDOW` in flight; mark synced per OK true.
async fn upload<S: SyncSource + ?Sized>(
    ws: &mut Ws,
    src: &S,
    ids: Vec<String>,
    report: &mut SyncReport,
) -> crate::Result<()> {
    let mut queue: VecDeque<String> = ids.into();
    let mut in_flight: HashSet<String> = HashSet::new();
    let mut to_mark: Vec<String> = Vec::new();
    let result = async {
        loop {
            while in_flight.len() < UPLOAD_WINDOW && !queue.is_empty() {
                let n = (UPLOAD_WINDOW - in_flight.len()).min(queue.len());
                let ids: Vec<String> = queue.drain(..n).collect();
                let events = src.load_events(&ids).await?;
                if events.len() < ids.len() {
                    let got: HashSet<&str> = events.iter().map(|e| e.id.as_str()).collect();
                    for id in ids.iter().filter(|id| !got.contains(id.as_str())) {
                        tracing::warn!(%id, "sync: no local body for event, skipped");
                    }
                }
                for ev in events {
                    if in_flight.insert(ev.id.clone()) {
                        ws.send(json!(["EVENT", ev])).await?;
                    }
                }
            }
            if in_flight.is_empty() {
                return Ok(());
            }
            match ws.recv(ROUND_TIMEOUT).await? {
                None => return Err(Error::Network("relay closed the connection".into())),
                Some(RelayMsg::Ok { id, accepted, msg }) if in_flight.remove(&id) => {
                    if accepted {
                        report.ok(&msg);
                        to_mark.push(id);
                        if to_mark.len() >= UPLOAD_WINDOW {
                            src.mark_synced(&std::mem::take(&mut to_mark)).await?;
                        }
                    } else {
                        report.failed += 1;
                        src.record_failure(&id, &msg).await?;
                    }
                }
                Some(_) => {}
            }
        }
    }
    .await;
    if !to_mark.is_empty() {
        src.mark_synced(&to_mark).await?;
    }
    result
}

/// D15 fallback: POST unsynced events in chunks of ≤ 500, mark synced per accepted result.
async fn batch_upload<S: SyncSource + ?Sized>(
    src: &S,
    api: &ApiClient,
    report: &mut SyncReport,
) -> crate::Result<()> {
    // Rows that stay unsynced after a post (rejected / no result); skipped on later rounds.
    let mut stuck: HashSet<String> = HashSet::new();
    let mut accepted_ids: HashSet<String> = HashSet::new();
    loop {
        let fresh: Vec<RawEvent> = src
            .unsynced(MAX_BATCH + stuck.len())
            .await?
            .into_iter()
            .filter(|e| !stuck.contains(&e.id))
            .take(MAX_BATCH)
            .collect();
        if fresh.is_empty() {
            return Ok(());
        }
        if fresh.iter().any(|e| accepted_ids.contains(&e.id)) {
            return Err(Error::Store("mark_synced did not take effect".into()));
        }
        report.have += fresh.len() as u64;
        let results: HashMap<String, crate::api::BatchResult> = api
            .post_batch(&fresh)
            .await?
            .results
            .into_iter()
            .map(|r| (r.id.clone(), r))
            .collect();
        let mut synced = Vec::new();
        for ev in fresh {
            match results.get(&ev.id) {
                Some(r) if r.accepted => {
                    if r.duplicate {
                        report.duplicates += 1;
                    } else {
                        report.uploaded += 1;
                    }
                    accepted_ids.insert(ev.id.clone());
                    synced.push(ev.id);
                }
                Some(r) => {
                    report.failed += 1;
                    src.record_failure(&ev.id, &r.message).await?;
                    stuck.insert(ev.id);
                }
                None => {
                    report.failed += 1;
                    src.record_failure(&ev.id, "no result from server").await?;
                    stuck.insert(ev.id);
                }
            }
        }
        if !synced.is_empty() {
            src.mark_synced(&synced).await?;
        }
    }
}

/// Server side: answer one NEG-OPEN/NEG-MSG. Used by the server relay endpoint.
pub struct NegResponder {
    inner: negentropy::Negentropy<'static, negentropy::NegentropyStorageVector>,
}

impl NegResponder {
    /// Build from (created_at, id) items; errors on bad frame limit.
    pub fn new(items: Vec<(u64, [u8; 32])>) -> crate::Result<Self> {
        let neg_err = |e: negentropy::Error| Error::Invalid(format!("negentropy: {e}"));
        let mut storage = NegentropyStorageVector::with_capacity(items.len());
        for (ts, id) in items {
            storage
                .insert(ts, Id::from_byte_array(id))
                .map_err(neg_err)?;
        }
        storage.seal().map_err(neg_err)?;
        Ok(Self {
            inner: Negentropy::owned(storage, FRAME_SIZE_LIMIT).map_err(neg_err)?,
        })
    }
    /// Hex in, hex out.
    pub fn reconcile_hex(&mut self, msg_hex: &str) -> crate::Result<String> {
        let msg = hex::decode(msg_hex).map_err(|e| Error::Invalid(format!("bad hex: {e}")))?;
        let reply = self
            .inner
            .reconcile(&msg)
            .map_err(|e| Error::Invalid(format!("negentropy: {e}")))?;
        Ok(hex::encode(reply))
    }
}

/// The relay messages the sync client cares about.
#[derive(Debug)]
enum RelayMsg {
    NegMsg {
        sub: String,
        hex: String,
    },
    NegErr {
        sub: String,
        msg: String,
    },
    Notice(String),
    Ok {
        id: String,
        accepted: bool,
        msg: String,
    },
    Other,
}

impl RelayMsg {
    fn parse(text: &str) -> Self {
        let Ok(Value::Array(a)) = serde_json::from_str::<Value>(text) else {
            return RelayMsg::Other;
        };
        let s = |i: usize| {
            a.get(i)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        };
        match a.first().and_then(Value::as_str) {
            Some("NEG-MSG") => RelayMsg::NegMsg {
                sub: s(1),
                hex: s(2),
            },
            Some("NEG-ERR") => RelayMsg::NegErr {
                sub: s(1),
                msg: s(2),
            },
            Some("NOTICE") => RelayMsg::Notice(s(1)),
            Some("OK") => RelayMsg::Ok {
                id: s(1),
                accepted: a.get(2).and_then(Value::as_bool).unwrap_or(false),
                msg: s(3),
            },
            _ => RelayMsg::Other,
        }
    }
}

/// Minimal private WebSocket client (the shared `relay::RelayConn` is owned elsewhere).
struct Ws {
    inner: WebSocketStream<MaybeTlsStream<TcpStream>>,
}

impl Ws {
    async fn connect(url: &str) -> crate::Result<Self> {
        let cfg = WebSocketConfig::default()
            .max_message_size(Some(MAX_MESSAGE_BYTES))
            .max_frame_size(Some(MAX_MESSAGE_BYTES));
        let connector = if url.starts_with("wss://") {
            Connector::Rustls(Arc::new(crate::http::tls_config()))
        } else {
            Connector::Plain
        };
        let fut =
            tokio_tungstenite::connect_async_tls_with_config(url, Some(cfg), true, Some(connector));
        let (inner, _) = tokio::time::timeout(CONNECT_TIMEOUT, fut)
            .await
            .map_err(|_| Error::Timeout(format!("connect {}", redact(url))))?
            .map_err(|e| Error::Network(format!("connect {}: {e}", redact(url))))?;
        Ok(Self { inner })
    }

    async fn send(&mut self, v: Value) -> crate::Result<()> {
        self.inner
            .send(Message::text(v.to_string()))
            .await
            .map_err(|e| Error::Network(format!("ws send: {e}")))
    }

    /// Next relay message; None when the socket closed; Err(Timeout) when nothing came in time.
    async fn recv(&mut self, timeout: Duration) -> crate::Result<Option<RelayMsg>> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let frame = tokio::time::timeout_at(deadline, self.inner.next())
                .await
                .map_err(|_| Error::Timeout("relay did not answer".into()))?;
            match frame {
                None | Some(Ok(Message::Close(_))) => return Ok(None),
                Some(Ok(Message::Text(t))) => return Ok(Some(RelayMsg::parse(t.as_str()))),
                Some(Ok(_)) => {}
                Some(Err(e)) => return Err(Error::Network(format!("ws recv: {e}"))),
            }
        }
    }

    async fn close(mut self) {
        let _ = self.inner.close(None).await;
    }
}

/// URL without its query string (never log the API token).
fn redact(url: &str) -> &str {
    url.split('?').next().unwrap_or(url)
}

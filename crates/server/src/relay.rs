//! `/relay`: a small NIP-01 + NIP-77 relay over our store (ARCHITECTURE.md §6, negentropy.md §5).
//!
//! REQ is answered from SQL, then the subscription stays open and receives newly stored events
//! from `PgStore::subscribe`. EVENT goes through `core::ingest` (D5). NEG-* sessions are served
//! by `core::sync::NegResponder` over (created_at, id) items selected in SQL.

use std::collections::HashMap;

use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::Response;
use futures::{SinkExt, StreamExt};
use magnetar_core::model::RawEvent;
use magnetar_core::sync::NegResponder;
use serde_json::{Value, json};
use tokio::sync::broadcast::error::RecvError;

use crate::api::{AppState, ingest_one};
use crate::filter::Filter;

/// Max WebSocket message (D3, negentropy frames).
pub const MAX_MESSAGE_BYTES: usize = 16 << 20;
pub const MAX_NEG_SESSIONS: usize = 10;
/// Max items per NEG-OPEN (D13).
pub const MAX_NEG_ITEMS: usize = 500_000;
pub const MAX_SUBSCRIPTIONS: usize = 32;
pub const MAX_SUB_ID_LEN: usize = 64;

pub async fn ws_handler(ws: WebSocketUpgrade, State(st): State<AppState>) -> Response {
    ws.max_message_size(MAX_MESSAGE_BYTES)
        .max_frame_size(MAX_MESSAGE_BYTES)
        .on_upgrade(move |socket| serve(socket, st))
}

fn notice(msg: &str) -> String {
    json!(["NOTICE", msg]).to_string()
}

fn closed(sub: &str, msg: &str) -> String {
    json!(["CLOSED", sub, msg]).to_string()
}

fn neg_err(sub: &str, msg: &str) -> String {
    json!(["NEG-ERR", sub, msg]).to_string()
}

fn event_msg(sub: &str, ev: &RawEvent) -> String {
    // RawEvent::to_json keeps tags/content verbatim; splice it in rather than re-encoding.
    format!(
        "[\"EVENT\",{},{}]",
        Value::String(sub.to_string()),
        ev.to_json()
    )
}

/// Per-connection state.
struct Session {
    st: AppState,
    subs: HashMap<String, Vec<Filter>>,
    negs: HashMap<String, NegResponder>,
}

async fn serve(socket: WebSocket, st: AppState) {
    let (mut tx, mut rx) = socket.split();
    // Subscribe before answering any REQ so nothing stored meanwhile is missed.
    let mut live = st.store.subscribe();
    let mut shutdown = st.shutdown.clone();
    let mut watching = true;
    let mut session = Session {
        st,
        subs: HashMap::new(),
        negs: HashMap::new(),
    };
    loop {
        let out = tokio::select! {
            msg = rx.next() => match msg {
                Some(Ok(Message::Text(t))) => session.handle(t.as_str()).await,
                Some(Ok(Message::Binary(_))) => vec![notice("invalid: binary messages are not supported")],
                Some(Ok(Message::Ping(_) | Message::Pong(_))) => continue,
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
            },
            ev = live.recv() => match ev {
                Ok(ev) => session.live(&ev),
                Err(RecvError::Lagged(n)) => {
                    tracing::warn!("relay subscriber lagged, {n} live events dropped");
                    Vec::new()
                }
                Err(RecvError::Closed) => break,
            },
            changed = shutdown.changed(), if watching => {
                match changed {
                    Err(_) => watching = false,
                    Ok(()) if *shutdown.borrow() => break,
                    Ok(()) => {}
                }
                continue;
            }
        };
        for m in out {
            if tx.send(Message::Text(m.into())).await.is_err() {
                return;
            }
        }
    }
    let _ = tx.send(Message::Close(None)).await;
}

fn sub_id(v: Option<&Value>) -> Option<String> {
    v.and_then(Value::as_str)
        .filter(|s| !s.is_empty() && s.chars().count() <= MAX_SUB_ID_LEN)
        .map(str::to_string)
}

impl Session {
    async fn handle(&mut self, text: &str) -> Vec<String> {
        let Ok(Value::Array(msg)) = serde_json::from_str::<Value>(text) else {
            return vec![notice("invalid: message is not a JSON array")];
        };
        match msg.first().and_then(Value::as_str) {
            Some("EVENT") => vec![self.event(msg.get(1)).await],
            Some("REQ") => self.req(&msg).await,
            Some("CLOSE") => {
                if let Some(sub) = sub_id(msg.get(1)) {
                    self.subs.remove(&sub);
                }
                Vec::new()
            }
            Some("NEG-OPEN") => vec![self.neg_open(&msg).await],
            Some("NEG-MSG") => self.neg_msg(&msg).into_iter().collect(),
            Some("NEG-CLOSE") => {
                if let Some(sub) = sub_id(msg.get(1)) {
                    self.negs.remove(&sub);
                }
                Vec::new()
            }
            Some(other) => vec![notice(&format!("unsupported: message type {other}"))],
            None => vec![notice("invalid: missing message type")],
        }
    }

    async fn event(&self, v: Option<&Value>) -> String {
        let Some(v) = v else {
            return notice("invalid: EVENT without an event");
        };
        let id = v.get("id").and_then(Value::as_str).unwrap_or_default().to_string();
        let raw: RawEvent = match serde_json::from_value(v.clone()) {
            Ok(raw) => raw,
            Err(_) if id.is_empty() => return notice("invalid: bad event json"),
            Err(_) => return json!(["OK", id, false, "invalid: bad event json"]).to_string(),
        };
        let r = ingest_one(&self.st.store, raw, magnetar_core::now()).await;
        json!(["OK", r.id, r.accepted, r.message]).to_string()
    }

    async fn req(&mut self, msg: &[Value]) -> Vec<String> {
        let Some(sub) = sub_id(msg.get(1)) else {
            return vec![notice("invalid: bad subscription id")];
        };
        let filters: Result<Vec<Filter>, String> = msg[2..].iter().map(Filter::from_json).collect();
        let filters = match filters {
            Ok(f) if f.is_empty() => return vec![closed(&sub, "invalid: no filters")],
            Ok(f) => f,
            Err(e) => {
                self.subs.remove(&sub);
                return vec![closed(&sub, &e)];
            }
        };
        if !self.subs.contains_key(&sub) && self.subs.len() >= MAX_SUBSCRIPTIONS {
            return vec![closed(&sub, "blocked: too many subscriptions")];
        }
        let mut events: Vec<RawEvent> = Vec::new();
        for f in &filters {
            match self.st.store.query_events(f).await {
                Ok(evs) => events.extend(evs),
                Err(e) => {
                    self.subs.remove(&sub);
                    return vec![closed(&sub, &format!("error: {e}"))];
                }
            }
        }
        events.sort_by(|a, b| b.created_at.cmp(&a.created_at).then_with(|| a.id.cmp(&b.id)));
        events.dedup_by(|a, b| a.id == b.id);
        let mut out: Vec<String> = events.iter().map(|e| event_msg(&sub, e)).collect();
        out.push(json!(["EOSE", sub]).to_string());
        self.subs.insert(sub, filters);
        out
    }

    fn live(&self, ev: &RawEvent) -> Vec<String> {
        self.subs
            .iter()
            .filter(|(_, fs)| fs.iter().any(|f| f.matches(ev)))
            .map(|(sub, _)| event_msg(sub, ev))
            .collect()
    }

    async fn neg_open(&mut self, msg: &[Value]) -> String {
        let Some(sub) = sub_id(msg.get(1)) else {
            return notice("invalid: bad subscription id");
        };
        // Re-opening a subscription replaces it.
        self.negs.remove(&sub);
        if self.negs.len() >= MAX_NEG_SESSIONS {
            return neg_err(&sub, "blocked: too many negentropy sessions");
        }
        let filter = match msg.get(2).map(Filter::from_json) {
            Some(Ok(f)) if f.neg_compatible() => f,
            Some(Ok(_)) => return neg_err(&sub, "blocked: unsupported filter"),
            Some(Err(e)) if e.starts_with("unsupported") => {
                return neg_err(&sub, "blocked: unsupported filter");
            }
            Some(Err(e)) => return neg_err(&sub, &e),
            None => return neg_err(&sub, "invalid: missing filter"),
        };
        let Some(initial) = msg.get(3).and_then(Value::as_str) else {
            return neg_err(&sub, "invalid: missing initial message");
        };
        if hex::decode(initial).is_err() {
            return neg_err(&sub, "invalid: bad hex");
        }
        let items = match self.st.store.neg_items(&filter, MAX_NEG_ITEMS).await {
            Ok(items) => items,
            Err(e) => return neg_err(&sub, &format!("error: {e}")),
        };
        if items.len() > MAX_NEG_ITEMS {
            return neg_err(&sub, "blocked: this query is too big");
        }
        let mut neg = match NegResponder::new(items) {
            Ok(n) => n,
            Err(e) => return neg_err(&sub, &format!("error: {e}")),
        };
        match neg.reconcile_hex(initial) {
            Ok(reply) => {
                self.negs.insert(sub.clone(), neg);
                json!(["NEG-MSG", sub, reply]).to_string()
            }
            Err(e) => neg_err(&sub, &format!("invalid: {e}")),
        }
    }

    fn neg_msg(&mut self, msg: &[Value]) -> Option<String> {
        let sub = sub_id(msg.get(1))?;
        let Some(neg) = self.negs.get_mut(&sub) else {
            return Some(neg_err(&sub, "closed: unknown negentropy subscription"));
        };
        let reply = match msg.get(2).and_then(Value::as_str) {
            Some(m) if hex::decode(m).is_ok() => neg.reconcile_hex(m),
            _ => Err(magnetar_core::Error::Invalid("bad hex".into())),
        };
        Some(match reply {
            Ok(reply) => json!(["NEG-MSG", sub, reply]).to_string(),
            Err(e) => {
                self.negs.remove(&sub);
                neg_err(&sub, &format!("invalid: {e}"))
            }
        })
    }
}

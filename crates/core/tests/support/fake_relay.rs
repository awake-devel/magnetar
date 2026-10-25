//! In-process NIP-01 relay with relay.nostr.net-like behaviour: `limit` clamped to a page size,
//! inclusive `until`/`since`, newest first with ties by lowest id, optional CLOSED on chosen REQs,
//! a NOTICE on connect, strfry-style NOTICE for unknown commands.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use futures::{SinkExt, StreamExt};
use magnetar_core::model::RawEvent;
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;

#[derive(Clone, Debug)]
pub struct Behaviour {
    pub page_clamp: usize,
    /// 1-based global REQ numbers answered with CLOSED instead of events.
    pub closed_reqs: Vec<usize>,
    pub notice_on_connect: bool,
    /// Accept REQs but never answer them.
    pub silent: bool,
}

impl Default for Behaviour {
    fn default() -> Self {
        Self {
            page_clamp: 100,
            closed_reqs: Vec::new(),
            notice_on_connect: true,
            silent: false,
        }
    }
}

#[derive(Default)]
pub struct State {
    pub events: Vec<RawEvent>,
    /// Every REQ filter received (first filter of each REQ), in order.
    pub reqs: Vec<Value>,
    pub closes: usize,
    pub connections: usize,
    pub published: Vec<RawEvent>,
}

pub struct FakeRelay {
    pub url: String,
    pub state: Arc<Mutex<State>>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for FakeRelay {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl FakeRelay {
    pub async fn start(events: Vec<RawEvent>, behaviour: Behaviour) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let state = Arc::new(Mutex::new(State {
            events,
            ..State::default()
        }));
        let st = state.clone();
        let task = tokio::spawn(async move {
            while let Ok((tcp, _)) = listener.accept().await {
                let st = st.clone();
                let b = behaviour.clone();
                tokio::spawn(async move {
                    let _ = serve(tcp, st, b).await;
                });
            }
        });
        Self { url, state, task }
    }

    pub fn add(&self, events: impl IntoIterator<Item = RawEvent>) {
        self.state.lock().unwrap().events.extend(events);
    }

    pub fn reqs(&self) -> Vec<Value> {
        self.state.lock().unwrap().reqs.clone()
    }
}

async fn serve(
    tcp: tokio::net::TcpStream,
    st: Arc<Mutex<State>>,
    b: Behaviour,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut ws = tokio_tungstenite::accept_async(tcp).await?;
    st.lock().unwrap().connections += 1;
    if b.notice_on_connect {
        ws.send(text(json!(["NOTICE", "welcome to the fake relay"]))).await?;
    }
    while let Some(msg) = ws.next().await {
        let msg = msg?;
        let Message::Text(t) = msg else {
            continue;
        };
        let Ok(Value::Array(arr)) = serde_json::from_str::<Value>(t.as_str()) else {
            ws.send(text(json!(["NOTICE", "ERROR: bad msg: not json"]))).await?;
            continue;
        };
        match arr.first().and_then(Value::as_str) {
            Some("REQ") => {
                let sub = arr.get(1).and_then(Value::as_str).unwrap_or("").to_string();
                let filters: Vec<Value> = arr.iter().skip(2).cloned().collect();
                let (n, out) = {
                    let mut s = st.lock().unwrap();
                    s.reqs.push(filters.first().cloned().unwrap_or(Value::Null));
                    let n = s.reqs.len();
                    (n, query(&s.events, &filters, b.page_clamp))
                };
                if b.silent {
                    continue;
                }
                if b.closed_reqs.contains(&n) {
                    ws.send(text(json!(["CLOSED", sub, "rate-limited: slow down"]))).await?;
                    continue;
                }
                for ev in out {
                    ws.send(text(json!(["EVENT", sub, ev]))).await?;
                }
                ws.send(text(json!(["EOSE", sub]))).await?;
            }
            Some("CLOSE") => st.lock().unwrap().closes += 1,
            Some("EVENT") => {
                let Some(ev) = arr.get(1).and_then(|v| serde_json::from_value::<RawEvent>(v.clone()).ok())
                else {
                    ws.send(text(json!(["NOTICE", "ERROR: bad event"]))).await?;
                    continue;
                };
                let dup = {
                    let mut s = st.lock().unwrap();
                    let dup = s.events.iter().any(|e| e.id == ev.id);
                    if !dup {
                        s.events.push(ev.clone());
                        s.published.push(ev.clone());
                    }
                    dup
                };
                let msg = if dup { "duplicate: already have this event" } else { "" };
                ws.send(text(json!(["OK", ev.id, true, msg]))).await?;
            }
            _ => {
                ws.send(text(json!(["NOTICE", "ERROR: bad msg: unknown cmd"]))).await?;
            }
        }
    }
    Ok(())
}

fn text(v: Value) -> Message {
    Message::Text(v.to_string().into())
}

fn query(events: &[RawEvent], filters: &[Value], clamp: usize) -> Vec<RawEvent> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for f in filters {
        let mut matched: Vec<&RawEvent> = events.iter().filter(|e| matches(e, f)).collect();
        matched.sort_by(|a, b| b.created_at.cmp(&a.created_at).then(a.id.cmp(&b.id)));
        let limit = f
            .get("limit")
            .and_then(Value::as_u64)
            .map_or(clamp, |l| (l as usize).min(clamp));
        for e in matched.into_iter().take(limit) {
            if seen.insert(e.id.clone()) {
                out.push(e.clone());
            }
        }
    }
    out
}

fn matches(e: &RawEvent, f: &Value) -> bool {
    let Some(obj) = f.as_object() else {
        return false;
    };
    for (k, v) in obj {
        let list = || v.as_array().cloned().unwrap_or_default();
        let ok = match k.as_str() {
            "kinds" => list().iter().any(|x| x.as_u64() == Some(e.kind as u64)),
            "authors" => list().iter().any(|x| x.as_str() == Some(&e.pubkey)),
            "ids" => list().iter().any(|x| x.as_str() == Some(&e.id)),
            "since" => v.as_i64().is_some_and(|s| e.created_at >= s),
            "until" => v.as_i64().is_some_and(|u| e.created_at <= u),
            "limit" => true,
            tag if tag.starts_with('#') && tag.len() == 2 => {
                let name = &tag[1..];
                let wanted = list();
                e.tags.iter().any(|t| {
                    t.first().map(String::as_str) == Some(name)
                        && t.get(1).is_some_and(|val| wanted.iter().any(|w| w.as_str() == Some(val)))
                })
            }
            _ => false,
        };
        if !ok {
            return false;
        }
    }
    true
}

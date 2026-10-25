//! Thin NIP-01/NIP-77 relay WebSocket client (D3). Owner: nostr agent.
//! tokio-tungstenite with our rustls config (crate::http::tls_config), max message 16 MiB,
//! 10 s connect timeout, ping keepalive. Tolerates NOTICE/AUTH/CLOSED at any time.

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::net::TcpStream;
use tokio::time::Instant;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::{Connector, MaybeTlsStream, WebSocketStream};

use crate::error::{Error, Result};
use crate::model::RawEvent;

pub const MAX_MESSAGE_BYTES: usize = 16 << 20;
/// Connect never waits longer than this, whatever the caller asks.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// A ping is sent when the relay has been silent this long while we wait.
const KEEPALIVE: Duration = Duration::from_secs(20);
/// Safety cap on events collected for one REQ (relays clamp `limit`, but a broken one might not).
const MAX_PAGE_EVENTS: usize = 10_000;

/// A relay → client message (unknown types are kept as `Other`).
#[derive(Clone, Debug, PartialEq)]
pub enum RelayMsg {
    Event {
        sub: String,
        event: RawEvent,
    },
    Eose {
        sub: String,
    },
    Closed {
        sub: String,
        msg: String,
    },
    Notice(String),
    Ok {
        id: String,
        accepted: bool,
        msg: String,
    },
    Auth(String),
    NegMsg {
        sub: String,
        hex: String,
    },
    NegErr {
        sub: String,
        msg: String,
    },
    Other(String),
}

impl RelayMsg {
    /// Tolerant parse of one relay frame: the type is matched case-insensitively, missing
    /// trailing fields default to empty, and anything unrecognised becomes `Other`.
    pub fn parse(text: &str) -> RelayMsg {
        Self::parse_value(text).unwrap_or_else(|| RelayMsg::Other(text.to_string()))
    }

    fn parse_value(text: &str) -> Option<RelayMsg> {
        let v: Value = serde_json::from_str(text).ok()?;
        let arr = v.as_array()?;
        let kind = arr.first()?.as_str()?.to_ascii_uppercase();
        let s = |i: usize| match arr.get(i) {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Null) | None => String::new(),
            Some(other) => other.to_string(),
        };
        Some(match kind.as_str() {
            "EVENT" => RelayMsg::Event {
                sub: s(1),
                event: serde_json::from_value(arr.get(2)?.clone()).ok()?,
            },
            "EOSE" => RelayMsg::Eose { sub: s(1) },
            "CLOSED" => RelayMsg::Closed {
                sub: s(1),
                msg: s(2),
            },
            "NOTICE" => RelayMsg::Notice(s(1)),
            "OK" => RelayMsg::Ok {
                id: s(1),
                accepted: match arr.get(2)? {
                    Value::Bool(b) => *b,
                    Value::String(b) => b.eq_ignore_ascii_case("true"),
                    _ => return None,
                },
                msg: s(3),
            },
            "AUTH" => RelayMsg::Auth(s(1)),
            "NEG-MSG" => RelayMsg::NegMsg {
                sub: s(1),
                hex: s(2),
            },
            "NEG-ERR" => RelayMsg::NegErr {
                sub: s(1),
                msg: s(2),
            },
            _ => return None,
        })
    }
}

/// Outcome of one REQ page.
#[derive(Clone, Debug, Default)]
pub struct Page {
    pub events: Vec<RawEvent>,
    /// Some(msg) if the relay sent CLOSED (or NOTICE treated as fatal for this sub) instead of EOSE.
    pub closed: Option<String>,
    pub timed_out: bool,
}

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

pub struct RelayConn {
    pub url: String,
    ws: Ws,
    sub_prefix: String,
    next_sub: u64,
    dead: bool,
}

fn tls() -> Arc<rustls::ClientConfig> {
    static TLS: OnceLock<Arc<rustls::ClientConfig>> = OnceLock::new();
    TLS.get_or_init(|| Arc::new(crate::http::tls_config()))
        .clone()
}

/// NOTICEs that mean the current REQ will never get its EOSE (strfry answers malformed or
/// unsupported REQs with a NOTICE only).
fn notice_is_fatal(msg: &str) -> bool {
    let m = msg.to_ascii_lowercase();
    [
        "error",
        "invalid",
        "bad req",
        "bad msg",
        "rate-limit",
        "too many",
        "blocked",
    ]
    .iter()
    .any(|p| m.contains(p))
}

impl RelayConn {
    /// Connect to a ws(s):// url (query string kept, e.g. `?token=` for our own relay).
    pub async fn connect(url: &str, timeout: Duration) -> Result<Self> {
        let err = |msg: String| Error::Relay {
            relay: url.to_string(),
            msg,
        };
        let request = url
            .into_client_request()
            .map_err(|e| err(format!("bad url: {e}")))?;
        let config = WebSocketConfig::default()
            .max_message_size(Some(MAX_MESSAGE_BYTES))
            .max_frame_size(Some(MAX_MESSAGE_BYTES));
        let connect = tokio_tungstenite::connect_async_tls_with_config(
            request,
            Some(config),
            true,
            Some(Connector::Rustls(tls())),
        );
        let (ws, _) = tokio::time::timeout(timeout.min(CONNECT_TIMEOUT), connect)
            .await
            .map_err(|_| Error::Timeout(format!("connect {url}")))?
            .map_err(|e| err(format!("connect: {e}")))?;
        Ok(Self {
            url: url.to_string(),
            ws,
            sub_prefix: format!("mg{:08x}", rand::random::<u32>()),
            next_sub: 0,
            dead: false,
        })
    }

    /// False once the relay closed the connection or a send/receive failed.
    pub fn is_open(&self) -> bool {
        !self.dead
    }

    fn err(&self, msg: impl Into<String>) -> Error {
        Error::Relay {
            relay: self.url.clone(),
            msg: msg.into(),
        }
    }

    /// Unique per connection, well under the 64-char NIP-01 limit.
    fn new_sub(&mut self) -> String {
        self.next_sub += 1;
        format!("{}-{}", self.sub_prefix, self.next_sub)
    }

    pub async fn send_json(&mut self, msg: &serde_json::Value) -> Result<()> {
        if self.dead {
            return Err(self.err("connection closed"));
        }
        let text = msg.to_string();
        if let Err(e) = self.ws.send(Message::Text(text.into())).await {
            self.dead = true;
            return Err(self.err(format!("send: {e}")));
        }
        Ok(())
    }

    /// Next relay message, None on close. Errors on timeout.
    pub async fn recv(&mut self, timeout: Duration) -> Result<Option<RelayMsg>> {
        if self.dead {
            return Ok(None);
        }
        let deadline = Instant::now() + timeout;
        loop {
            let now = Instant::now();
            if now >= deadline {
                return Err(Error::Timeout(format!("{}: no message", self.url)));
            }
            let wait = (deadline - now).min(KEEPALIVE);
            let next = match tokio::time::timeout(wait, self.ws.next()).await {
                Ok(next) => next,
                Err(_) => {
                    if Instant::now() < deadline {
                        // Silent relay: keep NATs and proxies from dropping us.
                        let _ = self.ws.send(Message::Ping(Vec::new().into())).await;
                    }
                    continue;
                }
            };
            let text = match next {
                None => {
                    self.dead = true;
                    return Ok(None);
                }
                Some(Err(e)) => {
                    self.dead = true;
                    use tokio_tungstenite::tungstenite::Error as WsError;
                    return match e {
                        WsError::ConnectionClosed | WsError::AlreadyClosed => Ok(None),
                        e => Err(self.err(format!("recv: {e}"))),
                    };
                }
                Some(Ok(Message::Text(t))) => t.as_str().to_string(),
                Some(Ok(Message::Binary(b))) => match String::from_utf8(b.to_vec()) {
                    Ok(t) => t,
                    Err(_) => continue,
                },
                Some(Ok(Message::Close(_))) => {
                    self.dead = true;
                    return Ok(None);
                }
                // Pings are answered by tungstenite on the next read/write.
                Some(Ok(_)) => continue,
            };
            return Ok(Some(RelayMsg::parse(&text)));
        }
    }

    /// REQ with one filter (JSON object), collect until EOSE/CLOSED/timeout, then CLOSE.
    pub async fn fetch_page(
        &mut self,
        filter: serde_json::Value,
        timeout: Duration,
    ) -> Result<Page> {
        self.fetch_page_multi(vec![filter], timeout).await
    }

    /// Like [`fetch_page`](Self::fetch_page) with several filters ORed in one REQ.
    pub async fn fetch_page_multi(
        &mut self,
        filters: Vec<serde_json::Value>,
        timeout: Duration,
    ) -> Result<Page> {
        let sub = self.new_sub();
        let mut req = vec![json!("REQ"), json!(sub)];
        req.extend(filters);
        self.send_json(&Value::Array(req)).await?;
        let deadline = Instant::now() + timeout;
        let mut page = Page::default();
        let mut need_close = true;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let msg = match self.recv(left).await {
                Ok(Some(m)) => m,
                Ok(None) => {
                    page.closed = Some("connection closed".into());
                    need_close = false;
                    break;
                }
                Err(Error::Timeout(_)) => {
                    page.timed_out = true;
                    break;
                }
                Err(e) => return Err(e),
            };
            match msg {
                RelayMsg::Event { sub: s, event } if s == sub => {
                    page.events.push(event);
                    if page.events.len() >= MAX_PAGE_EVENTS {
                        break;
                    }
                }
                RelayMsg::Eose { sub: s } if s == sub => break,
                RelayMsg::Closed { sub: s, msg } if s == sub => {
                    page.closed = Some(msg);
                    need_close = false;
                    break;
                }
                RelayMsg::Notice(msg) => {
                    tracing::debug!(relay = %self.url, %msg, "NOTICE");
                    if notice_is_fatal(&msg) {
                        page.closed = Some(format!("notice: {msg}"));
                        break;
                    }
                }
                RelayMsg::Auth(_) => tracing::debug!(relay = %self.url, "AUTH challenge ignored"),
                other => tracing::trace!(relay = %self.url, ?other, "ignored message"),
            }
        }
        if need_close {
            let _ = self.send_json(&json!(["CLOSE", sub])).await;
        }
        Ok(page)
    }

    /// Publish an event and wait for its OK. Returns (accepted, message).
    pub async fn publish(&mut self, ev: &RawEvent, timeout: Duration) -> Result<(bool, String)> {
        let value = serde_json::to_value(ev)?;
        self.send_json(&json!(["EVENT", value])).await?;
        let deadline = Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.recv(left).await? {
                Some(RelayMsg::Ok { id, accepted, msg }) if id.eq_ignore_ascii_case(&ev.id) => {
                    return Ok((accepted, msg));
                }
                Some(_) => {}
                None => return Err(self.err("connection closed before OK")),
            }
        }
    }

    pub async fn close(mut self) {
        if !self.dead {
            let _ = tokio::time::timeout(Duration::from_secs(2), self.ws.close(None)).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_relay_messages_tolerantly() {
        let ev = r#"{"id":"aa","pubkey":"bb","created_at":1,"kind":2003,"tags":[],"content":"","sig":"cc"}"#;
        match RelayMsg::parse(&format!(r#"["event","s1",{ev}]"#)) {
            RelayMsg::Event { sub, event } => {
                assert_eq!(sub, "s1");
                assert_eq!(event.kind, 2003);
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(
            RelayMsg::parse(r#"["EOSE","s1"]"#),
            RelayMsg::Eose { sub: "s1".into() }
        );
        assert_eq!(
            RelayMsg::parse(
                r#"["CLOSED","s1","ERROR: bad req: unrecognised filter item: search"]"#
            ),
            RelayMsg::Closed {
                sub: "s1".into(),
                msg: "ERROR: bad req: unrecognised filter item: search".into()
            }
        );
        assert_eq!(
            RelayMsg::parse(r#"["CLOSED","s1"]"#),
            RelayMsg::Closed {
                sub: "s1".into(),
                msg: String::new()
            }
        );
        assert_eq!(
            RelayMsg::parse(r#"["NOTICE","hello"]"#),
            RelayMsg::Notice("hello".into())
        );
        assert_eq!(
            RelayMsg::parse(r#"["OK","ab",true]"#),
            RelayMsg::Ok {
                id: "ab".into(),
                accepted: true,
                msg: String::new()
            }
        );
        assert_eq!(
            RelayMsg::parse(r#"["OK","ab",false,"duplicate: have it"]"#),
            RelayMsg::Ok {
                id: "ab".into(),
                accepted: false,
                msg: "duplicate: have it".into()
            }
        );
        assert_eq!(
            RelayMsg::parse(r#"["AUTH","challenge"]"#),
            RelayMsg::Auth("challenge".into())
        );
        assert_eq!(
            RelayMsg::parse(r#"["NEG-MSG","n1","61"]"#),
            RelayMsg::NegMsg {
                sub: "n1".into(),
                hex: "61".into()
            }
        );
        assert_eq!(
            RelayMsg::parse(r#"["NEG-ERR","n1","closed"]"#),
            RelayMsg::NegErr {
                sub: "n1".into(),
                msg: "closed".into()
            }
        );
        for junk in [
            r#"["COUNT","c",{"count":3}]"#,
            "not json",
            "{}",
            "[]",
            r#"["EVENT","s",{"id":1}]"#,
        ] {
            assert_eq!(RelayMsg::parse(junk), RelayMsg::Other(junk.into()));
        }
    }

    #[test]
    fn fatal_notices() {
        assert!(notice_is_fatal("ERROR: bad msg: unknown cmd"));
        assert!(notice_is_fatal("rate-limited: slow down"));
        assert!(!notice_is_fatal("welcome to the relay"));
    }
}

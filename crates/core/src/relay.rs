//! Thin NIP-01/NIP-77 relay WebSocket client (D3). Owner: nostr agent.
//! tokio-tungstenite with our rustls config (crate::http::tls_config), max message 16 MiB,
//! 10 s connect timeout, ping keepalive. Tolerates NOTICE/AUTH/CLOSED at any time.

use std::time::Duration;

use crate::error::Result;
use crate::model::RawEvent;

pub const MAX_MESSAGE_BYTES: usize = 16 << 20;

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

/// Outcome of one REQ page.
#[derive(Clone, Debug, Default)]
pub struct Page {
    pub events: Vec<RawEvent>,
    /// Some(msg) if the relay sent CLOSED (or NOTICE treated as fatal for this sub) instead of EOSE.
    pub closed: Option<String>,
    pub timed_out: bool,
}

pub struct RelayConn {
    pub url: String,
    // private: ws stream, sub counter
}

impl RelayConn {
    /// Connect to a ws(s):// url (query string kept, e.g. `?token=` for our own relay).
    pub async fn connect(url: &str, timeout: Duration) -> Result<Self> {
        let _ = (url, timeout);
        todo!("relay::connect")
    }
    pub async fn send_json(&mut self, msg: &serde_json::Value) -> Result<()> {
        let _ = msg;
        todo!()
    }
    /// Next relay message, None on close. Errors on timeout.
    pub async fn recv(&mut self, timeout: Duration) -> Result<Option<RelayMsg>> {
        let _ = timeout;
        todo!()
    }
    /// REQ with one filter (JSON object), collect until EOSE/CLOSED/timeout, then CLOSE.
    pub async fn fetch_page(
        &mut self,
        filter: serde_json::Value,
        timeout: Duration,
    ) -> Result<Page> {
        let _ = (filter, timeout);
        todo!()
    }
    /// Publish an event and wait for its OK. Returns (accepted, message).
    pub async fn publish(&mut self, ev: &RawEvent, timeout: Duration) -> Result<(bool, String)> {
        let _ = (ev, timeout);
        todo!()
    }
    pub async fn close(self) {
        todo!()
    }
}

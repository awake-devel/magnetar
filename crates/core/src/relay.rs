//! Thin NIP-01/NIP-77 relay WebSocket client (D3). Owner: nostr agent.
//! tokio-tungstenite with our rustls config (crate::http::tls_config), 4 MiB max message and a
//! bounded event decode (D111), 10 s connect timeout, ping keepalive. Tolerates
//! NOTICE/AUTH/CLOSED at any time.

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use std::cell::Cell;

use futures::{SinkExt, StreamExt};
use serde::Deserialize;
use serde::de::{self, DeserializeSeed, Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde_json::{Value, json};
use tokio::net::TcpStream;
use tokio::time::Instant;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::{Connector, MaybeTlsStream, WebSocketStream};

use crate::error::{Error, Result};
use crate::model::RawEvent;
use crate::netaddr::AddrPolicy;

/// Largest relay message (and frame) accepted (D111). Above ingest's `MAX_EVENT_BYTES` (1 MiB)
/// on purpose: an honest event a little over it (a kind 3 with ~10k follows and relay hints is
/// ~1.1 MB, and a relay escaping non-ASCII as `\uXXXX` makes the wire text longer than the
/// event) must reach ingest, which drops it as `TooLarge`, instead of failing the receive and
/// with it the whole relay. A message above this ends the connection. Was 16 MiB.
pub const MAX_MESSAGE_BYTES: usize = 4 << 20;
/// Decoded cost ([`decoded_cost`]) of the events one REQ page may keep (D111): a normal page of
/// 500 NIP-35 events costs a few MB, so 16 MiB never cuts a real page, while a relay streaming
/// large or tag-heavy events for the whole page timeout is stopped at about 16 MiB of decoded
/// events instead of 10,000 messages.
pub const MAX_PAGE_BYTES: usize = 16 << 20;
/// Most tags per event kept (D111): above NIP-35's 5000 `file` tags and `wot::MAX_FOLLOWS`
/// (10,000 `p` tags).
pub const MAX_EVENT_TAGS: usize = 20_000;
/// Most strings in all the tags of one event (D111): a 10,000-follow kind 3 has at most 40,000
/// (`["p", key, relay, petname]`), 5000 NIP-35 `file` tags 15,000. Counted while decoding, so
/// one tag of 250k elements (1 MB of text, ~8.6 MB of heap) is dropped after 50,000 of them.
pub const MAX_EVENT_TAG_VALUES: usize = 50_000;
/// Heap charged per decoded tag and tag string on top of its text (D111): a `String` header is
/// 24 bytes and the allocator rounds even a 1-byte string up to 32; the review measured ~34
/// bytes requested per `"a"` element (8.6 MB for 250k), rounded up for allocator overhead.
pub const DECODED_BYTES_PER_STRING: usize = 48;
/// Longest event `content` kept (D111): NIP-35 descriptions are cut at 64 KiB and profiles are
/// small, so nothing we use is longer.
pub const MAX_EVENT_CONTENT_BYTES: usize = 256 << 10;
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
    /// trailing fields default to empty, and anything unrecognised becomes `Other`. Decoded
    /// without an intermediate `serde_json::Value`, so an EVENT over the size limits is dropped
    /// while it is being read, never decoded whole (D111).
    pub fn parse(text: &str) -> RelayMsg {
        let dropped = Cell::new(None);
        let mut de = serde_json::Deserializer::from_str(text);
        let parsed = WireSeed(&dropped)
            .deserialize(&mut de)
            .and_then(|w| de.end().map(|()| w));
        if let Some(why) = dropped.take() {
            return RelayMsg::Other(format!("EVENT dropped: {why}"));
        }
        parsed
            .ok()
            .and_then(Self::from_wire)
            .unwrap_or_else(|| RelayMsg::Other(text.to_string()))
    }

    fn from_wire(w: Wire) -> Option<RelayMsg> {
        let Wire {
            kind,
            fields,
            event,
        } = w;
        let s = |i: usize| fields.get(i).map_or_else(String::new, Field::text);
        Some(match kind.to_ascii_uppercase().as_str() {
            "EVENT" => RelayMsg::Event {
                sub: s(0),
                event: event?,
            },
            "EOSE" => RelayMsg::Eose { sub: s(0) },
            "CLOSED" => RelayMsg::Closed {
                sub: s(0),
                msg: s(1),
            },
            "NOTICE" => RelayMsg::Notice(s(0)),
            "OK" => RelayMsg::Ok {
                id: s(0),
                accepted: match fields.get(1)? {
                    Field::Bool(b) => *b,
                    Field::Str(b) => b.eq_ignore_ascii_case("true"),
                    _ => return None,
                },
                msg: s(2),
            },
            "AUTH" => RelayMsg::Auth(s(0)),
            "NEG-MSG" => RelayMsg::NegMsg {
                sub: s(0),
                hex: s(1),
            },
            "NEG-ERR" => RelayMsg::NegErr {
                sub: s(0),
                msg: s(1),
            },
            _ => return None,
        })
    }
}

/// What one kept event is charged against [`MAX_PAGE_BYTES`] (D111): its message text plus
/// [`DECODED_BYTES_PER_STRING`] for every tag and tag string, since `["a"],` is 6 bytes of JSON
/// but ~70 bytes once a `Vec<String>`.
pub fn decoded_cost(text_len: usize, ev: &RawEvent) -> usize {
    let strings: usize = ev.tags.iter().map(|t| 1 + t.len()).sum();
    text_len + DECODED_BYTES_PER_STRING * strings
}

/// Where the bounded decoders say why they gave up on an event (D111).
type Dropped = Cell<Option<String>>;

fn drop_event<E: de::Error>(cell: &Dropped, why: String) -> E {
    let err = E::custom(&why);
    cell.set(Some(why));
    err
}

/// A relay message: its type, the scalar fields after it and, for EVENT, the event.
struct Wire {
    kind: String,
    fields: Vec<Field>,
    event: Option<RawEvent>,
}

/// One message field other than the event, kept as far as the parse needs it.
enum Field {
    Str(String),
    Bool(bool),
    Num(String),
    Null,
    /// An array or object, skipped without being decoded.
    Complex,
}

impl Field {
    fn text(&self) -> String {
        match self {
            Field::Str(s) | Field::Num(s) => s.clone(),
            Field::Bool(b) => b.to_string(),
            Field::Null => String::new(),
            Field::Complex => "[…]".into(),
        }
    }
}

impl<'de> Deserialize<'de> for Field {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Field;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a relay message field")
            }
            fn visit_str<E>(self, v: &str) -> std::result::Result<Field, E> {
                Ok(Field::Str(v.to_string()))
            }
            fn visit_string<E>(self, v: String) -> std::result::Result<Field, E> {
                Ok(Field::Str(v))
            }
            fn visit_bool<E>(self, v: bool) -> std::result::Result<Field, E> {
                Ok(Field::Bool(v))
            }
            fn visit_i64<E>(self, v: i64) -> std::result::Result<Field, E> {
                Ok(Field::Num(v.to_string()))
            }
            fn visit_u64<E>(self, v: u64) -> std::result::Result<Field, E> {
                Ok(Field::Num(v.to_string()))
            }
            fn visit_f64<E>(self, v: f64) -> std::result::Result<Field, E> {
                let n = serde_json::Number::from_f64(v).map_or_else(String::new, |n| n.to_string());
                Ok(Field::Num(n))
            }
            fn visit_unit<E>(self) -> std::result::Result<Field, E> {
                Ok(Field::Null)
            }
            fn visit_seq<A: SeqAccess<'de>>(
                self,
                mut a: A,
            ) -> std::result::Result<Field, A::Error> {
                while a.next_element::<IgnoredAny>()?.is_some() {}
                Ok(Field::Complex)
            }
            fn visit_map<A: MapAccess<'de>>(
                self,
                mut a: A,
            ) -> std::result::Result<Field, A::Error> {
                while a.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
                Ok(Field::Complex)
            }
        }
        d.deserialize_any(V)
    }
}

/// Fields kept after the type: NIP-01 messages have at most three.
const MAX_FIELDS: usize = 3;

/// The message array; anything past the fields NIP-01 uses is skipped undecoded.
struct WireSeed<'c>(&'c Dropped);

impl<'de> DeserializeSeed<'de> for WireSeed<'_> {
    type Value = Wire;
    fn deserialize<D: Deserializer<'de>>(self, d: D) -> std::result::Result<Wire, D::Error> {
        struct V<'c>(&'c Dropped);
        impl<'de> Visitor<'de> for V<'_> {
            type Value = Wire;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a relay message array")
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut a: A) -> std::result::Result<Wire, A::Error> {
                let kind: String = a
                    .next_element()?
                    .ok_or_else(|| de::Error::custom("empty message"))?;
                let mut fields = Vec::new();
                let mut event = None;
                if kind.eq_ignore_ascii_case("EVENT") {
                    if let Some(sub) = a.next_element::<Field>()? {
                        fields.push(sub);
                        event = a.next_element_seed(EventSeed(self.0))?;
                    }
                } else {
                    while fields.len() < MAX_FIELDS {
                        match a.next_element::<Field>()? {
                            Some(f) => fields.push(f),
                            None => break,
                        }
                    }
                }
                while a.next_element::<IgnoredAny>()?.is_some() {}
                Ok(Wire {
                    kind,
                    fields,
                    event,
                })
            }
        }
        d.deserialize_seq(V(self.0))
    }
}

/// Decodes one event as `RawEvent`'s derive does (unknown fields ignored, duplicates refused),
/// but gives up as soon as it passes [`MAX_EVENT_TAGS`], [`MAX_EVENT_TAG_VALUES`] or
/// [`MAX_EVENT_CONTENT_BYTES`] (D111).
struct EventSeed<'c>(&'c Dropped);

fn set_once<T, E: de::Error>(
    slot: &mut Option<T>,
    v: T,
    name: &'static str,
) -> std::result::Result<(), E> {
    if slot.replace(v).is_some() {
        return Err(E::duplicate_field(name));
    }
    Ok(())
}

impl<'de> DeserializeSeed<'de> for EventSeed<'_> {
    type Value = RawEvent;
    fn deserialize<D: Deserializer<'de>>(self, d: D) -> std::result::Result<RawEvent, D::Error> {
        struct V<'c>(&'c Dropped);
        impl<'de> Visitor<'de> for V<'_> {
            type Value = RawEvent;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a nostr event")
            }
            fn visit_map<A: MapAccess<'de>>(
                self,
                mut a: A,
            ) -> std::result::Result<RawEvent, A::Error> {
                let (mut id, mut pubkey, mut sig, mut content) = (None, None, None, None);
                let (mut created_at, mut kind, mut tags) = (None, None, None);
                while let Some(key) = a.next_key::<std::borrow::Cow<'de, str>>()? {
                    match key.as_ref() {
                        "id" => set_once(&mut id, a.next_value::<String>()?, "id")?,
                        "pubkey" => set_once(&mut pubkey, a.next_value::<String>()?, "pubkey")?,
                        "sig" => set_once(&mut sig, a.next_value::<String>()?, "sig")?,
                        "created_at" => {
                            set_once(&mut created_at, a.next_value::<i64>()?, "created_at")?
                        }
                        "kind" => set_once(&mut kind, a.next_value::<u16>()?, "kind")?,
                        "content" => {
                            let c = a.next_value::<String>()?;
                            if c.len() > MAX_EVENT_CONTENT_BYTES {
                                let why = format!(
                                    "content of {} bytes (max {MAX_EVENT_CONTENT_BYTES})",
                                    c.len()
                                );
                                return Err(drop_event(self.0, why));
                            }
                            set_once(&mut content, c, "content")?
                        }
                        "tags" => {
                            if tags.is_some() {
                                return Err(de::Error::duplicate_field("tags"));
                            }
                            tags = Some(a.next_value_seed(TagsSeed(self.0))?);
                        }
                        _ => {
                            a.next_value::<IgnoredAny>()?;
                        }
                    }
                }
                let missing = <A::Error as de::Error>::missing_field;
                Ok(RawEvent {
                    id: id.ok_or_else(|| missing("id"))?,
                    pubkey: pubkey.ok_or_else(|| missing("pubkey"))?,
                    created_at: created_at.ok_or_else(|| missing("created_at"))?,
                    kind: kind.ok_or_else(|| missing("kind"))?,
                    tags: tags.ok_or_else(|| missing("tags"))?,
                    content: content.ok_or_else(|| missing("content"))?,
                    sig: sig.ok_or_else(|| missing("sig"))?,
                })
            }
        }
        d.deserialize_map(V(self.0))
    }
}

/// `tags`, with the tags and the strings of all of them counted while they are read (D111).
struct TagsSeed<'c>(&'c Dropped);

impl<'de> DeserializeSeed<'de> for TagsSeed<'_> {
    type Value = Vec<Vec<String>>;
    fn deserialize<D: Deserializer<'de>>(
        self,
        d: D,
    ) -> std::result::Result<Vec<Vec<String>>, D::Error> {
        struct V<'c>(&'c Dropped);
        impl<'de> Visitor<'de> for V<'_> {
            type Value = Vec<Vec<String>>;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("an array of tags")
            }
            fn visit_seq<A: SeqAccess<'de>>(
                self,
                mut a: A,
            ) -> std::result::Result<Vec<Vec<String>>, A::Error> {
                let mut values = 0usize;
                let mut tags = Vec::new();
                while let Some(tag) = a.next_element_seed(TagSeed {
                    dropped: self.0,
                    values: &mut values,
                })? {
                    if tags.len() == MAX_EVENT_TAGS {
                        let why = format!("more than {MAX_EVENT_TAGS} tags");
                        return Err(drop_event(self.0, why));
                    }
                    tags.push(tag);
                }
                Ok(tags)
            }
        }
        d.deserialize_seq(V(self.0))
    }
}

/// One tag; `values` counts the strings of every tag of the event so far.
struct TagSeed<'c> {
    dropped: &'c Dropped,
    values: &'c mut usize,
}

impl<'de> DeserializeSeed<'de> for TagSeed<'_> {
    type Value = Vec<String>;
    fn deserialize<D: Deserializer<'de>>(self, d: D) -> std::result::Result<Vec<String>, D::Error> {
        d.deserialize_seq(self)
    }
}

impl<'de> Visitor<'de> for TagSeed<'_> {
    type Value = Vec<String>;
    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("a tag (array of strings)")
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut a: A) -> std::result::Result<Vec<String>, A::Error> {
        let mut tag = Vec::new();
        while let Some(v) = a.next_element::<String>()? {
            *self.values += 1;
            if *self.values > MAX_EVENT_TAG_VALUES {
                let why = format!("more than {MAX_EVENT_TAG_VALUES} tag values");
                return Err(drop_event(self.dropped, why));
            }
            tag.push(v);
        }
        Ok(tag)
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

/// TCP connect to the first of `addrs` (all already checked) that answers.
async fn connect_any(addrs: &[std::net::SocketAddr]) -> std::io::Result<TcpStream> {
    let mut last = std::io::Error::new(std::io::ErrorKind::NotFound, "no address");
    for a in addrs {
        match TcpStream::connect(a).await {
            Ok(s) => return Ok(s),
            Err(e) => last = e,
        }
    }
    Err(last)
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
    /// Connect to a ws(s):// url (query string kept, e.g. `?token=` for our own relay). For
    /// trusted URLs only (our own server, configured seeds and indexers, which may live on the
    /// LAN): it connects wherever DNS says. A relay URL learned from events goes through
    /// [`RelayConn::connect_checked`].
    pub async fn connect(url: &str, timeout: Duration) -> Result<Self> {
        Self::open(url, timeout, None).await
    }

    /// [`RelayConn::connect`] for an untrusted relay URL (D112): the host is resolved here, only
    /// addresses `policy` allows are kept ([`Error::Refused`] if none is), and the TCP connect
    /// goes to one of those checked addresses, so a name with A records for 192.168.1.1, a
    /// compose-network or tailnet address never gets the upgrade request.
    pub async fn connect_checked(url: &str, timeout: Duration, policy: AddrPolicy) -> Result<Self> {
        Self::open(url, timeout, Some(policy)).await
    }

    async fn open(url: &str, timeout: Duration, policy: Option<AddrPolicy>) -> Result<Self> {
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
        let timeout = timeout.min(CONNECT_TIMEOUT);
        let deadline = Instant::now() + timeout;
        let ws = match policy {
            None => {
                let connect = tokio_tungstenite::connect_async_tls_with_config(
                    request,
                    Some(config),
                    true,
                    Some(Connector::Rustls(tls())),
                );
                tokio::time::timeout_at(deadline, connect)
                    .await
                    .map_err(|_| Error::Timeout(format!("connect {url}")))?
                    .map_err(|e| err(format!("connect: {e}")))?
                    .0
            }
            Some(policy) => {
                let uri = request.uri();
                let host = uri
                    .host()
                    .ok_or_else(|| err("bad url: no host".into()))?
                    .to_string();
                let port = match (uri.port_u16(), uri.scheme_str()) {
                    (Some(p), _) => p,
                    (None, Some("wss")) => 443,
                    (None, Some("ws")) => 80,
                    _ => return Err(err("bad url: not ws/wss".into())),
                };
                let addrs = policy.resolve(url, &host, port, timeout).await?;
                let stream = tokio::time::timeout_at(deadline, connect_any(&addrs))
                    .await
                    .map_err(|_| Error::Timeout(format!("connect {url}")))?
                    .map_err(|e| err(format!("connect: {e}")))?;
                let _ = stream.set_nodelay(true);
                let handshake = tokio_tungstenite::client_async_tls_with_config(
                    request,
                    stream,
                    Some(config),
                    Some(Connector::Rustls(tls())),
                );
                tokio::time::timeout_at(deadline, handshake)
                    .await
                    .map_err(|_| Error::Timeout(format!("connect {url}")))?
                    .map_err(|e| err(format!("connect: {e}")))?
                    .0
            }
        };
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
        Ok(self.recv_text(timeout).await?.map(|t| RelayMsg::parse(&t)))
    }

    /// Next relay message as received (its length is what the page budget counts).
    async fn recv_text(&mut self, timeout: Duration) -> Result<Option<String>> {
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
            return Ok(Some(text));
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
        let mut kept_bytes = 0usize;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let (msg, len) = match self.recv_text(left).await {
                Ok(Some(t)) => (RelayMsg::parse(&t), t.len()),
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
                    kept_bytes += decoded_cost(len, &event);
                    page.events.push(event);
                    if page.events.len() >= MAX_PAGE_EVENTS || kept_bytes >= MAX_PAGE_BYTES {
                        // Like a short page: the caller goes on from the oldest event kept.
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

    /// D111: events with more tags or longer content than anything we use are dropped before
    /// they are decoded into a `RawEvent`.
    #[test]
    fn oversized_events_are_dropped() {
        let ev = |tags: usize, content: usize| {
            let tags: Vec<Vec<&str>> = (0..tags).map(|_| vec!["a"]).collect();
            json!(["EVENT", "s1", {"id":"aa","pubkey":"bb","created_at":1,"kind":2003,
                "tags": tags, "content": "x".repeat(content), "sig":"cc"}])
            .to_string()
        };
        assert!(matches!(
            RelayMsg::parse(&ev(MAX_EVENT_TAGS, MAX_EVENT_CONTENT_BYTES)),
            RelayMsg::Event { .. }
        ));
        for text in [
            ev(MAX_EVENT_TAGS + 1, 0),
            ev(0, MAX_EVENT_CONTENT_BYTES + 1),
        ] {
            match RelayMsg::parse(&text) {
                RelayMsg::Other(why) => assert!(why.starts_with("EVENT dropped"), "{why}"),
                other => panic!(
                    "kept an oversized event: {:?}",
                    std::mem::discriminant(&other)
                ),
            }
        }
    }

    /// D111 review: `MAX_EVENT_TAGS` counts outer tags only, so one tag with 200k strings
    /// (`["a","a",…]`, ~800 KB of text, ~7 MB once decoded) went through. Tag strings are
    /// counted across all tags while decoding.
    #[test]
    fn tag_strings_are_counted_across_tags() {
        let ev = |tags: Vec<Vec<&str>>| {
            json!(["EVENT", "s1", {"id":"aa","pubkey":"bb","created_at":1,"kind":3,
                "tags": tags, "content": "", "sig":"cc"}])
            .to_string()
        };
        let wide = ev(vec![vec!["a"; 200_000]]);
        assert!(wide.len() < MAX_MESSAGE_BYTES);
        match RelayMsg::parse(&wide) {
            RelayMsg::Other(why) => assert!(why.starts_with("EVENT dropped"), "{why}"),
            other => panic!(
                "kept a 200k-string tag: {:?}",
                std::mem::discriminant(&other)
            ),
        }
        // The count is per event, across tags: 4 × 12,501 strings is one over.
        let split = ev(vec![vec!["p"; MAX_EVENT_TAG_VALUES / 4 + 1]; 4]);
        assert!(matches!(RelayMsg::parse(&split), RelayMsg::Other(_)));
        let full = ev(vec![vec!["p"; MAX_EVENT_TAG_VALUES / 4]; 4]);
        match RelayMsg::parse(&full) {
            RelayMsg::Event { event, .. } => {
                assert_eq!(event.tags.len(), 4);
                // Charged by decoded size, not text: 4 tags + 50,000 strings.
                assert_eq!(
                    decoded_cost(full.len(), &event),
                    full.len() + DECODED_BYTES_PER_STRING * (4 + MAX_EVENT_TAG_VALUES)
                );
            }
            other => panic!("{other:?}"),
        }
    }

    /// The bounded decoder reads events as the `RawEvent` derive did.
    #[test]
    fn bounded_decode_matches_the_derive() {
        let raw = r#"{"id":"aa","pubkey":"bb","created_at":-5,"kind":1,"extra":{"x":[1,2]},
            "tags":[["e","x","wss://r"],[]],"content":"hé","sig":"cc"}"#;
        let want = RawEvent::from_json(raw).unwrap();
        match RelayMsg::parse(&format!(r#"["EVENT",7,{raw},"trailing"]"#)) {
            RelayMsg::Event { sub, event } => {
                assert_eq!(sub, "7");
                assert_eq!(event, want);
            }
            other => panic!("{other:?}"),
        }
        for bad in [
            r#"["EVENT","s",{"id":"aa","pubkey":"bb","created_at":1,"kind":1,"tags":[],"content":"","sig":"cc","id":"dd"}]"#,
            r#"["EVENT","s",{"id":"aa","pubkey":"bb","created_at":1,"kind":70000,"tags":[],"content":"","sig":"cc"}]"#,
            r#"["EVENT","s",{"id":"aa","pubkey":"bb","created_at":1,"kind":1,"tags":[[1]],"content":"","sig":"cc"}]"#,
            r#"["EVENT","s"]"#,
            r#"["EOSE","s"] x"#,
        ] {
            assert_eq!(RelayMsg::parse(bad), RelayMsg::Other(bad.into()), "{bad}");
        }
    }

    #[test]
    fn fatal_notices() {
        assert!(notice_is_fatal("ERROR: bad msg: unknown cmd"));
        assert!(notice_is_fatal("rate-limited: slow down"));
        assert!(!notice_is_fatal("welcome to the relay"));
    }
}

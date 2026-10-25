//! NIP-66 relay discovery (D30, docs/research/nip66.md). Owner: nostr agent.
//!
//! Relay monitors publish one kind-30166 event per (monitor, relay) with the relay URL in `d`,
//! refreshed every check, and announce themselves with kind 10166. One run:
//! 1. kind 10166 from every monitor relay → the set of announced monitor pubkeys,
//! 2. kind 30166 by those authors from the last `max_age`, paginated (D8) and bounded,
//! 3. verify, keep clearnet relays no monitor marks as requiring auth or payment,
//! 4. `upsert_relay(url, "nip66")` for the ones not already known, best corroborated first.
//!
//! Only announced monitors count: anyone can publish a 30166, and the crawler connects to what it
//! is given. The `N` (supported NIPs) tags are not used: the crawler needs neither NIP-50 nor
//! NIP-77 from public relays.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use futures::StreamExt;
use serde_json::{Value, json};
use tokio::time::Instant;

use crate::ingest::MAX_FUTURE_SECS;
use crate::model::{RawEvent, is_hex32};
use crate::relay::{CONNECT_TIMEOUT, RelayConn};
use crate::relayurl::normalize_relay;
use crate::store::Store;

pub const KIND_RELAY_DISCOVERY: u16 = 30166;
pub const KIND_MONITOR_ANNOUNCEMENT: u16 = 10166;
/// Source recorded on relays this module inserts.
pub const SOURCE: &str = "nip66";

/// Measured 2026-10-25: relaypag.es, monitorlizard and relay.nostr.watch each hold the same
/// ~12k events from the nostr.watch monitor fleet (relay.nostr.watch is much slower to page);
/// nos.lol and relay.damus.io also carry monitors that publish nowhere else.
pub const DEFAULT_MONITOR_RELAYS: &[&str] = &[
    "wss://relaypag.es",
    "wss://monitorlizard.nostr1.com",
    "wss://relay.nostr.watch",
    "wss://nos.lol",
    "wss://relay.damus.io",
];

/// Most relays answer `limit` > 500 by clamping; purplepag.es CLOSEs the REQ instead.
const PAGE_LIMIT: usize = 500;
/// Monitors in one `authors` filter (58 announced pubkeys were seen in total).
const MAX_MONITORS: usize = 250;
const MAX_ERRORS: usize = 50;
/// Extra time given to a monitor relay past the deadline before its pages are dropped.
const GRACE: Duration = Duration::from_secs(3);

#[derive(Clone, Debug)]
pub struct Nip66Config {
    pub monitor_relays: Vec<String>,
    /// Only 30166 events newer than this count (monitors refresh every 10 min to 24 h).
    pub max_age: Duration,
    /// Pages of 30166 per monitor relay per run (a full week was 26 pages of 500).
    pub max_pages: usize,
    pub page_timeout: Duration,
    /// Wall-clock budget for the whole run.
    pub budget: Duration,
    /// Relays inserted per run at most.
    pub max_new: usize,
}

impl Default for Nip66Config {
    fn default() -> Self {
        Self {
            monitor_relays: DEFAULT_MONITOR_RELAYS
                .iter()
                .map(|s| s.to_string())
                .collect(),
            max_age: Duration::from_secs(7 * 86_400),
            max_pages: 60,
            page_timeout: Duration::from_secs(60),
            budget: Duration::from_secs(300),
            max_new: 2000,
        }
    }
}

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct Nip66Report {
    pub relays_ok: u32,
    /// Announced monitor pubkeys (kind 10166) whose 30166 events were requested.
    pub monitors: u32,
    /// Distinct, fresh, verified 30166 events.
    pub events: u64,
    /// Bad id or signature.
    pub rejected: u64,
    /// Distinct usable relay URLs.
    pub candidates: u64,
    /// Relays some monitor marks as requiring auth or payment.
    pub restricted: u64,
    /// Events for tor/i2p/loki relays, hex-pubkey `d` tags or URLs the normalizer refuses.
    pub skipped: u64,
    /// New relay rows.
    pub inserted: u64,
    pub duration_ms: u64,
    pub errors: Vec<String>,
}

/// Why a 30166 event does not name a relay we can use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Skip {
    NotDiscovery,
    /// No `d` tag, or a hex pubkey (allowed by the spec for relays without a URL).
    NoUrl,
    /// `n` tag other than clearnet, or a .onion/.i2p/.loki host.
    Network,
    /// Rejected by `normalize_relay` (not ws/wss, private address, too long...).
    BadUrl,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    Relay(String),
    /// A clearnet relay that this monitor says requires `auth` or `payment` (R tags).
    Restricted(String),
    Skip(Skip),
}

fn tag_values<'a>(ev: &'a RawEvent, name: &'a str) -> impl Iterator<Item = &'a str> + 'a {
    ev.tags
        .iter()
        .filter(move |t| t.first().map(String::as_str) == Some(name))
        .filter_map(|t| t.get(1))
        .map(|v| v.trim())
}

/// Classify one kind-30166 event (signature not checked here).
pub fn classify(ev: &RawEvent) -> Verdict {
    if ev.kind != KIND_RELAY_DISCOVERY {
        return Verdict::Skip(Skip::NotDiscovery);
    }
    let Some(d) = tag_values(ev, "d").next().filter(|d| !d.is_empty()) else {
        return Verdict::Skip(Skip::NoUrl);
    };
    if is_hex32(&d.to_ascii_lowercase()) {
        return Verdict::Skip(Skip::NoUrl);
    }
    // `n` is optional (one live monitor never sets it); when present it must say clearnet.
    let mut networks = tag_values(ev, "n").peekable();
    if networks.peek().is_some() && !networks.any(|n| n.eq_ignore_ascii_case("clearnet")) {
        return Verdict::Skip(Skip::Network);
    }
    let Some(url) = normalize_relay(d, false) else {
        return Verdict::Skip(Skip::BadUrl);
    };
    let host = url::Url::parse(&url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
        .unwrap_or_default();
    if [".onion", ".i2p", ".loki"]
        .iter()
        .any(|s| host.ends_with(s))
    {
        return Verdict::Skip(Skip::Network);
    }
    // "auth" / "payment" are requirements; "!auth" / "!payment" say the opposite.
    if tag_values(ev, "R")
        .any(|r| r.eq_ignore_ascii_case("auth") || r.eq_ignore_ascii_case("payment"))
    {
        return Verdict::Restricted(url);
    }
    Verdict::Relay(url)
}

#[derive(Debug, Default)]
struct Agg {
    monitors: HashSet<String>,
    newest: i64,
    restricted: bool,
}

/// Usable relay URLs from verified 30166 events, most monitors first, then most recently seen,
/// then by URL. A relay any monitor marks as restricted is dropped.
pub fn candidates(events: &[RawEvent], report: &mut Nip66Report) -> Vec<String> {
    let mut by_url: HashMap<String, Agg> = HashMap::new();
    for ev in events {
        let (url, restricted) = match classify(ev) {
            Verdict::Relay(u) => (u, false),
            Verdict::Restricted(u) => (u, true),
            Verdict::Skip(_) => {
                report.skipped += 1;
                continue;
            }
        };
        let agg = by_url.entry(url).or_default();
        agg.monitors.insert(ev.pubkey.clone());
        agg.newest = agg.newest.max(ev.created_at);
        agg.restricted |= restricted;
    }
    report.restricted = by_url.values().filter(|a| a.restricted).count() as u64;
    let mut out: Vec<(usize, i64, String)> = by_url
        .into_iter()
        .filter(|(_, a)| !a.restricted)
        .map(|(u, a)| (a.monitors.len(), a.newest, u))
        .collect();
    out.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)).then(a.2.cmp(&b.2)));
    report.candidates = out.len() as u64;
    out.into_iter().map(|(_, _, u)| u).collect()
}

/// Fetch, filter and upsert. Errors from single relays end up in the report; only a store
/// failure while listing relays fails the run.
pub async fn discover<S: Store + ?Sized>(
    store: &S,
    cfg: &Nip66Config,
) -> crate::Result<Nip66Report> {
    let started = Instant::now();
    let mut report = Nip66Report::default();
    let events = fetch(cfg, crate::now(), started + cfg.budget, &mut report).await;
    let urls = candidates(&events, &mut report);

    let known: HashSet<String> = store
        .list_relays()
        .await?
        .into_iter()
        .map(|r| r.url)
        .collect();
    for url in urls
        .iter()
        .filter(|u| !known.contains(*u))
        .take(cfg.max_new)
    {
        if let Err(e) = store.upsert_relay(url, SOURCE).await {
            push_error(&mut report, url, &e.to_string());
            break;
        }
        report.inserted += 1;
    }
    report.duration_ms = started.elapsed().as_millis() as u64;
    Ok(report)
}

fn push_error(report: &mut Nip66Report, url: &str, msg: &str) {
    if report.errors.len() < MAX_ERRORS {
        report.errors.push(format!("{url}: {msg}"));
    }
}

struct Monitor {
    url: String,
    conn: Option<RelayConn>,
    events: Vec<RawEvent>,
    error: Option<String>,
}

/// Verified 30166 events from announced monitors, deduplicated across relays.
pub async fn fetch(
    cfg: &Nip66Config,
    now: i64,
    deadline: Instant,
    report: &mut Nip66Report,
) -> Vec<RawEvent> {
    let mut urls: Vec<String> = Vec::new();
    for raw in &cfg.monitor_relays {
        // Configured relays are trusted like seeds (a LAN monitor relay is fine).
        let url = normalize_relay(raw, true).unwrap_or_else(|| raw.trim().to_string());
        if (url.starts_with("ws://") || url.starts_with("wss://")) && !urls.contains(&url) {
            urls.push(url);
        }
    }
    let mut mons: Vec<Monitor> = urls
        .into_iter()
        .map(|url| Monitor {
            url,
            conn: None,
            events: Vec::new(),
            error: None,
        })
        .collect();

    // Phase 1: announcements. A monitor may announce on one relay and publish on another, so
    // the author set is the union over every relay.
    let page_timeout = cfg.page_timeout;
    futures::stream::iter(mons.iter_mut())
        .for_each_concurrent(None, |m| async move {
            let left = deadline.saturating_duration_since(Instant::now());
            let mut conn = match RelayConn::connect(&m.url, left.min(CONNECT_TIMEOUT)).await {
                Ok(c) => c,
                Err(e) => {
                    m.error = Some(e.to_string());
                    return;
                }
            };
            let filter = json!({"kinds": [KIND_MONITOR_ANNOUNCEMENT], "limit": PAGE_LIMIT});
            let left = deadline.saturating_duration_since(Instant::now());
            match conn.fetch_page(filter, left.min(page_timeout)).await {
                Ok(page) => m.events = page.events,
                Err(e) => m.error = Some(e.to_string()),
            }
            m.conn = Some(conn);
        })
        .await;

    let mut announced: HashMap<String, i64> = HashMap::new();
    let mut checked: HashSet<String> = HashSet::new();
    for m in &mut mons {
        for ev in m.events.drain(..) {
            if ev.kind != KIND_MONITOR_ANNOUNCEMENT
                || ev.created_at > now + MAX_FUTURE_SECS
                || !checked.insert(ev.id.clone())
            {
                continue;
            }
            if ev.verify().is_err() {
                report.rejected += 1;
                continue;
            }
            let at = announced.entry(ev.pubkey).or_default();
            *at = (*at).max(ev.created_at);
        }
    }
    // Most recently announced first if there are more than one filter takes.
    let mut authors: Vec<(i64, String)> = announced.into_iter().map(|(p, t)| (t, p)).collect();
    authors.sort_by(|a, b| b.cmp(a));
    let authors: Vec<String> = authors
        .into_iter()
        .take(MAX_MONITORS)
        .map(|(_, p)| p)
        .collect();
    report.monitors = authors.len() as u32;

    // Phase 2: discovery events by those authors, newest first, D8 pagination.
    let since = now - cfg.max_age.as_secs() as i64;
    if !authors.is_empty() {
        let authors = &authors;
        futures::stream::iter(mons.iter_mut())
            .for_each_concurrent(None, |m| async move {
                let Some(conn) = m.conn.as_mut() else {
                    return;
                };
                let run = page_all(conn, authors, since, cfg, deadline);
                match tokio::time::timeout_at(deadline + GRACE, run).await {
                    Ok((events, err)) => {
                        m.events = events;
                        if m.error.is_none() {
                            m.error = err;
                        }
                    }
                    Err(_) => m.error = Some("budget exhausted".into()),
                }
            })
            .await;
    }

    let mut out = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for m in &mut mons {
        if let Some(conn) = m.conn.take() {
            conn.close().await;
            if m.error.is_none() {
                report.relays_ok += 1;
            }
        }
        if let Some(e) = &m.error {
            push_error(report, &m.url, e);
        }
        for ev in m.events.drain(..) {
            if ev.kind != KIND_RELAY_DISCOVERY
                || ev.created_at < since
                || ev.created_at > now + MAX_FUTURE_SECS
                || !authors.contains(&ev.pubkey)
                || !seen.insert(ev.id.clone())
            {
                continue;
            }
            if ev.verify().is_err() {
                report.rejected += 1;
                continue;
            }
            out.push(ev);
        }
    }
    report.events = out.len() as u64;
    out
}

/// Pages until a page brings nothing new, the relay stops answering, or the page cap. Returns
/// what was collected and why it stopped early, if it did.
async fn page_all(
    conn: &mut RelayConn,
    authors: &[String],
    since: i64,
    cfg: &Nip66Config,
    deadline: Instant,
) -> (Vec<RawEvent>, Option<String>) {
    let mut events: Vec<RawEvent> = Vec::new();
    let mut ids: HashSet<String> = HashSet::new();
    let mut until: Option<i64> = None;
    for _ in 0..cfg.max_pages.max(1) {
        let mut filter = json!({
            "kinds": [KIND_RELAY_DISCOVERY],
            "authors": authors,
            "since": since,
            "limit": PAGE_LIMIT,
        });
        if let Some(u) = until {
            filter["until"] = Value::from(u);
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left < Duration::from_secs(1) {
            return (events, Some("budget exhausted".into()));
        }
        let page = match conn.fetch_page(filter, left.min(cfg.page_timeout)).await {
            Ok(p) => p,
            Err(e) => return (events, Some(e.to_string())),
        };
        let mut new = 0usize;
        let mut oldest: Option<i64> = None;
        for ev in page.events {
            oldest = Some(oldest.map_or(ev.created_at, |o| o.min(ev.created_at)));
            if ids.insert(ev.id.clone()) {
                new += 1;
                events.push(ev);
            }
        }
        if let Some(msg) = page.closed {
            return (events, Some(format!("closed: {msg}")));
        }
        if page.timed_out {
            return (events, Some("page timed out".into()));
        }
        match oldest {
            Some(o) if new > 0 && o > since => until = Some(o),
            _ => return (events, None),
        }
    }
    (events, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(tags: &[&[&str]]) -> RawEvent {
        RawEvent {
            id: "00".repeat(32),
            pubkey: "11".repeat(32),
            created_at: 1,
            kind: KIND_RELAY_DISCOVERY,
            tags: tags
                .iter()
                .map(|t| t.iter().map(|s| s.to_string()).collect())
                .collect(),
            content: String::new(),
            sig: String::new(),
        }
    }

    #[test]
    fn classifies_tags() {
        let relay = |s: &str| Verdict::Relay(s.to_string());
        assert_eq!(
            classify(&ev(&[
                &["d", "wss://Relay.Example.com/"],
                &["n", "clearnet"]
            ])),
            relay("wss://relay.example.com")
        );
        // No n tag (as published by one live monitor): the URL decides.
        assert_eq!(
            classify(&ev(&[&["d", "wss://relay.example.com/"]])),
            relay("wss://relay.example.com")
        );
        assert_eq!(
            classify(&ev(&[
                &["d", "wss://relay.example.com/"],
                &["R", "!auth"],
                &["R", "!payment"],
                &["R", "pow"],
                &["R", "writes"],
            ])),
            relay("wss://relay.example.com")
        );
        for r in ["auth", "payment", " Payment "] {
            assert_eq!(
                classify(&ev(&[&["d", "wss://relay.example.com/"], &["R", r]])),
                Verdict::Restricted("wss://relay.example.com".into()),
                "{r}"
            );
        }
        let skip = |s| Verdict::Skip(s);
        assert_eq!(
            classify(&ev(&[&["d", "wss://relay.example.com/"], &["n", "tor"]])),
            skip(Skip::Network)
        );
        assert_eq!(
            classify(&ev(&[&["d", "ws://nostr.jerryhome.i2p/"]])),
            skip(Skip::Network)
        );
        assert_eq!(classify(&ev(&[&["n", "clearnet"]])), skip(Skip::NoUrl));
        assert_eq!(
            classify(&ev(&[&["d", &"ab".repeat(32)]])),
            skip(Skip::NoUrl)
        );
        assert_eq!(
            classify(&ev(&[&["d", "ws://192.168.1.2:7777/"]])),
            skip(Skip::BadUrl)
        );
        assert_eq!(
            classify(&ev(&[&["d", "https://relay.example.com"]])),
            skip(Skip::BadUrl)
        );
        let mut other = ev(&[&["d", "wss://relay.example.com/"]]);
        other.kind = 30_000;
        assert_eq!(classify(&other), skip(Skip::NotDiscovery));
    }

    #[test]
    fn restricted_by_any_monitor_wins_and_ranking() {
        let mut a = ev(&[&["d", "wss://a.example.com/"]]);
        let mut a2 = ev(&[&["d", "wss://a.example.com"], &["R", "auth"]]);
        a2.pubkey = "22".repeat(32);
        let mut b = ev(&[&["d", "wss://b.example.com/"]]);
        b.created_at = 5;
        let c = ev(&[&["d", "wss://c.example.com/"]]);
        let mut c2 = c.clone();
        c2.pubkey = "33".repeat(32);
        let d = ev(&[&["d", "wss://d.example.com/"]]);
        let tor = ev(&[&["d", "ws://x.example.com"], &["n", "tor"]]);
        a.created_at = 9;
        let mut r = Nip66Report::default();
        let got = candidates(&[a, a2, b, c, c2, d, tor], &mut r);
        assert_eq!(
            got,
            vec![
                "wss://c.example.com",
                "wss://b.example.com",
                "wss://d.example.com"
            ]
        );
        assert_eq!((r.candidates, r.restricted, r.skipped), (3, 1, 1));
    }
}

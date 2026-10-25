//! NIP-66 relay discovery (D30, D31, docs/research/nip66.md). Owner: nostr agent.
//!
//! Relay monitors publish one kind-30166 event per (monitor, relay) with the relay URL in `d`,
//! refreshed every check, and announce themselves with kind 10166. One run:
//! 1. kind 10166 from every monitor relay → the set of announced monitor pubkeys,
//! 2. kind 30166 from the last `max_age`, paginated (D8) and bounded: first by the trusted
//!    monitors (own filter and page budget), then by the other announced ones,
//! 3. verify each event once and keep only (monitor, created_at, verdict) per event,
//! 4. `upsert_relay(url, "nip66")` for the ones not already known, trusted-reported first; relays
//!    only open monitors report are capped per run (`max_new_open`).
//!
//! Only trusted or announced monitors count: anyone can publish a 30166, and the crawler connects
//! to what it is given. Anyone can publish a 10166 too, so announced monitors can neither push the
//! trusted ones out of the request nor veto a relay a trusted monitor reports (D31). The `N`
//! (supported NIPs) tags are not used: the crawler needs neither NIP-50 nor NIP-77 from public
//! relays.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
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

/// Every announced monitor with more than 100 kind-30166 events in the last 7 days, measured
/// 2026-10-25 on relaypag.es and nos.lol (docs/research/nip66.md §3): 58faef87, the nostr.watch
/// fleet (0bffd9b5 and the 9b… keys), 45df0580, 9bbbb845, and 19f3383d (nos.lol, damus, primal
/// only). Their 10166 announcements are 80–130 days old (19f3383d's is fresh).
pub const DEFAULT_TRUSTED_MONITORS: &[&str] = &[
    "58faef87b34208c74ed36008eb3621a2a91f360c075c83b88e5348ea949df2d7",
    "0bffd9b5522abc0dddf9ddf874caef3926d1f36b2380ca053802dce31a5dafa7",
    "9bb7cd94d7b688a4070205d9fb5e9cca6bd781fe7cabe780e19fdd23a036e0a1",
    "9ba0ce3dcc28c26da0d0d87fa460c78b602a180b61eb70b62aba04505c6331f4",
    "9ba6484003e8e88600f97ebffd897b2fe82753082e8e0cd8ea19aac0ff2b712b",
    "9ba1d7892cd057f5aca5d629a5a601f64bc3e0f1fc6ed9c939845e25d5e1e254",
    "9ba046db56b8e6682c48af8d6425ffe80430a3cd0854d95381af27c5d27ca0f7",
    "9bac3d58ef5a34c7c4a9b05b07c98e4afc56655542387b4d36c9d270f898592e",
    "9b85d54cc4bc886d60782f80d676e41bc637ed3ecc73d2bb5aabadc499d6a340",
    "45df0580711f37c547270480d7aed2c7fc03ba5a4f8fef5a8787db0b19343de0",
    "9bbbb845e5b6c831c29789900769843ab43bb5047abe697870cb50b6fc9bf923",
    "19f3383da2a3022e76d87ee5f743baf6ad8d5f7add835e3b9bd9e7629f61a3e4",
];

/// Most relays answer `limit` > 500 by clamping; purplepag.es CLOSEs the REQ instead.
const PAGE_LIMIT: usize = 500;
/// Open (announced, untrusted) monitors in one `authors` filter (58 announced pubkeys were seen
/// in total).
const MAX_MONITORS: usize = 250;
const MAX_ERRORS: usize = 50;
/// Extra time given to a monitor relay past the deadline before its pages are dropped.
const GRACE: Duration = Duration::from_secs(3);

#[derive(Clone, Debug)]
pub struct Nip66Config {
    pub monitor_relays: Vec<String>,
    /// Hex pubkeys queried in their own filter whether or not they announce (D31).
    pub trusted_monitors: Vec<String>,
    /// Only 30166 events newer than this count (monitors refresh every 10 min to 24 h).
    pub max_age: Duration,
    /// Pages of trusted 30166 per monitor relay per run (a full week was 26 pages of 500).
    pub max_pages: usize,
    /// Pages of open-monitor 30166 per monitor relay per run (they had 60 events in a week).
    pub max_open_pages: usize,
    pub page_timeout: Duration,
    /// Wall-clock budget for the whole run.
    pub budget: Duration,
    /// Relays inserted per run at most.
    pub max_new: usize,
    /// Of those, relays that no trusted monitor reports.
    pub max_new_open: usize,
}

impl Default for Nip66Config {
    fn default() -> Self {
        let owned = |l: &[&str]| l.iter().map(|s| s.to_string()).collect();
        Self {
            monitor_relays: owned(DEFAULT_MONITOR_RELAYS),
            trusted_monitors: owned(DEFAULT_TRUSTED_MONITORS),
            max_age: Duration::from_secs(7 * 86_400),
            max_pages: 60,
            max_open_pages: 10,
            page_timeout: Duration::from_secs(60),
            budget: Duration::from_secs(300),
            max_new: 2000,
            max_new_open: 100,
        }
    }
}

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct Nip66Report {
    pub relays_ok: u32,
    /// Monitor pubkeys whose 30166 events were requested (trusted and announced).
    pub monitors: u32,
    /// Trusted monitors with at least one usable event (0 means the trusted list went stale).
    pub trusted: u32,
    /// Distinct, fresh, verified 30166 events.
    pub events: u64,
    /// Bad id or signature.
    pub rejected: u64,
    /// Distinct usable relay URLs.
    pub candidates: u64,
    /// Relays dropped because a monitor marks them as requiring auth or payment.
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

/// What one verified 30166 event says, without its content or tags: `fetch` keeps only this, so
/// a run holds no event bodies (a 30166 may carry a NIP-11 document of up to the relay's maximum
/// event size).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Observation {
    pub monitor: String,
    /// The monitor is in `trusted_monitors`.
    pub trusted: bool,
    pub created_at: i64,
    pub verdict: Verdict,
}

impl Observation {
    pub fn of(ev: &RawEvent, trusted: bool) -> Self {
        Self {
            monitor: ev.pubkey.clone(),
            trusted,
            created_at: ev.created_at,
            verdict: classify(ev),
        }
    }
}

/// A usable relay URL and whether a trusted monitor reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub url: String,
    pub trusted: bool,
}

#[derive(Debug, Default)]
struct Agg {
    trusted: HashSet<String>,
    open: HashSet<String>,
    newest: i64,
    restricted_trusted: bool,
    restricted_open: bool,
}

impl Agg {
    /// Open monitors only decide for relays no trusted monitor reports: otherwise one fresh 10166
    /// and one `R auth` would be enough to hide any relay.
    fn restricted(&self) -> bool {
        self.restricted_trusted || (self.trusted.is_empty() && self.restricted_open)
    }
}

/// Usable relays from 30166 observations: most trusted monitors first, then most open monitors,
/// then most recently seen, then by URL. A relay marked restricted is dropped (see `Agg`).
pub fn candidates(obs: &[Observation], report: &mut Nip66Report) -> Vec<Candidate> {
    let mut by_url: HashMap<&str, Agg> = HashMap::new();
    for o in obs {
        let (url, restricted) = match &o.verdict {
            Verdict::Relay(u) => (u, false),
            Verdict::Restricted(u) => (u, true),
            Verdict::Skip(_) => {
                report.skipped += 1;
                continue;
            }
        };
        let agg = by_url.entry(url).or_default();
        if o.trusted {
            agg.trusted.insert(o.monitor.clone());
            agg.restricted_trusted |= restricted;
        } else {
            agg.open.insert(o.monitor.clone());
            agg.restricted_open |= restricted;
        }
        agg.newest = agg.newest.max(o.created_at);
    }
    report.restricted = by_url.values().filter(|a| a.restricted()).count() as u64;
    let mut out: Vec<(usize, usize, i64, &str)> = by_url
        .into_iter()
        .filter(|(_, a)| !a.restricted())
        .map(|(u, a)| (a.trusted.len(), a.open.len(), a.newest, u))
        .collect();
    out.sort_by(|a, b| (b.0, b.1, b.2).cmp(&(a.0, a.1, a.2)).then(a.3.cmp(b.3)));
    report.candidates = out.len() as u64;
    out.into_iter()
        .map(|(t, _, _, u)| Candidate {
            url: u.to_string(),
            trusted: t > 0,
        })
        .collect()
}

/// Fetch, filter and upsert. Errors from single relays end up in the report; only a store
/// failure while listing relays fails the run.
pub async fn discover<S: Store + ?Sized>(
    store: &S,
    cfg: &Nip66Config,
) -> crate::Result<Nip66Report> {
    let started = Instant::now();
    let mut report = Nip66Report::default();
    let obs = fetch(cfg, crate::now(), started + cfg.budget, &mut report).await;
    let found = candidates(&obs, &mut report);
    drop(obs);

    let known: HashSet<String> = store
        .list_relays()
        .await?
        .into_iter()
        .map(|r| r.url)
        .collect();
    let mut open = 0usize;
    // Trusted-reported relays sort first, so the open cap never displaces them.
    let fresh = found
        .iter()
        .filter(|c| !known.contains(&c.url))
        .filter(|c| {
            if c.trusted {
                return true;
            }
            open += 1;
            open <= cfg.max_new_open
        });
    for c in fresh.take(cfg.max_new) {
        if let Err(e) = store.upsert_relay(&c.url, SOURCE).await {
            push_error(&mut report, &c.url, &e.to_string());
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
    announcements: Vec<RawEvent>,
    obs: Vec<Observation>,
    rejected: u64,
    error: Option<String>,
}

/// One `authors` filter paged on one relay.
struct Pass<'a> {
    authors: &'a [String],
    trusted: bool,
    max_pages: usize,
}

/// Verified 30166 observations from trusted and announced monitors, deduplicated across relays.
pub async fn fetch(
    cfg: &Nip66Config,
    now: i64,
    deadline: Instant,
    report: &mut Nip66Report,
) -> Vec<Observation> {
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
            announcements: Vec::new(),
            obs: Vec::new(),
            rejected: 0,
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
                Ok(page) => m.announcements = page.events,
                Err(e) => m.error = Some(e.to_string()),
            }
            m.conn = Some(conn);
        })
        .await;

    let mut trusted: Vec<String> = cfg
        .trusted_monitors
        .iter()
        .map(|p| p.trim().to_ascii_lowercase())
        .filter(|p| is_hex32(p))
        .collect();
    trusted.sort();
    trusted.dedup();

    // pubkey → (monitor relays carrying its announcement, newest announcement).
    let mut announced: HashMap<String, (HashSet<usize>, i64)> = HashMap::new();
    let mut checked: HashMap<(String, String), bool> = HashMap::new();
    for (i, m) in mons.iter_mut().enumerate() {
        for ev in m.announcements.drain(..) {
            if ev.kind != KIND_MONITOR_ANNOUNCEMENT || ev.created_at > now + MAX_FUTURE_SECS {
                continue;
            }
            let ok = *checked
                .entry((ev.id.clone(), ev.sig.clone()))
                .or_insert_with(|| {
                    let ok = ev.verify().is_ok();
                    report.rejected += u64::from(!ok);
                    ok
                });
            if ok && trusted.binary_search(&ev.pubkey).is_err() {
                let (relays, at) = announced.entry(ev.pubkey).or_default();
                relays.insert(i);
                *at = (*at).max(ev.created_at);
            }
        }
    }
    // Any order here can be gamed (created_at is the author's claim); announcements carried by
    // more monitor relays first at least costs a spammer more relays.
    let mut open: Vec<(usize, i64, String)> = announced
        .into_iter()
        .map(|(p, (r, t))| (r.len(), t, p))
        .collect();
    open.sort_by(|a, b| b.cmp(a));
    let open: Vec<String> = open
        .into_iter()
        .take(MAX_MONITORS)
        .map(|(_, _, p)| p)
        .collect();
    report.monitors = (trusted.len() + open.len()) as u32;

    // Phase 2: discovery events, trusted monitors first with their own page budget so that open
    // monitors cannot use it up, newest first, D8 pagination.
    let since = now - cfg.max_age.as_secs() as i64;
    let passes = [
        Pass {
            authors: &trusted,
            trusted: true,
            max_pages: cfg.max_pages,
        },
        Pass {
            authors: &open,
            trusted: false,
            max_pages: cfg.max_open_pages,
        },
    ];
    let window = Window {
        since,
        now,
        page_timeout,
        deadline,
    };
    let accepted: Mutex<HashSet<String>> = Mutex::new(HashSet::new());
    let (passes, accepted, window) = (&passes, &accepted, &window);
    futures::stream::iter(mons.iter_mut())
        .for_each_concurrent(None, |m| async move {
            let Some(conn) = m.conn.as_mut() else {
                return;
            };
            let (obs, rejected) = (&mut m.obs, &mut m.rejected);
            let run = async {
                for pass in passes.iter().filter(|p| !p.authors.is_empty()) {
                    page_all(conn, pass, window, accepted, obs, rejected).await?;
                }
                Ok::<(), String>(())
            };
            let err = match tokio::time::timeout_at(deadline + GRACE, run).await {
                Ok(r) => r.err(),
                Err(_) => Some("budget exhausted".into()),
            };
            if m.error.is_none() {
                m.error = err;
            }
        })
        .await;

    let mut out = Vec::new();
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
        report.rejected += m.rejected;
        out.append(&mut m.obs);
    }
    report.events = out.len() as u64;
    report.trusted = out
        .iter()
        .filter(|o| o.trusted)
        .map(|o| o.monitor.as_str())
        .collect::<HashSet<_>>()
        .len() as u32;
    out
}

struct Window {
    since: i64,
    now: i64,
    page_timeout: Duration,
    deadline: Instant,
}

/// Pages one filter until a page brings nothing new, the relay stops answering, or the page cap.
/// Each new event is checked, verified once across all relays (`accepted`) and reduced to an
/// `Observation` before the next page; only ids are kept. Returns why it stopped early, if it did.
async fn page_all(
    conn: &mut RelayConn,
    pass: &Pass<'_>,
    w: &Window,
    accepted: &Mutex<HashSet<String>>,
    out: &mut Vec<Observation>,
    rejected: &mut u64,
) -> Result<(), String> {
    let authors: HashSet<&str> = pass.authors.iter().map(String::as_str).collect();
    let mut ids: HashSet<String> = HashSet::new();
    let mut until: Option<i64> = None;
    for _ in 0..pass.max_pages.max(1) {
        let mut filter = json!({
            "kinds": [KIND_RELAY_DISCOVERY],
            "authors": pass.authors,
            "since": w.since,
            "limit": PAGE_LIMIT,
        });
        if let Some(u) = until {
            filter["until"] = Value::from(u);
        }
        let left = w.deadline.saturating_duration_since(Instant::now());
        if left < Duration::from_secs(1) {
            return Err("budget exhausted".into());
        }
        let page = conn
            .fetch_page(filter, left.min(w.page_timeout))
            .await
            .map_err(|e| e.to_string())?;
        let mut new = 0usize;
        let mut oldest: Option<i64> = None;
        for ev in page.events {
            oldest = Some(oldest.map_or(ev.created_at, |o| o.min(ev.created_at)));
            if !ids.insert(ev.id.clone()) {
                continue;
            }
            new += 1;
            if ev.kind != KIND_RELAY_DISCOVERY
                || ev.created_at < w.since
                || ev.created_at > w.now + MAX_FUTURE_SECS
                || !authors.contains(ev.pubkey.as_str())
                || accepted.lock().unwrap().contains(&ev.id)
            {
                continue;
            }
            // Outside the lock; a copy with a bad signature does not shadow a good one.
            if ev.verify().is_err() {
                *rejected += 1;
                continue;
            }
            if accepted.lock().unwrap().insert(ev.id.clone()) {
                out.push(Observation::of(&ev, pass.trusted));
            }
        }
        if let Some(msg) = page.closed {
            return Err(format!("closed: {msg}"));
        }
        if page.timed_out {
            return Err("page timed out".into());
        }
        match oldest {
            Some(o) if new > 0 && o > w.since => until = Some(o),
            _ => return Ok(()),
        }
    }
    Ok(())
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

    fn open(evs: &[RawEvent]) -> Vec<Observation> {
        evs.iter().map(|e| Observation::of(e, false)).collect()
    }

    fn urls(c: Vec<Candidate>) -> Vec<String> {
        c.into_iter().map(|c| c.url).collect()
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
        let got = urls(candidates(&open(&[a, a2, b, c, c2, d, tor]), &mut r));
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

    #[test]
    fn trusted_monitors_rank_first_and_cannot_be_vetoed() {
        let sybil = |i: u8, url: &str, extra: &[&[&str]]| {
            let d = ["d", url];
            let mut tags: Vec<&[&str]> = vec![&d];
            tags.extend_from_slice(extra);
            let mut e = ev(&tags);
            e.pubkey = format!("{i:02x}").repeat(32);
            e.created_at = 100;
            Observation::of(&e, false)
        };
        let trusted = |url: &str, extra: &[&[&str]]| {
            let d = ["d", url];
            let mut tags: Vec<&[&str]> = vec![&d];
            tags.extend_from_slice(extra);
            Observation::of(&ev(&tags), true)
        };
        let obs = vec![
            // Five open monitors agree on a fresh junk relay...
            sybil(0xa1, "wss://junk.example.com", &[]),
            sybil(0xa2, "wss://junk.example.com", &[]),
            sybil(0xa3, "wss://junk.example.com", &[]),
            sybil(0xa4, "wss://junk.example.com", &[]),
            sybil(0xa5, "wss://junk.example.com", &[]),
            // ...and try to hide a relay a trusted monitor reports.
            trusted("wss://real.example.com", &[&["R", "!auth"]]),
            sybil(0xa1, "wss://real.example.com", &[&["R", "auth"]]),
            // A trusted restriction still counts, and so does an open one on an open-only relay.
            trusted("wss://paid.example.com", &[&["R", "payment"]]),
            sybil(0xa1, "wss://open-paid.example.com", &[&["R", "auth"]]),
            sybil(0xa2, "wss://open-paid.example.com", &[]),
        ];
        let mut r = Nip66Report::default();
        let got = candidates(&obs, &mut r);
        assert_eq!(
            got,
            vec![
                Candidate {
                    url: "wss://real.example.com".into(),
                    trusted: true
                },
                Candidate {
                    url: "wss://junk.example.com".into(),
                    trusted: false
                },
            ]
        );
        assert_eq!((r.candidates, r.restricted), (2, 2));
    }
}

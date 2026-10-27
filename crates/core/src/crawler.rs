//! Crawler (D8–D10). Owner: nostr agent. Generic over Store; used by server daemon and phone.
//!
//! One run: pick relays (seeds ∪ stored ∪ NIP-65 write relays of publishers), then
//! 1. forward sweeps from each (relay, kind) cursor for every relay, so fresh events win the budget,
//! 2. bounded backfill of unfinished history plus comments for the torrents this run inserted,
//! 3. optionally metadata (0/3/10000/10002/1984/30007) for the trust graph from the indexers.
//!
//! Pagination (D8): `until = min(created_at)` of the previous page, inclusive, deduplicated by id and
//! strictly decreasing; a page with no new event ends a sweep. Cursors (D9): `newest_seen` is only
//! advanced when a forward sweep reached its `since` bound, so an interrupted sweep leaves no gap;
//! `backfill_until` is persisted after every page.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::time::Duration;

use futures::StreamExt;
use serde_json::{Map, Value, json};
use tokio::time::Instant;

use crate::ingest::{IngestOutcome, ingest_into};
use crate::model::*;
use crate::relay::{CONNECT_TIMEOUT, RelayConn};
use crate::relayurl::normalize_relay;
use crate::store::{Store, keys};

pub const DEFAULT_SEED_RELAYS: &[&str] = &[
    "wss://nostr.oxtr.dev",
    "wss://relay.nostr.net",
    "wss://nos.lol",
    "wss://relay.primal.net",
    "wss://relay.damus.io",
    "wss://relay.snort.social",
    "wss://nostr.mom",
    "wss://nostr21.com",
    "wss://nostr.wine",
    "wss://nostr.land",
    "wss://offchain.pub",
    "wss://nostr.bitcoiner.social",
];
pub const DEFAULT_INDEXERS: &[&str] = &[
    "wss://purplepag.es",
    "wss://user.kindpag.es",
    "wss://indexer.coracle.social",
];

/// Kinds fetched for the trust graph.
pub const METADATA_KINDS: &[u16] = &[0, 3, 10000, 10002, 1984, 30007];
/// Max ids / authors in one filter.
pub const COMMENT_BATCH: usize = 100;
pub const AUTHOR_BATCH: usize = 250;
/// Publishers whose NIP-65 write relays are considered, and whose metadata is refreshed.
const TOP_PUBLISHERS: usize = 500;
const PAGE_TIMEOUT: Duration = Duration::from_secs(20);
/// Forward sweeps are bounded by `since`; this only guards against a relay ignoring it.
const MAX_FORWARD_PAGES: usize = 1000;
/// Pages per targeted query (comments of new torrents, metadata batch).
const MAX_TARGET_PAGES: usize = 5;
/// Extra time given to a relay task past the deadline before it is dropped.
const GRACE: Duration = Duration::from_secs(3);
const CLOSED_RETRY_DELAY: Duration = Duration::from_secs(1);
/// CLOSED prefixes that refuse us rather than one filter (NIP-01 `blocked`, `rate-limited`,
/// `restricted`; NIP-42 `auth-required`). Asking again soon only adds traffic the relay already
/// declined, so such a visit backs off like a failure (D34). `error:`/`invalid:`/`unsupported:`
/// are about the filter (strfry rejects unknown filter items that way) and stay soft.
const REFUSALS: &[&str] = &["blocked", "rate-limited", "restricted", "auth-required"];
const MAX_ERRORS: usize = 50;
const MAX_HINTS: usize = 500;

#[derive(Clone, Debug)]
pub struct CrawlConfig {
    pub seed_relays: Vec<String>,
    pub indexers: Vec<String>,
    /// Max relays touched per run (server 30, phone 8).
    pub max_relays: usize,
    /// Slots of `max_relays` kept out of the top yielders and rotated through every other relay
    /// (never tried first, then least recently looked at), so discovered relays and lesser
    /// yielders get crawled instead of starving behind the top ones. Clamped to `max_relays`; see
    /// `select_relays` (D32, D33).
    pub explore_relays: usize,
    /// Wall-clock budget for one run.
    pub budget: Duration,
    /// Max pages per relay per run for backfill.
    pub max_backfill_pages: usize,
    pub page_limit: usize,
    pub overlap_secs: i64,
    /// Relays crawled concurrently.
    pub concurrency: usize,
    /// Also fetch kinds 0/3/10000/10002/1984/30007 for root + D1 + publishers from indexers.
    pub fetch_metadata: bool,
    /// Trust root (hex) whose follow graph is fetched when fetch_metadata.
    pub root: Option<String>,
}

impl CrawlConfig {
    pub fn server_default() -> Self {
        Self {
            seed_relays: DEFAULT_SEED_RELAYS.iter().map(|s| s.to_string()).collect(),
            indexers: DEFAULT_INDEXERS.iter().map(|s| s.to_string()).collect(),
            max_relays: 30,
            explore_relays: 10,
            budget: Duration::from_secs(600),
            max_backfill_pages: 200,
            page_limit: 500,
            overlap_secs: 3600,
            concurrency: 6,
            fetch_metadata: true,
            root: None,
        }
    }
    pub fn phone_default() -> Self {
        Self {
            max_relays: 8,
            explore_relays: 2,
            budget: Duration::from_secs(120),
            max_backfill_pages: 10,
            concurrency: 3,
            ..Self::server_default()
        }
    }
}

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct CrawlReport {
    pub relays_tried: u32,
    pub relays_ok: u32,
    pub events_seen: u64,
    pub inserted: u64,
    pub duplicates: u64,
    pub rejected: u64,
    pub comments: u64,
    pub metadata_events: u64,
    pub duration_ms: u64,
    pub errors: Vec<String>,
}

/// One crawl pass: relay discovery, forward sweep from cursors, bounded backfill, comments for new
/// torrents, optional metadata. Must respect `budget` and never panic on relay misbehaviour.
pub async fn crawl_once<S: Store + ?Sized>(
    store: &S,
    cfg: &CrawlConfig,
) -> crate::Result<CrawlReport> {
    let started = Instant::now();
    let deadline = started + cfg.budget;
    // Metadata gets the last quarter of the budget (it only feeds trust, events come first).
    let crawl_deadline = if cfg.fetch_metadata && !cfg.indexers.is_empty() {
        started + cfg.budget.mul_f64(0.75)
    } else {
        deadline
    };
    let now = crate::now();
    let ctx = Ctx {
        store,
        cfg,
        now,
        report: Mutex::new(CrawlReport::default()),
        hints: Mutex::new(HashMap::new()),
    };

    let urls = select_relays(store, cfg, now).await?;
    let mut runs: Vec<RelayRun> = urls.into_iter().map(RelayRun::new).collect();
    let concurrency = cfg.concurrency.max(1);

    // Phase 1: connect + forward sweeps everywhere.
    futures::stream::iter(runs.iter_mut())
        .for_each_concurrent(concurrency, |run| {
            let ctx = &ctx;
            async move {
                let phase = ctx.forward_phase(run, crawl_deadline);
                if tokio::time::timeout_at(crawl_deadline + GRACE, phase)
                    .await
                    .is_err()
                {
                    run.conn = None;
                }
            }
        })
        .await;

    // Phase 2: backfill + comments of the torrents inserted from each relay.
    futures::stream::iter(runs.iter_mut())
        .for_each_concurrent(concurrency, |run| {
            let ctx = &ctx;
            async move {
                let phase = ctx.backfill_phase(run, crawl_deadline);
                if tokio::time::timeout_at(crawl_deadline + GRACE, phase)
                    .await
                    .is_err()
                {
                    run.conn = None;
                }
                if let Some(conn) = run.conn.take() {
                    conn.close().await;
                }
            }
        })
        .await;

    for run in runs.iter().filter(|r| r.attempted) {
        let ok = run.ok();
        {
            let mut r = ctx.report.lock().unwrap();
            r.relays_tried += 1;
            r.relays_ok += u32::from(ok);
        }
        let err = run
            .fatal
            .as_deref()
            .or(run.refused.as_deref())
            .or(run.note.as_deref());
        if let Err(e) = store
            .record_relay_result(&run.url, ok, if ok { None } else { err }, run.found, now)
            .await
        {
            ctx.error(&run.url, &e.to_string());
        }
        if run.torrents > 0
            && let Err(e) = store.record_relay_torrents(&run.url, run.torrents).await
        {
            ctx.error(&run.url, &e.to_string());
        }
    }

    if cfg.fetch_metadata && !cfg.indexers.is_empty() && Instant::now() < deadline {
        ctx.metadata_phase(deadline).await;
    }

    // Relay hints and legacy kind-3 relay maps seen in ingested events.
    let hints: Vec<(String, &'static str)> = ctx.hints.lock().unwrap().drain().collect();
    for (url, source) in hints {
        if let Err(e) = store.upsert_relay(&url, source).await {
            ctx.error(&url, &e.to_string());
            break;
        }
    }

    let mut report = ctx.report.into_inner().unwrap();
    report.duration_ms = started.elapsed().as_millis() as u64;
    Ok(report)
}

/// The relays of one run (D32): at most `max_relays`, no duplicates, disabled or backed-off relays
/// skipped. Candidates are the seeds, every stored relay and the NIP-65 write relays of the top
/// publishers (stored as "nip65" on the way).
/// 1. Up to `max_relays - explore_relays` proven relays (`events_found > 0`), ranked by past yield,
///    then by how many publishers list them as write relays.
/// 2. Every other slot (so at least `explore_relays`) rotates through all remaining candidates,
///    zero-yield relays and the proven ones below the cutoff alike, least recently looked at
///    first: `next_try_at` is the last ok run for a reachable relay, the end of its backoff for a
///    failing one, and 0 for one never tried. Ties: higher yield, seeds in config order, then
///    most listed. Fresh installs therefore start with the seeds, newly discovered relays go
///    before anything already crawled, and neither seeds past the cap nor proven relays outside
///    the top yielders are cut forever (D33).
pub async fn select_relays<S: Store + ?Sized>(
    store: &S,
    cfg: &CrawlConfig,
    now: i64,
) -> crate::Result<Vec<String>> {
    let infos: HashMap<String, RelayInfo> = store
        .list_relays()
        .await?
        .into_iter()
        .map(|r| (r.url.clone(), r))
        .collect();

    // Seed URL -> config position.
    let mut seeds: HashMap<String, usize> = HashMap::new();
    for seed in &cfg.seed_relays {
        // Seeds are trusted config: keep them even when the public-relay normalizer would not
        // (e.g. a LAN or loopback relay).
        let url = normalize_relay(seed, true).unwrap_or_else(|| seed.trim().to_string());
        if !(url.starts_with("ws://") || url.starts_with("wss://")) || seeds.contains_key(&url) {
            continue;
        }
        store.upsert_relay(&url, "seed").await?;
        let pos = seeds.len();
        seeds.insert(url, pos);
    }

    // NIP-65 write relays of known publishers (oversized lists are already excluded by the store).
    let publishers = store.publishers(TOP_PUBLISHERS).await?;
    let mut freq: HashMap<String, u32> = HashMap::new();
    if !publishers.is_empty() {
        for (_, url, _, write) in store.relay_lists_for(&publishers).await? {
            if let Some(url) = normalize_relay(&url, false).filter(|_| write) {
                *freq.entry(url).or_default() += 1;
            }
        }
    }
    for url in freq.keys() {
        if !infos.contains_key(url) && !seeds.contains_key(url) {
            store.upsert_relay(url, "nip65").await?;
        }
    }

    // (events_found, next_try_at, seed position, publishers listing it, url)
    let mut cands: Vec<(u64, i64, usize, u32, String)> = Vec::new();
    let mut seen: HashSet<&str> = HashSet::new();
    for url in seeds.keys().chain(freq.keys()).chain(infos.keys()) {
        if !seen.insert(url.as_str()) {
            continue;
        }
        let info = infos.get(url);
        if info.is_some_and(|i| !i.enabled || i.next_try_at > now) {
            continue;
        }
        // Rows learned from events before the normalizer rejected them (e.g. a LAN host).
        let trusted = seeds.contains_key(url)
            || info.is_some_and(|i| matches!(i.source.as_str(), "seed" | "manual"));
        if !trusted && normalize_relay(url, false).is_none() {
            continue;
        }
        cands.push((
            info.map_or(0, |i| i.events_found),
            info.map_or(0, |i| i.next_try_at),
            seeds.get(url).copied().unwrap_or(usize::MAX),
            freq.get(url).copied().unwrap_or(0),
            url.clone(),
        ));
    }
    let (mut proven, mut rotation): (Vec<_>, Vec<_>) = cands.into_iter().partition(|c| c.0 > 0);
    proven.sort_by(|a, b| b.0.cmp(&a.0).then(b.3.cmp(&a.3)).then(a.4.cmp(&b.4)));
    let exploit = cfg.max_relays - cfg.explore_relays.min(cfg.max_relays);
    // The proven tail rotates with the zero-yield relays: lifetime yield only grows when a relay
    // is crawled, so a relay left below the cutoff could never climb back (D33).
    let tail = proven.split_off(exploit.min(proven.len()));
    rotation.extend(tail);
    rotation.sort_by(|a, b| {
        a.1.cmp(&b.1)
            .then(b.0.cmp(&a.0))
            .then(a.2.cmp(&b.2))
            .then(b.3.cmp(&a.3))
            .then(a.4.cmp(&b.4))
    });

    let mut out: Vec<String> = proven.into_iter().chain(rotation).map(|c| c.4).collect();
    out.truncate(cfg.max_relays);
    Ok(out)
}

struct RelayRun {
    url: String,
    conn: Option<RelayConn>,
    attempted: bool,
    connected: bool,
    /// Error that made the relay unusable this run (connect failure, protocol error).
    fatal: Option<String>,
    /// Non-fatal trouble (CLOSED, timeouts).
    note: Option<String>,
    /// The relay refused us (a `REFUSALS` CLOSED): no more REQs this run, and backoff.
    refused: Option<String>,
    /// A page went unanswered for the full `PAGE_TIMEOUT`.
    timed_out: bool,
    found: u64,
    /// Of `found`, new torrent events (kind 2003): `relays.torrents_found` (D102).
    torrents: u64,
    new_torrents: Vec<String>,
}

impl RelayRun {
    fn new(url: String) -> Self {
        Self {
            url,
            conn: None,
            attempted: false,
            connected: false,
            fatal: None,
            note: None,
            refused: None,
            timed_out: false,
            found: 0,
            torrents: 0,
            new_torrents: Vec::new(),
        }
    }

    /// Whether this visit resets the relay's backoff. Besides connect and protocol failures, a
    /// refusal and a timeout that brought nothing new count as failures: otherwise continuous
    /// mode would revisit a relay that keeps saying no every pass (D34).
    fn ok(&self) -> bool {
        self.connected
            && self.fatal.is_none()
            && self.refused.is_none()
            && !(self.timed_out && self.found == 0)
    }
}

/// True for a `closed: <prefix>: …` stop reason whose prefix is one of `REFUSALS`. Prefixes are
/// matched case-insensitively (strfry sends `ERROR:` upper-case, nostr-nips.md §1.5).
fn is_refusal(reason: &str) -> bool {
    reason
        .strip_prefix("closed: ")
        .and_then(|m| m.split_once(':'))
        .is_some_and(|(p, _)| REFUSALS.contains(&p.trim().to_ascii_lowercase().as_str()))
}

/// The swept streams per relay: (cursor kind, base filter).
fn sweeps() -> [(u16, Value); 3] {
    [
        (KIND_TORRENT, json!({"kinds": [KIND_TORRENT]})),
        (
            KIND_TORRENT_COMMENT,
            json!({"kinds": [KIND_TORRENT_COMMENT]}),
        ),
        (
            KIND_COMMENT,
            json!({"kinds": [KIND_COMMENT], "#K": ["2003"]}),
        ),
    ]
}

struct Ctx<'a, S: Store + ?Sized> {
    store: &'a S,
    cfg: &'a CrawlConfig,
    now: i64,
    report: Mutex<CrawlReport>,
    hints: Mutex<HashMap<String, &'static str>>,
}

/// Why a sweep ended early.
enum Halt {
    /// Budget, page cap, CLOSED or timeout: the relay is still usable later.
    Soft(String),
    /// Connection or store failure.
    Hard(String),
}

impl<'a, S: Store + ?Sized> Ctx<'a, S> {
    fn error(&self, url: &str, msg: &str) {
        let mut r = self.report.lock().unwrap();
        if r.errors.len() < MAX_ERRORS {
            r.errors.push(format!("{url}: {msg}"));
        }
    }

    async fn forward_phase(&self, run: &mut RelayRun, deadline: Instant) {
        let left = deadline.saturating_duration_since(Instant::now());
        if left < Duration::from_secs(1) {
            return;
        }
        run.attempted = true;
        match RelayConn::connect(&run.url, left.min(CONNECT_TIMEOUT)).await {
            Ok(c) => {
                run.conn = Some(c);
                run.connected = true;
            }
            Err(e) => {
                let msg = e.to_string();
                self.error(&run.url, &msg);
                run.fatal = Some(msg);
                return;
            }
        }
        for (kind, base) in sweeps() {
            if let Err(h) = self.forward_sweep(run, kind, base, deadline).await
                && self.halt(run, h)
            {
                return;
            }
            if kind == KIND_TORRENT
                && let Err(h) = self.comments_of_new(run, deadline, true).await
                && self.halt(run, h)
            {
                return;
            }
        }
    }

    async fn backfill_phase(&self, run: &mut RelayRun, deadline: Instant) {
        if run.conn.is_none() {
            return;
        }
        if let Err(h) = self
            .backfill(run, KIND_TORRENT, sweeps()[0].1.clone(), deadline)
            .await
            && self.halt(run, h)
        {
            return;
        }
        if let Err(h) = self.comments_of_new(run, deadline, true).await
            && self.halt(run, h)
        {
            return;
        }
        // Comment history only once this relay's torrent history is complete: earlier, most of
        // its comments would be rejected as orphans and the cursor would move past them for good.
        match self.cursor(&run.url, KIND_TORRENT).await {
            Ok(c) if c.backfill_done => {}
            Ok(_) => return,
            Err(h) => {
                self.halt(run, h);
                return;
            }
        }
        for (kind, base) in sweeps().into_iter().skip(1) {
            if let Err(h) = self.backfill(run, kind, base, deadline).await
                && self.halt(run, h)
            {
                return;
            }
        }
    }

    /// Record why a sweep stopped; true if nothing more should be asked of this relay.
    fn halt(&self, run: &mut RelayRun, h: Halt) -> bool {
        match h {
            Halt::Soft(msg) => {
                tracing::debug!(relay = %run.url, %msg, "sweep stopped");
                if msg == "budget" {
                    return true;
                }
                if is_refusal(&msg) {
                    self.error(&run.url, &msg);
                    run.refused = Some(msg);
                    run.conn = None;
                    return true;
                }
                run.timed_out |= msg == "timeout";
                run.note = Some(msg);
                false
            }
            Halt::Hard(msg) => {
                self.error(&run.url, &msg);
                // A store failure is not the relay's fault: no backoff for it.
                if msg.starts_with("store:") {
                    run.note = Some(msg);
                } else {
                    run.fatal = Some(msg);
                }
                run.conn = None;
                true
            }
        }
    }

    async fn cursor(&self, url: &str, kind: u16) -> Result<Cursor, Halt> {
        Ok(self
            .store
            .get_cursor(url, kind)
            .await
            .map_err(|e| Halt::Hard(format!("store: {e}")))?
            .unwrap_or(Cursor {
                relay: url.to_string(),
                kind,
                newest_seen: None,
                backfill_until: None,
                backfill_done: false,
            }))
    }

    async fn save_cursor(&self, c: &Cursor) -> Result<(), Halt> {
        self.store
            .put_cursor(c)
            .await
            .map_err(|e| Halt::Hard(format!("store: {e}")))
    }

    /// Everything since `newest_seen - overlap`. Never-crawled streams are left to backfill.
    async fn forward_sweep(
        &self,
        run: &mut RelayRun,
        kind: u16,
        base: Value,
        deadline: Instant,
    ) -> Result<(), Halt> {
        let mut cur = self.cursor(&run.url, kind).await?;
        let Some(newest) = cur.newest_seen else {
            return Ok(());
        };
        let mut pager = Pager::new(
            vec![base],
            Some(newest - self.cfg.overlap_secs),
            None,
            self.cfg.page_limit,
            MAX_FORWARD_PAGES,
        );
        let mut max_seen = newest;
        loop {
            match pager.next(run, deadline).await? {
                Step::Page {
                    events,
                    max_created,
                } => {
                    max_seen = max_seen.max(max_created.min(self.now));
                    self.ingest_all(run, events).await?;
                    self.comments_of_new(run, deadline, false).await?;
                }
                Step::Exhausted => {
                    cur.newest_seen = Some(max_seen);
                    return self.save_cursor(&cur).await;
                }
                Step::Stopped(reason, events) => {
                    self.ingest_all(run, events).await?;
                    return Err(Halt::Soft(reason));
                }
            }
        }
    }

    /// History from `backfill_until` downwards, at most `max_backfill_pages` pages per run.
    async fn backfill(
        &self,
        run: &mut RelayRun,
        kind: u16,
        base: Value,
        deadline: Instant,
    ) -> Result<(), Halt> {
        let mut cur = self.cursor(&run.url, kind).await?;
        if cur.backfill_done {
            return Ok(());
        }
        let mut pager = Pager::new(
            vec![base],
            None,
            cur.backfill_until,
            self.cfg.page_limit,
            self.cfg.max_backfill_pages,
        );
        loop {
            match pager.next(run, deadline).await? {
                Step::Page {
                    events,
                    max_created,
                } => {
                    if cur.newest_seen.is_none() {
                        // First contact: the forward sweep will start from the head of this page.
                        cur.newest_seen = Some(max_created.min(self.now));
                    }
                    cur.backfill_until = pager.until;
                    self.ingest_all(run, events).await?;
                    self.save_cursor(&cur).await?;
                    self.comments_of_new(run, deadline, false).await?;
                }
                Step::Exhausted => {
                    cur.backfill_done = true;
                    if cur.newest_seen.is_none() {
                        // Empty relay: forward sweeps start from now on.
                        cur.newest_seen = Some(self.now);
                    }
                    return self.save_cursor(&cur).await;
                }
                Step::Stopped(reason, events) => {
                    self.ingest_all(run, events).await?;
                    return Err(Halt::Soft(reason));
                }
            }
        }
    }

    /// Comments (2004 by `e`, 1111 by `E` or `e`) on the torrents this run inserted from `run`.
    /// Runs after every torrent page (full batches only) so a budget cut never leaves inserted
    /// torrents without their comment lookup; `all` flushes the remainder.
    async fn comments_of_new(
        &self,
        run: &mut RelayRun,
        deadline: Instant,
        all: bool,
    ) -> Result<(), Halt> {
        while run.new_torrents.len() >= COMMENT_BATCH || (all && !run.new_torrents.is_empty()) {
            let n = run.new_torrents.len().min(COMMENT_BATCH);
            let batch: Vec<String> = run.new_torrents.drain(..n).collect();
            let filters = vec![
                json!({"kinds": [KIND_TORRENT_COMMENT, KIND_COMMENT], "#e": batch}),
                json!({"kinds": [KIND_COMMENT], "#E": batch}),
            ];
            self.drain(run, filters, deadline).await?;
        }
        Ok(())
    }

    /// Paginate a cursor-less query to its end (bounded by MAX_TARGET_PAGES).
    async fn drain(
        &self,
        run: &mut RelayRun,
        filters: Vec<Value>,
        deadline: Instant,
    ) -> Result<(), Halt> {
        let mut pager = Pager::new(filters, None, None, self.cfg.page_limit, MAX_TARGET_PAGES);
        loop {
            match pager.next(run, deadline).await? {
                Step::Page { events, .. } => self.ingest_all(run, events).await?,
                Step::Exhausted => return Ok(()),
                Step::Stopped(reason, events) => {
                    self.ingest_all(run, events).await?;
                    return Err(Halt::Soft(reason));
                }
            }
        }
    }

    async fn ingest_all(&self, run: &mut RelayRun, events: Vec<RawEvent>) -> Result<(), Halt> {
        for ev in events {
            self.ingest_one(run, ev).await?;
        }
        Ok(())
    }

    async fn ingest_one(&self, run: &mut RelayRun, ev: RawEvent) -> Result<(), Halt> {
        let (id, kind) = (ev.id.clone(), ev.kind);
        let hints = relay_hints(&ev);
        // Fresh clock: a long run must not reject events that are only "future" against its start.
        let outcome = ingest_into(self.store, ev, crate::now())
            .await
            .map_err(|e| Halt::Hard(format!("store: {e}")))?;
        let mut r = self.report.lock().unwrap();
        r.events_seen += 1;
        match outcome {
            IngestOutcome::Inserted => {
                r.inserted += 1;
                run.found += 1;
                match kind {
                    KIND_TORRENT => {
                        run.torrents += 1;
                        run.new_torrents.push(id);
                    }
                    KIND_TORRENT_COMMENT | KIND_COMMENT => r.comments += 1,
                    _ => r.metadata_events += 1,
                }
                drop(r);
                let mut h = self.hints.lock().unwrap();
                for hint in hints {
                    if h.len() >= MAX_HINTS {
                        break;
                    }
                    h.entry(hint.0).or_insert(hint.1);
                }
            }
            IngestOutcome::Duplicate => r.duplicates += 1,
            IngestOutcome::Rejected(_) => r.rejected += 1,
        }
        Ok(())
    }

    /// Kinds 0/3/10000/10002/1984/30007 for the root, then its follows and the top publishers, from
    /// every indexer; afterwards the NIP-65 write relays of all of them are stored as candidates.
    async fn metadata_phase(&self, deadline: Instant) {
        let root = match &self.cfg.root {
            Some(r) => Some(r.clone()),
            None => self
                .store
                .get_setting(keys::TRUST_ROOT)
                .await
                .ok()
                .flatten()
                .map(|v| serde_json::from_str::<String>(&v).unwrap_or(v)),
        }
        .and_then(|r| parse_pubkey(&r).ok());
        let publishers = self
            .store
            .publishers(TOP_PUBLISHERS)
            .await
            .unwrap_or_default();

        let indexers: Vec<String> = self.cfg.indexers.clone();
        let root_ref = root.as_deref();
        let publishers_ref = &publishers;
        futures::stream::iter(indexers)
            .for_each_concurrent(self.cfg.concurrency.max(1), |url| async move {
                let mut run = RelayRun::new(url);
                let work = self.metadata_from(&mut run, root_ref, publishers_ref, deadline);
                let _ = tokio::time::timeout_at(deadline + GRACE, work).await;
                if let Some(conn) = run.conn.take() {
                    conn.close().await;
                }
            })
            .await;

        let mut authors: Vec<String> = root.iter().cloned().collect();
        if let Some(r) = &root {
            authors.extend(self.store.follows_of(r).await.unwrap_or_default());
        }
        authors.extend(publishers);
        let lists = match self.store.relay_lists_for(&authors).await {
            Ok(l) => l,
            Err(e) => {
                self.error("store", &e.to_string());
                return;
            }
        };
        let mut h = self.hints.lock().unwrap();
        for (_, url, _, write) in lists {
            if let Some(url) = normalize_relay(&url, false).filter(|_| write) {
                h.insert(url, "nip65");
            }
        }
    }

    async fn metadata_from(
        &self,
        run: &mut RelayRun,
        root: Option<&str>,
        publishers: &[String],
        deadline: Instant,
    ) {
        let left = deadline.saturating_duration_since(Instant::now());
        match RelayConn::connect(&run.url, left.min(CONNECT_TIMEOUT)).await {
            Ok(c) => run.conn = Some(c),
            Err(e) => {
                self.error(&run.url, &e.to_string());
                return;
            }
        }
        let mut authors: Vec<String> = Vec::new();
        if let Some(root) = root {
            if self
                .authors_meta(run, &[root.to_string()], deadline)
                .await
                .is_err()
            {
                return;
            }
            // Follows are known once the root's kind 3 is stored.
            authors.extend(self.store.follows_of(root).await.unwrap_or_default());
        }
        authors.extend(publishers.iter().cloned());
        let mut seen = HashSet::new();
        authors.retain(|a| Some(a.as_str()) != root && is_hex32(a) && seen.insert(a.clone()));
        for batch in authors.chunks(AUTHOR_BATCH) {
            if self.authors_meta(run, batch, deadline).await.is_err() {
                return;
            }
        }
    }

    async fn authors_meta(
        &self,
        run: &mut RelayRun,
        authors: &[String],
        deadline: Instant,
    ) -> Result<(), ()> {
        let base = json!({"kinds": METADATA_KINDS, "authors": authors});
        match self.drain(run, vec![base], deadline).await {
            Ok(()) => Ok(()),
            Err(h) => {
                if self.halt(run, h) {
                    Err(())
                } else {
                    Ok(())
                }
            }
        }
    }
}

/// Relay URLs worth remembering from an accepted event: tag relay hints (`e`/`p`/`E`/`P` index 2)
/// and the legacy relay map in a kind-3 content.
fn relay_hints(ev: &RawEvent) -> Vec<(String, &'static str)> {
    let mut out: Vec<(String, &'static str)> = ev
        .tags
        .iter()
        .filter(|t| matches!(t.first().map(String::as_str), Some("e" | "p" | "E" | "P")))
        .filter_map(|t| normalize_relay(t.get(2)?, false))
        .map(|u| (u, "hint"))
        .collect();
    if ev.kind == KIND_CONTACTS
        && ev.content.trim_start().starts_with('{')
        && let Ok(Value::Object(map)) = serde_json::from_str::<Value>(&ev.content)
    {
        out.extend(
            map.keys()
                .take(50)
                .filter_map(|k| normalize_relay(k, false))
                .map(|u| (u, "kind3")),
        );
    }
    out
}

enum Step {
    /// New (not yet seen in this sweep) events of one page and the page's newest created_at.
    Page {
        events: Vec<RawEvent>,
        max_created: i64,
    },
    /// The relay has nothing more for this query.
    Exhausted,
    /// Ended early (budget, page cap, CLOSED twice, timeout); partial new events attached.
    Stopped(String, Vec<RawEvent>),
}

/// D8 pagination state for one query on one connection.
struct Pager {
    bases: Vec<Map<String, Value>>,
    since: Option<i64>,
    /// `until` of the next page (inclusive).
    until: Option<i64>,
    limit: usize,
    seen: HashSet<String>,
    pages: usize,
    max_pages: usize,
    finished: bool,
}

impl Pager {
    fn new(
        bases: Vec<Value>,
        since: Option<i64>,
        until: Option<i64>,
        limit: usize,
        max_pages: usize,
    ) -> Self {
        Self {
            bases: bases
                .into_iter()
                .map(|b| match b {
                    Value::Object(m) => m,
                    _ => Map::new(),
                })
                .collect(),
            since,
            until,
            limit: limit.max(1),
            seen: HashSet::new(),
            pages: 0,
            max_pages,
            finished: false,
        }
    }

    fn filters(&self) -> Vec<Value> {
        self.bases
            .iter()
            .map(|base| {
                let mut f = base.clone();
                f.insert("limit".into(), json!(self.limit));
                if let Some(s) = self.since {
                    f.insert("since".into(), json!(s));
                }
                if let Some(u) = self.until {
                    f.insert("until".into(), json!(u));
                }
                Value::Object(f)
            })
            .collect()
    }

    fn fresh(&mut self, events: Vec<RawEvent>) -> Vec<RawEvent> {
        events
            .into_iter()
            .filter(|e| self.seen.insert(e.id.clone()))
            .collect()
    }

    async fn next(&mut self, run: &mut RelayRun, deadline: Instant) -> Result<Step, Halt> {
        if self.finished {
            return Ok(Step::Exhausted);
        }
        if self.pages >= self.max_pages {
            return Ok(Step::Stopped("page cap".into(), Vec::new()));
        }
        let Some(conn) = run.conn.as_mut() else {
            return Err(Halt::Hard("not connected".into()));
        };
        let mut retried = false;
        let page = loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left < Duration::from_secs(1) {
                return Ok(Step::Stopped("budget".into(), Vec::new()));
            }
            let wait = left.min(PAGE_TIMEOUT);
            let page = conn
                .fetch_page_multi(self.filters(), wait)
                .await
                .map_err(|e| Halt::Hard(e.to_string()))?;
            if let Some(msg) = &page.closed {
                if !conn.is_open() {
                    let events = self.fresh(page.events);
                    run.conn = None;
                    return Ok(Step::Stopped(format!("closed: {msg}"), events));
                }
                // One retry: relays CLOSE on transient rate limits.
                if !retried && left > CLOSED_RETRY_DELAY + Duration::from_secs(1) {
                    retried = true;
                    tokio::time::sleep(CLOSED_RETRY_DELAY).await;
                    continue;
                }
                let msg = format!("closed: {msg}");
                return Ok(Step::Stopped(msg, self.fresh(page.events)));
            }
            if page.timed_out {
                // A wait cut short by the crawl deadline says nothing about the relay.
                let reason = if wait < PAGE_TIMEOUT {
                    "budget"
                } else {
                    "timeout"
                };
                return Ok(Step::Stopped(reason.into(), self.fresh(page.events)));
            }
            break page;
        };
        self.pages += 1;
        let (Some(min), Some(max)) = (
            page.events.iter().map(|e| e.created_at).min(),
            page.events.iter().map(|e| e.created_at).max(),
        ) else {
            self.finished = true;
            return Ok(Step::Exhausted);
        };
        let events = self.fresh(page.events);
        if events.is_empty() {
            self.finished = true;
            return Ok(Step::Exhausted);
        }
        // Inclusive until, strictly decreasing: more than a page of events in one second would
        // otherwise loop forever.
        let next = match self.until {
            Some(u) if min >= u => u - 1,
            _ => min,
        };
        self.until = Some(next);
        if self.since.is_some_and(|s| next < s) {
            self.finished = true;
        }
        Ok(Step::Page {
            events,
            max_created: max,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hints_from_tags_and_kind3() {
        let ev = RawEvent {
            id: "00".repeat(32),
            pubkey: "11".repeat(32),
            created_at: 1,
            kind: KIND_CONTACTS,
            tags: vec![
                vec![
                    "p".into(),
                    "22".repeat(32),
                    "wss://relay.example.com/".into(),
                ],
                vec!["p".into(), "33".repeat(32), "ws://127.0.0.1:1".into()],
                vec!["t".into(), "x".into(), "wss://ignored.example.com".into()],
            ],
            content: r#"{"wss://nos.lol/":{"read":true,"write":true},"junk":{}}"#.into(),
            sig: String::new(),
        };
        assert_eq!(
            relay_hints(&ev),
            vec![
                ("wss://relay.example.com".to_string(), "hint"),
                ("wss://nos.lol".to_string(), "kind3"),
            ]
        );
    }

    #[test]
    fn refusals_by_prefix() {
        for r in [
            "closed: rate-limited: slow down",
            "closed: RESTRICTED: not a member",
            "closed: auth-required: we can't serve DMs to unauthenticated users",
            "closed: blocked: your IP",
        ] {
            assert!(is_refusal(r), "{r}");
        }
        for r in [
            "closed: ERROR: bad req: unrecognised filter item: search",
            "closed: unsupported: #K",
            "closed: rate limited",
            "timeout",
            "page cap",
        ] {
            assert!(!is_refusal(r), "{r}");
        }
    }

    #[test]
    fn soft_trouble_verdict() {
        let mut run = RelayRun::new("wss://r.example".into());
        run.connected = true;
        assert!(run.ok());
        run.note = Some("closed: error: bad filter".into());
        assert!(run.ok(), "a filter-level CLOSED is no reason to back off");
        run.timed_out = true;
        assert!(!run.ok(), "timed out and brought nothing new");
        run.found = 3;
        assert!(run.ok(), "slow but productive");
        run.refused = Some("closed: rate-limited: slow down".into());
        assert!(
            !run.ok(),
            "a refusal backs off even after a productive page"
        );
    }

    #[test]
    fn pager_filter_shape() {
        let mut p = Pager::new(vec![json!({"kinds": [2003]})], Some(10), None, 500, 3);
        assert_eq!(
            p.filters(),
            vec![json!({"kinds": [2003], "limit": 500, "since": 10})]
        );
        p.until = Some(20);
        assert_eq!(
            p.filters(),
            vec![json!({"kinds": [2003], "limit": 500, "since": 10, "until": 20})]
        );
    }
}

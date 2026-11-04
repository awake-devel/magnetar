//! A worker's DHT crawler (D97): its own `core::dht` nodes (own BEP 42 ids and ports), the
//! server's queue, seen filter, health probe and metadata fetcher, but the database replaced
//! by the server's worker API:
//!
//! sightings → seen filter → **triage at the server** (batches ≤ 1000 / 1 s; the server's
//! `dht_seen` decides and leases, so no two fleet members fetch the same hash) → fetch queue →
//! peer collection → BEP 9 fetch (raw bytes kept) → classify + **blocklist here** → upload
//! (blocked: hash only; else the raw dict, which the server verifies, classifies and screens
//! again) or a status (`no_peers`, failures, private, …).
//!
//! Memory is bounded for small machines: the pool, queue and seen filter sizes come from the
//! options, uploads wait in a byte-capped buffer (fetching stops when it is full), and the
//! [`Governor`] pauses discovery under memory or conntrack pressure. Nothing here logs a name.
//!
//! Header probes (D117, `MAGNETAR_PROBE`, off by default): after a verified dict that passed
//! the blocklist (adult and BEP 27 private included, D119), while no brake or pressure is on and the prober has a free
//! slot under its per-minute pace, the metadata connection is handed to a
//! [`magnetar_core::probe::Prober`] task that reads the few blocks sniff needs; its report
//! travels with the dict ([`DhtResult::probe`]). A probe never delays the dict for more than
//! its session cap, and a stop sends the dict without it.
//!
//! D118: with `MAGNETAR_DHT_DISCOVER=false` the engine runs no sampler and nothing it hears is
//! triaged: the node only serves peer lookups. With `MAGNETAR_PROBE_BACKFILL=true` a feeder
//! leases already indexed torrents from the server (`probe/lease`, at most twice
//! `MAGNETAR_PROBE_PARALLEL` in the worker at once) and queues them like triaged hashes: the
//! same peer collection, fetcher (pace, brake, address policy, identity), screen and probe
//! gates. A backfill dict waits up to [`BACKFILL_SLOT_WAIT`] for a probe slot and turn instead
//! of being skipped; it goes up with its probe on `dht/results`, where the lease makes it held.
//! Hashes still in the worker at a stop go back (`probe/release`).

use std::collections::{HashMap, HashSet};
use std::net::SocketAddrV4;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use magnetar_core::api::{WorkerBackfillState, WorkerDhtState, WorkerProbeState};
use magnetar_core::dht::{Dht, EngineConfig, EngineStats, Mode, Sighting, Via, collect_peers};
use magnetar_core::metainfo::{FetchConfig, FetchStats, Fetcher, Handoff, InfoFile, Outcome};
use magnetar_core::model::TorrentFile;
use magnetar_core::probe::{self, ProbeConfig, ProbeJob, ProbeResult, Prober, Skip};
use serde::Serialize;
use tokio::sync::{Notify, OwnedSemaphorePermit, Semaphore, mpsc, watch};
use tokio::task::JoinSet;
use tokio::time::Instant;

use super::client::Client;
use super::governor::Governor;
use super::seeds::with_retry;
use super::{Abort, state};
use crate::daemons::stop_requested;
use crate::dht::health::{self, HealthStats};
use crate::dht::queue::{FetchItem, FetchQueue};
use crate::dht::screen::Verdict;
use crate::dht::seen::{self, SeenFilter};
use crate::dht::{DhtOptions, classify, upnp};
use crate::fleet::proto::*;

const BATCH_MAX: usize = 1000;
const BATCH_WAIT: Duration = Duration::from_secs(1);
const TRIAGE_BACKLOG_MAX: usize = 50_000;
const LOOKUP_BUDGET: Duration = Duration::from_secs(10);
const UPLOAD_WAIT: Duration = Duration::from_secs(2);
/// Bytes of encoded dicts per upload request (a larger single dict goes alone).
const UPLOAD_BYTES: usize = 4 << 20;
const PERSIST_EVERY: Duration = Duration::from_secs(600);
const CONTROL_EVERY: Duration = Duration::from_millis(500);
/// Sampling pauses at this many queued hashes (or half the queue) and resumes at half (D62).
const QUEUE_HIGH: usize = 10_000;
/// Brake length while the governor reports pressure (renewed while it lasts).
const PRESSURE_BRAKE: Duration = Duration::from_secs(30);
/// A backfill dict waits this long at most for a probe slot and turn (D118), then goes up
/// without a probe (its lease is retried later by the server).
pub const BACKFILL_SLOT_WAIT: Duration = Duration::from_secs(30);
/// Backfill feeder: pause after an empty lease, a failed one, an old server (404), and while
/// a brake or pressure is on or enough leased hashes are in the worker.
const BACKFILL_IDLE: Duration = Duration::from_secs(60);
const BACKFILL_RETRY: Duration = Duration::from_secs(30);
const BACKFILL_OLD_SERVER: Duration = Duration::from_secs(600);
const BACKFILL_TICK: Duration = Duration::from_secs(2);

/// Probe backfill on this worker (D118): the leased hashes in it and its counters.
struct Backfill {
    /// Leased hashes queued, being collected, fetched or probed, and when their lease ends.
    out: Mutex<HashMap<[u8; 20], Instant>>,
    /// At most this many in the worker at once.
    cap: usize,
    leased: AtomicU64,
    released: AtomicU64,
    no_peers: AtomicU64,
    failed: AtomicU64,
    fetched: AtomicU64,
    probed: AtomicU64,
    unprobed: AtomicU64,
    lease_errors: AtomicU64,
}

impl Backfill {
    fn new(cap: usize) -> Self {
        Self {
            out: Mutex::default(),
            cap: cap.max(1),
            leased: AtomicU64::new(0),
            released: AtomicU64::new(0),
            no_peers: AtomicU64::new(0),
            failed: AtomicU64::new(0),
            fetched: AtomicU64::new(0),
            probed: AtomicU64::new(0),
            unprobed: AtomicU64::new(0),
            lease_errors: AtomicU64::new(0),
        }
    }

    fn has(&self, h: &[u8; 20]) -> bool {
        self.out.lock().unwrap().contains_key(h)
    }

    fn state(&self) -> WorkerBackfillState {
        WorkerBackfillState {
            leased: get(&self.leased),
            released: get(&self.released),
            outstanding: self.out.lock().unwrap().len() as u64,
            no_peers: get(&self.no_peers),
            failed: get(&self.failed),
            fetched: get(&self.fetched),
            probed: get(&self.probed),
            unprobed: get(&self.unprobed),
            lease_errors: get(&self.lease_errors),
        }
    }

    /// A leased hash leaves the worker with this result (counted by outcome).
    fn done(&self, item: &DhtResult) {
        let Some(h) = parse_hash(&item.sighted) else {
            return;
        };
        if self.out.lock().unwrap().remove(&h).is_none() {
            return;
        }
        match &item.outcome {
            DhtOutcome::Metadata { .. } => {
                add(&self.fetched, 1);
                add(
                    if item.probe.is_some() {
                        &self.probed
                    } else {
                        &self.unprobed
                    },
                    1,
                );
            }
            DhtOutcome::Failed { code } if code == "no_peers" => add(&self.no_peers, 1),
            _ => add(&self.failed, 1),
        }
    }

    /// A leased hash leaves the worker with nothing to upload (no blocklist loaded).
    fn drop_hash(&self, h: &[u8; 20]) {
        if self.out.lock().unwrap().remove(h).is_some() {
            add(&self.failed, 1);
        }
    }
}

#[derive(Default)]
struct Counters {
    sightings: AtomicU64,
    unique: AtomicU64,
    triaged: AtomicU64,
    queued: AtomicU64,
    triage_known: AtomicU64,
    triage_blocked: AtomicU64,
    triage_errors: AtomicU64,
    triage_dropped: AtomicU64,
    collected: AtomicU64,
    with_peers: AtomicU64,
    no_peers: AtomicU64,
    verified: AtomicU64,
    blocked_here: AtomicU64,
    uploaded: AtomicU64,
    upload_requests: AtomicU64,
    upload_errors: AtomicU64,
    upload_dropped: AtomicU64,
    upload_backlog: AtomicU64,
    server_stored: AtomicU64,
    server_not_new: AtomicU64,
    server_blocked: AtomicU64,
    server_rejected: AtomicU64,
    server_ignored: AtomicU64,
    unscreened: AtomicU64,
    paused_ms: AtomicU64,
}

fn add(a: &AtomicU64, n: u64) {
    a.fetch_add(n, Ordering::Relaxed);
}

fn get(a: &AtomicU64) -> u64 {
    a.load(Ordering::Relaxed)
}

/// What the worker's crawler did (JSON report at stop).
#[derive(Clone, Debug, Default, Serialize)]
pub struct CrawlReport {
    pub mode: String,
    pub ports: Vec<u16>,
    pub seconds: f64,
    pub sightings: u64,
    pub unique_hashes: u64,
    pub triaged: u64,
    /// Hashes the server told this worker to fetch.
    pub queued_by_server: u64,
    pub known_at_server: u64,
    pub blocked_at_server: u64,
    pub triage_errors: u64,
    pub triage_dropped: u64,
    pub collected: u64,
    pub with_peers: u64,
    pub no_peers: u64,
    /// Verified info dicts (fetcher), and of those stopped by this worker's blocklist.
    pub verified: u64,
    pub blocked_here: u64,
    /// Results handed to the server (dicts and statuses), requests, failed requests.
    pub uploaded: u64,
    pub upload_requests: u64,
    pub upload_errors: u64,
    /// Results the server refused for good (D113: a 4xx on a single-item upload), dropped.
    pub upload_dropped: u64,
    /// What the server answered.
    pub server_stored: u64,
    pub server_not_new: u64,
    pub server_blocked: u64,
    pub server_rejected: u64,
    pub server_ignored: u64,
    pub sampling_paused_secs: f64,
    pub queries_per_sec: f64,
    pub answered_share: f64,
    pub fetch: FetchStats,
    pub health: HealthStats,
    pub engine: EngineStats,
    /// Header probes (D117), when on.
    pub probe: Option<WorkerProbeState>,
    /// D118: whether it looked for new torrents, and the probe backfill when on.
    pub discover: bool,
    pub backfill: Option<WorkerBackfillState>,
}

struct Pending {
    item: DhtResult,
    _permit: Option<OwnedSemaphorePermit>,
    bytes: usize,
}

/// Rates for the heartbeat: counters at the previous call.
#[derive(Default)]
struct Snap {
    at: Option<Instant>,
    queries: u64,
    answered: u64,
    unique: u64,
    verified: u64,
}

pub struct Crawl {
    client: Client,
    opts: DhtOptions,
    queue: Mutex<FetchQueue>,
    notify: Notify,
    fetcher: Fetcher,
    c: Counters,
    health: Arc<Mutex<HealthStats>>,
    governor: Arc<Governor>,
    /// Upload buffer, in KiB units.
    buffer: Arc<Semaphore>,
    buffer_units: u32,
    screen_slots: Semaphore,
    dht: Mutex<Option<Dht>>,
    snap: Mutex<Snap>,
    max_results: usize,
    /// Header probes (D117): Some only on a worker with `MAGNETAR_PROBE=true`.
    prober: Option<Prober>,
    /// `MAGNETAR_DHT_DISCOVER` (D118): sample and triage what the node hears.
    discover: bool,
    /// Probe backfill (D118): Some with `MAGNETAR_PROBE_BACKFILL=true` (and probes on).
    backfill: Option<Backfill>,
}

fn probe_state(p: &Prober) -> WorkerProbeState {
    let s = p.stats();
    WorkerProbeState {
        upload: p.config().upload,
        started: s.started,
        ok: s.ok,
        partial: s.partial,
        choked: s.choked,
        no_pieces: s.no_pieces,
        failed: s.failed,
        skipped: s.skipped,
        bytes_in: s.bytes_in,
        uploaded: s.uploaded,
        files: s.files,
        verified_files: s.verified_files,
        surprises: s.surprises,
    }
}

impl Crawl {
    fn enqueue(&self, item: FetchItem) {
        if self.queue.lock().unwrap().push(item) {
            self.notify.notify_one();
        }
    }

    /// The heartbeat's DHT section (rates since the previous call).
    pub fn state(&self) -> Option<WorkerDhtState> {
        let dht = self.dht.lock().unwrap().clone()?;
        let e = dht.stats();
        let f = self.fetcher.stats();
        let verified = f.ok + f.private + f.v2_only + f.invalid;
        let unique = get(&self.c.unique);
        let now = Instant::now();
        let mut s = self.snap.lock().unwrap();
        let secs =
            s.at.map_or(0.0, |a| now.duration_since(a).as_secs_f64())
                .max(1.0);
        let d = |a: u64, b: u64| a.saturating_sub(b) as f64;
        let st = WorkerDhtState {
            mode: match self.opts.mode {
                Mode::Polite => "polite",
                Mode::Aggressive => "aggressive",
            }
            .into(),
            nodes: dht.virtual_nodes() as u32,
            ports: dht.local_ports(),
            qps: d(e.queries_sent, s.queries) / secs,
            answered_share: if e.queries_sent > s.queries {
                d(e.answered, s.answered) / d(e.queries_sent, s.queries)
            } else {
                0.0
            },
            brake_active: e.brake_active,
            health_brakes: self.health.lock().unwrap().brakes,
            sampling_paused: !self.discover || dht.sample_share() <= 0.0,
            routing_nodes: e.routing.iter().sum::<usize>() as u64,
            pool: e.pool.size as u64,
            queue: self.queue.lock().unwrap().len() as u64,
            unique_per_min: d(unique, s.unique) * 60.0 / secs,
            metadata_per_min: d(verified, s.verified) * 60.0 / secs,
            upload_backlog: get(&self.c.upload_backlog),
            external_ip_known: e.external_ip_known,
            probe: self.prober.as_ref().map(probe_state),
            discover: Some(self.discover),
            backfill: self.backfill.as_ref().map(Backfill::state),
        };
        *s = Snap {
            at: Some(now),
            queries: e.queries_sent,
            answered: e.answered,
            unique,
            verified,
        };
        Some(st)
    }
}

fn via_text(v: Via) -> &'static str {
    match v {
        Via::Sample => "sample",
        Via::Announce => "announce",
        Via::GetPeers => "get_peers",
    }
}

async fn triage_loop(
    p: Arc<Crawl>,
    mut rx: mpsc::Receiver<Sighting>,
    batches: mpsc::Sender<Vec<Sighting>>,
    mut stop: watch::Receiver<bool>,
) {
    let mut seen = SeenFilter::new(p.opts.bloom_bits, seen::DEFAULT_K, seen::ROTATE_EVERY);
    let mut batch: Vec<Sighting> = Vec::new();
    let mut flush_at = Instant::now() + BATCH_WAIT;
    let hand = |batch: &mut Vec<Sighting>| -> bool {
        if batch.is_empty() {
            return true;
        }
        match batches.try_send(std::mem::take(batch)) {
            Err(mpsc::error::TrySendError::Full(b)) => {
                *batch = b;
                false
            }
            _ => true,
        }
    };
    loop {
        tokio::select! {
            s = rx.recv() => match s {
                Some(s) => {
                    add(&p.c.sightings, 1);
                    if let Some(g) = &p.opts.geo {
                        g.dht_node(s.from, magnetar_core::now());
                    }
                    if !p.discover {
                        // D118: no new finds; an announce may still help a queued backfill hash.
                        if let (Via::Announce, Some(peer)) = (s.via, s.peer) {
                            let _ = p.queue.lock().unwrap().attach_announce(s.info_hash, peer, Instant::now());
                        }
                    } else if batch.len() >= TRIAGE_BACKLOG_MAX {
                        add(&p.c.triage_dropped, 1);
                    } else if seen.check_and_insert(&s.info_hash, Instant::now()) {
                        add(&p.c.unique, 1);
                        if batch.is_empty() {
                            flush_at = Instant::now() + BATCH_WAIT;
                        }
                        batch.push(s);
                        if batch.len() >= BATCH_MAX {
                            hand(&mut batch);
                        }
                    } else if let (Via::Announce, Some(peer)) = (s.via, s.peer) {
                        // A live peer for a hash we may be holding (D93).
                        let _ = p.queue.lock().unwrap().attach_announce(s.info_hash, peer, Instant::now());
                    }
                }
                None => break,
            },
            _ = tokio::time::sleep_until(flush_at), if !batch.is_empty() => {
                if !hand(&mut batch) {
                    flush_at = Instant::now() + Duration::from_millis(200);
                }
            }
            _ = stop_requested(&mut stop) => break,
        }
    }
}

/// Asks the server about each batch (one request at a time) and queues what it hands out.
async fn triage_task(
    p: Arc<Crawl>,
    mut rx: mpsc::Receiver<Vec<Sighting>>,
    abort: Abort,
    mut stop: watch::Receiver<bool>,
) {
    while let Some(batch) = rx.recv().await {
        // D105: codes of the telling nodes (never their addresses), node counts, AS names.
        let (geo, (geo_nodes, as_names)) = match &p.opts.geo {
            Some(g) => (
                g.dht_hashes(batch.iter().map(|s| s.from), false),
                g.take_upload(),
            ),
            None => (Vec::new(), (Vec::new(), Vec::new())),
        };
        let req = TriageRequest {
            items: batch
                .iter()
                .enumerate()
                .map(|(i, s)| {
                    let code = geo.get(i).copied().flatten();
                    TriageItem {
                        h: hex::encode(s.info_hash),
                        via: via_text(s.via).into(),
                        // "" = resolved, not in the database (counted as unknown).
                        cc: code.map(|c| c.cc_text()),
                        asn: code.and_then(|c| c.asn()),
                    }
                })
                .collect(),
            geo_nodes,
            as_names,
        };
        add(&p.c.triaged, batch.len() as u64);
        let r = with_retry(3, &mut stop, || {
            p.client.post::<_, TriageResponse>(TRIAGE, &req)
        })
        .await;
        let resp = match r {
            Ok(r) => r,
            Err(e) => {
                if e.is_fatal() {
                    abort.fatal(&e);
                    return;
                }
                if get(&p.c.triage_errors).is_multiple_of(50) {
                    tracing::warn!("worker: triage: {e}");
                }
                add(&p.c.triage_errors, 1);
                continue;
            }
        };
        add(&p.c.triage_known, u64::from(resp.known));
        add(&p.c.triage_blocked, u64::from(resp.blocked));
        let fetch: HashSet<[u8; 20]> = resp.fetch.iter().filter_map(|h| parse_hash(h)).collect();
        add(&p.c.queued, fetch.len() as u64);
        let now = Instant::now();
        let mut done = HashSet::new();
        let mut ordered = batch;
        ordered.sort_by_key(|s| s.via != Via::Announce);
        for s in ordered {
            if fetch.contains(&s.info_hash) && done.insert(s.info_hash) {
                p.enqueue(FetchItem {
                    info_hash: s.info_hash,
                    via: s.via,
                    source: s.source,
                    peer: s.peer,
                    retry: false,
                    queued_at: now,
                });
            }
        }
    }
}

struct Job {
    info_hash: [u8; 20],
    via: Via,
    peers: Vec<SocketAddrV4>,
}

async fn collector(
    p: Arc<Crawl>,
    dht: Dht,
    jobs: mpsc::Sender<Job>,
    out: mpsc::Sender<Pending>,
    mut stop: watch::Receiver<bool>,
) {
    let want = p.opts.peers_per_hash.max(1);
    loop {
        let item = p.queue.lock().unwrap().pop_for_collection();
        let Some(item) = item else {
            tokio::select! {
                _ = p.notify.notified() => {}
                _ = tokio::time::sleep(Duration::from_secs(1)) => {}
                _ = stop_requested(&mut stop) => return,
            }
            continue;
        };
        let c = tokio::select! {
            c = collect_peers(&dht, item.info_hash, item.peer, item.source, LOOKUP_BUDGET, want) => c,
            _ = stop_requested(&mut stop) => return,
        };
        let mut peers = c.peers;
        let mut via = item.via;
        if let Some(peer) = p.queue.lock().unwrap().collected(&item.info_hash)
            && dht.allows(&peer)
        {
            peers.retain(|x| *x != peer);
            peers.insert(0, peer);
            via = Via::Announce;
        }
        add(&p.c.collected, 1);
        if peers.is_empty() {
            add(&p.c.no_peers, 1);
            let r = Pending {
                item: DhtResult {
                    sighted: hex::encode(item.info_hash),
                    via: via_text(via).into(),
                    peers_seen: 0,
                    outcome: DhtOutcome::Failed {
                        code: "no_peers".into(),
                    },
                    probe: None,
                },
                _permit: None,
                bytes: 64,
            };
            if let Some(b) = &p.backfill {
                b.done(&r.item);
            }
            if out.send(r).await.is_err() {
                return;
            }
            continue;
        }
        add(&p.c.with_peers, 1);
        let job = Job {
            info_hash: item.info_hash,
            via,
            peers,
        };
        tokio::select! {
            r = jobs.send(job) => if r.is_err() { return },
            _ = stop_requested(&mut stop) => return,
        }
    }
}

/// What a header probe needs from the verified dict (D117).
struct ProbeSeed {
    raw: Arc<[u8]>,
    files: Vec<InfoFile>,
}

/// A screened result, and the probe input when this torrent may be probed.
struct Screened {
    outcome: DhtOutcome,
    seed: Option<ProbeSeed>,
}

/// Classify and screen a verified dict here, before anything leaves the machine. With probes
/// on, also decide whether the torrent may be probed: one that passed the blocklist (D117;
/// adult and private included since D119), with a dict of at most [`probe::MAX_DICT`].
fn outcome_item(p: &Crawl, job: &Job, outcome: Outcome) -> Option<Screened> {
    let skip = |why: Skip| {
        if let Some(pr) = &p.prober {
            pr.skip(why);
        }
    };
    let mut seed = None;
    let outcome = match outcome {
        Outcome::Ok(f) => {
            let Some(screen) = p.opts.screen.as_deref() else {
                // No blocklist loaded: nothing named may leave (fail closed). The server's
                // lease brings the hash back later.
                add(&p.c.unscreened, 1);
                skip(Skip::NoBlocklist);
                return None;
            };
            let files: Vec<TorrentFile> = f
                .info
                .files
                .iter()
                .map(|x| TorrentFile {
                    path: x.path.replace('\0', ""),
                    size: Some(x.size),
                })
                .collect();
            let class = classify::classify(&f.info.name, &files);
            match screen.verdict(&f.info.all_names, class.adult()) {
                Verdict::Block(tier) => {
                    add(&p.c.blocked_here, 1);
                    skip(Skip::Blocklist);
                    DhtOutcome::Blocked {
                        v1: (f.v1 != job.info_hash).then(|| hex::encode(f.v1)),
                        tier: tier.as_str().into(),
                        rule_version: screen.rule_version(),
                    }
                }
                Verdict::Allow => {
                    let raw = f.raw.as_deref()?;
                    if p.prober.is_some() {
                        // D119: adult and BEP 27 private dicts are probed like any other.
                        if raw.len() > probe::MAX_DICT {
                            skip(Skip::BigDict);
                        } else {
                            seed = Some(ProbeSeed {
                                raw: Arc::from(raw),
                                files: f.info.files.clone(),
                            });
                        }
                    }
                    DhtOutcome::Metadata {
                        raw: data_encoding::BASE64.encode(raw),
                    }
                }
            }
        }
        Outcome::Private => DhtOutcome::Private,
        Outcome::V2Only => DhtOutcome::V2Only,
        Outcome::Invalid(e) => DhtOutcome::Invalid {
            code: e.code().into(),
        },
    };
    Some(Screened { outcome, seed })
}

/// Further peers for a probe: those that took our handshake for this hash first, then the
/// rest of the DHT's list; never the hand-off peer itself.
fn probe_peers(h: &Handoff, found: &[SocketAddrV4]) -> Vec<SocketAddrV4> {
    let mut seen: HashSet<SocketAddrV4> = HashSet::from([h.conn.addr]);
    h.answered
        .iter()
        .chain(found)
        .filter(|a| seen.insert(**a))
        .copied()
        .take(16)
        .collect()
}

/// The worker-side gates of a probe (D117), in order: no brake (router health, answered
/// share or pressure brake on the fetcher), no memory / conntrack pressure, then a slot under
/// `MAGNETAR_PROBE_PARALLEL` and the `MAGNETAR_PROBE_PER_MIN` pace. Each refusal is counted.
fn probe_slot(p: &Crawl, prober: &Prober) -> Option<probe::ProbeSlot> {
    if !probe_allowed(p, prober) {
        return None;
    }
    prober.try_slot()
}

/// The brake and pressure gates of [`probe_slot`], each refusal counted.
/// One line per probe at debug level, target `probe_diag` (`RUST_LOG=info,probe_diag=debug`):
/// codes and counts only, never a hash, name or address (docs/PROBES.md, Diagnostics).
fn log_probe_diag(r: &ProbeResult, src: &str) {
    if !tracing::enabled!(target: "probe_diag", tracing::Level::DEBUG) {
        return;
    }
    tracing::debug!(target: "probe_diag", "{}", probe_diag_line(r, src));
}

fn log_probe_skip(why: &str) {
    tracing::debug!(target: "probe_diag", "kind=skip why={why}");
}

fn probe_diag_line(r: &ProbeResult, src: &str) -> String {
    fn pairs<K: std::fmt::Display, V: std::fmt::Display>(v: &[(K, V)]) -> String {
        if v.is_empty() {
            return "-".into();
        }
        v.iter()
            .map(|(k, n)| format!("{k}:{n}"))
            .collect::<Vec<_>>()
            .join(",")
    }
    fn ms(v: Option<u64>) -> String {
        v.map_or_else(|| "-".into(), |m| m.to_string())
    }
    let d = &r.diag;
    format!(
        "kind=probe src={src} status={} end={} late={} ms={} whole={} piece_kib={} targets={} \
         results={} cut={} no_bytes={} no_det={} low_conf={} struck={} first_byte_ms={} \
         unchoke_ms={} handoff_choked={} connects={} max_alive={} cap_blocked={} \
         cand_left={} drops={} choke_expired={} bytes_in={} incomplete={}",
        r.status.code(),
        d.end,
        d.late,
        d.ms,
        d.whole,
        d.piece_len / 1024,
        d.targets,
        r.files.len(),
        d.cut,
        d.no_bytes,
        d.no_detection,
        pairs(&d.low_conf),
        d.struck,
        ms(d.first_byte_ms),
        ms(d.unchoke_ms),
        d.handoff_choked,
        pairs(&d.connects),
        d.max_alive,
        d.cap_blocked,
        d.candidates_left,
        pairs(&d.drops),
        d.choke_expired,
        r.bytes_in,
        r.incomplete,
    )
}

fn probe_allowed(p: &Crawl, prober: &Prober) -> bool {
    if p.fetcher.braked() || p.fetcher.brake_level() > 0 {
        prober.skip(Skip::Brake);
        return false;
    }
    if p.governor.pressure() || p.governor.conntrack_full() {
        prober.skip(Skip::Pressure);
        return false;
    }
    true
}

fn cpu<R>(f: impl FnOnce() -> R) -> R {
    match tokio::runtime::Handle::try_current() {
        Ok(h) if h.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
            tokio::task::block_in_place(f)
        }
        _ => f(),
    }
}

async fn fetch_worker(
    p: Arc<Crawl>,
    jobs: Arc<tokio::sync::Mutex<mpsc::Receiver<Job>>>,
    out: mpsc::Sender<Pending>,
    mut stop: watch::Receiver<bool>,
) {
    loop {
        let job = tokio::select! {
            j = async { jobs.lock().await.recv().await } => match j {
                Some(j) => j,
                None => return,
            },
            _ = stop_requested(&mut stop) => return,
        };
        // With probes on, a verified dict comes with its connection (D117).
        let (result, handoff) = tokio::select! {
            r = async {
                if p.prober.is_some() {
                    p.fetcher.fetch_with_handoff(job.info_hash, &job.peers).await
                } else {
                    (p.fetcher.fetch(job.info_hash, &job.peers).await, None)
                }
            } => r,
            _ = stop_requested(&mut stop) => return,
        };
        let (outcome, seed) = match result {
            Ok(o) => {
                add(&p.c.verified, 1);
                let Ok(_slot) = p.screen_slots.acquire().await else {
                    return;
                };
                match cpu(|| outcome_item(&p, &job, o)) {
                    Some(s) => (s.outcome, s.seed),
                    None => {
                        if let Some(b) = &p.backfill {
                            b.drop_hash(&job.info_hash);
                        }
                        continue;
                    }
                }
            }
            Err(f) if f.oversize => (DhtOutcome::Oversize, None),
            Err(f) => (
                DhtOutcome::Failed {
                    code: f.code().into(),
                },
                None,
            ),
        };
        let had_dict = seed.is_some();
        // Not probed: the connection closes now (its permit goes back to the fetcher).
        let probe_input = match (seed, handoff, &p.prober) {
            (Some(seed), Some(h), Some(_)) => Some((seed, h)),
            (Some(_), None, Some(pr)) => {
                pr.skip(Skip::NoHandoff);
                None
            }
            _ => None,
        };
        let bytes = match &outcome {
            DhtOutcome::Metadata { raw } => raw.len() + 128,
            _ => 128,
        };
        // A dict waits for room in the upload buffer (this is what bounds the memory).
        let units = (bytes / 1024).max(1).min(p.buffer_units as usize) as u32;
        let permit = tokio::select! {
            r = p.buffer.clone().acquire_many_owned(units) => match r {
                Ok(x) => x,
                Err(_) => return,
            },
            _ = stop_requested(&mut stop) => return,
        };
        let pending = Pending {
            item: DhtResult {
                sighted: hex::encode(job.info_hash),
                via: via_text(job.via).into(),
                peers_seen: job.peers.len() as u32,
                outcome,
                probe: None,
            },
            _permit: Some(permit),
            bytes,
        };
        // D118: a leased backfill hash waits for its probe's slot and turn (bounded) instead
        // of being skipped; the brake and pressure gates still apply.
        let backfill = p.backfill.as_ref().is_some_and(|b| b.has(&job.info_hash));
        if backfill {
            if had_dict && probe_input.is_none() && p.prober.is_some() {
                log_probe_skip("no_handoff");
            }
            let waiting = probe_input.and_then(|(seed, h)| {
                let prober = p.prober.clone()?;
                let ok = probe_allowed(&p, &prober);
                if !ok {
                    log_probe_skip("gate");
                }
                ok.then_some((prober, seed, h))
            });
            let (p2, out, mut stop) = (p.clone(), out.clone(), stop.clone());
            tokio::spawn(async move {
                let mut pending = pending;
                if let Some((prober, seed, h)) = waiting {
                    let pj = ProbeJob {
                        info_hash: job.info_hash,
                        raw: seed.raw,
                        files: seed.files,
                        peers: probe_peers(&h, &job.peers),
                        conn: Some(h.conn),
                    };
                    tokio::select! {
                        r = async {
                            let slot = tokio::time::timeout(BACKFILL_SLOT_WAIT, prober.slot()).await.ok()?;
                            Some(prober.probe(slot, pj).await)
                        } => match r {
                            Some(r) => {
                                log_probe_diag(&r, "backfill");
                                if let Some(rep) = crate::probes::report_of(&r) {
                                    pending.bytes += serde_json::to_vec(&rep).map_or(0, |b| b.len());
                                    pending.item.probe = Some(rep);
                                }
                            }
                            None => {
                                log_probe_skip("slot_wait");
                                prober.skip(Skip::Busy);
                            }
                        },
                        _ = stop_requested(&mut stop) => {}
                    }
                }
                if let Some(b) = &p2.backfill {
                    b.done(&pending.item);
                }
                let _ = out.send(pending).await;
            });
            continue;
        }
        let probe = probe_input.and_then(|(seed, h)| {
            let prober = p.prober.clone()?;
            let slot = probe_slot(&p, &prober)?;
            Some((prober, slot, seed, h))
        });
        let Some((prober, slot, seed, h)) = probe else {
            if out.send(pending).await.is_err() {
                return;
            }
            continue;
        };
        let pj = ProbeJob {
            info_hash: job.info_hash,
            raw: seed.raw,
            files: seed.files,
            peers: probe_peers(&h, &job.peers),
            conn: Some(h.conn),
        };
        // The probe runs beside the fetches (bounded by the prober's slots and its session
        // cap); the dict waits for it, never longer, and goes without it on a stop.
        let (out, mut stop) = (out.clone(), stop.clone());
        tokio::spawn(async move {
            let mut pending = pending;
            tokio::select! {
                r = prober.probe(slot, pj) => {
                    log_probe_diag(&r, "live");
                    if let Some(rep) = crate::probes::report_of(&r) {
                        pending.bytes += serde_json::to_vec(&rep).map_or(0, |b| b.len());
                        pending.item.probe = Some(rep);
                    }
                }
                _ = stop_requested(&mut stop) => {}
            }
            let _ = out.send(pending).await;
        });
    }
}

async fn upload(p: &Crawl, batch: &mut Vec<Pending>, abort: &Abort) {
    if batch.is_empty() {
        return;
    }
    let pending = std::mem::take(batch);
    let n = pending.len() as u64;
    let mut permits = Vec::with_capacity(pending.len());
    let mut items = Vec::with_capacity(pending.len());
    for x in pending {
        items.push(x.item);
        permits.push(x._permit);
    }
    send_results(&p.client, items, abort, &p.c, n).await;
    drop(permits);
}

/// Longest wait between two tries of the same upload.
const UPLOAD_RETRY_MAX: Duration = Duration::from_secs(60);

/// Send `items` until the server has taken them. A transient failure (network, 408, 429,
/// 5xx) is retried with backoff: never given up while running (the hashes' server leases
/// would expire for nothing), and the buffer fills meanwhile so fetching waits. A refused
/// body (D113: 413 too large, 400, any other 4xx) would be refused again: it is split in
/// halves, and a single refused item is dropped with a warning (its server lease expires and
/// the hash comes due again there), so one bad item can never stall the uploads.
async fn send_results(client: &Client, items: Vec<DhtResult>, abort: &Abort, c: &Counters, n: u64) {
    // Pieces still to send, the next one last.
    let mut todo: Vec<Vec<DhtResult>> = vec![items];
    while let Some(items) = todo.pop() {
        let body = DhtResults { items };
        add(&c.upload_requests, 1);
        let mut wait = Duration::from_secs(1);
        loop {
            match client.post::<_, DhtAck>(DHT_RESULTS, &body).await {
                Ok(a) => {
                    add(&c.uploaded, body.items.len() as u64);
                    add(&c.server_stored, u64::from(a.stored));
                    add(&c.server_not_new, u64::from(a.not_new));
                    add(&c.server_blocked, u64::from(a.blocked));
                    add(&c.server_rejected, u64::from(a.rejected));
                    add(&c.server_ignored, u64::from(a.ignored));
                    break;
                }
                Err(e) if e.is_fatal() => {
                    abort.fatal(&e);
                    return;
                }
                Err(e) if !e.is_retryable() => {
                    add(&c.upload_errors, 1);
                    let mut items = body.items;
                    if items.len() > 1 {
                        tracing::warn!(
                            "worker: upload of {} results refused: {e}; sending it in halves",
                            items.len()
                        );
                        let second = items.split_off(items.len() / 2);
                        todo.push(second);
                        todo.push(items);
                    } else {
                        add(&c.upload_dropped, items.len() as u64);
                        tracing::warn!(
                            "worker: result for {} refused by the server, dropped: {e}",
                            items.first().map_or("?", |i| i.sighted.as_str())
                        );
                    }
                    break;
                }
                Err(e) => {
                    add(&c.upload_errors, 1);
                    if get(&c.upload_errors).is_multiple_of(20) || wait.as_secs() <= 1 {
                        tracing::warn!("worker: upload: {e}; retrying");
                    }
                    if abort.stopping() && wait >= Duration::from_secs(8) {
                        tracing::warn!(
                            "worker: stopping with {n} results not uploaded (their leases expire)"
                        );
                        return;
                    }
                    tokio::time::sleep(wait).await;
                    wait = (wait * 2).min(UPLOAD_RETRY_MAX);
                }
            }
        }
    }
}

/// Batches results into uploads (≤ `max_results` items, ≤ 4 MiB of dicts, or 2 s). Runs until
/// every sender is gone, so results finished before a stop are still sent.
async fn uploader(p: Arc<Crawl>, mut rx: mpsc::Receiver<Pending>, abort: Abort) {
    let mut batch: Vec<Pending> = Vec::new();
    let mut bytes = 0usize;
    let mut flush_at = Instant::now() + UPLOAD_WAIT;
    loop {
        tokio::select! {
            r = rx.recv() => match r {
                Some(x) => {
                    if batch.is_empty() {
                        flush_at = Instant::now() + UPLOAD_WAIT;
                    }
                    if !batch.is_empty() && bytes + x.bytes > UPLOAD_BYTES {
                        upload(&p, &mut batch, &abort).await;
                        bytes = 0;
                    }
                    bytes += x.bytes;
                    batch.push(x);
                    p.c.upload_backlog.store(batch.len() as u64, Ordering::Relaxed);
                    if batch.len() >= p.max_results {
                        upload(&p, &mut batch, &abort).await;
                        bytes = 0;
                    }
                }
                None => break,
            },
            _ = tokio::time::sleep_until(flush_at), if !batch.is_empty() => {
                upload(&p, &mut batch, &abort).await;
                bytes = 0;
            }
        }
        p.c.upload_backlog
            .store(batch.len() as u64, Ordering::Relaxed);
    }
    upload(&p, &mut batch, &abort).await;
}

/// Sampling share and brakes: queue backpressure (D62) and the governor (D100).
async fn control(p: Arc<Crawl>, dht: Dht, mut stop: watch::Receiver<bool>) {
    let high = (p.opts.queue_cap / 2).clamp(1, QUEUE_HIGH);
    let low = high / 2;
    let mut queue_paused = false;
    loop {
        tokio::select! {
            _ = tokio::time::sleep(CONTROL_EVERY) => {}
            _ = stop_requested(&mut stop) => return,
        }
        let len = p.queue.lock().unwrap().len();
        if queue_paused && len <= low {
            queue_paused = false;
        } else if !queue_paused && len >= high {
            queue_paused = true;
        }
        let pressure = p.governor.pressure();
        let paused = queue_paused || pressure;
        if paused {
            add(&p.c.paused_ms, CONTROL_EVERY.as_millis() as u64);
        }
        let share = if paused { 0.0 } else { 1.0 };
        if p.discover && dht.sample_share() != share {
            dht.set_sample_share(share);
        }
        if pressure || p.governor.conntrack_full() {
            let until = Instant::now() + PRESSURE_BRAKE;
            dht.brake_until(until);
            p.fetcher.brake_until(until);
        }
    }
}

fn save_state(dir: &std::path::Path, dht: &Dht, ports: &[u16]) {
    if let Err(e) = state::save(
        dir,
        dht.identity().as_ref(),
        ports,
        &dht.nodes_to_save(1000),
    ) {
        tracing::warn!("worker: saving DHT state: {e}");
    }
}

pub struct CrawlSetup {
    pub opts: DhtOptions,
    pub state_dir: PathBuf,
    pub upload_buffer: usize,
    pub max_results: usize,
    /// Header probes (D117): None = off (the default; never on the main server).
    pub probe: Option<ProbeConfig>,
    /// `MAGNETAR_DHT_DISCOVER` (D118): false = no sampler, no triage.
    pub discover: bool,
    /// `MAGNETAR_PROBE_BACKFILL` (D118): lease indexed torrents and probe them (needs `probe`).
    pub backfill: bool,
    /// The server's lease length (hello), for the feeder's view of when a lease ends.
    pub lease_secs: u64,
}

/// Lease backfill hashes while fewer than half of [`Backfill::cap`] are in the worker, and
/// queue them for peer collection like triaged hashes (D118). No lease under a brake or
/// memory / conntrack pressure. A hash whose lease ended while it was still queued (an
/// overflowing queue) is forgotten here; the server hands it out again.
async fn backfill_feeder(p: Arc<Crawl>, abort: Abort, mut stop: watch::Receiver<bool>) {
    let Some(b) = &p.backfill else { return };
    loop {
        let now = Instant::now();
        b.out.lock().unwrap().retain(|_, until| *until > now);
        let outstanding = b.out.lock().unwrap().len();
        let busy = outstanding * 2 > b.cap
            || p.fetcher.braked()
            || p.fetcher.brake_level() > 0
            || p.governor.pressure()
            || p.governor.conntrack_full();
        let pause = if busy {
            BACKFILL_TICK
        } else {
            let want = (b.cap - outstanding) as u32;
            match p
                .client
                .post::<_, ProbeLease>(PROBE_LEASE, &LeaseRequest { max: want })
                .await
            {
                Ok(l) => {
                    let ttl = (l.expires_at - magnetar_core::now()).max(1) as u64;
                    let until = Instant::now() + Duration::from_secs(ttl);
                    let mut n = 0u64;
                    for h in l.infohashes.iter().filter_map(|h| parse_hash(h)) {
                        if b.out.lock().unwrap().insert(h, until).is_some() {
                            continue;
                        }
                        n += 1;
                        p.enqueue(FetchItem {
                            info_hash: h,
                            via: Via::GetPeers,
                            source: None,
                            peer: None,
                            retry: false,
                            queued_at: Instant::now(),
                        });
                    }
                    add(&b.leased, n);
                    match (n, l.filling) {
                        (0, true) => BACKFILL_TICK,
                        (0, false) => BACKFILL_IDLE,
                        _ => Duration::ZERO,
                    }
                }
                Err(e) if e.is_fatal() => {
                    abort.fatal(&e);
                    return;
                }
                Err(e) if !e.is_retryable() => {
                    add(&b.lease_errors, 1);
                    tracing::warn!(
                        "worker: probe backfill lease refused ({e}): the server may be older \
                         than this worker; trying again in {} s",
                        BACKFILL_OLD_SERVER.as_secs()
                    );
                    BACKFILL_OLD_SERVER
                }
                Err(e) => {
                    if get(&b.lease_errors).is_multiple_of(20) {
                        tracing::warn!("worker: probe backfill lease: {e}");
                    }
                    add(&b.lease_errors, 1);
                    BACKFILL_RETRY
                }
            }
        };
        tokio::select! {
            _ = tokio::time::sleep(pause.max(Duration::from_millis(50))) => {}
            _ = stop_requested(&mut stop) => return,
        }
    }
}

/// Give back the leased hashes still in the worker at a stop (D118).
async fn backfill_release(p: &Crawl) {
    let Some(b) = &p.backfill else { return };
    let left: Vec<String> = b
        .out
        .lock()
        .unwrap()
        .drain()
        .map(|(h, _)| hex::encode(h))
        .collect();
    if left.is_empty() {
        return;
    }
    let n = left.len() as u64;
    let r = tokio::time::timeout(
        Duration::from_secs(10),
        p.client
            .post::<_, Ack>(PROBE_RELEASE, &Release { infohashes: left }),
    )
    .await;
    match r {
        Ok(Ok(_)) => add(&b.released, n),
        Ok(Err(e)) => tracing::warn!("worker: probe backfill release: {e} (the leases expire)"),
        Err(_) => tracing::warn!("worker: probe backfill release timed out (the leases expire)"),
    }
}

/// Start the crawler; it runs until `stop` and returns its report.
pub async fn start(
    setup: CrawlSetup,
    client: Client,
    governor: Arc<Governor>,
    abort: Abort,
    stop: watch::Receiver<bool>,
    engine_tweak: impl FnOnce(&mut EngineConfig),
) -> anyhow::Result<(Arc<Crawl>, tokio::task::JoinHandle<CrawlReport>)> {
    let opts = setup.opts;
    let mut cfg = EngineConfig::new(opts.mode);
    cfg.ports = opts.ports.clone();
    cfg.qps = opts.qps;
    cfg.pool_cap = opts.pool_cap;
    cfg.lookup_timeout = opts.lookup_timeout;
    cfg.pacer = opts.pacer.clone();
    // D118: no new finds: no BEP 51 sampler at all.
    cfg.sample &= setup.discover;
    let (identity, nodes) = state::load(&setup.state_dir, &opts.ports);
    cfg.identity = identity;
    cfg.known_nodes = nodes;
    engine_tweak(&mut cfg);
    if cfg.bootstrap.is_empty() && !cfg.allow_loopback {
        cfg.bootstrap = crate::dht::resolve_bootstrap().await;
    }
    anyhow::ensure!(
        !cfg.bootstrap.is_empty() || !cfg.known_nodes.is_empty(),
        "DHT: no bootstrap router resolved and no saved node"
    );
    let (dht, rx) = Dht::start(cfg.clone())
        .await
        .map_err(|e| anyhow::anyhow!("DHT: binding UDP {:?}: {e}", opts.ports))?;
    dht.set_sample_priority(opts.sample_priority);
    tracing::info!(
        "worker DHT {:?}: {} node(s) on UDP {:?}, {} q/s, pool {}, {} fetches, TCP {}/s ≤ {}{}",
        opts.mode,
        dht.virtual_nodes(),
        dht.local_ports(),
        opts.qps,
        opts.pool_cap,
        opts.fetch_concurrency,
        opts.tcp_rate,
        opts.tcp_max,
        if cfg.identity.is_some() {
            ", saved identity"
        } else {
            ""
        }
    );
    let fetcher = Fetcher::new(FetchConfig {
        metadata_max: opts.metadata_max,
        // In flight: room for a few maximal dicts, at most the server's 256 MiB.
        inflight_max: (opts.metadata_max * 4).clamp(8 << 20, 256 << 20),
        decode_parallel: 2,
        connects_per_sec: opts.tcp_rate,
        max_connections: opts.tcp_max,
        allow_loopback: cfg.allow_loopback,
        max_peers: opts.peers_per_hash.max(1),
        keep_raw: true,
        // D117: the fast bit, HAVE_NONE, early INTERESTED and the hand-off only with probes;
        // without them every metadata connection is byte for byte what it was.
        probe: setup.probe.is_some(),
        ..FetchConfig::default()
    });
    let prober = setup.probe.map(|cfg| {
        tracing::info!(
            "worker header probes on (D117): {} per min, {} at once, {} peers, ≤ {} KiB per \
             torrent, whole pieces ≤ {} KiB, upload {}",
            cfg.per_min,
            cfg.parallel,
            cfg.peers,
            cfg.max_bytes / 1024,
            cfg.whole_piece_max / 1024,
            if cfg.upload {
                "ON (sharing verified pieces)"
            } else {
                "off"
            }
        );
        Prober::new(cfg, fetcher.clone())
    });
    // At most twice the parallel probes in the worker at once, within half the queue.
    let backfill = match (&prober, setup.backfill) {
        (Some(pr), true) => {
            let cap = (pr.config().parallel * 2)
                .clamp(4, crate::probe_backfill::LEASE_MAX)
                .min((opts.queue_cap / 2).max(1));
            tracing::info!(
                "worker probe backfill on (D118): up to {cap} leased torrents at once{}",
                if setup.discover {
                    ""
                } else {
                    ", DHT discovery off"
                }
            );
            Some(Backfill::new(cap))
        }
        _ => None,
    };
    let units = u32::try_from((setup.upload_buffer / 1024).max(64)).unwrap_or(u32::MAX >> 4);
    let own_hub = opts.health.is_none();
    let hub = opts
        .health
        .clone()
        .unwrap_or_else(|| health::HealthHub::new(opts.pacer.clone()));
    let p = Arc::new(Crawl {
        client,
        queue: Mutex::new(FetchQueue::new(opts.queue_cap)),
        notify: Notify::new(),
        fetcher,
        c: Counters::default(),
        health: hub.stats.clone(),
        governor,
        buffer: Arc::new(Semaphore::new(units as usize)),
        buffer_units: units,
        screen_slots: Semaphore::new(2),
        dht: Mutex::new(Some(dht.clone())),
        snap: Mutex::default(),
        max_results: setup.max_results.clamp(1, crate::fleet::MAX_RESULTS),
        opts,
        prober,
        discover: setup.discover,
        backfill,
    });
    let started = Instant::now();
    let state_dir = setup.state_dir;
    let p2 = p.clone();
    let handle = tokio::spawn(async move {
        let p = p2;
        let (int_tx, int_rx) = watch::channel(false);
        let mut tasks = JoinSet::new();
        let (batch_tx, batch_rx) = mpsc::channel::<Vec<Sighting>>(4);
        let triage = tokio::spawn(triage_task(
            p.clone(),
            batch_rx,
            abort.clone(),
            int_rx.clone(),
        ));
        tasks.spawn(triage_loop(p.clone(), rx, batch_tx, int_rx.clone()));
        let workers = p.opts.fetch_concurrency.max(1);
        let (jobs_tx, jobs_rx) = mpsc::channel::<Job>(workers);
        let jobs_rx = Arc::new(tokio::sync::Mutex::new(jobs_rx));
        let (out_tx, out_rx) = mpsc::channel::<Pending>(workers * 2 + 64);
        let up = tokio::spawn(uploader(p.clone(), out_rx, abort.clone()));
        for _ in 0..workers {
            tasks.spawn(fetch_worker(
                p.clone(),
                jobs_rx.clone(),
                out_tx.clone(),
                int_rx.clone(),
            ));
        }
        for _ in 0..p.opts.peer_lookups.max(1) {
            tasks.spawn(collector(
                p.clone(),
                dht.clone(),
                jobs_tx.clone(),
                out_tx.clone(),
                int_rx.clone(),
            ));
        }
        drop(jobs_tx);
        drop(out_tx);
        // Under the worker's probe while the crawl lasts, or our own (D109).
        let _attached = hub.attach(dht.clone(), p.fetcher.clone());
        if own_hub {
            tasks.spawn(health::health_loop(
                p.opts.health_targets.clone(),
                hub.clone(),
                int_rx.clone(),
            ));
        }
        tasks.spawn(control(p.clone(), dht.clone(), int_rx.clone()));
        if p.backfill.is_some() {
            tasks.spawn(backfill_feeder(p.clone(), abort.clone(), int_rx.clone()));
        }
        let ports = p.opts.ports.clone();
        {
            let (dht, dir, ports, mut stop) = (
                dht.clone(),
                state_dir.clone(),
                ports.clone(),
                int_rx.clone(),
            );
            tasks.spawn(async move {
                loop {
                    tokio::select! {
                        _ = tokio::time::sleep(PERSIST_EVERY) => save_state(&dir, &dht, &ports),
                        _ = stop_requested(&mut stop) => return,
                    }
                }
            });
        }
        let mapper = p.opts.upnp.clone().map(|cfg| {
            let wanted: Vec<(upnp::Protocol, u16)> = dht
                .local_ports()
                .into_iter()
                .filter(|port| *port != 0)
                .map(|port| (upnp::Protocol::Udp, port))
                .collect();
            tokio::spawn(upnp::upnp_loop(
                cfg,
                wanted,
                upnp::Status::default(),
                int_rx.clone(),
            ))
        });
        let mut stop = stop;
        stop_requested(&mut stop).await;
        let _ = int_tx.send(true);
        let deadline = Instant::now() + Duration::from_secs(4);
        while let Ok(Some(_)) = tokio::time::timeout_at(deadline, tasks.join_next()).await {}
        tasks.abort_all();
        while tasks.join_next().await.is_some() {}
        let _ = tokio::time::timeout(Duration::from_secs(5), triage).await;
        // Every result sender is gone: the uploader sends what is left and ends.
        let _ = tokio::time::timeout(Duration::from_secs(20), up).await;
        backfill_release(&p).await;
        if let Some(m) = mapper {
            let _ = tokio::time::timeout(Duration::from_secs(3), m).await;
        }
        save_state(&state_dir, &dht, &ports);
        let rep = report(&p, &dht, started);
        *p.dht.lock().unwrap() = None;
        dht.shutdown().await;
        rep
    });
    Ok((p, handle))
}

fn report(p: &Crawl, dht: &Dht, started: Instant) -> CrawlReport {
    let secs = started.elapsed().as_secs_f64().max(0.001);
    let e = dht.stats();
    CrawlReport {
        mode: match p.opts.mode {
            Mode::Polite => "polite".into(),
            Mode::Aggressive => "aggressive".into(),
        },
        ports: dht.local_ports(),
        seconds: secs,
        sightings: get(&p.c.sightings),
        unique_hashes: get(&p.c.unique),
        triaged: get(&p.c.triaged),
        queued_by_server: get(&p.c.queued),
        known_at_server: get(&p.c.triage_known),
        blocked_at_server: get(&p.c.triage_blocked),
        triage_errors: get(&p.c.triage_errors),
        triage_dropped: get(&p.c.triage_dropped),
        collected: get(&p.c.collected),
        with_peers: get(&p.c.with_peers),
        no_peers: get(&p.c.no_peers),
        verified: get(&p.c.verified),
        blocked_here: get(&p.c.blocked_here),
        uploaded: get(&p.c.uploaded),
        upload_requests: get(&p.c.upload_requests),
        upload_errors: get(&p.c.upload_errors),
        upload_dropped: get(&p.c.upload_dropped),
        server_stored: get(&p.c.server_stored),
        server_not_new: get(&p.c.server_not_new),
        server_blocked: get(&p.c.server_blocked),
        server_rejected: get(&p.c.server_rejected),
        server_ignored: get(&p.c.server_ignored),
        sampling_paused_secs: get(&p.c.paused_ms) as f64 / 1000.0,
        queries_per_sec: e.queries_sent as f64 / secs,
        answered_share: if e.queries_sent > 0 {
            e.answered as f64 / e.queries_sent as f64
        } else {
            0.0
        },
        fetch: p.fetcher.stats(),
        health: p.health.lock().unwrap().clone(),
        engine: e,
        probe: p.prober.as_ref().map(probe_state),
        discover: p.discover,
        backfill: p.backfill.as_ref().map(Backfill::state),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use axum::Json;
    use axum::http::StatusCode;
    use axum::routing::post;

    /// A loopback server that answers 503 to the first request, refuses more than two items
    /// with 413 and an item for `bad` with 400, and records what it took.
    async fn picky_server() -> (String, Arc<Mutex<Vec<String>>>) {
        let took: Arc<Mutex<Vec<String>>> = Arc::default();
        let first = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let t = took.clone();
        let app = axum::Router::new().route(
            DHT_RESULTS,
            post(move |Json(r): Json<DhtResults>| {
                let t = t.clone();
                let first = first.clone();
                async move {
                    if first.swap(false, Ordering::Relaxed) {
                        return Err(StatusCode::SERVICE_UNAVAILABLE);
                    }
                    if r.items.len() > 2 {
                        return Err(StatusCode::PAYLOAD_TOO_LARGE);
                    }
                    if r.items.iter().any(|i| i.sighted == "bad") {
                        return Err(StatusCode::BAD_REQUEST);
                    }
                    t.lock()
                        .unwrap()
                        .extend(r.items.iter().map(|i| i.sighted.clone()));
                    Ok(Json(DhtAck {
                        stored: r.items.len() as u32,
                        ..Default::default()
                    }))
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{addr}"), took)
    }

    fn item(sighted: &str) -> DhtResult {
        DhtResult {
            sighted: sighted.into(),
            via: "sample".into(),
            peers_seen: 0,
            outcome: DhtOutcome::Private,
            probe: None,
        }
    }

    /// D113: a 413 splits the upload, a 400 on a single item drops it, a 503 is retried; the
    /// upload finishes instead of resending a refused body forever.
    #[tokio::test]
    async fn refused_uploads_are_split_then_dropped_not_retried_forever() {
        let (base, took) = picky_server().await;
        let client = Client::new(&base, "w1-token-aaaaaaaaaaaaaaaaaaaaaaaa").unwrap();
        let (abort, _rx) = Abort::new();
        let c = Counters::default();
        let items: Vec<DhtResult> = ["a", "b", "bad", "c", "d"].into_iter().map(item).collect();
        tokio::time::timeout(
            Duration::from_secs(20),
            send_results(&client, items, &abort, &c, 5),
        )
        .await
        .expect("a refused body must not be resent forever");
        let mut got = took.lock().unwrap().clone();
        got.sort();
        assert_eq!(got, ["a", "b", "c", "d"]);
        assert_eq!(get(&c.uploaded), 4);
        assert_eq!(get(&c.upload_dropped), 1);
        assert_eq!(get(&c.server_stored), 4);
        assert!(!abort.stopping(), "a refused body does not stop the worker");
    }
}

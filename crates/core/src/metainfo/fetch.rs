//! Bounded BEP 9 metadata fetcher over TCP (docs/DHT.md §4, D56): peers → verified info dict.
//!
//! Per peer: connect (3 s) → BEP 3 handshake with the BEP 10 bit → the peer's handshake (it must
//! set the bit too) → our extended handshake → the peer's (5 s from the connect) →
//! every 16 KiB block requested at once → blocks checked and assembled → hash of the raw bytes
//! (whole peer 10 s). Per hash: ≤ 12 peers, ≤ 4 at once, 30 s. Global: ≤ 150 connection attempts/s and ≤ 350 open
//! connections (router benchmark, D66),
//! 256 MiB of metadata in flight, 4 decodes at once (each reserving 3 × its size more from a
//! decode budget of its own).
//!
//! Verification (BEP 52 "the encoded form as found", never a decode-encode round trip):
//! `sha1(raw) == hash` → v1 (hybrid when it also carries `meta version` 2); else
//! `sha256(raw)[..20] == hash` → a truncated v2 hash: hybrid when the dict has `pieces` (stored
//! under its SHA-1), else v2-only. Anything else fails that peer.
//!
//! Peers are untrusted: every length is checked, unknown messages are skipped within a byte
//! budget, and nothing here logs a name or path (docs/DHT.md §7.1).

use std::net::SocketAddrV4;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use futures::stream::{FuturesUnordered, StreamExt};
use serde::Serialize;
use sha2::Digest;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::Semaphore;
use tokio::time::Instant;

use super::info::{InfoError, InfoSummary, parse_info};
use super::wire::{self, ExtHandshake, MetadataMsg, WireError};
use crate::dht::addr::AddrFilter;
use crate::dht::limiter::Pace;

/// Largest `extended` message we read (a `data` message is ~50 B of dict + 16 KiB; extended
/// handshakes are well under 1 KiB in practice).
const MAX_EXTENDED: usize = 64 * 1024;
/// Bytes of other messages (bitfield, have, …) skipped per peer before giving up on it.
const DISCARD_BUDGET: u64 = 4 * 1024 * 1024;
/// Metadata requests from the peer we answer with `reject` before giving up on it.
const MAX_REJECTS_SENT: usize = 64;
/// Budget accounting unit (the semaphore counts KiB so 256 MiB fits its u32 permits).
const UNIT: usize = 1024;

#[derive(Clone, Debug)]
pub struct FetchConfig {
    pub connect_timeout: Duration,
    /// From the connect to the peer's extended handshake.
    pub handshake_timeout: Duration,
    /// Whole peer session.
    pub peer_timeout: Duration,
    /// Whole hash, every peer included.
    pub hash_budget: Duration,
    pub max_peers: usize,
    pub parallel: usize,
    /// Largest `metadata_size` accepted (BEP 9 sets no limit).
    pub metadata_max: usize,
    /// Global bytes of metadata buffered at once.
    pub inflight_max: usize,
    /// New TCP connection attempts per second, all fetches (each SYN is a router NAT entry).
    pub connects_per_sec: u32,
    /// TCP connections open at once, all fetches.
    pub max_connections: usize,
    pub decode_parallel: usize,
    /// `v` in our extended handshake.
    pub client: String,
    /// Test seam: lets 127.0.0.0/8 peers through. Never set from configuration.
    pub allow_loopback: bool,
}

impl Default for FetchConfig {
    fn default() -> Self {
        Self {
            connect_timeout: Duration::from_secs(3),
            handshake_timeout: Duration::from_secs(5),
            peer_timeout: Duration::from_secs(10),
            hash_budget: Duration::from_secs(30),
            max_peers: 12,
            parallel: 4,
            metadata_max: 16 * 1024 * 1024,
            inflight_max: 256 * 1024 * 1024,
            // Router benchmark on the user's neufbox (docs/research/router-bench.md): 200
            // attempts/s safe, 400 breaks; ~440 open connections held. ~30 % under the knees.
            connects_per_sec: 150,
            max_connections: 350,
            decode_parallel: 4,
            client: format!("Magnetar {}", env!("CARGO_PKG_VERSION")),
            allow_loopback: false,
        }
    }
}

/// Why one peer gave nothing. Ordered by how far the session got.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum PeerError {
    /// Refused before any packet: non-global address (SSRF rule, docs/DHT.md §3.8).
    Filtered,
    Connect,
    Timeout,
    Closed,
    Handshake,
    NoExtensions,
    NoUtMetadata,
    /// The peer has no metadata to give (no or zero `metadata_size`).
    NoMetadataSize,
    Oversize,
    Protocol,
    Reject,
    BadBlock,
    HashMismatch,
}

impl PeerError {
    pub fn code(self) -> &'static str {
        match self {
            PeerError::Filtered => "filtered",
            PeerError::Connect => "connect",
            PeerError::Timeout => "timeout",
            PeerError::Closed => "closed",
            PeerError::Handshake => "handshake",
            PeerError::NoExtensions => "no_extensions",
            PeerError::NoUtMetadata => "no_ut_metadata",
            PeerError::NoMetadataSize => "no_metadata",
            PeerError::Oversize => "oversize",
            PeerError::Protocol => "protocol",
            PeerError::Reject => "reject",
            PeerError::BadBlock => "bad_block",
            PeerError::HashMismatch => "hash_mismatch",
        }
    }
}

impl From<std::io::Error> for PeerError {
    fn from(e: std::io::Error) -> Self {
        match e.kind() {
            std::io::ErrorKind::TimedOut => PeerError::Timeout,
            _ => PeerError::Closed,
        }
    }
}

/// Which hash the bytes matched (see the module doc).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Identity {
    /// SHA-1 matched; `v2` = SHA-256 when the dict is hybrid (`meta version` 2 + `pieces`).
    V1 { v2: Option<[u8; 32]> },
    /// The sighted hash was a truncated SHA-256 and the dict has `pieces`: stored under `v1`.
    Hybrid { v1: [u8; 20], v2: [u8; 32] },
    /// Truncated SHA-256 without `pieces`: no v1 identity.
    V2Only,
}

/// A verified, parsed info dict.
#[derive(Clone, Debug)]
pub struct Fetched {
    /// The hash it was asked for (the sighted one).
    pub sighted: [u8; 20],
    /// The hash to store it under: the sighted one, or the SHA-1 of a hybrid sighted by v2.
    pub v1: [u8; 20],
    pub v2: Option<[u8; 32]>,
    pub info: InfoSummary,
    pub metadata_size: usize,
    /// Peers connected to for this hash (successful one included).
    pub peers_tried: usize,
}

/// Final result for a hash with verified bytes.
#[derive(Clone, Debug)]
pub enum Outcome {
    /// v1 or hybrid, parsed: storable once classified and screened.
    Ok(Box<Fetched>),
    /// BEP 27 `private = 1`: not stored (its magnet is useless outside the private tracker).
    Private,
    /// Pure v2: no v1 identity for the data model.
    V2Only,
    /// Bytes matched the hash but the dict is unusable.
    Invalid(InfoError),
}

impl Outcome {
    pub fn code(&self) -> &'static str {
        match self {
            Outcome::Ok(_) => "ok",
            Outcome::Private => "private",
            Outcome::V2Only => "v2_only",
            Outcome::Invalid(e) => e.code(),
        }
    }
}

/// No peer produced verified metadata.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FetchFailure {
    /// The furthest any peer got.
    pub error: PeerError,
    /// Every peer that sent a usable extended handshake claimed more than `metadata_max`.
    pub oversize: bool,
    pub peers_tried: usize,
}

impl FetchFailure {
    pub fn code(&self) -> &'static str {
        if self.peers_tried == 0 {
            "no_peers"
        } else {
            self.error.code()
        }
    }
}

/// Counters for reports (never addresses or names).
#[derive(Default)]
struct Counters {
    hashes: AtomicU64,
    ok: AtomicU64,
    hybrid: AtomicU64,
    private: AtomicU64,
    v2_only: AtomicU64,
    invalid: AtomicU64,
    failed: AtomicU64,
    oversize: AtomicU64,
    peers_tried: AtomicU64,
    connected: AtomicU64,
    peer_errors: [AtomicU64; 13],
    tcp_bytes_in: AtomicU64,
    tcp_bytes_out: AtomicU64,
    metadata_bytes: AtomicU64,
    ok_ms_total: AtomicU64,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct FetchStats {
    pub hashes: u64,
    pub ok: u64,
    pub hybrid: u64,
    pub private: u64,
    pub v2_only: u64,
    pub invalid: u64,
    pub failed: u64,
    pub oversize: u64,
    pub peers_tried: u64,
    /// TCP connections that completed.
    pub connected: u64,
    /// Peer sessions that ended in each error (`PeerError::code`).
    pub peer_errors: Vec<(String, u64)>,
    pub tcp_bytes_in: u64,
    pub tcp_bytes_out: u64,
    /// Verified metadata bytes (successful fetches only).
    pub metadata_bytes: u64,
    /// Mean wall time of a successful hash, ms.
    pub ok_mean_ms: f64,
}

const PEER_ERRORS: [PeerError; 13] = [
    PeerError::Filtered,
    PeerError::Connect,
    PeerError::Timeout,
    PeerError::Closed,
    PeerError::Handshake,
    PeerError::NoExtensions,
    PeerError::NoUtMetadata,
    PeerError::NoMetadataSize,
    PeerError::Oversize,
    PeerError::Protocol,
    PeerError::Reject,
    PeerError::BadBlock,
    PeerError::HashMismatch,
];

fn add(a: &AtomicU64, n: u64) {
    a.fetch_add(n, Ordering::Relaxed);
}

struct Inner {
    cfg: FetchConfig,
    peer_id: [u8; 20],
    filter: AddrFilter,
    /// Metadata bytes in flight (downloading or decoding), in KiB.
    budget: Semaphore,
    decode: Semaphore,
    /// Decode working memory (3 × the metadata size per decode), in KiB. Separate from
    /// `budget` so downloads holding the whole budget cannot starve the decodes that would
    /// release it.
    decode_budget: Semaphore,
    connects: Mutex<Pace>,
    /// Open TCP connections.
    conns: Semaphore,
    /// While in the future, connection attempts are paced at half the rate (D66).
    braked_until: Mutex<Option<Instant>>,
    c: Counters,
}

/// Shared by every fetch of a crawler: holds the global bounds.
#[derive(Clone)]
pub struct Fetcher {
    inner: Arc<Inner>,
}

/// Azureus-style BEP 20 peer id: `-MG0300-` + 12 random bytes (docs/DHT.md §3.3).
pub fn new_peer_id() -> [u8; 20] {
    let mut id = [0u8; 20];
    id[..8].copy_from_slice(b"-MG0300-");
    for b in &mut id[8..] {
        *b = rand::random();
    }
    id
}

/// What the bytes are, by hash (the dict is not parsed yet).
enum Match {
    Sha1,
    TruncatedSha256 { v1: [u8; 20], v2: [u8; 32] },
}

fn match_hash(info_hash: &[u8; 20], raw: &[u8]) -> Option<Match> {
    let v1 = sha1_smol::Sha1::from(raw).digest().bytes();
    if &v1 == info_hash {
        return Some(Match::Sha1);
    }
    let v2: [u8; 32] = sha2::Sha256::digest(raw).into();
    if &v2[..20] == info_hash {
        return Some(Match::TruncatedSha256 { v1, v2 });
    }
    None
}

/// Verify raw bytes against the sighted hash and parse them. None = hash mismatch.
pub fn verify_and_parse(
    info_hash: &[u8; 20],
    raw: &[u8],
) -> Option<(Identity, Result<InfoSummary, InfoError>)> {
    let m = match_hash(info_hash, raw)?;
    let parsed = parse_info(raw);
    let id = match (m, &parsed) {
        (Match::Sha1, Ok(s)) if s.meta_version == Some(2) && s.has_pieces => Identity::V1 {
            v2: Some(sha2::Sha256::digest(raw).into()),
        },
        (Match::Sha1, Ok(s)) if !s.has_pieces && s.has_file_tree => Identity::V2Only,
        (Match::Sha1, _) => Identity::V1 { v2: None },
        (Match::TruncatedSha256 { v1, v2 }, Ok(s)) if s.has_pieces => Identity::Hybrid { v1, v2 },
        (Match::TruncatedSha256 { .. }, _) => Identity::V2Only,
    };
    Some((id, parsed))
}

impl Fetcher {
    pub fn new(mut cfg: FetchConfig) -> Self {
        // A single fetch must always fit the in-flight budget.
        cfg.inflight_max = cfg.inflight_max.max(UNIT);
        cfg.metadata_max = cfg.metadata_max.clamp(1, cfg.inflight_max);
        let units = u32::try_from(cfg.inflight_max / UNIT).unwrap_or(u32::MAX >> 3);
        let decode_units = (cfg.metadata_max.div_ceil(UNIT) * 3)
            .saturating_mul(cfg.decode_parallel.max(1))
            .min(Semaphore::MAX_PERMITS);
        let now = Instant::now();
        Self {
            inner: Arc::new(Inner {
                peer_id: new_peer_id(),
                filter: AddrFilter::new(cfg.allow_loopback),
                budget: Semaphore::new(units as usize),
                decode: Semaphore::new(cfg.decode_parallel.max(1)),
                decode_budget: Semaphore::new(decode_units),
                connects: Mutex::new(Pace::new(cfg.connects_per_sec.max(1), now)),
                conns: Semaphore::new(cfg.max_connections.clamp(1, Semaphore::MAX_PERMITS)),
                braked_until: Mutex::new(None),
                c: Counters::default(),
                cfg,
            }),
        }
    }

    /// Halve the connection-attempt rate until `until` (router health probe, D66).
    pub fn brake_until(&self, until: Instant) {
        let mut b = self.inner.braked_until.lock().unwrap();
        if b.is_none_or(|u| u < until) {
            *b = Some(until);
        }
    }

    pub fn config(&self) -> &FetchConfig {
        &self.inner.cfg
    }

    pub fn stats(&self) -> FetchStats {
        let c = &self.inner.c;
        let g = |a: &AtomicU64| a.load(Ordering::Relaxed);
        let ok = g(&c.ok);
        FetchStats {
            hashes: g(&c.hashes),
            ok,
            hybrid: g(&c.hybrid),
            private: g(&c.private),
            v2_only: g(&c.v2_only),
            invalid: g(&c.invalid),
            failed: g(&c.failed),
            oversize: g(&c.oversize),
            peers_tried: g(&c.peers_tried),
            connected: g(&c.connected),
            peer_errors: PEER_ERRORS
                .iter()
                .zip(c.peer_errors.iter())
                .map(|(e, n)| (e.code().to_string(), g(n)))
                .filter(|(_, n)| *n > 0)
                .collect(),
            tcp_bytes_in: g(&c.tcp_bytes_in),
            tcp_bytes_out: g(&c.tcp_bytes_out),
            metadata_bytes: g(&c.metadata_bytes),
            ok_mean_ms: if ok > 0 {
                g(&c.ok_ms_total) as f64 / ok as f64
            } else {
                0.0
            },
        }
    }

    /// Fetch, verify and parse the info dict of `info_hash` from `peers` (tried in order, the
    /// first `max_peers`, `parallel` at once). Returns on the first verified dict.
    pub async fn fetch(
        &self,
        info_hash: [u8; 20],
        peers: &[SocketAddrV4],
    ) -> Result<Outcome, FetchFailure> {
        let started = Instant::now();
        add(&self.inner.c.hashes, 1);
        let r = tokio::time::timeout(
            self.inner.cfg.hash_budget,
            self.fetch_inner(info_hash, peers),
        )
        .await
        .unwrap_or(Err(FetchFailure {
            error: PeerError::Timeout,
            oversize: false,
            peers_tried: peers.len().min(self.inner.cfg.max_peers),
        }));
        let c = &self.inner.c;
        match &r {
            Ok(Outcome::Ok(f)) => {
                add(&c.ok, 1);
                add(&c.metadata_bytes, f.metadata_size as u64);
                add(&c.ok_ms_total, started.elapsed().as_millis() as u64);
                if f.v2.is_some() {
                    add(&c.hybrid, 1);
                }
            }
            Ok(Outcome::Private) => add(&c.private, 1),
            Ok(Outcome::V2Only) => add(&c.v2_only, 1),
            Ok(Outcome::Invalid(_)) => add(&c.invalid, 1),
            Err(f) if f.oversize => add(&c.oversize, 1),
            Err(_) => add(&c.failed, 1),
        }
        r
    }

    async fn fetch_inner(
        &self,
        info_hash: [u8; 20],
        peers: &[SocketAddrV4],
    ) -> Result<Outcome, FetchFailure> {
        let cfg = &self.inner.cfg;
        let mut todo = peers.iter().copied().take(cfg.max_peers);
        let mut running = FuturesUnordered::new();
        let mut tried = 0usize;
        let mut furthest: Option<PeerError> = None;
        let mut sized = 0usize; // peers that told a size with ut_metadata
        let mut oversized = 0usize;
        loop {
            while running.len() < cfg.parallel.max(1) {
                let Some(p) = todo.next() else { break };
                tried += 1;
                add(&self.inner.c.peers_tried, 1);
                running.push(self.peer(info_hash, p));
            }
            let Some(r) = running.next().await else { break };
            match r {
                Ok((raw, id, parsed)) => {
                    return Ok(self.outcome(info_hash, raw, id, parsed, tried));
                }
                Err(e) => {
                    if let Some(i) = PEER_ERRORS.iter().position(|x| *x == e) {
                        add(&self.inner.c.peer_errors[i], 1);
                    }
                    if e > PeerError::NoMetadataSize {
                        sized += 1;
                    }
                    if e == PeerError::Oversize {
                        oversized += 1;
                    }
                    furthest = Some(furthest.map_or(e, |f| f.max(e)));
                }
            }
        }
        Err(FetchFailure {
            error: furthest.unwrap_or(PeerError::Filtered),
            oversize: sized > 0 && oversized == sized,
            peers_tried: tried,
        })
    }

    fn outcome(
        &self,
        sighted: [u8; 20],
        raw_len: usize,
        id: Identity,
        parsed: Result<InfoSummary, InfoError>,
        tried: usize,
    ) -> Outcome {
        let info = match (id, parsed) {
            (Identity::V2Only, _) => return Outcome::V2Only,
            (_, Err(e)) => return Outcome::Invalid(e),
            (_, Ok(s)) => s,
        };
        if info.private {
            return Outcome::Private;
        }
        let (v1, v2) = match id {
            Identity::V1 { v2 } => (sighted, v2),
            Identity::Hybrid { v1, v2 } => (v1, Some(v2)),
            Identity::V2Only => return Outcome::V2Only,
        };
        Outcome::Ok(Box::new(Fetched {
            sighted,
            v1,
            v2,
            info,
            metadata_size: raw_len,
            peers_tried: tried,
        }))
    }

    /// One peer session. Returns the raw size, identity and parse result of verified bytes.
    async fn peer(
        &self,
        info_hash: [u8; 20],
        addr: SocketAddrV4,
    ) -> Result<(usize, Identity, Result<InfoSummary, InfoError>), PeerError> {
        if !self.inner.filter.allows(&addr) {
            return Err(PeerError::Filtered);
        }
        let cfg = &self.inner.cfg;
        // Global bounds (home router NAT, docs/DHT.md §4 step 7): open connections, then the
        // attempt pace (halved while the health probe brakes, D66).
        let _conn = self
            .inner
            .conns
            .acquire()
            .await
            .map_err(|_| PeerError::Closed)?;
        let now = Instant::now();
        let braked = self
            .inner
            .braked_until
            .lock()
            .unwrap()
            .is_some_and(|u| now < u);
        let at = self.inner.connects.lock().unwrap().reserve(now, braked);
        tokio::time::sleep_until(at).await;
        let stream = tokio::time::timeout(cfg.connect_timeout, TcpStream::connect(addr))
            .await
            .map_err(|_| PeerError::Connect)?
            .map_err(|_| PeerError::Connect)?;
        add(&self.inner.c.connected, 1);
        let _ = stream.set_nodelay(true);
        let connected = Instant::now();
        tokio::time::timeout(cfg.peer_timeout, self.session(stream, info_hash, connected))
            .await
            .map_err(|_| PeerError::Timeout)?
    }

    async fn session(
        &self,
        stream: TcpStream,
        info_hash: [u8; 20],
        connected: Instant,
    ) -> Result<(usize, Identity, Result<InfoSummary, InfoError>), PeerError> {
        let cfg = &self.inner.cfg;
        let c = &self.inner.c;
        let (rd, mut wr) = stream.into_split();
        let mut rd = BufReader::with_capacity(32 * 1024, rd);
        let hello = wire::handshake(&info_hash, &self.inner.peer_id);
        wr.write_all(&hello).await?;
        add(&c.tcp_bytes_out, hello.len() as u64);

        let hs_deadline = connected + cfg.handshake_timeout;
        let mut discard = DISCARD_BUDGET;
        let ext: ExtHandshake = tokio::time::timeout_at(hs_deadline, async {
            let mut b = [0u8; wire::HANDSHAKE_LEN];
            rd.read_exact(&mut b).await?;
            add(&c.tcp_bytes_in, b.len() as u64);
            wire::parse_handshake(&b, &info_hash).map_err(|e| match e {
                WireError::NoExtensions => PeerError::NoExtensions,
                _ => PeerError::Handshake,
            })?;
            // BEP 10: the extended handshake goes to peers that set the extension bit, so ours
            // is sent only once theirs is known to have it.
            let ours = wire::our_extended_handshake(&cfg.client);
            wr.write_all(&ours).await?;
            add(&c.tcp_bytes_out, ours.len() as u64);
            loop {
                if let Msg::Extended(p) = read_msg(&mut rd, &mut discard, &c.tcp_bytes_in).await?
                    && p.first() == Some(&wire::EXT_HANDSHAKE)
                {
                    return wire::parse_ext_handshake(&p[1..]).map_err(|_| PeerError::Protocol);
                }
            }
        })
        .await
        .map_err(|_| PeerError::Timeout)??;

        let their_id = ext.ut_metadata.ok_or(PeerError::NoUtMetadata)?;
        let size = match ext.metadata_size {
            None | Some(..=0) => return Err(PeerError::NoMetadataSize),
            Some(s) => usize::try_from(s).unwrap_or(usize::MAX),
        };
        if size > cfg.metadata_max {
            return Err(PeerError::Oversize);
        }
        // Reserve the buffer before allocating it (global in-flight budget).
        let units = u32::try_from(size.div_ceil(UNIT)).map_err(|_| PeerError::Oversize)?;
        let _reserved = self
            .inner
            .budget
            .acquire_many(units)
            .await
            .map_err(|_| PeerError::Closed)?;
        let n = wire::block_count(size);
        let mut req = Vec::with_capacity(n * 32);
        for piece in 0..n {
            req.extend_from_slice(&wire::metadata_request(their_id, piece as u32));
        }
        wr.write_all(&req).await?;
        add(&c.tcp_bytes_out, req.len() as u64);

        let mut raw = vec![0u8; size];
        let mut have = vec![false; n];
        let mut missing = n;
        let mut rejects = 0usize;
        while missing > 0 {
            let Msg::Extended(p) = read_msg(&mut rd, &mut discard, &c.tcp_bytes_in).await? else {
                continue;
            };
            if p.first() != Some(&wire::OUR_UT_METADATA) {
                continue; // another extension (ut_pex, a repeated handshake, …)
            }
            match wire::parse_metadata_msg(&p[1..]).map_err(|_| PeerError::Protocol)? {
                MetadataMsg::Data {
                    piece,
                    total_size,
                    block,
                } => {
                    if total_size.is_some_and(|t| usize::try_from(t).ok() != Some(size)) {
                        return Err(PeerError::BadBlock);
                    }
                    let i = usize::try_from(piece).map_err(|_| PeerError::BadBlock)?;
                    let want = wire::block_len(size, i).ok_or(PeerError::BadBlock)?;
                    if block.len() != want {
                        return Err(PeerError::BadBlock);
                    }
                    let start = i * wire::BLOCK;
                    raw[start..start + want].copy_from_slice(block);
                    if !have[i] {
                        have[i] = true;
                        missing -= 1;
                    }
                }
                MetadataMsg::Reject { .. } => return Err(PeerError::Reject),
                // BEP 9: a peer without the entire metadata "MUST respond with a reject message
                // to any metadata request". At most a few per session (flood: give up).
                MetadataMsg::Request { piece } => {
                    rejects += 1;
                    if rejects > MAX_REJECTS_SENT {
                        return Err(PeerError::Protocol);
                    }
                    let r = wire::metadata_reject(their_id, piece);
                    wr.write_all(&r).await?;
                    add(&c.tcp_bytes_out, r.len() as u64);
                }
                MetadataMsg::Unknown => {}
            }
        }
        drop(rd);
        drop(wr);

        // Verify + parse off the async threads, 4 at once, each holding 4 × its size.
        let _decode = self
            .inner
            .decode
            .acquire()
            .await
            .map_err(|_| PeerError::Closed)?;
        let _extra = self
            .inner
            .decode_budget
            .acquire_many(units.saturating_mul(3))
            .await
            .map_err(|_| PeerError::Closed)?;
        let res = tokio::task::spawn_blocking(move || verify_and_parse(&info_hash, &raw))
            .await
            .map_err(|_| PeerError::Closed)?;
        match res {
            Some((id, parsed)) => Ok((size, id, parsed)),
            None => Err(PeerError::HashMismatch),
        }
    }
}

enum Msg {
    KeepAlive,
    /// Payload of an `extended` message (starting with the extended id).
    Extended(Vec<u8>),
    Other,
}

/// Read one length-prefixed message. `extended` messages up to [`MAX_EXTENDED`] are returned;
/// others are skipped within the per-peer `discard` budget.
async fn read_msg<R: AsyncRead + Unpin>(
    r: &mut R,
    discard: &mut u64,
    bytes_in: &AtomicU64,
) -> Result<Msg, PeerError> {
    let len = r.read_u32().await? as usize;
    add(bytes_in, 4);
    if len == 0 {
        return Ok(Msg::KeepAlive);
    }
    let id = r.read_u8().await?;
    let body = len - 1;
    add(bytes_in, len as u64);
    if id == wire::MSG_EXTENDED {
        if body > MAX_EXTENDED || body == 0 {
            return Err(PeerError::Protocol);
        }
        let mut v = vec![0u8; body];
        r.read_exact(&mut v).await?;
        return Ok(Msg::Extended(v));
    }
    let body = body as u64;
    if body > *discard {
        return Err(PeerError::Protocol);
    }
    *discard -= body;
    let copied = tokio::io::copy(&mut r.take(body), &mut tokio::io::sink()).await?;
    if copied != body {
        return Err(PeerError::Closed);
    }
    Ok(Msg::Other)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verification_paths() {
        let v1_raw = b"d6:lengthi5e4:name1:a12:piece lengthi16384e6:pieces20:aaaaaaaaaaaaaaaaaaaae";
        let h1 = sha1_smol::Sha1::from(&v1_raw[..]).digest().bytes();
        let (id, p) = verify_and_parse(&h1, v1_raw).unwrap();
        assert_eq!(id, Identity::V1 { v2: None });
        assert!(p.is_ok());
        assert!(verify_and_parse(&[0; 20], v1_raw).is_none());

        // hybrid: sighted by SHA-1 → v2 also recorded; sighted by truncated SHA-256 → stored by SHA-1.
        let hy = b"d9:file treed1:ad0:d6:lengthi5eeee6:lengthi5e12:meta versioni2e4:name1:a12:piece lengthi16384e6:pieces20:aaaaaaaaaaaaaaaaaaaae";
        let s1 = sha1_smol::Sha1::from(&hy[..]).digest().bytes();
        let s2: [u8; 32] = sha2::Sha256::digest(hy).into();
        let (id, _) = verify_and_parse(&s1, hy).unwrap();
        assert_eq!(id, Identity::V1 { v2: Some(s2) });
        let t: [u8; 20] = s2[..20].try_into().unwrap();
        let (id, _) = verify_and_parse(&t, hy).unwrap();
        assert_eq!(id, Identity::Hybrid { v1: s1, v2: s2 });

        // pure v2 under its truncated hash
        let v2 =
            b"d9:file treed1:ad0:d6:lengthi5eeee12:meta versioni2e4:name1:a12:piece lengthi16384ee";
        let t2: [u8; 32] = sha2::Sha256::digest(v2).into();
        let (id, _) = verify_and_parse(&t2[..20].try_into().unwrap(), v2).unwrap();
        assert_eq!(id, Identity::V2Only);
    }

    #[test]
    fn peer_id_shape() {
        let id = new_peer_id();
        assert_eq!(&id[..8], b"-MG0300-");
    }

    #[test]
    fn failure_codes() {
        let f = FetchFailure {
            error: PeerError::Connect,
            oversize: false,
            peers_tried: 0,
        };
        assert_eq!(f.code(), "no_peers");
        assert!(PeerError::HashMismatch > PeerError::Connect);
    }
}

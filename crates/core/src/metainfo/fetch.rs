//! Bounded BEP 9 metadata fetcher over TCP (docs/DHT.md §4, D56): peers → verified info dict.
//!
//! Per peer: connect (3 s) → BEP 3 handshake with the BEP 10 bit → the peer's handshake (it must
//! set the bit too) → our extended handshake → the peer's (5 s from the connect) →
//! every 16 KiB block requested at once → blocks checked and assembled → hash of the raw bytes
//! (whole peer 10 s). Per hash: ≤ 12 peers, ≤ 4 at once, 30 s. Global: ≤ 150 connection attempts/s and ≤ 350 open
//! connections (router benchmark, D66),
//! 256 MiB of metadata in flight, 4 decodes at once, each priced before it runs (3 × its size
//! plus 64 B per bencode value, counted without allocating) and reserved from a decode budget
//! of its own (3 × `metadata_max` × 4); above `metadata_max / 8` values it is `too_complex`.
//!
//! Verification (BEP 52 "the encoded form as found", never a decode-encode round trip):
//! `sha1(raw) == hash` → v1 (hybrid when it also carries `meta version` 2); else
//! `sha256(raw)[..20] == hash` → a truncated v2 hash: hybrid when the dict has `pieces` (stored
//! under its SHA-1), else v2-only. Anything else fails that peer.
//!
//! Peers are untrusted: every length is checked, unknown messages are skipped within a byte
//! budget, and nothing here logs a name or path (docs/DHT.md §7.1).

use std::collections::{HashMap, VecDeque};
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
use crate::liveness::bencode;

/// Largest `extended` message we read (a `data` message is ~50 B of dict + 16 KiB; extended
/// handshakes are well under 1 KiB in practice).
const MAX_EXTENDED: usize = 64 * 1024;
/// Bytes of other messages (bitfield, have, …) skipped per peer before giving up on it.
const DISCARD_BUDGET: u64 = 4 * 1024 * 1024;
/// Metadata requests from the peer we answer with `reject` before giving up on it.
const MAX_REJECTS_SENT: usize = 64;
/// Budget accounting unit (the semaphore counts KiB so 256 MiB fits its u32 permits).
const UNIT: usize = 1024;
/// Bytes one decoded bencode value may cost (`Value` is 32 B, a dict entry 56 B, `Vec` growth
/// up to 2×), used to price a decode before it runs.
const VALUE_COST: usize = 64;
/// Decode cost of the raw bytes themselves: the input, the copied strings, the names.
const RAW_COST: usize = 3;

/// Most values a decode may build: `metadata_max / 8` (2 M at 16 MiB, ~300 k files), at least
/// 64 k. Above it the dict is `invalid` (`too_complex`) without being decoded.
pub fn max_values(metadata_max: usize) -> usize {
    (metadata_max / 8).max(64 * 1024)
}

/// Bytes a decode of `raw_len` bytes holding `values` values may take.
fn decode_cost(raw_len: usize, values: usize) -> usize {
    raw_len
        .saturating_mul(RAW_COST)
        .saturating_add(values.saturating_mul(VALUE_COST))
}

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
    /// Fake-peer reputation (D93): strikes before a peer is skipped, for how long, and how many
    /// peers are remembered (LRU).
    pub fake_strikes: u8,
    pub fake_ban: Duration,
    pub fake_cap: usize,
    /// Keep the verified raw bytes in [`Fetched::raw`] (crawl workers upload them for the
    /// server to verify again, D97). Off on the server: the bytes are dropped after parsing.
    pub keep_raw: bool,
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
            fake_strikes: 3,
            fake_ban: Duration::from_secs(3600),
            fake_cap: 50_000,
            keep_raw: false,
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

    /// The error a [`code`](Self::code) names (worker reports, D97).
    pub fn from_code(code: &str) -> Option<Self> {
        PEER_ERRORS.into_iter().find(|e| e.code() == code)
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
    /// The verified bytes, only with [`FetchConfig::keep_raw`].
    pub raw: Option<Vec<u8>>,
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
    /// Peers not tried because the reputation cache skips them (fake peers, D93).
    pub skipped: usize,
}

impl FetchFailure {
    pub fn code(&self) -> &'static str {
        if self.peers_tried == 0 && self.skipped > 0 {
            "fake_peers"
        } else if self.peers_tried == 0 {
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
    fake_strikes: AtomicU64,
    fake_banned: AtomicU64,
    fake_skipped: AtomicU64,
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
    /// Fake-peer reputation (D93): strikes given (a peer took our handshake for the hash, then
    /// never sent its extended handshake), peers that reached the limit and were skipped for
    /// an hour, peer attempts skipped for that, peers remembered now.
    pub fake_strikes: u64,
    pub fake_banned: u64,
    pub fake_skipped: u64,
    pub fake_tracked: usize,
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
    /// Permits `decode_budget` was created with.
    decode_units: usize,
    connects: Mutex<Pace>,
    /// Open TCP connections.
    conns: Semaphore,
    /// While in the future, connection attempts are paced at half the rate (D66).
    braked_until: Mutex<Option<Instant>>,
    reputation: Mutex<Reputation>,
    c: Counters,
}

/// Peer ids of clients seen answering the BitTorrent handshake for **any** hash and then never
/// sending the BEP 10 handshake (docs research 2026-10-26: ~10 % of accepted handshakes). A
/// match only doubles a strike that the behaviour earned anyway; it is never a rule by itself.
const ECHO_PREFIXES: [&[u8]; 2] = [b"-XT", b"cf-aa84-"];

fn echo_prefix(peer_id: &[u8; 20]) -> bool {
    ECHO_PREFIXES.iter().any(|p| peer_id.starts_with(p))
}

struct RepEntry {
    strikes: u8,
    banned_until: Option<Instant>,
    last: Instant,
    stamp: u64,
}

/// Bounded fake-peer memory (D93): `ip:port` → strikes. A peer that accepts our handshake for
/// a hash and never sends its extended handshake gets a strike (two when its peer id has an
/// [`ECHO_PREFIXES`] prefix); at `limit` strikes it is skipped for `ban`. Strikes without a ban
/// are forgotten `ban` after the last one, verified metadata from the peer clears them, and
/// the least recently struck entry goes when `cap` is reached. Memory only: no database, no
/// address ranges.
pub(crate) struct Reputation {
    map: HashMap<SocketAddrV4, RepEntry>,
    /// Touch order with lazy stamps (stale pairs are skipped).
    order: VecDeque<(SocketAddrV4, u64)>,
    next: u64,
    cap: usize,
    limit: u8,
    ban: Duration,
}

impl Reputation {
    pub(crate) fn new(cap: usize, limit: u8, ban: Duration) -> Self {
        Self {
            map: HashMap::new(),
            order: VecDeque::new(),
            next: 0,
            cap: cap.max(1),
            limit: limit.max(1),
            ban,
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.map.len()
    }

    /// Whether `a` is skipped now (an expired entry is dropped on the way).
    pub(crate) fn skipped(&mut self, a: &SocketAddrV4, now: Instant) -> bool {
        let Some(e) = self.map.get(a) else {
            return false;
        };
        match e.banned_until {
            Some(u) if now < u => true,
            Some(_) => {
                self.map.remove(a);
                false
            }
            None => {
                if now.saturating_duration_since(e.last) >= self.ban {
                    self.map.remove(a);
                }
                false
            }
        }
    }

    /// Add `n` strikes; true when this makes the peer skipped.
    pub(crate) fn strike(&mut self, a: SocketAddrV4, n: u8, now: Instant) -> bool {
        if !self.map.contains_key(&a) && self.map.len() >= self.cap {
            self.evict();
        }
        self.next += 1;
        let stamp = self.next;
        let e = self.map.entry(a).or_insert(RepEntry {
            strikes: 0,
            banned_until: None,
            last: now,
            stamp,
        });
        if e.banned_until.is_some_and(|u| now >= u)
            || (e.banned_until.is_none() && now.saturating_duration_since(e.last) >= self.ban)
        {
            e.strikes = 0;
            e.banned_until = None;
        }
        e.strikes = e.strikes.saturating_add(n);
        e.last = now;
        e.stamp = stamp;
        self.order.push_back((a, stamp));
        let banned = e.banned_until.is_none() && e.strikes >= self.limit;
        if banned {
            e.banned_until = Some(now + self.ban);
        }
        if self.order.len() > 4 * self.cap + 1024 {
            let map = &self.map;
            self.order
                .retain(|(a, st)| map.get(a).is_some_and(|e| e.stamp == *st));
        }
        banned
    }

    pub(crate) fn clear(&mut self, a: &SocketAddrV4) {
        self.map.remove(a);
    }

    fn evict(&mut self) {
        while let Some((a, st)) = self.order.pop_front() {
            if self.map.get(&a).is_some_and(|e| e.stamp == st) {
                self.map.remove(&a);
                return;
            }
        }
    }
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
    Some(identify(m, raw, parsed))
}

fn identify(
    m: Match,
    raw: &[u8],
    parsed: Result<InfoSummary, InfoError>,
) -> (Identity, Result<InfoSummary, InfoError>) {
    let id = match (m, &parsed) {
        (Match::Sha1, Ok(s)) if s.meta_version == Some(2) && s.has_pieces => Identity::V1 {
            v2: Some(sha2::Sha256::digest(raw).into()),
        },
        (Match::Sha1, Ok(s)) if !s.has_pieces && s.has_file_tree => Identity::V2Only,
        (Match::Sha1, _) => Identity::V1 { v2: None },
        (Match::TruncatedSha256 { v1, v2 }, Ok(s)) if s.has_pieces => Identity::Hybrid { v1, v2 },
        (Match::TruncatedSha256 { .. }, _) => Identity::V2Only,
    };
    (id, parsed)
}

/// The [`Outcome`] of verified bytes (`raw_len` of them; `raw` kept only when asked).
fn outcome_of(
    sighted: [u8; 20],
    raw_len: usize,
    raw: Option<Vec<u8>>,
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
        raw,
    }))
}

/// Why bytes handed to [`outcome_from_raw`] are not an info dict of the hash.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RawError {
    /// Longer than the `metadata_max` given (nothing was hashed).
    TooLarge,
    /// Neither the SHA-1 nor the truncated SHA-256 of the bytes is the hash.
    Mismatch,
}

/// Verify bytes that did not come from our own fetcher (a crawl worker's upload, D97) exactly
/// as a fetched dict is: size bound, hash of the raw bytes, value count before any decode
/// (`too_complex` above [`max_values`]), parse, identity. Synchronous and CPU-bound (run it off
/// the async threads); its memory is bounded like a fetcher decode: about 3 × the size plus
/// 64 B per value, at most `metadata_max / 8` values.
pub fn outcome_from_raw(
    sighted: [u8; 20],
    raw: &[u8],
    metadata_max: usize,
) -> Result<Outcome, RawError> {
    if raw.len() > metadata_max {
        return Err(RawError::TooLarge);
    }
    let m = match_hash(&sighted, raw).ok_or(RawError::Mismatch)?;
    let limit = max_values(metadata_max);
    if bencode::count_values(raw, limit) > limit {
        let id = match m {
            Match::Sha1 => Identity::V1 { v2: None },
            Match::TruncatedSha256 { v1, v2 } => Identity::Hybrid { v1, v2 },
        };
        return Ok(outcome_of(
            sighted,
            raw.len(),
            None,
            id,
            Err(InfoError::TooComplex),
            0,
        ));
    }
    let (id, parsed) = identify(m, raw, parse_info(raw));
    Ok(outcome_of(sighted, raw.len(), None, id, parsed, 0))
}

impl Fetcher {
    pub fn new(mut cfg: FetchConfig) -> Self {
        // A single fetch must always fit the in-flight budget.
        // Whole units, so a fetch of `metadata_max` bytes (rounded up to a unit) always fits.
        cfg.inflight_max = (cfg.inflight_max.max(UNIT) / UNIT) * UNIT;
        cfg.metadata_max = cfg.metadata_max.clamp(1, cfg.inflight_max);
        let units = u32::try_from(cfg.inflight_max / UNIT).unwrap_or(u32::MAX >> 3);
        let decode_units = (cfg.metadata_max.div_ceil(UNIT) * RAW_COST)
            .saturating_mul(cfg.decode_parallel.max(1))
            .clamp(1, Semaphore::MAX_PERMITS);
        let now = Instant::now();
        Self {
            inner: Arc::new(Inner {
                peer_id: new_peer_id(),
                filter: AddrFilter::new(cfg.allow_loopback),
                budget: Semaphore::new(units as usize),
                decode: Semaphore::new(cfg.decode_parallel.max(1)),
                decode_budget: Semaphore::new(decode_units),
                decode_units,
                connects: Mutex::new(Pace::new(cfg.connects_per_sec.max(1), now)),
                conns: Semaphore::new(cfg.max_connections.clamp(1, Semaphore::MAX_PERMITS)),
                braked_until: Mutex::new(None),
                reputation: Mutex::new(Reputation::new(
                    cfg.fake_cap,
                    cfg.fake_strikes,
                    cfg.fake_ban,
                )),
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
            fake_strikes: g(&c.fake_strikes),
            fake_banned: g(&c.fake_banned),
            fake_skipped: g(&c.fake_skipped),
            fake_tracked: self.inner.reputation.lock().unwrap().len(),
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
            skipped: 0,
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
        // Peers the reputation cache skips do not use up the per-hash cap.
        let mut skipped = 0usize;
        let list: Vec<SocketAddrV4> = {
            let now = Instant::now();
            let mut rep = self.inner.reputation.lock().unwrap();
            peers
                .iter()
                .copied()
                .filter(|p| {
                    let s = rep.skipped(p, now);
                    skipped += usize::from(s);
                    !s
                })
                .take(cfg.max_peers)
                .collect()
        };
        add(&self.inner.c.fake_skipped, skipped as u64);
        let mut todo = list.into_iter();
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
            skipped,
        })
    }

    fn outcome(
        &self,
        sighted: [u8; 20],
        raw: Vec<u8>,
        id: Identity,
        parsed: Result<InfoSummary, InfoError>,
        tried: usize,
    ) -> Outcome {
        let len = raw.len();
        let raw = self.inner.cfg.keep_raw.then_some(raw);
        outcome_of(sighted, len, raw, id, parsed, tried)
    }

    /// One peer session. Returns the raw bytes, identity and parse result of verified bytes.
    async fn peer(
        &self,
        info_hash: [u8; 20],
        addr: SocketAddrV4,
    ) -> Result<(Vec<u8>, Identity, Result<InfoSummary, InfoError>), PeerError> {
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
        let mut hs: Option<[u8; 20]> = None;
        let mut ext = false;
        let r = tokio::time::timeout(
            cfg.peer_timeout,
            self.session(stream, info_hash, connected, &mut hs, &mut ext),
        )
        .await
        .unwrap_or(Err(PeerError::Timeout));
        match (&r, hs) {
            (Ok(_), _) => self.inner.reputation.lock().unwrap().clear(&addr),
            // Took the handshake for this hash, never sent the extended one: a strike.
            (Err(_), Some(id)) if !ext => {
                let n = if echo_prefix(&id) { 2 } else { 1 };
                add(&self.inner.c.fake_strikes, 1);
                let banned = self
                    .inner
                    .reputation
                    .lock()
                    .unwrap()
                    .strike(addr, n, Instant::now());
                if banned {
                    add(&self.inner.c.fake_banned, 1);
                }
            }
            _ => {}
        }
        r
    }

    async fn session(
        &self,
        stream: TcpStream,
        info_hash: [u8; 20],
        connected: Instant,
        accepted: &mut Option<[u8; 20]>,
        ext_seen: &mut bool,
    ) -> Result<(Vec<u8>, Identity, Result<InfoSummary, InfoError>), PeerError> {
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
            let theirs = wire::parse_handshake(&b, &info_hash).map_err(|e| match e {
                WireError::NoExtensions => PeerError::NoExtensions,
                _ => PeerError::Handshake,
            })?;
            *accepted = Some(theirs.peer_id);
            // BEP 10: the extended handshake goes to peers that set the extension bit, so ours
            // is sent only once theirs is known to have it.
            let ours = wire::our_extended_handshake(&cfg.client);
            wr.write_all(&ours).await?;
            add(&c.tcp_bytes_out, ours.len() as u64);
            loop {
                if let Msg::Extended(p) = read_msg(&mut rd, &mut discard, &c.tcp_bytes_in).await? {
                    if p.first() == Some(&wire::EXT_HANDSHAKE) {
                        *ext_seen = true;
                        return wire::parse_ext_handshake(&p[1..]).map_err(|_| PeerError::Protocol);
                    }
                    charge(&mut discard, p.len())?;
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
                // Another extension (ut_pex, a repeated handshake, …): skipped, but within the
                // same per-peer budget as other messages.
                charge(&mut discard, p.len())?;
                continue;
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
                MetadataMsg::Unknown => charge(&mut discard, p.len())?,
            }
        }
        drop(rd);
        drop(wr);

        // Verify, price, then parse off the async threads, `decode_parallel` at once. The hash
        // and the value count allocate nothing; the decode reserves its priced cost (raw bytes
        // × 3 + 64 B per value) from the decode budget, capped at the whole budget so the
        // largest allowed dict still runs (alone).
        let _decode = self
            .inner
            .decode
            .acquire()
            .await
            .map_err(|_| PeerError::Closed)?;
        let limit = max_values(cfg.metadata_max);
        let (raw, m, values) = tokio::task::spawn_blocking(move || {
            let m = match_hash(&info_hash, &raw);
            let n = m.as_ref().map_or(0, |_| bencode::count_values(&raw, limit));
            (raw, m, n)
        })
        .await
        .map_err(|_| PeerError::Closed)?;
        let m = m.ok_or(PeerError::HashMismatch)?;
        if values > limit {
            let id = match m {
                Match::Sha1 => Identity::V1 { v2: None },
                Match::TruncatedSha256 { v1, v2 } => Identity::Hybrid { v1, v2 },
            };
            return Ok((raw, id, Err(InfoError::TooComplex)));
        }
        let cost_units = decode_cost(size, values)
            .div_ceil(UNIT)
            .min(self.inner.decode_units);
        let _extra = self
            .inner
            .decode_budget
            .acquire_many(u32::try_from(cost_units).unwrap_or(u32::MAX))
            .await
            .map_err(|_| PeerError::Closed)?;
        let (raw, id, parsed) = tokio::task::spawn_blocking(move || {
            let parsed = parse_info(&raw);
            let (id, parsed) = identify(m, &raw, parsed);
            (raw, id, parsed)
        })
        .await
        .map_err(|_| PeerError::Closed)?;
        Ok((raw, id, parsed))
    }
}

/// Take `n` bytes of skipped traffic from the per-peer budget; past it the peer is dropped.
fn charge(discard: &mut u64, n: usize) -> Result<(), PeerError> {
    let n = n as u64;
    if n > *discard {
        return Err(PeerError::Protocol);
    }
    *discard -= n;
    Ok(())
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

    /// D97: what a worker uploads is verified exactly like a fetched dict.
    #[test]
    fn raw_uploads_are_verified_like_fetches() {
        let v1_raw = b"d6:lengthi5e4:name1:a12:piece lengthi16384e6:pieces20:aaaaaaaaaaaaaaaaaaaae";
        let h1 = sha1_smol::Sha1::from(&v1_raw[..]).digest().bytes();
        match outcome_from_raw(h1, v1_raw, 1 << 20).unwrap() {
            Outcome::Ok(f) => {
                assert_eq!(
                    (f.v1, f.info.name.as_str(), f.metadata_size),
                    (h1, "a", v1_raw.len())
                );
                assert!(f.raw.is_none());
            }
            o => panic!("{o:?}"),
        }
        assert_eq!(
            outcome_from_raw([0; 20], v1_raw, 1 << 20).unwrap_err(),
            RawError::Mismatch
        );
        assert_eq!(
            outcome_from_raw(h1, v1_raw, 10).unwrap_err(),
            RawError::TooLarge
        );
        let private = b"d6:lengthi5e4:name1:a12:piece lengthi16384e6:pieces20:aaaaaaaaaaaaaaaaaaaa7:privatei1ee";
        let hp = sha1_smol::Sha1::from(&private[..]).digest().bytes();
        assert!(matches!(
            outcome_from_raw(hp, private, 1 << 20),
            Ok(Outcome::Private)
        ));
        let junk = b"i5e";
        let hj = sha1_smol::Sha1::from(&junk[..]).digest().bytes();
        assert!(matches!(
            outcome_from_raw(hj, junk, 1 << 20),
            Ok(Outcome::Invalid(_))
        ));
        for e in PEER_ERRORS {
            assert_eq!(PeerError::from_code(e.code()), Some(e));
        }
        assert_eq!(
            InfoError::from_code("too_complex"),
            Some(InfoError::TooComplex)
        );
        assert_eq!(InfoError::from_code("x"), None);
    }

    #[test]
    fn decode_pricing() {
        use crate::liveness::bencode::{Value, count_values};
        // The per-value price covers the tree's node size.
        assert!(std::mem::size_of::<Value>() * 2 <= VALUE_COST);
        assert!(std::mem::size_of::<(Vec<u8>, Value)>() <= VALUE_COST);
        // dict, 2 keys, int, string = 5
        assert_eq!(count_values(b"d1:ai1e1:b2:xye", 100), 5);
        assert_eq!(count_values(b"llelelee", 100), 4);
        // stops past the limit
        assert_eq!(count_values(&b"le".repeat(1000), 10), 11);
        // malformed input ends the count, never panics
        assert_eq!(count_values(b"99999999999999999999999:x", 10), 1);
        assert_eq!(count_values(b"i12", 10), 1);
        assert_eq!(max_values(16 << 20), 2 << 20);
        assert_eq!(max_values(1 << 10), 64 * 1024);
        assert_eq!(decode_cost(10, 2), 30 + 128);
    }

    #[test]
    fn metadata_max_always_fits_the_budget() {
        let f = Fetcher::new(FetchConfig {
            inflight_max: 1500,
            metadata_max: 1 << 20,
            ..FetchConfig::default()
        });
        // 1500 B of budget is one whole KiB unit: a 1024 B fetch fits, a 1025 B one is
        // oversize instead of waiting forever for a second unit.
        assert_eq!(f.config().metadata_max, 1024);
        assert_eq!(f.inner.budget.available_permits(), 1);
    }

    #[test]
    fn reputation_strikes_bans_expires_and_is_bounded() {
        let a: SocketAddrV4 = "8.8.8.8:6881".parse().unwrap();
        let b: SocketAddrV4 = "8.8.4.4:6881".parse().unwrap();
        let hour = Duration::from_secs(3600);
        let t = Instant::now();
        let mut r = Reputation::new(3, 3, hour);
        assert!(!r.strike(a, 1, t));
        assert!(!r.strike(a, 1, t));
        assert!(!r.skipped(&a, t));
        assert!(r.strike(a, 1, t), "third strike");
        assert!(r.skipped(&a, t + hour - Duration::from_secs(1)));
        assert!(!r.skipped(&a, t + hour), "ban over");
        assert_eq!(r.len(), 0);
        // An echo prefix counts double, never alone: two failures, not one.
        assert!(!r.strike(b, 2, t));
        assert!(r.strike(b, 2, t));
        // Strikes without a ban are forgotten an hour after the last one.
        let c: SocketAddrV4 = "1.1.1.1:1".parse().unwrap();
        r.strike(c, 2, t);
        assert!(!r.strike(c, 1, t + hour), "old strikes reset");
        // Success clears.
        r.clear(&c);
        assert!(!r.skipped(&c, t));
        // Bounded: the least recently struck entry goes.
        let mut r = Reputation::new(2, 3, hour);
        for i in 0..10u16 {
            r.strike(SocketAddrV4::new([9, 9, 9, 9].into(), i + 1), 1, t);
        }
        assert_eq!(r.len(), 2);
        assert!(echo_prefix(b"-XT0100-abcdefghijkl"));
        assert!(echo_prefix(b"cf-aa84-abcdefghijkl"));
        assert!(!echo_prefix(b"-qB4650-abcdefghijkl"));
    }

    #[test]
    fn peer_id_shape() {
        let id = new_peer_id();
        assert_eq!(&id[..8], b"-MG0300-");
    }

    #[test]
    fn failure_codes() {
        let mut f = FetchFailure {
            error: PeerError::Connect,
            oversize: false,
            peers_tried: 0,
            skipped: 0,
        };
        assert_eq!(f.code(), "no_peers");
        f.skipped = 2;
        assert_eq!(f.code(), "fake_peers");
        assert!(PeerError::HashMismatch > PeerError::Connect);
    }
}

//! The probe's event loop and sniff driver (D117).
//!
//! Per target file: a [`SparseFile`] of the file's size, the planned first ranges, then
//! [`Detector::analyze`] → wanted ranges → 16 KiB blocks (or whole pieces) → requests to the
//! peers that have them → bytes inserted (verified pieces with `insert`, single blocks with
//! `insert_unverified`) → analyze again, until sniff wants nothing more or a cap is hit; then
//! [`Detector::finish`]. Sniff runs on the blocking pool (it is synchronous and catches its
//! own panics).
//!
//! Peers: the hand-off connection first; a second one (through the same [`Fetcher`], so the
//! same filter, pace, brake and connection cap) when the first is gone, still choked after
//! [`ProbeConfig::second_peer_after`], or lacks a wanted piece. Each peer's frames are read by
//! its own task into one channel, so no half-read frame is ever dropped by the loop.

use std::collections::{HashMap, HashSet, VecDeque};
use std::future::Future;
use std::net::SocketAddrV4;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::Duration;

use sniff::{Detection, Detector, Range, SparseFile};
use tokio::sync::mpsc;
use tokio::time::Instant;

use super::msg::{BLOCK, Block, PeerMsg, ProbeMsg, metadata_request};
use super::peer::{Avail, Event, Peer, bitfield};
use super::plan::{self, PlanFile, Target, blocks_for, pieces_for};
use super::serve::{ServeMsg, Take, UploadGate};
use super::verify::{AsmError, PieceAssembler};
use super::{FileResult, ProbeConfig, ProbeJob, ProbeResult, ProbeStatus};
use crate::metainfo::{Fetcher, InfoFile, Layout, PeerConn, PeerError, wire};

/// The loop wakes at least this often (timeouts, upload pacing).
const TICK: Duration = Duration::from_millis(100);
/// Most queued upload requests per probe.
const SERVE_QUEUE: usize = 256;
/// Metadata requests answered with `reject` per peer before it is dropped (as the fetcher).
const METADATA_REJECTS: usize = 64;
/// `reject_request` answers per peer; past it requests are ignored.
const REJECTS: usize = 1024;
/// An agreement read waits at most this long.
const AGREEMENT_WAIT: Duration = Duration::from_secs(5);

type Connecting = Pin<Box<dyn Future<Output = Result<PeerConn, PeerError>> + Send>>;

struct Cached {
    data: Vec<u8>,
    peer: usize,
}

struct Session<'a> {
    cfg: &'a ProbeConfig,
    fetcher: Fetcher,
    hash: [u8; 20],
    layout: Layout,
    files: &'a [InfoFile],
    peers: Vec<Option<Peer>>,
    /// Address of every slot (kept after the peer is gone).
    addrs: Vec<SocketAddrV4>,
    tx: mpsc::Sender<(usize, Event)>,
    rx: mpsc::Receiver<(usize, Event)>,
    candidates: VecDeque<SocketAddrV4>,
    connecting: Option<Connecting>,
    attempts: usize,
    /// Unverified blocks, by request.
    cache: HashMap<Block, Cached>,
    /// Hash-checked pieces.
    verified: HashMap<u32, Vec<u8>>,
    asm: HashMap<u32, PieceAssembler>,
    /// The one peer a piece fetched whole is asked from (so a hash mismatch names the liar).
    owner: HashMap<u32, usize>,
    retried: HashSet<u32>,
    failed_pieces: HashSet<u32>,
    /// Agreement read: blocks from this peer go here instead of the cache.
    side: Option<(usize, HashMap<Block, Vec<u8>>)>,
    gate: Option<UploadGate>,
    serve_q: VecDeque<(usize, Block)>,
    serve_after: Option<Instant>,
    /// `HAVE` messages for pieces held since the last turn (upload mode).
    announce: Vec<ServeMsg>,
    /// Piece payload received.
    bytes_in: u64,
    deadline: Instant,
    struck: HashSet<usize>,
    struck_addrs: HashSet<SocketAddrV4>,
    budget_hit: bool,
    ever_unchoked: bool,
    saw_pieces: bool,
    wire_in: Arc<AtomicU64>,
    wire_out: Arc<AtomicU64>,
}

/// What one sniff pass over a target gave.
struct Sniffed {
    detection: Option<Detection>,
    /// The verdict after the first read only (for the agreement read).
    first: Option<Detection>,
    /// Peers whose unverified blocks were read.
    contributors: HashSet<usize>,
    /// File ranges known at the end.
    known: Vec<Range>,
}

pub(super) async fn run(cfg: &ProbeConfig, fetcher: &Fetcher, job: ProbeJob) -> ProbeResult {
    let ProbeJob {
        info_hash,
        raw,
        files,
        conn,
        peers,
    } = job;
    if raw.len() > super::MAX_DICT {
        return ProbeResult::failed("big_dict");
    }
    let layout = match tokio::task::spawn_blocking(move || crate::metainfo::layout(&raw)).await {
        Ok(Ok(l)) => l,
        _ => return ProbeResult::failed("layout"),
    };
    if layout.sizes.len() != files.len()
        || layout.sizes.iter().zip(&files).any(|(s, f)| *s != f.size)
    {
        return ProbeResult::failed("layout");
    }
    let plan_files: Vec<PlanFile> = files
        .iter()
        .map(|f| PlanFile {
            path: &f.path,
            size: f.size,
        })
        .collect();
    let targets = plan::plan(&plan_files, cfg.max_targets);
    if targets.is_empty() {
        return ProbeResult::failed("no_targets");
    }
    let (tx, rx) = mpsc::channel(256);
    let mut first = None;
    let mut candidates = VecDeque::new();
    if let Some(c) = &conn {
        first = Some(c.addr);
    }
    for p in peers {
        if Some(p) != first && !candidates.contains(&p) {
            candidates.push_back(p);
        }
    }
    let mut s = Session {
        cfg,
        fetcher: fetcher.clone(),
        hash: info_hash,
        layout,
        files: &files,
        peers: Vec::new(),
        addrs: Vec::new(),
        tx,
        rx,
        candidates,
        connecting: None,
        attempts: 0,
        cache: HashMap::new(),
        verified: HashMap::new(),
        asm: HashMap::new(),
        owner: HashMap::new(),
        retried: HashSet::new(),
        failed_pieces: HashSet::new(),
        side: None,
        gate: UploadGate::new(cfg.upload, cfg.upload_max_bytes, cfg.upload_rate),
        serve_q: VecDeque::new(),
        serve_after: None,
        announce: Vec::new(),
        bytes_in: 0,
        deadline: Instant::now() + cfg.session,
        struck: HashSet::new(),
        struck_addrs: HashSet::new(),
        budget_hit: false,
        ever_unchoked: false,
        saw_pieces: false,
        wire_in: Arc::new(AtomicU64::new(0)),
        wire_out: Arc::new(AtomicU64::new(0)),
    };
    if let Some(c) = conn {
        s.add_peer(c);
    }
    s.run(&targets).await
}

impl Session<'_> {
    fn pieces(&self) -> u32 {
        self.layout.piece_count()
    }

    fn add_peer(&mut self, c: PeerConn) {
        let slot = self.peers.len();
        let p = Peer::start(
            c,
            slot,
            self.pieces(),
            self.cfg.pipeline,
            self.tx.clone(),
            self.wire_in.clone(),
            self.wire_out.clone(),
        );
        if !p.choked {
            self.ever_unchoked = true;
        }
        let dead = p.dead;
        self.addrs.push(p.addr);
        self.peers.push(Some(p));
        if let Some(r) = dead {
            self.kill(slot, r, None);
        }
    }

    fn alive(&self) -> impl Iterator<Item = &Peer> {
        self.peers.iter().flatten()
    }

    fn peer_mut(&mut self, slot: usize) -> Option<&mut Peer> {
        self.peers.get_mut(slot).and_then(Option::as_mut)
    }

    fn inflight_bytes(&self) -> u64 {
        self.alive()
            .flat_map(|p| p.inflight.iter())
            .map(|(b, _)| u64::from(b.len))
            .sum()
    }

    fn out_of_time(&self) -> bool {
        Instant::now() >= self.deadline
    }

    fn budget_left(&self) -> u64 {
        self.cfg.max_bytes.saturating_sub(self.bytes_in)
    }

    /// Close a peer; its outstanding requests that are still wanted go back to `queue`.
    fn kill(
        &mut self,
        slot: usize,
        _why: &'static str,
        requeue: Option<(&mut VecDeque<Block>, &HashSet<Block>)>,
    ) {
        let Some(p) = self.peers.get_mut(slot).and_then(Option::take) else {
            return;
        };
        // Pieces this peer was fetching whole start over with another peer.
        let owned: Vec<u32> = self
            .owner
            .iter()
            .filter(|(_, o)| **o == slot)
            .map(|(i, _)| *i)
            .collect();
        let mut again = Vec::new();
        for i in owned {
            again.extend(self.release(i));
        }
        if let Some((q, wanted)) = requeue {
            for b in p.inflight.iter().map(|(b, _)| b).chain(again.iter()) {
                if wanted.contains(b) && !q.contains(b) {
                    q.push_back(*b);
                }
            }
        }
        self.serve_q.retain(|(s, _)| *s != slot);
    }

    /// Start a piece fetched whole over (its peer is gone or refused a block): a fresh
    /// assembler and no owner. Returns every block of the piece, to ask again.
    fn release(&mut self, i: u32) -> Vec<Block> {
        self.owner.remove(&i);
        if self.asm.remove(&i).is_none() {
            return Vec::new();
        }
        let (Some(len), Some(h)) = (self.layout.piece_len(i), self.layout.piece_hash(i)) else {
            return Vec::new();
        };
        self.asm.insert(i, PieceAssembler::new(i, len as u32, h));
        blocks_for(
            u64::from(i) * self.layout.piece_length,
            len,
            self.layout.piece_length,
            self.layout.total,
            true,
        )
    }

    /// A peer sent bytes that failed a hash check: dropped, and its unverified blocks are no
    /// longer used.
    fn strike(&mut self, slot: usize, requeue: Option<(&mut VecDeque<Block>, &HashSet<Block>)>) {
        self.struck.insert(slot);
        if let Some(a) = self.addrs.get(slot) {
            self.struck_addrs.insert(*a);
        }
        self.cache.retain(|_, c| c.peer != slot);
        self.kill(slot, "hash_mismatch", requeue);
    }

    async fn run(mut self, targets: &[Target]) -> ProbeResult {
        let mut results = Vec::new();
        let mut missing = 0usize;
        let mut incomplete = false;
        let mut surprises = 0u32;
        for (n, t) in targets.iter().enumerate() {
            if self.out_of_time()
                || self.budget_left() == 0
                || (self.alive().count() == 0 && !self.can_connect())
            {
                missing += targets.len() - n;
                incomplete = true;
                break;
            }
            let whole = self.layout.piece_length <= self.cfg.whole_piece_max;
            let cap = self.file_cap(t, whole, &targets[n + 1..]);
            let s = self.sniff_file(t, whole, cap).await;
            let Some(mut d) = s.detection else {
                missing += 1;
                continue;
            };
            let mut contributors = s.contributors;
            let mut verified = !d.flags.unverified;
            let mut agreed = 1u8;
            let mut notes = Vec::new();
            let mut disagree = false;
            // Two-peer agreement on the main payload's first read, when a second unchoked
            // peer is already connected (decision 5).
            let other = (n == 0 && !verified)
                .then(|| self.agreement_peer(t, &contributors))
                .flatten();
            if let Some(other) = other
                && let Some(od) = self.agreement(t, other).await
            {
                let first = s.first.as_ref().unwrap_or(&d);
                if od.label == first.label
                    && od.confidence.abs_diff(first.confidence) <= self.cfg.agreement_slack
                {
                    agreed = 2;
                } else {
                    disagree = true;
                    notes.push("disagree");
                }
            }
            if !verified
                && (d.flags.suspicious_executable || d.flags.extension_mismatch || disagree)
            {
                surprises += 1;
                // Whole pieces under every range the verdict read, within the budget left.
                let cost: u64 = self.surprise_cost(t.file, &s.known);
                if cost <= self.budget_left() && !self.out_of_time() {
                    // The verified re-read may fetch every piece the verdict read, even when
                    // they cost more than the per-file cap once whole (D120).
                    let cap = self.cfg.caps.max_bytes.max(cost);
                    let v = self.sniff_file(t, true, cap).await;
                    match v.detection {
                        Some(vd)
                            if !vd.flags.unverified && vd.confidence >= self.cfg.min_confidence =>
                        {
                            notes.push(if vd.label == d.label {
                                "surprise_verified"
                            } else {
                                "surprise_changed"
                            });
                            d = vd;
                            verified = true;
                            contributors = v.contributors;
                        }
                        _ => notes.push("surprise_unverified"),
                    }
                } else {
                    notes.push("surprise_unverified");
                }
            }
            // Bytes from a peer later caught lying are not trusted.
            if !verified && contributors.iter().any(|c| self.struck.contains(c)) {
                missing += 1;
                continue;
            }
            if d.flags.incomplete {
                incomplete = true;
            }
            if d.confidence < self.cfg.min_confidence {
                missing += 1;
                continue;
            }
            let from = if verified {
                HashSet::new()
            } else {
                contributors
            };
            results.push((
                FileResult {
                    index: t.file as u32,
                    size: self.layout.sizes[t.file],
                    detection: d,
                    verified,
                    agreed,
                    notes,
                },
                from,
            ));
        }
        // A peer struck after an earlier target trusted its blocks: those results go too.
        let before = results.len();
        results.retain(|(_, from)| from.iter().all(|c| !self.struck.contains(c)));
        missing += before - results.len();
        let results: Vec<FileResult> = results.into_iter().map(|(r, _)| r).collect();
        self.linger().await;
        let status = if !results.is_empty() {
            if missing == 0 && !incomplete {
                ProbeStatus::Ok
            } else {
                ProbeStatus::Partial
            }
        } else if self.saw_pieces && !self.ever_unchoked && self.bytes_in == 0 {
            ProbeStatus::Choked
        } else if !self.saw_pieces {
            ProbeStatus::NoPieces
        } else {
            ProbeStatus::Failed("no_result")
        };
        ProbeResult {
            status,
            files: results,
            bytes_in: self.bytes_in,
            uploaded: self.gate.as_ref().map_or(0, UploadGate::served),
            peers_used: u8::try_from(self.peers.len()).unwrap_or(u8::MAX),
            unchoked: self.ever_unchoked,
            incomplete: incomplete || self.budget_hit,
            surprises,
        }
    }

    /// Bytes of the pieces under `known` (file ranges) that are not verified yet.
    fn surprise_cost(&self, file: usize, known: &[Range]) -> u64 {
        let off = self.layout.offsets[file];
        let mut pieces = HashSet::new();
        for r in known {
            for p in pieces_for(
                off + r.offset,
                r.len,
                self.layout.piece_length,
                self.layout.total,
            ) {
                if !self.verified.contains_key(&p) {
                    pieces.insert(p);
                }
            }
        }
        pieces
            .iter()
            .filter_map(|p| self.layout.piece_len(*p))
            .sum()
    }

    /// An unchoked live peer, other than the ones that gave the first read, that has the
    /// first read's pieces.
    fn agreement_peer(&self, t: &Target, contributors: &HashSet<usize>) -> Option<usize> {
        let off = self.layout.offsets[t.file];
        let blocks: Vec<Block> = t
            .initial
            .iter()
            .flat_map(|r| {
                blocks_for(
                    off + r.offset,
                    r.len,
                    self.layout.piece_length,
                    self.layout.total,
                    false,
                )
            })
            .collect();
        self.alive()
            .find(|p| {
                !contributors.contains(&p.slot)
                    && !p.choked
                    && blocks.iter().all(|b| p.avail.has(b.index))
            })
            .map(|p| p.slot)
    }

    /// The first read of `t` again, from peer `other` only; its verdict.
    async fn agreement(&mut self, t: &Target, other: usize) -> Option<Detection> {
        let off = self.layout.offsets[t.file];
        let size = self.layout.sizes[t.file];
        let blocks: Vec<Block> = t
            .initial
            .iter()
            .flat_map(|r| {
                blocks_for(
                    off + r.offset,
                    r.len,
                    self.layout.piece_length,
                    self.layout.total,
                    false,
                )
            })
            .collect();
        self.side = Some((other, HashMap::new()));
        let until = (Instant::now() + AGREEMENT_WAIT).min(self.deadline);
        self.get(&blocks, false, Some(other), until).await;
        let (_, got) = self.side.take()?;
        if blocks.iter().any(|b| !got.contains_key(b)) {
            return None;
        }
        let mut file = SparseFile::new(size).with_name(self.files[t.file].path.clone());
        for b in &blocks {
            let abs = u64::from(b.index) * self.layout.piece_length + u64::from(b.begin);
            if let Some(data) = got.get(b) {
                insert_clipped(&mut file, off, size, abs, data, false);
            }
        }
        let det = Detector::new().caps(self.caps_for(t));
        let out = tokio::task::spawn_blocking(move || det.finish(&file))
            .await
            .ok()?;
        out.detection().cloned()
    }

    fn caps_for(&self, t: &Target) -> sniff::Caps {
        sniff::Caps {
            head: t.initial.first().map_or(plan::HEAD, |r| r.len.max(1)),
            ..self.cfg.caps.clone()
        }
    }

    /// Piece bytes one target may use: sniff's per-file cap, or for a video payload (the
    /// largest file, always the first target) fetched in whole pieces, the torrent's budget
    /// less the first reads of the other targets (D120). An MP4 / MOV with its `moov` at the
    /// end costs, with whole pieces, the head piece plus one piece per track hop (a 2 h film
    /// with three audio tracks: 5 MiB at 1 MiB pieces), which a 4 MiB cap cut after the first
    /// audio track. Blocks cost a few 16 KiB reads, and other formats keep the per-file cap.
    fn file_cap(&self, t: &Target, whole: bool, rest: &[Target]) -> u64 {
        let per_file = self.cfg.caps.max_bytes;
        if !(whole && t.priority == plan::PAYLOAD && t.kind == plan::Kind::Video) {
            return per_file;
        }
        let mut pieces = HashSet::new();
        for r in rest {
            let off = self.layout.offsets[r.file];
            for x in &r.initial {
                pieces.extend(
                    pieces_for(
                        off + x.offset,
                        x.len,
                        self.layout.piece_length,
                        self.layout.total,
                    )
                    .filter(|p| !self.verified.contains_key(p)),
                );
            }
        }
        let reserve: u64 = pieces
            .iter()
            .filter_map(|p| self.layout.piece_len(*p))
            .sum();
        per_file.max(self.budget_left().saturating_sub(reserve))
    }

    /// Sniff one target: first read, then sniff's follow-ups, within the per-file caps.
    async fn sniff_file(&mut self, t: &Target, whole: bool, cap: u64) -> Sniffed {
        let off = self.layout.offsets[t.file];
        let size = self.layout.sizes[t.file];
        let caps = self.caps_for(t);
        let det = Detector::new().caps(caps.clone());
        let mut file = SparseFile::new(size).with_name(self.files[t.file].path.clone());
        let mut ranges = t.initial.clone();
        let mut rounds = 0u32;
        let mut first = None;
        let mut contributors = HashSet::new();
        loop {
            // Per-file byte cap: what is left of it, range by range. Whole pieces are charged
            // what they cost (D120): a 16-byte box header behind a 1 MiB piece costs 1 MiB, and
            // a follow-up that does not fit is skipped, so a later, cheaper one still can.
            let mut room = cap.saturating_sub(file.known_bytes());
            let mut blocks = Vec::new();
            let mut seen = HashSet::new();
            let mut charged = HashSet::new();
            for r in ranges.iter().take(caps.max_ranges) {
                let len = if whole {
                    r.len.min(caps.max_range)
                } else {
                    r.len.min(caps.max_range).min(room)
                };
                if len == 0 {
                    continue;
                }
                if whole {
                    let cost: u64 = pieces_for(
                        off + r.offset,
                        len,
                        self.layout.piece_length,
                        self.layout.total,
                    )
                    .filter(|p| !self.verified.contains_key(p) && !charged.contains(p))
                    .filter_map(|p| self.layout.piece_len(p))
                    .sum();
                    // The first read always goes (a piece may be longer than the cap).
                    if rounds > 0 && cost > room {
                        continue;
                    }
                    charged.extend(pieces_for(
                        off + r.offset,
                        len,
                        self.layout.piece_length,
                        self.layout.total,
                    ));
                    room = room.saturating_sub(cost);
                } else {
                    room -= len;
                }
                for b in blocks_for(
                    off + r.offset,
                    len,
                    self.layout.piece_length,
                    self.layout.total,
                    whole,
                ) {
                    if seen.insert(b) {
                        blocks.push(b);
                    }
                }
            }
            if blocks.is_empty() {
                break;
            }
            let until = self.deadline;
            self.get(&blocks, whole, None, until).await;
            let before = file.known_bytes();
            self.fill(&mut file, off, size, &blocks, whole, &mut contributors);
            let grown = file.known_bytes() > before;
            let d2 = det.clone();
            let Ok((f, a)) = tokio::task::spawn_blocking(move || {
                let a = d2.analyze(&file);
                (file, a)
            })
            .await
            else {
                return Sniffed {
                    detection: None,
                    first,
                    contributors,
                    known: Vec::new(),
                };
            };
            file = f;
            if first.is_none() && file.known_bytes() > 0 {
                first = Some(a.detection.clone());
            }
            if a.need.is_empty()
                || !grown
                || rounds >= caps.rounds
                || file.known_bytes() >= cap
                || self.out_of_time()
                || self.budget_left() == 0
            {
                break;
            }
            ranges = a.need;
            rounds += 1;
        }
        let known: Vec<Range> = file
            .ranges()
            .map(|(o, b)| Range::new(o, b.len() as u64))
            .collect();
        if file.known_bytes() == 0 {
            return Sniffed {
                detection: None,
                first,
                contributors,
                known,
            };
        }
        let detection = tokio::task::spawn_blocking(move || det.finish(&file))
            .await
            .ok()
            .and_then(|o| o.detection().cloned());
        Sniffed {
            detection,
            first,
            contributors,
            known,
        }
    }

    /// Insert what is known of `blocks` into the file: verified pieces as verified, single
    /// blocks (block mode only) as unverified.
    fn fill(
        &self,
        file: &mut SparseFile,
        off: u64,
        size: u64,
        blocks: &[Block],
        whole: bool,
        contributors: &mut HashSet<usize>,
    ) {
        let pl = self.layout.piece_length;
        let mut done = HashSet::new();
        for b in blocks {
            if let Some(p) = self.verified.get(&b.index) {
                if done.insert(b.index) {
                    insert_clipped(file, off, size, u64::from(b.index) * pl, p, true);
                }
                continue;
            }
            if whole {
                continue;
            }
            if let Some(c) = self.cache.get(b) {
                let abs = u64::from(b.index) * pl + u64::from(b.begin);
                if insert_clipped(file, off, size, abs, &c.data, false) {
                    contributors.insert(c.peer);
                }
            }
        }
    }

    fn can_connect(&self) -> bool {
        self.connecting.is_none()
            && self.peers.len() < self.cfg.peers
            && self.attempts < self.cfg.connect_attempts
            && !self.candidates.is_empty()
    }

    fn maybe_connect(&mut self, queue: &VecDeque<Block>) {
        if !self.can_connect() {
            return;
        }
        let now = Instant::now();
        let alive: Vec<&Peer> = self.alive().collect();
        let none = alive.is_empty();
        let lacking = queue.iter().any(|b| {
            !alive
                .iter()
                .any(|p| p.avail.has(b.index) && !p.refused.contains(b))
        });
        let choked = alive.iter().all(|p| {
            p.choked && now.saturating_duration_since(p.since) >= self.cfg.second_peer_after
        });
        if !(none || lacking || choked) {
            return;
        }
        let connected: HashSet<SocketAddrV4> = alive.iter().map(|p| p.addr).collect();
        while let Some(addr) = self.candidates.pop_front() {
            if self.struck_addrs.contains(&addr) || connected.contains(&addr) {
                continue;
            }
            self.attempts += 1;
            let f = self.fetcher.clone();
            let hash = self.hash;
            self.connecting = Some(Box::pin(
                async move { f.connect_for_probe(hash, addr).await },
            ));
            return;
        }
    }

    /// Fetch `blocks` (whole pieces when `whole`), from peer `only` when given, until done,
    /// stuck, or `until`.
    async fn get(&mut self, blocks: &[Block], whole: bool, only: Option<usize>, until: Instant) {
        let mut queue = VecDeque::new();
        let mut wanted = HashSet::new();
        for b in blocks {
            if self.verified.contains_key(&b.index) || self.failed_pieces.contains(&b.index) {
                continue;
            }
            if only.is_none() && !whole && self.cache.contains_key(b) {
                continue;
            }
            if whole && !self.asm.contains_key(&b.index) {
                let (Some(len), Some(h)) = (
                    self.layout.piece_len(b.index),
                    self.layout.piece_hash(b.index),
                ) else {
                    continue;
                };
                self.asm
                    .insert(b.index, PieceAssembler::new(b.index, len as u32, h));
            }
            wanted.insert(*b);
            // A block already held unverified goes into the assembler when its peer can give
            // the rest of the piece (a lie then fails the hash and names it).
            if whole && let Some(c) = self.cache.remove(b) {
                let alive = self.peers.get(c.peer).is_some_and(Option::is_some);
                let pinned = self.owner.get(&b.index).is_none_or(|o| *o == c.peer);
                if alive && pinned {
                    self.owner.insert(b.index, c.peer);
                    self.deliver_whole(*b, c.data, c.peer, &mut queue, &wanted);
                    continue;
                }
            }
            if !queue.contains(b) {
                queue.push_back(*b);
            }
        }
        // Blocks already requested from a peer are waited for, not asked again.
        let inflight: HashSet<Block> = self
            .alive()
            .filter(|p| only.is_none_or(|o| o == p.slot))
            .flat_map(|p| p.inflight.iter().map(|(b, _)| *b))
            .collect();
        queue.retain(|b| !inflight.contains(b));
        loop {
            if Instant::now() >= until.min(self.deadline) {
                break;
            }
            self.assign(&mut queue, &wanted, only).await;
            let outstanding = self
                .alive()
                .filter(|p| only.is_none_or(|o| o == p.slot))
                .any(|p| p.inflight.iter().any(|(b, _)| wanted.contains(b)));
            if queue.is_empty() && !outstanding {
                break;
            }
            if only.is_none() {
                self.maybe_connect(&queue);
            }
            if !outstanding && self.stuck(&queue, only) {
                break;
            }
            self.pump(&mut queue, &wanted, until).await;
        }
    }

    /// True when nothing more can come: no request out, no connection pending, and no live
    /// peer that may still unchoke us with a wanted piece.
    fn stuck(&mut self, queue: &VecDeque<Block>, only: Option<usize>) -> bool {
        if queue.is_empty() {
            return true;
        }
        if self.connecting.is_some() && only.is_none() {
            return false;
        }
        let smallest = queue.iter().map(|b| u64::from(b.len)).min().unwrap_or(0);
        if self.budget_left() < smallest {
            self.budget_hit = true;
            return true;
        }
        let now = Instant::now();
        let mut hope = false;
        let mut saw = false;
        for p in self
            .peers
            .iter()
            .flatten()
            .filter(|p| only.is_none_or(|o| o == p.slot))
        {
            let has = queue
                .iter()
                .any(|b| p.avail.has(b.index) && !p.refused.contains(b));
            if has {
                saw = true;
                if p.choked && now.saturating_duration_since(p.since) < self.cfg.unchoke_wait {
                    hope = true;
                }
            }
        }
        self.saw_pieces |= saw;
        if hope {
            return false;
        }
        if only.is_none() && self.can_connect() {
            self.maybe_connect(queue);
            return self.connecting.is_none();
        }
        true
    }

    /// Send requests to every peer with room.
    async fn assign(
        &mut self,
        queue: &mut VecDeque<Block>,
        wanted: &HashSet<Block>,
        only: Option<usize>,
    ) {
        let now = Instant::now();
        let cap = self.cfg.max_bytes;
        let mut spent = self.bytes_in + self.inflight_bytes();
        for slot in 0..self.peers.len() {
            if only.is_some_and(|o| o != slot) {
                continue;
            }
            let Some(p) = self.peers[slot].as_mut() else {
                continue;
            };
            let mut msgs = Vec::new();
            let mut asked = Vec::new();
            while p.has_room() {
                let owner = &self.owner;
                let Some(i) = queue.iter().position(|b| {
                    p.can_request(b) && owner.get(&b.index).is_none_or(|o| *o == slot)
                }) else {
                    break;
                };
                let b = queue[i];
                if spent + u64::from(b.len) > cap {
                    self.budget_hit = true;
                    break;
                }
                queue.remove(i);
                if self.asm.contains_key(&b.index) {
                    self.owner.insert(b.index, slot);
                }
                spent += u64::from(b.len);
                if p.inflight.is_empty() {
                    p.last_block = now;
                }
                p.inflight.push_back((b, now));
                msgs.push(ProbeMsg::Request(b));
                asked.push(b);
            }
            // A block asked again is no longer awaited from a choke that dropped it: only
            // the new request may answer it (else a peer answering both gets it counted
            // twice, past `max_bytes`).
            for b in &asked {
                for q in self.peers.iter_mut().flatten() {
                    q.late.remove(b);
                }
            }
            let Some(p) = self.peers[slot].as_mut() else {
                continue;
            };
            if !msgs.is_empty() || queue.iter().any(|b| p.avail.has(b.index)) {
                self.saw_pieces = true;
            }
            if !msgs.is_empty() && p.out.send_all(&msgs).await.is_err() {
                self.kill(slot, "write", Some((queue, wanted)));
            }
        }
    }

    /// One turn of the event loop: a peer message, a finished connection, or a tick.
    async fn pump(&mut self, queue: &mut VecDeque<Block>, wanted: &HashSet<Block>, until: Instant) {
        let wake = (Instant::now() + TICK).min(until).min(self.deadline);
        let connecting = self.connecting.is_some();
        tokio::select! {
            m = self.rx.recv() => {
                if let Some((slot, ev)) = m {
                    self.on_event(slot, ev, queue, wanted).await;
                }
            }
            c = async { self.connecting.as_mut().expect("guarded").await }, if connecting => {
                self.connecting = None;
                if let Ok(c) = c {
                    self.add_peer(c);
                }
            }
            _ = tokio::time::sleep_until(wake) => {}
        }
        // Drain what else arrived, without waiting.
        while let Ok((slot, ev)) = self.rx.try_recv() {
            self.on_event(slot, ev, queue, wanted).await;
        }
        self.timeouts(queue, wanted);
        self.serve_tick().await;
    }

    fn timeouts(&mut self, queue: &mut VecDeque<Block>, wanted: &HashSet<Block>) {
        let now = Instant::now();
        let stall = self.cfg.stall;
        let stalled: Vec<usize> = self
            .alive()
            .filter(|p| {
                !p.inflight.is_empty() && now.saturating_duration_since(p.last_block) > stall
            })
            .map(|p| p.slot)
            .collect();
        for s in stalled {
            self.kill(s, "stall", Some((queue, wanted)));
        }
    }

    async fn on_event(
        &mut self,
        slot: usize,
        ev: Event,
        queue: &mut VecDeque<Block>,
        wanted: &HashSet<Block>,
    ) {
        let pieces = self.pieces();
        let m = match ev {
            Event::Closed(r) => {
                self.kill(slot, r, Some((queue, wanted)));
                return;
            }
            Event::Msg(m) => m,
        };
        match m {
            PeerMsg::Request(b) => return self.on_request(slot, b).await,
            PeerMsg::Cancel(b) => {
                self.serve_q.retain(|(s, x)| !(*s == slot && *x == b));
                return;
            }
            PeerMsg::Interested => {
                if let Some(p) = self.peer_mut(slot) {
                    p.interested = true;
                }
                return self.maybe_unchoke(slot).await;
            }
            PeerMsg::Piece { index, begin, data } => {
                return self.on_piece(slot, index, begin, data, queue, wanted);
            }
            _ => {}
        }
        let Some(p) = self.peers.get_mut(slot).and_then(Option::as_mut) else {
            return;
        };
        let mut fail: Option<&'static str> = None;
        match m {
            PeerMsg::Choke => {
                p.choked = true;
                if !p.fast {
                    // BEP 3: a choke drops every outstanding request.
                    let gone: Vec<Block> = p.inflight.drain(..).map(|(b, _)| b).collect();
                    for b in gone {
                        p.late.insert(b);
                        if wanted.contains(&b) && !queue.contains(&b) {
                            queue.push_back(b);
                        }
                    }
                }
            }
            PeerMsg::Unchoke => {
                p.choked = false;
                p.last_block = Instant::now();
                self.ever_unchoked = true;
            }
            PeerMsg::NotInterested => p.interested = false,
            PeerMsg::Have(i) => {
                if p.on_have(i, pieces).is_err() {
                    fail = Some("bad_have");
                }
            }
            PeerMsg::Bitfield(bf) => match bitfield(&bf, pieces) {
                Ok(v) => p.avail = Avail::Pieces(v),
                Err(e) => fail = Some(e),
            },
            PeerMsg::HaveAll => p.avail = Avail::All,
            PeerMsg::HaveNone => p.avail = Avail::Pieces(vec![false; pieces as usize]),
            PeerMsg::AllowedFast(i) => {
                if i < pieces && p.allowed_fast.len() < 4096 {
                    p.allowed_fast.insert(i);
                }
            }
            PeerMsg::Reject(b) => {
                if let Some(i) = p.inflight.iter().position(|(x, _)| *x == b) {
                    p.inflight.remove(i);
                    p.refused.insert(b);
                    if wanted.contains(&b) && !queue.contains(&b) {
                        queue.push_back(b);
                    }
                    // A piece fetched whole from this peer goes to another one, whole.
                    if self.owner.get(&b.index) == Some(&slot) {
                        // Its other requests to this peer are cancelled first.
                        let mine: Vec<Block> = p
                            .inflight
                            .iter()
                            .filter(|(x, _)| x.index == b.index)
                            .map(|(x, _)| *x)
                            .collect();
                        p.inflight.retain(|(x, _)| x.index != b.index);
                        let cancels: Vec<ProbeMsg> =
                            mine.iter().map(|x| ProbeMsg::Cancel(*x)).collect();
                        for x in &mine {
                            p.late.insert(*x);
                        }
                        if p.out.send_all(&cancels).await.is_err() {
                            fail = Some("write");
                        }
                        for x in self.release(b.index) {
                            if wanted.contains(&x) && !queue.contains(&x) {
                                queue.push_back(x);
                            }
                        }
                    }
                }
            }
            PeerMsg::Extended { id, payload } => {
                if id == wire::OUR_UT_METADATA
                    && let Some(piece) = metadata_request(&payload)
                {
                    p.metadata_rejects += 1;
                    if p.metadata_rejects > METADATA_REJECTS {
                        fail = Some("metadata_flood");
                    } else if let Some(ut) = p.ut_metadata {
                        let r = ProbeMsg::MetadataReject {
                            ut_metadata: ut,
                            piece,
                        };
                        if p.out.send(&r).await.is_err() {
                            fail = Some("write");
                        }
                    }
                }
            }
            _ => {}
        }
        if let Some(r) = fail {
            self.kill(slot, r, Some((queue, wanted)));
        }
    }

    /// A `piece` message: only blocks we asked this peer for are taken (another drops it).
    fn on_piece(
        &mut self,
        slot: usize,
        index: u32,
        begin: u32,
        data: Vec<u8>,
        queue: &mut VecDeque<Block>,
        wanted: &HashSet<Block>,
    ) {
        let b = Block {
            index,
            begin,
            len: data.len() as u32,
        };
        let Some(p) = self.peers.get_mut(slot).and_then(Option::as_mut) else {
            return;
        };
        let requested = if let Some(i) = p.inflight.iter().position(|(x, _)| *x == b) {
            p.inflight.remove(i);
            true
        } else {
            p.late.remove(&b)
        };
        if !requested {
            self.kill(slot, "unrequested", Some((queue, wanted)));
            return;
        }
        // `assign` never asks past the budget; a late block (dropped by a choke) may still
        // come on top of it. The payload cap holds whatever peers send.
        if self.bytes_in + u64::from(b.len) > self.cfg.max_bytes {
            self.budget_hit = true;
            self.kill(slot, "over_budget", Some((queue, wanted)));
            return;
        }
        let Some(p) = self.peers.get_mut(slot).and_then(Option::as_mut) else {
            return;
        };
        p.last_block = Instant::now();
        p.bytes_in += u64::from(b.len);
        p.got_any = true;
        self.bytes_in += u64::from(b.len);
        queue.retain(|x| *x != b);
        self.deliver(b, data, slot, queue, wanted);
    }

    fn deliver(
        &mut self,
        b: Block,
        data: Vec<u8>,
        slot: usize,
        queue: &mut VecDeque<Block>,
        wanted: &HashSet<Block>,
    ) {
        if self.asm.contains_key(&b.index) {
            self.deliver_whole(b, data, slot, queue, wanted);
            return;
        }
        if let Some((s, got)) = &mut self.side
            && *s == slot
        {
            got.insert(b, data);
            return;
        }
        if self.verified.contains_key(&b.index) {
            return;
        }
        self.cache.entry(b).or_insert(Cached { data, peer: slot });
    }

    /// A block of a piece fetched whole. True when the piece is now verified.
    fn deliver_whole(
        &mut self,
        b: Block,
        data: Vec<u8>,
        slot: usize,
        queue: &mut VecDeque<Block>,
        wanted: &HashSet<Block>,
    ) -> bool {
        let Some(a) = self.asm.get_mut(&b.index) else {
            return false;
        };
        if a.insert(b.begin, data, slot).is_err() {
            self.kill(slot, "bad_block", Some((queue, wanted)));
            return false;
        }
        if !a.is_complete() {
            return false;
        }
        let a = self.asm.remove(&b.index).expect("present");
        self.owner.remove(&b.index);
        match a.finish() {
            Ok(v) => {
                self.cache.retain(|k, _| k.index != b.index);
                self.verified.insert(b.index, v.bytes().to_vec());
                // Upload mode only: the verified piece is held for serving and announced.
                if let Some(g) = &mut self.gate {
                    let have = ServeMsg::have(g.hold(v));
                    self.announce.push(have);
                }
                true
            }
            Err(AsmError::Mismatch(peers)) => {
                // One peer sent the whole piece (pinned): it lied. Several (cannot happen
                // with pinning): nobody is named, the piece is asked again.
                if let [p] = peers[..] {
                    self.strike(p, Some((queue, wanted)));
                }
                let index = b.index;
                if self.retried.insert(index)
                    && let (Some(len), Some(h)) =
                        (self.layout.piece_len(index), self.layout.piece_hash(index))
                {
                    self.asm
                        .insert(index, PieceAssembler::new(index, len as u32, h));
                    for blk in blocks_for(
                        u64::from(index) * self.layout.piece_length,
                        len,
                        self.layout.piece_length,
                        self.layout.total,
                        true,
                    ) {
                        if wanted.contains(&blk) && !queue.contains(&blk) {
                            queue.push_back(blk);
                        }
                    }
                } else {
                    self.failed_pieces.insert(index);
                }
                false
            }
            Err(AsmError::BadBlock) => false,
        }
    }

    async fn maybe_unchoke(&mut self, slot: usize) {
        let Some(g) = &self.gate else { return };
        if g.capped() {
            return;
        }
        let Some(p) = self.peers.get_mut(slot).and_then(Option::as_mut) else {
            return;
        };
        if !p.interested || p.unchoked_by_us {
            return;
        }
        if !g.held_pieces().any(|h| !p.avail.has(h.index())) {
            return;
        }
        let m = ServeMsg::unchoke(g);
        if p.out.serve(&m).await.is_ok() {
            p.unchoked_by_us = true;
        }
    }

    async fn on_request(&mut self, slot: usize, b: Block) {
        let ok = match (&self.gate, self.peers.get(slot).and_then(Option::as_ref)) {
            (Some(g), Some(p)) => {
                p.unchoked_by_us
                    && !g.capped()
                    && b.len > 0
                    && b.len <= BLOCK
                    && g.held(b.index)
                        .is_some_and(|h| b.begin.checked_add(b.len).is_some_and(|e| e <= h.len()))
                    && self.serve_q.len() < SERVE_QUEUE
            }
            _ => false,
        };
        if ok {
            self.serve_q.push_back((slot, b));
            return;
        }
        let Some(p) = self.peer_mut(slot) else { return };
        if p.fast && p.rejects_sent < REJECTS {
            p.rejects_sent += 1;
            let _ = p.out.send(&ProbeMsg::RejectRequest(b)).await;
        }
    }

    /// Upload mode: announce new verified pieces and serve queued requests within the caps.
    async fn serve_tick(&mut self) {
        if self.gate.is_none() {
            return;
        }
        // HAVE for every newly held piece, to every live peer of this probe.
        let new: Vec<ServeMsg> = std::mem::take(&mut self.announce);
        if !new.is_empty() {
            for slot in 0..self.peers.len() {
                if let Some(p) = self.peers[slot].as_mut() {
                    for m in &new {
                        let _ = p.out.serve(m).await;
                    }
                }
                self.maybe_unchoke(slot).await;
            }
        }
        let now = Instant::now();
        if self.serve_after.is_some_and(|t| now < t) {
            return;
        }
        self.serve_after = None;
        while let Some(&(slot, b)) = self.serve_q.front() {
            let Some(g) = self.gate.as_mut() else { return };
            if self
                .peers
                .get(slot)
                .and_then(Option::as_ref)
                .is_none_or(|p| !p.unchoked_by_us)
            {
                self.serve_q.pop_front();
                continue;
            }
            match g.take(b.len, now) {
                Take::Ok => {
                    self.serve_q.pop_front();
                    let msg = g
                        .held(b.index)
                        .and_then(|h| ServeMsg::piece(h, b.begin, b.len));
                    if let (Some(m), Some(p)) = (msg, self.peers[slot].as_mut())
                        && p.out.serve(&m).await.is_err()
                    {
                        self.kill(slot, "write", None);
                    }
                }
                Take::Wait(d) => {
                    self.serve_after = Some(now + d);
                    return;
                }
                Take::Capped => {
                    // The probe's upload cap: choke everyone, refuse what is queued.
                    let choke = ServeMsg::choke(g);
                    let queued: Vec<(usize, Block)> = self.serve_q.drain(..).collect();
                    for slot in 0..self.peers.len() {
                        if let Some(p) = self.peers[slot].as_mut()
                            && p.unchoked_by_us
                        {
                            let _ = p.out.serve(&choke).await;
                            p.unchoked_by_us = false;
                        }
                    }
                    for (slot, b) in queued {
                        if let Some(p) = self.peers.get_mut(slot).and_then(Option::as_mut)
                            && p.fast
                        {
                            let _ = p.out.send(&ProbeMsg::RejectRequest(b)).await;
                        }
                    }
                    return;
                }
            }
        }
    }

    /// Upload mode: keep serving the probe's peers a little after the last file.
    async fn linger(&mut self) {
        let Some(g) = &self.gate else { return };
        if g.held_pieces().next().is_none() {
            return;
        }
        let until = (Instant::now() + self.cfg.upload_linger).min(self.deadline);
        let mut queue = VecDeque::new();
        let wanted = HashSet::new();
        let mut quiet = 0u32;
        while Instant::now() < until {
            self.serve_tick().await;
            let busy = !self.serve_q.is_empty()
                || self.alive().any(|p| p.unchoked_by_us && p.interested)
                || !self.announce.is_empty();
            if busy {
                quiet = 0;
            } else {
                quiet += 1;
                // Nobody to serve for half a second: done.
                if quiet > 5 {
                    break;
                }
            }
            if self.gate.as_ref().is_some_and(UploadGate::capped) && self.serve_q.is_empty() {
                break;
            }
            self.pump(&mut queue, &wanted, until).await;
        }
    }
}

/// Insert the part of `data` (at absolute offset `abs`) that falls inside the file at `off`.
/// True when something was inserted.
fn insert_clipped(
    file: &mut SparseFile,
    off: u64,
    size: u64,
    abs: u64,
    data: &[u8],
    verified: bool,
) -> bool {
    let end = abs + data.len() as u64;
    let s = abs.max(off);
    let e = end.min(off + size);
    if s >= e {
        return false;
    }
    let bytes = data[(s - abs) as usize..(e - abs) as usize].to_vec();
    if verified {
        file.insert(s - off, bytes);
    } else {
        file.insert_unverified(s - off, bytes);
    }
    true
}

//! Header probes (docs/PROBES.md, D117; feature `probe`, crawl workers only). After a worker
//! fetched a torrent's info dict it already holds a connection to a peer that has the torrent:
//! the probe keeps it ([`crate::metainfo::Fetcher::fetch_with_handoff`]), asks for the few
//! 16 KiB blocks that `crates/sniff` needs for each target file ([`plan`]), and runs sniff on
//! them to learn real file types, console game ids and video/audio quality.
//!
//! Policy (user decisions, D117):
//! - only the blocks sniff asks for, marked unverified (`SparseFile::insert_unverified`); whole
//!   pieces fetched and SHA-1 checked when the piece length is at most
//!   [`ProbeConfig::whole_piece_max`], and when a result is surprising (`suspicious_executable`,
//!   `extension_mismatch`, or two peers disagreeing) before it counts as verified;
//! - per torrent at most [`ProbeConfig::max_bytes`]; per file sniff's [`sniff::Caps`];
//! - every connection goes through the metadata [`Fetcher`]: same address filter, attempt pace
//!   and brake, open-connection cap, peer id and client name (D55, D112, D115);
//! - upload off (default): only [`msg::ProbeMsg`] goes out (`HAVE_NONE` or nothing, never
//!   `unchoke`, `have`, `bitfield` or `piece`); upload on ([`ProbeConfig::upload`]): see
//!   [`serve`];
//! - nothing is written to disk; nothing here logs a name or path (docs/DHT.md §7.1).
//!
//! The probe never decides whether a torrent may be probed: the worker checks the blocklist,
//! the adult class, the brakes and the dict size first ([`Skip`] counts why it did not).

mod drive;
pub mod msg;
mod peer;
pub mod plan;
pub mod serve;
pub mod verify;

use std::net::SocketAddrV4;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::time::Instant;

use crate::metainfo::{Fetcher, InfoFile, PeerConn};

/// Version of the probe's results (wire and storage, D117).
pub const PROBE_VERSION: u16 = 1;
/// Dicts larger than this are not probed (the probe decodes the dict again for the layout).
pub const MAX_DICT: usize = 2 * 1024 * 1024;

/// Settings of the header probes. The `MAGNETAR_PROBE_*` settings fill the first fields; the
/// rest are documented constants (docs/PROBES.md).
#[derive(Clone, Debug)]
pub struct ProbeConfig {
    /// `MAGNETAR_PROBE_UPLOAD`: announce and serve whole verified pieces to the probe's peers.
    /// **On is sharing.** Off by default.
    pub upload: bool,
    /// `MAGNETAR_PROBE_WHOLE_PIECE_MAX`: pieces up to this length are fetched whole and
    /// SHA-1 checked.
    pub whole_piece_max: u64,
    /// `MAGNETAR_PROBE_MAX_BYTES`: piece bytes one probe may download.
    pub max_bytes: u64,
    /// `MAGNETAR_PROBE_PEERS`: connections one probe may use (the hand-off peer included).
    pub peers: usize,
    /// `MAGNETAR_PROBE_PARALLEL`: probes at once.
    pub parallel: usize,
    /// `MAGNETAR_PROBE_PER_MIN`: probes started per minute (no burst).
    pub per_min: u32,
    /// Target files per torrent (disc metadata files count as one).
    pub max_targets: usize,
    /// How long a choked peer is waited for.
    pub unchoke_wait: Duration,
    /// A peer with requests out and no block for this long is dropped.
    pub stall: Duration,
    /// Whole probe.
    pub session: Duration,
    /// Requests outstanding per peer (capped by the peer's BEP 10 `reqq`).
    pub pipeline: usize,
    /// A second peer is connected when the first one is still choked after this long, lacks
    /// a piece, or is gone.
    pub second_peer_after: Duration,
    /// Connection attempts per probe for further peers.
    pub connect_attempts: usize,
    /// Upload caps per probe (upload mode only).
    pub upload_max_bytes: u64,
    pub upload_rate: u64,
    /// After the last file, upload mode keeps serving its peers this long (within `session`).
    pub upload_linger: Duration,
    /// Per-file sniff caps; `head` is set per target ([`plan`]).
    pub caps: sniff::Caps,
    /// Detections below this confidence are not results (sniff's "store as fact" line).
    pub min_confidence: u8,
    /// Two peers disagree when the labels differ or the confidences differ by more.
    pub agreement_slack: u8,
}

impl Default for ProbeConfig {
    fn default() -> Self {
        Self {
            upload: false,
            whole_piece_max: 1024 * 1024,
            max_bytes: 8 * 1024 * 1024,
            peers: 2,
            parallel: 8,
            per_min: 30,
            max_targets: 8,
            unchoke_wait: Duration::from_secs(15),
            stall: Duration::from_secs(10),
            session: Duration::from_secs(45),
            pipeline: 64,
            second_peer_after: Duration::from_secs(3),
            connect_attempts: 4,
            upload_max_bytes: serve::UPLOAD_MAX_BYTES,
            upload_rate: serve::UPLOAD_RATE,
            upload_linger: Duration::from_secs(5),
            caps: sniff::Caps {
                head: plan::HEAD,
                tail: 0,
                rounds: 6,
                max_bytes: 4 * 1024 * 1024,
                max_range: 4 * 1024 * 1024,
                max_ranges: 16,
                inner_bytes: 64 * 1024,
                max_depth: 2,
            },
            min_confidence: 80,
            agreement_slack: 15,
        }
    }
}

/// One torrent to probe.
pub struct ProbeJob {
    pub info_hash: [u8; 20],
    /// The verified info dict (the piece layout and hashes come from it).
    pub raw: Arc<[u8]>,
    /// The display files, as [`crate::metainfo::InfoSummary::files`] (same order).
    pub files: Vec<InfoFile>,
    /// The hand-off connection, when there is one.
    pub conn: Option<PeerConn>,
    /// Further peers, in order of preference (the hand-off's `answered`, then the DHT's).
    pub peers: Vec<SocketAddrV4>,
}

/// One sniffed file.
#[derive(Clone, Debug)]
pub struct FileResult {
    /// Display index.
    pub index: u32,
    pub size: u64,
    pub detection: sniff::Detection,
    /// Every byte the verdict read was hash-checked (`!detection.flags.unverified`).
    pub verified: bool,
    /// Peers whose bytes gave the same verdict (2 after an agreement read).
    pub agreed: u8,
    /// `surprise_verified`, `surprise_changed`, `surprise_unverified`, `disagree`.
    pub notes: Vec<&'static str>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProbeStatus {
    /// Every target was sniffed to the end.
    Ok,
    /// Some results; some targets missing or cut short.
    Partial,
    /// Peers had the pieces and never unchoked us.
    Choked,
    /// No peer had a target piece.
    NoPieces,
    Failed(&'static str),
}

impl ProbeStatus {
    pub fn code(&self) -> &'static str {
        match self {
            ProbeStatus::Ok => "ok",
            ProbeStatus::Partial => "partial",
            ProbeStatus::Choked => "choked",
            ProbeStatus::NoPieces => "no_pieces",
            ProbeStatus::Failed(r) => r,
        }
    }
}

#[derive(Clone, Debug)]
pub struct ProbeResult {
    pub status: ProbeStatus,
    pub files: Vec<FileResult>,
    /// Piece payload received.
    pub bytes_in: u64,
    /// Piece payload served (upload mode).
    pub uploaded: u64,
    /// Connections used (the hand-off included).
    pub peers_used: u8,
    /// Some peer unchoked us.
    pub unchoked: bool,
    /// A target was cut short (budget, rounds, time).
    pub incomplete: bool,
    /// Surprise checks started.
    pub surprises: u32,
}

impl ProbeResult {
    fn failed(r: &'static str) -> Self {
        Self {
            status: ProbeStatus::Failed(r),
            files: Vec::new(),
            bytes_in: 0,
            uploaded: 0,
            peers_used: 0,
            unchoked: false,
            incomplete: false,
            surprises: 0,
        }
    }
}

/// Why a probe did not start (the worker's checks, then [`Prober::try_slot`]'s).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Skip {
    Rate,
    Busy,
    Adult,
    Blocklist,
    NoBlocklist,
    Brake,
    Pressure,
    BigDict,
    NoHandoff,
}

const SKIPS: [Skip; 9] = [
    Skip::Rate,
    Skip::Busy,
    Skip::Adult,
    Skip::Blocklist,
    Skip::NoBlocklist,
    Skip::Brake,
    Skip::Pressure,
    Skip::BigDict,
    Skip::NoHandoff,
];

impl Skip {
    pub fn code(self) -> &'static str {
        match self {
            Skip::Rate => "rate",
            Skip::Busy => "busy",
            Skip::Adult => "adult",
            Skip::Blocklist => "blocklist",
            Skip::NoBlocklist => "no_blocklist",
            Skip::Brake => "brake",
            Skip::Pressure => "pressure",
            Skip::BigDict => "big_dict",
            Skip::NoHandoff => "no_handoff",
        }
    }
}

/// Probe counters for the worker heartbeat (never addresses or names).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProbeStats {
    pub started: u64,
    pub ok: u64,
    pub partial: u64,
    pub choked: u64,
    pub no_pieces: u64,
    pub failed: u64,
    /// Probes not started, by [`Skip::code`].
    pub skipped: Vec<(String, u64)>,
    pub bytes_in: u64,
    pub uploaded: u64,
    pub files: u64,
    pub verified_files: u64,
    pub surprises: u64,
}

#[derive(Default)]
struct Counters {
    started: AtomicU64,
    ok: AtomicU64,
    partial: AtomicU64,
    choked: AtomicU64,
    no_pieces: AtomicU64,
    failed: AtomicU64,
    skipped: [AtomicU64; 9],
    bytes_in: AtomicU64,
    uploaded: AtomicU64,
    files: AtomicU64,
    verified_files: AtomicU64,
    surprises: AtomicU64,
}

fn add(a: &AtomicU64, n: u64) {
    a.fetch_add(n, Ordering::Relaxed);
}

struct Inner {
    cfg: ProbeConfig,
    fetcher: Fetcher,
    slots: Arc<Semaphore>,
    /// The next probe may start at this instant (`per_min`, no burst).
    next_at: Mutex<Option<Instant>>,
    c: Counters,
}

/// The header probes of one worker: global bounds (parallel slots, per-minute pace) and
/// counters. Cheap to clone.
#[derive(Clone)]
pub struct Prober {
    inner: Arc<Inner>,
}

/// A started probe's place in [`ProbeConfig::parallel`].
pub struct ProbeSlot {
    _permit: OwnedSemaphorePermit,
}

impl Prober {
    /// `fetcher` must be the worker's metadata fetcher (shared bounds, D117) with
    /// [`crate::metainfo::FetchConfig::probe`] on.
    pub fn new(cfg: ProbeConfig, fetcher: Fetcher) -> Self {
        let slots = Arc::new(Semaphore::new(cfg.parallel.clamp(1, 4096)));
        Self {
            inner: Arc::new(Inner {
                cfg,
                fetcher,
                slots,
                next_at: Mutex::new(None),
                c: Counters::default(),
            }),
        }
    }

    pub fn config(&self) -> &ProbeConfig {
        &self.inner.cfg
    }

    /// Count a probe the worker did not start.
    pub fn skip(&self, why: Skip) {
        if let Some(i) = SKIPS.iter().position(|s| *s == why) {
            add(&self.inner.c.skipped[i], 1);
        }
    }

    /// A slot when one is free and the per-minute pace allows a probe now (never waits).
    pub fn try_slot(&self) -> Option<ProbeSlot> {
        let now = Instant::now();
        let interval = Duration::from_secs_f64(60.0 / f64::from(self.inner.cfg.per_min.max(1)));
        {
            let next = self.inner.next_at.lock().unwrap();
            if next.is_some_and(|t| now < t) {
                drop(next);
                self.skip(Skip::Rate);
                return None;
            }
        }
        let Ok(permit) = self.inner.slots.clone().try_acquire_owned() else {
            self.skip(Skip::Busy);
            return None;
        };
        *self.inner.next_at.lock().unwrap() = Some(now + interval);
        Some(ProbeSlot { _permit: permit })
    }

    /// A slot, waiting for a free one and for the probe's turn of the per-minute pace (D118:
    /// backfill probes, whose torrents were leased for probing, wait instead of being skipped).
    /// Turns are handed out in call order, never two within one interval; nothing is counted
    /// as skipped. The caller bounds the wait (`tokio::time::timeout`); a waiter that gives up
    /// after it took its turn leaves that turn unused.
    pub async fn slot(&self) -> ProbeSlot {
        let permit = self
            .inner
            .slots
            .clone()
            .acquire_owned()
            .await
            .expect("the probe semaphore is never closed");
        let interval = Duration::from_secs_f64(60.0 / f64::from(self.inner.cfg.per_min.max(1)));
        let at = {
            let mut next = self.inner.next_at.lock().unwrap();
            let now = Instant::now();
            let at = next.map_or(now, |t| t.max(now));
            *next = Some(at + interval);
            at
        };
        tokio::time::sleep_until(at).await;
        ProbeSlot { _permit: permit }
    }

    /// Run one probe (at most [`ProbeConfig::session`] plus a few seconds of grace).
    pub async fn probe(&self, slot: ProbeSlot, job: ProbeJob) -> ProbeResult {
        let c = &self.inner.c;
        add(&c.started, 1);
        let cfg = &self.inner.cfg;
        let r = tokio::time::timeout(
            cfg.session + Duration::from_secs(5),
            drive::run(cfg, &self.inner.fetcher, job),
        )
        .await
        .unwrap_or_else(|_| ProbeResult::failed("timeout"));
        drop(slot);
        match r.status {
            ProbeStatus::Ok => add(&c.ok, 1),
            ProbeStatus::Partial => add(&c.partial, 1),
            ProbeStatus::Choked => add(&c.choked, 1),
            ProbeStatus::NoPieces => add(&c.no_pieces, 1),
            ProbeStatus::Failed(_) => add(&c.failed, 1),
        }
        add(&c.bytes_in, r.bytes_in);
        add(&c.uploaded, r.uploaded);
        add(&c.files, r.files.len() as u64);
        add(
            &c.verified_files,
            r.files.iter().filter(|f| f.verified).count() as u64,
        );
        add(&c.surprises, u64::from(r.surprises));
        r
    }

    pub fn stats(&self) -> ProbeStats {
        let c = &self.inner.c;
        let g = |a: &AtomicU64| a.load(Ordering::Relaxed);
        ProbeStats {
            started: g(&c.started),
            ok: g(&c.ok),
            partial: g(&c.partial),
            choked: g(&c.choked),
            no_pieces: g(&c.no_pieces),
            failed: g(&c.failed),
            skipped: SKIPS
                .iter()
                .zip(c.skipped.iter())
                .map(|(s, n)| (s.code().to_string(), g(n)))
                .filter(|(_, n)| *n > 0)
                .collect(),
            bytes_in: g(&c.bytes_in),
            uploaded: g(&c.uploaded),
            files: g(&c.files),
            verified_files: g(&c.verified_files),
            surprises: g(&c.surprises),
        }
    }
}

/// On a worker with probes on: no core dumps of this process (`PR_SET_DUMPABLE` 0), so piece
/// bytes held in memory are not written out by a crash (check1 #1). Linux only; elsewhere a
/// no-op. Returns whether the call succeeded.
pub fn harden_process() -> bool {
    #[cfg(target_os = "linux")]
    {
        nix::sys::prctl::set_dumpable(false).is_ok()
    }
    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The probe code never opens a file: pieces live in memory only (D117).
    #[test]
    fn no_file_io_in_probe_code() {
        for (name, src) in [
            ("mod.rs", include_str!("mod.rs")),
            ("drive.rs", include_str!("drive.rs")),
            ("peer.rs", include_str!("peer.rs")),
            ("serve.rs", include_str!("serve.rs")),
            ("verify.rs", include_str!("verify.rs")),
            ("msg.rs", include_str!("msg.rs")),
            ("plan.rs", include_str!("plan.rs")),
        ] {
            // Split so this list does not match itself.
            for bad in [
                concat!("std::", "fs"),
                concat!("tokio::", "fs"),
                concat!("fs::", "File"),
                concat!("Open", "Options"),
                concat!("Tcp", "Listener"),
            ] {
                assert!(!src.contains(bad), "{name} uses {bad}");
            }
        }
    }

    #[test]
    fn rate_and_slots() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let f = Fetcher::new(crate::metainfo::FetchConfig::default());
            let p = Prober::new(
                ProbeConfig {
                    per_min: 60_000,
                    parallel: 1,
                    ..ProbeConfig::default()
                },
                f,
            );
            let a = p.try_slot().expect("first");
            tokio::time::sleep(Duration::from_millis(5)).await;
            assert!(p.try_slot().is_none(), "one slot");
            drop(a);
            assert!(p.try_slot().is_some());
            assert!(p.try_slot().is_none(), "paced: no burst");
            let s = p.stats();
            assert!(s.skipped.iter().any(|(k, n)| k == "busy" && *n == 1));
            assert!(s.skipped.iter().any(|(k, n)| k == "rate" && *n >= 1));
        });
    }

    /// D118: the waiting slot keeps the pace (turns one interval apart, in order), waits for a
    /// free slot, and counts nothing as skipped.
    #[test]
    fn waiting_slot_keeps_the_pace() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let f = Fetcher::new(crate::metainfo::FetchConfig::default());
            // 600 a minute: turns 100 ms apart.
            let p = Prober::new(
                ProbeConfig {
                    per_min: 600,
                    parallel: 2,
                    ..ProbeConfig::default()
                },
                f,
            );
            let t0 = Instant::now();
            let a = p.slot().await;
            let b = p.slot().await;
            assert!(
                t0.elapsed() >= Duration::from_millis(95),
                "second turn one interval later"
            );
            // Both slots taken: the third waits for one to come back, then for its turn.
            let p2 = p.clone();
            let third = tokio::spawn(async move { p2.slot().await });
            tokio::time::sleep(Duration::from_millis(400)).await;
            assert!(!third.is_finished(), "no free slot");
            drop(a);
            let _c = tokio::time::timeout(Duration::from_secs(5), third)
                .await
                .expect("a freed slot is taken")
                .unwrap();
            drop(b);
            assert!(p.stats().skipped.is_empty(), "waiting is not skipping");
        });
    }
}

//! Header probes against fake peers on 127.0.0.1 (D117): blocks only on big pieces, whole
//! verified pieces on small ones, lying peers, the surprise check, chokes with and without BEP 6,
//! allowed-fast, `reqq`, hostile peers and the no-upload whitelist, upload mode and its caps,
//! identity, the single hand-off connection, the byte budget, the fetcher with probes off, and
//! an MP4 with its `moov` at the end on whole pieces and on blocks (D120).
//! Only the `allow_loopback` test seam differs from production; no other traffic.
#![cfg(feature = "probe")]

use std::collections::HashSet;
use std::net::SocketAddrV4;
use std::sync::Arc;
use std::time::Duration;

use magnetar_core::identity::{CLIENT_NAME, PEER_ID_PREFIX};
use magnetar_core::metainfo::{FetchConfig, Fetcher, Outcome};
use magnetar_core::probe::{ProbeConfig, ProbeJob, ProbeResult, ProbeStatus, Prober};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::time::Instant;

use crate::support::fake_seeder::{self as fs, ALLOWED_IDS, Has, Junk, Opts, Torrent, Unchoke};

const KIB: u64 = 1024;
const MIB: u64 = 1024 * 1024;

fn ebml(id: &[u8], data: &[u8]) -> Vec<u8> {
    let mut v = id.to_vec();
    if data.len() < 0x7f {
        v.push(0x80 | data.len() as u8);
    } else {
        v.extend_from_slice(&(0x4000u16 | data.len() as u16).to_be_bytes());
    }
    v.extend_from_slice(data);
    v
}

/// A Matroska header (HEVC + E-AC-3, as sniff's fixtures) then filler: `video.mkv` at 97.
fn mkv(size: usize) -> Vec<u8> {
    let mut head = ebml(&[0x42, 0x86], &[1]);
    head.extend(ebml(&[0x42, 0x82], b"matroska"));
    let mut out = ebml(&[0x1A, 0x45, 0xDF, 0xA3], &head);
    let mut entries = Vec::new();
    for (i, (ty, codec)) in [(1u8, "V_MPEGH/ISO/HEVC"), (2, "A_EAC3")]
        .iter()
        .enumerate()
    {
        let mut te = ebml(&[0xD7], &[i as u8 + 1]);
        te.extend(ebml(&[0x83], &[*ty]));
        te.extend(ebml(&[0x86], codec.as_bytes()));
        entries.extend(ebml(&[0xAE], &te));
    }
    let mut seg = ebml(
        &[0x15, 0x49, 0xA9, 0x66],
        &ebml(&[0x2A, 0xD7, 0xB1], &[0x0F, 0x42, 0x40]),
    );
    seg.extend(ebml(&[0x16, 0x54, 0xAE, 0x6B], &entries));
    seg.extend(ebml(&[0x1F, 0x43, 0xB6, 0x75], &[0u8; 32]));
    out.extend(ebml(&[0x18, 0x53, 0x80, 0x67], &seg));
    fill(out, size)
}

/// A Windows PE header (as sniff's fixtures) then filler: `exe.pe` at 97.
fn pe(size: usize) -> Vec<u8> {
    let mut b = vec![0u8; 0x600];
    let mut put = |at: usize, v: &[u8]| b[at..at + v.len()].copy_from_slice(v);
    put(0, b"MZ");
    put(0x3C, &0x80u32.to_le_bytes());
    put(0x80, b"PE\0\0");
    put(0x84, &0x8664u16.to_le_bytes());
    put(0x86, &1u16.to_le_bytes());
    put(0x94, &240u16.to_le_bytes());
    put(0x96, &0x22u16.to_le_bytes());
    let opt = 0x98;
    put(opt, &0x20bu16.to_le_bytes());
    put(opt + 68, &2u16.to_le_bytes());
    let sec = opt + 240;
    put(sec, b".text\0\0\0");
    put(sec + 16, &0x200u32.to_le_bytes());
    put(sec + 20, &0x400u32.to_le_bytes());
    fill(b, size)
}

fn fill(mut v: Vec<u8>, size: usize) -> Vec<u8> {
    let start = v.len();
    v.extend((start..size).map(|i| (i * 31 + 7) as u8));
    v
}

fn fetcher(probe: bool) -> Fetcher {
    Fetcher::new(FetchConfig {
        allow_loopback: true,
        connect_timeout: Duration::from_secs(1),
        handshake_timeout: Duration::from_secs(2),
        peer_timeout: Duration::from_secs(3),
        hash_budget: Duration::from_secs(10),
        metadata_max: 1 << 20,
        connects_per_sec: 1000,
        keep_raw: true,
        probe,
        ..FetchConfig::default()
    })
}

fn cfg() -> ProbeConfig {
    ProbeConfig {
        unchoke_wait: Duration::from_secs(2),
        stall: Duration::from_secs(2),
        session: Duration::from_secs(20),
        second_peer_after: Duration::from_millis(300),
        upload_linger: Duration::from_millis(800),
        per_min: 60_000,
        ..ProbeConfig::default()
    }
}

/// Metadata from `first` (the hand-off), then a probe with `others` as further peers.
async fn probe(
    t: &Arc<Torrent>,
    first: &fs::Seeder,
    others: &[SocketAddrV4],
    c: ProbeConfig,
) -> ProbeResult {
    let f = fetcher(true);
    let (r, h) = f.fetch_with_handoff(t.hash, &[first.addr]).await;
    let Ok(Outcome::Ok(fe)) = r else {
        panic!("metadata: {r:?}");
    };
    let h = h.expect("a hand-off with probes on");
    assert_eq!(h.answered, vec![first.addr]);
    let p = Prober::new(c, f);
    let slot = p.try_slot().expect("a slot");
    let r = p
        .probe(
            slot,
            ProbeJob {
                info_hash: t.hash,
                raw: fe.raw.expect("keep_raw").into(),
                files: fe.info.files.clone(),
                conn: Some(h.conn),
                peers: others.to_vec(),
            },
        )
        .await;
    let s = p.stats();
    assert_eq!(s.started, 1);
    assert_eq!(s.bytes_in, r.bytes_in);
    // Let the fake peers read what is still in flight before the test reads their logs.
    tokio::time::sleep(Duration::from_millis(150)).await;
    r
}

fn label(r: &ProbeResult) -> Vec<(&str, bool)> {
    r.files
        .iter()
        .map(|f| (f.detection.label.as_str(), f.verified))
        .collect()
}

fn assert_whitelist(l: &fs::Log) {
    let bad: Vec<u8> = l
        .ids
        .iter()
        .copied()
        .filter(|i| !ALLOWED_IDS.contains(i))
        .collect();
    assert!(bad.is_empty(), "forbidden ids sent: {bad:?}");
    assert_eq!(l.forbidden, 0);
    assert_eq!(l.piece_bytes, 0);
    assert!(l.haves.is_empty() && l.unchokes == 0);
    // ut_metadata: requests (0) and rejects (2) only, never data (1).
    assert!(l.metadata_types.iter().all(|t| *t == 0 || *t == 2));
}

/// 1, 11, 12: a single MKV with 4 MiB pieces: only the head blocks, unverified, within the caps,
/// one TCP connection for metadata and probe, honest identity.
#[tokio::test]
async fn blocks_only_on_big_pieces() {
    let t = Torrent::new("x", vec![("Movie.2024.2160p.mkv", mkv(6_000_000))], 4 * MIB);
    let s = fs::start(t.clone(), Opts::default()).await;
    let r = probe(&t, &s, &[], cfg()).await;
    assert_eq!(r.status, ProbeStatus::Ok, "{r:?}");
    assert_eq!(label(&r), vec![("video.mkv", false)]);
    // Diagnostics: block mode, every target tried, bytes and a time for them.
    assert_eq!(r.diag.end, "done");
    assert!(!r.diag.whole && !r.diag.late && r.diag.cut == 0);
    assert_eq!(
        (r.diag.targets, r.diag.no_bytes, r.diag.max_alive),
        (1, 0, 1)
    );
    assert!(r.diag.first_byte_ms.is_some() && r.diag.ms > 0);
    let f = &r.files[0];
    assert!(f.detection.flags.unverified);
    assert_eq!(f.detection.video.as_ref().unwrap().video[0].codec, "hevc");
    assert_eq!(
        r.bytes_in,
        64 * KIB,
        "the 64 KiB video head, in 16 KiB blocks"
    );
    let l = s.log();
    assert_eq!(l.accepts, 1, "metadata and probe on one connection");
    assert_eq!(l.served, 4);
    assert_whitelist(&l);
    assert!(l.peer_ids[0].starts_with(&PEER_ID_PREFIX));
    assert_eq!(l.client, vec![CLIENT_NAME.to_string()]);
    assert_eq!(
        l.reserved[0],
        [0, 0, 0, 0, 0, 0x10, 0, 0x04],
        "BEP 10 + BEP 6"
    );
    // HAVE_NONE first (both fast), then the extended handshake, then INTERESTED.
    assert_eq!(&l.ids[..3], &[15, 20, 2]);
}

/// 2: 256 KiB pieces: whole pieces, SHA-1 checked, verified.
#[tokio::test]
async fn small_pieces_are_fetched_whole_and_verified() {
    let t = Torrent::new("x", vec![("Movie.mkv", mkv(3_000_000))], 256 * KIB);
    let s = fs::start(t.clone(), Opts::default()).await;
    let r = probe(&t, &s, &[], cfg()).await;
    assert_eq!(label(&r), vec![("video.mkv", true)], "{r:?}");
    assert!(!r.files[0].detection.flags.unverified);
    assert_eq!(r.bytes_in, 256 * KIB, "one whole piece");
    assert_whitelist(&s.log());
}

fn flipped(d: &[u8]) -> Arc<Vec<u8>> {
    Arc::new(d.iter().map(|b| b ^ 0x5A).collect())
}

/// 3: a lying peer with small pieces fails the hash and is dropped; an honest second peer gives
/// the verified result; alone, no result.
#[tokio::test]
async fn lying_peer_with_small_pieces() {
    let data = mkv(2_000_000);
    let t = Torrent::new("x", vec![("Movie.mkv", data.clone())], 256 * KIB);
    let liar = Opts {
        lie: Some(flipped(&t.data)),
        ..Opts::default()
    };
    let bad = fs::start(t.clone(), liar.clone()).await;
    let good = fs::start(t.clone(), Opts::default()).await;
    let r = probe(&t, &bad, &[good.addr], cfg()).await;
    assert_eq!(label(&r), vec![("video.mkv", true)], "{r:?}");
    assert_eq!(r.peers_used, 2);
    assert!(good.log().served > 0);
    let alone = fs::start(t.clone(), liar).await;
    let r = probe(&t, &alone, &[], cfg()).await;
    assert!(r.files.is_empty(), "{r:?}");
}

/// 4: a lying peer with big pieces serves a PE header in an `.mkv`: the surprise check fetches
/// the whole piece, the hash fails, nothing is stored. With an honest peer as well, the
/// verified bytes win (`surprise_changed`). An honest PE behind a media name is verified and
/// keeps its label (`surprise_verified`).
#[tokio::test]
async fn surprise_check_on_big_pieces() {
    let t = Torrent::new("x", vec![("Movie.2024.mkv", mkv(6_000_000))], 4 * MIB);
    let evil = Arc::new(pe(t.data.len()));
    let liar = Opts {
        lie: Some(evil.clone()),
        ..Opts::default()
    };
    let bad = fs::start(t.clone(), liar.clone()).await;
    let r = probe(&t, &bad, &[], cfg()).await;
    assert!(r.files.is_empty(), "nothing from a liar: {r:?}");
    assert_eq!(r.surprises, 1);

    let bad = fs::start(t.clone(), liar).await;
    let good = fs::start(t.clone(), Opts::default()).await;
    let r = probe(&t, &bad, &[good.addr], cfg()).await;
    assert_eq!(label(&r), vec![("video.mkv", true)], "{r:?}");
    assert_eq!(r.files[0].notes, vec!["surprise_changed"]);

    let real = Torrent::new("x", vec![("Movie.2024.mkv", pe(6_000_000))], 4 * MIB);
    let s = fs::start(real.clone(), Opts::default()).await;
    let r = probe(&real, &s, &[], cfg()).await;
    assert_eq!(label(&r), vec![("exe.pe", true)], "{r:?}");
    let d = &r.files[0].detection;
    assert!(d.flags.suspicious_executable && d.flags.extension_mismatch);
    assert_eq!(r.files[0].notes, vec!["surprise_verified"]);
    assert!(r.bytes_in >= 4 * MIB && r.bytes_in <= cfg().max_bytes);
}

/// 5: no Fast extension: no HAVE_NONE, no bitfield; a choke drops the outstanding requests,
/// they are asked again after the unchoke and the transfer finishes.
#[tokio::test]
async fn no_fast_extension_and_rechoke() {
    let t = Torrent::new("x", vec![("Movie.mkv", mkv(3_000_000))], 256 * KIB);
    let s = fs::start(
        t.clone(),
        Opts {
            fast: false,
            rechoke_after: Some(3),
            delay: Duration::from_millis(5),
            ..Opts::default()
        },
    )
    .await;
    let r = probe(&t, &s, &[], cfg()).await;
    assert_eq!(label(&r), vec![("video.mkv", true)], "{r:?}");
    let l = s.log();
    assert_whitelist(&l);
    assert!(!l.ids.contains(&15) && !l.ids.contains(&5));
    assert!(
        l.requests > 16,
        "dropped requests asked again: {}",
        l.requests
    );
}

/// 6: a peer that never unchokes: `Choked` within the unchoke wait; an allowed-fast target piece
/// is fetched while choked.
#[tokio::test]
async fn never_unchoked_and_allowed_fast() {
    let t = Torrent::new("x", vec![("Movie.mkv", mkv(3_000_000))], 4 * MIB);
    let s = fs::start(
        t.clone(),
        Opts {
            unchoke: Unchoke::Never,
            ..Opts::default()
        },
    )
    .await;
    let started = Instant::now();
    let r = probe(&t, &s, &[], cfg()).await;
    assert_eq!(r.status, ProbeStatus::Choked, "{r:?}");
    assert!(!r.unchoked);
    assert!(started.elapsed() < Duration::from_secs(6));
    // Diagnostics: the hand-off peer choked us past the wait; no byte, no unchoke.
    assert!(r.diag.handoff_choked, "{:?}", r.diag);
    assert_eq!(r.diag.choke_expired, 1, "{:?}", r.diag);
    assert_eq!((r.diag.unchoke_ms, r.diag.first_byte_ms), (None, None));
    assert_eq!(r.diag.no_bytes, r.diag.targets - r.diag.cut);
    let s = fs::start(
        t.clone(),
        Opts {
            unchoke: Unchoke::Never,
            allowed_fast: vec![0],
            ..Opts::default()
        },
    )
    .await;
    let r = probe(&t, &s, &[], cfg()).await;
    assert_eq!(label(&r), vec![("video.mkv", false)], "{r:?}");
    assert!(!r.unchoked);
}

/// 7: upload off against a hostile seeder (it asks us for blocks and metadata all along), many
/// times: only whitelisted ids go out, never a piece, unchoke, have, bitfield or metadata data.
#[tokio::test]
async fn upload_off_never_shares_with_a_hostile_peer() {
    let t = Torrent::new("x", vec![("Movie.mkv", mkv(2_000_000))], 256 * KIB);
    let s = fs::start(
        t.clone(),
        Opts {
            hostile: true,
            ..Opts::default()
        },
    )
    .await;
    for _ in 0..12 {
        let r = probe(&t, &s, &[], cfg()).await;
        assert_eq!(label(&r), vec![("video.mkv", true)], "{r:?}");
    }
    let l = s.log();
    assert_whitelist(&l);
    assert!(
        l.rejects_got > 0,
        "a fast peer's requests get reject_request"
    );
    assert!(
        l.metadata_types.contains(&2),
        "metadata requests get a reject"
    );

    // Negative control: a client that does send them is caught (the detector is not vacuous).
    let mut c = tokio::net::TcpStream::connect(s.addr).await.unwrap();
    let mut hs = magnetar_core::metainfo::wire::handshake_with(&t.hash, &[1; 20], true).to_vec();
    use magnetar_core::metainfo::wire::frame;
    hs.extend(frame(1, &[]));
    hs.extend(frame(4, &0u32.to_be_bytes()));
    hs.extend(frame(5, &[0x80]));
    let mut p = vec![0, 0, 0, 0, 0, 0, 0, 0];
    p.extend_from_slice(&[0; 16]);
    hs.extend(frame(7, &p));
    c.write_all(&hs).await.unwrap();
    let mut buf = [0u8; 68];
    c.read_exact(&mut buf).await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(s.log().forbidden >= l.forbidden + 4);
}

/// 8: upload on: HAVE only for verified held pieces, pieces only inside them, other requests
/// rejected, never a bitfield or have_all, the byte cap and the rate cap kept.
#[tokio::test]
async fn upload_mode_shares_only_verified_pieces_within_caps() {
    let t = Torrent::new("x", vec![("Movie.mkv", mkv(1_000_000))], 256 * KIB);
    // The hand-off peer lacks piece 0 (the file's head) and wants it; a seed has it.
    let leech = Opts {
        has: Has::Pieces(vec![1, 2, 3]),
        leech: Some(3),
        ..Opts::default()
    };
    let a = fs::start(t.clone(), leech.clone()).await;
    let b = fs::start(t.clone(), Opts::default()).await;
    let c = ProbeConfig {
        upload: true,
        upload_max_bytes: 512 * KIB,
        upload_rate: 16 * MIB,
        ..cfg()
    };
    let r = probe(&t, &a, &[b.addr], c).await;
    assert_eq!(label(&r), vec![("video.mkv", true)], "{r:?}");
    let l = a.log();
    assert_eq!(l.haves, vec![0], "only the verified piece");
    assert!(l.unchokes >= 1);
    assert!(
        !l.ids.contains(&5) && !l.ids.contains(&14),
        "no bitfield, no have_all"
    );
    assert!(l.piece_bytes_ok, "served bytes are the torrent's");
    assert!(
        l.pieces_got.iter().all(|(i, _, _)| *i == 0),
        "{:?}",
        l.pieces_got
    );
    assert_eq!(l.piece_bytes, 512 * KIB, "the byte cap: {}", l.piece_bytes);
    assert_eq!(r.uploaded, 512 * KIB);
    assert!(
        l.rejects_got >= 1,
        "the request for a piece it does not hold"
    );
    assert!(l.chokes >= 1, "choked at the cap");
    // The seed is not interested: never unchoked, gets the HAVE only.
    let lb = b.log();
    assert_eq!(lb.unchokes, 0);
    assert_eq!(lb.piece_bytes, 0);

    // Rate cap: 64 KiB/s over the linger.
    let a = fs::start(t.clone(), leech.clone()).await;
    let b = fs::start(t.clone(), Opts::default()).await;
    let c = ProbeConfig {
        upload: true,
        upload_rate: 64 * KIB,
        upload_linger: Duration::from_millis(1500),
        ..cfg()
    };
    let started = Instant::now();
    let r = probe(&t, &a, &[b.addr], c).await;
    let secs = started.elapsed().as_secs_f64();
    let got = a.log().piece_bytes;
    assert!(got > 0);
    assert!(
        (got as f64) <= 64.0 * 1024.0 * (secs + 1.0),
        "{got} B in {secs:.2} s"
    );
    assert_eq!(r.uploaded, got);

    // Same peers, upload off: nothing shared.
    let a = fs::start(t.clone(), leech).await;
    let b = fs::start(t.clone(), Opts::default()).await;
    let r = probe(&t, &a, &[b.addr], cfg()).await;
    assert_eq!(label(&r), vec![("video.mkv", true)]);
    assert_whitelist(&a.log());
    assert_whitelist(&b.log());
}

/// 9: `reqq=4`: never more than 4 requests outstanding (control: more without it).
#[tokio::test]
async fn reqq_caps_the_pipeline() {
    let t = Torrent::new("x", vec![("Movie.mkv", mkv(1_000_000))], 256 * KIB);
    let s = fs::start(
        t.clone(),
        Opts {
            reqq: Some(4),
            delay: Duration::from_millis(2),
            ..Opts::default()
        },
    )
    .await;
    let r = probe(&t, &s, &[], cfg()).await;
    assert_eq!(label(&r), vec![("video.mkv", true)]);
    assert!(s.log().max_outstanding <= 4, "{}", s.log().max_outstanding);
    let s = fs::start(
        t.clone(),
        Opts {
            delay: Duration::from_millis(2),
            ..Opts::default()
        },
    )
    .await;
    probe(&t, &s, &[], cfg()).await;
    assert!(s.log().max_outstanding > 4);
}

/// Dead slots do not count toward `peers` (D142): two peers that are dropped leave room for the
/// good third candidate, which gives the result. (Before, `cap_blocked` and `no_peers`.)
#[tokio::test]
async fn dead_slots_do_not_block_the_next_peer() {
    let t = Torrent::new("x", vec![("Movie.mkv", mkv(1_000_000))], 256 * KIB);
    let bad = || Opts {
        junk: Junk::Unrequested,
        ..Opts::default()
    };
    let first = fs::start(t.clone(), bad()).await;
    let second = fs::start(t.clone(), bad()).await;
    let good = fs::start(t.clone(), Opts::default()).await;
    let r = probe(&t, &first, &[second.addr, good.addr], cfg()).await;
    let d = &r.diag;
    assert_eq!(label(&r), vec![("video.mkv", true)], "{r:?}");
    assert!(good.log().requests > 0, "the good peer was asked");
    assert!(!d.cap_blocked, "{d:?}");
    assert_eq!(d.candidates_left, 0, "{d:?}");
    assert_eq!(d.end, "done", "{d:?}");
    assert!(d.drops.contains(&("unrequested", 2)), "{d:?}");
    assert!(d.connects.contains(&("ok", 2)), "{d:?}");
}

/// Stale peer lists (D142): more refusing candidates than the old 4-attempt cap, then a good
/// one. Every candidate is tried, `connect_parallel` at a time; the hand-off peer has no piece.
#[tokio::test]
async fn many_refusing_candidates_then_a_good_one() {
    let t = Torrent::new("x", vec![("Movie.mkv", mkv(1_000_000))], 256 * KIB);
    let first = fs::start(
        t.clone(),
        Opts {
            has: Has::Pieces(vec![]),
            ..Opts::default()
        },
    )
    .await;
    let mut others = Vec::new();
    for _ in 0..7 {
        // A bound then closed port: connecting is refused at once.
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let std::net::SocketAddr::V4(a) = l.local_addr().unwrap() else {
            unreachable!()
        };
        others.push(a);
    }
    let good = fs::start(t.clone(), Opts::default()).await;
    others.push(good.addr);
    let r = probe(&t, &first, &others, cfg()).await;
    let d = &r.diag;
    assert_eq!(label(&r), vec![("video.mkv", true)], "{r:?}");
    assert!(good.log().requests > 0);
    let refused: u32 = d
        .connects
        .iter()
        .filter(|(k, _)| *k != "ok")
        .map(|(_, n)| *n)
        .sum();
    assert_eq!(refused, 7, "{d:?}");
    assert!(d.connects.contains(&("ok", 1)), "{d:?}");
    assert!(!d.cap_blocked && d.candidates_left == 0, "{d:?}");
}

/// `connect_parallel` and `connect_attempts` bound the attempts: with 1 attempt allowed the
/// good candidate behind 3 refusing ones is never reached, and `cap_blocked` says why.
#[tokio::test]
async fn connect_attempts_cap_is_reported() {
    let t = Torrent::new("x", vec![("Movie.mkv", mkv(1_000_000))], 256 * KIB);
    let first = fs::start(
        t.clone(),
        Opts {
            has: Has::Pieces(vec![]),
            ..Opts::default()
        },
    )
    .await;
    let mut others = Vec::new();
    for _ in 0..3 {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let std::net::SocketAddr::V4(a) = l.local_addr().unwrap() else {
            unreachable!()
        };
        others.push(a);
    }
    let good = fs::start(t.clone(), Opts::default()).await;
    others.push(good.addr);
    let c = ProbeConfig {
        connect_attempts: 1,
        connect_parallel: 1,
        ..cfg()
    };
    let r = probe(&t, &first, &others, c).await;
    assert!(r.files.is_empty(), "{r:?}");
    assert!(r.diag.cap_blocked, "{:?}", r.diag);
    assert_eq!(r.diag.candidates_left, 3, "{:?}", r.diag);
    assert_eq!(good.log().requests, 0);
}

/// Whole-piece mode, a peer that sends part of the piece and stalls (D142): the blocks received
/// are used as unverified data instead of thrown away.
#[tokio::test]
async fn partial_whole_piece_gives_an_unverified_result() {
    let t = Torrent::new("x", vec![("Movie.mkv", mkv(3_000_000))], 512 * KIB);
    let s = fs::start(
        t.clone(),
        Opts {
            stall_after: Some(8),
            ..Opts::default()
        },
    )
    .await;
    let r = probe(&t, &s, &[], cfg()).await;
    assert!(r.diag.whole, "{:?}", r.diag);
    assert_eq!(label(&r), vec![("video.mkv", false)], "{r:?}");
    assert!(r.files[0].detection.flags.unverified);
    assert_eq!(r.bytes_in, 128 * KIB, "8 blocks of 16 KiB");
    assert!(r.diag.drops.contains(&("stall", 1)), "{:?}", r.diag);
}

/// A liar that sends complete pieces is struck on the hash and nothing is used from it.
#[tokio::test]
async fn a_lying_peer_in_whole_mode_gives_no_result() {
    let t = Torrent::new("x", vec![("Movie.mkv", mkv(3_000_000))], 256 * KIB);
    let liar = fs::start(
        t.clone(),
        Opts {
            lie: Some(flipped(&t.data)),
            ..Opts::default()
        },
    )
    .await;
    let r = probe(&t, &liar, &[], cfg()).await;
    assert!(r.files.is_empty(), "{r:?}");
}

/// A liar that sends part of a whole piece and stalls: its blocks stay out of the result, the
/// honest second peer fetches the piece again and the result is verified.
#[tokio::test]
async fn a_stalled_liars_partial_piece_is_not_used() {
    let t = Torrent::new("x", vec![("Movie.mkv", mkv(3_000_000))], 256 * KIB);
    let liar = fs::start(
        t.clone(),
        Opts {
            lie: Some(flipped(&t.data)),
            stall_after: Some(4),
            ..Opts::default()
        },
    )
    .await;
    let good = fs::start(t.clone(), Opts::default()).await;
    let r = probe(&t, &liar, &[good.addr], cfg()).await;
    assert_eq!(label(&r), vec![("video.mkv", true)], "{r:?}");
    assert!(good.log().requests > 0, "{:?}", r.diag);
}

/// Several attempts at once (D142): three candidates accept and never handshake, the fourth is
/// good. With `connect_parallel` 4 the good one is reached at once and the silent attempts are
/// cancelled; with 1 each silent one holds the line for the handshake timeout first.
#[tokio::test]
async fn connection_attempts_run_in_parallel() {
    let t = Torrent::new("x", vec![("Movie.mkv", mkv(1_000_000))], 256 * KIB);
    let mut times = Vec::new();
    for parallel in [4usize, 1] {
        let first = fs::start(
            t.clone(),
            Opts {
                has: Has::Pieces(vec![]),
                ..Opts::default()
            },
        )
        .await;
        let mut others = Vec::new();
        for _ in 0..3 {
            let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let std::net::SocketAddr::V4(a) = l.local_addr().unwrap() else {
                unreachable!()
            };
            others.push(a);
            tokio::spawn(async move {
                let mut held = Vec::new();
                while let Ok((s, _)) = l.accept().await {
                    held.push(s);
                }
            });
        }
        let good = fs::start(t.clone(), Opts::default()).await;
        others.push(good.addr);
        let c = ProbeConfig {
            connect_parallel: parallel,
            ..cfg()
        };
        let start = std::time::Instant::now();
        let r = probe(&t, &first, &others, c).await;
        times.push(start.elapsed());
        assert_eq!(label(&r), vec![("video.mkv", true)], "{r:?}");
        if parallel == 4 {
            assert!(r.diag.connects.contains(&("cancelled", 3)), "{:?}", r.diag);
        }
    }
    assert!(
        times[0] + Duration::from_secs(3) < times[1],
        "parallel {:?} vs serial {:?}",
        times[0],
        times[1]
    );
}

/// 10: hostile input never panics: oversize pieces, spare bitfield bits, unrequested blocks,
/// floods, a `have` past the end. Blocks out of order and rejects are taken in stride.
#[tokio::test]
async fn hostile_input_is_survived() {
    let t = Torrent::new("x", vec![("Movie.mkv", mkv(1_000_000))], 256 * KIB);
    for junk in [
        Junk::OversizePiece,
        Junk::SpareBits,
        Junk::Unrequested,
        Junk::BadHave,
    ] {
        let s = fs::start(
            t.clone(),
            Opts {
                junk,
                ..Opts::default()
            },
        )
        .await;
        let r = probe(&t, &s, &[], cfg()).await;
        assert!(r.files.is_empty(), "{junk:?}: the peer is dropped: {r:?}");
    }
    for o in [
        Opts {
            junk: Junk::Flood,
            ..Opts::default()
        },
        Opts {
            out_of_order: true,
            ..Opts::default()
        },
        Opts {
            reject_every: Some(3),
            ..Opts::default()
        },
    ] {
        let s = fs::start(t.clone(), o.clone()).await;
        let good = fs::start(t.clone(), Opts::default()).await;
        let r = probe(&t, &s, &[good.addr], cfg()).await;
        assert_eq!(label(&r), vec![("video.mkv", true)], "{o:?}: {r:?}");
    }
}

/// 13: probes off: the fetcher's wire is what it was before D117 (BEP 10 bit only, no
/// HAVE_NONE, no INTERESTED) and there is no hand-off.
#[tokio::test]
async fn fetcher_without_probes_is_unchanged() {
    let t = Torrent::new("x", vec![("Movie.mkv", mkv(100_000))], 256 * KIB);
    let s = fs::start(t.clone(), Opts::default()).await;
    let f = fetcher(false);
    let (r, h) = f.fetch_with_handoff(t.hash, &[s.addr]).await;
    assert!(matches!(r, Ok(Outcome::Ok(_))));
    assert!(h.is_none());
    tokio::time::sleep(Duration::from_millis(100)).await;
    let l = s.log();
    assert_eq!(l.reserved[0], [0, 0, 0, 0, 0, 0x10, 0, 0]);
    let ids: HashSet<u8> = l.ids.iter().copied().collect();
    assert_eq!(
        ids,
        HashSet::from([20]),
        "extended messages only: {:?}",
        l.ids
    );
    // `connect_for_probe` refuses on a fetcher without probes.
    assert!(f.connect_for_probe(t.hash, s.addr).await.is_err());
}

/// 14: the byte budget stops the probe: cut short, flagged incomplete.
#[tokio::test]
async fn byte_budget_stops_the_probe() {
    let t = Torrent::new("x", vec![("Movie.mkv", mkv(6_000_000))], 4 * MIB);
    let s = fs::start(t.clone(), Opts::default()).await;
    let r = probe(
        &t,
        &s,
        &[],
        ProbeConfig {
            max_bytes: 32 * KIB,
            ..cfg()
        },
    )
    .await;
    assert!(r.incomplete, "{r:?}");
    assert!(r.bytes_in <= 32 * KIB);
    assert!(s.log().served <= 2);
}

/// Two-peer agreement on the main payload's first read, when a second peer is connected.
#[tokio::test]
async fn second_peer_agreement() {
    let t = Torrent::new("x", vec![("Movie.mkv", mkv(6_000_000))], 4 * MIB);
    // The hand-off peer has nothing yet (a second peer is connected), then everything.
    let late = Opts {
        has: Has::Pieces(vec![]),
        have_all_after: Some(Duration::from_millis(100)),
        ..Opts::default()
    };
    let slow = Opts {
        delay: Duration::from_millis(250),
        delay_first: 4,
        ..Opts::default()
    };
    let a = fs::start(t.clone(), late.clone()).await;
    let b = fs::start(t.clone(), slow.clone()).await;
    let r = probe(&t, &a, &[b.addr], cfg()).await;
    assert_eq!(label(&r), vec![("video.mkv", false)], "{r:?}");
    assert_eq!(r.files[0].agreed, 2, "{r:?}");
    assert!(a.log().served >= 4 && b.log().served >= 4);
    // The late peer lies: the peers disagree, the surprise check verifies from the honest one.
    let a = fs::start(
        t.clone(),
        Opts {
            lie: Some(Arc::new(pe(t.data.len()))),
            ..late
        },
    )
    .await;
    let b = fs::start(t.clone(), slow).await;
    let r = probe(&t, &a, &[b.addr], cfg()).await;
    assert_eq!(label(&r), vec![("video.mkv", true)], "{r:?}");
    assert_eq!(r.files[0].notes, vec!["disagree", "surprise_verified"]);
}

// ------------------------------------------------------------------ MP4 `moov` at the end (D120)

fn bx(ty: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut v = ((body.len() + 8) as u32).to_be_bytes().to_vec();
    v.extend_from_slice(ty);
    v.extend_from_slice(body);
    v
}

fn full(ty: &[u8; 4], body: &[u8]) -> Vec<u8> {
    bx(ty, &[&[0u8; 4][..], body].concat())
}

/// Packed ISO 639-2 code of an `mdhd`.
fn packed(l: &str) -> u16 {
    l.bytes().fold(0u16, |a, c| (a << 5) | u16::from(c - 0x60))
}

/// One `trak`: `tkhd`, `mdhd` (language), `hdlr`, `stsd` with `entry`, then sample tables of
/// `table` bytes each (`stsz`, `stco`), as a muxer writes them for a long film.
fn trak(id: u32, handler: &[u8; 4], lang: &str, entry: Vec<u8>, table: usize) -> Vec<u8> {
    let mut tkhd = vec![0u8; 8];
    tkhd.extend_from_slice(&id.to_be_bytes());
    tkhd.extend_from_slice(&[0u8; 68]);
    let mut mdhd = vec![0u8; 8];
    mdhd.extend_from_slice(&48_000u32.to_be_bytes());
    mdhd.extend_from_slice(&(48_000u32 * 7200).to_be_bytes());
    mdhd.extend_from_slice(&packed(lang).to_be_bytes());
    mdhd.extend_from_slice(&[0, 0]);
    let hdlr = full(b"hdlr", &[&[0u8; 4][..], handler, &[0u8; 13][..]].concat());
    let stsd = full(b"stsd", &[&1u32.to_be_bytes()[..], &entry].concat());
    let n = (table / 4).saturating_sub(3) as u32;
    let mut stsz = vec![0u8; 4];
    stsz.extend_from_slice(&n.to_be_bytes());
    stsz.resize(table, 0);
    let mut stco = n.to_be_bytes().to_vec();
    stco.resize(table, 0);
    let stbl = bx(
        b"stbl",
        &[stsd, full(b"stsz", &stsz), full(b"stco", &stco)].concat(),
    );
    let minf = bx(b"minf", &stbl);
    let mdia = bx(b"mdia", &[full(b"mdhd", &mdhd), hdlr, minf].concat());
    bx(b"trak", &[full(b"tkhd", &tkhd), mdia].concat())
}

/// ftyp, an `mdat` of `mdat` bytes, then `moov`: an H.264 video track whose tables take
/// `video_table` bytes each and one AAC track per language with `table` bytes each (2 h of
/// 24 fps video: 690 KB; of 48 kHz AAC: 1.35 MB).
fn mp4_moov_last(langs: &[&str], video_table: usize, table: usize, mdat: usize) -> Vec<u8> {
    let ftyp = bx(b"ftyp", b"isom\0\0\x02\0isomavc1mp41");
    let mdat = bx(b"mdat", &fill(Vec::new(), mdat));
    let mut avc = vec![0u8; 6];
    avc.extend_from_slice(&1u16.to_be_bytes());
    avc.extend_from_slice(&[0u8; 16]);
    avc.extend_from_slice(&1920u16.to_be_bytes());
    avc.extend_from_slice(&800u16.to_be_bytes());
    avc.resize(78, 0);
    let mut traks = vec![trak(1, b"vide", "und", bx(b"avc1", &avc), video_table)];
    for (i, l) in langs.iter().enumerate() {
        let mut a = vec![0u8; 6];
        a.extend_from_slice(&1u16.to_be_bytes());
        a.extend_from_slice(&[0u8; 8]);
        a.extend_from_slice(&2u16.to_be_bytes());
        a.extend_from_slice(&16u16.to_be_bytes());
        a.extend_from_slice(&[0u8; 4]);
        a.extend_from_slice(&(48_000u32 << 16).to_be_bytes());
        traks.push(trak(i as u32 + 2, b"soun", l, bx(b"mp4a", &a), table));
    }
    let mut mvhd = vec![0u8; 8];
    mvhd.extend_from_slice(&1000u32.to_be_bytes());
    mvhd.extend_from_slice(&7_200_000u32.to_be_bytes());
    mvhd.extend_from_slice(&[0u8; 80]);
    let moov = bx(b"moov", &[full(b"mvhd", &mvhd), traks.concat()].concat());
    [ftyp, mdat, moov].concat()
}

fn audio_langs(r: &ProbeResult) -> Vec<Option<String>> {
    r.files
        .first()
        .and_then(|f| f.detection.video.as_ref())
        .map(|v| v.audio.iter().map(|a| a.language.clone()).collect())
        .unwrap_or_default()
}

fn langs(l: &[&str]) -> Vec<Option<String>> {
    l.iter().map(|s| Some(s.to_string())).collect()
}

/// D120: an MP4 whose `moov` follows a 3 MiB `mdat` (a 1.4 MiB video track, three audio
/// tracks of 1.2 MiB), 1 MiB pieces fetched whole. Every track hop costs a piece: head, `moov`,
/// then one per audio track, 5 MiB. Before, the 4 MiB per-file cap (also spent on the hops over
/// the sample tables) stopped before the last audio language; the payload now has the
/// torrent's budget and the track skeleton comes before the tables, so every language is read,
/// verified.
#[tokio::test]
async fn mp4_moov_at_the_end_with_whole_pieces_reads_every_language() {
    let data = mp4_moov_last(
        &["eng", "fre", "spa"],
        700 * 1024,
        600 * 1024,
        3 * MIB as usize,
    );
    let t = Torrent::new("x", vec![("Film.2024.1080p.mp4", data)], MIB);
    let s = fs::start(t.clone(), Opts::default()).await;
    let r = probe(&t, &s, &[], cfg()).await;
    assert_eq!(label(&r), vec![("video.mp4", true)], "{r:?}");
    assert_eq!(audio_langs(&r), langs(&["eng", "fre", "spa"]), "{r:?}");
    assert_eq!(r.status, ProbeStatus::Ok, "{r:?}");
    assert_eq!(
        r.bytes_in,
        5 * MIB,
        "head, moov and one piece per audio track"
    );
    assert_whitelist(&s.log());
}

/// The same file with 4 MiB pieces (blocks only): the `moov` is found from the `mdat` size and
/// walked by its headers, a few 16 KiB blocks for every language.
#[tokio::test]
async fn mp4_moov_at_the_end_with_blocks_costs_a_few_blocks() {
    let data = mp4_moov_last(
        &["eng", "fre", "spa"],
        700 * 1024,
        600 * 1024,
        3 * MIB as usize,
    );
    let t = Torrent::new("x", vec![("Film.2024.1080p.mp4", data)], 4 * MIB);
    let s = fs::start(t.clone(), Opts::default()).await;
    let r = probe(&t, &s, &[], cfg()).await;
    assert_eq!(label(&r), vec![("video.mp4", false)], "{r:?}");
    assert_eq!(audio_langs(&r), langs(&["eng", "fre", "spa"]), "{r:?}");
    assert_eq!(r.status, ProbeStatus::Ok, "{r:?}");
    // The 64 KiB head, the `moov` header block (video track), the video `stco` head and one
    // block per audio track; their tables are never hopped over (192 KiB before).
    assert!(r.bytes_in <= 144 * KIB, "bytes {}", r.bytes_in);
}

/// D120 review: the payload's raised cap keeps the other targets' first reads. A film with six
/// audio tracks would take the whole 8 MiB budget at 1 MiB pieces (head, `moov`, one piece per
/// track); a 2 MiB featurette in its own pieces would then get nothing and count as missing.
/// The payload now stops one piece short (five of six languages, incomplete) and the featurette
/// is still read and verified.
#[tokio::test]
async fn mp4_payload_leaves_the_other_targets_their_first_read() {
    let data = mp4_moov_last(
        &["eng", "fre", "spa", "ger", "ita", "jpn"],
        700 * 1024,
        600 * 1024,
        3 * MIB as usize,
    );
    let t = Torrent::new(
        "x",
        vec![
            ("Extras/Making.Of.mkv", mkv(2 * MIB as usize)),
            ("Film.2024.1080p.mp4", data),
        ],
        MIB,
    );
    let s = fs::start(t.clone(), Opts::default()).await;
    let r = probe(&t, &s, &[], cfg()).await;
    let mut got = label(&r);
    got.sort();
    assert_eq!(
        got,
        vec![("video.mkv", true), ("video.mp4", true)],
        "both files are read: {r:?}"
    );
    assert!(r.bytes_in <= 8 * MIB, "bytes {}", r.bytes_in);
    let mp4 = r.files.iter().find(|f| f.index == 1).expect("payload");
    let n = mp4.detection.video.as_ref().map_or(0, |v| v.audio.len());
    assert!(n >= 5, "the payload still reads most tracks: {n}");
    assert_whitelist(&s.log());
}

/// D120 review: the surprise re-read of a second target is allowed every piece its unverified
/// verdict read, not only the 4 MiB per-file cap. An MP4 named `.mkv` (extension mismatch) beside
/// a film, 2 MiB pieces (blocks): its verdict read the head, the `moov` and the audio track, three
/// pieces. With whole pieces charged at cost, a 4 MiB cap skipped the third piece and the
/// verified re-read saw less than the verdict it confirms.
#[tokio::test]
async fn surprise_reread_of_a_second_target_gets_its_pieces() {
    let clip = mp4_moov_last(&["eng"], 700 * 1024, 1200 * 1024, 3 * MIB as usize);
    let t = Torrent::new(
        "x",
        vec![
            ("Extras/Clip.mkv", clip),
            ("Movie.2024.mkv", mkv(12 * MIB as usize)),
        ],
        2 * MIB,
    );
    let s = fs::start(t.clone(), Opts::default()).await;
    let r = probe(&t, &s, &[], cfg()).await;
    let clip = r.files.iter().find(|f| f.index == 0).expect("clip: {r:?}");
    assert!(clip.detection.flags.extension_mismatch, "{r:?}");
    assert_eq!(clip.notes, vec!["surprise_verified"], "{r:?}");
    assert!(clip.verified, "{r:?}");
    assert_eq!(audio_langs_of(clip), langs(&["eng"]), "{r:?}");
    assert!(r.bytes_in <= cfg().max_bytes, "bytes {}", r.bytes_in);
    assert_whitelist(&s.log());
}

fn audio_langs_of(f: &magnetar_core::probe::FileResult) -> Vec<Option<String>> {
    f.detection
        .video
        .as_ref()
        .map(|v| v.audio.iter().map(|a| a.language.clone()).collect())
        .unwrap_or_default()
}

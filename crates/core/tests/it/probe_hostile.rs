//! Scripted hostile peers against the probe (D117 review findings), loopback only: a
//! non-fast peer that answers both the requests a choke dropped and their re-sends, and a
//! peer caught lying after an earlier target trusted its blocks.
#![cfg(feature = "probe")]

use std::net::{Ipv4Addr, SocketAddrV4};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use magnetar_core::metainfo::{FetchConfig, Fetcher, Outcome, wire};
use magnetar_core::probe::{ProbeConfig, ProbeJob, ProbeResult, ProbeStatus, Prober};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::support::fake_seeder::Torrent;

const KIB: usize = 1024;

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

fn mkv(size: usize, vcodec: &str) -> Vec<u8> {
    let mut head = ebml(&[0x42, 0x86], &[1]);
    head.extend(ebml(&[0x42, 0x82], b"matroska"));
    let mut out = ebml(&[0x1A, 0x45, 0xDF, 0xA3], &head);
    let mut entries = Vec::new();
    for (i, (ty, codec)) in [(1u8, vcodec), (2, "A_EAC3")].iter().enumerate() {
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

fn fetcher() -> Fetcher {
    Fetcher::new(FetchConfig {
        allow_loopback: true,
        connect_timeout: Duration::from_secs(1),
        handshake_timeout: Duration::from_secs(2),
        peer_timeout: Duration::from_secs(3),
        hash_budget: Duration::from_secs(10),
        metadata_max: 1 << 20,
        connects_per_sec: 1000,
        keep_raw: true,
        probe: true,
        ..FetchConfig::default()
    })
}

/// What the probe sent us.
#[derive(Default, Debug)]
struct Seen {
    ids: Vec<u8>,
    requests: usize,
    piece_bytes_sent: usize,
}

#[derive(Clone, Copy, PartialEq)]
enum Script {
    /// Serve each request once, from `served` bytes.
    Plain,
    /// Non-fast: after the first batch of `n` requests, choke + unchoke, read the `n`
    /// re-requests, then answer all 2n requests (the late ones and the new ones).
    ChokeDouble(usize),
}

async fn read_frame(s: &mut TcpStream) -> std::io::Result<Option<(u8, Vec<u8>)>> {
    let len = s.read_u32().await? as usize;
    if len == 0 {
        return Ok(None);
    }
    let mut b = vec![0u8; len];
    s.read_exact(&mut b).await?;
    let id = b.remove(0);
    Ok(Some((id, b)))
}

fn frame(id: u8, body: &[u8]) -> Vec<u8> {
    let mut v = ((body.len() + 1) as u32).to_be_bytes().to_vec();
    v.push(id);
    v.extend_from_slice(body);
    v
}

fn ext_hs(meta: usize) -> Vec<u8> {
    let d = format!("d1:md11:ut_metadatai3ee13:metadata_sizei{meta}ee");
    let mut b = vec![0u8];
    b.extend_from_slice(d.as_bytes());
    frame(20, &b)
}

fn piece_msg(index: u32, begin: u32, data: &[u8]) -> Vec<u8> {
    let mut b = index.to_be_bytes().to_vec();
    b.extend_from_slice(&begin.to_be_bytes());
    b.extend_from_slice(data);
    frame(7, &b)
}

async fn peer(
    l: TcpListener,
    t: Arc<Torrent>,
    served: Arc<Vec<u8>>,
    fast: bool,
    script: Script,
    seen: Arc<Mutex<Seen>>,
) {
    let (mut s, _) = l.accept().await.unwrap();
    let mut hs = [0u8; 68];
    s.read_exact(&mut hs).await.unwrap();
    let mut out = [0u8; 68];
    out[0] = 19;
    out[1..20].copy_from_slice(b"BitTorrent protocol");
    out[25] = 0x10;
    if fast {
        out[27] |= 0x04;
    }
    out[28..48].copy_from_slice(&t.hash);
    out[48..68].copy_from_slice(b"-XX0001-aaaaaaaaaaaa");
    s.write_all(&out).await.unwrap();
    s.write_all(&ext_hs(t.info.len())).await.unwrap();
    if fast {
        s.write_all(&frame(14, &[])).await.unwrap(); // have_all
    } else {
        let n = t.pieces() as usize;
        let mut bf = vec![0u8; n.div_ceil(8)];
        for i in 0..n {
            bf[i / 8] |= 0x80 >> (i % 8);
        }
        s.write_all(&frame(5, &bf)).await.unwrap();
    }
    let mut pending: Vec<(u32, u32, u32)> = Vec::new();
    let mut phase = 0;
    loop {
        let f = match read_frame(&mut s).await {
            Ok(f) => f,
            Err(_) => return,
        };
        let Some((id, body)) = f else { continue };
        seen.lock().unwrap().ids.push(id);
        match id {
            20 => {
                if body[0] == 3 {
                    // metadata request to our ut_metadata (id 3)
                    let txt = String::from_utf8_lossy(&body[1..]).to_string();
                    let piece: usize = txt
                        .split("5:piecei")
                        .nth(1)
                        .and_then(|r| r.split('e').next())
                        .and_then(|n| n.parse().ok())
                        .unwrap_or(0);
                    let start = piece * 16 * KIB;
                    let end = (start + 16 * KIB).min(t.info.len());
                    let hdr = format!(
                        "d8:msg_typei1e5:piecei{piece}e10:total_sizei{}ee",
                        t.info.len()
                    );
                    // the prober's ut_metadata id is wire::OUR_UT_METADATA
                    let mut b = vec![wire::OUR_UT_METADATA];
                    b.extend_from_slice(hdr.as_bytes());
                    b.extend_from_slice(&t.info[start..end]);
                    s.write_all(&frame(20, &b)).await.unwrap();
                }
            }
            2 => {
                s.write_all(&frame(1, &[])).await.unwrap(); // unchoke
            }
            6 => {
                let index = u32::from_be_bytes(body[0..4].try_into().unwrap());
                let begin = u32::from_be_bytes(body[4..8].try_into().unwrap());
                let len = u32::from_be_bytes(body[8..12].try_into().unwrap());
                seen.lock().unwrap().requests += 1;
                let send = |idx: u32, beg: u32, ln: u32| {
                    let at = idx as usize * t.piece_len as usize + beg as usize;
                    piece_msg(idx, beg, &served[at..at + ln as usize])
                };
                match script {
                    Script::Plain => {
                        let m = send(index, begin, len);
                        seen.lock().unwrap().piece_bytes_sent += len as usize;
                        s.write_all(&m).await.unwrap();
                    }
                    Script::ChokeDouble(n) => {
                        pending.push((index, begin, len));
                        if phase == 0 && pending.len() == n {
                            phase = 1;
                            s.write_all(&frame(0, &[])).await.unwrap(); // choke
                            tokio::time::sleep(Duration::from_millis(50)).await;
                            s.write_all(&frame(1, &[])).await.unwrap(); // unchoke
                        } else if phase == 1 && pending.len() == 2 * n {
                            phase = 2;
                            for (i, b, l) in pending.drain(..) {
                                let m = send(i, b, l);
                                seen.lock().unwrap().piece_bytes_sent += l as usize;
                                s.write_all(&m).await.unwrap();
                            }
                        } else if phase == 2 {
                            let m = send(index, begin, len);
                            seen.lock().unwrap().piece_bytes_sent += len as usize;
                            s.write_all(&m).await.unwrap();
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

async fn run(
    t: Arc<Torrent>,
    served: Vec<u8>,
    fast: bool,
    script: Script,
    c: ProbeConfig,
) -> (ProbeResult, Arc<Mutex<Seen>>) {
    let l = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let addr = match l.local_addr().unwrap() {
        std::net::SocketAddr::V4(a) => a,
        _ => unreachable!(),
    };
    let seen = Arc::new(Mutex::new(Seen::default()));
    tokio::spawn(peer(
        l,
        t.clone(),
        Arc::new(served),
        fast,
        script,
        seen.clone(),
    ));
    let f = fetcher();
    let (r, h) = f.fetch_with_handoff(t.hash, &[addr]).await;
    let Ok(Outcome::Ok(fe)) = r else {
        panic!("metadata: {r:?}")
    };
    let h = h.expect("handoff");
    let p = Prober::new(c, f);
    let slot = p.try_slot().unwrap();
    let r = p
        .probe(
            slot,
            ProbeJob {
                info_hash: t.hash,
                raw: fe.raw.unwrap().into(),
                files: fe.info.files.clone(),
                conn: Some(h.conn),
                peers: Vec::<SocketAddrV4>::new(),
            },
        )
        .await;
    (r, seen)
}

fn cfg() -> ProbeConfig {
    ProbeConfig {
        unchoke_wait: Duration::from_secs(2),
        stall: Duration::from_secs(2),
        session: Duration::from_secs(10),
        per_min: 60_000,
        ..ProbeConfig::default()
    }
}

/// `max_bytes` caps the piece payload one probe takes. A non-fast peer that chokes,
/// unchokes and then answers both the dropped and the re-sent requests used to make the
/// probe accept twice the budget (128 KiB against 64 KiB).
#[tokio::test]
async fn choke_late_blocks_stay_within_max_bytes() {
    // One 4 MiB piece (block mode), one mkv: first read 64 KiB = 4 blocks.
    let data = mkv(4 * 1024 * KIB, "V_MPEGH/ISO/HEVC");
    let t = Torrent::new("x", vec![("movie.mkv", data.clone())], 4 * 1024 * 1024);
    let c = ProbeConfig {
        max_bytes: 64 * KIB as u64,
        ..cfg()
    };
    let (r, seen) = run(t, data, false, Script::ChokeDouble(4), c.clone()).await;
    let seen = seen.lock().unwrap();
    assert_eq!(seen.requests, 8, "the probe re-sent the 4 dropped requests");
    assert_eq!(seen.piece_bytes_sent, 128 * KIB, "the peer answered all 8");
    assert!(
        r.bytes_in <= c.max_bytes,
        "bytes_in {} past max_bytes {}",
        r.bytes_in,
        c.max_bytes
    );
    // The re-sent blocks answered once: the first read is complete and sniffed.
    assert_eq!(r.bytes_in, 64 * KIB as u64);
    assert!(r.files.iter().any(|f| f.index == 0), "{:?}", r.status);
}

/// "Bytes from a peer later caught lying are not trusted": a result built from a peer's
/// unverified blocks for an earlier target goes when a later target's surprise check strikes
/// that peer (it used to stay, wrong codec at confidence 97, and be uploaded).
#[tokio::test]
async fn struck_peer_voids_earlier_results() {
    // One 128 KiB piece holding a.mkv (100 KiB, the payload) and b.mkv (20 KiB, a PE).
    let a = mkv(100 * KIB, "V_MPEGH/ISO/HEVC");
    let b = pe(20 * KIB);
    let t = Torrent::new(
        "pack",
        vec![("pack/a.mkv", a.clone()), ("pack/b.mkv", b.clone())],
        128 * KIB as u64,
    );
    let mut served = t.data.clone();
    // The liar serves a different mkv head for a.mkv (AVC instead of HEVC).
    let fake = mkv(64 * KIB, "V_MPEG4/ISO/AVC");
    served[..64 * KIB].copy_from_slice(&fake);
    let c = ProbeConfig {
        // block mode: piece 128 KiB > 16 KiB
        whole_piece_max: 16 * KIB as u64,
        ..cfg()
    };
    let (r, seen) = run(t, served, true, Script::Plain, c).await;
    // Upload off: never a piece or an unchoke from the probe.
    let ids = &seen.lock().unwrap().ids;
    assert!(!ids.contains(&7) && !ids.contains(&1), "{ids:?}");
    assert!(r.surprises >= 1, "surprise path ran");
    assert!(
        r.files.iter().all(|f| f.index != 0),
        "a.mkv result from the struck liar kept: {:?}",
        r.files
            .iter()
            .map(|f| (f.index, &f.detection.label))
            .collect::<Vec<_>>()
    );
    assert!(!matches!(r.status, ProbeStatus::Ok), "{:?}", r.status);
}

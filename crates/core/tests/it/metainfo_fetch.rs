//! The BEP 9 metadata fetcher against in-process fake peers on loopback (docs/DHT.md §4, D56):
//! a well-behaved peer, and hostile or broken ones (reject, short block, wrong hash, oversize,
//! lying sizes, no extension bit, wrong infohash, silent, flooding, garbage). Only the
//! `allow_loopback` test seam differs from production.
#![cfg(feature = "dht-crawl")]

use std::net::{Ipv4Addr, SocketAddrV4};
use std::time::{Duration, Instant};

use magnetar_core::liveness::bencode::{self, Value};
use magnetar_core::metainfo::wire;
use magnetar_core::metainfo::{FetchConfig, Fetcher, Outcome, PeerError};
use sha2::Digest;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// How a fake peer misbehaves.
#[derive(Clone, Debug)]
enum Behave {
    Good,
    Reject,
    /// A non-last block one byte short.
    ShortBlock,
    /// Serves these bytes instead (another dict: hash mismatch).
    Serve(Vec<u8>),
    /// Claims this `metadata_size`.
    ClaimSize(i64),
    /// `data` messages with another `total_size`.
    WrongTotal,
    NoExtBit,
    WrongHash,
    NoUtMetadata,
    /// Accepts the connection, then says nothing.
    Silent,
    /// Sends a 4 GiB length prefix on a non-extended message.
    HugeOther,
    /// Sends a 1 MiB `extended` message.
    HugeExtended,
    /// Answers the handshake, then sends random garbage forever.
    Garbage,
    /// Sends metadata requests to us before serving (we must reject them, BEP 9).
    AsksFirst,
    /// Keeps sending `have` messages (discard budget).
    HaveFlood,
    /// Keeps sending 60 KiB `extended` messages of an extension we never negotiated (they
    /// count against the same discard budget).
    ExtFlood,
}

/// A deterministic bencoded info dict: keys sorted, as BEP 3 requires.
fn dict(entries: Vec<(&str, Value)>) -> Value {
    let mut e: Vec<(Vec<u8>, Value)> = entries
        .into_iter()
        .map(|(k, v)| (k.as_bytes().to_vec(), v))
        .collect();
    e.sort_by(|a, b| a.0.cmp(&b.0));
    Value::Dict(e)
}

fn b(s: &str) -> Value {
    Value::Bytes(s.as_bytes().to_vec())
}

/// A v1 multi-file info dict of roughly `pad` bytes (several 16 KiB blocks when large).
fn info_v1(name: &str, pad: usize, private: bool) -> Vec<u8> {
    let mut files = vec![dict(vec![
        ("length", Value::Int(123_456)),
        ("path", Value::List(vec![b("dir"), b("movie.mkv")])),
    ])];
    files.push(dict(vec![
        ("length", Value::Int(42)),
        ("path", Value::List(vec![b("readme.txt")])),
    ]));
    let pieces = vec![0xAB; 20 * (pad / 20).max(1)];
    let mut e = vec![
        ("files", Value::List(files)),
        ("name", b(name)),
        ("piece length", Value::Int(16384)),
        ("pieces", Value::Bytes(pieces)),
    ];
    if private {
        e.push(("private", Value::Int(1)));
    }
    bencode::encode(&dict(e))
}

fn sha1(raw: &[u8]) -> [u8; 20] {
    sha1_smol::Sha1::from(raw).digest().bytes()
}

async fn read_frame(s: &mut TcpStream) -> Option<(u8, Vec<u8>)> {
    let len = s.read_u32().await.ok()? as usize;
    if len == 0 {
        return Some((255, Vec::new()));
    }
    let mut v = vec![0u8; len];
    s.read_exact(&mut v).await.ok()?;
    Some((v[0], v[1..].to_vec()))
}

/// Serve one connection as `behave` for the dict `raw` under `hash`.
async fn serve(mut s: TcpStream, raw: Vec<u8>, hash: [u8; 20], behave: Behave) {
    let mut hs = [0u8; 68];
    if s.read_exact(&mut hs).await.is_err() {
        return;
    }
    if matches!(behave, Behave::Silent) {
        tokio::time::sleep(Duration::from_secs(60)).await;
        return;
    }
    let mut ours = wire::handshake(&hash, &[0x42; 20]);
    if matches!(behave, Behave::NoExtBit) {
        ours[25] = 0;
    }
    if matches!(behave, Behave::WrongHash) {
        ours[28] ^= 1;
    }
    if s.write_all(&ours).await.is_err() {
        return;
    }
    // Noise before the extended handshake: a bitfield, as real clients send.
    let _ = s.write_all(&wire::frame(5, &[0xFF; 8])).await;
    match behave {
        Behave::HugeOther => {
            let _ = s.write_all(&u32::MAX.to_be_bytes()).await;
            let _ = s.write_all(&[4]).await;
            tokio::time::sleep(Duration::from_secs(60)).await;
            return;
        }
        Behave::HugeExtended => {
            let _ = s.write_all(&(1u32 << 20).to_be_bytes()).await;
            let _ = s.write_all(&[20, 0]).await;
            let _ = s.write_all(&vec![b'x'; 1 << 20]).await;
            return;
        }
        Behave::Garbage => {
            for _ in 0..1000 {
                let junk: Vec<u8> = (0..512).map(|_| rand::random()).collect();
                if s.write_all(&junk).await.is_err() {
                    return;
                }
            }
            return;
        }
        Behave::ExtFlood => {
            let junk = vec![b'x'; 60 * 1024];
            for _ in 0..200 {
                if s.write_all(&wire::extended(7, &junk)).await.is_err() {
                    return;
                }
            }
            tokio::time::sleep(Duration::from_secs(60)).await;
            return;
        }
        Behave::HaveFlood => {
            for i in 0..2_000_000u32 {
                if s.write_all(&wire::frame(4, &i.to_be_bytes()))
                    .await
                    .is_err()
                {
                    return;
                }
            }
            return;
        }
        _ => {}
    }
    let size = match &behave {
        Behave::ClaimSize(n) => *n,
        Behave::Serve(other) => other.len() as i64,
        _ => raw.len() as i64,
    };
    let body = &match &behave {
        Behave::Serve(other) => other.clone(),
        _ => raw.clone(),
    };
    let mut m = vec![];
    if !matches!(behave, Behave::NoUtMetadata) {
        m.push(("ut_metadata", Value::Int(3)));
    }
    m.push(("ut_pex", Value::Int(1)));
    let theirs = dict(vec![
        ("m", dict(m)),
        ("metadata_size", Value::Int(size)),
        ("v", b("FakePeer 1.0")),
    ]);
    let mut got_ext = false;
    let mut rejects_seen = 0;
    loop {
        let Some((id, payload)) = read_frame(&mut s).await else {
            return;
        };
        if id != 20 || payload.is_empty() {
            continue;
        }
        if payload[0] == 0 && !got_ext {
            got_ext = true;
            let _ = s
                .write_all(&wire::extended(0, &bencode::encode(&theirs)))
                .await;
            if matches!(behave, Behave::AsksFirst) {
                for p in 0..3 {
                    let _ = s.write_all(&wire::metadata_request(1, p)).await;
                }
            }
            continue;
        }
        if payload[0] == 1 {
            // A message to OUR ut_metadata id... we are the server here; ignore.
            continue;
        }
        if payload[0] != 3 {
            continue;
        }
        let (req, _) = bencode::decode_prefix(&payload[1..]).unwrap();
        let mt = req.get(b"msg_type").and_then(Value::as_int).unwrap();
        if mt == 2 {
            rejects_seen += 1;
            assert!(rejects_seen <= 64);
            continue;
        }
        let piece = req.get(b"piece").and_then(Value::as_int).unwrap() as usize;
        if matches!(behave, Behave::Reject) {
            let r = dict(vec![
                ("msg_type", Value::Int(2)),
                ("piece", Value::Int(piece as i64)),
            ]);
            let _ = s.write_all(&wire::extended(1, &bencode::encode(&r))).await;
            continue;
        }
        let start = piece * wire::BLOCK;
        let end = (start + wire::BLOCK).min(body.len());
        let mut block = body.get(start..end).unwrap_or_default().to_vec();
        if matches!(behave, Behave::ShortBlock) && end - start == wire::BLOCK {
            block.pop();
        }
        let total = if matches!(behave, Behave::WrongTotal) {
            size + 1
        } else {
            size
        };
        let d = dict(vec![
            ("msg_type", Value::Int(1)),
            ("piece", Value::Int(piece as i64)),
            ("total_size", Value::Int(total)),
        ]);
        let mut msg = bencode::encode(&d);
        msg.extend_from_slice(&block);
        if s.write_all(&wire::extended(1, &msg)).await.is_err() {
            return;
        }
    }
}

/// A fake peer on 127.0.0.1: every connection behaves as `behave`.
async fn peer(raw: Vec<u8>, hash: [u8; 20], behave: Behave) -> SocketAddrV4 {
    let l = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let addr = match l.local_addr().unwrap() {
        std::net::SocketAddr::V4(a) => a,
        _ => unreachable!(),
    };
    tokio::spawn(async move {
        while let Ok((s, _)) = l.accept().await {
            tokio::spawn(serve(s, raw.clone(), hash, behave.clone()));
        }
    });
    addr
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
        ..FetchConfig::default()
    })
}

#[tokio::test]
async fn good_peer_multi_block() {
    let raw = info_v1("Some.Release.2024.1080p", 50_000, false);
    assert!(raw.len() > 3 * wire::BLOCK, "spans several blocks");
    let h = sha1(&raw);
    let p = peer(raw.clone(), h, Behave::Good).await;
    let f = fetcher();
    let out = f.fetch(h, &[p]).await.unwrap();
    let Outcome::Ok(got) = out else {
        panic!("{out:?}")
    };
    assert_eq!(got.v1, h);
    assert_eq!(got.sighted, h);
    assert_eq!(got.v2, None);
    assert_eq!(got.metadata_size, raw.len());
    assert_eq!(got.info.name, "Some.Release.2024.1080p");
    assert_eq!(got.info.files.len(), 2);
    assert_eq!(got.info.files[0].path, "dir/movie.mkv");
    assert_eq!(got.info.total_size, 123_456 + 42);
    let st = f.stats();
    assert_eq!((st.hashes, st.ok), (1, 1));
    assert!(st.tcp_bytes_in as usize > raw.len());
}

#[tokio::test]
async fn our_extended_handshake_waits_for_the_peers_bit() {
    // A peer without the extension bit never receives a message 20 from us.
    let l = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let addr = match l.local_addr().unwrap() {
        std::net::SocketAddr::V4(a) => a,
        _ => unreachable!(),
    };
    let h = [7u8; 20];
    let server = tokio::spawn(async move {
        let (mut s, _) = l.accept().await.unwrap();
        let mut hs = [0u8; 68];
        s.read_exact(&mut hs).await.unwrap();
        assert_eq!(hs[25] & 0x10, 0x10, "we set the BEP 10 bit");
        let mut ours = wire::handshake(&h, &[1; 20]);
        ours[25] = 0;
        s.write_all(&ours).await.unwrap();
        let mut rest = Vec::new();
        let _ = tokio::time::timeout(Duration::from_secs(2), s.read_to_end(&mut rest)).await;
        rest
    });
    let out = fetcher().fetch(h, &[addr]).await.unwrap_err();
    assert_eq!(out.error, PeerError::NoExtensions);
    assert!(
        server.await.unwrap().is_empty(),
        "nothing sent after the handshake"
    );
}

#[tokio::test]
async fn hostile_peers_fail_cleanly() {
    let raw = info_v1("x", 40_000, false);
    let h = sha1(&raw);
    let other = info_v1("y", 40_000, false);
    let cases: Vec<(Behave, PeerError)> = vec![
        (Behave::Reject, PeerError::Reject),
        (Behave::ShortBlock, PeerError::BadBlock),
        (Behave::Serve(other), PeerError::HashMismatch),
        (Behave::ClaimSize(2 << 20), PeerError::Oversize),
        (Behave::ClaimSize(0), PeerError::NoMetadataSize),
        (Behave::ClaimSize(-5), PeerError::NoMetadataSize),
        (Behave::ClaimSize(raw.len() as i64 + 1), PeerError::BadBlock),
        (Behave::ClaimSize(raw.len() as i64 - 1), PeerError::BadBlock),
        (Behave::WrongTotal, PeerError::BadBlock),
        (Behave::NoExtBit, PeerError::NoExtensions),
        (Behave::WrongHash, PeerError::Handshake),
        (Behave::NoUtMetadata, PeerError::NoUtMetadata),
        (Behave::Silent, PeerError::Timeout),
        (Behave::HugeOther, PeerError::Protocol),
        (Behave::HugeExtended, PeerError::Protocol),
        (Behave::HaveFlood, PeerError::Protocol),
        (Behave::ExtFlood, PeerError::Protocol),
    ];
    let f = fetcher();
    for (behave, want) in cases {
        let p = peer(raw.clone(), h, behave.clone()).await;
        let t0 = Instant::now();
        let err = f.fetch(h, &[p]).await.unwrap_err();
        assert!(t0.elapsed() < Duration::from_secs(5), "{behave:?} bounded");
        // A lying size can also end as a short read (closed) or a timeout: the block that
        // cannot exist is never sent. Only the exact codes are checked where they are certain.
        match behave {
            Behave::ClaimSize(n) if n > 0 && n < (1 << 20) => assert!(
                matches!(
                    err.error,
                    PeerError::BadBlock | PeerError::Timeout | PeerError::HashMismatch
                ),
                "{behave:?}: {err:?}"
            ),
            // 4 MiB of 9-byte messages: the discard budget or the handshake timeout ends it,
            // whichever comes first on this machine.
            Behave::HaveFlood => assert!(
                matches!(err.error, PeerError::Protocol | PeerError::Timeout),
                "{err:?}"
            ),
            _ => assert_eq!(err.error, want, "{behave:?}"),
        }
        assert_eq!(err.peers_tried, 1);
    }
    // Garbage after the handshake: any failure, never a panic, bounded.
    let p = peer(raw.clone(), h, Behave::Garbage).await;
    assert!(f.fetch(h, &[p]).await.is_err());
    let st = f.stats();
    assert_eq!(st.ok, 0);
    assert!(st.oversize >= 1, "{st:?}");
}

#[tokio::test]
async fn a_bad_peer_does_not_stop_a_good_one() {
    let raw = info_v1("Two.Peers", 20_000, false);
    let h = sha1(&raw);
    let bad = peer(raw.clone(), h, Behave::Serve(info_v1("z", 20_000, false))).await;
    let silent = peer(raw.clone(), h, Behave::Silent).await;
    let good = peer(raw.clone(), h, Behave::Good).await;
    let out = fetcher().fetch(h, &[bad, silent, good]).await.unwrap();
    assert!(matches!(out, Outcome::Ok(_)));
}

#[tokio::test]
async fn incoming_requests_are_rejected() {
    let raw = info_v1("Asker", 1000, false);
    let h = sha1(&raw);
    let p = peer(raw.clone(), h, Behave::AsksFirst).await;
    assert!(matches!(
        fetcher().fetch(h, &[p]).await.unwrap(),
        Outcome::Ok(_)
    ));
}

#[tokio::test]
async fn private_v2_only_hybrid_and_duplicates() {
    let f = fetcher();
    // BEP 27 private: verified but not storable.
    let raw = info_v1("Private", 1000, true);
    let h = sha1(&raw);
    let p = peer(raw.clone(), h, Behave::Good).await;
    assert!(matches!(f.fetch(h, &[p]).await.unwrap(), Outcome::Private));

    // Hybrid sighted under its truncated SHA-256: stored under the SHA-1, v2 kept.
    let tree = dict(vec![(
        "a.bin",
        dict(vec![(
            "",
            dict(vec![
                ("length", Value::Int(5000)),
                ("pieces root", Value::Bytes(vec![1; 32])),
            ]),
        )]),
    )]);
    let hy = bencode::encode(&dict(vec![
        ("file tree", tree.clone()),
        ("length", Value::Int(5000)),
        ("meta version", Value::Int(2)),
        ("name", b("a.bin")),
        ("piece length", Value::Int(16384)),
        ("pieces", Value::Bytes(vec![2; 20])),
    ]));
    let s1 = sha1(&hy);
    let s2: [u8; 32] = sha2::Sha256::digest(&hy).into();
    let t: [u8; 20] = s2[..20].try_into().unwrap();
    let p = peer(hy.clone(), t, Behave::Good).await;
    let Outcome::Ok(got) = f.fetch(t, &[p]).await.unwrap() else {
        panic!()
    };
    assert_eq!((got.sighted, got.v1, got.v2), (t, s1, Some(s2)));

    // Pure v2: no v1 identity.
    let v2 = bencode::encode(&dict(vec![
        ("file tree", tree),
        ("meta version", Value::Int(2)),
        ("name", b("a.bin")),
        ("piece length", Value::Int(16384)),
    ]));
    let s2: [u8; 32] = sha2::Sha256::digest(&v2).into();
    let t: [u8; 20] = s2[..20].try_into().unwrap();
    let p = peer(v2, t, Behave::Good).await;
    assert!(matches!(f.fetch(t, &[p]).await.unwrap(), Outcome::V2Only));

    // Duplicate keys: the bytes hash fine, the dict is refused.
    let dup = b"d6:lengthi5e4:name4:good4:name3:bad12:piece lengthi16384e6:pieces20:aaaaaaaaaaaaaaaaaaaae".to_vec();
    let h = sha1(&dup);
    let p = peer(dup, h, Behave::Good).await;
    let out = f.fetch(h, &[p]).await.unwrap();
    assert_eq!(out.code(), "duplicate_keys");
    let st = f.stats();
    assert_eq!(
        (st.private, st.v2_only, st.invalid, st.hybrid),
        (1, 1, 1, 1)
    );
}

#[tokio::test]
async fn loopback_and_lan_peers_are_refused_in_production() {
    let raw = info_v1("x", 1000, false);
    let h = sha1(&raw);
    let p = peer(raw, h, Behave::Good).await;
    let f = Fetcher::new(FetchConfig::default());
    let lan: SocketAddrV4 = "192.168.1.1:80".parse().unwrap();
    let err = f.fetch(h, &[p, lan]).await.unwrap_err();
    assert_eq!(err.error, PeerError::Filtered);
    assert_eq!(f.stats().connected, 0, "no connection attempted");
}

#[tokio::test]
async fn no_peers_and_the_peer_cap() {
    let f = fetcher();
    let err = f.fetch([1; 20], &[]).await.unwrap_err();
    assert_eq!(err.code(), "no_peers");
    // 20 silent peers: at most 12 are tried, 4 at once, and the hash budget bounds it.
    let mut ps = Vec::new();
    for _ in 0..20 {
        ps.push(peer(vec![], [1; 20], Behave::Silent).await);
    }
    let f = Fetcher::new(FetchConfig {
        allow_loopback: true,
        peer_timeout: Duration::from_millis(300),
        connects_per_sec: 1000,
        ..FetchConfig::default()
    });
    let t0 = Instant::now();
    let err = f.fetch([1; 20], &ps).await.unwrap_err();
    assert_eq!(err.peers_tried, 12);
    assert!(t0.elapsed() < Duration::from_secs(3), "{:?}", t0.elapsed());
}

#[tokio::test]
async fn open_connections_are_capped() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    // A peer that holds every connection silently and records the peak of open ones.
    let l = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let addr = match l.local_addr().unwrap() {
        std::net::SocketAddr::V4(a) => a,
        _ => unreachable!(),
    };
    let open = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let (o, pk) = (open.clone(), peak.clone());
    tokio::spawn(async move {
        while let Ok((mut s, _)) = l.accept().await {
            let (o, pk) = (o.clone(), pk.clone());
            tokio::spawn(async move {
                let n = o.fetch_add(1, Ordering::SeqCst) + 1;
                pk.fetch_max(n, Ordering::SeqCst);
                let mut buf = [0u8; 256];
                while let Ok(k) = s.read(&mut buf).await {
                    if k == 0 {
                        break;
                    }
                }
                o.fetch_sub(1, Ordering::SeqCst);
            });
        }
    });
    let f = Fetcher::new(FetchConfig {
        allow_loopback: true,
        connects_per_sec: 1000,
        max_connections: 3,
        peer_timeout: Duration::from_millis(400),
        handshake_timeout: Duration::from_millis(300),
        ..FetchConfig::default()
    });
    // 6 hashes × 4 parallel peers would open 24 at once without the cap.
    let jobs: Vec<_> = (0..6u8)
        .map(|i| {
            let f = f.clone();
            tokio::spawn(async move { f.fetch([i; 20], &[addr; 4]).await })
        })
        .collect();
    for j in jobs {
        assert!(j.await.unwrap().is_err());
    }
    assert!(peak.load(Ordering::SeqCst) <= 3, "{peak:?}");
    assert!(peak.load(Ordering::SeqCst) >= 1);
}

#[tokio::test]
async fn value_bombs_are_refused_before_decoding() {
    // 300 000 empty lists (2 bytes each, ~32× that as a decoded tree): above the value limit
    // for a 1 MiB metadata_max (131 072), so the verified dict is `too_complex`, not decoded.
    let bomb = Value::List((0..300_000).map(|_| Value::List(vec![])).collect());
    let raw = bencode::encode(&dict(vec![
        ("length", Value::Int(1)),
        ("name", b("bomb")),
        ("x", bomb),
    ]));
    assert!(raw.len() < 1 << 20);
    let h = sha1(&raw);
    let p = peer(raw.clone(), h, Behave::Good).await;
    let out = fetcher().fetch(h, &[p]).await.unwrap();
    assert!(
        matches!(
            out,
            Outcome::Invalid(magnetar_core::metainfo::InfoError::TooComplex)
        ),
        "{out:?}"
    );
    // The same dict with a value count under the limit decodes normally.
    let ok = bencode::encode(&dict(vec![
        ("length", Value::Int(1)),
        ("name", b("fine")),
        (
            "x",
            Value::List((0..1000).map(|_| Value::List(vec![])).collect()),
        ),
    ]));
    let h = sha1(&ok);
    let p = peer(ok.clone(), h, Behave::Good).await;
    assert!(matches!(
        fetcher().fetch(h, &[p]).await.unwrap(),
        Outcome::Ok(_)
    ));
}

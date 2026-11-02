//! A fake BitTorrent peer on 127.0.0.1 for the header-probe tests (D117): it serves an info
//! dict over `ut_metadata` and piece blocks, misbehaves on request (no Fast extension, late or
//! no unchoke, re-choke, `reqq`, allowed-fast, out-of-order blocks, rejects, lies, hostile
//! requests, junk), and records everything the prober sends: handshake, `v`, message ids,
//! outstanding requests, piece bytes, `have`s, unchokes and forbidden messages.
//!
//! Ported in spirit from the wire report's bench seeder (`probes/wire/bench`); loopback only.

use std::collections::VecDeque;
use std::net::{Ipv4Addr, SocketAddrV4};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use magnetar_core::liveness::bencode::{self, Value};
use magnetar_core::metainfo::{InfoFile, parse_info, wire};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::tcp::OwnedWriteHalf;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio::time::Instant;

pub const BLOCK: u32 = 16 * 1024;

/// A torrent: the info dict and its data.
pub struct Torrent {
    pub info: Vec<u8>,
    pub hash: [u8; 20],
    pub data: Vec<u8>,
    pub piece_len: u64,
}

fn dict(entries: Vec<(&str, Value)>) -> Value {
    let mut e: Vec<(Vec<u8>, Value)> = entries
        .into_iter()
        .map(|(k, v)| (k.as_bytes().to_vec(), v))
        .collect();
    e.sort_by(|a, b| a.0.cmp(&b.0));
    Value::Dict(e)
}

fn bytes(s: &str) -> Value {
    Value::Bytes(s.as_bytes().to_vec())
}

impl Torrent {
    /// A single-file torrent (`files.len() == 1` and no directory) or a multi-file one.
    pub fn new(name: &str, files: Vec<(&str, Vec<u8>)>, piece_len: u64) -> Arc<Torrent> {
        let mut data = Vec::new();
        let single = files.len() == 1;
        let mut list = Vec::new();
        for (path, d) in &files {
            list.push(dict(vec![
                ("length", Value::Int(d.len() as i64)),
                (
                    "path",
                    Value::List(path.split('/').map(bytes).collect::<Vec<_>>()),
                ),
            ]));
            data.extend_from_slice(d);
        }
        let mut pieces = Vec::new();
        for c in data.chunks(piece_len as usize) {
            pieces.extend_from_slice(&sha1_smol::Sha1::from(c).digest().bytes());
        }
        let mut e = vec![
            ("name", bytes(if single { files[0].0 } else { name })),
            ("piece length", Value::Int(piece_len as i64)),
            ("pieces", Value::Bytes(pieces)),
        ];
        if single {
            e.push(("length", Value::Int(data.len() as i64)));
        } else {
            e.push(("files", Value::List(list)));
        }
        let info = bencode::encode(&dict(e));
        let hash = sha1_smol::Sha1::from(&info).digest().bytes();
        Arc::new(Torrent {
            info,
            hash,
            data,
            piece_len,
        })
    }

    pub fn pieces(&self) -> u32 {
        self.data.len().div_ceil(self.piece_len as usize) as u32
    }

    pub fn files(&self) -> Vec<InfoFile> {
        parse_info(&self.info).unwrap().files
    }

    fn block(&self, index: u32, begin: u32, len: u32) -> Option<Vec<u8>> {
        let s = index as u64 * self.piece_len + begin as u64;
        let e = s + len as u64;
        if begin as u64 + len as u64 > self.piece_len || e > self.data.len() as u64 || len == 0 {
            return None;
        }
        Some(self.data[s as usize..e as usize].to_vec())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unchoke {
    /// When the prober says it is interested.
    OnInterest,
    After(Duration),
    Never,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Has {
    All,
    Pieces(Vec<u32>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Junk {
    None,
    /// A `piece` message with a 20 000-byte block.
    OversizePiece,
    /// A bitfield with the spare bits set.
    SpareBits,
    /// A block nobody asked for.
    Unrequested,
    /// Thousands of `have` and keep-alives.
    Flood,
    /// A `have` past the last piece.
    BadHave,
}

#[derive(Clone, Debug)]
pub struct Opts {
    pub fast: bool,
    pub unchoke: Unchoke,
    /// After this many blocks served, choke for 300 ms (requests dropped without BEP 6).
    pub rechoke_after: Option<usize>,
    pub reqq: Option<u32>,
    pub allowed_fast: Vec<u32>,
    /// Serve the blocks of each pair of requests in reverse order.
    pub out_of_order: bool,
    /// Reject every n-th request (BEP 6 only).
    pub reject_every: Option<usize>,
    /// Serve these bytes instead of the torrent's (same length).
    pub lie: Option<Arc<Vec<u8>>>,
    /// Right after the handshake: `interested`, a block request and a metadata request.
    pub hostile: bool,
    pub has: Has,
    /// Interested in the prober; requests what it announces, `repeat` times each.
    pub leech: Option<usize>,
    pub junk: Junk,
    /// Delay before each of the first `delay_first` blocks (all when 0).
    pub delay: Duration,
    pub delay_first: usize,
    /// Send `have_all` (or a full bitfield) this long after the handshake.
    pub have_all_after: Option<Duration>,
}

impl Default for Opts {
    fn default() -> Self {
        Self {
            fast: true,
            unchoke: Unchoke::OnInterest,
            rechoke_after: None,
            reqq: None,
            allowed_fast: Vec::new(),
            out_of_order: false,
            reject_every: None,
            lie: None,
            hostile: false,
            has: Has::All,
            leech: None,
            junk: Junk::None,
            delay: Duration::ZERO,
            delay_first: 0,
            have_all_after: None,
        }
    }
}

/// What the prober sent, over every connection.
#[derive(Clone, Debug, Default)]
pub struct Log {
    pub accepts: usize,
    pub peer_ids: Vec<[u8; 20]>,
    pub reserved: Vec<[u8; 8]>,
    pub client: Vec<String>,
    /// Message ids received from the prober, in order (255 = keep-alive).
    pub ids: Vec<u8>,
    /// `ut_metadata` msg_types received from the prober.
    pub metadata_types: Vec<i64>,
    pub requests: usize,
    pub max_outstanding: usize,
    /// `unchoke`, `have`, `bitfield`, `piece`, `have_all`, `ut_metadata` data.
    pub forbidden: usize,
    pub haves: Vec<u32>,
    pub unchokes: usize,
    pub chokes: usize,
    /// Piece payload received from the prober, and whether every byte was right.
    pub piece_bytes: u64,
    pub piece_bytes_ok: bool,
    pub pieces_got: Vec<(u32, u32, u32)>,
    pub rejects_got: usize,
    /// Blocks this peer served.
    pub served: usize,
    pub served_bytes: u64,
    /// Requests sent to the prober (leech mode).
    pub asked: usize,
}

pub struct Seeder {
    pub addr: SocketAddrV4,
    pub log: Arc<Mutex<Log>>,
}

impl Seeder {
    pub fn log(&self) -> Log {
        self.log.lock().unwrap().clone()
    }
}

pub async fn start(t: Arc<Torrent>, opts: Opts) -> Seeder {
    start_at(Ipv4Addr::LOCALHOST, t, opts).await
}

/// [`start`] on another loopback address (the server's worker tests announce it on a fake DHT
/// from that address).
pub async fn start_at(ip: Ipv4Addr, t: Arc<Torrent>, opts: Opts) -> Seeder {
    let l = TcpListener::bind((ip, 0)).await.unwrap();
    let addr = match l.local_addr().unwrap() {
        std::net::SocketAddr::V4(a) => a,
        _ => unreachable!(),
    };
    let log = Arc::new(Mutex::new(Log {
        piece_bytes_ok: true,
        ..Log::default()
    }));
    let lg = log.clone();
    tokio::spawn(async move {
        while let Ok((s, _)) = l.accept().await {
            lg.lock().unwrap().accepts += 1;
            tokio::spawn(conn(s, t.clone(), opts.clone(), lg.clone()));
        }
    });
    Seeder { addr, log }
}

fn req(index: u32, begin: u32, len: u32) -> Vec<u8> {
    let mut b = Vec::with_capacity(12);
    b.extend_from_slice(&index.to_be_bytes());
    b.extend_from_slice(&begin.to_be_bytes());
    b.extend_from_slice(&len.to_be_bytes());
    b
}

fn piece_msg(index: u32, begin: u32, data: &[u8]) -> Vec<u8> {
    let mut b = Vec::with_capacity(8 + data.len());
    b.extend_from_slice(&index.to_be_bytes());
    b.extend_from_slice(&begin.to_be_bytes());
    b.extend_from_slice(data);
    wire::frame(7, &b)
}

fn be(b: &[u8], i: usize) -> u32 {
    u32::from_be_bytes(b[i..i + 4].try_into().unwrap())
}

fn bitfield(t: &Torrent, has: &[u32], spare: bool) -> Vec<u8> {
    let n = t.pieces() as usize;
    let mut bf = vec![0u8; n.div_ceil(8)];
    for &i in has {
        if (i as usize) < n {
            bf[i as usize / 8] |= 0x80 >> (i % 8);
        }
    }
    if spare && !n.is_multiple_of(8) {
        bf[n / 8] |= 0x01;
    }
    bf
}

enum Ev {
    Frame(u8, Vec<u8>),
    End,
    Tick,
}

struct State {
    choked: bool,
    outstanding: VecDeque<(u32, u32, u32)>,
    served: usize,
    requests_seen: usize,
    unchoke_at: Option<Instant>,
    held: Option<(u32, u32, u32)>,
    junk_done: bool,
    prober_has: Vec<u32>,
    unchoked_by_prober: bool,
    have_all_at: Option<Instant>,
    asked: std::collections::HashSet<u32>,
}

async fn send(w: &mut OwnedWriteHalf, b: &[u8]) -> bool {
    w.write_all(b).await.is_ok()
}

async fn conn(s: TcpStream, t: Arc<Torrent>, o: Opts, log: Arc<Mutex<Log>>) {
    let (mut rd, mut w) = s.into_split();
    let mut hs = [0u8; 68];
    if rd.read_exact(&mut hs).await.is_err() || hs[28..48] != t.hash {
        return;
    }
    let prober_fast = hs[27] & 0x04 != 0;
    {
        let mut l = log.lock().unwrap();
        l.reserved.push(hs[20..28].try_into().unwrap());
        l.peer_ids.push(hs[48..68].try_into().unwrap());
    }
    let ours = wire::handshake_with(&t.hash, &[0x53; 20], o.fast);
    if !send(&mut w, &ours).await {
        return;
    }
    let fast = o.fast && prober_fast;
    // Our availability first (BEP 6 when both are fast).
    let all: Vec<u32> = (0..t.pieces()).collect();
    let has: &[u32] = match &o.has {
        Has::All => &all,
        Has::Pieces(p) => p,
    };
    let mut first = Vec::new();
    if fast && o.has == Has::All && o.junk != Junk::SpareBits {
        first.extend(wire::frame(14, &[]));
    } else if fast && has.is_empty() {
        first.extend(wire::frame(15, &[]));
    } else if !has.is_empty() {
        first.extend(wire::frame(
            5,
            &bitfield(&t, has, o.junk == Junk::SpareBits),
        ));
    }
    if fast {
        for &i in &o.allowed_fast {
            first.extend(wire::frame(17, &i.to_be_bytes()));
        }
    }
    let mut m = vec![("ut_metadata", Value::Int(2))];
    m.push(("ut_pex", Value::Int(9)));
    let mut e = vec![
        ("m", dict(m)),
        ("metadata_size", Value::Int(t.info.len() as i64)),
        ("v", bytes("FakeSeed 1.0")),
    ];
    if let Some(q) = o.reqq {
        e.push(("reqq", Value::Int(q as i64)));
    }
    first.extend(wire::extended(0, &bencode::encode(&dict(e))));
    if o.hostile || o.leech.is_some() {
        first.extend(wire::frame(2, &[]));
    }
    if o.hostile {
        first.extend(wire::frame(6, &req(0, 0, BLOCK)));
        first.extend(wire::metadata_request(1, 0));
    }
    if o.junk == Junk::Flood {
        for i in 0..3000u32 {
            first.extend(wire::frame(4, &(i % t.pieces()).to_be_bytes()));
            first.extend([0, 0, 0, 0]);
        }
    }
    if o.junk == Junk::BadHave {
        first.extend(wire::frame(4, &(t.pieces() + 5).to_be_bytes()));
    }
    if !send(&mut w, &first).await {
        return;
    }
    // Reader task: frames → channel (so timers can run between frames). Outstanding requests
    // are counted when they arrive: received minus answered (served, rejected or dropped).
    let (tx, mut rx) = mpsc::channel::<Option<(u8, Vec<u8>)>>(1024);
    let answered = Arc::new(AtomicUsize::new(0));
    let ans = answered.clone();
    let lg = log.clone();
    tokio::spawn(async move {
        let mut received = 0usize;
        while let Ok(len) = rd.read_u32().await {
            if len == 0 {
                if tx.send(Some((255, Vec::new()))).await.is_err() {
                    return;
                }
                continue;
            }
            if len > 1 << 20 {
                break;
            }
            let mut v = vec![0u8; len as usize];
            if rd.read_exact(&mut v).await.is_err() {
                break;
            }
            let id = v.remove(0);
            if id == 6 {
                received += 1;
                let out = received.saturating_sub(ans.load(Ordering::SeqCst));
                let mut l = lg.lock().unwrap();
                l.max_outstanding = l.max_outstanding.max(out);
            }
            if tx.send(Some((id, v))).await.is_err() {
                return;
            }
        }
        let _ = tx.send(None).await;
    });
    let mut st = State {
        choked: true,
        outstanding: VecDeque::new(),
        served: 0,
        requests_seen: 0,
        unchoke_at: match o.unchoke {
            Unchoke::After(d) => Some(Instant::now() + d),
            _ => None,
        },
        held: None,
        junk_done: false,
        prober_has: Vec::new(),
        unchoked_by_prober: false,
        have_all_at: o.have_all_after.map(|d| Instant::now() + d),
        asked: Default::default(),
    };
    loop {
        let wake = st
            .unchoke_at
            .unwrap_or_else(|| Instant::now() + Duration::from_millis(50))
            .min(Instant::now() + Duration::from_millis(50));
        let ev = tokio::select! {
            m = rx.recv() => match m {
                Some(Some((id, b))) => Ev::Frame(id, b),
                _ => Ev::End,
            },
            _ = tokio::time::sleep_until(wake) => Ev::Tick,
        };
        // Timers.
        if st.have_all_at.is_some_and(|t| Instant::now() >= t) {
            st.have_all_at = None;
            let m = if fast {
                wire::frame(14, &[])
            } else {
                wire::frame(5, &bitfield(&t, &all, false))
            };
            if !send(&mut w, &m).await {
                return;
            }
        }
        if st.unchoke_at.is_some_and(|t| Instant::now() >= t) {
            st.unchoke_at = None;
            st.choked = false;
            if !send(&mut w, &wire::frame(1, &[])).await {
                return;
            }
        }
        if let Some(h) = st.held.take()
            && !st.choked
        {
            if !serve_one(&mut w, &t, &o, &log, &mut st, h).await {
                return;
            }
            answered.fetch_add(1, Ordering::SeqCst);
        }
        let (id, body) = match ev {
            Ev::Tick => continue,
            Ev::End => return,
            Ev::Frame(id, body) => (id, body),
        };
        log.lock().unwrap().ids.push(id);
        match id {
            1 | 4 | 5 | 7 | 14 => {
                {
                    let mut l = log.lock().unwrap();
                    l.forbidden += 1;
                    match id {
                        1 => l.unchokes += 1,
                        4 => l.haves.push(be(&body, 0)),
                        7 => {
                            let (i, b) = (be(&body, 0), be(&body, 4));
                            let d = &body[8..];
                            l.piece_bytes += d.len() as u64;
                            l.pieces_got.push((i, b, d.len() as u32));
                            if t.block(i, b, d.len() as u32).as_deref() != Some(d) {
                                l.piece_bytes_ok = false;
                            }
                        }
                        _ => {}
                    }
                }
                if id == 1 {
                    st.unchoked_by_prober = true;
                }
                if id == 4 {
                    st.prober_has.push(be(&body, 0));
                }
                if let Some(rep) = o.leech
                    && st.unchoked_by_prober
                {
                    // Ask for every announced piece, `rep` times, plus one it never had.
                    let mut out = Vec::new();
                    for &p in &st.prober_has.clone() {
                        if !st.asked.insert(p) {
                            continue;
                        }
                        let plen =
                            (t.data.len() as u64 - p as u64 * t.piece_len).min(t.piece_len) as u32;
                        for _ in 0..rep {
                            let mut b = 0;
                            while b < plen {
                                out.extend(wire::frame(6, &req(p, b, BLOCK.min(plen - b))));
                                log.lock().unwrap().asked += 1;
                                b += BLOCK;
                            }
                        }
                        let other = (p + 1) % t.pieces();
                        out.extend(wire::frame(6, &req(other, 0, BLOCK)));
                        log.lock().unwrap().asked += 1;
                    }
                    if !out.is_empty() && !send(&mut w, &out).await {
                        return;
                    }
                }
            }
            0 => log.lock().unwrap().chokes += 1,
            16 => log.lock().unwrap().rejects_got += 1,
            2 => {
                if o.unchoke == Unchoke::OnInterest && st.choked {
                    st.choked = false;
                    if !send(&mut w, &wire::frame(1, &[])).await {
                        return;
                    }
                }
            }
            20 => {
                if body.is_empty() {
                    continue;
                }
                if body[0] == 0 {
                    if let Ok(v) = bencode::decode(&body[1..])
                        && let Some(c) = v.get(b"v").and_then(Value::as_bytes)
                    {
                        log.lock()
                            .unwrap()
                            .client
                            .push(String::from_utf8_lossy(c).into_owned());
                    }
                    continue;
                }
                if body[0] != 2 {
                    continue;
                }
                let Ok((v, _)) = bencode::decode_prefix(&body[1..]) else {
                    continue;
                };
                let mt = v.get(b"msg_type").and_then(Value::as_int).unwrap_or(-1);
                log.lock().unwrap().metadata_types.push(mt);
                if mt == 1 {
                    log.lock().unwrap().forbidden += 1;
                }
                if mt != 0 {
                    continue;
                }
                let piece = v.get(b"piece").and_then(Value::as_int).unwrap_or(0) as usize;
                let s = piece * wire::BLOCK;
                let e = (s + wire::BLOCK).min(t.info.len());
                let d = dict(vec![
                    ("msg_type", Value::Int(1)),
                    ("piece", Value::Int(piece as i64)),
                    ("total_size", Value::Int(t.info.len() as i64)),
                ]);
                let mut msg = bencode::encode(&d);
                msg.extend_from_slice(&t.info[s..e]);
                if !send(&mut w, &wire::extended(1, &msg)).await {
                    return;
                }
            }
            6 => {
                let r = (be(&body, 0), be(&body, 4), be(&body, 8));
                st.requests_seen += 1;
                st.outstanding.push_back(r);
                log.lock().unwrap().requests += 1;
                if !st.junk_done && o.junk != Junk::None {
                    st.junk_done = true;
                    let j = match o.junk {
                        Junk::OversizePiece => Some(piece_msg(r.0, r.1, &vec![0u8; 20_000])),
                        Junk::Unrequested => Some(piece_msg(r.0, r.1 + 7 * BLOCK, &[1u8; 100])),
                        _ => None,
                    };
                    if let Some(j) = j {
                        let _ = send(&mut w, &j).await;
                        continue;
                    }
                }
                let allowed = fast && o.allowed_fast.contains(&r.0);
                if st.choked && !allowed {
                    answered.fetch_add(1, Ordering::SeqCst);
                    if fast {
                        st.outstanding.pop_back();
                        if !send(&mut w, &wire::frame(16, &req(r.0, r.1, r.2))).await {
                            return;
                        }
                    }
                    // Without BEP 6 a request while choked is dropped.
                    else {
                        st.outstanding.pop_back();
                    }
                    continue;
                }
                if fast
                    && o.reject_every
                        .is_some_and(|n| st.requests_seen.is_multiple_of(n))
                {
                    answered.fetch_add(1, Ordering::SeqCst);
                    st.outstanding.pop_back();
                    if !send(&mut w, &wire::frame(16, &req(r.0, r.1, r.2))).await {
                        return;
                    }
                    continue;
                }
                if o.out_of_order && st.held.is_none() {
                    st.held = Some(r);
                    continue;
                }
                if !serve_one(&mut w, &t, &o, &log, &mut st, r).await {
                    return;
                }
                answered.fetch_add(1, Ordering::SeqCst);
                if let Some(h) = st.held.take() {
                    if !serve_one(&mut w, &t, &o, &log, &mut st, h).await {
                        return;
                    }
                    answered.fetch_add(1, Ordering::SeqCst);
                }
            }
            8 => {
                let r = (be(&body, 0), be(&body, 4), be(&body, 8));
                if st.outstanding.contains(&r) {
                    answered.fetch_add(1, Ordering::SeqCst);
                }
                st.outstanding.retain(|x| *x != r);
            }
            _ => {}
        }
    }
}

async fn serve_one(
    w: &mut OwnedWriteHalf,
    t: &Torrent,
    o: &Opts,
    log: &Arc<Mutex<Log>>,
    st: &mut State,
    r: (u32, u32, u32),
) -> bool {
    if !o.delay.is_zero() && (o.delay_first == 0 || st.served < o.delay_first) {
        tokio::time::sleep(o.delay).await;
    }
    st.outstanding.retain(|x| *x != r);
    let Some(mut d) = t.block(r.0, r.1, r.2) else {
        return true;
    };
    if let Some(other) = &o.lie {
        let s = (r.0 as u64 * t.piece_len + r.1 as u64) as usize;
        let n = d.len();
        d.copy_from_slice(&other[s..s + n]);
    }
    if !send(w, &piece_msg(r.0, r.1, &d)).await {
        return false;
    }
    st.served += 1;
    {
        let mut l = log.lock().unwrap();
        l.served += 1;
        l.served_bytes += d.len() as u64;
    }
    // Hostile: keep asking the prober for a block and for metadata.
    if o.hostile {
        let mut h = wire::frame(6, &req(r.0, 0, BLOCK));
        h.extend(wire::metadata_request(1, 0));
        if !send(w, &h).await {
            return false;
        }
    }
    if o.rechoke_after.is_some_and(|n| st.served == n) {
        st.choked = true;
        // Without BEP 6 the outstanding requests are dropped with the choke.
        st.outstanding.clear();
        st.unchoke_at = Some(Instant::now() + Duration::from_millis(300));
        if !send(w, &wire::frame(0, &[])).await {
            return false;
        }
    }
    true
}

/// Message ids a prober may send with upload off (keep-alive is 255 here).
pub const ALLOWED_IDS: [u8; 8] = [255, 2, 3, 6, 8, 15, 16, 20];

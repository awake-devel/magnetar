//! One peer of a header probe (D117): a reader task that turns frames into [`PeerMsg`]s on a
//! channel (so the probe's event loop never cancels a half-read frame), the write half behind
//! [`Out`] (only [`ProbeMsg`] and, with upload on, [`ServeMsg`] can be written), and what the
//! probe knows of the peer: choke state, pieces, allowed-fast set, outstanding requests.

use std::collections::{HashSet, VecDeque};
use std::net::SocketAddrV4;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::{OwnedSemaphorePermit, mpsc};
use tokio::task::JoinHandle;
use tokio::time::Instant;

use super::msg::{Block, MAX_FRAME, MsgError, PeerMsg, ProbeMsg};
use super::serve::ServeMsg;
use crate::metainfo::{PeerConn, PeerState, wire};

/// Bytes of messages a probe skips per peer (unknown ids, extensions, keep-alives).
const DISCARD: u64 = 1024 * 1024;
/// A write that takes longer drops the peer (it stopped reading).
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);

/// What the reader task reports.
#[derive(Debug)]
pub(crate) enum Event {
    Msg(PeerMsg),
    /// The connection ended or the peer broke a rule; the reason is a counter code.
    Closed(&'static str),
}

/// Aborts the reader task when the peer is dropped.
struct ReaderGuard(JoinHandle<()>);

impl Drop for ReaderGuard {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// The write half of a probe connection: only whitelisted messages go out.
pub(crate) struct Out {
    wr: OwnedWriteHalf,
    bytes_out: Arc<AtomicU64>,
}

impl Out {
    async fn write(&mut self, b: &[u8]) -> std::io::Result<()> {
        tokio::time::timeout(WRITE_TIMEOUT, self.wr.write_all(b))
            .await
            .map_err(|_| std::io::Error::from(std::io::ErrorKind::TimedOut))??;
        self.bytes_out.fetch_add(b.len() as u64, Ordering::Relaxed);
        Ok(())
    }

    pub(crate) async fn send(&mut self, m: &ProbeMsg) -> std::io::Result<()> {
        self.write(&m.encode()).await
    }

    pub(crate) async fn send_all(&mut self, ms: &[ProbeMsg]) -> std::io::Result<()> {
        let mut b = Vec::new();
        for m in ms {
            b.extend_from_slice(&m.encode());
        }
        if b.is_empty() {
            return Ok(());
        }
        tokio::time::timeout(WRITE_TIMEOUT, self.wr.write_all(&b))
            .await
            .map_err(|_| std::io::Error::from(std::io::ErrorKind::TimedOut))??;
        self.bytes_out.fetch_add(b.len() as u64, Ordering::Relaxed);
        Ok(())
    }

    pub(crate) async fn serve(&mut self, m: &ServeMsg) -> std::io::Result<()> {
        self.write(&m.encode()).await
    }
}

/// Which pieces the peer has.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Avail {
    All,
    Pieces(Vec<bool>),
}

impl Avail {
    pub(crate) fn has(&self, i: u32) -> bool {
        match self {
            Avail::All => true,
            Avail::Pieces(v) => v.get(i as usize).copied().unwrap_or(false),
        }
    }

    fn set(&mut self, i: u32) {
        if let Avail::Pieces(v) = self
            && let Some(b) = v.get_mut(i as usize)
        {
            *b = true;
        }
    }
}

/// Availability from what the peer said during the metadata phase. Err: a bitfield of the
/// wrong length or with spare bits set, or a `have` past the last piece (BEP 3: drop it).
pub(crate) fn avail_from(state: &PeerState, pieces: u32) -> Result<Avail, &'static str> {
    if state.have.iter().any(|&i| i >= pieces) {
        return Err("bad_have");
    }
    if state.have_all {
        return Ok(Avail::All);
    }
    let n = pieces as usize;
    let mut v = vec![false; n];
    if let Some(bf) = &state.bitfield
        && !state.have_none
    {
        v = bitfield(bf, pieces)?;
    }
    let mut a = Avail::Pieces(v);
    for &i in &state.have {
        a.set(i);
    }
    Ok(a)
}

/// Decode a BEP 3 bitfield (high bit first) for `pieces` pieces.
pub(crate) fn bitfield(bf: &[u8], pieces: u32) -> Result<Vec<bool>, &'static str> {
    let n = pieces as usize;
    if bf.len() != n.div_ceil(8) {
        return Err("bad_bitfield");
    }
    let mut v = vec![false; n];
    for (i, x) in v.iter_mut().enumerate() {
        *x = bf[i / 8] & (0x80 >> (i % 8)) != 0;
    }
    // Spare bits must be clear.
    if !n.is_multiple_of(8) && bf[n / 8] & (0xFFu8 >> (n % 8)) != 0 {
        return Err("bad_bitfield");
    }
    Ok(v)
}

/// One peer of a probe.
pub(crate) struct Peer {
    pub(crate) slot: usize,
    pub(crate) addr: SocketAddrV4,
    pub(crate) fast: bool,
    pub(crate) out: Out,
    _reader: ReaderGuard,
    _permit: OwnedSemaphorePermit,
    pub(crate) ut_metadata: Option<u8>,
    /// The peer advertised `ut_pex` (BEP 11): a message may come (D143).
    pub(crate) ut_pex: bool,
    /// `ut_pex` messages received from it, and addresses accepted from them.
    pub(crate) pex_msgs: usize,
    pub(crate) pex_added: usize,
    pub(crate) pipeline: usize,
    /// The peer chokes us.
    pub(crate) choked: bool,
    /// The peer is interested in us.
    pub(crate) interested: bool,
    pub(crate) avail: Avail,
    pub(crate) allowed_fast: HashSet<u32>,
    pub(crate) inflight: VecDeque<(Block, Instant)>,
    /// Requests re-queued on a choke without BEP 6: a late block for one of them is fine.
    pub(crate) late: HashSet<Block>,
    /// Blocks this peer rejected (never asked again from it).
    pub(crate) refused: HashSet<Block>,
    pub(crate) since: Instant,
    pub(crate) last_block: Instant,
    pub(crate) bytes_in: u64,
    /// We unchoked it (upload mode only).
    pub(crate) unchoked_by_us: bool,
    pub(crate) dead: Option<&'static str>,
    pub(crate) rejects_sent: usize,
    pub(crate) metadata_rejects: usize,
    pub(crate) got_any: bool,
}

impl Peer {
    /// Take over a handed-off or fresh connection: start its reader task on `tx`.
    pub(crate) fn start(
        conn: PeerConn,
        slot: usize,
        pieces: u32,
        pipeline: usize,
        tx: mpsc::Sender<(usize, Event)>,
        bytes_in: Arc<AtomicU64>,
        bytes_out: Arc<AtomicU64>,
    ) -> Peer {
        let fast = conn.fast();
        let now = Instant::now();
        let PeerConn {
            addr,
            ext,
            state,
            rd,
            wr,
            _permit,
            ..
        } = conn;
        let (avail, dead) = match avail_from(&state, pieces) {
            Ok(a) => (a, None),
            Err(e) => (Avail::Pieces(vec![false; pieces as usize]), Some(e)),
        };
        let reader = tokio::spawn(read_loop(rd, slot, tx, bytes_in));
        Peer {
            slot,
            addr,
            fast,
            out: Out { wr, bytes_out },
            _reader: ReaderGuard(reader),
            _permit,
            ut_metadata: ext.ut_metadata,
            ut_pex: ext.ut_pex.is_some(),
            pex_msgs: 0,
            pex_added: 0,
            pipeline: ext
                .reqq
                .map_or(pipeline, |q| (q as usize).min(pipeline))
                .max(1),
            choked: state.choked,
            interested: state.interested,
            avail,
            allowed_fast: state
                .allowed_fast
                .iter()
                .copied()
                .filter(|&i| i < pieces)
                .collect(),
            inflight: VecDeque::new(),
            late: HashSet::new(),
            refused: HashSet::new(),
            since: now,
            last_block: now,
            bytes_in: 0,
            unchoked_by_us: false,
            dead,
            rejects_sent: 0,
            metadata_rejects: 0,
            got_any: false,
        }
    }

    pub(crate) fn alive(&self) -> bool {
        self.dead.is_none()
    }

    /// The peer may send us this piece now (unchoked, or an allowed-fast piece).
    pub(crate) fn can_request(&self, b: &Block) -> bool {
        self.alive()
            && self.avail.has(b.index)
            && !self.refused.contains(b)
            && (!self.choked || (self.fast && self.allowed_fast.contains(&b.index)))
    }

    pub(crate) fn has_room(&self) -> bool {
        self.inflight.len() < self.pipeline
    }

    pub(crate) fn on_have(&mut self, i: u32, pieces: u32) -> Result<(), &'static str> {
        if i >= pieces {
            return Err("bad_have");
        }
        self.avail.set(i);
        Ok(())
    }
}

async fn read_loop(
    mut rd: BufReader<OwnedReadHalf>,
    slot: usize,
    tx: mpsc::Sender<(usize, Event)>,
    bytes_in: Arc<AtomicU64>,
) {
    let mut discard = DISCARD;
    let reason = loop {
        let len = match rd.read_u32().await {
            Ok(l) => l as usize,
            Err(_) => break "closed",
        };
        bytes_in.fetch_add(4, Ordering::Relaxed);
        if len == 0 {
            continue;
        }
        if len > MAX_FRAME {
            break "oversize";
        }
        let mut body = vec![0u8; len];
        if rd.read_exact(&mut body).await.is_err() {
            break "closed";
        }
        bytes_in.fetch_add(len as u64, Ordering::Relaxed);
        let id = body.remove(0);
        // `piece` messages are checked against what was requested; everything else is
        // charged to the per-peer budget.
        if id != wire::MSG_PIECE {
            match discard.checked_sub(len as u64) {
                Some(d) => discard = d,
                None => break "flood",
            }
        }
        let m = match PeerMsg::parse(id, body) {
            Ok(PeerMsg::Unknown(_) | PeerMsg::KeepAlive | PeerMsg::Port(_)) => continue,
            Ok(m) => m,
            Err(MsgError::TooLarge) => break "oversize",
            Err(MsgError::Malformed) => break "malformed",
        };
        if tx.send((slot, Event::Msg(m))).await.is_err() {
            return;
        }
    };
    let _ = tx.send((slot, Event::Closed(reason))).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bitfields_are_checked() {
        assert_eq!(bitfield(&[0b1010_0000], 3), Ok(vec![true, false, true]));
        assert_eq!(
            bitfield(&[0b1010_0001], 3),
            Err("bad_bitfield"),
            "spare bit"
        );
        assert_eq!(bitfield(&[0, 0], 3), Err("bad_bitfield"), "length");
        assert_eq!(bitfield(&[0xFF], 8), Ok(vec![true; 8]));
        let mut st = PeerState::default();
        st.have.push(2);
        assert_eq!(
            avail_from(&st, 3),
            Ok(Avail::Pieces(vec![false, false, true]))
        );
        st.have.push(3);
        assert_eq!(avail_from(&st, 3), Err("bad_have"));
        st.have_all = true;
        assert_eq!(
            avail_from(&st, 3),
            Err("bad_have"),
            "checked with have_all too"
        );
        st.have.pop();
        assert_eq!(avail_from(&st, 3), Ok(Avail::All));
    }
}

//! Upload mode of header probes (D117, `MAGNETAR_PROBE_UPLOAD=true`, off by default). Turning
//! it on **is** sharing: the probe announces (`HAVE`) and serves pieces to the peers of the
//! same probe.
//!
//! What it may share is fixed by types:
//! - [`UploadGate`] exists only when the setting is on ([`UploadGate::new`] is `None` otherwise);
//! - [`HeldPiece`] only comes from [`UploadGate::hold`], which takes a [`VerifiedPiece`] (a whole
//!   piece whose SHA-1 matched the info dict); bytes fetched as unverified blocks never become one;
//! - [`ServeMsg`] (`unchoke`, `choke`, `have`, `piece`) can only be built from the gate and a
//!   held piece, and a `piece` only inside the held piece's bounds.
//!
//! Never a bitfield or `have_all`, never an inbound listener, nothing written to disk; held
//! buffers are zeroed on drop and live only as long as the probe. Caps per probe: bytes served
//! ([`UPLOAD_MAX_BYTES`]) and rate ([`UPLOAD_RATE`]); at the byte cap every peer is choked.

use std::time::Duration;

use tokio::time::Instant;

use super::msg::BLOCK;
use super::verify::VerifiedPiece;
use crate::metainfo::wire;

/// Bytes one probe may serve (decision 6 of the plan: a constant until the eee trial).
pub const UPLOAD_MAX_BYTES: u64 = 4 * 1024 * 1024;
/// Upload rate of one probe, bytes per second.
pub const UPLOAD_RATE: u64 = 256 * 1024;

/// A verified piece the probe may serve.
pub struct HeldPiece {
    index: u32,
    bytes: Vec<u8>,
}

impl HeldPiece {
    pub fn index(&self) -> u32 {
        self.index
    }

    pub fn len(&self) -> u32 {
        self.bytes.len() as u32
    }

    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }
}

impl Drop for HeldPiece {
    fn drop(&mut self) {
        self.bytes.fill(0);
    }
}

/// What [`UploadGate::take`] allows now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Take {
    Ok,
    /// Over the rate: try again after this long.
    Wait(Duration),
    /// The byte cap is reached: choke everyone.
    Capped,
}

/// The upload state of one probe. Exists only with upload on.
pub struct UploadGate {
    max_bytes: u64,
    rate: u64,
    served: u64,
    /// Token bucket (bytes), at most one second of rate (and at least one block).
    tokens: f64,
    last: Instant,
    held: Vec<HeldPiece>,
}

impl UploadGate {
    /// The gate, only when `upload` is on.
    pub fn new(upload: bool, max_bytes: u64, rate: u64) -> Option<UploadGate> {
        upload.then(|| UploadGate {
            max_bytes,
            rate: rate.max(1),
            served: 0,
            tokens: rate.max(u64::from(BLOCK)) as f64,
            last: Instant::now(),
            held: Vec::new(),
        })
    }

    /// Keep a verified piece for serving (its bytes move into the gate).
    pub fn hold(&mut self, mut p: VerifiedPiece) -> &HeldPiece {
        let index = p.index();
        if let Some(i) = self.held.iter().position(|h| h.index == index) {
            return &self.held[i];
        }
        self.held.push(HeldPiece {
            index,
            bytes: p.take_bytes(),
        });
        self.held.last().expect("just pushed")
    }

    pub fn held(&self, index: u32) -> Option<&HeldPiece> {
        self.held.iter().find(|h| h.index == index)
    }

    pub fn held_pieces(&self) -> impl Iterator<Item = &HeldPiece> {
        self.held.iter()
    }

    pub fn served(&self) -> u64 {
        self.served
    }

    pub fn capped(&self) -> bool {
        self.served >= self.max_bytes
    }

    /// Spend `len` bytes of the caps, or say why not.
    pub fn take(&mut self, len: u32, now: Instant) -> Take {
        let len = u64::from(len);
        if self.served + len > self.max_bytes {
            return Take::Capped;
        }
        let cap = self.rate.max(u64::from(BLOCK)) as f64;
        let dt = now.saturating_duration_since(self.last).as_secs_f64();
        self.last = now;
        self.tokens = (self.tokens + dt * self.rate as f64).min(cap);
        if self.tokens < len as f64 {
            let need = (len as f64 - self.tokens) / self.rate as f64;
            return Take::Wait(Duration::from_secs_f64(need.max(0.001)));
        }
        self.tokens -= len as f64;
        self.served += len;
        Take::Ok
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Kind {
    Unchoke,
    Choke,
    Have(u32),
    Piece {
        index: u32,
        begin: u32,
        data: Vec<u8>,
    },
}

/// A message of the upload mode. Only buildable from the [`UploadGate`] and a [`HeldPiece`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServeMsg(Kind);

impl ServeMsg {
    pub fn unchoke(_gate: &UploadGate) -> ServeMsg {
        ServeMsg(Kind::Unchoke)
    }

    pub fn choke(_gate: &UploadGate) -> ServeMsg {
        ServeMsg(Kind::Choke)
    }

    pub fn have(p: &HeldPiece) -> ServeMsg {
        ServeMsg(Kind::Have(p.index))
    }

    /// `len` bytes at `begin` of the held piece; None outside it or above 16 KiB.
    pub fn piece(p: &HeldPiece, begin: u32, len: u32) -> Option<ServeMsg> {
        if len == 0 || len > BLOCK {
            return None;
        }
        let end = begin.checked_add(len)?;
        let data = p.bytes.get(begin as usize..end as usize)?.to_vec();
        Some(ServeMsg(Kind::Piece {
            index: p.index,
            begin,
            data,
        }))
    }

    /// The message id on the wire.
    pub fn id(&self) -> u8 {
        match self.0 {
            Kind::Unchoke => wire::MSG_UNCHOKE,
            Kind::Choke => wire::MSG_CHOKE,
            Kind::Have(_) => wire::MSG_HAVE,
            Kind::Piece { .. } => wire::MSG_PIECE,
        }
    }

    /// Payload bytes of a `piece` (0 for the others).
    pub fn data_len(&self) -> u32 {
        match &self.0 {
            Kind::Piece { data, .. } => data.len() as u32,
            _ => 0,
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        match &self.0 {
            Kind::Unchoke => wire::frame(wire::MSG_UNCHOKE, &[]),
            Kind::Choke => wire::frame(wire::MSG_CHOKE, &[]),
            Kind::Have(i) => wire::frame(wire::MSG_HAVE, &i.to_be_bytes()),
            Kind::Piece { index, begin, data } => {
                let mut b = Vec::with_capacity(8 + data.len());
                b.extend_from_slice(&index.to_be_bytes());
                b.extend_from_slice(&begin.to_be_bytes());
                b.extend_from_slice(data);
                wire::frame(wire::MSG_PIECE, &b)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::probe::verify::PieceAssembler;

    fn verified(index: u32, len: usize) -> VerifiedPiece {
        let data: Vec<u8> = (0..len).map(|i| i as u8).collect();
        let h = sha1_smol::Sha1::from(&data).digest().bytes();
        let mut a = PieceAssembler::new(index, len as u32, h);
        for (i, c) in data.chunks(BLOCK as usize).enumerate() {
            a.insert(i as u32 * BLOCK, c.to_vec(), 0).unwrap();
        }
        a.finish().unwrap()
    }

    #[test]
    fn gate_only_with_upload_on() {
        assert!(UploadGate::new(false, 1 << 20, 1 << 20).is_none());
        assert!(UploadGate::new(true, 1 << 20, 1 << 20).is_some());
    }

    #[test]
    fn pieces_stay_inside_held_bounds() {
        let mut g = UploadGate::new(true, 1 << 20, 1 << 20).unwrap();
        let h = g.hold(verified(2, 20_000));
        assert_eq!(ServeMsg::have(h).encode(), vec![0, 0, 0, 5, 4, 0, 0, 0, 2]);
        assert!(ServeMsg::piece(h, 0, BLOCK).is_some());
        assert!(ServeMsg::piece(h, BLOCK, 20_000 - BLOCK).is_some());
        assert!(ServeMsg::piece(h, BLOCK, BLOCK).is_none(), "past the piece");
        assert!(ServeMsg::piece(h, 0, BLOCK + 1).is_none());
        assert!(ServeMsg::piece(h, u32::MAX, 2).is_none());
        let m = ServeMsg::piece(h, 16, 4).unwrap();
        assert_eq!(m.encode(), {
            let mut v = vec![0, 0, 0, 13, 7, 0, 0, 0, 2, 0, 0, 0, 16];
            v.extend_from_slice(&[16, 17, 18, 19]);
            v
        });
        assert!(g.held(3).is_none());
    }

    #[test]
    fn caps_are_kept() {
        let mut g = UploadGate::new(true, 40_000, 20_000).unwrap();
        let t = Instant::now();
        assert_eq!(g.take(BLOCK, t), Take::Ok);
        assert!(matches!(g.take(BLOCK, t), Take::Wait(_)));
        assert_eq!(g.take(BLOCK, t + Duration::from_secs(1)), Take::Ok);
        assert_eq!(g.take(BLOCK, t + Duration::from_secs(5)), Take::Capped);
        assert_eq!(g.served(), 2 * u64::from(BLOCK));
        assert!(!g.capped());
    }
}

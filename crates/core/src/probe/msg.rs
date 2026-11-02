//! Peer wire messages of a header probe (D117): [`PeerMsg::parse`] for what a peer sends, and
//! [`ProbeMsg`], the only messages a probe can send while upload is off.
//!
//! [`ProbeMsg`] has no variant for `unchoke`, `have`, `bitfield`, `have_all`, `piece` or
//! `ut_metadata` data, so a probe cannot build one. The upload mode's messages are
//! [`super::serve::ServeMsg`], which can only be built from a held, hash-verified piece and
//! the upload gate that exists only with `MAGNETAR_PROBE_UPLOAD=true`.
//!
//! Everything parsed here comes from an untrusted peer: sizes are checked before use and
//! nothing panics on any input (property test below).

use crate::liveness::bencode::{self, Value};
use crate::metainfo::fetch::MAX_BITFIELD;
use crate::metainfo::wire;

/// The 16 KiB request size every client accepts (BEP 3 practice; BEP 52 fixes it for v2).
pub const BLOCK: u32 = 16 * 1024;
/// Largest `extended` payload read (as the metadata fetcher, `fetch.rs` `MAX_EXTENDED`).
pub const MAX_EXTENDED: usize = 64 * 1024;
/// Largest frame body read from a peer: a bitfield, an extended message, or a 16 KiB block.
pub const MAX_FRAME: usize = 1 + MAX_BITFIELD;

/// One request: `len` bytes at `begin` inside piece `index`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Block {
    pub index: u32,
    pub begin: u32,
    pub len: u32,
}

impl Block {
    fn payload(&self) -> [u8; 12] {
        let mut b = [0u8; 12];
        b[..4].copy_from_slice(&self.index.to_be_bytes());
        b[4..8].copy_from_slice(&self.begin.to_be_bytes());
        b[8..].copy_from_slice(&self.len.to_be_bytes());
        b
    }

    fn parse(body: &[u8]) -> Result<Block, MsgError> {
        if body.len() != 12 {
            return Err(MsgError::Malformed);
        }
        Ok(Block {
            index: be32(&body[..4])?,
            begin: be32(&body[4..8])?,
            len: be32(&body[8..12])?,
        })
    }
}

fn be32(b: &[u8]) -> Result<u32, MsgError> {
    let a: [u8; 4] = b.try_into().map_err(|_| MsgError::Malformed)?;
    Ok(u32::from_be_bytes(a))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MsgError {
    /// A message whose size breaks its BEP (a 5-byte `have`, a 13-byte `request`, ...).
    Malformed,
    /// A `piece` carrying more than [`BLOCK`] bytes, or a frame over [`MAX_FRAME`].
    TooLarge,
}

/// A message received from a peer (BEP 3, BEP 6, BEP 10).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PeerMsg {
    KeepAlive,
    Choke,
    Unchoke,
    Interested,
    NotInterested,
    Have(u32),
    Bitfield(Vec<u8>),
    Request(Block),
    Piece {
        index: u32,
        begin: u32,
        data: Vec<u8>,
    },
    Cancel(Block),
    Port(u16),
    Suggest(u32),
    HaveAll,
    HaveNone,
    Reject(Block),
    AllowedFast(u32),
    /// BEP 10: the extended id, then its payload.
    Extended {
        id: u8,
        payload: Vec<u8>,
    },
    /// An id we do not know (BEP 3: ignored).
    Unknown(u8),
}

impl PeerMsg {
    /// Parse the body of one frame (`id` and what follows it).
    pub fn parse(id: u8, mut body: Vec<u8>) -> Result<PeerMsg, MsgError> {
        if body.len() + 1 > MAX_FRAME {
            return Err(MsgError::TooLarge);
        }
        let empty = |m: PeerMsg| {
            if body.is_empty() {
                Ok(m)
            } else {
                Err(MsgError::Malformed)
            }
        };
        match id {
            wire::MSG_CHOKE => empty(PeerMsg::Choke),
            wire::MSG_UNCHOKE => empty(PeerMsg::Unchoke),
            wire::MSG_INTERESTED => empty(PeerMsg::Interested),
            wire::MSG_NOT_INTERESTED => empty(PeerMsg::NotInterested),
            wire::MSG_HAVE_ALL => empty(PeerMsg::HaveAll),
            wire::MSG_HAVE_NONE => empty(PeerMsg::HaveNone),
            wire::MSG_HAVE => Ok(PeerMsg::Have(be32(&body)?)),
            wire::MSG_SUGGEST => Ok(PeerMsg::Suggest(be32(&body)?)),
            wire::MSG_ALLOWED_FAST => Ok(PeerMsg::AllowedFast(be32(&body)?)),
            wire::MSG_BITFIELD => Ok(PeerMsg::Bitfield(body)),
            wire::MSG_PIECE_REQUEST => Ok(PeerMsg::Request(Block::parse(&body)?)),
            wire::MSG_CANCEL => Ok(PeerMsg::Cancel(Block::parse(&body)?)),
            wire::MSG_REJECT_REQUEST => Ok(PeerMsg::Reject(Block::parse(&body)?)),
            wire::MSG_PORT => {
                let b: [u8; 2] = body
                    .as_slice()
                    .try_into()
                    .map_err(|_| MsgError::Malformed)?;
                Ok(PeerMsg::Port(u16::from_be_bytes(b)))
            }
            wire::MSG_PIECE => {
                if body.len() < 8 {
                    return Err(MsgError::Malformed);
                }
                if body.len() - 8 > BLOCK as usize {
                    return Err(MsgError::TooLarge);
                }
                let index = be32(&body[..4])?;
                let begin = be32(&body[4..8])?;
                body.drain(..8);
                Ok(PeerMsg::Piece {
                    index,
                    begin,
                    data: body,
                })
            }
            wire::MSG_EXTENDED => {
                if body.is_empty() || body.len() > MAX_EXTENDED {
                    return Err(MsgError::Malformed);
                }
                let id = body.remove(0);
                Ok(PeerMsg::Extended { id, payload: body })
            }
            other => Ok(PeerMsg::Unknown(other)),
        }
    }
}

/// Every message a probe may send while upload is off (wire report, D117). There is no
/// variant for `unchoke`, `have`, `bitfield`, `have_all`, `piece` or `ut_metadata` data.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProbeMsg {
    KeepAlive,
    Interested,
    NotInterested,
    Request(Block),
    Cancel(Block),
    /// BEP 6: the first message to a peer that set the Fast bit too (sent by the fetcher).
    HaveNone,
    /// BEP 6: the answer to a peer's `request` (fast peers only).
    RejectRequest(Block),
    /// Our BEP 10 handshake (`m.ut_metadata` and `v` only).
    ExtHandshake {
        client: String,
    },
    /// BEP 9 `reject` for a peer's metadata request, sent with the peer's `ut_metadata` id.
    MetadataReject {
        ut_metadata: u8,
        piece: i64,
    },
}

impl ProbeMsg {
    /// The frame on the wire.
    pub fn encode(&self) -> Vec<u8> {
        match self {
            ProbeMsg::KeepAlive => vec![0, 0, 0, 0],
            ProbeMsg::Interested => wire::frame(wire::MSG_INTERESTED, &[]),
            ProbeMsg::NotInterested => wire::frame(wire::MSG_NOT_INTERESTED, &[]),
            ProbeMsg::Request(b) => wire::frame(wire::MSG_PIECE_REQUEST, &b.payload()),
            ProbeMsg::Cancel(b) => wire::frame(wire::MSG_CANCEL, &b.payload()),
            ProbeMsg::HaveNone => wire::frame(wire::MSG_HAVE_NONE, &[]),
            ProbeMsg::RejectRequest(b) => wire::frame(wire::MSG_REJECT_REQUEST, &b.payload()),
            ProbeMsg::ExtHandshake { client } => wire::our_extended_handshake(client),
            ProbeMsg::MetadataReject { ut_metadata, piece } => {
                wire::metadata_reject(*ut_metadata, *piece)
            }
        }
    }
}

/// A BEP 9 metadata request from a peer (`Some(piece)`), parsed from an extended payload sent
/// to our `ut_metadata` id.
pub fn metadata_request(payload: &[u8]) -> Option<i64> {
    let (v, _) = bencode::decode_prefix(payload).ok()?;
    if v.get(b"msg_type").and_then(Value::as_int) != Some(wire::MSG_REQUEST) {
        return None;
    }
    v.get(b"piece").and_then(Value::as_int)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn known_messages_round_trip() {
        let b = Block {
            index: 3,
            begin: 16384,
            len: 16384,
        };
        let r = ProbeMsg::Request(b).encode();
        assert_eq!(&r[..5], &[0, 0, 0, 13, 6]);
        assert_eq!(PeerMsg::parse(6, r[5..].to_vec()), Ok(PeerMsg::Request(b)));
        assert_eq!(
            PeerMsg::parse(16, ProbeMsg::RejectRequest(b).encode()[5..].to_vec()),
            Ok(PeerMsg::Reject(b))
        );
        assert_eq!(ProbeMsg::HaveNone.encode(), vec![0, 0, 0, 1, 15]);
        assert_eq!(ProbeMsg::KeepAlive.encode(), vec![0, 0, 0, 0]);
        let mut piece = vec![0, 0, 0, 1, 0, 0, 0, 0];
        piece.extend_from_slice(b"abc");
        assert_eq!(
            PeerMsg::parse(7, piece),
            Ok(PeerMsg::Piece {
                index: 1,
                begin: 0,
                data: b"abc".to_vec()
            })
        );
        assert_eq!(
            PeerMsg::parse(7, vec![0; 8 + 16385]),
            Err(MsgError::TooLarge)
        );
        assert_eq!(PeerMsg::parse(4, vec![0; 5]), Err(MsgError::Malformed));
        assert_eq!(PeerMsg::parse(1, vec![0]), Err(MsgError::Malformed));
        assert_eq!(PeerMsg::parse(99, vec![1, 2]), Ok(PeerMsg::Unknown(99)));
        assert_eq!(
            metadata_request(b"d8:msg_typei0e5:piecei2ee"),
            Some(2),
            "BEP 9 request"
        );
        assert_eq!(metadata_request(b"d8:msg_typei1e5:piecei2ee"), None);
    }

    proptest! {
        #[test]
        fn arbitrary_frames_never_panic(id in any::<u8>(), body in proptest::collection::vec(any::<u8>(), 0..300)) {
            let _ = PeerMsg::parse(id, body.clone());
            let _ = metadata_request(&body);
        }

        #[test]
        fn large_frames_are_bounded(id in any::<u8>(), extra in 0usize..64) {
            let body = vec![0u8; MAX_FRAME + extra];
            prop_assert_eq!(PeerMsg::parse(id, body), Err(MsgError::TooLarge));
        }
    }
}

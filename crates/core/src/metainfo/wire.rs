//! Peer wire messages the metadata fetcher needs (docs/DHT.md §4, D56), written from the BEP
//! texts: the BEP 3 handshake, length-prefixed messages, the BEP 10 extended handshake and the
//! BEP 9 `ut_metadata` messages. Pure functions over byte slices; `fetch.rs` does the I/O.
//!
//! Everything parsed here comes from an untrusted peer: lengths are checked before use and no
//! function panics on any input (property tests below).

use crate::liveness::bencode::{self, Value};

/// BEP 3: "character ninteen (decimal) followed by the string 'BitTorrent protocol'".
pub const PROTOCOL: &[u8; 19] = b"BitTorrent protocol";
pub const HANDSHAKE_LEN: usize = 1 + 19 + 8 + 20 + 20;
/// BEP 10: "bit 20 from the right … (reserved_byte[5] & 0x10)".
pub const EXT_BYTE: usize = 5;
pub const EXT_BIT: u8 = 0x10;
/// BEP 10: the `extended` message id.
pub const MSG_EXTENDED: u8 = 20;
/// BEP 10: extended message id 0 is the extended handshake.
pub const EXT_HANDSHAKE: u8 = 0;
/// The id we ask peers to use when they send `ut_metadata` messages to us (BEP 10: "The IDs are
/// local to this particular peer", i.e. the receiver chooses).
pub const OUR_UT_METADATA: u8 = 1;
/// BEP 9: "The metadata is handled in blocks of 16KiB (16384 Bytes)".
pub const BLOCK: usize = 16 * 1024;

/// BEP 9 message types.
pub const MSG_REQUEST: i64 = 0;
pub const MSG_DATA: i64 = 1;
pub const MSG_REJECT: i64 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WireError {
    /// Not a BitTorrent handshake.
    Protocol,
    /// The handshake carried another infohash.
    InfoHash,
    /// The peer does not set the BEP 10 extension bit.
    NoExtensions,
    /// A message that does not parse or breaks a rule of the BEPs.
    Malformed,
}

/// Our BEP 3 handshake: pstrlen, pstr, reserved (only the BEP 10 bit), infohash, peer id.
pub fn handshake(info_hash: &[u8; 20], peer_id: &[u8; 20]) -> [u8; HANDSHAKE_LEN] {
    let mut out = [0u8; HANDSHAKE_LEN];
    out[0] = 19;
    out[1..20].copy_from_slice(PROTOCOL);
    out[20 + EXT_BYTE] = EXT_BIT;
    out[28..48].copy_from_slice(info_hash);
    out[48..68].copy_from_slice(peer_id);
    out
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PeerHandshake {
    pub reserved: [u8; 8],
    pub info_hash: [u8; 20],
    pub peer_id: [u8; 20],
}

/// Check the peer's handshake against the infohash we asked for. The peer id is not checked:
/// peers from the DHT come without one (BEP 3's check applies to tracker peer lists).
pub fn parse_handshake(
    b: &[u8; HANDSHAKE_LEN],
    want: &[u8; 20],
) -> Result<PeerHandshake, WireError> {
    if b[0] != 19 || &b[1..20] != PROTOCOL {
        return Err(WireError::Protocol);
    }
    let mut hs = PeerHandshake {
        reserved: [0; 8],
        info_hash: [0; 20],
        peer_id: [0; 20],
    };
    hs.reserved.copy_from_slice(&b[20..28]);
    hs.info_hash.copy_from_slice(&b[28..48]);
    hs.peer_id.copy_from_slice(&b[48..68]);
    if &hs.info_hash != want {
        return Err(WireError::InfoHash);
    }
    if hs.reserved[EXT_BYTE] & EXT_BIT == 0 {
        return Err(WireError::NoExtensions);
    }
    Ok(hs)
}

/// A length-prefixed message: 4-byte big-endian length, then `id` and `payload`.
pub fn frame(id: u8, payload: &[u8]) -> Vec<u8> {
    let len = u32::try_from(payload.len() + 1).expect("our own messages are small");
    let mut out = Vec::with_capacity(4 + 1 + payload.len());
    out.extend_from_slice(&len.to_be_bytes());
    out.push(id);
    out.extend_from_slice(payload);
    out
}

/// An `extended` message (BEP 10): message id 20, then the extended id, then the payload.
pub fn extended(ext_id: u8, payload: &[u8]) -> Vec<u8> {
    let mut body = Vec::with_capacity(1 + payload.len());
    body.push(ext_id);
    body.extend_from_slice(payload);
    frame(MSG_EXTENDED, &body)
}

/// Our extended handshake: `m.ut_metadata` = [`OUR_UT_METADATA`] and `v`. No `metadata_size`
/// (BEP 9 adds it only when the sender has the metadata, and we have none).
pub fn our_extended_handshake(client: &str) -> Vec<u8> {
    let dict = Value::Dict(vec![
        (
            b"m".to_vec(),
            Value::Dict(vec![(
                b"ut_metadata".to_vec(),
                Value::Int(i64::from(OUR_UT_METADATA)),
            )]),
        ),
        (b"v".to_vec(), Value::Bytes(client.as_bytes().to_vec())),
    ]);
    extended(EXT_HANDSHAKE, &bencode::encode(&dict))
}

/// What we need from the peer's extended handshake.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ExtHandshake {
    /// The id to send `ut_metadata` messages to this peer with (None: absent or 0 = disabled).
    pub ut_metadata: Option<u8>,
    /// BEP 9 `metadata_size` (any integer as sent; the caller checks the range).
    pub metadata_size: Option<i64>,
}

/// Parse the payload of an extended handshake (after the extended id byte). Unknown keys are
/// ignored (BEP 10: "Any unknown names should be ignored").
pub fn parse_ext_handshake(payload: &[u8]) -> Result<ExtHandshake, WireError> {
    let v = bencode::decode(payload).map_err(|_| WireError::Malformed)?;
    if v.as_dict().is_none() {
        return Err(WireError::Malformed);
    }
    let ut_metadata = v
        .get(b"m")
        .and_then(|m| m.get(b"ut_metadata"))
        .and_then(Value::as_int)
        .and_then(|i| u8::try_from(i).ok())
        .filter(|&i| i != 0);
    let metadata_size = v.get(b"metadata_size").and_then(Value::as_int);
    Ok(ExtHandshake {
        ut_metadata,
        metadata_size,
    })
}

/// A BEP 9 `request` for `piece`, sent with the peer's own `ut_metadata` id.
pub fn metadata_request(peer_ut_metadata: u8, piece: u32) -> Vec<u8> {
    let dict = Value::Dict(vec![
        (b"msg_type".to_vec(), Value::Int(MSG_REQUEST)),
        (b"piece".to_vec(), Value::Int(i64::from(piece))),
    ]);
    extended(peer_ut_metadata, &bencode::encode(&dict))
}

/// A BEP 9 `reject` for `piece` ("does not have any additional keys").
pub fn metadata_reject(peer_ut_metadata: u8, piece: i64) -> Vec<u8> {
    let dict = Value::Dict(vec![
        (b"msg_type".to_vec(), Value::Int(MSG_REJECT)),
        (b"piece".to_vec(), Value::Int(piece)),
    ]);
    extended(peer_ut_metadata, &bencode::encode(&dict))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MetadataMsg<'a> {
    Request {
        piece: i64,
    },
    /// `data`: the block follows the bencoded dict in the same message.
    Data {
        piece: i64,
        total_size: Option<i64>,
        block: &'a [u8],
    },
    Reject {
        piece: i64,
    },
    /// BEP 9: "an unrecognized message ID MUST be ignored".
    Unknown,
}

/// Parse a `ut_metadata` payload (after the extended id byte).
pub fn parse_metadata_msg(payload: &[u8]) -> Result<MetadataMsg<'_>, WireError> {
    let (v, used) = bencode::decode_prefix(payload).map_err(|_| WireError::Malformed)?;
    if v.as_dict().is_none() {
        return Err(WireError::Malformed);
    }
    let msg_type = v
        .get(b"msg_type")
        .and_then(Value::as_int)
        .ok_or(WireError::Malformed)?;
    let piece = v.get(b"piece").and_then(Value::as_int);
    let rest = payload.get(used..).unwrap_or_default();
    Ok(match msg_type {
        MSG_REQUEST => MetadataMsg::Request {
            piece: piece.ok_or(WireError::Malformed)?,
        },
        MSG_DATA => MetadataMsg::Data {
            piece: piece.ok_or(WireError::Malformed)?,
            total_size: v.get(b"total_size").and_then(Value::as_int),
            block: rest,
        },
        MSG_REJECT => MetadataMsg::Reject {
            piece: piece.ok_or(WireError::Malformed)?,
        },
        _ => MetadataMsg::Unknown,
    })
}

/// Number of 16 KiB blocks for `size` bytes of metadata.
pub fn block_count(size: usize) -> usize {
    size.div_ceil(BLOCK)
}

/// Expected length of block `piece` of `size` bytes of metadata (BEP 9: every block but the
/// last is exactly 16 KiB), None when `piece` is out of range.
pub fn block_len(size: usize, piece: usize) -> Option<usize> {
    let start = piece.checked_mul(BLOCK)?;
    if start >= size {
        return None;
    }
    Some((size - start).min(BLOCK))
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn handshake_layout() {
        let h = handshake(&[7; 20], &[9; 20]);
        assert_eq!(h.len(), 68);
        assert_eq!(h[0], 19);
        assert_eq!(&h[1..20], b"BitTorrent protocol");
        assert_eq!(&h[20..28], &[0, 0, 0, 0, 0, 0x10, 0, 0]);
        assert_eq!(&h[28..48], &[7; 20]);
        assert_eq!(&h[48..68], &[9; 20]);
        assert!(parse_handshake(&h, &[7; 20]).is_ok());
        assert_eq!(parse_handshake(&h, &[8; 20]), Err(WireError::InfoHash));
        let mut no_ext = h;
        no_ext[25] = 0;
        assert_eq!(
            parse_handshake(&no_ext, &[7; 20]),
            Err(WireError::NoExtensions)
        );
        let mut bad = h;
        bad[3] = b'X';
        assert_eq!(parse_handshake(&bad, &[7; 20]), Err(WireError::Protocol));
    }

    #[test]
    fn extended_handshake_round_trip() {
        let ours = our_extended_handshake("Magnetar 0.2.0");
        // length prefix, id 20, ext id 0, then the dict.
        assert_eq!(&ours[4..6], &[20, 0]);
        assert_eq!(
            &ours[6..],
            b"d1:md11:ut_metadatai1ee1:v14:Magnetar 0.2.0e".as_slice()
        );
        let len = u32::from_be_bytes(ours[..4].try_into().unwrap()) as usize;
        assert_eq!(len, ours.len() - 4);
        // The BEP 9 example handshake.
        let hs = parse_ext_handshake(b"d1:md11:ut_metadatai3ee13:metadata_sizei31235ee").unwrap();
        assert_eq!(hs.ut_metadata, Some(3));
        assert_eq!(hs.metadata_size, Some(31235));
        // 0 = disabled (BEP 10), out-of-range ids are not a u8.
        let off = parse_ext_handshake(b"d1:md11:ut_metadatai0eee").unwrap();
        assert_eq!(off.ut_metadata, None);
        let big = parse_ext_handshake(b"d1:md11:ut_metadatai300eee").unwrap();
        assert_eq!(big.ut_metadata, None);
        assert_eq!(parse_ext_handshake(b"le"), Err(WireError::Malformed));
    }

    #[test]
    fn metadata_messages() {
        let r = metadata_request(3, 0);
        // The BEP 9 example request: d8:msg_typei0e5:piecei0ee
        assert_eq!(&r[4..6], &[20, 3]);
        assert_eq!(&r[6..], b"d8:msg_typei0e5:piecei0ee".as_slice());
        let mut data = b"d8:msg_typei1e5:piecei0e10:total_sizei3425ee".to_vec();
        data.extend_from_slice(b"xxxx");
        assert_eq!(
            parse_metadata_msg(&data).unwrap(),
            MetadataMsg::Data {
                piece: 0,
                total_size: Some(3425),
                block: b"xxxx"
            }
        );
        assert_eq!(
            parse_metadata_msg(b"d8:msg_typei2e5:piecei4ee").unwrap(),
            MetadataMsg::Reject { piece: 4 }
        );
        assert_eq!(
            parse_metadata_msg(b"d8:msg_typei9e5:piecei4ee").unwrap(),
            MetadataMsg::Unknown
        );
        assert_eq!(
            parse_metadata_msg(b"d5:piecei4ee"),
            Err(WireError::Malformed)
        );
    }

    #[test]
    fn blocks() {
        assert_eq!(block_count(1), 1);
        assert_eq!(block_count(BLOCK), 1);
        assert_eq!(block_count(BLOCK + 1), 2);
        assert_eq!(block_len(BLOCK + 1, 0), Some(BLOCK));
        assert_eq!(block_len(BLOCK + 1, 1), Some(1));
        assert_eq!(block_len(BLOCK + 1, 2), None);
        assert_eq!(block_len(10, usize::MAX), None);
    }

    proptest! {
        #[test]
        fn arbitrary_bytes_never_panic(b in proptest::collection::vec(any::<u8>(), 0..512)) {
            let _ = parse_ext_handshake(&b);
            let _ = parse_metadata_msg(&b);
            let mut hs = [0u8; HANDSHAKE_LEN];
            for (d, s) in hs.iter_mut().zip(b.iter()) {
                *d = *s;
            }
            let _ = parse_handshake(&hs, &[0; 20]);
        }

        #[test]
        fn mutated_data_messages_never_panic(
            piece in any::<i64>(), total in any::<i64>(), cut in 0usize..64, flip in any::<u8>()
        ) {
            let mut m = format!("d8:msg_typei1e5:piecei{piece}e10:total_sizei{total}ee").into_bytes();
            m.extend_from_slice(&[0xAB; 32]);
            if cut < m.len() {
                m[cut] ^= flip;
                m.truncate(m.len() - cut / 2);
            }
            let _ = parse_metadata_msg(&m);
        }
    }
}

//! Peer wire messages the metadata fetcher needs (docs/DHT.md §4, D56), written from the BEP
//! texts: the BEP 3 handshake, length-prefixed messages, the BEP 10 extended handshake and the
//! BEP 9 `ut_metadata` messages. Pure functions over byte slices; `fetch.rs` does the I/O.
//!
//! Everything parsed here comes from an untrusted peer: lengths are checked before use and no
//! function panics on any input (property tests below).

use std::net::{Ipv4Addr, SocketAddrV4};

use crate::liveness::bencode::{self, Value};

/// BEP 3: "character ninteen (decimal) followed by the string 'BitTorrent protocol'".
pub const PROTOCOL: &[u8; 19] = b"BitTorrent protocol";
pub const HANDSHAKE_LEN: usize = 1 + 19 + 8 + 20 + 20;
/// BEP 10: "bit 20 from the right … (reserved_byte[5] & 0x10)".
pub const EXT_BYTE: usize = 5;
pub const EXT_BIT: u8 = 0x10;
/// BEP 6 (Fast Extension): "the third least significant bit in the 8th byte of the reserved
/// bytes", `reserved[7] |= 0x04`.
pub const FAST_BYTE: usize = 7;
pub const FAST_BIT: u8 = 0x04;
/// BEP 3 peer wire message ids.
pub const MSG_CHOKE: u8 = 0;
pub const MSG_UNCHOKE: u8 = 1;
pub const MSG_INTERESTED: u8 = 2;
pub const MSG_NOT_INTERESTED: u8 = 3;
pub const MSG_HAVE: u8 = 4;
pub const MSG_BITFIELD: u8 = 5;
pub const MSG_PIECE_REQUEST: u8 = 6;
pub const MSG_PIECE: u8 = 7;
pub const MSG_CANCEL: u8 = 8;
pub const MSG_PORT: u8 = 9;
/// BEP 6 message ids.
pub const MSG_SUGGEST: u8 = 13;
pub const MSG_HAVE_ALL: u8 = 14;
pub const MSG_HAVE_NONE: u8 = 15;
pub const MSG_REJECT_REQUEST: u8 = 16;
pub const MSG_ALLOWED_FAST: u8 = 17;
/// BEP 10: the `extended` message id.
pub const MSG_EXTENDED: u8 = 20;
/// BEP 10: extended message id 0 is the extended handshake.
pub const EXT_HANDSHAKE: u8 = 0;
/// The id we ask peers to use when they send `ut_metadata` messages to us (BEP 10: "The IDs are
/// local to this particular peer", i.e. the receiver chooses).
pub const OUR_UT_METADATA: u8 = 1;
/// The id we ask peers to use for `ut_pex` (BEP 11) messages to us; advertised by header
/// probes only (D143). We never send `ut_pex`.
pub const OUR_UT_PEX: u8 = 2;
/// Most `added` entries kept from one `ut_pex` message (BEP 11 asks senders for at most 50; the
/// 64 KiB extended-message cap allows ~10 000). A longer list is cut, not an error.
pub const PEX_MAX_ADDED: usize = 1024;
/// BEP 11 `added.f` flag bit: the peer is a seed.
pub const PEX_SEED: u8 = 0x02;
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
    handshake_with(info_hash, peer_id, false)
}

/// Our BEP 3 handshake, with the BEP 6 Fast Extension bit too when `fast` (header probes on,
/// D117: the fetcher then sends `HAVE_NONE` to peers that set it as well).
pub fn handshake_with(info_hash: &[u8; 20], peer_id: &[u8; 20], fast: bool) -> [u8; HANDSHAKE_LEN] {
    let mut out = [0u8; HANDSHAKE_LEN];
    out[0] = 19;
    out[1..20].copy_from_slice(PROTOCOL);
    out[20 + EXT_BYTE] = EXT_BIT;
    if fast {
        out[20 + FAST_BYTE] |= FAST_BIT;
    }
    out[28..48].copy_from_slice(info_hash);
    out[48..68].copy_from_slice(peer_id);
    out
}

/// True when the reserved bytes of a handshake set the BEP 6 Fast Extension bit.
pub fn has_fast(reserved: &[u8; 8]) -> bool {
    reserved[FAST_BYTE] & FAST_BIT != 0
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
    our_extended_handshake_with(client, false)
}

/// [`our_extended_handshake`], with `m.ut_pex` = [`OUR_UT_PEX`] too when `pex` (header probes,
/// D143). With `pex` false the bytes are those of [`our_extended_handshake`].
pub fn our_extended_handshake_with(client: &str, pex: bool) -> Vec<u8> {
    let mut m = vec![(
        b"ut_metadata".to_vec(),
        Value::Int(i64::from(OUR_UT_METADATA)),
    )];
    if pex {
        m.push((b"ut_pex".to_vec(), Value::Int(i64::from(OUR_UT_PEX))));
    }
    let dict = Value::Dict(vec![
        (b"m".to_vec(), Value::Dict(m)),
        (b"v".to_vec(), Value::Bytes(client.as_bytes().to_vec())),
    ]);
    extended(EXT_HANDSHAKE, &bencode::encode(&dict))
}

/// What we need from the peer's extended handshake.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ExtHandshake {
    /// The id to send `ut_metadata` messages to this peer with (None: absent or 0 = disabled).
    pub ut_metadata: Option<u8>,
    /// The id the peer uses for `ut_pex` (BEP 11): present when it supports peer exchange, so
    /// a message may come (None: absent or 0 = disabled). We never send one (D143).
    pub ut_pex: Option<u8>,
    /// BEP 9 `metadata_size` (any integer as sent; the caller checks the range).
    pub metadata_size: Option<i64>,
    /// BEP 10 `reqq`: outstanding requests the peer accepts (1..=65535 kept, else None).
    pub reqq: Option<u32>,
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
    let ut_pex = v
        .get(b"m")
        .and_then(|m| m.get(b"ut_pex"))
        .and_then(Value::as_int)
        .and_then(|i| u8::try_from(i).ok())
        .filter(|&i| i != 0);
    let metadata_size = v.get(b"metadata_size").and_then(Value::as_int);
    let reqq = v
        .get(b"reqq")
        .and_then(Value::as_int)
        .and_then(|i| u32::try_from(i).ok())
        .filter(|&i| (1..=65_535).contains(&i));
    Ok(ExtHandshake {
        ut_metadata,
        ut_pex,
        metadata_size,
        reqq,
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

/// One peer learned from a `ut_pex` message: its address and whether it said it is a seed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PexPeer {
    pub addr: SocketAddrV4,
    pub seed: bool,
}

/// Parse a BEP 11 `ut_pex` payload (after the extended id byte): the compact IPv4 `added` list
/// (6 bytes each) with its `added.f` flags (one byte each, [`PEX_SEED`] = 0x02). `added6`,
/// `dropped` and `dropped6` are ignored. Untrusted input: at most [`PEX_MAX_ADDED`] entries are
/// returned, a trailing partial entry is ignored, a missing or short `added.f` means flags 0,
/// entries with port 0 or an unspecified address are skipped, and a payload that is not a
/// bencoded dict is an error (the caller ignores the message). Never panics.
pub fn parse_pex(payload: &[u8]) -> Result<Vec<PexPeer>, WireError> {
    let v = bencode::decode(payload).map_err(|_| WireError::Malformed)?;
    if v.as_dict().is_none() {
        return Err(WireError::Malformed);
    }
    // A wrong type (`added` an int or a list) is the same as absent.
    let added = v
        .get(b"added")
        .and_then(Value::as_bytes)
        .unwrap_or_default();
    let flags = v
        .get(b"added.f")
        .and_then(Value::as_bytes)
        .unwrap_or_default();
    let mut out = Vec::new();
    for (i, c) in added
        .as_chunks::<6>()
        .0
        .iter()
        .take(PEX_MAX_ADDED)
        .enumerate()
    {
        let ip = Ipv4Addr::new(c[0], c[1], c[2], c[3]);
        let port = u16::from_be_bytes([c[4], c[5]]);
        if port == 0 || ip.is_unspecified() {
            continue;
        }
        out.push(PexPeer {
            addr: SocketAddrV4::new(ip, port),
            seed: flags.get(i).is_some_and(|f| f & PEX_SEED != 0),
        });
    }
    Ok(out)
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
        assert!(!has_fast(&parse_handshake(&h, &[7; 20]).unwrap().reserved));
        let f = handshake_with(&[7; 20], &[9; 20], true);
        assert_eq!(&f[20..28], &[0, 0, 0, 0, 0, 0x10, 0, 0x04]);
        assert!(has_fast(&parse_handshake(&f, &[7; 20]).unwrap().reserved));
        assert_eq!(handshake_with(&[7; 20], &[9; 20], false), h);
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
        assert_eq!(hs.reqq, None);
        let q = parse_ext_handshake(b"d1:md11:ut_metadatai3ee4:reqqi250ee").unwrap();
        assert_eq!(q.reqq, Some(250));
        assert_eq!(parse_ext_handshake(b"d4:reqqi0ee").unwrap().reqq, None);
        assert_eq!(parse_ext_handshake(b"d4:reqqi-5ee").unwrap().reqq, None);
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
    fn extended_handshake_with_pex() {
        // Without pex: the bytes are those of the plain handshake.
        assert_eq!(
            our_extended_handshake_with("Magnetar 0.2.0", false),
            our_extended_handshake("Magnetar 0.2.0")
        );
        let p = our_extended_handshake_with("Magnetar 0.2.0", true);
        assert_eq!(
            &p[6..],
            b"d1:md11:ut_metadatai1e6:ut_pexi2ee1:v14:Magnetar 0.2.0e".as_slice()
        );
        let hs = parse_ext_handshake(&p[6..]).unwrap();
        assert_eq!(hs.ut_pex, Some(OUR_UT_PEX));
        assert_eq!(
            parse_ext_handshake(&our_extended_handshake("x")[6..])
                .unwrap()
                .ut_pex,
            None
        );
        let off = parse_ext_handshake(b"d1:md6:ut_pexi0eee").unwrap();
        assert_eq!(off.ut_pex, None);
    }

    fn compact(list: &[([u8; 4], u16)]) -> Vec<u8> {
        let mut b = Vec::new();
        for (ip, port) in list {
            b.extend_from_slice(ip);
            b.extend_from_slice(&port.to_be_bytes());
        }
        b
    }

    fn pex_msg(added: &[u8], flags: &[u8]) -> Vec<u8> {
        let mut m = format!("d5:added{}:", added.len()).into_bytes();
        m.extend_from_slice(added);
        m.extend_from_slice(format!("7:added.f{}:", flags.len()).as_bytes());
        m.extend_from_slice(flags);
        m.extend_from_slice(b"e");
        m
    }

    #[test]
    fn pex_parses_added_and_flags() {
        let a = compact(&[
            ([1, 2, 3, 4], 6881),
            ([5, 6, 7, 8], 51413),
            ([9, 9, 9, 9], 1),
        ]);
        let got = parse_pex(&pex_msg(&a, &[0x02, 0x01, 0x03])).unwrap();
        assert_eq!(got.len(), 3);
        assert_eq!(got[0].addr, "1.2.3.4:6881".parse().unwrap());
        assert!(got[0].seed);
        assert!(!got[1].seed, "0x01 = prefers encryption, not a seed");
        assert!(got[2].seed, "0x03 has the seed bit");
        // Short or missing flags: 0.
        let got = parse_pex(&pex_msg(&a, &[0x02])).unwrap();
        assert!(got[0].seed && !got[1].seed && !got[2].seed);
        let got = parse_pex(
            &format!("d5:added{}:", a.len())
                .bytes()
                .chain(a.iter().copied())
                .chain(*b"e")
                .collect::<Vec<u8>>(),
        )
        .unwrap();
        assert_eq!(got.len(), 3);
        // Other keys are ignored (added6, dropped, dropped6, unknown).
        let mut m = b"d5:added0:6:added618:".to_vec();
        m.extend_from_slice(&[7; 18]);
        m.extend_from_slice(b"7:dropped6:");
        m.extend_from_slice(&[1; 6]);
        m.extend_from_slice(b"e");
        assert_eq!(parse_pex(&m).unwrap(), vec![]);
        // Port 0 and 0.0.0.0 are skipped.
        let z = compact(&[([1, 2, 3, 4], 0), ([0, 0, 0, 0], 80), ([1, 2, 3, 4], 80)]);
        assert_eq!(parse_pex(&pex_msg(&z, &[])).unwrap().len(), 1);
    }

    #[test]
    fn pex_hostile_payloads_are_bounded_and_never_panic() {
        // Truncated compact list: the partial last entry is dropped.
        let mut a = compact(&[([1, 2, 3, 4], 80), ([5, 6, 7, 8], 81)]);
        a.extend_from_slice(&[9, 9, 9, 9]);
        let got = parse_pex(&pex_msg(&a, &[])).unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(parse_pex(&pex_msg(&[1, 2, 3], &[])).unwrap(), vec![]);
        // A declared length past the payload is malformed.
        assert!(parse_pex(b"d5:added600:abce").is_err());
        // Huge list: cut at PEX_MAX_ADDED (10 000 entries fit the 64 KiB message cap).
        let big: Vec<([u8; 4], u16)> = (0..10_000u32)
            .map(|i| ([10, 0, (i >> 8) as u8, i as u8], 1000 + (i % 5000) as u16))
            .collect();
        let got = parse_pex(&pex_msg(&compact(&big), &vec![0x02; 10_000])).unwrap();
        assert_eq!(got.len(), PEX_MAX_ADDED);
        // Wrong types are the same as absent; a non-dict is an error.
        assert_eq!(parse_pex(b"d5:addedi5e7:added.fli1eee").unwrap(), vec![]);
        assert_eq!(parse_pex(b"d5:addedl4:abcdee").unwrap(), vec![]);
        assert_eq!(parse_pex(b"de").unwrap(), vec![]);
        assert!(parse_pex(b"li1ee").is_err());
        assert!(parse_pex(b"i5e").is_err());
        assert!(parse_pex(b"").is_err());
        assert!(parse_pex(b"d5:added").is_err());
        // Deep nesting is refused by the decoder, not a stack overflow.
        let deep = [vec![b'l'; 100_000], vec![b'e'; 100_000]].concat();
        assert!(parse_pex(&deep).is_err());
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
            if let Ok(v) = parse_pex(&b) {
                prop_assert!(v.len() <= PEX_MAX_ADDED);
            }
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

//! `codec::tests`: closed captions signalled in the first access unit (D123).

use super::super::Tk;
use super::{h264_sei, hevc_sei, mpeg2_captions};

/// A GA94 T.35 payload with `triples` (marker / valid / type byte, two data bytes).
fn ga94(triples: &[[u8; 3]]) -> Vec<u8> {
    let mut p = vec![0xb5, 0x00, 0x31, b'G', b'A', b'9', b'4', 0x03];
    p.push(0x40 | triples.len() as u8);
    p.push(0xff);
    for t in triples {
        p.extend_from_slice(t);
    }
    p.push(0xff); // marker_bits
    p
}

/// An SEI NAL with one user_data_registered_itu_t_t35 message (header bytes first).
fn sei(header: &[u8], payload: &[u8]) -> Vec<u8> {
    let mut n = header.to_vec();
    n.push(4);
    n.push(payload.len() as u8);
    n.extend_from_slice(payload);
    n.push(0x80);
    n
}

#[test]
fn ga94_captions() {
    // H.264: one valid CEA-608 field-1 pair.
    let mut t = Tk::default();
    h264_sei(&sei(&[0x06], &ga94(&[[0xfc, 0x94, 0x2c]])), &mut t);
    assert_eq!(t.captions, 1);
    // HEVC prefix SEI (type 39): a valid DTVCC packet start (cc_type 3) and data (2).
    let mut t = Tk::default();
    hevc_sei(
        &sei(
            &[39 << 1, 1],
            &ga94(&[[0xff, 0x02, 0x21], [0xfe, 0x00, 0x00]]),
        ),
        &mut t,
    );
    assert_eq!(t.captions, 2);
    assert!(!t.hdr10plus);
    // Every triple invalid (cc_valid = 0): nothing.
    let mut t = Tk::default();
    h264_sei(
        &sei(
            &[0x06],
            &ga94(&[[0xf8, 0x80, 0x80], [0xf9, 0x80, 0x80], [0xfa, 0, 0]]),
        ),
        &mut t,
    );
    assert_eq!(t.captions, 0);
    // Valid but null CEA-608 padding (`80 80`, `00 00`) and a zero DTVCC data pair: nothing
    // (review of D123).
    let mut t = Tk::default();
    h264_sei(
        &sei(
            &[0x06],
            &ga94(&[[0xfc, 0x80, 0x80], [0xfd, 0x00, 0x00], [0xfe, 0, 0]]),
        ),
        &mut t,
    );
    assert_eq!(t.captions, 0);
    // Another T.35 provider: nothing.
    let mut other = ga94(&[[0xfc, 0x94, 0x2c]]);
    other[2] = 0x3c;
    let mut t = Tk::default();
    h264_sei(&sei(&[0x06], &other), &mut t);
    assert_eq!(t.captions, 0);
    // MPEG-2 user data: GA94 and the DVD style.
    let mut ud = vec![0, 0, 1, 0xb2];
    ud.extend_from_slice(b"GA94\x03\x41\xff\xfc\x94\x2c");
    assert_eq!(mpeg2_captions(&ud), 1);
    assert_eq!(mpeg2_captions(&[0, 0, 1, 0xb2, b'C', b'C', 1, 0xf8, 0]), 1);
    assert_eq!(mpeg2_captions(&[0, 0, 1, 0xb2, b'X', b'Y', 1, 0xf8, 0]), 0);
}

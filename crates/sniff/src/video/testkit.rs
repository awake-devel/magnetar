//! Container builders for the parsers' unit tests (a subset of `tests/video_fixtures/build.rs`,
//! which the integration tests use): Matroska elements, ISO-BMFF boxes, TS packets.

use super::{VideoInfo, analyze, recommended_caps};
use crate::sparse::SparseFile;

/// Recon over a fully known file.
pub fn info(bytes: &[u8]) -> VideoInfo {
    let f = SparseFile::from_bytes(bytes.to_vec());
    let r = analyze(&f, &recommended_caps());
    r.info.expect("video info")
}

// ---------------------------------------------------------------- Matroska

/// Matroska element with an 8-byte size.
pub fn el(id: &[u8], data: &[u8]) -> Vec<u8> {
    let mut v = id.to_vec();
    v.push(0x01);
    v.extend_from_slice(&(data.len() as u64).to_be_bytes()[1..]);
    v.extend_from_slice(data);
    v
}

pub fn uel(id: &[u8], n: u64) -> Vec<u8> {
    el(id, &n.to_be_bytes())
}

pub fn sel(id: &[u8], s: &str) -> Vec<u8> {
    el(id, s.as_bytes())
}

/// TrackEntry: number, type (1 video, 2 audio, 0x11 subtitle), codec ID, extra children.
pub fn track(number: u64, ty: u64, codec: &str, extra: &[Vec<u8>]) -> Vec<u8> {
    let mut t = uel(&[0xD7], number);
    t.extend(uel(&[0x73, 0xC5], 1000 + number));
    t.extend(uel(&[0x83], ty));
    t.extend(el(&[0x86], codec.as_bytes()));
    t.extend(extra.concat());
    el(&[0xAE], &t)
}

/// A Matroska file with `tracks` and no clusters.
pub fn mkv(tracks: &[Vec<u8>]) -> Vec<u8> {
    let mut h = uel(&[0x42, 0x86], 1);
    h.extend(el(&[0x42, 0x82], b"matroska"));
    let mut f = el(&[0x1A, 0x45, 0xDF, 0xA3], &h);
    let mut info = uel(&[0x2A, 0xD7, 0xB1], 1_000_000);
    info.extend(el(&[0x44, 0x89], &1000.0f64.to_be_bytes()));
    let seg = [
        el(&[0x15, 0x49, 0xA9, 0x66], &info),
        el(&[0x16, 0x54, 0xAE, 0x6B], &tracks.concat()),
    ]
    .concat();
    f.extend(el(&[0x18, 0x53, 0x80, 0x67], &seg));
    f
}

// ---------------------------------------------------------------- ISO-BMFF

pub fn bx(ty: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut v = ((body.len() + 8) as u32).to_be_bytes().to_vec();
    v.extend_from_slice(ty);
    v.extend_from_slice(body);
    v
}

pub fn full(ty: &[u8; 4], version: u8, flags: u32, body: &[u8]) -> Vec<u8> {
    let mut b = vec![version];
    b.extend_from_slice(&flags.to_be_bytes()[1..]);
    b.extend_from_slice(body);
    bx(ty, &b)
}

/// `hdlr` with a handler type and a C-string name.
pub fn hdlr(handler: &[u8; 4], name: &str) -> Vec<u8> {
    let mut b = vec![0u8; 4];
    b.extend_from_slice(handler);
    b.extend_from_slice(&[0; 12]);
    b.extend_from_slice(name.as_bytes());
    b.push(0);
    full(b"hdlr", 0, 0, &b)
}

/// A sample entry: `fourcc`, the 8-byte SampleEntry head, then `rest`.
pub fn entry(fourcc: &[u8; 4], rest: &[u8]) -> Vec<u8> {
    let mut b = vec![0, 0, 0, 0, 0, 0, 0, 1];
    b.extend_from_slice(rest);
    bx(fourcc, &b)
}

/// An audio sample entry (version 0: channels 2, 48 kHz) with child boxes.
pub fn audio_entry(fourcc: &[u8; 4], kids: &[u8]) -> Vec<u8> {
    let mut b = vec![0u8; 8];
    b.extend_from_slice(&[0, 2, 0, 16, 0, 0, 0, 0]);
    b.extend_from_slice(&(48000u32 << 16).to_be_bytes());
    b.extend_from_slice(kids);
    entry(fourcc, &b)
}

/// A `trak`: tkhd (track id), optional `tref`, mdia (mdhd language `eng`, hdlr, stbl/stsd),
/// optional `udta`.
pub fn trak(id: u32, handler: &[u8; 4], stsd_entry: &[u8], tref: &[u8], udta: &[u8]) -> Vec<u8> {
    trak_full(id, &hdlr(handler, ""), stsd_entry, tref, udta)
}

/// A `trak` with a hand-made `hdlr` box.
pub fn trak_with_hdlr(id: u32, stsd_entry: &[u8], hdlr: &[u8]) -> Vec<u8> {
    trak_full(id, hdlr, stsd_entry, &[], &[])
}

fn trak_full(id: u32, hdlr: &[u8], stsd_entry: &[u8], tref: &[u8], udta: &[u8]) -> Vec<u8> {
    let mut tkhd = vec![0u8; 8];
    tkhd.extend_from_slice(&id.to_be_bytes());
    tkhd.extend_from_slice(&[0u8; 72]);
    let mut mdhd = vec![0u8; 8];
    mdhd.extend_from_slice(&1000u32.to_be_bytes());
    mdhd.extend_from_slice(&1000u32.to_be_bytes());
    // "eng" packed.
    let code: u16 = ((u16::from(b'e') - 0x60) << 10)
        | ((u16::from(b'n') - 0x60) << 5)
        | (u16::from(b'g') - 0x60);
    mdhd.extend_from_slice(&code.to_be_bytes());
    mdhd.extend_from_slice(&[0, 0]);
    let mut stsd = 1u32.to_be_bytes().to_vec();
    stsd.extend_from_slice(stsd_entry);
    let stbl = bx(b"stbl", &full(b"stsd", 0, 0, &stsd));
    let minf = bx(b"minf", &stbl);
    let mdia = bx(
        b"mdia",
        &[full(b"mdhd", 0, 0, &mdhd), hdlr.to_vec(), minf].concat(),
    );
    let mut t = full(b"tkhd", 0, 3, &tkhd);
    if !tref.is_empty() {
        t.extend(bx(b"tref", tref));
    }
    t.extend(mdia);
    if !udta.is_empty() {
        t.extend(bx(b"udta", udta));
    }
    bx(b"trak", &t)
}

/// An MP4 file: ftyp, moov (mvhd + traks), a small mdat.
pub fn mp4(traks: &[Vec<u8>]) -> Vec<u8> {
    let mut mvhd = vec![0u8; 8];
    mvhd.extend_from_slice(&1000u32.to_be_bytes());
    mvhd.extend_from_slice(&1000u32.to_be_bytes());
    mvhd.extend_from_slice(&[0u8; 80]);
    let mut f = bx(b"ftyp", b"isom\0\0\0\0isom");
    f.extend(bx(
        b"moov",
        &[full(b"mvhd", 0, 0, &mvhd), traks.concat()].concat(),
    ));
    f.extend(bx(b"mdat", &[0u8; 64]));
    f
}

// ---------------------------------------------------------------- MPEG-TS

/// 188-byte TS packets carrying `data` on `pid` (first packet with PUSI).
pub fn ts_packets(pid: u16, data: &[u8], cc: &mut u8) -> Vec<u8> {
    let mut out = Vec::new();
    let mut first = true;
    for chunk in data.chunks(184) {
        let mut p = vec![
            0x47,
            ((pid >> 8) as u8 & 0x1f) | if first { 0x40 } else { 0 },
            pid as u8,
        ];
        if chunk.len() < 184 {
            let stuff = 184 - chunk.len();
            p.push(0x30 | (*cc & 0xf));
            p.push((stuff - 1) as u8);
            if stuff > 1 {
                p.push(0);
                p.extend(std::iter::repeat_n(0xff, stuff - 2));
            }
        } else {
            p.push(0x10 | (*cc & 0xf));
        }
        p.extend_from_slice(chunk);
        out.extend(p);
        *cc = cc.wrapping_add(1);
        first = false;
    }
    out
}

fn psi(table_id: u8, body: &[u8]) -> Vec<u8> {
    let len = body.len() + 5 + 4;
    let mut s = vec![
        0u8,
        table_id,
        0xb0 | ((len >> 8) as u8 & 0xf),
        len as u8,
        0,
        1,
        0xc1,
        0,
        0,
    ];
    s.extend_from_slice(body);
    s.extend_from_slice(&[0, 0, 0, 0]);
    s
}

/// PMT: (stream_type, pid, descriptors).
fn pmt(streams: &[(u8, u16, Vec<u8>)]) -> Vec<u8> {
    let mut b = vec![0xe1, 0x00, 0xf0, 0x00];
    for (st, pid, d) in streams {
        b.push(*st);
        b.push(0xe0 | (pid >> 8) as u8);
        b.push(*pid as u8);
        b.push(0xf0 | (d.len() >> 8) as u8);
        b.push(d.len() as u8);
        b.extend_from_slice(d);
    }
    psi(2, &b)
}

/// PES packet with a PTS.
pub fn pes(stream_id: u8, pts: u64, data: &[u8]) -> Vec<u8> {
    let mut p = vec![0, 0, 1, stream_id, 0, 0, 0x80, 0x80, 5];
    p.push(0x21 | (((pts >> 30) & 7) << 1) as u8);
    p.push((pts >> 22) as u8);
    p.push(0x01 | ((pts >> 14) & 0xfe) as u8);
    p.push((pts >> 7) as u8);
    p.push(0x01 | ((pts << 1) & 0xfe) as u8);
    p.extend_from_slice(data);
    p
}

/// A TS file: PAT, PMT on 0x100 with `streams`, then `payloads` (pid, PES data).
pub fn ts(streams: &[(u8, u16, Vec<u8>)], payloads: &[(u16, Vec<u8>)]) -> Vec<u8> {
    let mut cc = 0u8;
    let mut f = ts_packets(0, &psi(0, &[0, 1, 0xe1, 0x00]), &mut cc);
    f.extend(ts_packets(0x100, &pmt(streams), &mut cc));
    for (pid, data) in payloads {
        f.extend(ts_packets(*pid, data, &mut cc));
    }
    f
}

/// A descriptor.
pub fn desc(tag: u8, body: &[u8]) -> Vec<u8> {
    let mut d = vec![tag, body.len() as u8];
    d.extend_from_slice(body);
    d
}

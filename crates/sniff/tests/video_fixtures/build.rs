//! Container builders for the video tests: Matroska elements, ISO-BMFF boxes, TS packets, a
//! bit writer for hand-made bitstream headers, and a sparse fetch loop over `video::analyze`.
#![allow(dead_code)]

use sniff::video::{self, Recon};
use sniff::{Caps, Range, SparseFile};

/// First frames of ffmpeg-encoded streams, patched by `thd_patch.py` / `eac3_patch.py` where
/// named "atmos" / "joc"; ffprobe reports them as "Dolby TrueHD + Dolby Atmos" / "Dolby
/// Digital Plus + Dolby Atmos" / "DTS" (5.1). Spec-derived Atmos signalling, not real Atmos.
pub const EAC3_JOC: &[u8] = include_bytes!("eac3_joc.bin");
pub const EAC3_PLAIN: &[u8] = include_bytes!("eac3_plain.bin");
pub const TRUEHD_ATMOS: &[u8] = include_bytes!("truehd_atmos.bin");
pub const TRUEHD_51: &[u8] = include_bytes!("truehd_51.bin");
pub const DTS_CORE: &[u8] = include_bytes!("dts_core51.bin");

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

pub fn ebml_header(doctype: &str) -> Vec<u8> {
    let mut h = uel(&[0x42, 0x86], 1);
    h.extend(el(&[0x42, 0x82], doctype.as_bytes()));
    el(&[0x1A, 0x45, 0xDF, 0xA3], &h)
}

pub fn mkv_info(duration_ms: f64) -> Vec<u8> {
    let mut i = uel(&[0x2A, 0xD7, 0xB1], 1_000_000);
    i.extend(el(&[0x44, 0x89], &duration_ms.to_be_bytes()));
    el(&[0x15, 0x49, 0xA9, 0x66], &i)
}

/// TrackEntry: number, type (1 video, 2 audio, 0x11 subtitle), codec ID, extra children.
pub fn track(number: u64, ty: u64, codec: &str, extra: &[u8]) -> Vec<u8> {
    let mut t = uel(&[0xD7], number);
    t.extend(uel(&[0x73, 0xC5], 1000 + number));
    t.extend(uel(&[0x83], ty));
    t.extend(el(&[0x86], codec.as_bytes()));
    t.extend_from_slice(extra);
    el(&[0xAE], &t)
}

pub fn tracks(entries: &[Vec<u8>]) -> Vec<u8> {
    el(&[0x16, 0x54, 0xAE, 0x6B], &entries.concat())
}

pub fn simple_block(track: u8, payload: &[u8]) -> Vec<u8> {
    let mut b = vec![0x80 | track, 0, 0, 0x80];
    b.extend_from_slice(payload);
    el(&[0xA3], &b)
}

pub fn cluster(blocks: &[Vec<u8>]) -> Vec<u8> {
    let mut c = uel(&[0xE7], 0);
    c.extend(blocks.concat());
    el(&[0x1F, 0x43, 0xB6, 0x75], &c)
}

pub fn segment(children: &[Vec<u8>]) -> Vec<u8> {
    el(&[0x18, 0x53, 0x80, 0x67], &children.concat())
}

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

/// MSB-first bit writer.
#[derive(Default)]
pub struct Bw {
    pub bytes: Vec<u8>,
    bit: u32,
}

impl Bw {
    pub fn put(&mut self, n: u32, v: u64) -> &mut Bw {
        for i in (0..n).rev() {
            if self.bit == 0 {
                self.bytes.push(0);
            }
            if (v >> i) & 1 == 1 {
                let last = self.bytes.last_mut().unwrap();
                *last |= 0x80 >> self.bit;
            }
            self.bit = (self.bit + 1) % 8;
        }
        self
    }
    pub fn pad_to(&mut self, len: usize) -> Vec<u8> {
        let mut b = self.bytes.clone();
        b.resize(len.max(b.len()), 0);
        b
    }
}

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
            // Adaptation field stuffing.
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
        assert_eq!(p.len(), 188);
        out.extend(p);
        *cc = cc.wrapping_add(1);
        first = false;
    }
    out
}

pub fn psi(table_id: u8, body: &[u8]) -> Vec<u8> {
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
    s.extend_from_slice(&[0, 0, 0, 0]); // CRC (not checked by the recon)
    s
}

pub fn pat(pmt_pid: u16) -> Vec<u8> {
    psi(0, &[0, 1, 0xe0 | (pmt_pid >> 8) as u8, pmt_pid as u8])
}

/// PMT: (stream_type, pid, descriptors).
pub fn pmt(streams: &[(u8, u16, Vec<u8>)]) -> Vec<u8> {
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

pub struct Drive {
    pub recon: Recon,
    pub rounds: u32,
    pub fetched: u64,
    pub ranges: Vec<Range>,
}

/// Runs `video::analyze` against `bytes` the way the driver does: a head of `head` bytes, then
/// whatever ranges it asks for, up to `rounds` times.
pub fn drive(bytes: &[u8], head: u64, rounds: u32) -> Drive {
    let caps = Caps {
        head,
        rounds,
        ..video::recommended_caps()
    };
    let len = bytes.len() as u64;
    let mut f = SparseFile::new(len);
    let mut d = Drive {
        recon: Recon::default(),
        rounds: 0,
        fetched: 0,
        ranges: Vec::new(),
    };
    let get = |f: &mut SparseFile, d: &mut Drive, r: Range| {
        let e = (r.offset + r.len).min(len) as usize;
        let s = r.offset as usize;
        f.insert(r.offset, bytes[s..e].to_vec());
        d.fetched += (e - s) as u64;
        d.ranges.push(r);
    };
    get(&mut f, &mut d, Range::new(0, head.min(len)));
    loop {
        let r = video::analyze(&f, &caps);
        if r.need.is_empty() || d.rounds >= rounds {
            d.recon = r;
            return d;
        }
        d.rounds += 1;
        for want in &r.need {
            for m in f.missing(*want) {
                get(&mut f, &mut d, m);
            }
        }
    }
}

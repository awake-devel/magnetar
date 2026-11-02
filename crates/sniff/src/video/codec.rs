//! Video codec configuration records and first-frame scans: avcC / H.264 SPS, hvcC / HEVC SPS,
//! av1C / AV1 sequence header, vpcC, VP9 uncompressed frame header, `colr`, Dolby Vision
//! configuration (dvcC / dvvC / dvwC, Matroska BlockAdditionMapping, TS descriptor 0xB0),
//! HEVC SEI (mastering display, content light level, HDR10+) from length-prefixed or Annex-B
//! NAL units, AV1 metadata OBUs, MPEG-4 Part 2 VOS / VOL profile.

use super::bits::{Bits, Cur, leb128, unescape};
use super::{DolbyVision, Tk};

/// RBSP cap for SPS / SEI parsing.
const RBSP_CAP: usize = 64 * 1024;

pub(crate) fn chroma_name(c: u32) -> Option<&'static str> {
    ["4:0:0", "4:2:0", "4:2:2", "4:4:4"]
        .get(c as usize)
        .copied()
}

fn avc_profile(p: u32, constraints: u32) -> String {
    let c1 = constraints & 0x40 != 0;
    let c3 = constraints & 0x10 != 0;
    match (p, c1, c3) {
        (66, true, _) => "Constrained Baseline",
        (66, _, _) => "Baseline",
        (77, _, _) => "Main",
        (88, _, _) => "Extended",
        (100, _, _) => "High",
        (110, _, true) => "High 10 Intra",
        (110, _, _) => "High 10",
        (122, _, true) => "High 4:2:2 Intra",
        (122, _, _) => "High 4:2:2",
        (244, _, true) => "High 4:4:4 Intra",
        (244, _, _) => "High 4:4:4 Predictive",
        (44, _, _) => "CAVLC 4:4:4",
        (118, _, _) => "Multiview High",
        (128, _, _) => "Stereo High",
        _ => return format!("profile {p}"),
    }
    .into()
}

fn hevc_profile(p: u32) -> String {
    match p {
        1 => "Main".into(),
        2 => "Main 10".into(),
        3 => "Main Still Picture".into(),
        4 => "Rext".into(),
        9 => "SCC".into(),
        n => format!("profile {n}"),
    }
}

/// avcC: profile / level from the record, the rest from its first SPS.
pub(crate) fn avcc(b: &[u8], t: &mut Tk) -> Option<()> {
    let mut c = Cur::new(b);
    c.u8()?;
    let prof = c.u8()?;
    let cons = c.u8()?;
    let level = c.u8()?;
    c.u8()?;
    t.profile = Some(avc_profile(u32::from(prof), u32::from(cons)));
    t.level = Some(format!("{}.{}", level / 10, level % 10));
    let nsps = c.u8()? & 0x1f;
    if nsps > 0 {
        let l = usize::from(c.u16()?);
        h264_sps(c.take(l)?, t);
    }
    Some(())
}

fn h264_scaling(b: &mut Bits, n: usize) -> Option<()> {
    let (mut last, mut next) = (8i32, 8i32);
    for _ in 0..n {
        if next != 0 {
            next = (last + b.se()? + 256).rem_euclid(256);
        }
        if next != 0 {
            last = next;
        }
    }
    Some(())
}

/// H.264 SPS NAL (header byte included): resolution, bit depth, chroma, VUI colour + timing.
pub(crate) fn h264_sps(nal: &[u8], t: &mut Tk) -> Option<()> {
    let rb = unescape(nal, RBSP_CAP);
    let mut b = Bits::new(&rb);
    b.skip(8)?;
    let prof = b.u(8)?;
    let cons = b.u(8)?;
    let level = b.u(8)?;
    b.ue()?;
    if t.profile.is_none() {
        t.profile = Some(avc_profile(prof, cons));
        t.level = Some(format!("{}.{}", level / 10, level % 10));
    }
    let mut chroma = 1;
    if [100, 110, 122, 244, 44, 83, 86, 118, 128, 138, 139, 134, 135].contains(&prof) {
        chroma = b.ue()?;
        if chroma == 3 {
            b.skip(1)?;
        }
        t.bit_depth = u8::try_from(b.ue()?.saturating_add(8)).ok();
        b.ue()?;
        b.skip(1)?;
        if b.flag()? {
            for i in 0..(if chroma != 3 { 8 } else { 12 }) {
                if b.flag()? {
                    h264_scaling(&mut b, if i < 6 { 16 } else { 64 })?;
                }
            }
        }
    } else {
        t.bit_depth = Some(8);
    }
    t.chroma = chroma_name(chroma);
    b.ue()?;
    let poc = b.ue()?;
    if poc == 0 {
        b.ue()?;
    } else if poc == 1 {
        b.skip(1)?;
        b.se()?;
        b.se()?;
        let n = b.ue()?;
        if n > 255 {
            return None;
        }
        for _ in 0..n {
            b.se()?;
        }
    }
    b.ue()?;
    b.skip(1)?;
    let wmb = b.ue()?.saturating_add(1);
    let hmu = b.ue()?.saturating_add(1);
    let frame_mbs_only = b.bit()?;
    if frame_mbs_only == 0 {
        b.skip(1)?;
    }
    b.skip(1)?;
    let (mut cl, mut cr, mut ct, mut cb) = (0u32, 0u32, 0u32, 0u32);
    if b.flag()? {
        cl = b.ue()?;
        cr = b.ue()?;
        ct = b.ue()?;
        cb = b.ue()?;
    }
    let (sx, sy) = match chroma {
        1 => (2, 2),
        2 => (2, 1),
        _ => (1, 1),
    };
    let fm = 2 - frame_mbs_only;
    let w = wmb
        .saturating_mul(16)
        .saturating_sub(cl.saturating_add(cr).saturating_mul(sx));
    let h = fm
        .saturating_mul(hmu)
        .saturating_mul(16)
        .saturating_sub(ct.saturating_add(cb).saturating_mul(sy).saturating_mul(fm));
    if w > 0 && h > 0 {
        t.width.get_or_insert(w);
        t.height.get_or_insert(h);
    }
    if b.flag()? {
        vui_colour_and_timing(&mut b, t, false)?;
    }
    Some(())
}

/// VUI up to timing info (H.264 and HEVC share the start; HEVC has more fields before timing).
fn vui_colour_and_timing(b: &mut Bits, t: &mut Tk, hevc: bool) -> Option<()> {
    if b.flag()? && b.u(8)? == 255 {
        b.skip(32)?;
    }
    if b.flag()? {
        b.skip(1)?;
    }
    if b.flag()? {
        b.skip(4)?;
        if b.flag()? {
            t.primaries = u8::try_from(b.u(8)?).ok();
            t.transfer = u8::try_from(b.u(8)?).ok();
            t.matrix = u8::try_from(b.u(8)?).ok();
        }
    }
    if b.flag()? {
        b.ue()?;
        b.ue()?;
    }
    if hevc {
        b.skip(3)?;
        if b.flag()? {
            for _ in 0..4 {
                b.ue()?;
            }
        }
    }
    if b.flag()? {
        let n = b.u(32)?;
        let s = b.u(32)?;
        if n > 0 && s > 0 && t.fps.is_none() {
            let f = f64::from(s) / f64::from(n);
            t.fps = Some(if hevc { f } else { f / 2.0 });
        }
    }
    Some(())
}

/// hvcC: profile, chroma, bit depth from the header; SPS (VUI colour, fps) from the arrays.
/// Returns the NAL length size.
pub(crate) fn hvcc(b: &[u8], t: &mut Tk) -> Option<u8> {
    let mut c = Cur::new(b);
    c.u8()?;
    let p = c.u8()?;
    t.profile = Some(hevc_profile(u32::from(p & 0x1f)));
    c.skip(4 + 6)?;
    let level = c.u8()?;
    t.level = Some(format!("{}", f64::from(level) / 30.0));
    c.skip(2 + 1)?;
    t.chroma = chroma_name(u32::from(c.u8()? & 3));
    t.bit_depth = Some((c.u8()? & 7) + 8);
    c.skip(1 + 2)?;
    let len_size = (c.u8()? & 3) + 1;
    let narr = c.u8()?;
    for _ in 0..narr {
        let ty = c.u8()? & 0x3f;
        let n = c.u16()?;
        for _ in 0..n.min(64) {
            let l = usize::from(c.u16()?);
            let nal = c.take(l)?;
            match ty {
                33 => {
                    let _ = hevc_sps(nal, t);
                }
                39 | 40 => hevc_sei(nal, t),
                _ => {}
            }
        }
    }
    Some(len_size)
}

fn hevc_ptl(b: &mut Bits, max_sub: u32) -> Option<(u32, u32)> {
    b.skip(3)?;
    let prof = b.u(5)?;
    b.skip(32 + 48)?;
    let level = b.u(8)?;
    let n = usize::try_from(max_sub).ok()?.min(7);
    let mut flags = Vec::with_capacity(n);
    for _ in 0..n {
        flags.push((b.flag()?, b.flag()?));
    }
    if n > 0 {
        b.skip(2 * (8 - n))?;
    }
    for (profile_present, level_present) in flags {
        if profile_present {
            b.skip(88)?;
        }
        if level_present {
            b.skip(8)?;
        }
    }
    Some((prof, level))
}

/// HEVC SPS NAL (2-byte header included) through VUI: resolution, bit depth, chroma, colour,
/// timing.
pub(crate) fn hevc_sps(nal: &[u8], t: &mut Tk) -> Option<()> {
    let rb = unescape(nal, RBSP_CAP);
    let mut b = Bits::new(&rb);
    b.skip(16 + 4)?;
    let max_sub = b.u(3)?;
    b.skip(1)?;
    let (prof, level) = hevc_ptl(&mut b, max_sub)?;
    if t.profile.is_none() {
        t.profile = Some(hevc_profile(prof));
        t.level = Some(format!("{}", f64::from(level) / 30.0));
    }
    b.ue()?;
    let chroma = b.ue()?;
    if chroma == 3 {
        b.skip(1)?;
    }
    let mut w = b.ue()?;
    let mut h = b.ue()?;
    if b.flag()? {
        let (sx, sy) = match chroma {
            1 => (2, 2),
            2 => (2, 1),
            _ => (1, 1),
        };
        let (l, r, tp, bt) = (b.ue()?, b.ue()?, b.ue()?, b.ue()?);
        w = w.saturating_sub(l.saturating_add(r).saturating_mul(sx));
        h = h.saturating_sub(tp.saturating_add(bt).saturating_mul(sy));
    }
    if w > 0 && h > 0 {
        // The SPS size after the conformance window crop is the picture size (what ffprobe
        // reports); container sizes are often the coded size rounded to 8 or 16.
        t.width = Some(w);
        t.height = Some(h);
    }
    t.sps = true;
    t.bit_depth = u8::try_from(b.ue()?.saturating_add(8)).ok();
    t.chroma = chroma_name(chroma);
    b.ue()?;
    let log2_poc = b.ue()?.saturating_add(4);
    let sub_info = b.flag()?;
    let first = if sub_info { 0 } else { max_sub };
    for _ in first..=max_sub {
        b.ue()?;
        b.ue()?;
        b.ue()?;
    }
    for _ in 0..6 {
        b.ue()?;
    }
    if b.flag()? && b.flag()? {
        for size_id in 0..4u32 {
            let mut m = 0;
            while m < 6 {
                if !b.flag()? {
                    b.ue()?;
                } else {
                    let n = 64.min(1u32 << (4 + (size_id << 1)));
                    if size_id > 1 {
                        b.se()?;
                    }
                    for _ in 0..n {
                        b.se()?;
                    }
                }
                m += if size_id == 3 { 3 } else { 1 };
            }
        }
    }
    b.skip(2)?;
    if b.flag()? {
        b.skip(8)?;
        b.ue()?;
        b.ue()?;
        b.skip(1)?;
    }
    let nst = b.ue()?;
    if nst > 64 {
        return None;
    }
    let mut ndelta: Vec<u32> = Vec::new();
    for i in 0..nst {
        let inter = i != 0 && b.flag()?;
        if inter {
            b.skip(1)?;
            b.ue()?;
            let refn = *ndelta.last().unwrap_or(&0);
            let mut cnt = 0u32;
            for _ in 0..=refn.min(64) {
                let used = b.flag()?;
                let ud = if used { true } else { b.flag()? };
                if used || ud {
                    cnt += 1;
                }
            }
            ndelta.push(cnt);
        } else {
            let neg = b.ue()?;
            let pos = b.ue()?;
            if neg.saturating_add(pos) > 32 {
                return None;
            }
            for _ in 0..neg + pos {
                b.ue()?;
                b.skip(1)?;
            }
            ndelta.push(neg + pos);
        }
    }
    if b.flag()? {
        let n = b.ue()?;
        if n > 32 {
            return None;
        }
        for _ in 0..n {
            b.skip(usize::try_from(log2_poc).ok()?.saturating_add(1))?;
        }
    }
    b.skip(2)?;
    if b.flag()? {
        vui_colour_and_timing(&mut b, t, true)?;
    }
    Some(())
}

/// HEVC SEI NAL (prefix or suffix, 2-byte header included): mastering display (137), content
/// light level (144), HDR10+ (ITU-T T.35, B5 003C 0001 04).
pub(crate) fn hevc_sei(nal: &[u8], t: &mut Tk) {
    let rb = unescape(nal.get(2..).unwrap_or(&[]), RBSP_CAP);
    sei_messages(&rb, t);
}

fn sei_messages(rb: &[u8], t: &mut Tk) {
    let mut q = 0usize;
    for _ in 0..64 {
        if q + 2 > rb.len() || rb.get(q) == Some(&0x80) {
            break;
        }
        let mut read = || -> Option<usize> {
            let mut v = 0usize;
            loop {
                let x = *rb.get(q)?;
                q += 1;
                v = v.saturating_add(usize::from(x));
                if x != 0xff {
                    return Some(v);
                }
            }
        };
        let (Some(pt), Some(ps)) = (read(), read()) else {
            break;
        };
        let pl = rb.get(q..q.saturating_add(ps).min(rb.len())).unwrap_or(&[]);
        match pt {
            137 => t.mastering = true,
            144 if pl.len() >= 4 => {
                let mut c = Cur::new(pl);
                if let (Some(a), Some(b)) = (c.u16(), c.u16()) {
                    t.cll = Some((a, b));
                }
            }
            4 if is_hdr10plus_t35(pl) => t.hdr10plus = true,
            _ => {}
        }
        q = q.saturating_add(ps);
    }
}

fn is_hdr10plus_t35(pl: &[u8]) -> bool {
    pl.get(..6) == Some(&[0xb5, 0x00, 0x3c, 0x00, 0x01, 0x04])
}

/// HEVC NAL types of interest in an access unit.
fn hevc_nal(nal: &[u8], t: &mut Tk) -> bool {
    let Some(&h) = nal.first() else {
        return false;
    };
    let ty = (h >> 1) & 0x3f;
    match ty {
        39 | 40 => hevc_sei(nal, t),
        // The container may have given the size already (MP4 sample entry, MKV PixelWidth):
        // an in-band SPS still brings the VUI colour, the cropped size and the bit depth.
        33 if !t.sps => {
            let _ = hevc_sps(nal, t);
        }
        62 => t.dv_rpu = true,
        _ => {}
    }
    ty < 32 // a slice: prefix SEIs are over
}

/// Scans the start of an access unit with `len_size`-byte NAL lengths (MKV / MP4).
pub(crate) fn hevc_au(au: &[u8], len_size: u8, t: &mut Tk) {
    let n = usize::from(len_size.clamp(1, 4));
    let mut p = 0usize;
    for _ in 0..256 {
        let Some(lb) = au.get(p..p + n) else {
            break;
        };
        let len = lb.iter().fold(0usize, |a, &x| (a << 8) | usize::from(x));
        p += n;
        if len < 2 {
            break;
        }
        let end = p.saturating_add(len);
        let nal = au.get(p..end.min(au.len())).unwrap_or(&[]);
        let slice = hevc_nal(nal, t);
        if end > au.len() || slice {
            break;
        }
        p = end;
    }
}

/// True when the last bytes of an access unit end with a length-prefixed NAL of type 62.
pub(crate) fn ends_with_rpu(tail: &[u8], len_size: u8) -> bool {
    let n = usize::from(len_size.clamp(1, 4));
    let total = tail.len();
    for p in 0..total.saturating_sub(n + 2) {
        let Some(lb) = tail.get(p..p + n) else {
            break;
        };
        let len = lb.iter().fold(0usize, |a, &x| (a << 8) | usize::from(x));
        if p + n + len == total
            && tail.get(p + n).is_some_and(|h| (h >> 1) & 0x3f == 62)
            && tail.get(p + n + 1) == Some(&1)
        {
            return true;
        }
    }
    false
}

/// Annex-B NAL units (start-code delimited) in `buf`, the last one possibly truncated.
pub(crate) fn annexb(buf: &[u8]) -> Vec<&[u8]> {
    let mut starts = Vec::new();
    let mut i = 0usize;
    while i + 3 <= buf.len() && starts.len() < 512 {
        match memchr::memmem::find(buf.get(i..).unwrap_or(&[]), &[0, 0, 1]) {
            Some(k) => {
                starts.push(i + k + 3);
                i += k + 3;
            }
            None => break,
        }
    }
    let mut out = Vec::with_capacity(starts.len());
    for (j, s) in starts.iter().enumerate() {
        let mut e = starts.get(j + 1).map_or(buf.len(), |n| n - 3);
        // Drop the leading zero of a 4-byte start code / trailing zero bytes.
        while e > *s && buf.get(e - 1) == Some(&0) {
            e -= 1;
        }
        if let Some(n) = buf.get(*s..e) {
            out.push(n);
        }
    }
    out
}

/// Annex-B HEVC access units (TS): SPS, prefix SEIs up to the first slice. Returns true when a
/// slice was reached (nothing more to learn from this AU).
pub(crate) fn hevc_annexb(buf: &[u8], t: &mut Tk) -> bool {
    for nal in annexb(buf) {
        if hevc_nal(nal, t) {
            return true;
        }
    }
    false
}

/// av1C: profile, bit depth, chroma; config OBUs (sequence header with colour).
pub(crate) fn av1c(b: &[u8], t: &mut Tk) -> Option<()> {
    let mut c = Cur::new(b);
    c.u8()?;
    let a = c.u8()?;
    let b2 = c.u8()?;
    let prof = a >> 5;
    let hbd = (b2 >> 6) & 1;
    let twelve = (b2 >> 5) & 1;
    let mono = (b2 >> 4) & 1;
    let sx = (b2 >> 3) & 1;
    let sy = (b2 >> 2) & 1;
    t.profile = ["Main", "High", "Professional"]
        .get(usize::from(prof))
        .map(|s| s.to_string());
    t.level = Some(format!("{}", a & 0x1f));
    t.bit_depth = Some(match (hbd, twelve) {
        (1, 1) => 12,
        (1, _) => 10,
        _ => 8,
    });
    t.chroma = Some(match (mono, sx, sy) {
        (1, _, _) => "4:0:0",
        (_, 1, 1) => "4:2:0",
        (_, 1, _) => "4:2:2",
        _ => "4:4:4",
    });
    if let Some(obus) = b.get(4..) {
        av1_obus(obus, t);
    }
    Some(())
}

/// Walks OBUs (low-overhead format): sequence header, metadata (HDR CLL / MDCV / HDR10+).
/// Stops at the first frame OBU.
pub(crate) fn av1_obus(b: &[u8], t: &mut Tk) {
    let mut p = 0usize;
    for _ in 0..64 {
        let Some(&h) = b.get(p) else {
            break;
        };
        p += 1;
        let ty = (h >> 3) & 0xf;
        if (h >> 2) & 1 == 1 {
            p += 1;
        }
        let sz = if (h >> 1) & 1 == 1 {
            match leb128(b, &mut p).and_then(|v| usize::try_from(v).ok()) {
                Some(v) => v,
                None => break,
            }
        } else {
            b.len().saturating_sub(p)
        };
        let end = p.saturating_add(sz);
        let body = b.get(p..end.min(b.len())).unwrap_or(&[]);
        match ty {
            1 => {
                let _ = av1_seq(body, t);
            }
            5 => av1_metadata(body, t),
            3 | 4 | 6 => break,
            _ => {}
        }
        if end > b.len() {
            break;
        }
        p = end;
    }
}

fn av1_metadata(b: &[u8], t: &mut Tk) {
    let mut p = 0usize;
    let Some(ty) = leb128(b, &mut p) else {
        return;
    };
    let pl = b.get(p..).unwrap_or(&[]);
    match ty {
        1 if pl.len() >= 4 => {
            let mut c = Cur::new(pl);
            if let (Some(a), Some(f)) = (c.u16(), c.u16()) {
                t.cll = Some((a, f));
            }
        }
        2 => t.mastering = true,
        4 if is_hdr10plus_t35(pl) => t.hdr10plus = true,
        _ => {}
    }
}

/// AV1 sequence header OBU payload: resolution, bit depth, colour description, timing.
pub(crate) fn av1_seq(d: &[u8], t: &mut Tk) -> Option<()> {
    let mut b = Bits::new(d);
    let prof = b.u(3)?;
    b.skip(1)?;
    let reduced = b.flag()?;
    let mut dm_present = false;
    let mut bdl = 0u32;
    if reduced {
        b.skip(5)?;
    } else {
        if b.flag()? {
            let n = b.u(32)?;
            let s = b.u(32)?;
            if b.flag()? {
                let mut z = 0;
                while b.bit()? == 0 {
                    z += 1;
                    if z > 32 {
                        return None;
                    }
                }
                b.skip(z)?;
            }
            if n > 0 && s > 0 && t.fps.is_none() {
                t.fps = Some(f64::from(s) / f64::from(n));
            }
            dm_present = b.flag()?;
            if dm_present {
                bdl = b.u(5)? + 1;
                b.skip(32 + 5 + 5)?;
            }
        }
        let idd = b.flag()?;
        let ops = b.u(5)? + 1;
        for _ in 0..ops {
            b.skip(12)?;
            if b.u(5)? > 7 {
                b.skip(1)?;
            }
            if dm_present && b.flag()? {
                b.skip(usize::try_from(2 * bdl + 1).ok()?)?;
            }
            if idd && b.flag()? {
                b.skip(4)?;
            }
        }
    }
    let wb = b.u(4)? + 1;
    let hb = b.u(4)? + 1;
    let w = b.u(wb)?.saturating_add(1);
    let h = b.u(hb)?.saturating_add(1);
    t.width.get_or_insert(w);
    t.height.get_or_insert(h);
    if !reduced && b.flag()? {
        b.skip(7)?;
    }
    b.skip(3)?;
    if !reduced {
        b.skip(4)?;
        let order_hint = b.flag()?;
        if order_hint {
            b.skip(2)?;
        }
        let force_sct = if b.flag()? { 2 } else { b.bit()? };
        if force_sct > 0 && !b.flag()? {
            b.skip(1)?;
        }
        if order_hint {
            b.skip(3)?;
        }
    }
    b.skip(3)?;
    let hbd = b.flag()?;
    let mut bd = if hbd { 10 } else { 8 };
    if prof == 2 && hbd && b.flag()? {
        bd = 12;
    }
    t.bit_depth = Some(bd);
    if prof != 1 {
        b.skip(1)?; // mono_chrome
    }
    if b.flag()? {
        t.primaries = u8::try_from(b.u(8)?).ok();
        t.transfer = u8::try_from(b.u(8)?).ok();
        t.matrix = u8::try_from(b.u(8)?).ok();
    }
    Some(())
}

/// vpcC (VP codec configuration box, version 1).
pub(crate) fn vpcc(body: &[u8], t: &mut Tk) -> Option<()> {
    let mut c = Cur::new(body);
    c.skip(4)?;
    let prof = c.u8()?;
    let level = c.u8()?;
    let x = c.u8()?;
    t.profile = Some(format!("Profile {prof}"));
    t.level = Some(format!("{}.{}", level / 10, level % 10));
    t.bit_depth = Some(x >> 4);
    t.chroma = match (x >> 1) & 7 {
        0 | 1 => Some("4:2:0"),
        2 => Some("4:2:2"),
        3 => Some("4:4:4"),
        _ => None,
    };
    t.primaries = Some(c.u8()?);
    t.transfer = Some(c.u8()?);
    t.matrix = Some(c.u8()?);
    Some(())
}

/// Matroska VP9 CodecPrivate (feature list: 1 profile, 2 level, 3 bit depth, 4 chroma).
pub(crate) fn vp9_private(b: &[u8], t: &mut Tk) {
    let mut c = Cur::new(b);
    for _ in 0..8 {
        let (Some(id), Some(len)) = (c.u8(), c.u8()) else {
            break;
        };
        let Some(v) = c.take(usize::from(len)) else {
            break;
        };
        let x = v.first().copied().unwrap_or(0);
        match id {
            1 => t.profile = Some(format!("Profile {x}")),
            2 => t.level = Some(format!("{}.{}", x / 10, x % 10)),
            3 => t.bit_depth = Some(x),
            _ => {}
        }
    }
}

/// VP9 uncompressed header of a key frame: profile, bit depth, chroma, colour space, size.
pub(crate) fn vp9_frame(f: &[u8], t: &mut Tk) -> Option<()> {
    let mut b = Bits::new(f);
    if b.u(2)? != 2 {
        return None;
    }
    let lo = b.u(1)?;
    let hi = b.u(1)?;
    let prof = (hi << 1) | lo;
    if prof == 3 {
        b.skip(1)?;
    }
    if b.flag()? {
        return None; // show_existing_frame
    }
    let key = !b.flag()?;
    b.skip(2)?;
    if !key {
        return None;
    }
    if b.u(24)? != 0x49_83_42 {
        return None;
    }
    let mut bd = 8;
    if prof >= 2 {
        bd = if b.flag()? { 12 } else { 10 };
    }
    let cs = b.u(3)?;
    let mut chroma = "4:2:0";
    if cs != 7 {
        b.skip(1)?;
        if prof == 1 || prof == 3 {
            let sx = b.u(1)?;
            let sy = b.u(1)?;
            chroma = match (sx, sy) {
                (1, 1) => "4:2:0",
                (1, 0) => "4:2:2",
                _ => "4:4:4",
            };
            b.skip(1)?;
        }
    } else {
        chroma = "4:4:4";
        if prof == 1 || prof == 3 {
            b.skip(1)?;
        }
    }
    let w = b.u(16)? + 1;
    let h = b.u(16)? + 1;
    t.profile = Some(format!("Profile {prof}"));
    t.bit_depth = Some(bd);
    t.chroma = Some(chroma);
    t.width.get_or_insert(w);
    t.height.get_or_insert(h);
    Some(())
}

/// Dolby Vision decoder configuration record (dvcC / dvvC / dvwC, TS descriptor 0xB0 body).
pub(crate) fn dovi(b: &[u8], t: &mut Tk) -> Option<()> {
    let mut c = Cur::new(b);
    let _maj = c.u8()?;
    let _min = c.u8()?;
    let x = c.u16()?;
    let compat = c.u8().map(|v| v >> 4);
    let profile = u8::try_from(x >> 9).ok()?;
    if profile > 20 {
        return None;
    }
    t.dv = Some(DolbyVision {
        profile,
        level: u8::try_from((x >> 3) & 0x3f).ok()?,
        compatibility: compat,
        rpu: (x >> 2) & 1 == 1,
        enhancement_layer: (x >> 1) & 1 == 1,
        base_layer: x & 1 == 1,
    });
    Some(())
}

/// ISO-BMFF `colr` (nclx / nclc): primaries / transfer / matrix.
pub(crate) fn colr(b: &[u8], t: &mut Tk) -> Option<()> {
    let mut c = Cur::new(b);
    let ty = c.take(4)?;
    if ty == b"nclx" || ty == b"nclc" {
        t.primaries = u8::try_from(c.u16()?).ok();
        t.transfer = u8::try_from(c.u16()?).ok();
        t.matrix = u8::try_from(c.u16()?).ok();
    }
    Some(())
}

/// MPEG-4 Part 2: profile from the VOS header (or the VOL object type) in the first frame.
pub(crate) fn mpeg4_profile(f: &[u8], t: &mut Tk) -> Option<()> {
    const NAMES: [&str; 16] = [
        "Simple Profile",
        "Simple Scalable Profile",
        "Core Profile",
        "Main Profile",
        "N-bit Profile",
        "Scalable Texture Profile",
        "Simple Face Animation Profile",
        "Basic Animated Texture Profile",
        "Hybrid Profile",
        "Advanced Real Time Simple Profile",
        "Core Scalable Profile",
        "Advanced Coding Profile",
        "Advanced Core Profile",
        "Advanced Scalable Texture Profile",
        "Simple Studio Profile",
        "Advanced Simple Profile",
    ];
    let lim = f.get(..f.len().min(4096))?;
    t.bit_depth.get_or_insert(8);
    t.chroma.get_or_insert("4:2:0");
    if let Some(i) = memchr::memmem::find(lim, &[0, 0, 1, 0xb0]) {
        let pl = *lim.get(i + 4)?;
        t.profile = NAMES.get(usize::from(pl >> 4)).map(|s| s.to_string());
        t.level = Some(format!("{}", pl & 0xf));
        return Some(());
    }
    // VOL: 00 00 01 2x, random_accessible_vol(1), video_object_type_indication(8).
    let i = lim.windows(4).position(|w| {
        w.get(..3) == Some(&[0, 0, 1]) && w.get(3).is_some_and(|b| b & 0xf0 == 0x20)
    })?;
    let mut b = Bits::new(lim.get(i + 4..)?);
    b.skip(1)?;
    t.profile = match b.u(8)? {
        1 => NAMES.first().map(|s| s.to_string()),
        17 => NAMES.get(15).map(|s| s.to_string()),
        _ => None,
    };
    Some(())
}

// ---------------------------------------------------------------- MPEG-1 / MPEG-2 video

/// MPEG-1/2 video elementary stream (ISO/IEC 13818-2 6.2.2): size, frame rate, profile and
/// level, chroma and colour from the sequence header and its extensions. Returns true once a
/// sequence header was parsed and a picture start (0x00) follows, i.e. the extensions that
/// precede the first picture were seen.
pub(crate) fn mpeg2_es(buf: &[u8], t: &mut Tk) -> bool {
    const RATES: [(f64, f64); 9] = [
        (0.0, 1.0),
        (24000.0, 1001.0),
        (24.0, 1.0),
        (25.0, 1.0),
        (30000.0, 1001.0),
        (30.0, 1.0),
        (50.0, 1.0),
        (60000.0, 1001.0),
        (60.0, 1.0),
    ];
    let mut seq = false;
    let mut rate = None;
    for unit in annexb(buf) {
        let mut b = Bits::new(unit);
        match b.u(8) {
            Some(0xB3) => {
                let (Some(w), Some(h), Some(_aspect), Some(code)) =
                    (b.u(12), b.u(12), b.u(4), b.u(4))
                else {
                    continue;
                };
                let Some(&r) = RATES.get(code as usize).filter(|_| code > 0) else {
                    continue;
                };
                if w == 0 || h == 0 {
                    continue;
                }
                seq = true;
                rate = Some(r);
                t.width = Some(w);
                t.height = Some(h);
                t.bit_depth = Some(8);
                t.chroma = chroma_name(1);
                t.fps = Some(r.0 / r.1);
            }
            Some(0xB5) if seq => match b.u(4) {
                Some(1) => {
                    let (Some(pl), Some(_prog), Some(cf), Some(hx), Some(vx)) =
                        (b.u(8), b.u(1), b.u(2), b.u(2), b.u(2))
                    else {
                        continue;
                    };
                    // bit_rate_ext, marker, vbv_buffer_size_ext, low_delay, then the frame
                    // rate extension (its trailing zero bits may be cut with the unit).
                    let _ = (b.u(12), b.u(1), b.u(8), b.u(1));
                    let (n, d) = (b.u(2).unwrap_or(0), b.u(5).unwrap_or(0));
                    if pl & 0x80 != 0 {
                        // Escape: the 4:2:2 and multi-view profiles.
                        t.profile = Some(
                            match pl & 0x0f {
                                2 | 5 => "4:2:2",
                                10 | 11 | 13 | 14 => "Multiview",
                                _ => "Escape",
                            }
                            .into(),
                        );
                    } else {
                        t.profile = Some(
                            match (pl >> 4) & 7 {
                                1 => "High",
                                2 => "Spatially Scalable",
                                3 => "SNR Scalable",
                                4 => "Main",
                                5 => "Simple",
                                _ => "Reserved",
                            }
                            .into(),
                        );
                        t.level = match pl & 0x0f {
                            4 => Some("High".into()),
                            6 => Some("High 1440".into()),
                            8 => Some("Main".into()),
                            10 => Some("Low".into()),
                            _ => None,
                        };
                    }
                    if let Some(c) = chroma_name(cf).filter(|_| cf > 0) {
                        t.chroma = Some(c);
                    }
                    if let (Some(w), Some(h)) = (t.width, t.height) {
                        t.width = Some(w | (hx << 12));
                        t.height = Some(h | (vx << 12));
                    }
                    if let Some(r) = rate {
                        t.fps = Some(r.0 * f64::from(n + 1) / (r.1 * f64::from(d + 1)));
                    }
                }
                Some(2) => {
                    // Sequence display extension: video_format, colour_description.
                    let _ = b.u(3);
                    if b.flag() == Some(true)
                        && let (Some(p), Some(tr), Some(m)) = (b.u(8), b.u(8), b.u(8))
                    {
                        t.primaries = u8::try_from(p).ok();
                        t.transfer = u8::try_from(tr).ok();
                        t.matrix = u8::try_from(m).ok();
                    }
                }
                _ => {}
            },
            Some(0x00) if seq => return true,
            _ => {}
        }
    }
    false
}

// ---------------------------------------------------------------- ProRes

/// ProRes frame header (SMPTE RDD 36 5.1): frame size, `icpf`, header size, version, creator,
/// width, height, then the chroma format in the top bits of the flags byte. 4:2:2 is decoded
/// as 10 bit, 4:4:4 as 12 bit (what FFmpeg's decoder outputs).
/// Matroska stores the frame without its first 8 bytes (size and `icpf`).
pub(crate) fn prores_frame(f: &[u8], t: &mut Tk) {
    let body = if f.get(4..8) == Some(b"icpf") {
        f.get(8..).unwrap_or(&[])
    } else {
        f
    };
    let mut c = Cur::new(body);
    let (Some(hdr), Some(ver), Some(_creator), Some(w), Some(h), Some(flags)) =
        (c.u16(), c.u16(), c.take(4), c.u16(), c.u16(), c.u8())
    else {
        return;
    };
    if !(20..=512).contains(&hdr) || ver > 1 {
        return;
    }
    if w > 0 && h > 0 {
        t.width = Some(u32::from(w));
        t.height = Some(u32::from(h));
    }
    let (chroma, depth) = if flags & 0xC0 == 0xC0 {
        (3, 12)
    } else {
        (2, 10)
    };
    t.chroma = chroma_name(chroma);
    t.bit_depth = Some(depth);
}

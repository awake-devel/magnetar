//! MPEG-TS / M2TS (188, 192 and 204-byte packets): PAT → PMT (stream types, ISO 639
//! languages, registration / AC-3 / E-AC-3 / DTS / DVB descriptors, Dolby Vision descriptor
//! 0xB0), then the elementary streams from the head: SPS (+ HEVC prefix SEIs: mastering, CLL,
//! HDR10+) from the first video access unit, the first audio frames (TrueHD major sync,
//! E-AC-3 Atmos flag, DTS core + extension substream, AAC ADTS, MPEG audio). Duration from
//! the first PTS and the largest PTS in the tail.
//!
//! The head scan needs contiguous bytes from offset 0: it asks for the next chunk (doubling,
//! from 512 KiB) until every stream is done or the head budget (4 MiB) is spent. The tail is
//! asked for on the first pass.

use super::src::Src;
use super::{Kind, Parsed, Tk, audio, codec, lang};
use crate::types::Caps;

const TAIL: u64 = 128 * 1024;
const HEAD_MAX: u64 = 4 << 20;
const VIDEO_BUF: usize = 1 << 20;
const AUDIO_BUF: usize = 96 * 1024;

/// Duration of one access unit, for the end of the last one.
fn frame_secs(t: &Tk) -> f64 {
    let sr = f64::from(t.sample_rate.unwrap_or(48000).max(1));
    match (t.kind, t.codec.as_str()) {
        (Kind::Video, _) => t.fps.filter(|f| *f > 0.0).map_or(0.0, |f| 1.0 / f),
        (_, "ac3" | "eac3") => 1536.0 / sr,
        (_, "truehd" | "mlp") => 40.0 / 48000.0,
        (_, "dts") => 512.0 / sr,
        (_, "aac") => 1024.0 / sr,
        (_, "mp2" | "mp3") => 1152.0 / sr,
        _ => 0.0,
    }
}

/// Packet size and prefix (M2TS has a 4-byte timestamp before each packet).
pub(crate) fn packet_size(h: &[u8]) -> Option<(usize, usize)> {
    let sync = |o: usize| h.get(o) == Some(&0x47);
    for (ps, pre) in [(188usize, 0usize), (192, 4), (204, 0)] {
        if sync(pre) && sync(pre + ps) && (h.len() < pre + 2 * ps + 1 || sync(pre + 2 * ps)) {
            return Some((ps, pre));
        }
    }
    None
}

struct Es {
    pid: u16,
    tk: Tk,
    buf: Vec<u8>,
    done: bool,
    first_pts: Option<u64>,
    started: bool,
    tried_at: usize,
}

/// (pid, payload_unit_start, payload) of one 188-byte packet.
fn payload(pkt: &[u8]) -> Option<(u16, bool, &[u8])> {
    if pkt.len() < 188 || pkt.first() != Some(&0x47) {
        return None;
    }
    let b1 = *pkt.get(1)?;
    let pusi = b1 & 0x40 != 0;
    let pid = (u16::from(b1 & 0x1f) << 8) | u16::from(*pkt.get(2)?);
    let afc = (pkt.get(3)? >> 4) & 3;
    let mut o = 4usize;
    if afc & 2 != 0 {
        o += 1 + usize::from(*pkt.get(4)?);
    }
    if afc & 1 == 0 || o >= 188 {
        return Some((pid, pusi, &[]));
    }
    Some((pid, pusi, pkt.get(o..188)?))
}

/// PES header: (PTS, payload offset).
fn pes(p: &[u8]) -> Option<(Option<u64>, usize)> {
    if p.get(..3)? != [0, 0, 1] {
        return None;
    }
    let sid = *p.get(3)?;
    if matches!(sid, 0xbc | 0xbe | 0xbf | 0xf0 | 0xf1 | 0xff | 0xf2 | 0xf8) {
        return Some((None, 6));
    }
    let flags = *p.get(7)?;
    let hl = 9 + usize::from(*p.get(8)?);
    let pts = if flags & 0x80 != 0 {
        let b = p.get(9..14)?;
        let g = |i: usize| u64::from(b.get(i).copied().unwrap_or(0));
        Some(((g(0) >> 1) & 7) << 30 | g(1) << 22 | (g(2) >> 1) << 15 | g(3) << 7 | g(4) >> 1)
    } else {
        None
    };
    Some((pts, hl))
}

/// Table section body after the pointer field (`table_id` first), clipped to its length.
fn section(pl: &[u8]) -> Option<&[u8]> {
    let ptr = usize::from(*pl.first()?);
    let s = pl.get(1 + ptr..)?;
    let len = (usize::from(s.get(1)? & 0xf) << 8) | usize::from(*s.get(2)?);
    // Drop the CRC.
    s.get(..(3 + len).saturating_sub(4).min(s.len()))
}

/// A table section (`table_id` first) once all of it is there, without its CRC; `None` while
/// it continues in later packets. Sections are at most 1024 bytes (ISO 13818-1 PSI).
fn whole_section(s: &[u8]) -> Option<&[u8]> {
    let len = (usize::from(s.get(1)? & 0xf) << 8) | usize::from(*s.get(2)?);
    let total = (3 + len).min(1024);
    s.get(..total.saturating_sub(4).max(3))
        .filter(|_| s.len() >= total)
}

/// Parse the PMT section in `buf` once it is whole, then empty `buf`: `Some(true)` for a PMT
/// (`table_id` 2), `Some(false)` for another table, `None` while it continues.
fn take_section(buf: &mut Vec<u8>, es: &mut Vec<Es>) -> Option<bool> {
    let s = whole_section(buf)?;
    let is_pmt = s.first() == Some(&2);
    if is_pmt {
        pmt(s, es);
    }
    buf.clear();
    Some(is_pmt)
}

fn stream(st: u8) -> (Kind, &'static str) {
    match st {
        0x01 => (Kind::Video, "mpeg1video"),
        0x02 => (Kind::Video, "mpeg2video"),
        0x10 => (Kind::Video, "mpeg4"),
        0x1b => (Kind::Video, "h264"),
        0x20 => (Kind::Video, "h264"),
        0x24 => (Kind::Video, "hevc"),
        0x33 => (Kind::Video, "vvc"),
        0xea => (Kind::Video, "vc1"),
        0x03 | 0x04 => (Kind::Audio, "mp2"),
        0x0f => (Kind::Audio, "aac"),
        0x11 => (Kind::Audio, "aac_latm"),
        0x80 => (Kind::Audio, "pcm_bluray"),
        0x81 => (Kind::Audio, "ac3"),
        0x82 | 0x85 | 0x86 | 0xa2 => (Kind::Audio, "dts"),
        0x83 => (Kind::Audio, "truehd"),
        0x84 | 0x87 | 0xa1 => (Kind::Audio, "eac3"),
        0x90 => (Kind::Sub, "hdmv_pgs_subtitle"),
        0x92 => (Kind::Sub, "hdmv_text_subtitle"),
        _ => (Kind::Other, "data"),
    }
}

fn pmt(sct: &[u8], es: &mut Vec<Es>) {
    if sct.first() != Some(&2) || sct.len() < 12 {
        return;
    }
    let pil = (usize::from(sct.get(10).copied().unwrap_or(0) & 0xf) << 8)
        | usize::from(sct.get(11).copied().unwrap_or(0));
    let mut i = 12 + pil;
    while i + 5 <= sct.len() && es.len() < 64 {
        let (Some(&st), Some(&p1), Some(&p2), Some(&l1), Some(&l2)) = (
            sct.get(i),
            sct.get(i + 1),
            sct.get(i + 2),
            sct.get(i + 3),
            sct.get(i + 4),
        ) else {
            break;
        };
        let pid = (u16::from(p1 & 0x1f) << 8) | u16::from(p2);
        let il = (usize::from(l1 & 0xf) << 8) | usize::from(l2);
        let desc = sct.get(i + 5..(i + 5 + il).min(sct.len())).unwrap_or(&[]);
        let (kind, c) = stream(st);
        let mut t = Tk {
            kind,
            id: u64::from(pid),
            codec: c.to_string(),
            ..Tk::default()
        };
        if st == 0x85 {
            t.profile = Some("DTS-HD HRA".into());
        } else if st == 0x86 {
            t.profile = Some("DTS-HD MA".into());
        }
        let mut j = 0usize;
        while j + 2 <= desc.len() {
            let (Some(&tag), Some(&l)) = (desc.get(j), desc.get(j + 1)) else {
                break;
            };
            let l = usize::from(l);
            let body = desc.get(j + 2..(j + 2 + l).min(desc.len())).unwrap_or(&[]);
            match tag {
                0x0a if body.len() >= 3 => {
                    t.language = lang(&String::from_utf8_lossy(body.get(..3).unwrap_or(&[])))
                }
                0x59 if body.len() >= 3 => {
                    t.kind = Kind::Sub;
                    t.codec = "dvb_subtitle".into();
                    t.language = lang(&String::from_utf8_lossy(body.get(..3).unwrap_or(&[])));
                }
                0x56 => {
                    t.kind = Kind::Sub;
                    t.codec = "dvb_teletext".into();
                }
                0x6a => (t.kind, t.codec) = (Kind::Audio, "ac3".into()),
                0x7a => (t.kind, t.codec) = (Kind::Audio, "eac3".into()),
                0x7b => (t.kind, t.codec) = (Kind::Audio, "dts".into()),
                0x05 if st == 0x06 => match body.get(..4) {
                    Some(b"Opus") => (t.kind, t.codec) = (Kind::Audio, "opus".into()),
                    Some(b"AC-3") => (t.kind, t.codec) = (Kind::Audio, "ac3".into()),
                    Some(b"EAC3") => (t.kind, t.codec) = (Kind::Audio, "eac3".into()),
                    Some(b"DTS1" | b"DTS2" | b"DTS3") => {
                        (t.kind, t.codec) = (Kind::Audio, "dts".into())
                    }
                    _ => {}
                },
                0xb0 => {
                    let _ = codec::dovi(body, &mut t);
                }
                _ => {}
            }
            j += 2 + l;
        }
        if !es.iter().any(|e| e.pid == pid) {
            es.push(Es {
                pid,
                done: t.kind == Kind::Sub || t.kind == Kind::Other,
                tk: t,
                buf: Vec::new(),
                first_pts: None,
                started: false,
                tried_at: 0,
            });
        }
        i += 5 + il;
    }
}

/// Feeds an ES buffer to the right first-frame parser; marks the stream done when decided.
fn try_parse(e: &mut Es, last: bool) {
    if e.done {
        return;
    }
    let n = e.buf.len();
    let t = &mut e.tk;
    match t.kind {
        Kind::Video => {
            if n < e.tried_at + 4096 && !last && n < VIDEO_BUF {
                return;
            }
            e.tried_at = n;
            let slice = match t.codec.as_str() {
                "hevc" => codec::hevc_annexb(&e.buf, t),
                "mpeg2video" | "mpeg1video" => codec::mpeg2_es(&e.buf, t),
                "h264" => {
                    let mut seen_slice = false;
                    for nal in codec::annexb(&e.buf) {
                        match nal.first().map(|h| h & 0x1f) {
                            Some(7) if t.width.is_none() => {
                                let _ = codec::h264_sps(nal, t);
                            }
                            Some(1 | 5) => {
                                seen_slice = true;
                                break;
                            }
                            _ => {}
                        }
                    }
                    seen_slice
                }
                _ => true,
            };
            if (slice && t.width.is_some()) || n >= VIDEO_BUF || last {
                e.done = true;
                t.frame_done = slice;
            }
        }
        Kind::Audio => {
            // Enough for one full frame set: an E-AC-3 independent frame and its dependents,
            // a TrueHD access unit behind interleaved AC-3 frames, a DTS core frame plus its
            // extension substream (DTS-HD MA frames run to ~10 KiB).
            let need = match t.codec.as_str() {
                "truehd" | "ac3" | "eac3" => 4 * 1024,
                "dts" => 12 * 1024,
                _ => 256,
            };
            if n < need && !last && n < AUDIO_BUF {
                return;
            }
            let a = match t.codec.as_str() {
                "truehd" => audio::truehd(&e.buf),
                "dts" => audio::dts(&e.buf),
                "ac3" | "eac3" => audio::ac3(&e.buf),
                "aac" => audio::adts(&e.buf),
                "mp2" | "mp3" => audio::mpa(&e.buf),
                _ => None,
            };
            match a {
                Some(a) => {
                    t.apply_audio(a);
                    t.frame_done = true;
                    e.done = true;
                }
                None if n >= AUDIO_BUF || last || !t.wants_audio_frame() => e.done = true,
                None => {}
            }
        }
        _ => e.done = true,
    }
}

pub(crate) fn probe(src: &Src, caps: &Caps) -> Option<Parsed> {
    let size = src.len();
    let (ps, pre) = packet_size(src.peek(0, 1024)?)?;
    let mut out = Parsed {
        container: if ps == 192 { "m2ts" } else { "ts" },
        ..Parsed::default()
    };
    let budget = HEAD_MAX.min(caps.max_bytes / 2).max(512 * 1024);
    let head = src.avail(0, usize::try_from(budget).unwrap_or(usize::MAX));
    let mut pmt_pid: Option<u16> = None;
    let mut es: Vec<Es> = Vec::new();
    let mut have_pmt = false;
    let mut pmt_buf: Vec<u8> = Vec::new();
    let mut pmt_cc: Option<u8> = None;
    let mut pmt_stalled = false;
    let mut all_done = false;
    for pkt in head.chunks_exact(ps) {
        let Some((pid, pusi, pl)) = pkt.get(pre..).and_then(payload) else {
            continue;
        };
        if pid == 0 && pusi && pmt_pid.is_none() {
            if let Some(s) = section(pl).filter(|s| s.first() == Some(&0)) {
                let mut i = 8;
                while let Some(e) = s.get(i..i + 4) {
                    let prog = u16::from_be_bytes([e.first().copied()?, e.get(1).copied()?]);
                    if prog != 0 {
                        pmt_pid = Some(
                            u16::from_be_bytes([e.get(2).copied()?, e.get(3).copied()?]) & 0x1fff,
                        );
                        break;
                    }
                    i += 4;
                }
            }
            continue;
        }
        if Some(pid) == pmt_pid && !have_pmt {
            // A PMT longer than one packet (many audio and subtitle streams, each with its
            // descriptors) continues in the next packets of its PID (D120).
            if pl.is_empty() {
                continue;
            }
            let cc = pkt.get(pre + 3).map_or(0, |b| b & 0xf);
            if pmt_cc == Some(cc) {
                // A duplicate packet (ISO 13818-1 allows one): its bytes are already in.
                continue;
            }
            let next = pmt_cc.map(|c| (c + 1) & 0xf) == Some(cc);
            pmt_cc = Some(cc);
            if pusi {
                let ptr = usize::from(pl.first().copied().unwrap_or(0));
                // The bytes before the pointer end the section in progress (packed PSI).
                if !pmt_buf.is_empty() && !pmt_stalled && next {
                    pmt_buf.extend_from_slice(pl.get(1..(1 + ptr).min(pl.len())).unwrap_or(&[]));
                    if let Some(is_pmt) = take_section(&mut pmt_buf, &mut es) {
                        have_pmt = is_pmt;
                        if have_pmt {
                            continue;
                        }
                    }
                }
                pmt_buf.clear();
                pmt_stalled = false;
                pmt_buf.extend_from_slice(pl.get(1 + ptr..).unwrap_or(&[]));
            } else if !pmt_buf.is_empty() && !pmt_stalled {
                if next {
                    pmt_buf.extend_from_slice(pl);
                } else {
                    // A lost packet: the section cannot be completed; keep its first part for
                    // the fallback below until the next one starts.
                    pmt_stalled = true;
                }
            }
            if !pmt_stalled && let Some(is_pmt) = take_section(&mut pmt_buf, &mut es) {
                have_pmt = is_pmt;
            }
            continue;
        }
        let Some(e) = es.iter_mut().find(|e| e.pid == pid) else {
            continue;
        };
        if pusi {
            if let Some((pts, hl)) = pes(pl) {
                if e.first_pts.is_none() {
                    e.first_pts = pts;
                }
                if !e.done {
                    e.started = true;
                    e.buf.extend_from_slice(pl.get(hl..).unwrap_or(&[]));
                }
            }
        } else if e.started && !e.done {
            e.buf.extend_from_slice(pl);
        }
        try_parse(e, false);
        if have_pmt && es.iter().all(|e| e.done && e.first_pts.is_some()) {
            all_done = true;
            break;
        }
    }
    // A PMT whose continuation is not in the known head: the streams of its first part.
    if !have_pmt
        && pmt_buf.len() >= 12
        && let (Some(&l1), Some(&l2)) = (pmt_buf.get(1), pmt_buf.get(2))
    {
        let len = (usize::from(l1 & 0xf) << 8) | usize::from(l2);
        let s = pmt_buf.get(..(3 + len).min(pmt_buf.len())).unwrap_or(&[]);
        pmt(s, &mut es);
        have_pmt = s.first() == Some(&2);
        src.note("ts: PMT cut short");
    }
    if !have_pmt {
        src.note("ts: no PMT in the known head");
    }
    let reached_eof = head.len() as u64 >= size;
    if !all_done && !reached_eof {
        if (head.len() as u64) < budget {
            let chunk = (head.len() as u64).max(512 * 1024);
            src.want(head.len() as u64, chunk.min(budget - head.len() as u64));
        } else {
            let open: Vec<String> = es
                .iter()
                .filter(|e| !e.done || e.first_pts.is_none())
                .map(|e| format!("{:#x} {}", e.pid, e.tk.codec))
                .collect();
            src.note(format!(
                "ts: head budget spent with streams unfinished: {}",
                open.join(", ")
            ));
        }
    }
    for e in &mut es {
        if !e.done && (reached_eof || head.len() as u64 >= budget) {
            try_parse(e, true);
        }
    }
    // Duration as FFmpeg estimates it: from the earliest first PTS of any stream to the end
    // (largest PTS in the tail plus one frame) of the stream that ends last.
    let start = es.iter().filter_map(|e| e.first_pts).min_by_key(|p| *p);
    if let Some(first) = start {
        let tail_start = size.saturating_sub(TAIL);
        let aligned = tail_start.div_ceil(ps as u64).saturating_mul(ps as u64);
        if aligned < size
            && let Some(tail) = src.get(aligned, size - aligned)
        {
            let mut end: Option<f64> = None;
            for pkt in tail.chunks_exact(ps) {
                let Some((p, true, pl)) = pkt.get(pre..).and_then(payload) else {
                    continue;
                };
                let Some(e) = es.iter().find(|e| e.pid == p && e.first_pts.is_some()) else {
                    continue;
                };
                if let Some((Some(pts), _)) = pes(pl) {
                    let rel = pts.wrapping_sub(first) & ((1 << 33) - 1);
                    // A PTS far "before" the start is a wrap artefact or garbage.
                    if rel < (1 << 32) {
                        let t = rel as f64 / 90000.0 + frame_secs(&e.tk);
                        end = Some(end.map_or(t, |x| x.max(t)));
                    }
                }
            }
            match end {
                Some(t) => out.duration = Some(t),
                None => src.note("ts: no timestamp in the tail"),
            }
        }
    }
    out.tracks = es.into_iter().map(|e| e.tk).collect();
    Some(out)
}

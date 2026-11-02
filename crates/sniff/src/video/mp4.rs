//! ISO-BMFF / QuickTime: box headers only, `moov` → `trak` → `mdia` → `minf` → `stbl`
//! (`stsd`, the head of `stsz` and `stco` / `co64`); the big sample tables are skipped by their
//! headers. `moov` at the end is found from the `mdat` size. Fragmented files: `mvex` / `mehd`,
//! the first `moof` (frame rate, first samples) and `mfra` / `tfra` + the last `moof` for the
//! duration. First samples of HEVC / AV1 / VP9 video and E-AC-3 / TrueHD / DTS / AC-3 audio
//! are read for HDR SEI and premium-audio flags.
//!
//! Hardening against the prototype: `tfra` entries are located from their declared sizes and
//! checked against the box, track IDs are matched by `tkhd` track_ID (not by order), Macintosh
//! language codes use FFmpeg's full table, and every count, size and hop is capped.

use super::bits::Cur;
use super::src::Src;
use super::{AudioFeature, Kind, Parsed, Tk, audio, codec, lang};

/// Largest `stsd` / `moof` / `tkhd` body read.
const MAX_BODY: u64 = 1 << 20;
const MAX_CHILDREN: usize = 1024;
const VIDEO_SAMPLE: u64 = 64 * 1024;
const AUDIO_SAMPLE: u64 = 32 * 1024;

#[derive(Clone, Copy, Debug)]
struct Bx {
    ty: [u8; 4],
    body: u64,
    end: u64,
}

impl Bx {
    fn len(&self) -> u64 {
        self.end.saturating_sub(self.body)
    }
}

/// Box header at `off` inside `[.., lim)`. `None` when unknown (need recorded) or invalid.
fn header(src: &Src, off: u64, lim: u64) -> Option<Bx> {
    if off.saturating_add(8) > lim {
        return None;
    }
    let h = src.get(off, 16.min(lim - off))?;
    let mut c = Cur::new(h);
    let sz = u64::from(c.u32()?);
    let ty: [u8; 4] = c.take(4)?.try_into().ok()?;
    let (body, end) = match sz {
        0 => (off + 8, lim),
        1 => (off + 16, off.checked_add(c.u64()?)?),
        n if n < 8 => return None,
        n => (off + 8, off.checked_add(n)?),
    };
    if end > lim || body > end {
        return None;
    }
    Some(Bx { ty, body, end })
}

/// Child box headers of `[from, end)`; stops at the first unknown or invalid header.
fn children(src: &Src, mut off: u64, end: u64) -> Vec<Bx> {
    let mut v = Vec::new();
    while off.saturating_add(8) <= end && v.len() < MAX_CHILDREN {
        let Some(b) = header(src, off, end) else {
            break;
        };
        off = b.end;
        v.push(b);
    }
    v
}

fn body<'a>(src: &Src<'a>, b: &Bx, max: u64) -> Option<&'a [u8]> {
    src.get(b.body, b.len().min(max))
}

/// QuickTime Macintosh language codes (Apple `langXxx` numbering, FFmpeg's three-letter codes;
/// 95..=127 are unassigned).
const MAC_LANG: &[&str] = &[
    "eng", "fra", "ger", "ita", "dut", "sve", "spa", "dan", "por", "nor", "heb", "jpn", "ara",
    "fin", "gre", "ice", "mlt", "tur", "hr ", "chi", "urd", "hin", "tha", "kor", "lit", "pol",
    "hun", "est", "lav", "", "fo ", "", "rus", "chi", "", "iri", "alb", "ron", "ces", "slk", "slv",
    "yid", "sr ", "mac", "bul", "ukr", "bel", "uzb", "kaz", "aze", "aze", "arm", "geo", "mol",
    "kir", "tgk", "tuk", "mon", "", "pus", "kur", "kas", "snd", "tib", "nep", "san", "mar", "ben",
    "asm", "guj", "pa ", "ori", "mal", "kan", "tam", "tel", "", "bur", "khm", "lao", "vie", "ind",
    "tgl", "may", "may", "amh", "tir", "orm", "som", "swa", "", "run", "", "mlg", "epo", "", "",
    "", "", "", "", "", "", "", "", "", "", "", "", "", "", "", "", "", "", "", "", "", "", "", "",
    "", "", "", "", "", "", "", "wel", "baq", "cat", "lat", "que", "grn", "aym", "tat", "uig",
    "dzo", "jav",
];

fn mdhd_lang(code: u16) -> Option<String> {
    if code < 0x400 {
        return MAC_LANG
            .get(usize::from(code))
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(str::to_string);
    }
    if code == 0x7fff {
        return None;
    }
    let ch = |i: u16| char::from(((code >> i) & 0x1f) as u8 + 0x60);
    let s: String = [ch(10), ch(5), ch(0)].iter().collect();
    lang(&s)
}

/// Per-track state the walker needs besides the public fields.
#[derive(Default)]
struct Trak {
    tk: Tk,
    timescale: u64,
    mdur: u64,
    samples: u64,
    first_size: Option<u64>,
    first_off: Option<u64>,
    nal_len: u8,
    /// `dec3` present but without the Atmos extension: the frame decides.
    complete: bool,
}

pub(crate) fn probe(src: &Src) -> Option<Parsed> {
    let size = src.len();
    let mut out = Parsed {
        container: "mp4",
        ..Parsed::default()
    };
    let mut off = 0u64;
    let mut moov: Option<Bx> = None;
    let mut after_moov = None;
    for _ in 0..1024 {
        let Some(b) = header(src, off, size) else {
            break;
        };
        match &b.ty {
            b"ftyp" => {
                if let Some(d) = src.get(b.body, 4) {
                    let brand = String::from_utf8_lossy(d).trim().to_string();
                    if brand == "qt" {
                        out.container = "mov";
                    }
                    out.brand = Some(brand);
                }
            }
            b"moov" => {
                moov = Some(b);
                after_moov = Some(b.end);
                break;
            }
            _ => {}
        }
        if b.end >= size {
            break;
        }
        off = b.end;
    }
    let Some(moov) = moov else {
        if !src.wants_more() {
            src.note("mp4: no moov box");
        }
        return Some(out);
    };
    if out.brand.is_none() {
        out.container = "mov";
    }
    let mut timescale = 1000u64;
    let mut mvhd_dur = 0u64;
    let mut frag_dur = None;
    let mut traks: Vec<Trak> = Vec::new();
    let mut trex: Vec<(u32, u32, u32)> = Vec::new(); // (track_ID, default duration, default size)
    for b in children(src, moov.body, moov.end) {
        match &b.ty {
            b"mvhd" => {
                if let Some(d) = body(src, &b, 32) {
                    let mut c = Cur::new(d);
                    let v = c.u8();
                    let ok = (|| {
                        c.skip(3)?;
                        if v? == 1 {
                            c.skip(16)?;
                            timescale = u64::from(c.u32()?);
                            mvhd_dur = c.u64()?;
                        } else {
                            c.skip(8)?;
                            timescale = u64::from(c.u32()?);
                            mvhd_dur = u64::from(c.u32()?);
                        }
                        Some(())
                    })();
                    let _ = ok;
                }
            }
            b"mvex" => {
                out.fragmented = true;
                for m in children(src, b.body, b.end) {
                    let Some(d) = body(src, &m, 64) else {
                        continue;
                    };
                    let mut c = Cur::new(d);
                    match &m.ty {
                        b"mehd" => {
                            let v = c.u8();
                            let _ = c.skip(3);
                            frag_dur = if v == Some(1) {
                                c.u64()
                            } else {
                                c.u32().map(u64::from)
                            };
                        }
                        b"trex" => {
                            let _ = c.skip(4);
                            if let (Some(id), Some(_), Some(dur), Some(sz)) =
                                (c.u32(), c.u32(), c.u32(), c.u32())
                            {
                                trex.push((id, dur, sz));
                            }
                        }
                        _ => {}
                    }
                }
            }
            b"trak" if traks.len() < 64 => traks.push(trak(src, &b)),
            _ => {}
        }
    }
    let d = frag_dur.filter(|d| *d > 0).unwrap_or(mvhd_dur);
    if d > 0 && timescale > 0 {
        out.duration = Some(d as f64 / timescale as f64);
    }
    if out.fragmented {
        fragments(src, after_moov.unwrap_or(moov.end), &mut traks, &trex);
        if frag_dur.filter(|d| *d > 0).is_none()
            && let Some(dur) = mfra_duration(src, &traks, &trex)
        {
            out.duration = Some(dur);
        }
    }
    for t in &mut traks {
        first_sample(src, t);
    }
    out.tracks = traks.into_iter().map(|t| t.tk).collect();
    Some(out)
}

fn trak(src: &Src, b: &Bx) -> Trak {
    let mut t = Trak {
        nal_len: 4,
        ..Trak::default()
    };
    for x in children(src, b.body, b.end) {
        match &x.ty {
            b"tkhd" => {
                let Some(d) = body(src, &x, 104) else {
                    continue;
                };
                let mut c = Cur::new(d);
                let v = c.u8().unwrap_or(0);
                let flags = c.u24().unwrap_or(0);
                t.tk.default = flags & 1 == 1;
                let _ = c.skip(if v == 1 { 16 } else { 8 });
                t.tk.id = c.u32().map_or(0, u64::from);
                let n = d.len();
                if n >= 8 {
                    let mut e = Cur::new(d.get(n - 8..).unwrap_or(&[]));
                    let w = e.u32().unwrap_or(0) >> 16;
                    let h = e.u32().unwrap_or(0) >> 16;
                    if w > 0 && h > 0 {
                        t.tk.width = Some(w);
                        t.tk.height = Some(h);
                    }
                }
            }
            b"mdia" => mdia(src, &x, &mut t),
            _ => {}
        }
    }
    if t.tk.kind == Kind::Video && t.timescale > 0 && t.mdur > 0 && t.samples > 0 {
        t.tk.fps = Some(t.samples as f64 * t.timescale as f64 / t.mdur as f64);
    }
    t
}

fn mdia(src: &Src, b: &Bx, t: &mut Trak) {
    let kids = children(src, b.body, b.end);
    // hdlr first: the stsd layout depends on the handler.
    for m in &kids {
        if &m.ty == b"hdlr"
            && let Some(d) = body(src, m, 12)
        {
            t.tk.kind = match d.get(8..12) {
                Some(b"vide") => Kind::Video,
                Some(b"soun") => Kind::Audio,
                Some(b"subt" | b"sbtl" | b"text" | b"clcp") => Kind::Sub,
                _ => Kind::Other,
            };
        }
    }
    for m in &kids {
        match &m.ty {
            b"mdhd" => {
                let Some(d) = body(src, m, 64) else {
                    continue;
                };
                let mut c = Cur::new(d);
                let r = (|| {
                    let v = c.u8()?;
                    c.skip(3)?;
                    if v == 1 {
                        c.skip(16)?;
                        t.timescale = u64::from(c.u32()?);
                        t.mdur = c.u64()?;
                    } else {
                        c.skip(8)?;
                        t.timescale = u64::from(c.u32()?);
                        t.mdur = u64::from(c.u32()?);
                    }
                    Some(c.u16()? & 0x7fff)
                })();
                if let Some(code) = r {
                    t.tk.language = mdhd_lang(code);
                }
            }
            b"elng" if t.tk.language.is_none() => {
                if let Some(d) = body(src, m, 64) {
                    t.tk.language = lang(&super::text(d.get(4..).unwrap_or(&[])));
                }
            }
            b"minf" => {
                for s in children(src, m.body, m.end) {
                    if &s.ty == b"stbl" {
                        stbl(src, &s, t);
                    }
                }
            }
            _ => {}
        }
    }
}

fn stbl(src: &Src, b: &Bx, t: &mut Trak) {
    for x in children(src, b.body, b.end) {
        match &x.ty {
            b"stsd" => {
                if let Some(d) = body(src, &x, MAX_BODY) {
                    stsd(d, t);
                }
            }
            b"stsz" => {
                if let Some(d) = src.get(x.body, 16.min(x.len())) {
                    let mut c = Cur::new(d);
                    let _ = c.skip(4);
                    let fixed = c.u32().unwrap_or(0);
                    t.samples = c.u32().map_or(0, u64::from);
                    t.first_size = if fixed != 0 {
                        Some(u64::from(fixed))
                    } else {
                        c.u32().map(u64::from)
                    };
                }
            }
            b"stz2" => {
                if let Some(d) = src.get(x.body, 12.min(x.len())) {
                    let mut c = Cur::new(d);
                    let _ = c.skip(8);
                    t.samples = c.u32().map_or(0, u64::from);
                }
            }
            b"stco" | b"co64" => {
                if let Some(d) = src.get(x.body, 16.min(x.len())) {
                    let mut c = Cur::new(d);
                    let _ = c.skip(4);
                    if c.u32().unwrap_or(0) > 0 {
                        t.first_off = if &x.ty == b"co64" {
                            c.u64()
                        } else {
                            c.u32().map(u64::from)
                        };
                    }
                }
            }
            _ => {}
        }
    }
}

fn video_codec(fc: &[u8]) -> Option<&'static str> {
    Some(match fc {
        b"avc1" | b"avc2" | b"avc3" | b"avc4" | b"dvav" | b"dva1" => "h264",
        b"hvc1" | b"hev1" | b"dvh1" | b"dvhe" => "hevc",
        b"av01" | b"dav1" => "av1",
        b"vp09" => "vp9",
        b"vp08" => "vp8",
        b"mp4v" => "mpeg4",
        b"mp2v" | b"xdvd" | b"xd5c" => "mpeg2video",
        b"vvc1" | b"vvi1" => "vvc",
        b"apch" | b"apcn" | b"apcs" | b"apco" | b"ap4h" | b"ap4x" => "prores",
        b"jpeg" | b"mjpa" => "mjpeg",
        _ => return None,
    })
}

fn audio_codec(fc: &[u8]) -> Option<&'static str> {
    Some(match fc {
        b"mp4a" => "aac",
        b"ac-3" => "ac3",
        b"ec-3" => "eac3",
        b"ac-4" => "ac4",
        b"Opus" => "opus",
        b"fLaC" => "flac",
        b"alac" => "alac",
        b"dtsc" | b"dtsh" | b"dtsl" | b"dtse" => "dts",
        // DTS-UHD (ETSI TS 103 491, sync 0x40411BF2), not DTS Coherent Acoustics.
        b"dtsx" | b"dtsy" => "dtsuhd",
        b"mlpa" => "truehd",
        b".mp3" => "mp3",
        b"sowt" => "pcm_s16le",
        b"twos" => "pcm_s16be",
        b"lpcm" | b"ipcm" | b"in24" | b"fl32" => "pcm",
        _ => return None,
    })
}

fn sub_codec(fc: &[u8]) -> Option<&'static str> {
    Some(match fc {
        b"tx3g" | b"text" => "mov_text",
        b"wvtt" => "webvtt",
        b"stpp" => "ttml",
        b"c608" => "eia_608",
        _ => return None,
    })
}

fn stsd(d: &[u8], tr: &mut Trak) {
    let mut c = Cur::new(d);
    let _ = c.skip(8);
    let (Some(sz), Some(fc)) = (c.u32(), c.take(4)) else {
        return;
    };
    let entry = d
        .get(8..(8usize.saturating_add(sz as usize)).min(d.len()))
        .unwrap_or(&[]);
    let t = &mut tr.tk;
    let fcs = String::from_utf8_lossy(fc).to_string();
    let name = match t.kind {
        Kind::Video => video_codec(fc),
        Kind::Audio => audio_codec(fc),
        Kind::Sub => sub_codec(fc),
        Kind::Other => None,
    };
    t.codec = name.map_or_else(|| format!("?{fcs}"), str::to_string);
    match fc {
        b"dtsl" => t.profile = Some("DTS-HD MA".into()),
        b"dtse" => t.profile = Some("DTS Express".into()),
        // ETSI TS 103 491 Annex E: 'dtsx' carries DTS-UHD profile 2, 'dtsy' profile 3; both
        // are DTS:X streams.
        b"dtsx" => {
            t.profile = Some("DTS:X Profile 2".into());
            t.dts_x = Some(true);
            t.features.push(AudioFeature::DtsX);
        }
        b"dtsy" => {
            t.profile = Some("DTS:X Profile 3".into());
            t.dts_x = Some(true);
            t.features.push(AudioFeature::DtsX);
        }
        _ => {}
    }
    let kid_off = match t.kind {
        Kind::Video => {
            let mut v = Cur::new(entry);
            if v.skip(8 + 24).is_some() {
                let w = v.u16().map_or(0, u32::from);
                let h = v.u16().map_or(0, u32::from);
                if w > 0 && h > 0 {
                    t.width = Some(w);
                    t.height = Some(h);
                }
            }
            8 + 78
        }
        Kind::Audio => {
            let mut a = Cur::new(entry);
            let _ = a.skip(16);
            let ver = a.u16().unwrap_or(0);
            let _ = a.skip(6);
            let ch = a.u16();
            let _ = a.skip(6);
            let sr = a.u32().map(|v| v >> 16);
            if t.channels.is_none() {
                t.channels = ch.and_then(|c| u8::try_from(c).ok()).filter(|c| *c > 0);
            }
            t.sample_rate = sr.filter(|r| *r > 0);
            8 + 28
                + match ver {
                    1 => 16,
                    2 => 36,
                    _ => 0,
                }
        }
        _ => return,
    };
    sample_entry_children(entry.get(kid_off..).unwrap_or(&[]), tr, 0);
}

fn sample_entry_children(mut b: &[u8], tr: &mut Trak, depth: u8) {
    for _ in 0..64 {
        let mut c = Cur::new(b);
        let (Some(bs), Some(ty)) = (c.u32(), c.take(4)) else {
            return;
        };
        let bs = bs as usize;
        if bs < 8 || bs > b.len() {
            return;
        }
        let body = b.get(8..bs).unwrap_or(&[]);
        let t = &mut tr.tk;
        match ty {
            b"avcC" => {
                let _ = codec::avcc(body, t);
            }
            b"hvcC" => {
                if let Some(n) = codec::hvcc(body, t) {
                    tr.nal_len = n;
                }
            }
            b"av1C" => {
                let _ = codec::av1c(body, t);
            }
            b"vpcC" => {
                let _ = codec::vpcc(body, t);
            }
            b"colr" => {
                let _ = codec::colr(body, t);
            }
            b"mdcv" | b"SmDm" => t.mastering = true,
            b"clli" | b"CoLL" => {
                let mut x = Cur::new(if ty == b"CoLL" {
                    body.get(4..).unwrap_or(&[])
                } else {
                    body
                });
                if let (Some(a), Some(f)) = (x.u16(), x.u16()) {
                    t.cll = Some((a, f));
                }
            }
            b"dvcC" | b"dvvC" | b"dvwC" => {
                let _ = codec::dovi(body, t);
            }
            b"dac3" => {
                if let Some(a) = audio::dac3(body) {
                    t.apply_audio(a);
                }
            }
            b"dec3" => {
                if let Some(a) = audio::dec3(body) {
                    let known = a.atmos.is_some();
                    t.apply_audio(a);
                    // The first frame still decides "false" (or overrides a missing extension).
                    tr.complete = known;
                }
            }
            b"esds" => esds(body, t),
            b"wave" if depth == 0 => sample_entry_children(body, tr, 1),
            // Protected sample entry (encv / enca): sinf > frma holds the original format.
            b"sinf" if t.codec.starts_with('?') => {
                // The samples are encrypted: never parse a first frame of them.
                t.frame_done = true;
                let mut k = Cur::new(body);
                while let (Some(ks), Some(kt)) = (k.u32(), k.take(4)) {
                    let Some(kb) = k.take((ks as usize).saturating_sub(8)) else {
                        break;
                    };
                    if kt == b"frma"
                        && let Some(fc) = kb.get(..4)
                        && let Some(name) = match t.kind {
                            Kind::Video => video_codec(fc),
                            Kind::Audio => audio_codec(fc),
                            Kind::Sub => sub_codec(fc),
                            Kind::Other => None,
                        }
                    {
                        t.codec = name.to_string();
                    }
                }
            }
            b"btrt" => {
                let mut x = Cur::new(body);
                let _ = x.skip(8);
                if let Some(avg) = x.u32().filter(|a| *a > 0) {
                    t.bitrate_kbps = Some(u64::from(avg) / 1000);
                }
            }
            _ => {}
        }
        b = b.get(bs..).unwrap_or(&[]);
    }
}

/// `esds`: object type (AAC / MP3 / ...) and the AudioSpecificConfig.
fn esds(b: &[u8], t: &mut Tk) {
    let mut p = 4usize; // version + flags
    let desc = |p: &mut usize| -> Option<(u8, usize)> {
        let tag = *b.get(*p)?;
        *p += 1;
        let mut len = 0usize;
        for _ in 0..4 {
            let x = *b.get(*p)?;
            *p += 1;
            len = (len << 7) | usize::from(x & 0x7f);
            if x & 0x80 == 0 {
                break;
            }
        }
        Some((tag, len))
    };
    let Some((3, _)) = desc(&mut p) else {
        return;
    };
    p += 2;
    let Some(&flags) = b.get(p) else {
        return;
    };
    p += 1;
    if flags & 0x80 != 0 {
        p += 2;
    }
    if flags & 0x40 != 0 {
        p += 1 + usize::from(b.get(p).copied().unwrap_or(0));
    }
    if flags & 0x20 != 0 {
        p += 2;
    }
    let Some((4, _)) = desc(&mut p) else {
        return;
    };
    let Some(&oti) = b.get(p) else {
        return;
    };
    let avg = b
        .get(p + 9..p + 13)
        .and_then(|x| x.try_into().ok())
        .map(u32::from_be_bytes);
    p += 13;
    match oti {
        0x69 | 0x6b => t.codec = "mp3".into(),
        0xa5 => t.codec = "ac3".into(),
        0xa6 => t.codec = "eac3".into(),
        0xa9..=0xac => t.codec = "dts".into(),
        0xad => t.codec = "opus".into(),
        _ => {}
    }
    if let Some(a) = avg.filter(|a| *a > 0) {
        t.bitrate_kbps.get_or_insert(u64::from(a) / 1000);
    }
    if !matches!(oti, 0x40 | 0x66 | 0x67 | 0x68) {
        return;
    }
    if let Some((5, len)) = desc(&mut p)
        && let Some(asc) = b.get(p..p.saturating_add(len).min(b.len()))
        && let Some(a) = audio::aac_asc(asc)
    {
        t.apply_audio(a);
    }
}

/// First `moof` after `moov`: per-track first sample position / size and frame rate.
fn fragments(src: &Src, from: u64, traks: &mut [Trak], trex: &[(u32, u32, u32)]) {
    let size = src.len();
    let mut off = from;
    for _ in 0..16 {
        let Some(b) = header(src, off, size) else {
            return;
        };
        if &b.ty == b"moof" {
            let Some(d) = body(src, &b, MAX_BODY) else {
                return;
            };
            for f in parse_moof(d, off, trex) {
                if let Some(t) = traks.iter_mut().find(|t| t.tk.id == u64::from(f.track)) {
                    if t.first_off.is_none() || t.samples == 0 {
                        t.first_off = f.first_off;
                        t.first_size = f.first_size;
                    }
                    // The container's rate (what ffprobe reports) wins over the VUI timing, as
                    // the sample table does for a plain MP4: the mean sample duration of the
                    // first fragment (single durations jitter), else the first sample's.
                    if t.tk.kind == Kind::Video && t.timescale > 0 && t.samples == 0 {
                        if f.count >= 2 && f.total_dur > 0 {
                            t.tk.fps =
                                Some(t.timescale as f64 * f.count as f64 / f.total_dur as f64);
                        } else if t.tk.fps.is_none()
                            && let Some(dur) = f.first_dur.filter(|d| *d > 0)
                        {
                            t.tk.fps = Some(t.timescale as f64 / dur as f64);
                        }
                    }
                }
            }
            return;
        }
        if b.end >= size {
            return;
        }
        off = b.end;
    }
}

#[derive(Default, Debug)]
struct Traf {
    track: u32,
    base_time: Option<u64>,
    total_dur: u64,
    /// Samples in the fragment's runs.
    count: u64,
    first_dur: Option<u64>,
    first_off: Option<u64>,
    first_size: Option<u64>,
}

/// `moof` body at file offset `moof_off`: one entry per `traf`.
fn parse_moof(d: &[u8], moof_off: u64, trex: &[(u32, u32, u32)]) -> Vec<Traf> {
    let mut out = Vec::new();
    let mut p = 0usize;
    while out.len() < 64 {
        let Some((ty, body, next)) = inner_box(d, p) else {
            break;
        };
        if ty == b"traf"
            && let Some(t) = parse_traf(body, moof_off, trex)
        {
            out.push(t);
        }
        p = next;
    }
    out
}

fn inner_box(d: &[u8], p: usize) -> Option<(&[u8], &[u8], usize)> {
    let mut c = Cur::new(d.get(p..)?);
    let sz = c.u32()? as usize;
    let ty = c.take(4)?;
    if sz < 8 {
        return None;
    }
    let end = p.checked_add(sz)?;
    Some((ty, d.get(p + 8..end)?, end))
}

fn parse_traf(t: &[u8], moof_off: u64, trex: &[(u32, u32, u32)]) -> Option<Traf> {
    let mut r = Traf::default();
    let mut def_dur = 0u64;
    let mut def_size = 0u64;
    let mut base = moof_off;
    let mut q = 0usize;
    let mut first_trun = true;
    for _ in 0..256 {
        let Some((ty, b, next)) = inner_box(t, q) else {
            break;
        };
        let mut c = Cur::new(b);
        match ty {
            b"tfhd" => {
                let fl = c.u32()? & 0xff_ffff;
                r.track = c.u32()?;
                if let Some(&(_, d, s)) = trex.iter().find(|x| x.0 == r.track) {
                    def_dur = u64::from(d);
                    def_size = u64::from(s);
                }
                if fl & 1 != 0 {
                    base = c.u64()?;
                }
                if fl & 2 != 0 {
                    c.skip(4)?;
                }
                if fl & 8 != 0 {
                    def_dur = u64::from(c.u32()?);
                }
                if fl & 0x10 != 0 {
                    def_size = u64::from(c.u32()?);
                }
            }
            b"tfdt" => {
                let v = c.u8()?;
                c.skip(3)?;
                r.base_time = Some(if v == 1 {
                    c.u64()?
                } else {
                    u64::from(c.u32()?)
                });
            }
            b"trun" => {
                let fl = c.u32()? & 0xff_ffff;
                let n = c.u32()?;
                let data_off = if fl & 1 != 0 {
                    Some(i64::from(c.u32()? as i32))
                } else {
                    None
                };
                if fl & 4 != 0 {
                    c.skip(4)?;
                }
                let per = [0x100, 0x200, 0x400, 0x800]
                    .iter()
                    .filter(|f| fl & **f != 0)
                    .count()
                    * 4;
                for i in 0..n.min(1 << 20) {
                    let mut dur = def_dur;
                    let mut size = def_size;
                    if per > 0 && c.left() < per {
                        break;
                    }
                    if fl & 0x100 != 0 {
                        dur = u64::from(c.u32()?);
                    }
                    if fl & 0x200 != 0 {
                        size = u64::from(c.u32()?);
                    }
                    if fl & 0x400 != 0 {
                        c.skip(4)?;
                    }
                    if fl & 0x800 != 0 {
                        c.skip(4)?;
                    }
                    if i == 0 && first_trun {
                        r.first_dur = Some(dur);
                        r.first_size = Some(size);
                        r.first_off = data_off.and_then(|o| base.checked_add_signed(o));
                    }
                    r.total_dur = r.total_dur.saturating_add(dur);
                    r.count += 1;
                }
                first_trun = false;
            }
            _ => {}
        }
        q = next;
    }
    Some(r)
}

/// Fragmented duration from `mfra`: the last `tfra` entry of the first track, then that
/// fragment's own sample durations.
fn mfra_duration(src: &Src, traks: &[Trak], trex: &[(u32, u32, u32)]) -> Option<f64> {
    let size = src.len();
    if size < 16 {
        return None;
    }
    let t = src.get(size - 16, 16)?;
    let mut c = Cur::new(t);
    c.skip(4)?;
    if c.take(4)? != b"mfro" {
        src.note("mp4: fragmented without mfra; duration unknown");
        return None;
    }
    c.skip(4)?;
    let mfra_size = u64::from(c.u32()?);
    if mfra_size < 16 || mfra_size > size {
        return None;
    }
    let mfra = header(src, size - mfra_size, size)?;
    if &mfra.ty != b"mfra" {
        return None;
    }
    let tfra = children(src, mfra.body, mfra.end)
        .into_iter()
        .find(|b| &b.ty == b"tfra")?;
    let h = src.get(tfra.body, 16.min(tfra.len()))?;
    let mut c = Cur::new(h);
    let v = c.u8()?;
    c.skip(3)?;
    let tid = c.u32()?;
    let x = c.u32()?;
    let n = u64::from(c.u32()?);
    if n == 0 {
        return None;
    }
    let per = u64::from(if v == 1 { 16u32 } else { 8 })
        + u64::from((x >> 4) & 3)
        + 1
        + u64::from((x >> 2) & 3)
        + 1
        + u64::from(x & 3)
        + 1;
    let last = tfra
        .body
        .checked_add(16)?
        .checked_add(per.checked_mul(n - 1)?)?;
    if last.saturating_add(per) > tfra.end {
        src.note("mp4: tfra entries overrun the box");
        return None;
    }
    let e = src.get(last, per)?;
    let mut c = Cur::new(e);
    let time = if v == 1 {
        c.u64()?
    } else {
        u64::from(c.u32()?)
    };
    let moof_off = if v == 1 {
        c.u64()?
    } else {
        u64::from(c.u32()?)
    };
    let tr = traks.iter().find(|t| t.tk.id == u64::from(tid))?;
    if tr.timescale == 0 {
        return None;
    }
    let mut end = time;
    if moof_off < size {
        let m = header(src, moof_off, size).filter(|m| &m.ty == b"moof")?;
        let d = body(src, &m, MAX_BODY)?;
        if let Some(f) = parse_moof(d, moof_off, trex)
            .into_iter()
            .find(|f| f.track == tid)
        {
            end = f.base_time.unwrap_or(time).saturating_add(f.total_dur);
        }
    }
    Some(end as f64 / tr.timescale as f64)
}

fn first_sample(src: &Src, t: &mut Trak) {
    let tk = &mut t.tk;
    let audio =
        tk.wants_audio_frame() && !(t.complete && tk.codec == "eac3" && tk.atmos == Some(true));
    let video = tk.wants_video_frame();
    if !(audio || video) {
        return;
    }
    let (Some(off), Some(sz)) = (t.first_off, t.first_size) else {
        return;
    };
    if sz == 0 || off >= src.len() {
        return;
    }
    let Some(d) = src.get(off, sz.min(if audio { AUDIO_SAMPLE } else { VIDEO_SAMPLE })) else {
        return;
    };
    if audio {
        let a = match tk.codec.as_str() {
            "truehd" => audio::truehd(d),
            "dts" => audio::dts(d),
            _ => audio::ac3(d),
        };
        if let Some(a) = a {
            let prev = tk.atmos;
            tk.apply_audio(a);
            // dec3 said Atmos, the frame disagrees or is silent: keep the explicit flag.
            if prev == Some(true) && tk.atmos == Some(false) && t.complete {
                tk.atmos = Some(true);
            }
        }
        tk.frame_done = true;
        return;
    }
    match tk.codec.as_str() {
        "hevc" => {
            codec::hevc_au(d, t.nal_len, tk);
            if tk.dv.is_none() {
                let end = off.saturating_add(sz);
                let toff = end.saturating_sub(16 * 1024).max(off);
                match src.get(toff, end - toff) {
                    Some(tail) => tk.dv_rpu |= codec::ends_with_rpu(tail, t.nal_len),
                    None => return,
                }
            }
        }
        "av1" => codec::av1_obus(d, tk),
        "vp9" => {
            let _ = codec::vp9_frame(d, tk);
        }
        "prores" => codec::prores_frame(d, tk),
        _ => {}
    }
    tk.frame_done = true;
}

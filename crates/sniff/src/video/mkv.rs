//! Matroska / WebM: EBML header, Segment top level (SeekHead, Info, Tracks, Tags, first
//! Cluster), then a walk of the first Cluster(s) to the first block of every track whose
//! details are only in the bitstream (TrueHD / E-AC-3 / DTS / AC-3 audio, HEVC / AV1 / VP9
//! video).
//!
//! Hardening against the prototype: blocks are matched to tracks by `TrackNumber` (not by
//! track order); SeekHead targets are followed for Info / Tracks placed after the clusters;
//! header stripping (ContentCompAlgo 3) is undone and zlib-compressed frames are inflated
//! (capped); lacing is parsed to find the first frame; every element, child and hop is capped.

use std::io::Read;

use super::src::{BLOCK, Src};
use super::{Kind, Parsed, Tk, TrackKind, audio, codec, lang, text};
use crate::util::us;

const EBML: u32 = 0x1A45_DFA3;
const SEGMENT: u32 = 0x1853_8067;
const SEEKHEAD: u32 = 0x114D_9B74;
const INFO: u32 = 0x1549_A966;
const TRACKS: u32 = 0x1654_AE6B;
const TAGS: u32 = 0x1254_C367;
const CLUSTER: u32 = 0x1F43_B675;
const CUES: u32 = 0x1C53_BB6B;
const BLOCKGROUP: u32 = 0xA0;
const BLOCK_ID: u32 = 0xA1;
const SIMPLEBLOCK: u32 = 0xA3;
const BLOCK_ADDITIONS: u32 = 0x75A1;

const UNKNOWN: u64 = u64::MAX;
/// Largest element body read into memory (Tracks with big CodecPrivate, Tags).
const MAX_BODY: u64 = 2 << 20;
/// How far past the first cluster the block walk goes before giving up on a track.
const MAX_WALK: u64 = 96 << 20;
/// Bytes of a video frame read for SEI / OBU / frame header scans.
const VIDEO_FRAME: u64 = 64 * 1024;
/// Bytes of an audio block read for its first frame(s).
const AUDIO_FRAME: u64 = 32 * 1024;

#[derive(Clone, Copy)]
struct El {
    id: u32,
    data: u64,
    size: u64,
}

impl El {
    fn end(&self) -> u64 {
        if self.size == UNKNOWN {
            UNKNOWN
        } else {
            self.data.saturating_add(self.size)
        }
    }
}

/// EBML variable-size integer: (value without marker, length). `keep` keeps the marker (IDs).
fn vint(b: &[u8], keep: bool) -> Option<(u64, usize)> {
    let f = *b.first()?;
    if f == 0 {
        return None;
    }
    let n = f.leading_zeros() as usize + 1;
    let mut v = if keep {
        u64::from(f)
    } else {
        u64::from(f) & (0xff >> n)
    };
    for x in b.get(1..n)? {
        v = (v << 8) | u64::from(*x);
    }
    Some((v, n))
}

fn header(b: &[u8]) -> Option<(u32, u64, usize)> {
    let (id, a) = vint(b, true)?;
    if a > 4 {
        return None;
    }
    let (sz, n) = vint(b.get(a..)?, false)?;
    let all_ones = sz == (1u64 << (7 * n)) - 1;
    Some((
        u32::try_from(id).ok()?,
        if all_ones { UNKNOWN } else { sz },
        a + n,
    ))
}

/// Element header at `off` (need recorded when unknown).
fn el(src: &Src, off: u64) -> Option<El> {
    let h = src.get(off, 12)?;
    let (id, size, hl) = header(h)?;
    Some(El {
        id,
        data: off.saturating_add(hl as u64),
        size,
    })
}

/// Children of an in-memory element body (a truncated last child is dropped).
fn kids(buf: &[u8]) -> Vec<(u32, &[u8])> {
    let mut v = Vec::new();
    let mut p = 0usize;
    while p < buf.len() && v.len() < 4096 {
        let Some((id, sz, hl)) = buf.get(p..).and_then(header) else {
            break;
        };
        let st = p + hl;
        let Some(en) = us(sz).checked_add(st).filter(|e| *e <= buf.len()) else {
            break;
        };
        if let Some(b) = buf.get(st..en) {
            v.push((id, b));
        }
        p = en;
    }
    v
}

fn uint(b: &[u8]) -> u64 {
    b.iter().take(8).fold(0u64, |a, x| (a << 8) | u64::from(*x))
}

fn float(b: &[u8]) -> Option<f64> {
    match b.len() {
        4 => Some(f64::from(f32::from_be_bytes(b.try_into().ok()?))),
        8 => Some(f64::from_be_bytes(b.try_into().ok()?)),
        _ => None,
    }
}

/// Track state that only the parser needs.
#[derive(Default)]
struct Mt {
    tk: Tk,
    uid: u64,
    private: Vec<u8>,
    /// Header-stripping prefix (ContentCompAlgo 3).
    strip: Option<Vec<u8>>,
    zlib: bool,
    unsupported_encoding: bool,
    nal_len: u8,
    tries: u8,
}

#[derive(Default)]
struct St {
    doctype: String,
    timescale: u64,
    duration: Option<f64>,
    info: bool,
    tracks: Option<Vec<Mt>>,
    seek: Vec<(u32, u64)>,
    tags: Option<u64>,
    first_cluster: Option<El>,
}

pub(crate) fn probe(src: &Src) -> Option<Parsed> {
    let h = el(src, 0)?;
    if h.id != EBML || h.size == UNKNOWN || h.size > 4096 {
        return None;
    }
    let mut st = St {
        timescale: 1_000_000,
        ..St::default()
    };
    if let Some(hb) = src.get(h.data, h.size) {
        for (k, v) in kids(hb) {
            if k == 0x4282 {
                st.doctype = text(v);
            }
        }
    }
    let container = if st.doctype == "webm" { "webm" } else { "mkv" };
    let mut out = Parsed {
        container,
        brand: (!st.doctype.is_empty()).then(|| st.doctype.clone()),
        ..Parsed::default()
    };
    let Some(seg) = el(src, h.end()) else {
        return Some(out);
    };
    if seg.id != SEGMENT {
        return Some(out);
    }
    let seg_end = seg.end().min(src.len());
    walk_top(src, &seg, seg_end, &mut st);
    // SeekHead targets not met by the walk (Tracks after the clusters, a walk cut short).
    let seek = std::mem::take(&mut st.seek);
    for (id, pos) in &seek {
        let at = seg.data.saturating_add(*pos);
        if at >= seg_end {
            continue;
        }
        match *id {
            INFO if !st.info => {
                if let Some(e) = el(src, at).filter(|e| e.id == INFO)
                    && let Some(b) = body(src, &e)
                {
                    info(b, &mut st);
                }
            }
            TRACKS if st.tracks.is_none() => {
                if let Some(e) = el(src, at).filter(|e| e.id == TRACKS)
                    && let Some(b) = body(src, &e)
                {
                    st.tracks = Some(tracks(b));
                }
            }
            TAGS if st.tags.is_none() => st.tags = Some(at),
            _ => {}
        }
    }
    if let Some(d) = st.duration {
        out.duration = Some(d * st.timescale as f64 / 1e9);
    }
    let Some(mut tracks) = st.tracks.take() else {
        src.note(if src.wants_more() {
            "matroska: Tracks not reached yet"
        } else {
            "matroska: no Tracks element found"
        });
        return Some(out);
    };
    // Per-track bitrates (mkvmerge statistics tags), only when already at hand or close.
    if let Some(at) = st.tags
        && let Some(e) = el(src, at).filter(|e| e.id == TAGS && e.size <= 256 * 1024)
        && let Some(b) = body(src, &e)
    {
        tags(b, &mut tracks);
    }
    if let Some(cl) = st.first_cluster {
        frames(src, cl, &mut tracks);
    } else if tracks
        .iter()
        .any(|t| t.tk.wants_audio_frame() || t.tk.wants_video_frame())
    {
        src.note("matroska: first Cluster not reached yet");
    }
    for t in &tracks {
        if t.tk.wants_audio_frame() {
            src.note(format!(
                "matroska: first frame of audio track {} not parsed",
                t.tk.id
            ));
        }
        if t.unsupported_encoding {
            src.note(format!(
                "matroska: track {} uses an unsupported content encoding",
                t.tk.id
            ));
        }
    }
    out.tracks = tracks.into_iter().map(|t| t.tk).collect();
    Some(out)
}

fn body<'a>(src: &Src<'a>, e: &El) -> Option<&'a [u8]> {
    if e.size == UNKNOWN || e.size > MAX_BODY {
        return None;
    }
    src.get(e.data, e.size)
}

/// Level-1 element IDs (what a resync looks for).
const LEVEL1: [[u8; 4]; 8] = [
    [0x11, 0x4D, 0x9B, 0x74],
    [0x15, 0x49, 0xA9, 0x66],
    [0x16, 0x54, 0xAE, 0x6B],
    [0x1F, 0x43, 0xB6, 0x75],
    [0x1C, 0x53, 0xBB, 0x6B],
    [0x12, 0x54, 0xC3, 0x67],
    [0x10, 0x43, 0xA7, 0x70],
    [0x19, 0x41, 0xA4, 0x69],
];

/// Bytes scanned for a level-1 ID after junk between top-level elements.
const RESYNC: usize = 64 * 1024;

/// The offset of the next level-1 ID in the known bytes after `off` (FFmpeg's matroskadec
/// resyncs the same way on junk between elements, as in a live or damaged file).
fn resync(src: &Src, off: u64) -> Option<u64> {
    let b = src.avail(off.saturating_add(1), RESYNC);
    let hit = (0..b.len().saturating_sub(3)).find(|&i| {
        b.get(i..i + 4)
            .is_some_and(|w| LEVEL1.iter().any(|id| id == w))
    })?;
    Some(off + 1 + hit as u64)
}

fn walk_top(src: &Src, seg: &El, seg_end: u64, st: &mut St) {
    let mut off = seg.data;
    let mut resyncs = 0;
    for _ in 0..256 {
        if off >= seg_end {
            break;
        }
        let e = match src.peek(off, 12).map(header) {
            // Known bytes that are not an element header: junk, resync.
            Some(None) if resyncs < 4 => {
                resyncs += 1;
                match resync(src, off) {
                    Some(o) => {
                        src.note("matroska: junk between top-level elements skipped");
                        off = o;
                        continue;
                    }
                    None => {
                        src.note(
                            "matroska: junk between top-level elements, no level-1 ID after it",
                        );
                        break;
                    }
                }
            }
            _ => match el(src, off) {
                Some(e) => e,
                None => break,
            },
        };
        match e.id {
            SEEKHEAD => {
                if let Some(b) = body(src, &e) {
                    for (k, v) in kids(b) {
                        if k != 0x4DBB {
                            continue;
                        }
                        let (mut i, mut p) = (0u32, None);
                        for (k2, v2) in kids(v) {
                            match k2 {
                                0x53AB => i = u32::try_from(uint(v2)).unwrap_or(0),
                                0x53AC => p = Some(uint(v2)),
                                _ => {}
                            }
                        }
                        if let Some(p) = p
                            && st.seek.len() < 64
                        {
                            st.seek.push((i, p));
                        }
                    }
                }
            }
            INFO => {
                if let Some(b) = body(src, &e) {
                    info(b, st);
                }
            }
            TRACKS => {
                if let Some(b) = body(src, &e) {
                    st.tracks = Some(tracks(b));
                }
            }
            TAGS => st.tags = Some(off),
            CLUSTER => {
                st.first_cluster = Some(e);
                break;
            }
            _ => {}
        }
        if e.size == UNKNOWN {
            break;
        }
        off = e.end();
    }
}

fn info(b: &[u8], st: &mut St) {
    for (k, v) in kids(b) {
        match k {
            0x2AD7B1 => st.timescale = uint(v).clamp(1, 1_000_000_000_000),
            0x4489 => st.duration = float(v).filter(|d| d.is_finite() && *d >= 0.0),
            _ => {}
        }
    }
    st.info = true;
}

fn tracks(b: &[u8]) -> Vec<Mt> {
    kids(b)
        .into_iter()
        .filter(|(k, _)| *k == 0xAE)
        .take(128)
        .map(|(_, v)| track(v))
        .collect()
}

fn track(b: &[u8]) -> Mt {
    let mut m = Mt {
        nal_len: 4,
        ..Mt::default()
    };
    let t = &mut m.tk;
    t.default = true;
    let mut lang_iso: Option<String> = None;
    let mut lang_bcp: Option<String> = None;
    let mut id = String::new();
    let mut dv: Vec<&[u8]> = Vec::new();
    let (mut hearing_impaired, mut visual_impaired) = (false, false);
    let (mut text_descriptions, mut commentary) = (false, false);
    let mut original: Option<bool> = None;
    for (k, v) in kids(b) {
        match k {
            0xD7 => t.id = uint(v),
            0x73C5 => m.uid = uint(v),
            0x83 => {
                t.kind = match uint(v) {
                    1 => Kind::Video,
                    2 => Kind::Audio,
                    0x11 => Kind::Sub,
                    _ => Kind::Other,
                }
            }
            0x88 => t.default = uint(v) != 0,
            0x55AA => t.forced = uint(v) != 0,
            // Matroska v4 accessibility flags (D123): applied below, once the type is known.
            0x55AB => hearing_impaired = uint(v) != 0,
            0x55AC => visual_impaired = uint(v) != 0,
            0x55AD => text_descriptions = uint(v) != 0,
            0x55AE => original = Some(uint(v) != 0),
            0x55AF => commentary = uint(v) != 0,
            0x86 => id = text(v),
            0x63A2 => m.private = v.get(..v.len().min(1 << 20)).unwrap_or(&[]).to_vec(),
            0x22B59C => lang_iso = Some(text(v)),
            0x22B59D => lang_bcp = Some(text(v)),
            0x536E => t.name = Some(text(v)).filter(|s| !s.is_empty()),
            0x23E383 => {
                let dd = uint(v);
                if dd > 0 {
                    t.fps = Some(1e9 / dd as f64);
                }
            }
            0xE0 => video(v, t),
            0xE1 => {
                for (k2, v2) in kids(v) {
                    match k2 {
                        0xB5 => {
                            t.sample_rate =
                                float(v2).filter(|f| *f > 0.0 && *f < 1e7).map(|f| f as u32)
                        }
                        0x9F => t.channels = u8::try_from(uint(v2)).ok(),
                        0x6264 => t.bit_depth = u8::try_from(uint(v2)).ok(),
                        _ => {}
                    }
                }
            }
            0x41E4 => {
                let mut ty = 0;
                let mut ex: &[u8] = &[];
                for (k2, v2) in kids(v) {
                    match k2 {
                        0x41E7 => ty = uint(v2),
                        0x41ED => ex = v2,
                        _ => {}
                    }
                }
                // dvcC / dvvC / dvwC
                if matches!(ty, 0x6476_6343 | 0x6476_7643 | 0x6476_7743) {
                    dv.push(ex);
                }
            }
            0x6D80 => {
                for (_, enc) in kids(v).into_iter().filter(|(k, _)| *k == 0x6240) {
                    for (k3, v3) in kids(enc) {
                        if k3 == 0x5035 {
                            m.unsupported_encoding = true; // encrypted
                        }
                        if k3 != 0x5034 {
                            continue;
                        }
                        let mut algo = 0;
                        let mut settings: &[u8] = &[];
                        for (k4, v4) in kids(v3) {
                            match k4 {
                                0x4254 => algo = uint(v4),
                                0x4255 => settings = v4,
                                _ => {}
                            }
                        }
                        match algo {
                            0 => m.zlib = true,
                            3 => {
                                m.strip = Some(
                                    settings
                                        .get(..settings.len().min(256))
                                        .unwrap_or(&[])
                                        .to_vec(),
                                )
                            }
                            _ => m.unsupported_encoding = true,
                        }
                    }
                }
            }
            _ => {}
        }
    }
    let t = &mut m.tk;
    // Matroska's default language is English; `und` means unknown.
    t.language = match (lang_iso, lang_bcp) {
        (Some(l), b) if lang(&l).is_some() => lang(&l).or_else(|| b.as_deref().and_then(lang)),
        (Some(_), Some(b)) | (None, Some(b)) => lang(&b),
        (Some(_), None) => None,
        (None, None) => Some("eng".into()),
    };
    let name = codec_name(&id);
    t.codec = if name.is_empty() {
        format!("?{}", id.chars().take(32).collect::<String>())
    } else {
        name.to_string()
    };
    match id.as_str() {
        "D_WEBVTT/CAPTIONS" => t.add_kind(TrackKind::Sdh),
        "D_WEBVTT/DESCRIPTIONS" => t.add_kind(TrackKind::Description),
        // WebVTT metadata cues are not subtitles.
        "D_WEBVTT/METADATA" => t.kind = Kind::Other,
        _ => {}
    }
    match t.kind {
        Kind::Sub => {
            if hearing_impaired {
                t.add_kind(TrackKind::Sdh);
            }
            if visual_impaired || text_descriptions {
                t.add_kind(TrackKind::Description);
            }
            if commentary {
                t.add_kind(TrackKind::Commentary);
            }
        }
        Kind::Audio => {
            if hearing_impaired {
                t.add_kind(TrackKind::Dialogue);
            }
            if visual_impaired {
                t.add_kind(TrackKind::Description);
            }
            if commentary {
                t.add_kind(TrackKind::Commentary);
            }
            t.original = original;
        }
        _ => {}
    }
    let cp = m.private.clone();
    match t.codec.as_str() {
        "h264" if !cp.is_empty() => {
            let _ = codec::avcc(&cp, t);
        }
        "hevc" if !cp.is_empty() => {
            if let Some(n) = codec::hvcc(&cp, t) {
                m.nal_len = n;
            }
        }
        "av1" if !cp.is_empty() => {
            let _ = codec::av1c(&cp, t);
        }
        "vp9" if !cp.is_empty() => codec::vp9_private(&cp, t),
        "aac" if !cp.is_empty() => {
            if let Some(a) = audio::aac_asc(&cp) {
                t.apply_audio(a);
            }
        }
        "mpeg4" | "msmpeg4v3" | "vfw" if cp.len() >= 20 => vfw(&cp, t),
        _ => {}
    }
    if t.codec == "dts" && id.contains("LOSSLESS") {
        t.profile = Some("DTS-HD MA".into());
    }
    for ex in dv {
        let _ = codec::dovi(ex, t);
    }
    m
}

fn video(v: &[u8], t: &mut Tk) {
    for (k, v2) in kids(v) {
        match k {
            0xB0 => t.width = u32::try_from(uint(v2)).ok().filter(|w| *w > 0),
            0xBA => t.height = u32::try_from(uint(v2)).ok().filter(|h| *h > 0),
            0x55B0 => {
                for (k3, v3) in kids(v2) {
                    let n = u8::try_from(uint(v3)).ok();
                    match k3 {
                        0x55B1 => t.matrix = n,
                        0x55B2 => {
                            // Real muxers write 8, 10 or 12 (or nothing); anything outside
                            // 8..=16 (Chromium's colour.webm says 1) is not a video bit depth.
                            if let Some(bd) = n.filter(|b| (8..=16).contains(b)) {
                                t.bit_depth.get_or_insert(bd);
                            }
                        }
                        0x55BA => t.transfer = n,
                        0x55BB => t.primaries = n,
                        0x55BC => {
                            let c = u16::try_from(uint(v3)).unwrap_or(u16::MAX);
                            t.cll = Some((c, t.cll.map_or(0, |x| x.1)));
                        }
                        0x55BD => {
                            let f = u16::try_from(uint(v3)).unwrap_or(u16::MAX);
                            t.cll = Some((t.cll.map_or(0, |x| x.0), f));
                        }
                        0x55D0 => t.mastering = true,
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
}

/// V_MS/VFW/FOURCC: BITMAPINFOHEADER compression FourCC.
fn vfw(cp: &[u8], t: &mut Tk) {
    let fc = cp.get(16..20).unwrap_or(&[]).to_ascii_uppercase();
    super::vfw_codec(&fc, t);
}

fn codec_name(id: &str) -> &'static str {
    match id {
        "V_MPEG4/ISO/AVC" => "h264",
        "V_MPEGH/ISO/HEVC" => "hevc",
        "V_MPEGI/ISO/VVC" => "vvc",
        "V_AV1" => "av1",
        "V_VP9" => "vp9",
        "V_VP8" => "vp8",
        "V_MPEG4/ISO/ASP" | "V_MPEG4/ISO/SP" | "V_MPEG4/ISO/AP" => "mpeg4",
        "V_MPEG2" => "mpeg2video",
        "V_MPEG1" => "mpeg1video",
        "V_MS/VFW/FOURCC" => "vfw",
        "V_THEORA" => "theora",
        "V_PRORES" => "prores",
        "A_AAC" | "A_AAC/MPEG4/LC" | "A_AAC/MPEG4/LC/SBR" | "A_AAC/MPEG2/LC" => "aac",
        "A_AC3" | "A_AC3/BSID9" | "A_AC3/BSID10" => "ac3",
        "A_EAC3" => "eac3",
        "A_DTS" | "A_DTS/EXPRESS" | "A_DTS/LOSSLESS" => "dts",
        "A_TRUEHD" => "truehd",
        "A_MLP" => "mlp",
        "A_OPUS" => "opus",
        "A_FLAC" => "flac",
        "A_VORBIS" => "vorbis",
        "A_ALAC" => "alac",
        "A_MPEG/L3" => "mp3",
        "A_MPEG/L2" => "mp2",
        "A_PCM/INT/LIT" => "pcm_s16le",
        "A_PCM/INT/BIG" => "pcm_s16be",
        "A_PCM/FLOAT/IEEE" => "pcm_f32le",
        "S_TEXT/UTF8" | "S_TEXT/ASCII" => "subrip",
        "S_TEXT/ASS" | "S_TEXT/SSA" | "S_ASS" | "S_SSA" => "ass",
        "S_TEXT/WEBVTT" | "D_WEBVTT/SUBTITLES" | "D_WEBVTT/CAPTIONS" | "D_WEBVTT/DESCRIPTIONS" => {
            "webvtt"
        }
        "S_ARIBSUB" => "arib_caption",
        "S_TEXT/USF" => "usf",
        "S_HDMV/PGS" => "hdmv_pgs_subtitle",
        "S_HDMV/TEXTST" => "hdmv_text_subtitle",
        "S_VOBSUB" => "dvd_subtitle",
        "S_DVBSUB" => "dvb_subtitle",
        "S_KATE" => "kate",
        _ => "",
    }
}

fn tags(b: &[u8], tracks: &mut [Mt]) {
    for (k, tag) in kids(b).into_iter().take(512) {
        if k != 0x7373 {
            continue;
        }
        let mut uid = 0;
        let mut bps: Option<u64> = None;
        for (k2, v2) in kids(tag) {
            match k2 {
                0x63C0 => {
                    for (k3, v3) in kids(v2) {
                        if k3 == 0x63C5 {
                            uid = uint(v3);
                        }
                    }
                }
                0x67C8 => {
                    let (mut n, mut s) = (String::new(), String::new());
                    for (k3, v3) in kids(v2) {
                        match k3 {
                            0x45A3 => n = text(v3),
                            0x4487 => s = text(v3),
                            _ => {}
                        }
                    }
                    if n == "BPS" {
                        bps = s.parse().ok();
                    }
                }
                _ => {}
            }
        }
        if let (Some(b), true) = (bps, uid != 0)
            && let Some(t) = tracks.iter_mut().find(|t| t.uid == uid)
        {
            t.tk.bitrate_kbps = Some(b / 1000);
        }
    }
}

/// Walks the clusters from the first one to the first block of every track that wants it.
fn frames(src: &Src, first: El, tracks: &mut [Mt]) {
    let wanted = |tr: &[Mt]| {
        tr.iter()
            .any(|t| (t.tk.wants_audio_frame() || t.tk.wants_video_frame()) && t.tries < 8)
    };
    if !wanted(tracks) {
        return;
    }
    let start = first.data;
    let mut off = first.data;
    for _ in 0..4096 {
        if !wanted(tracks) {
            return;
        }
        if off.saturating_sub(start) > MAX_WALK {
            src.note("matroska: first blocks not found within 96 MiB of the first cluster");
            return;
        }
        let Some(e) = el(src, off) else {
            return;
        };
        match e.id {
            BLOCKGROUP if e.size != UNKNOWN => {
                // Block plus BlockAdditions (HDR10+ metadata of VP9 / AV1 in WebM).
                let (mut c, end) = (e.data, e.end());
                let (mut blk, mut add) = (None, None);
                for _ in 0..16 {
                    if c >= end {
                        break;
                    }
                    let Some(k) = el(src, c) else {
                        return; // asked for; next pass
                    };
                    match k.id {
                        BLOCK_ID => blk = Some(k),
                        BLOCK_ADDITIONS => add = Some(k),
                        _ => {}
                    }
                    if k.size == UNKNOWN {
                        break;
                    }
                    c = k.end();
                }
                if let Some(bk) = blk.filter(|b| b.size != UNKNOWN) {
                    if let Some(a) = add {
                        additions(src, &bk, &a, tracks);
                    }
                    block(src, &bk, tracks);
                }
            }
            CLUSTER | BLOCKGROUP => {
                off = e.data;
                continue;
            }
            SIMPLEBLOCK | BLOCK_ID if e.size != UNKNOWN => block(src, &e, tracks),
            CUES | TAGS | 0x1941_A469 | 0x1043_A770 | SEEKHEAD | INFO | TRACKS => {}
            _ => {}
        }
        if e.size == UNKNOWN {
            return;
        }
        off = e.end();
    }
}

/// ITU-T T.35 header of HDR10+ dynamic metadata (SMPTE ST 2094-40): country code (United
/// States), provider code (Samsung), provider-oriented code, application identifier 4.
const HDR10PLUS_T35: [u8; 6] = [0xB5, 0x00, 0x3C, 0x00, 0x01, 0x04];

/// BlockAdditions of a video block whose first frame is still wanted: HDR10+ metadata carried
/// as ITU-T T.35 in a BlockMore (BlockAddID 4 in WebM, the mapping Matroska registers).
fn additions(src: &Src, blk: &El, add: &El, tracks: &mut [Mt]) {
    let Some((tn, _)) = src
        .get(blk.data, 8.min(blk.size))
        .and_then(|h| vint(h, false))
    else {
        return;
    };
    let Some(m) = tracks.iter_mut().find(|t| t.tk.id == tn) else {
        return;
    };
    if !m.tk.wants_video_frame() || m.tries >= 8 {
        return;
    }
    let Some(b) = body(src, add) else {
        return;
    };
    for (k, more) in kids(b) {
        if k != 0xA6 {
            continue;
        }
        for (k2, v) in kids(more) {
            if k2 == 0xA5 && v.starts_with(&HDR10PLUS_T35) {
                m.tk.hdr10plus = true;
            }
        }
    }
}

fn block(src: &Src, e: &El, tracks: &mut [Mt]) {
    let Some(h) = src.get(e.data, 16.min(e.size)) else {
        return;
    };
    let Some((tn, a)) = vint(h, false) else {
        return;
    };
    let Some(m) = tracks.iter_mut().find(|t| t.tk.id == tn) else {
        return;
    };
    let audio_wanted = m.tk.wants_audio_frame();
    if !(audio_wanted || m.tk.wants_video_frame()) || m.tries >= 8 {
        return;
    }
    let Some(&flags) = h.get(a + 2) else {
        return;
    };
    let payload = e.data.saturating_add(a as u64 + 3);
    let plen = e.size.saturating_sub(a as u64 + 3);
    let want = plen.min(if audio_wanted {
        AUDIO_FRAME
    } else {
        VIDEO_FRAME
    });
    let Some(data) = src.get(payload, want) else {
        return; // asked for; next pass
    };
    m.tries += 1;
    // Lacing: the first frame starts after the lace header.
    let lacing = (flags >> 1) & 3;
    let (fstart, flen) = match lace_first(data, lacing) {
        Some(x) => x,
        None => return,
    };
    let Some(frame) = data.get(fstart..fstart.saturating_add(flen).min(data.len())) else {
        return;
    };
    let frame = match decode(m, frame) {
        Some(f) => f,
        None => return,
    };
    let t = &mut m.tk;
    if audio_wanted {
        let a = match t.codec.as_str() {
            "truehd" | "mlp" => audio::truehd(&frame),
            "dts" => audio::dts(&frame),
            _ => audio::ac3(&frame),
        };
        if let Some(a) = a {
            t.apply_audio(a);
            t.frame_done = true;
        }
        return;
    }
    match t.codec.as_str() {
        "hevc" => {
            codec::hevc_au(&frame, m.nal_len, t);
            // Dolby Vision RPU (last NAL of the AU) when no configuration record says so.
            if t.dv.is_none() && lacing == 0 && m.strip.is_none() && !m.zlib {
                let end = payload.saturating_add(plen);
                let tail_off = end.saturating_sub(BLOCK.min(plen)).max(payload);
                if let Some(tail) = src.get(tail_off, end - tail_off) {
                    t.dv_rpu |= codec::ends_with_rpu(tail, m.nal_len);
                } else {
                    return;
                }
            }
        }
        "av1" => codec::av1_obus(&frame, t),
        "vp9" => {
            let _ = codec::vp9_frame(&frame, t);
        }
        "prores" => codec::prores_frame(&frame, t),
        _ => {}
    }
    t.frame_done = true;
}

/// Offset and length of the first frame in a (possibly laced) block payload.
fn lace_first(d: &[u8], lacing: u8) -> Option<(usize, usize)> {
    if lacing == 0 {
        return Some((0, d.len()));
    }
    let n = usize::from(*d.first()?) + 1;
    match lacing {
        1 => {
            // Xiph: sizes as runs of 255.
            let mut p = 1usize;
            let mut size = 0usize;
            loop {
                let x = *d.get(p)?;
                p += 1;
                size += usize::from(x);
                if x != 255 {
                    break;
                }
            }
            // Skip the other n - 2 sizes.
            for _ in 0..n.saturating_sub(2) {
                loop {
                    let x = *d.get(p)?;
                    p += 1;
                    if x != 255 {
                        break;
                    }
                }
            }
            Some((p, size))
        }
        3 => {
            // EBML: first size as vint, then signed differences.
            let mut p = 1usize;
            let (first, l) = vint(d.get(p..)?, false)?;
            p += l;
            for _ in 0..n.saturating_sub(2) {
                let (_, l) = vint(d.get(p..)?, false)?;
                p += l;
            }
            Some((p, us(first)))
        }
        _ => Some((1, d.len().saturating_sub(1) / n)), // fixed-size lacing
    }
}

/// Undoes header stripping / zlib for a frame.
fn decode(m: &Mt, frame: &[u8]) -> Option<Vec<u8>> {
    if m.unsupported_encoding {
        return None;
    }
    if m.zlib {
        let mut out = Vec::new();
        let mut z = flate2::read::ZlibDecoder::new(frame).take(256 * 1024);
        let _ = z.read_to_end(&mut out);
        return (!out.is_empty()).then_some(out);
    }
    match &m.strip {
        Some(p) => {
            let mut v = Vec::with_capacity(p.len() + frame.len());
            v.extend_from_slice(p);
            v.extend_from_slice(frame);
            Some(v)
        }
        None => Some(frame.to_vec()),
    }
}

#[cfg(test)]
mod tests {
    use super::super::testkit::{info, mkv, sel, track, uel};
    use super::super::{AudioTrack, SubtitleTrack, TrackKind};

    fn kinds(k: &[TrackKind]) -> Vec<&'static str> {
        k.iter().map(|k| k.as_str()).collect()
    }

    fn sub(i: &super::super::VideoInfo, id: u64) -> &SubtitleTrack {
        i.subtitles.iter().find(|s| s.id == id).expect("subtitle")
    }

    fn audio(i: &super::super::VideoInfo, id: u64) -> &AudioTrack {
        i.audio.iter().find(|a| a.id == id).expect("audio")
    }

    #[test]
    fn accessibility_flags_give_kinds() {
        let f = mkv(&[
            track(1, 0x11, "S_TEXT/UTF8", &[uel(&[0x55, 0xAB], 1)]),
            track(2, 2, "A_AAC", &[uel(&[0x55, 0xAB], 1)]),
            track(3, 2, "A_AAC", &[uel(&[0x55, 0xAC], 1)]),
            track(4, 0x11, "S_TEXT/UTF8", &[uel(&[0x55, 0xAD], 1)]),
            track(5, 2, "A_AAC", &[uel(&[0x55, 0xAF], 1)]),
            track(6, 0x11, "S_TEXT/UTF8", &[uel(&[0x55, 0xAF], 1)]),
            track(7, 2, "A_AAC", &[uel(&[0x55, 0xAE], 0)]),
            track(8, 2, "A_AAC", &[uel(&[0x55, 0xAE], 1)]),
            track(9, 2, "A_AAC", &[]),
            // Flags at 0 say nothing.
            track(10, 0x11, "S_TEXT/UTF8", &[uel(&[0x55, 0xAB], 0)]),
            // FlagTextDescriptions is ignored on audio, FlagOriginal on subtitles.
            track(11, 2, "A_AAC", &[uel(&[0x55, 0xAD], 1)]),
            track(12, 0x11, "S_TEXT/UTF8", &[uel(&[0x55, 0xAE], 0)]),
        ]);
        let i = info(&f);
        assert_eq!(kinds(&sub(&i, 1).kinds), ["sdh"]);
        assert_eq!(kinds(&audio(&i, 2).kinds), ["dialogue"]);
        assert_eq!(kinds(&audio(&i, 3).kinds), ["description"]);
        assert_eq!(kinds(&sub(&i, 4).kinds), ["description"]);
        assert_eq!(kinds(&audio(&i, 5).kinds), ["commentary"]);
        assert_eq!(kinds(&sub(&i, 6).kinds), ["commentary"]);
        assert_eq!(kinds(&audio(&i, 7).kinds), ["dub"]);
        assert_eq!(kinds(&audio(&i, 8).kinds), ["original"]);
        assert!(audio(&i, 9).kinds.is_empty());
        assert!(sub(&i, 10).kinds.is_empty());
        assert!(audio(&i, 11).kinds.is_empty());
        assert!(sub(&i, 12).kinds.is_empty());
        // None of them came from a name.
        assert!(i.audio.iter().all(|a| a.name_kinds.is_empty()));
    }

    #[test]
    fn webvtt_kinds_and_new_codec_ids() {
        let f = mkv(&[
            track(1, 0x11, "D_WEBVTT/CAPTIONS", &[]),
            track(2, 0x11, "D_WEBVTT/DESCRIPTIONS", &[]),
            track(3, 0x11, "D_WEBVTT/METADATA", &[]),
            track(4, 0x11, "S_ARIBSUB", &[]),
            track(5, 0x11, "S_TEXT/USF", &[]),
            track(6, 0x11, "D_WEBVTT/SUBTITLES", &[]),
        ]);
        let i = info(&f);
        assert_eq!(i.subtitles.len(), 5, "{:?}", i.subtitles);
        assert_eq!(
            (sub(&i, 1).codec.as_str(), kinds(&sub(&i, 1).kinds)),
            ("webvtt", vec!["sdh"])
        );
        assert_eq!(
            (sub(&i, 2).codec.as_str(), kinds(&sub(&i, 2).kinds)),
            ("webvtt", vec!["description"])
        );
        assert!(i.subtitles.iter().all(|s| s.id != 3));
        assert_eq!(sub(&i, 4).codec, "arib_caption");
        assert_eq!(sub(&i, 5).codec, "usf");
        assert!(sub(&i, 6).kinds.is_empty());
    }

    #[test]
    fn name_overrides_forced_flag() {
        let name = |s: &str| sel(&[0x53, 0x6E], s);
        let f = mkv(&[
            track(
                1,
                0x11,
                "S_TEXT/UTF8",
                &[uel(&[0x55, 0xAA], 1), name("English (Full)")],
            ),
            track(
                2,
                0x11,
                "S_TEXT/UTF8",
                &[uel(&[0x55, 0xAA], 1), name("English (SDH)")],
            ),
            track(
                3,
                0x11,
                "S_TEXT/UTF8",
                &[uel(&[0x55, 0xAA], 0), name("Forced")],
            ),
            track(4, 0x11, "S_TEXT/UTF8", &[uel(&[0x55, 0xAA], 1)]),
            track(5, 2, "A_AAC", &[name("Commentary"), uel(&[0x55, 0xAE], 0)]),
        ]);
        let i = info(&f);
        // The raw flag is kept as written; the kinds say what the track is.
        assert!(sub(&i, 1).forced);
        assert!(sub(&i, 1).kinds.is_empty());
        assert_eq!(kinds(&sub(&i, 2).kinds), ["sdh"]);
        assert_eq!(kinds(&sub(&i, 2).name_kinds), ["sdh"]);
        assert!(!sub(&i, 3).forced);
        assert_eq!(kinds(&sub(&i, 3).kinds), ["forced"]);
        assert_eq!(kinds(&sub(&i, 3).name_kinds), ["forced"]);
        assert_eq!(kinds(&sub(&i, 4).kinds), ["forced"]);
        assert!(sub(&i, 4).name_kinds.is_empty());
        assert_eq!(kinds(&audio(&i, 5).kinds), ["commentary"]);
        assert_eq!(audio(&i, 5).name.as_deref(), Some("Commentary"));
    }
}

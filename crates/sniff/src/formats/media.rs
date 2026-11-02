//! Audio/video containers and streams that need structural checks: RIFF, Ogg, MPEG audio frame
//! chains (MP3/ADTS/AC-3), DTS and AMR frame chains, ProTracker MOD (sample table), MPEG-TS/M2TS,
//! MPEG-PS, elementary video streams, DV, PGS, Matroska, ISO BMFF (MP4/MOV/HEIF/AVIF) and ASF.
//! Simple magics live in `data/signatures/media.toml`; a short magic whose structure does not
//! check out stays at [`WEAK`] or below.
//!
//! The track summaries here (`tracks`, `codecs`) only refine the label; full video recon is
//! [`crate::video`].

use crate::types::{Hit, WEAK};
use crate::util::*;
use crate::view::View;

pub(crate) static CHECKS: &[(&str, crate::formats::Check)] = &[
    ("riff", riff),
    ("ogg", ogg),
    ("mpeg_audio", mpeg_audio_family),
    ("dts", dts),
    ("amr", amr),
    ("protracker", protracker),
    ("mpegts", mpegts),
    ("mpeg_ps", mpeg_ps),
    ("es_video", es_video),
    ("dv", dv),
    ("pgs", pgs),
    ("matroska", matroska),
    ("isobmff", isobmff),
    ("asf", asf),
];

fn riff(v: &View) -> Option<Hit> {
    let h = v.head();
    let rf64 = at(h, 0, b"RF64");
    if !(at(h, 0, b"RIFF") || at(h, 0, b"RIFX") || rf64) {
        return None;
    }
    let form = h.get(8..12)?;
    let size = u64::from(u32le(h, 4)?);
    let size_ok = rf64 || size.saturating_add(8) <= v.size().saturating_add(1);
    let c = if size_ok { 95 } else { 80 };
    let l = match form {
        b"AVI " | b"AVIX" => "video.avi",
        b"WAVE" => "audio.wav",
        b"WEBP" => "image.webp",
        b"CDXA" => "video.mpeg_ps", // VCD .dat: MPEG-1 PS in RIFF CDXA
        b"RMID" => "audio.midi",
        b"ACON" => "image.ani",
        b"AMV " => "video.amv",
        b"QLCM" => "audio.qcp",
        _ => return Some(Hit::new("misc.riff", 50).d("form", lossy(form))),
    };
    let mut hit = Hit::new(l, c);
    if l == "audio.wav"
        && let Some(i) = find(sl(h, 12, 512), b"fmt ")
        && let Some(tag) = u16le(h, 12 + i + 8)
    {
        hit.push("format_tag", format!("{tag:#06x}"));
        if tag == 0x2000 {
            hit.push("codec", "ac3-in-wav");
        }
        if tag == 0x0001 && contains(pre(h, 16384), &[0x1f, 0xff, 0xe8, 0x00]) {
            hit.push("codec", "dts-in-wav?");
        }
    }
    Some(hit)
}

fn ogg(v: &View) -> Option<Hit> {
    let h = v.head();
    if !at(h, 0, b"OggS") || u8at(h, 4) != Some(0) {
        return None;
    }
    let nseg = usize::from(u8at(h, 26)?);
    let b = h.get(27 + nseg..)?;
    let (l, codec) = if at(b, 0, b"\x01vorbis") {
        ("audio.ogg.vorbis", "vorbis")
    } else if at(b, 0, b"OpusHead") {
        ("audio.ogg.opus", "opus")
    } else if at(b, 0, b"\x80theora") {
        ("video.ogv", "theora")
    } else if at(b, 0, b"\x7fFLAC") || at(b, 0, b"fLaC") {
        ("audio.ogg.flac", "flac")
    } else if at(b, 0, b"Speex   ") {
        ("audio.ogg.speex", "speex")
    } else if at(b, 1, b"video") || at(b, 0, b"\x80kate") {
        ("video.ogv", "other")
    } else {
        ("audio.ogg", "unknown")
    };
    // A theora-less first page can still carry a video stream in a later BOS page.
    let l = if l != "video.ogv" && contains(pre(h, 65536), b"\x80theora") {
        "video.ogv"
    } else {
        l
    };
    // The page CRC is the structural check: 95+ only when the first page checks out.
    let page_len = 27
        + nseg
        + h.get(27..27 + nseg)?
            .iter()
            .map(|&x| usize::from(x))
            .sum::<usize>();
    let conf = match h.get(..page_len) {
        Some(page) if ogg_crc_ok(page) => 95,
        Some(_) => 70,
        // Page not read yet: the codec id is the second marker; without one "OggS\0" is a hint.
        None if codec == "unknown" => WEAK,
        None => 90,
    };
    Some(Hit::new(l, conf).d("first_codec", codec))
}

/// Ogg page CRC (CRC-32, polynomial 0x04C11DB7, not reflected, zero initial value) over the
/// page with its CRC field (bytes 22..26) taken as zero.
fn ogg_crc_ok(page: &[u8]) -> bool {
    let Some(want) = page.get(22..26).and_then(|b| b.try_into().ok()) else {
        return false;
    };
    let want = u32::from_le_bytes(want);
    let mut crc = 0u32;
    for (i, &b) in page.iter().enumerate() {
        let b = if (22..26).contains(&i) { 0 } else { b };
        crc ^= u32::from(b) << 24;
        for _ in 0..8 {
            crc = if crc & 0x8000_0000 != 0 {
                (crc << 1) ^ 0x04C1_1DB7
            } else {
                crc << 1
            };
        }
    }
    crc == want
}

/// MPEG-1/2 audio frame header -> frame length in bytes.
fn mpa_frame(b: &[u8]) -> Option<usize> {
    let (b0, b1, b2) = (u8at(b, 0)?, u8at(b, 1)?, u8at(b, 2)?);
    if b.len() < 4 || b0 != 0xFF || b1 & 0xE0 != 0xE0 {
        return None;
    }
    let ver = usize::from((b1 >> 3) & 3); // 0=2.5 1=res 2=v2 3=v1
    let layer = usize::from((b1 >> 1) & 3); // 1=L3 2=L2 3=L1
    let bri = usize::from(b2 >> 4);
    let sri = usize::from((b2 >> 2) & 3);
    let pad = usize::from((b2 >> 1) & 1);
    if ver == 1 || layer == 0 || bri == 0 || bri == 15 || sri == 3 {
        return None;
    }
    const BR_V1: [[u32; 16]; 3] = [
        [
            0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 0,
        ],
        [
            0, 32, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384, 0,
        ],
        [
            0, 32, 64, 96, 128, 160, 192, 224, 256, 288, 320, 352, 384, 416, 448, 0,
        ],
    ];
    const BR_V2: [[u32; 16]; 2] = [
        [
            0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160, 0,
        ],
        [
            0, 32, 48, 56, 64, 80, 96, 112, 128, 144, 160, 176, 192, 224, 256, 0,
        ],
    ];
    const SR: [[u32; 3]; 4] = [
        [11025, 12000, 8000],
        [0, 0, 0],
        [22050, 24000, 16000],
        [44100, 48000, 32000],
    ];
    let sr = *SR.get(ver)?.get(sri)?;
    let br = if ver == 3 {
        *BR_V1.get(layer - 1)?.get(bri)?
    } else {
        *BR_V2.get(usize::from(layer == 3))?.get(bri)?
    } * 1000;
    if sr == 0 {
        return None;
    }
    let len = if layer == 3 {
        (12 * br / sr) as usize * 4 + pad * 4
    } else if layer == 2 || ver == 3 {
        (144 * br / sr) as usize + pad
    } else {
        (72 * br / sr) as usize + pad
    };
    if len < 21 { None } else { Some(len) }
}

/// Number of consecutive frames (up to `need`) parsed by `f` from `start`.
fn chain<F: Fn(&[u8]) -> Option<usize>>(h: &[u8], start: usize, f: F, need: usize) -> usize {
    let mut o = start;
    let mut n = 0;
    while n < need {
        match h.get(o..).and_then(&f) {
            Some(l) => {
                n += 1;
                o = o.saturating_add(l);
                if o >= h.len() {
                    break;
                }
            }
            None => break,
        }
    }
    n
}

fn adts(b: &[u8]) -> Option<usize> {
    if b.len() < 7 || u8at(b, 0)? != 0xFF || u8at(b, 1)? & 0xF6 != 0xF0 {
        return None;
    }
    if (u8at(b, 2)? >> 2) & 0xF > 12 {
        return None;
    }
    let len = (usize::from(u8at(b, 3)? & 3) << 11)
        | (usize::from(u8at(b, 4)?) << 3)
        | usize::from(u8at(b, 5)? >> 5);
    if len < 7 { None } else { Some(len) }
}

fn ac3(b: &[u8]) -> Option<(usize, u8)> {
    if b.len() < 6 || u8at(b, 0)? != 0x0B || u8at(b, 1)? != 0x77 {
        return None;
    }
    let bsid = u8at(b, 5)? >> 3;
    if bsid <= 10 {
        let fscod = u8at(b, 4)? >> 6;
        let frm = usize::from(u8at(b, 4)? & 0x3F);
        if fscod == 3 || frm > 37 {
            return None;
        }
        const W48: [u16; 19] = [
            64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384, 448, 512, 640, 768, 896, 1024,
            1152, 1280,
        ];
        const W44: [u16; 19] = [
            69, 87, 104, 121, 139, 174, 208, 243, 278, 348, 417, 487, 557, 696, 835, 975, 1114,
            1253, 1393,
        ];
        const W32: [u16; 19] = [
            96, 120, 144, 168, 192, 240, 288, 336, 384, 480, 576, 672, 768, 960, 1152, 1344, 1536,
            1728, 1920,
        ];
        let words = match fscod {
            0 => *W48.get(frm / 2)?,
            1 => W44.get(frm / 2)? + (frm & 1) as u16,
            _ => *W32.get(frm / 2)?,
        };
        Some((usize::from(words) * 2, bsid))
    } else if bsid <= 16 {
        let words = ((usize::from(u8at(b, 2)? & 7) << 8) | usize::from(u8at(b, 3)?)) + 1;
        Some((words * 2, bsid))
    } else {
        None
    }
}

fn mpeg_audio_family(v: &View) -> Option<Hit> {
    let h = v.head();
    let mut off = 0usize;
    let mut id3 = false;
    if at(h, 0, b"ID3") && h.len() >= 10 && u8at(h, 3)? < 5 {
        let b = |i| usize::from(u8at(h, i).unwrap_or(0) & 0x7f);
        let sz = (b(6) << 21) | (b(7) << 14) | (b(8) << 7) | b(9);
        off = 10 + sz + if u8at(h, 5)? & 0x10 != 0 { 10 } else { 0 };
        id3 = true;
        if off >= h.len() {
            return Some(
                Hit::new("audio.mpeg", 60)
                    .d("note", "ID3v2 tag larger than head")
                    .need(off as u64, 4096),
            );
        }
        // ID3 can precede FLAC / AAC too.
        if at(h, off, b"fLaC") {
            return Some(Hit::new("audio.flac", 95).d("id3", true));
        }
    }
    // Skip zero padding after the tag.
    while off < h.len().min(8192) && u8at(h, off) == Some(0) {
        off += 1;
    }
    let mpa = chain(h, off, mpa_frame, 4);
    if mpa >= 3 || (mpa >= 1 && id3) {
        let layer = u8at(h, off + 1).map_or(1, |b| (b >> 1) & 3);
        let layer = match layer {
            1 => "3",
            2 => "2",
            _ => "1",
        };
        return Some(Hit::new("audio.mpeg", if mpa >= 3 { 92 } else { 75 }).d("layer", layer));
    }
    if chain(h, off, adts, 4) >= 3 {
        return Some(Hit::new("audio.aac", 92));
    }
    if chain(h, off, |b| ac3(b).map(|x| x.0), 3) >= 2 {
        let bsid = ac3(from(h, off)).map_or(0, |x| x.1);
        return Some(
            Hit::new(if bsid > 10 { "audio.eac3" } else { "audio.ac3" }, 92).d("bsid", bsid),
        );
    }
    if id3 {
        return Some(Hit::new("audio.mpeg", 55).d("note", "ID3 tag, no frame sync found"));
    }
    None
}

fn mpegts(v: &View) -> Option<Hit> {
    let h = v.head();
    for (stride, off, l) in [
        (192usize, 4usize, "video.m2ts"),
        (188, 0, "video.ts"),
        (204, 0, "video.ts"),
    ] {
        // Allow up to one packet of leading garbage (file starting mid-packet).
        for start in 0..stride.min(h.len()) {
            let mut n = 0;
            let mut o = start + off;
            while u8at(h, o) == Some(0x47) && n < 64 {
                n += 1;
                o += stride;
            }
            let possible = h.len().saturating_sub(start + off) / stride;
            if n >= 8 || (n >= 3 && n >= possible) {
                let mut hit = Hit::new(l, if n >= 16 { 96 } else { 85 }).d("packet", stride);
                if start != 0 {
                    hit.push("sync_offset", start);
                }
                return Some(hit);
            }
            if start > 0 && stride == 192 {
                break;
            }
        }
    }
    None
}

fn mpeg_ps(v: &View) -> Option<Hit> {
    let h = v.head();
    if !at(h, 0, &[0, 0, 1, 0xBA]) {
        return None;
    }
    let mpeg2 = u8at(h, 4).is_some_and(|b| b & 0xC0 == 0x40);
    // Stream ids in the first packs: video 0xE0-0xEF, private 0xBD (AC3/DTS/subpictures),
    // audio 0xC0-0xDF.
    let lim = h.len().min(512 * 1024);
    let (mut video, mut audio, mut subpic, mut private) = (0u32, 0u32, 0u32, 0u32);
    let mut i = 0;
    while let Some(j) = find(sl(h, i, lim), &[0, 0, 1]) {
        let o = i + j;
        if let Some(sid) = u8at(h, o + 3) {
            match sid {
                0xE0..=0xEF => video += 1,
                0xC0..=0xDF => audio += 1,
                0xBD => {
                    private += 1;
                    // PES header data length at +8, substream id after it.
                    if let (Some(hl), true) = (u8at(h, o + 8), mpeg2)
                        && u8at(h, o + 9 + usize::from(hl))
                            .is_some_and(|ss| (0x20..=0x3F).contains(&ss))
                    {
                        subpic += 1;
                    }
                }
                _ => {}
            }
        }
        i = o + 3;
        if i >= lim {
            break;
        }
    }
    // The pack header's marker bits and the start code right after it (MPEG-2: 14 bytes plus
    // stuffing, MPEG-1: 12) are the structural check; PES packets found are a second one.
    let pack_ok = if mpeg2 {
        h.get(4..14)
            .and_then(|b| <[u8; 10]>::try_from(b).ok())
            .is_some_and(|[b0, _, b2, _, b4, b5, _, _, b8, b9]| {
                b0 & 0xC4 == 0x44
                    && b2 & 0x04 != 0
                    && b4 & 0x04 != 0
                    && b5 & 0x01 != 0
                    && b8 & 0x03 == 0x03
                    && at(h, 14 + usize::from(b9 & 7), &[0, 0, 1])
            })
    } else {
        h.get(4..12)
            .and_then(|b| <[u8; 8]>::try_from(b).ok())
            .is_some_and(|[b0, _, b2, _, b4, b5, _, b7]| {
                b0 & 0xF1 == 0x21
                    && b2 & 1 != 0
                    && b4 & 1 != 0
                    && b5 & 0x80 != 0
                    && b7 & 1 != 0
                    && at(h, 12, &[0, 0, 1])
            })
    };
    if !pack_ok && video + audio + private < 2 {
        return Some(Hit::new("video.mpeg_ps", WEAK).d("note", "pack start code only"));
    }
    if video == 0 && audio == 0 && subpic > 0 && subpic == private {
        return Some(
            Hit::new("subtitle.vobsub", 92).d("note", "MPEG-PS with only DVD subpicture streams"),
        );
    }
    Some(
        Hit::new("video.mpeg_ps", 92)
            .d("mpeg", if mpeg2 { "2" } else { "1" })
            .d("video_pes", video),
    )
}

fn es_video(v: &View) -> Option<Hit> {
    let h = v.head();
    let o = if at(h, 0, &[0, 0, 0, 1]) {
        4
    } else if at(h, 0, &[0, 0, 1]) {
        3
    } else {
        0
    };
    if o > 0 {
        let b0 = u8at(h, o)?;
        let b1 = u8at(h, o + 1)?;
        if at(h, 0, &[0, 0, 1, 0xB3]) {
            // Sequence header: aspect ratio 1-4, frame rate code 1-8, the marker bit after the
            // bit rate, then another start code (extension, GOP, picture) within the next bytes
            // (quantiser matrices take up to 128).
            let seq_ok = matches!(
                (u8at(h, 7), u8at(h, 10)),
                (Some(ar), Some(br)) if (1..=4).contains(&(ar >> 4))
                    && (1..=8).contains(&(ar & 0xF))
                    && br & 0x20 != 0
            ) && find(sl(h, 12, 12 + 160), &[0, 0, 1])
                .is_some_and(|i| matches!(u8at(h, 12 + i + 3), Some(0xB5 | 0xB8 | 0x00 | 0xB2)));
            return Some(Hit::new("video.mpeg2_es", if seq_ok { 85 } else { WEAK }));
        }
        if at(h, 0, &[0, 0, 1, 0xB0]) || at(h, 0, &[0, 0, 1, 0x20]) {
            return Some(Hit::new("video.mpeg4_es", 70));
        }
        let hevc_t = (b0 >> 1) & 0x3F;
        // Count further start codes to make sure it is a NAL stream.
        let n = memchr::memmem::find_iter(pre(h, 65536), &[0, 0, 1]).count();
        if b0 & 0x80 == 0 && b1 == 0x01 && (32..=40).contains(&hevc_t) && n >= 3 {
            return Some(Hit::new("video.hevc", 85).d("first_nal", hevc_t));
        }
        let t = b0 & 0x1F;
        if b0 & 0x80 == 0 && matches!(t, 6 | 7 | 9) && (t != 6 || b0 == 0x06) && n >= 3 {
            return Some(Hit::new("video.h264", 85).d("first_nal", t));
        }
    }
    // AV1 low-overhead bitstream: temporal delimiter OBU (type 2, has_size) then size 0.
    if at(h, 0, &[0x12, 0x00]) && u8at(h, 2).is_some_and(|b| (b >> 3) & 0xF == 1) {
        let chain = av1_chain(h, 2, v.size());
        return Some(Hit::new("video.av1_obu", if chain { 85 } else { WEAK }));
    }
    None
}

/// AV1 OBU chain from `o` (the sequence header after the temporal delimiter): every OBU header
/// has the forbidden and reserved bits clear, a size field and a defined type, and its size
/// leads to the next one; the sequence profile is 0-2. Four OBUs, or all of a shorter file.
fn av1_chain(h: &[u8], mut o: usize, size: u64) -> bool {
    let mut n = 0;
    while n < 4 {
        let Some(b) = u8at(h, o) else {
            return false;
        };
        let ty = (b >> 3) & 0xF;
        if b & 0x83 != 0x02 || !matches!(ty, 1..=8 | 15) {
            return false;
        }
        let ext = usize::from(b & 4 != 0);
        let Some((sz, len)) = leb128(h, o + 1 + ext) else {
            return false;
        };
        let body = o + 1 + ext + len;
        if n == 0 && u8at(h, body).is_none_or(|p| p >> 5 > 2) {
            return false;
        }
        let Some(next) = usize::try_from(sz).ok().and_then(|s| body.checked_add(s)) else {
            return false;
        };
        n += 1;
        if next as u64 == size {
            return true;
        }
        o = next;
    }
    true
}

/// AV1 LEB128 size at `o`: (value, length in bytes).
fn leb128(b: &[u8], o: usize) -> Option<(u64, usize)> {
    let mut v = 0u64;
    for i in 0..8 {
        let x = u8at(b, o + i)?;
        v |= u64::from(x & 0x7F) << (7 * i);
        if x & 0x80 == 0 {
            return Some((v, i + 1));
        }
    }
    None
}

/// DV DIF: header section (id 0x1F, seq/dif fields), repeated every 80 bytes in blocks.
fn dv(v: &View) -> Option<Hit> {
    let h = v.head();
    if h.len() < 480 || !at(h, 0, &[0x1F, 0x07, 0x00]) {
        return None;
    }
    if u8at(h, 80)? >> 5 != 1 || u8at(h, 240)? >> 5 != 2 {
        return None;
    }
    // The rest of the first DIF sequence: subcode 2, VAUX 4-5, audio 6, video 7 (section
    // types in the top 3 bits of each 80-byte block).
    let seq_ok = [(160, 1), (320, 2), (400, 2), (480, 3), (560, 4)]
        .iter()
        .all(|&(o, t)| u8at(h, o).is_some_and(|b| b >> 5 == t));
    let system = if u8at(h, 3)? & 0x80 != 0 {
        "625/50"
    } else {
        "525/60"
    };
    Some(Hit::new("video.dv", if seq_ok { 90 } else { WEAK }).d("system", system))
}

/// PGS (.sup): chain of "PG" segments with known segment types.
fn pgs(v: &View) -> Option<Hit> {
    let h = v.head();
    if !at(h, 0, b"PG") {
        return None;
    }
    let mut o = 0usize;
    let mut n = 0;
    while at(h, o, b"PG") && n < 8 {
        let (Some(ty), Some(sz)) = (u8at(h, o + 10), u16be(h, o + 11)) else {
            break;
        };
        if !matches!(ty, 0x14 | 0x15 | 0x16 | 0x17 | 0x80) {
            break;
        }
        n += 1;
        o += 13 + usize::from(sz);
        if o >= h.len() {
            break;
        }
    }
    if n >= 3 || (n >= 1 && o >= h.len()) {
        return Some(Hit::new("subtitle.pgs", if n >= 3 { 95 } else { 70 }));
    }
    None
}

// ---------------------------------------------------------------- Matroska

/// EBML variable-length integer at `o`: (value, length). `strip` removes the length marker
/// (sizes); an all-ones size is "unknown" (u64::MAX).
pub(crate) fn ebml_vint(b: &[u8], o: usize, strip: bool) -> Option<(u64, usize)> {
    let f = u8at(b, o)?;
    if f == 0 {
        return None;
    }
    let len = f.leading_zeros() as usize + 1;
    let mut v = if strip {
        u64::from(f) & ((1u64 << (8 - len)) - 1)
    } else {
        u64::from(f)
    };
    for i in 1..len {
        v = (v << 8) | u64::from(u8at(b, o + i)?);
    }
    if strip && v == (1u64 << (7 * len)) - 1 {
        v = u64::MAX;
    }
    Some((v, len))
}

fn matroska(v: &View) -> Option<Hit> {
    let h = v.head();
    if !at(h, 0, &[0x1A, 0x45, 0xDF, 0xA3]) {
        return None;
    }
    let (hsz, l) = ebml_vint(h, 4, true)?;
    let hend = (4 + l).saturating_add(us(hsz.min(4096)));
    let mut doctype = String::new();
    let mut o = 4 + l;
    while o < hend.min(h.len()) {
        let (id, il) = ebml_vint(h, o, false)?;
        let (sz, szl) = ebml_vint(h, o + il, true)?;
        let d = o + il + szl;
        if id == 0x4282 {
            doctype = cstr(sl(h, d, d.saturating_add(us(sz.min(64)))));
        }
        let no = d.saturating_add(us(sz));
        if no <= o {
            break;
        }
        o = no;
    }
    let webm = doctype == "webm";
    let mut hit =
        Hit::new(if webm { "video.webm" } else { "video.mkv" }, 90).d("doctype", doctype.clone());
    if !webm && doctype != "matroska" {
        hit.conf = 60;
        return Some(hit);
    }
    // Segment
    let (sid, sil) = ebml_vint(h, hend, false)?;
    if sid != 0x18538067 {
        return Some(hit);
    }
    let (_, ssl) = ebml_vint(h, hend + sil, true)?;
    let seg_data = (hend + sil + ssl) as u64;
    let mut o = seg_data;
    let mut tracks_at: Option<u64> = None;
    for _ in 0..64 {
        let b = v.from(o);
        let (Some((id, il)), true) = (ebml_vint(b, 0, false), b.len() > 12) else {
            break;
        };
        let Some((sz, szl)) = ebml_vint(b, il, true) else {
            break;
        };
        let d = (il + szl) as u64;
        match id {
            0x114D9B74 => {
                // SeekHead: Seek { SeekID = Tracks, SeekPosition }
                let sh = sl(b, us(d), us(d.saturating_add(sz)));
                if let Some(j) = find(sh, &[0x53, 0xAB, 0x84, 0x16, 0x54, 0xAE, 0x6B]) {
                    let k = j + 7;
                    if at(sh, k, &[0x53, 0xAC])
                        && let Some((n, nl)) = ebml_vint(sh, k + 2, true)
                    {
                        let mut pos = 0u64;
                        for x in 0..us(n).min(8) {
                            pos = (pos << 8) | u64::from(u8at(sh, k + 2 + nl + x).unwrap_or(0));
                        }
                        tracks_at = Some(seg_data.saturating_add(pos));
                    }
                }
            }
            0x1654AE6B => {
                let tb = v.from(o.saturating_add(d));
                let want = us(sz.min(1 << 20));
                if tb.len() < want {
                    return Some(hit.need(o, d.saturating_add(sz.min(1 << 20))));
                }
                return Some(mkv_tracks(pre(tb, want), hit));
            }
            0x1F43B675 => break, // first Cluster: Tracks came before (or via the SeekHead)
            _ => {}
        }
        if sz == u64::MAX {
            break;
        }
        o = o.saturating_add(d).saturating_add(sz);
        if !v.has(o, 16) {
            break;
        }
    }
    if let Some(t) = tracks_at {
        if t >= v.size() {
            return Some(hit);
        }
        if !v.has(t, 16) {
            return Some(hit.need(t, 256 * 1024));
        }
        let b = v.from(t);
        if let (Some((0x1654AE6B, il)), true) = (ebml_vint(b, 0, false), b.len() > 12)
            && let Some((sz, szl)) = ebml_vint(b, il, true)
        {
            let s = il + szl;
            let want = us(sz).min(1 << 20);
            if b.len() < s + want {
                return Some(hit.need(t, (s + want) as u64));
            }
            return Some(mkv_tracks(sl(b, s, s + want), hit));
        }
    }
    Some(hit)
}

fn mkv_tracks(t: &[u8], mut hit: Hit) -> Hit {
    let mut o = 0;
    let (mut nv, mut na, mut ns) = (0u32, 0u32, 0u32);
    let mut codecs = Vec::new();
    while o < t.len() && codecs.len() < 256 {
        let Some((id, il)) = ebml_vint(t, o, false) else {
            break;
        };
        let Some((sz, szl)) = ebml_vint(t, o + il, true) else {
            break;
        };
        let d = o + il + szl;
        let end = d.saturating_add(us(sz)).min(t.len());
        if end <= o {
            break;
        }
        if id == 0xAE {
            let mut q = d;
            let (mut ty, mut codec) = (0u64, String::new());
            while q < end {
                let Some((cid, cl)) = ebml_vint(t, q, false) else {
                    break;
                };
                let Some((csz, csl)) = ebml_vint(t, q + cl, true) else {
                    break;
                };
                let cd = q + cl + csl;
                let ce = cd.saturating_add(us(csz)).min(end);
                match cid {
                    0x83 => {
                        ty = sl(t, cd, ce)
                            .iter()
                            .take(8)
                            .fold(0, |acc, &x| (acc << 8) | u64::from(x))
                    }
                    0x86 => codec = cstr(sl(t, cd, ce)),
                    _ => {}
                }
                let nq = cd.saturating_add(us(csz));
                if nq <= q {
                    break;
                }
                q = nq;
            }
            match ty {
                1 => nv += 1,
                2 => na += 1,
                17 => ns += 1,
                _ => {}
            }
            codecs.push(codec);
        }
        o = end;
    }
    if nv == 0 && na > 0 && hit.label() == "video.mkv" {
        hit.relabel("audio.mka");
    }
    hit.conf = 97;
    hit.d("tracks", format!("v{nv} a{na} s{ns}"))
        .d("codecs", codecs)
}

// ---------------------------------------------------------------- ISO BMFF

/// Largest `moov` the type check reads itself (bigger ones go to the video recon).
const MOOV_READ: u64 = 64 * 1024;

fn isobmff(v: &View) -> Option<Hit> {
    let h = v.head();
    let t = h.get(4..8)?;
    if !matches!(
        t,
        b"ftyp" | b"moov" | b"mdat" | b"free" | b"wide" | b"skip" | b"pnot" | b"styp"
    ) {
        return None;
    }
    let sz = u64::from(u32be(h, 0)?);
    if sz < 8 && sz != 1 && sz != 0 {
        return None;
    }
    let mut hit;
    if t == b"ftyp" {
        if !(16..=4096).contains(&sz) {
            return None;
        }
        let major = h.get(8..12)?;
        let mut brands: Vec<&[u8]> = vec![major];
        let mut i = 16;
        while i + 4 <= us(sz).min(h.len()) {
            brands.push(sl(h, i, i + 4));
            i += 4;
        }
        let has = |b: &[u8]| brands.contains(&b);
        let label = match major {
            b"qt  " => "video.mov",
            m if m.starts_with(b"3gp") || m.starts_with(b"3g2") || m.starts_with(b"3ge") => {
                "video.3gp"
            }
            b"avif" | b"avis" => "image.avif",
            b"heic" | b"heix" | b"hevc" | b"hevx" | b"heim" | b"heis" => "image.heic",
            b"mif1" | b"msf1" => {
                if has(b"avif") {
                    "image.avif"
                } else {
                    "image.heic"
                }
            }
            b"M4A " | b"M4B " | b"M4P " | b"F4A " | b"F4B " => "audio.m4a",
            b"crx " => "image.raw.cr3",
            b"jp2 " | b"jpx " => "image.jp2",
            _ => "video.mp4",
        };
        hit = Hit::new(label, 90).d("major_brand", lossy(major));
        if !matches!(label, "video.mp4" | "video.mov" | "video.3gp") {
            hit.conf = 96;
            return Some(hit);
        }
    } else {
        hit = Hit::new("video.mov", 70).d("note", "no ftyp (old QuickTime)");
    }
    // Walk top-level boxes to find moov (may be at the end: then ask for it).
    let mut o = 0u64;
    let mut seen = Vec::new();
    for _ in 0..64 {
        let Some(b) = v.at(o, 16).or_else(|| v.at(o, 8)) else {
            if o < v.size() {
                return Some(hit.need(o, 16));
            }
            break;
        };
        let mut bs = u64::from(u32be(b, 0)?);
        let ty = sl(b, 4, 8);
        if !ty.iter().all(|c| c.is_ascii_graphic() || *c == b' ') {
            hit.conf = hit.conf.min(60);
            break;
        }
        let hdr = if bs == 1 {
            bs = u64be(b, 8).unwrap_or(0);
            16
        } else {
            8
        };
        if bs == 0 {
            bs = v.size().saturating_sub(o);
        }
        if bs < hdr {
            break;
        }
        seen.push(lossy(ty));
        if ty == b"moov" {
            // A small moov is read here; a large one (sample tables of a long film run to
            // megabytes) is left to the video recon, which walks it by box headers and makes
            // the same audio-only call from its track handlers (engine `video_refines`).
            if bs > MOOV_READ {
                hit.conf = hit.conf.max(92);
                hit.push("moov_at", o);
                break;
            }
            let want = bs;
            match v.at(o, us(want)) {
                Some(m) => {
                    let handlers: Vec<String> = memchr::memmem::find_iter(m, b"hdlr")
                        .filter_map(|i| m.get(i + 12..i + 16).map(lossy))
                        .take(64)
                        .collect();
                    let codecs: Vec<String> = memchr::memmem::find_iter(m, b"stsd")
                        .filter_map(|i| m.get(i + 16..i + 20).map(lossy))
                        .take(64)
                        .collect();
                    let vide = handlers.iter().any(|x| x == "vide");
                    let soun = handlers.iter().any(|x| x == "soun");
                    if !vide && soun && hit.label() == "video.mp4" {
                        hit.relabel("audio.m4a");
                    }
                    hit.conf = 97;
                    hit.push("handlers", handlers);
                    hit.push("codecs", codecs);
                    hit.push("moov_at", o);
                }
                None => return Some(hit.need(o, want)),
            }
            break;
        }
        if ty == b"moof" {
            hit.push("fragmented", true);
            hit.conf = hit.conf.max(92);
            break;
        }
        o = o.saturating_add(bs);
        if o >= v.size() {
            break;
        }
    }
    Some(hit.d("boxes", seen))
}

// ---------------------------------------------------------------- ASF

const ASF_HDR: [u8; 16] = [
    0x30, 0x26, 0xB2, 0x75, 0x8E, 0x66, 0xCF, 0x11, 0xA6, 0xD9, 0x00, 0xAA, 0x00, 0x62, 0xCE, 0x6C,
];
const ASF_STREAM: [u8; 16] = [
    0x91, 0x07, 0xDC, 0xB7, 0xB7, 0xA9, 0xCF, 0x11, 0x8E, 0xE6, 0x00, 0xC0, 0x0C, 0x20, 0x53, 0x65,
];
const ASF_VIDEO: [u8; 16] = [
    0xC0, 0xEF, 0x19, 0xBC, 0x4D, 0x5B, 0xCF, 0x11, 0xA8, 0xFD, 0x00, 0x80, 0x5F, 0x5C, 0x44, 0x2B,
];
const ASF_AUDIO: [u8; 16] = [
    0x40, 0x9E, 0x69, 0xF8, 0x4D, 0x5B, 0xCF, 0x11, 0xA8, 0xFD, 0x00, 0x80, 0x5F, 0x5C, 0x44, 0x2B,
];

fn asf(v: &View) -> Option<Hit> {
    let h = v.head();
    if !at(h, 0, &ASF_HDR) {
        return None;
    }
    let hs = us(u64le(h, 16)?);
    let (mut nv, mut na) = (0u32, 0u32);
    for i in memchr::memmem::find_iter(pre(h, hs), &ASF_STREAM) {
        if at(h, i + 24, &ASF_VIDEO) {
            nv += 1;
        }
        if at(h, i + 24, &ASF_AUDIO) {
            na += 1;
        }
    }
    let mut hit = Hit::new(
        if nv > 0 { "video.asf" } else { "audio.wma" },
        if nv + na > 0 { 97 } else { 80 },
    )
    .d("streams", format!("v{nv} a{na}"));
    if hs > h.len() && nv + na == 0 && hs <= 1 << 22 {
        hit = hit.need(0, hs as u64);
    }
    Some(hit)
}

// ---------------------------------------------------------------- DTS, AMR, MOD

/// DTS core frame header after the sync word, from the 16-bit big-endian form of the stream:
/// (frame size in bytes, normal frame). FTYPE 1 + SHORT 31 (normal frame), NBLKS >= 5, FSIZE
/// >= 95 and a valid SFREQ code.
fn dts_header(b: &[u8; 12]) -> Option<usize> {
    let [_, _, _, _, b4, b5, b6, b7, b8, ..] = *b;
    let nblks = (usize::from(b4 & 1) << 6) | usize::from(b5 >> 2);
    let fsize = (usize::from(b5 & 3) << 12) | (usize::from(b6) << 4) | usize::from(b7 >> 4);
    let sfreq = (b8 >> 2) & 0xF;
    let ok = b4 & 0xFC == 0xFC
        && nblks >= 5
        && fsize >= 95
        && matches!(sfreq, 1 | 2 | 3 | 6 | 7 | 8 | 11 | 12 | 13);
    ok.then_some(fsize + 1)
}

/// The first 12 bytes of a DTS frame as a 16-bit big-endian stream: LE streams swap byte
/// pairs, 14-bit streams keep the low 14 bits of each 16-bit word.
fn dts_be16(b: &[u8], le: bool, w14: bool) -> Option<[u8; 12]> {
    let words = b.get(..16)?.as_chunks::<2>().0.iter().map(|&c| {
        let w = if le {
            u16::from_le_bytes(c)
        } else {
            u16::from_be_bytes(c)
        };
        if w14 {
            (u32::from(w & 0x3FFF), 14)
        } else {
            (u32::from(w), 16)
        }
    });
    let mut out = Vec::with_capacity(12);
    let (mut acc, mut nbits) = (0u64, 0u32);
    for (w, bits) in words {
        acc = (acc << bits) | u64::from(w);
        nbits += bits;
        while nbits >= 8 && out.len() < 12 {
            nbits -= 8;
            out.push((acc >> nbits) as u8);
        }
    }
    out.try_into().ok()
}

/// DTS core stream (16-bit BE / LE, 14-bit BE / LE): a valid frame header whose frame size
/// leads to the next sync word, core or DTS-HD extension substream (frame chain, 95). A next
/// frame past the known bytes is asked for; a single frame that ends the file: 85. Otherwise
/// the bare sync is left to the signature rule ([`WEAK`]).
fn dts(v: &View) -> Option<Hit> {
    let h = v.head();
    let (sync, le, w14): (&[u8], bool, bool) = match h.get(..4)? {
        [0x7F, 0xFE, 0x80, 0x01] => (&[0x7F, 0xFE, 0x80, 0x01], false, false),
        [0xFE, 0x7F, 0x01, 0x80] => (&[0xFE, 0x7F, 0x01, 0x80], true, false),
        [0x1F, 0xFF, 0xE8, 0x00] => (&[0x1F, 0xFF, 0xE8, 0x00], false, true),
        [0xFF, 0x1F, 0x00, 0xE8] => (&[0xFF, 0x1F, 0x00, 0xE8], true, true),
        _ => return None,
    };
    let b = dts_be16(h, le, w14)?;
    let fsize = dts_header(&b)?;
    let variant = match (le, w14) {
        (false, false) => "16-bit be",
        (true, false) => "16-bit le",
        (false, true) => "14-bit be",
        (true, true) => "14-bit le",
    };
    // 14-bit streams spread the frame over 16/14 as many bytes.
    let next = if w14 { fsize * 8 / 7 } else { fsize } as u64;
    let size = v.size();
    let hit = |c| {
        Hit::new("audio.dts", c)
            .d("variant", variant)
            .d("frame_bytes", next)
    };
    if next >= size {
        return (next == size).then(|| hit(85));
    }
    let lo = if w14 { next.saturating_sub(2) } else { next };
    let hi = if w14 { next + 2 } else { next };
    let Some(win) = v.at(lo, (hi - lo + 4).min(size - lo) as usize) else {
        let mut pending = hit(WEAK).need(lo, hi - lo + 4);
        // Outranks the bare signature rule at the same confidence so the range is fetched.
        pending.strength = 1000;
        return Some(pending);
    };
    // DTS-HD: the core frame is followed by an extension substream (sync 0x64582025) and
    // only then by the next core frame.
    let exss: &[u8] = if le {
        &[0x58, 0x64, 0x25, 0x20]
    } else {
        &[0x64, 0x58, 0x20, 0x25]
    };
    find(win, sync)
        .or_else(|| (!w14).then(|| find(win, exss)).flatten())
        .map(|_| hit(95))
}

/// AMR / AMR-WB storage format ("#!AMR\n", "#!AMR-WB\n"): a chain of frames whose header
/// bytes have the F bit and padding clear and a frame type with a known size. Four frames (or
/// all of a shorter file): 95.
fn amr(v: &View) -> Option<Hit> {
    const NB: [usize; 16] = [13, 14, 16, 18, 20, 21, 27, 32, 6, 0, 0, 0, 0, 0, 0, 1];
    const WB: [usize; 16] = [18, 24, 33, 37, 41, 47, 51, 59, 61, 6, 0, 0, 0, 0, 1, 1];
    let h = v.head();
    let (sizes, mut o, wb) = if at(h, 0, b"#!AMR\n") {
        (&NB, 6usize, false)
    } else if at(h, 0, b"#!AMR-WB\n") {
        (&WB, 9, true)
    } else {
        return None;
    };
    let mut n = 0;
    while n < 4 {
        let Some(b) = u8at(h, o) else { break };
        let len = sizes.get(usize::from((b >> 3) & 0xF)).copied().unwrap_or(0);
        if b & 0x83 != 0 || len == 0 {
            return None;
        }
        o += len;
        n += 1;
    }
    let ok = n >= 4 || (n >= 1 && o as u64 == v.size());
    ok.then(|| Hit::new("audio.amr", 95).d("variant", if wb { "wb" } else { "nb" }))
}

/// ProTracker MOD family: channel tag at 1080, then the 31-sample table (finetune 0-15, volume
/// 0-64), song length 1-128 and an order list of patterns below 128. The file must also hold
/// the patterns the order list uses (95; 85 when it is shorter, a truncated module).
fn protracker(v: &View) -> Option<Hit> {
    let h = v.head();
    let channels: u64 = match h.get(1080..1084)? {
        b"M.K." | b"M!K!" | b"FLT4" | b"4CHN" => 4,
        b"6CHN" => 6,
        b"8CHN" | b"OCTA" | b"FLT8" | b"CD81" => 8,
        _ => return None,
    };
    let samples_ok = (0..31).all(|i| {
        let s = 20 + i * 30;
        u8at(h, s + 24).is_some_and(|f| f <= 15) && u8at(h, s + 25).is_some_and(|v| v <= 64)
    });
    let song_len = u8at(h, 950)?;
    let orders = sl(h, 952, 1080);
    if !samples_ok || !(1..=128).contains(&song_len) || orders.iter().any(|&p| p >= 128) {
        return None;
    }
    let patterns = u64::from(*orders.iter().max()?) + 1;
    let full = 1084 + patterns * 64 * 4 * channels <= v.size();
    Some(
        Hit::new("audio.mod", if full { 95 } else { 85 })
            .d("channels", channels)
            .d("patterns", patterns)
            .d("title", cstr(pre(h, 20))),
    )
}

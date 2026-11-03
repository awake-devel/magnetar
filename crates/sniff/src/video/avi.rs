//! AVI (RIFF): LIST `hdrl` at the start (avih, strl / strh / strf / strn, odml / dmlh), then
//! the first video chunk of LIST `movi` for the MPEG-4 Part 2 profile (VOS / VOL header).
//! AVI has no language field.

use super::bits::Cur;
use super::src::Src;
use super::{Kind, Parsed, Tk, audio, codec, text};

/// RIFF chunk at `p` in `d`: (fourcc, body, next offset).
fn rd(d: &[u8], p: usize) -> Option<(&[u8], &[u8], usize)> {
    let id = d.get(p..p.checked_add(4)?)?;
    let n = u32::from_le_bytes(d.get(p + 4..p + 8)?.try_into().ok()?) as usize;
    let e = p.saturating_add(8).saturating_add(n).min(d.len());
    Some((
        id,
        d.get(p + 8..e)?,
        p.saturating_add(8).saturating_add(n).saturating_add(n & 1),
    ))
}

pub(crate) fn probe(src: &Src) -> Option<Parsed> {
    let mut out = Parsed {
        container: "avi",
        ..Parsed::default()
    };
    let h = src.get(0, 24)?;
    if h.get(12..16) != Some(b"LIST") || h.get(20..24) != Some(b"hdrl") {
        src.note("avi: no hdrl list first");
        return Some(out);
    }
    let len = u64::from(u32::from_le_bytes(h.get(16..20)?.try_into().ok()?));
    if !(4..=1 << 20).contains(&len) {
        src.note("avi: hdrl size out of range");
        return Some(out);
    }
    let d = src.get(24, len - 4)?;
    let mut us_per_frame = 0u32;
    let mut frames = 0u64;
    let mut video_len: Option<f64> = None;
    let mut p = 0usize;
    let mut idx = 0u64;
    for _ in 0..256 {
        let Some((id, body, next)) = rd(d, p) else {
            break;
        };
        match id {
            b"avih" if body.len() >= 40 => {
                let mut c = Cur::new(body);
                us_per_frame = c.le32().unwrap_or(0);
                let _ = c.skip(12);
                frames = c.le32().map_or(0, u64::from);
            }
            b"LIST" if body.get(..4) == Some(b"strl") => {
                let (t, dur) = strl(body, idx);
                idx += 1;
                // FFmpeg reports the longest stream.
                if let Some(d) = dur.filter(|d| d.is_finite() && *d > 0.0) {
                    video_len = Some(video_len.map_or(d, |v: f64| v.max(d)));
                }
                if out.tracks.len() < 64 {
                    out.tracks.push(t);
                }
            }
            b"LIST" if body.get(..4) == Some(b"odml") => {
                if let Some((b"dmlh", b2, _)) = rd(body, 4)
                    && let Some(n) = b2.get(..4).and_then(|x| x.try_into().ok())
                {
                    frames = frames.max(u64::from(u32::from_le_bytes(n)));
                }
            }
            _ => {}
        }
        p = next;
    }
    out.duration = video_len.or_else(|| {
        (us_per_frame > 0 && frames > 0).then(|| frames as f64 * f64::from(us_per_frame) / 1e6)
    });
    if out
        .tracks
        .iter()
        .any(|t| t.kind == Kind::Video && t.codec == "mpeg4")
    {
        first_video_chunk(src, 24 + len - 4, &mut out.tracks);
    }
    Some(out)
}

fn strl(body: &[u8], idx: u64) -> (Tk, Option<f64>) {
    let mut t = Tk {
        id: idx,
        ..Tk::default()
    };
    let mut q = 4usize;
    let mut dur = None;
    for _ in 0..32 {
        let Some((id2, b2, n2)) = rd(body, q) else {
            break;
        };
        match id2 {
            b"strh" if b2.len() >= 36 => {
                t.kind = match b2.get(..4) {
                    Some(b"vids") => Kind::Video,
                    Some(b"auds") => Kind::Audio,
                    Some(b"txts") => Kind::Sub,
                    _ => Kind::Other,
                };
                let mut c = Cur::new(b2.get(20..).unwrap_or(&[]));
                let scale = c.le32().unwrap_or(0);
                let rate = c.le32().unwrap_or(0);
                let _ = c.skip(4);
                let length = c.le32().unwrap_or(0);
                if scale > 0 && rate > 0 {
                    if t.kind == Kind::Video {
                        t.fps = Some(f64::from(rate) / f64::from(scale));
                    }
                    dur = Some(f64::from(length) * f64::from(scale) / f64::from(rate));
                }
            }
            b"strf" if t.kind == Kind::Video && b2.len() >= 20 => {
                let mut c = Cur::new(b2);
                let _ = c.skip(4);
                t.width = c.le32().filter(|w| *w > 0 && *w < 65536);
                t.height = c
                    .le32()
                    .map(|h| (h as i32).unsigned_abs())
                    .filter(|h| *h > 0 && *h < 65536);
                let _ = c.skip(4);
                let fc = c.take(4).unwrap_or(&[]).to_ascii_uppercase();
                super::vfw_codec(&fc, &mut t);
                // DivX XSUB bitmap subtitles travel as a `vids` stream (D123).
                if matches!(fc.as_slice(), b"DXSB" | b"DXSA") {
                    (t.kind, t.codec) = (Kind::Sub, "xsub".into());
                    (t.width, t.height, t.fps) = (None, None, None);
                }
            }
            b"strf" if t.kind == Kind::Audio && b2.len() >= 16 => {
                let mut c = Cur::new(b2);
                let mut tag = c.le16().unwrap_or(0);
                if tag == 0xfffe {
                    tag = b2
                        .get(24..26)
                        .and_then(|x| x.try_into().ok())
                        .map_or(0, u16::from_le_bytes);
                }
                t.channels = c
                    .le16()
                    .and_then(|v| u8::try_from(v).ok())
                    .filter(|c| *c > 0);
                t.sample_rate = c.le32().filter(|r| *r > 0);
                t.bitrate_kbps = c.le32().map(|b| u64::from(b) * 8 / 1000).filter(|b| *b > 0);
                t.layout = t.channels.map(|c| audio::layout_name(c, c >= 6));
                t.codec = match tag {
                    0x1 => "pcm_s16le".into(),
                    0x3 => "pcm_f32le".into(),
                    0x50 => "mp2".into(),
                    0x55 => "mp3".into(),
                    0xff | 0x1610 | 0x706d => "aac".into(),
                    0x2000 => "ac3".into(),
                    0x2001 => "dts".into(),
                    0x161 | 0x162 => "wmav2".into(),
                    0xf1ac => "flac".into(),
                    n => format!("?0x{n:x}"),
                };
            }
            b"strn" => t.name = Some(text(b2)).filter(|s| !s.is_empty()),
            _ => {}
        }
        q = n2;
    }
    (t, dur)
}

/// Walks the top-level RIFF chunks after hdrl to LIST movi and reads its first video chunk.
fn first_video_chunk(src: &Src, mut off: u64, tracks: &mut [Tk]) {
    let size = src.len();
    for _ in 0..16 {
        let Some(h) = src.get(off, 12) else {
            return;
        };
        let n = u64::from(u32::from_le_bytes(
            h.get(4..8)
                .and_then(|x| x.try_into().ok())
                .unwrap_or([0; 4]),
        ));
        if h.get(..4) == Some(b"LIST") && h.get(8..12) == Some(b"movi") {
            let mut p = off + 12;
            for _ in 0..64 {
                let Some(c) = src.get(p, 8) else {
                    return;
                };
                let id = c.get(..4).unwrap_or(&[]);
                let len = u64::from(u32::from_le_bytes(
                    c.get(4..8)
                        .and_then(|x| x.try_into().ok())
                        .unwrap_or([0; 4]),
                ));
                // ##dc / ##db: video data of stream ##.
                if id.get(2..4) == Some(b"dc") || id.get(2..4) == Some(b"db") {
                    let s: u64 = std::str::from_utf8(id.get(..2).unwrap_or(&[]))
                        .ok()
                        .and_then(|x| x.parse().ok())
                        .unwrap_or(u64::MAX);
                    if let Some(t) = tracks
                        .iter_mut()
                        .find(|t| t.id == s && t.kind == Kind::Video)
                        && len > 0
                    {
                        if let Some(f) = src.get(p + 8, len.min(4096)) {
                            let _ = codec::mpeg4_profile(f, t);
                        }
                        return;
                    }
                }
                if id == b"LIST" {
                    p += 12;
                    continue;
                }
                p = p
                    .saturating_add(8)
                    .saturating_add(len)
                    .saturating_add(len & 1);
                if p >= size {
                    return;
                }
            }
            return;
        }
        off = off
            .saturating_add(8)
            .saturating_add(n)
            .saturating_add(n & 1);
        if off >= size {
            return;
        }
    }
}

#[cfg(test)]
#[path = "avi_tests.rs"]
mod tests;

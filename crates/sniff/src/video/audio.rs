//! First-audio-frame parsers: AC-3, E-AC-3 (JOC / Atmos flag, dependent substreams), TrueHD /
//! MLP major sync (Atmos 16-channel presentation), DTS core and the DTS-HD extension substream
//! (XLL = Master Audio, XBR/X96 = High Resolution, LBR = Express, DTS:X sync in the XLL data),
//! AAC (AudioSpecificConfig, ADTS) and MPEG audio headers. Plus the MP4 `dac3` / `dec3` boxes.
//!
//! Atmos is reported the way FFmpeg decides it (and FFmpeg's profile strings are reused):
//! - TrueHD: the major sync says 4 substreams and bit 7 of `substream_info` (16-channel
//!   presentation) is set. The major sync checksum must match before anything is believed.
//! - E-AC-3: `flag_ec3_extension_type_a` in the first `addbsi` byte of an independent frame
//!   (ETSI TS 102 366 Annex E), or the same flag in the MP4 `dec3` box (Annex F).
//!
//! `atmos` is `Some(false)` only when the bytes that would carry the flag were parsed and it was
//! clear, and for codecs that cannot carry Atmos. It stays `None` when the frame was not reached
//! or did not parse. The Atmos vectors in the unit tests are built from the bitstream layout
//! (and checked against ffprobe); real TrueHD Atmos and E-AC-3 JOC streams (Dolby demos and
//! delivery-kit files, Blu-ray style AC-3 core + dependent) are checked in
//! `tests/real_samples.rs`.

use super::AudioFeature;
use super::bits::{Bits, Cur};

/// What a first frame (or a codec configuration record) says about an audio track.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct AudioInfo {
    pub codec: Option<&'static str>,
    pub profile: Option<String>,
    pub channels: Option<u8>,
    pub layout: Option<String>,
    pub sample_rate: Option<u32>,
    pub bit_depth: Option<u8>,
    pub bitrate_kbps: Option<u64>,
    pub atmos: Option<bool>,
    pub dts_x: Option<bool>,
    pub features: Vec<AudioFeature>,
}

// Channel mask bits (FFmpeg's AV_CH_* order; only used to count channels and name layouts).
const FL: u32 = 1;
const FR: u32 = 1 << 1;
const FC: u32 = 1 << 2;
const LFE: u32 = 1 << 3;
const BL: u32 = 1 << 4;
const BR: u32 = 1 << 5;
const FLC: u32 = 1 << 6;
const FRC: u32 = 1 << 7;
const BC: u32 = 1 << 8;
const SL: u32 = 1 << 9;
const SR: u32 = 1 << 10;
const TC: u32 = 1 << 11;
const TFL: u32 = 1 << 12;
const TFC: u32 = 1 << 13;
const TFR: u32 = 1 << 14;
const TBL: u32 = 1 << 15;
const TBR: u32 = 1 << 17;
const WL: u32 = 1 << 31;
const WR: u32 = 1 << 30;
const SDL: u32 = 1 << 29;
const SDR: u32 = 1 << 28;
const LFE2: u32 = 1 << 27;

fn count(mask: u32) -> u8 {
    u8::try_from(mask.count_ones()).unwrap_or(u8::MAX)
}

/// `5.1`, `7.1`, `stereo`, `mono`, ... from a channel count and an LFE flag.
pub(crate) fn layout_name(channels: u8, lfe: bool) -> String {
    match (channels, lfe) {
        (1, false) => "mono".into(),
        (2, false) => "stereo".into(),
        (n, true) if n >= 2 => format!("{}.1", n - 1),
        (n, _) => format!("{n}.0"),
    }
}

fn layout_from_mask(mask: u32) -> String {
    let lfe = count(mask & (LFE | LFE2));
    match (count(mask & !(LFE | LFE2)), lfe) {
        (1, 0) => "mono".into(),
        (2, 0) => "stereo".into(),
        (m, l) => format!("{m}.{l}"),
    }
}

// ---------------------------------------------------------------- AC-3 / E-AC-3

const AC3_RATES: [u32; 3] = [48000, 44100, 32000];
const AC3_KBPS: [u16; 19] = [
    32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384, 448, 512, 576, 640,
];

fn acmod_mask(acmod: u32, lfe: bool) -> u32 {
    let m = match acmod {
        0 | 2 => FL | FR,
        1 => FC,
        3 => FL | FC | FR,
        4 => FL | FR | BC,
        5 => FL | FC | FR | BC,
        6 => FL | FR | SL | SR,
        _ => FL | FC | FR | SL | SR,
    };
    if lfe { m | LFE } else { m }
}

/// E-AC-3 custom channel map (dependent substreams), MSB first.
const EAC3_MAP: [u32; 16] = [
    FL,
    FC,
    FR,
    SL,
    SR,
    FLC | FRC,
    BL | BR,
    BC,
    TC,
    SDL | SDR,
    WL | WR,
    TFL | TFR,
    TFC,
    TBL | TBR,
    LFE2,
    LFE,
];

/// One parsed (E-)AC-3 sync frame header.
#[derive(Clone, Debug, Default)]
struct Ac3Frame {
    eac3: bool,
    /// 0 independent, 1 dependent, 2 AC-3 converted.
    strmtyp: u32,
    substreamid: u32,
    frame_size: usize,
    sample_rate: u32,
    acmod: u32,
    lfe: bool,
    kbps: Option<u64>,
    chanmap: Option<u32>,
    /// `Some(flag)` when the bitstream info was parsed up to and including `addbsie`.
    ext_type_a: Option<bool>,
}

fn ac3_frame(f: &[u8]) -> Option<Ac3Frame> {
    if f.len() < 8 || f.first() != Some(&0x0b) || f.get(1) != Some(&0x77) {
        return None;
    }
    let bsid = f.get(5)? >> 3;
    let mut b = Bits::new(f);
    b.skip(16)?;
    if bsid <= 10 {
        b.skip(16)?; // crc1
        let fscod = b.u(2)?;
        let frmsizecod = b.u(6)?;
        let rate = *AC3_RATES.get(usize::try_from(fscod).ok()?)?;
        let kbps = u64::from(*AC3_KBPS.get(usize::try_from(frmsizecod >> 1).ok()?)?);
        let words: u64 = match fscod {
            0 => kbps * 2,
            1 => kbps * 2 * 320 / 294 + u64::from(frmsizecod & 1),
            _ => kbps * 3,
        };
        b.skip(5 + 3)?; // bsid, bsmod
        let acmod = b.u(3)?;
        if acmod & 1 == 1 && acmod != 1 {
            b.skip(2)?;
        }
        if acmod & 4 != 0 {
            b.skip(2)?;
        }
        if acmod == 2 {
            b.skip(2)?;
        }
        let lfe = b.flag()?;
        return Some(Ac3Frame {
            eac3: false,
            frame_size: usize::try_from(words * 2).ok()?,
            sample_rate: rate,
            acmod,
            lfe,
            kbps: Some(kbps),
            ext_type_a: Some(false),
            ..Ac3Frame::default()
        });
    }
    if !(11..=16).contains(&bsid) {
        return None;
    }
    let strmtyp = b.u(2)?;
    if strmtyp == 3 {
        return None;
    }
    let substreamid = b.u(3)?;
    let frame_size = (usize::try_from(b.u(11)?).ok()? + 1) * 2;
    let fscod = b.u(2)?;
    let (rate, nblk) = if fscod == 3 {
        let c2 = b.u(2)?;
        (*AC3_RATES.get(usize::try_from(c2).ok()?)? / 2, 6)
    } else {
        let n = *[1u32, 2, 3, 6].get(usize::try_from(b.u(2)?).ok()?)?;
        (*AC3_RATES.get(usize::try_from(fscod).ok()?)?, n)
    };
    let acmod = b.u(3)?;
    let lfe = b.flag()?;
    let mut fr = Ac3Frame {
        eac3: true,
        strmtyp,
        substreamid,
        frame_size,
        sample_rate: rate,
        acmod,
        lfe,
        kbps: Some(
            u64::try_from(frame_size).ok()? * 8 * u64::from(rate) / (256 * u64::from(nblk)) / 1000,
        ),
        ..Ac3Frame::default()
    };
    // The rest of the bitstream info (ETSI TS 102 366 E.1.2.2), up to addbsi. A failure here
    // leaves `ext_type_a` unknown but keeps the channel information read so far.
    fr.ext_type_a = eac3_tail(&mut b, &mut fr, fscod, nblk);
    Some(fr)
}

fn eac3_tail(b: &mut Bits, fr: &mut Ac3Frame, fscod: u32, nblk: u32) -> Option<bool> {
    let acmod = fr.acmod;
    let dual = if acmod == 0 { 2 } else { 1 };
    b.skip(5)?; // bsid
    for _ in 0..dual {
        b.skip(5)?; // dialnorm
        if b.flag()? {
            b.skip(8)?; // compr
        }
    }
    if fr.strmtyp == 1 && b.flag()? {
        fr.chanmap = Some(b.u(16)?);
    }
    if b.flag()? {
        // mixmdate
        if acmod > 2 {
            b.skip(2)?;
            if acmod & 1 != 0 {
                b.skip(6)?;
            }
            if acmod & 4 != 0 {
                b.skip(6)?;
            }
        }
        if fr.lfe && b.flag()? {
            b.skip(5)?;
        }
        if fr.strmtyp == 0 {
            for _ in 0..dual {
                if b.flag()? {
                    b.skip(6)?;
                }
            }
            if b.flag()? {
                b.skip(6)?;
            }
            match b.u(2)? {
                1 => b.skip(5)?,
                2 => b.skip(12)?,
                3 => {
                    let n = (usize::try_from(b.u(5)?).ok()? + 2) * 8;
                    b.skip(n)?;
                }
                _ => {}
            }
            if acmod < 2 {
                for _ in 0..dual {
                    if b.flag()? {
                        b.skip(14)?;
                    }
                }
            }
            if b.flag()? {
                for _ in 0..nblk {
                    if nblk == 1 || b.flag()? {
                        b.skip(5)?;
                    }
                }
            }
        }
    }
    if b.flag()? {
        // infomdate
        b.skip(3 + 2)?;
        if acmod == 2 {
            b.skip(4)?;
        }
        if acmod >= 6 {
            b.skip(2)?;
        }
        for _ in 0..dual {
            if b.flag()? {
                b.skip(8)?;
            }
        }
        if fscod != 3 {
            b.skip(1)?;
        }
    }
    if fr.strmtyp == 0 && nblk != 6 {
        b.skip(1)?;
    }
    if fr.strmtyp == 2 && (nblk == 6 || b.flag()?) {
        b.skip(6)?;
    }
    if !b.flag()? {
        return Some(false); // addbsie
    }
    let _addbsil = b.u(6)?;
    b.skip(7)?;
    b.flag()
}

/// AC-3 / E-AC-3 frames at the start of `buf` (a block, a sample or an ES buffer). The first
/// sync word within the first 64 bytes starts the chain; up to 16 frames are read so dependent
/// substreams (7.1 and up) add their channels. A Blu-ray style E-AC-3 stream is an AC-3 frame
/// (bsid 6, the core) followed by E-AC-3 dependent frames: it is reported as E-AC-3 with the
/// dependents' channels and their JOC flag.
pub(crate) fn ac3(buf: &[u8]) -> Option<AudioInfo> {
    let start = memchr::memmem::find(buf.get(..buf.len().min(64))?, &[0x0b, 0x77])?;
    let mut p = start;
    let mut base: Option<Ac3Frame> = None;
    let mut mask = 0u32;
    let mut frames = 0;
    let mut any_eac3 = false;
    let mut atmos: Option<bool> = None;
    let mut kbps = 0u64;
    while frames < 16 {
        let Some(f) = buf.get(p..).and_then(ac3_frame) else {
            break;
        };
        frames += 1;
        if f.frame_size < 8 {
            break;
        }
        match (&base, f.strmtyp, f.substreamid) {
            (None, 0 | 2, _) => {
                mask = acmod_mask(f.acmod, f.lfe);
                atmos = f.ext_type_a;
                kbps = f.kbps.unwrap_or(0);
                any_eac3 = f.eac3;
                base = Some(f.clone());
            }
            (Some(_), 1, 0) if f.eac3 => {
                any_eac3 = true;
                atmos = match (atmos, f.ext_type_a) {
                    (Some(true), _) | (_, Some(true)) => Some(true),
                    (Some(false), Some(false)) => Some(false),
                    _ => None,
                };
                kbps += f.kbps.unwrap_or(0);
                mask |= f.chanmap.map_or(0, |m| {
                    EAC3_MAP
                        .iter()
                        .enumerate()
                        .filter(|(i, _)| m & (1 << (15 - i)) != 0)
                        .fold(0, |a, (_, c)| a | c)
                });
                if f.chanmap.is_none() {
                    mask |= acmod_mask(f.acmod, f.lfe);
                }
            }
            // A second independent frame of substream 0 (or a second AC-3 frame): the next
            // sync frame, stop.
            (Some(_), 0, 0) => break,
            _ => {}
        }
        p = p.saturating_add(f.frame_size);
    }
    let f = base?;
    let channels = count(mask);
    let mut a = AudioInfo {
        codec: Some(if any_eac3 { "eac3" } else { "ac3" }),
        channels: Some(channels),
        layout: Some(layout_from_mask(mask)),
        sample_rate: Some(f.sample_rate),
        bitrate_kbps: if kbps > 0 { Some(kbps) } else { f.kbps },
        atmos,
        ..AudioInfo::default()
    };
    if a.atmos == Some(true) {
        a.profile = Some("Dolby Digital Plus + Dolby Atmos".into());
        a.features = vec![AudioFeature::Atmos, AudioFeature::Joc];
    }
    Some(a)
}

/// MP4 `dac3` body: channels.
pub(crate) fn dac3(body: &[u8]) -> Option<AudioInfo> {
    let mut b = Bits::new(body);
    let fscod = b.u(2)?;
    b.skip(5 + 3)?;
    let acmod = b.u(3)?;
    let lfe = b.flag()?;
    let kbps = AC3_KBPS.get(usize::try_from(b.u(5)?).ok()?).copied();
    let mask = acmod_mask(acmod, lfe);
    Some(AudioInfo {
        codec: Some("ac3"),
        channels: Some(count(mask)),
        layout: Some(layout_from_mask(mask)),
        sample_rate: AC3_RATES.get(usize::try_from(fscod).ok()?).copied(),
        bitrate_kbps: kbps.map(u64::from),
        atmos: Some(false),
        ..AudioInfo::default()
    })
}

/// MP4 `dec3` body (ETSI TS 102 366 F.6): channels of the first independent substream plus its
/// dependents; the trailing `flag_ec3_extension_type_a` when present.
pub(crate) fn dec3(body: &[u8]) -> Option<AudioInfo> {
    let mut b = Bits::new(body);
    let kbps = b.u(13)?;
    let num_ind = b.u(3)? + 1;
    let mut mask = 0;
    let mut rate = None;
    for i in 0..num_ind {
        let fscod = b.u(2)?;
        b.skip(5 + 1 + 1 + 3)?; // bsid, reserved, asvc, bsmod
        let acmod = b.u(3)?;
        let lfe = b.flag()?;
        b.skip(3)?;
        let num_dep = b.u(4)?;
        let chan_loc = if num_dep > 0 {
            b.u(9)?
        } else {
            b.skip(1).map(|_| 0)?
        };
        if i == 0 {
            rate = AC3_RATES.get(usize::try_from(fscod).ok()?).copied();
            mask = acmod_mask(acmod, lfe);
            // chan_loc: Lc/Rc, Lrs/Rrs, Cs, Ts, Lsd/Rsd, Lw/Rw, Lvh/Rvh, Cvh, LFE2 (MSB first),
            // i.e. chanmap entries 5..=12 and 14.
            for n in 0..9usize {
                if chan_loc & (1 << (8 - n)) != 0 {
                    let idx = if n == 8 { 14 } else { 5 + n };
                    mask |= EAC3_MAP.get(idx).copied().unwrap_or(0);
                }
            }
        }
    }
    // Optional extension: reserved(7) flag_ec3_extension_type_a(1) complexity_index(8).
    let ext = if b.left() >= 16 {
        b.skip(7)?;
        b.flag()
    } else {
        None
    };
    let mut a = AudioInfo {
        codec: Some("eac3"),
        channels: Some(count(mask)),
        layout: Some(layout_from_mask(mask)),
        sample_rate: rate,
        bitrate_kbps: Some(u64::from(kbps)),
        // An absent extension does not mean "no Atmos" (older muxers omit it): first frame.
        atmos: ext.filter(|e| *e),
        ..AudioInfo::default()
    };
    if a.atmos == Some(true) {
        a.profile = Some("Dolby Digital Plus + Dolby Atmos".into());
        a.features = vec![AudioFeature::Atmos, AudioFeature::Joc];
    }
    Some(a)
}

// ---------------------------------------------------------------- TrueHD / MLP

/// CRC-16 (polynomial 0x002D, MSB first, init 0) used by the MLP major sync.
fn crc16_2d(data: &[u8]) -> u16 {
    let mut c: u16 = 0;
    for &x in data {
        c ^= u16::from(x) << 8;
        for _ in 0..8 {
            c = if c & 0x8000 != 0 {
                (c << 1) ^ 0x002d
            } else {
                c << 1
            };
        }
    }
    c
}

const THD_CHANS: [u8; 13] = [2, 1, 1, 2, 2, 2, 2, 1, 1, 2, 2, 1, 1];

fn thd_channels(arr: u32) -> (u8, bool) {
    let mut n = 0u8;
    for (i, c) in THD_CHANS.iter().enumerate() {
        if arr & (1 << i) != 0 {
            n = n.saturating_add(*c);
        }
    }
    // Bit 2 is LFE, bit 12 LFE2.
    (n, arr & 0b100 != 0 || arr & (1 << 12) != 0)
}

/// A major sync at `ms` (the 0xF8726FBx word): checked against its checksum.
fn major_sync(ms: &[u8]) -> Option<AudioInfo> {
    let head = ms.get(..28)?;
    let mut size = 28usize;
    if head.get(25).is_some_and(|b| b & 1 == 1) {
        size += 2 + usize::from(head.get(26)? >> 4) * 2;
    }
    let h = ms.get(..size)?;
    let crc = crc16_2d(h.get(..size - 4)?)
        ^ u16::from_be_bytes(h.get(size - 4..size - 2)?.try_into().ok()?);
    if crc != u16::from_be_bytes(h.get(size - 2..size)?.try_into().ok()?) {
        return None;
    }
    let mut b = Bits::new(h);
    b.skip(24)?;
    let stream_type = b.u(8)?;
    let mut a = AudioInfo::default();
    let ratebits;
    if stream_type == 0xba {
        ratebits = b.u(4)?;
        b.skip(4 + 2 + 2)?;
        let arr6 = b.u(5)?;
        b.skip(2)?;
        let arr8 = b.u(13)?;
        let (ch, lfe) = if arr8 != 0 {
            thd_channels(arr8)
        } else {
            thd_channels(arr6)
        };
        a.codec = Some("truehd");
        a.channels = Some(ch);
        a.layout = Some(layout_name(ch, lfe));
    } else if stream_type == 0xbb {
        let bits = b.u(4)?;
        b.skip(4)?;
        ratebits = b.u(4)?;
        a.codec = Some("mlp");
        a.bit_depth = match bits {
            0 => Some(16),
            1 => Some(20),
            2 => Some(24),
            _ => None,
        };
        b.skip(4 + 11)?;
        let _ = b.u(5)?;
    } else {
        return None;
    }
    if ratebits != 0xf {
        let base = if ratebits & 8 != 0 { 44100u32 } else { 48000 };
        a.sample_rate = base.checked_shl(ratebits & 7);
    }
    let mut c = Cur::new(h);
    c.skip(14)?;
    let peak = c.u16()? & 0x7fff;
    let subs = c.u8()?;
    let num_substreams = subs >> 4;
    let substream_info = c.u8()?;
    // peak_data_rate (in sample_rate / 16 bit/s) is a peak, not a bitrate: not reported.
    let _ = peak;
    a.features.push(AudioFeature::Lossless);
    if stream_type == 0xba {
        let spatial = substream_info & 0x80 != 0;
        a.atmos = match (spatial, num_substreams) {
            (true, 4) => Some(true),
            (true, _) => None, // inconsistent: 16-channel presentation without its substream
            (false, _) => Some(false),
        };
        if a.atmos == Some(true) {
            a.profile = Some("Dolby TrueHD + Dolby Atmos".into());
            a.features.push(AudioFeature::Atmos);
        }
    } else {
        a.atmos = Some(false);
    }
    Some(a)
}

/// The first valid TrueHD / MLP major sync in `buf` (an AU starts with 4 bytes of header, so
/// the sync sits at +4; Blu-ray streams interleave AC-3 frames, hence the search).
pub(crate) fn truehd(buf: &[u8]) -> Option<AudioInfo> {
    let lim = buf.get(..buf.len().min(256 * 1024))?;
    let mut from = 0;
    for _ in 0..64 {
        let i = from + memchr::memmem::find(lim.get(from..)?, &[0xf8, 0x72, 0x6f])?;
        if let Some(a) = lim.get(i..).and_then(major_sync) {
            return Some(a);
        }
        from = i + 3;
    }
    None
}

// ---------------------------------------------------------------- DTS
//
// Written from ETSI TS 102 114. The DTS-HD extension substream (MA / HRA) and DTS:X (a
// sync-word search in the XLL data) are checked on real streams in `tests/real_samples.rs`
// (DTS:X demos in MKV / M2TS / MKA / raw, DTS-HD HRA 7.1 96 kHz); DTS Express is UNVERIFIED.

const DTS_CORE: [u8; 4] = [0x7f, 0xfe, 0x80, 0x01];
const DTS_EXSS: [u8; 4] = [0x64, 0x58, 0x20, 0x25];
const DTS_AMODE_CH: [u8; 16] = [1, 2, 2, 2, 2, 3, 3, 4, 4, 5, 6, 6, 6, 7, 8, 8];
const DTS_RATES: [u32; 16] = [
    0, 8000, 16000, 32000, 0, 0, 11025, 22050, 44100, 0, 0, 12000, 24000, 48000, 0, 0,
];
const DTS_EXSS_RATES: [u32; 16] = [
    8000, 16000, 32000, 64000, 128000, 22050, 44100, 88200, 176400, 352800, 12000, 24000, 48000,
    96000, 192000, 384000,
];
const DTS_KBPS: [u16; 29] = [
    32, 56, 64, 96, 112, 128, 192, 224, 256, 320, 384, 448, 512, 576, 640, 768, 960, 1024, 1152,
    1280, 1344, 1408, 1411, 1472, 1536, 0, 0, 0, 0,
];

// Coding component mask (ETSI TS 102 114 7.5.4, FFmpeg's DCA_CSS_* / DCA_EXSS_*).
const EXSS_XBR: u32 = 0x020;
const EXSS_XXCH: u32 = 0x040;
const EXSS_X96: u32 = 0x080;
const EXSS_LBR: u32 = 0x100;
const EXSS_XLL: u32 = 0x200;

#[derive(Default)]
struct Exss {
    mask: u32,
    channels: Option<u8>,
    sample_rate: Option<u32>,
    bit_depth: Option<u8>,
    /// Header end and total size (bytes from the sync word).
    header: usize,
    size: usize,
}

fn exss(f: &[u8]) -> Option<Exss> {
    if f.get(..4)? != DTS_EXSS {
        return None;
    }
    let mut b = Bits::new(f);
    b.skip(32 + 8)?;
    let index = b.u(2)?;
    let wide = b.flag()?;
    let header = usize::try_from(b.u(8 + 4 * u32::from(wide))?).ok()? + 1;
    let nbits = 16 + 4 * u32::from(wide);
    let size = usize::try_from(b.u(nbits)?).ok()? + 1;
    let mut x = Exss {
        header,
        size,
        ..Exss::default()
    };
    let static_fields = b.flag()?;
    let mut mix_enabled = false;
    let mut mix_configs: Vec<u32> = Vec::new();
    if static_fields {
        b.skip(2 + 3)?;
        if b.flag()? {
            b.skip(36)?;
        }
        let npresents = b.u(3)? + 1;
        let nassets = b.u(3)? + 1;
        if npresents > 1 || nassets > 1 {
            return Some(x); // multi-asset streams: header sizes only
        }
        let mut active = 0;
        for _ in 0..npresents {
            active = b.u(index + 1)?;
        }
        for j in 0..=index {
            if active & (1 << j) != 0 {
                b.skip(8)?;
            }
        }
        mix_enabled = b.flag()?;
        if mix_enabled {
            b.skip(2)?;
            let nb = (b.u(2)? + 1) << 2;
            let n = b.u(2)? + 1;
            for _ in 0..n {
                mix_configs.push(b.u(nb)?.count_ones());
            }
        }
    }
    b.skip(usize::try_from(nbits).ok()?)?; // asset size
    // Asset descriptor.
    b.skip(9 + 3)?;
    let mut embedded_stereo = false;
    let mut embedded_6ch = false;
    let mut total = 0;
    if static_fields {
        if b.flag()? {
            b.skip(4)?;
        }
        if b.flag()? {
            b.skip(24)?;
        }
        if b.flag()? {
            let n = usize::try_from(b.u(10)?).ok()? + 1;
            b.skip(n * 8)?;
        }
        x.bit_depth = u8::try_from(b.u(5)? + 1).ok();
        x.sample_rate = DTS_EXSS_RATES.get(usize::try_from(b.u(4)?).ok()?).copied();
        total = b.u(8)? + 1;
        x.channels = u8::try_from(total).ok();
        if b.flag()? {
            embedded_stereo = total > 2 && b.flag()?;
            embedded_6ch = total > 6 && b.flag()?;
            let mut nb = 0;
            if b.flag()? {
                nb = (b.u(2)? + 1) << 2;
                b.skip(usize::try_from(nb).ok()?)?;
            }
            let nsets = b.u(3)?;
            if nsets > 0 && nb == 0 {
                return Some(x);
            }
            let mut spk = Vec::new();
            for _ in 0..nsets {
                spk.push(b.u(nb)?.count_ones());
            }
            for n in spk {
                let nch = b.u(5)? + 1;
                for _ in 0..n {
                    let m = b.u(nch)?;
                    b.skip(usize::try_from(m.count_ones() * 5).ok()?)?;
                }
            }
        } else {
            b.skip(3)?;
        }
    }
    let drc = b.flag()?;
    if drc {
        b.skip(8)?;
    }
    if b.flag()? {
        b.skip(5)?;
    }
    if drc && embedded_stereo {
        b.skip(8)?;
    }
    if mix_enabled && b.flag()? {
        b.skip(1 + 6)?;
        if b.u(2)? == 3 {
            b.skip(8)?;
        } else {
            b.skip(3)?;
        }
        if b.flag()? {
            for n in &mix_configs {
                b.skip(usize::try_from(6 * n).ok()?)?;
            }
        } else {
            b.skip(6 * mix_configs.len())?;
        }
        let mut dmix = total;
        if embedded_6ch {
            dmix += 6;
        }
        if embedded_stereo {
            dmix += 2;
        }
        for n in &mix_configs {
            for _ in 0..dmix.min(64) {
                let m = b.u(*n)?;
                b.skip(usize::try_from(m.count_ones() * 6).ok()?)?;
            }
        }
    }
    x.mask = match b.u(2)? {
        0 => b.u(12)?,
        1 => EXSS_XLL,
        2 => EXSS_LBR,
        _ => 0,
    };
    Some(x)
}

/// DTS core frame (and the DTS-HD extension substream after it), or an extension substream
/// alone (DTS Express / DTS-HD without core).
pub(crate) fn dts(buf: &[u8]) -> Option<AudioInfo> {
    let lim = buf.get(..buf.len().min(256 * 1024))?;
    let core_at = memchr::memmem::find(lim.get(..lim.len().min(4096))?, &DTS_CORE);
    let exss_from;
    let mut a = AudioInfo {
        codec: Some("dts"),
        atmos: Some(false),
        ..AudioInfo::default()
    };
    let mut core_mask = 0u32;
    if let Some(c) = core_at {
        let f = lim.get(c..)?;
        let mut b = Bits::new(f);
        b.skip(32 + 1 + 5 + 1 + 7)?;
        let fsize = usize::try_from(b.u(14)?).ok()? + 1;
        let amode = b.u(6)?;
        let sfreq = b.u(4)?;
        let rate = b.u(5)?;
        b.skip(1 + 1 + 1 + 1 + 1)?;
        let ext_id = b.u(3)?;
        let ext = b.flag()?;
        b.skip(1)?;
        let lff = b.u(2)?;
        let lfe = lff == 1 || lff == 2;
        let mut ch = DTS_AMODE_CH
            .get(usize::try_from(amode).ok()?)
            .copied()
            .unwrap_or(2);
        if ext && ext_id == 0 {
            core_mask |= 0x008; // XCh: one more surround channel (6.1)
            ch = ch.saturating_add(1);
        } else if ext && ext_id == 2 {
            core_mask |= 0x004; // X96
        } else if ext && ext_id == 6 {
            core_mask |= 0x002; // XXCh
        }
        let ch = ch.saturating_add(u8::from(lfe));
        a.channels = Some(ch);
        a.layout = Some(layout_name(ch, lfe));
        a.sample_rate = DTS_RATES
            .get(usize::try_from(sfreq).ok()?)
            .copied()
            .filter(|r| *r > 0);
        a.bitrate_kbps = DTS_KBPS
            .get(usize::try_from(rate).ok()?)
            .copied()
            .filter(|k| *k > 0)
            .map(u64::from);
        exss_from = c.saturating_add(fsize);
    } else {
        exss_from = memchr::memmem::find(lim.get(..lim.len().min(4096))?, &DTS_EXSS)?;
    }
    let ex = lim.get(exss_from..).and_then(exss);
    let mask = ex.as_ref().map_or(0, |e| e.mask);
    let profile = if mask & EXSS_XLL != 0 {
        a.features.push(AudioFeature::DtsHdMa);
        a.features.push(AudioFeature::Lossless);
        let e = ex.as_ref()?;
        let body = lim
            .get(exss_from.saturating_add(e.header)..exss_from.saturating_add(e.size))
            .unwrap_or(&[]);
        let x = memchr::memmem::find(body, &[0x02, 0x00, 0x08, 0x50]).is_some();
        let imax = body.windows(4).any(|w| {
            w.first() == Some(&0xf1)
                && w.get(1) == Some(&0x40)
                && w.get(2) == Some(&0x00)
                && w.get(3).is_some_and(|b| b & 0xfe == 0xd0)
        });
        a.dts_x = Some(x || imax);
        if x || imax {
            a.features.push(AudioFeature::DtsX);
        }
        if imax {
            "DTS-HD MA + DTS:X IMAX"
        } else if x {
            "DTS-HD MA + DTS:X"
        } else {
            "DTS-HD MA"
        }
    } else {
        a.dts_x = if ex.is_some() || core_at.is_some() {
            Some(false)
        } else {
            None
        };
        if mask & (EXSS_XBR | EXSS_XXCH | EXSS_X96) != 0 {
            a.features.push(AudioFeature::DtsHdHra);
            "DTS-HD HRA"
        } else if mask & EXSS_LBR != 0 {
            "DTS Express"
        } else if core_mask & 0x004 != 0 {
            "DTS 96/24"
        } else if core_mask & 0x00a != 0 {
            "DTS-ES"
        } else {
            "DTS"
        }
    };
    a.profile = Some(profile.into());
    if let Some(e) = ex {
        if let Some(c) = e.channels {
            let lfe = a.layout.as_deref().is_some_and(|l| l.ends_with(".1"));
            a.channels = Some(c);
            a.layout = Some(layout_name(c, lfe && c > 2));
        }
        a.sample_rate = e.sample_rate.or(a.sample_rate);
        a.bit_depth = e.bit_depth;
    }
    Some(a)
}

// ---------------------------------------------------------------- AAC, MPEG audio

const AAC_RATES: [u32; 13] = [
    96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000, 7350,
];
const AAC_CH: [u8; 8] = [0, 1, 2, 3, 4, 5, 6, 8];

fn aac_profile(aot: u32) -> String {
    match aot {
        1 => "Main".into(),
        2 => "LC".into(),
        3 => "SSR".into(),
        4 => "LTP".into(),
        5 => "HE-AAC".into(),
        29 => "HE-AACv2".into(),
        23 => "LD".into(),
        39 => "ELD".into(),
        42 => "xHE-AAC".into(),
        n => format!("AOT {n}"),
    }
}

/// AudioSpecificConfig (MP4 `esds`, Matroska CodecPrivate).
pub(crate) fn aac_asc(asc: &[u8]) -> Option<AudioInfo> {
    let mut b = Bits::new(asc);
    let mut aot = b.u(5)?;
    if aot == 31 {
        aot = 32 + b.u(6)?;
    }
    let idx = b.u(4)?;
    let rate = if idx == 15 {
        Some(b.u(24)?)
    } else {
        AAC_RATES.get(usize::try_from(idx).ok()?).copied()
    };
    let cc = b.u(4)?;
    let mut profile = aot;
    // Explicit SBR / PS signalling.
    if aot == 5 || aot == 29 {
        let ext_idx = b.u(4)?;
        if ext_idx == 15 {
            b.skip(24)?;
        }
        let _ = b.u(5);
        profile = aot;
    }
    let ch = AAC_CH
        .get(usize::try_from(cc).ok()?)
        .copied()
        .filter(|c| *c > 0);
    Some(AudioInfo {
        codec: Some("aac"),
        profile: Some(aac_profile(profile)),
        channels: ch,
        layout: ch.map(|c| layout_name(c, c >= 6)),
        sample_rate: rate,
        atmos: Some(false),
        ..AudioInfo::default()
    })
}

/// ADTS header (MPEG-TS AAC).
pub(crate) fn adts(buf: &[u8]) -> Option<AudioInfo> {
    let i = buf
        .windows(2)
        .take(64)
        .position(|w| w.first() == Some(&0xff) && w.get(1).is_some_and(|b| b & 0xf6 == 0xf0))?;
    let mut b = Bits::new(buf.get(i..)?);
    b.skip(16)?;
    let aot = b.u(2)? + 1;
    let idx = b.u(4)?;
    b.skip(1)?;
    let cc = b.u(3)?;
    let ch = AAC_CH
        .get(usize::try_from(cc).ok()?)
        .copied()
        .filter(|c| *c > 0);
    Some(AudioInfo {
        codec: Some("aac"),
        profile: Some(aac_profile(aot)),
        channels: ch,
        layout: ch.map(|c| layout_name(c, c >= 6)),
        sample_rate: AAC_RATES.get(usize::try_from(idx).ok()?).copied(),
        atmos: Some(false),
        ..AudioInfo::default()
    })
}

/// MPEG-1/2 audio frame header: layer (mp2 / mp3), channels, sample rate, bitrate.
pub(crate) fn mpa(buf: &[u8]) -> Option<AudioInfo> {
    let i = buf.windows(2).take(64).position(|w| {
        w.first() == Some(&0xff) && w.get(1).is_some_and(|b| b & 0xe0 == 0xe0 && b & 0x06 != 0)
    })?;
    let mut b = Bits::new(buf.get(i..)?);
    b.skip(11)?;
    let ver = b.u(2)?;
    let layer = b.u(2)?;
    b.skip(1)?;
    let br = b.u(4)?;
    let sr = b.u(2)?;
    b.skip(2 + 1)?;
    let mode = b.u(2)?;
    if ver == 1 || sr == 3 || br == 15 {
        return None;
    }
    let base = *[44100u32, 48000, 32000].get(usize::try_from(sr).ok()?)?;
    let rate = match ver {
        3 => base,
        2 => base / 2,
        _ => base / 4,
    };
    let codec = match layer {
        1 => "mp3",
        2 => "mp2",
        _ => "mp1",
    };
    let ch = if mode == 3 { 1 } else { 2 };
    Some(AudioInfo {
        codec: Some(codec),
        channels: Some(ch),
        layout: Some(layout_name(ch, false)),
        sample_rate: Some(rate),
        atmos: Some(false),
        ..AudioInfo::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layouts() {
        assert_eq!(layout_from_mask(FL | FR), "stereo");
        assert_eq!(layout_from_mask(FL | FR | FC | LFE | SL | SR), "5.1");
        assert_eq!(
            layout_from_mask(FL | FR | FC | LFE | SL | SR | BL | BR),
            "7.1"
        );
        assert_eq!(layout_name(8, true), "7.1");
    }

    #[test]
    fn crc_matches_ffmpeg_truehd() {
        // Major sync written by FFmpeg's TrueHD encoder (5.1, 48 kHz, 2 substreams).
        let ms = [
            0xf8, 0x72, 0x6f, 0xba, 0x00, 0x07, 0x80, 0x0f, 0xb7, 0x52, 0x00, 0x00, 0x00, 0x00,
            0x8c, 0x7f, 0x20, 0x3c, 0, 0, 0, 0, 0, 0, 0, 0, 0xe4, 0xd1,
        ];
        let a = major_sync(&ms).unwrap();
        assert_eq!(a.codec, Some("truehd"));
        assert_eq!(a.channels, Some(6));
        assert_eq!(a.sample_rate, Some(48000));
        assert_eq!(a.atmos, Some(false));
        let mut bad = ms;
        bad[17] |= 0x80;
        assert_eq!(
            major_sync(&bad),
            None,
            "checksum must catch the flipped bit"
        );
    }
}

//! Video quality recon: container, codec, profile, resolution, frame rate, bit depth, HDR
//! (HDR10 / HDR10+ / HLG / Dolby Vision profile), duration, bitrate, audio tracks (codec,
//! channels, language, Atmos / JOC / TrueHD / DTS-HD / DTS:X where determinable) and subtitle
//! tracks, from the same sparse ranges as the type detector. Audio and subtitle tracks carry
//! their kinds (SDH, forced, commentary, audio description, dub, ...: [`TrackKind`], D123).
//!
//! Containers: Matroska / WebM ([`mkv`]), MP4 / MOV including `moov` at the end and fragmented
//! files ([`mp4`]), MPEG-TS / M2TS ([`ts`]) and AVI ([`avi`]). Codec records and first-frame
//! parsing live in `codec` (video) and `audio` (premium audio: TrueHD / E-AC-3 Atmos, DTS-HD).
//!
//! Protocol: like the detector, [`analyze`] never does I/O. It reads what the [`SparseFile`]
//! holds and returns what it could work out plus the ranges it still wants (MKV Tracks behind a
//! SeekHead, an MP4 `moov` at the end, the first audio frame behind a large video keyframe, the
//! TS tail for the last timestamp). Requests are rounded to 16 KiB torrent blocks. Walkers stop
//! a chain at the first missing piece but carry on with independent chains, so one pass asks
//! for several ranges. [`probe`] is the same pass folded into a [`VideoOutcome`].
//!
//! Values come from the bitstream, not from names; a value that the fetched bytes do not
//! decide stays `None` (in particular [`AudioTrack::atmos`]).

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::Ordering;

use serde::Serialize;

use crate::sparse::{Range, SparseFile};
use crate::types::Caps;

mod audio;
mod avi;
mod bits;
mod codec;
mod kind;
mod mkv;
mod mp4;
mod src;
#[cfg(test)]
mod testkit;
mod ts;

pub use kind::{NameClass, NameTrack, TrackKind, classify_name};
use src::Src;

/// Result of one recon pass.
#[derive(Clone, Debug, PartialEq)]
pub enum VideoOutcome {
    Info(Box<VideoInfo>),
    /// Fetch these ranges and call again.
    NeedMore(Vec<Range>),
    /// Container not handled (yet), or not a video container.
    Unsupported,
}

/// What one pass found, and what it still wants.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Recon {
    /// `None` when the container was not recognised, or not enough was known to say anything.
    pub info: Option<VideoInfo>,
    /// Ranges that would complete or refine `info` (block-aligned, sorted, not yet known),
    /// held to `caps.max_ranges`, `caps.max_range` and `caps.max_bytes`.
    pub need: Vec<Range>,
    /// Some byte the pass read was inserted with [`SparseFile::insert_unverified`]: the
    /// values in `info` may come from a peer that could not be checked.
    pub unverified: bool,
}

/// What a video file holds.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct VideoInfo {
    /// `mkv`, `webm`, `mp4`, `mov`, `avi`, `ts`, `m2ts`.
    pub container: String,
    /// Major brand (MP4) or doctype (Matroska).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub brand: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// Overall bitrate (file size / duration).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bitrate_kbps: Option<u64>,
    /// Fragmented MP4 (`mvex` in `moov`).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub fragmented: bool,
    pub video: Vec<VideoTrack>,
    pub audio: Vec<AudioTrack>,
    pub subtitles: Vec<SubtitleTrack>,
    /// Parser remarks ("first audio frame of track 3 not reached", ...).
    pub notes: Vec<String>,
}

/// HDR signalling found in the container, the codec configuration or the first frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HdrFormat {
    /// PQ transfer (SMPTE ST 2084).
    Hdr10,
    /// HDR10 plus SMPTE ST 2094-40 dynamic metadata (seen in the first frame).
    Hdr10Plus,
    /// ARIB STD-B67 transfer.
    Hlg,
    /// Dolby Vision (profile in [`DolbyVision`] when a configuration record was found).
    DolbyVision,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct DolbyVision {
    pub profile: u8,
    pub level: u8,
    /// Base-layer compatibility ID (profile 8.x: 1 = HDR10, 2 = SDR, 4 = HLG).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compatibility: Option<u8>,
    /// RPU present flag of the configuration record.
    pub rpu: bool,
    /// Enhancement layer present (profile 7 dual layer).
    pub enhancement_layer: bool,
    /// Base layer present.
    pub base_layer: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct VideoTrack {
    /// Track number (Matroska), track ID (MP4), PID (TS) or stream index (AVI).
    pub id: u64,
    /// FFmpeg codec names: `h264`, `hevc`, `av1`, `vp9`, `vp8`, `mpeg2video`, `mpeg4`, `vc1`, ...
    pub codec: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub level: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fps: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bit_depth: Option<u8>,
    /// `4:2:0`, `4:2:2`, `4:4:4`, `4:0:0`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chroma: Option<String>,
    /// H.273 colour primaries / transfer / matrix codes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub primaries: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transfer: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matrix: Option<u8>,
    pub hdr: Vec<HdrFormat>,
    /// SMPTE ST 2086 mastering display metadata present (container or first frame).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub mastering_display: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_cll: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_fall: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dolby_vision: Option<DolbyVision>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bitrate_kbps: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub default: bool,
}

/// Object-based / lossless extensions recognised in an audio stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioFeature {
    /// Dolby Atmos (TrueHD with the 16-channel presentation, or E-AC-3 JOC).
    Atmos,
    /// E-AC-3 joint object coding.
    Joc,
    DtsHdMa,
    DtsHdHra,
    DtsX,
    Lossless,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct AudioTrack {
    pub id: u64,
    /// FFmpeg codec names: `aac`, `ac3`, `eac3`, `truehd`, `mlp`, `dts`, `flac`, `opus`, `mp3`,
    /// `dtsuhd` (DTS-UHD, MP4 `dtsx` / `dtsy`; ffmpeg has no name for it),
    /// `mp2`, `vorbis`, `pcm`, ...
    pub codec: String,
    /// FFmpeg profile string where it has one (`LC`, `DTS-HD MA`, `Dolby TrueHD + Dolby Atmos`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channels: Option<u8>,
    /// `5.1`, `7.1`, `stereo`, ...
    #[serde(skip_serializing_if = "Option::is_none")]
    pub layout: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sample_rate: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bit_depth: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bitrate_kbps: Option<u64>,
    pub features: Vec<AudioFeature>,
    /// `Some(true)` / `Some(false)` when the bitstream decides it (or the codec cannot carry
    /// Atmos); `None` when the first frame was not reached or did not parse.
    pub atmos: Option<bool>,
    /// DTS:X, decided like `atmos` (only meaningful for DTS).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dts_x: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Resolved kinds (D123): container / bitstream flags plus the name classifier. Sorted.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub kinds: Vec<TrackKind>,
    /// The subset of `kinds` that only the name gave.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub name_kinds: Vec<TrackKind>,
    pub default: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct SubtitleTrack {
    pub id: u64,
    /// FFmpeg codec names: `subrip`, `ass`, `webvtt`, `hdmv_pgs_subtitle`, `dvd_subtitle`,
    /// `mov_text`, `dvb_subtitle`, `dvb_teletext`, `eia_608`, `eia_708`, `ttml`,
    /// `arib_caption`, `usf`, `xsub`, ...
    pub codec: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Resolved kinds (D123): container / bitstream flags plus the name classifier. Sorted.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub kinds: Vec<TrackKind>,
    /// The subset of `kinds` that only the name gave.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub name_kinds: Vec<TrackKind>,
    pub default: bool,
    /// The container's forced flag as written (Matroska FlagForced); the resolved meaning is
    /// [`TrackKind::Forced`] in `kinds`.
    pub forced: bool,
}

/// Video for Windows FOURCC (AVI `strh`/`strf`, Matroska `V_MS/VFW/FOURCC`), upper-cased, to
/// the FFmpeg codec name; raw YUV FOURCCs also give the chroma and bit depth. A zero FOURCC
/// (BI_RGB) is uncompressed RGB.
pub(crate) fn vfw_codec(fc: &[u8], t: &mut Tk) {
    let yuv = |t: &mut Tk, c: u32| {
        t.chroma = codec::chroma_name(c);
        t.bit_depth = Some(8);
    };
    t.codec = match fc {
        b"XVID" | b"DIVX" | b"DX50" | b"FMP4" | b"MP4V" | b"3IV2" => "mpeg4".into(),
        b"H264" | b"X264" | b"AVC1" => "h264".into(),
        b"HEVC" | b"H265" | b"HVC1" | b"X265" => "hevc".into(),
        b"DIV3" | b"MP43" => "msmpeg4v3".into(),
        b"MP42" | b"DIV2" => "msmpeg4v2".into(),
        b"MP41" | b"MPG4" | b"DIV1" => "msmpeg4v1".into(),
        b"MJPG" => "mjpeg".into(),
        b"WMV1" => "wmv1".into(),
        b"WMV2" => "wmv2".into(),
        b"WMV3" => "wmv3".into(),
        b"WVC1" => "vc1".into(),
        b"VP80" => "vp8".into(),
        b"VP90" => "vp9".into(),
        b"AV01" => "av1".into(),
        b"MPG2" | b"MPEG" => "mpeg2video".into(),
        b"FFV1" => "ffv1".into(),
        b"HFYU" => "huffyuv".into(),
        b"CVID" => "cinepak".into(),
        b"DVSD" => "dvvideo".into(),
        b"I420" | b"IYUV" | b"YV12" => {
            yuv(t, 1);
            "rawvideo".into()
        }
        b"YUY2" | b"UYVY" | b"YUYV" => {
            yuv(t, 2);
            "rawvideo".into()
        }
        [0, 0, 0, 0] => "rawvideo".into(),
        _ => format!("?{}", String::from_utf8_lossy(fc)),
    };
}

/// Labels the engine hands to [`analyze`].
pub fn is_video_container(label: &str) -> bool {
    matches!(
        label,
        "video.mkv"
            | "video.webm"
            | "video.mp4"
            | "video.mov"
            | "video.3gp"
            | "video.avi"
            | "video.ts"
            | "video.m2ts"
            | "video.mpeg_ps"
            | "video.flv"
            | "video.asf"
            | "video.ogv"
            // Audio-only containers and raw streams: the audio recon (Atmos, DTS:X, channels).
            | "audio.mka"
            | "audio.m4a"
            | "audio.ac3"
            | "audio.eac3"
            | "audio.dts"
            | "audio.truehd"
            | "audio.mlp"
    )
}

/// Caps that let the recon finish on the formats measured (`tests/video_corpus.rs`): 64 KiB head, the
/// tail asked for by the parsers, 10 rounds, the default 8 MiB budget (a TS head scan may use
/// half of it). The detector's defaults (3 rounds) are enough for Matroska and AVI; MP4 with
/// large tables and TS need more rounds.
pub fn recommended_caps() -> Caps {
    Caps {
        head: 64 * 1024,
        rounds: 10,
        ..Caps::default()
    }
}

/// Video recon over the known ranges of `file` (bounded by `caps`), folded into an outcome.
pub fn probe(file: &SparseFile, caps: &Caps) -> VideoOutcome {
    let r = analyze(file, caps);
    match r.info {
        None if r.need.is_empty() => VideoOutcome::Unsupported,
        Some(i) if r.need.is_empty() => VideoOutcome::Info(Box::new(i)),
        _ => VideoOutcome::NeedMore(r.need),
    }
}

/// Video recon over the known ranges of `file`: what is known so far plus the ranges wanted.
/// Never panics (a parser panic is caught, counted in [`crate::panics_caught`] and yields an
/// empty result).
pub fn analyze(file: &SparseFile, caps: &Caps) -> Recon {
    match catch_unwind(AssertUnwindSafe(|| analyze_inner(file, caps))) {
        Ok(r) => r,
        Err(_) => {
            crate::engine::PANICS.fetch_add(1, Ordering::Relaxed);
            Recon::default()
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Container {
    Mkv,
    Mp4,
    Avi,
    Ts,
}

fn sniff(h: &[u8]) -> Option<Container> {
    let at = |o: usize, m: &[u8]| h.get(o..o + m.len()) == Some(m);
    if at(0, &[0x1a, 0x45, 0xdf, 0xa3]) {
        return Some(Container::Mkv);
    }
    if at(0, b"RIFF") && (at(8, b"AVI ") || at(8, b"AVIX")) {
        return Some(Container::Avi);
    }
    let boxes: [&[u8]; 8] = [
        b"ftyp", b"moov", b"mdat", b"free", b"wide", b"skip", b"styp", b"pnot",
    ];
    if boxes.iter().any(|t| at(4, t)) {
        return Some(Container::Mp4);
    }
    if ts::packet_size(h).is_some() {
        return Some(Container::Ts);
    }
    None
}

/// Bytes of a raw audio stream read for its first frames (an E-AC-3 frame set with its
/// dependents, a DTS-HD frame with XLL, a TrueHD access unit with its major sync).
const RAW_AUDIO_HEAD: u64 = 64 * 1024;

/// A raw AC-3 / E-AC-3, DTS (core, DTS-HD, `.dtshd` wrapper) or TrueHD / MLP stream: one
/// audio track from its first frames.
fn raw_audio(src: &Src, head: &[u8]) -> Option<Parsed> {
    let at = |o: usize, m: &[u8]| head.get(o..o + m.len()) == Some(m);
    let (container, kind) = if at(0, &[0x0b, 0x77]) {
        ("ac3", 0)
    } else if at(0, &[0x7f, 0xfe, 0x80, 0x01])
        || at(0, &[0x64, 0x58, 0x20, 0x25])
        || at(0, b"DTSHDHDR")
    {
        ("dts", 1)
    } else if at(4, &[0xf8, 0x72, 0x6f, 0xba]) {
        ("truehd", 2)
    } else if at(4, &[0xf8, 0x72, 0x6f, 0xbb]) {
        ("mlp", 2)
    } else {
        return None;
    };
    let buf = src.get(0, RAW_AUDIO_HEAD.min(src.len()))?;
    let a = match kind {
        0 => audio::ac3(buf),
        1 => audio::dts(buf),
        _ => audio::truehd(buf),
    }?;
    let mut tk = Tk {
        kind: Kind::Audio,
        codec: container.to_string(),
        frame_done: true,
        ..Tk::default()
    };
    tk.apply_audio(a);
    let container = match tk.codec.as_str() {
        "eac3" => "eac3",
        _ => container,
    };
    Some(Parsed {
        container,
        tracks: vec![tk],
        ..Parsed::default()
    })
}

fn analyze_inner(file: &SparseFile, caps: &Caps) -> Recon {
    let src = Src::new(file, caps.max_range);
    let Some(head) = src.get(0, 1024) else {
        let (mut need, _) = src.take();
        crate::engine::cap_ranges(&mut need, caps);
        return Recon {
            info: None,
            need,
            unverified: false,
        };
    };
    let parsed = match sniff(head) {
        Some(Container::Mkv) => mkv::probe(&src),
        Some(Container::Mp4) => mp4::probe(&src),
        Some(Container::Avi) => avi::probe(&src),
        Some(Container::Ts) => ts::probe(&src, caps),
        None => raw_audio(&src, head),
    };
    let size = src.len();
    let unverified = src.read_unverified();
    let (mut need, notes) = src.take();
    crate::engine::cap_ranges(&mut need, caps);
    let Some(p) = parsed else {
        return Recon {
            info: None,
            need,
            unverified,
        };
    };
    Recon {
        info: Some(p.finish(size, notes)),
        need,
        unverified,
    }
}

// ------------------------------------------------------------------ internal track model

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Kind {
    #[default]
    Other,
    Video,
    Audio,
    Sub,
}

/// One track while a container is parsed; split into the public types by [`Parsed::finish`].
#[derive(Clone, Debug, Default)]
pub(crate) struct Tk {
    pub kind: Kind,
    pub id: u64,
    pub codec: String,
    pub profile: Option<String>,
    pub level: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub fps: Option<f64>,
    pub bit_depth: Option<u8>,
    pub chroma: Option<&'static str>,
    pub primaries: Option<u8>,
    pub transfer: Option<u8>,
    pub matrix: Option<u8>,
    pub mastering: bool,
    pub cll: Option<(u16, u16)>,
    pub hdr10plus: bool,
    pub dv: Option<DolbyVision>,
    /// Dolby Vision RPU NAL seen in the first frame.
    pub dv_rpu: bool,
    /// An HEVC SPS was parsed (from the configuration record or in band).
    pub sps: bool,
    pub channels: Option<u8>,
    pub layout: Option<String>,
    pub sample_rate: Option<u32>,
    pub bitrate_kbps: Option<u64>,
    pub features: Vec<AudioFeature>,
    pub atmos: Option<bool>,
    pub dts_x: Option<bool>,
    pub language: Option<String>,
    pub name: Option<String>,
    pub default: bool,
    pub forced: bool,
    /// The first frame (audio or video) was parsed.
    pub frame_done: bool,
    /// Kinds from container flags and bitstream fields, merged with the name in `finish`.
    pub kinds: Vec<TrackKind>,
    /// Matroska FlagOriginal: None absent, Some(false) present and 0 (dub), Some(true) original.
    pub original: Option<bool>,
    /// Video only: closed captions seen in the first access unit (bit 1 = CEA-608, bit 2 =
    /// CEA-708).
    pub captions: u8,
}

impl Tk {
    /// Codecs whose interesting properties are only in the first audio frame.
    pub fn wants_audio_frame(&self) -> bool {
        self.kind == Kind::Audio
            && !self.frame_done
            && (matches!(self.codec.as_str(), "eac3" | "truehd" | "mlp" | "dts")
                // AC-3 cannot carry Atmos: its frame only matters when nothing gave channels.
                || (self.codec == "ac3" && self.channels.is_none()))
    }

    /// Video codecs whose first frame adds HDR / profile details.
    pub fn wants_video_frame(&self) -> bool {
        self.kind == Kind::Video
            && !self.frame_done
            && matches!(self.codec.as_str(), "hevc" | "av1" | "vp9" | "prores")
    }

    /// Codecs that cannot carry Atmos: `atmos` is known to be false without a frame (AC-4 can
    /// carry it, A-JOC, and its `dac4` is not parsed).
    pub fn settle_atmos(&mut self) {
        if self.kind == Kind::Audio
            && self.atmos.is_none()
            && !matches!(
                self.codec.as_str(),
                "eac3" | "truehd" | "ac4" | "" | "unknown"
            )
            && !self.codec.starts_with('?')
        {
            self.atmos = Some(false);
        }
    }

    /// Adds a kind from a container flag or a bitstream field (once).
    pub fn add_kind(&mut self, k: TrackKind) {
        if !self.kinds.contains(&k) {
            self.kinds.push(k);
        }
    }

    /// Merges what a first frame or configuration record says (frame values win).
    pub fn apply_audio(&mut self, a: audio::AudioInfo) {
        if let Some(k) = a.service {
            self.add_kind(k);
        }
        if let Some(c) = a.codec {
            // An AC-3 frame inside a TrueHD track is the interleaved compatibility stream; inside
            // an E-AC-3 track it is the core of a Blu-ray style stream whose dependent frames were
            // not in the buffer: neither says anything about the track's own codec or Atmos.
            if (self.codec == "truehd" || self.codec == "eac3") && c == "ac3" {
                if self.codec == "eac3" {
                    self.sample_rate = self.sample_rate.or(a.sample_rate);
                    self.channels = self.channels.or(a.channels);
                    self.layout = self.layout.take().or(a.layout);
                    return;
                }
            } else {
                self.codec = c.to_string();
            }
        }
        if a.profile.is_some() {
            self.profile = a.profile;
        }
        self.channels = a.channels.or(self.channels);
        self.layout = a.layout.or(self.layout.take());
        self.sample_rate = a.sample_rate.or(self.sample_rate);
        self.bit_depth = a.bit_depth.or(self.bit_depth);
        self.bitrate_kbps = self.bitrate_kbps.or(a.bitrate_kbps);
        if a.atmos.is_some() {
            self.atmos = a.atmos;
        }
        if a.dts_x.is_some() {
            self.dts_x = a.dts_x;
        }
        for f in a.features {
            if !self.features.contains(&f) {
                self.features.push(f);
            }
        }
    }
}

/// What a container parser produced.
#[derive(Clone, Debug, Default)]
pub(crate) struct Parsed {
    pub container: &'static str,
    pub brand: Option<String>,
    /// Seconds.
    pub duration: Option<f64>,
    pub fragmented: bool,
    pub tracks: Vec<Tk>,
}

impl Parsed {
    fn finish(mut self, size: u64, notes: Vec<String>) -> VideoInfo {
        let duration_ms = self
            .duration
            .filter(|d| d.is_finite() && *d > 0.0 && *d < 1e9)
            .map(|d| (d * 1000.0).round() as u64);
        let mut info = VideoInfo {
            container: self.container.to_string(),
            brand: self.brand.take(),
            duration_ms,
            bitrate_kbps: duration_ms
                .filter(|d| *d > 0)
                .map(|d| size.saturating_mul(8) / d),
            fragmented: self.fragmented,
            notes,
            ..VideoInfo::default()
        };
        let mut captions: Vec<SubtitleTrack> = Vec::new();
        for mut t in self.tracks.into_iter().take(256) {
            t.settle_atmos();
            if t.kind == Kind::Video && t.captions != 0 {
                for (bit, codec) in [(1u8, "eia_608"), (2, "eia_708")] {
                    if t.captions & bit != 0 {
                        captions.push(SubtitleTrack {
                            id: t.id,
                            codec: codec.into(),
                            kinds: vec![TrackKind::Cc],
                            ..SubtitleTrack::default()
                        });
                    }
                }
            }
            let (kinds, name_kinds) = match t.kind {
                Kind::Audio => kind::resolve(&t, NameTrack::Audio),
                Kind::Sub => kind::resolve(&t, NameTrack::Subtitle),
                _ => (Vec::new(), Vec::new()),
            };
            if t.kind == Kind::Audio && is_lossy(&t.codec, &t.features) {
                t.bit_depth = None; // a container "bit depth" means nothing for lossy codecs
            }
            match t.kind {
                Kind::Video => info.video.push(video_track(t)),
                Kind::Audio => info.audio.push(AudioTrack {
                    id: t.id,
                    codec: t.codec,
                    profile: t.profile,
                    channels: t.channels,
                    // No frame gave the speaker map: ffmpeg's default layout for the common
                    // channel counts (the others are ambiguous).
                    layout: t.layout.or_else(|| {
                        let name = match t.channels? {
                            1 => "mono",
                            2 => "stereo",
                            6 => "5.1",
                            8 => "7.1",
                            _ => return None,
                        };
                        Some(name.to_string())
                    }),
                    sample_rate: t.sample_rate,
                    bit_depth: t.bit_depth,
                    bitrate_kbps: t.bitrate_kbps,
                    features: t.features,
                    atmos: t.atmos,
                    dts_x: t.dts_x,
                    language: t.language,
                    name: t.name,
                    kinds,
                    name_kinds,
                    default: t.default,
                }),
                Kind::Sub => info.subtitles.push(SubtitleTrack {
                    id: t.id,
                    codec: t.codec,
                    language: t.language,
                    name: t.name,
                    kinds,
                    name_kinds,
                    default: t.default,
                    forced: t.forced,
                }),
                Kind::Other => {}
            }
        }
        // Captions signalled in the first video access unit (D123), unless the container
        // already lists a track of that codec: an ATSC caption_service_descriptor on the same
        // PID (which has languages), or an MP4 `c608` / `c708` track with its own id (review
        // of D123: the same service would show twice).
        for c in captions {
            if !info.subtitles.iter().any(|s| s.codec == c.codec) {
                info.subtitles.push(c);
            }
        }
        info
    }
}

fn video_track(mut t: Tk) -> VideoTrack {
    if t.codec == "vp8" {
        // VP8 has a single format: 8-bit 4:2:0.
        t.bit_depth = t.bit_depth.or(Some(8));
        t.chroma = t.chroma.or(Some("4:2:0"));
    }
    let mut hdr = Vec::new();
    match t.transfer {
        Some(16) => {
            hdr.push(HdrFormat::Hdr10);
            if t.hdr10plus {
                hdr.push(HdrFormat::Hdr10Plus);
            }
        }
        Some(18) => hdr.push(HdrFormat::Hlg),
        _ => {}
    }
    if t.dv.is_some() || t.dv_rpu {
        hdr.push(HdrFormat::DolbyVision);
    }
    VideoTrack {
        id: t.id,
        codec: t.codec,
        profile: t.profile,
        level: t.level,
        width: t.width,
        height: t.height,
        fps: t.fps.filter(|f| f.is_finite() && *f > 0.0 && *f < 10_000.0),
        bit_depth: t.bit_depth,
        chroma: t.chroma.map(str::to_string),
        primaries: t.primaries,
        transfer: t.transfer,
        matrix: t.matrix,
        hdr,
        mastering_display: t.mastering,
        max_cll: t.cll.map(|c| c.0),
        max_fall: t.cll.map(|c| c.1),
        dolby_vision: t.dv,
        bitrate_kbps: t.bitrate_kbps,
        language: t.language,
        name: t.name,
        default: t.default,
    }
}

fn is_lossy(codec: &str, features: &[AudioFeature]) -> bool {
    match codec {
        "aac" | "ac3" | "eac3" | "opus" | "mp3" | "mp2" | "vorbis" | "aac_latm" | "wmav2"
        | "dtsuhd" => true,
        "dts" => !features.contains(&AudioFeature::Lossless),
        _ => false,
    }
}

/// ISO 639-2 code or `None` for "und" / empty / non-letters.
pub(crate) fn lang(s: &str) -> Option<String> {
    let s = s.trim_matches(char::from(0)).trim();
    if s.is_empty() || s.eq_ignore_ascii_case("und") || s.len() > 35 {
        return None;
    }
    if !s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return None;
    }
    Some(s.to_string())
}

/// Lossy UTF-8 without NULs, trimmed, at most 256 characters.
pub(crate) fn text(b: &[u8]) -> String {
    let e = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    String::from_utf8_lossy(b.get(..e).unwrap_or(b))
        .trim()
        .chars()
        .take(256)
        .collect()
}

#[cfg(test)]
mod finish_tests {
    use super::*;

    fn video(id: u64, captions: u8) -> Tk {
        Tk {
            kind: Kind::Video,
            id,
            codec: "hevc".into(),
            captions,
            ..Tk::default()
        }
    }

    /// A caption service the container lists (an MP4 `c608` track, id 2) is not repeated by
    /// the GA94 captions of the video (id 1); a codec the container lacks is added.
    #[test]
    fn captions_in_the_video_do_not_repeat_a_caption_track() {
        let parsed = Parsed {
            container: "mp4",
            brand: None,
            duration: None,
            fragmented: false,
            tracks: vec![
                video(1, 1 | 2),
                Tk {
                    kind: Kind::Sub,
                    id: 2,
                    codec: "eia_608".into(),
                    ..Tk::default()
                },
            ],
        };
        let info = parsed.finish(1000, Vec::new());
        let subs: Vec<(u64, &str)> = info
            .subtitles
            .iter()
            .map(|s| (s.id, s.codec.as_str()))
            .collect();
        assert_eq!(subs, [(2, "eia_608"), (1, "eia_708")]);
    }
}

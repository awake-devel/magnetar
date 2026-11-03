//! Header-probe results (D117, docs/PROBES.md) on both sides of the worker API:
//!
//! - **worker**: [`report_of`] turns a [`ProbeResult`] (sniff detections) into the wire
//!   [`ProbeReport`]: detections of confidence 80+ only, strings cleaned and capped, language
//!   codes normalised to ISO 639-2/B lowercase, track lists capped. No path or name travels:
//!   files are display indexes into the info dict the server stores;
//! - **server**: [`validate`] checks an uploaded report against the server's own file list
//!   and sniff's label registry (nothing a worker says is trusted beyond what is checkable,
//!   D113), [`store`] writes it to `probe_attrs` / `probe_files` (one savepoint per row, D110)
//!   only for a stored DHT row whose hash is not blocked (adult rows included since D119), and
//!   [`probe_for`] reads it back for the detail API with paths from the server's file list.
//!
//! The server never runs a probe: [`magnetar_core::probe::Prober`] is built only by the
//! worker's crawler (`worker::crawl`), and only with `MAGNETAR_PROBE=true`.

use std::collections::HashSet;

use magnetar_core::model::{
    PROBE_AUDIO_KINDS, PROBE_MAX_TRACK_NAME, PROBE_SUB_KINDS, ProbeAudioTrack, ProbeFile,
    ProbeFileInfo, ProbeGame, ProbeInfo, ProbeSubTrack, ProbeVideo, ProbeVideoTrack, TorrentFile,
};
use magnetar_core::probe::{PROBE_VERSION, ProbeResult, ProbeStatus};
use sqlx::PgPool;
use sqlx::types::Json;

use crate::fleet::proto::ProbeReport;

/// Most files in one report (the probe plans at most 8 targets; disc metadata adds a few).
pub const MAX_FILES: usize = 16;
/// Detections below this are not stored (sniff's "store as fact" line).
pub const MIN_CONFIDENCE: u8 = 80;
pub const MAX_VIDEO_TRACKS: usize = 8;
pub const MAX_AUDIO_TRACKS: usize = 32;
pub const MAX_SUB_TRACKS: usize = 64;
/// Strings in a report, bytes (a game title gets [`MAX_TITLE`]).
pub const MAX_TEXT: usize = 128;
pub const MAX_TITLE: usize = 256;
/// Serialised report, bytes.
pub const MAX_JSON: usize = 64 * 1024;

/// Every flag a stored file may carry.
pub const FLAGS: [&str; 8] = [
    "extension_mismatch",
    "suspicious_executable",
    "incomplete",
    "agreed",
    "disagree",
    "surprise_verified",
    "surprise_changed",
    "surprise_unverified",
];
const HDR: [&str; 4] = ["hdr10", "hdr10_plus", "hlg", "dolby_vision"];
/// Audio kinds whose track does not count as "audio in that language" (D124): the
/// `a:{lang}` tag is not written for them (the `audio_langs` column keeps the language).
const AUDIO_KINDS_NOT_LANGUAGE: [&str; 3] = ["commentary", "music", "karaoke"];
/// Subtitle kinds whose track does not count as "subtitles in that language" (D124): no
/// `s:{lang}` tag for them (the `sub_langs` column keeps the language).
const SUB_KINDS_NOT_LANGUAGE: [&str; 4] = ["forced", "signs", "commentary", "description"];
const GAME_KINDS: [&str; 4] = ["package", "image", "compressed_image", "metadata"];
/// sniff's platforms (a platform added to sniff later is refused by a server that does not
/// list it: the probe only, never the dict).
const PLATFORMS: [sniff::Platform; 36] = {
    use sniff::Platform::*;
    [
        Ps5,
        Ps4,
        Ps3,
        Psp,
        PsVita,
        PspOrVita,
        Ps1OrPs2,
        Ps1,
        Ps2,
        Switch,
        Xbox,
        Xbox360,
        XboxOne,
        Wii,
        WiiU,
        GameCube,
        WiiOrGameCube,
        N3ds,
        Nds,
        Gba,
        Gb,
        Gbc,
        Nes,
        Fds,
        Snes,
        N64,
        MasterSystem,
        GameGear,
        Genesis,
        Sega32x,
        SegaCd,
        Saturn,
        Dreamcast,
        Lynx,
        Atari7800,
        NeoGeoPocket,
    ]
};

/// A serde unit-enum value as its snake_case text.
fn enum_text<T: serde::Serialize>(v: &T) -> Option<String> {
    serde_json::to_value(v)
        .ok()
        .and_then(|j| j.as_str().map(str::to_string))
}

fn platform_of(text: &str) -> Option<sniff::Platform> {
    PLATFORMS
        .iter()
        .copied()
        .find(|p| enum_text(p).as_deref() == Some(text))
}

/// The warning for `MAGNETAR_PROBE=true` on any command but `worker`: probes are a crawl
/// worker setting and nothing else builds a prober (D117). None when there is nothing to say.
pub fn not_here(
    cfg: &crate::config::Config,
    command: &crate::config::Command,
) -> Option<&'static str> {
    (cfg.probe && !matches!(command, crate::config::Command::Worker { .. })).then_some(
        "MAGNETAR_PROBE is a crawl-worker setting (docs/PROBES.md): this command never runs \
         header probes; ignored",
    )
}

// ---------------------------------------------------------------- worker side

/// Printable text: control characters dropped, cut to `max` bytes on a character boundary.
fn clean(s: &str, max: usize) -> Option<String> {
    let mut out = String::new();
    for c in s.chars().filter(|c| !c.is_control()) {
        if out.len() + c.len_utf8() > max {
            break;
        }
        out.push(c);
    }
    let out = out.trim().to_string();
    (!out.is_empty()).then_some(out)
}

/// A codec / container token: `[a-z0-9_.+-]`, 1–32 bytes.
fn token(s: &str) -> Option<String> {
    let t = s.trim().to_ascii_lowercase();
    ok_token(&t).then_some(t)
}

fn ok_token(s: &str) -> bool {
    (1..=32).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_.+-".contains(&b))
}

/// ISO 639-1 → 639-2/B for the languages releases carry most.
const ISO1: [(&str, &str); 58] = [
    ("en", "eng"),
    ("fr", "fre"),
    ("de", "ger"),
    ("es", "spa"),
    ("it", "ita"),
    ("pt", "por"),
    ("ru", "rus"),
    ("ja", "jpn"),
    ("zh", "chi"),
    ("ko", "kor"),
    ("ar", "ara"),
    ("nl", "dut"),
    ("sv", "swe"),
    ("no", "nor"),
    ("nb", "nob"),
    ("nn", "nno"),
    ("da", "dan"),
    ("fi", "fin"),
    ("pl", "pol"),
    ("cs", "cze"),
    ("sk", "slo"),
    ("hu", "hun"),
    ("ro", "rum"),
    ("bg", "bul"),
    ("el", "gre"),
    ("tr", "tur"),
    ("he", "heb"),
    ("hi", "hin"),
    ("th", "tha"),
    ("vi", "vie"),
    ("id", "ind"),
    ("ms", "may"),
    ("uk", "ukr"),
    ("hr", "hrv"),
    ("sr", "srp"),
    ("sl", "slv"),
    ("et", "est"),
    ("lv", "lav"),
    ("lt", "lit"),
    ("fa", "per"),
    ("is", "ice"),
    ("ca", "cat"),
    ("eu", "baq"),
    ("gl", "glg"),
    ("ta", "tam"),
    ("te", "tel"),
    ("bn", "ben"),
    ("ur", "urd"),
    ("sq", "alb"),
    ("mk", "mac"),
    ("hy", "arm"),
    ("ka", "geo"),
    ("cy", "wel"),
    ("ga", "gle"),
    ("la", "lat"),
    ("tl", "tgl"),
    ("ml", "mal"),
    ("kn", "kan"),
];

/// ISO 639-2/T → /B where they differ (Matroska writes /B).
const ISO2T: [(&str, &str); 19] = [
    ("fra", "fre"),
    ("deu", "ger"),
    ("zho", "chi"),
    ("ces", "cze"),
    ("slk", "slo"),
    ("ron", "rum"),
    ("ell", "gre"),
    ("nld", "dut"),
    ("fas", "per"),
    ("isl", "ice"),
    ("eus", "baq"),
    ("sqi", "alb"),
    ("mkd", "mac"),
    ("hye", "arm"),
    ("kat", "geo"),
    ("cym", "wel"),
    ("msa", "may"),
    ("mya", "bur"),
    ("bod", "tib"),
];

/// A track language as ISO 639-2/B lowercase (`en`, `en-US`, `ENG`, `fra` → `eng`, `eng`,
/// `eng`, `fre`; check0 #6: MP4 and TS write BCP 47 or /T, Matroska /B). Unknown, `und` and
/// the special codes give None.
pub fn normalize_lang(s: &str) -> Option<String> {
    let primary = s.trim().split(['-', '_']).next()?.to_ascii_lowercase();
    if !primary.bytes().all(|b| b.is_ascii_lowercase()) {
        return None;
    }
    match primary.len() {
        2 => ISO1
            .iter()
            .find(|(a, _)| *a == primary)
            .map(|(_, b)| b.to_string()),
        3 if ["und", "zxx", "mis", "mul", "qaa"].contains(&primary.as_str()) => None,
        3 => Some(
            ISO2T
                .iter()
                .find(|(t, _)| *t == primary)
                .map_or(primary.clone(), |(_, b)| b.to_string()),
        ),
        _ => None,
    }
}

fn game_of(g: &sniff::GameInfo) -> Option<ProbeGame> {
    let platform = enum_text(&g.platform)?;
    let id = |s: &Option<String>| s.as_deref().and_then(|s| clean(s, MAX_TEXT));
    Some(ProbeGame {
        platform,
        kind: enum_text(&g.kind),
        format: clean(g.format, MAX_TEXT),
        title_id: id(&g.title_id).filter(|t| ok_id(t)),
        content_id: id(&g.content_id).filter(|t| ok_id(t)),
        db_id: id(&g.db_id).filter(|d| sniff::db_id_valid(g.platform, d)),
        title: g.title.as_deref().and_then(|s| clean(s, MAX_TITLE)),
        version: id(&g.version),
        region: g.region.and_then(|r| clean(r, MAX_TEXT)),
    })
}

/// A title / content id: `[A-Za-z0-9_.-]`, 1–64 bytes.
fn ok_id(s: &str) -> bool {
    (1..=64).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
}

fn video_of(v: &sniff::VideoInfo) -> Option<ProbeVideo> {
    /// Track kinds the server stores (D123): sniff's kinds in `allowed`, sorted, unique.
    fn kinds_of(k: &[sniff::video::TrackKind], allowed: &[&str]) -> Vec<String> {
        let mut v: Vec<String> = k
            .iter()
            .map(|k| k.as_str())
            .filter(|k| allowed.contains(k))
            .map(str::to_string)
            .collect();
        v.sort();
        v.dedup();
        v
    }
    /// The track name, cleaned and cut to `PROBE_MAX_TRACK_NAME` bytes (D123).
    fn track_name(n: &Option<String>) -> Option<String> {
        n.as_deref()
            .and_then(|n| clean(n, magnetar_core::model::PROBE_MAX_TRACK_NAME))
    }
    let container = token(&v.container)?;
    let video = v
        .video
        .iter()
        .filter_map(|t| {
            // The profile only with the flag it describes (D121).
            let dv = t
                .dolby_vision
                .filter(|_| t.hdr.contains(&sniff::video::HdrFormat::DolbyVision));
            Some(ProbeVideoTrack {
                codec: token(&t.codec)?,
                profile: t.profile.as_deref().and_then(|p| clean(p, MAX_TEXT)),
                width: t.width.filter(|w| (1..=MAX_DIMENSION).contains(w)),
                height: t.height.filter(|h| (1..=MAX_DIMENSION).contains(h)),
                fps_milli: t
                    .fps
                    .filter(|f| f.is_finite() && *f > 0.0 && *f <= 1000.0)
                    .map(|f| ((f * 1000.0).round() as u32).clamp(1, MAX_FPS_MILLI)),
                bit_depth: t.bit_depth.filter(|b| (1..=MAX_BIT_DEPTH).contains(b)),
                hdr: t.hdr.iter().filter_map(enum_text).collect(),
                dv_profile: dv.map(|d| d.profile).filter(|p| *p <= MAX_DV_PROFILE),
                dv_compat: dv
                    .and_then(|d| d.compatibility)
                    .filter(|c| *c <= MAX_DV_COMPAT),
            })
        })
        .take(MAX_VIDEO_TRACKS)
        .collect();
    let audio = v
        .audio
        .iter()
        .filter_map(|a| {
            let feature = |f: sniff::video::AudioFeature| a.features.contains(&f);
            Some(ProbeAudioTrack {
                codec: token(&a.codec)?,
                channels: a.channels.filter(|c| (1..=MAX_CHANNELS).contains(c)),
                lang: a.language.as_deref().and_then(normalize_lang),
                atmos: a.atmos == Some(true) || feature(sniff::video::AudioFeature::Atmos),
                dts_x: a.dts_x == Some(true) || feature(sniff::video::AudioFeature::DtsX),
                kinds: kinds_of(&a.kinds, &PROBE_AUDIO_KINDS),
                name: track_name(&a.name),
                // Detail-only fields (D128): never in a report.
                pos: None,
                fix: None,
            })
        })
        .take(MAX_AUDIO_TRACKS)
        .collect();
    let subs = v
        .subtitles
        .iter()
        .filter_map(|s| {
            let mut kinds = kinds_of(&s.kinds, &PROBE_SUB_KINDS);
            let mut lang = s.language.as_deref().and_then(normalize_lang);
            // `enm` (English with honorifics, as fansub releases tag it) is English plus the
            // honorifics kind (D123).
            let primary = s
                .language
                .as_deref()
                .and_then(|l| l.split(['-', '_']).next());
            if primary.is_some_and(|p| p.eq_ignore_ascii_case("enm")) {
                lang = Some("eng".into());
                if !kinds.iter().any(|k| k == "honorifics") {
                    kinds.push("honorifics".into());
                    kinds.sort();
                }
            }
            Some(ProbeSubTrack {
                codec: token(&s.codec)?,
                lang,
                kinds,
                name: track_name(&s.name),
                pos: None,
                fix: None,
            })
        })
        .take(MAX_SUB_TRACKS)
        .collect();
    Some(ProbeVideo {
        container,
        duration_ms: v.duration_ms.filter(|d| (1..=MAX_DURATION_MS).contains(d)),
        bitrate_kbps: v
            .bitrate_kbps
            .filter(|b| (1..=MAX_BITRATE_KBPS).contains(b)),
        video,
        audio,
        subs,
    })
}

/// What a worker uploads for a finished probe: None when it found nothing storable (status
/// other than `ok` / `partial`, or no detection of confidence 80+).
pub fn report_of(r: &ProbeResult) -> Option<ProbeReport> {
    if !matches!(r.status, ProbeStatus::Ok | ProbeStatus::Partial) {
        return None;
    }
    let mut seen = HashSet::new();
    let files: Vec<ProbeFile> = r
        .files
        .iter()
        .filter(|f| f.detection.confidence >= MIN_CONFIDENCE && seen.insert(f.index))
        .filter_map(|f| {
            let d = &f.detection;
            let mut flags: Vec<String> = Vec::new();
            for (on, name) in [
                (d.flags.extension_mismatch, "extension_mismatch"),
                (d.flags.suspicious_executable, "suspicious_executable"),
                (d.flags.incomplete, "incomplete"),
                (f.agreed >= 2, "agreed"),
            ] {
                if on {
                    flags.push(name.into());
                }
            }
            for n in &f.notes {
                if FLAGS.contains(n) && !flags.iter().any(|x| x == n) {
                    flags.push((*n).into());
                }
            }
            let label = sniff::label::info(&d.label)?.id;
            // Only the blocks the server takes on this label (else it refuses the report).
            let kind = kind_of(label);
            Some(ProbeFile {
                i: f.index,
                size: f.size,
                label: label.to_string(),
                confidence: d.confidence.min(100),
                verified: f.verified,
                flags,
                game: d
                    .game
                    .as_ref()
                    .filter(|_| GAME_KIND_LABELS.contains(&kind))
                    .and_then(game_of),
                video: d
                    .video
                    .as_ref()
                    .filter(|_| VIDEO_KINDS.contains(&kind))
                    .and_then(video_of),
            })
        })
        .take(MAX_FILES)
        .collect();
    if files.is_empty() {
        return None;
    }
    let mut report = ProbeReport {
        v: PROBE_VERSION,
        status: r.status.code().into(),
        bytes_in: r.bytes_in,
        uploaded: r.uploaded,
        peers: r.peers_used,
        files,
    };
    // Track names are the first thing to give up when the report would be refused for its
    // size (D123); the kinds stay. Still too large, the server refuses it as before.
    if serde_json::to_vec(&report).map_or(usize::MAX, |b| b.len()) > MAX_JSON {
        for v in report.files.iter_mut().filter_map(|f| f.video.as_mut()) {
            v.audio.iter_mut().for_each(|a| a.name = None);
            v.subs.iter_mut().for_each(|s| s.name = None);
        }
    }
    Some(report)
}

#[cfg(test)]
mod worker_kind_tests {
    use magnetar_core::model::{PROBE_MAX_TRACK_NAME, ProbeSubTrack};
    use magnetar_core::probe::{FileResult, PROBE_VERSION, ProbeResult, ProbeStatus};
    use sniff::video::{AudioTrack, SubtitleTrack, TrackKind, VideoInfo};

    use super::{MAX_JSON, MAX_SUB_TRACKS, report_of, video_of};

    fn info(audio: Vec<AudioTrack>, subtitles: Vec<SubtitleTrack>) -> VideoInfo {
        VideoInfo {
            container: "matroska".into(),
            audio,
            subtitles,
            ..VideoInfo::default()
        }
    }

    #[test]
    fn video_of_keeps_kinds_and_names() {
        let v = info(
            vec![AudioTrack {
                codec: "ac3".into(),
                channels: Some(2),
                language: Some("en".into()),
                name: Some("Director's\u{7} Commentary".into()),
                // Unsorted, duplicated, and a subtitle kind that is no audio kind.
                kinds: vec![TrackKind::Original, TrackKind::Commentary, TrackKind::Sdh],
                ..AudioTrack::default()
            }],
            vec![
                SubtitleTrack {
                    codec: "hdmv_pgs_subtitle".into(),
                    language: Some("eng".into()),
                    name: Some(format!("English (SDH) {}", "x".repeat(80))),
                    kinds: vec![TrackKind::Sdh, TrackKind::Dub, TrackKind::Sdh],
                    ..SubtitleTrack::default()
                },
                SubtitleTrack {
                    codec: "subrip".into(),
                    language: Some("fre".into()),
                    ..SubtitleTrack::default()
                },
            ],
        );
        let p = video_of(&v).unwrap();
        assert_eq!(p.audio[0].kinds, ["commentary", "original"]);
        assert_eq!(p.audio[0].name.as_deref(), Some("Director's Commentary"));
        assert_eq!(p.audio[0].lang.as_deref(), Some("eng"));
        assert_eq!(p.subs[0].kinds, ["sdh"]);
        let n = p.subs[0].name.as_deref().unwrap();
        assert!(n.starts_with("English (SDH) x") && n.len() == PROBE_MAX_TRACK_NAME);
        assert_eq!(
            p.subs[1],
            ProbeSubTrack {
                codec: "subrip".into(),
                lang: Some("fre".into()),
                ..ProbeSubTrack::default()
            }
        );
        // The wire JSON leaves out empty kinds and absent names (§3).
        let j = serde_json::to_value(&p).unwrap();
        assert_eq!(
            j["subs"][1],
            serde_json::json!({"codec": "subrip", "lang": "fre"})
        );
        assert_eq!(
            j["audio"][0]["kinds"],
            serde_json::json!(["commentary", "original"])
        );
    }

    #[test]
    fn enm_is_english_with_honorifics() {
        let v = info(
            vec![],
            vec![
                SubtitleTrack {
                    codec: "ass".into(),
                    language: Some("enm".into()),
                    ..SubtitleTrack::default()
                },
                SubtitleTrack {
                    codec: "ass".into(),
                    language: Some("enm".into()),
                    kinds: vec![TrackKind::Honorifics, TrackKind::Signs],
                    ..SubtitleTrack::default()
                },
            ],
        );
        let p = video_of(&v).unwrap();
        assert_eq!(p.subs[0].lang.as_deref(), Some("eng"));
        assert_eq!(p.subs[0].kinds, ["honorifics"]);
        assert_eq!(p.subs[1].lang.as_deref(), Some("eng"));
        assert_eq!(p.subs[1].kinds, ["honorifics", "signs"]);
    }

    #[test]
    fn names_dropped_when_the_report_is_too_large() {
        let sub = SubtitleTrack {
            codec: "hdmv_pgs_subtitle".into(),
            language: Some("eng".into()),
            name: Some("n".repeat(PROBE_MAX_TRACK_NAME)),
            kinds: vec![TrackKind::Sdh],
            ..SubtitleTrack::default()
        };
        let v = info(vec![], vec![sub; MAX_SUB_TRACKS]);
        let file = |index: u32| FileResult {
            index,
            size: 1_000_000,
            detection: sniff::Detection {
                label: "video.mkv".into(),
                mime: String::new(),
                description: String::new(),
                confidence: 95,
                fields: Vec::new(),
                game: None,
                video: Some(v.clone()),
                flags: sniff::Flags::default(),
                extensions: Vec::new(),
                candidates: Vec::new(),
            },
            verified: true,
            agreed: 1,
            notes: Vec::new(),
        };
        let mut r = ProbeResult {
            status: ProbeStatus::Ok,
            files: (0..16).map(file).collect(),
            bytes_in: 1,
            uploaded: 0,
            peers_used: 1,
            unchoked: true,
            incomplete: false,
            surprises: 0,
        };
        let rep = report_of(&r).unwrap();
        assert_eq!(rep.files.len(), 16);
        let len = serde_json::to_vec(&rep).unwrap().len();
        assert!(len <= MAX_JSON, "{len}");
        let subs = &rep.files[0].video.as_ref().unwrap().subs;
        assert_eq!(subs.len(), MAX_SUB_TRACKS);
        assert!(subs.iter().all(|s| s.name.is_none() && s.kinds == ["sdh"]));
        // A small report keeps its names.
        r.files.truncate(1);
        let rep = report_of(&r).unwrap();
        let subs = &rep.files[0].video.as_ref().unwrap().subs;
        assert!(subs.iter().all(|s| s.name.is_some()));
    }

    #[test]
    fn probe_version_is_2() {
        assert_eq!(PROBE_VERSION, 2);
    }
}

// ---------------------------------------------------------------- server side

fn ok_text(s: &str, max: usize) -> bool {
    !s.is_empty() && s.len() <= max && !s.chars().any(char::is_control)
}

fn ok_opt(s: &Option<String>, max: usize) -> bool {
    s.as_deref().is_none_or(|s| ok_text(s, max))
}

/// Track kinds (D124): values of `allowed` only, no duplicates (order is not checked).
fn ok_kinds(kinds: &[String], allowed: &[&str]) -> bool {
    kinds.len() <= allowed.len()
        && kinds.iter().all(|k| allowed.contains(&k.as_str()))
        && kinds
            .iter()
            .enumerate()
            .all(|(i, k)| !kinds[..i].contains(k))
}

fn ok_lang(s: &Option<String>) -> bool {
    s.as_deref()
        .is_none_or(|l| l.len() == 3 && l.bytes().all(|b| b.is_ascii_lowercase()))
}

fn check_game(g: &ProbeGame) -> Result<(), &'static str> {
    let platform = platform_of(&g.platform).ok_or("platform")?;
    if !g.kind.as_deref().is_none_or(|k| GAME_KINDS.contains(&k)) {
        return Err("game_kind");
    }
    let ids = [&g.title_id, &g.content_id];
    if !ids.iter().all(|i| i.as_deref().is_none_or(ok_id)) {
        return Err("game_id");
    }
    if !g
        .db_id
        .as_deref()
        .is_none_or(|d| sniff::db_id_valid(platform, d))
    {
        return Err("db_id");
    }
    let texts = [&g.format, &g.version, &g.region];
    if !texts.iter().all(|t| ok_opt(t, MAX_TEXT)) || !ok_opt(&g.title, MAX_TITLE) {
        return Err("text");
    }
    Ok(())
}

/// Bounds of the numbers in a report (the worker keeps only values inside them).
pub const MAX_DIMENSION: u32 = 65_535;
/// Frames per second × 1000 (1000 fps).
pub const MAX_FPS_MILLI: u32 = 1_000_000;
pub const MAX_BIT_DEPTH: u8 = 32;
/// Dolby Vision profiles run 0–10 and 20 (MV-HEVC); compatibility ids are 4 bits (D121).
pub const MAX_DV_PROFILE: u8 = 20;
pub const MAX_DV_COMPAT: u8 = 15;
pub const MAX_CHANNELS: u8 = 64;
/// 30 days.
pub const MAX_DURATION_MS: u64 = 30 * 24 * 3600 * 1000;
/// 10 Gbit/s.
pub const MAX_BITRATE_KBPS: u64 = 10_000_000;

fn in_range<T: PartialOrd>(v: Option<T>, lo: T, hi: T) -> bool {
    v.is_none_or(|v| v >= lo && v <= hi)
}

/// Label kinds (the part before the first `.`) whose detections may carry each block: video
/// and audio recon only on media labels, console ids on game, game data, package, disc,
/// archive, executable and misc labels (never on text, documents, pictures, fonts...).
const VIDEO_KINDS: [&str; 2] = ["video", "audio"];
const GAME_KIND_LABELS: [&str; 7] = [
    "game", "gamedata", "package", "disc", "archive", "exe", "misc",
];

fn kind_of(label: &str) -> &str {
    label.split('.').next().unwrap_or("")
}

fn check_video(v: &ProbeVideo) -> Result<(), &'static str> {
    if !ok_token(&v.container) {
        return Err("container");
    }
    if !in_range(v.duration_ms, 1, MAX_DURATION_MS)
        || !in_range(v.bitrate_kbps, 1, MAX_BITRATE_KBPS)
    {
        return Err("video_numbers");
    }
    if v.video.len() > MAX_VIDEO_TRACKS
        || v.audio.len() > MAX_AUDIO_TRACKS
        || v.subs.len() > MAX_SUB_TRACKS
    {
        return Err("tracks");
    }
    for t in &v.video {
        if !ok_token(&t.codec)
            || !ok_opt(&t.profile, MAX_TEXT)
            || t.hdr.len() > HDR.len()
            || !t.hdr.iter().all(|h| HDR.contains(&h.as_str()))
            || !in_range(t.width, 1, MAX_DIMENSION)
            || !in_range(t.height, 1, MAX_DIMENSION)
            || !in_range(t.fps_milli, 1, MAX_FPS_MILLI)
            || !in_range(t.bit_depth, 1, MAX_BIT_DEPTH)
            || !in_range(t.dv_profile, 0, MAX_DV_PROFILE)
            || !in_range(t.dv_compat, 0, MAX_DV_COMPAT)
            || ((t.dv_profile.is_some() || t.dv_compat.is_some())
                && !t.hdr.iter().any(|h| h == "dolby_vision"))
        {
            return Err("video_track");
        }
    }
    for a in &v.audio {
        if !ok_token(&a.codec)
            || !ok_lang(&a.lang)
            || !in_range(a.channels, 1, MAX_CHANNELS)
            || !ok_kinds(&a.kinds, &PROBE_AUDIO_KINDS)
            || !ok_opt(&a.name, PROBE_MAX_TRACK_NAME)
            // A worker never sends the detail's correction fields (D129).
            || a.pos.is_some()
            || a.fix.is_some()
        {
            return Err("audio_track");
        }
    }
    for s in &v.subs {
        if !ok_token(&s.codec)
            || !ok_lang(&s.lang)
            || !ok_kinds(&s.kinds, &PROBE_SUB_KINDS)
            || !ok_opt(&s.name, PROBE_MAX_TRACK_NAME)
            || s.pos.is_some()
            || s.fix.is_some()
        {
            return Err("sub_track");
        }
    }
    Ok(())
}

/// Check an uploaded report against the server's own file list of that torrent (`files`,
/// the first `FILES_MAX` display files of `dht_torrents.files`). The error is a short code
/// for logs and counts (never a value from the report).
pub fn validate(r: &ProbeReport, files: &[TorrentFile]) -> Result<(), &'static str> {
    if r.v == 0 || r.v > PROBE_VERSION {
        return Err("version");
    }
    if !matches!(r.status.as_str(), "ok" | "partial") {
        return Err("status");
    }
    if r.files.is_empty() || r.files.len() > MAX_FILES {
        return Err("file_count");
    }
    if serde_json::to_vec(r).map_or(usize::MAX, |b| b.len()) > MAX_JSON {
        return Err("too_large");
    }
    let mut seen = HashSet::new();
    for f in &r.files {
        if !seen.insert(f.i) {
            return Err("duplicate");
        }
        let stored = files.get(f.i as usize).ok_or("index")?;
        if stored.size != Some(f.size) {
            return Err("size");
        }
        if sniff::label::info(&f.label).is_none() {
            return Err("label");
        }
        if !(MIN_CONFIDENCE..=100).contains(&f.confidence) {
            return Err("confidence");
        }
        if f.flags.len() > FLAGS.len() || !f.flags.iter().all(|x| FLAGS.contains(&x.as_str())) {
            return Err("flags");
        }
        if let Some(g) = &f.game {
            if !GAME_KIND_LABELS.contains(&kind_of(&f.label)) {
                return Err("game_label");
            }
            check_game(g)?;
        }
        if let Some(v) = &f.video {
            if !VIDEO_KINDS.contains(&kind_of(&f.label)) {
                return Err("video_label");
            }
            check_video(v)?;
        }
    }
    Ok(())
}

/// The summary columns of `probe_attrs`, from the main payload (the largest probed file).
#[derive(Debug, Default, PartialEq)]
pub struct Summary {
    pub files_verified: i16,
    pub kind: Option<String>,
    pub platform: Option<String>,
    pub title_id: Option<String>,
    pub container: Option<String>,
    pub vcodec: Option<String>,
    pub height: Option<i32>,
    pub hdr: Vec<String>,
    pub bit_depth: Option<i16>,
    pub duration_ms: Option<i64>,
    pub audio_codecs: Vec<String>,
    pub audio_langs: Vec<String>,
    pub sub_langs: Vec<String>,
    pub flags: Vec<String>,
    /// The search facets (D122, `facet_values`): `k:video`, `r:2160p`, `v:hevc`,
    /// `h:dolby_vision`, `a:fre`, `s:eng`, `ac:eac3`, `p:ps5`, `x:atmos`; and the track kinds
    /// (D124): `ak:commentary`, `ap:eng:commentary` (language and kind of one audio track),
    /// `sk:sdh`, `sp:fre:sdh`, `sf:image` (subtitle format class, `sub_format`).
    pub tags: Vec<String>,
}

fn push_unique(v: &mut Vec<String>, s: &str) {
    if !v.iter().any(|x| x == s) {
        v.push(s.to_string());
    }
}

/// Resolution class of a picture: its height, or the 16:9 height of its width when that is
/// larger (D122: a 2.39:1 UHD film is 3840×1606 and must class as `2160p`, a 1920×800 one as
/// `1080p`; release names count the width the same way).
pub fn resolution(width: Option<u32>, height: u32) -> &'static str {
    let by_width = width.map_or(0, |w| u32::try_from(u64::from(w) * 9 / 16).unwrap_or(0));
    match height.max(by_width) {
        h if h >= 2000 => "2160p",
        h if h >= 1000 => "1080p",
        h if h >= 700 => "720p",
        h if h >= 560 => "576p",
        h if h >= 470 => "480p",
        _ => "sd",
    }
}

/// The format class of a subtitle codec (D124, docs/PROBES.md "Track kinds"): `text`, `image`
/// or `cc` (CEA-608/708 captions); None for a codec of no known class. The app keeps the same
/// table (`ProbeFormat.kt`).
pub fn sub_format(codec: &str) -> Option<&'static str> {
    match codec {
        "subrip" | "ass" | "ssa" | "webvtt" | "mov_text" | "ttml" | "hdmv_text_subtitle"
        | "dvb_teletext" | "arib_caption" | "kate" | "usf" => Some("text"),
        "hdmv_pgs_subtitle" | "dvd_subtitle" | "dvb_subtitle" | "xsub" => Some("image"),
        "eia_608" | "eia_708" => Some("cc"),
        _ => None,
    }
}

/// The pair facets (D125): a language filter and a kind filter of the same family become one
/// filter on the `{lang}:{kind}` pairs of single tracks: (language facet, kind facet, pair
/// facet).
pub const PAIR_FACETS: [(&str, &str, &str); 2] = [
    ("probe_language", "probe_audio_kind", "probe_audio_pair"),
    ("probe_subtitles", "probe_sub_kind", "probe_sub_pair"),
];

/// The header-probe search filters of `q` (D122) as (facet name, values), the language codes
/// normalised as stored (ISO 639-2/B: `fr`, `fra`, `FRE` → `fre`). A code that names no
/// language is kept as typed, so it matches nothing instead of dropping the filter.
///
/// D125: a language filter with a kind filter of the same family (`probe_subtitles` with
/// `probe_sub_kind`, `probe_language` with `probe_audio_kind`) is replaced by one pair filter
/// (`probe_sub_pair` / `probe_audio_pair`, values `{lang}:{kind}` for every language and kind
/// asked): "French + SDH" is one French SDH track, not French subtitles and some SDH track.
/// Only the stored kinds ([`PROBE_SUB_KINDS`], [`PROBE_AUDIO_KINDS`]) are paired, so the pair
/// filter holds at most 9 values per language however many kinds a request lists (review of
/// D125: an unchecked cross product of two long lists would exhaust memory); when no kind
/// asked is a stored one, the first is kept, so the filter still matches nothing.
pub fn search_filters(q: &magnetar_core::query::SearchQuery) -> Vec<(&'static str, Vec<String>)> {
    let mut out = q.probe_filters();
    for (name, values) in out.iter_mut() {
        if matches!(*name, "probe_language" | "probe_subtitles") {
            let mut norm: Vec<String> = Vec::with_capacity(values.len());
            for v in values.iter() {
                let n = normalize_lang(v).unwrap_or_else(|| v.clone());
                if !norm.contains(&n) {
                    norm.push(n);
                }
            }
            *values = norm;
        }
    }
    for (lang, kind, pair) in PAIR_FACETS {
        let langs = out.iter().find(|(n, _)| *n == lang).map(|(_, v)| v.clone());
        let kinds = out.iter().find(|(n, _)| *n == kind).map(|(_, v)| v.clone());
        if let (Some(langs), Some(asked)) = (langs, kinds) {
            out.retain(|(n, _)| *n != lang && *n != kind);
            let known: &[&str] = if kind == "probe_sub_kind" {
                &PROBE_SUB_KINDS
            } else {
                &PROBE_AUDIO_KINDS
            };
            let mut kinds: Vec<String> = Vec::new();
            for k in &asked {
                if known.contains(&k.as_str()) && !kinds.contains(k) {
                    kinds.push(k.clone());
                }
            }
            if kinds.is_empty() {
                kinds.extend(asked.into_iter().take(1));
            }
            let pairs = langs
                .iter()
                .flat_map(|l| kinds.iter().map(move |k| format!("{l}:{k}")))
                .collect();
            out.push((pair, pairs));
        }
    }
    out
}

/// The `probe_attrs.tags` prefixes a header-probe facet (D122) reads; none for another name.
pub fn tag_prefixes(name: &str) -> &'static [&'static str] {
    match name {
        "probe_language" => &["a:"],
        "probe_subtitles" => &["s:"],
        "probe_resolution" => &["r:"],
        "probe_codec" => &["v:"],
        "probe_hdr" => &["h:"],
        "probe_audio" => &["ac:", "x:"],
        // D125 track kinds; the pair facets are internal (`search_filters`).
        "probe_sub_kind" => &["sk:"],
        "probe_audio_kind" => &["ak:"],
        "probe_sub_format" => &["sf:"],
        "probe_sub_pair" => &["sp:"],
        "probe_audio_pair" => &["ap:"],
        _ => &[],
    }
}

/// Search facet values (D122) of the stored `probe_attrs.tags` for facet `name`: the tag
/// suffixes of its prefixes, plus `any` on `probe_hdr` when the picture has an HDR format.
pub fn facet_values(name: &str, tags: &[String]) -> Vec<String> {
    let prefixes = tag_prefixes(name);
    let mut out: Vec<String> = Vec::new();
    for t in tags {
        if let Some(v) = prefixes.iter().find_map(|p| t.strip_prefix(p))
            && !v.is_empty()
            && !out.iter().any(|x| x == v)
        {
            out.push(v.to_string());
        }
    }
    if name == "probe_hdr" && !out.is_empty() {
        out.push("any".to_string());
    }
    out
}

/// The summary of a report as detected (no correction applied).
pub fn summary(r: &ProbeReport) -> Summary {
    summary_of(&r.files)
}

/// The summary columns and tags of `files` (D117, D122, D124). Since D129 `files` are the
/// effective files (`probe_fix::apply`): a track marked removed counts nowhere, an added track
/// (no codec) gives no `ac:` tag and no audio codec.
pub fn summary_of(files: &[ProbeFile]) -> Summary {
    let mut s = Summary {
        files_verified: files.iter().filter(|f| f.verified).count() as i16,
        ..Summary::default()
    };
    for f in files {
        for x in &f.flags {
            push_unique(&mut s.flags, x);
        }
    }
    let Some(main) = files
        .iter()
        .max_by_key(|f| (f.size, std::cmp::Reverse(f.i)))
    else {
        return s;
    };
    let kind = main.label.split('.').next().unwrap_or("").to_string();
    push_unique(&mut s.tags, &format!("k:{kind}"));
    s.kind = Some(kind);
    if let Some(g) = &main.game {
        push_unique(&mut s.tags, &format!("p:{}", g.platform));
        s.platform = Some(g.platform.clone());
        s.title_id = g.title_id.clone();
    }
    if let Some(v) = &main.video {
        s.container = Some(v.container.clone());
        s.duration_ms = v.duration_ms.and_then(|d| i64::try_from(d).ok());
        if let Some(t) = v.video.first() {
            s.vcodec = Some(t.codec.clone());
            push_unique(&mut s.tags, &format!("v:{}", t.codec));
            if let Some(h) = t.height {
                s.height = i32::try_from(h).ok();
                push_unique(&mut s.tags, &format!("r:{}", resolution(t.width, h)));
            }
            s.bit_depth = t.bit_depth.map(i16::from);
            for h in &t.hdr {
                push_unique(&mut s.hdr, h);
                push_unique(&mut s.tags, &format!("h:{h}"));
            }
        }
        let removed =
            |f: &Option<magnetar_core::model::ProbeTrackFix>| f.as_ref().is_some_and(|f| f.removed);
        for a in v.audio.iter().filter(|a| !removed(&a.fix)) {
            if !a.codec.is_empty() {
                push_unique(&mut s.audio_codecs, &a.codec);
                push_unique(&mut s.tags, &format!("ac:{}", a.codec));
            }
            if let Some(l) = &a.lang {
                push_unique(&mut s.audio_langs, l);
                // D124: a commentary, music-only or karaoke track is not audio in its language.
                let speech = !a
                    .kinds
                    .iter()
                    .any(|k| AUDIO_KINDS_NOT_LANGUAGE.contains(&k.as_str()));
                if speech {
                    push_unique(&mut s.tags, &format!("a:{l}"));
                }
            }
            for k in &a.kinds {
                push_unique(&mut s.tags, &format!("ak:{k}"));
                if let Some(l) = &a.lang {
                    push_unique(&mut s.tags, &format!("ap:{l}:{k}"));
                }
            }
            if a.atmos {
                push_unique(&mut s.tags, "x:atmos");
            }
            if a.dts_x {
                push_unique(&mut s.tags, "x:dts_x");
            }
        }
        for t in v.subs.iter().filter(|t| !removed(&t.fix)) {
            if let Some(l) = &t.lang {
                push_unique(&mut s.sub_langs, l);
                // D124: forced, signs, commentary and description tracks are not "subtitles
                // in that language".
                let full = !t
                    .kinds
                    .iter()
                    .any(|k| SUB_KINDS_NOT_LANGUAGE.contains(&k.as_str()));
                if full {
                    push_unique(&mut s.tags, &format!("s:{l}"));
                }
            }
            for k in &t.kinds {
                push_unique(&mut s.tags, &format!("sk:{k}"));
                if let Some(l) = &t.lang {
                    push_unique(&mut s.tags, &format!("sp:{l}:{k}"));
                }
            }
            if let Some(class) = sub_format(&t.codec) {
                push_unique(&mut s.tags, &format!("sf:{class}"));
            }
        }
    }
    s
}

/// One report to store: the stored row's v1 infohash (hex) and what the worker sent.
pub struct Upload {
    pub infohash: String,
    pub report: ProbeReport,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct StoreOutcome {
    pub stored: u32,
    /// Refused by [`validate`], or no storable row (none, blocked), or a weaker
    /// result than the one already stored.
    pub rejected: u32,
}

/// Validate and write reports a worker uploaded with dicts the server just verified and
/// persisted. A row is written only when the `dht_torrents` row exists and the hash is not
/// blocked (adult rows are probed too, D119); a re-probe replaces the stored one only when it is of a newer probe version or verifies at least as
/// many files. One savepoint per row (D110).
pub async fn store(
    pool: &PgPool,
    worker: &str,
    items: Vec<Upload>,
    now: i64,
) -> sqlx::Result<StoreOutcome> {
    let mut o = StoreOutcome::default();
    if items.is_empty() {
        return Ok(o);
    }
    let hashes: Vec<String> = items.iter().map(|u| u.infohash.clone()).collect();
    let rows: Vec<(String, Json<Vec<TorrentFile>>)> = sqlx::query_as(
        "SELECT d.infohash, d.files FROM dht_torrents d
         WHERE d.infohash = ANY($1)
           AND NOT EXISTS (SELECT 1 FROM blocked_infohashes b WHERE b.infohash = d.infohash)",
    )
    .bind(&hashes)
    .fetch_all(pool)
    .await?;
    let files: std::collections::HashMap<String, Vec<TorrentFile>> =
        rows.into_iter().map(|(h, f)| (h, f.0)).collect();
    let mut tx = pool.begin().await?;
    for u in items {
        let Some(list) = files.get(&u.infohash) else {
            o.rejected += 1;
            continue;
        };
        if let Err(code) = validate(&u.report, list) {
            o.rejected += 1;
            tracing::warn!(worker = %worker, "worker probe refused: {code}");
            continue;
        }
        sqlx::query("SAVEPOINT probe_row").execute(&mut *tx).await?;
        match write_one(&mut tx, worker, &u, now).await {
            Ok(written) => {
                sqlx::query("RELEASE SAVEPOINT probe_row")
                    .execute(&mut *tx)
                    .await?;
                if written {
                    o.stored += 1;
                } else {
                    o.rejected += 1;
                }
            }
            Err(e) if crate::dht::persist::row_level(&e) => {
                sqlx::query("ROLLBACK TO SAVEPOINT probe_row")
                    .execute(&mut *tx)
                    .await?;
                sqlx::query("RELEASE SAVEPOINT probe_row")
                    .execute(&mut *tx)
                    .await?;
                o.rejected += 1;
                tracing::warn!(
                    "worker probe: row skipped: {}",
                    crate::dht::persist::error_class(&e)
                );
            }
            Err(e) => return Err(e),
        }
    }
    tx.commit().await?;
    Ok(o)
}

async fn write_one(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    worker: &str,
    u: &Upload,
    now: i64,
) -> sqlx::Result<bool> {
    let r = &u.report;
    // D129: the summary and tags carry the user's track corrections, so a re-probe (D127's
    // included) keeps them in search. The probe row is locked before the corrections are read,
    // as the correction write does (`probe_fix::correct`): no lost update, no deadlock.
    sqlx::query("SELECT 1 FROM probe_attrs WHERE infohash = $1 FOR UPDATE")
        .bind(&u.infohash)
        .execute(&mut **tx)
        .await?;
    let fixes = crate::probe_fix::load(&mut **tx, &u.infohash).await?;
    let s = summary_of(&crate::probe_fix::apply(&r.files, r.v, &fixes).0);
    let bytes_in = i32::try_from(r.bytes_in).unwrap_or(i32::MAX);
    // The guard again inside the transaction: the row may have gone since the read above.
    let written: Option<i32> = sqlx::query_scalar(
        "INSERT INTO probe_attrs AS p (infohash, probe_version, probed_at, supplied_by, status,
             files_probed, files_verified, bytes_in, kind, platform, title_id, container, vcodec,
             height, hdr, bit_depth, duration_ms, audio_codecs, audio_langs, sub_langs, flags, tags)
         SELECT $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18,
                $19, $20, $21, $22
         WHERE EXISTS (SELECT 1 FROM dht_torrents d WHERE d.infohash = $1)
           AND NOT EXISTS (SELECT 1 FROM blocked_infohashes b WHERE b.infohash = $1)
         ON CONFLICT (infohash) DO UPDATE SET
             probe_version = EXCLUDED.probe_version, probed_at = EXCLUDED.probed_at,
             supplied_by = EXCLUDED.supplied_by, status = EXCLUDED.status,
             files_probed = EXCLUDED.files_probed, files_verified = EXCLUDED.files_verified,
             bytes_in = EXCLUDED.bytes_in, kind = EXCLUDED.kind, platform = EXCLUDED.platform,
             title_id = EXCLUDED.title_id, container = EXCLUDED.container,
             vcodec = EXCLUDED.vcodec, height = EXCLUDED.height, hdr = EXCLUDED.hdr,
             bit_depth = EXCLUDED.bit_depth, duration_ms = EXCLUDED.duration_ms,
             audio_codecs = EXCLUDED.audio_codecs, audio_langs = EXCLUDED.audio_langs,
             sub_langs = EXCLUDED.sub_langs, flags = EXCLUDED.flags, tags = EXCLUDED.tags
         WHERE EXCLUDED.probe_version > p.probe_version
            OR (EXCLUDED.probe_version = p.probe_version
                AND EXCLUDED.files_verified >= p.files_verified)
         RETURNING 1",
    )
    .bind(&u.infohash)
    .bind(r.v as i16)
    .bind(now)
    .bind(worker)
    .bind(&r.status)
    .bind(r.files.len() as i16)
    .bind(s.files_verified)
    .bind(bytes_in)
    .bind(&s.kind)
    .bind(&s.platform)
    .bind(&s.title_id)
    .bind(&s.container)
    .bind(&s.vcodec)
    .bind(s.height)
    .bind(&s.hdr)
    .bind(s.bit_depth)
    .bind(s.duration_ms)
    .bind(&s.audio_codecs)
    .bind(&s.audio_langs)
    .bind(&s.sub_langs)
    .bind(&s.flags)
    .bind(&s.tags)
    .fetch_optional(&mut **tx)
    .await?;
    if written.is_none() {
        return Ok(false);
    }
    sqlx::query(
        "INSERT INTO probe_files (infohash, files) VALUES ($1, $2)
         ON CONFLICT (infohash) DO UPDATE SET files = EXCLUDED.files",
    )
    .bind(&u.infohash)
    .bind(Json(&r.files))
    .execute(&mut **tx)
    .await?;
    Ok(true)
}

/// `probe_attrs` version, time, status, files probed / verified, `probe_files.files`, and the
/// DHT row's title and files.
type StoredProbeRow = (
    i16,
    i64,
    String,
    i16,
    i16,
    Json<Vec<ProbeFile>>,
    String,
    Json<Vec<TorrentFile>>,
);

/// Where the DHT row's file `i` is in `shown` (the list the detail returns as `files`): the
/// same position when the lists are the same, else the entry with the same path (the DHT
/// path, or with the torrent's root folder `title/` on either side) and a compatible size.
pub fn shown_index(
    title: &str,
    dht: &[TorrentFile],
    shown: &[TorrentFile],
    i: usize,
) -> Option<usize> {
    let f = dht.get(i)?;
    if shown.get(i) == Some(f) {
        return Some(i);
    }
    let root = format!("{title}/");
    let bare = |p: &str| -> String { p.strip_prefix(root.as_str()).unwrap_or(p).to_string() };
    let want = bare(&f.path);
    shown.iter().position(|s| {
        bare(&s.path) == want && (s.size.is_none() || f.size.is_none() || s.size == f.size)
    })
}

/// The stored probe of `infohash` for the detail API: indexes into `shown` (the file list the
/// detail returns as `files`, a Nostr event's or the DHT row's; entries not in it are left
/// out), paths from `shown`, descriptions from sniff's label registry. None for a torrent
/// never probed (or whose DHT row is gone). Adult torrents show their probe too (D119).
pub async fn probe_for(
    pool: &PgPool,
    infohash: &str,
    shown: &[TorrentFile],
) -> sqlx::Result<Option<ProbeInfo>> {
    let row: Option<StoredProbeRow> = sqlx::query_as(
        "SELECT p.probe_version, p.probed_at, p.status, p.files_probed, p.files_verified, f.files,
                d.title, d.files
         FROM probe_attrs p
         JOIN probe_files f USING (infohash)
         JOIN dht_torrents d USING (infohash)
         WHERE p.infohash = $1",
    )
    .bind(infohash)
    .fetch_optional(pool)
    .await?;
    let Some((version, probed_at, status, probed, verified, list, title, dht)) = row else {
        return Ok(None);
    };
    let version = u16::try_from(version).unwrap_or(0);
    // D129: the user's track corrections over the detection.
    let fixes = crate::probe_fix::load(pool, infohash).await?;
    let (effective, unplaced) = crate::probe_fix::apply(&list.0, version, &fixes);
    let path_of = |i: u32| -> String {
        shown_index(&title, &dht.0, shown, i as usize)
            .and_then(|at| shown.get(at))
            .or_else(|| dht.0.get(i as usize))
            .map_or(String::new(), |f| f.path.clone())
    };
    let unplaced = unplaced
        .into_iter()
        .map(|u| u.info(path_of(u.fix.file_i)))
        .collect();
    let files = effective
        .into_iter()
        .filter_map(|f| {
            let at = shown_index(&title, &dht.0, shown, f.i as usize)?;
            let stored = &shown[at];
            Some(ProbeFileInfo {
                index: u32::try_from(at).ok()?,
                dht_index: f.i,
                path: stored.path.clone(),
                size: f.size,
                description: sniff::label::info(&f.label)
                    .map_or(String::new(), |l| l.description.to_string()),
                label: f.label,
                confidence: f.confidence,
                verified: f.verified,
                flags: f.flags,
                game: f.game,
                video: f.video,
            })
        })
        .collect();
    Ok(Some(ProbeInfo {
        version,
        probed_at,
        status,
        verified: probed > 0 && verified == probed,
        files,
        unplaced,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(i: u32, size: u64, label: &str) -> ProbeFile {
        ProbeFile {
            i,
            size,
            label: label.into(),
            confidence: 97,
            verified: true,
            ..Default::default()
        }
    }

    fn stored(sizes: &[u64]) -> Vec<TorrentFile> {
        sizes
            .iter()
            .enumerate()
            .map(|(i, s)| TorrentFile {
                path: format!("f{i}"),
                size: Some(*s),
            })
            .collect()
    }

    fn report(files: Vec<ProbeFile>) -> ProbeReport {
        ProbeReport {
            v: PROBE_VERSION,
            status: "ok".into(),
            files,
            ..Default::default()
        }
    }

    #[test]
    fn platforms_cover_sniff() {
        let texts: HashSet<String> = PLATFORMS.iter().filter_map(enum_text).collect();
        assert_eq!(texts.len(), PLATFORMS.len());
        assert_eq!(platform_of("ps5"), Some(sniff::Platform::Ps5));
        assert_eq!(
            platform_of("neo_geo_pocket"),
            Some(sniff::Platform::NeoGeoPocket)
        );
        assert_eq!(platform_of("amiga"), None);
    }

    #[test]
    fn languages_are_normalised() {
        for (i, o) in [
            ("en", Some("eng")),
            ("en-US", Some("eng")),
            ("ENG", Some("eng")),
            ("fra", Some("fre")),
            ("fre", Some("fre")),
            ("de", Some("ger")),
            ("pt_BR", Some("por")),
            ("und", None),
            ("", None),
            ("x1", None),
            ("english", None),
        ] {
            assert_eq!(normalize_lang(i).as_deref(), o, "{i}");
        }
    }

    /// Review finding: numbers were never bounded and a payload block was taken under any
    /// label (`text.plain` with `platform: ps5`, a height past `i32::MAX` stored NULL yet
    /// tagged `r:2160p`). The worker drops what the server refuses.
    #[test]
    fn validation_bounds_numbers_and_fits_blocks_to_labels() {
        let files = stored(&[100, 5000]);
        let track = |t: ProbeVideoTrack| ProbeVideo {
            container: "matroska".into(),
            video: vec![t],
            ..Default::default()
        };
        let hevc = || ProbeVideoTrack {
            codec: "hevc".into(),
            height: Some(2160),
            ..Default::default()
        };
        let with_video = |label: &str, v: ProbeVideo| {
            report(vec![ProbeFile {
                video: Some(v),
                ..file(1, 5000, label)
            }])
        };
        assert_eq!(
            validate(&with_video("video.mkv", track(hevc())), &files),
            Ok(())
        );
        assert_eq!(
            validate(&with_video("audio.flac", track(hevc())), &files),
            Ok(())
        );
        let bad = [
            (with_video("text.plain", track(hevc())), "video_label"),
            (
                with_video(
                    "video.mkv",
                    track(ProbeVideoTrack {
                        height: Some(u32::MAX),
                        ..hevc()
                    }),
                ),
                "video_track",
            ),
            (
                with_video(
                    "video.mkv",
                    track(ProbeVideoTrack {
                        fps_milli: Some(u32::MAX),
                        ..hevc()
                    }),
                ),
                "video_track",
            ),
            (
                with_video(
                    "video.mkv",
                    track(ProbeVideoTrack {
                        bit_depth: Some(255),
                        ..hevc()
                    }),
                ),
                "video_track",
            ),
            (
                with_video(
                    "video.mkv",
                    track(ProbeVideoTrack {
                        width: Some(0),
                        ..hevc()
                    }),
                ),
                "video_track",
            ),
            (
                with_video(
                    "video.mkv",
                    ProbeVideo {
                        duration_ms: Some(u64::MAX),
                        ..track(hevc())
                    },
                ),
                "video_numbers",
            ),
            (
                with_video(
                    "video.mkv",
                    ProbeVideo {
                        bitrate_kbps: Some(u64::MAX),
                        ..track(hevc())
                    },
                ),
                "video_numbers",
            ),
            (
                with_video(
                    "video.mkv",
                    ProbeVideo {
                        audio: vec![ProbeAudioTrack {
                            codec: "eac3".into(),
                            channels: Some(255),
                            ..Default::default()
                        }],
                        ..track(hevc())
                    },
                ),
                "audio_track",
            ),
            (
                report(vec![ProbeFile {
                    game: Some(ProbeGame {
                        platform: "ps5".into(),
                        ..Default::default()
                    }),
                    ..file(1, 5000, "text.plain")
                }]),
                "game_label",
            ),
        ];
        for (r, code) in bad {
            assert_eq!(validate(&r, &files), Err(code), "{r:?}");
        }
        // A game block on a game label still passes.
        let game = report(vec![ProbeFile {
            game: Some(ProbeGame {
                platform: "ps5".into(),
                ..Default::default()
            }),
            ..file(1, 5000, "game.ps5.pkg")
        }]);
        assert_eq!(validate(&game, &files), Ok(()));
    }

    /// Review finding: on a torrent with a Nostr and a DHT row, probe indexes pointed into the
    /// DHT list while the detail shows the Nostr one.
    #[test]
    fn probe_indexes_point_into_the_shown_list() {
        let f = |p: &str, n: u64| TorrentFile {
            path: p.into(),
            size: Some(n),
        };
        let dht = vec![f("a.nfo", 10), f("b.mkv", 500)];
        assert_eq!(shown_index("T", &dht, &dht, 1), Some(1));
        let nostr = vec![f("T/b.mkv", 500), f("T/a.nfo", 10)];
        assert_eq!(shown_index("T", &dht, &nostr, 1), Some(0));
        assert_eq!(shown_index("T", &dht, &nostr, 0), Some(1));
        // Same path, another size: not the same file.
        assert_eq!(shown_index("T", &dht, &[f("b.mkv", 499)], 1), None);
        // Not listed: left out.
        assert_eq!(shown_index("T", &dht, &[f("c.mkv", 500)], 1), None);
        assert_eq!(shown_index("T", &dht, &nostr, 2), None);
    }

    #[test]
    fn validation_follows_the_servers_file_list() {
        let files = stored(&[100, 5000]);
        assert_eq!(
            validate(&report(vec![file(1, 5000, "video.mkv")]), &files),
            Ok(())
        );
        let bad = [
            (report(vec![file(2, 5000, "video.mkv")]), "index"),
            (report(vec![file(1, 4999, "video.mkv")]), "size"),
            (report(vec![file(1, 5000, "video.nonsense")]), "label"),
            (
                report(vec![file(1, 5000, "video.mkv"), file(1, 5000, "video.mkv")]),
                "duplicate",
            ),
            (report(vec![]), "file_count"),
            (
                report((0..17).map(|i| file(i, 1, "video.mkv")).collect()),
                "file_count",
            ),
            (
                report(vec![ProbeFile {
                    confidence: 79,
                    ..file(1, 5000, "video.mkv")
                }]),
                "confidence",
            ),
            (
                report(vec![ProbeFile {
                    flags: vec!["trust_me".into()],
                    ..file(1, 5000, "video.mkv")
                }]),
                "flags",
            ),
            (
                report(vec![ProbeFile {
                    game: Some(ProbeGame {
                        platform: "ps5".into(),
                        title: Some("x".repeat(MAX_TITLE + 1)),
                        ..Default::default()
                    }),
                    ..file(1, 5000, "game.ps5.pkg")
                }]),
                "text",
            ),
            (
                report(vec![ProbeFile {
                    game: Some(ProbeGame {
                        platform: "ps5".into(),
                        db_id: Some("not-a-serial".into()),
                        ..Default::default()
                    }),
                    ..file(1, 5000, "game.ps5.pkg")
                }]),
                "db_id",
            ),
            (
                report(vec![ProbeFile {
                    game: Some(ProbeGame {
                        platform: "amiga".into(),
                        ..Default::default()
                    }),
                    ..file(1, 5000, "game.ps5.pkg")
                }]),
                "platform",
            ),
            (
                report(vec![ProbeFile {
                    video: Some(ProbeVideo {
                        container: "matroska".into(),
                        audio: vec![ProbeAudioTrack {
                            codec: "eac3".into(),
                            lang: Some("en\u{7}".into()),
                            ..Default::default()
                        }],
                        ..Default::default()
                    }),
                    ..file(1, 5000, "video.mkv")
                }]),
                "audio_track",
            ),
            (
                report(vec![ProbeFile {
                    video: Some(ProbeVideo {
                        container: "matroska".into(),
                        subs: (0..65)
                            .map(|_| ProbeSubTrack {
                                codec: "srt".into(),
                                lang: None,
                                ..Default::default()
                            })
                            .collect(),
                        ..Default::default()
                    }),
                    ..file(1, 5000, "video.mkv")
                }]),
                "tracks",
            ),
            (
                ProbeReport {
                    v: PROBE_VERSION + 1,
                    ..report(vec![file(1, 5000, "video.mkv")])
                },
                "version",
            ),
            (
                ProbeReport {
                    status: "choked".into(),
                    ..report(vec![file(1, 5000, "video.mkv")])
                },
                "status",
            ),
        ];
        for (r, code) in bad {
            assert_eq!(validate(&r, &files), Err(code));
        }
    }

    #[test]
    fn summary_and_tags_come_from_the_main_file() {
        let mut big = file(1, 5000, "video.mkv");
        big.flags = vec!["agreed".into()];
        big.video = Some(ProbeVideo {
            container: "matroska".into(),
            duration_ms: Some(7_200_000),
            video: vec![ProbeVideoTrack {
                codec: "hevc".into(),
                height: Some(2160),
                bit_depth: Some(10),
                hdr: vec!["dolby_vision".into(), "hdr10".into()],
                ..Default::default()
            }],
            audio: vec![
                ProbeAudioTrack {
                    codec: "eac3".into(),
                    lang: Some("fre".into()),
                    atmos: true,
                    ..Default::default()
                },
                ProbeAudioTrack {
                    codec: "eac3".into(),
                    lang: Some("eng".into()),
                    ..Default::default()
                },
            ],
            subs: vec![ProbeSubTrack {
                codec: "subrip".into(),
                lang: Some("eng".into()),
                ..Default::default()
            }],
            ..Default::default()
        });
        let mut small = file(0, 100, "text.plain");
        small.verified = false;
        small.flags = vec!["extension_mismatch".into()];
        let s = summary(&report(vec![small, big]));
        assert_eq!(s.files_verified, 1);
        assert_eq!(s.kind.as_deref(), Some("video"));
        assert_eq!(s.height, Some(2160));
        assert_eq!(s.vcodec.as_deref(), Some("hevc"));
        assert_eq!(s.audio_langs, vec!["fre", "eng"]);
        assert_eq!(s.audio_codecs, vec!["eac3"]);
        assert_eq!(s.flags, vec!["extension_mismatch", "agreed"]);
        for t in [
            "k:video",
            "r:2160p",
            "v:hevc",
            "h:dolby_vision",
            "a:fre",
            "s:eng",
            "x:atmos",
        ] {
            assert!(s.tags.iter().any(|x| x == t), "{t}: {:?}", s.tags);
        }
    }

    /// D122: the class follows the width too (scope films), as release names count it.
    #[test]
    fn resolution_class_counts_the_width() {
        assert_eq!(resolution(Some(3840), 1606), "2160p", "2.39:1 UHD");
        assert_eq!(resolution(Some(3840), 2160), "2160p");
        assert_eq!(
            resolution(Some(7680), 3200),
            "2160p",
            "8K is in the top class"
        );
        assert_eq!(resolution(Some(1920), 800), "1080p");
        assert_eq!(resolution(Some(1440), 1080), "1080p", "4:3 HD");
        assert_eq!(resolution(Some(1280), 536), "720p");
        assert_eq!(resolution(Some(720), 576), "576p");
        assert_eq!(resolution(Some(720), 480), "480p");
        assert_eq!(
            resolution(None, 1606),
            "1080p",
            "no width: the height alone"
        );
        assert_eq!(resolution(Some(640), 272), "sd");
        assert_eq!(resolution(Some(u32::MAX), 1), "2160p");
        // Through the summary that writes the stored tag (was `r:1080p`).
        let mut scope = file(0, 5000, "video.mkv");
        scope.video = Some(ProbeVideo {
            container: "matroska".into(),
            video: vec![ProbeVideoTrack {
                codec: "hevc".into(),
                width: Some(3840),
                height: Some(1606),
                ..Default::default()
            }],
            ..Default::default()
        });
        let s = summary(&report(vec![scope]));
        assert!(s.tags.iter().any(|t| t == "r:2160p"), "{:?}", s.tags);
        assert_eq!(s.height, Some(1606), "the stored height stays the real one");
    }

    #[test]
    fn search_facets_and_filters_from_tags() {
        let tags: Vec<String> = [
            "k:video",
            "v:hevc",
            "r:2160p",
            "h:dolby_vision",
            "h:hdr10",
            "ac:truehd",
            "a:fre",
            "a:eng",
            "x:atmos",
            "s:eng",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(facet_values("probe_language", &tags), vec!["fre", "eng"]);
        assert_eq!(facet_values("probe_subtitles", &tags), vec!["eng"]);
        assert_eq!(facet_values("probe_resolution", &tags), vec!["2160p"]);
        assert_eq!(facet_values("probe_codec", &tags), vec!["hevc"]);
        assert_eq!(
            facet_values("probe_hdr", &tags),
            vec!["dolby_vision", "hdr10", "any"]
        );
        assert_eq!(facet_values("probe_audio", &tags), vec!["truehd", "atmos"]);
        assert!(
            facet_values("probe_hdr", &tags[..2]).is_empty(),
            "SDR: no any"
        );
        assert!(facet_values("nope", &tags).is_empty());
        let q = magnetar_core::query::SearchQuery {
            probe_languages: vec!["fr".into(), "FRA".into(), "eng".into(), "xx".into()],
            probe_subtitles: vec!["en-US".into()],
            probe_hdr: vec!["Any".into()],
            ..Default::default()
        };
        assert_eq!(
            search_filters(&q),
            vec![
                (
                    "probe_language",
                    vec!["fre".into(), "eng".into(), "xx".into()]
                ),
                ("probe_subtitles", vec!["eng".into()]),
                ("probe_hdr", vec!["any".into()]),
            ]
        );
    }

    /// The worker → server example of docs/PROBES.md (D123, spec §3): kinds and names on
    /// audio and subtitle tracks.
    const KINDS_JSON: &str = r#"{"container":"matroska","duration_ms":7200000,
        "video":[{"codec":"h264","width":1920,"height":1080}],
        "audio":[{"codec":"eac3","channels":6,"lang":"fre"},
                 {"codec":"ac3","channels":2,"lang":"eng","kinds":["commentary"],"name":"Director's Commentary"},
                 {"codec":"aac","channels":2,"lang":"fre","kinds":["description"],"name":"Audiodescription"}],
        "subs":[{"codec":"hdmv_pgs_subtitle","lang":"eng","kinds":["sdh"],"name":"English (SDH)"},
                {"codec":"subrip","lang":"fre","kinds":["forced"],"name":"Français (forcés)"},
                {"codec":"subrip","lang":"fre"},
                {"codec":"eia_608","kinds":["cc"]},
                {"codec":"ass","lang":"jpn","kinds":["signs"],"name":"Signs & Songs"}]}"#;

    fn kinds_video() -> ProbeVideo {
        serde_json::from_str(KINDS_JSON).unwrap()
    }

    /// D129: `pos` and `fix` are the detail's fields; a worker report carrying either is
    /// refused (a worker cannot plant a correction); a track without them writes neither.
    #[test]
    fn worker_reports_cannot_carry_corrections() {
        let files = stored(&[100, 5000]);
        let with = |v: ProbeVideo| {
            report(vec![ProbeFile {
                video: Some(v),
                ..file(1, 5000, "video.mkv")
            }])
        };
        assert_eq!(validate(&with(kinds_video()), &files), Ok(()));
        let mut v = kinds_video();
        v.audio[0].pos = Some(0);
        assert_eq!(validate(&with(v), &files), Err("audio_track"));
        let mut v = kinds_video();
        v.audio[1].fix = Some(magnetar_core::model::ProbeTrackFix::default());
        assert_eq!(validate(&with(v), &files), Err("audio_track"));
        let mut v = kinds_video();
        v.subs[2].pos = Some(2);
        assert_eq!(validate(&with(v), &files), Err("sub_track"));
        let mut v = kinds_video();
        v.subs[0].fix = Some(magnetar_core::model::ProbeTrackFix {
            removed: true,
            ..Default::default()
        });
        assert_eq!(validate(&with(v), &files), Err("sub_track"));
        // As a worker sends it (JSON): the fields are read, then refused.
        let json = r#"{"container":"matroska","subs":[{"codec":"subrip","lang":"eng",
            "fix":{"id":1,"fields":["lang"],"at":0,"by":"x"}}]}"#;
        let v: ProbeVideo = serde_json::from_str(json).unwrap();
        assert_eq!(validate(&with(v), &files), Err("sub_track"));
        // A track without them writes neither (worker reports and `probe_files` unchanged).
        let s = serde_json::to_string(&kinds_video()).unwrap();
        assert!(!s.contains("\"pos\"") && !s.contains("\"fix\""), "{s}");
    }

    /// D124: kinds are whitelisted per track type, without duplicates; names are short
    /// printable text.
    #[test]
    fn kinds_and_names_validated() {
        let v = kinds_video();
        assert_eq!(v.audio[1].kinds, vec!["commentary"]);
        assert_eq!(v.audio[1].name.as_deref(), Some("Director's Commentary"));
        assert_eq!(v.subs[3].kinds, vec!["cc"]);
        assert_eq!(check_video(&v), Ok(()));
        let files = stored(&[100, 5000]);
        let with = |v: ProbeVideo| {
            report(vec![ProbeFile {
                video: Some(v),
                ..file(1, 5000, "video.mkv")
            }])
        };
        assert_eq!(validate(&with(kinds_video()), &files), Ok(()));
        let audio = |f: &dyn Fn(&mut ProbeAudioTrack)| {
            let mut v = kinds_video();
            f(&mut v.audio[1]);
            v
        };
        let sub = |f: &dyn Fn(&mut ProbeSubTrack)| {
            let mut v = kinds_video();
            f(&mut v.subs[0]);
            v
        };
        let bad = [
            (audio(&|a| a.kinds = vec!["loud".into()]), "audio_track"),
            (
                audio(&|a| a.kinds = vec!["commentary".into(), "commentary".into()]),
                "audio_track",
            ),
            (audio(&|a| a.kinds = vec!["sdh".into()]), "audio_track"),
            (
                audio(&|a| a.name = Some("x".repeat(PROBE_MAX_TRACK_NAME + 1))),
                "audio_track",
            ),
            (
                audio(&|a| a.name = Some("two\nlines".into())),
                "audio_track",
            ),
            (audio(&|a| a.name = Some(String::new())), "audio_track"),
            (sub(&|s| s.kinds = vec!["dub".into()]), "sub_track"),
            (sub(&|s| s.kinds = vec!["Sdh".into()]), "sub_track"),
            (
                sub(&|s| s.kinds = vec!["sdh".into(), "cc".into(), "sdh".into()]),
                "sub_track",
            ),
            (
                sub(&|s| s.name = Some("é".repeat(PROBE_MAX_TRACK_NAME / 2 + 1))),
                "sub_track",
            ),
            (sub(&|s| s.name = Some("a\u{7}b".into())), "sub_track"),
        ];
        for (v, code) in bad {
            assert_eq!(check_video(&v), Err(code), "{v:?}");
            assert_eq!(validate(&with(v), &files), Err(code));
        }
        // Order is not checked; a name of exactly the limit passes.
        let ok = sub(&|s| {
            s.kinds = vec!["sdh".into(), "forced".into()];
            s.name = Some("x".repeat(PROBE_MAX_TRACK_NAME));
        });
        assert_eq!(check_video(&ok), Ok(()));
        let every = audio(&|a| a.kinds = PROBE_AUDIO_KINDS.map(String::from).to_vec());
        assert_eq!(check_video(&every), Ok(()));
    }

    /// D124 golden vector: per-track kind and pair tags, the format class, and the stricter
    /// `a:` / `s:` rule (a commentary track is not English audio; signs are not Japanese
    /// subtitles), while the language columns keep every language.
    #[test]
    fn kind_tags_and_language_rules() {
        let mut main = file(1, 5000, "video.mkv");
        main.video = Some(kinds_video());
        let s = summary(&report(vec![file(0, 100, "text.plain"), main]));
        let got: std::collections::BTreeSet<&str> = s.tags.iter().map(String::as_str).collect();
        let want: std::collections::BTreeSet<&str> = "k:video v:h264 r:1080p \
            ac:eac3 ac:ac3 ac:aac a:fre ak:commentary ap:eng:commentary ak:description \
            ap:fre:description s:eng sk:sdh sp:eng:sdh sf:image sk:forced sp:fre:forced sf:text \
            s:fre sk:cc sf:cc sk:signs sp:jpn:signs"
            .split_whitespace()
            .collect();
        assert_eq!(got, want);
        assert_eq!(s.tags.len(), want.len(), "no duplicate tags");
        assert_eq!(s.audio_langs, vec!["fre", "eng"]);
        assert_eq!(s.sub_langs, vec!["eng", "fre", "jpn"]);
        // Through the facets the index reads.
        assert_eq!(facet_values("probe_language", &s.tags), vec!["fre"]);
        assert_eq!(facet_values("probe_subtitles", &s.tags), vec!["eng", "fre"]);
        assert_eq!(
            facet_values("probe_sub_kind", &s.tags),
            vec!["sdh", "forced", "cc", "signs"]
        );
        assert_eq!(
            facet_values("probe_audio_kind", &s.tags),
            vec!["commentary", "description"]
        );
        assert_eq!(
            facet_values("probe_sub_format", &s.tags),
            vec!["image", "text", "cc"]
        );
        assert_eq!(
            facet_values("probe_sub_pair", &s.tags),
            vec!["eng:sdh", "fre:forced", "jpn:signs"]
        );
        assert_eq!(
            facet_values("probe_audio_pair", &s.tags),
            vec!["eng:commentary", "fre:description"]
        );
        // Music and karaoke tracks lose `a:` too; dub, original... keep it.
        for (kind, lang_tag) in [
            ("music", false),
            ("karaoke", false),
            ("dub", true),
            ("original", true),
            ("description", true),
            ("dialogue", true),
        ] {
            let mut f = file(0, 5000, "video.mkv");
            f.video = Some(ProbeVideo {
                container: "matroska".into(),
                audio: vec![ProbeAudioTrack {
                    codec: "aac".into(),
                    lang: Some("ger".into()),
                    kinds: vec![kind.into()],
                    ..Default::default()
                }],
                ..Default::default()
            });
            let s = summary(&report(vec![f]));
            assert_eq!(s.tags.iter().any(|t| t == "a:ger"), lang_tag, "{kind}");
            assert_eq!(s.audio_langs, vec!["ger"], "{kind}");
        }
        for (kind, lang_tag) in [
            ("description", false),
            ("commentary", false),
            ("sdh", true),
            ("cc", true),
            ("honorifics", true),
            ("bilingual", true),
            ("easy_reader", true),
        ] {
            let mut f = file(0, 5000, "video.mkv");
            f.video = Some(ProbeVideo {
                container: "matroska".into(),
                subs: vec![ProbeSubTrack {
                    codec: "subrip".into(),
                    lang: Some("ger".into()),
                    kinds: vec![kind.into()],
                    ..Default::default()
                }],
                ..Default::default()
            });
            let s = summary(&report(vec![f]));
            assert_eq!(s.tags.iter().any(|t| t == "s:ger"), lang_tag, "{kind}");
            assert_eq!(s.sub_langs, vec!["ger"], "{kind}");
        }
    }

    /// The format classes of spec §1.3 (the app keeps the same table).
    #[test]
    fn subtitle_format_classes() {
        for c in [
            "subrip",
            "ass",
            "ssa",
            "webvtt",
            "mov_text",
            "ttml",
            "hdmv_text_subtitle",
            "dvb_teletext",
            "arib_caption",
            "kate",
            "usf",
        ] {
            assert_eq!(sub_format(c), Some("text"), "{c}");
        }
        for c in ["hdmv_pgs_subtitle", "dvd_subtitle", "dvb_subtitle", "xsub"] {
            assert_eq!(sub_format(c), Some("image"), "{c}");
        }
        assert_eq!(sub_format("eia_608"), Some("cc"));
        assert_eq!(sub_format("eia_708"), Some("cc"));
        assert_eq!(sub_format("srt"), None);
        assert_eq!(sub_format(""), None);
    }

    /// Probes stored before D123 (version 1) have no `kinds` or `name`: they still parse, and
    /// serialise back unchanged (no empty fields).
    #[test]
    fn old_probe_json_parses_without_kinds() {
        let old = r#"{"container":"matroska","audio":[{"codec":"eac3","channels":6,"lang":"fre","atmos":true}],"subs":[{"codec":"subrip","lang":"eng"}]}"#;
        let v: ProbeVideo = serde_json::from_str(old).unwrap();
        assert!(v.audio[0].kinds.is_empty() && v.audio[0].name.is_none());
        assert!(v.subs[0].kinds.is_empty() && v.subs[0].name.is_none());
        assert_eq!(check_video(&v), Ok(()));
        let back = serde_json::to_value(&v).unwrap();
        assert_eq!(back["audio"][0].get("kinds"), None);
        assert_eq!(back["audio"][0].get("name"), None);
        assert_eq!(back["subs"][0].get("kinds"), None);
        assert_eq!(
            back["subs"][0],
            serde_json::json!({"codec": "subrip", "lang": "eng"})
        );
        let mut f = file(0, 5000, "video.mkv");
        f.video = Some(v);
        let s = summary(&report(vec![f]));
        assert!(s.tags.iter().any(|t| t == "a:fre"));
        assert!(s.tags.iter().any(|t| t == "s:eng"));
        assert!(
            !s.tags
                .iter()
                .any(|t| t.starts_with("ak:") || t.starts_with("sk:") || t.starts_with("sp:")),
            "{:?}",
            s.tags
        );
    }

    /// D125: a language and a kind of the same family become one filter on single tracks'
    /// `{lang}:{kind}` pairs; other filters, and a kind alone, are untouched.
    #[test]
    fn pair_filters_replace_language_and_kind() {
        use magnetar_core::query::SearchQuery;
        let q = SearchQuery {
            probe_subtitles: vec!["fr".into()],
            probe_sub_kinds: vec!["sdh".into()],
            ..Default::default()
        };
        assert_eq!(
            search_filters(&q),
            vec![("probe_sub_pair", vec!["fre:sdh".to_string()])]
        );
        let q = SearchQuery {
            probe_languages: vec!["fra".into(), "en".into()],
            probe_audio_kinds: vec!["Description".into(), "commentary".into()],
            probe_subtitles: vec!["jpn".into()],
            probe_sub_formats: vec!["image".into()],
            probe_hdr: vec!["any".into()],
            ..Default::default()
        };
        assert_eq!(
            search_filters(&q),
            vec![
                ("probe_subtitles", vec!["jpn".to_string()]),
                ("probe_hdr", vec!["any".to_string()]),
                ("probe_sub_format", vec!["image".to_string()]),
                (
                    "probe_audio_pair",
                    vec![
                        "fre:description".to_string(),
                        "fre:commentary".to_string(),
                        "eng:description".to_string(),
                        "eng:commentary".to_string(),
                    ]
                ),
            ]
        );
        // A kind of the other family does not pair.
        let q = SearchQuery {
            probe_languages: vec!["fre".into()],
            probe_sub_kinds: vec!["sdh".into()],
            ..Default::default()
        };
        assert_eq!(
            search_filters(&q),
            vec![
                ("probe_language", vec!["fre".to_string()]),
                ("probe_sub_kind", vec!["sdh".to_string()]),
            ]
        );
        // Only stored kinds are paired (review of D125): thousands of junk kinds with
        // thousands of languages stay |languages| × 1, not a cross product of both lists.
        let junk = |p: &str| (0..5000).map(|i| format!("{p}{i}")).collect::<Vec<_>>();
        let q = SearchQuery {
            probe_subtitles: junk("l"),
            probe_sub_kinds: [vec!["sdh".to_string()], junk("k"), vec!["sdh".into()]].concat(),
            ..Default::default()
        };
        let f = search_filters(&q);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].0, "probe_sub_pair");
        assert_eq!(f[0].1.len(), 5000);
        assert!(f[0].1.iter().all(|v| v.ends_with(":sdh")));
        // No stored kind at all: the first one asked is kept, so nothing matches.
        let q = SearchQuery {
            probe_subtitles: vec!["fre".into()],
            probe_sub_kinds: junk("k"),
            ..Default::default()
        };
        assert_eq!(
            search_filters(&q),
            vec![("probe_sub_pair", vec!["fre:k0".to_string()])]
        );
        for (facet, prefix) in [
            ("probe_sub_kind", "sk:"),
            ("probe_audio_kind", "ak:"),
            ("probe_sub_format", "sf:"),
            ("probe_sub_pair", "sp:"),
            ("probe_audio_pair", "ap:"),
        ] {
            assert_eq!(tag_prefixes(facet), &[prefix], "{facet}");
        }
    }

    #[test]
    fn worker_reports_keep_only_storable_detections() {
        use magnetar_core::probe::FileResult;
        let det = |label: &str, confidence: u8| sniff::Detection {
            label: label.into(),
            mime: String::new(),
            description: String::new(),
            confidence,
            fields: Vec::new(),
            game: None,
            video: None,
            flags: sniff::Flags::default(),
            extensions: Vec::new(),
            candidates: Vec::new(),
        };
        let fr = |index: u32, d: sniff::Detection, verified: bool| FileResult {
            index,
            size: 10,
            detection: d,
            verified,
            agreed: 2,
            notes: vec!["surprise_verified"],
        };
        let mut r = ProbeResult {
            status: ProbeStatus::Partial,
            files: vec![
                fr(0, det("video.mkv", 97), true),
                fr(1, det("video.mkv", 60), true),
                fr(2, det("no.such.label", 99), true),
            ],
            bytes_in: 1,
            uploaded: 0,
            peers_used: 2,
            unchoked: true,
            incomplete: true,
            surprises: 1,
        };
        let w = report_of(&r).unwrap();
        assert_eq!(w.status, "partial");
        assert_eq!(w.files.len(), 1, "below 80 and unknown labels are dropped");
        assert_eq!(w.files[0].flags, vec!["agreed", "surprise_verified"]);
        assert_eq!(validate(&w, &stored(&[10])), Ok(()));
        r.status = ProbeStatus::Choked;
        assert!(report_of(&r).is_none());
    }

    /// D121: the Dolby Vision profile and base-layer id travel with the `dolby_vision` flag,
    /// so the app can show "Dolby Vision 8.1"; never alone, never out of range.
    #[test]
    fn dolby_vision_profile_is_kept_with_its_flag() {
        use sniff::video::{DolbyVision, HdrFormat, VideoTrack};
        let dv = DolbyVision {
            profile: 8,
            level: 6,
            compatibility: Some(1),
            rpu: true,
            enhancement_layer: false,
            base_layer: true,
        };
        let info = |hdr: Vec<HdrFormat>| sniff::VideoInfo {
            container: "mkv".into(),
            video: vec![VideoTrack {
                codec: "hevc".into(),
                hdr,
                dolby_vision: Some(dv),
                ..Default::default()
            }],
            ..Default::default()
        };
        let v = video_of(&info(vec![HdrFormat::DolbyVision, HdrFormat::Hdr10])).unwrap();
        assert_eq!(v.video[0].dv_profile, Some(8));
        assert_eq!(v.video[0].dv_compat, Some(1));
        assert_eq!(check_video(&v), Ok(()));
        let plain = video_of(&info(vec![HdrFormat::Hdr10])).unwrap();
        assert_eq!(
            (plain.video[0].dv_profile, plain.video[0].dv_compat),
            (None, None),
            "a configuration record without the flag is not reported"
        );
        let track = |t: ProbeVideoTrack| ProbeVideo {
            container: "matroska".into(),
            video: vec![t],
            ..Default::default()
        };
        let dv_track = ProbeVideoTrack {
            codec: "hevc".into(),
            hdr: vec!["dolby_vision".into()],
            dv_profile: Some(5),
            ..Default::default()
        };
        assert_eq!(check_video(&track(dv_track.clone())), Ok(()));
        for bad in [
            ProbeVideoTrack {
                hdr: vec!["hdr10".into()],
                ..dv_track.clone()
            },
            ProbeVideoTrack {
                dv_profile: Some(21),
                ..dv_track.clone()
            },
            ProbeVideoTrack {
                dv_compat: Some(16),
                ..dv_track.clone()
            },
        ] {
            assert_eq!(check_video(&track(bad)), Err("video_track"));
        }
    }

    /// D117: the main server never probes. A prober is built in one place only, the worker's
    /// crawler, and only from its `CrawlSetup::probe` (`MAGNETAR_PROBE=true` on a worker).
    #[test]
    fn only_the_worker_crawler_builds_a_prober() {
        fn walk(dir: &std::path::Path, out: &mut Vec<(String, String)>) {
            for e in std::fs::read_dir(dir).unwrap() {
                let p = e.unwrap().path();
                if p.is_dir() {
                    walk(&p, out);
                } else if p.extension().is_some_and(|x| x == "rs") {
                    out.push((
                        p.display().to_string(),
                        std::fs::read_to_string(&p).unwrap(),
                    ));
                }
            }
        }
        let mut files = Vec::new();
        walk(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
            &mut files,
        );
        let needle = concat!("Prober", "::new(");
        let users: Vec<&str> = files
            .iter()
            .filter(|(_, src)| src.contains(needle))
            .map(|(p, _)| p.as_str())
            .collect();
        assert_eq!(users.len(), 1, "{users:?}");
        assert!(users[0].ends_with("worker/crawl.rs"), "{users:?}");
        let fetch_probe = concat!("probe: ", "true");
        assert!(
            !files.iter().any(|(_, src)| src.contains(fetch_probe)),
            "the fetcher's probe mode is never forced on"
        );
    }

    #[test]
    fn text_is_cleaned() {
        assert_eq!(clean("a\u{0}b\nc", 10).as_deref(), Some("abc"));
        assert_eq!(clean("ééé", 5).as_deref(), Some("éé"));
        assert_eq!(clean("\u{1}", 5), None);
        assert_eq!(token("HEVC").as_deref(), Some("hevc"));
        assert_eq!(token("h 264"), None);
    }
}

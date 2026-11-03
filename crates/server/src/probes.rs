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
    ProbeAudioTrack, ProbeFile, ProbeFileInfo, ProbeGame, ProbeInfo, ProbeSubTrack, ProbeVideo,
    ProbeVideoTrack, TorrentFile,
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
    let container = token(&v.container)?;
    let video = v
        .video
        .iter()
        .filter_map(|t| {
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
            })
        })
        .take(MAX_AUDIO_TRACKS)
        .collect();
    let subs = v
        .subtitles
        .iter()
        .filter_map(|s| {
            Some(ProbeSubTrack {
                codec: token(&s.codec)?,
                lang: s.language.as_deref().and_then(normalize_lang),
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
    Some(ProbeReport {
        v: PROBE_VERSION,
        status: r.status.code().into(),
        bytes_in: r.bytes_in,
        uploaded: r.uploaded,
        peers: r.peers_used,
        files,
    })
}

// ---------------------------------------------------------------- server side

fn ok_text(s: &str, max: usize) -> bool {
    !s.is_empty() && s.len() <= max && !s.chars().any(char::is_control)
}

fn ok_opt(s: &Option<String>, max: usize) -> bool {
    s.as_deref().is_none_or(|s| ok_text(s, max))
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
        {
            return Err("video_track");
        }
    }
    for a in &v.audio {
        if !ok_token(&a.codec) || !ok_lang(&a.lang) || !in_range(a.channels, 1, MAX_CHANNELS) {
            return Err("audio_track");
        }
    }
    for s in &v.subs {
        if !ok_token(&s.codec) || !ok_lang(&s.lang) {
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
    /// Facets for a later search filter (`k:video`, `r:2160p`, `v:hevc`, `h:dolby_vision`,
    /// `a:fre`, `s:eng`, `ac:eac3`, `p:ps5`, `x:atmos`); no index on them yet (check1 #3).
    pub tags: Vec<String>,
}

fn push_unique(v: &mut Vec<String>, s: &str) {
    if !v.iter().any(|x| x == s) {
        v.push(s.to_string());
    }
}

/// Resolution class of a picture height.
pub fn resolution(height: u32) -> &'static str {
    match height {
        h if h >= 2000 => "2160p",
        h if h >= 1000 => "1080p",
        h if h >= 700 => "720p",
        h if h >= 560 => "576p",
        h if h >= 470 => "480p",
        _ => "sd",
    }
}

pub fn summary(r: &ProbeReport) -> Summary {
    let mut s = Summary {
        files_verified: r.files.iter().filter(|f| f.verified).count() as i16,
        ..Summary::default()
    };
    for f in &r.files {
        for x in &f.flags {
            push_unique(&mut s.flags, x);
        }
    }
    let Some(main) = r
        .files
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
                push_unique(&mut s.tags, &format!("r:{}", resolution(h)));
            }
            s.bit_depth = t.bit_depth.map(i16::from);
            for h in &t.hdr {
                push_unique(&mut s.hdr, h);
                push_unique(&mut s.tags, &format!("h:{h}"));
            }
        }
        for a in &v.audio {
            push_unique(&mut s.audio_codecs, &a.codec);
            push_unique(&mut s.tags, &format!("ac:{}", a.codec));
            if let Some(l) = &a.lang {
                push_unique(&mut s.audio_langs, l);
                push_unique(&mut s.tags, &format!("a:{l}"));
            }
            if a.atmos {
                push_unique(&mut s.tags, "x:atmos");
            }
            if a.dts_x {
                push_unique(&mut s.tags, "x:dts_x");
            }
        }
        for t in &v.subs {
            if let Some(l) = &t.lang {
                push_unique(&mut s.sub_langs, l);
                push_unique(&mut s.tags, &format!("s:{l}"));
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
    let s = summary(r);
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
    let files = list
        .0
        .into_iter()
        .filter_map(|f| {
            let at = shown_index(&title, &dht.0, shown, f.i as usize)?;
            let stored = &shown[at];
            Some(ProbeFileInfo {
                index: u32::try_from(at).ok()?,
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
        version: u16::try_from(version).unwrap_or(0),
        probed_at,
        status,
        verified: probed > 0 && verified == probed,
        files,
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

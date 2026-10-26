//! Release attributes parsed from a torrent name and its file list (ARCHITECTURE.md D44):
//! resolution, source, video codec, HDR, audio codecs and channels, audio and subtitle
//! languages (French scene tags included), release group, season/episode, year, editions, and
//! for music and books the format, bit depth, sample rate and bitrate.
//!
//! No dependencies: the server stores the result per infohash (`torrent_attrs`) and indexes it,
//! the phone parses its own cache the same way, so filters mean the same thing on both.
//! docs/FILTERS.md has the language mapping table and the measured accuracy.
//!
//! The name is cut into tokens at separators (`.` `_` space brackets `-` …). Tokens that can
//! also be ordinary title words (`French`, `Italian`, `DE`, `VO`, `DV`, `TS`, …) only count in
//! the **tag region**, which starts at the first unambiguous release tag (a year after the
//! first word, `S01E02`, `1080p`, `WEB-DL`, `x264`, `VOSTFR`, `MULTi`, …), or inside a bracket
//! that holds nothing but language words (`[ENG]`, `[Hindi+English]`). Scene tags that are never
//! title words (`VFF`, `VFQ`, `VOSTFR`, `TRUEFRENCH`, `MULTi`, …) count anywhere but on the first
//! word of the title.

use serde::{Deserialize, Serialize};

use crate::model::{Category, TorrentFile, TorrentSummary};

/// Bump when the parser's output changes: stored attributes of an older version are re-parsed
/// (server backfill and the phone cache).
pub const PARSER_VERSION: i16 = 1;

/// Parsed attributes of one release. Every field is optional/empty when the name does not say.
/// Values are lowercase codes (tables in docs/FILTERS.md).
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ReleaseAttrs {
    /// "4320p" | "2160p" | "1440p" | "1080p" | "720p" | "576p" | "480p" | "sd"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolution: Option<String>,
    /// "remux" | "bluray" | "web-dl" | "webrip" | "hdrip" | "hdtv" | "dvd" | "vhs" | "screener" |
    /// "telesync" | "cam"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// "h264" | "h265" | "av1" | "vp9" | "xvid" | "mpeg2" | "vc1"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub video_codec: Option<String>,
    /// "hdr10" | "hdr10+" | "dv" | "hlg"
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub hdr: Vec<String>,
    /// Video (8/10/12) or audio (16/24) bit depth.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bit_depth: Option<u8>,
    /// "aac" | "ac3" | "eac3" | "dts" | "dts-hd" | "dts-x" | "truehd" | "atmos" | "flac" |
    /// "opus" | "mp3" | "pcm" | "alac" | "vorbis" | "dsd"
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub audio: Vec<String>,
    /// "1.0" | "2.0" | "5.1" | "7.1"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channels: Option<String>,
    /// Audio languages: ISO 639-1 codes, regional variants "fr-fr" (VFF), "fr-ca" (VFQ),
    /// "es-419" (Latino), "pt-br", and the pseudo-languages "original" (VO, VOSTFR, MULTi) and
    /// "multi" (several tracks).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub languages: Vec<String>,
    /// Subtitle languages (ISO 639-1, "multi").
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub subtitles: Vec<String>,
    /// French scene tags as written, normalised: "multi", "vff", "vfq", "vf2", "vfi", "vof",
    /// "truefrench", "french", "vostfr", "subfrench", "vo".
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub french: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub season: Option<i32>,
    /// Last season of a multi-season pack (`S01-S05`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub season_end: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub episode: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub episode_end: Option<i32>,
    /// Complete series / season pack ("COMPLETE", "INTEGRALE", "Batch").
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub complete: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub year: Option<i32>,
    /// "repack" | "proper" | "extended" | "uncut" | "unrated" | "directors-cut" | "imax" |
    /// "remastered" | "theatrical" | "criterion" | "limited" | "internal" | "3d" |
    /// "open-matte" | "hybrid" | "custom" | "light" | "fansub" | "uncensored"
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub editions: Vec<String>,
    /// Music bitrate: "320", "256", …, "v0", "v2".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bitrate: Option<String>,
    /// Music sample rate in kHz: "44.1", "48", "96", "192".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sample_rate: Option<String>,
    /// Book/comic/audiobook formats: "epub" | "pdf" | "mobi" | "azw3" | "cbz" | "cbr" |
    /// "djvu" | "fb2" | "m4b".
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub formats: Vec<String>,
}

/// Resolutions best first (grouping and display order).
pub const RESOLUTIONS: &[&str] = &[
    "4320p", "2160p", "1440p", "1080p", "720p", "576p", "480p", "sd",
];
pub const SOURCES: &[&str] = &[
    "remux", "bluray", "web-dl", "webrip", "hdrip", "hdtv", "dvd", "vhs", "screener", "telesync",
    "cam",
];
pub const VIDEO_CODECS: &[&str] = &["h265", "h264", "av1", "vp9", "xvid", "mpeg2", "vc1"];
pub const HDR_FORMATS: &[&str] = &["dv", "hdr10+", "hdr10", "hlg"];
pub const AUDIO_CODECS: &[&str] = &[
    "truehd", "atmos", "dts-x", "dts-hd", "dts", "eac3", "ac3", "aac", "flac", "opus", "mp3",
    "pcm", "alac", "vorbis", "dsd",
];

/// Separators between tokens. `-` is one too; `Tok::sep_before` keeps the exact text between
/// two tokens (the anime episode rule wants " - ", the group rule a bare "-").
fn is_sep(c: char) -> bool {
    matches!(
        c,
        ' ' | '.'
            | '_'
            | '-'
            | '['
            | ']'
            | '('
            | ')'
            | '{'
            | '}'
            | ','
            | '+'
            | '|'
            | '/'
            | '\\'
            | ';'
            | ':'
            | '~'
            | '!'
            | '?'
            | '*'
            | '"'
            | '\''
            | '&'
            | '#'
            | '@'
            | '='
            | '–'
            | '—'
            | '「'
            | '」'
            | '【'
            | '】'
            | '（'
            | '）'
            | '［'
            | '］'
            | '　'
            | '・'
            | '\u{2019}'
    ) || c.is_whitespace()
}

fn opens(c: char) -> bool {
    matches!(c, '[' | '(' | '{' | '【' | '（' | '［' | '「')
}

fn closes(c: char) -> bool {
    matches!(c, ']' | ')' | '}' | '】' | '）' | '］' | '」')
}

#[derive(Clone, Debug)]
struct Tok {
    /// As written.
    raw: String,
    /// Lowercase.
    low: String,
    /// Separator text before this token (empty for the first one).
    sep_before: String,
    /// Bracket instance this token is in (0 = none).
    bracket: u32,
}

impl Tok {
    fn upper(&self) -> bool {
        self.raw.chars().any(|c| c.is_alphabetic()) && !self.raw.chars().any(|c| c.is_lowercase())
    }
    fn digits(&self) -> Option<i64> {
        (!self.low.is_empty()
            && self.low.len() <= 6
            && self.low.bytes().all(|b| b.is_ascii_digit()))
        .then(|| self.low.parse().ok())
        .flatten()
    }
}

/// Spellings folded before tokenising so a tag is one token (`DD+5.1` → `DDP5.1`).
const FOLDS: &[(&str, &str)] = &[
    ("dd+", "ddp"),
    ("hdr10+", "hdr10plus"),
    ("e-ac-3", "eac3"),
    ("e-ac3", "eac3"),
    ("blu-ray", "bluray"),
    ("web-dl", "webdl"),
    ("web-rip", "webrip"),
    ("dts-hd", "dtshd"),
    ("dts-x", "dtsx"),
    ("dts:x", "dtsx"),
    ("dts-ma", "dtsma"),
    ("vc-1", "vc1"),
    ("mpeg-2", "mpeg2"),
    ("dolby vision", "dovi"),
    ("dolby.vision", "dovi"),
    ("director's cut", "directorscut"),
    ("directors cut", "directorscut"),
    ("directors.cut", "directorscut"),
    ("open matte", "openmatte"),
    ("open.matte", "openmatte"),
    ("pt-br", "ptbr"),
    ("multi-sub", "multisub"),
    ("multi-subs", "multisubs"),
    ("multi.sub", "multisub"),
    ("multi sub", "multisub"),
    ("multi-audio", "multi"),
    ("dual-audio", "dualaudio"),
    ("dual audio", "dualaudio"),
    ("dual.audio", "dualaudio"),
    ("dual áudio", "dualaudio"),
    ("vost-fr", "vostfr"),
    ("vost.fr", "vostfr"),
    ("vost fr", "vostfr"),
    ("sub.french", "subfrench"),
    ("sub french", "subfrench"),
    ("sous-titres", "soustitres"),
    ("he-aac", "aac"),
    ("aac-lc", "aac"),
    ("x-264", "x264"),
    ("x-265", "x265"),
    ("10-bit", "10bit"),
    ("10 bit", "10bit"),
    ("8-bit", "8bit"),
    ("12-bit", "12bit"),
    ("16-bit", "16bit"),
    ("24-bit", "24bit"),
];

/// ASCII-case-insensitive replace of every `from` (lowercase) by `to`.
fn fold(name: &str) -> String {
    let mut out = name.to_string();
    for (from, to) in FOLDS {
        let mut search_from = 0;
        loop {
            let lower = out.to_lowercase();
            // Lowercasing can change byte lengths for non-ASCII text: only use positions when
            // both strings line up.
            if lower.len() != out.len() {
                break;
            }
            let Some(rel) = lower[search_from..].find(from) else {
                break;
            };
            let at = search_from + rel;
            out.replace_range(at..at + from.len(), to);
            search_from = at + to.len();
        }
    }
    out
}

fn tokenize(name: &str) -> Vec<Tok> {
    let mut toks = Vec::new();
    let mut cur = String::new();
    let mut sep = String::new();
    let mut depth = 0u32;
    let mut bracket_id = 0u32;
    let mut current_bracket = 0u32;
    for c in name.chars() {
        if is_sep(c) {
            if !cur.is_empty() {
                toks.push(Tok {
                    low: cur.to_lowercase(),
                    raw: std::mem::take(&mut cur),
                    sep_before: std::mem::take(&mut sep),
                    bracket: current_bracket,
                });
            }
            if opens(c) {
                depth += 1;
                if depth == 1 {
                    bracket_id += 1;
                    current_bracket = bracket_id;
                }
            } else if closes(c) {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    current_bracket = 0;
                }
            }
            sep.push(c);
        } else {
            cur.push(c);
        }
    }
    if !cur.is_empty() {
        toks.push(Tok {
            low: cur.to_lowercase(),
            raw: cur,
            sep_before: sep,
            bracket: current_bracket,
        });
    }
    toks
}

fn push(v: &mut Vec<String>, s: &str) {
    if !v.iter().any(|x| x == s) {
        v.push(s.to_string());
    }
}

/// ISO 639-1 code for a language word or code. `strong` = unambiguous (never a title word):
/// those count outside the tag region too.
fn language(word: &str) -> Option<&'static str> {
    Some(match word {
        "english" | "anglais" | "inglés" | "ingles" => "en",
        "eng" => "en",
        "french" | "français" | "francais" | "fra" | "fre" => "fr",
        "german" | "deutsch" | "allemand" | "ger" | "deu" => "de",
        "italian" | "italiano" | "italien" | "ita" => "it",
        "spanish" | "español" | "espanol" | "castellano" | "espagnol" | "spa" | "esp" => "es",
        "latino" | "lat" => "es-419",
        "japanese" | "japonais" | "jpn" | "jap" => "ja",
        "korean" | "coréen" | "kor" => "ko",
        "russian" | "rus" => "ru",
        "portuguese" | "portugais" | "português" | "por" => "pt",
        "ptbr" | "brazilian" => "pt-br",
        "chinese" | "chinois" | "mandarin" | "cantonese" | "chi" | "zho" => "zh",
        "dutch" | "néerlandais" | "nld" | "dut" => "nl",
        "polish" | "pol" => "pl",
        "hindi" | "hin" => "hi",
        "turkish" | "tur" => "tr",
        "arabic" | "ara" => "ar",
        "swedish" | "swe" => "sv",
        "norwegian" | "nor" => "no",
        "danish" | "dan" => "da",
        "finnish" | "fin" => "fi",
        "czech" | "cze" | "ces" => "cs",
        "hungarian" | "hun" => "hu",
        "ukrainian" | "ukr" => "uk",
        "thai" | "tha" => "th",
        "vietnamese" | "vie" => "vi",
        "greek" | "gre" | "ell" => "el",
        "hebrew" | "heb" => "he",
        "tamil" | "tam" => "ta",
        "telugu" | "tel" => "te",
        "romanian" | "rum" | "ron" => "ro",
        _ => return None,
    })
}

/// Two-letter codes: only uppercase and in the tag region (`FR`, `EN`, `PT`…); lowercase `fr`
/// is accepted for books (`2024.fr.epub`).
fn two_letter(word: &str) -> Option<&'static str> {
    Some(match word {
        "en" => "en",
        "fr" => "fr",
        "de" => "de",
        "it" => "it",
        "es" => "es",
        "pt" => "pt",
        "jp" => "ja",
        "ja" => "ja",
        "kr" => "ko",
        "ru" => "ru",
        "nl" => "nl",
        "pl" => "pl",
        "cn" => "zh",
        _ => return None,
    })
}

/// Subtitle-language codes found in subtitle file names (`Movie.fr.srt`, `2_English.srt`):
/// the suffix context makes two- and three-letter codes safe.
fn file_language(word: &str) -> Option<&'static str> {
    if let Some(l) = language(word) {
        return Some(l);
    }
    Some(match word {
        "en" | "fr" | "de" | "it" | "es" | "pt" | "ja" | "ko" | "ru" | "nl" | "pl" | "sv"
        | "no" | "da" | "fi" | "cs" | "hu" | "tr" | "ar" | "he" | "el" | "ro" | "uk" | "th"
        | "vi" | "hi" | "zh" | "id" | "ms" => {
            return two_letter(word).or(match word {
                "ko" => Some("ko"),
                "sv" => Some("sv"),
                "no" => Some("no"),
                "da" => Some("da"),
                "fi" => Some("fi"),
                "cs" => Some("cs"),
                "hu" => Some("hu"),
                "tr" => Some("tr"),
                "ar" => Some("ar"),
                "he" => Some("he"),
                "el" => Some("el"),
                "ro" => Some("ro"),
                "uk" => Some("uk"),
                "th" => Some("th"),
                "vi" => Some("vi"),
                "hi" => Some("hi"),
                "zh" => Some("zh"),
                "id" => Some("id"),
                "ms" => Some("ms"),
                _ => None,
            });
        }
        "chs" | "cht" | "sc" | "tc" => "zh",
        "jpn" => "ja",
        "kor" => "ko",
        "fre" | "fra" => "fr",
        "ger" | "deu" => "de",
        "spa" | "esp" => "es",
        "por" => "pt",
        "ptbr" | "pob" => "pt-br",
        _ => return None,
    })
}

/// Streaming services: their tag means a web source when nothing else names one.
fn streaming(word: &str) -> bool {
    matches!(
        word,
        "amzn"
            | "nf"
            | "netflix"
            | "dsnp"
            | "dsny"
            | "hmax"
            | "atvp"
            | "pcok"
            | "pmtp"
            | "hulu"
            | "crav"
            | "stan"
            | "cr"
            | "adn"
            | "itunes"
            | "ip"
            | "shahid"
            | "viki"
            | "baha"
            | "iq"
            | "hbo"
            | "sho"
            | "mubi"
            | "tver"
    )
}

/// Height in pixels of a resolution token (`1080p`, `1920x1080`, `4k`, `bd1080p`).
fn resolution_height(t: &str) -> Option<(u32, bool)> {
    // (height, explicit): explicit = written as a resolution, not inferred from 4K/UHD.
    match t {
        "4k" | "uhd" | "2160" | "4klight" => return Some((2160, false)),
        "8k" => return Some((4320, false)),
        "fhd" => return Some((1080, false)),
        _ => {}
    }
    if let Some((w, h)) = t.split_once('x') {
        let (w, h): (u32, u32) = (w.parse().ok()?, h.parse().ok()?);
        return ((320..=8000).contains(&w) && (200..=4400).contains(&h)).then_some((h, true));
    }
    let digits_start = t.find(|c: char| c.is_ascii_digit())?;
    let prefix = &t[..digits_start];
    if !matches!(prefix, "" | "bd" | "web" | "hdtv" | "uhd" | "fhd" | "hd") {
        return None;
    }
    let rest = &t[digits_start..];
    let num_end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    let (num, suffix) = rest.split_at(num_end);
    let h: u32 = num.parse().ok()?;
    // "HD1080", "BD720": a tag prefix makes the bare height explicit too.
    let prefixed = !prefix.is_empty() && suffix.is_empty() && matches!(h, 720 | 1080 | 2160);
    if !matches!(suffix, "p" | "i") && !prefixed {
        return None;
    }
    (240..=4400).contains(&h).then_some((h, true))
}

fn resolution_bucket(h: u32) -> &'static str {
    match h {
        3000.. => "4320p",
        1600.. => "2160p",
        1300.. => "1440p",
        800.. => "1080p",
        650.. => "720p",
        540.. => "576p",
        400.. => "480p",
        _ => "sd",
    }
}

fn is_year(t: &Tok) -> Option<i32> {
    (t.low.len() == 4)
        .then(|| t.digits())
        .flatten()
        .and_then(|y| i32::try_from(y).ok())
        .filter(|y| (1900..=2099).contains(y))
}

/// `s01e02`, `s01e02e03`, `s1`, `s01` → (season, first episode, last episode).
fn season_episode(t: &str) -> Option<(i32, Option<i32>, Option<i32>)> {
    let rest = t.strip_prefix('s')?;
    let s_end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    if s_end == 0 || s_end > 3 {
        return None;
    }
    let season: i32 = rest[..s_end].parse().ok()?;
    let mut rest = &rest[s_end..];
    let mut eps: Vec<i32> = Vec::new();
    while let Some(r) = rest.strip_prefix('e') {
        let r = r.strip_prefix('p').unwrap_or(r);
        let end = r.find(|c: char| !c.is_ascii_digit()).unwrap_or(r.len());
        if end == 0 || end > 4 {
            return None;
        }
        eps.push(r[..end].parse().ok()?);
        rest = &r[end..];
    }
    if !rest.is_empty() {
        return None;
    }
    Some((
        season,
        eps.first().copied(),
        eps.last().copied().filter(|_| eps.len() > 1),
    ))
}

/// `1x02` → (1, 2).
fn cross_episode(t: &str) -> Option<(i32, i32)> {
    let (s, e) = t.split_once('x')?;
    if s.is_empty() || s.len() > 2 || !(2..=3).contains(&e.len()) {
        return None;
    }
    Some((s.parse().ok()?, e.parse().ok()?))
}

/// A height alone in a bracket (`[1080]`): a resolution, not an episode.
fn bare_height_bracket(toks: &[Tok], i: usize) -> bool {
    let b = toks[i].bracket;
    b != 0
        && matches!(
            toks[i].low.as_str(),
            "1080" | "720" | "576" | "480" | "2160"
        )
        && toks.iter().filter(|t| t.bracket == b).count() == 1
}

/// `2nd`, `4th`, `3rd`, `1st` → 2, 4, 3, 1 (seasons up to 99).
fn ordinal(t: &str) -> Option<i32> {
    let n = t
        .strip_suffix("st")
        .or_else(|| t.strip_suffix("nd"))
        .or_else(|| t.strip_suffix("rd"))
        .or_else(|| t.strip_suffix("th"))?;
    (!n.is_empty() && n.len() <= 2 && n.bytes().all(|b| b.is_ascii_digit()))
        .then(|| n.parse().ok())
        .flatten()
        .filter(|n| (1..100).contains(n))
}

/// `13v2` (a fansub's second version of episode 13) → 13.
fn versioned_episode(t: &str) -> Option<i64> {
    let (n, v) = t.split_once('v')?;
    (!n.is_empty() && n.len() <= 4 && v.len() == 1 && v.bytes().all(|b| b.is_ascii_digit()))
        .then(|| n.parse().ok())
        .flatten()
}

/// `e08`, `ep06`, `e1075` → 8, 6, 1075.
fn bare_episode(t: &str) -> Option<i32> {
    let r = t.strip_prefix("ep").or_else(|| t.strip_prefix('e'))?;
    (!r.is_empty() && r.len() <= 4 && r.bytes().all(|b| b.is_ascii_digit()))
        .then(|| r.parse().ok())
        .flatten()
}

/// Words that sit among release tags without being unambiguous tags themselves: next to one
/// of these, an ambiguous language word (`FRENCH`) or a year counts.
fn is_tagish(t: &Tok, next: Option<&Tok>) -> bool {
    is_strong(t, next)
        || language(&t.low).is_some()
        || season_episode(&t.low).is_some()
        || matches!(
            t.low.as_str(),
            "vo" | "vf"
                | "sub"
                | "subs"
                | "dub"
                | "dubbed"
                | "dual"
                | "audio"
                | "doc"
                | "comic"
                | "ebook"
                | "hybrid"
                | "fastsub"
                | "sdr"
                | "custom"
                | "remastered"
                | "remasterise"
                | "remasterisé"
                | "restaure"
                | "restauré"
                | "restored"
                | "extended"
                | "unrated"
                | "uncut"
                | "imax"
                | "final"
                | "dl"
                | "rip"
                | "web"
                | "bd"
                | "dvd5"
                | "dvd9"
                | "dvd"
                | "ad"
        )
}

/// What one token is, for the tag-region decision.
fn is_strong(t: &Tok, next: Option<&Tok>) -> bool {
    let w = t.low.as_str();
    if resolution_height(w).is_some_and(|(_, explicit)| explicit) && w != "2160" {
        return true;
    }
    if season_episode(w).is_some_and(|(_, e, _)| e.is_some()) || cross_episode(w).is_some() {
        return true;
    }
    // "Charlotte's.Web.2006": only WEB, WEB-DL or WEB.Rip are tags.
    if w == "web" {
        return t.upper() || next.is_some_and(|n| matches!(n.low.as_str(), "dl" | "rip"));
    }
    matches!(
        w,
        "bluray"
            | "bdrip"
            | "brrip"
            | "remux"
            | "webdl"
            | "webrip"
            | "hdtv"
            | "dvdrip"
            | "hdrip"
            | "x264"
            | "x265"
            | "h264"
            | "h265"
            | "hevc"
            | "avc"
            | "xvid"
            | "divx"
            | "av1"
            | "vostfr"
            | "subfrench"
            | "truefrench"
            | "vff"
            | "vfq"
            | "vf2"
            | "vfi"
            | "vof"
            | "multi"
            | "aac"
            | "ac3"
            | "dts"
            | "flac"
            | "mp3"
            | "epub"
            | "pdf"
            | "cbz"
            | "cbr"
            | "mobi"
            | "azw3"
            | "hdr"
            | "hdr10"
            | "hdr10plus"
            | "dovi"
            | "4k"
            | "uhd"
            | "integrale"
            | "intégrale"
            | "complete"
            | "repack"
            | "proper"
            | "hdlight"
            | "mhd"
            | "4klight"
            | "dualaudio"
            | "multisub"
            | "multisubs"
            | "320kbps"
    ) || (w == "h" && next.is_some_and(|n| matches!(n.low.as_str(), "264" | "265")))
}

fn audio_codec(w: &str) -> Option<(&'static str, Option<&'static str>)> {
    // (codec, channel major digit attached to the token: "ddp5" → 5).
    let (base, digit) = match w.char_indices().last() {
        Some((i, c)) if c.is_ascii_digit() && i > 0 => {
            let base = &w[..i];
            if matches!(
                base,
                "ddp"
                    | "dd"
                    | "aac"
                    | "truehd"
                    | "atmos"
                    | "opus"
                    | "flac"
                    | "pcm"
                    | "lpcm"
                    | "dts"
            ) {
                (base, Some(c))
            } else {
                (w, None)
            }
        }
        _ => (w, None),
    };
    let codec = match base {
        "aac" => "aac",
        "ac3" | "dd" => "ac3",
        "ddp" | "eac3" => "eac3",
        "dts" => "dts",
        "dtshd" | "dtsma" | "hdma" | "dtshdma" => "dts-hd",
        "dtsx" => "dts-x",
        "truehd" => "truehd",
        "atmos" => "atmos",
        "flac" => "flac",
        "opus" => "opus",
        "mp3" => "mp3",
        "pcm" | "lpcm" => "pcm",
        "alac" => "alac",
        "vorbis" | "ogg" => "vorbis",
        "dsd" | "dsf" => "dsd",
        _ => return None,
    };
    let major = digit.map(|c| match c {
        '1' => "1",
        '2' => "2",
        '5' => "5",
        '6' => "6",
        '7' => "7",
        _ => "",
    });
    Some((codec, major.filter(|m| !m.is_empty())))
}

fn channels_of(major: &str, minor: &str) -> Option<&'static str> {
    Some(match (major, minor) {
        ("1", "0") => "1.0",
        ("2", "0") => "2.0",
        ("5", "1") | ("6", "0") => "5.1",
        ("7", "1") => "7.1",
        _ => return None,
    })
}

const VIDEO_EXT: &[&str] = &[
    "mkv", "mp4", "avi", "m4v", "ts", "m2ts", "wmv", "mov", "mpg", "mpeg", "webm", "flv", "vob",
    "divx", "ogm", "rmvb", "iso",
];
const SUB_EXT: &[&str] = &["srt", "ass", "ssa", "sub", "idx", "vtt", "sup", "smi"];
const AUDIO_TRACK_EXT: &[&str] = &["mka", "ac3", "eac3", "dts", "thd", "aac"];
const MUSIC_EXT: &[(&str, &str)] = &[
    ("flac", "flac"),
    ("mp3", "mp3"),
    ("m4a", "aac"),
    ("aac", "aac"),
    ("opus", "opus"),
    ("ogg", "vorbis"),
    ("wav", "pcm"),
    ("aiff", "pcm"),
    ("alac", "alac"),
    ("dsf", "dsd"),
    ("dff", "dsd"),
    ("ape", "ape"),
    ("wv", "wavpack"),
];
const BOOK_EXT: &[&str] = &[
    "epub", "pdf", "mobi", "azw3", "azw", "cbz", "cbr", "djvu", "fb2", "m4b",
];

fn ext_of(path: &str) -> Option<String> {
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    let (_, ext) = name.rsplit_once('.')?;
    (1..=5)
        .contains(&ext.len())
        .then(|| ext.to_ascii_lowercase())
}

/// A file or folder name without directories and a known extension.
fn stem_of(path: &str) -> &str {
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    match name.rsplit_once('.') {
        Some((s, ext))
            if !s.is_empty()
                && (VIDEO_EXT.contains(&ext.to_ascii_lowercase().as_str())
                    || SUB_EXT.contains(&ext.to_ascii_lowercase().as_str())
                    || BOOK_EXT.contains(&ext.to_ascii_lowercase().as_str())
                    || MUSIC_EXT.iter().any(|(e, _)| e.eq_ignore_ascii_case(ext))) =>
        {
            s
        }
        _ => name,
    }
}

/// Words that end a release group search (`WEB-DL` must not give group "DL").
fn not_a_group(w: &str) -> bool {
    let l = w.to_lowercase();
    if language(&l).is_some()
        || two_letter(&l).is_some()
        || bare_episode(&l).is_some()
        || season_episode(&l).is_some()
        || versioned_episode(&l).is_some()
    {
        return true;
    }
    matches!(
        l.as_str(),
        "dl" | "hd"
            | "ma"
            | "x"
            | "rip"
            | "sub"
            | "subs"
            | "audio"
            | "raws"
            | "ray"
            | "1"
            | "0"
            | "notag"
            | "nogrp"
            | "nogroup"
            | "notgroup"
            | "unknown"
    ) || l.bytes().all(|b| b.is_ascii_digit())
        || resolution_height(&l).is_some()
        || audio_codec(&l).is_some()
        || matches!(
            l.as_str(),
            "x264" | "x265" | "h264" | "h265" | "hevc" | "avc" | "webdl" | "webrip" | "bluray"
        )
}

/// Release group: `…-GROUP` at the end (before a trailing "(alt title)", "[hash]" or site tag),
/// or a leading `[Group]` (fansubs). `region_byte` = where the tag region starts in `name`:
/// a hyphen inside the title (`Spider-Man`) is not a group.
fn release_group(name: &str, region_byte: usize, leading_ok: bool) -> Option<String> {
    let mut end = name.trim_end();
    for ext in [".mkv", ".mp4", ".avi", ".ts"] {
        if end.len() > ext.len()
            && end.is_char_boundary(end.len() - ext.len())
            && end[end.len() - ext.len()..].eq_ignore_ascii_case(ext)
        {
            end = &end[..end.len() - ext.len()];
        }
    }
    // Strip trailing groups: "(Choc)", "[ABCD1234]", "[rartv]", "{…}".
    loop {
        let t = end.trim_end();
        let Some(last) = t.chars().last() else { break };
        let open = match last {
            ')' => '(',
            ']' => '[',
            '}' => '{',
            _ => break,
        };
        match t.rfind(open) {
            Some(i) if i > 0 => end = &t[..i],
            _ => break,
        }
    }
    let end = end.trim_end();
    if let Some(i) = end.rfind('-') {
        let cand = end[i + 1..].trim();
        let ok = |c: &str| {
            (2..=24).contains(&c.chars().count())
                && c.chars()
                    .all(|ch| ch.is_alphanumeric() || ch == '_' || ch == '@')
                && !not_a_group(c)
        };
        if i >= region_byte && ok(cand) {
            return Some(cand.to_string());
        }
        // "Tsundere-Raws": the word before a hyphenated "Raws" belongs to the group.
        if cand.eq_ignore_ascii_case("raws") {
            let head = end[..i].trim_end();
            if let Some(j) = head.rfind(['-', '.', ' ']) {
                let prev = &head[j + 1..];
                if j >= region_byte && ok(prev) {
                    return Some(format!("{prev}-{cand}"));
                }
            }
        }
    }
    // Fansub style: "[SubsPlease] Title - 01 (1080p)". Elsewhere a leading bracket is a
    // platform or a studio ("[Switch]", "[OnlyFans]").
    let t = name.trim_start();
    if leading_ok
        && let Some(rest) = t.strip_prefix('[')
        && let Some(close) = rest.find(']')
    {
        let g = rest[..close].trim();
        let letters = g.chars().filter(|c| c.is_alphabetic()).count();
        if (2..=30).contains(&g.chars().count())
            && letters >= 2
            && g.split(' ').count() <= 3
            && resolution_height(&g.to_lowercase()).is_none()
        {
            return Some(g.to_string());
        }
    }
    None
}

#[derive(Default)]
struct Ctx {
    anime: bool,
    book: bool,
    music: bool,
    /// Films, series, anime, adult: MULTi means the French scene's French + original audio.
    /// Elsewhere (software, games, books) it only means several languages.
    video: bool,
}

/// Parse one name (torrent title or file name).
fn parse_one(name: &str, ctx: &Ctx) -> ReleaseAttrs {
    let mut a = ReleaseAttrs::default();
    let folded = fold(name.trim());
    let mut toks = tokenize(&folded);
    // A trailing file extension is not a tag (".ts" is not telesync).
    if let Some(last) = toks.last()
        && last.sep_before.ends_with('.')
        && (VIDEO_EXT.contains(&last.low.as_str()) || SUB_EXT.contains(&last.low.as_str()))
        && toks.len() > 1
    {
        toks.pop();
    }
    if toks.is_empty() {
        return a;
    }
    // The title starts after a leading "[Group]" bracket.
    let title_start = if toks[0].bracket != 0 {
        let b = toks[0].bracket;
        toks.iter()
            .position(|t| t.bracket != b)
            .unwrap_or(toks.len())
    } else {
        0
    };
    let fansub = title_start > 0 || ctx.anime;
    let mut fansub_episode = false;
    let strong_at = |i: usize| is_strong(&toks[i], toks.get(i + 1));
    let region = (title_start + 1..toks.len())
        .find(|&i| strong_at(i) || is_year(&toks[i]).is_some())
        .unwrap_or(toks.len());
    // A bracket holding only language words (`[ENG]`, `[Hindi+English]`, `(French)`).
    let lang_only_bracket = |b: u32| {
        b != 0
            && toks.iter().filter(|t| t.bracket == b).all(|t| {
                language(&t.low).is_some()
                    || matches!(t.low.as_str(), "dub" | "dubbed" | "audio" | "sub" | "subs")
            })
    };
    let in_tags = |i: usize| i >= region || lang_only_bracket(toks[i].bracket);
    // Ambiguous words also need a tag next to them (`S01E05 The French Connection` is a title).
    let tag_neighbour = |i: usize| {
        let near = |j: usize| {
            toks.get(j)
                .is_some_and(|t| is_tagish(t, toks.get(j + 1)) || is_year(t).is_some())
        };
        i + 1 == toks.len() || near(i + 1) || (i > 0 && near(i - 1))
    };
    // A book format right next to it ("…2025.fr.epub" filed as audio).
    let format_neighbour = |i: usize| {
        let fmt = |t: &Tok| BOOK_EXT.contains(&t.low.as_str());
        toks.get(i + 1).is_some_and(fmt) || (i > 0 && fmt(&toks[i - 1]))
    };
    // "Uldis Zarins - EN - PDF": an unambiguous tag next to it.
    let strong_neighbour = |i: usize| {
        toks.get(i + 1)
            .is_some_and(|n| is_strong(n, toks.get(i + 2)))
            || (i > 0 && is_strong(&toks[i - 1], Some(&toks[i])))
    };
    let lang_ok = |i: usize| {
        let t = &toks[i];
        i > title_start
            && (lang_only_bracket(t.bracket)
                || (in_tags(i) && (t.upper() || tag_neighbour(i)))
                // "…l.Ouest.FRENCH.1080p": an uppercase language right before the first tag.
                || (t.upper() && t.low.len() > 2 && tag_neighbour(i))
                || ((ctx.book || format_neighbour(i)) && t.low == "fr" && tag_neighbour(i)))
    };

    let mut years: Vec<(usize, i32)> = Vec::new();
    let mut streaming_tag = false;
    let mut explicit_res = false;
    let mut i = 0;
    while i < toks.len() {
        let t = &toks[i];
        let w = t.low.as_str();
        let next = toks.get(i + 1);
        let next_low = next.map(|n| n.low.as_str()).unwrap_or("");
        let prev_low = if i > 0 { toks[i - 1].low.as_str() } else { "" };
        let after_title = i > title_start;

        // --- year
        // Years in a long trailing bracket are an alternative title or an air date
        // ("(Les Feux de l'Amour du 16 janvier 2026 - …)").
        if after_title
            && let Some(y) = is_year(t)
            && (t.bracket == 0
                || i < region
                || toks.iter().filter(|x| x.bracket == t.bracket).count() < 4)
        {
            years.push((i, y));
        }

        // --- resolution
        if let Some((h, explicit)) = resolution_height(w)
            && (explicit || after_title)
            && (w != "2160" || in_tags(i))
        {
            let r = resolution_bucket(h);
            // The first explicit one wins ("1080p.BDRip … & 480p.LDRip" is a 1080p release).
            if (explicit && !explicit_res) || a.resolution.is_none() {
                a.resolution = Some(r.to_string());
            }
            explicit_res |= explicit;
            if w.starts_with("bd") && a.source.is_none() {
                a.source = Some("bluray".into());
            }
            if w == "4klight" {
                push(&mut a.editions, "light");
            }
        }
        if w == "sd" && in_tags(i) && a.resolution.is_none() {
            a.resolution = Some("sd".into());
        }
        // A bare height next to a source tag ("FRENCH 1080 HDTV") or alone in brackets ("[1080]").
        if matches!(w, "1080" | "720" | "576" | "480")
            && (in_tags(i) || bare_height_bracket(&toks, i))
            && a.resolution.is_none()
            && (bare_height_bracket(&toks, i)
                || next.is_some_and(|n| is_strong(n, toks.get(i + 2)))
                || (i > 0 && is_strong(&toks[i - 1], Some(t))))
        {
            a.resolution = Some(format!("{w}p"));
        }

        // --- season / episode
        if let Some((s, e, e_end)) = season_episode(w)
            && after_title
            && (e.is_some() || s <= 99)
        {
            a.season.get_or_insert(s);
            if let Some(e) = e {
                a.episode.get_or_insert(e);
            }
            if let Some(ee) = e_end {
                a.episode_end = Some(ee);
            }
            // "S1+S2+S3"
            if e.is_none() && next.is_some_and(|n| n.sep_before == "+") {
                let mut j = i + 1;
                while let Some(n) = toks.get(j).filter(|n| n.sep_before == "+")
                    && let Some((s2, None, _)) = season_episode(&n.low)
                {
                    if s2 > s {
                        a.season_end = Some(s2);
                    }
                    j += 1;
                }
            }
            // "S01-S05", "S01-05" (joined), "S01E01-E02", "S01E01-02".
            if let Some(n) = next
                && n.sep_before == "-"
            {
                if let Some((s2, None, _)) = season_episode(&n.low)
                    && e.is_none()
                    && s2 > s
                {
                    a.season_end = Some(s2);
                    i += 1;
                } else if let Some(e2) = bare_episode(&n.low).or(n.digits().map(|d| d as i32))
                    && let Some(e1) = e
                    && e2 > e1
                    && e2 - e1 < 500
                {
                    a.episode_end = Some(e2);
                    i += 1;
                } else if e.is_none()
                    && let Some(s2) = n.digits()
                    && (s as i64) < s2
                    && s2 < 100
                {
                    a.season_end = Some(s2 as i32);
                    i += 1;
                }
            }
        } else if let Some((s, e)) = cross_episode(w)
            && after_title
            && s <= 50
        {
            a.season.get_or_insert(s);
            a.episode.get_or_insert(e);
        } else if let Some(e) = bare_episode(w)
            && after_title
            && w != "e"
        {
            a.episode.get_or_insert(e);
        } else if matches!(w, "season" | "saison")
            && let Some(n) = ordinal(prev_low)
        {
            // "Jigokuraku 2nd Season - 01": the number after is the episode.
            a.season.get_or_insert(n);
        } else if matches!(
            w,
            "season" | "saison" | "seasons" | "saisons" | "staffel" | "temporada"
        ) && let Some(n) = next.and_then(Tok::digits)
            && (1..100).contains(&n)
            && !next.is_some_and(|n| n.sep_before.contains('-'))
        {
            a.season.get_or_insert(n as i32);
            // "Season 1-5", "Saison 1 à 3"
            let mut j = i + 2;
            if toks
                .get(j)
                .is_some_and(|t| matches!(t.low.as_str(), "à" | "a" | "to" | "au"))
            {
                j += 1;
            }
            if let Some(n2) = toks.get(j).and_then(Tok::digits)
                && n2 > n
                && n2 < 100
                && (toks[j].sep_before.contains('-') || j > i + 2)
            {
                a.season_end = Some(n2 as i32);
            }
        } else if matches!(w, "episode" | "épisode" | "ep")
            && !ctx.music
            && let Some(n) = next.and_then(Tok::digits)
            && n < 10_000
            && next.and_then(is_year).is_none()
        {
            a.episode.get_or_insert(n as i32);
        } else if matches!(
            w,
            "complete" | "integrale" | "intégrale" | "integral" | "batch"
        ) && after_title
            && ctx.video
            // "COMPLETE.BLURAY" / "Complete UHD BluRay" is a full disc, not a complete series.
            && !matches!(next_low, "bluray" | "uhd" | "bdmv" | "bd" | "bd25" | "bd50" | "bd66")
        {
            a.complete = true;
        } else if fansub
            && after_title
            && a.episode.is_none()
            && let Some(n) = t.digits().or_else(|| versioned_episode(&t.low))
            && t.low.len() <= 6
            && is_year(t).is_none()
            // "Gintama - 3-nen Z-gumi" is a title word, not episode 3.
            && !next.is_some_and(|n| n.sep_before == "-" && n.digits().is_none())
            && !bare_height_bracket(&toks, i)
            && (t.sep_before.contains(" - ")
                || t.sep_before.ends_with("- ")
                || (t.bracket != 0
                    && toks.iter().filter(|x| x.bracket == t.bracket).count() <= 2
                    && toks
                        .iter()
                        .filter(|x| x.bracket == t.bracket)
                        .all(|x| x.digits().is_some())))
        {
            // Anime: "Title - 07 (1080p)", "One Piece - 1152", "[05][WebRip]", "(01-12)".
            a.episode = Some(n as i32);
            fansub_episode = true;
            if let Some(n2) = next
                && (n2.sep_before == "-" || n2.sep_before == "~")
                && let Some(e2) = n2.digits()
                && e2 > n
                && e2 < 10_000
            {
                a.episode_end = Some(e2 as i32);
                i += 1;
            }
        }

        // --- source
        let source = match w {
            "remux" | "bdremux" => Some("remux"),
            "bluray" | "bdrip" | "brrip" | "bdmv" | "bd25" | "bd50" | "bd66" | "bd100" => {
                Some("bluray")
            }
            "bd" if in_tags(i) => Some("bluray"),
            "webdl" => Some("web-dl"),
            "webrip" | "webdlrip" => Some("webrip"),
            "web" if next_low == "rip" => Some("webrip"),
            "web" if next_low == "dl" || (after_title && (t.upper() || in_tags(i))) => {
                Some("web-dl")
            }
            "hdrip" => Some("hdrip"),
            "hdtv" | "pdtv" | "dsr" | "tvrip" | "hdtvrip" | "satrip" | "dvb" | "dvbrip" => {
                Some("hdtv")
            }
            "dvdrip" | "dvd" | "dvd5" | "dvd9" | "dvdr" | "fulldvd" | "dvdfull" => Some("dvd"),
            "vhsrip" | "vhs" if in_tags(i) => Some("vhs"),
            "dvdscr" | "bdscr" | "screener" | "r5" => Some("screener"),
            "scr" if in_tags(i) => Some("screener"),
            "cam" | "hdcam" | "camrip" if in_tags(i) || w != "cam" => Some("cam"),
            "telesync" | "hdts" | "pdvd" | "telecine" | "hdtc" => Some("telesync"),
            "ts" | "tc" if in_tags(i) && t.upper() => Some("telesync"),
            _ => None,
        };
        if in_tags(i) && streaming(w) && (t.upper() || w.len() > 3) {
            streaming_tag = true;
        }
        if let Some(s) = source {
            let rank = |s: &str| match s {
                "remux" => 0,
                "cam" | "telesync" | "screener" => 1,
                "bluray" => 2,
                "webrip" => 3,
                "web-dl" => 4,
                _ => 5,
            };
            if a.source.as_deref().is_none_or(|cur| rank(s) < rank(cur)) {
                a.source = Some(s.to_string());
            }
        }

        // --- video codec / bit depth
        let codec = match w {
            "x264" | "h264" | "avc" => Some("h264"),
            "x265" | "h265" | "hevc" => Some("h265"),
            "h" if next_low == "264" => Some("h264"),
            "h" if next_low == "265" => Some("h265"),
            "av1" => Some("av1"),
            "vp9" => Some("vp9"),
            "xvid" | "divx" => Some("xvid"),
            "mpeg2" => Some("mpeg2"),
            "vc1" => Some("vc1"),
            _ => None,
        };
        if let Some(c) = codec
            && (after_title || t.bracket != 0)
            && (a.video_codec.is_none() || c == "h265")
        {
            a.video_codec = Some(c.to_string());
        }
        // "FLAC16BITS": the codec glued to the bit depth.
        let bits = match w.strip_prefix("flac").filter(|r| r.contains("bit")) {
            Some(r) => {
                push(&mut a.audio, "flac");
                r
            }
            None => w,
        };
        match bits {
            "10bit" | "10bits" | "hi10p" | "hi10" => a.bit_depth = Some(10),
            "12bit" | "12bits" => a.bit_depth = Some(12),
            "8bit" | "8bits" => {
                a.bit_depth.get_or_insert(8);
            }
            "16bit" | "16bits" | "16b" => a.bit_depth = Some(16),
            "24bit" | "24bits" | "24b" => a.bit_depth = Some(24),
            "10" | "16" | "24" | "12" if matches!(next_low, "bit" | "bits") && after_title => {
                a.bit_depth = w.parse().ok();
            }
            _ => {}
        }

        // --- HDR
        match w {
            "hdr" | "hdr10" if after_title => push(&mut a.hdr, "hdr10"),
            "hdr10plus" => push(&mut a.hdr, "hdr10+"),
            "dovi" => push(&mut a.hdr, "dv"),
            "dv" if in_tags(i) && t.upper() => push(&mut a.hdr, "dv"),
            "hlg" if in_tags(i) => push(&mut a.hdr, "hlg"),
            _ => {}
        }

        // --- audio codecs and channels
        if (in_tags(i) || t.bracket != 0)
            && after_title
            && let Some((codec, major)) = audio_codec(w)
            && (w != "dd" || in_tags(i))
        {
            let codec = if codec == "dts" && next_low == "hd" {
                "dts-hd"
            } else {
                codec
            };
            push(&mut a.audio, codec);
            if let Some(m) = major {
                if let Some(c) = next.and_then(|n| channels_of(m, &n.low)) {
                    a.channels.get_or_insert(c.to_string());
                    i += 1;
                } else if m == "2" && codec != "atmos" {
                    a.channels.get_or_insert("2.0".to_string());
                }
            }
        }
        if w == "hd" && prev_low == "dts" {
            a.audio.retain(|c| c != "dts");
            push(&mut a.audio, "dts-hd");
        }
        if w == "ma" && matches!(prev_low, "hd" | "dtshd") {
            a.audio.retain(|c| c != "dts");
            push(&mut a.audio, "dts-hd");
        }
        if matches!(w, "6ch" | "6chs") {
            a.channels.get_or_insert("5.1".into());
        } else if matches!(w, "8ch" | "8chs") {
            a.channels.get_or_insert("7.1".into());
        } else if w == "2ch" {
            a.channels.get_or_insert("2.0".into());
        } else if w.len() == 1
            && in_tags(i)
            && let Some(n) = next
            && n.low.len() == 1
            && let Some(c) = channels_of(w, &n.low)
            && (n.sep_before == "."
                || audio_codec(prev_low).is_some()
                || matches!(
                    prev_low,
                    "ma" | "hd" | "atmos" | "audio" | "dualaudio" | "dual" | "aac"
                ))
        {
            a.channels.get_or_insert(c.to_string());
            i += 1;
        }

        // --- music bitrate / sample rate
        if after_title {
            if let Some(n) = w
                .strip_suffix("kbps")
                .or_else(|| w.strip_suffix("kbits"))
                .or_else(|| w.strip_suffix("kbs"))
                .or_else(|| {
                    w.strip_suffix("kb")
                        .filter(|_| next_low == "s" || in_tags(i))
                })
                .or_else(|| w.strip_suffix('k').filter(|_| in_tags(i)))
                .and_then(|n| n.parse::<u32>().ok())
                .filter(|n| (32..=1411).contains(n))
            {
                a.bitrate.get_or_insert(n.to_string());
            } else if let Some(n) = t.digits().filter(|n| (32..=1411).contains(n))
                && matches!(next_low, "kbps" | "kb" | "kbit" | "kbits")
            {
                a.bitrate.get_or_insert(n.to_string());
            } else if let Some(n) = t
                .digits()
                .filter(|n| matches!(n, 128 | 160 | 192 | 224 | 256 | 320))
                && matches!(prev_low, "mp3" | "aac" | "ogg" | "opus")
            {
                a.bitrate.get_or_insert(n.to_string());
            } else if matches!(w, "v0" | "v2") && in_tags(i) && t.upper() {
                a.bitrate.get_or_insert(w.to_string());
            }
            let khz = |s: &str| -> Option<String> {
                let v: f32 = s.parse().ok()?;
                (8.0..=768.0).contains(&v).then(|| {
                    if v.fract() == 0.0 {
                        format!("{}", v as u32)
                    } else {
                        format!("{v:.1}")
                    }
                })
            };
            if let Some(hz) = w.strip_suffix("hz").and_then(|n| n.parse::<u32>().ok())
                && (8000..=768_000).contains(&hz)
                && let Some(k) = khz(&format!("{}", hz as f32 / 1000.0))
            {
                a.sample_rate.get_or_insert(k);
            }
            if let Some(num) = w.strip_suffix("khz") {
                let full = if !num.is_empty()
                    && num.len() <= 2
                    && prev_low.len() <= 3
                    && toks[i].sep_before.chars().all(|c| c == '.' || c == ',')
                    && !prev_low.is_empty()
                    && prev_low.bytes().all(|b| b.is_ascii_digit())
                {
                    format!("{prev_low}.{num}")
                } else if num.is_empty() {
                    // "44.1 kHz": the number is in the tokens before.
                    match (i.checked_sub(2).map(|j| toks[j].low.as_str()), prev_low) {
                        (Some(a1), b1)
                            if toks[i - 1].sep_before.chars().all(|c| c == '.' || c == ',')
                                && a1.len() <= 3
                                && a1.bytes().all(|b| b.is_ascii_digit())
                                && b1.len() == 1 =>
                        {
                            format!("{a1}.{b1}")
                        }
                        (_, b1) => b1.to_string(),
                    }
                } else {
                    num.to_string()
                };
                if let Some(k) = khz(&full) {
                    a.sample_rate.get_or_insert(k);
                }
            }
        }

        // --- formats (books, audiobooks)
        if after_title
            && matches!(
                w,
                "epub" | "pdf" | "mobi" | "azw3" | "azw" | "cbz" | "cbr" | "djvu" | "fb2" | "m4b"
            )
        {
            push(&mut a.formats, if w == "azw" { "azw3" } else { w });
        }

        // --- editions
        let edition = match w {
            "repack" | "rerip" | "repack2" => Some("repack"),
            "proper" => Some("proper"),
            "real" if matches!(next_low, "proper" | "repack") => Some("proper"),
            "extended" if after_title => Some("extended"),
            "uncut" if after_title => Some("uncut"),
            "unrated" if after_title => Some("unrated"),
            "directorscut" => Some("directors-cut"),
            "dc" if in_tags(i) && t.upper() => Some("directors-cut"),
            "imax" if after_title => Some("imax"),
            "remastered" | "remaster" | "remasterise" | "remasterisé" | "restored" | "restaure"
            | "restauré"
                if after_title =>
            {
                Some("remastered")
            }
            "theatrical" if in_tags(i) => Some("theatrical"),
            "criterion" if after_title => Some("criterion"),
            "limited" if in_tags(i) && t.upper() => Some("limited"),
            "internal" if in_tags(i) => Some("internal"),
            "3d" | "hsbs" | "sbs" if in_tags(i) => Some("3d"),
            "openmatte" => Some("open-matte"),
            "hybrid" if in_tags(i) => Some("hybrid"),
            "custom" if in_tags(i) && t.upper() => Some("custom"),
            "hdlight" | "mhd" | "4klight" => Some("light"),
            "light"
                if in_tags(i)
                    && (resolution_height(prev_low).is_some()
                        || matches!(prev_low, "version" | "4k")) =>
            {
                Some("light")
            }
            "fansub" => Some("fansub"),
            "uncensored" if after_title => Some("uncensored"),
            _ => None,
        };
        if let Some(e) = edition {
            push(&mut a.editions, e);
        }

        // --- languages
        if after_title {
            language_token(
                &mut a,
                &toks,
                i,
                &lang_ok,
                in_tags(i),
                (strong_neighbour(i), format_neighbour(i)),
                ctx,
            );
        }
        i += 1;
    }

    // Year: the first one followed by a tag or alone in brackets ("Blade Runner 2049 2017
    // 1080p" is 2017, "L'Armée des Ombres 1969 REMASTERISÉ 2024" is 1969), the start of a range
    // ("2008-2013"); else the last one before the first tag, else the first one.
    let bracket_len = |b: u32| toks.iter().filter(|t| t.bracket == b).count();
    let tagged = years.iter().find(|(j, _)| {
        let t = &toks[*j];
        (t.bracket != 0 && bracket_len(t.bracket) == 1)
            || *j + 1 == toks.len()
            || is_tagish(&toks[*j + 1], toks.get(*j + 2))
    });
    let first_tag = (title_start + 1..toks.len())
        .find(|&j| strong_at(j))
        .unwrap_or(toks.len());
    let chosen = tagged.or_else(|| years.iter().rfind(|(j, _)| *j <= first_tag));
    a.year = match chosen {
        Some((j, y)) => {
            let range_start = years
                .iter()
                .find(|(pj, _)| pj + 1 == *j && toks[*j].sep_before.trim() == "-");
            Some(range_start.map_or(*y, |(_, y0)| *y0))
        }
        None => years.first().map(|(_, y)| *y),
    };

    // "AMZN", "NF", "DSNP"… without WEB-DL: still a web download.
    if a.source.is_none() && streaming_tag {
        a.source = Some("web-dl".into());
    }
    // CJK fansub conventions (subtitle and dub markers, episode numbers in the raw name).
    cjk_languages(name, &mut a);
    if a.episode.is_none() {
        a.episode = cjk_episode(name);
    }

    let region_byte = region_byte_offset(&folded, &toks, region);
    a.group = release_group(&folded, region_byte, ctx.anime || fansub_episode);
    a
}

/// Byte offset in `folded` where token `region` starts (or the end).
fn region_byte_offset(folded: &str, toks: &[Tok], region: usize) -> usize {
    if region >= toks.len() {
        return folded.len();
    }
    let mut pos = 0;
    for (k, t) in toks.iter().enumerate() {
        let Some(found) = folded[pos..].find(&t.raw) else {
            return folded.len();
        };
        let start = pos + found;
        if k == region {
            return start;
        }
        pos = start + t.raw.len();
    }
    folded.len()
}

/// "DVD" among the tags (a DVD remux is SD too).
fn dvd_seen(name: &str) -> bool {
    tokenize(name).iter().any(|t| {
        matches!(
            t.low.as_str(),
            "dvd" | "dvdrip" | "dvd5" | "dvd9" | "dvdr" | "pal" | "ntsc"
        )
    })
}

/// "第992话", "[第07集]": CJK episode numbers.
fn cjk_episode(name: &str) -> Option<i32> {
    let start = name.find('第')? + '第'.len_utf8();
    let rest = &name[start..];
    let end = rest.find(|c: char| !c.is_ascii_digit())?;
    let marker = rest[end..].chars().next()?;
    (end > 0 && matches!(marker, '话' | '話' | '集'))
        .then(|| rest[..end].parse().ok())
        .flatten()
}

fn cjk_languages(name: &str, a: &mut ReleaseAttrs) {
    let has = |s: &str| name.contains(s);
    if has("简") || has("繁") || has("字幕") || has("中字") || has("内封") || has("内嵌")
    {
        push(&mut a.subtitles, "zh");
    }
    if has("英配") {
        push(&mut a.languages, "en");
    }
    if has("中配") || has("国语") || has("國語") || has("粤语") || has("粵語") || has("国漫")
    {
        push(&mut a.languages, "zh");
    }
}

/// Languages from token `i` (audio unless a `sub` word is next to it).
fn language_token(
    a: &mut ReleaseAttrs,
    toks: &[Tok],
    i: usize,
    lang_ok: &dyn Fn(usize) -> bool,
    in_tags: bool,
    // An unambiguous tag / a book format right next to the token.
    (strong_near, format_near): (bool, bool),
    ctx: &Ctx,
) {
    let t = &toks[i];
    let w = t.low.as_str();
    let next = toks.get(i + 1).map(|n| n.low.as_str()).unwrap_or("");
    let prev = if i > 0 { toks[i - 1].low.as_str() } else { "" };
    let sub_word = |s: &str| {
        matches!(
            s,
            "sub" | "subs" | "subbed" | "subtitles" | "subtitle" | "soustitres" | "st" | "srt"
        )
    };
    let audio = |a: &mut ReleaseAttrs, l: &str| push(&mut a.languages, l);
    let french = |a: &mut ReleaseAttrs, tag: &str| push(&mut a.french, tag);
    match w {
        // Unambiguous French scene tags.
        "vff" | "truefrench" => {
            audio(a, "fr");
            audio(a, "fr-fr");
            french(a, w);
        }
        "vfq" | "vq" => {
            audio(a, "fr");
            audio(a, "fr-ca");
            french(a, "vfq");
        }
        "vf2" => {
            audio(a, "fr");
            audio(a, "fr-fr");
            audio(a, "fr-ca");
            french(a, "vf2");
        }
        "vfi" => {
            audio(a, "fr");
            french(a, "vfi");
        }
        "vof" => {
            audio(a, "fr");
            audio(a, "original");
            french(a, "vof");
        }
        "vf" if t.upper() || in_tags => {
            audio(a, "fr");
        }
        "vofstfr" => {
            audio(a, "fr");
            audio(a, "original");
            push(&mut a.subtitles, "fr");
            french(a, "vof");
        }
        "vffstfr" => {
            audio(a, "fr");
            audio(a, "fr-fr");
            push(&mut a.subtitles, "fr");
            french(a, "vff");
        }
        "vfstfr" => {
            audio(a, "fr");
            push(&mut a.subtitles, "fr");
        }
        "stfr" => push(&mut a.subtitles, "fr"),
        "vostfr" | "vostf" => {
            audio(a, "original");
            push(&mut a.subtitles, "fr");
            french(a, "vostfr");
        }
        "subfrench" | "subfr" => {
            audio(a, "original");
            push(&mut a.subtitles, "fr");
            french(a, "subfrench");
        }
        "vosta" | "vosten" => {
            audio(a, "original");
            push(&mut a.subtitles, "en");
        }
        "vost" => audio(a, "original"),
        "vo" if t.upper() || in_tags => {
            audio(a, "original");
            french(a, "vo");
        }
        "multi" if sub_word(next) => push(&mut a.subtitles, "multi"),
        "multi" if ctx.video => {
            // French scene MULTi: the French dub plus the original track.
            audio(a, "multi");
            audio(a, "fr");
            audio(a, "original");
            french(a, "multi");
        }
        "multi" => audio(a, "multi"),
        "multisub" | "multisubs" | "multisubtitles" => push(&mut a.subtitles, "multi"),
        "dualaudio" | "dual" if w == "dualaudio" || in_tags || t.upper() => audio(a, "multi"),
        "dub" | "dubbed" => {
            if let Some(l) = language(prev) {
                audio(a, l);
            }
        }
        "chs" | "cht" | "big5" if t.upper() || t.bracket != 0 => push(&mut a.subtitles, "zh"),
        "gb" if t.upper() && t.bracket != 0 => push(&mut a.subtitles, "zh"),
        "esub" | "esubs" => push(&mut a.subtitles, "en"),
        // Erai-raws marks Chinese animation "(CA)".
        "ca" if ctx.anime
            && t.upper()
            && t.bracket != 0
            && toks.iter().filter(|x| x.bracket == t.bracket).count() == 1 =>
        {
            audio(a, "zh")
        }
        _ if w.starts_with("multi")
            && w[5..].bytes().all(|b| b.is_ascii_digit())
            && w.len() > 5 =>
        {
            // "MULTi12" (games: 12 languages).
            audio(a, "multi");
        }
        _ => {
            // Language words and codes, "ENG-SUB", "Sub_Esp", "engsub", "subita".
            let (word, sub_suffix) = match w
                .strip_suffix("subs")
                .or_else(|| w.strip_suffix("sub"))
                .filter(|r| r.len() >= 2 && language(r).is_some())
            {
                Some(r) => (r, true),
                None => match w
                    .strip_prefix("sub")
                    .filter(|r| r.len() >= 2 && language(r).is_some())
                {
                    Some(r) => (r, true),
                    None => (w, false),
                },
            };
            let near = strong_near || format_near;
            let code = language(word).or_else(|| {
                (word.len() == 2
                    && (in_tags || ctx.book || near)
                    && (t.upper() || ((ctx.book || format_near) && word == "fr")))
                    .then(|| two_letter(word))
                    .flatten()
            });
            let Some(code) = code else { return };
            if !(sub_suffix || lang_ok(i) || (word.len() == 2 && (in_tags || near))) {
                return;
            }
            if sub_suffix || sub_word(next) || sub_word(prev) {
                push(&mut a.subtitles, code);
            } else {
                if code == "fr" {
                    french(a, "french");
                }
                audio(a, code);
                if code == "es-419" || code == "pt-br" {
                    audio(a, &code[..2]);
                }
            }
        }
    }
}

/// Order the language list: real languages first (French variants after "fr"), then
/// "original", then "multi".
fn tidy(a: &mut ReleaseAttrs) {
    let rank = |l: &str| match l {
        "multi" => 3,
        "original" => 2,
        _ => 0,
    };
    a.languages
        .sort_by(|x, y| rank(x).cmp(&rank(y)).then_with(|| x.cmp(y)));
    a.subtitles.sort();
    // "DTS HDMA" also matched plain DTS on the way.
    if a.audio.iter().any(|c| c == "dts-hd" || c == "dts-x") {
        a.audio.retain(|c| c != "dts");
    }
    a.audio
        .sort_by_key(|c| AUDIO_CODECS.iter().position(|x| x == c).unwrap_or(99));
    a.hdr
        .sort_by_key(|c| HDR_FORMATS.iter().position(|x| x == c).unwrap_or(99));
}

/// Language codes of a subtitle or audio track file name: `Movie.fr.srt`, `Movie.French.srt`,
/// `Subs/2_English.srt`, `Movie.eng.forced.srt`, `Movie.pt-BR.srt`.
fn file_track_languages(path: &str) -> Vec<&'static str> {
    let stem = stem_of(path);
    let words: Vec<String> = stem
        .split(['.', '_', ' ', '-', '[', ']', '(', ')'])
        .filter(|w| !w.is_empty())
        .map(|w| w.to_lowercase())
        .collect();
    let mut out = Vec::new();
    // The last words carry the language; "forced", "sdh", "cc", "full" may follow it.
    for w in words.iter().rev().take(3) {
        if matches!(
            w.as_str(),
            "forced" | "sdh" | "cc" | "full" | "hi" | "default"
        ) {
            continue;
        }
        if w == "br" {
            continue;
        }
        if let Some(l) = file_language(w) {
            if !out.contains(&l) {
                out.push(l);
            }
            break;
        }
        // Only the trailing words are tags; a word in the title ends the search.
        if words.len() > 1 {
            break;
        }
    }
    // "Movie.pt-BR.srt"
    if words.len() >= 2 && words[words.len() - 1] == "br" && words[words.len() - 2] == "pt" {
        out = vec!["pt-br"];
    }
    out
}

/// Attributes of a torrent from its title and files. `category` turns on anime and book rules.
pub fn parse_release(title: &str, files: &[TorrentFile], category: Category) -> ReleaseAttrs {
    let ctx = Ctx {
        anime: category == Category::Anime,
        book: category == Category::Book,
        music: category == Category::Audio,
        video: matches!(
            category,
            Category::Video | Category::Anime | Category::Porn | Category::Other
        ),
    };
    let title = title.trim();
    let is_hash = title.len() == 40 && title.bytes().all(|b| b.is_ascii_hexdigit());
    let mut a = if is_hash {
        ReleaseAttrs::default()
    } else {
        parse_one(title, &ctx)
    };

    // DHT-style or bare titles: the largest video file usually has the release name.
    let poor = a.resolution.is_none() && a.source.is_none() && a.video_codec.is_none();
    let videos = files
        .iter()
        .filter(|f| ext_of(&f.path).is_some_and(|e| VIDEO_EXT.contains(&e.as_str()) && e != "iso"));
    if poor && let Some(v) = videos.clone().max_by_key(|f| f.size.unwrap_or(0)) {
        let f = parse_one(stem_of(&v.path), &ctx);
        merge_missing(&mut a, f);
    }
    let has_video = videos.clone().next().is_some();

    // Subtitle and audio track files with a language suffix.
    let mut file_subs: Vec<&str> = Vec::new();
    let mut file_audio: Vec<&str> = Vec::new();
    for f in files.iter().take(2000) {
        let Some(ext) = ext_of(&f.path) else { continue };
        if SUB_EXT.contains(&ext.as_str()) {
            for l in file_track_languages(&f.path) {
                if !file_subs.contains(&l) {
                    file_subs.push(l);
                }
            }
        } else if has_video && AUDIO_TRACK_EXT.contains(&ext.as_str()) {
            for l in file_track_languages(&f.path) {
                if !file_audio.contains(&l) {
                    file_audio.push(l);
                }
            }
        }
    }
    for l in file_subs {
        push(&mut a.subtitles, l);
    }
    if a.languages.is_empty() {
        for l in file_audio {
            push(&mut a.languages, l);
        }
    }

    // Music: the file types say the format when the title does not.
    if category == Category::Audio && a.audio.is_empty() {
        let mut counts: Vec<(&str, usize)> = Vec::new();
        for f in files {
            if let Some(ext) = ext_of(&f.path)
                && let Some((_, codec)) = MUSIC_EXT.iter().find(|(e, _)| *e == ext)
                && AUDIO_CODECS.contains(codec)
            {
                match counts.iter_mut().find(|(c, _)| c == codec) {
                    Some((_, n)) => *n += 1,
                    None => counts.push((codec, 1)),
                }
            }
        }
        if let Some((c, _)) = counts.iter().max_by_key(|(_, n)| *n) {
            push(&mut a.audio, c);
        }
    }
    // Books and comics: every book format among the files.
    if matches!(category, Category::Book | Category::Audio | Category::Other)
        || !a.formats.is_empty()
    {
        for f in files.iter().take(2000) {
            if let Some(ext) = ext_of(&f.path)
                && BOOK_EXT.contains(&ext.as_str())
                && (category == Category::Book || ext == "m4b" || ext == "epub")
            {
                push(&mut a.formats, if ext == "azw" { "azw3" } else { &ext });
            }
        }
    }

    // DVD is SD by definition.
    if a.resolution.is_none()
        && ctx.video
        && (a.source.as_deref() == Some("dvd") || dvd_seen(title))
    {
        a.resolution = Some("sd".into());
    }
    // Anime: Japanese audio unless the name names another track (an English or French dub).
    if ctx.anime {
        let explicit = a
            .languages
            .iter()
            .any(|l| !matches!(l.as_str(), "multi" | "original"));
        let vo = a.languages.iter().any(|l| l == "original");
        let multi_only = a
            .languages
            .iter()
            .all(|l| matches!(l.as_str(), "multi" | "fr" | "original"));
        if a.languages.is_empty() || vo || (multi_only && a.languages.iter().any(|l| l == "multi"))
        {
            push(&mut a.languages, "ja");
            if a.languages.iter().any(|l| l == "multi") && !explicit {
                // Dual audio fansubs: Japanese + English.
                push(&mut a.languages, "en");
            }
        }
    }
    // A complete pack of seasons.
    if a.season_end.is_some() {
        a.complete = true;
    }
    tidy(&mut a);
    a
}

/// Fill what the title did not say from a file name's parse.
fn merge_missing(a: &mut ReleaseAttrs, f: ReleaseAttrs) {
    if a.resolution.is_none() {
        a.resolution = f.resolution;
    }
    if a.source.is_none() {
        a.source = f.source;
    }
    if a.video_codec.is_none() {
        a.video_codec = f.video_codec;
    }
    if a.hdr.is_empty() {
        a.hdr = f.hdr;
    }
    if a.bit_depth.is_none() {
        a.bit_depth = f.bit_depth;
    }
    if a.audio.is_empty() {
        a.audio = f.audio;
    }
    if a.channels.is_none() {
        a.channels = f.channels;
    }
    if a.languages.is_empty() {
        a.languages = f.languages;
        a.french = f.french;
    }
    for s in f.subtitles {
        push(&mut a.subtitles, &s);
    }
    if a.group.is_none() {
        a.group = f.group;
    }
    if a.season.is_none() && a.episode.is_none() {
        a.season = f.season;
        a.season_end = f.season_end;
        a.episode = f.episode;
        a.episode_end = f.episode_end;
    }
    if a.year.is_none() {
        a.year = f.year;
    }
    for e in f.editions {
        push(&mut a.editions, &e);
    }
}

impl ReleaseAttrs {
    /// Key of the language group a release belongs to in the per-title view:
    /// "multi-vff", "multi-vfq", "multi-vf2", "multi", "vff", "vfq", "vf2", "vfi", "vof",
    /// "french", "vostfr", "original", an ISO code, or "unknown".
    pub fn language_key(&self) -> String {
        let has = |t: &str| self.french.iter().any(|x| x == t);
        let variant = if has("vf2") || (has("vff") && has("vfq")) {
            Some("vf2")
        } else if has("vff") || has("truefrench") {
            Some("vff")
        } else if has("vfq") {
            Some("vfq")
        } else if has("vfi") {
            Some("vfi")
        } else {
            None
        };
        if has("multi") || self.languages.iter().any(|l| l == "multi") {
            return match variant {
                Some(v) => format!("multi-{v}"),
                None => "multi".into(),
            };
        }
        if let Some(v) = variant {
            return v.into();
        }
        if has("vof") {
            return "vof".into();
        }
        if has("vostfr") || has("subfrench") {
            return "vostfr".into();
        }
        if self.languages.iter().any(|l| l == "fr") {
            return "french".into();
        }
        match self.languages.iter().find(|l| *l != "original") {
            Some(l) => l.split('-').next().unwrap_or(l).to_string(),
            None if self.languages.iter().any(|l| l == "original") => "original".into(),
            None => "unknown".into(),
        }
    }
}

/// Display label of a `language_key` or a language facet value.
pub fn language_label(key: &str) -> String {
    let base = match key {
        "multi-vff" => "MULTi (VFF)",
        "multi-vfq" => "MULTi (VFQ)",
        "multi-vf2" => "MULTi (VF2)",
        "multi-vfi" => "MULTi (VFI)",
        "multi" => "MULTi",
        "vff" => "VFF (France)",
        "vfq" => "VFQ (Québec)",
        "vf2" => "VF2 (France + Québec)",
        "vfi" => "VFI (international French)",
        "vof" => "VOF (French original)",
        "french" | "fr" => "French",
        "fr-fr" => "French (France)",
        "fr-ca" => "French (Québec)",
        "vostfr" => "VOSTFR",
        "original" => "Original (VO)",
        "unknown" => "Unknown",
        "en" => "English",
        "de" => "German",
        "it" => "Italian",
        "es" => "Spanish",
        "es-419" => "Spanish (Latino)",
        "ja" => "Japanese",
        "ko" => "Korean",
        "ru" => "Russian",
        "pt" => "Portuguese",
        "pt-br" => "Portuguese (Brazil)",
        "zh" => "Chinese",
        "nl" => "Dutch",
        "pl" => "Polish",
        "hi" => "Hindi",
        "tr" => "Turkish",
        "ar" => "Arabic",
        "sv" => "Swedish",
        "no" => "Norwegian",
        "da" => "Danish",
        "fi" => "Finnish",
        "cs" => "Czech",
        "hu" => "Hungarian",
        "uk" => "Ukrainian",
        "th" => "Thai",
        "vi" => "Vietnamese",
        "el" => "Greek",
        "he" => "Hebrew",
        "ta" => "Tamil",
        "te" => "Telugu",
        "ro" => "Romanian",
        other => return other.to_uppercase(),
    };
    base.to_string()
}

/// Releases one per-title view shows at most (best seeded first, D44).
pub const MAX_RELEASES: usize = 1000;

/// One group of the per-title release view: one season/episode (series), one resolution, one
/// language key (`ReleaseAttrs::language_key`).
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ReleaseGroup {
    /// Series only. A season pack has `episode` None; a complete-series pack has both None.
    pub season: Option<i32>,
    pub episode: Option<i32>,
    pub resolution: Option<String>,
    pub language: String,
    pub language_label: String,
    /// Most seeders first (unknown last), then newest.
    pub releases: Vec<TorrentSummary>,
}

/// Group the releases of one media (D44). `by_episode`: series/anime, grouped by season and
/// episode first (packs and episode ranges before single episodes).
pub fn group_releases(hits: Vec<TorrentSummary>, by_episode: bool) -> Vec<ReleaseGroup> {
    type Key = (Option<i32>, Option<i32>, usize, String);
    let mut groups: Vec<(Key, ReleaseGroup)> = Vec::new();
    for h in hits {
        let a = h.attrs.clone().unwrap_or_default();
        let (season, episode) = if by_episode {
            let single = a.episode_end.is_none() && !a.complete;
            (a.season, a.episode.filter(|_| single))
        } else {
            (None, None)
        };
        let lang = a.language_key();
        let key: Key = (
            season,
            episode,
            resolution_rank(a.resolution.as_deref()),
            lang.clone(),
        );
        match groups.iter_mut().find(|(k, _)| *k == key) {
            Some((_, g)) => g.releases.push(h),
            None => groups.push((
                key,
                ReleaseGroup {
                    season,
                    episode,
                    resolution: a.resolution.clone(),
                    language_label: language_label(&lang),
                    language: lang,
                    releases: vec![h],
                },
            )),
        }
    }
    for (_, g) in &mut groups {
        g.releases.sort_by(|x, y| {
            y.seeders
                .is_some()
                .cmp(&x.seeders.is_some())
                .then(y.seeders.cmp(&x.seeders))
                .then(y.created_at.cmp(&x.created_at))
        });
    }
    // Seasons and episodes in order (packs first), best resolution first, then the language
    // with the most releases.
    groups.sort_by(|(a, ga), (b, gb)| {
        (a.0, a.1, a.2)
            .cmp(&(b.0, b.1, b.2))
            .then(gb.releases.len().cmp(&ga.releases.len()))
            .then(a.3.cmp(&b.3))
    });
    groups.into_iter().map(|(_, g)| g).collect()
}

/// Display label of a facet value (`facet` as in the facet API: "language", "resolution", …).
pub fn facet_label(facet: &str, value: &str) -> String {
    let fixed = match (facet, value) {
        ("resolution", "4320p") => "4320p (8K)",
        ("resolution", "2160p") => "2160p (4K)",
        ("resolution", "sd") => "SD",
        ("source", "remux") => "Remux",
        ("source", "bluray") => "BluRay",
        ("source", "web-dl") => "WEB-DL",
        ("source", "webrip") => "WEBRip",
        ("source", "hdrip") => "HDRip",
        ("source", "hdtv") => "HDTV",
        ("source", "dvd") => "DVD",
        ("source", "vhs") => "VHS",
        ("source", "screener") => "Screener",
        ("source", "telesync") => "TS/TC",
        ("source", "cam") => "CAM",
        ("codec", "h265") => "H.265/HEVC",
        ("codec", "h264") => "H.264/AVC",
        ("codec", "av1") => "AV1",
        ("codec", "vp9") => "VP9",
        ("codec", "xvid") => "XviD/DivX",
        ("codec", "mpeg2") => "MPEG-2",
        ("codec", "vc1") => "VC-1",
        ("hdr", "dv") => "Dolby Vision",
        ("hdr", "hdr10+") => "HDR10+",
        ("hdr", "hdr10") => "HDR10",
        ("hdr", "hlg") => "HLG",
        ("audio", "truehd") => "TrueHD",
        ("audio", "atmos") => "Atmos",
        ("audio", "dts-x") => "DTS:X",
        ("audio", "dts-hd") => "DTS-HD MA",
        ("audio", "dts") => "DTS",
        ("audio", "eac3") => "DD+ (E-AC-3)",
        ("audio", "ac3") => "DD (AC-3)",
        ("audio", "aac") => "AAC",
        ("audio", "flac") => "FLAC",
        ("audio", "opus") => "Opus",
        ("audio", "mp3") => "MP3",
        ("audio", "pcm") => "PCM",
        ("audio", "alac") => "ALAC",
        ("audio", "vorbis") => "Vorbis",
        ("audio", "dsd") => "DSD",
        ("media_kind", "movie") => "Movie",
        ("media_kind", "series") => "Series",
        ("media_kind", "anime") => "Anime",
        ("subtitles", "multi") => "Multi-subs",
        // Result sources (D51).
        ("origin", "nostr") => "Nostr",
        ("origin", "dht") => "DHT",
        _ => "",
    };
    if !fixed.is_empty() {
        return fixed.to_string();
    }
    match facet {
        "language" | "subtitles" => match value {
            "multi" => "Multi-language".into(),
            v => language_label(v),
        },
        "genre" | "edition" => {
            let mut c = value.chars();
            c.next()
                .map(|f| f.to_uppercase().chain(c).collect())
                .unwrap_or_default()
        }
        "format" => value.to_uppercase(),
        _ => value.to_string(),
    }
}

/// Sort rank of a resolution value (best first; unknown last).
pub fn resolution_rank(r: Option<&str>) -> usize {
    r.and_then(|r| RESOLUTIONS.iter().position(|x| *x == r))
        .unwrap_or(RESOLUTIONS.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(name: &str) -> ReleaseAttrs {
        parse_release(name, &[], Category::Video)
    }

    fn langs(a: &ReleaseAttrs) -> Vec<&str> {
        a.languages.iter().map(String::as_str).collect()
    }

    #[test]
    fn french_scene_tags() {
        let a = p("Haunted.Mansion.2023.MULTi.VF2.1080p.WEB.H264-FW (Le manoir hanté)");
        assert_eq!(langs(&a), ["fr", "fr-ca", "fr-fr", "original", "multi"]);
        assert_eq!(a.language_key(), "multi-vf2");
        assert_eq!(a.group.as_deref(), Some("FW"));
        assert_eq!(a.year, Some(2023));
        let a = p("Shook.2024.VFQ.720p.WEB-DL.H264-Slay3R (Choc)");
        assert_eq!(langs(&a), ["fr", "fr-ca"]);
        assert_eq!(a.resolution.as_deref(), Some("720p"));
        assert_eq!(a.source.as_deref(), Some("web-dl"));
        assert_eq!(a.group.as_deref(), Some("Slay3R"));
        let a = p("White.Men.Cant.Jump.2023.TRUEFRENCH.720p.DSNP.WEB-DL.DDP5.1.H.264-FCK");
        assert_eq!(langs(&a), ["fr", "fr-fr"]);
        assert_eq!(a.audio, ["eac3"]);
        assert_eq!(a.channels.as_deref(), Some("5.1"));
        assert_eq!(a.video_codec.as_deref(), Some("h264"));
        let a = p("Captain.Tsubasa.(2018).S02E07.VOSTFR.1080p.WEBRip.HEVC.AAC-ShC23");
        assert_eq!(langs(&a), ["original"]);
        assert_eq!(a.subtitles, ["fr"]);
        assert_eq!((a.season, a.episode), (Some(2), Some(7)));
        assert_eq!(a.language_key(), "vostfr");
        let a = p("Piggy.2022.SUBFRENCH.FANSUB.1080p.BluRay.DTS.x264-NOGRP");
        assert_eq!(a.subtitles, ["fr"]);
        assert_eq!(a.group, None);
        assert!(a.editions.contains(&"fansub".to_string()));
        let a = p(
            "La Dame de Shanghai (1947) MULTi VFI 2160p 10bit 4KLight DV HDR BluRay DTS-HD MA 1.0 x265-QTZ (The Lady from Shanghai)",
        );
        assert_eq!(a.language_key(), "multi-vfi");
        assert_eq!(a.hdr, ["dv", "hdr10"]);
        assert_eq!(a.audio, ["dts-hd"]);
        assert_eq!(a.channels.as_deref(), Some("1.0"));
        assert_eq!(a.bit_depth, Some(10));
        assert!(a.editions.contains(&"light".to_string()));
        let a = p("Ouistreham.2021.VOFSTFR.FR3.HDTV.1080p.H264.AAC-LC");
        assert_eq!(langs(&a), ["fr", "original"]);
        assert_eq!(a.subtitles, ["fr"]);
        assert_eq!(a.source.as_deref(), Some("hdtv"));
        let a = p("Peter.Macleod.Sagesse.Reportee.2014.French.VQ.1080p.AMZN.WEB-DL.H264-NoTag");
        assert_eq!(langs(&a), ["fr", "fr-ca"]);
        let a = p(
            "Olympique.Lyonnais.Inside.S02E01.De.La.Relegation.Aux.Premiers.Succes.VOF.1080p.WEB.H264",
        );
        assert_eq!(a.language_key(), "vof");
        assert!(!a.languages.contains(&"de".to_string()), "{a:?}");
    }

    #[test]
    fn title_words_are_not_languages() {
        let a = p("French.Kiss.1995.1080p.BluRay.x264");
        assert!(a.languages.is_empty(), "{a:?}");
        let a = p("The.Italian.Job.2003.1080p.BluRay.x264-GRP");
        assert!(a.languages.is_empty(), "{a:?}");
        let a = p("South.Park.S27E03.The.English.Patient.1080p.WEB.h264-ETHEL");
        assert!(a.languages.is_empty(), "{a:?}");
        let a =
            p("Rugby à XIII.NRL.Grand Final.2017.Storm vs Cowboys.French.HDTV.1080p.h264-Freek911");
        assert_eq!(langs(&a), ["fr"]);
        let a = p("Spider-Man.No.Way.Home");
        assert_eq!(a.group, None);
    }

    #[test]
    fn anime_fansubs() {
        let an = |n: &str| parse_release(n, &[], Category::Anime);
        let a = an("[SubsPlease] Toumei Otoko to Ningen Onna - 07 (720p) [4874FF4E].mkv");
        assert_eq!(a.group.as_deref(), Some("SubsPlease"));
        assert_eq!(a.episode, Some(7));
        assert_eq!(a.resolution.as_deref(), Some("720p"));
        assert_eq!(langs(&a), ["ja"]);
        let a = an(
            "[Judas] Gekijouban Boku no Kokoro no Yabai Yatsu [1080p][HEVC x265 10bit][Dual-Audio][Multi-Subs] (Movie)",
        );
        assert_eq!(langs(&a), ["en", "ja", "multi"]);
        assert_eq!(a.subtitles, ["multi"]);
        let a = an(
            "[Yameii] KILL BLUE - S01E06 [English Dub] [CR WEB-DL 720p H264 AAC] [283F9850] (Kill Ao)",
        );
        assert_eq!(langs(&a), ["en"]);
        let a = an("[Erai-raws] One Piece - 1152 [1080p CR WEB-DL AVC AAC][MultiSub][29B99D93]");
        assert_eq!(a.episode, Some(1152));
        assert_eq!(a.subtitles, ["multi"]);
        assert_eq!(a.group.as_deref(), Some("Erai-raws"));
        let a = an("[ANi]  小書痴的下剋上 - 04 [1080P][Baha][WEB-DL][AAC AVC][CHT][MP4]");
        assert_eq!(a.subtitles, ["zh"]);
        assert_eq!(a.episode, Some(4));
        let a = an(
            "The Angel Next Door Spoils Me Rotten S02E04 VOSTFR 1080p WEB x264 AAC -Tsundere-Raws (CR)",
        );
        assert_eq!(a.group.as_deref(), Some("Tsundere-Raws"));
        assert_eq!(langs(&a), ["ja", "original"]);
        let a = an("[SubsPlease] Gintama - 3-nen Z-gumi Ginpachi-sensei (01-12) (480p) [Batch]");
        assert_eq!((a.episode, a.episode_end), (Some(1), Some(12)));
        assert!(a.complete);
    }

    #[test]
    fn audio_and_video_tags() {
        let a = p("Big Mistakes S01E08 Rock Paper Scissors 2160p NF WEB-DL DDP5 1 Atmos DV HDR");
        assert_eq!(a.audio, ["atmos", "eac3"]);
        assert_eq!(a.channels.as_deref(), Some("5.1"));
        assert_eq!(a.hdr, ["dv", "hdr10"]);
        assert_eq!(a.resolution.as_deref(), Some("2160p"));
        let a = p("13 Minutes 2021 MULTi 1080p Bluray Remux AVC DTS HDMA-KPX");
        assert_eq!(a.source.as_deref(), Some("remux"));
        assert_eq!(a.audio, ["dts-hd"]);
        assert_eq!(a.group.as_deref(), Some("KPX"));
        let a = p("Lord.of.Mysteries.S01E01.MULTI.816p.CR.WEB.x264-TenmaLand");
        assert_eq!(a.resolution.as_deref(), Some("1080p"));
        let a = p("The Night Manager S02E04 2160p AMZN WEB-DL DD 5 1 H 265-playWEB");
        assert_eq!(a.audio, ["ac3"]);
        assert_eq!(a.channels.as_deref(), Some("5.1"));
        assert_eq!(a.video_codec.as_deref(), Some("h265"));
        assert_eq!(a.group.as_deref(), Some("playWEB"));
        let a = p("Top.Gun.Maverick.2022.VO.VFF.DTSHDMA.4K.UHD.HDR10.H265.DoVi.HDR10PLUS");
        assert_eq!(a.hdr, ["dv", "hdr10+", "hdr10"]);
        assert_eq!(a.resolution.as_deref(), Some("2160p"));
        assert_eq!(a.audio, ["dts-hd"]);
    }

    #[test]
    fn years_and_seasons() {
        assert_eq!(p("Blade.Runner.2049.2017.1080p").year, Some(2017));
        assert_eq!(p("1917.2019.1080p").year, Some(2019));
        assert_eq!(
            p("Breaking Bad Season 1-5 2008-2013 1080p").year,
            Some(2008)
        );
        let a = p("Breaking Bad Season 1-5 2008-2013 1080p");
        assert_eq!((a.season, a.season_end), (Some(1), Some(5)));
        assert!(a.complete);
        let a = p("Buffy.The.Vampire.Slayer.S04.MULTi.1080p.WEB.H264-NERO");
        assert_eq!((a.season, a.episode), (Some(4), None));
        let a = p("Friends.S01-S10.COMPLETE.1080p");
        assert_eq!((a.season, a.season_end), (Some(1), Some(10)));
        let a = p("Show.S01E01-E03.720p");
        assert_eq!((a.episode, a.episode_end), (Some(1), Some(3)));
    }

    #[test]
    fn music_and_books() {
        let m = |n: &str| parse_release(n, &[], Category::Audio);
        let a = m("Julie Fuchs - Amadè - 2022  [Flac 24 Bits 96 kHz]");
        assert_eq!(a.audio, ["flac"]);
        assert_eq!(a.bit_depth, Some(24));
        assert_eq!(a.sample_rate.as_deref(), Some("96"));
        let a = m("U2 - Please PopHeart Live - 1997 - CDrip [Flac 16 Bits 44,1 kHz]");
        assert_eq!(a.sample_rate.as_deref(), Some("44.1"));
        let a = m("Michael Bublé - The Best of Buble (Sing-a-Long Edition).2025.MP3.320KBPS-G11");
        assert_eq!(a.bitrate.as_deref(), Some("320"));
        assert_eq!(a.audio, ["mp3"]);
        let a = m("Ruth Ware - L'heure tourne [2025] [mp3 à 128 Kb/s]");
        assert_eq!(a.bitrate.as_deref(), Some("128"));
        let b = |n: &str| parse_release(n, &[], Category::Book);
        let a = b("L'Ultime.Secret.de.la.glace.bleue.James.Rollins.2024.fr.epub-G11");
        assert_eq!(a.formats, ["epub"]);
        assert_eq!(langs(&a), ["fr"]);
        let a = b("MDF SD N°4: The Hawker Sea Fury Royal Navy & Export Versions [PDF] EN");
        assert_eq!(a.formats, ["pdf"]);
        assert_eq!(langs(&a), ["en"]);
    }

    #[test]
    fn files_fill_poor_titles() {
        let f = |p: &str, s: u64| TorrentFile {
            path: p.into(),
            size: Some(s),
        };
        let files = [
            f("Movie/The.Matrix.1999.1080p.BluRay.x264-GRP.mkv", 9_000),
            f("Movie/Subs/2_English.srt", 10),
            f("Movie/The.Matrix.1999.fr.srt", 10),
            f("Movie/sample.mkv", 10),
        ];
        let a = parse_release("Movie", &files, Category::Video);
        assert_eq!(a.resolution.as_deref(), Some("1080p"));
        assert_eq!(a.source.as_deref(), Some("bluray"));
        assert_eq!(a.year, Some(1999));
        assert_eq!(a.subtitles, ["en", "fr"]);
        let files = [
            f("a/01 Song.flac", 1),
            f("a/02 Song.flac", 1),
            f("a/cover.jpg", 1),
        ];
        assert_eq!(
            parse_release("Some Album", &files, Category::Audio).audio,
            ["flac"]
        );
    }

    #[test]
    fn serde_round_trip_skips_empty() {
        let a = p("Fargo.S05E09.MULTi.1080p.WEB.x264-FW");
        let j = serde_json::to_string(&a).unwrap();
        assert!(!j.contains("hdr"), "{j}");
        let back: ReleaseAttrs = serde_json::from_str(&j).unwrap();
        assert_eq!(back, a);
        assert_eq!(
            serde_json::from_str::<ReleaseAttrs>("{}").unwrap(),
            ReleaseAttrs::default()
        );
    }
}

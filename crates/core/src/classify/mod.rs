//! Category and adult tagging from a torrent's name and file list (ARCHITECTURE.md D52, D69;
//! docs/DHT.md §6). Pure, no dependencies, no feature: DHT torrents are classified with it in
//! the server, and Nostr ingest uses its adult signal for the blocklist's context tier.
//!
//! **Nothing here logs** (D53: classification runs before the blocklist decision, on names
//! that may be about to be blocked).
//!
//! Order:
//! 1. bytes per extension class (video, audio, book, application, game image, picture, disc
//!    image, archive, other); a class holding more than half of the known bytes wins;
//! 2. **adult** ([`adult_signal`] on the name and every path, with video + picture bytes at
//!    least half of the total, or no sizes at all) → [`Category::Porn`];
//! 3. **game**: a console image majority, a repack/scene game tag in the name, or an
//!    installer set (`setup.exe` with `.bin`/`.cab` data parts) → [`Category::Game`];
//! 4. the winning class; video with the fansub shape `[Group] Title - 05 [1080p]` →
//!    [`Category::Anime`] (the server confirms more cases with anitomy-ng);
//! 5. no winner, a disc image or an archive majority: hints in the name (release attributes
//!    of `core::release`, software/audio/book words).

use std::collections::HashSet;
use std::sync::OnceLock;

use crate::blocklist::{ngrams, normalize};
use crate::model::{Category, TorrentFile};
use crate::release;

/// Stored per DHT row (`dht_torrents.classifier_version`): rows below it are re-classified.
/// 0 = stored before any classifier (stage 2).
pub const CLASSIFIER_VERSION: i16 = 1;

const ADULT_LIST: &str = include_str!("adult.txt");

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Class {
    Video,
    Audio,
    Book,
    Application,
    /// Console/emulator images (nsp, xci, wbfs, …).
    GameImage,
    Picture,
    /// iso, img, bin/cue, …: video discs, games and operating systems alike.
    Disc,
    Archive,
    Other,
}

const CLASSES: [Class; 9] = [
    Class::Video,
    Class::Audio,
    Class::Book,
    Class::Application,
    Class::GameImage,
    Class::Picture,
    Class::Disc,
    Class::Archive,
    Class::Other,
];

/// Extension (lowercase, without the dot) of a path's last component, if it has a short one.
pub fn extension(path: &str) -> Option<String> {
    let file = path.rsplit(['/', '\\']).next().unwrap_or(path);
    let (stem, ext) = file.rsplit_once('.')?;
    if stem.is_empty() || ext.is_empty() || ext.len() > 8 {
        return None;
    }
    Some(ext.to_ascii_lowercase())
}

pub fn class_of(ext: &str) -> Class {
    match ext {
        "mkv" | "mp4" | "avi" | "m4v" | "ts" | "m2ts" | "mts" | "wmv" | "mov" | "mpg" | "mpeg"
        | "webm" | "flv" | "vob" | "divx" | "ogm" | "ogv" | "rmvb" | "rm" | "3gp" | "f4v"
        | "asf" | "m2v" | "evo" => Class::Video,
        "mp3" | "flac" | "wav" | "aac" | "ogg" | "oga" | "opus" | "m4a" | "m4b" | "ape"
        | "alac" | "wma" | "dsf" | "dff" | "wv" | "aiff" | "aif" | "mka" | "mpc" | "tta"
        | "ac3" | "dts" | "mid" | "midi" => Class::Audio,
        "epub" | "pdf" | "mobi" | "azw" | "azw3" | "azw4" | "kfx" | "djvu" | "djv" | "fb2"
        | "cbz" | "cbr" | "cb7" | "cbt" | "chm" | "lit" | "doc" | "docx" | "rtf" | "odt"
        | "pdb" => Class::Book,
        "exe" | "msi" | "dmg" | "pkg" | "apk" | "xapk" | "apks" | "deb" | "rpm" | "appimage"
        | "jar" | "msix" | "msixbundle" | "appx" | "ipa" | "run" | "dll" | "flatpak" | "snap"
        | "vsix" => Class::Application,
        "nsp" | "nsz" | "xci" | "xcz" | "wbfs" | "rvz" | "wua" | "gcm" | "cia" | "3ds" | "nds"
        | "gba" | "gbc" | "gb" | "sfc" | "smc" | "n64" | "z64" | "v64" | "nes" | "vpk" | "pbp"
        | "cso" | "chd" | "xex" => Class::GameImage,
        "jpg" | "jpeg" | "png" | "gif" | "bmp" | "webp" | "heic" | "heif" | "tif" | "tiff"
        | "cr2" | "cr3" | "nef" | "arw" | "dng" | "orf" | "rw2" | "raf" | "psd" | "avif"
        | "jxl" => Class::Picture,
        "iso" | "img" | "bin" | "cue" | "mdf" | "mds" | "nrg" | "ccd" | "cdi" | "vhd" | "vhdx"
        | "vmdk" | "ova" | "qcow2" | "wim" | "esd" => Class::Disc,
        "zip" | "rar" | "7z" | "tar" | "gz" | "tgz" | "xz" | "bz2" | "zst" | "lz" | "lzma"
        | "cab" | "arj" | "lzh" | "sitx" => Class::Archive,
        e if e.len() == 3 && e.starts_with('r') && e[1..].bytes().all(|b| b.is_ascii_digit()) => {
            Class::Archive // rar volumes .r00 … .r99
        }
        e if e.len() == 3 && e.bytes().all(|b| b.is_ascii_digit()) => Class::Archive, // .001
        _ => Class::Other,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Classification {
    pub category: Category,
    /// Adult words/studios/codes in the name or paths, whatever the file types: the
    /// blocklist's context tier uses this (wider than `category == Porn`, D53).
    pub adult_signal: bool,
    /// Which step decided, for stats and tests (never the text).
    pub rule: Rule,
}

impl Classification {
    /// Adult for the D52 filter and the blocklist's context tier.
    pub fn adult(&self) -> bool {
        self.category == Category::Porn || self.adult_signal
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Rule {
    Adult,
    GameTag,
    GameFiles,
    Anime,
    Bytes,
    NameHint,
    Unknown,
}

impl Rule {
    pub fn as_str(self) -> &'static str {
        match self {
            Rule::Adult => "adult",
            Rule::GameTag => "game_tag",
            Rule::GameFiles => "game_files",
            Rule::Anime => "anime",
            Rule::Bytes => "bytes",
            Rule::NameHint => "name_hint",
            Rule::Unknown => "unknown",
        }
    }
}

struct AdultList {
    strong: HashSet<String>,
    weak: HashSet<String>,
    prefixes: Vec<String>,
}

fn adult_list() -> &'static AdultList {
    static L: OnceLock<AdultList> = OnceLock::new();
    L.get_or_init(|| {
        let mut l = AdultList {
            strong: HashSet::new(),
            weak: HashSet::new(),
            prefixes: Vec::new(),
        };
        for line in ADULT_LIST.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((kind, phrase)) = line.split_once(' ') else {
                continue;
            };
            let toks = normalize(phrase).swap_remove(0).join(" ");
            match kind {
                "strong" => {
                    l.strong.insert(toks);
                }
                "weak" => {
                    l.weak.insert(toks);
                }
                "prefix" => l.prefixes.push(toks),
                _ => {}
            }
        }
        l
    })
}

/// JAV studio labels: `LABEL-123` / `LABEL123` as two or one token(s), a strong adult signal.
const JAV_LABELS: &[&str] = &[
    "ssis", "ssni", "snis", "ipx", "ipz", "ipzz", "midv", "mide", "miaa", "mird", "abp", "abw",
    "abf", "stars", "sdde", "sdmu", "sdab", "pred", "jul", "juy", "jufe", "meyd", "cawd", "kawd",
    "pppd", "hnd", "dasd", "dass", "waaa", "adn", "ebod", "mxgs", "soe", "rbd", "nhdta", "dvdes",
    "hunta", "gvh", "docp", "ure", "atid", "shkd", "mudr", "eyan", "hmn", "nsps", "venx", "vec",
    "fsdss", "sone", "start", "achj", "roe", "mkmp", "ofje", "huntb", "nacr", "jufd", "mvsd",
    "cjod", "dldss", "fset", "nnpj", "sqte", "tppn", "xvsr", "aukg", "bban", "gvg", "hmpd", "kire",
];

fn jav_code(toks: &[String]) -> bool {
    let digits = |t: &str| (3..=5).contains(&t.len()) && t.bytes().all(|b| b.is_ascii_digit());
    for (i, t) in toks.iter().enumerate() {
        if JAV_LABELS.contains(&t.as_str()) && toks.get(i + 1).is_some_and(|n| digits(n)) {
            return true;
        }
        // Glued `ssis123`.
        let alpha: String = t.chars().take_while(|c| c.is_ascii_lowercase()).collect();
        if !alpha.is_empty()
            && alpha.len() < t.len()
            && JAV_LABELS.contains(&alpha.as_str())
            && digits(&t[alpha.len()..])
        {
            return true;
        }
    }
    false
}

/// A path without its file extension (so `Movie.ass` subtitles do not read as a word).
fn strip_ext(path: &str) -> &str {
    match extension(path) {
        Some(e) => &path[..path.len() - e.len() - 1],
        None => path,
    }
}

/// Adult words, studios or release codes in `texts`: one strong signal, or two distinct weak
/// ones. `xxx` as the very first word of the name does not count (the film *xXx*).
pub fn adult_signal<S: AsRef<str>>(name: &str, paths: &[S]) -> bool {
    let l = adult_list();
    let mut weak: HashSet<String> = HashSet::new();
    let mut texts: Vec<&str> = vec![name];
    texts.extend(paths.iter().map(|p| strip_ext(p.as_ref())));
    let mut seen: HashSet<String> = HashSet::new();
    for (k, text) in texts.iter().enumerate() {
        for toks in normalize(text) {
            if jav_code(&toks) {
                return true;
            }
            for t in &toks {
                if t.len() >= 4 && l.prefixes.iter().any(|p| t.starts_with(p.as_str())) {
                    return true;
                }
            }
            for g in ngrams(&toks) {
                if !seen.insert(g.clone()) {
                    continue;
                }
                if l.strong.contains(&g) {
                    let leading_xxx =
                        k == 0 && g == "xxx" && toks.first().map(String::as_str) == Some("xxx");
                    if !leading_xxx {
                        return true;
                    }
                }
                if l.weak.contains(&g) {
                    weak.insert(g);
                    if weak.len() >= 2 {
                        return true;
                    }
                }
            }
        }
    }
    false
}

/// Repack groups and scene groups that release games (whole words).
const GAME_TAGS: &[&str] = &[
    "fitgirl",
    "dodi",
    "elamigos",
    "gog",
    "codex",
    "plaza",
    "skidrow",
    "reloaded",
    "empress",
    "tenoke",
    "rune",
    "flt",
    "darksiders",
    "razor1911",
    "cpy",
    "hoodlum",
    "prophet",
    "xatab",
    "kaos",
    "chronos",
    "tinyiso",
    "simplex",
    "doge",
    "goldberg",
    "steamrip",
    "kaoskrew",
    "rg mechanics",
    "mechanics",
    "r g mechanics",
    "decepticon",
    "catalyst",
    "igg",
    "ali213",
    "3dm",
    "dinobytes",
    "anadius",
    "voksi",
    "steampunks",
    "i know",
    "masquerade",
    "fckdrm",
    "nsw",
    "switch nsp",
    "ps4",
    "ps3",
    "psp",
    "ps vita",
    "xbox360",
    "wii",
    "wiiu",
    "3ds",
];

/// Words of software releases.
const APP_HINTS: &[&str] = &[
    "x64",
    "x86",
    "win64",
    "win32",
    "windows",
    "macos",
    "linux",
    "portable",
    "setup",
    "installer",
    "keygen",
    "patch",
    "activator",
    "multilingual",
    "repack",
    "plugin",
    "plugins",
    "vst",
    "vst3",
    "adobe",
    "autodesk",
    "microsoft office",
    "office",
    "ubuntu",
    "debian",
    "fedora",
    "archlinux",
    "arch linux",
    "mint",
    "kali",
    "android",
    "apk",
    "build",
    "amd64",
    "arm64",
    "aarch64",
    "iso",
    "server",
];

const AUDIO_HINTS: &[&str] = &[
    "flac",
    "mp3",
    "320kbps",
    "320",
    "kbps",
    "discography",
    "album",
    "albums",
    "lossless",
    "24bit",
    "16bit",
    "vinyl",
    "lp",
    "ep",
    "ost",
    "soundtrack",
    "audiobook",
    "hi res",
    "web flac",
    "aac",
    "alac",
    "dsd",
];

const BOOK_HINTS: &[&str] = &[
    "epub",
    "pdf",
    "mobi",
    "azw3",
    "ebook",
    "ebooks",
    "comics",
    "comic",
    "manga",
    "magazine",
    "magazines",
    "cbr",
    "cbz",
];

fn has_any(grams: &HashSet<String>, words: &[&str]) -> bool {
    words.iter().any(|w| grams.contains(*w))
}

fn grams_of(name: &str) -> HashSet<String> {
    let mut s = HashSet::new();
    for toks in normalize(name) {
        s.extend(ngrams(&toks));
    }
    s
}

/// The fansub shape: `[Group] Title - 05 …`, `[Group] Title [CRC32]`, `[Group] Title (Batch)`.
pub fn fansub_shape(name: &str) -> bool {
    let n = name.trim();
    let Some(rest) = n.strip_prefix('[') else {
        return false;
    };
    let Some(close) = rest.find(']') else {
        return false;
    };
    if close == 0 || close > 40 || !fansub_group(&rest[..close]) {
        return false;
    }
    let after = &rest[close + 1..];
    // ` - 05`, ` - 1080`(no), `- 12v2`, `E05`
    let bytes = after.as_bytes();
    let mut episode = false;
    let mut i = 0;
    while i + 3 < bytes.len() {
        if bytes[i] == b' ' && bytes[i + 1] == b'-' && bytes[i + 2] == b' ' {
            let digits = bytes[i + 3..]
                .iter()
                .take_while(|b| b.is_ascii_digit())
                .count();
            if (1..=4).contains(&digits) {
                let num: u32 = after[i + 3..i + 3 + digits].parse().unwrap_or(0);
                // A resolution or a year is not an episode.
                if !(num == 480
                    || num == 720
                    || num == 1080
                    || num == 2160
                    || (1900..2100).contains(&num))
                {
                    episode = true;
                    break;
                }
            }
        }
        i += 1;
    }
    let crc = after.split(['[', ']']).any(|p| {
        p.len() == 8
            && p.bytes().all(|b| b.is_ascii_hexdigit())
            && p.bytes()
                .any(|b| b.is_ascii_alphabetic() || b.is_ascii_digit())
            && !p.bytes().all(|b| b.is_ascii_digit())
    });
    let lower = after.to_lowercase();
    let batch = lower.contains("batch") || lower.contains("complete");
    episode || crc || (batch && lower.contains('['))
}

/// Western sites and taggers also put a bracket first (`[ www.site.org ]`, `[TGx]`): those
/// are not fansub groups.
pub fn fansub_group(g: &str) -> bool {
    let g = g.trim().to_lowercase();
    !g.is_empty()
        && !g.contains('.')
        && !g.contains("www")
        && !matches!(
            g.as_str(),
            "tgx"
                | "eztv"
                | "ettv"
                | "rarbg"
                | "yts"
                | "yify"
                | "1337x"
                | "rartv"
                | "mp4"
                | "mkv"
                | "hd"
                | "4k"
                | "1080p"
                | "720p"
                | "2160p"
                | "req"
                | "request"
        )
}

/// Known anime release groups (first bracket).
const ANIME_GROUPS: &[&str] = &[
    "subsplease",
    "erai-raws",
    "horriblesubs",
    "ember",
    "judas",
    "asw",
    "tsundere-raws",
    "commie",
    "gjm",
    "coalgirls",
    "beatrice-raws",
    "anime time",
    "yameii",
    "dkb",
    "neoborn",
    "sallysubs",
    "ohys-raws",
    "leopard-raws",
    "nc-raws",
    "lilith-raws",
    "sakurato",
    "doki",
    "underwater",
    "vcb-studio",
    "moozzi2",
    "reinforce",
    "kametsu",
    "anime land",
    "toonshub",
    "varyg",
    "smol",
    "kawaiika-raws",
    "lostyears",
    "a-l",
    "seiya",
    "kaleido-subs",
    "mtbb",
    "cerberus",
    "nyanpasu",
    "golumpa",
    "animerg",
    "akatsuki",
    "bonkai77",
    "breeze",
    "hi10",
];

fn anime_group(name: &str) -> bool {
    let n = name.trim();
    n.strip_prefix('[')
        .and_then(|r| r.split_once(']'))
        .is_some_and(|(g, _)| ANIME_GROUPS.contains(&g.trim().to_lowercase().as_str()))
}

fn installer_set(files: &[TorrentFile]) -> bool {
    let mut setup = false;
    let mut parts = 0;
    for f in files {
        let file = f
            .path
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or(&f.path)
            .to_ascii_lowercase();
        if file == "setup.exe" || file == "install.exe" {
            setup = true;
        }
        if (file.ends_with(".bin")
            && (file.starts_with("fg-") || file.starts_with("data") || file.starts_with("setup-")))
            || (file.ends_with(".cab") && file.starts_with("data"))
            || file.ends_with(".doi")
        {
            parts += 1;
        }
    }
    setup && parts > 0
}

/// Classify a torrent by its display name and files (padding files already dropped).
pub fn classify(name: &str, files: &[TorrentFile]) -> Classification {
    let mut bytes = [0u128; CLASSES.len()];
    let sized = files.iter().any(|f| f.size.is_some_and(|s| s > 0));
    for f in files {
        let w = if sized {
            u128::from(f.size.unwrap_or(0))
        } else {
            1
        };
        let c = extension(&f.path).map_or(Class::Other, |e| class_of(&e));
        let i = CLASSES
            .iter()
            .position(|x| *x == c)
            .unwrap_or(CLASSES.len() - 1);
        bytes[i] += w;
    }
    // A single-file torrent's name carries its extension.
    if files.is_empty()
        && let Some(e) = extension(name)
    {
        let i = CLASSES.iter().position(|x| *x == class_of(&e)).unwrap_or(8);
        bytes[i] += 1;
    }
    let total: u128 = bytes.iter().sum();
    let share = |c: Class| -> u128 { bytes[CLASSES.iter().position(|x| *x == c).unwrap_or(8)] };
    let winner = (total > 0)
        .then(|| CLASSES.iter().copied().find(|c| share(*c) * 2 > total))
        .flatten();

    let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
    let adult_signal = adult_signal(name, &paths);
    let visual = share(Class::Video) + share(Class::Picture);
    let mk = |category, rule| Classification {
        category,
        adult_signal,
        rule,
    };
    if adult_signal && (total == 0 || visual * 2 >= total) {
        return mk(Category::Porn, Rule::Adult);
    }

    let grams = grams_of(name);
    if winner == Some(Class::GameImage) {
        return mk(Category::Game, Rule::GameFiles);
    }
    let not_media = !matches!(
        winner,
        Some(Class::Video | Class::Audio | Class::Book | Class::Picture)
    );
    if not_media && has_any(&grams, GAME_TAGS) {
        return mk(Category::Game, Rule::GameTag);
    }
    if not_media && installer_set(files) {
        return mk(Category::Game, Rule::GameFiles);
    }
    match winner {
        Some(Class::Video) => {
            if fansub_shape(name) || anime_group(name) {
                return mk(Category::Anime, Rule::Anime);
            }
            return mk(Category::Video, Rule::Bytes);
        }
        Some(Class::Audio) => return mk(Category::Audio, Rule::Bytes),
        Some(Class::Book) => return mk(Category::Book, Rule::Bytes),
        Some(Class::Application) => return mk(Category::Application, Rule::Bytes),
        Some(Class::Picture) => return mk(Category::Picture, Rule::Bytes),
        _ => {}
    }

    // No media majority: what the name says.
    let attrs = release::parse_release(name, files, Category::Other);
    let video_name = attrs.resolution.is_some()
        || attrs.video_codec.is_some()
        || (attrs.source.is_some() && attrs.source.as_deref() != Some("web"))
        || attrs.episode.is_some()
        || attrs.season.is_some();
    if video_name && !has_any(&grams, APP_HINTS) {
        if fansub_shape(name) || anime_group(name) {
            return mk(Category::Anime, Rule::Anime);
        }
        return mk(Category::Video, Rule::NameHint);
    }
    if has_any(&grams, AUDIO_HINTS) && share(Class::Audio) > 0 {
        return mk(Category::Audio, Rule::NameHint);
    }
    if has_any(&grams, BOOK_HINTS) {
        return mk(Category::Book, Rule::NameHint);
    }
    if has_any(&grams, APP_HINTS) || winner == Some(Class::Disc) && share(Class::Application) > 0 {
        return mk(Category::Application, Rule::NameHint);
    }
    if video_name {
        return mk(Category::Video, Rule::NameHint);
    }
    if has_any(&grams, AUDIO_HINTS) {
        return mk(Category::Audio, Rule::NameHint);
    }
    // Largest known media class, even without a majority (≥ 25 %).
    let best = [
        (Class::Video, Category::Video),
        (Class::Audio, Category::Audio),
        (Class::Book, Category::Book),
        (Class::Application, Category::Application),
        (Class::Picture, Category::Picture),
    ]
    .into_iter()
    .max_by_key(|(c, _)| share(*c))
    .filter(|(c, _)| share(*c) * 4 >= total && share(*c) > 0);
    if let Some((_, cat)) = best {
        return mk(cat, Rule::Bytes);
    }
    mk(Category::Other, Rule::Unknown)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(path: &str, size: u64) -> TorrentFile {
        TorrentFile {
            path: path.into(),
            size: Some(size),
        }
    }
    const MB: u64 = 1 << 20;

    fn cat(name: &str, files: &[TorrentFile]) -> Category {
        classify(name, files).category
    }

    #[test]
    fn extension_classes() {
        assert_eq!(extension("a/b/Movie.2024.MKV").as_deref(), Some("mkv"));
        assert_eq!(extension(".hidden"), None);
        assert_eq!(extension("noext"), None);
        assert_eq!(class_of("r07"), Class::Archive);
        assert_eq!(class_of("001"), Class::Archive);
        assert_eq!(class_of("nsp"), Class::GameImage);
        assert_eq!(class_of("iso"), Class::Disc);
    }

    #[test]
    fn categories_from_bytes() {
        assert_eq!(
            cat(
                "Some.Movie.2023.1080p.BluRay.x264-GRP",
                &[
                    f("Some.Movie.2023.1080p.BluRay.x264-GRP.mkv", 8000 * MB),
                    f("x.nfo", 1)
                ]
            ),
            Category::Video
        );
        assert_eq!(
            cat(
                "Artist - Album (2020) [FLAC]",
                &[
                    f("01.flac", 30 * MB),
                    f("02.flac", 30 * MB),
                    f("cover.jpg", MB)
                ]
            ),
            Category::Audio
        );
        assert_eq!(
            cat("Some Book", &[f("Some Book.epub", 2 * MB)]),
            Category::Book
        );
        assert_eq!(
            cat(
                "Tool 3.2",
                &[f("tool-3.2-setup.msi", 80 * MB), f("readme.txt", 1)]
            ),
            Category::Application
        );
        assert_eq!(
            cat("Holiday photos", &[f("a.jpg", 5 * MB), f("b.cr3", 25 * MB)]),
            Category::Picture
        );
        assert_eq!(cat("x", &[f("x.dat", MB)]), Category::Other);
    }

    #[test]
    fn single_file_and_no_sizes() {
        assert_eq!(cat("Song.mp3", &[]), Category::Audio);
        let nosize = |p: &str| TorrentFile {
            path: p.into(),
            size: None,
        };
        assert_eq!(
            cat(
                "Show",
                &[nosize("e1.mkv"), nosize("e2.mkv"), nosize("e1.srt")]
            ),
            Category::Video
        );
    }

    #[test]
    fn games() {
        assert_eq!(
            cat(
                "Some Game [FitGirl Repack]",
                &[f("setup.exe", 5 * MB), f("fg-01.bin", 4000 * MB)]
            ),
            Category::Game
        );
        assert_eq!(
            cat("Some.Game-TENOKE", &[f("tenoke-some.game.iso", 9000 * MB)]),
            Category::Game
        );
        assert_eq!(
            cat(
                "Some Game",
                &[f("setup.exe", 5 * MB), f("data1.bin", 900 * MB)]
            ),
            Category::Game
        );
        assert_eq!(
            cat("Some Game [NSP]", &[f("Some Game [0100].nsp", 3000 * MB)]),
            Category::Game
        );
        // A video by a group named like a game tag stays video.
        assert_eq!(
            cat("Film.2020.1080p-CODEX", &[f("film.mkv", 4000 * MB)]),
            Category::Video
        );
    }

    #[test]
    fn anime_shape() {
        assert!(fansub_shape(
            "[SubsPlease] Some Show - 05 (1080p) [ABCD1234].mkv"
        ));
        assert!(fansub_shape("[Group] Some Show [1080p] [0A1B2C3D]"));
        assert!(fansub_shape("[Group] Some Show (Batch) [1080p]"));
        assert!(!fansub_shape("[Group] Some Film - 1080p"));
        assert!(!fansub_shape("Some.Show.S01E05.1080p.WEB.h264"));
        assert!(!fansub_shape("[] x - 05"));
        assert!(!fansub_shape("[ www.Site.org ] - Show - 05 [1080p]"));
        assert!(!fansub_shape("[TGx] Show - 05"));
        assert_eq!(
            cat(
                "[SubsPlease] Some Show - 05 (1080p) [ABCD1234].mkv",
                &[f(
                    "[SubsPlease] Some Show - 05 (1080p) [ABCD1234].mkv",
                    1400 * MB
                )]
            ),
            Category::Anime
        );
        assert_eq!(
            cat(
                "Some.Show.S01E05.1080p.WEB.h264-GRP",
                &[f("some.show.s01e05.mkv", 1400 * MB)]
            ),
            Category::Video
        );
    }

    #[test]
    fn adult_needs_a_signal_and_visual_bytes() {
        let clip = [f("clip.mp4", 900 * MB)];
        assert_eq!(
            cat("Brazzers.24.01.02.Someone.XXX.1080p", &clip),
            Category::Porn
        );
        assert_eq!(cat("Some Studio - MILF scene", &clip), Category::Porn);
        assert_eq!(cat("SSIS-123 something", &clip), Category::Porn);
        assert_eq!(cat("ssis123", &clip), Category::Porn);
        assert_eq!(
            cat("x", &[f("dir/pornstar clip.mp4", 900 * MB)]),
            Category::Porn
        );
        // Two weak words, or only one.
        assert_eq!(cat("Naked fetish party", &clip), Category::Porn);
        assert_eq!(cat("Sex.Education.S01E01.1080p", &clip), Category::Video);
        assert_eq!(
            cat(
                "Sex.Education.S01E01.1080p",
                &[
                    f("Sex.Education.S01E01.mkv", 900 * MB),
                    f("Sex.Education.S01E01.ass", 1)
                ]
            ),
            Category::Video,
            "subtitle extension is not a word"
        );
        assert_eq!(
            cat("xXx.2002.1080p.BluRay.x264", &clip),
            Category::Video,
            "the film"
        );
        assert_eq!(cat("Moby Dick 1956", &clip), Category::Video);
        // A signal with audio bytes: not porn, but the signal is kept for the blocklist.
        let c = classify("Club hits XXX", &[f("a.mp3", 50 * MB)]);
        assert_eq!(c.category, Category::Audio);
        assert!(c.adult_signal && c.adult());
        // Name only.
        assert_eq!(cat("Some studio XXX clip", &[]), Category::Porn);
    }

    #[test]
    fn name_hints_without_a_media_majority() {
        assert_eq!(
            cat(
                "Some.Show.S02.1080p.WEB-DL.x265-GRP",
                &[f("a.rar", 2000 * MB), f("a.r00", 2000 * MB)]
            ),
            Category::Video
        );
        assert_eq!(
            cat(
                "ubuntu-24.04-desktop-amd64.iso",
                &[f("ubuntu-24.04-desktop-amd64.iso", 5000 * MB)]
            ),
            Category::Application
        );
        assert_eq!(
            cat("Some Comics Pack", &[f("a.zip", 300 * MB)]),
            Category::Book
        );
        assert_eq!(cat("random", &[f("a.zip", 300 * MB)]), Category::Other);
    }
}

//! Release-name parsing (D40) with `parse-torrent-title` 2.0.2, the parser that scored best on
//! the 1,328 imdb-tagged torrents (93.6% correct, metadata-enrichment.md §3).
//!
//! NIP-35 events give a title; DHT-crawled torrents give only the info-dict name and the file
//! list. [`names_for`] returns the names worth trying in order: the torrent name, then the
//! largest video file (scene packs often name the folder badly but the file well).

use crate::model::TorrentFile;

/// Fields the matcher uses.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ParsedName {
    /// Cleaned title (never empty when `parse_name` returns Some).
    pub title: String,
    /// Other spellings worth trying: a trailing "(…)" alternative title (the YGG bridge appends
    /// the French title that way) and the title without a "(US)"-style country tag.
    pub alt_titles: Vec<String>,
    /// First year (a "2008-2013" range gives 2008).
    pub year: Option<i32>,
    /// First season (packs "S01-S08" give 1) and first episode.
    pub season: Option<i32>,
    pub episode: Option<i32>,
    /// Season or episode markers present: look for a series first.
    pub is_series: bool,
}

/// Video file extensions considered for the DHT fallback.
pub const VIDEO_EXTENSIONS: &[&str] = &[
    "mkv", "mp4", "avi", "m4v", "ts", "m2ts", "wmv", "mov", "mpg", "mpeg", "webm", "flv", "vob",
    "divx", "ogm", "rmvb",
];

fn is_video(path: &str) -> bool {
    path.rsplit_once('.')
        .is_some_and(|(_, ext)| VIDEO_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str()))
}

/// File name without directories and extension.
fn stem(path: &str) -> &str {
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    match name.rsplit_once('.') {
        Some((s, ext)) if (2..=5).contains(&ext.len()) && !s.is_empty() => s,
        _ => name,
    }
}

/// The largest video file's name (without extension), if any file is a video.
pub fn largest_video(files: &[TorrentFile]) -> Option<&str> {
    files
        .iter()
        .filter(|f| is_video(&f.path))
        .max_by_key(|f| f.size.unwrap_or(0))
        .map(|f| stem(&f.path))
        .filter(|s| !s.trim().is_empty())
}

/// Names to try for a torrent, best first, without duplicates. A title that is just the
/// infohash (NIP-35 fallback when an event has neither title nor files) is skipped.
pub fn names_for<'a>(title: &'a str, files: &'a [TorrentFile]) -> Vec<&'a str> {
    let mut out: Vec<&str> = Vec::new();
    let t = title.trim();
    let is_hash = t.len() == 40 && t.bytes().all(|b| b.is_ascii_hexdigit());
    if !t.is_empty() && !is_hash {
        out.push(t);
    }
    if let Some(v) = largest_video(files)
        && !out
            .iter()
            .any(|o| o.eq_ignore_ascii_case(v) || stem(o) == v)
    {
        out.push(v);
    }
    out
}

fn int(v: &parse_torrent_title::Value) -> Option<i64> {
    v.as_int().or_else(|| {
        let s = v.as_str()?;
        let d: String = s.chars().take_while(char::is_ascii_digit).collect();
        d.parse().ok()
    })
}

/// A trailing "(…)" group after the release tags, e.g.
/// `Shook.2024.VFQ.720p.WEB-DL.H264-Slay3R (Choc)` → `Choc`; several titles separated by " / "
/// (`(Rex, Chien flic / Kommissar Rex)`) are each returned.
fn trailing_alts(name: &str) -> Vec<String> {
    let Some(alt) = trailing_group(name) else {
        return Vec::new();
    };
    alt.split(" / ")
        .map(|a| a.split(" - Saison").next().unwrap_or(a).trim().to_string())
        .filter(|a| a.chars().filter(|c| c.is_alphabetic()).count() >= 3)
        .collect()
}

fn trailing_group(name: &str) -> Option<&str> {
    let t = name.trim_end();
    let inner = t.strip_suffix(')')?;
    let open = inner.rfind('(')?;
    let alt = inner[open + 1..].trim();
    // Short tags like (US), (2019) or (Libre echange) are only alternatives if they have letters
    // and are not years/countries; the matcher still needs an exact dataset hit.
    let letters = alt.chars().filter(|c| c.is_alphabetic()).count();
    if letters < 3 || open == 0 {
        return None;
    }
    Some(alt)
}

/// Words a release name leaves at the end of the parsed title for a complete-series pack.
const COMPLETE_WORDS: &[&str] = &["complete", "integrale", "intégrale", "integral"];
/// Words that make a "complete" pack a set of films rather than a series.
const COLLECTION_WORDS: &[&str] = &[
    "collection",
    "trilogy",
    "trilogie",
    "saga",
    "quadrilogy",
    "pentalogy",
    "anthology",
    "duology",
];
/// Country tags release groups append to a series name ("Euphoria US", "Shameless UK").
const COUNTRY_WORDS: &[&str] = &["us", "uk", "au", "nz", "ca"];

/// `title` minus a trailing word from `words` (case-insensitive), if any.
fn strip_last_word(title: &str, words: &[&str]) -> Option<String> {
    let (head, last) = title.trim_end().rsplit_once(' ')?;
    words
        .contains(&last.to_lowercase().as_str())
        .then(|| head.trim_end().to_string())
        .filter(|h| !h.is_empty())
}

/// "The Office (US)" → "The Office".
fn without_country(title: &str) -> Option<String> {
    let t = title.trim_end();
    let inner = t.strip_suffix(')')?;
    let open = inner.rfind('(')?;
    let tag = &inner[open + 1..];
    (tag.len() == 2 && tag.bytes().all(|b| b.is_ascii_uppercase()))
        .then(|| inner[..open].trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Parse one release name. None if nothing usable is left as a title.
pub fn parse_name(name: &str) -> Option<ParsedName> {
    let p = parse_torrent_title::parse(name);
    let mut title = p.title().trim().to_string();
    let mut complete = p.complete().unwrap_or(false);
    if let Some(t) = strip_last_word(&title, COMPLETE_WORDS) {
        title = t;
        complete = true;
    }
    if title.is_empty() {
        return None;
    }
    let year = p
        .year()
        .and_then(int)
        .and_then(|y| i32::try_from(y).ok())
        .filter(|y| (1870..=2100).contains(y));
    let first = |single: Option<i64>, many: Option<&[i64]>| {
        single
            .or_else(|| many.and_then(|m| m.first().copied()))
            .and_then(|n| i32::try_from(n).ok())
    };
    let season = first(p.season(), p.seasons());
    let episode = first(p.episode(), p.episodes());
    let lower = title.to_lowercase();
    let collection = lower.split(' ').any(|w| COLLECTION_WORDS.contains(&w));
    // A complete pack without season markers is a whole series, unless it says collection.
    let is_series = season.is_some() || episode.is_some() || (complete && !collection);
    let mut alt_titles = Vec::new();
    if let Some(a) = without_country(&title).or_else(|| strip_last_word(&title, COUNTRY_WORDS)) {
        alt_titles.push(a);
    }
    for a in trailing_alts(name) {
        if !a.eq_ignore_ascii_case(&title) && !alt_titles.contains(&a) {
            alt_titles.push(a);
        }
    }
    Some(ParsedName {
        title,
        alt_titles,
        year,
        season,
        episode,
        is_series,
    })
}

/// Parse each candidate name in order; the first parse gives the title, later names only fill
/// a missing year/season/episode (a pack folder named "Show" with files "Show.S01E02…").
pub fn parse_torrent(title: &str, files: &[TorrentFile]) -> Vec<ParsedName> {
    names_for(title, files)
        .into_iter()
        .filter_map(parse_name)
        .collect()
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

    #[test]
    fn movie_and_episode() {
        let p = parse_name("Oppenheimer.2023.2160p.10bit.HDR.BluRay.6CH.x265.HEVC-PSA").unwrap();
        assert_eq!(p.title, "Oppenheimer");
        assert_eq!(p.year, Some(2023));
        assert!(!p.is_series);
        let p = parse_name("Peacemaker 2022 S02E01 1080p HEVC x265-MeGusta").unwrap();
        assert_eq!(p.title, "Peacemaker");
        assert_eq!((p.season, p.episode), (Some(2), Some(1)));
        assert!(p.is_series);
        let p = parse_name("Breaking Bad Season 1-5 2008-2013 1080p").unwrap();
        assert_eq!((p.year, p.season, p.episode), (Some(2008), Some(1), None));
        let p = parse_name("1917.2019.1080p").unwrap();
        assert_eq!((p.title.as_str(), p.year), ("1917", Some(2019)));
    }

    #[test]
    fn alternatives() {
        let p = parse_name("Shook.2024.VFQ.720p.WEB-DL.H264-Slay3R (Choc)").unwrap();
        assert_eq!(p.title, "Shook");
        assert_eq!(p.alt_titles, vec!["Choc".to_string()]);
        let p = parse_name("The Office (US) S03E01").unwrap();
        assert!(p.alt_titles.contains(&"The Office".to_string()), "{p:?}");
        let p = parse_name("Euphoria.US.S03E03.1080p.x265-ELiTE").unwrap();
        assert_eq!(p.alt_titles, vec!["Euphoria".to_string()]);
        let p = parse_name(
            "Inspector.Rex.S11.FRENCH.1080p.WEB.H264-THESYNDiCATE (Rex, Chien flic / Kommissar Rex)",
        )
        .unwrap();
        assert_eq!(p.alt_titles, vec!["Rex, Chien flic", "Kommissar Rex"]);
    }

    #[test]
    fn complete_packs() {
        let p = parse_name("The.Marvelous.Mrs.Maisel.COMPLETE.MULTi.1080p.WEB.H264-THESYNDiCATE")
            .unwrap();
        assert_eq!(p.title, "The Marvelous Mrs Maisel");
        assert!(p.is_series);
        let p = parse_name("Falco.iNTEGRALE.FRENCH.1080p.WEB.H264-BTT").unwrap();
        assert!(p.is_series);
        let p = parse_name("Harry Potter Collection 2001-2011 1080p BluRay HEVC x265 5.1 BONE")
            .unwrap();
        assert!(!p.is_series);
    }

    #[test]
    fn dht_names() {
        let files = [
            f("Movie/sample.mkv", 10),
            f("Movie/The.Matrix.1999.1080p.BluRay.x264.mkv", 9_000),
            f("Movie/The.Matrix.1999.nfo", 1),
        ];
        assert_eq!(
            largest_video(&files),
            Some("The.Matrix.1999.1080p.BluRay.x264")
        );
        let hash = "0123456789abcdef0123456789abcdef01234567";
        assert_eq!(
            names_for(hash, &files),
            vec!["The.Matrix.1999.1080p.BluRay.x264"]
        );
        assert_eq!(names_for("Movie", &files).len(), 2);
        assert!(names_for("", &[]).is_empty());
        let parsed = parse_torrent("Movie", &files);
        assert_eq!(parsed[1].year, Some(1999));
    }
}

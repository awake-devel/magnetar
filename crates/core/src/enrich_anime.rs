//! Offline anime identification (D41, docs/ENRICHMENT-ANIME.md).
//!
//! Pure code: no I/O, no database. The server downloads and imports the datasets, builds an
//! [`AnimeIndex`] from them and calls [`AnimeIndex::identify`] per torrent.
//!
//! - **anime-offline-database** (AOD, cedya77 continuation of manami-project, ODbL 1.0 +
//!   DbCL 1.0): titles, synonyms, type, episode count, season/year and the provider ids
//!   (MyAnimeList, AniList, AniDB, Kitsu) of one anime entry. MAL and AniDB list every season
//!   (and every cour) as its own entry.
//! - **Fribb anime-lists** `anime-list-full.json`: maps those entries to TVDB / TMDB / IMDb and,
//!   crucially, gives the TVDB (and TMDB) **season number** and episode offset of each entry.
//!
//! Matching (measured in docs/ENRICHMENT-ANIME.md): exact normalised title or synonym against
//! AOD, ranked by year and type; then a season-aware remap through Fribb (`S03` of the series
//! whose first season matched → the entry Fribb maps to TVDB season 3), and an absolute-episode
//! remap for releases numbered past the first season (`Golden Kamuy - 38`).
//!
//! Strong ids (MyAnimeList / AniList / AniDB links in descriptions, `i` tags) always win over
//! title matching. Nothing here is ever published back to Nostr (D41).

use std::collections::{HashMap, HashSet};

use serde::Deserialize;

use crate::error::{Error, Result};
use crate::model::TorrentFile;

/// Bump when the matching logic changes, so stored `offline_title` rows are recomputed.
pub const MATCHER_VERSION: i32 = 1;

/// Confidence of a match taken from an `i` tag (`mal:`, `anilist:`).
pub const CONF_ITAG: f32 = 1.0;
/// Confidence of a match taken from a description link.
pub const CONF_DESC_LINK: f32 = 0.97;

/// AOD entry type.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AnimeType {
    Tv,
    Movie,
    Ova,
    Ona,
    Special,
    Unknown,
}

impl AnimeType {
    pub fn parse(s: &str) -> Self {
        match s.to_ascii_uppercase().as_str() {
            "TV" => AnimeType::Tv,
            "MOVIE" => AnimeType::Movie,
            "OVA" => AnimeType::Ova,
            "ONA" => AnimeType::Ona,
            "SPECIAL" => AnimeType::Special,
            _ => AnimeType::Unknown,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            AnimeType::Tv => "TV",
            AnimeType::Movie => "MOVIE",
            AnimeType::Ova => "OVA",
            AnimeType::Ona => "ONA",
            AnimeType::Special => "SPECIAL",
            AnimeType::Unknown => "UNKNOWN",
        }
    }
}

/// One anime-offline-database entry, reduced to what matching and `media` need.
#[derive(Clone, Debug, PartialEq)]
pub struct AodEntry {
    pub mal: Option<u32>,
    pub anilist: Option<u32>,
    pub anidb: Option<u32>,
    pub kitsu: Option<u32>,
    pub title: String,
    pub synonyms: Vec<String>,
    pub kind: AnimeType,
    pub episodes: Option<u32>,
    /// "WINTER" | "SPRING" | "SUMMER" | "FALL" | "UNDEFINED"
    pub season: Option<String>,
    pub year: Option<i32>,
    /// "FINISHED" | "ONGOING" | "UPCOMING" | "UNKNOWN"
    pub status: Option<String>,
    /// Median of the provider scores, 1-10.
    pub score: Option<f32>,
    pub duration_secs: Option<u32>,
    pub tags: Vec<String>,
    /// Provider CDN URL. Not used for display (the phone never contacts third parties).
    pub picture: Option<String>,
}

/// Metadata line at the top of the AOD JSONL file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AodHeader {
    pub last_update: Option<String>,
    pub license: Option<String>,
}

#[derive(Deserialize)]
struct RawAodHeader {
    #[serde(rename = "lastUpdate")]
    last_update: Option<String>,
    license: Option<RawLicense>,
}

#[derive(Deserialize)]
struct RawLicense {
    name: Option<String>,
}

#[derive(Deserialize)]
struct RawAod {
    #[serde(default)]
    sources: Vec<String>,
    title: String,
    #[serde(default, rename = "type")]
    kind: Option<String>,
    #[serde(default)]
    episodes: Option<u32>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default, rename = "animeSeason")]
    anime_season: Option<RawAodSeason>,
    #[serde(default)]
    picture: Option<String>,
    #[serde(default)]
    duration: Option<RawAodDuration>,
    #[serde(default)]
    score: Option<RawAodScore>,
    #[serde(default)]
    synonyms: Vec<String>,
    #[serde(default)]
    tags: Vec<String>,
}

#[derive(Deserialize)]
struct RawAodSeason {
    season: Option<String>,
    year: Option<i32>,
}

#[derive(Deserialize)]
struct RawAodDuration {
    value: Option<u32>,
    unit: Option<String>,
}

#[derive(Deserialize)]
struct RawAodScore {
    median: Option<f32>,
}

/// Numeric id after `<host>/anime/` in a provider URL.
fn source_id(sources: &[String], host: &str) -> Option<u32> {
    sources.iter().find_map(|s| {
        let rest = s.split_once(host)?.1.strip_prefix("/anime/")?;
        let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
        digits.parse().ok().filter(|&n| n > 0)
    })
}

/// Parse one AOD JSONL line. `Ok(None)` for the header and blank lines.
pub fn parse_aod_line(line: &str) -> Result<Option<AodEntry>> {
    let line = line.trim();
    if line.is_empty() || line.starts_with("{\"$schema\"") {
        return Ok(None);
    }
    let raw: RawAod = serde_json::from_str(line)?;
    let duration_secs = raw.duration.and_then(|d| {
        let v = d.value?;
        match d.unit.as_deref() {
            Some("SECONDS") | None => Some(v),
            _ => None,
        }
    });
    let (season, year) = match raw.anime_season {
        Some(s) => (s.season.filter(|s| s != "UNDEFINED"), s.year),
        None => (None, None),
    };
    Ok(Some(AodEntry {
        mal: source_id(&raw.sources, "myanimelist.net"),
        anilist: source_id(&raw.sources, "anilist.co"),
        anidb: source_id(&raw.sources, "anidb.net"),
        kitsu: source_id(&raw.sources, "kitsu.app").or(source_id(&raw.sources, "kitsu.io")),
        title: raw.title,
        synonyms: raw.synonyms,
        kind: raw
            .kind
            .as_deref()
            .map_or(AnimeType::Unknown, AnimeType::parse),
        episodes: raw.episodes.filter(|&e| e > 0),
        season,
        year: year.filter(|&y| y > 1900),
        status: raw.status,
        score: raw.score.and_then(|s| s.median),
        duration_secs,
        tags: raw.tags,
        picture: raw.picture,
    }))
}

/// Parse the AOD header line (the first line of the JSONL file).
pub fn parse_aod_header(line: &str) -> Option<AodHeader> {
    let h: RawAodHeader = serde_json::from_str(line.trim()).ok()?;
    Some(AodHeader {
        last_update: h.last_update,
        license: h.license.and_then(|l| l.name),
    })
}

/// Parse a whole AOD JSONL document. Malformed lines are counted, not fatal.
pub fn parse_aod_jsonl(text: &str) -> (Option<AodHeader>, Vec<AodEntry>, usize) {
    let mut header = None;
    let mut out = Vec::new();
    let mut bad = 0;
    for (i, line) in text.lines().enumerate() {
        if i == 0
            && let Some(h) = parse_aod_header(line)
            && line.contains("\"$schema\"")
        {
            header = Some(h);
            continue;
        }
        match parse_aod_line(line) {
            Ok(Some(e)) => out.push(e),
            Ok(None) => {}
            Err(_) => bad += 1,
        }
    }
    (header, out, bad)
}

/// One Fribb `anime-list-full.json` entry (merged per AniDB id upstream).
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
pub struct FribbEntry {
    #[serde(default, rename = "type")]
    pub kind: Option<String>,
    #[serde(default, rename = "mal_id")]
    pub mal: Option<u32>,
    #[serde(default, rename = "anilist_id")]
    pub anilist: Option<u32>,
    #[serde(default, rename = "anidb_id")]
    pub anidb: Option<u32>,
    #[serde(default, rename = "imdb_id", deserialize_with = "string_or_list")]
    pub imdb: Vec<String>,
    #[serde(default, rename = "themoviedb_id")]
    pub tmdb: Option<FribbTmdb>,
    #[serde(default, rename = "tvdb_id")]
    pub tvdb: Option<u32>,
    #[serde(default)]
    pub season: Option<FribbPair>,
    #[serde(default)]
    pub episode_offset: Option<FribbPair>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
pub struct FribbTmdb {
    #[serde(default)]
    pub tv: Option<u32>,
    #[serde(default, deserialize_with = "num_or_list")]
    pub movie: Vec<u32>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Deserialize)]
pub struct FribbPair {
    #[serde(default)]
    pub tvdb: Option<i32>,
    #[serde(default)]
    pub tmdb: Option<i32>,
}

fn string_or_list<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<String>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum V {
        One(String),
        Many(Vec<String>),
        None(()),
    }
    Ok(match V::deserialize(d)? {
        V::One(s) => vec![s],
        V::Many(v) => v,
        V::None(()) => Vec::new(),
    })
}

fn num_or_list<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<u32>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum V {
        One(u32),
        Many(Vec<u32>),
        None(()),
    }
    Ok(match V::deserialize(d)? {
        V::One(n) => vec![n],
        V::Many(v) => v,
        V::None(()) => Vec::new(),
    })
}

impl FribbEntry {
    pub fn tmdb_tv(&self) -> Option<u32> {
        self.tmdb.as_ref().and_then(|t| t.tv)
    }

    pub fn tmdb_movie(&self) -> Option<u32> {
        self.tmdb.as_ref().and_then(|t| t.movie.first().copied())
    }

    /// Valid `tt` ids only.
    pub fn imdb_id(&self) -> Option<&str> {
        self.imdb.iter().map(String::as_str).find(|s| is_imdb_id(s))
    }

    fn series(&self) -> Option<SeriesKey> {
        if let Some(t) = self.tvdb {
            return Some(SeriesKey::Tvdb(t));
        }
        self.tmdb_tv().map(SeriesKey::TmdbTv)
    }

    /// Season number on the series the entry belongs to (TVDB numbering for a TVDB key).
    fn season_on(&self, key: SeriesKey) -> Option<i32> {
        let p = self.season?;
        match key {
            SeriesKey::Tvdb(_) => p.tvdb,
            SeriesKey::TmdbTv(_) => p.tmdb,
        }
    }

    fn offset_on(&self, key: SeriesKey) -> i32 {
        let Some(p) = self.episode_offset else {
            return 0;
        };
        match key {
            SeriesKey::Tvdb(_) => p.tvdb,
            SeriesKey::TmdbTv(_) => p.tmdb,
        }
        .unwrap_or(0)
        .max(0)
    }
}

/// Parse the Fribb `anime-list-full.json` array.
pub fn parse_fribb(json: &str) -> Result<Vec<FribbEntry>> {
    serde_json::from_str(json).map_err(|e| Error::Invalid(format!("fribb anime-list-full: {e}")))
}

pub fn is_imdb_id(s: &str) -> bool {
    s.len() >= 9
        && s.len() <= 12
        && s.starts_with("tt")
        && s[2..].bytes().all(|b| b.is_ascii_digit())
}

/// What we could read from a release name.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Release {
    /// Title candidates, most likely first (parsed title, then alternative titles found in
    /// brackets or after `|`).
    pub titles: Vec<String>,
    pub season: Option<i32>,
    pub episode: Option<i32>,
    pub year: Option<i32>,
}

/// Anime-native ids of one torrent from `i` tags and description links.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AnimeIds {
    pub mal: Option<u32>,
    pub anilist: Option<u32>,
    pub anidb: Option<u32>,
    /// True when they came from `i` tags (spec), false for description links.
    pub from_itag: bool,
}

impl AnimeIds {
    pub fn is_empty(&self) -> bool {
        self.mal.is_none() && self.anilist.is_none() && self.anidb.is_none()
    }
}

fn digits_after(s: &str, prefix: &str) -> Option<u32> {
    let mut rest = s;
    while let Some(i) = rest.find(prefix) {
        let tail = &rest[i + prefix.len()..];
        let d: String = tail.chars().take_while(char::is_ascii_digit).collect();
        if let Ok(n) = d.parse::<u32>()
            && n > 0
        {
            return Some(n);
        }
        rest = tail;
    }
    None
}

/// MAL / AniList / AniDB ids from `i` tags (`mal:123`, `anilist:456`, `anidb:789`) first, then
/// from links in the description (`myanimelist.net/anime/123`, `anilist.co/anime/456`,
/// `anidb.net/anime/789`, legacy `anidb.net/a789` and `anidb.net/perl-bin/animedb.pl?…aid=789`).
pub fn extract_ids(description: &str, external_ids: &[(String, String)]) -> AnimeIds {
    let mut ids = AnimeIds::default();
    for (k, v) in external_ids {
        let n = v
            .trim()
            .split(|c: char| !c.is_ascii_digit())
            .next()
            .and_then(|d| d.parse::<u32>().ok())
            .filter(|&n| n > 0);
        match k.trim().to_ascii_lowercase().as_str() {
            "mal" | "myanimelist" => ids.mal = ids.mal.or(n),
            "anilist" => ids.anilist = ids.anilist.or(n),
            "anidb" => ids.anidb = ids.anidb.or(n),
            _ => {}
        }
    }
    if !ids.is_empty() {
        ids.from_itag = true;
        return ids;
    }
    let lower = description.to_ascii_lowercase();
    ids.mal = digits_after(&lower, "myanimelist.net/anime/")
        .or_else(|| digits_after(&lower, "myanimelist.net/anime.php?id="));
    ids.anilist = digits_after(&lower, "anilist.co/anime/");
    ids.anidb = digits_after(&lower, "anidb.net/anime/")
        .or_else(|| digits_after(&lower, "anidb.net/a"))
        .or_else(|| {
            lower
                .contains("anidb.net/perl-bin/animedb.pl")
                .then(|| digits_after(&lower, "aid="))
                .flatten()
        });
    ids
}

/// Lowercase, keep letters and digits (any script), everything else separates words. `&` reads
/// as "and" and apostrophes vanish so `Hero's` and `Heros` meet.
pub fn normalize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut space = true;
    for c in s.chars() {
        let c = match c {
            '’' | '\'' | '`' | '´' => continue,
            '×' => 'x',
            '&' => {
                if !space {
                    out.push(' ');
                }
                out.push_str("and ");
                space = true;
                continue;
            }
            c => c,
        };
        if c.is_alphanumeric() {
            for l in c.to_lowercase() {
                out.push(l);
            }
            space = false;
        } else if !space {
            out.push(' ');
            space = true;
        }
    }
    if out.ends_with(' ') {
        out.pop();
    }
    out
}

fn ordinal_number(w: &str) -> Option<i32> {
    let w = w.trim_end_matches(['.', ':']);
    let digits: String = w.chars().take_while(char::is_ascii_digit).collect();
    if !digits.is_empty() {
        let suffix = &w[digits.len()..];
        if matches!(suffix, "" | "st" | "nd" | "rd" | "th") {
            return digits.parse().ok();
        }
        return None;
    }
    Some(match w {
        "first" => 1,
        "second" => 2,
        "third" => 3,
        "fourth" => 4,
        "fifth" => 5,
        "sixth" => 6,
        "ii" => 2,
        "iii" => 3,
        "iv" => 4,
        _ => return None,
    })
}

/// Split a trailing season marker off a normalised title: `medalist season 2`,
/// `medalist 2nd season`, `dr stone s4` → (`medalist`, 2) etc.
pub fn split_season(norm: &str) -> Option<(String, i32)> {
    let words: Vec<&str> = norm.split(' ').collect();
    let n = words.len();
    if n >= 3 && words[n - 2] == "season" {
        let s = ordinal_number(words[n - 1])?;
        return Some((words[..n - 2].join(" "), s));
    }
    if n >= 3 && matches!(words[n - 2], "tv" | "тв") {
        let s = ordinal_number(words[n - 1])?;
        return Some((words[..n - 2].join(" "), s));
    }
    if n >= 3 && words[n - 1] == "season" {
        let s = ordinal_number(words[n - 2])?;
        return Some((words[..n - 2].join(" "), s));
    }
    if n >= 2 {
        let w = words[n - 1];
        if let Some(d) = w.strip_prefix('s')
            && !d.is_empty()
            && d.len() <= 2
            && d.bytes().all(|b| b.is_ascii_digit())
        {
            return Some((words[..n - 1].join(" "), d.parse().ok()?));
        }
    }
    None
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum SeriesKey {
    Tvdb(u32),
    TmdbTv(u32),
}

/// How a match was found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MatchHow {
    /// Strong id from an `i` tag.
    Itag,
    /// Strong id from a description link.
    DescLink,
    /// Title matched, entry used as is.
    Title,
    /// Title matched the series; Fribb season mapping picked the season entry.
    SeasonRemap,
    /// Title matched the series; the absolute episode number picked a later season.
    EpisodeRemap,
}

impl MatchHow {
    /// `torrent_media.method` value (contract).
    pub fn method(&self) -> &'static str {
        match self {
            MatchHow::Itag => "itag",
            MatchHow::DescLink => "desc_link",
            _ => "offline_title",
        }
    }
}

/// Result of identifying one torrent.
#[derive(Clone, Debug, PartialEq)]
pub struct AnimeMatch {
    /// Index into [`AnimeIndex::entries`].
    pub entry: usize,
    pub how: MatchHow,
    pub confidence: f32,
    /// Season on the series (TVDB numbering) when known.
    pub season: Option<i32>,
    /// Episode within the matched entry (absolute numbers are rebased onto the entry).
    pub episode: Option<i32>,
}

/// Cross ids for one AOD entry from Fribb.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CrossIds {
    pub imdb: Option<String>,
    pub tmdb_tv: Option<u32>,
    pub tmdb_movie: Option<u32>,
    pub tvdb: Option<u32>,
    pub season: Option<i32>,
    pub episode_offset: i32,
    /// Which of the ids above this entry owns. Several MAL entries (seasons, cours, recaps)
    /// share one IMDb / TVDB / TMDB series id, but `media` holds each external id once: the
    /// owner is the first TV entry of the series (lowest season ≥ 1, then offset, then year).
    pub owns_imdb: bool,
    pub owns_tmdb_tv: bool,
    pub owns_tmdb_movie: bool,
    pub owns_tvdb: bool,
}

impl CrossIds {
    /// Only the ids this entry owns (what goes into its `media` row).
    pub fn owned(&self) -> CrossIds {
        CrossIds {
            imdb: self.imdb.clone().filter(|_| self.owns_imdb),
            tmdb_tv: self.tmdb_tv.filter(|_| self.owns_tmdb_tv),
            tmdb_movie: self.tmdb_movie.filter(|_| self.owns_tmdb_movie),
            tvdb: self.tvdb.filter(|_| self.owns_tvdb),
            ..self.clone()
        }
    }
}

/// In-memory matching index (~40k entries, ~200k title keys).
pub struct AnimeIndex {
    pub entries: Vec<AodEntry>,
    by_title: HashMap<String, Vec<u32>>,
    by_mal: HashMap<u32, u32>,
    by_anilist: HashMap<u32, u32>,
    by_anidb: HashMap<u32, u32>,
    /// Fribb entry per AOD entry.
    fribb_of: Vec<Option<FribbEntry>>,
    /// Series → AOD entries that belong to it.
    series: HashMap<SeriesKey, Vec<u32>>,
    /// `"{entry}/{kind}:{id}"` for each shared id an entry owns (see [`CrossIds::owned`]).
    owned: HashSet<String>,
}

/// Season-1 TV entries of a series sorted by (season, offset), with episode counts: used for
/// absolute episode numbers.
struct Seasonal {
    entry: u32,
    season: i32,
    offset: i32,
}

impl AnimeIndex {
    pub fn new(entries: Vec<AodEntry>, fribb: Vec<FribbEntry>) -> Self {
        let mut by_title: HashMap<String, Vec<u32>> = HashMap::new();
        let mut by_mal = HashMap::new();
        let mut by_anilist = HashMap::new();
        let mut by_anidb = HashMap::new();
        for (i, e) in entries.iter().enumerate() {
            let i = i as u32;
            let mut keys: HashSet<String> = HashSet::new();
            keys.insert(normalize(&e.title));
            for s in &e.synonyms {
                keys.insert(normalize(s));
            }
            for k in keys {
                if !k.is_empty() {
                    by_title.entry(k).or_default().push(i);
                }
            }
            if let Some(m) = e.mal {
                by_mal.entry(m).or_insert(i);
            }
            if let Some(a) = e.anilist {
                by_anilist.entry(a).or_insert(i);
            }
            if let Some(a) = e.anidb {
                by_anidb.entry(a).or_insert(i);
            }
        }
        let mut fribb_of: Vec<Option<FribbEntry>> = vec![None; entries.len()];
        for f in fribb {
            let i = f
                .mal
                .and_then(|m| by_mal.get(&m))
                .or_else(|| f.anilist.and_then(|a| by_anilist.get(&a)))
                .or_else(|| f.anidb.and_then(|a| by_anidb.get(&a)))
                .copied();
            if let Some(i) = i
                && fribb_of[i as usize].is_none()
            {
                fribb_of[i as usize] = Some(f);
            }
        }
        let mut series: HashMap<SeriesKey, Vec<u32>> = HashMap::new();
        for (i, f) in fribb_of.iter().enumerate() {
            if let Some(k) = f.as_ref().and_then(FribbEntry::series) {
                series.entry(k).or_default().push(i as u32);
            }
        }
        // Primary owner of each shared id: TV before others, lowest season ≥ 1, lowest offset,
        // then earliest year, then lowest index.
        let rank = |i: u32| {
            let e = &entries[i as usize];
            let f = fribb_of[i as usize].as_ref();
            let season = f
                .and_then(|f| f.season.and_then(|s| s.tvdb.or(s.tmdb)))
                .unwrap_or(99);
            let season = if season <= 0 { 50 } else { season };
            let offset = f.map_or(0, |f| f.episode_offset.and_then(|o| o.tvdb).unwrap_or(0));
            (
                e.kind != AnimeType::Tv,
                season,
                offset,
                e.year.unwrap_or(9999),
                i,
            )
        };
        let mut owner: HashMap<String, u32> = HashMap::new();
        for (i, f) in fribb_of.iter().enumerate() {
            let Some(f) = f else { continue };
            let i = i as u32;
            let mut keys = Vec::new();
            if let Some(t) = f.imdb_id() {
                keys.push(format!("imdb:{t}"));
            }
            if let Some(t) = f.tvdb {
                keys.push(format!("tvdb:{t}"));
            }
            if let Some(t) = f.tmdb_tv() {
                keys.push(format!("tmdbtv:{t}"));
            }
            if let Some(t) = f.tmdb_movie() {
                keys.push(format!("tmdbmovie:{t}"));
            }
            for k in keys {
                owner
                    .entry(k)
                    .and_modify(|o| {
                        if rank(i) < rank(*o) {
                            *o = i;
                        }
                    })
                    .or_insert(i);
            }
        }
        let owned: HashSet<String> = owner.into_iter().map(|(k, i)| format!("{i}/{k}")).collect();
        AnimeIndex {
            entries,
            by_title,
            by_mal,
            by_anilist,
            by_anidb,
            fribb_of,
            series,
            owned,
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn by_mal(&self, mal: u32) -> Option<usize> {
        self.by_mal.get(&mal).map(|&i| i as usize)
    }

    /// TVDB/TMDB/IMDb ids and season of an entry (from Fribb).
    pub fn cross_ids(&self, entry: usize) -> CrossIds {
        let Some(f) = self.fribb_of.get(entry).and_then(Option::as_ref) else {
            return CrossIds::default();
        };
        let key = f.series();
        let owns = |k: String| self.owned.contains(&format!("{entry}/{k}"));
        CrossIds {
            imdb: f.imdb_id().map(str::to_string),
            tmdb_tv: f.tmdb_tv(),
            tmdb_movie: f.tmdb_movie(),
            tvdb: f.tvdb,
            season: key.and_then(|k| f.season_on(k)),
            episode_offset: key.map_or(0, |k| f.offset_on(k)),
            owns_imdb: f.imdb_id().is_some_and(|t| owns(format!("imdb:{t}"))),
            owns_tmdb_tv: f.tmdb_tv().is_some_and(|t| owns(format!("tmdbtv:{t}"))),
            owns_tmdb_movie: f
                .tmdb_movie()
                .is_some_and(|t| owns(format!("tmdbmovie:{t}"))),
            owns_tvdb: f.tvdb.is_some_and(|t| owns(format!("tvdb:{t}"))),
        }
    }

    /// Strong ids → entry.
    pub fn by_ids(&self, ids: &AnimeIds) -> Option<usize> {
        ids.mal
            .and_then(|m| self.by_mal.get(&m))
            .or_else(|| ids.anilist.and_then(|a| self.by_anilist.get(&a)))
            .or_else(|| ids.anidb.and_then(|a| self.by_anidb.get(&a)))
            .map(|&i| i as usize)
    }

    /// Exact normalised title / synonym lookup.
    pub fn candidates(&self, title: &str) -> &[u32] {
        self.by_title
            .get(&normalize(title))
            .map_or(&[], Vec::as_slice)
    }

    fn rank_key(
        &self,
        i: u32,
        year: Option<i32>,
        season: Option<i32>,
        episode: Option<i32>,
    ) -> i64 {
        let e = &self.entries[i as usize];
        let mut s = 0i64;
        if year.is_some() && e.year == year {
            s += 1000;
        }
        if let (Some(ep), Some(n)) = (episode, e.episodes)
            && ep >= 1
            && (ep as u32) <= n
        {
            s += 40;
        }
        match e.kind {
            AnimeType::Tv => s += 100,
            AnimeType::Ona => s += 60,
            AnimeType::Movie if episode.is_none() && season.is_none() => s += 50,
            _ => {}
        }
        if let Some(f) = self.fribb_of[i as usize].as_ref()
            && let Some(k) = f.series()
            && let Some(sn) = f.season_on(k)
        {
            match season {
                Some(want) if want == sn => s += 300,
                None if sn == 1 => s += 20,
                _ => {}
            }
        }
        // Newer entries win remaining ties: most torrents are current-season releases.
        s * 10_000 + i64::from(e.year.unwrap_or(0))
    }

    fn best(&self, c: &[u32], r: &Release) -> Option<u32> {
        c.iter()
            .copied()
            .max_by_key(|&i| (self.rank_key(i, r.year, r.season, r.episode), -(i as i64)))
    }

    fn series_of(&self, c: &[u32], first: u32) -> Option<SeriesKey> {
        std::iter::once(first)
            .chain(c.iter().copied())
            .find_map(|i| self.fribb_of[i as usize].as_ref()?.series())
    }

    fn seasonal(&self, key: SeriesKey) -> Vec<Seasonal> {
        let mut v: Vec<Seasonal> = self
            .series
            .get(&key)
            .into_iter()
            .flatten()
            .filter_map(|&i| {
                let f = self.fribb_of[i as usize].as_ref()?;
                let season = f.season_on(key)?;
                (season >= 1).then(|| Seasonal {
                    entry: i,
                    season,
                    offset: f.offset_on(key),
                })
            })
            .collect();
        v.sort_by_key(|s| (s.season, s.offset, s.entry));
        v
    }

    /// Entry for season `s` of the series; with several cours in one season, the one whose
    /// episode range contains `episode` (season-relative), else the first.
    fn season_entry(&self, key: SeriesKey, s: i32, episode: Option<i32>) -> Option<(u32, i32)> {
        let all = self.seasonal(key);
        let cands: Vec<&Seasonal> = all
            .iter()
            .filter(|x| x.season == s)
            .filter(|x| {
                // Only TV/ONA-like entries, not recaps or movies sharing the season number.
                matches!(
                    self.entries[x.entry as usize].kind,
                    AnimeType::Tv | AnimeType::Ona | AnimeType::Unknown
                )
            })
            .collect();
        let first = cands.first()?;
        if let Some(ep) = episode {
            for c in cands.iter().rev() {
                if ep > c.offset {
                    return Some((c.entry, ep - c.offset));
                }
            }
        }
        Some((first.entry, episode.unwrap_or(0) - first.offset))
    }

    /// Absolute episode number past the first entry: walk the series in season order.
    fn absolute_entry(&self, key: SeriesKey, episode: i32) -> Option<(u32, i32, i32)> {
        let mut start = 0i32;
        let mut prev_season = 0;
        for s in self.seasonal(key) {
            let e = &self.entries[s.entry as usize];
            if !matches!(e.kind, AnimeType::Tv | AnimeType::Ona) {
                continue;
            }
            let n = e.episodes? as i32;
            if s.season != prev_season {
                prev_season = s.season;
            }
            if episode > start && episode <= start + n {
                return Some((s.entry, s.season, episode - start));
            }
            start += n;
        }
        None
    }

    /// Lookup keys for one normalised title: the title itself, the title without a season
    /// suffix, and for a later season the usual ways sequels are named (`x 2`, `x season 2`,
    /// `x 2nd season`, `x ii`). Each key comes with the season it implies and whether the key
    /// itself names that season (`marked`).
    fn title_keys(norm: &str, season: Option<i32>) -> Vec<(String, Option<i32>, bool)> {
        let mut keys = Vec::new();
        match split_season(norm) {
            Some((base, s)) if season.is_none_or(|x| x == s) => {
                keys.push((norm.to_string(), Some(s), true));
                keys.push((base, Some(s), false));
            }
            _ => keys.push((norm.to_string(), season, false)),
        }
        if let Some(s) = season.filter(|&s| s > 1) {
            let ord = match s {
                2 => "2nd".to_string(),
                3 => "3rd".to_string(),
                n => format!("{n}th"),
            };
            let roman = [
                "", "i", "ii", "iii", "iv", "v", "vi", "vii", "viii", "ix", "x",
            ]
            .get(s as usize)
            .copied()
            .unwrap_or("");
            let base = keys[keys.len() - 1].0.clone();
            for k in [
                format!("{base} {s}"),
                format!("{base} season {s}"),
                format!("{base} {ord} season"),
                format!("{base} s{s}"),
                format!("{base} {roman}"),
            ] {
                if !k.ends_with(' ') {
                    keys.push((k, Some(s), true));
                }
            }
        }
        keys
    }

    /// Fribb season of an entry on its own series.
    fn entry_season(&self, i: usize) -> Option<i32> {
        let f = self.fribb_of[i].as_ref()?;
        f.season_on(f.series()?)
    }

    /// Pick an entry among exact title candidates, then remap to the right season / cour.
    fn match_candidates(&self, c: &[u32], r: &Release, marked: bool) -> Option<AnimeMatch> {
        let base = self.best(c, r)?;
        let unique = c.len() == 1;
        let base_entry = &self.entries[base as usize];
        let key = self.series_of(c, base);
        let base_season = self.entry_season(base as usize);
        let past_end = |n: Option<u32>| matches!((r.episode, n), (Some(ep), Some(n)) if ep > 0 && ep as u32 > n);
        // Season given and the matched entry is another season, or a season's later cour
        // (S01E21 of a 13-episode first cour): remap through Fribb.
        if !marked
            && let (Some(s), Some(k)) = (r.season, key)
            && s >= 1
            && (base_season != Some(s) || past_end(base_entry.episodes))
            && let Some((e, ep)) = self.season_entry(k, s, r.episode)
            && e != base
        {
            return Some(AnimeMatch {
                entry: e as usize,
                how: MatchHow::SeasonRemap,
                confidence: 0.85,
                season: Some(s),
                episode: r.episode.map(|_| ep),
            });
        }
        // No season, episode past the matched entry's length: absolute numbering.
        if r.season.is_none()
            && past_end(base_entry.episodes)
            && let (Some(ep), Some(k)) = (r.episode, key)
            && let Some((e, s, rel)) = self.absolute_entry(k, ep)
        {
            return Some(AnimeMatch {
                entry: e as usize,
                how: MatchHow::EpisodeRemap,
                confidence: 0.8,
                season: Some(s),
                episode: Some(rel),
            });
        }
        let year_ok = r.year.is_some() && base_entry.year == r.year;
        Some(AnimeMatch {
            entry: base as usize,
            how: MatchHow::Title,
            confidence: if unique || year_ok { 0.9 } else { 0.75 },
            season: if marked {
                r.season
            } else {
                base_season.or(r.season)
            },
            episode: r.episode,
        })
    }

    /// How well a match fits what the release says (season, episode range, specials).
    fn agreement(&self, m: &AnimeMatch, r: &Release, marked: bool) -> i32 {
        let e = &self.entries[m.entry];
        let mut a = 0;
        match r.season {
            Some(0) => {
                if matches!(e.kind, AnimeType::Ova | AnimeType::Special | AnimeType::Ona) {
                    a += 2;
                }
            }
            Some(s) => match self.entry_season(m.entry) {
                Some(es) if es == s => a += 2,
                Some(_) if marked => a += 1,
                Some(_) => a -= 2,
                None if marked => a += 2,
                None => {}
            },
            None => {}
        }
        if let (Some(ep), Some(n)) = (m.episode.or(r.episode), e.episodes)
            && ep > 0
        {
            a += if ep as u32 <= n { 1 } else { -1 };
        }
        a
    }

    /// Title match with the season / absolute episode remaps. Every title candidate is tried;
    /// the match that agrees best with the release's season and episode wins, earlier titles
    /// break ties.
    pub fn match_release(&self, r: &Release) -> Option<AnimeMatch> {
        let mut best: Option<(i32, AnimeMatch)> = None;
        for (pos, t) in r.titles.iter().enumerate() {
            let norm = normalize(t);
            if norm.is_empty() {
                continue;
            }
            for (key, season, marked) in Self::title_keys(&norm, r.season) {
                let Some(c) = self.by_title.get(&key) else {
                    continue;
                };
                let rr = Release {
                    season,
                    ..r.clone()
                };
                let Some(mut m) = self.match_candidates(c, &rr, marked) else {
                    continue;
                };
                let score = self.agreement(&m, &rr, marked) * 100 - pos as i32;
                if pos > 0 {
                    m.confidence -= 0.03;
                }
                if best.as_ref().is_none_or(|(s, _)| score > *s) {
                    best = Some((score, m));
                }
            }
        }
        best.map(|(score, mut m)| {
            if score < 0 {
                m.confidence -= 0.15;
            }
            m.confidence = m.confidence.clamp(0.0, 1.0);
            m
        })
    }

    /// Strong ids first, then the title.
    pub fn identify(&self, ids: &AnimeIds, release: &Release) -> Option<AnimeMatch> {
        if let Some(e) = self.by_ids(ids) {
            let cross = self.cross_ids(e);
            return Some(AnimeMatch {
                entry: e,
                how: if ids.from_itag {
                    MatchHow::Itag
                } else {
                    MatchHow::DescLink
                },
                confidence: if ids.from_itag {
                    CONF_ITAG
                } else {
                    CONF_DESC_LINK
                },
                season: cross.season.or(release.season),
                episode: release.episode,
            });
        }
        self.match_release(release)
    }
}

const VIDEO_EXT: &[&str] = &[
    "mkv", "mp4", "avi", "m4v", "webm", "ts", "m2ts", "wmv", "mov",
];

/// The name to parse: the torrent title, or for DHT-style input with an unusable name (empty
/// or a bare infohash) the largest video file's base name.
pub fn release_name<'a>(title: &'a str, files: &'a [TorrentFile]) -> &'a str {
    let t = title.trim();
    let bare_hash = t.len() == 40 && t.bytes().all(|b| b.is_ascii_hexdigit());
    if !t.is_empty() && !bare_hash {
        return t;
    }
    files
        .iter()
        .filter(|f| {
            f.path
                .rsplit_once('.')
                .is_some_and(|(_, ext)| VIDEO_EXT.contains(&ext.to_ascii_lowercase().as_str()))
        })
        .max_by_key(|f| f.size.unwrap_or(0))
        .map(|f| f.path.rsplit('/').next().unwrap_or(&f.path))
        .unwrap_or(t)
}

/// Alternative titles a release name carries besides its main title: the content of `(…)`
/// groups that look like a title (Tsundere-Raws, Yameii, Gecko put the romaji / English titles
/// there, comma-separated after language tags), and the parts after ` | ` (SubsPlus+, Russian
/// releases).
pub fn alt_titles(name: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut push = |s: &str| {
        let s = s
            .trim()
            .trim_matches(|c: char| matches!(c, ')' | '(' | '[' | ']' | ' '));
        if looks_like_title(s) && !out.iter().any(|o| o == s) {
            out.push(s.to_string());
        }
    };
    let mut depth = 0usize;
    let mut start = 0usize;
    for (i, c) in name.char_indices() {
        match c {
            '(' => {
                if depth == 0 {
                    start = i + 1;
                }
                depth += 1;
            }
            ')' if depth > 0 => {
                depth -= 1;
                if depth == 0 {
                    for part in split_alts(&name[start..i]) {
                        push(part);
                    }
                }
            }
            _ => {}
        }
    }
    let mut pipes = name.split(" | ");
    pipes.next();
    for p in pipes {
        let p = p.split(['[', '(']).next().unwrap_or("");
        push(p);
    }
    out
}

/// Split a bracket group into title candidates: always on `;`, ` | ` and commas without a
/// following space (`Quanzhi Fashi VII,Versatile Mage Season 7`); on `, ` only when the group
/// also holds tags (`VF, FRENCH, VOSTFR, Enen no Shouboutai, Fire Force Season 3`), because
/// titles contain commas too (`Kanojo, Okarishimasu`).
fn split_alts(group: &str) -> Vec<&str> {
    let mut parts: Vec<&str> = vec![group];
    for sep in [";", " | ", "; "] {
        parts = parts.iter().flat_map(|p| p.split(sep)).collect();
    }
    parts = parts
        .iter()
        .flat_map(|p| {
            let mut v = Vec::new();
            let mut last = 0;
            let b = p.as_bytes();
            for (i, &c) in b.iter().enumerate() {
                if c == b',' && b.get(i + 1).is_some_and(|n| *n != b' ') {
                    v.push(&p[last..i]);
                    last = i + 1;
                }
            }
            v.push(&p[last..]);
            v
        })
        .collect();
    let tagged = parts
        .iter()
        .flat_map(|p| p.split(", "))
        .any(|w| TAG_WORDS.contains(&normalize(w).as_str()));
    if tagged {
        parts = parts.iter().flat_map(|p| p.split(", ")).collect();
    }
    parts
}

/// Bracket contents that are language / source tags, not titles.
const TAG_WORDS: &[&str] = &[
    "vf",
    "vostfr",
    "french",
    "subfrench",
    "multi",
    "multi subs",
    "readnfo",
    "vff",
    "vfq",
    "truefrench",
    "english",
    "japanese",
    "eng",
    "jap",
    "dual audio",
    "english dub",
    "cr",
    "adn",
    "nf",
    "amzn",
    "abema",
    "ova",
    "oav",
    "complete",
    "batch",
];

fn looks_like_title(s: &str) -> bool {
    let letters = s.chars().filter(|c| c.is_alphabetic()).count();
    if letters < 4 {
        return false;
    }
    let n = normalize(s);
    if TAG_WORDS.contains(&n.as_str()) {
        return false;
    }
    const TECH: &[&str] = &[
        "1080p",
        "720p",
        "480p",
        "2160p",
        "1080",
        "web",
        "dl",
        "webrip",
        "bd",
        "bdrip",
        "hevc",
        "x264",
        "x265",
        "h264",
        "h 264",
        "264",
        "265",
        "avc",
        "aac",
        "flac",
        "opus",
        "multi",
        "subs",
        "msubs",
        "dual",
        "audio",
        "cr",
        "adn",
        "amzn",
        "nf",
        "dsnp",
        "hidive",
        "batch",
        "vostfr",
        "eng",
        "raw",
        "tv",
        "end",
        "uncensored",
        "multisub",
        "av1",
        "10bit",
        "8bit",
        "hdr",
        "bluray",
        "remux",
        "ytb",
        "yt",
        "dub",
        "dubs",
        "sub",
        "2 0",
    ];
    let words: Vec<&str> = n.split(' ').collect();
    let tech = words.iter().filter(|w| TECH.contains(w)).count();
    tech * 2 < words.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn aod(mal: u32, title: &str, syn: &[&str], kind: AnimeType, eps: u32, year: i32) -> AodEntry {
        AodEntry {
            mal: Some(mal),
            anilist: Some(mal + 100_000),
            anidb: None,
            kitsu: None,
            title: title.into(),
            synonyms: syn.iter().map(|s| s.to_string()).collect(),
            kind,
            episodes: Some(eps),
            season: None,
            year: Some(year),
            status: None,
            score: None,
            duration_secs: None,
            tags: vec![],
            picture: None,
        }
    }

    fn fribb(mal: u32, tvdb: u32, season: i32, offset: i32, imdb: &str) -> FribbEntry {
        FribbEntry {
            kind: Some("TV".into()),
            mal: Some(mal),
            tvdb: Some(tvdb),
            imdb: if imdb.is_empty() {
                vec![]
            } else {
                vec![imdb.into()]
            },
            season: Some(FribbPair {
                tvdb: Some(season),
                tmdb: Some(season),
            }),
            episode_offset: (offset > 0).then_some(FribbPair {
                tvdb: Some(offset),
                tmdb: None,
            }),
            ..Default::default()
        }
    }

    fn index() -> AnimeIndex {
        let entries = vec![
            aod(40748, "Jujutsu Kaisen", &["JJK"], AnimeType::Tv, 24, 2020),
            aod(
                51009,
                "Jujutsu Kaisen 2nd Season",
                &[],
                AnimeType::Tv,
                23,
                2023,
            ),
            aod(
                57658,
                "Jujutsu Kaisen: Shimetsu Kaiyuu - Zenpen",
                &[],
                AnimeType::Tv,
                12,
                2026,
            ),
            aod(48561, "Jujutsu Kaisen 0", &[], AnimeType::Movie, 1, 2021),
            aod(36028, "Golden Kamuy", &[], AnimeType::Tv, 12, 2018),
            aod(
                37989,
                "Golden Kamuy 2nd Season",
                &[],
                AnimeType::Tv,
                12,
                2018,
            ),
            aod(
                40059,
                "Golden Kamuy 3rd Season",
                &[],
                AnimeType::Tv,
                12,
                2020,
            ),
            aod(
                50528,
                "Golden Kamuy 4th Season",
                &[],
                AnimeType::Tv,
                13,
                2023,
            ),
            aod(1, "Hunter x Hunter", &[], AnimeType::Tv, 62, 1999),
            aod(
                11061,
                "Hunter x Hunter (2011)",
                &["Hunter x Hunter"],
                AnimeType::Tv,
                148,
                2011,
            ),
        ];
        let f = vec![
            fribb(40748, 377543, 1, 0, "tt12343534"),
            fribb(51009, 377543, 2, 0, "tt12343534"),
            fribb(57658, 377543, 3, 0, "tt12343534"),
            FribbEntry {
                kind: Some("MOVIE".into()),
                mal: Some(48561),
                tvdb: Some(377543),
                imdb: vec!["tt14331144".into()],
                season: Some(FribbPair {
                    tvdb: Some(0),
                    tmdb: None,
                }),
                ..Default::default()
            },
            fribb(36028, 331753, 1, 0, ""),
            fribb(37989, 331753, 2, 0, ""),
            fribb(40059, 331753, 3, 0, ""),
            fribb(50528, 331753, 4, 0, ""),
        ];
        AnimeIndex::new(entries, f)
    }

    fn rel(t: &str, s: Option<i32>, e: Option<i32>, y: Option<i32>) -> Release {
        Release {
            titles: vec![t.into()],
            season: s,
            episode: e,
            year: y,
        }
    }

    fn mal_of(ix: &AnimeIndex, m: &AnimeMatch) -> u32 {
        ix.entries[m.entry].mal.unwrap()
    }

    #[test]
    fn normalizes() {
        assert_eq!(normalize("  JUJUTSU-KAISEN: S03!! "), "jujutsu kaisen s03");
        assert_eq!(normalize("Hero’s Party & Co."), "heros party and co");
        assert_eq!(normalize("Hunter×Hunter"), "hunterxhunter");
        assert_eq!(normalize("Ōkami-san"), "ōkami san");
        assert_eq!(normalize("葬送のフリーレン"), "葬送のフリーレン");
    }

    #[test]
    fn splits_season_suffixes() {
        assert_eq!(
            split_season("medalist season 2"),
            Some(("medalist".into(), 2))
        );
        assert_eq!(
            split_season("medalist 2nd season"),
            Some(("medalist".into(), 2))
        );
        assert_eq!(split_season("dr stone s4"), Some(("dr stone".into(), 4)));
        assert_eq!(split_season("kaiju no 8"), None);
        assert_eq!(split_season("season"), None);
    }

    #[test]
    fn plain_title_match() {
        let ix = index();
        let m = ix
            .match_release(&rel("Jujutsu Kaisen", None, Some(5), None))
            .unwrap();
        assert_eq!(mal_of(&ix, &m), 40748);
        assert_eq!(m.how, MatchHow::Title);
        assert_eq!(m.season, Some(1));
        assert!(
            ix.match_release(&rel("Unknown Show", None, None, None))
                .is_none()
        );
    }

    #[test]
    fn season_remap_through_fribb() {
        let ix = index();
        let m = ix
            .match_release(&rel("JUJUTSU KAISEN", Some(3), Some(1), None))
            .unwrap();
        assert_eq!(mal_of(&ix, &m), 57658);
        assert_eq!(m.how, MatchHow::SeasonRemap);
        assert_eq!(m.season, Some(3));
        // Season in the title itself.
        let m = ix
            .match_release(&rel("Golden Kamuy Season 4", None, Some(2), None))
            .unwrap();
        assert_eq!(mal_of(&ix, &m), 50528);
        // Season 1 stays.
        let m = ix
            .match_release(&rel("Golden Kamuy", Some(1), Some(2), None))
            .unwrap();
        assert_eq!(mal_of(&ix, &m), 36028);
    }

    #[test]
    fn absolute_episode_remap() {
        let ix = index();
        let m = ix
            .match_release(&rel("Golden Kamuy", None, Some(38), None))
            .unwrap();
        assert_eq!(mal_of(&ix, &m), 50528);
        assert_eq!(m.how, MatchHow::EpisodeRemap);
        assert_eq!(m.season, Some(4));
        assert_eq!(m.episode, Some(2));
    }

    #[test]
    fn year_and_episodes_rank_duplicates() {
        let ix = index();
        let m = ix
            .match_release(&rel("Hunter x Hunter", None, Some(3), Some(2011)))
            .unwrap();
        assert_eq!(mal_of(&ix, &m), 11061);
        let m = ix
            .match_release(&rel("Hunter x Hunter", None, Some(120), None))
            .unwrap();
        assert_eq!(mal_of(&ix, &m), 11061);
    }

    #[test]
    fn strong_ids_win() {
        let ix = index();
        let ids = extract_ids(
            "Info: https://myanimelist.net/anime/51009/Jujutsu_Kaisen_2nd_Season",
            &[],
        );
        assert_eq!(ids.mal, Some(51009));
        assert!(!ids.from_itag);
        let m = ix
            .identify(&ids, &rel("Golden Kamuy", None, None, None))
            .unwrap();
        assert_eq!(mal_of(&ix, &m), 51009);
        assert_eq!(m.how.method(), "desc_link");
        assert_eq!(m.season, Some(2));
        let ids = extract_ids("", &[("mal".into(), "40748".into())]);
        assert!(ids.from_itag);
        assert_eq!(
            ix.identify(&ids, &Release::default()).unwrap().how,
            MatchHow::Itag
        );
    }

    #[test]
    fn extracts_links() {
        let d = "AniDB: https://anidb.net/a18363 | AniList https://anilist.co/anime/172463/ x";
        let ids = extract_ids(d, &[]);
        assert_eq!(ids.anidb, Some(18363));
        assert_eq!(ids.anilist, Some(172463));
        assert_eq!(ids.mal, None);
        let d = "http://anidb.net/perl-bin/animedb.pl?show=anime&aid=69";
        assert_eq!(extract_ids(d, &[]).anidb, Some(69));
        assert!(extract_ids("anidb.net/anime/", &[]).is_empty());
        assert_eq!(
            extract_ids("", &[("imdb".into(), "tt1".into())]),
            AnimeIds::default()
        );
    }

    #[test]
    fn owner_of_shared_ids() {
        let ix = index();
        let first = ix.by_mal(40748).unwrap();
        let third = ix.by_mal(57658).unwrap();
        let movie = ix.by_mal(48561).unwrap();
        let c = ix.cross_ids(first).owned();
        assert_eq!(c.imdb.as_deref(), Some("tt12343534"));
        assert_eq!(c.tvdb, Some(377543));
        let c = ix.cross_ids(third);
        assert_eq!(c.season, Some(3));
        assert_eq!(c.imdb.as_deref(), Some("tt12343534"));
        assert_eq!(c.owned().imdb, None);
        assert_eq!(c.owned().tvdb, None);
        // The movie owns its own IMDb id but not the series' TVDB id.
        let c = ix.cross_ids(movie).owned();
        assert_eq!(c.imdb.as_deref(), Some("tt14331144"));
        assert_eq!(c.tvdb, None);
    }

    #[test]
    fn parses_aod_and_fribb() {
        let doc = concat!(
            r#"{"$schema":"x","license":{"name":"ODbL","url":"u"},"lastUpdate":"2026-10-25"}"#,
            "\n",
            r#"{"sources":["https://anidb.net/anime/23","https://anilist.co/anime/1","https://myanimelist.net/anime/1"],"title":"Cowboy Bebop","type":"TV","episodes":26,"status":"FINISHED","animeSeason":{"season":"SPRING","year":1998},"duration":{"value":1440,"unit":"SECONDS"},"score":{"median":8.6},"synonyms":["CB"],"tags":["action"]}"#,
            "\n",
            "not json\n"
        );
        let (h, e, bad) = parse_aod_jsonl(doc);
        assert_eq!(h.unwrap().last_update.as_deref(), Some("2026-10-25"));
        assert_eq!(bad, 1);
        assert_eq!(e.len(), 1);
        let e = &e[0];
        assert_eq!((e.mal, e.anilist, e.anidb), (Some(1), Some(1), Some(23)));
        assert_eq!(e.kind, AnimeType::Tv);
        assert_eq!(e.year, Some(1998));
        assert_eq!(e.duration_secs, Some(1440));
        let f = parse_fribb(
            r#"[{"type":"MOVIE","anidb_id":7,"mal_id":164,"imdb_id":["tt0119698"],"themoviedb_id":{"movie":[128]}},
                {"anidb_id":5458,"themoviedb_id":{"tv":44298},"tvdb_id":91021,"season":{"tvdb":1,"tmdb":1},"episode_offset":{"tvdb":11}}]"#,
        )
        .unwrap();
        assert_eq!(f[0].tmdb_movie(), Some(128));
        assert_eq!(f[0].imdb_id(), Some("tt0119698"));
        assert_eq!(f[1].tmdb_tv(), Some(44298));
        assert_eq!(f[1].episode_offset.unwrap().tvdb, Some(11));
    }

    #[test]
    fn dht_input_uses_largest_video() {
        let files = vec![
            TorrentFile {
                path: "Show/[G] Show - 01.mkv".into(),
                size: Some(100),
            },
            TorrentFile {
                path: "Show/[G] Show - 02.mkv".into(),
                size: Some(300),
            },
            TorrentFile {
                path: "Show/big.nfo".into(),
                size: Some(9000),
            },
        ];
        assert_eq!(release_name("", &files), "[G] Show - 02.mkv");
        assert_eq!(
            release_name("0123456789abcdef0123456789abcdef01234567", &files),
            "[G] Show - 02.mkv"
        );
        assert_eq!(release_name("[G] Show (01-02)", &files), "[G] Show (01-02)");
    }

    #[test]
    fn finds_alt_titles() {
        assert_eq!(
            alt_titles(
                "Dorohedoro S02E06 VOSTFR 720p WEB x264 AAC -Tsundere-Raws (CR) (Dorohedoro Season 2)"
            ),
            vec!["Dorohedoro Season 2"]
        );
        assert_eq!(
            alt_titles(
                "[Yameii] Rent-a-Girlfriend - S05E04 [English Dub] [CR WEB-DL 720p H264 AAC] [F7D24D3B] (Kanojo, Okarishimasu Season 5 | S5)"
            ),
            vec!["Kanojo, Okarishimasu Season 5"]
        );
        assert_eq!(
            alt_titles(
                "[SubsPlus+] Hell Mode - S02E01 (ADN WEB-DL 1080p AVC AAC) | HELL MODE: The Hardcore Gamer"
            ),
            vec!["HELL MODE: The Hardcore Gamer"]
        );
        assert!(alt_titles("[SubsPlease] Show - 01 (1080p) [ABCD1234].mkv").is_empty());
    }
}

//! Offline IMDb identification (D40): exact normalised-title lookup, ranked by type, year and
//! votes (metadata-enrichment.md §4.1: 93.6% correct on 1,328 imdb-tagged torrents).
//!
//! The server looks up every [`name_keys`] of a [`ParsedName`] in its `imdb_names` table and
//! hands the candidates to [`rank`]. Everything here is pure so it is unit-tested without a DB.

use unicode_normalization::UnicodeNormalization;
use unicode_normalization::char::is_combining_mark;

use crate::enrich::parse::ParsedName;
use crate::model::MediaKind;

/// Bump when ranking or normalisation changes: the server rematches every non-manual
/// `offline_title` link made by an older version (`torrent_media.matcher_version`).
pub const MATCHER_VERSION: i32 = 1;

/// IMDb `titleType`s kept by the import (everything else, incl. tvEpisode rows in
/// title.basics, shorts and video games, is dropped; episodes go to `imdb_episodes`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TitleType {
    Movie = 1,
    TvMovie = 2,
    Video = 3,
    TvSeries = 4,
    TvMiniSeries = 5,
    TvSpecial = 6,
}

impl TitleType {
    pub fn from_imdb(s: &str) -> Option<Self> {
        Some(match s {
            "movie" => TitleType::Movie,
            "tvMovie" => TitleType::TvMovie,
            "video" => TitleType::Video,
            "tvSeries" => TitleType::TvSeries,
            "tvMiniSeries" => TitleType::TvMiniSeries,
            "tvSpecial" => TitleType::TvSpecial,
            _ => return None,
        })
    }

    /// Stored as smallint in `imdb_titles.title_type`.
    pub fn code(self) -> i16 {
        self as i16
    }

    pub fn from_code(c: i16) -> Option<Self> {
        Some(match c {
            1 => TitleType::Movie,
            2 => TitleType::TvMovie,
            3 => TitleType::Video,
            4 => TitleType::TvSeries,
            5 => TitleType::TvMiniSeries,
            6 => TitleType::TvSpecial,
            _ => return None,
        })
    }

    pub fn is_series(self) -> bool {
        matches!(self, TitleType::TvSeries | TitleType::TvMiniSeries)
    }

    pub fn is_movie(self) -> bool {
        matches!(
            self,
            TitleType::Movie | TitleType::TvMovie | TitleType::Video
        )
    }

    /// `media.kind` for this type (tvSpecial counts as a film).
    pub fn media_kind(self) -> MediaKind {
        if self.is_series() {
            MediaKind::Series
        } else {
            MediaKind::Movie
        }
    }
}

/// Lowercase, strip diacritics (NFKD minus combining marks), `&` → "and", every other
/// non-alphanumeric run → one space. Letters of any script survive, so Cyrillic or CJK titles
/// still match themselves.
pub fn normalize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut space = false;
    for c in s.nfkd() {
        if is_combining_mark(c) {
            continue;
        }
        if c.is_alphanumeric() {
            if space && !out.is_empty() {
                out.push(' ');
            }
            space = false;
            out.extend(c.to_lowercase());
        } else if c == '&' {
            if !out.is_empty() {
                out.push(' ');
            }
            out.push_str("and");
            space = true;
        } else {
            space = true;
        }
    }
    out
}

/// Lookup keys for one title: the normal form, plus the form with apostrophes dropped instead of
/// split ("Ocean's Eleven" → "ocean s eleven" and "oceans eleven"; release names do both).
pub fn name_keys(title: &str) -> Vec<String> {
    let mut keys = Vec::with_capacity(2);
    let a = normalize(title);
    if !a.is_empty() {
        keys.push(a);
    }
    if title.contains(['\'', '’', '`']) {
        let b = normalize(&title.replace(['\'', '’', '`'], ""));
        if !b.is_empty() && !keys.contains(&b) {
            keys.push(b);
        }
    }
    keys
}

/// Every key worth looking up for a parsed name (title first, then alternatives).
pub fn query_keys(p: &ParsedName) -> Vec<String> {
    let mut keys: Vec<String> = Vec::new();
    for t in std::iter::once(&p.title).chain(p.alt_titles.iter()) {
        for k in name_keys(t) {
            if !keys.contains(&k) {
                keys.push(k);
            }
        }
    }
    keys
}

/// Which dataset name a candidate matched.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NameSource {
    Primary = 1,
    Original = 2,
    Aka = 3,
}

impl NameSource {
    pub fn from_code(c: i16) -> Self {
        match c {
            1 => NameSource::Primary,
            2 => NameSource::Original,
            _ => NameSource::Aka,
        }
    }
}

/// One `imdb_titles` row reached through an exact name hit.
#[derive(Clone, Debug, PartialEq)]
pub struct Candidate {
    /// tconst without "tt".
    pub id: u32,
    pub title_type: TitleType,
    pub year: Option<i32>,
    pub end_year: Option<i32>,
    pub votes: u32,
    pub source: NameSource,
    /// The candidate was found through `ParsedName.title` (not an alternative title).
    pub main_title: bool,
}

/// The chosen title and how sure we are.
#[derive(Clone, Debug, PartialEq)]
pub struct Ranked {
    pub id: u32,
    pub confidence: f32,
}

/// Year distance, treating a series' run (start..=end, open-ended if still running) as exact.
fn year_distance(p: &ParsedName, c: &Candidate) -> Option<i32> {
    let (y, start) = (p.year?, c.year?);
    if c.title_type.is_series() {
        let end = c.end_year.unwrap_or(i32::MAX);
        if (start..=end).contains(&y) {
            return Some(0);
        }
        return Some(if y < start { start - y } else { y - end });
    }
    Some((y - start).abs())
}

fn type_ok(p: &ParsedName, c: &Candidate) -> bool {
    if p.is_series {
        c.title_type.is_series()
    } else {
        c.title_type.is_movie()
    }
}

/// Selection score from the research matcher (type 1000, exact year 500, ±1 300, votes up to
/// 100), plus preferences for the main title over alternatives and for primary/original names
/// over akas (both smaller than a year step).
fn score(p: &ParsedName, c: &Candidate) -> f64 {
    let mut s = if type_ok(p, c) { 1000.0 } else { 0.0 };
    match year_distance(p, c) {
        Some(0) => s += 500.0,
        Some(1) => s += 300.0,
        _ => {}
    }
    if c.main_title {
        s += 150.0;
    }
    // A primary/original title hit beats a localised one in the same type/year tier.
    if c.source != NameSource::Aka {
        s += 120.0;
    }
    s + f64::from(c.votes.min(100_000)) / 1000.0
}

/// Confidence tier of a candidate before ambiguity penalties.
fn tier(p: &ParsedName, c: &Candidate) -> f32 {
    let ok = type_ok(p, c);
    match (ok, year_distance(p, c), p.year) {
        (true, Some(0), _) => 0.95,
        (true, Some(1), _) => 0.85,
        (true, None, None) => 0.75,
        (true, None, Some(_)) => 0.7,
        (true, Some(_), _) => 0.45,
        (false, Some(0), _) => 0.55,
        (false, _, _) => 0.35,
    }
}

/// Pick the best candidate. Confidence drops when another candidate sits in the same tier with
/// comparable votes (two films of the same name and year), and for alternative-title hits.
pub fn rank(p: &ParsedName, cands: &[Candidate]) -> Option<Ranked> {
    let best = cands.iter().max_by(|a, b| {
        score(p, a)
            .total_cmp(&score(p, b))
            .then(a.votes.cmp(&b.votes))
            .then(b.id.cmp(&a.id))
    })?;
    let mut conf = tier(p, best);
    let rival = cands
        .iter()
        // A title reached only through an aka does not cast doubt on a primary/original hit
        // (with akas on, many films have some other film's title as a localised name).
        .filter(|c| c.id != best.id && tier(p, c) >= conf && c.source as i16 <= best.source as i16)
        .map(|c| c.votes)
        .max();
    if let Some(rv) = rival {
        // Same tier: votes decide. Close votes = coin flip.
        conf -= if best.votes >= rv.saturating_mul(5) {
            0.05
        } else if best.votes >= rv.saturating_mul(2) {
            0.15
        } else {
            0.3
        };
    }
    if best.source == NameSource::Aka {
        // Localised titles are shared by remakes and per-country versions of a format
        // ("Les Schtroumpfs" 1981/2021, "Mask Singer"): without a year to tell them apart an
        // aka hit was right only ~55% of the time on the French YGG set (docs/ENRICHMENT.md §4).
        conf -= if p.year.is_some() { 0.05 } else { 0.3 };
    }
    if !best.main_title {
        conf -= 0.05;
    }
    Some(Ranked {
        id: best.id,
        confidence: conf.clamp(0.05, 0.99),
    })
}

/// Prefix fallback (the name has a subtitle IMDb's title lacks): accept only an unambiguous
/// same-type, exact-year candidate, at 0.55 (shown, but below every exact-title tier).
pub fn prefix_confidence(p: &ParsedName, cands: &[Candidate]) -> Option<Ranked> {
    let mut ok = cands
        .iter()
        .filter(|c| type_ok(p, c) && year_distance(p, c) == Some(0));
    let first = ok.next()?;
    if ok.next().is_some() {
        return None;
    }
    Some(Ranked {
        id: first.id,
        confidence: 0.55,
    })
}

/// Fuzzy (pg_trgm) fallback confidence: always below 0.6, and only for a same-type hit with the
/// exact year (anything looser was mostly wrong in the measurement, docs/ENRICHMENT.md §4).
pub fn fuzzy_confidence(p: &ParsedName, c: &Candidate, similarity: f32) -> Option<f32> {
    if !type_ok(p, c) || year_distance(p, c) != Some(0) {
        return None;
    }
    Some((0.3 * similarity / 0.6 + 0.25).min(0.55))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::enrich::parse::parse_name;

    fn cand(id: u32, t: TitleType, year: i32, votes: u32) -> Candidate {
        Candidate {
            id,
            title_type: t,
            year: Some(year),
            end_year: None,
            votes,
            source: NameSource::Primary,
            main_title: true,
        }
    }

    #[test]
    fn normalisation() {
        assert_eq!(
            normalize("Le fabuleux destin d'Amélie Poulain"),
            "le fabuleux destin d amelie poulain"
        );
        assert_eq!(normalize("  Fast & Furious 6 "), "fast and furious 6");
        assert_eq!(normalize("WALL·E"), "wall e");
        assert_eq!(normalize("Брат"), "брат");
        assert_eq!(normalize("ＦＵＬＬ　ＷＩＤＴＨ"), "full width");
        assert_eq!(
            name_keys("Ocean's Eleven"),
            vec!["ocean s eleven", "oceans eleven"]
        );
        assert_eq!(name_keys("..."), Vec::<String>::new());
    }

    #[test]
    fn ranking() {
        let p = parse_name("Dune.2021.2160p.WEB-DL").unwrap();
        let c = [
            cand(87182, TitleType::Movie, 1984, 180_000),
            cand(1160419, TitleType::Movie, 2021, 900_000),
            cand(142032, TitleType::TvMiniSeries, 2000, 20_000),
        ];
        let r = rank(&p, &c).unwrap();
        assert_eq!(r.id, 1160419);
        assert!(r.confidence >= 0.9, "{r:?}");

        // S/E markers prefer the series even against a more popular film.
        let p = parse_name("Fargo.S05E01.1080p").unwrap();
        let c = [
            cand(116282, TitleType::Movie, 1996, 700_000),
            cand(2802850, TitleType::TvSeries, 2014, 420_000),
        ];
        assert_eq!(rank(&p, &c).unwrap().id, 2802850);

        // No year, two same-type candidates with close votes: low confidence.
        let p = parse_name("Heat.1080p.BluRay").unwrap();
        let c = [
            cand(113277, TitleType::Movie, 1995, 70_000),
            cand(2, TitleType::Movie, 2013, 50_000),
        ];
        let r = rank(&p, &c).unwrap();
        assert_eq!(r.id, 113277);
        assert!(r.confidence < 0.5, "{r:?}");
        assert_eq!(rank(&p, &[]), None);
    }

    #[test]
    fn aka_rivals_are_weak() {
        let p = parse_name("Heat.1080p.BluRay").unwrap();
        let mut aka = cand(2, TitleType::Movie, 2013, 50_000);
        aka.source = NameSource::Aka;
        let c = [cand(113277, TitleType::Movie, 1995, 70_000), aka.clone()];
        let r = rank(&p, &c).unwrap();
        assert_eq!(r.id, 113277);
        assert!(r.confidence >= 0.7, "{r:?}");
        // An aka-only hit without a year is not shown.
        let r = rank(&p, &[aka]).unwrap();
        assert!(
            r.confidence < crate::enrich::MIN_DISPLAY_CONFIDENCE,
            "{r:?}"
        );
    }

    #[test]
    fn series_runs_count_as_exact_year() {
        let p = parse_name("Peacemaker 2025 S02E01 1080p").unwrap();
        let mut c = cand(13146488, TitleType::TvSeries, 2022, 200_000);
        c.end_year = None;
        assert_eq!(year_distance(&p, &c), Some(0));
        c.end_year = Some(2023);
        assert_eq!(year_distance(&p, &c), Some(2));
    }

    #[test]
    fn fuzzy_needs_type_and_year() {
        let p = parse_name("Oppenhiemer.2023.1080p").unwrap();
        let c = cand(15398776, TitleType::Movie, 2023, 900_000);
        let conf = fuzzy_confidence(&p, &c, 0.7).unwrap();
        assert!(conf < 0.6);
        let c2 = cand(15398776, TitleType::Movie, 2020, 900_000);
        assert_eq!(fuzzy_confidence(&p, &c2, 0.9), None);
    }
}

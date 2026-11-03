//! Search query types and text sanitizers (ARCHITECTURE.md §5).

use serde::{Deserialize, Serialize};

use crate::model::{Category, MediaKind, TorrentSummary};

pub const DEFAULT_LIMIT: u32 = 50;
pub const MAX_LIMIT: u32 = 200;
/// Deepest result offset a search accepts (D110): 50 pages of [`MAX_LIMIT`]. Every result up to
/// `offset + limit` is kept while ranking (the index's top-k heaps, the SQL fallback's OFFSET
/// scan), so an unbounded offset held every matching document in memory. Above it the server
/// answers 400; nobody scrolls that far, narrower filters find the rest.
pub const MAX_OFFSET: u32 = 10_000;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct SearchQuery {
    pub text: String,
    pub categories: Vec<Category>,
    pub min_size: Option<u64>,
    pub max_size: Option<u64>,
    pub since: Option<i64>,
    pub until: Option<i64>,
    pub min_seeders: Option<u32>,
    pub hide_dead: bool,
    /// None = use setting `trust.threshold`.
    pub min_trust: Option<u8>,
    pub sort: Sort,
    pub descending: bool,
    pub limit: u32,
    pub offset: u32,
    pub include_hidden: bool,
    /// Show adult documents (D52). Default false: documents whose canonical category is
    /// `porn`, or with a DHT row classified `porn`, are left out — unless `categories`
    /// selects `porn` (choosing it is consent). Older apps never send it, so they get the
    /// default too.
    pub include_adult: bool,
    // Release and media filters (D44). Within one field any value matches; fields combine
    // with AND. Values are the lowercase codes of `release::ReleaseAttrs` (docs/FILTERS.md).
    // Older servers ignore these fields, so the app only offers them when the server says it
    // has them (`Health::features`).
    /// Audio languages ("fr", "fr-ca", "en", "original", "multi", …).
    pub languages: Vec<String>,
    pub subtitles: Vec<String>,
    /// "2160p", "1080p", …, "sd".
    pub resolutions: Vec<String>,
    /// "remux", "bluray", "web-dl", …
    pub sources: Vec<String>,
    /// "h265", "h264", "av1", …
    pub video_codecs: Vec<String>,
    /// "dv", "hdr10+", "hdr10", "hlg".
    pub hdr: Vec<String>,
    /// Audio codecs ("truehd", "atmos", "eac3", …).
    pub audio: Vec<String>,
    /// Genres of the linked media, lowercase ("drama", "comedy", …).
    pub genres: Vec<String>,
    pub media_kinds: Vec<MediaKind>,
    /// Minimum IMDb-style rating (0–10) of the linked media; unrated never passes.
    pub min_rating: Option<f32>,
    /// Year range: the linked media's year, else the year in the name.
    pub min_year: Option<i32>,
    pub max_year: Option<i32>,
    /// Sources (D51): `nostr`, `dht`; any of them (a torrent known from both matches either).
    /// Empty = every source. Older servers ignore it, so the app offers the chip only when
    /// the server lists `api::features::ORIGIN`.
    pub origins: Vec<String>,
    // Header-probe filters (D122): what a crawl worker's probe read from the files themselves
    // (docs/PROBES.md), not the name. Only probed torrents can match. Within one field any
    // value matches; fields combine with AND, with each other and with the D44 filters. Older
    // servers ignore them, so the app offers them only when the server lists
    // `api::features::PROBE_FILTERS`.
    /// Audio track languages, ISO 639-2/B (`fre`, `eng`, `jpn`; 639-1 and 639-2/T codes are
    /// accepted and normalised by the server: `fr`, `fra` → `fre`).
    pub probe_languages: Vec<String>,
    /// Subtitle track languages, as `probe_languages`.
    pub probe_subtitles: Vec<String>,
    /// Picture class of the main video track: `2160p` (8K included), `1080p`, `720p`, `576p`,
    /// `480p`, `sd`. "4K and up" is `["2160p"]`, "1080p and up" `["2160p", "1080p"]`.
    pub probe_resolutions: Vec<String>,
    /// FFmpeg codec names of the main video track (`hevc`, `h264`, `av1`, `vp9`, …).
    pub probe_video_codecs: Vec<String>,
    /// `any` (any HDR format), `dolby_vision`, `hdr10`, `hdr10_plus`, `hlg`.
    pub probe_hdr: Vec<String>,
    /// Audio codecs of the main file (`truehd`, `eac3`, `dts`, `aac`, …) and the object-audio
    /// marks `atmos`, `dts_x`.
    pub probe_audio: Vec<String>,
}

/// Facet names of the header-probe filters (D122), in `SearchQuery::probe_filters` order.
pub const PROBE_FACETS: [&str; 6] = [
    "probe_language",
    "probe_subtitles",
    "probe_resolution",
    "probe_codec",
    "probe_hdr",
    "probe_audio",
];

impl SearchQuery {
    /// The header-probe filters set (D122) as (facet name, lowercase values, deduplicated).
    /// Language codes are only lowercased here; the server normalises them to ISO 639-2/B.
    pub fn probe_filters(&self) -> Vec<(&'static str, Vec<String>)> {
        let lower = |v: &[String]| -> Vec<String> {
            let mut out: Vec<String> = Vec::new();
            for s in v {
                let s = s.trim().to_lowercase();
                if !s.is_empty() && !out.contains(&s) {
                    out.push(s);
                }
            }
            out
        };
        let mut out: Vec<(&'static str, Vec<String>)> = PROBE_FACETS
            .iter()
            .zip([
                &self.probe_languages,
                &self.probe_subtitles,
                &self.probe_resolutions,
                &self.probe_video_codecs,
                &self.probe_hdr,
                &self.probe_audio,
            ])
            .map(|(name, v)| (*name, lower(v)))
            .collect();
        out.retain(|(_, v)| !v.is_empty());
        out
    }

    /// Needs a server with `api::features::PROBE_FILTERS`: an older one would ignore them and
    /// answer unfiltered.
    pub fn needs_probe_filter_feature(&self) -> bool {
        !self.probe_filters().is_empty()
    }
}

impl SearchQuery {
    /// The release/media filters set (D44) as (facet name, lowercase values), the facet each
    /// one filters on in the search index.
    pub fn attr_filters(&self) -> Vec<(&'static str, Vec<String>)> {
        let lower = |v: &[String]| -> Vec<String> {
            v.iter()
                .map(|s| s.trim().to_lowercase())
                .filter(|s| !s.is_empty())
                .collect()
        };
        let mut out = vec![
            ("language", lower(&self.languages)),
            ("subtitles", lower(&self.subtitles)),
            ("resolution", lower(&self.resolutions)),
            ("source", lower(&self.sources)),
            ("codec", lower(&self.video_codecs)),
            ("hdr", lower(&self.hdr)),
            ("audio", lower(&self.audio)),
            ("genre", lower(&self.genres)),
            (
                "media_kind",
                self.media_kinds
                    .iter()
                    .map(|k| k.as_str().to_string())
                    .collect(),
            ),
        ];
        out.retain(|(_, v)| !v.is_empty());
        out
    }

    /// Any D44 filter set (release attributes, genre, rating, year).
    pub fn has_attr_filters(&self) -> bool {
        !self.attr_filters().is_empty()
            || self.min_rating.is_some()
            || self.min_year.is_some()
            || self.max_year.is_some()
    }

    /// Needs a server with `api::features::FILTERS`: an older one would ignore the D44
    /// filters (unfiltered results) and reject `Sort::Rating`.
    pub fn needs_filter_feature(&self) -> bool {
        self.has_attr_filters() || self.sort == Sort::Rating
    }
}

impl SearchQuery {
    /// The source filter (D51), lowercased, known values only (`nostr`, `dht`), deduplicated;
    /// empty = every source. Both values together select everything, so that is empty too.
    pub fn origin_filter(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for o in &self.origins {
            let o = o.trim().to_lowercase();
            if matches!(o.as_str(), "nostr" | "dht") && !out.contains(&o) {
                out.push(o);
            }
        }
        if out.len() == 2 {
            out.clear();
        }
        out
    }

    /// The empty query's browse rule (D76): with no text (no words, no media token) and no
    /// source filter, DHT-only torrents are listed only once a seed check found peers
    /// (seeders, or DHT peers for a torrent without trackers, > 0). Fresh unchecked DHT
    /// torrents would otherwise flood the newest-first list. Any sort; Nostr results and any
    /// torrent with a Nostr event are unaffected; a text search or an explicit source filter
    /// (`origins` = `dht` alone) returns every DHT result. So does a header-probe filter
    /// (D122): only probed torrents match it, every one of them is a DHT find whose peers sent
    /// pieces, and most were never seed-checked, so the rule would hide nearly all of them.
    pub fn browse_hides_unchecked_dht(&self) -> bool {
        self.text.trim().is_empty()
            && self.origin_filter().is_empty()
            && self.probe_filters().is_empty()
    }

    /// Adult documents are left out of this query (D52).
    pub fn hides_adult(&self) -> bool {
        !self.include_adult && !self.categories.contains(&Category::Porn)
    }

    /// Effective limit (default 50, max 200).
    pub fn effective_limit(&self) -> u32 {
        match self.limit {
            0 => DEFAULT_LIMIT,
            n => n.min(MAX_LIMIT),
        }
    }

    /// Relevance only makes sense with text; browse falls back to Date.
    pub fn effective_sort(&self) -> Sort {
        if self.sort == Sort::Relevance && self.text.trim().is_empty() {
            Sort::Date
        } else {
            self.sort
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Sort {
    #[default]
    Relevance,
    Date,
    Size,
    Seeders,
    /// Rating of the linked media (D44), unrated last. Older servers reject it.
    Rating,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct SearchPage {
    pub hits: Vec<TorrentSummary>,
    pub total: u64,
    /// "server" | "local"
    pub source: String,
    /// `total` is a lower bound ("1,000+"): the exact count was too expensive (D43). Older
    /// servers never send it.
    #[serde(default)]
    pub total_is_lower_bound: bool,
}

/// Split user text into safe word tokens (letters/digits in any script), lowercased.
pub fn tokens(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(|t| t.to_lowercase())
        .take(16)
        .collect()
}

/// SQLite FTS5 MATCH expression: every token quoted, last one as prefix. None if no tokens.
pub fn fts5_match(text: &str) -> Option<String> {
    let toks = tokens(text);
    if toks.is_empty() {
        return None;
    }
    let n = toks.len();
    Some(
        toks.iter()
            .enumerate()
            .map(|(i, t)| {
                let q = format!("\"{}\"", t.replace('"', "\"\""));
                if i + 1 == n { format!("{q}*") } else { q }
            })
            .collect::<Vec<_>>()
            .join(" "),
    )
}

/// Postgres `to_tsquery('simple', ..)` expression: tokens ANDed, last one prefix. None if no tokens.
/// Shortest last word that is still matched as a prefix (typed-as-you-go search).
pub const MIN_PREFIX_CHARS: usize = 3;

pub fn pg_tsquery(text: &str) -> Option<String> {
    let toks = tokens(text);
    if toks.is_empty() {
        return None;
    }
    let n = toks.len();
    Some(
        toks.iter()
            .enumerate()
            .map(|(i, t)| {
                let t = t.replace(['\'', '\\', ':', '&', '|', '!', '(', ')', '<', '>'], "");
                // A 1–2 letter prefix matches most of the index (`'o':*` took 42 s on 400k rows).
                if i + 1 == n && t.chars().count() >= MIN_PREFIX_CHARS {
                    format!("'{t}':*")
                } else {
                    format!("'{t}'")
                }
            })
            .collect::<Vec<_>>()
            .join(" & "),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizers() {
        assert_eq!(
            fts5_match("Big.Buck \"bun"),
            Some("\"big\" \"buck\" \"bun\"*".into())
        );
        assert_eq!(fts5_match("  ..  "), None);
        assert_eq!(pg_tsquery("ubuntu 24"), Some("'ubuntu' & '24'".into()));
        assert_eq!(pg_tsquery("ubuntu 240"), Some("'ubuntu' & '240':*".into()));
        assert_eq!(pg_tsquery("o"), Some("'o'".into()));
        assert_eq!(pg_tsquery("onl"), Some("'onl':*".into()));
        assert_eq!(tokens("Été 1080p"), vec!["été", "1080p"]);
    }

    #[test]
    fn adult_hidden_by_default() {
        let q = SearchQuery::default();
        assert!(q.hides_adult());
        let q: SearchQuery = serde_json::from_str(r#"{"text":"x"}"#).unwrap();
        assert!(q.hides_adult(), "older apps send no include_adult");
        let q = SearchQuery {
            include_adult: true,
            ..Default::default()
        };
        assert!(!q.hides_adult());
        let q = SearchQuery {
            categories: vec![Category::Video, Category::Porn],
            ..Default::default()
        };
        assert!(!q.hides_adult(), "selecting porn is consent");
    }

    #[test]
    fn probe_filters_default_and_lowercase() {
        // Older apps and saved searches send none of the D122 fields.
        let q: SearchQuery = serde_json::from_str(r#"{"text":"x","languages":["fr"]}"#).unwrap();
        assert!(q.probe_filters().is_empty());
        assert!(!q.needs_probe_filter_feature());
        let q: SearchQuery = serde_json::from_str(
            r#"{"probe_languages":[" FRE ","fre",""],"probe_hdr":["any"],"probe_audio":["Atmos"]}"#,
        )
        .unwrap();
        assert_eq!(
            q.probe_filters(),
            vec![
                ("probe_language", vec!["fre".to_string()]),
                ("probe_hdr", vec!["any".to_string()]),
                ("probe_audio", vec!["atmos".to_string()]),
            ]
        );
        assert!(q.needs_probe_filter_feature());
        assert!(!q.has_attr_filters(), "probe filters are not D44 filters");
    }
}

//! Metadata enrichment (D40): identify the film/series/anime behind a torrent from data that is
//! not on Nostr or the DHT. See docs/ENRICHMENT.md and docs/research/metadata-enrichment.md.
//!
//! - [`ids`]: strong IDs from NIP-35 `i` tags and description links (always compiled; no deps).
//! - [`parse`]: release-name parsing with `parse-torrent-title` (feature `enrich`).
//! - [`imdb_match`]: ranking of IMDb dataset candidates for a parsed name (feature `enrich`).
//!
//! The server stores the results in `media` / `torrent_media`; nothing here is ever published.

pub mod ids;
#[cfg(feature = "enrich")]
pub mod imdb_match;
#[cfg(feature = "enrich")]
pub mod parse;

/// `torrent_media.method` values (the column has a CHECK on exactly these).
pub mod method {
    /// NIP-35 `i` tag (`imdb:tt…`, `tmdb:movie:N`, …).
    pub const ITAG: &str = "itag";
    /// A link in the description (imdb.com/title/tt…, themoviedb.org/movie/N, …).
    pub const DESC_LINK: &str = "desc_link";
    /// Offline title match against a dataset (IMDb, anime-offline-database).
    pub const OFFLINE_TITLE: &str = "offline_title";
    /// Search through an online API.
    pub const API_SEARCH: &str = "api_search";
    /// Set by hand; enrichment never overwrites it.
    pub const MANUAL: &str = "manual";
}

/// Links below this confidence are kept (they help rematching and debugging) but not shown:
/// `TorrentDetail.media` and the `TorrentSummary.media_*` fields stay None. Measured precision
/// per confidence bucket is in docs/ENRICHMENT.md §4.
pub const MIN_DISPLAY_CONFIDENCE: f32 = 0.5;

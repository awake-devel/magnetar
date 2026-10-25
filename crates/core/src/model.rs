//! Domain model shared by every component (ARCHITECTURE.md §3).
//!
//! All hex strings are lowercase. Times are unix seconds.

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::query::SearchQuery;

/// 64 lowercase hex chars (event id or pubkey).
pub type Hex32 = String;
/// 40 lowercase hex chars (BitTorrent v1 infohash).
pub type InfoHash = String;

pub const KIND_PROFILE: u16 = 0;
pub const KIND_CONTACTS: u16 = 3;
pub const KIND_REPORT: u16 = 1984;
pub const KIND_TORRENT: u16 = 2003;
pub const KIND_TORRENT_COMMENT: u16 = 2004;
pub const KIND_COMMENT: u16 = 1111;
pub const KIND_MUTE_LIST: u16 = 10000;
pub const KIND_RELAY_LIST: u16 = 10002;
pub const KIND_KIND_MUTE_SET: u16 = 30007;

/// A raw nostr event exactly as received (tags and content untouched, see nostr-nips.md §1.2).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RawEvent {
    pub id: Hex32,
    pub pubkey: Hex32,
    pub created_at: i64,
    pub kind: u16,
    pub tags: Vec<Vec<String>>,
    pub content: String,
    pub sig: String,
}

impl RawEvent {
    /// Parse NIP-01 JSON. Does NOT verify.
    pub fn from_json(s: &str) -> Result<Self> {
        Ok(serde_json::from_str(s)?)
    }

    /// NIP-01 wire JSON.
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("RawEvent serializes")
    }

    /// Recompute the id and verify the BIP340 signature (D5).
    pub fn verify(&self) -> Result<()> {
        let ev = nostr::prelude::Event::from_json(self.to_json())
            .map_err(|_| Error::Rejected(Rejection::BadJson))?;
        if !ev.verify_id() {
            return Err(Error::Rejected(Rejection::BadId));
        }
        if !ev.verify_signature() {
            return Err(Error::Rejected(Rejection::BadSig));
        }
        Ok(())
    }

    /// First tag with this name.
    pub fn tag(&self, name: &str) -> Option<&Vec<String>> {
        self.tags
            .iter()
            .find(|t| t.first().map(String::as_str) == Some(name))
    }

    /// Id as 32 raw bytes.
    pub fn id_bytes(&self) -> Result<[u8; 32]> {
        let mut out = [0u8; 32];
        hex::decode_to_slice(&self.id, &mut out).map_err(|_| Error::Rejected(Rejection::BadId))?;
        Ok(out)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct TorrentFile {
    pub path: String,
    pub size: Option<u64>,
}

/// Result of parsing a kind 2003 (D4). Produced by `nip35::parse_torrent`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ParsedTorrent {
    pub event_id: Hex32,
    pub pubkey: Hex32,
    pub created_at: i64,
    pub infohash: InfoHash,
    pub infohash_v2: Option<String>,
    pub title: String,
    pub description: String,
    pub files: Vec<TorrentFile>,
    pub total_size: Option<u64>,
    pub trackers: Vec<String>,
    pub category: Category,
    pub category_path: Vec<String>,
    pub hashtags: Vec<String>,
    pub external_ids: Vec<(String, String)>,
    pub source_url: Option<String>,
    pub published_at: Option<i64>,
    pub pow_bits: u8,
    pub pow_committed: Option<u8>,
}

impl ParsedTorrent {
    /// Space-joined file paths used for full-text indexing (capped at 32 KiB).
    pub fn files_text(&self) -> String {
        let mut s = String::new();
        for f in &self.files {
            if s.len() > 32 * 1024 {
                break;
            }
            if !s.is_empty() {
                s.push(' ');
            }
            s.push_str(&f.path);
        }
        s
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    Video,
    Audio,
    Application,
    Game,
    Porn,
    Anime,
    Book,
    Picture,
    Other,
}

impl Category {
    pub const ALL: [Category; 9] = [
        Category::Video,
        Category::Audio,
        Category::Application,
        Category::Game,
        Category::Porn,
        Category::Anime,
        Category::Book,
        Category::Picture,
        Category::Other,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            Category::Video => "video",
            Category::Audio => "audio",
            Category::Application => "application",
            Category::Game => "game",
            Category::Porn => "porn",
            Category::Anime => "anime",
            Category::Book => "book",
            Category::Picture => "picture",
            Category::Other => "other",
        }
    }
}

impl std::str::FromStr for Category {
    type Err = Error;
    fn from_str(s: &str) -> Result<Self> {
        Category::ALL
            .into_iter()
            .find(|c| c.as_str() == s)
            .ok_or_else(|| Error::Invalid(format!("unknown category {s}")))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Comment {
    pub event_id: Hex32,
    pub pubkey: Hex32,
    pub created_at: i64,
    /// 2004 or 1111.
    pub kind: u16,
    pub torrent_event_id: Hex32,
    pub content: String,
}

/// What ingest decided about one verified raw event.
#[derive(Clone, Debug)]
pub enum IngestItem {
    Torrent(Box<ParsedTorrent>, RawEvent),
    /// Comment whose target is a known (or at least referenced) torrent. The orphan check
    /// (D7) needs the store and is done by `ingest::ingest_into`.
    Comment(Comment, RawEvent),
    ContactList(RawEvent),
    MuteList(RawEvent),
    Report(RawEvent),
    RelayList(RawEvent),
    Profile(RawEvent),
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Rejection {
    BadJson,
    BadId,
    BadSig,
    FutureDated,
    UnsupportedKind,
    NoInfohash,
    OrphanComment,
    TooLarge,
}

impl Rejection {
    /// NIP-01 OK message for this rejection.
    pub fn ok_message(&self) -> &'static str {
        match self {
            Rejection::BadJson => "invalid: bad json",
            Rejection::BadId => "invalid: event id does not match",
            Rejection::BadSig => "invalid: bad signature",
            Rejection::FutureDated => "invalid: created_at too far in the future",
            Rejection::UnsupportedKind => "blocked: kind not accepted",
            Rejection::NoInfohash => "invalid: no valid infohash",
            Rejection::OrphanComment => "blocked: comment references unknown torrent",
            Rejection::TooLarge => "invalid: event too large",
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LivenessSource {
    Udp,
    Http,
    Dht,
    DhtValues,
    None,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Liveness {
    pub infohash: InfoHash,
    pub seeders: Option<u32>,
    pub leechers: Option<u32>,
    pub completed: Option<u32>,
    pub dht_peers_est: Option<u32>,
    pub source: LivenessSource,
    pub trackers_ok: u16,
    pub trackers_failed: u16,
    pub checked_at: i64,
    pub expires_at: i64,
}

impl Liveness {
    /// Best available peer count: max(seeders, dht estimate).
    pub fn best_peers(&self) -> Option<u32> {
        match (self.seeders, self.dht_peers_est) {
            (None, None) => None,
            (a, b) => Some(a.unwrap_or(0).max(b.unwrap_or(0))),
        }
    }

    /// Fresh, answered, and nobody seeding. Unknown is never dead (D17).
    pub fn is_dead(&self, now: i64) -> bool {
        now < self.expires_at && self.best_peers() == Some(0)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct TrustFactor {
    pub code: String,
    pub delta: i32,
    pub detail: String,
    pub evidence: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct TrustScore {
    pub pubkey: Hex32,
    pub score: u8,
    pub factors: Vec<TrustFactor>,
    pub root: Option<Hex32>,
    pub computed_at: i64,
    pub graph_version: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct RelayInfo {
    pub url: String,
    /// "seed" | "nip65" | "kind3" | "hint" | "nip66" | "manual"
    pub source: String,
    pub enabled: bool,
    pub last_ok: Option<i64>,
    pub last_err: Option<String>,
    pub consecutive_failures: u32,
    pub next_try_at: i64,
    pub events_found: u64,
    pub supports_neg: Option<bool>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Cursor {
    pub relay: String,
    pub kind: u16,
    pub newest_seen: Option<i64>,
    pub backfill_until: Option<i64>,
    pub backfill_done: bool,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OverrideAction {
    Block,
    Trust,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SavedSearch {
    pub id: i64,
    pub name: String,
    pub query: SearchQuery,
    pub notify: bool,
    pub last_checked: Option<i64>,
    pub last_seen_created_at: Option<i64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct TorrentSummary {
    pub infohash: InfoHash,
    pub event_id: Hex32,
    pub title: String,
    pub category: Category,
    pub total_size: Option<u64>,
    pub file_count: u32,
    pub created_at: i64,
    pub pubkey: Hex32,
    pub npub: String,
    pub seeders: Option<u32>,
    pub leechers: Option<u32>,
    pub dead: Option<bool>,
    pub trust: u8,
    pub pow_bits: u8,
    pub publisher_count: u32,
    pub source_url: Option<String>,
    pub rank: f32,
    pub pinned: bool,
    pub hidden: bool,
    /// Linked title (D40), only when the link is confident enough to show.
    #[serde(default)]
    pub media_title: Option<String>,
    #[serde(default)]
    pub media_year: Option<i32>,
    #[serde(default)]
    pub media_kind: Option<MediaKind>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct EventRef {
    pub event_id: Hex32,
    pub pubkey: Hex32,
    pub npub: String,
    pub created_at: i64,
    pub title: String,
    pub trust: u8,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct TorrentDetail {
    pub summary: TorrentSummary,
    pub description: String,
    pub files: Vec<TorrentFile>,
    pub trackers: Vec<String>,
    pub hashtags: Vec<String>,
    pub category_path: Vec<String>,
    pub external_ids: Vec<(String, String)>,
    pub magnet: String,
    pub liveness: Option<Liveness>,
    pub trust: TrustScore,
    pub events: Vec<EventRef>,
    pub comments: Vec<Comment>,
    /// Identified film/series/anime (D40); None when unknown or below the display threshold.
    #[serde(default)]
    pub media: Option<MediaInfo>,
}

/// What a `media` row describes (D40).
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum MediaKind {
    Movie,
    Series,
    Anime,
}

impl MediaKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            MediaKind::Movie => "movie",
            MediaKind::Series => "series",
            MediaKind::Anime => "anime",
        }
    }
}

impl std::str::FromStr for MediaKind {
    type Err = Error;
    fn from_str(s: &str) -> Result<Self> {
        match s {
            "movie" => Ok(MediaKind::Movie),
            "series" => Ok(MediaKind::Series),
            "anime" => Ok(MediaKind::Anime),
            _ => Err(Error::Invalid(format!("unknown media kind {s}"))),
        }
    }
}

/// A torrent's identified title, from the server's `media` + `torrent_media` rows (D40).
/// Enriched data is never republished to Nostr (IMDb/TMDB terms, metadata-enrichment.md §5).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct MediaInfo {
    pub media_id: i64,
    pub kind: MediaKind,
    pub title: String,
    #[serde(default)]
    pub original_title: Option<String>,
    #[serde(default)]
    pub year: Option<i32>,
    #[serde(default)]
    pub end_year: Option<i32>,
    #[serde(default)]
    pub genres: Vec<String>,
    #[serde(default)]
    pub rating: Option<f32>,
    #[serde(default)]
    pub votes: Option<u32>,
    #[serde(default)]
    pub runtime_min: Option<i32>,
    /// Season/episode of this torrent (from its name or link), not of the title.
    #[serde(default)]
    pub season: Option<i32>,
    #[serde(default)]
    pub episode: Option<i32>,
    #[serde(default)]
    pub imdb: Option<String>,
    /// (Movie, id) = themoviedb.org/movie/id, (Series, id) = themoviedb.org/tv/id.
    #[serde(default)]
    pub tmdb: Option<(MediaKind, u32)>,
    #[serde(default)]
    pub mal: Option<u32>,
    #[serde(default)]
    pub anilist: Option<u32>,
    /// "itag" | "desc_link" | "offline_title" | "api_search" | "manual"
    pub method: String,
    pub confidence: f32,
    #[serde(default)]
    pub overview: Option<String>,
    /// Server-relative URL such as `/api/v1/media/{id}/poster`.
    #[serde(default)]
    pub poster: Option<String>,
    /// TMDB episode name when `season` and `episode` are known (D42, additive).
    #[serde(default)]
    pub episode_title: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct PublisherStats {
    pub first_seen: Option<i64>,
    pub torrent_count: u64,
    pub has_profile: bool,
    pub has_relay_list: bool,
    /// Max kind-2003 events in any rolling 1 h window.
    pub max_per_hour: u32,
    /// Any of the publisher's torrents carries a proxy tag (NIP-48 bridge).
    pub is_bridge: bool,
    /// Comments by root/D1 members on this publisher's torrents (filled by the trust engine
    /// through `Store::comment_authors_on`; stores may leave 0).
    pub wot_comments: u32,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct StoreStats {
    pub torrents: u64,
    pub events: u64,
    pub publishers: u64,
    pub comments: u64,
    pub relays: u64,
    pub unsynced: u64,
    pub db_bytes: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct DownloadInfo {
    pub id: u64,
    pub name: String,
    pub infohash: InfoHash,
    /// "initializing" | "live" | "paused" | "error" | "finished"
    pub state: String,
    pub progress: f32,
    pub downloaded: u64,
    pub total: u64,
    pub down_bps: u64,
    pub up_bps: u64,
    pub peers: u32,
    pub error: Option<String>,
    pub output_dir: String,
}

/// Lowercase 64-hex check.
pub fn is_hex32(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// hex pubkey -> npub (bech32). Returns the hex unchanged if it is invalid.
pub fn npub(hex_pubkey: &str) -> String {
    use nostr::nips::nip19::ToBech32;
    nostr::prelude::PublicKey::from_hex(hex_pubkey)
        .ok()
        .and_then(|pk| pk.to_bech32().ok())
        .unwrap_or_else(|| hex_pubkey.to_string())
}

/// hex | npub | nprofile | `nostr:` URI -> lowercase hex pubkey.
pub fn parse_pubkey(input: &str) -> Result<Hex32> {
    nostr::prelude::PublicKey::parse(input.trim())
        .map(|pk| pk.to_hex())
        .map_err(|e| Error::Invalid(format!("pubkey: {e}")))
}

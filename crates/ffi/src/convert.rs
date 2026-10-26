//! core <-> Ffi type conversions and error mapping.

use magnetar_core::Error;
use magnetar_core::api::{FacetSet, LivenessHistory, LivenessStats, MediaReleases};
use magnetar_core::model::*;
use magnetar_core::query::{SearchPage, SearchQuery, Sort};
use magnetar_core::release::{self, ReleaseAttrs};
use serde::{Deserialize, Serialize};

use crate::*;

impl From<Error> for FfiError {
    fn from(e: Error) -> Self {
        match e {
            Error::Invalid(msg) => FfiError::Invalid { msg },
            Error::Rejected(r) => FfiError::Invalid {
                msg: r.ok_message().to_string(),
            },
            Error::Network(msg) | Error::Timeout(msg) => FfiError::Network { msg },
            Error::Relay { relay, msg } => FfiError::Network {
                msg: format!("{relay}: {msg}"),
            },
            Error::Server { status, msg } => FfiError::Server { status, msg },
            Error::Store(msg) => FfiError::Db { msg },
            Error::Other(msg) => FfiError::Other { msg },
        }
    }
}

impl From<FfiCategory> for Category {
    fn from(c: FfiCategory) -> Self {
        match c {
            FfiCategory::Video => Category::Video,
            FfiCategory::Audio => Category::Audio,
            FfiCategory::Application => Category::Application,
            FfiCategory::Game => Category::Game,
            FfiCategory::Porn => Category::Porn,
            FfiCategory::Anime => Category::Anime,
            FfiCategory::Book => Category::Book,
            FfiCategory::Picture => Category::Picture,
            FfiCategory::Other => Category::Other,
        }
    }
}

impl From<Category> for FfiCategory {
    fn from(c: Category) -> Self {
        match c {
            Category::Video => FfiCategory::Video,
            Category::Audio => FfiCategory::Audio,
            Category::Application => FfiCategory::Application,
            Category::Game => FfiCategory::Game,
            Category::Porn => FfiCategory::Porn,
            Category::Anime => FfiCategory::Anime,
            Category::Book => FfiCategory::Book,
            Category::Picture => FfiCategory::Picture,
            Category::Other => FfiCategory::Other,
        }
    }
}

impl From<FfiSort> for Sort {
    fn from(s: FfiSort) -> Self {
        match s {
            FfiSort::Relevance => Sort::Relevance,
            FfiSort::Date => Sort::Date,
            FfiSort::Size => Sort::Size,
            FfiSort::Seeders => Sort::Seeders,
            FfiSort::Rating => Sort::Rating,
        }
    }
}

impl From<Sort> for FfiSort {
    fn from(s: Sort) -> Self {
        match s {
            Sort::Relevance => FfiSort::Relevance,
            Sort::Date => FfiSort::Date,
            Sort::Size => FfiSort::Size,
            Sort::Seeders => FfiSort::Seeders,
            Sort::Rating => FfiSort::Rating,
        }
    }
}

impl From<FfiSearchQuery> for SearchQuery {
    fn from(q: FfiSearchQuery) -> Self {
        SearchQuery {
            text: q.text,
            categories: q.categories.into_iter().map(Into::into).collect(),
            min_size: q.min_size,
            max_size: q.max_size,
            since: q.since,
            until: q.until,
            min_seeders: q.min_seeders,
            hide_dead: q.hide_dead,
            min_trust: q.min_trust,
            sort: q.sort.into(),
            descending: q.descending,
            limit: q.limit,
            offset: q.offset,
            include_hidden: q.include_hidden,
            languages: q.languages,
            subtitles: q.subtitles,
            resolutions: q.resolutions,
            sources: q.sources,
            video_codecs: q.video_codecs,
            hdr: q.hdr,
            audio: q.audio,
            genres: q.genres,
            media_kinds: q.media_kinds.into_iter().map(Into::into).collect(),
            min_rating: q.min_rating,
            min_year: q.min_year,
            max_year: q.max_year,
        }
    }
}

impl From<SearchQuery> for FfiSearchQuery {
    fn from(q: SearchQuery) -> Self {
        FfiSearchQuery {
            text: q.text,
            categories: q.categories.into_iter().map(Into::into).collect(),
            min_size: q.min_size,
            max_size: q.max_size,
            since: q.since,
            until: q.until,
            min_seeders: q.min_seeders,
            hide_dead: q.hide_dead,
            min_trust: q.min_trust,
            sort: q.sort.into(),
            descending: q.descending,
            limit: q.limit,
            offset: q.offset,
            include_hidden: q.include_hidden,
            languages: q.languages,
            subtitles: q.subtitles,
            resolutions: q.resolutions,
            sources: q.sources,
            video_codecs: q.video_codecs,
            hdr: q.hdr,
            audio: q.audio,
            genres: q.genres,
            media_kinds: q.media_kinds.into_iter().map(Into::into).collect(),
            min_rating: q.min_rating,
            min_year: q.min_year,
            max_year: q.max_year,
        }
    }
}

impl From<TorrentSummary> for FfiTorrentSummary {
    fn from(t: TorrentSummary) -> Self {
        FfiTorrentSummary {
            infohash: t.infohash,
            event_id: t.event_id,
            title: t.title,
            category: t.category.into(),
            total_size: t.total_size,
            file_count: t.file_count,
            created_at: t.created_at,
            pubkey: t.pubkey,
            npub: t.npub,
            seeders: t.seeders,
            leechers: t.leechers,
            dead: t.dead,
            trust: t.trust,
            pow_bits: t.pow_bits,
            publisher_count: t.publisher_count,
            source_url: t.source_url,
            rank: t.rank,
            pinned: t.pinned,
            hidden: t.hidden,
            media_title: t.media_title,
            media_year: t.media_year,
            media_kind: t.media_kind.map(Into::into),
            matched_file: t.matched_file,
            media_id: t.media_id,
            media_rating: t.media_rating,
            attrs: t.attrs.map(Into::into),
        }
    }
}

impl From<FfiMediaKind> for MediaKind {
    fn from(k: FfiMediaKind) -> Self {
        match k {
            FfiMediaKind::Movie => MediaKind::Movie,
            FfiMediaKind::Series => MediaKind::Series,
            FfiMediaKind::Anime => MediaKind::Anime,
        }
    }
}

/// Short labels for a result row (`FfiReleaseAttrs::badges`).
pub fn badges(a: &ReleaseAttrs) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(r) = &a.resolution {
        out.push(r.clone());
    }
    if let Some(s) = &a.source {
        out.push(release::facet_label("source", s));
    }
    if let Some(c) = &a.video_codec {
        out.push(release::facet_label("codec", c));
    }
    if let Some(h) = a.hdr.first() {
        out.push(release::facet_label("hdr", h));
    }
    let key = a.language_key();
    if key != "unknown" {
        out.push(release::language_label(&key));
    }
    if let Some(c) = a.audio.first() {
        let mut l = release::facet_label("audio", c);
        if let Some(ch) = &a.channels {
            l = format!("{l} {ch}");
        }
        out.push(l);
    }
    for f in &a.formats {
        out.push(f.to_uppercase());
    }
    if let Some(b) = a.bit_depth.filter(|b| *b >= 16) {
        out.push(format!("{b} bit"));
    }
    if let Some(b) = &a.bitrate {
        out.push(format!("{b} kbps"));
    }
    out
}

impl From<ReleaseAttrs> for FfiReleaseAttrs {
    fn from(a: ReleaseAttrs) -> Self {
        let key = a.language_key();
        FfiReleaseAttrs {
            badges: badges(&a),
            language_label: release::language_label(&key),
            language_key: key,
            resolution: a.resolution,
            source: a.source,
            video_codec: a.video_codec,
            hdr: a.hdr,
            bit_depth: a.bit_depth,
            audio: a.audio,
            channels: a.channels,
            languages: a.languages,
            subtitles: a.subtitles,
            french: a.french,
            group: a.group,
            season: a.season,
            season_end: a.season_end,
            episode: a.episode,
            episode_end: a.episode_end,
            complete: a.complete,
            year: a.year,
            editions: a.editions,
            bitrate: a.bitrate,
            sample_rate: a.sample_rate,
            formats: a.formats,
        }
    }
}

impl From<FacetSet> for FfiFacetSet {
    fn from(s: FacetSet) -> Self {
        FfiFacetSet {
            total: s.total,
            facets: s
                .facets
                .into_iter()
                .map(|f| FfiFacet {
                    counts: f
                        .counts
                        .into_iter()
                        .map(|c| FfiFacetCount {
                            label: release::facet_label(&f.field, &c.value),
                            value: c.value,
                            count: c.count,
                        })
                        .collect(),
                    field: f.field,
                    total: f.total,
                })
                .collect(),
        }
    }
}

pub fn releases(r: MediaReleases, source: &str, server_error: Option<String>) -> FfiMediaReleases {
    FfiMediaReleases {
        media: r.media.map(Into::into),
        total: r.total,
        truncated: r.truncated,
        groups: r
            .groups
            .into_iter()
            .map(|g| FfiReleaseGroup {
                season: g.season,
                episode: g.episode,
                resolution: g.resolution,
                language: g.language,
                language_label: g.language_label,
                releases: g.releases.into_iter().map(Into::into).collect(),
            })
            .collect(),
        source: source.into(),
        server_error,
    }
}

impl From<MediaKind> for FfiMediaKind {
    fn from(k: MediaKind) -> Self {
        match k {
            MediaKind::Movie => FfiMediaKind::Movie,
            MediaKind::Series => FfiMediaKind::Series,
            MediaKind::Anime => FfiMediaKind::Anime,
        }
    }
}

impl From<MediaInfo> for FfiMediaInfo {
    fn from(m: MediaInfo) -> Self {
        let imdb_url = m
            .imdb
            .as_deref()
            .filter(|i| {
                i.len() > 2 && i.starts_with("tt") && i[2..].bytes().all(|b| b.is_ascii_digit())
            })
            .map(|i| format!("https://www.imdb.com/title/{i}/"));
        FfiMediaInfo {
            media_id: m.media_id,
            kind: m.kind.into(),
            title: m.title,
            original_title: m.original_title,
            year: m.year,
            end_year: m.end_year,
            genres: m.genres,
            rating: m.rating,
            votes: m.votes,
            runtime_min: m.runtime_min,
            season: m.season,
            episode: m.episode,
            episode_title: m.episode_title,
            imdb: m.imdb,
            imdb_url,
            tmdb_kind: m.tmdb.map(|(k, _)| k.into()),
            tmdb_id: m.tmdb.map(|(_, id)| id),
            mal: m.mal,
            anilist: m.anilist,
            method: m.method,
            confidence: m.confidence,
            overview: m.overview,
            poster: m.poster,
        }
    }
}

pub fn page(p: SearchPage, server_error: Option<String>) -> FfiSearchPage {
    FfiSearchPage {
        hits: p.hits.into_iter().map(Into::into).collect(),
        total: p.total,
        total_is_lower_bound: p.total_is_lower_bound,
        source: p.source,
        server_error,
    }
}

impl From<Liveness> for FfiLiveness {
    fn from(l: Liveness) -> Self {
        let source = match l.source {
            LivenessSource::Udp => "udp",
            LivenessSource::Http => "http",
            LivenessSource::Dht => "dht",
            LivenessSource::DhtValues => "dht_values",
            LivenessSource::None => "none",
        };
        FfiLiveness {
            seeders: l.seeders,
            leechers: l.leechers,
            completed: l.completed,
            dht_peers_est: l.dht_peers_est,
            source: source.into(),
            trackers_ok: l.trackers_ok,
            trackers_failed: l.trackers_failed,
            checked_at: l.checked_at,
            expires_at: l.expires_at,
        }
    }
}

impl From<TrustScore> for FfiTrust {
    fn from(t: TrustScore) -> Self {
        FfiTrust {
            pubkey: t.pubkey,
            score: t.score,
            factors: t
                .factors
                .into_iter()
                .map(|f| FfiTrustFactor {
                    code: f.code,
                    delta: f.delta,
                    detail: f.detail,
                    evidence: f.evidence,
                })
                .collect(),
            root: t.root,
            computed_at: t.computed_at,
        }
    }
}

pub fn detail(d: TorrentDetail, source: &str) -> FfiTorrentDetail {
    FfiTorrentDetail {
        summary: d.summary.into(),
        description: d.description,
        files: d
            .files
            .into_iter()
            .map(|f| FfiFile {
                path: f.path,
                size: f.size,
            })
            .collect(),
        trackers: d.trackers,
        hashtags: d.hashtags,
        category_path: d.category_path,
        external_ids: d
            .external_ids
            .into_iter()
            .map(|(key, value)| FfiKv { key, value })
            .collect(),
        magnet: d.magnet,
        liveness: d.liveness.map(Into::into),
        trust: d.trust.into(),
        events: d
            .events
            .into_iter()
            .map(|e| FfiEventRef {
                event_id: e.event_id,
                pubkey: e.pubkey,
                npub: e.npub,
                created_at: e.created_at,
                title: e.title,
                trust: e.trust,
            })
            .collect(),
        comments: d
            .comments
            .into_iter()
            .map(|c| FfiComment {
                event_id: c.event_id,
                npub: npub(&c.pubkey),
                pubkey: c.pubkey,
                created_at: c.created_at,
                content: c.content,
            })
            .collect(),
        source: source.into(),
        media: d.media.map(Into::into),
    }
}

impl From<SavedSearch> for FfiSavedSearch {
    fn from(s: SavedSearch) -> Self {
        FfiSavedSearch {
            id: s.id,
            name: s.name,
            query: s.query.into(),
            notify: s.notify,
            last_checked: s.last_checked,
        }
    }
}

impl From<RelayInfo> for FfiRelay {
    fn from(r: RelayInfo) -> Self {
        FfiRelay {
            url: r.url,
            source: r.source,
            enabled: r.enabled,
            last_ok: r.last_ok,
            last_err: r.last_err,
            events_found: r.events_found,
        }
    }
}

impl From<DownloadInfo> for FfiDownload {
    fn from(d: DownloadInfo) -> Self {
        FfiDownload {
            id: d.id,
            name: d.name,
            infohash: d.infohash,
            state: d.state,
            progress: d.progress,
            downloaded: d.downloaded,
            total: d.total,
            down_bps: d.down_bps,
            up_bps: d.up_bps,
            peers: d.peers,
            error: d.error,
            output_dir: d.output_dir,
        }
    }
}

impl From<magnetar_core::crawler::CrawlReport> for FfiCrawlReport {
    fn from(r: magnetar_core::crawler::CrawlReport) -> Self {
        FfiCrawlReport {
            relays_tried: r.relays_tried,
            relays_ok: r.relays_ok,
            events_seen: r.events_seen,
            inserted: r.inserted,
            duplicates: r.duplicates,
            rejected: r.rejected,
            duration_ms: r.duration_ms,
            errors: r.errors,
        }
    }
}

impl From<magnetar_core::sync::SyncReport> for FfiSyncReport {
    fn from(r: magnetar_core::sync::SyncReport) -> Self {
        FfiSyncReport {
            method: r.method,
            have: r.have,
            uploaded: r.uploaded,
            duplicates: r.duplicates,
            failed: r.failed,
            error: r.error,
        }
    }
}

impl From<crate::store::CleanupReport> for FfiCleanupReport {
    fn from(r: crate::store::CleanupReport) -> Self {
        FfiCleanupReport {
            evicted: r.evicted,
            bytes_before: r.bytes_before,
            bytes_after: r.bytes_after,
            unsynced_kept: r.unsynced_kept,
        }
    }
}

/// Persisted form of `FfiConfig` (settings key `ffi.config`).
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct StoredConfig {
    pub server_url: Option<String>,
    pub api_token: Option<String>,
    pub root_pubkey: Option<String>,
    pub trust_threshold: u8,
    pub cache_cap_mb: u32,
    pub seed_relays: Vec<String>,
    pub download_dir: String,
    pub state_dir: String,
}

impl From<&FfiConfig> for StoredConfig {
    fn from(c: &FfiConfig) -> Self {
        StoredConfig {
            server_url: c.server_url.clone(),
            api_token: c.api_token.clone(),
            root_pubkey: c.root_pubkey.clone(),
            trust_threshold: c.trust_threshold,
            cache_cap_mb: c.cache_cap_mb,
            seed_relays: c.seed_relays.clone(),
            download_dir: c.download_dir.clone(),
            state_dir: c.state_dir.clone(),
        }
    }
}

impl From<LivenessStats> for FfiLivenessStats {
    fn from(s: LivenessStats) -> Self {
        Self {
            enabled: s.enabled,
            torrents: s.torrents,
            never_checked: s.never_checked,
            due_now: s.due_now,
            checked_24h: s.checked_24h,
            checks_per_hour: s.checks_per_hour,
            alive: s.alive,
            dead: s.dead,
            unknown: s.unknown,
            trackers: s.trackers,
            trackers_backed_off: s.trackers_backed_off,
            dht_nodes: s.dht_nodes,
            last_batch_at: s.last_batch_at,
            last_batch_size: s.last_batch.as_ref().map_or(0, |b| b.hashes),
            last_batch_ms: s.last_batch.as_ref().map_or(0, |b| b.ms),
        }
    }
}

impl From<LivenessHistory> for FfiLivenessHistory {
    fn from(h: LivenessHistory) -> Self {
        Self {
            infohash: h.infohash,
            points: h
                .points
                .into_iter()
                .map(|p| FfiLivenessPoint {
                    at: p.at,
                    seeders: p.seeders,
                    leechers: p.leechers,
                    peers_est: p.peers_est,
                })
                .collect(),
            keep_days: h.keep_days,
        }
    }
}

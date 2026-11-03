package app.magnetar.api

import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable

/*
 * Mirrors of the server's JSON (magnetar_core::{api, model, query}; docs/API.md). Field names
 * are the Rust names; every field has a default so an older server's answer still parses, and
 * the decoder ignores unknown fields and turns an unknown enum value into the default
 * (docs/APP.md §4). Lists of enum-like values (categories, media kinds) are plain strings, so a
 * value added by a newer server never fails a whole saved search.
 *
 * The stats and graph types are in StatsModels.kt.
 */

/** `GET /health` (no auth). */
@Serializable
data class Health(
    val status: String = "",
    val version: String = "",
    val db: Boolean = false,
    /** `magnetar_core::api::features`; a client hides what the server does not list. */
    val features: List<String> = emptyList(),
) {
    fun has(feature: String): Boolean = feature in features
}

/** `Health.features` values the app checks. */
object Features {
    const val FILTERS = "filters"
    const val FACET_SET = "facet_set"
    const val MEDIA_RELEASES = "media_releases"
    const val LIVENESS_HISTORY = "liveness_history"
    const val STATS = "stats"
    const val GRAPHS = "graphs"
    const val ADULT_FILTER = "adult_filter"
    const val ORIGIN = "origin"
    const val DHT = "dht"
    const val SAVED_SEARCH_POLL = "saved_search_poll"
    const val BROWSE_CHECKED_DHT = "browse_checked_dht"
    const val CRAWL_SOURCES = "crawl_sources"
    const val CRAWL_MAP = "crawl_map"
    const val PROBE_FILTERS = "probe_filters"
    /** Track-kind filters and facets (D125): `probe_sub_kinds`, `probe_audio_kinds`, `probe_sub_formats`. */
    const val PROBE_KINDS = "probe_kinds"
    const val WORKERS = "workers"
}

/** `{"error": "<code>", "message": "…"}` of every 4xx/5xx. */
@Serializable
data class ApiErrorBody(val error: String = "", val message: String = "")

@Serializable
enum class Category {
    @SerialName("video") VIDEO,
    @SerialName("audio") AUDIO,
    @SerialName("application") APPLICATION,
    @SerialName("game") GAME,
    @SerialName("porn") PORN,
    @SerialName("anime") ANIME,
    @SerialName("book") BOOK,
    @SerialName("picture") PICTURE,
    @SerialName("other") OTHER,
}

@Serializable
enum class MediaKind {
    @SerialName("movie") MOVIE,
    @SerialName("series") SERIES,
    @SerialName("anime") ANIME,
}

/** Where an infohash is known from (D51). Older servers send none (= Nostr). */
@Serializable
enum class Origin {
    @SerialName("nostr") NOSTR,
    @SerialName("dht") DHT,
    @SerialName("both") BOTH,
}

@Serializable
enum class Sort {
    @SerialName("relevance") RELEVANCE,
    @SerialName("date") DATE,
    @SerialName("size") SIZE,
    @SerialName("seeders") SEEDERS,
    @SerialName("rating") RATING,
}

/** `magnetar_core::query::SearchQuery`, the body of every search-like POST. */
@Serializable
data class SearchQuery(
    val text: String = "",
    /** Category codes (`video`, `porn`, …). Selecting `porn` shows adult hits (server rule). */
    val categories: List<String> = emptyList(),
    @SerialName("min_size") val minSize: Long? = null,
    @SerialName("max_size") val maxSize: Long? = null,
    val since: Long? = null,
    val until: Long? = null,
    @SerialName("min_seeders") val minSeeders: Int? = null,
    @SerialName("hide_dead") val hideDead: Boolean = false,
    /** null = the server's trust threshold. */
    @SerialName("min_trust") val minTrust: Int? = null,
    val sort: Sort = Sort.RELEVANCE,
    val descending: Boolean = true,
    val limit: Int = DEFAULT_LIMIT,
    val offset: Int = 0,
    @SerialName("include_hidden") val includeHidden: Boolean = false,
    /** The Settings switch (D52); off = adult torrents hidden, never deleted. */
    @SerialName("include_adult") val includeAdult: Boolean = false,
    val languages: List<String> = emptyList(),
    val subtitles: List<String> = emptyList(),
    val resolutions: List<String> = emptyList(),
    val sources: List<String> = emptyList(),
    @SerialName("video_codecs") val videoCodecs: List<String> = emptyList(),
    val hdr: List<String> = emptyList(),
    val audio: List<String> = emptyList(),
    val genres: List<String> = emptyList(),
    @SerialName("media_kinds") val mediaKinds: List<String> = emptyList(),
    @SerialName("min_rating") val minRating: Float? = null,
    @SerialName("min_year") val minYear: Int? = null,
    @SerialName("max_year") val maxYear: Int? = null,
    /** Source chips (D51): `nostr`, `dht`; empty = every source. */
    val origins: List<String> = emptyList(),
    /**
     * Header-probe filters (D122, `Features.PROBE_FILTERS`): what a probe read from the files.
     * Languages ISO 639-2/B (`fre`, `eng`), resolution classes, FFmpeg codec names, HDR
     * (`any`, `dolby_vision`, …), audio codecs plus `atmos` / `dts_x`.
     */
    @SerialName("probe_languages") val probeLanguages: List<String> = emptyList(),
    @SerialName("probe_subtitles") val probeSubtitles: List<String> = emptyList(),
    @SerialName("probe_resolutions") val probeResolutions: List<String> = emptyList(),
    @SerialName("probe_video_codecs") val probeVideoCodecs: List<String> = emptyList(),
    @SerialName("probe_hdr") val probeHdr: List<String> = emptyList(),
    @SerialName("probe_audio") val probeAudio: List<String> = emptyList(),
    /**
     * Track-kind filters (D125, `Features.PROBE_KINDS`): subtitle kinds (`sdh`, `cc`, `forced`, …),
     * audio kinds (`original`, `description`, `commentary`, …) and subtitle format classes
     * (`text`, `image`, `cc`). A kind with a language of the same family means one track.
     */
    @SerialName("probe_sub_kinds") val probeSubKinds: List<String> = emptyList(),
    @SerialName("probe_audio_kinds") val probeAudioKinds: List<String> = emptyList(),
    @SerialName("probe_sub_formats") val probeSubFormats: List<String> = emptyList(),
) {
    companion object {
        const val DEFAULT_LIMIT = 50
        const val MAX_LIMIT = 200
    }
}

/** Release attributes parsed from the name and files (D44, docs/FILTERS.md). */
@Serializable
data class ReleaseAttrs(
    val resolution: String? = null,
    val source: String? = null,
    @SerialName("video_codec") val videoCodec: String? = null,
    val hdr: List<String> = emptyList(),
    @SerialName("bit_depth") val bitDepth: Int? = null,
    val audio: List<String> = emptyList(),
    val channels: String? = null,
    val languages: List<String> = emptyList(),
    val subtitles: List<String> = emptyList(),
    val french: List<String> = emptyList(),
    val group: String? = null,
    val season: Int? = null,
    @SerialName("season_end") val seasonEnd: Int? = null,
    val episode: Int? = null,
    @SerialName("episode_end") val episodeEnd: Int? = null,
    val complete: Boolean = false,
    val year: Int? = null,
    val editions: List<String> = emptyList(),
    val bitrate: String? = null,
    @SerialName("sample_rate") val sampleRate: String? = null,
    val formats: List<String> = emptyList(),
)

/** One result row (`magnetar_core::model::TorrentSummary`). */
@Serializable
data class TorrentSummary(
    val infohash: String = "",
    @SerialName("event_id") val eventId: String = "",
    val title: String = "",
    val category: Category = Category.OTHER,
    @SerialName("total_size") val totalSize: Long? = null,
    @SerialName("file_count") val fileCount: Int = 0,
    @SerialName("created_at") val createdAt: Long = 0,
    val pubkey: String = "",
    val npub: String = "",
    val seeders: Int? = null,
    val leechers: Int? = null,
    val dead: Boolean? = null,
    val trust: Int = 0,
    @SerialName("pow_bits") val powBits: Int = 0,
    @SerialName("publisher_count") val publisherCount: Int = 0,
    @SerialName("source_url") val sourceUrl: String? = null,
    val rank: Float = 0f,
    val pinned: Boolean = false,
    val hidden: Boolean = false,
    @SerialName("media_title") val mediaTitle: String? = null,
    @SerialName("media_year") val mediaYear: Int? = null,
    @SerialName("media_kind") val mediaKind: MediaKind? = null,
    @SerialName("matched_file") val matchedFile: String? = null,
    @SerialName("media_id") val mediaId: Long? = null,
    @SerialName("media_rating") val mediaRating: Float? = null,
    val attrs: ReleaseAttrs? = null,
    val origin: Origin = Origin.NOSTR,
    /** max(tracker seeders, DHT estimate) of the latest check (D77); null = never checked. */
    @SerialName("best_peers") val bestPeers: Int? = null,
) {
    /** What a row shows as "seeders": the tracker count, else the best known estimate. */
    val shownSeeders: Int? get() = seeders ?: bestPeers
}

/** `POST /api/v1/search`. */
@Serializable
data class SearchPage(
    val hits: List<TorrentSummary> = emptyList(),
    val total: Long = 0,
    val source: String = "",
    /** `total` is a lower bound ("1,000+", D43). */
    @SerialName("total_is_lower_bound") val totalIsLowerBound: Boolean = false,
)

/** A saved search stored on the server (D77). `id` 0 = create. */
@Serializable
data class SavedSearch(
    val id: Long = 0,
    val name: String = "",
    val query: SearchQuery = SearchQuery(),
    val notify: Boolean = false,
    @SerialName("last_checked") val lastChecked: Long? = null,
    @SerialName("last_seen_created_at") val lastSeenCreatedAt: Long? = null,
)

/** `GET /api/v1/saved-searches/poll` (D77): stateless, the client keeps [watermark]. */
@Serializable
data class SavedSearchPoll(
    val watermark: Long = 0,
    val results: List<SavedSearchHits> = emptyList(),
)

@Serializable
data class SavedSearchHits(
    val search: SavedSearch = SavedSearch(),
    val hits: List<TorrentSummary> = emptyList(),
    val truncated: Boolean = false,
)

/** `GET|PUT /api/v1/settings/trust`: root npub or hex on input, hex on output. */
@Serializable
data class TrustSettings(
    val root: String? = null,
    val threshold: Int = 20,
)

/** `POST /api/v1/facets` body (D44): counts of several facets over one search. */
@Serializable
data class FacetsRequest(
    val query: SearchQuery = SearchQuery(),
    /** Facet names: `category`, `language`, `resolution`, `origin`, … (docs/FILTERS.md). */
    val fields: List<String> = emptyList(),
    /** Values per facet (server default 20, at most 200). */
    val size: Int = 30,
)

@Serializable
data class FacetCount(val value: String = "", val count: Long = 0)

@Serializable
data class FacetCounts(
    val field: String = "",
    /** Results this facet's counts are taken over (every filter but its own). */
    val total: Long = 0,
    /** Most frequent first. */
    val counts: List<FacetCount> = emptyList(),
)

/** Disjunctive counts: each facet applies every filter of the query except its own. */
@Serializable
data class FacetSet(
    /** Results of the query with every filter. */
    val total: Long = 0,
    val facets: List<FacetCounts> = emptyList(),
) {
    fun field(name: String): FacetCounts? = facets.firstOrNull { it.field == name }
}

@Serializable
data class TorrentFile(val path: String = "", val size: Long? = null)

@Serializable
data class Comment(
    @SerialName("event_id") val eventId: String = "",
    val pubkey: String = "",
    @SerialName("created_at") val createdAt: Long = 0,
    val kind: Int = 0,
    val content: String = "",
)

@Serializable
data class EventRef(
    @SerialName("event_id") val eventId: String = "",
    val pubkey: String = "",
    val npub: String = "",
    @SerialName("created_at") val createdAt: Long = 0,
    val title: String = "",
    val trust: Int = 0,
)

/** A seed check's answer (D17/D45); `source` is `udp`, `http`, `dht`, `dht_values` or `none`. */
@Serializable
data class Liveness(
    val infohash: String = "",
    val seeders: Int? = null,
    val leechers: Int? = null,
    val completed: Int? = null,
    @SerialName("dht_peers_est") val dhtPeersEst: Int? = null,
    val source: String = "none",
    @SerialName("trackers_ok") val trackersOk: Int = 0,
    @SerialName("trackers_failed") val trackersFailed: Int = 0,
    @SerialName("checked_at") val checkedAt: Long = 0,
    @SerialName("expires_at") val expiresAt: Long = 0,
) {
    /** max(seeders, DHT estimate), null when neither answered. */
    val bestPeers: Int?
        get() = if (seeders == null && dhtPeersEst == null) null else maxOf(seeders ?: 0, dhtPeersEst ?: 0)
}

@Serializable
data class TrustFactor(
    val code: String = "",
    val delta: Int = 0,
    val detail: String = "",
    val evidence: List<String> = emptyList(),
)

@Serializable
data class TrustScore(
    val pubkey: String = "",
    val score: Int = 0,
    val factors: List<TrustFactor> = emptyList(),
    val root: String? = null,
    @SerialName("computed_at") val computedAt: Long = 0,
)

/** What the server's DHT crawler verified (D51). */
@Serializable
data class DhtInfo(
    @SerialName("first_seen") val firstSeen: Long = 0,
    @SerialName("fetched_at") val fetchedAt: Long = 0,
    val title: String = "",
    @SerialName("file_count") val fileCount: Int = 0,
    @SerialName("total_size") val totalSize: Long = 0,
    /** Classifier category (D69); a plain string so a newer value never fails the page. */
    val category: String? = null,
    @SerialName("peers_seen") val peersSeen: Int = 0,
    @SerialName("discovered_via") val discoveredVia: String = "",
    @SerialName("infohash_v2") val infohashV2: String? = null,
)

/** The identified film/series/anime of a torrent or title page (D40, D42). */
@Serializable
data class MediaInfo(
    @SerialName("media_id") val mediaId: Long = 0,
    val kind: MediaKind = MediaKind.MOVIE,
    val title: String = "",
    @SerialName("original_title") val originalTitle: String? = null,
    val year: Int? = null,
    @SerialName("end_year") val endYear: Int? = null,
    val genres: List<String> = emptyList(),
    val rating: Float? = null,
    val votes: Long? = null,
    @SerialName("runtime_min") val runtimeMin: Int? = null,
    val season: Int? = null,
    val episode: Int? = null,
    val imdb: String? = null,
    val overview: String? = null,
    /** Server-relative poster URL (`/api/v1/media/{id}/poster`); null = none. */
    val poster: String? = null,
    @SerialName("episode_title") val episodeTitle: String? = null,
) {
    val imdbUrl: String? get() = imdb?.takeIf { IMDB_ID.matches(it) }?.let { "https://www.imdb.com/title/$it/" }

    private companion object {
        val IMDB_ID = Regex("^tt[0-9]{5,10}$")
    }
}

/** `GET /api/v1/torrents/{infohash}`. */
@Serializable
data class TorrentDetail(
    val summary: TorrentSummary = TorrentSummary(),
    val description: String = "",
    val files: List<TorrentFile> = emptyList(),
    val trackers: List<String> = emptyList(),
    val hashtags: List<String> = emptyList(),
    @SerialName("category_path") val categoryPath: List<String> = emptyList(),
    /** Pairs `[kind, id]` (`imdb`, `tmdb`, …). */
    @SerialName("external_ids") val externalIds: List<List<String>> = emptyList(),
    val magnet: String = "",
    val liveness: Liveness? = null,
    val trust: TrustScore = TrustScore(),
    val events: List<EventRef> = emptyList(),
    val comments: List<Comment> = emptyList(),
    val media: MediaInfo? = null,
    val dht: DhtInfo? = null,
    /** Header-probe results (D117, D121); null when never probed or from an older server. */
    val probe: ProbeInfo? = null,
)

/**
 * What a crawl worker's header probe read from the files themselves (D117, docs/API.md
 * "Header probes"). Every field has a default: a newer or partial answer never fails the page.
 */
@Serializable
data class ProbeInfo(
    val version: Int = 0,
    @SerialName("probed_at") val probedAt: Long = 0,
    /** `ok` (every target file read to the end) or `partial`. */
    val status: String = "",
    /** Every listed result was read from hash-checked pieces. */
    val verified: Boolean = false,
    val files: List<ProbeFileInfo> = emptyList(),
)

@Serializable
data class ProbeFileInfo(
    /** Index into [TorrentDetail.files] of the same detail. */
    val index: Int = 0,
    val path: String = "",
    val size: Long = 0,
    /** sniff label id (`video.mkv`, `game.ps5.pkg`, `exe.pe`, …). */
    val label: String = "",
    val description: String = "",
    val confidence: Int = 0,
    val verified: Boolean = false,
    val flags: List<String> = emptyList(),
    val game: ProbeGame? = null,
    val video: ProbeVideo? = null,
)

@Serializable
data class ProbeGame(
    val platform: String = "",
    val kind: String? = null,
    val format: String? = null,
    @SerialName("title_id") val titleId: String? = null,
    @SerialName("content_id") val contentId: String? = null,
    @SerialName("db_id") val dbId: String? = null,
    val title: String? = null,
    val version: String? = null,
    val region: String? = null,
)

@Serializable
data class ProbeVideo(
    val container: String = "",
    @SerialName("duration_ms") val durationMs: Long? = null,
    @SerialName("bitrate_kbps") val bitrateKbps: Long? = null,
    val video: List<ProbeVideoTrack> = emptyList(),
    val audio: List<ProbeAudioTrack> = emptyList(),
    val subs: List<ProbeSubTrack> = emptyList(),
)

@Serializable
data class ProbeVideoTrack(
    val codec: String = "",
    val profile: String? = null,
    val width: Int? = null,
    val height: Int? = null,
    /** Frames per second × 1000. */
    @SerialName("fps_milli") val fpsMilli: Int? = null,
    @SerialName("bit_depth") val bitDepth: Int? = null,
    /** `hdr10`, `hdr10_plus`, `hlg`, `dolby_vision`. */
    val hdr: List<String> = emptyList(),
    /** Dolby Vision profile and base-layer compatibility id (D121); older servers send none. */
    @SerialName("dv_profile") val dvProfile: Int? = null,
    @SerialName("dv_compat") val dvCompat: Int? = null,
)

@Serializable
data class ProbeAudioTrack(
    val codec: String = "",
    val channels: Int? = null,
    /** ISO 639-2/B lowercase (`eng`, `fre`, `ger`). */
    val lang: String? = null,
    val atmos: Boolean = false,
    @SerialName("dts_x") val dtsX: Boolean = false,
    /** Track kinds (D123): `original`, `dub`, `description`, `commentary`, …; v1 probes send none. */
    val kinds: List<String> = emptyList(),
    /** The track name as the file has it (D123): kept by the server, never shown (free text from peers). */
    val name: String? = null,
)

@Serializable
data class ProbeSubTrack(
    val codec: String = "",
    val lang: String? = null,
    /** Track kinds (D123): `forced`, `sdh`, `cc`, `signs`, …; v1 probes send none. */
    val kinds: List<String> = emptyList(),
    /** As [ProbeAudioTrack.name]: never shown. */
    val name: String? = null,
)

@Serializable
data class LivenessPoint(
    val at: Long = 0,
    val seeders: Int? = null,
    val leechers: Int? = null,
    @SerialName("peers_est") val peersEst: Int? = null,
)

/** `GET /api/v1/torrents/{infohash}/liveness-history` (D45): oldest first, a step line. */
@Serializable
data class LivenessHistory(
    val infohash: String = "",
    val points: List<LivenessPoint> = emptyList(),
    @SerialName("keep_days") val keepDays: Int = 0,
)

/** One group of the title page: season/episode (series), resolution, language. */
@Serializable
data class ReleaseGroup(
    val season: Int? = null,
    val episode: Int? = null,
    val resolution: String? = null,
    val language: String = "",
    @SerialName("language_label") val languageLabel: String = "",
    val releases: List<TorrentSummary> = emptyList(),
)

/** `POST /api/v1/media/{id}/releases` (D44). */
@Serializable
data class MediaReleases(
    val media: MediaInfo? = null,
    val total: Long = 0,
    val truncated: Boolean = false,
    val groups: List<ReleaseGroup> = emptyList(),
)

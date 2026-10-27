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
 * This stage covers what the shell, Settings and Saved need; the detail, facet, stats and graph
 * types are added by the stages that draw them.
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

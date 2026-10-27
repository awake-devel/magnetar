package app.magnetar.ui.search

import app.magnetar.api.FacetCounts
import app.magnetar.api.Features
import app.magnetar.api.SearchQuery
import app.magnetar.api.Sort
import app.magnetar.ui.common.FILTER_OPTIONS
import app.magnetar.ui.common.facetLabel
import kotlin.math.abs

/** "Published" presets of the filter sheet. */
enum class DatePreset(val label: String, val seconds: Long?) {
    ANY("Any time", null),
    DAY("24 h", 86_400),
    WEEK("7 days", 7 * 86_400),
    MONTH("30 days", 30 * 86_400),
    YEAR("1 year", 365 * 86_400),
}

/** Facets of the filter sheet with chips, in order (docs/FILTERS.md names). */
val FACET_FIELDS = listOf("language", "subtitles", "resolution", "source", "codec", "hdr", "audio", "genre", "media_kind")

val FACET_TITLES = mapOf(
    "language" to "Audio language",
    "subtitles" to "Subtitles",
    "resolution" to "Resolution",
    "source" to "Release source",
    "codec" to "Video codec",
    "hdr" to "HDR",
    "audio" to "Audio",
    "genre" to "Genre",
    "media_kind" to "Type",
)

/** Category codes in the order of the sheet (`magnetar_core::model::Category`). */
val CATEGORIES = listOf("video", "audio", "application", "game", "porn", "anime", "book", "picture", "other")

/** Source chips (D51): the value sent in `origins` and its label. */
val ORIGIN_CHIPS = listOf("nostr" to "Nostr", "dht" to "DHT")

/** What the filter sheet sets. Codes as in docs/FILTERS.md; within a field any value matches. */
data class SearchFilters(
    val categories: Set<String> = emptySet(),
    val minSizeMb: Long? = null,
    val maxSizeMb: Long? = null,
    val date: DatePreset = DatePreset.ANY,
    val minSeeders: Int? = null,
    val hideDead: Boolean = false,
    /** null = the server's trust threshold. */
    val minTrust: Int? = null,
    val languages: Set<String> = emptySet(),
    val subtitles: Set<String> = emptySet(),
    val resolutions: Set<String> = emptySet(),
    val sources: Set<String> = emptySet(),
    val codecs: Set<String> = emptySet(),
    val hdr: Set<String> = emptySet(),
    val audio: Set<String> = emptySet(),
    val genres: Set<String> = emptySet(),
    val kinds: Set<String> = emptySet(),
    /** IMDb-style rating 0–10; null = any. */
    val minRating: Float? = null,
    val minYear: Int? = null,
    val maxYear: Int? = null,
    /** Result sources (D51): "nostr", "dht"; empty = both. */
    val origins: Set<String> = emptySet(),
) {
    val activeCount: Int
        get() = listOf(
            categories.isNotEmpty(), minSizeMb != null || maxSizeMb != null, date != DatePreset.ANY,
            minSeeders != null, hideDead, minTrust != null,
            languages.isNotEmpty(), subtitles.isNotEmpty(), resolutions.isNotEmpty(), sources.isNotEmpty(),
            codecs.isNotEmpty(), hdr.isNotEmpty(), audio.isNotEmpty(), genres.isNotEmpty(), kinds.isNotEmpty(),
            minRating != null, minYear != null || maxYear != null, origins.isNotEmpty(),
        ).count { it }

    /** Selected values of facet [field] (also `category` and `origin`). */
    fun selected(field: String): Set<String> = when (field) {
        "category" -> categories
        "language" -> languages
        "subtitles" -> subtitles
        "resolution" -> resolutions
        "source" -> sources
        "codec" -> codecs
        "hdr" -> hdr
        "audio" -> audio
        "genre" -> genres
        "media_kind" -> kinds
        "origin" -> origins
        else -> emptySet()
    }

    /** [value] of facet [field] switched on or off. */
    fun toggle(field: String, value: String): SearchFilters {
        fun Set<String>.flip() = if (value in this) this - value else this + value
        return when (field) {
            "category" -> copy(categories = categories.flip())
            "language" -> copy(languages = languages.flip())
            "subtitles" -> copy(subtitles = subtitles.flip())
            "resolution" -> copy(resolutions = resolutions.flip())
            "source" -> copy(sources = sources.flip())
            "codec" -> copy(codecs = codecs.flip())
            "hdr" -> copy(hdr = hdr.flip())
            "audio" -> copy(audio = audio.flip())
            "genre" -> copy(genres = genres.flip())
            "media_kind" -> copy(kinds = kinds.flip())
            "origin" -> copy(origins = origins.flip())
            else -> this
        }
    }

    /**
     * Only what the server can apply: a field it would ignore must not be sent, or it answers
     * unfiltered (docs/APP.md §4). Release/media filters need `filters`, Source chips `origin`.
     */
    fun supportedBy(features: Set<String>): SearchFilters {
        var f = this
        if (Features.FILTERS !in features) {
            f = f.copy(
                languages = emptySet(), subtitles = emptySet(), resolutions = emptySet(), sources = emptySet(),
                codecs = emptySet(), hdr = emptySet(), audio = emptySet(), genres = emptySet(), kinds = emptySet(),
                minRating = null, minYear = null, maxYear = null,
            )
        }
        if (Features.ORIGIN !in features) f = f.copy(origins = emptySet())
        return f
    }
}

private const val MB = 1024L * 1024L

/**
 * The request body of a search. [includeAdult] is the Settings switch, sent on every search
 * (D52): adult torrents are hidden while it is off, never deleted.
 */
fun buildQuery(
    text: String,
    filters: SearchFilters,
    sort: Sort,
    descending: Boolean,
    includeAdult: Boolean,
    offset: Int = 0,
    limit: Int = SearchQuery.DEFAULT_LIMIT,
    nowSec: Long = System.currentTimeMillis() / 1000,
) = SearchQuery(
    text = text.trim(),
    categories = CATEGORIES.filter { it in filters.categories } + filters.categories.filter { it !in CATEGORIES }.sorted(),
    minSize = filters.minSizeMb?.let { it * MB },
    maxSize = filters.maxSizeMb?.let { it * MB },
    since = filters.date.seconds?.let { nowSec - it },
    minSeeders = filters.minSeeders,
    hideDead = filters.hideDead,
    minTrust = filters.minTrust,
    sort = sort,
    descending = descending,
    limit = limit,
    offset = offset,
    includeAdult = includeAdult,
    languages = filters.languages.sorted(),
    subtitles = filters.subtitles.sorted(),
    resolutions = filters.resolutions.sorted(),
    sources = filters.sources.sorted(),
    videoCodecs = filters.codecs.sorted(),
    hdr = filters.hdr.sorted(),
    audio = filters.audio.sorted(),
    genres = filters.genres.sorted(),
    mediaKinds = filters.kinds.sorted(),
    minRating = filters.minRating,
    minYear = filters.minYear,
    maxYear = filters.maxYear,
    origins = filters.origins.sorted(),
)

/** Inverse of [buildQuery], for running a saved search; `since` snaps to the nearest preset. */
fun filtersOf(q: SearchQuery, nowSec: Long = System.currentTimeMillis() / 1000) = SearchFilters(
    categories = q.categories.toSet(),
    minSizeMb = q.minSize?.let { it / MB },
    maxSizeMb = q.maxSize?.let { it / MB },
    date = q.since?.let { s ->
        val window = nowSec - s
        DatePreset.entries.filter { it.seconds != null }.minBy { abs(it.seconds!! - window) }
    } ?: DatePreset.ANY,
    minSeeders = q.minSeeders,
    hideDead = q.hideDead,
    minTrust = q.minTrust,
    languages = q.languages.toSet(),
    subtitles = q.subtitles.toSet(),
    resolutions = q.resolutions.toSet(),
    sources = q.sources.toSet(),
    codecs = q.videoCodecs.toSet(),
    hdr = q.hdr.toSet(),
    audio = q.audio.toSet(),
    genres = q.genres.toSet(),
    kinds = q.mediaKinds.toSet(),
    minRating = q.minRating,
    minYear = q.minYear,
    maxYear = q.maxYear,
    origins = q.origins.toSet(),
)

/** One chip of the filter sheet: value, label, count (null = no counts). */
data class FacetChip(val value: String, val label: String, val count: Long?)

/**
 * Chips of one facet: with counts, the counted values (most frequent first) plus any selected
 * value the counts lack (count 0); without, the built-in list of [app.magnetar.ui.common.FILTER_OPTIONS].
 */
fun facetChips(field: String, selected: Set<String>, counts: FacetCounts?): List<FacetChip> {
    val chips = counts?.counts?.map { FacetChip(it.value, facetLabel(field, it.value), it.count) }
        ?: FILTER_OPTIONS[field].orEmpty().map { FacetChip(it, facetLabel(field, it), null) }
    val missing = selected.filter { v -> chips.none { it.value == v } }.sorted()
        .map { FacetChip(it, facetLabel(field, it), if (counts != null) 0L else null) }
    return chips + missing
}

/**
 * Keeps each facet's chips where they first appeared while the sheet is open: picking a chip
 * must not move the others under the finger. A value the new counts lack stays in its place
 * with count 0 (shown disabled); new values go at the end.
 */
class ChipOrder {
    private val order = HashMap<String, List<String>>()

    fun arrange(field: String, chips: List<FacetChip>): List<FacetChip> {
        if (chips.none { it.count != null }) return chips
        val known = order[field] ?: chips.map { it.value }.also { order[field] = it }
        val byValue = chips.associateBy { it.value }
        val fresh = chips.filter { it.value !in known }
        if (fresh.isNotEmpty()) order[field] = known + fresh.map { it.value }
        return known.map { v -> byValue[v] ?: FacetChip(v, facetLabel(field, v), 0L) } + fresh
    }
}

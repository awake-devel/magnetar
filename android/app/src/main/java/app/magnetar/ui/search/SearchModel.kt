package app.magnetar.ui.search

import app.magnetar.api.FacetCounts
import app.magnetar.api.Features
import app.magnetar.api.FileKindCatalogue
import app.magnetar.api.SearchQuery
import app.magnetar.api.Sort
import app.magnetar.ui.common.FILTER_OPTIONS
import app.magnetar.ui.common.categoryLabel
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
    "probe_language" to "Audio track",
    "probe_subtitles" to "Subtitle track",
    "probe_resolution" to "Picture",
    "probe_codec" to "Video track",
    "probe_hdr" to "HDR (probed)",
    "probe_audio" to "Audio format",
    "probe_audio_kind" to "Audio track kind",
    "probe_sub_kind" to "Subtitle kind",
    "probe_sub_format" to "Subtitle format",
    FILE_KIND_FIELD to FILE_KIND_TITLE,
)

/**
 * Header-probe facets (D122): what a crawl worker read from the files, not the name. Shown
 * under their own heading when the server lists `probe_filters`; only probed torrents match.
 */
val PROBE_FACET_FIELDS = listOf(
    "probe_language", "probe_audio_kind", "probe_subtitles", "probe_sub_kind", "probe_sub_format",
    "probe_resolution", "probe_codec", "probe_hdr", "probe_audio",
)

/**
 * Track-kind facets (D125): offered only when the server lists `probe_kinds`. A kind with a
 * language of the same family means one track ("French" + "SDH" = a French SDH track).
 */
val PROBE_KIND_FIELDS = listOf("probe_audio_kind", "probe_sub_kind", "probe_sub_format")

/** The filter sheet's one-line note under the probe heading when a kind facet is shown (D125). */
const val KIND_PAIR_HINT = "A kind with a language means one track: French + SDH = a French SDH track."

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
    /** Header-probe filters (D122), values of the `probe_*` facets. */
    val probeLanguages: Set<String> = emptySet(),
    val probeSubtitles: Set<String> = emptySet(),
    val probeResolutions: Set<String> = emptySet(),
    val probeCodecs: Set<String> = emptySet(),
    val probeHdr: Set<String> = emptySet(),
    val probeAudio: Set<String> = emptySet(),
    /** Track-kind filters (D125), values of `probe_audio_kind`, `probe_sub_kind`, `probe_sub_format`. */
    val probeAudioKinds: Set<String> = emptySet(),
    val probeSubKinds: Set<String> = emptySet(),
    val probeSubFormats: Set<String> = emptySet(),
    /** File kinds (D137): sniff groups or labels of any probed file, kept normalised ([normalizeFileKinds]). */
    val probeFileKinds: Set<String> = emptySet(),
) {
    val activeCount: Int
        get() = listOf(
            categories.isNotEmpty(), minSizeMb != null || maxSizeMb != null, date != DatePreset.ANY,
            minSeeders != null, hideDead, minTrust != null,
            languages.isNotEmpty(), subtitles.isNotEmpty(), resolutions.isNotEmpty(), sources.isNotEmpty(),
            codecs.isNotEmpty(), hdr.isNotEmpty(), audio.isNotEmpty(), genres.isNotEmpty(), kinds.isNotEmpty(),
            minRating != null, minYear != null || maxYear != null, origins.isNotEmpty(),
            probeLanguages.isNotEmpty(), probeSubtitles.isNotEmpty(), probeResolutions.isNotEmpty(),
            probeCodecs.isNotEmpty(), probeHdr.isNotEmpty(), probeAudio.isNotEmpty(),
            probeAudioKinds.isNotEmpty(), probeSubKinds.isNotEmpty(), probeSubFormats.isNotEmpty(),
            probeFileKinds.isNotEmpty(),
        ).count { it }

    /** Any header-probe filter set (D122), file kinds (D137) included. */
    val hasProbeFilters: Boolean
        get() = PROBE_FACET_FIELDS.any { selected(it).isNotEmpty() } || probeFileKinds.isNotEmpty()

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
        "probe_language" -> probeLanguages
        "probe_subtitles" -> probeSubtitles
        "probe_resolution" -> probeResolutions
        "probe_codec" -> probeCodecs
        "probe_hdr" -> probeHdr
        "probe_audio" -> probeAudio
        "probe_audio_kind" -> probeAudioKinds
        "probe_sub_kind" -> probeSubKinds
        "probe_sub_format" -> probeSubFormats
        FILE_KIND_FIELD -> probeFileKinds
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
            "probe_language" -> copy(probeLanguages = probeLanguages.flip())
            "probe_subtitles" -> copy(probeSubtitles = probeSubtitles.flip())
            "probe_resolution" -> copy(probeResolutions = probeResolutions.flip())
            "probe_codec" -> copy(probeCodecs = probeCodecs.flip())
            "probe_hdr" -> copy(probeHdr = probeHdr.flip())
            "probe_audio" -> copy(probeAudio = probeAudio.flip())
            "probe_audio_kind" -> copy(probeAudioKinds = probeAudioKinds.flip())
            "probe_sub_kind" -> copy(probeSubKinds = probeSubKinds.flip())
            "probe_sub_format" -> copy(probeSubFormats = probeSubFormats.flip())
            // A plain flip (an active chip's ×); the dropdown uses [toggleFileKind] below.
            FILE_KIND_FIELD -> copy(probeFileKinds = probeFileKinds.flip())
            else -> this
        }
    }

    /** File kind [value] switched with group / label checkbox semantics ([app.magnetar.ui.search.toggleFileKind]). */
    fun toggleFileKind(value: String, labelsOf: (String) -> List<String>): SearchFilters =
        copy(probeFileKinds = app.magnetar.ui.search.toggleFileKind(probeFileKinds, value, labelsOf).toSet())

    /** [field]'s selection cleared (a dropdown's ×); `date` resets Published. */
    fun clear(field: String): SearchFilters = when (field) {
        "date" -> copy(date = DatePreset.ANY)
        else -> selected(field).fold(this) { f, v -> f.toggle(field, v) }
    }

    /**
     * Only what the server can apply: a field it would ignore must not be sent, or it answers
     * unfiltered (docs/APP.md §4). Release/media filters need `filters`, Source chips `origin`,
     * probe filters `probe_filters`, track-kind filters `probe_kinds` as well (D125: a server with
     * `probe_filters` only would ignore the kinds and answer unfiltered).
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
        if (Features.PROBE_FILTERS !in features) {
            f = f.copy(
                probeLanguages = emptySet(), probeSubtitles = emptySet(), probeResolutions = emptySet(),
                probeCodecs = emptySet(), probeHdr = emptySet(), probeAudio = emptySet(),
            )
        }
        if (Features.PROBE_FILTERS !in features || Features.PROBE_KINDS !in features) {
            f = f.copy(probeAudioKinds = emptySet(), probeSubKinds = emptySet(), probeSubFormats = emptySet())
        }
        // D137: an older server would ignore `probe_file_kinds` and answer unfiltered.
        if (Features.PROBE_FILE_KINDS !in features) f = f.copy(probeFileKinds = emptySet())
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
    probeLanguages = filters.probeLanguages.sorted(),
    probeSubtitles = filters.probeSubtitles.sorted(),
    probeResolutions = filters.probeResolutions.sorted(),
    probeVideoCodecs = filters.probeCodecs.sorted(),
    probeHdr = filters.probeHdr.sorted(),
    probeAudio = filters.probeAudio.sorted(),
    probeSubKinds = filters.probeSubKinds.sorted(),
    probeAudioKinds = filters.probeAudioKinds.sorted(),
    probeSubFormats = filters.probeSubFormats.sorted(),
    probeFileKinds = filters.probeFileKinds.sorted(),
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
    probeLanguages = q.probeLanguages.toSet(),
    probeSubtitles = q.probeSubtitles.toSet(),
    probeResolutions = q.probeResolutions.toSet(),
    probeCodecs = q.probeVideoCodecs.toSet(),
    probeHdr = q.probeHdr.toSet(),
    probeAudio = q.probeAudio.toSet(),
    probeAudioKinds = q.probeAudioKinds.toSet(),
    probeSubKinds = q.probeSubKinds.toSet(),
    probeSubFormats = q.probeSubFormats.toSet(),
    probeFileKinds = q.probeFileKinds.toSet(),
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

    /** [values] in the order they first appeared under [key], those gone included, new ones last. */
    fun arrangeValues(key: String, values: List<String>): List<String> {
        val known = order[key] ?: values.also { order[key] = it }
        val fresh = values.filter { it !in known }
        if (fresh.isNotEmpty()) order[key] = known + fresh
        return known + fresh
    }
}

/**
 * One removable chip of the sheet's active-filter row: field, value, label, and the [values]
 * it removes (several for one chip of many file kinds, D140 C3).
 */
data class ActiveFilter(val field: String, val value: String, val label: String, val values: List<String> = listOf(value))

/**
 * The active list filters as chips, in the sheet's order (D140): categories, sources, the
 * release filters, Published, then the probe filters with File kind first. `date` is Published.
 */
fun SearchFilters.activeFilters(catalogue: FileKindCatalogue? = null): List<ActiveFilter> = buildList {
    for (c in CATEGORIES.filter { it in categories } + categories.filter { it !in CATEGORIES }.sorted()) {
        add(ActiveFilter("category", c, categoryLabel(c)))
    }
    for (o in origins.sorted()) add(ActiveFilter("origin", o, ORIGIN_CHIPS.firstOrNull { it.first == o }?.second ?: o))
    for (field in FACET_FIELDS) for (v in selected(field).sorted()) add(ActiveFilter(field, v, facetLabel(field, v)))
    if (date != DatePreset.ANY) add(ActiveFilter("date", date.name.lowercase(), date.label))
    for (c in fileKindChips(probeFileKinds, fileKindLabelsOf(catalogue)) { fileKindName(catalogue, it) }) {
        add(ActiveFilter(FILE_KIND_FIELD, c.values.joinToString(","), c.label, c.values))
    }
    for (field in PROBE_FACET_FIELDS) for (v in selected(field).sorted()) add(ActiveFilter(field, v, facetLabel(field, v)))
}

/** [a] removed: its value switched off, or Published back to any time. */
fun SearchFilters.without(a: ActiveFilter): SearchFilters = when {
    a.field == "date" -> copy(date = DatePreset.ANY)
    a.field == FILE_KIND_FIELD -> copy(probeFileKinds = probeFileKinds - a.values.toSet())
    a.value in selected(a.field) -> toggle(a.field, a.value)
    else -> this
}

/**
 * The file-kind rows kept where they first appeared while the sheet is open (as [ChipOrder]
 * does for chips): a group or label that drops to 0 stays in place with count 0. Without counts
 * the catalogue order is already stable.
 */
fun ChipOrder.arrangeFileKinds(groups: List<FileKindGroupOption>, catalogue: FileKindCatalogue?): List<FileKindGroupOption> {
    if (groups.all { it.group.count == null }) return groups
    val byGroup = groups.associateBy { it.group.value }
    return arrangeValues(FILE_KIND_FIELD, groups.map { it.group.value }).map { g ->
        val cur = byGroup[g] ?: FileKindGroupOption(FileKindOption(g, fileKindName(catalogue, g), 0L), emptyList())
        val byLabel = cur.labels.associateBy { it.value }
        val labels = arrangeValues("$FILE_KIND_FIELD/$g", cur.labels.map { it.value })
        cur.copy(labels = labels.map { byLabel[it] ?: FileKindOption(it, fileKindName(catalogue, it), 0L) })
    }
}

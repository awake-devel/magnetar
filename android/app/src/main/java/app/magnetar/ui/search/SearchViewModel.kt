package app.magnetar.ui.search

import android.app.Application
import androidx.lifecycle.viewModelScope
import app.magnetar.ffi.FfiCategory
import app.magnetar.ffi.FfiFacetSet
import app.magnetar.ffi.FfiMediaKind
import app.magnetar.ffi.FfiSavedSearch
import app.magnetar.ffi.FfiSearchQuery
import app.magnetar.ffi.FfiSort
import app.magnetar.ffi.FfiTorrentSummary
import app.magnetar.ffiCatching
import app.magnetar.ui.common.BaseViewModel
import app.magnetar.userMessage
import kotlinx.coroutines.FlowPreview
import kotlinx.coroutines.Job
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.collectLatest
import kotlinx.coroutines.flow.debounce
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.drop
import kotlinx.coroutines.flow.filterNotNull
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch

enum class DatePreset(val label: String, val seconds: Long?) {
    ANY("Any time", null),
    DAY("24 h", 86_400),
    WEEK("7 days", 7 * 86_400),
    MONTH("30 days", 30 * 86_400),
    YEAR("1 year", 365 * 86_400),
}

data class SearchFilters(
    val categories: Set<FfiCategory> = emptySet(),
    val minSizeMb: Long? = null,
    val maxSizeMb: Long? = null,
    val date: DatePreset = DatePreset.ANY,
    val minSeeders: Int? = null,
    val hideDead: Boolean = false,
    /** null = the configured trust threshold. */
    val minTrust: Int? = null,
    // Release/media filters (D44): codes as in docs/FILTERS.md, any of a set matches.
    val languages: Set<String> = emptySet(),
    val subtitles: Set<String> = emptySet(),
    val resolutions: Set<String> = emptySet(),
    val sources: Set<String> = emptySet(),
    val codecs: Set<String> = emptySet(),
    val hdr: Set<String> = emptySet(),
    val audio: Set<String> = emptySet(),
    val genres: Set<String> = emptySet(),
    val kinds: Set<FfiMediaKind> = emptySet(),
    /** IMDb-style rating 0–10; null = any. */
    val minRating: Float? = null,
    val minYear: Int? = null,
    val maxYear: Int? = null,
) {
    val activeCount: Int
        get() = listOf(
            categories.isNotEmpty(), minSizeMb != null, maxSizeMb != null, date != DatePreset.ANY,
            minSeeders != null, hideDead, minTrust != null,
            languages.isNotEmpty(), subtitles.isNotEmpty(), resolutions.isNotEmpty(), sources.isNotEmpty(),
            codecs.isNotEmpty(), hdr.isNotEmpty(), audio.isNotEmpty(), genres.isNotEmpty(), kinds.isNotEmpty(),
            minRating != null, minYear != null || maxYear != null,
        ).count { it }

    /** The values selected for facet [field] (the names of `FACET_FIELDS`). */
    fun selected(field: String): Set<String> = when (field) {
        "language" -> languages
        "subtitles" -> subtitles
        "resolution" -> resolutions
        "source" -> sources
        "codec" -> codecs
        "hdr" -> hdr
        "audio" -> audio
        "genre" -> genres
        "media_kind" -> kinds.mapTo(HashSet()) { it.code() }
        else -> emptySet()
    }

    /** [value] of facet [field] switched on or off. */
    fun toggle(field: String, value: String): SearchFilters {
        fun Set<String>.flip() = if (value in this) this - value else this + value
        return when (field) {
            "language" -> copy(languages = languages.flip())
            "subtitles" -> copy(subtitles = subtitles.flip())
            "resolution" -> copy(resolutions = resolutions.flip())
            "source" -> copy(sources = sources.flip())
            "codec" -> copy(codecs = codecs.flip())
            "hdr" -> copy(hdr = hdr.flip())
            "audio" -> copy(audio = audio.flip())
            "genre" -> copy(genres = genres.flip())
            "media_kind" -> {
                val k = mediaKindOf(value) ?: return this
                copy(kinds = if (k in kinds) kinds - k else kinds + k)
            }
            else -> this
        }
    }

    /** Only what [mode] can apply (filters the server or the cache cannot test are dropped). */
    fun supportedBy(mode: FilterMode): SearchFilters = when (mode) {
        FilterMode.FULL -> this
        FilterMode.LOCAL -> copy(genres = emptySet(), kinds = emptySet(), minRating = null)
        FilterMode.BASIC -> copy(
            languages = emptySet(), subtitles = emptySet(), resolutions = emptySet(), sources = emptySet(),
            codecs = emptySet(), hdr = emptySet(), audio = emptySet(), genres = emptySet(), kinds = emptySet(),
            minRating = null, minYear = null, maxYear = null,
        )
    }
}

fun FfiMediaKind.code(): String = name.lowercase()

fun mediaKindOf(code: String): FfiMediaKind? = FfiMediaKind.entries.firstOrNull { it.code() == code }

/**
 * Which filters the app offers (D44). LOCAL: no server configured or it did not answer, so
 * searches use the phone cache, which knows the release attributes and years but only the media
 * (genre, kind, rating) of details opened online. FULL: a server with the `filters` feature.
 * BASIC: a server older than D44, which would ignore the new filters.
 */
enum class FilterMode { LOCAL, FULL, BASIC }

fun filterMode(hasServer: Boolean, features: List<String>?): FilterMode = when {
    !hasServer || features == null -> FilterMode.LOCAL
    "filters" in features -> FilterMode.FULL
    else -> FilterMode.BASIC
}

/** Facets shown in the filter sheet, in order. */
val FACET_FIELDS = listOf("language", "subtitles", "resolution", "source", "codec", "hdr", "audio", "genre", "media_kind")

data class SearchUiState(
    val text: String = "",
    val filters: SearchFilters = SearchFilters(),
    val sort: FfiSort = FfiSort.RELEVANCE,
    val descending: Boolean = true,
    val hits: List<FfiTorrentSummary> = emptyList(),
    val total: ULong = 0u,
    /** [total] is a lower bound ("1,000+"). */
    val totalIsLowerBound: Boolean = false,
    val loading: Boolean = false,
    val loadingMore: Boolean = false,
    val endReached: Boolean = false,
    val error: String? = null,
    /** "server" | "local" for the last page. */
    val source: String? = null,
    val serverError: String? = null,
    /** Bumped for every fresh first page, so the list can scroll back to the top. */
    val generation: Int = 0,
    val filterMode: FilterMode = FilterMode.LOCAL,
    /** The server can count facets (`facet_set`); still null counts while its index builds. */
    val facetCounts: Boolean = false,
)

private const val MB = 1024L * 1024L
/** Pause after a change in the filter sheet before its counts are refreshed. */
const val FACET_DEBOUNCE_MS = 300L
private const val PAGE = 50u
/** Typing pause before a search runs; the server index answers in well under this. */
const val SEARCH_DEBOUNCE_MS = 250L

fun SearchUiState.toQuery(offset: UInt = 0u, nowSec: Long = System.currentTimeMillis() / 1000) = FfiSearchQuery(
    text = text.trim(),
    categories = filters.categories.toList(),
    minSize = filters.minSizeMb?.let { (it * MB).toULong() },
    maxSize = filters.maxSizeMb?.let { (it * MB).toULong() },
    since = filters.date.seconds?.let { nowSec - it },
    until = null,
    minSeeders = filters.minSeeders?.toUInt(),
    hideDead = filters.hideDead,
    minTrust = filters.minTrust?.toUByte(),
    sort = sort,
    descending = descending,
    limit = PAGE,
    offset = offset,
    includeHidden = false,
    languages = filters.languages.sorted(),
    subtitles = filters.subtitles.sorted(),
    resolutions = filters.resolutions.sorted(),
    sources = filters.sources.sorted(),
    videoCodecs = filters.codecs.sorted(),
    hdr = filters.hdr.sorted(),
    audio = filters.audio.sorted(),
    genres = filters.genres.sorted(),
    mediaKinds = filters.kinds.sortedBy { it.ordinal },
    minRating = filters.minRating,
    minYear = filters.minYear,
    maxYear = filters.maxYear,
)

/** Inverse of [toQuery], for running a saved search; `since` snaps to the nearest preset. */
fun FfiSearchQuery.toUiState(nowSec: Long = System.currentTimeMillis() / 1000) = SearchUiState(
    text = text,
    filters = SearchFilters(
        categories = categories.toSet(),
        minSizeMb = minSize?.let { (it.toLong() / MB) },
        maxSizeMb = maxSize?.let { (it.toLong() / MB) },
        date = since?.let { s ->
            val window = nowSec - s
            DatePreset.entries.filter { it.seconds != null }.minBy { kotlin.math.abs(it.seconds!! - window) }
        } ?: DatePreset.ANY,
        minSeeders = minSeeders?.toInt(),
        hideDead = hideDead,
        minTrust = minTrust?.toInt(),
        languages = languages.toSet(),
        subtitles = subtitles.toSet(),
        resolutions = resolutions.toSet(),
        sources = sources.toSet(),
        codecs = videoCodecs.toSet(),
        hdr = hdr.toSet(),
        audio = audio.toSet(),
        genres = genres.toSet(),
        kinds = mediaKinds.toSet(),
        minRating = minRating,
        minYear = minYear,
        maxYear = maxYear,
    ),
    sort = sort,
    descending = descending,
)

@OptIn(FlowPreview::class)
class SearchViewModel(application: Application) : BaseViewModel(application) {
    private val _state = MutableStateFlow(SearchUiState())
    val state: StateFlow<SearchUiState> = _state.asStateFlow()

    val settings get() = magApp.settings

    /** Counts for the open filter sheet (D44); null = none (offline, old server, index building). */
    private val _facets = MutableStateFlow<FfiFacetSet?>(null)
    val facets: StateFlow<FfiFacetSet?> = _facets.asStateFlow()
    /** Draft filters of the open sheet; counts follow them with a short debounce. */
    private val facetDraft = MutableStateFlow<SearchFilters?>(null)

    private val textInput = MutableStateFlow("")
    private var searchJob: Job? = null
    /** Bumped by every new search: a result applies only while its search is still the newest. */
    private var searchSeq = 0

    init {
        viewModelScope.launch {
            textInput.drop(1).debounce(SEARCH_DEBOUNCE_MS).distinctUntilChanged().collectLatest { search() }
        }
        // A new server URL, token, trust threshold or adult switch changes what a search
        // returns (e.g. the "offline results (401)" banner after the token is fixed): run it
        // again.
        viewModelScope.launch {
            magApp.settings.map { listOf(it.serverUrl, it.apiToken, it.trustThreshold, it.showAdult) }
                .distinctUntilChanged().drop(1).collect {
                    refreshFeatures()
                    search()
                }
        }
        viewModelScope.launch {
            facetDraft.filterNotNull().debounce(FACET_DEBOUNCE_MS).collectLatest { loadFacets(it) }
        }
        refreshFeatures()
        viewModelScope.launch {
            magApp.pendingSearch.filterNotNull().collect { q ->
                magApp.pendingSearch.value = null
                _state.value = q.toUiState()
                textInput.value = q.text
                search()
            }
        }
        search()
    }

    fun onTextChange(text: String) {
        _state.update { it.copy(text = text) }
        textInput.value = text
    }

    fun submit() = search()

    /** What the server supports decides which filters the sheet offers (D44). */
    fun refreshFeatures() {
        viewModelScope.launch {
            val hasServer = magApp.settings.value.hasServer
            val features = if (hasServer) ffiCatching { magApp.magnetar().serverFeatures() }.getOrNull() else null
            _state.update {
                it.copy(
                    filterMode = filterMode(hasServer, features),
                    facetCounts = features?.contains("facet_set") == true,
                )
            }
        }
    }

    /** The filter sheet opened or its draft changed: count again (debounced). */
    fun draftFilters(f: SearchFilters?) {
        if (f == null) _facets.value = null
        facetDraft.value = f
    }

    private suspend fun loadFacets(f: SearchFilters) {
        val s = _state.value
        if (!s.facetCounts || s.filterMode != FilterMode.FULL) {
            _facets.value = null
            return
        }
        val query = s.copy(filters = f).toQuery()
        // 503 while the server builds its index: no counts, the chips still work.
        _facets.value = ffiCatching { magApp.magnetar().facets(query, FACET_FIELDS + "category") }.getOrNull()
    }

    fun setFilters(f: SearchFilters) {
        _state.update { it.copy(filters = f) }
        search()
    }

    fun setSort(sort: FfiSort) {
        // Relevance reads best-first; everything else defaults to newest/largest/most first too.
        _state.update { it.copy(sort = sort, descending = true) }
        search()
    }

    fun toggleDirection() {
        _state.update { it.copy(descending = !it.descending) }
        search()
    }

    private fun search() {
        // The newest query wins: the previous request is cancelled and its late answer dropped.
        searchJob?.cancel()
        val seq = ++searchSeq
        val query = _state.value.toQuery()
        val server = magApp.settings.value.hasServer
        magApp.lastQuery = query
        // A cancelled loadMore never clears its own flag.
        _state.update { it.copy(loading = true, loadingMore = false, error = null, endReached = false) }
        searchJob = viewModelScope.launch {
            if (server) {
                // The local cache answers in milliseconds: show it while the server works.
                ffiCatching { magApp.magnetar().search(query, false) }.onSuccess { page ->
                    _state.update {
                        if (seq != searchSeq || !it.loading) {
                            it
                        } else {
                            it.copy(
                                hits = page.hits, total = page.total, totalIsLowerBound = page.totalIsLowerBound,
                                source = page.source, serverError = null, endReached = true,
                                generation = it.generation + 1,
                            )
                        }
                    }
                }
            }
            ffiCatching { magApp.magnetar().search(query, server) }
                .onSuccess { page ->
                    // The server answers again after an offline start: offer its filters.
                    if (page.source == "server" && _state.value.filterMode == FilterMode.LOCAL) refreshFeatures()
                    _state.update {
                        if (seq != searchSeq) {
                            it
                        } else {
                            // Same first hit as the cached preview: keep the scroll position.
                            val sameTop = it.hits.firstOrNull()?.infohash == page.hits.firstOrNull()?.infohash
                            it.copy(
                                hits = page.hits, total = page.total, totalIsLowerBound = page.totalIsLowerBound,
                                loading = false, source = page.source, serverError = page.serverError,
                                endReached = page.hits.size.toUInt() < PAGE,
                                generation = if (sameTop && it.hits.isNotEmpty()) it.generation else it.generation + 1,
                            )
                        }
                    }
                }
                .onFailure { e ->
                    _state.update {
                        if (seq != searchSeq) it else it.copy(loading = false, hits = emptyList(), error = e.userMessage())
                    }
                }
        }
    }

    fun loadMore() {
        val s = _state.value
        if (s.loading || s.loadingMore || s.endReached || s.hits.isEmpty()) return
        val query = s.toQuery(offset = s.hits.size.toUInt())
        val seq = searchSeq
        _state.update { it.copy(loadingMore = true) }
        searchJob = viewModelScope.launch {
            ffiCatching { magApp.magnetar().search(query, magApp.settings.value.hasServer) }
                .onSuccess { page ->
                    _state.update {
                        if (seq != searchSeq) return@update it.copy(loadingMore = false)
                        val known = it.hits.mapTo(HashSet()) { h -> h.infohash }
                        val merged = it.hits + page.hits.filter { h -> h.infohash !in known }
                        it.copy(
                            hits = merged, total = page.total, totalIsLowerBound = page.totalIsLowerBound,
                            loadingMore = false, source = page.source, serverError = page.serverError,
                            endReached = page.hits.size.toUInt() < PAGE ||
                                (!page.totalIsLowerBound && merged.size.toULong() >= page.total),
                        )
                    }
                }
                .onFailure { e ->
                    _state.update { it.copy(loadingMore = false) }
                    if (seq == searchSeq) message(e.userMessage())
                }
        }
    }

    fun saveSearch(name: String, notify: Boolean) = launchFfi { m ->
        val q = _state.value.toQuery()
        io { m.saveSearch(FfiSavedSearch(id = 0, name = name, query = q, notify = notify, lastChecked = null)) }
        message("Saved \"$name\"")
    }
}

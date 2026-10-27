package app.magnetar.ui.search

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import app.magnetar.api.ApiException
import app.magnetar.api.Category
import app.magnetar.api.FacetSet
import app.magnetar.api.FacetsRequest
import app.magnetar.api.Features
import app.magnetar.api.SavedSearch
import app.magnetar.api.SearchPage
import app.magnetar.api.SearchQuery
import app.magnetar.api.Sort
import app.magnetar.api.TorrentSummary
import app.magnetar.data.Repository
import app.magnetar.ui.common.isOffline
import app.magnetar.ui.common.userMessage
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.FlowPreview
import kotlinx.coroutines.Job
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.collectLatest
import kotlinx.coroutines.flow.debounce
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.drop
import kotlinx.coroutines.flow.filterNotNull
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.flow.receiveAsFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch

/** Typing pause before a search runs (docs/APP.md §2). */
const val SEARCH_DEBOUNCE_MS = 250L

/** Pause after a change in the open filter sheet before its counts are asked again. */
const val FACET_DEBOUNCE_MS = 300L

/** Facet values asked per field: enough for every language/genre a sheet shows. */
const val FACET_SIZE = 40

data class SearchUiState(
    val text: String = "",
    val filters: SearchFilters = SearchFilters(),
    val sort: Sort = Sort.RELEVANCE,
    val descending: Boolean = true,
    val hits: List<TorrentSummary> = emptyList(),
    val total: Long = 0,
    /** [total] is a lower bound ("1,000+", D43). */
    val totalIsLowerBound: Boolean = false,
    val loading: Boolean = false,
    val loadingMore: Boolean = false,
    val endReached: Boolean = true,
    /** The last search failed; [hits] may still hold a cached page. */
    val error: String? = null,
    /** The failure was the network or a timeout: [hits] (if any) are the cached page. */
    val offline: Boolean = false,
    /** The shown page is the cache's, saved at this time (unix ms); null = live. */
    val cachedAtMs: Long? = null,
    /** No server in Settings. */
    val notConfigured: Boolean = false,
    /** Bumped for every new first page, so the list scrolls back to the top. */
    val generation: Int = 0,
    /** The server's `/health` features (empty until it answered). */
    val features: Set<String> = emptySet(),
) {
    val canFilterReleases: Boolean get() = Features.FILTERS in features
    val canCountFacets: Boolean get() = Features.FACET_SET in features
    val canFilterOrigin: Boolean get() = Features.ORIGIN in features
}

/** One search to run; [seq] makes a retry of the same query a new value. */
private data class SearchRequest(
    val text: String,
    val filters: SearchFilters,
    val sort: Sort,
    val descending: Boolean,
    val immediate: Boolean,
    val seq: Long,
    /** When the user asked (keystroke, chip, retry), for the time-to-first-results log. */
    val askedAtMs: Long,
)

/**
 * The Search tab (docs/APP.md §1, §2): the text box is debounced by [SEARCH_DEBOUNCE_MS], filter,
 * sort and retry run at once; the newest search wins (`collectLatest` cancels the call in flight,
 * which cancels its OkHttp call). The cached first page is shown at once and replaced by the
 * server's answer.
 */
@OptIn(FlowPreview::class)
class SearchViewModel(
    private val repo: Repository,
    private val clock: () -> Long = System::currentTimeMillis,
    /** Time from the user's action to results on screen: "<ms> live|cached". */
    private val onTiming: (String) -> Unit = {},
) : ViewModel() {
    private val _state = MutableStateFlow(SearchUiState())
    val state: StateFlow<SearchUiState> = _state.asStateFlow()

    private val _facets = MutableStateFlow<FacetSet?>(null)

    /** Counts of the open filter sheet; null = none (no `facet_set`, index building, error). */
    val facets: StateFlow<FacetSet?> = _facets.asStateFlow()
    private val facetDraft = MutableStateFlow<SearchFilters?>(null)

    private val messagesChannel = Channel<String>(Channel.BUFFERED)
    val messages: Flow<String> = messagesChannel.receiveAsFlow()

    private var seq = 0L
    private val requests = MutableStateFlow<SearchRequest?>(null)
    private var moreJob: Job? = null

    /** seq of the search whose page is on screen; a late "load more" of an older one is dropped. */
    private var shownSeq = -1L

    init {
        viewModelScope.launch {
            requests.filterNotNull()
                .debounce { if (it.immediate) 0L else SEARCH_DEBOUNCE_MS }
                .collectLatest { run(it) }
        }
        // Server, token, trust threshold or adult switch change what a search returns.
        viewModelScope.launch {
            repo.settings.map { listOf(it.serverUrl, it.apiToken, it.trustThreshold, it.showAdult) }
                .distinctUntilChanged().drop(1).collect { submit() }
        }
        viewModelScope.launch {
            facetDraft.debounce(FACET_DEBOUNCE_MS).collectLatest { f -> _facets.value = f?.let { loadFacets(it) } }
        }
        viewModelScope.launch {
            repo.pendingSearch.filterNotNull().collect { q ->
                repo.pendingSearch.value = null
                runSaved(q)
            }
        }
        submit()
    }

    fun onTextChange(text: String) {
        if (text == _state.value.text) return
        _state.update { it.copy(text = text) }
        enqueue(immediate = false)
    }

    /** Search key, Retry, or a settings change: run now. */
    fun submit() = enqueue(immediate = true)

    fun setFilters(f: SearchFilters) {
        _state.update { it.copy(filters = f) }
        submit()
    }

    fun setSort(sort: Sort) {
        // Every order reads best/newest/largest first by default.
        _state.update { it.copy(sort = sort, descending = true) }
        submit()
    }

    fun toggleDirection() {
        _state.update { it.copy(descending = !it.descending) }
        submit()
    }

    /** A saved search run from the Saved tab: its text, filters and sort replace the current ones. */
    fun runSaved(q: SearchQuery) {
        _state.update { it.copy(text = q.text, filters = filtersOf(q, clock() / 1000), sort = q.sort, descending = q.descending) }
        submit()
    }

    /** The filter sheet opened (or its draft changed): count again, debounced. null = closed. */
    fun draftFilters(f: SearchFilters?) {
        if (f == null) _facets.value = null
        facetDraft.value = f
    }

    private fun enqueue(immediate: Boolean) {
        val s = _state.value
        requests.value = SearchRequest(s.text, s.filters, s.sort, s.descending, immediate, ++seq, clock())
    }

    private fun query(r: SearchRequest, features: Set<String>, offset: Int = 0) = buildQuery(
        r.text, r.filters.supportedBy(features), r.sort.takeIf { it != Sort.RATING || Features.FILTERS in features } ?: Sort.RELEVANCE,
        r.descending, repo.settings.value.showAdult, offset = offset, nowSec = clock() / 1000,
    )

    /** The page cache key: the query with a fixed clock, so "7 days" is the same key tomorrow. */
    private fun cacheKey(r: SearchRequest, features: Set<String>) =
        repo.keyOf(SearchQuery.serializer(), query(r, features).copy(since = r.filters.date.seconds?.let { -it }))

    private suspend fun run(r: SearchRequest) {
        moreJob?.cancel()
        _state.update { it.copy(loading = true, loadingMore = false, error = null, notConfigured = false) }
        if (!repo.settings.value.hasServer) {
            _state.update {
                it.copy(loading = false, notConfigured = true, hits = emptyList(), total = 0, cachedAtMs = null, endReached = true)
            }
            return
        }
        val health = repo.refreshHealth()
        val features = repo.health.value?.features?.toSet().orEmpty()
        _state.update { it.copy(features = features) }
        val key = cacheKey(r, features)
        val cached = repo.readCache(CACHE_SEARCH, key, SearchPage.serializer())
        if (cached != null) {
            _state.update {
                it.copy(
                    hits = visible(cached.value.hits, r, features), total = cached.value.total,
                    totalIsLowerBound = cached.value.totalIsLowerBound, cachedAtMs = cached.savedAtMs,
                    endReached = true, generation = it.generation + 1,
                )
            }
            onTiming("${clock() - r.askedAtMs} cached")
        }
        try {
            if (health == null) throw ApiException.Network(java.io.IOException("health unreachable"))
            val page = repo.service().search(query(r, features))
            val hits = visible(page.hits, r, features)
            _state.update {
                // Same first hit as the preview: keep the scroll position.
                val sameTop = it.cachedAtMs != null && it.hits.firstOrNull()?.infohash == hits.firstOrNull()?.infohash
                it.copy(
                    hits = hits, total = page.total, totalIsLowerBound = page.totalIsLowerBound,
                    loading = false, error = null, offline = false, cachedAtMs = null,
                    endReached = page.hits.size < SearchQuery.DEFAULT_LIMIT,
                    generation = if (sameTop) it.generation else it.generation + 1,
                )
            }
            shownSeq = r.seq
            onTiming("${clock() - r.askedAtMs} live")
            repo.writeCache(CACHE_SEARCH, key, SearchPage.serializer(), page)
        } catch (e: CancellationException) {
            throw e
        } catch (e: ApiException) {
            if (e is ApiException.NotConfigured) {
                _state.update { it.copy(loading = false, notConfigured = true, hits = emptyList(), total = 0) }
                return
            }
            _state.update {
                val keep = e.isOffline && cached != null
                it.copy(
                    loading = false, error = e.userMessage(), offline = e.isOffline,
                    hits = if (keep) it.hits else emptyList(), total = if (keep) it.total else 0,
                    cachedAtMs = if (keep) it.cachedAtMs else null, endReached = true,
                )
            }
            if (e.isOffline) repo.refreshHealth(force = true)
        }
    }

    /**
     * A server without the adult filter (pre-D52) cannot hide adult torrents: the app drops
     * category `porn` itself while the switch is off, as the old app did (docs/APP.md §6). That
     * is all: no detection of its own, and the server's rule is never made stricter.
     */
    private fun visible(hits: List<TorrentSummary>, r: SearchRequest, features: Set<String>): List<TorrentSummary> {
        if (Features.ADULT_FILTER in features || repo.settings.value.showAdult || "porn" in r.filters.categories) return hits
        return hits.filter { it.category != Category.PORN }
    }

    fun loadMore() {
        val s = _state.value
        val r = requests.value ?: return
        if (s.loading || s.loadingMore || s.endReached || s.hits.isEmpty() || s.cachedAtMs != null || shownSeq != r.seq) return
        _state.update { it.copy(loadingMore = true) }
        moreJob = viewModelScope.launch {
            try {
                val page = repo.service().search(query(r, s.features, offset = s.hits.size))
                if (requests.value?.seq != r.seq) return@launch
                _state.update {
                    val known = it.hits.mapTo(HashSet()) { h -> h.infohash }
                    val merged = it.hits + visible(page.hits, r, s.features).filter { h -> h.infohash !in known }
                    it.copy(
                        hits = merged, total = page.total, totalIsLowerBound = page.totalIsLowerBound, loadingMore = false,
                        endReached = page.hits.size < SearchQuery.DEFAULT_LIMIT ||
                            (!page.totalIsLowerBound && merged.size >= page.total),
                    )
                }
            } catch (e: CancellationException) {
                _state.update { it.copy(loadingMore = false) }
                throw e
            } catch (e: ApiException) {
                _state.update { it.copy(loadingMore = false) }
                messagesChannel.trySend(e.userMessage())
            }
        }
    }

    private suspend fun loadFacets(f: SearchFilters): FacetSet? {
        val s = _state.value
        if (!s.canCountFacets) return null
        val q = buildQuery(s.text, f.supportedBy(s.features), Sort.RELEVANCE, true, repo.settings.value.showAdult, nowSec = clock() / 1000)
        val fields = buildList {
            add("category")
            if (s.canFilterOrigin) add("origin")
            if (s.canFilterReleases) addAll(FACET_FIELDS)
        }
        return try {
            repo.service().facets(FacetsRequest(q, fields, FACET_SIZE))
        } catch (e: CancellationException) {
            throw e
        } catch (_: ApiException) {
            // 503 while the server builds its index, or offline: no counts, the chips still work.
            null
        }
    }

    /** "Save search" from the current text, filters and sort (incl. the adult switch). */
    fun saveSearch(name: String, notify: Boolean) {
        val s = _state.value
        val q = buildQuery(s.text, s.filters.supportedBy(s.features), s.sort, s.descending, repo.settings.value.showAdult, nowSec = clock() / 1000)
        viewModelScope.launch {
            try {
                repo.service().createSavedSearch(SavedSearch(name = name, query = q, notify = notify))
                repo.savedSearchesChanged.value++
                messagesChannel.trySend("Saved \"$name\"")
            } catch (e: CancellationException) {
                throw e
            } catch (e: ApiException) {
                messagesChannel.trySend("Not saved: ${e.userMessage()}")
            }
        }
    }

    companion object {
        const val CACHE_SEARCH = "search"
    }
}

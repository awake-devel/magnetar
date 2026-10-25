package app.magnetar.ui.search

import android.app.Application
import androidx.lifecycle.viewModelScope
import app.magnetar.ffi.FfiCategory
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
) {
    val activeCount: Int
        get() = listOf(
            categories.isNotEmpty(), minSizeMb != null, maxSizeMb != null, date != DatePreset.ANY,
            minSeeders != null, hideDead, minTrust != null,
        ).count { it }
}

data class SearchUiState(
    val text: String = "",
    val filters: SearchFilters = SearchFilters(),
    val sort: FfiSort = FfiSort.RELEVANCE,
    val descending: Boolean = true,
    val hits: List<FfiTorrentSummary> = emptyList(),
    val total: ULong = 0u,
    val loading: Boolean = false,
    val loadingMore: Boolean = false,
    val endReached: Boolean = false,
    val error: String? = null,
    /** "server" | "local" for the last page. */
    val source: String? = null,
    val serverError: String? = null,
    /** Bumped for every fresh first page, so the list can scroll back to the top. */
    val generation: Int = 0,
)

private const val MB = 1024L * 1024L
private const val PAGE = 50u

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
    ),
    sort = sort,
    descending = descending,
)

@OptIn(FlowPreview::class)
class SearchViewModel(application: Application) : BaseViewModel(application) {
    private val _state = MutableStateFlow(SearchUiState())
    val state: StateFlow<SearchUiState> = _state.asStateFlow()

    val settings get() = magApp.settings

    private val textInput = MutableStateFlow("")
    private var searchJob: Job? = null

    init {
        viewModelScope.launch {
            textInput.drop(1).debounce(400).distinctUntilChanged().collectLatest { search() }
        }
        // A new server URL, token or trust threshold changes what a search returns (e.g. the
        // "offline results (401)" banner after the token is fixed): run it again.
        viewModelScope.launch {
            magApp.settings.map { Triple(it.serverUrl, it.apiToken, it.trustThreshold) }
                .distinctUntilChanged().drop(1).collect { search() }
        }
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
        searchJob?.cancel()
        val query = _state.value.toQuery()
        magApp.lastQuery = query
        _state.update { it.copy(loading = true, error = null, endReached = false) }
        searchJob = viewModelScope.launch {
            ffiCatching { magApp.magnetar().search(query, magApp.settings.value.hasServer) }
                .onSuccess { page ->
                    _state.update {
                        it.copy(
                            hits = page.hits, total = page.total, loading = false, source = page.source,
                            serverError = page.serverError, endReached = page.hits.size.toUInt() < PAGE,
                            generation = it.generation + 1,
                        )
                    }
                }
                .onFailure { e -> _state.update { it.copy(loading = false, hits = emptyList(), error = e.userMessage()) } }
        }
    }

    fun loadMore() {
        val s = _state.value
        if (s.loading || s.loadingMore || s.endReached || s.hits.isEmpty()) return
        val query = s.toQuery(offset = s.hits.size.toUInt())
        _state.update { it.copy(loadingMore = true) }
        searchJob = viewModelScope.launch {
            ffiCatching { magApp.magnetar().search(query, magApp.settings.value.hasServer) }
                .onSuccess { page ->
                    _state.update {
                        val known = it.hits.mapTo(HashSet()) { h -> h.infohash }
                        val merged = it.hits + page.hits.filter { h -> h.infohash !in known }
                        it.copy(
                            hits = merged, total = page.total, loadingMore = false, source = page.source,
                            serverError = page.serverError,
                            endReached = page.hits.size.toUInt() < PAGE || merged.size.toULong() >= page.total,
                        )
                    }
                }
                .onFailure { e ->
                    _state.update { it.copy(loadingMore = false) }
                    message(e.userMessage())
                }
        }
    }

    fun saveSearch(name: String, notify: Boolean) = launchFfi { m ->
        val q = _state.value.toQuery()
        io { m.saveSearch(FfiSavedSearch(id = 0, name = name, query = q, notify = notify, lastChecked = null)) }
        message("Saved \"$name\"")
    }
}

package app.magnetar.ui.stats

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import app.magnetar.api.ApiException
import app.magnetar.api.Breakdown
import app.magnetar.api.BreakdownRequest
import app.magnetar.api.BreakdownSet
import app.magnetar.api.Features
import app.magnetar.api.Health
import app.magnetar.api.SearchQuery
import app.magnetar.api.Sort
import app.magnetar.api.StatsHealth
import app.magnetar.api.Timeseries
import app.magnetar.data.Repository
import app.magnetar.ui.common.isOffline
import app.magnetar.ui.common.userMessage
import app.magnetar.ui.search.SearchFilters
import app.magnetar.ui.search.buildQuery
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.async
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.collectLatest
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.drop
import kotlinx.coroutines.flow.filter
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch

data class StatsUiState(
    val hasServer: Boolean = false,
    /** null until `/health` answered. */
    val health: Health? = null,
    /** `/health` was asked (so a null [health] means it did not answer). */
    val healthChecked: Boolean = false,
    val healthError: String? = null,
    val filters: SearchFilters = SearchFilters(),
    val breakdowns: BreakdownSet? = null,
    val years: Breakdown? = null,
    val loadingBreakdowns: Boolean = false,
    val breakdownError: String? = null,
    /** The breakdowns shown are a cached answer saved at this time. */
    val breakdownCachedAtMs: Long? = null,
    val stats: StatsHealth? = null,
    val statsError: String? = null,
    val statsCachedAtMs: Long? = null,
    val activity: Activity = Activity.TOTAL,
    val hourly: Boolean = false,
    val series: Timeseries? = null,
    val seriesLoading: Boolean = false,
    val seriesError: String? = null,
    /**
     * Features of the last `/health` answer from this server (cached), used while it is
     * unreachable: the cached charts are keyed by the request, which depends on the features.
     */
    val knownFeatures: List<String>? = null,
) {
    val features: Set<String> get() = (health?.features ?: knownFeatures).orEmpty().toSet()
    val hasStats: Boolean get() = Features.STATS in features
    val hasGraphs: Boolean get() = Features.GRAPHS in features
    val hasDht: Boolean get() = Features.DHT in features || stats?.dht != null
}

/**
 * The Stats tab (D47, docs/STATS.md): `/stats/health`, breakdowns with cross-filtering, one
 * activity series. Cross-filter requests are one flow with `collectLatest`: a newer tap cancels
 * the counts in flight. Answers are cached (D83) and shown at once on the next visit or offline.
 */
class StatsViewModel(
    private val repo: Repository,
    private val clock: () -> Long = System::currentTimeMillis,
) : ViewModel() {
    private val _state = MutableStateFlow(StatsUiState(hasServer = repo.settings.value.hasServer))
    val state: StateFlow<StatsUiState> = _state.asStateFlow()

    /** Bumped for each breakdown load; the latest wins. */
    private val breakdownRequests = MutableStateFlow(0)
    private val seriesRequests = MutableStateFlow(0)

    init {
        // 0 = nothing asked yet; a collector that starts late still sees the latest request.
        viewModelScope.launch { breakdownRequests.filter { it > 0 }.collectLatest { loadBreakdowns() } }
        viewModelScope.launch { seriesRequests.filter { it > 0 }.collectLatest { loadSeries() } }
        // Another server, token or adult switch changes every count.
        viewModelScope.launch {
            repo.settings.map { listOf(it.serverUrl, it.apiToken, it.showAdult) }.distinctUntilChanged().drop(1).collect {
                refresh()
            }
        }
        refresh()
    }

    fun refresh() {
        val hasServer = repo.settings.value.hasServer
        _state.update { it.copy(hasServer = hasServer) }
        if (!hasServer) {
            _state.update { StatsUiState(hasServer = false, healthChecked = true) }
            return
        }
        viewModelScope.launch {
            val h = repo.refreshHealth(force = true)
            val known = if (h != null) {
                repo.writeCache(CACHE_FEATURES, "", Health.serializer(), h)
                h.features
            } else {
                repo.readCache(CACHE_FEATURES, "", Health.serializer())?.value?.features
            }
            _state.update {
                it.copy(health = h, knownFeatures = known, healthChecked = true, healthError = if (h == null) "Server unreachable" else null)
            }
            if (h?.has(Features.STATS) == true || h == null) {
                breakdownRequests.value++
                seriesRequests.value++
                loadStats()
            }
        }
    }

    private fun query(filters: SearchFilters): SearchQuery {
        val features = _state.value.features
        return buildQuery(
            text = "",
            filters = filters.supportedBy(features),
            sort = Sort.RELEVANCE,
            descending = true,
            includeAdult = repo.settings.value.showAdult,
            limit = 1,
            nowSec = 0,
        )
    }

    private suspend fun loadBreakdowns() {
        val s = _state.value
        val features = s.features
        val q = query(s.filters)
        val main = BreakdownRequest(q, breakdownFields(features), top = 10)
        val years = BreakdownRequest(q, listOf("year"), top = 50)
        val mainKey = repo.keyOf(BreakdownRequest.serializer(), main)
        val yearKey = repo.keyOf(BreakdownRequest.serializer(), years)
        val cached = repo.readCache(CACHE_BREAKDOWN, mainKey, BreakdownSet.serializer())
        val cachedYears = repo.readCache(CACHE_BREAKDOWN, yearKey, BreakdownSet.serializer())
        _state.update {
            it.copy(
                loadingBreakdowns = true,
                breakdownError = null,
                breakdowns = cached?.value ?: it.breakdowns,
                years = cachedYears?.value?.field("year") ?: it.years,
                breakdownCachedAtMs = cached?.savedAtMs,
            )
        }
        if (s.health == null && cached == null) {
            _state.update { it.copy(loadingBreakdowns = false, breakdownError = "Server unreachable") }
            return
        }
        try {
            val service = repo.service()
            coroutineScope {
                val y = async { runCatching { service.breakdown(years) } }
                val set = service.breakdown(main)
                repo.writeCache(CACHE_BREAKDOWN, mainKey, BreakdownSet.serializer(), set)
                val ys = y.await().getOrNull()
                if (ys != null) repo.writeCache(CACHE_BREAKDOWN, yearKey, BreakdownSet.serializer(), ys)
                _state.update {
                    it.copy(
                        breakdowns = set,
                        years = ys?.field("year"),
                        loadingBreakdowns = false,
                        breakdownCachedAtMs = null,
                    )
                }
            }
        } catch (e: CancellationException) {
            throw e
        } catch (e: ApiException) {
            _state.update {
                it.copy(
                    loadingBreakdowns = false,
                    breakdownError = e.userMessage(),
                    // A cached answer stays offline; another error clears it (it would mislead).
                    breakdowns = if (e.isOffline) it.breakdowns else null,
                    years = if (e.isOffline) it.years else null,
                    breakdownCachedAtMs = if (e.isOffline) it.breakdownCachedAtMs else null,
                )
            }
        }
    }

    private fun loadStats() = viewModelScope.launch {
        val cached = repo.readCache(CACHE_HEALTH, "", StatsHealth.serializer())
        if (cached != null && _state.value.stats == null) {
            _state.update { it.copy(stats = cached.value, statsCachedAtMs = cached.savedAtMs) }
        }
        try {
            val h = repo.service().statsHealth()
            repo.writeCache(CACHE_HEALTH, "", StatsHealth.serializer(), h)
            _state.update { it.copy(stats = h, statsError = null, statsCachedAtMs = null) }
        } catch (e: CancellationException) {
            throw e
        } catch (e: ApiException) {
            _state.update { it.copy(statsError = e.userMessage()) }
        }
    }

    private suspend fun loadSeries() {
        val s = _state.value
        val bucket = if (s.hourly) "hour" else "day"
        _state.update { it.copy(seriesLoading = true, seriesError = null) }
        try {
            val set = repo.service().timeseries(listOf(s.activity.metric), bucket)
            _state.update { it.copy(series = set.series.firstOrNull { x -> x.metric == s.activity.metric }, seriesLoading = false) }
        } catch (e: CancellationException) {
            throw e
        } catch (e: ApiException) {
            _state.update { it.copy(series = null, seriesLoading = false, seriesError = e.userMessage()) }
        }
    }

    fun pickActivity(a: Activity) {
        if (a == _state.value.activity) return
        _state.update { it.copy(activity = a, series = null) }
        seriesRequests.value++
    }

    fun setHourly(hourly: Boolean) {
        if (hourly == _state.value.hourly) return
        _state.update { it.copy(hourly = hourly, series = null) }
        seriesRequests.value++
    }

    /** Tap on a bar, slice or year: cross-filter every other chart on it. */
    fun toggle(field: String, value: String) {
        if (field !in FILTERABLE) return
        _state.update { it.copy(filters = it.filters.toggleStat(field, value)) }
        breakdownRequests.value++
    }

    fun clearFilters() {
        if (_state.value.filters == SearchFilters()) return
        _state.update { it.copy(filters = SearchFilters()) }
        breakdownRequests.value++
    }

    /**
     * A search with the active cross-filters, plus [field] = [value] when given (long press),
     * for the Search tab ([Repository.pendingSearch]). A value that is not a search filter
     * (a publisher category) becomes the search text.
     */
    fun searchFor(field: String? = null, value: String? = null): SearchQuery {
        var f = _state.value.filters
        var text = ""
        if (field != null && value != null) {
            if (field in FILTERABLE) {
                if (value !in f.selectedStat(field)) f = f.toggleStat(field, value)
            } else {
                text = value
            }
        }
        return buildQuery(
            text = text,
            filters = f,
            sort = Sort.RELEVANCE,
            descending = true,
            includeAdult = repo.settings.value.showAdult,
            nowSec = clock() / 1000,
        )
    }

    fun openSearch(field: String? = null, value: String? = null) {
        repo.pendingSearch.value = searchFor(field, value)
    }

    companion object {
        const val CACHE_BREAKDOWN = "stats/breakdown"
        const val CACHE_HEALTH = "stats/health"
        const val CACHE_FEATURES = "stats/features"
    }
}

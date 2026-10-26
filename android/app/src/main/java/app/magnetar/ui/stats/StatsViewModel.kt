package app.magnetar.ui.stats

import android.app.Application
import androidx.lifecycle.viewModelScope
import app.magnetar.ffi.FfiBreakdown
import app.magnetar.ffi.FfiBreakdownSet
import app.magnetar.ffi.FfiCategory
import app.magnetar.ffi.FfiStatsHealth
import app.magnetar.ffi.FfiTimeseries
import app.magnetar.ffiCatching
import app.magnetar.ui.charts.ValueUnit
import app.magnetar.ui.common.BaseViewModel
import app.magnetar.ui.search.SearchFilters
import app.magnetar.ui.search.SearchUiState
import app.magnetar.ui.search.toQuery
import app.magnetar.userMessage
import kotlinx.coroutines.Job
import kotlinx.coroutines.async
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch

/** One activity chart the user can pick (D47 metrics). */
enum class Activity(val metric: String, val label: String, val unit: ValueUnit) {
    TOTAL("torrents_total", "Torrents over time", ValueUnit.COUNT),
    ADDED("torrents_added", "Added", ValueUnit.COUNT),
    PUBLISHED("torrents_published", "Published", ValueUnit.COUNT),
    CRAWL("crawl_events", "Crawled events", ValueUnit.COUNT),
    RELAYS("relays_discovered", "Relays found", ValueUnit.COUNT),
    CHECKS("liveness_checks", "Seed checks", ValueUnit.COUNT),
    DB("db_size", "Database size", ValueUnit.BYTES),
}

/** The breakdown charts, in screen order. */
val BREAKDOWN_FIELDS = listOf(
    "language", "resolution", "category", "source", "codec", "hdr", "audio", "genre", "media_kind", "subcategory",
)

/** Chart titles. */
val FIELD_TITLES = mapOf(
    "language" to "Audio languages", "resolution" to "Resolutions", "category" to "Categories",
    "source" to "Sources", "codec" to "Video codecs", "hdr" to "HDR", "audio" to "Audio formats",
    "genre" to "Genres", "media_kind" to "Movies, series, anime", "subcategory" to "Publisher categories",
    "year" to "Years",
)

/** Fields that can cross-filter (the others only show counts). */
val FILTERABLE = setOf("language", "resolution", "category", "source", "codec", "hdr", "audio", "genre", "media_kind", "year")

data class StatsUiState(
    val hasServer: Boolean = false,
    /** null until asked, or when the server did not answer. */
    val features: List<String>? = null,
    /** The server was asked for its features (so a null [features] means it did not answer). */
    val featuresChecked: Boolean = false,
    val filters: SearchFilters = SearchFilters(),
    val breakdowns: FfiBreakdownSet? = null,
    val years: FfiBreakdown? = null,
    val loadingBreakdowns: Boolean = false,
    val breakdownError: String? = null,
    val health: FfiStatsHealth? = null,
    val activity: Activity = Activity.TOTAL,
    val hourly: Boolean = false,
    val series: FfiTimeseries? = null,
    val seriesError: String? = null,
) {
    val stats: Boolean get() = features?.contains("stats") == true
    val graphs: Boolean get() = features?.contains("graphs") == true
}

/** Cross-filter on [value] of [field] switched on or off (categories and years included). */
fun SearchFilters.toggleStat(field: String, value: String): SearchFilters = when (field) {
    "category" -> FfiCategory.entries.firstOrNull { it.name.equals(value, true) }
        ?.let { c -> copy(categories = if (c in categories) categories - c else categories + c) } ?: this
    "year" -> value.toIntOrNull()?.let { y ->
        if (minYear == y && maxYear == y) copy(minYear = null, maxYear = null) else copy(minYear = y, maxYear = y)
    } ?: this
    else -> toggle(field, value)
}

/** Values of [field] the cross-filter selects. */
fun SearchFilters.selectedStat(field: String): Set<String> = when (field) {
    "category" -> categories.mapTo(HashSet()) { it.name.lowercase() }
    "year" -> if (minYear != null && minYear == maxYear) setOf(minYear.toString()) else emptySet()
    else -> selected(field)
}

class StatsViewModel(application: Application) : BaseViewModel(application) {
    private val _state = MutableStateFlow(StatsUiState())
    val state: StateFlow<StatsUiState> = _state.asStateFlow()
    private var breakdownJob: Job? = null
    private var seriesJob: Job? = null

    init {
        refresh()
    }

    fun refresh() {
        val hasServer = magApp.settings.value.hasServer
        _state.update { it.copy(hasServer = hasServer) }
        viewModelScope.launch {
            val features = if (hasServer) ffiCatching { magApp.magnetar().serverFeatures() }.getOrNull() else null
            _state.update { it.copy(features = features, featuresChecked = true) }
            loadBreakdowns()
            if (_state.value.stats) {
                loadSeries()
                loadHealth()
            } else {
                _state.update { it.copy(health = null, series = null) }
            }
        }
    }

    private fun loadBreakdowns() {
        breakdownJob?.cancel()
        _state.update { it.copy(loadingBreakdowns = true) }
        val filters = _state.value.filters
        val query = SearchUiState(filters = filters).toQuery()
        breakdownJob = viewModelScope.launch {
            val m = magApp.magnetar()
            val main = async { ffiCatching { m.statsBreakdown(query, BREAKDOWN_FIELDS, 10u) } }
            // Years as a histogram: many more values.
            val years = async { ffiCatching { m.statsBreakdown(query, listOf("year"), 50u) } }
            main.await()
                .onSuccess { set ->
                    _state.update {
                        it.copy(
                            breakdowns = set,
                            years = years.await().getOrNull()?.breakdowns?.firstOrNull(),
                            loadingBreakdowns = false,
                            breakdownError = null,
                        )
                    }
                }
                .onFailure { e -> _state.update { it.copy(loadingBreakdowns = false, breakdownError = e.userMessage()) } }
        }
    }

    private fun loadHealth() = viewModelScope.launch {
        val h = ffiCatching { magApp.magnetar().statsHealth() }.getOrNull()
        _state.update { it.copy(health = h) }
    }

    private fun loadSeries() {
        seriesJob?.cancel()
        val s = _state.value
        seriesJob = viewModelScope.launch {
            ffiCatching {
                magApp.magnetar().statsTimeseries(listOf(s.activity.metric), if (s.hourly) "hour" else "day", null)
            }
                .onSuccess { set -> _state.update { it.copy(series = set.series.firstOrNull(), seriesError = null) } }
                .onFailure { e -> _state.update { it.copy(series = null, seriesError = e.userMessage()) } }
        }
    }

    fun pickActivity(a: Activity) {
        _state.update { it.copy(activity = a, series = null) }
        loadSeries()
    }

    fun setHourly(hourly: Boolean) {
        _state.update { it.copy(hourly = hourly, series = null) }
        loadSeries()
    }

    /** Tap on a bar or slice: cross-filter every other chart on it (server only). */
    fun toggle(field: String, value: String) {
        if (field !in FILTERABLE || _state.value.breakdowns?.source != "server") return
        _state.update { it.copy(filters = it.filters.toggleStat(field, value)) }
        loadBreakdowns()
    }

    fun clearFilters() {
        _state.update { it.copy(filters = SearchFilters()) }
        loadBreakdowns()
    }

    /**
     * Run a search with the current cross-filters, plus [field] = [value] when given (long
     * press); the Search tab picks it up.
     */
    fun openSearch(field: String? = null, value: String? = null) {
        var f = _state.value.filters
        if (field != null && value != null) {
            if (field in FILTERABLE && value !in f.selectedStat(field)) f = f.toggleStat(field, value)
        }
        val text = if (field != null && value != null && field !in FILTERABLE) value else ""
        magApp.pendingSearch.value = SearchUiState(text = text, filters = f).toQuery()
    }
}

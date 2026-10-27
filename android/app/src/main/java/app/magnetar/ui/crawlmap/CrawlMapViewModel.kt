package app.magnetar.ui.crawlmap

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import app.magnetar.api.ApiException
import app.magnetar.api.CrawlMap
import app.magnetar.data.Repository
import app.magnetar.ui.common.isOffline
import app.magnetar.ui.common.userMessage
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.collectLatest
import kotlinx.coroutines.flow.filter
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch

data class CrawlMapUiState(
    val layer: MapLayer = MapLayer.DHT,
    val window: MapWindow = MapWindow.DAY,
    /** The metric picked by the user; null = the answer's first. */
    val metric: String? = null,
    val map: CrawlMap? = null,
    val loading: Boolean = true,
    val error: String? = null,
    /** The map shown is a cached answer saved at this time. */
    val cachedAtMs: Long? = null,
    /** 404: a server without `crawl_map`. */
    val missing: Boolean = false,
    /** The tapped country (ISO alpha-2). */
    val selected: String? = null,
) {
    val metricIndex: Int get() = map?.let { metricIndex(it, metric) } ?: 0
    val metricName: String? get() = map?.metrics?.getOrNull(metricIndex)
}

/**
 * The crawl map (feature `crawl_map`): one `GET /api/v1/stats/map` per layer and window, cached
 * per request (D83) and shown at once or offline with its age. A newer choice cancels the
 * request in flight.
 */
class CrawlMapViewModel(private val repo: Repository) : ViewModel() {
    private val _state = MutableStateFlow(CrawlMapUiState())
    val state: StateFlow<CrawlMapUiState> = _state.asStateFlow()
    private val requests = MutableStateFlow(0)

    init {
        viewModelScope.launch { requests.filter { it > 0 }.collectLatest { load() } }
        refresh()
    }

    fun refresh() {
        requests.value++
    }

    fun setLayer(layer: MapLayer) {
        if (layer == _state.value.layer) return
        // Another layer has other metrics: back to its first one.
        _state.update { it.copy(layer = layer, metric = null, map = null, cachedAtMs = null, selected = null) }
        refresh()
    }

    fun setWindow(window: MapWindow) {
        if (window == _state.value.window) return
        _state.update { it.copy(window = window, map = null, cachedAtMs = null) }
        refresh()
    }

    fun pickMetric(metric: String) = _state.update { it.copy(metric = metric) }

    /** Tap on a country: select it, tap again (or on the sea, null) to clear. */
    fun select(code: String?) = _state.update { it.copy(selected = code?.uppercase()?.takeIf { c -> c != it.selected }) }

    private suspend fun load() {
        val s = _state.value
        val key = cacheKey(s.layer, s.window)
        val cached = repo.readCache(CACHE, key, CrawlMap.serializer())
        _state.update {
            it.copy(
                loading = true,
                error = null,
                missing = false,
                map = cached?.value ?: it.map,
                cachedAtMs = cached?.savedAtMs ?: it.cachedAtMs,
            )
        }
        try {
            val m = repo.service().crawlMap(s.layer.api, s.window.api)
            repo.writeCache(CACHE, key, CrawlMap.serializer(), m)
            _state.update { it.copy(map = m, loading = false, cachedAtMs = null) }
        } catch (e: CancellationException) {
            throw e
        } catch (e: ApiException.NotFound) {
            _state.update { it.copy(loading = false, missing = true, map = null, cachedAtMs = null) }
        } catch (e: ApiException) {
            _state.update {
                it.copy(
                    loading = false,
                    error = e.userMessage(),
                    // A cached map stays offline; another error clears it (it would mislead).
                    map = if (e.isOffline) it.map else null,
                    cachedAtMs = if (e.isOffline) it.cachedAtMs else null,
                )
            }
        }
    }

    companion object {
        const val CACHE = "stats/map"

        fun cacheKey(layer: MapLayer, window: MapWindow) = "layer=${layer.api}&window=${window.api}"
    }
}

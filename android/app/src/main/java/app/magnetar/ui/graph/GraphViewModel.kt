package app.magnetar.ui.graph

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import app.magnetar.api.ApiException
import app.magnetar.api.Graph
import app.magnetar.data.Repository
import app.magnetar.ui.common.isOffline
import app.magnetar.ui.common.userMessage
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineDispatcher
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.ensureActive
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

data class GraphUiState(
    val loading: Boolean = true,
    val graph: Graph? = null,
    /** Node positions (layout units), updated while the layout settles. */
    val x: FloatArray = FloatArray(0),
    val y: FloatArray = FloatArray(0),
    val settled: Boolean = false,
    val selected: Int? = null,
    val error: String? = null,
    /** 404: unknown title / torrent, or a server older than D47. */
    val missing: Boolean = false,
    val cachedAtMs: Long? = null,
)

/**
 * Loads one D47 graph and lays it out on [layoutDispatcher], publishing positions every few
 * steps; leaving the screen cancels the layout.
 */
class GraphViewModel(
    private val repo: Repository,
    /** `wot` | `relays` | `media/{id}` | `swarm/{infohash}`. */
    val path: String,
    private val layoutDispatcher: CoroutineDispatcher = Dispatchers.Default,
) : ViewModel() {
    private val _state = MutableStateFlow(GraphUiState())
    val state: StateFlow<GraphUiState> = _state.asStateFlow()
    private var loadJob: Job? = null
    private var layoutJob: Job? = null

    init {
        refresh()
    }

    fun refresh() {
        loadJob?.cancel()
        _state.update { it.copy(loading = true, error = null) }
        loadJob = viewModelScope.launch {
            val cached = repo.readCache(CACHE, path, Graph.serializer())
            if (cached != null && _state.value.graph == null) {
                show(cached.value, cached.savedAtMs)
            }
            try {
                val g = repo.service().graph(path)
                repo.writeCache(CACHE, path, Graph.serializer(), g)
                show(g, null)
            } catch (e: CancellationException) {
                throw e
            } catch (e: ApiException.NotFound) {
                layoutJob?.cancel()
                _state.value = GraphUiState(loading = false, missing = true)
            } catch (e: ApiException) {
                _state.update {
                    if (e.isOffline && it.graph != null) {
                        it.copy(loading = false, error = e.userMessage())
                    } else {
                        GraphUiState(loading = false, error = e.userMessage())
                    }
                }
            }
        }
    }

    private fun show(g: Graph, cachedAtMs: Long?) {
        val same = _state.value.graph == g
        _state.update { it.copy(loading = false, graph = g, cachedAtMs = cachedAtMs, error = null) }
        if (!same) layout(g)
    }

    private fun layout(g: Graph) {
        layoutJob?.cancel()
        _state.update { it.copy(selected = null, settled = false, x = FloatArray(0), y = FloatArray(0)) }
        layoutJob = viewModelScope.launch {
            val l = ForceLayout(g.nodes.size, g.edges.map { it.source to it.target }, pinned = subjectOf(g))
            withContext(layoutDispatcher) {
                l.run { x, y ->
                    ensureActive()
                    _state.update { it.copy(x = x, y = y) }
                }
            }
            _state.update { it.copy(x = l.x.copyOf(), y = l.y.copyOf(), settled = true) }
        }
    }

    fun select(i: Int?) = _state.update { it.copy(selected = i?.takeIf { n -> n in it.graph?.nodes.orEmpty().indices }) }

    companion object {
        const val CACHE = "graphs"

        /** Kinds of a graph's subject (its first node), pinned at the centre. */
        val SUBJECT_KINDS = setOf("root", "media", "torrent")

        fun subjectOf(g: Graph): Int? = g.nodes.firstOrNull()?.takeIf { it.kind in SUBJECT_KINDS }?.let { 0 }
    }
}

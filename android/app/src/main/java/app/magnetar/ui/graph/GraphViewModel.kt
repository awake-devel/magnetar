package app.magnetar.ui.graph

import android.app.Application
import androidx.lifecycle.SavedStateHandle
import androidx.lifecycle.viewModelScope
import app.magnetar.ffi.FfiGraph
import app.magnetar.ffiCatching
import app.magnetar.ui.common.BaseViewModel
import app.magnetar.userMessage
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.ensureActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

data class GraphUiState(
    val loading: Boolean = true,
    val graph: FfiGraph? = null,
    /** Node positions (layout units), updated while the layout settles. */
    val x: FloatArray = FloatArray(0),
    val y: FloatArray = FloatArray(0),
    val settled: Boolean = false,
    val selected: Int? = null,
    val error: String? = null,
    /** The server answered 404: unknown title/torrent or a server older than D47. */
    val missing: Boolean = false,
)

/** Loads one graph (D47) and lays it out on [Dispatchers.Default], publishing every few steps. */
class GraphViewModel(application: Application, handle: SavedStateHandle) : BaseViewModel(application) {
    /** "wot" | "relays" | "media/{id}" | "swarm/{infohash}". */
    val path: String = checkNotNull(handle["path"])

    private val _state = MutableStateFlow(GraphUiState())
    val state: StateFlow<GraphUiState> = _state.asStateFlow()
    private var layoutJob: Job? = null

    val hasServer: Boolean get() = magApp.settings.value.hasServer

    init {
        refresh()
    }

    fun refresh() {
        if (!hasServer) {
            _state.value = GraphUiState(loading = false)
            return
        }
        _state.update { it.copy(loading = true, error = null) }
        viewModelScope.launch {
            ffiCatching { magApp.magnetar().graph(path) }
                .onSuccess { g ->
                    if (g == null) {
                        _state.value = GraphUiState(loading = false, missing = true)
                    } else {
                        _state.value = GraphUiState(loading = false, graph = g)
                        layout(g)
                    }
                }
                .onFailure { e -> _state.update { it.copy(loading = false, error = e.userMessage()) } }
        }
    }

    private fun layout(g: FfiGraph) {
        layoutJob?.cancel()
        layoutJob = viewModelScope.launch {
            val edges = g.edges.map { it.source.toInt() to it.target.toInt() }
            val subject = g.nodes.firstOrNull()?.takeIf { it.kind in SUBJECT_KINDS }?.let { 0 }
            val l = ForceLayout(g.nodes.size, edges, pinned = subject)
            withContext(Dispatchers.Default) {
                l.run { x, y ->
                    // Leaving the screen stops the layout at its next progress report.
                    ensureActive()
                    _state.update { it.copy(x = x, y = y) }
                }
            }
            _state.update { it.copy(x = l.x.copyOf(), y = l.y.copyOf(), settled = true) }
        }
    }

    fun select(i: Int?) = _state.update { it.copy(selected = i) }

    fun npub(hex: String): String = runCatching { app.magnetar.ffi.npubEncode(hex) }.getOrDefault(hex)

    companion object {
        /** Kinds of the graph's subject (first node), pinned at the centre. */
        val SUBJECT_KINDS = setOf("root", "media", "torrent")
    }
}

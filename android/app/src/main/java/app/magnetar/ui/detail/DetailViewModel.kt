package app.magnetar.ui.detail

import android.app.Application
import androidx.lifecycle.SavedStateHandle
import androidx.lifecycle.viewModelScope
import app.magnetar.download.DownloadService
import app.magnetar.ffi.FfiException
import app.magnetar.ffi.FfiSavedSearch
import app.magnetar.ffi.FfiSearchQuery
import app.magnetar.ffi.FfiSort
import app.magnetar.ffi.FfiTorrentDetail
import app.magnetar.ffiCatching
import app.magnetar.ui.common.BaseViewModel
import app.magnetar.ui.common.fallbackMagnet
import app.magnetar.userMessage
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.receiveAsFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch

data class DetailUiState(
    val loading: Boolean = true,
    val detail: FfiTorrentDetail? = null,
    val error: String? = null,
    val checkingLiveness: Boolean = false,
)

class DetailViewModel(application: Application, handle: SavedStateHandle) : BaseViewModel(application) {
    val infohash: String = checkNotNull(handle["infohash"])
    /** The magnet: link that opened us, when we came from outside. */
    private val incomingMagnet: String? = handle["magnet"]

    private val _state = MutableStateFlow(DetailUiState())
    val state: StateFlow<DetailUiState> = _state.asStateFlow()

    val settings get() = magApp.settings

    /** Magnets to hand to an external client (phone engine unavailable). */
    private val _external = Channel<String>(Channel.BUFFERED)
    val externalOpen = _external.receiveAsFlow()

    val magnet: String
        get() = _state.value.detail?.magnet?.takeIf { it.isNotBlank() } ?: incomingMagnet ?: fallbackMagnet(infohash)

    init {
        refresh()
    }

    fun refresh() {
        _state.update { it.copy(loading = true, error = null) }
        viewModelScope.launch {
            ffiCatching { magApp.magnetar().detail(infohash, magApp.settings.value.hasServer) }
                .onSuccess { d -> _state.update { it.copy(loading = false, detail = d) } }
                .onFailure { e -> _state.update { it.copy(loading = false, error = e.userMessage()) } }
        }
    }

    fun checkLiveness() {
        _state.update { it.copy(checkingLiveness = true) }
        launchFfi(onError = { msg -> _state.update { it.copy(checkingLiveness = false) }; message(msg) }) { m ->
            val live = m.checkLiveness(infohash)
            _state.update { s -> s.copy(checkingLiveness = false, detail = s.detail?.copy(liveness = live)) }
            if (live == null) message("No liveness data")
        }
    }

    fun togglePinned() = launchFfi { m ->
        val pinned = !(_state.value.detail?.summary?.pinned ?: false)
        io { m.setPinned(infohash, pinned) }
        updateSummary { it.copy(pinned = pinned) }
        message(if (pinned) "Pinned (kept through cache cleanup)" else "Unpinned")
    }

    fun toggleHidden() = launchFfi { m ->
        val hidden = !(_state.value.detail?.summary?.hidden ?: false)
        io { m.setHidden(infohash, hidden) }
        updateSummary { it.copy(hidden = hidden) }
        message(if (hidden) "Hidden from results" else "Visible in results again")
    }

    fun blockPublisher() = launchFfi { m ->
        val d = _state.value.detail ?: return@launchFfi
        m.blockPublisher(d.summary.pubkey, true)
        message("Publisher blocked")
        refresh()
    }

    fun downloadOnPhone() {
        val magnet = magnet
        viewModelScope.launch {
            ffiCatching { magApp.magnetar().downloadStart(magnet, false) }
                .onSuccess {
                    DownloadService.start(magApp)
                    message("Downloading ${it.name.ifBlank { "torrent" }}")
                }
                .onFailure { e ->
                    if (e is FfiException.Invalid) {
                        // Phone engine unavailable (ARCHITECTURE.md §8.3): hand off instead.
                        message("${e.msg} Opening in another app…")
                        _external.trySend(magnet)
                    } else {
                        message(e.userMessage())
                    }
                }
        }
    }

    fun downloadOnServer() = launchFfi { m ->
        val dl = m.downloadStart(magnet, true)
        message("Server is downloading ${dl.name.ifBlank { "torrent" }}")
    }

    fun saveSearch(name: String, notify: Boolean) = launchFfi { m ->
        val q = queryToSave().copy(offset = 0u)
        io { m.saveSearch(FfiSavedSearch(id = 0, name = name, query = q, notify = notify, lastChecked = null)) }
        message("Saved \"$name\"")
    }

    fun suggestedSearchName(): String =
        queryToSave().text.ifBlank { infohash }

    /**
     * The last text search, else this torrent's title. A blank last query (plain browse) is not
     * used: the dialog would offer the title as the name while saving "any text".
     */
    private fun queryToSave(): FfiSearchQuery =
        magApp.lastQuery?.takeIf { it.text.isNotBlank() } ?: defaultQuery()

    private fun defaultQuery() = FfiSearchQuery(
        text = _state.value.detail?.summary?.title ?: "",
        categories = emptyList(), minSize = null, maxSize = null, since = null, until = null,
        minSeeders = null, hideDead = false, minTrust = null, sort = FfiSort.RELEVANCE, descending = true,
        limit = 50u, offset = 0u, includeHidden = false,
    )

    private fun updateSummary(f: (app.magnetar.ffi.FfiTorrentSummary) -> app.magnetar.ffi.FfiTorrentSummary) {
        _state.update { s -> s.copy(detail = s.detail?.let { it.copy(summary = f(it.summary)) }) }
    }
}

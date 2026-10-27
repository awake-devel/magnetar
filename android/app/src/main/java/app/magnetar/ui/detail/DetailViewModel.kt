package app.magnetar.ui.detail

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import app.magnetar.api.ApiException
import app.magnetar.api.Features
import app.magnetar.api.LivenessHistory
import app.magnetar.api.TorrentDetail
import app.magnetar.data.Repository
import app.magnetar.ui.common.fallbackMagnet
import app.magnetar.ui.common.isOffline
import app.magnetar.ui.common.userMessage
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.receiveAsFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch

data class DetailUiState(
    val loading: Boolean = true,
    val detail: TorrentDetail? = null,
    val error: String? = null,
    val offline: Boolean = false,
    /** The shown detail is the cache's (unix ms); null = live. */
    val cachedAtMs: Long? = null,
    val checking: Boolean = false,
    /** Seeders over time (D45); null without `liveness_history` or before it loaded. */
    val history: LivenessHistory? = null,
)

/** Torrent detail (docs/APP.md §1). No download buttons: *Open magnet* / *Copy magnet* only. */
class DetailViewModel(
    private val repo: Repository,
    val infohash: String,
    /** Wait before asking once more for a poster/overview the server is still fetching (D42). */
    private val mediaRetryMs: Long = 4_000,
) : ViewModel() {
    private val _state = MutableStateFlow(DetailUiState())
    val state: StateFlow<DetailUiState> = _state.asStateFlow()

    private val messagesChannel = Channel<String>(Channel.BUFFERED)
    val messages: Flow<String> = messagesChannel.receiveAsFlow()

    val magnet: String get() = _state.value.detail?.magnet?.takeIf { it.isNotBlank() } ?: fallbackMagnet(infohash)

    init {
        refresh()
    }

    fun refresh() {
        _state.update { it.copy(loading = true, error = null) }
        viewModelScope.launch {
            if (_state.value.detail == null) {
                repo.readCache(CACHE_DETAIL, infohash, TorrentDetail.serializer())?.let { c ->
                    _state.update { if (it.detail == null) it.copy(detail = c.value, cachedAtMs = c.savedAtMs) else it }
                }
            }
            try {
                val d = repo.service().detail(infohash)
                _state.update { it.copy(loading = false, detail = d, cachedAtMs = null, offline = false) }
                repo.writeCache(CACHE_DETAIL, infohash, TorrentDetail.serializer(), d)
                loadHistory()
                retryMedia(d)
            } catch (e: CancellationException) {
                throw e
            } catch (e: ApiException) {
                _state.update { it.copy(loading = false, error = e.userMessage(), offline = e.isOffline) }
            }
        }
    }

    /**
     * The server answers with the media it has and asks TMDB in the background (D42), so the
     * overview or poster may arrive a moment later: ask once more.
     */
    private suspend fun retryMedia(d: TorrentDetail) {
        val m = d.media ?: return
        if (m.poster != null && m.overview != null) return
        delay(mediaRetryMs)
        try {
            val again = repo.service().detail(infohash)
            if (again.media != null) {
                _state.update { it.copy(detail = again) }
                repo.writeCache(CACHE_DETAIL, infohash, TorrentDetail.serializer(), again)
            }
        } catch (e: CancellationException) {
            throw e
        } catch (_: ApiException) {
        }
    }

    private suspend fun loadHistory() {
        repo.refreshHealth()
        if (!repo.has(Features.LIVENESS_HISTORY)) return
        try {
            val h = repo.service().livenessHistory(infohash, 90)
            _state.update { it.copy(history = h) }
        } catch (e: CancellationException) {
            throw e
        } catch (_: ApiException) {
        }
    }

    /** "Check now": the server asks the trackers and the DHT (up to 45 s). */
    fun checkNow() {
        if (_state.value.checking) return
        _state.update { it.copy(checking = true) }
        viewModelScope.launch {
            try {
                val live = repo.service().checkLiveness(infohash)
                _state.update { s ->
                    s.copy(
                        checking = false,
                        detail = s.detail?.let { d ->
                            d.copy(
                                liveness = live,
                                summary = d.summary.copy(
                                    seeders = live.seeders, leechers = live.leechers, bestPeers = live.bestPeers,
                                    dead = live.bestPeers?.let { it == 0 },
                                ),
                            )
                        },
                    )
                }
                loadHistory()
            } catch (e: CancellationException) {
                _state.update { it.copy(checking = false) }
                throw e
            } catch (e: ApiException) {
                _state.update { it.copy(checking = false) }
                messagesChannel.trySend("Check failed: ${e.userMessage()}")
            }
        }
    }

    companion object {
        const val CACHE_DETAIL = "detail"
    }
}

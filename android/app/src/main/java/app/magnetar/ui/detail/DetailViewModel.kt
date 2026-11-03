package app.magnetar.ui.detail

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import app.magnetar.api.ApiException
import app.magnetar.api.Features
import app.magnetar.api.LivenessHistory
import app.magnetar.api.ProbeCorrectionItem
import app.magnetar.api.ProbeCorrectionRequest
import app.magnetar.api.ProbeInfo
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
    /** The server takes track corrections (`probe_corrections`, D131): the card shows edit actions. */
    val canCorrect: Boolean = false,
    /** A correction is being sent. */
    val correcting: Boolean = false,
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

    /**
     * Bumped by every saved correction: a detail fetch that started before it may answer the
     * probe as it was, so that fetch keeps the shown probe ([withProbeSince]).
     */
    private var probeGen = 0

    /** [fetched], keeping the shown probe when a correction landed since the fetch started at [gen]. */
    private fun withProbeSince(fetched: TorrentDetail, gen: Int): TorrentDetail {
        val shown = _state.value.detail?.probe
        return if (gen != probeGen && shown != null) fetched.copy(probe = shown) else fetched
    }

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
                val gen = probeGen
                val d = withProbeSince(repo.service().detail(infohash), gen)
                _state.update { it.copy(loading = false, detail = d, cachedAtMs = null, offline = false) }
                repo.writeCache(CACHE_DETAIL, infohash, TorrentDetail.serializer(), d)
                loadHistory()
                retryMedia(d)
            } catch (e: CancellationException) {
                throw e
            } catch (e: ApiException) {
                _state.update { it.copy(loading = false, error = e.userMessage(), offline = e.isOffline) }
                // A cached detail keeps its edit actions when the features are known (D131).
                repo.refreshHealth()
                _state.update { it.copy(canCorrect = repo.has(Features.PROBE_CORRECTIONS)) }
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
            val gen = probeGen
            val again = withProbeSince(repo.service().detail(infohash), gen)
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
        _state.update { it.copy(canCorrect = repo.has(Features.PROBE_CORRECTIONS)) }
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

    /**
     * Sends track corrections (D130): on success the answer replaces the detail's probe and the
     * cache, [message] goes to the snackbar and [done] gets true; on failure the reason goes to
     * the snackbar and [done] gets false, so the editor keeps the draft for a retry. No offline
     * queue: corrections are rare.
     */
    fun saveCorrections(
        items: List<ProbeCorrectionItem>,
        message: String = "Correction saved",
        done: (Boolean) -> Unit = {},
    ) {
        if (items.isEmpty()) return done(true)
        correct(message, done) { repo.service().postProbeCorrections(infohash, ProbeCorrectionRequest(items = items)) }
    }

    /** Undo = delete the correction: back to the detection. */
    fun undoCorrection(id: Long, done: (Boolean) -> Unit = {}) {
        correct("Correction removed", done) { repo.service().deleteProbeCorrection(infohash, id) }
    }

    private fun correct(message: String, done: (Boolean) -> Unit, call: suspend () -> ProbeInfo) {
        if (_state.value.correcting) return done(false)
        _state.update { it.copy(correcting = true) }
        viewModelScope.launch {
            try {
                val probe = call()
                probeGen++
                val d = _state.value.detail?.copy(probe = probe)
                _state.update { it.copy(correcting = false, detail = d ?: it.detail) }
                if (d != null) repo.writeCache(CACHE_DETAIL, infohash, TorrentDetail.serializer(), d)
                messagesChannel.trySend(message)
                done(true)
            } catch (e: CancellationException) {
                _state.update { it.copy(correcting = false) }
                throw e
            } catch (e: ApiException) {
                _state.update { it.copy(correcting = false) }
                messagesChannel.trySend(
                    if (e.isOffline) "Offline: the correction was not saved" else "Couldn't save the correction: ${e.userMessage()}",
                )
                done(false)
            }
        }
    }

    companion object {
        const val CACHE_DETAIL = "detail"
    }
}

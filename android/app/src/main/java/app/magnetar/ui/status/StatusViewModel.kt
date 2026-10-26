package app.magnetar.ui.status

import android.app.Application
import androidx.lifecycle.viewModelScope
import androidx.work.WorkInfo
import androidx.work.WorkManager
import app.magnetar.ffi.FfiDhtStats
import app.magnetar.ffi.FfiLivenessStats
import app.magnetar.ffi.FfiStats
import app.magnetar.ffiCatching
import app.magnetar.ui.common.BaseViewModel
import app.magnetar.userMessage
import app.magnetar.work.CrawlSyncWorker
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch

data class StatusUiState(
    val stats: FfiStats? = null,
    val error: String? = null,
    /** null = not checked / no server. */
    val serverHealthy: Boolean? = null,
    val serverError: String? = null,
    /** The server's seed-check backlog (D45); null offline or from an older server. */
    val liveness: FfiLivenessStats? = null,
    /** The server's DHT crawler (D58); null when it runs none. */
    val dht: FfiDhtStats? = null,
    val checkingServer: Boolean = false,
    val workRunning: Boolean = false,
    val lastRun: List<String> = emptyList(),
    val lastRunErrors: List<String> = emptyList(),
)

class StatusViewModel(application: Application) : BaseViewModel(application) {
    private val _state = MutableStateFlow(StatusUiState())
    val state: StateFlow<StatusUiState> = _state.asStateFlow()

    val hasServer: Boolean get() = magApp.settings.value.hasServer

    init {
        refresh()
        viewModelScope.launch {
            WorkManager.getInstance(magApp).getWorkInfosForUniqueWorkFlow(CrawlSyncWorker.NOW_NAME).collect { infos ->
                val info = infos.firstOrNull() ?: return@collect
                val running = info.state == WorkInfo.State.RUNNING || info.state == WorkInfo.State.ENQUEUED
                val wasRunning = _state.value.workRunning
                _state.update {
                    it.copy(
                        workRunning = running,
                        lastRun = info.outputData.getNullableStringArray(CrawlSyncWorker.KEY_SUMMARY)?.filterNotNull() ?: it.lastRun,
                        lastRunErrors = info.outputData.getNullableStringArray(CrawlSyncWorker.KEY_ERRORS)?.filterNotNull()
                            ?: it.lastRunErrors,
                    )
                }
                if (wasRunning && !running) refresh()
            }
        }
    }

    fun refresh() {
        launchFfi(onError = { msg -> _state.update { it.copy(error = msg) } }) { m ->
            val stats = io { m.stats() }
            _state.update { it.copy(stats = stats, error = null) }
        }
        checkServer()
    }

    fun checkServer() {
        if (!hasServer) {
            _state.update { it.copy(serverHealthy = null, serverError = null, liveness = null, dht = null) }
            return
        }
        _state.update { it.copy(checkingServer = true) }
        viewModelScope.launch {
            ffiCatching { magApp.magnetar().serverHealth() }
                .onSuccess { ok -> _state.update { it.copy(checkingServer = false, serverHealthy = ok, serverError = null) } }
                .onFailure { e ->
                    _state.update { it.copy(checkingServer = false, serverHealthy = false, serverError = e.userMessage()) }
                }
        }
        viewModelScope.launch {
            val live = ffiCatching { magApp.magnetar().serverLiveness() }.getOrNull()
            _state.update { it.copy(liveness = live) }
        }
        viewModelScope.launch {
            val dht = ffiCatching { magApp.magnetar().serverDht() }.getOrNull()
            _state.update { it.copy(dht = dht) }
        }
    }

    fun checkSavedSearchesNow() = CrawlSyncWorker.runNow(magApp)
}

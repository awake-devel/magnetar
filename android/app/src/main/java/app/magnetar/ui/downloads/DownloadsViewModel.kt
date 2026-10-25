package app.magnetar.ui.downloads

import android.app.Application
import app.magnetar.download.DownloadService
import app.magnetar.ffi.FfiDownload
import app.magnetar.ffiCatching
import app.magnetar.ui.common.BaseViewModel
import app.magnetar.userMessage
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update

data class DownloadsUiState(
    val onServer: Boolean = false,
    val items: List<FfiDownload> = emptyList(),
    val loaded: Boolean = false,
    val error: String? = null,
)

class DownloadsViewModel(application: Application) : BaseViewModel(application) {
    private val _state = MutableStateFlow(DownloadsUiState())
    val state: StateFlow<DownloadsUiState> = _state.asStateFlow()

    val hasServer: Boolean get() = magApp.settings.value.hasServer

    fun selectTab(onServer: Boolean) {
        if (onServer == _state.value.onServer) return
        _state.value = DownloadsUiState(onServer = onServer)
    }

    /** One poll; the screen calls this every 2 s while it is visible. */
    suspend fun refresh() {
        val onServer = _state.value.onServer
        ffiCatching { magApp.magnetar().downloads(onServer) }
            .onSuccess { list ->
                _state.update { if (it.onServer == onServer) it.copy(items = list, loaded = true, error = null) else it }
            }
            .onFailure { e ->
                _state.update { if (it.onServer == onServer) it.copy(loaded = true, error = e.userMessage()) else it }
            }
    }

    fun action(d: FfiDownload, action: String) {
        val onServer = _state.value.onServer
        launchFfi { m ->
            m.downloadAction(d.id, action, onServer)
            // Resuming a phone torrent needs the service to keep the process alive.
            if (!onServer && action == "resume") DownloadService.start(magApp)
            refresh()
        }
    }
}

package app.magnetar.ui.common

import android.app.Application
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import app.magnetar.MagnetarApp
import app.magnetar.ffi.Magnetar
import app.magnetar.ffiCatching
import app.magnetar.userMessage
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.receiveAsFlow
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

/** Snackbar plumbing and ffi error handling shared by every screen. */
abstract class BaseViewModel(application: Application) : AndroidViewModel(application) {
    protected val magApp: MagnetarApp get() = getApplication()

    private val _messages = Channel<String>(Channel.BUFFERED)
    val messages: Flow<String> = _messages.receiveAsFlow()

    protected fun message(text: String) {
        _messages.trySend(text)
    }

    /** Runs [block] with the shared instance; any failure becomes a snackbar (or [onError]). */
    protected fun launchFfi(
        onError: (String) -> Unit = ::message,
        block: suspend (Magnetar) -> Unit,
    ): Job = viewModelScope.launch {
        ffiCatching { block(magApp.magnetar()) }.onFailure { onError(it.userMessage()) }
    }

    /** For the blocking (non-suspend) ffi calls. */
    protected suspend fun <T> io(block: () -> T): T = withContext(Dispatchers.IO) { block() }
}

package app.magnetar.ui.settings

import android.app.Application
import androidx.lifecycle.viewModelScope
import app.magnetar.AppSettings
import app.magnetar.BuildConfigInfo
import app.magnetar.ffi.pubkeyDecode
import app.magnetar.ffi.version
import app.magnetar.ffiCatching
import app.magnetar.ui.common.BaseViewModel
import app.magnetar.userMessage
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch

data class SettingsForm(
    val serverUrl: String,
    val apiToken: String,
    val rootNpub: String,
    val trustThreshold: Int,
    val cacheCapMb: String,
    val seedRelays: String,
    val backgroundSync: Boolean,
) {
    companion object {
        fun from(s: AppSettings) = SettingsForm(
            serverUrl = s.serverUrl,
            apiToken = s.apiToken,
            rootNpub = s.rootNpub,
            trustThreshold = s.trustThreshold,
            cacheCapMb = s.cacheCapMb.toString(),
            seedRelays = s.seedRelays.joinToString("\n"),
            backgroundSync = s.backgroundSync,
        )
    }
}

data class SettingsUiState(
    val form: SettingsForm,
    val serverUrlError: String? = null,
    val npubError: String? = null,
    val cacheError: String? = null,
    val relaysError: String? = null,
    val serverWarning: String? = null,
    val testing: Boolean = false,
    val testResult: String? = null,
    val dirty: Boolean = false,
    val coreVersion: String? = null,
)

/** Pure checks; npub decoding goes through the ffi in [SettingsViewModel.save]. */
fun validateServerUrl(url: String): Pair<String?, String?> {
    val u = url.trim()
    if (u.isEmpty()) return null to null
    val parsed = try {
        java.net.URI(u)
    } catch (_: java.net.URISyntaxException) {
        return "Not a valid URL" to null
    }
    val scheme = parsed.scheme?.lowercase()
    if (scheme != "http" && scheme != "https" || parsed.host.isNullOrEmpty()) {
        return "Must be an http:// or https:// URL" to null
    }
    val warning = if (scheme == "http" && parsed.host.lowercase() !in setOf("127.0.0.1", "localhost")) {
        "Plain HTTP is only allowed to 127.0.0.1/localhost. Use https (e.g. tailscale cert)."
    } else {
        null
    }
    return null to warning
}

fun invalidRelays(text: String): List<String> = text.lines().map { it.trim() }.filter { it.isNotEmpty() }
    .filterNot { it.startsWith("wss://") || it.startsWith("ws://") }

class SettingsViewModel(application: Application) : BaseViewModel(application) {
    private val _state = MutableStateFlow(SettingsUiState(SettingsForm.from(magApp.settings.value)))
    val state: StateFlow<SettingsUiState> = _state.asStateFlow()

    val appVersion = BuildConfigInfo.versionName(application)

    init {
        viewModelScope.launch {
            val v = ffiCatching { io { version() } }.getOrElse { "unavailable (${it.userMessage()})" }
            _state.update { it.copy(coreVersion = v) }
        }
    }

    fun edit(f: (SettingsForm) -> SettingsForm) {
        _state.update {
            val form = f(it.form)
            it.copy(form = form, dirty = true, serverWarning = validateServerUrl(form.serverUrl).second)
        }
    }

    /** Validates, persists and pushes the config. Returns false when a field is invalid. */
    suspend fun saveNow(): Boolean {
        val form = _state.value.form
        val (urlError, urlWarning) = validateServerUrl(form.serverUrl)
        val cache = form.cacheCapMb.trim().toIntOrNull()
        val cacheError = if (cache == null || cache < 16) "At least 16 MiB" else null
        val badRelays = invalidRelays(form.seedRelays)
        val relaysError = if (badRelays.isNotEmpty()) "Not a ws:// or wss:// URL: ${badRelays.first()}" else null
        val npubError = form.rootNpub.trim().takeIf { it.isNotEmpty() }?.let { npub ->
            ffiCatching { io { pubkeyDecode(npub) } }.exceptionOrNull()?.let { "Invalid key: ${it.userMessage()}" }
        }
        _state.update {
            it.copy(
                serverUrlError = urlError, serverWarning = urlWarning, cacheError = cacheError,
                relaysError = relaysError, npubError = npubError,
            )
        }
        if (urlError != null || cacheError != null || relaysError != null || npubError != null) return false
        val settings = AppSettings(
            serverUrl = form.serverUrl.trim(),
            apiToken = form.apiToken.trim(),
            rootNpub = form.rootNpub.trim(),
            trustThreshold = form.trustThreshold,
            cacheCapMb = cache!!,
            seedRelays = form.seedRelays.lines().map { it.trim() }.filter { it.isNotEmpty() },
            backgroundSync = form.backgroundSync,
        )
        return ffiCatching { magApp.updateSettings(settings) }
            .onSuccess { _state.update { it.copy(dirty = false) } }
            .onFailure { message("Could not apply settings: ${it.userMessage()}") }
            .isSuccess
    }

    fun save() {
        viewModelScope.launch { if (saveNow()) message("Settings saved") }
    }

    fun testConnection() {
        viewModelScope.launch {
            if (!saveNow()) return@launch
            if (!magApp.settings.value.hasServer) {
                _state.update { it.copy(testResult = "No server URL set (offline mode)") }
                return@launch
            }
            _state.update { it.copy(testing = true, testResult = null) }
            val result = ffiCatching { magApp.magnetar().serverHealth() }
                .fold({ ok -> if (ok) "Server reachable" else "Server answered but is not healthy" }, { it.userMessage() })
            _state.update { it.copy(testing = false, testResult = result) }
        }
    }
}

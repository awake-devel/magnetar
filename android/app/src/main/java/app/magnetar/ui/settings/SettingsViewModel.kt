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

const val PRIVATE_KEY_ERROR =
    "That is your private key (nsec): never paste it into an app. Use your public key (npub1…). The field was cleared."

/** True for a Nostr private key (bech32 nsec or ncryptsec), which the trust root must never hold. */
fun isPrivateKey(key: String): Boolean = key.trim().lowercase().let { it.startsWith("nsec1") || it.startsWith("ncryptsec1") }

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
    val warning = when {
        scheme != "http" || isPrivateTransport(parsed.host) -> null
        isTailscaleCgnat(parsed.host) ->
            "Plain HTTP to a 100.x address: encrypted only while Tailscale is connected on this " +
                "phone. Without it, 100.64.0.0/10 is carrier-grade NAT space and the API token " +
                "travels readable. Prefer the *.ts.net name or https://."
        else ->
            "Plain HTTP: the API token and results can be read by anyone on this network. " +
                "Prefer https:// (tailscale serve or a reverse proxy)."
    }
    return null to warning
}

/**
 * Hosts where plain http:// is not exposed on the wire (D39): loopback, and Tailscale names and
 * addresses that cannot be reached any other way: MagicDNS `*.ts.net` (does not resolve without
 * the tunnel) and the Tailscale IPv6 range fd7a:115c:a1e0::/48 (not routed outside it). Tailscale
 * IPv4 addresses (100.64.0.0/10) are not here: that range is also carrier-grade NAT (RFC 6598),
 * routed in clear by mobile and ISP networks when the tunnel is down; see [isTailscaleCgnat].
 * A short MagicDNS name (`http://nas:8787`) cannot be told apart from a LAN name and still warns.
 */
fun isPrivateTransport(host: String): Boolean {
    val h = host.lowercase().removePrefix("[").removeSuffix("]")
    if (h == "localhost" || h == "::1" || h.endsWith(".ts.net")) return true
    val v4 = ipv4(h)
    if (v4 != null) return v4[0] == 127
    return h.startsWith("fd7a:115c:a1e0:")
}

/** 100.64.0.0/10: a Tailscale IPv4 address, or carrier-grade NAT when the tunnel is down. */
fun isTailscaleCgnat(host: String): Boolean {
    val v4 = ipv4(host) ?: return false
    return v4[0] == 100 && v4[1] in 64..127
}

private fun ipv4(host: String): List<Int>? {
    val parts = host.split('.').takeIf { it.size == 4 }?.map { it.toIntOrNull() ?: return null } ?: return null
    return parts.takeIf { p -> p.all { it in 0..255 } }
}

fun invalidRelays(text: String): List<String> = text.lines().map { it.trim() }.filter { it.isNotEmpty() }
    .filterNot { it.startsWith("wss://") || it.startsWith("ws://") }

class SettingsViewModel(application: Application) : BaseViewModel(application) {
    private val _state = MutableStateFlow(
        SettingsForm.from(magApp.settings.value).let { SettingsUiState(it, serverWarning = validateServerUrl(it.serverUrl).second) },
    )
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
            if (isPrivateKey(form.rootNpub)) {
                // Cleared as soon as it is pasted, before anything could store it.
                return@update it.copy(form = form.copy(rootNpub = ""), npubError = PRIVATE_KEY_ERROR)
            }
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
        // A private key is never kept, not even in the form: clear it and say what to paste instead.
        if (isPrivateKey(form.rootNpub)) {
            _state.update { it.copy(form = it.form.copy(rootNpub = ""), npubError = PRIVATE_KEY_ERROR) }
            return false
        }
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
        val pushed = ffiCatching { magApp.updateSettings(settings) }
            .onSuccess { _state.update { it.copy(dirty = false) } }
            .onFailure { message("Could not apply settings: ${it.userMessage()}") }
            .isSuccess
        // With a server, trust is computed there: give it the same root and threshold.
        if (pushed && settings.hasServer) {
            ffiCatching {
                magApp.magnetar().pushTrustSettings(settings.rootNpub.ifEmpty { null }, settings.trustThreshold.toUByte())
            }.onFailure { message("Saved, but the server did not take the trust root: ${it.userMessage()}") }
        }
        return pushed
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

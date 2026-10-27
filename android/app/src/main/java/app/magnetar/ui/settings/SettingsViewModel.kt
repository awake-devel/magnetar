package app.magnetar.ui.settings

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import app.magnetar.api.ApiException
import app.magnetar.api.Features
import app.magnetar.api.MagnetarService
import app.magnetar.api.TrustSettings
import app.magnetar.data.Repository
import app.magnetar.settings.AppSettings
import app.magnetar.settings.Check
import app.magnetar.settings.Nip19
import app.magnetar.settings.PRIVATE_KEY_ERROR
import app.magnetar.settings.checkRootKey
import app.magnetar.settings.checkServerUrl
import app.magnetar.settings.checkToken
import app.magnetar.settings.isPrivateKey
import app.magnetar.settings.normalizeServerUrl
import app.magnetar.ui.common.userMessage
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.receiveAsFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch

/** The text fields of Settings; saved together with *Save* (the switches apply at once). */
data class SettingsForm(
    val serverUrl: String = "",
    val apiToken: String = "",
    val rootKey: String = "",
    val threshold: Int = AppSettings.DEFAULT_THRESHOLD,
) {
    companion object {
        fun of(s: AppSettings) = SettingsForm(s.serverUrl, s.apiToken, s.rootNpub, s.trustThreshold)
    }
}

data class SettingsUiState(
    val form: SettingsForm = SettingsForm(),
    /** What is stored; [dirty] compares the form to it. */
    val saved: SettingsForm = SettingsForm(),
    val url: Check = Check(),
    val token: Check = Check(),
    val root: Check = Check(),
    val saving: Boolean = false,
    val testing: Boolean = false,
    val testResult: String? = null,
    val testOk: Boolean? = null,
    /** The server's trust settings as last read (root as hex), null until read. */
    val serverTrust: TrustSettings? = null,
    val serverTrustError: String? = null,
) {
    val dirty: Boolean get() = form != saved
    val canSave: Boolean get() = dirty && !saving && url.error == null && token.error == null && root.error == null
}

/**
 * Settings (docs/APP.md §1): server URL + token + *Test connection*; trust root (Amber NIP-55 or
 * pasted, an nsec is cleared at once) and threshold, pushed to the server with
 * `PUT /api/v1/settings/trust`; the adult and notification switches.
 *
 * @param update writes the settings ([app.magnetar.settings.SettingsStore.update]).
 * @param serviceFor a client for given settings (the unsaved form's URL and token for the test).
 */
class SettingsViewModel(
    private val repo: Repository,
    private val update: ((AppSettings) -> AppSettings) -> Unit,
    private val serviceFor: (AppSettings) -> MagnetarService,
) : ViewModel() {
    private val _state = MutableStateFlow(
        SettingsForm.of(repo.settings.value).let { SettingsUiState(form = it, saved = it, url = checkServerUrl(it.serverUrl)) },
    )
    val state: StateFlow<SettingsUiState> = _state.asStateFlow()

    private val _messages = Channel<String>(Channel.BUFFERED)
    val messages = _messages.receiveAsFlow()

    /** The root field was changed here: only then may an empty root clear the server's. */
    private var rootEdited = false

    init {
        loadServerTrust()
    }

    /** Reads the server's trust settings; fills an empty root field with the server's root. */
    fun loadServerTrust() {
        val s = repo.settings.value
        if (!s.hasServer) return
        viewModelScope.launch {
            try {
                val t = serviceFor(s).trustSettings()
                _state.update { st ->
                    val fill = st.form.rootKey.isBlank() && !rootEdited && t.root != null
                    val root = if (fill && t.root != null) Nip19.npub(t.root) else st.form.rootKey
                    st.copy(
                        serverTrust = t,
                        serverTrustError = null,
                        form = st.form.copy(rootKey = root),
                        saved = if (fill) st.saved.copy(rootKey = root) else st.saved,
                    )
                }
            } catch (e: CancellationException) {
                throw e
            } catch (e: ApiException) {
                _state.update { it.copy(serverTrustError = e.userMessage()) }
            }
        }
    }

    fun setServerUrl(v: String) = _state.update { it.copy(form = it.form.copy(serverUrl = v), url = checkServerUrl(v), testResult = null) }

    fun setToken(v: String) = _state.update { it.copy(form = it.form.copy(apiToken = v), token = checkToken(v), testResult = null) }

    /** A pasted nsec never stays in the field, not even unsaved. */
    fun setRootKey(v: String) {
        rootEdited = true
        if (isPrivateKey(v)) {
            _state.update { it.copy(form = it.form.copy(rootKey = ""), root = Check(error = PRIVATE_KEY_ERROR)) }
            return
        }
        _state.update { it.copy(form = it.form.copy(rootKey = v), root = checkRootKey(v)) }
    }

    /** The answer of a NIP-55 signer (`get_public_key`): hex or npub, shown as npub. */
    fun onSignerKey(key: String?) {
        val k = key?.trim().orEmpty()
        if (k.isEmpty()) return
        val hex = Nip19.pubkeyHex(k)
        if (hex == null) {
            _messages.trySend("The signer answered something that is not a public key")
            return
        }
        setRootKey(Nip19.npub(hex))
    }

    fun setThreshold(v: Int) = _state.update { it.copy(form = it.form.copy(threshold = v.coerceIn(0, 100))) }

    fun setShowAdult(on: Boolean) = update { it.copy(showAdult = on) }

    fun setNotify(on: Boolean) = update { it.copy(notifySavedSearches = on) }

    /** Validates, stores, and pushes the trust root and threshold to the server. */
    fun save() {
        viewModelScope.launch { saveNow() }
    }

    internal suspend fun saveNow(): Boolean {
        val f = _state.value.form
        val url = checkServerUrl(f.serverUrl)
        val token = checkToken(f.apiToken)
        val root = checkRootKey(f.rootKey)
        _state.update { it.copy(url = url, token = token, root = root) }
        if (url.error != null || token.error != null || root.error != null) return false
        _state.update { it.copy(saving = true) }
        val previous = repo.settings.value
        update {
            it.copy(
                serverUrl = normalizeServerUrl(f.serverUrl),
                apiToken = f.apiToken.trim(),
                rootNpub = f.rootKey.trim(),
                trustThreshold = f.threshold,
            )
        }
        val now = repo.settings.value
        val stored = SettingsForm.of(now)
        _state.update { it.copy(form = stored, saved = stored) }
        if (now.serverUrl != previous.serverUrl || now.apiToken != previous.apiToken) {
            _state.update { it.copy(serverTrust = null, serverTrustError = null) }
            repo.refreshHealth(force = true)
        }
        val pushed = if (now.hasServer) pushTrust(now) else null
        _state.update { it.copy(saving = false) }
        _messages.trySend(
            when (pushed) {
                null -> "Saved"
                true -> "Saved · trust settings sent to the server"
                false -> "Saved on the phone · the server did not take the trust settings: ${_state.value.serverTrustError}"
            },
        )
        rootEdited = false
        return true
    }

    /**
     * `PUT /api/v1/settings/trust` when the server's root or threshold differ from ours. An
     * empty root field clears the server's root only when it was emptied here (a phone that
     * never had one keeps the server's). True = in sync, false = failed.
     */
    private suspend fun pushTrust(s: AppSettings): Boolean {
        val service = try {
            serviceFor(s)
        } catch (e: ApiException) {
            _state.update { it.copy(serverTrustError = e.userMessage()) }
            return false
        }
        return try {
            val current = service.trustSettings()
            val hex = Nip19.pubkeyHex(s.rootNpub)
            val root = if (hex == null && !rootEdited) current.root else hex
            val desired = TrustSettings(root = root, threshold = s.trustThreshold)
            val result = if (current.root?.lowercase() != root || current.threshold != desired.threshold) {
                service.putTrustSettings(desired)
            } else {
                current
            }
            _state.update { it.copy(serverTrust = result, serverTrustError = null) }
            true
        } catch (e: CancellationException) {
            throw e
        } catch (e: ApiException) {
            _state.update { it.copy(serverTrustError = e.userMessage()) }
            false
        }
    }

    /** Checks the URL and token in the form (saved or not): `/health`, then an authenticated read. */
    fun testConnection() {
        val f = _state.value.form
        val url = checkServerUrl(f.serverUrl)
        if (f.serverUrl.isBlank() || url.error != null) {
            _state.update { it.copy(url = url, testResult = url.error ?: "Enter the server URL first", testOk = false) }
            return
        }
        val token = checkToken(f.apiToken)
        if (token.error != null) {
            _state.update { it.copy(token = token, testResult = token.error, testOk = false) }
            return
        }
        _state.update { it.copy(testing = true, testResult = null, testOk = null) }
        viewModelScope.launch {
            val (ok, text) = runTest(AppSettings(serverUrl = normalizeServerUrl(f.serverUrl), apiToken = f.apiToken.trim()))
            _state.update { it.copy(testing = false, testResult = text, testOk = ok) }
        }
    }

    private suspend fun runTest(s: AppSettings): Pair<Boolean, String> {
        val service = try {
            serviceFor(s)
        } catch (e: ApiException) {
            return false to e.userMessage()
        }
        val health = try {
            service.health()
        } catch (e: CancellationException) {
            throw e
        } catch (e: ApiException) {
            return false to e.userMessage()
        }
        return try {
            service.trustSettings()
            val missing = listOfNotNull(
                "saved-search alerts".takeUnless { health.has(Features.SAVED_SEARCH_POLL) },
                "charts".takeUnless { health.has(Features.STATS) },
                "graphs".takeUnless { health.has(Features.GRAPHS) },
            )
            true to buildString {
                append("Connected: Magnetar ").append(health.version.ifBlank { "?" }).append(", token accepted")
                if (missing.isNotEmpty()) append(". This server is older than the app: no ").append(missing.joinToString(", "))
            }
        } catch (e: CancellationException) {
            throw e
        } catch (e: ApiException.Unauthorized) {
            false to "The server answers (Magnetar ${health.version}), but it rejected the token"
        } catch (e: ApiException) {
            false to "The server answers (Magnetar ${health.version}), but: ${e.userMessage()}"
        }
    }
}

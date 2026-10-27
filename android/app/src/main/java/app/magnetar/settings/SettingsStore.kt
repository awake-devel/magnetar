package app.magnetar.settings

import android.content.Context
import android.content.SharedPreferences
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow

/**
 * The settings, in the old app's SharedPreferences file `settings` with its keys, so the
 * server URL, token, trust root and switches survive the upgrade (docs/APP.md §7, D81).
 * [migrate] runs in the constructor; it is idempotent.
 */
class SettingsStore(private val prefs: SharedPreferences) {
    constructor(context: Context) : this(context.getSharedPreferences(FILE, Context.MODE_PRIVATE))

    init {
        migrate(prefs)
    }

    private val state = MutableStateFlow(load(prefs))
    val settings: StateFlow<AppSettings> = state.asStateFlow()

    fun update(transform: (AppSettings) -> AppSettings) {
        val new = sanitize(transform(state.value))
        prefs.edit()
            .putString(K_SERVER, new.serverUrl)
            .putString(K_TOKEN, new.apiToken)
            .putString(K_ROOT, new.rootNpub)
            .putInt(K_THRESHOLD, new.trustThreshold)
            .putBoolean(K_ADULT, new.showAdult)
            .putBoolean(K_NOTIFY, new.notifySavedSearches)
            .apply()
        state.value = new
    }

    companion object {
        const val FILE = "settings"
        const val VERSION = 1

        // Keys of the old app (Settings.kt before the rebuild), kept as they were.
        internal const val K_SERVER = "server_url"
        internal const val K_TOKEN = "api_token"
        internal const val K_ROOT = "root_npub"
        internal const val K_THRESHOLD = "trust_threshold"
        internal const val K_ADULT = "show_adult"

        // New in version 1.
        internal const val K_VERSION = "settings_version"
        internal const val K_NOTIFY = "notify_saved_searches"

        // Old keys with no meaning any more (phone cache, phone crawler, background crawl).
        internal const val K_OLD_CACHE = "cache_cap_mb"
        internal const val K_OLD_RELAYS = "seed_relays"
        internal const val K_OLD_BACKGROUND = "background_sync"

        /**
         * Version 0 (the old app, or a fresh install) → 1: `background_sync` (crawl + saved-search
         * poll) becomes `notify_saved_searches`; the phone-cache and relay keys are removed; a
         * private key in the root field is dropped; the URL is normalised. A fresh install gets
         * the defaults (adult content hidden).
         */
        fun migrate(prefs: SharedPreferences) {
            if (prefs.getInt(K_VERSION, 0) >= VERSION) return
            val edit = prefs.edit()
            if (prefs.contains(K_OLD_BACKGROUND)) {
                edit.putBoolean(K_NOTIFY, prefs.getBoolean(K_OLD_BACKGROUND, true))
            }
            prefs.getString(K_SERVER, null)?.let { edit.putString(K_SERVER, normalizeServerUrl(it)) }
            prefs.getString(K_TOKEN, null)?.let { edit.putString(K_TOKEN, it.trim()) }
            prefs.getString(K_ROOT, null)?.let {
                edit.putString(K_ROOT, if (isPrivateKey(it)) "" else it.trim())
            }
            if (prefs.contains(K_THRESHOLD)) {
                edit.putInt(K_THRESHOLD, prefs.getInt(K_THRESHOLD, AppSettings.DEFAULT_THRESHOLD).coerceIn(0, 100))
            }
            edit.remove(K_OLD_BACKGROUND).remove(K_OLD_CACHE).remove(K_OLD_RELAYS)
            edit.putInt(K_VERSION, VERSION)
            // apply(): visible to the reads below at once, written to disk off the main thread
            // (the store is built in Application.onCreate). Lost on a crash before the write,
            // the migration simply runs again: it is idempotent.
            edit.apply()
        }

        fun load(prefs: SharedPreferences): AppSettings {
            val d = AppSettings()
            return sanitize(
                AppSettings(
                    serverUrl = prefs.getString(K_SERVER, null) ?: d.serverUrl,
                    apiToken = prefs.getString(K_TOKEN, null) ?: d.apiToken,
                    rootNpub = prefs.getString(K_ROOT, null) ?: d.rootNpub,
                    trustThreshold = prefs.getInt(K_THRESHOLD, d.trustThreshold),
                    showAdult = prefs.getBoolean(K_ADULT, d.showAdult),
                    notifySavedSearches = prefs.getBoolean(K_NOTIFY, d.notifySavedSearches),
                ),
            )
        }

        private fun sanitize(s: AppSettings) = s.copy(
            serverUrl = normalizeServerUrl(s.serverUrl),
            apiToken = s.apiToken.trim(),
            rootNpub = s.rootNpub.trim().takeUnless(::isPrivateKey) ?: "",
            trustThreshold = s.trustThreshold.coerceIn(0, 100),
        )
    }
}

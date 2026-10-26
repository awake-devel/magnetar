package app.magnetar

import android.content.Context
import androidx.core.content.edit
import app.magnetar.ffi.FfiConfig

data class AppSettings(
    val serverUrl: String = DEFAULT_SERVER_URL,
    val apiToken: String = "",
    val rootNpub: String = "",
    val trustThreshold: Int = 20,
    val cacheCapMb: Int = 512,
    /** One relay URL per entry; empty = core defaults. */
    val seedRelays: List<String> = emptyList(),
    val backgroundSync: Boolean = true,
    /** Settings → "Show adult content" (D52): off = adult torrents hidden everywhere. */
    val showAdult: Boolean = false,
) {
    val hasServer: Boolean get() = serverUrl.isNotBlank()

    fun toFfiConfig(dirs: AppDirs) = FfiConfig(
        serverUrl = serverUrl.trim().ifEmpty { null },
        apiToken = apiToken.trim().ifEmpty { null },
        rootPubkey = rootNpub.trim().ifEmpty { null },
        trustThreshold = trustThreshold.coerceIn(0, 100).toUByte(),
        cacheCapMb = cacheCapMb.coerceAtLeast(1).toUInt(),
        seedRelays = seedRelays.map { it.trim() }.filter { it.isNotEmpty() },
        downloadDir = dirs.download.absolutePath,
        stateDir = dirs.state.absolutePath,
        showAdult = showAdult,
    )

    companion object {
        /** No server by default: the app crawls and searches offline until one is set (D39). */
        const val DEFAULT_SERVER_URL = ""
    }
}

class SettingsStore(context: Context) {
    private val prefs = context.getSharedPreferences("settings", Context.MODE_PRIVATE)

    fun load(): AppSettings {
        val d = AppSettings()
        return AppSettings(
            serverUrl = prefs.getString(K_SERVER, d.serverUrl) ?: d.serverUrl,
            apiToken = prefs.getString(K_TOKEN, d.apiToken) ?: d.apiToken,
            rootNpub = prefs.getString(K_ROOT, d.rootNpub) ?: d.rootNpub,
            trustThreshold = prefs.getInt(K_THRESHOLD, d.trustThreshold),
            cacheCapMb = prefs.getInt(K_CACHE, d.cacheCapMb),
            seedRelays = prefs.getString(K_RELAYS, null)
                ?.lines()?.map { it.trim() }?.filter { it.isNotEmpty() }
                ?: d.seedRelays,
            backgroundSync = prefs.getBoolean(K_BACKGROUND, d.backgroundSync),
            showAdult = prefs.getBoolean(K_ADULT, d.showAdult),
        )
    }

    fun save(s: AppSettings) = prefs.edit {
        putString(K_SERVER, s.serverUrl.trim())
        putString(K_TOKEN, s.apiToken.trim())
        putString(K_ROOT, s.rootNpub.trim())
        putInt(K_THRESHOLD, s.trustThreshold)
        putInt(K_CACHE, s.cacheCapMb)
        putString(K_RELAYS, s.seedRelays.joinToString("\n"))
        putBoolean(K_BACKGROUND, s.backgroundSync)
        putBoolean(K_ADULT, s.showAdult)
    }

    private companion object {
        const val K_SERVER = "server_url"
        const val K_TOKEN = "api_token"
        const val K_ROOT = "root_npub"
        const val K_THRESHOLD = "trust_threshold"
        const val K_CACHE = "cache_cap_mb"
        const val K_RELAYS = "seed_relays"
        const val K_BACKGROUND = "background_sync"
        const val K_ADULT = "show_adult"
    }
}

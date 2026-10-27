package app.magnetar

import android.app.Application
import androidx.work.WorkManager
import app.magnetar.api.ApiException
import app.magnetar.api.Http
import app.magnetar.api.MagnetarApi
import app.magnetar.settings.SettingsStore
import okhttp3.OkHttpClient

/** Process-wide pieces: the settings, the HTTP client and the API client built from them. */
class MagnetarApp : Application() {
    val settingsStore: SettingsStore by lazy { SettingsStore(this) }

    /** One connection pool and dispatcher for the whole app (docs/APP.md §4). */
    val http: OkHttpClient by lazy { Http.baseClient() }

    @Volatile private var cached: Pair<Pair<String, String>, MagnetarApi>? = null

    /**
     * The client for the server in Settings, rebuilt when the URL or token changes.
     * Throws [ApiException.NotConfigured] while no (valid) server is set.
     */
    fun api(): MagnetarApi {
        val s = settingsStore.settings.value
        val key = s.serverUrl to s.apiToken
        cached?.let { (k, api) -> if (k == key) return api }
        return MagnetarApi.create(s.serverUrl, s.apiToken, http).also { cached = key to it }
    }

    override fun onCreate() {
        super.onCreate()
        settingsStore // runs the settings migration before any screen reads them
        Notifications.createChannels(this)
        removeLegacyWork()
    }

    /** The old app's background crawl (its worker class no longer exists, docs/APP.md §7). */
    private fun removeLegacyWork() {
        runCatching {
            val wm = WorkManager.getInstance(this)
            LEGACY_WORK.forEach(wm::cancelUniqueWork)
        }
    }

    private companion object {
        val LEGACY_WORK = listOf("crawl-sync", "crawl-sync-now")
    }
}

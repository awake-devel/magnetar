package app.magnetar

import android.app.Application
import android.graphics.BitmapFactory
import android.util.Log
import android.util.LruCache
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.asImageBitmap
import androidx.work.WorkManager
import app.magnetar.api.ApiException
import app.magnetar.api.Http
import app.magnetar.api.TimingListener
import app.magnetar.api.MagnetarApi
import app.magnetar.api.awaitBytes
import app.magnetar.cache.PageCache
import app.magnetar.data.Repository
import app.magnetar.settings.SettingsStore
import app.magnetar.work.LegacyImport
import app.magnetar.work.SavedSearchPolling
import java.io.File
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import okhttp3.Cache
import okhttp3.OkHttpClient
import okhttp3.Request

/** Process-wide pieces: the settings, the HTTP client, the API client, the repository. */
class MagnetarApp : Application() {
    val settingsStore: SettingsStore by lazy { SettingsStore(this) }

    /** One connection pool and dispatcher for the whole app (docs/APP.md §4). */
    val http: OkHttpClient by lazy {
        Http.baseClient().let {
            if (BuildConfig.DEBUG) it.newBuilder().eventListenerFactory { TimingListener() }.build() else it
        }
    }

    /** Lives as long as the process: settings watchers, the poll scheduling. */
    val appScope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)

    val repository: Repository by lazy {
        Repository(
            settings = settingsStore.settings,
            serviceFor = { api() },
            cache = PageCache(File(cacheDir, "pages")),
            scope = appScope,
        )
    }

    @Volatile private var cached: Pair<Pair<String, String>, MagnetarApi>? = null

    /**
     * The client for the server in Settings, rebuilt when the URL or token changes.
     * Throws [ApiException.NotConfigured] while no (valid) server is set.
     */
    fun api(): MagnetarApi {
        val s = settingsStore.settings.value
        val key = s.serverUrl to s.apiToken
        cached?.let { (k, api) -> if (k == key) return api }
        return MagnetarApi.create(s.serverUrl, s.apiToken, http).also {
            cached = key to it
            posterClient = null
        }
    }

    // --- posters (D85): the app's OkHttp client with an HTTP disk cache, no image library ---

    /** Honours the server's `Cache-Control: private, max-age=604800` and `ETag` (docs/API.md). */
    private val posterDiskCache by lazy { Cache(File(cacheDir, "posters"), 30L * 1024 * 1024) }

    @Volatile private var posterClient: Pair<MagnetarApi, OkHttpClient>? = null

    /** Decoded posters by server + path, ~16 MB. */
    private val posterMemory = object : LruCache<String, ImageBitmap>(16 * 1024 * 1024) {
        override fun sizeOf(key: String, value: ImageBitmap) = value.width * value.height * 4
    }

    /** Loads [path] (server-relative, from `MediaInfo.poster`) from the configured server; null on any failure. */
    suspend fun loadPoster(path: String): ImageBitmap? {
        val api = try {
            api()
        } catch (_: ApiException) {
            return null
        }
        val url = api.posterUrl(path) ?: return null
        val memKey = url.toString()
        posterMemory.get(memKey)?.let { return it }
        val client = posterClient?.takeIf { it.first === api }?.second
            ?: api.httpClient.newBuilder().cache(posterDiskCache).build().also { posterClient = api to it }
        return try {
            val bytes = client.newCall(Request.Builder().url(url).get().build()).awaitBytes() ?: return null
            withContext(Dispatchers.Default) { decodePoster(bytes) }?.also { posterMemory.put(memKey, it) }
        } catch (e: CancellationException) {
            throw e
        } catch (_: Exception) {
            null
        }
    }

    /** Down-sampled to at most ~600 px high: a card shows it at ~160 dp. */
    private fun decodePoster(bytes: ByteArray): ImageBitmap? {
        val bounds = BitmapFactory.Options().apply { inJustDecodeBounds = true }
        BitmapFactory.decodeByteArray(bytes, 0, bytes.size, bounds)
        if (bounds.outHeight <= 0) return null
        var sample = 1
        while (bounds.outHeight / (sample * 2) >= 600) sample *= 2
        val opts = BitmapFactory.Options().apply { inSampleSize = sample }
        return BitmapFactory.decodeByteArray(bytes, 0, bytes.size, opts)?.asImageBitmap()
    }

    /** Time-to-first-results, for the emulator measurement (no query text, no titles). */
    fun logTiming(what: String) {
        Log.i(TIMING_TAG, what)
    }

    override fun onCreate() {
        super.onCreate()
        settingsStore // runs the settings migration before any screen reads them
        Notifications.createChannels(this)
        removeLegacyWork()
        SavedSearchPolling.watch(this)
        importLegacySearches()
    }

    /**
     * The old app's phone-only saved searches go to the server once a server is set and
     * reachable (docs/APP.md §7); the old files are deleted only after all of them are there.
     */
    private fun importLegacySearches() {
        val db = getDatabasePath(LEGACY_DB)
        if (!db.exists()) return
        appScope.launch {
            settingsStore.settings.map { it.serverUrl to it.apiToken }.distinctUntilChanged().collect { (server, _) ->
                if (server.isBlank() || !db.exists()) return@collect
                val rows = withContext(Dispatchers.IO) { LegacyImport.read(db) } ?: return@collect
                val service = try {
                    api()
                } catch (_: ApiException) {
                    return@collect
                }
                if (LegacyImport.upload(service, rows)) {
                    withContext(Dispatchers.IO) { LegacyImport.deleteOldFiles(db, File(filesDir, "rqbit")) }
                    if (rows.isNotEmpty()) repository.savedSearchesChanged.value++
                }
            }
        }
    }

    /** The old app's background crawl (its worker class no longer exists, docs/APP.md §7). */
    private fun removeLegacyWork() {
        runCatching {
            val wm = WorkManager.getInstance(this)
            LEGACY_WORK.forEach(wm::cancelUniqueWork)
        }
    }

    companion object {
        const val TIMING_TAG = "MagnetarTiming"
        private val LEGACY_WORK = listOf("crawl-sync", "crawl-sync-now")
        private const val LEGACY_DB = "magnetar.db"
    }
}

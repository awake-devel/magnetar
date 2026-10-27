package app.magnetar.data

import app.magnetar.api.ApiException
import app.magnetar.api.Health
import app.magnetar.api.MagnetarApi
import app.magnetar.api.MagnetarService
import app.magnetar.api.SearchQuery
import app.magnetar.cache.PageCache
import app.magnetar.settings.AppSettings
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineDispatcher
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.drop
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext
import kotlinx.serialization.KSerializer

/** A cached answer and when it was saved. */
class Cached<T>(val value: T, val savedAtMs: Long)

/**
 * What every screen shares: the settings, the service for the configured server, its
 * `/health` features, the page cache, and a search handed over by another tab (a saved search
 * run from the Saved tab). One per process ([app.magnetar.MagnetarApp]); tests build their own
 * with a fake service.
 */
class Repository(
    val settings: StateFlow<AppSettings>,
    private val serviceFor: (AppSettings) -> MagnetarService,
    private val cache: PageCache?,
    scope: CoroutineScope,
    private val io: CoroutineDispatcher = Dispatchers.IO,
) {
    private val _health = MutableStateFlow<Health?>(null)

    /** The server's `/health`, null until it answered (or while no server is set). */
    val health: StateFlow<Health?> = _health.asStateFlow()

    private val healthLock = Mutex()

    /** The server URL [_health] was answered by: a settings change can run a search before [init]'s watcher clears it. */
    @Volatile private var healthServer: String? = null

    /** A search to run on the Search tab (from Saved); the Search tab clears it. */
    val pendingSearch = MutableStateFlow<SearchQuery?>(null)

    /** Bumped when a saved search was created elsewhere (Search tab), so Saved reloads. */
    val savedSearchesChanged = MutableStateFlow(0)

    init {
        // Another server or token: forget its features and its cached pages (docs/APP.md §5).
        scope.launch {
            settings.map { it.serverUrl to it.apiToken }.distinctUntilChanged().drop(1).collect {
                _health.value = null
                cache?.let { c -> withContext(io) { c.clear() } }
            }
        }
    }

    /** The service for the server in Settings; [ApiException.NotConfigured] while none is set. */
    fun service(): MagnetarService = serviceFor(settings.value)

    fun has(feature: String): Boolean = _health.value?.has(feature) == true

    /**
     * `/health` once per server (again with [force]); null when the server cannot be reached,
     * so a later search can try again. Never throws.
     */
    suspend fun refreshHealth(force: Boolean = false): Health? = healthLock.withLock {
        val key = settings.value.serverUrl
        if (!force && healthServer == key) _health.value?.let { return it }
        try {
            val h = service().health()
            if (settings.value.serverUrl == key) {
                healthServer = key
                _health.value = h
            }
            h
        } catch (e: CancellationException) {
            throw e
        } catch (_: ApiException) {
            null
        }
    }

    suspend fun <T> readCache(endpoint: String, key: String, ser: KSerializer<T>): Cached<T>? {
        val c = cache ?: return null
        val server = settings.value.serverUrl
        return withContext(io) {
            val e = c.get(server, endpoint, key) ?: return@withContext null
            runCatching { Cached(MagnetarApi.JSON.decodeFromString(ser, e.json), e.savedAtMs) }.getOrNull()
        }
    }

    suspend fun <T> writeCache(endpoint: String, key: String, ser: KSerializer<T>, value: T) {
        val c = cache ?: return
        val server = settings.value.serverUrl
        withContext(io) { c.put(server, endpoint, key, MagnetarApi.JSON.encodeToString(ser, value)) }
    }

    /** Canonical cache key of a request body (encodeDefaults: the adult switch is always in it). */
    fun <T> keyOf(ser: KSerializer<T>, body: T): String = MagnetarApi.JSON.encodeToString(ser, body)
}

package app.magnetar

import android.app.Application
import android.os.Environment
import app.magnetar.ffi.FfiSearchQuery
import app.magnetar.ffi.Magnetar
import app.magnetar.work.CrawlSyncWorker
import java.io.File
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext

/** Holds the single [Magnetar] instance of the process; workers, service and UI share it. */
class MagnetarApp : Application() {
    val settingsStore by lazy { SettingsStore(this) }

    private val _settings by lazy { MutableStateFlow(settingsStore.load()) }
    val settings: StateFlow<AppSettings> get() = _settings.asStateFlow()

    /** A saved search the user asked to run; the Search screen consumes it. */
    val pendingSearch = MutableStateFlow<FfiSearchQuery?>(null)

    /** Last query run on the Search screen, offered by "Save search" on the detail screen. */
    @Volatile var lastQuery: FfiSearchQuery? = null

    private val openLock = Mutex()
    @Volatile private var instance: Magnetar? = null

    val dirs: AppDirs by lazy {
        val external = getExternalFilesDir(Environment.DIRECTORY_DOWNLOADS)
        AppDirs(
            download = File(external ?: File(filesDir, "downloads"), "magnetar"),
            state = File(filesDir, "rqbit"),
        )
    }

    override fun onCreate() {
        super.onCreate()
        Notifications.createChannels(this)
        CrawlSyncWorker.schedulePeriodic(this, settings.value.backgroundSync)
    }

    /**
     * Opens the ffi store on first use, off the main thread. A failed open is not cached, so the
     * next call retries (e.g. after the user fixes a setting).
     */
    suspend fun magnetar(): Magnetar {
        instance?.let { return it }
        return openLock.withLock {
            instance ?: withContext(Dispatchers.IO) {
                dirs.download.mkdirs()
                dirs.state.mkdirs()
                Magnetar.open(
                    getDatabasePath(DB_NAME).also { it.parentFile?.mkdirs() }.absolutePath,
                    settings.value.toFfiConfig(dirs),
                )
            }.also { instance = it }
        }
    }

    /** Persists [new] and pushes it to the open instance, if any. */
    suspend fun updateSettings(new: AppSettings) {
        val old = settings.value
        withContext(Dispatchers.IO) {
            settingsStore.save(new)
            instance?.setConfig(new.toFfiConfig(dirs))
        }
        _settings.value = new
        if (old.backgroundSync != new.backgroundSync) {
            CrawlSyncWorker.schedulePeriodic(this, new.backgroundSync)
        }
    }

    companion object {
        const val DB_NAME = "magnetar.db"
    }
}

data class AppDirs(val download: File, val state: File)

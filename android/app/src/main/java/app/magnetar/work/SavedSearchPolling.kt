package app.magnetar.work

import androidx.work.Constraints
import androidx.work.ExistingPeriodicWorkPolicy
import androidx.work.NetworkType
import androidx.work.PeriodicWorkRequestBuilder
import androidx.work.WorkManager
import app.magnetar.MagnetarApp
import app.magnetar.api.Features
import java.util.concurrent.TimeUnit
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.launch

/** Keeps the unique periodic work `saved-search-poll` enqueued exactly while it should run (D84). */
object SavedSearchPolling {
    /** Watches the switch and the server: enqueue (1 h, network connected) or cancel. */
    fun watch(app: MagnetarApp) {
        app.appScope.launch {
            app.settingsStore.settings
                .map { Triple(it.notifySavedSearches, it.serverUrl, it.apiToken) }
                .distinctUntilChanged()
                .collect { (notify, server, _) ->
                    val wm = runCatching { WorkManager.getInstance(app) }.getOrNull() ?: return@collect
                    if (!notify || server.isBlank()) {
                        wm.cancelUniqueWork(SavedSearchPollWorker.WORK_NAME)
                        return@collect
                    }
                    val health = app.repository.refreshHealth(force = true)
                    when {
                        // Unreachable now: leave whatever is scheduled; the worker checks again.
                        health == null -> {}
                        health.has(Features.SAVED_SEARCH_POLL) -> enqueue(wm)
                        else -> wm.cancelUniqueWork(SavedSearchPollWorker.WORK_NAME)
                    }
                }
        }
    }

    private fun enqueue(wm: WorkManager) {
        val request = PeriodicWorkRequestBuilder<SavedSearchPollWorker>(1, TimeUnit.HOURS)
            .setConstraints(Constraints.Builder().setRequiredNetworkType(NetworkType.CONNECTED).build())
            .build()
        wm.enqueueUniquePeriodicWork(SavedSearchPollWorker.WORK_NAME, ExistingPeriodicWorkPolicy.KEEP, request)
    }
}

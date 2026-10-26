package app.magnetar.work

import android.content.Context
import android.util.Log
import androidx.work.BackoffPolicy
import androidx.work.Constraints
import androidx.work.CoroutineWorker
import androidx.work.ExistingPeriodicWorkPolicy
import androidx.work.ExistingWorkPolicy
import androidx.work.NetworkType
import androidx.work.OneTimeWorkRequestBuilder
import androidx.work.PeriodicWorkRequestBuilder
import androidx.work.WorkManager
import androidx.work.WorkerParameters
import androidx.work.workDataOf
import app.magnetar.MagnetarApp
import app.magnetar.Notifications
import app.magnetar.ffiCatching
import app.magnetar.userMessage
import java.util.concurrent.TimeUnit
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext

/**
 * Hourly background job (the app is a server frontend: the phone no longer crawls):
 * cache cleanup, then the saved-search poll through the server, with notifications.
 * Each step runs even if the other failed; failures are reported in the output data and the log.
 */
class CrawlSyncWorker(context: Context, params: WorkerParameters) : CoroutineWorker(context, params) {

    override suspend fun doWork(): Result {
        val app = applicationContext as MagnetarApp
        val m = ffiCatching { app.magnetar() }.getOrElse {
            Log.w(TAG, "open failed", it)
            return Result.failure(workDataOf(KEY_ERRORS to arrayOf(it.userMessage())))
        }
        val errors = mutableListOf<String>()
        val summary = mutableListOf<String>()

        ffiCatching { withContext(Dispatchers.IO) { m.cleanup() } }
            .onSuccess { r -> if (r.evicted > 0u) summary += "cleanup: ${r.evicted} evicted" }
            .onFailure { errors += "cleanup: ${it.userMessage()}" }
        ffiCatching { m.pollSavedSearches() }
            .onSuccess { hits ->
                val withNew = hits.filter { it.newHits.isNotEmpty() }
                summary += if (withNew.isEmpty()) "saved searches: nothing new" else "saved searches: ${withNew.size} with new hits"
                Notifications.postSavedSearchHits(applicationContext, withNew)
            }
            .onFailure { errors += "saved searches: ${it.userMessage()}" }

        errors.forEach { Log.w(TAG, it) }
        return Result.success(
            workDataOf(KEY_SUMMARY to summary.toTypedArray(), KEY_ERRORS to errors.toTypedArray()),
        )
    }

    companion object {
        private const val TAG = "CrawlSyncWorker"

        const val PERIODIC_NAME = "crawl-sync"
        const val NOW_NAME = "crawl-sync-now"
        const val KEY_SUMMARY = "summary"
        const val KEY_ERRORS = "errors"

        fun schedulePeriodic(context: Context, enabled: Boolean) {
            val wm = WorkManager.getInstance(context)
            if (!enabled) {
                wm.cancelUniqueWork(PERIODIC_NAME)
                return
            }
            // A few small API calls: any network, no charging needed.
            val constraints = Constraints.Builder()
                .setRequiredNetworkType(NetworkType.CONNECTED)
                .setRequiresBatteryNotLow(true)
                .build()
            val request = PeriodicWorkRequestBuilder<CrawlSyncWorker>(1, TimeUnit.HOURS)
                .setConstraints(constraints)
                .setBackoffCriteria(BackoffPolicy.EXPONENTIAL, 10, TimeUnit.MINUTES)
                .build()
            wm.enqueueUniquePeriodicWork(PERIODIC_NAME, ExistingPeriodicWorkPolicy.UPDATE, request)
        }

        /** User-requested run: any network, no charging requirement. */
        fun runNow(context: Context) {
            val request = OneTimeWorkRequestBuilder<CrawlSyncWorker>()
                .setConstraints(Constraints.Builder().setRequiredNetworkType(NetworkType.CONNECTED).build())
                .build()
            WorkManager.getInstance(context).enqueueUniqueWork(NOW_NAME, ExistingWorkPolicy.KEEP, request)
        }
    }
}

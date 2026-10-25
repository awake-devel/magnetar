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
 * crawl → sync → cleanup → saved-search poll. Each step runs even if an earlier one failed;
 * failures are reported in the output data and in the log.
 */
class CrawlSyncWorker(context: Context, params: WorkerParameters) : CoroutineWorker(context, params) {

    override suspend fun doWork(): Result {
        val app = applicationContext as MagnetarApp
        val mode = inputData.getString(KEY_MODE) ?: MODE_ALL
        val m = ffiCatching { app.magnetar() }.getOrElse {
            Log.w(TAG, "open failed", it)
            return Result.failure(workDataOf(KEY_ERRORS to arrayOf(it.userMessage())))
        }
        val errors = mutableListOf<String>()
        val summary = mutableListOf<String>()

        if (mode == MODE_ALL) {
            ffiCatching { m.crawlOnce(CRAWL_SECONDS) }
                .onSuccess { r ->
                    summary += "crawl: ${r.inserted} new of ${r.eventsSeen} from ${r.relaysOk}/${r.relaysTried} relays"
                    errors += r.errors.take(5)
                }
                .onFailure { errors += "crawl: ${it.userMessage()}" }
        }
        ffiCatching { m.syncNow() }
            .onSuccess { r ->
                summary += "sync (${r.method}): ${r.uploaded} uploaded"
                r.error?.let { errors += "sync: $it" }
            }
            .onFailure { errors += "sync: ${it.userMessage()}" }
        if (mode == MODE_ALL) {
            ffiCatching { withContext(Dispatchers.IO) { m.cleanup() } }
                .onSuccess { r -> if (r.evicted > 0u) summary += "cleanup: ${r.evicted} evicted" }
                .onFailure { errors += "cleanup: ${it.userMessage()}" }
        }
        ffiCatching { m.pollSavedSearches() }
            .onSuccess { hits ->
                val withNew = hits.filter { it.newHits.isNotEmpty() }
                if (withNew.isNotEmpty()) summary += "saved searches: ${withNew.size} with new hits"
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
        private const val CRAWL_SECONDS = 120u

        const val PERIODIC_NAME = "crawl-sync"
        const val NOW_NAME = "crawl-sync-now"
        const val KEY_MODE = "mode"
        const val KEY_SUMMARY = "summary"
        const val KEY_ERRORS = "errors"
        const val MODE_ALL = "all"
        const val MODE_SYNC = "sync"

        fun schedulePeriodic(context: Context, enabled: Boolean) {
            val wm = WorkManager.getInstance(context)
            if (!enabled) {
                wm.cancelUniqueWork(PERIODIC_NAME)
                return
            }
            val constraints = Constraints.Builder()
                .setRequiredNetworkType(NetworkType.UNMETERED)
                .setRequiresCharging(true)
                .setRequiresBatteryNotLow(true)
                .build()
            val request = PeriodicWorkRequestBuilder<CrawlSyncWorker>(1, TimeUnit.HOURS)
                .setConstraints(constraints)
                .setBackoffCriteria(BackoffPolicy.EXPONENTIAL, 10, TimeUnit.MINUTES)
                .setInputData(workDataOf(KEY_MODE to MODE_ALL))
                .build()
            wm.enqueueUniquePeriodicWork(PERIODIC_NAME, ExistingPeriodicWorkPolicy.UPDATE, request)
        }

        /** User-requested run: any network, no charging requirement. */
        fun runNow(context: Context, mode: String) {
            val request = OneTimeWorkRequestBuilder<CrawlSyncWorker>()
                .setConstraints(Constraints.Builder().setRequiredNetworkType(NetworkType.CONNECTED).build())
                .setInputData(workDataOf(KEY_MODE to mode))
                .build()
            WorkManager.getInstance(context).enqueueUniqueWork(NOW_NAME, ExistingWorkPolicy.KEEP, request)
        }
    }
}

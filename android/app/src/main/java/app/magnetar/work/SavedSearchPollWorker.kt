package app.magnetar.work

import android.Manifest
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import androidx.core.app.NotificationCompat
import androidx.core.app.NotificationManagerCompat
import androidx.core.content.ContextCompat
import androidx.core.content.edit
import androidx.core.net.toUri
import androidx.work.CoroutineWorker
import androidx.work.WorkerParameters
import app.magnetar.MagnetarApp
import app.magnetar.MainActivity
import app.magnetar.Notifications
import app.magnetar.R
import app.magnetar.api.ApiException
import app.magnetar.api.Features
import kotlinx.coroutines.CancellationException

/**
 * The hourly saved-search poll (docs/APP.md §8, D84): one `GET /api/v1/saved-searches/poll`
 * with the watermark this phone keeps, one notification per search with new hits.
 */
class SavedSearchPollWorker(context: Context, params: WorkerParameters) : CoroutineWorker(context, params) {
    override suspend fun doWork(): Result {
        val app = applicationContext as MagnetarApp
        val s = app.settingsStore.settings.value
        if (!s.notifySavedSearches || !s.hasServer) return Result.success()
        val prefs = app.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
        val since = sinceFor(
            prefs.getString(K_SERVER, null),
            prefs.getLong(K_WATERMARK, -1).takeIf { it >= 0 },
            s.serverUrl,
        )
        return try {
            val health = app.repository.refreshHealth()
            if (health != null && !health.has(Features.SAVED_SEARCH_POLL)) return Result.success()
            val poll = app.api().pollSavedSearches(since, s.showAdult)
            if (Notifications.canPost(app)) {
                poll.results.forEach { r -> alertContent(r)?.let { post(app, r.search.id, it) } }
            }
            prefs.edit {
                putString(K_SERVER, s.serverUrl)
                putLong(K_WATERMARK, poll.watermark)
            }
            Result.success()
        } catch (e: CancellationException) {
            throw e
        } catch (e: ApiException) {
            if (e is ApiException.Network || e is ApiException.Timeout) Result.retry() else Result.success()
        }
    }

    private fun post(context: Context, searchId: Long, c: AlertContent) {
        if (ContextCompat.checkSelfPermission(context, Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED &&
            android.os.Build.VERSION.SDK_INT >= android.os.Build.VERSION_CODES.TIRAMISU
        ) {
            return
        }
        val tap = Intent(Intent.ACTION_VIEW, "magnetar://torrent/${c.infohash}".toUri(), context, MainActivity::class.java)
            .addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP or Intent.FLAG_ACTIVITY_CLEAR_TOP)
        val id = (searchId % Int.MAX_VALUE).toInt()
        val pending = PendingIntent.getActivity(context, id, tap, PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT)
        val style = NotificationCompat.InboxStyle().also { st ->
            c.lines.forEach(st::addLine)
            c.more?.let(st::setSummaryText)
        }
        val n = NotificationCompat.Builder(context, Notifications.CHANNEL_SAVED)
            .setSmallIcon(R.drawable.ic_notification)
            .setContentTitle(c.title)
            .setContentText(c.lines.firstOrNull())
            .setStyle(style)
            .setContentIntent(pending)
            .setAutoCancel(true)
            .build()
        try {
            NotificationManagerCompat.from(context).notify(id, n)
        } catch (_: SecurityException) {
        }
    }

    companion object {
        const val WORK_NAME = "saved-search-poll"
        const val PREFS = "notify"
        const val K_SERVER = "server_url"
        const val K_WATERMARK = "watermark"
    }
}

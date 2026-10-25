package app.magnetar.download

import android.app.Notification
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.os.Build
import android.os.IBinder
import android.util.Log
import androidx.core.app.NotificationCompat
import androidx.core.app.NotificationManagerCompat
import androidx.core.app.ServiceCompat
import androidx.core.content.ContextCompat
import app.magnetar.MagnetarApp
import app.magnetar.Notifications
import app.magnetar.R
import app.magnetar.ffi.FfiDownload
import app.magnetar.ffiCatching
import app.magnetar.formatBytes
import app.magnetar.formatRate
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.withTimeoutOrNull

/**
 * Keeps the process alive while phone downloads run (dataSync FGS, android.md §6). Started only
 * from a user tap; stops itself once nothing is active, or on the Android 15+ dataSync timeout.
 */
class DownloadService : Service() {
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)
    private var poller: Job? = null

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        ServiceCompat.startForeground(
            this,
            Notifications.ID_DOWNLOAD_PROGRESS,
            progressNotification(emptyList()),
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) ServiceInfo.FOREGROUND_SERVICE_TYPE_DATA_SYNC else 0,
        )
        if (poller?.isActive != true) poller = scope.launch { poll() }
        return START_NOT_STICKY
    }

    private suspend fun poll() {
        val app = application as MagnetarApp
        var idlePolls = 0
        while (scope.isActive) {
            val list = ffiCatching { app.magnetar().downloads(false) }
                .onFailure { Log.w(TAG, "downloads() failed", it) }
                .getOrDefault(emptyList())
            val active = list.filter { it.state == "live" || it.state == "initializing" }
            // Allow a few polls for a just-added torrent to appear before giving up.
            idlePolls = if (active.isEmpty()) idlePolls + 1 else 0
            if (idlePolls >= IDLE_POLLS_BEFORE_STOP) {
                stopSelf()
                return
            }
            notifyProgress(active)
            delay(POLL_MS)
        }
    }

    private fun notifyProgress(active: List<FfiDownload>) {
        if (!Notifications.canPost(this)) return
        try {
            NotificationManagerCompat.from(this)
                .notify(Notifications.ID_DOWNLOAD_PROGRESS, progressNotification(active))
        } catch (_: SecurityException) {
        }
    }

    private fun progressNotification(active: List<FfiDownload>): Notification {
        val total = active.sumOf { it.total }
        val done = active.sumOf { it.downloaded }
        val rate = active.sumOf { it.downBps }
        val title = when (active.size) {
            0 -> "Starting download…"
            1 -> active[0].name
            else -> "${active.size} downloads"
        }
        val b = NotificationCompat.Builder(this, Notifications.CHANNEL_DOWNLOADS)
            .setSmallIcon(R.drawable.ic_notification)
            .setContentTitle(title)
            .setOngoing(true)
            .setOnlyAlertOnce(true)
            .setContentIntent(Notifications.openAppIntent(this))
            .setForegroundServiceBehavior(NotificationCompat.FOREGROUND_SERVICE_IMMEDIATE)
        if (total > 0u) {
            val pct = (done.toDouble() / total.toDouble() * 100).toInt().coerceIn(0, 100)
            b.setProgress(100, pct, false)
                .setContentText("${formatBytes(done)} / ${formatBytes(total)} · ${formatRate(rate)}")
        } else {
            b.setProgress(0, 0, true)
        }
        return b.build()
    }

    /** Android 15+ dataSync cap (6 h / 24 h): must stop within seconds. */
    override fun onTimeout(startId: Int, fgsType: Int) {
        poller?.cancel()
        val app = application as MagnetarApp
        scope.launch {
            withTimeoutOrNull(PAUSE_TIMEOUT_MS) { ffiCatching { app.magnetar().pauseAllDownloads() } }
            postPausedNotification()
            stopSelf()
        }
    }

    private fun postPausedNotification() {
        if (!Notifications.canPost(this)) return
        val n = NotificationCompat.Builder(this, Notifications.CHANNEL_DOWNLOADS)
            .setSmallIcon(R.drawable.ic_notification)
            .setContentTitle("Downloads paused")
            .setContentText("Android's background time limit was reached. Open Magnetar to resume.")
            .setContentIntent(Notifications.openAppIntent(this))
            .setAutoCancel(true)
            .build()
        try {
            NotificationManagerCompat.from(this).notify(Notifications.ID_DOWNLOAD_PAUSED, n)
        } catch (_: SecurityException) {
        }
    }

    override fun onDestroy() {
        scope.cancel()
        super.onDestroy()
    }

    companion object {
        private const val TAG = "DownloadService"
        private const val POLL_MS = 2_000L
        private const val IDLE_POLLS_BEFORE_STOP = 5
        private const val PAUSE_TIMEOUT_MS = 3_000L

        /** Call only in response to a user action (dataSync FGS start rules). */
        fun start(context: Context) {
            ContextCompat.startForegroundService(context, Intent(context, DownloadService::class.java))
        }
    }
}

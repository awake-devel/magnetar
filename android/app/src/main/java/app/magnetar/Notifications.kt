package app.magnetar

import android.Manifest
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Build
import androidx.core.app.NotificationCompat
import androidx.core.app.NotificationManagerCompat
import androidx.core.content.ContextCompat
import app.magnetar.ffi.FfiSavedSearchHit

object Notifications {
    const val CHANNEL_SAVED = "saved_searches"
    const val CHANNEL_DOWNLOADS = "downloads"

    const val ID_DOWNLOAD_PROGRESS = 1
    const val ID_DOWNLOAD_PAUSED = 2
    private const val ID_SAVED_BASE = 1000

    fun createChannels(context: Context) {
        val nm = context.getSystemService(NotificationManager::class.java)
        nm.createNotificationChannels(
            listOf(
                NotificationChannel(CHANNEL_SAVED, "Saved search alerts", NotificationManager.IMPORTANCE_DEFAULT),
                NotificationChannel(CHANNEL_DOWNLOADS, "Downloads", NotificationManager.IMPORTANCE_LOW),
            ),
        )
    }

    fun canPost(context: Context): Boolean =
        (Build.VERSION.SDK_INT < Build.VERSION_CODES.TIRAMISU ||
            ContextCompat.checkSelfPermission(context, Manifest.permission.POST_NOTIFICATIONS) ==
            PackageManager.PERMISSION_GRANTED) &&
            NotificationManagerCompat.from(context).areNotificationsEnabled()

    fun torrentIntent(context: Context, infohash: String): PendingIntent {
        val intent = Intent(Intent.ACTION_VIEW, Uri.parse("magnetar://torrent/$infohash"))
            .setPackage(context.packageName)
            .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_SINGLE_TOP)
        return PendingIntent.getActivity(
            context, infohash.hashCode(), intent,
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
    }

    fun openAppIntent(context: Context): PendingIntent = PendingIntent.getActivity(
        context, 0,
        Intent(context, MainActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP),
        PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
    )

    /** One notification per saved search with new hits; tapping opens the newest hit. */
    fun postSavedSearchHits(context: Context, hits: List<FfiSavedSearchHit>) {
        if (!canPost(context)) return
        val nm = NotificationManagerCompat.from(context)
        for (hit in hits) {
            val first = hit.newHits.firstOrNull() ?: continue
            val n = hit.newHits.size
            val style = NotificationCompat.InboxStyle()
            hit.newHits.take(5).forEach { style.addLine(it.title) }
            if (n > 5) style.setSummaryText("+${n - 5} more")
            val notification = NotificationCompat.Builder(context, CHANNEL_SAVED)
                .setSmallIcon(R.drawable.ic_notification)
                .setContentTitle("${hit.search.name}: $n new")
                .setContentText(first.title)
                .setStyle(style)
                .setContentIntent(torrentIntent(context, first.infohash))
                .setAutoCancel(true)
                .build()
            try {
                nm.notify(ID_SAVED_BASE + (hit.search.id % 100_000).toInt(), notification)
            } catch (_: SecurityException) {
                // Permission revoked between the check and the call.
            }
        }
    }
}

package app.magnetar

import android.Manifest
import android.app.NotificationChannel
import android.app.NotificationManager
import android.content.Context
import android.content.pm.PackageManager
import android.os.Build
import androidx.core.app.NotificationManagerCompat
import androidx.core.content.ContextCompat

object Notifications {
    /** The old app's channel id, so the user's sound/silence choices for it survive (D84). */
    const val CHANNEL_SAVED = "saved_searches"

    /** The old app's download channel; there are no downloads any more. */
    private const val LEGACY_CHANNEL_DOWNLOADS = "downloads"

    fun createChannels(context: Context) {
        val nm = context.getSystemService(NotificationManager::class.java) ?: return
        nm.createNotificationChannel(
            NotificationChannel(CHANNEL_SAVED, "Saved search alerts", NotificationManager.IMPORTANCE_DEFAULT),
        )
        nm.deleteNotificationChannel(LEGACY_CHANNEL_DOWNLOADS)
    }

    fun canPost(context: Context): Boolean =
        (Build.VERSION.SDK_INT < Build.VERSION_CODES.TIRAMISU ||
            ContextCompat.checkSelfPermission(context, Manifest.permission.POST_NOTIFICATIONS) ==
            PackageManager.PERMISSION_GRANTED) &&
            NotificationManagerCompat.from(context).areNotificationsEnabled()
}

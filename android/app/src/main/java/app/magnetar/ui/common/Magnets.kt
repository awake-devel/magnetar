package app.magnetar.ui.common

import android.content.ActivityNotFoundException
import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Build
import app.magnetar.MainActivity

/**
 * Hands [magnet] to another torrent app. Magnetar itself handles magnet: links, so it is excluded.
 * Returns false (after copying the magnet) when no other app can take it.
 */
fun openMagnetExternally(context: Context, magnet: String): Boolean {
    val intent = Intent(Intent.ACTION_VIEW, Uri.parse(magnet)).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
    val others = queryViewers(context, intent).filter { it.packageName != context.packageName }
    return try {
        when (others.size) {
            0 -> throw ActivityNotFoundException()
            1 -> context.startActivity(intent.setClassName(others[0].packageName, others[0].name))
            else -> context.startActivity(
                Intent.createChooser(intent, "Open magnet with")
                    .putExtra(Intent.EXTRA_EXCLUDE_COMPONENTS, arrayOf(ComponentName(context, MainActivity::class.java)))
                    .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK),
            )
        }
        true
    } catch (_: ActivityNotFoundException) {
        copyToClipboard(context, "magnet", magnet)
        false
    }
}

private fun queryViewers(context: Context, intent: Intent) =
    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
        context.packageManager.queryIntentActivities(intent, PackageManager.ResolveInfoFlags.of(0))
    } else {
        @Suppress("DEPRECATION")
        context.packageManager.queryIntentActivities(intent, 0)
    }.map { it.activityInfo }

fun fallbackMagnet(infohash: String): String = "magnet:?xt=urn:btih:$infohash"

fun shortKey(npub: String): String = if (npub.length > 20) npub.take(12) + "…" + npub.takeLast(6) else npub

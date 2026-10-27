package app.magnetar.ui.common

import android.content.ActivityNotFoundException
import android.content.Context
import android.content.Intent
import androidx.core.net.toUri

/**
 * *Open magnet* (docs/APP.md §1): hands [magnet] to a torrent app with `ACTION_VIEW`; with no
 * such app the magnet is copied instead and false is returned. The app downloads nothing itself.
 */
fun openMagnetExternally(context: Context, magnet: String): Boolean {
    val intent = Intent(Intent.ACTION_VIEW, magnet.toUri()).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
    return try {
        context.startActivity(intent)
        true
    } catch (_: ActivityNotFoundException) {
        copyToClipboard(context, "magnet", magnet)
        false
    } catch (_: SecurityException) {
        copyToClipboard(context, "magnet", magnet)
        false
    }
}

fun fallbackMagnet(infohash: String): String = "magnet:?xt=urn:btih:$infohash"

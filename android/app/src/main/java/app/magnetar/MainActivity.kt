package app.magnetar

import android.Manifest
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import android.os.Bundle
import android.widget.Toast
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.activity.result.contract.ActivityResultContracts
import androidx.core.content.ContextCompat
import androidx.core.content.edit
import app.magnetar.ui.MagnetarNavHost
import app.magnetar.ui.theme.MagnetarTheme
import kotlinx.coroutines.flow.MutableStateFlow

/** A torrent to show, from a magnet: link, a magnetar://torrent/ deep link or a notification. */
data class OpenRequest(val infohash: String, val magnet: String?)

class MainActivity : ComponentActivity() {
    private val openRequests = MutableStateFlow<OpenRequest?>(null)

    private val notificationPermission =
        registerForActivityResult(ActivityResultContracts.RequestPermission()) { }

    override fun onCreate(savedInstanceState: Bundle?) {
        enableEdgeToEdge()
        super.onCreate(savedInstanceState)
        if (savedInstanceState == null) handleIntent(intent)
        askNotificationsOnce()
        setContent {
            MagnetarTheme {
                MagnetarNavHost(openRequests)
            }
        }
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        setIntent(intent)
        handleIntent(intent)
    }

    private fun handleIntent(intent: Intent?) {
        if (intent?.action != Intent.ACTION_VIEW) return
        val uri = intent.data ?: return
        val request = parseOpenUri(uri.toString())
        if (request == null) {
            Toast.makeText(this, "Not a BitTorrent v1 magnet link", Toast.LENGTH_LONG).show()
            return
        }
        openRequests.value = request
    }

    /** Saved-search alerts and download progress need it on API 33+; ask once, Settings can ask again. */
    private fun askNotificationsOnce() {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.TIRAMISU) return
        if (ContextCompat.checkSelfPermission(this, Manifest.permission.POST_NOTIFICATIONS) ==
            PackageManager.PERMISSION_GRANTED
        ) {
            return
        }
        val prefs = getSharedPreferences("ui", MODE_PRIVATE)
        if (prefs.getBoolean("asked_notifications", false)) return
        prefs.edit { putBoolean("asked_notifications", true) }
        notificationPermission.launch(Manifest.permission.POST_NOTIFICATIONS)
    }
}

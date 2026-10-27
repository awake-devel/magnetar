package app.magnetar.ui.settings

import androidx.compose.material3.Button
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import app.magnetar.BuildConfig
import app.magnetar.MagnetarApp
import app.magnetar.api.ApiException
import app.magnetar.ui.common.PlaceholderScreen
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.launch

/**
 * Skeleton: shows the migrated server and checks it. The full screen (URL + token fields, Amber,
 * threshold, adult switch, notifications, attribution) comes in the settings stage.
 */
@Composable
fun SettingsScreen(app: MagnetarApp) {
    val settings by app.settingsStore.settings.collectAsState()
    val scope = rememberCoroutineScope()
    var result by remember { mutableStateOf<String?>(null) }

    PlaceholderScreen(
        title = "Settings",
        text = buildString {
            append("Server: ").append(settings.serverUrl.ifEmpty { "not set" }).append('\n')
            append("Token: ").append(if (settings.apiToken.isEmpty()) "not set" else "set").append('\n')
            append("Show adult content: ").append(if (settings.showAdult) "on" else "off").append('\n')
            append("Saved-search notifications: ").append(if (settings.notifySavedSearches) "on" else "off").append('\n')
            append("Version ").append(BuildConfig.VERSION_NAME)
        },
    ) {
        Button(onClick = {
            result = "Testing…"
            scope.launch {
                result = try {
                    val h = app.api().testConnection()
                    "Connected: server ${h.version}, token accepted"
                } catch (e: CancellationException) {
                    throw e
                } catch (e: ApiException) {
                    when (e) {
                        is ApiException.NotConfigured -> "No server set"
                        is ApiException.Unauthorized -> "Token rejected"
                        else -> e.message ?: "Failed"
                    }
                }
            }
        }) { Text("Test connection") }
        result?.let { Text(it) }
    }
}

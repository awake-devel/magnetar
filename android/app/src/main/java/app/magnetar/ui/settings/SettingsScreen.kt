package app.magnetar.ui.settings

import android.Manifest
import android.os.Build
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Slider
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import app.magnetar.Notifications
import app.magnetar.ui.common.SectionTitle
import kotlin.math.roundToInt

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun SettingsScreen(vm: SettingsViewModel = viewModel()) {
    val state by vm.state.collectAsStateWithLifecycle()
    val form = state.form
    val context = LocalContext.current
    val snackbar = remember { SnackbarHostState() }
    var notificationsOk by remember { mutableStateOf(Notifications.canPost(context)) }
    val permissionLauncher = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) { granted ->
        notificationsOk = granted && Notifications.canPost(context)
    }
    LaunchedEffect(vm) { vm.messages.collect { snackbar.showSnackbar(it) } }

    Scaffold(
        topBar = { TopAppBar(title = { Text("Settings") }) },
        snackbarHost = { SnackbarHost(snackbar) },
    ) { padding ->
        Column(
            Modifier.padding(padding).fillMaxSize().verticalScroll(rememberScrollState()).padding(horizontal = 16.dp),
        ) {
            SectionTitle("Server")
            Field(
                "Server URL", form.serverUrl, { v -> vm.edit { it.copy(serverUrl = v) } },
                error = state.serverUrlError,
                hint = state.serverWarning ?: "Empty = offline only. Default http://127.0.0.1:8787 (Termux).",
                keyboard = KeyboardType.Uri,
            )
            OutlinedTextField(
                value = form.apiToken,
                onValueChange = { v -> vm.edit { it.copy(apiToken = v) } },
                label = { Text("API token") },
                singleLine = true,
                visualTransformation = PasswordVisualTransformation(),
                modifier = Modifier.fillMaxWidth(),
            )
            Row(Modifier.padding(top = 8.dp), verticalAlignment = Alignment.CenterVertically) {
                OutlinedButton(onClick = vm::testConnection, enabled = !state.testing) {
                    Text(if (state.testing) "Testing…" else "Test connection")
                }
                Spacer(Modifier.padding(4.dp))
                state.testResult?.let { Text(it, style = MaterialTheme.typography.bodySmall) }
            }

            SectionTitle("Trust")
            Field(
                "Root npub", form.rootNpub, { v -> vm.edit { it.copy(rootNpub = v) } },
                error = state.npubError,
                hint = "Your key (npub, hex or nprofile). Trust is computed from its follows.",
            )
            Text("Trust threshold: ${form.trustThreshold}", style = MaterialTheme.typography.bodyMedium)
            Slider(
                value = form.trustThreshold.toFloat(),
                onValueChange = { v -> vm.edit { it.copy(trustThreshold = v.roundToInt()) } },
                valueRange = 0f..100f,
            )

            SectionTitle("Cache and crawling")
            Field(
                "Cache cap (MiB)", form.cacheCapMb, { v -> vm.edit { it.copy(cacheCapMb = v.filter(Char::isDigit)) } },
                error = state.cacheError,
                keyboard = KeyboardType.Number,
            )
            OutlinedTextField(
                value = form.seedRelays,
                onValueChange = { v -> vm.edit { it.copy(seedRelays = v) } },
                label = { Text("Seed relays (one per line)") },
                supportingText = { Text(state.relaysError ?: "Empty = built-in defaults") },
                isError = state.relaysError != null,
                minLines = 3,
                modifier = Modifier.fillMaxWidth(),
            )
            Row(Modifier.fillMaxWidth().padding(top = 8.dp), verticalAlignment = Alignment.CenterVertically) {
                Column(Modifier.weight(1f)) {
                    Text("Background crawl and sync")
                    Text("Hourly, on Wi-Fi while charging", style = MaterialTheme.typography.bodySmall)
                }
                Switch(checked = form.backgroundSync, onCheckedChange = { v -> vm.edit { it.copy(backgroundSync = v) } })
            }

            SectionTitle("Notifications")
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(
                    if (notificationsOk) "Allowed" else "Not allowed: saved-search alerts and download progress stay silent",
                    Modifier.weight(1f),
                    style = MaterialTheme.typography.bodyMedium,
                )
                if (!notificationsOk && Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
                    OutlinedButton(onClick = { permissionLauncher.launch(Manifest.permission.POST_NOTIFICATIONS) }) {
                        Text("Allow")
                    }
                }
            }

            Row(
                Modifier.fillMaxWidth().padding(vertical = 16.dp),
                horizontalArrangement = Arrangement.End,
            ) {
                Button(onClick = vm::save, enabled = state.dirty) { Text("Save") }
            }
            Text(
                "Magnetar ${vm.appVersion} · core ${state.coreVersion ?: "…"}",
                style = MaterialTheme.typography.labelSmall,
            )
            TmdbAttribution()
            Spacer(Modifier.height(24.dp))
        }
    }
}

@Composable
private fun Field(
    label: String,
    value: String,
    onChange: (String) -> Unit,
    error: String? = null,
    hint: String? = null,
    keyboard: KeyboardType = KeyboardType.Text,
) {
    OutlinedTextField(
        value = value,
        onValueChange = onChange,
        label = { Text(label) },
        singleLine = true,
        isError = error != null,
        supportingText = (error ?: hint)?.let { { Text(it) } },
        keyboardOptions = KeyboardOptions(keyboardType = keyboard),
        modifier = Modifier.fillMaxWidth(),
    )
}

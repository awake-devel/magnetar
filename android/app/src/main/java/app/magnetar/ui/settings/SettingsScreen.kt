package app.magnetar.ui.settings

import android.Manifest
import android.app.Activity
import android.content.Intent
import android.net.Uri
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
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Slider
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.text.input.VisualTransformation
import androidx.compose.ui.unit.dp
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.compose.LocalLifecycleOwner
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.repeatOnLifecycle
import app.magnetar.BuildConfig
import app.magnetar.Notifications
import app.magnetar.api.Features
import app.magnetar.api.Health
import app.magnetar.settings.AppSettings
import app.magnetar.settings.Nip19
import app.magnetar.ui.common.SectionTitle
import app.magnetar.ui.common.shortKey
import kotlin.math.roundToInt

/** NIP-55 `get_public_key` request to the installed signer app (Amber); the private key stays there. */
private fun signerIntent(): Intent =
    Intent(Intent.ACTION_VIEW, Uri.parse("nostrsigner:")).putExtra("type", "get_public_key")

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun SettingsScreen(vm: SettingsViewModel, settings: AppSettings, health: Health?) {
    val state by vm.state.collectAsStateWithLifecycle()
    val form = state.form
    val context = LocalContext.current
    val snackbar = remember { SnackbarHostState() }
    LaunchedEffect(vm) { vm.messages.collect { snackbar.showSnackbar(it) } }

    // Notification permission: read again whenever the screen comes back (the user may have
    // changed it in the system settings).
    var notificationsOk by remember { mutableStateOf(Notifications.canPost(context)) }
    val lifecycle = LocalLifecycleOwner.current.lifecycle
    LaunchedEffect(lifecycle) {
        lifecycle.repeatOnLifecycle(Lifecycle.State.RESUMED) { notificationsOk = Notifications.canPost(context) }
    }
    val permission = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) {
        notificationsOk = Notifications.canPost(context)
    }
    val hasSigner = remember { signerIntent().resolveActivity(context.packageManager) != null }
    val signer = rememberLauncherForActivityResult(ActivityResultContracts.StartActivityForResult()) { r ->
        val data = r.data
        if (r.resultCode == Activity.RESULT_OK && data?.getBooleanExtra("rejected", false) != true) {
            // NIP-55 answers in `result`; older Amber versions used `signature`.
            vm.onSignerKey(data?.getStringExtra("result") ?: data?.getStringExtra("signature"))
        }
    }
    var showToken by rememberSaveable { mutableStateOf(false) }

    Scaffold(
        topBar = { TopAppBar(title = { Text("Settings") }) },
        snackbarHost = { SnackbarHost(snackbar) },
    ) { padding ->
        Column(
            Modifier
                .padding(padding)
                .fillMaxSize()
                .verticalScroll(rememberScrollState())
                .padding(horizontal = 16.dp)
                .testTag("settings_page"),
        ) {
            SectionTitle("Server")
            OutlinedTextField(
                value = form.serverUrl,
                onValueChange = vm::setServerUrl,
                label = { Text("Server URL") },
                singleLine = true,
                isError = state.url.error != null,
                supportingText = {
                    Text(state.url.error ?: state.url.warning ?: "Your Magnetar server, e.g. https://myhost.tailnet.ts.net")
                },
                keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Uri, imeAction = ImeAction.Next),
                modifier = Modifier.fillMaxWidth().testTag("settings_url"),
            )
            OutlinedTextField(
                value = form.apiToken,
                onValueChange = vm::setToken,
                label = { Text("API token") },
                singleLine = true,
                isError = state.token.error != null,
                supportingText = state.token.error?.let { { Text(it) } },
                visualTransformation = if (showToken) VisualTransformation.None else PasswordVisualTransformation(),
                keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Password),
                trailingIcon = {
                    TextButton(onClick = { showToken = !showToken }) { Text(if (showToken) "Hide" else "Show") }
                },
                modifier = Modifier.fillMaxWidth().testTag("settings_token"),
            )
            Row(Modifier.padding(top = 8.dp), verticalAlignment = Alignment.CenterVertically) {
                OutlinedButton(
                    onClick = vm::testConnection,
                    enabled = !state.testing,
                    modifier = Modifier.testTag("settings_test"),
                ) { Text(if (state.testing) "Testing…" else "Test connection") }
            }
            state.testResult?.let {
                Text(
                    (if (state.testOk == true) "✓ " else "✗ ") + it,
                    style = MaterialTheme.typography.bodySmall,
                    color = if (state.testOk == true) MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.error,
                    modifier = Modifier.padding(top = 4.dp).testTag("settings_test_result"),
                )
            }

            SectionTitle("Trust")
            OutlinedTextField(
                value = form.rootKey,
                onValueChange = vm::setRootKey,
                label = { Text("Trust root (your public key)") },
                singleLine = true,
                isError = state.root.error != null,
                supportingText = {
                    Text(state.root.error ?: "npub, nprofile or hex. Never your nsec: trust is computed from the accounts it follows.")
                },
                modifier = Modifier.fillMaxWidth().testTag("settings_root"),
            )
            if (hasSigner) {
                OutlinedButton(onClick = { signer.launch(signerIntent()) }, modifier = Modifier.testTag("settings_amber")) {
                    Text("Get my key from Amber")
                }
            }
            Text(
                "Trust threshold: ${form.threshold}",
                style = MaterialTheme.typography.bodyMedium,
                modifier = Modifier.padding(top = 12.dp),
            )
            Slider(
                value = form.threshold.toFloat(),
                onValueChange = { vm.setThreshold(it.roundToInt()) },
                valueRange = 0f..100f,
                modifier = Modifier
                    .testTag("settings_threshold")
                    .semantics { stateDescription = "${form.threshold} of 100" },
            )
            Text(
                "Publishers scoring below it are hidden from search. The server applies it to every client.",
                style = MaterialTheme.typography.bodySmall,
            )
            ServerTrustLine(state)
            Row(
                Modifier.fillMaxWidth().padding(vertical = 12.dp),
                horizontalArrangement = Arrangement.End,
                verticalAlignment = Alignment.CenterVertically,
            ) {
                if (state.dirty) {
                    Text("Unsaved changes", style = MaterialTheme.typography.labelSmall, modifier = Modifier.weight(1f))
                }
                Button(onClick = vm::save, enabled = state.canSave, modifier = Modifier.testTag("settings_save")) {
                    Text(if (state.saving) "Saving…" else "Save")
                }
            }
            HorizontalDivider()

            SectionTitle("Content")
            SwitchRow(
                title = "Show adult content",
                text = "Off: adult torrents stay out of search, browse, title pages, stats and saved-search alerts. " +
                    "They are hidden, not deleted.",
                checked = settings.showAdult,
                onChange = vm::setShowAdult,
                tag = "settings_adult",
            )

            SectionTitle("Notifications")
            val pollSupported = health?.has(Features.SAVED_SEARCH_POLL)
            SwitchRow(
                title = "Saved-search alerts",
                text = when {
                    !settings.hasServer -> "Hourly check of your saved searches on the server (set a server first)"
                    pollSupported == false -> "Needs a newer server (it does not offer the saved-search poll)"
                    else -> "Hourly check of your saved searches on the server for new matches"
                },
                checked = settings.notifySavedSearches,
                onChange = vm::setNotify,
                tag = "settings_notify",
            )
            Row(verticalAlignment = Alignment.CenterVertically, modifier = Modifier.padding(top = 4.dp)) {
                Text(
                    if (notificationsOk) "Notifications allowed" else "Notifications not allowed: alerts stay silent",
                    Modifier.weight(1f),
                    style = MaterialTheme.typography.bodySmall,
                )
                if (!notificationsOk && Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
                    OutlinedButton(onClick = { permission.launch(Manifest.permission.POST_NOTIFICATIONS) }) { Text("Allow") }
                }
            }
            HorizontalDivider(Modifier.padding(top = 12.dp))

            SectionTitle("About")
            Text(
                "Magnetar ${BuildConfig.VERSION_NAME}" + (health?.version?.takeIf { it.isNotBlank() }?.let { " · server $it" } ?: ""),
                style = MaterialTheme.typography.bodyMedium,
                modifier = Modifier.testTag("settings_version"),
            )
            TmdbAttribution()
            Spacer(Modifier.height(24.dp))
        }
    }
}

@Composable
private fun ServerTrustLine(state: SettingsUiState) {
    val t = state.serverTrust
    val text = when {
        state.serverTrustError != null -> "Server trust settings: ${state.serverTrustError}"
        t == null -> null
        else -> "On the server: threshold ${t.threshold}, root " + (t.root?.let { shortKey(Nip19.npub(it)) } ?: "none") +
            if (t.threshold != state.saved.threshold) " · Save sends ${state.saved.threshold}" else ""
    }
    text?.let {
        Text(
            it,
            style = MaterialTheme.typography.labelSmall,
            modifier = Modifier.padding(top = 6.dp).testTag("settings_server_trust"),
        )
    }
}

@Composable
private fun SwitchRow(title: String, text: String, checked: Boolean, onChange: (Boolean) -> Unit, tag: String) {
    Row(Modifier.fillMaxWidth().padding(vertical = 4.dp), verticalAlignment = Alignment.CenterVertically) {
        Column(Modifier.weight(1f)) {
            Text(title, style = MaterialTheme.typography.bodyLarge)
            Text(text, style = MaterialTheme.typography.bodySmall)
        }
        Spacer(Modifier.width(12.dp))
        Switch(
            checked = checked,
            onCheckedChange = onChange,
            modifier = Modifier.testTag(tag).semantics { contentDescription = title },
        )
    }
}

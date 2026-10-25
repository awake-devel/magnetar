package app.magnetar.ui.status

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Refresh
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.ListItem
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import app.magnetar.ffi.FfiStats
import app.magnetar.formatAgo
import app.magnetar.formatBytes
import app.magnetar.ui.common.Banner
import app.magnetar.ui.common.SectionTitle

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun StatusScreen(vm: StatusViewModel = viewModel()) {
    val state by vm.state.collectAsStateWithLifecycle()
    val snackbar = remember { SnackbarHostState() }
    LaunchedEffect(vm) { vm.messages.collect { snackbar.showSnackbar(it) } }

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text("Status") },
                actions = { IconButton(onClick = vm::refresh) { Icon(Icons.Filled.Refresh, "Refresh") } },
            )
        },
        snackbarHost = { SnackbarHost(snackbar) },
    ) { padding ->
        LazyColumn(Modifier.padding(padding).fillMaxSize()) {
            state.error?.let { item { Banner(it, error = true) } }
            item {
                Column(Modifier.padding(horizontal = 16.dp)) {
                    state.stats?.let { StatsCard(it) }
                    ServerCard(state, vm.hasServer, vm::checkServer)
                    Row(Modifier.padding(top = 12.dp), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                        Button(onClick = vm::crawlNow, enabled = !state.workRunning) { Text("Crawl now") }
                        OutlinedButton(onClick = vm::syncNow, enabled = !state.workRunning && vm.hasServer) {
                            Text("Sync now")
                        }
                    }
                    if (state.workRunning) {
                        Text("Running…", style = MaterialTheme.typography.bodySmall)
                        LinearProgressIndicator(Modifier.fillMaxWidth())
                    }
                    state.lastRun.forEach { Text(it, style = MaterialTheme.typography.bodySmall) }
                    state.lastRunErrors.forEach {
                        Text(it, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.error)
                    }
                    SectionTitle("Relays (${state.relays.size})")
                }
            }
            items(state.relays, key = { it.url }) { r ->
                ListItem(
                    headlineContent = { Text(r.url, maxLines = 1, overflow = TextOverflow.Ellipsis) },
                    supportingContent = {
                        Text(
                            listOfNotNull(
                                r.source,
                                "${r.eventsFound} events",
                                r.lastOk?.let { "ok ${formatAgo(it)}" },
                                r.lastErr?.let { "error: $it" },
                            ).joinToString(" · "),
                            maxLines = 2,
                            overflow = TextOverflow.Ellipsis,
                        )
                    },
                    trailingContent = { Switch(checked = r.enabled, onCheckedChange = { vm.setRelayEnabled(r, it) }) },
                )
            }
        }
    }
}

@Composable
private fun StatsCard(s: FfiStats) {
    Card(Modifier.fillMaxWidth().padding(top = 8.dp)) {
        Column(Modifier.padding(12.dp)) {
            Text("Local cache", style = MaterialTheme.typography.titleSmall)
            StatLine("Torrents", s.torrents.toString())
            StatLine("Events", s.events.toString())
            StatLine("Publishers", s.publishers.toString())
            StatLine("Comments", s.comments.toString())
            StatLine("Relays", s.relays.toString())
            StatLine("Not yet synced", s.unsynced.toString())
            StatLine("Database size", formatBytes(s.dbBytes))
            StatLine("Last crawl", s.lastCrawl?.let { formatAgo(it) } ?: "never")
            StatLine("Last sync", s.lastSync?.let { formatAgo(it) } ?: "never")
        }
    }
}

@Composable
private fun StatLine(label: String, value: String) {
    Row(Modifier.fillMaxWidth()) {
        Text(label, Modifier.weight(1f), style = MaterialTheme.typography.bodyMedium)
        Text(value, style = MaterialTheme.typography.bodyMedium)
    }
}

@Composable
private fun ServerCard(state: StatusUiState, hasServer: Boolean, onCheck: () -> Unit) {
    Card(Modifier.fillMaxWidth().padding(top = 8.dp)) {
        Column(Modifier.padding(12.dp)) {
            Row {
                Text("Server", Modifier.weight(1f), style = MaterialTheme.typography.titleSmall)
                if (hasServer) {
                    OutlinedButton(onClick = onCheck, enabled = !state.checkingServer) { Text("Check") }
                }
            }
            Text(
                when {
                    !hasServer -> "Not configured (offline mode)"
                    state.checkingServer -> "Checking…"
                    state.serverHealthy == true -> "Healthy"
                    state.serverHealthy == false -> "Unreachable" + (state.serverError?.let { ": $it" } ?: "")
                    else -> "Unknown"
                },
                style = MaterialTheme.typography.bodyMedium,
            )
        }
    }
}

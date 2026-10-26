package app.magnetar.ui.status

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.ListItem
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import app.magnetar.backlogEta
import app.magnetar.ffi.FfiLivenessStats
import app.magnetar.ffi.FfiStats
import app.magnetar.formatAgo
import app.magnetar.formatBytes
import app.magnetar.formatCount
import app.magnetar.ui.common.Banner
import app.magnetar.ui.common.SectionTitle

/**
 * Local cache, server and seed checks: the Status section of the Stats tab (D47). The
 * caller owns the top bar (refresh) and the snackbar for [StatusViewModel.messages].
 */
@Composable
fun StatusContent(vm: StatusViewModel, modifier: Modifier = Modifier) {
    val state by vm.state.collectAsStateWithLifecycle()
    LazyColumn(modifier.fillMaxSize()) {
        state.error?.let { item { Banner(it, error = true) } }
        item {
            Column(Modifier.padding(horizontal = 16.dp)) {
                state.stats?.let { StatsCard(it) }
                ServerCard(state, vm.hasServer, vm::checkServer)
                state.liveness?.let { LivenessCard(it) }
                Row(Modifier.padding(top = 12.dp), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    Button(onClick = vm::checkSavedSearchesNow, enabled = !state.workRunning && vm.hasServer) {
                        Text("Check saved searches now")
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
            StatLine("Database size", formatBytes(s.dbBytes))
        }
    }
}

/** Seed checks on the server (D45): backlog, pace, latest results. */
@Composable
private fun LivenessCard(l: FfiLivenessStats) {
    Card(Modifier.fillMaxWidth().padding(top = 8.dp)) {
        Column(Modifier.padding(12.dp)) {
            Text("Seed checks (server)", style = MaterialTheme.typography.titleSmall)
            if (!l.enabled) Text("Turned off on the server", style = MaterialTheme.typography.bodySmall)
            StatLine("Never checked", "${formatCount(l.neverChecked)} of ${formatCount(l.torrents)}")
            StatLine("Due now", formatCount(l.dueNow))
            StatLine("Checked, last 24 h", formatCount(l.checked24h))
            StatLine("Checks per hour", formatCount(l.checksPerHour))
            backlogEta(l.dueNow, l.checksPerHour)?.let { StatLine("Backlog cleared in", it) }
            StatLine("Seeded / dead / no answer", "${formatCount(l.alive)} / ${formatCount(l.dead)} / ${formatCount(l.unknown)}")
            if (l.trackersBackedOff > 0uL) {
                StatLine("Trackers backing off", "${formatCount(l.trackersBackedOff)} of ${formatCount(l.trackers)}")
            }
            l.lastBatchAt?.let {
                StatLine("Last batch", "${formatCount(l.lastBatchSize)} in ${l.lastBatchMs.toLong() / 1000} s, ${formatAgo(it)}")
            }
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

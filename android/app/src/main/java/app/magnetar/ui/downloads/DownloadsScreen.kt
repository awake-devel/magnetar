package app.magnetar.ui.downloads

import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Delete
import androidx.compose.material.icons.filled.PlayArrow
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.PrimaryTabRow
import androidx.compose.material3.Scaffold
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.Tab
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.compose.LocalLifecycleOwner
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.repeatOnLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import app.magnetar.ffi.FfiDownload
import app.magnetar.formatBytes
import app.magnetar.formatRate
import app.magnetar.ui.common.AppIcons
import app.magnetar.ui.common.CenteredMessage
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive

private const val POLL_MS = 2_000L

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun DownloadsScreen(vm: DownloadsViewModel = viewModel()) {
    val state by vm.state.collectAsStateWithLifecycle()
    val snackbar = remember { SnackbarHostState() }
    val lifecycle = LocalLifecycleOwner.current
    var toDelete by remember { mutableStateOf<FfiDownload?>(null) }

    LaunchedEffect(vm) { vm.messages.collect { snackbar.showSnackbar(it) } }
    // Poll only while this screen is visible (STARTED), restarting on tab switch.
    LaunchedEffect(state.onServer, lifecycle) {
        lifecycle.repeatOnLifecycle(Lifecycle.State.STARTED) {
            while (isActive) {
                vm.refresh()
                delay(POLL_MS)
            }
        }
    }

    Scaffold(
        topBar = { TopAppBar(title = { Text("Downloads") }) },
        snackbarHost = { SnackbarHost(snackbar) },
    ) { padding ->
        Column(Modifier.padding(padding).fillMaxSize()) {
            PrimaryTabRow(selectedTabIndex = if (state.onServer) 1 else 0) {
                Tab(selected = !state.onServer, onClick = { vm.selectTab(false) }, text = { Text("Phone") })
                Tab(selected = state.onServer, onClick = { vm.selectTab(true) }, text = { Text("Server") })
            }
            when {
                state.onServer && !vm.hasServer -> CenteredMessage("No server configured. Set one in Settings.")
                state.error != null && state.items.isEmpty() -> CenteredMessage(state.error!!)
                !state.loaded -> LinearProgressIndicator(Modifier.fillMaxWidth())
                state.items.isEmpty() -> CenteredMessage("No downloads. Start one from a torrent's page.")
                else -> LazyColumn {
                    items(state.items, key = { it.id.toString() + it.infohash }) { d ->
                        DownloadRow(d, onAction = { a -> if (a == "delete") toDelete = d else vm.action(d, a) })
                        HorizontalDivider()
                    }
                }
            }
        }
    }

    toDelete?.let { d ->
        AlertDialog(
            onDismissRequest = { toDelete = null },
            title = { Text("Remove download?") },
            text = { Text("\"${d.name.ifBlank { d.infohash }}\" will be stopped and removed from the list.") },
            confirmButton = { TextButton(onClick = { toDelete = null; vm.action(d, "delete") }) { Text("Remove") } },
            dismissButton = { TextButton(onClick = { toDelete = null }) { Text("Cancel") } },
        )
    }
}

@Composable
private fun DownloadRow(d: FfiDownload, onAction: (String) -> Unit) {
    Column(Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 8.dp)) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text(
                d.name.ifBlank { d.infohash },
                Modifier.weight(1f),
                style = MaterialTheme.typography.titleSmall,
                maxLines = 2,
                overflow = TextOverflow.Ellipsis,
            )
            when (d.state) {
                "live", "initializing" -> IconButton(onClick = { onAction("pause") }) { Icon(AppIcons.Pause, "Pause") }
                "paused", "error" -> IconButton(onClick = { onAction("resume") }) { Icon(Icons.Filled.PlayArrow, "Resume") }
            }
            IconButton(onClick = { onAction("delete") }) { Icon(Icons.Filled.Delete, "Remove") }
        }
        LinearProgressIndicator(progress = { d.progress.coerceIn(0f, 1f) }, modifier = Modifier.fillMaxWidth())
        Box(Modifier.padding(top = 4.dp)) {
            Text(
                listOfNotNull(
                    d.state,
                    "${formatBytes(d.downloaded)} / ${formatBytes(d.total)}",
                    "${(d.progress * 100).toInt()}%",
                    if (d.state == "live") "↓ ${formatRate(d.downBps)} ↑ ${formatRate(d.upBps)}" else null,
                    if (d.state == "live") "${d.peers} peers" else null,
                ).joinToString(" · "),
                style = MaterialTheme.typography.bodySmall,
            )
        }
        d.error?.let { Text(it, color = MaterialTheme.colorScheme.error, style = MaterialTheme.typography.bodySmall) }
    }
}

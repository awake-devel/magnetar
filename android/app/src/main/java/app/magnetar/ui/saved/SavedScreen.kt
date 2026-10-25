package app.magnetar.ui.saved

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Delete
import androidx.compose.material.icons.filled.Refresh
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.ListItem
import androidx.compose.material3.Scaffold
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
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import app.magnetar.ffi.FfiSavedSearch
import app.magnetar.ffi.FfiSearchQuery
import app.magnetar.formatAge
import app.magnetar.formatBytes
import app.magnetar.ui.common.CenteredMessage
import app.magnetar.ui.common.categoryLabel

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun SavedScreen(onRun: () -> Unit, vm: SavedViewModel = viewModel()) {
    val state by vm.state.collectAsStateWithLifecycle()
    val snackbar = remember { SnackbarHostState() }
    var toDelete by remember { mutableStateOf<FfiSavedSearch?>(null) }
    LaunchedEffect(vm) { vm.messages.collect { snackbar.showSnackbar(it) } }

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text("Saved searches") },
                actions = { IconButton(onClick = vm::refresh) { Icon(Icons.Filled.Refresh, "Refresh") } },
            )
        },
        snackbarHost = { SnackbarHost(snackbar) },
    ) { padding ->
        Box(Modifier.padding(padding).fillMaxSize()) {
            when {
                state.loading && state.searches.isEmpty() ->
                    Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) { CircularProgressIndicator() }
                state.error != null -> CenteredMessage(state.error!!, "Retry", vm::refresh)
                state.searches.isEmpty() ->
                    CenteredMessage("No saved searches. Use the star on the Search screen to save one.")
                else -> LazyColumn {
                    items(state.searches, key = { it.id }) { s ->
                        ListItem(
                            modifier = Modifier.clickable { vm.run(s); onRun() },
                            headlineContent = { Text(s.name) },
                            supportingContent = {
                                Text(
                                    describe(s.query) +
                                        (s.lastChecked?.let { "\nchecked ${formatAge(it)} ago" } ?: ""),
                                )
                            },
                            trailingContent = {
                                Row(verticalAlignment = Alignment.CenterVertically) {
                                    Switch(checked = s.notify, onCheckedChange = { vm.setNotify(s, it) })
                                    IconButton(onClick = { toDelete = s }) { Icon(Icons.Filled.Delete, "Delete") }
                                }
                            },
                        )
                        HorizontalDivider()
                    }
                }
            }
        }
    }

    toDelete?.let { s ->
        AlertDialog(
            onDismissRequest = { toDelete = null },
            title = { Text("Delete \"${s.name}\"?") },
            confirmButton = { TextButton(onClick = { toDelete = null; vm.delete(s) }) { Text("Delete") } },
            dismissButton = { TextButton(onClick = { toDelete = null }) { Text("Cancel") } },
        )
    }
}

private fun describe(q: FfiSearchQuery): String = buildList {
    add(if (q.text.isBlank()) "any text" else "\"${q.text}\"")
    if (q.categories.isNotEmpty()) add(q.categories.joinToString("/") { categoryLabel(it) })
    q.minSize?.let { add("≥ ${formatBytes(it)}") }
    q.maxSize?.let { add("≤ ${formatBytes(it)}") }
    q.minSeeders?.let { add("≥ $it seeders") }
    if (q.hideDead) add("alive only")
    q.minTrust?.let { add("trust ≥ $it") }
}.joinToString(" · ")

package app.magnetar.ui.saved

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
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
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import app.magnetar.api.SavedSearch
import app.magnetar.api.SearchQuery
import app.magnetar.ui.common.Banner
import app.magnetar.ui.common.CenteredMessage
import app.magnetar.ui.common.categoryLabel
import app.magnetar.ui.common.facetLabel
import app.magnetar.ui.common.formatAgo
import app.magnetar.ui.common.formatBytes
import app.magnetar.ui.common.formatRating

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun SavedScreen(vm: SavedViewModel, onRun: () -> Unit, onOpenSettings: () -> Unit, notificationsOn: Boolean) {
    val state by vm.state.collectAsStateWithLifecycle()
    val snackbar = remember { SnackbarHostState() }
    var toDelete by remember { mutableStateOf<SavedSearch?>(null) }
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
        Column(Modifier.padding(padding).fillMaxSize()) {
            if (!state.loading && state.error == null && !state.canEdit) {
                Banner("Notifications and editing need a newer server (saved_search_poll).")
            } else if (state.canEdit && !notificationsOn) {
                Banner("Saved-search notifications are off in Settings.")
            }
            Box(Modifier.fillMaxSize()) {
                when {
                    state.loading && state.searches.isEmpty() ->
                        Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) { CircularProgressIndicator() }
                    state.notConfigured -> CenteredMessage("Set your server in Settings", "Settings", onAction = onOpenSettings)
                    state.error != null -> CenteredMessage(state.error!!, "Retry", onAction = vm::refresh)
                    state.searches.isEmpty() ->
                        CenteredMessage("No saved searches. Use the star on the Search tab to save one.")
                    else -> LazyColumn(Modifier.testTag("saved_list")) {
                        items(state.searches, key = { it.id }) { s ->
                            ListItem(
                                modifier = Modifier.clickable { vm.run(s); onRun() }.testTag("saved_item"),
                                headlineContent = { Text(s.name) },
                                supportingContent = {
                                    Text(describe(s.query) + (s.lastChecked?.let { "\nchecked ${formatAgo(it)}" } ?: ""))
                                },
                                trailingContent = {
                                    Row(verticalAlignment = Alignment.CenterVertically) {
                                        Switch(
                                            checked = s.notify,
                                            onCheckedChange = { vm.setNotify(s, it) },
                                            enabled = state.canEdit,
                                            modifier = Modifier.semantics { contentDescription = "Notify for ${s.name}" },
                                        )
                                        IconButton(onClick = { toDelete = s }) { Icon(Icons.Filled.Delete, "Delete ${s.name}") }
                                    }
                                },
                            )
                            HorizontalDivider()
                        }
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

/** One line of what a saved query asks: "\"dune\" · Video · 2160p · ★ ≥ 7.0". */
fun describe(q: SearchQuery): String = buildList {
    add(if (q.text.isBlank()) "any text" else "\"${q.text}\"")
    if (q.categories.isNotEmpty()) add(q.categories.joinToString("/") { categoryLabel(it) })
    if (q.origins.isNotEmpty()) add(q.origins.joinToString("/") { facetLabel("origin", it) })
    q.minSize?.let { add("≥ ${formatBytes(it)}") }
    q.maxSize?.let { add("≤ ${formatBytes(it)}") }
    q.minSeeders?.let { add("≥ $it seeders") }
    if (q.hideDead) add("alive only")
    q.minTrust?.let { add("trust ≥ $it") }
    val lists = listOf(
        "language" to q.languages, "subtitles" to q.subtitles, "resolution" to q.resolutions,
        "source" to q.sources, "codec" to q.videoCodecs, "hdr" to q.hdr, "audio" to q.audio, "genre" to q.genres,
        "media_kind" to q.mediaKinds,
    )
    for ((field, values) in lists) {
        if (values.isNotEmpty()) add(values.joinToString("/") { facetLabel(field, it) })
    }
    q.minRating?.let { add(formatRating(it).replace("★ ", "★ ≥ ")) }
    when {
        q.minYear != null && q.maxYear != null -> add("${q.minYear}–${q.maxYear}")
        q.minYear != null -> add("≥ ${q.minYear}")
        q.maxYear != null -> add("≤ ${q.maxYear}")
    }
}.joinToString(" · ")

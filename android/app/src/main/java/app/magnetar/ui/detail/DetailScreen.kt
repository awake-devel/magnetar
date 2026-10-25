package app.magnetar.ui.detail

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyListScope
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.automirrored.filled.KeyboardArrowRight
import androidx.compose.material.icons.filled.KeyboardArrowDown
import androidx.compose.material.icons.filled.MoreVert
import androidx.compose.material.icons.filled.Refresh
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.AssistChip
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilledTonalButton
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import app.magnetar.ffi.FfiLiveness
import app.magnetar.ffi.FfiTorrentDetail
import app.magnetar.ffi.FfiTrust
import app.magnetar.formatAge
import app.magnetar.formatBytes
import app.magnetar.formatTimestamp
import app.magnetar.stripHtml
import app.magnetar.ui.common.AppIcons
import app.magnetar.ui.common.CategoryIcon
import app.magnetar.ui.common.CenteredMessage
import app.magnetar.ui.common.SaveSearchDialog
import app.magnetar.ui.common.SectionTitle
import app.magnetar.ui.common.TrustBadge
import app.magnetar.ui.common.copyToClipboard
import app.magnetar.ui.common.openMagnetExternally
import app.magnetar.ui.common.shortKey
import app.magnetar.ui.search.peersText
import kotlinx.coroutines.launch

private const val MAX_FILE_ROWS = 300

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun DetailScreen(onBack: () -> Unit, vm: DetailViewModel = viewModel()) {
    val state by vm.state.collectAsStateWithLifecycle()
    val settings by vm.settings.collectAsStateWithLifecycle()
    val context = LocalContext.current
    val snackbar = remember { SnackbarHostState() }
    val scope = rememberCoroutineScope()
    var menu by remember { mutableStateOf(false) }
    var confirmBlock by remember { mutableStateOf(false) }
    var showSave by remember { mutableStateOf(false) }

    fun copy(label: String, text: String) {
        copyToClipboard(context, label, text)
        scope.launch { snackbar.showSnackbar("$label copied") }
    }

    fun openMagnet(magnet: String) {
        if (!openMagnetExternally(context, magnet)) {
            scope.launch { snackbar.showSnackbar("No torrent app installed. Magnet copied to clipboard.") }
        }
    }

    LaunchedEffect(vm) { vm.messages.collect { snackbar.showSnackbar(it) } }
    LaunchedEffect(vm) { vm.externalOpen.collect { openMagnet(it) } }

    val detail = state.detail
    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text("Torrent") },
                navigationIcon = {
                    IconButton(onClick = onBack) { Icon(Icons.AutoMirrored.Filled.ArrowBack, "Back") }
                },
                actions = {
                    IconButton(onClick = vm::refresh) { Icon(Icons.Filled.Refresh, "Refresh") }
                    Box {
                        IconButton(onClick = { menu = true }) { Icon(Icons.Filled.MoreVert, "More") }
                        DropdownMenu(expanded = menu, onDismissRequest = { menu = false }) {
                            if (detail != null) {
                                DropdownMenuItem(
                                    text = { Text(if (detail.summary.pinned) "Unpin" else "Pin") },
                                    onClick = { menu = false; vm.togglePinned() },
                                )
                                DropdownMenuItem(
                                    text = { Text(if (detail.summary.hidden) "Unhide" else "Hide") },
                                    onClick = { menu = false; vm.toggleHidden() },
                                )
                                DropdownMenuItem(
                                    text = { Text("Block publisher…") },
                                    onClick = { menu = false; confirmBlock = true },
                                )
                            }
                            DropdownMenuItem(
                                text = { Text("Save search…") },
                                onClick = { menu = false; showSave = true },
                            )
                        }
                    }
                },
            )
        },
        snackbarHost = { SnackbarHost(snackbar) },
    ) { padding ->
        Box(Modifier.padding(padding).fillMaxSize()) {
            when {
                state.loading && detail == null ->
                    Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) { CircularProgressIndicator() }
                detail == null -> NotCached(
                    infohash = vm.infohash,
                    error = state.error,
                    onRetry = vm::refresh,
                    onOpen = { openMagnet(vm.magnet) },
                    onCopy = { copy("Magnet", vm.magnet) },
                    onDownload = vm::downloadOnPhone,
                )
                else -> DetailBody(
                    detail = detail,
                    threshold = settings.trustThreshold,
                    hasServer = settings.hasServer,
                    checkingLiveness = state.checkingLiveness,
                    onCopy = ::copy,
                    onOpen = { openMagnet(vm.magnet) },
                    onDownloadPhone = vm::downloadOnPhone,
                    onDownloadServer = vm::downloadOnServer,
                    onCheckLiveness = vm::checkLiveness,
                )
            }
        }
    }

    if (confirmBlock && detail != null) {
        AlertDialog(
            onDismissRequest = { confirmBlock = false },
            title = { Text("Block publisher?") },
            text = {
                Text(
                    "Everything published by ${shortKey(detail.summary.npub)} will be hidden, here and on " +
                        "the server when it is reachable.",
                )
            },
            confirmButton = {
                TextButton(onClick = { confirmBlock = false; vm.blockPublisher() }) { Text("Block") }
            },
            dismissButton = { TextButton(onClick = { confirmBlock = false }) { Text("Cancel") } },
        )
    }
    if (showSave) {
        SaveSearchDialog(
            initialName = vm.suggestedSearchName(),
            onDismiss = { showSave = false },
            onSave = { name, notify -> showSave = false; vm.saveSearch(name, notify) },
        )
    }
}

@Composable
private fun NotCached(
    infohash: String,
    error: String?,
    onRetry: () -> Unit,
    onOpen: () -> Unit,
    onCopy: () -> Unit,
    onDownload: () -> Unit,
) {
    Column(Modifier.fillMaxSize().padding(24.dp), verticalArrangement = Arrangement.Center) {
        Text(error ?: "This torrent is not in the cache yet.", style = MaterialTheme.typography.bodyLarge)
        Spacer(Modifier.padding(4.dp))
        Text(infohash, fontFamily = FontFamily.Monospace, style = MaterialTheme.typography.bodySmall)
        Spacer(Modifier.padding(8.dp))
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            OutlinedButton(onClick = onRetry) { Text("Retry") }
            OutlinedButton(onClick = onCopy) { Text("Copy magnet") }
        }
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            Button(onClick = onDownload) { Text("Download") }
            OutlinedButton(onClick = onOpen) { Text("Open in…") }
        }
    }
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun DetailBody(
    detail: FfiTorrentDetail,
    threshold: Int,
    hasServer: Boolean,
    checkingLiveness: Boolean,
    onCopy: (String, String) -> Unit,
    onOpen: () -> Unit,
    onDownloadPhone: () -> Unit,
    onDownloadServer: () -> Unit,
    onCheckLiveness: () -> Unit,
) {
    val s = detail.summary
    val tree = remember(detail.files) { buildFileTree(detail.files) }
    var expanded by remember(detail.files) {
        // Small torrents open fully; big ones start collapsed.
        mutableStateOf(if (detail.files.size <= 30) allDirs(tree) else emptySet())
    }
    var trustOpen by remember { mutableStateOf(false) }

    LazyColumn(Modifier.fillMaxSize().padding(horizontal = 16.dp)) {
        item {
            Row(verticalAlignment = Alignment.CenterVertically) {
                CategoryIcon(s.category)
                Spacer(Modifier.width(8.dp))
                SelectionContainer(Modifier.weight(1f)) {
                    Text(s.title, style = MaterialTheme.typography.titleLarge)
                }
            }
            Text(
                listOfNotNull(
                    formatBytes(s.totalSize),
                    "${s.fileCount} files",
                    peersText(s.seeders, s.leechers),
                    if (s.dead == true) "dead" else null,
                    formatTimestamp(s.createdAt),
                    detail.source,
                ).joinToString(" · "),
                style = MaterialTheme.typography.bodyMedium,
            )
            if (detail.categoryPath.isNotEmpty()) {
                Text(detail.categoryPath.joinToString(" › "), style = MaterialTheme.typography.bodySmall)
            }
            FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp), modifier = Modifier.padding(top = 8.dp)) {
                Button(onClick = onOpen) { Text("Open magnet") }
                OutlinedButton(onClick = { onCopy("Magnet", detail.magnet) }) {
                    Icon(AppIcons.ContentCopy, null, Modifier.size(18.dp))
                    Spacer(Modifier.width(4.dp))
                    Text("Copy")
                }
                FilledTonalButton(onClick = onDownloadPhone) {
                    Icon(AppIcons.Download, null, Modifier.size(18.dp))
                    Spacer(Modifier.width(4.dp))
                    Text("On phone")
                }
                if (hasServer) {
                    FilledTonalButton(onClick = onDownloadServer) {
                        Icon(AppIcons.Download, null, Modifier.size(18.dp))
                        Spacer(Modifier.width(4.dp))
                        Text("On server")
                    }
                }
            }
            if (s.pinned || s.hidden) {
                Text(
                    listOfNotNull(if (s.pinned) "📌 pinned" else null, if (s.hidden) "hidden" else null).joinToString(" · "),
                    style = MaterialTheme.typography.labelMedium,
                )
            }
        }

        item {
            SectionTitle("Infohash")
            CopyRow(s.infohash, monospace = true) { onCopy("Infohash", s.infohash) }
            SectionTitle("Publisher")
            CopyRow(s.npub, monospace = true) { onCopy("npub", s.npub) }
            if (s.publisherCount > 1u) Text("${s.publisherCount} publishers announced this torrent")
            if (s.powBits > 0u) Text("Proof of work: ${s.powBits} bits", style = MaterialTheme.typography.bodySmall)
        }

        item { TrustCard(detail.trust, threshold, trustOpen) { trustOpen = !trustOpen } }

        item { LivenessCard(detail.liveness, checkingLiveness, onCheckLiveness) }

        if (detail.files.isNotEmpty()) {
            item { SectionTitle("Files (${detail.files.size})") }
            val rows = flattenTree(tree, expanded)
            items(rows.take(MAX_FILE_ROWS), key = { "f:" + it.node.path }) { row ->
                FileRowView(row) {
                    expanded = if (row.expanded) expanded - row.node.path else expanded + row.node.path
                }
            }
            if (rows.size > MAX_FILE_ROWS) item { Text("… ${rows.size - MAX_FILE_ROWS} more") }
        }

        listSection("Trackers", detail.trackers) { Text(it, style = MaterialTheme.typography.bodySmall, fontFamily = FontFamily.Monospace) }

        if (detail.hashtags.isNotEmpty() || detail.externalIds.isNotEmpty()) {
            item {
                SectionTitle("Tags")
                FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    detail.hashtags.forEach { AssistChip(onClick = {}, label = { Text("#$it") }) }
                    detail.externalIds.forEach { kv ->
                        AssistChip(onClick = { onCopy(kv.key, kv.value) }, label = { Text("${kv.key}: ${kv.value}") })
                    }
                }
            }
        }

        val description = stripHtml(detail.description)
        if (description.isNotBlank()) {
            item {
                SectionTitle("Description")
                SelectionContainer { Text(description, style = MaterialTheme.typography.bodyMedium) }
            }
        }

        if (detail.comments.isNotEmpty()) {
            item { SectionTitle("Comments (${detail.comments.size})") }
            items(detail.comments, key = { "c:" + it.eventId }) { c ->
                Column(Modifier.padding(vertical = 6.dp)) {
                    Text("${shortKey(c.npub)} · ${formatAge(c.createdAt)}", style = MaterialTheme.typography.labelMedium)
                    Text(stripHtml(c.content), style = MaterialTheme.typography.bodyMedium)
                }
                HorizontalDivider()
            }
        }

        val others = detail.events.filter { it.eventId != s.eventId }
        if (others.isNotEmpty()) {
            item { SectionTitle("Also published by") }
            items(others, key = { "e:" + it.eventId }) { e ->
                Row(Modifier.fillMaxWidth().padding(vertical = 6.dp), verticalAlignment = Alignment.CenterVertically) {
                    Column(Modifier.weight(1f)) {
                        Text(e.title, style = MaterialTheme.typography.bodyMedium)
                        Text("${shortKey(e.npub)} · ${formatAge(e.createdAt)}", style = MaterialTheme.typography.labelSmall)
                    }
                    TrustBadge(e.trust.toInt(), threshold)
                }
            }
        }
        item { Spacer(Modifier.padding(24.dp)) }
    }
}

private fun allDirs(root: FileNode): Set<String> {
    val out = mutableSetOf<String>()
    fun walk(n: FileNode) {
        for (c in n.children.values) {
            if (!c.isFile) {
                out += c.path
                walk(c)
            }
        }
    }
    walk(root)
    return out
}

private fun <T> LazyListScope.listSection(title: String, list: List<T>, row: @Composable (T) -> Unit) {
    if (list.isEmpty()) return
    item {
        SectionTitle(title)
        list.forEach { row(it) }
    }
}

@Composable
private fun CopyRow(text: String, monospace: Boolean, onCopy: () -> Unit) {
    Row(verticalAlignment = Alignment.CenterVertically) {
        Text(
            text,
            Modifier.weight(1f),
            fontFamily = if (monospace) FontFamily.Monospace else FontFamily.Default,
            style = MaterialTheme.typography.bodySmall,
        )
        IconButton(onClick = onCopy) { Icon(AppIcons.ContentCopy, "Copy") }
    }
}

@Composable
private fun TrustCard(trust: FfiTrust, threshold: Int, open: Boolean, onToggle: () -> Unit) {
    Card(Modifier.fillMaxWidth().padding(top = 12.dp).clickable(onClick = onToggle)) {
        Column(Modifier.padding(12.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text("Trust", style = MaterialTheme.typography.titleSmall, modifier = Modifier.weight(1f))
                TrustBadge(trust.score.toInt(), threshold)
                Icon(if (open) Icons.Filled.KeyboardArrowDown else Icons.AutoMirrored.Filled.KeyboardArrowRight, null)
            }
            if (open) {
                trust.root?.let { Text("Relative to root ${shortKey(it)}", style = MaterialTheme.typography.bodySmall) }
                if (trust.factors.isEmpty()) Text("No factors recorded.", style = MaterialTheme.typography.bodySmall)
                for (f in trust.factors) {
                    Row(Modifier.padding(top = 6.dp)) {
                        Text(
                            (if (f.delta >= 0) "+" else "") + f.delta,
                            fontWeight = FontWeight.Bold,
                            color = if (f.delta >= 0) MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.error,
                            modifier = Modifier.width(44.dp),
                        )
                        Column {
                            Text(f.detail.ifBlank { f.code }, style = MaterialTheme.typography.bodyMedium)
                            Text(f.code, style = MaterialTheme.typography.labelSmall)
                            f.evidence.take(5).forEach {
                                Text("• " + shortKey(it), style = MaterialTheme.typography.labelSmall)
                            }
                        }
                    }
                }
                Text("Computed ${formatAge(trust.computedAt)} ago", style = MaterialTheme.typography.labelSmall)
            }
        }
    }
}

@Composable
private fun LivenessCard(l: FfiLiveness?, checking: Boolean, onCheck: () -> Unit) {
    Card(Modifier.fillMaxWidth().padding(top = 12.dp)) {
        Column(Modifier.padding(12.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text("Swarm", style = MaterialTheme.typography.titleSmall, modifier = Modifier.weight(1f))
                TextButton(onClick = onCheck, enabled = !checking) { Text("Check now") }
            }
            if (checking) LinearProgressIndicator(Modifier.fillMaxWidth())
            if (l == null) {
                Text("Not checked yet", style = MaterialTheme.typography.bodyMedium)
            } else {
                Text(
                    "${l.seeders ?: "?"} seeders · ${l.leechers ?: "?"} leechers" +
                        (l.completed?.let { " · $it completed" } ?: ""),
                    style = MaterialTheme.typography.bodyLarge,
                )
                l.dhtPeersEst?.let { Text("≈$it DHT peers", style = MaterialTheme.typography.bodyMedium) }
                Text(
                    "via ${l.source} · trackers ${l.trackersOk} ok / ${l.trackersFailed} failed · " +
                        "checked ${formatAge(l.checkedAt)} ago",
                    style = MaterialTheme.typography.labelSmall,
                )
            }
        }
    }
}

@Composable
private fun FileRowView(row: TreeRow, onToggle: () -> Unit) {
    val n = row.node
    Row(
        Modifier
            .fillMaxWidth()
            .then(if (n.isFile) Modifier else Modifier.clickable(onClick = onToggle))
            .padding(start = (row.depth * 16).dp, top = 4.dp, bottom = 4.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Icon(if (n.isFile) AppIcons.File else AppIcons.Folder, null, Modifier.size(18.dp))
        Spacer(Modifier.width(6.dp))
        Text(
            n.name + if (!n.isFile) " (${n.fileCount})" else "",
            Modifier.weight(1f),
            style = MaterialTheme.typography.bodySmall,
        )
        Text(formatBytes(n.totalSize), style = MaterialTheme.typography.labelSmall)
    }
}

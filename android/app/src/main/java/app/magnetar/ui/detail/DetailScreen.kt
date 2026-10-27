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
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.automirrored.filled.KeyboardArrowRight
import androidx.compose.material.icons.filled.KeyboardArrowDown
import androidx.compose.material.icons.filled.Refresh
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
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
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import app.magnetar.api.DhtInfo
import app.magnetar.api.Liveness
import app.magnetar.api.LivenessHistory
import app.magnetar.api.Origin
import app.magnetar.api.TorrentDetail
import app.magnetar.api.TrustScore
import app.magnetar.ui.common.AppIcons
import app.magnetar.ui.common.Banner
import app.magnetar.ui.common.CategoryIcon
import app.magnetar.ui.common.CenteredMessage
import app.magnetar.ui.common.DhtChip
import app.magnetar.ui.common.SectionTitle
import app.magnetar.ui.common.TrustBadge
import app.magnetar.ui.common.attrDetails
import app.magnetar.ui.common.cachedAgo
import app.magnetar.ui.common.copyToClipboard
import app.magnetar.ui.common.formatAge
import app.magnetar.ui.common.formatAgo
import app.magnetar.ui.common.formatBytes
import app.magnetar.ui.common.formatTimestamp
import app.magnetar.ui.common.openMagnetExternally
import app.magnetar.ui.common.peersText
import app.magnetar.ui.common.shortKey
import app.magnetar.ui.common.sourceLabel
import app.magnetar.ui.common.stripHtml
import app.magnetar.ui.common.trustLevel
import kotlinx.coroutines.launch

private const val MAX_FILE_ROWS = 300

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun DetailScreen(
    vm: DetailViewModel,
    threshold: Int,
    onBack: () -> Unit,
    onOpenMedia: ((Long) -> Unit)?,
    onOpenGraph: ((String) -> Unit)? = null,
) {
    val state by vm.state.collectAsStateWithLifecycle()
    val context = LocalContext.current
    val snackbar = remember { SnackbarHostState() }
    val scope = rememberCoroutineScope()

    fun copy(label: String, text: String) {
        copyToClipboard(context, label, text)
        scope.launch { snackbar.showSnackbar("$label copied") }
    }

    fun openMagnet() {
        if (!openMagnetExternally(context, vm.magnet)) {
            scope.launch { snackbar.showSnackbar("No torrent app installed. Magnet copied to the clipboard.") }
        }
    }

    LaunchedEffect(vm) { vm.messages.collect { snackbar.showSnackbar(it) } }

    val detail = state.detail
    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text("Torrent") },
                navigationIcon = { IconButton(onClick = onBack) { Icon(Icons.AutoMirrored.Filled.ArrowBack, "Back") } },
                actions = { IconButton(onClick = vm::refresh) { Icon(Icons.Filled.Refresh, "Refresh") } },
            )
        },
        snackbarHost = { SnackbarHost(snackbar) },
    ) { padding ->
        Column(Modifier.padding(padding).fillMaxSize()) {
            if (state.loading && detail != null) LinearProgressIndicator(Modifier.fillMaxWidth())
            if (detail != null && state.error != null) {
                Banner(
                    (if (state.offline) "Offline: " else "") + state.error +
                        (state.cachedAtMs?.let { " · " + cachedAgo(it) } ?: ""),
                    error = true,
                )
            } else if (detail != null && state.cachedAtMs != null) {
                Banner(cachedAgo(state.cachedAtMs!!))
            }
            Box(Modifier.fillMaxSize()) {
                when {
                    detail == null && state.loading ->
                        Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) { CircularProgressIndicator() }
                    detail == null -> Column(Modifier.fillMaxSize()) {
                        CenteredMessage(state.error ?: "Not on this server", "Retry", Modifier.weight(1f), onAction = vm::refresh)
                        Row(Modifier.padding(24.dp), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                            OutlinedButton(onClick = { copy("Magnet", vm.magnet) }) { Text("Copy magnet") }
                            OutlinedButton(onClick = ::openMagnet) { Text("Open magnet") }
                        }
                    }
                    else -> DetailBody(
                        detail = detail,
                        threshold = threshold,
                        checking = state.checking,
                        history = state.history,
                        onCopy = ::copy,
                        onOpen = ::openMagnet,
                        onCheck = vm::checkNow,
                        onOpenMedia = onOpenMedia,
                        onOpenGraph = onOpenGraph,
                    )
                }
            }
        }
    }
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun DetailBody(
    detail: TorrentDetail,
    threshold: Int,
    checking: Boolean,
    history: LivenessHistory?,
    onCopy: (String, String) -> Unit,
    onOpen: () -> Unit,
    onCheck: () -> Unit,
    onOpenMedia: ((Long) -> Unit)?,
    onOpenGraph: ((String) -> Unit)?,
) {
    val s = detail.summary
    val tree = remember(detail.files) { buildFileTree(detail.files) }
    var expanded by remember(detail.files) {
        // Small torrents open fully; big ones start collapsed.
        mutableStateOf(if (detail.files.size <= 30) allDirs(tree) else emptySet())
    }
    var trustOpen by remember { mutableStateOf(false) }

    LazyColumn(Modifier.fillMaxSize().padding(horizontal = 16.dp).testTag("detail")) {
        item {
            Row(verticalAlignment = Alignment.CenterVertically) {
                CategoryIcon(s.category)
                Spacer(Modifier.width(8.dp))
                SelectionContainer(Modifier.weight(1f)) {
                    Text(s.title, style = MaterialTheme.typography.titleLarge, modifier = Modifier.testTag("detail_title"))
                }
            }
            Text(
                listOfNotNull(
                    formatBytes(s.totalSize),
                    if (s.fileCount == 1) "1 file" else "${s.fileCount} files",
                    peersText(s.seeders, s.leechers, s.bestPeers),
                    if (s.dead == true) "dead" else null,
                    formatTimestamp(s.createdAt),
                ).joinToString(" · "),
                style = MaterialTheme.typography.bodyMedium,
            )
            if (detail.categoryPath.isNotEmpty()) {
                Text(detail.categoryPath.joinToString(" › "), style = MaterialTheme.typography.bodySmall)
            }
            // What the name and files say about the release (D44).
            s.attrs?.let(::attrDetails)?.takeIf { it.isNotEmpty() }?.let { line ->
                Text(
                    line.joinToString(" · "),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.secondary,
                    modifier = Modifier.testTag("detail_attrs"),
                )
            }
            // No download buttons (D79): the magnet goes to a torrent app or the clipboard.
            FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp), modifier = Modifier.padding(top = 8.dp)) {
                Button(onClick = onOpen, modifier = Modifier.testTag("open_magnet")) { Text("Open magnet") }
                OutlinedButton(onClick = { onCopy("Magnet", detail.magnet.ifBlank { "magnet:?xt=urn:btih:${s.infohash}" }) }) {
                    Icon(AppIcons.ContentCopy, null, Modifier.size(18.dp))
                    Spacer(Modifier.width(4.dp))
                    Text("Copy magnet")
                }
            }
        }

        detail.media?.let { m -> item { MediaCard(m, onOpenMedia) } }

        item {
            SwarmCard(detail.liveness, s.bestPeers, checking, history, onCheck, onGraph = onOpenGraph?.let { open -> { open("swarm/${s.infohash}") } })
        }

        // DHT-only: no publisher, so no trust score; the card says why it is listed (D74).
        detail.dht?.let { d -> item { DhtCard(d, s.title) } }
        if (s.origin != Origin.DHT) {
            item { TrustCard(detail.trust, threshold, trustOpen) { trustOpen = !trustOpen } }
        }

        item {
            SectionTitle("Infohash")
            CopyRow(s.infohash) { onCopy("Infohash", s.infohash) }
            SectionTitle("Source")
            Text(sourceLabel(s.origin), style = MaterialTheme.typography.bodyMedium)
            if (s.origin != Origin.DHT && s.npub.isNotEmpty()) {
                SectionTitle("Publisher")
                CopyRow(s.npub) { onCopy("npub", s.npub) }
                if (s.publisherCount > 1) Text("${s.publisherCount} publishers announced this torrent")
                if (s.powBits > 0) Text("Proof of work: ${s.powBits} bits", style = MaterialTheme.typography.bodySmall)
            }
        }

        if (detail.files.isNotEmpty()) {
            item { SectionTitle("Files (${detail.files.size})") }
            val rows = flattenTree(tree, expanded)
            items(rows.take(MAX_FILE_ROWS), key = { "f:" + it.node.path + if (it.node.isFile) "" else "/" }) { row ->
                FileRowView(row) {
                    expanded = if (row.expanded) expanded - row.node.path else expanded + row.node.path
                }
            }
            if (rows.size > MAX_FILE_ROWS) item { Text("… ${rows.size - MAX_FILE_ROWS} more") }
        }

        if (detail.trackers.isNotEmpty()) {
            item {
                SectionTitle("Trackers (${detail.trackers.size})")
                detail.trackers.take(50).forEach {
                    Text(it, style = MaterialTheme.typography.bodySmall, fontFamily = FontFamily.Monospace)
                }
            }
        }

        if (detail.hashtags.isNotEmpty() || detail.externalIds.isNotEmpty()) {
            item {
                SectionTitle("Tags")
                FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    detail.hashtags.forEach { Text("#$it", style = MaterialTheme.typography.bodySmall) }
                    detail.externalIds.filter { it.size == 2 }.forEach { (k, v) ->
                        Text("$k: $v", style = MaterialTheme.typography.bodySmall, modifier = Modifier.clickable { onCopy(k, v) })
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
                    Text("${shortKey(c.pubkey)} · ${formatAge(c.createdAt)}", style = MaterialTheme.typography.labelMedium)
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
                    TrustBadge(e.trust, threshold)
                }
            }
        }
        item { Spacer(Modifier.padding(24.dp)) }
    }
}

@Composable
private fun CopyRow(text: String, onCopy: () -> Unit) {
    Row(verticalAlignment = Alignment.CenterVertically) {
        Text(text, Modifier.weight(1f), fontFamily = FontFamily.Monospace, style = MaterialTheme.typography.bodySmall)
        IconButton(onClick = onCopy) { Icon(AppIcons.ContentCopy, "Copy") }
    }
}

/** What the server's DHT crawler verified (D51). */
@Composable
private fun DhtCard(d: DhtInfo, shownTitle: String) {
    Card(Modifier.fillMaxWidth().padding(top = 12.dp)) {
        Column(Modifier.padding(12.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text("Found on the DHT", style = MaterialTheme.typography.titleSmall, modifier = Modifier.weight(1f))
                DhtChip()
            }
            Text(
                "Name and files were downloaded from the swarm and verified against the infohash; " +
                    "no publisher stands behind them.",
                style = MaterialTheme.typography.bodySmall,
            )
            if (d.title.isNotEmpty() && d.title != shownTitle) Text("Torrent name: ${d.title}", style = MaterialTheme.typography.bodySmall)
            Text(
                "First seen ${formatTimestamp(d.firstSeen)} · ${d.peersSeen} peers then · via ${d.discoveredVia.replace('_', ' ')}",
                style = MaterialTheme.typography.labelSmall,
            )
            d.infohashV2?.let { Text("BitTorrent v2 hash: ${shortKey(it)}", style = MaterialTheme.typography.labelSmall) }
        }
    }
}

@Composable
private fun TrustCard(trust: TrustScore, threshold: Int, open: Boolean, onToggle: () -> Unit) {
    Card(Modifier.fillMaxWidth().padding(top = 12.dp).clickable(onClick = onToggle).testTag("trust_card")) {
        Column(Modifier.padding(12.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text("Trust", style = MaterialTheme.typography.titleSmall, modifier = Modifier.weight(1f))
                TrustBadge(
                    trust.score,
                    threshold,
                    level = trustLevel(trust.score, threshold, trust.factors.map { it.code }, trust.factors.any { it.delta < 0 }),
                )
                Icon(
                    if (open) Icons.Filled.KeyboardArrowDown else Icons.AutoMirrored.Filled.KeyboardArrowRight,
                    if (open) "Hide trust factors" else "Show trust factors",
                )
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
                            f.evidence.take(5).forEach { Text("• " + shortKey(it), style = MaterialTheme.typography.labelSmall) }
                        }
                    }
                }
                if (trust.computedAt > 0) Text("Computed ${formatAgo(trust.computedAt)}", style = MaterialTheme.typography.labelSmall)
            }
        }
    }
}

/** Swarm: the latest check, *Check now*, and the seeder history (D45). */
@Composable
private fun SwarmCard(
    l: Liveness?,
    bestPeers: Int?,
    checking: Boolean,
    history: LivenessHistory?,
    onCheck: () -> Unit,
    onGraph: (() -> Unit)?,
) {
    Card(Modifier.fillMaxWidth().padding(top = 12.dp).testTag("swarm_card")) {
        Column(Modifier.padding(12.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text("Swarm", style = MaterialTheme.typography.titleSmall, modifier = Modifier.weight(1f))
                TextButton(onClick = onCheck, enabled = !checking, modifier = Modifier.testTag("check_now")) {
                    Text(if (checking) "Checking…" else "Check now")
                }
            }
            if (checking) LinearProgressIndicator(Modifier.fillMaxWidth())
            if (l == null) {
                Text(bestPeers?.let { "≈$it peers (last check)" } ?: "Not checked yet", style = MaterialTheme.typography.bodyMedium)
            } else {
                val status = when (l.bestPeers) {
                    null -> "unknown"
                    0 -> "dead"
                    else -> "seeded"
                }
                Text(
                    "${l.seeders ?: "?"} seeders · ${l.leechers ?: "?"} leechers" + (l.completed?.let { " · $it completed" } ?: ""),
                    style = MaterialTheme.typography.bodyLarge,
                    modifier = Modifier.semantics { contentDescription = "Swarm $status: ${l.seeders ?: "unknown"} seeders" },
                )
                l.dhtPeersEst?.let { Text("≈$it DHT peers", style = MaterialTheme.typography.bodyMedium) }
                Text(
                    "$status · via ${l.source.replace('_', ' ')} · trackers ${l.trackersOk} ok / ${l.trackersFailed} failed · " +
                        "checked ${formatAgo(l.checkedAt)}",
                    style = MaterialTheme.typography.labelSmall,
                )
            }
            history?.let { h ->
                Text("Seeders, last ${h.keepDays.takeIf { it > 0 } ?: 90} days", style = MaterialTheme.typography.labelSmall, modifier = Modifier.padding(top = 8.dp))
                SeederHistoryChart(h.points)
            }
            onGraph?.let {
                TextButton(onClick = it, modifier = Modifier.testTag("swarm_graph")) {
                    Icon(AppIcons.Graph, null, Modifier.size(18.dp))
                    Spacer(Modifier.width(4.dp))
                    Text("Swarm graph")
                }
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
            .padding(start = (row.depth * 16).dp, top = 6.dp, bottom = 6.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Icon(if (n.isFile) AppIcons.File else AppIcons.Folder, if (n.isFile) "File" else "Folder", Modifier.size(18.dp))
        Spacer(Modifier.width(6.dp))
        Text(n.name + if (!n.isFile) " (${n.fileCount})" else "", Modifier.weight(1f), style = MaterialTheme.typography.bodySmall)
        Text(formatBytes(n.totalSize), style = MaterialTheme.typography.labelSmall)
    }
}

package app.magnetar.ui.graph

import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.gestures.detectTransformGestures
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.Refresh
import androidx.compose.material3.Card
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
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
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.graphics.drawscope.withTransform
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.layout.onSizeChanged
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.TextLayoutResult
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.drawText
import androidx.compose.ui.text.rememberTextMeasurer
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.IntSize
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import app.magnetar.ffi.FfiGraph
import app.magnetar.ffi.FfiGraphNode
import app.magnetar.ui.common.Banner
import app.magnetar.ui.common.CenteredMessage
import app.magnetar.ui.common.copyToClipboard
import app.magnetar.ui.theme.ChartColors
import app.magnetar.ui.theme.chartColors
import kotlin.math.max
import kotlin.math.min
import kotlin.math.sqrt

/** Colour slot per node kind, the same in every graph (a kind never changes colour). */
private val KIND_SLOT = mapOf(
    "root" to 0, "media" to 0, "torrent" to 0,
    "publisher" to 1, "release" to 1,
    "follow" to 2, "language" to 2, "tracker" to 2,
    "quality" to 3, "season" to 4, "relay" to 5, "group" to 6, "dht" to 6,
)

private val KIND_LABEL = mapOf(
    "root" to "You (trust root)", "follow" to "Followed by you", "publisher" to "Publisher",
    "relay" to "Relay", "media" to "Title", "quality" to "Quality", "release" to "Release",
    "language" to "Language", "group" to "Release group", "season" to "Season",
    "torrent" to "Torrent", "tracker" to "Tracker", "dht" to "DHT",
)

fun graphTitle(path: String): String = when {
    path == "wot" -> "Web of trust"
    path == "relays" -> "Relays and publishers"
    path.startsWith("media/") -> "Releases of this title"
    path.startsWith("swarm/") -> "Swarm sources"
    else -> "Graph"
}

private fun ChartColors.kindColor(kind: String): Color = categorical[(KIND_SLOT[kind] ?: 7) % categorical.size]

/**
 * One D47 graph, force-laid-out off the main thread and drawn on a Canvas: pinch to zoom, drag
 * to pan, tap a node for its details (and to open a torrent, a title, or copy a key/URL).
 */
@OptIn(ExperimentalMaterial3Api::class, ExperimentalLayoutApi::class)
@Composable
fun GraphScreen(
    onBack: () -> Unit,
    onOpenTorrent: (String) -> Unit,
    onOpenMedia: (Long) -> Unit,
    vm: GraphViewModel = viewModel(),
) {
    val state by vm.state.collectAsStateWithLifecycle()
    val snackbar = remember { SnackbarHostState() }
    val colors = chartColors()
    val context = LocalContext.current
    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text(graphTitle(vm.path)) },
                navigationIcon = {
                    IconButton(onClick = onBack) { Icon(Icons.AutoMirrored.Filled.ArrowBack, "Back") }
                },
                actions = { IconButton(onClick = vm::refresh) { Icon(Icons.Filled.Refresh, "Reload") } },
            )
        },
        snackbarHost = { SnackbarHost(snackbar) },
    ) { padding ->
        Column(Modifier.padding(padding).fillMaxSize()) {
            val g = state.graph
            when {
                !vm.hasServer -> CenteredMessage("Graphs are drawn from the server's data. Set a server in Settings to see them.")
                state.loading -> Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) { CircularProgressIndicator() }
                state.error != null -> CenteredMessage("Could not load the graph: ${state.error}", "Retry", vm::refresh)
                state.missing -> CenteredMessage("The server has no such graph (unknown item, or a server older than this app).")
                g == null -> {}
                g.nodes.isEmpty() -> CenteredMessage(g.note ?: "Nothing to show yet.")
                else -> {
                    g.note?.let { Banner(it) }
                    if (g.truncated) Banner("Only the ${g.nodes.size} most important nodes are shown.")
                    Legend(g, colors)
                    Box(Modifier.weight(1f).fillMaxWidth()) {
                        GraphCanvas(g, state, colors, onSelect = vm::select)
                        if (!state.settled) {
                            Text(
                                "Laying out…",
                                Modifier.align(Alignment.TopEnd).padding(8.dp),
                                style = MaterialTheme.typography.labelSmall,
                                color = colors.inkMuted,
                            )
                        }
                    }
                    state.selected?.let { i ->
                        g.nodes.getOrNull(i)?.let { n ->
                            NodeCard(
                                n = n,
                                neighbours = g.edges.count { it.source.toInt() == i || it.target.toInt() == i },
                                colors = colors,
                                onClose = { vm.select(null) },
                                onOpenTorrent = onOpenTorrent,
                                onOpenMedia = onOpenMedia,
                                onCopy = { label, text -> copyToClipboard(context, label, text) },
                                npub = vm::npub,
                            )
                        }
                    }
                }
            }
        }
    }
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun Legend(g: FfiGraph, colors: ChartColors) {
    val kinds = remember(g) { g.nodes.map { it.kind }.distinct() }
    FlowRow(
        Modifier.fillMaxWidth().padding(horizontal = 12.dp, vertical = 4.dp),
        horizontalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        for (k in kinds) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Box(Modifier.size(10.dp).background(colors.kindColor(k), CircleShape))
                Spacer(Modifier.width(4.dp))
                Text(KIND_LABEL[k] ?: k, style = MaterialTheme.typography.labelSmall)
            }
        }
    }
}

@Composable
private fun GraphCanvas(g: FfiGraph, state: GraphUiState, colors: ChartColors, onSelect: (Int?) -> Unit) {
    val density = LocalDensity.current
    val measurer = rememberTextMeasurer(cacheSize = 256)
    var size by remember { mutableStateOf(IntSize.Zero) }
    // Pan/zoom state; until the user moves the view, it follows the layout to keep it fitted.
    var scale by remember(g) { mutableFloatStateOf(1f) }
    var ox by remember(g) { mutableFloatStateOf(0f) }
    var oy by remember(g) { mutableFloatStateOf(0f) }
    var userMoved by remember(g) { mutableStateOf(false) }
    val x = state.x
    val y = state.y
    val n = min(g.nodes.size, x.size)
    val maxW = remember(g) { g.nodes.maxOfOrNull { it.weight }?.coerceAtLeast(1.0) ?: 1.0 }
    val minR = with(density) { 5.dp.toPx() }
    val spanR = with(density) { 13.dp.toPx() }
    val radius = remember(g, minR) {
        FloatArray(g.nodes.size) { i ->
            val w = sqrt((g.nodes[i].weight.coerceAtLeast(0.0) / maxW).toFloat())
            val r = minR + spanR * w
            if (i == 0 && g.nodes[0].kind in GraphViewModel.SUBJECT_KINDS) max(r, minR * 2.4f) else r
        }
    }
    val neighbours = remember(g) {
        Array(g.nodes.size) { mutableSetOf<Int>() }.also { a ->
            for (e in g.edges) {
                val s = e.source.toInt()
                val t = e.target.toInt()
                if (s in a.indices && t in a.indices) { a[s] += t; a[t] += s }
            }
        }
    }
    val maxEdge = remember(g) { g.edges.maxOfOrNull { it.weight }?.coerceAtLeast(1.0) ?: 1.0 }
    val labels = remember(g) { HashMap<Int, TextLayoutResult>() }
    val labelStyle = TextStyle(color = colors.ink, fontSize = 11.sp)

    LaunchedEffect(x, size, userMoved) {
        if (userMoved || n == 0 || size == IntSize.Zero) return@LaunchedEffect
        var minX = Float.MAX_VALUE; var maxX = -Float.MAX_VALUE
        var minY = Float.MAX_VALUE; var maxY = -Float.MAX_VALUE
        for (i in 0 until n) {
            minX = min(minX, x[i]); maxX = max(maxX, x[i])
            minY = min(minY, y[i]); maxY = max(maxY, y[i])
        }
        val pad = spanR * 2
        val s = min(
            (size.width - 2 * pad) / (maxX - minX).coerceAtLeast(1f),
            (size.height - 2 * pad) / (maxY - minY).coerceAtLeast(1f),
        ).coerceIn(0.05f, 6f)
        scale = s
        ox = size.width / 2f - (minX + maxX) / 2f * s
        oy = size.height / 2f - (minY + maxY) / 2f * s
    }

    Canvas(
        Modifier
            .fillMaxSize()
            .onSizeChanged { size = it }
            .semantics {
                contentDescription = "${g.nodes.size} nodes, ${g.edges.size} links. Tap a node for details."
            }
            .pointerInput(g) {
                detectTransformGestures { centroid, pan, zoom, _ ->
                    userMoved = true
                    val ns = (scale * zoom).coerceIn(0.05f, 20f)
                    val f = ns / scale
                    ox = centroid.x - (centroid.x - ox) * f + pan.x
                    oy = centroid.y - (centroid.y - oy) * f + pan.y
                    scale = ns
                }
            }
            .pointerInput(g, x) {
                detectTapGestures { p ->
                    // Nearest node within its radius (at least a 24 dp touch target).
                    val touch = 12.dp.toPx()
                    var best: Int? = null
                    var bestD = Float.MAX_VALUE
                    for (i in 0 until min(g.nodes.size, x.size)) {
                        val sx = x[i] * scale + ox
                        val sy = y[i] * scale + oy
                        val d = (sx - p.x) * (sx - p.x) + (sy - p.y) * (sy - p.y)
                        val r = max(radius[i], touch)
                        if (d < r * r && d < bestD) { best = i; bestD = d }
                    }
                    onSelect(best)
                }
            },
    ) {
        val sel = state.selected
        val near = sel?.let { neighbours.getOrNull(it) }
        withTransform({ translate(ox, oy) }) {
            for (e in g.edges) {
                val a = e.source.toInt()
                val b = e.target.toInt()
                if (a >= n || b >= n) continue
                val lit = sel != null && (a == sel || b == sel)
                val w = 1f + 2f * (e.weight / maxEdge).toFloat()
                drawLine(
                    color = if (lit) colors.ink else colors.edge.copy(alpha = if (sel == null) 0.35f else 0.12f),
                    start = Offset(x[a] * scale, y[a] * scale),
                    end = Offset(x[b] * scale, y[b] * scale),
                    strokeWidth = if (lit) w + 1f else w,
                )
            }
            for (i in 0 until n) {
                val c = Offset(x[i] * scale, y[i] * scale)
                val node = g.nodes[i]
                val dim = sel != null && i != sel && near?.contains(i) != true
                val color = colors.kindColor(node.kind).let { if (dim) it.copy(alpha = 0.3f) else it }
                val r = radius[i]
                drawCircle(color, r, c)
                // Trust score as a ring: the share of the circle is the score.
                node.score?.let { s ->
                    drawArc(
                        colors.ink.copy(alpha = if (dim) 0.2f else 0.8f),
                        startAngle = -90f,
                        sweepAngle = 360f * s.toInt() / 100f,
                        useCenter = false,
                        topLeft = Offset(c.x - r - 3f, c.y - r - 3f),
                        size = Size((r + 3f) * 2, (r + 3f) * 2),
                        style = Stroke(width = 2.5f),
                    )
                }
                if (i == sel) drawCircle(colors.ink, r + 6f, c, style = Stroke(width = 3f))
            }
            // Labels: the subject, big nodes, the selection and its neighbours, more as you zoom.
            val showAll = scale > 2.2f || n <= 25
            for (i in 0 until n) {
                val big = radius[i] > minR + spanR * 0.55f
                val wanted = i == 0 || i == sel || near?.contains(i) == true || showAll || (big && sel == null)
                if (!wanted) continue
                val layout = labels.getOrPut(i) {
                    measurer.measure(
                        g.nodes[i].label,
                        labelStyle,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                        constraints = androidx.compose.ui.unit.Constraints(maxWidth = with(density) { 140.dp.roundToPx() }),
                    )
                }
                val c = Offset(x[i] * scale, y[i] * scale)
                val tl = Offset(c.x - layout.size.width / 2f, c.y + radius[i] + 3f)
                drawRect(
                    colors.grid.copy(alpha = 0.85f),
                    topLeft = tl,
                    size = Size(layout.size.width.toFloat(), layout.size.height.toFloat()),
                )
                drawText(layout, topLeft = tl)
            }
        }
    }
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun NodeCard(
    n: FfiGraphNode,
    neighbours: Int,
    colors: ChartColors,
    onClose: () -> Unit,
    onOpenTorrent: (String) -> Unit,
    onOpenMedia: (Long) -> Unit,
    onCopy: (String, String) -> Unit,
    npub: (String) -> String,
) {
    Card(Modifier.fillMaxWidth().padding(8.dp).heightIn(max = 260.dp)) {
        Column(Modifier.padding(12.dp).verticalScroll(rememberScrollState())) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Box(Modifier.size(12.dp).background(colors.kindColor(n.kind), CircleShape))
                Spacer(Modifier.width(6.dp))
                Text(n.label, Modifier.weight(1f), style = MaterialTheme.typography.titleSmall, maxLines = 2, overflow = TextOverflow.Ellipsis)
                TextButton(onClick = onClose) { Text("Close") }
            }
            Text(
                listOfNotNull(KIND_LABEL[n.kind] ?: n.kind, n.score?.let { "trust $it" }, "$neighbours links").joinToString(" · "),
                style = MaterialTheme.typography.labelMedium,
                color = colors.inkMuted,
            )
            for (d in n.detail) Text(d, style = MaterialTheme.typography.bodySmall)
            FlowRow(Modifier.padding(top = 6.dp), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                when (n.kind) {
                    "torrent", "release" -> OutlinedButton(onClick = { onOpenTorrent(n.id) }) { Text("Open torrent") }
                    "media" -> n.id.toLongOrNull()?.let { id -> OutlinedButton(onClick = { onOpenMedia(id) }) { Text("Open title") } }
                    "root", "follow", "publisher" -> OutlinedButton(onClick = { onCopy("npub", npub(n.id)) }) { Text("Copy npub") }
                    "relay", "tracker" -> OutlinedButton(onClick = { onCopy("URL", n.id) }) { Text("Copy URL") }
                    else -> {}
                }
            }
        }
    }
}

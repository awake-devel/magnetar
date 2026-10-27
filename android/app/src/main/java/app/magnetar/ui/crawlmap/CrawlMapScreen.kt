package app.magnetar.ui.crawlmap

import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.gestures.awaitEachGesture
import androidx.compose.foundation.gestures.awaitFirstDown
import androidx.compose.foundation.gestures.calculateCentroid
import androidx.compose.foundation.gestures.calculatePan
import androidx.compose.foundation.gestures.calculateZoom
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.Refresh
import androidx.compose.material3.Card
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilterChip
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedCard
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.produceState
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clipToBounds
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.graphics.drawscope.withTransform
import androidx.compose.ui.graphics.lerp
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.selected
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.LinkAnnotation
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.TextLinkStyles
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextDecoration
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.text.withLink
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import app.magnetar.api.CrawlMap
import app.magnetar.api.GeoCount
import app.magnetar.ui.common.Banner
import app.magnetar.ui.common.cachedAgo
import app.magnetar.ui.common.formatAgo
import app.magnetar.ui.common.formatCompact
import app.magnetar.ui.common.formatDate
import app.magnetar.ui.common.formatNumber
import app.magnetar.ui.common.formatShare
import app.magnetar.ui.theme.ChartColors
import app.magnetar.ui.theme.chartColors

/** Rows of the top lists before "Show all". */
private const val TOP_ROWS = 15

/**
 * The crawl map (feature `crawl_map`, docs/APP.md §15): DHT discoveries or Nostr relays per
 * country on a world choropleth (Compose Canvas, sequential log scale), the top countries and
 * networks. The DB-IP and Natural Earth attributions stay visible at the bottom of the screen.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun CrawlMapScreen(vm: CrawlMapViewModel, onBack: () -> Unit) {
    val state by vm.state.collectAsStateWithLifecycle()
    val colors = chartColors()
    val context = LocalContext.current
    val world by produceState<WorldShapes?>(null) { value = WorldMapAsset.load(context) }
    val map = state.map
    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text("Crawl map") },
                navigationIcon = { IconButton(onClick = onBack) { Icon(Icons.AutoMirrored.Filled.ArrowBack, "Back") } },
                actions = { IconButton(onClick = vm::refresh, modifier = Modifier.testTag("crawl_map_refresh")) { Icon(Icons.Filled.Refresh, "Refresh") } },
            )
        },
        bottomBar = { Attribution(map) },
    ) { padding ->
        LazyColumn(Modifier.padding(padding).fillMaxSize().testTag("crawl_map")) {
            item(key = "top") {
                Column(Modifier.fillMaxWidth().heightIn(min = 1.dp)) {
                    if (state.loading) LinearProgressIndicator(Modifier.fillMaxWidth())
                    val error = state.error
                    when {
                        state.missing -> Banner("This server has no crawl map (it needs a newer version).")
                        error != null -> Banner((if (map != null) "Offline: " else "") + error + (state.cachedAtMs?.let { " · " + cachedAgo(it) } ?: ""), error = true)
                        state.cachedAtMs != null -> Banner(cachedAgo(state.cachedAtMs!!))
                    }
                    if (map != null && !map.available) {
                        Banner(geoReasonMessage(map.reason), Modifier.testTag("crawl_map_unavailable"))
                    }
                }
            }
            item(key = "controls") { Controls(state, vm, colors) }
            item(key = "map") {
                OutlinedCard(Modifier.fillMaxWidth().padding(horizontal = 12.dp, vertical = 6.dp)) {
                    Column(Modifier.padding(8.dp)) {
                        MapPart(world, state, colors, vm::select)
                    }
                }
            }
            if (map != null) {
                val metric = state.metricIndex
                val total = totalOf(map, metric)
                item(key = "countries") {
                    TopList(
                        title = "Top countries",
                        tag = "top_countries",
                        rows = topBy(map.countries, metric, Int.MAX_VALUE),
                        metric = metric,
                        total = total,
                        selected = state.selected,
                        colors = colors,
                        label = { g -> g.country?.let { "${countryName(it)} (${it.uppercase()})" } ?: "?" },
                        onClick = { g -> g.country?.let { vm.select(it) } },
                    ) {
                        val unknown = map.unknown.getOrNull(metric) ?: 0
                        if (unknown > 0) {
                            Text(
                                "Location unknown: ${formatNumber(unknown)} (${formatShare(unknown, total)})",
                                style = MaterialTheme.typography.labelSmall,
                                color = colors.inkMuted,
                                modifier = Modifier.padding(top = 4.dp).testTag("crawl_map_unknown"),
                            )
                        }
                    }
                }
                item(key = "networks") {
                    TopList(
                        title = "Top networks (AS)",
                        tag = "top_networks",
                        rows = topBy(map.networks, metric, Int.MAX_VALUE),
                        metric = metric,
                        total = total,
                        selected = null,
                        colors = colors,
                        label = { g -> networkLabel(g) + (g.country?.let { " · ${it.uppercase()}" } ?: "") },
                        onClick = null,
                    ) {
                        if (map.networks.isNotEmpty()) {
                            Text(
                                "The server lists its top ${map.networks.size} networks.",
                                style = MaterialTheme.typography.labelSmall,
                                color = colors.inkMuted,
                                modifier = Modifier.padding(top = 4.dp),
                            )
                        }
                    }
                }
            }
            item { Spacer(Modifier.height(8.dp)) }
        }
    }
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun Controls(state: CrawlMapUiState, vm: CrawlMapViewModel, colors: ChartColors) {
    Column(Modifier.padding(horizontal = 16.dp, vertical = 4.dp)) {
        FlowRow(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
            for (l in MapLayer.entries) {
                FilterChip(
                    selected = state.layer == l,
                    onClick = { vm.setLayer(l) },
                    label = { Text(l.label) },
                    modifier = Modifier.testTag("crawl_map_layer_${l.api}"),
                )
            }
            Spacer(Modifier.width(8.dp))
            for (w in MapWindow.entries) {
                FilterChip(
                    selected = state.window == w,
                    onClick = { vm.setWindow(w) },
                    label = { Text(w.label) },
                    modifier = Modifier.testTag("crawl_map_window_${w.api}"),
                )
            }
        }
        val map = state.map ?: return@Column
        if (map.metrics.size > 1) {
            FlowRow(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                for (m in map.metrics) {
                    FilterChip(
                        selected = state.metricName == m,
                        onClick = { vm.pickMetric(m) },
                        label = { Text(metricLabel(m)) },
                        modifier = Modifier.testTag("crawl_map_metric_$m"),
                    )
                }
            }
        }
        val name = state.metricName
        val span = when {
            state.window == MapWindow.DAY -> "last 24 hours"
            map.since > 0 -> "since ${formatDate(map.since)}"
            else -> "all time"
        }
        Text(
            listOfNotNull(name?.let { metricHint(it) }, span, map.computedAt.takeIf { it > 0 }?.let { "computed ${formatAgo(it)}" }).joinToString(" · "),
            style = MaterialTheme.typography.labelSmall,
            color = colors.inkMuted,
        )
    }
}

/** Colour of a country with shade [t] (null = no data). */
private fun ChartColors.mapFill(t: Float?): Color = if (t == null) mapEmpty else lerp(mapLow, mapHigh, t)

@Composable
private fun MapPart(world: WorldShapes?, state: CrawlMapUiState, colors: ChartColors, onSelect: (String?) -> Unit) {
    val map = state.map
    val metric = state.metricIndex
    val counts = remember(map, metric) { map?.let { countryCounts(it, metric) }.orEmpty() }
    val scale = remember(counts) { LogScale.of(counts.values) }
    if (world == null) {
        Box(Modifier.fillMaxWidth().aspectRatio(2.5f), contentAlignment = Alignment.Center) {
            Text("Loading the world map…", style = MaterialTheme.typography.bodySmall, color = colors.inkMuted)
        }
    } else {
        WorldCanvas(world, counts, scale, state.selected, colors, onSelect)
    }
    Legend(scale, colors)
    val sel = state.selected
    if (sel != null && map != null) SelectedCountry(sel, map, colors) { onSelect(null) }
}

/**
 * The choropleth: pinch to zoom, drag to pan once zoomed (unzoomed, a drag scrolls the page),
 * double tap to reset, tap a country to read it.
 */
@Composable
private fun WorldCanvas(
    world: WorldShapes,
    counts: Map<String, Long>,
    scale: LogScale?,
    selected: String?,
    colors: ChartColors,
    onSelect: (String?) -> Unit,
) {
    val m = world.map
    var zoom by remember { mutableFloatStateOf(1f) }
    var offset by remember { mutableStateOf(Offset.Zero) }
    val select by rememberUpdatedState(onSelect)
    val fills = remember(world, counts, scale, colors) {
        m.countries.map { c -> colors.mapFill(scale?.shade(counts[c.code] ?: 0)) }
    }
    val described = remember(counts) {
        val top = counts.entries.sortedByDescending { it.value }.take(5)
        "World map, ${counts.size} countries with data" +
            if (top.isEmpty()) "" else "; most: " + top.joinToString { "${countryName(it.key)} ${formatNumber(it.value)}" }
    }
    Canvas(
        Modifier
            .fillMaxWidth()
            .aspectRatio(m.width.toFloat() / m.height)
            .clipToBounds()
            .testTag("crawl_map_canvas")
            .semantics { contentDescription = described }
            .pointerInput(world) {
                fun clamp(o: Offset, z: Float) = Offset(
                    o.x.coerceIn(size.width * (1 - z), 0f),
                    o.y.coerceIn(size.height * (1 - z), 0f),
                )
                awaitEachGesture {
                    awaitFirstDown(requireUnconsumed = false)
                    do {
                        val event = awaitPointerEvent()
                        val fingers = event.changes.count { it.pressed }
                        if (fingers >= 2) {
                            val z = (zoom * event.calculateZoom()).coerceIn(1f, 8f)
                            val c = event.calculateCentroid()
                            val pan = event.calculatePan()
                            // Keep the point under the fingers in place while zooming.
                            offset = clamp(c - (c - offset) * (z / zoom) + pan, z)
                            zoom = z
                            event.changes.forEach { it.consume() }
                        } else if (fingers == 1 && zoom > 1f) {
                            val pan = event.calculatePan()
                            if (pan != Offset.Zero) {
                                offset = clamp(offset + pan, zoom)
                                event.changes.forEach { it.consume() }
                            }
                        }
                    } while (event.changes.any { it.pressed })
                }
            }
            .pointerInput(world) {
                detectTapGestures(
                    onDoubleTap = {
                        zoom = 1f
                        offset = Offset.Zero
                    },
                    onTap = { o ->
                        val base = size.width.toFloat() / m.width
                        val p = (o - offset) / (zoom * base)
                        select(m.countryAt(p.x, p.y)?.code)
                    },
                )
            },
    ) {
        val base = size.width / m.width
        val k = base * zoom
        withTransform({
            translate(offset.x, offset.y)
            scale(k, k, pivot = Offset.Zero)
        }) {
            val border = Stroke(width = 0.6.dp.toPx() / k)
            for (i in m.countries.indices) {
                drawPath(world.paths[i], fills[i])
                drawPath(world.paths[i], colors.mapBorder, style = border)
            }
            val s = selected?.let { code -> m.countries.indexOfFirst { it.code == code } }?.takeIf { it >= 0 }
            if (s != null) drawPath(world.paths[s], colors.ink, style = Stroke(width = 2.dp.toPx() / k))
        }
    }
    Text(
        if (zoom > 1f) "Drag to move, double tap to reset" else "Tap a country · pinch to zoom",
        style = MaterialTheme.typography.labelSmall,
        color = colors.inkMuted,
    )
}

/** Min … max of the log scale, plus the neutral "no data" swatch. */
@Composable
private fun Legend(scale: LogScale?, colors: ChartColors) {
    Row(Modifier.fillMaxWidth().padding(top = 4.dp).testTag("crawl_map_legend"), verticalAlignment = Alignment.CenterVertically) {
        Box(Modifier.size(12.dp).background(colors.mapEmpty, RoundedCornerShape(2.dp)))
        Text(" no data", style = MaterialTheme.typography.labelSmall, color = colors.inkMuted)
        Spacer(Modifier.width(12.dp))
        if (scale == null) return@Row
        Text(formatCompact(scale.min.toDouble()), style = MaterialTheme.typography.labelSmall)
        val stops = remember(colors) { List(6) { lerp(colors.mapLow, colors.mapHigh, it / 5f) } }
        Box(
            Modifier
                .weight(1f)
                .padding(horizontal = 6.dp)
                .height(10.dp)
                .background(Brush.horizontalGradient(stops), RoundedCornerShape(2.dp))
                .semantics { contentDescription = "Log scale from ${formatNumber(scale.min)} to ${formatNumber(scale.max)}" },
        )
        Text(formatCompact(scale.max.toDouble()), style = MaterialTheme.typography.labelSmall)
        Text(" (log)", style = MaterialTheme.typography.labelSmall, color = colors.inkMuted)
    }
}

@Composable
private fun SelectedCountry(code: String, map: CrawlMap, colors: ChartColors, onClear: () -> Unit) {
    val row = map.countries.firstOrNull { it.country.equals(code, ignoreCase = true) }
    Card(Modifier.fillMaxWidth().padding(top = 8.dp).testTag("crawl_map_selected")) {
        Column(Modifier.padding(12.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(
                    "${countryName(code)} ($code)",
                    Modifier.weight(1f).semantics { heading() },
                    style = MaterialTheme.typography.titleSmall,
                )
                TextButton(onClick = onClear) { Text("Clear") }
            }
            if (row == null) {
                Text("No data", style = MaterialTheme.typography.bodySmall, color = colors.inkMuted)
            } else {
                for ((i, m) in map.metrics.withIndex()) {
                    val n = row.count(i)
                    Row(Modifier.fillMaxWidth().padding(top = 2.dp)) {
                        Text(metricLabel(m), Modifier.weight(1f), style = MaterialTheme.typography.bodySmall)
                        Text(
                            "${formatNumber(n)} (${formatShare(n, totalOf(map, i))})",
                            style = MaterialTheme.typography.bodySmall,
                        )
                    }
                }
            }
        }
    }
}

/** Bars of [metric] with the count and share of [total]; "Show all" past [TOP_ROWS]. */
@Composable
private fun TopList(
    title: String,
    tag: String,
    rows: List<GeoCount>,
    metric: Int,
    total: Long,
    selected: String?,
    colors: ChartColors,
    label: (GeoCount) -> String,
    onClick: ((GeoCount) -> Unit)?,
    footer: @Composable () -> Unit,
) {
    var all by rememberSaveable(tag) { mutableStateOf(false) }
    val max = rows.maxOfOrNull { it.count(metric) }?.coerceAtLeast(1) ?: 1
    OutlinedCard(Modifier.fillMaxWidth().padding(horizontal = 12.dp, vertical = 6.dp).testTag(tag)) {
        Column(Modifier.padding(12.dp)) {
            Text(title, style = MaterialTheme.typography.titleSmall, modifier = Modifier.semantics { heading() })
            if (rows.isEmpty()) Text("No data", style = MaterialTheme.typography.bodySmall, color = colors.inkMuted)
            for (g in if (all) rows else rows.take(TOP_ROWS)) {
                val n = g.count(metric)
                val on = selected != null && g.country.equals(selected, ignoreCase = true)
                val text = label(g)
                val share = formatShare(n, total)
                Row(
                    Modifier
                        .fillMaxWidth()
                        .heightIn(min = 36.dp)
                        .then(if (onClick != null) Modifier.clickable(role = Role.Button) { onClick(g) } else Modifier)
                        .semantics(mergeDescendants = true) {
                            this.selected = on
                            contentDescription = "$text: ${formatNumber(n)}, $share"
                        },
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Text(
                        text,
                        Modifier.width(140.dp),
                        style = MaterialTheme.typography.bodySmall,
                        fontWeight = if (on) FontWeight.Bold else null,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                    BoxWithConstraints(Modifier.weight(1f).height(14.dp)) {
                        val frac = (n.toDouble() / max).toFloat().coerceIn(0.02f, 1f)
                        Box(
                            Modifier
                                .width(maxWidth * frac)
                                .height(14.dp)
                                .background(if (on) colors.barSelected else colors.bar, RoundedCornerShape(topEnd = 4.dp, bottomEnd = 4.dp)),
                        )
                    }
                    Text(formatCompact(n.toDouble()), Modifier.width(52.dp), style = MaterialTheme.typography.labelMedium, textAlign = TextAlign.End)
                    Text(share, Modifier.width(44.dp), style = MaterialTheme.typography.labelSmall, color = colors.inkMuted, textAlign = TextAlign.End)
                }
            }
            if (rows.size > TOP_ROWS) {
                TextButton(onClick = { all = !all }, modifier = Modifier.testTag("${tag}_all")) {
                    Text(if (all) "Show fewer" else "Show all ${rows.size}")
                }
            }
            footer()
        }
    }
}

/**
 * Always on screen: DB-IP's CC BY 4.0 licence asks for a link back on every page that shows
 * its results; Natural Earth is public domain (credited anyway).
 */
@Composable
private fun Attribution(map: CrawlMap?) {
    val link = TextLinkStyles(SpanStyle(color = MaterialTheme.colorScheme.primary, textDecoration = TextDecoration.Underline))
    val built = map?.database?.countryBuild?.let { buildMonth(it) }
    Surface(tonalElevation = 2.dp, modifier = Modifier.fillMaxWidth().testTag("crawl_map_attribution")) {
        Text(
            buildAnnotatedString {
                withLink(LinkAnnotation.Url("https://db-ip.com", link)) { append("IP Geolocation by DB-IP") }
                append(" (CC BY 4.0")
                if (built != null) append(", database of $built")
                append(") · Map: Natural Earth (public domain)")
            },
            style = MaterialTheme.typography.labelSmall,
            modifier = Modifier.padding(horizontal = 16.dp, vertical = 6.dp),
        )
    }
}

package app.magnetar.ui.stats

import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.FilledTonalButton
import androidx.compose.material3.FilterChip
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedCard
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import app.magnetar.api.CrawlMachine
import app.magnetar.api.CrawlSources
import app.magnetar.api.DhtSources
import app.magnetar.api.NostrSources
import app.magnetar.api.RouteCount
import app.magnetar.ui.common.AppIcons
import app.magnetar.ui.common.bucketLabel
import app.magnetar.ui.common.cachedAgo
import app.magnetar.ui.common.formatCompact
import app.magnetar.ui.common.formatDate
import app.magnetar.ui.common.formatNumber
import app.magnetar.ui.theme.ChartColors

/*
 * "Where discoveries come from" (feature `crawl_sources`, docs/APP.md §15): DHT routes with
 * their conversion and the last 48 hours, Nostr relays by discovery source and the top relays,
 * one row per machine, and the button to the crawl map (feature `crawl_map`). Every bar has its
 * label and number written next to it.
 */

@Composable
internal fun SourcesCard(state: StatsUiState, colors: ChartColors, onOpenCrawlMap: (() -> Unit)?) {
    val s = state.sources
    OutlinedCard(Modifier.fillMaxWidth().padding(horizontal = 12.dp, vertical = 6.dp).testTag("sources_card")) {
        Column(Modifier.padding(12.dp)) {
            Text("Where discoveries come from", style = MaterialTheme.typography.titleSmall, modifier = Modifier.semantics { heading() })
            val note = listOfNotNull(
                state.sourcesError?.let { (if (s != null) "Offline: " else "") + it },
                state.sourcesCachedAtMs?.let { cachedAgo(it) },
            ).joinToString(" · ")
            if (note.isNotEmpty()) {
                Text(
                    note,
                    style = MaterialTheme.typography.labelSmall,
                    color = if (state.sourcesError != null) MaterialTheme.colorScheme.error else colors.inkMuted,
                )
            }
            when {
                s != null -> SourcesBody(s, colors)
                state.sourcesError == null -> LinearProgressIndicator(Modifier.fillMaxWidth().padding(vertical = 12.dp))
            }
            if (onOpenCrawlMap != null) {
                FilledTonalButton(onClick = onOpenCrawlMap, modifier = Modifier.padding(top = 8.dp).testTag("open_crawl_map")) {
                    Icon(AppIcons.Public, null, Modifier.padding(end = 8.dp))
                    Text("Crawl map")
                }
            }
        }
    }
}

@Composable
private fun SourcesBody(s: CrawlSources, colors: ChartColors) {
    s.dht?.let { DhtPart(it, colors) }
    s.nostr?.let { NostrPart(it, colors) }
    if (s.machines.isNotEmpty()) MachinesPart(s.machines, colors)
}

@Composable
private fun SubTitle(text: String) {
    Text(text, Modifier.padding(top = 12.dp, bottom = 2.dp).semantics { heading() }, style = MaterialTheme.typography.labelLarge)
}

@Composable
private fun Muted(text: String, colors: ChartColors) {
    Text(text, style = MaterialTheme.typography.labelSmall, color = colors.inkMuted)
}

@Composable
private fun KeyValue(label: String, value: String) {
    Row(Modifier.fillMaxWidth().padding(top = 2.dp)) {
        Text(label, Modifier.weight(1f), style = MaterialTheme.typography.bodySmall)
        Text(value, style = MaterialTheme.typography.bodySmall)
    }
}

/** A one-hue bar of [frac] of the width with its number after it. */
@Composable
private fun ValueBar(frac: Float, color: Color, text: String) {
    Row(Modifier.fillMaxWidth().padding(vertical = 1.dp), verticalAlignment = Alignment.CenterVertically) {
        BoxWithConstraints(Modifier.weight(1f).height(12.dp)) {
            Box(
                Modifier
                    .width(maxWidth * frac.coerceIn(0.01f, 1f))
                    .height(12.dp)
                    .background(color, RoundedCornerShape(topEnd = 3.dp, bottomEnd = 3.dp)),
            )
        }
        Text(text, Modifier.width(96.dp), style = MaterialTheme.typography.labelSmall, textAlign = TextAlign.End)
    }
}

@Composable
private fun DhtPart(d: DhtSources, colors: ChartColors) {
    SubTitle("DHT: hashes seen and torrents stored, per route")
    val routes = orderedRoutes(d.routes)
    val max = routes.maxOfOrNull { it.seen }?.coerceAtLeast(1) ?: 1
    if (routes.isEmpty()) Muted("Nothing seen yet", colors)
    for (r in routes) RouteRow(r, max, colors)
    if (d.hourly.isNotEmpty()) RouteHours(d, colors)
}

@Composable
private fun RouteRow(r: RouteCount, max: Long, colors: ChartColors) {
    val conversion = formatConversion(r.stored, r.seen)
    Column(
        Modifier
            .fillMaxWidth()
            .padding(top = 6.dp)
            .semantics(mergeDescendants = true) {
                contentDescription = "${routeLabel(r.via)}: ${formatNumber(r.seen)} seen, ${formatNumber(r.stored)} stored, $conversion stored"
            }
            .testTag("route_${r.via}"),
    ) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text(routeLabel(r.via), Modifier.weight(1f), style = MaterialTheme.typography.bodySmall)
            Text("$conversion stored", style = MaterialTheme.typography.labelMedium, fontWeight = FontWeight.Bold)
        }
        ValueBar((r.seen.toDouble() / max).toFloat(), colors.bar, "seen ${formatCompact(r.seen.toDouble())}")
        ValueBar((r.stored.toDouble() / max).toFloat(), colors.barSelected, "stored ${formatCompact(r.stored.toDouble())}")
    }
}

/** The last 48 hours as stacked columns per route (categorical slots in [ROUTE_ORDER]); tap reads an hour. */
@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun RouteHours(d: DhtSources, colors: ChartColors) {
    var stored by rememberSaveable { mutableStateOf(false) }
    val hours = d.hourly
    val names = remember(hours) { hourRoutes(hours) }
    var picked by remember(hours) { mutableStateOf<Int?>(null) }
    fun values(i: Int): List<Long> = names.map { n -> (if (stored) hours[i].stored else hours[i].seen)[n] ?: 0 }
    val top = hours.indices.maxOfOrNull { values(it).sum() }?.coerceAtLeast(1) ?: 1
    fun colorOf(name: String): Color =
        ROUTE_ORDER.indexOf(name).takeIf { it >= 0 }?.let { colors.categorical[it] } ?: colors.other
    Row(Modifier.padding(top = 10.dp), verticalAlignment = Alignment.CenterVertically) {
        Text("Last ${hours.size} hours", Modifier.weight(1f), style = MaterialTheme.typography.labelMedium)
        FilterChip(selected = !stored, onClick = { stored = false }, label = { Text("Seen") })
        FilterChip(
            selected = stored,
            onClick = { stored = true },
            label = { Text("Stored") },
            modifier = Modifier.padding(start = 6.dp).testTag("route_hours_stored"),
        )
    }
    val p = picked?.takeIf { it in hours.indices }
    Text(
        if (p != null) {
            bucketLabel(hours[p].bucket, hourly = true) + ": " +
                names.zip(values(p)).joinToString(" · ") { (n, v) -> "${routeLabel(n)} ${formatNumber(v)}" }
        } else {
            "highest hour ${formatNumber(top)} · tap a column"
        },
        style = MaterialTheme.typography.labelSmall,
        fontWeight = if (p != null) FontWeight.Bold else null,
        modifier = Modifier.testTag("route_hours_value"),
    )
    Canvas(
        Modifier
            .fillMaxWidth()
            .height(96.dp)
            .testTag("route_hours_chart")
            .semantics { contentDescription = "${if (stored) "Stored" else "Seen"} per hour by route, ${hours.size} hours, highest ${formatNumber(top)}" }
            .pointerInput(hours) {
                detectTapGestures { o -> picked = (o.x / size.width * hours.size).toInt().coerceIn(0, hours.size - 1) }
            },
    ) {
        val cw = size.width / hours.size
        val gap = if (cw > 4f) cw * 0.2f else 0f
        drawLine(colors.axis, Offset(0f, size.height - 1f), Offset(size.width, size.height - 1f), 2f)
        for (i in hours.indices) {
            var y = size.height
            for ((n, v) in names.zip(values(i))) {
                if (v <= 0) continue
                val h = (v.toFloat() / top * (size.height - 4f)).coerceAtLeast(1f)
                drawRect(colorOf(n), Offset(i * cw + gap / 2, y - h), Size((cw - gap).coerceAtLeast(1f), h))
                y -= h
            }
            if (i == p) drawLine(colors.ink.copy(alpha = 0.5f), Offset(i * cw + cw / 2, 0f), Offset(i * cw + cw / 2, size.height), 1f)
        }
    }
    Row(Modifier.fillMaxWidth()) {
        Muted(bucketLabel(hours.first().bucket, hourly = true), colors)
        Spacer(Modifier.weight(1f))
        Muted(bucketLabel(hours.last().bucket, hourly = true), colors)
    }
    FlowRow(horizontalArrangement = Arrangement.spacedBy(12.dp)) {
        for (n in names) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Box(Modifier.size(10.dp).background(colorOf(n), CircleShape))
                Spacer(Modifier.width(4.dp))
                Text(routeLabel(n), style = MaterialTheme.typography.labelSmall)
            }
        }
    }
}

@Composable
private fun NostrPart(n: NostrSources, colors: ChartColors) {
    SubTitle("Nostr: relays by how they were found")
    val rows = n.bySource.sortedByDescending { it.relays }
    val max = rows.maxOfOrNull { it.relays }?.coerceAtLeast(1) ?: 1
    if (rows.isEmpty()) Muted("No relays yet", colors)
    for (r in rows) {
        Column(
            Modifier
                .fillMaxWidth()
                .padding(top = 4.dp)
                .semantics(mergeDescendants = true) {}
                .testTag("relay_source_${r.source}"),
        ) {
            Text(relaySourceLabel(r.source), style = MaterialTheme.typography.bodySmall)
            ValueBar((r.relays.toDouble() / max).toFloat(), colors.bar, "${formatCompact(r.relays.toDouble())} relays")
            Muted(
                "${formatNumber(r.enabled)} enabled · ${formatNumber(r.delivered)} delivered · " +
                    "${formatCompact(r.events.toDouble())} events · ${formatCompact(r.torrents.toDouble())} torrents",
                colors,
            )
        }
    }
    if (n.topRelays.isNotEmpty()) {
        SubTitle("Top relays")
        if (n.torrentsSince > 0) Muted("Torrents counted since ${formatDate(n.torrentsSince)}", colors)
        for ((i, r) in n.topRelays.withIndex()) {
            if (i > 0) HorizontalDivider(Modifier.padding(vertical = 2.dp), color = colors.grid)
            Column(Modifier.fillMaxWidth().padding(vertical = 2.dp).semantics(mergeDescendants = true) {}) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Text(
                        shortRelayUrl(r.url),
                        Modifier.weight(1f),
                        style = MaterialTheme.typography.bodySmall,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                    Text(formatCompact(r.events.toDouble()) + " ev", style = MaterialTheme.typography.labelMedium)
                    Text(formatCompact(r.torrents.toDouble()) + " tor", Modifier.width(64.dp), style = MaterialTheme.typography.labelMedium, textAlign = TextAlign.End)
                }
                val place = relayPlace(r)
                val state = when {
                    !r.enabled -> "disabled"
                    r.ok24h -> "answered in 24 h"
                    else -> "not answering"
                }
                Muted(listOfNotNull(relaySourceLabel(r.source), state, place).joinToString(" · "), colors)
            }
        }
        Muted("ev = events, tor = torrents", colors)
    }
}

@Composable
private fun MachinesPart(machines: List<CrawlMachine>, colors: ChartColors) {
    SubTitle("Machines")
    for (m in machines.sortedBy { if (it.kind == "server") 0 else 1 }) {
        Column(Modifier.fillMaxWidth().padding(top = 4.dp).testTag("machine_${m.name}")) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(machineTitle(m), Modifier.weight(1f), style = MaterialTheme.typography.bodySmall, fontWeight = FontWeight.Bold)
                Box(Modifier.size(8.dp).background(if (m.connected) colors.good else colors.unknown, CircleShape))
                Spacer(Modifier.width(4.dp))
                Text(if (m.connected) "connected" else "not connected", style = MaterialTheme.typography.labelSmall)
            }
            for ((k, v) in machineLines(m)) KeyValue(k, v)
        }
    }
}

package app.magnetar.ui.charts

import androidx.compose.foundation.Canvas
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.background
import androidx.compose.foundation.combinedClickable
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
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.selected
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import app.magnetar.ffi.FfiBreakdown
import app.magnetar.ffi.FfiFacetCount
import app.magnetar.formatCompact
import app.magnetar.formatNumber
import app.magnetar.formatShare
import app.magnetar.ui.theme.ChartColors
import app.magnetar.ui.theme.slotOf
import kotlin.math.PI
import kotlin.math.atan2
import kotlin.math.hypot
import kotlin.math.min

/** Fixed vocabularies of the donut fields: a value keeps its colour slot whatever its rank. */
val DONUT_ORDER = mapOf(
    "category" to listOf("video", "anime", "audio", "book", "game", "application", "porn", "picture"),
    "resolution" to listOf("2160p", "1080p", "720p", "576p", "480p", "sd", "1440p", "4320p"),
    "media_kind" to listOf("movie", "series", "anime"),
)

/** "Other (12 values)" / "No value" lines under a chart. */
@Composable
private fun RestLines(b: FfiBreakdown, colors: ChartColors) {
    val parts = buildList {
        if (b.other > 0uL) add("Other: ${formatNumber(b.other.toLong())} in ${formatNumber(b.otherValues.toLong())} more values")
        if (b.missing > 0uL) add("Unknown: ${formatNumber(b.missing.toLong())}")
    }
    if (parts.isNotEmpty()) {
        Text(
            parts.joinToString(" · "),
            style = MaterialTheme.typography.labelSmall,
            color = colors.inkMuted,
            modifier = Modifier.padding(top = 4.dp),
        )
    }
}

/**
 * Horizontal bars, most frequent first, one hue (magnitude): tap toggles a cross-filter on the
 * value, long press opens Search with it. Labels and numbers are always written next to the
 * bar, so nothing relies on colour.
 */
@OptIn(ExperimentalFoundationApi::class)
@Composable
fun BreakdownBars(
    b: FfiBreakdown,
    selected: Set<String>,
    colors: ChartColors,
    onToggle: (FfiFacetCount) -> Unit,
    onOpen: (FfiFacetCount) -> Unit,
) {
    val max = b.counts.maxOfOrNull { it.count }?.coerceAtLeast(1uL) ?: 1uL
    Column(Modifier.fillMaxWidth()) {
        if (b.counts.isEmpty()) {
            Text("No values", style = MaterialTheme.typography.bodySmall, color = colors.inkMuted)
        }
        for (c in b.counts) {
            val on = c.value in selected
            val share = formatShare(c.count, b.total)
            Row(
                Modifier
                    .fillMaxWidth()
                    .combinedClickable(
                        role = Role.Checkbox,
                        onClick = { onToggle(c) },
                        onLongClick = { onOpen(c) },
                    )
                    .semantics {
                        this.selected = on
                        contentDescription = "${c.label}: ${formatNumber(c.count.toLong())}, $share"
                    }
                    .padding(vertical = 3.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Text(
                    c.label,
                    Modifier.width(110.dp),
                    style = MaterialTheme.typography.bodySmall,
                    fontWeight = if (on) FontWeight.Bold else null,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
                BoxWithConstraints(Modifier.weight(1f).height(16.dp)) {
                    val frac = (c.count.toDouble() / max.toDouble()).toFloat().coerceIn(0.02f, 1f)
                    Box(
                        Modifier
                            .width(maxWidth * frac)
                            .height(16.dp)
                            .background(if (on) colors.barSelected else colors.bar, RoundedCornerShape(topEnd = 4.dp, bottomEnd = 4.dp)),
                    )
                }
                Text(
                    formatCompact(c.count.toDouble()),
                    Modifier.width(56.dp),
                    style = MaterialTheme.typography.labelMedium,
                    textAlign = TextAlign.End,
                    fontWeight = if (on) FontWeight.Bold else null,
                )
                Text(share, Modifier.width(44.dp), style = MaterialTheme.typography.labelSmall, color = colors.inkMuted, textAlign = TextAlign.End)
            }
        }
        RestLines(b, colors)
    }
}

/** A donut slice: value (null = "Other"), count, colour. */
private data class Slice(val count: FfiFacetCount?, val n: ULong, val color: Color)

private fun slices(b: FfiBreakdown, order: List<String>, colors: ChartColors): List<Slice> {
    val out = mutableListOf<Slice>()
    var other = b.other
    for (c in b.counts) {
        val color = colors.slotOf(c.value, order)
        if (color == null) other += c.count else out += Slice(c, c.count, color)
    }
    if (other > 0uL) out += Slice(null, other, colors.other)
    return out
}

/**
 * Donut for fields with a fixed, short vocabulary ([DONUT_ORDER]), with a legend that carries
 * the label, count and share of every slice (tap: cross-filter, long press: Search).
 */
@OptIn(ExperimentalFoundationApi::class)
@Composable
fun BreakdownDonut(
    b: FfiBreakdown,
    selected: Set<String>,
    colors: ChartColors,
    onToggle: (FfiFacetCount) -> Unit,
    onOpen: (FfiFacetCount) -> Unit,
) {
    val order = DONUT_ORDER[b.field].orEmpty()
    val parts = remember(b, colors) { slices(b, order, colors) }
    // The gesture handler outlives a recomposition: it calls the latest callbacks.
    val toggle by rememberUpdatedState(onToggle)
    val open by rememberUpdatedState(onOpen)
    val sum = parts.sumOf { it.n.toDouble() }.coerceAtLeast(1.0)
    Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
        Box(Modifier.size(132.dp), contentAlignment = Alignment.Center) {
            Canvas(
                Modifier
                    .size(132.dp)
                    .pointerInput(parts) {
                        fun hit(o: Offset): FfiFacetCount? {
                            val c = Offset(size.width / 2f, size.height / 2f)
                            val r = min(size.width, size.height) / 2f
                            val d = hypot(o.x - c.x, o.y - c.y)
                            if (d < r * 0.45f || d > r) return null
                            var a = (atan2(o.y - c.y, o.x - c.x) * 180 / PI + 90).toFloat()
                            if (a < 0) a += 360f
                            var start = 0f
                            for (p in parts) {
                                val sweep = (p.n.toDouble() / sum * 360).toFloat()
                                if (a < start + sweep) return p.count
                                start += sweep
                            }
                            return null
                        }
                        detectTapGestures(
                            onTap = { o -> hit(o)?.let(toggle) },
                            onLongPress = { o -> hit(o)?.let(open) },
                        )
                    },
            ) {
                val stroke = size.minDimension * 0.22f
                val inset = stroke / 2
                val arcSize = Size(size.width - stroke, size.height - stroke)
                var start = -90f
                for (p in parts) {
                    val sweep = (p.n.toDouble() / sum * 360).toFloat()
                    val on = p.count?.value in selected
                    // A 2° surface gap between slices.
                    val gap = if (parts.size > 1) 1f else 0f
                    drawArc(
                        color = p.color,
                        startAngle = start + gap,
                        sweepAngle = (sweep - 2 * gap).coerceAtLeast(0.5f),
                        useCenter = false,
                        topLeft = Offset(inset, inset),
                        size = arcSize,
                        style = Stroke(width = if (on) stroke * 1.25f else stroke),
                    )
                    start += sweep
                }
            }
            Column(horizontalAlignment = Alignment.CenterHorizontally) {
                Text(formatCompact(b.total.toDouble()), style = MaterialTheme.typography.titleMedium)
                Text("total", style = MaterialTheme.typography.labelSmall, color = colors.inkMuted)
            }
        }
        Spacer(Modifier.width(12.dp))
        Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(2.dp)) {
            for (p in parts) {
                val c = p.count
                val on = c?.value in selected
                val label = c?.label ?: "Other"
                Row(
                    Modifier
                        .fillMaxWidth()
                        .then(
                            if (c != null) {
                                Modifier.combinedClickable(
                                    role = Role.Checkbox,
                                    onClick = { onToggle(c) },
                                    onLongClick = { onOpen(c) },
                                )
                            } else {
                                Modifier
                            },
                        )
                        .semantics { this.selected = on }
                        .padding(vertical = 2.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Box(Modifier.size(10.dp).background(p.color, CircleShape))
                    Spacer(Modifier.width(6.dp))
                    Text(
                        label,
                        Modifier.weight(1f),
                        style = MaterialTheme.typography.bodySmall,
                        fontWeight = if (on) FontWeight.Bold else null,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                    Text(formatCompact(p.n.toDouble()), style = MaterialTheme.typography.labelMedium)
                    Text(
                        formatShare(p.n, b.total),
                        Modifier.width(40.dp),
                        style = MaterialTheme.typography.labelSmall,
                        color = colors.inkMuted,
                        textAlign = TextAlign.End,
                    )
                }
            }
        }
    }
    if (b.missing > 0uL) {
        Text(
            "Unknown: ${formatNumber(b.missing.toLong())}",
            style = MaterialTheme.typography.labelSmall,
            color = colors.inkMuted,
        )
    }
}

/**
 * Years as columns in time order (a histogram, so gaps stay visible); tap toggles the year as
 * a cross-filter, long press opens Search. The tallest column and the selected ones are labelled.
 */
@Composable
fun YearColumns(
    b: FfiBreakdown,
    selected: Set<String>,
    colors: ChartColors,
    onToggle: (FfiFacetCount) -> Unit,
    onOpen: (FfiFacetCount) -> Unit,
) {
    val years = remember(b) {
        b.counts.mapNotNull { c -> c.value.toIntOrNull()?.let { it to c } }.sortedBy { it.first }
    }
    if (years.isEmpty()) {
        Text("No values", style = MaterialTheme.typography.bodySmall, color = colors.inkMuted)
        return
    }
    val first = years.first().first
    val last = years.last().first
    val span = (last - first + 1).coerceAtLeast(1)
    val max = years.maxOf { it.second.count }.coerceAtLeast(1uL).toDouble()
    val byYear = years.associate { it.first to it.second }
    val toggle by rememberUpdatedState(onToggle)
    val open by rememberUpdatedState(onOpen)
    fun at(x: Float, width: Float): FfiFacetCount? = byYear[first + (x / width * span).toInt().coerceIn(0, span - 1)]
    Column(Modifier.fillMaxWidth()) {
        Canvas(
            Modifier
                .fillMaxWidth()
                .height(120.dp)
                .semantics {
                    contentDescription = years.joinToString { "${it.first}: ${formatNumber(it.second.count.toLong())}" }
                }
                .pointerInput(years) {
                    detectTapGestures(
                        onTap = { o -> at(o.x, size.width.toFloat())?.let(toggle) },
                        onLongPress = { o -> at(o.x, size.width.toFloat())?.let(open) },
                    )
                },
        ) {
            val w = size.width / span
            val gap = if (w > 4f) 2f else 0f
            drawLine(colors.axis, Offset(0f, size.height), Offset(size.width, size.height), 1f)
            for ((y, c) in years) {
                val h = (c.count.toDouble() / max * (size.height - 4f)).toFloat().coerceAtLeast(1f)
                val x = (y - first) * w
                drawRect(
                    if (c.value in selected) colors.barSelected else colors.bar,
                    topLeft = Offset(x + gap / 2, size.height - h),
                    size = Size((w - gap).coerceAtLeast(1f), h),
                )
            }
        }
        Row(Modifier.fillMaxWidth()) {
            Text("$first", style = MaterialTheme.typography.labelSmall, color = colors.inkMuted)
            Spacer(Modifier.weight(1f))
            val peak = years.maxBy { it.second.count }
            Text(
                "peak ${peak.first}: ${formatNumber(peak.second.count.toLong())}",
                style = MaterialTheme.typography.labelSmall,
                color = colors.inkMuted,
            )
            Spacer(Modifier.weight(1f))
            Text("$last", style = MaterialTheme.typography.labelSmall, color = colors.inkMuted)
        }
        val sel = years.filter { it.second.value in selected }
        if (sel.isNotEmpty()) {
            Text(
                sel.joinToString { "${it.first}: ${formatNumber(it.second.count.toLong())}" },
                style = MaterialTheme.typography.labelMedium,
                fontWeight = FontWeight.Bold,
            )
        }
        RestLines(b, colors)
    }
}

/**
 * Seeded / dead / no answer as one stacked bar in the status colours, each part labelled with
 * its number (status colours never carry meaning alone).
 */
@OptIn(ExperimentalLayoutApi::class)
@Composable
fun StatusBar(parts: List<Triple<String, ULong, Color>>, colors: ChartColors) {
    val total = parts.sumOf { it.second.toDouble() }.coerceAtLeast(1.0)
    Column(Modifier.fillMaxWidth()) {
        Canvas(Modifier.fillMaxWidth().height(14.dp)) {
            var x = 0f
            for ((_, n, color) in parts) {
                val w = (n.toDouble() / total * size.width).toFloat()
                if (w > 0f) drawRect(color, Offset(x, 0f), Size((w - 2f).coerceAtLeast(1f), size.height))
                x += w
            }
        }
        FlowRow(Modifier.fillMaxWidth().padding(top = 4.dp), horizontalArrangement = Arrangement.spacedBy(12.dp)) {
            val sum = parts.fold(0uL) { a, p -> a + p.second }
            for ((label, n, color) in parts) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Box(Modifier.size(10.dp).background(color, CircleShape))
                    Spacer(Modifier.width(4.dp))
                    Text(
                        "$label ${formatCompact(n.toDouble())} (${formatShare(n, sum)})",
                        style = MaterialTheme.typography.labelSmall,
                        color = colors.ink,
                    )
                }
            }
        }
    }
}

package app.magnetar.ui.stats

import androidx.compose.foundation.Canvas
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.background
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.gestures.awaitEachGesture
import androidx.compose.foundation.gestures.awaitFirstDown
import androidx.compose.foundation.gestures.calculateCentroid
import androidx.compose.foundation.gestures.calculateZoom
import androidx.compose.foundation.gestures.detectHorizontalDragGestures
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
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.selected
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import app.magnetar.api.Breakdown
import app.magnetar.api.FacetCount
import app.magnetar.api.Timeseries
import app.magnetar.ui.common.bucketLabel
import app.magnetar.ui.common.formatBytes
import app.magnetar.ui.common.formatCompact
import app.magnetar.ui.common.formatNumber
import app.magnetar.ui.common.formatShare
import app.magnetar.ui.theme.ChartColors
import app.magnetar.ui.theme.slotOf
import kotlin.math.PI
import kotlin.math.atan2
import kotlin.math.hypot
import kotlin.math.min

/*
 * The Stats tab's charts, drawn with Compose Canvas (D89: no chart library). Every bar, slice
 * and column has its label and number written next to it or announced, so colour never carries
 * meaning alone (docs/APP.md §9).
 */

/** "Other: 9,120 in 41 more values · Unknown: 310,200" under a chart. */
@Composable
private fun RestLines(b: Breakdown, colors: ChartColors) {
    val parts = buildList {
        if (b.other > 0) add("Other: ${formatNumber(b.other)} in ${formatNumber(b.otherValues)} more values")
        if (b.missing > 0) add("Unknown: ${formatNumber(b.missing)}")
    }
    if (parts.isNotEmpty()) {
        Text(parts.joinToString(" · "), style = MaterialTheme.typography.labelSmall, color = colors.inkMuted, modifier = Modifier.padding(top = 4.dp))
    }
}

/**
 * Horizontal bars, most frequent first, one hue (magnitude): tap toggles a cross-filter on the
 * value, long press opens Search with it.
 */
@OptIn(ExperimentalFoundationApi::class)
@Composable
fun BreakdownBars(
    b: Breakdown,
    selected: Set<String>,
    colors: ChartColors,
    onToggle: (FacetCount) -> Unit,
    onOpen: (FacetCount) -> Unit,
) {
    val max = b.counts.maxOfOrNull { it.count }?.coerceAtLeast(1) ?: 1
    Column(Modifier.fillMaxWidth()) {
        if (b.counts.isEmpty()) Text("No values", style = MaterialTheme.typography.bodySmall, color = colors.inkMuted)
        for (c in b.counts) {
            val on = c.value in selected
            val label = valueLabel(b.field, c.value)
            val share = formatShare(c.count, b.total)
            Row(
                Modifier
                    .fillMaxWidth()
                    .heightIn(min = 40.dp)
                    .combinedClickable(role = Role.Checkbox, onClick = { onToggle(c) }, onLongClick = { onOpen(c) })
                    .semantics(mergeDescendants = true) {
                        this.selected = on
                        contentDescription = "$label: ${formatNumber(c.count)}, $share"
                    }
                    .testTag("bar_${b.field}_${c.value}"),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Text(
                    label,
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
data class Slice(val count: FacetCount?, val n: Long, val color: Color)

/** Slices of a fixed-vocabulary field: values past the colour slots fold into "Other". */
fun slices(b: Breakdown, order: List<String>, colors: ChartColors): List<Slice> {
    val out = mutableListOf<Slice>()
    var other = b.other
    for (c in b.counts) {
        val color = colors.slotOf(c.value, order)
        if (color == null) other += c.count else out += Slice(c, c.count, color)
    }
    if (other > 0) out += Slice(null, other, colors.other)
    return out
}

/** Index of the slice under [angleDeg] (0 = 12 o'clock, clockwise) of slices sized [sizes]. */
fun sliceAt(angleDeg: Float, sizes: List<Long>): Int? {
    val sum = sizes.sum().toDouble()
    if (sum <= 0) return null
    var a = angleDeg % 360f
    if (a < 0) a += 360f
    var start = 0.0
    sizes.forEachIndexed { i, n ->
        val sweep = n / sum * 360
        if (a < start + sweep) return i
        start += sweep
    }
    return null
}

/**
 * Donut for fields with a short fixed vocabulary ([DONUT_ORDER]); the legend carries label,
 * count and share of every slice (tap: cross-filter, long press: Search).
 */
@OptIn(ExperimentalFoundationApi::class)
@Composable
fun BreakdownDonut(
    b: Breakdown,
    selected: Set<String>,
    colors: ChartColors,
    onToggle: (FacetCount) -> Unit,
    onOpen: (FacetCount) -> Unit,
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
                    .semantics { contentDescription = "Donut of ${FIELD_TITLES[b.field] ?: b.field}; values listed beside it" }
                    .pointerInput(parts) {
                        fun hit(o: Offset): FacetCount? {
                            val c = Offset(size.width / 2f, size.height / 2f)
                            val r = min(size.width, size.height) / 2f
                            val d = hypot(o.x - c.x, o.y - c.y)
                            if (d < r * 0.45f || d > r) return null
                            val a = (atan2(o.y - c.y, o.x - c.x) * 180 / PI + 90).toFloat()
                            return sliceAt(a, parts.map { it.n })?.let { parts[it].count }
                        }
                        detectTapGestures(onTap = { o -> hit(o)?.let(toggle) }, onLongPress = { o -> hit(o)?.let(open) })
                    },
            ) {
                val stroke = size.minDimension * 0.22f
                val inset = stroke / 2
                val arcSize = Size(size.width - stroke, size.height - stroke)
                var start = -90f
                for (p in parts) {
                    val sweep = (p.n.toDouble() / sum * 360).toFloat()
                    val on = p.count?.value in selected
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
        Column(Modifier.weight(1f)) {
            for (p in parts) {
                val c = p.count
                val on = c?.value in selected
                val label = c?.let { valueLabel(b.field, it.value) } ?: "Other"
                Row(
                    Modifier
                        .fillMaxWidth()
                        .heightIn(min = 40.dp)
                        .then(
                            if (c != null) {
                                Modifier
                                    .combinedClickable(role = Role.Checkbox, onClick = { onToggle(c) }, onLongClick = { onOpen(c) })
                                    .testTag("slice_${b.field}_${c.value}")
                            } else {
                                Modifier
                            },
                        )
                        .semantics(mergeDescendants = true) {
                            this.selected = on
                            contentDescription = "$label: ${formatNumber(p.n)}, ${formatShare(p.n, b.total)}"
                        },
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
    if (b.missing > 0) {
        Text("Unknown: ${formatNumber(b.missing)}", style = MaterialTheme.typography.labelSmall, color = colors.inkMuted)
    }
}

/**
 * Years as columns in time order (a histogram: gaps stay visible). Tap toggles the year as a
 * cross-filter, long press opens Search; the peak and the selected years are written out.
 */
@Composable
fun YearColumns(
    b: Breakdown,
    selected: Set<String>,
    colors: ChartColors,
    onToggle: (FacetCount) -> Unit,
    onOpen: (FacetCount) -> Unit,
) {
    val years = remember(b) { b.counts.mapNotNull { c -> c.value.toIntOrNull()?.let { it to c } }.sortedBy { it.first } }
    if (years.isEmpty()) {
        Text("No values", style = MaterialTheme.typography.bodySmall, color = colors.inkMuted)
        return
    }
    val first = years.first().first
    val last = years.last().first
    val span = (last - first + 1).coerceAtLeast(1)
    val max = years.maxOf { it.second.count }.coerceAtLeast(1).toDouble()
    val byYear = years.associate { it.first to it.second }
    val toggle by rememberUpdatedState(onToggle)
    val open by rememberUpdatedState(onOpen)
    fun at(x: Float, width: Float): FacetCount? = byYear[first + (x / width * span).toInt().coerceIn(0, span - 1)]
    Column(Modifier.fillMaxWidth()) {
        Canvas(
            Modifier
                .fillMaxWidth()
                .height(120.dp)
                .testTag("year_chart")
                .semantics { contentDescription = years.joinToString { "${it.first}: ${formatNumber(it.second.count)}" } }
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
                val h = (c.count / max * (size.height - 4f)).toFloat().coerceAtLeast(1f)
                drawRect(
                    if (c.value in selected) colors.barSelected else colors.bar,
                    topLeft = Offset((y - first) * w + gap / 2, size.height - h),
                    size = Size((w - gap).coerceAtLeast(1f), h),
                )
            }
        }
        Row(Modifier.fillMaxWidth()) {
            Text("$first", style = MaterialTheme.typography.labelSmall, color = colors.inkMuted)
            Spacer(Modifier.weight(1f))
            val peak = years.maxBy { it.second.count }
            Text("peak ${peak.first}: ${formatNumber(peak.second.count)}", style = MaterialTheme.typography.labelSmall, color = colors.inkMuted)
            Spacer(Modifier.weight(1f))
            Text("$last", style = MaterialTheme.typography.labelSmall, color = colors.inkMuted)
        }
        val sel = years.filter { it.second.value in selected }
        if (sel.isNotEmpty()) {
            Text(
                sel.joinToString { "${it.first}: ${formatNumber(it.second.count)}" },
                style = MaterialTheme.typography.labelMedium,
                fontWeight = FontWeight.Bold,
            )
        }
        RestLines(b, colors)
    }
}

/** One part of a stacked status bar: label, count, colour. */
data class StatusPart(val label: String, val n: Long, val color: Color)

/** A stacked bar in the status colours, every part labelled with its number and share. */
@OptIn(ExperimentalLayoutApi::class)
@Composable
fun StatusBar(parts: List<StatusPart>, colors: ChartColors, modifier: Modifier = Modifier) {
    val sum = parts.sumOf { it.n }
    Column(modifier.fillMaxWidth()) {
        Canvas(
            Modifier
                .fillMaxWidth()
                .height(14.dp)
                .semantics { contentDescription = parts.joinToString { "${it.label} ${formatNumber(it.n)}" } },
        ) {
            var x = 0f
            for (p in parts) {
                val w = if (sum > 0) (p.n.toDouble() / sum * size.width).toFloat() else 0f
                if (w > 0f) drawRect(p.color, Offset(x, 0f), Size((w - 2f).coerceAtLeast(1f), size.height))
                x += w
            }
        }
        FlowRow(Modifier.fillMaxWidth().padding(top = 4.dp), horizontalArrangement = Arrangement.spacedBy(12.dp)) {
            for (p in parts) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Box(Modifier.size(10.dp).background(p.color, CircleShape))
                    Spacer(Modifier.width(4.dp))
                    Text("${p.label} ${formatCompact(p.n.toDouble())} (${formatShare(p.n, sum)})", style = MaterialTheme.typography.labelSmall, color = colors.ink)
                }
            }
        }
    }
}

/**
 * The visible part of a series of [n] points: [first] and [count] (at least [MIN] points, or all
 * of them). Pinch zooms around a focus point, drag pans; pure, so it is unit tested.
 */
data class ChartWindow(val n: Int, val first: Int = 0, val count: Int = n) {
    /** Zoom by [factor] (> 1 = in) keeping the point at [focus] (0–1 across the view) in place. */
    fun zoom(factor: Float, focus: Float): ChartWindow {
        if (n <= MIN || factor <= 0f) return this
        val newCount = (count / factor).toInt().coerceIn(minOf(MIN, n), n)
        val anchor = first + focus.coerceIn(0f, 1f) * count
        val newFirst = (anchor - focus.coerceIn(0f, 1f) * newCount).toInt()
        return copy(count = newCount).withFirst(newFirst)
    }

    /** Pan by [fraction] of the view (positive = towards older points). */
    fun pan(fraction: Float): ChartWindow = withFirst(first - (fraction * count).toInt())

    private fun withFirst(f: Int) = copy(first = f.coerceIn(0, (n - count).coerceAtLeast(0)))

    /** Point index under [x] (0–1 across the view). */
    fun indexAt(x: Float): Int = (first + (x.coerceIn(0f, 0.9999f) * count).toInt()).coerceIn(0, (n - 1).coerceAtLeast(0))

    companion object {
        const val MIN = 7
    }
}

fun seriesValue(v: Long, unit: ValueUnit): String = when (unit) {
    ValueUnit.COUNT -> formatNumber(v)
    ValueUnit.BYTES -> formatBytes(v)
}

private fun axisValue(v: Double, unit: ValueUnit): String = when (unit) {
    ValueUnit.COUNT -> formatCompact(v)
    ValueUnit.BYTES -> formatBytes(v.toLong())
}

/**
 * One activity series (D47): counters as columns (one per hour or day), gauges as a line. Pinch
 * to zoom, drag to pan, tap or press for a value. Day buckets are UTC days.
 */
@Composable
fun ActivityChart(series: Timeseries, unit: ValueUnit, colors: ChartColors, modifier: Modifier = Modifier) {
    val points = series.points
    if (points.isEmpty()) {
        Text("No data yet", style = MaterialTheme.typography.bodySmall, color = colors.inkMuted)
        return
    }
    val hourly = series.bucket == "hour"
    val counter = series.kind == "counter"
    var window by remember(series) { mutableStateOf(ChartWindow(points.size)) }
    var picked by remember(series) { mutableStateOf<Int?>(null) }
    val w = window
    val visible = points.subList(w.first, (w.first + w.count).coerceAtMost(points.size))
    val top = (visible.maxOfOrNull { it.value } ?: 0).coerceAtLeast(1)
    val bottom = if (counter) 0L else (visible.minOfOrNull { it.value } ?: 0).let { lo -> if (lo == top) 0 else lo }
    val color = colors.categorical[0]
    Column(modifier) {
        Row {
            Text(axisValue(top.toDouble(), unit), style = MaterialTheme.typography.labelSmall, color = colors.inkMuted, modifier = Modifier.weight(1f))
            val p = picked?.let { points.getOrNull(it) }
            Text(
                if (p != null) "${bucketLabel(p.at, hourly)}: ${seriesValue(p.value, unit)}" else "latest ${seriesValue(points.last().value, unit)}",
                style = MaterialTheme.typography.labelMedium,
                fontWeight = if (p != null) FontWeight.Bold else null,
                modifier = Modifier.testTag("activity_value"),
            )
        }
        Canvas(
            Modifier
                .fillMaxWidth()
                .height(160.dp)
                .testTag("activity_chart")
                .semantics {
                    contentDescription = "${series.metric}: ${points.size} ${if (hourly) "hours" else "days"}, " +
                        "latest ${seriesValue(points.last().value, unit)}, highest shown ${seriesValue(top, unit)}"
                }
                // Pinch (two fingers) zooms and a horizontal drag pans; a vertical drag is left
                // to the page, so the chart never traps the list's scroll.
                .pointerInput(series) {
                    awaitEachGesture {
                        awaitFirstDown(requireUnconsumed = false)
                        do {
                            val event = awaitPointerEvent()
                            if (event.changes.count { it.pressed } >= 2) {
                                val zoom = event.calculateZoom()
                                if (zoom != 1f) {
                                    window = window.zoom(zoom, event.calculateCentroid().x / size.width)
                                    event.changes.forEach { it.consume() }
                                }
                            }
                        } while (event.changes.any { it.pressed })
                    }
                }
                .pointerInput(series) {
                    detectHorizontalDragGestures { change, dx ->
                        change.consume()
                        window = window.pan(dx / size.width)
                    }
                }
                .pointerInput(series) {
                    detectTapGestures(
                        onTap = { o -> picked = window.indexAt(o.x / size.width) },
                        onLongPress = { o -> picked = window.indexAt(o.x / size.width) },
                    )
                },
        ) {
            val n = visible.size
            val cw = size.width / n
            fun y(v: Long) = size.height - ((v - bottom).toFloat() / (top - bottom).coerceAtLeast(1)) * (size.height - 4f) - 2f
            drawLine(colors.axis, Offset(0f, size.height - 1f), Offset(size.width, size.height - 1f), 2f)
            drawLine(colors.grid, Offset(0f, y(top)), Offset(size.width, y(top)), 1f)
            val sel = picked?.minus(w.first)
            if (counter) {
                val gap = if (cw > 4f) cw * 0.2f else 0f
                visible.forEachIndexed { i, p ->
                    val top0 = y(p.value)
                    drawRect(
                        if (i == sel) colors.barSelected else color,
                        topLeft = Offset(i * cw + gap / 2, top0),
                        size = Size((cw - gap).coerceAtLeast(1f), (size.height - top0).coerceAtLeast(1f)),
                    )
                }
            } else {
                val path = Path()
                val area = Path()
                visible.forEachIndexed { i, p ->
                    val x = i * cw + cw / 2
                    if (i == 0) {
                        path.moveTo(x, y(p.value))
                        area.moveTo(x, size.height)
                    } else {
                        path.lineTo(x, y(p.value))
                    }
                    area.lineTo(x, y(p.value))
                }
                area.lineTo((n - 1) * cw + cw / 2, size.height)
                area.close()
                drawPath(area, color.copy(alpha = 0.15f))
                drawPath(path, color, style = Stroke(width = 4f))
                if (sel != null && sel in 0 until n) {
                    drawCircle(colors.ink, 6f, Offset(sel * cw + cw / 2, y(visible[sel].value)))
                }
            }
            if (sel != null && sel in 0 until n) {
                drawLine(colors.ink.copy(alpha = 0.4f), Offset(sel * cw + cw / 2, 0f), Offset(sel * cw + cw / 2, size.height), 1f)
            }
        }
        Row {
            Text(bucketLabel(visible.first().at, hourly), style = MaterialTheme.typography.labelSmall, color = colors.inkMuted, modifier = Modifier.weight(1f))
            if (w.count < w.n) Text("zoomed: ${w.count} of ${w.n}", style = MaterialTheme.typography.labelSmall, color = colors.inkMuted)
            Spacer(Modifier.weight(1f))
            Text(bucketLabel(visible.last().at, hourly), style = MaterialTheme.typography.labelSmall, color = colors.inkMuted)
        }
    }
}

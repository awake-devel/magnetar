package app.magnetar.ui.charts

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import app.magnetar.ffi.FfiLivenessPoint
import app.magnetar.ffi.FfiTimeseries
import app.magnetar.formatBytes
import app.magnetar.formatCompact
import app.magnetar.formatNumber
import app.magnetar.ui.theme.ChartColors
import com.patrykandpatrick.vico.compose.cartesian.CartesianChartHost
import com.patrykandpatrick.vico.compose.cartesian.CartesianDrawingContext
import com.patrykandpatrick.vico.compose.cartesian.Zoom
import com.patrykandpatrick.vico.compose.cartesian.axis.HorizontalAxis
import com.patrykandpatrick.vico.compose.cartesian.axis.VerticalAxis
import com.patrykandpatrick.vico.compose.cartesian.axis.rememberAxisGuidelineComponent
import com.patrykandpatrick.vico.compose.cartesian.axis.rememberAxisLabelComponent
import com.patrykandpatrick.vico.compose.cartesian.axis.rememberAxisLineComponent
import com.patrykandpatrick.vico.compose.cartesian.data.CartesianChartModel
import com.patrykandpatrick.vico.compose.cartesian.data.CartesianValueFormatter
import com.patrykandpatrick.vico.compose.cartesian.data.ColumnCartesianLayerModel
import com.patrykandpatrick.vico.compose.cartesian.data.LineCartesianLayerModel
import com.patrykandpatrick.vico.compose.cartesian.layer.ColumnCartesianLayer
import com.patrykandpatrick.vico.compose.cartesian.layer.LineCartesianLayer
import com.patrykandpatrick.vico.compose.cartesian.layer.rememberColumnCartesianLayer
import com.patrykandpatrick.vico.compose.cartesian.layer.rememberLine
import com.patrykandpatrick.vico.compose.cartesian.layer.rememberLineCartesianLayer
import com.patrykandpatrick.vico.compose.cartesian.marker.rememberDefaultCartesianMarker
import com.patrykandpatrick.vico.compose.cartesian.rememberCartesianChart
import com.patrykandpatrick.vico.compose.cartesian.rememberVicoScrollState
import com.patrykandpatrick.vico.compose.cartesian.rememberVicoZoomState
import com.patrykandpatrick.vico.compose.common.Fill
import com.patrykandpatrick.vico.compose.common.component.rememberLineComponent
import com.patrykandpatrick.vico.compose.common.component.rememberTextComponent
import java.time.Instant
import java.time.ZoneId
import java.time.ZoneOffset
import java.time.format.DateTimeFormatter
import java.time.format.FormatStyle
import java.util.Locale

/** What a metric's values are, for its labels. */
enum class ValueUnit { COUNT, BYTES }

/**
 * Bucket start → short date ("12 oct.", "Oct 12") or hour ("14:00") in [zone]. Day buckets of
 * the activity series are UTC days, so they are labelled in UTC (a local zone west of UTC
 * would name the day before); hours and history points use the device's zone.
 */
fun bucketLabel(
    at: Long,
    hourly: Boolean,
    locale: Locale = Locale.getDefault(),
    zone: ZoneId = ZoneId.systemDefault(),
): String {
    val t = Instant.ofEpochSecond(at).atZone(zone)
    return if (hourly) {
        DateTimeFormatter.ofPattern("HH:mm", locale).format(t)
    } else {
        DateTimeFormatter.ofPattern(if (locale.language == "fr") "d MMM" else "MMM d", locale).format(t)
    }
}

private fun valueLabel(v: Double, unit: ValueUnit): String = when (unit) {
    ValueUnit.COUNT -> formatCompact(v)
    ValueUnit.BYTES -> formatBytes(v.coerceAtLeast(0.0).toULong())
}

/**
 * One activity series (D47): counters as columns (one per hour or day), gauges as a line.
 * Pinch to zoom, press to read a value. x is the bucket's index, labelled with its date.
 */
@Composable
fun ActivityChart(series: FfiTimeseries, unit: ValueUnit, colors: ChartColors, modifier: Modifier = Modifier) {
    val points = series.points
    if (points.isEmpty()) {
        Text("No data yet", style = MaterialTheme.typography.bodySmall, color = colors.inkMuted)
        return
    }
    val hourly = series.bucket == "hour"
    val counter = series.kind == "counter"
    val zone = if (hourly) ZoneId.systemDefault() else ZoneOffset.UTC
    val model = remember(series) {
        val x = points.indices.toList()
        val y = points.map { it.value }
        CartesianChartModel(
            if (counter) {
                ColumnCartesianLayerModel.build { series(x, y) }
            } else {
                LineCartesianLayerModel.build { series(x, y) }
            },
        )
    }
    val labelStyle = TextStyle(color = colors.inkMuted, fontSize = 11.sp)
    val bottom = HorizontalAxis.rememberBottom(
        label = rememberAxisLabelComponent(style = labelStyle),
        line = rememberAxisLineComponent(fill = Fill(colors.axis)),
        guideline = null,
        valueFormatter = remember(series) {
            CartesianValueFormatter { _, x, _ ->
                points.getOrNull(x.toInt())?.let { bucketLabel(it.at, hourly, zone = zone) } ?: " "
            }
        },
        itemPlacer = remember(points.size) {
            HorizontalAxis.ItemPlacer.aligned(spacing = { (points.size / 4).coerceAtLeast(1) })
        },
    )
    val start = VerticalAxis.rememberStart(
        label = rememberAxisLabelComponent(style = labelStyle),
        line = null,
        guideline = rememberAxisGuidelineComponent(fill = Fill(colors.grid)),
        valueFormatter = remember(unit) { CartesianValueFormatter { _, v, _ -> valueLabel(v, unit) } },
    )
    val marker = rememberDefaultCartesianMarker(
        label = rememberTextComponent(style = TextStyle(color = colors.ink, fontSize = 12.sp)),
        valueFormatter = remember(series, unit) {
            com.patrykandpatrick.vico.compose.cartesian.marker.DefaultCartesianMarker.ValueFormatter { _, targets ->
                val i = targets.firstOrNull()?.x?.toInt() ?: return@ValueFormatter ""
                val p = points.getOrNull(i) ?: return@ValueFormatter ""
                bucketLabel(p.at, hourly, zone = zone) + ": " + when (unit) {
                    ValueUnit.COUNT -> formatNumber(p.value)
                    ValueUnit.BYTES -> formatBytes(p.value.coerceAtLeast(0).toULong())
                }
            }
        },
    )
    val layer = if (counter) {
        rememberColumnCartesianLayer(
            columnProvider = ColumnCartesianLayer.ColumnProvider.series(
                rememberLineComponent(fill = Fill(colors.categorical[0]), thickness = 6.dp),
            ),
        )
    } else {
        rememberLineCartesianLayer(
            lineProvider = LineCartesianLayer.LineProvider.series(
                LineCartesianLayer.rememberLine(
                    fill = remember(colors) { LineCartesianLayer.LineFill.single(Fill(colors.categorical[0])) },
                    areaFill = remember(colors) {
                        LineCartesianLayer.AreaFill.single(Fill(colors.categorical[0].copy(alpha = 0.15f)))
                    },
                ),
            ),
        )
    }
    val last = points.last()
    Column(modifier) {
        CartesianChartHost(
            chart = rememberCartesianChart(layer, startAxis = start, bottomAxis = bottom, marker = marker),
            model = model,
            modifier = Modifier
                .fillMaxWidth()
                .height(180.dp)
                .semantics {
                    contentDescription = "${series.metric}: latest ${valueLabel(last.value.toDouble(), unit)}, " +
                        "${points.size} ${if (hourly) "hours" else "days"}"
                },
            scrollState = rememberVicoScrollState(scrollEnabled = true),
            zoomState = rememberVicoZoomState(
                zoomEnabled = true,
                initialZoom = remember { Zoom.Content },
                minZoom = remember { Zoom.Content },
            ),
        )
    }
}

/** Holds each value until the next point: liveness history is a step line (D45). */
private object StepInterpolator : LineCartesianLayer.Interpolator {
    override fun interpolate(
        context: CartesianDrawingContext,
        path: Path,
        points: List<Offset>,
        visibleIndexRange: IntRange,
    ) {
        for (i in visibleIndexRange) {
            val p = points[i]
            if (i == visibleIndexRange.first) {
                path.moveTo(p.x, p.y)
            } else {
                path.lineTo(p.x, points[i - 1].y)
                path.lineTo(p.x, p.y)
            }
        }
    }
}

/**
 * Seeders and leechers of one torrent over time (D45 history), as step lines on a time axis
 * (x = days from the first point), with a legend: two series, so colour is never alone.
 */
@Composable
fun SwarmHistoryChart(points: List<FfiLivenessPoint>, colors: ChartColors, modifier: Modifier = Modifier) {
    // x in whole hours since the first point (Vico wants x on its step); one point per hour.
    val answered = points.filter { it.seeders != null || it.leechers != null }
    val base = answered.firstOrNull()?.at ?: 0L
    val usable = answered.distinctBy { (it.at - base) / 3600 }
    if (usable.size < 2) {
        Text(
            if (usable.isEmpty()) "No history yet" else "One check so far; the chart starts with the next one",
            style = MaterialTheme.typography.bodySmall,
            color = colors.inkMuted,
        )
        return
    }
    val t0 = base
    val seedColor = colors.categorical[0]
    val leechColor = colors.categorical[1]
    val model = remember(usable) {
        val x = usable.map { (it.at - t0) / 3600 }
        CartesianChartModel(
            LineCartesianLayerModel.build {
                series(x, usable.map { (it.seeders ?: 0u).toLong() })
                series(x, usable.map { (it.leechers ?: 0u).toLong() })
            },
        )
    }
    val labelStyle = TextStyle(color = colors.inkMuted, fontSize = 11.sp)
    val line = { c: Color ->
        LineCartesianLayer.LineFill.single(Fill(c))
    }
    val layer = rememberLineCartesianLayer(
        lineProvider = LineCartesianLayer.LineProvider.series(
            LineCartesianLayer.rememberLine(fill = remember(seedColor) { line(seedColor) }, interpolator = StepInterpolator),
            LineCartesianLayer.rememberLine(fill = remember(leechColor) { line(leechColor) }, interpolator = StepInterpolator),
        ),
    )
    val hourFmt = remember(t0) {
        CartesianValueFormatter { _, x, _ -> bucketLabel(t0 + (x * 3600).toLong(), hourly = false) }
    }
    val marker = rememberDefaultCartesianMarker(
        label = rememberTextComponent(style = TextStyle(color = colors.ink, fontSize = 12.sp)),
    )
    val last = usable.last()
    Column(modifier) {
        CartesianChartHost(
            chart = rememberCartesianChart(
                layer,
                startAxis = VerticalAxis.rememberStart(
                    label = rememberAxisLabelComponent(style = labelStyle),
                    line = null,
                    guideline = rememberAxisGuidelineComponent(fill = Fill(colors.grid)),
                    valueFormatter = remember { CartesianValueFormatter { _, v, _ -> formatCompact(v) } },
                ),
                bottomAxis = HorizontalAxis.rememberBottom(
                    label = rememberAxisLabelComponent(style = labelStyle),
                    line = rememberAxisLineComponent(fill = Fill(colors.axis)),
                    guideline = null,
                    valueFormatter = hourFmt,
                    itemPlacer = remember { HorizontalAxis.ItemPlacer.segmented() },
                ),
                marker = marker,
                getXStep = { _, _, _ -> 1.0 },
            ),
            model = model,
            modifier = Modifier
                .fillMaxWidth()
                .height(160.dp)
                .semantics {
                    contentDescription = "Seeders and leechers over ${usable.size} checks; latest " +
                        "${last.seeders ?: "?"} seeders, ${last.leechers ?: "?"} leechers"
                },
            scrollState = rememberVicoScrollState(scrollEnabled = false),
            zoomState = rememberVicoZoomState(
                zoomEnabled = false,
                initialZoom = remember { Zoom.Content },
                minZoom = remember { Zoom.Content },
            ),
        )
        Row(horizontalArrangement = Arrangement.spacedBy(16.dp), verticalAlignment = Alignment.CenterVertically) {
            Legend("Seeders", seedColor)
            Legend("Leechers", leechColor)
        }
    }
}

@Composable
private fun Legend(label: String, color: Color) {
    Row(verticalAlignment = Alignment.CenterVertically) {
        Box(Modifier.size(10.dp).background(color, CircleShape))
        Spacer(Modifier.width(4.dp))
        Text(label, style = MaterialTheme.typography.labelSmall)
    }
}

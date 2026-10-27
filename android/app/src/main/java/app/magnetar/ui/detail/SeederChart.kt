package app.magnetar.ui.detail

import androidx.compose.foundation.Canvas
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import app.magnetar.api.LivenessPoint
import app.magnetar.ui.common.formatDate
import app.magnetar.ui.theme.chartColors

/** A step series: (time, value) points, each value held until the next point (D45). */
data class StepSeries(val points: List<Pair<Long, Int>>) {
    val max: Int get() = points.maxOfOrNull { it.second } ?: 0
}

/**
 * Seeders (max of tracker seeders and DHT estimate, as rows show them) and leechers of a
 * history; a point where a source did not answer is skipped, not drawn as 0.
 */
fun seederSeries(points: List<LivenessPoint>): Pair<StepSeries, StepSeries> {
    val seeders = points.mapNotNull { p ->
        val best = if (p.seeders == null && p.peersEst == null) null else maxOf(p.seeders ?: 0, p.peersEst ?: 0)
        best?.let { p.at to it }
    }
    val leechers = points.mapNotNull { p -> p.leechers?.let { p.at to it } }
    return StepSeries(seeders) to StepSeries(leechers)
}

/** Step chart of the seed history (Compose Canvas, no chart library in this stage). */
@Composable
fun SeederHistoryChart(points: List<LivenessPoint>, nowSec: Long = System.currentTimeMillis() / 1000) {
    val colors = chartColors()
    val (seeders, leechers) = seederSeries(points)
    if (seeders.points.isEmpty() && leechers.points.isEmpty()) {
        Text("No seed checks recorded yet", style = MaterialTheme.typography.bodySmall)
        return
    }
    val start = points.first().at
    val end = maxOf(nowSec, points.last().at + 1)
    val top = maxOf(seeders.max, leechers.max, 1)
    val last = seeders.points.lastOrNull()?.second
    val seedColor = colors.categorical[0]
    val leechColor = colors.categorical[1]
    Column(Modifier.padding(top = 8.dp)) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text("max $top", style = MaterialTheme.typography.labelSmall, color = colors.inkMuted, modifier = Modifier.weight(1f))
            Legend(seedColor, "seeders")
            Spacer(Modifier.width(12.dp))
            Legend(leechColor, "leechers")
        }
        Canvas(
            Modifier.fillMaxWidth().height(120.dp).testTag("seeder_chart").semantics {
                contentDescription = "Seeders over time: ${seeders.points.size} checks, at most $top" +
                    (last?.let { ", latest $it" } ?: "")
            },
        ) {
            val w = size.width
            val h = size.height
            fun x(t: Long) = ((t - start).toFloat() / (end - start).toFloat()) * w
            fun y(v: Int) = h - (v.toFloat() / top.toFloat()) * (h - 4f) - 2f
            drawLine(colors.axis, Offset(0f, h - 1f), Offset(w, h - 1f), strokeWidth = 2f)
            drawLine(colors.grid, Offset(0f, y(top)), Offset(w, y(top)), strokeWidth = 1f)
            fun drawStep(s: StepSeries, color: Color) {
                if (s.points.isEmpty()) return
                val path = Path()
                s.points.forEachIndexed { i, (t, v) ->
                    if (i == 0) path.moveTo(x(t), y(v)) else path.lineTo(x(t), y(v))
                    val next = s.points.getOrNull(i + 1)?.first ?: end
                    path.lineTo(x(next), y(v))
                }
                drawPath(path, color, style = Stroke(width = 4f))
            }
            drawStep(leechers, leechColor)
            drawStep(seeders, seedColor)
        }
        Row {
            Text(formatDate(start), style = MaterialTheme.typography.labelSmall, color = colors.inkMuted, modifier = Modifier.weight(1f))
            Text("now", style = MaterialTheme.typography.labelSmall, color = colors.inkMuted)
        }
    }
}

@Composable
private fun Legend(color: Color, label: String) {
    Row(verticalAlignment = Alignment.CenterVertically) {
        Canvas(Modifier.size(10.dp)) { drawRect(color) }
        Spacer(Modifier.width(4.dp))
        Text(label, style = MaterialTheme.typography.labelSmall)
    }
}

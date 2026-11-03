package app.magnetar.ui.stats

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import app.magnetar.api.FleetStats
import app.magnetar.api.WorkerStats
import app.magnetar.ui.common.cachedAgo
import app.magnetar.ui.theme.ChartColors

/**
 * Header probes (D117, D118): the backfill queue's progress (probed against what is left to
 * try), its states and closed outcomes, then one block per worker with its badges and backfill
 * counters. Shown only when the server sends `probe_backfill`.
 */
@Composable
internal fun ProbesContent(fleet: FleetStats, colors: ChartColors, cachedAtMs: Long?) {
    val b = fleet.probeBackfill ?: return
    cachedAtMs?.let { Text(cachedAgo(it), style = MaterialTheme.typography.labelSmall, color = colors.inkMuted) }
    val p = backfillProgress(b)
    Row(Modifier.fillMaxWidth().padding(top = 4.dp), verticalAlignment = Alignment.CenterVertically) {
        Text("Backfill", Modifier.weight(1f), style = MaterialTheme.typography.labelSmall, color = colors.inkMuted)
        Text(backfillPercent(p), style = MaterialTheme.typography.labelSmall, modifier = Modifier.testTag("probes_percent"))
    }
    LinearProgressIndicator(
        progress = { p.fraction },
        modifier = Modifier.fillMaxWidth().padding(vertical = 4.dp).testTag("probes_progress")
            .semantics { contentDescription = "Backfill " + backfillPercent(p) },
        color = colors.good,
        trackColor = colors.unknown.copy(alpha = 0.35f),
    )
    for ((k, v) in backfillLines(b)) Line(k, v)
    val workers = fleet.workers
    if (workers.isNotEmpty()) {
        Text("Workers", Modifier.padding(top = 8.dp), style = MaterialTheme.typography.labelSmall, color = colors.inkMuted)
        for (w in workers.sortedBy { it.name }) WorkerBlock(w, colors)
    }
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun WorkerBlock(w: WorkerStats, colors: ChartColors) {
    Column(Modifier.fillMaxWidth().padding(top = 6.dp).testTag("probes_worker_${w.name}")) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text(w.name.ifBlank { "?" }, Modifier.weight(1f), style = MaterialTheme.typography.bodySmall, fontWeight = FontWeight.Bold)
            Box(Modifier.size(8.dp).background(if (w.connected) colors.good else colors.unknown, CircleShape))
            Spacer(Modifier.width(4.dp))
            Text(if (w.connected) "connected" else "not connected", style = MaterialTheme.typography.labelSmall)
        }
        val badges = workerBadges(w)
        if (badges.isNotEmpty()) {
            FlowRow(Modifier.padding(top = 2.dp), horizontalArrangement = Arrangement.spacedBy(4.dp)) {
                for (badge in badges) {
                    Box(
                        Modifier
                            .border(1.dp, MaterialTheme.colorScheme.outline, RoundedCornerShape(8.dp))
                            .padding(horizontal = 6.dp)
                            .testTag("probes_badge_${w.name}_${badge.replace(' ', '_')}"),
                    ) { Text(badge, style = MaterialTheme.typography.labelSmall) }
                }
            }
        }
        val lines = workerBackfillLines(w)
        if (lines.isEmpty()) {
            Text("no backfill", style = MaterialTheme.typography.labelSmall, color = colors.inkMuted)
        } else {
            for ((k, v) in lines) Line(k, v)
        }
    }
}

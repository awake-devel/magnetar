package app.magnetar.ui.detail

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Card
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import app.magnetar.api.ProbeInfo

/** "Detected": what a worker read from the files themselves (D117, D121). */
@Composable
fun ProbeCard(p: ProbeInfo, files: List<ProbeFileView>) {
    Card(Modifier.fillMaxWidth().padding(top = 12.dp).testTag("probe_card")) {
        Column(Modifier.padding(12.dp)) {
            Text("Detected", style = MaterialTheme.typography.titleSmall)
            Text(
                "Read from the first blocks of each file, not from its name.",
                style = MaterialTheme.typography.bodySmall,
            )
            Text(probeSummary(p), style = MaterialTheme.typography.labelSmall)
            files.forEachIndexed { i, f ->
                if (i > 0) HorizontalDivider(Modifier.padding(top = 8.dp))
                Text(
                    f.name,
                    style = MaterialTheme.typography.bodyMedium,
                    fontWeight = FontWeight.Medium,
                    modifier = Modifier.padding(top = 8.dp),
                )
                Text(
                    f.headline,
                    style = MaterialTheme.typography.labelSmall,
                    color = if (f.verified) MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.onSurfaceVariant,
                )
                f.warnings.forEach {
                    Text("⚠ $it", style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.error)
                }
                f.lines.forEach { Text(it, style = MaterialTheme.typography.bodySmall) }
            }
        }
    }
}

package app.magnetar.ui.detail

import android.content.ActivityNotFoundException
import android.content.Intent
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.material3.Card
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.core.net.toUri
import app.magnetar.api.MediaInfo
import app.magnetar.ui.common.Poster
import app.magnetar.ui.common.formatRating
import app.magnetar.ui.common.mediaHeadline
import app.magnetar.ui.common.mediaKindLabel
import java.util.Locale

private fun votesText(v: Long): String = when {
    v >= 1_000_000 -> String.format(Locale.ROOT, "%.1fM", v / 1_000_000.0)
    v >= 1_000 -> String.format(Locale.ROOT, "%.0fk", v / 1_000.0)
    else -> v.toString()
}

/** "S01E02 · The Kingsroad", "Season 1", or null. */
fun seasonEpisodeText(m: MediaInfo): String? {
    val s = m.season ?: return null
    val e = m.episode
    val code = if (e != null && e > 0) String.format(Locale.ROOT, "S%02dE%02d", s, e) else "Season $s"
    return listOfNotNull(code, m.episodeTitle).joinToString(" · ")
}

/** "Movie · ★ 7.4 (812k) · 155 min". */
fun mediaFacts(m: MediaInfo): String = listOfNotNull(
    mediaKindLabel(m.kind),
    m.rating?.let { r -> formatRating(r) + (m.votes?.let { " (${votesText(it)})" } ?: "") },
    m.runtimeMin?.takeIf { it > 0 }?.let { "$it min" },
).joinToString(" · ")

/** The identified film/series/anime (D40): 2:3 poster, facts, genres, overview, "All releases". */
@OptIn(ExperimentalLayoutApi::class)
@Composable
fun MediaCard(m: MediaInfo, onOpenMedia: ((Long) -> Unit)?, modifier: Modifier = Modifier) {
    val context = LocalContext.current
    var overviewOpen by remember(m.mediaId) { mutableStateOf(false) }
    Card(modifier.fillMaxWidth().padding(top = 12.dp).testTag("media_card")) {
        Column(Modifier.padding(12.dp)) {
            Row {
                if (m.poster != null) {
                    Poster(m.poster, "Poster of ${m.title}")
                    Spacer(Modifier.width(12.dp))
                }
                Column(Modifier.weight(1f)) {
                    Text(mediaHeadline(m.title, m.year, m.endYear), style = MaterialTheme.typography.titleMedium)
                    m.originalTitle?.takeIf { it != m.title }?.let { Text(it, style = MaterialTheme.typography.bodySmall) }
                    Text(mediaFacts(m), style = MaterialTheme.typography.bodyMedium)
                    if (m.genres.isNotEmpty()) Text(m.genres.joinToString(", "), style = MaterialTheme.typography.bodySmall)
                    seasonEpisodeText(m)?.let { Text(it, style = MaterialTheme.typography.bodyMedium) }
                }
            }
            m.overview?.takeIf { it.isNotBlank() }?.let { o ->
                Text(
                    o,
                    style = MaterialTheme.typography.bodyMedium,
                    maxLines = if (overviewOpen) Int.MAX_VALUE else 4,
                    overflow = TextOverflow.Ellipsis,
                    modifier = Modifier.padding(top = 8.dp).clickable { overviewOpen = !overviewOpen },
                )
            }
            FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp), modifier = Modifier.padding(top = 8.dp)) {
                m.imdbUrl?.let { url ->
                    OutlinedButton(onClick = {
                        try {
                            context.startActivity(Intent(Intent.ACTION_VIEW, url.toUri()))
                        } catch (_: ActivityNotFoundException) {
                        }
                    }) { Text("IMDb") }
                }
                // Every release of this title grouped by quality and language (D44).
                if (onOpenMedia != null) {
                    TextButton(onClick = { onOpenMedia(m.mediaId) }, modifier = Modifier.testTag("all_releases")) {
                        Text("All releases")
                    }
                }
            }
        }
    }
}

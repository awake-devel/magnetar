package app.magnetar.ui.detail

import android.content.ActivityNotFoundException
import android.content.Intent
import android.graphics.BitmapFactory
import android.net.Uri
import androidx.compose.foundation.Image
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Card
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import app.magnetar.ffi.FfiMediaInfo
import app.magnetar.ffi.FfiMediaKind
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext

fun mediaKindLabel(k: FfiMediaKind): String = when (k) {
    FfiMediaKind.MOVIE -> "Movie"
    FfiMediaKind.SERIES -> "Series"
    FfiMediaKind.ANIME -> "Anime"
}

/** "Title (2011)" or "Title (2011–2019)". */
fun mediaHeadline(title: String, year: Int?, endYear: Int? = null): String = when {
    year == null -> title
    endYear != null && endYear != year -> "$title ($year–$endYear)"
    else -> "$title ($year)"
}

private fun votesText(v: UInt): String = when {
    v >= 1_000_000u -> "%.1fM".format(v.toDouble() / 1_000_000)
    v >= 1_000u -> "%.0fk".format(v.toDouble() / 1_000)
    else -> v.toString()
}

/** "S01E02 · The Kingsroad", "Season 1", or null. */
fun seasonEpisodeText(m: FfiMediaInfo): String? {
    val s = m.season ?: return null
    val e = m.episode
    val code = if (e != null && e > 0) "S%02dE%02d".format(s, e) else "Season $s"
    return listOfNotNull(code, m.episodeTitle).joinToString(" · ")
}

@Composable
private fun Poster(path: String?, modifier: Modifier = Modifier) {
    var bitmap by remember(path) { mutableStateOf<ImageBitmap?>(null) }
    LaunchedEffect(path) {
        bitmap = path?.let {
            withContext(Dispatchers.IO) { runCatching { BitmapFactory.decodeFile(it)?.asImageBitmap() }.getOrNull() }
        }
    }
    bitmap?.let {
        Image(
            bitmap = it,
            contentDescription = "Poster",
            contentScale = ContentScale.Crop,
            modifier = modifier.width(100.dp).aspectRatio(2f / 3f).clip(RoundedCornerShape(6.dp)),
        )
    }
}

/** Identified film/series/anime (D40) with TMDB overview + poster served by our server (D42). */
@OptIn(ExperimentalLayoutApi::class)
@Composable
fun MediaCard(
    m: FfiMediaInfo,
    posterPath: String?,
    onOpenMedia: (Long) -> Unit,
) {
    val context = LocalContext.current
    var overviewOpen by remember(m.mediaId) { mutableStateOf(false) }
    Card(Modifier.fillMaxWidth().padding(top = 12.dp)) {
        Column(Modifier.padding(12.dp)) {
            Row {
                Poster(posterPath)
                if (posterPath != null) Spacer(Modifier.width(12.dp))
                Column(Modifier.weight(1f)) {
                    Text(mediaHeadline(m.title, m.year, m.endYear), style = MaterialTheme.typography.titleMedium)
                    m.originalTitle?.takeIf { it != m.title }?.let {
                        Text(it, style = MaterialTheme.typography.bodySmall)
                    }
                    Text(
                        listOfNotNull(
                            mediaKindLabel(m.kind),
                            m.rating?.let { r -> "★ %.1f".format(r) + (m.votes?.let { " (${votesText(it)})" } ?: "") },
                            m.runtimeMin?.takeIf { it > 0 }?.let { "$it min" },
                        ).joinToString(" · "),
                        style = MaterialTheme.typography.bodyMedium,
                    )
                    if (m.genres.isNotEmpty()) {
                        Text(m.genres.joinToString(", "), style = MaterialTheme.typography.bodySmall)
                    }
                    seasonEpisodeText(m)?.let { Text(it, style = MaterialTheme.typography.bodyMedium) }
                }
            }
            m.overview?.let { o ->
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
                            context.startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(url)))
                        } catch (_: ActivityNotFoundException) {
                        }
                    }) { Text("IMDb") }
                }
                // Every release of this title grouped by quality and language (D44).
                TextButton(onClick = { onOpenMedia(m.mediaId) }) { Text("All releases") }
            }
        }
    }
}

package app.magnetar.ui.common

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Column
import androidx.compose.material3.ListItem
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.text.style.TextOverflow
import app.magnetar.api.TorrentSummary

/** "Dune: Part Two (2024) · ★ 8.5" for a hit linked to a title (D40), else null. */
fun mediaLine(hit: TorrentSummary): String? {
    val t = hit.mediaTitle ?: return null
    return listOfNotNull(mediaHeadline(t, hit.mediaYear), hit.mediaRating?.let { formatRating(it) }).joinToString(" · ")
}

/** "4.37 GiB · 12 S / 3 L · dead · 5d". */
fun statsLine(hit: TorrentSummary): String = listOfNotNull(
    formatBytes(hit.totalSize),
    peersText(hit.seeders, hit.leechers, hit.bestPeers),
    if (hit.dead == true) "dead" else null,
    formatAge(hit.createdAt),
).joinToString(" · ")

/**
 * One result (docs/APP.md §1): media line (tap = title page), title, size/peers/age, the
 * release attributes, the file the text matched in, and the source badge.
 */
@Composable
fun ResultRow(
    hit: TorrentSummary,
    threshold: Int,
    onOpen: (String) -> Unit,
    onOpenMedia: ((Long) -> Unit)?,
    modifier: Modifier = Modifier,
    showMedia: Boolean = true,
) {
    ListItem(
        modifier = modifier.clickable { onOpen(hit.infohash) }.testTag("result"),
        leadingContent = { CategoryIcon(hit.category) },
        overlineContent = mediaLine(hit)?.takeIf { showMedia }?.let { line ->
            {
                val id = hit.mediaId
                val link = id != null && onOpenMedia != null
                Text(
                    line + if (link) "  ›" else "",
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                    color = if (link) MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = if (link) Modifier.clickable { onOpenMedia(id) }.testTag("media_line") else Modifier,
                )
            }
        },
        headlineContent = { Text(hit.title, maxLines = 2, overflow = TextOverflow.Ellipsis) },
        supportingContent = {
            Column {
                Text(statsLine(hit))
                hit.attrs?.let(::attrBadges)?.takeIf { it.isNotEmpty() }?.let { b ->
                    Text(
                        b.joinToString(" · "),
                        style = MaterialTheme.typography.labelSmall,
                        color = MaterialTheme.colorScheme.secondary,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                }
                hit.matchedFile?.let { f ->
                    Text(
                        "matched in file: ${fileName(f)}",
                        style = MaterialTheme.typography.labelSmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                }
            }
        },
        trailingContent = { SourceBadge(hit.origin, hit.trust, threshold) },
    )
}

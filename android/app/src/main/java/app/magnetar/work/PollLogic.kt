package app.magnetar.work

import app.magnetar.api.SavedSearchHits

/**
 * The poll's watermark belongs to one server (docs/APP.md §8): another server starts with a
 * first poll (no `since`: no hits, the server's clock as the watermark).
 */
fun sinceFor(storedServer: String?, storedWatermark: Long?, currentServer: String): Long? =
    if (storedServer == currentServer) storedWatermark else null

/** What one saved-search notification shows. */
data class AlertContent(
    val title: String,
    /** At most [MAX_LINES] titles, newest first. */
    val lines: List<String>,
    /** "+N more", or null. */
    val more: String?,
    /** Tap target: the newest hit (magnetar://torrent/{infohash}). */
    val infohash: String,
) {
    companion object {
        const val MAX_LINES = 5
    }
}

/** One InboxStyle notification per saved search with new hits; null when it has none. */
fun alertContent(r: SavedSearchHits): AlertContent? {
    val newest = r.hits.maxByOrNull { it.createdAt } ?: return null
    val ordered = r.hits.sortedByDescending { it.createdAt }
    val shown = ordered.take(AlertContent.MAX_LINES).map { it.title }
    val rest = ordered.size - shown.size
    val n = ordered.size
    return AlertContent(
        title = "${r.search.name}: " + (if (r.truncated) "$n+ new" else if (n == 1) "1 new" else "$n new"),
        lines = shown,
        more = when {
            rest > 0 -> "+$rest more" + if (r.truncated) "+" else ""
            r.truncated -> "and more"
            else -> null
        },
        infohash = newest.infohash,
    )
}

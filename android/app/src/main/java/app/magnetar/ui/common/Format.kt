package app.magnetar.ui.common

import java.util.Locale

fun formatBytes(bytes: Long?): String {
    if (bytes == null || bytes < 0) return "?"
    if (bytes < 1024) return "$bytes B"
    val units = listOf("KiB", "MiB", "GiB", "TiB", "PiB")
    var v = bytes / 1024.0
    var i = 0
    while (v >= 1024 && i < units.lastIndex) {
        v /= 1024
        i++
    }
    return String.format(Locale.ROOT, if (v < 10) "%.2f %s" else "%.1f %s", v, units[i])
}

/** "1,234" or, when the server only knows a lower bound (D43), "1,000+". */
fun formatCount(n: Long, lowerBound: Boolean = false): String =
    String.format(Locale.ROOT, "%,d", n) + if (lowerBound) "+" else ""

/** "1 result", "1,000+ results". */
fun resultsText(n: Long, lowerBound: Boolean): String =
    formatCount(n, lowerBound) + if (n == 1L && !lowerBound) " result" else " results"

/** "3m", "5h", "12d", "2y" relative to [nowSec]. */
fun formatAge(epochSec: Long, nowSec: Long = System.currentTimeMillis() / 1000): String {
    val d = (nowSec - epochSec).coerceAtLeast(0)
    return when {
        d < 60 -> "now"
        d < 3600 -> "${d / 60}m"
        d < 86_400 -> "${d / 3600}h"
        d < 365 * 86_400 -> "${d / 86_400}d"
        else -> "${d / (365 * 86_400)}y"
    }
}

/** "just now", "5m ago". */
fun formatAgo(epochSec: Long, nowSec: Long = System.currentTimeMillis() / 1000): String =
    formatAge(epochSec, nowSec).let { if (it == "now") "just now" else "$it ago" }

fun formatTimestamp(epochSec: Long): String =
    java.text.DateFormat.getDateTimeInstance(java.text.DateFormat.MEDIUM, java.text.DateFormat.SHORT)
        .format(java.util.Date(epochSec * 1000))

fun formatDate(epochSec: Long): String =
    java.text.DateFormat.getDateInstance(java.text.DateFormat.MEDIUM).format(java.util.Date(epochSec * 1000))

/** "★ 7.4". */
fun formatRating(r: Float): String = String.format(Locale.ROOT, "★ %.1f", r)

/** "12 S / 3 L", "≈40 peers" (DHT estimate only) or "peers unknown". */
fun peersText(seeders: Int?, leechers: Int?, bestPeers: Int? = null): String = when {
    seeders == null && leechers == null && bestPeers != null -> "≈$bestPeers peers"
    seeders == null && leechers == null -> "peers unknown"
    else -> "${seeders ?: "?"} S / ${leechers ?: "?"} L"
}

/** Last path component of a file inside a torrent ("Season 1/E01.mkv" → "E01.mkv"). */
fun fileName(path: String): String = path.trimEnd('/').substringAfterLast('/')

fun shortKey(key: String): String = if (key.length > 20) key.take(12) + "…" + key.takeLast(6) else key

enum class TrustLevel { OK, LOW, NEGATIVE }

/** Trust factor codes that pin a publisher to "do not show". */
val BLOCKING_TRUST_CODES = setOf("BLOCKED", "ROOT_MUTED")

/**
 * How a trust badge is tinted: OK at or above the threshold; NEGATIVE (red) only for a block,
 * a root mute or a negative factor; otherwise LOW, neutral (an unknown publisher is no warning).
 */
fun trustLevel(score: Int, threshold: Int, factorCodes: Collection<String> = emptyList(), negativeFactor: Boolean = false): TrustLevel =
    when {
        factorCodes.any { it in BLOCKING_TRUST_CODES } -> TrustLevel.NEGATIVE
        score >= threshold -> TrustLevel.OK
        negativeFactor -> TrustLevel.NEGATIVE
        else -> TrustLevel.LOW
    }

private val blockTags = Regex("(?i)<\\s*(br|/p|/div|/li|/h[1-6]|/tr)\\b[^>]*>")
private val anyTag = Regex("<[^>]*>")
private val numericEntity = Regex("&#(x?)([0-9a-fA-F]+);")
private val namedEntities = mapOf(
    "&nbsp;" to " ", "&lt;" to "<", "&gt;" to ">", "&quot;" to "\"", "&#39;" to "'", "&apos;" to "'",
)

/** Event descriptions are often HTML or BBCode-ish; show them as plain text. */
fun stripHtml(input: String): String {
    var s = input.replace(blockTags, "\n").replace(anyTag, "")
    s = numericEntity.replace(s) { m ->
        val radix = if (m.groupValues[1].isEmpty()) 10 else 16
        m.groupValues[2].toIntOrNull(radix)?.takeIf { Character.isValidCodePoint(it) }
            ?.let { String(Character.toChars(it)) } ?: m.value
    }
    for ((k, v) in namedEntities) s = s.replace(k, v)
    s = s.replace("&amp;", "&")
    return s.lines().joinToString("\n") { it.trimEnd() }
        .replace(Regex("\n{3,}"), "\n\n")
        .trim()
}

/** "cached · 5 min ago" for a preview saved at [savedAtMs]. */
fun cachedAgo(savedAtMs: Long, nowMs: Long = System.currentTimeMillis()): String {
    val s = ((nowMs - savedAtMs) / 1000).coerceAtLeast(0)
    val age = when {
        s < 60 -> "just now"
        s < 3600 -> "${s / 60} min ago"
        s < 86_400 -> "${s / 3600} h ago"
        else -> "${s / 86_400} d ago"
    }
    return "cached · $age"
}

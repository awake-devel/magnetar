package app.magnetar

import java.util.Locale

fun formatBytes(bytes: ULong?): String {
    if (bytes == null) return "?"
    val b = bytes.toDouble()
    if (b < 1024) return "$bytes B"
    val units = listOf("KiB", "MiB", "GiB", "TiB", "PiB")
    var v = b / 1024
    var i = 0
    while (v >= 1024 && i < units.lastIndex) {
        v /= 1024
        i++
    }
    return String.format(Locale.ROOT, if (v < 10) "%.2f %s" else "%.1f %s", v, units[i])
}

fun formatRate(bps: ULong): String = formatBytes(bps) + "/s"

/** "1,234" or, when the server only knows a lower bound (D43), "1,000+". */
fun formatCount(n: ULong, lowerBound: Boolean = false): String =
    String.format(Locale.ROOT, "%,d", n.toLong()) + if (lowerBound) "+" else ""

/** Last path component of a file inside a torrent ("Season 1/E01.mkv" -> "E01.mkv"). */
fun fileName(path: String): String = path.trimEnd('/').substringAfterLast('/')

enum class TrustLevel { OK, LOW, NEGATIVE }

/** Trust factor codes that pin a publisher to "do not show". */
val BLOCKING_TRUST_CODES = setOf("BLOCKED", "ROOT_MUTED")

/**
 * How a trust badge is tinted: OK at or above the threshold; NEGATIVE (red) only when the score
 * is explained by a block, a root mute or a negative factor; otherwise LOW, a neutral grey
 * (an unknown publisher simply has no score yet, which is not a warning).
 */
fun trustLevel(score: Int, threshold: Int, factorCodes: Collection<String> = emptyList(), negativeFactor: Boolean = false): TrustLevel =
    when {
        factorCodes.any { it in BLOCKING_TRUST_CODES } -> TrustLevel.NEGATIVE
        score >= threshold -> TrustLevel.OK
        negativeFactor -> TrustLevel.NEGATIVE
        else -> TrustLevel.LOW
    }

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

/** "just now", "5m ago", "3d ago": [formatAge] for "… ago" phrases. */
fun formatAgo(epochSec: Long, nowSec: Long = System.currentTimeMillis() / 1000): String =
    formatAge(epochSec, nowSec).let { if (it == "now") "just now" else "$it ago" }

fun formatTimestamp(epochSec: Long): String =
    java.text.DateFormat.getDateTimeInstance(java.text.DateFormat.MEDIUM, java.text.DateFormat.SHORT)
        .format(java.util.Date(epochSec * 1000))

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

private const val BASE32 = "ABCDEFGHIJKLMNOPQRSTUVWXYZ234567"

/** v1 infohash of a magnet URI as lowercase 40-hex (hex or 32-char base32 btih), else null. */
fun magnetInfohash(uri: String): String? {
    if (!uri.startsWith("magnet:", ignoreCase = true)) return null
    val xt = uri.substringAfter('?', "").split('&')
        .mapNotNull { p -> p.split('=', limit = 2).takeIf { it.size == 2 } }
        .filter { (k, _) -> k.equals("xt", true) || k.matches(Regex("(?i)xt\\.\\d+")) }
        .map { it[1] }
        .firstOrNull { it.startsWith("urn:btih:", ignoreCase = true) }
        ?: return null
    return normalizeInfohash(xt.substring("urn:btih:".length))
}

fun normalizeInfohash(s: String): String? {
    val t = s.trim()
    if (t.length == 40 && t.all { it.isDigit() || it.lowercaseChar() in 'a'..'f' }) return t.lowercase()
    if (t.length == 32) return base32ToHex(t.uppercase())
    return null
}

private fun base32ToHex(s: String): String? {
    val out = StringBuilder(40)
    var buffer = 0
    var bits = 0
    for (c in s) {
        val v = BASE32.indexOf(c).takeIf { it >= 0 } ?: return null
        buffer = (buffer shl 5) or v
        bits += 5
        if (bits >= 8) {
            bits -= 8
            out.append("%02x".format((buffer shr bits) and 0xff))
        }
    }
    return out.toString().takeIf { it.length == 40 }
}

/** A magnet: link or a magnetar://torrent/<infohash> deep link, else null. */
fun parseOpenUri(s: String): OpenRequest? = when {
    s.startsWith("magnet:", ignoreCase = true) -> magnetInfohash(s)?.let { OpenRequest(it, s) }
    s.startsWith("magnetar://torrent/", ignoreCase = true) ->
        normalizeInfohash(s.substring("magnetar://torrent/".length).substringBefore('?').trimEnd('/'))
            ?.let { OpenRequest(it, null) }
    else -> null
}

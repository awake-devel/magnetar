package app.magnetar

import app.magnetar.ffi.FfiDhtStats
import java.util.Locale

/** "1 h 05 min", "3 d 4 h", "42 s": how long the crawler has been running. */
fun formatUptime(secs: ULong): String {
    val s = secs.toLong()
    return when {
        s < 60 -> "$s s"
        s < 3600 -> "${s / 60} min"
        s < 86_400 -> String.format(Locale.ROOT, "%d h %02d min", s / 3600, (s % 3600) / 60)
        else -> "${s / 86_400} d ${(s % 86_400) / 3600} h"
    }
}

/** "12/min" with one decimal below 10. */
fun formatRate(perMin: Double, unit: String = "min", locale: Locale = Locale.getDefault()): String =
    (if (perMin < 10) String.format(locale, "%.1f", perMin) else formatNumber(Math.round(perMin), locale)) + "/$unit"

fun formatPercent(share: Double, locale: Locale = Locale.getDefault()): String =
    java.text.NumberFormat.getPercentInstance(locale).apply { maximumFractionDigits = 0 }.format(share)

/**
 * The DHT crawler as label → value lines (D58), for the Stats health card and the Status
 * screen: discovery rate, queue, metadata success, what is stored, router ports.
 */
fun dhtLines(d: FfiDhtStats, locale: Locale = Locale.getDefault()): List<Pair<String, String>> {
    val out = mutableListOf<Pair<String, String>>()
    out += "Crawler" to when {
        d.running -> "${d.mode} · ${d.virtualNodes} node${if (d.virtualNodes == 1u) "" else "s"} · up ${formatUptime(d.uptimeSecs)}"
        d.lastError != null -> "restarting after an error: ${d.lastError}"
        else -> "${d.mode} · starting"
    }
    if (!d.running) {
        out += "Stored" to formatNumber(d.storedTotal.toLong(), locale) + " torrents"
        return out
    }
    out += "Discovery" to formatRate(d.hashesPerMin, locale = locale) + " new hashes"
    out += "Queue" to formatNumber(d.queueLen.toLong(), locale) + " waiting for peers" +
        if (d.samplingPaused) " (sampling paused)" else ""
    out += "Metadata" to formatRate(d.metadataPerMin, locale = locale) + " · " +
        formatPercent(d.fetchSuccessRate, locale) + " of fetches succeed"
    out += "Stored" to formatNumber(d.storedTotal.toLong(), locale) + " torrents" +
        if (d.adultTotal > 0uL) " (${formatNumber(d.adultTotal.toLong(), locale)} adult)" else ""
    if (d.blockedTotal > 0uL) out += "Blocked (illegal content)" to formatNumber(d.blockedTotal.toLong(), locale)
    out += "DHT queries" to formatRate(d.queriesPerSec, "s", locale) + " · " +
        formatPercent(d.answeredShare, locale) + " answered" + if (d.brake) " · brake on" else ""
    if (d.mode == "aggressive") out += "Incoming queries" to formatRate(d.incomingPerMin, locale = locale)
    d.upnpState?.let { st ->
        out += "Router ports" to when (st) {
            "mapped", "partial" -> d.upnpMappings.joinToString(", ") +
                if (st == "partial") " (some failed: ${d.upnpError ?: "?"})" else ""
            "failed" -> "not mapped: ${d.upnpError ?: "no UPnP gateway answered"}"
            "searching" -> "looking for the router…"
            else -> "not mapped"
        }
    }
    if (d.externalIp != "unknown") out += "Address" to d.externalIp
    return out
}

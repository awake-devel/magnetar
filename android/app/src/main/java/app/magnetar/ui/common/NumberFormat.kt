package app.magnetar.ui.common

import java.text.NumberFormat
import java.time.Instant
import java.time.ZoneId
import java.time.format.DateTimeFormatter
import java.util.Locale
import kotlin.math.abs
import kotlin.math.roundToLong

/*
 * Numbers of the Stats tab in the phone's locale (docs/STATS.md "Colours and numbers"):
 * "1 234 567" / "12,3 k" in French, "1,234,567" / "12.3k" in English.
 */

/** Grouped integer: "1,234,567" (en), "1 234 567" (fr, narrow no-break space). */
fun formatNumber(n: Long, locale: Locale = Locale.getDefault()): String = NumberFormat.getIntegerInstance(locale).format(n)

/**
 * Short form for chart labels: "950", "12.3k", "1.2M", "3.4B" (one decimal under 100, none
 * above); French uses a comma and a space before the unit ("12,3 k").
 */
fun formatCompact(v: Double, locale: Locale = Locale.getDefault()): String {
    val a = abs(v)
    if (a < 1000) return formatNumber(v.roundToLong(), locale)
    val (div, unit) = when {
        a < 1e6 -> 1e3 to "k"
        a < 1e9 -> 1e6 to "M"
        else -> 1e9 to "B"
    }
    val x = v / div
    val digits = if (abs(x) < 100) 1 else 0
    val rounded = if (digits == 1) (x * 10).roundToLong() / 10.0 else x.roundToLong().toDouble()
    // 999 950 rounds to "1000k": say "1M" instead.
    if (unit != "B" && abs(rounded) >= 1000) return formatCompact(if (v < 0) -div * 1000 else div * 1000, locale)
    val text = NumberFormat.getNumberInstance(locale).apply {
        maximumFractionDigits = digits
        minimumFractionDigits = 0
    }.format(rounded)
    return text + (if (locale.language == "fr") " " else "") + unit
}

/** Share of [n] in [total]: "42%", "<1%" for a small non-zero share, "–" without a total. */
fun formatShare(n: Long, total: Long, locale: Locale = Locale.getDefault()): String {
    if (total <= 0) return "–"
    val share = n.toDouble() / total.toDouble()
    if (n > 0 && share < 0.005) return "<" + formatPercent(0.01, locale)
    return formatPercent(share, locale)
}

/** "42%" ("42 %" in French), no decimals. */
fun formatPercent(share: Double, locale: Locale = Locale.getDefault()): String =
    NumberFormat.getPercentInstance(locale).apply { maximumFractionDigits = 0 }.format(share)

/** "42 s", "12 min", "1 h 05 min", "3 d 4 h": how long something has been running. */
fun formatUptime(secs: Long): String {
    val s = secs.coerceAtLeast(0)
    return when {
        s < 60 -> "$s s"
        s < 3600 -> "${s / 60} min"
        s < 86_400 -> String.format(Locale.ROOT, "%d h %02d min", s / 3600, (s % 3600) / 60)
        else -> "${s / 86_400} d ${(s % 86_400) / 3600} h"
    }
}

/** "4.5/min" (one decimal under 10), "18,146/min". */
fun formatRate(perUnit: Double, unit: String = "min", locale: Locale = Locale.getDefault()): String =
    (if (perUnit < 10) NumberFormat.getNumberInstance(locale).apply { maximumFractionDigits = 1; minimumFractionDigits = 1 }.format(perUnit)
    else formatNumber(perUnit.roundToLong(), locale)) + "/$unit"

/**
 * How long the server's seed checker needs for what is due now at its last-hour pace (D45):
 * "under an hour", "≈ 5 h", "≈ 3 d"; null when nothing is due or nothing was checked lately.
 */
fun backlogEta(due: Long, checksPerHour: Long): String? {
    if (due <= 0 || checksPerHour <= 0) return null
    val hours = due.toDouble() / checksPerHour.toDouble()
    return when {
        hours < 1 -> "under an hour"
        hours < 48 -> "≈ ${hours.roundToLong()} h"
        else -> "≈ ${(hours / 24).roundToLong()} d"
    }
}

/**
 * Bucket start → "Oct 12" / "12 oct." (days) or "14:00" (hours). Day buckets of the activity
 * series are UTC days and are labelled in UTC (a zone west of UTC would name the day before);
 * hours use the phone's zone.
 */
fun bucketLabel(
    at: Long,
    hourly: Boolean,
    locale: Locale = Locale.getDefault(),
    zone: ZoneId = if (hourly) ZoneId.systemDefault() else ZoneId.of("UTC"),
): String {
    val t = Instant.ofEpochSecond(at).atZone(zone)
    val pattern = when {
        hourly -> "HH:mm"
        locale.language == "fr" -> "d MMM"
        else -> "MMM d"
    }
    return DateTimeFormatter.ofPattern(pattern, locale).format(t)
}

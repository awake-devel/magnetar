package app.magnetar.ui.crawlmap

import app.magnetar.api.CrawlMap
import app.magnetar.api.GeoCount
import java.time.Instant
import java.time.ZoneOffset
import java.time.format.DateTimeFormatter
import java.util.Locale
import kotlin.math.ln

/** The two layers of the crawl map (`layer=`). */
enum class MapLayer(val api: String, val label: String) {
    DHT("dht", "DHT discoveries"),
    NOSTR("nostr", "Nostr relays"),
}

/** `window=`: the last 24 h or everything since [CrawlMap.since]. */
enum class MapWindow(val api: String, val label: String) {
    DAY("day", "Day"),
    ALL("all", "All"),
}

/** A metric's chip label. */
fun metricLabel(metric: String): String = when (metric) {
    "hashes" -> "Hashes"
    "stored" -> "Stored"
    "nodes" -> "Nodes"
    "relays" -> "Relays"
    "events" -> "Events"
    "torrents" -> "Torrents"
    else -> metric.replaceFirstChar { it.uppercase() }
}

/** What a metric counts, under the chips. */
fun metricHint(metric: String): String = when (metric) {
    "hashes" -> "Infohashes nodes there gave us"
    "stored" -> "Torrents stored from nodes there"
    "nodes" -> "DHT nodes heard from, each counted once per UTC day"
    "relays" -> "Relays whose host resolves there"
    "events" -> "Events received from relays there"
    "torrents" -> "Torrents received from relays there"
    else -> metric
}

/** Why the server's geo database cannot place addresses (`available: false`). */
fun geoReasonMessage(reason: String?): String = when (reason) {
    "disabled" -> "The server's geo database is off, so nothing new is placed on the map."
    "missing" -> "The server's geo database is not downloaded yet, so nothing new is placed on the map."
    "invalid" -> "The server's geo database could not be read, so nothing new is placed on the map."
    else -> "The server's geo database is not available."
}

/** "Germany" for `DE` in the phone's language; the code itself when the platform does not know it. */
fun countryName(code: String, locale: Locale = Locale.getDefault()): String {
    val c = code.uppercase()
    if (c.length != 2 || !c.all { it in 'A'..'Z' }) return c
    val name = Locale.Builder().setRegion(c).build().getDisplayCountry(locale)
    return if (name.isBlank() || name == c) c else name
}

/** "September 2026" (UTC) for a database build time. */
fun buildMonth(epochSec: Long, locale: Locale = Locale.getDefault()): String =
    DateTimeFormatter.ofPattern("LLLL yyyy", locale).format(Instant.ofEpochSecond(epochSec).atZone(ZoneOffset.UTC))

/** "AS24940 Hetzner Online GmbH" (or just the AS number / organisation). */
fun networkLabel(g: GeoCount): String = listOfNotNull(g.asn?.let { "AS$it" }, g.asOrg?.takeIf { it.isNotBlank() })
    .joinToString(" ").ifEmpty { "Unknown network" }

/** Index of [metric] in [map]'s metrics, the first one when it is absent. */
fun metricIndex(map: CrawlMap, metric: String?): Int = map.metrics.indexOf(metric).coerceAtLeast(0)

/** Country code → count of [metric] (codes upper-cased; zero counts left out). */
fun countryCounts(map: CrawlMap, metric: Int): Map<String, Long> = buildMap {
    for (c in map.countries) {
        val code = c.country?.uppercase() ?: continue
        val n = c.count(metric)
        if (n > 0) put(code, (get(code) ?: 0) + n)
    }
}

/** The [n] rows with the most [metric], zero rows left out. */
fun topBy(rows: List<GeoCount>, metric: Int, n: Int): List<GeoCount> =
    rows.filter { it.count(metric) > 0 }.sortedByDescending { it.count(metric) }.take(n)

/** The total of [metric]: the server's, else the sum of countries and unknown. */
fun totalOf(map: CrawlMap, metric: Int): Long =
    map.totals.getOrNull(metric) ?: (map.countries.sumOf { it.count(metric) } + (map.unknown.getOrNull(metric) ?: 0))

/**
 * Sequential log scale of the choropleth: a count → 0 (the smallest non-zero count) … 1 (the
 * largest); null for 0 or no data (drawn in the neutral colour).
 */
class LogScale(val min: Long, val max: Long) {
    fun shade(v: Long): Float? {
        if (v <= 0) return null
        if (max <= min) return 1f
        val t = (ln(v.toDouble()) - ln(min.toDouble())) / (ln(max.toDouble()) - ln(min.toDouble()))
        return t.toFloat().coerceIn(0f, 1f)
    }

    companion object {
        /** The scale of [values]; null when none is above zero. */
        fun of(values: Collection<Long>): LogScale? {
            val positive = values.filter { it > 0 }
            if (positive.isEmpty()) return null
            return LogScale(positive.min(), positive.max())
        }
    }
}

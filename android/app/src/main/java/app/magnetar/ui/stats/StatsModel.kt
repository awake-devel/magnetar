package app.magnetar.ui.stats

import app.magnetar.api.DhtStats
import app.magnetar.api.Features
import app.magnetar.api.LivenessStats
import app.magnetar.api.SearchIndexStatus
import app.magnetar.ui.common.backlogEta
import app.magnetar.ui.common.facetLabel
import app.magnetar.ui.common.formatAgo
import app.magnetar.ui.common.formatBytes
import app.magnetar.ui.common.formatNumber
import app.magnetar.ui.common.formatPercent
import app.magnetar.ui.common.formatRate
import app.magnetar.ui.common.formatUptime
import app.magnetar.ui.search.SearchFilters
import java.util.Locale

/** What a metric's values are, for its labels. */
enum class ValueUnit { COUNT, BYTES }

/** One activity chart the user can pick (D47 metrics, D74 DHT metrics). */
enum class Activity(val metric: String, val label: String, val unit: ValueUnit = ValueUnit.COUNT, val dht: Boolean = false) {
    TOTAL("torrents_total", "Torrents over time"),
    ADDED("torrents_added", "Added"),
    PUBLISHED("torrents_published", "Published"),
    CRAWL("crawl_events", "Crawled events"),
    RELAYS("relays_discovered", "Relays found"),
    CHECKS("liveness_checks", "Seed checks"),
    DB("db_size", "Database size", ValueUnit.BYTES),
    DHT_ADDED("dht_torrents_added", "DHT torrents added", dht = true),
    DHT_SEEN("dht_hashes_seen", "DHT hashes found", dht = true),
    DHT_OK("dht_fetch_ok", "DHT metadata fetched", dht = true),
    DHT_FAIL("dht_fetch_fail", "DHT fetches failed", dht = true),

    /** A count only: the illegal-content blocklist's matches are never listed (docs/DHT.md §7). */
    DHT_BLOCKED("dht_blocked", "DHT blocked (count)", dht = true),
    ;

    companion object {
        /** The metrics a server with [dht] (crawler on) offers. */
        fun offered(dht: Boolean): List<Activity> = entries.filter { !it.dht || dht }
    }
}

/** The breakdown charts, in screen order (years come last, as a histogram of their own). */
val BREAKDOWN_FIELDS = listOf(
    "category", "language", "resolution", "media_kind", "source", "codec", "hdr", "audio", "genre", "subcategory",
)

/** The breakdowns a server with [features] can count: `origin` (Nostr / DHT, D51) is newer. */
fun breakdownFields(features: Set<String>): List<String> =
    if (Features.ORIGIN in features) BREAKDOWN_FIELDS + "origin" else BREAKDOWN_FIELDS

val FIELD_TITLES = mapOf(
    "language" to "Audio languages", "resolution" to "Resolutions", "category" to "Categories",
    "source" to "Release sources", "codec" to "Video codecs", "hdr" to "HDR", "audio" to "Audio formats",
    "genre" to "Genres", "media_kind" to "Movies, series, anime", "subcategory" to "Publisher categories",
    "year" to "Years", "origin" to "Found via (Nostr / DHT)",
)

/** Fields whose values are search filters (cross-filter on tap); the others open a text search. */
val FILTERABLE = setOf(
    "language", "resolution", "category", "source", "codec", "hdr", "audio", "genre", "media_kind", "year", "origin",
)

/** A torrent can count under several values of these. */
val MULTI_VALUED = setOf("language", "hdr", "audio", "genre", "origin")

/** Fixed vocabularies drawn as donuts: a value keeps its colour slot whatever its rank. */
val DONUT_ORDER = mapOf(
    "category" to listOf("video", "anime", "audio", "book", "game", "application", "porn", "picture"),
    "resolution" to listOf("2160p", "1080p", "720p", "576p", "480p", "sd", "1440p", "4320p"),
    "media_kind" to listOf("movie", "series", "anime"),
    "origin" to listOf("nostr", "dht"),
)

/** The label of a chart value ("fr" → "French", "web-dl" → "WEB-DL", "dht" → "DHT"). */
fun valueLabel(field: String, value: String): String = when (field) {
    "origin" -> when (value) {
        "nostr" -> "Nostr"
        "dht" -> "DHT"
        else -> value
    }
    "subcategory", "year" -> value
    else -> facetLabel(field, value)
}

/** [value] of [field] switched on or off as a cross-filter (a year is min = max = that year). */
fun SearchFilters.toggleStat(field: String, value: String): SearchFilters = when (field) {
    "year" -> value.toIntOrNull()?.let { y ->
        if (minYear == y && maxYear == y) copy(minYear = null, maxYear = null) else copy(minYear = y, maxYear = y)
    } ?: this
    in FILTERABLE -> toggle(field, value)
    else -> this
}

/** Values of [field] the cross-filter selects. */
fun SearchFilters.selectedStat(field: String): Set<String> = when (field) {
    "year" -> if (minYear != null && minYear == maxYear) setOf(minYear.toString()) else emptySet()
    else -> selected(field)
}

/** The active cross-filters as (field, value) chips, in chart order. */
fun SearchFilters.statChips(): List<Pair<String, String>> =
    (BREAKDOWN_FIELDS + "origin" + "year").filter { it in FILTERABLE }.flatMap { f -> selectedStat(f).sorted().map { f to it } }

/**
 * Seed checks (D45) as the health card shows them: the latest result of the torrents checked at
 * least once (seeded / dead / no answer), and, separately, those not checked yet. "Not checked
 * yet" is not a result, so it is not a slice of the same bar.
 */
data class SeedChecks(val seeded: Long, val dead: Long, val noAnswer: Long, val notChecked: Long, val torrents: Long) {
    val checked: Long get() = seeded + dead + noAnswer
}

fun seedChecks(l: LivenessStats): SeedChecks =
    SeedChecks(l.alive, l.dead, l.unknown, l.neverChecked, maxOf(l.torrents, l.alive + l.dead + l.unknown + l.neverChecked))

/** The seed-check backlog as label → value lines (Status tab). */
fun backlogLines(l: LivenessStats, locale: Locale = Locale.getDefault(), nowSec: Long = System.currentTimeMillis() / 1000): List<Pair<String, String>> =
    buildList {
        if (!l.enabled) add("Checker" to "turned off on the server")
        add("Due now" to formatNumber(l.dueNow, locale) + (backlogEta(l.dueNow, l.checksPerHour)?.let { " (cleared $it)" } ?: ""))
        add("Never checked" to "${formatNumber(l.neverChecked, locale)} of ${formatNumber(l.torrents, locale)}")
        add("Checked, last 24 h" to formatNumber(l.checked24h, locale))
        add("Checks per hour" to formatNumber(l.checksPerHour, locale))
        if (l.trackers > 0) {
            add("Trackers backing off" to "${formatNumber(l.trackersBackedOff, locale)} of ${formatNumber(l.trackers, locale)}")
        }
        l.lastBatch?.let { b ->
            add(
                "Last batch" to "${formatNumber(b.hashes, locale)} in ${formatUptime(b.ms / 1000)}" +
                    (l.lastBatchAt?.let { ", ${formatAgo(it, nowSec)}" } ?: ""),
            )
        }
    }

/** The search index (D43) as label → value lines. */
fun indexLines(s: SearchIndexStatus, locale: Locale = Locale.getDefault(), nowSec: Long = System.currentTimeMillis() / 1000): List<Pair<String, String>> =
    buildList {
        add(
            "State" to when (s.state) {
                "ready" -> "ready"
                "building" -> "building" + if (s.buildTotal > 0) " (${formatPercent(s.buildDone.toDouble() / s.buildTotal, locale)})" else ""
                "error" -> "error" + (s.error?.let { ": $it" } ?: "")
                else -> s.state.ifBlank { "unknown" }
            },
        )
        add("Searchable torrents" to formatNumber(s.docs, locale))
        add("Changes queued" to formatNumber(s.pending, locale) + (s.lagSecs?.takeIf { s.pending > 0 }?.let { " (oldest ${formatUptime(it)})" } ?: ""))
        s.lastCommit?.let { add("Last update" to formatAgo(it, nowSec)) }
        if (s.bytes > 0) add("Size" to formatBytes(s.bytes))
    }

/**
 * The server's DHT crawler (D58, D74) as label → value lines: discovery, queue, metadata,
 * stored, blocked (a count only), queries, router ports.
 */
fun dhtLines(d: DhtStats, locale: Locale = Locale.getDefault()): List<Pair<String, String>> {
    val out = mutableListOf<Pair<String, String>>()
    out += "Crawler" to when {
        d.running -> "${d.mode} · ${d.virtualNodes} node${if (d.virtualNodes == 1) "" else "s"} · up ${formatUptime(d.uptimeSecs)}"
        d.lastError != null -> "restarting after an error: ${d.lastError}"
        else -> "${d.mode} · starting"
    }
    if (!d.running) {
        out += "Stored" to formatNumber(d.storedTotal, locale) + " torrents"
        return out
    }
    out += "Discovery" to formatRate(d.hashesPerMin, locale = locale) + " new hashes"
    out += "Queue" to formatNumber(d.queueLen, locale) + " waiting for peers" + if (d.samplingPaused) " (sampling paused)" else ""
    out += "Metadata" to formatRate(d.metadataPerMin, locale = locale) + " · " +
        formatPercent(d.fetchSuccessRate, locale) + " of fetches succeed"
    out += "Stored" to formatNumber(d.storedTotal, locale) + " torrents" +
        if (d.adultTotal > 0) " (${formatNumber(d.adultTotal, locale)} adult)" else ""
    if (d.blockedTotal > 0) out += "Blocked (illegal content)" to formatNumber(d.blockedTotal, locale)
    out += "DHT queries" to formatRate(d.queriesPerSec, "s", locale) + " · " +
        formatPercent(d.answeredShare, locale) + " answered" + if (d.brake) " · brake on" else ""
    if (d.mode == "aggressive") out += "Incoming queries" to formatRate(d.incomingPerMin, locale = locale)
    d.upnp?.takeIf { it.state != "off" }?.let { u ->
        val maps = u.mappings.joinToString(", ") { "${it.protocol} ${it.port}" }
        out += "Router ports" to when (u.state) {
            "mapped" -> maps
            "partial" -> "$maps (some failed: ${u.lastError ?: "?"})"
            "failed" -> "not mapped: ${u.lastError ?: "no UPnP gateway answered"}"
            "searching" -> "looking for the router…"
            else -> u.state
        }
    }
    if (d.externalIp != "unknown") out += "Address" to d.externalIp
    return out
}

package app.magnetar.ui.stats

import app.magnetar.api.CrawlMachine
import app.magnetar.api.RouteCount
import app.magnetar.api.RouteHour
import app.magnetar.api.TopRelay
import app.magnetar.ui.common.formatNumber
import app.magnetar.ui.common.formatRate
import java.text.NumberFormat
import java.util.Locale

/*
 * "Where discoveries come from" (feature `crawl_sources`): labels and numbers of the card, pure
 * so they are unit tested.
 */

/** DHT routes in a fixed order, so a route keeps its colour slot. */
val ROUTE_ORDER = listOf("sample", "get_peers", "announce")

fun routeLabel(via: String): String = when (via) {
    "sample" -> "Sampling (BEP 51)"
    "get_peers" -> "Lookups (get_peers)"
    "announce" -> "Announces"
    else -> via
}

/** [routes] in [ROUTE_ORDER], unknown routes after them. */
fun orderedRoutes(routes: List<RouteCount>): List<RouteCount> =
    routes.sortedBy { r -> ROUTE_ORDER.indexOf(r.via).let { if (it < 0) ROUTE_ORDER.size else it } }

/** Route names of a series of hours, [ROUTE_ORDER] first. */
fun hourRoutes(hours: List<RouteHour>): List<String> {
    val seen = hours.flatMap { it.seen.keys + it.stored.keys }.toSet()
    return ROUTE_ORDER.filter { it in seen } + (seen - ROUTE_ORDER.toSet()).sorted()
}

/**
 * Stored / seen as a percent: one decimal under 10 % ("6.6%"), "<0.1%" for a tiny non-zero
 * share, "–" when nothing was seen.
 */
fun formatConversion(stored: Long, seen: Long, locale: Locale = Locale.getDefault()): String {
    if (seen <= 0) return "–"
    val share = stored.toDouble() / seen.toDouble()
    val f = NumberFormat.getPercentInstance(locale)
    if (stored > 0 && share < 0.001) return "<" + f.apply { maximumFractionDigits = 1 }.format(0.001)
    f.maximumFractionDigits = if (share < 0.1) 1 else 0
    return f.format(share)
}

/** How the server found a relay. */
fun relaySourceLabel(source: String): String = when (source) {
    "nip66" -> "NIP-66 monitors"
    "nip65" -> "NIP-65 relay lists"
    "seed" -> "Seed list"
    "hint" -> "Relay hints in events"
    "kind3" -> "Contact lists (kind 3)"
    "manual" -> "Added by hand"
    else -> source.ifBlank { "Unknown" }
}

/** "DE · Hetzner Online GmbH (AS24940)", or null when nothing is known. */
fun relayPlace(r: TopRelay): String? {
    val net = when {
        !r.asOrg.isNullOrBlank() && r.asn != null -> "${r.asOrg} (AS${r.asn})"
        !r.asOrg.isNullOrBlank() -> r.asOrg
        r.asn != null -> "AS${r.asn}"
        else -> null
    }
    return listOfNotNull(r.country?.takeIf { it.isNotBlank() }?.uppercase(), net).joinToString(" · ").ifEmpty { null }
}

/** A relay URL without its `wss://` for a list row. */
fun shortRelayUrl(url: String): String = url.removePrefix("wss://").removePrefix("ws://").trimEnd('/')

/** One machine (server or worker) as label → value lines. */
fun machineLines(m: CrawlMachine, locale: Locale = Locale.getDefault()): List<Pair<String, String>> = buildList {
    add("Triaged" to formatNumber(m.dhtTriaged, locale))
    if (m.dhtQueued > 0) add("Queued" to formatNumber(m.dhtQueued, locale))
    add("Metadata fetched" to formatNumber(m.dhtMetadata, locale))
    add("Stored" to formatNumber(m.dhtStored, locale) + " · " + formatRate(m.storedPerMin, locale = locale))
    add("Seed checks" to formatNumber(m.seedChecks, locale))
}

fun machineTitle(m: CrawlMachine): String =
    m.name.ifBlank { "?" } + when (m.kind) {
        "server" -> " (server)"
        "worker" -> " (worker)"
        else -> m.kind.takeIf { it.isNotBlank() }?.let { " ($it)" }.orEmpty()
    }

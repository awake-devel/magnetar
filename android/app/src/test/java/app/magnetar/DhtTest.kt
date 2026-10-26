package app.magnetar

import app.magnetar.ffi.FfiDhtStats
import app.magnetar.ffi.FfiOrigin
import app.magnetar.ui.common.sourceLabel
import app.magnetar.ui.search.SearchFilters
import app.magnetar.ui.search.SearchUiState
import app.magnetar.ui.search.toQuery
import app.magnetar.ui.search.toUiState
import app.magnetar.ui.stats.breakdownFields
import app.magnetar.ui.stats.selectedStat
import app.magnetar.ui.stats.toggleStat
import java.util.Locale
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/** DHT results in the app (D51) and the crawler's numbers (D58). */
class DhtTest {
    private fun stats(running: Boolean = true) = FfiDhtStats(
        mode = "aggressive",
        running = running,
        uptimeSecs = 3_900uL,
        virtualNodes = 4u,
        externalIp = "public",
        routingNodes = 300uL,
        poolNodes = 120_000uL,
        queriesPerSec = 164.2,
        answeredShare = 0.52,
        brake = false,
        incomingPerMin = 3.0,
        hashesPerMin = 2_522.0,
        hashesSeen = 11_350uL,
        queueLen = 8_000uL,
        samplingPaused = true,
        metadataPerMin = 95.5,
        fetchSuccessRate = 0.259,
        fetchOk = 716uL,
        fetchFailed = 2_000uL,
        stored = 700uL,
        blocked = 3uL,
        storedTotal = 1_470uL,
        adultTotal = 261uL,
        blockedTotal = 10uL,
        seenQueued = 7_000uL,
        seenFailed = 2_000uL,
        upnpState = "mapped",
        upnpMappings = listOf("udp 6891", "udp 6892"),
        upnpError = null,
        lastError = null,
    )

    @Test
    fun sourceChipMapsToTheQueryAndBack() {
        val s = SearchUiState(filters = SearchFilters(origins = setOf("dht")))
        val q = s.toQuery(nowSec = 1_800_000_000L)
        assertEquals(listOf("dht"), q.origins)
        assertEquals(setOf("dht"), q.toUiState().filters.origins)
        assertEquals(1, s.filters.activeCount)
        val both = SearchFilters().toggle("origin", "nostr").toggle("origin", "dht")
        assertEquals(setOf("nostr", "dht"), both.selected("origin"))
        assertEquals(setOf("dht"), both.toggle("origin", "nostr").origins)
        // Stats cross-filters go through the same toggle.
        assertEquals(setOf("dht"), SearchFilters().toggleStat("origin", "dht").selectedStat("origin"))
    }

    @Test
    fun originBreakdownOnlyForServersThatKnowIt() {
        assertTrue("origin" !in breakdownFields(listOf("stats")))
        assertTrue("origin" in breakdownFields(listOf("stats", "origin")))
        assertTrue("origin" !in breakdownFields(null))
    }

    @Test
    fun sourceLabels() {
        assertEquals("Nostr", sourceLabel(FfiOrigin.NOSTR))
        assertTrue(sourceLabel(FfiOrigin.DHT).startsWith("DHT"))
        assertEquals("Nostr and DHT", sourceLabel(FfiOrigin.BOTH))
    }

    @Test
    fun crawlerLines() {
        val lines = dhtLines(stats(), Locale.US).toMap()
        assertEquals("aggressive · 4 nodes · up 1 h 05 min", lines["Crawler"])
        assertEquals("2,522/min new hashes", lines["Discovery"])
        assertEquals("8,000 waiting for peers (sampling paused)", lines["Queue"])
        assertEquals("96/min · 26% of fetches succeed", lines["Metadata"])
        assertEquals("1,470 torrents (261 adult)", lines["Stored"])
        assertEquals("10", lines["Blocked (illegal content)"])
        assertEquals("164/s · 52% answered", lines["DHT queries"])
        assertEquals("3.0/min", lines["Incoming queries"])
        assertEquals("udp 6891, udp 6892", lines["Router ports"])
        assertEquals("public", lines["Address"])
        val down = dhtLines(stats(running = false).copy(lastError = "port in use"), Locale.US).toMap()
        assertEquals("restarting after an error: port in use", down["Crawler"])
        assertEquals(setOf("Crawler", "Stored"), down.keys)
        val failed = dhtLines(stats().copy(upnpState = "failed", upnpError = "no answer"), Locale.US).toMap()
        assertEquals("not mapped: no answer", failed["Router ports"])
    }

    @Test
    fun uptimes() {
        assertEquals("42 s", formatUptime(42uL))
        assertEquals("5 min", formatUptime(300uL))
        assertEquals("2 h 00 min", formatUptime(7_200uL))
        assertEquals("3 d 4 h", formatUptime(3uL * 86_400uL + 4uL * 3_600uL))
    }
}

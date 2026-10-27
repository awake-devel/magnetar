package app.magnetar.ui

import app.magnetar.api.DhtStats
import app.magnetar.api.LivenessStats
import app.magnetar.api.SearchIndexStatus
import app.magnetar.api.UpnpMapping
import app.magnetar.api.UpnpStatus
import app.magnetar.ui.common.backlogEta
import app.magnetar.ui.common.bucketLabel
import app.magnetar.ui.common.formatCompact
import app.magnetar.ui.common.formatNumber
import app.magnetar.ui.common.formatPercent
import app.magnetar.ui.common.formatRate
import app.magnetar.ui.common.formatShare
import app.magnetar.ui.common.formatUptime
import app.magnetar.ui.search.SearchFilters
import app.magnetar.ui.stats.Activity
import app.magnetar.ui.stats.ChartWindow
import app.magnetar.ui.stats.backlogLines
import app.magnetar.ui.stats.breakdownFields
import app.magnetar.ui.stats.dhtLines
import app.magnetar.ui.stats.indexLines
import app.magnetar.ui.stats.seedChecks
import app.magnetar.ui.stats.selectedStat
import app.magnetar.ui.stats.sliceAt
import app.magnetar.ui.stats.statChips
import app.magnetar.ui.stats.toggleStat
import app.magnetar.ui.stats.valueLabel
import java.time.ZoneId
import java.util.Locale
import org.testng.Assert.assertEquals
import org.testng.Assert.assertFalse
import org.testng.Assert.assertNull
import org.testng.Assert.assertTrue
import org.testng.annotations.Test

class StatsFormatTest {
    private val en = Locale.US
    private val fr = Locale.FRANCE

    /** French groups with a (narrow) no-break space; compare with plain spaces. */
    private fun plain(s: String) = s.replace(' ', ' ').replace(' ', ' ')

    @Test
    fun `numbers follow the locale`() {
        assertEquals(formatNumber(1_234_567, en), "1,234,567")
        assertEquals(plain(formatNumber(1_234_567, fr)), "1 234 567")
        assertEquals(formatCompact(950.0, en), "950")
        assertEquals(formatCompact(12_345.0, en), "12.3k")
        assertEquals(plain(formatCompact(12_345.0, fr)), "12,3 k")
        assertEquals(formatCompact(958_106.0, en), "958k")
        assertEquals(formatCompact(1_234_567.0, en), "1.2M")
        assertEquals(formatCompact(11_510_920_895.0, en), "11.5B")
        // Rounding up to the next unit says so instead of "1000k".
        assertEquals(formatCompact(999_950.0, en), "1M")
        assertEquals(formatCompact(-12_345.0, en), "-12.3k")
    }

    @Test
    fun `shares and percents`() {
        assertEquals(formatShare(1, 4, en), "25%")
        assertEquals(formatShare(1, 1000, en), "<1%")
        assertEquals(formatShare(0, 1000, en), "0%")
        assertEquals(formatShare(5, 0, en), "–")
        assertEquals(plain(formatPercent(0.4269, fr)), "43 %")
    }

    @Test
    fun `durations, rates and the backlog`() {
        assertEquals(formatUptime(42), "42 s")
        assertEquals(formatUptime(508), "8 min")
        assertEquals(formatUptime(3900), "1 h 05 min")
        assertEquals(formatUptime(3 * 86_400 + 4 * 3600 + 5), "3 d 4 h")
        assertEquals(formatRate(4.54, locale = en), "4.5/min")
        assertEquals(formatRate(18_145.69, locale = en), "18,146/min")
        assertEquals(formatRate(2779.27, "s", en), "2,779/s")
        assertNull(backlogEta(0, 100))
        assertNull(backlogEta(100, 0))
        assertEquals(backlogEta(50, 100), "under an hour")
        // Production on 2026-10-27: 431,893 due at 30,000 per hour.
        assertEquals(backlogEta(431_893, 30_000), "≈ 14 h")
        assertEquals(backlogEta(10_000_000, 30_000), "≈ 14 d")
    }

    @Test
    fun `day buckets are UTC days, hours the phone's zone`() {
        // 2026-10-26 00:00 UTC is still the 25th in New York.
        val at = 1_793_059_200L - 86_400
        assertEquals(bucketLabel(at, hourly = false, locale = en), "Oct 26")
        assertEquals(bucketLabel(at, hourly = false, locale = fr), "26 oct.")
        assertEquals(bucketLabel(at, hourly = true, locale = en, zone = ZoneId.of("America/New_York")), "20:00")
        assertEquals(bucketLabel(at, hourly = true, locale = en, zone = ZoneId.of("UTC")), "00:00")
    }

    @Test
    fun `chart window zooms around the finger and pans within bounds`() {
        val w = ChartWindow(91)
        assertEquals(w.indexAt(0f), 0)
        assertEquals(w.indexAt(1f), 90)
        val z = w.zoom(3f, 0.5f)
        assertEquals(z.count, 30)
        // The middle point stays under the focus.
        assertEquals(z.indexAt(0.5f), 45)
        val left = z.pan(10f)
        assertEquals(left.first, 0)
        val right = z.pan(-10f)
        assertEquals(right.first, 91 - 30)
        // Never fewer than 7 points, never more than all of them.
        assertEquals(w.zoom(100f, 0f).count, ChartWindow.MIN)
        assertEquals(z.zoom(0.01f, 0.5f), z.copy(first = 0, count = 91))
        // A short series does not zoom.
        assertEquals(ChartWindow(5).zoom(4f, 0.5f), ChartWindow(5))
    }

    @Test
    fun `donut hit test by angle`() {
        val sizes = listOf(50L, 25L, 25L)
        assertEquals(sliceAt(10f, sizes), 0)
        assertEquals(sliceAt(200f, sizes), 1)
        assertEquals(sliceAt(300f, sizes), 2)
        assertEquals(sliceAt(-10f, sizes), 2)
        assertNull(sliceAt(10f, listOf(0L)))
    }

    @Test
    fun `cross-filters toggle, years are one-year ranges, chips list them`() {
        var f = SearchFilters()
        f = f.toggleStat("language", "fr").toggleStat("year", "2024").toggleStat("origin", "dht")
        assertEquals(f.languages, setOf("fr"))
        assertEquals(f.minYear, 2024)
        assertEquals(f.maxYear, 2024)
        assertEquals(f.selectedStat("year"), setOf("2024"))
        assertEquals(f.statChips(), listOf("language" to "fr", "origin" to "dht", "year" to "2024"))
        // A second tap removes it; a publisher category is not a filter.
        f = f.toggleStat("year", "2024").toggleStat("subcategory", "filmvidéo")
        assertNull(f.minYear)
        assertEquals(f.selectedStat("year"), emptySet<String>())
        assertEquals(f.statChips(), listOf("language" to "fr", "origin" to "dht"))
        assertEquals(SearchFilters(minYear = 2000, maxYear = 2010).selectedStat("year"), emptySet<String>())
    }

    @Test
    fun `labels and fields`() {
        assertEquals(valueLabel("origin", "dht"), "DHT")
        assertEquals(valueLabel("language", "fr-ca"), "French (Québec)")
        assertEquals(valueLabel("category", "video"), "Video")
        assertEquals(valueLabel("subcategory", "filmvidéo"), "filmvidéo")
        assertTrue("origin" in breakdownFields(setOf("stats", "origin")))
        assertFalse("origin" in breakdownFields(setOf("stats")))
        assertFalse(Activity.offered(dht = false).any { it.dht })
        assertTrue(Activity.offered(dht = true).map { it.metric }.containsAll(listOf("dht_torrents_added", "dht_hashes_seen", "dht_blocked")))
    }

    @Test
    fun `seed checks keep not checked yet apart from the results`() {
        // Production on 2026-10-27.
        val l = LivenessStats(torrents = 957_462, neverChecked = 367_301, alive = 340_984, dead = 233_018, unknown = 16_159)
        val s = seedChecks(l)
        assertEquals(s.checked, 590_161)
        assertEquals(s.notChecked, 367_301)
        assertEquals(s.torrents, 957_462)
        assertEquals(formatShare(s.notChecked, s.torrents, en), "38%")
        val lines = backlogLines(l.copy(enabled = true, dueNow = 431_893, checksPerHour = 30_000), en).toMap()
        assertEquals(lines["Due now"], "431,893 (cleared ≈ 14 h)")
        assertEquals(lines["Never checked"], "367,301 of 957,462")
        assertFalse(lines.containsKey("Checker"))
        assertEquals(backlogLines(LivenessStats(enabled = false), en).first(), "Checker" to "turned off on the server")
    }

    @Test
    fun `index and DHT lines`() {
        val idx = indexLines(SearchIndexStatus(state = "building", buildDone = 25, buildTotal = 100, docs = 10, pending = 3, lagSecs = 90), en).toMap()
        assertEquals(idx["State"], "building (25%)")
        assertEquals(idx["Changes queued"], "3 (oldest 1 min)")
        val d = DhtStats(
            mode = "aggressive", running = true, uptimeSecs = 508, virtualNodes = 4, externalIp = "public",
            queriesPerSec = 2779.27, answeredShare = 0.427, incomingPerMin = 28_992.5, hashesPerMin = 18_145.7,
            queueLen = 347, metadataPerMin = 1707.6, fetchSuccessRate = 0.23, storedTotal = 455_540,
            adultTotal = 83_257, blockedTotal = 4_982,
            upnp = UpnpStatus(state = "mapped", mappings = listOf(UpnpMapping("udp", 6891), UpnpMapping("tcp", 6891))),
        )
        val lines = dhtLines(d, en).toMap()
        assertEquals(lines["Crawler"], "aggressive · 4 nodes · up 8 min")
        assertEquals(lines["Discovery"], "18,146/min new hashes")
        assertEquals(lines["Stored"], "455,540 torrents (83,257 adult)")
        // The blocklist shows up as a number, nothing else.
        assertEquals(lines["Blocked (illegal content)"], "4,982")
        assertEquals(lines["Router ports"], "udp 6891, tcp 6891")
        assertEquals(lines["Address"], "public")
        assertEquals(dhtLines(DhtStats(mode = "polite", running = false, lastError = "bind failed"), en).first().second, "restarting after an error: bind failed")
    }
}

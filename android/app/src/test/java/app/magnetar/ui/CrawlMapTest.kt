package app.magnetar.ui

import app.magnetar.api.ApiException
import app.magnetar.api.CrawlMachine
import app.magnetar.api.CrawlMap
import app.magnetar.api.CrawlSources
import app.magnetar.api.DhtSources
import app.magnetar.api.GeoCount
import app.magnetar.api.RouteCount
import app.magnetar.api.RouteHour
import app.magnetar.api.TopRelay
import app.magnetar.ui.crawlmap.CrawlMapViewModel
import app.magnetar.ui.crawlmap.LogScale
import app.magnetar.ui.crawlmap.MapLayer
import app.magnetar.ui.crawlmap.MapWindow
import app.magnetar.ui.crawlmap.WorldMap
import app.magnetar.ui.crawlmap.buildMonth
import app.magnetar.ui.crawlmap.countryCounts
import app.magnetar.ui.crawlmap.countryName
import app.magnetar.ui.crawlmap.geoReasonMessage
import app.magnetar.ui.crawlmap.metricIndex
import app.magnetar.ui.crawlmap.networkLabel
import app.magnetar.ui.crawlmap.topBy
import app.magnetar.ui.crawlmap.totalOf
import app.magnetar.ui.stats.StatsViewModel
import app.magnetar.ui.stats.formatConversion
import app.magnetar.ui.stats.hourRoutes
import app.magnetar.ui.stats.machineLines
import app.magnetar.ui.stats.machineTitle
import app.magnetar.ui.stats.orderedRoutes
import app.magnetar.ui.stats.relayPlace
import app.magnetar.ui.stats.relaySourceLabel
import app.magnetar.ui.stats.routeLabel
import app.magnetar.ui.stats.shortRelayUrl
import java.io.File
import java.io.IOException
import java.util.Locale
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.advanceUntilIdle
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.test.setMain
import org.testng.Assert.assertEquals
import org.testng.Assert.assertFalse
import org.testng.Assert.assertNotNull
import org.testng.Assert.assertNull
import org.testng.Assert.assertTrue
import org.testng.Assert.expectThrows
import org.testng.annotations.AfterMethod
import org.testng.annotations.Test

@OptIn(ExperimentalCoroutinesApi::class)
class CrawlMapTest {
    private val en = Locale.US
    private val fr = Locale.FRANCE

    @AfterMethod
    fun reset() = Dispatchers.resetMain()

    private fun TestScope.main() = Dispatchers.setMain(StandardTestDispatcher(testScheduler))

    // --- World map asset ---

    private fun assetBytes(): ByteArray {
        val rel = "src/main/assets/${WorldMap.ASSET}"
        val f = listOf(File(rel), File("app/$rel"), File("android/app/$rel")).first { it.isFile }
        return f.readBytes()
    }

    /** Map units of a longitude / latitude. */
    private fun WorldMap.at(lon: Double, lat: Double) = Pair(((lon - west) * scale).toFloat(), ((north - lat) * scale).toFloat())

    @Test
    fun `the world asset parses with the known countries`() {
        val bytes = assetBytes()
        assertTrue(bytes.size < 100 * 1024, "asset is ${bytes.size} bytes")
        val m = WorldMap.parse(bytes)
        assertEquals(m.scale, 10)
        assertEquals(m.west, -180)
        assertEquals(m.north, 84)
        assertEquals(m.width, 3600)
        assertEquals(m.height, 1420)
        assertTrue(m.countries.size in 150..200, "${m.countries.size} countries")
        // ISO_A2 is -99 for France and Norway in Natural Earth: ISO_A2_EH is used instead.
        for (c in listOf("FR", "US", "NO", "DE", "BR", "CN", "RU", "AU", "JP", "XK", "TW")) assertNotNull(m.country(c), c)
        assertNull(m.country("AQ"), "Antarctica is cropped out")
        assertEquals(m.countries.map { it.code }.toSet().size, m.countries.size, "one entry per code")
        for (c in m.countries) {
            assertTrue(c.code.matches(Regex("[A-Z]{2}")), c.code)
            assertTrue(c.rings.isNotEmpty() && c.rings.size < 40, "${c.code}: ${c.rings.size} rings")
            for (r in c.rings) {
                assertTrue(r.size >= 6 && r.size % 2 == 0, c.code)
                for (i in r.indices) assertTrue(r[i] in 0..(if (i % 2 == 0) m.width else m.height), c.code)
            }
        }
        // Multi-part countries keep their parts: the US (Alaska, Hawaii), France (Corsica, Guiana).
        assertTrue(m.country("US")!!.rings.size >= 3)
        assertTrue(m.country("FR")!!.rings.size >= 2)
        assertTrue(m.countries.sumOf { it.points } in 3000..20000)
    }

    @Test
    fun `a point finds its country`() {
        val m = WorldMap.parse(assetBytes())
        fun at(lon: Double, lat: Double) = m.at(lon, lat).let { (x, y) -> m.countryAt(x, y)?.code }
        assertEquals(at(2.35, 48.85), "FR") // Paris
        assertEquals(at(-100.0, 40.0), "US")
        assertEquals(at(10.75, 59.9), "NO") // Oslo
        assertEquals(at(13.4, 52.5), "DE") // Berlin
        assertEquals(at(-47.9, -15.8), "BR") // Brasília
        assertEquals(at(139.7, 35.7), "JP") // Tokyo
        assertNull(at(-30.0, 30.0), "Atlantic")
    }

    @Test
    fun `a broken asset is refused`() {
        val bytes = assetBytes()
        expectThrows(IllegalArgumentException::class.java) { WorldMap.parse(bytes.copyOf(bytes.size - 3)) }
        expectThrows(IllegalArgumentException::class.java) { WorldMap.parse(bytes + byteArrayOf(0)) }
        expectThrows(IllegalArgumentException::class.java) { WorldMap.parse("nope".toByteArray()) }
        expectThrows(IllegalArgumentException::class.java) { WorldMap.parse(ByteArray(0)) }
    }

    // --- Scale and labels ---

    @Test
    fun `log scale shades`() {
        val s = LogScale.of(listOf(0L, 10L, 100L, 1000L))!!
        assertEquals(s.min, 10L)
        assertEquals(s.max, 1000L)
        assertNull(s.shade(0))
        assertEquals(s.shade(10)!!, 0f, 1e-6f)
        assertEquals(s.shade(100)!!, 0.5f, 1e-6f)
        assertEquals(s.shade(1000)!!, 1f, 1e-6f)
        assertEquals(s.shade(5000)!!, 1f, 1e-6f)
        assertEquals(LogScale.of(listOf(7L))!!.shade(7), 1f)
        assertNull(LogScale.of(listOf(0L, 0L)))
        assertNull(LogScale.of(emptyList()))
    }

    @Test
    fun `country names, months, networks and reasons`() {
        assertEquals(countryName("DE", en), "Germany")
        assertEquals(countryName("de", fr), "Allemagne")
        assertEquals(countryName("ZZZ", en), "ZZZ")
        assertEquals(countryName("1", en), "1")
        assertEquals(buildMonth(1790000000, en), "September 2026")
        assertEquals(networkLabel(GeoCount(asn = 24940, asOrg = "Hetzner Online GmbH")), "AS24940 Hetzner Online GmbH")
        assertEquals(networkLabel(GeoCount(asn = 3320)), "AS3320")
        assertEquals(networkLabel(GeoCount()), "Unknown network")
        assertTrue(geoReasonMessage("missing").contains("not downloaded yet"))
        assertTrue(geoReasonMessage("disabled").contains("is off"))
        assertTrue(geoReasonMessage(null).isNotBlank())
    }

    private val sample = CrawlMap(
        layer = "dht", window = "day", available = true, metrics = listOf("hashes", "stored", "nodes"),
        countries = listOf(
            GeoCount(country = "US", counts = listOf(120_000, 3_000, 40_000)),
            GeoCount(country = "de", counts = listOf(5_000, 4_000, 800)),
            GeoCount(country = "FR", counts = listOf(0, 0, 10)),
            GeoCount(country = null, counts = listOf(9, 9, 9)),
        ),
        networks = listOf(GeoCount(country = "DE", asn = 24940, asOrg = "Hetzner", counts = listOf(5_000, 200, 800))),
        unknown = listOf(1_000, 10, 50),
        totals = listOf(400_000, 9_000, 120_000),
    )

    @Test
    fun `counts per country and top lists follow the metric`() {
        assertEquals(metricIndex(sample, "stored"), 1)
        assertEquals(metricIndex(sample, "relays"), 0)
        assertEquals(metricIndex(sample, null), 0)
        assertEquals(countryCounts(sample, 0), mapOf("US" to 120_000L, "DE" to 5_000L))
        assertEquals(countryCounts(sample, 2)["FR"], 10L)
        assertEquals(topBy(sample.countries, 1, 10).map { it.country }, listOf("de", "US", null))
        assertEquals(topBy(sample.countries, 0, 1).single().country, "US")
        assertEquals(totalOf(sample, 1), 9_000L)
        assertEquals(totalOf(sample.copy(totals = emptyList()), 0), 120_000L + 5_000 + 9 + 1_000)
    }

    // --- Sources helpers ---

    @Test
    fun `routes, conversion and relay labels`() {
        assertEquals(routeLabel("get_peers"), "Lookups (get_peers)")
        assertEquals(routeLabel("sample"), "Sampling (BEP 51)")
        assertEquals(routeLabel("announce"), "Announces")
        assertEquals(
            orderedRoutes(listOf(RouteCount("x"), RouteCount("announce"), RouteCount("sample"), RouteCount("get_peers"))).map { it.via },
            listOf("sample", "get_peers", "announce", "x"),
        )
        assertEquals(hourRoutes(listOf(RouteHour(seen = mapOf("announce" to 1, "zz" to 2), stored = mapOf("sample" to 1)))), listOf("sample", "announce", "zz"))
        assertEquals(formatConversion(1_009_417, 4_256_462, en), "24%")
        assertEquals(formatConversion(143_237, 2_156_244, en), "6.6%")
        assertTrue(formatConversion(143_237, 2_156_244, fr).startsWith("6,6"), "French decimal comma")
        assertEquals(formatConversion(1, 1_000_000, en), "<0.1%")
        assertEquals(formatConversion(0, 100, en), "0%")
        assertEquals(formatConversion(5, 0, en), "–")
        assertEquals(relaySourceLabel("nip66"), "NIP-66 monitors")
        assertEquals(relaySourceLabel("other"), "other")
        assertEquals(relayPlace(TopRelay(country = "de", asn = 24940, asOrg = "Hetzner Online GmbH")), "DE · Hetzner Online GmbH (AS24940)")
        assertEquals(relayPlace(TopRelay(asn = 3320)), "AS3320")
        assertNull(relayPlace(TopRelay()))
        assertEquals(shortRelayUrl("wss://relay.example/"), "relay.example")
    }

    @Test
    fun `machine lines`() {
        val m = CrawlMachine(name = "eee", kind = "worker", connected = true, dhtTriaged = 120_000, dhtStored = 8_000, seedChecks = 30_000, storedPerMin = 80.5)
        assertEquals(machineTitle(m), "eee (worker)")
        val lines = machineLines(m, en).toMap()
        assertEquals(lines["Triaged"], "120,000")
        assertEquals(lines["Stored"], "8,000 · 81/min")
        assertEquals(lines["Seed checks"], "30,000")
        assertFalse("Queued" in lines)
    }

    // --- View models ---

    private fun sourcesFake(vararg extra: String) = FakeService().apply {
        features = listOf("filters", "stats", "graphs") + extra
        sourcesAnswer = {
            CrawlSources(
                dht = DhtSources(routes = listOf(RouteCount("sample", 100, 25))),
                machines = listOf(CrawlMachine(name = "server", kind = "server", connected = true)),
                computedAt = 5,
            )
        }
    }

    @Test
    fun `the sources card loads only when the server lists crawl_sources`() = runTest {
        main()
        val fake = sourcesFake("crawl_sources", "crawl_map")
        val vm = StatsViewModel(repo(fake))
        advanceUntilIdle()
        val s = vm.state.value
        assertTrue(s.hasCrawlSources && s.hasCrawlMap)
        assertEquals(s.sources?.dht?.routes?.single()?.stored, 25L)
        assertNull(s.sourcesCachedAtMs)
        assertEquals(fake.sourcesCalls, 1)

        // An older server: no request, no card, no map button.
        val old = sourcesFake()
        val vm2 = StatsViewModel(repo(old))
        advanceUntilIdle()
        assertFalse(vm2.state.value.hasCrawlSources)
        assertFalse(vm2.state.value.hasCrawlMap)
        assertNull(vm2.state.value.sources)
        assertEquals(old.sourcesCalls, 0)

        // Sources without the map: the card, no button.
        val noMap = sourcesFake("crawl_sources")
        val vm3 = StatsViewModel(repo(noMap))
        advanceUntilIdle()
        assertTrue(vm3.state.value.hasCrawlSources)
        assertFalse(vm3.state.value.hasCrawlMap)
    }

    @Test
    fun `the sources card is cached and kept offline`() = runTest {
        main()
        val fake = sourcesFake("crawl_sources")
        val r = repo(fake)
        StatsViewModel(r)
        advanceUntilIdle()
        // The server is down when the tab opens: the cached features and answer are used.
        fake.healthError = ApiException.Network(IOException("down"))
        fake.sourcesError = ApiException.Network(IOException("down"))
        val vm = StatsViewModel(r)
        advanceUntilIdle()
        val s = vm.state.value
        assertTrue(s.hasCrawlSources)
        assertEquals(s.sources?.computedAt, 5L)
        assertNotNull(s.sourcesCachedAtMs)
        assertNotNull(s.sourcesError)

        // Another error clears it: it would mislead.
        fake.healthError = null
        fake.sourcesError = ApiException.Http(500, "boom", "boom")
        vm.refresh()
        advanceUntilIdle()
        assertNull(vm.state.value.sources)
        assertEquals(vm.state.value.sourcesError, "boom")
    }

    private fun mapFake() = FakeService().apply {
        features = listOf("stats", "crawl_sources", "crawl_map")
        mapAnswer = { l, w ->
            if (l == "dht") {
                sample.copy(window = w)
            } else {
                CrawlMap(layer = l, window = w, available = false, reason = "missing", metrics = listOf("relays", "events", "torrents"))
            }
        }
    }

    @Test
    fun `the map loads per layer and window, the metric resets with the layer`() = runTest {
        main()
        val fake = mapFake()
        val vm = CrawlMapViewModel(repo(fake))
        advanceUntilIdle()
        assertEquals(fake.mapCalls, listOf("dht" to "day"))
        var s = vm.state.value
        assertFalse(s.loading)
        assertEquals(s.map?.layer, "dht")
        assertEquals(s.metricName, "hashes")
        vm.pickMetric("nodes")
        assertEquals(vm.state.value.metricIndex, 2)

        vm.setWindow(MapWindow.ALL)
        advanceUntilIdle()
        assertEquals(fake.mapCalls.last(), "dht" to "all")
        assertEquals(vm.state.value.metricName, "nodes", "the metric stays with the window")

        vm.setLayer(MapLayer.NOSTR)
        advanceUntilIdle()
        s = vm.state.value
        assertEquals(fake.mapCalls.last(), "nostr" to "all")
        assertEquals(s.metricName, "relays")
        assertFalse(s.map!!.available)
        assertEquals(s.map!!.reason, "missing")

        // Same choice again: no request.
        val n = fake.mapCalls.size
        vm.setLayer(MapLayer.NOSTR)
        vm.setWindow(MapWindow.ALL)
        advanceUntilIdle()
        assertEquals(fake.mapCalls.size, n)
    }

    @Test
    fun `a newer choice cancels the map in flight`() = runTest {
        main()
        val fake = mapFake().apply { mapDelayMs = 1_000 }
        val vm = CrawlMapViewModel(repo(fake))
        vm.setLayer(MapLayer.NOSTR)
        advanceUntilIdle()
        assertEquals(vm.state.value.map?.layer, "nostr")
        assertEquals(vm.state.value.layer, MapLayer.NOSTR)
    }

    @Test
    fun `tap selects a country, again clears it`() = runTest {
        main()
        val vm = CrawlMapViewModel(repo(mapFake()))
        advanceUntilIdle()
        vm.select("de")
        assertEquals(vm.state.value.selected, "DE")
        vm.select("DE")
        assertNull(vm.state.value.selected)
        vm.select("US")
        vm.select(null)
        assertNull(vm.state.value.selected)
    }

    @Test
    fun `the map is cached per request and shown offline with its age`() = runTest {
        main()
        val fake = mapFake()
        val r = repo(fake)
        CrawlMapViewModel(r)
        advanceUntilIdle()
        fake.mapError = ApiException.Network(IOException("down"))
        val vm = CrawlMapViewModel(r)
        advanceUntilIdle()
        var s = vm.state.value
        assertEquals(s.map?.countries?.size, 4)
        assertNotNull(s.cachedAtMs)
        assertNotNull(s.error)
        // Another request (window) has no cached answer: nothing is shown for it.
        vm.setWindow(MapWindow.ALL)
        advanceUntilIdle()
        s = vm.state.value
        assertNull(s.map)
        assertNull(s.cachedAtMs)
        assertNotNull(s.error)
    }

    @Test
    fun `a server without the map says so`() = runTest {
        main()
        val fake = mapFake().apply { mapError = ApiException.NotFound("not found") }
        val vm = CrawlMapViewModel(repo(fake))
        advanceUntilIdle()
        assertTrue(vm.state.value.missing)
        assertNull(vm.state.value.map)
    }
}

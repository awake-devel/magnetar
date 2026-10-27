package app.magnetar.ui

import app.magnetar.api.ApiException
import app.magnetar.api.Breakdown
import app.magnetar.api.BreakdownSet
import app.magnetar.api.FacetCount
import app.magnetar.api.Graph
import app.magnetar.api.GraphEdge
import app.magnetar.api.GraphNode
import app.magnetar.api.LivenessStats
import app.magnetar.api.StatsHealth
import app.magnetar.api.Timeseries
import app.magnetar.api.TimeseriesPoint
import app.magnetar.api.TimeseriesSet
import app.magnetar.api.TrustSettings
import app.magnetar.settings.AppSettings
import app.magnetar.settings.PRIVATE_KEY_ERROR
import app.magnetar.ui.graph.GraphViewModel
import app.magnetar.ui.settings.SettingsViewModel
import app.magnetar.ui.stats.Activity
import app.magnetar.ui.stats.StatsViewModel
import java.io.IOException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.UnconfinedTestDispatcher
import kotlinx.coroutines.test.advanceUntilIdle
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.test.setMain
import org.testng.Assert.assertEquals
import org.testng.Assert.assertFalse
import org.testng.Assert.assertNotNull
import org.testng.Assert.assertNull
import org.testng.Assert.assertTrue
import org.testng.annotations.AfterMethod
import org.testng.annotations.Test

@OptIn(ExperimentalCoroutinesApi::class)
class StatsSettingsViewModelTest {
    private val hex = "3bf0c63fcb93463407af97a5e5ee64fa883d107ef9e558472c4eb9aaaefa459d"
    private val npub = "npub180cvv07tjdrrgpa0j7j7tmnyl2yr6yr7l8j4s3evf6u64th6gkwsyjh6w6"

    @AfterMethod
    fun reset() = Dispatchers.resetMain()

    private fun TestScope.main() = Dispatchers.setMain(StandardTestDispatcher(testScheduler))

    private fun statsFake() = FakeService().apply {
        features = listOf("filters", "facet_set", "stats", "graphs", "adult_filter", "origin", "dht")
        breakdownAnswer = { r ->
            BreakdownSet(
                total = 100,
                breakdowns = r.fields.map { f ->
                    Breakdown(field = f, total = 100, counts = listOf(FacetCount(if (f == "year") "2024" else "$f-a", 60), FacetCount("$f-b", 40)))
                },
            )
        }
        statsHealthAnswer = { StatsHealth(torrents = 10, liveness = LivenessStats(torrents = 10, neverChecked = 4, alive = 3, dead = 2, unknown = 1)) }
        timeseriesAnswer = { m, b ->
            TimeseriesSet(series = listOf(Timeseries(metric = m.first(), kind = "counter", bucket = b, points = listOf(TimeseriesPoint(1, 5)))))
        }
    }

    // --- Stats ---

    @Test
    fun `stats load health, breakdowns with the adult switch, years and the series`() = runTest {
        main()
        val fake = statsFake()
        val vm = StatsViewModel(repo(fake))
        advanceUntilIdle()
        val s = vm.state.value
        assertTrue(s.hasStats && s.hasGraphs && s.hasDht)
        assertEquals(s.stats?.torrents, 10)
        assertEquals(s.breakdowns?.total, 100)
        assertEquals(s.years?.counts?.first()?.value, "2024")
        assertEquals(s.series?.metric, "torrents_total")
        val main = fake.breakdownRequests.first { "year" !in it.fields }
        assertTrue("origin" in main.fields)
        assertEquals(main.top, 10)
        assertFalse(main.query.includeAdult, "adult hidden by default")
        assertEquals(fake.breakdownRequests.first { "year" in it.fields }.top, 50)
    }

    @Test
    fun `a tap cross-filters every chart, a newer tap wins, and the switch is sent`() = runTest {
        main()
        val fake = statsFake()
        val settings = MutableStateFlow(AppSettings(serverUrl = "http://server.test", apiToken = "t", showAdult = true))
        val vm = StatsViewModel(repo(fake, settings))
        advanceUntilIdle()
        fake.breakdownRequests.clear()
        fake.breakdownDelayMs = 1_000
        vm.toggle("language", "fr")
        vm.toggle("resolution", "1080p")
        advanceUntilIdle()
        val main = fake.breakdownRequests.filter { "year" !in it.fields }
        // The first tap's request was cancelled by the second; the last one has both filters.
        assertEquals(main.last().query.languages, listOf("fr"))
        assertEquals(main.last().query.resolutions, listOf("1080p"))
        assertTrue(main.last().query.includeAdult)
        assertEquals(vm.state.value.filters.languages, setOf("fr"))
        // A field that is not a filter does nothing; Clear empties them.
        vm.toggle("subcategory", "x")
        vm.clearFilters()
        advanceUntilIdle()
        assertEquals(fake.breakdownRequests.last().query.languages, emptyList<String>())
    }

    @Test
    fun `long press searches with the filters, a publisher category as text`() = runTest {
        main()
        val fake = statsFake()
        val r = repo(fake)
        val vm = StatsViewModel(r)
        advanceUntilIdle()
        vm.toggle("language", "fr")
        val q = vm.searchFor("year", "2024")
        assertEquals(q.languages, listOf("fr"))
        assertEquals(q.minYear, 2024)
        assertEquals(q.maxYear, 2024)
        assertEquals(q.text, "")
        assertFalse(q.includeAdult)
        assertEquals(vm.searchFor("subcategory", "filmvidéo").text, "filmvidéo")
        // Long press on an already selected value keeps it selected.
        assertEquals(vm.searchFor("language", "fr").languages, listOf("fr"))
        vm.openSearch()
        assertEquals(r.pendingSearch.value?.languages, listOf("fr"))
    }

    @Test
    fun `activity picks a metric and hours`() = runTest {
        main()
        val fake = statsFake()
        val vm = StatsViewModel(repo(fake))
        advanceUntilIdle()
        vm.pickActivity(Activity.DHT_ADDED)
        advanceUntilIdle()
        vm.setHourly(true)
        advanceUntilIdle()
        assertEquals(fake.timeseriesCalls.last(), listOf("dht_torrents_added") to "hour")
        assertEquals(vm.state.value.series?.bucket, "hour")
    }

    @Test
    fun `offline shows the cached breakdowns with a banner, an older server says so`() = runTest {
        main()
        val fake = statsFake()
        val r = repo(fake)
        StatsViewModel(r)
        advanceUntilIdle()
        fake.breakdownError = ApiException.Network(IOException("down"))
        val vm2 = StatsViewModel(r)
        advanceUntilIdle()
        assertEquals(vm2.state.value.breakdowns?.total, 100)
        assertNotNull(vm2.state.value.breakdownError)

        val old = FakeService().apply { features = listOf("filters") }
        val vm3 = StatsViewModel(repo(old))
        advanceUntilIdle()
        assertFalse(vm3.state.value.hasStats)
        assertTrue(old.breakdownRequests.isEmpty())
    }

    @Test
    fun `no server, no requests`() = runTest {
        main()
        val fake = statsFake()
        val vm = StatsViewModel(repo(fake, MutableStateFlow(AppSettings())))
        advanceUntilIdle()
        assertFalse(vm.state.value.hasServer)
        assertEquals(fake.healthCalls, 0)
    }

    // --- Graphs ---

    @Test
    fun `graph loads and lays out, unknown item is missing`() = runTest {
        main()
        val fake = FakeService()
        fake.graphAnswer = { p ->
            if (p == "swarm/x") throw ApiException.NotFound("no")
            Graph(
                kind = "wot",
                nodes = listOf(GraphNode(id = hex, kind = "root"), GraphNode(id = "b", kind = "publisher", score = 40), GraphNode(id = "c", kind = "publisher")),
                edges = listOf(GraphEdge(0, 1), GraphEdge(0, 2)),
            )
        }
        val vm = GraphViewModel(repo(fake), "wot", layoutDispatcher = UnconfinedTestDispatcher(testScheduler))
        advanceUntilIdle()
        val s = vm.state.value
        assertEquals(s.graph?.nodes?.size, 3)
        assertTrue(s.settled)
        assertEquals(s.x.size, 3)
        // The subject (trust root) is pinned at the centre.
        assertEquals(s.x[0], 0f)
        vm.select(1)
        assertEquals(vm.state.value.selected, 1)
        vm.select(9)
        assertNull(vm.state.value.selected)
        val missing = GraphViewModel(repo(fake), "swarm/x", layoutDispatcher = UnconfinedTestDispatcher(testScheduler))
        advanceUntilIdle()
        assertTrue(missing.state.value.missing)
    }

    // --- Settings ---

    private class Store(initial: AppSettings) {
        val flow = MutableStateFlow(initial)
        fun update(t: (AppSettings) -> AppSettings) {
            flow.value = t(flow.value)
        }
    }

    private fun TestScope.settingsVm(fake: FakeService, store: Store) =
        SettingsViewModel(repo(fake, store.flow), store::update) { s ->
            if (s.serverUrl.isBlank()) throw ApiException.NotConfigured()
            fake
        }

    @Test
    fun `a pasted nsec is cleared at once and never stored`() = runTest {
        main()
        val store = Store(AppSettings(serverUrl = "http://server.test", apiToken = "t"))
        val vm = settingsVm(FakeService(), store)
        advanceUntilIdle()
        vm.setRootKey("nsec1vl029mgpspedva04g90vltkh6fvh240zqtv9k0t9af8935ke9laqsnlfe5")
        assertEquals(vm.state.value.form.rootKey, "")
        assertEquals(vm.state.value.root.error, PRIVATE_KEY_ERROR)
        assertTrue(vm.saveNow())
        assertEquals(store.flow.value.rootNpub, "")
    }

    @Test
    fun `save stores the form and pushes root and threshold when they differ`() = runTest {
        main()
        val fake = FakeService().apply { trust = TrustSettings(root = null, threshold = 20) }
        val store = Store(AppSettings(serverUrl = "http://server.test", apiToken = "t"))
        val vm = settingsVm(fake, store)
        advanceUntilIdle()
        vm.setRootKey(npub)
        vm.setThreshold(35)
        assertTrue(vm.state.value.dirty)
        assertTrue(vm.saveNow())
        assertEquals(store.flow.value.rootNpub, npub)
        assertEquals(store.flow.value.trustThreshold, 35)
        assertEquals(fake.trustPuts.single(), TrustSettings(root = hex, threshold = 35))
        assertFalse(vm.state.value.dirty)
        // Saving again with nothing changed sends nothing.
        assertTrue(vm.saveNow())
        assertEquals(fake.trustPuts.size, 1)
    }

    @Test
    fun `an empty root field takes the server's root and never clears it by itself`() = runTest {
        main()
        val fake = FakeService().apply { trust = TrustSettings(root = hex, threshold = 20) }
        val store = Store(AppSettings(serverUrl = "http://server.test", apiToken = "t", trustThreshold = 30))
        val vm = settingsVm(fake, store)
        advanceUntilIdle()
        assertEquals(vm.state.value.form.rootKey, npub)
        assertFalse(vm.state.value.dirty)
        // The migrated threshold differs from the server's: Save pushes it, keeping the root.
        vm.setThreshold(31)
        assertTrue(vm.saveNow())
        assertEquals(fake.trustPuts.single(), TrustSettings(root = hex, threshold = 31))

        // Offline at open (no prefill), then Save: the server's root is kept.
        val fake2 = FakeService().apply { trust = TrustSettings(root = hex, threshold = 20); trustError = ApiException.Network(IOException("x")) }
        val vm2 = settingsVm(fake2, Store(AppSettings(serverUrl = "http://server.test", apiToken = "t")))
        advanceUntilIdle()
        assertEquals(vm2.state.value.form.rootKey, "")
        fake2.trustError = null
        vm2.setThreshold(40)
        assertTrue(vm2.saveNow())
        assertEquals(fake2.trustPuts.single(), TrustSettings(root = hex, threshold = 40))

        // Emptied on purpose: the root is cleared on the server.
        vm.setRootKey("")
        assertTrue(vm.saveNow())
        assertEquals(fake.trustPuts.last(), TrustSettings(root = null, threshold = 31))
    }

    @Test
    fun `invalid fields block saving`() = runTest {
        main()
        val store = Store(AppSettings())
        val vm = settingsVm(FakeService(), store)
        vm.setServerUrl("ftp://x")
        assertFalse(vm.state.value.canSave)
        assertFalse(vm.saveNow())
        vm.setServerUrl("https://ryoku.tail26814a.ts.net/")
        vm.setRootKey("hello")
        assertFalse(vm.saveNow())
        vm.setRootKey("")
        assertTrue(vm.saveNow())
        assertEquals(store.flow.value.serverUrl, "https://ryoku.tail26814a.ts.net")
    }

    @Test
    fun `test connection reports the server, a rejected token and missing features`() = runTest {
        main()
        val fake = FakeService().apply { features = listOf("filters", "stats", "graphs") }
        val vm = settingsVm(fake, Store(AppSettings()))
        vm.testConnection()
        assertEquals(vm.state.value.testOk, false)
        vm.setServerUrl("http://server.test")
        vm.setToken("t")
        vm.testConnection()
        advanceUntilIdle()
        assertEquals(vm.state.value.testOk, true)
        assertTrue(vm.state.value.testResult!!.contains("token accepted"))
        assertTrue(vm.state.value.testResult!!.contains("saved-search alerts"))
        fake.trustError = ApiException.Unauthorized("bad token")
        vm.testConnection()
        advanceUntilIdle()
        assertEquals(vm.state.value.testOk, false)
        assertTrue(vm.state.value.testResult!!.contains("rejected the token"))
        fake.healthError = ApiException.Network(IOException("down"))
        vm.testConnection()
        advanceUntilIdle()
        assertEquals(vm.state.value.testResult, "Server unreachable")
    }

    @Test
    fun `the signer's hex key is shown as npub, switches apply at once`() = runTest {
        main()
        val store = Store(AppSettings(serverUrl = "http://server.test", apiToken = "t"))
        val vm = settingsVm(FakeService(), store)
        vm.onSignerKey(hex)
        assertEquals(vm.state.value.form.rootKey, npub)
        vm.onSignerKey("garbage")
        assertEquals(vm.state.value.form.rootKey, npub)
        vm.setShowAdult(true)
        assertTrue(store.flow.value.showAdult)
        vm.setNotify(false)
        assertFalse(store.flow.value.notifySavedSearches)
    }
}

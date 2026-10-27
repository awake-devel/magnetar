package app.magnetar.ui

import app.magnetar.api.ApiException
import app.magnetar.api.Category
import app.magnetar.api.FacetCount
import app.magnetar.api.FacetCounts
import app.magnetar.api.FacetSet
import app.magnetar.api.SearchPage
import app.magnetar.api.Sort
import app.magnetar.api.TorrentSummary
import app.magnetar.cache.PageCache
import app.magnetar.settings.AppSettings
import app.magnetar.ui.search.SEARCH_DEBOUNCE_MS
import app.magnetar.ui.search.SearchFilters
import app.magnetar.ui.search.SearchViewModel
import java.io.IOException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.advanceTimeBy
import kotlinx.coroutines.test.advanceUntilIdle
import kotlinx.coroutines.test.currentTime
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.runCurrent
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
class SearchViewModelTest {
    @AfterMethod
    fun reset() = Dispatchers.resetMain()

    private fun TestScope.vm(
        fake: FakeService,
        settings: MutableStateFlow<AppSettings> = MutableStateFlow(AppSettings(serverUrl = "http://server.test", apiToken = "t")),
        cache: PageCache? = PageCache(tempDir()),
        timings: MutableList<String> = mutableListOf(),
    ): SearchViewModel {
        Dispatchers.setMain(StandardTestDispatcher(testScheduler))
        return SearchViewModel(repo(fake, settings, cache), clock = { currentTime }, onTiming = { timings += it })
    }

    @Test
    fun `typing is debounced 250 ms and only the last text is searched`() = runTest {
        val fake = FakeService()
        val vm = vm(fake)
        advanceUntilIdle()
        fake.searches.clear()
        for (t in listOf("d", "du", "dun", "dune")) {
            vm.onTextChange(t)
            advanceTimeBy(100)
        }
        // 100 ms after the last keystroke: nothing sent yet.
        assertEquals(fake.searches.size, 0)
        advanceTimeBy(SEARCH_DEBOUNCE_MS)
        runCurrent()
        assertEquals(fake.searches.map { it.text }, listOf("dune"))
        advanceUntilIdle()
        assertEquals(vm.state.value.hits.single().title, "dune")
    }

    @Test
    fun `a newer search cancels the one in flight and its answer never shows`() = runTest {
        val fake = FakeService()
        // "slow" takes 5 s, "fast" answers at once.
        fake.searchDelayMs = { if (it.text == "slow") 5_000 else 0 }
        val vm = vm(fake)
        advanceUntilIdle()
        vm.onTextChange("slow")
        advanceTimeBy(SEARCH_DEBOUNCE_MS + 100)
        assertTrue(fake.searches.any { it.text == "slow" })
        vm.onTextChange("fast")
        advanceUntilIdle()
        assertEquals(vm.state.value.hits.single().title, "fast")
        assertFalse(fake.completedSearches.any { it.text == "slow" }, "the slow call was cancelled")
        assertFalse(vm.state.value.loading)
    }

    @Test
    fun `filters and sort run at once, without the debounce`() = runTest {
        val fake = FakeService()
        val vm = vm(fake)
        advanceUntilIdle()
        fake.searches.clear()
        vm.setFilters(SearchFilters(resolutions = setOf("2160p"), origins = setOf("dht")))
        runCurrent()
        assertEquals(fake.searches.single().resolutions, listOf("2160p"))
        assertEquals(fake.searches.single().origins, listOf("dht"))
        vm.setSort(Sort.SEEDERS)
        runCurrent()
        assertEquals(fake.searches.last().sort, Sort.SEEDERS)
        assertTrue(fake.searches.last().descending)
    }

    @Test
    fun `the adult switch is sent on every search and the result is not filtered by the app`() = runTest {
        val fake = FakeService()
        fake.searchAnswer = { SearchPage(hits = listOf(TorrentSummary(infohash = "a".repeat(40), title = "x", category = Category.PORN))) }
        val settings = MutableStateFlow(AppSettings(serverUrl = "http://server.test", apiToken = "t", showAdult = false))
        val vm = vm(fake, settings)
        advanceUntilIdle()
        assertFalse(fake.searches.last().includeAdult)
        // A server with adult_filter decides; the app shows what it answers (no stricter rule).
        assertEquals(vm.state.value.hits.size, 1)
        settings.value = settings.value.copy(showAdult = true)
        advanceUntilIdle()
        assertTrue(fake.searches.last().includeAdult)
    }

    @Test
    fun `a server without adult_filter gets the old client-side category rule only`() = runTest {
        val fake = FakeService()
        fake.features = listOf("filters")
        fake.searchAnswer = {
            SearchPage(
                hits = listOf(
                    TorrentSummary(infohash = "a".repeat(40), title = "adult", category = Category.PORN),
                    TorrentSummary(infohash = "b".repeat(40), title = "film", category = Category.VIDEO),
                ),
            )
        }
        val vm = vm(fake)
        advanceUntilIdle()
        assertEquals(vm.state.value.hits.map { it.title }, listOf("film"))
        // Picking the category shows them, as the server rule does.
        vm.setFilters(SearchFilters(categories = setOf("porn")))
        advanceUntilIdle()
        assertEquals(vm.state.value.hits.size, 2)
    }

    @Test
    fun `release filters are not sent to a server that would ignore them`() = runTest {
        val fake = FakeService()
        fake.features = listOf("facet_set")
        val vm = vm(fake)
        advanceUntilIdle()
        vm.setFilters(SearchFilters(resolutions = setOf("1080p"), origins = setOf("dht"), categories = setOf("video")))
        advanceUntilIdle()
        val q = fake.searches.last()
        assertEquals(q.resolutions, emptyList<String>())
        assertEquals(q.origins, emptyList<String>())
        assertEquals(q.categories, listOf("video"))
    }

    @Test
    fun `the cached first page shows at once, then the live one replaces it`() = runTest {
        val fake = FakeService()
        val cache = PageCache(tempDir())
        val first = vm(fake, cache = cache)
        advanceUntilIdle()
        first.onTextChange("dune")
        advanceUntilIdle()
        // A second session: the server is slow, the cache answers first.
        fake.searchDelayMs = { 3_000 }
        fake.searchAnswer = { SearchPage(hits = listOf(FakeService.hit("live"))) }
        val timings = mutableListOf<String>()
        val second = vm(fake, cache = cache, timings = timings)
        second.onTextChange("dune")
        advanceTimeBy(SEARCH_DEBOUNCE_MS + 1_000)
        assertEquals(second.state.value.hits.single().title, "dune")
        assertNotNull(second.state.value.cachedAtMs)
        assertTrue(second.state.value.loading)
        advanceUntilIdle()
        assertEquals(second.state.value.hits.single().title, "live")
        assertNull(second.state.value.cachedAtMs)
        assertTrue(timings.any { it.contains("cached len=4") } && timings.any { it.contains("live len=4") }, timings.toString())
    }

    @Test
    fun `offline keeps the cached page with the error, other errors clear it`() = runTest {
        val fake = FakeService()
        val cache = PageCache(tempDir())
        val vm = vm(fake, cache = cache)
        advanceUntilIdle()
        fake.searchError = ApiException.Network(IOException("down"))
        vm.submit()
        advanceUntilIdle()
        assertTrue(vm.state.value.offline)
        assertEquals(vm.state.value.hits.size, 1)
        assertNotNull(vm.state.value.cachedAtMs)
        fake.searchError = ApiException.Unauthorized("no")
        vm.submit()
        advanceUntilIdle()
        assertFalse(vm.state.value.offline)
        assertTrue(vm.state.value.hits.isEmpty())
        assertEquals(vm.state.value.error, "Token rejected: check it in Settings")
    }

    @Test
    fun `a lower-bound total is kept for the 1,000+ count`() = runTest {
        val fake = FakeService()
        fake.searchAnswer = { SearchPage(hits = listOf(FakeService.hit("x")), total = 1000, totalIsLowerBound = true) }
        val vm = vm(fake)
        advanceUntilIdle()
        assertEquals(vm.state.value.total, 1000)
        assertTrue(vm.state.value.totalIsLowerBound)
    }

    @Test
    fun `no server set shows the Settings hint and sends nothing`() = runTest {
        val fake = FakeService()
        val vm = vm(fake, MutableStateFlow(AppSettings()))
        advanceUntilIdle()
        assertTrue(vm.state.value.notConfigured)
        assertTrue(fake.searches.isEmpty())
    }

    @Test
    fun `an invalid server URL asks for Settings instead of saying unreachable`() = runTest {
        val fake = FakeService()
        Dispatchers.setMain(StandardTestDispatcher(testScheduler))
        val settings = MutableStateFlow(AppSettings(serverUrl = "not a url", apiToken = "t"))
        val r = app.magnetar.data.Repository(
            settings, { s -> app.magnetar.api.MagnetarApi.create(s.serverUrl, s.apiToken) }, null, backgroundScope,
            StandardTestDispatcher(testScheduler),
        )
        val vm = SearchViewModel(r, clock = { currentTime })
        advanceUntilIdle()
        assertTrue(vm.state.value.notConfigured)
        assertTrue(fake.searches.isEmpty())
    }

    @Test
    fun `the open sheet asks for live counts of its draft, debounced`() = runTest {
        val fake = FakeService()
        fake.facetAnswer = { FacetSet(total = 3, facets = listOf(FacetCounts("resolution", 3, listOf(FacetCount("1080p", 3))))) }
        val vm = vm(fake)
        advanceUntilIdle()
        vm.draftFilters(SearchFilters(languages = setOf("fr")))
        vm.draftFilters(SearchFilters(languages = setOf("fr", "en")))
        advanceUntilIdle()
        val req = fake.facetRequests.single()
        assertEquals(req.query.languages, listOf("en", "fr"))
        assertTrue("category" in req.fields && "origin" in req.fields && "resolution" in req.fields, req.fields.toString())
        assertEquals(vm.facets.value?.total, 3)
        vm.draftFilters(null)
        advanceUntilIdle()
        assertNull(vm.facets.value)
    }

    @Test
    fun `save search stores the text, filters, sort and adult switch on the server`() = runTest {
        val fake = FakeService()
        val vm = vm(fake)
        advanceUntilIdle()
        vm.onTextChange("dune")
        vm.setFilters(SearchFilters(resolutions = setOf("2160p"), minTrust = 40))
        vm.setSort(Sort.DATE)
        advanceUntilIdle()
        vm.saveSearch("Dune 4K", notify = true)
        advanceUntilIdle()
        val s = fake.saved.single()
        assertEquals(s.name, "Dune 4K")
        assertTrue(s.notify)
        assertEquals(s.query.text, "dune")
        assertEquals(s.query.resolutions, listOf("2160p"))
        assertEquals(s.query.minTrust, 40)
        assertEquals(s.query.sort, Sort.DATE)
        assertFalse(s.query.includeAdult)
        assertEquals(s.query.offset, 0)
    }

    @Test
    fun `load more appends the next page and stops at the end`() = runTest {
        val fake = FakeService()
        fake.searchAnswer = { q ->
            val n = if (q.offset == 0) 50 else 10
            SearchPage(hits = (0 until n).map { FakeService.hit("h${q.offset + it}", "%040d".format(q.offset + it)) }, total = 60)
        }
        val vm = vm(fake)
        advanceUntilIdle()
        assertFalse(vm.state.value.endReached)
        vm.loadMore()
        advanceUntilIdle()
        assertEquals(vm.state.value.hits.size, 60)
        assertTrue(vm.state.value.endReached)
        assertEquals(fake.searches.last().offset, 50)
    }

    @Test
    fun `running a saved search replaces text, filters and sort`() = runTest {
        val fake = FakeService()
        val vm = vm(fake)
        advanceUntilIdle()
        vm.runSaved(app.magnetar.api.SearchQuery(text = "ubuntu", categories = listOf("application"), sort = Sort.SEEDERS))
        advanceUntilIdle()
        assertEquals(vm.state.value.text, "ubuntu")
        assertEquals(vm.state.value.filters.categories, setOf("application"))
        assertEquals(fake.searches.last().sort, Sort.SEEDERS)
    }
}

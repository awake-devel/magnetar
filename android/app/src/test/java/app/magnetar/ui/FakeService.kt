package app.magnetar.ui

import app.magnetar.api.BreakdownRequest
import app.magnetar.api.BreakdownSet
import app.magnetar.api.CrawlMap
import app.magnetar.api.CrawlSources
import app.magnetar.api.FacetSet
import app.magnetar.api.Graph
import app.magnetar.api.StatsHealth
import app.magnetar.api.TimeseriesSet
import app.magnetar.api.TrustSettings
import app.magnetar.api.FacetsRequest
import app.magnetar.api.Health
import app.magnetar.api.Liveness
import app.magnetar.api.LivenessHistory
import app.magnetar.api.MagnetarService
import app.magnetar.api.MediaReleases
import app.magnetar.api.SavedSearch
import app.magnetar.api.SavedSearchPoll
import app.magnetar.api.SearchPage
import app.magnetar.api.SearchQuery
import app.magnetar.api.TorrentDetail
import app.magnetar.api.TorrentSummary
import app.magnetar.cache.PageCache
import app.magnetar.data.Repository
import app.magnetar.settings.AppSettings
import java.io.File
import java.nio.file.Files
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.StandardTestDispatcher

/** A server in memory: every call is recorded; answers and delays are set per test. */
open class FakeService : MagnetarService {
    var features = listOf("filters", "facet_set", "media_releases", "liveness_history", "adult_filter", "origin", "saved_search_poll")
    var searchDelayMs: (SearchQuery) -> Long = { 0 }
    var searchAnswer: (SearchQuery) -> SearchPage = { q -> SearchPage(hits = listOf(hit(q.text.ifBlank { "browse" }))) }
    var searchError: Exception? = null
    var facetAnswer: (FacetsRequest) -> FacetSet = { FacetSet() }
    var detailAnswer: (String) -> TorrentDetail = { ih -> TorrentDetail(summary = TorrentSummary(infohash = ih, title = "T $ih")) }
    var liveness: Liveness = Liveness(seeders = 7, leechers = 2, source = "udp", checkedAt = 1)
    var releases: MediaReleases = MediaReleases()
    val saved = mutableListOf<SavedSearch>()

    val searches = mutableListOf<SearchQuery>()
    val completedSearches = mutableListOf<SearchQuery>()
    val facetRequests = mutableListOf<FacetsRequest>()
    val releaseQueries = mutableListOf<Pair<Long, SearchQuery>>()
    val historyCalls = mutableListOf<String>()
    var checks = 0
    var updates = mutableListOf<SavedSearch>()

    var trust = TrustSettings(root = null, threshold = 20)
    val trustPuts = mutableListOf<TrustSettings>()
    var trustError: Exception? = null
    var statsHealthAnswer: () -> StatsHealth = { StatsHealth() }
    var breakdownAnswer: (BreakdownRequest) -> BreakdownSet = { BreakdownSet() }
    var breakdownError: Exception? = null
    var breakdownDelayMs = 0L
    val breakdownRequests = mutableListOf<BreakdownRequest>()
    var timeseriesAnswer: (List<String>, String) -> TimeseriesSet = { _, _ -> TimeseriesSet() }
    val timeseriesCalls = mutableListOf<Pair<List<String>, String>>()
    var graphAnswer: (String) -> Graph = { Graph() }
    val graphCalls = mutableListOf<String>()

    var sourcesAnswer: () -> CrawlSources = { CrawlSources() }
    var sourcesError: Exception? = null
    var sourcesCalls = 0
    var mapAnswer: (String, String) -> CrawlMap = { l, w -> CrawlMap(layer = l, window = w, available = true) }
    var mapError: Exception? = null
    var mapDelayMs = 0L
    val mapCalls = mutableListOf<Pair<String, String>>()

    var healthDelayMs = 0L
    var healthError: Exception? = null
    var healthCalls = 0

    override suspend fun health(): Health {
        healthCalls++
        kotlinx.coroutines.delay(healthDelayMs)
        healthError?.let { throw it }
        return Health(status = "ok", version = "test", db = true, features = features)
    }

    override suspend fun search(query: SearchQuery): SearchPage {
        searches += query
        kotlinx.coroutines.delay(searchDelayMs(query))
        searchError?.let { throw it }
        completedSearches += query
        return searchAnswer(query)
    }

    override suspend fun facets(request: FacetsRequest): FacetSet {
        facetRequests += request
        return facetAnswer(request)
    }

    override suspend fun detail(infohash: String) = detailAnswer(infohash)

    override suspend fun checkLiveness(infohash: String): Liveness {
        checks++
        return liveness
    }

    override suspend fun livenessHistory(infohash: String, days: Int): LivenessHistory {
        historyCalls += infohash
        return LivenessHistory(infohash = infohash, keepDays = 90)
    }

    override suspend fun mediaReleases(mediaId: Long, query: SearchQuery): MediaReleases {
        releaseQueries += mediaId to query
        return releases
    }

    override suspend fun savedSearches() = saved.toList()

    override suspend fun createSavedSearch(search: SavedSearch): SavedSearch {
        val s = search.copy(id = (saved.maxOfOrNull { it.id } ?: 0) + 1)
        saved += s
        return s
    }

    override suspend fun updateSavedSearch(search: SavedSearch): SavedSearch {
        updates += search
        saved.replaceAll { if (it.id == search.id) search else it }
        return search
    }

    override suspend fun deleteSavedSearch(id: Long) {
        saved.removeAll { it.id == id }
    }

    override suspend fun pollSavedSearches(since: Long?, includeAdult: Boolean) = SavedSearchPoll()

    override suspend fun trustSettings(): TrustSettings {
        trustError?.let { throw it }
        return trust
    }

    override suspend fun putTrustSettings(settings: TrustSettings): TrustSettings {
        trustError?.let { throw it }
        trustPuts += settings
        trust = settings
        return settings
    }

    override suspend fun statsHealth() = statsHealthAnswer()

    override suspend fun breakdown(request: BreakdownRequest): BreakdownSet {
        breakdownRequests += request
        kotlinx.coroutines.delay(breakdownDelayMs)
        breakdownError?.let { throw it }
        return breakdownAnswer(request)
    }

    override suspend fun timeseries(metrics: List<String>, bucket: String, since: Long?): TimeseriesSet {
        timeseriesCalls += metrics to bucket
        return timeseriesAnswer(metrics, bucket)
    }

    override suspend fun graph(path: String): Graph {
        graphCalls += path
        return graphAnswer(path)
    }

    override suspend fun crawlSources(): CrawlSources {
        sourcesCalls++
        sourcesError?.let { throw it }
        return sourcesAnswer()
    }

    override suspend fun crawlMap(layer: String, window: String): CrawlMap {
        mapCalls += layer to window
        kotlinx.coroutines.delay(mapDelayMs)
        mapError?.let { throw it }
        return mapAnswer(layer, window)
    }

    companion object {
        fun hit(title: String, ih: String = "%040x".format(title.hashCode().toLong() and 0xffffffffL)) =
            TorrentSummary(infohash = ih, title = title)
    }
}

fun tempDir(): File = Files.createTempDirectory("magnetar-test").toFile().also { it.deleteOnExit() }

/** A repository on [fake] with [settings], its cache in a temp dir. */
fun TestScope.repo(
    fake: FakeService,
    settings: MutableStateFlow<AppSettings> = MutableStateFlow(AppSettings(serverUrl = "http://server.test", apiToken = "t")),
    cache: PageCache? = PageCache(tempDir()),
    scope: CoroutineScope = backgroundScope,
) = Repository(settings, { fake }, cache, scope, io = StandardTestDispatcher(testScheduler))

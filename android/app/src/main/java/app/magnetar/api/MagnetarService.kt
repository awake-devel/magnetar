package app.magnetar.api

/**
 * What the screens ask the server (docs/APP.md §4). [MagnetarApi] is the real one; the view
 * model tests use a fake with controllable delays, so debounce and cancellation are tested
 * on virtual time.
 */
interface MagnetarService {
    suspend fun health(): Health
    suspend fun search(query: SearchQuery): SearchPage
    suspend fun facets(request: FacetsRequest): FacetSet
    suspend fun detail(infohash: String): TorrentDetail
    suspend fun checkLiveness(infohash: String): Liveness
    suspend fun livenessHistory(infohash: String, days: Int): LivenessHistory
    suspend fun mediaReleases(mediaId: Long, query: SearchQuery): MediaReleases
    suspend fun savedSearches(): List<SavedSearch>
    suspend fun createSavedSearch(search: SavedSearch): SavedSearch
    suspend fun updateSavedSearch(search: SavedSearch): SavedSearch
    suspend fun deleteSavedSearch(id: Long)
    suspend fun pollSavedSearches(since: Long?, includeAdult: Boolean): SavedSearchPoll
    suspend fun trustSettings(): TrustSettings
    suspend fun putTrustSettings(settings: TrustSettings): TrustSettings
    suspend fun statsHealth(): StatsHealth
    suspend fun breakdown(request: BreakdownRequest): BreakdownSet
    suspend fun timeseries(metrics: List<String>, bucket: String, since: Long? = null): TimeseriesSet

    /** [path]: `wot`, `relays`, `media/{id}` or `swarm/{infohash}`. */
    suspend fun graph(path: String): Graph
}

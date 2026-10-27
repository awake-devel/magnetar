package app.magnetar.api

import java.io.InterruptedIOException
import java.io.IOException
import java.util.concurrent.TimeUnit
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import kotlinx.serialization.KSerializer
import kotlinx.serialization.SerializationException
import kotlinx.serialization.builtins.ListSerializer
import kotlinx.serialization.json.Json
import okhttp3.HttpUrl
import okhttp3.HttpUrl.Companion.toHttpUrlOrNull
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.RequestBody
import okhttp3.RequestBody.Companion.toRequestBody

/**
 * The server's REST API (docs/API.md), one suspend function per endpoint (docs/APP.md §4).
 *
 * Every call has the call timeout of its [RequestClass], is cancelled with its coroutine, and
 * fails only with an [ApiException]. The token goes only to [baseUrl]'s origin.
 */
class MagnetarApi(
    val baseUrl: HttpUrl,
    token: String,
    base: OkHttpClient = Http.baseClient(),
    /** Call timeout per request class; tests shorten it. */
    callTimeoutMs: (RequestClass) -> Long = RequestClass::callTimeoutMs,
) : MagnetarService {
    /** Use this client for anything else that talks to the server (posters via Coil). */
    val httpClient: OkHttpClient = base.newBuilder()
        .addInterceptor(AuthInterceptor(baseUrl, token.trim()))
        .build()

    /**
     * The read timeout follows the call timeout: the base client's 15 s read timeout would
     * otherwise cut a 45 s seed check or a 60 s poll while the server is still working on the
     * answer (it sends no byte before it is done).
     */
    private val clients: Map<RequestClass, OkHttpClient> = RequestClass.entries.associateWith {
        val ms = callTimeoutMs(it)
        httpClient.newBuilder()
            .callTimeout(ms, TimeUnit.MILLISECONDS)
            .readTimeout(ms, TimeUnit.MILLISECONDS)
            .build()
    }

    /** `GET /health` (no auth needed): version and features. */
    override suspend fun health(): Health = get("health", RequestClass.DETAIL, Health.serializer())

    /**
     * Settings → "Test connection": the server answers, and the token is accepted (an
     * authenticated read that changes nothing).
     */
    suspend fun testConnection(): Health {
        val health = health()
        trustSettings()
        return health
    }

    override suspend fun search(query: SearchQuery): SearchPage =
        post("api/v1/search", RequestClass.SEARCH, SearchQuery.serializer(), query, SearchPage.serializer())

    /** Disjunctive counts for the filter sheet (feature `facet_set`); 503 while the index builds. */
    override suspend fun facets(request: FacetsRequest): FacetSet =
        post("api/v1/facets", RequestClass.SEARCH, FacetsRequest.serializer(), request, FacetSet.serializer())

    override suspend fun detail(infohash: String): TorrentDetail =
        get("api/v1/torrents/${segment(infohash)}", RequestClass.DETAIL, TorrentDetail.serializer())

    /** "Check now": the server asks the trackers and the DHT (up to 45 s). */
    override suspend fun checkLiveness(infohash: String): Liveness = send(
        "POST", "api/v1/torrents/${segment(infohash)}/liveness", RequestClass.CHECK,
        ByteArray(0).toRequestBody(null), Liveness.serializer(),
    )

    /** Feature `liveness_history`; at most 90 days. */
    override suspend fun livenessHistory(infohash: String, days: Int): LivenessHistory {
        val url = url("api/v1/torrents/${segment(infohash)}/liveness-history").newBuilder()
            .addQueryParameter("days", days.coerceIn(1, 90).toString()).build()
        return decode(LivenessHistory.serializer(), execute(Request.Builder().url(url).get().build(), RequestClass.DETAIL))
    }

    /** Feature `media_releases`: the title page, [query]'s filters applied. */
    override suspend fun mediaReleases(mediaId: Long, query: SearchQuery): MediaReleases = post(
        "api/v1/media/$mediaId/releases", RequestClass.SEARCH,
        SearchQuery.serializer(), query, MediaReleases.serializer(),
    )

    /**
     * Absolute URL of a server-relative poster path (`MediaInfo.poster`), on the configured
     * server only (so the bearer header goes nowhere else); null for anything else.
     */
    fun posterUrl(path: String): HttpUrl? {
        if (!path.startsWith("/") || path.startsWith("//")) return null
        val rel = path.substringBefore('?').substringBefore('#').trimStart('/')
        if (rel.split('/').any { it == ".." || it == "." }) return null
        return baseUrl.newBuilder().addPathSegments(rel).build()
    }

    override suspend fun trustSettings(): TrustSettings =
        get("api/v1/settings/trust", RequestClass.DETAIL, TrustSettings.serializer())

    /**
     * `PUT /api/v1/settings/trust`. Sends both fields: a null [TrustSettings.root] clears the
     * root, so pass the current one when only the threshold changes.
     */
    override suspend fun putTrustSettings(settings: TrustSettings): TrustSettings = send(
        "PUT", "api/v1/settings/trust", RequestClass.DETAIL,
        encode(TrustSettings.serializer(), settings), TrustSettings.serializer(),
    )

    override suspend fun savedSearches(): List<SavedSearch> =
        get("api/v1/saved-searches", RequestClass.DETAIL, ListSerializer(SavedSearch.serializer()))

    /** Creates (id 0) a saved search; returns it with its server id. */
    override suspend fun createSavedSearch(search: SavedSearch): SavedSearch = post(
        "api/v1/saved-searches", RequestClass.DETAIL,
        SavedSearch.serializer(), search.copy(id = 0), SavedSearch.serializer(),
    )

    /** Needs `saved_search_poll`; keeps the server notifier's watermarks unless the query changes. */
    override suspend fun updateSavedSearch(search: SavedSearch): SavedSearch = send(
        "PUT", "api/v1/saved-searches/${search.id}", RequestClass.DETAIL,
        encode(SavedSearch.serializer(), search), SavedSearch.serializer(),
    )

    override suspend fun deleteSavedSearch(id: Long) {
        execute(request("api/v1/saved-searches/$id").delete().build(), RequestClass.DETAIL)
    }

    /**
     * `GET /api/v1/saved-searches/poll` (D77): new hits since [since] (null = first poll: no
     * hits, the server clock as watermark). [includeAdult] = the Settings switch.
     */
    override suspend fun pollSavedSearches(since: Long?, includeAdult: Boolean): SavedSearchPoll {
        val url = url("api/v1/saved-searches/poll").newBuilder().apply {
            if (since != null) addQueryParameter("since", since.toString())
            if (includeAdult) addQueryParameter("adult", "1")
        }.build()
        val text = execute(Request.Builder().url(url).get().build(), RequestClass.POLL)
        return decode(SavedSearchPoll.serializer(), text)
    }

    /** Feature `stats` (D47): seed checks, index, relays, crawler, DHT crawler. */
    override suspend fun statsHealth(): StatsHealth =
        get("api/v1/stats/health", RequestClass.STATS, StatsHealth.serializer())

    /** Feature `stats`; 503 `search_index_unavailable` while the index builds. */
    override suspend fun breakdown(request: BreakdownRequest): BreakdownSet = post(
        "api/v1/stats/breakdown", RequestClass.STATS,
        BreakdownRequest.serializer(), request.copy(top = request.top.coerceIn(1, 50)), BreakdownSet.serializer(),
    )

    /** Feature `stats`: [metrics] per `hour` or `day` (UTC buckets) since [since] (server default when null). */
    override suspend fun timeseries(metrics: List<String>, bucket: String, since: Long?): TimeseriesSet {
        val url = url("api/v1/stats/timeseries").newBuilder().apply {
            addQueryParameter("metric", metrics.joinToString(","))
            addQueryParameter("bucket", if (bucket == "hour") "hour" else "day")
            if (since != null) addQueryParameter("since", since.toString())
        }.build()
        return decode(TimeseriesSet.serializer(), execute(Request.Builder().url(url).get().build(), RequestClass.STATS))
    }

    /**
     * Feature `graphs`: `wot`, `relays`, `media/{id}`, `swarm/{infohash}`; anything else is
     * refused before a request is made.
     */
    override suspend fun graph(path: String): Graph {
        val p = when {
            path == "wot" || path == "relays" -> path
            path.startsWith("media/") && path.removePrefix("media/").toLongOrNull()?.let { it > 0 } == true ->
                "media/" + path.removePrefix("media/").toLong()
            path.startsWith("swarm/") -> "swarm/" + segment(path.removePrefix("swarm/"))
            else -> throw ApiException.NotFound("No such graph")
        }
        return get("api/v1/graphs/$p", RequestClass.STATS, Graph.serializer())
    }

    /**
     * Feature `crawl_sources`: DHT routes, relays by discovery source, top relays, machines. A
     * relay URL whose host (or a machine name that) is an IP literal is redacted before anything
     * shows or caches it.
     */
    override suspend fun crawlSources(): CrawlSources {
        val s = get("api/v1/stats/sources", RequestClass.STATS, CrawlSources.serializer())
        return s.copy(
            nostr = s.nostr?.let { n -> n.copy(topRelays = n.topRelays.map { it.copy(url = redactIpHost(it.url)) }) },
            machines = s.machines.map { it.copy(name = redactIpHost(it.name)) },
        )
    }

    /** Feature `crawl_map`; any other [layer] or [window] is refused before a request is made. */
    override suspend fun crawlMap(layer: String, window: String): CrawlMap {
        if (layer !in MAP_LAYERS || window !in MAP_WINDOWS) throw ApiException.NotFound("No such map")
        val url = url("api/v1/stats/map").newBuilder()
            .addQueryParameter("layer", layer)
            .addQueryParameter("window", window)
            .build()
        return decode(CrawlMap.serializer(), execute(Request.Builder().url(url).get().build(), RequestClass.STATS))
    }

    // --- plumbing ---

    /** An infohash as a path segment: 40 hex digits (a deep link's value is checked again here). */
    private fun segment(infohash: String): String {
        val h = infohash.trim().lowercase()
        if (!INFOHASH.matches(h)) throw ApiException.NotFound("Not a torrent infohash")
        return h
    }

    internal fun url(path: String): HttpUrl = baseUrl.newBuilder().addPathSegments(path).build()

    private fun request(path: String) = Request.Builder().url(url(path))

    private suspend fun <T> get(path: String, cls: RequestClass, out: KSerializer<T>): T =
        decode(out, execute(request(path).get().build(), cls))

    private suspend fun <I, T> post(
        path: String, cls: RequestClass, inSer: KSerializer<I>, body: I, out: KSerializer<T>,
    ): T = send("POST", path, cls, encode(inSer, body), out)

    private suspend fun <T> send(
        method: String, path: String, cls: RequestClass, body: RequestBody, out: KSerializer<T>,
    ): T = decode(out, execute(request(path).method(method, body).build(), cls))

    private fun <I> encode(ser: KSerializer<I>, value: I): RequestBody =
        JSON.encodeToString(ser, value).toRequestBody(JSON_MEDIA)

    private suspend fun <T> decode(ser: KSerializer<T>, text: String): T =
        withContext(Dispatchers.Default) {
            try {
                JSON.decodeFromString(ser, text)
            } catch (e: SerializationException) {
                throw ApiException.Decode(e)
            } catch (e: IllegalArgumentException) {
                throw ApiException.Decode(e)
            }
        }

    /** Runs [request]; returns the body of a 2xx, else throws the matching [ApiException]. */
    private suspend fun execute(request: Request, cls: RequestClass): String {
        val result = try {
            clients.getValue(cls).newCall(request).awaitText()
        } catch (e: CancellationException) {
            throw e
        } catch (e: ApiException) {
            // Raised by our own interceptor (a token that cannot go into a header).
            throw e
        } catch (e: InterruptedIOException) {
            // OkHttp's call timeout and SocketTimeoutException both land here.
            throw ApiException.Timeout(e)
        } catch (e: IOException) {
            throw ApiException.Network(e)
        }
        if (result.status in 200..299) return result.body
        throw errorOf(result)
    }

    companion object {
        val JSON = Json {
            ignoreUnknownKeys = true
            coerceInputValues = true
            explicitNulls = false
            encodeDefaults = true
        }
        private val JSON_MEDIA = "application/json".toMediaType()
        private val INFOHASH = Regex("^[0-9a-f]{40}$")
        val MAP_LAYERS = setOf("dht", "nostr")
        val MAP_WINDOWS = setOf("day", "all")
        private val IPV4 = Regex("""^\d{1,3}(\.\d{1,3}){3}$""")

        /**
         * [url] with an IP-literal host replaced (`wss://203.0.113.5:7447/x` → `wss://(IP address)`):
         * the app never shows or stores an IP address. Host names are kept.
         */
        fun redactIpHost(url: String): String {
            val scheme = url.substringBefore("://", "")
            val rest = if (scheme.isEmpty()) url else url.substringAfter("://")
            val authority = rest.substringBefore('/').substringBefore('?').substringBefore('#').substringAfterLast('@')
            val host = if (authority.startsWith("[")) authority.substringBefore(']') + "]" else authority.substringBefore(':')
            val ip = host.startsWith("[") || IPV4.matches(host) || authority.count { it == ':' } > 1
            if (!ip) return url
            return (if (scheme.isEmpty()) "" else "$scheme://") + "(IP address)"
        }

        /**
         * The client for the server in Settings, or [ApiException.NotConfigured] when [serverUrl]
         * is empty or not an http(s) URL. A path prefix (`https://host/magnetar`) is kept.
         */
        fun create(
            serverUrl: String,
            token: String,
            base: OkHttpClient = Http.baseClient(),
            callTimeoutMs: (RequestClass) -> Long = RequestClass::callTimeoutMs,
        ): MagnetarApi = MagnetarApi(
            parseBaseUrl(serverUrl) ?: throw ApiException.NotConfigured(), token, base, callTimeoutMs,
        )

        /** `https://host/x` → `https://host/x/`; null for blank or non-http(s) input. */
        fun parseBaseUrl(serverUrl: String): HttpUrl? {
            val trimmed = serverUrl.trim()
            if (trimmed.isEmpty()) return null
            val url = trimmed.toHttpUrlOrNull() ?: return null
            if (url.scheme != "http" && url.scheme != "https") return null
            val segments = url.encodedPathSegments.filter { it.isNotEmpty() }
            val path = segments.joinToString("/", prefix = "/", postfix = if (segments.isEmpty()) "" else "/")
            return url.newBuilder().encodedPath(path).query(null).fragment(null).build()
        }

        internal fun errorOf(result: HttpResult): ApiException {
            val body = runCatching { JSON.decodeFromString(ApiErrorBody.serializer(), result.body) }
                .getOrNull()
            val code = body?.error.orEmpty()
            val message = body?.message?.takeIf { it.isNotBlank() } ?: "HTTP ${result.status}"
            return when {
                result.status == 401 -> ApiException.Unauthorized(message)
                result.status == 404 -> ApiException.NotFound(message)
                result.status == 503 && code == "search_index_unavailable" ->
                    ApiException.IndexBuilding(message)
                else -> ApiException.Http(result.status, code, message)
            }
        }
    }
}

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
) {
    /** Use this client for anything else that talks to the server (posters via Coil). */
    val httpClient: OkHttpClient = base.newBuilder()
        .addInterceptor(AuthInterceptor(baseUrl, token.trim()))
        .build()

    private val clients: Map<RequestClass, OkHttpClient> = RequestClass.entries.associateWith {
        httpClient.newBuilder().callTimeout(callTimeoutMs(it), TimeUnit.MILLISECONDS).build()
    }

    /** `GET /health` (no auth needed): version and features. */
    suspend fun health(): Health = get("health", RequestClass.DETAIL, Health.serializer())

    /**
     * Settings → "Test connection": the server answers, and the token is accepted (an
     * authenticated read that changes nothing).
     */
    suspend fun testConnection(): Health {
        val health = health()
        trustSettings()
        return health
    }

    suspend fun search(query: SearchQuery): SearchPage =
        post("api/v1/search", RequestClass.SEARCH, SearchQuery.serializer(), query, SearchPage.serializer())

    suspend fun trustSettings(): TrustSettings =
        get("api/v1/settings/trust", RequestClass.DETAIL, TrustSettings.serializer())

    /**
     * `PUT /api/v1/settings/trust`. Sends both fields: a null [TrustSettings.root] clears the
     * root, so pass the current one when only the threshold changes.
     */
    suspend fun putTrustSettings(settings: TrustSettings): TrustSettings = send(
        "PUT", "api/v1/settings/trust", RequestClass.DETAIL,
        encode(TrustSettings.serializer(), settings), TrustSettings.serializer(),
    )

    suspend fun savedSearches(): List<SavedSearch> =
        get("api/v1/saved-searches", RequestClass.DETAIL, ListSerializer(SavedSearch.serializer()))

    /** Creates (id 0) a saved search; returns it with its server id. */
    suspend fun createSavedSearch(search: SavedSearch): SavedSearch = post(
        "api/v1/saved-searches", RequestClass.DETAIL,
        SavedSearch.serializer(), search.copy(id = 0), SavedSearch.serializer(),
    )

    /** Needs `saved_search_poll`; keeps the server notifier's watermarks unless the query changes. */
    suspend fun updateSavedSearch(search: SavedSearch): SavedSearch = send(
        "PUT", "api/v1/saved-searches/${search.id}", RequestClass.DETAIL,
        encode(SavedSearch.serializer(), search), SavedSearch.serializer(),
    )

    suspend fun deleteSavedSearch(id: Long) {
        execute(request("api/v1/saved-searches/$id").delete().build(), RequestClass.DETAIL)
    }

    /**
     * `GET /api/v1/saved-searches/poll` (D77): new hits since [since] (null = first poll: no
     * hits, the server clock as watermark). [includeAdult] = the Settings switch.
     */
    suspend fun pollSavedSearches(since: Long?, includeAdult: Boolean): SavedSearchPoll {
        val url = url("api/v1/saved-searches/poll").newBuilder().apply {
            if (since != null) addQueryParameter("since", since.toString())
            if (includeAdult) addQueryParameter("adult", "1")
        }.build()
        val text = execute(Request.Builder().url(url).get().build(), RequestClass.POLL)
        return decode(SavedSearchPoll.serializer(), text)
    }

    // --- plumbing ---

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

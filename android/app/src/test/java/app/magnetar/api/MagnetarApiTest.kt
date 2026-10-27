package app.magnetar.api

import java.util.concurrent.TimeUnit
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.async
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeout
import mockwebserver3.MockResponse
import mockwebserver3.MockWebServer
import okhttp3.Request
import org.testng.Assert.assertEquals
import org.testng.Assert.assertFalse
import org.testng.Assert.assertNull
import org.testng.Assert.assertTrue
import org.testng.Assert.fail
import org.testng.annotations.AfterMethod
import org.testng.annotations.BeforeMethod
import org.testng.annotations.Test

class MagnetarApiTest {
    private lateinit var server: MockWebServer

    @BeforeMethod
    fun start() {
        server = MockWebServer()
        server.start()
    }

    @AfterMethod
    fun stop() {
        server.close()
    }

    private fun api(token: String = "secret-token", prefix: String = "/", timeoutMs: Long? = null) =
        MagnetarApi.create(
            server.url(prefix).toString(), token,
            callTimeoutMs = { cls -> timeoutMs ?: cls.callTimeoutMs },
        )

    private fun json(body: String, code: Int = 200) =
        MockResponse.Builder().code(code).addHeader("Content-Type", "application/json").body(body).build()

    private inline fun <reified E : ApiException> expect(block: () -> Unit): E {
        try {
            block()
        } catch (e: ApiException) {
            if (e is E) return e
            fail("expected ${E::class.simpleName}, got ${e::class.simpleName}: ${e.message}")
        }
        fail("expected ${E::class.simpleName}, nothing thrown")
        error("unreachable")
    }

    @Test
    fun `search posts the query with the bearer token`() = runBlocking {
        server.enqueue(json("""{"hits":[],"total":0,"source":"server"}"""))
        api().search(SearchQuery(text = "dune", categories = listOf("video"), includeAdult = false))
        val req = server.takeRequest()
        assertEquals(req.method, "POST")
        assertEquals(req.url.encodedPath, "/api/v1/search")
        assertEquals(req.headers["Authorization"], "Bearer secret-token")
        val body = MagnetarApi.JSON.parseToJsonElement(req.body!!.utf8()).toString()
        assertTrue(body.contains("\"text\":\"dune\""), body)
        assertTrue(body.contains("\"categories\":[\"video\"]"), body)
        // The adult switch is always sent, so the server never guesses (D52).
        assertTrue(body.contains("\"include_adult\":false"), body)
        assertTrue(body.contains("\"limit\":50"), body)
        // Nulls are left out (min_trust absent = the server's threshold).
        assertFalse(body.contains("min_trust"), body)
    }

    @Test
    fun `a path prefix in the server URL is kept`() = runBlocking {
        server.enqueue(json("""{"status":"ok","version":"0.2.0","db":true}"""))
        api(prefix = "/magnetar").health()
        assertEquals(server.takeRequest().url.encodedPath, "/magnetar/health")
    }

    @Test
    fun `older and newer answers both parse`() = runBlocking {
        // An unknown field, an unknown category (a newer server) and no origin / best_peers /
        // attrs (an older server).
        server.enqueue(
            json(
                """{"hits":[
                  {"infohash":"aa","title":"A","category":"hologram","file_count":1,"created_at":5,
                   "trust":40,"seeders":null,"future_field":{"x":1}},
                  {"infohash":"bb","title":"B","category":"video","origin":"dht","best_peers":7,
                   "media_kind":"movie","attrs":{"resolution":"1080p","languages":["fr"]}}
                ],"total":2,"source":"server","total_is_lower_bound":true,"took_ms":3}""",
            ),
        )
        val page = api().search(SearchQuery())
        assertEquals(page.total, 2L)
        assertTrue(page.totalIsLowerBound)
        val (a, b) = page.hits
        assertEquals(a.category, Category.OTHER)
        assertEquals(a.origin, Origin.NOSTR)
        assertNull(a.bestPeers)
        assertNull(a.shownSeeders)
        assertEquals(b.origin, Origin.DHT)
        assertEquals(b.shownSeeders, 7)
        assertEquals(b.mediaKind, MediaKind.MOVIE)
        assertEquals(b.attrs?.resolution, "1080p")
        assertEquals(b.attrs?.languages, listOf("fr"))
    }

    @Test
    fun `health features`() = runBlocking {
        server.enqueue(json("""{"status":"ok","version":"0.3.0","db":true,"features":["stats","saved_search_poll"]}"""))
        val h = api().health()
        assertTrue(h.has(Features.SAVED_SEARCH_POLL))
        assertFalse(h.has(Features.GRAPHS))
        server.enqueue(json("""{"status":"ok","version":"0.1.0","db":true}"""))
        assertEquals(api().health().features, emptyList<String>())
    }

    @Test
    fun `error answers map to typed exceptions`() = runBlocking {
        server.enqueue(json("""{"error":"unauthorized","message":"bad token"}""", 401))
        assertEquals(expect<ApiException.Unauthorized> { runBlocking { api().search(SearchQuery()) } }.message, "bad token")

        server.enqueue(json("""{"error":"search_index_unavailable","message":"building"}""", 503))
        expect<ApiException.IndexBuilding> { runBlocking { api().search(SearchQuery()) } }

        server.enqueue(json("""{"error":"not_found","message":"no such saved search"}""", 404))
        expect<ApiException.NotFound> { runBlocking { api().deleteSavedSearch(9) } }

        server.enqueue(MockResponse.Builder().code(502).body("<html>bad gateway</html>").build())
        val e = expect<ApiException.Http> { runBlocking { api().search(SearchQuery()) } }
        assertEquals(e.status, 502)
        assertEquals(e.code, "")
        assertEquals(e.message, "HTTP 502")

        server.enqueue(json("""{"error":"invalid","message":"bad sort"}""", 400))
        val inv = expect<ApiException.Http> { runBlocking { api().search(SearchQuery()) } }
        assertEquals(inv.code, "invalid")

        server.enqueue(json("""not json"""))
        expect<ApiException.Decode> { runBlocking { api().search(SearchQuery()) } }
    }

    @Test
    fun `a slow server times out instead of hanging`() {
        server.enqueue(json("""{"hits":[],"total":0,"source":"server"}""").newBuilder().headersDelay(5, TimeUnit.SECONDS).build())
        val started = System.nanoTime()
        expect<ApiException.Timeout> { runBlocking { api(timeoutMs = 300).search(SearchQuery()) } }
        val ms = (System.nanoTime() - started) / 1_000_000
        assertTrue(ms < 3_000, "took $ms ms")
    }

    @Test
    fun `an unreachable server is a network error`() {
        val url = server.url("/").toString()
        server.close()
        expect<ApiException.Network> { runBlocking { MagnetarApi.create(url, "t").health() } }
    }

    @Test
    fun `cancelling a stale search cancels the call`() = runBlocking {
        server.enqueue(json("""{"hits":[],"total":0,"source":"server"}""").newBuilder().headersDelay(10, TimeUnit.SECONDS).build())
        val api = api()
        val received = CompletableDeferred<Unit>()
        val search = async(Dispatchers.Default) { api.search(SearchQuery(text = "old")) }
        withContext(Dispatchers.IO) { server.takeRequest(); received.complete(Unit) }
        received.await()
        val started = System.nanoTime()
        search.cancel()
        withTimeout(2_000) { search.join() }
        assertTrue(search.isCancelled)
        // The OkHttp call is gone too, so the connection is free for the next search.
        withTimeout(2_000) {
            while (api.httpClient.dispatcher.runningCallsCount() > 0) kotlinx.coroutines.delay(10)
        }
        assertTrue((System.nanoTime() - started) / 1_000_000 < 2_000)
    }

    @Test
    fun `the token never goes to another host`() = runBlocking {
        val other = MockWebServer()
        other.start()
        try {
            other.enqueue(MockResponse.Builder().code(200).body("img").build())
            val api = api()
            withContext(Dispatchers.IO) {
                api.httpClient.newCall(Request.Builder().url(other.url("/poster.jpg")).build()).execute().close()
            }
            assertNull(other.takeRequest().headers["Authorization"])
        } finally {
            other.close()
        }
        // Same host, other port: another origin.
        val a = MagnetarApi.parseBaseUrl("http://h:8080")!!
        assertFalse(AuthInterceptor.sameOrigin(a, MagnetarApi.parseBaseUrl("http://h:8081")!!))
        assertFalse(AuthInterceptor.sameOrigin(a, MagnetarApi.parseBaseUrl("https://h:8080")!!))
        assertTrue(AuthInterceptor.sameOrigin(a, MagnetarApi.parseBaseUrl("http://H:8080/x")!!))
    }

    @Test
    fun `no token, no header`() = runBlocking {
        server.enqueue(json("""{"status":"ok","version":"0.2.0","db":true}"""))
        api(token = "  ").health()
        assertNull(server.takeRequest().headers["Authorization"])
    }

    @Test
    fun `server URL parsing`() {
        assertNull(MagnetarApi.parseBaseUrl(""))
        assertNull(MagnetarApi.parseBaseUrl("   "))
        assertNull(MagnetarApi.parseBaseUrl("ftp://host"))
        assertNull(MagnetarApi.parseBaseUrl("not a url"))
        assertEquals(MagnetarApi.parseBaseUrl("https://h.ts.net/").toString(), "https://h.ts.net/")
        assertEquals(MagnetarApi.parseBaseUrl(" https://h.ts.net ").toString(), "https://h.ts.net/")
        assertEquals(MagnetarApi.parseBaseUrl("http://10.0.0.2:8080/m//").toString(), "http://10.0.0.2:8080/m/")
        assertEquals(MagnetarApi.parseBaseUrl("https://h/m?x=1#f").toString(), "https://h/m/")
        expect<ApiException.NotConfigured> { MagnetarApi.create("", "t") }
    }

    @Test
    fun `saved-search poll sends the watermark and the adult switch`() = runBlocking {
        server.enqueue(
            json(
                """{"watermark":1790000050,"results":[{"search":{"id":3,"name":"Dune","query":{"text":"dune"},
                "notify":true},"hits":[{"infohash":"cc","title":"Dune","category":"video"}],"truncated":false}]}""",
            ),
        )
        val poll = api().pollSavedSearches(since = 1790000000, includeAdult = true)
        val req = server.takeRequest()
        assertEquals(req.url.encodedPath, "/api/v1/saved-searches/poll")
        assertEquals(req.url.queryParameter("since"), "1790000000")
        assertEquals(req.url.queryParameter("adult"), "1")
        assertEquals(poll.watermark, 1790000050L)
        assertEquals(poll.results.single().search.query.text, "dune")
        assertEquals(poll.results.single().hits.single().infohash, "cc")

        server.enqueue(json("""{"watermark":1790000100,"results":[]}"""))
        api().pollSavedSearches(since = null, includeAdult = false)
        val first = server.takeRequest()
        assertNull(first.url.queryParameter("since"))
        assertNull(first.url.queryParameter("adult"))
    }

    @Test
    fun `saved searches create, update, delete`() = runBlocking {
        server.enqueue(json("""{"id":12,"name":"Dune","query":{"text":"dune"},"notify":true}"""))
        val created = api().createSavedSearch(SavedSearch(id = 99, name = "Dune", query = SearchQuery(text = "dune"), notify = true))
        assertEquals(created.id, 12L)
        val post = server.takeRequest()
        assertEquals(post.method, "POST")
        assertTrue(post.body!!.utf8().contains("\"id\":0"))

        server.enqueue(json("""{"id":12,"name":"Dune 2","query":{"text":"dune"},"notify":false}"""))
        api().updateSavedSearch(created.copy(name = "Dune 2", notify = false))
        val put = server.takeRequest()
        assertEquals(put.method, "PUT")
        assertEquals(put.url.encodedPath, "/api/v1/saved-searches/12")

        server.enqueue(MockResponse.Builder().code(204).build())
        api().deleteSavedSearch(12)
        val del = server.takeRequest()
        assertEquals(del.method, "DELETE")
        assertEquals(del.url.encodedPath, "/api/v1/saved-searches/12")
    }

    @Test
    fun `test connection checks the token`() = runBlocking {
        server.enqueue(json("""{"status":"ok","version":"0.2.0","db":true}"""))
        server.enqueue(json("""{"root":null,"threshold":20}"""))
        assertEquals(api().testConnection().version, "0.2.0")
        assertEquals(server.takeRequest().url.encodedPath, "/health")
        val trust = server.takeRequest()
        assertEquals(trust.url.encodedPath, "/api/v1/settings/trust")
        assertEquals(trust.headers["Authorization"], "Bearer secret-token")

        server.enqueue(json("""{"status":"ok","version":"0.2.0","db":true}"""))
        server.enqueue(json("""{"error":"unauthorized","message":"missing or invalid token"}""", 401))
        expect<ApiException.Unauthorized> { runBlocking { api(token = "wrong").testConnection() } }
    }

    @Test
    fun `trust settings send root and threshold`() = runBlocking {
        server.enqueue(json("""{"root":"3bf0","threshold":30}"""))
        val out = api().putTrustSettings(TrustSettings(root = "npub1abc", threshold = 30))
        assertEquals(out.root, "3bf0")
        val body = server.takeRequest().body!!.utf8()
        assertTrue(body.contains("\"root\":\"npub1abc\""), body)
        assertTrue(body.contains("\"threshold\":30"), body)
    }
}

package app.magnetar.api

import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicReference
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.async
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeout
import mockwebserver3.MockResponse
import mockwebserver3.MockWebServer
import okhttp3.OkHttpClient
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
    fun `a long request class is not cut by the base read timeout`() = runBlocking {
        // The server answers after 800 ms; the base read timeout is 200 ms, the class allows 3 s.
        server.enqueue(json("""{"watermark":5,"results":[]}""").newBuilder().headersDelay(800, TimeUnit.MILLISECONDS).build())
        val base = OkHttpClient.Builder().readTimeout(200, TimeUnit.MILLISECONDS).build()
        val api = MagnetarApi.create(server.url("/").toString(), "t", base, callTimeoutMs = { 3_000 })
        assertEquals(api.pollSavedSearches(since = 1, includeAdult = false).watermark, 5L)
    }

    @Test
    fun `a token that cannot be sent fails the call without crashing the app`() {
        val uncaught = AtomicReference<Throwable?>(null)
        val previous = Thread.getDefaultUncaughtExceptionHandler()
        Thread.setDefaultUncaughtExceptionHandler { _, e -> uncaught.set(e) }
        try {
            for (bad in listOf("abc\ndef", "abc\u00a0def", "jeton-é")) {
                val e = expect<ApiException.Unauthorized> { runBlocking { api(token = bad).search(SearchQuery()) } }
                assertEquals(e.message, AuthInterceptor.BAD_TOKEN)
            }
            Thread.sleep(200)
            assertNull(uncaught.get(), "uncaught on the dispatcher: ${uncaught.get()}")
            assertEquals(server.requestCount, 0)
        } finally {
            Thread.setDefaultUncaughtExceptionHandler(previous)
        }
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

    private val ih = "0123456789abcdef0123456789abcdef01234567"

    @Test
    fun `facets post the query, the fields and the size`() = runBlocking {
        server.enqueue(json("""{"total":901,"facets":[{"field":"origin","total":901,"counts":[{"value":"nostr","count":586},{"value":"dht","count":315}]}]}"""))
        val set = api().facets(FacetsRequest(SearchQuery(text = "dune"), listOf("category", "origin"), 40))
        assertEquals(set.total, 901)
        assertEquals(set.field("origin")!!.counts.map { it.value }, listOf("nostr", "dht"))
        val req = server.takeRequest()
        assertEquals(req.url.encodedPath, "/api/v1/facets")
        val body = req.body!!.utf8()
        assertTrue(body.contains("\"fields\":[\"category\",\"origin\"]"), body)
        assertTrue(body.contains("\"size\":40"), body)
        assertTrue(body.contains("\"include_adult\":false"), body)
    }

    @Test
    fun `detail decodes the server's tuples and unknown fields`() = runBlocking {
        server.enqueue(
            json(
                """{"summary":{"infohash":"$ih","title":"Dune","category":"video","origin":"both","best_peers":12,"future":1},
                "description":"","files":[{"path":"a/b.mkv","size":5},{"path":"c","size":null}],"trackers":[],"hashtags":[],
                "category_path":["Movies"],"external_ids":[["imdb","tt1160419"]],"magnet":"magnet:?xt=urn:btih:$ih",
                "liveness":{"infohash":"$ih","seeders":3,"leechers":1,"completed":null,"dht_peers_est":9,"source":"dht_values",
                "trackers_ok":1,"trackers_failed":0,"checked_at":5,"expires_at":9},
                "trust":{"pubkey":"","score":40,"factors":[{"code":"FOLLOWED","delta":40,"detail":"d","evidence":[]}],"root":null,"computed_at":1,"graph_version":3},
                "events":[],"comments":[],
                "media":{"media_id":7,"kind":"movie","title":"Dune","year":2021,"genres":["Science Fiction"],"rating":8.0,
                "tmdb":["movie",438631],"imdb":"tt1160419","method":"itag","confidence":0.9,"poster":"/api/v1/media/7/poster"},
                "dht":{"first_seen":1,"fetched_at":2,"title":"Dune","file_count":2,"total_size":5,"category":"video","peers_seen":4,
                "discovered_via":"sample","infohash_v2":null}}""",
            ),
        )
        val d = api().detail(ih.uppercase())
        assertEquals(server.takeRequest().url.encodedPath, "/api/v1/torrents/$ih")
        assertEquals(d.summary.origin, Origin.BOTH)
        assertEquals(d.summary.shownSeeders, 12)
        assertEquals(d.externalIds, listOf(listOf("imdb", "tt1160419")))
        assertEquals(d.liveness!!.bestPeers, 9)
        assertEquals(d.media!!.imdbUrl, "https://www.imdb.com/title/tt1160419/")
        assertEquals(d.files[1].size, null)
        assertEquals(d.dht!!.discoveredVia, "sample")
        assertEquals(d.trust.factors.single().code, "FOLLOWED")
    }

    @Test
    fun `a malformed infohash is refused before anything is sent`() = runBlocking {
        expect<ApiException.NotFound> { runBlocking { api().detail("../settings/trust") } }
        expect<ApiException.NotFound> { runBlocking { api().checkLiveness("abc") } }
        assertEquals(server.requestCount, 0)
    }

    @Test
    fun `check now, seed history and title page`() = runBlocking {
        server.enqueue(json("""{"infohash":"$ih","seeders":null,"leechers":null,"dht_peers_est":4,"source":"dht","trackers_ok":0,"trackers_failed":2,"checked_at":1,"expires_at":2}"""))
        assertEquals(api().checkLiveness(ih).bestPeers, 4)
        val check = server.takeRequest()
        assertEquals(check.method, "POST")
        assertEquals(check.url.encodedPath, "/api/v1/torrents/$ih/liveness")

        server.enqueue(json("""{"infohash":"$ih","points":[{"at":1,"seeders":2,"leechers":0,"peers_est":null}],"keep_days":90}"""))
        assertEquals(api().livenessHistory(ih, 365).points.single().seeders, 2)
        val hist = server.takeRequest()
        assertEquals(hist.url.encodedPath, "/api/v1/torrents/$ih/liveness-history")
        assertEquals(hist.url.queryParameter("days"), "90")

        server.enqueue(json("""{"media":{"media_id":7,"kind":"series","title":"S","method":"x","confidence":1},"total":1,"truncated":false,
            "groups":[{"season":1,"episode":null,"resolution":"1080p","language":"vostfr","language_label":"VOSTFR","releases":[{"infohash":"$ih","title":"S01"}]}]}"""))
        val r = api().mediaReleases(7, SearchQuery(minTrust = 0))
        assertEquals(r.groups.single().languageLabel, "VOSTFR")
        val rel = server.takeRequest()
        assertEquals(rel.method, "POST")
        assertEquals(rel.url.encodedPath, "/api/v1/media/7/releases")
        assertTrue(rel.body!!.utf8().contains("\"min_trust\":0"))
    }

    @Test
    fun `poster urls stay on the configured server`() {
        val a = api(prefix = "/magnetar")
        assertEquals(a.posterUrl("/api/v1/media/7/poster")!!.encodedPath, "/magnetar/api/v1/media/7/poster")
        assertEquals(a.posterUrl("/api/v1/media/7/poster")!!.host, server.hostName)
        assertNull(a.posterUrl("https://evil.example/x.jpg"))
        assertNull(a.posterUrl("//evil.example/x.jpg"))
        assertNull(a.posterUrl("/api/../../x"))
    }

    @Test
    fun `stats endpoints`() = runBlocking {
        server.enqueue(
            json(
                """{"torrents":958106,"liveness":{"enabled":true,"torrents":957462,"never_checked":367301,"due_now":431893,
                "checked_24h":575104,"checks_per_hour":30000,"alive":340984,"dead":233018,"unknown":16159,"trackers":9,
                "trackers_backed_off":1,"dht_nodes":4096,"last_batch":{"items":2000,"hashes":2000,"ms":178861,"new_field":1},
                "last_batch_at":1793068974},"search_index":{"state":"ready","docs":958088,"pending":96,"lag_secs":1},
                "relays":{"total":1476,"enabled":1476,"ok_24h":1019,"failing":470,"productive":88},
                "crawler":{"running":true,"last_run":1793069082,"last_report":{"relays_tried":40,"relays_ok":40,"errors":[]},"passes":8},
                "media_linked":413971,"attrs_missing":0,"computed_at":1793069116,
                "dht":{"mode":"aggressive","running":true,"ports":[6891,6892],"queries_per_sec":2779.27,"blocked_total":4982,
                "upnp":{"state":"off","mappings":[]}}}""",
            ),
        )
        val h = api().statsHealth()
        assertEquals(h.liveness?.neverChecked, 367301L)
        assertEquals(h.liveness?.lastBatch?.ms, 178861L)
        assertEquals(h.relays.ok24h, 1019L)
        assertEquals(h.dht?.blockedTotal, 4982L)
        assertEquals(h.dht?.ports, listOf(6891, 6892))
        assertEquals(server.takeRequest().url.encodedPath, "/api/v1/stats/health")

        server.enqueue(json("""{"total":5,"breakdowns":[{"field":"language","total":5,"counts":[{"value":"fr","count":3}],"other":1,"other_values":1,"missing":1}],"computed_at":1}"""))
        val b = api().breakdown(BreakdownRequest(SearchQuery(includeAdult = false), listOf("language"), top = 500))
        assertEquals(b.field("language")?.counts?.single()?.count, 3L)
        val br = server.takeRequest()
        assertEquals(br.method, "POST")
        assertEquals(br.url.encodedPath, "/api/v1/stats/breakdown")
        val body = br.body!!.utf8()
        assertTrue(body.contains("\"top\":50"), body)
        assertTrue(body.contains("\"include_adult\":false"), body)

        server.enqueue(json("""{"since":1,"until":2,"series":[{"metric":"db_size","kind":"gauge","bucket":"day","points":[{"at":1,"value":11510920895}]}]}"""))
        val t = api().timeseries(listOf("db_size", "dht_torrents_added"), "hour", since = 100)
        assertEquals(t.series.single().points.single().value, 11510920895L)
        val tr = server.takeRequest()
        assertEquals(tr.url.encodedPath, "/api/v1/stats/timeseries")
        assertEquals(tr.url.queryParameter("metric"), "db_size,dht_torrents_added")
        assertEquals(tr.url.queryParameter("bucket"), "hour")
        assertEquals(tr.url.queryParameter("since"), "100")
    }

    @Test
    fun `graph paths are checked before anything is sent`() = runBlocking {
        val ih = "ab".repeat(20)
        server.enqueue(json("""{"kind":"swarm","nodes":[{"id":"$ih","kind":"torrent","label":"x","weight":3,"score":null,"detail":[]}],"edges":[],"truncated":false,"note":null}"""))
        assertEquals(api().graph("swarm/${ih.uppercase()}").nodes.single().kind, "torrent")
        assertEquals(server.takeRequest().url.encodedPath, "/api/v1/graphs/swarm/$ih")
        server.enqueue(json("""{"kind":"media","nodes":[],"edges":[{"source":0,"target":1,"weight":1,"kind":"quality"}]}"""))
        assertEquals(api().graph("media/42").edges.single().target, 1)
        assertEquals(server.takeRequest().url.encodedPath, "/api/v1/graphs/media/42")
        for (bad in listOf("../health", "media/x", "media/-1", "swarm/zz", "wot/../relays", "")) {
            expect<ApiException.NotFound> { runBlocking { api().graph(bad) } }
        }
        assertEquals(server.requestCount, 2)
        server.enqueue(json("""{"error":"not_found","message":"unknown media"}""", 404))
        expect<ApiException.NotFound> { runBlocking { api().graph("media/7") } }
    }
}

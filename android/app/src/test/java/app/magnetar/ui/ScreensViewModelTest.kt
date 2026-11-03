package app.magnetar.ui

import app.magnetar.api.ApiException
import app.magnetar.api.MediaInfo
import app.magnetar.api.ProbeAudioTrack
import app.magnetar.api.ProbeCorrectionItem
import app.magnetar.api.ProbeFileInfo
import app.magnetar.api.ProbeInfo
import app.magnetar.api.ProbeTrackFix
import app.magnetar.api.ProbeVideo
import app.magnetar.api.MediaReleases
import app.magnetar.api.ReleaseGroup
import app.magnetar.api.SavedSearch
import app.magnetar.api.SearchQuery
import app.magnetar.api.Sort
import app.magnetar.api.TorrentDetail
import app.magnetar.api.TorrentSummary
import app.magnetar.ui.detail.DetailViewModel
import app.magnetar.ui.saved.SavedViewModel
import app.magnetar.ui.saved.describe
import app.magnetar.ui.search.SearchFilters
import app.magnetar.ui.title.TitleViewModel
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.first
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
import org.testng.annotations.AfterMethod
import org.testng.annotations.Test

@OptIn(ExperimentalCoroutinesApi::class)
class ScreensViewModelTest {
    private val ih = "ab".repeat(20)

    @AfterMethod
    fun reset() = Dispatchers.resetMain()

    private fun TestScope.main() = Dispatchers.setMain(StandardTestDispatcher(testScheduler))

    @Test
    fun `detail loads, checks now and reloads the history`() = runTest {
        main()
        val fake = FakeService()
        val vm = DetailViewModel(repo(fake), ih)
        advanceUntilIdle()
        assertEquals(vm.state.value.detail?.summary?.title, "T $ih")
        assertNotNull(vm.state.value.history)
        assertEquals(fake.historyCalls.size, 1)
        vm.checkNow()
        advanceUntilIdle()
        assertEquals(fake.checks, 1)
        assertEquals(vm.state.value.detail?.liveness?.seeders, 7)
        assertEquals(vm.state.value.detail?.summary?.bestPeers, 7)
        assertEquals(vm.state.value.detail?.summary?.dead, false)
        assertEquals(fake.historyCalls.size, 2)
        assertTrue(vm.magnet.startsWith("magnet:?xt=urn:btih:$ih"))
    }

    @Test
    fun `no history without liveness_history, and the media is asked once more when the poster is missing`() = runTest {
        main()
        val fake = FakeService()
        fake.features = listOf("filters")
        var calls = 0
        fake.detailAnswer = {
            calls++
            TorrentDetail(
                summary = TorrentSummary(infohash = ih, title = "x"),
                media = MediaInfo(mediaId = 3, title = "M", poster = if (calls > 1) "/api/v1/media/3/poster" else null),
            )
        }
        val vm = DetailViewModel(repo(fake), ih, mediaRetryMs = 10)
        advanceUntilIdle()
        assertNull(vm.state.value.history)
        assertEquals(calls, 2)
        assertEquals(vm.state.value.detail?.media?.poster, "/api/v1/media/3/poster")
    }

    @Test
    fun `detail shows the cached copy offline`() = runTest {
        main()
        val fake = FakeService()
        val r = repo(fake)
        DetailViewModel(r, ih)
        advanceUntilIdle()
        fake.detailAnswer = { throw app.magnetar.api.ApiException.Network(java.io.IOException("down")) }
        val vm = DetailViewModel(r, ih)
        advanceUntilIdle()
        assertEquals(vm.state.value.detail?.summary?.title, "T $ih")
        assertNotNull(vm.state.value.cachedAtMs)
        assertTrue(vm.state.value.offline)
    }

    @Test
    fun `title page sends the search filters without text, chips narrow in place`() = runTest {
        main()
        val fake = FakeService()
        fun rel(n: String) = TorrentSummary(infohash = n.repeat(40).take(40), title = n)
        fake.releases = MediaReleases(
            media = MediaInfo(mediaId = 9, title = "Show"),
            total = 3,
            groups = listOf(
                ReleaseGroup(season = 1, resolution = "1080p", language = "multi-vff", languageLabel = "MULTi (VFF)", releases = listOf(rel("a"))),
                ReleaseGroup(season = 1, episode = 2, resolution = "720p", language = "vostfr", languageLabel = "VOSTFR", releases = listOf(rel("b"))),
                ReleaseGroup(season = 2, resolution = "1080p", language = "vostfr", languageLabel = "VOSTFR", releases = listOf(rel("c"))),
            ),
        )
        val vm = TitleViewModel(repo(fake), 9, SearchFilters(minTrust = 0, resolutions = setOf("1080p", "720p")))
        advanceUntilIdle()
        val (id, q) = fake.releaseQueries.single()
        assertEquals(id, 9)
        assertEquals(q.text, "")
        assertEquals(q.minTrust, 0)
        assertEquals(q.resolutions, listOf("1080p", "720p"))
        assertEquals(q.sort, Sort.SEEDERS)
        val s = vm.state.value
        assertTrue(s.isSeries)
        assertEquals(s.seasons, listOf(1, 2))
        assertEquals(s.resolutions, listOf("1080p", "720p"))
        assertEquals(s.languages.map { it.first }, listOf("multi-vff", "vostfr"))
        vm.setSeason(1)
        vm.setLanguage("vostfr")
        assertEquals(vm.state.value.shown.single().releases.single().title, "b")
        vm.setSeason(1)
        assertEquals(vm.state.value.shown.size, 2)
        vm.clearFilters()
        advanceUntilIdle()
        assertTrue(fake.releaseQueries.last().second.resolutions.isEmpty())
        assertNull(fake.releaseQueries.last().second.minTrust)
    }

    @Test
    fun `saved list, notify toggle (only with saved_search_poll) and delete`() = runTest {
        main()
        val fake = FakeService()
        fake.saved += SavedSearch(id = 1, name = "b", query = SearchQuery(text = "ubuntu"))
        fake.saved += SavedSearch(id = 2, name = "A", query = SearchQuery(), notify = true)
        val r = repo(fake)
        val vm = SavedViewModel(r)
        advanceUntilIdle()
        assertEquals(vm.state.value.searches.map { it.name }, listOf("A", "b"))
        assertTrue(vm.state.value.canEdit)
        vm.setNotify(vm.state.value.searches[1], true)
        advanceUntilIdle()
        assertTrue(fake.saved.first { it.id == 1L }.notify)
        vm.delete(vm.state.value.searches[0])
        advanceUntilIdle()
        assertEquals(fake.saved.map { it.id }, listOf(1L))
        assertEquals(vm.state.value.searches.map { it.id }, listOf(1L))
        vm.run(fake.saved.single())
        assertEquals(r.pendingSearch.value?.text, "ubuntu")
    }

    @Test
    fun `an older server lists saved searches but cannot edit them`() = runTest {
        main()
        val fake = FakeService()
        fake.features = listOf("filters")
        fake.saved += SavedSearch(id = 1, name = "x")
        val vm = SavedViewModel(repo(fake))
        advanceUntilIdle()
        assertFalse(vm.state.value.canEdit)
        vm.setNotify(vm.state.value.searches.single(), true)
        advanceUntilIdle()
        assertTrue(fake.updates.isEmpty())
    }

    @Test
    fun `a saved query is described in one line`() {
        val q = SearchQuery(text = "dune", categories = listOf("video"), resolutions = listOf("2160p"), minRating = 7f, minYear = 2020, origins = listOf("dht"))
        assertEquals(describe(q), "\"dune\" · Video · DHT · 2160p (4K) · ★ ≥ 7.0 · ≥ 2020")
        assertEquals(describe(SearchQuery()), "any text")
        // D122 review: a search filtered only by what the probes read is not "any text" alone.
        assertEquals(
            describe(SearchQuery(probeHdr = listOf("any"), probeAudio = listOf("atmos"), probeLanguages = listOf("fre"))),
            "any text · files: French, Any HDR, Atmos",
        )
    }

    @Test
    fun `a saved query with a kind is described`() {
        assertEquals(
            describe(SearchQuery(probeSubtitles = listOf("fre"), probeSubKinds = listOf("sdh"))),
            "any text · files: French, SDH",
        )
        assertEquals(
            describe(SearchQuery(probeLanguages = listOf("fre"), probeAudioKinds = listOf("description"), probeSubFormats = listOf("image", "text"))),
            "any text · files: French, Audio description, Image (PGS, VobSub…)/Text (SRT, ASS…)",
        )
    }

    private fun probed(lang: String, fix: ProbeTrackFix? = null) = ProbeInfo(
        version = 2,
        files = listOf(
            ProbeFileInfo(
                path = "a.mkv",
                video = ProbeVideo(audio = listOf(ProbeAudioTrack(codec = "ac3", channels = 2, lang = lang, pos = 0, fix = fix))),
            ),
        ),
    )

    private fun detailWithProbe(fake: FakeService) {
        fake.detailAnswer = { TorrentDetail(summary = TorrentSummary(infohash = ih, title = "x"), probe = probed("eng")) }
    }

    @Test
    fun `saving a correction replaces the probe and the cache`() = runTest {
        main()
        val fake = FakeService()
        fake.features = fake.features + "probe_corrections"
        detailWithProbe(fake)
        val after = probed("fre", ProbeTrackFix(id = 5, fields = listOf("lang"), by = "android"))
        fake.correctionAnswer = { after }
        val r = repo(fake)
        val vm = DetailViewModel(r, ih)
        advanceUntilIdle()
        val item = ProbeCorrectionItem(file = 0, track = "audio", pos = 0, set = listOf("lang"), lang = "fre")
        var result: Boolean? = null
        vm.saveCorrections(listOf(item)) { result = it }
        assertTrue(vm.state.value.correcting)
        advanceUntilIdle()
        assertEquals(result, true)
        assertFalse(vm.state.value.correcting)
        val (sentTo, request) = fake.correctionPosts.single()
        assertEquals(sentTo, ih)
        assertEquals(request.device, "android")
        assertEquals(request.items, listOf(item))
        assertEquals(vm.state.value.detail?.probe, after)
        assertEquals(vm.state.value.detail?.summary?.title, "x")
        assertEquals(vm.messages.first(), "Correction saved")
        val cached = r.readCache(DetailViewModel.CACHE_DETAIL, ih, TorrentDetail.serializer())
        assertEquals(cached?.value?.probe, after)

        // Undo: DELETE the correction, the answer replaces the probe again.
        fake.correctionAnswer = { probed("eng") }
        vm.undoCorrection(5)
        advanceUntilIdle()
        assertEquals(fake.correctionDeletes.single(), ih to 5L)
        assertEquals(vm.state.value.detail?.probe, probed("eng"))
        assertEquals(vm.messages.first(), "Correction removed")
        assertEquals(r.readCache(DetailViewModel.CACHE_DETAIL, ih, TorrentDetail.serializer())?.value?.probe, probed("eng"))
    }

    @Test
    fun `a detail fetch from before a correction keeps the corrected probe`() = runTest {
        main()
        val fake = FakeService()
        fake.features = fake.features + "probe_corrections"
        detailWithProbe(fake)
        val after = probed("fre", ProbeTrackFix(id = 5, fields = listOf("lang"), by = "android"))
        fake.correctionAnswer = { after }
        val r = repo(fake)
        val vm = DetailViewModel(r, ih)
        advanceUntilIdle()
        // A refresh in flight (answering the probe as it was) while the correction is saved.
        fake.detailDelayMs = 1_000
        fake.detailAnswer = { TorrentDetail(summary = TorrentSummary(infohash = ih, title = "y"), probe = probed("eng")) }
        vm.refresh()
        vm.saveCorrections(listOf(ProbeCorrectionItem(file = 0, track = "audio", pos = 0, set = listOf("lang"), lang = "fre")))
        advanceUntilIdle()
        assertEquals(vm.state.value.detail?.summary?.title, "y")
        assertEquals(vm.state.value.detail?.probe, after)
        assertEquals(r.readCache(DetailViewModel.CACHE_DETAIL, ih, TorrentDetail.serializer())?.value?.probe, after)
        // A fetch started after the correction takes the server's probe as it is.
        vm.refresh()
        advanceUntilIdle()
        assertEquals(vm.state.value.detail?.probe, probed("eng"))
    }

    @Test
    fun `a failed correction keeps the draft and says why`() = runTest {
        main()
        val fake = FakeService()
        fake.features = fake.features + "probe_corrections"
        detailWithProbe(fake)
        val vm = DetailViewModel(repo(fake), ih)
        advanceUntilIdle()
        val before = vm.state.value.detail
        val item = ProbeCorrectionItem(file = 0, track = "audio", pos = 0, set = listOf("lang"), lang = "xx")
        fake.correctionError = ApiException.Http(400, "invalid", "item 0: unknown language code \"xx\"")
        var result: Boolean? = null
        vm.saveCorrections(listOf(item)) { result = it }
        advanceUntilIdle()
        assertEquals(result, false)
        assertFalse(vm.state.value.correcting)
        assertEquals(vm.state.value.detail, before)
        assertEquals(vm.messages.first(), "Couldn't save the correction: item 0: unknown language code \"xx\"")
        // Offline: said so; nothing is queued.
        fake.correctionError = ApiException.Network(java.io.IOException("down"))
        result = null
        vm.saveCorrections(listOf(item)) { result = it }
        advanceUntilIdle()
        assertEquals(result, false)
        assertEquals(vm.messages.first(), "Offline: the correction was not saved")
        vm.undoCorrection(3) { result = it }
        advanceUntilIdle()
        assertEquals(result, false)
        assertEquals(vm.messages.first(), "Offline: the correction was not saved")
        assertEquals(fake.correctionPosts.size, 2)
        assertEquals(vm.state.value.detail, before)
    }

    @Test
    fun `corrections need probe_corrections`() = runTest {
        main()
        val fake = FakeService()
        detailWithProbe(fake)
        val r = repo(fake)
        val vm = DetailViewModel(r, ih)
        advanceUntilIdle()
        assertFalse(vm.state.value.canCorrect)

        val newer = FakeService()
        newer.features = newer.features + "probe_corrections"
        detailWithProbe(newer)
        val r2 = repo(newer)
        val vm2 = DetailViewModel(r2, ih)
        advanceUntilIdle()
        assertTrue(vm2.state.value.canCorrect)
        // A cached detail keeps the edit actions once the features are known.
        newer.detailAnswer = { throw ApiException.Network(java.io.IOException("down")) }
        val cachedVm = DetailViewModel(r2, ih)
        advanceUntilIdle()
        assertNotNull(cachedVm.state.value.cachedAtMs)
        assertTrue(cachedVm.state.value.canCorrect)
    }
}

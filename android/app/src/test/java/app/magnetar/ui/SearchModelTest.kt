package app.magnetar.ui

import app.magnetar.api.FacetCount
import app.magnetar.api.FacetCounts
import app.magnetar.api.MagnetarApi
import app.magnetar.api.SearchQuery
import app.magnetar.api.Sort
import app.magnetar.ui.common.facetLabel
import app.magnetar.ui.search.ChipOrder
import app.magnetar.ui.search.DatePreset
import app.magnetar.ui.search.FACET_TITLES
import app.magnetar.ui.search.PROBE_FACET_FIELDS
import app.magnetar.ui.search.PROBE_KIND_FIELDS
import app.magnetar.ui.search.SearchFilters
import app.magnetar.ui.search.buildQuery
import app.magnetar.ui.search.facetChips
import app.magnetar.ui.search.filtersOf
import org.testng.Assert.assertEquals
import org.testng.Assert.assertNull
import org.testng.Assert.assertTrue
import org.testng.annotations.Test

class SearchModelTest {
    private val now = 1_800_000_000L

    @Test
    fun `query and filters round-trip`() {
        val f = SearchFilters(
            categories = setOf("book", "video"), minSizeMb = 100, maxSizeMb = 4000, date = DatePreset.WEEK,
            minSeeders = 3, hideDead = true, minTrust = 0, languages = setOf("fr", "en"), subtitles = setOf("fr"),
            resolutions = setOf("2160p"), sources = setOf("web-dl"), codecs = setOf("h265"), hdr = setOf("dv"),
            audio = setOf("atmos"), genres = setOf("drama"), kinds = setOf("series"), minRating = 7.5f,
            minYear = 2000, maxYear = 2024, origins = setOf("dht"),
        )
        val q = buildQuery(" dune ", f, Sort.RATING, false, includeAdult = true, nowSec = now)
        assertEquals(q.text, "dune")
        // Categories in the sheet's order, lists sorted: the same filters give the same body.
        assertEquals(q.categories, listOf("video", "book"))
        assertEquals(q.languages, listOf("en", "fr"))
        assertEquals(q.minSize, 100L * 1024 * 1024)
        assertEquals(q.since, now - 7 * 86_400)
        assertEquals(q.minTrust, 0)
        assertTrue(q.includeAdult)
        assertEquals(filtersOf(q, now), f)
        assertEquals(f.activeCount, 18)
    }

    @Test
    fun `unsupported filters are dropped for an older server`() {
        val f = SearchFilters(resolutions = setOf("1080p"), minRating = 7f, origins = setOf("nostr"), categories = setOf("video"))
        val basic = f.supportedBy(emptySet())
        assertEquals(basic, SearchFilters(categories = setOf("video")))
        val withFilters = f.supportedBy(setOf("filters"))
        assertEquals(withFilters.resolutions, setOf("1080p"))
        assertTrue(withFilters.origins.isEmpty())
        assertEquals(f.supportedBy(setOf("filters", "origin")), f)
    }

    @Test
    fun `toggle and selected cover every chip field`() {
        var f = SearchFilters()
        for (field in listOf("category", "language", "subtitles", "resolution", "source", "codec", "hdr", "audio", "genre", "media_kind", "origin")) {
            f = f.toggle(field, "x")
            assertEquals(f.selected(field), setOf("x"), field)
            f = f.toggle(field, "x")
            assertTrue(f.selected(field).isEmpty(), field)
        }
    }

    @Test
    fun `chips without counts are the built-in list, a selected unknown value is kept`() {
        val chips = facetChips("resolution", setOf("1080p"), null)
        assertEquals(chips.first().value, "4320p")
        assertTrue(chips.all { it.count == null })
        val counted = facetChips("language", setOf("ko"), FacetCounts("language", 10, listOf(FacetCount("fr", 7), FacetCount("en", 3))))
        assertEquals(counted.map { it.value to it.count }, listOf("fr" to 7L, "en" to 3L, "ko" to 0L))
        assertEquals(counted[0].label, "French")
    }

    @Test
    fun `chips stay where they first appeared and a value that drops to zero stays in place`() {
        val order = ChipOrder()
        fun counts(vararg p: Pair<String, Long>) =
            facetChips("resolution", emptySet(), FacetCounts("resolution", 0, p.map { FacetCount(it.first, it.second) }))
        val first = order.arrange("resolution", counts("1080p" to 50, "2160p" to 20, "720p" to 5))
        assertEquals(first.map { it.value }, listOf("1080p", "2160p", "720p"))
        // After picking a language, 2160p is now the most frequent and 720p is gone.
        val second = order.arrange("resolution", counts("2160p" to 30, "1080p" to 10, "sd" to 1))
        assertEquals(second.map { it.value }, listOf("1080p", "2160p", "720p", "sd"))
        assertEquals(second.first { it.value == "720p" }.count, 0L)
        assertEquals(second.first { it.value == "2160p" }.count, 30L)
        // Without counts nothing is reordered.
        assertNull(order.arrange("resolution", facetChips("resolution", emptySet(), null)).first().count)
    }

    @Test
    fun `probe filters round-trip and are dropped for a server without probe_filters`() {
        val f = SearchFilters(
            probeLanguages = setOf("jpn", "fre"), probeSubtitles = setOf("eng"), probeResolutions = setOf("2160p"),
            probeCodecs = setOf("hevc"), probeHdr = setOf("any"), probeAudio = setOf("atmos"), categories = setOf("video"),
        )
        val q = buildQuery("", f, Sort.DATE, true, includeAdult = false, nowSec = now)
        assertEquals(q.probeLanguages, listOf("fre", "jpn"))
        assertEquals(q.probeSubtitles, listOf("eng"))
        assertEquals(q.probeResolutions, listOf("2160p"))
        assertEquals(q.probeVideoCodecs, listOf("hevc"))
        assertEquals(q.probeHdr, listOf("any"))
        assertEquals(q.probeAudio, listOf("atmos"))
        assertEquals(filtersOf(q, now), f)
        assertEquals(f.activeCount, 7)
        assertTrue(f.hasProbeFilters)
        // A D44-era server (filters, origin) would ignore them and answer unfiltered.
        val old = f.supportedBy(setOf("filters", "origin"))
        assertEquals(old, SearchFilters(categories = setOf("video")))
        assertTrue(!old.hasProbeFilters)
        assertEquals(f.supportedBy(setOf("probe_filters")), f)
        // The release (name) filters are independent of them.
        val both = SearchFilters(resolutions = setOf("2160p"), probeResolutions = setOf("2160p"))
        assertEquals(both.supportedBy(setOf("filters")), SearchFilters(resolutions = setOf("2160p")))
    }

    @Test
    fun `probe chips toggle, list built-in values and read as names`() {
        var f = SearchFilters()
        for (field in PROBE_FACET_FIELDS) {
            f = f.toggle(field, "x")
            assertEquals(f.selected(field), setOf("x"), field)
            f = f.toggle(field, "x")
            assertTrue(f.selected(field).isEmpty(), field)
            assertTrue(facetChips(field, emptySet(), null).isNotEmpty(), field)
        }
        assertEquals(facetLabel("probe_language", "fre"), "French")
        assertEquals(facetLabel("probe_subtitles", "ice"), "Icelandic")
        assertEquals(facetLabel("probe_language", "xyz"), "XYZ")
        assertEquals(facetLabel("probe_hdr", "any"), "Any HDR")
        assertEquals(facetLabel("probe_resolution", "2160p"), "2160p (4K and up)")
        assertEquals(facetLabel("probe_audio", "atmos"), "Atmos")
        // The name-parsed facet keeps its own labels.
        assertEquals(facetLabel("language", "fr"), "French")
        val counted = facetChips("probe_hdr", setOf("hlg"), FacetCounts("probe_hdr", 5, listOf(FacetCount("any", 5), FacetCount("hdr10", 4))))
        assertEquals(counted.map { it.value to it.count }, listOf("any" to 5L, "hdr10" to 4L, "hlg" to 0L))
    }

    @Test
    fun `probe kind filters round-trip and are dropped without probe_kinds`() {
        val f = SearchFilters(
            probeSubtitles = setOf("fre"), probeSubKinds = setOf("sdh", "forced"), probeAudioKinds = setOf("description"),
            probeSubFormats = setOf("image"), probeLanguages = setOf("fre"),
        )
        val q = buildQuery("", f, Sort.DATE, true, includeAdult = false, nowSec = now)
        assertEquals(q.probeSubKinds, listOf("forced", "sdh"))
        assertEquals(q.probeAudioKinds, listOf("description"))
        assertEquals(q.probeSubFormats, listOf("image"))
        assertEquals(filtersOf(q, now), f)
        assertEquals(f.activeCount, 5)
        assertTrue(f.hasProbeFilters)
        assertTrue(SearchFilters(probeSubFormats = setOf("text")).hasProbeFilters)
        // The wire names of the shared contract (D125).
        val json = MagnetarApi.JSON.encodeToString(SearchQuery.serializer(), q)
        assertTrue("\"probe_sub_kinds\":[\"forced\",\"sdh\"]" in json, json)
        assertTrue("\"probe_audio_kinds\":[\"description\"]" in json, json)
        assertTrue("\"probe_sub_formats\":[\"image\"]" in json, json)
        // A D122 server (probe_filters, no probe_kinds) would ignore the kinds and answer
        // unfiltered: they are not sent, the language filters still are.
        val d122 = f.supportedBy(setOf("filters", "probe_filters"))
        assertEquals(d122, SearchFilters(probeSubtitles = setOf("fre"), probeLanguages = setOf("fre")))
        // Without probe_filters nothing of the probes goes, probe_kinds or not.
        assertEquals(f.supportedBy(setOf("probe_kinds")), SearchFilters())
        assertEquals(f.supportedBy(setOf("probe_filters", "probe_kinds")), f)
    }

    @Test
    fun `kind chips toggle and read as names`() {
        assertEquals(
            PROBE_FACET_FIELDS,
            listOf(
                "probe_language", "probe_audio_kind", "probe_subtitles", "probe_sub_kind", "probe_sub_format",
                "probe_resolution", "probe_codec", "probe_hdr", "probe_audio",
            ),
        )
        assertTrue(PROBE_FACET_FIELDS.containsAll(PROBE_KIND_FIELDS))
        assertEquals(FACET_TITLES["probe_audio_kind"], "Audio track kind")
        assertEquals(FACET_TITLES["probe_sub_kind"], "Subtitle kind")
        assertEquals(FACET_TITLES["probe_sub_format"], "Subtitle format")
        var f = SearchFilters()
        for (field in PROBE_KIND_FIELDS) {
            f = f.toggle(field, "x")
            assertEquals(f.selected(field), setOf("x"), field)
            f = f.toggle(field, "x")
            assertTrue(f.selected(field).isEmpty(), field)
        }
        assertEquals(
            facetChips("probe_audio_kind", emptySet(), null).map { it.label },
            listOf(
                "Original", "Dubbed", "Voice-over", "Audio description", "Clear dialogue", "Commentary", "Music only",
                "Karaoke", "Alternate mix",
            ),
        )
        assertEquals(
            facetChips("probe_sub_kind", emptySet(), null).map { it.value to it.label },
            listOf(
                "sdh" to "SDH", "cc" to "Closed captions", "forced" to "Forced", "signs" to "Signs & songs",
                "commentary" to "Commentary", "description" to "Descriptions", "honorifics" to "Honorifics",
                "bilingual" to "Bilingual", "easy_reader" to "Easy reader",
            ),
        )
        assertEquals(
            facetChips("probe_sub_format", emptySet(), null).map { it.value to it.label },
            listOf("text" to "Text (SRT, ASS…)", "image" to "Image (PGS, VobSub…)", "cc" to "Closed captions (608/708)"),
        )
        // A kind a newer server counts and this app does not know reads as words.
        assertEquals(facetLabel("probe_sub_kind", "sign_language"), "Sign language")
        val counted = facetChips("probe_sub_kind", setOf("forced"), FacetCounts("probe_sub_kind", 3, listOf(FacetCount("sdh", 3))))
        assertEquals(counted.map { it.label to it.count }, listOf("SDH" to 3L, "Forced" to 0L))
    }
}

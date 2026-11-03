package app.magnetar.ui

import app.magnetar.api.FacetCount
import app.magnetar.api.FacetCounts
import app.magnetar.api.Sort
import app.magnetar.ui.common.facetLabel
import app.magnetar.ui.search.ChipOrder
import app.magnetar.ui.search.DatePreset
import app.magnetar.ui.search.PROBE_FACET_FIELDS
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
}

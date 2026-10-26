package app.magnetar

import app.magnetar.ffi.FfiCategory
import app.magnetar.ffi.FfiMediaKind
import app.magnetar.ffi.FfiSort
import app.magnetar.ui.search.FilterMode
import app.magnetar.ui.search.filterMode
import app.magnetar.ui.search.DatePreset
import app.magnetar.ui.search.SearchFilters
import app.magnetar.ui.search.SearchUiState
import app.magnetar.ui.search.toQuery
import app.magnetar.ui.search.toUiState
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class SearchQueryTest {
    private val now = 1_800_000_000L

    @Test
    fun filtersMapToQuery() {
        val s = SearchUiState(
            text = "  sintel ",
            filters = SearchFilters(
                categories = setOf(FfiCategory.VIDEO),
                minSizeMb = 100,
                maxSizeMb = 2048,
                date = DatePreset.WEEK,
                minSeeders = 3,
                hideDead = true,
                minTrust = 40,
            ),
            sort = FfiSort.SEEDERS,
            descending = false,
        )
        val q = s.toQuery(offset = 50u, nowSec = now)
        assertEquals("sintel", q.text)
        assertEquals(listOf(FfiCategory.VIDEO), q.categories)
        assertEquals(100uL * 1024u * 1024u, q.minSize)
        assertEquals(2048uL * 1024u * 1024u, q.maxSize)
        assertEquals(now - 7 * 86_400, q.since)
        assertNull(q.until)
        assertEquals(3u, q.minSeeders)
        assertEquals(true, q.hideDead)
        assertEquals(40.toUByte(), q.minTrust)
        assertEquals(FfiSort.SEEDERS, q.sort)
        assertEquals(false, q.descending)
        assertEquals(50u, q.offset)
        assertEquals(false, q.includeHidden)
    }

    @Test
    fun emptyFiltersAreUnset() {
        val q = SearchUiState().toQuery(nowSec = now)
        assertNull(q.minSize)
        assertNull(q.since)
        assertNull(q.minTrust)
        assertEquals(0, SearchFilters().activeCount)
    }

    @Test
    fun savedQueryRoundTrips() {
        val s = SearchUiState(
            text = "x",
            filters = SearchFilters(categories = setOf(FfiCategory.BOOK), minSizeMb = 5, date = DatePreset.MONTH),
            sort = FfiSort.DATE,
        )
        // A saved query read back a few hours later still snaps to the 30-day preset.
        val back = s.toQuery(nowSec = now).toUiState(nowSec = now + 5 * 3600)
        assertEquals(s.text, back.text)
        assertEquals(s.filters, back.filters)
        assertEquals(s.sort, back.sort)
    }

    @Test
    fun releaseFiltersRoundTrip() {
        val f = SearchFilters(
            languages = setOf("fr-ca", "original"),
            subtitles = setOf("fr"),
            resolutions = setOf("2160p", "1080p"),
            sources = setOf("remux"),
            codecs = setOf("h265"),
            hdr = setOf("dv"),
            audio = setOf("atmos"),
            genres = setOf("drama"),
            kinds = setOf(FfiMediaKind.SERIES),
            minRating = 7.5f,
            minYear = 2010,
            maxYear = 2020,
        )
        val q = SearchUiState(filters = f, sort = FfiSort.RATING).toQuery(nowSec = now)
        assertEquals(listOf("fr-ca", "original"), q.languages)
        assertEquals(listOf("1080p", "2160p"), q.resolutions)
        assertEquals(listOf("h265"), q.videoCodecs)
        assertEquals(listOf(FfiMediaKind.SERIES), q.mediaKinds)
        assertEquals(7.5f, q.minRating)
        assertEquals(FfiSort.RATING, q.sort)
        val back = q.toUiState(nowSec = now)
        assertEquals(f, back.filters)
        assertEquals(11, f.activeCount)
    }

    @Test
    fun toggleAndModes() {
        var f = SearchFilters()
        f = f.toggle("language", "fr").toggle("media_kind", "anime").toggle("resolution", "720p")
        assertEquals(setOf("fr"), f.languages)
        assertEquals(setOf(FfiMediaKind.ANIME), f.kinds)
        assertEquals(setOf("anime"), f.selected("media_kind"))
        assertEquals(SearchFilters(languages = setOf("fr"), resolutions = setOf("720p")), f.supportedBy(FilterMode.LOCAL))
        assertEquals(SearchFilters(), f.supportedBy(FilterMode.BASIC))
        assertEquals(f, f.toggle("resolution", "720p").toggle("resolution", "720p"))
        assertEquals(FilterMode.LOCAL, filterMode(hasServer = false, features = listOf("filters")))
        assertEquals(FilterMode.LOCAL, filterMode(hasServer = true, features = null))
        assertEquals(FilterMode.BASIC, filterMode(hasServer = true, features = emptyList()))
        assertEquals(FilterMode.FULL, filterMode(hasServer = true, features = listOf("filters", "facet_set")))
    }
}

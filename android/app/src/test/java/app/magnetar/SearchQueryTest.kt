package app.magnetar

import app.magnetar.ffi.FfiCategory
import app.magnetar.ffi.FfiSort
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
}

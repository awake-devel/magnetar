package app.magnetar

import app.magnetar.ffi.FfiCategory
import app.magnetar.ui.charts.bucketLabel
import app.magnetar.ui.search.SearchFilters
import app.magnetar.ui.stats.selectedStat
import app.magnetar.ui.stats.toggleStat
import java.util.Locale
import org.junit.Assert.assertEquals
import org.junit.Test

class StatsTest {
    @Test
    fun crossFiltersToggle() {
        var f = SearchFilters()
        f = f.toggleStat("language", "fr").toggleStat("category", "video").toggleStat("year", "2023")
        assertEquals(setOf("fr"), f.selectedStat("language"))
        assertEquals(setOf(FfiCategory.VIDEO), f.categories)
        assertEquals(setOf("video"), f.selectedStat("category"))
        assertEquals(2023, f.minYear)
        assertEquals(setOf("2023"), f.selectedStat("year"))
        f = f.toggleStat("year", "2023").toggleStat("category", "video").toggleStat("category", "nope")
        assertEquals(null, f.minYear)
        assertEquals(emptySet<FfiCategory>(), f.categories)
        assertEquals(1, f.activeCount)
    }

    @Test
    fun numbersFollowTheLocale() {
        assertEquals("1,234,567", formatNumber(1_234_567, Locale.US))
        // French groups with a narrow no-break space (U+202F) on recent JDKs, a no-break space before.
        val fr = formatNumber(1_234_567, Locale.FRANCE)
        assertEquals("1234567", fr.filter { it.isDigit() })
        assertEquals(true, fr.contains(' ') || fr.contains(' '))
        assertEquals("1.2k", formatCompact(1_234.0, Locale.US))
        assertEquals("12k", formatCompact(12_345.0, Locale.US))
        assertEquals("1,2\u202Fk", formatCompact(1_234.0, Locale.FRANCE))
        assertEquals("4.5M", formatCompact(4_500_000.0, Locale.US))
        assertEquals("950", formatCompact(950.0, Locale.US))
        assertEquals("", formatShare(1u, 0u, Locale.US))
        assertEquals("25%", formatShare(1u, 4u, Locale.US))
    }

    @Test
    fun bucketLabels() {
        val tz = java.util.TimeZone.getDefault()
        try {
            java.util.TimeZone.setDefault(java.util.TimeZone.getTimeZone("UTC"))
            assertEquals("Oct 12", bucketLabel(1_760_227_200, hourly = false, locale = Locale.US))
            assertEquals("12 oct.", bucketLabel(1_760_227_200, hourly = false, locale = Locale.FRANCE))
            assertEquals("00:00", bucketLabel(1_760_227_200, hourly = true, locale = Locale.FRANCE))
        } finally {
            java.util.TimeZone.setDefault(tz)
        }
    }
}

package app.magnetar.work

import app.magnetar.api.SavedSearch
import app.magnetar.api.SavedSearchHits
import app.magnetar.api.TorrentSummary
import org.testng.Assert.assertEquals
import org.testng.Assert.assertNull
import org.testng.annotations.Test

class PollLogicTest {
    @Test
    fun `the watermark belongs to its server`() {
        assertEquals(sinceFor("http://a", 100, "http://a"), 100L)
        assertNull(sinceFor("http://a", 100, "http://b"))
        assertNull(sinceFor(null, null, "http://a"))
    }

    private fun hits(n: Int, truncated: Boolean = false) = SavedSearchHits(
        search = SavedSearch(id = 4, name = "Dune"),
        hits = (1..n).map { TorrentSummary(infohash = "%040d".format(it), title = "t$it", createdAt = it.toLong()) },
        truncated = truncated,
    )

    @Test
    fun `one notification per search, newest first, five lines and the rest counted`() {
        val c = alertContent(hits(8))!!
        assertEquals(c.title, "Dune: 8 new")
        assertEquals(c.lines, listOf("t8", "t7", "t6", "t5", "t4"))
        assertEquals(c.more, "+3 more")
        assertEquals(c.infohash, "%040d".format(8))
        val one = alertContent(hits(1))!!
        assertEquals(one.title, "Dune: 1 new")
        assertNull(one.more)
        assertEquals(alertContent(hits(200, truncated = true))!!.title, "Dune: 200+ new")
        assertNull(alertContent(hits(0)))
    }
}

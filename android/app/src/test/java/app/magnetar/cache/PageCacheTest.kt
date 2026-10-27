package app.magnetar.cache

import app.magnetar.ui.tempDir
import org.testng.Assert.assertEquals
import org.testng.Assert.assertNotEquals
import org.testng.Assert.assertNull
import org.testng.annotations.Test

class PageCacheTest {
    @Test
    fun `entries are keyed by server, endpoint and body`() {
        var now = 1_000L
        val c = PageCache(tempDir(), clock = { now })
        c.put("http://a", "search", """{"include_adult":false}""", "page-a")
        assertEquals(c.get("http://a", "search", """{"include_adult":false}""")?.json, "page-a")
        assertEquals(c.get("http://a", "search", """{"include_adult":false}""")?.savedAtMs, 1_000L)
        // The adult switch is part of the body: the other value never reads this entry.
        assertNull(c.get("http://a", "search", """{"include_adult":true}"""))
        assertNull(c.get("http://b", "search", """{"include_adult":false}"""))
        assertNull(c.get("http://a", "detail", """{"include_adult":false}"""))
        assertNotEquals(PageCache.key("a", "bc", ""), PageCache.key("ab", "c", ""))
        now = 2_000L
        c.put("http://a", "search", """{"include_adult":false}""", "page-a2")
        assertEquals(c.get("http://a", "search", """{"include_adult":false}""")?.json, "page-a2")
    }

    @Test
    fun `least recently used entries go first past the entry and byte caps`() {
        var now = 0L
        val c = PageCache(tempDir(), maxBytes = 10_000, maxEntries = 3, clock = { now += 1000; now })
        for (i in 1..3) c.put("s", "e", "$i", "x")
        c.get("s", "e", "1") // 1 is now the most recent
        c.put("s", "e", "4", "x")
        assertEquals(c.size().first, 3)
        assertNull(c.get("s", "e", "2"))
        assertEquals(c.get("s", "e", "1")?.json, "x")
        c.put("s", "e", "big", "y".repeat(9_990))
        // The big one alone nearly fills the cap: everything older goes.
        assertEquals(c.size().first, 1)
        assertEquals(c.get("s", "e", "big")?.json?.length, 9_990)
    }

    @Test
    fun `clear empties it`() {
        val c = PageCache(tempDir())
        c.put("s", "e", "1", "x")
        c.clear()
        assertNull(c.get("s", "e", "1"))
        assertEquals(c.size().first, 0)
    }
}

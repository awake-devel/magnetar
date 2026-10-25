package app.magnetar

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class FormatTest {
    private val sintel = "08ada5a7a6183aae1e09d831df6748d566095a10"

    @Test
    fun bytes() {
        assertEquals("?", formatBytes(null))
        assertEquals("512 B", formatBytes(512u))
        assertEquals("1.00 KiB", formatBytes(1024u))
        assertEquals("1.50 MiB", formatBytes(1_572_864u))
        assertEquals("129.2 MiB", formatBytes(135_476_452u))
        assertEquals("4.00 GiB", formatBytes(4uL shl 30))
    }

    @Test
    fun age() {
        val now = 1_000_000_000L
        assertEquals("now", formatAge(now - 5, now))
        assertEquals("5m", formatAge(now - 300, now))
        assertEquals("2h", formatAge(now - 7_200, now))
        assertEquals("3d", formatAge(now - 3 * 86_400, now))
        assertEquals("2y", formatAge(now - 2 * 365 * 86_400, now))
        assertEquals("now", formatAge(now + 100, now))
    }

    @Test
    fun htmlIsStripped() {
        val html = "<p>Big <b>Buck</b> Bunny</p><br/>Rated &amp; reviewed &lt;4K&gt; &#169; &#x2014;<script>x</script>"
        assertEquals("Big Buck Bunny\n\nRated & reviewed <4K> © —x", stripHtml(html))
        assertEquals("plain text", stripHtml("  plain text \n"))
        assertEquals("a\n\nb", stripHtml("a\n\n\n\n\nb"))
    }

    @Test
    fun magnetHex() {
        val m = "magnet:?xt=urn:btih:${sintel.uppercase()}&dn=Sintel&tr=udp%3A%2F%2Ftracker.opentrackr.org%3A1337"
        assertEquals(sintel, magnetInfohash(m))
    }

    @Test
    fun magnetBase32() {
        assertEquals(sintel, magnetInfohash("magnet:?dn=x&xt=urn:btih:BCW2LJ5GDA5K4HQJ3AY56Z2I2VTASWQQ"))
        assertEquals(sintel, magnetInfohash("magnet:?xt=urn:btih:bcw2lj5gda5k4hqj3ay56z2i2vtaswqq"))
    }

    @Test
    fun magnetRejects() {
        assertNull(magnetInfohash("https://example.com/?xt=urn:btih:$sintel"))
        assertNull(magnetInfohash("magnet:?xt=urn:btmh:1220abcd"))
        assertNull(magnetInfohash("magnet:?xt=urn:btih:1234"))
        assertNull(normalizeInfohash("z".repeat(40)))
        assertNull(normalizeInfohash("1".repeat(32))) // '1' is not base32
    }

    @Test
    fun openUris() {
        assertEquals(OpenRequest(sintel, null), parseOpenUri("magnetar://torrent/$sintel"))
        assertEquals(OpenRequest(sintel, null), parseOpenUri("magnetar://torrent/${sintel.uppercase()}/"))
        val m = "magnet:?xt=urn:btih:$sintel"
        assertEquals(OpenRequest(sintel, m), parseOpenUri(m))
        assertNull(parseOpenUri("magnetar://torrent/nothex"))
        assertNull(parseOpenUri("https://example.com"))
    }
}

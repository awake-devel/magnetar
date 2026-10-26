package app.magnetar

import app.magnetar.ui.settings.invalidRelays
import app.magnetar.ui.settings.validateServerUrl
import java.io.File
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class SettingsTest {
    private val dirs = AppDirs(File("/data/dl/magnetar"), File("/data/files/rqbit"))

    @Test
    fun defaultsMapToConfig() {
        val c = AppSettings().toFfiConfig(dirs)
        assertNull(c.serverUrl)
        assertNull(c.apiToken)
        assertNull(c.rootPubkey)
        assertEquals(20.toUByte(), c.trustThreshold)
        assertEquals(512u, c.cacheCapMb)
        assertEquals(emptyList<String>(), c.seedRelays)
        assertEquals("/data/dl/magnetar", c.downloadDir)
        assertEquals("/data/files/rqbit", c.stateDir)
    }

    @Test
    fun blanksBecomeNullAndValuesAreClamped() {
        val c = AppSettings(
            serverUrl = "  ",
            apiToken = " t ",
            rootNpub = "npub1x ",
            trustThreshold = 250,
            cacheCapMb = 0,
            seedRelays = listOf(" wss://a ", "", "wss://b"),
        ).toFfiConfig(dirs)
        assertNull(c.serverUrl)
        assertEquals("t", c.apiToken)
        assertEquals("npub1x", c.rootPubkey)
        assertEquals(100.toUByte(), c.trustThreshold)
        assertEquals(1u, c.cacheCapMb)
        assertEquals(listOf("wss://a", "wss://b"), c.seedRelays)
    }

    @Test
    fun serverUrlValidation() {
        assertEquals(null to null, validateServerUrl(""))
        assertEquals(null to null, validateServerUrl("http://127.0.0.1:8787"))
        assertEquals(null to null, validateServerUrl("http://LOCALHOST:8787"))
        assertEquals(null to null, validateServerUrl("https://phone.tail1234.ts.net"))
        assertEquals(null to null, validateServerUrl("http://nas.tail1234.ts.net:8787"))
        assertEquals(null to null, validateServerUrl("http://[fd7a:115c:a1e0::1]:8787"))
        assertEquals(null to null, validateServerUrl("http://127.0.0.2:8787"))
        assertNotNull(validateServerUrl("http://192.168.1.20:8787").second)
        assertNotNull(validateServerUrl("http://100.128.0.1:8787").second)
        // 100.64.0.0/10 is Tailscale *or* carrier-grade NAT: warns, with the Tailscale-specific text.
        val cgnat = validateServerUrl("http://100.101.102.103:8787").second
        assertNotNull(cgnat)
        assertTrue(cgnat!!.contains("Tailscale is connected"))
        assertFalse(validateServerUrl("http://192.168.1.20:8787").second!!.contains("Tailscale is connected"))
        assertNull(validateServerUrl("https://100.101.102.103:8787").second)
        assertNotNull(validateServerUrl("http://nas.local:8787").second)
        assertNotNull(validateServerUrl("http://evil.ts.net.example.com").second)
        assertNotNull(validateServerUrl("ftp://host").first)
        assertNotNull(validateServerUrl("127.0.0.1:8787").first)
        assertNotNull(validateServerUrl("http://bad host").first)
    }

    @Test
    fun relayValidation() {
        assertEquals(listOf("https://x"), invalidRelays("wss://relay.damus.io\n\n https://x \nws://127.0.0.1:7777"))
    }

    @Test
    fun privateKeysAreRecognised() {
        org.junit.Assert.assertTrue(app.magnetar.ui.settings.isPrivateKey(" nsec1qqqq"))
        org.junit.Assert.assertTrue(app.magnetar.ui.settings.isPrivateKey("NCRYPTSEC1abc"))
        org.junit.Assert.assertFalse(app.magnetar.ui.settings.isPrivateKey("npub1abc"))
        org.junit.Assert.assertFalse(app.magnetar.ui.settings.isPrivateKey(""))
    }
}

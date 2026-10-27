package app.magnetar.settings

import org.testng.Assert.assertEquals
import org.testng.Assert.assertFalse
import org.testng.Assert.assertTrue
import org.testng.annotations.Test

class SettingsStoreTest {
    /** What the old app (Settings.kt before the rebuild) wrote into the `settings` file. */
    private fun oldAppPrefs(background: Boolean = true, adult: Boolean = false) = FakePrefs(
        mapOf(
            "server_url" to " https://ryoku.example.ts.net/ ",
            "api_token" to "tok123 ",
            "root_npub" to "npub1examplexyz",
            "trust_threshold" to 35,
            "cache_cap_mb" to 512,
            "seed_relays" to "wss://a.example\nwss://b.example",
            "background_sync" to background,
            "show_adult" to adult,
        ),
    )

    @Test
    fun `old app settings survive the upgrade`() {
        val prefs = oldAppPrefs()
        val s = SettingsStore(prefs).settings.value
        assertEquals(s.serverUrl, "https://ryoku.example.ts.net")
        assertEquals(s.apiToken, "tok123")
        assertEquals(s.rootNpub, "npub1examplexyz")
        assertEquals(s.trustThreshold, 35)
        assertFalse(s.showAdult)
        assertTrue(s.notifySavedSearches)
        // Obsolete keys are gone, the schema version is written.
        assertFalse(prefs.contains("cache_cap_mb"))
        assertFalse(prefs.contains("seed_relays"))
        assertFalse(prefs.contains("background_sync"))
        assertEquals(prefs.getInt("settings_version", 0), SettingsStore.VERSION)
    }

    @Test
    fun `background sync off becomes notifications off, adult switch kept`() {
        val s = SettingsStore(oldAppPrefs(background = false, adult = true)).settings.value
        assertFalse(s.notifySavedSearches)
        assertTrue(s.showAdult)
    }

    @Test
    fun `fresh install gets the defaults with adult content hidden`() {
        val prefs = FakePrefs()
        val s = SettingsStore(prefs).settings.value
        assertEquals(s, AppSettings())
        assertFalse(s.showAdult)
        assertTrue(s.notifySavedSearches)
        assertEquals(prefs.getInt("settings_version", 0), SettingsStore.VERSION)
    }

    @Test
    fun `a private key in the root field is dropped`() {
        val prefs = FakePrefs(mapOf("root_npub" to "nsec1secretsecret"))
        assertEquals(SettingsStore(prefs).settings.value.rootNpub, "")
        assertEquals(prefs.getString("root_npub", null), "")
        val enc = FakePrefs(mapOf("root_npub" to " NCRYPTSEC1abc"))
        assertEquals(SettingsStore(enc).settings.value.rootNpub, "")
    }

    @Test
    fun `out of range threshold is clamped`() {
        val prefs = FakePrefs(mapOf("trust_threshold" to 250))
        assertEquals(SettingsStore(prefs).settings.value.trustThreshold, 100)
    }

    @Test
    fun `migration runs once and keeps later edits`() {
        val prefs = oldAppPrefs()
        SettingsStore(prefs).update { it.copy(notifySavedSearches = false, showAdult = true) }
        // The old key reappearing (it cannot, but a second run must not re-read it) changes nothing.
        prefs.edit().putBoolean("background_sync", true).commit()
        val again = SettingsStore(prefs).settings.value
        assertFalse(again.notifySavedSearches)
        assertTrue(again.showAdult)
        assertEquals(again.serverUrl, "https://ryoku.example.ts.net")
    }

    @Test
    fun `update persists sanitized values`() {
        val prefs = FakePrefs()
        val store = SettingsStore(prefs)
        store.update {
            it.copy(serverUrl = " http://192.168.1.2:8080// ", apiToken = " t ", rootNpub = "nsec1oops", trustThreshold = -5)
        }
        val s = store.settings.value
        assertEquals(s.serverUrl, "http://192.168.1.2:8080")
        assertEquals(s.apiToken, "t")
        assertEquals(s.rootNpub, "")
        assertEquals(s.trustThreshold, 0)
        assertEquals(SettingsStore.load(prefs), s)
    }

    @Test
    fun `private key detection`() {
        assertTrue(isPrivateKey("nsec1abc"))
        assertTrue(isPrivateKey("  NSEC1ABC"))
        assertTrue(isPrivateKey("ncryptsec1abc"))
        assertFalse(isPrivateKey("npub1abc"))
        assertFalse(isPrivateKey("3bf0c63fcb93463407af97a5e5ee64fa883d107ef9e558472c4eb9aaaefa459d"))
        assertFalse(isPrivateKey(""))
    }
}

package app.magnetar.settings

/** What the user sets in Settings (docs/APP.md §1). Kept in SharedPreferences, see [SettingsStore]. */
data class AppSettings(
    /** Server base URL; empty = not set up yet (the app then asks for it). */
    val serverUrl: String = "",
    val apiToken: String = "",
    /** Trust root: npub, hex or nprofile; never a private key ([isPrivateKey]). */
    val rootNpub: String = "",
    /** 0–100; the server's copy is what filters, this one is pushed on save. */
    val trustThreshold: Int = DEFAULT_THRESHOLD,
    /** "Show adult content" (D52): off = adult torrents hidden everywhere, never deleted. */
    val showAdult: Boolean = false,
    /** Hourly saved-search notifications through the server's poll (D84). */
    val notifySavedSearches: Boolean = true,
) {
    val hasServer: Boolean get() = serverUrl.isNotBlank()

    companion object {
        const val DEFAULT_THRESHOLD = 20
    }
}

/** True for a Nostr private key (bech32 nsec or ncryptsec), which the trust root must never hold. */
fun isPrivateKey(key: String): Boolean =
    key.trim().lowercase().let { it.startsWith("nsec1") || it.startsWith("ncryptsec1") }

/** Trims and drops trailing slashes: `https://host/ ` → `https://host`. */
fun normalizeServerUrl(url: String): String = url.trim().trimEnd('/')

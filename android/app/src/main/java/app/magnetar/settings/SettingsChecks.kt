package app.magnetar.settings

import app.magnetar.api.MagnetarApi

/** Shown when an nsec is pasted into the trust root field; the field is cleared at once. */
const val PRIVATE_KEY_ERROR =
    "That is a private key (nsec): never paste it into an app. Use your public key (npub1…). The field was cleared."

/** A Settings field check: an [error] blocks saving, a [warning] does not. */
data class Check(val error: String? = null, val warning: String? = null)

/**
 * The server URL field: empty is allowed (no server yet); otherwise an http(s) URL with a host.
 * Plain http:// to a host that is neither loopback nor reachable only through Tailscale warns
 * that the token travels readable (D39).
 */
fun checkServerUrl(url: String): Check {
    val u = url.trim()
    if (u.isEmpty()) return Check()
    val parsed = MagnetarApi.parseBaseUrl(u)
        ?: return Check(error = if (u.contains("://") && !u.startsWith("http", ignoreCase = true)) {
            "Must be an http:// or https:// URL"
        } else if (!u.contains("://")) {
            "Start with https:// or http://"
        } else {
            "Not a valid URL"
        })
    if (parsed.scheme != "http" || isPrivateTransport(parsed.host)) return Check()
    return Check(
        warning = if (isTailscaleCgnat(parsed.host)) {
            "Plain HTTP to a 100.x address: encrypted only while Tailscale is connected on this phone. " +
                "Without it, 100.64.0.0/10 is carrier-grade NAT space and the API token travels readable. " +
                "Prefer the *.ts.net name or https://."
        } else {
            "Plain HTTP: the API token and results can be read by anyone on this network. " +
                "Prefer https:// (tailscale serve or a reverse proxy)."
        },
    )
}

/**
 * Hosts where plain http:// is not exposed on the wire (D39): loopback, MagicDNS `*.ts.net`
 * names and the Tailscale IPv6 range fd7a:115c:a1e0::/48. Tailscale IPv4 (100.64.0.0/10) is not
 * here: that range is also carrier-grade NAT, routed in clear when the tunnel is down.
 */
fun isPrivateTransport(host: String): Boolean {
    val h = host.lowercase().removePrefix("[").removeSuffix("]")
    if (h == "localhost" || h == "::1" || h.endsWith(".ts.net")) return true
    ipv4(h)?.let { return it[0] == 127 }
    return h.startsWith("fd7a:115c:a1e0:")
}

/** 100.64.0.0/10: a Tailscale address, or carrier-grade NAT when the tunnel is down. */
fun isTailscaleCgnat(host: String): Boolean {
    val v4 = ipv4(host) ?: return false
    return v4[0] == 100 && v4[1] in 64..127
}

private fun ipv4(host: String): List<Int>? {
    val parts = host.split('.').takeIf { it.size == 4 }?.map { it.toIntOrNull() ?: return null } ?: return null
    return parts.takeIf { p -> p.all { it in 0..255 } }
}

/** The trust root field: empty (no root), or an npub / nprofile / 64-digit hex public key. */
fun checkRootKey(key: String): Check {
    val k = key.trim()
    if (k.isEmpty()) return Check()
    if (isPrivateKey(k)) return Check(error = PRIVATE_KEY_ERROR)
    if (Nip19.pubkeyHex(k) == null) return Check(error = "Not a public key: paste an npub1…, an nprofile1… or 64 hex digits")
    return Check()
}

/** The API token goes into an HTTP header: printable ASCII only (a pasted line break would fail every call). */
fun checkToken(token: String): Check {
    val t = token.trim()
    if (t.any { it.code < 0x21 || it.code > 0x7e }) return Check(error = "The token has a space, line break or non-ASCII character")
    return Check()
}

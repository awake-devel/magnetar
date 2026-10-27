package app.magnetar.cache

import java.io.File
import java.io.IOException
import java.security.MessageDigest

/**
 * Recent server answers on disk (docs/APP.md §5, D83): first search pages, details, title
 * pages, as the JSON the app decoded them from. Keyed by SHA-256 of (server URL, endpoint,
 * canonical request body incl. the adult switch), so another server, endpoint, query or switch
 * never reads an entry. Capped at [maxBytes] / [maxEntries], least recently used out first.
 *
 * Only server answers go in, so nothing the server filtered out (the illegal-content blocklist,
 * docs/DHT.md §7; adult hits while the switch is off) is ever stored here. Each entry is one
 * file: line 1 the save time (unix ms), the rest the JSON. All methods block: call them off the
 * main thread. Every I/O failure is swallowed (the cache is only a convenience).
 */
class PageCache(
    private val dir: File,
    private val maxBytes: Long = 20L * 1024 * 1024,
    private val maxEntries: Int = 500,
    private val clock: () -> Long = System::currentTimeMillis,
) {
    class Entry(val savedAtMs: Long, val json: String)

    private val lock = Any()

    fun get(server: String, endpoint: String, body: String): Entry? = synchronized(lock) {
        val f = file(server, endpoint, body)
        try {
            if (!f.isFile) return null
            val text = f.readText()
            val nl = text.indexOf('\n')
            val at = text.substring(0, nl.coerceAtLeast(0)).toLongOrNull() ?: return null.also { f.delete() }
            // Most recently used = newest mtime; LRU eviction reads it.
            f.setLastModified(clock())
            Entry(at, text.substring(nl + 1))
        } catch (_: IOException) {
            null
        } catch (_: SecurityException) {
            null
        }
    }

    fun put(server: String, endpoint: String, body: String, json: String) = synchronized(lock) {
        try {
            dir.mkdirs()
            val f = file(server, endpoint, body)
            val tmp = File(dir, f.name + ".tmp")
            tmp.writeText("${clock()}\n$json")
            if (!tmp.renameTo(f)) {
                f.delete()
                tmp.renameTo(f)
            }
            f.setLastModified(clock())
            trim()
        } catch (_: IOException) {
        } catch (_: SecurityException) {
        }
    }

    /** Server URL or token changed (docs/APP.md §5). */
    fun clear() = synchronized(lock) {
        dir.listFiles()?.forEach { it.delete() }
    }

    /** Entries and bytes on disk (tests, diagnostics). */
    fun size(): Pair<Int, Long> = synchronized(lock) {
        val files = dir.listFiles().orEmpty().filter { it.isFile }
        files.size to files.sumOf { it.length() }
    }

    private fun trim() {
        val files = dir.listFiles().orEmpty().filter { it.isFile }.sortedByDescending { it.lastModified() }
        var bytes = 0L
        for ((i, f) in files.withIndex()) {
            bytes += f.length()
            if (i >= maxEntries || bytes > maxBytes) f.delete()
        }
    }

    private fun file(server: String, endpoint: String, body: String) =
        File(dir, key(server, endpoint, body))

    companion object {
        fun key(server: String, endpoint: String, body: String): String {
            val md = MessageDigest.getInstance("SHA-256")
            for (part in listOf(server, endpoint, body)) {
                md.update(part.toByteArray(Charsets.UTF_8))
                md.update(0)
            }
            return md.digest().joinToString("") { "%02x".format(it) }
        }
    }
}

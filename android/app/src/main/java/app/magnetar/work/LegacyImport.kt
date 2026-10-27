package app.magnetar.work

import android.database.sqlite.SQLiteDatabase
import app.magnetar.api.ApiException
import app.magnetar.api.MagnetarApi
import app.magnetar.api.MagnetarService
import app.magnetar.api.SavedSearch
import app.magnetar.api.SearchQuery
import java.io.File
import kotlinx.coroutines.CancellationException

/** One saved search of the old app's SQLite file. */
data class LegacySearch(val name: String, val queryJson: String, val notify: Boolean)

/**
 * The old app kept its saved searches only on the phone, in `databases/magnetar.db` (table
 * `saved_searches(name, query JSON, notify)`, docs/APP.md §7). They are created on the server
 * once, skipping a name + query already there; only when every one was created (or was there)
 * are the old database and the old `files/rqbit/` state deleted. Downloaded files are left alone.
 */
object LegacyImport {
    /** Reads the old rows with the platform SQLite API; null when there is no readable old file. */
    fun read(db: File): List<LegacySearch>? {
        if (!db.isFile) return null
        return try {
            SQLiteDatabase.openDatabase(db.path, null, SQLiteDatabase.OPEN_READONLY).use { d ->
                d.rawQuery("SELECT name, query, notify FROM saved_searches ORDER BY id", null).use { c ->
                    buildList {
                        while (c.moveToNext()) add(LegacySearch(c.getString(0) ?: "", c.getString(1) ?: "{}", c.getInt(2) != 0))
                    }
                }
            }
        } catch (_: Exception) {
            // No such table (an old app that never saved one), or a file SQLite cannot read.
            if (runCatching { tableMissing(db) }.getOrDefault(false)) emptyList() else null
        }
    }

    private fun tableMissing(db: File): Boolean =
        SQLiteDatabase.openDatabase(db.path, null, SQLiteDatabase.OPEN_READONLY).use { d ->
            d.rawQuery("SELECT name FROM sqlite_master WHERE type = 'table' AND name = 'saved_searches'", null).use { it.count == 0 }
        }

    /**
     * Creates [old] on the server, skipping those already there (same name and query). True when
     * every one is on the server now, so the old files may go.
     */
    suspend fun upload(service: MagnetarService, old: List<LegacySearch>): Boolean {
        if (old.isEmpty()) return true
        val existing = try {
            service.savedSearches()
        } catch (e: CancellationException) {
            throw e
        } catch (_: ApiException) {
            return false
        }
        fun canon(q: SearchQuery) = MagnetarApi.JSON.encodeToString(SearchQuery.serializer(), q.copy(offset = 0))
        val have = existing.mapTo(HashSet()) { it.name to canon(it.query) }
        for (s in old) {
            val q = runCatching { MagnetarApi.JSON.decodeFromString(SearchQuery.serializer(), s.queryJson) }.getOrNull()
                ?: SearchQuery(text = s.name)
            if ((s.name to canon(q)) in have) continue
            try {
                service.createSavedSearch(SavedSearch(name = s.name.ifBlank { q.text.ifBlank { "Saved search" } }, query = q.copy(offset = 0), notify = s.notify))
                have += s.name to canon(q)
            } catch (e: CancellationException) {
                throw e
            } catch (_: ApiException) {
                return false
            }
        }
        return true
    }

    /** `magnetar.db`, its `-wal` / `-shm` / `-journal`, and the old torrent engine's state. */
    fun deleteOldFiles(db: File, rqbitState: File) {
        for (suffix in listOf("", "-wal", "-shm", "-journal")) File(db.path + suffix).delete()
        rqbitState.deleteRecursively()
    }
}

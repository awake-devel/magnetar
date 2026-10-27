package app.magnetar.ui.crawlmap

import android.content.Context
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.graphics.PathFillType
import java.io.ByteArrayInputStream
import java.io.DataInputStream
import java.io.EOFException
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext

/** One country of the world outline: its ISO alpha-2 code and its rings (x, y interleaved, map units). */
class CountryShape(val code: String, val rings: List<IntArray>) {
    val points: Int get() = rings.sumOf { it.size / 2 }

    /** Even-odd test over every ring (outer rings and holes alike), in map units. */
    fun contains(x: Float, y: Float): Boolean {
        var inside = false
        for (r in rings) {
            val n = r.size / 2
            var j = n - 1
            for (i in 0 until n) {
                val xi = r[2 * i].toFloat()
                val yi = r[2 * i + 1].toFloat()
                val xj = r[2 * j].toFloat()
                val yj = r[2 * j + 1].toFloat()
                if ((yi > y) != (yj > y) && x < (xj - xi) * (y - yi) / (yj - yi) + xi) inside = !inside
                j = i
            }
        }
        return inside
    }
}

/**
 * The world outline of the crawl map (docs/APP.md §15): Natural Earth 1:110m Admin 0 countries
 * (public domain), simplified, equirectangular, quantised by `android/tools/world_map.py` into
 * `assets/world_110m.bin`. x = (lon − [west]) × [scale], y = ([north] − lat) × [scale].
 */
class WorldMap(
    val scale: Int,
    val west: Int,
    val north: Int,
    val width: Int,
    val height: Int,
    val countries: List<CountryShape>,
) {
    private val byCode = countries.associateBy { it.code }

    fun country(code: String): CountryShape? = byCode[code.uppercase()]

    /** The country under ([x], [y]) in map units, or null (sea, Antarctica, a country left out). */
    fun countryAt(x: Float, y: Float): CountryShape? = countries.firstOrNull { it.contains(x, y) }

    companion object {
        const val ASSET = "world_110m.bin"
        private const val MAGIC = 0x4D47574D // "MGWM"

        /** Parses the asset; [IllegalArgumentException] for anything that is not a version-1 map. */
        fun parse(bytes: ByteArray): WorldMap {
            val d = DataInputStream(ByteArrayInputStream(bytes))
            try {
                require(d.readInt() == MAGIC) { "not a world map" }
                require(d.readUnsignedByte() == 1) { "unknown world map version" }
                val scale = d.readUnsignedShort()
                val west = d.readShort().toInt()
                val north = d.readShort().toInt()
                val width = d.readUnsignedShort()
                val height = d.readUnsignedShort()
                require(scale > 0 && width > 0 && height > 0) { "empty world map" }
                val n = d.readUnsignedShort()
                val countries = ArrayList<CountryShape>(n)
                repeat(n) {
                    val code = String(byteArrayOf(d.readByte(), d.readByte()), Charsets.US_ASCII)
                    val rings = List(d.readUnsignedShort()) {
                        val points = d.readUnsignedShort()
                        IntArray(points * 2) { d.readUnsignedShort() }
                    }
                    countries += CountryShape(code, rings)
                }
                require(d.read() == -1) { "trailing bytes in world map" }
                return WorldMap(scale, west, north, width, height, countries)
            } catch (e: EOFException) {
                throw IllegalArgumentException("truncated world map", e)
            }
        }
    }
}

/** The parsed map with one even-odd [Path] per country, built once off the main thread. */
class WorldShapes(val map: WorldMap) {
    val paths: List<Path> = map.countries.map { c ->
        Path().apply {
            fillType = PathFillType.EvenOdd
            for (r in c.rings) {
                if (r.size < 6) continue
                moveTo(r[0].toFloat(), r[1].toFloat())
                for (i in 1 until r.size / 2) lineTo(r[2 * i].toFloat(), r[2 * i + 1].toFloat())
                close()
            }
        }
    }
}

/** Loads [WorldMap.ASSET] once per process, on [Dispatchers.Default]; null if it cannot be read. */
object WorldMapAsset {
    private val lock = Mutex()

    @Volatile private var loaded: WorldShapes? = null

    suspend fun load(context: Context): WorldShapes? = loaded ?: lock.withLock {
        loaded ?: withContext(Dispatchers.Default) {
            try {
                val bytes = context.applicationContext.assets.open(WorldMap.ASSET).use { it.readBytes() }
                WorldShapes(WorldMap.parse(bytes))
            } catch (e: CancellationException) {
                throw e
            } catch (_: Exception) {
                null
            }
        }.also { loaded = it }
    }
}

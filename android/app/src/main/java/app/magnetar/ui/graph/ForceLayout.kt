package app.magnetar.ui.graph

import kotlin.math.max
import kotlin.math.min
import kotlin.math.sqrt
import kotlin.random.Random

/**
 * Force-directed layout (Fruchterman–Reingold with gravity and cooling) for the D47 graphs:
 * every pair repels, edges pull, a weak pull towards the centre keeps components together.
 * O(n²) per step, fine for the server's 200-node cap (≈ 40 k pairs, a few ms per step on a
 * phone). Plain Kotlin and deterministic (seeded start), run off the main thread by the caller.
 *
 * @param n nodes; [edges] pairs of node indexes; [pinned] stays at the origin (the graph's
 *   subject: trust root, title, torrent).
 */
class ForceLayout(
    private val n: Int,
    private val edges: List<Pair<Int, Int>>,
    private val pinned: Int? = null,
    seed: Int = 7,
) {
    val x = FloatArray(n)
    val y = FloatArray(n)
    private val dx = FloatArray(n)
    private val dy = FloatArray(n)

    /** Ideal edge length (layout units; the view scales to fit). */
    private val k = 40f

    /**
     * Repulsion stops beyond this distance. Without a cutoff, parts with no edge between them
     * push each other until gravity balances it, several cluster radii away, and fitting the
     * view to them shrinks the main cluster to a corner.
     */
    private val cutoff2 = (6 * k) * (6 * k)

    /**
     * Between nodes of different connected parts the repulsion reaches only 1.5 edge lengths: a
     * detached island or a lone node is still kept from overlapping the others, but is not pushed
     * out to a halo several cluster radii away (where it would freeze as the layout cools, and the
     * fitted view would shrink the main cluster). Gravity then packs the islands around it.
     */
    private val crossCutoff2 = (1.5f * k) * (1.5f * k)

    /** Connected part of each node (union-find over [edges]). */
    private val part = IntArray(n) { it }.also { p ->
        fun find(a: Int): Int {
            var r = a
            while (p[r] != r) r = p[r]
            var c = a
            while (p[c] != r) { val next = p[c]; p[c] = r; c = next }
            return r
        }
        for ((a, b) in edges) if (a in 0 until n && b in 0 until n) p[find(a)] = find(b)
        for (i in 0 until n) p[i] = find(i)
    }
    private var temperature = k * 3
    var steps = 0
        private set

    init {
        // A disc of radius ~ k·√n, so the start is neither crushed nor exploded.
        val rnd = Random(seed)
        val r = k * sqrt(n.toFloat())
        for (i in 0 until n) {
            val a = rnd.nextFloat() * 2 * Math.PI.toFloat()
            val d = r * sqrt(rnd.nextFloat())
            x[i] = d * kotlin.math.cos(a)
            y[i] = d * kotlin.math.sin(a)
        }
        pinned?.let { x[it] = 0f; y[it] = 0f }
    }

    /** One step; returns the largest move (a settled layout moves under 0.5). */
    fun step(): Float {
        dx.fill(0f)
        dy.fill(0f)
        val k2 = k * k
        for (i in 0 until n) {
            for (j in i + 1 until n) {
                var ddx = x[i] - x[j]
                var ddy = y[i] - y[j]
                var d2 = ddx * ddx + ddy * ddy
                if (d2 < 0.01f) {
                    // Coincident nodes: push apart along a fixed direction per pair.
                    ddx = ((i * 31 + j) % 7 - 3).toFloat() * 0.1f + 0.05f
                    ddy = ((i * 17 + j) % 5 - 2).toFloat() * 0.1f + 0.05f
                    d2 = ddx * ddx + ddy * ddy
                }
                if (d2 > (if (part[i] == part[j]) cutoff2 else crossCutoff2)) continue
                val f = k2 / d2
                dx[i] += ddx * f
                dy[i] += ddy * f
                dx[j] -= ddx * f
                dy[j] -= ddy * f
            }
        }
        for ((a, b) in edges) {
            if (a !in 0 until n || b !in 0 until n || a == b) continue
            val ddx = x[a] - x[b]
            val ddy = y[a] - y[b]
            val d = sqrt(ddx * ddx + ddy * ddy).coerceAtLeast(0.01f)
            val f = d / k
            dx[a] -= ddx * f
            dy[a] -= ddy * f
            dx[b] += ddx * f
            dy[b] += ddy * f
        }
        var moved = 0f
        for (i in 0 until n) {
            if (i == pinned) continue
            // Gravity grows with distance, so detached parts stay in view.
            dx[i] -= x[i] * 0.02f
            dy[i] -= y[i] * 0.02f
            val len = sqrt(dx[i] * dx[i] + dy[i] * dy[i])
            if (len > 0f) {
                val m = min(len, temperature)
                x[i] += dx[i] / len * m
                y[i] += dy[i] / len * m
                moved = max(moved, m)
            }
        }
        // Cools below the settle threshold, so `run` stops once the layout has frozen (a floor
        // at the threshold itself would keep `moved` at 0.5 and always run to the cap).
        temperature = max(temperature * 0.97f, 0.25f)
        steps++
        return moved
    }

    /** Steps until settled or [maxSteps]; [onProgress] gets a copy every [every] steps. */
    fun run(maxSteps: Int = 400, every: Int = 25, onProgress: (FloatArray, FloatArray) -> Unit = { _, _ -> }) {
        for (s in 1..maxSteps) {
            val moved = step()
            if (s % every == 0) onProgress(x.copyOf(), y.copyOf())
            if (moved < 0.5f && s > 50) break
        }
    }
}

/** Screen transform of a layout: screen = layout × [scale] + ([ox], [oy]). */
data class ViewTransform(val scale: Float, val ox: Float, val oy: Float)

/**
 * The transform that fits the first [n] positions into a [width] × [height] view with [pad]
 * pixels free on every side (node radii and labels), centred; [scale] is clamped to
 * [minScale]–[maxScale] so a single node or a crushed layout does not zoom absurdly.
 */
fun fitTransform(
    x: FloatArray,
    y: FloatArray,
    n: Int = minOf(x.size, y.size),
    width: Float,
    height: Float,
    pad: Float,
    minScale: Float = 0.05f,
    maxScale: Float = 6f,
): ViewTransform {
    if (n <= 0 || width <= 0f || height <= 0f) return ViewTransform(1f, width / 2f, height / 2f)
    var minX = Float.MAX_VALUE
    var maxX = -Float.MAX_VALUE
    var minY = Float.MAX_VALUE
    var maxY = -Float.MAX_VALUE
    for (i in 0 until n) {
        minX = min(minX, x[i]); maxX = max(maxX, x[i])
        minY = min(minY, y[i]); maxY = max(maxY, y[i])
    }
    val s = min(
        (width - 2 * pad).coerceAtLeast(1f) / (maxX - minX).coerceAtLeast(1f),
        (height - 2 * pad).coerceAtLeast(1f) / (maxY - minY).coerceAtLeast(1f),
    ).coerceIn(minScale, maxScale)
    return ViewTransform(s, width / 2f - (minX + maxX) / 2f * s, height / 2f - (minY + maxY) / 2f * s)
}

/**
 * Index of the node nearest to ([px], [py]) on screen within its touch radius ([radius] or at
 * least [minTouch] pixels), or null.
 */
fun nodeAt(px: Float, py: Float, x: FloatArray, y: FloatArray, n: Int, t: ViewTransform, radius: FloatArray, minTouch: Float): Int? {
    var best: Int? = null
    var bestD = Float.MAX_VALUE
    for (i in 0 until minOf(n, x.size, y.size, radius.size)) {
        val sx = x[i] * t.scale + t.ox
        val sy = y[i] * t.scale + t.oy
        val d = (sx - px) * (sx - px) + (sy - py) * (sy - py)
        val r = max(radius[i], minTouch)
        if (d < r * r && d < bestD) {
            best = i
            bestD = d
        }
    }
    return best
}

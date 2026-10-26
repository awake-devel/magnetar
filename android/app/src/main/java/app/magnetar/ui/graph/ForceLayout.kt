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

package app.magnetar.ui

import app.magnetar.ui.graph.ForceLayout
import app.magnetar.ui.graph.ViewTransform
import app.magnetar.ui.graph.fitTransform
import app.magnetar.ui.graph.nodeAt
import kotlin.math.hypot
import org.testng.Assert.assertEquals
import org.testng.Assert.assertNull
import org.testng.Assert.assertTrue
import org.testng.annotations.Test

class GraphLayoutTest {
    private fun dist(l: ForceLayout, a: Int, b: Int) = hypot((l.x[a] - l.x[b]).toDouble(), (l.y[a] - l.y[b]).toDouble())

    @Test
    fun `linked nodes end up closer and the subject stays centred`() {
        // Two stars of 10 leaves, hubs 0 and 11, joined by one edge.
        val edges = (1..10).map { 0 to it } + (12..21).map { 11 to it } + listOf(0 to 11)
        val l = ForceLayout(22, edges, pinned = 0)
        l.run(maxSteps = 400)
        assertEquals(l.x[0], 0f)
        assertEquals(l.y[0], 0f)
        assertTrue((0 until 22).all { l.x[it].isFinite() && l.y[it].isFinite() })
        val within = (1..10).map { dist(l, 0, it) }.average()
        val across = (12..21).map { dist(l, 0, it) }.average()
        assertTrue(within < across, "leaves near their hub: $within vs $across")
    }

    @Test
    fun `deterministic and settles before the cap`() {
        val edges = (1 until 50).map { (it - 1) / 3 to it }
        val a = ForceLayout(50, edges).apply { run() }
        val b = ForceLayout(50, edges).apply { run() }
        assertTrue(a.x.contentEquals(b.x) && a.y.contentEquals(b.y))
        assertTrue(a.steps in 51 until 400, "stops once settled: ${a.steps}")
    }

    @Test
    fun `copes with odd input`() {
        ForceLayout(0, emptyList()).run()
        // Out-of-range and self edges are ignored, a single node stays put.
        val l = ForceLayout(1, listOf(0 to 0, 0 to 5), pinned = 0)
        l.run()
        assertEquals(l.x[0], 0f)
    }

    @Test
    fun `detached islands and lone nodes stay near the main cluster`() {
        // The web-of-trust shape seen on production (107 nodes, 76 edges): a 60-node star, a
        // separate 4-node star, and 20 publishers with no edge at all.
        val edges = (1 until 60).map { 0 to it } + (61..63).map { 60 to it }
        val n = 84
        val l = ForceLayout(n, edges, pinned = 0).apply { run() }
        val cx = (0 until 60).map { l.x[it] }.average()
        val cy = (0 until 60).map { l.y[it] }.average()
        val radius = (0 until 60).maxOf { hypot(l.x[it] - cx, l.y[it] - cy) }
        val apart = hypot(l.x[60] - cx, l.y[60] - cy)
        assertTrue(apart < 3 * radius, "detached hub within 3 cluster radii: $apart vs $radius")
        val farthestLone = (64 until n).maxOf { hypot(l.x[it] - cx, l.y[it] - cy) }
        assertTrue(farthestLone < 3 * radius, "lone nodes within 3 cluster radii: $farthestLone vs $radius")
        // Fitted to a phone screen, the main cluster still fills a fair part of it.
        val t = fitTransform(l.x, l.y, n, width = 1080f, height = 1600f, pad = 60f)
        assertTrue(radius * t.scale > 1080 * 0.12, "main cluster drawn at ${radius * t.scale} px")
    }

    @Test
    fun `fit puts every node inside the padded view, centred`() {
        val x = floatArrayOf(-100f, 300f, 50f)
        val y = floatArrayOf(-20f, 40f, 400f)
        val t = fitTransform(x, y, width = 1000f, height = 800f, pad = 50f)
        for (i in x.indices) {
            val sx = x[i] * t.scale + t.ox
            val sy = y[i] * t.scale + t.oy
            assertTrue(sx in 49.9f..950.1f && sy in 49.9f..750.1f, "node $i at $sx,$sy")
        }
        // The limiting axis (height: 420 units into 700 px) touches both pads.
        val top = (-20f) * t.scale + t.oy
        val bottom = 400f * t.scale + t.oy
        assertEquals(top, 50f, 0.5f)
        assertEquals(bottom, 750f, 0.5f)
        val midX = ((-100f + 300f) / 2) * t.scale + t.ox
        assertEquals(midX, 500f, 0.5f)
    }

    @Test
    fun `fit of a single node or nothing does not zoom absurdly`() {
        val one = fitTransform(floatArrayOf(5f), floatArrayOf(5f), width = 500f, height = 500f, pad = 20f)
        assertTrue(one.scale <= 6f)
        assertEquals(5f * one.scale + one.ox, 250f, 0.01f)
        val none = fitTransform(FloatArray(0), FloatArray(0), width = 500f, height = 500f, pad = 20f)
        assertEquals(none, ViewTransform(1f, 250f, 250f))
    }

    @Test
    fun `tap hits the nearest node within its touch radius`() {
        val x = floatArrayOf(0f, 10f, 100f)
        val y = floatArrayOf(0f, 0f, 0f)
        val t = ViewTransform(2f, 100f, 100f)
        val r = floatArrayOf(5f, 5f, 30f)
        // Node 1 is at (120, 100); node 0 at (100, 100).
        assertEquals(nodeAt(118f, 101f, x, y, 3, t, r, minTouch = 12f), 1)
        assertEquals(nodeAt(103f, 100f, x, y, 3, t, r, minTouch = 12f), 0)
        // Big node 2 at (300, 100) with a 30 px radius.
        assertEquals(nodeAt(325f, 100f, x, y, 3, t, r, minTouch = 12f), 2)
        assertNull(nodeAt(200f, 300f, x, y, 3, t, r, minTouch = 12f))
        // A layout still shorter than the node list (first progress report) never overruns.
        assertNull(nodeAt(0f, 0f, FloatArray(0), FloatArray(0), 3, t, r, minTouch = 12f))
    }
}

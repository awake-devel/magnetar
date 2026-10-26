package app.magnetar

import app.magnetar.ui.graph.ForceLayout
import kotlin.math.hypot
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class ForceLayoutTest {
    private fun dist(l: ForceLayout, a: Int, b: Int) = hypot((l.x[a] - l.x[b]).toDouble(), (l.y[a] - l.y[b]).toDouble())

    @Test
    fun linkedNodesEndUpCloserAndTheSubjectStaysCentred() {
        // Two stars of 10 leaves, hubs 0 and 11, joined by one edge.
        val edges = (1..10).map { 0 to it } + (12..21).map { 11 to it } + listOf(0 to 11)
        val l = ForceLayout(22, edges, pinned = 0)
        l.run(maxSteps = 400)
        assertEquals(0f, l.x[0], 0f)
        assertEquals(0f, l.y[0], 0f)
        assertTrue((0 until 22).all { l.x[it].isFinite() && l.y[it].isFinite() })
        val within = (1..10).map { dist(l, 0, it) }.average()
        val across = (12..21).map { dist(l, 0, it) }.average()
        assertTrue("leaves near their hub: $within vs $across", within < across)
    }

    @Test
    fun deterministicAndSettles() {
        val edges = (1 until 50).map { (it - 1) / 3 to it }
        val a = ForceLayout(50, edges).apply { run() }
        val b = ForceLayout(50, edges).apply { run() }
        assertTrue(a.x.contentEquals(b.x) && a.y.contentEquals(b.y))
        assertTrue("stops before the cap once settled: ${a.steps}", a.steps in 51 until 400)
    }

    @Test
    fun copesWithOddInput() {
        ForceLayout(0, emptyList()).run()
        // Out-of-range and self edges are ignored, a single node stays put.
        val l = ForceLayout(1, listOf(0 to 0, 0 to 5), pinned = 0)
        l.run()
        assertEquals(0f, l.x[0], 0f)
    }

    @Test
    fun detachedPartsStayNearTheMainCluster() {
        // A 60-node star and a separate 4-node star (the relays graph's shape).
        val edges = (1 until 60).map { 0 to it } + (61..63).map { 60 to it }
        val l = ForceLayout(64, edges).apply { run() }
        val cx = (0 until 60).map { l.x[it] }.average()
        val cy = (0 until 60).map { l.y[it] }.average()
        val radius = (0 until 60).maxOf { hypot(l.x[it] - cx, l.y[it] - cy) }
        val apart = hypot(l.x[60] - cx, l.y[60] - cy)
        assertTrue("detached hub within 3 cluster radii: $apart vs $radius", apart < 3 * radius)
    }
}

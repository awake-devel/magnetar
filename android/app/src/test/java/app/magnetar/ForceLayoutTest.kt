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
        assertTrue("stops before the cap once settled", a.steps in 51..400)
    }

    @Test
    fun copesWithOddInput() {
        ForceLayout(0, emptyList()).run()
        // Out-of-range and self edges are ignored, a single node stays put.
        val l = ForceLayout(1, listOf(0 to 0, 0 to 5), pinned = 0)
        l.run()
        assertEquals(0f, l.x[0], 0f)
    }
}

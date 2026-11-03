package app.magnetar.ui

import app.magnetar.api.ApiException
import app.magnetar.api.FleetStats
import app.magnetar.api.MagnetarApi
import app.magnetar.api.NamedCount
import app.magnetar.api.ProbeBackfillStats
import app.magnetar.api.WorkerBackfillState
import app.magnetar.api.WorkerDhtState
import app.magnetar.api.WorkerProbeState
import app.magnetar.api.WorkerStats
import app.magnetar.api.named
import app.magnetar.ui.stats.Activity
import app.magnetar.ui.stats.BackfillProgress
import app.magnetar.ui.stats.StatsViewModel
import app.magnetar.ui.stats.backfillLines
import app.magnetar.ui.stats.backfillPercent
import app.magnetar.ui.stats.backfillProgress
import app.magnetar.ui.stats.outcomeLabel
import app.magnetar.ui.stats.workerBackfillLines
import app.magnetar.ui.stats.workerBadges
import java.util.Locale
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.advanceUntilIdle
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.test.setMain
import org.testng.Assert.assertEquals
import org.testng.Assert.assertFalse
import org.testng.Assert.assertNotNull
import org.testng.Assert.assertNull
import org.testng.Assert.assertTrue
import org.testng.annotations.AfterMethod
import org.testng.annotations.Test

/** Header-probe stats (D117, D118): `GET /api/v1/stats` decoding, the Status-tab lines, loading. */
@OptIn(ExperimentalCoroutinesApi::class)
class ProbeStatsTest {
    private val en = Locale.US
    /** The app's own decoder (ignoreUnknownKeys), as the API client and the cache use it. */
    private val json = MagnetarApi.JSON

    @AfterMethod
    fun reset() = Dispatchers.resetMain()

    private fun TestScope.main() = Dispatchers.setMain(StandardTestDispatcher(testScheduler))

    /** A newer server's answer, shaped as magnetar_core::api::StatsResponse serializes it. */
    private val newer = """{"torrents":1800000,"crawler":{"running":false},"dht":null,
        "workers":[{"name":"eee","connected":true,"version":"0.9","arch":"x86_64","first_seen":1,"last_seen":2,
          "leases_held":0,"liveness_results_total":5,"stored_total":7,
          "backfill_leased_total":512,"backfill_probed_total":40,"backfill_unprobed_total":300,
          "pressure":false,"dht":{"mode":"polite","nodes":1,"ports":[6881],"qps":4.5,"discover":false,
            "probe":{"upload":false,"started":50,"ok":40,"skipped":[["rate",3],["adult",1]]},
            "backfill":{"leased":512,"released":2,"outstanding":30,"no_peers":250,"failed":20,"fetched":60,
              "probed":40,"unprobed":20,"lease_errors":1}}},
         {"name":"rpi","connected":false,"dht":{"mode":"polite","qps":1.0}}],
        "probe_backfill":{"waiting":1000,"leased":30,"retrying":200,"remaining":1230,
          "closed":[["no_peers",400],["adult",12],["private",3]],"probed":770,"filling":false,
          "filled_at":1793000000,"at":1793000100,"future":1}}"""

    /** An older server: no `workers` backfill counters, no `probe_backfill`. */
    private val older = """{"torrents":5,"crawler":{"running":true},"workers":[{"name":"eee","connected":true,
        "dht":{"mode":"polite","qps":3.0}}]}"""

    @Test
    fun `a newer stats answer decodes the backfill and the worker counters`() {
        val f = json.decodeFromString(FleetStats.serializer(), newer)
        val b = f.probeBackfill!!
        assertEquals(b.remaining, 1230L)
        assertEquals(b.probed, 770L)
        assertEquals(b.closed, listOf(NamedCount("no_peers", 400), NamedCount("adult", 12), NamedCount("private", 3)))
        assertEquals(b.filledAt, 1793000000L)
        val eee = f.workers.first { it.name == "eee" }
        assertEquals(eee.backfillLeasedTotal, 512L)
        assertEquals(eee.backfillProbedTotal, 40L)
        assertEquals(eee.backfillUnprobedTotal, 300L)
        assertEquals(eee.dht?.discover, false)
        assertEquals(eee.dht?.backfill?.noPeers, 250L)
        assertEquals(eee.dht?.backfill?.leaseErrors, 1L)
        assertEquals(eee.dht?.probe?.skipped, listOf(NamedCount("rate", 3), NamedCount("adult", 1)))
        val rpi = f.workers.first { it.name == "rpi" }
        assertNull(rpi.dht?.discover)
        assertNull(rpi.dht?.backfill)
        // The cache writes and reads the same shape (pairs as two-element arrays).
        val again = json.decodeFromString(FleetStats.serializer(), json.encodeToString(FleetStats.serializer(), f))
        assertEquals(again, f)
        assertTrue(json.encodeToString(FleetStats.serializer(), f).contains("[\"no_peers\",400]"))
    }

    @Test
    fun `an older stats answer decodes with nothing to show`() {
        val f = json.decodeFromString(FleetStats.serializer(), older)
        assertNull(f.probeBackfill)
        val w = f.workers.single()
        assertEquals(w.backfillLeasedTotal, 0L)
        assertNull(w.dht?.discover)
        assertNull(w.dht?.backfill)
        assertTrue(workerBadges(w).isEmpty())
        assertTrue(workerBackfillLines(w).isEmpty())
        // And an answer without any worker field at all.
        assertEquals(json.decodeFromString(FleetStats.serializer(), """{"torrents":1}"""), FleetStats())
    }

    @Test
    fun `malformed closed pairs are dropped, not fatal`() {
        val b = json.decodeFromString(
            ProbeBackfillStats.serializer(),
            """{"closed":[["gone",5],[7,1],"x",[],["blocked"],{"a":1},["adult",9]]}""",
        )
        assertEquals(b.closed.named(), listOf(NamedCount("adult", 9), NamedCount("gone", 5), NamedCount("blocked", 0)))
    }

    @Test
    fun `closed rows with no outcome count, as unknown`() {
        // The server sends `""` for closed rows whose last_outcome is NULL (probe_backfill.rs).
        val b = json.decodeFromString(
            ProbeBackfillStats.serializer(),
            """{"closed":[["adult",12],["",5],[3,1]],"probed":1,"remaining":1}""",
        )
        assertEquals(b.closed.named(), listOf(NamedCount("adult", 12), NamedCount("", 5)))
        val m = backfillLines(b, en).toMap()
        assertEquals(m["Closed"], "17")
        assertEquals(m["  unknown"], "5")
        assertEquals(outcomeLabel(""), "unknown")
        // The blank bucket and the dropped malformed entry survive the cache round trip as they were.
        val again = json.decodeFromString(ProbeBackfillStats.serializer(), json.encodeToString(ProbeBackfillStats.serializer(), b))
        assertEquals(again.closed.named(), b.closed.named())
    }

    @Test
    fun `progress is probed against what is left to try`() {
        val p = backfillProgress(ProbeBackfillStats(waiting = 10, leased = 5, retrying = 5, remaining = 20, probed = 60))
        assertEquals(p, BackfillProgress(60, 20))
        assertEquals(p.total, 80L)
        assertEquals(p.fraction, 0.75f)
        assertEquals(backfillPercent(p, en), "75% probed")
        // Nothing queued, nothing probed: no division by zero.
        assertEquals(backfillProgress(ProbeBackfillStats()).fraction, 0f)
        assertEquals(backfillPercent(backfillProgress(ProbeBackfillStats()), en), "nothing queued")
        // Never 100% while something is left, never 0% once something is probed.
        assertEquals(backfillPercent(BackfillProgress(9_999, 1), en), "99% probed")
        assertEquals(backfillPercent(BackfillProgress(1, 9_999), en), "<1% probed")
        assertEquals(backfillPercent(BackfillProgress(5, 0), en), "100% probed")
        // A `remaining` lower than its parts (counts taken a moment apart) uses the parts.
        assertEquals(backfillProgress(ProbeBackfillStats(waiting = 7, leased = 3, remaining = 0, probed = 10)).remaining, 10L)
    }

    @Test
    fun `backfill lines list the states and the closed outcomes`() {
        val f = json.decodeFromString(FleetStats.serializer(), newer)
        val lines = backfillLines(f.probeBackfill!!, en, nowSec = 1793000160)
        val m = lines.toMap()
        assertEquals(m["Probed"], "770 of 2,000")
        assertEquals(m["Remaining"], "1,230")
        assertEquals(m["Waiting / leased / retrying"], "1,000 / 30 / 200")
        assertEquals(m["Closed"], "415")
        assertEquals(m["  no peers"], "400")
        assertEquals(m["  adult"], "12")
        // Most first.
        assertEquals(lines.map { it.first }.filter { it.startsWith("  ") }, listOf("  no peers", "  adult", "  private"))
        assertTrue(m["Queue fill"]!!.startsWith("last pass"))
        assertEquals(backfillLines(ProbeBackfillStats(filling = true), en).toMap()["Queue fill"], "filling now")
        assertFalse(backfillLines(ProbeBackfillStats(), en).toMap().containsKey("Closed"))
        assertEquals(outcomeLabel("no_peers"), "no peers")
        assertEquals(outcomeLabel("v2_only"), "v2 only")
        assertEquals(outcomeLabel("some_new_code"), "some new code")
    }

    @Test
    fun `worker badges and backfill counters`() {
        val f = json.decodeFromString(FleetStats.serializer(), newer)
        val eee = f.workers.first { it.name == "eee" }
        assertEquals(workerBadges(eee), listOf("discovery off", "backfill on", "probes on"))
        val m = workerBackfillLines(eee, en).toMap()
        assertEquals(m["Leased"], "512")
        assertEquals(m["Probed / without a probe"], "40 / 300")
        assertEquals(m["In the worker now"], "30")
        assertEquals(m["Fetched / no peers / failed"], "60 / 250 / 20")
        assertEquals(m["Lease errors"], "1")
        // Discovering on (true) is the default, not a badge; server totals alone still show.
        val w = WorkerStats(name = "x", backfillLeasedTotal = 3, dht = WorkerDhtState(discover = true))
        assertTrue(workerBadges(w).isEmpty())
        assertEquals(workerBackfillLines(w, en).toMap()["Leased"], "3")
        assertFalse(workerBackfillLines(w, en).toMap().containsKey("In the worker now"))
        val on = WorkerStats(dht = WorkerDhtState(backfill = WorkerBackfillState(), probe = WorkerProbeState()))
        assertEquals(workerBadges(on), listOf("backfill on", "probes on"))
    }

    @Test
    fun `the probe series is offered only with backfill counts`() {
        assertFalse(Activity.offered(dht = true).any { it.metric == "probe_backfill_dht_rows" })
        assertTrue(Activity.offered(dht = false, probes = true).any { it.metric == "probe_backfill_dht_rows" })
    }

    private fun fleetFake(vararg extra: String) = FakeService().apply {
        features = listOf("filters", "stats", "graphs") + extra
        fleetAnswer = { json.decodeFromString(FleetStats.serializer(), newer) }
    }

    @Test
    fun `the stats screen asks for workers only when the server has them`() = runTest {
        main()
        val fake = fleetFake("workers")
        val vm = StatsViewModel(repo(fake))
        advanceUntilIdle()
        assertEquals(fake.fleetCalls, 1)
        assertTrue(vm.state.value.hasProbes)
        assertEquals(vm.state.value.fleet?.probeBackfill?.probed, 770L)

        // An older server: no request, no section.
        val old = fleetFake()
        val vm2 = StatsViewModel(repo(old))
        advanceUntilIdle()
        assertEquals(old.fleetCalls, 0)
        assertFalse(vm2.state.value.hasProbes)

        // Workers without the backfill (older server, or none configured): no section.
        val noBackfill = FakeService().apply {
            features = listOf("stats", "workers")
            fleetAnswer = { json.decodeFromString(FleetStats.serializer(), older) }
        }
        val vm3 = StatsViewModel(repo(noBackfill))
        advanceUntilIdle()
        assertNotNull(vm3.state.value.fleet)
        assertFalse(vm3.state.value.hasProbes)

        // A server error drops the section; it does not break the screen.
        val failing = FakeService().apply {
            features = listOf("stats", "workers")
            fleetError = ApiException.Http(500, "internal", "boom")
        }
        val vm4 = StatsViewModel(repo(failing))
        advanceUntilIdle()
        assertNull(vm4.state.value.fleet)
        assertFalse(vm4.state.value.hasProbes)
    }

    @Test
    fun `the probe series falls back to the total when the backfill counts go away`() = runTest {
        main()
        val fake = fleetFake("workers")
        val vm = StatsViewModel(repo(fake))
        advanceUntilIdle()
        vm.pickActivity(Activity.PROBE_DHT_ROWS)
        advanceUntilIdle()
        assertEquals(fake.timeseriesCalls.last().first, listOf("probe_backfill_dht_rows"))

        // The next refresh fails on /api/v1/stats: the chip is hidden, so the pick must go too.
        fake.fleetError = ApiException.Http(500, "internal", "boom")
        vm.refresh()
        advanceUntilIdle()
        assertFalse(vm.state.value.hasProbes)
        assertEquals(vm.state.value.activity, Activity.TOTAL)
        assertEquals(fake.timeseriesCalls.last().first, listOf(Activity.TOTAL.metric))

        // A server without workers at all: same.
        fake.fleetError = null
        vm.refresh()
        advanceUntilIdle()
        vm.pickActivity(Activity.PROBE_DHT_ROWS)
        advanceUntilIdle()
        fake.features = listOf("filters", "stats", "graphs")
        vm.refresh()
        advanceUntilIdle()
        assertEquals(vm.state.value.activity, Activity.TOTAL)
        assertEquals(fake.timeseriesCalls.last().first, listOf(Activity.TOTAL.metric))
    }
}

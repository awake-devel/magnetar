package app.magnetar.ui.stats

import app.magnetar.api.ProbeBackfillStats
import app.magnetar.api.WorkerStats
import app.magnetar.api.named
import app.magnetar.ui.common.formatAgo
import app.magnetar.ui.common.formatNumber
import app.magnetar.ui.common.formatPercent
import java.util.Locale

/*
 * The "Header probes" section of the Status tab (D117, D118, docs/PROBES.md §10): the server's
 * backfill queue and each worker's backfill counters. Counts only.
 */

/**
 * Backfill progress: torrents with a probe against those still to try. Closed rows (given up
 * for good) are in neither: they are listed by outcome instead.
 */
data class BackfillProgress(val probed: Long, val remaining: Long) {
    val total: Long get() = probed + remaining

    /** 0–1; 0 without anything to count. */
    val fraction: Float get() = if (total > 0) (probed.toDouble() / total).toFloat().coerceIn(0f, 1f) else 0f
}

fun backfillProgress(b: ProbeBackfillStats): BackfillProgress =
    BackfillProgress(b.probed.coerceAtLeast(0), maxOf(b.remaining, b.waiting + b.leased + b.retrying).coerceAtLeast(0))

/** "42% probed", "<1% probed", "100% probed", "nothing queued". */
fun backfillPercent(p: BackfillProgress, locale: Locale = Locale.getDefault()): String = when {
    p.total <= 0 -> "nothing queued"
    p.remaining == 0L -> formatPercent(1.0, locale) + " probed"
    p.probed > 0 && p.fraction < 0.005 -> "<" + formatPercent(0.01, locale) + " probed"
    // Never round up to 100% while something is left.
    else -> formatPercent(if (p.fraction >= 0.995f) 0.99 else p.fraction.toDouble(), locale) + " probed"
}

/** Label of a closed outcome (`no_peers` → "no peers"). */
fun outcomeLabel(outcome: String?): String = when (outcome) {
    // Closed with no recorded outcome: the server sends a blank name.
    null, "" -> "unknown"
    "no_peers" -> "no peers"
    "v2_only" -> "v2 only"
    "adult" -> "adult"
    "blocked" -> "blocked"
    "gone" -> "gone"
    "private" -> "private"
    "invalid" -> "invalid"
    "oversize" -> "too large"
    else -> outcome.trim().ifEmpty { "unknown" }.replace('_', ' ')
}

/** The backfill queue as label → value lines, under its progress bar. */
fun backfillLines(b: ProbeBackfillStats, locale: Locale = Locale.getDefault(), nowSec: Long = System.currentTimeMillis() / 1000): List<Pair<String, String>> =
    buildList {
        val p = backfillProgress(b)
        add("Probed" to "${formatNumber(p.probed, locale)} of ${formatNumber(p.total, locale)}")
        add("Remaining" to formatNumber(p.remaining, locale))
        add(
            "Waiting / leased / retrying" to
                "${formatNumber(b.waiting, locale)} / ${formatNumber(b.leased, locale)} / ${formatNumber(b.retrying, locale)}",
        )
        val closed = b.closed.named()
        if (closed.isNotEmpty()) {
            add("Closed" to formatNumber(closed.sumOf { it.count }, locale))
            for (c in closed) add("  " + outcomeLabel(c.name) to formatNumber(c.count, locale))
        }
        add(
            "Queue fill" to when {
                b.filling -> "filling now"
                b.filledAt != null -> "last pass " + formatAgo(b.filledAt, nowSec)
                else -> "not filled yet"
            },
        )
        if (b.at > 0) add("Counted" to formatAgo(b.at, nowSec))
    }

/** Badges of a worker card: what it is set to do. */
fun workerBadges(w: WorkerStats): List<String> = buildList {
    val d = w.dht ?: return@buildList
    // Older workers send no `discover` and always discover.
    if (d.discover == false) add("discovery off")
    if (d.backfill != null) add("backfill on")
    if (d.probe != null) add("probes on")
}

/**
 * A worker's backfill counters: the server's totals since it started, then what the worker's
 * last heartbeat said (since the worker started). Empty when it never took part.
 */
fun workerBackfillLines(w: WorkerStats, locale: Locale = Locale.getDefault()): List<Pair<String, String>> = buildList {
    val b = w.dht?.backfill
    if (w.backfillLeasedTotal == 0L && w.backfillProbedTotal == 0L && w.backfillUnprobedTotal == 0L && b == null) return@buildList
    add("Leased" to formatNumber(w.backfillLeasedTotal, locale))
    add(
        "Probed / without a probe" to
            "${formatNumber(w.backfillProbedTotal, locale)} / ${formatNumber(w.backfillUnprobedTotal, locale)}",
    )
    if (b != null) {
        add("In the worker now" to formatNumber(b.outstanding, locale))
        add("Fetched / no peers / failed" to "${formatNumber(b.fetched, locale)} / ${formatNumber(b.noPeers, locale)} / ${formatNumber(b.failed, locale)}")
        if (b.leaseErrors > 0) add("Lease errors" to formatNumber(b.leaseErrors, locale))
    }
}

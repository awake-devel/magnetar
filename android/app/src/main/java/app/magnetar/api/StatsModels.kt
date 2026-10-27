package app.magnetar.api

import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable

/*
 * Stats and graph answers (D47, docs/STATS.md), mirrors of magnetar_core::api. Same rules as
 * Models.kt: Rust field names, a default for every field, kinds and states as plain strings.
 */

/** `POST /api/v1/stats/breakdown` body: top values of [fields] over [query]'s results. */
@Serializable
data class BreakdownRequest(
    val query: SearchQuery = SearchQuery(),
    val fields: List<String> = emptyList(),
    /** Values per field, server default 10, at most 50. */
    val top: Int = 10,
)

/** Top values of one field; each field is counted with every filter of the query but its own. */
@Serializable
data class Breakdown(
    val field: String = "",
    val total: Long = 0,
    /** Most frequent first; multi-valued fields count a torrent once per value. */
    val counts: List<FacetCount> = emptyList(),
    /** Sum of the values after the top ones, and how many such values. */
    val other: Long = 0,
    @SerialName("other_values") val otherValues: Long = 0,
    /** Torrents without any value for this field. */
    val missing: Long = 0,
)

@Serializable
data class BreakdownSet(
    val total: Long = 0,
    val breakdowns: List<Breakdown> = emptyList(),
    @SerialName("computed_at") val computedAt: Long = 0,
) {
    fun field(name: String): Breakdown? = breakdowns.firstOrNull { it.field == name }
}

@Serializable
data class TimeseriesPoint(val at: Long = 0, val value: Long = 0)

@Serializable
data class Timeseries(
    val metric: String = "",
    /** `counter` (events per bucket) | `gauge` (last sample; buckets without one are left out). */
    val kind: String = "",
    /** `hour` | `day` (a metric kept per day only says `day` even when hours were asked). */
    val bucket: String = "",
    val points: List<TimeseriesPoint> = emptyList(),
)

/** `GET /api/v1/stats/timeseries`. */
@Serializable
data class TimeseriesSet(
    val since: Long = 0,
    val until: Long = 0,
    val series: List<Timeseries> = emptyList(),
)

@Serializable
data class BatchReport(
    val items: Long = 0,
    val hashes: Long = 0,
    val trackers: Long = 0,
    @SerialName("trackers_backed_off") val trackersBackedOff: Long = 0,
    @SerialName("tracker_requests") val trackerRequests: Long = 0,
    @SerialName("tracker_failures") val trackerFailures: Long = 0,
    @SerialName("tracker_answered") val trackerAnswered: Long = 0,
    @SerialName("dht_walks") val dhtWalks: Long = 0,
    @SerialName("dht_answered") val dhtAnswered: Long = 0,
    val alive: Long = 0,
    val dead: Long = 0,
    val unknown: Long = 0,
    val ms: Long = 0,
)

/** Seed checks on the server (D45). Counts are distinct infohashes. */
@Serializable
data class LivenessStats(
    val enabled: Boolean = false,
    val torrents: Long = 0,
    @SerialName("never_checked") val neverChecked: Long = 0,
    /** Due now, never checked included. */
    @SerialName("due_now") val dueNow: Long = 0,
    @SerialName("checked_24h") val checked24h: Long = 0,
    @SerialName("checks_per_hour") val checksPerHour: Long = 0,
    /** Latest result of the checked ones: seeded / answered with nobody seeding / no answer. */
    val alive: Long = 0,
    val dead: Long = 0,
    val unknown: Long = 0,
    val trackers: Long = 0,
    @SerialName("trackers_backed_off") val trackersBackedOff: Long = 0,
    @SerialName("dht_nodes") val dhtNodes: Long = 0,
    @SerialName("last_batch") val lastBatch: BatchReport? = null,
    @SerialName("last_batch_at") val lastBatchAt: Long? = null,
)

/** The server's search index (D43). */
@Serializable
data class SearchIndexStatus(
    /** `building` | `ready` | `error`. */
    val state: String = "",
    val docs: Long = 0,
    @SerialName("build_done") val buildDone: Long = 0,
    @SerialName("build_total") val buildTotal: Long = 0,
    @SerialName("build_secs") val buildSecs: Long? = null,
    @SerialName("last_commit") val lastCommit: Long? = null,
    val pending: Long = 0,
    @SerialName("lag_secs") val lagSecs: Long? = null,
    val bytes: Long = 0,
    val error: String? = null,
)

@Serializable
data class RelayHealth(
    val total: Long = 0,
    val enabled: Long = 0,
    @SerialName("ok_24h") val ok24h: Long = 0,
    val failing: Long = 0,
    val productive: Long = 0,
)

@Serializable
data class CrawlReport(
    @SerialName("relays_tried") val relaysTried: Int = 0,
    @SerialName("relays_ok") val relaysOk: Int = 0,
    @SerialName("events_seen") val eventsSeen: Long = 0,
    val inserted: Long = 0,
    val duplicates: Long = 0,
    val rejected: Long = 0,
    @SerialName("duration_ms") val durationMs: Long = 0,
    val errors: List<String> = emptyList(),
)

@Serializable
data class CrawlerStatus(
    val running: Boolean = false,
    @SerialName("last_run") val lastRun: Long? = null,
    @SerialName("last_report") val lastReport: CrawlReport? = null,
    val passes: Long = 0,
    @SerialName("last_error") val lastError: String? = null,
    @SerialName("next_run") val nextRun: Long? = null,
)

@Serializable
data class UpnpMapping(val protocol: String = "", val port: Int = 0, @SerialName("lease_secs") val leaseSecs: Long = 0)

@Serializable
data class UpnpStatus(
    /** `off` | `searching` | `mapped` | `partial` | `failed`. */
    val state: String = "off",
    val gateway: String? = null,
    val mappings: List<UpnpMapping> = emptyList(),
    @SerialName("renewed_at") val renewedAt: Long? = null,
    @SerialName("last_error") val lastError: String? = null,
)

/**
 * The server's DHT crawler (D58, D74). Rates are over the last ~10 minutes; `*_total` counts come
 * from the database. `blocked*` are counts only (the illegal-content blocklist, docs/DHT.md §7):
 * the app never asks for, or shows, anything those rules matched.
 */
@Serializable
data class DhtStats(
    val mode: String = "",
    val running: Boolean = false,
    @SerialName("uptime_secs") val uptimeSecs: Long = 0,
    @SerialName("virtual_nodes") val virtualNodes: Int = 0,
    val ports: List<Int> = emptyList(),
    /** `public` | `private` | `cgnat` | `unknown` (a class, never the address). */
    @SerialName("external_ip") val externalIp: String = "unknown",
    @SerialName("routing_nodes") val routingNodes: Long = 0,
    @SerialName("pool_nodes") val poolNodes: Long = 0,
    @SerialName("pool_bep51") val poolBep51: Long = 0,
    @SerialName("queries_per_sec") val queriesPerSec: Double = 0.0,
    @SerialName("answered_share") val answeredShare: Double = 0.0,
    val brake: Boolean = false,
    @SerialName("incoming_per_min") val incomingPerMin: Double = 0.0,
    @SerialName("hashes_per_min") val hashesPerMin: Double = 0.0,
    @SerialName("hashes_seen") val hashesSeen: Long = 0,
    @SerialName("queue_len") val queueLen: Long = 0,
    @SerialName("queue_lanes") val queueLanes: List<Long> = emptyList(),
    @SerialName("sampling_paused") val samplingPaused: Boolean = false,
    @SerialName("metadata_per_min") val metadataPerMin: Double = 0.0,
    @SerialName("fetch_success_rate") val fetchSuccessRate: Double = 0.0,
    @SerialName("fetch_ok") val fetchOk: Long = 0,
    @SerialName("fetch_failed") val fetchFailed: Long = 0,
    val stored: Long = 0,
    val blocked: Long = 0,
    @SerialName("stored_total") val storedTotal: Long = 0,
    @SerialName("adult_total") val adultTotal: Long = 0,
    @SerialName("blocked_total") val blockedTotal: Long = 0,
    @SerialName("seen_queued") val seenQueued: Long = 0,
    @SerialName("seen_failed") val seenFailed: Long = 0,
    val upnp: UpnpStatus? = null,
    @SerialName("last_error") val lastError: String? = null,
)

/** `GET /api/v1/stats/health` (D47). */
@Serializable
data class StatsHealth(
    val torrents: Long = 0,
    val liveness: LivenessStats? = null,
    @SerialName("search_index") val searchIndex: SearchIndexStatus? = null,
    val relays: RelayHealth = RelayHealth(),
    val crawler: CrawlerStatus = CrawlerStatus(),
    @SerialName("media_linked") val mediaLinked: Long = 0,
    @SerialName("attrs_missing") val attrsMissing: Long = 0,
    @SerialName("computed_at") val computedAt: Long = 0,
    val dht: DhtStats? = null,
)

/**
 * A graph node. `id` depends on `kind`: a pubkey (hex) for `root`/`follow`/`publisher`, a URL for
 * `relay`/`tracker`, an infohash for `torrent`/`release`, a media id for `media`, the value for
 * `quality`/`language`/`group`/`season`, `dht` for the DHT.
 */
@Serializable
data class GraphNode(
    val id: String = "",
    val kind: String = "",
    val label: String = "",
    val weight: Double = 0.0,
    /** Trust score 0–100 of a publisher, when known. */
    val score: Int? = null,
    val detail: List<String> = emptyList(),
)

@Serializable
data class GraphEdge(
    /** Indexes into [Graph.nodes]. */
    val source: Int = 0,
    val target: Int = 0,
    val weight: Double = 0.0,
    val kind: String = "",
)

/** `GET /api/v1/graphs/…`: at most 200 nodes. */
@Serializable
data class Graph(
    val kind: String = "",
    val nodes: List<GraphNode> = emptyList(),
    val edges: List<GraphEdge> = emptyList(),
    val truncated: Boolean = false,
    val note: String? = null,
)

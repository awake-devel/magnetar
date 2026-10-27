package app.magnetar.ui.stats

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Close
import androidx.compose.material.icons.filled.Refresh
import androidx.compose.material.icons.filled.Search
import androidx.compose.material3.AssistChip
import androidx.compose.material3.Card
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilterChip
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.InputChip
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedCard
import androidx.compose.material3.PrimaryTabRow
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Tab
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import app.magnetar.api.Breakdown
import app.magnetar.api.FacetCount
import app.magnetar.api.Features
import app.magnetar.api.StatsHealth
import app.magnetar.ui.common.AppIcons
import app.magnetar.ui.common.Banner
import app.magnetar.ui.common.CenteredMessage
import app.magnetar.ui.common.cachedAgo
import app.magnetar.ui.common.formatAgo
import app.magnetar.ui.common.formatNumber
import app.magnetar.ui.common.formatShare
import app.magnetar.ui.theme.ChartColors
import app.magnetar.ui.theme.chartColors

private enum class StatsTab(val label: String, val tag: String) {
    CHARTS("Charts", "stats_tab_charts"),
    GRAPHS("Graphs", "stats_tab_graphs"),
    STATUS("Status", "stats_tab_status"),
}

/**
 * The Stats tab (D47, docs/STATS.md): Charts (health, breakdowns with cross-filtering,
 * activity), Graphs (web of trust, relays; title and swarm graphs open from their pages) and
 * Status (server, index, seed-check backlog, DHT crawler).
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun StatsScreen(vm: StatsViewModel, onSearch: () -> Unit, onOpenGraph: (String) -> Unit, onOpenSettings: () -> Unit) {
    val state by vm.state.collectAsStateWithLifecycle()
    var tab by rememberSaveable { mutableIntStateOf(0) }
    val colors = chartColors()
    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text("Stats") },
                actions = { IconButton(onClick = vm::refresh, modifier = Modifier.testTag("stats_refresh")) { Icon(Icons.Filled.Refresh, "Refresh") } },
            )
        },
    ) { padding ->
        Column(Modifier.padding(padding).fillMaxSize()) {
            PrimaryTabRow(selectedTabIndex = tab) {
                for (t in StatsTab.entries) {
                    Tab(selected = tab == t.ordinal, onClick = { tab = t.ordinal }, text = { Text(t.label) }, modifier = Modifier.testTag(t.tag))
                }
            }
            when {
                !state.hasServer -> CenteredMessage("Stats are computed by your server. Set it in Settings.", "Settings", onAction = onOpenSettings)
                else -> when (StatsTab.entries[tab]) {
                    StatsTab.CHARTS -> Charts(state, colors, vm, onSearch)
                    StatsTab.GRAPHS -> Graphs(state, onOpenGraph)
                    StatsTab.STATUS -> Status(state, colors)
                }
            }
        }
    }
}

@Composable
private fun Charts(state: StatsUiState, colors: ChartColors, vm: StatsViewModel, onSearch: () -> Unit) {
    val set = state.breakdowns
    LazyColumn(Modifier.fillMaxSize().testTag("stats_charts")) {
        if (state.healthChecked && state.health != null && !state.hasStats) {
            item { Banner("This server is older than the app and has no charts (it lacks the stats feature).") }
            return@LazyColumn
        }
        // One first item for everything that appears late (progress, banners, health): items
        // inserted above the first visible one would be scrolled out of view (keyed list). It is
        // never empty (a zero-height first item does not anchor the scroll position).
        item(key = "top") {
            Column(Modifier.fillMaxWidth().heightIn(min = 1.dp)) {
                if (state.loadingBreakdowns) LinearProgressIndicator(Modifier.fillMaxWidth())
                val error = state.breakdownError
                if (error != null) {
                    Banner((if (set != null) "Offline: " else "") + error + (state.breakdownCachedAtMs?.let { " · " + cachedAgo(it) } ?: ""), error = true)
                } else {
                    state.breakdownCachedAtMs?.let { Banner(cachedAgo(it)) }
                }
                state.stats?.let { h -> HealthCard(h, colors) }
            }
        }
        item(key = "filters") { FilterRow(state, vm, onSearch) }
        if (set != null) {
            item(key = "total") {
                Text(
                    "${formatNumber(set.total)} torrents" + if (state.filters.activeCount > 0) " match" else "",
                    Modifier.padding(horizontal = 16.dp, vertical = 4.dp).testTag("stats_total"),
                    style = MaterialTheme.typography.titleMedium,
                )
            }
            items(set.breakdowns.filter { it.counts.isNotEmpty() || it.missing > 0 }, key = { it.field }) { b ->
                BreakdownCard(b, state, colors, vm, onSearch)
            }
            state.years?.takeIf { it.counts.isNotEmpty() }?.let { y -> item(key = "year") { BreakdownCard(y, state, colors, vm, onSearch) } }
        }
        item(key = "activity") { ActivityCard(state, colors, vm) }
        item { Text(" ", Modifier.padding(8.dp)) }
    }
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun FilterRow(state: StatsUiState, vm: StatsViewModel, onSearch: () -> Unit) {
    val chips = state.filters.statChips()
    Column(Modifier.padding(horizontal = 16.dp, vertical = 4.dp)) {
        if (chips.isEmpty()) {
            Text("Tap a bar or slice to filter every chart by it; long-press to search for it.", style = MaterialTheme.typography.bodySmall)
            return@Column
        }
        FlowRow(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
            for ((field, value) in chips) {
                InputChip(
                    selected = true,
                    onClick = { vm.toggle(field, value) },
                    label = { Text(valueLabel(field, value)) },
                    trailingIcon = { Icon(Icons.Filled.Close, "Remove ${valueLabel(field, value)}") },
                    modifier = Modifier.testTag("stats_chip_${field}_$value"),
                )
            }
        }
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp), verticalAlignment = Alignment.CenterVertically) {
            AssistChip(
                onClick = { vm.openSearch(); onSearch() },
                label = { Text("Search these") },
                leadingIcon = { Icon(Icons.Filled.Search, null) },
                modifier = Modifier.testTag("stats_search_these"),
            )
            TextButton(onClick = vm::clearFilters, modifier = Modifier.testTag("stats_clear")) { Text("Clear") }
        }
    }
}

@Composable
private fun BreakdownCard(b: Breakdown, state: StatsUiState, colors: ChartColors, vm: StatsViewModel, onSearch: () -> Unit) {
    val selected = state.filters.selectedStat(b.field)
    val toggle = { c: FacetCount -> vm.toggle(b.field, c.value) }
    val open = { c: FacetCount -> vm.openSearch(b.field, c.value); onSearch() }
    OutlinedCard(Modifier.fillMaxWidth().padding(horizontal = 12.dp, vertical = 6.dp).testTag("breakdown_${b.field}")) {
        Column(Modifier.padding(12.dp)) {
            Text(FIELD_TITLES[b.field] ?: b.field, style = MaterialTheme.typography.titleSmall, modifier = Modifier.semantics { heading() })
            Text(
                "of ${formatNumber(b.total)}" + if (b.field in MULTI_VALUED) " · a torrent can have several" else "",
                style = MaterialTheme.typography.labelSmall,
                color = colors.inkMuted,
                modifier = Modifier.padding(bottom = 6.dp),
            )
            when {
                b.field == "year" -> YearColumns(b, selected, colors, toggle, open)
                b.field in DONUT_ORDER -> BreakdownDonut(b, selected, colors, toggle, open)
                else -> BreakdownBars(b, selected, colors, toggle, open)
            }
        }
    }
}

@Composable
private fun HealthCard(h: StatsHealth, colors: ChartColors) {
    Card(Modifier.fillMaxWidth().padding(horizontal = 12.dp, vertical = 6.dp).testTag("health_card")) {
        Column(Modifier.padding(12.dp)) {
            Text("Health", style = MaterialTheme.typography.titleSmall, modifier = Modifier.semantics { heading() })
            h.liveness?.let { l ->
                val s = seedChecks(l)
                Text("Seed checks: latest result of the ${formatNumber(s.checked)} torrents checked", style = MaterialTheme.typography.labelSmall, color = colors.inkMuted)
                StatusBar(
                    listOf(
                        StatusPart("Seeded", s.seeded, colors.good),
                        StatusPart("Dead", s.dead, colors.critical),
                        StatusPart("No answer", s.noAnswer, colors.unknown),
                    ),
                    colors,
                    Modifier.padding(top = 4.dp),
                )
                Line(
                    "Not checked yet",
                    "${formatNumber(s.notChecked)} (${formatShare(s.notChecked, s.torrents)} of ${formatNumber(s.torrents)})",
                    tag = "health_not_checked",
                )
            }
            h.searchIndex?.let { s ->
                Line(
                    "Search index",
                    when (s.state) {
                        "ready" -> if (s.pending > 0) "${formatNumber(s.pending)} changes queued" else "up to date"
                        "building" -> "building (charts count with it once done)"
                        else -> s.state
                    },
                )
            }
            Line("Relays answering (24 h)", "${formatNumber(h.relays.ok24h)} of ${formatNumber(h.relays.enabled)}")
            if (h.relays.failing > 0) Line("Relays failing", formatNumber(h.relays.failing))
            Line("Linked to a title", "${formatNumber(h.mediaLinked)} of ${formatNumber(h.torrents)}")
            if (h.attrsMissing > 0) Line("Names still to parse", formatNumber(h.attrsMissing))
            Line(
                "Last crawl",
                when {
                    h.crawler.running -> "running"
                    h.crawler.lastRun != null -> formatAgo(h.crawler.lastRun)
                    else -> "not yet"
                } + (h.crawler.lastError?.let { " · error: $it" } ?: ""),
            )
            h.dht?.let { d ->
                Text("DHT crawler", Modifier.padding(top = 8.dp), style = MaterialTheme.typography.labelSmall, color = colors.inkMuted)
                for ((label, value) in dhtLines(d)) Line(label, value)
            }
        }
    }
}

@Composable
private fun Line(label: String, value: String, tag: String? = null) {
    Row(Modifier.fillMaxWidth().padding(top = 2.dp).then(if (tag != null) Modifier.testTag(tag) else Modifier)) {
        Text(label, Modifier.weight(1f), style = MaterialTheme.typography.bodySmall)
        Text(value, style = MaterialTheme.typography.bodySmall)
    }
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun ActivityCard(state: StatsUiState, colors: ChartColors, vm: StatsViewModel) {
    OutlinedCard(Modifier.fillMaxWidth().padding(horizontal = 12.dp, vertical = 6.dp).testTag("activity_card")) {
        Column(Modifier.padding(12.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text("Activity", Modifier.weight(1f).semantics { heading() }, style = MaterialTheme.typography.titleSmall)
                FilterChip(selected = !state.hourly, onClick = { vm.setHourly(false) }, label = { Text("Days") })
                FilterChip(
                    selected = state.hourly,
                    onClick = { vm.setHourly(true) },
                    label = { Text("Hours") },
                    modifier = Modifier.padding(start = 6.dp).testTag("activity_hours"),
                )
            }
            FlowRow(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                for (a in Activity.offered(state.hasDht)) {
                    FilterChip(
                        selected = state.activity == a,
                        onClick = { vm.pickActivity(a) },
                        label = { Text(a.label) },
                        modifier = Modifier.testTag("activity_${a.metric}"),
                    )
                }
            }
            val s = state.series
            when {
                state.seriesError != null -> Text(state.seriesError, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.error)
                s == null -> LinearProgressIndicator(Modifier.fillMaxWidth().padding(vertical = 24.dp))
                else -> {
                    if (s.bucket == "day" && state.hourly) {
                        Text("Kept per day only", style = MaterialTheme.typography.labelSmall, color = colors.inkMuted)
                    }
                    ActivityChart(s, state.activity.unit, colors, Modifier.padding(top = 8.dp))
                    Text(
                        if (s.kind == "counter") {
                            "${seriesValue(s.points.sumOf { it.value }, state.activity.unit)} in ${s.points.size} " +
                                "${if (s.bucket == "hour") "hours" else "days"} (UTC buckets) · pinch to zoom, tap for a value"
                        } else {
                            "pinch to zoom, tap for a value (UTC buckets)"
                        },
                        style = MaterialTheme.typography.labelSmall,
                        color = colors.inkMuted,
                    )
                }
            }
        }
    }
}

@Composable
private fun Graphs(state: StatsUiState, onOpen: (String) -> Unit) {
    LazyColumn(Modifier.fillMaxSize().padding(horizontal = 12.dp).testTag("stats_graphs")) {
        if (!state.hasGraphs) {
            item {
                if (!state.healthChecked) {
                    LinearProgressIndicator(Modifier.fillMaxWidth().padding(vertical = 12.dp))
                } else {
                    Banner(if (state.health == null) "The server did not answer." else "This server is older than the app and has no graphs.")
                }
            }
            return@LazyColumn
        }
        val cards = listOf(
            Triple("wot", "Web of trust", "You, the people you follow, and the publishers they vouch for, with each publisher's trust score and why."),
            Triple("relays", "Relays and publishers", "Which relays the most active publishers write to, sized by their torrents."),
        )
        items(cards, key = { it.first }) { (path, title, text) ->
            Card(onClick = { onOpen(path) }, modifier = Modifier.fillMaxWidth().padding(vertical = 6.dp).testTag("graph_card_$path")) {
                Row(Modifier.padding(16.dp), verticalAlignment = Alignment.CenterVertically) {
                    Icon(AppIcons.Graph, null, Modifier.padding(end = 12.dp))
                    Column {
                        Text(title, style = MaterialTheme.typography.titleMedium)
                        Text(text, style = MaterialTheme.typography.bodySmall)
                    }
                }
            }
        }
        item {
            Text(
                "A title's releases (by quality, language and group) open from its title page, a torrent's swarm " +
                    "sources from its detail page.",
                style = MaterialTheme.typography.bodySmall,
                modifier = Modifier.padding(vertical = 8.dp),
            )
        }
    }
}

@Composable
private fun Status(state: StatsUiState, colors: ChartColors) {
    val h = state.stats
    LazyColumn(Modifier.fillMaxSize().padding(horizontal = 12.dp).testTag("stats_status")) {
        item {
            StatusCard("Server", "status_server") {
                val health = state.health
                when {
                    !state.healthChecked -> LinearProgressIndicator(Modifier.fillMaxWidth())
                    health == null -> Text("Unreachable" + (state.healthError?.let { ": $it" } ?: ""), color = MaterialTheme.colorScheme.error)
                    else -> {
                        Line("State", if (health.status == "ok" && health.db) "healthy" else "${health.status}, database ${if (health.db) "ok" else "down"}")
                        Line("Version", health.version.ifBlank { "?" })
                        val missing = listOf(
                            Features.STATS to "charts", Features.GRAPHS to "graphs", Features.SAVED_SEARCH_POLL to "saved-search alerts",
                            Features.FILTERS to "release filters", Features.ORIGIN to "Nostr / DHT source",
                        ).filter { !health.has(it.first) }.map { it.second }
                        if (missing.isNotEmpty()) Line("Needs a newer server for", missing.joinToString(", "))
                    }
                }
            }
        }
        if (h == null) {
            item {
                val error = state.statsError
                if (error != null) {
                    Banner(error, error = true)
                } else if (state.hasStats) {
                    LinearProgressIndicator(Modifier.fillMaxWidth().padding(vertical = 12.dp))
                }
            }
            return@LazyColumn
        }
        state.statsCachedAtMs?.let { item { Banner(cachedAgo(it) + (state.statsError?.let { e -> " · $e" } ?: "")) } }
        h.searchIndex?.let { s -> item { StatusCard("Search index", "status_index") { for ((k, v) in indexLines(s)) Line(k, v) } } }
        h.liveness?.let { l ->
            item {
                StatusCard("Seed checks", "status_backlog") {
                    val s = seedChecks(l)
                    StatusBar(
                        listOf(
                            StatusPart("Seeded", s.seeded, colors.good),
                            StatusPart("Dead", s.dead, colors.critical),
                            StatusPart("No answer", s.noAnswer, colors.unknown),
                        ),
                        colors,
                    )
                    for ((k, v) in backlogLines(l)) Line(k, v)
                }
            }
        }
        item {
            StatusCard("Relays and crawler", "status_relays") {
                Line("Relays", "${formatNumber(h.relays.enabled)} enabled, ${formatNumber(h.relays.ok24h)} answered in 24 h")
                Line("Failing now", formatNumber(h.relays.failing))
                Line("Ever delivered a torrent", formatNumber(h.relays.productive))
                Line("Crawl passes", formatNumber(h.crawler.passes))
                h.crawler.lastReport?.let { r -> Line("Last pass", "${r.relaysOk}/${r.relaysTried} relays, ${formatNumber(r.inserted)} new events") }
            }
        }
        h.dht?.let { d -> item { StatusCard("DHT crawler", "status_dht") { for ((k, v) in dhtLines(d)) Line(k, v) } } }
        item { Text(" ", Modifier.padding(8.dp)) }
    }
}

@Composable
private fun StatusCard(title: String, tag: String, content: @Composable () -> Unit) {
    Card(Modifier.fillMaxWidth().padding(vertical = 6.dp).testTag(tag)) {
        Column(Modifier.padding(12.dp)) {
            Text(title, style = MaterialTheme.typography.titleSmall, modifier = Modifier.semantics { heading() })
            content()
        }
    }
}

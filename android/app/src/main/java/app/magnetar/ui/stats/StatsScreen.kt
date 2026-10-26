package app.magnetar.ui.stats

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
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
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.Tab
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import app.magnetar.backlogEta
import app.magnetar.dhtLines
import app.magnetar.ffi.FfiBreakdown
import app.magnetar.ffi.FfiStatsHealth
import app.magnetar.formatAgo
import app.magnetar.formatNumber
import app.magnetar.ui.charts.ActivityChart
import app.magnetar.ui.charts.BreakdownBars
import app.magnetar.ui.charts.BreakdownDonut
import app.magnetar.ui.charts.DONUT_ORDER
import app.magnetar.ui.charts.StatusBar
import app.magnetar.ui.charts.YearColumns
import app.magnetar.ui.common.Banner
import app.magnetar.ui.status.StatusContent
import app.magnetar.ui.status.StatusViewModel
import app.magnetar.ui.theme.ChartColors
import app.magnetar.ui.theme.chartColors

private enum class StatsTab(val label: String) { CHARTS("Charts"), GRAPHS("Graphs"), STATUS("Status") }

/**
 * The Stats tab (D47), which replaced the Status tab so the bottom bar keeps five entries:
 * charts of what the index holds (cross-filtering), activity over time and health; the
 * network graphs; and the former Status screen (local cache, server, seed checks, relays).
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun StatsScreen(
    onSearch: () -> Unit,
    onOpenGraph: (String) -> Unit,
    vm: StatsViewModel = viewModel(),
    statusVm: StatusViewModel = viewModel(),
) {
    val state by vm.state.collectAsStateWithLifecycle()
    var tab by rememberSaveable { mutableIntStateOf(0) }
    val snackbar = remember { SnackbarHostState() }
    LaunchedEffect(statusVm) { statusVm.messages.collect { snackbar.showSnackbar(it) } }
    LaunchedEffect(vm) { vm.messages.collect { snackbar.showSnackbar(it) } }
    val colors = chartColors()

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text("Stats") },
                actions = {
                    IconButton(onClick = { if (tab == StatsTab.STATUS.ordinal) statusVm.refresh() else vm.refresh() }) {
                        Icon(Icons.Filled.Refresh, "Refresh")
                    }
                },
            )
        },
        snackbarHost = { SnackbarHost(snackbar) },
    ) { padding ->
        Column(Modifier.padding(padding).fillMaxSize()) {
            PrimaryTabRow(selectedTabIndex = tab) {
                for (t in StatsTab.entries) {
                    Tab(selected = tab == t.ordinal, onClick = { tab = t.ordinal }, text = { Text(t.label) })
                }
            }
            when (StatsTab.entries[tab]) {
                StatsTab.CHARTS -> Charts(state, colors, vm, onSearch)
                StatsTab.GRAPHS -> Graphs(state, onOpenGraph)
                StatsTab.STATUS -> StatusContent(statusVm)
            }
        }
    }
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun Charts(state: StatsUiState, colors: ChartColors, vm: StatsViewModel, onSearch: () -> Unit) {
    val set = state.breakdowns
    val local = set?.source == "local"
    LazyColumn(Modifier.fillMaxSize()) {
        when {
            !state.hasServer -> item {
                Banner("No server: counts of the torrents cached on this phone (no filters, fewer charts).")
            }
            !state.featuresChecked -> {}
            state.features == null -> item { Banner("The server did not answer: showing the phone's cache.") }
            !state.stats -> item { Banner("This server is older than the app and has no charts: showing the phone's cache.") }
            local -> item {
                Banner("The server could not count (${set.serverError ?: "unknown error"}): showing the phone's cache.")
            }
        }
        if (state.loadingBreakdowns) item { LinearProgressIndicator(Modifier.fillMaxWidth()) }
        state.breakdownError?.let { item { Banner(it, error = true) } }
        state.health?.let { h -> item { HealthCard(h, colors) } }
        if (state.filters.activeCount > 0 || !local) {
            item { FilterRow(state, vm, onSearch) }
        }
        if (set != null) {
            item {
                Text(
                    "${formatNumber(set.total.toLong())} torrents" +
                        if (state.filters.activeCount > 0) " match" else "",
                    Modifier.padding(horizontal = 16.dp, vertical = 4.dp),
                    style = MaterialTheme.typography.titleMedium,
                )
            }
            items(set.breakdowns.filter { it.counts.isNotEmpty() || it.missing > 0uL }, key = { it.field }) { b ->
                BreakdownCard(b, state, colors, vm, onSearch, interactive = !local)
            }
            state.years?.takeIf { it.counts.isNotEmpty() && !local }?.let { y ->
                item(key = "year") { BreakdownCard(y, state, colors, vm, onSearch, interactive = true) }
            }
        }
        if (state.stats) item { ActivityCard(state, colors, vm) }
        item { Text(" ", Modifier.padding(8.dp)) }
    }
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun FilterRow(state: StatsUiState, vm: StatsViewModel, onSearch: () -> Unit) {
    val f = state.filters
    val chips = buildList {
        f.categories.forEach { add("category" to it.name.lowercase()) }
        for (field in listOf("language", "resolution", "source", "codec", "hdr", "audio", "genre", "media_kind")) {
            f.selected(field).forEach { add(field to it) }
        }
        f.selectedStat("year").forEach { add("year" to it) }
    }
    Column(Modifier.padding(horizontal = 16.dp, vertical = 4.dp)) {
        if (chips.isEmpty()) {
            Text(
                "Tap a bar or slice to filter every chart by it; long-press to search for it.",
                style = MaterialTheme.typography.bodySmall,
            )
        } else {
            FlowRow(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                for ((field, value) in chips) {
                    InputChip(
                        selected = true,
                        onClick = { vm.toggle(field, value) },
                        label = { Text(app.magnetar.ffi.facetLabel(if (field == "subcategory") "category_path" else field, value)) },
                        trailingIcon = { Icon(Icons.Filled.Close, "Remove", Modifier.padding(0.dp)) },
                    )
                }
            }
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                AssistChip(
                    onClick = { vm.openSearch(); onSearch() },
                    label = { Text("Search these") },
                    leadingIcon = { Icon(Icons.Filled.Search, null) },
                )
                TextButton(onClick = vm::clearFilters) { Text("Clear") }
            }
        }
    }
}

@Composable
private fun BreakdownCard(
    b: FfiBreakdown,
    state: StatsUiState,
    colors: ChartColors,
    vm: StatsViewModel,
    onSearch: () -> Unit,
    interactive: Boolean,
) {
    val selected = state.filters.selectedStat(b.field)
    val toggle = { c: app.magnetar.ffi.FfiFacetCount -> if (interactive) vm.toggle(b.field, c.value) }
    val open = { c: app.magnetar.ffi.FfiFacetCount -> vm.openSearch(b.field, c.label.takeIf { b.field == "subcategory" } ?: c.value); onSearch() }
    OutlinedCard(Modifier.fillMaxWidth().padding(horizontal = 12.dp, vertical = 6.dp)) {
        Column(Modifier.padding(12.dp)) {
            Text(FIELD_TITLES[b.field] ?: b.field, style = MaterialTheme.typography.titleSmall)
            Text(
                "of ${formatNumber(b.total.toLong())}" + if (b.field in setOf("language", "hdr", "audio", "genre")) {
                    " · a torrent can have several"
                } else {
                    ""
                },
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
private fun HealthCard(h: FfiStatsHealth, colors: ChartColors) {
    Card(Modifier.fillMaxWidth().padding(horizontal = 12.dp, vertical = 6.dp)) {
        Column(Modifier.padding(12.dp)) {
            Text("Health", style = MaterialTheme.typography.titleSmall)
            h.liveness?.let { l ->
                Text("Seed checks, latest result per torrent", style = MaterialTheme.typography.labelSmall, color = colors.inkMuted)
                StatusBar(
                    listOf(
                        Triple("Seeded", l.alive, colors.good),
                        Triple("Dead", l.dead, colors.critical),
                        Triple("No answer", l.unknown, colors.unknown),
                        Triple("Not checked yet", l.neverChecked, colors.grid),
                    ),
                    colors,
                )
                Line("Due now", formatNumber(l.dueNow.toLong()) + (backlogEta(l.dueNow, l.checksPerHour)?.let { " (cleared $it)" } ?: ""))
            }
            h.indexState?.let { s ->
                Line(
                    "Search index",
                    when (s) {
                        "ready" -> if (h.indexPending > 0uL) "${formatNumber(h.indexPending.toLong())} changes queued" else "up to date"
                        "building" -> "building (charts appear when it is done)"
                        else -> s
                    },
                )
            }
            Line("Relays answering (24 h)", "${formatNumber(h.relaysOk24h.toLong())} of ${formatNumber(h.relaysEnabled.toLong())}")
            if (h.relaysFailing > 0uL) Line("Relays failing", formatNumber(h.relaysFailing.toLong()))
            Line("Linked to a title", "${formatNumber(h.mediaLinked.toLong())} of ${formatNumber(h.torrents.toLong())}")
            if (h.attrsMissing > 0uL) Line("Names still to parse", formatNumber(h.attrsMissing.toLong()))
            Line(
                "Last crawl",
                when {
                    h.crawlerRunning -> "running"
                    h.crawlerLastRun != null -> formatAgo(h.crawlerLastRun!!)
                    else -> "not yet"
                } + (h.crawlerLastError?.let { " · error: $it" } ?: ""),
            )
            // The server's DHT crawler (D58): discovery rate, queue, metadata success.
            h.dht?.let { d ->
                Text(
                    "DHT crawler",
                    Modifier.padding(top = 8.dp),
                    style = MaterialTheme.typography.labelSmall,
                    color = colors.inkMuted,
                )
                for ((label, value) in dhtLines(d)) Line(label, value)
            }
        }
    }
}

@Composable
private fun Line(label: String, value: String) {
    Row(Modifier.fillMaxWidth().padding(top = 2.dp)) {
        Text(label, Modifier.weight(1f), style = MaterialTheme.typography.bodySmall)
        Text(value, style = MaterialTheme.typography.bodySmall)
    }
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun ActivityCard(state: StatsUiState, colors: ChartColors, vm: StatsViewModel) {
    OutlinedCard(Modifier.fillMaxWidth().padding(horizontal = 12.dp, vertical = 6.dp)) {
        Column(Modifier.padding(12.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text("Activity", Modifier.weight(1f), style = MaterialTheme.typography.titleSmall)
                FilterChip(selected = !state.hourly, onClick = { vm.setHourly(false) }, label = { Text("Days") })
                FilterChip(
                    selected = state.hourly,
                    onClick = { vm.setHourly(true) },
                    label = { Text("Hours") },
                    modifier = Modifier.padding(start = 6.dp),
                )
            }
            FlowRow(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                for (a in Activity.entries.filter { !it.dht || state.dht }) {
                    FilterChip(selected = state.activity == a, onClick = { vm.pickActivity(a) }, label = { Text(a.label) })
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
                    val sum = s.points.sumOf { it.value }
                    Text(
                        if (s.kind == "counter") {
                            "${formatNumber(sum)} in ${s.points.size} ${if (s.bucket == "hour") "hours" else "days"} (UTC buckets)"
                        } else {
                            "pinch to zoom, press for a value"
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
    LazyColumn(Modifier.fillMaxSize().padding(horizontal = 12.dp)) {
        if (!state.graphs) {
            if (state.hasServer && !state.featuresChecked) {
                item { LinearProgressIndicator(Modifier.fillMaxWidth().padding(vertical = 12.dp)) }
                return@LazyColumn
            }
            item {
                Banner(
                    when {
                        !state.hasServer -> "Graphs are drawn from the server's data. Set a server in Settings to see them."
                        state.features == null -> "The server did not answer."
                        else -> "This server is older than the app and has no graphs."
                    },
                )
            }
            return@LazyColumn
        }
        val cards = listOf(
            Triple("wot", "Web of trust", "You, the people you follow, and the publishers they vouch for, with each publisher's trust score and why."),
            Triple("relays", "Relays and publishers", "Which relays the most active publishers write to, sized by their torrents."),
        )
        items(cards, key = { it.first }) { (path, title, text) ->
            Card(onClick = { onOpen(path) }, modifier = Modifier.fillMaxWidth().padding(vertical = 6.dp)) {
                Column(Modifier.padding(16.dp)) {
                    Text(title, style = MaterialTheme.typography.titleMedium)
                    Text(text, style = MaterialTheme.typography.bodySmall)
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

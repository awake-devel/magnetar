package app.magnetar.ui.search

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Clear
import androidx.compose.material.icons.filled.KeyboardArrowDown
import androidx.compose.material.icons.filled.KeyboardArrowUp
import androidx.compose.material.icons.filled.Search
import androidx.compose.material.icons.filled.Star
import androidx.compose.material3.BadgedBox
import androidx.compose.material3.Badge
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilterChip
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.ListItem
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.RadioButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Slider
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.derivedStateOf
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.runtime.snapshotFlow
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import app.magnetar.ffi.FfiCategory
import app.magnetar.ffi.FfiSort
import app.magnetar.ffi.FfiTorrentSummary
import app.magnetar.formatAge
import app.magnetar.formatBytes
import app.magnetar.ui.common.AppIcons
import app.magnetar.ui.common.Banner
import app.magnetar.ui.common.CategoryIcon
import app.magnetar.ui.common.CenteredMessage
import app.magnetar.ui.common.SaveSearchDialog
import app.magnetar.ui.common.TrustBadge
import app.magnetar.ui.common.categoryLabel
import kotlin.math.roundToInt

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun SearchScreen(onOpen: (String) -> Unit, vm: SearchViewModel = viewModel()) {
    val state by vm.state.collectAsStateWithLifecycle()
    val settings by vm.settings.collectAsStateWithLifecycle()
    val snackbar = remember { SnackbarHostState() }
    var showFilters by remember { mutableStateOf(false) }
    var showSort by remember { mutableStateOf(false) }
    var showSave by remember { mutableStateOf(false) }

    LaunchedEffect(vm) { vm.messages.collect { snackbar.showSnackbar(it) } }

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text("Magnetar") },
                actions = {
                    IconButton(onClick = { showSave = true }) { Icon(Icons.Filled.Star, "Save search") }
                    Box {
                        IconButton(onClick = { showSort = true }) { Icon(AppIcons.Sort, "Sort") }
                        SortMenu(
                            expanded = showSort,
                            sort = state.sort,
                            descending = state.descending,
                            onDismiss = { showSort = false },
                            onSort = { vm.setSort(it); showSort = false },
                            onToggleDirection = { vm.toggleDirection(); showSort = false },
                        )
                    }
                    IconButton(onClick = { showFilters = true }) {
                        BadgedBox(badge = {
                            if (state.filters.activeCount > 0) Badge { Text("${state.filters.activeCount}") }
                        }) { Icon(AppIcons.FilterList, "Filters") }
                    }
                },
            )
        },
        snackbarHost = { SnackbarHost(snackbar) },
    ) { padding ->
        Column(Modifier.padding(padding).fillMaxSize()) {
            OutlinedTextField(
                value = state.text,
                onValueChange = vm::onTextChange,
                modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 4.dp),
                placeholder = { Text("Search torrents") },
                leadingIcon = { Icon(Icons.Filled.Search, null) },
                trailingIcon = {
                    if (state.text.isNotEmpty()) {
                        IconButton(onClick = { vm.onTextChange("") }) { Icon(Icons.Filled.Clear, "Clear") }
                    }
                },
                singleLine = true,
                keyboardOptions = KeyboardOptions(imeAction = ImeAction.Search),
                keyboardActions = KeyboardActions(onSearch = { vm.submit() }),
            )
            if (state.loading) LinearProgressIndicator(Modifier.fillMaxWidth()) else Spacer(Modifier.height(4.dp))
            if (state.source == "local" && state.serverError != null) {
                Banner("Offline results (server unreachable: ${state.serverError})")
            }
            when {
                state.error != null && state.hits.isEmpty() ->
                    CenteredMessage(state.error!!, "Retry", vm::submit)
                state.hits.isEmpty() -> if (!state.loading) {
                    CenteredMessage(if (state.text.isBlank()) "Nothing cached yet. Crawl from the Status tab." else "No results")
                }
                else -> ResultList(state, settings.trustThreshold, onOpen, vm::loadMore)
            }
        }
    }

    if (showFilters) {
        FilterSheet(
            initial = state.filters,
            defaultTrust = settings.trustThreshold,
            onDismiss = { showFilters = false },
            onApply = { vm.setFilters(it); showFilters = false },
        )
    }
    if (showSave) {
        SaveSearchDialog(
            initialName = state.text.ifBlank { "All torrents" },
            onDismiss = { showSave = false },
            onSave = { name, notify -> vm.saveSearch(name, notify); showSave = false },
        )
    }
}

@Composable
private fun ResultList(state: SearchUiState, threshold: Int, onOpen: (String) -> Unit, loadMore: () -> Unit) {
    val listState = rememberLazyListState()
    val nearEnd by remember {
        derivedStateOf {
            val last = listState.layoutInfo.visibleItemsInfo.lastOrNull()?.index ?: 0
            last >= listState.layoutInfo.totalItemsCount - 10
        }
    }
    LaunchedEffect(listState) {
        snapshotFlow { nearEnd }.collect { if (it) loadMore() }
    }
    LazyColumn(state = listState, modifier = Modifier.fillMaxSize()) {
        item {
            Text(
                "${state.total} results" + if (state.source != null) " · ${state.source}" else "",
                style = MaterialTheme.typography.labelMedium,
                modifier = Modifier.padding(horizontal = 16.dp, vertical = 4.dp),
            )
        }
        items(state.hits, key = { it.infohash }) { hit ->
            ResultRow(hit, threshold, Modifier.clickable { onOpen(hit.infohash) })
            HorizontalDivider()
        }
        if (state.loadingMore) {
            item {
                Box(Modifier.fillMaxWidth().padding(16.dp), contentAlignment = Alignment.Center) {
                    CircularProgressIndicator()
                }
            }
        }
    }
}

fun peersText(seeders: UInt?, leechers: UInt?): String =
    if (seeders == null && leechers == null) "peers unknown" else "${seeders ?: "?"} S / ${leechers ?: "?"} L"

@Composable
private fun ResultRow(hit: FfiTorrentSummary, threshold: Int, modifier: Modifier) {
    ListItem(
        modifier = modifier,
        leadingContent = { CategoryIcon(hit.category) },
        headlineContent = { Text(hit.title, maxLines = 2, overflow = TextOverflow.Ellipsis) },
        supportingContent = {
            val dead = if (hit.dead == true) " · dead" else ""
            val pinned = if (hit.pinned) "📌 " else ""
            Text("$pinned${formatBytes(hit.totalSize)} · ${peersText(hit.seeders, hit.leechers)}$dead · ${formatAge(hit.createdAt)}")
        },
        trailingContent = { TrustBadge(hit.trust.toInt(), threshold) },
    )
}

@Composable
private fun SortMenu(
    expanded: Boolean,
    sort: FfiSort,
    descending: Boolean,
    onDismiss: () -> Unit,
    onSort: (FfiSort) -> Unit,
    onToggleDirection: () -> Unit,
) {
    DropdownMenu(expanded = expanded, onDismissRequest = onDismiss) {
        for (s in FfiSort.entries) {
            DropdownMenuItem(
                text = { Text(s.name.lowercase().replaceFirstChar { it.uppercase() }) },
                leadingIcon = { RadioButton(selected = s == sort, onClick = null) },
                onClick = { onSort(s) },
            )
        }
        HorizontalDivider()
        DropdownMenuItem(
            text = { Text(if (descending) "Descending" else "Ascending") },
            leadingIcon = {
                Icon(if (descending) Icons.Filled.KeyboardArrowDown else Icons.Filled.KeyboardArrowUp, null)
            },
            onClick = onToggleDirection,
        )
    }
}

@OptIn(ExperimentalMaterial3Api::class, ExperimentalLayoutApi::class)
@Composable
private fun FilterSheet(
    initial: SearchFilters,
    defaultTrust: Int,
    onDismiss: () -> Unit,
    onApply: (SearchFilters) -> Unit,
) {
    var f by remember { mutableStateOf(initial) }
    var minSize by remember { mutableStateOf(initial.minSizeMb?.toString() ?: "") }
    var maxSize by remember { mutableStateOf(initial.maxSizeMb?.toString() ?: "") }
    var minSeeders by remember { mutableStateOf(initial.minSeeders?.toString() ?: "") }
    ModalBottomSheet(onDismissRequest = onDismiss, sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true)) {
        Column(Modifier.padding(horizontal = 16.dp).verticalScroll(rememberScrollState())) {
            Text("Categories", style = MaterialTheme.typography.titleSmall)
            FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                for (c in FfiCategory.entries) {
                    FilterChip(
                        selected = c in f.categories,
                        onClick = { f = f.copy(categories = if (c in f.categories) f.categories - c else f.categories + c) },
                        label = { Text(categoryLabel(c)) },
                    )
                }
            }
            Spacer(Modifier.height(8.dp))
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                NumberField("Min size (MiB)", minSize, { minSize = it }, Modifier.weight(1f))
                NumberField("Max size (MiB)", maxSize, { maxSize = it }, Modifier.weight(1f))
            }
            Spacer(Modifier.height(8.dp))
            Text("Published", style = MaterialTheme.typography.titleSmall)
            FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                for (p in DatePreset.entries) {
                    FilterChip(selected = f.date == p, onClick = { f = f.copy(date = p) }, label = { Text(p.label) })
                }
            }
            Spacer(Modifier.height(8.dp))
            NumberField("Min seeders", minSeeders, { minSeeders = it }, Modifier.fillMaxWidth())
            Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
                Text("Hide dead torrents", Modifier.weight(1f))
                Switch(checked = f.hideDead, onCheckedChange = { f = f.copy(hideDead = it) })
            }
            val trust = f.minTrust ?: defaultTrust
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(
                    "Min trust: $trust" + if (f.minTrust == null) " (default)" else "",
                    Modifier.weight(1f),
                    style = MaterialTheme.typography.titleSmall,
                )
                if (f.minTrust != null) TextButton(onClick = { f = f.copy(minTrust = null) }) { Text("Reset") }
            }
            Slider(
                value = trust.toFloat(),
                onValueChange = { f = f.copy(minTrust = it.roundToInt()) },
                valueRange = 0f..100f,
            )
            Row(Modifier.fillMaxWidth().padding(vertical = 16.dp), horizontalArrangement = Arrangement.End) {
                OutlinedButton(onClick = {
                    f = SearchFilters(); minSize = ""; maxSize = ""; minSeeders = ""
                }) { Text("Clear all") }
                Spacer(Modifier.width(8.dp))
                Button(onClick = {
                    onApply(
                        f.copy(
                            minSizeMb = minSize.toLongOrNull(),
                            maxSizeMb = maxSize.toLongOrNull(),
                            minSeeders = minSeeders.toIntOrNull(),
                        ),
                    )
                }) { Text("Apply") }
            }
        }
    }
}

@Composable
private fun NumberField(label: String, value: String, onChange: (String) -> Unit, modifier: Modifier) {
    OutlinedTextField(
        value = value,
        onValueChange = { v -> onChange(v.filter { it.isDigit() }.take(9)) },
        label = { Text(label) },
        singleLine = true,
        keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Number),
        modifier = modifier,
    )
}

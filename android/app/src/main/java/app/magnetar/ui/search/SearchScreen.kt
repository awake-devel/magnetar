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
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
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
import app.magnetar.fileName
import app.magnetar.formatAge
import app.magnetar.formatBytes
import app.magnetar.formatCount
import app.magnetar.ui.common.AppIcons
import app.magnetar.ui.common.Banner
import app.magnetar.ui.common.CategoryIcon
import app.magnetar.ui.common.CenteredMessage
import app.magnetar.ui.common.SaveSearchDialog
import app.magnetar.ui.common.SourceBadge
import app.magnetar.ui.common.categoryLabel
import app.magnetar.ui.detail.mediaHeadline
import kotlin.math.roundToInt

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun SearchScreen(onOpen: (String) -> Unit, onOpenMedia: (Long) -> Unit = {}, vm: SearchViewModel = viewModel()) {
    val state by vm.state.collectAsStateWithLifecycle()
    val facets by vm.facets.collectAsStateWithLifecycle()
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
                            withRating = state.filterMode == FilterMode.FULL,
                            sort = state.sort,
                            descending = state.descending,
                            onDismiss = { showSort = false },
                            onSort = { vm.setSort(it); showSort = false },
                            onToggleDirection = { vm.toggleDirection(); showSort = false },
                        )
                    }
                    IconButton(onClick = { showFilters = true; vm.draftFilters(state.filters) }) {
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
                placeholder = { Text("Search torrents or imdb:tt…") },
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
                    // "Nothing cached" only fits an unfiltered local browse; an empty server page
                    // (e.g. trust threshold above every score) is just "no results".
                    val emptyCache = state.text.isBlank() && state.source == "local" && state.filters.activeCount == 0
                    CenteredMessage(
                        if (emptyCache) {
                            "Nothing cached yet. Set your server in Settings."
                        } else {
                            "No results. Filters and the trust threshold (${settings.trustThreshold}) apply."
                        },
                    )
                }
                else -> ResultList(state, settings.trustThreshold, onOpen, onOpenMedia, vm::loadMore)
            }
        }
    }

    if (showFilters) {
        FilterSheet(
            initial = state.filters,
            defaultTrust = settings.trustThreshold,
            mode = state.filterMode,
            facets = facets,
            onDraft = vm::draftFilters,
            onApply = { vm.setFilters(it); showFilters = false; vm.draftFilters(null) },
            sourceChips = state.originFilter,
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
private fun ResultList(
    state: SearchUiState,
    threshold: Int,
    onOpen: (String) -> Unit,
    onOpenMedia: (Long) -> Unit,
    loadMore: () -> Unit,
) {
    val listState = rememberLazyListState()
    // A new search (text, filters, sort, saved search, settings) starts at the top, not at the
    // scroll position of the previous result list (which also kept triggering loadMore).
    var shownGeneration by rememberSaveable { mutableIntStateOf(state.generation) }
    LaunchedEffect(state.generation) {
        if (state.generation != shownGeneration) {
            listState.scrollToItem(0)
            shownGeneration = state.generation
        }
    }
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
                "${formatCount(state.total, state.totalIsLowerBound)} results" +
                    (if (state.source != null) " · ${state.source}" else "") +
                    (if (state.loading && state.source == "local") " · asking the server…" else ""),
                style = MaterialTheme.typography.labelMedium,
                modifier = Modifier.padding(horizontal = 16.dp, vertical = 4.dp),
            )
        }
        items(state.hits, key = { it.infohash }) { hit ->
            ResultRow(hit, threshold, Modifier.clickable { onOpen(hit.infohash) }, onOpenMedia)
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
private fun ResultRow(hit: FfiTorrentSummary, threshold: Int, modifier: Modifier, onOpenMedia: (Long) -> Unit) {
    ListItem(
        modifier = modifier,
        leadingContent = { CategoryIcon(hit.category) },
        overlineContent = hit.mediaTitle?.let { t ->
            {
                // The linked title opens its page: every release of it, grouped (D44).
                val id = hit.mediaId
                val rating = hit.mediaRating?.let { " · ★ %.1f".format(it) } ?: ""
                Text(
                    mediaHeadline(t, hit.mediaYear) + rating + if (id != null) "  ›" else "",
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                    color = if (id != null) MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = if (id != null) Modifier.clickable { onOpenMedia(id) } else Modifier,
                )
            }
        },
        headlineContent = { Text(hit.title, maxLines = 2, overflow = TextOverflow.Ellipsis) },
        supportingContent = {
            val dead = if (hit.dead == true) " · dead" else ""
            val pinned = if (hit.pinned) "📌 " else ""
            Column {
                Text("$pinned${formatBytes(hit.totalSize)} · ${peersText(hit.seeders, hit.leechers)}$dead · ${formatAge(hit.createdAt)}")
                hit.attrs?.badges?.takeIf { it.isNotEmpty() }?.let { b ->
                    Text(
                        b.joinToString(" · "),
                        style = MaterialTheme.typography.labelSmall,
                        color = MaterialTheme.colorScheme.secondary,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                }
                hit.matchedFile?.let { f ->
                    Text(
                        "matched in file: ${fileName(f)}",
                        style = MaterialTheme.typography.labelSmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                }
            }
        },
        trailingContent = { SourceBadge(hit.origin, hit.trust.toInt(), threshold) },
    )
}

@Composable
private fun SortMenu(
    expanded: Boolean,
    withRating: Boolean,
    sort: FfiSort,
    descending: Boolean,
    onDismiss: () -> Unit,
    onSort: (FfiSort) -> Unit,
    onToggleDirection: () -> Unit,
) {
    DropdownMenu(expanded = expanded, onDismissRequest = onDismiss) {
        // Rating needs a server with D44 filters (the phone only knows ratings of opened details).
        for (s in FfiSort.entries.filter { withRating || it != FfiSort.RATING || sort == FfiSort.RATING }) {
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

@Composable
fun NumberField(label: String, value: String, onChange: (String) -> Unit, modifier: Modifier) {
    OutlinedTextField(
        value = value,
        onValueChange = { v -> onChange(v.filter { it.isDigit() }.take(9)) },
        label = { Text(label) },
        singleLine = true,
        keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Number),
        modifier = modifier,
    )
}

package app.magnetar.ui.search

import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Clear
import androidx.compose.material.icons.filled.KeyboardArrowDown
import androidx.compose.material.icons.filled.KeyboardArrowUp
import androidx.compose.material.icons.filled.Search
import androidx.compose.material.icons.filled.Star
import androidx.compose.material3.Badge
import androidx.compose.material3.BadgedBox
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.RadioButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
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
import androidx.compose.ui.platform.LocalSoftwareKeyboardController
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import app.magnetar.api.Sort
import app.magnetar.ui.common.AppIcons
import app.magnetar.ui.common.Banner
import app.magnetar.ui.common.CenteredMessage
import app.magnetar.ui.common.ResultRow
import app.magnetar.ui.common.cachedAgo
import app.magnetar.ui.common.resultsText
import app.magnetar.ui.saved.SaveSearchDialog

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun SearchScreen(
    vm: SearchViewModel,
    threshold: Int,
    onOpen: (String) -> Unit,
    onOpenMedia: (Long) -> Unit,
    onOpenSettings: () -> Unit,
) {
    val state by vm.state.collectAsStateWithLifecycle()
    val facets by vm.facets.collectAsStateWithLifecycle()
    val snackbar = remember { SnackbarHostState() }
    var showFilters by rememberSaveable { mutableStateOf(false) }
    var showSort by remember { mutableStateOf(false) }
    var showSave by remember { mutableStateOf(false) }
    val keyboard = LocalSoftwareKeyboardController.current

    LaunchedEffect(vm) { vm.messages.collect { snackbar.showSnackbar(it) } }
    // Reopened after a configuration change: count again.
    LaunchedEffect(showFilters) { if (showFilters) vm.draftFilters(vm.state.value.filters) }

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text("Magnetar") },
                actions = {
                    IconButton(onClick = { showSave = true }, modifier = Modifier.testTag("save_search")) {
                        Icon(Icons.Filled.Star, "Save search")
                    }
                    Box {
                        IconButton(onClick = { showSort = true }, modifier = Modifier.testTag("sort")) { Icon(AppIcons.Sort, "Sort") }
                        SortMenu(
                            expanded = showSort,
                            withRating = state.canFilterReleases,
                            sort = state.sort,
                            descending = state.descending,
                            onDismiss = { showSort = false },
                            onSort = { vm.setSort(it); showSort = false },
                            onToggleDirection = { vm.toggleDirection(); showSort = false },
                        )
                    }
                    IconButton(onClick = { showFilters = true }, modifier = Modifier.testTag("filters")) {
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
                modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 4.dp).testTag("search_box"),
                placeholder = { Text("Search torrents or imdb:tt…") },
                leadingIcon = { Icon(Icons.Filled.Search, null) },
                trailingIcon = {
                    if (state.text.isNotEmpty()) {
                        IconButton(onClick = { vm.onTextChange("") }) { Icon(Icons.Filled.Clear, "Clear search") }
                    }
                },
                singleLine = true,
                keyboardOptions = KeyboardOptions(imeAction = ImeAction.Search),
                keyboardActions = KeyboardActions(onSearch = { keyboard?.hide(); vm.submit() }),
            )
            if (state.loading) LinearProgressIndicator(Modifier.fillMaxWidth()) else Spacer(Modifier.height(4.dp))
            if (state.offline && state.hits.isNotEmpty()) {
                Banner("Offline: ${state.error}. Showing the cached page.", error = true)
            }
            when {
                state.notConfigured -> CenteredMessage("Set your server in Settings", "Settings", onAction = onOpenSettings)
                state.error != null && state.hits.isEmpty() -> CenteredMessage(state.error!!, "Retry", onAction = vm::submit)
                state.hits.isEmpty() -> if (!state.loading) {
                    CenteredMessage(
                        if (state.filters.activeCount > 0) {
                            "No results. Filters and the trust threshold apply."
                        } else {
                            "No results. The trust threshold applies."
                        },
                    )
                }
                else -> ResultList(state, threshold, onOpen, onOpenMedia, vm::loadMore)
            }
        }
    }

    if (showFilters) {
        FilterSheet(
            initial = state.filters,
            defaultTrust = threshold,
            state = state,
            facets = facets,
            onDraft = vm::draftFilters,
            onApply = {
                showFilters = false
                vm.draftFilters(null)
                vm.setFilters(it)
            },
        )
    }
    if (showSave) {
        SaveSearchDialog(
            initialName = state.text.trim().ifBlank { "All torrents" },
            filterCount = state.filters.activeCount,
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
    // A new search starts at the top, not at the previous list's scroll position.
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
    LaunchedEffect(listState) { snapshotFlow { nearEnd }.collect { if (it) loadMore() } }
    LazyColumn(state = listState, modifier = Modifier.fillMaxSize().testTag("results")) {
        item {
            Text(
                resultsText(state.total, state.totalIsLowerBound) +
                    (state.cachedAtMs?.let { " · " + cachedAgo(it) } ?: ""),
                style = MaterialTheme.typography.labelMedium,
                modifier = Modifier.padding(horizontal = 16.dp, vertical = 4.dp).testTag("result_count"),
            )
        }
        items(state.hits, key = { it.infohash }) { hit ->
            ResultRow(hit, threshold, onOpen, onOpenMedia)
            HorizontalDivider()
        }
        if (state.loadingMore) {
            item {
                Box(Modifier.fillMaxWidth().padding(16.dp), contentAlignment = Alignment.Center) { CircularProgressIndicator() }
            }
        }
    }
}

val SORT_LABELS = mapOf(
    Sort.RELEVANCE to "Relevance",
    Sort.DATE to "Date",
    Sort.SIZE to "Size",
    Sort.SEEDERS to "Seeders",
    Sort.RATING to "Rating",
)

@Composable
private fun SortMenu(
    expanded: Boolean,
    withRating: Boolean,
    sort: Sort,
    descending: Boolean,
    onDismiss: () -> Unit,
    onSort: (Sort) -> Unit,
    onToggleDirection: () -> Unit,
) {
    DropdownMenu(expanded = expanded, onDismissRequest = onDismiss) {
        // Rating needs a server with the release filters (D44).
        for (s in Sort.entries.filter { withRating || it != Sort.RATING }) {
            DropdownMenuItem(
                text = { Text(SORT_LABELS.getValue(s)) },
                leadingIcon = { RadioButton(selected = s == sort, onClick = null) },
                onClick = { onSort(s) },
                modifier = Modifier.testTag("sort_${s.name.lowercase()}"),
            )
        }
        HorizontalDivider()
        DropdownMenuItem(
            text = { Text(if (descending) "Descending" else "Ascending") },
            leadingIcon = { Icon(if (descending) Icons.Filled.KeyboardArrowDown else Icons.Filled.KeyboardArrowUp, null) },
            onClick = onToggleDirection,
        )
    }
}

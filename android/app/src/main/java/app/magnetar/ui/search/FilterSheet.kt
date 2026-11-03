package app.magnetar.ui.search

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Close
import androidx.compose.material3.Button
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.InputChip
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Slider
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.testTagsAsResourceId
import androidx.compose.ui.unit.dp
import app.magnetar.api.FacetCounts
import app.magnetar.api.FacetSet
import app.magnetar.api.FileKindCatalogue
import app.magnetar.ui.common.NumberField
import app.magnetar.ui.common.categoryLabel
import app.magnetar.ui.common.formatCount
import java.util.Locale
import kotlin.math.roundToInt

/**
 * The filter sheet (docs/APP.md §12, D140): one dropdown per filter group, with live
 * disjunctive counts from the server next to every value, values that stay where they first
 * appeared (a value that would give nothing stays in place, disabled), the active filters as
 * removable chips at the top, and Back / swipe-down apply like the Show button.
 */
@OptIn(ExperimentalMaterial3Api::class, ExperimentalLayoutApi::class, androidx.compose.ui.ExperimentalComposeUiApi::class)
@Composable
fun FilterSheet(
    initial: SearchFilters,
    defaultTrust: Int,
    state: SearchUiState,
    facets: FacetSet?,
    onDraft: (SearchFilters) -> Unit,
    onApply: (SearchFilters) -> Unit,
    fileKinds: FileKindCatalogue? = null,
) {
    var f by remember { mutableStateOf(initial) }
    var minSize by remember { mutableStateOf(initial.minSizeMb?.toString() ?: "") }
    var maxSize by remember { mutableStateOf(initial.maxSizeMb?.toString() ?: "") }
    var minSeeders by remember { mutableStateOf(initial.minSeeders?.toString() ?: "") }
    var minYear by remember { mutableStateOf(initial.minYear?.toString() ?: "") }
    var maxYear by remember { mutableStateOf(initial.maxYear?.toString() ?: "") }

    /** The one open dropdown (its field), null = all closed. */
    var open by remember { mutableStateOf<String?>(null) }
    fun current() = f.copy(
        minSizeMb = minSize.toLongOrNull(),
        maxSizeMb = maxSize.toLongOrNull(),
        minSeeders = minSeeders.toIntOrNull(),
        minYear = minYear.toIntOrNull()?.takeIf { minYear.length == 4 },
        maxYear = maxYear.toIntOrNull()?.takeIf { maxYear.length == 4 },
    )
    // Live counts follow the draft (the view model debounces).
    LaunchedEffect(f, minSize, maxSize, minSeeders, minYear, maxYear) { onDraft(current()) }
    val order = remember { ChipOrder() }

    fun expandedChange(field: String): (Boolean) -> Unit = { want ->
        if (want) open = field else if (open == field) open = null
    }

    @Composable
    fun Dropdown(field: String, chips: List<FacetChip>, title: String = FACET_TITLES[field] ?: field) {
        FilterDropdown(
            field = field,
            title = title,
            options = order.arrange(field, chips),
            selected = f.selected(field),
            expanded = open == field,
            onExpandedChange = expandedChange(field),
            onToggle = { v -> f = f.toggle(field, v) },
            onClear = { f = f.clear(field) },
            // With counts, values nothing would match are disabled. The adult category stays
            // enabled: while the switch is off its count is 0, and picking it is how the server
            // shows adult torrents (D52, docs/APP.md §6).
            enabled = { chip -> chip.count != 0L || (field == "category" && chip.value == "porn") },
            modifier = Modifier.padding(top = 4.dp),
        )
    }

    ModalBottomSheet(
        // Swiping the sheet away or pressing Back keeps the picks, like the Show button.
        onDismissRequest = { onApply(current()) },
        sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true),
        modifier = Modifier.testTag("filter_sheet"),
    ) {
        Column(Modifier.semantics { testTagsAsResourceId = true }.padding(horizontal = 16.dp).verticalScroll(rememberScrollState())) {
            ActiveRow(f.activeFilters(fileKinds), onRemove = { f = f.without(it) }, onClearAll = {
                f = SearchFilters(); minSize = ""; maxSize = ""; minSeeders = ""; minYear = ""; maxYear = ""
            }, anyActive = current().activeCount > 0)
            val categoryCounts: FacetCounts? = facets?.field("category")
            Dropdown(
                "category",
                CATEGORIES.map { c ->
                    FacetChip(c, categoryLabel(c), categoryCounts?.let { counts -> counts.counts.firstOrNull { it.value == c }?.count ?: 0L })
                },
                title = "Category",
            )
            // Source (D51): Nostr publishers or the server's DHT crawler; a torrent known from
            // both counts under each. Offered when the server knows sources, kept while set.
            if (state.canFilterOrigin || f.origins.isNotEmpty()) {
                val originCounts = facets?.field("origin")
                Dropdown(
                    "origin",
                    ORIGIN_CHIPS.map { (v, label) ->
                        FacetChip(v, label, originCounts?.let { counts -> counts.counts.firstOrNull { it.value == v }?.count ?: 0L })
                    },
                    title = "Source",
                )
            }
            for (field in FACET_FIELDS) {
                val selected = f.selected(field)
                // A filter the server cannot apply stays visible while set, so it can be cleared.
                if (!state.canFilterReleases && selected.isEmpty()) continue
                val chips = facetChips(field, selected, facets?.field(field))
                if (chips.isEmpty()) continue
                Dropdown(field, chips)
            }
            FilterDropdown(
                field = "date",
                title = "Published",
                options = DatePreset.entries.map { FacetChip(it.name.lowercase(), it.label, null) },
                selected = if (f.date == DatePreset.ANY) emptySet() else setOf(f.date.name.lowercase()),
                expanded = open == "date",
                onExpandedChange = expandedChange("date"),
                onToggle = { v -> f = f.copy(date = DatePreset.entries.first { it.name.lowercase() == v }) },
                onClear = { f = f.copy(date = DatePreset.ANY) },
                single = true,
                anyValue = DatePreset.ANY.name.lowercase(),
                modifier = Modifier.padding(top = 4.dp),
            )
            // Header probes (D122): read from the files by a crawl worker, so only probed
            // torrents match. Same dropdowns and counts as above, under their own heading.
            if (state.canFilterProbes || f.hasProbeFilters) {
                val showFileKinds = state.canFilterFileKinds || f.probeFileKinds.isNotEmpty()
                val probeChips = PROBE_FACET_FIELDS.mapNotNull { field ->
                    val selected = f.selected(field)
                    if (!state.canFilterProbes && selected.isEmpty()) return@mapNotNull null
                    // Track kinds (D125) need `probe_kinds`; kept while set, so they can be cleared.
                    if (field in PROBE_KIND_FIELDS && !state.canFilterProbeKinds && selected.isEmpty()) return@mapNotNull null
                    facetChips(field, selected, facets?.field(field)).takeIf { it.isNotEmpty() }?.let { field to it }
                }
                if (probeChips.isNotEmpty() || showFileKinds) {
                    Spacer(Modifier.height(12.dp))
                    Text(
                        "From the files (probed torrents only)",
                        style = MaterialTheme.typography.titleMedium,
                        modifier = Modifier.semantics { heading() },
                    )
                    if (probeChips.any { (field, _) -> field == "probe_audio_kind" || field == "probe_sub_kind" }) {
                        Text(KIND_PAIR_HINT, style = MaterialTheme.typography.bodySmall)
                    }
                    // File kinds (D137): every group and label sniff knows, from every probed file.
                    if (showFileKinds) {
                        val counts = if (state.canFilterFileKinds) facets?.field(FILE_KIND_FIELD) else null
                        val labelsOf = fileKindLabelsOf(fileKinds, counts)
                        FileKindDropdown(
                            selected = f.probeFileKinds,
                            catalogue = if (state.canFilterFileKinds) fileKinds else null,
                            counts = counts,
                            order = order,
                            expanded = open == FILE_KIND_FIELD,
                            onExpandedChange = expandedChange(FILE_KIND_FIELD),
                            onToggle = { v -> f = f.toggleFileKind(v, labelsOf) },
                            onClear = { f = f.clear(FILE_KIND_FIELD) },
                            note = if (state.canFilterFileKinds) null else "Needs a newer server",
                            modifier = Modifier.padding(top = 4.dp),
                        )
                    }
                    for ((field, chips) in probeChips) Dropdown(field, chips)
                }
            }
            if (state.canFilterReleases || f.minRating != null) {
                Spacer(Modifier.height(8.dp))
                val rating = f.minRating ?: 0f
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Text(
                        if (f.minRating == null) "Min rating: any" else String.format(Locale.ROOT, "Min rating: %.1f", rating),
                        Modifier.weight(1f),
                        style = MaterialTheme.typography.titleSmall,
                    )
                    if (f.minRating != null) TextButton(onClick = { f = f.copy(minRating = null) }) { Text("Reset") }
                }
                Slider(
                    value = rating,
                    onValueChange = { v -> f = f.copy(minRating = ((v * 2).roundToInt() / 2f).takeIf { it > 0f }) },
                    valueRange = 0f..9.5f,
                    steps = 18,
                    modifier = Modifier.testTag("rating_slider"),
                )
            }
            if (state.canFilterReleases || minYear.isNotEmpty() || maxYear.isNotEmpty()) {
                Title("Year")
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    NumberField("From", minYear, { minYear = it }, Modifier.weight(1f), maxDigits = 4)
                    NumberField("To", maxYear, { maxYear = it }, Modifier.weight(1f), maxDigits = 4)
                }
            }
            Title("Size")
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                NumberField("Min (MiB)", minSize, { minSize = it }, Modifier.weight(1f))
                NumberField("Max (MiB)", maxSize, { maxSize = it }, Modifier.weight(1f))
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
                    "Min trust: $trust" + if (f.minTrust == null) " (server threshold)" else "",
                    Modifier.weight(1f),
                    style = MaterialTheme.typography.titleSmall,
                )
                if (f.minTrust != null) TextButton(onClick = { f = f.copy(minTrust = null) }) { Text("Reset") }
            }
            Slider(
                value = trust.toFloat(),
                onValueChange = { f = f.copy(minTrust = it.roundToInt()) },
                valueRange = 0f..100f,
                modifier = Modifier.testTag("trust_slider"),
            )
            Row(Modifier.fillMaxWidth().padding(vertical = 16.dp), horizontalArrangement = Arrangement.End) {
                OutlinedButton(
                    onClick = { f = SearchFilters(); minSize = ""; maxSize = ""; minSeeders = ""; minYear = ""; maxYear = "" },
                    modifier = Modifier.testTag("filter_clear_all_footer"),
                ) { Text("Clear all") }
                Spacer(Modifier.width(8.dp))
                Button(onClick = { onApply(current()) }, modifier = Modifier.testTag("filters_apply")) {
                    Text(facets?.let { "Show ${formatCount(it.total)}" } ?: "Apply")
                }
            }
        }
    }
}

@Composable
private fun Title(text: String) {
    Spacer(Modifier.height(8.dp))
    Text(text, style = MaterialTheme.typography.titleSmall, modifier = Modifier.semantics { heading() })
}

/**
 * The active filters at the top of the sheet: one removable chip per selected value ("French ×")
 * in one wrapped row, and "Clear all" (D140). Nothing active: a one-line hint.
 */
@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun ActiveRow(active: List<ActiveFilter>, onRemove: (ActiveFilter) -> Unit, onClearAll: () -> Unit, anyActive: Boolean) {
    Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
        Text(
            if (anyActive) "Active filters" else "No filters",
            Modifier.weight(1f).semantics { heading() },
            style = MaterialTheme.typography.titleSmall,
        )
        TextButton(onClick = onClearAll, enabled = anyActive, modifier = Modifier.testTag("filter_clear_all")) { Text("Clear all") }
    }
    if (active.isEmpty()) return
    FlowRow(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
        for (a in active) {
            InputChip(
                selected = true,
                onClick = { onRemove(a) },
                label = { Text(a.label) },
                trailingIcon = { Icon(Icons.Filled.Close, "Remove ${a.label}") },
                modifier = Modifier.testTag("filter_active_${a.field}_${a.value}"),
            )
        }
    }
}

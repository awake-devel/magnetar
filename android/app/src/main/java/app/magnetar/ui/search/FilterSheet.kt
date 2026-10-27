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
import androidx.compose.material3.Button
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilterChip
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
import androidx.compose.ui.unit.dp
import app.magnetar.api.FacetCounts
import app.magnetar.api.FacetSet
import app.magnetar.ui.common.NumberField
import app.magnetar.ui.common.categoryLabel
import app.magnetar.ui.common.formatCount
import java.util.Locale
import kotlin.math.roundToInt

private fun chipText(label: String, count: Long?) = label + (count?.let { " (${formatCount(it)})" } ?: "")

/**
 * The filter sheet (docs/APP.md §1): live disjunctive counts from the server next to every chip,
 * chips that stay where they first appeared (a value that would give nothing stays in place,
 * disabled), and Back / swipe-down apply like the Show button.
 */
@OptIn(ExperimentalMaterial3Api::class, ExperimentalLayoutApi::class)
@Composable
fun FilterSheet(
    initial: SearchFilters,
    defaultTrust: Int,
    state: SearchUiState,
    facets: FacetSet?,
    onDraft: (SearchFilters) -> Unit,
    onApply: (SearchFilters) -> Unit,
) {
    var f by remember { mutableStateOf(initial) }
    var minSize by remember { mutableStateOf(initial.minSizeMb?.toString() ?: "") }
    var maxSize by remember { mutableStateOf(initial.maxSizeMb?.toString() ?: "") }
    var minSeeders by remember { mutableStateOf(initial.minSeeders?.toString() ?: "") }
    var minYear by remember { mutableStateOf(initial.minYear?.toString() ?: "") }
    var maxYear by remember { mutableStateOf(initial.maxYear?.toString() ?: "") }
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

    @Composable
    fun ChipRow(field: String, chips: List<FacetChip>) {
        FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            for (chip in order.arrange(field, chips)) {
                val on = chip.value in f.selected(field)
                FilterChip(
                    selected = on,
                    onClick = { f = f.toggle(field, chip.value) },
                    label = { Text(chipText(chip.label, chip.count)) },
                    // With counts, values nothing would match stay in place but disabled. The
                    // adult category stays enabled: while the switch is off its count is 0, and
                    // picking it is how the server shows adult torrents (D52, docs/APP.md §6).
                    enabled = on || chip.count != 0L || (field == "category" && chip.value == "porn"),
                    modifier = Modifier.testTag("chip_${field}_${chip.value}"),
                )
            }
        }
    }

    @Composable
    fun Title(text: String) {
        Spacer(Modifier.height(8.dp))
        Text(text, style = MaterialTheme.typography.titleSmall, modifier = Modifier.semantics { heading() })
    }

    ModalBottomSheet(
        // Swiping the sheet away or pressing Back keeps the picks, like the Show button.
        onDismissRequest = { onApply(current()) },
        sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true),
        modifier = Modifier.testTag("filter_sheet"),
    ) {
        Column(Modifier.padding(horizontal = 16.dp).verticalScroll(rememberScrollState())) {
            Title("Categories")
            val categoryCounts: FacetCounts? = facets?.field("category")
            ChipRow(
                "category",
                CATEGORIES.map { c ->
                    FacetChip(c, categoryLabel(c), categoryCounts?.let { counts -> counts.counts.firstOrNull { it.value == c }?.count ?: 0L })
                },
            )
            // Source (D51): Nostr publishers or the server's DHT crawler; a torrent known from
            // both counts under each. Offered when the server knows sources, kept while set.
            if (state.canFilterOrigin || f.origins.isNotEmpty()) {
                Title("Source")
                val originCounts = facets?.field("origin")
                ChipRow(
                    "origin",
                    ORIGIN_CHIPS.map { (v, label) ->
                        FacetChip(v, label, originCounts?.let { counts -> counts.counts.firstOrNull { it.value == v }?.count ?: 0L })
                    },
                )
            }
            for (field in FACET_FIELDS) {
                val selected = f.selected(field)
                // A filter the server cannot apply stays visible while set, so it can be cleared.
                if (!state.canFilterReleases && selected.isEmpty()) continue
                val chips = facetChips(field, selected, facets?.field(field))
                if (chips.isEmpty()) continue
                Title(FACET_TITLES[field] ?: field)
                ChipRow(field, chips)
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
            Title("Published")
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
                OutlinedButton(onClick = {
                    f = SearchFilters(); minSize = ""; maxSize = ""; minSeeders = ""; minYear = ""; maxYear = ""
                }) { Text("Clear all") }
                Spacer(Modifier.width(8.dp))
                Button(onClick = { onApply(current()) }, modifier = Modifier.testTag("filters_apply")) {
                    Text(facets?.let { "Show ${formatCount(it.total)}" } ?: "Apply")
                }
            }
        }
    }
}

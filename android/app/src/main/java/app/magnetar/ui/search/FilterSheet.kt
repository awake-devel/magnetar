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
import androidx.compose.ui.unit.dp
import app.magnetar.ffi.FfiCategory
import app.magnetar.ffi.FfiFacetSet
import app.magnetar.ffi.facetLabel
import app.magnetar.ffi.filterOptions
import app.magnetar.formatCount
import app.magnetar.ui.common.categoryLabel
import kotlin.math.roundToInt

/** Section titles of the release/media facets (D44). */
private val FACET_TITLES = mapOf(
    "language" to "Audio language",
    "subtitles" to "Subtitles",
    "resolution" to "Resolution",
    "source" to "Source",
    "codec" to "Video codec",
    "hdr" to "HDR",
    "audio" to "Audio",
    "genre" to "Genre",
    "media_kind" to "Type",
)

/** Facets the phone cache cannot test well: hidden offline unless a saved search set them. */
private val SERVER_ONLY = setOf("genre", "media_kind")

/** One chip: value, label, count (null = unknown). */
data class FacetChip(val value: String, val label: String, val count: ULong?)

/**
 * Chips of one facet: with server counts, the counted values (most frequent first) plus any
 * selected value the counts lack; without, the built-in list (`filter_options`).
 */
fun facetChips(field: String, selected: Set<String>, facets: FfiFacetSet?): List<FacetChip> {
    val counted = facets?.facets?.firstOrNull { it.field == field }
    val chips = counted?.counts?.map { FacetChip(it.value, it.label, it.count) }
        ?: filterOptions(field).map { FacetChip(it, facetLabel(field, it), null) }
    val missing = selected.filter { v -> chips.none { it.value == v } }
        .map { FacetChip(it, facetLabel(field, it), if (counted != null) 0uL else null) }
    return chips + missing
}

@OptIn(ExperimentalMaterial3Api::class, ExperimentalLayoutApi::class)
@Composable
fun FilterSheet(
    initial: SearchFilters,
    defaultTrust: Int,
    mode: FilterMode,
    facets: FfiFacetSet?,
    onDraft: (SearchFilters) -> Unit,
    onDismiss: () -> Unit,
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
        minYear = minYear.toIntOrNull(),
        maxYear = maxYear.toIntOrNull(),
    )
    // Live counts follow the draft (the view model debounces).
    LaunchedEffect(f, minSize, maxSize, minSeeders, minYear, maxYear) { onDraft(current()) }

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
            for (field in FACET_FIELDS) {
                val selected = f.selected(field)
                val supported = when (mode) {
                    FilterMode.FULL -> true
                    FilterMode.LOCAL -> field !in SERVER_ONLY
                    FilterMode.BASIC -> false
                }
                // A filter the current server cannot apply stays visible while set, so it can be cleared.
                if (!supported && selected.isEmpty()) continue
                val chips = facetChips(field, selected, facets)
                if (chips.isEmpty()) continue
                Spacer(Modifier.height(8.dp))
                Text(FACET_TITLES[field] ?: field, style = MaterialTheme.typography.titleSmall)
                FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    for (chip in chips) {
                        val on = chip.value in selected
                        // With counts, values nothing would match are left out unless selected.
                        if (!on && chip.count == 0uL) continue
                        FilterChip(
                            selected = on,
                            onClick = { f = f.toggle(field, chip.value) },
                            label = { Text(chip.label + (chip.count?.let { " (${formatCount(it)})" } ?: "")) },
                        )
                    }
                }
            }
            if (mode == FilterMode.FULL || f.minRating != null) {
                Spacer(Modifier.height(8.dp))
                val rating = f.minRating ?: 0f
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Text(
                        if (f.minRating == null) "Min rating: any" else "Min rating: %.1f".format(rating),
                        Modifier.weight(1f),
                        style = MaterialTheme.typography.titleSmall,
                    )
                    if (f.minRating != null) TextButton(onClick = { f = f.copy(minRating = null) }) { Text("Reset") }
                }
                Slider(
                    value = rating,
                    onValueChange = { v ->
                        val r = (v * 2).roundToInt() / 2f
                        f = f.copy(minRating = r.takeIf { it > 0f })
                    },
                    valueRange = 0f..9.5f,
                    steps = 18,
                )
            }
            if (mode != FilterMode.BASIC || minYear.isNotEmpty() || maxYear.isNotEmpty()) {
                Spacer(Modifier.height(8.dp))
                Text("Year", style = MaterialTheme.typography.titleSmall)
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    NumberField("From", minYear, { minYear = it.take(4) }, Modifier.weight(1f))
                    NumberField("To", maxYear, { maxYear = it.take(4) }, Modifier.weight(1f))
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
                    f = SearchFilters(); minSize = ""; maxSize = ""; minSeeders = ""; minYear = ""; maxYear = ""
                }) { Text("Clear all") }
                Spacer(Modifier.width(8.dp))
                Button(onClick = { onApply(current()) }) {
                    Text(facets?.let { "Show ${formatCount(it.total)}" } ?: "Apply")
                }
            }
        }
    }
}

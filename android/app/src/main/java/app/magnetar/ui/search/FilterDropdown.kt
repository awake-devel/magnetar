package app.magnetar.ui.search

import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Clear
import androidx.compose.material.icons.filled.KeyboardArrowDown
import androidx.compose.material.icons.filled.KeyboardArrowUp
import androidx.compose.material3.Checkbox
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.ExposedDropdownMenuAnchorType
import androidx.compose.material3.ExposedDropdownMenuBox
import androidx.compose.material3.ExposedDropdownMenuBoxScope
import androidx.compose.material3.ExposedDropdownMenuDefaults
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.RadioButton
import androidx.compose.material3.Text
import androidx.compose.material3.TriStateCheckbox
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateMapOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.role
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.state.ToggleableState
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import app.magnetar.api.FacetCounts
import app.magnetar.api.FileKindCatalogue
import app.magnetar.ui.common.formatCount
import app.magnetar.ui.common.formatNumber

/*
 * The filter sheet's dropdowns (D140, docs/APP.md §12): one Material 3 exposed dropdown per
 * filter group. The anchor shows the title and the selection ("French, +2"), a clear × and the
 * arrow; the menu lists the values with a checkbox (a radio for Published) and the live count.
 * Picking a value does not close the menu; an outside tap or Back closes it and keeps the picks.
 */

/** Height of an open menu: it scrolls inside, the sheet stays put. */
private val MENU_MAX_HEIGHT = 360.dp

/**
 * One filter group as a dropdown. [options] are in display order ([ChipOrder]); [enabled] is the
 * sheet's rule (a value nothing would match is disabled unless selected). More than
 * [DROPDOWN_SEARCH_MIN] options make the anchor editable: typing filters the menu.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun FilterDropdown(
    field: String,
    title: String,
    options: List<FacetChip>,
    selected: Set<String>,
    expanded: Boolean,
    onExpandedChange: (Boolean) -> Unit,
    onToggle: (String) -> Unit,
    onClear: () -> Unit,
    modifier: Modifier = Modifier,
    enabled: (FacetChip) -> Boolean = { true },
    /** Published: radios, and a pick closes the menu. */
    single: Boolean = false,
    /** For [single]: the option that stands for "nothing selected" ("Any time"). */
    anyValue: String? = null,
    note: String? = null,
) {
    var query by remember(field) { mutableStateOf("") }
    LaunchedEffect(expanded) { if (!expanded) query = "" }
    val editable = options.size > DROPDOWN_SEARCH_MIN
    val labels = options.associate { it.value to it.label }
    val summary = dropdownSummary(selected, options.map { it.value }) { labels[it] ?: it }
    ExposedDropdownMenuBox(expanded = expanded, onExpandedChange = onExpandedChange, modifier = modifier.fillMaxWidth().testTag("dd_$field")) {
        Anchor(field, title, summary, editable, expanded, query, { query = it }, onExpandedChange, selected.isNotEmpty(), onClear, note)
        ExposedDropdownMenu(expanded = expanded, onDismissRequest = { onExpandedChange(false) }, modifier = Modifier.heightIn(max = MENU_MAX_HEIGHT)) {
            val shown = options.filter { optionMatches(query, it.value, it.label) }
            if (shown.isEmpty()) NoMatch()
            for (o in shown) {
                val on = o.value in selected || (single && selected.isEmpty() && o.value == anyValue)
                OptionItem(
                    tag = "opt_${field}_${o.value}",
                    label = o.label,
                    count = o.count,
                    checked = on,
                    enabled = on || enabled(o),
                    single = single,
                    onClick = {
                        onToggle(o.value)
                        if (single) onExpandedChange(false)
                    },
                )
            }
        }
    }
}

/**
 * File kind (D137): groups with a tri-state checkbox and an expand button, labels under them
 * (collapsed by default, which keeps the menu small). Always searchable: by name, id, extension
 * and group name; a search shows the matching groups expanded.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun FileKindDropdown(
    selected: Set<String>,
    catalogue: FileKindCatalogue?,
    counts: FacetCounts?,
    order: ChipOrder,
    expanded: Boolean,
    onExpandedChange: (Boolean) -> Unit,
    onToggle: (String) -> Unit,
    onClear: () -> Unit,
    modifier: Modifier = Modifier,
    note: String? = null,
) {
    val field = FILE_KIND_FIELD
    var query by remember { mutableStateOf("") }
    LaunchedEffect(expanded) { if (!expanded) query = "" }
    val open = remember { mutableStateMapOf<String, Boolean>() }
    val groups = order.arrangeFileKinds(fileKindGroups(catalogue, counts, selected), catalogue)
    val summary = fileKindSummary(selected, fileKindOrder(groups), fileKindLabelsOf(catalogue, counts)) { fileKindName(catalogue, it) }
    ExposedDropdownMenuBox(expanded = expanded, onExpandedChange = onExpandedChange, modifier = modifier.fillMaxWidth().testTag("dd_$field")) {
        Anchor(field, FILE_KIND_TITLE, summary, true, expanded, query, { query = it }, onExpandedChange, selected.isNotEmpty(), onClear, note)
        ExposedDropdownMenu(expanded = expanded, onDismissRequest = { onExpandedChange(false) }, modifier = Modifier.heightIn(max = MENU_MAX_HEIGHT)) {
            val searching = foldForSearch(query).isNotEmpty()
            val shown = filterFileKindGroups(groups, query)
            if (shown.isEmpty()) NoMatch()
            for (g in shown) {
                val id = g.group.value
                val state = fileKindState(selected, id)
                val isOpen = searching || open[id] == true
                val toggleable = when (state) {
                    GroupState.CHECKED -> ToggleableState.On
                    GroupState.MIXED -> ToggleableState.Indeterminate
                    GroupState.UNCHECKED -> ToggleableState.Off
                }
                val groupEnabled = state != GroupState.UNCHECKED || g.group.count != 0L
                DropdownMenuItem(
                    text = { Text(g.group.label, maxLines = 1, overflow = TextOverflow.Ellipsis) },
                    onClick = { onToggle(id) },
                    enabled = groupEnabled,
                    leadingIcon = { TriStateCheckbox(state = toggleable, onClick = null, enabled = groupEnabled) },
                    trailingIcon = {
                        Row(verticalAlignment = Alignment.CenterVertically) {
                            g.group.count?.let { CountText(it) }
                            if (g.labels.isNotEmpty() && !searching) {
                                IconButton(
                                    onClick = { open[id] = !isOpen },
                                    modifier = Modifier.testTag("opt_${field}_expand_$id"),
                                ) {
                                    Icon(
                                        if (isOpen) Icons.Filled.KeyboardArrowUp else Icons.Filled.KeyboardArrowDown,
                                        (if (isOpen) "Hide the kinds of " else "Show the kinds of ") + g.group.label,
                                    )
                                }
                            }
                        }
                    },
                    modifier = Modifier.testTag("opt_${field}_group_$id").semantics {
                        role = Role.Checkbox
                        stateDescription = when (state) {
                            GroupState.CHECKED -> "Selected"
                            GroupState.MIXED -> "Partly selected"
                            GroupState.UNCHECKED -> "Not selected"
                        }
                    },
                )
                if (isOpen) {
                    for (l in g.labels) {
                        val on = fileKindChecked(selected, l.value)
                        OptionItem(
                            tag = "opt_${field}_${l.value}",
                            label = l.label,
                            count = l.count,
                            checked = on,
                            enabled = on || l.count != 0L,
                            single = false,
                            indent = 24.dp,
                            onClick = { onToggle(l.value) },
                        )
                    }
                }
            }
        }
    }
}

/**
 * The collapsed control: title, summary, clear × and arrow. Read-only for short lists; for
 * long ones the field is the search box while the menu is open.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun ExposedDropdownMenuBoxScope.Anchor(
    field: String,
    title: String,
    summary: String,
    editable: Boolean,
    expanded: Boolean,
    query: String,
    onQuery: (String) -> Unit,
    onExpandedChange: (Boolean) -> Unit,
    hasSelection: Boolean,
    onClear: () -> Unit,
    note: String?,
) {
    val searching = editable && expanded
    OutlinedTextField(
        value = if (searching) query else summary,
        onValueChange = { v ->
            if (!editable) return@OutlinedTextField
            if (expanded) onQuery(v) else onExpandedChange(true)
        },
        readOnly = !editable,
        singleLine = true,
        label = { Text(title) },
        placeholder = if (searching) ({ Text("Type to filter · $summary") }) else null,
        supportingText = note?.let { { Text(it, color = MaterialTheme.colorScheme.error) } },
        trailingIcon = {
            Row(verticalAlignment = Alignment.CenterVertically) {
                if (hasSelection) {
                    // The anchor's own tap handler also toggles the menu: closing it here wins.
                    IconButton(
                        onClick = { onClear(); onExpandedChange(false) },
                        modifier = Modifier.testTag("dd_${field}_clear"),
                    ) { Icon(Icons.Filled.Clear, "Clear $title") }
                }
                ExposedDropdownMenuDefaults.TrailingIcon(expanded = expanded)
            }
        },
        modifier = Modifier
            .menuAnchor(if (editable) ExposedDropdownMenuAnchorType.PrimaryEditable else ExposedDropdownMenuAnchorType.PrimaryNotEditable)
            .fillMaxWidth()
            .then(if (editable) Modifier.testTag("dd_${field}_search") else Modifier)
            .semantics { contentDescription = "$title: $summary" },
    )
}

@Composable
private fun OptionItem(
    tag: String,
    label: String,
    count: Long?,
    checked: Boolean,
    enabled: Boolean,
    single: Boolean,
    onClick: () -> Unit,
    indent: Dp = 0.dp,
) {
    DropdownMenuItem(
        text = { Text(label, maxLines = 2, overflow = TextOverflow.Ellipsis) },
        onClick = onClick,
        enabled = enabled,
        leadingIcon = {
            if (single) RadioButton(selected = checked, onClick = null, enabled = enabled) else Checkbox(checked = checked, onCheckedChange = null, enabled = enabled)
        },
        trailingIcon = count?.let { c -> { CountText(c) } },
        contentPadding = PaddingValues(start = 12.dp + indent, end = 12.dp),
        modifier = Modifier.testTag(tag).semantics {
            role = if (single) Role.RadioButton else Role.Checkbox
            stateDescription = if (checked) "Selected" else "Not selected"
        },
    )
}

/** A value's count, read as "1 234 torrents". */
@Composable
private fun CountText(count: Long) {
    Text(
        formatCount(count),
        style = MaterialTheme.typography.labelMedium,
        modifier = Modifier.semantics { contentDescription = "${formatNumber(count)} torrents" },
    )
}

@Composable
private fun NoMatch() {
    Text("No match", Modifier.padding(16.dp), style = MaterialTheme.typography.bodySmall)
}

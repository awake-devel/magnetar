package app.magnetar.ui.search

import app.magnetar.api.FacetCounts
import app.magnetar.api.FileKindCatalogue
import java.text.Normalizer

/*
 * File kinds read by the header probes (D137, D140): pure functions shared by the filter sheet
 * and the Stats tab, with the same test vectors as the web client (`js/file-kinds.js`). A value
 * is a sniff group (`video`, no dot) or a label (`video.mkv`, `exe.pe.dotnet`); the group of a
 * label is the part before its first dot. A group matches every label in it.
 */

/** Facet, `/facets` field and query-string name of the file-kind filter. */
const val FILE_KIND_FIELD = "probe_file_kind"

/** Dropdown title of the file-kind filter. */
const val FILE_KIND_TITLE = "File kind"

/** More options than this get a search box (always for File kind). */
const val DROPDOWN_SEARCH_MIN = 12

/** Values asked for the file-kind facet (`FacetsRequest.sizes`): every group and label. */
const val FILE_KIND_FACET_SIZE = 400

fun isFileKindGroup(value: String): Boolean = '.' !in value

/** `exe.pe.dotnet` → `exe`; a group is its own group. */
fun fileKindGroupOf(value: String): String = value.substringBefore('.')

/**
 * The labels of each group: the catalogue's, or without one the labels the current [counts]
 * list (the dropdown then knows no others).
 */
fun fileKindLabelsOf(catalogue: FileKindCatalogue?, counts: FacetCounts? = null): (String) -> List<String> {
    if (catalogue != null && catalogue.groups.isNotEmpty()) {
        val byGroup = catalogue.groups.associate { g -> g.id to g.labels.map { it.id } }
        return { g -> byGroup[g].orEmpty() }
    }
    val fromCounts = counts?.counts.orEmpty().map { it.value }.filter { !isFileKindGroup(it) }.groupBy { fileKindGroupOf(it) }
    return { g -> fromCounts[g].orEmpty() }
}

/**
 * The canonical selection: lowercase, trimmed, no duplicates; a selected group drops its
 * labels; every label of a group selected becomes the group; sorted.
 */
fun normalizeFileKinds(values: Collection<String>, labelsOf: (String) -> List<String>): List<String> {
    val set = values.map { it.trim().lowercase() }.filter { it.isNotEmpty() }.toMutableSet()
    val groups = set.filter { isFileKindGroup(it) }.toSet()
    set.removeAll { !isFileKindGroup(it) && fileKindGroupOf(it) in groups }
    for (g in set.filter { !isFileKindGroup(it) }.map { fileKindGroupOf(it) }.toSet()) {
        val labels = labelsOf(g)
        if (labels.isNotEmpty() && labels.all { it in set }) {
            set.removeAll(labels.toSet())
            set += g
        }
    }
    return set.sorted()
}

/**
 * [value] (a group or a label) switched in [selection], with real checkbox semantics: a checked
 * group is removed, an unchecked one selected (its labels dropped); a label of a checked group
 * leaves the group's other labels selected.
 */
fun toggleFileKind(selection: Collection<String>, value: String, labelsOf: (String) -> List<String>): List<String> {
    val v = value.trim().lowercase()
    val sel = normalizeFileKinds(selection, labelsOf)
    if (v.isEmpty()) return sel
    val next = if (isFileKindGroup(v)) {
        if (v in sel) sel - v else sel.filter { fileKindGroupOf(it) != v } + v
    } else {
        val g = fileKindGroupOf(v)
        when {
            g in sel -> sel - g + labelsOf(g).filter { it != v }
            v in sel -> sel - v
            else -> sel + v
        }
    }
    return normalizeFileKinds(next, labelsOf)
}

/** Checkbox state of a group header. */
enum class GroupState { CHECKED, MIXED, UNCHECKED }

fun fileKindState(selection: Collection<String>, group: String): GroupState = when {
    group in selection -> GroupState.CHECKED
    selection.any { !isFileKindGroup(it) && fileKindGroupOf(it) == group } -> GroupState.MIXED
    else -> GroupState.UNCHECKED
}

/** A label is ticked when it is selected or its group is. */
fun fileKindChecked(selection: Collection<String>, value: String): Boolean =
    value in selection || (!isFileKindGroup(value) && fileKindGroupOf(value) in selection)

/**
 * The name of a group or label: the catalogue's, else (no catalogue, or a value it lacks) a
 * group id with a capital first letter and a label's raw id.
 */
fun fileKindName(catalogue: FileKindCatalogue?, value: String): String {
    if (isFileKindGroup(value)) {
        catalogue?.groups?.firstOrNull { it.id == value }?.let { return it.name }
        return value.replaceFirstChar { it.uppercaseChar() }
    }
    val g = catalogue?.groups?.firstOrNull { it.id == fileKindGroupOf(value) }
    return g?.labels?.firstOrNull { it.id == value }?.name ?: value
}

/** One row of a dropdown: value, label, count (null = no counts), extensions (file kinds). */
data class FileKindOption(val value: String, val label: String, val count: Long?, val extensions: List<String> = emptyList())

/** A file-kind group and the labels shown under it. */
data class FileKindGroupOption(val group: FileKindOption, val labels: List<FileKindOption>)

/**
 * The File kind dropdown's rows. With counts: the groups with a count or a selection, most
 * frequent first (ties in catalogue order), each with its labels that have a count or are
 * selected, most frequent first; without counts: every catalogue group and label in catalogue
 * order. A selected value the counts lack has count 0.
 */
fun fileKindGroups(catalogue: FileKindCatalogue?, counts: FacetCounts?, selection: Collection<String>): List<FileKindGroupOption> {
    val catGroups = catalogue?.groups.orEmpty()
    val catIndex = catGroups.withIndex().associate { (i, g) -> g.id to i }
    val labelIndex = HashMap<String, Int>()
    val extensions = HashMap<String, List<String>>()
    for (g in catGroups) for ((i, l) in g.labels.withIndex()) { labelIndex[l.id] = i; extensions[l.id] = l.extensions }
    fun opt(v: String, c: Long?) = FileKindOption(v, fileKindName(catalogue, v), c, extensions[v].orEmpty())
    if (counts == null) {
        val out = catGroups.map { g -> FileKindGroupOption(opt(g.id, null), g.labels.map { opt(it.id, null) }) }.toMutableList()
        // A selected value the catalogue lacks (or no catalogue at all) stays, so it can be cleared.
        val known = out.flatMap { listOf(it.group.value) + it.labels.map { l -> l.value } }.toSet()
        val extra = selection.filter { it !in known }.groupBy { fileKindGroupOf(it) }
        for ((g, vs) in extra.toSortedMap()) {
            val i = out.indexOfFirst { it.group.value == g }
            val labels = vs.filter { !isFileKindGroup(it) }.sorted().map { opt(it, null) }
            if (i >= 0) out[i] = out[i].copy(labels = out[i].labels + labels) else out += FileKindGroupOption(opt(g, null), labels)
        }
        return out
    }
    val byValue = counts.counts.associate { it.value to it.count }
    val groupIds = (counts.counts.map { fileKindGroupOf(it.value) } + selection.map { fileKindGroupOf(it) }).toSet()
    val groups = groupIds.filter { g ->
        (byValue[g] ?: 0L) > 0 || selection.any { fileKindGroupOf(it) == g } ||
            counts.counts.any { it.count > 0 && fileKindGroupOf(it.value) == g }
    }.sortedWith(compareByDescending<String> { byValue[it] ?: 0L }.thenBy { catIndex[it] ?: Int.MAX_VALUE }.thenBy { it })
    return groups.map { g ->
        val labelIds = (counts.counts.map { it.value } + selection)
            .filter { !isFileKindGroup(it) && fileKindGroupOf(it) == g }.toSet()
            .filter { (byValue[it] ?: 0L) > 0 || it in selection }
            .sortedWith(compareByDescending<String> { byValue[it] ?: 0L }.thenBy { labelIndex[it] ?: Int.MAX_VALUE }.thenBy { it })
        FileKindGroupOption(opt(g, byValue[g] ?: 0L), labelIds.map { opt(it, byValue[it] ?: 0L) })
    }
}

/** Every value of [groups] in option order (group, then its labels). */
fun fileKindOrder(groups: List<FileKindGroupOption>): List<String> =
    groups.flatMap { listOf(it.group.value) + it.labels.map { l -> l.value } }

/**
 * The collapsed dropdown's text: "Any", the one value's label, or "{first in option order}, +{n−1}".
 * A value not in [order] sorts after those that are.
 */
fun dropdownSummary(selected: Collection<String>, order: List<String>, label: (String) -> String): String {
    if (selected.isEmpty()) return "Any"
    val index = order.withIndex().associate { (i, v) -> v to i }
    val first = selected.sortedWith(compareBy<String> { index[it] ?: Int.MAX_VALUE }.thenBy { it }).first()
    return if (selected.size == 1) label(first) else "${label(first)}, +${selected.size - 1}"
}

/** More labels of one group than this are shown as one chip (D140 C3). */
const val KIND_CHIP_MAX = 3

/** One removable chip of a file-kind selection: the [values] it removes and its [label]. */
data class FileKindChip(val values: List<String>, val label: String)

/**
 * The selection as removable chips (D140 C3, as `fileKindChips` in the web client): a group is
 * one chip, up to [KIND_CHIP_MAX] labels of a group one chip each, more one chip for the group:
 * "{group} except {the 1–2 labels left out}" or "{group}: {n} kinds". Unticking one label of a
 * checked group leaves every other catalogue label selected; this keeps that one chip instead
 * of one per label.
 */
fun fileKindChips(selection: Collection<String>, labelsOf: (String) -> List<String>, name: (String) -> String): List<FileKindChip> {
    val out = mutableListOf<FileKindChip>()
    for (g in selection.map { fileKindGroupOf(it) }.toSortedSet()) {
        if (g in selection) out += FileKindChip(listOf(g), name(g))
        val labels = selection.filter { !isFileKindGroup(it) && fileKindGroupOf(it) == g }.sorted()
        if (labels.size <= KIND_CHIP_MAX) {
            for (l in labels) out += FileKindChip(listOf(l), name(l))
            continue
        }
        val left = labelsOf(g).filter { it !in labels }
        val label = if (left.size in 1..2) "${name(g)} except ${left.joinToString(", ") { name(it) }}" else "${name(g)}: ${labels.size} kinds"
        out += FileKindChip(labels, label)
    }
    return out
}

/**
 * The collapsed File kind text: the [fileKindChips] labels as [dropdownSummary] puts them ("Any",
 * the one chip, or "{first in option order}, +{n−1}").
 */
fun fileKindSummary(selection: Collection<String>, order: List<String>, labelsOf: (String) -> List<String>, name: (String) -> String): String {
    val chips = fileKindChips(selection, labelsOf, name)
    if (chips.isEmpty()) return "Any"
    val index = order.withIndex().associate { (i, v) -> v to i }
    val first = chips.minBy { c -> c.values.minOf { index[it] ?: Int.MAX_VALUE } }
    return if (chips.size == 1) first.label else "${first.label}, +${chips.size - 1}"
}

/** Lowercase without accents ("Français" → "francais"), for the dropdown search. */
fun foldForSearch(s: String): String =
    Normalizer.normalize(s, Normalizer.Form.NFD).replace(Regex("\\p{M}+"), "").lowercase().trim()

/** A plain option matches on its label or its value. */
fun optionMatches(query: String, value: String, label: String): Boolean {
    val q = foldForSearch(query)
    return q.isEmpty() || foldForSearch(label).contains(q) || foldForSearch(value).contains(q)
}

/** A file-kind label matches on its name, id or an extension. */
fun fileKindMatches(query: String, option: FileKindOption): Boolean {
    val q = foldForSearch(query)
    return q.isEmpty() || foldForSearch(option.label).contains(q) || foldForSearch(option.value).contains(q) ||
        option.extensions.any { foldForSearch(it).contains(q) }
}

/**
 * The groups a search shows: a group whose name or id matches keeps all its labels, another
 * keeps only its matching labels, and a group with no match is left out. Blank = all.
 */
fun filterFileKindGroups(groups: List<FileKindGroupOption>, query: String): List<FileKindGroupOption> {
    if (foldForSearch(query).isEmpty()) return groups
    return groups.mapNotNull { g ->
        if (fileKindMatches(query, g.group)) return@mapNotNull g
        val labels = g.labels.filter { fileKindMatches(query, it) }
        if (labels.isEmpty()) null else g.copy(labels = labels)
    }
}

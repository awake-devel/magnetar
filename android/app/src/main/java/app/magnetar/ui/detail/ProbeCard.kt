package app.magnetar.ui.detail

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Edit
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilterChip
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.SegmentedButton
import androidx.compose.material3.SegmentedButtonDefaults
import androidx.compose.material3.SingleChoiceSegmentedButtonRow
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.SheetValue
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextDecoration
import androidx.compose.ui.unit.dp
import app.magnetar.api.ProbeCorrectionItem
import app.magnetar.api.ProbeInfo
import app.magnetar.ui.common.fileName

/** Sends correction items; `done(true)` once saved (closes the editor), `done(false)` keeps it. */
typealias SaveCorrections = (items: List<ProbeCorrectionItem>, message: String, done: (Boolean) -> Unit) -> Unit

/** Deletes one correction; `done` as [SaveCorrections]. */
typealias UndoCorrection = (id: Long, done: (Boolean) -> Unit) -> Unit

/**
 * "Detected": what a worker read from the files themselves (D117, D121). With [canCorrect]
 * (`probe_corrections`, D131) every audio and subtitle track is a row with an edit button, and
 * corrections that no longer anchor are listed under the files.
 */
@Composable
fun ProbeCard(
    p: ProbeInfo,
    files: List<ProbeFileView>,
    canCorrect: Boolean = false,
    correcting: Boolean = false,
    onSave: SaveCorrections = { _, _, done -> done(false) },
    onUndo: UndoCorrection = { _, done -> done(false) },
) {
    var editing by remember { mutableStateOf<Pair<TrackDraft, TrackRow?>?>(null) }
    Card(Modifier.fillMaxWidth().padding(top = 12.dp).testTag("probe_card")) {
        Column(Modifier.padding(12.dp)) {
            Text("Detected", style = MaterialTheme.typography.titleSmall)
            Text(
                "Read from the first blocks of each file, not from its name.",
                style = MaterialTheme.typography.bodySmall,
            )
            Text(probeSummary(p), style = MaterialTheme.typography.labelSmall)
            files.forEachIndexed { i, f ->
                if (i > 0) HorizontalDivider(Modifier.padding(top = 8.dp))
                Text(
                    f.name,
                    style = MaterialTheme.typography.bodyMedium,
                    fontWeight = FontWeight.Medium,
                    modifier = Modifier.padding(top = 8.dp),
                )
                Text(
                    f.headline,
                    style = MaterialTheme.typography.labelSmall,
                    color = if (f.verified) MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.onSurfaceVariant,
                )
                f.warnings.forEach {
                    Text("⚠ $it", style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.error)
                }
                val info = f.file
                if (canCorrect && info?.video != null) {
                    f.videoLines.forEach { Text(it, style = MaterialTheme.typography.bodySmall) }
                    TrackRows(
                        info = info,
                        enabled = !correcting,
                        onEdit = { row -> editing = draftOf(row) to row },
                    )
                    FlowRow(horizontalArrangement = Arrangement.spacedBy(4.dp)) {
                        TextButton(
                            onClick = { editing = draftOf(info.dhtIndex) to null },
                            enabled = !correcting,
                            modifier = Modifier.testTag("probe_add_track_${info.dhtIndex}"),
                        ) { Text("Add a track the probe missed") }
                        val unchecked = checkAllItems(info)
                        if (unchecked.isNotEmpty()) {
                            TextButton(
                                onClick = { onSave(unchecked, "Marked ${unchecked.size} tracks as checked") {} },
                                enabled = !correcting,
                                modifier = Modifier.testTag("probe_check_all_${info.dhtIndex}"),
                            ) { Text("All tracks match VLC") }
                        }
                    }
                    f.otherLines.forEach { Text(it, style = MaterialTheme.typography.bodySmall) }
                } else {
                    f.lines.forEach { Text(it, style = MaterialTheme.typography.bodySmall) }
                }
            }
            if (canCorrect && p.unplaced.isNotEmpty()) {
                HorizontalDivider(Modifier.padding(top = 8.dp))
                Text(
                    "Corrections that no longer match the probe",
                    style = MaterialTheme.typography.bodyMedium,
                    fontWeight = FontWeight.Medium,
                    modifier = Modifier.padding(top = 8.dp),
                )
                p.unplaced.forEach { u ->
                    Row(Modifier.fillMaxWidth().testTag("probe_unplaced_${u.id}"), verticalAlignment = Alignment.CenterVertically) {
                        Text(
                            unplacedText(u, fileName(u.path).ifBlank { u.path }),
                            style = MaterialTheme.typography.bodySmall,
                            modifier = Modifier.weight(1f),
                        )
                        TextButton(onClick = { onUndo(u.id) {} }, enabled = !correcting) { Text("Remove") }
                    }
                }
            }
        }
    }
    editing?.let { (draft, row) ->
        TrackEditorSheet(
            start = draft,
            row = row,
            correcting = correcting,
            onDismiss = { editing = null },
            onSave = { item -> onSave(listOf(item), "Correction saved") { ok -> if (ok) editing = null } },
            onUndo = { id -> onUndo(id) { ok -> if (ok) editing = null } },
        )
    }
}

@Composable
private fun TrackRows(info: app.magnetar.api.ProbeFileInfo, enabled: Boolean, onEdit: (TrackRow) -> Unit) {
    val rows = remember(info) { trackRows(info) }
    // Keyed on the file, not on `info`: a saved correction replaces `info`, and a list the user
    // expanded stays expanded.
    var allSubs by rememberSaveable(info.dhtIndex) { mutableStateOf(false) }
    val subs = rows.filter { it.track == TRACK_SUB }
    val shownSubs = if (allSubs || subs.size <= MAX_SUBTITLE_ROWS) subs else subs.take(MAX_SUBTITLE_ROWS)
    rows.filter { it.track == TRACK_AUDIO }.forEach { TrackRowView(it, enabled) { onEdit(it) } }
    shownSubs.forEach { TrackRowView(it, enabled) { onEdit(it) } }
    if (shownSubs.size < subs.size) {
        TextButton(onClick = { allSubs = true }) { Text("Show all ${subs.size} subtitle tracks") }
    }
}

@Composable
private fun TrackRowView(row: TrackRow, enabled: Boolean, onEdit: () -> Unit) {
    var showDetected by remember(row.file, row.track, row.pos, row.fix?.id) { mutableStateOf(false) }
    val tag = "probe_track_${row.file}_${row.track}_" + (row.pos?.toString() ?: "added${row.fix?.id ?: 0}")
    Row(
        Modifier.fillMaxWidth().clickable(enabled = enabled, onClick = onEdit).testTag(tag),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Column(Modifier.weight(1f).padding(vertical = 4.dp)) {
            Text(
                row.text,
                style = MaterialTheme.typography.bodySmall,
                textDecoration = if (row.removed) TextDecoration.LineThrough else null,
            )
            row.name?.let {
                Text("“$it”", style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
            }
            row.marker?.let { Text(it, style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.primary) }
            row.review?.let { Text(it, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.error) }
            if (row.corrected && row.detectedText != null) {
                if (showDetected) {
                    Text(
                        "Detected: ${row.detectedText}",
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
                TextButton(onClick = { showDetected = !showDetected }) {
                    Text(if (showDetected) "Hide detected" else "Show detected")
                }
            }
        }
        IconButton(onClick = onEdit, enabled = enabled) { Icon(Icons.Filled.Edit, contentDescription = row.editLabel) }
    }
}

@OptIn(ExperimentalMaterial3Api::class, ExperimentalLayoutApi::class)
@Composable
private fun TrackEditorSheet(
    start: TrackDraft,
    row: TrackRow?,
    correcting: Boolean,
    onDismiss: () -> Unit,
    onSave: (ProbeCorrectionItem) -> Unit,
    onUndo: (Long) -> Unit,
) {
    var d by remember(start) { mutableStateOf(start) }
    var picking by remember { mutableStateOf(false) }
    // While sending, a swipe, a scrim tap or back must not close the sheet: on a failure the
    // draft stays for a retry (D131).
    val sending by rememberUpdatedState(correcting)
    val sheet = rememberModalBottomSheetState(
        skipPartiallyExpanded = true,
        confirmValueChange = { it != SheetValue.Hidden || !sending },
    )
    ModalBottomSheet(
        onDismissRequest = { if (!sending) onDismiss() },
        sheetState = sheet,
        modifier = Modifier.testTag("probe_editor"),
    ) {
        Column(Modifier.fillMaxWidth().padding(horizontal = 16.dp).padding(bottom = 16.dp)) {
            Text(editorTitle(d), style = MaterialTheme.typography.titleMedium)
            Text(
                "Compare with VLC's Audio Track / Sub Track menu: tracks are listed in the same order.",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            if (row != null && !row.added && row.detectedText != null) {
                Text("Detected: ${row.detectedText}", style = MaterialTheme.typography.bodySmall, modifier = Modifier.padding(top = 4.dp))
                row.detectedName?.let { Text("Name in the file: “$it”", style = MaterialTheme.typography.bodySmall) }
            }
            if (d.start == null) {
                SingleChoiceSegmentedButtonRow(Modifier.fillMaxWidth().padding(top = 8.dp)) {
                    listOf(TRACK_AUDIO to "Audio", TRACK_SUB to "Subtitle").forEachIndexed { i, (t, label) ->
                        SegmentedButton(
                            selected = d.track == t,
                            onClick = { if (d.track != t) d = d.copy(track = t, kinds = emptySet()) },
                            shape = SegmentedButtonDefaults.itemShape(i, 2),
                            enabled = !correcting,
                        ) { Text(label) }
                    }
                }
            }
            val fieldsOn = !d.removed && !correcting
            OutlinedTextField(
                value = languageLabel(d.lang),
                onValueChange = {},
                readOnly = true,
                enabled = fieldsOn,
                label = { Text("Language") },
                modifier = Modifier.fillMaxWidth().padding(top = 8.dp).testTag("probe_editor_language"),
                trailingIcon = {
                    TextButton(onClick = { picking = true }, enabled = fieldsOn) { Text("Change") }
                },
            )
            Text("Kind", style = MaterialTheme.typography.labelMedium, modifier = Modifier.padding(top = 8.dp))
            val order = if (d.track == TRACK_AUDIO) AUDIO_KIND_ORDER else SUB_KIND_ORDER
            val labels = if (d.track == TRACK_AUDIO) AUDIO_KIND_LABELS else SUB_KIND_LABELS
            FlowRow(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                (order + d.kinds.filter { it !in order }.sorted()).forEach { k ->
                    val on = k in d.kinds
                    FilterChip(
                        selected = on,
                        onClick = { d = d.copy(kinds = if (on) d.kinds - k else d.kinds + k) },
                        label = { Text(labels[k] ?: k.replace('_', ' ')) },
                        enabled = fieldsOn,
                        modifier = Modifier.testTag("probe_editor_kind_$k"),
                    )
                }
            }
            OutlinedTextField(
                value = d.name,
                onValueChange = { d = d.copy(name = cleanTrackName(it)) },
                enabled = fieldsOn,
                singleLine = true,
                label = { Text("Track name (as VLC shows it)") },
                supportingText = { Text("Optional") },
                modifier = Modifier.fillMaxWidth().padding(top = 8.dp).testTag("probe_editor_name"),
            )
            if (d.pos != null) {
                Row(Modifier.fillMaxWidth().padding(top = 8.dp), verticalAlignment = Alignment.CenterVertically) {
                    Column(Modifier.weight(1f)) {
                        Text("This track doesn't exist", style = MaterialTheme.typography.bodyMedium)
                        Text(
                            "VLC does not list it. It is left out of search.",
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                    Switch(
                        checked = d.removed,
                        onCheckedChange = { d = d.copy(removed = it) },
                        enabled = !correcting,
                        modifier = Modifier.testTag("probe_editor_removed"),
                    )
                }
            }
            if (correcting) LinearProgressIndicator(Modifier.fillMaxWidth().padding(top = 8.dp))
            FlowRow(
                Modifier.fillMaxWidth().padding(top = 12.dp),
                horizontalArrangement = Arrangement.spacedBy(8.dp, Alignment.End),
            ) {
                d.fixId?.let { id ->
                    OutlinedButton(onClick = { onUndo(id) }, enabled = !correcting, modifier = Modifier.testTag("probe_editor_undo")) {
                        Text("Undo correction")
                    }
                }
                TextButton(onClick = onDismiss, enabled = !correcting) { Text("Cancel") }
                val (label, ok) = primaryAction(d)
                Button(
                    onClick = { itemOf(d)?.let(onSave) },
                    enabled = ok && !correcting,
                    modifier = Modifier.testTag("probe_editor_save"),
                ) { Text(label) }
            }
            Spacer(Modifier.padding(4.dp))
        }
    }
    if (picking) {
        LanguagePicker(
            onPick = { code ->
                d = d.copy(lang = code)
                picking = false
            },
            onDismiss = { picking = false },
        )
    }
}

@Composable
private fun LanguagePicker(onPick: (String?) -> Unit, onDismiss: () -> Unit) {
    var query by remember { mutableStateOf("") }
    val choices = remember(query) { languageChoices(query) }
    AlertDialog(
        onDismissRequest = onDismiss,
        confirmButton = {},
        dismissButton = { TextButton(onClick = onDismiss) { Text("Cancel") } },
        title = { Text("Language") },
        text = {
            Column {
                OutlinedTextField(
                    value = query,
                    onValueChange = { query = it },
                    singleLine = true,
                    label = { Text("Search languages") },
                    modifier = Modifier.fillMaxWidth().testTag("probe_language_search"),
                )
                LazyColumn(Modifier.heightIn(max = 360.dp).padding(top = 8.dp)) {
                    items(choices, key = { it.code ?: "" }) { c ->
                        Text(
                            c.label,
                            style = MaterialTheme.typography.bodyMedium,
                            modifier = Modifier.fillMaxWidth().clickable { onPick(c.code) }.padding(vertical = 10.dp)
                                .semantics { contentDescription = c.label }
                                .testTag("probe_language_${c.code ?: "none"}"),
                        )
                    }
                }
            }
        },
    )
}

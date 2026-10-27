package app.magnetar.ui.saved

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.testTagsAsResourceId

/** "Save search": the current text, filters and sort, stored on the server (D77). */
@OptIn(androidx.compose.ui.ExperimentalComposeUiApi::class)
@Composable
fun SaveSearchDialog(
    initialName: String,
    filterCount: Int,
    onDismiss: () -> Unit,
    onSave: (name: String, notify: Boolean) -> Unit,
) {
    var name by remember { mutableStateOf(initialName) }
    var notify by remember { mutableStateOf(true) }
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text("Save search") },
        text = {
            Column(Modifier.semantics { testTagsAsResourceId = true }) {
                OutlinedTextField(
                    value = name,
                    onValueChange = { name = it.take(200) },
                    label = { Text("Name") },
                    singleLine = true,
                    modifier = Modifier.fillMaxWidth().testTag("save_name"),
                )
                if (filterCount > 0) {
                    Text(
                        if (filterCount == 1) "With the active filter" else "With the $filterCount active filters",
                        style = MaterialTheme.typography.bodySmall,
                    )
                }
                Row(
                    Modifier.fillMaxWidth(),
                    horizontalArrangement = Arrangement.SpaceBetween,
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Text("Notify on new results")
                    Switch(checked = notify, onCheckedChange = { notify = it })
                }
            }
        },
        confirmButton = {
            TextButton(
                onClick = { onSave(name.trim(), notify) },
                // The confirm button is outside the text slot: tag it in its own semantics.
                enabled = name.isNotBlank(),
                modifier = Modifier.semantics { testTagsAsResourceId = true }.testTag("save_confirm"),
            ) { Text("Save") }
        },
        dismissButton = { TextButton(onClick = onDismiss) { Text("Cancel") } },
    )
}

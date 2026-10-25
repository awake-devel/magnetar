package app.magnetar.ui.common

import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Button
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import app.magnetar.ffi.FfiCategory

fun categoryEmoji(c: FfiCategory): String = when (c) {
    FfiCategory.VIDEO -> "🎬" // clapper board
    FfiCategory.AUDIO -> "🎵" // musical note
    FfiCategory.APPLICATION -> "💾" // floppy disk
    FfiCategory.GAME -> "🎮" // video game
    FfiCategory.PORN -> "🔞" // no one under 18
    FfiCategory.ANIME -> "🌸" // cherry blossom
    FfiCategory.BOOK -> "📚" // books
    FfiCategory.PICTURE -> "🖼" // framed picture
    FfiCategory.OTHER -> "📦" // package
}

fun categoryLabel(c: FfiCategory): String = c.name.lowercase().replaceFirstChar { it.uppercase() }

@Composable
fun CategoryIcon(c: FfiCategory, modifier: Modifier = Modifier) {
    Text(categoryEmoji(c), style = MaterialTheme.typography.titleLarge, modifier = modifier)
}

/** Score badge; tinted as a warning when under the user's threshold. */
@Composable
fun TrustBadge(score: Int, threshold: Int, modifier: Modifier = Modifier) {
    val ok = score >= threshold
    val bg = if (ok) MaterialTheme.colorScheme.primaryContainer else MaterialTheme.colorScheme.errorContainer
    val fg = if (ok) MaterialTheme.colorScheme.onPrimaryContainer else MaterialTheme.colorScheme.onErrorContainer
    Box(
        modifier
            .background(bg, RoundedCornerShape(8.dp))
            .padding(horizontal = 8.dp, vertical = 2.dp),
    ) {
        Text("★ $score", color = fg, style = MaterialTheme.typography.labelMedium, fontWeight = FontWeight.Bold)
    }
}

@Composable
fun Banner(text: String, modifier: Modifier = Modifier, error: Boolean = false) {
    Surface(
        color = if (error) MaterialTheme.colorScheme.errorContainer else MaterialTheme.colorScheme.secondaryContainer,
        modifier = modifier.fillMaxWidth(),
    ) {
        Text(text, modifier = Modifier.padding(horizontal = 16.dp, vertical = 8.dp), style = MaterialTheme.typography.bodyMedium)
    }
}

@Composable
fun CenteredMessage(text: String, actionLabel: String? = null, onAction: () -> Unit = {}) {
    Box(Modifier.fillMaxSize().padding(24.dp), contentAlignment = Alignment.Center) {
        Column(horizontalAlignment = Alignment.CenterHorizontally) {
            Text(text, textAlign = TextAlign.Center, style = MaterialTheme.typography.bodyLarge)
            if (actionLabel != null) {
                Spacer(Modifier.height(12.dp))
                Button(onClick = onAction) { Text(actionLabel) }
            }
        }
    }
}

@Composable
fun SectionTitle(text: String, modifier: Modifier = Modifier) {
    Text(
        text,
        style = MaterialTheme.typography.titleSmall,
        color = MaterialTheme.colorScheme.primary,
        modifier = modifier.padding(top = 16.dp, bottom = 4.dp),
    )
}

fun copyToClipboard(context: Context, label: String, text: String) {
    context.getSystemService(ClipboardManager::class.java)?.setPrimaryClip(ClipData.newPlainText(label, text))
}

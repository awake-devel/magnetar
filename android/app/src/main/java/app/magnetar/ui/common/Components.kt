package app.magnetar.ui.common

import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.Button
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import app.magnetar.api.Category
import app.magnetar.api.Origin

fun categoryEmoji(c: Category): String = when (c) {
    Category.VIDEO -> "🎬" // clapper board
    Category.AUDIO -> "🎵" // musical note
    Category.APPLICATION -> "💾" // floppy disk
    Category.GAME -> "🎮" // video game
    Category.PORN -> "🔞" // no one under 18
    Category.ANIME -> "🌸" // cherry blossom
    Category.BOOK -> "📚" // books
    Category.PICTURE -> "🖼" // framed picture
    Category.OTHER -> "📦" // package
}

@Composable
fun CategoryIcon(c: Category, modifier: Modifier = Modifier) {
    Text(
        categoryEmoji(c),
        style = MaterialTheme.typography.titleLarge,
        modifier = modifier.semantics { contentDescription = categoryLabel(c) },
    )
}

/** Score badge: primary at or above the threshold, neutral below, red only for [TrustLevel.NEGATIVE]. */
@Composable
fun TrustBadge(score: Int, threshold: Int, modifier: Modifier = Modifier, level: TrustLevel = trustLevel(score, threshold)) {
    val (bg, fg) = when (level) {
        TrustLevel.OK -> MaterialTheme.colorScheme.primaryContainer to MaterialTheme.colorScheme.onPrimaryContainer
        TrustLevel.LOW -> MaterialTheme.colorScheme.surfaceVariant to MaterialTheme.colorScheme.onSurfaceVariant
        TrustLevel.NEGATIVE -> MaterialTheme.colorScheme.errorContainer to MaterialTheme.colorScheme.onErrorContainer
    }
    Box(
        modifier
            .background(bg, RoundedCornerShape(8.dp))
            .padding(horizontal = 8.dp, vertical = 2.dp)
            .semantics { contentDescription = "Trust $score" },
    ) {
        Text("★ $score", color = fg, style = MaterialTheme.typography.labelMedium, fontWeight = FontWeight.Bold)
    }
}

/**
 * Result badge by source (D51, D74): the trust star for Nostr results, a neutral outlined
 * "DHT" in its place for DHT-only ones (no publisher to score), the star with a small "DHT"
 * chip when both hold the torrent.
 */
@Composable
fun SourceBadge(origin: Origin, score: Int, threshold: Int, modifier: Modifier = Modifier) {
    when (origin) {
        Origin.NOSTR -> TrustBadge(score, threshold, modifier)
        Origin.DHT -> DhtChip(modifier)
        Origin.BOTH -> Column(modifier, horizontalAlignment = Alignment.End) {
            TrustBadge(score, threshold)
            Spacer(Modifier.height(2.dp))
            DhtChip(small = true)
        }
    }
}

/** Neutral outlined "DHT" label (not a score: no colour of the trust scale). */
@Composable
fun DhtChip(modifier: Modifier = Modifier, small: Boolean = false) {
    Box(
        modifier
            .border(1.dp, MaterialTheme.colorScheme.outline, RoundedCornerShape(8.dp))
            .padding(horizontal = if (small) 6.dp else 8.dp, vertical = if (small) 0.dp else 2.dp)
            .semantics { contentDescription = "Found on the DHT" },
    ) {
        Text(
            "DHT",
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            style = if (small) MaterialTheme.typography.labelSmall else MaterialTheme.typography.labelMedium,
            fontWeight = FontWeight.Bold,
        )
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
fun CenteredMessage(text: String, actionLabel: String? = null, modifier: Modifier = Modifier, onAction: () -> Unit = {}) {
    Box(modifier.fillMaxSize().padding(24.dp), contentAlignment = Alignment.Center) {
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

@Composable
fun NumberField(label: String, value: String, onChange: (String) -> Unit, modifier: Modifier, maxDigits: Int = 9) {
    OutlinedTextField(
        value = value,
        onValueChange = { v -> onChange(v.filter { it.isDigit() }.take(maxDigits)) },
        label = { Text(label) },
        singleLine = true,
        keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Number),
        modifier = modifier,
    )
}

fun copyToClipboard(context: Context, label: String, text: String) {
    context.getSystemService(ClipboardManager::class.java)?.setPrimaryClip(ClipData.newPlainText(label, text))
}

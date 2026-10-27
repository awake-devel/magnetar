package app.magnetar.ui.common

import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.MaterialTheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.produceState
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp

/** Loads a server-relative poster path (`MediaInfo.poster`); null = none. Set by the activity. */
val LocalPosterLoader = staticCompositionLocalOf<suspend (String) -> ImageBitmap?> { { null } }

private sealed interface PosterState {
    data object Loading : PosterState
    data class Loaded(val image: ImageBitmap) : PosterState
    data object None : PosterState
}

/**
 * A 2:3 poster (TMDB posters are 2:3). The frame is drawn while loading so the card does not
 * jump; without a poster nothing is drawn.
 */
@Composable
fun Poster(path: String?, description: String, width: Dp = 104.dp, modifier: Modifier = Modifier) {
    if (path == null) return
    val loader = LocalPosterLoader.current
    val state by produceState<PosterState>(PosterState.Loading, path) {
        value = loader(path)?.let { PosterState.Loaded(it) } ?: PosterState.None
    }
    val frame = modifier.width(width).aspectRatio(2f / 3f).clip(RoundedCornerShape(6.dp))
    when (val s = state) {
        PosterState.Loading -> Box(frame.background(MaterialTheme.colorScheme.surfaceVariant))
        is PosterState.Loaded -> Image(s.image, contentDescription = description, contentScale = ContentScale.Crop, modifier = frame)
        PosterState.None -> {}
    }
}

package app.magnetar.ui.media

import android.graphics.BitmapFactory
import androidx.compose.foundation.Image
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilterChip
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.ListItem
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import app.magnetar.ffi.FfiMediaInfo
import app.magnetar.ffi.FfiReleaseGroup
import app.magnetar.ffi.facetLabel
import app.magnetar.formatAge
import app.magnetar.formatBytes
import app.magnetar.ui.common.Banner
import app.magnetar.ui.common.CenteredMessage
import app.magnetar.ui.detail.mediaHeadline
import app.magnetar.ui.detail.mediaKindLabel
import app.magnetar.ui.search.peersText
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext

/** "Season 5 · Episode 9", "Season 5 (pack)", "Complete series", or null (films). */
fun episodeHeading(g: FfiReleaseGroup, series: Boolean): String? = when {
    !series -> null
    g.season != null && g.episode != null -> "Season ${g.season} · Episode ${g.episode}"
    g.season != null -> "Season ${g.season} (pack)"
    g.episode != null -> "Episode ${g.episode}"
    else -> "Complete series / packs"
}

/** "1080p · MULTi (VFF) · 3 releases". */
fun groupHeading(g: FfiReleaseGroup): String = listOfNotNull(
    g.resolution?.let { facetLabel("resolution", it) } ?: "Unknown quality",
    g.languageLabel,
    if (g.releases.size == 1) "1 release" else "${g.releases.size} releases",
).joinToString(" · ")

@OptIn(ExperimentalMaterial3Api::class, ExperimentalLayoutApi::class)
@Composable
fun MediaScreen(
    onBack: () -> Unit,
    onOpenTorrent: (String) -> Unit,
    onOpenGraph: (String) -> Unit = {},
    vm: MediaViewModel = viewModel(),
) {
    val state by vm.state.collectAsStateWithLifecycle()
    val snackbar = remember { SnackbarHostState() }
    LaunchedEffect(vm) { vm.messages.collect { snackbar.showSnackbar(it) } }
    val media = state.releases?.media
    val series = state.releases?.groups.orEmpty().any { it.season != null || it.episode != null }

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text(media?.title ?: "Releases", maxLines = 1, overflow = TextOverflow.Ellipsis) },
                navigationIcon = { IconButton(onClick = onBack) { Icon(Icons.AutoMirrored.Filled.ArrowBack, "Back") } },
                actions = {
                    // The release graph (D47) is drawn from the server's data.
                    if (state.releases?.source == "server" && state.graphs) {
                        IconButton(onClick = { onOpenGraph("media/${vm.mediaId}") }) {
                            Icon(app.magnetar.ui.common.AppIcons.Graph, "Release graph")
                        }
                    }
                },
            )
        },
        snackbarHost = { SnackbarHost(snackbar) },
    ) { padding ->
        Column(Modifier.padding(padding).fillMaxSize()) {
            if (state.loading) LinearProgressIndicator(Modifier.fillMaxWidth())
            val r = state.releases
            if (r?.source == "local" && r.serverError != null) {
                Banner("Cached releases only (server unreachable: ${r.serverError})")
            }
            when {
                state.error != null && r == null -> CenteredMessage(state.error!!, "Retry", vm::refresh)
                r == null -> {}
                r.groups.isEmpty() -> CenteredMessage("No releases known for this title.")
                else -> LazyColumn(Modifier.fillMaxSize()) {
                    item { media?.let { MediaHeader(it, state.posterPath) } }
                    item {
                        Column(Modifier.padding(horizontal = 16.dp)) {
                            val langs = r.groups.map { it.language to it.languageLabel }.distinct()
                            if (langs.size > 1) {
                                FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                                    for ((key, label) in langs) {
                                        FilterChip(state.language == key, { vm.setLanguage(key) }, { Text(label) })
                                    }
                                }
                            }
                            val res = r.groups.mapNotNull { it.resolution }.distinct()
                            if (res.size > 1) {
                                FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                                    for (x in res) {
                                        FilterChip(state.resolution == x, { vm.setResolution(x) }, { Text(facetLabel("resolution", x)) })
                                    }
                                }
                            }
                            Text(
                                "${r.total} releases" + if (r.truncated) " (best seeded shown)" else "",
                                style = MaterialTheme.typography.labelMedium,
                                modifier = Modifier.padding(vertical = 4.dp),
                            )
                        }
                    }
                    var lastEpisode: String? = null
                    for (g in state.shown) {
                        val ep = episodeHeading(g, series)
                        if (ep != null && ep != lastEpisode) {
                            item(key = "ep-$ep") {
                                Text(
                                    ep,
                                    style = MaterialTheme.typography.titleMedium,
                                    color = MaterialTheme.colorScheme.primary,
                                    modifier = Modifier.padding(start = 16.dp, end = 16.dp, top = 16.dp),
                                )
                            }
                            lastEpisode = ep
                        }
                        item(key = "g-${g.season}-${g.episode}-${g.resolution}-${g.language}") {
                            Text(
                                groupHeading(g),
                                style = MaterialTheme.typography.titleSmall,
                                modifier = Modifier.padding(start = 16.dp, end = 16.dp, top = 8.dp),
                            )
                        }
                        items(g.releases, key = { "${g.season}-${g.episode}-${it.infohash}" }) { t ->
                            ListItem(
                                modifier = Modifier.clickable { onOpenTorrent(t.infohash) },
                                headlineContent = { Text(t.title, maxLines = 2, overflow = TextOverflow.Ellipsis) },
                                supportingContent = {
                                    Column {
                                        Text("${formatBytes(t.totalSize)} · ${peersText(t.seeders, t.leechers)} · ${formatAge(t.createdAt)}")
                                        t.attrs?.badges?.takeIf { it.isNotEmpty() }?.let {
                                            Text(
                                                it.joinToString(" · "),
                                                style = MaterialTheme.typography.labelSmall,
                                                color = MaterialTheme.colorScheme.secondary,
                                                maxLines = 1,
                                                overflow = TextOverflow.Ellipsis,
                                            )
                                        }
                                    }
                                },
                            )
                            HorizontalDivider()
                        }
                    }
                }
            }
        }
    }
}

@Composable
private fun MediaHeader(m: FfiMediaInfo, posterPath: String?) {
    var bitmap by remember(posterPath) { mutableStateOf<ImageBitmap?>(null) }
    LaunchedEffect(posterPath) {
        bitmap = posterPath?.let {
            withContext(Dispatchers.IO) { runCatching { BitmapFactory.decodeFile(it)?.asImageBitmap() }.getOrNull() }
        }
    }
    Row(Modifier.padding(16.dp)) {
        bitmap?.let {
            Image(
                bitmap = it,
                contentDescription = "Poster",
                contentScale = ContentScale.Crop,
                modifier = Modifier.width(96.dp).aspectRatio(2f / 3f).clip(RoundedCornerShape(6.dp)),
            )
            Spacer(Modifier.width(12.dp))
        }
        Column(Modifier.weight(1f)) {
            Text(mediaHeadline(m.title, m.year, m.endYear), style = MaterialTheme.typography.titleLarge)
            Text(
                listOfNotNull(
                    mediaKindLabel(m.kind),
                    m.rating?.let { "★ %.1f".format(it) },
                    m.runtimeMin?.takeIf { it > 0 }?.let { "$it min" },
                ).joinToString(" · "),
                style = MaterialTheme.typography.bodyMedium,
            )
            if (m.genres.isNotEmpty()) Text(m.genres.joinToString(", "), style = MaterialTheme.typography.bodySmall)
            m.overview?.let { Text(it, style = MaterialTheme.typography.bodySmall, maxLines = 6, overflow = TextOverflow.Ellipsis) }
        }
    }
}

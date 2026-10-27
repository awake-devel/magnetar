package app.magnetar.ui.title

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
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilterChip
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import app.magnetar.api.MediaInfo
import app.magnetar.api.ReleaseGroup
import app.magnetar.ui.common.AppIcons
import app.magnetar.ui.common.Banner
import app.magnetar.ui.common.CenteredMessage
import app.magnetar.ui.common.Poster
import app.magnetar.ui.common.ResultRow
import app.magnetar.ui.common.cachedAgo
import app.magnetar.ui.common.facetLabel
import app.magnetar.ui.common.mediaHeadline
import app.magnetar.ui.detail.mediaFacts

/** "Season 5 · Episode 9", "Season 5 (pack)", "Complete series / packs", or null (films). */
fun episodeHeading(g: ReleaseGroup, series: Boolean): String? = when {
    !series -> null
    g.season != null && g.episode != null -> "Season ${g.season} · Episode ${g.episode}"
    g.season != null -> "Season ${g.season} (pack)"
    g.episode != null -> "Episode ${g.episode}"
    else -> "Complete series / packs"
}

/** "1080p · MULTi (VFF) · 3 releases". */
fun groupHeading(g: ReleaseGroup): String = listOf(
    g.resolution?.let { facetLabel("resolution", it) } ?: "Unknown quality",
    g.languageLabel.ifBlank { "Unknown language" },
    if (g.releases.size == 1) "1 release" else "${g.releases.size} releases",
).joinToString(" · ")

@OptIn(ExperimentalMaterial3Api::class, ExperimentalLayoutApi::class)
@Composable
fun TitleScreen(
    vm: TitleViewModel,
    threshold: Int,
    onBack: () -> Unit,
    onOpenTorrent: (String) -> Unit,
    onOpenGraph: ((String) -> Unit)? = null,
) {
    val state by vm.state.collectAsStateWithLifecycle()
    val r = state.releases
    val media = r?.media
    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text(media?.title ?: "Releases", maxLines = 1, overflow = TextOverflow.Ellipsis) },
                navigationIcon = { IconButton(onClick = onBack) { Icon(Icons.AutoMirrored.Filled.ArrowBack, "Back") } },
                actions = {
                    onOpenGraph?.let { open ->
                        IconButton(onClick = { open("media/${vm.mediaId}") }, modifier = Modifier.testTag("title_graph")) {
                            Icon(AppIcons.Graph, "Releases graph")
                        }
                    }
                },
            )
        },
    ) { padding ->
        Column(Modifier.padding(padding).fillMaxSize()) {
            if (state.loading) LinearProgressIndicator(Modifier.fillMaxWidth())
            if (r != null && state.error != null) {
                Banner((if (state.offline) "Offline: " else "") + state.error, error = true)
            } else if (r != null && state.cachedAtMs != null) {
                Banner(cachedAgo(state.cachedAtMs!!))
            }
            when {
                r == null && state.error != null -> CenteredMessage(state.error!!, "Retry", onAction = vm::refresh)
                r == null -> {}
                else -> LazyColumn(Modifier.fillMaxSize().testTag("title_page")) {
                    item { media?.let { MediaHeader(it) } }
                    item {
                        Column(Modifier.padding(horizontal = 16.dp)) {
                            if (state.filters.activeCount > 0) {
                                Row(verticalAlignment = Alignment.CenterVertically) {
                                    Text(
                                        if (state.filters.activeCount == 1) "1 search filter applies" else "${state.filters.activeCount} search filters apply",
                                        style = MaterialTheme.typography.bodySmall,
                                        modifier = Modifier.weight(1f),
                                    )
                                    TextButton(onClick = vm::clearFilters) { Text("Show all") }
                                }
                            }
                            if (state.seasons.size > 1) {
                                FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                                    for (s in state.seasons) {
                                        FilterChip(state.season == s, { vm.setSeason(s) }, { Text("Season $s") }, Modifier.testTag("title_season_$s"))
                                    }
                                }
                            }
                            if (state.resolutions.size > 1) {
                                FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                                    for (x in state.resolutions) {
                                        FilterChip(state.resolution == x, { vm.setResolution(x) }, { Text(facetLabel("resolution", x)) }, Modifier.testTag("title_res_$x"))
                                    }
                                }
                            }
                            if (state.languages.size > 1) {
                                FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                                    for ((key, label) in state.languages) {
                                        FilterChip(state.language == key, { vm.setLanguage(key) }, { Text(label.ifBlank { key }) }, Modifier.testTag("title_lang_$key"))
                                    }
                                }
                            }
                            Text(
                                (if (r.total == 1L) "1 release" else "${r.total} releases") + if (r.truncated) " (best seeded shown)" else "",
                                style = MaterialTheme.typography.labelMedium,
                                modifier = Modifier.padding(vertical = 4.dp).testTag("title_count"),
                            )
                        }
                    }
                    if (r.groups.isEmpty()) {
                        item { CenteredMessage("No releases of this title match.") }
                    }
                    var lastEpisode: String? = null
                    for (g in state.shown) {
                        val ep = episodeHeading(g, state.isSeries)
                        if (ep != null && ep != lastEpisode) {
                            item(key = "ep-$ep") {
                                Text(
                                    ep,
                                    style = MaterialTheme.typography.titleMedium,
                                    color = MaterialTheme.colorScheme.primary,
                                    modifier = Modifier.padding(start = 16.dp, end = 16.dp, top = 16.dp).semantics { heading() },
                                )
                            }
                            lastEpisode = ep
                        }
                        item(key = "g-${g.season}-${g.episode}-${g.resolution}-${g.language}") {
                            Text(
                                groupHeading(g),
                                style = MaterialTheme.typography.titleSmall,
                                modifier = Modifier.padding(start = 16.dp, end = 16.dp, top = 8.dp).testTag("title_group"),
                            )
                        }
                        items(g.releases, key = { "${g.season}-${g.episode}-${g.resolution}-${g.language}-${it.infohash}" }) { t ->
                            ResultRow(t, threshold, onOpenTorrent, onOpenMedia = null, showMedia = false)
                            HorizontalDivider()
                        }
                    }
                }
            }
        }
    }
}

@Composable
private fun MediaHeader(m: MediaInfo) {
    Row(Modifier.padding(16.dp)) {
        if (m.poster != null) {
            Poster(m.poster, "Poster of ${m.title}", width = 96.dp)
            Spacer(Modifier.width(12.dp))
        }
        Column(Modifier.weight(1f)) {
            Text(mediaHeadline(m.title, m.year, m.endYear), style = MaterialTheme.typography.titleLarge)
            Text(mediaFacts(m), style = MaterialTheme.typography.bodyMedium)
            if (m.genres.isNotEmpty()) Text(m.genres.joinToString(", "), style = MaterialTheme.typography.bodySmall)
            m.overview?.let { Text(it, style = MaterialTheme.typography.bodySmall, maxLines = 6, overflow = TextOverflow.Ellipsis) }
        }
    }
}

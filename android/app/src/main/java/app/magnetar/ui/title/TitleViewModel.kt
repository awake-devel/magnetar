package app.magnetar.ui.title

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import app.magnetar.api.ApiException
import app.magnetar.api.MediaReleases
import app.magnetar.api.ReleaseGroup
import app.magnetar.api.Sort
import app.magnetar.data.Repository
import app.magnetar.ui.common.isOffline
import app.magnetar.ui.common.userMessage
import app.magnetar.ui.search.SearchFilters
import app.magnetar.ui.search.buildQuery
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch

data class TitleUiState(
    val loading: Boolean = true,
    val releases: MediaReleases? = null,
    val error: String? = null,
    val offline: Boolean = false,
    val cachedAtMs: Long? = null,
    /** The Search tab's filters applied to the releases (cleared by "Show all"). */
    val filters: SearchFilters = SearchFilters(),
    /** In-page chips: one season, one resolution, one language group; null = all. */
    val season: Int? = null,
    val resolution: String? = null,
    val language: String? = null,
) {
    val groups: List<ReleaseGroup> get() = releases?.groups.orEmpty()

    /** Series pages group by season/episode first (packs and ranges have no episode). */
    val isSeries: Boolean get() = groups.any { it.season != null || it.episode != null }

    val seasons: List<Int> get() = groups.mapNotNull { it.season }.distinct().sorted()
    val resolutions: List<String> get() = groups.mapNotNull { it.resolution }.distinct()
    val languages: List<Pair<String, String>> get() = groups.map { it.language to it.languageLabel }.distinct()

    /** Groups passing the in-page chips. */
    val shown: List<ReleaseGroup>
        get() = groups.filter {
            (season == null || it.season == season) &&
                (resolution == null || it.resolution == resolution) &&
                (language == null || it.language == language)
        }
}

/** The title page (D44): every release of one movie/series, grouped by season/episode, quality, language. */
class TitleViewModel(
    private val repo: Repository,
    val mediaId: Long,
    initialFilters: SearchFilters,
) : ViewModel() {
    private val _state = MutableStateFlow(TitleUiState(filters = initialFilters))
    val state: StateFlow<TitleUiState> = _state.asStateFlow()

    init {
        refresh()
    }

    fun refresh() {
        _state.update { it.copy(loading = true, error = null) }
        viewModelScope.launch {
            repo.refreshHealth()
            val features = repo.health.value?.features?.toSet().orEmpty()
            // No text: it would narrow inside the title's releases. Filters, trust and the adult switch apply.
            val q = buildQuery("", _state.value.filters.supportedBy(features), Sort.SEEDERS, true, repo.settings.value.showAdult)
            val key = "$mediaId " + repo.keyOf(app.magnetar.api.SearchQuery.serializer(), q.copy(since = _state.value.filters.date.seconds?.let { -it }))
            if (_state.value.releases == null) {
                repo.readCache(CACHE_TITLE, key, MediaReleases.serializer())?.let { c ->
                    _state.update { if (it.releases == null) it.copy(releases = c.value, cachedAtMs = c.savedAtMs) else it }
                }
            }
            try {
                val r = repo.service().mediaReleases(mediaId, q)
                _state.update { it.copy(loading = false, releases = r, cachedAtMs = null, offline = false) }
                repo.writeCache(CACHE_TITLE, key, MediaReleases.serializer(), r)
            } catch (e: CancellationException) {
                throw e
            } catch (e: ApiException) {
                _state.update { it.copy(loading = false, error = e.userMessage(), offline = e.isOffline) }
            }
        }
    }

    /** "Show all": the page without the Search tab's filters. */
    fun clearFilters() {
        _state.update { it.copy(filters = SearchFilters(), releases = null, season = null, resolution = null, language = null) }
        refresh()
    }

    fun setSeason(s: Int?) = _state.update { it.copy(season = if (it.season == s) null else s) }

    fun setResolution(r: String?) = _state.update { it.copy(resolution = if (it.resolution == r) null else r) }

    fun setLanguage(l: String?) = _state.update { it.copy(language = if (it.language == l) null else l) }

    companion object {
        const val CACHE_TITLE = "title"
    }
}

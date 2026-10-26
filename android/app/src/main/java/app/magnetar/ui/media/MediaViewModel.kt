package app.magnetar.ui.media

import android.app.Application
import androidx.lifecycle.SavedStateHandle
import androidx.lifecycle.viewModelScope
import app.magnetar.ffi.FfiMediaReleases
import app.magnetar.ffi.FfiReleaseGroup
import app.magnetar.ffiCatching
import app.magnetar.ui.common.BaseViewModel
import app.magnetar.ui.search.SearchUiState
import app.magnetar.ui.search.toQuery
import app.magnetar.userMessage
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch

data class MediaUiState(
    val loading: Boolean = true,
    val releases: FfiMediaReleases? = null,
    val error: String? = null,
    val posterPath: String? = null,
    /** In-page filters: one language group key and one resolution, null = all. */
    val language: String? = null,
    val resolution: String? = null,
    /** The server draws release graphs (D47); false offline or from an older server. */
    val graphs: Boolean = false,
) {
    /** Groups passing the in-page filters. */
    val shown: List<FfiReleaseGroup>
        get() = releases?.groups.orEmpty().filter {
            (language == null || it.language == language) && (resolution == null || it.resolution == resolution)
        }
}

/** The title page (D44): every release of one movie/series, grouped by season/episode, quality, language. */
class MediaViewModel(application: Application, handle: SavedStateHandle) : BaseViewModel(application) {
    val mediaId: Long = checkNotNull(handle.get<Long>("mediaId"))

    private val _state = MutableStateFlow(MediaUiState())
    val state: StateFlow<MediaUiState> = _state.asStateFlow()

    init {
        refresh()
    }

    fun refresh() {
        _state.update { it.copy(loading = true, error = null) }
        viewModelScope.launch {
            // Every release under the configured trust threshold; the page filters in place.
            val query = SearchUiState().toQuery()
            ffiCatching { magApp.magnetar().mediaReleases(mediaId, query) }
                .onSuccess { r ->
                    _state.update { it.copy(loading = false, releases = r) }
                    if (r.source == "server") {
                        val features = ffiCatching { magApp.magnetar().serverFeatures() }.getOrNull().orEmpty()
                        _state.update { it.copy(graphs = "graphs" in features) }
                    }
                    val poster = r.media?.poster
                    if (poster != null) {
                        ffiCatching { magApp.magnetar().mediaPoster(mediaId, poster) }
                            .onSuccess { p -> _state.update { it.copy(posterPath = p) } }
                    }
                }
                .onFailure { e -> _state.update { it.copy(loading = false, error = e.userMessage()) } }
        }
    }

    fun setLanguage(key: String?) = _state.update { it.copy(language = if (it.language == key) null else key) }

    fun setResolution(r: String?) = _state.update { it.copy(resolution = if (it.resolution == r) null else r) }
}

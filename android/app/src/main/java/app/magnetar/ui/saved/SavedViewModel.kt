package app.magnetar.ui.saved

import android.app.Application
import app.magnetar.ffi.FfiSavedSearch
import app.magnetar.ui.common.BaseViewModel
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update

data class SavedUiState(
    val loading: Boolean = true,
    val searches: List<FfiSavedSearch> = emptyList(),
    val error: String? = null,
)

class SavedViewModel(application: Application) : BaseViewModel(application) {
    private val _state = MutableStateFlow(SavedUiState())
    val state: StateFlow<SavedUiState> = _state.asStateFlow()

    init {
        refresh()
    }

    fun refresh() {
        _state.update { it.copy(loading = true, error = null) }
        launchFfi(onError = { msg -> _state.update { it.copy(loading = false, error = msg) } }) { m ->
            val list = io { m.savedSearches() }
            _state.update { it.copy(loading = false, searches = list) }
        }
    }

    fun setNotify(s: FfiSavedSearch, notify: Boolean) = launchFfi { m ->
        io { m.saveSearch(s.copy(notify = notify)) }
        refresh()
    }

    fun delete(s: FfiSavedSearch) = launchFfi { m ->
        io { m.deleteSavedSearch(s.id) }
        message("Deleted \"${s.name}\"")
        refresh()
    }

    /** Hands the query to the Search screen. */
    fun run(s: FfiSavedSearch) {
        magApp.pendingSearch.value = s.query.copy(offset = 0u)
    }
}

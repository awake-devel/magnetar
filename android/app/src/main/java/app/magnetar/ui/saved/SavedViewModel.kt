package app.magnetar.ui.saved

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import app.magnetar.api.ApiException
import app.magnetar.api.Features
import app.magnetar.api.SavedSearch
import app.magnetar.data.Repository
import app.magnetar.ui.common.userMessage
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.drop
import kotlinx.coroutines.flow.receiveAsFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch

data class SavedUiState(
    val loading: Boolean = true,
    val searches: List<SavedSearch> = emptyList(),
    val error: String? = null,
    val notConfigured: Boolean = false,
    /** The server can edit a saved search and poll for notifications (`saved_search_poll`, D77). */
    val canEdit: Boolean = false,
)

/** The Saved tab: the server's saved searches (D77). */
class SavedViewModel(private val repo: Repository) : ViewModel() {
    private val _state = MutableStateFlow(SavedUiState())
    val state: StateFlow<SavedUiState> = _state.asStateFlow()

    private val messagesChannel = Channel<String>(Channel.BUFFERED)
    val messages: Flow<String> = messagesChannel.receiveAsFlow()

    init {
        refresh()
        viewModelScope.launch { repo.savedSearchesChanged.drop(1).collect { refresh() } }
        viewModelScope.launch { repo.settings.drop(1).collect { refresh() } }
    }

    fun refresh() {
        _state.update { it.copy(loading = true, error = null) }
        viewModelScope.launch {
            try {
                if (!repo.settings.value.hasServer) throw ApiException.NotConfigured()
                val list = repo.service().savedSearches()
                repo.refreshHealth()
                _state.update {
                    it.copy(
                        loading = false, searches = list.sortedBy { s -> s.name.lowercase() }, notConfigured = false,
                        canEdit = repo.has(Features.SAVED_SEARCH_POLL),
                    )
                }
            } catch (e: CancellationException) {
                throw e
            } catch (e: ApiException) {
                _state.update {
                    it.copy(loading = false, error = e.userMessage(), notConfigured = e is ApiException.NotConfigured)
                }
            }
        }
    }

    /** Runs [s] on the Search tab. */
    fun run(s: SavedSearch) {
        repo.pendingSearch.value = s.query
    }

    fun setNotify(s: SavedSearch, notify: Boolean) {
        if (!_state.value.canEdit) return
        replace(s.copy(notify = notify))
        viewModelScope.launch {
            try {
                replace(repo.service().updateSavedSearch(s.copy(notify = notify)))
            } catch (e: CancellationException) {
                throw e
            } catch (e: ApiException) {
                replace(s)
                messagesChannel.trySend("Not changed: ${e.userMessage()}")
            }
        }
    }

    fun delete(s: SavedSearch) {
        viewModelScope.launch {
            try {
                repo.service().deleteSavedSearch(s.id)
                _state.update { st -> st.copy(searches = st.searches.filter { it.id != s.id }) }
                messagesChannel.trySend("Deleted \"${s.name}\"")
            } catch (e: CancellationException) {
                throw e
            } catch (e: ApiException) {
                if (e is ApiException.NotFound) {
                    _state.update { st -> st.copy(searches = st.searches.filter { it.id != s.id }) }
                } else {
                    messagesChannel.trySend("Not deleted: ${e.userMessage()}")
                }
            }
        }
    }

    private fun replace(s: SavedSearch) =
        _state.update { st -> st.copy(searches = st.searches.map { if (it.id == s.id) s else it }) }
}

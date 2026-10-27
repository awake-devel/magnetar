package app.magnetar.ui.common

import app.magnetar.api.ApiException

/** What a screen says for a failed call (docs/APP.md §4). */
fun Throwable.userMessage(): String = when (this) {
    is ApiException.NotConfigured -> "Set your server in Settings"
    is ApiException.Unauthorized -> "Token rejected: check it in Settings"
    is ApiException.NotFound -> "Not on this server"
    is ApiException.IndexBuilding -> "The server is building its search index"
    is ApiException.Timeout -> "The server did not answer in time"
    is ApiException.Network -> "Server unreachable"
    is ApiException.Decode -> "Unexpected answer from the server"
    is ApiException.Http -> message ?: "HTTP $status"
    else -> message ?: toString()
}

/** Errors after which a cached page is still worth showing (with an "Offline" banner). */
val Throwable.isOffline: Boolean
    get() = this is ApiException.Network || this is ApiException.Timeout || this is ApiException.IndexBuilding

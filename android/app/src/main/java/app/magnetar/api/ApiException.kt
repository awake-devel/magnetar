package app.magnetar.api

import java.io.IOException

/**
 * Every failure of a [MagnetarApi] call, as the screens render it (docs/APP.md §4). A cancelled
 * coroutine is never one of these: cancellation propagates as `CancellationException`.
 */
sealed class ApiException(message: String, cause: Throwable? = null) : IOException(message, cause) {
    /** No server URL in Settings, or not an http(s) URL. */
    class NotConfigured(message: String = "No server set") : ApiException(message)

    /** 401: the token is missing or wrong. */
    class Unauthorized(message: String) : ApiException(message)

    /** 404: unknown torrent, title or saved search. */
    class NotFound(message: String) : ApiException(message)

    /** 503 `search_index_unavailable`: the server is (re)building its search index. */
    class IndexBuilding(message: String) : ApiException(message)

    /** Any other 4xx/5xx; [code] is the server's error code (`invalid`, `upstream`, …). */
    class Http(val status: Int, val code: String, message: String) : ApiException(message)

    /** The call (or its connect / read) took longer than its timeout. */
    class Timeout(cause: Throwable) : ApiException("The server did not answer in time", cause)

    /** DNS, refused connection, TLS, reset… */
    class Network(cause: Throwable) : ApiException(cause.message ?: "Server unreachable", cause)

    /** The answer was not the JSON this app expects. */
    class Decode(cause: Throwable) : ApiException("Unexpected answer from the server", cause)
}

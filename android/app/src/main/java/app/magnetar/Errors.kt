package app.magnetar

import app.magnetar.ffi.FfiException
import app.magnetar.ffi.InternalException
import kotlin.coroutines.cancellation.CancellationException

/** A message fit for a snackbar or an error line. */
fun Throwable.userMessage(): String = when (this) {
    is FfiException.Db -> "Database error: $msg"
    is FfiException.Network -> "Network error: $msg"
    is FfiException.Invalid -> msg
    is FfiException.Server -> "Server error ${status}: $msg"
    is FfiException.Other -> msg
    // Rust panics surface as InternalException.
    is InternalException -> "Internal error: ${message ?: "unknown"}"
    is UnsatisfiedLinkError -> "Native library missing: ${message ?: ""}"
    else -> message ?: javaClass.simpleName
}

/**
 * Like [runCatching] but lets coroutine cancellation through and also catches the [Error]s that
 * JNA raises when the native library cannot be loaded.
 */
inline fun <T> ffiCatching(block: () -> T): Result<T> = try {
    Result.success(block())
} catch (e: CancellationException) {
    throw e
} catch (e: Exception) {
    Result.failure(e)
} catch (e: UnsatisfiedLinkError) {
    Result.failure(e)
} catch (e: NoClassDefFoundError) {
    Result.failure(e)
}

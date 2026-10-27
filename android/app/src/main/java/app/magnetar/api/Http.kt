package app.magnetar.api

import java.io.IOException
import java.util.concurrent.TimeUnit
import kotlin.coroutines.resumeWithException
import kotlinx.coroutines.suspendCancellableCoroutine
import okhttp3.Call
import okhttp3.Callback
import okhttp3.HttpUrl
import okhttp3.Interceptor
import okhttp3.OkHttpClient
import okhttp3.Response

/**
 * Call timeouts per request class (docs/APP.md §4): the whole request, DNS to last byte. The
 * search box must never wait long, a forced seed check may.
 */
enum class RequestClass(val callTimeoutMs: Long) {
    SEARCH(10_000),
    DETAIL(15_000),
    CHECK(45_000),
    STATS(30_000),
    POLL(60_000),
}

object Http {
    /**
     * The process-wide base client; [MagnetarApi] derives its clients with `newBuilder()`, which
     * shares the connection pool and dispatcher.
     */
    fun baseClient(): OkHttpClient = OkHttpClient.Builder()
        .connectTimeout(5, TimeUnit.SECONDS)
        .readTimeout(15, TimeUnit.SECONDS)
        .writeTimeout(15, TimeUnit.SECONDS)
        .callTimeout(RequestClass.DETAIL.callTimeoutMs, TimeUnit.MILLISECONDS)
        .build()
}

/**
 * Adds the bearer token only to requests for the configured server (same scheme, host and
 * port), so a redirect or an image on another host never receives it.
 */
class AuthInterceptor(private val server: HttpUrl, private val token: String) : Interceptor {
    override fun intercept(chain: Interceptor.Chain): Response {
        val request = chain.request()
        if (token.isEmpty() || !sameOrigin(request.url, server)) return chain.proceed(request)
        return chain.proceed(request.newBuilder().header("Authorization", "Bearer $token").build())
    }

    companion object {
        fun sameOrigin(a: HttpUrl, b: HttpUrl): Boolean =
            a.scheme == b.scheme && a.host.equals(b.host, ignoreCase = true) && a.port == b.port
    }
}

/** Status and body text of a finished call. */
class HttpResult(val status: Int, val body: String)

/**
 * Runs the call on OkHttp's dispatcher and reads the body there too, so the coroutine stays
 * suspended (and cancellable) until the last byte: cancelling it cancels the call, whether it
 * is still connecting or already streaming the body.
 */
suspend fun Call.awaitText(): HttpResult = suspendCancellableCoroutine { cont ->
    cont.invokeOnCancellation { cancel() }
    enqueue(object : Callback {
        override fun onResponse(call: Call, response: Response) {
            val result = try {
                response.use { HttpResult(it.code, it.body.string()) }
            } catch (e: IOException) {
                onFailure(call, e)
                return
            }
            cont.resume(result) { _, _, _ -> }
        }

        override fun onFailure(call: Call, e: IOException) {
            if (!cont.isCancelled) cont.resumeWithException(e)
        }
    })
}

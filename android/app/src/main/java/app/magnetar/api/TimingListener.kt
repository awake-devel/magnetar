package app.magnetar.api

import android.util.Log
import java.io.IOException
import java.net.InetAddress
import java.net.InetSocketAddress
import java.net.Proxy
import okhttp3.Call
import okhttp3.EventListener
import okhttp3.Handshake
import okhttp3.Protocol
import okhttp3.Response

/**
 * Debug builds only: one log line per call with the time of each phase (DNS, connect, TLS,
 * first byte, end) and the path, never the query string, body or headers (no token, no
 * search text). Tag `MagnetarHttp`.
 */
class TimingListener : EventListener() {
    private var start = 0L
    private val marks = StringBuilder()

    private fun mark(what: String) {
        marks.append(' ').append(what).append('=').append((System.nanoTime() - start) / 1_000_000)
    }

    override fun callStart(call: Call) {
        start = System.nanoTime()
    }

    override fun dnsEnd(call: Call, domainName: String, inetAddressList: List<InetAddress>) = mark("dns")
    override fun connectEnd(call: Call, inetSocketAddress: InetSocketAddress, proxy: Proxy, protocol: Protocol?) = mark("connect")
    override fun secureConnectEnd(call: Call, handshake: Handshake?) = mark("tls")
    override fun connectionAcquired(call: Call, connection: okhttp3.Connection) = mark("conn")
    override fun responseHeadersEnd(call: Call, response: Response) = mark("headers${response.code}")
    override fun callEnd(call: Call) {
        mark("end")
        Log.i(TAG, call.request().method + " " + call.request().url.encodedPath + marks)
    }

    override fun callFailed(call: Call, ioe: IOException) {
        mark("failed:" + ioe.javaClass.simpleName)
        Log.i(TAG, call.request().method + " " + call.request().url.encodedPath + marks)
    }

    companion object {
        const val TAG = "MagnetarHttp"
    }
}

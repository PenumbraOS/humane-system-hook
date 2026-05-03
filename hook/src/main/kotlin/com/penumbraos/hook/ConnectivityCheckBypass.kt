package com.penumbraos.hook

import android.net.Network
import android.util.Log
import java.net.HttpURLConnection
import java.net.URL

/**
 * Replace Humane's custom connectivity check with the standard Android endpoint.
 *
 * Humane's [humaneinternal.system.network.NetworkUtils.validateNetworkConnection]
 * does an HTTP GET to `http://connectivity-check.<env>.humane.cloud` and expects
 * a 204. That host is dead, so it always returns FAILURE, which makes
 * [humaneinternal.system.network.NetworkMonitor.getWifiInternetCheckStatus]
 * report not-connected and the home screen show the offline banner even when
 * the device is online.
 *
 * We replace the implementation with a GET to `connectivitycheck.gstatic.com/generate_204`
 * (Android's standard captive-portal probe), mapping:
 *   - 204                    -> CONNECTED
 *   - 301/302/303/305/307/308 -> WALLED_GARDEN
 *   - other / IOException    -> FAILURE
 */
object ConnectivityCheckBypass {

    private const val TAG = "PenumbraHook"

    private const val PROBE_URL = "http://connectivitycheck.gstatic.com/generate_204"
    private const val TIMEOUT_MS = 10_000

    private val REDIRECT_CODES = setOf(301, 302, 303, 305, 307, 308)

    fun install(cl: ClassLoader) {
        val className = "humaneinternal.system.network.NetworkUtils"
        val clazz = try {
            cl.loadClass(className)
        } catch (_: ClassNotFoundException) {
            Log.w(TAG, "  $className not found, skipping connectivity check bypass")
            return
        }

        val resultEnumClass = try {
            cl.loadClass("$className\$NetworkRequestResult")
        } catch (_: ClassNotFoundException) {
            Log.w(TAG, "  $className\$NetworkRequestResult not found, skipping")
            return
        }

        val connected = enumValue(resultEnumClass, "CONNECTED") ?: return
        val failure = enumValue(resultEnumClass, "FAILURE") ?: return
        val walledGarden = enumValue(resultEnumClass, "WALLED_GARDEN") ?: return

        HookUtils.hookMethodBefore(
            clazz,
            "validateNetworkConnection",
            arrayOf(Network::class.java),
        ) { param ->
            val network = param.args[0] as? Network
            param.result = if (network == null) {
                failure
            } else {
                probe(network, connected, failure, walledGarden)
            }
        }

        Log.i(TAG, "  Connectivity check redirected to $PROBE_URL")
    }

    private fun enumValue(enumClass: Class<*>, name: String): Any? {
        return try {
            val valueOf = enumClass.getDeclaredMethod("valueOf", String::class.java)
            valueOf.invoke(null, name)
        } catch (t: Throwable) {
            Log.e(TAG, "  Could not resolve NetworkRequestResult.$name: ${t.message}")
            null
        }
    }

    private fun probe(
        network: Network,
        connected: Any,
        failure: Any,
        walledGarden: Any,
    ): Any {
        var conn: HttpURLConnection? = null
        return try {
            conn = network.openConnection(URL(PROBE_URL)) as HttpURLConnection
            conn.connectTimeout = TIMEOUT_MS
            conn.readTimeout = TIMEOUT_MS
            conn.instanceFollowRedirects = false
            conn.useCaches = false
            // Touch the stream like the original implementation does, so that
            // captive portals which return 200 with HTML don't get classified
            // as CONNECTED based on a stale code.
            try { conn.inputStream.close() } catch (_: Throwable) { /* ignored */ }
            val code = conn.responseCode
            Log.d(TAG, "Connectivity probe ($PROBE_URL) -> $code")
            when {
                code == 204 -> connected
                code in REDIRECT_CODES -> walledGarden
                code in 200..299 -> walledGarden // 200 OK from a captive portal
                else -> failure
            }
        } catch (t: Throwable) {
            Log.d(TAG, "Connectivity probe failed: ${t.javaClass.simpleName}: ${t.message}")
            failure
        } finally {
            try { conn?.disconnect() } catch (_: Throwable) { /* ignored */ }
        }
    }
}

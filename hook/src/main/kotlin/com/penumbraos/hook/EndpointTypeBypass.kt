package com.penumbraos.hook

import android.util.Log

/**
 * Redirect the music experience's Tidal REST client to the local server.
 *
 * Every Tidal request URL is built by `Method.url()` as
 * `endpointType.baseUrl() + endpoint`, where
 * [humane.experience.music.provider.tidal.util.EndpointType] is an enum
 * {API, AUTH, LOGIN}. `EndpointType.baseUrl()` is `String.format(url, "")`
 * over a template field (`https://api%s.tidal.com/v1` for API), so it returns
 * the single base host for every call — the direct analog of
 * `ChannelFactory.getGatewayUri()` that [ChannelFactoryBypass] redirects.
 *
 * We hook `baseUrl()` after-return and rewrite only the API host to our local
 * server over plaintext http. The music APK declares usesCleartextTraffic=true,
 * so http://127.0.0.1 is permitted, and plaintext sidesteps Tidal cert pinning
 * entirely (pinning only applies to TLS) — same trick as ChannelFactory's
 * non-443 usePlaintext() path.
 *
 * AUTH/LOGIN are left untouched: the partner token is already stubbed by the
 * server's PartnerTokenRPCService.
 */
object EndpointTypeBypass {

    private const val TAG = "PenumbraHook"

    private const val ENDPOINT_TYPE_CLASS =
        "humane.experience.music.provider.tidal.util.EndpointType"

    /** Original Tidal API origin (prod, after `%s` -> ""). */
    private const val TIDAL_API_ORIGIN = "https://api.tidal.com"

    /** Local shim origin + path prefix that replaces the Tidal API origin. */
    const val SHIM_API_BASE = "http://127.0.0.1:8080/tidal-shim"

    fun install(cl: ClassLoader) {
        val clazz = try {
            cl.loadClass(ENDPOINT_TYPE_CLASS)
        } catch (_: ClassNotFoundException) {
            Log.w(TAG, "  $ENDPOINT_TYPE_CLASS not found, skipping Tidal redirect")
            return
        }

        try {
            clazz.getDeclaredMethod("baseUrl").also { it.isAccessible = true }
        } catch (_: NoSuchMethodException) {
            Log.w(TAG, "  EndpointType.baseUrl() missing, skipping Tidal redirect")
            return
        }

        HookUtils.hookMethodAfter(clazz, "baseUrl", emptyArray()) { param ->
            if (param.throwable != null) return@hookMethodAfter
            val original = param.result as? String ?: return@hookMethodAfter
            if (original.startsWith(TIDAL_API_ORIGIN)) {
                val redirected = SHIM_API_BASE + original.removePrefix(TIDAL_API_ORIGIN)
                param.result = redirected
                Log.w(TAG, "  EndpointType.baseUrl() redirected: $original -> $redirected")
            }
        }

        Log.w(TAG, "  Tidal API redirect installed (-> $SHIM_API_BASE)")
    }
}

package com.penumbraos.hook

import android.util.Log

/**
 * Redirect all gRPC traffic to the local mock server by hooking
 * ChannelFactory.getGatewayUri() in every process that has it on classpath.
 *
 * Two ChannelFactory class names exist across Humane APKs:
 * - humaneinternal.system.network.ChannelFactory (ironman, photography, krypto)
 * - humane.grandcentral.network.ChannelFactory   (grandcentral)
 *
 * Both have the same getGatewayUri() method that returns the gRPC endpoint.
 * By returning our mock server address with a non-443 port, ChannelFactory's
 * own newChannel() logic triggers usePlaintext() — no TLS/mTLS needed.
 */
object ChannelFactoryBypass {

    private const val TAG = "PenumbraHook"

    /** Mock server address. Non-443 port triggers usePlaintext() in ChannelFactory. */
    const val MOCK_SERVER_URI = "192.168.1.125:9090"

    private val CHANNEL_FACTORY_CLASSES = listOf(
        "humaneinternal.system.network.ChannelFactory",
        "humane.grandcentral.network.ChannelFactory",
    )

    fun install(cl: ClassLoader) {
        var hooked = 0
        for (className in CHANNEL_FACTORY_CLASSES) {
            val clazz = try {
                cl.loadClass(className)
            } catch (_: ClassNotFoundException) {
                continue
            }

            val method = try {
                clazz.getDeclaredMethod("getGatewayUri").also { it.isAccessible = true }
            } catch (_: NoSuchMethodException) {
                Log.w(TAG, "  $className found but getGatewayUri() missing, skipping")
                continue
            }

            HookUtils.hookMethodAfter(clazz, "getGatewayUri", emptyArray()) { param ->
                if (param.throwable == null) {
                    val original = param.result
                    param.result = MOCK_SERVER_URI
                    Log.i(TAG, "  ChannelFactory.getGatewayUri() redirected: $original -> $MOCK_SERVER_URI")
                }
            }
            hooked++
        }

        if (hooked > 0) {
            Log.i(TAG, "  ChannelFactory bypass installed ($hooked class(es) hooked)")
        } else {
            Log.w(TAG, "  No ChannelFactory classes found on classpath")
        }
    }
}

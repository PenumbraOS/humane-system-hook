package com.penumbraos.hook

import android.util.Log

/**
 * Hooks for the krypto APK (package: hu.ma.ne.krypto).
 *
 * The krypto process runs KryptoService which manages Kryptonite KMS,
 * privacy databases, and crypto keys. Its PrivacyClient creates its own
 * ChannelFactory targeting api.prod.humane.cloud. We redirect that to
 * the mock server and bypass encryption.
 */
object KryptoHooks {

    private const val TAG = "PenumbraHook"

    fun install(cl: ClassLoader) {
        Log.i(TAG, "Installing krypto hooks...")

        ChannelFactoryBypass.install(cl)
        DataProtectorBypass.install(cl)

        Log.i(TAG, "Krypto hooks installed")
    }
}

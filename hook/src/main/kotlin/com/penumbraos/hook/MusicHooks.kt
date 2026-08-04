package com.penumbraos.hook

import android.util.Log

/**
 * Hooks for the music experience APK (package: humane.experience.music).
 *
 * Redirects the app's Tidal REST client to the local server so the Ai Pin's
 * native music UI is backed by our shim instead of api.tidal.com.
 */
object MusicHooks {

    private const val TAG = "PenumbraHook"

    fun install(cl: ClassLoader) {
        Log.w(TAG, "Installing music hooks...")

        TcmSilencer.install(cl)

        ConnectivityCheckBypass.install(cl)

        TidalAuthBypass.install(cl)

        EndpointTypeBypass.install(cl)

        Log.w(TAG, "Music hooks installed")
    }
}

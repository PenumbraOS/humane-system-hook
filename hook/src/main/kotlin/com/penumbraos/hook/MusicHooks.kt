package com.penumbraos.hook

import android.util.Log

/**
 * Hooks for the music experience APK (package: humane.experience.music).
 *
 * Injection into this /system/app package (SELinux domain ironman_experience,
 * sandbox UID) is verified working on hardware. Modules:
 *  - TcmSilencer / ConnectivityCheckBypass: baseline experience-process cleanup.
 *  - EndpointTypeBypass: redirect the Tidal REST client to the local server.
 */
object MusicHooks {

    private const val TAG = "PenumbraHook"

    fun install(cl: ClassLoader) {
        Log.w(TAG, "=== MUSIC HOOKS INSTALLING ===")

        TcmSilencer.install(cl)

        ConnectivityCheckBypass.install(cl)

        TidalAuthBypass.install(cl)

        EndpointTypeBypass.install(cl)

        Log.w(TAG, "=== MUSIC HOOKS INSTALLED ===")
    }
}

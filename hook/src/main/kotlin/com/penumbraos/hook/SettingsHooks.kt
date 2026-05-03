package com.penumbraos.hook

import android.util.Log

/**
 * Hooks for the settings experience APK (package: humane.experience.settings).
 */
object SettingsHooks {

    private const val TAG = "PenumbraHook"

    fun install(cl: ClassLoader) {
        Log.i(TAG, "Installing settings hooks...")

        TcmSilencer.install(cl)
        ConnectivityCheckBypass.install(cl)

        Log.i(TAG, "Settings hooks installed")
    }
}

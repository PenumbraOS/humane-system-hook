package com.penumbraos.hook

import android.util.Log

/**
 * Hooks for the photography experience APK (package: humane.experience.photography).
 *
 * The photography process runs the upload pipeline (MemoryUploadWorkerImpl,
 * AssetUploadWorkerImpl) which encrypts data via DataProtectorWrapper before
 * sending it to the server. We install the encryption bypass hooks here so
 * thumbnails, locations, and file uploads arrive as plaintext.
 */
object PhotographyHooks {

    private const val TAG = "PenumbraHook"

    fun install(cl: ClassLoader) {
        Log.i(TAG, "Installing photography hooks...")

        ChannelFactoryBypass.install(cl)
        DataProtectorBypass.install(cl)

        Log.i(TAG, "Photography hooks installed")
    }
}

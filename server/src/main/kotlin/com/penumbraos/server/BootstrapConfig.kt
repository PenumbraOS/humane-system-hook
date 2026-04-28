package com.penumbraos.server

import android.content.Context
import android.util.Log
import java.io.File

object BootstrapConfig {

    private const val TAG = "PenumbraServer"
    private const val BOOTSTRAP_ASSET = "bootstrap-config.toml"
    private const val CONFIG_FILE_NAME = "config.toml"
    private const val MEDIA_DIR_NAME = "media"
    private const val DB_FILE_NAME = "penumbra.db"
    private const val STORAGE_MEDIA_PLACEHOLDER = "__APP_MEDIA_DIR__"
    private const val STORAGE_DB_PLACEHOLDER = "__APP_DB_PATH__"

    fun ensureCanonicalConfig(context: Context): String {
        val externalRoot = context.getExternalFilesDir(null)
            ?: throw IllegalStateException("External files dir unavailable")

        check(externalRoot.exists() || externalRoot.mkdirs()) {
            "Failed to create external files dir at ${externalRoot.absolutePath}"
        }

        val configFile = File(externalRoot, CONFIG_FILE_NAME)
        val mediaDir = File(externalRoot, MEDIA_DIR_NAME)
        val dbFile = File(externalRoot, DB_FILE_NAME)

        check(mediaDir.exists() || mediaDir.mkdirs()) {
            "Failed to create media dir at ${mediaDir.absolutePath}"
        }

        check(dbFile.parentFile?.exists() == true || dbFile.parentFile?.mkdirs() == true) {
            "Failed to create db parent dir at ${dbFile.parentFile?.absolutePath}"
        }

        Log.i(
            TAG,
            "Resolved external storage paths: " +
                "root=${externalRoot.absolutePath}, " +
                "config=${configFile.absolutePath}, " +
                "db=${dbFile.absolutePath}, " +
                "media=${mediaDir.absolutePath}",
        )

        if (configFile.exists()) {
            Log.i(TAG, "Using existing canonical config at ${configFile.absolutePath}")
            return configFile.absolutePath
        }

        val bootstrapToml = context.assets.open(BOOTSTRAP_ASSET).bufferedReader().use { it.readText() }
        val renderedToml = bootstrapToml
            .replace(STORAGE_MEDIA_PLACEHOLDER, mediaDir.absolutePath)
            .replace(STORAGE_DB_PLACEHOLDER, dbFile.absolutePath)

        configFile.writeText(renderedToml)
        Log.i(TAG, "Wrote canonical config to ${configFile.absolutePath}")
        return configFile.absolutePath
    }
}

package com.penumbraos.server

import android.content.Context
import android.os.Environment
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
    private const val PERSISTENT_ROOT_DIR_NAME = "PenumbraOS"

    fun ensureCanonicalConfig(context: Context): String {
        val externalRoot = File(Environment.getExternalStorageDirectory(), PERSISTENT_ROOT_DIR_NAME)

        check(externalRoot.exists() || externalRoot.mkdirs()) {
            "Failed to create persistent storage dir at ${externalRoot.absolutePath}"
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

    /** Best-effort extraction of advertised metadata from the config. */
    data class AdvertisedConfig(val displayName: String, val httpPort: Int)

    fun readAdvertisedConfig(configPath: String): AdvertisedConfig {
        val defaults = AdvertisedConfig(displayName = "Ai Pin", httpPort = 8080)
        val text = try {
            File(configPath).readText()
        } catch (t: Throwable) {
            Log.w(TAG, "Failed to read canonical config for advertisement", t)
            return defaults
        }

        // Tiny, intentionally-naive TOML scraper: we only need two scalars and
        // we control the file format. Comments and tables are tolerated;
        // multi-line strings, inline tables, and arrays of tables are not used here.
        var displayName: String? = null
        var httpPort: Int? = null

        for (rawLine in text.lineSequence()) {
            val line = rawLine.substringBefore('#').trim()
            if (line.isEmpty() || line.startsWith('[')) continue
            val eq = line.indexOf('=')
            if (eq <= 0) continue
            val key = line.substring(0, eq).trim()
            val value = line.substring(eq + 1).trim().trim('"', '\'')
            when (key) {
                "display_name" -> if (value.isNotEmpty()) displayName = value
                "http_bind_addr" -> {
                    val portStr = value.substringAfterLast(':', "")
                    portStr.toIntOrNull()?.let { httpPort = it }
                }
            }
        }

        return AdvertisedConfig(
            displayName = displayName ?: defaults.displayName,
            httpPort = httpPort ?: defaults.httpPort,
        )
    }
}

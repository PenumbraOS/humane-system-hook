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
    private const val LOG_DIR_NAME = "logs"
    private const val STORAGE_MEDIA_PLACEHOLDER = "__APP_MEDIA_DIR__"
    private const val STORAGE_DB_PLACEHOLDER = "__APP_DB_PATH__"
    private const val LOG_DIR_PLACEHOLDER = "__APP_LOG_DIR__"
    private const val PERSISTENT_ROOT_DIR_NAME = "PenumbraOS"

    fun ensureCanonicalConfig(context: Context): String {
        val externalRoot = File(Environment.getExternalStorageDirectory(), PERSISTENT_ROOT_DIR_NAME)

        check(externalRoot.exists() || externalRoot.mkdirs()) {
            "Failed to create persistent storage dir at ${externalRoot.absolutePath}"
        }

        val configFile = File(externalRoot, CONFIG_FILE_NAME)
        val mediaDir = File(externalRoot, MEDIA_DIR_NAME)
        val dbFile = File(externalRoot, DB_FILE_NAME)
        val logDir = File(externalRoot, LOG_DIR_NAME)

        check(mediaDir.exists() || mediaDir.mkdirs()) {
            "Failed to create media dir at ${mediaDir.absolutePath}"
        }

        check(dbFile.parentFile?.exists() == true || dbFile.parentFile?.mkdirs() == true) {
            "Failed to create db parent dir at ${dbFile.parentFile?.absolutePath}"
        }

        check(logDir.exists() || logDir.mkdirs()) {
            "Failed to create log dir at ${logDir.absolutePath}"
        }

        Log.i(
            TAG,
            "Resolved external storage paths: " +
                "root=${externalRoot.absolutePath}, " +
                "config=${configFile.absolutePath}, " +
                "db=${dbFile.absolutePath}, " +
                "media=${mediaDir.absolutePath}, " +
                "logs=${logDir.absolutePath}",
        )

        if (configFile.exists()) {
            Log.i(TAG, "Using existing canonical config at ${configFile.absolutePath}")
            applyAndroidManagedDefaults(
                configFile,
                managedFields(mediaDir, dbFile, logDir),
            )
            return configFile.absolutePath
        }

        val bootstrapToml = context.assets.open(BOOTSTRAP_ASSET).bufferedReader().use { it.readText() }
        val renderedToml = bootstrapToml
            .replace(STORAGE_MEDIA_PLACEHOLDER, mediaDir.absolutePath)
            .replace(STORAGE_DB_PLACEHOLDER, dbFile.absolutePath)
            .replace(LOG_DIR_PLACEHOLDER, logDir.absolutePath)

        configFile.writeText(renderedToml)
        Log.i(TAG, "Wrote canonical config to ${configFile.absolutePath}")
        return configFile.absolutePath
    }

    private data class ManagedField(val section: String, val key: String, val value: String)

    private fun managedFields(
        mediaDir: File,
        dbFile: File,
        logDir: File,
    ): List<ManagedField> = listOf(
        ManagedField("storage", "media_dir", mediaDir.absolutePath),
        ManagedField("storage", "db_path", dbFile.absolutePath),
        ManagedField("logging", "log_dir", logDir.absolutePath),
    )

    /**
     * Idempotent migration: ensures every Android-managed field exists in the
     * given config file. Missing fields are inserted at the top of their
     * section (creating the section if absent). Existing values are never
     * overwritten
     */
    private fun applyAndroidManagedDefaults(configFile: File, fields: List<ManagedField>) {
        val original = try {
            configFile.readText()
        } catch (t: Throwable) {
            Log.w(TAG, "Failed to read config for migration; skipping", t)
            return
        }

        var text = original
        val bySection = fields.groupBy { it.section }
        var addedAny = false

        for ((section, items) in bySection) {
            val (newText, changed) = ensureFieldsInSection(text, section, items)
            if (changed) {
                text = newText
                addedAny = true
            }
        }

        if (!addedAny) return

        try {
            val bak = File(configFile.parentFile, "${configFile.name}.bak")
            bak.writeText(original)
            configFile.writeText(text)
            Log.i(
                TAG,
                "Migrated ${configFile.absolutePath}: added missing Android-managed defaults",
            )
        } catch (t: Throwable) {
            Log.w(TAG, "Failed to persist migrated config", t)
        }
    }

    /**
     * Returns (newText, changed). Inserts any of `items` whose `key` is not
     * already present under `[section]`. If `[section]` is absent, appends a
     * fresh section at end of file.
     */
    private fun ensureFieldsInSection(
        text: String,
        section: String,
        items: List<ManagedField>,
    ): Pair<String, Boolean> {
        val sectionHeader = "[$section]"
        val lines = text.lines().toMutableList()

        val headerIdx = lines.indexOfFirst { it.trim() == sectionHeader }

        if (headerIdx == -1) {
            val sb = StringBuilder(text)
            if (text.isNotEmpty() && !text.endsWith("\n")) sb.append("\n")
            if (text.isNotEmpty() && !text.endsWith("\n\n")) sb.append("\n")
            sb.append("[").append(section).append("]\n")
            for (f in items) {
                sb.append(f.key).append(" = \"").append(escapeToml(f.value)).append("\"\n")
            }
            return sb.toString() to true
        }

        // Scope of this section: until next [header] or EOF.
        var endIdx = lines.size
        for (i in headerIdx + 1 until lines.size) {
            val t = lines[i].trim()
            if (t.startsWith("[") && t.endsWith("]")) {
                endIdx = i
                break
            }
        }

        val presentKeys = mutableSetOf<String>()
        for (i in headerIdx + 1 until endIdx) {
            val t = lines[i].substringBefore('#').trim()
            if (t.isEmpty()) continue
            val eq = t.indexOf('=')
            if (eq > 0) presentKeys += t.substring(0, eq).trim()
        }

        val missing = items.filterNot { it.key in presentKeys }
        if (missing.isEmpty()) return text to false

        val toInsert = missing.map { """${it.key} = "${escapeToml(it.value)}"""" }
        lines.addAll(headerIdx + 1, toInsert)
        return lines.joinToString("\n") to true
    }

    private fun escapeToml(value: String): String =
        value.replace("\\", "\\\\").replace("\"", "\\\"")

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

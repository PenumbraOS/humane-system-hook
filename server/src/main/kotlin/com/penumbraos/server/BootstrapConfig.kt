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
    private const val MEMORY_FILE_NAME = "assistant-memory.mv2"
    private const val STORAGE_MEDIA_PLACEHOLDER = "__APP_MEDIA_DIR__"
    private const val STORAGE_DB_PLACEHOLDER = "__APP_DB_PATH__"
    private const val LOG_DIR_PLACEHOLDER = "__APP_LOG_DIR__"
    private const val MEMORY_PATH_PLACEHOLDER = "__APP_MEMORY_PATH__"
    private const val PERSISTENT_ROOT_DIR_NAME = "PenumbraOS"
    private val LEGACY_DEFAULT_SYSTEM_PROMPT_LINES = listOf(
        "system_prompt = \"You are a helpful assistant running on a Humane AI Pin. Keep responses concise - they will be displayed on a laser projector and spoken aloud.\"",
    )
    private val LEGACY_DEFAULT_STATUS_PROMPT_LINES = listOf(
        "status_prompt = \"\"\"",
        "Current request status:",
        "- Current timestamp: {{current_timestamp}}",
        "- Current date: {{current_date}}",
        "- Current time: {{current_time}}",
        "{{#if location_name}}- User location: {{location_name}}{{else}}- User location: unknown",
        "{{/if}}{{#if coordinates}}- User coordinates: {{coordinates}}",
        "{{/if}}",
        "This status applies to the current user request only. If it conflicts with earlier conversation history, prefer this current status.",
        "\"\"\"",
    )

    fun ensurePersistentRoot(): File {
        val externalRoot = File(Environment.getExternalStorageDirectory(), PERSISTENT_ROOT_DIR_NAME)

        check(externalRoot.exists() || externalRoot.mkdirs()) {
            "Failed to create persistent storage dir at ${externalRoot.absolutePath}"
        }

        return externalRoot
    }

    fun ensureCanonicalConfig(context: Context): String {
        val externalRoot = ensurePersistentRoot()

        val configFile = File(externalRoot, CONFIG_FILE_NAME)
        val mediaDir = File(externalRoot, MEDIA_DIR_NAME)
        val dbFile = File(externalRoot, DB_FILE_NAME)
        val logDir = File(externalRoot, LOG_DIR_NAME)
        val memoryFile = File(externalRoot, MEMORY_FILE_NAME)

        check(mediaDir.exists() || mediaDir.mkdirs()) {
            "Failed to create media dir at ${mediaDir.absolutePath}"
        }

        check(dbFile.parentFile?.exists() == true || dbFile.parentFile?.mkdirs() == true) {
            "Failed to create db parent dir at ${dbFile.parentFile?.absolutePath}"
        }

        check(logDir.exists() || logDir.mkdirs()) {
            "Failed to create log dir at ${logDir.absolutePath}"
        }

        Log.w(
            TAG,
            "Resolved external storage paths: " +
                "root=${externalRoot.absolutePath}, " +
                "config=${configFile.absolutePath}, " +
                "db=${dbFile.absolutePath}, " +
                "media=${mediaDir.absolutePath}, " +
                "logs=${logDir.absolutePath}, " +
                "memory=${memoryFile.absolutePath}",
        )

        if (configFile.exists()) {
            Log.w(TAG, "Using existing canonical config at ${configFile.absolutePath}")
            applyAndroidConfigMigrations(
                configFile,
                managedFields(mediaDir, dbFile, logDir, memoryFile),
            )
            return configFile.absolutePath
        }

        val bootstrapToml = context.assets.open(BOOTSTRAP_ASSET).bufferedReader().use { it.readText() }
        val renderedToml = bootstrapToml
            .replace(STORAGE_MEDIA_PLACEHOLDER, mediaDir.absolutePath)
            .replace(STORAGE_DB_PLACEHOLDER, dbFile.absolutePath)
            .replace(LOG_DIR_PLACEHOLDER, logDir.absolutePath)
            .replace(MEMORY_PATH_PLACEHOLDER, memoryFile.absolutePath)

        configFile.writeText(renderedToml)
        Log.w(TAG, "Wrote canonical config to ${configFile.absolutePath}")
        return configFile.absolutePath
    }

    private data class ManagedField(val section: String, val key: String, val value: String)

    private data class SectionBounds(val headerIdx: Int, val endIdx: Int)

    private data class SectionEntry(val lineIdx: Int, val key: String, val value: String)

    private fun managedFields(
        mediaDir: File,
        dbFile: File,
        logDir: File,
        memoryFile: File,
    ): List<ManagedField> = listOf(
        ManagedField("storage", "media_dir", mediaDir.absolutePath),
        ManagedField("storage", "db_path", dbFile.absolutePath),
        ManagedField("logging", "log_dir", logDir.absolutePath),
        ManagedField("llm.memory", "path", memoryFile.absolutePath),
    )

    /**
     * Idempotent Android config migrations:
     * - ensures every Android-managed field exists in the given config file;
     * - removes legacy bootstrap prompt defaults when they exactly match the
     *   generated built-in defaults, so uncustomized users keep following app
     *   defaults.
     *
     * Existing custom values are never overwritten.
     */
    private fun applyAndroidConfigMigrations(configFile: File, fields: List<ManagedField>) {
        val original = try {
            configFile.readText()
        } catch (t: Throwable) {
            Log.w(TAG, "Failed to read config for migration; skipping", t)
            return
        }

        var text = original
        val bySection = fields.groupBy { it.section }
        var changedAny = false
        var addedManagedDefaults = false
        var removedLegacySystemPrompt = false
        var removedLegacyStatusPrompt = false

        for ((section, items) in bySection) {
            val (newText, changed) = ensureFieldsInSection(text, section, items)
            if (changed) {
                text = newText
                changedAny = true
                addedManagedDefaults = true
            }
        }

        val (textWithoutLegacySystemPrompt, removedSystemPrompt) =
            removeLegacyDefaultPrompt(text, LEGACY_DEFAULT_SYSTEM_PROMPT_LINES)
        if (removedSystemPrompt) {
            text = textWithoutLegacySystemPrompt
            changedAny = true
            removedLegacySystemPrompt = true
        }

        val (textWithoutLegacyStatusPrompt, removedStatusPrompt) =
            removeLegacyDefaultPrompt(text, LEGACY_DEFAULT_STATUS_PROMPT_LINES)
        if (removedStatusPrompt) {
            text = textWithoutLegacyStatusPrompt
            changedAny = true
            removedLegacyStatusPrompt = true
        }

        val (textWithMigratedWebSearchKey, migratedWebSearchKey) =
            migrateGeminiGoogleSearchKey(text)
        if (migratedWebSearchKey) {
            text = textWithMigratedWebSearchKey
            changedAny = true
        }

        if (!changedAny) return

        try {
            val bak = File(configFile.parentFile, "${configFile.name}.bak")
            bak.writeText(original)
            configFile.writeText(text)
            Log.w(
                TAG,
                "Migrated ${configFile.absolutePath}: " +
                    "addedManagedDefaults=$addedManagedDefaults, " +
                    "removedLegacySystemPrompt=$removedLegacySystemPrompt, " +
                    "removedLegacyStatusPrompt=$removedLegacyStatusPrompt, " +
                    "migratedWebSearchKey=$migratedWebSearchKey",
            )
        } catch (t: Throwable) {
            Log.w(TAG, "Failed to persist migrated config", t)
        }
    }

    /** Renames legacy `[llm].gemini_google_search` to `[llm].web_search`; never overwrites an existing `web_search`. */
    private fun migrateGeminiGoogleSearchKey(text: String): Pair<String, Boolean> {
        val lines = text.lines().toMutableList()
        val bounds = findSectionBounds(lines, "llm") ?: return text to false
        val entries = scanSectionEntries(lines, bounds)

        val legacy = entries.find { it.key == "gemini_google_search" } ?: return text to false
        val webSearchPresent = entries.any { it.key == "web_search" }

        lines.removeAt(legacy.lineIdx)
        if (!webSearchPresent) {
            lines.add(legacy.lineIdx, "web_search = ${legacy.value}")
        }

        return lines.joinToString("\n") to true
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
        val lines = text.lines().toMutableList()
        val bounds = findSectionBounds(lines, section)

        if (bounds == null) {
            val sb = StringBuilder(text)
            if (text.isNotEmpty() && !text.endsWith("\n")) sb.append("\n")
            if (text.isNotEmpty() && !text.endsWith("\n\n")) sb.append("\n")
            sb.append("[").append(section).append("]\n")
            for (f in items) {
                sb.append(f.key).append(" = \"").append(escapeToml(f.value)).append("\"\n")
            }
            return sb.toString() to true
        }

        val presentKeys = scanSectionEntries(lines, bounds).map { it.key }.toSet()
        val missing = items.filterNot { it.key in presentKeys }
        if (missing.isEmpty()) return text to false

        val toInsert = missing.map { """${it.key} = "${escapeToml(it.value)}"""" }
        lines.addAll(bounds.headerIdx + 1, toInsert)
        return lines.joinToString("\n") to true
    }

    private fun removeLegacyDefaultPrompt(
        text: String,
        expectedLines: List<String>,
    ): Pair<String, Boolean> {
        val lines = text.lines().toMutableList()
        val bounds = findSectionBounds(lines, "server") ?: return text to false
        val lastStart = bounds.endIdx - expectedLines.size

        if (lastStart < bounds.headerIdx + 1) return text to false

        for (i in bounds.headerIdx + 1..lastStart) {
            val candidate = lines.subList(i, i + expectedLines.size)
            if (candidate.map { it.trim() } == expectedLines) {
                repeat(expectedLines.size) { lines.removeAt(i) }
                return lines.joinToString("\n") to true
            }
        }

        return text to false
    }

    private fun findSectionBounds(lines: List<String>, section: String): SectionBounds? {
        val sectionHeader = "[$section]"
        val headerIdx = lines.indexOfFirst { it.trim() == sectionHeader }
        if (headerIdx == -1) return null

        var endIdx = lines.size
        for (i in headerIdx + 1 until lines.size) {
            val trimmed = lines[i].trim()
            if (trimmed.startsWith("[") && trimmed.endsWith("]")) {
                endIdx = i
                break
            }
        }

        return SectionBounds(headerIdx, endIdx)
    }

    /** Parses top-level `key = value` lines within a section, keeping each line's index for in-place edits. */
    private fun scanSectionEntries(lines: List<String>, bounds: SectionBounds): List<SectionEntry> {
        val entries = mutableListOf<SectionEntry>()
        for (i in bounds.headerIdx + 1 until bounds.endIdx) {
            val t = lines[i].substringBefore('#').trim()
            if (t.isEmpty()) continue
            val eq = t.indexOf('=')
            if (eq <= 0) continue
            entries += SectionEntry(i, t.substring(0, eq).trim(), t.substring(eq + 1).trim())
        }
        return entries
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

package com.penumbraos.hook

import android.content.Context
import android.os.Handler
import android.os.Looper
import android.util.Log
import java.io.File
import java.lang.reflect.Field
import java.lang.reflect.Proxy

/**
 * Dual-engine facade: routes Apple Music tracks through Apple's on-device
 * MusicKit `MediaPlayerController` while every other track stays on Humane's
 * ExoPlayer — without changing `MediaManager`, the queue, the UI, or the
 * assistant flow.
 *
 * The shim returns a sentinel playback URL (`penumbra-apple://<catalogId>`) for
 * Apple tracks. We hook Humane's `MediaPlayer` wrapper (the single seam
 * `MediaManager` talks to): when `setUri` sees the sentinel we enter "Apple
 * mode" and delegate play/pause/stop/seek/position/isPlaying to the MusicKit
 * controller, and we drive Humane's Rx publishers (`isPlaying`, `playNextItem`)
 * from the controller's `Listener` so `MediaManager` advances the queue and
 * updates state exactly as it would for ExoPlayer. Any non-sentinel URL flips
 * back to ExoPlayer.
 *
 * FairPlay is software-only in `libappleMusicSDK.so`, so this needs no Widevine.
 */
object MusicKitEngine {

    private const val TAG = "PenumbraHook"
    private const val SENTINEL = "penumbra-apple://"
    private const val NATIVE_LIB = "libappleMusicSDK.so"

    /**
     * Tokens the local server publishes for the player (developer + Music User
     * Token), next to its `config.toml`. Written by `music::write_device_tokens`
     * in server-rs. Preferred over the compiled-in [AppleTokens] fallback so a
     * Center sign-in / `.p8` config drives playback with no hardcoded token.
     */
    private const val APPLE_TOKENS_FILE = "/sdcard/PenumbraOS/apple_tokens.json"

    private const val MEDIA_PLAYER = "humane.experience.music.playback.MediaPlayer"
    private const val MEDIA_ITEM = "androidx.media3.common.MediaItem"
    private const val FACTORY = "com.apple.android.music.playback.controller.MediaPlayerControllerFactory"
    private const val TOKEN_PROVIDER = "com.apple.android.sdk.authentication.TokenProvider"
    private const val LISTENER = "com.apple.android.music.playback.controller.MediaPlayerController\$Listener"
    private const val QUEUE_BUILDER = "com.apple.android.music.playback.queue.CatalogPlaybackQueueItemProvider\$Builder"
    private const val QUEUE_PROVIDER = "com.apple.android.music.playback.queue.PlaybackQueueItemProvider"
    private const val MEDIA_ITEM_TYPE = "com.apple.android.music.playback.model.MediaItemType"

    private const val STATE_PLAYING = 1 // com.apple.android.music.playback.model.PlaybackState.PLAYING

    // If a track never finishes its initial buffering within this window it is
    // treated as non-streamable (some catalog tracks won't play via FairPlay)
    // and skipped so playback never hangs on a bad track.
    private const val BUFFER_TIMEOUT_MS = 8_000L

    private lateinit var cl: ClassLoader
    @Volatile private var appleMode = false
    @Volatile private var controller: Any? = null
    // Catalog id currently loaded into the controller. Humane calls setUri twice
    // per track (and again on every skip); without this guard each repeat fires a
    // fresh prepare() and the same song decodes on top of itself ("weird audio").
    @Volatile private var currentCatalogId: String? = null

    // Set when we ask the controller to stop (user next/stop/clear). The ended
    // single-item queue then spams onItemEnded; without this we'd echo each one
    // back to Humane's playNext publisher, which re-stops us — an infinite loop.
    @Volatile private var stopRequested = false
    // At most one playNext per loaded track, so a real end advances once.
    @Volatile private var endedForwarded = false

    // Watchdog that skips a track stuck in initial buffering. Created lazily on
    // the main looper (the process Looper isn't ready at class-init time).
    private val mainHandler by lazy { Handler(Looper.getMainLooper()) }
    @Volatile private var watchdog: Runnable? = null

    // Captured Humane MediaPlayer wrapper instance + its Rx subjects.
    @Volatile private var wrapper: Any? = null
    private var isPlayingSubject: Field? = null
    private var playNextSubject: Field? = null

    fun install(classLoader: ClassLoader) {
        cl = classLoader
        installReportingJobSuppressor()
        loadNativeLib()
        hookMediaPlayerWrapper()
        Log.w(TAG, "[musickit] dual-engine facade installed")
    }

    // ── wrapper hooks ────────────────────────────────────────────────

    private fun hookMediaPlayerWrapper() {
        val mp = try {
            cl.loadClass(MEDIA_PLAYER)
        } catch (t: Throwable) {
            Log.e(TAG, "[musickit] $MEDIA_PLAYER not found: ${t.message}"); return
        }

        // Every source funnels through the wrapper's two entry points: setUri
        // (initial "play") and setMediaItem (skip/next go straight here, NOT via
        // setUri). Hook both so no sentinel ever reaches ExoPlayer.
        HookUtils.hookMethodBefore(mp, "setUri", arrayOf(String::class.java)) { param ->
            captureWrapper(param.thisObject)
            if (routeSource(param.args[0] as? String)) param.result = null
        }
        val mediaItemClass = runCatching { cl.loadClass(MEDIA_ITEM) }.getOrNull()
        if (mediaItemClass != null) {
            HookUtils.hookMethodBefore(mp, "setMediaItem", arrayOf(mediaItemClass)) { param ->
                captureWrapper(param.thisObject)
                if (routeSource(uriOf(param.args[0]))) param.result = null
            }
        } else {
            Log.e(TAG, "[musickit] $MEDIA_ITEM not found; skip/next may leak to ExoPlayer")
        }

        // Delegate transport to the controller while in Apple mode.
        shortCircuitWhenApple(mp, "prepare") { /* already prepared in setUri */ }
        shortCircuitWhenApple(mp, "play") { invokeController("play") }
        shortCircuitWhenApple(mp, "pause") { invokeController("pause") }
        shortCircuitWhenApple(mp, "stop") { cancelWatchdog(); stopRequested = true; currentCatalogId = null; invokeController("stop") }
        shortCircuitWhenApple(mp, "clearMediaItems") { cancelWatchdog(); stopRequested = true; currentCatalogId = null; invokeController("stop") }

        HookUtils.hookMethodBefore(mp, "seekTo", arrayOf(Long::class.javaPrimitiveType!!)) { param ->
            if (appleMode) {
                invokeController("seekToPosition", Long::class.javaPrimitiveType!! to param.args[0])
                param.result = null
            }
        }
        HookUtils.hookMethodBefore(mp, "currentPosition", emptyArray()) { param ->
            if (appleMode) param.result = (invokeController("getCurrentPosition") as? Long) ?: 0L
        }
        HookUtils.hookMethodBefore(mp, "isPlaying", emptyArray()) { param ->
            if (appleMode) param.result = (invokeController("getPlaybackState") as? Int) == STATE_PLAYING
        }
    }

    /**
     * Decide what to do with a source URL set on the wrapper. Returns true when
     * it is an Apple sentinel and the caller should short-circuit ExoPlayer.
     */
    private fun routeSource(url: String?): Boolean {
        if (url != null && url.startsWith(SENTINEL)) {
            val catalogId = url.removePrefix(SENTINEL).substringBefore('?')
            appleMode = true
            if (catalogId == currentCatalogId) {
                // Humane sets the same source twice per track — ignore, or the song
                // re-prepares and overlaps itself ("weird audio").
                Log.w(TAG, "[musickit] source -> APPLE $catalogId (already loaded, ignored)")
            } else {
                Log.w(TAG, "[musickit] source -> APPLE $catalogId")
                startAppleTrack(catalogId)
            }
            return true
        }
        if (appleMode) {
            // Switching back to a non-Apple source: hand off to ExoPlayer.
            Log.w(TAG, "[musickit] source -> non-Apple, leaving Apple mode")
            cancelWatchdog()
            appleMode = false
            currentCatalogId = null
            invokeController("stop")
        }
        return false
    }

    /** Pull the source URL out of a media3 `MediaItem` (not minified: `localConfiguration.uri`). */
    private fun uriOf(mediaItem: Any?): String? = runCatching {
        val lc = mediaItem?.javaClass?.getField("localConfiguration")?.get(mediaItem) ?: return null
        lc.javaClass.getField("uri").get(lc)?.toString()
    }.getOrNull()

    private fun shortCircuitWhenApple(mp: Class<*>, name: String, action: () -> Unit) {
        HookUtils.hookMethodBefore(mp, name, emptyArray()) { param ->
            if (appleMode) {
                runCatching { action() }.onFailure { Log.e(TAG, "[musickit] $name delegate failed: ${it.message}") }
                param.result = null
            }
        }
    }

    private fun captureWrapper(instance: Any?) {
        if (wrapper != null || instance == null) return
        wrapper = instance
        runCatching {
            isPlayingSubject = instance.javaClass.getDeclaredField("isPlayingPublishSubject").apply { isAccessible = true }
            playNextSubject = instance.javaClass.getDeclaredField("playNextItemPublishSubject").apply { isAccessible = true }
        }.onFailure { Log.e(TAG, "[musickit] subject capture failed: ${it.message}") }
    }

    // ── Apple playback ───────────────────────────────────────────────

    private fun startAppleTrack(catalogId: String) {
        try {
            val controller = ensureController() ?: return
            // Tear down any queue already playing before loading a new track so
            // two decode pipelines never run at once.
            if (currentCatalogId != null) runCatching {
                controller.javaClass.getMethod("stop").invoke(controller)
            }
            currentCatalogId = catalogId
            stopRequested = false
            endedForwarded = false
            val songType = cl.loadClass(MEDIA_ITEM_TYPE).getField("SONG").getInt(null)
            val builder = cl.loadClass(QUEUE_BUILDER).getDeclaredConstructor().newInstance()
            builder.javaClass.getMethod("items", Int::class.javaPrimitiveType, Array<String>::class.java)
                .invoke(builder, songType, arrayOf(catalogId))
            val provider = builder.javaClass.getMethod("build").invoke(builder)
            controller.javaClass.getMethod("prepare", cl.loadClass(QUEUE_PROVIDER), Boolean::class.javaPrimitiveType)
                .invoke(controller, provider, true) // playWhenReady
            armWatchdog(catalogId)
        } catch (t: Throwable) {
            val cause = (t as? java.lang.reflect.InvocationTargetException)?.targetException ?: t
            Log.e(TAG, "[musickit] startAppleTrack failed: ${cause.javaClass.name}: ${cause.message}", cause)
        }
    }

    /**
     * Arm a one-shot skip for `catalogId`: if it hasn't finished buffering (i.e.
     * begun real playback) by [BUFFER_TIMEOUT_MS], advance the queue so a
     * non-streamable track never hangs playback. Cancelled by
     * `onBufferingStateChanged(false)`.
     */
    private fun armWatchdog(catalogId: String) {
        cancelWatchdog()
        val r = Runnable {
            runCatching {
                if (appleMode && catalogId == currentCatalogId) {
                    Log.w(TAG, "[musickit] track $catalogId stuck buffering; skipping")
                    endedForwarded = true
                    currentCatalogId = null
                    runCatching { invokeController("stop") }
                    pushSubject(playNextSubject, true)
                }
            }.onFailure { Log.e(TAG, "[musickit] watchdog failed: ${it.message}") }
        }
        watchdog = r
        runCatching { mainHandler.postDelayed(r, BUFFER_TIMEOUT_MS) }
    }

    private fun cancelWatchdog() {
        watchdog?.let { r -> runCatching { mainHandler.removeCallbacks(r) } }
        watchdog = null
    }

    /**
     * Read the developer + Music User Token the server published to
     * [APPLE_TOKENS_FILE]. Returns `(null, null)` if the file is missing or
     * unreadable so the caller falls back to the compiled-in tokens. Never
     * throws — a bad/absent file must not break playback.
     */
    private fun readPublishedTokens(): Pair<String?, String?> {
        return runCatching {
            val file = File(APPLE_TOKENS_FILE)
            if (!file.canRead()) return@runCatching Pair(null, null)
            val json = org.json.JSONObject(file.readText())
            val dev = json.optString("developer_token").takeIf { it.isNotBlank() }
            val user = json.optString("user_token").takeIf { it.isNotBlank() }
            Pair(dev, user)
        }.getOrElse {
            Log.w(TAG, "[musickit] could not read published tokens: ${it.message}")
            Pair(null, null)
        }
    }

    private fun tokenSource(fromFile: String?): String = if (fromFile != null) "server" else "builtin"

    @Synchronized
    private fun ensureController(): Any? {
        controller?.let { return it }
        val context = appContext() ?: run { Log.e(TAG, "[musickit] no context for controller"); return null }
        val tokenProviderClass = cl.loadClass(TOKEN_PROVIDER)
        // Prefer the server-published tokens; fall back to the compiled-in ones.
        val (fileDev, fileUser) = readPublishedTokens()
        val devToken = fileDev ?: AppleTokens.DEV_TOKEN
        val userToken = fileUser ?: AppleTokens.USER_TOKEN
        Log.w(TAG, "[musickit] tokens: developer=${tokenSource(fileDev)}, user=${tokenSource(fileUser)}")
        val tokenProvider = Proxy.newProxyInstance(cl, arrayOf(tokenProviderClass)) { _, m, _ ->
            when (m.name) {
                "getDeveloperToken" -> devToken
                "getUserToken" -> userToken
                else -> null
            }
        }
        val c = cl.loadClass(FACTORY)
            .getMethod("createLocalController", Context::class.java, tokenProviderClass)
            .invoke(null, context, tokenProvider)
        attachListener(c)
        controller = c
        Log.w(TAG, "[musickit] controller ready: ${c?.javaClass?.name}")
        return c
    }

    /** Bridge the controller's callbacks onto Humane's Rx publishers. */
    private fun attachListener(controller: Any?) {
        controller ?: return
        val listenerClass = cl.loadClass(LISTENER)
        val listener = Proxy.newProxyInstance(cl, arrayOf(listenerClass)) { _, m, args ->
            when (m.name) {
                "onPlaybackStateChanged" -> {
                    // (controller, oldState, newState)
                    val newState = (args?.getOrNull(2) as? Int) ?: -1
                    pushSubject(isPlayingSubject, newState == STATE_PLAYING)
                }
                "onBufferingStateChanged" -> {
                    // Buffering finished → real playback started → disarm the skip.
                    if (args?.getOrNull(1) == false) cancelWatchdog()
                }
                "onItemEnded" -> onItemEnded((args?.getOrNull(2) as? Number)?.toLong() ?: -1L)
                "onPlaybackError" -> Log.e(TAG, "[musickit] onPlaybackError: ${args?.getOrNull(1)}")
            }
            null
        }
        controller.javaClass.getMethod("addListener", listenerClass).invoke(controller, listener)
    }

    /**
     * A queue item ended. Separate a natural completion (advance Humane's queue
     * once) from a stop-induced end (Humane is already advancing — echoing it
     * would loop) and from the `-1` spam the ended single-item queue emits.
     */
    private fun onItemEnded(position: Long) {
        if (position < 0) return // spam from the ended/empty queue
        if (stopRequested) {
            // We initiated the stop (user next/stop); Humane drives the advance.
            stopRequested = false
            return
        }
        if (!endedForwarded) {
            endedForwarded = true
            currentCatalogId = null
            pushSubject(playNextSubject, true) // natural end: advance Humane's queue
        }
    }

    private fun pushSubject(field: Field?, value: Any) {
        val w = wrapper ?: return
        val f = field ?: return
        runCatching {
            val subject = f.get(w) ?: return
            subject.javaClass.getMethod("onNext", Any::class.java).invoke(subject, value)
        }.onFailure { Log.e(TAG, "[musickit] pushSubject failed: ${it.message}") }
    }

    private fun invokeController(name: String, arg: Pair<Class<*>, Any?>? = null): Any? {
        val c = controller ?: return null
        return if (arg == null) c.javaClass.getMethod(name).invoke(c)
        else c.javaClass.getMethod(name, arg.first).invoke(c, arg.second)
    }

    // ── boilerplate (native load, job suppressor, context) ───────────

    private fun loadNativeLib() {
        for (path in listOf(
            "/data/app/com.penumbraos.hook-injected/lib/arm64/$NATIVE_LIB",
            "/data/app/com.penumbraos.hook-injected/lib/arm64-v8a/$NATIVE_LIB",
        )) {
            if (File(path).exists()) {
                runCatching { System.load(path) }
                    .onSuccess { Log.w(TAG, "[musickit] loaded $path") }
                    .onFailure { Log.e(TAG, "[musickit] load failed $path: ${it.message}") }
                return
            }
        }
        Log.w(TAG, "[musickit] $NATIVE_LIB not found")
    }

    /**
     * The SDK reports telemetry via JobIntentService -> JobScheduler {schedule,
     * enqueue}, targeting a component that can't exist in the injected app —
     * which crashes the process. Swallow just that job.
     */
    private fun installReportingJobSuppressor() {
        val suppress: (de.robv.android.xposed.XC_MethodHook.MethodHookParam) -> Unit = { param ->
            try {
                val info = param.args.getOrNull(0)
                val service = info?.javaClass?.getMethod("getService")?.invoke(info)
                val className = service?.javaClass?.getMethod("getClassName")?.invoke(service) as? String
                if (className?.contains("com.apple.android.music.playback.reporting") == true) {
                    param.result = 1 // RESULT_SUCCESS
                }
            } catch (_: Throwable) { }
        }
        try {
            val js = Class.forName("android.app.JobSchedulerImpl")
            val jobInfo = Class.forName("android.app.job.JobInfo")
            val jobWorkItem = Class.forName("android.app.job.JobWorkItem")
            HookUtils.hookMethodBefore(js, "schedule", arrayOf(jobInfo), suppress)
            HookUtils.hookMethodBefore(js, "enqueue", arrayOf(jobInfo, jobWorkItem), suppress)
        } catch (t: Throwable) {
            Log.e(TAG, "[musickit] job suppressor failed: ${t.message}")
        }
    }

    private fun appContext(): Context? = runCatching {
        Class.forName("android.app.ActivityThread").getMethod("currentApplication").invoke(null) as? Context
    }.getOrNull()
}

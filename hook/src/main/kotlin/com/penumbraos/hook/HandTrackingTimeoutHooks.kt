package com.penumbraos.hook

import android.app.Application
import android.content.Context
import android.os.Handler
import android.os.Looper
import android.provider.Settings
import android.util.Log
import de.robv.android.xposed.XC_MethodHook
import de.robv.android.xposed.XposedBridge
import java.lang.reflect.Field
import java.lang.reflect.Method
import java.time.Instant
import java.util.UUID

/**
 * Replaces Humane's low-power hand-tracking lifetime policy.
 *
 * Humane's original HandTrackingManager uses indefinite "timer exceptions" for
 * narration, calls, music, and laser/projection state. In practice that can leave
 * the ToF/hand scanner searching for a hand long after the interaction ended.
 *
 * Amended policy:
 * - Only touchpad activity starts a tracking session from idle
 * - TTS keeps tracking alive for the duration of the narration
 * - Projection callbacks refresh a short timeout while a projected hand is present
 * - Music/call/alert/sound state does not start or hold hand tracking by default
 */
object HandTrackingTimeoutHooks {

    private const val TAG = "PenumbraHook"

    private const val MANAGER_CLASS = "humaneinternal.system.coordination.HandTrackingManager"
    private const val REASON_CLASS = "humaneinternal.system.coordination.HandTrackingManager\$Reason"
    private const val MAIN_APPLICATION_CLASS = "humaneinternal.system.MainApplication"
    private const val HAND_TRACKING_SERVICE_CLASS = "humaneinternal.system.coordination.HandTrackingService"
    private const val FLAT_HAND_CALLBACK_CLASS = "humaneinternal.system.hats.FlatHandService\$FlatHandCallback"
    private const val PROJECTABLE_POSITION_CLASS = "humane.handtracking.ProjectableFlatHandPosition"
    private const val ARBITRATOR_CLASS = "humaneinternal.system.tao.Arbitrator"

    private const val DEFAULT_TIMEOUT_MS = 10_000L
    private const val KEY_TIMEOUT_MS = "penumbra.hand_tracking.timeout_ms"
    private const val KEY_ALLOW_ALERT_START = "penumbra.hand_tracking.allow_alert_start"
    private const val KEY_ALLOW_SOUND_START = "penumbra.hand_tracking.allow_sound_start"

    @Volatile
    private var installed = false

    @Volatile
    private var sessionArmed = false

    @Volatile
    private var aiResponseActive = false

    @Volatile
    private var narrationActive = false

    @Volatile
    private var projectionActive = false

    @Volatile
    private var managerRef: Any? = null

    @Volatile
    private var appContext: Context? = null

    @Volatile
    private var runtimeInitialized = false

    @Volatile
    private var timerGeneration = 0

    private val timerHandler = Handler(Looper.getMainLooper())

    private lateinit var managerClass: Class<*>
    private lateinit var reasonClass: Class<*>
    private lateinit var sharedInstanceMethod: Method
    private lateinit var startIfNeededMethod: Method
    private lateinit var cancelTimerMethod: Method
    private lateinit var stopMethod: Method
    private lateinit var timerExceptionsField: Field
    private lateinit var contextField: Field
    private lateinit var timeoutIntentCounterField: Field
    private lateinit var lastPendingTimeoutField: Field
    private lateinit var isHandTrackingRunningField: Field
    private lateinit var handTrackingServiceClass: Class<*>

    fun install(cl: ClassLoader) {
        if (installed) return

        try {
            loadManagerSymbols(cl)
            hookApplicationContext(cl)
            hookUpdate()
            hookStop()
            hookProjectionCallbacks(cl)
            hookArbitratorActiveResponse(cl)
            installed = true
            Log.w(TAG, "  Hand tracking timeout hooks installed")
        } catch (t: Throwable) {
            Log.e(TAG, "  Failed to install hand tracking timeout hooks", t)
        }
    }

    private fun loadManagerSymbols(cl: ClassLoader) {
        managerClass = cl.loadClass(MANAGER_CLASS)
        reasonClass = cl.loadClass(REASON_CLASS)

        sharedInstanceMethod = managerClass.getDeclaredMethod("sharedInstance").apply { isAccessible = true }
        startIfNeededMethod = managerClass.getDeclaredMethod("startIfNeeded").apply { isAccessible = true }
        cancelTimerMethod = managerClass.getDeclaredMethod("cancelTimer").apply { isAccessible = true }
        stopMethod = managerClass.getDeclaredMethod("stop").apply { isAccessible = true }

        timerExceptionsField = managerClass.getDeclaredField("mTimerExceptions").apply { isAccessible = true }
        contextField = managerClass.getDeclaredField("mContext").apply { isAccessible = true }
        timeoutIntentCounterField = managerClass.getDeclaredField("mTimeoutIntentCounter").apply { isAccessible = true }
        lastPendingTimeoutField = managerClass.getDeclaredField("mLastPendingTimeout").apply { isAccessible = true }
        isHandTrackingRunningField = managerClass.getDeclaredField("mIsHandTrackingRunning").apply { isAccessible = true }
        handTrackingServiceClass = cl.loadClass(HAND_TRACKING_SERVICE_CLASS)
    }

    private fun hookApplicationContext(cl: ClassLoader) {
        try {
            val applicationClass = cl.loadClass(MAIN_APPLICATION_CLASS)
            val method = applicationClass.getDeclaredMethod("onCreate").apply { isAccessible = true }
            XposedBridge.hookMethod(method, object : XC_MethodHook() {
                override fun afterHookedMethod(param: MethodHookParam) {
                    val application = param.thisObject as? Application ?: return
                    appContext = application.applicationContext
                    Log.w(TAG, "  Captured application context for hand tracking timeout hook")
                }
            })
            Log.w(TAG, "  Hooked MainApplication.onCreate() for hand tracking context")
        } catch (t: Throwable) {
            Log.w(TAG, "  MainApplication.onCreate hand tracking context hook unavailable: ${t.message}")
        }
    }

    private fun hookUpdate() {
        val updateMethod = managerClass.getDeclaredMethod("update", reasonClass).apply { isAccessible = true }
        XposedBridge.hookMethod(updateMethod, object : XC_MethodHook() {
            override fun beforeHookedMethod(param: MethodHookParam) {
                val manager = param.thisObject ?: return
                val reason = (param.args.getOrNull(0) as? Enum<*>)?.name ?: return
                managerRef = manager

                try {
                    handleUpdate(manager, reason)
                } catch (t: Throwable) {
                    Log.e(TAG, "  HandTrackingManager.update($reason) hook failed", t)
                }

                // Suppress Humane's original indefinite timer-exception policy.
                param.result = null
            }
        })
        Log.w(TAG, "  Hooked HandTrackingManager.update(Reason)")
    }

    private fun hookStop() {
        XposedBridge.hookMethod(stopMethod, object : XC_MethodHook() {
            override fun afterHookedMethod(param: MethodHookParam) {
                resetHookState()
                val manager = param.thisObject
                if (manager != null) {
                    clearTimerExceptions(manager)
                }
                Log.w(TAG, "  Hand tracking session stopped; hook state cleared")
            }
        })
        Log.w(TAG, "  Hooked HandTrackingManager.stop()")
    }

    private fun hookProjectionCallbacks(cl: ClassLoader) {
        val callbackClass = try {
            cl.loadClass(FLAT_HAND_CALLBACK_CLASS)
        } catch (t: Throwable) {
            Log.w(TAG, "  $FLAT_HAND_CALLBACK_CLASS unavailable, projection refresh hooks skipped: ${t.message}")
            return
        }

        hookProjectionMethod(callbackClass, "onNewFlatHandProjection", emptyArray()) {
            handleProjectionStart()
        }
        hookProjectionMethod(callbackClass, "onFlatHandProjectionLost", emptyArray()) {
            handleProjectionLost()
        }

        try {
            val projectablePositionClass = cl.loadClass(PROJECTABLE_POSITION_CLASS)
            hookProjectionMethod(callbackClass, "onProjectableFlatHandDetected", arrayOf(projectablePositionClass)) {
                handleProjectionRefresh()
            }
        } catch (t: Throwable) {
            Log.w(TAG, "  Projectable projection refresh hook unavailable: ${t.message}")
        }
    }

    private fun hookArbitratorActiveResponse(cl: ClassLoader) {
        val arbitratorClass = try {
            cl.loadClass(ARBITRATOR_CLASS)
        } catch (t: Throwable) {
            Log.w(TAG, "  $ARBITRATOR_CLASS unavailable, AI response hold hooks skipped: ${t.message}")
            return
        }

        try {
            val method = arbitratorClass.getDeclaredMethod(
                "eventForTranscription",
                UUID::class.java,
                Instant::class.java,
                String::class.java,
                Boolean::class.javaPrimitiveType,
            ).apply { isAccessible = true }
            XposedBridge.hookMethod(method, object : XC_MethodHook() {
                override fun afterHookedMethod(param: MethodHookParam) {
                    val manager = managerForActiveSession() ?: return
                    val wasHeld = isActiveHold()
                    aiResponseActive = true
                    if (!wasHeld) {
                        cancelTimer(manager)
                        Log.w(TAG, "  Hand tracking timeout held for active AI response")
                    }
                }
            })
            Log.w(TAG, "  Hooked Arbitrator.eventForTranscription()")
        } catch (t: Throwable) {
            Log.w(TAG, "  Arbitrator.eventForTranscription hook unavailable: ${t.message}")
        }

        try {
            val method = arbitratorClass.getDeclaredMethod("clearInteractiveSession").apply { isAccessible = true }
            XposedBridge.hookMethod(method, object : XC_MethodHook() {
                override fun afterHookedMethod(param: MethodHookParam) {
                    val manager = managerForActiveSession() ?: return
                    aiResponseActive = false
                    narrationActive = false
                    if (!projectionActive) {
                        scheduleTimeout(manager, "interactive_session_clear")
                    }
                }
            })
            Log.w(TAG, "  Hooked Arbitrator.clearInteractiveSession()")
        } catch (t: Throwable) {
            Log.w(TAG, "  Arbitrator.clearInteractiveSession hook unavailable: ${t.message}")
        }
    }

    private fun hookProjectionMethod(
        clazz: Class<*>,
        methodName: String,
        paramTypes: Array<Class<*>>,
        handler: () -> Unit,
    ) {
        try {
            val method = clazz.getDeclaredMethod(methodName, *paramTypes).apply { isAccessible = true }
            XposedBridge.hookMethod(method, object : XC_MethodHook() {
                override fun afterHookedMethod(param: MethodHookParam) {
                    try {
                        handler()
                    } catch (t: Throwable) {
                        Log.e(TAG, "  Projection hook $methodName failed", t)
                    }
                }
            })
            Log.w(TAG, "  Hooked FlatHandCallback.$methodName()")
        } catch (t: Throwable) {
            Log.w(TAG, "  FlatHandCallback.$methodName hook unavailable: ${t.message}")
        }
    }

    private fun handleUpdate(manager: Any, reason: String) {
        ensureRuntimeInitialized(manager)
        clearTimerExceptions(manager)

        when (reason) {
            "TOUCHPAD" -> {
                sessionArmed = true
                aiResponseActive = false
                narrationActive = false
                projectionActive = false
                startIfNeededMethod.invoke(manager)
                scheduleTimeout(manager, "touchpad")
            }

            "NARRATION_START" -> {
                if (!sessionArmed) return
                aiResponseActive = true
                narrationActive = true
                cancelTimer(manager)
                Log.w(TAG, "  Hand tracking timeout held for narration")
            }

            "NARRATION_END" -> {
                if (!sessionArmed) return
                aiResponseActive = false
                narrationActive = false
                scheduleTimeout(manager, "narration_end")
            }

            "LASER_START" -> {
                if (!sessionArmed) return
                projectionActive = true
                if (!isActiveHold()) {
                    scheduleTimeout(manager, "projection_start")
                }
            }

            "LASER_END" -> {
                if (!sessionArmed) return
                projectionActive = false
                if (!isActiveHold()) {
                    scheduleTimeout(manager, "projection_lost")
                }
            }

            "ALERT" -> {
                if (allowAlertStart(manager)) {
                    sessionArmed = true
                    startIfNeededMethod.invoke(manager)
                    scheduleTimeout(manager, "alert")
                }
            }

            "SOUND" -> {
                if (allowSoundStart(manager)) {
                    sessionArmed = true
                    startIfNeededMethod.invoke(manager)
                    scheduleTimeout(manager, "sound")
                }
            }

            "CALL_START", "CALL_END", "MUSIC_START", "MUSIC_END" -> {
                // Intentionally ignored. Music/call UI interactions must be initiated by touchpad again.
                Log.w(TAG, "  Ignoring hand tracking reason $reason")
            }

            else -> {
                Log.w(TAG, "  Ignoring unknown hand tracking reason $reason")
            }
        }
    }

    private fun handleProjectionStart() {
        val manager = managerForActiveSession() ?: return
        projectionActive = true
        clearTimerExceptions(manager)
        if (!isActiveHold()) {
            scheduleTimeout(manager, "projection_callback_start")
        }
    }

    private fun handleProjectionRefresh() {
        val manager = managerForActiveSession() ?: return
        if (!projectionActive || isActiveHold()) return
        clearTimerExceptions(manager)
        scheduleTimeout(manager, "projection_callback_refresh")
    }

    private fun handleProjectionLost() {
        val manager = managerForActiveSession() ?: return
        projectionActive = false
        clearTimerExceptions(manager)
        if (!isActiveHold()) {
            scheduleTimeout(manager, "projection_callback_lost")
        }
    }

    private fun managerForActiveSession(): Any? {
        if (!sessionArmed) return null
        val manager = managerRef ?: runCatching { sharedInstanceMethod.invoke(null) }.getOrNull()?.also { managerRef = it }
        if (manager != null) {
            ensureRuntimeInitialized(manager)
        }
        return manager
    }

    private fun scheduleTimeout(manager: Any, reason: String) {
        ensureRuntimeInitialized(manager)
        clearTimerExceptions(manager)
        cancelTimer(manager)

        val timeoutMs = timeoutMs(configContext(manager))
        val generation = ++timerGeneration
        val targetManager = manager
        timerHandler.postDelayed({
            if (generation != timerGeneration || !sessionArmed || isActiveHold()) {
                return@postDelayed
            }
            try {
                clearTimerExceptions(targetManager)
                Log.w(TAG, "  Hand tracking timeout elapsed; stopping session ($reason)")
                stopMethod.invoke(targetManager)
            } catch (t: Throwable) {
                Log.e(TAG, "  Failed to stop hand tracking on timeout", t)
            }
        }, timeoutMs)

        Log.w(TAG, "  Scheduled hand tracking stop in ${timeoutMs}ms ($reason)")
    }

    private fun cancelTimer(manager: Any) {
        timerGeneration++
        timerHandler.removeCallbacksAndMessages(null)
        runCatching { cancelTimerMethod.invoke(manager) }
            .onFailure { Log.w(TAG, "  Failed to cancel Humane hand tracking timer: ${it.message}") }
        runCatching { lastPendingTimeoutField.set(manager, null) }
    }

    private fun clearTimerExceptions(manager: Any) {
        runCatching {
            val exceptions = timerExceptionsField.get(manager)
            if (exceptions is MutableList<*>) {
                exceptions.clear()
            } else if (exceptions is java.util.Collection<*>) {
                @Suppress("UNCHECKED_CAST")
                (exceptions as java.util.Collection<Any>).clear()
            }
        }.onFailure {
            Log.w(TAG, "  Failed to clear hand tracking timer exceptions: ${it.message}")
        }
    }

    private fun isActiveHold(): Boolean {
        return aiResponseActive || narrationActive
    }

    private fun ensureRuntimeInitialized(manager: Any) {
        val context = appContext ?: return
        runCatching {
            if (contextField.get(manager) == null) {
                contextField.set(manager, context)
                Log.w(TAG, "  Backfilled HandTrackingManager.mContext from application context")
            }
        }.onFailure {
            Log.w(TAG, "  Failed to backfill HandTrackingManager.mContext: ${it.message}")
        }

        if (runtimeInitialized) return
        runCatching {
            val sharedInstance = handTrackingServiceClass.getDeclaredMethod("sharedInstance").invoke(null)
            handTrackingServiceClass.getDeclaredMethod("initialize").invoke(sharedInstance)
            runtimeInitialized = true
            Log.w(TAG, "  Initialized HandTrackingService for timeout hook")
        }.onFailure {
            Log.w(TAG, "  Failed to initialize HandTrackingService for timeout hook: ${it.message}")
        }
    }

    private fun configContext(manager: Any): Context? {
        return (runCatching { contextField.get(manager) as? Context }.getOrNull()) ?: appContext
    }

    private fun timeoutMs(context: Context?): Long {
        val configured = if (context == null) {
            DEFAULT_TIMEOUT_MS
        } else {
            runCatching {
                Settings.Global.getLong(context.contentResolver, KEY_TIMEOUT_MS, DEFAULT_TIMEOUT_MS)
            }.getOrDefault(DEFAULT_TIMEOUT_MS)
        }
        return configured.coerceIn(1_000L, 60_000L)
    }

    private fun allowAlertStart(manager: Any): Boolean {
        val context = configContext(manager) ?: return false
        return runCatching {
            Settings.Global.getInt(context.contentResolver, KEY_ALLOW_ALERT_START, 0) != 0
        }.getOrDefault(false)
    }

    private fun allowSoundStart(manager: Any): Boolean {
        val context = configContext(manager) ?: return false
        return runCatching {
            Settings.Global.getInt(context.contentResolver, KEY_ALLOW_SOUND_START, 0) != 0
        }.getOrDefault(false)
    }

    private fun resetHookState() {
        sessionArmed = false
        aiResponseActive = false
        narrationActive = false
        projectionActive = false
        timerGeneration += 1
        timerHandler.removeCallbacksAndMessages(null)
    }
}

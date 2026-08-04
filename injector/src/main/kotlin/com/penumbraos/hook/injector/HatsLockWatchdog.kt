package com.penumbraos.hook.injector

import android.app.AlarmManager
import android.app.PendingIntent
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.os.Handler
import android.os.IBinder
import android.os.PowerManager
import android.os.SystemClock
import android.util.Log
import java.lang.reflect.Field
import java.util.concurrent.atomic.AtomicBoolean

/**
 * Bounds how long PowerManagerService's "User Activity" HATSLock can be held.
 *
 * PMS acquires a HATSLock on user activity and schedules its own release using SystemClock.uptimeMillis(), which does not advance
 * across suspend. When the device sleeps with the lock held, the release message
 * never becomes due, HATSManager's refcount never reaches 0, and its 8s doze
 * timeout can never fire, resulting in hand tracking staying in LPHT indefinitely
 */
object HatsLockWatchdog {

    private const val TAG = "PenumbraHatsWatchdog"
    private const val PROP_DISABLE = "debug.penumbra.hatswatchdog.disable"

    /** PowerManagerService.MSG_ACQUIRE_HATS_LOCK */
    private const val MSG_ACQUIRE_HATS_LOCK = 6

    /** PowerManagerService.MSG_HATS_TIMEOUT */
    private const val MSG_HATS_TIMEOUT = 7

    /**
     * Longest the lock may stay held while the device is non-interactive before
     * we force the release. Must be higher than PMS's deadline
     */
    private const val MAX_HOLD_MS = 60_000L

    /** Watchdog tick period, on the elapsed-realtime clock */
    private const val CHECK_INTERVAL_MS = 30_000L

    private const val ALARM_ACTION = "com.penumbraos.hook.HATS_LOCK_WATCHDOG"

    private val installed = AtomicBoolean(false)

    private var appContext: Context? = null
    private var powerManagerService: Any? = null
    private var pmsHandler: Handler? = null
    private var pmsHatsLockField: Field? = null

    /** elapsedRealtime when we first observed mHATSLock non-null; 0 means not held */
    @Volatile
    private var heldSinceElapsed = 0L

    fun install(context: Context) {
        if (!installed.compareAndSet(false, true)) return

        try {
            if (isDisabled()) {
                Log.w(TAG, "HATSLock watchdog DISABLED via $PROP_DISABLE")
                return
            }
            if (!resolvePmsReferences()) {
                Log.e(TAG, "Could not resolve PMS references, watchdog inactive")
                return
            }

            val ctx = context.applicationContext ?: context
            appContext = ctx

            val filter = IntentFilter().apply {
                addAction(Intent.ACTION_SCREEN_OFF)
                addAction(ALARM_ACTION)
            }
            ctx.registerReceiver(receiver, filter)
            scheduleNextCheck(ctx)

            Log.w(TAG, "HATSLock watchdog active (maxHold=${MAX_HOLD_MS}ms interval=${CHECK_INTERVAL_MS}ms)")
        } catch (error: Throwable) {
            Log.e(TAG, "HATSLock watchdog install failed; leaving PMS untouched", error)
        }
    }

    private val receiver = object : BroadcastReceiver() {
        override fun onReceive(context: Context, intent: Intent) {
            try {
                when (intent.action) {
                    // The deadline PMS passes to handleAcquireHATSLock is the
                    // user-activity timeout, the same as when the display turns
                    // off. This is the PMS intended turn off time
                    Intent.ACTION_SCREEN_OFF -> forceRelease("screen-off", checkInteractive = false)

                    ALARM_ACTION -> {
                        checkStaleHold()
                        appContext?.let { scheduleNextCheck(it) }
                    }
                }
            } catch (error: Throwable) {
                Log.e(TAG, "HATSLock watchdog tick failed", error)
            }
        }
    }

    private fun isHatsLockHeld(): Boolean {
        val pms = powerManagerService ?: return false
        val field = pmsHatsLockField ?: return false
        return runCatching { field.get(pms) }.getOrNull() != null
    }

    private fun checkStaleHold() {
        val now = SystemClock.elapsedRealtime()

        if (!isHatsLockHeld()) {
            heldSinceElapsed = 0L
            return
        }

        val since = heldSinceElapsed
        if (since == 0L) {
            heldSinceElapsed = now
            return
        }

        val heldMs = now - since
        if (heldMs >= MAX_HOLD_MS) {
            forceRelease("held ${heldMs}ms", checkInteractive = true)
        }
    }

    private fun forceRelease(reason: String, checkInteractive: Boolean) {
        val handler = pmsHandler ?: return

        if (!isHatsLockHeld()) {
            heldSinceElapsed = 0L
            return
        }

        if (checkInteractive && isInteractive()) {
            return;
        }

        Log.w(TAG, "Forcing HATSLock release ($reason)")
        heldSinceElapsed = 0L

        handler.post {
            try {
                handler.removeMessages(MSG_HATS_TIMEOUT)
                handler.removeMessages(MSG_ACQUIRE_HATS_LOCK)
                handler.sendMessage(
                    handler.obtainMessage(MSG_HATS_TIMEOUT).apply { isAsynchronous = true }
                )
            } catch (error: Throwable) {
                Log.e(TAG, "Failed to post MSG_HATS_TIMEOUT", error)
            }
        }
    }

    private fun isInteractive(): Boolean {
        val ctx = appContext ?: return false
        return try {
            (ctx.getSystemService(Context.POWER_SERVICE) as PowerManager).isInteractive
        } catch (error: Throwable) {
            Log.w(TAG, "Could not read interactive state, assuming interactive", error)
            true
        }
    }

    private fun scheduleNextCheck(context: Context) {
        try {
            val am = context.getSystemService(Context.ALARM_SERVICE) as AlarmManager
            val pendingIntent = PendingIntent.getBroadcast(
                context,
                0,
                Intent(ALARM_ACTION).setPackage(context.packageName),
                PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
            )
            am.set(
                AlarmManager.ELAPSED_REALTIME,
                SystemClock.elapsedRealtime() + CHECK_INTERVAL_MS,
                pendingIntent,
            )
        } catch (error: Throwable) {
            Log.e(TAG, "Failed to schedule watchdog tick", error)
        }
    }

    private fun resolvePmsReferences(): Boolean {
        val powerBinder = getService("power") ?: run {
            Log.e(TAG, "power service unavailable")
            return false
        }

        val pms = unwrapOuterOrFind(powerBinder, "com.android.server.power.PowerManagerService") ?: run {
            Log.e(TAG, "could not resolve PowerManagerService from power binder")
            return false
        }
        powerManagerService = pms

        pmsHandler = getFieldByName(pms, "mHandler") as? Handler ?: run {
            Log.e(TAG, "PowerManagerService.mHandler unavailable")
            return false
        }

        pmsHatsLockField = findFieldRecursive(pms.javaClass, "mHATSLock") ?: run {
            Log.e(TAG, "PowerManagerService.mHATSLock unavailable")
            return false
        }

        return true
    }

    private fun getService(name: String): IBinder? {
        val serviceManagerClass = Class.forName("android.os.ServiceManager")
        val getService = serviceManagerClass
            .getDeclaredMethod("getService", String::class.java)
            .also { it.isAccessible = true }
        return getService.invoke(null, name) as? IBinder
    }

    private fun unwrapOuterOrFind(instance: Any?, targetClassName: String): Any? {
        if (instance == null) return null
        if (instance.javaClass.name == targetClassName) return instance

        getFieldByName(instance, "this\$0")?.let { outer ->
            if (outer.javaClass.name == targetClassName) return outer
        }

        for (field in allFields(instance.javaClass)) {
            val value = runCatching { field.get(instance) }.getOrNull() ?: continue
            if (value.javaClass.name == targetClassName) return value
        }
        return null
    }

    private fun allFields(clazz: Class<*>): List<Field> {
        val fields = mutableListOf<Field>()
        var current: Class<*>? = clazz
        while (current != null) {
            for (field in current.declaredFields) {
                runCatching { field.isAccessible = true }
                fields += field
            }
            current = current.superclass
        }
        return fields
    }

    private fun findFieldRecursive(clazz: Class<*>, name: String): Field? {
        var current: Class<*>? = clazz
        while (current != null) {
            try {
                return current.getDeclaredField(name).also { it.isAccessible = true }
            } catch (_: NoSuchFieldException) {
                current = current.superclass
            } catch (error: Throwable) {
                Log.w(TAG, "field unavailable: ${clazz.name}.$name (${error.javaClass.simpleName}: ${error.message})")
                return null
            }
        }
        return null
    }

    private fun getFieldByName(instance: Any?, name: String): Any? {
        if (instance == null) return null
        val field = findFieldRecursive(instance.javaClass, name) ?: return null
        return runCatching { field.get(instance) }.getOrNull()
    }

    private fun isDisabled(): Boolean {
        return try {
            val sysPropClass = Class.forName("android.os.SystemProperties")
            val getMethod = sysPropClass.getDeclaredMethod("get", String::class.java, String::class.java)
            getMethod.invoke(null, PROP_DISABLE, "0") == "1"
        } catch (_: Throwable) {
            false
        }
    }
}

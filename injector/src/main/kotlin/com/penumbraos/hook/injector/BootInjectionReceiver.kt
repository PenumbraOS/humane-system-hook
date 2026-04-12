package com.penumbraos.hook.injector

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.util.Log

/**
 * Automatically injects hooks into all target packages at boot.
 * 
 * Events should fire from within `system_server` before any targets start.
 */
class BootInjectionReceiver : BroadcastReceiver() {

    companion object {
        private const val TAG = "PenumbraInjector"

        /**
         * System property to disable auto-injection at boot.
         * Set via: adb shell setprop debug.penumbra.disable 1
         */
        private const val PROP_DISABLE = "debug.penumbra.disable"

        /**
         * All packages to inject into at boot. Each must have a matching
         * hook module registered in HookComponentFactory.HOOK_MODULES.
         */
        private val TARGET_PACKAGES = listOf(
            "hu.ma.ne.ironman",
            "humane.experience.onboarding",
            "humane.experience.photography",
            "hu.ma.ne.krypto",
        )

        /**
         * Tracks which packages we've already injected this boot cycle.
         * Prevents duplicate injections.
         */
        private val injectedPackages = mutableSetOf<String>()
    }

    override fun onReceive(context: Context, intent: Intent) {
        try {
            handleBoot(context, intent)
        } catch (error: Throwable) {
            // CRITICAL: never let an exception escape onReceive in system_server.
            Log.e(TAG, "BootInjectionReceiver.onReceive failed", error)
        }
    }

    private fun handleBoot(context: Context, intent: Intent) {
        val action = intent.action ?: return

        if (action != Intent.ACTION_LOCKED_BOOT_COMPLETED &&
            action != Intent.ACTION_BOOT_COMPLETED) {
            return
        }

        // Check kill switch
        if (isDisabled()) {
            Log.i(TAG, "Boot injection DISABLED via $PROP_DISABLE")
            return
        }

        Log.i(TAG, "Boot injection triggered by $action")

        // Initialize PMS references
        PackageInjector.ensureInitialized()
        if (!PackageInjector.isInitialized) {
            Log.e(TAG, "PackageInjector failed to initialize, skipping boot injection")
            return
        }

        val isBootCompleted = action == Intent.ACTION_BOOT_COMPLETED

        for (packageName in TARGET_PACKAGES) {
            try {
                injectPackage(context, packageName, isBootCompleted)
            } catch (error: Throwable) {
                Log.e(TAG, "Failed to inject $packageName", error)
            }
        }

        Log.i(TAG, "Boot injection complete")
    }

    /**
     * Inject a single target package. On BOOT_COMPLETED,
     * also force-stop and relaunch if the target was already running unhooked.
     */
    private fun injectPackage(context: Context, packageName: String, forceRestart: Boolean) {
        if (packageName in injectedPackages) {
            Log.i(TAG, "Already injected $packageName this boot, skipping")
            return
        }

        val success = PackageInjector.inject(packageName)
        if (!success) {
            Log.w(TAG, "Injection returned false for $packageName (may not be installed)")
            return
        }

        injectedPackages.add(packageName)
        Log.i(TAG, "Injected $packageName")

        // On BOOT_COMPLETED: targets may already be running from a LOCKED_BOOT_COMPLETED
        // launch that happened before our injection. Force-stop + relaunch to pick up hooks.
        // On LOCKED_BOOT_COMPLETED: targets haven't started yet, no restart needed.
        if (forceRestart) {
            Log.i(TAG, "Force-restarting $packageName to pick up hooks after BOOT_COMPLETED")
            Thread {
                try {
                    forceStopAndRelaunch(context, packageName)
                } catch (error: Throwable) {
                    Log.e(TAG, "Relaunch failed for $packageName", error)
                }
            }.start()
        }
    }

    private fun forceStopAndRelaunch(context: Context, packageName: String) {
        val am = context.getSystemService(Context.ACTIVITY_SERVICE) as android.app.ActivityManager
        val forceStop = am.javaClass.getDeclaredMethod(
            "forceStopPackage", String::class.java
        )
        forceStop.invoke(am, packageName)

        Thread.sleep(500)

        val launchIntent = context.packageManager.getLaunchIntentForPackage(packageName)
        if (launchIntent != null) {
            launchIntent.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
            context.startActivity(launchIntent)
            Log.i(TAG, "Relaunched $packageName via launch intent")
        } else {
            Log.i(TAG, "No launch intent for $packageName, relying on system auto-restart")
        }
    }

    /**
     * Check the debug.penumbra.disable system property via reflection.
     * SystemProperties is on the boot classpath but hidden from the SDK.
     */
    private fun isDisabled(): Boolean {
        return try {
            val sysPropClass = Class.forName("android.os.SystemProperties")
            val getMethod = sysPropClass.getDeclaredMethod("get", String::class.java, String::class.java)
            val value = getMethod.invoke(null, PROP_DISABLE, "") as String
            value == "1"
        } catch (t: Throwable) {
            Log.w(TAG, "Failed to read $PROP_DISABLE, assuming not disabled", t)
            false
        }
    }
}

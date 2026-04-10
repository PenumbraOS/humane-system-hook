package com.penumbraos.hook.injector

import android.util.Log
import java.io.File

/**
 * Directly mutates PMS's in-memory data structures to inject our hook APK
 * into a target package's classpath and override its appComponentFactory.
 *
 * Runs inside system_server (UID 1000). No native code, no AMS hooks,
 * no binder wrapping — pure reflection into PMS's live objects.
 *
 * How it works:
 *   PMS parses all installed packages at boot. The parsed data lives in two maps:
 *     - PMS.mPackages: WatchedArrayMap<String, AndroidPackage>
 *       AndroidPackage is actually ParsingPackageImpl which has:
 *         - setAppComponentFactory(String) — public method (line 2723)
 *     - PMS.mSettings.mPackages: WatchedArrayMap<String, PackageSetting>
 *       PackageSetting has:
 *         - getPkgState() — public, returns PackageStateUnserialized (line 436)
 *         - PackageStateUnserialized.setUsesLibraryFiles(List<String>) — public (line 187)
 *
 *   PMS's snapshot mechanism (ComputerTracker/ComputerEngine) does shallow copies.
 *   AndroidPackage and PackageStateUnserialized objects are shared by reference
 *   between live and snapshot paths. Mutations are immediately visible to both.
 *
 *   Reversible: reboot re-parses from APK on disk, restoring original values.
 *
 * Safety:
 *   - All reflection is wrapped in try/catch. Failures are logged and swallowed.
 *   - init() failure means inject() becomes a no-op.
 *   - If hook APK not found, injection is aborted cleanly.
 *   - system_server never crashes from our code.
 *
 * Android 12L (API 32) AOSP android-12.1.0_r1.
 */
object PackageInjector {

    private const val TAG = "PenumbraInjector"
    private const val HOOK_APK_PACKAGE = "com.penumbraos.hook"
    private const val HOOK_FACTORY_CLASS = "com.penumbraos.hook.HookComponentFactory"

    @Volatile
    var isInitialized = false
        private set

    private var initAttempted = false

    // Cached references from init()
    private var pmsPackages: Any? = null      // WatchedArrayMap<String, AndroidPackage>
    private var settingsPackages: Any? = null  // WatchedArrayMap<String, PackageSetting>
    private var mapGetMethod: java.lang.reflect.Method? = null  // WatchedArrayMap.get(Object)

    @Synchronized
    fun ensureInitialized() {
        if (initAttempted) return
        initAttempted = true

        try {
            init()
            isInitialized = true
            Log.i(TAG, "PackageInjector initialized")
        } catch (t: Throwable) {
            Log.e(TAG, "PackageInjector initialization FAILED", t)
        }
    }

    /**
     * Inject the hook APK into the target package's PMS data.
     *
     * After calling this, the NEXT launch of [packageName] will:
     *   1. Have our hook APK on its classpath (via sharedLibraryFiles)
     *   2. Use HookComponentFactory as its appComponentFactory
     *
     * The caller is responsible for force-stopping and relaunching the target.
     *
     * Returns true if injection was configured successfully.
     */
    fun inject(packageName: String): Boolean {
        if (!isInitialized) {
            Log.e(TAG, "Not initialized, cannot inject")
            return false
        }

        try {
            return doInject(packageName)
        } catch (t: Throwable) {
            Log.e(TAG, "inject($packageName) failed", t)
            return false
        }
    }

    // -----------------------------------------------------------------------
    // Initialization
    // -----------------------------------------------------------------------

    private fun init() {
        // In Android 12L, PMS extends IPackageManager.Stub directly.
        // ServiceManager.getService("package") returns the PMS Binder object itself
        // (not a proxy) when called from within system_server.
        //
        // ServiceManager is on the boot classpath (framework.jar), so our APK's
        // classloader can find it without any special classloader tricks.
        val serviceManager = Class.forName("android.os.ServiceManager")
        val getService = serviceManager.getDeclaredMethod("getService", String::class.java)
        val pms = getService.invoke(null, "package")
            ?: throw RuntimeException("ServiceManager.getService('package') returned null")
        Log.i(TAG, "Got PMS: ${pms.javaClass.name}")

        // PMS.mPackages — WatchedArrayMap<String, AndroidPackage> (line 873)
        val mPackagesField = pms.javaClass.getDeclaredField("mPackages")
        mPackagesField.isAccessible = true
        pmsPackages = mPackagesField.get(pms)
            ?: throw RuntimeException("PMS.mPackages is null")
        Log.i(TAG, "Got PMS.mPackages: ${pmsPackages!!.javaClass.name}")

        // PMS.mSettings — Settings (line 923)
        val mSettingsField = pms.javaClass.getDeclaredField("mSettings")
        mSettingsField.isAccessible = true
        val settings = mSettingsField.get(pms)
            ?: throw RuntimeException("PMS.mSettings is null")

        // Settings.mPackages — WatchedArrayMap<String, PackageSetting> (line 368)
        val settingsMPackagesField = settings.javaClass.getDeclaredField("mPackages")
        settingsMPackagesField.isAccessible = true
        settingsPackages = settingsMPackagesField.get(settings)
            ?: throw RuntimeException("Settings.mPackages is null")
        Log.i(TAG, "Got Settings.mPackages: ${settingsPackages!!.javaClass.name}")

        // Cache the get() method on WatchedArrayMap (extends ArrayMap which has get(Object))
        mapGetMethod = pmsPackages!!.javaClass.getMethod("get", Any::class.java)
        mapGetMethod!!.isAccessible = true

        Log.i(TAG, "PackageInjector init complete")
    }

    // -----------------------------------------------------------------------
    // Injection
    // -----------------------------------------------------------------------

    private fun doInject(packageName: String): Boolean {
        val hookApkPath = findHookApkPath()
        if (hookApkPath == null) {
            Log.e(TAG, "Hook APK not found on disk, aborting injection")
            return false
        }

        Log.i(TAG, "=== INJECTING into $packageName ===")
        Log.i(TAG, "  Hook APK: $hookApkPath")

        // 1. Mutate AndroidPackage (ParsingPackageImpl) — set appComponentFactory
        val androidPackage = mapGetMethod!!.invoke(pmsPackages, packageName)
        if (androidPackage == null) {
            Log.e(TAG, "  Package '$packageName' not found in PMS.mPackages")
            return false
        }
        Log.i(TAG, "  AndroidPackage class: ${androidPackage.javaClass.name}")

        // Save original appComponentFactory for logging
        val getFactory = androidPackage.javaClass.getMethod("getAppComponentFactory")
        val originalFactory = getFactory.invoke(androidPackage) as? String ?: ""
        Log.i(TAG, "  Original appComponentFactory: '$originalFactory'")

        // Set our factory
        val setFactory = androidPackage.javaClass.getMethod(
            "setAppComponentFactory", String::class.java
        )
        setFactory.invoke(androidPackage, HOOK_FACTORY_CLASS)
        Log.i(TAG, "  Set appComponentFactory -> $HOOK_FACTORY_CLASS")

        // 2. Mutate PackageSetting — add hook APK to usesLibraryFiles
        val packageSetting = mapGetMethod!!.invoke(settingsPackages, packageName)
        if (packageSetting == null) {
            Log.e(TAG, "  Package '$packageName' not found in Settings.mPackages")
            // appComponentFactory was already set but without the APK on classpath
            // HookComponentFactory won't be found — the app will crash.
            // Revert appComponentFactory.
            setFactory.invoke(androidPackage, originalFactory)
            Log.e(TAG, "  Reverted appComponentFactory to original")
            return false
        }

        // PackageSetting.getPkgState() — public method returning PackageStateUnserialized
        val getPkgState = packageSetting.javaClass.getMethod("getPkgState")
        val pkgState = getPkgState.invoke(packageSetting)
            ?: throw RuntimeException("PackageSetting.getPkgState() returned null")

        // PackageStateUnserialized.getUsesLibraryFiles() — returns List<String>
        val getLibFiles = pkgState.javaClass.getMethod("getUsesLibraryFiles")
        @Suppress("UNCHECKED_CAST")
        val existingLibFiles = getLibFiles.invoke(pkgState) as List<String>

        // Only add if not already present
        if (hookApkPath !in existingLibFiles) {
            val newLibFiles = ArrayList(existingLibFiles)
            newLibFiles.add(hookApkPath)

            // PackageStateUnserialized.setUsesLibraryFiles(List<String>) — public
            val setLibFiles = pkgState.javaClass.getMethod(
                "setUsesLibraryFiles", List::class.java
            )
            setLibFiles.invoke(pkgState, newLibFiles)
            Log.i(TAG, "  Added to usesLibraryFiles: $hookApkPath")
            Log.i(TAG, "  Full usesLibraryFiles: $newLibFiles")
        } else {
            Log.i(TAG, "  Hook APK already in usesLibraryFiles")
        }

        Log.i(TAG, "=== Injection configured for $packageName ===")
        Log.i(TAG, "  Force-stop and relaunch the target to activate.")
        return true
    }

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    private fun findHookApkPath(): String? {
        val primary = File("/data/app/$HOOK_APK_PACKAGE-injected/base.apk")
        if (primary.exists()) return primary.absolutePath

        // Fallback scan
        val dataApp = File("/data/app")
        val dirs = dataApp.listFiles() ?: return null
        for (dir in dirs) {
            if (dir.name.startsWith(HOOK_APK_PACKAGE) && dir.isDirectory) {
                val apk = File(dir, "base.apk")
                if (apk.exists()) return apk.absolutePath
            }
        }
        return null
    }
}

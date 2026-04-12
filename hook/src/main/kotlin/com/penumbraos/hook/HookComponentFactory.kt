package com.penumbraos.hook

import android.app.AppComponentFactory
import android.app.Application
import android.util.Log
import java.io.File

/**
 * Entry point for hook code running inside the target process.
 *
 * The injector sets this as the target's appComponentFactory. Android instantiates
 * this class and calls instantiateApplication() before Application.attachBaseContext().
 *
 * Hook modules are registered in [HOOK_MODULES]. Each module declares a "probe" class
 * that must exist on the target's classloader for the module to activate. This allows
 * a single hook APK to be injected into multiple target APKs
 *
 * All hook initialization is wrapped in try/catch. If anything fails, the original
 * AppComponentFactory is delegated to and the app starts normally (unhooked).
 */
class HookComponentFactory : AppComponentFactory() {

    companion object {
        const val TAG = "PenumbraHook"
        const val HOOK_PACKAGE = "com.penumbraos.hook"

        /**
         * Registry of hook modules. Each entry is a probe class name and an
         * install function. The probe class is loaded from the target's
         * classloader — if it exists, that module's install() runs.
         *
         * Multiple modules can match the same process.
         */
        private val HOOK_MODULES: List<Pair<String, (ClassLoader) -> Unit>> = listOf(
            "humaneinternal.system.MainApplication" to IronmanHooks::install,
            "humane.experience.onboarding.OnboardingExperience" to OnboardingHooks::install,
            "system.PhotographyExperienceApplication" to PhotographyHooks::install,
            "humaneinternal.system.krypto.KryptoService" to KryptoHooks::install,
        )
    }

    override fun instantiateApplication(cl: ClassLoader, className: String): Application {
        Log.i(TAG, "HookComponentFactory.instantiateApplication()")
        Log.i(TAG, "  className=$className")
        Log.i(TAG, "  classLoader=$cl")
        Log.i(TAG, "  process=${android.os.Process.myPid()}")

        try {
            loadNativeLibs()
            installMatchingModules(cl)
        } catch (t: Throwable) {
            Log.e(TAG, "Hook init failed, continuing without hooks", t)
        }

        return createApplication(cl, className)
    }

    /**
     * Probe the target classloader and install every matching hook module.
     */
    private fun installMatchingModules(cl: ClassLoader) {
        var installed = 0
        for ((probeClass, install) in HOOK_MODULES) {
            try {
                cl.loadClass(probeClass)
            } catch (_: ClassNotFoundException) {
                continue
            }
            Log.i(TAG, "  Module matched: $probeClass")
            try {
                install(cl)
                installed++
            } catch (t: Throwable) {
                Log.e(TAG, "  Module install failed for $probeClass", t)
            }
        }
        if (installed == 0) {
            Log.w(TAG, "  No hook modules matched this process")
        } else {
            Log.i(TAG, "  $installed hook module(s) installed")
        }
    }

    /**
     * Load native libraries by absolute path.
     *
     * We're running inside the target process, so System.loadLibrary() would search
     * the target's nativeLibraryDir, not ours. We find our own APK's extracted lib
     * directory and load explicitly.
     *
     * Load order:
     * 1. AliuHook libs (libc++_shared, liblsplant, libaliuhook) — in dependency order
     * 2. Frida Gadget — starts a listen server on port 27042 for interactive exploration.
     *    Config file (libfrida-gadget.config.so) must be extracted alongside it;
     *    it sets on_load=resume so ironman is not blocked at startup.
     */
    private fun loadNativeLibs() {
        val libDir = findHookNativeLibDir()
        if (libDir == null) {
            Log.w(TAG, "Could not find hook APK native lib directory, AliuHook may fail")
            return
        }

        Log.i(TAG, "Loading native libs from: $libDir")

        // AliuHook native libs — in dependency order
        val aliuHookLibs = listOf("libc++_shared.so", "liblsplant.so", "libaliuhook.so")
        for (libName in aliuHookLibs) {
            val libFile = File(libDir, libName)
            if (libFile.exists()) {
                System.load(libFile.absolutePath)
                Log.i(TAG, "  Loaded $libName")
            } else {
                Log.w(TAG, "  $libName not found in $libDir")
            }
        }

        // Frida Gadget — loaded AFTER AliuHook so method hooks are already installed.
        // On load, Gadget reads libfrida-gadget.config.so from the same directory
        // and starts a listen server (127.0.0.1:27042, on_load=resume).
        val fridaLib = File(libDir, "libfrida-gadget.so")
        if (fridaLib.exists()) {
            try {
                System.load(fridaLib.absolutePath)
                Log.i(TAG, "  Loaded libfrida-gadget.so — Frida listen server starting on :27042")
            } catch (t: Throwable) {
                Log.e(TAG, "  Failed to load Frida Gadget (non-fatal, continuing)", t)
            }
        } else {
            Log.w(TAG, "  libfrida-gadget.so not found in $libDir (Frida disabled)")
        }
    }

    /**
     * Locate our APK's extracted native library directory.
     *
     * system-injector installs APKs to /data/app/<package>-injected/base.apk
     * and Android extracts native libs to /data/app/<package>-injected/lib/arm64/
     */
    private fun findHookNativeLibDir(): File? {
        val dataApp = File("/data/app")
        if (!dataApp.isDirectory) return null

        // Primary: exact path from system-injector's naming convention
        val injectedDir = File(dataApp, "$HOOK_PACKAGE-injected")
        if (injectedDir.isDirectory) {
            val found = findLibSubdir(injectedDir)
            if (found != null) return found
        }

        // Fallback: scan /data/app/ for any directory starting with our package name
        val dirs = dataApp.listFiles() ?: return null
        for (dir in dirs) {
            if (dir.name.startsWith(HOOK_PACKAGE) && dir.isDirectory) {
                val found = findLibSubdir(dir)
                if (found != null) return found
            }
        }

        return null
    }

    private fun findLibSubdir(appDir: File): File? {
        for (subdir in listOf("lib/arm64", "lib/arm64-v8a")) {
            val candidate = File(appDir, subdir)
            if (candidate.isDirectory && (candidate.listFiles()?.isNotEmpty() == true)) {
                return candidate
            }
        }
        return null
    }

    /**
     * Instantiate the Application class directly.
     *
     * The original appComponentFactory (androidx.core.app.CoreComponentFactory) just
     * does the same thing — calls cl.loadClass(className).newInstance(). No need to
     * delegate through it.
     */
    private fun createApplication(cl: ClassLoader, className: String): Application {
        Log.i(TAG, "Instantiating $className")
        return cl.loadClass(className).getDeclaredConstructor().newInstance() as Application
    }
}

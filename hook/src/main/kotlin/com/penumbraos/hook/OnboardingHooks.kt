package com.penumbraos.hook

import android.util.Log

/**
 * Hooks for the humane_onboarding APK (package: humane.experience.onboarding).
 *
 * The OPAQUE PAKE protocol runs client-side inside this APK, not in ironman.
 * We hook the native JNI methods in hu.ma.ne.Opaque to bypass real crypto
 * and return deterministic values that our mock server expects.
 *
 * Probe class: humane.experience.onboarding.OnboardingExperience
 */
object OnboardingHooks {

    private const val TAG = "PenumbraHook"

    /**
     * Fixed 32-byte session key for OPAQUE bypass. The mock server must use
     * this same key for AES-256-GCM encryption of the DUC certificate in
     * CreateDeviceUserBinding responses.
     */
    private val OPAQUE_SESSION_KEY = ByteArray(32) { 0x42 }

    fun install(cl: ClassLoader) {
        Log.i(TAG, "Installing onboarding hooks...")

        hookOpaque(cl)

        ConnectivityCheckBypass.install(cl)

        Log.i(TAG, "Onboarding hooks installed")
    }

    /**
     * Hook the OPAQUE native methods in hu.ma.ne.Opaque to bypass the real
     * OPAQUE PAKE protocol. This allows onboarding to complete against our
     * mock server without needing a real OPAQUE server registration.
     *
     * The native methods are replaced with deterministic stubs:
     * - clientLoginNew() -> fixed pointer value (1)
     * - clientLoginStart(ptr, pin) -> fixed 32-byte array (KE1 placeholder)
     * - clientLoginFinish(ptr, pin, serverBytes) -> fixed 32-byte array (KE3 placeholder)
     * - clientLoginGetSessionKey(ptr) -> fixed 32-byte key (shared with mock server)
     * - clientLoginGetExportKey(ptr) -> fixed 32-byte array
     * - clientLoginDestroy(ptr) -> no-op
     */
    private fun hookOpaque(cl: ClassLoader) {
        val className = "hu.ma.ne.Opaque"
        val clazz = try {
            cl.loadClass(className)
        } catch (e: ClassNotFoundException) {
            Log.w(TAG, "  $className not found, skipping OPAQUE hooks")
            return
        }

        val placeholder32 = ByteArray(32) { 0x00 }

        // clientLoginNew() -> long
        HookUtils.hookMethodBefore(clazz, "clientLoginNew", emptyArray()) { param ->
            param.result = 1L
            Log.i(TAG, "  OPAQUE: clientLoginNew() -> 1 (stub pointer)")
        }

        // clientLoginStart(long, String) -> byte[]
        HookUtils.hookMethodBefore(clazz, "clientLoginStart", arrayOf(Long::class.javaPrimitiveType!!, String::class.java)) { param ->
            param.result = placeholder32.clone()
            Log.i(TAG, "  OPAQUE: clientLoginStart() -> 32-byte placeholder KE1")
        }

        // clientLoginFinish(long, String, byte[]) -> byte[]
        HookUtils.hookMethodBefore(clazz, "clientLoginFinish", arrayOf(Long::class.javaPrimitiveType!!, String::class.java, ByteArray::class.java)) { param ->
            param.result = placeholder32.clone()
            Log.i(TAG, "  OPAQUE: clientLoginFinish() -> 32-byte placeholder KE3 (non-null = success)")
        }

        // clientLoginGetSessionKey(long) -> byte[]
        HookUtils.hookMethodBefore(clazz, "clientLoginGetSessionKey", arrayOf(Long::class.javaPrimitiveType!!)) { param ->
            param.result = OPAQUE_SESSION_KEY.clone()
            Log.i(TAG, "  OPAQUE: clientLoginGetSessionKey() -> fixed 32-byte session key")
        }

        // clientLoginGetExportKey(long) -> byte[]
        HookUtils.hookMethodBefore(clazz, "clientLoginGetExportKey", arrayOf(Long::class.javaPrimitiveType!!)) { param ->
            param.result = placeholder32.clone()
            Log.i(TAG, "  OPAQUE: clientLoginGetExportKey() -> 32-byte placeholder export key")
        }

        // clientLoginDestroy(long) -> void
        HookUtils.hookMethodBefore(clazz, "clientLoginDestroy", arrayOf(Long::class.javaPrimitiveType!!)) { param ->
            param.result = null
            Log.i(TAG, "  OPAQUE: clientLoginDestroy() -> no-op")
        }

        Log.i(TAG, "  OPAQUE hooks installed — all native crypto bypassed")
    }
}

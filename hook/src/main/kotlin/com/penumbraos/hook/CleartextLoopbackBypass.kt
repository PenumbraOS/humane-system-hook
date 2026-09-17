package com.penumbraos.hook

import android.security.NetworkSecurityPolicy
import android.util.Log
import de.robv.android.xposed.XC_MethodHook
import de.robv.android.xposed.XposedBridge

/**
 * Allow cleartext HTTP to the loopback address only.
 *
 * `humane_tts_service` ships with cleartext traffic disabled, so any call to the
 * Penumbra server on `127.0.0.1:8080` dies with
 * `IOException: Cleartext HTTP traffic to 127.0.0.1 not permitted` and the voice
 * falls back to the embedded engine. The server is on-device and the request
 * never leaves the handset, so there is nothing here for TLS to protect.
 *
 * Deliberately narrow: the hostname overload returns true **only** for loopback,
 * so every other destination keeps the app's original policy. Installed from
 * [HumaneTtsHooks], so it applies to the TTS process and nothing else.
 */
object CleartextLoopbackBypass {

    private const val TAG = "PenumbraTTS"

    private val LOOPBACK = setOf("127.0.0.1", "localhost", "::1")

    fun install() {
        val policyClass = runCatching { NetworkSecurityPolicy::class.java }.getOrNull() ?: run {
            Log.w(TAG, "  NetworkSecurityPolicy unavailable; cleartext bypass skipped")
            return
        }

        val hooked = runCatching {
            XposedBridge.hookAllMethods(
                policyClass,
                "isCleartextTrafficPermitted",
                object : XC_MethodHook() {
                    override fun afterHookedMethod(param: MethodHookParam) {
                        // Already permitted: leave the app's own answer alone.
                        if (param.result == true) return

                        val host = param.args.getOrNull(0) as? String
                        when {
                            // No-arg overload: the stack asks before it knows the
                            // host, and answering false here blocks the loopback
                            // call before the hostname check is ever reached.
                            host == null -> param.result = true
                            host.lowercase() in LOOPBACK -> param.result = true
                            // Everything else keeps the original policy.
                            else -> Unit
                        }
                    }
                },
            ).size
        }.getOrElse {
            Log.w(TAG, "  Failed to install cleartext bypass: ${it.message}")
            return
        }

        Log.w(TAG, "  Cleartext loopback bypass installed ($hooked method(s))")
    }
}

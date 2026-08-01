package com.penumbraos.hook

import android.util.Log
import java.util.concurrent.CompletableFuture

/**
 * Make the music experience's Tidal client believe it has a valid session token.
 *
 * Every Tidal request is gated by `TidalClient.token()`: it reads the cached
 * token via `TidalUserManager.getTokenFromSharedInstance()` /`fetchToken()`
 * (both `CompletableFuture<String>`) and, in `lambda$token$1`, throws
 * `TidalTokenNotFoundException` ("To play music, please link your music
 * account.") when the token is null/empty. Our stubbed partner token
 * (PartnerTokenRPCService) is empty, so the client bails before ever issuing a
 * REST request — which is why the [EndpointTypeBypass] redirect never fires.
 *
 * We force those getters to complete with a non-empty stub token. The local
 * shim ignores the token value, so any non-empty string unblocks the whole
 * REST path. Auth/login hosts are left alone.
 */
object TidalAuthBypass {

    private const val TAG = "PenumbraHook"

    private const val TIDAL_USER_MANAGER =
        "humane.experience.music.provider.tidal.auth.TidalUserManager"

    /** Any non-empty value works; the shim does not validate it. */
    private const val STUB_TOKEN = "penumbra-shim-token"

    fun install(cl: ClassLoader) {
        val clazz = try {
            cl.loadClass(TIDAL_USER_MANAGER)
        } catch (_: ClassNotFoundException) {
            Log.w(TAG, "  $TIDAL_USER_MANAGER not found, skipping Tidal auth bypass")
            return
        }

        var hooked = 0
        for (methodName in listOf("getTokenFromSharedInstance", "fetchToken")) {
            try {
                clazz.getDeclaredMethod(methodName)
            } catch (_: NoSuchMethodException) {
                Log.w(TAG, "  TidalUserManager.$methodName() missing, skipping")
                continue
            }
            HookUtils.hookMethodAfter(clazz, methodName, emptyArray()) { param ->
                if (param.throwable == null) {
                    param.result = CompletableFuture.completedFuture(STUB_TOKEN)
                }
            }
            hooked++
        }

        if (hooked > 0) {
            Log.w(TAG, "  Tidal auth bypass installed (token -> non-empty stub, $hooked getter(s))")
        } else {
            Log.w(TAG, "  No Tidal token getters found, auth bypass inactive")
        }
    }
}

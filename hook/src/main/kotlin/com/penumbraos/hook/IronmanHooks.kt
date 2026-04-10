package com.penumbraos.hook

import android.util.Log
import de.robv.android.xposed.XC_MethodHook
import de.robv.android.xposed.XposedBridge

/**
 * Debug hooks for ironman's gRPC infrastructure.
 *
 * First iteration: discovery + logging only. Does not modify any behavior.
 * Enumerates methods on ChannelFactory classes and hooks all declared methods
 * to log invocations.
 */
object IronmanHooks {

    private const val TAG = "PenumbraHook"

    private val CHANNEL_FACTORY_CLASSES = listOf(
        "humaneinternal.system.network.ChannelFactory",
        "humane.grandcentral.network.ChannelFactory",
    )

    fun install(cl: ClassLoader) {
        Log.i(TAG, "Installing ironman debug hooks...")

        for (className in CHANNEL_FACTORY_CLASSES) {
            hookChannelFactory(cl, className)
        }

        Log.i(TAG, "Ironman debug hooks installed")
    }

    private fun hookChannelFactory(cl: ClassLoader, className: String) {
        val clazz = try {
            cl.loadClass(className)
        } catch (e: ClassNotFoundException) {
            Log.w(TAG, "  $className not found, skipping")
            return
        }

        // Discovery: log all declared methods so we can identify hook targets
        val methods = clazz.declaredMethods
        Log.i(TAG, "  $className has ${methods.size} declared methods:")
        for (m in methods) {
            val params = m.parameterTypes.joinToString(", ") { it.simpleName }
            Log.i(TAG, "    ${m.name}($params) -> ${m.returnType.simpleName}")
        }

        // Hook every declared method for full visibility
        var hooked = 0
        for (method in methods) {
            try {
                method.isAccessible = true
                val methodName = method.name
                val paramTypes = method.parameterTypes.map { it.simpleName }

                XposedBridge.hookMethod(method, object : XC_MethodHook() {
                    override fun beforeHookedMethod(param: MethodHookParam) {
                        Log.i(TAG, ">>> $className.$methodName($paramTypes)")
                        Log.i(TAG, "    thread=${Thread.currentThread().name}")
                        param.args?.forEachIndexed { i, arg ->
                            Log.i(TAG, "    arg[$i]=${summarizeArg(arg)}")
                        }
                    }

                    override fun afterHookedMethod(param: MethodHookParam) {
                        if (param.throwable != null) {
                            Log.i(TAG, "<<< $className.$methodName() threw: ${param.throwable}")
                        } else {
                            Log.i(TAG, "<<< $className.$methodName() -> ${summarizeArg(param.result)}")
                        }
                    }
                })
                hooked++
            } catch (t: Throwable) {
                Log.e(TAG, "  Failed to hook ${method.name}: ${t.message}")
            }
        }
        Log.i(TAG, "  Hooked $hooked/${methods.size} methods on $className")
    }

    /**
     * Summarize an argument for logging without calling toString() on potentially
     * large or sensitive objects.
     */
    private fun summarizeArg(arg: Any?): String {
        if (arg == null) return "null"
        return try {
            when (arg) {
                is String -> "\"$arg\""
                is Number, is Boolean -> arg.toString()
                is Enum<*> -> arg.name
                else -> "${arg.javaClass.simpleName}@${System.identityHashCode(arg).toString(16)}"
            }
        } catch (t: Throwable) {
            "${arg.javaClass.simpleName}(toString failed)"
        }
    }
}

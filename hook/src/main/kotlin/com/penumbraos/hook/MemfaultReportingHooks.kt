package com.penumbraos.hook

import android.util.Log

/**
 * Silences Memfault's embedded RemoteMetricsService in any process that bundles it.
 *
 * The Memfault reporting library records metrics via record$reporting_lib_release()
 * and finishes reports via finishReport$reporting_lib_release().
 *
 * Treat telemetry as successfully dropped so callers do not keep handling this as
 * a retryable/report-finish failure.
 */
object MemfaultReportingHooks {
    private const val TAG = "PenumbraHook"

    fun install(cl: ClassLoader) {
        hookRemoteMetricsService(cl)
    }

    private fun hookRemoteMetricsService(cl: ClassLoader) {
        val className = "com.memfault.bort.reporting.RemoteMetricsService"
        val clazz = try {
            cl.loadClass(className)
        } catch (_: ClassNotFoundException) {
            Log.w(TAG, "  $className not found, skipping Memfault reporting hook")
            return
        }

        // record$reporting_lib_release(MetricValue) -> void
        try {
            val metricValueClass = cl.loadClass("com.memfault.bort.reporting.MetricValue")
            HookUtils.hookMethodBefore(clazz, "record\$reporting_lib_release", arrayOf(metricValueClass)) { param ->
                param.result = null
            }
        } catch (t: Throwable) {
            Log.w(TAG, "  Failed to hook RemoteMetricsService.record: ${t.message}")
        }

        // finishReport$reporting_lib_release(String, long, boolean) -> boolean
        try {
            HookUtils.hookMethodBefore(
                clazz,
                "finishReport\$reporting_lib_release",
                arrayOf(String::class.java, Long::class.javaPrimitiveType!!, Boolean::class.javaPrimitiveType!!),
            ) { param ->
                param.result = true
            }
        } catch (t: Throwable) {
            Log.w(TAG, "  Failed to hook RemoteMetricsService.finishReport: ${t.message}")
        }

        Log.w(TAG, "  Memfault RemoteMetricsService hooks installed (metrics dropped)")
    }
}

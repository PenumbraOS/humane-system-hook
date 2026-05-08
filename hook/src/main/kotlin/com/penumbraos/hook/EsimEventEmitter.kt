package com.penumbraos.hook

import android.os.Process
import android.util.Log
import org.json.JSONObject
import java.io.OutputStreamWriter
import java.net.Socket
import java.util.concurrent.LinkedBlockingQueue

object EsimEventEmitter {

    private const val TAG = "PenumbraHook"
    private const val TCP_HOST = "127.0.0.1"
    private const val TCP_PORT = 16789
    private const val SOURCE_PROCESS = "humane.connectivity.esimlpa"

    private val pendingEvents = LinkedBlockingQueue<PendingEvent>()

    init {
        Thread({
            while (true) {
                try {
                    val event = pendingEvents.take()
                    send(event)
                } catch (t: Throwable) {
                    Log.w(TAG, "eSIM event worker failed", t)
                }
            }
        }, "penumbra-esim-events").apply {
            isDaemon = true
            uncaughtExceptionHandler = Thread.UncaughtExceptionHandler { thread, error ->
                Log.e(TAG, "Uncaught on ${thread.name}", error)
            }
            start()
        }
    }

    fun emitActionStarted() {
        emit(
            "esim.action_started",
            EsimOperationContext.currentAction,
            JSONObject().put(
                "extras",
                JSONObject()
                    .put("iccid", EsimOperationContext.currentIccid)
                    .put("activationCode", EsimOperationContext.currentActivationCode)
                    .put("nickname", EsimOperationContext.currentNickname)
                    .put("penumbra_source", EsimOperationContext.currentSource)
            )
        )
    }

    fun emitSyspropUpdate(key: String, value: String?) {
        emit(
            "esim.sysprop_update",
            EsimOperationContext.currentAction,
            JSONObject()
                .put("key", key)
                .put("value", value)
        )
    }

    fun emitDeviceIdentifier(key: String, value: String?) {
        emitSyspropUpdate(key, value)
    }

    fun emitProfileMutationResult(operation: String, result: String, message: String?) {
        emit(
            "esim.profile_mutation_result",
            EsimOperationContext.currentAction,
            JSONObject()
                .put("operation", operation)
                .put("target_iccid", EsimOperationContext.currentIccid)
                .put("nickname", EsimOperationContext.currentNickname)
                .put("result", result)
                .put("message", message)
        )
    }

    fun emitDownloadProgress(phase: String, progress: Int? = null, message: String? = null) {
        emit(
            "esim.download_progress",
            EsimOperationContext.currentAction,
            JSONObject()
                .put("phase", phase)
                .put("progress", progress?.let { Integer.valueOf(it) } ?: JSONObject.NULL)
                .put("iccid", EsimOperationContext.currentDownloadIccid)
                .put("message", message)
        )
    }

    fun emitDownloadResult(result: String, message: String?) {
        emit(
            "esim.download_result",
            EsimOperationContext.currentAction,
            JSONObject()
                .put("result", result)
                .put("iccid", EsimOperationContext.currentDownloadIccid)
                .put("message", message)
        )
    }

    private fun emit(type: String, action: String?, payload: JSONObject) {
        val event = JSONObject()
            .put("version", 1)
            .put("type", type)
            .put("ts_ms", System.currentTimeMillis())
            .put("source_process", SOURCE_PROCESS)
            .put("source_pid", Process.myPid())
            .put("request_id", EsimOperationContext.currentRequestId)
            .put("action", action)
            .put("payload", payload)

        pendingEvents.offer(PendingEvent(type, event.toString()))
    }

    private fun send(event: PendingEvent) {
        var socket: Socket? = null
        var writer: OutputStreamWriter? = null
        try {
            socket = Socket(TCP_HOST, TCP_PORT)
            writer = OutputStreamWriter(socket.getOutputStream(), Charsets.UTF_8)
            writer.write(event.body)
            writer.write('\n'.code)
            writer.flush()
        } catch (t: Throwable) {
            Log.w(TAG, "Failed to emit eSIM event type=${event.type}", t)
        } finally {
            try {
                writer?.close()
            } catch (_: Throwable) {
            }
            try {
                socket?.close()
            } catch (_: Throwable) {
            }
        }
    }

    private data class PendingEvent(
        val type: String,
        val body: String,
    )
}

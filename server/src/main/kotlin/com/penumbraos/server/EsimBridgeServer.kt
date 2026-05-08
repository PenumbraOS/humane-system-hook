package com.penumbraos.server

import android.content.Context
import android.net.wifi.WifiManager
import android.util.Log
import org.json.JSONObject
import java.io.BufferedReader
import java.io.InputStreamReader
import java.io.OutputStreamWriter
import java.net.InetAddress
import java.net.ServerSocket
import java.net.Socket
import java.util.concurrent.CopyOnWriteArraySet
import java.util.concurrent.atomic.AtomicBoolean

// Server for pushing data from hooks injected into the Humane LPA into our world
object EsimBridgeServer {

    private const val TAG = "PenumbraEsimBridge"
    const val TCP_PORT = 16790

    private val running = AtomicBoolean(false)
    private val clients = CopyOnWriteArraySet<ClientConnection>()

    @Volatile
    private var appContext: Context? = null

    @Volatile
    private var serverSocket: ServerSocket? = null

    @Volatile
    private var acceptThread: Thread? = null

    private val typedEventListener: (JSONObject) -> Unit = { event ->
        broadcast(event)
    }

    fun start(context: Context) {
        appContext = context.applicationContext
        if (!running.compareAndSet(false, true)) {
            return
        }

        EsimEventStore.addTypedEventListener(typedEventListener)

        try {
            val socket = ServerSocket(TCP_PORT, 50, InetAddress.getByName("127.0.0.1"))
            serverSocket = socket
            acceptThread = Thread({
                acceptLoop(socket)
            }, "penumbra-esim-bridge").apply {
                isDaemon = true
                uncaughtExceptionHandler = Thread.UncaughtExceptionHandler { thread, error ->
                    Log.e(TAG, "Uncaught on ${thread.name}", error)
                }
                start()
            }
            Log.w(TAG, "Started eSIM bridge server on 127.0.0.1:$TCP_PORT")
        } catch (t: Throwable) {
            running.set(false)
            serverSocket = null
            acceptThread = null
            EsimEventStore.removeTypedEventListener(typedEventListener)
            Log.e(TAG, "Failed to start eSIM bridge server", t)
        }
    }

    fun stop() {
        running.set(false)
        EsimEventStore.removeTypedEventListener(typedEventListener)
        try {
            serverSocket?.close()
        } catch (t: Throwable) {
            Log.w(TAG, "Failed to close eSIM bridge server", t)
        }
        serverSocket = null
        acceptThread = null
        clients.forEach { it.close() }
        clients.clear()
    }

    private fun acceptLoop(socket: ServerSocket) {
        while (running.get()) {
            val client = try {
                socket.accept()
            } catch (t: Throwable) {
                if (running.get()) {
                    Log.w(TAG, "Accept loop failed", t)
                }
                break
            }
            handleClient(client)
        }
    }

    private fun handleClient(socket: Socket) {
        val connection = try {
            ClientConnection(socket)
        } catch (t: Throwable) {
            Log.w(TAG, "Failed to wrap bridge client", t)
            try {
                socket.close()
            } catch (_: Throwable) {
            }
            return
        }

        clients.add(connection)
        Thread({
            try {
                BufferedReader(InputStreamReader(socket.getInputStream())).use { reader ->
                    while (running.get()) {
                        val line = reader.readLine() ?: break
                        if (line.isBlank()) continue
                        handleMessage(connection, line)
                    }
                }
            } catch (t: Throwable) {
                if (running.get()) {
                    Log.w(TAG, "Bridge client read failed", t)
                }
            } finally {
                clients.remove(connection)
                connection.close()
            }
        }, "penumbra-esim-bridge-client").apply {
            isDaemon = true
            uncaughtExceptionHandler = Thread.UncaughtExceptionHandler { thread, error ->
                Log.e(TAG, "Uncaught on ${thread.name}", error)
            }
            start()
        }
    }

    private fun handleMessage(connection: ClientConnection, line: String) {
        try {
            val message = JSONObject(line)
            when (message.optString("type")) {
                "esim.request" -> handleRequestMessage(connection, message)
                "cellular.status_request" -> handleCellularStatusRequest(connection, message)
                "wifi.set_enabled_request" -> handleWifiSetEnabledRequest(connection, message)
                "cellular.set_enabled_request" -> handleCellularSetEnabledRequest(connection, message)
                else -> {
                    connection.send(
                        JSONObject()
                            .put("type", "esim.bridge_error")
                            .put("message", "Unsupported message type")
                            .put("raw_type", message.optString("type"))
                    )
                }
            }
        } catch (t: Throwable) {
            Log.w(TAG, "Failed to parse bridge message: $line", t)
            connection.send(
                JSONObject()
                    .put("type", "esim.bridge_error")
                    .put("message", "Invalid JSON request")
            )
        }
    }

    private fun handleRequestMessage(connection: ClientConnection, message: JSONObject) {
        val context = appContext
        if (context == null) {
            connection.send(
                JSONObject()
                    .put("type", "esim.bridge_error")
                    .put("message", "Server app context unavailable")
                    .put("request_id", message.optString("request_id"))
            )
            return
        }

        val requestId = message.optString("request_id").takeIf { it.isNotEmpty() }
        val action = message.optString("action").takeIf { it.isNotEmpty() }
        val payload = message.optJSONObject("payload") ?: JSONObject()

        if (requestId == null || action == null) {
            connection.send(
                JSONObject()
                    .put("type", "esim.bridge_error")
                    .put("message", "request_id and action are required")
                    .put("request_id", requestId ?: JSONObject.NULL)
            )
            return
        }

        val iccid = payload.optString("iccid").takeIf { it.isNotEmpty() }
        val activationCode = payload.optString("activationCode").takeIf { it.isNotEmpty() }
        val nickname = payload.optString("nickname").takeIf { it.isNotEmpty() }

        EsimController.dispatch(
            context = context,
            requestId = requestId,
            lpaAction = action,
            iccid = iccid,
            activationCode = activationCode,
            nickname = nickname,
            source = "rust",
        )

        connection.send(
            JSONObject()
                .put("type", "esim.request_accepted")
                .put("request_id", requestId)
                .put("action", action)
        )
    }

    private fun handleCellularStatusRequest(connection: ClientConnection, message: JSONObject) {
        val context = appContext
        if (context == null) {
            connection.send(
                JSONObject()
                    .put("type", "cellular.status_error")
                    .put("message", "Server app context unavailable")
                    .put("request_id", message.optString("request_id").ifEmpty { JSONObject.NULL })
            )
            return
        }

        connection.send(
            JSONObject()
                .put("type", "cellular.status_result")
                .put("request_id", message.optString("request_id").ifEmpty { JSONObject.NULL })
                .put("payload", CellularStatusProvider.snapshot(context))
        )
    }

    private fun handleWifiSetEnabledRequest(connection: ClientConnection, message: JSONObject) {
        val context = appContext
        val requestId = message.optString("request_id").ifEmpty { null }
        if (context == null) {
            connection.send(
                JSONObject()
                    .put("type", "wifi.set_enabled_error")
                    .put("request_id", requestId ?: JSONObject.NULL)
                    .put("payload", JSONObject().put("message", "Server app context unavailable"))
            )
            return
        }

        val enabled = message.optJSONObject("payload")?.optBoolean("enabled")
        if (enabled == null) {
            connection.send(
                JSONObject()
                    .put("type", "wifi.set_enabled_error")
                    .put("request_id", requestId ?: JSONObject.NULL)
                    .put("payload", JSONObject().put("message", "enabled is required"))
            )
            return
        }

        try {
            val wifiManager = context.applicationContext.getSystemService(Context.WIFI_SERVICE) as? WifiManager
                ?: throw IllegalStateException("WifiManager unavailable")
            val success = wifiManager.setWifiEnabled(enabled)
            if (!success) {
                throw IllegalStateException("WifiManager rejected toggle request")
            }
            connection.send(
                JSONObject()
                    .put("type", "wifi.set_enabled_result")
                    .put("request_id", requestId ?: JSONObject.NULL)
                    .put("payload", JSONObject()
                        .put("result", "success")
                        .put("enabled", enabled)
                    )
            )
        } catch (t: Throwable) {
            Log.w(TAG, "Failed to toggle Wi-Fi", t)
            connection.send(
                JSONObject()
                    .put("type", "wifi.set_enabled_error")
                    .put("request_id", requestId ?: JSONObject.NULL)
                    .put("payload", JSONObject()
                        .put("message", t.message ?: "Failed to toggle Wi-Fi")
                    )
            )
        }
    }

    private fun handleCellularSetEnabledRequest(connection: ClientConnection, message: JSONObject) {
        val context = appContext
        val requestId = message.optString("request_id").ifEmpty { null }
        if (context == null) {
            connection.send(
                JSONObject()
                    .put("type", "cellular.set_enabled_error")
                    .put("request_id", requestId ?: JSONObject.NULL)
                    .put("payload", JSONObject().put("message", "Server app context unavailable"))
            )
            return
        }

        val enabled = message.optJSONObject("payload")?.optBoolean("enabled")
        if (enabled == null) {
            connection.send(
                JSONObject()
                    .put("type", "cellular.set_enabled_error")
                    .put("request_id", requestId ?: JSONObject.NULL)
                    .put("payload", JSONObject().put("message", "enabled is required"))
            )
            return
        }

        try {
            val command = listOf("cmd", "phone", "data", if (enabled) "enable" else "disable")
            val process = ProcessBuilder(command)
                .redirectErrorStream(false)
                .start()
            val stdout = process.inputStream.bufferedReader().use { it.readText() }.trim()
            val stderr = process.errorStream.bufferedReader().use { it.readText() }.trim()
            val exitCode = process.waitFor()
            if (exitCode != 0) {
                val suffix = listOf(stdout, stderr)
                    .filter { it.isNotEmpty() }
                    .joinToString(" | ")
                    .takeIf { it.isNotEmpty() }
                    ?.let { ": $it" }
                    .orEmpty()
                throw IllegalStateException("cmd phone data ${if (enabled) "enable" else "disable"} failed (exit $exitCode)$suffix")
            }
            connection.send(
                JSONObject()
                    .put("type", "cellular.set_enabled_result")
                    .put("request_id", requestId ?: JSONObject.NULL)
                    .put("payload", JSONObject()
                        .put("result", "success")
                        .put("enabled", enabled)
                    )
            )
        } catch (t: Throwable) {
            Log.w(TAG, "Failed to toggle cellular data", t)
            connection.send(
                JSONObject()
                    .put("type", "cellular.set_enabled_error")
                    .put("request_id", requestId ?: JSONObject.NULL)
                    .put("payload", JSONObject()
                        .put("message", t.message ?: "Failed to toggle cellular data")
                    )
            )
        }
    }

    private fun broadcast(event: JSONObject) {
        val dead = mutableListOf<ClientConnection>()
        for (client in clients) {
            if (!client.send(event)) {
                dead += client
            }
        }
        dead.forEach {
            clients.remove(it)
            it.close()
        }
    }

    private class ClientConnection(private val socket: Socket) {
        private val writer = OutputStreamWriter(socket.getOutputStream(), Charsets.UTF_8)

        @Synchronized
        fun send(message: JSONObject): Boolean {
            return try {
                writer.write(message.toString())
                writer.write('\n'.code)
                writer.flush()
                true
            } catch (t: Throwable) {
                Log.w(TAG, "Failed to send bridge message", t)
                false
            }
        }

        fun close() {
            try {
                writer.close()
            } catch (_: Throwable) {
            }
            try {
                socket.close()
            } catch (_: Throwable) {
            }
        }
    }
}

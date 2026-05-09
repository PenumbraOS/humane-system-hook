package com.penumbraos.server

import android.net.LocalServerSocket
import android.net.LocalSocket
import android.util.Log
import java.io.Closeable
import java.io.InputStream
import java.io.OutputStream
import java.net.Socket
import java.net.SocketException
import java.util.concurrent.atomic.AtomicBoolean

/**
 * Bridges browser WebUSB/ADB Center traffic to the local Penumbra HTTP server.
 *
 * Center opens ADB service `localabstract:penumbra_http`. Android adbd connects
 * that to this LocalServerSocket, and this bridge proxies bytes to
 * 127.0.0.1:8080. This avoids adbd's arbitrary `tcp:<port>` service, which is
 * blocked by SELinux on some builds.
 */
object CenterUsbBridge {
    private const val TAG = "PenumbraUsbBridge"
    private const val ABSTRACT_SOCKET_NAME = "penumbra_http"
    private const val HTTP_HOST = "127.0.0.1"
    private const val HTTP_PORT = 8080
    private const val COPY_BUFFER_SIZE = 8192

    private val running = AtomicBoolean(false)
    private var serverSocket: LocalServerSocket? = null

    fun start() {
        if (!running.compareAndSet(false, true)) {
            Log.w(TAG, "USB bridge already running")
            return
        }

        try {
            val socket = LocalServerSocket(ABSTRACT_SOCKET_NAME)
            serverSocket = socket
            val thread = Thread({ acceptLoop(socket) }, "penumbra-usb-bridge-accept")
            thread.isDaemon = true
            thread.uncaughtExceptionHandler = safeHandler()
            thread.start()
            Log.w(TAG, "USB bridge listening on localabstract:$ABSTRACT_SOCKET_NAME")
        } catch (t: Throwable) {
            running.set(false)
            serverSocket = null
            Log.e(TAG, "Failed to start USB bridge", t)
        }
    }

    fun stop() {
        running.set(false)
        closeQuietly(serverSocket)
        serverSocket = null
    }

    private fun acceptLoop(socket: LocalServerSocket) {
        try {
            while (running.get()) {
                val localSocket = try {
                    socket.accept()
                } catch (t: Throwable) {
                    if (running.get()) {
                        Log.w(TAG, "USB bridge accept failed", t)
                    }
                    continue
                }

                try {
                    bridgeConnection(localSocket)
                } catch (t: Throwable) {
                    Log.w(TAG, "Failed to bridge USB connection", t)
                    closeQuietly(localSocket)
                }
            }
        } finally {
            running.set(false)
            closeQuietly(socket)
        }
    }

    private fun bridgeConnection(localSocket: LocalSocket) {
        val httpSocket = try {
            Socket(HTTP_HOST, HTTP_PORT)
        } catch (t: Throwable) {
            Log.w(TAG, "Failed to connect to local HTTP server", t)
            closeQuietly(localSocket)
            return
        }

        val localIn = localSocket.inputStream
        val localOut = localSocket.outputStream
        val httpIn = httpSocket.getInputStream()
        val httpOut = httpSocket.getOutputStream()

        val txDone = AtomicBoolean(false)
        val rxDone = AtomicBoolean(false)

        fun closeIfDone() {
            if (txDone.get() && rxDone.get()) {
                closeQuietly(localSocket)
                closeQuietly(httpSocket)
            }
        }

        val txThread = Thread({
            try {
                copyStream(localIn, httpOut)
                shutdownOutputQuietly(httpSocket)
            } catch (t: Throwable) {
                if (!isExpectedSocketClose(t)) {
                    Log.w(TAG, "USB bridge request copy failed", t)
                }
                closeQuietly(httpSocket)
            } finally {
                txDone.set(true)
                closeIfDone()
            }
        }, "penumbra-usb-bridge-tx")
        txThread.isDaemon = true
        txThread.uncaughtExceptionHandler = safeHandler()

        val rxThread = Thread({
            try {
                copyStream(httpIn, localOut)
                shutdownOutputQuietly(localSocket)
            } catch (t: Throwable) {
                if (!isExpectedSocketClose(t)) {
                    Log.w(TAG, "USB bridge response copy failed", t)
                }
                closeQuietly(localSocket)
            } finally {
                rxDone.set(true)
                closeIfDone()
            }
        }, "penumbra-usb-bridge-rx")
        rxThread.isDaemon = true
        rxThread.uncaughtExceptionHandler = safeHandler()

        txThread.start()
        rxThread.start()
    }

    private fun copyStream(input: InputStream, output: OutputStream) {
        val buffer = ByteArray(COPY_BUFFER_SIZE)
        while (true) {
            val bytesRead = input.read(buffer)
            if (bytesRead == -1) break
            output.write(buffer, 0, bytesRead)
            output.flush()
        }
    }

    private fun isExpectedSocketClose(t: Throwable): Boolean {
        return t is SocketException && t.message?.contains("Socket closed", ignoreCase = true) == true
    }

    private fun shutdownOutputQuietly(socket: Socket) {
        try {
            if (!socket.isClosed && !socket.isOutputShutdown) {
                socket.shutdownOutput()
            }
        } catch (_: Throwable) {
        }
    }

    private fun shutdownOutputQuietly(socket: LocalSocket) {
        try {
            socket.shutdownOutput()
        } catch (_: Throwable) {
        }
    }

    private fun safeHandler() = Thread.UncaughtExceptionHandler { thread, error ->
        if (isExpectedSocketClose(error)) {
            Log.w(TAG, "Socket closed on ${thread.name}")
        } else {
            Log.e(TAG, "Uncaught on ${thread.name}", error)
        }
    }

    private fun closeQuietly(closeable: Any?) {
        try {
            when (closeable) {
                is LocalServerSocket -> closeable.close()
                is LocalSocket -> closeable.close()
                is Socket -> closeable.close()
                is Closeable -> closeable.close()
            }
        } catch (_: Throwable) {
        }
    }
}

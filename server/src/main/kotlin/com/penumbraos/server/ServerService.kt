package com.penumbraos.server

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.Service
import android.content.Context
import android.content.Intent
import android.net.wifi.WifiManager
import android.os.Build
import android.os.Handler
import android.os.IBinder
import android.os.Looper
import android.util.Log

class ServerService : Service() {

    companion object {
        private const val TAG = "PenumbraServer"
        private const val CHANNEL_ID = "penumbra_server"
        private const val CHANNEL_NAME = "Penumbra Server"
        private const val NOTIFICATION_ID = 1001
        private const val MULTICAST_LOCK_TAG = "penumbra-jmdns"

        fun start(context: Context) {
            val intent = Intent(context, ServerService::class.java)
            context.startForegroundService(intent)
        }
    }

    private lateinit var advertiser: JmDnsAdvertiser
    private var multicastLock: WifiManager.MulticastLock? = null
    private val mainHandler = Handler(Looper.getMainLooper())

    @Volatile
    private var advertisedConfig: BootstrapConfig.AdvertisedConfig? = null

    override fun onCreate() {
        super.onCreate()
        createNotificationChannel()
        startForegroundCompat()

        acquireMulticastLock()
        EsimSocketServer.start()
        EsimBridgeServer.start(applicationContext)
        CenterUsbBridge.start()

        advertiser = JmDnsAdvertiser()

        ServerRuntime.setStateListener { running ->
            mainHandler.post {
                if (running) {
                    advertisedConfig?.let {
                        advertiser.start(it.httpPort, it.displayName)
                    }
                } else {
                    advertiser.stop()
                }
            }
        }
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        try {
            val configPath = BootstrapConfig.ensureCanonicalConfig(applicationContext)
            advertisedConfig = BootstrapConfig.readAdvertisedConfig(configPath)
            ServerRuntime.start(applicationContext, configPath)
            updateNotification("On-device server running")
        } catch (t: Throwable) {
            Log.e(TAG, "Failed to start server runtime", t)
            updateNotification("On-device server failed to start")
        }
        return START_STICKY
    }

    override fun onDestroy() {
        try {
            advertiser.stop()
        } catch (t: Throwable) {
            Log.w(TAG, "Failed to stop advertiser", t)
        }
        try {
            ServerRuntime.setStateListener(null)
            ServerRuntime.stop()
        } catch (t: Throwable) {
            Log.w(TAG, "Failed to stop runtime cleanly", t)
        }
        CenterUsbBridge.stop()
        EsimBridgeServer.stop()
        EsimSocketServer.stop()
        releaseMulticastLock()
        super.onDestroy()
    }

    override fun onBind(intent: Intent?): IBinder? = null

    private fun acquireMulticastLock() {
        try {
            val wifi = applicationContext.getSystemService(Context.WIFI_SERVICE) as WifiManager
            val lock = wifi.createMulticastLock(MULTICAST_LOCK_TAG).apply {
                setReferenceCounted(false)
                acquire()
            }
            multicastLock = lock
            Log.w(TAG, "Acquired Wi-Fi multicast lock")
        } catch (t: Throwable) {
            Log.w(TAG, "Failed to acquire multicast lock", t)
        }
    }

    private fun releaseMulticastLock() {
        try {
            multicastLock?.let {
                if (it.isHeld) it.release()
            }
        } catch (t: Throwable) {
            Log.w(TAG, "Failed to release multicast lock", t)
        }
        multicastLock = null
    }

    private fun startForegroundCompat() {
        startForeground(NOTIFICATION_ID, buildNotification("Starting on-device server"))
    }

    private fun updateNotification(text: String) {
        val manager = getSystemService(NotificationManager::class.java)
        manager.notify(NOTIFICATION_ID, buildNotification(text))
    }

    private fun createNotificationChannel() {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.O) return

        val channel = NotificationChannel(
            CHANNEL_ID,
            CHANNEL_NAME,
            NotificationManager.IMPORTANCE_LOW,
        ).apply {
            description = "Foreground service for Penumbra server"
            setShowBadge(false)
        }

        val manager = getSystemService(NotificationManager::class.java)
        manager.createNotificationChannel(channel)
    }

    private fun buildNotification(text: String): Notification {
        return Notification.Builder(this, CHANNEL_ID)
            .setContentTitle("Penumbra Server")
            .setContentText(text)
            .setSmallIcon(android.R.drawable.stat_notify_sync)
            .setOngoing(true)
            .build()
    }
}

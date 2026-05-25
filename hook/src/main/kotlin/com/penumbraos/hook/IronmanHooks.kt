package com.penumbraos.hook

import android.app.Application
import android.os.IBinder
import android.provider.Settings
import android.util.Log
import de.robv.android.xposed.XC_MethodHook
import de.robv.android.xposed.XposedBridge
import java.security.cert.X509Certificate

/**
 * Hooks for ironman.
 *
 *
 * Ordering requirement: [hookCredentialManager] MUST run before
 * [ChannelFactoryBypass.install] — without it, ironman crash-loops
 * due to missing device certificates before any gRPC redirect fires.
 */
object IronmanHooks {

    private const val TAG = "PenumbraHook"
    private const val HUMANE_DISPLAY_VERSION_KEY = "penumbra.humane_display_version"

    fun install(cl: ClassLoader) {
        Log.w(TAG, "Installing ironman hooks...")
        Log.w(TAG, "  Mock server: ${ChannelFactoryBypass.MOCK_SERVER_URI}")

        // Credential hooks must be installed first — without these, ironman
        // crash-loops before ChannelFactory hooks ever get a chance to run.
        hookCredentialManager(cl)

        // Redirect all gRPC traffic to mock server
        ChannelFactoryBypass.install(cl)

        // Hook DAC signature generation as a safety net
        hookDacSignature(cl)

        // Encryption bypass: make all data protection calls return plaintext
        hookDataProtector(cl)

        // Ephemeral encryption bypass: short-circuit prepare(), make encrypt/decrypt
        // pass plaintext through EncryptedData envelopes so the mock server can
        // read encrypted RPC endpoints (weather, nearby, chat, etc.) in the clear.
        EphemeralProtectionBypass.install(cl)

        // Provisioning state fix: only force NORMAL mode and DUC_PROVISIONED=1
        // if onboarding has already completed. On a fresh device, leave these
        // alone so the onboarding UI runs naturally.
        hookApplicationOnCreate(cl)

        // Block Datadog SDK initialization (RUM, tracing, crash reports, log forwarding
        // to browser-intake-datadoghq.com)
        DatadogBypass.install(cl)

        // Block Microsoft Cognitive Services Speech SDK telemetry
        // (1DS protocol to mobile.events.data.microsoft.com)
        MsTelemetryBypass.install(cl)

        // Block Humane connectivity check phone-home
        // (HTTP GET to connectivity-check.prod.humane.cloud — cosmetic only)
        ConnectivityCheckBypass.install(cl)

        // Silence Memfault RemoteMetricsService
        hookRemoteMetricsService(cl)

        // Bypass blocking IWlcService.disableTx binder calls
        WirelessChargingBypass.install(cl)

        Log.w(TAG, "Ironman hooks installed")
    }

    /**
     * Patch AbstractCredentialKeyManager to survive missing device credentials.
     *
     * The device's hardware keystore (TEE) may have no provisioned cert chain
     * for the "DeviceUserCreds" alias. When this happens:
     *
     * - getCertificateChain(): HumaneCertificate.getCertificateChain() returns null,
     *   and the for-each loop over null throws NPE. This crashes newChannel() in
     *   the TLS path, AND getLeafCertificate() -> addCertHeader() in the plaintext
     *   path (called by NetworkManager.newServiceStub()).
     *
     * - getPrivateKey(): if getKeyPair() throws, the catch block sets keyPair=null,
     *   then keyPair.getPrivate() NPEs. Hit during SSLContext.init() in the TLS path.
     *
     * We suppress both NPEs so ironman can proceed to the plaintext/redirect path.
     */
    private fun hookCredentialManager(cl: ClassLoader) {
        val className = "humaneinternal.system.credentials.AbstractCredentialKeyManager"
        val clazz = try {
            cl.loadClass(className)
        } catch (e: ClassNotFoundException) {
            Log.w(TAG, "  $className not found, skipping credential hooks")
            return
        }

        // getCertificateChain(String) -> X509Certificate[]
        // Suppress NPE from for-each over null cert chain; return empty array
        // so callers like getLeafCertificate() get Optional.empty() instead of crashing.
        try {
            val method = clazz.getDeclaredMethod("getCertificateChain", String::class.java)
            method.isAccessible = true
            XposedBridge.hookMethod(method, object : XC_MethodHook() {
                override fun afterHookedMethod(param: MethodHookParam) {
                    if (param.throwable is NullPointerException) {
                        param.throwable = null
                        param.result = emptyArray<X509Certificate>()
                        Log.w(TAG, "getCertificateChain() NPE suppressed — device cert chain missing, returning empty array")
                    }
                }
            })
            Log.w(TAG, "  Hooked $className.getCertificateChain()")
        } catch (t: Throwable) {
            Log.e(TAG, "  Failed to hook getCertificateChain: ${t.message}")
        }

        // getPrivateKey(String) -> PrivateKey
        // Suppress NPE from keyPair.getPrivate() when keyPair is null after catch;
        // return null so SSLContext silently skips client cert presentation.
        try {
            val method = clazz.getDeclaredMethod("getPrivateKey", String::class.java)
            method.isAccessible = true
            XposedBridge.hookMethod(method, object : XC_MethodHook() {
                override fun afterHookedMethod(param: MethodHookParam) {
                    if (param.throwable is NullPointerException) {
                        param.throwable = null
                        param.result = null
                        Log.w(TAG, "getPrivateKey() NPE suppressed — device keypair missing, returning null")
                    }
                }
            })
            Log.w(TAG, "  Hooked $className.getPrivateKey()")
        } catch (t: Throwable) {
            Log.e(TAG, "  Failed to hook getPrivateKey: ${t.message}")
        }
    }

    /**
     * Hook Application.onCreate() to run provisioning fix once we have a Context.
     *
     * instantiateApplication() runs before attachBaseContext(), so we can't
     * access ContentResolver there. onCreate() runs after attach, giving us
     * full Context access.
     *
     * Only runs in the main process — the :voiceinteractor process doesn't
     * need provisioning fixes and shouldn't call setBaselineMode().
     */
    private fun hookApplicationOnCreate(cl: ClassLoader) {
        try {
            val appClass = cl.loadClass("humaneinternal.system.MainApplication")
            val onCreateMethod = appClass.getDeclaredMethod("onCreate")
            onCreateMethod.isAccessible = true
            XposedBridge.hookMethod(onCreateMethod, object : XC_MethodHook() {
                override fun afterHookedMethod(param: MethodHookParam) {
                    val app = param.thisObject as Application
                    val processName = app.applicationInfo?.processName ?: ""
                    val myProcess = Application.getProcessName() ?: ""
                    Log.w(TAG, "Application.onCreate() — process=$myProcess")

                    // Only run in main process (not :voiceinteractor)
                    if (myProcess.contains(":")) {
                        Log.w(TAG, "  Skipping provisioning fix in subprocess: $myProcess")
                        return
                    }

                    try {
                        cacheHumaneDisplayVersion(app, cl)
                    } catch (t: Throwable) {
                        Log.e(TAG, "Humane display version cache failed", t)
                    }

                    try {
                        fixProvisioningState(app, cl)
                    } catch (t: Throwable) {
                        Log.e(TAG, "Provisioning fix failed", t)
                    }
                }
            })
            Log.w(TAG, "  Hooked MainApplication.onCreate() for provisioning fix")
        } catch (t: Throwable) {
            Log.e(TAG, "  Failed to hook MainApplication.onCreate(): ${t.message}")
            // Fallback: try hooking android.app.Application.onCreate() directly
            try {
                val appClass = Application::class.java
                val onCreateMethod = appClass.getDeclaredMethod("onCreate")
                XposedBridge.hookMethod(onCreateMethod, object : XC_MethodHook() {
                    override fun afterHookedMethod(param: MethodHookParam) {
                        val app = param.thisObject as? Application ?: return
                        val myProcess = Application.getProcessName() ?: ""
                        if (myProcess.contains(":")) return

                        try {
                            cacheHumaneDisplayVersion(app, cl)
                        } catch (t: Throwable) {
                            Log.e(TAG, "Humane display version cache failed", t)
                        }

                        try {
                            fixProvisioningState(app, cl)
                        } catch (t: Throwable) {
                            Log.e(TAG, "Provisioning fix failed", t)
                        }
                    }
                })
                Log.w(TAG, "  Hooked Application.onCreate() (fallback) for provisioning fix")
            } catch (t2: Throwable) {
                Log.e(TAG, "  Failed to hook Application.onCreate() fallback: ${t2.message}")
            }
        }
    }

    /**
     * Cache Humane's display software version in Settings.Global so the Penumbra
     * server can expose it without directly reading SELinux-restricted
     * ro.humane.* properties.
     */
    private fun cacheHumaneDisplayVersion(app: Application, cl: ClassLoader) {
        val displayVersion = try {
            val deviceAgentClass = cl.loadClass("humaneinternal.system.utils.DeviceAgent")
            val method = deviceAgentClass.getMethod("getDisplayVersion")
            method.invoke(null) as? String
        } catch (t: Throwable) {
            Log.w(TAG, "  Failed to read DeviceAgent.getDisplayVersion(): ${t.javaClass.simpleName}: ${t.message}")
            null
        }?.trim().orEmpty()

        if (displayVersion.isEmpty()) {
            Log.w(TAG, "  Humane display version is empty; not caching")
            return
        }

        val success = Settings.Global.putString(
            app.contentResolver,
            HUMANE_DISPLAY_VERSION_KEY,
            displayVersion,
        )
        if (success) {
            Log.w(TAG, "  Cached Humane display version in Settings.Global: $displayVersion")
        } else {
            Log.w(TAG, "  Settings.Global.putString($HUMANE_DISPLAY_VERSION_KEY) returned false")
        }
    }

    /**
     * Fix the provisioning state iff onboarding has already completed.
     *
     * If DUC_PROVISIONED is already 1, this is a reboot after successful onboarding.
     * We ensure baseline mode stays NORMAL (recovery from any mode drift).
     *
     * If DUC_PROVISIONED is 0, onboarding has NOT completed yet. We leave
     * everything alone so the onboarding UI can run naturally.
     */
    private fun fixProvisioningState(app: Application, cl: ClassLoader) {
        val resolver = app.contentResolver
        val key = "humane.settings.global.DUC_PROVISIONED"
        val current = Settings.Global.getInt(resolver, key, 0)

        Log.w(TAG, "=== Provisioning state check: DUC_PROVISIONED=$current ===")

        if (current == 0) {
            Log.w(TAG, "  Device not yet provisioned — letting onboarding run naturally")
            return
        }

        // Already provisioned — ensure baseline mode is NORMAL after reboot
        Log.w(TAG, "  Device already provisioned — ensuring NORMAL mode")
        fixBaselineMode(cl)

        Log.w(TAG, "=== Provisioning fix complete ===")
    }

    /**
     * Call ISystemModeService.setBaselineMode(0) via reflection on the AIDL proxy.
     *
     * The AIDL-generated classes are in ironman's classloader:
     *   - ISystemModeService.Stub.asInterface(binder) returns the proxy
     *   - proxy.setBaselineMode((byte) 0) sets NORMAL mode
     *
     * setBaselineMode() in SystemModeService (system_server) will:
     *   1. Validate mode is baseline (0 or 1)
     *   2. Write to SharedPreferences: putInt("baselineMode", 0)
     *   3. If current mode != 0, call resetMode() which transitions to NORMAL
     *   4. resetMode() calls __setModeInternal() which broadcasts via IModeCallback
     *   5. TouchpadActionManager receives onModeSet() and updates mMode to 0
     */
    private fun fixBaselineMode(cl: ClassLoader) {
        try {
            // Get the SystemModeService binder
            val serviceManagerClass = Class.forName("android.os.ServiceManager")
            val getServiceMethod = serviceManagerClass.getMethod("getService", String::class.java)
            val binder = getServiceMethod.invoke(null, "humane.service.SystemModeService") as? IBinder

            if (binder == null) {
                Log.w(TAG, "  SystemModeService binder not found — service may not be ready yet")
                return
            }

            // Load ISystemModeService.Stub and call asInterface()
            val stubClass = cl.loadClass("humane.sysmode.ISystemModeService\$Stub")
            val asInterfaceMethod = stubClass.getMethod("asInterface", IBinder::class.java)
            val service = asInterfaceMethod.invoke(null, binder)
                ?: run {
                    Log.w(TAG, "  ISystemModeService.Stub.asInterface() returned null")
                    return
                }

            // Check current baseline mode first (idempotent)
            val getBaselineMethod = service.javaClass.getMethod("getBaselineMode")
            val currentBaseline = getBaselineMethod.invoke(service) as Byte

            Log.w(TAG, "  Current baseline mode: $currentBaseline")

            if (currentBaseline == 0.toByte()) {
                Log.w(TAG, "  Baseline already NORMAL (0), no change needed")
                return
            }

            // Set baseline to NORMAL (0)
            val setBaselineMethod = service.javaClass.getMethod("setBaselineMode", Byte::class.javaPrimitiveType)
            setBaselineMethod.invoke(service, 0.toByte())
            Log.w(TAG, "  setBaselineMode(0) — SUCCESS — mode transitioned from $currentBaseline to NORMAL")

        } catch (t: Throwable) {
            Log.e(TAG, "  setBaselineMode failed: ${t.javaClass.simpleName}: ${t.message}")
            // Non-fatal — device continues with whatever mode it had
        }
    }

    /**
     * Bypass Krypton data protection so all captured data arrives as plaintext.
     */
    private fun hookDataProtector(cl: ClassLoader) {
        DataProtectorBypass.install(cl)
    }

    /**
     * Ensure DUC_PROVISIONED is set to 1 in Settings.Global.
     *
     * This is the flag that controls:
     * - SystemUI TouchpadEventReceiver: mOnboardingComplete (enables all gestures)
     * - PersistenceService: bindCentralIfProvisioned() (starts CentralService)
     * - LaserSoundFeedbackManager: switches from PROACTIVE to SUBTLE guidance
     *
     * The ContentObserver in SystemUI fires immediately on change.
     */
    @Suppress("unused")
    private fun fixDucProvisioned(app: Application) {
        try {
            val resolver = app.contentResolver
            val key = "humane.settings.global.DUC_PROVISIONED"
            val current = Settings.Global.getInt(resolver, key, 0)

            Log.w(TAG, "  Current DUC_PROVISIONED: $current")

            if (current != 0) {
                Log.w(TAG, "  DUC_PROVISIONED already set ($current), no change needed")
                return
            }

            val success = Settings.Global.putInt(resolver, key, 1)
            if (success) {
                Log.w(TAG, "  DUC_PROVISIONED set to 1 — SUCCESS")
            } else {
                Log.w(TAG, "  DUC_PROVISIONED putInt returned false — may lack permission")
            }
        } catch (t: Throwable) {
            Log.e(TAG, "  DUC_PROVISIONED fix failed: ${t.javaClass.simpleName}: ${t.message}")
        }
    }

    /**
     * Hook DeviceAttestationManager.generateVerifierSignature() as a safety net.
     *
     * During onboarding, UserBindingManager calls dacManager.generateVerifierSignature()
     * to sign the device ID with the DAC private key. If the DAC private key is
     * missing from the HumaneKeyStore TEE, this would crash.
     *
     * We hook it to return a dummy signature if it throws. The mock server
     * ignores signatures anyway.
     */
    private fun hookDacSignature(cl: ClassLoader) {
        val className = "humaneinternal.system.credentials.DeviceAttestationManager"
        val clazz = try {
            cl.loadClass(className)
        } catch (e: ClassNotFoundException) {
            Log.w(TAG, "  $className not found, skipping DAC signature hook")
            return
        }

        // generateVerifierSignature(byte[]) -> byte[]
        // It calls CryptoUtils.generateSignature(payload, dacPrivateKey) internally
        try {
            val method = clazz.getDeclaredMethod("generateVerifierSignature", ByteArray::class.java)
            method.isAccessible = true
            XposedBridge.hookMethod(method, object : XC_MethodHook() {
                override fun afterHookedMethod(param: MethodHookParam) {
                    if (param.throwable != null) {
                        // DAC private key missing — return a dummy ECDSA-like signature
                        param.throwable = null
                        param.result = ByteArray(64) { 0x01 }
                        Log.w(TAG, "  DAC generateVerifierSignature() threw ${param.throwable?.javaClass?.simpleName} — returning dummy signature")
                    } else {
                        Log.w(TAG, "  DAC generateVerifierSignature() succeeded with real signature")
                    }
                }
            })
            Log.w(TAG, "  Hooked $className.generateVerifierSignature() (safety net)")
        } catch (t: Throwable) {
            Log.e(TAG, "  Failed to hook generateVerifierSignature: ${t.message}")
        }
    }

    /**
     * Silence Memfault's embedded RemoteMetricsService.
     *
     * The Memfault reporting library is compiled into ironman (not in the Bort APK).
     * It records metrics via record$reporting_lib_release() and periodically
     * finishes reports via finishReport$reporting_lib_release(). Both call
     * withRemoteLogger() which looks up the "memfault_structured" binder service.
     * With the daemon stopped, every call logs:
     *   "Unable to get a handle to memfault_structured"
     *
     * Hook both entry points to no-op, eliminating the binder lookup and log noise.
     */
    private fun hookRemoteMetricsService(cl: ClassLoader) {
        val className = "com.memfault.bort.reporting.RemoteMetricsService"
        val clazz = try {
            cl.loadClass(className)
        } catch (_: ClassNotFoundException) {
            Log.w(TAG, "  $className not found, skipping RemoteMetrics hook")
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
                param.result = false
            }
        } catch (t: Throwable) {
            Log.w(TAG, "  Failed to hook RemoteMetricsService.finishReport: ${t.message}")
        }

        Log.w(TAG, "  RemoteMetricsService hooks installed (metrics silenced)")
    }

}

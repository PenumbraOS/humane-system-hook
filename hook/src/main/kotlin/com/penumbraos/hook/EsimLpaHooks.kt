package com.penumbraos.hook

import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import android.telephony.TelephonyManager
import android.util.Log
import java.lang.reflect.Method
import java.util.ArrayList
import java.util.Collections
import java.util.concurrent.atomic.AtomicBoolean

/**
 * Hooks for the eSIM LPA process (package: humane.connectivity.esimlpa).
 */
object EsimLpaHooks {

    private const val TAG = "PenumbraHook"
    private const val MAX_ITERATIONS = 200
    private const val PROTECTED_T_MOBILE_NAME = "t-mobile"
    private const val PROTECTED_GSMA_TEST_PREFIX = "gsma test profile"
    private const val DEVICE_IDENTIFIER_IMEI_KEY = "humane.esim.IMEI"

    // Hex encoding of ASCII "Humane"
    private const val HUMANE_HEX = "48756D616E65"

    /**
     * Scoping flag for the carrier lock bypass. getProfileName() hook patch
     * only applies when this is flag is true. Set to true at the
     * start of downloadVerifyAndEnableProfileAPI, cleared at the start of
     * every onStartCommand (every new intent).
     */
    private val bypassActive = AtomicBoolean(false)
    private val installedProfileControllerListeners = Collections.newSetFromMap(java.util.WeakHashMap<Any, Boolean>())
    private val installedDownloadControllerListeners = Collections.newSetFromMap(java.util.WeakHashMap<Any, Boolean>())
    private val installedCommunicationManagerListeners = Collections.newSetFromMap(java.util.WeakHashMap<Any, Boolean>())

    // -- Cached reflection handles (resolved once in install()) --

    // Util static methods
    private lateinit var getBERLengthInIntMethod: Method      // Util.getBERLengthInInt(String, int)
    private lateinit var getBERLengthSizeStrMethod: Method     // Util.getBERLengthSizeInNibbles(String, int)
    private lateinit var getBERLengthSizeIntMethod: Method     // Util.getBERLengthSizeInNibbles(int)

    // FillerEngine private fill* methods
    private lateinit var fillIccidMethod: Method
    private lateinit var fillHexStringMethod: Method
    private lateinit var fillIconTypeMethod: Method
    private lateinit var fillIconMethod: Method
    private lateinit var fillProfileClassMethod: Method
    private lateinit var fillPprIdsMethod: Method
    private lateinit var fillNotificationConfigInfoMethod: Method
    private lateinit var fillOperatorIdMethod: Method

    // Data class getSize methods
    private lateinit var iccidGetSizeMethod: Method
    private lateinit var hexStringGetSizeMethod: Method
    private lateinit var iconTypeGetSizeMethod: Method
    private lateinit var profileClassGetSizeMethod: Method
    private lateinit var pprIdsGetSizeMethod: Method
    private lateinit var operatorIdGetSizeMethod: Method
    private lateinit var notifConfigGetSizeMethod: Method  // on NotificationConfigurationInformation

    // StoreMetadataRequest setters
    private lateinit var smrSetIccid: Method
    private lateinit var smrSetServiceProviderName: Method
    private lateinit var smrSetProfileName: Method
    private lateinit var smrSetIconType: Method
    private lateinit var smrSetIcon: Method
    private lateinit var smrSetProfileClass: Method
    private lateinit var smrSetProfilePolicyRules: Method
    private lateinit var smrSetNotificationConfigInfo: Method
    private lateinit var smrSetProfileOwner: Method

    // Classes
    private lateinit var storeMetadataRequestClass: Class<*>

    fun install(cl: ClassLoader) {
        Log.w(TAG, "Installing eSIM LPA hooks...")

        try {
            TcmSilencer.install(cl)
            resolveClasses(cl)
            hookActionCapture(cl)
            hookSyspropCapture(cl)
            hookProfileMutationCapture(cl)
            hookDownloadCapture(cl)
            hookDeleteProtection(cl)
            hookCarrierLock(cl)
            hookBF25Parser(cl)
            Log.w(TAG, "eSIM LPA hooks installed")
        } catch (t: Throwable) {
            Log.e(TAG, "Failed to install eSIM LPA hooks", t)
        }
    }

    private fun resolveClasses(cl: ClassLoader) {
        val fillerEngineClass = cl.loadClass("es.com.valid.lib_lpa.controler.FillerEngine")
        val utilClass = cl.loadClass("es.com.valid.lib_lpa.controler.Util")
        storeMetadataRequestClass = cl.loadClass("es.com.valid.lib_lpa.dataClasses.StoreMetadataRequest")
        val iccidClass = cl.loadClass("es.com.valid.lib_lpa.dataClasses.Iccid")
        val hexStringClass = cl.loadClass("es.com.valid.lib_lpa.dataClasses.HexString")
        val iconTypeClass = cl.loadClass("es.com.valid.lib_lpa.dataClasses.IconType")
        val profileClassClass = cl.loadClass("es.com.valid.lib_lpa.dataClasses.ProfileClass")
        val pprIdsClass = cl.loadClass("es.com.valid.lib_lpa.dataClasses.PprIds")
        val notifConfigClass = cl.loadClass("es.com.valid.lib_lpa.dataClasses.NotificationConfigurationInformation")
        val operatorIdClass = cl.loadClass("es.com.valid.lib_lpa.dataClasses.OperatorId")

        // Util static methods
        getBERLengthInIntMethod = utilClass.getDeclaredMethod("getBERLengthInInt", String::class.java, Int::class.javaPrimitiveType!!)
        getBERLengthInIntMethod.isAccessible = true
        getBERLengthSizeStrMethod = utilClass.getDeclaredMethod("getBERLengthSizeInNibbles", String::class.java, Int::class.javaPrimitiveType!!)
        getBERLengthSizeStrMethod.isAccessible = true
        getBERLengthSizeIntMethod = utilClass.getDeclaredMethod("getBERLengthSizeInNibbles", Int::class.javaPrimitiveType!!)
        getBERLengthSizeIntMethod.isAccessible = true

        // FillerEngine private methods
        fillIccidMethod = fillerEngineClass.getDeclaredMethod("fillIccid", String::class.java, Int::class.javaPrimitiveType!!)
        fillIccidMethod.isAccessible = true
        fillHexStringMethod = fillerEngineClass.getDeclaredMethod("fillHexString", String::class.java, Int::class.javaPrimitiveType!!)
        fillHexStringMethod.isAccessible = true
        fillIconTypeMethod = fillerEngineClass.getDeclaredMethod("fillIconType", String::class.java, Int::class.javaPrimitiveType!!)
        fillIconTypeMethod.isAccessible = true
        fillIconMethod = fillerEngineClass.getDeclaredMethod("fillIcon", String::class.java, Int::class.javaPrimitiveType!!)
        fillIconMethod.isAccessible = true
        fillProfileClassMethod = fillerEngineClass.getDeclaredMethod("fillProfileClass", String::class.java, Int::class.javaPrimitiveType!!)
        fillProfileClassMethod.isAccessible = true
        fillPprIdsMethod = fillerEngineClass.getDeclaredMethod("fillPprIds", String::class.java, Int::class.javaPrimitiveType!!)
        fillPprIdsMethod.isAccessible = true
        fillNotificationConfigInfoMethod = fillerEngineClass.getDeclaredMethod("fillNotificationConfigurationInfo", String::class.java, Int::class.javaPrimitiveType!!)
        fillNotificationConfigInfoMethod.isAccessible = true
        fillOperatorIdMethod = fillerEngineClass.getDeclaredMethod("fillOperatorId", String::class.java, Int::class.javaPrimitiveType!!)
        fillOperatorIdMethod.isAccessible = true

        // getSize methods on data classes
        iccidGetSizeMethod = iccidClass.getMethod("getSize")
        hexStringGetSizeMethod = hexStringClass.getMethod("getSize")
        iconTypeGetSizeMethod = iconTypeClass.getMethod("getSize")
        profileClassGetSizeMethod = profileClassClass.getMethod("getSize")
        pprIdsGetSizeMethod = pprIdsClass.getMethod("getSize")
        operatorIdGetSizeMethod = operatorIdClass.getMethod("getSize")
        notifConfigGetSizeMethod = notifConfigClass.getMethod("getSize")

        // StoreMetadataRequest setters
        smrSetIccid = storeMetadataRequestClass.getMethod("setIccid", iccidClass)
        smrSetServiceProviderName = storeMetadataRequestClass.getMethod("setServiceProviderName", hexStringClass)
        smrSetProfileName = storeMetadataRequestClass.getMethod("setProfileName", hexStringClass)
        smrSetIconType = storeMetadataRequestClass.getMethod("setIconType", iconTypeClass)
        smrSetIcon = storeMetadataRequestClass.getMethod("setIcon", ByteArray::class.java)
        smrSetProfileClass = storeMetadataRequestClass.getMethod("setProfileClass", profileClassClass)
        smrSetProfilePolicyRules = storeMetadataRequestClass.getMethod("setProfilePolicyRules", pprIdsClass)
        smrSetNotificationConfigInfo = storeMetadataRequestClass.getMethod(
            "setNotificationConfigurationInfo",
            java.lang.reflect.Array.newInstance(notifConfigClass, 0).javaClass
        )
        smrSetProfileOwner = storeMetadataRequestClass.getMethod("setProfileOwner", operatorIdClass)

        Log.w(TAG, "  eSIM LPA reflection resolved successfully")
    }

    private fun hookActionCapture(cl: ClassLoader) {
        val factoryServiceClass = cl.loadClass("humane.connectivity.esimlpa.factoryService")

        HookUtils.hookMethodBefore(
            factoryServiceClass,
            "onStartCommand",
            arrayOf(Intent::class.java, Int::class.javaPrimitiveType!!, Int::class.javaPrimitiveType!!)
        ) { param ->
            val intent = param.args[0] as? Intent
            EsimOperationContext.clear()
            EsimOperationContext.currentAction = intent?.action
            EsimOperationContext.currentRequestId = intent?.getStringExtra("penumbra_request_id")
            EsimOperationContext.currentIccid = intent?.getStringExtra("iccid")
            EsimOperationContext.currentActivationCode = intent?.getStringExtra("activationCode")
            EsimOperationContext.currentNickname = intent?.getStringExtra("Nickname")
            EsimOperationContext.currentSource = intent?.getStringExtra("penumbra_source")
            EsimEventEmitter.emitActionStarted()
            if (EsimOperationContext.currentAction == "humane.connectivity.esimlpa.getEID") {
                emitImeiIdentifier(param.thisObject)
            }
        }

        Log.w(TAG, "  eSIM action capture installed")
    }

    private fun hookSyspropCapture(cl: ClassLoader) {
        val factoryServiceClass = cl.loadClass("humane.connectivity.esimlpa.factoryService")

        HookUtils.hookMethodAfter(
            factoryServiceClass,
            "setSysProp",
            arrayOf(String::class.java, String::class.java)
        ) { param ->
            val key = param.args[0] as? String ?: return@hookMethodAfter
            val value = param.args[1] as? String
            EsimEventEmitter.emitSyspropUpdate(key, value)
        }

        Log.w(TAG, "  eSIM sysprop capture installed")
    }

    private fun hookProfileMutationCapture(cl: ClassLoader) {
        val profileInfoControlerClass = cl.loadClass("es.com.valid.lib_lpa.controler.ProfileInfoControler")
        val listenerInterfaceClass = cl.loadClass("es.com.valid.lib_lpa.controler.ProfileInfoControler\$ProfileInfoControlerListener")

        HookUtils.hookMethodAfter(
            profileInfoControlerClass,
            "setProfileInfoControlerListener",
            arrayOf(listenerInterfaceClass)
        ) { param ->
            val listener = param.args[0] ?: return@hookMethodAfter
            installProfileInfoListenerHooks(listener)
        }

        Log.w(TAG, "  eSIM profile mutation capture installed")
    }

    private fun currentMutationOperation(): String {
        return when (EsimOperationContext.currentAction) {
            "humane.connectivity.esimlpa.enableProfile" -> "enable"
            "humane.connectivity.esimlpa.disableProfile",
            "humane.connectivity.esimlpa.disableActiveProfile" -> "disable"
            "humane.connectivity.esimlpa.deleteProfile" -> "delete"
            "humane.connectivity.esimlpa.setNickname" -> "set_nickname"
            else -> "unknown"
        }
    }

    private fun classifyMutationResult(message: String?): String {
        val normalized = message?.lowercase().orEmpty()
        return when {
            normalized.isEmpty() -> "success"
            "error" in normalized || "fail" in normalized || "not exist" in normalized -> "error"
            else -> "success"
        }
    }

    private fun hookDownloadCapture(cl: ClassLoader) {
        val downloadControlerClass = cl.loadClass("es.com.valid.lib_lpa.controler.DownloadControler")
        val downloadListenerInterfaceClass = cl.loadClass("es.com.valid.lib_lpa.controler.DownloadControler\$DownloadControlerListener")
        val communicationManagerClass = cl.loadClass("es.com.valid.lib_lpa.cardCommunication.CommunicationManager")
        val communicationManagerListenerClass = cl.loadClass("es.com.valid.lib_lpa.cardCommunication.CommunicationManager\$CommunicationManagerListener")

        HookUtils.hookMethodAfter(
            downloadControlerClass,
            "setDownloadControlerListener",
            arrayOf(downloadListenerInterfaceClass)
        ) { param ->
            val listener = param.args[0] ?: return@hookMethodAfter
            installDownloadListenerHooks(listener)
        }

        HookUtils.hookMethodAfter(
            communicationManagerClass,
            "setCommunicationManagerListener",
            arrayOf(communicationManagerListenerClass)
        ) { param ->
            val listener = param.args[0] ?: return@hookMethodAfter
            installCommunicationManagerListenerHooks(listener)
        }

        Log.w(TAG, "  eSIM download capture installed")
    }

    private fun classifyDownloadResult(message: String?): String {
        val normalized = message?.lowercase().orEmpty()
        return when {
            "disallowed" in normalized -> "disallowed_profile"
            else -> "error"
        }
    }

    private fun hookDeleteProtection(cl: ClassLoader) {
        val factoryServiceClass = cl.loadClass("humane.connectivity.esimlpa.factoryService")
        val communicationManagerClass = cl.loadClass("es.com.valid.lib_lpa.cardCommunication.CommunicationManager")
        val profileInfoClass = cl.loadClass("es.com.valid.lib_lpa.dataClasses.ProfileInfo")
        val hexStringClass = cl.loadClass("es.com.valid.lib_lpa.dataClasses.HexString")
        val utilClass = cl.loadClass("es.com.valid.lib_lpa.common.Util")

        val getApplicationContextMethod = factoryServiceClass.getMethod("getApplicationContext")
        val getInstanceMethod = communicationManagerClass.getMethod("getInstance", android.content.Context::class.java)
        val openConnectionMethod = communicationManagerClass.getMethod("openConnection")
        val closeConnectionMethod = communicationManagerClass.getMethod("closeConnection")
        val getProfileListAsArrayMethod = communicationManagerClass.getMethod("getProfileListAsArray")
        val getIccidMethod = profileInfoClass.getMethod("getIccid")
        val getProfileNameMethod = profileInfoClass.getMethod("getProfileName")
        val getValueRotatedMethod = cl.loadClass("es.com.valid.lib_lpa.dataClasses.Iccid").getMethod("getValueRotated")
        val getHexValueMethod = hexStringClass.getMethod("getValue")
        val hexToAsciiMethod = utilClass.getMethod("HexToAscII", String::class.java)

        HookUtils.hookMethodBefore(
            factoryServiceClass,
            "deleteProfileUtil",
            arrayOf(String::class.java)
        ) { param ->
            val targetIccid = param.args[0] as? String ?: return@hookMethodBefore
            val context = getApplicationContextMethod.invoke(param.thisObject) as? android.content.Context ?: return@hookMethodBefore
            val communicationManager = getInstanceMethod.invoke(null, context) ?: return@hookMethodBefore

            var opened = false
            try {
                openConnectionMethod.invoke(communicationManager)
                opened = true
                @Suppress("UNCHECKED_CAST")
                val profiles = getProfileListAsArrayMethod.invoke(communicationManager) as? ArrayList<Any> ?: return@hookMethodBefore
                val profile = profiles.firstOrNull { candidate ->
                    val iccidObject = getIccidMethod.invoke(candidate) ?: return@firstOrNull false
                    val rotated = getValueRotatedMethod.invoke(iccidObject) as? String ?: return@firstOrNull false
                    rotated.equals(targetIccid, ignoreCase = true)
                } ?: return@hookMethodBefore

                val profileName = decodeProfileName(getProfileNameMethod.invoke(profile), getHexValueMethod, hexToAsciiMethod)
                if (!isProtectedProfileName(profileName)) {
                    return@hookMethodBefore
                }

                val protectedName = profileName?.ifBlank { null } ?: "unknown"
                val message = "Deletion blocked for protected profile: $protectedName"
                Log.w(TAG, "  Protected delete blocked iccid=$targetIccid name=$protectedName")
                EsimEventEmitter.emitProfileMutationResult("delete", "protected", message)
                param.result = null
            } catch (t: Throwable) {
                Log.w(TAG, "  Protected delete inspection failed; allowing delete", t)
            } finally {
                if (opened) {
                    try {
                        closeConnectionMethod.invoke(communicationManager)
                    } catch (closeError: Throwable) {
                        Log.w(TAG, "  Failed to close connection after delete protection check", closeError)
                    }
                }
            }
        }

        Log.w(TAG, "  eSIM delete protection installed")
    }

    private fun installProfileInfoListenerHooks(listener: Any) {
        synchronized(installedProfileControllerListeners) {
            if (!installedProfileControllerListeners.add(listener)) {
                return
            }
        }

        val listenerClass = listener.javaClass
        HookUtils.hookMethodAfter(listenerClass, "onEnable", arrayOf(String::class.java)) { param ->
            val message = param.args[0] as? String
            EsimEventEmitter.emitProfileMutationResult("enable", classifyMutationResult(message), message)
        }
        HookUtils.hookMethodAfter(listenerClass, "onDisable", arrayOf(String::class.java)) { param ->
            val message = param.args[0] as? String
            EsimEventEmitter.emitProfileMutationResult("disable", classifyMutationResult(message), message)
        }
        HookUtils.hookMethodAfter(listenerClass, "onDelete", arrayOf(String::class.java)) { param ->
            val message = param.args[0] as? String
            EsimEventEmitter.emitProfileMutationResult("delete", classifyMutationResult(message), message)
        }
        HookUtils.hookMethodAfter(listenerClass, "onsetNickName", arrayOf(String::class.java)) { param ->
            val message = param.args[0] as? String
            EsimEventEmitter.emitProfileMutationResult("set_nickname", classifyMutationResult(message), message)
        }
        HookUtils.hookMethodAfter(listenerClass, "onError", arrayOf(String::class.java)) { param ->
            val message = param.args[0] as? String
            EsimEventEmitter.emitProfileMutationResult(currentMutationOperation(), "error", message)
        }
    }

    private fun installDownloadListenerHooks(listener: Any) {
        synchronized(installedDownloadControllerListeners) {
            if (!installedDownloadControllerListeners.add(listener)) {
                return
            }
        }

        val listenerClass = listener.javaClass
        val listenerClassLoader = listenerClass.classLoader ?: return
        val storeMetadataRequestClass = listenerClassLoader.loadClass("es.com.valid.lib_lpa.dataClasses.StoreMetadataRequest")
        val pprResultClass = listenerClassLoader.loadClass("es.com.valid.lib_lpa.dataClasses.PprResult")
        val getIccidMethod = storeMetadataRequestClass.getMethod("getIccid")

        HookUtils.hookMethodAfter(listenerClass, "onProgress", arrayOf(Int::class.javaPrimitiveType!!)) { param ->
            val progress = param.args[0] as? Int
            EsimEventEmitter.emitDownloadProgress("download_progress", progress)
        }
        HookUtils.hookMethodAfter(
            listenerClass,
            "onMutualAuthenticationCompleted",
            arrayOf(storeMetadataRequestClass, pprResultClass)
        ) { param ->
            val storeMetadataRequest = param.args[0] ?: return@hookMethodAfter
            val iccid = try {
                val iccidObject = getIccidMethod.invoke(storeMetadataRequest)
                iccidObject?.javaClass?.getMethod("getValueRotated")?.invoke(iccidObject) as? String
            } catch (t: Throwable) {
                Log.w(TAG, "  Failed to extract download ICCID", t)
                null
            }
            EsimOperationContext.currentDownloadIccid = iccid
            EsimEventEmitter.emitDownloadProgress("mutual_auth_completed", message = iccid)
        }
        HookUtils.hookMethodAfter(listenerClass, "onFinished", arrayOf(String::class.java)) { param ->
            val message = param.args[0] as? String
            EsimEventEmitter.emitDownloadProgress("finished", message = message)
            EsimEventEmitter.emitDownloadResult("success", message)
        }
        HookUtils.hookMethodAfter(listenerClass, "onError", arrayOf(String::class.java)) { param ->
            val message = param.args[0] as? String
            EsimEventEmitter.emitDownloadResult(classifyDownloadResult(message), message)
        }
    }

    private fun installCommunicationManagerListenerHooks(listener: Any) {
        synchronized(installedCommunicationManagerListeners) {
            if (!installedCommunicationManagerListeners.add(listener)) {
                return
            }
        }

        val listenerClass = listener.javaClass
        HookUtils.hookMethodAfter(listenerClass, "onProgress", arrayOf(Int::class.javaPrimitiveType!!)) { param ->
            val progress = param.args[0] as? Int
            EsimEventEmitter.emitDownloadProgress("communication_progress", progress)
        }
        HookUtils.hookMethodAfter(listenerClass, "onError", arrayOf(String::class.java)) { param ->
            val message = param.args[0] as? String
            EsimEventEmitter.emitDownloadResult("error", message)
        }
    }

    private fun emitImeiIdentifier(factoryService: Any) {
        try {
            val context = factoryService.javaClass.getMethod("getApplicationContext").invoke(factoryService) as? android.content.Context
                ?: return
            val telephonyManager = context.getSystemService("phone") as? TelephonyManager
            val imei = getCurrentImei(context, telephonyManager)
            if (!imei.isNullOrBlank()) {
                EsimEventEmitter.emitDeviceIdentifier(DEVICE_IDENTIFIER_IMEI_KEY, imei)
            }
        } catch (t: Throwable) {
            Log.w(TAG, "  Failed to emit IMEI identifier", t)
        }
    }

    private fun getCurrentImei(context: android.content.Context, telephonyManager: TelephonyManager?): String? {
        if (telephonyManager == null) return null
        if (context.checkSelfPermission("android.permission.READ_PHONE_STATE") != PackageManager.PERMISSION_GRANTED) {
            return null
        }

        return try {
            val imei = if (Build.VERSION.SDK_INT >= 26) {
                val tmWrapperClass = context.classLoader?.loadClass("es.com.valid.lib_lpa.cardCommunication.TMWrapper")
                val slotIndex = tmWrapperClass?.getMethod("getSlotIndex")?.invoke(null) as? Int ?: -2
                if (slotIndex == -2) {
                    telephonyManager.imei
                } else {
                    telephonyManager.getImei(slotIndex)
                }
            } else {
                @Suppress("DEPRECATION")
                telephonyManager.deviceId
            }
            imei?.let {
                if (it.length % 2 == 1) "${it}F" else it
            }
        } catch (t: Throwable) {
            Log.w(TAG, "  Failed to read IMEI", t)
            null
        }
    }

    private fun decodeProfileName(
        profileNameObject: Any?,
        getHexValueMethod: Method,
        hexToAsciiMethod: Method,
    ): String? {
        if (profileNameObject == null) return null
        return try {
            val hexValue = getHexValueMethod.invoke(profileNameObject) as? String ?: return null
            if (hexValue.isBlank()) return null
            (hexToAsciiMethod.invoke(null, hexValue) as? String)?.trim()
        } catch (t: Throwable) {
            Log.w(TAG, "  Failed to decode profile name", t)
            null
        }
    }

    private fun isProtectedProfileName(profileName: String?): Boolean {
        val normalized = profileName?.trim()?.lowercase().orEmpty()
        if (normalized.isEmpty()) return false
        return normalized.startsWith(PROTECTED_T_MOBILE_NAME) || normalized.startsWith(PROTECTED_GSMA_TEST_PREFIX)
    }

    /**
     * Hook ProfileInfo.getProfileName() to return a HexString with value
     * "Humane" when in the critical section. This makes the carrier lock check in
     * factoryService line 839 always pass (the `!equals("Humane")` condition is
     * always false), so the delete branch is unreachable for any profile.
     */
    private fun hookCarrierLock(cl: ClassLoader) {
        val factoryServiceClass = cl.loadClass("humane.connectivity.esimlpa.factoryService")
        val profileInfoClass = cl.loadClass("es.com.valid.lib_lpa.dataClasses.ProfileInfo")
        val hexStringClass = cl.loadClass("es.com.valid.lib_lpa.dataClasses.HexString")
        val setValueMethod = hexStringClass.getMethod("setValue", String::class.java)

        // Clear the bypass flag at the start of every new intent
        HookUtils.hookMethodBefore(
            factoryServiceClass,
            "onStartCommand",
            arrayOf(Intent::class.java, Int::class.javaPrimitiveType!!, Int::class.javaPrimitiveType!!)
        ) { _ ->
            if (bypassActive.getAndSet(false)) {
                Log.w(TAG, "  Carrier lock bypass: cleared stale flag on new intent")
            }
        }

        // Set the bypass flag when the download-verify-enable flow starts
        HookUtils.hookMethodBefore(
            factoryServiceClass,
            "downloadVerifyAndEnableProfileAPI",
            arrayOf(String::class.java)
        ) { _ ->
            bypassActive.set(true)
            Log.w(TAG, "  Carrier lock bypass: activated for downloadVerifyAndEnableProfile")
        }

        // Patch getProfileName() only when bypass is active
        HookUtils.hookMethodAfter(profileInfoClass, "getProfileName", emptyArray()) { param ->
            if (bypassActive.get()) {
                val hexString = param.result
                if (hexString != null) {
                    setValueMethod.invoke(hexString, HUMANE_HEX)
                    Log.w(TAG, "  Carrier lock bypass: getProfileName() -> \"Humane\"")
                }
            }
        }

        Log.w(TAG, "  Carrier lock bypass installed")
    }

    /**
     * Replace FillerEngine.fillStoreMetadataRequest(String, int) with a version
     * that properly handles unknown TLV tags (including multi-byte tags like BF76)
     * by skipping them instead of infinite-looping.
     */
    private fun hookBF25Parser(cl: ClassLoader) {
        val fillerEngineClass = cl.loadClass("es.com.valid.lib_lpa.controler.FillerEngine")

        HookUtils.hookMethodBefore(
            fillerEngineClass,
            "fillStoreMetadataRequest",
            arrayOf(String::class.java, Int::class.javaPrimitiveType!!)
        ) { param ->
            val str = param.args[0] as String
            val i = param.args[1] as Int
            val fillerEngine = param.thisObject

            param.result = parseBF25(fillerEngine, str, i)
        }

        Log.w(TAG, "  BF25 parser fix installed (FillerEngine.fillStoreMetadataRequest)")
    }

    /**
     * Replacement algorithm for fillStoreMetadataRequest, with proper
     * multi-byte BER-TLV tag support.
     */
    private fun parseBF25(fillerEngine: Any, str: String, i: Int): Any {
        val smr = storeMetadataRequestClass.getDeclaredConstructor().newInstance()

        // Verify BF25 tag at offset
        val i4 = i + 4
        if (str.substring(i, i4).uppercase() != "BF25") {
            throw Exception("Invalid Tag for Profiles Metadata")
        }

        // Parse BF25 envelope
        val lengthBytes = getBERLengthInInt(str, i4)
        val lengthFieldSize = getBERLengthSizeInNibbles(str, i4)
        val dataStart = i4 + lengthFieldSize
        val dataEnd = dataStart + lengthBytes * 2

        Log.w(TAG, "  BF25 parser: length=$lengthBytes bytes, dataStart=$dataStart, dataEnd=$dataEnd")

        var offset = dataStart
        var iterations = 0

        while (offset < dataEnd && iterations < MAX_ITERATIONS) {
            iterations++

            if (offset + 2 > str.length) {
                Log.e(TAG, "  BF25 parser: offset $offset out of bounds (str.length=${str.length})")
                break
            }

            // Read first byte of tag
            val firstTagByte = str.substring(offset, offset + 2).uppercase()
            val firstByte = firstTagByte.toInt(16)

            // Check for multi-byte tag: low 5 bits of first byte all set
            val tag: String
            val tagNibbles: Int
            if ((firstByte and 0x1F) == 0x1F) {
                // Multi-byte tag (e.g., BF76)
                if (offset + 4 > str.length) {
                    Log.e(TAG, "  BF25 parser: not enough data for multi-byte tag at $offset")
                    break
                }
                tag = str.substring(offset, offset + 4).uppercase()
                tagNibbles = 4
            } else {
                tag = firstTagByte
                tagNibbles = 2
            }

            Log.w(TAG, "  BF25 parser: iteration=$iterations, offset=$offset, tag=$tag")

            var consumed = 0

            try {
                when (tag) {
                    "5A" -> { // ICCID
                        val obj = fillIccidMethod.invoke(fillerEngine, str, offset)
                        consumed = iccidGetSizeMethod.invoke(obj) as Int
                        smrSetIccid.invoke(smr, obj)
                    }
                    "91" -> { // ServiceProviderName
                        val obj = fillHexStringMethod.invoke(fillerEngine, str, offset)
                        consumed = hexStringGetSizeMethod.invoke(obj) as Int
                        smrSetServiceProviderName.invoke(smr, obj)
                    }
                    "92" -> { // ProfileName
                        val obj = fillHexStringMethod.invoke(fillerEngine, str, offset)
                        consumed = hexStringGetSizeMethod.invoke(obj) as Int
                        smrSetProfileName.invoke(smr, obj)
                    }
                    "93" -> { // IconType
                        val obj = fillIconTypeMethod.invoke(fillerEngine, str, offset)
                        consumed = iconTypeGetSizeMethod.invoke(obj) as Int
                        smrSetIconType.invoke(smr, obj)
                    }
                    "94" -> { // Icon — manual size (getSize reports inner data, not full TLV)
                        val obj = fillIconMethod.invoke(fillerEngine, str, offset) as ByteArray
                        val lenOffset = offset + 2
                        val lenBytes = getBERLengthInInt(str, lenOffset)
                        val lenFieldSize = getBERLengthSizeInNibbles(str, lenOffset)
                        consumed = 2 + lenFieldSize + lenBytes * 2
                        smrSetIcon.invoke(smr, obj)
                    }
                    "95" -> { // ProfileClass
                        val obj = fillProfileClassMethod.invoke(fillerEngine, str, offset)
                        consumed = profileClassGetSizeMethod.invoke(obj) as Int
                        smrSetProfileClass.invoke(smr, obj)
                    }
                    "99" -> { // ProfilePolicyRules
                        val obj = fillPprIdsMethod.invoke(fillerEngine, str, offset)
                        consumed = pprIdsGetSizeMethod.invoke(obj) as Int
                        smrSetProfilePolicyRules.invoke(smr, obj)
                    }
                    "B6" -> { // NotificationConfigurationInfo — manual size
                        val obj = fillNotificationConfigInfoMethod.invoke(fillerEngine, str, offset)
                        // obj is NotificationConfigurationInformation[]
                        val arr = obj as Array<*>
                        var innerBytes = 0
                        for (item in arr) {
                            if (item != null) {
                                innerBytes += (notifConfigGetSizeMethod.invoke(item) as Int) / 2
                            }
                        }
                        val berLenSize = getBERLengthSizeInNibbles(innerBytes)
                        consumed = 2 + berLenSize + innerBytes * 2
                        smrSetNotificationConfigInfo.invoke(smr, obj)
                    }
                    "B7" -> { // OperatorId
                        val obj = fillOperatorIdMethod.invoke(fillerEngine, str, offset)
                        consumed = operatorIdGetSizeMethod.invoke(obj) as Int
                        smrSetProfileOwner.invoke(smr, obj)
                    }
                    else -> {
                        // Unknown tag — skip it properly
                        val lengthOffset = offset + tagNibbles
                        val lenBytes = getBERLengthInInt(str, lengthOffset)
                        val lenFieldSize = getBERLengthSizeInNibbles(str, lengthOffset)
                        consumed = tagNibbles + lenFieldSize + lenBytes * 2
                        Log.w(TAG, "  BF25 parser: skipping unknown tag $tag, consumed=$consumed nibbles")
                    }
                }
            } catch (e: Throwable) {
                // On error, try to skip the tag
                Log.e(TAG, "  BF25 parser: error processing tag $tag at offset $offset: ${e.message}")
                try {
                    val lengthOffset = offset + tagNibbles
                    val lenBytes = getBERLengthInInt(str, lengthOffset)
                    val lenFieldSize = getBERLengthSizeInNibbles(str, lengthOffset)
                    consumed = tagNibbles + lenFieldSize + lenBytes * 2
                    Log.w(TAG, "  BF25 parser: skipping errored tag $tag, consumed=$consumed nibbles")
                } catch (skipError: Throwable) {
                    Log.e(TAG, "  BF25 parser: cannot skip tag $tag after error, breaking", skipError)
                    break
                }
            }

            if (consumed > 0) {
                offset += consumed
            } else {
                Log.e(TAG, "  BF25 parser: tag $tag consumed 0 nibbles, breaking to prevent infinite loop")
                break
            }
        }

        if (iterations >= MAX_ITERATIONS) {
            Log.w(TAG, "  BF25 parser: max iterations ($MAX_ITERATIONS) reached")
        }

        Log.w(TAG, "  BF25 parser: completed, processed $iterations tags")
        return smr
    }

    private fun getBERLengthInInt(str: String, offset: Int): Int {
        return getBERLengthInIntMethod.invoke(null, str, offset) as Int
    }

    private fun getBERLengthSizeInNibbles(str: String, offset: Int): Int {
        return getBERLengthSizeStrMethod.invoke(null, str, offset) as Int
    }

    private fun getBERLengthSizeInNibbles(lengthInBytes: Int): Int {
        return getBERLengthSizeIntMethod.invoke(null, lengthInBytes) as Int
    }
}

package com.penumbraos.hook

import android.util.Log
import de.robv.android.xposed.XC_MethodHook
import de.robv.android.xposed.XposedBridge
import java.lang.reflect.Method
import java.nio.ByteBuffer

/**
 * Bypass the Krypton ephemeral encryption system so all "encrypted" RPCs
 * send and receive plaintext.
 */
object EphemeralProtectionBypass {

    private const val TAG = "PenumbraHook"

    fun install(cl: ClassLoader) {
        val epmClassName = "humaneinternal.system.krypto.ephemeral.EphemeralProtectionManager"
        val epmClass = try {
            cl.loadClass(epmClassName)
        } catch (_: ClassNotFoundException) {
            Log.w(TAG, "  $epmClassName not found, skipping ephemeral protection bypass")
            return
        }

        val channelIdClass = cl.loadClass("hu.ma.ne.krypton.ephemeral.EphemeralChannelId")
        val generatedMessageLiteClass = cl.loadClass("com.google.protobuf.GeneratedMessageLite")
        val encryptedDataClass = cl.loadClass("humane.common.encryption.EncryptedData")
        val encryptionInfoClass = cl.loadClass("humane.common.encryption.EncryptionInformation")

        // Pre-resolve builder methods for EncryptedData and EncryptionInformation
        val edNewBuilder = encryptedDataClass.getMethod("newBuilder")
        val eiNewBuilder = encryptionInfoClass.getMethod("newBuilder")

        // ByteString.copyFrom(byte[])
        val byteStringClass = cl.loadClass("com.google.protobuf.ByteString")
        val byteStringCopyFrom = byteStringClass.getMethod("copyFrom", ByteArray::class.java)

        // ─── prepare() overloads → return true ────────────────────────

        hookPrepare(epmClass, channelIdClass)
        hookPrepareWithTimeout(epmClass, channelIdClass)

        // ─── encrypt() overloads → plaintext passthrough ──────────────

        hookEncrypt(
            epmClass, channelIdClass, generatedMessageLiteClass,
            edNewBuilder, eiNewBuilder, byteStringCopyFrom, byteStringClass, encryptionInfoClass,
        )
        hookEncryptWithTimeout(
            epmClass, channelIdClass, generatedMessageLiteClass,
            edNewBuilder, eiNewBuilder, byteStringCopyFrom, byteStringClass, encryptionInfoClass,
        )

        // ─── decrypt() → plaintext passthrough ────────────────────────

        hookDecrypt(epmClass, channelIdClass, encryptedDataClass, cl)

        Log.i(TAG, "  Ephemeral protection bypass installed on $epmClassName")
    }

    // ─── prepare() hooks ──────────────────────────────────────────────

    private fun hookPrepare(epmClass: Class<*>, channelIdClass: Class<*>) {
        try {
            val method = epmClass.getDeclaredMethod("prepare", channelIdClass)
            method.isAccessible = true
            XposedBridge.hookMethod(method, object : XC_MethodHook() {
                override fun beforeHookedMethod(param: MethodHookParam) {
                    param.result = true
                }
            })
            Log.i(TAG, "  Hooked EphemeralProtectionManager.prepare(EphemeralChannelId)")
        } catch (t: Throwable) {
            Log.e(TAG, "  Failed to hook prepare(EphemeralChannelId): ${t.message}")
        }
    }

    private fun hookPrepareWithTimeout(epmClass: Class<*>, channelIdClass: Class<*>) {
        try {
            val method = epmClass.getDeclaredMethod(
                "prepare", channelIdClass, java.time.Duration::class.java,
            )
            method.isAccessible = true
            XposedBridge.hookMethod(method, object : XC_MethodHook() {
                override fun beforeHookedMethod(param: MethodHookParam) {
                    param.result = true
                }
            })
            Log.i(TAG, "  Hooked EphemeralProtectionManager.prepare(EphemeralChannelId, Duration)")
        } catch (t: Throwable) {
            Log.e(TAG, "  Failed to hook prepare(EphemeralChannelId, Duration): ${t.message}")
        }
    }

    // ─── encrypt() hooks ──────────────────────────────────────────────

    /**
     * Build a plaintext EncryptedData envelope via reflection:
     *   EncryptedData {
     *     data: raw proto bytes (as ByteString),
     *     encryptionInformation: EncryptionInformation { kid: java class name }
     *   }
     */
    private fun buildPlaintextEnvelope(
        proto: Any,
        edNewBuilder: Method,
        eiNewBuilder: Method,
        byteStringCopyFrom: Method,
        byteStringClass: Class<*>,
        encryptionInfoClass: Class<*>,
    ): Any {
        // Serialize the proto: proto.toByteArray() -> byte[]
        val toByteArray = proto.javaClass.getMethod("toByteArray")
        val rawBytes = toByteArray.invoke(proto) as ByteArray
        val className = proto.javaClass.name

        // Build EncryptionInformation { kid: className }
        val eiBuilder = eiNewBuilder.invoke(null)
        val setKid = eiBuilder.javaClass.getMethod("setKid", String::class.java)
        setKid.invoke(eiBuilder, className)
        val eiBuild = eiBuilder.javaClass.getMethod("build")
        val encInfo = eiBuild.invoke(eiBuilder)

        // Build EncryptedData { data: ByteString(rawBytes), encryptionInformation: encInfo }
        val edBuilder = edNewBuilder.invoke(null)
        val byteString = byteStringCopyFrom.invoke(null, rawBytes)
        // Use the pre-resolved ByteString class for method lookup — the runtime
        // concrete class (e.g. ByteString$LiteralByteString) won't match the
        // declared parameter type of setData(ByteString).
        val setData = edBuilder.javaClass.getMethod("setData", byteStringClass)
        setData.invoke(edBuilder, byteString)
        val setEncInfo = edBuilder.javaClass.getMethod("setEncryptionInformation", encryptionInfoClass)
        setEncInfo.invoke(edBuilder, encInfo)

        val edBuild = edBuilder.javaClass.getMethod("build")
        return edBuild.invoke(edBuilder)
    }

    private fun hookEncrypt(
        epmClass: Class<*>,
        channelIdClass: Class<*>,
        generatedMessageLiteClass: Class<*>,
        edNewBuilder: Method,
        eiNewBuilder: Method,
        byteStringCopyFrom: Method,
        byteStringClass: Class<*>,
        encryptionInfoClass: Class<*>,
    ) {
        try {
            val method = epmClass.getDeclaredMethod(
                "encrypt", channelIdClass, generatedMessageLiteClass,
            )
            method.isAccessible = true
            XposedBridge.hookMethod(method, object : XC_MethodHook() {
                override fun beforeHookedMethod(param: MethodHookParam) {
                    val proto = param.args[1]
                    try {
                        param.result = buildPlaintextEnvelope(
                            proto, edNewBuilder, eiNewBuilder, byteStringCopyFrom, byteStringClass, encryptionInfoClass,
                        )
                    } catch (t: Throwable) {
                        Log.e(TAG, "  encrypt() bypass failed: ${t.message}", t)
                        // Let original method run (will likely fail too)
                        return
                    }
                }
            })
            Log.i(TAG, "  Hooked EphemeralProtectionManager.encrypt(EphemeralChannelId, GeneratedMessageLite)")
        } catch (t: Throwable) {
            Log.e(TAG, "  Failed to hook encrypt(2-arg): ${t.message}")
        }
    }

    private fun hookEncryptWithTimeout(
        epmClass: Class<*>,
        channelIdClass: Class<*>,
        generatedMessageLiteClass: Class<*>,
        edNewBuilder: Method,
        eiNewBuilder: Method,
        byteStringCopyFrom: Method,
        byteStringClass: Class<*>,
        encryptionInfoClass: Class<*>,
    ) {
        try {
            val method = epmClass.getDeclaredMethod(
                "encrypt", channelIdClass, generatedMessageLiteClass,
                java.time.Duration::class.java,
            )
            method.isAccessible = true
            XposedBridge.hookMethod(method, object : XC_MethodHook() {
                override fun beforeHookedMethod(param: MethodHookParam) {
                    val proto = param.args[1]
                    try {
                        param.result = buildPlaintextEnvelope(
                            proto, edNewBuilder, eiNewBuilder, byteStringCopyFrom, byteStringClass, encryptionInfoClass,
                        )
                    } catch (t: Throwable) {
                        Log.e(TAG, "  encrypt() bypass failed: ${t.message}", t)
                        return
                    }
                }
            })
            Log.i(TAG, "  Hooked EphemeralProtectionManager.encrypt(EphemeralChannelId, GeneratedMessageLite, Duration)")
        } catch (t: Throwable) {
            Log.e(TAG, "  Failed to hook encrypt(3-arg): ${t.message}")
        }
    }

    // ─── decrypt() hook ───────────────────────────────────────────────

    private fun hookDecrypt(
        epmClass: Class<*>,
        channelIdClass: Class<*>,
        encryptedDataClass: Class<*>,
        cl: ClassLoader,
    ) {
        try {
            val method = epmClass.getDeclaredMethod(
                "decrypt", channelIdClass, encryptedDataClass,
            )
            method.isAccessible = true
            XposedBridge.hookMethod(method, object : XC_MethodHook() {
                override fun beforeHookedMethod(param: MethodHookParam) {
                    val encryptedData = param.args[1]
                    try {
                        // Extract EncryptionInformation.kid (the class name)
                        val getEncInfo = encryptedData.javaClass.getMethod("getEncryptionInformation")
                        val encInfo = getEncInfo.invoke(encryptedData)
                        val getKid = encInfo.javaClass.getMethod("getKid")
                        val className = getKid.invoke(encInfo) as String

                        if (className.isEmpty()) {
                            Log.w(TAG, "  decrypt() bypass: empty kid, falling through to original")
                            return
                        }

                        // Extract data bytes: EncryptedData.getData() -> ByteString
                        val getData = encryptedData.javaClass.getMethod("getData")
                        val byteString = getData.invoke(encryptedData)
                        val toByteArray = byteString.javaClass.getMethod("toByteArray")
                        val rawBytes = toByteArray.invoke(byteString) as ByteArray

                        // Reconstruct the proto: Class.forName(className).parseFrom(byte[])
                        val protoClass = cl.loadClass(className)
                        val parseFrom = protoClass.getMethod("parseFrom", ByteArray::class.java)
                        val result = parseFrom.invoke(null, rawBytes)

                        param.result = result
                    } catch (t: Throwable) {
                        Log.e(TAG, "  decrypt() bypass failed: ${t.message}", t)
                        // Let original method run
                        return
                    }
                }
            })
            Log.i(TAG, "  Hooked EphemeralProtectionManager.decrypt(EphemeralChannelId, EncryptedData)")
        } catch (t: Throwable) {
            Log.e(TAG, "  Failed to hook decrypt: ${t.message}")
        }
    }
}

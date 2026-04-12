package com.penumbraos.hook

import android.util.Log

/**
 * Bypass Krypton data protection so all captured data arrives as plaintext.
 *
 * DataProtectorWrapper is the single chokepoint through which every piece of
 * user data (thumbnails, locations, notes, file uploads) gets encrypted before
 * being sent to the server. By hooking its three "protect" methods we ensure
 * EncryptedData.data always contains the raw bytes and HTTP PUT bodies are
 * never encrypted.
 *
 */
object DataProtectorBypass {

    private const val TAG = "PenumbraHook"

    fun install(cl: ClassLoader) {
        val className = "dependency.implementations.DataProtectorWrapper"
        val clazz = try {
            cl.loadClass(className)
        } catch (e: ClassNotFoundException) {
            Log.w(TAG, "  $className not found, skipping encryption bypass hooks")
            return
        }

        // Pre-load reflection targets used by multiple hooks
        val simpleKrKeyIdClass = try {
            cl.loadClass("hu.ma.ne.krypton.key.SimpleKrKeyId")
        } catch (e: ClassNotFoundException) {
            Log.e(TAG, "  SimpleKrKeyId not found — cannot install encryption bypass")
            return
        }
        val protectedDataClass = try {
            cl.loadClass("hu.ma.ne.dataprotection.data.ProtectedData")
        } catch (e: ClassNotFoundException) {
            Log.e(TAG, "  ProtectedData not found — cannot install encryption bypass")
            return
        }
        val krKeyIdClass = try {
            cl.loadClass("hu.ma.ne.krypton.key.KrKeyId")
        } catch (e: ClassNotFoundException) {
            Log.e(TAG, "  KrKeyId not found — cannot install encryption bypass")
            return
        }

        // Constructor: SimpleKrKeyId(String)
        val simpleKrKeyIdCtor = simpleKrKeyIdClass.getConstructor(String::class.java)

        // Constructor: ProtectedData(KrKeyId, int, int, ByteBuffer)
        val protectedDataCtor = protectedDataClass.getConstructor(
            krKeyIdClass,
            Int::class.javaPrimitiveType,
            Int::class.javaPrimitiveType,
            java.nio.ByteBuffer::class.java,
        )

        // Create a reusable fake key ID
        val fakeKeyId = simpleKrKeyIdCtor.newInstance("plaintext")

        // ─── Hook 1: createKey(int[]) -> KrKeyId ───────────────────────
        HookUtils.hookMethodBefore(
            clazz,
            "createKey",
            arrayOf(IntArray::class.java),
        ) { param ->
            param.result = fakeKeyId
            Log.i(TAG, "  DataProtector.createKey() → plaintext key (bypassed)")
        }

        // ─── Hook 2: protectDataWithExistingKey(ByteBuffer, KrKeyId, int, PersonalFlatMetadata) -> ProtectedData ───
        val personalFlatMetaClass = try {
            cl.loadClass("humane.personaldata.PersonalFlatMetadata")
        } catch (e: ClassNotFoundException) {
            Log.w(TAG, "  PersonalFlatMetadata not found, skipping protectDataWithExistingKey hook")
            null
        }

        if (personalFlatMetaClass != null) {
            HookUtils.hookMethodBefore(
                clazz,
                "protectDataWithExistingKey",
                arrayOf<Class<*>>(
                    java.nio.ByteBuffer::class.java,
                    krKeyIdClass,
                    Int::class.javaPrimitiveType!!,
                    personalFlatMetaClass,
                ),
            ) { param ->
                val inputBuffer = param.args[0] as java.nio.ByteBuffer
                val keyId = param.args[1] // KrKeyId
                val objectId = param.args[2] as Int

                // Duplicate the buffer so the caller's position isn't affected
                val dupBuffer = inputBuffer.duplicate()

                param.result = protectedDataCtor.newInstance(keyId, 0, objectId, dupBuffer)

                val size = dupBuffer.remaining()
                Log.i(TAG, "  DataProtector.protectDataWithExistingKey() → plaintext passthrough ($size bytes)")
            }
        }

        // ─── Hook 3: protectProtoWithNewKey(GeneratedMessageLite, PersonalProtoMetadata) -> ProtectedData ───
        val personalProtoMetaClass = try {
            cl.loadClass("humane.personaldata.PersonalProtoMetadata")
        } catch (e: ClassNotFoundException) {
            Log.w(TAG, "  PersonalProtoMetadata not found, skipping protectProtoWithNewKey hook")
            null
        }
        val generatedMessageLiteClass = try {
            cl.loadClass("com.google.protobuf.GeneratedMessageLite")
        } catch (e: ClassNotFoundException) {
            Log.w(TAG, "  GeneratedMessageLite not found, skipping protectProtoWithNewKey hook")
            null
        }

        if (personalProtoMetaClass != null && generatedMessageLiteClass != null) {
            HookUtils.hookMethodBefore(
                clazz,
                "protectProtoWithNewKey",
                arrayOf(generatedMessageLiteClass, personalProtoMetaClass),
            ) { param ->
                val proto = param.args[0]

                // Call proto.toByteArray() via reflection (GeneratedMessageLite method)
                val toByteArrayMethod = proto.javaClass.getMethod("toByteArray")
                val rawBytes = toByteArrayMethod.invoke(proto) as ByteArray

                val buffer = java.nio.ByteBuffer.wrap(rawBytes)
                param.result = protectedDataCtor.newInstance(fakeKeyId, 0, 0, buffer)

                Log.i(TAG, "  DataProtector.protectProtoWithNewKey() → plaintext serialized proto (${rawBytes.size} bytes)")
            }
        }

        Log.i(TAG, "  Encryption bypass hooks installed on $className")
    }
}

package com.penumbraos.hook

import android.os.SystemClock
import android.util.Log
import org.json.JSONObject
import de.robv.android.xposed.XC_MethodHook
import de.robv.android.xposed.XposedBridge
import java.util.concurrent.atomic.AtomicLong

/**
 * Streaming replacement for the embedded Microsoft TTS engine
 */
object HumaneTtsHooks {
    private const val TAG = "PenumbraTTS"
    private const val SAMPLE_RATE_HZ = 24000
    private const val AUDIO_FORMAT_PCM_16BIT = 2
    private const val CHANNEL_COUNT_MONO = 1
    private const val TARGET_READ_BYTES = 2400 // 50ms at 24kHz 16-bit mono PCM
    private const val MAX_READ_BYTES = 16000

    // The Penumbra server, on the device. It speaks through a cloud voice; the
    // embedded Microsoft engine below stays as the fallback, so an unreachable
    // server or a failed request costs quality, never speech.
    private const val SERVER_HOST = "127.0.0.1"
    private const val SERVER_PORT = 8080
    private const val SERVER_CONNECT_TIMEOUT_MS = 1_500
    private const val SERVER_READ_TIMEOUT_MS = 15_000

    private val nextRequestId = AtomicLong(1)
    private val requestIdByThread = ThreadLocal<Long?>()
    private val onSynthesizeStartByThread = ThreadLocal<Long?>()
    private val initializeStartByThread = ThreadLocal<Long?>()
    private val loadLanguageStartByThread = ThreadLocal<Long?>()

    fun install(cl: ClassLoader) {
        Log.w(TAG, "Installing Humane TTS hooks...")
        // The server voice lives on 127.0.0.1, which this process blocks by default.
        CleartextLoopbackBypass.install()
        hookHumaneTtsService(cl)
        Log.w(TAG, "Humane TTS hooks installed")
    }

    private fun hookHumaneTtsService(cl: ClassLoader) {
        val serviceClass = loadClassOrNull(cl, "humane.voice.tts.HumaneTTSService") ?: return
        val synthesisRequestClass = loadClassOrNull(cl, "android.speech.tts.SynthesisRequest") ?: return
        val synthesisCallbackClass = loadClassOrNull(cl, "android.speech.tts.SynthesisCallback") ?: return

        hookMethod(serviceClass, "onSynthesizeText", synthesisRequestClass, synthesisCallbackClass,
            before = { param ->
                val requestId = nextRequestId.getAndIncrement()
                val startMs = nowMs()
                requestIdByThread.set(requestId)
                onSynthesizeStartByThread.set(startMs)

                val request = param.args.getOrNull(0)
                val callback = param.args.getOrNull(1)
                if (request != null && callback != null) {
                    val handled = tryServerSynthesis(request, callback, requestId) ||
                        tryAudioDataStreamSynthesis(param.thisObject, request, callback, requestId)
                    if (handled) {
                        param.result = null
                    }
                }
            },
            after = { param ->
                val requestId = requestIdByThread.get()
                val throwable = param.throwable
                if (throwable != null) {
                    val startMs = onSynthesizeStartByThread.get() ?: 0L
                    Log.w(
                        TAG,
                        "id=$requestId tts hookThrowable totalMs=${elapsedSince(startMs)} " +
                            "throwable=${throwable.javaClass.name}:${throwable.message}"
                    )
                }
                requestIdByThread.remove()
                onSynthesizeStartByThread.remove()
            }
        )

        hookMethod(serviceClass, "initializeSynthesizer",
            before = {
                initializeStartByThread.set(nowMs())
            },
            after = { param ->
                val startMs = initializeStartByThread.get() ?: 0L
                val synthesizer = getDeclaredField(param.thisObject, "mSynthesizer")
                Log.w(
                    TAG,
                    "id=${requestId()} initializeSynthesizer end durationMs=${elapsedSince(startMs)} " +
                        "throwable=${param.throwable?.javaClass?.name ?: "none"} synthesizer=${synthesizer?.javaClass?.name}"
                )
                initializeStartByThread.remove()
            }
        )

        hookMethod(serviceClass, "onLoadLanguage", String::class.java, String::class.java, String::class.java,
            before = {
                loadLanguageStartByThread.set(nowMs())
            },
            after = { param ->
                val result = param.result as? Int
                val throwable = param.throwable
                if (throwable != null || result == -2 || result == -1) {
                    val startMs = loadLanguageStartByThread.get() ?: 0L
                    Log.w(
                        TAG,
                        "id=${requestId()} tts languageLoadFailed durationMs=${elapsedSince(startMs)} " +
                            "result=$result throwable=${throwable?.javaClass?.name ?: "none"}"
                    )
                }
                loadLanguageStartByThread.remove()
            }
        )
    }

    /**
     * Speak via the Penumbra server's cloud voice.
     *
     * Deliberately a raw socket rather than HttpURLConnection: this process
     * ships with cleartext HTTP disabled, and the platform rejects
     * `http://127.0.0.1:8080` before the request is ever sent
     * ("Cleartext HTTP traffic to 127.0.0.1 not permitted"). That policy is
     * enforced by the URL stack, not by sockets, so speaking HTTP/1.1 directly
     * sidesteps it without relaxing the policy for anything else. The server is
     * on-device and the bytes never leave the handset.
     *
     * Returns false for anything unexpected so the caller falls through to the
     * embedded Microsoft engine.
     */
    private fun tryServerSynthesis(request: Any, synthesisCallback: Any, requestId: Long): Boolean {
        val startMs = nowMs()
        var callbackStarted = false
        var socket: java.net.Socket? = null

        return try {
            val rawText = callNoArg(request, "getCharSequenceText")?.toString() ?: return false
            val text = stripSsml(rawText).trim()
            if (text.isEmpty()) return false

            val payload = JSONObject().put("text", text).toString().toByteArray(Charsets.UTF_8)

            socket = java.net.Socket()
            socket.connect(java.net.InetSocketAddress(SERVER_HOST, SERVER_PORT), SERVER_CONNECT_TIMEOUT_MS)
            socket.soTimeout = SERVER_READ_TIMEOUT_MS

            val out = socket.getOutputStream()
            // `Connection: close` lets an identity-encoded body end at EOF.
            val head = buildString {
                append("POST /api/tts HTTP/1.1\r\n")
                append("Host: ").append(SERVER_HOST).append(":").append(SERVER_PORT).append("\r\n")
                append("Content-Type: application/json\r\n")
                append("Content-Length: ").append(payload.size).append("\r\n")
                append("Connection: close\r\n\r\n")
            }
            out.write(head.toByteArray(Charsets.US_ASCII))
            out.write(payload)
            out.flush()

            val input = java.io.BufferedInputStream(socket.getInputStream())

            val statusLine = readLine(input) ?: return false
            val status = statusLine.split(' ').getOrNull(1)?.toIntOrNull() ?: return false

            var chunked = false
            while (true) {
                val line = readLine(input) ?: return false
                if (line.isEmpty()) break
                val lower = line.lowercase()
                if (lower.startsWith("transfer-encoding:") && lower.contains("chunked")) {
                    chunked = true
                }
            }

            if (status != 200) {
                // 503 just means no provider is configured — expected, and quiet.
                Log.w(TAG, "id=$requestId server tts unavailable status=$status; using device voice")
                return false
            }

            val androidMaxBufferSize = callNoArg(synthesisCallback, "getMaxBufferSize") as? Int ?: MAX_READ_BYTES
            val readBufferSize = minOf(TARGET_READ_BYTES, androidMaxBufferSize).coerceAtLeast(1024)
            val buffer = ByteArray(readBufferSize)
            var totalBytes = 0L
            var firstAudioMs = -1L
            // Bytes left in the current chunk; identity bodies run to EOF.
            var remaining = if (chunked) 0L else Long.MAX_VALUE

            while (true) {
                if (chunked && remaining == 0L) {
                    val sizeLine = readLine(input) ?: break
                    if (sizeLine.isEmpty()) continue
                    val size = sizeLine.substringBefore(';').trim().toLongOrNull(16) ?: break
                    if (size == 0L) break
                    remaining = size
                }

                val want = minOf(buffer.size.toLong(), remaining).toInt()
                val read = input.read(buffer, 0, want)
                if (read <= 0) break
                if (chunked) {
                    remaining -= read
                    // Consume the CRLF that terminates a completed chunk.
                    if (remaining == 0L) readLine(input)
                }

                if (firstAudioMs < 0L) firstAudioMs = elapsedSince(startMs)

                if (!callbackStarted) {
                    val startResult = synthesisCallback.javaClass.getMethod(
                        "start",
                        Int::class.javaPrimitiveType,
                        Int::class.javaPrimitiveType,
                        Int::class.javaPrimitiveType,
                    ).invoke(synthesisCallback, SAMPLE_RATE_HZ, AUDIO_FORMAT_PCM_16BIT, CHANNEL_COUNT_MONO) as? Int
                    if (startResult != 0) {
                        throw IllegalStateException("synthesisCallback.start returned $startResult")
                    }
                    callbackStarted = true
                    Log.w(TAG, "id=$requestId server tts playbackStart firstAudioMs=$firstAudioMs len=${text.length}")
                }

                val audioResult = synthesisCallback.javaClass.getMethod(
                    "audioAvailable",
                    ByteArray::class.java,
                    Int::class.javaPrimitiveType,
                    Int::class.javaPrimitiveType,
                ).invoke(synthesisCallback, buffer, 0, read) as? Int
                if (audioResult != 0) {
                    Log.w(TAG, "id=$requestId server tts interrupted bytes=$totalBytes result=$audioResult")
                    break
                }
                totalBytes += read
            }

            // Nothing played means nothing was committed, so the embedded engine
            // can still take the utterance.
            if (!callbackStarted) return false

            callNoArg(synthesisCallback, "done")
            Log.w(
                TAG,
                "id=$requestId server tts done totalMs=${elapsedSince(startMs)} " +
                    "firstAudioMs=$firstAudioMs bytes=$totalBytes backend=server"
            )
            true
        } catch (t: Throwable) {
            Log.w(TAG, "id=$requestId server tts failed (${t.javaClass.simpleName}: ${t.message}); using device voice")
            // Once audio has started the callback is ours, so finish it rather
            // than handing a half-spoken utterance to the fallback.
            if (callbackStarted) {
                runCatching { callNoArg(synthesisCallback, "done") }
                true
            } else {
                false
            }
        } finally {
            runCatching { socket?.close() }
        }
    }

    /** Read one CRLF-terminated header/chunk line, or null at EOF. */
    private fun readLine(input: java.io.InputStream): String? {
        val out = java.io.ByteArrayOutputStream(64)
        while (true) {
            val b = input.read()
            if (b < 0) return if (out.size() == 0) null else out.toString("US-ASCII")
            if (b == '\n'.code) return out.toString("US-ASCII").trimEnd('\r')
            out.write(b)
        }
    }

    /** Humane may hand us SSML; the cloud voice wants the words only. */
    private fun stripSsml(text: String): String {
        if (!text.trimStart().startsWith("<")) return text
        return text.replace(Regex("<[^>]*>"), " ").replace(Regex("\\s+"), " ")
    }

    private fun tryAudioDataStreamSynthesis(service: Any, request: Any, synthesisCallback: Any, requestId: Long): Boolean {
        val startMs = nowMs()
        var callbackStarted = false
        var result: Any? = null
        var audioStream: Any? = null

        return try {
            val text = callNoArg(request, "getCharSequenceText")?.toString() ?: return false

            val language = callNoArg(request, "getLanguage") as? String ?: "eng"
            val country = callNoArg(request, "getCountry") as? String ?: "USA"
            val variant = callNoArg(request, "getVariant") as? String ?: ""
            val loadResult = callDeclared(service, "onLoadLanguage", arrayOf(String::class.java, String::class.java, String::class.java), language, country, variant) as? Int
            if (loadResult == -2 || loadResult == -1) {
                Log.w(TAG, "id=$requestId stream fallback/error: onLoadLanguage result=$loadResult")
                return false
            }

            val synthesizer = getDeclaredField(service, "mSynthesizer") ?: return false
            val ssml = buildSsml(service, text)

            result = synthesizer.javaClass.getMethod("StartSpeakingSsml", String::class.java).invoke(synthesizer, ssml)
            val startSpeakingMs = elapsedSince(startMs)

            val cl = service.javaClass.classLoader ?: return false
            val audioDataStreamClass = cl.loadClass("com.microsoft.cognitiveservices.speech.AudioDataStream")
            val synthesisResultClass = cl.loadClass("com.microsoft.cognitiveservices.speech.SpeechSynthesisResult")
            audioStream = audioDataStreamClass.getMethod("fromResult", synthesisResultClass).invoke(null, result)
            val readDataMethod = audioDataStreamClass.getMethod("readData", ByteArray::class.java)

            val androidMaxBufferSize = callNoArg(synthesisCallback, "getMaxBufferSize") as? Int ?: MAX_READ_BYTES
            val readBufferSize = minOf(TARGET_READ_BYTES, androidMaxBufferSize).coerceAtLeast(1024)
            val buffer = ByteArray(readBufferSize)
            var totalBytes = 0L
            var readCount = 0
            var firstAudioMs = -1L

            while (true) {
                val read = (readDataMethod.invoke(audioStream, buffer as Any) as Number).toLong()
                if (read <= 0L) {
                    break
                }
                if (firstAudioMs < 0L) {
                    firstAudioMs = elapsedSince(startMs)
                }
                if (!callbackStarted) {
                    val startResult = synthesisCallback.javaClass.getMethod(
                        "start",
                        Int::class.javaPrimitiveType,
                        Int::class.javaPrimitiveType,
                        Int::class.javaPrimitiveType,
                    ).invoke(synthesisCallback, SAMPLE_RATE_HZ, AUDIO_FORMAT_PCM_16BIT, CHANNEL_COUNT_MONO) as? Int
                    if (startResult != 0) {
                        throw IllegalStateException("synthesisCallback.start returned $startResult")
                    }
                    callbackStarted = true
                    Log.w(TAG, "id=$requestId tts playbackStart firstAudioMs=$firstAudioMs startSpeakingMs=$startSpeakingMs len=${text.length}")
                }

                val audioResult = synthesisCallback.javaClass.getMethod(
                    "audioAvailable",
                    ByteArray::class.java,
                    Int::class.javaPrimitiveType,
                    Int::class.javaPrimitiveType,
                ).invoke(synthesisCallback, buffer, 0, read.toInt()) as? Int
                if (audioResult != 0) {
                    Log.w(
                        TAG,
                        "id=$requestId tts interrupted totalMs=${elapsedSince(startMs)} firstAudioMs=$firstAudioMs " +
                            "startSpeakingMs=$startSpeakingMs bytes=$totalBytes reads=$readCount callbackResult=$audioResult"
                    )
                    safeClose(audioStream)
                    safeClose(result)
                    return true
                }

                totalBytes += read
                readCount++
            }

            val reason = callNoArg(result, "getReason")?.toString()
            if (totalBytes <= 0L) {
                Log.w(TAG, "id=$requestId stream produced no audio reason=$reason")
                callNoArg(synthesisCallback, "error")
            } else {
                val doneResult = callNoArg(synthesisCallback, "done")
                Log.w(
                    TAG,
                    "id=$requestId tts done totalMs=${elapsedSince(startMs)} firstAudioMs=$firstAudioMs " +
                        "startSpeakingMs=$startSpeakingMs bytes=$totalBytes reads=$readCount reason=$reason " +
                        "callbackResult=$doneResult ${describeSynthesisResult(result)}"
                )
            }
            safeClose(audioStream)
            safeClose(result)
            true
        } catch (t: Throwable) {
            Log.e(TAG, "id=$requestId stream synthesis failed started=$callbackStarted", t)
            safeClose(audioStream)
            safeClose(result)
            if (callbackStarted) {
                callNoArg(synthesisCallback, "error")
                true
            } else {
                false
            }
        }
    }

    private fun buildSsml(service: Any, text: String): String {
        val isValid = callDeclared(service, "isValidSSML", arrayOf(String::class.java), text) as? Boolean ?: false
        return if (isValid) {
            text
        } else {
            "<speak version=\"1.0\" xmlns=\"http://www.w3.org/2001/10/synthesis\" xml:lang=\"en-US\">$text</speak>"
        }
    }

    private fun callDeclared(target: Any?, name: String, paramTypes: Array<Class<*>>, vararg args: Any?): Any? {
        if (target == null) return null
        return try {
            val method = target.javaClass.getDeclaredMethod(name, *paramTypes)
            method.isAccessible = true
            method.invoke(target, *args)
        } catch (_: Throwable) {
            null
        }
    }

    private fun getDeclaredField(target: Any?, name: String): Any? {
        if (target == null) return null
        return try {
            val field = target.javaClass.getDeclaredField(name)
            field.isAccessible = true
            field.get(target)
        } catch (_: Throwable) {
            null
        }
    }

    private fun safeClose(target: Any?) {
        try {
            if (target is AutoCloseable) {
                target.close()
            } else {
                callNoArg(target, "close")
            }
        } catch (_: Throwable) {
        }
    }

    private fun hookMethod(
        clazz: Class<*>,
        name: String,
        vararg paramTypes: Class<*>,
        before: ((XC_MethodHook.MethodHookParam) -> Unit)? = null,
        after: ((XC_MethodHook.MethodHookParam) -> Unit)? = null,
    ) {
        try {
            val method = clazz.getDeclaredMethod(name, *paramTypes)
            method.isAccessible = true
            XposedBridge.hookMethod(method, object : XC_MethodHook() {
                override fun beforeHookedMethod(param: MethodHookParam) {
                    before?.invoke(param)
                }

                override fun afterHookedMethod(param: MethodHookParam) {
                    after?.invoke(param)
                }
            })
            Log.w(TAG, "  Hooked ${clazz.name}.$name(${paramTypes.joinToString { it.simpleName }})")
        } catch (t: Throwable) {
            Log.w(TAG, "  Failed to hook ${clazz.name}.$name: ${t.message}")
        }
    }


    private fun loadClassOrNull(cl: ClassLoader, className: String): Class<*>? {
        return try {
            cl.loadClass(className)
        } catch (t: Throwable) {
            Log.w(TAG, "  $className not found, skipping")
            null
        }
    }

    private fun describeSynthesisResult(result: Any?): String {
        if (result == null) return "result=null"
        return try {
            val reason = callNoArg(result, "getReason")
            val resultId = callNoArg(result, "getResultId")
            val audioLength = callNoArg(result, "getAudioLength")
            val properties = callNoArg(result, "getProperties")
            val firstByteMs = getPropertyByEnumName(properties, "SpeechServiceResponse_SynthesisFirstByteLatencyMs")
            val finishMs = getPropertyByEnumName(properties, "SpeechServiceResponse_SynthesisFinishLatencyMs")
            val underrunMs = getPropertyByEnumName(properties, "SpeechServiceResponse_SynthesisUnderrunTimeMs")
            val connectionMs = getPropertyByEnumName(properties, "SpeechServiceResponse_SynthesisConnectionLatencyMs")
            val networkMs = getPropertyByEnumName(properties, "SpeechServiceResponse_SynthesisNetworkLatencyMs")
            val serviceMs = getPropertyByEnumName(properties, "SpeechServiceResponse_SynthesisServiceLatencyMs")
            val backend = getPropertyByEnumName(properties, "SpeechServiceResponse_SynthesisBackend")
            "reason=$reason resultId=$resultId audioLength=$audioLength firstByteMs=$firstByteMs " +
                "finishMs=$finishMs underrunMs=$underrunMs connectionMs=$connectionMs " +
                "networkMs=$networkMs serviceMs=$serviceMs backend=$backend"
        } catch (t: Throwable) {
            "result=${result.javaClass.name} describeError=${t.javaClass.simpleName}:${t.message}"
        }
    }

    private fun getPropertyByEnumName(properties: Any?, enumName: String): String {
        if (properties == null) return "null"
        return try {
            val classLoader = properties.javaClass.classLoader ?: return "noClassLoader"
            val propertyIdClass = classLoader.loadClass("com.microsoft.cognitiveservices.speech.PropertyId")
            @Suppress("UNCHECKED_CAST")
            val enumClass = propertyIdClass as Class<out Enum<*>>
            val propertyId = enumClass.enumConstants?.firstOrNull { it.name == enumName } ?: return "missingEnum"
            properties.javaClass.getMethod("getProperty", propertyIdClass).invoke(properties, propertyId)?.toString() ?: "null"
        } catch (t: Throwable) {
            "error:${t.javaClass.simpleName}"
        }
    }

    private fun callNoArg(target: Any?, name: String): Any? {
        if (target == null) return null
        return try {
            target.javaClass.getMethod(name).invoke(target)
        } catch (_: Throwable) {
            try {
                val method = target.javaClass.getDeclaredMethod(name)
                method.isAccessible = true
                method.invoke(target)
            } catch (_: Throwable) {
                null
            }
        }
    }

    private fun requestId(): Long? = requestIdByThread.get()
    private fun nowMs(): Long = SystemClock.elapsedRealtime()
    private fun elapsedSince(startMs: Long): Long = if (startMs > 0L) nowMs() - startMs else -1L
}

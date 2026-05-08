package com.penumbraos.server

import android.util.Log
import org.json.JSONArray
import org.json.JSONObject
import java.util.ArrayDeque
import java.util.concurrent.CopyOnWriteArraySet

object EsimEventStore {

    private const val TAG = "PenumbraEsimEvents"
    private const val MAX_RECENT_EVENTS = 128
    private const val PROTECTED_T_MOBILE_NAME = "t-mobile"
    private const val PROTECTED_GSMA_TEST_PREFIX = "gsma test profile"

    private val recentEvents = ArrayDeque<JSONObject>()
    private val pendingProfiles = linkedMapOf<Int, String>()
    private val typedEventListeners = CopyOnWriteArraySet<(JSONObject) -> Unit>()

    @Volatile
    private var currentAction: String? = null

    @Volatile
    private var currentRequestId: String? = null

    @Volatile
    private var lastIntentResult: String? = null

    @Volatile
    private var pendingProfileCount: Int? = null

    @Volatile
    private var currentImei: String? = null

    @Volatile
    private var activeDownloadRequestId: String? = null

    @Volatile
    private var activeDownloadTerminalResult: JSONObject? = null

    @Synchronized
    fun onEvent(event: JSONObject) {
        appendRecent(event)
        Log.w(TAG, "Received event type=${event.optString("type")} action=${event.optString("action")}")

        when (event.optString("type")) {
            "esim.action_started" -> handleActionStarted(event)
            "esim.sysprop_update" -> handleSyspropUpdate(event)
            "esim.profile_mutation_result" -> handleProfileMutationResult(event)
            "esim.download_progress" -> handleDownloadProgress(event)
            "esim.download_result" -> handleDownloadResult(event)
        }
    }

    fun addTypedEventListener(listener: (JSONObject) -> Unit) {
        typedEventListeners.add(listener)
    }

    fun removeTypedEventListener(listener: (JSONObject) -> Unit) {
        typedEventListeners.remove(listener)
    }

    private fun handleActionStarted(event: JSONObject) {
        val action = event.optString("action").takeIf { it.isNotEmpty() }
        currentAction = action
        currentRequestId = event.optString("request_id").takeIf { it.isNotEmpty() }
        notifyTypedEvent(event)
        when (action) {
            "humane.connectivity.esimlpa.getProfiles" -> {
                pendingProfiles.clear()
                pendingProfileCount = null
                lastIntentResult = null
            }
            "humane.connectivity.esimlpa.getActiveProfile",
            "humane.connectivity.esimlpa.getActiveprofileICCID",
            "humane.connectivity.esimlpa.getEID" -> {
                lastIntentResult = null
                currentImei = null
            }
            "humane.connectivity.esimlpa.downloadVerifyAndEnableProfile",
            "humane.connectivity.esimlpa.downloadAndEnableProfile" -> {
                activeDownloadRequestId = currentRequestId
                activeDownloadTerminalResult = null
            }
        }
    }

    private fun handleSyspropUpdate(event: JSONObject) {
        val payload = event.optJSONObject("payload") ?: return
        val key = payload.optString("key")
        val value = payload.optString("value")

        when {
            key.startsWith("humane.esim.Profile") -> {
                val index = key.removePrefix("humane.esim.Profile").toIntOrNull() ?: return
                pendingProfiles[index] = value
                maybeSynthesizeProfilesResult()
            }
            key == "humane.esim.NmbrOfProfiles" -> {
                pendingProfileCount = value.toIntOrNull()
                maybeSynthesizeProfilesResult()
            }
            key == "humane.esim.lastintent.result" -> {
                lastIntentResult = value
                maybeSynthesizeProfilesResult()
                maybeSynthesizeActiveProfileResult(payload)
                maybeSynthesizeActiveIccidResult(payload)
                maybeSynthesizeDeviceIdentifiersResult(payload)
            }
            key == "humane.esim.ActiveProfile" -> maybeSynthesizeActiveProfileResult(payload)
            key == "humane.esim.ICCID" -> maybeSynthesizeActiveIccidResult(payload)
            key == "humane.esim.EID" || key == "humane.esim.IMEI" -> maybeSynthesizeDeviceIdentifiersResult(payload)
        }
    }

    private fun handleProfileMutationResult(event: JSONObject) {
        Log.w(TAG, "Received profile_mutation_result payload=${event.optJSONObject("payload")}")
        notifyTypedEvent(event)
    }

    private fun handleDownloadProgress(event: JSONObject) {
        if (isDownloadTerminal(event)) {
            Log.w(TAG, "Ignoring download_progress after terminal result payload=${event.optJSONObject("payload")}")
            return
        }
        Log.w(TAG, "Received download_progress payload=${event.optJSONObject("payload")}")
        notifyTypedEvent(event)
    }

    private fun handleDownloadResult(event: JSONObject) {
        if (isDownloadTerminal(event)) {
            Log.w(TAG, "Ignoring duplicate download_result payload=${event.optJSONObject("payload")}")
            return
        }
        activeDownloadTerminalResult = JSONObject(event.toString())
        Log.w(TAG, "Received download_result payload=${event.optJSONObject("payload")}")
        notifyTypedEvent(event)
    }

    private fun isDownloadTerminal(event: JSONObject): Boolean {
        val action = event.optString("action").takeIf { it.isNotEmpty() } ?: currentAction
        if (action != "humane.connectivity.esimlpa.downloadVerifyAndEnableProfile" &&
            action != "humane.connectivity.esimlpa.downloadAndEnableProfile"
        ) {
            return false
        }

        if (activeDownloadTerminalResult == null) {
            return false
        }

        val eventRequestId = event.optString("request_id").takeIf { it.isNotEmpty() }
        return eventRequestId == null || eventRequestId == activeDownloadRequestId
    }

    private fun maybeSynthesizeProfilesResult() {
        if (currentAction != "humane.connectivity.esimlpa.getProfiles") return
        if (lastIntentResult != "getProfile success") return

        val expectedCount = pendingProfileCount ?: return
        if (pendingProfiles.size < expectedCount) return

        val profiles = JSONArray()
        for (index in 0 until expectedCount) {
            val raw = pendingProfiles[index] ?: return
            val parts = raw.split(",", limit = 5)
            if (parts.size < 5) return

            profiles.put(
                JSONObject()
                    .put("name", parts[0])
                    .put("state", parts[1])
                    .put("iccid", parts[2])
                    .put("service_provider", parts[3])
                    .put("nickname", parts[4])
                    .put("protected", isDeletionProtected(parts[0]))
            )
        }

        val event = baseEvent("esim.profiles_result")
            .put(
                "payload",
                JSONObject()
                    .put("result", "success")
                    .put("count", expectedCount)
                    .put("profiles", profiles)
                    .put("raw_lastintent_result", lastIntentResult)
            )

        appendRecent(event)
        Log.w(TAG, "Synthesized profiles_result count=$expectedCount payload=${event.optJSONObject("payload")}")
        notifyTypedEvent(event)
    }

    private fun maybeSynthesizeActiveProfileResult(payload: JSONObject) {
        if (currentAction != "humane.connectivity.esimlpa.getActiveProfile") return

        val activeProfile = payload.takeIf {
            it.optString("key") == "humane.esim.ActiveProfile"
        }?.optString("value")

        if (activeProfile != null && lastIntentResult == "Get Ative profile success") {
            val parts = activeProfile.split(",", limit = 5)
            if (parts.size < 5) return

            val event = baseEvent("esim.active_profile_result")
                .put(
                    "payload",
                    JSONObject()
                        .put("result", "success")
                        .put(
                            "profile",
                            JSONObject()
                                .put("name", parts[0])
                                .put("state", parts[1])
                                .put("iccid", parts[2])
                                .put("service_provider", parts[3])
                                .put("nickname", parts[4])
                                .put("protected", isDeletionProtected(parts[0]))
                        )
                        .put("raw_lastintent_result", lastIntentResult)
                )

            appendRecent(event)
            Log.w(TAG, "Synthesized active_profile_result payload=${event.optJSONObject("payload")}")
            notifyTypedEvent(event)
            return
        }

        if (lastIntentResult == "No Active profile") {
            val event = baseEvent("esim.active_profile_result")
                .put("payload", JSONObject().put("result", "no_active_profile"))
            appendRecent(event)
            Log.w(TAG, "Synthesized active_profile_result no_active_profile payload=${event.optJSONObject("payload")}")
            notifyTypedEvent(event)
        }
    }

    private fun maybeSynthesizeActiveIccidResult(payload: JSONObject) {
        if (currentAction != "humane.connectivity.esimlpa.getActiveprofileICCID") return

        val iccid = payload.takeIf {
            it.optString("key") == "humane.esim.ICCID"
        }?.optString("value")

        if (!iccid.isNullOrEmpty() && lastIntentResult == "Get Ative profile ICCID success") {
            val event = baseEvent("esim.active_iccid_result")
                .put(
                    "payload",
                    JSONObject()
                        .put("result", "success")
                        .put("iccid", iccid)
                        .put("raw_lastintent_result", lastIntentResult)
                )
            appendRecent(event)
            Log.w(TAG, "Synthesized active_iccid_result payload=${event.optJSONObject("payload")}")
            notifyTypedEvent(event)
            return
        }

        if (lastIntentResult == "No Active profile") {
            val event = baseEvent("esim.active_iccid_result")
                .put("payload", JSONObject().put("result", "no_active_profile"))
            appendRecent(event)
            Log.w(TAG, "Synthesized active_iccid_result no_active_profile payload=${event.optJSONObject("payload")}")
            notifyTypedEvent(event)
        }
    }

    private fun maybeSynthesizeDeviceIdentifiersResult(payload: JSONObject) {
        if (currentAction != "humane.connectivity.esimlpa.getEID") return

        when (payload.optString("key")) {
            "humane.esim.IMEI" -> currentImei = payload.optString("value").takeIf { it.isNotEmpty() }
        }

        val eid = payload.takeIf {
            it.optString("key") == "humane.esim.EID"
        }?.optString("value")

        if (!eid.isNullOrEmpty() && lastIntentResult == "Get EID success") {
            val event = baseEvent("esim.device_identifiers_result")
                .put(
                    "payload",
                    JSONObject()
                        .put("result", "success")
                        .put("eid", eid)
                        .put("imei", currentImei?.let { it } ?: JSONObject.NULL)
                        .put("raw_lastintent_result", lastIntentResult)
                )
            appendRecent(event)
            Log.w(TAG, "Synthesized device_identifiers_result payload=${event.optJSONObject("payload")}")
            notifyTypedEvent(event)
        }
    }

    private fun notifyTypedEvent(event: JSONObject) {
        val snapshot = JSONObject(event.toString())
        for (listener in typedEventListeners) {
            try {
                listener(snapshot)
            } catch (t: Throwable) {
                Log.w(TAG, "Typed event listener failed", t)
            }
        }
    }

    private fun isDeletionProtected(profileName: String?): Boolean {
        val normalized = profileName?.trim()?.lowercase().orEmpty()
        if (normalized.isEmpty()) return false
        return normalized.startsWith(PROTECTED_T_MOBILE_NAME) || normalized.startsWith(PROTECTED_GSMA_TEST_PREFIX)
    }

    private fun baseEvent(type: String): JSONObject {
        return JSONObject()
            .put("version", 1)
            .put("type", type)
            .put("ts_ms", System.currentTimeMillis())
            .put("source_process", "com.penumbraos.server")
            .put("source_pid", android.os.Process.myPid())
            .put("request_id", currentRequestId?.let { it } ?: JSONObject.NULL)
            .put("action", currentAction)
    }

    private fun appendRecent(event: JSONObject) {
        recentEvents.addLast(JSONObject(event.toString()))
        while (recentEvents.size > MAX_RECENT_EVENTS) {
            recentEvents.removeFirst()
        }
    }
}

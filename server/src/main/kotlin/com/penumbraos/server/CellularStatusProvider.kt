package com.penumbraos.server

import android.content.Context
import android.content.pm.PackageManager
import android.net.ConnectivityManager
import android.net.NetworkCapabilities
import android.telephony.ServiceState
import android.telephony.SignalStrength
import android.telephony.TelephonyManager
import org.json.JSONObject

object CellularStatusProvider {

    private const val REGISTRATION_STATE_NOT_REGISTERED_OR_SEARCHING = 0
    private const val REGISTRATION_STATE_HOME = 1
    private const val REGISTRATION_STATE_NOT_REGISTERED_SEARCHING = 2
    private const val REGISTRATION_STATE_DENIED = 3
    private const val REGISTRATION_STATE_UNKNOWN = 4
    private const val REGISTRATION_STATE_ROAMING = 5

    private const val DATA_DISCONNECTED = 0
    private const val DATA_CONNECTING = 1
    private const val DATA_CONNECTED = 2
    private const val DATA_SUSPENDED = 3

    fun snapshot(context: Context): JSONObject {
        val telephonyManager = context.getSystemService(Context.TELEPHONY_SERVICE) as? TelephonyManager
            ?: return statusPayload(
                status = "error",
                reason = "telephony_unavailable",
                message = "Telephony service unavailable",
                cellularUsable = false,
                details = JSONObject(),
            )

        if (context.checkSelfPermission("android.permission.READ_PHONE_STATE") != PackageManager.PERMISSION_GRANTED) {
            return statusPayload(
                status = "error",
                reason = "permission_missing",
                message = "Phone state permission unavailable",
                cellularUsable = false,
                details = JSONObject(),
            )
        }

        val serviceState = runCatching { telephonyManager.serviceState }.getOrNull()
        val signalStrength = runCatching { telephonyManager.signalStrength }.getOrNull()
        val connectivity = getConnectivitySnapshot(context)
        val rejectCause = firstRejectCause(serviceState)
        val telephonyRegistered = isTelephonyRegistered(serviceState)
        val mobileDataEnabled = getMobileDataEnabled(telephonyManager)
        val dataState = getDataState(telephonyManager)
        val dataConnected = dataState == DATA_CONNECTED || connectivity.hasCellularNetwork
        val signalLevel = runCatching { signalStrength?.level }.getOrNull()
        val operatorName = runCatching { telephonyManager.networkOperatorName }.getOrNull()
        val networkType = runCatching { telephonyManager.dataNetworkType }.getOrNull()

        val summary = deriveSummary(
            serviceState = serviceState,
            mobileDataEnabled = mobileDataEnabled,
            dataState = dataState,
            dataConnected = dataConnected,
            telephonyRegistered = telephonyRegistered,
            connectivity = connectivity,
            rejectCause = rejectCause,
        )

        val details = JSONObject()
            .put("operator_name", safeValue(operatorName))
            .put("network_type", networkTypeLabel(networkType))
            .put("service_state", serviceStateStateLabel(serviceState))
            .put("signal_level", signalLevel ?: JSONObject.NULL)
            .put("signal_dbm", signalStrengthDbm(signalStrength) ?: JSONObject.NULL)
            .put("mobile_data_enabled", mobileDataEnabled)
            .put("data_connected", dataConnected)
            .put("data_connection_state", dataStateLabel(dataState))
            .put("internet_validated", connectivity.hasValidatedCellularNetwork)

        if (rejectCause != null && rejectCause > 0) {
            details.put("reject_cause", rejectCause)
        }

        return statusPayload(
            status = summary.status,
            reason = summary.reason,
            message = summary.message,
            cellularUsable = summary.cellularUsable,
            details = details,
        )
    }

    private fun deriveSummary(
        serviceState: ServiceState?,
        mobileDataEnabled: Boolean,
        dataState: Int?,
        dataConnected: Boolean,
        telephonyRegistered: Boolean,
        connectivity: ConnectivitySnapshot,
        rejectCause: Int?,
    ): StatusSummary {
        val state = serviceState?.state
        val emergencyOnly = isEmergencyOnly(serviceState)

        return when {
            connectivity.hasValidatedCellularNetwork -> {
                StatusSummary(
                    status = "working",
                    reason = "validated",
                    message = "Cellular internet is working",
                    cellularUsable = true,
                )
            }
            !mobileDataEnabled -> {
                StatusSummary(
                    status = "off",
                    reason = "mobile_data_disabled",
                    message = "Mobile data is turned off",
                    cellularUsable = false,
                )
            }
            state == ServiceState.STATE_POWER_OFF -> {
                StatusSummary(
                    status = "off",
                    reason = "radio_off",
                    message = "Cellular radio is turned off",
                    cellularUsable = false,
                )
            }
            rejectCause != null && rejectCause > 0 -> {
                StatusSummary(
                    status = "error",
                    reason = "network_denied",
                    message = "Carrier denied network access",
                    cellularUsable = false,
                )
            }
            emergencyOnly || state == ServiceState.STATE_EMERGENCY_ONLY -> {
                StatusSummary(
                    status = "no_service",
                    reason = "emergency_only",
                    message = "Only emergency service is available",
                    cellularUsable = false,
                )
            }
            state == ServiceState.STATE_OUT_OF_SERVICE -> {
                StatusSummary(
                    status = "no_service",
                    reason = "out_of_service",
                    message = "No cellular service is available",
                    cellularUsable = false,
                )
            }
            dataConnected || dataState == DATA_CONNECTED || dataState == DATA_SUSPENDED || connectivity.hasCellularNetwork -> {
                StatusSummary(
                    status = "limited",
                    reason = "connected_no_internet",
                    message = "Cellular connected, but internet access is not working",
                    cellularUsable = false,
                )
            }
            telephonyRegistered -> {
                StatusSummary(
                    status = "error",
                    reason = "no_data_connection",
                    message = "Cellular service is registered, but no data connection was established",
                    cellularUsable = false,
                )
            }
            else -> {
                StatusSummary(
                    status = "no_service",
                    reason = "searching",
                    message = "Searching for cellular service",
                    cellularUsable = false,
                )
            }
        }
    }

    private fun statusPayload(
        status: String,
        reason: String,
        message: String,
        cellularUsable: Boolean,
        details: JSONObject,
    ): JSONObject {
        return JSONObject()
            .put("status", status)
            .put("reason", reason)
            .put("message", message)
            .put("cellular_usable", cellularUsable)
            .put("details", details)
    }

    private fun firstRejectCause(serviceState: ServiceState?): Int? {
        val infos = serviceState?.let { getCompatList(it, "getNetworkRegistrationInfoList") } ?: return null
        for (info in infos) {
            if (info == null) continue
            val rejectCause = getCompatInt(info, "getRejectCause")
            if (rejectCause != null && rejectCause != 0) {
                return rejectCause
            }
        }
        return null
    }

    private fun isTelephonyRegistered(serviceState: ServiceState?): Boolean {
        if (serviceState == null) return false
        if (serviceState.state == ServiceState.STATE_IN_SERVICE) return true
        val dataRegState = getCompatInt(serviceState, "getDataRegState")
        val voiceRegState = getCompatInt(serviceState, "getVoiceRegState")
        return dataRegState == ServiceState.STATE_IN_SERVICE || voiceRegState == ServiceState.STATE_IN_SERVICE
    }

    private fun getMobileDataEnabled(telephonyManager: TelephonyManager): Boolean {
        return getCompatBoolean(telephonyManager, "isDataEnabled") ?: false
    }

    private fun getDataState(telephonyManager: TelephonyManager): Int? {
        return getCompatInt(telephonyManager, "getDataState")
    }

    private fun getConnectivitySnapshot(context: Context): ConnectivitySnapshot {
        val connectivityManager = context.getSystemService(Context.CONNECTIVITY_SERVICE) as? ConnectivityManager
            ?: return ConnectivitySnapshot()

        var hasCellularNetwork = false
        var hasValidatedCellularNetwork = false
        val allNetworks = runCatching { connectivityManager.allNetworks }.getOrNull().orEmpty()
        for (network in allNetworks) {
            val capabilities = runCatching { connectivityManager.getNetworkCapabilities(network) }.getOrNull() ?: continue
            if (!capabilities.hasTransport(NetworkCapabilities.TRANSPORT_CELLULAR)) continue
            hasCellularNetwork = true
            if (capabilities.hasCapability(NetworkCapabilities.NET_CAPABILITY_VALIDATED)) {
                hasValidatedCellularNetwork = true
            }
        }

        return ConnectivitySnapshot(
            hasCellularNetwork = hasCellularNetwork,
            hasValidatedCellularNetwork = hasValidatedCellularNetwork,
        )
    }

    private fun serviceStateStateLabel(serviceState: ServiceState?): String {
        if (serviceState == null) return "unknown"
        return when (serviceState.state) {
            ServiceState.STATE_IN_SERVICE -> "in_service"
            ServiceState.STATE_OUT_OF_SERVICE -> "out_of_service"
            ServiceState.STATE_EMERGENCY_ONLY -> "emergency_only"
            ServiceState.STATE_POWER_OFF -> "power_off"
            else -> "unknown"
        }
    }

    private fun dataStateLabel(state: Int?): String {
        return when (state) {
            null -> "unknown"
            DATA_DISCONNECTED -> "disconnected"
            DATA_CONNECTING -> "connecting"
            DATA_CONNECTED -> "connected"
            DATA_SUSPENDED -> "suspended"
            else -> state.toString()
        }
    }

    private fun networkTypeLabel(type: Int?): String {
        return when (type) {
            null -> "unknown"
            TelephonyManager.NETWORK_TYPE_UNKNOWN -> "unknown"
            TelephonyManager.NETWORK_TYPE_GPRS -> "GPRS"
            TelephonyManager.NETWORK_TYPE_EDGE -> "EDGE"
            TelephonyManager.NETWORK_TYPE_UMTS -> "UMTS"
            TelephonyManager.NETWORK_TYPE_HSDPA -> "HSDPA"
            TelephonyManager.NETWORK_TYPE_HSUPA -> "HSUPA"
            TelephonyManager.NETWORK_TYPE_HSPA -> "HSPA"
            TelephonyManager.NETWORK_TYPE_LTE -> "LTE"
            TelephonyManager.NETWORK_TYPE_NR -> "NR"
            TelephonyManager.NETWORK_TYPE_IWLAN -> "IWLAN"
            else -> type.toString()
        }
    }

    private fun safeValue(value: String?): Any {
        return value?.takeIf { it.isNotEmpty() } ?: JSONObject.NULL
    }

    private fun isEmergencyOnly(serviceState: ServiceState?): Boolean {
        return serviceState?.let { getCompatBoolean(it, "isEmergencyOnly") } ?: false
    }

    private fun signalStrengthDbm(signalStrength: SignalStrength?): Int? {
        return runCatching { signalStrength?.cellSignalStrengths?.firstOrNull()?.dbm }.getOrNull()
    }

    private fun getCompatInt(target: Any, methodName: String): Int? {
        val value = getCompatValue(target, methodName) ?: return null
        return when (value) {
            is Int -> value
            is Number -> value.toInt()
            else -> null
        }
    }

    private fun getCompatBoolean(target: Any, methodName: String): Boolean? {
        val value = getCompatValue(target, methodName) ?: return null
        return value as? Boolean
    }

    @Suppress("UNCHECKED_CAST")
    private fun getCompatList(target: Any, methodName: String): List<Any?> {
        val value = getCompatValue(target, methodName) ?: return emptyList()
        return when (value) {
            is List<*> -> value as List<Any?>
            is Array<*> -> value.toList() as List<Any?>
            else -> emptyList()
        }
    }

    private fun getCompatValue(target: Any, methodName: String): Any? {
        return runCatching {
            val method = target.javaClass.methods.firstOrNull {
                it.name == methodName && it.parameterTypes.isEmpty()
            } ?: return null
            method.isAccessible = true
            method.invoke(target)
        }.getOrNull()
    }

    private data class ConnectivitySnapshot(
        val hasCellularNetwork: Boolean = false,
        val hasValidatedCellularNetwork: Boolean = false,
    )

    private data class StatusSummary(
        val status: String,
        val reason: String,
        val message: String,
        val cellularUsable: Boolean,
    )
}

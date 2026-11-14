package com.pixelcallai.streamer

import android.Manifest
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Build
import android.os.Bundle
import android.telecom.PhoneAccountHandle
import android.telecom.TelecomManager
import android.telephony.TelephonyManager
import android.util.Log
import androidx.core.content.ContextCompat

/**
 * Best-effort outbound dial for agent-initiated callbacks.
 * Prefers [TelecomManager.placeCall] when we hold ROLE_DIALER; falls back to
 * [Intent.ACTION_CALL] when [Manifest.permission.CALL_PHONE] is granted.
 * A `sub_id` picks the SIM ([TelecomManager.EXTRA_PHONE_ACCOUNT_HANDLE]); without one,
 * or when no call-capable account has that subscription, Android's default is used.
 */
object OutboundCallHelper {
    private const val TAG = "OutboundCall"

    fun placeCall(context: Context, number: String, callId: String, subId: Int? = null): Boolean {
        val cleaned = number.trim()
        if (cleaned.isEmpty()) {
            Log.w(TAG, "placeCall: empty number call_id=$callId")
            return false
        }
        val uri = Uri.fromParts("tel", cleaned, null)
        val defaultDialer = DialerRoleHelper.isDefaultDialer(context)
        val account = subId?.let { phoneAccountFor(context, it) }
        if (subId != null) {
            Log.i(
                TAG,
                "sub_id=$subId account=${if (account != null) "found" else "not found, default SIM"} call_id=$callId",
            )
        }

        if (defaultDialer) {
            try {
                val telecom = context.getSystemService(TelecomManager::class.java)
                val extras = Bundle()
                if (account != null) extras.putParcelable(TelecomManager.EXTRA_PHONE_ACCOUNT_HANDLE, account)
                telecom?.placeCall(uri, extras)
                Log.i(TAG, "TelecomManager.placeCall ok call_id=$callId len=${cleaned.length}")
                return true
            } catch (e: SecurityException) {
                Log.w(TAG, "TelecomManager.placeCall denied call_id=$callId: ${e.message}")
            } catch (e: Exception) {
                Log.w(TAG, "TelecomManager.placeCall failed call_id=$callId: ${e.message}")
            }
        } else {
            Log.w(
                TAG,
                "not default dialer — TelecomManager.placeCall blocked; trying ACTION_CALL call_id=$callId",
            )
        }

        if (ContextCompat.checkSelfPermission(context, Manifest.permission.CALL_PHONE)
            != PackageManager.PERMISSION_GRANTED
        ) {
            Log.w(TAG, "CALL_PHONE not granted call_id=$callId")
            return false
        }
        return try {
            val intent = Intent(Intent.ACTION_CALL, uri)
                .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
            if (account != null) intent.putExtra(TelecomManager.EXTRA_PHONE_ACCOUNT_HANDLE, account)
            context.startActivity(intent)
            Log.i(TAG, "ACTION_CALL issued call_id=$callId len=${cleaned.length}")
            true
        } catch (e: Exception) {
            Log.w(TAG, "ACTION_CALL failed call_id=$callId: ${e.message}")
            false
        }
    }

    /** The call-capable phone account of SIM [subId]; null when none (or before Android 11). */
    fun phoneAccountFor(context: Context, subId: Int): PhoneAccountHandle? {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.R) return null
        return try {
            val telecom = context.getSystemService(TelecomManager::class.java) ?: return null
            val tm = context.getSystemService(TelephonyManager::class.java) ?: return null
            val accounts = telecom.callCapablePhoneAccounts.orEmpty().map { it to tm.getSubscriptionId(it) }
            SimSubscriptions.pick(accounts, subId)
        } catch (e: SecurityException) {
            Log.w(TAG, "phone accounts not readable: ${e.message}")
            null
        } catch (e: Exception) {
            Log.w(TAG, "phone account lookup failed: ${e.message}")
            null
        }
    }
}

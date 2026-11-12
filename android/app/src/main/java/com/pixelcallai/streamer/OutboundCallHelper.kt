package com.pixelcallai.streamer

import android.Manifest
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Bundle
import android.telecom.TelecomManager
import android.util.Log
import androidx.core.content.ContextCompat

/**
 * Best-effort outbound dial for agent-initiated callbacks.
 * Prefers [TelecomManager.placeCall] when we hold ROLE_DIALER; falls back to
 * [Intent.ACTION_CALL] when [Manifest.permission.CALL_PHONE] is granted.
 */
object OutboundCallHelper {
    private const val TAG = "OutboundCall"

    fun placeCall(context: Context, number: String, callId: String): Boolean {
        val cleaned = number.trim()
        if (cleaned.isEmpty()) {
            Log.w(TAG, "placeCall: empty number call_id=$callId")
            return false
        }
        val uri = Uri.fromParts("tel", cleaned, null)
        val defaultDialer = DialerRoleHelper.isDefaultDialer(context)

        if (defaultDialer) {
            try {
                val telecom = context.getSystemService(TelecomManager::class.java)
                telecom?.placeCall(uri, Bundle())
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
            context.startActivity(intent)
            Log.i(TAG, "ACTION_CALL issued call_id=$callId len=${cleaned.length}")
            true
        } catch (e: Exception) {
            Log.w(TAG, "ACTION_CALL failed call_id=$callId: ${e.message}")
            false
        }
    }
}

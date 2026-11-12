package com.pixelcallai.streamer

import android.content.Intent
import android.os.Build
import android.telecom.Call
import android.telecom.CallScreeningService
import android.util.Log

/**
 * Call Screening role: silence ringtone when possible.
 * Blocklist **disabled** — never reject/disallow; every caller reaches the AI.
 *
 * **AOSP contract (must not throw):** for an *allowed* call
 * (`setDisallowCall(false)`), `setRejectCall` / `setSkipCallLog` /
 * `setSkipNotification` must all be false. Only `setSilenceCall(true)` is
 * valid on allow. Building an illegal [CallResponse] throws
 * `IllegalStateException: Invalid response state for allowed call` and, if
 * uncaught, kills the whole process — including [CallAudioService] when they
 * share a process → no TYPE_TELEPHONY inject after answer.
 *
 * This service runs in process `:screening` (manifest) so a residual crash
 * cannot tear down the streamer; [onScreenCall] is also fail-open try/catch.
 */
class SilentCallScreeningService : CallScreeningService() {
    override fun onScreenCall(callDetails: Call.Details) {
        // Outer try/catch is mandatory: never let screening kill the media path.
        try {
            screenIncoming(callDetails)
        } catch (t: Throwable) {
            Log.e(TAG, "onScreenCall uncaught — fail-open allow", t)
            respondAllowSilent(callDetails, forceDefaults = true)
        }
    }

    private fun screenIncoming(callDetails: Call.Details) {
        val direction = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
            callDetails.callDirection
        } else {
            Call.Details.DIRECTION_INCOMING
        }
        Log.i(TAG, "onScreenCall direction=$direction")
        var caller = ""
        try {
            val handle = callDetails.handle?.schemeSpecificPart
            if (!handle.isNullOrBlank()) {
                caller = handle
                Prefs.setLastCallerId(this, handle)
                Log.i(TAG, "stashed caller_id from screening len=${handle.length}")
            }
        } catch (_: Exception) {
        }

        if (direction != Call.Details.DIRECTION_INCOMING) {
            respondAllowSilent(callDetails, silence = false)
            return
        }

        // Allow + silence ring. Never skipCallLog / skipNotification on allow
        // (AOSP throws IllegalStateException → process death).
        respondAllowSilent(callDetails, silence = true)
        Log.i(TAG, "screened: silence=true reject=false skipNotification=false (allow-legal)")

        try {
            LocalAudioGuard(this).applyHardMute()
        } catch (e: Exception) {
            Log.w(TAG, "applyHardMute: ${e.message}")
        }

        // Arm streamer early with EXTRA so the agent's hello sees the caller before OFFHOOK.
        // Intent targets default process CallAudioService (screening is :screening).
        if (Prefs.isAiCallEnabled(this)) {
            try {
                val i = Intent(this, CallAudioService::class.java)
                    .setAction(CallAudioService.ACTION_START)
                if (caller.isNotBlank()) {
                    i.putExtra(CallAudioService.EXTRA_CALLER_ID, caller)
                }
                if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
                    startForegroundService(i)
                } else {
                    startService(i)
                }
            } catch (e: Exception) {
                Log.w(TAG, "arm CallAudioService failed: ${e.message}")
            }
        }
    }

    /**
     * Single respond path. Allowed-call legal matrix only.
     * @param forceDefaults if true, empty builder (all false) after prior failure
     * @param silence silence ringtone; only used when !forceDefaults
     */
    private fun respondAllowSilent(
        callDetails: Call.Details,
        silence: Boolean = true,
        forceDefaults: Boolean = false,
    ) {
        try {
            val response = if (forceDefaults) {
                CallResponse.Builder().build()
            } else {
                CallResponse.Builder()
                    .setDisallowCall(false)
                    .setRejectCall(false)
                    .setSilenceCall(silence)
                    .setSkipCallLog(false)
                    .setSkipNotification(false)
                    .build()
            }
            respondToCall(callDetails, response)
        } catch (t: Throwable) {
            Log.e(TAG, "respondToCall failed — last-chance empty allow", t)
            try {
                respondToCall(callDetails, CallResponse.Builder().build())
            } catch (t2: Throwable) {
                Log.e(TAG, "last-chance respondToCall failed", t2)
            }
        }
    }

    companion object {
        private const val TAG = "SilentCallScreen"
    }
}

package com.pixelcallai.streamer

import android.content.Intent
import android.os.Build
import android.telecom.Call
import android.telecom.InCallService
import android.telecom.VideoProfile
import android.util.Log
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.atomic.AtomicLong

/**
 * UI InCallService — silence + arm pipeline, **do not answer** until
 * [CallAudioService] confirms the agent connection is ready (connect first,
 * then answer). Only the primary in-call UI while holding ROLE_DIALER.
 */
class SilentInCallService : InCallService() {
    override fun onCreate() {
        super.onCreate()
        activeService = this
    }

    override fun onDestroy() {
        if (activeService === this) {
            activeService = null
            liveCalls.clear()
            callInstances.clear()
            activeCall = null
            activeCallInstance.set(NO_CALL_INSTANCE)
        }
        super.onDestroy()
    }

    override fun onCallAdded(call: Call) {
        super.onCallAdded(call)
        liveCalls.add(call)
        val instanceId = callInstances.computeIfAbsent(call) { nextCallInstance.incrementAndGet() }
        val current = activeCall
        if (current == null || current !in liveCalls || !isEstablishedCall(current)) {
            selectActiveCall(call, instanceId)
        }
        val state = call.state
        val caller = stashCallerId(call)
        Log.i(TAG, "onCallAdded state=$state caller_len=${caller.length}")
        ensureArmed(caller)
        LocalAudioGuard(this).applyHardMute()
        forceMuteCall(call)
        when (state) {
            Call.STATE_RINGING, Call.STATE_NEW -> {
                // Do NOT answer here — CallAudioService waits for the agent first.
                Log.i(TAG, "RINGING — defer answer to CallAudioService (agent gate)")
                forceMuteCall(call)
                LocalAudioGuard(this).applyHardMute()
            }
            Call.STATE_ACTIVE -> {
                forceMuteCall(call)
                selectActiveCall(call, instanceId)
                signalCallActive(caller, instanceId)
            }
            Call.STATE_CONNECTING, Call.STATE_DIALING -> {
                forceMuteCall(call)
            }
        }
        call.registerCallback(object : Call.Callback() {
            override fun onStateChanged(call: Call, state: Int) {
                Log.i(TAG, "call state -> $state")
                forceMuteCall(call)
                if (state == Call.STATE_ACTIVE) {
                    if (call !in liveCalls) return
                    selectActiveCall(call, instanceId)
                }
                if (state == Call.STATE_ACTIVE || state == Call.STATE_RINGING) {
                    LocalAudioGuard(this@SilentInCallService).applyHardMute()
                    ensureArmed(stashCallerId(call))
                }
                if (state == Call.STATE_ACTIVE) {
                    signalCallActive(stashCallerId(call), instanceId)
                }
            }
        })
    }

    override fun onCallRemoved(call: Call) {
        super.onCallRemoved(call)
        liveCalls.remove(call)
        val instanceId = callInstances.remove(call) ?: NO_CALL_INSTANCE
        val removedActiveCall = activeCallInstance.compareAndSet(instanceId, NO_CALL_INSTANCE)
        if (removedActiveCall) {
            preferredLiveCall()?.let { replacement ->
                selectActiveCall(replacement, callInstances[replacement] ?: NO_CALL_INSTANCE)
            } ?: run {
                activeCall = null
            }
        }
        Log.i(TAG, "onCallRemoved")
        if (removedActiveCall) {
            signalCallEnded(instanceId)
        }
    }

    override fun onCanAddCallChanged(canAddCall: Boolean) {
        // no UI
    }

    private fun forceMuteCall(call: Call) {
        // Normal phone mode: the owner talks on this call; never mute it.
        if (!Prefs.isAiCallEnabled(this)) return
        try {
            setMuted(true)
            Log.i(TAG, "InCallService.setMuted(true) state=${call.state}")
        } catch (e: Exception) {
            Log.w(TAG, "setMuted failed: ${e.message}")
        }
    }

    private fun ensureArmed(callerId: String? = null) {
        if (!Prefs.isAiCallEnabled(this)) return
        try {
            val i = Intent(this, CallAudioService::class.java)
                .setAction(CallAudioService.ACTION_START)
            val id = callerId?.trim().orEmpty().ifBlank { Prefs.lastCallerId(this) }
            if (id.isNotBlank()) {
                i.putExtra(CallAudioService.EXTRA_CALLER_ID, id)
            }
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
                startForegroundService(i)
            } else {
                startService(i)
            }
        } catch (e: Exception) {
            Log.w(TAG, "ensureArmed failed: ${e.message}")
        }
    }

    private fun signalCallActive(callerId: String?, instanceId: Long) {
        startCallAudioService(CallAudioService.ACTION_CALL_ACTIVE, callerId, instanceId)
    }

    private fun signalCallEnded(instanceId: Long) {
        startCallAudioService(CallAudioService.ACTION_CALL_ENDED, null, instanceId)
    }

    private fun startCallAudioService(action: String, callerId: String?, instanceId: Long) {
        if (!Prefs.isAiCallEnabled(this)) return
        try {
            val i = Intent(this, CallAudioService::class.java).setAction(action)
                .putExtra(CallAudioService.EXTRA_CALL_INSTANCE_ID, instanceId)
            val id = callerId?.trim().orEmpty().ifBlank { Prefs.lastCallerId(this) }
            if (id.isNotBlank()) i.putExtra(CallAudioService.EXTRA_CALLER_ID, id)
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
                startForegroundService(i)
            } else {
                startService(i)
            }
        } catch (e: Exception) {
            Log.w(TAG, "signal $action failed: ${e.message}")
        }
    }

    private fun stashCallerId(call: Call): String {
        return try {
            val handle = call.details?.handle?.schemeSpecificPart
            if (!handle.isNullOrBlank()) {
                Prefs.setLastCallerId(this, handle)
                Log.i(TAG, "stashed caller_id len=${handle.length}")
                handle
            } else {
                ""
            }
        } catch (e: Exception) {
            Log.w(TAG, "stashCallerId failed: ${e.message}")
            ""
        }
    }

    companion object {
        private const val TAG = "SilentInCall"

        @Volatile
        private var activeCall: Call? = null

        private const val NO_CALL_INSTANCE = 0L
        private val nextCallInstance = AtomicLong(NO_CALL_INSTANCE)
        private val activeCallInstance = AtomicLong(NO_CALL_INSTANCE)

        @Volatile
        private var activeService: SilentInCallService? = null

        private val liveCalls = ConcurrentHashMap.newKeySet<Call>()
        private val callInstances = ConcurrentHashMap<Call, Long>()

        /** Null means InCallService is unavailable, so callers must act conservatively. */
        fun hasEstablishedCall(): Boolean? {
            if (activeService == null) return null
            return liveCalls.any(::isEstablishedCall)
        }

        /** Null means the InCall service is unavailable; false means the ID is stale. */
        fun isLiveCallInstance(instanceId: Long): Boolean? {
            if (activeService == null) return null
            return instanceId != NO_CALL_INSTANCE && callInstances.containsValue(instanceId)
        }

        /**
         * True when the live call was placed from this phone (manual dial or callback).
         * Null when InCallService is not bound (caller must stay conservative).
         */
        fun isOutgoingCall(): Boolean? {
            if (activeService == null) return null
            val call = preferredLiveCall() ?: return null
            return try {
                call.details.callDirection == Call.Details.DIRECTION_OUTGOING
            } catch (_: Exception) {
                null
            }
        }

        fun currentActiveCallInstanceId(): Long =
            if (activeService == null) NO_CALL_INSTANCE else activeCallInstance.get()

        private fun selectActiveCall(call: Call, instanceId: Long) {
            if (instanceId == NO_CALL_INSTANCE || call !in liveCalls) return
            activeCall = call
            while (true) {
                val current = activeCallInstance.get()
                if (current == instanceId || activeCallInstance.compareAndSet(current, instanceId)) return
            }
        }

        private fun preferredLiveCall(): Call? =
            liveCalls.firstOrNull(::isEstablishedCall)
                ?: liveCalls.firstOrNull { it.state == Call.STATE_RINGING || it.state == Call.STATE_NEW }
                ?: liveCalls.firstOrNull()

        private fun isEstablishedCall(call: Call): Boolean =
            when (call.state) {
                Call.STATE_ACTIVE,
                Call.STATE_HOLDING,
                Call.STATE_CONNECTING,
                Call.STATE_DIALING,
                -> true
                else -> false
            }

        /** Re-assert Telecom mic mute while we own the active call (hold loop). */
        fun forceMuteActive(): Boolean {
            val svc = activeService ?: return false
            return try {
                svc.setMuted(true)
                true
            } catch (e: Exception) {
                Log.w(TAG, "forceMuteActive failed: ${e.message}")
                false
            }
        }

        /** Defer-ring path: answer the active call once CallAudioService confirms the agent. */
        fun answerActiveIfHeld(): Boolean {
            val call = activeCall ?: return false
            return try {
                if (call.state == Call.STATE_RINGING || call.state == Call.STATE_NEW) {
                    call.answer(VideoProfile.STATE_AUDIO_ONLY)
                    Log.i(TAG, "answerActiveIfHeld issued")
                    true
                } else {
                    false
                }
            } catch (e: Exception) {
                Log.w(TAG, "answerActiveIfHeld failed: ${e.message}")
                false
            }
        }

        /** Agent hangup: disconnect the active Telecom call if we own InCallService. */
        fun disconnectActive(expectedInstanceId: Long = NO_CALL_INSTANCE): Boolean {
            if (expectedInstanceId != NO_CALL_INSTANCE && activeCallInstance.get() != expectedInstanceId) {
                return false
            }
            val call = activeCall ?: return false
            if (call !in liveCalls) return false
            return try {
                call.disconnect()
                Log.i(TAG, "disconnectActive issued")
                true
            } catch (e: Exception) {
                Log.w(TAG, "disconnectActive failed: ${e.message}")
                false
            }
        }
    }
}

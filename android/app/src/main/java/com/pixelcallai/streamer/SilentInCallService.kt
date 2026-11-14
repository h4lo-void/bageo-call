package com.pixelcallai.streamer

import android.content.Intent
import android.os.Build
import android.telecom.Call
import android.telecom.InCallService
import android.telecom.VideoProfile
import android.telephony.SubscriptionManager
import android.telephony.TelephonyManager
import android.util.Log
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.atomic.AtomicLong

/**
 * UI InCallService — silence + arm pipeline, **do not answer** until
 * [CallAudioService] confirms the agent connection is ready (connect first,
 * then answer). Only the primary in-call UI while holding ROLE_DIALER.
 *
 * In AI mode it also:
 * - keeps each call's number and SIM ([CallInfo]); a new call first drops the previous
 *   call's, so a hidden number never inherits the last caller's;
 * - rejects a second call that rings while another call is up (call waiting), tells
 *   [CallAudioService] (`call_waiting_rejected`) and leaves the first call untouched;
 * - leaves a call dialed by hand on the phone alone: not muted, no AI. Calls the agent
 *   asked for (`place_call`) are not "by hand" ([CallAudioService.isAgentCallbackPending]).
 */
class SilentInCallService : InCallService() {
    /** Number and SIM of one call as the agent gets them. [callerId] "" = hidden or unknown. */
    class CallInfo(
        val instanceId: Long,
        val callerId: String,
        val simCountry: String,
        val simSubId: Int?,
    )

    private class SimInfo(val country: String, val subId: Int?)

    override fun onCreate() {
        super.onCreate()
        activeService = this
    }

    override fun onDestroy() {
        if (activeService === this) {
            activeService = null
            liveCalls.clear()
            callInstances.clear()
            callInfos.clear()
            activeCall = null
            activeCallInstance.set(NO_CALL_INSTANCE)
        }
        super.onDestroy()
    }

    override fun onCallAdded(call: Call) {
        super.onCallAdded(call)
        liveCalls.add(call)
        val instanceId = callInstances.computeIfAbsent(call) { nextCallInstance.incrementAndGet() }
        if (Prefs.isAiCallEnabled(this) && isWaitingCall(call)) {
            rejectWaitingCall(call, instanceId)
            return
        }
        val current = activeCall
        if (current == null || current !in liveCalls || !isEstablishedCall(current)) {
            selectActiveCall(call, instanceId)
        }
        val state = call.state
        val info = stashCallInfo(call, instanceId)
        val ownCall = isOwnCall(call)
        Log.i(
            TAG,
            "onCallAdded state=$state instance=$instanceId caller_len=${info.callerId.length} " +
                "sim_country=${info.simCountry.ifEmpty { "?" }} sim_sub_id=${info.simSubId ?: "?"} own=$ownCall",
        )
        if (ownCall) {
            // Dialed by hand on this phone: the owner talks on it. No mute, no AI.
            if (Prefs.isAiCallEnabled(this)) Log.i(TAG, "own outgoing call — not muted, AI not attached")
        } else {
            ensureArmed(info.callerId, instanceId)
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
                    signalCallActive(info.callerId, instanceId)
                }
                Call.STATE_CONNECTING, Call.STATE_DIALING -> {
                    forceMuteCall(call)
                }
            }
        }
        call.registerCallback(object : Call.Callback() {
            override fun onStateChanged(call: Call, state: Int) {
                Log.i(TAG, "call state -> $state instance=$instanceId")
                // The owner's own call: never muted, never handed to the AI.
                if (isOwnCall(call)) return
                forceMuteCall(call)
                if (state == Call.STATE_ACTIVE) {
                    if (call !in liveCalls) return
                    selectActiveCall(call, instanceId)
                }
                if (state == Call.STATE_ACTIVE || state == Call.STATE_RINGING) {
                    LocalAudioGuard(this@SilentInCallService).applyHardMute()
                    ensureArmed(refreshCallerId(call, instanceId), instanceId)
                }
                if (state == Call.STATE_ACTIVE) {
                    signalCallActive(refreshCallerId(call, instanceId), instanceId)
                }
            }
        })
    }

    override fun onCallRemoved(call: Call) {
        super.onCallRemoved(call)
        liveCalls.remove(call)
        val instanceId = callInstances.remove(call) ?: NO_CALL_INSTANCE
        callInfos.remove(instanceId)
        // Every mode (normal phone mode never reaches CallAudioService's cleanup).
        if (liveCalls.isEmpty()) {
            Prefs.setLastSimCountry(this, "")
            Prefs.setLastSimSubId(this, null)
        }
        // A rejected waiting call was never selected: removing it changes nothing for
        // the call that stays up (no replacement, no CALL_ENDED).
        val removedActiveCall = activeCallInstance.compareAndSet(instanceId, NO_CALL_INSTANCE)
        if (removedActiveCall) {
            preferredLiveCall()?.let { replacement ->
                selectActiveCall(replacement, callInstances[replacement] ?: NO_CALL_INSTANCE)
            } ?: run {
                activeCall = null
            }
        }
        Log.i(TAG, "onCallRemoved instance=$instanceId selected=$removedActiveCall")
        if (removedActiveCall) {
            signalCallEnded(instanceId)
        }
    }

    override fun onCanAddCallChanged(canAddCall: Boolean) {
        // no UI
    }

    /** Incoming and ringing while another call is up (dialing, active or on hold). */
    private fun isWaitingCall(call: Call): Boolean {
        val incoming = try {
            call.details.callDirection == Call.Details.DIRECTION_INCOMING
        } catch (_: Exception) {
            false
        }
        val others = liveCalls.filter { it !== call }.map { it.state }
        return isCallWaiting(incoming, call.state, others)
    }

    /**
     * AI mode, call waiting: reject the second call at once. Its number and SIM are only
     * read (the first call's stored number and SIM stay), it is never selected, muted or
     * armed, so the first call's media session and mute state do not change.
     */
    private fun rejectWaitingCall(call: Call, instanceId: Long) {
        val caller = handleOf(call)
        val sim = simOf(call)
        Log.i(
            TAG,
            "call waiting: reject second call instance=$instanceId caller_len=${caller.length} " +
                "sim_country=${sim.country.ifEmpty { "?" }} sim_sub_id=${sim.subId ?: "?"}",
        )
        var rejected = try {
            call.reject(false, null)
            true
        } catch (e: Exception) {
            Log.w(TAG, "reject waiting call failed: ${e.message}")
            false
        }
        if (!rejected) {
            rejected = try {
                call.disconnect()
                true
            } catch (e: Exception) {
                Log.w(TAG, "disconnect waiting call failed: ${e.message}")
                false
            }
        }
        Log.i(TAG, "call waiting rejected=$rejected instance=$instanceId")
        try {
            val i = Intent(this, CallAudioService::class.java)
                .setAction(CallAudioService.ACTION_CALL_WAITING_REJECTED)
                .putExtra(CallAudioService.EXTRA_WAITING_CALLER_ID, caller)
                .putExtra(CallAudioService.EXTRA_SIM_COUNTRY, sim.country)
                .putExtra(CallAudioService.EXTRA_SIM_SUB_ID, sim.subId ?: -1)
            startCallService(i)
        } catch (e: Exception) {
            Log.w(TAG, "call waiting report failed: ${e.message}")
        }
    }

    private fun forceMuteCall(call: Call) {
        // Normal phone mode: the owner talks on this call; never mute it.
        if (!Prefs.isAiCallEnabled(this)) return
        // A call the owner dialed by hand on the phone: never mute it either.
        if (isOwnCall(call)) return
        try {
            setMuted(true)
            Log.i(TAG, "InCallService.setMuted(true) state=${call.state}")
        } catch (e: Exception) {
            Log.w(TAG, "setMuted failed: ${e.message}")
        }
    }

    /** No fallback to an older call's number: blank (hidden number) stays blank. */
    private fun ensureArmed(callerId: String, instanceId: Long) {
        if (!Prefs.isAiCallEnabled(this)) return
        try {
            val i = Intent(this, CallAudioService::class.java)
                .setAction(CallAudioService.ACTION_START)
                .putExtra(CallAudioService.EXTRA_CALL_INSTANCE_ID, instanceId)
            val id = callerId.trim()
            if (id.isNotBlank()) {
                i.putExtra(CallAudioService.EXTRA_CALLER_ID, id)
            }
            startCallService(i)
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
            val id = callerId?.trim().orEmpty()
            if (id.isNotBlank()) i.putExtra(CallAudioService.EXTRA_CALLER_ID, id)
            startCallService(i)
        } catch (e: Exception) {
            Log.w(TAG, "signal $action failed: ${e.message}")
        }
    }

    private fun startCallService(i: Intent) {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            startForegroundService(i)
        } else {
            startService(i)
        }
    }

    /**
     * A new call: drop the previous call's number and SIM, then keep this call's. The
     * stored copy ([Prefs]) follows the selected call only, so a call that is not
     * selected cannot overwrite the number of the call that is up.
     */
    private fun stashCallInfo(call: Call, instanceId: Long): CallInfo {
        val caller = handleOf(call)
        val sim = simOf(call)
        val info = CallInfo(instanceId, caller, sim.country, sim.subId)
        callInfos[instanceId] = info
        if (activeCallInstance.get() == instanceId) {
            Prefs.clearLastCallInfo(this)
            if (caller.isNotEmpty()) Prefs.setLastCallerId(this, caller)
            Prefs.setLastSimCountry(this, sim.country)
            Prefs.setLastSimSubId(this, sim.subId)
        }
        return info
    }

    /** The number can arrive after onCallAdded; keep the latest non-empty one. */
    private fun refreshCallerId(call: Call, instanceId: Long): String {
        val known = callInfos[instanceId]
        val handle = handleOf(call)
        if (handle.isEmpty() || known == null || handle == known.callerId) {
            return known?.callerId ?: handle
        }
        callInfos[instanceId] = CallInfo(instanceId, handle, known.simCountry, known.simSubId)
        if (activeCallInstance.get() == instanceId) Prefs.setLastCallerId(this, handle)
        Log.i(TAG, "caller_id now known instance=$instanceId len=${handle.length}")
        return handle
    }

    private fun handleOf(call: Call): String = try {
        call.details?.handle?.schemeSpecificPart?.trim().orEmpty()
    } catch (e: Exception) {
        Log.w(TAG, "caller id lookup failed: ${e.message}")
        ""
    }

    /** Which SIM the call uses: country (see [Prefs.lastSimCountry]) and subscription id. */
    private fun simOf(call: Call): SimInfo {
        // Before Android 11 there is no PhoneAccountHandle → subscription lookup: unknown.
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.R) return SimInfo("", null)
        return try {
            val handle = call.details?.accountHandle ?: return SimInfo("", null)
            val tm = getSystemService(TelephonyManager::class.java) ?: return SimInfo("", null)
            val subId = tm.getSubscriptionId(handle)
            if (!SubscriptionManager.isValidSubscriptionId(subId)) return SimInfo("", null)
            val iso = tm.createForSubscriptionId(subId).simCountryIso.orEmpty().trim().lowercase()
            SimInfo(iso, subId)
        } catch (e: Exception) {
            Log.w(TAG, "sim lookup failed: ${e.message}")
            SimInfo("", null)
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
        /** Number and SIM per call instance (instance ids grow with every call). */
        private val callInfos = ConcurrentHashMap<Long, CallInfo>()

        /**
         * Call waiting: an incoming call rings (or is new) while another call is dialing,
         * connecting, active or on hold.
         */
        internal fun isCallWaiting(incoming: Boolean, state: Int, otherCallStates: Collection<Int>): Boolean =
            incoming &&
                (state == Call.STATE_RINGING || state == Call.STATE_NEW) &&
                otherCallStates.any(::isEstablishedState)

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

        /** A call the owner dialed by hand is up: nothing may mute the phone now. */
        fun isOwnCallInProgress(): Boolean {
            if (activeService == null) return false
            return liveCalls.any {
                it.state != Call.STATE_DISCONNECTED && it.state != Call.STATE_DISCONNECTING && isOwnCall(it)
            }
        }

        /**
         * Number and SIM of call [instanceId] (of the selected call for 0). Null without
         * InCallService or before onCallAdded stored it.
         */
        fun callInfo(instanceId: Long): CallInfo? {
            if (activeService == null) return null
            val id = if (instanceId != NO_CALL_INSTANCE) instanceId else activeCallInstance.get()
            return if (id == NO_CALL_INSTANCE) null else callInfos[id]
        }

        /**
         * The ringing call CallAudioService is about to report, once onCallAdded stored
         * its number and SIM. Null while that has not happened yet, and for call waiting.
         */
        fun ringingCallInfo(): CallInfo? {
            if (activeService == null) return null
            if (liveCalls.any(::isEstablishedCall)) return null
            val call = ringingCall() ?: return null
            val id = callInstances[call] ?: return null
            return callInfos[id]
        }

        fun currentActiveCallInstanceId(): Long =
            if (activeService == null) NO_CALL_INSTANCE else activeCallInstance.get()

        private fun ringingCall(): Call? =
            activeCall?.takeIf { it in liveCalls && it.state == Call.STATE_RINGING }
                ?: liveCalls.firstOrNull { it.state == Call.STATE_RINGING }

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

        private fun isEstablishedCall(call: Call): Boolean = isEstablishedState(call.state)

        private fun isEstablishedState(state: Int): Boolean =
            when (state) {
                Call.STATE_ACTIVE,
                Call.STATE_HOLDING,
                Call.STATE_CONNECTING,
                Call.STATE_DIALING,
                -> true
                else -> false
            }

        /** Dialed on this phone by the owner (not a `place_call` from the agent). */
        private fun isOwnCall(call: Call): Boolean = try {
            call.details.callDirection == Call.Details.DIRECTION_OUTGOING &&
                !CallAudioService.isAgentCallbackPending()
        } catch (_: Exception) {
            false
        }

        /** Re-assert Telecom mic mute while we own the active call (hold loop). */
        fun forceMuteActive(): Boolean {
            val svc = activeService ?: return false
            if (isOwnCallInProgress()) return false
            return try {
                svc.setMuted(true)
                true
            } catch (e: Exception) {
                Log.w(TAG, "forceMuteActive failed: ${e.message}")
                false
            }
        }

        /** The owner's own call: take back the Telecom mute this app may have set. */
        fun unmuteOwnCall(): Boolean {
            val svc = activeService ?: return false
            return try {
                svc.setMuted(false)
                Log.i(TAG, "InCallService.setMuted(false) for the owner's own call")
                true
            } catch (e: Exception) {
                Log.w(TAG, "unmuteOwnCall failed: ${e.message}")
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

        /** Agent `answer_plan` with `reject`: decline the ringing call without answering it. */
        fun rejectRinging(): Boolean {
            if (activeService == null) return false
            val call = ringingCall() ?: return false
            return try {
                call.reject(false, null)
                Log.i(TAG, "rejectRinging issued")
                true
            } catch (e: Exception) {
                Log.w(TAG, "rejectRinging failed: ${e.message}")
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

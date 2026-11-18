// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

package com.pixelcallai.streamer

import android.content.Intent
import android.content.Context
import android.net.Uri
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.os.SystemClock
import android.telecom.Call
import android.telecom.DisconnectCause
import android.telecom.InCallService
import android.telecom.PhoneAccountHandle
import android.telecom.TelecomManager
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
 *   asked for (`place_call`) are not "by hand": that is decided once per call, when it is
 *   added ([CallInfo.agentCallback]), so clearing the agent's pending call later cannot
 *   turn a callback into a call by hand.
 */
class SilentInCallService : InCallService() {
    /** Number and SIM of one call as the agent gets them. [callerId] "" = hidden or unknown. */
    data class CallInfo(
        val instanceId: Long,
        val callerId: String,
        val simCountry: String,
        val simSubId: Int?,
        /** Placed from this phone (by hand or for the agent). */
        val outgoing: Boolean,
        /** Placed for the agent's `place_call` (outgoing while it was pending when the call was added). */
        val agentCallback: Boolean,
    ) {
        /** Dialed by hand by the owner: never muted, no AI. */
        val ownCall: Boolean get() = outgoing && !agentCallback
    }

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
        // A call that rings while another is up is declined first, also during a whitelist
        // urgent call's merge: it is never taken as a part of that call (the owner is
        // talking on it). Calls the merge itself makes are told apart by isMergeMadeCall.
        val job = addJob
        val madeByMerge = job != null && job.continuesCall() && isMergeMadeCall(call)
        if (!madeByMerge && Prefs.isAiCallEnabled(this) && isWaitingCall(call)) {
            rejectWaitingCall(call, instanceId)
            return
        }
        // Whitelist urgent call: the owner leg and the merged conference are handled apart.
        if (adoptForAddCall(call, instanceId)) return
        val current = activeCall
        if (current == null || current !in liveCalls || !isEstablishedCall(current)) {
            selectActiveCall(call, instanceId)
        }
        val state = call.state
        val outgoing = try {
            call.details.callDirection == Call.Details.DIRECTION_OUTGOING
        } catch (_: Exception) {
            false
        }
        // Decided now, for this call only: the agent's pending place_call is the call it
        // asked for. Later changes of that pending state do not change this call.
        val agentCallback = outgoing && CallAudioService.isAgentCallbackPending()
        val info = stashCallInfo(call, instanceId, outgoing, agentCallback)
        val ownCall = info.ownCall
        Log.i(
            TAG,
            "onCallAdded state=$state instance=$instanceId caller_len=${info.callerId.length} " +
                "sim_country=${info.simCountry.ifEmpty { "?" }} sim_sub_id=${info.simSubId ?: "?"} " +
                "outgoing=$outgoing agent_callback=$agentCallback own=$ownCall",
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
        // Whitelist urgent call: while the merge runs (and after it) the original call's
        // instance goes on in the calls the merge makes, even before they show up.
        val job = addJob
        val keepsOriginal = job != null && job.continuesCall() && instanceId == job.originalInstance
        // A merged conference keeps the original call's instance: keep its info too.
        if (!keepsOriginal && !callInstances.containsValue(instanceId)) callInfos.remove(instanceId)
        // Every mode (normal phone mode never reaches CallAudioService's cleanup): with
        // no call left, nothing stored may outlive it (number, SIM country, sub id).
        if (liveCalls.isEmpty()) Prefs.clearLastCallInfo(this)
        // A rejected waiting call was never selected: removing it changes nothing for
        // the call that stays up (no replacement, no CALL_ENDED).
        val removedActiveCall = activeCallInstance.compareAndSet(instanceId, NO_CALL_INSTANCE)
        if (removedActiveCall) {
            // A call with the same instance (the merged conference) goes on as this call.
            val sameInstance = liveCalls.firstOrNull { callInstances[it] == instanceId }
            if (keepsOriginal && sameInstance == null) {
                // The merge ended the original call before its conference showed up: keep
                // the instance (no CALL_ENDED, no media switch) — the conference takes it.
                val stand = liveCalls.firstOrNull { it.state != Call.STATE_DISCONNECTED }
                activeCall = stand
                activeCallInstance.set(instanceId)
                Log.i(TAG, "add_call id=${job?.id}: original call removed during the merge — instance=$instanceId kept for the conference")
            } else (sameInstance ?: preferredLiveCall())?.let { replacement ->
                selectActiveCall(replacement, callInstances[replacement] ?: NO_CALL_INSTANCE)
            } ?: run {
                activeCall = null
            }
        }
        val continued = removedActiveCall && activeCallInstance.get() == instanceId
        Log.i(TAG, "onCallRemoved instance=$instanceId selected=$removedActiveCall continued=$continued")
        addJob?.let { onAddCallRemoved(it, call) }
        if (removedActiveCall && !continued) {
            signalCallEnded(instanceId)
        }
    }

    override fun onCanAddCallChanged(canAddCall: Boolean) {
        // no UI
    }

    /**
     * Whitelist urgent call: take the owner's leg or the merged conference out of the
     * normal handling. The leg is muted and never armed (no second AI session); the
     * conference keeps the original call's instance (same media session and call_end).
     * True when the call was taken.
     */
    private fun adoptForAddCall(call: Call, instanceId: Long): Boolean {
        val job = addJob ?: return false
        if (call === job.original || call === job.leg) return false
        // During the merge and after it, every new call is this call going on (Pixel 9 /
        // Tello: the merge ends both calls and adds the caller and the owner as
        // participants plus a PROPERTY_CONFERENCE host). Never a new call, never the AI.
        if (job.continuesCall()) {
            adoptContinuation(job, call, instanceId)
            return true
        }
        val outgoing = try {
            call.details.callDirection == Call.Details.DIRECTION_OUTGOING
        } catch (_: Exception) {
            false
        }
        if (!outgoing || job.leg != null) return false
        // After a failure only a leg that shows up shortly after (placeCall is async) is ours.
        if (job.done && SystemClock.elapsedRealtime() > job.lateLegUntil) return false
        job.leg = call
        val sim = simOf(call)
        callInfos[instanceId] = CallInfo(instanceId, handleOf(call), sim.country, sim.subId, outgoing = true, agentCallback = true)
        if (job.done) {
            // The attempt already gave up (e.g. no outgoing call within the dial timeout).
            Log.w(TAG, "add_call id=${job.id}: owner leg showed up after the attempt ended — disconnecting it")
            try {
                call.disconnect()
            } catch (e: Exception) {
                Log.w(TAG, "add_call id=${job.id}: late leg disconnect failed: ${e.message}")
            }
            return true
        }
        Log.i(
            TAG,
            "add_call id=${job.id}: owner leg added instance=$instanceId state=${stateName(call.state)} " +
                "number=${job.masked} sim_sub_id=${sim.subId ?: "?"} original_state=${stateName(job.original.state)}",
        )
        forceMuteCall(call)
        call.registerCallback(object : Call.Callback() {
            override fun onStateChanged(call: Call, state: Int) {
                Log.i(
                    TAG,
                    "add_call id=${job.id}: owner leg state -> ${stateName(state)} " +
                        "original_state=${stateName(job.original.state)} parent=${call.parent != null}",
                )
                forceMuteCall(call)
                onLegState(job, call, state)
            }

            override fun onParentChanged(call: Call, parent: Call?) {
                Log.i(TAG, "add_call id=${job.id}: owner leg parent=${parent != null}")
                if (parent != null && !job.done) onAddCallMerged(job, "owner leg joined a conference")
            }

            override fun onConferenceableCallsChanged(call: Call, conferenceableCalls: List<Call>) {
                Log.i(
                    TAG,
                    "add_call id=${job.id}: owner leg conferenceable=${conferenceableCalls.size} " +
                        "with_original=${conferenceableCalls.contains(job.original)}",
                )
            }
        })
        onLegState(job, call, call.state)
        return true
    }

    /**
     * A call the merge made (conference host or participant): mapped to the original
     * call's instance (same media session, same call_end), muted, no RINGING flow, no
     * answer gate, no AI. Merged once a conference host shows up or one of them is
     * ACTIVE on the original call's phone account.
     */
    private fun adoptContinuation(job: AddCallJob, call: Call, instanceId: Long) {
        callInstances[call] = job.originalInstance
        job.adopted.add(call)
        val details = call.details
        val host = try {
            details.hasProperty(Call.Details.PROPERTY_CONFERENCE)
        } catch (_: Exception) {
            false
        }
        val canLeaveConference = try {
            details.can(Call.Details.CAPABILITY_DISCONNECT_FROM_CONFERENCE)
        } catch (_: Exception) {
            false
        }
        val participant = call.parent != null || canLeaveConference
        val sameAccount = details.accountHandle == job.account
        val role = when {
            host -> "conference host"
            participant -> "participant"
            else -> "other"
        }
        val direction = try {
            if (details.callDirection == Call.Details.DIRECTION_OUTGOING) "out" else "in"
        } catch (_: Exception) {
            "?"
        }
        Log.i(
            TAG,
            "add_call id=${job.id}: new call during/after the merge taken as the same call role=$role " +
                "state=${stateName(call.state)} dir=$direction children=${call.children.size} same_account=$sameAccount " +
                "caps=[${capsOf(call)}] props=[${propsOf(call)}] instance=$instanceId->${job.originalInstance} (no ringing, no AI)",
        )
        forceMuteCall(call)
        val current = activeCall
        if (host) {
            job.conference = call
            selectActiveCall(call, job.originalInstance)
        } else if (current == null || current !in liveCalls) {
            selectActiveCall(call, job.originalInstance)
        }
        call.registerCallback(object : Call.Callback() {
            override fun onStateChanged(call: Call, state: Int) {
                Log.i(TAG, "add_call id=${job.id}: $role state -> ${stateName(state)} children=${call.children.size}")
                forceMuteCall(call)
                if (!job.done && state == Call.STATE_ACTIVE && sameAccount) {
                    onAddCallMerged(job, "$role ACTIVE on the original call's account")
                }
            }

            override fun onChildrenChanged(call: Call, children: List<Call>) {
                job.peakChildren = maxOf(job.peakChildren, children.size)
                Log.i(TAG, "add_call id=${job.id}: $role children=${children.size} peak=${job.peakChildren}")
                if (!job.done && children.isNotEmpty()) onAddCallMerged(job, "$role has ${children.size} children")
                if (job.merged && job.peakChildren >= 2 && children.size < 2) {
                    endRestAfterMerge(job, "conference dropped to ${children.size} participant(s)")
                }
            }

            override fun onParentChanged(call: Call, parent: Call?) {
                Log.i(TAG, "add_call id=${job.id}: $role parent=${parent != null}")
            }
        })
        // Ordinary GSM / VoLTE networks: the original call and the owner leg are already the
        // conference's children when the host shows up, so no later change reports "2". Count
        // them now, or the drop to one person after the owner hangs up would never be noticed.
        job.peakChildren = maxOf(job.peakChildren, call.children.size, call.parent?.children?.size ?: 0)
        if (!job.done) {
            when {
                host -> onAddCallMerged(job, "conference host added (PROPERTY_CONFERENCE)")
                call.children.isNotEmpty() -> onAddCallMerged(job, "$role added with ${call.children.size} children")
                call.state == Call.STATE_ACTIVE && sameAccount -> onAddCallMerged(job, "$role added ACTIVE on the original call's account")
            }
        }
    }

    /**
     * A call the conference merge made: the host, a participant of a conference, or a call
     * that is not ringing at all. A new call that rings is none of these: it stays a waiting call.
     */
    private fun isMergeMadeCall(call: Call): Boolean {
        if (call.state != Call.STATE_RINGING) return true
        val details = try {
            call.details
        } catch (_: Exception) {
            null
        }
        val host = try {
            details?.hasProperty(Call.Details.PROPERTY_CONFERENCE) == true
        } catch (_: Exception) {
            false
        }
        val participant = try {
            call.parent != null || details?.can(Call.Details.CAPABILITY_DISCONNECT_FROM_CONFERENCE) == true
        } catch (_: Exception) {
            false
        }
        return host || participant
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
    private fun stashCallInfo(call: Call, instanceId: Long, outgoing: Boolean, agentCallback: Boolean): CallInfo {
        val caller = handleOf(call)
        val sim = simOf(call)
        val info = CallInfo(instanceId, caller, sim.country, sim.subId, outgoing, agentCallback)
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
        callInfos[instanceId] = known.copy(callerId = handle)
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

        /** Whitelist urgent call: the second call to the owner and how far it got (main thread). */
        private class AddCallJob(
            val id: String,
            val masked: String,
            val original: Call,
            val originalInstance: Long,
            val account: PhoneAccountHandle,
            val report: (String, String) -> Unit,
        ) {
            var leg: Call? = null
            var conference: Call? = null
            var dialingReported = false
            var answered = false
            var mergeAttempts = 0
            @Volatile var done = false
            @Volatile var merged = false
            var failReported = false
            /** Failed before the owner leg showed up: until then a new outgoing call is that leg. */
            var lateLegUntil = 0L
            /** Most children the merged conference had (2 = caller and owner both on it). */
            var peakChildren = 0
            /** Calls the merge made (conference host, participants): the same call going on. */
            val adopted: MutableSet<Call> = java.util.Collections.newSetFromMap(ConcurrentHashMap<Call, Boolean>())

            /** Merge requested and not settled, or merged: new calls are this call going on. */
            fun continuesCall(): Boolean = (mergeAttempts > 0 && !done) || merged
        }

        @Volatile
        private var addJob: AddCallJob? = null
        private val addCallHandler = Handler(Looper.getMainLooper())
        private const val ADD_CALL_DIAL_TIMEOUT_MS = 10_000L
        private const val ADD_CALL_MERGE_RETRY_MS = 3_000L
        private const val ADD_CALL_UNHOLD_TRIES = 8
        private const val ADD_CALL_UNHOLD_EVERY_MS = 1_000L
        private const val ADD_CALL_REST_CHECK_MS = 1_500L
        /** After the conference request: removed calls and new calls are the merge at work. */
        private const val ADD_CALL_MERGE_WINDOW_MS = 15_000L

        /** Only the last four digits go to the log. */
        fun maskNumber(number: String): String = "***" + number.filter { it.isDigit() }.takeLast(4)

        /**
         * A whitelist urgent call is dialing the owner, or the owner is merged in: the
         * agent's hangup must not end the caller's call now.
         */
        fun isAddCallActiveOrMerged(): Boolean {
            val job = addJob ?: return false
            return !job.done || job.merged
        }

        /**
         * Agent `add_call_abort` (no final result in time, main thread): end the owner leg
         * and take the caller's call off hold; merged already: say so again.
         */
        fun abortAddCall(id: String) {
            val job = addJob
            if (job == null || job.id != id) {
                Log.i(TAG, "add_call_abort id=$id: no such add_call (current=${job?.id ?: "none"})")
                return
            }
            when {
                job.merged -> {
                    Log.i(TAG, "add_call_abort id=$id: already merged — reporting merged again")
                    job.report("merged", "")
                }
                !job.done -> {
                    Log.w(TAG, "add_call_abort id=$id: agent gave up — ending the owner leg, taking the call off hold")
                    failAddCall(job, "aborted")
                }
                else -> Log.i(TAG, "add_call_abort id=$id: already ended (failed, reported=${job.failReported})")
            }
        }

        private fun capsOf(call: Call): String = try {
            Call.Details.capabilitiesToString(call.details.callCapabilities)
        } catch (_: Exception) {
            "?"
        }

        private fun propsOf(call: Call): String = try {
            Call.Details.propertiesToString(call.details.callProperties)
        } catch (_: Exception) {
            "?"
        }

        private fun stateName(state: Int): String = when (state) {
            Call.STATE_NEW -> "NEW"
            Call.STATE_DIALING -> "DIALING"
            Call.STATE_RINGING -> "RINGING"
            Call.STATE_HOLDING -> "HOLDING"
            Call.STATE_ACTIVE -> "ACTIVE"
            Call.STATE_DISCONNECTED -> "DISCONNECTED"
            Call.STATE_SELECT_PHONE_ACCOUNT -> "SELECT_PHONE_ACCOUNT"
            Call.STATE_CONNECTING -> "CONNECTING"
            Call.STATE_DISCONNECTING -> "DISCONNECTING"
            Call.STATE_PULLING_CALL -> "PULLING_CALL"
            else -> "STATE_$state"
        }

        /**
         * Agent `add_call` (main thread): put the active call on hold by dialing [number]
         * from the same phone account (same SIM), then merge once the owner answers.
         * [report] gets dialing / active / merged / failed(reason). The original call is
         * never disconnected here: without hold support nothing is dialed at all.
         */
        fun startAddCall(
            context: Context,
            id: String,
            number: String,
            answerTimeoutMs: Long,
            report: (String, String) -> Unit,
        ) {
            val masked = maskNumber(number)
            // Never dial service codes (* #) or emergency / short numbers.
            if (number.contains('*') || number.contains('#') || number.count { it.isDigit() } <= 3) {
                Log.w(TAG, "add_call id=$id: number $masked refused (service code or too short) — not dialing")
                report("failed", "bad_number")
                return
            }
            val svc = activeService
            if (svc == null) {
                Log.w(TAG, "add_call id=$id: InCallService not bound")
                report("failed", "no_incall_service")
                return
            }
            addJob?.let {
                if (!it.done) {
                    Log.w(TAG, "add_call id=$id: add_call ${it.id} still running")
                    report("failed", "busy")
                    return
                }
            }
            val original = activeCall
            val originalInstance = activeCallInstance.get()
            if (original == null || original !in liveCalls || original.state != Call.STATE_ACTIVE) {
                Log.w(TAG, "add_call id=$id: no active call to put on hold state=${original?.let { stateName(it.state) }}")
                report("failed", "no_active_call")
                return
            }
            val canAdd = try {
                svc.canAddCall()
            } catch (_: Exception) {
                false
            }
            Log.i(
                TAG,
                "add_call id=$id number=$masked original instance=$originalInstance caps=[${capsOf(original)}] " +
                    "props=[${propsOf(original)}] can_add_call=$canAdd live_calls=${liveCalls.size}",
            )
            if (!canAdd) {
                Log.w(TAG, "add_call id=$id: Telecom says no call can be added now — not dialing")
                report("failed", "cannot_add_call")
                return
            }
            if (!original.details.can(Call.Details.CAPABILITY_HOLD)) {
                Log.w(TAG, "add_call id=$id: original call has no CAPABILITY_HOLD — not dialing (it could drop the call)")
                report("failed", "cannot_hold")
                return
            }
            val account = original.details.accountHandle
            if (account == null) {
                Log.w(TAG, "add_call id=$id: original call has no phone account — not dialing")
                report("failed", "no_account")
                return
            }
            val telecom = context.getSystemService(TelecomManager::class.java)
            if (telecom == null) {
                report("failed", "no_telecom")
                return
            }
            val job = AddCallJob(id, masked, original, originalInstance, account, report)
            addJob = job
            try {
                val extras = Bundle()
                extras.putParcelable(TelecomManager.EXTRA_PHONE_ACCOUNT_HANDLE, account)
                telecom.placeCall(Uri.fromParts("tel", number, null), extras)
                Log.i(TAG, "add_call id=$id: placeCall issued on the original call's phone account (same SIM)")
            } catch (e: SecurityException) {
                Log.w(TAG, "add_call id=$id: placeCall denied: ${e.message}")
                failAddCall(job, "dial_denied")
                return
            } catch (e: Exception) {
                Log.w(TAG, "add_call id=$id: placeCall failed: ${e.message}")
                failAddCall(job, "dial_failed")
                return
            }
            addCallHandler.postDelayed({
                if (!job.done && job.leg == null) {
                    Log.w(TAG, "add_call id=$id: no outgoing call appeared within ${ADD_CALL_DIAL_TIMEOUT_MS}ms")
                    failAddCall(job, "not_dialed")
                }
            }, ADD_CALL_DIAL_TIMEOUT_MS)
            addCallHandler.postDelayed({
                if (!job.done && !job.answered) {
                    Log.w(TAG, "add_call id=$id: owner did not answer within ${answerTimeoutMs}ms")
                    failAddCall(job, "no_answer")
                }
            }, answerTimeoutMs)
        }

        private fun onLegState(job: AddCallJob, leg: Call, state: Int) {
            if (job.done) return
            when (state) {
                Call.STATE_SELECT_PHONE_ACCOUNT -> {
                    Log.i(TAG, "add_call id=${job.id}: Telecom asks for a SIM — choosing the original call's")
                    try {
                        leg.phoneAccountSelected(job.account, false)
                    } catch (e: Exception) {
                        Log.w(TAG, "add_call id=${job.id}: phoneAccountSelected failed: ${e.message}")
                    }
                }
                Call.STATE_NEW, Call.STATE_CONNECTING, Call.STATE_DIALING -> {
                    if (!job.dialingReported) {
                        job.dialingReported = true
                        job.report("dialing", "")
                    }
                }
                Call.STATE_ACTIVE -> {
                    if (!job.answered) {
                        job.answered = true
                        Log.i(TAG, "add_call id=${job.id}: owner answered — merging shortly")
                        job.report("active", "")
                        addCallHandler.postDelayed({ tryMerge(job) }, 600)
                    }
                }
                Call.STATE_DISCONNECTED -> onLegGone(job, leg)
            }
        }

        private fun legEndReason(leg: Call): String {
            val code = try {
                leg.details.disconnectCause?.code
            } catch (_: Exception) {
                null
            }
            return when (code) {
                DisconnectCause.BUSY -> "busy"
                DisconnectCause.REJECTED -> "rejected"
                DisconnectCause.ERROR -> "dial_error"
                DisconnectCause.REMOTE -> "owner_hung_up"
                DisconnectCause.RESTRICTED -> "restricted"
                else -> "leg_ended"
            }
        }

        private fun onLegGone(job: AddCallJob, leg: Call) {
            val reason = legEndReason(leg)
            if (job.mergeAttempts > 0) {
                // An IMS merge ends both calls and goes on in new ones (Pixel 9 / Tello:
                // cause REMOTE, ImsReasonInfo 510 / 108). Decided when the merge window ends.
                Log.i(
                    TAG,
                    "add_call id=${job.id}: owner leg ended during the merge cause=$reason — not a failure, " +
                        "waiting for the conference (merge window)",
                )
                return
            }
            Log.w(TAG, "add_call id=${job.id}: owner leg ended before a merge cause=$reason")
            failAddCall(job, reason)
        }

        private fun findConference(job: AddCallJob): Call? =
            job.conference?.takeIf { it in liveCalls }
                ?: job.original.parent?.takeIf { it in liveCalls }
                ?: job.adopted.firstOrNull { it in liveCalls && it.children.isNotEmpty() }
                ?: liveCalls.firstOrNull {
                    it.state == Call.STATE_ACTIVE &&
                        try {
                            it.details.hasProperty(Call.Details.PROPERTY_CONFERENCE)
                        } catch (_: Exception) {
                            false
                        }
                }

        /**
         * Merge as the stock dialer does: the active call (the owner leg) conferences a
         * call from its conferenceable list, else MERGE_CONFERENCE (CDMA / some IMS).
         * A second try uses the next way; no conference in time fails the attempt.
         */
        private fun tryMerge(job: AddCallJob) {
            if (job.done) return
            val leg = job.leg
            if (leg == null || leg !in liveCalls) {
                failAddCall(job, "leg_ended")
                return
            }
            val orig = job.original
            if (orig.parent != null || leg.parent != null || findConference(job) != null) {
                onAddCallMerged(job, "already in a conference")
                return
            }
            job.mergeAttempts++
            val legConf = leg.conferenceableCalls
            val origConf = orig.conferenceableCalls
            Log.i(
                TAG,
                "add_call id=${job.id}: merge attempt=${job.mergeAttempts} " +
                    "leg state=${stateName(leg.state)} caps=[${capsOf(leg)}] conferenceable=${legConf.size} has_original=${legConf.contains(orig)} | " +
                    "original state=${stateName(orig.state)} caps=[${capsOf(orig)}] conferenceable=${origConf.size} has_leg=${origConf.contains(leg)}",
            )
            val first = job.mergeAttempts == 1
            val how = try {
                when {
                    first && legConf.contains(orig) -> {
                        leg.conference(orig)
                        "leg.conference(original)"
                    }
                    first && origConf.contains(leg) -> {
                        orig.conference(leg)
                        "original.conference(leg)"
                    }
                    leg.details.can(Call.Details.CAPABILITY_MERGE_CONFERENCE) -> {
                        leg.mergeConference()
                        "leg.mergeConference()"
                    }
                    orig.details.can(Call.Details.CAPABILITY_MERGE_CONFERENCE) -> {
                        orig.mergeConference()
                        "original.mergeConference()"
                    }
                    first -> {
                        leg.conference(orig)
                        "leg.conference(original) without conferenceable list or MERGE_CONFERENCE"
                    }
                    else -> {
                        orig.conference(leg)
                        "original.conference(leg) without conferenceable list or MERGE_CONFERENCE"
                    }
                }
            } catch (e: Exception) {
                "error ${e.message}"
            }
            Log.i(TAG, "add_call id=${job.id}: merge requested via $how")
            if (job.mergeAttempts == 1) {
                addCallHandler.postDelayed({ mergeWindowEnded(job) }, ADD_CALL_MERGE_WINDOW_MS)
                // A second try only while nothing moved (both calls up, no new calls yet).
                addCallHandler.postDelayed({
                    val legUp = job.leg?.let { it in liveCalls } == true
                    if (!job.done && job.adopted.isEmpty() && job.original in liveCalls && legUp) tryMerge(job)
                }, ADD_CALL_MERGE_RETRY_MS)
            }
        }

        /**
         * The merge window is over without a clear merge signal: judged by what is still up.
         * Only "nothing left" counts as the caller gone.
         */
        private fun mergeWindowEnded(job: AddCallJob) {
            if (job.done) return
            val live = liveCalls.filter { it.state != Call.STATE_DISCONNECTED && it.state != Call.STATE_DISCONNECTING }
            val origUp = job.original in live
            val legUp = job.leg?.let { it in live } == true
            val madeUp = job.adopted.count { it in live }
            Log.i(
                TAG,
                "add_call id=${job.id}: merge window over live=${live.size} original_up=$origUp leg_up=$legUp merge_calls_up=$madeUp",
            )
            when {
                live.isEmpty() -> failAddCall(job, "caller_left")
                madeUp > 0 && !origUp -> onAddCallMerged(job, "merge window over: only calls the merge made are up ($madeUp)")
                origUp && legUp -> failAddCall(job, "merge_failed")
                origUp -> failAddCall(job, "leg_ended")
                legUp -> failAddCall(job, "caller_left")
                else -> onAddCallMerged(job, "merge window over: ${live.size} call(s) still up")
            }
        }

        private fun onAddCallMerged(job: AddCallJob, why: String) {
            if (job.done) return
            job.done = true
            job.merged = true
            addCallHandler.removeCallbacksAndMessages(null)
            Log.i(TAG, "add_call id=${job.id}: MERGED ($why) — the AI leaves, the call stays up")
            job.report("merged", "")
        }

        /** End the owner leg, take the original call off hold, then report the failure. */
        private fun failAddCall(job: AddCallJob, reason: String) {
            if (job.done) return
            job.done = true
            addCallHandler.removeCallbacksAndMessages(null)
            val leg = job.leg
            if (leg == null) job.lateLegUntil = SystemClock.elapsedRealtime() + 15_000L
            Log.w(
                TAG,
                "add_call id=${job.id}: FAILED reason=$reason leg_state=${leg?.let { stateName(it.state) } ?: "none"} " +
                    "original_state=${stateName(job.original.state)}",
            )
            if (leg != null && leg.state != Call.STATE_DISCONNECTED && leg.state != Call.STATE_DISCONNECTING) {
                try {
                    leg.disconnect()
                    Log.i(TAG, "add_call id=${job.id}: owner leg disconnected")
                } catch (e: Exception) {
                    Log.w(TAG, "add_call id=${job.id}: owner leg disconnect failed: ${e.message}")
                }
            }
            if (job.original !in liveCalls) {
                reportFailed(job, reason)
                return
            }
            addCallHandler.postDelayed({ restoreOriginal(job, reason, 1) }, 500)
        }

        /**
         * Failed only once the caller's call is ACTIVE again (the AI would otherwise talk
         * into a held call): unhold every second, at most [ADD_CALL_UNHOLD_TRIES] times,
         * then report `unhold_failed` (the agent alerts the owner, the AI stays quiet).
         */
        private fun restoreOriginal(job: AddCallJob, reason: String, attempt: Int) {
            val orig = job.original
            if (orig !in liveCalls) {
                Log.w(TAG, "add_call id=${job.id}: original call is gone")
                reportFailed(job, "caller_left")
                return
            }
            val state = orig.state
            if (state == Call.STATE_ACTIVE) {
                Log.i(TAG, "add_call id=${job.id}: original call active again (attempt=$attempt) — reporting failed reason=$reason")
                reportFailed(job, reason)
                return
            }
            if (attempt > ADD_CALL_UNHOLD_TRIES) {
                Log.w(TAG, "add_call id=${job.id}: original still ${stateName(state)} after $ADD_CALL_UNHOLD_TRIES unhold tries — unhold_failed")
                reportFailed(job, "unhold_failed")
                return
            }
            if (state == Call.STATE_HOLDING) {
                try {
                    orig.unhold()
                    Log.i(TAG, "add_call id=${job.id}: unhold original attempt=$attempt")
                } catch (e: Exception) {
                    Log.w(TAG, "add_call id=${job.id}: unhold failed attempt=$attempt: ${e.message}")
                }
            } else {
                Log.i(TAG, "add_call id=${job.id}: original state=${stateName(state)} attempt=$attempt")
            }
            addCallHandler.postDelayed({ restoreOriginal(job, reason, attempt + 1) }, ADD_CALL_UNHOLD_EVERY_MS)
        }

        private fun reportFailed(job: AddCallJob, reason: String) {
            if (job.failReported) return
            job.failReported = true
            job.report("failed", reason)
        }

        /** A call left while an add_call runs or after it merged. */
        private fun onAddCallRemoved(job: AddCallJob, call: Call) {
            if (!job.done) {
                if (call === job.original) {
                    if (job.mergeAttempts > 0) {
                        val reason = try {
                            call.details.disconnectCause?.reason.orEmpty()
                        } catch (_: Exception) {
                            ""
                        }
                        Log.i(
                            TAG,
                            "add_call id=${job.id}: original call removed during the merge cause=${legEndReason(call)} " +
                                "reason=${reason.ifEmpty { "-" }} — not caller_left, waiting for the conference",
                        )
                        return
                    }
                    Log.w(TAG, "add_call id=${job.id}: caller hung up while the owner was dialed")
                    failAddCall(job, "caller_left")
                }
                return
            }
            if (job.merged) {
                // The original call and the owner leg end as part of the merge: not a leave.
                // Who left is read from the calls the merge made (and the host's children).
                if (call in job.adopted) {
                    val which = if (call === job.conference) "conference host" else "participant"
                    Log.i(TAG, "add_call id=${job.id}: after the merge the $which call was removed cause=${legEndReason(call)}")
                    endRestAfterMerge(job, "$which left (${legEndReason(call)})")
                } else {
                    Log.i(TAG, "add_call id=${job.id}: after the merge a pre-merge call was removed (part of the merge) cause=${legEndReason(call)}")
                }
            }
            if (liveCalls.isEmpty() && addJob === job) addJob = null
        }

        /**
         * After the merge one side left: the AI has left the call already, so whoever is
         * still on would wait on an empty line. End what is left — the only case where
         * the phone ends a call by itself. A conference still holding two people is kept.
         */
        private fun endRestAfterMerge(job: AddCallJob, why: String) {
            addCallHandler.postDelayed({
                val rest = liveCalls.filter { it.state != Call.STATE_DISCONNECTED && it.state != Call.STATE_DISCONNECTING }
                if (rest.isEmpty()) return@postDelayed
                val conf = findConference(job)
                // A live conference that never listed its people (some IMS) cannot be
                // judged: it is kept rather than ended by a guess.
                if (conf != null && (conf.children.size >= 2 || job.peakChildren == 0)) {
                    Log.i(
                        TAG,
                        "add_call id=${job.id}: $why but the conference is still up children=${conf.children.size} " +
                            "peak=${job.peakChildren} — keeping it",
                    )
                    return@postDelayed
                }
                Log.i(TAG, "add_call id=${job.id}: after the merge the $why — the AI has left, ending the rest (${rest.size}) so nobody waits on an empty line")
                for (c in rest) {
                    try {
                        c.disconnect()
                    } catch (e: Exception) {
                        Log.w(TAG, "add_call id=${job.id}: disconnect after merge failed: ${e.message}")
                    }
                }
            }, ADD_CALL_REST_CHECK_MS)
        }


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
         * True when the live call was dialed by hand on this phone; false for an incoming
         * call or an agent callback. Null when InCallService is not bound or has not
         * stored the call yet (caller must stay conservative).
         */
        fun isOwnCallUp(): Boolean? {
            if (activeService == null) return null
            val call = preferredLiveCall() ?: return null
            val id = callInstances[call] ?: return null
            return callInfos[id]?.ownCall
        }

        /** Call [instanceId] was dialed by hand on this phone (false when unknown). */
        fun isOwnCallInstance(instanceId: Long): Boolean =
            activeService != null && callInfos[instanceId]?.ownCall == true

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

        /**
         * Dialed on this phone by the owner, as stored when the call was added (not a
         * `place_call` from the agent). A call without stored info is not treated as one,
         * so it stays muted.
         */
        private fun isOwnCall(call: Call): Boolean {
            val id = callInstances[call] ?: return false
            return callInfos[id]?.ownCall == true
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

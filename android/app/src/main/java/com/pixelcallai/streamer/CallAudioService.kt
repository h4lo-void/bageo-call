package com.pixelcallai.streamer

import java.io.File

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.pm.ServiceInfo
import android.content.Intent
import android.os.Build
import android.os.Handler
import android.os.IBinder
import android.os.Looper
import android.os.Process
import android.os.SystemClock
import android.telecom.TelecomManager
import android.telephony.PhoneStateListener
import android.telephony.TelephonyCallback
import android.telephony.TelephonyManager
import android.util.Log
import androidx.core.app.NotificationCompat
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.async
import kotlinx.coroutines.cancel
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.withTimeoutOrNull
import java.util.concurrent.atomic.AtomicLong
import java.util.concurrent.Executors
import java.util.concurrent.Future

/**
 * Background watcher + silent AI call handler.
 * - Normal phone mode ([Prefs.phoneMode] = normal): only the CONTROL_IDLE connection
 *   (status + remote mode switch). No muting, no roles, no answering.
 * - Idle/armed: CONTROL_IDLE WebSocket to the agent; hard-mute local mic/speaker.
 * - RINGING: wait until the agent control path is up, report `incoming`, ring for the
 *   agent's `answer_plan` (~one ring; the agent warms up the AI session meanwhile),
 *   then answer (or decline when the plan says `reject`). Agent not up in time: the call
 *   is not answered and keeps ringing (carrier missed call / voicemail).
 * - OFFHOOK: TYPE_TELEPHONY inject + media BridgeClient; the agent decides what the
 *   caller hears (AI voice). No phone-local menu (LOCAL_MENU_ENABLED=false).
 *   A call dialed by hand on the phone gets no AI and is not muted. If the media
 *   connection drops and is not back within [MEDIA_LOST_HANGUP_MS], the call is ended.
 */
class CallAudioService : Service() {
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)
    private var captureJob: Job? = null
    private var sessionJob: Job? = null
    private var bridge: BridgeClient? = null
    private var controlClient: AgentControlClient? = null
    /** Agent `send_sms`; answers on the control connection. */
    private var smsSender: SmsSender? = null
    private var injector: UplinkInjector? = null
    private var audioGuard: LocalAudioGuard? = null
    /**
     * AudioTrack teardown can block inside AudioFlinger even after stop/interrupt.
     * Keep it off the service main thread and serialize it with the next start.
     */
    private val injectorStopExecutor = Executors.newSingleThreadExecutor { runnable ->
        Thread(runnable, "telephony-injector-stop").apply { isDaemon = true }
    }
    @Volatile private var injectorStopFuture: Future<*>? = null
    /** Agent callback ids from `place_call` (cleared on session end). */
    private var pendingOutboundCallId: String? = null
    private var pendingOutboundNumber: String? = null
    private var pendingOutboundTaskId: String? = null
    /** The `place_call` reached DIALING/OFFHOOK; one that never does is dropped after 30 s. */
    @Volatile private var pendingOutboundDialed = false
    private var pendingOutboundExpiry: Runnable? = null
    private var telephonyManager: TelephonyManager? = null
    private var telephonyCallback: TelephonyCallback? = null
    @Suppress("DEPRECATION")
    private var legacyListener: PhoneStateListener? = null
    @Volatile private var callActive = false
    /** Invalidates every callback/job owned by the previous media session. */
    private val sessionGeneration = AtomicLong(0)
    /** Prevents a delayed async restore from stopping a newly re-armed service. */
    private val stopRequestSequence = AtomicLong(0)
    private val mainHandler = Handler(Looper.getMainLooper())
    private var pendingServiceStopToken = NO_PENDING_SERVICE_STOP
    private var serviceStopWatchdog: Runnable? = null
    /** Makes generation validation atomic with ownership/global session mutations. */
    private val sessionOwnershipLock = Any()
    /** Telecom call instance represented by the media bridge, or zero when unknown. */
    private val mediaCallInstance = AtomicLong(NO_CALL_INSTANCE)
    private var uplinkMissingNotified = false
    /** Phone-local menu/wait inject — drop agent PCM so it does not double-play. */
    @Volatile private var localMenuPlaying = false
    @Volatile private var localWaitPlaying = false
    /** Only one "please wait" clip per call (prevents double play). */
    @Volatile private var localWaitPlayedThisCall = false
    /** Drop agent binary frames while the local wait clip owns the uplink. */
    @Volatile private var dropAgentPcm = false
    private var localMenuJob: Job? = null
    private var localWaitJob: Job? = null
    private var answerJob: Job? = null
    private var callStateWatchdogJob: Job? = null
    @Volatile private var answerScheduled = false
    /** Ringing call waiting for the agent's `answer_plan`. */
    @Volatile private var pendingAnswerPlan: CompletableDeferred<AnswerPlan>? = null
    /** The agent did not come up in time: this ringing call is left ringing, not answered. */
    @Volatile private var ringGivenUp = false
    /** The agent's `answer_plan` said `reject` for the call that is ringing. */
    @Volatile private var answerRejected = false
    /** OFFHOOK left without AI: a call dialed by hand, or its direction stayed unknown. */
    @Volatile private var offhookWithoutAi = false
    private var offhookRecheckJob: Job? = null
    /** Started when the media connection drops; ends the call if it stays down. */
    private var mediaLostJob: Job? = null
    /** Control messages that found the connection closed; sent after the next hello. */
    private val pendingControlJson = java.util.concurrent.ConcurrentLinkedQueue<org.json.JSONObject>()
    /** Last call state (ringing/active vs idle) reported to the agent in `status`. */
    @Volatile private var reportedInCall: Boolean? = null
    /**
     * Result of a switch the agent asked for, found at start-up because the app was
     * stopped while the root script ran; sent as `set_mode_result` once connected.
     */
    private val unreportedResult = java.util.concurrent.atomic.AtomicReference<PhoneModeManager.ApplyLog?>(null)
    private val modeListener = PhoneModeManager.Listener { mode ->
        mainHandler.post { onPhoneModeChanged(mode) }
    }

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onCreate() {
        super.onCreate()
        audioGuard = LocalAudioGuard(this)
        smsSender = SmsSender(this) { msg -> controlClient?.sendJson(msg) }
        telephonyManager = getSystemService(TELEPHONY_SERVICE) as TelephonyManager
        PhoneModeManager.addListener(modeListener)
        if (Prefs.legacyAiSwitchOff(this) && !Prefs.hasPhoneModeFile(this)) {
            Log.w(
                TAG,
                "the old 'AI answering' switch was off; this version answers with the AI " +
                    "until 'AI answers my calls' is turned off",
            )
        }
        // Finish an interrupted switch, follow the module's mode file, and pick up the
        // result of a switch whose answer was lost when the app was stopped.
        PhoneModeManager.syncFromRoot(this) { log ->
            unreportedResult.set(log)
            flushUnreportedResult()
        }
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        val action = intent?.action
        if (action != ACTION_STOP) {
            cancelPendingServiceStop("new start action=${action ?: "sticky"}")
        }
        intent?.getStringExtra(EXTRA_CALLER_ID)?.trim()?.takeIf { it.isNotEmpty() }?.let {
            val instanceId = intent?.getLongExtra(EXTRA_CALL_INSTANCE_ID, NO_CALL_INSTANCE) ?: NO_CALL_INSTANCE
            if (!acceptsCallerIdFor(instanceId)) {
                // A second (waiting) call's number must not replace the number of the call that is up.
                Log.i(TAG, "EXTRA_CALLER_ID ignored: not the call in progress (instance=$instanceId)")
                return@let
            }
            Prefs.setLastCallerId(this, it)
            Log.i(TAG, "EXTRA_CALLER_ID stashed len=${it.length}")
            // InCall/screening EXTRA can arrive after media hello already went out empty.
            forwardCallerIdIfMediaLive(it)
        }
        when (action) {
            ACTION_STOP -> {
                disarmAndStop(startId)
                return START_NOT_STICKY
            }
            ACTION_SMOKE_CONNECT -> {
                armWatcher()
                scope.launch {
                    startCallSession(smoke = true)
                    Log.i(TAG, "SMOKE_CONNECT: bridge session forced (no cellular call)")
                }
            }
            ACTION_CALL_ACTIVE -> {
                armWatcher()
                Log.i(TAG, "explicit Telecom ACTIVE — start/recover media now")
                handleExplicitCallActive(intent.getLongExtra(EXTRA_CALL_INSTANCE_ID, NO_CALL_INSTANCE))
            }
            ACTION_CALL_ENDED -> {
                // SilentInCallService uses startForegroundService for this action. The
                // service may have been recreated, so enter foreground before teardown.
                armWatcher()
                // Never synthesize IDLE: an older call can be removed after a newer call
                // is already OFFHOOK. Reconcile against Telecom's aggregate state.
                Log.i(TAG, "explicit Telecom call removed — reconcile actual call state")
                handleExplicitCallEnded(intent.getLongExtra(EXTRA_CALL_INSTANCE_ID, NO_CALL_INSTANCE))
            }
            ACTION_CALL_WAITING_REJECTED -> {
                armWatcher()
                sendCallWaitingRejected(
                    callerId = intent?.getStringExtra(EXTRA_WAITING_CALLER_ID).orEmpty(),
                    simCountry = intent?.getStringExtra(EXTRA_SIM_COUNTRY).orEmpty(),
                    simSubId = intent?.getIntExtra(EXTRA_SIM_SUB_ID, -1)?.takeIf { SimSubscriptions.isValid(it) },
                )
            }
            ACTION_SET_ENABLED -> {
                // Older entry point (adb): on = the AI answers, off = normal phone.
                val en = intent.getBooleanExtra(EXTRA_ENABLED, true)
                armWatcher()
                val target = if (en) Prefs.PHONE_MODE_AI else Prefs.PHONE_MODE_NORMAL
                PhoneModeManager.requestSwitch(this, target, callBusy = isCallBusy()) { result ->
                    Log.i(TAG, "SET_ENABLED=$en → ok=${result.ok} mode=${result.mode} error=${result.error}")
                }
            }
            else -> armWatcher()
        }
        return START_STICKY
    }

    /**
     * Foreground with the types the mode needs: microphone (and phone call on 14+) in
     * AI mode, phone call only in normal mode (the app holds no phone role then, and a
     * microphone service started in the background can be refused). A refusal must
     * not crash the app: it falls back to the plain call, and failing that only logs,
     * so the control connection still comes up.
     */
    private fun enterForeground() {
        val notification = buildNotification(getString(R.string.notification_text_armed))
        val type = when {
            Build.VERSION.SDK_INT < Build.VERSION_CODES.Q -> null
            !Prefs.isAiCallEnabled(this) -> ServiceInfo.FOREGROUND_SERVICE_TYPE_PHONE_CALL
            Build.VERSION.SDK_INT >= Build.VERSION_CODES.UPSIDE_DOWN_CAKE ->
                ServiceInfo.FOREGROUND_SERVICE_TYPE_MICROPHONE or ServiceInfo.FOREGROUND_SERVICE_TYPE_PHONE_CALL
            else -> ServiceInfo.FOREGROUND_SERVICE_TYPE_MICROPHONE
        }
        try {
            if (type != null) startForeground(NOTIF_ID, notification, type) else startForeground(NOTIF_ID, notification)
            return
        } catch (e: Exception) {
            Log.e(TAG, "startForeground type=$type refused", e)
        }
        try {
            startForeground(NOTIF_ID, notification)
        } catch (e: Exception) {
            Log.e(TAG, "startForeground refused; the service keeps going without it", e)
        }
    }

    private fun armWatcher() {
        enterForeground()
        if (!Prefs.isAiCallEnabled(this)) {
            // Normal phone mode: keep the agent connection (status, remote switch) and
            // the call-state callback (in_call for the agent); touch nothing else.
            try {
                (getSystemService(AUDIO_SERVICE) as android.media.AudioManager).isMicrophoneMute = false
            } catch (e: Exception) {
                Log.w(TAG, "microphone unmute: ${e.message}")
            }
            registerPhoneCallbacks()
            startControlClient()
            Log.i(TAG, "normal phone mode: service connected, calls left alone")
            return
        }
        audioGuard?.applyHardMute()
        DialerRoleHelper.ensureRoles(this)
        registerPhoneCallbacks()
        // After crash restart, TelephonyCallback may never re-deliver the state edge
        // (RINGING/OFFHOOK already sticky). Poll and fire handleCallState now so
        // ACTIVE→media hello does not sit ~8s waiting for a phantom OFFHOOK edge.
        catchUpCallState(reason = "armWatcher")
        startCallStateWatchdog()
        startControlClient()
        Log.i(
            TAG,
            "armed silent-local watcher defaultDialer=${DialerRoleHelper.isDefaultDialer(this)} " +
                "muteHoldMs=${LocalAudioGuard.HOLD_INTERVAL_MS}",
        )
    }

    private fun registerPhoneCallbacks() {
        val tm = telephonyManager ?: return
        var freshlyRegistered = false
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
            if (telephonyCallback == null) {
                val cb = object : TelephonyCallback(), TelephonyCallback.CallStateListener {
                    override fun onCallStateChanged(state: Int) = handleCallState(state)
                }
                telephonyCallback = cb
                try {
                    tm.registerTelephonyCallback(mainExecutor, cb)
                    freshlyRegistered = true
                } catch (t: Throwable) {
                    telephonyCallback = null
                    Log.e(TAG, "registerTelephonyCallback failed; watchdog remains active", t)
                }
            }
        } else {
            if (legacyListener == null) {
                @Suppress("DEPRECATION")
                val listener = object : PhoneStateListener() {
                    @Deprecated("Deprecated in Java")
                    override fun onCallStateChanged(state: Int, phoneNumber: String?) {
                        if (!phoneNumber.isNullOrBlank()) {
                            Prefs.setLastCallerId(this@CallAudioService, phoneNumber)
                        }
                        handleCallState(state)
                    }
                }
                legacyListener = listener
                @Suppress("DEPRECATION")
                tm.listen(listener, PhoneStateListener.LISTEN_CALL_STATE)
                freshlyRegistered = true
            }
        }
        if (freshlyRegistered) {
            catchUpCallState(reason = "register")
        }
    }

    /**
     * Sticky call-state recovery: if register/arm happens while already RINGING or
     * OFFHOOK (post-crash, late START intent), fire [handleCallState] without waiting
     * for a new edge. IDLE tears down only when local state still looks active.
     * Double-fire is guarded in handleCallState (answerScheduled / callActive).
     */
    private fun catchUpCallState(reason: String) {
        val tm = telephonyManager ?: return
        val state = try {
            @Suppress("DEPRECATION")
            tm.callState
        } catch (e: Exception) {
            Log.w(TAG, "callState poll failed ($reason): ${e.message}")
            return
        }
        when (state) {
            TelephonyManager.CALL_STATE_RINGING,
            TelephonyManager.CALL_STATE_OFFHOOK,
            -> {
                Log.i(TAG, "callState catch-up ($reason): state=$state → handleCallState now")
                try {
                    handleCallState(state)
                } catch (t: Throwable) {
                    Log.e(TAG, "callState catch-up handle failed ($reason)", t)
                }
            }
            TelephonyManager.CALL_STATE_IDLE -> {
                if (callActive || answerScheduled || bridge != null || injector != null) {
                    Log.w(TAG, "callState catch-up ($reason): IDLE with stale session — tear down")
                    handleCallState(state)
                } else {
                    Log.i(TAG, "callState catch-up ($reason): IDLE")
                }
            }
            else -> Log.i(TAG, "callState catch-up ($reason): other=$state")
        }
    }

    /**
     * Telephony callbacks are edge-triggered and can be missed after process/service churn.
     * Poll quietly while armed and act only on a state change or an inconsistent session.
     */
    private fun startCallStateWatchdog() {
        if (callStateWatchdogJob?.isActive == true) return
        callStateWatchdogJob = scope.launch {
            // Seed so the first poll is not a synthetic OFFHOOK/ACTIVE edge.
            var lastState: Int? = currentTelephonyCallState()
            while (isActive) {
                val state = try {
                    @Suppress("DEPRECATION")
                    telephonyManager?.callState
                } catch (e: Exception) {
                    Log.w(TAG, "callState watchdog poll failed: ${e.message}")
                    null
                }
                if (state != null) {
                    val recoveryNeeded = when (state) {
                        // A ring the agent was not ready for is left alone (not answered).
                        TelephonyManager.CALL_STATE_RINGING -> !answerScheduled && !callActive && !ringGivenUp
                        // Do not treat a live media WS as missing just because a
                        // duplicate OFFHOOK/ACTIVE race cleared a local flag. A call
                        // left without AI (dialed by hand) is not a missing session.
                        TelephonyManager.CALL_STATE_OFFHOOK ->
                            !callActive && bridge == null && injector == null && !offhookWithoutAi
                        TelephonyManager.CALL_STATE_IDLE ->
                            callActive || answerScheduled || bridge != null || injector != null
                        else -> false
                    }
                    if (state != lastState || recoveryNeeded) {
                        Log.i(
                            TAG,
                            "callState watchdog state=$state previous=$lastState recovery=$recoveryNeeded",
                        )
                        handleCallState(state)
                        lastState = state
                    }
                }
                delay(CALL_STATE_WATCHDOG_MS)
            }
        }
    }

    private fun unregisterPhoneCallbacks() {
        val tm = telephonyManager ?: return
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
            telephonyCallback?.let { tm.unregisterTelephonyCallback(it) }
            telephonyCallback = null
        } else {
            @Suppress("DEPRECATION")
            legacyListener?.let { tm.listen(it, PhoneStateListener.LISTEN_NONE) }
            legacyListener = null
        }
    }

    /** ACTIVE is an asynchronous service intent, never evidence that OFFHOOK still holds. */
    private fun handleExplicitCallActive(instanceId: Long) {
        val live = SilentInCallService.isLiveCallInstance(instanceId)
        val selected = SilentInCallService.currentActiveCallInstanceId()
        val state = currentTelephonyCallState()
        if (
            instanceId == NO_CALL_INSTANCE || live != true || selected != instanceId ||
            state != TelephonyManager.CALL_STATE_OFFHOOK
        ) {
            Log.w(TAG, "ignore stale ACTIVE instance=$instanceId live=$live selected=$selected state=$state")
            catchUpCallState(reason = "explicit-call-active-stale")
            return
        }
        if (SilentInCallService.isOwnCallInstance(instanceId)) {
            Log.i(TAG, "ignore ACTIVE instance=$instanceId: a call dialed by hand on the phone")
            return
        }
        val current = mediaCallInstance.get()
        if (keepMediaIfSamePstnCall("explicit ACTIVE", instanceId)) {
            return
        }
        if (callActive || bridge != null || injector != null) {
            Log.i(TAG, "explicit ACTIVE switching media instance=$current -> $instanceId")
            endCallSession()
        }
        offhookWithoutAi = false
        startCallSession(smoke = false, callInstanceId = instanceId)
    }

    /** An old call's removal must not tear down the media session selected for a newer call. */
    private fun handleExplicitCallEnded(instanceId: Long) {
        val current = mediaCallInstance.get()
        if (instanceId != NO_CALL_INSTANCE && current != NO_CALL_INSTANCE && current != instanceId) {
            Log.i(TAG, "ignore stale ENDED instance=$instanceId current=$current")
            return
        }
        if (instanceId != NO_CALL_INSTANCE && current == NO_CALL_INSTANCE) {
            val selected = SilentInCallService.currentActiveCallInstanceId()
            if (selected == NO_CALL_INSTANCE || selected == instanceId) {
                Log.i(TAG, "explicit ENDED ending unbound media session instance=$instanceId")
                endCallSession()
            } else {
                Log.i(TAG, "explicit ENDED unbound keep; bind selected=$selected")
                mediaCallInstance.set(selected)
            }
            catchUpCallState(reason = "explicit-call-ended")
            return
        }
        if (instanceId != NO_CALL_INSTANCE && current == instanceId) {
            Log.i(TAG, "explicit ENDED ending matching media instance=$instanceId")
            endCallSession()
        }
        // Reconcile only against the aggregate Telecom state. A replacement call will send
        // its own ACTIVE intent with a fresh instance ID.
        catchUpCallState(reason = "explicit-call-ended")
    }

    private fun currentTelephonyCallState(): Int? = try {
        @Suppress("DEPRECATION")
        telephonyManager?.callState
    } catch (e: Exception) {
        Log.w(TAG, "callState poll failed: ${e.message}")
        null
    }

    private fun currentCallInstanceForTelephony(): Long {
        val fromInCall = SilentInCallService.currentActiveCallInstanceId()
        return if (fromInCall != NO_CALL_INSTANCE) fromInCall else mediaCallInstance.get()
    }

    /**
     * Duplicate ACTIVE / OFFHOOK / watchdog for the same PSTN call must keep the
     * existing media BridgeClient. Instance 0 means "not yet bound" and is the
     * same call once InCall publishes a real id. Distinct live instance ids are
     * a rapid re-call and still replace the session.
     */
    private fun isSamePstnCall(current: Long, incoming: Long): Boolean {
        if (current == incoming) return true
        if (current == NO_CALL_INSTANCE || incoming == NO_CALL_INSTANCE) return true
        return false
    }

    private fun keepMediaIfSamePstnCall(reason: String, incomingInstanceId: Long): Boolean {
        val current = mediaCallInstance.get()
        val hasSession = synchronized(sessionOwnershipLock) {
            callActive || bridge != null || injector != null
        }
        if (!hasSession) return false
        if (!isSamePstnCall(current, incomingInstanceId)) return false
        if (incomingInstanceId != NO_CALL_INSTANCE && current != incomingInstanceId) {
            mediaCallInstance.set(incomingInstanceId)
            Log.i(
                TAG,
                "$reason keep media WS (session-dedup) bind instance=$current -> $incomingInstanceId",
            )
        } else {
            Log.i(TAG, "$reason keep media WS (session-dedup) instance=$current")
        }
        return true
    }

    private fun isSessionCurrent(generation: Long, callInstanceId: Long): Boolean =
        synchronized(sessionOwnershipLock) {
            callActive && sessionGeneration.get() == generation &&
                isSamePstnCall(mediaCallInstance.get(), callInstanceId)
        }

    private fun mutateCurrentSession(
        generation: Long,
        callInstanceId: Long,
        mutation: () -> Unit,
    ): Boolean = synchronized(sessionOwnershipLock) {
        if (
            !callActive || sessionGeneration.get() != generation ||
            !isSamePstnCall(mediaCallInstance.get(), callInstanceId)
        ) {
            false
        } else {
            mutation()
            true
        }
    }

    private fun handleCallState(state: Int) {
        reportCallState(state != TelephonyManager.CALL_STATE_IDLE)
        if (!Prefs.isAiCallEnabled(this)) {
            Log.i(TAG, "normal phone mode; leave call state=$state alone")
            return
        }
        when (state) {
            TelephonyManager.CALL_STATE_RINGING -> {
                if (answerScheduled) {
                    Log.i(TAG, "RINGING ignored (answer already scheduled)")
                    return
                }
                // The agent was not ready for this call: it keeps ringing (carrier missed
                // call / voicemail) and is not scheduled again.
                if (ringGivenUp) return
                val established = SilentInCallService.hasEstablishedCall()
                if (established == true) {
                    // Call waiting (SilentInCallService rejects the second call) or a
                    // ring next to a call dialed by hand: leave the call that is up alone.
                    Log.i(TAG, "RINGING ignored as call waiting (established call remains)")
                    return
                }
                if (callActive) {
                    if (established == null) {
                        Log.w(TAG, "RINGING while local session active; InCall state unavailable — keep current call")
                        return
                    }
                    // Keep only the ringing call's own number (never the stale session's).
                    val caller = SilentInCallService.ringingCallInfo()?.callerId.orEmpty()
                    Log.w(TAG, "RINGING with stale local session — tear down before answer")
                    endCallSession()
                    if (caller.isNotBlank()) Prefs.setLastCallerId(this, caller)
                }
                answerScheduled = true
                answerRejected = false
                offhookWithoutAi = false
                Log.i(TAG, "RINGING — wait agent CONTROL_IDLE (max ${AGENT_READY_TIMEOUT_MS}ms) then answer")
                audioGuard?.applyHardMute()
                audioGuard?.startHoldLoop()
                startControlClient()
                answerJob?.cancel()
                answerJob = scope.launch {
                    val ringAt = SystemClock.elapsedRealtime()
                    // Do not create an AudioTrack during RINGING. Android 16's
                    // AudioFlinger can block createTrack while Telecom is changing
                    // call mode, then abort audioserver and leave the call silent.
                    Log.i(TAG, "defer telephony AudioTrack until MODE_IN_CALL")
                    // Wait until the control path to the agent is live (or timeout).
                    val agentReady = waitForAgentControlReady(AGENT_READY_TIMEOUT_MS)
                    if (callActive) return@launch
                    if (!agentReady) {
                        // Answering now would give the caller dead air: leave the call
                        // ringing so the carrier's missed call / voicemail takes it.
                        ringGivenUp = true
                        answerScheduled = false
                        Log.w(
                            TAG,
                            "agent not ready within ${AGENT_READY_TIMEOUT_MS}ms — not answering; " +
                                "the call keeps ringing (missed call / voicemail)",
                        )
                        return@launch
                    }
                    // This call's own number and SIM, once SilentInCallService stored them.
                    val details = awaitRingingCallDetails(ringAt + CALL_INFO_WAIT_MS)
                    // Ring about once before answering, like a person reaching for the
                    // phone; the agent prepares the AI's opening line meanwhile.
                    val plan = requestAnswerPlan(details)
                    if (plan.reject || answerRejected) {
                        rejectRingingCall()
                        return@launch
                    }
                    val waitMs = plan.delayMs - (SystemClock.elapsedRealtime() - ringAt)
                    if (waitMs > 0) {
                        Log.i(TAG, "ring ${plan.delayMs}ms before answer — wait ${waitMs}ms more")
                        delay(waitMs)
                    }
                    if (callActive) return@launch
                    if (answerRejected) {
                        rejectRingingCall()
                        return@launch
                    }
                    answerSilently()
                    Log.i(
                        TAG,
                        "acceptRingingCall after agentReady=$agentReady rang_ms=${SystemClock.elapsedRealtime() - ringAt}",
                    )
                }
            }
            TelephonyManager.CALL_STATE_OFFHOOK -> {
                val answeredRing = answerScheduled
                answerScheduled = false
                ringGivenUp = false
                answerRejected = false
                answerJob?.cancel()
                answerJob = null
                val callInstanceId = currentCallInstanceForTelephony()
                // Catch-up + callback can both fire OFFHOOK. Duplicate OFFHOOK/ACTIVE
                // for the same PSTN call (instance still 0, then InCall publishes the
                // real id) must keep the existing BridgeClient. A distinct live
                // Telecom call must never inherit the prior call's bridge or injector.
                if (keepMediaIfSamePstnCall("OFFHOOK", callInstanceId)) {
                    Log.i(TAG, "OFFHOOK ignored (same media session)")
                    return
                }
                if (callActive) {
                    Log.i(TAG, "OFFHOOK switching media instance=${mediaCallInstance.get()} -> $callInstanceId")
                    endCallSession()
                }
                // A call dialed by hand on this phone is the user's own call:
                // never attach the AI (it would greet into the ringback tone) and
                // give the owner the microphone and earpiece back.
                val kind = classifyOffhook(
                    agentCallback = pendingOutboundCallId != null,
                    answeredRing = answeredRing,
                    ownCall = SilentInCallService.isOwnCallUp(),
                )
                when (kind) {
                    OffhookKind.OWN -> onOwnCallOffhook()
                    OffhookKind.UNKNOWN -> recheckOffhookDirection(answeredRing)
                    OffhookKind.AI -> startOffhookSession(callInstanceId)
                }
            }
            TelephonyManager.CALL_STATE_IDLE -> {
                Log.i(TAG, "IDLE — end call session")
                if (answerScheduled && !callActive) sendIncomingCancel()
                answerScheduled = false
                ringGivenUp = false
                answerRejected = false
                offhookWithoutAi = false
                offhookRecheckJob?.cancel()
                offhookRecheckJob = null
                answerJob?.cancel()
                answerJob = null
                audioGuard?.stopHoldLoop()
                endCallSession()
                // No call left: nothing stored may reach the next call (a hidden number
                // must not pick up this call's number when the InCall data is late).
                Prefs.clearLastCallInfo(this)
                // Stay hard-muted while armed (idle watcher); do not restore yet.
                audioGuard?.applyHardMute()
            }
        }
    }

    /** OFFHOOK of an incoming call or of the agent's `place_call`: start the AI session. */
    private fun startOffhookSession(callInstanceId: Long) {
        offhookWithoutAi = false
        val outboundId = pendingOutboundCallId
        if (outboundId != null) {
            pendingOutboundDialed = true
            Log.i(
                TAG,
                "onOutboundReady call_id=$outboundId number_len=${pendingOutboundNumber?.length ?: 0}",
            )
            controlClient?.sendJson(
                org.json.JSONObject()
                    .put("type", "outbound_ready")
                    .put("call_id", outboundId)
                    .put("task_id", pendingOutboundTaskId.orEmpty())
                    .put("number", pendingOutboundNumber.orEmpty()),
            )
        }
        Log.i(TAG, "OFFHOOK — start silent stream session outbound=${outboundId != null}")
        audioGuard?.applyHardMute()
        audioGuard?.startHoldLoop()
        startCallSession(smoke = false, callInstanceId = callInstanceId)
    }

    /** A call dialed by hand on the phone: no AI, Telecom and local mutes taken back. */
    private fun onOwnCallOffhook() {
        offhookWithoutAi = true
        Log.i(TAG, "OFFHOOK outgoing manual call — AI session not started; microphone and earpiece unmuted")
        audioGuard?.stopHoldLoop()
        SilentInCallService.unmuteOwnCall()
        audioGuard?.restoreForOwnCall()
    }

    /**
     * InCallService does not know the call yet (not bound, or onCallAdded not run): ask
     * again a few times off the main thread (onCallAdded runs there). Still unknown → the
     * AI is not attached; an explicit ACTIVE from SilentInCallService can still start it.
     */
    private fun recheckOffhookDirection(answeredRing: Boolean) {
        offhookWithoutAi = true
        Log.i(TAG, "OFFHOOK direction unknown (InCallService not bound) — check again")
        offhookRecheckJob?.cancel()
        offhookRecheckJob = scope.launch {
            var ownCall: Boolean? = null
            for (attempt in 1..OUTGOING_CHECK_ATTEMPTS) {
                delay(OUTGOING_CHECK_RETRY_MS)
                ownCall = SilentInCallService.isOwnCallUp()
                if (ownCall != null) break
            }
            val decided = ownCall
            mainHandler.post { resolveOffhookDirection(decided, answeredRing) }
        }
    }

    private fun resolveOffhookDirection(ownCall: Boolean?, answeredRing: Boolean) {
        if (currentTelephonyCallState() != TelephonyManager.CALL_STATE_OFFHOOK) return
        val hasSession = synchronized(sessionOwnershipLock) {
            callActive || bridge != null || injector != null
        }
        if (hasSession) return
        when (classifyOffhook(pendingOutboundCallId != null, answeredRing, ownCall)) {
            OffhookKind.AI -> startOffhookSession(currentCallInstanceForTelephony())
            OffhookKind.OWN -> onOwnCallOffhook()
            OffhookKind.UNKNOWN -> Log.w(
                TAG,
                "OFFHOOK direction still unknown after ${OUTGOING_CHECK_ATTEMPTS * OUTGOING_CHECK_RETRY_MS}ms — " +
                    "AI not attached",
            )
        }
    }

    private fun answerSilently() {
        try {
            val telecom = getSystemService(TELECOM_SERVICE) as TelecomManager
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
                @Suppress("DEPRECATION")
                telecom.acceptRingingCall()
                Log.i(TAG, "acceptRingingCall issued")
                return
            }
        } catch (e: SecurityException) {
            Log.w(TAG, "acceptRingingCall denied: ${e.message}")
        } catch (e: Exception) {
            Log.w(TAG, "acceptRingingCall failed: ${e.message}")
        }
        // Prefer InCallService.answer if we own the dialer role.
        if (SilentInCallService.answerActiveIfHeld()) {
            Log.i(TAG, "answer via SilentInCallService")
            return
        }
        try {
            Runtime.getRuntime().exec(arrayOf("su", "-c", "input keyevent 5"))
        } catch (_: Exception) {
            Log.w(TAG, "root answer fallback unavailable")
        }
    }

    /**
     * `incoming` → agent `answer_plan`: how long to ring (the agent starts the AI
     * session meanwhile). No reply within [ANSWER_PLAN_WAIT_MS] (older agent) → default.
     */
    private suspend fun requestAnswerPlan(details: AgentCallDetails): AnswerPlan {
        val plan = CompletableDeferred<AnswerPlan>()
        pendingAnswerPlan = plan
        val incoming = org.json.JSONObject()
            .put("type", "incoming")
            .put("serial", deviceSerial())
            .put("caller_id", details.callerId)
            .put("sim_country", details.simCountry)
            .put("sample_rate", BuildConfig.SAMPLE_RATE)
        details.simSubId?.let { incoming.put("sim_sub_id", it) }
        controlClient?.sendJson(incoming)
        Log.i(
            TAG,
            "incoming sent caller_len=${details.callerId.length} " +
                "sim_country=${details.simCountry.ifEmpty { "?" }} sim_sub_id=${details.simSubId ?: "?"}",
        )
        val answer = withTimeoutOrNull(ANSWER_PLAN_WAIT_MS) { plan.await() }
        pendingAnswerPlan = null
        if (answer == null) {
            Log.w(TAG, "no answer_plan within ${ANSWER_PLAN_WAIT_MS}ms — ring ${DEFAULT_ANSWER_RING_MS}ms")
        }
        val result = answer ?: AnswerPlan(DEFAULT_ANSWER_RING_MS, reject = false)
        return result.copy(delayMs = result.delayMs.coerceIn(0L, MAX_ANSWER_RING_MS))
    }

    /** Agent `answer_plan` `reject`: decline the ringing call; IDLE then sends `incoming_cancel`. */
    private fun rejectRingingCall() {
        var ok = SilentInCallService.rejectRinging()
        if (!ok) {
            try {
                val telecom = getSystemService(TELECOM_SERVICE) as TelecomManager
                @Suppress("DEPRECATION")
                ok = telecom.endCall()
            } catch (e: Exception) {
                Log.w(TAG, "endCall for reject failed: ${e.message}")
            }
        }
        Log.i(TAG, "answer_plan reject: ringing call declined ok=$ok (not answered)")
    }

    /**
     * Number and SIM of the call that rings, from SilentInCallService (waits until
     * [deadline] for its onCallAdded). Without it: the stored values, which a new call
     * cleared first, so they never belong to the previous call.
     */
    private suspend fun awaitRingingCallDetails(deadline: Long): AgentCallDetails {
        while (true) {
            SilentInCallService.ringingCallInfo()?.let { info ->
                return AgentCallDetails(info.callerId, info.simCountry, info.simSubId)
            }
            if (SystemClock.elapsedRealtime() >= deadline) break
            delay(CALL_INFO_POLL_MS)
        }
        Log.w(TAG, "ringing call's number/SIM not known within ${CALL_INFO_WAIT_MS}ms — using the stored values")
        return storedCallDetails()
    }

    /** Number and SIM of call [instanceId] (0 = the selected call); stored values without InCallService. */
    private fun callDetailsFor(instanceId: Long): AgentCallDetails {
        val info = SilentInCallService.callInfo(instanceId) ?: return storedCallDetails()
        return AgentCallDetails(info.callerId, info.simCountry, info.simSubId)
    }

    private fun storedCallDetails(): AgentCallDetails =
        AgentCallDetails(Prefs.lastCallerId(this), Prefs.lastSimCountry(this), Prefs.lastSimSubId(this))

    /**
     * A number from SilentInCallService or screening must belong to the call that is up,
     * not to a second (waiting) call nor to a call that already ended: with an instance it
     * must be a live call and the selected one; without one (screening) no call may be
     * established.
     */
    private fun acceptsCallerIdFor(instanceId: Long): Boolean {
        if (instanceId != NO_CALL_INSTANCE) {
            if (SilentInCallService.isLiveCallInstance(instanceId) != true) return false
            val selected = SilentInCallService.currentActiveCallInstanceId()
            return selected == NO_CALL_INSTANCE || selected == instanceId
        }
        return SilentInCallService.hasEstablishedCall() != true
    }

    /** SilentInCallService rejected a second call during a call: tell the agent. */
    private fun sendCallWaitingRejected(callerId: String, simCountry: String, simSubId: Int?) {
        val msg = org.json.JSONObject()
            .put("type", "call_waiting_rejected")
            .put("serial", deviceSerial())
            .put("caller_id", callerId)
            .put("sim_country", simCountry)
        simSubId?.let { msg.put("sim_sub_id", it) }
        sendControlOrQueue(msg)
        Log.i(TAG, "call_waiting_rejected caller_len=${callerId.length} sim_sub_id=${simSubId ?: "?"}")
    }

    /** Send on the control connection now, or after its next hello when it is closed. */
    private fun sendControlOrQueue(msg: org.json.JSONObject) {
        val client = controlClient
        if (client != null && client.isOpen()) {
            client.sendJson(msg)
            return
        }
        while (pendingControlJson.size >= MAX_PENDING_CONTROL_MESSAGES) pendingControlJson.poll()
        pendingControlJson.offer(msg)
        Log.i(TAG, "control connection closed — ${msg.optString("type")} queued until it is back")
    }

    private fun flushPendingControl() {
        val client = controlClient ?: return
        if (!client.isOpen()) return
        while (true) {
            val msg = pendingControlJson.poll() ?: break
            client.sendJson(msg)
            Log.i(TAG, "queued ${msg.optString("type")} sent")
        }
    }

    /** The call ended while it rang (never answered): the agent drops the AI session it warmed up. */
    private fun sendIncomingCancel() {
        controlClient?.sendJson(
            org.json.JSONObject()
                .put("type", "incoming_cancel")
                .put("serial", deviceSerial()),
        )
        Log.i(TAG, "incoming_cancel sent (ended before answer)")
    }

    /**
     * Poll the CONTROL_IDLE WebSocket until it is open (agent reachable) or timeout.
     * Does not open media/hello — so the agent will not start talking while still ringing.
     */
    private suspend fun waitForAgentControlReady(timeoutMs: Long): Boolean {
        val deadline = System.currentTimeMillis() + timeoutMs
        startControlClient()
        var attemptedReconnect = false
        while (System.currentTimeMillis() < deadline) {
            if (callActive) return false
            val c = controlClient
            if (c != null && c.isOpen()) {
                Log.i(TAG, "agent CONTROL_IDLE is open")
                return true
            }
            if (!attemptedReconnect && System.currentTimeMillis() > deadline - timeoutMs + 1500) {
                attemptedReconnect = true
                Log.i(TAG, "CONTROL_IDLE not open — force reconnect")
                stopControlClient()
                startControlClient()
            }
            delay(150)
        }
        return controlClient?.isOpen() == true
    }

    private fun startCallSession(
        smoke: Boolean,
        callInstanceId: Long = currentCallInstanceForTelephony(),
    ) {
        // Same PSTN call (duplicate ACTIVE/OFFHOOK/watchdog, or 0→real instance
        // bind): keep the live BridgeClient. Do not endCallSession+hello again.
        if (!smoke) {
            val current = mediaCallInstance.get()
            val hasSameSession = synchronized(sessionOwnershipLock) {
                (callActive || bridge != null || injector != null) &&
                    isSamePstnCall(current, callInstanceId)
            }
            if (hasSameSession) {
                if (callInstanceId != NO_CALL_INSTANCE && current != callInstanceId) {
                    mediaCallInstance.set(callInstanceId)
                }
                Log.i(
                    TAG,
                    "startCallSession: keep existing media WS (session-dedup) instance=$current",
                )
                return
            }
        }
        // Rapid re-call: always tear down prior session first (race was leaving
        // callActive=true / dead injector → bridge connects but remote hears silence).
        val hasPreviousSession = synchronized(sessionOwnershipLock) {
            callActive || bridge != null || injector != null
        }
        if (hasPreviousSession) {
            Log.w(TAG, "startCallSession: forcing end of previous session (rapid re-call)")
            endCallSession()
        }
        // endCallSession invalidates ownership immediately, but native release may
        // still be inside a blocking write. Wait on IO before opening a replacement track.
        val previousInjectorStop = synchronized(sessionOwnershipLock) { injectorStopFuture }
        val generation = synchronized(sessionOwnershipLock) {
            val next = sessionGeneration.incrementAndGet()
            mediaCallInstance.set(callInstanceId)
            callActive = true
            localWaitPlayedThisCall = false
            dropAgentPcm = false
            next
        }
        updateNotification(getString(R.string.notification_text_in_call))
        val newSessionJob = scope.launch(start = CoroutineStart.LAZY) {
            previousInjectorStop?.let { stopFuture ->
                try {
                    stopFuture.get()
                    Log.i(TAG, "previous injector teardown complete before new session")
                } catch (e: Exception) {
                    Log.w(TAG, "previous injector teardown wait failed: ${e.message}")
                }
            }
            // Ring already pre-warmed inject; short settle then open path for ~1s-to-audio.
            if (!smoke) {
                delay(OFFHOOK_SETTLE_MS)
            }
            if (!isSessionCurrent(generation, callInstanceId)) return@launch

            audioGuard?.applyHardMute()
            audioGuard?.startHoldLoop()

            val serial = deviceSerial()
            val details = callDetailsFor(callInstanceId)
            val dumpRoot = createDeviceProtectedStorageContext().cacheDir

            // Parallel: TYPE_TELEPHONY inject + media WS hello (was serial → +0.5–2s).
            val inj = UplinkInjectorFactory.create(
                this@CallAudioService,
                onUnavailableOnce = {
                    if (isSessionCurrent(generation, callInstanceId)) notifyUplinkMissing()
                },
                requireInCallMode = !smoke,
            )
            if (!mutateCurrentSession(generation, callInstanceId) { injector = inj }) {
                inj.stop()
                return@launch
            }
            val injectJob = async {
                if (!isSessionCurrent(generation, callInstanceId)) return@async false
                // AudioManager calls inside start() can block while audioserver recovers.
                // Never hold sessionOwnershipLock across that Binder boundary.
                var startOk = inj.start()
                if (!isSessionCurrent(generation, callInstanceId)) {
                    inj.stop()
                    return@async false
                }
                audioGuard?.applyHardMute()
                if (!startOk && !smoke) {
                    for (attempt in 1..4) {
                        delay(80L * attempt)
                        if (!isSessionCurrent(generation, callInstanceId)) return@async false
                        inj.stop()
                        if (!isSessionCurrent(generation, callInstanceId)) return@async false
                        val retryStarted = inj.start()
                        if (!isSessionCurrent(generation, callInstanceId)) {
                            inj.stop()
                            return@async false
                        }
                        if (retryStarted) {
                            startOk = true
                            break
                        }
                        audioGuard?.applyHardMute()
                        Log.i(TAG, "uplink injector retry#$attempt")
                    }
                }
                startOk
            }

            val client = BridgeClient(
                url = Prefs.agentUrl(this@CallAudioService),
                onBinary = { pcm ->
                    if (!isSessionCurrent(generation, callInstanceId)) return@BridgeClient
                    // Never inject agent audio during local menu/wait (double-play root cause).
                    if (localMenuPlaying || localWaitPlaying || dropAgentPcm) return@BridgeClient
                    inj.writePcm(pcm)
                },
                onText = { msg -> handleBridgeText(msg, generation, callInstanceId, inj) },
                pcmDumpDir = File(dumpRoot, "tts_pcm"),
                onDisconnected = { reason -> onMediaConnectionLost(generation, callInstanceId, smoke, reason) },
            )
            if (!mutateCurrentSession(generation, callInstanceId) {
                    bridge = client
                    client.connect(
                        serial = serial,
                        callerId = details.callerId,
                        sampleRate = BuildConfig.SAMPLE_RATE,
                        simCountry = details.simCountry,
                        simSubId = details.simSubId,
                    )
                }
            ) {
                client.close()
                inj.stop()
                return@launch
            }
            // Hello starts while ownership is held so teardown cannot close then be
            // followed by a stale connect that resurrects the prior session.
            val startOk = injectJob.await()
            if (!isSessionCurrent(generation, callInstanceId)) {
                client.close()
                inj.stop()
                return@launch
            }
            Log.i(
                TAG,
                "uplink injector workerStarted=$startOk routeReady=${inj.isRouteReady()} " +
                    "available=${inj.isAvailable()} smoke=$smoke " +
                    "backend=${inj.javaClass.simpleName} (parallel with bridge hello)",
            )
            scope.launch {
                val ready = inj.awaitRouteReady(1_500)
                if (!isSessionCurrent(generation, callInstanceId)) return@launch
                Log.i(TAG, "uplink route watchdog ready=$ready generation=$generation")
                if (!ready) notifyUplinkMissing()
            }
            if (!inj.isAvailable() && isSessionCurrent(generation, callInstanceId)) {
                Log.e(TAG, "TYPE_TELEPHONY absent — bridge still opens; inject may stay silent")
                notifyUplinkMissing()
            }
            Log.i(TAG, "bridge connecting smoke=$smoke injectStartOk=$startOk localMenu=${BuildConfig.LOCAL_MENU_ENABLED}")

            // Optional phone-local greeting (OFF by default — the agent streams audio over the WebSocket).
            if (!smoke && BuildConfig.LOCAL_MENU_ENABLED) {
                val newLocalMenuJob = scope.launch(start = CoroutineStart.LAZY) {
                    delay(LOCAL_MENU_DELAY_MS)
                    if (!isSessionCurrent(generation, callInstanceId)) return@launch
                    val ready = inj.awaitRouteReady(2500)
                    if (!isSessionCurrent(generation, callInstanceId)) return@launch
                    Log.i(TAG, "menu routeReady=$ready — start local menu")
                    val pcm = LocalMenuPlayer.loadMenuPcm(this@CallAudioService)
                    if (!isSessionCurrent(generation, callInstanceId)) return@launch
                    if (pcm.isEmpty()) {
                        Log.e(TAG, "local menu asset empty — agent fallback")
                        client.sendJson(
                            org.json.JSONObject()
                                .put("type", "local_menu_failed")
                                .put("serial", serial)
                                .put("reason", "asset_empty"),
                        )
                        return@launch
                    }
                    if (!isSessionCurrent(generation, callInstanceId)) return@launch
                    if (!mutateCurrentSession(generation, callInstanceId) {
                            localMenuPlaying = true
                        }
                    ) return@launch
                    client.sendJson(
                        org.json.JSONObject()
                            .put("type", "local_menu_started")
                            .put("serial", serial)
                            .put("pcm_bytes", pcm.size)
                            .put("hour", LocalMenuPlayer.taipeiHour()),
                    )
                    try {
                        LocalMenuPlayer.playInto(
                            inj,
                            pcm,
                            { !isSessionCurrent(generation, callInstanceId) || !localMenuPlaying },
                            "menu",
                        )
                    } finally {
                        if (isSessionCurrent(generation, callInstanceId)) {
                            mutateCurrentSession(generation, callInstanceId) {
                                localMenuPlaying = false
                            }
                            client.sendJson(
                                org.json.JSONObject()
                                    .put("type", "local_menu_done")
                                    .put("serial", serial),
                            )
                        }
                    }
                    Log.i(TAG, "local menu done serial=$serial")
                }
                val menuInstalled = mutateCurrentSession(generation, callInstanceId) {
                    localMenuJob?.cancel()
                    localMenuJob = newLocalMenuJob
                }
                if (menuInstalled) {
                    newLocalMenuJob.start()
                } else {
                    newLocalMenuJob.cancel()
                }
            }

            val newCaptureJob = scope.launch(start = CoroutineStart.LAZY) {
                // Mic mute is intentional for local uplink; VOICE_DOWNLINK/VOICE_CALL
                // must still produce remote PCM for the agent's VAD. Do not unmute mic.
                audioGuard?.applyHardMute()
                val capture = VoiceCallCapture(BuildConfig.SAMPLE_RATE)
                try {
                    capture.open()
                    Log.i(
                        TAG,
                        "downlink capture open source=${capture.activeSourceName()} " +
                            "micMute=true (downlink must stay live)",
                    )
                    val buf = ByteArray(capture.frameBytes)
                    var sent = 0L
                    while (isActive && isSessionCurrent(generation, callInstanceId)) {
                        val n = capture.read(buf)
                        if (n > 0 && isSessionCurrent(generation, callInstanceId)) {
                            client.sendPcm(buf.copyOf(n))
                            sent++
                            // Continuous PCM after answer even if near-silent (the agent has a timeout fallback).
                            if (sent == 1L || sent % 50L == 0L) {
                                Log.i(TAG, "downlink framesSent=$sent ${capture.statsSummary()}")
                            }
                        }
                    }
                    Log.i(TAG, "downlink capture loop end framesSent=$sent ${capture.statsSummary()}")
                } catch (e: Exception) {
                    Log.e(TAG, "capture failed: ${e.message}", e)
                } finally {
                    capture.close()
                }
            }
            val captureInstalled = mutateCurrentSession(generation, callInstanceId) {
                captureJob?.cancel()
                captureJob = newCaptureJob
            }
            if (captureInstalled) {
                newCaptureJob.start()
            } else {
                newCaptureJob.cancel()
            }
        }
        val sessionInstalled = mutateCurrentSession(generation, callInstanceId) {
            sessionJob?.cancel()
            sessionJob = newSessionJob
        }
        if (sessionInstalled) {
            newSessionJob.start()
        } else {
            newSessionJob.cancel()
        }
    }

    private fun startControlClient() {
        if (controlClient != null) return
        val agentUrl = Prefs.agentUrl(this)
        val client = AgentControlClient(
            url = agentUrl,
            onCommand = { obj -> handleControlCommand(obj) },
            describe = { hello ->
                val busy = isCallBusy()
                reportedInCall = busy
                hello.put("phone_mode", Prefs.phoneMode(this))
                hello.put("in_call", busy)
                // The agent sends the console address again after this hello (if it has one).
                Prefs.setConsoleUrl(this, null)
            },
            onOpened = {
                // The mode the phone really ended up in, and a result lost with a restart.
                sendStatus()
                flushUnreportedResult()
                flushPendingControl()
                // Text message results the agent may have missed while disconnected.
                smsSender?.replay()
            },
        )
        controlClient = client
        client.connect(deviceSerial())
        Log.i(TAG, "control client started url=${Prefs.redactUrl(agentUrl)}")
    }

    private fun stopControlClient() {
        controlClient?.close()
        controlClient = null
    }

    /** Idle control WS + in-call bridge WS both route agent JSON here for dial commands. */
    private fun handleControlCommand(obj: org.json.JSONObject) {
        when (obj.optString("type")) {
            "place_call" -> handlePlaceCall(obj)
            "blocklist_sync" -> handleBlocklistSync(obj)
            "answer_plan" -> {
                val plan = AnswerPlan(
                    delayMs = obj.optLong("delay_ms", DEFAULT_ANSWER_RING_MS),
                    reject = obj.optBoolean("reject", false),
                )
                Log.i(
                    TAG,
                    "answer_plan delay_ms=${plan.delayMs} reject=${plan.reject} prewarm=${obj.optBoolean("prewarm")}",
                )
                // A reject that arrives after the plan wait still counts while the call rings.
                if (plan.reject && answerScheduled && !callActive) answerRejected = true
                pendingAnswerPlan?.complete(plan)
            }
            "set_mode" -> handleSetMode(obj)
            "send_sms" -> smsSender?.handle(obj)
            "agent_info" -> {
                Prefs.setConsoleUrl(this, obj.optString("console_url"))
                Log.i(TAG, "agent_info console_url_set=${Prefs.consoleUrl(this) != null}")
            }
            else -> Log.i(TAG, "control cmd type=${obj.optString("type")}")
        }
    }

    /** Agent `set_mode` (web console): switch, then answer with `set_mode_result`. */
    private fun handleSetMode(obj: org.json.JSONObject) {
        // The id also names the root script's run, so a result lost with an app
        // restart can still be sent later (see flushUnreportedResult).
        val id = PhoneModeManager.safeId(obj.optString("id"))
        val mode = obj.optString("mode").trim()
        Log.i(TAG, "set_mode from agent mode=$mode id=$id")
        val runId = id.ifEmpty { PhoneModeManager.newLocalId() }
        PhoneModeManager.requestSwitch(this, mode, callBusy = isCallBusy(), id = runId) { result ->
            sendModeResult(id, result.ok, result.mode, result.error)
        }
    }

    private fun sendModeResult(id: String, ok: Boolean, mode: String, error: String) {
        if (id.isEmpty()) return
        val client = controlClient
        if (client == null || !client.isOpen()) {
            // Not connected right now: send it after the next hello.
            unreportedResult.set(PhoneModeManager.ApplyLog(id, mode, true, if (ok) "" else error.ifEmpty { "failed" }))
            return
        }
        client.sendJson(
            org.json.JSONObject()
                .put("type", "set_mode_result")
                .put("id", id)
                .put("ok", ok)
                .put("mode", mode)
                .put("error", error),
        )
        Prefs.setReportedApplyId(this, id)
    }

    /** Send the result found at start-up once the control connection is open. */
    private fun flushUnreportedResult() {
        if (controlClient?.isOpen() != true) return
        val log = unreportedResult.getAndSet(null) ?: return
        Log.i(TAG, "sending the result of switch id=${log.id} found at start-up ok=${log.ok}")
        sendModeResult(log.id, log.ok, Prefs.phoneMode(this), log.error)
    }

    /** Main thread: the phone mode was (re)applied by [PhoneModeManager]. */
    private fun onPhoneModeChanged(mode: String) {
        Log.i(TAG, "phone mode now $mode")
        if (mode == Prefs.PHONE_MODE_AI) {
            armWatcher()
        } else {
            callStateWatchdogJob?.cancel()
            callStateWatchdogJob = null
            answerJob?.cancel()
            answerJob = null
            answerScheduled = false
            audioGuard?.stopHoldLoop()
            updateNotification()
        }
        sendStatus()
    }

    /** A call rings or is active on this phone (in either mode). */
    private fun isCallBusy(): Boolean {
        if (callActive || answerScheduled) return true
        val state = currentTelephonyCallState()
        return state != null && state != TelephonyManager.CALL_STATE_IDLE
    }

    private fun reportCallState(inCall: Boolean) {
        if (reportedInCall == inCall) return
        reportedInCall = inCall
        sendStatus(inCall)
    }

    /** `status` on the control connection: the phone mode and whether a call is on. */
    private fun sendStatus(inCall: Boolean = isCallBusy()) {
        controlClient?.sendJson(
            org.json.JSONObject()
                .put("type", "status")
                .put("mode", Prefs.phoneMode(this))
                .put("in_call", inCall),
        )
    }

    /**
     * Agent `blocklist_sync`: stored but unused (blocklist reject is disabled).
     * Missing `numbers` → no-op. Screening never rejects from this cache.
     */
    private fun handleBlocklistSync(obj: org.json.JSONObject) {
        val arr = obj.optJSONArray("numbers") ?: return
        val numbers = mutableListOf<String>()
        for (i in 0 until arr.length()) {
            val n = arr.optString(i).trim()
            if (n.isNotEmpty()) numbers.add(n)
        }
        Prefs.setBlocklist(this, numbers)
        Log.i(TAG, "blocklist_sync stored unused count=${numbers.size}")
    }

    private fun handlePlaceCall(obj: org.json.JSONObject) {
        val number = obj.optString("number").trim()
        val callId = obj.optString("call_id").trim()
        val taskId = obj.optString("task_id").trim()
        fun fail(reason: String) {
            Log.w(TAG, "place_call failed reason=$reason call_id=$callId")
            controlClient?.sendJson(
                org.json.JSONObject()
                    .put("type", "place_call_failed")
                    .put("call_id", callId)
                    .put("task_id", taskId)
                    .put("number", number)
                    .put("reason", reason),
            )
        }
        if (!Prefs.isAiCallEnabled(this)) {
            fail("ai_call_disabled")
            return
        }
        if (number.isEmpty() || callId.isEmpty()) {
            fail("missing number or call_id")
            return
        }
        // One call at a time: a callback dialed next to a call that rings or is up would
        // be mixed up with it (media session, mute state, call_end).
        if (isCallBusy()) {
            fail("busy")
            return
        }
        val subId = SimSubscriptions.fromCommand(obj)
        Log.i(
            TAG,
            "place_call call_id=$callId task_id=$taskId number_len=${number.length} sub_id=${subId ?: "-"}",
        )
        setPendingOutbound(callId, number, taskId.ifEmpty { null })
        Prefs.setLastCallerId(this, number)
        val ok = OutboundCallHelper.placeCall(this, number, callId, subId)
        controlClient?.sendJson(
            org.json.JSONObject()
                .put("type", if (ok) "place_call_ack" else "place_call_failed")
                .put("call_id", callId)
                .put("task_id", taskId)
                .put("number", number)
                .put("reason", if (ok) "" else "dial_helper_failed"),
        )
        if (!ok) {
            clearPendingOutbound(callId)
        }
    }

    /** Remember the agent's `place_call`; dropped if it does not reach DIALING/OFFHOOK in 30 s. */
    private fun setPendingOutbound(callId: String, number: String, taskId: String?) {
        val expiry = Runnable { expirePendingOutbound(callId) }
        val old = synchronized(sessionOwnershipLock) {
            pendingOutboundCallId = callId
            pendingOutboundNumber = number
            pendingOutboundTaskId = taskId
            pendingOutboundDialed = false
            agentCallbackPending = true
            pendingOutboundExpiry.also { pendingOutboundExpiry = expiry }
        }
        old?.let { mainHandler.removeCallbacks(it) }
        mainHandler.postDelayed(expiry, OUTBOUND_DIAL_TIMEOUT_MS)
    }

    private fun clearPendingOutbound(callId: String) {
        val old = synchronized(sessionOwnershipLock) {
            if (pendingOutboundCallId != callId) return
            pendingOutboundCallId = null
            pendingOutboundNumber = null
            pendingOutboundTaskId = null
            pendingOutboundDialed = false
            agentCallbackPending = false
            pendingOutboundExpiry.also { pendingOutboundExpiry = null }
        }
        old?.let { mainHandler.removeCallbacks(it) }
    }

    /** Otherwise the next call (even one dialed by hand) would be taken for the callback. */
    private fun expirePendingOutbound(callId: String) {
        val expired = synchronized(sessionOwnershipLock) {
            if (pendingOutboundCallId != callId || pendingOutboundDialed) return@synchronized false
            pendingOutboundCallId = null
            pendingOutboundNumber = null
            pendingOutboundTaskId = null
            agentCallbackPending = false
            pendingOutboundExpiry = null
            true
        }
        if (expired) {
            Log.w(
                TAG,
                "place_call call_id=$callId did not reach DIALING/OFFHOOK within ${OUTBOUND_DIAL_TIMEOUT_MS}ms — dropped",
            )
        }
    }

    private fun handleBridgeText(
        msg: String,
        generation: Long,
        callInstanceId: Long,
        inj: UplinkInjector,
    ) {
        if (!isSessionCurrent(generation, callInstanceId)) return
        try {
            val obj = org.json.JSONObject(msg)
            when (obj.optString("type")) {
                "place_call" -> {
                    if (!isSessionCurrent(generation, callInstanceId)) return
                    handlePlaceCall(obj)
                }
                "tts_start" -> {
                    val source = obj.optString("source")
                    if (!isSessionCurrent(generation, callInstanceId)) return
                    // AI Live audio — never block.
                    if (source == "gemini_live_tts" || source.startsWith("gemini_live")) {
                        if (!mutateCurrentSession(generation, callInstanceId) {
                                dropAgentPcm = false
                                localWaitPlaying = false
                                localMenuPlaying = false
                            }
                        ) return
                        Log.i(TAG, "tts_start AI — open inject source=$source")
                        return
                    }
                    // Wait: ONLY exact gemini_wait (once per call).
                    if (source == "gemini_wait") {
                        val duplicate = synchronized(sessionOwnershipLock) {
                            isSessionCurrent(generation, callInstanceId) && localWaitPlayedThisCall
                        }
                        if (duplicate) {
                            Log.i(TAG, "skip duplicate wait tts_start (already played this call)")
                            mutateCurrentSession(generation, callInstanceId) {
                                dropAgentPcm = true // keep dropping any accidental agent wait PCM
                            }
                            return
                        }
                        startLocalWait(source, generation, callInstanceId, inj)
                        return
                    }
                    // Other TTS (hold/goodbye/fail): stop local clips, clear queue.
                    if (!mutateCurrentSession(generation, callInstanceId) {
                            dropAgentPcm = false
                            localMenuPlaying = false
                            localWaitPlaying = false
                        }
                    ) return
                    // clearPending can wait behind a blocking AudioTrack.write. Keep it
                    // outside sessionOwnershipLock so call teardown remains non-blocking.
                    inj.clearPending()
                    Log.i(TAG, "tts_start source=$source cleared uplink queue")
                }
                "tts_end" -> {
                    val source = obj.optString("source")
                    if (source == "gemini_wait") {
                        if (!isSessionCurrent(generation, callInstanceId)) return
                        if (!mutateCurrentSession(generation, callInstanceId) {
                                dropAgentPcm = false
                                localWaitPlaying = false
                            }
                        ) return
                        Log.i(TAG, "tts_end gemini_wait — resume inject")
                    }
                }
                "reply_text", "tts_text" -> {
                    Log.i(TAG, "reply_text len=${obj.optString("text").length}")
                }
                "stop_local_menu" -> {
                    if (!isSessionCurrent(generation, callInstanceId)) return
                    // No-op when LOCAL_MENU_ENABLED=false; still clear flags if stale.
                    Log.i(TAG, "stop_local_menu reason=${obj.optString("reason")} enabled=${BuildConfig.LOCAL_MENU_ENABLED}")
                    if (!mutateCurrentSession(generation, callInstanceId) {
                            localMenuPlaying = false
                        }
                    ) return
                    if (BuildConfig.LOCAL_MENU_ENABLED) inj.clearPending()
                }
                "dtmf" -> {
                    if (!isSessionCurrent(generation, callInstanceId)) return
                    // The agent confirmed a DTMF key — stop local menu inject immediately.
                    Log.i(TAG, "agent dtmf digit=${obj.optString("digit")} — stop local menu")
                    if (!mutateCurrentSession(generation, callInstanceId) {
                            localMenuPlaying = false
                        }
                    ) return
                    inj.clearPending()
                }
                "hangup" -> {
                    if (!isSessionCurrent(generation, callInstanceId)) return
                    Log.i(TAG, "agent hangup reason=${obj.optString("reason")}")
                    hangupFromAgent(generation, callInstanceId)
                }
            }
        } catch (_: Exception) {
        }
    }

    /** End the cellular call when the agent sends `{"type":"hangup"}`. */
    private fun hangupFromAgent(generation: Long, callInstanceId: Long) {
        scope.launch {
            // Small settle so last TTS frame flushes into the injector.
            delay(250)
            if (!isSessionCurrent(generation, callInstanceId)) return@launch
            var ok = SilentInCallService.disconnectActive(callInstanceId)
            if (!ok) {
                if (!isSessionCurrent(generation, callInstanceId)) return@launch
                try {
                    val telecom = getSystemService(TELECOM_SERVICE) as TelecomManager
                    @Suppress("DEPRECATION")
                    ok = telecom.endCall()
                    Log.i(TAG, "TelecomManager.endCall=$ok")
                } catch (e: SecurityException) {
                    Log.w(TAG, "endCall denied: ${e.message}")
                } catch (e: Exception) {
                    Log.w(TAG, "endCall failed: ${e.message}")
                }
            }
            if (!ok) {
                // Retry root ENDCALL a few times — some Pixel builds ignore a single event.
                for (i in 1..3) {
                    try {
                        Runtime.getRuntime().exec(arrayOf("su", "-c", "input keyevent 6")).waitFor()
                        Log.i(TAG, "root KEYCODE_ENDCALL attempt=$i")
                        delay(350)
                        if (!isSessionCurrent(generation, callInstanceId)) return@launch
                        if (!callActive) {
                            ok = true
                            break
                        }
                    } catch (e: Exception) {
                        Log.w(TAG, "root hangup fallback unavailable: ${e.message}")
                        break
                    }
                }
            }
            if (!ok) {
                Log.e(TAG, "hangupFromAgent: all disconnect paths failed — caller may stay on the line")
            }
        }
    }

    /**
     * The media connection closed or failed during a call. There is no reconnect for a
     * call in progress, so unless it is open again after [MEDIA_LOST_HANGUP_MS] the call
     * is ended the way an agent `hangup` ends it, instead of leaving the caller in silence.
     */
    private fun onMediaConnectionLost(generation: Long, callInstanceId: Long, smoke: Boolean, reason: String) {
        if (smoke) return
        val watch = scope.launch(start = CoroutineStart.LAZY) {
            delay(MEDIA_LOST_HANGUP_MS)
            if (!isSessionCurrent(generation, callInstanceId)) return@launch
            val client = synchronized(sessionOwnershipLock) { bridge }
            if (client != null && client.isOpen()) {
                Log.i(TAG, "media connection open again — keep the call")
                return@launch
            }
            Log.w(TAG, "media connection still down after ${MEDIA_LOST_HANGUP_MS}ms — hang up the call")
            hangupFromAgent(generation, callInstanceId)
        }
        val started = mutateCurrentSession(generation, callInstanceId) {
            if (mediaLostJob?.isActive != true) mediaLostJob = watch
        } && mediaLostJob === watch
        if (started) {
            Log.w(TAG, "media connection lost ($reason) — hang up in ${MEDIA_LOST_HANGUP_MS}ms unless it is back")
            watch.start()
        } else {
            watch.cancel()
        }
    }

    private fun notifyUplinkMissing() {
        if (uplinkMissingNotified) return
        uplinkMissingNotified = true
        val nm = getSystemService(NotificationManager::class.java)
        val channelId = "call_stream_info"
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            nm.createNotificationChannel(
                NotificationChannel(
                    channelId,
                    getString(R.string.channel_info),
                    NotificationManager.IMPORTANCE_LOW,
                ),
            )
        }
        val n = NotificationCompat.Builder(this, channelId)
            .setContentTitle(getString(R.string.uplink_missing_title))
            .setContentText(getString(R.string.uplink_missing_text))
            .setSmallIcon(android.R.drawable.ic_dialog_info)
            .setAutoCancel(true)
            .setSilent(true)
            .build()
        nm.notify(NOTIF_INFO_ID, n)
    }

    private fun endCallSession() {
        var endingInstance = NO_CALL_INSTANCE
        var oldInjector: UplinkInjector? = null
        var oldBridge: BridgeClient? = null
        var oldAnswerJob: Job? = null
        var oldLocalMenuJob: Job? = null
        var oldLocalWaitJob: Job? = null
        var oldCaptureJob: Job? = null
        var oldSessionJob: Job? = null
        var oldMediaLostJob: Job? = null
        var oldOutboundExpiry: Runnable? = null
        var outboundCallId: String? = null
        var outboundTaskId: String? = null
        var wasIdle = false
        var rangUnanswered = false
        val invalidatedGeneration = synchronized(sessionOwnershipLock) {
            val invalidated = sessionGeneration.incrementAndGet()
            endingInstance = mediaCallInstance.getAndSet(NO_CALL_INSTANCE)
            wasIdle = !callActive && bridge == null && injector == null &&
                answerJob == null && localMenuJob == null && localWaitJob == null &&
                captureJob == null && sessionJob == null && pendingOutboundCallId == null
            rangUnanswered = answerScheduled && !callActive
            callActive = false
            answerScheduled = false
            localMenuPlaying = false
            localWaitPlaying = false
            localWaitPlayedThisCall = false
            dropAgentPcm = false
            oldAnswerJob = answerJob.also { answerJob = null }
            oldLocalMenuJob = localMenuJob.also { localMenuJob = null }
            oldLocalWaitJob = localWaitJob.also { localWaitJob = null }
            oldCaptureJob = captureJob.also { captureJob = null }
            oldSessionJob = sessionJob.also { sessionJob = null }
            oldMediaLostJob = mediaLostJob.also { mediaLostJob = null }
            oldInjector = injector.also { injector = null }
            oldBridge = bridge.also { bridge = null }
            outboundCallId = pendingOutboundCallId.also { pendingOutboundCallId = null }
            pendingOutboundNumber = null
            outboundTaskId = pendingOutboundTaskId.also { pendingOutboundTaskId = null }
            pendingOutboundDialed = false
            agentCallbackPending = false
            oldOutboundExpiry = pendingOutboundExpiry.also { pendingOutboundExpiry = null }
            if (oldInjector != null) {
                // Do not call the injector's stop method on the service main thread. The
                // worker may be blocked in AudioTrack.write and native teardown can
                // exceed Android's service ANR window.
                injectorStopFuture = try {
                    injectorStopExecutor.submit {
                        try {
                            oldInjector?.stop()
                        } catch (e: Exception) {
                            Log.w(TAG, "injector stop: ${e.message}")
                        }
                    }
                } catch (e: Exception) {
                    Log.e(TAG, "schedule injector stop failed", e)
                    null
                }
            }
            invalidated
        }
        oldMediaLostJob?.cancel()
        oldOutboundExpiry?.let { mainHandler.removeCallbacks(it) }
        if (rangUnanswered) sendIncomingCancel()
        if (wasIdle) {
            Log.i(TAG, "endCallSession invalidated idle generation=$invalidatedGeneration")
            return
        }
        Log.i(TAG, "endCallSession instance=$endingInstance hasBridge=${oldBridge != null} hasInj=${oldInjector != null}")
        oldAnswerJob?.cancel()
        oldLocalMenuJob?.cancel()
        oldLocalWaitJob?.cancel()
        oldCaptureJob?.cancel()
        oldSessionJob?.cancel()
        // Injector stop is already scheduled above; keep WS teardown independent so a
        // stuck AudioTrack cannot delay call_end or block the service main thread.
        // The next session waits for this stop before creating a fresh AudioTrack.
        val serial = deviceSerial()
        val caller = currentCallerId()
        val client = oldBridge
        // Close WS off critical path but without waiting for hangup race.
        scope.launch {
            try {
                // In-call session WS (inbound + outbound audio path).
                client?.sendCallEnd(serial, caller)
                // CONTROL_IDLE path: agent callback state machine needs call_id/task_id.
                if (outboundCallId != null) {
                    controlClient?.sendJson(
                        org.json.JSONObject()
                            .put("type", "call_end")
                            .put("call_id", outboundCallId)
                            .put("task_id", outboundTaskId.orEmpty())
                            .put("serial", serial)
                            .put("reason", "normal"),
                    )
                }
                delay(80)
            } catch (_: Exception) {
            } finally {
                try {
                    client?.close()
                } catch (_: Exception) {
                }
            }
        }
        Prefs.clearLastCallerId(this)
        updateNotification(getString(R.string.notification_text_armed))
    }

    /** Push a late EXTRA CID onto the existing media WS as `ringing` (agent waits on empty hello). */
    private fun forwardCallerIdIfMediaLive(callerId: String) {
        if (callerId.isBlank()) return
        val client = synchronized(sessionOwnershipLock) { bridge }
        if (client == null) return
        client.sendRinging(deviceSerial(), callerId)
        Log.i(TAG, "late caller_id forwarded to media WS len=${callerId.length}")
    }

    /**
     * Number of the call in progress: this call's own (SilentInCallService), else the
     * stored one. Blank = hidden or unknown; never an older call's number (the call log
     * only lists a call after it ended, so it always held the previous caller).
     */
    private fun currentCallerId(): String = callDetailsFor(currentCallInstanceForTelephony()).callerId

    private fun cancelPendingServiceStop(reason: String) {
        val oldToken = pendingServiceStopToken
        pendingServiceStopToken = NO_PENDING_SERVICE_STOP
        serviceStopWatchdog?.let { mainHandler.removeCallbacks(it) }
        serviceStopWatchdog = null
        if (oldToken != NO_PENDING_SERVICE_STOP) {
            Log.i(TAG, "pending service stop invalidated token=$oldToken reason=$reason")
        }
    }

    private fun finishServiceStop(
        stopToken: Long,
        startId: Int,
        guard: LocalAudioGuard?,
        restored: Boolean,
    ) {
        if (pendingServiceStopToken != stopToken) {
            Log.i(TAG, "ignore stale service stop callback token=$stopToken")
            return
        }
        pendingServiceStopToken = NO_PENDING_SERVICE_STOP
        serviceStopWatchdog?.let { mainHandler.removeCallbacks(it) }
        serviceStopWatchdog = null
        if (restored && audioGuard === guard) audioGuard = null
        stopForeground(STOP_FOREGROUND_REMOVE)
        val stopped = stopSelfResult(startId)
        Log.i(TAG, "service stop complete token=$stopToken restored=$restored stopped=$stopped")
    }

    private fun disarmAndStop(startId: Int) {
        cancelPendingServiceStop("superseded by ACTION_STOP")
        val stopToken = stopRequestSequence.incrementAndGet()
        pendingServiceStopToken = stopToken
        endCallSession()
        callStateWatchdogJob?.cancel()
        callStateWatchdogJob = null
        stopControlClient()
        unregisterPhoneCallbacks()
        val guard = audioGuard
        if (guard == null) {
            finishServiceStop(stopToken, startId, null, restored = true)
            return
        }
        // Restore is serialized on LocalAudioGuard's worker. Stopping the service
        // first could let Android kill the process with the restore still queued.
        val watchdog = Runnable {
            if (pendingServiceStopToken == stopToken) {
                Log.e(
                    TAG,
                    "audio restore did not finish within ${SERVICE_STOP_RESTORE_TIMEOUT_MS}ms; " +
                        "stopping service without blocking main thread",
                )
                finishServiceStop(stopToken, startId, guard, restored = false)
            }
        }
        serviceStopWatchdog = watchdog
        mainHandler.postDelayed(watchdog, SERVICE_STOP_RESTORE_TIMEOUT_MS)
        guard.restore {
            finishServiceStop(stopToken, startId, guard, restored = true)
        }
    }

    override fun onDestroy() {
        PhoneModeManager.removeListener(modeListener)
        smsSender?.close()
        smsSender = null
        cancelPendingServiceStop("service destroyed")
        endCallSession()
        callStateWatchdogJob?.cancel()
        callStateWatchdogJob = null
        unregisterPhoneCallbacks()
        audioGuard?.restore()
        audioGuard = null
        scope.cancel()
        // Allow an already queued injector stop to finish, but reject future work.
        injectorStopExecutor.shutdown()
        super.onDestroy()
    }

    private fun updateNotification(text: String? = null) {
        val nm = getSystemService(NotificationManager::class.java)
        nm.notify(NOTIF_ID, buildNotification(text ?: getString(R.string.notification_text_armed)))
    }

    /** The agent's reply to `incoming`: ring [delayMs], or decline the call ([reject]). */
    private data class AnswerPlan(val delayMs: Long, val reject: Boolean)

    /** What the agent is told about a call: number ("" = hidden), SIM country and subscription. */
    private data class AgentCallDetails(val callerId: String, val simCountry: String, val simSubId: Int?)

    /** How an OFFHOOK call is handled: with the AI, as the owner's own call, or not yet known. */
    internal enum class OffhookKind { AI, OWN, UNKNOWN }

    private fun buildNotification(text: String): Notification {
        val channelId = "call_stream_silent"
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            val nm = getSystemService(NotificationManager::class.java)
            nm.createNotificationChannel(
                NotificationChannel(
                    channelId,
                    getString(R.string.channel_service),
                    NotificationManager.IMPORTANCE_MIN,
                ),
            )
        }
        val pi = PendingIntent.getActivity(
            this,
            0,
            Intent(this, MainActivity::class.java),
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
        val enabled = Prefs.isAiCallEnabled(this)
        return NotificationCompat.Builder(this, channelId)
            .setContentTitle(getString(R.string.notification_title))
            .setContentText(if (enabled) text else getString(R.string.notification_text_normal))
            .setSmallIcon(android.R.drawable.ic_lock_silent_mode)
            .setContentIntent(pi)
            .setOngoing(true)
            .setSilent(true)
            .setPriority(NotificationCompat.PRIORITY_MIN)
            .build()
    }

    companion object {
        const val ACTION_START = "com.pixelcallai.streamer.START"
        const val ACTION_STOP = "com.pixelcallai.streamer.STOP"
        const val ACTION_SET_ENABLED = "com.pixelcallai.streamer.SET_ENABLED"
        const val ACTION_SMOKE_CONNECT = "com.pixelcallai.streamer.SMOKE_CONNECT"
        const val ACTION_CALL_ACTIVE = "com.pixelcallai.streamer.CALL_ACTIVE"
        const val ACTION_CALL_ENDED = "com.pixelcallai.streamer.CALL_ENDED"
        /** SilentInCallService rejected a call that rang during another call. */
        const val ACTION_CALL_WAITING_REJECTED = "com.pixelcallai.streamer.CALL_WAITING_REJECTED"
        const val EXTRA_ENABLED = "enabled"
        /** Incoming caller number from screening / InCall (not MSISDN). */
        const val EXTRA_CALLER_ID = "caller_id"
        /** Monotonic Telecom Call instance assigned by [SilentInCallService]. */
        const val EXTRA_CALL_INSTANCE_ID = "call_instance_id"
        /** [ACTION_CALL_WAITING_REJECTED]: the rejected call's number, SIM country and subscription id (-1 = unknown). */
        const val EXTRA_WAITING_CALLER_ID = "waiting_caller_id"
        const val EXTRA_SIM_COUNTRY = "sim_country"
        const val EXTRA_SIM_SUB_ID = "sim_sub_id"
        private const val NO_CALL_INSTANCE = 0L
        private const val NO_PENDING_SERVICE_STOP = 0L
        private const val NOTIF_ID = 42
        private const val NOTIF_INFO_ID = 43
        private const val TAG = "CallAudioService"
        /**
         * Max ring wait for the agent's CONTROL_IDLE connection. Not up by then: the call
         * is not answered (it rings on to the carrier's missed call / voicemail).
         * The agent starts talking only after the media hello.
         */
        private const val AGENT_READY_TIMEOUT_MS = 12_000L
        /** Ring time when the agent sends no `answer_plan` (about one ring). */
        private const val DEFAULT_ANSWER_RING_MS = 3_000L
        private const val MAX_ANSWER_RING_MS = 8_000L
        private const val ANSWER_PLAN_WAIT_MS = 500L
        /** Longest wait after RINGING for SilentInCallService to store this call's number and SIM. */
        private const val CALL_INFO_WAIT_MS = 150L
        private const val CALL_INFO_POLL_MS = 10L
        /** OFFHOOK direction unknown (InCallService not bound yet): ask again this often. */
        private const val OUTGOING_CHECK_ATTEMPTS = 4
        private const val OUTGOING_CHECK_RETRY_MS = 50L
        /** A `place_call` that has not reached DIALING/OFFHOOK by then is forgotten. */
        private const val OUTBOUND_DIAL_TIMEOUT_MS = 30_000L
        /** Media connection down this long during a call → the call is ended. */
        private const val MEDIA_LOST_HANGUP_MS = 3_000L
        private const val MAX_PENDING_CONTROL_MESSAGES = 10
        /** Minimal settle after OFFHOOK before inject open (the agent owns any greeting delay). */
        private const val OFFHOOK_SETTLE_MS = 50L
        private const val CALL_STATE_WATCHDOG_MS = 250L
        private const val SERVICE_STOP_RESTORE_TIMEOUT_MS = 30_000L
        /**
         * After inject start, wait this many ms before local menu inject (only if LOCAL_MENU on).
         */
        private const val LOCAL_MENU_DELAY_MS = 50L

        /** The agent's `place_call` is being dialed or is up (read by [SilentInCallService]). */
        @Volatile
        private var agentCallbackPending = false

        /** True while a call the agent asked for is pending; such a call is not "dialed by hand". */
        fun isAgentCallbackPending(): Boolean = agentCallbackPending

        /**
         * Whether an OFFHOOK call gets the AI. [ownCall] = dialed by hand (stored per call
         * by SilentInCallService; an agent callback is false); null = InCallService does
         * not know the call yet, and a call this app answered after it rang is incoming
         * even then.
         */
        internal fun classifyOffhook(agentCallback: Boolean, answeredRing: Boolean, ownCall: Boolean?): OffhookKind =
            when {
                agentCallback -> OffhookKind.AI
                ownCall == true -> OffhookKind.OWN
                ownCall == false -> OffhookKind.AI
                answeredRing -> OffhookKind.AI
                else -> OffhookKind.UNKNOWN
            }
    }

    /** Play `assets/ivr_wait.wav` once per call into uplink. */
    private fun startLocalWait(
        source: String,
        generation: Long,
        callInstanceId: Long,
        inj: UplinkInjector,
    ) {
        if (!isSessionCurrent(generation, callInstanceId)) return
        if (localWaitPlayedThisCall) {
            Log.i(TAG, "startLocalWait ignored — already played this call")
            return
        }
        localWaitPlayedThisCall = true
        if (!isSessionCurrent(generation, callInstanceId)) {
            Log.w(TAG, "startLocalWait skipped no injector source=$source")
            dropAgentPcm = false
            return
        }
        localWaitJob?.cancel()
        localWaitJob = scope.launch {
            val pcm = LocalMenuPlayer.loadWaitPcm(this@CallAudioService)
            if (!isSessionCurrent(generation, callInstanceId)) return@launch
            if (pcm.isEmpty()) {
                Log.w(TAG, "local wait asset empty source=$source")
                localWaitPlaying = false
                dropAgentPcm = false
                return@launch
            }
            localMenuPlaying = false
            inj.clearPending()
            if (!isSessionCurrent(generation, callInstanceId)) return@launch
            localWaitPlaying = true
            dropAgentPcm = true // block any agent PCM for the entire wait
            Log.i(TAG, "local wait ONCE source=$source bytes=${pcm.size}")
            try {
                inj.awaitRouteReady(1500)
                if (!isSessionCurrent(generation, callInstanceId)) return@launch
                LocalMenuPlayer.playInto(
                    inj,
                    pcm,
                    { !isSessionCurrent(generation, callInstanceId) || !localWaitPlaying },
                    "wait",
                )
            } finally {
                if (isSessionCurrent(generation, callInstanceId)) {
                    localWaitPlaying = false
                    dropAgentPcm = false
                }
            }
            Log.i(TAG, "local wait done source=$source")
        }
    }
}

private fun CallAudioService.deviceSerial(): String {
    return try {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            Build.getSerial()
        } else {
            @Suppress("DEPRECATION")
            Build.SERIAL
        }
    } catch (_: SecurityException) {
        "pixel-${Process.myUid()}"
    }.ifBlank { "pixel-${Process.myUid()}" }
}

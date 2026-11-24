// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

package com.pixelcallai.streamer

import java.io.File

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.pm.ServiceInfo
import android.content.Intent
import android.media.AudioAttributes
import android.media.Ringtone
import android.media.RingtoneManager
import android.os.Build
import android.os.Handler
import android.os.IBinder
import android.os.Looper
import android.os.Process
import android.os.SystemClock
import android.telecom.TelecomManager
import android.telephony.PhoneStateListener
import android.telephony.SubscriptionManager
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
import kotlinx.coroutines.withContext
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
    @Volatile private var controlClient: AgentControlClient? = null
    /** Agent `send_sms`; answers on the control connection. */
    private var smsSender: SmsSender? = null
    private var injector: UplinkInjector? = null
    private var audioGuard: LocalAudioGuard? = null
    private var wakeController: CallWakeController? = null
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
    private var answerRetryJob: Job? = null
    private var callStateWatchdogJob: Job? = null
    @Volatile private var answerScheduled = false
    @Volatile private var lastRingingSignalAt = 0L
    /** [ensureHeavyInit] ran. */
    private var heavyInitDone = false
    /** Number of a call the AI could not take and handed to the owner; cleared once answered. */
    @Volatile private var aiMissedCaller: String? = null
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
    /** Result of a switch the agent asked for, found at start-up because the app was
     * stopped while the root script ran; sent as `set_mode_result` once connected.
     */
    private val unreportedResult = java.util.concurrent.atomic.AtomicReference<PhoneModeManager.ApplyLog?>(null)
    @Volatile private var localRingtone: Ringtone? = null
    @Volatile private var pendingUrgentPromptId: String? = null
    @Volatile private var activeTakeoverCaller: String = ""
    @Volatile private var activeTakeoverCallerName: String = ""
    @Volatile private var activeUrgentReason: String = ""
    private val modeListener = PhoneModeManager.Listener { mode ->
        mainHandler.post { onPhoneModeChanged(mode) }
    }

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onCreate() {
        super.onCreate()
        instance = this
        audioGuard = LocalAudioGuard(this)
        telephonyManager = getSystemService(TELEPHONY_SERVICE) as TelephonyManager
        PhoneModeManager.addListener(modeListener)
        if (isStandaloneQuietNormal()) {
            // A normal phone with nothing to do: the start that created the service most
            // likely ends it right away, so the agent and the root check wait until needed.
            Log.i(TAG, "standalone normal phone: start-up work deferred")
        } else {
            ensureHeavyInit()
        }
    }

    /** Standalone normal phone with the console not on screen: the service has nothing to do. */
    private fun isStandaloneQuietNormal(): Boolean =
        BuildConfig.STANDALONE && !Prefs.isAiCallEnabled(this) && !consoleLinkWanted()

    /**
     * Standalone normal phone: the control connection stays up while the console is on
     * screen, for [CONSOLE_LINK_LINGER_MS] after it left, and while a call the agent
     * dialed (AI call) is in progress, so AI calls and text replies work. Calls are not
     * muted and Bageo does not take the phone app role for this.
     */
    private fun consoleLinkWanted(): Boolean {
        if (!BuildConfig.STANDALONE || Prefs.isAiCallEnabled(this)) return false
        if (consoleVisible || SystemClock.elapsedRealtime() < consoleLinkUntil) return true
        return pendingOutboundCallId != null || callActive || anyCallBusy
    }

    /** Standalone normal phone: connect for the console (calls are left alone). */
    private fun openConsoleLink() {
        mainHandler.removeCallbacks(consoleLinkCheck)
        // Opened after the console left (a switch to a normal phone just then): it still ends.
        if (!consoleVisible) {
            scheduleConsoleLinkCheck((consoleLinkUntil - SystemClock.elapsedRealtime()).coerceAtLeast(0L))
        }
        registerPhoneCallbacks()
        startControlClient()
        Log.i(TAG, "standalone normal phone: console on screen, control connected, calls untouched")
    }

    private val consoleLinkCheck = Runnable { closeConsoleLinkIfUnused() }

    private fun scheduleConsoleLinkCheck(delayMs: Long = 0L) {
        mainHandler.removeCallbacks(consoleLinkCheck)
        mainHandler.postDelayed(consoleLinkCheck, delayMs)
    }

    /** Main thread: the console link is no longer needed (normal phone, console gone, no AI call). */
    private fun closeConsoleLinkIfUnused() {
        if (!BuildConfig.STANDALONE || Prefs.isAiCallEnabled(this)) return
        // On its way to AI mode: the switch arms the service itself.
        if (PhoneModeManager.isSwitchingTo(Prefs.PHONE_MODE_AI)) return
        if (consoleLinkWanted()) {
            if (!consoleVisible) scheduleConsoleLinkCheck(CONSOLE_LINK_RECHECK_MS)
            return
        }
        callStateWatchdogJob?.cancel()
        callStateWatchdogJob = null
        unregisterPhoneCallbacks()
        stopControlClient()
        stopForeground(STOP_FOREGROUND_REMOVE)
        getSystemService(NotificationManager::class.java)?.cancel(NOTIF_ID)
        stopSelf()
        Log.i(TAG, "standalone normal phone: console closed, control connection and service stopped")
    }

    /** Starts with no service work to do: in a quiet normal phone these end the service at once. */
    private fun isPlainStart(action: String?): Boolean =
        action == null || action == ACTION_START || action == ACTION_REFRESH_NOTIF

    /**
     * The service's start-up work (agent, SMS, SIM watch, root mode check), done once,
     * when the service first has something to do.
     */
    private fun ensureHeavyInit() {
        if (heavyInitDone) return
        heavyInitDone = true
        if (BuildConfig.STANDALONE) {
            wakeController = CallWakeController.create(this)
            EmbeddedAgent.ensureStarted(this)
        }
        smsSender = SmsSender(this) { msg -> sendControlOrQueue(msg) }
        watchSimChanges()
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
        if (isPlainStart(action) && isStandaloneQuietNormal() && !callActive && !ownerHandlingCallFlag) {
            // Normal phone: an outside start (app screen, boot, sticky restart) has nothing
            // to do. Enter the foreground as Android requires, then stop at once.
            enterForeground()
            stopForeground(STOP_FOREGROUND_REMOVE)
            getSystemService(NotificationManager::class.java)?.cancel(NOTIF_ID)
            stopSelf()
            Log.i(TAG, "standalone normal phone: start ${action ?: "sticky"} ignored, service stopped")
            return START_NOT_STICKY
        }
        if (action != ACTION_STOP) {
            cancelPendingServiceStop("new start action=${action ?: "sticky"}")
            ensureHeavyInit()
            if (BuildConfig.STANDALONE) {
                EmbeddedAgent.ensureStarted(this)
            }
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
            ACTION_CALL_RINGING -> {
                lastRingingSignalAt = SystemClock.elapsedRealtime()
                armWatcher()
                if (Prefs.isAiCallEnabled(this)) {
                    Log.i(TAG, "explicit Telecom RINGING — schedule AI answer")
                    handleCallState(TelephonyManager.CALL_STATE_RINGING)
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
            ACTION_OWNER_TAKEOVER -> {
                if (BuildConfig.STANDALONE) {
                    performOwnerTakeover(intent?.getStringExtra(EXTRA_URGENT_ID))
                }
                armWatcher()
            }
            ACTION_DECLINE_URGENT -> {
                armWatcher()
                if (BuildConfig.STANDALONE) {
                    performDeclineUrgent(intent?.getStringExtra(EXTRA_URGENT_ID).orEmpty())
                }
            }
            ACTION_WHITELIST_ANSWER -> {
                if (BuildConfig.STANDALONE) {
                    performWhitelistAnswer()
                }
                armWatcher()
            }
            ACTION_WHITELIST_HAND_AI -> {
                if (BuildConfig.STANDALONE) {
                    performWhitelistHandToAi()
                }
                armWatcher()
            }
            ACTION_WHITELIST_REJECT -> {
                if (BuildConfig.STANDALONE) {
                    performWhitelistReject()
                }
                armWatcher()
            }
            ACTION_OWNER_HANGUP -> {
                armWatcher()
                if (BuildConfig.STANDALONE) {
                    performOwnerHangup()
                }
            }
            ACTION_REFRESH_NOTIF -> {
                armWatcher()
            }
            ACTION_SWITCH_TO_NORMAL -> {
                armWatcher()
                if (BuildConfig.STANDALONE) {
                    Prefs.markManualModeSwitch(this)
                    PhoneModeManager.requestSwitch(this, Prefs.PHONE_MODE_NORMAL, callBusy = isCallBusy()) { result ->
                        Log.i(TAG, "notif switch normal ok=${result.ok} err=${result.error}")
                    }
                }
            }
            ACTION_CONSOLE_VISIBLE -> {
                if (BuildConfig.STANDALONE && !Prefs.isAiCallEnabled(this)) openConsoleLink() else armWatcher()
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
            // Standalone: the call notification is a call style one; it goes with a phone call service.
            BuildConfig.STANDALONE ->
                ServiceInfo.FOREGROUND_SERVICE_TYPE_MICROPHONE or ServiceInfo.FOREGROUND_SERVICE_TYPE_PHONE_CALL
            else -> ServiceInfo.FOREGROUND_SERVICE_TYPE_MICROPHONE
        }
        try {
            if (type != null) startForeground(NOTIF_ID, notification, type) else startForeground(NOTIF_ID, notification)
        } catch (e: Exception) {
            Log.e(TAG, "startForeground type=$type refused", e)
            try {
                startForeground(NOTIF_ID, notification)
            } catch (e2: Exception) {
                Log.e(TAG, "startForeground refused; the service keeps going without it", e2)
            }
        }
        if (shouldHideIdleNotification()) {
            stopForeground(STOP_FOREGROUND_REMOVE)
        }
    }

    private fun shouldHideIdleNotification(): Boolean =
        BuildConfig.STANDALONE &&
            !Prefs.isAiCallEnabled(this) &&
            !anyCallBusy &&
            !callActive &&
            !answerScheduled &&
            !ownerHandlingCallFlag

    private fun armWatcher() {
        enterForeground()
        if (!Prefs.isAiCallEnabled(this)) {
            // Normal phone mode: restore mic/audio immediately.
            audioGuard?.stopHoldLoop()
            audioGuard?.restore()
            try {
                (getSystemService(AUDIO_SERVICE) as android.media.AudioManager).isMicrophoneMute = false
            } catch (e: Exception) {
                Log.w(TAG, "microphone unmute: ${e.message}")
            }
            if (BuildConfig.STANDALONE) {
                stopForeground(STOP_FOREGROUND_REMOVE)
                getSystemService(NotificationManager::class.java)?.cancel(NOTIF_ID)
                if (consoleLinkWanted()) {
                    openConsoleLink()
                } else {
                    unregisterPhoneCallbacks()
                    stopControlClient()
                    stopSelf()
                    Log.i(TAG, "standalone normal mode: service stopped, original phone active")
                }
                return
            }
            registerPhoneCallbacks()
            startControlClient()
            Log.i(TAG, "normal phone mode: service connected, calls left alone")
            return
        }
        ensureHeavyInit()
        if (PhoneModeManager.isSwitchingTo(Prefs.PHONE_MODE_NORMAL)) {
            // The app's copy says AI until the script has made this a normal phone: no
            // muting and no phone app role grab that would fight it.
            registerPhoneCallbacks()
            startControlClient()
            Log.i(TAG, "switching to a normal phone: not muting, phone app left alone")
            return
        }
        if (BuildConfig.STANDALONE && Prefs.isStandaloneDailySound(this) && isPhoneQuiet()) {
            // Daily Sound with no call: nothing stays turned down. This also puts back what
            // a Daily Sound call changed; after the silent AI mode only the microphone and
            // speaker go back (the module has just set the volumes, which the guard saved as zero).
            audioGuard?.settleForDailySound()
        } else {
            audioGuard?.applyHardMute()
        }
        scope.launch(Dispatchers.IO) { DialerRoleHelper.ensureRoles(applicationContext) }
        registerPhoneCallbacks()
        // After crash restart, TelephonyCallback may never re-deliver the state edge
        // (RINGING/OFFHOOK already sticky). Poll and fire handleCallState now so
        // ACTIVE→media hello does not sit ~8s waiting for a phantom OFFHOOK edge.
        catchUpCallState(reason = "armWatcher")
        if (!BuildConfig.STANDALONE || effectiveCallState() != TelephonyManager.CALL_STATE_IDLE) {
            startCallStateWatchdog()
        }
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
        val state = effectiveCallState() ?: return
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
                if (isIdleInsideRing()) {
                    Log.i(TAG, "callState catch-up ($reason): IDLE while Telecom still rings — keep scheduled answer")
                } else if (callActive || answerScheduled || bridge != null || injector != null) {
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
            var lastState: Int? = effectiveCallState()
            while (isActive) {
                val state = effectiveCallState()
                if (state != null) {
                    val awaitingEarlyRingBind =
                        state == TelephonyManager.CALL_STATE_IDLE && isIdleInsideRing()
                    val recoveryNeeded = when (state) {
                        // A ring the agent was not ready for or handed to the owner is left alone.
                        TelephonyManager.CALL_STATE_RINGING ->
                            shouldRecoverRingingInWatchdog(
                                answerScheduled = answerScheduled,
                                callActive = callActive,
                                ringGivenUp = ringGivenUp,
                                ownerHandlingCall = BuildConfig.STANDALONE && ownerHandlingCallFlag,
                            )
                        // Do not treat a live media WS as missing just because a
                        // duplicate OFFHOOK/ACTIVE race cleared a local flag. A call
                        // left without AI (dialed by hand) is not a missing session.
                        TelephonyManager.CALL_STATE_OFFHOOK ->
                            !callActive && bridge == null && injector == null && !offhookWithoutAi
                        TelephonyManager.CALL_STATE_IDLE ->
                            !awaitingEarlyRingBind &&
                                (callActive || answerScheduled || bridge != null || injector != null)
                        else -> false
                    }
                    if ((state != lastState && !awaitingEarlyRingBind) || recoveryNeeded) {
                        Log.i(
                            TAG,
                            "callState watchdog state=$state previous=$lastState recovery=$recoveryNeeded",
                        )
                        handleCallState(state)
                        lastState = state
                    }
                    if (BuildConfig.STANDALONE &&
                        state == TelephonyManager.CALL_STATE_IDLE &&
                        !awaitingEarlyRingBind &&
                        !callActive &&
                        !answerScheduled &&
                        bridge == null &&
                        injector == null &&
                        !ownerHandlingCallFlag
                    ) {
                        Log.i(TAG, "standalone watchdog idle — pausing until next call")
                        break
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
        if (SilentInCallService.isAddCallActiveOrMerged()) {
            keepMediaForAddCall("explicit ACTIVE instance=$instanceId")
            return
        }
        val live = SilentInCallService.isLiveCallInstance(instanceId)
        val selected = SilentInCallService.currentActiveCallInstanceId()
        val state = effectiveCallState()
        if (
            instanceId == NO_CALL_INSTANCE || live != true || selected != instanceId ||
            state != TelephonyManager.CALL_STATE_OFFHOOK
        ) {
            Log.w(TAG, "ignore stale ACTIVE instance=$instanceId live=$live selected=$selected state=$state")
            catchUpCallState(reason = "explicit-call-active-stale")
            return
        }
        if (SilentInCallService.isOwnCallInstance(instanceId) || (BuildConfig.STANDALONE && ownerHandlingCallFlag)) {
            Log.i(TAG, "ignore ACTIVE instance=$instanceId: owner handling call")
            if (BuildConfig.STANDALONE && ownerHandlingCallFlag) {
                stopLocalRingtone()
                cancelOwnerCallNotification()
                audioGuard?.restoreForOwnCall()
                SilentInCallService.unmuteOwnCall()
                launchOwnerCallUi(
                    mode = "in_call",
                    callerId = activeTakeoverCaller.ifBlank { currentCallerId() },
                    callerName = activeTakeoverCallerName,
                    reason = "",
                    urgentId = "",
                )
            }
            return
        }
        if (BuildConfig.STANDALONE) startCallStateWatchdog()
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
        // Telecom removed the call: an IDLE from now on is real, not a ring still on its way.
        if (BuildConfig.STANDALONE) lastRingingSignalAt = 0L
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

    private fun effectiveCallState(): Int? =
        resolveEffectiveCallState(currentTelephonyCallState(), SilentInCallService.effectiveTelecomCallState())

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

    /**
     * Whitelist urgent call dialing or merged: calls the merge adds are the same call.
     * Keep the media session as it is; never start another one (no bridge, no AI, no
     * injected audio) and never switch it to another instance.
     */
    private fun keepMediaForAddCall(reason: String) {
        val hasSession = synchronized(sessionOwnershipLock) { callActive || bridge != null || injector != null }
        Log.i(
            TAG,
            "$reason: add_call dialing or merged — keep media instance=${mediaCallInstance.get()} " +
                "has_session=$hasSession (no new session, no AI)",
        )
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
        if (Looper.myLooper() != Looper.getMainLooper()) {
            mainHandler.post { handleCallState(state) }
            return
        }
        if (BuildConfig.STANDALONE && state == TelephonyManager.CALL_STATE_IDLE &&
            Prefs.isAiCallEnabled(this) && isIdleInsideRing()
        ) {
            // TelephonyManager can say IDLE while Telecom still rings (the service was just
            // woken by the ring, VoWiFi, a second SIM). The watchdog checks again shortly.
            Log.i(TAG, "IDLE ignored — Telecom still has the call (or it is on its way)")
            startCallStateWatchdog()
            return
        }
        reportCallState(state != TelephonyManager.CALL_STATE_IDLE)
        if (!Prefs.isAiCallEnabled(this)) {
            if (!isNormalPhoneAgentCall(state)) {
                Log.i(TAG, "normal phone mode; leave call state=$state alone")
                if (BuildConfig.STANDALONE && state == TelephonyManager.CALL_STATE_IDLE) scheduleConsoleLinkCheck()
                return
            }
            Log.i(TAG, "normal phone mode: AI call the agent dialed, state=$state")
        }
        when (state) {
            TelephonyManager.CALL_STATE_RINGING -> {
                anyCallBusy = true
                if (BuildConfig.STANDALONE) {
                    enterForeground()
                    startCallStateWatchdog()
                }
                wakeController?.onCallActive("ringing")
                if (BuildConfig.STANDALONE && ownerHandlingCallFlag) {
                    Log.i(TAG, "RINGING ignored (owner handling incoming call)")
                    return
                }
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
                audioGuard?.silenceRingingCall()
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
                        ringGivenUp = true
                        answerScheduled = false
                        if (BuildConfig.STANDALONE) {
                            // The ring was silenced for the AI: give the call back to the
                            // owner (it rings, the call screen opens) instead of a silent ring.
                            Log.w(TAG, "agent not ready within ${AGENT_READY_TIMEOUT_MS}ms — the call goes to the owner")
                            handRingingCallToOwner("agent_not_ready", tellAgent = false)
                        } else {
                            // Answering now would give the caller dead air: leave the call
                            // ringing so the carrier's missed call / voicemail takes it.
                            Log.w(
                                TAG,
                                "agent not ready within ${AGENT_READY_TIMEOUT_MS}ms — not answering; " +
                                    "the call keeps ringing (missed call / voicemail)",
                            )
                        }
                        return@launch
                    }
                    // This call's own number and SIM, once SilentInCallService stored them.
                    val details = awaitRingingCallDetails(ringAt + CALL_INFO_WAIT_MS)
                    // Ring about once before answering, like a person reaching for the
                    // phone; the agent prepares the AI's opening line meanwhile.
                    val start = requestAnswerPlan(details)
                    var plan = start.plan
                    if (applyRingDecision(plan, details)) return@launch
                    val late = start.late
                    var waitMs = plan.delayMs - (SystemClock.elapsedRealtime() - ringAt)
                    if (late != null) {
                        // No decision yet: ring the usual time, and follow the agent's
                        // decision (reject, owner pickup, ring time) if it comes meanwhile.
                        val latePlan = when {
                            late.isCompleted -> late.await()
                            waitMs > 0 -> withTimeoutOrNull(waitMs) { late.await() }
                            else -> null
                        }
                        pendingAnswerPlan = null
                        if (latePlan != null) {
                            plan = latePlan.copy(delayMs = latePlan.delayMs.coerceIn(0L, MAX_ANSWER_RING_MS))
                            Log.i(
                                TAG,
                                "late answer_plan applied while ringing reject=${plan.reject} owner_pickup=${plan.ownerPickup}",
                            )
                            if (applyRingDecision(plan, details)) return@launch
                            waitMs = plan.delayMs - (SystemClock.elapsedRealtime() - ringAt)
                            if (waitMs > 0) delay(waitMs)
                        }
                    } else if (waitMs > 0) {
                        Log.i(TAG, "ring ${plan.delayMs}ms before answer — wait ${waitMs}ms more")
                        delay(waitMs)
                    }
                    if (callActive) return@launch
                    if (answerRejected) {
                        rejectRingingCall()
                        return@launch
                    }
                    if (BuildConfig.STANDALONE && ownerHandlingCallFlag) {
                        // An owner pickup came just now, after the waiting above: ring the owner.
                        applyRingDecision(
                            AnswerPlan(
                                delayMs = 0L,
                                reject = false,
                                ownerPickup = true,
                                callerName = activeTakeoverCallerName,
                                reason = activeUrgentReason,
                            ),
                            details,
                        )
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
                anyCallBusy = true
                if (BuildConfig.STANDALONE) {
                    enterForeground()
                    startCallStateWatchdog()
                }
                wakeController?.onCallActive("offhook")
                if (BuildConfig.STANDALONE && ownerHandlingCallFlag) {
                    answerScheduled = false
                    answerJob?.cancel()
                    answerJob = null
                    // The owner answered: the call the AI could not take is not missed.
                    aiMissedCaller = null
                    stopLocalRingtone()
                    cancelOwnerCallNotification()
                    audioGuard?.stopHoldLoop()
                    audioGuard?.restoreForOwnCall()
                    SilentInCallService.unmuteOwnCall()
                    updateNotification()
                    launchOwnerCallUi(
                        mode = "in_call",
                        callerId = activeTakeoverCaller.ifBlank { currentCallerId() },
                        callerName = activeTakeoverCallerName,
                        reason = "",
                        urgentId = "",
                    )
                    Log.i(TAG, "OFFHOOK ownerHandlingCallFlag=true — kept on phone for owner")
                    return
                }
                val answeredRing = answerScheduled
                // A reject that came after the answer went out: hang up before the AI joins.
                val lateReject = BuildConfig.STANDALONE && answeredRing && answerRejected
                answerScheduled = false
                ringGivenUp = false
                answerRejected = false
                answerJob?.cancel()
                answerJob = null
                answerRetryJob?.cancel()
                answerRetryJob = null
                if (lateReject) {
                    offhookWithoutAi = true
                    Log.i(TAG, "OFFHOOK after a late reject — hanging up before the AI joins")
                    performOwnerHangup()
                    return
                }
                val callInstanceId = currentCallInstanceForTelephony()
                if (SilentInCallService.isAddCallActiveOrMerged()) {
                    keepMediaForAddCall("OFFHOOK instance=$callInstanceId")
                    return
                }
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
                anyCallBusy = false
                if (BuildConfig.STANDALONE) {
                    stopLocalRingtone()
                    cancelOwnerCallNotification()
                    aiMissedCaller?.let { caller ->
                        aiMissedCaller = null
                        showAiMissedNotice(caller)
                    }
                    ownerHandlingCallFlag = false
                    pendingUrgentPromptId = null
                    activeTakeoverCaller = ""
                    activeTakeoverCallerName = ""
                    activeUrgentReason = ""
                }
                wakeController?.onCallIdle()
                Log.i(TAG, "IDLE — end call session")
                if (answerScheduled && !callActive) sendIncomingCancel()
                answerScheduled = false
                lastRingingSignalAt = 0L
                ringGivenUp = false
                answerRejected = false
                offhookWithoutAi = false
                offhookRecheckJob?.cancel()
                offhookRecheckJob = null
                answerJob?.cancel()
                answerJob = null
                answerRetryJob?.cancel()
                answerRetryJob = null
                audioGuard?.stopHoldLoop()
                endCallSession()
                // No call left: nothing stored may reach the next call (a hidden number
                // must not pick up this call's number when the InCall data is late).
                Prefs.clearLastCallInfo(this)
                if (BuildConfig.STANDALONE && Prefs.isStandaloneDailySound(this)) {
                    audioGuard?.settleForDailySound()
                } else {
                    // Stay hard-muted while armed (idle watcher); do not restore yet.
                    audioGuard?.applyHardMute()
                }
                audioGuard?.refreshConsolePreview()
                updateNotification()
                if (BuildConfig.STANDALONE && !Prefs.isAiCallEnabled(this)) scheduleConsoleLinkCheck()
            }
        }
    }

    /**
     * Standalone normal phone: [state] belongs to a call the agent dialed through the
     * console link (the AI talks on it). A ring is never one: the owner answers it.
     */
    private fun isNormalPhoneAgentCall(state: Int): Boolean =
        BuildConfig.STANDALONE &&
            state != TelephonyManager.CALL_STATE_RINGING &&
            (pendingOutboundCallId != null || callActive || synchronized(sessionOwnershipLock) { bridge != null })

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
            sendControlOrQueue(
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
        // Standalone: the call notification turns into the owner's call (hang up, back to the call screen).
        if (BuildConfig.STANDALONE) updateNotification()
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
        if (effectiveCallState() != TelephonyManager.CALL_STATE_OFFHOOK) return
        val hasSession = synchronized(sessionOwnershipLock) {
            callActive || bridge != null || injector != null
        }
        if (hasSession) return
        when (classifyOffhook(pendingOutboundCallId != null, answeredRing, ownCall)) {
            OffhookKind.AI -> startOffhookSession(currentCallInstanceForTelephony())
            OffhookKind.OWN -> onOwnCallOffhook()
            OffhookKind.UNKNOWN -> {
                Log.w(
                    TAG,
                    "OFFHOOK direction still unknown after ${OUTGOING_CHECK_ATTEMPTS * OUTGOING_CHECK_RETRY_MS}ms — " +
                        "restoring audio for own call",
                )
                onOwnCallOffhook()
            }
        }
    }

    private fun acceptViaTelecomManager(): Boolean {
        return try {
            val telecom = getSystemService(TELECOM_SERVICE) as TelecomManager
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
                @Suppress("DEPRECATION")
                telecom.acceptRingingCall()
                Log.i(TAG, "acceptRingingCall issued via TelecomManager")
                true
            } else {
                false
            }
        } catch (e: SecurityException) {
            Log.w(TAG, "acceptRingingCall denied: ${e.message}")
            false
        } catch (e: Exception) {
            Log.w(TAG, "acceptRingingCall failed: ${e.message}")
            false
        }
    }

    private fun answerSilently() {
        // Primary: Default Dialer InCallService.answer(STATE_AUDIO_ONLY) — reliable on Android 14–16.
        val answeredInCall = SilentInCallService.answerActiveIfHeld()
        if (answeredInCall) {
            Log.i(TAG, "answer via SilentInCallService")
        } else {
            acceptViaTelecomManager()
        }
        // Verify transition out of RINGING; retry with TelecomManager + root fallback if still ringing.
        answerRetryJob?.cancel()
        answerRetryJob = scope.launch {
            for (attempt in 1..2) {
                delay(if (attempt == 1) 450L else 650L)
                if (callActive || ownerHandlingCallFlag || answerRejected) return@launch
                if (effectiveCallState() != TelephonyManager.CALL_STATE_RINGING) return@launch
                Log.w(TAG, "call still RINGING after answer attempt #$attempt — retrying pickup")
                val inCallOk = withContext(Dispatchers.Main) { SilentInCallService.answerActiveIfHeld() }
                if (!inCallOk || attempt == 2) {
                    acceptViaTelecomManager()
                }
                if (attempt == 2) {
                    // KEYCODE_CALL: the system answers the ringing call. (`cmd telecom` has no
                    // command that answers a call.)
                    val cmdOk = RootShell.run("input keyevent 5", 5).ok
                    Log.i(TAG, "root answer fallback attempt=$attempt ok=$cmdOk")
                }
            }
            if (!BuildConfig.STANDALONE) return@launch
            delay(ANSWER_GIVE_UP_MS)
            if (callActive || ownerHandlingCallFlag || answerRejected) return@launch
            if (effectiveCallState() != TelephonyManager.CALL_STATE_RINGING) return@launch
            Log.w(TAG, "call still RINGING after every answer attempt — the call goes to the owner")
            handRingingCallToOwner("answer_failed", tellAgent = true)
        }
    }

    /**
     * The agent's decision for the ringing call: decline it, or ring the owner directly
     * (whitelist). True when the call is taken care of and the AI does not answer.
     */
    private fun applyRingDecision(plan: AnswerPlan, details: AgentCallDetails): Boolean {
        if (plan.reject || answerRejected) {
            rejectRingingCall()
            return true
        }
        if (BuildConfig.STANDALONE && plan.ownerPickup) {
            ownerHandlingCallFlag = true
            offhookWithoutAi = true
            answerScheduled = false
            activeTakeoverCaller = details.callerId
            activeTakeoverCallerName = plan.callerName
            activeUrgentReason = plan.reason
            audioGuard?.stopHoldLoop()
            audioGuard?.restoreForOwnCall()
            SilentInCallService.unmuteOwnCall()
            startLocalRingtone()
            notifyAndLaunchOwnerUi(
                mode = "incoming_whitelist",
                callerId = details.callerId,
                callerName = plan.callerName,
                reason = plan.reason,
                urgentId = "",
            )
            Log.i(TAG, "whitelist owner_pickup=true → ringing owner directly")
            return true
        }
        return false
    }

    /**
     * Standalone: the AI could not take the ringing call (agent not up, or answering
     * failed). Its ring was silenced for the AI, so it goes back to the owner: Bageo
     * rings, the call screen opens, and the notification says the AI did not answer.
     */
    private fun handRingingCallToOwner(why: String, tellAgent: Boolean) {
        if (!BuildConfig.STANDALONE) return
        if (Looper.myLooper() != Looper.getMainLooper()) {
            mainHandler.post { handRingingCallToOwner(why, tellAgent) }
            return
        }
        if (callActive || ownerHandlingCallFlag) return
        if (effectiveCallState() != TelephonyManager.CALL_STATE_RINGING) {
            Log.i(TAG, "AI did not answer ($why); the call no longer rings")
            return
        }
        // VoWiFi: TelephonyManager can still say RINGING after Telecom has put the call up.
        // An answered call is not handed back.
        if (SilentInCallService.effectiveTelecomCallState() == TelephonyManager.CALL_STATE_OFFHOOK) {
            Log.i(TAG, "Telecom already has the call up ($why) — not handed to the owner")
            return
        }
        val caller = SilentInCallService.ringingCallInfo()?.callerId.orEmpty().ifBlank { currentCallerId() }
        answerJob?.cancel()
        answerJob = null
        answerRetryJob?.cancel()
        answerRetryJob = null
        pendingAnswerPlan?.cancel()
        pendingAnswerPlan = null
        answerScheduled = false
        ringGivenUp = true
        ownerHandlingCallFlag = true
        offhookWithoutAi = true
        activeTakeoverCaller = caller
        activeTakeoverCallerName = ""
        activeUrgentReason = ""
        aiMissedCaller = caller
        audioGuard?.stopHoldLoop()
        audioGuard?.restoreForOwnCall()
        SilentInCallService.unmuteOwnCall()
        // Only an agent that took the ring hears that it ended (a queued message would
        // reach a later call otherwise).
        if (tellAgent) sendIncomingCancel()
        startLocalRingtone()
        notifyAndLaunchOwnerUi(
            mode = "incoming_whitelist",
            callerId = caller,
            callerName = "",
            reason = "",
            urgentId = "",
            aiMissed = true,
        )
        updateNotification()
        Log.w(TAG, "AI did not answer ($why) — call handed to the owner (ringing)")
    }

    /**
     * IDLE from TelephonyManager while the ring is still on: Telecom still has the call,
     * or the ring signal came moments ago and InCallService has not got the call yet.
     */
    private fun isIdleInsideRing(): Boolean {
        val signalAt = lastRingingSignalAt
        val since = if (signalAt == 0L) Long.MAX_VALUE else SystemClock.elapsedRealtime() - signalAt
        val telecomState = SilentInCallService.effectiveTelecomCallState()
        if (!BuildConfig.STANDALONE) {
            // Bridge: the rule as before (ring grace only while InCallService is not bound).
            return answerScheduled && !callActive && telecomState == null && since < RING_SIGNAL_GRACE_MS
        }
        return shouldHoldIdleForRing(
            callActive = callActive,
            answerScheduled = answerScheduled,
            telecomState = telecomState,
            telecomHasCalls = SilentInCallService.hasLiveCalls(),
            sinceRingSignalMs = since,
        )
    }

    /** No call anywhere: nothing rings, nothing is up, nothing is being answered. */
    private fun isPhoneQuiet(): Boolean =
        !callActive &&
            !answerScheduled &&
            !ownerHandlingCallFlag &&
            !anyCallBusy &&
            effectiveCallState() == TelephonyManager.CALL_STATE_IDLE &&
            !SilentInCallService.hasLiveCalls()

    /** After the call the AI could not take ended unanswered: a missed-call notice. */
    private fun showAiMissedNotice(callerId: String) {
        val nm = getSystemService(NotificationManager::class.java) ?: return
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            nm.createNotificationChannel(
                NotificationChannel(
                    AI_MISSED_CHANNEL_ID,
                    standaloneString("sa_channel_ai_missed", "AI 沒接起來的來電"),
                    NotificationManager.IMPORTANCE_DEFAULT,
                ),
            )
        }
        val openPi = PendingIntent.getActivity(
            this,
            300,
            Intent(this, MainActivity::class.java),
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
        val builder = NotificationCompat.Builder(this, AI_MISSED_CHANNEL_ID)
            .setSmallIcon(android.R.drawable.stat_notify_missed_call)
            .setContentTitle(standaloneString("sa_ai_missed_title", "AI 沒接起來"))
            .setContentText(callerId.ifBlank { standaloneString("sa_call_unknown", "未知來電") })
            .setContentIntent(openPi)
            .setAutoCancel(true)
        if (callerId.isNotBlank()) {
            val dialIntent = Intent().apply {
                setClassName(this@CallAudioService, "com.pixelcallai.streamer.OwnerCallActivity")
                putExtra("mode", "dialer")
                putExtra("dial_number", callerId)
                addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_SINGLE_TOP)
            }
            val dialPi = PendingIntent.getActivity(
                this,
                301,
                dialIntent,
                PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
            )
            builder.addAction(android.R.drawable.ic_menu_call, standaloneString("sa_btn_callback", "回撥"), dialPi)
        }
        nm.notify(NOTIF_AI_MISSED_ID, builder.build())
    }

    /**
     * `incoming` → agent `answer_plan`: how long to ring (the agent starts the AI
     * session meanwhile). No reply within [ANSWER_PLAN_WAIT_MS] (older agent) → default;
     * Standalone keeps listening ([AnswerPlanStart.late]) so a late decision still counts.
     */
    private suspend fun requestAnswerPlan(details: AgentCallDetails): AnswerPlanStart {
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
        if (answer == null) {
            Log.w(TAG, "no answer_plan within ${ANSWER_PLAN_WAIT_MS}ms — ring ${DEFAULT_ANSWER_RING_MS}ms")
            val keepListening = BuildConfig.STANDALONE
            if (!keepListening) pendingAnswerPlan = null
            return AnswerPlanStart(
                AnswerPlan(DEFAULT_ANSWER_RING_MS, reject = false),
                if (keepListening) plan else null,
            )
        }
        pendingAnswerPlan = null
        return AnswerPlanStart(answer.copy(delayMs = answer.delayMs.coerceIn(0L, MAX_ANSWER_RING_MS)), null)
    }

    /** Agent `answer_plan` `reject`: decline the ringing call; IDLE then sends `incoming_cancel`. */
    private fun rejectRingingCall(retryOnFail: Boolean = true) {
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
        if (!ok) {
            sendControlOrQueue(
                org.json.JSONObject()
                    .put("type", "reject_failed")
                    .put("serial", deviceSerial()),
            )
            if (retryOnFail) {
                mainHandler.postDelayed(
                    {
                        if (!callActive && effectiveCallState() == TelephonyManager.CALL_STATE_RINGING) {
                            rejectRingingCall(retryOnFail = false)
                        }
                    },
                    REJECT_RETRY_MS,
                )
            }
        }
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
        sendControlOrQueue(
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
                // The address the control connection got through on (USB or Wi-Fi).
                url = AgentControlClient.activeUrl(Prefs.connectionCandidates(this@CallAudioService)),
                onBinary = { pcm ->
                    if (!isSessionCurrent(generation, callInstanceId)) return@BridgeClient
                    if (BuildConfig.STANDALONE && ownerHandlingCallFlag) return@BridgeClient
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
                        if (BuildConfig.STANDALONE && ownerHandlingCallFlag) break
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
        val client = AgentControlClient(
            candidates = { Prefs.connectionCandidates(this) },
            onCommand = { obj -> handleControlCommand(obj) },
            isBusy = { isCallBusy() },
            describe = { hello ->
                val busy = isCallBusy()
                reportedInCall = busy
                hello.put("phone_mode", Prefs.phoneMode(this))
                hello.put("in_call", busy)
                // The agent sends console_url and agent_version again after this hello.
                Prefs.setConsoleUrl(this, null)
                Prefs.setAgentVersion(this, null)
            },
            sims = { SimList.read(this) },
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
        Log.i(
            TAG,
            "control client started urls=${Prefs.connectionCandidates(this).map { Prefs.redactUrl(it) }}",
        )
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
                    ownerPickup = obj.optBoolean("owner_pickup", false),
                    callerName = obj.optString("caller_name", ""),
                    reason = obj.optString("reason", ""),
                )
                Log.i(
                    TAG,
                    "answer_plan delay_ms=${plan.delayMs} reject=${plan.reject} owner_pickup=${plan.ownerPickup} prewarm=${obj.optBoolean("prewarm")}",
                )
                // A reject that arrives after the plan wait still counts while the call rings.
                if (plan.reject && answerScheduled && !callActive) answerRejected = true
                val waiting = pendingAnswerPlan
                if (waiting != null) {
                    waiting.complete(plan)
                } else if (BuildConfig.STANDALONE && plan.ownerPickup && answerScheduled && !callActive &&
                    !ownerHandlingCallFlag
                ) {
                    // Came after the answer went out but before the AI joined: the call is
                    // the owner's once it connects (the OFFHOOK owner path opens the call screen).
                    ownerHandlingCallFlag = true
                    offhookWithoutAi = true
                    activeTakeoverCallerName = plan.callerName
                    activeUrgentReason = plan.reason
                    Log.i(TAG, "late answer_plan owner_pickup — the call goes to the owner")
                }
            }
            "set_mode" -> handleSetMode(obj)
            "send_sms" -> smsSender?.handle(obj)
            "local_notice" -> if (BuildConfig.STANDALONE) showStandaloneSummaryNotification(this, obj)
            "agent_info" -> {
                Prefs.setConsoleUrl(this, obj.optString("console_url"))
                Prefs.setAgentVersion(this, obj.optString("agent_version"))
                // `wifi` missing (older agent): keep the stored addresses. `urls` empty: forget them.
                val urls = obj.optJSONObject("wifi")?.optJSONArray("urls")
                Prefs.applyAgentWifi(this, urls?.let { a -> (0 until a.length()).map { a.optString(it) } })
                Log.i(
                    TAG,
                    "agent_info console_url_set=${Prefs.consoleUrl(this) != null} agent_version=${Prefs.agentVersion(this) ?: ""} wifi_field=${urls != null} wifi_paired=${Prefs.wifiUrls(this).isNotEmpty()}",
                )
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
            sendStatus()
        } else {
            callStateWatchdogJob?.cancel()
            callStateWatchdogJob = null
            answerJob?.cancel()
            answerJob = null
            answerRetryJob?.cancel()
            answerRetryJob = null
            answerScheduled = false
            lastRingingSignalAt = 0L
            audioGuard?.stopHoldLoop()
            audioGuard?.restore()
            try {
                (getSystemService(AUDIO_SERVICE) as android.media.AudioManager).isMicrophoneMute = false
            } catch (_: Exception) {
            }
            sendStatus(inCall = false)
            if (BuildConfig.STANDALONE) {
                stopForeground(STOP_FOREGROUND_REMOVE)
                getSystemService(NotificationManager::class.java)?.cancel(NOTIF_ID)
                if (!consoleLinkWanted()) {
                    unregisterPhoneCallbacks()
                    stopControlClient()
                    stopSelf()
                    return
                }
                // The console is on screen: keep the connection for AI calls and text replies.
                openConsoleLink()
            }
            updateNotification()
        }
    }

    /** A call rings or is active on this phone (in either mode). */
    private fun isCallBusy(): Boolean {
        if (callActive || answerScheduled) return true
        val state = effectiveCallState()
        return state != null && state != TelephonyManager.CALL_STATE_IDLE
    }

    private fun reportCallState(inCall: Boolean) {
        if (reportedInCall == inCall) return
        reportedInCall = inCall
        sendStatus(inCall)
    }

    private var simListener: SubscriptionManager.OnSubscriptionsChangedListener? = null
    private var lastSims: String? = null
    private val simChangedReport = Runnable {
        val now = try {
            SimList.read(this)?.toString()
        } catch (e: Exception) {
            null
        }
        if (now != null && now != lastSims) {
            lastSims = now
            Log.i(TAG, "SIM cards changed: reporting them to the agent")
            sendStatus()
        }
    }
    private val simRecheck = Runnable { simChangedReport.run() }

    /**
     * A SIM card put in, taken out or swapped while the phone stays connected: tell the
     * agent (a `status` with the new `sims` list) so the console and errands follow.
     * The system calls this several times while a card comes up, so wait a few seconds
     * and only send when the list really changed.
     * Also look again 1 and 3 minutes after start: at boot the module grants the
     * phone-number permission after this service is already up, so the first list
     * has no numbers.
     */
    private fun watchSimChanges() {
        val subs = getSystemService(SubscriptionManager::class.java) ?: return
        lastSims = try {
            SimList.read(this)?.toString()
        } catch (e: Exception) {
            null
        }
        mainHandler.postDelayed(simRecheck, 60_000)
        mainHandler.postDelayed(simRecheck, 180_000)
        val listener = object : SubscriptionManager.OnSubscriptionsChangedListener() {
            override fun onSubscriptionsChanged() {
                mainHandler.removeCallbacks(simChangedReport)
                mainHandler.postDelayed(simChangedReport, 5_000)
            }
        }
        try {
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
                subs.addOnSubscriptionsChangedListener(mainExecutor, listener)
            } else {
                @Suppress("DEPRECATION")
                subs.addOnSubscriptionsChangedListener(listener) // onCreate runs on the main looper
            }
            simListener = listener
        } catch (e: Exception) {
            Log.w(TAG, "cannot watch SIM changes: ${e.message}")
        }
    }

    private fun stopWatchingSimChanges() {
        mainHandler.removeCallbacks(simChangedReport)
        mainHandler.removeCallbacks(simRecheck)
        val listener = simListener ?: return
        simListener = null
        try {
            getSystemService(SubscriptionManager::class.java)?.removeOnSubscriptionsChangedListener(listener)
        } catch (_: Exception) {
        }
    }

    /** `status` on the control connection: the phone mode and whether a call is on. */
    private fun sendStatus(inCall: Boolean = isCallBusy()) {
        val status = org.json.JSONObject()
            .put("type", "status")
            .put("mode", Prefs.phoneMode(this))
            .put("in_call", inCall)
        try {
            SimList.read(this)?.let { status.put("sims", it) }
        } catch (e: Exception) {
            Log.w(TAG, "status sims failed: ${e.message}")
        }
        controlClient?.sendJson(status)
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
            sendControlOrQueue(
                org.json.JSONObject()
                    .put("type", "place_call_failed")
                    .put("call_id", callId)
                    .put("task_id", taskId)
                    .put("number", number)
                    .put("reason", reason),
            )
        }
        // Standalone normal phone with the console link up: the agent may dial (the AI talks
        // on the call); the phone stays a normal phone and nothing is muted.
        val normalPhoneLink = BuildConfig.STANDALONE && !Prefs.isAiCallEnabled(this) && consoleLinkWanted()
        if (!Prefs.isAiCallEnabled(this) && !normalPhoneLink) {
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
        anyCallBusy = true
        if (normalPhoneLink) {
            registerPhoneCallbacks()
            // A call is starting: keep the service running while it lasts.
            enterForeground()
        }
        wakeController?.onCallActive("place_call")
        Log.i(
            TAG,
            "place_call call_id=$callId task_id=$taskId number_len=${number.length} sub_id=${subId ?: "-"}",
        )
        setPendingOutbound(callId, number, taskId.ifEmpty { null })
        Prefs.setLastCallerId(this, number)
        val ok = OutboundCallHelper.placeCall(this, number, callId, subId)
        sendControlOrQueue(
            org.json.JSONObject()
                .put("type", if (ok) "place_call_ack" else "place_call_failed")
                .put("call_id", callId)
                .put("task_id", taskId)
                .put("number", number)
                .put(
                    "reason",
                    when {
                        ok -> ""
                        // The SIM the agent asked for is not in this phone: nothing was dialed.
                        subId != null && OutboundCallHelper.phoneAccountFor(this, subId) == null -> "sim_not_found"
                        else -> "dial_helper_failed"
                    },
                ),
        )
        if (!ok) {
            clearPendingOutbound(callId)
            if (normalPhoneLink) finishNormalPhoneAgentDial()
        }
    }

    /** Standalone normal phone: the agent's call did not start, so nothing keeps the link busy. */
    private fun finishNormalPhoneAgentDial() {
        if (callActive || effectiveCallState() != TelephonyManager.CALL_STATE_IDLE) return
        anyCallBusy = false
        updateNotification()
        scheduleConsoleLinkCheck()
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
            if (BuildConfig.STANDALONE && !Prefs.isAiCallEnabled(this)) finishNormalPhoneAgentDial()
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
                "add_call" -> {
                    if (!isSessionCurrent(generation, callInstanceId)) return
                    handleAddCall(obj, callInstanceId)
                }
                "add_call_abort" -> {
                    val id = obj.optString("id").trim()
                    Log.i(TAG, "add_call_abort received id=$id")
                    mainHandler.post { SilentInCallService.abortAddCall(id) }
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
                "owner_urgent_prompt" -> {
                    if (BuildConfig.STANDALONE && !ownerHandlingCallFlag) {
                        val urgentId = obj.optString("id").trim()
                        val caller = obj.optString("caller_id").ifBlank { currentCallerId() }
                        val callerName = obj.optString("caller_name")
                        val reason = obj.optString("reason")
                        pendingUrgentPromptId = urgentId
                        activeTakeoverCaller = caller
                        activeTakeoverCallerName = callerName
                        activeUrgentReason = reason
                        Log.i(TAG, "owner_urgent_prompt received id=$urgentId")
                        mainHandler.post {
                            startLocalRingtone()
                            notifyAndLaunchOwnerUi(
                                mode = "urgent_prompt",
                                callerId = caller,
                                callerName = callerName,
                                reason = reason,
                                urgentId = urgentId,
                            )
                        }
                    }
                }
                "hangup" -> {
                    if (!isSessionCurrent(generation, callInstanceId)) return
                    // Whitelist urgent call or owner takeover: caller's call stays up.
                    if (SilentInCallService.isAddCallActiveOrMerged() || (BuildConfig.STANDALONE && ownerHandlingCallFlag)) {
                        Log.w(TAG, "agent hangup reason=${obj.optString("reason")} ignored: owner/add_call active")
                        return
                    }
                    Log.i(TAG, "agent hangup reason=${obj.optString("reason")}")
                    hangupFromAgent(generation, callInstanceId)
                }
            }
        } catch (_: Exception) {
        }
    }

    /**
     * Agent `add_call` (whitelist urgent call, media connection): dial the owner from
     * this call's SIM and merge the two calls ([SilentInCallService.startAddCall]).
     * Every step goes back as `add_call_result` on the media connection.
     */
    private fun handleAddCall(obj: org.json.JSONObject, callInstanceId: Long) {
        val id = obj.optString("id").trim()
        val number = obj.optString("number").trim()
        val answerTimeoutMs = obj.optLong("answer_timeout_ms", 25_000L).coerceIn(5_000L, 60_000L)
        Log.i(
            TAG,
            "add_call received id=$id number=${SilentInCallService.maskNumber(number)} " +
                "merge=${obj.optBoolean("merge", true)} answer_timeout_ms=$answerTimeoutMs instance=$callInstanceId",
        )
        val report: (String, String) -> Unit = { status, reason ->
            Log.i(TAG, "add_call_result id=$id status=$status reason=${reason.ifEmpty { "-" }}")
            val client = bridge
            if (client == null) {
                Log.w(TAG, "add_call_result id=$id status=$status not sent: media connection gone")
            } else {
                client.sendJson(
                    org.json.JSONObject()
                        .put("type", "add_call_result")
                        .put("id", id)
                        .put("status", status)
                        .put("reason", reason),
                )
            }
        }
        if (!Prefs.isAiCallEnabled(this)) {
            report("failed", "ai_call_disabled")
            return
        }
        if (id.isEmpty() || number.isEmpty()) {
            report("failed", "missing_id_or_number")
            return
        }
        mainHandler.post { SilentInCallService.startAddCall(this, id, number, answerTimeoutMs, report) }
    }

    /** End the cellular call when the agent sends `{"type":"hangup"}`. */
    private fun hangupFromAgent(generation: Long, callInstanceId: Long) {
        scope.launch {
            // Small settle so last TTS frame flushes into the injector.
            delay(250)
            if (!isSessionCurrent(generation, callInstanceId)) return@launch
            // Second guard: a whitelist urgent call is dialing the owner or merged.
            if (SilentInCallService.isAddCallActiveOrMerged()) {
                Log.w(TAG, "hangupFromAgent ignored: add_call dialing or merged — the caller's call stays up")
                return@launch
            }
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
                    val res = RootShell.run("input keyevent 6", 5)
                    Log.i(TAG, "root KEYCODE_ENDCALL attempt=$i exit=${res.exit}")
                    delay(350)
                    if (!isSessionCurrent(generation, callInstanceId)) return@launch
                    if (!callActive) {
                        ok = true
                        break
                    }
                    if (!res.ok) {
                        Log.w(TAG, "root hangup fallback unavailable: exit=${res.exit}")
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
        var capturedCaller = ""
        var wasIdle = false
        var rangUnanswered = false
        val invalidatedGeneration = synchronized(sessionOwnershipLock) {
            val invalidated = sessionGeneration.incrementAndGet()
            endingInstance = mediaCallInstance.getAndSet(NO_CALL_INSTANCE)
            capturedCaller = (if (endingInstance != NO_CALL_INSTANCE) {
                callDetailsFor(endingInstance).callerId
            } else {
                ""
            }).ifBlank { pendingOutboundNumber.orEmpty() }
                .ifBlank { currentCallerId() }
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
        val caller = capturedCaller
        val client = oldBridge
        // Close WS off critical path but without waiting for hangup race.
        scope.launch {
            try {
                // In-call session WS (inbound + outbound audio path).
                client?.sendCallEnd(serial, caller)
                // CONTROL_IDLE path: agent callback state machine needs call_id/task_id.
                if (outboundCallId != null) {
                    sendControlOrQueue(
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
        anyCallBusy = false
        stopLocalRingtone()
        if (instance === this) instance = null
        ownerHandlingCallFlag = false
        wakeController?.releaseNow()
        wakeController = null
        stopWatchingSimChanges()
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
        if (shouldHideIdleNotification()) {
            stopForeground(STOP_FOREGROUND_REMOVE)
            nm.cancel(NOTIF_ID)
            return
        }
        val body = text ?: getString(R.string.notification_text_armed)
        try {
            nm.notify(NOTIF_ID, buildNotification(body))
        } catch (e: IllegalArgumentException) {
            // Android refuses a call style notification outside a foreground service.
            Log.w(TAG, "call style notification refused, posting the plain one: ${e.message}")
            nm.notify(NOTIF_ID, buildNotification(body, callStyle = false))
        }
    }

    /** The agent's reply to `incoming`: ring [delayMs], or decline/direct-pickup. */
    private data class AnswerPlan(
        val delayMs: Long,
        val reject: Boolean,
        val ownerPickup: Boolean = false,
        val callerName: String = "",
        val reason: String = "",
    )

    /** The plan to start with, and (Standalone, no reply in time) the reply still awaited. */
    private class AnswerPlanStart(val plan: AnswerPlan, val late: CompletableDeferred<AnswerPlan>?)

    /** What the agent is told about a call: number ("" = hidden), SIM country and subscription. */
    private data class AgentCallDetails(val callerId: String, val simCountry: String, val simSubId: Int?)

    /** How an OFFHOOK call is handled: with the AI, as the owner's own call, or not yet known. */
    internal enum class OffhookKind { AI, OWN, UNKNOWN }

    private fun buildNotification(text: String, callStyle: Boolean = true): Notification {
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
        if (!BuildConfig.STANDALONE) {
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
        val inAiCall = callActive && !ownerHandlingCallFlag
        // A call the owner dialed by hand (AI mode): shown like a call the owner took over.
        val ownCallUp = !inAiCall && !ownerHandlingCallFlag && enabled && SilentInCallService.isOwnCallInProgress()
        val ownerOnCall = ownerHandlingCallFlag || ownCallUp
        // A call in progress gets its own channel: shown with the alerting notifications
        // (expanded, buttons visible) instead of the collapsed silent section. No sound.
        val live = inAiCall || ownerOnCall
        val title = when {
            inAiCall -> standaloneString("sa_notif_ai_call_title", "AI 正在通話中")
            ownerOnCall -> standaloneString("sa_notif_owner_call_title", "通話中（本人接聽）")
            enabled -> standaloneString("sa_call_idle_title", "AI 接聽已開啟")
            else -> getString(R.string.notification_text_normal)
        }
        val callerKnown = activeTakeoverCallerName.ifBlank { activeTakeoverCaller }
            .ifBlank { currentCallerId() }
        val callerLabel = callerKnown.ifBlank { text }
        val contentText = when {
            live -> callerLabel
            enabled -> standaloneString("sa_call_idle_text", "來電會先由 AI 接聽")
            else -> getString(R.string.notification_text_normal)
        }
        val persistentIdle = Prefs.isStandalonePersistentNotif(this)
        val contentPi = if (ownerOnCall) {
            PendingIntent.getActivity(
                this,
                300,
                ownerCallIntent("in_call", callerKnown, activeTakeoverCallerName, "", ""),
                PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
            )
        } else {
            pi
        }
        val builder = NotificationCompat.Builder(this, if (live) ensureLiveCallChannel() else channelId)
            .setContentTitle(title)
            .setContentText(contentText)
            .setSmallIcon(android.R.drawable.ic_lock_silent_mode)
            .setContentIntent(contentPi)
            .setOngoing(true)
            .setSilent(true)
            .setPriority(if (live) NotificationCompat.PRIORITY_HIGH else NotificationCompat.PRIORITY_MIN)

        if (live) {
            builder.setCategory(NotificationCompat.CATEGORY_CALL)
                .setOnlyAlertOnce(true)
                .setVisibility(NotificationCompat.VISIBILITY_PUBLIC)
                // A group of its own: never bundled (and folded away) with the call summaries.
                .setGroup(LIVE_CALL_GROUP)
        }

        if (!live && !persistentIdle) {
            builder.setForegroundServiceBehavior(NotificationCompat.FOREGROUND_SERVICE_DEFERRED)
        }

        val hangupPi = serviceActionPi(ACTION_OWNER_HANGUP, 102)
        if (live && callStyle && Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
            // Android 12+: a call notification like the phone's own, kept on top and open,
            // with the system's hang-up button; "take over" is the extra button.
            val person = androidx.core.app.Person.Builder()
                .setName(callerKnown.ifBlank { title })
                .setImportant(true)
                .build()
            builder.setStyle(NotificationCompat.CallStyle.forOngoingCall(person, hangupPi))
            if (inAiCall) {
                builder.addAction(
                    android.R.drawable.ic_menu_call,
                    standaloneString("sa_btn_takeover", "我來接聽"),
                    serviceActionPi(ACTION_OWNER_TAKEOVER, 101),
                )
            }
        } else if (inAiCall) {
            builder.addAction(
                android.R.drawable.ic_menu_call,
                standaloneString("sa_btn_takeover", "我來接聽"),
                serviceActionPi(ACTION_OWNER_TAKEOVER, 101),
            )
            builder.addAction(
                android.R.drawable.ic_menu_close_clear_cancel,
                standaloneString("sa_btn_hangup", "掛斷"),
                hangupPi,
            )
        } else if (ownerOnCall) {
            builder.addAction(
                android.R.drawable.ic_menu_close_clear_cancel,
                standaloneString("sa_btn_hangup", "掛斷"),
                hangupPi,
            )
        } else if (enabled && persistentIdle) {
            builder.addAction(
                android.R.drawable.ic_menu_revert,
                standaloneString("sa_btn_switch_normal", "切為一般手機"),
                serviceActionPi(ACTION_SWITCH_TO_NORMAL, 103),
            )
        }
        return builder.build()
    }

    /** Channel of the notification while a call is in progress: high importance, silent. */
    private fun ensureLiveCallChannel(): String {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            val channel = NotificationChannel(
                LIVE_CALL_CHANNEL_ID,
                standaloneString("sa_channel_live_call", "通話中"),
                NotificationManager.IMPORTANCE_HIGH,
            ).apply {
                setSound(null, null)
                enableVibration(false)
                setShowBadge(false)
                lockscreenVisibility = Notification.VISIBILITY_PUBLIC
            }
            getSystemService(NotificationManager::class.java)?.createNotificationChannel(channel)
        }
        return LIVE_CALL_CHANNEL_ID
    }

    private fun standaloneString(name: String, fallback: String): String = try {
        val id = resources.getIdentifier(name, "string", packageName)
        if (id != 0) getString(id) else fallback
    } catch (_: Exception) {
        fallback
    }

    private fun serviceActionPi(action: String, code: Int, urgentId: String? = null): PendingIntent {
        val i = Intent(this, CallAudioService::class.java).setAction(action)
        if (!urgentId.isNullOrBlank()) i.putExtra(EXTRA_URGENT_ID, urgentId)
        val flags = PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE
        return if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            PendingIntent.getForegroundService(this, code, i, flags)
        } else {
            PendingIntent.getService(this, code, i, flags)
        }
    }

    private fun performOwnerTakeover(urgentId: String?) {
        ownerHandlingCallFlag = true
        stopLocalRingtone()
        cancelOwnerCallNotification()
        val effectiveUrgent = urgentId?.takeIf { it.isNotBlank() } ?: pendingUrgentPromptId.orEmpty()
        pendingUrgentPromptId = null
        val client = synchronized(sessionOwnershipLock) {
            localMenuPlaying = false
            localWaitPlaying = false
            dropAgentPcm = true
            localMenuJob?.cancel()
            localWaitJob?.cancel()
            captureJob?.cancel()
            bridge
        }
        injector?.let { inj ->
            injector = null
            injectorStopFuture = try {
                injectorStopExecutor.submit { try { inj.stop() } catch (_: Exception) {} }
            } catch (_: Exception) { null }
        }
        if (effectiveUrgent.isNotBlank()) {
            client?.sendJson(
                org.json.JSONObject()
                    .put("type", "add_call_result")
                    .put("id", effectiveUrgent)
                    .put("status", "merged"),
            )
        }
        client?.sendJson(
            org.json.JSONObject()
                .put("type", "owner_takeover")
                .put("serial", deviceSerial())
                .apply { if (effectiveUrgent.isNotBlank()) put("urgent_id", effectiveUrgent) },
        )
        audioGuard?.stopHoldLoop()
        audioGuard?.restoreForOwnCall()
        SilentInCallService.unmuteOwnCall()
        updateNotification()
        launchOwnerCallUi(
            mode = "in_call",
            callerId = activeTakeoverCaller.ifBlank { currentCallerId() },
            callerName = activeTakeoverCallerName,
            reason = "",
            urgentId = "",
        )
        Log.i(TAG, "owner took over live call urgentId=$effectiveUrgent")
    }

    private fun performDeclineUrgent(urgentId: String) {
        stopLocalRingtone()
        cancelOwnerCallNotification()
        val id = urgentId.ifBlank { pendingUrgentPromptId.orEmpty() }
        pendingUrgentPromptId = null
        if (id.isNotBlank()) {
            synchronized(sessionOwnershipLock) { bridge }?.sendJson(
                org.json.JSONObject()
                    .put("type", "add_call_result")
                    .put("id", id)
                    .put("status", "failed")
                    .put("reason", "owner_declined"),
            )
        }
        Log.i(TAG, "owner declined urgent prompt id=$id — AI resumes")
    }

    private fun performWhitelistAnswer() {
        answerJob?.cancel()
        answerJob = null
        answerRetryJob?.cancel()
        answerRetryJob = null
        pendingAnswerPlan?.cancel()
        pendingAnswerPlan = null
        answerScheduled = false
        stopLocalRingtone()
        cancelOwnerCallNotification()
        ownerHandlingCallFlag = true
        offhookWithoutAi = true
        audioGuard?.stopHoldLoop()
        audioGuard?.restoreForOwnCall()
        SilentInCallService.answerForOwner()
        updateNotification()
        launchOwnerCallUi(
            mode = "in_call",
            callerId = activeTakeoverCaller.ifBlank { currentCallerId() },
            callerName = activeTakeoverCallerName,
            reason = "",
            urgentId = "",
        )
    }

    private fun performWhitelistHandToAi() {
        answerJob?.cancel()
        answerJob = null
        pendingAnswerPlan?.cancel()
        pendingAnswerPlan = null
        stopLocalRingtone()
        cancelOwnerCallNotification()
        ownerHandlingCallFlag = false
        offhookWithoutAi = false
        ringGivenUp = false
        answerRejected = false
        answerScheduled = true
        audioGuard?.silenceRingingCall()
        audioGuard?.startHoldLoop()
        startControlClient()
        answerSilently()
        Log.i(TAG, "whitelist incoming handed to AI by owner")
    }

    private fun performWhitelistReject() {
        answerJob?.cancel()
        answerJob = null
        answerRetryJob?.cancel()
        answerRetryJob = null
        pendingAnswerPlan?.cancel()
        pendingAnswerPlan = null
        answerScheduled = false
        stopLocalRingtone()
        cancelOwnerCallNotification()
        ownerHandlingCallFlag = false
        rejectRingingCall(retryOnFail = false)
    }

    private fun performOwnerHangup() {
        stopLocalRingtone()
        cancelOwnerCallNotification()
        if (!SilentInCallService.disconnectActive()) {
            try {
                @Suppress("DEPRECATION")
                (getSystemService(TELECOM_SERVICE) as TelecomManager).endCall()
            } catch (_: Exception) {
            }
        }
    }

    private fun startLocalRingtone() {
        stopLocalRingtone()
        try {
            val uri = RingtoneManager.getDefaultUri(RingtoneManager.TYPE_RINGTONE) ?: return
            val ringtone = RingtoneManager.getRingtone(this, uri) ?: return
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) {
                ringtone.isLooping = true
            }
            ringtone.audioAttributes = AudioAttributes.Builder()
                .setUsage(AudioAttributes.USAGE_NOTIFICATION_RINGTONE)
                .setContentType(AudioAttributes.CONTENT_TYPE_SONIFICATION)
                .build()
            ringtone.play()
            localRingtone = ringtone
        } catch (e: Exception) {
            Log.w(TAG, "startLocalRingtone failed: ${e.message}")
        }
    }

    private fun stopLocalRingtone() {
        val r = localRingtone ?: return
        localRingtone = null
        try {
            if (r.isPlaying) r.stop()
        } catch (_: Exception) {
        }
    }

    private fun ownerCallIntent(
        mode: String,
        callerId: String,
        callerName: String,
        reason: String,
        urgentId: String,
        aiMissed: Boolean = false,
    ): Intent = Intent().apply {
        setClassName(this@CallAudioService, "com.pixelcallai.streamer.OwnerCallActivity")
        putExtra("mode", mode)
        putExtra("caller_id", callerId)
        putExtra("caller_name", callerName)
        putExtra("reason", reason)
        putExtra("urgent_id", urgentId)
        putExtra("ai_missed", aiMissed)
        addFlags(
            Intent.FLAG_ACTIVITY_NEW_TASK or
                Intent.FLAG_ACTIVITY_SINGLE_TOP or
                Intent.FLAG_ACTIVITY_CLEAR_TOP,
        )
    }

    private fun launchOwnerCallUi(
        mode: String,
        callerId: String,
        callerName: String,
        reason: String,
        urgentId: String,
        aiMissed: Boolean = false,
    ) {
        try {
            startActivity(ownerCallIntent(mode, callerId, callerName, reason, urgentId, aiMissed))
        } catch (e: Exception) {
            Log.w(TAG, "launchOwnerCallUi failed: ${e.message}")
        }
    }

    private fun notifyAndLaunchOwnerUi(
        mode: String,
        callerId: String,
        callerName: String,
        reason: String,
        urgentId: String,
        aiMissed: Boolean = false,
    ) {
        val nm = getSystemService(NotificationManager::class.java)
        val chId = "bageo_owner_calls"
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            nm.createNotificationChannel(
                NotificationChannel(
                    chId,
                    standaloneString("sa_channel_owner_calls", "重要來電與急事接聽"),
                    NotificationManager.IMPORTANCE_HIGH,
                ),
            )
        }
        val intent = ownerCallIntent(mode, callerId, callerName, reason, urgentId, aiMissed)
        val fullScreenPi = PendingIntent.getActivity(
            this,
            200,
            intent,
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
        val display = callerName.ifBlank { callerId }.ifBlank { "來電" }
        val titlePrefix = when {
            aiMissed -> standaloneString("sa_ai_missed_title", "AI 沒接起來")
            mode == "urgent_prompt" -> standaloneString("sa_notif_urgent_title", "緊急來電")
            else -> standaloneString("sa_notif_whitelist_title", "重要來電")
        }
        val title = "$titlePrefix: $display"
        val body = when {
            aiMissed -> standaloneString("sa_ai_missed_ringing_text", "已改成響鈴，請你自己接")
            mode == "urgent_prompt" && reason.isNotBlank() -> reason
            else -> callerId.ifBlank { display }
        }
        val b = NotificationCompat.Builder(this, chId)
            .setSmallIcon(android.R.drawable.sym_call_incoming)
            .setContentTitle(title)
            .setContentText(body)
            .setStyle(NotificationCompat.BigTextStyle().bigText(body))
            .setPriority(NotificationCompat.PRIORITY_MAX)
            .setCategory(NotificationCompat.CATEGORY_CALL)
            .setFullScreenIntent(fullScreenPi, true)
            .setContentIntent(fullScreenPi)
            .setAutoCancel(false)
            .setOngoing(true)
        if (mode == "urgent_prompt") {
            b.addAction(
                android.R.drawable.ic_menu_call,
                standaloneString("sa_btn_takeover_now", "立刻接手"),
                serviceActionPi(ACTION_OWNER_TAKEOVER, 201, urgentId),
            )
            b.addAction(
                android.R.drawable.ic_menu_close_clear_cancel,
                standaloneString("sa_btn_let_ai_handle", "讓 AI 請他留話"),
                serviceActionPi(ACTION_DECLINE_URGENT, 202, urgentId),
            )
        } else {
            b.addAction(
                android.R.drawable.ic_menu_call,
                standaloneString("sa_btn_answer", "接聽"),
                serviceActionPi(ACTION_WHITELIST_ANSWER, 203),
            )
            // The AI just could not take this call: no "hand to AI" button then.
            if (!aiMissed) {
                b.addAction(
                    android.R.drawable.ic_menu_manage,
                    standaloneString("sa_btn_hand_ai", "交給 AI"),
                    serviceActionPi(ACTION_WHITELIST_HAND_AI, 204),
                )
            }
            b.addAction(
                android.R.drawable.ic_menu_close_clear_cancel,
                standaloneString("sa_btn_hangup", "掛斷"),
                serviceActionPi(ACTION_WHITELIST_REJECT, 205),
            )
        }
        nm.notify(NOTIF_OWNER_CALL_ID, b.build())
        launchOwnerCallUi(mode, callerId, callerName, reason, urgentId, aiMissed)
    }

    private fun cancelOwnerCallNotification() {
        try {
            getSystemService(NotificationManager::class.java)?.cancel(NOTIF_OWNER_CALL_ID)
        } catch (_: Exception) {
        }
    }

    companion object {
        const val ACTION_START = "com.pixelcallai.streamer.START"
        const val ACTION_STOP = "com.pixelcallai.streamer.STOP"
        const val ACTION_SET_ENABLED = "com.pixelcallai.streamer.SET_ENABLED"
        const val ACTION_SMOKE_CONNECT = "com.pixelcallai.streamer.SMOKE_CONNECT"
        const val ACTION_CALL_RINGING = "com.pixelcallai.streamer.CALL_RINGING"
        const val ACTION_CALL_ACTIVE = "com.pixelcallai.streamer.CALL_ACTIVE"
        const val ACTION_CALL_ENDED = "com.pixelcallai.streamer.CALL_ENDED"
        const val ACTION_OWNER_TAKEOVER = "com.pixelcallai.streamer.OWNER_TAKEOVER"
        const val ACTION_DECLINE_URGENT = "com.pixelcallai.streamer.DECLINE_URGENT"
        const val ACTION_WHITELIST_ANSWER = "com.pixelcallai.streamer.WHITELIST_ANSWER"
        const val ACTION_WHITELIST_HAND_AI = "com.pixelcallai.streamer.WHITELIST_HAND_AI"
        const val ACTION_WHITELIST_REJECT = "com.pixelcallai.streamer.WHITELIST_REJECT"
        const val ACTION_OWNER_HANGUP = "com.pixelcallai.streamer.OWNER_HANGUP"
        const val ACTION_REFRESH_NOTIF = "com.pixelcallai.streamer.REFRESH_NOTIF"
        const val ACTION_SWITCH_TO_NORMAL = "com.pixelcallai.streamer.SWITCH_TO_NORMAL"
        /** Standalone normal phone: the console came on screen ([onConsoleVisible]). */
        const val ACTION_CONSOLE_VISIBLE = "com.pixelcallai.streamer.CONSOLE_VISIBLE"
        const val EXTRA_URGENT_ID = "urgent_id"
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
        internal const val RING_SIGNAL_GRACE_MS = 1800L
        private const val NOTIF_ID = 42
        private const val NOTIF_INFO_ID = 43
        private const val NOTIF_OWNER_CALL_ID = 44
        private const val NOTIF_AI_MISSED_ID = 45
        private const val LIVE_CALL_CHANNEL_ID = "bageo_live_call"
        /** Group of the call notification alone, so Android never bundles it with the summaries. */
        private const val LIVE_CALL_GROUP = "bageo_live_call"
        /** Standalone normal phone: the console link stays up this long after the console left the screen. */
        internal const val CONSOLE_LINK_LINGER_MS = 120_000L
        /** Console gone but the link still needed (AI call in progress): look again this often. */
        private const val CONSOLE_LINK_RECHECK_MS = 15_000L
        private const val AI_MISSED_CHANNEL_ID = "bageo_ai_missed"
        private const val NOTIF_SUMMARY_BASE_ID = 1000
        private val nextSummaryNotifId = java.util.concurrent.atomic.AtomicInteger(0)

        @Volatile
        private var instance: CallAudioService? = null

        @Volatile
        private var ownerHandlingCallFlag = false

        fun isOwnerHandlingCall(): Boolean = BuildConfig.STANDALONE && ownerHandlingCallFlag

        /** The console is on screen ([onConsoleVisible] / [onConsoleHidden]). */
        @Volatile
        private var consoleVisible = false

        /** [SystemClock.elapsedRealtime] until which the link outlives the console; 0 = not lingering. */
        @Volatile
        private var consoleLinkUntil = 0L

        /**
         * Standalone: the console came on screen (the app's onResume). A normal phone then
         * connects to Bageo so AI calls can be dialed and text replies sent; nothing is
         * muted, the phone app role and the mode stay as they are. AI mode is connected anyway.
         */
        fun onConsoleVisible(ctx: Context) {
            if (!BuildConfig.STANDALONE) return
            consoleVisible = true
            consoleLinkUntil = 0L
            if (Prefs.isAiCallEnabled(ctx)) return
            val app = ctx.applicationContext
            try {
                // The app is in the foreground: a plain start, no notification.
                app.startService(Intent(app, CallAudioService::class.java).setAction(ACTION_CONSOLE_VISIBLE))
            } catch (e: Exception) {
                Log.w(TAG, "console link start failed: ${e.message}")
            }
        }

        /**
         * Standalone: the console left the screen (the app's onPause). A normal phone keeps
         * the link [CONSOLE_LINK_LINGER_MS] longer, and after that until the AI call in
         * progress has ended.
         */
        fun onConsoleHidden(ctx: Context) {
            if (!BuildConfig.STANDALONE) return
            consoleVisible = false
            consoleLinkUntil = SystemClock.elapsedRealtime() + CONSOLE_LINK_LINGER_MS
            if (Prefs.isAiCallEnabled(ctx)) return
            val svc = instance ?: return
            svc.mainHandler.post { svc.scheduleConsoleLinkCheck(CONSOLE_LINK_LINGER_MS) }
        }

        /**
         * Standalone AI mode: the owner dialed a call by hand (from any app). Open the call
         * screen (number, time, speaker, mute, keypad, hang up) and turn the call
         * notification into the owner's call. Runs on [SilentInCallService]'s thread,
         * which Android lets start a screen while it hosts a call.
         */
        fun showOwnCallScreen(ctx: Context, number: String) {
            if (!BuildConfig.STANDALONE) return
            val intent = Intent().apply {
                setClassName(ctx, "com.pixelcallai.streamer.OwnerCallActivity")
                putExtra("mode", "in_call")
                putExtra("caller_id", number)
                putExtra("caller_name", "")
                putExtra("reason", "")
                putExtra("urgent_id", "")
                putExtra("ai_missed", false)
                addFlags(
                    Intent.FLAG_ACTIVITY_NEW_TASK or
                        Intent.FLAG_ACTIVITY_SINGLE_TOP or
                        Intent.FLAG_ACTIVITY_CLEAR_TOP,
                )
            }
            try {
                ctx.startActivity(intent)
                Log.i(TAG, "own call: call screen opened")
            } catch (e: Exception) {
                Log.w(TAG, "own call: call screen failed: ${e.message}")
            }
            instance?.let { svc -> svc.mainHandler.post { svc.updateNotification() } }
        }

        fun isLiveAiCallActive(): Boolean =
            BuildConfig.STANDALONE && (instance?.let { it.callActive && !ownerHandlingCallFlag } == true)

        /**
         * Combine legacy `TelephonyManager.callState` with direct [SilentInCallService]
         * Telecom state so VoWiFi / secondary eSIM calls where `TelephonyManager.callState`
         * lags or stays `CALL_STATE_IDLE` are still recognized as `RINGING` or `OFFHOOK`.
         */
        internal fun resolveEffectiveCallState(tmState: Int?, telecomState: Int?): Int? {
            if (tmState == TelephonyManager.CALL_STATE_OFFHOOK || tmState == TelephonyManager.CALL_STATE_RINGING) {
                return tmState
            }
            if (telecomState == TelephonyManager.CALL_STATE_OFFHOOK || telecomState == TelephonyManager.CALL_STATE_RINGING) {
                return telecomState
            }
            return tmState ?: telecomState
        }

        internal fun shouldRecoverRingingInWatchdog(
            answerScheduled: Boolean,
            callActive: Boolean,
            ringGivenUp: Boolean,
            ownerHandlingCall: Boolean,
        ): Boolean = !answerScheduled && !callActive && !ringGivenUp && !ownerHandlingCall

        /**
         * An IDLE from TelephonyManager that must not end the call handling: Telecom still
         * reports a ringing or connected call, or (no call active yet) an answer is
         * scheduled, Telecom has no call yet, and the ring signal came less than
         * [RING_SIGNAL_GRACE_MS] ago. [sinceRingSignalMs] is Long.MAX_VALUE without a signal.
         */
        internal fun shouldHoldIdleForRing(
            callActive: Boolean,
            answerScheduled: Boolean,
            telecomState: Int?,
            telecomHasCalls: Boolean,
            sinceRingSignalMs: Long,
        ): Boolean {
            if (telecomState == TelephonyManager.CALL_STATE_RINGING ||
                telecomState == TelephonyManager.CALL_STATE_OFFHOOK
            ) {
                return true
            }
            if (callActive || !answerScheduled || telecomHasCalls) return false
            return sinceRingSignalMs in 0 until RING_SIGNAL_GRACE_MS
        }

        private fun startCallServiceCompat(ctx: Context, intent: Intent) {
            val app = ctx.applicationContext
            try {
                if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
                    app.startForegroundService(intent)
                } else {
                    app.startService(intent)
                }
            } catch (e: Exception) {
                Log.w(TAG, "startForegroundService(${intent.action}) failed: ${e.message}")
                try {
                    app.startService(intent)
                } catch (_: Exception) {
                }
            }
        }

        fun requestOwnerTakeover(ctx: Context, urgentId: String? = null) {
            if (!BuildConfig.STANDALONE) return
            val i = Intent(ctx, CallAudioService::class.java).setAction(ACTION_OWNER_TAKEOVER)
            if (!urgentId.isNullOrBlank()) i.putExtra(EXTRA_URGENT_ID, urgentId)
            startCallServiceCompat(ctx, i)
        }

        fun declineUrgentPrompt(ctx: Context, urgentId: String) {
            if (!BuildConfig.STANDALONE) return
            startCallServiceCompat(
                ctx,
                Intent(ctx, CallAudioService::class.java)
                    .setAction(ACTION_DECLINE_URGENT)
                    .putExtra(EXTRA_URGENT_ID, urgentId),
            )
        }

        fun answerWhitelistForOwner(ctx: Context) {
            if (!BuildConfig.STANDALONE) return
            startCallServiceCompat(ctx, Intent(ctx, CallAudioService::class.java).setAction(ACTION_WHITELIST_ANSWER))
        }

        fun handWhitelistToAi(ctx: Context) {
            if (!BuildConfig.STANDALONE) return
            startCallServiceCompat(ctx, Intent(ctx, CallAudioService::class.java).setAction(ACTION_WHITELIST_HAND_AI))
        }

        fun rejectWhitelistForOwner(ctx: Context) {
            if (!BuildConfig.STANDALONE) return
            startCallServiceCompat(ctx, Intent(ctx, CallAudioService::class.java).setAction(ACTION_WHITELIST_REJECT))
        }

        fun hangupActiveCall(ctx: Context) {
            if (!BuildConfig.STANDALONE) return
            startCallServiceCompat(ctx, Intent(ctx, CallAudioService::class.java).setAction(ACTION_OWNER_HANGUP))
        }

        fun refreshNotification(ctx: Context) {
            if (!BuildConfig.STANDALONE) return
            if (!Prefs.isAiCallEnabled(ctx)) return
            startCallServiceCompat(ctx, Intent(ctx, CallAudioService::class.java).setAction(ACTION_REFRESH_NOTIF))
        }

        fun ensureArmed(ctx: Context) {
            val app = ctx.applicationContext
            val svc = Intent(app, CallAudioService::class.java).setAction(ACTION_START)
            try {
                if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
                    app.startForegroundService(svc)
                } else {
                    app.startService(svc)
                }
            } catch (e: Exception) {
                Log.w(TAG, "ensureArmed failed: ${e.message}")
                try {
                    app.startService(svc)
                } catch (_: Exception) {
                }
            }
        }

        fun showStandaloneSummaryNotification(ctx: Context, notice: org.json.JSONObject) {
            if (!BuildConfig.STANDALONE || !Prefs.isStandaloneCallSummaryNotif(ctx)) return
            val nm = ctx.getSystemService(NotificationManager::class.java) ?: return
            val chId = "bageo_call_summaries"
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
                val chNameId = ctx.resources.getIdentifier("sa_channel_summaries", "string", ctx.packageName)
                val chName = if (chNameId != 0) ctx.getString(chNameId) else "AI 通話摘要"
                nm.createNotificationChannel(
                    NotificationChannel(chId, chName, NotificationManager.IMPORTANCE_DEFAULT),
                )
            }
            val callId = notice.optString("call_id").trim()
            val callerId = notice.optString("caller_id").trim()
            val title = notice.optString("title").ifBlank { callerId.ifBlank { "AI 通話摘要" } }
            val body = notice.optString("body").trim()
            if (body.isBlank()) return
            val notifId = NOTIF_SUMMARY_BASE_ID + (nextSummaryNotifId.getAndIncrement() % 200)
            val openIntent = Intent(ctx, MainActivity::class.java).apply {
                if (callId.isNotBlank()) putExtra("open_call_id", callId)
                addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_SINGLE_TOP)
            }
            val openPi = PendingIntent.getActivity(
                ctx,
                notifId,
                openIntent,
                PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
            )
            val builder = NotificationCompat.Builder(ctx, chId)
                .setSmallIcon(android.R.drawable.sym_action_chat)
                .setContentTitle(title)
                .setContentText(body.lineSequence().firstOrNull().orEmpty())
                .setStyle(NotificationCompat.BigTextStyle().bigText(body))
                .setContentIntent(openPi)
                .setAutoCancel(true)
            if (callerId.isNotBlank()) {
                val callbackLabelId = ctx.resources.getIdentifier("sa_btn_callback", "string", ctx.packageName)
                val callbackLabel = if (callbackLabelId != 0) ctx.getString(callbackLabelId) else "回撥"
                val dialIntent = Intent().apply {
                    setClassName(ctx, "com.pixelcallai.streamer.OwnerCallActivity")
                    putExtra("mode", "dialer")
                    putExtra("dial_number", callerId)
                    addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_SINGLE_TOP)
                }
                val dialPi = PendingIntent.getActivity(
                    ctx,
                    notifId + 10_000,
                    dialIntent,
                    PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
                )
                builder.addAction(android.R.drawable.ic_menu_call, callbackLabel, dialPi)
            }
            nm.notify(notifId, builder.build())
        }
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
        /** After the last answer attempt: still ringing this much later → the call goes to the owner. */
        private const val ANSWER_GIVE_UP_MS = 1_500L
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
        private const val REJECT_RETRY_MS = 3_000L
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

        @Volatile
        private var anyCallBusy = false

        /** True while RINGING, OFFHOOK, or outbound dial is in progress (used to mute console preview audio). */
        fun isAnyCallBusy(): Boolean = anyCallBusy

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

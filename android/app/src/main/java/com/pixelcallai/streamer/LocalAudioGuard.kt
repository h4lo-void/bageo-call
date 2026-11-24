// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

package com.pixelcallai.streamer

import android.app.NotificationManager
import android.content.Context
import android.media.AudioAttributes
import android.media.AudioFocusRequest
import android.media.AudioManager
import android.os.Build
import android.os.Handler
import android.os.HandlerThread
import android.os.Looper
import android.os.Process
import android.os.SystemClock
import android.telecom.TelecomManager
import android.util.Log
import java.util.concurrent.ConcurrentLinkedQueue
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicInteger
import java.util.concurrent.atomic.AtomicLong

/**
 * Non-blocking facade for the process-wide local audio guard.
 *
 * Android audio Binder calls can block while audioserver is restarting. Every caller,
 * including CallAudioService, InCallService, and CallScreeningService, therefore only
 * posts an intent to one serialized worker. The screening service runs in its own
 * process and naturally gets a separate coordinator.
 */
class LocalAudioGuard(context: Context) {
    private val coordinator = coordinator(context.applicationContext)

    /** Queue a hard mute and return without waiting for AudioManager. */
    fun applyHardMute() = coordinator.requestHardMute(fromHold = false, rootPass = true)

    /**
     * Immediately silence an incoming AI call, even before [CallAudioService] marks the
     * call busy. Standalone Daily Sound: only this call is silenced (the ringer, ring and
     * notification volumes are not touched) and everything changed for the call is put
     * back by the [restore] at the end of the call.
     */
    fun silenceRingingCall() =
        coordinator.requestHardMute(fromHold = false, rootPass = true, forceCallMute = true)

    fun startHoldLoop() = coordinator.startHoldLoop()

    fun stopHoldLoop() = coordinator.stopHoldLoop()

    /** Queue restore behind any active audio Binder call. */
    fun restore(onFinished: (() -> Unit)? = null) = coordinator.requestRestore(onFinished)

    /**
     * A call the owner dialed by hand in AI mode: [restore], then also take back the
     * root microphone mute and raise the earpiece from its minimum, so the owner can
     * hear and be heard. The next [applyHardMute] (call ended) mutes everything again.
     */
    fun restoreForOwnCall() = coordinator.requestRestoreForOwnCall()

    /**
     * Normal phone mode: stop muting and forget the state saved when muting began, so
     * a later [restore] cannot put the AI-mode values back. [PhoneModeManager] sets the
     * normal volumes itself.
     */
    fun release() = coordinator.requestRelease()

    /**
     * Standalone app foreground preview: temporarily allow media playback (`STREAM_MUSIC`)
     * for voice/ambience previews and call recordings when idle; immediately re-muted when
     * backgrounded or when any call starts.
     */
    fun setConsolePreviewAllowed(allowed: Boolean) = coordinator.requestConsolePreview(allowed)

    fun refreshConsolePreview() = coordinator.refreshConsolePreviewIfAllowed()

    /**
     * The guard has saved the phone's audio state and changed it (it mutes now, or a call
     * ended without a restore yet). Volumes read now are not the owner's own.
     */
    fun isMutingNow(): Boolean = coordinator.isArmed()

    companion object {
        const val HOLD_INTERVAL_MS = 500L

        /**
         * Android 15 and later: setInterruptionFilter() from an app no longer changes the
         * phone's Do Not Disturb. It turns on a mode of the app's own ("Do Not Disturb
         * (Bageo)" in Settings > Modes) that the module's `cmd notification set_dnd off`
         * cannot turn off, so switching to a normal phone failed with `dnd_failed`. There
         * the module's Do Not Disturb (root, the phone's own) is the only one, and the app
         * just turns its own mode off.
         */
        fun appHasOwnDndMode(): Boolean =
            Build.VERSION.SDK_INT >= Build.VERSION_CODES.VANILLA_ICE_CREAM

        /**
         * Android 15+: turn off Bageo's own Do Not Disturb mode, including one an older
         * version left on. Only that mode: the phone's own Do Not Disturb is not touched.
         * Does nothing on older Android.
         */
        fun turnOffOwnDndMode(context: Context) {
            if (!appHasOwnDndMode()) return
            try {
                context.getSystemService(NotificationManager::class.java)
                    ?.setInterruptionFilter(NotificationManager.INTERRUPTION_FILTER_ALL)
                Log.i("LocalAudioGuard", "own Do Not Disturb mode off")
            } catch (e: Exception) {
                Log.w("LocalAudioGuard", "own Do Not Disturb mode off failed: ${e.message}")
            }
        }

        private val coordinatorLock = Any()

        @Volatile
        private var sharedCoordinator: LocalAudioGuardCoordinator? = null

        private fun coordinator(context: Context): LocalAudioGuardCoordinator {
            sharedCoordinator?.let { return it }
            return synchronized(coordinatorLock) {
                sharedCoordinator ?: LocalAudioGuardCoordinator(context).also {
                    sharedCoordinator = it
                }
            }
        }
    }
}

/** Owns all mutable audio state and is the only class that calls AudioManager. */
private class LocalAudioGuardCoordinator(context: Context) {
    private val appContext = context.applicationContext
    private val am = appContext.getSystemService(Context.AUDIO_SERVICE) as AudioManager
    private val nm = appContext.getSystemService(NotificationManager::class.java)
    private val mainHandler = Handler(Looper.getMainLooper())
    private val workerThread = HandlerThread(
        "local-audio-guard",
        Process.THREAD_PRIORITY_BACKGROUND,
    ).apply { start() }
    private val workerHandler = Handler(workerThread.looper)

    // Request-side state. Public methods only touch atomics and Handler queues.
    private val desiredMuted = AtomicBoolean(false)
    private val mutePending = AtomicBoolean(false)
    private val fullPassPending = AtomicBoolean(false)
    private val rootPassPending = AtomicBoolean(false)
    private val forceCallMutePending = AtomicBoolean(false)
    private val restorePending = AtomicBoolean(false)
    private val ownCallPending = AtomicBoolean(false)
    private val releasePending = AtomicBoolean(false)
    private val consolePreviewAllowed = AtomicBoolean(false)
    private val previewPending = AtomicBoolean(false)
    private val drainScheduled = AtomicBoolean(false)
    private val restoreCallbacks = ConcurrentLinkedQueue<() -> Unit>()

    private val holdActive = AtomicBoolean(false)
    private val holdTick = AtomicInteger(0)
    private val operationSequence = AtomicLong(0)
    private val activeOperation = AtomicLong(0)

    // Worker-only state. Never read or written from a service/main callback.
    private var savedMicMute: Boolean? = null
    private var savedSpeaker: Boolean? = null
    private var savedMode: Int? = null
    private var savedRinger: Int? = null
    private var savedInterruption: Int? = null
    private val savedVolumes = mutableMapOf<Int, Int>()
    private var armed = false
    /** Armed in Standalone Daily Sound mode: only the call's own changes are put back. */
    private var armedDaily = false
    /** [armed], readable from any thread. */
    private val armedFlag = AtomicBoolean(false)
    private var hardMuteRuns = 0L

    private val holdRunnable = object : Runnable {
        override fun run() {
            if (!holdActive.get()) return
            val tick = holdTick.incrementAndGet()
            // Hold ticks only re-assert mic/Telecom mute. Full local stream controls
            // were captured on first arm and must not race every AudioTrack create.
            requestHardMute(fromHold = true, rootPass = tick % ROOT_PASS_TICKS == 0)
            mainHandler.postDelayed(this, LocalAudioGuard.HOLD_INTERVAL_MS)
        }
    }

    fun requestHardMute(fromHold: Boolean, rootPass: Boolean, forceCallMute: Boolean = false) {
        // An ordinary phone (normal mode) is never muted.
        if (!Prefs.isAiCallEnabled(appContext)) return
        // Nor one on its way to a normal phone (the app's copy says AI until the switch ends).
        if (PhoneModeManager.isSwitchingTo(Prefs.PHONE_MODE_NORMAL)) return
        // Nor is a call the owner dialed by hand (AI mode): they are talking on it.
        if (SilentInCallService.isOwnCallInProgress()) return
        desiredMuted.set(true)
        if (!fromHold) fullPassPending.set(true)
        if (rootPass) rootPassPending.set(true)
        if (forceCallMute) forceCallMutePending.set(true)
        mutePending.set(true)
        scheduleDrain()
    }

    fun isArmed(): Boolean = armedFlag.get()

    fun startHoldLoop() {
        if (holdActive.getAndSet(true)) return
        holdTick.set(0)
        requestHardMute(fromHold = false, rootPass = true, forceCallMute = true)
        mainHandler.removeCallbacks(holdRunnable)
        mainHandler.postDelayed(holdRunnable, LocalAudioGuard.HOLD_INTERVAL_MS)
        Log.i(TAG, "mute hold loop started intervalMs=${LocalAudioGuard.HOLD_INTERVAL_MS}")
    }

    fun stopHoldLoop() {
        if (!holdActive.getAndSet(false)) return
        mainHandler.removeCallbacks(holdRunnable)
        Log.i(TAG, "mute hold loop stopped")
    }

    fun requestRestore(onFinished: (() -> Unit)?) {
        stopHoldLoop()
        onFinished?.let { restoreCallbacks.add(it) }
        // A queued mute that has not started is stale after restore. If a fresh arm
        // arrives, it sets desiredMuted/pending again and runs after restore.
        desiredMuted.set(false)
        mutePending.set(false)
        fullPassPending.set(false)
        rootPassPending.set(false)
        forceCallMutePending.set(false)
        restorePending.set(true)
        scheduleDrain()
    }

    fun requestRestoreForOwnCall() {
        ownCallPending.set(true)
        requestRestore(null)
    }

    fun requestRelease() {
        stopHoldLoop()
        desiredMuted.set(false)
        mutePending.set(false)
        fullPassPending.set(false)
        rootPassPending.set(false)
        forceCallMutePending.set(false)
        restorePending.set(false)
        releasePending.set(true)
        scheduleDrain()
    }

    fun requestConsolePreview(allowed: Boolean) {
        consolePreviewAllowed.set(allowed)
        previewPending.set(true)
        scheduleDrain()
    }

    fun refreshConsolePreviewIfAllowed() {
        if (!consolePreviewAllowed.get()) return
        previewPending.set(true)
        scheduleDrain()
    }

    private fun scheduleDrain() {
        if (!drainScheduled.compareAndSet(false, true)) return
        if (!workerHandler.post(drainRunnable)) {
            drainScheduled.set(false)
            Log.e(TAG, "failed to schedule audio guard worker")
        }
    }

    private val drainRunnable = Runnable { drainRequests() }

    private fun drainRequests() {
        try {
            while (true) {
                if (releasePending.getAndSet(false)) {
                    runTrackedOperation("release") { forgetSavedState() }
                    // A service stop waiting for a restore may finish now.
                    completeRestoreCallbacks()
                    continue
                }
                if (restorePending.getAndSet(false)) {
                    runTrackedOperation("restore") { restoreBlocking() }
                    if (ownCallPending.getAndSet(false)) {
                        runTrackedOperation("own-call") { unmuteForOwnCallBlocking() }
                    }
                    completeRestoreCallbacks()
                    if (desiredMuted.get()) mutePending.set(true)
                    continue
                }
                if (previewPending.getAndSet(false)) {
                    runTrackedOperation("preview-audio") { applyConsolePreviewBlocking() }
                    continue
                }

                if (!desiredMuted.get()) {
                    mutePending.set(false)
                    fullPassPending.set(false)
                    rootPassPending.set(false)
                    forceCallMutePending.set(false)
                    break
                }
                if (!mutePending.getAndSet(false)) break

                val fullPass = fullPassPending.getAndSet(false)
                val rootPass = rootPassPending.getAndSet(false) || fullPass
                val forceCallMute = forceCallMutePending.getAndSet(false)
                // Restore wins over a mute that was queued but not yet started.
                if (!desiredMuted.get() || restorePending.get()) continue
                runTrackedOperation("hard-mute") {
                    applyHardMuteBlocking(
                        fromHold = !fullPass,
                        rootPass = rootPass,
                        forceCallMute = forceCallMute,
                    )
                }
            }
        } finally {
            drainScheduled.set(false)
            if (
                releasePending.get() ||
                restorePending.get() ||
                previewPending.get() ||
                (desiredMuted.get() && mutePending.get())
            ) {
                scheduleDrain()
            }
        }
    }

    private inline fun runTrackedOperation(name: String, block: () -> Unit) {
        val operationId = operationSequence.incrementAndGet()
        val startedAt = SystemClock.elapsedRealtime()
        activeOperation.set(operationId)
        mainHandler.postDelayed(
            {
                if (activeOperation.get() == operationId) {
                    Log.e(
                        TAG,
                        "$name audio worker blocked longer than ${AUDIO_OPERATION_WARN_MS}ms; " +
                            "service main thread remains free",
                    )
                }
            },
            AUDIO_OPERATION_WARN_MS,
        )
        try {
            block()
        } finally {
            activeOperation.compareAndSet(operationId, 0)
            val elapsed = SystemClock.elapsedRealtime() - startedAt
            if (elapsed >= AUDIO_OPERATION_WARN_MS) {
                Log.w(TAG, "$name audio worker recovered elapsedMs=$elapsed")
            }
        }
    }

    private fun completeRestoreCallbacks() {
        if (restoreCallbacks.isEmpty()) return
        val callbacks = mutableListOf<() -> Unit>()
        while (true) {
            callbacks.add(restoreCallbacks.poll() ?: break)
        }
        mainHandler.post {
            callbacks.forEach { callback ->
                try {
                    callback()
                } catch (t: Throwable) {
                    Log.e(TAG, "restore callback failed", t)
                }
            }
        }
    }

    private fun applyHardMuteBlocking(fromHold: Boolean, rootPass: Boolean, forceCallMute: Boolean = false) {
        // Queued before the phone switched to normal mode or while owner took over the call: drop it.
        if (!Prefs.isAiCallEnabled(appContext)) return
        if (BuildConfig.STANDALONE && CallAudioService.isOwnerHandlingCall()) return
        val dailySound = BuildConfig.STANDALONE && Prefs.isStandaloneDailySound(appContext)
        val callSeen =
            holdActive.get() ||
                CallAudioService.isAnyCallBusy() ||
                SilentInCallService.hasRingingOrLiveAiCall()
        // Daily Sound needs a call that is really there: a late request (sent while a call
        // rang, run after it ended) must not leave the microphone muted until the next call.
        val callInProgress = callSeen || (forceCallMute && !dailySound)
        if (dailySound && !callSeen) {
            // Daily Sound mode keeps idle ringtone, notification, media, and alarm audio untouched.
            return
        }
        var firstArm = false
        if (!armed) {
            try {
                savedMicMute = am.isMicrophoneMute
            } catch (_: Exception) {
            }
            try {
                @Suppress("DEPRECATION")
                savedSpeaker = am.isSpeakerphoneOn
            } catch (_: Exception) {
            }
            try {
                savedMode = am.mode
            } catch (_: Exception) {
            }
            try {
                savedRinger = am.ringerMode
            } catch (_: Exception) {
            }
            try {
                savedInterruption = nm?.currentInterruptionFilter
            } catch (_: Exception) {
            }
            for (stream in TRACKED_STREAMS) {
                try {
                    savedVolumes[stream] = am.getStreamVolume(stream)
                } catch (_: Exception) {
                }
            }
            armed = true
            armedDaily = dailySound
            armedFlag.set(true)
            firstArm = true
        }

        // Android can clear mic mute on answer/MODE_IN_CALL, so this is the only
        // AudioManager operation repeated by the 500 ms hold loop.
        try {
            am.isMicrophoneMute = true
        } catch (e: Exception) {
            Log.w(TAG, "setMicrophoneMute failed: ${e.message}")
        }

        // Strongest call-path mic mute while this app is the dialer.
        SilentInCallService.forceMuteActive()

        // Speakerphone is an edge operation, not a hold-loop operation.
        if (!fromHold) {
            try {
                @Suppress("DEPRECATION")
                am.isSpeakerphoneOn = false
            } catch (e: Exception) {
                Log.w(TAG, "isSpeakerphoneOn=false failed: ${e.message}")
            }
            ensureEarpieceSilent()
            if (dailySound) silenceThisRing()
        }

        if (dailySound) {
            // Only this call is silenced: Bageo's phone app does not ring for it, the call
            // screener asked for silence, and [silenceThisRing] covers the rest. The ringer
            // mode and the ring, notification, and media volumes stay as the owner set them.
        } else if (firstArm) {
            // setStreamMute() is intentionally forbidden here. On Android 16 it can
            // block in AudioService for 20s after an audioserver createTrack timeout.
            // Volume zero + silent ringer + DND provide the same local-hear guard.
            for (stream in LOCAL_ZERO_STREAMS) {
                if (stream == AudioManager.STREAM_MUSIC && consolePreviewAllowed.get() && !callInProgress) {
                    continue
                }
                try {
                    am.setStreamVolume(stream, 0, 0)
                } catch (e: Exception) {
                    Log.w(TAG, "setStreamVolume($stream)=0 failed: ${e.message}")
                    tryZeroViaSettings(stream)
                }
            }

            try {
                am.ringerMode = AudioManager.RINGER_MODE_SILENT
            } catch (e: Exception) {
                Log.w(TAG, "RINGER_MODE_SILENT failed: ${e.message}")
            }

            applyZenSilence()
            abandonFocus()
        } else if (callInProgress) {
            if (!fromHold) {
                try {
                    if (am.getStreamVolume(AudioManager.STREAM_RING) != 0) {
                        am.setStreamVolume(AudioManager.STREAM_RING, 0, 0)
                        tryZeroViaSettings(AudioManager.STREAM_RING)
                    }
                } catch (_: Exception) {
                }
            }
            // Ensure preview media audio is silenced immediately once a call starts.
            try {
                if (am.getStreamVolume(AudioManager.STREAM_MUSIC) != 0) {
                    am.setStreamVolume(AudioManager.STREAM_MUSIC, 0, 0)
                    tryZeroViaSettings(AudioManager.STREAM_MUSIC)
                }
            } catch (_: Exception) {
            }
        }

        if (rootPass && !dailySound) applyRootForceMute()

        hardMuteRuns++
        if (!fromHold) {
            Log.i(
                TAG,
                "hard-mute queued worker applied firstArm=$firstArm runs=$hardMuteRuns dailySound=$dailySound " +
                    "(mic+telecom mute, speaker off, local volumes zero, " +
                    "VOICE_CALL earpiece at min; TYPE_TELEPHONY inject is fixed gain)",
            )
        }
    }

    /**
     * Daily Sound: stop the ringtone of the ringing call only, if anything plays it
     * (Bageo's phone app does not; this covers a moment Bageo is not the phone app).
     * Needs the phone app role; refused otherwise, which is fine.
     */
    private fun silenceThisRing() {
        try {
            appContext.getSystemService(TelecomManager::class.java)?.silenceRinger()
        } catch (e: Exception) {
            Log.w(TAG, "silenceRinger failed: ${e.message}")
        }
    }

    /**
     * A dedicated phone never plays or records anything locally: no speaker, no
     * mic, and the earpiece stays at its minimum index so anyone near the phone
     * hears nothing (pinning it to max leaked the caller and the AI out of the
     * earpiece). Pixel VOICE_DOWNLINK capture sits after this volume, so the far
     * end reaches the agent quiet (≈ −40 dBFS, RMS ~100–500); the agent normalizes
     * it (AGC for the AI input, recordings, VAD). TYPE_TELEPHONY inject is a
     * fixed-volume device in AudioPolicy, so the AI level is not affected.
     */
    private fun ensureEarpieceSilent() {
        try {
            val stream = AudioManager.STREAM_VOICE_CALL
            val min = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) {
                am.getStreamMinVolume(stream)
            } else {
                1
            }
            val cur = am.getStreamVolume(stream)
            if (cur > min) {
                am.setStreamVolume(stream, min, 0)
                Log.i(TAG, "voice call volume $cur -> $min (earpiece silent; agent normalizes capture)")
            }
        } catch (e: Exception) {
            Log.w(TAG, "voice call volume min failed: ${e.message}")
        }
    }

    private fun applyZenSilence() {
        // Android 15+: the module already turned the phone's Do Not Disturb on (root), and
        // the ringer is silent with its volume at zero. A filter set here would only add
        // Bageo's own mode, which blocks the switch to a normal phone.
        if (LocalAudioGuard.appHasOwnDndMode()) return
        try {
            if (nm != null && Build.VERSION.SDK_INT >= Build.VERSION_CODES.M) {
                if (!nm.isNotificationPolicyAccessGranted) {
                    Log.w(TAG, "ACCESS_NOTIFICATION_POLICY not granted; trying filter anyway")
                }
                nm.setInterruptionFilter(NotificationManager.INTERRUPTION_FILTER_NONE)
            }
        } catch (e: Exception) {
            Log.w(TAG, "zen INTERRUPTION_FILTER_NONE failed: ${e.message}")
        }
    }

    /** Best-effort mic/root knobs. Never touches telephony output routing. */
    private fun applyRootForceMute() {
        val cmds = listOf(
            "cmd media.audio_flinger mute mic 1",
            "settings put system volume_ring 0",
            "settings put system volume_notification 0",
            "settings put system volume_music 0",
            "settings put system volume_system 0",
            "settings put system volume_alarm 0",
        ) + ROOT_MIC_MUTE_CONTROLS.map { "tinymix '$it' 1" }
        for (cmd in cmds) trySu(cmd)
    }

    private fun trySu(cmd: String) {
        try {
            Runtime.getRuntime().exec(arrayOf("su", "-c", cmd))
        } catch (_: Exception) {
        }
    }

    private fun tryZeroViaSettings(stream: Int) {
        val name = when (stream) {
            AudioManager.STREAM_RING -> "volume_ring"
            AudioManager.STREAM_NOTIFICATION -> "volume_notification"
            AudioManager.STREAM_MUSIC -> "volume_music"
            AudioManager.STREAM_SYSTEM -> "volume_system"
            AudioManager.STREAM_ALARM -> "volume_alarm"
            else -> return
        }
        trySu("settings put system $name 0")
    }

    private fun abandonFocus() {
        try {
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
                val request = AudioFocusRequest.Builder(AudioManager.AUDIOFOCUS_GAIN_TRANSIENT)
                    .setAudioAttributes(
                        AudioAttributes.Builder()
                            .setUsage(AudioAttributes.USAGE_NOTIFICATION)
                            .setContentType(AudioAttributes.CONTENT_TYPE_SONIFICATION)
                            .build(),
                    )
                    .setOnAudioFocusChangeListener { }
                    .build()
                am.requestAudioFocus(request)
                am.abandonAudioFocusRequest(request)
            } else {
                @Suppress("DEPRECATION")
                am.abandonAudioFocus(null)
            }
        } catch (e: Exception) {
            Log.w(TAG, "abandonFocus: ${e.message}")
        }
    }

    private fun restoreBlocking() {
        if (!armed) {
            Log.i(TAG, "audio restore skipped (not armed)")
            return
        }
        if (armedDaily) {
            restoreDailyCallBlocking()
            return
        }
        try {
            savedMicMute?.let { am.isMicrophoneMute = it }
        } catch (_: Exception) {
        }
        try {
            savedSpeaker?.let {
                @Suppress("DEPRECATION")
                am.isSpeakerphoneOn = it
            }
        } catch (_: Exception) {
        }
        try {
            savedMode?.let { am.mode = it }
        } catch (_: Exception) {
        }
        try {
            savedRinger?.let { am.ringerMode = it }
        } catch (_: Exception) {
        }
        try {
            // Android 15+: putting the saved filter back would turn Bageo's own mode on.
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.M && !LocalAudioGuard.appHasOwnDndMode()) {
                savedInterruption?.let { nm?.setInterruptionFilter(it) }
            }
        } catch (_: Exception) {
        }
        for ((stream, volume) in savedVolumes) {
            // Keep the earpiece at min: raising it again would leak the start of
            // the next call if the service is not re-armed in time.
            if (stream == AudioManager.STREAM_VOICE_CALL) continue
            try {
                am.setStreamVolume(stream, volume, 0)
            } catch (_: Exception) {
            }
        }
        savedMicMute = null
        savedSpeaker = null
        savedMode = null
        savedRinger = null
        savedInterruption = null
        savedVolumes.clear()
        armed = false
        armedDaily = false
        armedFlag.set(false)
        Log.i(TAG, "audio state restored")
    }

    /**
     * Daily Sound: put back only what the call changed (microphone mute, speaker,
     * earpiece volume). The ringer and the other volumes were not touched, and writing
     * old values back could undo a change the owner made during the call. The audio mode
     * is Telecom's (the saved one is the ringing mode when the guard armed on a ring).
     */
    private fun restoreDailyCallBlocking() {
        try {
            savedMicMute?.let { am.isMicrophoneMute = it }
        } catch (_: Exception) {
        }
        try {
            savedSpeaker?.let {
                @Suppress("DEPRECATION")
                am.isSpeakerphoneOn = it
            }
        } catch (_: Exception) {
        }
        try {
            savedVolumes[AudioManager.STREAM_VOICE_CALL]?.let { volume ->
                if (am.getStreamVolume(AudioManager.STREAM_VOICE_CALL) != volume) {
                    am.setStreamVolume(AudioManager.STREAM_VOICE_CALL, volume, 0)
                }
            }
        } catch (_: Exception) {
        }
        savedMicMute = null
        savedSpeaker = null
        savedMode = null
        savedRinger = null
        savedInterruption = null
        savedVolumes.clear()
        armed = false
        armedDaily = false
        armedFlag.set(false)
        Log.i(TAG, "audio state restored (Daily Sound call)")
    }

    /**
     * After [restoreBlocking] for the owner's own call: the root mic mutes are not
     * part of the saved state, and the earpiece is kept at its minimum on purpose, so
     * undo both here (the same controls normal phone mode resets).
     */
    private fun unmuteForOwnCallBlocking() {
        try {
            am.isMicrophoneMute = false
        } catch (e: Exception) {
            Log.w(TAG, "own call: microphone unmute failed: ${e.message}")
        }
        trySu("cmd media.audio_flinger mute mic 0")
        for (control in ROOT_MIC_MUTE_CONTROLS) trySu("tinymix '$control' 0")
        try {
            val stream = AudioManager.STREAM_VOICE_CALL
            val min = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) am.getStreamMinVolume(stream) else 1
            val cur = am.getStreamVolume(stream)
            if (cur <= min) {
                val normal = PhoneModeManager.defaultVolume(am.getStreamMaxVolume(stream))
                am.setStreamVolume(stream, normal, 0)
                Log.i(TAG, "own call: voice call volume $cur -> $normal")
            }
        } catch (e: Exception) {
            Log.w(TAG, "own call: voice call volume failed: ${e.message}")
        }
        Log.i(TAG, "own call: microphone and earpiece back to normal")
    }

    private fun forgetSavedState() {
        LocalAudioGuard.turnOffOwnDndMode(appContext)
        savedMicMute = null
        savedSpeaker = null
        savedMode = null
        savedRinger = null
        savedInterruption = null
        savedVolumes.clear()
        armed = false
        armedDaily = false
        armedFlag.set(false)
        Log.i(TAG, "audio guard released (normal phone mode)")
    }

    private fun applyConsolePreviewBlocking() {
        if (!Prefs.isAiCallEnabled(appContext)) return
        // Daily Sound never mutes media, so there is nothing to allow or take back.
        if (BuildConfig.STANDALONE && Prefs.isStandaloneDailySound(appContext)) return
        val allow = consolePreviewAllowed.get() && !holdActive.get() && !CallAudioService.isAnyCallBusy()
        try {
            if (allow) {
                val maxVol = am.getStreamMaxVolume(AudioManager.STREAM_MUSIC).coerceAtLeast(1)
                val target = savedVolumes[AudioManager.STREAM_MUSIC]?.takeIf { it > 0 }
                    ?: PhoneModeManager.defaultVolume(maxVol)
                am.setStreamVolume(AudioManager.STREAM_MUSIC, target, 0)
                trySu("settings put system volume_music $target")
                Log.i(TAG, "console preview audio enabled STREAM_MUSIC=$target")
            } else {
                am.setStreamVolume(AudioManager.STREAM_MUSIC, 0, 0)
                tryZeroViaSettings(AudioManager.STREAM_MUSIC)
                Log.i(TAG, "console preview audio muted STREAM_MUSIC=0")
            }
        } catch (e: Exception) {
            Log.w(TAG, "console preview volume update failed: ${e.message}")
        }
    }

    companion object {
        private const val TAG = "LocalAudioGuard"
        private const val ROOT_PASS_TICKS = 4
        private const val AUDIO_OPERATION_WARN_MS = 3000L

        /** tinymix microphone mutes set by [applyRootForceMute] (absent ones just fail). */
        private val ROOT_MIC_MUTE_CONTROLS = listOf(
            "Mic Mute",
            "Microphone Mute",
            "ADC1 Mute",
            "ADC2 Mute",
            "IN1 Mute",
            "IN2 Mute",
        )

        private val TRACKED_STREAMS = intArrayOf(
            AudioManager.STREAM_VOICE_CALL,
            AudioManager.STREAM_MUSIC,
            AudioManager.STREAM_RING,
            AudioManager.STREAM_NOTIFICATION,
            AudioManager.STREAM_SYSTEM,
            AudioManager.STREAM_ALARM,
            AudioManager.STREAM_DTMF,
        )

        /** VOICE_CALL is handled by [ensureEarpieceSilent] (min index, not zero). */
        private val LOCAL_ZERO_STREAMS = intArrayOf(
            AudioManager.STREAM_MUSIC,
            AudioManager.STREAM_RING,
            AudioManager.STREAM_NOTIFICATION,
            AudioManager.STREAM_SYSTEM,
            AudioManager.STREAM_ALARM,
            AudioManager.STREAM_DTMF,
        )
    }
}

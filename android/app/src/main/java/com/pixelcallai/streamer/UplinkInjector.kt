// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

package com.pixelcallai.streamer

import android.content.Context
import android.media.AudioAttributes
import android.media.AudioDeviceInfo
import android.media.AudioFormat
import android.media.AudioManager
import android.media.AudioTrack
import android.os.Build
import android.os.SystemClock
import android.util.Log
import java.io.File
import java.io.FileOutputStream
import java.util.concurrent.CountDownLatch
import java.util.concurrent.LinkedBlockingDeque
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicLong

/**
 * Inject TTS PCM into the cellular call uplink so the remote party hears AI
 * without any local speaker playback.
 *
 * Stock Android does not allow third-party apps to replace the call mic path.
 * Working path on modern Pixels (BCP-class): privileged/system app writes an
 * [AudioTrack] to [AudioDeviceInfo.TYPE_TELEPHONY]. Magisk module
 * `pixelcallai_uplink` installs this app as priv-app with MODIFY_PHONE_STATE.
 */
interface UplinkInjector {
    /** True when a privileged injection backend is present and usable. */
    fun isAvailable(): Boolean

    /**
     * Begin an injection session. Must be called only while call is OFFHOOK/ACTIVE.
     * @return true when the backend worker was started. PCM may queue while the
     * telephony route is still becoming available.
     */
    fun start(): Boolean

    /** Queue s16le mono PCM at the bridge sample rate while the worker is running. */
    fun writePcm(pcm: ByteArray)

    /**
     * Drop queued but not-yet-written frames (e.g. new `tts_start` after DTMF barge-in).
     * Prevents residual menu PCM from splicing into hold music as crackle.
     */
    fun clearPending() {}

    /** Wait until TYPE_TELEPHONY inject is live (or timeout). */
    fun awaitRouteReady(timeoutMs: Long): Boolean = true

    /** Non-blocking route state used by diagnostics and recovery monitoring. */
    fun isRouteReady(): Boolean = true

    /** End session and release resources. */
    fun stop()
}

/**
 * SILENT_LOCAL: TYPE_TELEPHONY only. Never earpiece/speaker, never RootFifo fallthrough
 * for TTS (fifo is probe/legacy only). Stub if telephony device absent.
 */
object UplinkInjectorFactory {
    fun create(
        context: Context,
        onUnavailableOnce: () -> Unit,
        requireInCallMode: Boolean = false,
        onHealthReport: (UplinkHealthReport) -> Unit = {},
        isCallConnected: () -> Boolean = { true },
    ): UplinkInjector {
        val telephony = TelephonyUplinkInjector(
            context,
            onUnavailableOnce = onUnavailableOnce,
            requireInCallMode = requireInCallMode,
            onHealthReport = onHealthReport,
            isCallConnected = isCallConnected,
        )
        if (telephony.isAvailable()) {
            Log.i(TAG, "using TelephonyUplinkInjector (TYPE_TELEPHONY only)")
        } else {
            // TYPE_TELEPHONY can appear shortly after Telecom reports OFFHOOK.
            // Keep the telephony-only worker alive so it can latch the device later;
            // never replace it with an earpiece/speaker/fifo path.
            Log.w(TAG, "TYPE_TELEPHONY not visible yet — telephony-only worker will retry")
        }
        return telephony
    }

    private const val TAG = "UplinkInjectorFactory"
}

/** Default: no injection. Soft-fail; never falls back to speaker. */
class StubUplinkInjector(
    private val context: Context,
    private val onUnavailableOnce: () -> Unit,
) : UplinkInjector {
    private var notified = false

    override fun isAvailable(): Boolean = false

    override fun start(): Boolean {
        notifyOnce()
        Log.w(TAG, "uplink injection unavailable (need Magisk pixelcallai_uplink / priv-app)")
        return false
    }

    override fun writePcm(pcm: ByteArray) {
        if (!isAvailable()) notifyOnce()
    }

    override fun isRouteReady(): Boolean = false

    override fun stop() {}

    private fun notifyOnce() {
        if (notified) return
        notified = true
        onUnavailableOnce()
    }

    companion object {
        private const val TAG = "UplinkInjector"
    }
}

/**
 * BCP-style inject: AudioTrack → preferred device TYPE_TELEPHONY.
 * Requires system/priv-app + MODIFY_PHONE_STATE for reliable routing on Pixel.
 * Never uses speaker/earpiece STREAM devices for TTS.
 *
 * Open track only during an active call (OFFHOOK). Refuse to write PCM unless
 * setPreferredDevice succeeds and routed device is TYPE_TELEPHONY — otherwise
 * AudioTrack would leak to earpiece (local sound) and remote hears silence.
 *
 * Keepalive: an idle AudioTrack lets the Pixel 9 audio HAL go to sleep, and the next
 * write then rebuilds the track (`restoreTrack_l: dead IAudioTrack`, about 0.6 s), which
 * swallowed a short opening "喂？". Once the queue has been empty for
 * [KEEPALIVE_IDLE_MS], zeros are written [KEEPALIVE_CHUNK_MS] at a time whenever the
 * audio already in the track is about to run out, until [stop]. Queued audio is always
 * written first and at once; the zeros are not counted in `bytesWritten`.
 *
 * Tuning: the track's settings come from an [UplinkProfile]. A sampler thread compares the
 * playback head with real time ([UplinkHealthMeter]); when the track keeps losing audio
 * the worker rebuilds it with the next profile ([UplinkTuner]). Real calls store the
 * outcome ([UplinkTuneStore]) and hand a summary to [onHealthReport] when the worker ends.
 */
class TelephonyUplinkInjector(
    context: Context,
    private val sampleRate: Int = BuildConfig.SAMPLE_RATE,
    private val onUnavailableOnce: () -> Unit = {},
    /** Real cellular calls must wait for Telecom's MODE_IN_CALL before createTrack. */
    private val requireInCallMode: Boolean = false,
    private val onHealthReport: (UplinkHealthReport) -> Unit = {},
    /**
     * The other side answered (Telecom ACTIVE), not just dialing. The unsupported checks
     * only count time from then on, so a call still ringing out is never judged.
     */
    private val isCallConnected: () -> Boolean = { true },
) : UplinkInjector {
    private val appContext = context.applicationContext
    private val am = appContext.getSystemService(AudioManager::class.java)
    // Head-burst greets + Gemini continuous TTS; deep queue absorbs 500 ms prebuffer
    // and multi-second prompts without truncating inject (still realtime drain by AudioTrack).
    private data class QueuedPcm(val pcm: ByteArray, val clearEpoch: Long)

    private val queue = LinkedBlockingDeque<QueuedPcm>(2400)
    /** Serializes clear/enqueue/requeue so cleared frames cannot cross the barrier. */
    private val queueLock = Any()
    /** Prevents start from overlapping a stop/join of the previous worker. */
    private val lifecycleLock = Any()
    /** Protects publication and capture of the AudioTrack used to unblock writes. */
    private val trackLock = Any()
    /** Makes clearPending a barrier against an already-entered blocking write. */
    private val writeBarrierLock = Any()
    private val clearEpoch = AtomicLong(0)
    private val running = AtomicBoolean(false)
    private val routeReady = AtomicBoolean(false)
    private var worker: Thread? = null
    /** Live track for per-write preferred/routed TYPE_TELEPHONY checks. */
    @Volatile private var activeTrack: AudioTrack? = null
    private var bytesWritten = 0L
    /** Keepalive zeros written (not call audio; kept out of [bytesWritten]). */
    private var keepaliveBytes = 0L
    private var abortLogged = AtomicBoolean(false)
    private var readyLatch = CountDownLatch(1)
    private var dropWarnLogged = AtomicBoolean(false)
    private var savedMode: Int = AudioManager.MODE_NORMAL
    private var modeChanged: Boolean = false
    private val unavailableNotified = AtomicBoolean(false)
    /** Last sample written — used to soften frame joins after queue pressure. */
    @Volatile private var lastSample: Short = 0
    private val joinLock = Any()

    /** Guards [tuner] state, [meterTrack] and [meterOfTrack] between worker and sampler. */
    private val meterLock = Any()
    @Volatile private var tuner: UplinkTuner? = null
    /** The live track the sampler reads, and the meter of the profile it was built with. */
    private var meterTrack: AudioTrack? = null
    private var meterOfTrack: UplinkHealthMeter? = null
    /** Set by the sampler: the worker rebuilds the track with [UplinkTuner.current]. */
    @Volatile private var switchRequested = false
    private var sampler: Thread? = null
    // Worker-thread only.
    private var caps = TelephonyCaps()
    private var writeErrors = 0
    private var underruns = 0L
    private var lastPlan: UplinkTrackPlan? = null
    private var lastBufferMs = 0L
    private var lastOpenFailReason = "track_rejected"

    override fun isAvailable(): Boolean {
        val changed = logTelephonyDevices()
        val tel = telephonyOutput()
        if (tel == null) {
            if (changed) Log.w(TAG, "TYPE_TELEPHONY output device not found")
            return false
        }
        return true
    }

    override fun start(): Boolean = synchronized(lifecycleLock) {
        // Concurrent/reuse: worker is already opening TYPE_TELEPHONY or is live.
        if (running.get()) {
            return@synchronized true
        }
        if (worker?.isAlive == true) {
            Log.e(TAG, "start refused: previous telephony-uplink worker is still stopping")
            return@synchronized false
        }
        running.set(true)
        val tel = telephonyOutput()
        if (tel == null) {
            Log.w(TAG, "start: TYPE_TELEPHONY not visible yet; worker will keep retrying")
        }
        bytesWritten = 0L
        keepaliveBytes = 0L
        routeReady.set(false)
        synchronized(writeBarrierLock) {
            synchronized(queueLock) {
                clearEpoch.incrementAndGet()
                queue.clear()
            }
        }
        readyLatch = CountDownLatch(1)
        dropWarnLogged.set(false)
        try {
            modeChanged = false
            savedMode = am.mode
            // During real OFFHOOK, mode is already MODE_IN_CALL — do not override.
            // Only nudge for idle smoke probes. A real call must not ask AudioFlinger
            // to create a VOICE_COMMUNICATION track while Telecom is still switching
            // from MODE_NORMAL; Android 16 can block createTrack and abort audioserver.
            if (!requireInCallMode && savedMode == AudioManager.MODE_NORMAL) {
                am.mode = AudioManager.MODE_IN_COMMUNICATION
                modeChanged = true
            }
            Log.i(TAG, "start with audioMode=${am.mode} (saved=$savedMode)")
        } catch (e: Exception) {
            Log.w(TAG, "set mode failed: ${e.message}")
        }
        synchronized(meterLock) {
            tuner = null
            meterTrack = null
            meterOfTrack = null
        }
        switchRequested = false
        worker = Thread({
            runTrackLoop(tel)
        }, "telephony-uplink").also {
            it.isDaemon = true
            it.start()
        }
        sampler = Thread({
            runSampler()
        }, "telephony-uplink-health").also {
            it.isDaemon = true
            it.start()
        }
        // Non-blocking: call session opens bridge immediately; writePcm buffers into
        // queue while worker latches TYPE_TELEPHONY (was readyLatch.await 4s → ~6s silence).
        Log.i(
            TAG,
            "TelephonyUplinkInjector worker started device=${tel?.productName ?: "pending"} " +
                "id=${tel?.id ?: -1} " +
                "mode=${am.mode} (routeReady latches async; PCM queues until prefer=TELEPHONY)",
        )
        true
    }

    override fun clearPending() {
        val n = synchronized(writeBarrierLock) {
            synchronized(queueLock) {
                val pending = queue.size
                clearEpoch.incrementAndGet()
                queue.clear()
                pending
            }
        }
        synchronized(joinLock) { lastSample = 0 }
        if (n > 0) {
            Log.i(TAG, "uplink queue cleared pendingFrames=$n")
        }
    }

    override fun awaitRouteReady(timeoutMs: Long): Boolean {
        if (routeReady.get()) return true
        val ok = readyLatch.await(timeoutMs.coerceAtLeast(1), TimeUnit.MILLISECONDS)
        return ok && routeReady.get()
    }

    override fun isRouteReady(): Boolean = routeReady.get()

    override fun writePcm(pcm: ByteArray) {
        if (!running.get() || pcm.isEmpty()) return
        // Always enqueue while the injector is running. Worker only track.write after
        // prefer=TYPE_TELEPHONY. Dropping early frames was the classic ~5–6s silence +
        // missing start of the greeting (the agent paced audio while the route was
        // still warming).
        if (!routeReady.get() && dropWarnLogged.compareAndSet(false, true)) {
            Log.w(
                TAG,
                "writePcm buffering frame bytes=${pcm.size} while routeReady=false " +
                    "queue=${queue.size} (will playout when TYPE_TELEPHONY prefer ok)",
            )
        }
        val framed = softLimitS16le(pcm, 0.79f)
        val frameEpoch = clearEpoch.get()
        // Prefer waiting for drain over dropping — drop-oldest cut menu after "Good".
        var tries = 0
        while (true) {
            if (!running.get()) return
            val enqueued = synchronized(queueLock) {
                if (!running.get() || frameEpoch != clearEpoch.get()) {
                    return
                }
                queue.offer(QueuedPcm(framed, frameEpoch))
            }
            if (enqueued) return
            tries++
            if (tries == 1 || tries % 50 == 0) {
                Log.w(TAG, "uplink queue full size=${queue.size} wait tries=$tries")
            }
            if (tries > 400) { // ~2s still full — last resort drop oldest
                val inserted = synchronized(queueLock) {
                    if (!running.get() || frameEpoch != clearEpoch.get()) {
                        return
                    }
                    queue.poll()
                    queue.offer(QueuedPcm(framed, frameEpoch))
                }
                if (!inserted) {
                    Log.w(TAG, "uplink queue overflow; dropped frame")
                }
                return
            }
            try {
                Thread.sleep(5)
            } catch (_: InterruptedException) {
                return
            }
        }
    }

    override fun stop(): Unit = synchronized(lifecycleLock) {
        // AudioTrack.write(WRITE_BLOCKING) is not reliably interruptible. Stop the
        // published track first so the worker can leave the native write and release it.
        val currentTrack = synchronized(trackLock) {
            running.set(false)
            routeReady.set(false)
            activeTrack
        }
        try {
            currentTrack?.pause()
        } catch (_: Exception) {
        }
        try {
            currentTrack?.flush()
        } catch (_: Exception) {
        }
        try {
            currentTrack?.stop()
        } catch (_: Exception) {
        }

        sampler?.interrupt()
        sampler = null
        val currentWorker = worker
        currentWorker?.interrupt()
        synchronized(writeBarrierLock) {
            synchronized(queueLock) {
                clearEpoch.incrementAndGet()
                queue.clear()
                queue.offer(QueuedPcm(ByteArray(0), clearEpoch.get())) // wake poll
            }
        }
        currentWorker?.join(1500)
        if (currentWorker?.isAlive == true) {
            // Last resort: release forces a blocked native write to fail. A still-live
            // worker remains registered so start() cannot overlap it with a new track.
            try {
                currentTrack?.release()
            } catch (_: Exception) {
            }
            currentWorker.interrupt()
            currentWorker.join(500)
        }
        synchronized(trackLock) {
            if (activeTrack === currentTrack) activeTrack = null
        }
        worker = currentWorker?.takeIf { it.isAlive }
        readyLatch.countDown()
        abortLogged.set(false)
        dropWarnLogged.set(false)
        if (modeChanged) {
            try {
                am.mode = savedMode
            } catch (_: Exception) {
            }
            modeChanged = false
        }
        if (worker != null) {
            Log.e(TAG, "TelephonyUplinkInjector stop timed out; restart held until worker exits")
        }
        Log.i(TAG, "TelephonyUplinkInjector stopped bytesWritten=$bytesWritten keepaliveBytes=$keepaliveBytes")
    }

    /**
     * Preferred device must stay TYPE_TELEPHONY.
     *
     * Do **not** require [AudioTrack.getRoutedDevice] == TYPE_TELEPHONY: on Pixel 7/7a/8
     * during OFFHOOK, routed often reports BUILTIN_EARPIECE/SPEAKER even when
     * setPreferredDevice(TYPE_TELEPHONY) succeeded and uplink mix is live. Requiring
     * routed==TELEPHONY caused bytesWritten=0 (remote hears silence).
     */
    private fun isTelephonyRoute(track: AudioTrack): Boolean {
        val pref = track.preferredDevice
        if (pref == null || pref.type != AudioDeviceInfo.TYPE_TELEPHONY) return false
        val routed = track.routedDevice
        if (routed != null && routed.type != AudioDeviceInfo.TYPE_TELEPHONY) {
            // Warn once-ish via debug path; still allow writes.
            Log.d(
                TAG,
                "routedType=${routed.type} name=${routed.productName} while pref=TELEPHONY — allow (Pixel HAL quirk)",
            )
        }
        return true
    }

    private fun telephonyOutput(): AudioDeviceInfo? {
        return am.getDevices(AudioManager.GET_DEVICES_OUTPUTS)
            .firstOrNull { it.type == AudioDeviceInfo.TYPE_TELEPHONY }
    }

    /**
     * Logs the output devices only when they differ from the last report (the app's
     * screen asks every few seconds). True when this call logged.
     */
    private fun logTelephonyDevices(): Boolean {
        return try {
            val outs = am.getDevices(AudioManager.GET_DEVICES_OUTPUTS)
            val tel = outs.filter { it.type == AudioDeviceInfo.TYPE_TELEPHONY }
            val summary = "outputs=${outs.size} TYPE_TELEPHONY=${tel.size} " +
                tel.joinToString { "id=${it.id}:${it.productName}" }
            if (summary == lastDeviceSummary) {
                false
            } else {
                lastDeviceSummary = summary
                Log.i(TAG, summary)
                true
            }
        } catch (e: Exception) {
            Log.w(TAG, "list devices failed: ${e.message}")
            false
        }
    }

    private fun runTrackLoop(initialTel: AudioDeviceInfo?) {
        var attempt = 0
        writeErrors = 0
        underruns = 0L
        lastPlan = null
        lastBufferMs = 0L
        caps = TelephonyCaps()
        // When the other side was first seen to answer, and when the current profile started
        // failing to open a track after that; 0 = not yet.
        var connectedSince = 0L
        var openFailSince = 0L
        var noDevice = false
        while (running.get()) {
            attempt = (attempt + 1).coerceAtMost(1_000_000)
            if (requireInCallMode && am.mode != AudioManager.MODE_IN_CALL) {
                if (attempt == 1 || attempt % 5 == 0) {
                    Log.i(TAG, "waiting for MODE_IN_CALL before createTrack mode=${am.mode} attempt=$attempt")
                }
                try {
                    Thread.sleep(retryDelayMs(attempt))
                } catch (_: InterruptedException) {
                    break
                }
                continue
            }
            if (connectedSince == 0L && callConnected()) connectedSince = SystemClock.elapsedRealtime()
            // The start-time handle is only a first-attempt hint. If AudioManager no
            // longer lists it, later attempts must wait for a fresh device handle.
            val tel = telephonyOutput() ?: initialTel.takeIf { attempt == 1 }
            if (tel == null) {
                if (unavailableNotified.compareAndSet(false, true)) {
                    onUnavailableOnce()
                }
                if (!noDevice && connectedSince != 0L &&
                    SystemClock.elapsedRealtime() - connectedSince >= UNSUPPORTED_AFTER_MS &&
                    tuner?.everLive != true
                ) {
                    noDevice = true
                    Log.w(TAG, "uplink tune: no TYPE_TELEPHONY output ${UNSUPPORTED_AFTER_MS}ms into the call")
                }
                val retryMs = retryDelayMs(attempt)
                Log.w(TAG, "TYPE_TELEPHONY unavailable attempt=$attempt retry_ms=$retryMs")
                try {
                    Thread.sleep(retryMs)
                } catch (_: InterruptedException) {
                    break
                }
                continue
            }
            val t = tuner ?: newTuner(tel).also { created ->
                synchronized(meterLock) { tuner = created }
            }
            var wentLive = false
            val ok = runOneTrack(tel, attempt, t) { wentLive = true }
            if (!running.get()) break
            if (ok) {
                break
            }
            routeReady.set(false)
            if (switchRequested) {
                switchRequested = false
                openFailSince = 0L
                val profile = synchronized(meterLock) { t.current }
                Log.w(TAG, "uplink tune: rebuilding the track with profile=${profile.id}")
                continue
            }
            if (wentLive) {
                openFailSince = 0L
            } else if (connectedSince != 0L) {
                val now = SystemClock.elapsedRealtime()
                if (openFailSince == 0L) {
                    openFailSince = now
                } else if (now - openFailSince >= UNSUPPORTED_AFTER_MS) {
                    val reason = lastOpenFailReason
                    val (from, next) = synchronized(meterLock) { t.current to t.onOpenFailed(reason) }
                    // Restart the clock either way: the next profile, or the same one again.
                    openFailSince = 0L
                    if (next != null) {
                        Log.w(TAG, "uplink tune: profile=${from.id} cannot open ($reason), trying ${next.id}")
                        continue
                    }
                }
            }
            val retryMs = retryDelayMs(attempt)
            Log.w(TAG, "recreating AudioTrack after failure attempt=$attempt retry_ms=$retryMs")
            try {
                Thread.sleep(retryMs)
            } catch (_: InterruptedException) {
                break
            }
        }
        running.set(false)
        readyLatch.countDown()
        finishTuning(noDevice)
    }

    /** What [finishTuning] reads from the tuner, taken under [meterLock] in one go. */
    private data class TuneSnapshot(
        val decision: TuneDecision,
        val totals: Triple<Long, Int, Int>,
        val audioMs: Long,
        val start: UplinkProfile,
        val current: UplinkProfile,
        val source: StartSource,
        val switches: Int,
        val autoSwitch: Boolean,
        val live: Boolean,
    )

    /** Playback head of [track]; null when the track was released meanwhile. */
    private fun readHead(track: AudioTrack): Int? = try {
        track.playbackHeadPosition
    } catch (_: Exception) {
        null
    }

    private fun callConnected(): Boolean = try {
        isCallConnected()
    } catch (_: Exception) {
        false
    }

    /** The call's tuner, from what the telephony output lists and what this phone stored. */
    private fun newTuner(tel: AudioDeviceInfo): UplinkTuner {
        caps = readCaps(tel)
        val chip = UplinkTuneStore.chip
        val measureOnly = UplinkTuneStore.measureOnly
        val callCaps = caps
        val samePlan = { a: UplinkProfile, b: UplinkProfile ->
            planTrack(a, callCaps, sampleRate) == planTrack(b, callCaps, sampleRate)
        }
        val forced = UplinkTuneStore.forcedProfile()
        val readStore = forced == null && !measureOnly
        val (start, source) = pickStartProfile(
            forced = forced,
            avoid = if (readStore) UplinkTuneStore.avoidedProfile(appContext) else null,
            remembered = if (readStore) UplinkTuneStore.rememberedProfile(appContext) else null,
            chip = chip,
            caps = caps,
            inputRate = sampleRate,
            measureOnly = measureOnly,
            samePlan = samePlan,
        )
        Log.i(
            TAG,
            "uplink tune: start profile=${start.id} source=${source.id} chip=$chip " +
                "measure_only=$measureOnly caps ${caps.describe()}",
        )
        return UplinkTuner(
            start,
            source,
            autoSwitch = !measureOnly,
            defaultStart = defaultStartProfile(chip, caps, sampleRate).first,
            keepOwnerProblem = UplinkTuneStore.keepOwnerProblem(appContext),
            samePlan = samePlan,
        )
    }

    private fun readCaps(tel: AudioDeviceInfo): TelephonyCaps {
        fun list(read: () -> IntArray): List<Int> = try {
            read().toList()
        } catch (_: Exception) {
            emptyList()
        }
        return TelephonyCaps(
            sampleRates = list { tel.sampleRates },
            channelCounts = list { tel.channelCounts },
            channelMasks = list { tel.channelMasks },
            encodings = list { tel.encodings },
        )
    }

    /**
     * Sampler thread: reads the live track's head every [HEALTH_SAMPLE_MS]. The head is read
     * outside [meterLock]: on a track the server invalidated that call runs restoreTrack_l
     * (a Binder call that can stall), and the worker must never wait on it. The sample only
     * counts if the same track is still the measured one afterwards; a track the worker
     * released in between throws IllegalStateException or reads 0, and is skipped.
     */
    private fun runSampler() {
        var loggedEvents = 0
        while (running.get()) {
            try {
                Thread.sleep(HEALTH_SAMPLE_MS)
            } catch (_: InterruptedException) {
                break
            }
            val track = synchronized(meterLock) { meterTrack } ?: continue
            val head = readHead(track) ?: continue
            val now = SystemClock.elapsedRealtime()
            synchronized(meterLock) {
                if (meterTrack !== track) return@synchronized
                val meter = meterOfTrack ?: return@synchronized
                val t = tuner ?: return@synchronized
                val lost = meter.sample(now, head)
                if (lost >= UplinkHealthMeter.MIN_EVENT_MS && (loggedEvents < 5 || loggedEvents % 20 == 0)) {
                    Log.w(
                        TAG,
                        "uplink health: lost_ms=$lost profile=${t.current.id} " +
                            "total_lost_ms=${meter.lostMs} events=${meter.lossEvents}",
                    )
                }
                if (lost >= UplinkHealthMeter.MIN_EVENT_MS) loggedEvents++
                if (!switchRequested && !t.isUnhealthy(t.current) && meter.isUnhealthy(now)) {
                    val from = t.current
                    val next = t.onUnhealthy()
                    Log.w(
                        TAG,
                        "uplink health: profile=${from.id} unhealthy lost_ms=${meter.lostMs} " +
                            "events=${meter.lossEvents} rebuilds=${meter.rebuilds} -> " +
                            (next?.id ?: "stay") + " measure_only=${!t.autoSwitch}",
                    )
                    if (next != null) switchRequested = true
                }
            }
        }
    }

    /**
     * After the worker ends: store the call's outcome and report it. Only real calls
     * ([requireInCallMode]); idle probes never store or report.
     */
    private fun finishTuning(noDevice: Boolean) {
        if (!requireInCallMode) return
        try {
            val measureOnly = UplinkTuneStore.measureOnly
            val t = synchronized(meterLock) { tuner }
                ?: if (noDevice) {
                    UplinkTuner(
                        UplinkProfile.STANDARD,
                        StartSource.DEFAULT,
                        autoSwitch = !measureOnly,
                        keepOwnerProblem = UplinkTuneStore.keepOwnerProblem(appContext),
                    )
                } else {
                    return
                }
            // One consistent snapshot: the sampler may still be finishing a sample.
            val snap = synchronized(meterLock) {
                if (noDevice) t.markNoDevice()
                TuneSnapshot(
                    decision = t.decision(),
                    totals = t.totals(),
                    audioMs = t.meterOf(t.current)?.audioMs ?: 0L,
                    start = t.start,
                    current = t.current,
                    source = t.source,
                    switches = t.switches,
                    autoSwitch = t.autoSwitch,
                    live = t.everLive,
                )
            }
            val decision = snap.decision
            if (!snap.live && decision.fit == null) return
            // A forced (adb) call that settled nothing leaves the stored state untouched.
            if (snap.source != StartSource.FORCED || decision != TuneDecision()) {
                UplinkTuneStore.apply(appContext, decision, snap.current, snap.live)
            }
            val plan = lastPlan
            val report = UplinkHealthReport(
                startProfile = snap.start,
                finalProfile = snap.current,
                source = snap.source,
                trackRate = plan?.trackRate ?: 0,
                channels = plan?.channels ?: 0,
                bufferMs = lastBufferMs,
                lostMs = snap.totals.first,
                lossEvents = snap.totals.second,
                rebuilds = snap.totals.third,
                writeErrors = writeErrors,
                underruns = underruns,
                switches = snap.switches,
                measureOnly = !snap.autoSwitch,
                audioMs = snap.audioMs,
                live = snap.live,
                fit = UplinkTuneStore.fit(appContext),
                reason = decision.reason ?: UplinkTuneStore.reason(appContext),
                caps = caps,
            )
            Log.i(
                TAG,
                "uplink health report start=${report.startProfile.id} final=${report.finalProfile.id} " +
                    "source=${report.source.id} rate=${report.trackRate} channels=${report.channels} " +
                    "buffer_ms=${report.bufferMs} lost_ms=${report.lostMs} events=${report.lossEvents} " +
                    "rebuilds=${report.rebuilds} write_errors=${report.writeErrors} " +
                    "underruns=${report.underruns} switches=${report.switches} " +
                    "measure_only=${report.measureOnly} audio_ms=${report.audioMs} " +
                    "live=${report.live} fit=${report.fit.id} reason=${report.reason ?: "-"}",
            )
            onHealthReport(report)
        } catch (e: Exception) {
            Log.w(TAG, "uplink health report failed: ${e.message}")
        }
    }

    /**
     * @return true if stopped cleanly (running=false); false if track died and should recreate.
     */
    private fun runOneTrack(
        telephony: AudioDeviceInfo,
        attempt: Int,
        tuner: UplinkTuner,
        onLive: () -> Unit,
    ): Boolean {
        val profile = synchronized(meterLock) { tuner.current }
        val chip = UplinkTuneStore.chip
        // MediaTek (MT6985) lists only 44100/48000 for TELEPHONY and resets its HiFi output
        // path for every other track rate, which killed the track every ~270 ms. The profile
        // decides the rate: standard (Pixel included) keeps [sampleRate] with no resampling.
        val plan = planTrack(profile, caps, sampleRate)
        val trackRate = plan.trackRate
        val channels = plan.channels
        val primed = plan.primed
        val frameBytes = 2 * channels
        val channelMask = if (channels == 2) AudioFormat.CHANNEL_OUT_STEREO else AudioFormat.CHANNEL_OUT_MONO
        val encoding = AudioFormat.ENCODING_PCM_16BIT
        Log.i(
            TAG,
            "track profile=${profile.id} rate=$trackRate channels=$channels input=$sampleRate deviceRates=" +
                caps.sampleRates.joinToString(",", "[", "]") + " mediatek=${chip == ChipKind.MEDIATEK}",
        )
        lastOpenFailReason = "track_rejected"
        // Fresh state for every new track (also after a rebuild).
        val resampler = if (trackRate == sampleRate) null else PcmResampler(sampleRate, trackRate)
        val minBuf = AudioTrack.getMinBufferSize(trackRate, channelMask, encoding)
        if (minBuf <= 0) {
            Log.e(TAG, "getMinBufferSize failed: $minBuf")
            return false
        }
        val format = AudioFormat.Builder()
            .setSampleRate(trackRate)
            .setChannelMask(channelMask)
            .setEncoding(encoding)
            .build()
        val attrs = AudioAttributes.Builder()
            .setUsage(AudioAttributes.USAGE_VOICE_COMMUNICATION)
            .setContentType(AudioAttributes.CONTENT_TYPE_SPEECH)
            .build()
        // MediaTek's 48 kHz minBuf is ~128 ms (12,300 bytes); minBuf * 6 is 769 ms, which
        // AudioFlinger's FS_FILLING startup check cannot fill before its ~550 ms BUFFER TIMEOUT
        // removes the track and triggers stopOutput() -> setOutputDevices(force=1) -> an endless
        // restoreTrack_l loop every ~240 ms. Primed profiles keep about 160 ms; standard
        // (Pixel) keeps the original size.
        val bufBytes = profileBufferBytes(plan, minBuf)
        val bufDurationMs = pcmMsFor(bufBytes, trackRate, channels).coerceAtLeast(KEEPALIVE_CHUNK_MS)
        var track: AudioTrack? = null
        var wentLive = false
        // Frames handed to this track (probe, keepalive, call audio) and call audio alone.
        var trackFrames = 0L
        var audioFrames = 0L
        try {
            track = AudioTrack.Builder()
                .setAudioAttributes(attrs)
                .setAudioFormat(format)
                // Larger stream buffer (~200 ms) reduces underrun clicks on hold music.
                .setBufferSizeInBytes(bufBytes)
                .setTransferMode(AudioTrack.MODE_STREAM)
                .build()
            val liveTrack = track ?: return false
            if (liveTrack.state != AudioTrack.STATE_INITIALIZED) {
                Log.e(TAG, "AudioTrack not initialized state=${liveTrack.state}")
                return false
            }
            val preferOk = track.setPreferredDevice(telephony)
            val pref = track.preferredDevice
            Log.i(
                TAG,
                "setPreferredDevice ok=$preferOk prefType=${pref?.type} " +
                    "prefName=${pref?.productName} attempt=$attempt",
            )
            if (!preferOk || pref?.type != AudioDeviceInfo.TYPE_TELEPHONY) {
                Log.e(
                    TAG,
                    "setPreferredDevice(TYPE_TELEPHONY) failed prefType=${pref?.type} — " +
                        "need priv-app/MODIFY_PHONE_STATE or active call. Will NOT play " +
                        "(prevents earpiece/speaker leak).",
                )
                lastOpenFailReason = "prefer_rejected"
                return false
            }
            try {
                track.setVolume(1.0f)
            } catch (e: Exception) {
                Log.w(TAG, "setVolume(1) failed: ${e.message}")
            }
            if (primed && Build.VERSION.SDK_INT >= 31) {
                try {
                    liveTrack.setStartThresholdInFrames(
                        (trackRate / 25).coerceIn(1, liveTrack.bufferSizeInFrames),
                    )
                } catch (e: Exception) {
                    Log.w(TAG, "setStartThresholdInFrames failed: ${e.message}")
                }
            }
            track.play()
            synchronized(trackLock) {
                if (!running.get()) return true
                activeTrack = liveTrack
            }
            if (track.playState != AudioTrack.PLAYSTATE_PLAYING) {
                Log.e(TAG, "AudioTrack bad playState=${track.playState}")
                return false
            }
            // Probe write: confirm track accepts data without DEAD_OBJECT.
            // Primed profiles fill the whole buffer with silence at once so AudioFlinger leaves
            // FS_FILLING on the first mixer cycle instead of timing out after ~550 ms.
            val probe = ShortArray(if (primed) bufBytes / 2 else trackRate / 100 * channels)
            val probeN = synchronized(writeBarrierLock) {
                if (!running.get()) null else liveTrack.write(
                    probe,
                    0,
                    probe.size,
                    AudioTrack.WRITE_BLOCKING,
                )
            } ?: return true
            val routed = track.routedDevice
            Log.i(
                TAG,
                "AudioTrack playState=PLAYING minBuf=$minBuf rate=$trackRate " +
                    "probeWrite=$probeN prefType=${track.preferredDevice?.type} " +
                    "routedType=${routed?.type} routedName=${routed?.productName}",
            )
            if (probeN < 0) {
                writeErrors++
                Log.e(TAG, "probe write failed: $probeN — telephony path not live yet")
                return false
            }
            trackFrames += (probeN / channels).toLong()
            // Prefer TYPE_TELEPHONY is enough. Routed may show earpiece on Pixel HAL —
            // aborting here was the classic "agent plays audio, remote hears silence" bug.
            if (track.preferredDevice?.type != AudioDeviceInfo.TYPE_TELEPHONY) {
                Log.e(TAG, "preferred lost TYPE_TELEPHONY after play — abort")
                lastOpenFailReason = "prefer_rejected"
                return false
            }
            if (routed != null && routed.type != AudioDeviceInfo.TYPE_TELEPHONY) {
                Log.w(
                    TAG,
                    "routed is ${routed.type}/${routed.productName} but preferred=TELEPHONY — " +
                        "continuing inject (Pixel quirk; not earpiece fallthrough)",
                )
            }
            abortLogged.set(false)
            dropWarnLogged.set(false)
            synchronized(trackLock) {
                if (!running.get()) return true
                routeReady.set(true)
            }
            unavailableNotified.set(false)
            readyLatch.countDown()
            Log.i(
                TAG,
                "routeReady — draining queue frames=${queue.size} bytesWritten=$bytesWritten",
            )
            wentLive = true
            onLive()
            lastPlan = plan
            lastBufferMs = pcmMsFor(bufBytes, trackRate, channels)
            val baseHead = readHead(liveTrack) ?: 0
            val baseAt = SystemClock.elapsedRealtime()
            synchronized(meterLock) {
                val meter = tuner.meter()
                meter.beginTrack(baseAt, trackRate, baseHead)
                meterTrack = liveTrack
                meterOfTrack = meter
                tuner.markLive()
            }
            var consecutiveErrors = 0
            val silence = ByteArray(pcmBytesFor(KEEPALIVE_CHUNK_MS, trackRate, channels))
            var keepaliveLogged = false
            var seenClearEpoch = clearEpoch.get()
            // When the last call audio was written, and when the audio handed to the
            // track (call audio or keepalive zeros) is estimated to run out.
            var lastAudioAt = SystemClock.elapsedRealtime()
            var playoutUntil = if (primed) {
                lastAudioAt + pcmMsFor(probeN * 2, trackRate, channels)
            } else {
                lastAudioAt
            }
            while (running.get()) {
                if (switchRequested) return false
                val keepaliveAt = if (primed) {
                    playoutUntil - PRIMED_KEEPALIVE_LEAD_MS
                } else {
                    maxOf(lastAudioAt + KEEPALIVE_IDLE_MS, playoutUntil - KEEPALIVE_LEAD_MS)
                }
                val waitMs = (keepaliveAt - SystemClock.elapsedRealtime()).coerceIn(1L, KEEPALIVE_IDLE_MS)
                val queued = queue.poll(waitMs, TimeUnit.MILLISECONDS)
                if (queued == null) {
                    if (SystemClock.elapsedRealtime() < keepaliveAt) continue
                    val n = writeKeepalive(liveTrack, silence) ?: continue
                    if (n < 0) {
                        writeErrors++
                        Log.e(TAG, "keepalive write error: $n")
                        if (n == AudioTrack.ERROR_DEAD_OBJECT) {
                            // Rebuild now, while nobody is talking, not on the next word.
                            routeReady.set(false)
                            return false
                        }
                    } else {
                        if (!keepaliveLogged) {
                            keepaliveLogged = true
                            Log.i(TAG, "keepalive: queue idle ${KEEPALIVE_IDLE_MS}ms, writing silence to keep the track awake")
                        }
                        keepaliveBytes += n.toLong()
                        trackFrames += (n / frameBytes).toLong()
                        // The track's last sample is now 0; the next audio blends from there.
                        synchronized(joinLock) { lastSample = 0 }
                        resampler?.reset()
                    }
                    val now = SystemClock.elapsedRealtime()
                    playoutUntil = maxOf(now, playoutUntil) + KEEPALIVE_CHUNK_MS
                    if (primed) {
                        playoutUntil = playoutUntil.coerceAtMost(now + bufDurationMs)
                    }
                    continue
                }
                if (queued.pcm.isEmpty()) continue
                if (queued.clearEpoch != clearEpoch.get()) {
                    Log.i(TAG, "discard cleared PCM before write epoch=${queued.clearEpoch}")
                    continue
                }
                if (queued.clearEpoch != seenClearEpoch) {
                    seenClearEpoch = queued.clearEpoch
                    resampler?.reset()
                }
                val chunk = queued.pcm
                // Require preferred=TYPE_TELEPHONY for every write (no earpiece leak).
                // Prefer-only check matches isTelephonyRoute (Pixel routed quirk allowed).
                if (!isTelephonyRoute(track)) {
                    routeReady.set(false)
                    requeueFront(queued, "preferred route lost")
                    Log.e(
                        TAG,
                        "abort mid-stream write: preferred left TYPE_TELEPHONY " +
                            "prefType=${track.preferredDevice?.type} " +
                            "routedType=${track.routedDevice?.type}",
                    )
                    return false
                }
                val joined = synchronized(joinLock) {
                    blendJoinInPlace(chunk, lastSample).also { blended ->
                        if (blended.size >= 2) {
                            val o = blended.size - 2
                            lastSample = (
                                (blended[o].toInt() and 0xff) or
                                    (blended[o + 1].toInt() shl 8)
                                ).toShort()
                        }
                    }
                }
                // The queue and the join blend work at [sampleRate] in mono; only the track
                // rate and channel count differ.
                val resampled = resampler?.process(joined) ?: joined
                val toWrite = if (channels == 2) duplicateToTwoChannels(resampled) else resampled
                // What is left of this chunk at the input rate, for the next track.
                fun restFrom(offset: Int): ByteArray = if (resampler == null && channels == 1) {
                    toWrite.copyOfRange(offset, toWrite.size)
                } else {
                    val inOffset = ((offset / frameBytes).toLong() * sampleRate / trackRate).toInt() * 2
                    joined.copyOfRange(inOffset.coerceAtMost(joined.size), joined.size)
                }
                val writeStartedAt = SystemClock.elapsedRealtime()
                var offset = 0
                while (offset < toWrite.size && running.get()) {
                    if (queued.clearEpoch != clearEpoch.get()) {
                        Log.i(TAG, "discard cleared PCM during write epoch=${queued.clearEpoch}")
                        break
                    }
                    if (switchRequested) {
                        requeueFront(QueuedPcm(restFrom(offset), queued.clearEpoch), "profile switch")
                        return false
                    }
                    val n = synchronized(writeBarrierLock) {
                        if (!running.get() || queued.clearEpoch != clearEpoch.get()) {
                            null
                        } else {
                            liveTrack.write(
                                toWrite,
                                offset,
                                toWrite.size - offset,
                                AudioTrack.WRITE_BLOCKING,
                            )
                        }
                    }
                    if (n == null) {
                        Log.i(TAG, "discard cleared PCM at write barrier epoch=${queued.clearEpoch}")
                        break
                    }
                    if (n < 0) {
                        consecutiveErrors++
                        writeErrors++
                        Log.e(TAG, "AudioTrack.write error: $n consecutive=$consecutiveErrors")
                        if (n == AudioTrack.ERROR_DEAD_OBJECT || consecutiveErrors >= 3) {
                            routeReady.set(false)
                            // Requeue what is left at the input rate (the track is rebuilt with a
                            // fresh resampler).
                            requeueFront(
                                QueuedPcm(restFrom(offset), queued.clearEpoch),
                                "AudioTrack write failed",
                            )
                            return false
                        }
                        try {
                            Thread.sleep(20)
                        } catch (_: InterruptedException) {
                            return !running.get()
                        }
                        continue
                    }
                    consecutiveErrors = 0
                    offset += n
                    bytesWritten += n.toLong()
                    trackFrames += (n / frameBytes).toLong()
                    audioFrames += (n / frameBytes).toLong()
                }
                val writeEndedAt = SystemClock.elapsedRealtime()
                playoutUntil = maxOf(writeStartedAt, playoutUntil) + pcmMsFor(offset, trackRate, channels)
                if (primed) {
                    playoutUntil = playoutUntil.coerceAtMost(writeEndedAt + bufDurationMs)
                }
                lastAudioAt = writeEndedAt
            }
            return true
        } catch (e: Exception) {
            Log.e(TAG, "telephony inject failed: ${e.message}", e)
            return false
        } finally {
            synchronized(trackLock) {
                if (activeTrack === track) activeTrack = null
            }
            val liveTrack = track
            if (wentLive && liveTrack != null) {
                // A live track that ends while the call goes on, and not for a profile switch,
                // failed: the audio still in it is lost.
                val failed = running.get() && !switchRequested
                // Outside meterLock: on a dead track this may run restoreTrack_l.
                val endHead = if (failed) readHead(liveTrack) else null
                val endAt = SystemClock.elapsedRealtime()
                synchronized(meterLock) {
                    val meter = meterOfTrack.takeIf { meterTrack === liveTrack }
                    if (meter != null) {
                        meter.addAudioFrames(audioFrames)
                        if (failed) {
                            val pendingFrames = endHead?.let {
                                headAdvanceFrames(it, trackFrames.toInt())
                                    .coerceAtMost((bufBytes / frameBytes).toLong())
                            } ?: 0L
                            meter.noteRebuild(endAt, pendingFrames * 1000L / trackRate)
                        }
                        meter.endTrack()
                    }
                    meterTrack = null
                    meterOfTrack = null
                }
                try {
                    underruns += liveTrack.underrunCount.toLong()
                } catch (_: Exception) {
                }
            }
            // Clear only after track tear-down so in-flight writers stop cleanly.
            try {
                track?.pause()
            } catch (_: Exception) {
            }
            try {
                track?.flush()
            } catch (_: Exception) {
            }
            try {
                track?.stop()
            } catch (_: Exception) {
            }
            try {
                track?.release()
            } catch (_: Exception) {
            }
            routeReady.set(false)
        }
    }

    /** Preserve the not-yet-written head frame across AudioTrack recreation. */
    private fun requeueFront(queued: QueuedPcm, reason: String) {
        if (queued.pcm.isEmpty() || !running.get()) return
        val requeued = synchronized(queueLock) {
            running.get() && queued.clearEpoch == clearEpoch.get() && queue.offerFirst(queued)
        }
        if (requeued) {
            Log.w(TAG, "requeued pending PCM bytes=${queued.pcm.size} reason=$reason queue=${queue.size}")
        } else {
            Log.i(TAG, "discarded stale pending PCM bytes=${queued.pcm.size} reason=$reason")
        }
    }

    /**
     * One chunk of keepalive zeros, behind the same write barrier as call audio (so
     * [clearPending] still waits for it). Null when stopping or when call audio is
     * queued (that goes first); otherwise bytes written or a negative AudioTrack error.
     */
    private fun writeKeepalive(track: AudioTrack, silence: ByteArray): Int? =
        synchronized(writeBarrierLock) {
            if (!running.get() || !queue.isEmpty()) {
                null
            } else {
                track.write(silence, 0, silence.size, AudioTrack.WRITE_BLOCKING)
            }
        }

    companion object {
        private const val TAG = "TelephonyUplink"
        /** Last device list written to the log (shared by every instance). */
        @Volatile
        private var lastDeviceSummary: String? = null
        private const val JOIN_JUMP = 2500
        private const val JOIN_SAMPLES = 32 // ~2 ms @ 16 kHz
        /** Queue empty this long → start writing keepalive zeros. */
        private const val KEEPALIVE_IDLE_MS = 100L
        /** Length of one chunk of keepalive zeros. */
        private const val KEEPALIVE_CHUNK_MS = 40L
        /** Write the next chunk when about this much audio is left in the track. */
        private const val KEEPALIVE_LEAD_MS = 20L
        /**
         * Primed profiles top the track up when ~80 ms are left, so its ~160 ms buffer
         * ([PRIMED_BUFFER_MS]) never runs dry (no FS_FILLING BUFFER TIMEOUT on MediaTek).
         */
        private const val PRIMED_KEEPALIVE_LEAD_MS = 80L
        /** How often the sampler reads the playback head. */
        private const val HEALTH_SAMPLE_MS = 40L
        /**
         * In a call this long with no telephony output, or with every try of a profile
         * failing to open, the phone counts as unsupported (or the next profile is tried).
         */
        private const val UNSUPPORTED_AFTER_MS = 5_000L

        internal fun retryDelayMs(attempt: Int): Long =
            (attempt.coerceAtLeast(1) * 100L).coerceAtMost(500L)

        /** Soft-knee ceiling (~−2 dBFS) — avoids hard clip pops on high notes. */
        fun softLimitS16le(pcm: ByteArray, ceiling: Float): ByteArray {
            if (pcm.size < 2 || ceiling !in 0.2f..0.98f) return pcm.copyOf()
            val out = pcm.copyOf()
            val ceil = ceiling.toDouble() * Short.MAX_VALUE
            val knee = ceil * 0.85
            var i = 0
            while (i + 1 < out.size) {
                val s = ((out[i].toInt() and 0xff) or (out[i + 1].toInt() shl 8)).toShort().toDouble()
                val a = kotlin.math.abs(s)
                val shaped = if (a <= knee) {
                    s
                } else {
                    val t = ((a - knee) / (ceil - knee + 1.0)).coerceIn(0.0, 4.0)
                    val v = knee + (ceil - knee) * (t / (1.0 + t))
                    kotlin.math.sign(s) * v.coerceAtMost(ceil)
                }
                val v = shaped.toInt().coerceIn(Short.MIN_VALUE.toInt(), Short.MAX_VALUE.toInt())
                out[i] = (v and 0xff).toByte()
                out[i + 1] = ((v shr 8) and 0xff).toByte()
                i += 2
            }
            return out
        }

        /** Blend frame head toward previous last sample when jump would click. */
        fun blendJoinInPlace(pcm: ByteArray, prevLast: Short): ByteArray {
            if (pcm.size < 8) return pcm
            val first = ((pcm[0].toInt() and 0xff) or (pcm[1].toInt() shl 8)).toShort()
            if (kotlin.math.abs(first - prevLast) < JOIN_JUMP) return pcm
            val n = pcm.size / 2
            val blend = n.coerceAtMost(JOIN_SAMPLES)
            val out = pcm.copyOf()
            for (i in 0 until blend) {
                val o = i * 2
                val s = ((out[o].toInt() and 0xff) or (out[o + 1].toInt() shl 8)).toShort().toDouble()
                val g = (i + 1).toDouble() / blend
                val mixed = prevLast.toDouble() * (1.0 - g) + s * g
                val v = mixed.toInt().coerceIn(Short.MIN_VALUE.toInt(), Short.MAX_VALUE.toInt())
                out[o] = (v and 0xff).toByte()
                out[o + 1] = ((v shr 8) and 0xff).toByte()
            }
            return out
        }
    }
}

/**
 * Best-effort root path: append PCM to module FIFO/file that a Magisk
 * `service.sh` helper consumes. Marker binary/script at known paths.
 */
class RootFifoUplinkInjector : UplinkInjector {
    private val helperPaths = listOf(
        "/data/adb/modules/bageo_ai_call_agent/inject",
        "/data/adb/modules/pixelcallai_uplink/inject",
        "/data/local/tmp/pixelcallai_uplink",
    )
    private val pcmPath = "/data/local/tmp/pixelcallai_uplink.pcm"
    private var out: FileOutputStream? = null
    private var availableCached: Boolean? = null

    override fun isAvailable(): Boolean {
        availableCached?.let { return it }
        val ok = helperPaths.any { pathExistsExecutable(it) } ||
            pathExistsExecutable("/data/adb/modules/bageo_ai_call_agent") ||
            pathExistsExecutable("/data/adb/modules/pixelcallai_uplink")
        availableCached = ok
        return ok
    }

    override fun start(): Boolean {
        if (!isAvailable()) return false
        return try {
            Runtime.getRuntime().exec(
                arrayOf("su", "-c", "mkdir -p /data/local/tmp; : > $pcmPath; chmod 666 $pcmPath"),
            ).waitFor(1000, TimeUnit.MILLISECONDS)
            out = FileOutputStream(File(pcmPath), true)
            Log.i(TAG, "RootFifoUplinkInjector writing $pcmPath")
            true
        } catch (e: Exception) {
            Log.e(TAG, "fifo start failed: ${e.message}")
            false
        }
    }

    override fun writePcm(pcm: ByteArray) {
        try {
            out?.write(pcm)
            out?.flush()
        } catch (e: Exception) {
            Log.w(TAG, "fifo write failed: ${e.message}")
        }
    }

    override fun stop() {
        try {
            out?.close()
        } catch (_: Exception) {
        }
        out = null
    }

    private fun pathExistsExecutable(path: String): Boolean {
        return try {
            val p = Runtime.getRuntime().exec(arrayOf("su", "-c", "test -e $path && echo OK"))
            val finished = p.waitFor(800, TimeUnit.MILLISECONDS)
            if (!finished) {
                p.destroyForcibly()
                false
            } else {
                val text = p.inputStream.bufferedReader().readText().trim()
                p.exitValue() == 0 && text.contains("OK")
            }
        } catch (_: Exception) {
            false
        }
    }

    companion object {
        private const val TAG = "RootFifoUplink"
    }
}

/** @deprecated Use [UplinkInjectorFactory]; kept for probe-only call sites. */
class RootUplinkInjectorProbe : UplinkInjector by RootFifoUplinkInjector()

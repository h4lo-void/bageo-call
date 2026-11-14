package com.pixelcallai.streamer

import android.content.Context
import android.media.AudioAttributes
import android.media.AudioDeviceInfo
import android.media.AudioFormat
import android.media.AudioManager
import android.media.AudioTrack
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
    ): UplinkInjector {
        val telephony = TelephonyUplinkInjector(
            context,
            onUnavailableOnce = onUnavailableOnce,
            requireInCallMode = requireInCallMode,
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
 */
class TelephonyUplinkInjector(
    context: Context,
    private val sampleRate: Int = BuildConfig.SAMPLE_RATE,
    private val onUnavailableOnce: () -> Unit = {},
    /** Real cellular calls must wait for Telecom's MODE_IN_CALL before createTrack. */
    private val requireInCallMode: Boolean = false,
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
    private val unavailableNotified = AtomicBoolean(false)
    /** Last sample written — used to soften frame joins after queue pressure. */
    @Volatile private var lastSample: Short = 0
    private val joinLock = Any()

    override fun isAvailable(): Boolean {
        logTelephonyDevices()
        val tel = telephonyOutput()
        if (tel == null) {
            Log.w(TAG, "TYPE_TELEPHONY output device not found")
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
            savedMode = am.mode
            // During real OFFHOOK, mode is already MODE_IN_CALL — do not override.
            // Only nudge for idle smoke probes. A real call must not ask AudioFlinger
            // to create a VOICE_COMMUNICATION track while Telecom is still switching
            // from MODE_NORMAL; Android 16 can block createTrack and abort audioserver.
            if (!requireInCallMode && savedMode == AudioManager.MODE_NORMAL) {
                am.mode = AudioManager.MODE_IN_COMMUNICATION
            }
            Log.i(TAG, "start with audioMode=${am.mode} (saved=$savedMode)")
        } catch (e: Exception) {
            Log.w(TAG, "set mode failed: ${e.message}")
        }
        worker = Thread({
            runTrackLoop(tel)
        }, "telephony-uplink").also {
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
        try {
            am.mode = savedMode
        } catch (_: Exception) {
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

    private fun logTelephonyDevices() {
        try {
            val outs = am.getDevices(AudioManager.GET_DEVICES_OUTPUTS)
            val tel = outs.filter { it.type == AudioDeviceInfo.TYPE_TELEPHONY }
            Log.i(
                TAG,
                "outputs=${outs.size} TYPE_TELEPHONY=${tel.size} " +
                    tel.joinToString { "id=${it.id}:${it.productName}" },
            )
        } catch (e: Exception) {
            Log.w(TAG, "list devices failed: ${e.message}")
        }
    }

    private fun runTrackLoop(initialTel: AudioDeviceInfo?) {
        var attempt = 0
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
            // The start-time handle is only a first-attempt hint. If AudioManager no
            // longer lists it, later attempts must wait for a fresh device handle.
            val tel = telephonyOutput() ?: initialTel.takeIf { attempt == 1 }
            if (tel == null) {
                if (unavailableNotified.compareAndSet(false, true)) {
                    onUnavailableOnce()
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
            val ok = runOneTrack(tel, attempt)
            if (!running.get()) break
            if (ok) {
                break
            }
            routeReady.set(false)
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
    }

    /**
     * @return true if stopped cleanly (running=false); false if track died and should recreate.
     */
    private fun runOneTrack(telephony: AudioDeviceInfo, attempt: Int): Boolean {
        val channelMask = AudioFormat.CHANNEL_OUT_MONO
        val encoding = AudioFormat.ENCODING_PCM_16BIT
        val minBuf = AudioTrack.getMinBufferSize(sampleRate, channelMask, encoding)
        if (minBuf <= 0) {
            Log.e(TAG, "getMinBufferSize failed: $minBuf")
            return false
        }
        val format = AudioFormat.Builder()
            .setSampleRate(sampleRate)
            .setChannelMask(channelMask)
            .setEncoding(encoding)
            .build()
        val attrs = AudioAttributes.Builder()
            .setUsage(AudioAttributes.USAGE_VOICE_COMMUNICATION)
            .setContentType(AudioAttributes.CONTENT_TYPE_SPEECH)
            .build()
        var track: AudioTrack? = null
        try {
            track = AudioTrack.Builder()
                .setAudioAttributes(attrs)
                .setAudioFormat(format)
                // Larger stream buffer (~200 ms) reduces underrun clicks on hold music.
                .setBufferSizeInBytes((minBuf * 6).coerceAtLeast(sampleRate / 5 * 2))
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
                return false
            }
            try {
                track.setVolume(1.0f)
            } catch (e: Exception) {
                Log.w(TAG, "setVolume(1) failed: ${e.message}")
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
            val probe = ShortArray(160) // 10ms @ 16k
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
                "AudioTrack playState=PLAYING minBuf=$minBuf rate=$sampleRate " +
                    "probeWrite=$probeN prefType=${track.preferredDevice?.type} " +
                    "routedType=${routed?.type} routedName=${routed?.productName}",
            )
            if (probeN < 0) {
                Log.e(TAG, "probe write failed: $probeN — telephony path not live yet")
                return false
            }
            // Prefer TYPE_TELEPHONY is enough. Routed may show earpiece on Pixel HAL —
            // aborting here was the classic "agent plays audio, remote hears silence" bug.
            if (track.preferredDevice?.type != AudioDeviceInfo.TYPE_TELEPHONY) {
                Log.e(TAG, "preferred lost TYPE_TELEPHONY after play — abort")
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
            var consecutiveErrors = 0
            val silence = ByteArray(pcmBytesForMs(KEEPALIVE_CHUNK_MS))
            var keepaliveLogged = false
            // When the last call audio was written, and when the audio handed to the
            // track (call audio or keepalive zeros) is estimated to run out.
            var lastAudioAt = SystemClock.elapsedRealtime()
            var playoutUntil = lastAudioAt
            while (running.get()) {
                val keepaliveAt = maxOf(lastAudioAt + KEEPALIVE_IDLE_MS, playoutUntil - KEEPALIVE_LEAD_MS)
                val waitMs = (keepaliveAt - SystemClock.elapsedRealtime()).coerceIn(1L, KEEPALIVE_IDLE_MS)
                val queued = queue.poll(waitMs, TimeUnit.MILLISECONDS)
                if (queued == null) {
                    if (SystemClock.elapsedRealtime() < keepaliveAt) continue
                    val n = writeKeepalive(liveTrack, silence) ?: continue
                    if (n < 0) {
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
                        // The track's last sample is now 0; the next audio blends from there.
                        synchronized(joinLock) { lastSample = 0 }
                    }
                    playoutUntil = maxOf(SystemClock.elapsedRealtime(), playoutUntil) + KEEPALIVE_CHUNK_MS
                    continue
                }
                if (queued.pcm.isEmpty()) continue
                if (queued.clearEpoch != clearEpoch.get()) {
                    Log.i(TAG, "discard cleared PCM before write epoch=${queued.clearEpoch}")
                    continue
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
                val toWrite = synchronized(joinLock) {
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
                val writeStartedAt = SystemClock.elapsedRealtime()
                var offset = 0
                while (offset < toWrite.size && running.get()) {
                    if (queued.clearEpoch != clearEpoch.get()) {
                        Log.i(TAG, "discard cleared PCM during write epoch=${queued.clearEpoch}")
                        break
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
                        Log.e(TAG, "AudioTrack.write error: $n consecutive=$consecutiveErrors")
                        if (n == AudioTrack.ERROR_DEAD_OBJECT || consecutiveErrors >= 3) {
                            routeReady.set(false)
                            requeueFront(
                                QueuedPcm(toWrite.copyOfRange(offset, toWrite.size), queued.clearEpoch),
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
                    if (bytesWritten == 0L || bytesWritten % 32000L < n) {
                        Log.i(TAG, "DEBUG-3c97db injector write bytes=$n total=$bytesWritten state=${track.state} playState=${track.playState} queue=${queue.size}")
                    }
                    offset += n
                    bytesWritten += n.toLong()
                }
                playoutUntil = maxOf(writeStartedAt, playoutUntil) + pcmMsForBytes(offset)
                lastAudioAt = SystemClock.elapsedRealtime()
            }
            return true
        } catch (e: Exception) {
            Log.e(TAG, "telephony inject failed: ${e.message}", e)
            return false
        } finally {
            synchronized(trackLock) {
                if (activeTrack === track) activeTrack = null
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

    private fun pcmBytesForMs(ms: Long): Int = (sampleRate.toLong() * ms / 1000L).toInt() * 2

    private fun pcmMsForBytes(bytes: Int): Long = bytes.toLong() * 1000L / (sampleRate.toLong() * 2L)

    companion object {
        private const val TAG = "TelephonyUplink"
        private const val JOIN_JUMP = 2500
        private const val JOIN_SAMPLES = 32 // ~2 ms @ 16 kHz
        /** Queue empty this long → start writing keepalive zeros. */
        private const val KEEPALIVE_IDLE_MS = 100L
        /** Length of one chunk of keepalive zeros. */
        private const val KEEPALIVE_CHUNK_MS = 40L
        /** Write the next chunk when about this much audio is left in the track. */
        private const val KEEPALIVE_LEAD_MS = 20L

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
        "/data/adb/modules/pixelcallai_uplink/inject",
        "/data/local/tmp/pixelcallai_uplink",
    )
    private val pcmPath = "/data/local/tmp/pixelcallai_uplink.pcm"
    private var out: FileOutputStream? = null
    private var availableCached: Boolean? = null

    override fun isAvailable(): Boolean {
        availableCached?.let { return it }
        val ok = helperPaths.any { pathExistsExecutable(it) } ||
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

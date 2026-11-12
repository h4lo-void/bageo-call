package com.pixelcallai.streamer

import android.media.AudioFormat
import android.media.AudioRecord
import android.media.MediaRecorder
import android.util.Log
import kotlin.math.sqrt

/**
 * Capture the remote party (downlink) for the agent's VAD / speech recognition.
 * During OFFHOOK prefers VOICE_DOWNLINK then VOICE_CALL (priv-app / CAPTURE_AUDIO_OUTPUT).
 * Mic mute must not block downlink — we never rely on VOICE_COMMUNICATION for real calls.
 */
class VoiceCallCapture(private val sampleRate: Int) {
    private var recorder: AudioRecord? = null
    private var activeSource: Int = -1
    private var activeSourceName: String = "none"
    private var chunksSent: Long = 0
    private var bytesSent: Long = 0
    private var maxRms: Double = 0.0
    private var lastRmsLogMs: Long = 0
    // Larger uplink WS binary chunks when reverse-USB is always-on (still 16 kHz mono).
    val frameBytes: Int = sampleRate * 60 / 1000 * 2 // 60ms s16le mono = 1920 B @ 16 kHz

    fun open() {
        val min = AudioRecord.getMinBufferSize(
            sampleRate,
            AudioFormat.CHANNEL_IN_MONO,
            AudioFormat.ENCODING_PCM_16BIT,
        )
        val bufferSize = maxOf(min, frameBytes * 6)
        // Real-call downlink only. VOICE_COMMUNICATION can follow mic-mute and is uplink-biased.
        val sources = listOf(
            Source(MediaRecorder.AudioSource.VOICE_DOWNLINK, "VOICE_DOWNLINK"),
            Source(MediaRecorder.AudioSource.VOICE_CALL, "VOICE_CALL"),
        )
        var lastError: Exception? = null
        var bestSilent: Pair<AudioRecord, Source>? = null
        var bestSilentMaxRms = -1.0

        for (src in sources) {
            try {
                val r = AudioRecord(
                    src.id,
                    sampleRate,
                    AudioFormat.CHANNEL_IN_MONO,
                    AudioFormat.ENCODING_PCM_16BIT,
                    bufferSize,
                )
                if (r.state != AudioRecord.STATE_INITIALIZED) {
                    Log.w(TAG, "AudioRecord not initialized source=${src.name}")
                    r.release()
                    continue
                }
                r.startRecording()
                val probe = probeRms(r)
                Log.i(
                    TAG,
                    "probe source=${src.name} id=${src.id} maxRms=${"%.1f".format(probe.maxRms)} " +
                        "avgRms=${"%.1f".format(probe.avgRms)} frames=${probe.frames} ok=${probe.maxRms >= MIN_LIVE_RMS}",
                )
                // VOICE_DOWNLINK is far-end only. Callers are usually silent in the
                // first 200 ms after answer, so a quiet probe is normal — never fall
                // through to VOICE_CALL (adds our own inject on top of the caller).
                if (probe.maxRms >= MIN_LIVE_RMS || src.id == MediaRecorder.AudioSource.VOICE_DOWNLINK) {
                    // Prefer first live downlink source.
                    bestSilent?.first?.let { releaseQuiet(it) }
                    recorder = r
                    activeSource = src.id
                    activeSourceName = src.name
                    Log.i(TAG, "AudioRecord started source=${src.name} id=${src.id} rate=$sampleRate (live downlink)")
                    return
                }
                // Keep the least-silent candidate if nothing has energy yet (early OFFHOOK).
                if (probe.maxRms > bestSilentMaxRms) {
                    bestSilent?.first?.let { releaseQuiet(it) }
                    bestSilent = r to src
                    bestSilentMaxRms = probe.maxRms
                } else {
                    releaseQuiet(r)
                }
            } catch (e: Exception) {
                lastError = e
                Log.w(TAG, "AudioRecord source=${src.name} failed: ${e.message}")
            }
        }

        val fallback = bestSilent
        if (fallback != null) {
            recorder = fallback.first
            activeSource = fallback.second.id
            activeSourceName = fallback.second.name
            Log.w(
                TAG,
                "AudioRecord started source=${fallback.second.name} id=${fallback.second.id} " +
                    "rate=$sampleRate (near-silent probe maxRms=${"%.1f".format(bestSilentMaxRms)}; " +
                    "will keep streaming — mic mute must not gate downlink)",
            )
            return
        }
        throw IllegalStateException("Unable to open call downlink audio source", lastError)
    }

    /**
     * Read one frame. Returns bytes read, or <=0 on error/EOF.
     * Side-effect: periodic RMS / source logs for debugging the agent's VAD.
     */
    fun read(buf: ByteArray): Int {
        val r = recorder ?: return -1
        val n = r.read(buf, 0, buf.size)
        if (n > 0) {
            chunksSent++
            bytesSent += n.toLong()
            val rms = pcmRms(buf, n)
            if (rms > maxRms) maxRms = rms
            val now = System.currentTimeMillis()
            if (chunksSent == 1L || now - lastRmsLogMs >= RMS_LOG_INTERVAL_MS) {
                lastRmsLogMs = now
                Log.i(
                    TAG,
                    "downlink pcm source=$activeSourceName chunks=$chunksSent bytes=$bytesSent " +
                        "chunkRms=${"%.1f".format(rms)} maxRms=${"%.1f".format(maxRms)}",
                )
            }
        } else if (n < 0) {
            Log.w(TAG, "AudioRecord.read error=$n source=$activeSourceName")
        }
        return n
    }

    fun activeSourceName(): String = activeSourceName

    fun statsSummary(): String =
        "source=$activeSourceName chunks=$chunksSent bytes=$bytesSent maxRms=${"%.1f".format(maxRms)}"

    fun close() {
        Log.i(TAG, "closing ${statsSummary()}")
        try {
            recorder?.stop()
        } catch (_: Exception) {
        }
        recorder?.release()
        recorder = null
    }

    private data class Source(val id: Int, val name: String)

    private data class Probe(val maxRms: Double, val avgRms: Double, val frames: Int)

    private fun probeRms(r: AudioRecord): Probe {
        val buf = ByteArray(frameBytes)
        var max = 0.0
        var sum = 0.0
        var frames = 0
        // ~200ms warm-up to catch early remote audio after answer.
        repeat(5) {
            val n = r.read(buf, 0, buf.size)
            if (n > 0) {
                val rms = pcmRms(buf, n)
                if (rms > max) max = rms
                sum += rms
                frames++
            }
        }
        val avg = if (frames > 0) sum / frames else 0.0
        return Probe(max, avg, frames)
    }

    companion object {
        private const val TAG = "VoiceCallCapture"
        /** Below this after probe → treat as not-yet-live (keep streaming anyway). */
        private const val MIN_LIVE_RMS = 80.0
        private const val RMS_LOG_INTERVAL_MS = 1000L

        private fun releaseQuiet(r: AudioRecord) {
            try {
                r.stop()
            } catch (_: Exception) {
            }
            try {
                r.release()
            } catch (_: Exception) {
            }
        }

        fun pcmRms(buf: ByteArray, len: Int): Double {
            val n = len / 2
            if (n <= 0) return 0.0
            var acc = 0.0
            var i = 0
            while (i + 1 < len) {
                val s = (buf[i].toInt() and 0xff) or (buf[i + 1].toInt() shl 8)
                val sample = s.toShort().toDouble()
                acc += sample * sample
                i += 2
            }
            return sqrt(acc / n)
        }
    }
}

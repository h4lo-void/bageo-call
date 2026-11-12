package com.pixelcallai.streamer

import android.content.Context
import android.util.Log
import java.io.ByteArrayOutputStream
import java.nio.ByteBuffer
import java.nio.ByteOrder

/**
 * Optional phone-local greeting / wait clips (assets → UplinkInjector).
 *
 * Default: OFF (`BuildConfig.LOCAL_MENU_ENABLED=false`). The agent streams all audio
 * over the WebSocket → TYPE_TELEPHONY. The assets are kept for debugging only.
 *
 * Time-of-day buckets (Asia/Taipei):
 * morning 05–10 · noon 11–13 · afternoon 14–17 · evening 18–21 · late night → evening
 */
object LocalMenuPlayer {
    private const val TAG = "LocalMenuPlayer"
    const val WAIT_ASSET = "ivr_wait.wav"
    /** ~40 ms @ 16 kHz mono s16le. */
    private const val FRAME_BYTES = 1280

    /**
     * Load the full menu PCM for the current hour (Asia/Taipei).
     * Falls back: period file → `ivr_menu.wav` → empty.
     */
    fun loadMenuPcm(context: Context): ByteArray {
        val hour = taipeiHour()
        val primary = menuAssetForHour(hour)
        val pcm = loadAssetPcm(context, primary)
        if (pcm.isNotEmpty()) {
            Log.i(TAG, "menu asset=$primary hour=$hour (Taipei) bytes=${pcm.size}")
            return pcm
        }
        Log.w(TAG, "menu asset missing $primary hour=$hour — try ivr_menu.wav")
        return loadAssetPcm(context, "ivr_menu.wav")
    }

    /** Same time-of-day buckets as the agent's greeting clips. */
    fun menuAssetForHour(hour: Int): String = when (hour) {
        in 5..10 -> "ivr_menu_morning.wav"
        in 11..13 -> "ivr_menu_noon.wav"
        in 14..17 -> "ivr_menu_afternoon.wav"
        in 18..21 -> "ivr_menu_evening.wav"
        else -> "ivr_menu_evening.wav" // 22–4: reuse the evening greeting
    }

    fun taipeiHour(): Int {
        return try {
            val tz = java.util.TimeZone.getTimeZone("Asia/Taipei")
            val cal = java.util.Calendar.getInstance(tz)
            cal.get(java.util.Calendar.HOUR_OF_DAY)
        } catch (_: Exception) {
            java.util.Calendar.getInstance().get(java.util.Calendar.HOUR_OF_DAY)
        }
    }

    fun loadWaitPcm(context: Context): ByteArray = loadAssetPcm(context, WAIT_ASSET)

    fun loadAssetPcm(context: Context, asset: String): ByteArray {
        return try {
            context.assets.open(asset).use { input ->
                extractPcm16Mono(input.readBytes())
            }
        } catch (e: Exception) {
            Log.e(TAG, "load $asset failed: ${e.message}")
            ByteArray(0)
        }
    }

    /**
     * Inject PCM into telephony uplink. Blocks until drained or [shouldStop].
     * Pre-buffers a ~500 ms head (zero sleep), then drains at ~realtime so the full
     * clip is not truncated. The main path uses agent audio (LOCAL_MENU_ENABLED=false).
     */
    fun playInto(
        injector: UplinkInjector,
        pcm: ByteArray,
        shouldStop: () -> Boolean,
        label: String = "clip",
    ): Boolean {
        if (pcm.isEmpty()) {
            Log.w(TAG, "playInto($label): empty pcm")
            return false
        }
        val secs = pcm.size.toDouble() / (BuildConfig.SAMPLE_RATE * 2.0)
        Log.i(TAG, "local $label inject start bytes=${pcm.size} (~${"%.1f".format(secs)}s)")
        var offset = 0
        var frames = 0
        // ~0.5 s head-start (12–13 × 40 ms) burst, then 39 ms sleep ≈ 40 ms wall.
        val prebufferFrames = 13
        while (offset < pcm.size) {
            if (shouldStop()) {
                Log.i(TAG, "local $label stopped early frames=$frames")
                return frames > 0
            }
            val end = (offset + FRAME_BYTES).coerceAtMost(pcm.size)
            val chunk = pcm.copyOfRange(offset, end)
            injector.writePcm(chunk)
            offset = end
            frames++
            if (frames <= prebufferFrames) {
                continue
            }
            try {
                Thread.sleep(39L)
            } catch (_: InterruptedException) {
                break
            }
        }
        Log.i(TAG, "local $label inject done frames=$frames bytes=${pcm.size}")
        return true
    }

    /** Minimal WAV → raw s16le PCM (mono). Stereo is downmixed. */
    private fun extractPcm16Mono(wav: ByteArray): ByteArray {
        if (wav.size < 44) return ByteArray(0)
        // RIFF....WAVE
        if (wav[0] != 'R'.code.toByte() || wav[8] != 'W'.code.toByte()) {
            Log.w(TAG, "not RIFF/WAVE — treating whole file as raw pcm")
            return wav
        }
        var pos = 12
        var channels = 1
        var bits = 16
        var rate = 16000
        var data: ByteArray? = null
        while (pos + 8 <= wav.size) {
            val id = String(wav, pos, 4, Charsets.US_ASCII)
            val size = ByteBuffer.wrap(wav, pos + 4, 4).order(ByteOrder.LITTLE_ENDIAN).int
            val payload = pos + 8
            if (size < 0 || payload + size > wav.size + 1) break
            when (id) {
                "fmt " -> {
                    if (size >= 16) {
                        val bb = ByteBuffer.wrap(wav, payload, size).order(ByteOrder.LITTLE_ENDIAN)
                        bb.short // format
                        channels = bb.short.toInt() and 0xffff
                        rate = bb.int
                        bb.int // byte rate
                        bb.short // block align
                        bits = bb.short.toInt() and 0xffff
                    }
                }
                "data" -> {
                    data = wav.copyOfRange(payload, payload + size)
                    break
                }
            }
            pos = payload + size + (size and 1) // word align
        }
        val raw = data ?: return ByteArray(0)
        if (bits != 16) {
            Log.w(TAG, "unsupported bits=$bits (need 16)")
            return ByteArray(0)
        }
        Log.i(TAG, "wav fmt channels=$channels rate=$rate bits=$bits data=${raw.size}")
        if (channels <= 1) return raw
        // Downmix stereo → mono
        val samples = raw.size / 4
        val out = ByteArray(samples * 2)
        val bb = ByteBuffer.wrap(raw).order(ByteOrder.LITTLE_ENDIAN)
        val ob = ByteBuffer.wrap(out).order(ByteOrder.LITTLE_ENDIAN)
        for (i in 0 until samples) {
            val l = bb.short.toInt()
            val r = bb.short.toInt()
            ob.putShort(((l + r) / 2).toShort())
        }
        return out
    }
}

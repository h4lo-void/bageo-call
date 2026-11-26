// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

package com.pixelcallai.streamer

/**
 * True on a MediaTek chip: [Build.SOC_MANUFACTURER] (API 31+) is "MediaTek"/"Mediatek"
 * (any case), otherwise [Build.HARDWARE] starts with "mt" (any case). Arguments are
 * passed in so this can be unit-tested.
 */
fun isMediaTekChip(sdkInt: Int, socManufacturer: String?, hardware: String?): Boolean =
    if (sdkInt >= 31) {
        socManufacturer.orEmpty().trim().equals("MediaTek", ignoreCase = true)
    } else {
        hardware.orEmpty().trim().startsWith("mt", ignoreCase = true)
    }

/**
 * Picks the sample rate for the telephony uplink AudioTrack.
 *
 * [deviceRates] is [android.media.AudioDeviceInfo.getSampleRates] of the TELEPHONY output.
 * Only on [mediatek] chips (MT6985 lists only 44100 and 48000 and resets its HiFi output
 * path every time a track with another rate is opened) can the rate change: an empty array
 * means "any rate" and a list holding [inputRate] takes the agent's rate as is, both keep
 * [inputRate]; otherwise take 48000, then 44100, then the largest rate listed. Every other
 * chip (Pixel 9 lists only 48000 but must stay untouched) always gets [inputRate].
 */
fun pickTelephonyTrackRate(deviceRates: IntArray, inputRate: Int, mediatek: Boolean): Int {
    if (!mediatek || deviceRates.isEmpty() || deviceRates.contains(inputRate)) return inputRate
    if (deviceRates.contains(48000)) return 48000
    if (deviceRates.contains(44100)) return 44100
    return deviceRates.maxOrNull() ?: inputRate
}

/**
 * Streaming linear-interpolation resampler for mono s16le PCM.
 *
 * Output sample k sits at input position k * inRate / outRate - 1 (one input sample of
 * delay), and the sample before the first one is taken equal to the first one. That way
 * every input sample yields exactly outRate / inRate output samples (16000 to 48000 gives
 * 3N for N), no output ever needs the next chunk, and the state carried between chunks is
 * one sample, so chunk boundaries cannot add clicks or drop samples. Positions are exact
 * integer fractions, so the result does not depend on how the stream is cut into chunks.
 * Not thread-safe: use one instance per track.
 */
class PcmResampler(private val inRate: Int, private val outRate: Int) {
    init {
        require(inRate > 0 && outRate > 0) { "rates must be positive" }
    }

    /** Input samples consumed so far. */
    private var inTotal = 0L
    /** Index of the next output sample. */
    private var nextOut = 0L
    /** The last input sample consumed (the first one before any input). */
    private var last = 0
    private var started = false
    /** Low byte of a sample whose high byte has not arrived yet. */
    private var carryByte = -1

    fun process(pcm: ByteArray): ByteArray {
        if (pcm.isEmpty()) return pcm
        var bytes = pcm
        var len = pcm.size
        if (carryByte >= 0) {
            bytes = ByteArray(len + 1)
            bytes[0] = carryByte.toByte()
            System.arraycopy(pcm, 0, bytes, 1, len)
            len += 1
            carryByte = -1
        }
        val n = len / 2
        if (len % 2 == 1) carryByte = bytes[len - 1].toInt() and 0xff
        val input = ShortArray(n)
        for (i in 0 until n) {
            input[i] = ((bytes[2 * i].toInt() and 0xff) or (bytes[2 * i + 1].toInt() shl 8)).toShort()
        }
        val out = processSamples(input)
        val res = ByteArray(out.size * 2)
        for (i in out.indices) {
            val v = out[i].toInt()
            res[2 * i] = (v and 0xff).toByte()
            res[2 * i + 1] = (v shr 8).toByte()
        }
        return res
    }

    fun processSamples(input: ShortArray): ShortArray {
        if (input.isEmpty()) return ShortArray(0)
        if (!started) {
            started = true
            last = input[0].toInt()
        }
        val total = inTotal + input.size
        // Output k needs input index floor(k * in / out - 1) + 1 <= total - 1.
        val endOut = (total * outRate + inRate - 1) / inRate
        val count = (endOut - nextOut).toInt()
        val out = ShortArray(count)
        var k = nextOut
        for (j in 0 until count) {
            val num = k * inRate - outRate
            val idx = Math.floorDiv(num, outRate.toLong())
            val frac = num - idx * outRate
            val a = sampleAt(idx, input)
            val b = sampleAt(idx + 1, input)
            val v = Math.floorDiv(a * (outRate - frac) + b * frac + outRate / 2, outRate.toLong())
            out[j] = v.coerceIn(Short.MIN_VALUE.toLong(), Short.MAX_VALUE.toLong()).toShort()
            k++
        }
        nextOut = endOut
        last = input[input.size - 1].toInt()
        inTotal = total
        return out
    }

    /** Input sample with global index [g]; only [inTotal] - 1 and this chunk are reachable. */
    private fun sampleAt(g: Long, input: ShortArray): Long =
        if (g < inTotal) last.toLong() else input[(g - inTotal).toInt()].toLong()
}

// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

package com.pixelcallai.streamer

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import kotlin.math.PI
import kotlin.math.abs
import kotlin.math.sin

class PcmResamplerTest {
    private fun sine(n: Int, freq: Double, amp: Double): ShortArray =
        ShortArray(n) { Math.round(amp * sin(2.0 * PI * freq * it / 16000.0)).toInt().toShort() }

    @Test
    fun track_rate_unlimited_device_keeps_input_rate() {
        assertEquals(16000, pickTelephonyTrackRate(intArrayOf(), 16000, true))
    }

    @Test
    fun track_rate_device_listing_input_rate_keeps_it() {
        assertEquals(16000, pickTelephonyTrackRate(intArrayOf(16000, 48000), 16000, true))
    }

    @Test
    fun track_rate_prefers_48000_then_44100_then_largest() {
        assertEquals(48000, pickTelephonyTrackRate(intArrayOf(44100, 48000), 16000, true))
        assertEquals(44100, pickTelephonyTrackRate(intArrayOf(44100), 16000, true))
        assertEquals(32000, pickTelephonyTrackRate(intArrayOf(8000, 32000), 16000, true))
    }

    @Test
    fun track_rate_non_mediatek_always_keeps_input_rate() {
        assertEquals(16000, pickTelephonyTrackRate(intArrayOf(48000), 16000, false))
        assertEquals(16000, pickTelephonyTrackRate(intArrayOf(44100, 48000), 16000, false))
    }

    @Test
    fun mediatek_chip_detection() {
        assertTrue(isMediaTekChip(34, "MediaTek", "mt6985"))
        assertTrue(isMediaTekChip(34, "Mediatek", "x"))
        assertTrue(isMediaTekChip(34, "MEDIATEK", "x"))
        assertTrue(!isMediaTekChip(34, "Google", "mt6985"))
        assertTrue(!isMediaTekChip(34, "", "mt6985"))
        assertTrue(isMediaTekChip(30, "", "MT6893"))
        assertTrue(!isMediaTekChip(30, "MediaTek", "tensor"))
        assertTrue(!isMediaTekChip(34, null, null))
    }

    @Test
    fun upsample_to_48k_triples_the_length() {
        for (n in listOf(1, 2, 160, 161, 1000)) {
            assertEquals(3 * n, PcmResampler(16000, 48000).processSamples(ShortArray(n)).size)
        }
    }

    @Test
    fun dc_stays_dc() {
        val out = PcmResampler(16000, 48000).processSamples(ShortArray(500) { 1234.toShort() })
        assertTrue(out.all { it.toInt() == 1234 })
        val neg = PcmResampler(16000, 44100).processSamples(ShortArray(500) { (-20000).toShort() })
        assertTrue(neg.all { it.toInt() == -20000 })
    }

    @Test
    fun sine_values_follow_the_input_with_one_sample_delay() {
        val amp = 10000.0
        val freq = 200.0
        val out = PcmResampler(16000, 48000).processSamples(sine(800, freq, amp))
        for (k in 3 until out.size) {
            val expected = amp * sin(2.0 * PI * freq * (k / 48000.0 - 1.0 / 16000.0))
            assertTrue("k=$k got=${out[k]} expected=$expected", abs(out[k] - expected) <= 15.0)
        }
    }

    @Test
    fun irregular_chunks_give_the_same_samples_as_one_pass() {
        for (outRate in listOf(48000, 44100)) {
            val input = sine(3000, 440.0, 12000.0)
            val whole = PcmResampler(16000, outRate).processSamples(input)
            val r = PcmResampler(16000, outRate)
            val parts = ArrayList<Short>()
            var pos = 0
            val sizes = intArrayOf(1, 7, 160, 3, 333, 2, 480, 99, 1)
            var i = 0
            while (pos < input.size) {
                val size = minOf(sizes[i % sizes.size], input.size - pos)
                i++
                r.processSamples(input.copyOfRange(pos, pos + size)).forEach { parts.add(it) }
                pos += size
            }
            assertEquals(whole.size, parts.size)
            for (k in whole.indices) {
                assertTrue("outRate=$outRate k=$k", abs(whole[k] - parts[k]) <= 1)
            }
        }
    }

    @Test
    fun upsample_to_44k_length_is_within_one_sample() {
        for (n in listOf(160, 1600, 16000, 3333)) {
            val out = PcmResampler(16000, 44100).processSamples(ShortArray(n))
            val exact = n * 44100.0 / 16000.0
            assertTrue("n=$n got=${out.size}", abs(out.size - exact) <= 1.0)
        }
    }

    @Test
    fun byte_api_handles_odd_splits_like_one_pass() {
        val input = sine(600, 300.0, 9000.0)
        val bytes = ByteArray(input.size * 2)
        for (i in input.indices) {
            bytes[2 * i] = (input[i].toInt() and 0xff).toByte()
            bytes[2 * i + 1] = (input[i].toInt() shr 8).toByte()
        }
        val whole = PcmResampler(16000, 48000).process(bytes)
        val r = PcmResampler(16000, 48000)
        val joined = r.process(bytes.copyOfRange(0, 101)) +
            r.process(bytes.copyOfRange(101, 640)) +
            r.process(bytes.copyOfRange(640, bytes.size))
        assertEquals(bytes.size * 3, whole.size)
        assertArrayEquals(whole, joined)
    }
}

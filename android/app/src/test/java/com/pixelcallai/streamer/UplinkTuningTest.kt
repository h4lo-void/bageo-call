// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

package com.pixelcallai.streamer

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class UplinkTuningTest {
    private val k60Caps = TelephonyCaps(sampleRates = listOf(44100, 48000))
    private val pixel9Caps = TelephonyCaps(sampleRates = listOf(48000))

    // ---------------------------------------------------------------- profiles

    @Test
    fun standard_is_the_old_non_mediatek_track() {
        for (caps in listOf(TelephonyCaps(), pixel9Caps, k60Caps)) {
            for (chip in listOf(ChipKind.GOOGLE, ChipKind.OTHER)) {
                val plan = planTrack(UplinkProfile.STANDARD, caps, chip, 16000)
                assertEquals(pickTelephonyTrackRate(caps.sampleRates.toIntArray(), 16000, false), plan.trackRate)
                assertEquals(1, plan.channels)
                assertFalse(plan.primed)
                for (minBuf in listOf(640, 1280, 1920, 12300)) {
                    // b825ea2, non-MediaTek: (minBuf * 6).coerceAtLeast(trackRate / 5 * 2)
                    assertEquals((minBuf * 6).coerceAtLeast(16000 / 5 * 2), profileBufferBytes(plan, minBuf))
                }
            }
        }
    }

    @Test
    fun mediatek_start_is_b825ea2_on_k60_ultra() {
        val (start, source) = pickStartProfile(null, null, null, ChipKind.MEDIATEK, k60Caps, 16000, false)
        assertEquals(UplinkProfile.PRIMED, start)
        assertEquals(StartSource.CHIP, source)
        val plan = planTrack(start, k60Caps, ChipKind.MEDIATEK, 16000)
        // b825ea2: pickTelephonyTrackRate(rates, 16000, mediatek = true), mono, 160 ms.
        assertEquals(pickTelephonyTrackRate(intArrayOf(44100, 48000), 16000, true), plan.trackRate)
        assertEquals(48000, plan.trackRate)
        assertEquals(1, plan.channels)
        assertTrue(plan.primed)
        // minBuf 12,300 bytes at 48 kHz → minBuf.coerceAtLeast(160 ms) = 15,360 bytes.
        assertEquals(15360, profileBufferBytes(plan, 12300))
        assertEquals(20000, profileBufferBytes(plan, 20000))
    }

    @Test
    fun mediatek_stays_mono_even_if_it_lists_stereo_only() {
        val caps = TelephonyCaps(sampleRates = listOf(48000), channelCounts = listOf(2))
        assertEquals(1, planTrack(UplinkProfile.PRIMED, caps, ChipKind.MEDIATEK, 16000).channels)
        assertEquals(1, planTrack(UplinkProfile.STANDARD, caps, ChipKind.GOOGLE, 16000).channels)
        assertEquals(2, planTrack(UplinkProfile.STANDARD, caps, ChipKind.OTHER, 16000).channels)
        assertEquals(2, planTrack(UplinkProfile.PRIMED_48K_STEREO, caps, ChipKind.MEDIATEK, 16000).channels)
    }

    @Test
    fun profile_rates() {
        val none = IntArray(0)
        assertEquals(16000, pickProfileTrackRate(UplinkProfile.STANDARD, intArrayOf(48000), 16000))
        assertEquals(16000, pickProfileTrackRate(UplinkProfile.PRIMED, none, 16000))
        assertEquals(16000, pickProfileTrackRate(UplinkProfile.PRIMED, intArrayOf(16000, 48000), 16000))
        assertEquals(48000, pickProfileTrackRate(UplinkProfile.PRIMED, intArrayOf(44100, 48000), 16000))
        assertEquals(48000, pickProfileTrackRate(UplinkProfile.PRIMED_48K, none, 16000))
        assertEquals(48000, pickProfileTrackRate(UplinkProfile.PRIMED_48K, intArrayOf(16000, 48000), 16000))
        assertEquals(44100, pickProfileTrackRate(UplinkProfile.PRIMED_48K, intArrayOf(16000, 44100), 16000))
        assertEquals(32000, pickProfileTrackRate(UplinkProfile.PRIMED_48K_STEREO, intArrayOf(8000, 32000), 16000))
    }

    @Test
    fun profile_buffers() {
        val primed16 = planTrack(UplinkProfile.PRIMED, TelephonyCaps(), ChipKind.OTHER, 16000)
        assertEquals(16000, primed16.trackRate)
        assertEquals(5120, profileBufferBytes(primed16, 1280))
        val stereo = planTrack(UplinkProfile.PRIMED_48K_STEREO, TelephonyCaps(), ChipKind.OTHER, 16000)
        assertEquals(48000, stereo.trackRate)
        assertEquals(2, stereo.channels)
        assertEquals(30720, profileBufferBytes(stereo, 12300))
        val standardStereo = UplinkTrackPlan(16000, 2, false)
        assertEquals(12800, profileBufferBytes(standardStereo, 100))
        assertEquals(160L, pcmMsFor(30720, 48000, 2))
    }

    @Test
    fun mono_to_stereo_duplicates_each_sample() {
        val mono = byteArrayOf(1, 2, 3, 4)
        assertArrayEquals(byteArrayOf(1, 2, 1, 2, 3, 4, 3, 4), monoToStereo(mono))
        assertArrayEquals(ByteArray(0), monoToStereo(ByteArray(0)))
    }

    @Test
    fun start_profile_order() {
        fun start(
            forced: UplinkProfile? = null,
            avoid: UplinkProfile? = null,
            remembered: UplinkProfile? = null,
            chip: ChipKind = ChipKind.OTHER,
            caps: TelephonyCaps = TelephonyCaps(),
            measureOnly: Boolean = false,
        ) = pickStartProfile(forced, avoid, remembered, chip, caps, 16000, measureOnly)

        assertEquals(UplinkProfile.STANDARD to StartSource.DEFAULT, start())
        assertEquals(UplinkProfile.PRIMED to StartSource.CAPS, start(caps = pixel9Caps))
        assertEquals(UplinkProfile.STANDARD to StartSource.DEFAULT, start(caps = TelephonyCaps(listOf(16000, 48000))))
        assertEquals(UplinkProfile.PRIMED_48K to StartSource.REMEMBERED, start(remembered = UplinkProfile.PRIMED_48K))
        assertEquals(
            UplinkProfile.PRIMED to StartSource.OWNER_REPORT,
            start(avoid = UplinkProfile.STANDARD, remembered = UplinkProfile.STANDARD),
        )
        assertEquals(UplinkProfile.STANDARD to StartSource.FORCED, start(forced = UplinkProfile.STANDARD, chip = ChipKind.MEDIATEK))
        // Google: measure only keeps the b825ea2 path and ignores what is stored.
        assertEquals(
            UplinkProfile.STANDARD to StartSource.DEFAULT,
            start(remembered = UplinkProfile.PRIMED, avoid = UplinkProfile.STANDARD, chip = ChipKind.GOOGLE, caps = pixel9Caps, measureOnly = true),
        )
        assertEquals(UplinkProfile.PRIMED to StartSource.CHIP, start(chip = ChipKind.MEDIATEK, measureOnly = true))
        // A Google phone with tuning on still starts like before, whatever it lists.
        assertEquals(UplinkProfile.STANDARD to StartSource.DEFAULT, start(chip = ChipKind.GOOGLE, caps = pixel9Caps))
    }

    @Test
    fun owner_report_skips_profiles_that_build_the_same_track() {
        val same = { a: UplinkProfile, b: UplinkProfile ->
            planTrack(a, k60Caps, ChipKind.MEDIATEK, 16000) == planTrack(b, k60Caps, ChipKind.MEDIATEK, 16000)
        }
        // On the K60 Ultra primed already runs at 48 kHz, so primed-48k is the same track.
        val (start, source) = pickStartProfile(null, UplinkProfile.PRIMED, null, ChipKind.MEDIATEK, k60Caps, 16000, false, same)
        assertEquals(UplinkProfile.PRIMED_48K_STEREO, start)
        assertEquals(StartSource.OWNER_REPORT, source)
    }

    // ---------------------------------------------------------------- head position

    @Test
    fun head_advance_handles_wrap_and_steps_back() {
        assertEquals(300L, headAdvanceFrames(100, 400))
        assertEquals(300L, headAdvanceFrames(-100, 200))
        assertEquals(10L, headAdvanceFrames(Int.MAX_VALUE - 4, Int.MIN_VALUE + 5))
        assertEquals(0L, headAdvanceFrames(500, 400))
        assertEquals(0L, headAdvanceFrames(1000, 0))
    }

    // ---------------------------------------------------------------- health meter

    /** Drives a meter with a head that follows [headAt] (ms → frames), sampled every [step] ms. */
    private fun run(
        meter: UplinkHealthMeter,
        rate: Int,
        fromMs: Long,
        toMs: Long,
        step: Long = 40,
        startHead: Int = 0,
        headAt: (Long) -> Long,
    ): Boolean {
        meter.beginTrack(fromMs, rate, startHead)
        var unhealthy = false
        var t = fromMs + step
        while (t <= toMs) {
            meter.sample(t, (startHead.toLong() + headAt(t - fromMs)).toInt())
            if (meter.isUnhealthy(t)) unhealthy = true
            t += step
        }
        return unhealthy
    }

    @Test
    fun healthy_track_playing_in_real_time() {
        val m = UplinkHealthMeter()
        assertFalse(run(m, 16000, 0, 30_000) { it * 16 })
        assertEquals(0L, m.lostMs)
        assertEquals(0, m.lossEvents)
    }

    @Test
    fun healthy_track_with_100ms_mixer_periods() {
        val m = UplinkHealthMeter()
        assertFalse(run(m, 48000, 0, 30_000) { (it / 100) * 4800 })
        assertEquals(0L, m.lostMs)
    }

    @Test
    fun long_silence_underrun_and_cleared_queue_are_healthy() {
        val m = UplinkHealthMeter()
        // Plays 1 s, stops for 3 s (empty queue or cleared), plays 2 s, stops 0.5 s, plays.
        val head = { t: Long ->
            when {
                t < 1000 -> t
                t < 4000 -> 1000L
                t < 6000 -> 1000L + (t - 4000)
                t < 6500 -> 3000L
                else -> 3000L + (t - 6500)
            } * 16
        }
        assertFalse(run(m, 16000, 0, 20_000, headAt = head))
        assertEquals(0L, m.lostMs)
    }

    @Test
    fun keepalive_silence_at_real_time_with_late_sampler_is_healthy() {
        val m = UplinkHealthMeter()
        // The sampler thread was starved for a second: a 1 s step is skipped, not counted.
        m.beginTrack(0, 16000, 0)
        m.sample(40, 640)
        assertEquals(0L, m.sample(1040, 16640))
        assertEquals(0L, m.sample(1080, 17280))
        assertFalse(m.isUnhealthy(1080))
    }

    @Test
    fun track_rebuilt_by_the_server_every_240ms_is_unhealthy() {
        val m = UplinkHealthMeter()
        // Head stands still, then restoreTrack_l jumps it by the ~240 ms the app had written.
        val unhealthy = run(m, 48000, 0, 3_000) { t -> (t / 240) * 240 * 48 }
        assertTrue(unhealthy)
        assertTrue("lost=${m.lostMs}", m.lostMs >= 1000)
        assertTrue(m.isUnhealthy(3_000))
        assertTrue(m.lossEvents >= 5)
    }

    @Test
    fun one_route_change_at_call_start_is_not_unhealthy() {
        val m = UplinkHealthMeter()
        // A single 500 ms jump at 1 s, real time otherwise.
        val unhealthy = run(m, 16000, 0, 15_000) { t -> (t + if (t >= 1000) 500 else 0) * 16 }
        assertFalse(unhealthy)
        assertEquals(1, m.lossEvents)
        assertTrue(m.lostMs in 300L..500L)
    }

    @Test
    fun startup_burst_within_credit_is_healthy() {
        val m = UplinkHealthMeter()
        // The output pulls 100 ms at once right after start, then real time.
        assertFalse(run(m, 48000, 0, 5_000) { t -> (t + 100) * 48 })
        assertEquals(0L, m.lostMs)
    }

    @Test
    fun wrap_around_while_healthy_is_not_a_loss() {
        val m = UplinkHealthMeter()
        val start = Int.MAX_VALUE - 48000 // wraps after one second at 48 kHz
        assertFalse(run(m, 48000, 0, 5_000, startHead = start) { it * 48 })
        assertEquals(0L, m.lostMs)
    }

    @Test
    fun repeated_app_rebuilds_are_unhealthy() {
        val m = UplinkHealthMeter()
        m.beginTrack(0, 16000, 0)
        m.noteRebuild(100, 0)
        m.noteRebuild(400, 0)
        assertFalse(m.isUnhealthy(400))
        m.noteRebuild(700, 0)
        assertTrue(m.isUnhealthy(700))
        assertFalse(m.isUnhealthy(6000))
        assertEquals(3, m.rebuilds)
    }

    @Test
    fun audio_ms_counts_call_audio_frames() {
        val m = UplinkHealthMeter()
        m.beginTrack(0, 48000, 0)
        m.addAudioFrames(48000 * 12)
        assertEquals(12_000L, m.audioMs)
    }

    // ---------------------------------------------------------------- ladder

    /** One 40 ms sample on the current profile that loses [lostMs] beyond what is allowed. */
    private fun lose(t: UplinkTuner, rate: Int, lostMs: Long) {
        val allowedMs = UplinkHealthMeter.CREDIT_CAP_MS + 40
        t.meter().apply {
            beginTrack(0, rate, 0)
            assertEquals(lostMs, sample(40, (rate * (allowedMs + lostMs) / 1000).toInt()))
        }
    }

    @Test
    fun ladder_climbs_then_settles_on_the_least_lossy() {
        val t = UplinkTuner(UplinkProfile.STANDARD, StartSource.FORCED, autoSwitch = true)
        t.markLive()
        lose(t, 16000, 1000)
        assertEquals(UplinkProfile.PRIMED, t.onUnhealthy())
        lose(t, 16000, 50)
        assertEquals(UplinkProfile.PRIMED_48K, t.onUnhealthy())
        lose(t, 48000, 900)
        assertEquals(UplinkProfile.PRIMED_48K_STEREO, t.onUnhealthy())
        lose(t, 48000, 900)
        // Every profile tried: back to the one that lost the least, once.
        assertEquals(UplinkProfile.PRIMED, t.onUnhealthy())
        assertNull(t.onUnhealthy())
        assertEquals(4, t.switches)
        val d = t.decision()
        assertEquals(AudioFit.PROBLEM, d.fit)
        assertTrue(d.forget)
        assertNull(d.remember)
    }

    @Test
    fun ladder_from_mediatek_skips_the_same_track_and_wraps() {
        val same = { a: UplinkProfile, b: UplinkProfile ->
            planTrack(a, k60Caps, ChipKind.MEDIATEK, 16000) == planTrack(b, k60Caps, ChipKind.MEDIATEK, 16000)
        }
        val t = UplinkTuner(UplinkProfile.PRIMED, StartSource.CHIP, autoSwitch = true, samePlan = same)
        assertEquals(UplinkProfile.PRIMED_48K_STEREO, t.onUnhealthy())
        assertEquals(UplinkProfile.STANDARD, t.onUnhealthy())
    }

    @Test
    fun measure_only_never_switches_but_records_problem() {
        val t = UplinkTuner(UplinkProfile.STANDARD, StartSource.DEFAULT, autoSwitch = false)
        t.markLive()
        assertNull(t.onUnhealthy())
        assertEquals(UplinkProfile.STANDARD, t.current)
        val d = t.decision()
        assertEquals(AudioFit.PROBLEM, d.fit)
        assertNull(d.remember)
        assertFalse(d.forget)
    }

    @Test
    fun switch_then_healthy_long_call_is_adjusted_and_remembered() {
        val t = UplinkTuner(UplinkProfile.STANDARD, StartSource.FORCED, autoSwitch = true)
        t.markLive()
        assertEquals(UplinkProfile.PRIMED, t.onUnhealthy())
        t.meter().apply { beginTrack(0, 48000, 0); addAudioFrames(48000L * 15) }
        val d = t.decision()
        assertEquals(TuneDecision(remember = UplinkProfile.PRIMED, fit = AudioFit.ADJUSTED), d)
    }

    @Test
    fun open_failures_on_every_profile_are_unsupported() {
        val t = UplinkTuner(UplinkProfile.STANDARD, StartSource.DEFAULT, autoSwitch = true)
        assertEquals(UplinkProfile.PRIMED, t.onOpenFailed("prefer_rejected"))
        assertEquals(UplinkProfile.PRIMED_48K, t.onOpenFailed("prefer_rejected"))
        assertEquals(UplinkProfile.PRIMED_48K_STEREO, t.onOpenFailed("track_rejected"))
        assertNull(t.onOpenFailed("prefer_rejected"))
        val d = t.decision()
        assertEquals(AudioFit.UNSUPPORTED, d.fit)
        assertEquals("prefer_rejected", d.reason)
    }

    @Test
    fun no_telephony_device_is_unsupported_and_a_live_track_wins() {
        val t = UplinkTuner(UplinkProfile.STANDARD, StartSource.DEFAULT, autoSwitch = true)
        t.markNoDevice()
        assertEquals(TuneDecision(forget = true, fit = AudioFit.UNSUPPORTED, reason = "no_telephony_device"), t.decision())
        t.markLive()
        assertEquals(TuneDecision(), t.decision())
    }

    @Test
    fun one_failed_open_is_not_unsupported_yet() {
        val t = UplinkTuner(UplinkProfile.STANDARD, StartSource.DEFAULT, autoSwitch = true)
        t.onOpenFailed("prefer_rejected")
        assertEquals(TuneDecision(), t.decision())
    }

    // ---------------------------------------------------------------- memory

    @Test
    fun memory_rules() {
        val long = MIN_MEASURED_AUDIO_MS
        val short = MIN_MEASURED_AUDIO_MS - 1
        val p = UplinkProfile.PRIMED
        // First profile healthy through a long call: ok and remembered.
        assertEquals(TuneDecision(remember = p, fit = AudioFit.OK), decideTune(true, StartSource.CHIP, p, false, false, false, long))
        // Too short: nothing changes.
        assertEquals(TuneDecision(), decideTune(true, StartSource.CHIP, p, false, false, false, short))
        // The remembered profile went bad and the next one was too short to prove: forget, untested.
        assertEquals(
            TuneDecision(forget = true, fit = AudioFit.UNTESTED),
            decideTune(true, StartSource.REMEMBERED, UplinkProfile.PRIMED_48K, true, true, false, short),
        )
        // Same with a long call: the new profile replaces it.
        assertEquals(
            TuneDecision(remember = UplinkProfile.PRIMED_48K, fit = AudioFit.ADJUSTED),
            decideTune(true, StartSource.REMEMBERED, UplinkProfile.PRIMED_48K, true, true, false, long),
        )
        // Measure only: long healthy → ok, short → nothing, never a profile.
        assertEquals(TuneDecision(fit = AudioFit.OK), decideTune(false, StartSource.DEFAULT, UplinkProfile.STANDARD, false, false, false, long))
        assertEquals(TuneDecision(), decideTune(false, StartSource.DEFAULT, UplinkProfile.STANDARD, false, false, false, short))
    }

    @Test
    fun store_keys_follow_the_fingerprint() {
        val fp = "brand/product/device:14/ID/1:user/release-keys"
        assertEquals("profile@$fp", UplinkTuneStore.key("profile", fp))
        val keys = listOf("profile@$fp", "fit@$fp", "profile@old/fp", "fit@old/fp")
        assertEquals(listOf("profile@old/fp", "fit@old/fp"), UplinkTuneStore.staleKeys(keys, fp))
    }

    @Test
    fun ids_round_trip() {
        for (p in UplinkProfile.entries) assertEquals(p, UplinkProfile.parse(p.id))
        assertEquals(UplinkProfile.PRIMED_48K, UplinkProfile.parse(" Primed-48K "))
        assertNull(UplinkProfile.parse("loud"))
        assertNull(UplinkProfile.parse(null))
        for (f in AudioFit.entries) assertEquals(f, AudioFit.parse(f.id))
    }

    @Test
    fun chip_kinds() {
        assertEquals(ChipKind.GOOGLE, chipKind("Google", false))
        assertEquals(ChipKind.MEDIATEK, chipKind("Xiaomi", true))
        assertEquals(ChipKind.OTHER, chipKind("samsung", false))
    }
}

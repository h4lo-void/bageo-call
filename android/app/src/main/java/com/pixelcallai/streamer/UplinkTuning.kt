// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

package com.pixelcallai.streamer

/**
 * Settings for the AudioTrack that carries the AI's voice into the call, tried in this
 * order (smallest change first) when the track on this phone turns out unhealthy.
 *
 * - [STANDARD]: the original track (input rate, ~6x minBuf, starts from a 10 ms probe).
 * - [PRIMED]: small buffer (~160 ms) filled with silence at play() plus a start threshold,
 *   so AudioFlinger leaves FS_FILLING on its first cycle; the rate is converted only when
 *   the device lists rates without the input rate.
 * - [PRIMED_48K]: [PRIMED] at 48 kHz (or what the device lists).
 * - [PRIMED_48K_STEREO]: [PRIMED_48K] with the mono voice copied to two channels.
 *
 * Approach informed by gsm2sip (MIT), https://github.com/pulpoff/gsm2sip: settings per
 * chip family with a generic fallback, and a stereo Telephony Tx track.
 */
enum class UplinkProfile(val id: String, val primed: Boolean, val rate48k: Boolean, val stereo: Boolean) {
    STANDARD("standard", false, false, false),
    PRIMED("primed", true, false, false),
    PRIMED_48K("primed-48k", true, true, false),
    PRIMED_48K_STEREO("primed-48k-stereo", true, true, true),
    ;

    companion object {
        fun parse(id: String?): UplinkProfile? {
            val key = id?.trim()?.lowercase().orEmpty()
            return entries.firstOrNull { it.id == key }
        }
    }
}

/** How well this phone carries the AI's voice into calls (sent to Bageo as `audio_fit`). */
enum class AudioFit(val id: String) {
    /** No call long enough to judge yet. */
    UNTESTED("untested"),
    /** The first settings tried worked for a whole call. */
    OK("ok"),
    /** Other settings were needed; they worked and are remembered. */
    ADJUSTED("adjusted"),
    /** Audio still got lost with every setting, or the owner said the caller heard nothing. */
    PROBLEM("problem"),
    /** No telephony output to play into, or the phone refused every setting. */
    UNSUPPORTED("unsupported"),
    ;

    companion object {
        fun parse(id: String?): AudioFit? {
            val key = id?.trim()?.lowercase().orEmpty()
            return entries.firstOrNull { it.id == key }
        }
    }
}

/** Why a call started from its profile. */
enum class StartSource(val id: String) {
    /** `setprop debug.bageo.uplink_profile <id>` (testing). */
    FORCED("forced"),
    /** The owner said the caller did not hear the AI: the next profile after the last one used. */
    OWNER_REPORT("owner_report"),
    REMEMBERED("remembered"),
    /** Known chip family (MediaTek). */
    CHIP("chip"),
    /** The telephony output's listed capabilities. */
    CAPS("caps"),
    DEFAULT("default"),
}

/** Google (Pixel) phones, MediaTek chips, everything else. */
enum class ChipKind { GOOGLE, MEDIATEK, OTHER }

fun chipKind(manufacturer: String?, mediatek: Boolean): ChipKind = when {
    manufacturer.orEmpty().trim().equals("Google", ignoreCase = true) -> ChipKind.GOOGLE
    mediatek -> ChipKind.MEDIATEK
    else -> ChipKind.OTHER
}

/** What the TELEPHONY output device lists (empty = not listed, i.e. "any"). */
data class TelephonyCaps(
    val sampleRates: List<Int> = emptyList(),
    val channelCounts: List<Int> = emptyList(),
    val channelMasks: List<Int> = emptyList(),
    val encodings: List<Int> = emptyList(),
) {
    fun describe(): String =
        "rates=${sampleRates.joinToString(",", "[", "]")} " +
            "channels=${channelCounts.joinToString(",", "[", "]")} " +
            "masks=${channelMasks.joinToString(",", "[", "]")} " +
            "encodings=${encodings.joinToString(",", "[", "]")}"
}

/** Rate, channels and buffer of the track for one profile on this phone. */
data class UplinkTrackPlan(val trackRate: Int, val channels: Int, val primed: Boolean)

/**
 * Track sample rate for [profile]. [STANDARD][UplinkProfile.STANDARD] always keeps
 * [inputRate]; [PRIMED][UplinkProfile.PRIMED] is [pickTelephonyTrackRate] for a MediaTek
 * chip (the rate b825ea2 used there); the 48 kHz profiles take 48000 unless the device
 * lists rates without it (then 44100, then the largest listed).
 */
fun pickProfileTrackRate(profile: UplinkProfile, deviceRates: IntArray, inputRate: Int): Int = when {
    !profile.primed -> inputRate
    !profile.rate48k -> pickTelephonyTrackRate(deviceRates, inputRate, true)
    deviceRates.isEmpty() || deviceRates.contains(48000) -> 48000
    deviceRates.contains(44100) -> 44100
    else -> deviceRates.maxOrNull() ?: 48000
}

/**
 * Channels for [profile]: only the last step of the ladder is stereo, so every other
 * profile (standard included) stays mono exactly as before, whatever the device lists.
 */
fun pickProfileChannels(profile: UplinkProfile): Int = if (profile.stereo) 2 else 1

fun planTrack(profile: UplinkProfile, caps: TelephonyCaps, inputRate: Int): UplinkTrackPlan =
    UplinkTrackPlan(
        trackRate = pickProfileTrackRate(profile, caps.sampleRates.toIntArray(), inputRate),
        channels = pickProfileChannels(profile),
        primed = profile.primed,
    )

/** Bytes of [ms] of s16 PCM at [rate] with [channels]. */
fun pcmBytesFor(ms: Long, rate: Int, channels: Int): Int = (rate.toLong() * ms / 1000L).toInt() * 2 * channels

/** Milliseconds of [bytes] of s16 PCM at [rate] with [channels]. */
fun pcmMsFor(bytes: Int, rate: Int, channels: Int): Long = bytes.toLong() * 1000L / (rate.toLong() * 2L * channels)

/** Primed profiles keep about this much audio in the track. */
const val PRIMED_BUFFER_MS = 160L

/**
 * Track buffer size in bytes. Standard is the original `minBuf * 6` (at least 200 ms);
 * primed profiles use about [PRIMED_BUFFER_MS] so the whole buffer can be filled at once.
 */
fun profileBufferBytes(plan: UplinkTrackPlan, minBuf: Int): Int =
    if (plan.primed) {
        minBuf.coerceAtLeast(pcmBytesFor(PRIMED_BUFFER_MS, plan.trackRate, plan.channels))
    } else {
        (minBuf * 6).coerceAtLeast(plan.trackRate / 5 * 2 * plan.channels)
    }

/** Writes each s16le sample of [mono] twice (left, right) for a two-channel track. */
fun duplicateToTwoChannels(mono: ByteArray): ByteArray {
    val frames = mono.size / 2
    val twoChannel = ByteArray(frames * 4)
    for (f in 0 until frames) {
        val low = mono[2 * f]
        val high = mono[2 * f + 1]
        twoChannel[4 * f] = low
        twoChannel[4 * f + 1] = high
        twoChannel[4 * f + 2] = low
        twoChannel[4 * f + 3] = high
    }
    return twoChannel
}

/**
 * This phone's first profile when nothing is stored: MediaTek primed (b825ea2), Google
 * standard, other chips standard unless the telephony output lists rates without
 * [inputRate] (then primed, which converts the rate).
 *
 * Capabilities first, then per-chip choices: approach informed by Oboe QuirksManager
 * (Apache-2.0), https://github.com/google/oboe.
 */
fun defaultStartProfile(chip: ChipKind, caps: TelephonyCaps, inputRate: Int): Pair<UplinkProfile, StartSource> =
    when {
        chip == ChipKind.MEDIATEK -> UplinkProfile.PRIMED to StartSource.CHIP
        chip == ChipKind.GOOGLE -> UplinkProfile.STANDARD to StartSource.DEFAULT
        caps.sampleRates.isNotEmpty() && !caps.sampleRates.contains(inputRate) ->
            UplinkProfile.PRIMED to StartSource.CAPS
        else -> UplinkProfile.STANDARD to StartSource.DEFAULT
    }

/**
 * First profile of a call.
 *
 * With [measureOnly] (Google phones) the phone keeps what b825ea2 did and ignores
 * everything else, [forced] included: those phones must sound exactly as before. Then
 * [forced] (testing), then the profile after [avoid] (the owner said the caller heard
 * nothing with it), then the [remembered] one, then [defaultStartProfile]. [samePlan]
 * tells two profiles apart on this phone, so a profile that would build the same track
 * as [avoid] is skipped.
 */
fun pickStartProfile(
    forced: UplinkProfile?,
    avoid: UplinkProfile?,
    remembered: UplinkProfile?,
    chip: ChipKind,
    caps: TelephonyCaps,
    inputRate: Int,
    measureOnly: Boolean,
    samePlan: (UplinkProfile, UplinkProfile) -> Boolean = { a, b -> a == b },
): Pair<UplinkProfile, StartSource> {
    val fallback = defaultStartProfile(chip, caps, inputRate)
    if (measureOnly) {
        return if (chip == ChipKind.MEDIATEK) fallback else UplinkProfile.STANDARD to StartSource.DEFAULT
    }
    if (forced != null) return forced to StartSource.FORCED
    if (avoid != null) {
        val next = ladderAfter(avoid).firstOrNull { !samePlan(it, avoid) }
        if (next != null) return next to StartSource.OWNER_REPORT
    }
    if (remembered != null) return remembered to StartSource.REMEMBERED
    return fallback
}

/** The other profiles, in ladder order starting after [p] and wrapping around. */
fun ladderAfter(p: UplinkProfile): List<UplinkProfile> {
    val all = UplinkProfile.entries
    return (1 until all.size).map { all[(p.ordinal + it) % all.size] }
}

/**
 * How far the playback head moved between two readings of
 * `AudioTrack.getPlaybackHeadPosition()`, which is an unsigned 32-bit frame count in an
 * Int and wraps. A step back (only flush/stop/reload reset it, and this code never calls
 * those while measuring) reads as a huge forward step after the wrap and counts as 0.
 *
 * Approach informed by AndroidX Media3 AudioTrackPositionTracker (Apache-2.0),
 * https://github.com/androidx/media.
 */
fun headAdvanceFrames(prev: Int, cur: Int): Long {
    val d = (cur.toLong() - prev.toLong()) and 0xFFFFFFFFL
    return if (d >= 0x80000000L) 0L else d
}

/**
 * Measures how much audio one profile's telephony tracks threw away. No Android classes,
 * the caller passes clock readings and head positions.
 *
 * When the audio server kills a track, the client's restoreTrack_l() builds a new one
 * and sets the playback head to every frame the app had written ("the frames that will
 * be lost by track recreation", AOSP frameworks/av libaudioclient AudioTrack.cpp,
 * Apache-2.0, Android 12 to 16; only the behavior is relied on). So the head
 * jumps ahead of real time by the audio that never played; no write error is raised.
 * A healthy track's head cannot move faster than real time except in mixer-period steps.
 *
 * Each [sample] allows the head to move by the real time since the last one plus unused
 * credit from earlier samples; the credit carried over is capped at [CREDIT_CAP_MS], so a
 * stalled head cannot bank time. Head movement beyond that is lost audio. Underruns, long
 * silence and cleared queues only slow the head down and never count. Gaps over
 * [MAX_SAMPLE_GAP_MS] (the sampler did not run) are skipped.
 */
class UplinkHealthMeter {
    private var rate = 0
    private var lastHead = 0
    private var lastAt = 0L
    private var creditFrames = 0L
    private var tracking = false
    private var audioFrames = 0L
    /** Loss events inside the judging window: time, lost ms, 1 when it was an app rebuild. */
    private val recent = ArrayDeque<LongArray>()

    var lostMs = 0L
        private set
    var lossEvents = 0
        private set
    var rebuilds = 0
        private set
    var activeMs = 0L
        private set

    val trackRate: Int get() = rate

    /** Call audio (not keepalive zeros) written while this profile was in use. */
    val audioMs: Long get() = if (rate > 0) audioFrames * 1000L / rate else 0L

    fun beginTrack(nowMs: Long, rate: Int, head: Int) {
        this.rate = rate
        lastHead = head
        lastAt = nowMs
        // Right after start some outputs pull a few periods at once.
        creditFrames = framesFor(CREDIT_CAP_MS)
        tracking = true
    }

    fun endTrack() {
        tracking = false
    }

    fun addAudioFrames(frames: Long) {
        if (frames > 0) audioFrames += frames
    }

    /** @return the audio lost since the last sample, in ms. */
    fun sample(nowMs: Long, head: Int): Long {
        if (!tracking || rate <= 0) return 0L
        val dt = nowMs - lastAt
        val advance = headAdvanceFrames(lastHead, head)
        lastHead = head
        lastAt = nowMs
        if (dt < 0) return 0L
        activeMs += dt
        val cap = framesFor(CREDIT_CAP_MS)
        if (dt > MAX_SAMPLE_GAP_MS) {
            creditFrames = cap
            return 0L
        }
        val available = creditFrames + rate.toLong() * dt / 1000L
        if (advance <= available) {
            creditFrames = (available - advance).coerceAtMost(cap)
            return 0L
        }
        val lostFrames = advance - available
        creditFrames = 0L
        val ms = lostFrames * 1000L / rate
        lostMs += ms
        if (ms >= MIN_EVENT_MS) {
            lossEvents++
            record(nowMs, ms, 0L)
        }
        return ms
    }

    /** The app rebuilt the track after a write error; [pendingMs] were still in it. */
    fun noteRebuild(nowMs: Long, pendingMs: Long) {
        val ms = pendingMs.coerceAtLeast(0L)
        rebuilds++
        lostMs += ms
        lossEvents++
        record(nowMs, ms, 1L)
    }

    private fun record(nowMs: Long, ms: Long, rebuild: Long) {
        recent.addLast(longArrayOf(nowMs, ms, rebuild))
        prune(nowMs)
    }

    private fun prune(nowMs: Long) {
        while (recent.isNotEmpty() && nowMs - recent.first()[0] > WINDOW_MS) recent.removeFirst()
    }

    /**
     * Unhealthy: inside the last [WINDOW_MS], at least [MIN_EVENTS] loss events that
     * together lost at least [MIN_WINDOW_LOST_MS], or [MIN_REBUILDS] app rebuilds. One
     * route change at call start (one event) never qualifies.
     */
    fun isUnhealthy(nowMs: Long): Boolean {
        prune(nowMs)
        var lost = 0L
        var rebuildCount = 0
        for (e in recent) {
            lost += e[1]
            if (e[2] == 1L) rebuildCount++
        }
        return (recent.size >= MIN_EVENTS && lost >= MIN_WINDOW_LOST_MS) || rebuildCount >= MIN_REBUILDS
    }

    /** Share of the time in use that was lost (for picking the least bad profile). */
    fun lossRate(): Double = if (activeMs > 0) lostMs.toDouble() / activeMs else Double.MAX_VALUE

    private fun framesFor(ms: Long): Long = rate.toLong() * ms / 1000L

    companion object {
        const val CREDIT_CAP_MS = 100L
        const val MAX_SAMPLE_GAP_MS = 300L
        const val MIN_EVENT_MS = 10L
        const val WINDOW_MS = 5_000L
        const val MIN_EVENTS = 3
        const val MIN_WINDOW_LOST_MS = 300L
        const val MIN_REBUILDS = 3
    }
}

/** What to store after a call. [fit] null = leave the stored state as it is. */
data class TuneDecision(
    val remember: UplinkProfile? = null,
    val forget: Boolean = false,
    val fit: AudioFit? = null,
    val reason: String? = null,
)

/** A call counts as measured once this much of the AI's voice went through one profile. */
const val MIN_MEASURED_AUDIO_MS = 10_000L

/**
 * The outcome of a call whose track went live.
 *
 * Measure-only phones never store a profile: unhealthy → problem, a long healthy call → ok,
 * except that a problem the owner reported stays ([keepOwnerProblem]): such a phone has no
 * other settings to try, so overwriting it would just drop the report.
 *
 * With [StartSource.FORCED] (adb testing) only the path under test is stored: the forced
 * profile lost audio, another one took over and stayed healthy through a long call.
 * Anything else leaves the stored state alone.
 *
 * Otherwise an unhealthy final profile (every profile tried) → problem and the memory is
 * dropped. A profile reached only because others could not open a track
 * ([reachedByOpenFailure] while the start never lost audio) is not stored. A final
 * profile that stayed healthy through [MIN_MEASURED_AUDIO_MS] of voice is remembered:
 * adjusted when this call switched away from a profile that lost audio, when it is not
 * this phone's default first profile ([finalIsDefault]), or after an owner report; ok
 * otherwise. A short call changes
 * nothing, except that a remembered profile which turned unhealthy is forgotten.
 */
fun decideTune(
    autoSwitch: Boolean,
    source: StartSource,
    finalProfile: UplinkProfile,
    switchedForLoss: Boolean,
    startUnhealthy: Boolean,
    finalUnhealthy: Boolean,
    finalAudioMs: Long,
    finalIsDefault: Boolean,
    reachedByOpenFailure: Boolean = false,
    keepOwnerProblem: Boolean = false,
): TuneDecision {
    val long = finalAudioMs >= MIN_MEASURED_AUDIO_MS
    if (!autoSwitch) {
        return when {
            keepOwnerProblem -> TuneDecision()
            finalUnhealthy || startUnhealthy -> TuneDecision(fit = AudioFit.PROBLEM, reason = "audio_lost")
            long -> TuneDecision(fit = AudioFit.OK)
            else -> TuneDecision()
        }
    }
    if (source == StartSource.FORCED) {
        return if (startUnhealthy && switchedForLoss && !finalUnhealthy && long) {
            TuneDecision(remember = finalProfile, fit = AudioFit.ADJUSTED)
        } else {
            TuneDecision()
        }
    }
    if (finalUnhealthy) return TuneDecision(forget = true, fit = AudioFit.PROBLEM, reason = "audio_lost")
    if (long) {
        // Got here only because other profiles would not open: keep what is stored.
        if (reachedByOpenFailure && !startUnhealthy) return TuneDecision()
        val adjusted = switchedForLoss || !finalIsDefault || source == StartSource.OWNER_REPORT
        return TuneDecision(remember = finalProfile, fit = if (adjusted) AudioFit.ADJUSTED else AudioFit.OK)
    }
    if (startUnhealthy && source == StartSource.REMEMBERED) {
        return TuneDecision(forget = true, fit = AudioFit.UNTESTED)
    }
    return TuneDecision()
}

/**
 * One call's walk up the profile ladder. Not thread-safe: the injector guards it.
 *
 * [autoSwitch] false = measure only. [defaultStart] is this phone's first profile when
 * nothing is stored ([defaultStartProfile]). [keepOwnerProblem]: see [decideTune].
 * [samePlan] says when two profiles would build the same track on this phone; those are
 * tried once.
 */
class UplinkTuner(
    val start: UplinkProfile,
    val source: StartSource,
    val autoSwitch: Boolean,
    private val defaultStart: UplinkProfile = start,
    private val keepOwnerProblem: Boolean = false,
    private val samePlan: (UplinkProfile, UplinkProfile) -> Boolean = { a, b -> a == b },
) {
    var current: UplinkProfile = start
        private set
    var switches = 0
        private set
    /** Switches made because the profile in use lost audio (not because it would not open). */
    var lossSwitches = 0
        private set
    /** A track of this call reached the telephony route. */
    var everLive = false
        private set

    private val tried = LinkedHashSet<UplinkProfile>().apply { add(start) }
    private val meters = LinkedHashMap<UplinkProfile, UplinkHealthMeter>()
    private val unhealthy = HashSet<UplinkProfile>()
    private val openFailed = LinkedHashMap<UplinkProfile, String>()
    private var settled = false
    /** Every profile failed to open once; the tuner went back to [start] and stays there. */
    private var openExhausted = false
    private var noDevice = false

    fun meter(): UplinkHealthMeter = meters.getOrPut(current) { UplinkHealthMeter() }

    fun meterOf(p: UplinkProfile): UplinkHealthMeter? = meters[p]

    fun isUnhealthy(p: UplinkProfile): Boolean = p in unhealthy

    fun markLive() {
        everLive = true
    }

    /** No telephony output appeared for long enough during the call. */
    fun markNoDevice() {
        noDevice = true
    }

    /**
     * The current profile lost audio. @return the profile to rebuild with, or null to stay
     * (measure only, or every profile was tried and this one lost the least).
     */
    fun onUnhealthy(): UplinkProfile? {
        if (!unhealthy.add(current)) return null
        if (!autoSwitch || settled) return null
        nextUntried()?.let { return switchTo(it, forLoss = true) }
        settled = true
        val best = meters.entries
            .filter { it.key !in openFailed && it.value.activeMs > 0 }
            .minByOrNull { it.value.lossRate() }
            ?.key
        return if (best != null && best != current) switchTo(best, forLoss = true) else null
    }

    /**
     * The current profile could not open a track on the telephony route for long enough
     * ([reason]: prefer_rejected or track_rejected). @return the next profile; once every
     * profile failed, [start] again (the caller keeps retrying it); null to stay.
     */
    fun onOpenFailed(reason: String): UplinkProfile? {
        if (current in openFailed) return null
        openFailed[current] = reason
        if (!autoSwitch) return null
        if (!openExhausted) nextUntried()?.let { return switchTo(it, forLoss = false) }
        openExhausted = true
        return if (current != start) switchTo(start, forLoss = false) else null
    }

    private fun nextUntried(): UplinkProfile? = ladderAfter(current).firstOrNull { p ->
        p !in tried && tried.none { samePlan(it, p) }
    }

    private fun switchTo(p: UplinkProfile, forLoss: Boolean): UplinkProfile {
        current = p
        tried.add(p)
        switches++
        if (forLoss) lossSwitches++
        return p
    }

    /** Lost audio, loss events and app rebuilds over every profile of the call. */
    fun totals(): Triple<Long, Int, Int> {
        var lost = 0L
        var events = 0
        var rebuilds = 0
        for (m in meters.values) {
            lost += m.lostMs
            events += m.lossEvents
            rebuilds += m.rebuilds
        }
        return Triple(lost, events, rebuilds)
    }

    /**
     * What to store once the call is over. A call whose track never went live is
     * unsupported only with a cause (no telephony output, or every profile refused);
     * that keeps a remembered profile, since it may still be right once the route opens.
     */
    fun decision(): TuneDecision {
        if (!everLive) {
            val reason = when {
                noDevice -> "no_telephony_device"
                openFailed.isNotEmpty() && (openExhausted || !autoSwitch) ->
                    if (openFailed.values.any { it == "prefer_rejected" }) "prefer_rejected" else "track_rejected"
                else -> null
            }
            return if (reason == null || source == StartSource.FORCED || (!autoSwitch && keepOwnerProblem)) {
                TuneDecision()
            } else {
                TuneDecision(fit = AudioFit.UNSUPPORTED, reason = reason)
            }
        }
        return decideTune(
            autoSwitch = autoSwitch,
            source = source,
            finalProfile = current,
            switchedForLoss = lossSwitches > 0,
            startUnhealthy = start in unhealthy,
            finalUnhealthy = current in unhealthy,
            finalAudioMs = meters[current]?.audioMs ?: 0L,
            finalIsDefault = samePlan(current, defaultStart),
            reachedByOpenFailure = current != start,
            keepOwnerProblem = keepOwnerProblem,
        )
    }
}

/** Measurement summary of one call, sent to Bageo as `uplink_health`. */
data class UplinkHealthReport(
    val startProfile: UplinkProfile,
    val finalProfile: UplinkProfile,
    val source: StartSource,
    val trackRate: Int,
    val channels: Int,
    val bufferMs: Long,
    val lostMs: Long,
    val lossEvents: Int,
    val rebuilds: Int,
    val writeErrors: Int,
    val underruns: Long,
    val switches: Int,
    val measureOnly: Boolean,
    val audioMs: Long,
    val live: Boolean,
    val fit: AudioFit,
    val reason: String?,
    val caps: TelephonyCaps,
)

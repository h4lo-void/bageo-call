// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

package com.pixelcallai.streamer

/**
 * Standalone: when a ringing call goes back to the owner, and how the phone alerts the
 * owner then. Plain logic without Android, so it is tested on a computer; [OwnerRing]
 * and [SilentInCallService] do the work.
 */
internal object RingHandoffRules {
    /** AI mode: the call service has this long to take a ringing call before the owner gets it. */
    const val CLAIM_TIMEOUT_MS = 2_000L

    /** The service took the call: look again this often, in case it stops before answering. */
    const val RECHECK_MS = 2_000L

    enum class Next {
        /** Look again later. */
        WAIT,
        /** Ring the owner now. */
        HAND_TO_OWNER,
        /** Nothing more to do: the call stopped ringing, or the owner already has it. */
        DONE,
    }

    /**
     * One look at a ringing call, [waitedMs] after it started ringing. A normal phone never
     * waits (Bageo does not answer there). In AI mode the call waits while the call service
     * has it ([serviceTookIt]); the service hands it over itself when the AI cannot answer.
     * When the service has not taken it within [CLAIM_TIMEOUT_MS] (not running, or it did
     * not see the ring), the owner gets it.
     */
    fun next(
        stillRinging: Boolean,
        aiMode: Boolean,
        serviceTookIt: Boolean,
        ownerHasIt: Boolean,
        waitedMs: Long,
    ): Next = when {
        !stillRinging || ownerHasIt -> Next.DONE
        !aiMode -> Next.HAND_TO_OWNER
        serviceTookIt -> Next.WAIT
        waitedMs < CLAIM_TIMEOUT_MS -> Next.WAIT
        else -> Next.HAND_TO_OWNER
    }

    /** How long until the next look after a [Next.WAIT]. */
    fun nextLookInMs(waitedMs: Long): Long =
        if (waitedMs < CLAIM_TIMEOUT_MS) CLAIM_TIMEOUT_MS - waitedMs else RECHECK_MS

    enum class Ringer { NORMAL, VIBRATE, SILENT }

    class Alert(val sound: Boolean, val vibrate: Boolean)

    /**
     * The phone's own settings decide: Do Not Disturb that does not let this call through
     * means no sound and no vibration; a silent ringer the same; a vibrating ringer only
     * vibrates; a normal ringer rings and vibrates when "vibrate for calls" is on.
     */
    fun alertFor(ringer: Ringer, dndAllowsCall: Boolean, vibrateWhenRinging: Boolean): Alert = when {
        !dndAllowsCall -> Alert(sound = false, vibrate = false)
        ringer == Ringer.SILENT -> Alert(sound = false, vibrate = false)
        ringer == Ringer.VIBRATE -> Alert(sound = false, vibrate = true)
        else -> Alert(sound = true, vibrate = vibrateWhenRinging)
    }
}

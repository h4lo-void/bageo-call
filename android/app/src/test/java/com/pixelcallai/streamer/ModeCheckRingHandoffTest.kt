// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

package com.pixelcallai.streamer

import com.pixelcallai.streamer.RingHandoffRules.Next
import com.pixelcallai.streamer.RingHandoffRules.Ringer
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Standalone: the app's mode follows the phone (module mode and phone app role), and a
 * ringing call nobody takes goes to the owner in time, ringing as the phone allows.
 */
class ModeCheckRingHandoffTest {
    private val ai = Prefs.PHONE_MODE_AI
    private val normal = Prefs.PHONE_MODE_NORMAL

    @Test
    fun module_made_ai_by_hand_and_bageo_is_the_phone_app_makes_the_app_ai() {
        // The case seen on a Pixel 9: apply_mode.sh ai run as root, the app still said normal.
        assertEquals(ModeFix.ADOPT_AI, decideModeFix(normal, ai, bageoIsPhoneApp = true, busy = false))
        // Role unknown (Android did not say): the module's AI still wins.
        assertEquals(ModeFix.ADOPT_AI, decideModeFix(normal, ai, bageoIsPhoneApp = null, busy = false))
    }

    @Test
    fun module_ai_while_another_app_is_the_phone_app_keeps_the_normal_phone() {
        // Calls still ring through the other phone app: the app must not start answering.
        assertEquals(ModeFix.KEEP, decideModeFix(normal, ai, bageoIsPhoneApp = false, busy = false))
    }

    @Test
    fun module_normal_always_makes_the_app_normal() {
        assertEquals(ModeFix.ADOPT_NORMAL, decideModeFix(ai, normal, bageoIsPhoneApp = true, busy = false))
        assertEquals(ModeFix.ADOPT_NORMAL, decideModeFix(ai, normal, bageoIsPhoneApp = false, busy = false))
        assertEquals(ModeFix.ADOPT_NORMAL, decideModeFix(ai, normal, bageoIsPhoneApp = null, busy = false))
    }

    @Test
    fun nothing_changes_when_they_agree_or_the_module_is_unknown_or_busy() {
        assertEquals(ModeFix.KEEP, decideModeFix(ai, ai, bageoIsPhoneApp = true, busy = false))
        assertEquals(ModeFix.KEEP, decideModeFix(normal, normal, bageoIsPhoneApp = true, busy = false))
        // No state settled since this boot (helper not started yet, or a switch unfinished).
        assertEquals(ModeFix.KEEP, decideModeFix(normal, null, bageoIsPhoneApp = true, busy = false))
        // A switch runs, or a call already rings for the owner.
        assertEquals(ModeFix.KEEP, decideModeFix(normal, ai, bageoIsPhoneApp = true, busy = true))
        assertEquals(ModeFix.KEEP, decideModeFix(ai, normal, bageoIsPhoneApp = true, busy = true))
    }

    @Test
    fun a_normal_phone_rings_for_the_owner_at_once() {
        assertEquals(Next.HAND_TO_OWNER, next(aiMode = false, waitedMs = 0L))
        assertEquals(Next.HAND_TO_OWNER, next(aiMode = false, serviceTookIt = true, waitedMs = 0L))
    }

    @Test
    fun ai_mode_gives_the_call_service_two_seconds_to_take_the_call() {
        assertEquals(Next.WAIT, next(aiMode = true, waitedMs = 0L))
        assertEquals(Next.WAIT, next(aiMode = true, waitedMs = RingHandoffRules.CLAIM_TIMEOUT_MS - 1))
        assertEquals(Next.HAND_TO_OWNER, next(aiMode = true, waitedMs = RingHandoffRules.CLAIM_TIMEOUT_MS))
        assertTrue(RingHandoffRules.CLAIM_TIMEOUT_MS in 1_500L..2_000L)
    }

    @Test
    fun a_call_the_service_took_keeps_waiting_and_goes_to_the_owner_once_it_lets_go() {
        // Waiting for the agent: the service hands the call over itself when it gives up.
        assertEquals(Next.WAIT, next(aiMode = true, serviceTookIt = true, waitedMs = 10_000L))
        // The service stopped (switched, crashed) with the call still ringing.
        assertEquals(Next.HAND_TO_OWNER, next(aiMode = true, serviceTookIt = false, waitedMs = 10_000L))
    }

    @Test
    fun nothing_more_once_the_ring_ended_or_the_owner_has_the_call() {
        assertEquals(Next.DONE, next(stillRinging = false, aiMode = true, waitedMs = 5_000L))
        assertEquals(Next.DONE, next(stillRinging = false, aiMode = false, waitedMs = 0L))
        // Rung once already (no second ring), or a whitelist call ringing for the owner.
        assertEquals(Next.DONE, next(aiMode = true, ownerHasIt = true, waitedMs = 5_000L))
        assertEquals(Next.DONE, next(aiMode = false, ownerHasIt = true, waitedMs = 0L))
    }

    @Test
    fun the_next_look_comes_at_the_time_limit_then_every_recheck() {
        assertEquals(RingHandoffRules.CLAIM_TIMEOUT_MS, RingHandoffRules.nextLookInMs(0L))
        assertEquals(RingHandoffRules.CLAIM_TIMEOUT_MS - 500L, RingHandoffRules.nextLookInMs(500L))
        assertEquals(RingHandoffRules.RECHECK_MS, RingHandoffRules.nextLookInMs(RingHandoffRules.CLAIM_TIMEOUT_MS))
        assertEquals(RingHandoffRules.RECHECK_MS, RingHandoffRules.nextLookInMs(9_000L))
    }

    @Test
    fun the_owner_ring_follows_the_ringer_and_do_not_disturb() {
        val ring = RingHandoffRules.alertFor(Ringer.NORMAL, dndAllowsCall = true, vibrateWhenRinging = true)
        assertTrue(ring.sound)
        assertTrue(ring.vibrate)
        val ringOnly = RingHandoffRules.alertFor(Ringer.NORMAL, dndAllowsCall = true, vibrateWhenRinging = false)
        assertTrue(ringOnly.sound)
        assertFalse(ringOnly.vibrate)
        val buzz = RingHandoffRules.alertFor(Ringer.VIBRATE, dndAllowsCall = true, vibrateWhenRinging = false)
        assertFalse(buzz.sound)
        assertTrue(buzz.vibrate)
        val silent = RingHandoffRules.alertFor(Ringer.SILENT, dndAllowsCall = true, vibrateWhenRinging = true)
        assertFalse(silent.sound)
        assertFalse(silent.vibrate)
        for (ringer in Ringer.values()) {
            val dnd = RingHandoffRules.alertFor(ringer, dndAllowsCall = false, vibrateWhenRinging = true)
            assertFalse(dnd.sound)
            assertFalse(dnd.vibrate)
        }
    }

    private fun next(
        stillRinging: Boolean = true,
        aiMode: Boolean,
        serviceTookIt: Boolean = false,
        ownerHasIt: Boolean = false,
        waitedMs: Long,
    ): Next = RingHandoffRules.next(stillRinging, aiMode, serviceTookIt, ownerHasIt, waitedMs)
}

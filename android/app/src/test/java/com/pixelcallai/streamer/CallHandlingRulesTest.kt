// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

package com.pixelcallai.streamer

import android.telephony.TelephonyManager
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** Ring handling and the normal-phone volumes: rules that decide what the phone does. */
class CallHandlingRulesTest {
    private val ringing = TelephonyManager.CALL_STATE_RINGING
    private val offhook = TelephonyManager.CALL_STATE_OFFHOOK
    private val idle = TelephonyManager.CALL_STATE_IDLE
    private val noSignal = Long.MAX_VALUE

    @Test
    fun idle_from_telephony_does_not_cancel_a_call_telecom_still_rings() {
        // The service was just woken by the ring: TelephonyManager says IDLE, Telecom rings.
        assertTrue(CallAudioService.shouldHoldIdleForRing(false, true, ringing, true, noSignal))
        // Same while the call is up (VoWiFi lag), even with the AI session running.
        assertTrue(CallAudioService.shouldHoldIdleForRing(true, false, offhook, true, noSignal))
    }

    @Test
    fun idle_right_after_the_ring_signal_waits_for_telecom_to_add_the_call() {
        // InCallService bound (state IDLE, no call yet) or not bound (null): both wait.
        assertTrue(CallAudioService.shouldHoldIdleForRing(false, true, idle, false, 300L))
        assertTrue(CallAudioService.shouldHoldIdleForRing(false, true, null, false, 300L))
        // The wait is short: after it, IDLE is believed.
        assertFalse(
            CallAudioService.shouldHoldIdleForRing(false, true, idle, false, CallAudioService.RING_SIGNAL_GRACE_MS),
        )
    }

    @Test
    fun a_real_idle_is_not_held() {
        // Telecom removed the call (the ring signal is cleared then).
        assertFalse(CallAudioService.shouldHoldIdleForRing(false, true, idle, false, noSignal))
        // Nothing scheduled: an ordinary IDLE.
        assertFalse(CallAudioService.shouldHoldIdleForRing(false, false, idle, false, 300L))
        // An AI call ended and Telecom agrees.
        assertFalse(CallAudioService.shouldHoldIdleForRing(true, false, idle, false, 300L))
        // Telecom lists a call that neither rings nor is up (an outgoing one not dialing yet).
        assertFalse(CallAudioService.shouldHoldIdleForRing(false, true, idle, true, 300L))
    }

    @Test
    fun the_earpiece_bageo_turned_down_is_never_saved_as_the_normal_volume() {
        // Earpiece at its lowest step (1) during AI mode: keep the 8 saved before.
        assertEquals(8, chooseSavedVolume(current = 1, previous = 8, min = 1))
        // A real change by the owner is saved.
        assertEquals(10, chooseSavedVolume(current = 10, previous = 8, min = 1))
        // Nothing saved before: the current value is all there is.
        assertEquals(1, chooseSavedVolume(current = 1, previous = null, min = 1))
        assertEquals(8, chooseSavedVolume(current = null, previous = 8, min = 1))
        assertNull(chooseSavedVolume(current = null, previous = null, min = 0))
    }

    @Test
    fun a_ring_or_notification_volume_the_owner_set_to_its_lowest_is_saved_as_it_is() {
        // Only the earpiece keeps the older value; the owner's own low ring volume counts.
        assertEquals(1, chooseSavedVolume(current = 1, previous = 5, min = 1, keepPreviousAtMin = false))
        assertEquals(0, chooseSavedVolume(current = 0, previous = 5, min = 0, keepPreviousAtMin = false))
    }

    @Test
    fun the_owner_taking_a_call_hears_their_own_earpiece_volume() {
        // The volume from before the call wins over the saved normal-phone one.
        assertEquals(6, ownCallEarpieceVolume(beforeCall = 6, savedNormal = 9, min = 1, max = 15))
        // Bageo kept the earpiece at its lowest before the call: the normal-phone value.
        assertEquals(9, ownCallEarpieceVolume(beforeCall = 1, savedNormal = 9, min = 1, max = 15))
        // Nothing known: half of the maximum, never the lowest step.
        assertEquals(8, ownCallEarpieceVolume(beforeCall = null, savedNormal = 1, min = 1, max = 15))
    }

    @Test
    fun normal_phone_volumes_use_the_owners_own_values() {
        // Daily Sound: the owner's current ring volume wins over an old saved one.
        assertEquals(6, pickNormalVolume(current = 6, saved = 4, min = 0, max = 7, trustCurrent = true))
        // Silent AI mode (volumes held at zero by Bageo): the saved value.
        assertEquals(4, pickNormalVolume(current = 0, saved = 4, min = 0, max = 7, trustCurrent = false))
        // Earpiece left at its lowest step: never taken as the owner's volume.
        assertEquals(8, pickNormalVolume(current = 1, saved = 8, min = 1, max = 15, trustCurrent = true))
        // Nothing usable: half of the maximum, never zero.
        assertEquals(8, pickNormalVolume(current = 0, saved = 0, min = 0, max = 15, trustCurrent = true))
        // A stream the phone lacks is skipped.
        assertEquals(0, pickNormalVolume(current = 3, saved = 3, min = 0, max = 0, trustCurrent = true))
    }

    @Test
    fun daily_sound_requests_are_told_apart_like_the_script_does() {
        assertTrue(isNoDndId(formatModeRequestId(noDnd = true, suffix = "x1")))
        assertTrue(isNoDndId("nodnd-agent-7"))
        assertFalse(isNoDndId(formatModeRequestId(noDnd = false, suffix = "x2")))
        assertFalse(isNoDndId("agent-nodnd-7"))
    }
}

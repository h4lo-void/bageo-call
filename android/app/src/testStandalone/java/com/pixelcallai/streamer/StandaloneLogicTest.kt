// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

package com.pixelcallai.streamer

import android.app.NotificationManager
import android.telephony.TelephonyManager
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class StandaloneLogicTest {
    @Test
    fun dedicatedDndDecideModeTarget() {
        assertNull(
            DedicatedDndManager.decideModeTarget(
                autoEnabled = false,
                anyDndAllowed = false,
                interruptionFilter = NotificationManager.INTERRUPTION_FILTER_PRIORITY,
                ruleMatchesOurs = true,
                currentMode = Prefs.PHONE_MODE_NORMAL,
            ),
        )
        // Turning dedicated rule ON switches Normal -> AI
        assertEquals(
            Prefs.PHONE_MODE_AI,
            DedicatedDndManager.decideModeTarget(
                autoEnabled = true,
                anyDndAllowed = false,
                interruptionFilter = NotificationManager.INTERRUPTION_FILTER_PRIORITY,
                ruleMatchesOurs = true,
                currentMode = Prefs.PHONE_MODE_NORMAL,
            ),
        )
        // Already AI -> no duplicate switch
        assertNull(
            DedicatedDndManager.decideModeTarget(
                autoEnabled = true,
                anyDndAllowed = false,
                interruptionFilter = NotificationManager.INTERRUPTION_FILTER_PRIORITY,
                ruleMatchesOurs = true,
                currentMode = Prefs.PHONE_MODE_AI,
            ),
        )
        // Turning DND off switches AI -> Normal
        assertEquals(
            Prefs.PHONE_MODE_NORMAL,
            DedicatedDndManager.decideModeTarget(
                autoEnabled = true,
                anyDndAllowed = false,
                interruptionFilter = NotificationManager.INTERRUPTION_FILTER_ALL,
                ruleMatchesOurs = false,
                currentMode = Prefs.PHONE_MODE_AI,
            ),
        )
        // Foreign DND does not trigger AI unless anyDndAllowed=true
        assertEquals(
            Prefs.PHONE_MODE_NORMAL,
            DedicatedDndManager.decideModeTarget(
                autoEnabled = true,
                anyDndAllowed = false,
                interruptionFilter = NotificationManager.INTERRUPTION_FILTER_PRIORITY,
                ruleMatchesOurs = false,
                currentMode = Prefs.PHONE_MODE_AI,
            ),
        )
        assertEquals(
            Prefs.PHONE_MODE_AI,
            DedicatedDndManager.decideModeTarget(
                autoEnabled = true,
                anyDndAllowed = true,
                interruptionFilter = NotificationManager.INTERRUPTION_FILTER_PRIORITY,
                ruleMatchesOurs = false,
                currentMode = Prefs.PHONE_MODE_NORMAL,
            ),
        )
        // Manual override to NORMAL while DND stays ON (previousDndActive == true -> true) must NOT revert to AI!
        assertNull(
            DedicatedDndManager.decideModeTarget(
                autoEnabled = true,
                anyDndAllowed = true,
                interruptionFilter = NotificationManager.INTERRUPTION_FILTER_PRIORITY,
                ruleMatchesOurs = true,
                currentMode = Prefs.PHONE_MODE_NORMAL,
                previousDndActive = true,
            ),
        )
        // Genuine edge OFF -> ON triggers AI
        assertEquals(
            Prefs.PHONE_MODE_AI,
            DedicatedDndManager.decideModeTarget(
                autoEnabled = true,
                anyDndAllowed = true,
                interruptionFilter = NotificationManager.INTERRUPTION_FILTER_PRIORITY,
                ruleMatchesOurs = false,
                currentMode = Prefs.PHONE_MODE_NORMAL,
                previousDndActive = false,
            ),
        )
        // AI turned on by hand stays on when Do Not Disturb goes on and off again
        assertNull(
            DedicatedDndManager.decideModeTarget(
                autoEnabled = true,
                anyDndAllowed = true,
                interruptionFilter = NotificationManager.INTERRUPTION_FILTER_ALL,
                ruleMatchesOurs = false,
                currentMode = Prefs.PHONE_MODE_AI,
                previousDndActive = true,
                aiCameFromDnd = false,
            ),
        )
        // AI that Do Not Disturb started ends with it
        assertEquals(
            Prefs.PHONE_MODE_NORMAL,
            DedicatedDndManager.decideModeTarget(
                autoEnabled = true,
                anyDndAllowed = true,
                interruptionFilter = NotificationManager.INTERRUPTION_FILTER_ALL,
                ruleMatchesOurs = false,
                currentMode = Prefs.PHONE_MODE_AI,
                previousDndActive = true,
                aiCameFromDnd = true,
            ),
        )
    }

    @Test
    fun nodndRequestIdFormatting() {
        val noDnd = formatModeRequestId(noDnd = true, suffix = "abc")
        assertEquals("app-nodnd-abc", noDnd)
        assert(noDnd.startsWith("app-"))

        val normal = formatModeRequestId(noDnd = false, suffix = "abc")
        assertEquals("app-abc", normal)
    }

    @Test
    fun normalizeWhitelistActions() {
        assertEquals(Prefs.WHITELIST_ACTION_URGENT_PROMPT, Prefs.normalizeWhitelistAction(""))
        assertEquals(Prefs.WHITELIST_ACTION_RING_PHONE, Prefs.normalizeWhitelistAction("RING_PHONE"))
        assertEquals(Prefs.WHITELIST_ACTION_FORWARD_NUMBER, Prefs.normalizeWhitelistAction("forward_number"))
        assertEquals(Prefs.WHITELIST_ACTION_AI_ONLY, Prefs.normalizeWhitelistAction("ai_only"))
    }

    @Test
    fun resolveEffectiveCallStatePrefersLiveTelecomWhenTelephonyIdle() {
        // VoWiFi / secondary eSIM: TelephonyManager reports IDLE (0), but InCallService sees RINGING (1) or OFFHOOK (2).
        assertEquals(
            TelephonyManager.CALL_STATE_RINGING,
            CallAudioService.resolveEffectiveCallState(
                TelephonyManager.CALL_STATE_IDLE,
                TelephonyManager.CALL_STATE_RINGING,
            ),
        )
        assertEquals(
            TelephonyManager.CALL_STATE_OFFHOOK,
            CallAudioService.resolveEffectiveCallState(
                TelephonyManager.CALL_STATE_IDLE,
                TelephonyManager.CALL_STATE_OFFHOOK,
            ),
        )
        // Cellular call before InCallService binds: TelephonyManager reports RINGING while telecomState is null.
        assertEquals(
            TelephonyManager.CALL_STATE_RINGING,
            CallAudioService.resolveEffectiveCallState(
                TelephonyManager.CALL_STATE_RINGING,
                null,
            ),
        )
        // Both idle: returns IDLE.
        assertEquals(
            TelephonyManager.CALL_STATE_IDLE,
            CallAudioService.resolveEffectiveCallState(
                TelephonyManager.CALL_STATE_IDLE,
                TelephonyManager.CALL_STATE_IDLE,
            ),
        )
        assertNull(CallAudioService.resolveEffectiveCallState(null, null))
    }

    @Test
    fun watchdogNeverReEntersRingingWhileOwnerHandlesWhitelistRing() {
        assertTrue(
            CallAudioService.shouldRecoverRingingInWatchdog(
                answerScheduled = false,
                callActive = false,
                ringGivenUp = false,
                ownerHandlingCall = false,
            ),
        )
        assertFalse(
            CallAudioService.shouldRecoverRingingInWatchdog(
                answerScheduled = false,
                callActive = false,
                ringGivenUp = false,
                ownerHandlingCall = true,
            ),
        )
        assertFalse(
            CallAudioService.shouldRecoverRingingInWatchdog(
                answerScheduled = true,
                callActive = false,
                ringGivenUp = false,
                ownerHandlingCall = false,
            ),
        )
    }
}

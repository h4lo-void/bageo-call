// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

package com.pixelcallai.streamer

import android.app.NotificationManager
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
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
}

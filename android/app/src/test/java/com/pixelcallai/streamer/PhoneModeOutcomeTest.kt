// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

package com.pixelcallai.streamer

import com.pixelcallai.streamer.PhoneModeManager.ApplyLog
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/** The app follows the module's mode file; it never keeps a mode the file does not have. */
class PhoneModeOutcomeTest {
    private val ai = Prefs.PHONE_MODE_AI
    private val normal = Prefs.PHONE_MODE_NORMAL

    @Test
    fun no_result_in_time_keeps_the_mode_the_app_had() {
        // Switching to a normal phone: nothing came back, so the app is still the AI phone.
        val r = resolveOutcome(normal, ai, null, null)
        assertFalse(r.ok)
        assertEquals(ai, r.mode)
        assertEquals(PhoneModeManager.ERR_FAILED, r.error)
    }

    @Test
    fun a_switch_counts_only_when_the_file_says_the_target() {
        val done = ApplyLog("id1", normal, true, "")
        val r = resolveOutcome(normal, ai, done, normal)
        assertTrue(r.ok)
        assertEquals(normal, r.mode)
        assertEquals("", r.error)

        // The script says it worked but the file still has the old mode: not a success.
        val odd = resolveOutcome(normal, ai, done, ai)
        assertFalse(odd.ok)
        assertEquals(ai, odd.mode)
    }

    @Test
    fun failed_switch_to_normal_stays_ai_and_keeps_the_blocking_mode_name() {
        // The script put the AI phone back; the name of the mode that kept Do Not Disturb on
        // is passed on as written, whatever the phone's language.
        for (name in listOf("Schlafen", "睡眠", "Не беспокоить", "おやすみ時間", "Sono")) {
            val log = ApplyLog("id2", normal, true, PhoneModeManager.ERR_DND, name)
            val r = resolveOutcome(normal, ai, log, ai)
            assertFalse(r.ok)
            assertEquals(ai, r.mode)
            assertEquals(PhoneModeManager.ERR_DND, r.error)
            assertEquals(name, r.dndMode)
        }
    }

    @Test
    fun failed_switch_to_ai_ends_as_a_normal_phone() {
        // The script undid its steps and wrote the old mode back: the app follows the file.
        val log = ApplyLog("id3", ai, true, PhoneModeManager.ERR_DND)
        val r = resolveOutcome(ai, normal, log, normal)
        assertFalse(r.ok)
        assertEquals(normal, r.mode)
        assertEquals(PhoneModeManager.ERR_DND, r.error)
    }

    @Test
    fun mode_comes_from_the_file_even_when_the_app_had_another() {
        val log = ApplyLog("id4", ai, true, PhoneModeManager.ERR_DIALER)
        assertEquals(normal, resolveOutcome(ai, ai, log, normal).mode)
        // Unknown file: the app keeps what it had.
        assertEquals(normal, resolveOutcome(ai, normal, log, null).mode)
    }

    @Test
    fun an_unfinished_log_without_an_error_is_a_failure() {
        val r = resolveOutcome(ai, normal, ApplyLog("id5", ai, false, ""), normal)
        assertFalse(r.ok)
        assertEquals(PhoneModeManager.ERR_FAILED, r.error)
    }
}

// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

package com.pixelcallai.streamer

import android.content.Intent
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

/** Normal-mode dial hand-off keeps a tel number and drops everything else. */
class DialHandOffTest {
    @Test
    fun the_hand_off_is_a_dial() {
        assertEquals(Intent.ACTION_DIAL, DIAL_HAND_OFF_ACTION)
        assertEquals("android.intent.action.DIAL", DIAL_HAND_OFF_ACTION)
    }

    @Test
    fun a_tel_link_keeps_the_number_and_drops_separators() {
        assertEquals("0912345678", dialHandOffNumber("tel", "0912-345-678"))
        assertEquals("+15551234567", dialHandOffNumber("TEL", " +1 (555) 123-4567 "))
        assertEquals("*67*0912345678", dialHandOffNumber("tel", "*67*0912345678"))
        assertEquals("*#*#4636#*#*", dialHandOffNumber("tel", "*#*#4636#*#*"))
        assertEquals("5551234,123;9", dialHandOffNumber("tel", "5551234,123;9"))
    }

    @Test
    fun no_tel_data_opens_the_pad_with_no_number() {
        assertNull(dialHandOffNumber(null, null))
        assertNull(dialHandOffNumber("tel", null))
        assertNull(dialHandOffNumber("tel", "   "))
        assertNull(dialHandOffNumber("tel", "---()"))
        // CALL_BUTTON and a bare DIAL have no tel data.
        assertNull(dialHandOffNumber(null, "0912345678"))
        assertNull(dialHandOffNumber("http", "0912345678"))
    }

    @Test
    fun a_number_with_any_other_character_is_dropped() {
        assertNull(dialHandOffNumber("tel", "0912abc5678"))
        assertNull(dialHandOffNumber("tel", "0912/345"))
        assertNull(dialHandOffNumber("tel", "0912?extra=1"))
        assertNull(dialHandOffNumber("tel", "0912#Intent;action=evil"))
        assertNull(dialHandOffNumber("tel", "12+34"))
        assertNull(dialHandOffNumber("tel", "123\n456"))
        assertNull(dialHandOffNumber("tel", "1".repeat(41)))
    }
}

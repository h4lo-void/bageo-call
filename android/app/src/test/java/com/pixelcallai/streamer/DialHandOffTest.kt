// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

package com.pixelcallai.streamer

import android.content.Intent
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

/** Normal-mode dial hand-off keeps a filtered tel number and drops everything else. */
class DialHandOffTest {
    @Test
    fun only_dial_view_and_call_button_are_forwarded() {
        assertEquals("0912345678", dialForwardNumber(Intent.ACTION_DIAL, "tel", "0912345678"))
        assertEquals("0912345678", dialForwardNumber(Intent.ACTION_VIEW, "tel", "0912345678"))
        assertEquals("", dialForwardNumber(Intent.ACTION_CALL_BUTTON, null, null))
        assertNull(dialForwardNumber(Intent.ACTION_CALL, "tel", "0912345678"))
        assertNull(dialForwardNumber("android.intent.action.CALL_PRIVILEGED", "tel", "0912345678"))
        assertNull(dialForwardNumber(null, "tel", "0912345678"))
        assertNull(dialForwardNumber("", "tel", "0912345678"))
    }

    @Test
    fun the_number_keeps_only_digits_and_dial_marks() {
        assertEquals("0912345678", dialableNumber("0912-345-678"))
        assertEquals("+15551234567", dialableNumber(" +1 (555) 123-4567 "))
        assertEquals("*67*0912345678", dialableNumber("*67*0912345678"))
        assertEquals("*#*#4636#*#*", dialableNumber("*#*#4636#*#*"))
        assertEquals("12+34", dialableNumber("12+34"))
        assertEquals("55512341239", dialableNumber("5551234,123;9"))
        assertEquals("09125678", dialableNumber("0912abc5678"))
        assertEquals("0912", dialableNumber("0912?extra=no"))
        assertEquals("0912#", dialableNumber("0912#Intent;action=x"))
        assertEquals("123456", dialableNumber("123\n456"))
        assertEquals("1".repeat(DIALABLE_NUMBER_MAX), dialableNumber("1".repeat(DIALABLE_NUMBER_MAX + 8)))
        assertEquals("", dialableNumber(null))
        assertEquals("", dialableNumber("   "))
        assertEquals("", dialableNumber("---()"))
        assertEquals("0912345678", dialForwardNumber(Intent.ACTION_DIAL, "TEL", "0912-345-678"))
        assertEquals("", dialForwardNumber(Intent.ACTION_DIAL, "http", "0912345678"))
        assertEquals("", dialForwardNumber(Intent.ACTION_VIEW, null, "0912345678"))
        assertEquals("", dialForwardNumber(Intent.ACTION_CALL_BUTTON, "tel", "---()"))
    }

    @Test
    fun extras_and_flags_are_not_carried() {
        val plain = dialForwardNumber(Intent.ACTION_VIEW, "tel", "0912-345-678")
        val withExtras = dialForwardNumber(
            Intent.ACTION_VIEW,
            "tel",
            "0912-345-678",
            ignoredExtras = mapOf(
                Intent.EXTRA_PHONE_NUMBER to "999",
                "from" to "test",
            ),
            ignoredFlags = Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_GRANT_READ_URI_PERMISSION,
        )
        assertEquals("0912345678", plain)
        assertEquals(plain, withExtras)
    }
}

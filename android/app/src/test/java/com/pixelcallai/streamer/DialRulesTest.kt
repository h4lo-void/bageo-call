// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

package com.pixelcallai.streamer

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class DialRulesTest {
    @Test
    fun place_call_refuses_emergency_short_and_mmi_codes() {
        for (n in listOf("110", "119", "112", "911", "999", "165", "1999", "+886110", "0800")) {
            assertEquals(n, DialRules.NOT_ALLOWED, DialRules.check(n, "tw"))
        }
        assertEquals(DialRules.BAD_NUMBER, DialRules.check("*21*0912345678#", "tw"))
        assertEquals(DialRules.BAD_NUMBER, DialRules.check("#31#", ""))
        assertEquals(DialRules.BAD_NUMBER, DialRules.check("09-1234-abcd", "tw"))
        assertEquals(DialRules.BAD_NUMBER, DialRules.check("", "tw"))
    }

    @Test
    fun place_call_refuses_premium_and_abroad_by_prefix() {
        assertEquals(DialRules.NOT_ALLOWED, DialRules.check("0204123456", "tw"))
        assertEquals(DialRules.NOT_ALLOWED, DialRules.check("+886 204 123 456", "us"))
        assertEquals(DialRules.NOT_ALLOWED, DialRules.check("+1 900 555 0100", "tw"))
        assertEquals(DialRules.NOT_ALLOWED, DialRules.check("19005550100", "us"))
        assertEquals(DialRules.NOT_ALLOWED, DialRules.check("+44 909 879 0000", ""))
        assertEquals(DialRules.NOT_ALLOWED, DialRules.check("0990123456", "jp"))
        assertEquals(DialRules.NOT_ALLOWED, DialRules.check("002 1 555 0100", "tw"))
        assertEquals(DialRules.NOT_ALLOWED, DialRules.check("0019175550100", ""))
        assertEquals(DialRules.NOT_ALLOWED, DialRules.check("011 44 20 7946 0000", "us"))
        assertEquals(DialRules.NOT_ALLOWED, DialRules.check("010 1 555 0100", "jp"))
    }

    @Test
    fun place_call_allows_ordinary_and_toll_free_numbers() {
        assertNull(DialRules.check("0912345678", "tw"))
        assertNull(DialRules.check("0912-345-678", "tw"))
        assertNull(DialRules.check("０９１２３４５６７８", "tw"))
        assertNull(DialRules.check("+886 912 345 678", ""))
        assertNull(DialRules.check("0800 000 123", "tw"))
        assertNull(DialRules.check("+1 415 555 0100", "tw"))
        assertNull(DialRules.check("(415) 555-0100", "us"))
        assertNull(DialRules.check("+81 3 1234 5678", "jp"))
        assertNull(DialRules.check("+33 1 23 45 67 89", ""))
    }
}

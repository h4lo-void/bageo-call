// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

package com.pixelcallai.streamer

import org.junit.Assert.assertEquals
import org.junit.Test

class DeviceNameTest {
    @Test
    fun market_name_is_used_when_present() {
        val props = mapOf("ro.product.marketname" to "Redmi K60 Ultra")
        assertEquals("Redmi K60 Ultra", DeviceName.pick("23078PND5G") { props[it] })
    }

    @Test
    fun later_property_is_used_when_earlier_ones_are_missing() {
        val props = mapOf("ro.config.marketing_name" to "Redmi K60 Ultra")
        assertEquals("Redmi K60 Ultra", DeviceName.pick("23078PND5G") { props[it] })
    }

    @Test
    fun falls_back_to_model_when_all_empty() {
        assertEquals("Pixel 7", DeviceName.pick("Pixel 7") { "" })
        assertEquals("Pixel 7", DeviceName.pick(" Pixel 7 ") { null })
    }

    @Test
    fun blank_values_are_skipped() {
        val props = mapOf(
            "ro.product.marketname" to "   ",
            "ro.product.vendor.marketname" to " Redmi K60 Ultra ",
        )
        assertEquals("Redmi K60 Ultra", DeviceName.pick("23078PND5G") { props[it] })
    }

    @Test
    fun a_throwing_lookup_counts_as_missing() {
        assertEquals("Pixel 7", DeviceName.pick("Pixel 7") { throw IllegalStateException("no") })
    }
}

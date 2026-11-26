// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

package com.pixelcallai.streamer

import android.os.Build

/** Name shown for this phone (sent to Bageo and used as the label in the app). */
object DeviceName {
    private val MARKET_NAME_PROPERTIES = listOf(
        "ro.product.marketname",
        "ro.product.vendor.marketname",
        "ro.product.odm.marketname",
        "ro.config.marketing_name",
    )

    /**
     * The retail name (e.g. "Redmi K60 Ultra" instead of "23078PND5G") when the phone
     * publishes one, otherwise [Build.MODEL]. Never reads the user-editable device name
     * from Settings, which can hold a person's name.
     */
    val display: String by lazy { pick(Build.MODEL.orEmpty()) { readSystemProperty(it) } }

    /** First non-blank value of the market-name properties, else [model] (trimmed). */
    internal fun pick(model: String, read: (String) -> String?): String {
        for (key in MARKET_NAME_PROPERTIES) {
            val value = try {
                read(key)?.trim()
            } catch (_: Exception) {
                null
            }
            if (!value.isNullOrEmpty()) return value
        }
        return model.trim()
    }

    private fun readSystemProperty(key: String): String? = try {
        Class.forName("android.os.SystemProperties")
            .getMethod("get", String::class.java, String::class.java)
            .invoke(null, key, "") as? String
    } catch (_: Throwable) {
        null
    }
}

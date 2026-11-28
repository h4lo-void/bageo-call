// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

package com.pixelcallai.streamer

/**
 * Numbers the app refuses to call or text for Bageo (`place_call`, `add_call`,
 * `send_sms`), whatever Bageo asks: emergency and other short codes, MMI codes, numbers
 * written after an international call prefix, and premium-rate numbers. The same rules
 * as Bageo's own check (`errands.rs`: `clean_number`, `short_code`,
 * `international_prefix`, `PREMIUM`), which also checks the number plan in full; this
 * copy is the phone's last line. Plain Kotlin, so the rules are easy to test.
 */
object DialRules {
    const val BAD_NUMBER = "bad_number"
    const val NOT_ALLOWED = "not_allowed"

    /** Premium-rate national prefixes (after the trunk 0), by country. */
    private val PREMIUM = mapOf(
        "tw" to listOf("20"),
        "jp" to listOf("990"),
        "kr" to listOf("60"),
        "hk" to listOf("900"),
        "sg" to listOf("1900"),
        "gb" to listOf("9"),
        "de" to listOf("900", "137", "138"),
        "fr" to listOf("89"),
        "au" to listOf("190"),
    )
    private val NANP_PREMIUM = listOf("900", "976")

    /** Calling codes of the countries above (and +1), longest first. */
    private val CODES = listOf(
        "886" to "tw", "852" to "hk", "81" to "jp", "82" to "kr", "65" to "sg",
        "44" to "gb", "49" to "de", "33" to "fr", "61" to "au", "1" to "us",
    )
    private val NANP = setOf("us", "ca", "pr", "vi", "gu", "as", "mp", "do", "jm", "tt", "bs", "bb")

    /** The number without what people write around the digits; null when it is not one. */
    fun clean(raw: String): String? {
        val out = StringBuilder()
        for (c0 in raw.trim()) {
            val c = when (c0) {
                in '０'..'９' -> '0' + (c0 - '０')
                '＋' -> '+'
                else -> c0
            }
            when {
                c in '0'..'9' -> out.append(c)
                c == '+' && out.isEmpty() -> out.append('+')
                c == ' ' || c == '-' || c == '(' || c == ')' || c == '.' || c == '\u3000' || c == '－' || c == '（' || c == '）' -> {}
                else -> return null
            }
        }
        val digits = out.trimStart('+').length
        return if (digits in 1..15) out.toString() else null
    }

    private fun internationalPrefix(region: String, digits: String): Boolean =
        digits.startsWith("00") ||
            (region == "tw" && digits.startsWith("01")) ||
            (region in NANP && digits.startsWith("011")) ||
            (region == "jp" && digits.startsWith("010")) ||
            ((region == "ru" || region == "kz") && digits.startsWith("810"))

    private fun premium(region: String, national: String): Boolean {
        val list = if (region in NANP) NANP_PREMIUM else PREMIUM[region].orEmpty()
        return list.any { national.startsWith(it) } || (region == "tw" && national.startsWith("1"))
    }

    /**
     * Null when [number] may be called or texted from a SIM of [simCountry] (ISO 3166
     * alpha-2, "" when unknown); else [BAD_NUMBER] or [NOT_ALLOWED].
     */
    fun check(number: String, simCountry: String): String? {
        if (number.contains('*') || number.contains('#')) return BAD_NUMBER
        val n = clean(number) ?: return BAD_NUMBER
        val digits = n.trimStart('+')
        if (digits.length <= 7) return NOT_ALLOWED
        if (n.startsWith('+')) {
            val (code, region) = CODES.firstOrNull { digits.startsWith(it.first) } ?: return null
            val national = digits.removePrefix(code).trimStart('0')
            return if (premium(region, national)) NOT_ALLOWED else null
        }
        val region = simCountry.trim().lowercase()
        if (internationalPrefix(region, digits)) return NOT_ALLOWED
        if (region.isEmpty()) return null
        val national = if (region in NANP && digits.length == 11 && digits.startsWith('1')) digits.drop(1) else digits.trimStart('0')
        return if (premium(region, national)) NOT_ALLOWED else null
    }
}

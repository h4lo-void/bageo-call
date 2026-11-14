package com.pixelcallai.streamer

import org.json.JSONObject
import java.util.Locale

/**
 * SIM subscription ids: the optional `sub_id` of agent commands and phone accounts, and
 * the optional `sim` of `send_sms` (a SIM named by carrier or by its own number).
 */
object SimSubscriptions {
    private const val KEY = "sub_id"
    private const val SIM_KEY = "sim"
    private const val INVALID = -1

    /** Digits a `sim` needs before it is also compared with the SIMs' own numbers. */
    const val MIN_NUMBER_DIGITS = 7

    /** One active SIM as [matching] sees it; [number] null = unknown or not readable. */
    data class Sim(
        val subId: Int,
        val carrierName: String?,
        val displayName: String?,
        val number: String?,
    )

    /** `sub_id` of an agent command; null when absent or not a subscription id (= default SIM). */
    fun fromCommand(obj: JSONObject): Int? {
        if (!obj.has(KEY) || obj.isNull(KEY)) return null
        return obj.optInt(KEY, INVALID).takeIf { isValid(it) }
    }

    /** `sim` of an agent `send_sms`, trimmed; null when absent or blank. */
    fun simFromCommand(obj: JSONObject): String? {
        if (!obj.has(SIM_KEY) || obj.isNull(SIM_KEY)) return null
        return obj.optString(SIM_KEY).trim().takeIf { it.isNotEmpty() }
    }

    /** True when [wanted] has enough digits to be compared with the SIMs' numbers. */
    fun looksLikeNumber(wanted: String): Boolean = digitsOf(wanted).length >= MIN_NUMBER_DIGITS

    /**
     * The subscription ids of the [sims] that [wanted] names: carrier name or display name
     * equal to it or containing it (any case), or, when [wanted] has at least
     * [MIN_NUMBER_DIGITS] digits, a SIM whose own number ends in the same digits (the
     * shorter of the two numbers, at least [MIN_NUMBER_DIGITS] of them). A text goes out
     * only when exactly one matches.
     */
    fun matching(sims: List<Sim>, wanted: String): List<Int> {
        val name = wanted.trim().lowercase(Locale.ROOT)
        if (name.isEmpty()) return emptyList()
        val digits = digitsOf(wanted)
        return sims.filter { sim ->
            nameHas(sim.carrierName, name) || nameHas(sim.displayName, name) ||
                (digits.length >= MIN_NUMBER_DIGITS && sameNumber(sim.number, digits))
        }.map { it.subId }.distinct()
    }

    private fun nameHas(simName: String?, wanted: String): Boolean {
        val n = simName?.trim()?.lowercase(Locale.ROOT)
        return !n.isNullOrEmpty() && n.contains(wanted)
    }

    private fun sameNumber(simNumber: String?, wantedDigits: String): Boolean {
        val own = digitsOf(simNumber ?: return false)
        val n = minOf(own.length, wantedDigits.length)
        return n >= MIN_NUMBER_DIGITS && own.takeLast(n) == wantedDigits.takeLast(n)
    }

    private fun digitsOf(s: String): String = s.filter { it in '0'..'9' }

    /** Real subscription ids are 0 or more; Int.MAX_VALUE means "the default one". */
    fun isValid(subId: Int): Boolean = subId >= 0 && subId != Int.MAX_VALUE

    /** The first [accounts] entry whose subscription id is [subId]. */
    fun <T> pick(accounts: List<Pair<T, Int>>, subId: Int): T? =
        accounts.firstOrNull { it.second == subId }?.first
}

package com.pixelcallai.streamer

import org.json.JSONObject

/** SIM subscription ids: the optional `sub_id` of agent commands and phone accounts. */
object SimSubscriptions {
    private const val KEY = "sub_id"
    private const val INVALID = -1

    /** `sub_id` of an agent command; null when absent or not a subscription id (= default SIM). */
    fun fromCommand(obj: JSONObject): Int? {
        if (!obj.has(KEY) || obj.isNull(KEY)) return null
        return obj.optInt(KEY, INVALID).takeIf { isValid(it) }
    }

    /** Real subscription ids are 0 or more; Int.MAX_VALUE means "the default one". */
    fun isValid(subId: Int): Boolean = subId >= 0 && subId != Int.MAX_VALUE

    /** The first [accounts] entry whose subscription id is [subId]. */
    fun <T> pick(accounts: List<Pair<T, Int>>, subId: Int): T? =
        accounts.firstOrNull { it.second == subId }?.first
}

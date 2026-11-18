// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

package com.pixelcallai.streamer

import android.Manifest
import android.content.Context
import android.content.pm.PackageManager
import android.os.Build
import android.telephony.SubscriptionInfo
import android.telephony.SubscriptionManager
import android.telephony.TelephonyManager
import android.util.Log
import androidx.core.content.ContextCompat
import org.json.JSONArray
import org.json.JSONObject

/**
 * The SIMs of this phone as the agent's `sims` list in the control `hello` and `status`:
 * `[{"sub_id": 3, "country": "tw", "slot": 0, "carrier": "...", "number": "..."}, ...]` —
 * every active SIM, so the agent knows a second SIM and its line number (when the card
 * exposes one) before any call came in on it.
 */
object SimList {
    private const val TAG = "SimList"

    /** One SIM: subscription id, two-letter country, slot (-1 = unknown), optional carrier & number. */
    class Sim(
        val subId: Int,
        val country: String,
        val slot: Int,
        val carrier: String = "",
        val number: String = "",
    )

    /** A lower-case two-letter country code; "" when [raw] is not one. */
    fun cleanCountry(raw: String?): String {
        val c = raw.orEmpty().trim().lowercase()
        return if (c.length == 2 && c.all { it in 'a'..'z' }) c else ""
    }

    /** Clean a SIM-reported phone number to `+` and digits; "" when absent or unreadable. */
    fun cleanNumber(raw: String?): String {
        val out = StringBuilder()
        for (ch in raw.orEmpty().trim()) {
            when {
                ch in '0'..'9' -> out.append(ch)
                ch == '+' && out.isEmpty() -> out.append('+')
                ch == ' ' || ch == '-' || ch == '(' || ch == ')' || ch == '.' -> {}
                else -> return ""
            }
        }
        val digits = out.removePrefix("+").length
        return if (digits in 4..15) out.toString() else ""
    }

    /**
     * The `sims` array; null when no SIM has a real subscription id and a country (the
     * agent ignores a list it cannot read, so then the field is left out).
     */
    fun toJson(sims: List<Sim>): JSONArray? {
        val usable = sims
            .filter { SimSubscriptions.isValid(it.subId) && it.country.isNotEmpty() }
            .distinctBy { it.subId }
            .sortedBy { it.subId }
        if (usable.isEmpty()) return null
        val list = JSONArray()
        for (sim in usable) {
            val item = JSONObject().put("sub_id", sim.subId).put("country", sim.country)
            if (sim.slot >= 0) item.put("slot", sim.slot)
            if (sim.carrier.isNotEmpty()) item.put("carrier", sim.carrier)
            if (sim.number.isNotEmpty()) item.put("number", sim.number)
            list.put(item)
        }
        return list
    }

    @Suppress("DEPRECATION")
    private fun readNumber(
        context: Context,
        subscriptions: SubscriptionManager,
        info: SubscriptionInfo,
    ): String {
        val hasPerm = ContextCompat.checkSelfPermission(
            context,
            Manifest.permission.READ_PHONE_NUMBERS,
        ) == PackageManager.PERMISSION_GRANTED
        if (hasPerm && Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            try {
                val n = cleanNumber(subscriptions.getPhoneNumber(info.subscriptionId))
                if (n.isNotEmpty()) return n
            } catch (_: Exception) {
            }
        }
        return try {
            cleanNumber(info.number)
        } catch (_: Exception) {
            ""
        }
    }

    /** The active SIMs now; null without READ_PHONE_STATE or when none can be read. */
    fun read(context: Context): JSONArray? {
        if (ContextCompat.checkSelfPermission(context, Manifest.permission.READ_PHONE_STATE)
            != PackageManager.PERMISSION_GRANTED
        ) {
            return null
        }
        return try {
            val subscriptions = context.getSystemService(SubscriptionManager::class.java) ?: return null
            val telephony = context.getSystemService(TelephonyManager::class.java) ?: return null
            val sims = subscriptions.activeSubscriptionInfoList.orEmpty().map { info ->
                val country = telephony.createForSubscriptionId(info.subscriptionId).simCountryIso
                val carrier = info.displayName?.toString().orEmpty().trim().ifEmpty {
                    info.carrierName?.toString().orEmpty().trim()
                }.take(40)
                val number = readNumber(context, subscriptions, info)
                Sim(info.subscriptionId, cleanCountry(country), info.simSlotIndex, carrier, number)
            }
            toJson(sims)
        } catch (e: SecurityException) {
            Log.w(TAG, "SIM list not readable: ${e.message}")
            null
        } catch (e: Exception) {
            Log.w(TAG, "SIM list failed: ${e.message}")
            null
        }
    }
}
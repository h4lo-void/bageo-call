package com.pixelcallai.streamer

import android.Manifest
import android.content.Context
import android.content.pm.PackageManager
import android.telephony.SubscriptionManager
import android.telephony.TelephonyManager
import android.util.Log
import androidx.core.content.ContextCompat
import org.json.JSONArray
import org.json.JSONObject

/**
 * The SIMs of this phone as the agent's `sims` list in the control `hello` and `status`:
 * `[{"sub_id": 3, "country": "tw", "slot": 0}, ...]` — every active SIM, so the agent
 * knows a second SIM before any call came in on it.
 */
object SimList {
    private const val TAG = "SimList"

    /** One SIM: subscription id, two-letter country of the card ("" = unknown), slot (-1 = unknown). */
    class Sim(val subId: Int, val country: String, val slot: Int)

    /** A lower-case two-letter country code; "" when [raw] is not one. */
    fun cleanCountry(raw: String?): String {
        val c = raw.orEmpty().trim().lowercase()
        return if (c.length == 2 && c.all { it in 'a'..'z' }) c else ""
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
            list.put(item)
        }
        return list
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
                Sim(info.subscriptionId, cleanCountry(country), info.simSlotIndex)
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
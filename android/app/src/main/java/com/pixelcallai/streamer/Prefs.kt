// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

package com.pixelcallai.streamer

import android.content.Context
import android.content.SharedPreferences
import java.io.File
import java.net.URI
import org.json.JSONArray

object Prefs {
    private const val NAME = "pixel_call_ai"
    const val KEY_MODE = "AUDIO_MODE"
    const val KEY_LAST_CALLER_ID = "LAST_CALLER_ID"
    const val KEY_LAST_SIM_COUNTRY = "LAST_SIM_COUNTRY"
    const val KEY_LAST_SIM_SUB_ID = "LAST_SIM_SUB_ID"
    const val KEY_AGENT_URL = "AGENT_URL"
    /**
     * Wi-Fi addresses the agent sent over USB (`agent_info`), as a JSON array. They carry
     * the pairing password, so they stay in this private storage and out of every log.
     */
    const val KEY_WIFI_URLS = "WIFI_URLS"
    /**
     * The address paired by IP address ([IpPairing]), with the pairing key. Kept apart from
     * [KEY_WIFI_URLS]: a Bageo in Docker sends no Wi-Fi addresses, and that must not clear it.
     */
    const val KEY_IP_URL = "IP_PAIRED_URL"
    /** Web console address the agent sent (`agent_info`); cleared on every new connection. */
    const val KEY_CONSOLE_URL = "CONSOLE_URL"
    /** Version of the connected Bageo agent (`agent_info`); cleared on every new connection. */
    const val KEY_AGENT_VERSION = "AGENT_VERSION"
    /** Id of the last mode switch whose `set_mode_result` went to the agent. */
    const val KEY_REPORTED_APPLY_ID = "REPORTED_APPLY_ID"
    /**
     * The switch of app versions before 0.2.2 ("AI answering", default on). It is not
     * used any more; when an upgraded phone still has it off, the setup screen says that
     * the phone now answers with the AI until the new switch is turned off.
     */
    private const val KEY_LEGACY_AI_CALL_ENABLED = "AI_CALL_ENABLED"
    /**
     * Legacy agent-synced spam blocklist key. Reject gating is **disabled**;
     * [setBlocklist] may still store unused values from `blocklist_sync`.
     */
    const val KEY_BLOCKLIST = "SPAM_BLOCKLIST"
    const val MODE_SILENT_LOCAL = "SILENT_LOCAL"

    /** The AI answers every call (the module's takeover). */
    const val PHONE_MODE_AI = "ai"
    /** An ordinary phone: calls ring and the owner answers them. */
    const val PHONE_MODE_NORMAL = "normal"

    /**
     * Mirror of the module's `/data/adb/pixelcallai/mode` (see [PhoneModeManager]).
     * A plain file, not SharedPreferences: the `:screening` process reads it too, and
     * SharedPreferences would serve it a stale cached copy.
     */
    private const val PHONE_MODE_FILE = "phone_mode"

    /**
     * Characters dropped before the mode is compared, the same as service.sh and the
     * root script: blanks (space, tab, CR, LF, VT, FF) and a UTF-8 byte order mark.
     */
    private const val MODE_JUNK = " \t\r\n\u000B\u000C\uFEFF"

    /**
     * This process's copy of the mode, so the call path does not read a file every
     * 500 ms. Set by [setPhoneMode] (the main process makes every switch); the
     * `:screening` process reloads it with [refreshPhoneMode] once per call.
     */
    @Volatile
    private var cachedPhoneMode: String? = null

    /**
     * Device-protected prefs so BootReceiver / direct-boot arming works before the
     * lockscreen is dismissed (a dedicated phone usually sits locked on a USB cable).
     */
    private fun sp(ctx: Context): SharedPreferences {
        val de = ctx.createDeviceProtectedStorageContext()
        return de.getSharedPreferences(NAME, Context.MODE_PRIVATE)
    }

    private fun phoneModeFile(ctx: Context): File =
        File(ctx.createDeviceProtectedStorageContext().filesDir, PHONE_MODE_FILE)

    /**
     * [PHONE_MODE_AI] unless the phone was switched to a normal phone. No file = AI,
     * which is how every phone ran before the switch existed. (The old
     * `AI_CALL_ENABLED` preference does not change it; see [legacyAiSwitchOff].)
     */
    fun phoneMode(ctx: Context): String = cachedPhoneMode ?: refreshPhoneMode(ctx)

    /** Read the mode file again (the `:screening` process, once per incoming call). */
    fun refreshPhoneMode(ctx: Context): String {
        val text = try {
            val f = phoneModeFile(ctx)
            if (f.isFile) f.readText() else ""
        } catch (_: Exception) {
            ""
        }
        return normalizeMode(text).also { cachedPhoneMode = it }
    }

    /** "normal" after dropping blanks and a BOM; anything else is AI. */
    fun normalizeMode(raw: String): String =
        if (raw.filterNot { it in MODE_JUNK } == PHONE_MODE_NORMAL) PHONE_MODE_NORMAL else PHONE_MODE_AI

    fun hasPhoneModeFile(ctx: Context): Boolean = try {
        phoneModeFile(ctx).isFile
    } catch (_: Exception) {
        false
    }

    /** Only [PhoneModeManager] calls this, after the module's file says the same. */
    fun setPhoneMode(ctx: Context, mode: String): Boolean {
        val file = phoneModeFile(ctx)
        return try {
            file.parentFile?.mkdirs()
            val tmp = File(file.parentFile, "$PHONE_MODE_FILE.tmp")
            tmp.writeText(mode)
            if (!tmp.renameTo(file)) file.writeText(mode)
            refreshPhoneMode(ctx) == mode
        } catch (_: Exception) {
            false
        }
    }

    /** True in AI mode: the app answers calls, mutes the phone and holds the phone-app roles. */
    fun isAiCallEnabled(ctx: Context): Boolean = phoneMode(ctx) == PHONE_MODE_AI

    /** The old "AI answering" switch was off on this phone (before the upgrade). */
    fun legacyAiSwitchOff(ctx: Context): Boolean =
        !sp(ctx).getBoolean(KEY_LEGACY_AI_CALL_ENABLED, true)

    /** After the first switch with the new control the old setting means nothing. */
    fun clearLegacyAiSwitch(ctx: Context) {
        sp(ctx).edit().remove(KEY_LEGACY_AI_CALL_ENABLED).apply()
    }

    fun reportedApplyId(ctx: Context): String =
        sp(ctx).getString(KEY_REPORTED_APPLY_ID, "").orEmpty()

    fun setReportedApplyId(ctx: Context, id: String) {
        sp(ctx).edit().putString(KEY_REPORTED_APPLY_ID, id).apply()
    }

    /** The web console's address, when the connected agent sent one. */
    fun consoleUrl(ctx: Context): String? =
        sp(ctx).getString(KEY_CONSOLE_URL, null)?.trim()?.takeIf { isWebUrl(it) }

    /** Blank or not an http(s) address removes it (the button is hidden). */
    fun setConsoleUrl(ctx: Context, url: String?) {
        val cleaned = url?.trim().orEmpty()
        val editor = sp(ctx).edit()
        if (isWebUrl(cleaned)) editor.putString(KEY_CONSOLE_URL, cleaned) else editor.remove(KEY_CONSOLE_URL)
        editor.apply()
    }

    /** Version of the connected Bageo agent, when the agent sent one. */
    fun agentVersion(ctx: Context): String? =
        sp(ctx).getString(KEY_AGENT_VERSION, null)?.trim()?.takeIf { it.isNotEmpty() }

    /** Blank removes it (the version mismatch hint is hidden). */
    fun setAgentVersion(ctx: Context, version: String?) {
        val cleaned = version?.trim().orEmpty()
        val editor = sp(ctx).edit()
        if (cleaned.isNotEmpty()) editor.putString(KEY_AGENT_VERSION, cleaned) else editor.remove(KEY_AGENT_VERSION)
        editor.apply()
    }

    fun isWebUrl(url: String): Boolean {
        val u = url.trim()
        if (!u.startsWith("http://") && !u.startsWith("https://")) return false
        return try {
            !URI(u).host.isNullOrBlank()
        } catch (_: Exception) {
            false
        }
    }

    /**
     * WebSocket URL of the agent. Default [BuildConfig.BRIDGE_WS_URL]
     * (`ws://127.0.0.1:8765`, reached through `adb reverse` over USB). A stored value
     * that is not a valid `ws://` / `wss://` URL is ignored, so a bad setting can
     * never break the call path.
     */
    fun agentUrl(ctx: Context): String {
        val stored = sp(ctx).getString(KEY_AGENT_URL, null)?.trim()
        return if (stored != null && isValidAgentUrl(stored)) stored else BuildConfig.BRIDGE_WS_URL
    }

    /** Blank, invalid or default value clears the override (back to the USB default). */
    fun setAgentUrl(ctx: Context, url: String?) {
        val cleaned = url?.trim().orEmpty()
        val editor = sp(ctx).edit()
        if (cleaned == BuildConfig.BRIDGE_WS_URL || !isValidAgentUrl(cleaned)) {
            editor.remove(KEY_AGENT_URL)
        } else {
            editor.putString(KEY_AGENT_URL, cleaned)
        }
        editor.apply()
    }

    /** Must start with `ws://` or `wss://` and parse to a URI with a host. */
    fun isValidAgentUrl(url: String): Boolean {
        val u = url.trim()
        if (!u.startsWith("ws://") && !u.startsWith("wss://")) return false
        return try {
            !URI(u).host.isNullOrBlank()
        } catch (_: Exception) {
            false
        }
    }

    /** For logs: drop the query string, which may carry an access token. */
    fun redactUrl(url: String): String = url.substringBefore('?')

    /** True when the user typed their own address under Advanced (it replaces USB and Wi-Fi). */
    fun isManualAgentUrl(ctx: Context): Boolean = agentUrl(ctx) != BuildConfig.BRIDGE_WS_URL

    /** The Wi-Fi addresses the agent gave this phone; empty when not paired. */
    fun wifiUrls(ctx: Context): List<String> {
        val raw = sp(ctx).getString(KEY_WIFI_URLS, null) ?: return emptyList()
        return try {
            val arr = JSONArray(raw)
            (0 until arr.length()).map { arr.optString(it) }.filter { isValidAgentUrl(it) }
                .distinct().take(ConnectionPlan.MAX_WIFI_URLS)
        } catch (_: Exception) {
            emptyList()
        }
    }

    /** Only valid `ws://` / `wss://` addresses are kept; an empty list forgets Wi-Fi. */
    fun setWifiUrls(ctx: Context, urls: List<String>) {
        val clean = urls.map { it.trim() }.filter { isValidAgentUrl(it) }.distinct()
            .take(ConnectionPlan.MAX_WIFI_URLS)
        val editor = sp(ctx).edit()
        if (clean.isEmpty()) editor.remove(KEY_WIFI_URLS) else editor.putString(KEY_WIFI_URLS, JSONArray(clean).toString())
        editor.apply()
    }

    fun clearWifiUrls(ctx: Context) = setWifiUrls(ctx, emptyList())

    /** The address paired by IP address, when there is one. */
    fun ipUrl(ctx: Context): String? =
        sp(ctx).getString(KEY_IP_URL, null)?.trim()?.takeIf { isValidAgentUrl(it) }

    /** Null or an invalid address forgets it. */
    fun setIpUrl(ctx: Context, url: String?) {
        val cleaned = url?.trim().orEmpty()
        val editor = sp(ctx).edit()
        if (isValidAgentUrl(cleaned)) editor.putString(KEY_IP_URL, cleaned) else editor.remove(KEY_IP_URL)
        editor.apply()
    }

    /** The phone can reach Bageo without the cable: Wi-Fi addresses from USB, or pairing by IP. */
    fun hasNetworkPairing(ctx: Context): Boolean = wifiUrls(ctx).isNotEmpty() || ipUrl(ctx) != null

    /** "Forget Wi-Fi connection": both kinds. */
    fun forgetNetworkPairing(ctx: Context) {
        clearWifiUrls(ctx)
        setIpUrl(ctx, null)
    }

    /**
     * Handles the `wifi` part of `agent_info`: [incoming] null = the field was missing, keep
     * what is stored; an empty list clears it.
     */
    fun applyAgentWifi(ctx: Context, incoming: List<String>?) {
        if (incoming == null) return
        setWifiUrls(ctx, ConnectionPlan.nextWifiUrls(wifiUrls(ctx), incoming) { isValidAgentUrl(it) })
    }

    /** The addresses to try, in order (see [ConnectionPlan.candidates]). */
    fun connectionCandidates(ctx: Context): List<String> =
        if (BuildConfig.STANDALONE) {
            listOf(BuildConfig.BRIDGE_WS_URL)
        } else {
            ConnectionPlan.candidates(
                BuildConfig.BRIDGE_WS_URL,
                agentUrl(ctx),
                ConnectionPlan.networkUrls(ipUrl(ctx), wifiUrls(ctx)),
            )
        }

    fun audioMode(ctx: Context): String =
        sp(ctx).getString(KEY_MODE, MODE_SILENT_LOCAL) ?: MODE_SILENT_LOCAL

    /** Incoming caller id from screening / InCall / PHONE_STATE (not the phone's own MSISDN). */
    fun lastCallerId(ctx: Context): String =
        sp(ctx).getString(KEY_LAST_CALLER_ID, "") ?: ""

    fun setLastCallerId(ctx: Context, callerId: String) {
        val cleaned = callerId.trim()
        if (cleaned.isEmpty()) return
        sp(ctx).edit().putString(KEY_LAST_CALLER_ID, cleaned).apply()
    }

    fun clearLastCallerId(ctx: Context) {
        sp(ctx).edit().remove(KEY_LAST_CALLER_ID).apply()
    }

    /**
     * Country of the SIM the current call came in on (ISO 3166 alpha-2, lowercase;
     * "" = unknown). The agent waits for a carrier's international-call warning only
     * on SIMs whose carrier plays one.
     */
    fun lastSimCountry(ctx: Context): String =
        sp(ctx).getString(KEY_LAST_SIM_COUNTRY, "") ?: ""

    fun setLastSimCountry(ctx: Context, iso: String) {
        sp(ctx).edit().putString(KEY_LAST_SIM_COUNTRY, iso.trim().lowercase()).apply()
    }

    /** Subscription id of the SIM the current call uses; null = unknown. */
    fun lastSimSubId(ctx: Context): Int? =
        sp(ctx).getInt(KEY_LAST_SIM_SUB_ID, NO_SUB_ID).takeIf { it >= 0 }

    fun setLastSimSubId(ctx: Context, subId: Int?) {
        val editor = sp(ctx).edit()
        if (subId == null || subId < 0) editor.remove(KEY_LAST_SIM_SUB_ID) else editor.putInt(KEY_LAST_SIM_SUB_ID, subId)
        editor.apply()
    }

    /**
     * A new call starts: forget the previous call's number and SIM, so a hidden number
     * can never show up as the last caller.
     */
    fun clearLastCallInfo(ctx: Context) {
        sp(ctx).edit()
            .remove(KEY_LAST_CALLER_ID)
            .putString(KEY_LAST_SIM_COUNTRY, "")
            .remove(KEY_LAST_SIM_SUB_ID)
            .apply()
    }

    fun blocklist(ctx: Context): Set<String> =
        sp(ctx).getStringSet(KEY_BLOCKLIST, emptySet()) ?: emptySet()

    /** Stored but unused — blocklist reject is disabled; screening never consults this. */
    fun setBlocklist(ctx: Context, numbers: Collection<String>) {
        val cleaned = numbers.map { it.trim() }.filter { it.isNotEmpty() }.toSet()
        sp(ctx).edit().putStringSet(KEY_BLOCKLIST, cleaned).apply()
    }

    /** Always false — spam blocklist / pre-answer reject is disabled. */
    @Suppress("UNUSED_PARAMETER")
    fun isBlocklisted(ctx: Context, caller: String): Boolean = false

    /** Kept for the prefs contract / historical normalization (spaces + dashes). */
    @Suppress("unused")
    private fun normalizeNumber(s: String): String =
        s.filter { !it.isWhitespace() && it != '-' }

    private const val KEY_STANDALONE_LAN = "STANDALONE_LAN_ENABLED"
    private const val KEY_STANDALONE_DAILY_SOUND = "STANDALONE_DAILY_SOUND"
    private const val KEY_STANDALONE_PERSISTENT_NOTIF = "STANDALONE_PERSISTENT_NOTIF"
    private const val KEY_STANDALONE_CALL_SUMMARY_NOTIF = "STANDALONE_CALL_SUMMARY_NOTIF"
    private const val KEY_STANDALONE_WHITELIST_ACTION = "STANDALONE_WHITELIST_ACTION"
    private const val KEY_STANDALONE_DND_AUTO_AI = "STANDALONE_DND_AUTO_AI"
    private const val KEY_STANDALONE_DND_ANY_MODE = "STANDALONE_DND_ANY_MODE"
    private const val KEY_STANDALONE_DND_RULE_ID = "STANDALONE_DND_RULE_ID"
    private const val KEY_SMS_LAST_INBOX_ID = "SMS_LAST_INBOX_ID"
    private const val KEY_SMS_PENDING_QUEUE = "SMS_PENDING_QUEUE"
    private const val KEY_SMS_DEDUP_CACHE = "SMS_DEDUP_CACHE"

    const val WHITELIST_ACTION_URGENT_PROMPT = "urgent_prompt"
    const val WHITELIST_ACTION_RING_PHONE = "ring_phone"
    const val WHITELIST_ACTION_FORWARD_NUMBER = "forward_number"
    const val WHITELIST_ACTION_AI_ONLY = "ai_only"

    /** Standalone edition: whether same-Wi-Fi LAN console access is turned on (default false). */
    fun isStandaloneLanEnabled(ctx: Context): Boolean =
        sp(ctx).getBoolean(KEY_STANDALONE_LAN, false)

    fun setStandaloneLanEnabled(ctx: Context, enabled: Boolean) {
        sp(ctx).edit().putBoolean(KEY_STANDALONE_LAN, enabled).apply()
    }

    /** Standalone edition: keep daily notification/media sounds intact while AI handles calls (default true). */
    fun isStandaloneDailySound(ctx: Context): Boolean =
        sp(ctx).getBoolean(KEY_STANDALONE_DAILY_SOUND, true)

    fun setStandaloneDailySound(ctx: Context, enabled: Boolean) {
        sp(ctx).edit().putBoolean(KEY_STANDALONE_DAILY_SOUND, enabled).apply()
    }

    /** Standalone edition: keep pinned "AI 正在幫你接電話" notification visible when idle (default true). */
    fun isStandalonePersistentNotif(ctx: Context): Boolean =
        sp(ctx).getBoolean(KEY_STANDALONE_PERSISTENT_NOTIF, true)

    fun setStandalonePersistentNotif(ctx: Context, enabled: Boolean) {
        sp(ctx).edit().putBoolean(KEY_STANDALONE_PERSISTENT_NOTIF, enabled).apply()
    }

    /** Standalone edition: post local call summary Android notifications after AI calls (default true). */
    fun isStandaloneCallSummaryNotif(ctx: Context): Boolean =
        sp(ctx).getBoolean(KEY_STANDALONE_CALL_SUMMARY_NOTIF, true)

    fun setStandaloneCallSummaryNotif(ctx: Context, enabled: Boolean) {
        sp(ctx).edit().putBoolean(KEY_STANDALONE_CALL_SUMMARY_NOTIF, enabled).apply()
    }

    /** Standalone edition: how whitelist/important callers are handled locally. */
    fun standaloneWhitelistAction(ctx: Context): String =
        normalizeWhitelistAction(sp(ctx).getString(KEY_STANDALONE_WHITELIST_ACTION, WHITELIST_ACTION_URGENT_PROMPT).orEmpty())

    fun setStandaloneWhitelistAction(ctx: Context, action: String) {
        sp(ctx).edit().putString(KEY_STANDALONE_WHITELIST_ACTION, normalizeWhitelistAction(action)).apply()
    }

    fun normalizeWhitelistAction(raw: String): String =
        when (raw.trim().lowercase()) {
            WHITELIST_ACTION_RING_PHONE -> WHITELIST_ACTION_RING_PHONE
            WHITELIST_ACTION_FORWARD_NUMBER -> WHITELIST_ACTION_FORWARD_NUMBER
            WHITELIST_ACTION_AI_ONLY -> WHITELIST_ACTION_AI_ONLY
            else -> WHITELIST_ACTION_URGENT_PROMPT
        }

    /** Standalone edition: automatically switch AI on/off with dedicated DND rule. */
    fun isStandaloneDndAutoAi(ctx: Context): Boolean =
        sp(ctx).getBoolean(KEY_STANDALONE_DND_AUTO_AI, false)

    fun setStandaloneDndAutoAi(ctx: Context, enabled: Boolean) {
        sp(ctx).edit().putBoolean(KEY_STANDALONE_DND_AUTO_AI, enabled).apply()
    }

    /** Standalone edition: trigger AI auto-switch on any system DND mode, not only the dedicated rule. */
    fun isStandaloneDndAnyMode(ctx: Context): Boolean =
        sp(ctx).getBoolean(KEY_STANDALONE_DND_ANY_MODE, false)

    fun setStandaloneDndAnyMode(ctx: Context, enabled: Boolean) {
        sp(ctx).edit().putBoolean(KEY_STANDALONE_DND_ANY_MODE, enabled).apply()
    }

    /** Persisted system ID of Bageo's dedicated AutomaticZenRule. */
    fun standaloneDndRuleId(ctx: Context): String =
        sp(ctx).getString(KEY_STANDALONE_DND_RULE_ID, "").orEmpty()

    fun setStandaloneDndRuleId(ctx: Context, ruleId: String) {
        sp(ctx).edit().putString(KEY_STANDALONE_DND_RULE_ID, ruleId.trim()).apply()
    }

    /** Standalone SMS forwarder: last inbox `_id` seen (`-1` until baseline initialized). */
    fun smsLastInboxId(ctx: Context): Long =
        sp(ctx).getLong(KEY_SMS_LAST_INBOX_ID, -1L)

    fun setSmsLastInboxId(ctx: Context, id: Long) {
        sp(ctx).edit().putLong(KEY_SMS_LAST_INBOX_ID, id).apply()
    }

    /** Standalone SMS forwarder: persisted JSON array of unacknowledged outbound events. */
    fun smsPendingQueue(ctx: Context): String? =
        sp(ctx).getString(KEY_SMS_PENDING_QUEUE, null)

    fun setSmsPendingQueue(ctx: Context, json: String) {
        sp(ctx).edit().putString(KEY_SMS_PENDING_QUEUE, json).apply()
    }

    /** Standalone SMS forwarder: persisted JSON map of recent sender+hash dedup timestamps. */
    fun smsDedupCache(ctx: Context): String? =
        sp(ctx).getString(KEY_SMS_DEDUP_CACHE, null)

    fun setSmsDedupCache(ctx: Context, json: String) {
        sp(ctx).edit().putString(KEY_SMS_DEDUP_CACHE, json).apply()
    }

    private const val NO_SUB_ID = -1
}

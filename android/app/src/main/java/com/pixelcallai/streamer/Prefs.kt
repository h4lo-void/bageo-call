package com.pixelcallai.streamer

import android.content.Context
import android.content.SharedPreferences
import java.io.File
import java.net.URI

object Prefs {
    private const val NAME = "pixel_call_ai"
    const val KEY_MODE = "AUDIO_MODE"
    const val KEY_LAST_CALLER_ID = "LAST_CALLER_ID"
    const val KEY_LAST_SIM_COUNTRY = "LAST_SIM_COUNTRY"
    const val KEY_AGENT_URL = "AGENT_URL"
    /** Web console address the agent sent (`agent_info`); cleared on every new connection. */
    const val KEY_CONSOLE_URL = "CONSOLE_URL"
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
}

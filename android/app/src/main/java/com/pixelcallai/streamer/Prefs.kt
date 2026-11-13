package com.pixelcallai.streamer

import android.content.Context
import android.content.SharedPreferences
import java.io.File
import java.net.URI

object Prefs {
    private const val NAME = "pixel_call_ai"
    const val KEY_MODE = "AUDIO_MODE"
    const val KEY_LAST_CALLER_ID = "LAST_CALLER_ID"
    const val KEY_AGENT_URL = "AGENT_URL"
    /** Web console address the agent sent (`agent_info`); cleared on every new connection. */
    const val KEY_CONSOLE_URL = "CONSOLE_URL"
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
     * `AI_CALL_ENABLED` preference is no longer read.)
     */
    fun phoneMode(ctx: Context): String {
        val text = try {
            val f = phoneModeFile(ctx)
            if (f.isFile) f.readText().trim() else ""
        } catch (_: Exception) {
            ""
        }
        return if (text == PHONE_MODE_NORMAL) PHONE_MODE_NORMAL else PHONE_MODE_AI
    }

    /** Only [PhoneModeManager] calls this, after the module's file says the same. */
    fun setPhoneMode(ctx: Context, mode: String): Boolean {
        val file = phoneModeFile(ctx)
        return try {
            file.parentFile?.mkdirs()
            val tmp = File(file.parentFile, "$PHONE_MODE_FILE.tmp")
            tmp.writeText(mode)
            if (!tmp.renameTo(file)) file.writeText(mode)
            phoneMode(ctx) == mode
        } catch (_: Exception) {
            false
        }
    }

    /** True in AI mode: the app answers calls, mutes the phone and holds the phone-app roles. */
    fun isAiCallEnabled(ctx: Context): Boolean = phoneMode(ctx) == PHONE_MODE_AI

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

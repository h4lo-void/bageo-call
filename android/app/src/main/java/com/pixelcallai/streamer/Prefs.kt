package com.pixelcallai.streamer

import android.content.Context
import android.content.SharedPreferences
import java.net.URI

object Prefs {
    private const val NAME = "pixel_call_ai"
    const val KEY_AI_CALL_ENABLED = "AI_CALL_ENABLED"
    const val KEY_MODE = "AUDIO_MODE"
    const val KEY_LAST_CALLER_ID = "LAST_CALLER_ID"
    const val KEY_AGENT_URL = "AGENT_URL"
    /**
     * Legacy agent-synced spam blocklist key. Reject gating is **disabled**;
     * [setBlocklist] may still store unused values from `blocklist_sync`.
     */
    const val KEY_BLOCKLIST = "SPAM_BLOCKLIST"
    const val MODE_SILENT_LOCAL = "SILENT_LOCAL"

    /**
     * Device-protected prefs so BootReceiver / direct-boot arming works before the
     * lockscreen is dismissed (a dedicated phone usually sits locked on a USB cable).
     */
    private fun sp(ctx: Context): SharedPreferences {
        val de = ctx.createDeviceProtectedStorageContext()
        return de.getSharedPreferences(NAME, Context.MODE_PRIVATE)
    }

    fun isAiCallEnabled(ctx: Context): Boolean =
        sp(ctx).getBoolean(KEY_AI_CALL_ENABLED, true)

    fun setAiCallEnabled(ctx: Context, enabled: Boolean) {
        sp(ctx).edit().putBoolean(KEY_AI_CALL_ENABLED, enabled).apply()
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

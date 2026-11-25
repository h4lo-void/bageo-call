// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

package com.pixelcallai.streamer

import android.app.AutomaticZenRule
import android.app.NotificationManager
import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Build
import android.provider.Settings
import android.service.notification.Condition
import android.util.Log
import androidx.annotation.RequiresApi

/**
 * Bageo's own mode, "Bageo AI" (Settings > Modes; on Android 15+ also in the status bar's
 * Modes panel).
 *
 * Android 15+: the mode can be turned on and off by hand and silences nothing (the
 * module's script still owns the phone's Do Not Disturb in the silent AI mode). It is on
 * exactly while the phone is in AI mode: turning it on or off by hand switches Bageo to
 * AI answering or a normal phone, and every mode switch of Bageo turns it on or off.
 * Older Android: the rule of earlier versions (it cannot be turned on by hand there).
 *
 * "Do Not Disturb turns on AI answering" (the console's setting) decides whether the
 * phone's other Do Not Disturb modes switch Bageo too.
 */
object DedicatedDndManager {
    private const val TAG = "DedicatedDndManager"
    private val conditionUri: Uri
        get() = Uri.parse("condition://com.pixelcallai.streamer/dnd-ai")

    private const val MANUAL_SWITCH_COOLDOWN_MS = 10_000L

    /** Raise when the rule's look or behavior changes: existing rules are updated once. */
    private const val RULE_VERSION = 2

    // NotificationManager.AUTOMATIC_RULE_STATUS_* (ACTIVATED and DEACTIVATED are Android 15+).
    private const val STATUS_REMOVED = 3
    private const val STATUS_ACTIVATED = 4
    private const val STATUS_DEACTIVATED = 5

    /** Pure decision helper for unit tests and receiver transitions. */
    internal fun decideModeTarget(
        autoEnabled: Boolean,
        anyDndAllowed: Boolean,
        interruptionFilter: Int,
        ruleMatchesOurs: Boolean,
        currentMode: String,
        previousDndActive: Boolean? = null,
        aiCameFromDnd: Boolean = true,
    ): String? {
        if (!autoEnabled) return null
        val dndActive = interruptionFilter != NotificationManager.INTERRUPTION_FILTER_ALL
        // When previousDndActive is known and unchanged, do not override a manual switch.
        if (previousDndActive != null && previousDndActive == dndActive) return null
        val active = if (anyDndAllowed) {
            dndActive
        } else {
            ruleMatchesOurs && dndActive
        }
        val desired = if (active) Prefs.PHONE_MODE_AI else Prefs.PHONE_MODE_NORMAL
        // Do Not Disturb going off only ends AI answering that it started: an owner who
        // turned AI on by hand keeps it when they flip Do Not Disturb on and off.
        if (desired == Prefs.PHONE_MODE_NORMAL && currentMode == Prefs.PHONE_MODE_AI && !aiCameFromDnd) return null
        return if (desired != currentMode) desired else null
    }

    /** Android 15+ mode that Bageo keeps in step with AI mode. */
    private fun mirrorsMode(ctx: Context): Boolean =
        Build.VERSION.SDK_INT >= Build.VERSION_CODES.VANILLA_ICE_CREAM && Prefs.standaloneDndRuleMirrors(ctx)

    fun hasPolicyAccess(ctx: Context): Boolean {
        val nm = ctx.getSystemService(NotificationManager::class.java) ?: return false
        return nm.isNotificationPolicyAccessGranted
    }

    /**
     * Make (or update) the mode and return its id ("" without Do Not Disturb access).
     * [explicit] = asked for from the console; otherwise (app start, access granted) a
     * mode the owner deleted in Settings is not made again.
     */
    fun ensureRule(ctx: Context, explicit: Boolean = true): String {
        val nm = ctx.getSystemService(NotificationManager::class.java) ?: return ""
        if (!nm.isNotificationPolicyAccessGranted) return ""
        // The mode Android made for an older version that set Do Not Disturb itself.
        LocalAudioGuard.removeOwnDndMode(ctx)
        if (explicit) {
            Prefs.setStandaloneDndRuleDismissed(ctx, false)
        } else if (Prefs.isStandaloneDndRuleDismissed(ctx)) {
            return ""
        }
        val existing = try {
            nm.automaticZenRules
        } catch (_: Exception) {
            emptyMap<String, AutomaticZenRule>()
        }
        val savedId = Prefs.standaloneDndRuleId(ctx)
        val id = when {
            savedId.isNotBlank() && existing.containsKey(savedId) -> savedId
            else -> existing.entries.firstOrNull { (_, r) -> r.conditionId == conditionUri }?.key
        }
        if (id != null) {
            if (id != savedId) Prefs.setStandaloneDndRuleId(ctx, id)
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.VANILLA_ICE_CREAM &&
                Prefs.standaloneDndRuleVersion(ctx) < RULE_VERSION
            ) {
                try {
                    nm.updateAutomaticZenRule(id, buildModeRule(ctx))
                    Prefs.setStandaloneDndRuleVersion(ctx, RULE_VERSION, mirrors = true)
                    Log.i(TAG, "updated dedicated zen rule id=$id to a mode that can be turned on by hand")
                } catch (e: Exception) {
                    Log.w(TAG, "updateAutomaticZenRule failed: ${e.message}")
                }
            }
            return id
        }
        return try {
            val modeRule = Build.VERSION.SDK_INT >= Build.VERSION_CODES.VANILLA_ICE_CREAM
            val rule = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.VANILLA_ICE_CREAM) {
                buildModeRule(ctx)
            } else {
                buildLegacyRule(ctx)
            }
            val addedId = nm.addAutomaticZenRule(rule).orEmpty()
            if (addedId.isNotBlank()) {
                Prefs.setStandaloneDndRuleId(ctx, addedId)
                Prefs.setStandaloneDndRuleVersion(ctx, RULE_VERSION, mirrors = modeRule)
                Log.i(TAG, "created dedicated zen rule id=$addedId mode=$modeRule")
            }
            addedId
        } catch (e: Exception) {
            Log.w(TAG, "addAutomaticZenRule failed: ${e.message}")
            ""
        }
    }

    /**
     * Android 15+: a mode the owner can turn on and off (status bar panel, Settings) that
     * does not silence anything: Bageo's AI mode does the silencing it needs itself.
     */
    @RequiresApi(Build.VERSION_CODES.VANILLA_ICE_CREAM)
    private fun buildModeRule(ctx: Context): AutomaticZenRule =
        AutomaticZenRule.Builder(ctx.getString(R.string.sa_dnd_rule_name), conditionUri)
            .setType(AutomaticZenRule.TYPE_OTHER)
            .setInterruptionFilter(NotificationManager.INTERRUPTION_FILTER_ALL)
            .setManualInvocationAllowed(true)
            .setTriggerDescription(ctx.getString(R.string.sa_dnd_rule_trigger))
            .setIconResId(R.drawable.ic_bageo_mode)
            .setConfigurationActivity(ComponentName(ctx, MainActivity::class.java))
            .setEnabled(true)
            .build()

    /** Android 14 and older: the rule as earlier versions made it. */
    @Suppress("DEPRECATION")
    private fun buildLegacyRule(ctx: Context): AutomaticZenRule =
        AutomaticZenRule(
            ctx.getString(R.string.sa_dnd_rule_name),
            null,
            ComponentName(ctx, MainActivity::class.java),
            conditionUri,
            null,
            NotificationManager.INTERRUPTION_FILTER_PRIORITY,
            true,
        )

    /**
     * Bageo's mode is now [mode] (after a switch, or at start): turn the Bageo mode on in AI
     * mode and off in a normal phone. Changes made this way are not reported back as the
     * owner's, so they do not start another switch.
     */
    fun syncRuleState(ctx: Context, mode: String) {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.VANILLA_ICE_CREAM) return
        if (!mirrorsMode(ctx)) return
        val nm = ctx.getSystemService(NotificationManager::class.java) ?: return
        if (!nm.isNotificationPolicyAccessGranted) return
        val id = Prefs.standaloneDndRuleId(ctx)
        if (id.isBlank()) return
        val want = if (mode == Prefs.PHONE_MODE_AI) Condition.STATE_TRUE else Condition.STATE_FALSE
        try {
            if (nm.getAutomaticZenRuleState(id) == want) return
            val summary = ctx.getString(R.string.sa_dnd_rule_name)
            nm.setAutomaticZenRuleState(id, Condition(conditionUri, summary, want, Condition.SOURCE_USER_ACTION))
            Log.i(TAG, "Bageo mode ${if (want == Condition.STATE_TRUE) "on" else "off"} to match phone mode $mode")
        } catch (e: Exception) {
            Log.w(TAG, "setAutomaticZenRuleState failed: ${e.message}")
        }
    }

    /**
     * Called for every applied mode (switch thread) and at start: the Bageo mode follows,
     * and the screen-less handler of Android's after-call request is on only in AI mode
     * (Bageo is the phone app then).
     */
    fun onModeApplied(ctx: Context, mode: String) {
        syncRuleState(ctx, mode)
        setPostCallHandlerEnabled(ctx, mode == Prefs.PHONE_MODE_AI)
    }

    private fun setPostCallHandlerEnabled(ctx: Context, enabled: Boolean) {
        val pm = ctx.packageManager ?: return
        val component = ComponentName(ctx, PostCallActivity::class.java)
        val state = if (enabled) {
            PackageManager.COMPONENT_ENABLED_STATE_ENABLED
        } else {
            PackageManager.COMPONENT_ENABLED_STATE_DISABLED
        }
        try {
            if (pm.getComponentEnabledSetting(component) == state) return
            pm.setComponentEnabledSetting(component, state, PackageManager.DONT_KILL_APP)
        } catch (e: Exception) {
            Log.w(TAG, "after-call handler ${if (enabled) "on" else "off"} failed: ${e.message}")
        }
    }

    /**
     * Android told Bageo its mode changed ([status] is NotificationManager's
     * AUTOMATIC_RULE_STATUS_*). Turned on or off by the owner (status bar, Settings): switch
     * Bageo to match. Deleted: remember it, so it is made again only from the console.
     */
    fun onRuleStatusChanged(ctx: Context, ruleId: String, status: Int) {
        val ours = Prefs.standaloneDndRuleId(ctx)
        if (ruleId.isNotBlank() && ours.isNotBlank() && ruleId != ours) return
        when (status) {
            STATUS_ACTIVATED, STATUS_DEACTIVATED -> {
                if (mirrorsMode(ctx)) onManualToggle(ctx, status == STATUS_ACTIVATED) else evaluateAndApply(ctx)
            }
            STATUS_REMOVED -> {
                if (ruleId.isNotBlank() && ruleId == ours) {
                    Prefs.setStandaloneDndRuleDismissed(ctx, true)
                    Log.i(TAG, "Bageo mode deleted in Settings")
                }
            }
            else -> evaluateAndApply(ctx)
        }
    }

    private fun onManualToggle(ctx: Context, on: Boolean) {
        val target = if (on) Prefs.PHONE_MODE_AI else Prefs.PHONE_MODE_NORMAL
        if (PhoneModeManager.isSwitchingTo(target)) return
        val current = PhoneModeManager.reconcileLocalMirror(ctx)
        if (current == target) return
        if (PhoneModeManager.isSwitching() || !PhoneModeManager.isPhoneIdle(ctx)) {
            // No switch now (one runs, or a call is up): the mode shows Bageo's real mode again.
            Log.i(TAG, "Bageo mode turned ${if (on) "on" else "off"} by hand, but no switch now")
            syncRuleState(ctx, current)
            return
        }
        Log.i(TAG, "Bageo mode turned ${if (on) "on" else "off"} by hand -> switching phone mode to $target")
        Prefs.markManualModeSwitch(ctx)
        PhoneModeManager.requestSwitch(ctx, target, callBusy = false) { res ->
            if (res.ok && res.mode == Prefs.PHONE_MODE_AI) {
                CallAudioService.ensureArmed(ctx)
            }
            // A switch that did not start puts the mode back (a finished one already did).
            if (!res.ok) syncRuleState(ctx, res.mode)
            Log.i(TAG, "manual Bageo mode switch to $target ok=${res.ok} err=${res.error}")
        }
    }

    /** The phone's Do Not Disturb went on or off (any mode): "Do Not Disturb turns on AI answering". */
    fun evaluateAndApply(ctx: Context) {
        if (!Prefs.isStandaloneDndAutoAi(ctx)) return
        val anyMode = Prefs.isStandaloneDndAnyMode(ctx)
        val mirrors = mirrorsMode(ctx)
        // Android 15+: the Bageo mode silences nothing and is handled in onRuleStatusChanged;
        // only the phone's other modes are left for this setting.
        if (mirrors && !anyMode) return
        // Only run DND auto-switch when Daily Sound is on (otherwise Bageo itself mutates DND).
        if (!Prefs.isStandaloneDailySound(ctx)) return
        val nm = ctx.getSystemService(NotificationManager::class.java) ?: return
        val filter = nm.currentInterruptionFilter
        val dndNow = filter != NotificationManager.INTERRUPTION_FILTER_ALL
        val prevDnd = Prefs.lastStandaloneDndActive(ctx)
        Prefs.setLastStandaloneDndActive(ctx, dndNow)
        val nowMs = System.currentTimeMillis()
        if (nowMs - Prefs.lastManualModeSwitchAt(ctx) < MANUAL_SWITCH_COOLDOWN_MS) {
            Log.i(TAG, "skip DND auto-switch within manual switch cooldown")
            return
        }
        val currentMode = PhoneModeManager.reconcileLocalMirror(ctx)
        val ruleId = Prefs.standaloneDndRuleId(ctx)
        val ruleExistsAndEnabled = !mirrors && ruleId.isNotBlank() && try {
            nm.automaticZenRules[ruleId]?.isEnabled == true
        } catch (_: Exception) {
            false
        }
        val target = decideModeTarget(
            autoEnabled = true,
            anyDndAllowed = anyMode,
            interruptionFilter = filter,
            ruleMatchesOurs = ruleExistsAndEnabled,
            currentMode = currentMode,
            previousDndActive = prevDnd,
            aiCameFromDnd = Prefs.dndAiAt(ctx) > Prefs.lastManualModeSwitchAt(ctx),
        ) ?: return
        val busy = PhoneModeManager.isPhoneIdle(ctx).not()
        if (busy) {
            Log.i(TAG, "skip DND auto-switch to $target while call busy")
            return
        }
        Log.i(TAG, "DND state edge ($prevDnd -> $dndNow) -> switching phone mode to $target")
        PhoneModeManager.requestSwitch(ctx, target, callBusy = false) { res ->
            if (res.ok && res.mode == Prefs.PHONE_MODE_AI) {
                CallAudioService.ensureArmed(ctx)
                Prefs.markDndAi(ctx)
            }
            Log.i(TAG, "DND auto-switch to $target ok=${res.ok} err=${res.error}")
        }
    }

    fun openSystemDndSettings(ctx: Context) {
        val intent = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
            Intent("android.settings.AUTOMATIC_ZEN_RULE_SETTINGS").apply {
                val id = ensureRule(ctx)
                if (id.isNotBlank()) {
                    putExtra("android.provider.extra.AUTOMATIC_ZEN_RULE_ID", id)
                }
            }
        } else {
            Intent(Settings.ACTION_NOTIFICATION_POLICY_ACCESS_SETTINGS)
        }
        try {
            ctx.startActivity(intent.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))
        } catch (_: Exception) {
            try {
                ctx.startActivity(
                    Intent(Settings.ACTION_NOTIFICATION_POLICY_ACCESS_SETTINGS)
                        .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK),
                )
            } catch (_: Exception) {
            }
        }
    }
}

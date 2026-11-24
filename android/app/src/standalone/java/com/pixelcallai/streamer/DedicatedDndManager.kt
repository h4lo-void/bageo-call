// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

package com.pixelcallai.streamer

import android.app.AutomaticZenRule
import android.app.NotificationManager
import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.os.Build
import android.provider.Settings
import android.util.Log

/**
 * Manages a dedicated Android Do Not Disturb / Modes rule ("Bageo AI") so users can
 * trigger AI call answering simply by turning this Focus/DND mode on or off in Quick Settings.
 */
object DedicatedDndManager {
    private const val TAG = "DedicatedDndManager"
    private val conditionUri: Uri
        get() = Uri.parse("condition://com.pixelcallai.streamer/dnd-ai")

    private const val MANUAL_SWITCH_COOLDOWN_MS = 10_000L

    /** Pure decision helper for unit tests and receiver transitions. */
    internal fun decideModeTarget(
        autoEnabled: Boolean,
        anyDndAllowed: Boolean,
        interruptionFilter: Int,
        ruleMatchesOurs: Boolean,
        currentMode: String,
        previousDndActive: Boolean? = null,
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
        return if (desired != currentMode) desired else null
    }

    fun hasPolicyAccess(ctx: Context): Boolean {
        val nm = ctx.getSystemService(NotificationManager::class.java) ?: return false
        return nm.isNotificationPolicyAccessGranted
    }

    fun ensureRule(ctx: Context): String {
        val nm = ctx.getSystemService(NotificationManager::class.java) ?: return ""
        if (!nm.isNotificationPolicyAccessGranted) return ""
        val savedId = Prefs.standaloneDndRuleId(ctx)
        val existing = try {
            nm.automaticZenRules
        } catch (_: Exception) {
            emptyMap<String, AutomaticZenRule>()
        }
        if (savedId.isNotBlank() && existing.containsKey(savedId)) {
            return savedId
        }
        val matching = existing.entries.firstOrNull { (_, r) -> r.conditionId == conditionUri }
        if (matching != null) {
            Prefs.setStandaloneDndRuleId(ctx, matching.key)
            return matching.key
        }
        return try {
            val name = ctx.getString(R.string.sa_dnd_rule_name)
            val configActivity = ComponentName(ctx, MainActivity::class.java)
            val rule = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
                AutomaticZenRule(
                    name,
                    null,
                    configActivity,
                    conditionUri,
                    null,
                    NotificationManager.INTERRUPTION_FILTER_PRIORITY,
                    true,
                )
            } else {
                @Suppress("DEPRECATION")
                AutomaticZenRule(
                    name,
                    configActivity,
                    conditionUri,
                    NotificationManager.INTERRUPTION_FILTER_PRIORITY,
                    true,
                )
            }
            val addedId = nm.addAutomaticZenRule(rule).orEmpty()
            if (addedId.isNotBlank()) {
                Prefs.setStandaloneDndRuleId(ctx, addedId)
                Log.i(TAG, "created dedicated zen rule id=$addedId")
            }
            addedId
        } catch (e: Exception) {
            Log.w(TAG, "addAutomaticZenRule failed: ${e.message}")
            ""
        }
    }

    fun isDedicatedRuleActive(ctx: Context): Boolean {
        val nm = ctx.getSystemService(NotificationManager::class.java) ?: return false
        if (nm.currentInterruptionFilter == NotificationManager.INTERRUPTION_FILTER_ALL) return false
        val ruleId = Prefs.standaloneDndRuleId(ctx)
        if (ruleId.isBlank()) return false
        val rule = try {
            nm.automaticZenRules[ruleId]
        } catch (_: Exception) {
            null
        } ?: return false
        if (!rule.isEnabled) return false
        return if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
            val state = try {
                nm.consolidatedNotificationPolicy
                true
            } catch (_: Exception) {
                true
            }
            state && nm.currentInterruptionFilter != NotificationManager.INTERRUPTION_FILTER_ALL
        } else {
            true
        }
    }

    fun evaluateAndApply(ctx: Context) {
        if (!Prefs.isStandaloneDndAutoAi(ctx)) return
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
        val ruleExistsAndEnabled = ruleId.isNotBlank() && try {
            nm.automaticZenRules[ruleId]?.isEnabled == true
        } catch (_: Exception) {
            false
        }
        val target = decideModeTarget(
            autoEnabled = true,
            anyDndAllowed = Prefs.isStandaloneDndAnyMode(ctx),
            interruptionFilter = filter,
            ruleMatchesOurs = ruleExistsAndEnabled,
            currentMode = currentMode,
            previousDndActive = prevDnd,
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

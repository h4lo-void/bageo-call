// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

package com.pixelcallai.streamer

import android.app.NotificationManager
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent

/**
 * Do Not Disturb and Bageo's own mode: the owner turned the Bageo mode on or off (status
 * bar, Settings), the phone's Do Not Disturb changed, or Bageo just got Do Not Disturb
 * access (then the Bageo mode is made). Android sends these to Bageo directly.
 */
class DndStatusReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent?) {
        val received = intent ?: return
        val action = received.action ?: return
        val ctx = context.applicationContext
        when (action) {
            NotificationManager.ACTION_INTERRUPTION_FILTER_CHANGED ->
                DedicatedDndManager.evaluateAndApply(ctx)
            ACTION_RULE_STATUS_CHANGED ->
                DedicatedDndManager.onRuleStatusChanged(
                    ctx,
                    received.getStringExtra(EXTRA_RULE_ID).orEmpty(),
                    received.getIntExtra(EXTRA_RULE_STATUS, -1),
                )
            NotificationManager.ACTION_NOTIFICATION_POLICY_ACCESS_GRANTED_CHANGED -> {
                val pending = goAsync()
                Thread({
                    try {
                        if (DedicatedDndManager.ensureRule(ctx, explicit = false).isNotBlank()) {
                            DedicatedDndManager.syncRuleState(ctx, PhoneModeManager.reconcileLocalMirror(ctx))
                        }
                    } finally {
                        pending.finish()
                    }
                }, "bageo-mode-access").start()
            }
        }
    }

    private companion object {
        // NotificationManager.ACTION_AUTOMATIC_ZEN_RULE_STATUS_CHANGED and its extras.
        const val ACTION_RULE_STATUS_CHANGED = "android.app.action.AUTOMATIC_ZEN_RULE_STATUS_CHANGED"
        const val EXTRA_RULE_ID = "android.app.extra.AUTOMATIC_ZEN_RULE_ID"
        const val EXTRA_RULE_STATUS = "android.app.extra.AUTOMATIC_ZEN_RULE_STATUS"
    }
}

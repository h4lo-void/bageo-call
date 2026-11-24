// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

package com.pixelcallai.streamer

import android.app.NotificationManager
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent

/** Listens for Do Not Disturb / Focus Mode changes to toggle AI answering when enabled. */
class DndStatusReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent?) {
        val action = intent?.action ?: return
        if (action == NotificationManager.ACTION_INTERRUPTION_FILTER_CHANGED ||
            action == "android.app.action.AUTOMATIC_ZEN_RULE_STATUS_CHANGED"
        ) {
            DedicatedDndManager.evaluateAndApply(context.applicationContext)
        }
    }
}

// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

package com.pixelcallai.streamer

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent

/** Standalone: the answer and decline buttons of [OwnerRing]'s call notification. */
class OwnerRingActionReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent?) {
        when (intent?.action) {
            OwnerRing.ACTION_ANSWER -> OwnerRing.answer(context)
            OwnerRing.ACTION_DECLINE -> OwnerRing.decline(context)
        }
    }
}

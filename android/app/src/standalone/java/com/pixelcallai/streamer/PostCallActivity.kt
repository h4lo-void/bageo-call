// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

package com.pixelcallai.streamer

import android.app.Activity
import android.os.Bundle

/**
 * Android asks the phone app to show an after-call screen when a call ends
 * (android.telecom.action.POST_CALL). Bageo has none: this takes the request and closes
 * at once, without a screen. Turned on only in AI mode, when Bageo is the phone app
 * ([DedicatedDndManager.onModeApplied]).
 */
class PostCallActivity : Activity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        finish()
    }
}

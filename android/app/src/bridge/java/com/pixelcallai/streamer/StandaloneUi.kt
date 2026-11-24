// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

package com.pixelcallai.streamer

import android.app.Activity
import android.content.Intent
import android.os.Bundle

/**
 * No-op stub for the `bridge` flavor; the full hybrid UI implementation lives in
 * `src/standalone/java/.../StandaloneUi.kt`.
 */
@Suppress("UNUSED_PARAMETER")
class StandaloneUi(private val activity: Activity) {
    fun onCreate(savedInstanceState: Bundle?) {}
    fun onNewIntent(intent: Intent) {}
    fun onStart() {}
    fun onStop() {}
    fun onResume() {}
    fun onPause() {}
    fun onDestroy() {}
    fun onRequestPermissionsResult(requestCode: Int, permissions: Array<out String>, grantResults: IntArray) {}
    fun onActivityResult(requestCode: Int, resultCode: Int, data: Intent?) {}
    fun onBackPressed(): Boolean = false

    companion object {
        const val ENABLED = false
    }
}
